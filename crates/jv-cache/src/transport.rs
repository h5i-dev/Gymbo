//! Getting bytes from a repository.
//!
//! An interface rather than a function, so the fetcher can be tested against a
//! scripted repository with no network, and so a `file:` repository is served by
//! the same code path as an HTTP one.
//!
//! A missing file is `Ok(None)` rather than an error: a 404 is the ordinary
//! answer when several repositories are consulted in order, and treating it as a
//! failure would turn a normal resolve into a pile of reported errors.

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;

use jv_repo::Credentials;

/// A transport-level failure. Absence is not one of these.
#[derive(Clone, Debug, thiserror::Error, PartialEq, Eq)]
pub enum TransportError {
    #[error("{url}: {reason}")]
    Request { url: String, reason: String },
    #[error("{url}: the repository answered {status}")]
    Status { url: String, status: u16 },
    #[error("{url}: authentication required")]
    Unauthorized { url: String },
}

/// The future a transport returns. Boxed so the trait stays object-safe, which
/// is what lets the fetcher hold a transport it did not choose.
pub type Fetching<'a> =
    Pin<Box<dyn Future<Output = Result<Option<Vec<u8>>, TransportError>> + Send + 'a>>;

/// Retrieves a URL.
pub trait Transport: Send + Sync {
    /// Fetches a URL, or `Ok(None)` when the repository does not have it.
    fn get<'a>(&'a self, url: &'a str, credentials: &'a Credentials) -> Fetching<'a>;
}

/// The real transport: HTTP for remote repositories, the filesystem for `file:`.
#[derive(Debug)]
pub struct HttpTransport {
    client: reqwest::Client,
}

impl HttpTransport {
    /// Builds a client with the timeouts a resolve needs.
    ///
    /// Connections are pooled and HTTP/2 is enabled, which matters because a
    /// cold resolve is dominated by many small POM and metadata requests to one
    /// host rather than by a few large ones.
    pub fn new() -> Result<Self, TransportError> {
        let client = reqwest::Client::builder()
            .user_agent(concat!("jv/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(std::time::Duration::from_secs(10))
            .timeout(std::time::Duration::from_secs(120))
            .pool_max_idle_per_host(16)
            .build()
            .map_err(|error| TransportError::Request {
                url: "<client>".to_owned(),
                reason: error.to_string(),
            })?;
        Ok(Self { client })
    }
}

impl Transport for HttpTransport {
    fn get<'a>(&'a self, url: &'a str, credentials: &'a Credentials) -> Fetching<'a> {
        Box::pin(async move {
            if let Some(path) = local_path(url) {
                return read_local(&path);
            }

            let mut request = self.client.get(url);
            if let Some(username) = &credentials.username {
                request = request.basic_auth(username, credentials.password.as_ref());
            }
            let response = request
                .send()
                .await
                .map_err(|error| TransportError::Request {
                    url: url.to_owned(),
                    reason: error.to_string(),
                })?;

            let status = response.status();
            if status == reqwest::StatusCode::NOT_FOUND || status == reqwest::StatusCode::GONE {
                return Ok(None);
            }
            if status == reqwest::StatusCode::UNAUTHORIZED
                || status == reqwest::StatusCode::FORBIDDEN
            {
                return Err(TransportError::Unauthorized {
                    url: url.to_owned(),
                });
            }
            if !status.is_success() {
                return Err(TransportError::Status {
                    url: url.to_owned(),
                    status: status.as_u16(),
                });
            }

            let bytes = response
                .bytes()
                .await
                .map_err(|error| TransportError::Request {
                    url: url.to_owned(),
                    reason: error.to_string(),
                })?;
            Ok(Some(bytes.to_vec()))
        })
    }
}

/// The filesystem path a `file:` URL names, if it is one.
fn local_path(url: &str) -> Option<PathBuf> {
    let rest = url
        .strip_prefix("file://")
        .or_else(|| url.strip_prefix("file:"))?;
    // `file:///path` leaves a leading slash; `file://host/path` is not something
    // a Maven repository uses.
    Some(PathBuf::from(rest))
}

