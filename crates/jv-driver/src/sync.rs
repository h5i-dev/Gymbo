//! `jv sync` — download everything a build needs, and put it where Maven looks.
//!
//! This is jv's way into a CI pipeline that is not ready to stop using Maven.
//! `jv sync && mvn -o verify` should work: jv does the downloading, which is the
//! slow part, and Maven does the building, which is the part nobody wants to
//! reimplement. That only works if the result is indistinguishable from what
//! Maven would have downloaded itself, which is what this module is about.
//!
//! # Two passes, because a build needs more than its dependencies
//!
//! `dependency:go-offline` resolves the project's dependencies *and* its
//! plugins, and the plugins are the half people forget: a `~/.m2` with every jar
//! the code imports but no `maven-compiler-plugin` cannot compile anything.
//! Ported from `GoOfflineMojo` and `ResolverUtil.getProjectPlugins`, which take
//! plugins from three places — `<reporting><plugins>`, `<build><plugins>` and
//! `<build><pluginManagement><plugins>` — in that order.
//!
//! # Why the artifacts are hardlinked
//!
//! jv's cache is keyed by URL; Maven's local repository is keyed by coordinates.
//! Copying would double the disk a CI cache has to carry, so jv hardlinks and
//! falls back to copying when the two live on different filesystems. Either way
//! jv only ever *creates* files in the local repository, never overwrites one:
//! that directory belongs to Maven, and a file already there is Maven's answer,
//! not jv's to correct.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use jv_model::{Artifact, Dependency, Plugin, Scope};
use jv_repo::artifact_path;
use jv_resolver::{CollectRequest, Verbosity};

use crate::error::DriverError;
use crate::project::Project;
use crate::session::Session;
use crate::tracking::Tracking;

/// What to sync.
#[derive(Clone, Debug)]
pub struct SyncRequest {
    /// Resolve `<build><plugins>` and friends as well as dependencies.
    ///
    /// On by default: without it the result does not support `mvn -o`, which is
    /// the entire point.
    pub plugins: bool,
    /// Also fetch each dependency's transitive plugin dependencies.
    pub plugin_dependencies: bool,
    /// Materialize into Maven's local repository. Off leaves everything in jv's
    /// own cache, which is enough for `jv` itself but not for `mvn -o`.
    pub local_repository: Option<PathBuf>,
    /// Skip artifacts the reactor itself produces; nothing has published them.
    pub exclude_reactor: bool,
}

impl Default for SyncRequest {
    fn default() -> Self {
        Self {
            plugins: true,
            plugin_dependencies: true,
            local_repository: None,
            exclude_reactor: true,
        }
    }
}

/// What a sync did.
#[derive(Debug, Default)]
pub struct SyncReport {
    /// Artifacts now present in the cache.
    pub artifacts: Vec<Artifact>,
    /// Artifacts linked or copied into Maven's local repository.
    pub materialized: Vec<PathBuf>,
    /// Artifacts no repository has. Not fatal: an optional dependency's jar and
    /// a plugin that only exists in a private repository both land here, and
    /// failing the whole sync over one would make jv useless on real projects.
    pub missing: Vec<String>,
    pub warnings: Vec<String>,
}

impl SyncReport {
    pub fn is_empty(&self) -> bool {
        self.artifacts.is_empty()
    }
}

