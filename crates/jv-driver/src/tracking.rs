//! `_remote.repositories` — telling Maven where a file came from.
//!
//! Maven 3.9 resolves through `EnhancedLocalRepositoryManager`, which keeps a
//! `_remote.repositories` file in each version directory recording, per file,
//! which repository it was downloaded from. The format is a
//! `java.util.Properties` file whose keys are `<file name>><repository id>` and
//! whose values are always empty:
//!
//! ```text
//! #NOTE: This is a Maven Resolver internal implementation file, its format can be changed without prior notice.
//! jackson-core-2.17.1.jar>central=
//! jackson-core-2.17.1.pom>central=
//! ```
//!
//! # Why jv writes one at all
//!
//! It does not have to. `EnhancedLocalRepositoryManager.checkFind` has an
//! explicit escape hatch: a file present but mentioned nowhere in the tracking
//! file is treated as locally installed and accepted. So a repository jv
//! populated and left untracked resolves offline just fine.
//!
//! The reason to write one anyway is the hazard on the other side of that
//! branch. `isTracked` matches on the file name *prefix*, so a file that is
//! mentioned but not under a repository the current build has configured is
//! **rejected** — the escape hatch does not fire. Maven writes a tracking file
//! whenever it downloads anything into that directory, so a later online build
//! can turn jv's untracked files into a directory where some names are tracked
//! and some are not. Writing complete entries up front removes that whole class
//! of problem.
//!
//! # Why the empty repository id
//!
//! Every entry jv writes includes the `<file name>>=` form — the id Maven uses
//! for `mvn install`, meaning "this was installed locally". `checkFind` accepts
//! that unconditionally, *before* it looks at the request's repositories. The
//! real repository id is written too, because it is true and is what Maven
//! itself would record, but it cannot be relied on alone: when the user has a
//! mirror, Maven's effective repository id is the mirror's, not `central`, and
//! an entry naming the wrong id is worse than no entry at all.
//!
//! Ported from `EnhancedLocalRepositoryManager` and `DefaultTrackingFileManager`
//! (maven-resolver 1.9.22, which is what Maven 3.9.9 ships).

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::error::DriverError;

/// The file name Maven looks for. Configurable upstream via
/// `aether.enhancedLocalRepository.trackingFilename`, but nothing changes it.
pub const TRACKING_FILE: &str = "_remote.repositories";

/// The comment Maven's own writer puts at the top.
///
/// Reproduced so that a `_remote.repositories` jv wrote and one Maven wrote look
/// the same to anyone reading them. Maven also writes a timestamp line; jv does
/// not, because it would make every sync rewrite every file for no reason.
const HEADER: &str = "#NOTE: This is a Maven Resolver internal implementation file, its format can be changed without prior notice.";

/// One directory's tracking file.
#[derive(Debug, Default)]
pub struct Tracking {
    /// Keys, already in `name>id` form, sorted so the file is byte-stable
    /// across runs.
    entries: BTreeSet<String>,
}

impl Tracking {
    /// Reads the tracking file in a directory, or an empty one if there is none.
    ///
    /// Reading first, rather than overwriting, is what keeps jv from discarding
    /// what Maven already recorded about files jv did not place.
    pub fn read(directory: &Path) -> Result<Self, DriverError> {
        let path = directory.join(TRACKING_FILE);
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(source) => return Err(DriverError::Io { path, source }),
        };
        Ok(Self {
            entries: text.lines().filter_map(parse_key).collect(),
        })
    }

    /// Records a file, both as locally installed and under its repository.
    pub fn record(&mut self, file_name: &str, repository: Option<&str>) {
        self.entries.push_key(file_name, "");
        if let Some(id) = repository.filter(|id| !id.is_empty()) {
            self.entries.push_key(file_name, id);
        }
    }

    /// Whether a file name is mentioned at all.
    ///
    /// This is upstream's `isTracked`, and the reason it matters: a file that is
    /// mentioned but not under a configured repository is rejected offline,
    /// while one mentioned nowhere is accepted.
    pub fn tracks(&self, file_name: &str) -> bool {
        let prefix = format!("{}>", escape(file_name));
        self.entries.iter().any(|key| key.starts_with(&prefix))
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Writes the file, replacing whatever was there.
    ///
    /// Through a temp file and a rename, because a truncating write here is not
    /// a recoverable loss. Interrupt one mid-write and the truncation can land
    /// inside a key — leaving a file *mentioned* under an id no configured
    /// repository has, and with no locally-installed form beside it. That is
    /// precisely the state `docs/spec/local-repository.md` records as fatal:
    /// `mvn -o` then refuses an artifact that is physically present, and no
    /// amount of re-running fixes it because the artifact is already there so jv
    /// never rewrites the file.
    ///
    /// This directory belongs to Maven, so the temp name is unique per writer and
    /// the rename is atomic: a concurrent Maven never sees a partial file.
    pub fn write(&self, directory: &Path) -> Result<PathBuf, DriverError> {
        let path = directory.join(TRACKING_FILE);
        let mut text = String::with_capacity(64 + self.entries.len() * 48);
        text.push_str(HEADER);
        text.push('\n');
        for key in &self.entries {
            text.push_str(key);
            text.push_str("=\n");
        }

        let temporary = directory.join(format!("{TRACKING_FILE}.jv{}.tmp", std::process::id()));
        let write = || -> std::io::Result<()> {
            let mut file = std::fs::File::create(&temporary)?;
            std::io::Write::write_all(&mut file, text.as_bytes())?;
            // Deliberately no `sync_all`.
            //
            // The hazard this file has is a *partial* one — an artifact
            // mentioned under an id no repository has, which `mvn -o` then
            // refuses while the jar sits right there. The temp-and-rename above
            // is what prevents that: a rename is atomic, so a reader sees the
            // old file or the whole new one, never half of either.
            //
            // An fsync guards something narrower: a machine crash between the
            // rename and the journal commit. It cost more than everything else
            // `jv sync` does combined. Each one forces an ext4 journal commit
            // that flushes every hardlink and directory creation queued behind
            // it, so on a 3,800-artifact sync — around a thousand of these
            // files — the process sat in `jbd2_log_wait_commit` for about fifty
            // seconds while using four seconds of CPU.
            //
            // The trade is worth making because the loss is recoverable by the
            // thing the user would do anyway: run `jv sync` again. A tracking
            // file that never landed leaves its artifacts *untracked*, and an
            // untracked artifact resolves offline perfectly well — that is the
            // escape hatch in `docs/spec/local-repository.md`, and upstream's
            // own `testFindUntrackedFile` covers it.
            std::fs::rename(&temporary, &path)
        };
        if let Err(source) = write() {
            // Leaving a stray temp file in Maven's tree would be its own small
            // rudeness.
            let _ = std::fs::remove_file(&temporary);
            return Err(DriverError::Io {
                path: path.clone(),
                source,
            });
        }
        Ok(path)
    }
}

