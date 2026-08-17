//! Version tokenization and comparison.
//!
//! Port of `org.eclipse.aether.util.version.GenericVersion`. The algorithm and
//! its edge cases are subtle enough that this file follows upstream's structure
//! closely on purpose: tokenizer state machine, padding trim, then a
//! kind-aware comparison that pads the shorter side on demand.

use std::cmp::Ordering;
use std::fmt;
use std::hash::{Hash, Hasher};

use crate::qualifiers::{LABEL_ALPHA, LABEL_BETA, LABEL_MILESTONE, token_qualifier};

/// Kind discriminants from upstream's `GenericVersion.Item`.
///
/// The values are load-bearing twice over: item comparison falls back to the
/// difference of two kinds, and [`Item::is_number`] tests `kind & 2 == 0`. Do
/// not renumber them.
const KIND_MIN: u8 = 0;
const KIND_QUALIFIER: u8 = 2;
const KIND_STRING: u8 = 3;
const KIND_INT: u8 = 4;
const KIND_BIGINT: u8 = 5;
const KIND_MAX: u8 = 8;

/// One segment of a tokenized version.
///
/// Exposed so tools can explain a comparison (`jv` surfaces this when a user
/// disputes a resolved version), not because callers need to build them.
#[derive(Clone, Debug)]
pub enum Item {
    /// The literal final segment `min`: smaller than any padding.
    Min,
    /// A recognized qualifier, carrying its sort shift.
    Qualifier(i32),
    /// An unrecognized alphabetic segment, always lowercased.
    Str(String),
    /// A numeric segment that fits comfortably in a machine integer.
    Int(u64),
    /// A numeric segment too long for [`Item::Int`], kept as normalized decimal
    /// digits with no leading zeros.
    BigInt(String),
    /// The literal final segment `max`: greater than any padding.
    Max,
}

impl Item {
    fn kind(&self) -> u8 {
        match self {
            Item::Min => KIND_MIN,
            Item::Qualifier(_) => KIND_QUALIFIER,
            Item::Str(_) => KIND_STRING,
            Item::Int(_) => KIND_INT,
            Item::BigInt(_) => KIND_BIGINT,
            Item::Max => KIND_MAX,
        }
    }

    /// Whether this segment participates in numeric padding.
    ///
    /// Note that `min` and `max` count as numbers here, matching upstream's
    /// `kind & KIND_QUALIFIER == 0` test.
    pub fn is_number(&self) -> bool {
        self.kind() & KIND_QUALIFIER == 0
    }

    /// Compares this item against the implied padding item (`0` for numeric
    /// positions, `ga` for qualifier positions).
    ///
    /// Upstream spells this `compareTo(null)`.
    fn compare_to_pad(&self) -> Ordering {
        match self {
            Item::Min => Ordering::Less,
            Item::Max | Item::BigInt(_) | Item::Str(_) => Ordering::Greater,
            Item::Int(v) => v.cmp(&0),
            Item::Qualifier(v) => v.cmp(&0),
        }
    }
}

impl Ord for Item {
    fn cmp(&self, other: &Self) -> Ordering {
        let rel = self.kind().cmp(&other.kind());
        if rel != Ordering::Equal {
            return rel;
        }
        match (self, other) {
            (Item::Min, Item::Min) | (Item::Max, Item::Max) => Ordering::Equal,
            (Item::Int(a), Item::Int(b)) => a.cmp(b),
            (Item::Qualifier(a), Item::Qualifier(b)) => a.cmp(b),
            (Item::BigInt(a), Item::BigInt(b)) => cmp_digits(a, b),
            (Item::Str(a), Item::Str(b)) => cmp_ignore_case(a, b),
            // Unreachable: equal kinds imply equal variants.
            _ => Ordering::Equal,
        }
    }
}

impl PartialOrd for Item {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for Item {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for Item {}

impl Hash for Item {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.kind().hash(state);
        match self {
            Item::Min | Item::Max => {}
            Item::Int(v) => v.hash(state),
            Item::Qualifier(v) => v.hash(state),
            Item::BigInt(v) => v.hash(state),
            // Equality is case-insensitive, so the hash must fold case too.
            Item::Str(v) => {
                for c in v.chars().flat_map(char::to_lowercase) {
                    c.hash(state);
                }
            }
        }
    }
}

impl fmt::Display for Item {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Item::Min => f.write_str("min"),
            Item::Max => f.write_str("max"),
            Item::Qualifier(v) => write!(f, "{v}"),
            Item::Str(v) => f.write_str(v),
            Item::Int(v) => write!(f, "{v}"),
            Item::BigInt(v) => f.write_str(v),
        }
    }
}