/// Downloads everything `projects` need and, when asked, puts it where Maven
/// looks for it.
pub fn sync(
    session: &Session,
    projects: &[&Project],
    request: &SyncRequest,
) -> Result<SyncReport, DriverError> {
    let mut report = SyncReport::default();

    // The reactor's own artifacts are built, not downloaded; asking a repository
    // for one produces a 404 that means nothing.
    let reactor: BTreeSet<String> = if request.exclude_reactor {
        projects
            .iter()
            .map(|project| {
                let artifact = project.artifact();
                format!("{}:{}", artifact.group_id, artifact.artifact_id)
            })
            .collect()
    } else {
        BTreeSet::new()
    };

    let mut wanted: Vec<Artifact> = Vec::new();

    for project in projects {
        // Test scope, because a build runs tests: it is the widest composition
        // and anything narrower leaves `mvn -o test` unable to start.
        let resolution = session.resolve_project(project, Verbosity::None)?;
        for (id, _depth) in resolution.collected.graph.preorder() {
            let node = resolution.collected.graph.node(id);
            if node.omitted_for.is_some() {
                continue;
            }
            // `system` dependencies are on a path the POM states; there is
            // nothing to download and no repository that would have them.
            if node.scope() == Scope::System {
                continue;
            }
            let Some(artifact) = &node.artifact else {
                continue;
            };
            push(&mut wanted, &reactor, artifact.clone());
        }

        if request.plugins {
            for plugin in project_plugins(project) {
                let Some(artifact) = plugin_artifact(&plugin) else {
                    // A plugin with no version is one whose version Maven would
                    // resolve from metadata at build time; jv cannot pick it
                    // without guessing, so it says so rather than guessing.
                    report.warnings.push(format!(
                        "{}: plugin {}:{} declares no version and was not synced",
                        project.path.display(),
                        plugin.group_id_or_default(),
                        plugin.artifact_id.as_deref().unwrap_or("[unknown]")
                    ));
                    continue;
                };
                push(&mut wanted, &reactor, artifact.clone());

                if request.plugin_dependencies {
                    for dependency in plugin_dependencies(session, &artifact, &plugin)? {
                        push(&mut wanted, &reactor, dependency);
                    }
                }
            }
        }
    }

    // Tracking files are per directory, and a directory holds every file of one
    // GAV, so they are collected as the artifacts are placed and written once at
    // the end. Writing per file would rewrite the same file three times.
    let mut tracking: BTreeMap<PathBuf, Tracking> = BTreeMap::new();

    for artifact in wanted {
        // The POM travels with the jar: Maven reads it on every resolve, and a
        // local repository holding jars without POMs is one `mvn -o` rejects.
        let pom = Artifact {
            classifier: String::new(),
            extension: "pom".to_owned(),
            ..artifact.clone()
        };
        for candidate in [pom, artifact] {
            match session.source().materialize(&candidate)? {
                Some(found) => {
                    if let Some(local) = &request.local_repository {
                        if let Some(linked) =
                            materialize(&found.path, local, &candidate, &mut report)?
                        {
                            record_tracking(&mut tracking, &linked, found.repository.as_deref())?;
                            report.materialized.push(linked);
                        }
                    }
                    report.artifacts.push(candidate);
                }
                None => report.missing.push(format!(
                    "{}:{}:{}:{}",
                    candidate.group_id,
                    candidate.artifact_id,
                    candidate.extension,
                    candidate.version
                )),
            }
        }
    }

    for (directory, entries) in &tracking {
        if let Err(error) = entries.write(directory) {
            // Maven resolves an untracked file anyway, so failing to write the
            // tracking file costs fidelity, not correctness.
            report
                .warnings
                .push(format!("cannot write the tracking file: {error}"));
        }
    }

    report.warnings.extend(session.warnings());
    Ok(report)
}

/// Records a placed file in its directory's tracking file, reading what Maven
/// already wrote there the first time the directory is touched.
fn record_tracking(
    tracking: &mut BTreeMap<PathBuf, Tracking>,
    placed: &Path,
    repository: Option<&str>,
) -> Result<(), DriverError> {
    let (Some(directory), Some(file_name)) = (placed.parent(), placed.file_name()) else {
        return Ok(());
    };
    let entries = match tracking.get_mut(directory) {
        Some(entries) => entries,
        None => tracking
            .entry(directory.to_path_buf())
            .or_insert(Tracking::read(directory)?),
    };
    entries.record(&file_name.to_string_lossy(), repository);
    Ok(())
}

/// Adds an artifact once, skipping anything the reactor produces.
fn push(wanted: &mut Vec<Artifact>, reactor: &BTreeSet<String>, artifact: Artifact) {
    if reactor.contains(&format!("{}:{}", artifact.group_id, artifact.artifact_id)) {
        return;
    }
    if !wanted.contains(&artifact) {
        wanted.push(artifact);
    }
}

