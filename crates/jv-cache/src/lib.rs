//! Getting repository files onto the local disk, and not getting them twice.
//!
//! Four pieces, layered:
//!
//! - [`store`] owns the on-disk cache: URL-keyed paths, atomic writes, and the
//!   sidecars that record absence and freshness.
//! - [`transport`] is the byte-moving interface — HTTP, `file:`, or a scripted
//!   map for tests.
//! - [`checksum`] verifies a download against what the repository published.
//! - [`fetch`] is the policy that ties them together: cache, then `~/.m2`, then
//!   the repositories in order.
//!
//! Everything above this crate — descriptor reading, collection, the CLI — asks
//! [`Fetcher`] for bytes and never learns where they came from.
//!
//! ```no_run
//! use jv_cache::{Fetcher, HttpTransport, Store};
//! use jv_model::Artifact;
//! use jv_repo::Repository;
//!
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! let store = Store::default_location().expect("a cache directory");
//! let fetcher = Fetcher::new(store, Box::new(HttpTransport::new()?));
//!
//! let pom = fetcher
//!     .artifact(
//!         &[Repository::central()],
//!         &Artifact::new("org.slf4j", "slf4j-api", "2.0.9").with_extension("pom"),
//!     )
//!     .await?;
//! println!("{} bytes from {:?}", pom.bytes.len(), pom.origin);
//! # Ok(())
//! # }
//! ```

pub mod checksum;
pub mod fetch;
pub mod store;
pub mod transport;

pub use checksum::{ChecksumError, PREFERRED, digest, parse_published, verify};
pub use fetch::{FetchError, Fetched, Fetcher, Origin};
pub use store::{DownloadLock, FORMAT_VERSION, Locking, Store, StoreError};
pub use transport::{HttpTransport, MapTransport, OfflineTransport, Transport, TransportError};

/// Maven's local repository: `<localRepository>` from `settings.xml` if it
/// declares one, otherwise `~/.m2/repository`.
///
/// Returns `None` when the directory does not exist, so a machine that has never
/// run Maven simply has nothing to read rather than a path that always misses.
///
/// jv reads this directory but never writes to it: it belongs to another tool,
/// and a half-written file there would break Maven builds rather than just jv's.
pub fn maven_local_repository(settings: &jv_model::Settings) -> Option<std::path::PathBuf> {
    let candidate = match settings.local_repository.as_deref() {
        Some(declared) if !declared.trim().is_empty() => std::path::PathBuf::from(declared.trim()),
        _ => dirs::home_dir()?.join(".m2").join("repository"),
    };
    candidate.is_dir().then_some(candidate)
}
