//! Version ranges in Maven's mathematical-interval syntax.
//!
//! Port of `org.eclipse.aether.util.version.GenericVersionRange` and
//! `UnionVersionRange`.

use std::fmt;

use crate::error::InvalidVersionSpec;
use crate::version::Version;

/// One end of a range: a version, and whether the range includes it.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Bound {
    version: Version,
    inclusive: bool,
}

impl Bound {
    pub fn new(version: Version, inclusive: bool) -> Self {
        Self { version, inclusive }
    }

    pub fn version(&self) -> &Version {
        &self.version
    }

    pub fn is_inclusive(&self) -> bool {
        self.inclusive
    }
}

/// A single interval, such as `[1.0,2.0)`, `[1.0,)` or `[1.0]`.
///
/// Either end may be absent, meaning unbounded in that direction. A range with
/// no comma pins one exact version and must use `[]`.
///
/// # Examples
///
/// ```
/// use jv_version::{Range, Version};
///
/// let r = Range::parse("[1.0,2.0)").unwrap();
/// assert!(r.contains(&Version::parse("1.0")));
/// assert!(r.contains(&Version::parse("1.9")));
/// assert!(!r.contains(&Version::parse("2.0")));
///
/// // `[1.2.*]` is shorthand for everything in the 1.2 line.
/// let line = Range::parse("[1.2.*]").unwrap();
/// assert!(line.contains(&Version::parse("1.2.7")));
/// assert!(!line.contains(&Version::parse("1.3")));
/// ```
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Range {
    lower: Option<Bound>,
    upper: Option<Bound>,
}

impl Range {
    /// Parses a single interval.
    pub fn parse(spec: &str) -> Result<Self, InvalidVersionSpec> {
        let invalid = |reason: &str| {
            InvalidVersionSpec::new(spec, format!("Invalid version range {spec}, {reason}"))
        };

        let lower_inclusive = match spec.chars().next() {
            Some('[') => true,
            Some('(') => false,
            _ => return Err(invalid("a range must start with either [ or (")),
        };
        let upper_inclusive = match spec.chars().last() {
            Some(']') => true,
            Some(')') => false,
            _ => return Err(invalid("a range must end with either ] or )")),
        };

        // Both delimiters are single-byte ASCII, so byte slicing is safe.
        let body = &spec[1..spec.len() - 1];

        let (lower, upper) = match body.find(',') {
            None => {
                if !lower_inclusive || !upper_inclusive {
                    return Err(invalid("single version must be surrounded by []"));
                }
                let version = body.trim();
                // `[1.2.*]` is shorthand for `[1.2.min,1.2.max]`. The dot is
                // required and is kept: upstream tests `endsWith(".*")` and
                // strips only the star. Firing on a bare `*` would turn `[1.2*]`
                // — an exact version to Maven, matching essentially nothing —
                // into a range over every 1.2.x, and `[*]` into everything.
                if version.ends_with(".*") {
                    let prefix = &version[..version.len() - 1];
                    (
                        Some(Version::parse(&format!("{prefix}min"))),
                        Some(Version::parse(&format!("{prefix}max"))),
                    )
                } else {
                    let exact = Version::parse(version);
                    (Some(exact.clone()), Some(exact))
                }
            }
            Some(index) => {
                let lower_text = body[..index].trim();
                let upper_text = body[index + 1..].trim();

                // More than two bounds, e.g. "(1,2,3)".
                if upper_text.contains(',') {
                    return Err(invalid("bounds may not contain additional ','"));
                }

                let lower = (!lower_text.is_empty()).then(|| Version::parse(lower_text));
                let upper = (!upper_text.is_empty()).then(|| Version::parse(upper_text));

                if let (Some(l), Some(u)) = (&lower, &upper) {
                    if u < l {
                        return Err(invalid("lower bound must not be greater than upper bound"));
                    }
                }
                (lower, upper)
            }
        };

        Ok(Self {
            lower: lower.map(|v| Bound::new(v, lower_inclusive)),
            upper: upper.map(|v| Bound::new(v, upper_inclusive)),
        })
    }

    /// The lower end, or `None` if unbounded below.
    pub fn lower_bound(&self) -> Option<&Bound> {
        self.lower.as_ref()
    }

