//! Maven repositories: where artifacts live, which ones to ask, and how often.
//!
//! Three concerns, deliberately separated. [`layout`] turns coordinates into
//! paths and is pure arithmetic. [`policy`] decides when a cached file is stale
//! and what a bad checksum costs. [`repository`] turns a POM's declared
//! repositories into the ones jv will actually contact, which is where
//! `settings.xml` mirrors and credentials apply, and [`settings`] merges the
//! installation's `settings.xml` with the user's before any of that runs.
//!
//! Downloading is `jv-cache`'s job; nothing here touches the network.

pub mod layout;
pub mod policy;
pub mod proxy;
pub mod repository;
pub mod settings;

pub use layout::{
    Checksum, METADATA_FILE, MetadataLocation, artifact_path, checksum_path, join_url,
};
pub use policy::{ChecksumPolicy, Policy, UpdatePolicy};
pub use proxy::{ProxyEndpoint, ProxySelector};
pub use repository::{
    CENTRAL_ID, CENTRAL_URL, Credentials, Repository, Trust, from_model, resolve_repositories,
    resolve_with_trust,
};
pub use settings::merge_settings;
