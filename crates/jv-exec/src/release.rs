//! Which version an endpoint with no version means.
//!
//! # Why not `<release>`
//!
//! `maven-metadata.xml` offers two answers already computed: `<latest>`, which
//! is whatever was deployed most recently and includes snapshots, and
//! `<release>`, which is the most recent non-snapshot. `<release>` is the right
//! *idea* — it is what Maven's `RELEASE` keyword reads — but it is a single
//! string one repository wrote at deploy time. It is frequently absent from
//! mirrors and from metadata aggregated by a proxy, and it says nothing about
//! versions held in a second configured repository. The `<versions>` list does
//! not have those problems: `DescriptorSource::versions` already merges it
//! across every repository in play. So jv computes the answer from the list
//! rather than trusting a field, which is also the only way an endpoint can be
//! resolved against a repository set the publisher never saw.
//!
//! # The ladder
//!
//! 1. the greatest version that is neither a snapshot nor a prerelease;
//! 2. failing that, the greatest non-snapshot — Maven's own `RELEASE` rule;
//! 3. failing that, the greatest version there is.
//!
//! Rung 1 is stricter than Maven, deliberately. Maven resolves `RELEASE` for a
//! *build*, where an author who wrote `RELEASE` accepted whatever comes; `jvx`
//! resolves it for a tool someone is about to run, and launching an alpha
//! because it happens to sort highest is a worse surprise than launching the
//! stable build published a month earlier. Rungs 2 and 3 keep a project that has
//! only ever published previews, or only snapshots, runnable at all.
//!
//! Ordering is [`Version`]'s, so `1.10` beats `1.9` and the file's own order —
//! which is deploy order, not version order — is never trusted.

use jv_model::is_snapshot_version;
use jv_version::{Version, qualifier};

/// The version an endpoint that named none should resolve to.
pub fn select(versions: &[String]) -> Option<&str> {
    greatest(versions, |version| {
        !is_snapshot_version(version) && !is_prerelease(version)
    })
    .or_else(|| greatest(versions, |version| !is_snapshot_version(version)))
    .or_else(|| greatest(versions, |_| true))
}

/// Whether a version announces itself as an alpha, beta, milestone, release
/// candidate or snapshot.
///
/// This is Maven's own fuzzy qualifier detection, and it is a heuristic: a
/// version like `3.3.0-arm64` reads as a milestone because `m6` appears inside
/// it. That is survivable here only because [`select`] falls through — a project
/// whose every version trips the heuristic still resolves, one rung down.
fn is_prerelease(version: &str) -> bool {
    qualifier(version).is_some_and(|shift| shift < 0)
}

fn greatest(versions: &[String], keep: impl Fn(&str) -> bool) -> Option<&str> {
    versions
        .iter()
        .map(String::as_str)
        .filter(|version| keep(version))
        .max_by(|left, right| Version::parse(left).cmp(&Version::parse(right)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn versions(list: &[&str]) -> Vec<String> {
        list.iter().map(|version| (*version).to_owned()).collect()
    }

    #[test]
    fn the_greatest_release_is_chosen() {
        assert_eq!(select(&versions(&["1.0", "1.1", "1.2"])), Some("1.2"));
    }

    #[test]
    fn versions_are_ordered_numerically_rather_than_lexically() {
        // The trap the whole `jv-version` crate exists for: `1.9` sorts after
        // `1.10` as text, and a tool that gets this wrong silently runs an old
        // build for months.
        assert_eq!(select(&versions(&["1.9", "1.10", "1.2"])), Some("1.10"));
    }

    #[test]
    fn deploy_order_is_not_trusted() {
        // Repositories append on deploy, so a backported patch release leaves
        // the list out of order. Taking the last entry would pick it.
        assert_eq!(select(&versions(&["2.0", "1.9.1"])), Some("2.0"));
    }

    #[test]
    fn a_snapshot_never_wins_over_a_release() {
        assert_eq!(select(&versions(&["1.0", "2.0-SNAPSHOT"])), Some("1.0"));
    }

    #[test]
    fn a_timestamped_snapshot_never_wins_either() {
        // The form a snapshot takes once a repository has expanded it, which no
        // longer contains the word SNAPSHOT.
        assert_eq!(
            select(&versions(&["1.0", "2.0-20240115.103000-7"])),
            Some("1.0")
        );
    }

    #[test]
    fn a_prerelease_never_wins_over_a_release() {
        for prerelease in [
            "2.0-alpha1",
            "2.0-beta2",
            "2.0-M1",
            "2.0-rc3",
            "2.0-SNAPSHOT",
        ] {
            assert_eq!(
                select(&versions(&["1.0", prerelease])),
                Some("1.0"),
                "{prerelease} should lose to 1.0"
            );
        }
    }

    #[test]
    fn a_service_pack_is_a_release() {
        // `sp` sorts *after* the plain release in Maven's qualifier table, so it
        // is a release that happens to be qualified, not a preview.
        assert_eq!(select(&versions(&["1.0", "1.0-sp1"])), Some("1.0-sp1"));
    }

    #[test]
    fn a_final_qualifier_is_a_release() {
        // Netty and the Spring ecosystem spell their releases this way.
        assert_eq!(
            select(&versions(&["4.1.99.Final", "4.1.100.Final"])),
            Some("4.1.100.Final")
        );
    }

    #[test]
    fn a_project_with_only_prereleases_still_resolves() {
        // Rung 2: better to run the newest thing published than to refuse.
        assert_eq!(
            select(&versions(&["1.0-alpha1", "1.0-alpha2"])),
            Some("1.0-alpha2")
        );
    }

    #[test]
    fn a_project_with_only_snapshots_still_resolves() {
        // Rung 3, which is the only rung left for a tool published solely to a
        // snapshot repository.
        assert_eq!(
            select(&versions(&["1.0-SNAPSHOT", "1.1-SNAPSHOT"])),
            Some("1.1-SNAPSHOT")
        );
    }

    #[test]
    fn an_empty_list_selects_nothing() {
        assert_eq!(select(&[]), None);
    }

    #[test]
    fn the_jre_and_android_flavours_of_guava_are_both_releases() {
        // Guava's qualifiers are not previews, and reading them as such would
        // pin jvx to some ancient unqualified version.
        assert_eq!(
            select(&versions(&["33.4.0-jre", "33.4.8-jre"])),
            Some("33.4.8-jre")
        );
        assert_eq!(
            select(&versions(&["33.4.8-android"])),
            Some("33.4.8-android")
        );
    }
}
