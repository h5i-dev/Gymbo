//! Getting a file, from wherever it already is.
//!
//! The order is what makes a warm run fast and a cold run correct:
//!
//! 1. jv's own cache, unless an update policy says the entry is stale;
//! 2. `~/.m2/repository`, read-only — a machine that has already built with
//!    Maven has most of what jv needs, and re-downloading it would be waste
//!    dressed up as caution;
//! 3. each repository in turn, verifying the strongest published checksum.
//!
//! A 404 from a repository is normal and is remembered, so a resolve that spans
//! several repositories does not pay for the same absence twice.

use std::path::PathBuf;
use std::time::SystemTime;

use jv_model::Artifact;
use jv_repo::{ChecksumPolicy, Policy, Repository, artifact_path, checksum_path, join_url};

use crate::checksum::{self, ChecksumError};
use crate::store::{Store, StoreError};
use crate::transport::{Transport, TransportError};

/// A file could not be obtained.
#[derive(Debug, thiserror::Error)]
pub enum FetchError {
    #[error("{path} is not available in any configured repository")]
    NotFound { path: String },
    #[error("{path} is not cached, and jv is offline")]
    Offline { path: String },
    #[error(transparent)]
    Transport(#[from] TransportError),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error("{url}: {source}")]
    Checksum {
        url: String,
        #[source]
        source: ChecksumError,
    },
}

/// Where a file came from, which the CLI reports and the tests assert on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Origin {
    /// jv's cache had it.
    Cache,
    /// `~/.m2/repository` had it.
    LocalRepository,
    /// It was downloaded.
    Repository,
}

/// A retrieved file.
#[derive(Clone, Debug)]
pub struct Fetched {
    pub bytes: Vec<u8>,
    pub origin: Origin,
    /// The repository it came from, when it was downloaded.
    pub repository: Option<String>,
    /// Where the bytes are on disk — in jv's cache, or in `~/.m2/repository`
    /// when that is where they were found. Callers that need a file rather than
    /// bytes, such as building a classpath, use this instead of recomputing it.
    pub path: PathBuf,
    /// Problems that did not stop the fetch — a checksum mismatch under a `warn`
    /// policy, most of all. The caller is expected to show these; a checksum
    /// policy named `warn` that produces no warning is just `ignore`.
    pub warnings: Vec<String>,
}

/// Retrieves repository files, caching what it downloads.
pub struct Fetcher {
    store: Store,
    transport: Box<dyn Transport>,
    /// `~/.m2/repository`, read but never written. jv keeps its own store as the
    /// source of truth; writing into Maven's would risk corrupting a directory
    /// another tool owns.
    local_repository: Option<PathBuf>,
    offline: bool,
    /// How long a recorded 404 is trusted before the repository is asked again.
    missing_ttl: std::time::Duration,
}

impl std::fmt::Debug for Fetcher {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Fetcher")
            .field("store", &self.store)
            .field("local_repository", &self.local_repository)
            .field("offline", &self.offline)
            .finish_non_exhaustive()
    }
}

impl Fetcher {
    pub fn new(store: Store, transport: Box<dyn Transport>) -> Self {
        Self {
            store,
            transport,
            local_repository: None,
            offline: false,
            // Long enough that a multi-repository resolve does not re-ask, short
            // enough that an artifact published today is found today.
            missing_ttl: std::time::Duration::from_secs(3600),
        }
    }

    /// Reads `~/.m2/repository` opportunistically.
    pub fn with_local_repository(mut self, path: impl Into<PathBuf>) -> Self {
        self.local_repository = Some(path.into());
        self
    }

    /// Refuses to contact any repository.
    pub fn offline(mut self, offline: bool) -> Self {
        self.offline = offline;
        self
    }

    pub fn store(&self) -> &Store {
        &self.store
    }

    /// Fetches an artifact by coordinates.
    pub async fn artifact(
        &self,
        repositories: &[Repository],
        artifact: &Artifact,
    ) -> Result<Fetched, FetchError> {
        let path = artifact_path(artifact);
        let usable: Vec<&Repository> = repositories
            .iter()
            .filter(|repository| repository.accepts(&artifact.version))
            .collect();
        self.fetch(&usable, &path, &artifact.version, true).await
    }