trait PushKey {
    fn push_key(&mut self, file_name: &str, repository: &str);
}

impl PushKey for BTreeSet<String> {
    fn push_key(&mut self, file_name: &str, repository: &str) {
        // `>` is the separator and is never escaped, on either side.
        self.insert(format!("{}>{}", escape(file_name), escape(repository)));
    }
}

/// Reads one line of a `Properties` file, returning the key with its escaping
/// left as written.
///
/// Values are always empty in this file, so only the key matters; keeping it
/// escaped means a round trip cannot change the bytes.
fn parse_key(line: &str) -> Option<String> {
    let line = line.trim_start();
    if line.is_empty() || line.starts_with('#') || line.starts_with('!') {
        return None;
    }
    // The separator is the first unescaped `=` or `:`. A backslash escapes the
    // next character, which is how a file name containing `=` survives.
    let mut escaped = false;
    for (index, character) in line.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match character {
            '\\' => escaped = true,
            '=' | ':' => return Some(line[..index].to_owned()),
            _ => {}
        }
    }
    // A line with no separator is a key with an empty value, which `load`
    // accepts and which jv should not silently drop.
    Some(line.to_owned())
}

/// Escapes a string the way `java.util.Properties.store` escapes a key.
///
/// Getting this wrong does not corrupt the file visibly — it produces a key
/// Maven's `load` reads as something else, and the artifact then fails to
/// resolve offline with no indication why.
fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '\\' => out.push_str("\\\\"),
            ' ' => out.push_str("\\ "),
            '=' => out.push_str("\\="),
            ':' => out.push_str("\\:"),
            '#' => out.push_str("\\#"),
            '!' => out.push_str("\\!"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\u{c}' => out.push_str("\\f"),
            // `store` writes ISO-8859-1 and escapes everything above it, so a
            // raw UTF-8 byte would be read back as a different character.
            character if (character as u32) > 0x7e => {
                let mut buffer = [0u16; 2];
                for unit in character.encode_utf16(&mut buffer) {
                    out.push_str(&format!("\\u{unit:04x}"));
                }
            }
            character => out.push(character),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_recorded_file_gets_both_forms() {
        let mut tracking = Tracking::default();
        tracking.record("jackson-core-2.17.1.jar", Some("central"));
        let written: Vec<&str> = tracking.entries.iter().map(String::as_str).collect();
        // The locally-installed form is what makes the entry survive a user who
        // has a mirror; the real id is recorded because it is true.
        assert_eq!(
            written,
            [
                "jackson-core-2.17.1.jar>",
                "jackson-core-2.17.1.jar>central"
            ]
        );
    }

    #[test]
    fn a_file_with_no_repository_still_gets_the_local_form() {
        let mut tracking = Tracking::default();
        tracking.record("a-1.0.jar", None);
        assert_eq!(tracking.entries.len(), 1);
        assert!(tracking.tracks("a-1.0.jar"));
    }

    #[test]
    fn the_written_file_matches_mavens_shape() {
        let dir = tempfile::tempdir().unwrap();
        let mut tracking = Tracking::default();
        tracking.record("a-1.0.pom", Some("central"));
        let path = tracking.write(dir.path()).unwrap();
        assert_eq!(path.file_name().unwrap(), TRACKING_FILE);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            format!("{HEADER}\na-1.0.pom>=\na-1.0.pom>central=\n")
        );
    }

    #[test]
    fn reading_then_writing_keeps_what_maven_recorded() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(TRACKING_FILE),
            "#NOTE: something\n#Mon Aug 17 13:23:43 EDT 2026\nold-1.0.jar>central=\n",
        )
        .unwrap();

        let mut tracking = Tracking::read(dir.path()).unwrap();
        tracking.record("new-1.0.jar", Some("central"));
        tracking.write(dir.path()).unwrap();

        let written = std::fs::read_to_string(dir.path().join(TRACKING_FILE)).unwrap();
        // Discarding Maven's own entries would leave files it downloaded
        // mentioned nowhere, which is the exact state that breaks offline
        // resolution for them.
        assert!(written.contains("old-1.0.jar>central="));
        assert!(written.contains("new-1.0.jar>="));
        // The timestamp comment is not reproduced: it would rewrite every file
        // on every sync for no reason.
        assert!(!written.contains("EDT"));
    }

    #[test]
    fn writing_leaves_no_temporary_behind() {
        let dir = tempfile::tempdir().unwrap();
        let mut tracking = Tracking::default();
        tracking.record("a-1.0.jar", Some("central"));
        tracking.write(dir.path()).unwrap();

        let strays: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name != TRACKING_FILE)
            .collect();
        assert!(strays.is_empty(), "left behind: {strays:?}");
    }

    #[test]
    fn a_rewrite_is_never_observed_half_written() {
        // The failure this guards is not theoretical: a truncation landing inside
        // a key leaves a file mentioned under an id no repository has, which
        // makes `mvn -o` refuse an artifact that is physically present — and jv
        // never rewrites it, because the artifact is already in place.
        let dir = tempfile::tempdir().unwrap();
        let mut first = Tracking::default();
        for index in 0..200 {
            first.record(&format!("artifact-{index}.jar"), Some("central"));
        }
        first.write(dir.path()).unwrap();

        let mut second = Tracking::read(dir.path()).unwrap();
        second.record("late-arrival.jar", Some("central"));
        second.write(dir.path()).unwrap();

        // Every line is complete: a key, then `=`, and nothing truncated.
        let written = std::fs::read_to_string(dir.path().join(TRACKING_FILE)).unwrap();
        for line in written.lines().filter(|line| !line.starts_with('#')) {
            assert!(line.ends_with('='), "truncated line: {line:?}");
            assert!(line.contains('>'), "malformed key: {line:?}");
        }
        assert!(written.contains("late-arrival.jar>="));
        assert!(written.contains("artifact-199.jar>="));
    }

    #[test]
    fn a_missing_file_reads_as_empty() {
        let dir = tempfile::tempdir().unwrap();
        assert!(Tracking::read(dir.path()).unwrap().is_empty());
    }

    #[test]
    fn comments_and_blank_lines_are_not_entries() {
        assert_eq!(parse_key("#NOTE: whatever"), None);
        assert_eq!(parse_key("!also a comment"), None);
        assert_eq!(parse_key("   "), None);
        assert_eq!(
            parse_key("a-1.0.jar>central="),
            Some("a-1.0.jar>central".to_owned())
        );
    }

    #[test]
    fn an_escaped_separator_does_not_end_the_key() {
        // A file name containing `=` is escaped by Maven's writer, and reading
        // it back naively would truncate the key and silently lose the entry.
        assert_eq!(
            parse_key("odd\\=name-1.0.jar>central="),
            Some("odd\\=name-1.0.jar>central".to_owned())
        );
    }

    #[test]
    fn escaping_matches_what_properties_store_produces() {
        // Verified against a real JDK 21 `Properties.store` run.
        assert_eq!(escape("a b-1.0.jar"), "a\\ b-1.0.jar");
        assert_eq!(escape("my:repo=x id"), "my\\:repo\\=x\\ id");
        assert_eq!(escape("rep#o!x"), "rep\\#o\\!x");
        assert_eq!(escape("back\\slash"), "back\\\\slash");
        // `>` is the separator and is never escaped on either side.
        assert_eq!(escape("a>b"), "a>b");
    }

    #[test]
    fn non_latin1_characters_are_written_as_unicode_escapes() {
        // `store` writes ISO-8859-1, so a raw UTF-8 byte would be read back as
        // a different character and the key would not match.
        assert_eq!(escape("caf\u{e9}"), "caf\\u00e9");
        assert_eq!(escape("\u{1f600}"), "\\ud83d\\ude00");
    }

    #[test]
    fn tracking_is_matched_on_the_file_name_prefix() {
        let mut tracking = Tracking::default();
        tracking.record("jackson-core-2.17.1.jar", Some("central"));
        assert!(tracking.tracks("jackson-core-2.17.1.jar"));
        // A sibling in the same directory is a separate question; upstream's
        // `isTracked` is per file name, not per directory.
        assert!(!tracking.tracks("jackson-core-2.17.1.pom"));
    }
}
