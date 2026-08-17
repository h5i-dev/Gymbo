//! Reading `META-INF/MANIFEST.MF` out of a jar.
//!
//! The format has two traps that a naive `split(':')` walks straight into.
//!
//! The first is line folding. The spec caps a manifest line at 72 bytes, and the
//! JDK's own writer honours it, so a long value is continued on the next line
//! with a single leading space that is *not* part of the value. Real jars hit
//! this constantly: google-java-format's manifest folds its `Add-Exports` value
//! across four lines. A parser that ignores folding reads a truncated value and
//! then fails to find a class that is right there.
//!
//! The second is sections. Everything before the first blank line is the main
//! section; after it come per-entry sections, each introduced by `Name:`, which
//! may carry attributes with the same names. Only the main section's
//! `Main-Class` means "run this", so parsing stops at the blank line.
//!
//! Attribute names are compared case-insensitively, as `java.util.jar.Attributes`
//! does.

use std::io::Read as _;
use std::path::Path;

use crate::error::ExecError;

/// The one entry name the spec allows; jars that spell it differently are not
/// jars as far as the JVM is concerned either.
const MANIFEST: &str = "META-INF/MANIFEST.MF";

/// Reads a jar's manifest, or `None` when it has none.
///
/// A jar without a manifest is unusual but legal, and it is not an error worth
/// stopping for: the caller has a ladder to fall through.
pub fn read(jar: &Path) -> Result<Option<String>, ExecError> {
    let file = std::fs::File::open(jar).map_err(|source| ExecError::Io {
        path: jar.to_owned(),
        source,
    })?;
    let mut archive = zip::ZipArchive::new(file).map_err(|source| ExecError::Jar {
        path: jar.to_owned(),
        source,
    })?;
    let mut entry = match archive.by_name(MANIFEST) {
        Ok(entry) => entry,
        Err(zip::result::ZipError::FileNotFound) => return Ok(None),
        Err(source) => {
            return Err(ExecError::Jar {
                path: jar.to_owned(),
                source,
            });
        }
    };

    // Capped, because the jar comes from a repository and its manifest is read
    // before anything about it has been established. A 6 MB jar can hold a
    // manifest that inflates to gigabytes, and reading it to the end would take
    // the machine down before the tool is ever launched. Real manifests are a
    // few kilobytes; a megabyte is already absurd.
    let mut bytes = Vec::new();
    std::io::Read::take(&mut entry, MAX_MANIFEST)
        .read_to_end(&mut bytes)
        .map_err(|source| ExecError::Io {
            path: jar.to_owned(),
            source,
        })?;
    Ok(Some(unfold(&bytes)))
}

/// The most manifest jv will read.
///
/// A cap, not a limit anyone should reach: the JDK's own manifests are a couple
/// of kilobytes.
const MAX_MANIFEST: u64 = 1 << 20;

/// Joins folded lines, then decodes.
///
/// The order matters and is the JDK's. A manifest line is wrapped at 72 *bytes*,
/// so the fold can land in the middle of a multi-byte character — decoding first
/// turns that one character into two replacement characters, and the value is
/// then wrong in a way no later processing can undo. `café.Main` folded across
/// the `é` came back as `caf<?><?>.Main`.
///
/// Decoding is still lossy at the end: the spec says UTF-8, but a manifest
/// carrying a genuinely malformed vendor name should not stop jv reading
/// `Main-Class`.
fn unfold(bytes: &[u8]) -> String {
    let mut joined: Vec<u8> = Vec::with_capacity(bytes.len());
    for (index, line) in bytes.split(|byte| *byte == b'\n').enumerate() {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        match line.strip_prefix(b" ") {
            // A continuation: append its bytes with no separator at all.
            Some(rest) if index > 0 => joined.extend_from_slice(rest),
            _ => {
                if index > 0 {
                    joined.push(b'\n');
                }
                joined.extend_from_slice(line);
            }
        }
    }
    String::from_utf8_lossy(&joined).into_owned()
}

