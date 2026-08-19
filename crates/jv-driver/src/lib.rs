//! Where the pure crates meet the machine.
//!
//! `jv-version`, `jv-model`, `jv-model-builder` and `jv-resolver` are all pure:
//! given the same inputs they produce the same outputs, and none of them knows
//! that a network exists. That is what makes them testable against Maven's own
//! corpora. This crate is the other half — the one that reads `settings.xml`,
//! decides which repositories to contact, fetches POMs, and feeds the result
//! through collection and conflict resolution.
//!
//! Start at [`Session`]. Everything else here is what it is built from.
//!
//! ```no_run
//! use jv_driver::{Config, Session};
//! use jv_resolver::Verbosity;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let session = Session::new(&Config::new())?;
//! let project = session.project(std::path::Path::new("."))?;
//! let resolution = session.resolve_project(&project, Verbosity::None)?;
//! println!("{} nodes", resolution.collected.graph.len());
//! # Ok(())
//! # }
//! ```

pub mod config;
pub mod error;
pub mod java;
pub mod mvn_config;
pub mod prefetch;
pub mod project;
pub mod session;
pub mod snapshot;
pub mod source;
pub mod sync;
pub mod tracking;

pub use config::Config;
pub use error::DriverError;
pub use project::{Project, find_pom, load_project};
pub use session::{Resolution, Session};
pub use snapshot::LocalSnapshot;
pub use source::{Materialized, RepositorySource};
pub use sync::{SyncReport, SyncRequest, sync};
pub use tracking::Tracking;
