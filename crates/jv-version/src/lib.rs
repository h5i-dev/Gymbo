//! Maven-compatible version ordering, ranges, and constraints.
//!
//! This is a port of the `generic` version scheme from Maven Resolver
//! (`org.eclipse.aether.util.version`), which is the scheme Maven actually uses
//! at runtime. The legacy `org.apache.maven.artifact.versioning.ComparableVersion`
//! survives only in Maven's deprecated `compat/maven-artifact` module and is not
//! on the resolution path, so `GenericVersion` is what jv must agree with.
//!
//! # Scheme summary
//!
//! A version is a sequence of numeric and alphabetic segments. The characters
//! `-`, `_` and `.`, as well as transitions between digits and letters, delimit
//! segments; all delimiters are equivalent. Numeric segments compare
//! numerically, alphabetic segments compare lexicographically and
//! case-insensitively, except for well-known qualifiers which sort before any
//! other string:
//!
//! ```text
//! alpha = a < beta = b < milestone = m < cr = rc < snapshot < final = ga = release < sp
//! ```
//!
//! An empty segment is equivalent to `0`. The tokens `min` and `max` may appear
//! as the final segment to denote the smallest/greatest version with a given
//! prefix; the range form `[M.N.*]` is shorthand for `[M.N.min,M.N.max]`.
//!
//! Numbers and strings are incomparable. Where segments of different kinds would
//! collide, comparison assumes the shorter side is padded with trailing `0` or
//! `ga` segments until the mismatch resolves, so `1-alpha` = `1.0.0-alpha` <
//! `1.0.1-ga` = `1.0.1`.
//!
//! # Examples
//!
//! ```
//! use jv_version::{Constraint, Version};
//!
//! assert!(Version::parse("1.0") < Version::parse("1.0.1"));
//! assert_eq!(Version::parse("1"), Version::parse("1.0.0"));
//! assert!(Version::parse("1.0-SNAPSHOT") < Version::parse("1.0"));
//!
//! let c = Constraint::parse("[1.0,2.0)").unwrap();
//! assert!(c.contains(&Version::parse("1.5")));
//! assert!(!c.contains(&Version::parse("2.0")));
//! ```

mod constraint;
mod error;
mod qualifiers;
mod range;
mod version;

pub use constraint::Constraint;
pub use error::InvalidVersionSpec;
pub use qualifiers::{
    QUALIFIER_ALPHA, QUALIFIER_BETA, QUALIFIER_MILESTONE, QUALIFIER_RC, QUALIFIER_SNAPSHOT,
    QUALIFIER_SP, QUALIFIER_ZERO, qualifier,
};
pub use range::{Bound, Range, RangeSet};
pub use version::{Item, Version};
