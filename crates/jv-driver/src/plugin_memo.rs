//! Plugin dependency closures, remembered between runs.
//!
//! Resolving a plugin's closure is a full dependency collection, and on a warm
//! cache it is where nearly all of `jv sync` goes: 442ms of a 466ms sync on
//! commons-io, against 18ms for the project's own dependency tree. Resolving
//! them in parallel took that to about 190ms of 213ms; it is still the cost.
//!
//! A closure is a pure function of the plugin's coordinates, the
//! `<plugin><dependencies>` block that can change what those coordinates
//! resolve to, and the repositories in scope. So it can be written down.
//!
//! # Why this is allowed to be a cache
//!
//! The thing that makes a remembered resolve dangerous is that the answer can
//! change without the question changing: a new release satisfies a range, a
//! snapshot moves, a repository appears. Maven has the same problem and answers
//! it with an update policy — by default it re-checks metadata once a day and
//! otherwise trusts what it has. This cache expires on exactly that schedule,
//! reusing [`UpdatePolicy`] rather than inventing a second notion of stale, so a
//! memo can never be staler than what Maven would have used anyway. `-U` forces
//! a re-check and bypasses it, as it bypasses Maven's.
//!
//! Offline, nothing upstream can change, so a memo never expires.
//!
//! # What is deliberately not remembered
//!
//! An entry is written only for a resolve that went cleanly: no warnings, no
//! unreadable descriptors, and no snapshot anywhere in the closure. A degraded
//! answer cached is a degraded answer repeated, and it would be repeated
//! silently — the second run has no warning to print, because it did no work.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use jv_model::{Artifact, is_snapshot_version};
use jv_repo::UpdatePolicy;
use serde::{Deserialize, Serialize};

/// Bumped whenever the stored shape changes, so an older file is a miss rather
/// than something misread into the wrong fields.
const SCHEMA: u32 = 1;

/// A closure as stored: coordinates only, since the bytes live in the cache.
#[derive(Debug, Serialize, Deserialize)]
struct StoredArtifact {
    group_id: String,
    artifact_id: String,
    version: String,
    classifier: String,
    extension: String,
}

impl From<&Artifact> for StoredArtifact {
    fn from(artifact: &Artifact) -> Self {
        Self {
            group_id: artifact.group_id.clone(),
            artifact_id: artifact.artifact_id.clone(),
            version: artifact.version.clone(),
            classifier: artifact.classifier.clone(),
            extension: artifact.extension.clone(),
        }
    }
}

