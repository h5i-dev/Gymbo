//! Differential test against real Maven's effective POM.
//!
//! `mvn help:effective-pom` prints exactly what jv's pipeline is trying to
//! reproduce, so comparing against it is the strongest available check: it
//! catches every ordering mistake, every merge rule applied in the wrong
//! direction, and every property resolved from the wrong source — including cases
//! nobody thought to write a test for.
//!
//! Maven's output is itself a POM, so jv's own parser reads it, and the
//! comparison is between two `Model` values rather than between two blobs of
//! text. Fields jv deliberately does not model are excluded from the comparison
//! and listed in the crate docs.
//!
//! Skipped unless a Maven 3.9 is available. Point `JV_MVN` at one, or have `mvn`
//! on `PATH`; `JV_REQUIRE_ORACLE=1` turns absence into a failure. The first run
//! populates `~/.m2` with the help plugin and the fixtures' BOMs, after which the
//! test needs no network.

use std::path::{Path, PathBuf};
use std::process::Command;

use jv_model::{Dependency, Model, Plugin, PluginExecution, parse_pom};
use jv_model_builder::{BuildContext, ModelBuilder, ModelSource, SourcedModel};

/// Finds a Maven launcher, or explains why there is none.
fn maven() -> Result<PathBuf, String> {
    if let Some(from_env) = std::env::var_os("JV_MVN") {
        let path = PathBuf::from(from_env);
        if path.is_file() {
            return Ok(path);
        }
        return Err(format!(
            "JV_MVN points at {}, which is not a file",
            path.display()
        ));
    }
    let probe = Command::new("mvn").arg("-v").output();
    match probe {
        Ok(output) if output.status.success() => Ok(PathBuf::from("mvn")),
        _ => Err("no mvn on PATH and JV_MVN is unset".to_owned()),
    }
}

fn local_repository() -> PathBuf {
    std::env::var_os("JV_LOCAL_REPO")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".m2/repository")))
        .expect("a home directory")
}

/// Reads POMs from the filesystem and from the local repository.
///
/// A fixture's parent resolves through `<relativePath>`, and its BOMs through the
/// local repository that Maven has already populated — which is what makes this
/// test hermetic after its first run.
struct LocalModelSource {
    local_repository: PathBuf,
}

impl ModelSource for LocalModelSource {
    fn get(
        &self,
        group_id: &str,
        artifact_id: &str,
        version: &str,
    ) -> Result<SourcedModel, String> {
        let path = self
            .local_repository
            .join(group_id.replace('.', "/"))
            .join(artifact_id)
            .join(version)
            .join(format!("{artifact_id}-{version}.pom"));
        let xml = std::fs::read_to_string(&path)
            .map_err(|error| format!("{}: {error}", path.display()))?;
        let parsed = parse_pom(&xml).map_err(|error| error.to_string())?;
        Ok(SourcedModel::new(
            parsed.model,
            format!("{group_id}:{artifact_id}:{version}"),
        ))
    }

    fn get_at_path(&self, path: &Path) -> Result<Option<SourcedModel>, String> {
        let candidate = if path.is_dir() {
            path.join("pom.xml")
        } else {
            path.to_path_buf()
        };
        if !candidate.is_file() {
            return Ok(None);
        }
        let xml = std::fs::read_to_string(&candidate)
            .map_err(|error| format!("{}: {error}", candidate.display()))?;
        let parsed = parse_pom(&xml).map_err(|error| error.to_string())?;
        let basedir = candidate
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_default();
        Ok(Some(
            SourcedModel::new(parsed.model, candidate.display().to_string()).with_basedir(basedir),
        ))
    }
}