/// The plugins a project uses, in the order `ResolverUtil.getProjectPlugins`
/// gathers them: reporting, then build, then pluginManagement.
///
/// Order decides which declaration of a duplicated plugin supplies the version,
/// so it is not cosmetic.
fn project_plugins(project: &Project) -> Vec<Plugin> {
    let mut plugins: Vec<Plugin> = Vec::new();
    let mut push_unique = |plugin: &Plugin| {
        let key = (
            plugin.group_id_or_default().to_owned(),
            plugin.artifact_id.clone(),
        );
        let already = plugins.iter().any(|held: &Plugin| {
            (
                held.group_id_or_default().to_owned(),
                held.artifact_id.clone(),
            ) == key
        });
        if !already {
            plugins.push(plugin.clone());
        }
    };

    // `<reporting>` is not modelled yet; when it is, its plugins come first.
    if let Some(build) = &project.model.build {
        for plugin in &build.plugins {
            push_unique(plugin);
        }
        for plugin in &build.plugin_management {
            push_unique(plugin);
        }
    }
    plugins
}

/// A plugin's own artifact, if it states a version.
fn plugin_artifact(plugin: &Plugin) -> Option<Artifact> {
    Some(Artifact::new(
        plugin.group_id_or_default(),
        plugin.artifact_id.as_deref()?,
        plugin.version.as_deref()?,
    ))
}

/// Everything a plugin needs to run: its own dependency tree, plus any
/// `<dependencies>` the POM added to it.
///
/// Resolved at runtime scope, which is what a plugin classloader gets.
fn plugin_dependencies(
    session: &Session,
    artifact: &Artifact,
    plugin: &Plugin,
) -> Result<Vec<Artifact>, DriverError> {
    let request = CollectRequest {
        root_dependency: Some(Dependency {
            group_id: artifact.group_id.clone(),
            artifact_id: artifact.artifact_id.clone(),
            version: Some(artifact.version.clone()),
            ..Dependency::default()
        }),
        // A `<plugin><dependencies>` entry replaces or adds to what the plugin
        // declares, which is how projects pin a driver or a compiler version.
        dependencies: plugin.dependencies.clone(),
        ..CollectRequest::default()
    };

    // A plugin jv cannot read the descriptor for is not a reason to fail the
    // sync; the plugin's own jar is already queued, and Maven will report the
    // real problem when it tries to run it.
    let Ok(resolution) = session.resolve_request(&request, Verbosity::None) else {
        return Ok(Vec::new());
    };

    let graph = &resolution.collected.graph;
    let mut artifacts = Vec::new();
    for (id, _depth) in graph.preorder() {
        let node = graph.node(id);
        if node.omitted_for.is_some() {
            continue;
        }
        if !matches!(node.scope(), Scope::Compile | Scope::Runtime) {
            continue;
        }
        if let Some(found) = &node.artifact {
            artifacts.push(found.clone());
        }
    }
    Ok(artifacts)
}