impl From<&StoredArtifact> for Artifact {
    fn from(stored: &StoredArtifact) -> Self {
        Self {
            group_id: stored.group_id.clone(),
            artifact_id: stored.artifact_id.clone(),
            version: stored.version.clone(),
            classifier: stored.classifier.clone(),
            extension: stored.extension.clone(),
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct Entry {
    schema: u32,
    /// Seconds since the epoch. Stored rather than taken from the file's mtime
    /// because a cache directory restored by CI arrives with whatever mtimes the
    /// unpacking gave it.
    resolved_at: u64,
    dependencies: Vec<StoredArtifact>,
    /// What the plugin selects at run time, each with its own closure.
    extras: Vec<(StoredArtifact, Vec<StoredArtifact>)>,
}

/// A resolved closure, in the shape the caller works in.
#[derive(Debug, Default)]
pub struct Remembered {
    pub dependencies: Vec<Artifact>,
    pub extras: Vec<(Artifact, Vec<Artifact>)>,
}

/// Closures kept alongside jv's cache.
#[derive(Debug)]
pub struct PluginMemo {
    root: PathBuf,
    /// Fingerprint of the repositories in scope, mixed into every key: the same
    /// plugin resolved against a different repository set is a different
    /// question.
    repositories: u64,
    policy: UpdatePolicy,
    /// Offline runs never expire an entry: nothing upstream can move while jv
    /// refuses to look at it.
    ///
    /// They do still write. An offline resolve that completed cleanly read every
    /// POM it needed out of the cache and reached the same answer an online one
    /// would have — an incomplete one is caught by `clean`, because a POM that
    /// is not cached is an unreadable descriptor. The one thing an offline
    /// resolve can get differently is a version range read against stale
    /// metadata, and an online run inside the same freshness window would have
    /// used that same stale metadata, which is what the window means.
    offline: bool,
}

impl PluginMemo {
    pub fn new(
        cache_root: &Path,
        repository_urls: &[String],
        forced_update: Option<UpdatePolicy>,
        offline: bool,
    ) -> Self {
        let mut hasher = DefaultHasher::new();
        let mut sorted: Vec<&String> = repository_urls.iter().collect();
        sorted.sort();
        sorted.dedup();
        for url in sorted {
            url.hash(&mut hasher);
        }
        Self {
            root: cache_root.join("plugin-closures"),
            repositories: hasher.finish(),
            // `-U` means "do not trust what you have", which has to include
            // this. Without the bypass, forcing an update would re-check every
            // repository and then use a remembered answer anyway.
            policy: forced_update.unwrap_or_default(),
            offline,
        }
    }

    /// The file an entry lives in.
    ///
    /// Named by a hash of everything the answer depends on, in a two-level
    /// directory so a machine that has resolved thousands of plugins does not
    /// put them all in one directory.
    fn path(&self, key: &str) -> PathBuf {
        let mut hasher = DefaultHasher::new();
        SCHEMA.hash(&mut hasher);
        self.repositories.hash(&mut hasher);
        key.hash(&mut hasher);
        let digest = format!("{:016x}", hasher.finish());
        self.root.join(&digest[..2]).join(format!("{digest}.json"))
    }

    /// A remembered closure, if one is present and still current.
    pub fn get(&self, key: &str) -> Option<Remembered> {
        let bytes = std::fs::read(self.path(key)).ok()?;
        let entry: Entry = serde_json::from_slice(&bytes).ok()?;
        if entry.schema != SCHEMA {
            return None;
        }
        if !self.offline {
            let resolved_at =
                SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(entry.resolved_at);
            if self.policy.is_stale(resolved_at, SystemTime::now()) {
                return None;
            }
        }
        Some(Remembered {
            dependencies: entry.dependencies.iter().map(Artifact::from).collect(),
            extras: entry
                .extras
                .iter()
                .map(|(extra, dependencies)| {
                    (
                        Artifact::from(extra),
                        dependencies.iter().map(Artifact::from).collect(),
                    )
                })
                .collect(),
        })
    }

    /// Remembers a closure, if it is one worth remembering.
    ///
    /// Failure to write is not an error: the memo is an optimisation, and a
    /// read-only or full cache directory should slow a sync down rather than
    /// fail it.
    pub fn put(&self, key: &str, closure: &Remembered, clean: bool) {
        if !clean || !worth_keeping(closure) {
            return;
        }
        let entry = Entry {
            schema: SCHEMA,
            resolved_at: SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .map(|since| since.as_secs())
                .unwrap_or(0),
            dependencies: closure.dependencies.iter().map(StoredArtifact::from).collect(),
            extras: closure
                .extras
                .iter()
                .map(|(extra, dependencies)| {
                    (
                        StoredArtifact::from(extra),
                        dependencies.iter().map(StoredArtifact::from).collect(),
                    )
                })
                .collect(),
        };
        let Ok(bytes) = serde_json::to_vec(&entry) else {
            return;
        };
        let path = self.path(key);
        if let Some(parent) = path.parent() {
            if std::fs::create_dir_all(parent).is_err() {
                return;
            }
        }
        // Written to a neighbour and renamed, so a reader never sees half a
        // file. Two syncs racing write the same content, so whichever rename
        // lands last is still correct.
        let temporary = path.with_extension(format!("tmp{}", std::process::id()));
        if std::fs::write(&temporary, &bytes).is_ok() && std::fs::rename(&temporary, &path).is_err()
        {
            let _ = std::fs::remove_file(&temporary);
        }
    }
}

/// Whether a closure is stable enough to write down.
///
/// A snapshot's bytes change under a version that does not, so a closure
/// containing one is only true until the next deployment. The update policy
/// would expire it eventually; not storing it at all is simpler and costs
/// nothing, because a build that depends on a snapshot is asking for the newest
/// one every time.
fn worth_keeping(closure: &Remembered) -> bool {
    let mut artifacts = closure.dependencies.iter().chain(
        closure
            .extras
            .iter()
            .flat_map(|(extra, dependencies)| std::iter::once(extra).chain(dependencies)),
    );
    !artifacts.any(|artifact| is_snapshot_version(&artifact.version))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn artifact(version: &str) -> Artifact {
        Artifact::new("g", "a", version)
    }

    fn memo(root: &Path, offline: bool) -> PluginMemo {
        PluginMemo::new(root, &["https://repo".to_owned()], None, offline)
    }

    #[test]
    fn a_closure_survives_a_round_trip() {
        let directory = tempfile::tempdir().expect("a temp dir");
        let memo = memo(directory.path(), false);
        let closure = Remembered {
            dependencies: vec![artifact("1.0")],
            extras: vec![(artifact("2.0"), vec![artifact("3.0")])],
        };
        memo.put("key", &closure, true);

        let found = memo.get("key").expect("a remembered closure");
        assert_eq!(found.dependencies, closure.dependencies);
        assert_eq!(found.extras, closure.extras);
    }

    #[test]
    fn a_different_repository_set_is_a_different_question() {
        // The same plugin resolved against another repository list can resolve
        // differently, so it must not read the first answer.
        let directory = tempfile::tempdir().expect("a temp dir");
        let first = PluginMemo::new(directory.path(), &["https://one".to_owned()], None, false);
        first.put(
            "key",
            &Remembered {
                dependencies: vec![artifact("1.0")],
                ..Remembered::default()
            },
            true,
        );

        let second = PluginMemo::new(directory.path(), &["https://two".to_owned()], None, false);
        assert!(second.get("key").is_none());
    }

    #[test]
    fn repository_order_is_not_part_of_the_question() {
        // Repositories accumulate as POMs are read, so the same set arrives in
        // different orders between runs. Treating that as a different question
        // would miss every time.
        let directory = tempfile::tempdir().expect("a temp dir");
        let urls = ["https://one".to_owned(), "https://two".to_owned()];
        let forward = PluginMemo::new(directory.path(), &urls, None, false);
        forward.put(
            "key",
            &Remembered {
                dependencies: vec![artifact("1.0")],
                ..Remembered::default()
            },
            true,
        );

        let reversed: Vec<String> = urls.iter().rev().cloned().collect();
        let backward = PluginMemo::new(directory.path(), &reversed, None, false);
        assert!(backward.get("key").is_some());
    }

    #[test]
    fn a_degraded_resolve_is_not_remembered() {
        // The reason this matters: the second run has no warning to print,
        // because it does no work. A cached degraded answer is a silent one.
        let directory = tempfile::tempdir().expect("a temp dir");
        let memo = memo(directory.path(), false);
        memo.put(
            "key",
            &Remembered {
                dependencies: vec![artifact("1.0")],
                ..Remembered::default()
            },
            false,
        );
        assert!(memo.get("key").is_none());
    }

    #[test]
    fn a_snapshot_closure_is_not_remembered() {
        let directory = tempfile::tempdir().expect("a temp dir");
        let memo = memo(directory.path(), false);
        memo.put(
            "key",
            &Remembered {
                dependencies: vec![artifact("1.0-SNAPSHOT")],
                ..Remembered::default()
            },
            true,
        );
        assert!(memo.get("key").is_none());
    }

    #[test]
    fn a_snapshot_among_a_plugins_runtime_picks_is_caught_too() {
        // `worth_keeping` has to look through the extras, not just the top
        // level: Surefire's providers are resolved the same way and can be
        // snapshots in a project that builds them.
        let directory = tempfile::tempdir().expect("a temp dir");
        let memo = memo(directory.path(), false);
        memo.put(
            "key",
            &Remembered {
                dependencies: vec![artifact("1.0")],
                extras: vec![(artifact("2.0"), vec![artifact("9.9-SNAPSHOT")])],
            },
            true,
        );
        assert!(memo.get("key").is_none());
    }

    #[test]
    fn an_offline_run_both_reads_and_writes() {
        // A clean offline resolve read every POM it needed out of the cache, so
        // it reached the answer an online run would have. An incomplete one
        // never gets here: an uncached POM is an unreadable descriptor, which
        // clears `clean`.
        let directory = tempfile::tempdir().expect("a temp dir");
        let closure = Remembered {
            dependencies: vec![artifact("1.0")],
            ..Remembered::default()
        };
        memo(directory.path(), false).put("key", &closure, true);

        let offline = memo(directory.path(), true);
        assert!(offline.get("key").is_some());

        offline.put("other", &closure, true);
        assert!(offline.get("other").is_some());
    }

    #[test]
    fn a_forced_update_ignores_what_was_remembered() {
        // `-U` means "do not trust what you have". Re-checking every repository
        // and then reusing a remembered resolve would defeat the flag.
        let directory = tempfile::tempdir().expect("a temp dir");
        memo(directory.path(), false).put(
            "key",
            &Remembered {
                dependencies: vec![artifact("1.0")],
                ..Remembered::default()
            },
            true,
        );

        let forced = PluginMemo::new(
            directory.path(),
            &["https://repo".to_owned()],
            Some(UpdatePolicy::Always),
            false,
        );
        assert!(forced.get("key").is_none());
    }

    #[test]
    fn a_never_policy_keeps_an_entry_indefinitely() {
        let directory = tempfile::tempdir().expect("a temp dir");
        let never = PluginMemo::new(
            directory.path(),
            &["https://repo".to_owned()],
            Some(UpdatePolicy::Never),
            false,
        );
        never.put(
            "key",
            &Remembered {
                dependencies: vec![artifact("1.0")],
                ..Remembered::default()
            },
            true,
        );
        assert!(never.get("key").is_some());
    }

    #[test]
    fn an_entry_from_another_schema_is_a_miss() {
        let directory = tempfile::tempdir().expect("a temp dir");
        let memo = memo(directory.path(), false);
        let path = memo.path("key");
        std::fs::create_dir_all(path.parent().expect("a parent")).expect("the directory");
        std::fs::write(
            &path,
            br#"{"schema":999,"resolved_at":0,"dependencies":[],"extras":[]}"#,
        )
        .expect("the file");
        assert!(memo.get("key").is_none());
    }

    #[test]
    fn a_corrupt_entry_is_a_miss_rather_than_a_failure() {
        let directory = tempfile::tempdir().expect("a temp dir");
        let memo = memo(directory.path(), false);
        let path = memo.path("key");
        std::fs::create_dir_all(path.parent().expect("a parent")).expect("the directory");
        std::fs::write(&path, b"not json at all").expect("the file");
        assert!(memo.get("key").is_none());
    }
}
