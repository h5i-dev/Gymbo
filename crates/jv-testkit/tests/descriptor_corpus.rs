//! Reads every `.ini` artifact description in Maven Resolver's corpus.
//!
//! These files stand in for POMs in the collection tests, so being able to read
//! all of them is what will make those tests runnable against jv's collector.
//! The corpus is large — `cycle-big/` alone holds hundreds of files — which is
//! exactly what makes it worth running: a reader that copes with the hand-written
//! examples can still trip over the generated ones.
//!
//! Skips itself when `_reference/` is absent; `JV_REQUIRE_ORACLE=1` makes that a
//! failure, as CI does.

use std::path::{Path, PathBuf};

use jv_testkit::ini_descriptors;

/// Both places Maven Resolver keeps `.ini` descriptors.
///
/// The collection corpus is the bulk of it, but it happens to use no exclusions
/// or repositories; the reader's own fixtures do, and leaving them out would
/// mean those parts of the grammar went untested against real files.
fn corpus_dirs() -> Vec<PathBuf> {
    let resolver = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../_reference/maven-resolver");
    [
        "maven-resolver-impl/src/test/resources/artifact-descriptions",
        "maven-resolver-test-util/src/test/resources/org/eclipse/aether/internal/test/util",
    ]
    .iter()
    .map(|relative| resolver.join(relative))
    .filter(|path| path.is_dir())
    .collect()
}

#[test]
fn every_ini_description_parses() {
    let corpora = corpus_dirs();
    if corpora.is_empty() {
        if std::env::var_os("JV_REQUIRE_ORACLE").is_some() {
            panic!("JV_REQUIRE_ORACLE is set but _reference/maven-resolver is missing");
        }
        eprintln!("skipping: _reference/ not present (see docs/development.md)");
        return;
    }

    let mut files = 0usize;
    let mut dependencies = 0usize;
    let mut managed = 0usize;
    let mut with_exclusions = 0usize;
    let mut with_relocation = 0usize;
    let mut with_repositories = 0usize;
    let mut unmodelled_scopes: Vec<String> = Vec::new();
    let mut failures = Vec::new();

    let mut stack = corpora.clone();
    let mut paths = Vec::new();
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|ext| ext == "ini") {
                paths.push(path);
            }
        }
    }
    paths.sort();

    for path in &paths {
        let Ok(text) = std::fs::read_to_string(path) else {
            failures.push(format!("{}: unreadable", path.display()));
            continue;
        };
        files += 1;
        match ini_descriptors::parse_description(&text) {
            Ok(description) => {
                dependencies += description.dependencies.len();
                managed += description.managed_dependencies.len();
                if description
                    .dependencies
                    .iter()
                    .any(|dependency| !dependency.exclusions.is_empty())
                {
                    with_exclusions += 1;
                }
                if description.relocation.is_some() {
                    with_relocation += 1;
                }
                if !description.repositories.is_empty() {
                    with_repositories += 1;
                }
                for scope in &description.unmodelled_scopes {
                    if !unmodelled_scopes.contains(scope) {
                        unmodelled_scopes.push(scope.clone());
                    }
                }

                // Every dependency must carry usable coordinates; a silently
                // empty one would make a collection test pass for the wrong
                // reason.
                for dependency in description
                    .dependencies
                    .iter()
                    .chain(&description.managed_dependencies)
                {
                    if dependency.group_id.is_empty()
                        || dependency.artifact_id.is_empty()
                        || dependency.version.as_deref().unwrap_or_default().is_empty()
                    {
                        failures.push(format!(
                            "{}: incomplete dependency {dependency}",
                            path.display()
                        ));
                    }
                }
            }
            Err(error) => failures.push(format!("{}: {error}", path.display())),
        }
    }

    assert!(files > 500, "found only {files} descriptor files");
    assert!(
        failures.is_empty(),
        "{} descriptor file(s) failed:\n{}",
        failures.len(),
        failures
            .iter()
            .take(20)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
    // The corpus has to have exercised the constructs, or a green run says little.
    assert!(with_exclusions > 0, "no descriptor exercised exclusions");
    assert!(with_relocation > 0, "no descriptor exercised a relocation");
    assert!(managed > 0, "no descriptor exercised managed dependencies");
    // Maven's scope is a free string and jv's is an enum, so a file can use a
    // scope jv cannot represent. Three do, all of them placeholders rather than
    // real scopes: `managedScope` marks that management was applied, and
    // `scope`/`scope5` are dummies in the reader's own parser fixtures. Pinning
    // the set means a fourth shows up as a failure rather than a silent `None`.
    unmodelled_scopes.sort();
    assert_eq!(
        unmodelled_scopes,
        vec![
            "managedScope".to_owned(),
            "scope".to_owned(),
            "scope5".to_owned()
        ],
        "the set of scopes jv cannot model has changed"
    );

    eprintln!(
        "parsed {files} descriptor file(s): {dependencies} dependencies, {managed} managed, \
         {with_exclusions} with exclusions, {with_relocation} with relocations, \
         {with_repositories} with repositories, \
         {} unmodelled scope(s)",
        unmodelled_scopes.len()
    );
}