/// The value of a main-section attribute.
pub fn attribute(manifest: &str, name: &str) -> Option<String> {
    let mut pending: Option<(String, String)> = None;
    let mut found = None;

    for raw in manifest.split('\n') {
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        if line.is_empty() {
            break;
        }
        // `read` has already joined folds at the byte level, so this does nothing
        // for a manifest that came through it. It stays because `attribute` is
        // public and reading raw manifest text with it must still work.
        if let Some(continuation) = line.strip_prefix(' ') {
            if let Some((_, value)) = pending.as_mut() {
                value.push_str(continuation);
            }
            continue;
        }
        if let Some((key, value)) = pending.take() {
            // Last one wins, matching `Attributes`' map semantics, so the loop
            // runs to the end of the section rather than returning here.
            if key.eq_ignore_ascii_case(name) {
                found = Some(value);
            }
        }
        pending = line
            .split_once(':')
            .map(|(key, value)| (key.trim().to_owned(), value.trim_start().to_owned()));
    }

    if let Some((key, value)) = pending {
        if key.eq_ignore_ascii_case(name) {
            found = Some(value);
        }
    }
    found.map(|value| value.trim_end().to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_attribute_is_read() {
        let manifest = "Manifest-Version: 1.0\nMain-Class: com.example.Main\n";
        assert_eq!(
            attribute(manifest, "Main-Class").as_deref(),
            Some("com.example.Main")
        );
    }

    #[test]
    fn a_folded_value_is_rejoined() {
        // This is google-java-format's own manifest shape: the 72-byte cap put a
        // break in the middle of a package name.
        let manifest = "Main-Class: com.google.googlejavaformat.java.Ma\n in\n";
        assert_eq!(
            attribute(manifest, "Main-Class").as_deref(),
            Some("com.google.googlejavaformat.java.Main")
        );
    }

    #[test]
    fn a_value_folded_over_several_lines_is_rejoined() {
        let manifest = "Add-Exports: jdk.compiler/com.sun.tools.javac.api jdk.compiler/com.s\n un.tools.javac.code jdk.compiler/com.sun.tools.javac.file\n";
        assert_eq!(
            attribute(manifest, "Add-Exports").as_deref(),
            Some(
                "jdk.compiler/com.sun.tools.javac.api jdk.compiler/com.sun.tools.javac.code jdk.compiler/com.sun.tools.javac.file"
            )
        );
    }

    #[test]
    fn carriage_returns_are_stripped() {
        // Jars built on Windows, and a good many built on CI, use CRLF here.
        let manifest = "Manifest-Version: 1.0\r\nMain-Class: com.example.Main\r\n";
        assert_eq!(
            attribute(manifest, "Main-Class").as_deref(),
            Some("com.example.Main")
        );
    }

    #[test]
    fn a_fold_splitting_a_character_is_rejoined_before_decoding() {
        // A manifest line wraps at 72 *bytes*, so a fold can land inside a
        // multi-byte character. Decoding first turns that one character into two
        // replacement characters and the value is wrong in a way nothing later
        // can undo — `java -jar` on the same jar reports `café.Main`.
        let folded = b"Manifest-Version: 1.0\nMain-Class: caf\xc3\n \xa9.Main\n\n";
        let text = unfold(folded);
        assert_eq!(attribute(&text, "Main-Class").as_deref(), Some("café.Main"));
    }

    #[test]
    fn unfolding_leaves_an_ordinary_manifest_alone() {
        let plain = b"Manifest-Version: 1.0\r\nMain-Class: com.example.Main\r\n\r\n";
        let text = unfold(plain);
        assert_eq!(
            attribute(&text, "Main-Class").as_deref(),
            Some("com.example.Main")
        );
    }

    #[test]
    fn names_are_matched_case_insensitively() {
        let manifest = "main-class: com.example.Main\n";
        assert_eq!(
            attribute(manifest, "Main-Class").as_deref(),
            Some("com.example.Main")
        );
    }

    #[test]
    fn a_per_entry_section_cannot_supply_the_main_class() {
        // The blank line ends the main section; anything after it describes one
        // jar entry, and treating it as global is how a signed jar's per-entry
        // digests turn into nonsense attributes.
        let manifest =
            "Manifest-Version: 1.0\n\nName: a/b/C.class\nMain-Class: com.example.NotThis\n";
        assert_eq!(attribute(manifest, "Main-Class"), None);
    }

    #[test]
    fn an_absent_attribute_yields_nothing() {
        assert_eq!(attribute("Manifest-Version: 1.0\n", "Main-Class"), None);
        assert_eq!(attribute("", "Main-Class"), None);
    }

    #[test]
    fn a_line_with_no_colon_ends_the_previous_attribute() {
        // Malformed, but it must not silently become part of the value above it.
        let manifest = "Main-Class: com.example.Main\ngarbage\n";
        assert_eq!(
            attribute(manifest, "Main-Class").as_deref(),
            Some("com.example.Main")
        );
    }

    #[test]
    fn a_manifest_without_a_trailing_newline_still_parses() {
        assert_eq!(
            attribute("Main-Class: com.example.Main", "Main-Class").as_deref(),
            Some("com.example.Main")
        );
    }
}