fn read_local(path: &PathBuf) -> Result<Option<Vec<u8>>, TransportError> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(TransportError::Request {
            url: path.display().to_string(),
            reason: error.to_string(),
        }),
    }
}

/// A transport backed by a fixed map, for tests and for `--offline` behaviour
/// that must not reach the network even by accident.
#[derive(Debug, Default)]
pub struct MapTransport {
    entries: std::collections::HashMap<String, Vec<u8>>,
    /// URLs that should report a transport failure rather than absence.
    failures: std::collections::HashMap<String, String>,
}

impl MapTransport {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, url: impl Into<String>, bytes: impl Into<Vec<u8>>) -> &mut Self {
        self.entries.insert(url.into(), bytes.into());
        self
    }

    /// Makes a URL fail rather than 404, which is how a broken repository
    /// differs from one that simply does not carry an artifact.
    pub fn fail(&mut self, url: impl Into<String>, reason: impl Into<String>) -> &mut Self {
        self.failures.insert(url.into(), reason.into());
        self
    }
}

impl Transport for MapTransport {
    fn get<'a>(&'a self, url: &'a str, _credentials: &'a Credentials) -> Fetching<'a> {
        let result = match self.failures.get(url) {
            Some(reason) => Err(TransportError::Request {
                url: url.to_owned(),
                reason: reason.clone(),
            }),
            None => Ok(self.entries.get(url).cloned()),
        };
        Box::pin(async move { result })
    }
}

/// A transport that refuses every request, used in offline mode so that a code
/// path which forgot to check cannot silently reach the network.
#[derive(Debug, Default)]
pub struct OfflineTransport;

impl Transport for OfflineTransport {
    fn get<'a>(&'a self, url: &'a str, _credentials: &'a Credentials) -> Fetching<'a> {
        let url = url.to_owned();
        Box::pin(async move {
            Err(TransportError::Request {
                url,
                reason: "jv is offline".to_owned(),
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_map_transport_serves_and_reports_absence() {
        let mut transport = MapTransport::new();
        transport.insert("https://host/a.jar", b"bytes".to_vec());
        let credentials = Credentials::default();

        assert_eq!(
            transport
                .get("https://host/a.jar", &credentials)
                .await
                .unwrap(),
            Some(b"bytes".to_vec())
        );
        // Absence is an answer, not a failure.
        assert_eq!(
            transport
                .get("https://host/missing.jar", &credentials)
                .await
                .unwrap(),
            None
        );
    }

    #[tokio::test]
    async fn a_broken_repository_is_distinguishable_from_an_empty_one() {
        let mut transport = MapTransport::new();
        transport.fail("https://host/a.jar", "connection reset");
        let error = transport
            .get("https://host/a.jar", &Credentials::default())
            .await
            .unwrap_err();
        assert!(error.to_string().contains("connection reset"));
    }

    #[tokio::test]
    async fn offline_refuses_everything() {
        let transport = OfflineTransport;
        let error = transport
            .get("https://host/a.jar", &Credentials::default())
            .await
            .unwrap_err();
        assert!(error.to_string().contains("offline"));
    }

    #[tokio::test]
    async fn file_urls_are_read_from_disk() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.jar");
        std::fs::write(&file, b"local bytes").unwrap();

        let transport = HttpTransport::new().expect("a client");
        let url = format!("file://{}", file.display());
        assert_eq!(
            transport.get(&url, &Credentials::default()).await.unwrap(),
            Some(b"local bytes".to_vec())
        );

        let missing = format!("file://{}", dir.path().join("nope.jar").display());
        assert_eq!(
            transport
                .get(&missing, &Credentials::default())
                .await
                .unwrap(),
            None
        );
    }

    #[test]
    fn file_urls_are_recognised_in_both_spellings() {
        assert_eq!(
            local_path("file:///opt/repo/a.jar"),
            Some(PathBuf::from("/opt/repo/a.jar"))
        );
        assert_eq!(
            local_path("file:/opt/repo/a.jar"),
            Some(PathBuf::from("/opt/repo/a.jar"))
        );
        assert_eq!(local_path("https://host/a.jar"), None);
    }
}
