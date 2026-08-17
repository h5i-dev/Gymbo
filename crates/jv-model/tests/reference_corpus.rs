//! Parses every POM, `settings.xml` and `maven-metadata.xml` in the reference
//! clones.
//!
//! The unit tests check that the parser handles constructs someone thought to
//! write down. This checks it against a few thousand POMs written by other
//! people over two decades, including Maven's own deliberately malformed test
//! fixtures.
//!
//! The invariant asserted is precise rather than statistical: **jv fails to
//! parse a file only when the file is not well-formed XML, or its root element
//! is not `<project>`.** Anything else is a parser bug. Stating it this way
//! keeps the test portable — it needs no allowlist of known-bad paths, so it
//! stays meaningful as the corpus changes.
//!
//! Skips itself when `_reference/` is absent; set `JV_REQUIRE_ORACLE=1` to make
//! that a failure, as CI does.

use std::path::{Path, PathBuf};
use std::time::Instant;

use jv_model::{ParseError, TypeRegistry, parse_metadata, parse_pom, parse_settings};

fn reference_dir() -> Option<PathBuf> {
    let candidate = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("_reference");
    candidate.is_dir().then_some(candidate)
}

/// Whether a document is well-formed XML, judged independently of jv's POM
/// parsing so that "we rejected it because it is broken" is a checkable claim
/// rather than an excuse.
fn is_well_formed_xml(text: &str) -> bool {
    let mut reader = quick_xml::Reader::from_str(text);
    reader.config_mut().check_end_names = true;
    reader.config_mut().expand_empty_elements = true;
    let mut depth = 0i32;
    loop {
        match reader.read_event() {
            // Reaching the end with elements still open is not well-formed, and
            // the reader does not report it on its own.
            Ok(quick_xml::events::Event::Eof) => return depth == 0,
            Ok(quick_xml::events::Event::Start(_)) => depth += 1,
            Ok(quick_xml::events::Event::End(_)) => depth -= 1,
            Ok(_) => {}
            Err(_) => return false,
        }
    }
}

#[derive(Default)]
struct Stats {
    files: usize,
    parsed: usize,
    malformed_xml: usize,
    not_a_project: usize,
    with_parent: usize,
    with_profiles: usize,
    with_management: usize,
    with_relocation: usize,
    dependencies: usize,
    managed_dependencies: usize,
    plugins: usize,
    warnings: usize,
}