    /// Fetches a repository-relative path that is not an artifact, such as
    /// `maven-metadata.xml`.
    ///
    /// Returns `None` rather than failing when no repository has it: a missing
    /// metadata file means "this repository holds no versions of that artifact",
    /// which is an answer.
    pub async fn optional(
        &self,
        repositories: &[Repository],
        path: &str,
        version_hint: &str,
    ) -> Result<Option<Fetched>, FetchError> {
        let usable: Vec<&Repository> = repositories
            .iter()
            .filter(|repository| repository.accepts(version_hint))
            .collect();
        match self.fetch(&usable, path, version_hint, false).await {
            Ok(fetched) => Ok(Some(fetched)),
            Err(FetchError::NotFound { .. }) => Ok(None),
            Err(other) => Err(other),
        }
    }

    async fn fetch(
        &self,
        repositories: &[&Repository],
        path: &str,
        version: &str,
        verify_checksums: bool,
    ) -> Result<Fetched, FetchError> {
        // 1. jv's cache, per repository, respecting the update policy.
        for repository in repositories {
            let url = join_url(&repository.url, path);
            if let Some(bytes) = self.cached(&url, repository.policy_for(version))? {
                return Ok(Fetched {
                    bytes,
                    origin: Origin::Cache,
                    repository: Some(repository.id.clone()),
                    path: self.store.path_for(&url)?,
                    warnings: Vec::new(),
                });
            }
        }

        // 2. Maven's local repository. Its layout is the same, so the
        // repository-relative path is also the path inside it.
        if let Some(local) = &self.local_repository {
            let candidate = local.join(path);
            if let Ok(bytes) = std::fs::read(&candidate) {
                return Ok(Fetched {
                    bytes,
                    origin: Origin::LocalRepository,
                    repository: None,
                    path: candidate,
                    warnings: Vec::new(),
                });
            }
        }

        if self.offline {
            return Err(FetchError::Offline {
                path: path.to_owned(),
            });
        }

        // 3. Ask each repository, in order.
        let mut transport_error = None;
        for repository in repositories {
            let url = join_url(&repository.url, path);
            if self.recently_missing(&url)? {
                continue;
            }

            // Held for the duration of this repository's download so that a
            // second jv process waits rather than fetching the same file.
            let _lock = self.store.lock(&url)?;
            // It may have arrived while this process was waiting for the lock.
            if let Some(bytes) = self.cached(&url, repository.policy_for(version))? {
                return Ok(Fetched {
                    bytes,
                    origin: Origin::Cache,
                    repository: Some(repository.id.clone()),
                    path: self.store.path_for(&url)?,
                    warnings: Vec::new(),
                });
            }

            match self.transport.get(&url, &repository.credentials).await {
                Ok(Some(bytes)) => {
                    let warnings = if verify_checksums {
                        self.verify(repository, &url, &bytes, version).await?
                    } else {
                        Vec::new()
                    };
                    let path = self.store.write(&url, &bytes)?;
                    self.store.clear_missing(&url)?;
                    return Ok(Fetched {
                        bytes,
                        origin: Origin::Repository,
                        repository: Some(repository.id.clone()),
                        path,
                        warnings,
                    });
                }
                Ok(None) => {
                    self.store.record_missing(&url)?;
                }
                // One broken repository must not hide an artifact another one
                // has, so the error is held back until every repository is
                // exhausted.
                Err(error) => transport_error = Some(error),
            }
        }

        match transport_error {
            Some(error) => Err(FetchError::Transport(error)),
            None => Err(FetchError::NotFound {
                path: path.to_owned(),
            }),
        }
    }

    /// The cached bytes, if present and not stale.
    fn cached(&self, url: &str, policy: &Policy) -> Result<Option<Vec<u8>>, StoreError> {
        let Some(bytes) = self.store.read(url)? else {
            return Ok(None);
        };
        // An immutable artifact is never stale; only the update policy can make
        // a cached entry unusable, and it applies to mutable files.
        if let Some(checked) = self.store.checked_at(url)? {
            if policy.update.is_stale(checked, SystemTime::now()) && !self.offline {
                return Ok(None);
            }
        }
        Ok(Some(bytes))
    }

