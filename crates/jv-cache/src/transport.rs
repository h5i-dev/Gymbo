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

use jv_repo::{Credentials, ProxySelector};

/// A transport-level failure. Absence is not one of these.
#[derive(Clone, Debug, thiserror::Error, PartialEq, Eq)]
pub enum TransportError {
    #[error("{url}: {reason}")]
    Request { url: String, reason: String },
    #[error("{url}: the repository answered {status}")]
    Status {
        url: String,
        status: u16,
        /// `Retry-After`, in seconds, when the server sent one. A throttling
        /// repository usually says how long to wait, and guessing shorter than
        /// it asked is how a retry storm starts.
        retry_after: Option<u64>,
    },
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
        Self::with_proxies(ProxySelector::default())
    }

    /// The same client, routing through the `settings.xml` `<proxies>`.
    ///
    /// Selection is per URL rather than per client because Maven's is: which
    /// proxy applies depends on the repository's protocol *and* on that proxy's
    /// own `<nonProxyHosts>`, so one client cannot be bound to one proxy.
    ///
    /// With no active proxy configured the builder is left alone, which keeps
    /// reqwest's own `HTTPS_PROXY` / `NO_PROXY` handling. A configured proxy
    /// takes precedence over the environment, as Maven's does.
    pub fn with_proxies(selector: ProxySelector) -> Result<Self, TransportError> {
        let mut builder = reqwest::Client::builder()
            .user_agent(concat!("jv/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(std::time::Duration::from_secs(10))
            .timeout(std::time::Duration::from_secs(120))
            .pool_max_idle_per_host(64)
            // HTTP/2 multiplexes every request to a host over one TCP
            // connection, so that connection's flow-control window is the
            // ceiling for the whole sync. hyper's default is a fixed 64 KB,
            // which on any link with real latency caps throughput far below
            // what the network can do — jv measured 8.3 MB/s where `curl`
            // over the same protocol to the same host managed 20 MB/s.
            //
            // The adaptive window lets hyper size it from an estimate of the
            // bandwidth-delay product instead, which is what curl does.
            .http2_adaptive_window(true);

        if !selector.is_empty() {
            builder = builder.proxy(reqwest::Proxy::custom(move |url| {
                selector
                    .select(url.as_str())
                    .and_then(|endpoint| reqwest::Url::parse(&endpoint.url).ok())
            }));
        }

        let client = builder.build().map_err(|error| TransportError::Request {
            url: "<client>".to_owned(),
            reason: error.to_string(),
        })?;
        Ok(Self { client })
    }
}

/// How many times a transient failure is retried before it is reported.
///
/// Three attempts covers the failure that actually happens — a connection
/// dropped under load, or a moment of 503 from a CDN edge — without turning a
/// genuinely unreachable repository into a long wait.
const ATTEMPTS: usize = 5;

/// The longest jv will wait between attempts, however long the server asks.
///
/// A repository is allowed to say "come back in an hour"; a build is not
/// allowed to sit there for it.
const MAX_BACKOFF: std::time::Duration = std::time::Duration::from_secs(30);

/// The wait before retry *n*, doubling.
///
/// Short, because the failures being retried are transient by definition and a
/// resolve blocked behind a slow backoff is worse than one that retries
/// eagerly. Rate limiting is the exception — see [`backoff_for`].
fn backoff(attempt: usize) -> std::time::Duration {
    std::time::Duration::from_millis(100 << attempt)
}

/// The wait before retry *n* for a particular failure.
///
/// A 429 is not a dropped connection. The server is asking for less traffic,
/// and 100 ms later is not less traffic: jv used to give up after three tries
/// spanning 700 ms and fail the whole resolve with "the repository answered
/// 429", which is how a rate-limited sync turned into "cannot read POM for
/// spring-boot-starter-parent" — an error naming neither the cause nor the
/// remedy.
///
/// So a throttle gets seconds rather than milliseconds, and `Retry-After` wins
/// over any guess when the server sent one.
fn backoff_for(error: &TransportError, attempt: usize) -> std::time::Duration {
    match error {
        TransportError::Status {
            status: 429,
            retry_after,
            ..
        } => retry_after
            .map(std::time::Duration::from_secs)
            .unwrap_or_else(|| std::time::Duration::from_secs(1 << attempt))
            .min(MAX_BACKOFF),
        TransportError::Status {
            retry_after: Some(seconds),
            ..
        } => std::time::Duration::from_secs(*seconds).min(MAX_BACKOFF),
        _ => backoff(attempt),
    }
}

impl Transport for HttpTransport {
    fn get<'a>(&'a self, url: &'a str, credentials: &'a Credentials) -> Fetching<'a> {
        Box::pin(async move {
            if let Some(path) = local_path(url) {
                return read_local(&path);
            }

            // Retrying matters more for correctness than for speed. A dropped
            // connection while fetching an imported BOM does not fail the
            // resolve — it makes the BOM's managed versions silently absent, and
            // the build then picks different versions than it should. A resolve
            // must not depend on the network having a good minute.
            let mut last: Option<TransportError> = None;
            for attempt in 0..ATTEMPTS {
                if attempt > 0 {
                    let wait = last
                        .as_ref()
                        .map(|error| backoff_for(error, attempt - 1))
                        .unwrap_or_else(|| backoff(attempt - 1));
                    tokio::time::sleep(wait).await;
                }
                match self.attempt(url, credentials).await {
                    Ok(result) => return Ok(result),
                    Err(error) if is_transient(&error) => last = Some(error),
                    Err(error) => return Err(error),
                }
            }
            Err(last.unwrap_or_else(|| TransportError::Request {
                url: url.to_owned(),
                reason: "no attempt was made".to_owned(),
            }))
        })
    }
}

