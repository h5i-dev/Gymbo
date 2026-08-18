//! When a cached file is stale, and what a bad checksum costs.
//!
//! Ported from `DefaultUpdatePolicyAnalyzer` and `RepositoryPolicy`. These two
//! settings are what make a resolver fast without making it wrong: releases are
//! immutable so a cached one is never re-fetched, while snapshots and metadata
//! carry a freshness window.

use std::time::{Duration, SystemTime};

/// How often a mutable file is re-fetched.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum UpdatePolicy {
    /// Every invocation.
    Always,
    /// Once per day, the Maven default.
    #[default]
    Daily,
    /// Every N minutes.
    Interval(u64),
    /// Never, once cached.
    Never,
}

impl UpdatePolicy {
    /// Reads the `<updatePolicy>` spelling, falling back to the default for
    /// anything unrecognized rather than failing a resolve over a typo.
    pub fn parse(text: &str) -> Self {
        let text = text.trim();
        if let Some(minutes) = text.strip_prefix("interval:") {
            return minutes
                .trim()
                .parse::<u64>()
                .map_or(UpdatePolicy::Daily, UpdatePolicy::Interval);
        }
        match text {
            "always" => UpdatePolicy::Always,
            "never" => UpdatePolicy::Never,
            _ => UpdatePolicy::Daily,
        }
    }

    /// Whether a file last checked at `last_checked` should be re-fetched.
    ///
    /// # Examples
    ///
    /// Both instants are given, and the example pins them rather than reading
    /// the clock: `daily` compares UTC *days*, so "an hour ago" is on the
    /// previous day for one hour out of every twenty-four, and an example built
    /// on `SystemTime::now()` fails during exactly that hour.
    ///
    /// ```
    /// use std::time::{Duration, SystemTime, UNIX_EPOCH};
    /// use jv_repo::UpdatePolicy;
    ///
    /// // Midday, and an hour before it, on the same UTC day.
    /// let noon = UNIX_EPOCH + Duration::from_secs(1_705_320_000);
    /// let an_hour_before = noon - Duration::from_secs(3600);
    ///
    /// assert!(!UpdatePolicy::Daily.is_stale(an_hour_before, noon));
    /// assert!(UpdatePolicy::Always.is_stale(an_hour_before, noon));
    /// assert!(!UpdatePolicy::Never.is_stale(an_hour_before, noon));
    ///
    /// let a_week_before = noon - Duration::from_secs(7 * 86_400);
    /// assert!(UpdatePolicy::Daily.is_stale(a_week_before, noon));
    ///
    /// // And the case the clock-reading version tripped over: an hour ago can
    /// // be yesterday, which `daily` treats as stale however recent it is.
    /// let just_after_midnight = UNIX_EPOCH + Duration::from_secs(1_705_276_800 + 600);
    /// let before_midnight = just_after_midnight - Duration::from_secs(3600);
    /// assert!(UpdatePolicy::Daily.is_stale(before_midnight, just_after_midnight));
    /// ```
    pub fn is_stale(&self, last_checked: SystemTime, now: SystemTime) -> bool {
        match self {
            UpdatePolicy::Always => true,
            UpdatePolicy::Never => false,
            UpdatePolicy::Daily => {
                // Maven compares against the start of today rather than a rolling
                // 24 hours, so a build at 09:00 refreshes what a build at 23:00
                // yesterday fetched.
                elapsed(last_checked, now) >= Duration::from_secs(86_400)
                    || !same_day(last_checked, now)
            }
            UpdatePolicy::Interval(minutes) => {
                elapsed(last_checked, now) >= Duration::from_secs(minutes * 60)
            }
        }
    }
}

fn elapsed(from: SystemTime, to: SystemTime) -> Duration {
    to.duration_since(from).unwrap_or(Duration::ZERO)
}

/// Whether two instants fall on the same UTC day.
///
/// Computed from the epoch rather than with a calendar library: the only
/// question is which 86400-second block each falls in.
fn same_day(left: SystemTime, right: SystemTime) -> bool {
    let day = |time: SystemTime| {
        time.duration_since(SystemTime::UNIX_EPOCH)
            .map(|since| since.as_secs() / 86_400)
            .unwrap_or(0)
    };
    day(left) == day(right)
}

/// What to do when a downloaded file does not match its published checksum.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ChecksumPolicy {
    /// Reject the file.
    Fail,
    /// Accept it and say so. Maven 3's default, and jv's: being stricter by
    /// default would fail builds that Maven completes, which is the one thing a
    /// drop-in replacement cannot do.
    #[default]
    Warn,
    /// Accept it silently.
    Ignore,
}

impl ChecksumPolicy {
    pub fn parse(text: &str) -> Self {
        match text.trim() {
            "fail" => ChecksumPolicy::Fail,
            "ignore" => ChecksumPolicy::Ignore,
            _ => ChecksumPolicy::Warn,
        }
    }

    /// Whether a mismatch should stop the resolve.
    pub fn is_fatal(&self) -> bool {
        matches!(self, ChecksumPolicy::Fail)
    }
}

/// A repository's handling of one class of version.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Policy {
    pub enabled: bool,
    pub update: UpdatePolicy,
    pub checksum: ChecksumPolicy,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            enabled: true,
            update: UpdatePolicy::default(),
            checksum: ChecksumPolicy::default(),
        }
    }
}