#[test]
fn parses_every_reference_pom() {
    let Some(reference) = reference_dir() else {
        if std::env::var_os("JV_REQUIRE_ORACLE").is_some() {
            panic!("JV_REQUIRE_ORACLE is set but _reference/ is missing");
        }
        eprintln!("skipping: _reference/ not present (see docs/development.md)");
        return;
    };

    let mut stats = Stats::default();
    let mut bugs: Vec<String> = Vec::new();
    let mut warning_examples: Vec<String> = Vec::new();
    let types = TypeRegistry::new();
    let started = Instant::now();

    for entry in walkdir::WalkDir::new(&reference)
        .follow_links(true)
        .into_iter()
        .filter_map(Result::ok)
    {
        let path = entry.path();
        if !entry.file_type().is_file() {
            continue;
        }
        let is_pom = path.file_name().is_some_and(|n| n == "pom.xml")
            || path.extension().is_some_and(|e| e == "pom");
        if !is_pom {
            continue;
        }
        // A handful of fixtures are not UTF-8; Maven would reject those too, and
        // they say nothing about the parser.
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        stats.files += 1;

        match parse_pom(&text) {
            Ok(pom) => {
                stats.parsed += 1;
                let model = &pom.model;
                stats.with_parent += usize::from(model.parent.is_some());
                stats.with_profiles += usize::from(!model.profiles.is_empty());
                stats.with_management += usize::from(!model.dependency_management.is_empty());
                stats.dependencies += model.dependencies.len();
                stats.managed_dependencies += model.dependency_management.len();
                if let Some(build) = &model.build {
                    stats.plugins += build.plugins.len() + build.plugin_management.len();
                }
                if model
                    .distribution_management
                    .as_ref()
                    .is_some_and(|d| d.relocation.is_some())
                {
                    stats.with_relocation += 1;
                }
                stats.warnings += pom.warnings.len();
                if !pom.warnings.is_empty() && warning_examples.len() < 5 {
                    warning_examples.push(format!("{}: {}", path.display(), pom.warnings[0]));
                }

                // Every dependency must expand to a usable artifact identity.
                for dependency in model
                    .dependencies
                    .iter()
                    .chain(&model.dependency_management)
                {
                    let artifact = dependency.to_artifact(&types);
                    if artifact.extension.is_empty() {
                        bugs.push(format!(
                            "{}: dependency {dependency} produced an empty extension",
                            path.display()
                        ));
                    }
                }
            }
            Err(ParseError::UnexpectedRoot { .. }) => stats.not_a_project += 1,
            Err(error) => {
                if is_well_formed_xml(&text) {
                    bugs.push(format!("{}: {error}", path.display()));
                } else {
                    stats.malformed_xml += 1;
                }
            }
        }
    }

    let elapsed = started.elapsed();

    assert!(
        stats.files > 1000,
        "found only {} POMs under {}; the corpus walk is broken",
        stats.files,
        reference.display()
    );

    assert!(
        bugs.is_empty(),
        "{} well-formed POM(s) failed to parse:\n{}",
        bugs.len(),
        bugs.iter().take(20).cloned().collect::<Vec<_>>().join("\n")
    );

    // The corpus must actually exercise the interesting constructs, or a green
    // run would prove very little.
    assert!(stats.with_parent > 100, "too few POMs with a <parent>");
    assert!(stats.with_profiles > 20, "too few POMs with <profiles>");
    assert!(
        stats.with_management > 20,
        "too few POMs with <dependencyManagement>"
    );
    assert!(stats.dependencies > 1000, "too few dependencies parsed");

    eprintln!(
        "parsed {}/{} POMs in {:?} ({} malformed XML, {} not a project)\n  \
         {} with parent, {} with profiles, {} with dependencyManagement, {} with relocation\n  \
         {} dependencies, {} managed, {} plugins, {} warnings",
        stats.parsed,
        stats.files,
        elapsed,
        stats.malformed_xml,
        stats.not_a_project,
        stats.with_parent,
        stats.with_profiles,
        stats.with_management,
        stats.with_relocation,
        stats.dependencies,
        stats.managed_dependencies,
        stats.plugins,
        stats.warnings,
    );
    for example in &warning_examples {
        eprintln!("  warning example: {example}");
    }
}

/// Counts of one XML format's corpus run.
struct FormatRun {
    files: usize,
    parsed: usize,
    malformed_xml: usize,
    wrong_root: usize,
    bugs: Vec<String>,
}