/// Compares two normalized (leading-zero-free) decimal digit strings
/// numerically. Length decides first, then lexicographic order.
fn cmp_digits(a: &str, b: &str) -> Ordering {
    a.len().cmp(&b.len()).then_with(|| a.cmp(b))
}

/// Case-insensitive comparison matching `String.compareToIgnoreCase`.
///
/// The tokenizer lowercases every string segment, so this only differs from a
/// plain comparison for items built by hand.
fn cmp_ignore_case(a: &str, b: &str) -> Ordering {
    let mut ai = a.chars().flat_map(char::to_lowercase);
    let mut bi = b.chars().flat_map(char::to_lowercase);
    loop {
        match (ai.next(), bi.next()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) => {
                let rel = x.cmp(&y);
                if rel != Ordering::Equal {
                    return rel;
                }
            }
        }
    }
}

/// A Maven version, comparable under the `generic` scheme.
///
/// Parsing never fails: any string is a valid version. Two versions that differ
/// as strings may still compare equal, because trailing padding segments are
/// trimmed (`1`, `1.0` and `1.0.0-ga` are all the same version).
///
/// # Examples
///
/// ```
/// use jv_version::Version;
///
/// assert_eq!(Version::parse("1.0.0"), Version::parse("1"));
/// assert!(Version::parse("1.0-alpha-1") < Version::parse("1.0"));
/// assert!(Version::parse("1.0") < Version::parse("1.0-sp1"));
/// // The original spelling is preserved for display.
/// assert_eq!(Version::parse("1.0.0").as_str(), "1.0.0");
/// ```
#[derive(Clone, Debug)]
pub struct Version {
    raw: String,
    items: Vec<Item>,
}

impl Version {
    /// Tokenizes a version string. Any input is accepted.
    pub fn parse(version: &str) -> Self {
        let mut items = Vec::new();
        let mut tokenizer = Tokenizer::new(version);
        while let Some(item) = tokenizer.next_item() {
            items.push(item);
        }
        trim_padding(&mut items);
        Self {
            raw: version.to_owned(),
            items,
        }
    }

    /// The version string as written.
    pub fn as_str(&self) -> &str {
        &self.raw
    }