    /// Whether a recorded absence is recent enough to trust.
    fn recently_missing(&self, url: &str) -> Result<bool, StoreError> {
        let Some(since) = self.store.missing_since(url)? else {
            return Ok(false);
        };
        Ok(SystemTime::now()
            .duration_since(since)
            .map(|elapsed| elapsed < self.missing_ttl)
            .unwrap_or(false))
    }

    /// Checks downloaded bytes against the strongest checksum the repository
    /// publishes, returning any warning it produced.
    ///
    /// A repository that publishes none is accepted: many internal repositories
    /// do not, and refusing them would make jv unusable where Maven works.
    async fn verify(
        &self,
        repository: &Repository,
        url: &str,
        bytes: &[u8],
        version: &str,
    ) -> Result<Vec<String>, FetchError> {
        let policy = repository.policy_for(version).checksum;
        if policy == ChecksumPolicy::Ignore {
            return Ok(Vec::new());
        }

        for algorithm in checksum::PREFERRED {
            let checksum_url = checksum_path(url, *algorithm);
            let published = match self
                .transport
                .get(&checksum_url, &repository.credentials)
                .await
            {
                Ok(Some(bytes)) => bytes,
                // Not published, or the request failed: try a weaker algorithm.
                Ok(None) | Err(_) => continue,
            };
            let text = String::from_utf8_lossy(&published);
            match checksum::verify(bytes, &text, *algorithm) {
                Ok(()) => return Ok(Vec::new()),
                Err(source) => {
                    if policy.is_fatal() {
                        return Err(FetchError::Checksum {
                            url: url.to_owned(),
                            source,
                        });
                    }
                    // Reported rather than fatal, matching Maven's default. The
                    // weaker algorithms are not consulted after a mismatch: the
                    // file is already known not to be what was published.
                    return Ok(vec![format!("{url}: {source}")]);
                }
            }
        }
        Ok(Vec::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::MapTransport;
    use jv_repo::{ChecksumPolicy, Policy, UpdatePolicy};

    const CENTRAL: &str = "https://repo1.maven.org/maven2";

    fn artifact() -> Artifact {
        Artifact::new("org.slf4j", "slf4j-api", "2.0.9")
    }

    fn url_for(base: &str, artifact: &Artifact) -> String {
        join_url(base, &artifact_path(artifact))
    }

    fn repository() -> Repository {
        Repository::new("central", CENTRAL)
    }

    fn fetcher(transport: MapTransport) -> (tempfile::TempDir, Fetcher) {
        let dir = tempfile::tempdir().expect("temp dir");
        let fetcher = Fetcher::new(Store::new(dir.path()), Box::new(transport));
        (dir, fetcher)
    }

    #[tokio::test]
    async fn a_download_is_returned_and_cached() {
        let mut transport = MapTransport::new();
        transport.insert(url_for(CENTRAL, &artifact()), b"jar".to_vec());
        let (_dir, fetcher) = fetcher(transport);

        let first = fetcher
            .artifact(&[repository()], &artifact())
            .await
            .unwrap();
        assert_eq!(first.origin, Origin::Repository);
        assert_eq!(first.bytes, b"jar");

        // The second call must not go to the repository.
        let second = fetcher
            .artifact(&[repository()], &artifact())
            .await
            .unwrap();
        assert_eq!(second.origin, Origin::Cache);
        assert_eq!(second.repository.as_deref(), Some("central"));
    }

    #[tokio::test]
    async fn repositories_are_tried_in_order() {
        let other = Repository::new("other", "https://other.example/repo");
        let mut transport = MapTransport::new();
        // Only the second repository has it.
        transport.insert(
            url_for("https://other.example/repo", &artifact()),
            b"jar".to_vec(),
        );
        let (_dir, fetcher) = fetcher(transport);

        let fetched = fetcher
            .artifact(&[repository(), other], &artifact())
            .await
            .unwrap();
        assert_eq!(fetched.repository.as_deref(), Some("other"));
    }

    #[tokio::test]
    async fn absence_everywhere_is_reported_once() {
        let (_dir, fetcher) = fetcher(MapTransport::new());
        let error = fetcher
            .artifact(&[repository()], &artifact())
            .await
            .unwrap_err();
        assert!(matches!(error, FetchError::NotFound { .. }));
        // The 404 was remembered, so a second attempt does not re-ask.
        let url = url_for(CENTRAL, &artifact());
        assert!(fetcher.store().missing_since(&url).unwrap().is_some());
    }

    #[tokio::test]
    async fn a_broken_repository_does_not_hide_a_working_one() {
        let other = Repository::new("other", "https://other.example/repo");
        let mut transport = MapTransport::new();
        transport.fail(url_for(CENTRAL, &artifact()), "connection reset");
        transport.insert(
            url_for("https://other.example/repo", &artifact()),
            b"jar".to_vec(),
        );
        let (_dir, fetcher) = fetcher(transport);

        let fetched = fetcher
            .artifact(&[repository(), other], &artifact())
            .await
            .unwrap();
        assert_eq!(fetched.repository.as_deref(), Some("other"));
    }

    #[tokio::test]
    async fn a_broken_repository_is_reported_when_nothing_else_has_it() {
        let mut transport = MapTransport::new();
        transport.fail(url_for(CENTRAL, &artifact()), "connection reset");
        let (_dir, fetcher) = fetcher(transport);

        let error = fetcher
            .artifact(&[repository()], &artifact())
            .await
            .unwrap_err();
        // Reporting "not found" here would send the user looking for a typo in
        // their coordinates rather than at their network.
        assert!(matches!(error, FetchError::Transport(_)));
    }

    #[tokio::test]
    async fn a_matching_checksum_passes() {
        let bytes = b"jar".to_vec();
        let url = url_for(CENTRAL, &artifact());
        let mut transport = MapTransport::new();
        transport.insert(url.clone(), bytes.clone());
        transport.insert(
            format!("{url}.sha1"),
            checksum::digest(&bytes, jv_repo::Checksum::Sha1).into_bytes(),
        );
        let (_dir, fetcher) = fetcher(transport);
        assert!(fetcher.artifact(&[repository()], &artifact()).await.is_ok());
    }

    #[tokio::test]
    async fn a_mismatched_checksum_fails_under_a_strict_policy() {
        let url = url_for(CENTRAL, &artifact());
        let mut transport = MapTransport::new();
        transport.insert(url.clone(), b"jar".to_vec());
        transport.insert(
            format!("{url}.sha1"),
            checksum::digest(b"something else", jv_repo::Checksum::Sha1).into_bytes(),
        );
        let (_dir, fetcher) = fetcher(transport);

        let strict = Repository {
            releases: Policy {
                checksum: ChecksumPolicy::Fail,
                ..Policy::default()
            },
            ..repository()
        };
        let error = fetcher.artifact(&[strict], &artifact()).await.unwrap_err();
        assert!(matches!(error, FetchError::Checksum { .. }));
    }

    #[tokio::test]
    async fn a_mismatched_checksum_only_warns_by_default() {
        let url = url_for(CENTRAL, &artifact());
        let mut transport = MapTransport::new();
        transport.insert(url.clone(), b"jar".to_vec());
        transport.insert(
            format!("{url}.sha1"),
            checksum::digest(b"something else", jv_repo::Checksum::Sha1).into_bytes(),
        );
        let (_dir, fetcher) = fetcher(transport);
        // Maven's default is warn, and being stricter than Maven by default
        // would reject builds that Maven completes.
        let fetched = fetcher
            .artifact(&[repository()], &artifact())
            .await
            .unwrap();
        // But it must actually warn. A `warn` policy that says nothing is
        // `ignore` under another name, and hides a tampered artifact.
        assert_eq!(fetched.warnings.len(), 1);
        assert!(fetched.warnings[0].contains("sha1 mismatch"));
    }

    #[tokio::test]
    async fn a_good_download_warns_about_nothing() {
        let bytes = b"jar".to_vec();
        let url = url_for(CENTRAL, &artifact());
        let mut transport = MapTransport::new();
        transport.insert(url.clone(), bytes.clone());
        transport.insert(
            format!("{url}.sha1"),
            checksum::digest(&bytes, jv_repo::Checksum::Sha1).into_bytes(),
        );
        let (_dir, fetcher) = fetcher(transport);
        let fetched = fetcher
            .artifact(&[repository()], &artifact())
            .await
            .unwrap();
        assert!(fetched.warnings.is_empty());
    }

    #[tokio::test]
    async fn a_repository_publishing_no_checksum_is_accepted() {
        let mut transport = MapTransport::new();
        transport.insert(url_for(CENTRAL, &artifact()), b"jar".to_vec());
        let (_dir, fetcher) = fetcher(transport);
        let strict = Repository {
            releases: Policy {
                checksum: ChecksumPolicy::Fail,
                ..Policy::default()
            },
            ..repository()
        };
        assert!(fetcher.artifact(&[strict], &artifact()).await.is_ok());
    }

    #[tokio::test]
    async fn the_local_repository_is_preferred_over_downloading() {
        let dir = tempfile::tempdir().unwrap();
        let m2 = dir.path().join("m2");
        let path = m2.join(artifact_path(&artifact()));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"from m2").unwrap();

        let mut transport = MapTransport::new();
        transport.insert(url_for(CENTRAL, &artifact()), b"downloaded".to_vec());
        let cache = tempfile::tempdir().unwrap();
        let fetcher =
            Fetcher::new(Store::new(cache.path()), Box::new(transport)).with_local_repository(&m2);

        let fetched = fetcher
            .artifact(&[repository()], &artifact())
            .await
            .unwrap();
        assert_eq!(fetched.origin, Origin::LocalRepository);
        assert_eq!(fetched.bytes, b"from m2");
    }

    #[tokio::test]
    async fn offline_serves_the_cache_and_refuses_the_network() {
        let mut transport = MapTransport::new();
        transport.insert(url_for(CENTRAL, &artifact()), b"jar".to_vec());
        let dir = tempfile::tempdir().unwrap();
        let online = Fetcher::new(Store::new(dir.path()), Box::new(transport));
        online.artifact(&[repository()], &artifact()).await.unwrap();

        // A fresh offline fetcher over the same store, with a transport that
        // would fail if consulted.
        let offline =
            Fetcher::new(Store::new(dir.path()), Box::new(MapTransport::new())).offline(true);
        let fetched = offline
            .artifact(&[repository()], &artifact())
            .await
            .unwrap();
        assert_eq!(fetched.origin, Origin::Cache);

        let missing = Artifact::new("org.slf4j", "slf4j-simple", "2.0.9");
        let error = offline
            .artifact(&[repository()], &missing)
            .await
            .unwrap_err();
        assert!(matches!(error, FetchError::Offline { .. }));
    }

    #[tokio::test]
    async fn a_stale_entry_is_refetched() {
        let url = url_for(CENTRAL, &artifact());
        let mut transport = MapTransport::new();
        transport.insert(url.clone(), b"first".to_vec());
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path());
        let fetcher = Fetcher::new(store.clone(), Box::new(transport));

        let always = Repository {
            releases: Policy {
                update: UpdatePolicy::Always,
                ..Policy::default()
            },
            ..repository()
        };
        let first = fetcher
            .artifact(std::slice::from_ref(&always), &artifact())
            .await
            .unwrap();
        assert_eq!(first.origin, Origin::Repository);
        // With `always`, the cache is never trusted.
        let second = fetcher.artifact(&[always], &artifact()).await.unwrap();
        assert_eq!(second.origin, Origin::Repository);
    }

    #[tokio::test]
    async fn a_repository_that_refuses_snapshots_is_skipped() {
        let snapshot = Artifact::new("g", "a", "1.0-SNAPSHOT");
        let mut transport = MapTransport::new();
        transport.insert(url_for(CENTRAL, &snapshot), b"jar".to_vec());
        let (_dir, fetcher) = fetcher(transport);

        // Central declines snapshots, so nothing is asked and nothing is found.
        let error = fetcher
            .artifact(&[Repository::central()], &snapshot)
            .await
            .unwrap_err();
        assert!(matches!(error, FetchError::NotFound { .. }));
    }

    #[tokio::test]
    async fn optional_files_report_absence_as_none() {
        let (_dir, fetcher) = fetcher(MapTransport::new());
        let found = fetcher
            .optional(&[repository()], "g/a/maven-metadata.xml", "1.0")
            .await
            .unwrap();
        assert!(found.is_none());
    }
}
