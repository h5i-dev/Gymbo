//! Version constraints: what a POM's `<version>` element may say.
//!
//! Port of `org.eclipse.aether.util.version.GenericVersionConstraint` and the
//! parsing in `VersionSchemeSupport#parseVersionConstraint`.

use std::fmt;

use crate::error::InvalidVersionSpec;
use crate::range::{Bound, Range, RangeSet};
use crate::version::Version;

/// A constraint on the version of a dependency.
///
/// A constraint is either a single version — Maven calls this a *soft*
/// requirement, since a nearer declaration may override it — or a set of
/// ranges, which is a *hard* requirement.
///
/// # Examples
///
/// ```
/// use jv_version::{Constraint, Version};
///
/// // A bare version is a soft requirement on exactly that version.
/// let soft = Constraint::parse("1.0").unwrap();
/// assert!(soft.version().is_some());
/// assert!(soft.contains(&Version::parse("1.0")));
/// assert!(!soft.contains(&Version::parse("1.1")));
///
/// // Ranges may be unioned with commas.
/// let hard = Constraint::parse("[1.0,2.0),[3.0,)").unwrap();
/// assert!(hard.range_set().is_some());
/// assert!(hard.contains(&Version::parse("1.5")));
/// assert!(!hard.contains(&Version::parse("2.5")));
/// assert!(hard.contains(&Version::parse("4.0")));
/// ```
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Constraint {
    /// A bare version: a soft requirement, overridable by a nearer declaration.
    Version(Version),
    /// One or more ranges: a hard requirement.
    Ranges(RangeSet),
}

impl Constraint {
    /// Parses a constraint specification.
    ///
    /// Anything that does not start with `[` or `(` is taken as a bare version,
    /// which always parses; only range syntax can fail.
    pub fn parse(spec: &str) -> Result<Self, InvalidVersionSpec> {
        let mut rest = spec;
        let mut ranges: Vec<Range> = Vec::new();

        while rest.starts_with('[') || rest.starts_with('(') {
            // Take up to whichever closing bracket comes first; a range never
            // nests, so the first one always ends this member.
            let close = match (rest.find(')'), rest.find(']')) {
                (None, None) => {
                    return Err(InvalidVersionSpec::new(
                        spec,
                        format!("Unbounded version range {spec}"),
                    ));
                }
                (Some(paren), None) => paren,
                (None, Some(bracket)) => bracket,
                (Some(paren), Some(bracket)) => paren.min(bracket),
            };

            ranges.push(Range::parse(&rest[..=close])?);

            rest = rest[close + 1..].trim();
            if let Some(stripped) = rest.strip_prefix(',') {
                rest = stripped.trim();
            }
        }

        if !rest.is_empty() && !ranges.is_empty() {
            return Err(InvalidVersionSpec::new(
                spec,
                format!("Invalid version range {spec}, expected [ or ( but got {rest}"),
            ));
        }

        if ranges.is_empty() {
            Ok(Constraint::Version(Version::parse(spec)))
        } else {
            Ok(Constraint::Ranges(RangeSet::new(ranges)))
        }
    }

    /// The single version, if this is a soft requirement.
    pub fn version(&self) -> Option<&Version> {
        match self {
            Constraint::Version(v) => Some(v),
            Constraint::Ranges(_) => None,
        }
    }

    /// The ranges, if this is a hard requirement.
    pub fn range_set(&self) -> Option<&RangeSet> {
        match self {
            Constraint::Version(_) => None,
            Constraint::Ranges(r) => Some(r),
        }
    }

    /// The lower end across the whole constraint, if it is a range set.
    pub fn lower_bound(&self) -> Option<&Bound> {
        self.range_set().and_then(RangeSet::lower_bound)
    }

    /// The upper end across the whole constraint, if it is a range set.
    pub fn upper_bound(&self) -> Option<&Bound> {
        self.range_set().and_then(RangeSet::upper_bound)
    }

    /// Whether `version` satisfies this constraint.
    ///
    /// A soft requirement is satisfied only by an equal version; note that
    /// equality is the scheme's, so `1.0` satisfies a constraint of `1`.
    pub fn contains(&self, version: &Version) -> bool {
        match self {
            Constraint::Version(v) => v == version,
            Constraint::Ranges(r) => r.contains(version),
        }
    }
}

impl fmt::Display for Constraint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Constraint::Version(v) => write!(f, "{v}"),
            Constraint::Ranges(r) => write!(f, "{r}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        Version::parse(s)
    }

    #[test]
    fn bare_version_is_soft() {
        let c = Constraint::parse("1.0").unwrap();
        assert_eq!(c.version(), Some(&v("1.0")));
        assert!(c.range_set().is_none());
        assert!(c.contains(&v("1.0")));
        assert!(c.contains(&v("1"))); // equal under the scheme
        assert!(!c.contains(&v("1.0.1")));
    }

    #[test]
    fn single_range() {
        let c = Constraint::parse("[1.0,2.0)").unwrap();
        assert!(c.version().is_none());
        assert_eq!(c.range_set().unwrap().ranges().len(), 1);
        assert!(c.contains(&v("1.5")));
    }

    #[test]
    fn union_of_ranges() {
        let c = Constraint::parse("[1.0,2.0),[3.0,4.0]").unwrap();
        assert_eq!(c.range_set().unwrap().ranges().len(), 2);
        assert!(c.contains(&v("1.0")));
        assert!(!c.contains(&v("2.5")));
        assert!(c.contains(&v("4.0")));
        assert_eq!(c.lower_bound().unwrap().version(), &v("1.0"));
        assert_eq!(c.upper_bound().unwrap().version(), &v("4.0"));
    }

    #[test]
    fn whitespace_between_members_is_tolerated() {
        let c = Constraint::parse("[1.0,2.0) , [3.0,4.0]").unwrap();
        assert_eq!(c.range_set().unwrap().ranges().len(), 2);
    }

    #[test]
    fn trailing_junk_after_a_range_is_rejected() {
        let err = Constraint::parse("[1.0,2.0)oops").unwrap_err();
        assert!(err.reason().contains("expected [ or ( but got oops"));
    }

    #[test]
    fn unterminated_range_is_rejected() {
        let err = Constraint::parse("[1.0,2.0").unwrap_err();
        assert!(err.reason().contains("Unbounded version range"));
    }

    #[test]
    fn display_round_trips() {
        assert_eq!(Constraint::parse("1.0").unwrap().to_string(), "1.0");
        assert_eq!(
            Constraint::parse("[1.0,2.0)").unwrap().to_string(),
            "[1.0,2.0)"
        );
    }
}