    /// The upper end, or `None` if unbounded above.
    pub fn upper_bound(&self) -> Option<&Bound> {
        self.upper.as_ref()
    }

    /// Whether `version` falls inside this interval.
    pub fn contains(&self, version: &Version) -> bool {
        if let Some(lower) = &self.lower {
            match lower.version().cmp(version) {
                std::cmp::Ordering::Equal if !lower.is_inclusive() => return false,
                std::cmp::Ordering::Greater => return false,
                _ => {}
            }
        }
        if let Some(upper) = &self.upper {
            match upper.version().cmp(version) {
                std::cmp::Ordering::Equal if !upper.is_inclusive() => return false,
                std::cmp::Ordering::Less => return false,
                _ => {}
            }
        }
        true
    }
}

impl fmt::Display for Range {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.lower {
            Some(lower) => {
                f.write_str(if lower.is_inclusive() { "[" } else { "(" })?;
                write!(f, "{}", lower.version())?;
            }
            None => f.write_str("(")?,
        }
        f.write_str(",")?;
        match &self.upper {
            Some(upper) => {
                write!(f, "{}", upper.version())?;
                f.write_str(if upper.is_inclusive() { "]" } else { ")" })?;
            }
            None => f.write_str(")")?,
        }
        Ok(())
    }
}

/// A union of intervals, as written `[1.0,2.0),[3.0,)`.
///
/// A version satisfies the set when it falls in any member interval. The
/// reported bounds span the whole set: the widest lower and upper ends of its
/// members, preferring an inclusive end when two members tie.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct RangeSet {
    ranges: Vec<Range>,
    lower: Option<Bound>,
    upper: Option<Bound>,
}

impl RangeSet {
    /// Builds a set from intervals, dropping duplicates.
    ///
    /// Upstream keeps members in a `HashSet`, leaving display order up to hash
    /// iteration; jv keeps first-occurrence order instead so output is stable.
    /// Membership and bounds do not depend on order either way.
    pub fn new(ranges: impl IntoIterator<Item = Range>) -> Self {
        let mut unique: Vec<Range> = Vec::new();
        for range in ranges {
            if !unique.contains(&range) {
                unique.push(range);
            }
        }

        let mut lower: Option<Bound> = None;
        for range in &unique {
            match range.lower_bound() {
                // Any member unbounded below leaves the whole set unbounded.
                None => {
                    lower = None;
                    break;
                }
                Some(candidate) => match &lower {
                    None => lower = Some(candidate.clone()),
                    Some(current) => {
                        let rel = candidate.version().cmp(current.version());
                        if rel == std::cmp::Ordering::Less
                            || (rel == std::cmp::Ordering::Equal && !current.is_inclusive())
                        {
                            lower = Some(candidate.clone());
                        }
                    }
                },
            }
        }

        let mut upper: Option<Bound> = None;
        for range in &unique {
            match range.upper_bound() {
                None => {
                    upper = None;
                    break;
                }
                Some(candidate) => match &upper {
                    None => upper = Some(candidate.clone()),
                    Some(current) => {
                        let rel = candidate.version().cmp(current.version());
                        if rel == std::cmp::Ordering::Greater
                            || (rel == std::cmp::Ordering::Equal && !current.is_inclusive())
                        {
                            upper = Some(candidate.clone());
                        }
                    }
                },
            }
        }

        Self {
            ranges: unique,
            lower,
            upper,
        }
    }

    /// The member intervals, in first-occurrence order.
    pub fn ranges(&self) -> &[Range] {
        &self.ranges
    }

    /// The widest lower end across the set, or `None` if unbounded below.
    pub fn lower_bound(&self) -> Option<&Bound> {
        self.lower.as_ref()
    }

    /// The widest upper end across the set, or `None` if unbounded above.
    pub fn upper_bound(&self) -> Option<&Bound> {
        self.upper.as_ref()
    }

    /// Whether `version` falls in any member interval.
    pub fn contains(&self, version: &Version) -> bool {
        self.ranges.iter().any(|r| r.contains(version))
    }
}

impl fmt::Display for RangeSet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, range) in self.ranges.iter().enumerate() {
            if i > 0 {
                f.write_str(", ")?;
            }
            write!(f, "{range}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        Version::parse(s)
    }

