//! Deciding which class to run.
//!
//! Two rungs, tried in order:
//!
//! 1. what the caller said — `--main`, or the endpoint's `@` suffix;
//! 2. `Main-Class` in the manifest of the jar the endpoint names.
//!
//! Rung 2 reads *that* jar's manifest and no other. Every transitive dependency
//! on the classpath may carry a `Main-Class` of its own, and picking one of
//! those would mean `jvx g:a` runs something the user never named. coursier
//! reaches the same conclusion from the other direction: its
//! `MainClass.retainedMainClassOpt` collects the manifests of the whole
//! classpath and then spends most of its length filtering them back down to the
//! main dependency's. Reading one jar is that answer without the search.
//!
//! `ROADMAP.md` §3.7 lists a third rung — scanning class entries for a unique
//! `*Main`-shaped candidate, as jgo's `find_main_classes` does. It is left out
//! here on purpose: a guess that produces a *running* JVM is much harder to
//! diagnose than an error naming the two rungs that were tried, and every tool
//! worth launching this way ships a manifest.

use std::path::Path;

use crate::error::ExecError;
use crate::manifest;

/// Which rung of the ladder answered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// `--main`, or the endpoint's `@`.
    Requested,
    /// The jar's `Main-Class` attribute.
    Manifest,
}

/// The class to run, and where the answer came from.
///
/// `jar` is the endpoint's own artifact, already materialized; `None` means it
/// could not be, which is a distinct case from "it has no manifest" and is
/// reported as such.
pub fn detect(
    requested: Option<&str>,
    jar: Option<&Path>,
    endpoint: &str,
) -> Result<(String, Source), ExecError> {
    let requested = requested.map(str::trim).filter(|name| !name.is_empty());
    if let Some(name) = requested {
        return Ok((name.to_owned(), Source::Requested));
    }

    let mut tried = vec!["no --main and no `@mainClass`".to_owned()];
    match jar {
        Some(jar) => {
            let manifest = manifest::read(jar)?;
            if let Some(name) = manifest
                .as_deref()
                .and_then(|manifest| manifest::attribute(manifest, "Main-Class"))
            {
                return Ok((name, Source::Manifest));
            }
            let name = jar.file_name().unwrap_or(jar.as_os_str());
            tried.push(match manifest {
                Some(_) => format!("no Main-Class in {}", name.to_string_lossy()),
                None => format!("no manifest in {}", name.to_string_lossy()),
            });
        }
        None => tried.push("the artifact itself was not downloaded".to_owned()),
    }

    Err(ExecError::NoMainClass {
        endpoint: endpoint.to_owned(),
        tried,
    })
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;
    use std::path::PathBuf;

    use super::*;

    /// Writes a jar holding exactly the manifest given, or none at all.
    fn jar(directory: &Path, manifest: Option<&str>) -> PathBuf {
        let path = directory.join("tool.jar");
        let file = std::fs::File::create(&path).expect("a jar");
        let mut writer = zip::ZipWriter::new(file);
        if let Some(manifest) = manifest {
            // Stored rather than deflated so the fixture does not depend on
            // which compression features the zip crate was built with.
            let options: zip::write::FileOptions<'_, ()> = zip::write::FileOptions::default()
                .compression_method(zip::CompressionMethod::Stored);
            writer
                .start_file("META-INF/MANIFEST.MF", options)
                .expect("an entry");
            writer.write_all(manifest.as_bytes()).expect("the manifest");
        }
        writer.finish().expect("a finished jar");
        path
    }

    #[test]
    fn an_explicit_class_wins_over_the_manifest() {
        let directory = tempfile::tempdir().expect("a temp dir");
        let jar = jar(directory.path(), Some("Main-Class: com.example.FromJar\n"));
        let (name, source) =
            detect(Some("com.example.Asked"), Some(&jar), "g:a:1.0").expect("a class");
        assert_eq!(name, "com.example.Asked");
        assert_eq!(source, Source::Requested);
    }

    #[test]
    fn the_manifest_answers_when_nothing_was_asked_for() {
        let directory = tempfile::tempdir().expect("a temp dir");
        let jar = jar(directory.path(), Some("Main-Class: com.example.FromJar\n"));
        let (name, source) = detect(None, Some(&jar), "g:a:1.0").expect("a class");
        assert_eq!(name, "com.example.FromJar");
        assert_eq!(source, Source::Manifest);
    }

    #[test]
    fn an_empty_explicit_class_falls_through() {
        // `--main ""` is a shell variable that expanded to nothing, not a
        // request to run the empty class.
        let directory = tempfile::tempdir().expect("a temp dir");
        let jar = jar(directory.path(), Some("Main-Class: com.example.FromJar\n"));
        let (name, _) = detect(Some("  "), Some(&jar), "g:a:1.0").expect("a class");
        assert_eq!(name, "com.example.FromJar");
    }

    #[test]
    fn a_jar_without_a_main_class_reports_both_rungs() {
        let directory = tempfile::tempdir().expect("a temp dir");
        let jar = jar(directory.path(), Some("Manifest-Version: 1.0\n"));
        let error = detect(None, Some(&jar), "g:a:1.0").expect_err("no class");
        let message = error.to_string();
        assert!(message.contains("g:a:1.0"), "{message}");
        assert!(message.contains("--main"), "{message}");
        assert!(message.contains("no Main-Class in tool.jar"), "{message}");
    }

    #[test]
    fn a_jar_with_no_manifest_at_all_says_so() {
        // Distinguishable from "has a manifest, no Main-Class", because the two
        // point at different mistakes: the wrong artifact versus the wrong flag.
        let directory = tempfile::tempdir().expect("a temp dir");
        let jar = jar(directory.path(), None);
        let error = detect(None, Some(&jar), "g:a:1.0").expect_err("no class");
        assert!(error.to_string().contains("no manifest in tool.jar"));
    }

    #[test]
    fn a_missing_jar_is_reported_without_reading_it() {
        let error = detect(None, None, "g:a:1.0").expect_err("no class");
        assert!(error.to_string().contains("not downloaded"));
    }

    #[test]
    fn a_file_that_is_not_a_jar_is_an_error_rather_than_a_fallthrough() {
        // A truncated download would otherwise be reported as "no manifest",
        // sending the user to look at the wrong artifact.
        let directory = tempfile::tempdir().expect("a temp dir");
        let path = directory.path().join("tool.jar");
        std::fs::write(&path, b"not a zip").expect("a file");
        let error = detect(None, Some(&path), "g:a:1.0").expect_err("a jar error");
        assert!(matches!(error, ExecError::Jar { .. }), "{error}");
    }
}
