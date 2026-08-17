//! Recognized version qualifiers and the "shift" each applies when sorting.
//!
//! Port of `org.eclipse.aether.util.version.GenericQualifiers`.

/// A preview qualifier: `alpha`, or `a` immediately followed by a number.
pub const QUALIFIER_ALPHA: i32 = -5;
/// A preview qualifier: `beta`, or `b` immediately followed by a number.
pub const QUALIFIER_BETA: i32 = -4;
/// A preview qualifier: `milestone`, or `m` immediately followed by a number.
pub const QUALIFIER_MILESTONE: i32 = -3;
/// A preview qualifier: release candidate (`rc` or `cr`).
pub const QUALIFIER_RC: i32 = -2;
/// A preview qualifier: `snapshot`.
pub const QUALIFIER_SNAPSHOT: i32 = -1;
/// A final release (`ga`, `final`, `release`) — sorts as if absent.
pub const QUALIFIER_ZERO: i32 = 0;
/// A service pack (`sp`) — sorts *after* the unqualified release.
pub const QUALIFIER_SP: i32 = 1;

pub(crate) const LABEL_ALPHA: &str = "alpha";
pub(crate) const LABEL_BETA: &str = "beta";
pub(crate) const LABEL_MILESTONE: &str = "milestone";

/// Qualifier labels and shifts, in the iteration order of upstream's
/// `LinkedHashMap`. The order is load-bearing for [`qualifier`], which returns
/// the first label found in the input.
const QUALIFIERS: &[(&str, i32)] = &[
    (LABEL_ALPHA, QUALIFIER_ALPHA),
    (LABEL_BETA, QUALIFIER_BETA),
    (LABEL_MILESTONE, QUALIFIER_MILESTONE),
    ("rc", QUALIFIER_RC),
    ("cr", QUALIFIER_RC),
    ("snapshot", QUALIFIER_SNAPSHOT),
    ("ga", QUALIFIER_ZERO),
    ("final", QUALIFIER_ZERO),
    ("release", QUALIFIER_ZERO),
    ("sp", QUALIFIER_SP),
];

/// Looks up an exact, already-lowercased token in the qualifier table.
///
/// Used by the tokenizer, where a whole segment is either a known qualifier or
/// an opaque string.
pub(crate) fn token_qualifier(token: &str) -> Option<i32> {
    QUALIFIERS
        .iter()
        .find(|(label, _)| *label == token)
        .map(|(_, shift)| *shift)
}

/// Detects a qualifier anywhere inside a version string, returning its "shift".
///
/// A negative shift marks a preview version (alpha, beta, milestone, release
/// candidate, snapshot), zero marks a final version (ga, final, release), and a
/// positive shift marks a service pack. `None` means no qualifier was detected.
///
/// This is the fuzzy, whole-string counterpart to the tokenizer's exact lookup:
/// it answers "is this version a preview?" for inputs like `1.0-beta-3` without
/// tokenizing. A label only counts when it is not embedded in a longer word, so
/// `1.0-arc` does not read as a release candidate.
///
/// # Examples
///
/// ```
/// use jv_version::{qualifier, QUALIFIER_ALPHA, QUALIFIER_RC};
///
/// assert_eq!(qualifier("1.0-alpha-1"), Some(QUALIFIER_ALPHA));
/// assert_eq!(qualifier("1.0-a1"), Some(QUALIFIER_ALPHA));
/// assert_eq!(qualifier("1.0-rc2"), Some(QUALIFIER_RC));
/// assert_eq!(qualifier("1.0-arc"), None);
/// assert_eq!(qualifier("1.0"), None);
/// ```
pub fn qualifier(token: &str) -> Option<i32> {
    // Java operates on UTF-16 chars here; collect so index arithmetic below
    // matches its `charAt`/`indexOf` semantics rather than UTF-8 byte offsets.
    let chars: Vec<char> = token.chars().collect();
    // The most trivial preview version is "a1", so anything shorter cannot
    // carry a qualifier.
    if chars.len() <= 1 {
        return None;
    }
    let lower: Vec<char> = token.to_lowercase().chars().collect();

    // Simple case: a full qualifier label appears, surrounded by non-letters so
    // that "rc" is not detected inside "1.0-arc".
    for (label, shift) in QUALIFIERS {
        let label: Vec<char> = label.chars().collect();
        if let Some(pos) = index_of(&lower, &label) {
            let boundary_before = pos == 0 || !lower[pos - 1].is_alphabetic();
            let boundary_after =
                pos + label.len() >= lower.len() || !lower[pos + label.len()].is_alphabetic();
            if boundary_before && boundary_after {
                return Some(*shift);
            }
        }
    }

    // Complex case: 'a', 'b' or 'm' immediately followed by a number.
    for ch in ['a', 'b', 'm'] {
        if let Some(idx) = lower.iter().rposition(|c| *c == ch) {
            if lower.len() > idx + 1 && lower[idx + 1].is_ascii_digit() {
                return Some(match ch {
                    'a' => QUALIFIER_ALPHA,
                    'b' => QUALIFIER_BETA,
                    _ => QUALIFIER_MILESTONE,
                });
            }
        }
    }

    None
}

/// First index at which `needle` occurs in `haystack`, mirroring
/// `String.indexOf` over chars.
fn index_of(haystack: &[char], needle: &[char]) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    if needle.len() > haystack.len() {
        return None;
    }
    (0..=haystack.len() - needle.len()).find(|&i| &haystack[i..i + needle.len()] == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_token_lookup() {
        assert_eq!(token_qualifier("alpha"), Some(QUALIFIER_ALPHA));
        assert_eq!(token_qualifier("cr"), Some(QUALIFIER_RC));
        assert_eq!(token_qualifier("rc"), Some(QUALIFIER_RC));
        assert_eq!(token_qualifier("ga"), Some(QUALIFIER_ZERO));
        assert_eq!(token_qualifier("final"), Some(QUALIFIER_ZERO));
        assert_eq!(token_qualifier("release"), Some(QUALIFIER_ZERO));
        assert_eq!(token_qualifier("sp"), Some(QUALIFIER_SP));
        assert_eq!(token_qualifier("foo"), None);
        // The fuzzy detector's abbreviations are not exact tokens.
        assert_eq!(token_qualifier("a"), None);
    }

    #[test]
    fn label_must_be_surrounded_by_non_letters() {
        assert_eq!(qualifier("1.0-arc"), None);
        assert_eq!(qualifier("1.0-rc"), Some(QUALIFIER_RC));
        assert_eq!(qualifier("1.0-rc-1"), Some(QUALIFIER_RC));
    }

    #[test]
    fn single_char_abbreviations_need_a_following_digit() {
        assert_eq!(qualifier("1.0-a1"), Some(QUALIFIER_ALPHA));
        assert_eq!(qualifier("1.0-b2"), Some(QUALIFIER_BETA));
        assert_eq!(qualifier("1.0-m3"), Some(QUALIFIER_MILESTONE));
        // No digit follows, and no full label is present.
        assert_eq!(qualifier("1.0-x"), None);
    }

    #[test]
    fn too_short_to_qualify() {
        assert_eq!(qualifier("a"), None);
        assert_eq!(qualifier(""), None);
    }
}