impl Policy {
    /// Combines two declarations of the same repository, taking the more
    /// permissive availability and the more eager refresh.
    ///
    /// Two POMs in one build can declare the same repository id with different
    /// policies; Maven does not make the build pick one, and neither does this.
    pub fn merge(&self, other: &Policy) -> Policy {
        Policy {
            enabled: self.enabled || other.enabled,
            update: if self.update.eagerness() >= other.update.eagerness() {
                self.update
            } else {
                other.update
            },
            // The stricter checksum policy wins: a repository someone asked to
            // verify strictly should not be relaxed by another declaration.
            checksum: if self.checksum.strictness() >= other.checksum.strictness() {
                self.checksum
            } else {
                other.checksum
            },
        }
    }
}

impl UpdatePolicy {
    /// How eagerly this policy refreshes, for comparison.
    fn eagerness(self) -> u64 {
        match self {
            UpdatePolicy::Always => u64::MAX,
            UpdatePolicy::Interval(minutes) => u64::MAX - minutes.max(1),
            UpdatePolicy::Daily => 1,
            UpdatePolicy::Never => 0,
        }
    }
}

impl ChecksumPolicy {
    fn strictness(self) -> u8 {
        match self {
            ChecksumPolicy::Fail => 2,
            ChecksumPolicy::Warn => 1,
            ChecksumPolicy::Ignore => 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ago(seconds: u64) -> SystemTime {
        SystemTime::now() - Duration::from_secs(seconds)
    }

    #[test]
    fn update_policies_parse() {
        assert_eq!(UpdatePolicy::parse("always"), UpdatePolicy::Always);
        assert_eq!(UpdatePolicy::parse("never"), UpdatePolicy::Never);
        assert_eq!(UpdatePolicy::parse("daily"), UpdatePolicy::Daily);
        assert_eq!(
            UpdatePolicy::parse("interval:30"),
            UpdatePolicy::Interval(30)
        );
        assert_eq!(
            UpdatePolicy::parse(" interval:5 "),
            UpdatePolicy::Interval(5)
        );
        // Unrecognized spellings fall back rather than failing a resolve.
        assert_eq!(UpdatePolicy::parse("hourly"), UpdatePolicy::Daily);
        assert_eq!(UpdatePolicy::parse("interval:soon"), UpdatePolicy::Daily);
    }

    #[test]
    fn always_and_never_are_absolute() {
        let now = SystemTime::now();
        assert!(UpdatePolicy::Always.is_stale(now, now));
        assert!(!UpdatePolicy::Never.is_stale(ago(365 * 86_400), now));
    }

    #[test]
    fn intervals_count_minutes() {
        let now = SystemTime::now();
        let policy = UpdatePolicy::Interval(30);
        assert!(!policy.is_stale(ago(29 * 60), now));
        assert!(policy.is_stale(ago(31 * 60), now));
    }

    #[test]
    fn daily_refreshes_across_a_day_boundary() {
        let now = SystemTime::now();
        // Well within a day.
        assert!(!UpdatePolicy::Daily.is_stale(now, now));
        // Long past one.
        assert!(UpdatePolicy::Daily.is_stale(ago(2 * 86_400), now));
        // And a check from a previous UTC day counts as stale even if it was
        // less than 24 hours ago, which is what Maven does.
        let start_of_today = SystemTime::UNIX_EPOCH
            + Duration::from_secs(
                now.duration_since(SystemTime::UNIX_EPOCH)
                    .unwrap()
                    .as_secs()
                    / 86_400
                    * 86_400,
            );
        let just_before_midnight = start_of_today - Duration::from_secs(1);
        assert!(UpdatePolicy::Daily.is_stale(just_before_midnight, now));
    }

    #[test]
    fn checksum_policies_parse_and_rank() {
        assert_eq!(ChecksumPolicy::parse("fail"), ChecksumPolicy::Fail);
        assert_eq!(ChecksumPolicy::parse("ignore"), ChecksumPolicy::Ignore);
        assert_eq!(ChecksumPolicy::parse("warn"), ChecksumPolicy::Warn);
        assert_eq!(ChecksumPolicy::parse("nonsense"), ChecksumPolicy::Warn);
        assert!(ChecksumPolicy::Fail.is_fatal());
        assert!(!ChecksumPolicy::Warn.is_fatal());
    }

    #[test]
    fn merging_takes_the_permissive_availability_and_strict_checksum() {
        let lenient = Policy {
            enabled: false,
            update: UpdatePolicy::Never,
            checksum: ChecksumPolicy::Ignore,
        };
        let strict = Policy {
            enabled: true,
            update: UpdatePolicy::Always,
            checksum: ChecksumPolicy::Fail,
        };
        let merged = lenient.merge(&strict);
        assert!(merged.enabled);
        assert_eq!(merged.update, UpdatePolicy::Always);
        assert_eq!(merged.checksum, ChecksumPolicy::Fail);
        // Merging is symmetric.
        assert_eq!(strict.merge(&lenient), merged);
    }

    #[test]
    fn a_shorter_interval_is_more_eager_than_a_longer_one() {
        let quick = Policy {
            update: UpdatePolicy::Interval(5),
            ..Policy::default()
        };
        let slow = Policy {
            update: UpdatePolicy::Interval(60),
            ..Policy::default()
        };
        assert_eq!(quick.merge(&slow).update, UpdatePolicy::Interval(5));
        // And any interval beats daily.
        let daily = Policy::default();
        assert_eq!(slow.merge(&daily).update, UpdatePolicy::Interval(60));
    }
}
