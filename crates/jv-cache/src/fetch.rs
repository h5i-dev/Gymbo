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

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use jv_model::Artifact;
use jv_repo::{ChecksumPolicy, Policy, Repository, artifact_path, checksum_path, join_url};

use crate::checksum::{self, ChecksumError};
use crate::store::{Locking, Store, StoreError};
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
    #[error("{url}: no checksum was published, and the checksum policy is `fail`")]
    MissingChecksum { url: String },
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
    /// Repositories that failed at the transport level, by URL.
    ///
    /// A 404 is an answer and is remembered on disk. A refused connection is
    /// not, and used to be remembered nowhere — so a repository that no longer
    /// exists was re-probed for *every* artifact, on every run. A POM graph
    /// accumulates a lot of those: `nexus.codehaus.org` and
    /// `snapshots.repository.codehaus.org` are still named by POMs published a
    /// decade ago, and both now refuse connections in about 100ms. Eighty-odd
    /// artifacts that no repository has, times a couple of dead hosts, is the
    /// thirteen to sixteen seconds a warm `jv sync` was spending on the network
    /// while using almost no CPU.
    ///
    /// Per process rather than persisted: a host that is down for a minute
    /// should not be written off until tomorrow, and a resolve is short enough
    /// that one probe per run is a fair price for noticing it came back.
    ///
    /// The reason is kept, not just the URL, because skipping a repository must
    /// not change what jv *says*. Without it, a run where every repository had
    /// failed reported "not available in any configured repository" for the
    /// second artifact onwards — which reads as a typo in the coordinates
    /// rather than as a network that is down.
    unreachable: Arc<Mutex<HashMap<String, String>>>,
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
            unreachable: Arc::new(Mutex::new(HashMap::new())),
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

    /// Whether this fetcher will refuse to contact a repository.
    ///
    /// Read by callers that cache a *derived* answer rather than bytes: nothing
    /// upstream can change while offline, so a remembered answer cannot go
    /// stale.
    pub fn is_offline(&self) -> bool {
        self.offline
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

    /// Puts an artifact in the cache and returns where it landed, without
    /// reading it.
    ///
    /// `jv sync` wants a few hundred files on disk so it can link them into
    /// `~/.m2`; it never looks inside one. Going through [`Fetcher::artifact`]
    /// for that meant loading every jar into memory and then reading the same
    /// file again to copy it — hundreds of megabytes allocated and discarded on a
    /// build with fat jars.
    ///
    /// A cache hit costs a `stat` here rather than a full read. A miss still
    /// downloads through the same path, because the bytes have to be verified and
    /// written whether or not the caller wants them.
    pub async fn locate(
        &self,
        repositories: &[Repository],
        artifact: &Artifact,
    ) -> Result<Fetched, FetchError> {
        let path = artifact_path(artifact);
        let usable: Vec<&Repository> = repositories
            .iter()
            .filter(|repository| repository.accepts(&artifact.version))
            .collect();
        for repository in &usable {
            let url = join_url(&repository.url, &path);
            let is_mutable = jv_model::is_snapshot_version(&artifact.version);
            if self.is_fresh(&url, repository.policy_for(&artifact.version), is_mutable)? {
                return Ok(Fetched {
                    bytes: Vec::new(),
                    origin: Origin::Cache,
                    repository: Some(repository.id.clone()),
                    path: self.store.path_for(&url)?,
                    warnings: Vec::new(),
                });
            }
        }
        // Guarded exactly as `fetch`'s local-repository step is, and for the same
        // reason: with every repository blocked by a mirror, or a snapshot
        // against a release-only repository, `~/.m2` would otherwise satisfy a
        // request the configuration says jv may not make — and `jv sync` would
        // then copy that artifact into the local repository.
        if let (Some(local), false) = (&self.local_repository, usable.is_empty()) {
            let candidate = local.join(&path);
            if candidate.is_file() {
                return Ok(Fetched {
                    bytes: Vec::new(),
                    origin: Origin::LocalRepository,
                    repository: None,
                    path: candidate,
                    warnings: Vec::new(),
                });
            }
        }
        self.artifact(repositories, artifact).await
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
        // Metadata changes whenever something is deployed, and a snapshot
        // changes whenever it is rebuilt. Everything else in a Maven repository
        // is immutable by convention, and jv relies on that.
        let is_mutable = !verify_checksums || jv_model::is_snapshot_version(version);

        // 1. jv's cache, per repository, respecting the update policy.
        for repository in repositories {
            let url = join_url(&repository.url, path);
            if let Some(bytes) = self.cached(&url, repository.policy_for(version), is_mutable)? {
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
        //
        // Only when some repository would have served the file. With every
        // repository blocked by a mirror, or a snapshot against a release-only
        // repository, `~/.m2` would otherwise quietly satisfy a request that is
        // supposed to fail — reporting success for an artifact the configuration
        // says jv may not have.
        if let (Some(local), false) = (&self.local_repository, repositories.is_empty()) {
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
        // Whether any repository gave a definitive answer, 404 included.
        let mut answered = false;
        for repository in repositories {
            let url = join_url(&repository.url, path);
            // Already refused a connection this run. Deliberately *not* counted
            // as an answer: it told us nothing, and treating silence as "absent"
            // is how an artifact somebody has becomes one nobody has. Its
            // failure is carried forward so the error at the end still names a
            // network problem.
            if let Some(reason) = self
                .unreachable
                .lock()
                .expect("unreachable")
                .get(&repository.url)
            {
                transport_error = Some(TransportError::Request {
                    url: join_url(&repository.url, path),
                    reason: reason.clone(),
                });
                continue;
            }
            if self.recently_missing(&url, repository.policy_for(version))? {
                // A recorded 404 is still that repository's answer. Not
                // counting it meant a warm cache turned a definitive "absent"
                // into whatever the *next* repository said — and when the next
                // one was unreachable, an artifact nobody has became a hard
                // failure that stopped the whole sync.
                answered = true;
                continue;
            }

            // Held for the duration of this repository's download so that a
            // second downloader waits rather than fetching the same file.
            //
            // Non-blocking, and it has to be: `flock` inside a future parks a
            // runtime worker, and this fetcher runs many downloads at once, so
            // blocking here would let the prefetcher deadlock against itself.
            // When somebody else holds it, `await_download` waits for their
            // result to appear instead of waiting on the lock.
            // Held for the duration of the download; named `_lock` because
            // nothing reads it, only its lifetime matters.
            let _lock = self.store.try_lock(&url)?;
            if matches!(_lock, Locking::Contended) {
                if let Some(bytes) = self
                    .await_download(&url, repository.policy_for(version), is_mutable)
                    .await?
                {
                    return Ok(Fetched {
                        bytes,
                        origin: Origin::Cache,
                        repository: Some(repository.id.clone()),
                        path: self.store.path_for(&url)?,
                        warnings: Vec::new(),
                    });
                }
                // They gave up or failed; fetching it again is better than
                // reporting an absence that is not real.
            }
            // It may have arrived while this task was yielding.
            if let Some(bytes) = self.cached(&url, repository.policy_for(version), is_mutable)? {
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
                    // Only what can change is stamped. A release artifact is
                    // immutable, so there is nothing for an update policy to
                    // re-check and the stamp would be a second file and a second
                    // directory walk per artifact for nothing.
                    if is_mutable {
                        self.store.record_checked(&url)?;
                    }
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
                    answered = true;
                    self.store.record_missing(&url)?;
                }
                // One broken repository must not hide an artifact another one
                // has, so the error is held back until every repository is
                // exhausted.
                Err(error) => {
                    // Retries are already exhausted by the time this surfaces,
                    // so the repository is not merely busy.
                    self.unreachable
                        .lock()
                        .expect("unreachable")
                        .insert(repository.url.clone(), error.to_string());
                    transport_error = Some(error);
                }
            }
        }

        match transport_error {
            // A repository that answered "no" is an answer. Reporting the
            // unreachable one instead turns an artifact that is genuinely
            // absent everywhere into a hard failure, and takes the whole sync
            // with it: flyway names a driver that is not on Central and also
            // lists `maven.java.net`, dead for years, so Central's 404 was
            // overridden by a connection error and `jv sync` stopped.
            //
            // The transport error still surfaces — the repository is recorded
            // as unreachable and warned about once — but it no longer decides
            // the outcome for an artifact somebody else already ruled on.
            Some(error) if !answered => Err(FetchError::Transport(error)),
            _ => Err(FetchError::NotFound {
                path: path.to_owned(),
            }),
        }
    }

    /// Waits for whoever holds a URL's download lock to finish.
    ///
    /// Polls rather than blocking on the lock, because blocking would park a
    /// runtime worker. The interval is short enough that a fast download is not
    /// noticeably delayed and the ceiling is high enough to cover a large jar on
    /// a slow link; past it, the caller downloads the file itself rather than
    /// waiting forever on a peer that may have died.
    async fn await_download(
        &self,
        url: &str,
        policy: &Policy,
        is_mutable: bool,
    ) -> Result<Option<Vec<u8>>, StoreError> {
        const INTERVAL: std::time::Duration = std::time::Duration::from_millis(25);
        const ATTEMPTS: usize = 40 * 60; // one minute

        for _ in 0..ATTEMPTS {
            tokio::time::sleep(INTERVAL).await;
            if let Some(bytes) = self.cached(url, policy, is_mutable)? {
                return Ok(Some(bytes));
            }
            // The holder finished without producing anything — a 404, or a
            // failure. Stop waiting and let the caller try. `Unavailable` ends
            // the wait too: on a filesystem that cannot lock there is nobody to
            // wait for.
            if !matches!(self.store.try_lock(url)?, Locking::Contended) {
                return Ok(None);
            }
        }
        Ok(None)
    }

    /// Whether a cached entry is present and usable, without reading it.
    ///
    /// The same rules as [`Fetcher::cached`], asked of the filesystem metadata
    /// rather than the contents.
    fn is_fresh(&self, url: &str, policy: &Policy, is_mutable: bool) -> Result<bool, StoreError> {
        if !self.store.path_for(url)?.is_file() {
            return Ok(false);
        }
        if !is_mutable || self.offline {
            return Ok(true);
        }
        Ok(match self.store.checked_at(url)? {
            Some(checked) => !policy.update.is_stale(checked, SystemTime::now()),
            // As in `cached`: a mutable entry with no stamp is stale.
            None => false,
        })
    }

    /// The cached bytes, if present and usable.
    ///
    /// `is_mutable` decides whether the update policy applies at all. An
    /// immutable release is never stale — that is what makes a warm resolve free
    /// — while metadata and snapshots carry a freshness window.
    fn cached(
        &self,
        url: &str,
        policy: &Policy,
        is_mutable: bool,
    ) -> Result<Option<Vec<u8>>, StoreError> {
        let Some(bytes) = self.store.read(url)? else {
            return Ok(None);
        };
        if !is_mutable || self.offline {
            return Ok(Some(bytes));
        }
        match self.store.checked_at(url)? {
            Some(checked) if !policy.update.is_stale(checked, SystemTime::now()) => Ok(Some(bytes)),
            Some(_) => Ok(None),
            // A mutable entry with no stamp is stale, not fresh. The stamp is a
            // separate file, so a process killed between writing the content and
            // writing the stamp leaves one without the other — and reading that
            // as "fresh" pinned a snapshot to a stale build permanently, with no
            // timestamp for `-U` to compare against and dislodge.
            None => Ok(None),
        }
    }

    /// Whether a recorded absence is recent enough to trust.
    ///
    /// `always` skips the record entirely. That is what Maven's `-U` actually
    /// does for a release: it forces a check for a *missing* one, rather than
    /// re-downloading one already present — a released artifact is immutable, so
    /// there is nothing a second download could tell you.
    fn recently_missing(&self, url: &str, policy: &Policy) -> Result<bool, StoreError> {
        if policy.update == jv_repo::UpdatePolicy::Always {
            return Ok(false);
        }
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

        // All algorithms at once. Asking in turn cost the common case — an
        // artifact publishing only `.sha1` — two sequential 404s before the one
        // that answers, on the critical path of every single download.
        let published = futures_util::future::join_all(checksum::PREFERRED.iter().map(
            |algorithm| async move {
                self.transport
                    .get(&checksum_path(url, *algorithm), &repository.credentials)
                    .await
            },
        ))
        .await;

        for (algorithm, published) in checksum::PREFERRED.iter().zip(published) {
            let published = match published {
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

        // No checksum could be obtained from any algorithm. Under `fail` that is
        // a failure, not a pass: an attacker in the path who rewrites the
        // artifact and 404s the checksums would otherwise be accepted by the
        // *strictest* setting, which is a verification bypass rather than a
        // lenience. Upstream's `AbstractChecksumPolicy.onNoMoreChecksums` throws
        // for the same reason.
        if policy.is_fatal() {
            return Err(FetchError::MissingChecksum {
                url: url.to_owned(),
            });
        }
        // Under `warn` — Maven's default, and jv's — a repository that publishes
        // no checksums is accepted. Many internal repositories do not, and
        // refusing them would make jv unusable where Maven works.
        //
        // The message names the *repository*, not the artifact: a repository
        // without checksums has none for anything, and one line per artifact
        // would bury every other warning under a hundred copies of the same
        // sentence. Callers deduplicate identical messages, so this collapses to
        // one.
        Ok(vec![format!(
            "{} publishes no checksums, so nothing downloaded from it can be verified",
            repository.url
        )])
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

    /// A second artifact, so a test can ask for two different things and see
    /// whether the same repository was probed twice.
    fn other_artifact() -> Artifact {
        Artifact::new("com.example", "other", "1.0")
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
    async fn a_repository_that_refused_a_connection_is_not_asked_again() {
        // The reason this exists. A 404 is an answer and is remembered on disk;
        // a refused connection was remembered nowhere, so a host that no longer
        // exists was probed once per artifact. On a real graph that is dozens of
        // dead-host round trips per run, which is where a warm `jv sync` was
        // spending thirteen seconds while using almost no CPU.
        let dead = Repository::new("dead", "https://dead.example/repo");
        let mut transport = MapTransport::new();
        transport.fail_host("https://dead.example/repo", "connection refused");
        transport.insert(url_for(CENTRAL, &artifact()), b"one".to_vec());
        transport.insert(url_for(CENTRAL, &other_artifact()), b"two".to_vec());
        let requests = transport.requests();

        let (_dir, fetcher) = fetcher(transport);
        let repositories = [dead, repository()];

        // The dead repository comes first, so it is asked before Central.
        fetcher.artifact(&repositories, &artifact()).await.unwrap();
        fetcher
            .artifact(&repositories, &other_artifact())
            .await
            .unwrap();

        assert_eq!(
            requests
                .lock()
                .expect("requests")
                .iter()
                .filter(|url| url.starts_with("https://dead.example/repo"))
                .count(),
            1,
            "a dead host was probed more than once"
        );
    }

    #[tokio::test]
    async fn a_dead_repository_does_not_stop_a_working_one_serving_later_artifacts() {
        // The danger of the breaker: skipping a repository must not turn into
        // skipping the answer. Everything still resolves, from the repository
        // that works.
        let dead = Repository::new("dead", "https://dead.example/repo");
        let mut transport = MapTransport::new();
        transport.fail_host("https://dead.example/repo", "connection refused");
        transport.insert(url_for(CENTRAL, &artifact()), b"one".to_vec());
        transport.insert(url_for(CENTRAL, &other_artifact()), b"two".to_vec());

        let (_dir, fetcher) = fetcher(transport);
        let repositories = [dead, repository()];

        let first = fetcher.artifact(&repositories, &artifact()).await.unwrap();
        let second = fetcher
            .artifact(&repositories, &other_artifact())
            .await
            .unwrap();
        assert_eq!(first.bytes, b"one".to_vec());
        assert_eq!(second.bytes, b"two".to_vec());
        assert_eq!(second.repository.as_deref(), Some("central"));
    }

    #[tokio::test]
    async fn every_repository_being_dead_is_still_reported_as_a_transport_error() {
        // Skipping a repository it has given up on must not make jv say "no
        // repository has this" — that sends the reader looking for a typo in
        // their coordinates instead of at their network.
        let mut transport = MapTransport::new();
        transport.fail_host(CENTRAL, "connection refused");
        let (_dir, fetcher) = fetcher(transport);

        fetcher
            .artifact(&[repository()], &artifact())
            .await
            .unwrap_err();
        let error = fetcher
            .artifact(&[repository()], &other_artifact())
            .await
            .unwrap_err();
        assert!(
            matches!(error, FetchError::Transport(_)),
            "a skipped repository was reported as absence, not as a network problem"
        );
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
    async fn a_repository_publishing_no_checksum_is_accepted_but_reported() {
        let mut transport = MapTransport::new();
        transport.insert(url_for(CENTRAL, &artifact()), b"jar".to_vec());
        let (_dir, fetcher) = fetcher(transport);
        // Under the default `warn`: many internal repositories publish no
        // checksums and refusing them would make jv unusable where Maven works.
        let fetched = fetcher
            .artifact(&[repository()], &artifact())
            .await
            .unwrap();
        assert_eq!(fetched.warnings.len(), 1);
        assert!(fetched.warnings[0].contains("no checksums"));
        // Named by repository, not by artifact: one line, not one per jar.
        assert!(fetched.warnings[0].contains(CENTRAL));
    }

    #[tokio::test]
    async fn no_checksum_at_all_is_a_failure_under_a_strict_policy() {
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
        // Accepting here would let anyone in the path rewrite the artifact and
        // 404 the checksums, and be accepted by the *strictest* setting. That is
        // a verification bypass, not a lenience.
        let error = fetcher.artifact(&[strict], &artifact()).await.unwrap_err();
        assert!(matches!(error, FetchError::MissingChecksum { .. }));
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
    async fn a_stale_snapshot_is_refetched() {
        let snapshot = Artifact::new("g", "a", "1.0-SNAPSHOT");
        let mut transport = MapTransport::new();
        transport.insert(url_for(CENTRAL, &snapshot), b"first".to_vec());
        let (_dir, fetcher) = fetcher(transport);

        let always = Repository {
            snapshots: Policy {
                update: UpdatePolicy::Always,
                ..Policy::default()
            },
            ..repository()
        };
        let first = fetcher
            .artifact(std::slice::from_ref(&always), &snapshot)
            .await
            .unwrap();
        assert_eq!(first.origin, Origin::Repository);
        // A snapshot can change under you, so `always` never trusts the cache.
        let second = fetcher
            .artifact(std::slice::from_ref(&always), &snapshot)
            .await
            .unwrap();
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
    async fn a_mutable_entry_with_no_freshness_stamp_is_stale() {
        // The stamp is a separate file, so a process killed between writing the
        // content and writing the stamp leaves one without the other. Reading
        // that as "fresh" pinned a snapshot to a stale build permanently, with no
        // timestamp for `-U` to compare against.
        let snapshot = Artifact::new("g", "a", "1.0-SNAPSHOT");
        let url = url_for(CENTRAL, &snapshot);
        let mut transport = MapTransport::new();
        transport.insert(url.clone(), b"fresh".to_vec());
        let (_dir, fetcher) = fetcher(transport);

        // Content in the cache, no stamp beside it.
        fetcher.store().write(&url, b"stale").unwrap();
        let fetched = fetcher.artifact(&[repository()], &snapshot).await.unwrap();
        assert_eq!(fetched.origin, Origin::Repository);
        assert_eq!(fetched.bytes, b"fresh");
    }

    #[tokio::test]
    async fn a_release_with_no_stamp_is_still_served_from_the_cache() {
        // An immutable release has nothing to re-check, and treating a missing
        // stamp as stale there would make every warm resolve re-download
        // everything.
        let url = url_for(CENTRAL, &artifact());
        let (_dir, fetcher) = fetcher(MapTransport::new());
        fetcher.store().write(&url, b"jar").unwrap();
        let fetched = fetcher
            .artifact(&[repository()], &artifact())
            .await
            .unwrap();
        assert_eq!(fetched.origin, Origin::Cache);
    }

    #[tokio::test]
    async fn the_local_repository_does_not_rescue_a_blocked_repository() {
        let dir = tempfile::tempdir().unwrap();
        let m2 = dir.path().join("m2");
        let path = m2.join(artifact_path(&artifact()));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"from m2").unwrap();

        let cache = tempfile::tempdir().unwrap();
        let fetcher = Fetcher::new(Store::new(cache.path()), Box::new(MapTransport::new()))
            .with_local_repository(&m2);

        // A mirror declared every repository unreachable, so nothing may serve
        // this. Answering out of `~/.m2` would report success for an artifact the
        // configuration says jv may not have.
        let blocked = Repository {
            blocked: true,
            ..repository()
        };
        assert!(fetcher.artifact(&[blocked], &artifact()).await.is_err());
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