    /// The tokenized form, after padding has been trimmed.
    pub fn items(&self) -> &[Item] {
        &self.items
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.raw)
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        compare_items(&self.items, &other.items)
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for Version {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for Version {}

impl Hash for Version {
    fn hash<H: Hasher>(&self, state: &mut H) {
        // Hashing the trimmed items, not the raw string, is what keeps `1` and
        // `1.0` hashing alike as their equality demands.
        self.items.hash(state);
    }
}

impl From<&str> for Version {
    fn from(value: &str) -> Self {
        Version::parse(value)
    }
}

/// Drops trailing segments that are indistinguishable from padding, so that
/// `1.0.0` reduces to `1` and `1.0-ga` to `1`.
///
/// Runs of numeric and non-numeric items are trimmed independently, which is
/// why the scan tracks which kind it is currently inside.
fn trim_padding(items: &mut Vec<Item>) {
    if items.is_empty() {
        return;
    }
    let mut number: Option<bool> = None;
    let mut end = items.len() as isize - 1;
    let mut i = end;
    while i > 0 {
        let idx = i as usize;
        let is_number = items[idx].is_number();
        if Some(is_number) != number {
            end = i;
            number = Some(is_number);
        }
        let at_tail = idx == items.len() - 1;
        if end == i
            && (at_tail || items[idx - 1].is_number() == is_number)
            && items[idx].compare_to_pad() == Ordering::Equal
        {
            items.remove(idx);
            end -= 1;
        }
        i -= 1;
    }
}

/// Compares two tokenized versions, padding the shorter side as needed.
fn compare_items(these: &[Item], those: &[Item]) -> Ordering {
    // Tracks the kind of the last segment compared, which decides *which* side
    // gets padded when the kinds diverge further along.
    let mut number = true;
    let mut index = 0usize;
    loop {
        match (index >= these.len(), index >= those.len()) {
            (true, true) => return Ordering::Equal,
            (true, false) => return compare_padding(those, index, None).reverse(),
            (false, true) => return compare_padding(these, index, None),
            (false, false) => {}
        }

        let this_item = &these[index];
        let that_item = &those[index];

        if this_item.is_number() != that_item.is_number() {
            if index == 0 {
                return this_item.cmp(that_item);
            }
            return if number == this_item.is_number() {
                compare_padding(these, index, Some(number))
            } else {
                compare_padding(those, index, Some(number)).reverse()
            };
        }

        let rel = this_item.cmp(that_item);
        if rel != Ordering::Equal {
            return rel;
        }
        number = this_item.is_number();
        index += 1;
    }
}

/// Compares the tail of `items` from `index` against implied padding, skipping
/// segments whose kind does not match `number` when one is given.
fn compare_padding(items: &[Item], index: usize, number: Option<bool>) -> Ordering {
    let mut rel = Ordering::Equal;
    for item in &items[index..] {
        if let Some(n) = number {
            if n != item.is_number() {
                continue;
            }
        }
        rel = item.compare_to_pad();
        if rel != Ordering::Equal {
            break;
        }
    }
    rel
}

/// Splits a version string into segments.
///
/// Segments break on `.`, `-`, `_`, and on every transition between digits and
/// non-digits. Leading zeros inside a numeric segment are dropped so that
/// numeric comparison — and the `Int`/`BigInt` split — see normalized digits.
struct Tokenizer<'a> {
    version: &'a str,
    len: usize,
    index: usize,
}

impl<'a> Tokenizer<'a> {
    fn new(version: &'a str) -> Self {
        // An empty version behaves exactly like "0".
        let version = if version.is_empty() { "0" } else { version };
        Self {
            len: version.len(),
            version,
            index: 0,
        }
    }

    fn next_item(&mut self) -> Option<Item> {
        if self.index >= self.len {
            return None;
        }
        let bytes = self.version.as_bytes();

        // -2: fresh, -1: inside a string, 0: numeric so far all zeros,
        // 1: numeric with a non-zero digit seen.
        let mut state: i8 = -2;
        let mut start = self.index;
        let mut end = self.len;
        let mut terminated_by_number = false;

        while self.index < self.len {
            let c = bytes[self.index];
            if c == b'.' || c == b'-' || c == b'_' {
                end = self.index;
                self.index += 1;
                break;
            }
            if c.is_ascii_digit() {
                if state == -1 {
                    // digit right after letters: a segment boundary
                    end = self.index;
                    terminated_by_number = true;
                    break;
                }
                if state == 0 {
                    // strip a leading zero
                    start += 1;
                }
                state = if state > 0 || c > b'0' { 1 } else { 0 };
            } else {
                if state >= 0 {
                    // letter right after digits: a segment boundary
                    end = self.index;
                    break;
                }
                state = -1;
            }
            self.index += 1;
        }

        // Every break above lands on a UTF-8 char boundary: delimiters and
        // digits are ASCII, and a digit→letter break begins a fresh char.
        let (token, number) = if end > start {
            (&self.version[start..end], state >= 0)
        } else {
            // An empty segment (e.g. the gap in "1..2") reads as 0.
            ("0", true)
        };

        Some(self.to_item(token, number, terminated_by_number))
    }