/// Asks Maven for a project's effective POM.
fn maven_effective_pom(mvn: &Path, project: &Path) -> Result<Model, String> {
    let output_file = std::env::temp_dir().join(format!(
        "jv-effective-{}.xml",
        project
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("project")
    ));
    let output = Command::new(mvn)
        .current_dir(project)
        // `-N` keeps an aggregator from reporting its whole reactor, which would
        // arrive wrapped in <projects> rather than as one <project>.
        .args(["-q", "-B", "-N", "help:effective-pom"])
        .arg(format!("-Doutput={}", output_file.display()))
        .output()
        .map_err(|error| format!("cannot run {}: {error}", mvn.display()))?;
    if !output.status.success() {
        return Err(format!(
            "mvn failed in {}:\n{}\n{}",
            project.display(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    let xml = std::fs::read_to_string(&output_file)
        .map_err(|error| format!("{}: {error}", output_file.display()))?;
    let parsed = parse_pom(&xml).map_err(|error| error.to_string())?;
    Ok(parsed.model)
}

/// jv's answer for the same project.
fn jv_effective_pom(project: &Path) -> Result<Model, String> {
    let pom = project.join("pom.xml");
    let xml =
        std::fs::read_to_string(&pom).map_err(|error| format!("{}: {error}", pom.display()))?;
    let model = parse_pom(&xml).map_err(|error| error.to_string())?.model;
    let source = LocalModelSource {
        local_repository: local_repository(),
    };
    // Maven builds a *project* model, which always has plugin processing on, so
    // the comparison is only like-for-like with the bindings injected.
    let built = ModelBuilder::new(&source, BuildContext::from_environment())
        .with_lifecycle_bindings(true)
        .build(SourcedModel::new(model, pom.display().to_string()).with_basedir(project))
        .map_err(|error| error.to_string())?;
    Ok(built.model)
}

/// A dependency rendered so that a mismatch reads clearly.
fn describe(dependency: &Dependency) -> String {
    let mut text = dependency.to_string();
    if dependency.is_optional() {
        text.push_str(" (optional)");
    }
    if !dependency.exclusions.is_empty() {
        let mut exclusions: Vec<String> = dependency
            .exclusions
            .iter()
            .map(ToString::to_string)
            .collect();
        exclusions.sort();
        text.push_str(&format!(" excluding [{}]", exclusions.join(", ")));
    }
    text
}

/// A plugin rendered so that a mismatch reads clearly: coordinates, then every
/// execution as `id@phase[goals]`.
///
/// Executions are the part worth comparing. The coordinates alone would pass with
/// a plugin bound to no phase at all, which is exactly the mistake a hand-written
/// binding table makes.
fn describe_plugin(plugin: &Plugin) -> String {
    let mut text = format!(
        "{}:{}:{}",
        plugin.group_id_or_default(),
        plugin.artifact_id.as_deref().unwrap_or("?"),
        plugin.version.as_deref().unwrap_or("?")
    );
    let executions: Vec<String> = plugin.executions.iter().map(describe_execution).collect();
    if !executions.is_empty() {
        text.push_str(&format!(" {{{}}}", executions.join(" ")));
    }
    text
}

fn describe_execution(execution: &PluginExecution) -> String {
    format!(
        "{}@{}[{}]",
        execution.id_or_default(),
        execution.phase.as_deref().unwrap_or("-"),
        execution.goals.join(",")
    )
}

fn describe_plugins(model: &Model) -> String {
    model
        .build
        .as_ref()
        .map(|build| {
            build
                .plugins
                .iter()
                .map(describe_plugin)
                .collect::<Vec<_>>()
                .join("\n       ")
        })
        .unwrap_or_default()
}

/// Compares the parts of a model jv claims to reproduce.
fn compare(project: &Path, expected: &Model, actual: &Model, failures: &mut Vec<String>) {
    let mut mismatch = |what: &str, expected: String, actual: String| {
        if expected != actual {
            failures.push(format!(
                "{}: {what}\n  mvn: {expected}\n  jv:  {actual}",
                project.display()
            ));
        }
    };

    mismatch(
        "groupId",
        format!("{:?}", expected.group_id),
        format!("{:?}", actual.group_id),
    );
    mismatch(
        "artifactId",
        format!("{:?}", expected.artifact_id),
        format!("{:?}", actual.artifact_id),
    );
    mismatch(
        "version",
        format!("{:?}", expected.version),
        format!("{:?}", actual.version),
    );
    mismatch(
        "packaging",
        expected.packaging_or_default().to_owned(),
        actual.packaging_or_default().to_owned(),
    );

    // Order matters: it decides which declaration a resolver sees first.
    mismatch(
        "dependencies",
        expected
            .dependencies
            .iter()
            .map(describe)
            .collect::<Vec<_>>()
            .join("\n       "),
        actual
            .dependencies
            .iter()
            .map(describe)
            .collect::<Vec<_>>()
            .join("\n       "),
    );
    mismatch(
        "dependencyManagement",
        expected
            .dependency_management
            .iter()
            .map(describe)
            .collect::<Vec<_>>()
            .join("\n       "),
        actual
            .dependency_management
            .iter()
            .map(describe)
            .collect::<Vec<_>>()
            .join("\n       "),
    );

    // Build plugins, in order: this is the only check on the lifecycle bindings
    // that does not just compare jv against jv's own copy of the binding table.
    mismatch(
        "build plugins",
        describe_plugins(expected),
        describe_plugins(actual),
    );

    // Properties: Maven adds a few of its own that jv has no reason to invent, so
    // this checks that every property Maven reports and jv could know about
    // agrees, rather than demanding identical maps.
    for (key, value) in &expected.properties {
        if let Some(ours) = actual.properties.get(key) {
            mismatch(&format!("property {key}"), value.clone(), ours.clone());
        }
    }

    mismatch(
        "repository ids",
        expected
            .repositories
            .iter()
            .filter_map(|repository| repository.id.clone())
            .collect::<Vec<_>>()
            .join(", "),
        actual
            .repositories
            .iter()
            .filter_map(|repository| repository.id.clone())
            .collect::<Vec<_>>()
            .join(", "),
    );
}

/// Fixture directories, each holding a `pom.xml`, relative to this crate.
fn fixtures() -> Vec<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let mut projects = Vec::new();
    let Ok(entries) = std::fs::read_dir(&root) else {
        return projects;
    };
    for entry in entries.flatten() {
        collect_projects(&entry.path(), &mut projects);
    }
    projects.sort();
    projects
}

fn collect_projects(dir: &Path, projects: &mut Vec<PathBuf>) {
    if !dir.is_dir() {
        return;
    }
    if dir.join("pom.xml").is_file() {
        projects.push(dir.to_path_buf());
    }
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            collect_projects(&entry.path(), projects);
        }
    }
}

#[test]
fn effective_poms_match_maven() {
    let required = std::env::var_os("JV_REQUIRE_ORACLE").is_some();
    let mvn = match maven() {
        Ok(path) => path,
        Err(why) => {
            if required {
                panic!("JV_REQUIRE_ORACLE is set but {why}");
            }
            eprintln!("skipping the Maven oracle: {why}");
            return;
        }
    };

    let projects = fixtures();
    assert!(
        projects.len() >= 3,
        "found only {} fixture projects; the walk is broken",
        projects.len()
    );

    let mut failures = Vec::new();
    let mut compared = 0usize;
    for project in &projects {
        let expected = match maven_effective_pom(&mvn, project) {
            Ok(model) => model,
            Err(why) => {
                failures.push(format!("{}: {why}", project.display()));
                continue;
            }
        };
        let actual = match jv_effective_pom(project) {
            Ok(model) => model,
            Err(why) => {
                failures.push(format!(
                    "{}: jv could not build the model: {why}",
                    project.display()
                ));
                continue;
            }
        };
        compare(project, &expected, &actual, &mut failures);
        compared += 1;
    }

    assert!(
        failures.is_empty(),
        "{} disagreement(s) with Maven across {compared} project(s):\n\n{}",
        failures.len(),
        failures.join("\n\n")
    );
    eprintln!("effective POMs match Maven on {compared} project(s)");
}
