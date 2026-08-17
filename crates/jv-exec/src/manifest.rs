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

    // Lossy rather than strict: the spec says UTF-8, but folding splits on a
    // byte boundary, so a manifest carrying a non-ASCII vendor name can be
    // genuinely malformed. Refusing to read `Main-Class` over that would be a
    // failure caused by a field nobody asked about.
    let mut bytes = Vec::new();
    entry
        .read_to_end(&mut bytes)
        .map_err(|source| ExecError::Io {
            path: jar.to_owned(),
            source,
        })?;
    Ok(Some(String::from_utf8_lossy(&bytes).into_owned()))
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