/// Applies the same invariant as the POM test to another Maven XML format:
/// parsing fails only for malformed XML or an unexpected root element.
fn run_format<T>(
    reference: &Path,
    matches_name: impl Fn(&Path) -> bool,
    parse: impl Fn(&str) -> Result<T, ParseError>,
    mut inspect: impl FnMut(&T, &Path, &mut Vec<String>),
) -> FormatRun {
    let mut run = FormatRun {
        files: 0,
        parsed: 0,
        malformed_xml: 0,
        wrong_root: 0,
        bugs: Vec::new(),
    };
    for entry in walkdir::WalkDir::new(reference)
        .follow_links(true)
        .into_iter()
        .filter_map(Result::ok)
    {
        let path = entry.path();
        if !entry.file_type().is_file() || !matches_name(path) {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        run.files += 1;
        match parse(&text) {
            Ok(value) => {
                run.parsed += 1;
                inspect(&value, path, &mut run.bugs);
            }
            Err(ParseError::UnexpectedRoot { .. }) => run.wrong_root += 1,
            Err(error) => {
                if is_well_formed_xml(&text) {
                    run.bugs.push(format!("{}: {error}", path.display()));
                } else {
                    run.malformed_xml += 1;
                }
            }
        }
    }
    run
}

#[test]
fn parses_every_reference_settings_file() {
    let Some(reference) = reference_dir() else {
        if std::env::var_os("JV_REQUIRE_ORACLE").is_some() {
            panic!("JV_REQUIRE_ORACLE is set but _reference/ is missing");
        }
        eprintln!("skipping: _reference/ not present (see docs/development.md)");
        return;
    };

    let mut with_mirrors = 0usize;
    let mut with_servers = 0usize;
    let mut with_profiles = 0usize;
    let run = run_format(
        &reference,
        |path| {
            path.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("settings") && n.ends_with(".xml"))
        },
        parse_settings,
        |settings, _path, _bugs| {
            with_mirrors += usize::from(!settings.mirrors.is_empty());
            with_servers += usize::from(!settings.servers.is_empty());
            with_profiles += usize::from(!settings.profiles.is_empty());
        },
    );

    assert!(run.files > 50, "found only {} settings files", run.files);
    assert!(
        run.bugs.is_empty(),
        "{} well-formed settings file(s) failed to parse:\n{}",
        run.bugs.len(),
        run.bugs.join("\n")
    );
    // Not every file named settings*.xml is a settings file; the corpus holds
    // Modello schemas and plugin fixtures too. What matters is that the ones that
    // are get read, and that the interesting sections are exercised.
    assert!(with_mirrors > 0, "no settings file exercised <mirrors>");
    assert!(with_servers > 0, "no settings file exercised <servers>");
    eprintln!(
        "parsed {}/{} settings files ({} malformed XML, {} not settings); \
         {with_mirrors} with mirrors, {with_servers} with servers, {with_profiles} with profiles",
        run.parsed, run.files, run.malformed_xml, run.wrong_root
    );
}

#[test]
fn parses_every_reference_metadata_file() {
    let Some(reference) = reference_dir() else {
        if std::env::var_os("JV_REQUIRE_ORACLE").is_some() {
            panic!("JV_REQUIRE_ORACLE is set but _reference/ is missing");
        }
        eprintln!("skipping: _reference/ not present (see docs/development.md)");
        return;
    };

    let mut with_versions = 0usize;
    let mut with_snapshot = 0usize;
    let mut resolvable_snapshots = 0usize;
    let run = run_format(
        &reference,
        |path| {
            path.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("maven-metadata") && n.ends_with(".xml"))
        },
        parse_metadata,
        |metadata, path, bugs| {
            with_versions += usize::from(!metadata.versions().is_empty());
            if metadata
                .versioning
                .as_ref()
                .is_some_and(|v| v.snapshot.is_some() || !v.snapshot_versions.is_empty())
            {
                with_snapshot += 1;
                if metadata.snapshot_version("jar", "").is_some() {
                    resolvable_snapshots += 1;
                }
            }
            // A version list with an empty entry would silently become a request
            // for the artifact's directory itself.
            if metadata.versions().iter().any(|v| v.is_empty()) {
                bugs.push(format!(
                    "{}: metadata lists an empty version",
                    path.display()
                ));
            }
        },
    );

    assert!(run.files > 100, "found only {} metadata files", run.files);
    assert!(
        run.bugs.is_empty(),
        "{} well-formed metadata file(s) failed to parse:\n{}",
        run.bugs.len(),
        run.bugs.join("\n")
    );
    assert!(
        with_versions > 20,
        "too few metadata files listing versions"
    );
    assert!(with_snapshot > 0, "no metadata file exercised snapshots");
    eprintln!(
        "parsed {}/{} metadata files ({} malformed XML, {} not metadata); \
         {with_versions} listing versions, {with_snapshot} with snapshots \
         ({resolvable_snapshots} resolvable)",
        run.parsed, run.files, run.malformed_xml, run.wrong_root
    );
}