/// Puts a cached file where Maven expects it.
///
/// Returns the destination, or `None` when something is already there. A file
/// already in the local repository is Maven's, and replacing it would be jv
/// overwriting an answer it was not asked to give.
fn materialize(
    cached: &Path,
    local_repository: &Path,
    artifact: &Artifact,
    report: &mut SyncReport,
) -> Result<Option<PathBuf>, DriverError> {
    let destination = local_repository.join(artifact_path(artifact));
    if destination.exists() {
        return Ok(None);
    }
    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent).map_err(|source| DriverError::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    }

    // A hardlink costs no disk, which matters because a CI cache pays for the
    // local repository and jv's cache both. It fails across filesystems, and on
    // filesystems that have no hardlinks at all, so a copy is the fallback
    // rather than an error.
    match std::fs::hard_link(cached, &destination) {
        Ok(()) => Ok(Some(destination)),
        Err(_) => match std::fs::copy(cached, &destination) {
            Ok(_) => Ok(Some(destination)),
            Err(source) => {
                // One unwritable file should not abort a sync of a thousand.
                report
                    .warnings
                    .push(format!("cannot place {}: {source}", destination.display()));
                Ok(None)
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jv_model::{Build, Model};

    fn plugin(group: Option<&str>, artifact: &str, version: Option<&str>) -> Plugin {
        Plugin {
            group_id: group.map(str::to_owned),
            artifact_id: Some(artifact.to_owned()),
            version: version.map(str::to_owned),
            ..Plugin::default()
        }
    }

    fn project_with(build: Build) -> Project {
        Project {
            model: Model {
                group_id: Some("com.example".to_owned()),
                artifact_id: Some("app".to_owned()),
                version: Some("1.0".to_owned()),
                build: Some(build),
                ..Model::default()
            },
            path: PathBuf::from("pom.xml"),
            modules: Vec::new(),
        }
    }

    #[test]
    fn build_plugins_come_before_managed_ones() {
        // The first declaration supplies the version, so a managed entry must
        // not displace the build one.
        let project = project_with(Build {
            plugins: vec![plugin(None, "maven-compiler-plugin", Some("3.13.0"))],
            plugin_management: vec![
                plugin(None, "maven-compiler-plugin", Some("3.0.0")),
                plugin(None, "maven-surefire-plugin", Some("3.2.5")),
            ],
            ..Build::default()
        });
        let plugins = project_plugins(&project);
        assert_eq!(plugins.len(), 2);
        assert_eq!(plugins[0].version.as_deref(), Some("3.13.0"));
        assert_eq!(
            plugins[1].artifact_id.as_deref(),
            Some("maven-surefire-plugin")
        );
    }

    #[test]
    fn a_plugin_without_a_group_defaults_to_mavens() {
        let found = plugin_artifact(&plugin(None, "maven-jar-plugin", Some("3.4.1"))).unwrap();
        assert_eq!(found.group_id, "org.apache.maven.plugins");
        // And a plugin's own artifact is a jar, not a pom.
        assert_eq!(found.extension, "jar");
    }

    #[test]
    fn a_plugin_without_a_version_cannot_be_addressed() {
        // Maven would resolve it from metadata at build time; jv refuses to
        // guess, and the caller turns this into a warning.
        assert!(plugin_artifact(&plugin(None, "maven-jar-plugin", None)).is_none());
    }

    #[test]
    fn the_reactors_own_artifacts_are_not_queued() {
        let reactor: BTreeSet<String> = ["com.example:lib".to_owned()].into_iter().collect();
        let mut wanted = Vec::new();
        push(
            &mut wanted,
            &reactor,
            Artifact::new("com.example", "lib", "1.0"),
        );
        push(
            &mut wanted,
            &reactor,
            Artifact::new("org.slf4j", "slf4j-api", "2.0.9"),
        );
        // Nothing has published the reactor's own module; asking for it produces
        // a 404 that means nothing.
        assert_eq!(wanted.len(), 1);
        assert_eq!(wanted[0].artifact_id, "slf4j-api");
    }

    #[test]
    fn an_artifact_is_queued_once_however_often_it_appears() {
        let mut wanted = Vec::new();
        for _ in 0..3 {
            push(
                &mut wanted,
                &BTreeSet::new(),
                Artifact::new("org.slf4j", "slf4j-api", "2.0.9"),
            );
        }
        assert_eq!(wanted.len(), 1);
    }

    #[test]
    fn materializing_never_replaces_what_maven_already_has() {
        let dir = tempfile::tempdir().unwrap();
        let cached = dir.path().join("cached.jar");
        std::fs::write(&cached, b"from jv").unwrap();

        let local = dir.path().join("m2");
        let artifact = Artifact::new("org.slf4j", "slf4j-api", "2.0.9");
        let destination = local.join(artifact_path(&artifact));
        std::fs::create_dir_all(destination.parent().unwrap()).unwrap();
        std::fs::write(&destination, b"maven put this here").unwrap();

        let mut report = SyncReport::default();
        assert_eq!(
            materialize(&cached, &local, &artifact, &mut report).unwrap(),
            None
        );
        // That directory belongs to Maven; a file already in it is Maven's
        // answer, not jv's to correct.
        assert_eq!(std::fs::read(&destination).unwrap(), b"maven put this here");
    }

    #[test]
    fn materializing_places_a_file_at_mavens_layout() {
        let dir = tempfile::tempdir().unwrap();
        let cached = dir.path().join("cached.jar");
        std::fs::write(&cached, b"jar bytes").unwrap();
        let local = dir.path().join("m2");

        let artifact = Artifact::new("org.slf4j", "slf4j-api", "2.0.9");
        let mut report = SyncReport::default();
        let placed = materialize(&cached, &local, &artifact, &mut report)
            .unwrap()
            .expect("a destination");
        assert!(placed.ends_with("org/slf4j/slf4j-api/2.0.9/slf4j-api-2.0.9.jar"));
        assert_eq!(std::fs::read(&placed).unwrap(), b"jar bytes");
        assert!(report.warnings.is_empty());
    }
}
