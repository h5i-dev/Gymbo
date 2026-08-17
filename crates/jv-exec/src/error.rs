//! What can go wrong between a coordinate and a running JVM.

use std::path::PathBuf;

use crate::endpoint::EndpointError;

/// Launching a tool failed.
#[derive(Debug, thiserror::Error)]
pub enum ExecError {
    #[error(transparent)]
    Endpoint(#[from] EndpointError),
    #[error("no Java runtime found; set JAVA_HOME or put `java` on PATH")]
    NoJava,
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{path} is not a readable jar: {source}")]
    Jar {
        path: PathBuf,
        #[source]
        source: zip::result::ZipError,
    },
    /// Deliberately carries what was tried rather than a bare "no main class":
    /// the useful half of this failure is knowing which rung of the ladder was
    /// expected to answer, because that is what tells the user whether to pass
    /// `--main` or to check they named the right artifact.
    #[error("cannot tell which class to run for {endpoint}; tried {}", .tried.join(", then "))]
    NoMainClass {
        endpoint: String,
        tried: Vec<String>,
    },
    #[error("no published version of {group_id}:{artifact_id} was found")]
    NoVersion {
        group_id: String,
        artifact_id: String,
    },
    #[error("cannot run {java}: {source}")]
    Launch {
        java: PathBuf,
        #[source]
        source: std::io::Error,
    },
}
