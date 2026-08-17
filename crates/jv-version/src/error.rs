use std::fmt;

/// A version range or constraint that could not be parsed.
///
/// Mirrors `org.eclipse.aether.version.InvalidVersionSpecificationException`:
/// it carries the offending specification alongside the reason so callers can
/// report both without re-deriving either.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InvalidVersionSpec {
    spec: String,
    reason: String,
}

impl InvalidVersionSpec {
    pub(crate) fn new(spec: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            spec: spec.into(),
            reason: reason.into(),
        }
    }

    /// The specification that failed to parse.
    pub fn spec(&self) -> &str {
        &self.spec
    }

    /// Why it failed to parse.
    pub fn reason(&self) -> &str {
        &self.reason
    }
}

impl fmt::Display for InvalidVersionSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.reason)
    }
}

impl std::error::Error for InvalidVersionSpec {}
