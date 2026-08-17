//! Checks the layout against paths Maven actually wrote.
//!
//! Every file under `~/.m2/repository` was placed there by Maven using the
//! layout this crate reimplements, so reconstructing each path from its
//! coordinates is a differential test with a corpus that costs nothing to
//! obtain — it grows on its own as the Maven-oracle tests run.
//!
//! Skips itself when there is no local repository to read.

use std::path::{Path, PathBuf};

use jv_model::{Artifact, base_version_of};
use jv_repo::artifact_path;

fn local_repository() -> Option<PathBuf> {
    let path = std::env::var_os("JV_LOCAL_REPO")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".m2/repository"))
        })?;
    path.is_dir().then_some(path)
}

/// Recovers the coordinates a repository path encodes.
///
/// The directory gives the group, artifact and *base* version; the file name
/// gives the resolved version, the classifier and the extension. Splitting the
/// file name needs the directory's version as an anchor, because
/// `a-1.0-sources.jar` and `a-1.0-20240115.103000-7.jar` are otherwise the same
/// shape.
fn parse_repository_path(relative: &Path) -> Option<Artifact> {
    let parts: Vec<&str> = relative
        .components()
        .map(|component| component.as_os_str().to_str())
        .collect::<Option<Vec<_>>>()?;
    if parts.len() < 4 {
        return None;
    }
    let file_name = parts[parts.len() - 1];
    let directory_version = parts[parts.len() - 2];
    let artifact_id = parts[parts.len() - 3];
    let group_id = parts[..parts.len() - 3].join(".");

    let rest = file_name.strip_prefix(&format!("{artifact_id}-"))?;

    // The version is the longest prefix whose base version is the directory's,
    // so a timestamped snapshot wins over the shorter release-looking prefix.
    let mut version = None;
    for (index, byte) in rest.char_indices() {
        if byte != '-' && byte != '.' {
            continue;
        }
        let candidate = &rest[..index];
        if base_version_of(candidate) == directory_version {
            version = Some(candidate);
        }
    }
    let version = version?;
    let remainder = &rest[version.len()..];

    let (classifier, extension) = match remainder.strip_prefix('-') {
        // `-classifier.extension`; the first dot ends the classifier so that a
        // compound extension such as `tar.gz` stays whole.
        Some(tail) => {
            let (classifier, extension) = tail.split_once('.')?;
            (classifier, extension)
        }
        None => ("", remainder.strip_prefix('.')?),
    };

    Some(Artifact {
        group_id,
        artifact_id: artifact_id.to_owned(),
        version: version.to_owned(),
        classifier: classifier.to_owned(),
        extension: extension.to_owned(),
    })
}

#[test]
fn every_local_artifact_path_round_trips() {
    let Some(repository) = local_repository() else {
        eprintln!("skipping: no local repository (run the Maven oracle tests to populate one)");
        return;
    };

    let mut checked = 0usize;
    let mut unparsed = Vec::new();
    let mut mismatched = Vec::new();
    let mut deep_groups = 0usize;
    let mut with_classifier = 0usize;

    let mut stack = vec![repository.clone()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let is_artifact = path
                .extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| matches!(ext, "jar" | "pom" | "war" | "zip"));
            if !is_artifact {
                continue;
            }
            let Ok(relative) = path.strip_prefix(&repository) else {
                continue;
            };
            let expected = relative.to_string_lossy().replace('\\', "/");

            match parse_repository_path(relative) {
                None => unparsed.push(expected),
                Some(artifact) => {
                    checked += 1;
                    if artifact.group_id.contains('.') {
                        deep_groups += 1;
                    }
                    if !artifact.classifier.is_empty() {
                        with_classifier += 1;
                    }
                    let actual = artifact_path(&artifact);
                    if actual != expected {
                        mismatched.push(format!("  expected {expected}\n  got      {actual}"));
                    }
                }
            }
        }
    }

    if checked == 0 {
        eprintln!("skipping: local repository holds no artifacts yet");
        return;
    }

    assert!(
        mismatched.is_empty(),
        "{} path(s) did not round-trip:\n{}",
        mismatched.len(),
        mismatched.join("\n")
    );
    assert!(
        unparsed.is_empty(),
        "{} path(s) could not be read back into coordinates:\n{}",
        unparsed.len(),
        unparsed.join("\n")
    );
    // Dotted group ids are the case that turns into nested directories, so a run
    // that saw none would not have tested much.
    assert!(deep_groups > 0, "no artifact had a dotted group id");

    eprintln!(
        "{checked} local artifact path(s) round-tripped \
         ({deep_groups} with dotted group ids, {with_classifier} with classifiers)"
    );
}
