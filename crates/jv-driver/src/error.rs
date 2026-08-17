//! What can go wrong once a resolve touches the world.
//!
//! The pure crates each have their own error type; this is where they meet, plus
//! the failures that only exist because there is a filesystem and a network.

use std::path::PathBuf;

/// A resolve failed.
#[derive(Debug, thiserror::Error)]
pub enum DriverError {
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{path} is not a readable settings.xml: {source}")]
    Settings {
        path: PathBuf,
        #[source]
        source: jv_model::ParseError,
    },
    #[error("{source_name} is not a readable POM: {source}")]
    Pom {
        source_name: String,
        #[source]
        source: jv_model::ParseError,
    },
    #[error("cannot build the effective model for {source_name}: {source}")]
    Model {
        source_name: String,
        #[source]
        source: jv_model_builder::BuildError,
    },
    #[error(transparent)]
    Fetch(#[from] jv_cache::FetchError),
    #[error(transparent)]
    Collect(#[from] jv_resolver::CollectError),
    #[error(transparent)]
    Resolve(#[from] jv_resolver::ResolveError),
    #[error("no pom.xml in {0} or any directory above it")]
    NoProject(PathBuf),
    #[error("jv's cache directory cannot be determined; pass one explicitly")]
    NoCacheDirectory,
    #[error("{0}")]
    Other(String),
}
