//! Problems found while building a model.
//!
//! Maven reports most model defects without failing: a POM that Maven builds
//! with warnings must not be one jv refuses. Only a defect that makes the model
//! unusable — an unreadable parent, an unparseable POM — stops the build.

use std::fmt;

/// How serious a model problem is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    /// Worth telling the user, but the model is usable.
    Warning,
    /// The model is wrong in a way that will probably produce wrong results,
    /// but Maven still proceeds, so jv does too.
    Error,
}

/// One problem, with enough context to act on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Problem {
    pub severity: Severity,
    pub message: String,
    /// Which POM the problem was found in, as a coordinate or a path.
    pub source: String,
}

impl Problem {
    pub fn warning(source: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Warning,
            message: message.into(),
            source: source.into(),
        }
    }

    pub fn error(source: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Error,
            message: message.into(),
            source: source.into(),
        }
    }
}

impl fmt::Display for Problem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let label = match self.severity {
            Severity::Warning => "warning",
            Severity::Error => "error",
        };
        write!(f, "{label}: {} ({})", self.message, self.source)
    }
}

/// A model that could not be built at all.
#[derive(Debug, thiserror::Error)]
pub enum BuildError {
    #[error("cannot read POM for {coordinates}: {reason}")]
    MissingModel { coordinates: String, reason: String },
    #[error("cannot parse POM at {location}: {error}")]
    Parse {
        location: String,
        error: jv_model::ParseError,
    },
    #[error("the parent chain of {coordinates} is circular; it revisits {repeated}")]
    ParentCycle {
        coordinates: String,
        repeated: String,
    },
    #[error("{coordinates} declares no artifactId, so it cannot be identified")]
    NoArtifactId { coordinates: String },
}