    #[test]
    fn bounded_range() {
        let r = Range::parse("[1.0,2.0)").unwrap();
        assert_eq!(r.lower_bound().unwrap().version(), &v("1.0"));
        assert!(r.lower_bound().unwrap().is_inclusive());
        assert_eq!(r.upper_bound().unwrap().version(), &v("2.0"));
        assert!(!r.upper_bound().unwrap().is_inclusive());
        assert!(r.contains(&v("1.0")));
        assert!(!r.contains(&v("2.0")));
        assert!(!r.contains(&v("0.9")));
    }

    #[test]
    fn open_ends() {
        let up = Range::parse("[1.0,)").unwrap();
        assert!(up.upper_bound().is_none());
        assert!(up.contains(&v("99")));
        assert!(!up.contains(&v("0.1")));

        let down = Range::parse("(,2.0]").unwrap();
        assert!(down.lower_bound().is_none());
        assert!(down.contains(&v("0.1")));
        assert!(down.contains(&v("2.0")));
        assert!(!down.contains(&v("2.1")));
    }

    #[test]
    fn exact_version_range() {
        let r = Range::parse("[1.0]").unwrap();
        assert!(r.contains(&v("1.0")));
        assert!(r.contains(&v("1"))); // equal under the scheme
        assert!(!r.contains(&v("1.0.1")));
    }

    #[test]
    fn wildcard_range_covers_the_line() {
        let r = Range::parse("[1.2.*]").unwrap();
        assert!(r.contains(&v("1.2")));
        assert!(r.contains(&v("1.2.0")));
        assert!(r.contains(&v("1.2.9.9")));
        assert!(!r.contains(&v("1.3")));
        assert!(!r.contains(&v("1.1")));
    }

    #[test]
    fn malformed_ranges_are_rejected() {
        for spec in [
            "1.0",       // no brackets
            "[1.0,2.0",  // unterminated
            "1.0,2.0]",  // no opening bracket
            "(1.0)",     // single version needs []
            "[1.0)",     // single version needs []
            "(1,2,3)",   // too many bounds
            "[2.0,1.0]", // inverted
        ] {
            assert!(Range::parse(spec).is_err(), "{spec} should be rejected");
        }
    }

    #[test]
    fn display_round_trips() {
        for spec in ["[1.0,2.0)", "(1.0,2.0]", "[1.0,)", "(,2.0)"] {
            assert_eq!(Range::parse(spec).unwrap().to_string(), spec);
        }
    }

    #[test]
    fn union_spans_widest_bounds() {
        let set = RangeSet::new([
            Range::parse("[1.0,2.0)").unwrap(),
            Range::parse("[3.0,4.0]").unwrap(),
        ]);
        assert_eq!(set.lower_bound().unwrap().version(), &v("1.0"));
        assert_eq!(set.upper_bound().unwrap().version(), &v("4.0"));
        assert!(set.contains(&v("1.5")));
        assert!(!set.contains(&v("2.5"))); // the gap
        assert!(set.contains(&v("3.5")));
    }

    #[test]
    fn union_with_an_open_member_is_open() {
        let set = RangeSet::new([
            Range::parse("[1.0,2.0)").unwrap(),
            Range::parse("[3.0,)").unwrap(),
        ]);
        assert!(set.upper_bound().is_none());
        assert!(set.lower_bound().is_some());
    }

    #[test]
    fn union_prefers_inclusive_on_ties() {
        let set = RangeSet::new([
            Range::parse("(1.0,2.0)").unwrap(),
            Range::parse("[1.0,1.5)").unwrap(),
        ]);
        assert!(set.lower_bound().unwrap().is_inclusive());
    }

    #[test]
    fn union_dedups_and_keeps_order() {
        let set = RangeSet::new([
            Range::parse("[3.0,4.0]").unwrap(),
            Range::parse("[1.0,2.0)").unwrap(),
            Range::parse("[3.0,4.0]").unwrap(),
        ]);
        assert_eq!(set.ranges().len(), 2);
        assert_eq!(set.to_string(), "[3.0,4.0], [1.0,2.0)");
    }

    #[test]
    fn empty_union_contains_nothing() {
        let set = RangeSet::new([]);
        assert!(!set.contains(&v("1.0")));
        assert!(set.lower_bound().is_none());
        assert!(set.upper_bound().is_none());
    }
}