    fn to_item(&self, token: &str, number: bool, terminated_by_number: bool) -> Item {
        if number {
            // Ten digits is where upstream switches representation; below it a
            // machine integer always suffices.
            if token.len() < 10 {
                Item::Int(token.parse::<u64>().expect("token is ASCII digits only"))
            } else {
                Item::BigInt(token.to_owned())
            }
        } else {
            // `min` and `max` are only special as the final segment.
            if self.index >= self.len {
                if token.eq_ignore_ascii_case("min") {
                    return Item::Min;
                }
                if token.eq_ignore_ascii_case("max") {
                    return Item::Max;
                }
            }
            // A lone letter immediately followed by a number is shorthand:
            // "1.0a1" means "1.0-alpha-1".
            let expanded = if terminated_by_number && token.chars().count() == 1 {
                match token.as_bytes()[0] {
                    b'a' | b'A' => LABEL_ALPHA,
                    b'b' | b'B' => LABEL_BETA,
                    b'm' | b'M' => LABEL_MILESTONE,
                    _ => token,
                }
            } else {
                token
            };
            let lowered = expanded.to_lowercase();
            match token_qualifier(&lowered) {
                Some(shift) => Item::Qualifier(shift),
                None => Item::Str(lowered),
            }
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
    fn empty_version_is_zero() {
        assert_eq!(v(""), v("0"));
        assert_eq!(v("").items().len(), 1);
    }

    #[test]
    fn trailing_padding_is_trimmed() {
        assert_eq!(v("1").items().len(), 1);
        assert_eq!(v("1.0").items().len(), 1);
        assert_eq!(v("1.0.0").items().len(), 1);
        assert_eq!(v("1-ga").items().len(), 1);
        // A non-zero qualifier is not padding.
        assert_eq!(v("1-alpha").items().len(), 2);
    }

    #[test]
    fn leading_zeros_are_stripped() {
        assert_eq!(v("1.007"), v("1.7"));
        assert_eq!(v("000"), v("0"));
    }

    #[test]
    fn delimiters_are_equivalent() {
        assert_eq!(v("1.2.3"), v("1-2-3"));
        assert_eq!(v("1.2.3"), v("1_2_3"));
    }

    #[test]
    fn digit_letter_transition_splits() {
        assert_eq!(v("1.0alpha1"), v("1.0-alpha-1"));
        assert_eq!(v("1.0a1"), v("1.0-alpha-1"));
        assert_eq!(v("1.0b2"), v("1.0-beta-2"));
        assert_eq!(v("1.0m3"), v("1.0-milestone-3"));
    }

    #[test]
    fn qualifier_ordering() {
        let chain = [
            "1.0-alpha-1",
            "1.0-beta-1",
            "1.0-milestone-1",
            "1.0-rc-1",
            "1.0-SNAPSHOT",
            "1.0",
            "1.0-sp-1",
            "1.0.1",
        ];
        for w in chain.windows(2) {
            assert!(v(w[0]) < v(w[1]), "{} should sort before {}", w[0], w[1]);
        }
    }

    #[test]
    fn cr_and_rc_are_the_same() {
        assert_eq!(v("1.0-cr1"), v("1.0-rc1"));
    }

    #[test]
    fn padding_across_kind_mismatch() {
        // The scheme's headline example.
        assert_eq!(v("1-alpha"), v("1.0.0-alpha"));
        assert!(v("1-alpha") < v("1.0.1-ga"));
        assert_eq!(v("1.0.1-ga"), v("1.0.1"));
    }

    #[test]
    fn min_and_max_bound_a_prefix() {
        assert!(v("1.2.min") < v("1.2"));
        assert!(v("1.2") < v("1.2.max"));
        assert!(v("1.2.min") < v("1.2.9999"));
        assert!(v("1.2.9999") < v("1.2.max"));
        // Only recognized in final position.
        assert_ne!(v("1.min.2"), v("1.2"));
    }

    #[test]
    fn big_numeric_segments() {
        // Ten digits or more take the BigInt path; ordering must still hold.
        assert!(v("1.9999999999") > v("1.999999999"));
        assert!(v("1.10000000000") > v("1.9999999999"));
        assert_eq!(v("1.0000000001"), v("1.1"));
    }

    #[test]
    fn case_insensitive_strings() {
        assert_eq!(v("1.0-Foo"), v("1.0-foo"));
        assert_eq!(v("1.0-SNAPSHOT"), v("1.0-snapshot"));
    }

    #[test]
    fn equal_versions_hash_alike() {
        use std::collections::HashSet;
        let mut set = HashSet::new();
        set.insert(v("1"));
        assert!(set.contains(&v("1.0")));
        assert!(set.contains(&v("1.0.0")));
        assert!(set.contains(&v("1-ga")));
        assert!(!set.contains(&v("1.0.1")));
    }

    #[test]
    fn display_preserves_input() {
        assert_eq!(v("1.0.0-SNAPSHOT").to_string(), "1.0.0-SNAPSHOT");
    }

    #[test]
    fn non_ascii_does_not_panic() {
        // Discouraged, but must not split a multi-byte char or panic.
        let _ = v("1.0-日本語");
        assert_eq!(v("1.0-日本語"), v("1.0-日本語"));
        assert!(v("1.0") < v("1.0-日本語"));
    }
}