impl HttpTransport {
    /// One request, with no retrying of its own.
    async fn attempt(
        &self,
        url: &str,
        credentials: &Credentials,
    ) -> Result<Option<Vec<u8>>, TransportError> {
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
        if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
            return Err(TransportError::Unauthorized {
                url: url.to_owned(),
            });
        }
        if !status.is_success() {
            // `Retry-After` is either a delay in seconds or an HTTP date; only
            // the numeric form is honoured, because the date form needs a clock
            // comparison to be worth anything and servers that throttle send
            // seconds.
            let retry_after = response
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.trim().parse::<u64>().ok());
            return Err(TransportError::Status {
                url: url.to_owned(),
                status: status.as_u16(),
                retry_after,
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
    }
}

/// Whether a failure is worth trying again.
///
/// Transport-level failures are: the connection did not survive. Status-level
/// ones are retried only where the server is saying "not now" — 5xx, 408, 429 —
/// and never where it is saying "not this", which would just repeat the same
/// answer more slowly.
fn is_transient(error: &TransportError) -> bool {
    match error {
        TransportError::Request { .. } => true,
        TransportError::Status { status, .. } => *status >= 500 || *status == 408 || *status == 429,
        TransportError::Unauthorized { .. } => false,
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
    /// Every URL asked for, in order, so a test can assert that something was
    /// *not* asked twice. Counting requests is the only way to observe a
    /// circuit breaker: its whole effect is a request that does not happen.
    asked: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    /// Whole hosts that fail, by URL prefix.
    failing_hosts: Vec<(String, String)>,
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

    /// Makes every URL under a repository fail, which is what a host that no
    /// longer resolves looks like.
    pub fn fail_host(&mut self, prefix: impl Into<String>, reason: impl Into<String>) -> &mut Self {
        self.failing_hosts.push((prefix.into(), reason.into()));
        self
    }

    /// A handle to the request log, kept by the test after the transport is
    /// boxed into a `Fetcher` and no longer reachable.
    pub fn requests(&self) -> std::sync::Arc<std::sync::Mutex<Vec<String>>> {
        std::sync::Arc::clone(&self.asked)
    }

    /// How many times a URL beginning with `prefix` was asked for.
    pub fn asked_under(&self, prefix: &str) -> usize {
        self.asked
            .lock()
            .expect("asked")
            .iter()
            .filter(|url| url.starts_with(prefix))
            .count()
    }
}

impl Transport for MapTransport {
    fn get<'a>(&'a self, url: &'a str, _credentials: &'a Credentials) -> Fetching<'a> {
        self.asked.lock().expect("asked").push(url.to_owned());
        let host_failure = self
            .failing_hosts
            .iter()
            .find(|(prefix, _)| url.starts_with(prefix.as_str()))
            .map(|(_, reason)| reason.clone());
        let result = match host_failure.or_else(|| self.failures.get(url).cloned()) {
            Some(reason) => Err(TransportError::Request {
                url: url.to_owned(),
                reason,
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
    fn a_throttle_waits_seconds_rather_than_milliseconds() {
        // The bug this encodes: three tries spanning 700ms against a server
        // asking for less traffic, then failing the resolve outright.
        let throttled = TransportError::Status {
            url: "u".to_owned(),
            status: 429,
            retry_after: None,
        };
        assert_eq!(backoff_for(&throttled, 0).as_secs(), 1);
        assert_eq!(backoff_for(&throttled, 2).as_secs(), 4);
        assert!(
            backoff_for(&throttled, 0) > backoff(0) * 5,
            "a rate limit must not be retried on the connection-drop schedule"
        );
    }

    #[test]
    fn retry_after_beats_the_guess() {
        let asked = TransportError::Status {
            url: "u".to_owned(),
            status: 429,
            retry_after: Some(7),
        };
        assert_eq!(backoff_for(&asked, 0).as_secs(), 7);
    }

    #[test]
    fn a_server_cannot_park_the_build_indefinitely() {
        let greedy = TransportError::Status {
            url: "u".to_owned(),
            status: 503,
            retry_after: Some(3600),
        };
        assert_eq!(backoff_for(&greedy, 0), MAX_BACKOFF);
    }

    #[test]
    fn an_ordinary_transient_failure_keeps_the_short_schedule() {
        // Only throttling gets the long wait; a dropped connection is still
        // retried eagerly, because waiting does not help it.
        let dropped = TransportError::Request {
            url: "u".to_owned(),
            reason: "connection reset".to_owned(),
        };
        assert_eq!(backoff_for(&dropped, 0), backoff(0));
    }

    #[test]
    fn only_failures_worth_repeating_are_retried() {
        let request = TransportError::Request {
            url: "u".to_owned(),
            reason: "connection reset".to_owned(),
        };
        assert!(is_transient(&request));
        // "Not now" is worth asking again.
        for status in [500, 502, 503, 408, 429] {
            assert!(is_transient(&TransportError::Status {
                url: "u".to_owned(),
                status,
                retry_after: None
            }));
        }
        // "Not this" is not: repeating it only makes the same answer slower.
        for status in [400, 405, 410, 451] {
            assert!(!is_transient(&TransportError::Status {
                url: "u".to_owned(),
                status,
                retry_after: None
            }));
        }
        assert!(!is_transient(&TransportError::Unauthorized {
            url: "u".to_owned()
        }));
    }

    #[test]
    fn backoff_grows_and_stays_short() {
        assert_eq!(backoff(0), std::time::Duration::from_millis(100));
        assert_eq!(backoff(1), std::time::Duration::from_millis(200));
        // The failures on this schedule are transient; a resolve stuck behind a
        // long backoff is worse than one that retries eagerly and gives up.
        // Bounded on the total rather than on one step, because the step count
        // rose with `ATTEMPTS` and what matters is how long a doomed fetch
        // holds the resolve up.
        let total: std::time::Duration = (0..ATTEMPTS - 1).map(backoff).sum();
        assert!(
            total <= std::time::Duration::from_secs(2),
            "the connection-drop schedule should stay under two seconds, was {total:?}"
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
