//! Maven repositories: where artifacts live and how they are addressed.
//!
//! Work in progress. [`layout`] is complete; repository resolution (mirrors,
//! authentication, update policies) and the download driver follow. See
//! `ROADMAP.md` M4.

pub mod layout;

pub use layout::{
    Checksum, METADATA_FILE, MetadataLocation, artifact_path, checksum_path, join_url,
};
