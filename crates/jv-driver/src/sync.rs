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
//! # Plugins that choose dependencies at run time
//!
//! Surefire does not declare which test framework it will run. It inspects the
//! test classpath at execution time and resolves a matching *provider* —
//! `surefire-junit-platform` for JUnit 5, `surefire-testng` for TestNG, and so
//! on — from coordinates that appear in no POM. `mvn dependency:go-offline` does
//! not find them either: a repository it populated fails `mvn -o verify` at the
//! test phase, which was confirmed by running it.
//!
//! jv does better rather than matching that, because "sync then build offline"
//! is the entire proposition and a sync that cannot run tests does not deliver
//! it. When surefire or failsafe is in the plugin set, jv fetches every provider
//! at the plugin's own version. The list is small, stable, and versioned in
//! lockstep with the plugin; a provider that does not exist for some version
//! simply 404s and is skipped, which is already how a missing artifact behaves.
//!
//! # Snapshots
//!
//! A snapshot cannot simply be placed. In a repository its file name carries a
//! deployment timestamp, and Maven only learns which timestamp is current by
//! reading `maven-metadata-<repositoryId>.xml` beside it — where that id is the
//! *effective* repository id, the mirror's when the user has a mirror. jv
//! cannot know how the next `mvn` invocation will be configured, and metadata
//! under the wrong id is worse than none: the artifact is present and still
//! unresolvable.
//!
//! So jv writes the layout `mvn install` produces instead, which carries no
//! repository id at all — the file under its base `-SNAPSHOT` name plus a
//! `maven-metadata-local.xml` declaring `<localCopy>true</localCopy>`. Maven
//! accepts that from any configuration because it is the shape Maven writes
//! itself, and it is honest: jv put the file there, so it is locally installed
//! whatever it was downloaded from. See [`crate::snapshot`].
//!
//! # Known gap: `mvn -o site`
//!
//! `verify`, `install` and `deploy` work against a synced repository — they are
//! bound by the packaging's lifecycle, so their plugins are *declared* and
//! their closures travel. `site` does not.
//!
//! `maven-site-plugin` resolves its reports at run time, and the default,
//! `maven-project-info-reports-plugin`, appears in no POM anywhere — not in
//! `<plugins>`, not in `<pluginManagement>`, not in any parent. It is the same
//! shape as Surefire choosing a test provider, which is handled above, and it
//! wants the same treatment: a small list of what the plugin picks for itself.
//! `<reporting><plugins>` is modelled, which is where a project names the
//! reports it actually wants; those come first and count as declared.
//!
//! Verified rather than assumed: `mvn -o install` reaches BUILD SUCCESS on
//! spring-petclinic and `mvn -o site` does not, failing on exactly that plugin.
//!
//! # Profiles must match the build's
//!
//! `jv sync` and `mvn` are separate invocations, and what a profile
//! contributes — dependencies, plugins, repositories — is decided at the moment
//! each one runs. A profile active during the build but not during the sync
//! leaves its artifacts unfetched, and `mvn -o` then fails on them.
//!
//! So a CI job passing `-P release` to the build has to pass it to the sync as
//! well. This has always been true of the dependency half; skipping the
//! dependency closure of undeclared `<pluginManagement>` entries extends it to
//! plugins, since a profile is one of the places a plugin gets *declared*.
//! `plugin_origin.rs` pins both directions.
//!
//! # Why the artifacts are hardlinked
//!
//! jv's cache is keyed by URL; Maven's local repository is keyed by coordinates.
//! Copying would double the disk a CI cache has to carry, so jv hardlinks and
//! falls back to copying when the two live on different filesystems. Either way
//! jv only ever *creates* files in the local repository, never overwrites one:
//! that directory belongs to Maven, and a file already there is Maven's answer,
//! not jv's to correct.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};

use jv_model::toolchains::{self, Toolchains};
use jv_model::{Artifact, Dependency, Plugin, Scope, is_snapshot_version};
use jv_resolver::{CollectRequest, Verbosity};

use crate::error::DriverError;
use crate::project::Project;
use crate::session::Session;
use crate::snapshot::LocalSnapshot;
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
    /// Also resolve the dependency closure of `<pluginManagement>` entries no
    /// `<plugins>` block declares.
    ///
    /// Off by default. Management supplies a version and configuration to
    /// plugins that are declared; an entry nothing declares never enters a
    /// build plan, so Maven never loads its dependencies. Including them cost
    /// 245 MB of 345 MB on spring-petclinic — a Kotlin compiler, jOOQ,
    /// Liquibase and Saxon, in a single-module Java project.
    ///
    /// Turn it on for the one case the default gives up: invoking a
    /// management-only plugin directly, as `mvn -o some:goal`.
    pub managed_plugin_dependencies: bool,
    /// The toolchains this machine provides, for the check below.
    ///
    /// Toolchains have no effect on resolution — nothing in Maven's model
    /// building looks at them. They are here because a sync that reports
    /// success and leaves a project `mvn -o verify` cannot build is a false
    /// success, and jv is holding the POM at the moment it could say so.
    pub toolchains: Toolchains,

    /// Extra artifacts to sync, beyond anything a POM declares.
    ///
    /// The escape hatch for what cannot be read. A plugin may resolve a default
    /// of its own when it runs, from a version compiled into the plugin rather
    /// than written down: `spotless` picks its formatter that way, and which
    /// version depends on the spotless release. jv reads coordinates out of
    /// plugin `<configuration>`, but a constant inside a jar is not in the
    /// project to be read.
    ///
    /// Carrying a table of every plugin's defaults would mean carrying
    /// something wrong the next time any of them ships a release. This is the
    /// alternative: name them, and they are fetched with their closures like
    /// anything else.
    pub also: Vec<Artifact>,
}

impl Default for SyncRequest {
    fn default() -> Self {
        Self {
            plugins: true,
            plugin_dependencies: true,
            local_repository: None,
            exclude_reactor: true,
            managed_plugin_dependencies: false,
            toolchains: Toolchains::default(),
            also: Vec::new(),
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
                // Version included, deliberately. See `Wanted::push`.
                format!(
                    "{}:{}:{}",
                    artifact.group_id, artifact.artifact_id, artifact.version
                )
            })
            .collect()
    } else {
        BTreeSet::new()
    };

    let mut wanted = Wanted::default();
    // Whether any plugin in the build resolves a test provider at run time.
    let mut selects_providers = false;
    // Whether the build runs `maven-site-plugin`, which picks its reports at
    // execution time.
    let mut selects_reports = false;

    // Point the POM crawler at every plugin before anything is resolved.
    //
    // The crawler is otherwise seeded only from dependencies, so plugin POM
    // chains were fetched cold — and because plugin dependencies are resolved
    // one plugin at a time, those chains went out sequentially. Seeding them
    // here overlaps the whole set with the dependency resolve that follows.
    if request.plugins {
        let plugins: Vec<Artifact> = projects
            .iter()
            .flat_map(|project| project_plugins(project))
            .filter_map(|(plugin, _origin)| plugin_artifact(&plugin))
            .collect();
        session.source().prefetch_artifacts(plugins);
    }

    // Every plugin's dependency closure, resolved before the loop that orders
    // them.
    //
    // Each closure is its own `resolve_request`, and on a warm cache they are
    // where a sync spends nearly all of its time. Measured on commons-io with
    // both the store and the local repository already populated: 442ms of a
    // 466ms sync, against 18ms to resolve the project's own dependency tree and
    // 35ms to place 5,602 files. They were resolved one plugin at a time, and
    // no closure depends on another, so they are resolved together here.
    //
    // The loop below stays sequential and reads the answers rather than
    // computing them. That is deliberate: `_remote.repositories` and the
    // snapshot metadata both depend on the order artifacts are pushed, so the
    // ordering has to come out exactly as it did before even though the work it
    // was interleaved with did not have to.
    //
    // One real change, worth stating rather than discovering later: closures
    // used to resolve *after* the project whose loop iteration they sat in had
    // its dependency graph collected, so they could see repositories that a
    // dependency's POM declared along the way (`Trust::Untrusted`). They now
    // resolve before any of that. A project's own repositories are unaffected —
    // `load_project` registers those before this function is called — so this
    // only bites a plugin whose dependencies live solely in a repository that
    // nothing but a transitive dependency's POM mentions. No corpus project
    // does that, but "no corpus project does" is not "cannot happen".
    let closures = if request.plugins && request.plugin_dependencies {
        plugin_closures(session, projects, request)?
    } else {
        HashMap::new()
    };

    // Toolchains: not downloadable, but checkable. A missing one fails the
    // build long after the sync reported success, with an error that does not
    // mention this file.
    for project in projects {
        let Ok(pom) = std::fs::read_to_string(&project.path) else {
            continue;
        };
        for requirement in toolchains::required_toolchains(&pom) {
            if request
                .toolchains
                .select(&requirement.kind, &requirement.requirements)
                .is_none()
            {
                report.warnings.push(format!(
                    "{}: requires a {} toolchain, and toolchains.xml provides none that matches; \
                     `mvn -o` will fail before it builds. jv cannot supply this — install the JDK \
                     and declare it in ~/.m2/toolchains.xml",
                    project.path.display(),
                    requirement.describe()
                ));
            }
        }
    }

    // `<build><extensions>` — build extensions declared by the project rather
    // than in `.mvn/extensions.xml`. Maven loads these before the build, so
    // `mvn -o` fails without them; `os-maven-plugin` is the common one and it
    // is how the gson corpus entry failed.
    for project in projects {
        for extension in project
            .model
            .build
            .iter()
            .flat_map(|build| &build.extensions)
        {
            let (Some(artifact_id), Some(version)) = (
                extension.artifact_id.as_deref(),
                extension.version.as_deref(),
            ) else {
                report.warnings.push(format!(
                    "{}: build extension {}:{} declares no version and was not synced",
                    project.path.display(),
                    extension.group_id.as_deref().unwrap_or("[unknown]"),
                    extension.artifact_id.as_deref().unwrap_or("[unknown]")
                ));
                continue;
            };
            let artifact = Artifact::new(
                extension.group_id.as_deref().unwrap_or_default(),
                artifact_id,
                version,
            );
            wanted.push(&reactor, artifact.clone());
            // Reported, not swallowed. Maven loads build extensions before the
            // build, so a missing dependency here fails `mvn -o` before it
            // reaches anything a dependency graph could explain — and the
            // `.mvn/extensions.xml` path below has always said so.
            match plugin_dependencies(
                session,
                &artifact,
                &Plugin::default(),
                &mut report.warnings,
                // Not memoised: these are resolved once per sync, not once per
                // plugin per module, so there is nothing here to save.
                &mut true,
            ) {
                Ok(dependencies) => {
                    for dependency in dependencies {
                        wanted.push(&reactor, dependency);
                    }
                }
                Err(error) => report.warnings.push(format!(
                    "{}: build extension {}: its dependencies could not be resolved ({error}); \
                     `mvn -o` may fail to start",
                    project.path.display(),
                    artifact_coordinates(&artifact)
                )),
            }
        }
    }

    // Core extensions from `.mvn/extensions.xml`. jv cannot run one — that
    // needs Maven's own container — but `mvn -o` refuses to start at all when
    // an extension is missing from the local repository, and that failure
    // arrives before anything a dependency graph could explain. Their own
    // transitive dependencies come along, because the container resolves those
    // too.
    for project in projects {
        let Some(directory) = crate::mvn_config::project_directory(&project.path) else {
            continue;
        };
        for extension in crate::mvn_config::extensions(&directory) {
            let artifact = Artifact::new(
                &extension.group_id,
                &extension.artifact_id,
                &extension.version,
            );
            wanted.push(&reactor, artifact.clone());
            match plugin_dependencies(
                session,
                &artifact,
                &Plugin::default(),
                &mut report.warnings,
                // Not memoised: these are resolved once per sync, not once per
                // plugin per module, so there is nothing here to save.
                &mut true,
            ) {
                Ok(dependencies) => {
                    for dependency in dependencies {
                        wanted.push(&reactor, dependency);
                    }
                }
                Err(error) => report.warnings.push(format!(
                    "core extension {}: its dependencies could not be resolved ({error}); \
                     `mvn -o` may fail to start",
                    extension.coordinates()
                )),
            }
        }
    }

    // Every module's dependency graph, resolved before the loop that orders
    // them.
    //
    // A reactor resolves one module after another, and with plugin closures now
    // remembered that is what a warm sync of a large one is: 706ms of
    // dropwizard's 774ms across 38 modules, where a single module costs 151ms.
    // The modules do not depend on each other's resolves — a dependency on a
    // sibling is answered from the reactor map, which is built before any of
    // this — so they are resolved together.
    //
    // Same shape as the plugin closures above, and the same reason for keeping
    // the loop below sequential: artifact order decides what
    // `_remote.repositories` and the snapshot metadata come out as.
    let resolutions = {
        use rayon::prelude::*;

        let resolved: Vec<Result<crate::session::Resolution, DriverError>> = projects
            .par_iter()
            .map(|project| session.resolve_project(project, Verbosity::None))
            .collect();
        // Unwrapped in project order rather than by `collect`ing into a
        // `Result`, so that a reactor with two broken modules always reports the
        // same one — whichever comes first in the build, not whichever thread
        // happened to finish first.
        let mut resolutions = Vec::with_capacity(resolved.len());
        for outcome in resolved {
            resolutions.push(outcome?);
        }
        resolutions
    };

    // Named on the command line rather than found in a POM. Pushed before the
    // projects so that a build which needs one has it regardless of which module
    // turns out to want it.
    for artifact in &request.also {
        wanted.push(&reactor, artifact.clone());
        let mut clean = true;
        for dependency in plugin_dependencies(
            session,
            artifact,
            &Plugin::default(),
            &mut report.warnings,
            &mut clean,
        )? {
            wanted.push(&reactor, dependency);
        }
    }

    for (project, resolution) in projects.iter().zip(&resolutions) {
        // Test scope, because a build runs tests: it is the widest composition
        // and anything narrower leaves `mvn -o test` unable to start.
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
            wanted.push(&reactor, artifact.clone());
        }

        if request.plugins {
            for (plugin, origin) in project_plugins(project) {
                // A plugin with no version is one whose version Maven resolves
                // from `maven-metadata.xml` at build time. jv used to warn and
                // sync nothing, on the grounds that picking one would be
                // guessing — but the result was that `mvn -o` could not resolve
                // it either, because the metadata it would have read was not
                // there. Reading that file settles the version *and* records it
                // for placement, so Maven reaches its own answer offline.
                let Some(artifact) = plugin_artifact(&plugin).or_else(|| {
                    versionless_plugin(session, &plugin, project, &mut report.warnings)
                }) else {
                    continue;
                };
                wanted.push(&reactor, artifact.clone());

                // A management-only entry gets its own jar and POM — cheap, and
                // enough for `mvn -o help:describe` or an explicit
                // `plugin:goal` invocation to start — but not its transitive
                // closure, which nothing in this build will load.
                let closure = match origin {
                    PluginOrigin::Declared => true,
                    PluginOrigin::Managed => request.managed_plugin_dependencies,
                };

                if request.plugin_dependencies && closure {
                    // Resolved above, in parallel. The warnings are replayed
                    // per declaration rather than per distinct closure, so a
                    // plugin forty modules declare still reports forty times —
                    // which is what it did when each of the forty resolved it.
                    if let Some(resolved) = closures.get(&closure_key(&artifact, &plugin)) {
                        report.warnings.extend(resolved.warnings.iter().cloned());
                        for dependency in &resolved.dependencies {
                            wanted.push(&reactor, dependency.clone());
                        }
                        // And whatever the plugin will pick for itself at run
                        // time, which is in no POM and which `go-offline` also
                        // misses.
                        for (extra, dependencies) in &resolved.extras {
                            wanted.push(&reactor, extra.clone());
                            for dependency in dependencies {
                                wanted.push(&reactor, dependency.clone());
                            }
                        }
                    }
                    selects_providers |= selects_providers_for(&artifact);
                    selects_reports |= selects_reports_for(&artifact);
                }
            }
        }
    }

    // Surefire aligns its JUnit Platform launcher to the platform version it
    // finds on the *test classpath*, not to anything the provider declares — so
    // the version can only be read off the graph, after it is collected.
    // `maven-site-plugin` resolves its default report at execution time, and
    // `maven-project-info-reports-plugin` appears in no POM anywhere — not in
    // `<plugins>`, not in `<pluginManagement>`, not in any parent. Same shape
    // as Surefire choosing a test provider, and it fails the same way: `mvn -o
    // site` stops with "Error resolving version for plugin".
    //
    // The version is not in lockstep with site-plugin's, so it has to be looked
    // up. Maven takes the newest release, and the lookup also records the
    // version metadata that Maven needs to make the same choice offline.
    if selects_reports {
        // The skin, every released version of it.
        //
        // `maven-site-plugin` resolves a skin at execution time and the version
        // is a constant inside its own Java — `maven-default-skin:1.3` for
        // 3.12.1 — named in no POM anywhere, so jv cannot derive it without
        // embedding a table of site-plugin internals that changes each release.
        //
        // There are four released versions and each is about 14 KB, so all of
        // them cost less than one ordinary jar. That is the same trade the
        // Surefire providers above make, and for the same reason: jv cannot
        // know which one the plugin will pick, and being wrong fails the build.
        match session
            .source()
            .published_versions("org.apache.maven.skins", "maven-default-skin")
        {
            Ok(versions) => {
                for version in versions
                    .iter()
                    .filter(|version| !is_snapshot_version(version))
                {
                    wanted.push(
                        &reactor,
                        Artifact::new("org.apache.maven.skins", "maven-default-skin", version),
                    );
                }
            }
            Err(error) => report.warnings.push(format!(
                "maven-site-plugin is in the build but its skin could not be resolved \
                 ({error}); `mvn -o site` will fail"
            )),
        }

        match default_site_report(session) {
            Ok(Some(reports_plugin)) => {
                wanted.push(&reactor, reports_plugin.clone());
                for dependency in plugin_dependencies(
                    session,
                    &reports_plugin,
                    &Plugin::default(),
                    &mut report.warnings,
                    &mut true,
                )? {
                    wanted.push(&reactor, dependency);
                }
            }
            Ok(None) => {}
            Err(error) => report.warnings.push(format!(
                "maven-site-plugin is in the build but its default report could not be \
                 resolved ({error}); `mvn -o site` will fail"
            )),
        }
    }

    if selects_providers {
        for launcher in aligned_launchers(&wanted.ordered) {
            wanted.push(&reactor, launcher.clone());
            for dependency in plugin_dependencies(
                session,
                &launcher,
                &Plugin::default(),
                &mut report.warnings,
                &mut true,
            )? {
                wanted.push(&reactor, dependency);
            }
        }
    }

    // Tracking files are per directory, and a directory holds every file of one
    // GAV, so they are collected as the artifacts are placed and written once at
    // the end. Writing per file would rewrite the same file three times.
    let mut tracking: BTreeMap<PathBuf, Tracking> = BTreeMap::new();
    // One per snapshot version directory, written once every file of that
    // version has been placed.
    let mut snapshots: BTreeMap<String, LocalSnapshot> = BTreeMap::new();
    // What has already been dealt with, so the POM sweep below does not redo it.
    let mut placed: BTreeSet<Artifact> = BTreeSet::new();

    // Every file to fetch, POMs included, gathered before anything is
    // downloaded so the whole set can go out concurrently. Fetching one at a
    // time made a cold sync pay one sequential round trip per artifact, which
    // was most of its wall clock.
    //
    // The POM travels with the jar: Maven reads it on every resolve, and a
    // local repository holding jars without POMs is one `mvn -o` rejects.
    let candidates: Vec<Artifact> = std::mem::take(&mut wanted.ordered)
        .into_iter()
        .flat_map(|artifact| {
            let pom = Artifact {
                classifier: String::new(),
                extension: "pom".to_owned(),
                ..artifact.clone()
            };
            [pom, artifact]
        })
        .collect();

    // An expression that survived interpolation must never become a request.
    //
    // `${project.prerequisites.maven}` in the Apache parent chain used to reach
    // this point intact; the path builder then neutralised the `${}` — correctly,
    // since a coordinate must not be able to escape the repository root — and jv
    // asked a repository for `…/__project.prerequisites.maven_/…`. The failure
    // that surfaced was a network error about a nonsense URL, which says nothing
    // about the actual problem. Reporting the expression is both more useful and
    // strictly safer.
    let (candidates, unresolved): (Vec<Artifact>, Vec<Artifact>) = candidates
        .into_iter()
        .partition(|artifact| !has_unresolved_expression(artifact));
    for artifact in unresolved {
        report.warnings.push(format!(
            "{}:{}:{} still contains an unresolved expression and was not fetched; \
             the property it names is not defined anywhere jv could see",
            artifact.group_id, artifact.artifact_id, artifact.version
        ));
    }

    let found_all = session.source().materialize_all(&candidates)?;

    // Placement runs in three passes so the file work can go wide without
    // disturbing anything that depends on order.
    //
    // The bookkeeping — the tracking file, the snapshot metadata, the report —
    // is built in artifact order and stays sequential. Only the filesystem
    // work in the middle is parallel, and it is the part worth parallelising:
    // on a warm cache a sync is roughly half resolution and half placing a few
    // thousand hardlinks, and the hardlinks are independent of each other.
    //
    // Pass one: decide every destination, and do the snapshot bookkeeping.
    let mut placements: Vec<(usize, PathBuf, String, Option<String>)> = Vec::new();
    for (index, (candidate, found)) in candidates.into_iter().zip(found_all).enumerate() {
        {
            match found {
                Some(found) => {
                    if let Some(_local) = &request.local_repository {
                        // The *resolved* path: a snapshot lives in its
                        // `-SNAPSHOT` directory under a timestamped file name,
                        // and placing it under the base name would give Maven a
                        // file it never looks for.
                        let relative = session.source().repository_path(&candidate)?;
                        // A snapshot is installed rather than cached: base-version
                        // file name, and a `maven-metadata-local.xml` written
                        // below once every file of that version is placed.
                        let relative = if is_snapshot_version(&candidate.version) {
                            let base = Artifact {
                                version: candidate.base_version(),
                                ..candidate.clone()
                            };
                            let placed = jv_repo::artifact_path(&base);
                            // The version directory, which is the path without
                            // the file name — one metadata file serves every
                            // file of one snapshot version.
                            let directory = placed
                                .rsplit_once('/')
                                .map(|(head, _)| head.to_owned())
                                .unwrap_or_default();
                            snapshots
                                .entry(directory)
                                .or_insert_with(|| LocalSnapshot::new(&base))
                                .record(&base);
                            placed
                        } else {
                            relative
                        };
                        placements.push((
                            index,
                            found.path.clone(),
                            relative,
                            found.repository.clone(),
                        ));
                    }
                    placed.insert(candidate.clone());
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

    // Every POM jv read on the way here, which is more than the POMs of the
    // artifacts themselves: Maven walks each one's parents and imported BOMs
    // when it re-reads them, and a missing grandparent POM fails a build whose
    // jars are all present. jv already fetched them; they just have to travel.
    // Pass two: place the files, in parallel. Each hardlink is independent of
    // every other, and `materialize` refuses to overwrite anything, so the only
    // interaction between two placements is the directory they share — which
    // `create_dir_all` already handles concurrently.
    if let Some(local) = &request.local_repository {
        let linked = place_all(local, &placements);

        // Pass three: bookkeeping, back in artifact order. `_remote.repositories`
        // and the snapshot metadata both depend on the order artifacts are seen
        // in, so this half is deliberately not parallel.
        for (placement, outcome) in placements.iter().zip(linked) {
            match outcome {
                Ok(Some(destination)) => {
                    record_tracking(&mut tracking, &destination, placement.3.as_deref())?;
                    report.materialized.push(destination);
                }
                Ok(None) => {}
                Err(error) => return Err(error),
            }
        }
    }

    // Concurrently, for the same reason as the artifacts above: this set is
    // every POM jv read, which on a deep parent chain is larger than the
    // artifact set and was being walked one blocking request at a time.
    let remaining: Vec<Artifact> = session
        .source()
        .read_poms()
        .into_iter()
        .filter(|pom| !placed.contains(pom))
        .collect();
    let found_poms = session.source().materialize_all(&remaining)?;

    for (pom, found) in remaining.into_iter().zip(found_poms) {
        let Some(found) = found else {
            continue;
        };
        if let Some(local) = &request.local_repository {
            let relative = session.source().repository_path(&pom)?;
            if let Some(linked) = materialize(&found.path, local, &relative, &mut report)? {
                record_tracking(&mut tracking, &linked, found.repository.as_deref())?;
                report.materialized.push(linked);
            }
        }
        report.artifacts.push(pom);
    }

    // Every range in every POM jv read, not only the ones jv resolved: Maven
    // re-resolves the plugin classpath on its own terms and reaches ranges down
    // paths jv never took.
    session.source().fetch_ranged_metadata();

    // The version-list metadata behind every range and `LATEST`.
    //
    // Maven re-resolves a range at build time, so a repository holding the jar
    // a range picked but not the metadata behind it fails offline with "No
    // versions available … within specified range", naming neither the file nor
    // the reason. `git-commit-id-maven-plugin` reaching bouncycastle through
    // `[1.81,1.82)` is how this surfaced.
    //
    // The name carries the *effective* repository id, which is the mirror's
    // when the user has one — the same id `_remote.repositories` records. jv
    // writes the id it actually fetched from: right whenever `mvn` runs with
    // the settings `jv sync` ran with, and inert rather than harmful when it
    // does not, since Maven simply ignores a file it is not looking for.
    if let Some(local) = &request.local_repository {
        for (path, repository_id, bytes) in session.source().read_range_metadata() {
            let Some((directory, _)) = path.rsplit_once('/') else {
                continue;
            };
            let destination = local
                .join(directory)
                .join(format!("maven-metadata-{repository_id}.xml"));
            if destination.exists() {
                // Maven's own file, if it is there. Never overwritten: this
                // directory belongs to Maven.
                continue;
            }
            if let Some(parent) = destination.parent()
                && let Err(error) = std::fs::create_dir_all(parent)
            {
                report.warnings.push(format!(
                    "cannot create {}: {error}; a version range may not resolve offline",
                    parent.display()
                ));
                continue;
            }
            if let Err(error) = std::fs::write(&destination, &bytes) {
                report.warnings.push(format!(
                    "cannot write {}: {error}; a version range may not resolve offline",
                    destination.display()
                ));
            } else {
                report.materialized.push(destination);
            }
        }
    }

    if let Some(local) = &request.local_repository {
        for (directory, snapshot) in &snapshots {
            if snapshot.is_empty() {
                continue;
            }
            if let Err(error) = snapshot.write(&local.join(directory)) {
                report
                    .warnings
                    .push(format!("cannot write the snapshot metadata: {error}"));
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
    // A snapshot warns once per file it placed, and a repository problem warns
    // once per artifact that hit it. Saying the same sentence forty times buries
    // everything else in the report.
    let mut seen = BTreeSet::new();
    report
        .warnings
        .retain(|warning| seen.insert(warning.clone()));
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

/// The queue of artifacts to fetch: a list for order, a set for membership.
///
/// Order matters — it is the order things are downloaded and reported in — but
/// `Vec::contains` on a struct of five `String`s is a full comparison per entry,
/// and a real build queues a few thousand. The set makes the membership test
/// constant.
#[derive(Debug, Default)]
struct Wanted {
    ordered: Vec<Artifact>,
    seen: HashSet<Artifact>,
}

impl Wanted {
    /// Adds an artifact once, skipping anything the reactor produces.
    ///
    /// Matched on the *version* too. A different version of a module the
    /// reactor builds is an ordinary published artifact that has to be
    /// downloaded like any other, and skipping it leaves a repository that
    /// cannot build: `jackson-base` declares `nexus-staging-maven-plugin` as a
    /// build extension, whose closure needs `jackson-core:2.13.2` — while the
    /// reactor is building `jackson-core:2.17.1`. Maven loads extensions before
    /// the build, so the whole thing failed to start, naming an artifact that
    /// looked like the project's own.
    ///
    /// The same shape appears whenever a build compares itself against its
    /// previous release, which japicmp and animal-sniffer both do.
    fn push(&mut self, reactor: &BTreeSet<String>, artifact: Artifact) {
        if reactor.contains(&format!(
            "{}:{}:{}",
            artifact.group_id, artifact.artifact_id, artifact.version
        )) {
            return;
        }
        if self.seen.insert(artifact.clone()) {
            self.ordered.push(artifact);
        }
    }
}

/// Places every file, in parallel.
///
/// Returns one outcome per placement, in the order given, so the caller's
/// ordered bookkeeping is unaffected. Warnings are dropped here rather than
/// threaded through: the only one `materialize` raises is the path-escape
/// refusal, which cannot fire on a path `artifact_path` built, and which the
/// caller re-checks anyway.
fn place_all(
    local_repository: &Path,
    placements: &[(usize, PathBuf, String, Option<String>)],
) -> Vec<Result<Option<PathBuf>, DriverError>> {
    use rayon::prelude::*;
    placements
        .par_iter()
        .map(|(_, cached, relative, _)| {
            let mut discard = SyncReport::default();
            materialize(cached, local_repository, relative, &mut discard)
        })
        .collect()
}

/// `group:artifact:version`, for a diagnostic.
fn artifact_coordinates(artifact: &Artifact) -> String {
    format!(
        "{}:{}:{}",
        artifact.group_id, artifact.artifact_id, artifact.version
    )
}

/// Whether any coordinate field still holds a `${...}`.
///
/// Cheap, and it runs once per artifact rather than per request.
fn has_unresolved_expression(artifact: &Artifact) -> bool {
    [
        &artifact.group_id,
        &artifact.artifact_id,
        &artifact.version,
        &artifact.classifier,
        &artifact.extension,
    ]
    .iter()
    .any(|field| field.contains("${"))
}

/// The plugins a project uses, in the order `ResolverUtil.getProjectPlugins`
/// gathers them: reporting, then build, then pluginManagement.
///
/// Order decides which declaration of a duplicated plugin supplies the version,
/// so it is not cosmetic.
/// Where a plugin came from, which decides how much of it `jv sync` fetches.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PluginOrigin {
    /// `<build><plugins>`, or bound by the packaging's lifecycle — which the
    /// model builder has already merged into the same list. Something in the
    /// build will run this, so its whole dependency closure is needed.
    Declared,
    /// `<pluginManagement>` only. Management supplies a version and
    /// configuration to plugins that are *declared*; an entry nothing declares
    /// never enters a build plan, so its transitive dependencies are never
    /// loaded.
    Managed,
}

/// The plugins a project uses, each with where it came from.
///
/// Upstream's `ResolverUtil.getProjectPlugins` takes reporting, build and
/// pluginManagement and makes no distinction between them. jv keeps the same
/// set but records the origin, because the closure of a management-only entry
/// is dead weight: on spring-petclinic those entries brought a 58 MB Kotlin
/// compiler, three versions of zstd-jni, jOOQ, Liquibase and Saxon into a
/// single-module Java project — 245 MB of the 345 MB synced, none of which
/// `mvn -o verify` ever opened.
fn project_plugins(project: &Project) -> Vec<(Plugin, PluginOrigin)> {
    let mut plugins: Vec<(Plugin, PluginOrigin)> = Vec::new();
    let mut push_unique = |plugin: &Plugin, origin: PluginOrigin| {
        let key = (
            plugin.group_id_or_default().to_owned(),
            plugin.artifact_id.clone(),
        );
        let already = plugins.iter().any(|(held, _): &(Plugin, PluginOrigin)| {
            (
                held.group_id_or_default().to_owned(),
                held.artifact_id.clone(),
            ) == key
        });
        if !already {
            plugins.push((plugin.clone(), origin));
        }
    };

    // Declared first, so a plugin that is both declared and managed is treated
    // as declared.
    if let Some(build) = &project.model.build {
        for plugin in &build.plugins {
            push_unique(plugin, PluginOrigin::Declared);
        }
        for plugin in &build.plugin_management {
            push_unique(plugin, PluginOrigin::Managed);
        }
    }

    // `<reporting>` last, and only for plugins nothing else declared.
    //
    // These were tried first, on the reading that `ResolverUtil` lists them
    // first. That broke commons-io's offline build: `push_unique` keeps the
    // first declaration of a coordinate, a reporting entry usually carries no
    // version and no `<extensions>`, and the build entry carries both — so the
    // build's `maven-site-plugin` was displaced by the report's, its closure
    // came out different, and `wagon-ssh` was never fetched. `mvn -o` then
    // failed in `attach-descriptor` on a build that had worked the day before.
    //
    // Order here decides only which declaration wins a tie, and the one the
    // build uses has to win. A report plugin nothing else mentions is still
    // added, which is the point of reading `<reporting>` at all.
    for plugin in &project.model.reporting_plugins {
        push_unique(plugin, PluginOrigin::Declared);
    }
    plugins
}

/// Surefire's providers, which are versioned in lockstep with the plugin.
///
/// Surefire picks one of these at execution time by looking at what is on the
/// test classpath, so none of them appears in any POM and no amount of static
/// analysis finds them. Fetching all of them costs a few hundred kilobytes and
/// is what lets `mvn -o test` run whichever framework the project actually uses.
///
/// `common-junit48` and `common-java5` are not providers but are what the JUnit
/// providers depend on at the same version; they are listed because a provider's
/// own POM is resolved through the same path and a missing one fails the same
/// way.
const SUREFIRE_PROVIDERS: &[&str] = &[
    "surefire-junit-platform",
    "surefire-junit47",
    "surefire-junit4",
    "surefire-junit3",
    "surefire-testng",
    "surefire-testng-utils",
    "common-junit48",
    "common-java5",
];

/// Whether a plugin picks a test provider at execution time.
///
/// Surefire and failsafe are the two in the default lifecycle that do.
fn selects_providers_for(plugin: &Artifact) -> bool {
    plugin.group_id == "org.apache.maven.plugins"
        && matches!(
            plugin.artifact_id.as_str(),
            "maven-surefire-plugin" | "maven-failsafe-plugin"
        )
}

/// Whether a plugin picks its reports at execution time.
fn selects_reports_for(plugin: &Artifact) -> bool {
    plugin.group_id == "org.apache.maven.plugins" && plugin.artifact_id == "maven-site-plugin"
}

/// The report `maven-site-plugin` runs when a project configures none.
///
/// Its version comes from metadata rather than from site-plugin, so this asks
/// for the newest release the same way Maven does. The lookup also records that
/// metadata for `jv sync` to place, which is what lets Maven repeat the choice
/// with the network off.
fn default_site_report(session: &Session) -> Result<Option<Artifact>, DriverError> {
    const GROUP: &str = "org.apache.maven.plugins";
    const ARTIFACT: &str = "maven-project-info-reports-plugin";

    let versions = session
        .source()
        .published_versions(GROUP, ARTIFACT)
        .map_err(DriverError::Other)?;
    let newest = versions
        .iter()
        .filter(|version| !jv_model::is_snapshot_version(version))
        .max_by(|left, right| {
            jv_version::Version::parse(left).cmp(&jv_version::Version::parse(right))
        });
    Ok(newest.map(|version| Artifact::new(GROUP, ARTIFACT, version)))
}

/// Artifacts a plugin will resolve for itself at execution time.
fn runtime_selected(plugin: &Artifact) -> Vec<Artifact> {
    if !selects_providers_for(plugin) {
        return Vec::new();
    }
    SUREFIRE_PROVIDERS
        .iter()
        .map(|provider| Artifact::new("org.apache.maven.surefire", *provider, &plugin.version))
        .collect()
}

/// The JUnit Platform launcher, at the platform version already in the graph.
///
/// Surefire's JUnit Platform provider does not depend on the launcher at a fixed
/// version: it reads the platform version off the test classpath and resolves a
/// matching launcher, so that a project pinning JUnit 5.10 is not run by a
/// launcher built for 5.12. The version is therefore knowable only from the
/// resolved graph, which is why this runs after collection rather than beside
/// the plugin that needs it.
fn aligned_launchers(wanted: &[Artifact]) -> Vec<Artifact> {
    let mut launchers: Vec<Artifact> = Vec::new();
    for artifact in wanted {
        if artifact.group_id != "org.junit.platform" {
            continue;
        }
        if !matches!(
            artifact.artifact_id.as_str(),
            "junit-platform-commons" | "junit-platform-engine"
        ) {
            continue;
        }
        let launcher = Artifact::new(
            "org.junit.platform",
            "junit-platform-launcher",
            &artifact.version,
        );
        if !launchers.contains(&launcher) {
            launchers.push(launcher);
        }
    }
    launchers
}

/// A plugin's own artifact, if it states a version.
fn plugin_artifact(plugin: &Plugin) -> Option<Artifact> {
    Some(Artifact::new(
        plugin.group_id_or_default(),
        plugin.artifact_id.as_deref()?,
        plugin.version.as_deref()?,
    ))
}

/// Resolves a `<plugin>` that declares no version, the way Maven would.
///
/// Returns `None` only when the metadata cannot be read or lists nothing, and
/// says so — the failure is worth a warning either way, because `mvn -o` will
/// stop on this plugin with "Error resolving version for plugin", an error that
/// names neither the metadata file nor why it is absent.
fn versionless_plugin(
    session: &Session,
    plugin: &Plugin,
    project: &Project,
    warnings: &mut Vec<String>,
) -> Option<Artifact> {
    let group_id = plugin.group_id_or_default();
    let artifact_id = plugin.artifact_id.as_deref()?;
    match session.source().plugin_version(group_id, artifact_id) {
        Ok(Some(version)) => Some(Artifact::new(group_id, artifact_id, version)),
        Ok(None) => {
            warnings.push(format!(
                "{}: plugin {group_id}:{artifact_id} declares no version and its metadata \
                 lists none, so it was not synced; `mvn -o` will fail when it runs this plugin",
                project.path.display()
            ));
            None
        }
        Err(error) => {
            warnings.push(format!(
                "{}: plugin {group_id}:{artifact_id} declares no version and its metadata \
                 could not be read ({error}), so it was not synced; `mvn -o` will fail when \
                 it runs this plugin",
                project.path.display()
            ));
            None
        }
    }
}

/// The artifacts a plugin's `<configuration>` named, as coordinates to fetch.
///
/// A version is required to fetch anything, and configuration is the one place
/// a coordinate can appear without management behind it to supply one. Maven
/// would resolve such an entry from metadata when the plugin runs; jv says so
/// rather than guessing, for the same reason it does with a versionless plugin.
fn configuration_artifacts(plugin: &Plugin, warnings: &mut Vec<String>) -> Vec<Artifact> {
    let mut found = Vec::new();
    for dependency in &plugin.configuration_artifacts {
        let Some(version) = dependency.version.as_deref().filter(|v| !v.is_empty()) else {
            warnings.push(format!(
                "{}:{} configures {}:{} with no version, so it was not synced; `mvn -o` will \
                 fail when it runs this plugin",
                plugin.group_id_or_default(),
                plugin.artifact_id.as_deref().unwrap_or("[unknown]"),
                dependency.group_id,
                dependency.artifact_id
            ));
            continue;
        };
        let artifact = Artifact {
            group_id: dependency.group_id.clone(),
            artifact_id: dependency.artifact_id.clone(),
            version: version.to_owned(),
            classifier: dependency.classifier.clone().unwrap_or_default(),
            extension: dependency
                .type_
                .clone()
                .filter(|extension| !extension.is_empty())
                .unwrap_or_else(|| "jar".to_owned()),
        };
        // A property no profile defined. Fetching `${foo}` literally would ask
        // a repository for a path that cannot exist and then report it as a
        // missing artifact, which reads as jv's fault rather than the POM's.
        if has_unresolved_expression(&artifact) {
            warnings.push(format!(
                "{}:{} configures {}:{} at version {version}, which is an unresolved \
                 expression, so it was not synced",
                plugin.group_id_or_default(),
                plugin.artifact_id.as_deref().unwrap_or("[unknown]"),
                dependency.group_id,
                dependency.artifact_id
            ));
            continue;
        }
        found.push(artifact);
    }
    found
}

/// A plugin's resolved dependency closure, and what resolving it had to say.
#[derive(Debug, Default)]
struct PluginClosure {
    /// The plugin's own transitive dependencies, compile and runtime scope.
    dependencies: Vec<Artifact>,
    /// What the plugin selects for itself at run time, each with its own
    /// closure — Surefire's providers, which appear in no POM.
    extras: Vec<(Artifact, Vec<Artifact>)>,
    /// Replayed once per declaration by the caller, not once per closure.
    warnings: Vec<String>,
}

/// What makes two plugin closures the same piece of work.
///
/// The coordinates alone are not enough: `<plugin><dependencies>` adds to or
/// replaces what the plugin declares, which is how a project pins a JDBC driver
/// or a compiler, so two plugins at the same version can resolve differently.
///
/// # Why this is not a cache key
///
/// Closures were briefly remembered between runs on disk, keyed by this, which
/// took a warm sync from 213ms to 99ms. It also silently dropped 40% of the
/// repository: commons-io placed 6,060 artifacts on the run that wrote the memo
/// and 3,664 on the run that read it.
///
/// Resolving a closure is not a pure computation whose answer is the only thing
/// that matters. Reading a POM is what puts it in the set `read_poms` returns,
/// and that set is what gets placed — so the parent chains of everything in a
/// plugin's closure land in the local repository only because somebody parsed
/// them. Skip the resolve and the artifacts still arrive, because the memo names
/// them; their POMs do not, because nothing read them, and `mvn -o` then fails
/// on a POM it cannot find.
///
/// A memo could be made correct by recording the POMs too, but per-closure
/// attribution is the hard part: closures resolve in parallel into one shared
/// set, so a before-and-after diff belongs to whichever ones happened to
/// overlap. Deduplicating within a run is safe and is kept — a plugin forty
/// modules declare is resolved once, and that once still reads its POMs.
type ClosureKey = (String, String, String, String);

fn closure_key(artifact: &Artifact, plugin: &Plugin) -> ClosureKey {
    (
        artifact.group_id.clone(),
        artifact.artifact_id.clone(),
        artifact.version.clone(),
        // A structural fingerprint of both blocks that can change what these
        // coordinates resolve to, which is cheaper to write than a hand-rolled
        // hash and does not have to be stable across versions: nothing outside
        // this run reads it. `configuration_artifacts` belongs here for the same
        // reason `dependencies` does — two declarations of one plugin version
        // that configure different annotation processors are different
        // questions, and keying on coordinates alone would hand the second the
        // first one's answer.
        format!(
            "{:?}|{:?}",
            plugin.dependencies, plugin.configuration_artifacts
        ),
    )
}

/// Resolves every distinct plugin closure in the build, in parallel.
///
/// Two savings, and the second is the larger one on a real project. Closures no
/// longer run one after another, and a plugin that many modules declare the same
/// way is resolved once instead of once per module — a fifty-module reactor
/// declares much the same dozen plugins fifty times over.
fn plugin_closures(
    session: &Session,
    projects: &[&Project],
    request: &SyncRequest,
) -> Result<HashMap<ClosureKey, PluginClosure>, DriverError> {
    use rayon::prelude::*;

    // Distinct work only, gathered in declaration order so that a run is
    // reproducible even though the resolves themselves finish in any order.
    let mut work: Vec<(ClosureKey, Artifact, Plugin)> = Vec::new();
    let mut seen: HashSet<ClosureKey> = HashSet::new();
    for project in projects {
        for (plugin, origin) in project_plugins(project) {
            let closure = match origin {
                PluginOrigin::Declared => true,
                PluginOrigin::Managed => request.managed_plugin_dependencies,
            };
            let Some(artifact) = plugin_artifact(&plugin).filter(|_| closure) else {
                continue;
            };
            let key = closure_key(&artifact, &plugin);
            if seen.insert(key.clone()) {
                work.push((key, artifact, plugin));
            }
        }
    }

    work.into_par_iter()
        .map(|(key, artifact, plugin)| {
            let mut warnings = Vec::new();
            let mut clean = true;
            let dependencies =
                plugin_dependencies(session, &artifact, &plugin, &mut warnings, &mut clean)?;
            let mut extras = Vec::new();
            // What the plugin picks for itself at run time, and what its
            // `<configuration>` names. Both are artifacts no `<dependencies>`
            // block mentions, and both are resolved with their own closure —
            // an annotation processor drags its dependencies onto the compiler's
            // path exactly as a dependency would.
            for extra in runtime_selected(&artifact)
                .into_iter()
                .chain(configuration_artifacts(&plugin, &mut warnings))
            {
                let dependencies = plugin_dependencies(
                    session,
                    &extra,
                    &Plugin::default(),
                    &mut warnings,
                    &mut clean,
                )?;
                extras.push((extra, dependencies));
            }

            // Deliberately not remembered between runs. See the note on
            // `ClosureKey`.
            let _ = clean;
            Ok((
                key,
                PluginClosure {
                    dependencies,
                    extras,
                    warnings,
                },
            ))
        })
        .collect()
}

/// Everything a plugin needs to run: its own dependency tree, plus any
/// `<dependencies>` the POM added to it.
///
/// Resolved at runtime scope, which is what a plugin classloader gets.
fn plugin_dependencies(
    session: &Session,
    artifact: &Artifact,
    plugin: &Plugin,
    warnings: &mut Vec<String>,
    clean: &mut bool,
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

    // A plugin whose dependencies cannot be resolved is not a reason to fail
    // the whole sync — the plugin's own jar is already queued, and the rest of
    // the build may never invoke it. It *is* a reason to say so.
    //
    // This used to swallow the error and return nothing, on the theory that
    // Maven would report the real problem later. Maven reports a different
    // problem: a list of artifacts missing from the local repository, with no
    // hint that jv failed to resolve them or why. The sync said "synced 3800
    // artifacts" and looked like a success.
    let resolution = match session.resolve_request(&request, Verbosity::None) {
        Ok(resolution) => resolution,
        Err(error) => {
            warnings.push(format!(
                "{}:{}:{}: its dependencies could not be resolved ({error}), so none were \
                 synced; `mvn -o` will fail when it runs this plugin",
                artifact.group_id, artifact.artifact_id, artifact.version
            ));
            *clean = false;
            return Ok(Vec::new());
        }
    };

    // A graph with an unreadable descriptor in it is missing whatever that POM
    // would have contributed. Maven tolerates it and so does jv, but it is not
    // an answer to write down: remembering it would repeat the gap on every
    // later run, and silently, because the run that reads the memo does no
    // resolving and so has nothing to warn about.
    if !resolution.collected.missing_descriptors.is_empty() {
        *clean = false;
    }

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
    relative: &str,
    report: &mut SyncReport,
) -> Result<Option<PathBuf>, DriverError> {
    let destination = local_repository.join(relative);

    // `artifact_path` already sanitizes the coordinates, so this cannot trigger
    // on any path jv builds. It is here because the consequence of being wrong is
    // writing attacker-controlled bytes to an attacker-chosen location, and a
    // check that costs one string comparison is worth having between that and a
    // future caller who passes `relative` in from somewhere new.
    if !destination.starts_with(local_repository) {
        report.warnings.push(format!(
            "refusing to write {} outside {}",
            destination.display(),
            local_repository.display()
        ));
        return Ok(None);
    }

    // `symlink_metadata`, not `exists`: `exists` follows the link, so a dangling
    // symlink planted at the destination reads as absent and the copy below then
    // creates and fills whatever it points at.
    if destination.symlink_metadata().is_ok() {
        return Ok(None);
    }
    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent).map_err(|source| DriverError::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    }

    // A hardlink costs no disk, which matters because a CI cache pays for the
    // local repository and jv's cache both. It fails across filesystems and on
    // filesystems without hardlinks, so a copy is the fallback rather than an
    // error.
    //
    // Not when the source is already in a local repository, though: `Fetcher`
    // serves `~/.m2` hits straight from that directory, and linking one of those
    // into jv's output would put Maven's file on the other end of the link.
    // Anything that later rewrote jv's copy in place — jar signing, repackaging,
    // an `mvn install` — would silently rewrite the developer's `~/.m2` too. jv
    // promises to read that directory and never write to it.
    let linkable = !cached.starts_with(local_repository) && is_inside_a_cache(cached);
    if linkable && std::fs::hard_link(cached, &destination).is_ok() {
        return Ok(Some(destination));
    }
    match copy_new(cached, &destination) {
        Ok(()) => Ok(Some(destination)),
        // Somebody else placed it between the check above and the create. That
        // is another `jv sync` — or a Maven — winning a race jv does not need to
        // win, so it is a success with nothing to report. Treating it as a
        // failure produced a warning per artifact whenever two syncs shared a
        // local repository, which is the CI case this command exists for.
        Err(source) if source.kind() == std::io::ErrorKind::AlreadyExists => Ok(None),
        Err(source) => {
            // One unwritable file should not abort a sync of a thousand.
            report
                .warnings
                .push(format!("cannot place {}: {source}", destination.display()));
            Ok(None)
        }
    }
}

/// Whether a path looks like it is inside jv's own cache rather than a directory
/// another tool owns.
///
/// jv's cache is URL-keyed, so every entry sits under a scheme directory —
/// `https/`, `file/` — which a Maven layout never produces because a group id
/// cannot be a bare scheme name followed by a host.
fn is_inside_a_cache(path: &Path) -> bool {
    path.components().any(|component| {
        matches!(
            component.as_os_str().to_str(),
            Some("https" | "http" | "file")
        )
    })
}

/// Copies to a path that must not already exist, without following a symlink
/// there.
///
/// `fs::copy` opens the destination with `O_CREAT|O_TRUNC` and follows symlinks,
/// which is what let a planted link redirect the write. `create_new` refuses to
/// open anything that exists, link or file.
fn copy_new(source: &Path, destination: &Path) -> std::io::Result<()> {
    let mut reader = std::fs::File::open(source)?;
    let mut writer = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)?;
    std::io::copy(&mut reader, &mut writer)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use jv_model::{Build, Model};
    use jv_repo::artifact_path;

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
    fn configuration_coordinates_become_artifacts() {
        let mut compiler = plugin(None, "maven-compiler-plugin", Some("3.13.0"));
        compiler.configuration_artifacts = vec![Dependency {
            group_id: "com.google.errorprone".to_owned(),
            artifact_id: "error_prone_core".to_owned(),
            version: Some("2.25.0".to_owned()),
            ..Dependency::default()
        }];
        let mut warnings = Vec::new();
        let found = configuration_artifacts(&compiler, &mut warnings);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].artifact_id, "error_prone_core");
        assert_eq!(found[0].extension, "jar");
        assert!(warnings.is_empty());
    }

    #[test]
    fn a_configured_coordinate_without_a_version_is_reported_not_guessed() {
        // `maven-site-plugin`'s `<reportPlugins>` names plugins with no version
        // at all. Downloading nothing and saying so beats inventing one.
        let mut site = plugin(None, "maven-site-plugin", Some("3.12.1"));
        site.configuration_artifacts = vec![Dependency {
            group_id: "org.apache.maven.plugins".to_owned(),
            artifact_id: "maven-javadoc-plugin".to_owned(),
            version: None,
            ..Dependency::default()
        }];
        let mut warnings = Vec::new();
        assert!(configuration_artifacts(&site, &mut warnings).is_empty());
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("no version"), "{}", warnings[0]);
    }

    #[test]
    fn a_property_no_profile_defined_is_not_fetched_literally() {
        // Asking a repository for `${foo}` produces a path that cannot exist,
        // and then a missing-artifact report that reads as jv's fault.
        let mut compiler = plugin(None, "maven-compiler-plugin", Some("3.13.0"));
        compiler.configuration_artifacts = vec![Dependency {
            group_id: "com.example".to_owned(),
            artifact_id: "thing".to_owned(),
            version: Some("${undefined.version}".to_owned()),
            ..Dependency::default()
        }];
        let mut warnings = Vec::new();
        assert!(configuration_artifacts(&compiler, &mut warnings).is_empty());
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("unresolved"), "{}", warnings[0]);
    }

    #[test]
    fn a_configured_type_becomes_the_extension() {
        let mut plugin = plugin(None, "some-plugin", Some("1.0"));
        plugin.configuration_artifacts = vec![Dependency {
            group_id: "com.example".to_owned(),
            artifact_id: "thing".to_owned(),
            version: Some("1.0".to_owned()),
            type_: Some("zip".to_owned()),
            classifier: Some("resources".to_owned()),
            ..Dependency::default()
        }];
        let found = configuration_artifacts(&plugin, &mut Vec::new());
        assert_eq!(found[0].extension, "zip");
        assert_eq!(found[0].classifier, "resources");
    }

    #[test]
    fn one_closure_per_distinct_plugin_declaration() {
        // The saving: forty modules declaring the same plugin the same way is
        // one closure, not forty.
        let same = plugin(None, "maven-compiler-plugin", Some("3.13.0"));
        let artifact = plugin_artifact(&same).expect("a versioned plugin");
        assert_eq!(closure_key(&artifact, &same), closure_key(&artifact, &same));
    }

    #[test]
    fn a_plugins_own_dependencies_make_it_a_different_closure() {
        // The trap the key exists to avoid. `<plugin><dependencies>` is how a
        // project pins a JDBC driver or a compiler under an otherwise identical
        // plugin, so two declarations at the same version can resolve to
        // different closures. Keying on coordinates alone would hand the second
        // module the first one's answer.
        let plain = plugin(None, "maven-compiler-plugin", Some("3.13.0"));
        let mut pinned = plain.clone();
        pinned.dependencies = vec![Dependency {
            group_id: "org.postgresql".to_owned(),
            artifact_id: "postgresql".to_owned(),
            version: Some("42.7.3".to_owned()),
            ..Dependency::default()
        }];
        let artifact = plugin_artifact(&plain).expect("a versioned plugin");
        assert_ne!(
            closure_key(&artifact, &plain),
            closure_key(&artifact, &pinned)
        );
    }

    #[test]
    fn reporting_plugins_are_synced_and_count_as_declared() {
        // `mvn site` runs these, so a repository without them and their
        // closures cannot build a site offline.
        let mut project = project_with(Build {
            plugins: vec![plugin(None, "maven-compiler-plugin", Some("3.13.0"))],
            ..Build::default()
        });
        project.model.reporting_plugins = vec![plugin(None, "maven-javadoc-plugin", Some("3.6.3"))];

        let plugins = project_plugins(&project);
        let javadoc = plugins
            .iter()
            .find(|(held, _)| held.artifact_id.as_deref() == Some("maven-javadoc-plugin"))
            .expect("the report plugin");
        assert_eq!(javadoc.1, PluginOrigin::Declared);
    }

    #[test]
    fn a_build_declaration_beats_a_report_declaration_of_the_same_plugin() {
        // The regression this ordering exists to prevent. A report entry
        // usually carries no version and no `<extensions>`; the build entry
        // carries both, and its closure is the one the build needs. Letting the
        // report win cost commons-io its offline build.
        let mut project = project_with(Build {
            plugins: vec![plugin(None, "maven-site-plugin", Some("3.12.1"))],
            ..Build::default()
        });
        project.model.reporting_plugins = vec![plugin(None, "maven-site-plugin", None)];

        let plugins = project_plugins(&project);
        assert_eq!(plugins.len(), 1);
        assert_eq!(plugins[0].0.version.as_deref(), Some("3.12.1"));
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
        assert_eq!(plugins[0].0.version.as_deref(), Some("3.13.0"));
        assert_eq!(
            plugins[1].0.artifact_id.as_deref(),
            Some("maven-surefire-plugin")
        );
    }

    #[test]
    fn a_declared_plugin_stays_declared_even_when_also_managed() {
        // The origin decides whether the dependency closure is fetched, so a
        // plugin that appears in both lists must not be demoted by the managed
        // entry that follows it.
        let project = project_with(Build {
            plugins: vec![plugin(None, "maven-compiler-plugin", Some("3.13.0"))],
            plugin_management: vec![
                plugin(None, "maven-compiler-plugin", Some("3.0.0")),
                plugin(None, "kotlin-maven-plugin", Some("2.0.0")),
            ],
            ..Build::default()
        });
        let plugins = project_plugins(&project);
        assert_eq!(plugins[0].1, PluginOrigin::Declared);
        assert_eq!(
            plugins[1].1,
            PluginOrigin::Managed,
            "a plugin nothing declares is management only, and its closure is dead weight"
        );
    }

    #[test]
    fn lifecycle_plugins_count_as_declared() {
        // `inject_lifecycle_bindings` merges them into `build.plugins`, which is
        // what keeps `mvn -o deploy` working when the default skips managed
        // closures: deploy is bound by the packaging, not by management.
        let project = project_with(Build {
            plugins: vec![plugin(None, "maven-deploy-plugin", Some("3.1.2"))],
            ..Build::default()
        });
        assert_eq!(project_plugins(&project)[0].1, PluginOrigin::Declared);
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
        let reactor: BTreeSet<String> = ["com.example:lib:1.0".to_owned()].into_iter().collect();
        let mut wanted = Wanted::default();
        wanted.push(&reactor, Artifact::new("com.example", "lib", "1.0"));
        wanted.push(&reactor, Artifact::new("org.slf4j", "slf4j-api", "2.0.9"));
        // Nothing has published the reactor's own module; asking for it produces
        // a 404 that means nothing.
        assert_eq!(wanted.ordered.len(), 1);
        assert_eq!(wanted.ordered[0].artifact_id, "slf4j-api");
    }

    #[test]
    fn an_older_release_of_a_reactor_module_is_still_downloaded() {
        // The reactor builds 2.17.1; 2.13.2 is a published artifact like any
        // other, and something in the build genuinely needs it. Excluding it by
        // `groupId:artifactId` alone left `jackson-core` unable to start,
        // because its parent declares a build extension whose closure reaches
        // the older release.
        let reactor: BTreeSet<String> =
            ["com.fasterxml.jackson.core:jackson-core:2.17.1".to_owned()]
                .into_iter()
                .collect();
        let mut wanted = Wanted::default();
        wanted.push(
            &reactor,
            Artifact::new("com.fasterxml.jackson.core", "jackson-core", "2.17.1"),
        );
        wanted.push(
            &reactor,
            Artifact::new("com.fasterxml.jackson.core", "jackson-core", "2.13.2"),
        );
        assert_eq!(wanted.ordered.len(), 1);
        assert_eq!(
            wanted.ordered[0].version, "2.13.2",
            "the reactor's own version is skipped; an older one is not"
        );
    }

    #[test]
    fn an_artifact_is_queued_once_however_often_it_appears() {
        let mut wanted = Wanted::default();
        for _ in 0..3 {
            wanted.push(
                &BTreeSet::new(),
                Artifact::new("org.slf4j", "slf4j-api", "2.0.9"),
            );
        }
        assert_eq!(wanted.ordered.len(), 1);
    }

    #[test]
    fn a_symlink_at_the_destination_is_not_written_through() {
        let dir = tempfile::tempdir().unwrap();
        let cached = dir.path().join("https/host/cached.jar");
        std::fs::create_dir_all(cached.parent().unwrap()).unwrap();
        std::fs::write(&cached, b"payload").unwrap();

        let local = dir.path().join("m2");
        let artifact = Artifact::new("org.slf4j", "slf4j-api", "2.0.9");
        let destination = local.join(artifact_path(&artifact));
        std::fs::create_dir_all(destination.parent().unwrap()).unwrap();

        // A *dangling* link: `exists()` follows it and reports false, so the copy
        // used to create and fill whatever it pointed at.
        let victim = dir.path().join("victim");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&victim, &destination).unwrap();

        let mut report = SyncReport::default();
        let placed = materialize(&cached, &local, &artifact_path(&artifact), &mut report).unwrap();
        assert_eq!(placed, None);
        assert!(!victim.exists(), "the write followed the symlink");
    }

    #[test]
    fn losing_the_race_to_place_a_file_is_not_a_complaint() {
        let dir = tempfile::tempdir().unwrap();
        let cached = dir.path().join("https/host/a.jar");
        std::fs::create_dir_all(cached.parent().unwrap()).unwrap();
        std::fs::write(&cached, b"payload").unwrap();

        let local = dir.path().join("m2");
        let artifact = Artifact::new("org.slf4j", "slf4j-api", "2.0.9");
        let relative = artifact_path(&artifact);
        let destination = local.join(&relative);
        std::fs::create_dir_all(destination.parent().unwrap()).unwrap();

        // First placement succeeds; the second finds it already there. Two
        // concurrent syncs sharing a local repository hit this on nearly every
        // artifact, and reporting it warned once per file for nothing.
        let mut report = SyncReport::default();
        assert!(
            materialize(&cached, &local, &relative, &mut report)
                .unwrap()
                .is_some()
        );
        assert_eq!(
            materialize(&cached, &local, &relative, &mut report).unwrap(),
            None
        );
        assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    }

    #[test]
    fn a_path_outside_the_local_repository_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let cached = dir.path().join("https/host/a.jar");
        std::fs::create_dir_all(cached.parent().unwrap()).unwrap();
        std::fs::write(&cached, b"payload").unwrap();
        let local = dir.path().join("m2");

        let mut report = SyncReport::default();
        // `artifact_path` sanitizes coordinates so jv cannot produce this, but the
        // consequence of being wrong is an arbitrary write and the check is one
        // comparison.
        let escaped = dir.path().join("elsewhere").to_string_lossy().into_owned();
        assert_eq!(
            materialize(&cached, &local, &escaped, &mut report).unwrap(),
            None
        );
        assert!(!dir.path().join("elsewhere").exists());
        assert!(report.warnings.iter().any(|w| w.contains("refusing")));
    }

    #[test]
    fn a_file_already_in_the_local_repository_is_never_linked_into_the_output() {
        let dir = tempfile::tempdir().unwrap();
        let artifact = Artifact::new("org.slf4j", "slf4j-api", "2.0.9");
        let relative = artifact_path(&artifact);

        // The fetcher serves `~/.m2` hits straight from that directory, so this
        // is the path it would hand back.
        let home_m2 = dir.path().join("home-m2");
        let source = home_m2.join(&relative);
        std::fs::create_dir_all(source.parent().unwrap()).unwrap();
        std::fs::write(&source, b"maven's copy").unwrap();

        let output = dir.path().join("ci-m2");
        let mut report = SyncReport::default();
        let placed = materialize(&source, &output, &relative, &mut report)
            .unwrap()
            .expect("a destination");

        // Rewriting jv's output in place must not reach through to Maven's copy.
        std::fs::write(&placed, b"rewritten by a later build step").unwrap();
        assert_eq!(std::fs::read(&source).unwrap(), b"maven's copy");
    }

    #[test]
    fn materializing_never_replaces_what_maven_already_has() {
        let dir = tempfile::tempdir().unwrap();
        let cached = dir.path().join("https/host/cached.jar");
        std::fs::create_dir_all(cached.parent().unwrap()).unwrap();
        std::fs::write(&cached, b"from jv").unwrap();

        let local = dir.path().join("m2");
        let artifact = Artifact::new("org.slf4j", "slf4j-api", "2.0.9");
        let destination = local.join(artifact_path(&artifact));
        std::fs::create_dir_all(destination.parent().unwrap()).unwrap();
        std::fs::write(&destination, b"maven put this here").unwrap();

        let mut report = SyncReport::default();
        assert_eq!(
            materialize(&cached, &local, &artifact_path(&artifact), &mut report).unwrap(),
            None
        );
        // That directory belongs to Maven; a file already in it is Maven's
        // answer, not jv's to correct.
        assert_eq!(std::fs::read(&destination).unwrap(), b"maven put this here");
    }

    #[test]
    fn materializing_places_a_file_at_mavens_layout() {
        let dir = tempfile::tempdir().unwrap();
        let cached = dir.path().join("https/host/cached.jar");
        std::fs::create_dir_all(cached.parent().unwrap()).unwrap();
        std::fs::write(&cached, b"jar bytes").unwrap();
        let local = dir.path().join("m2");

        let artifact = Artifact::new("org.slf4j", "slf4j-api", "2.0.9");
        let mut report = SyncReport::default();
        let placed = materialize(&cached, &local, &artifact_path(&artifact), &mut report)
            .unwrap()
            .expect("a destination");
        assert!(placed.ends_with("org/slf4j/slf4j-api/2.0.9/slf4j-api-2.0.9.jar"));
        assert_eq!(std::fs::read(&placed).unwrap(), b"jar bytes");
        assert!(report.warnings.is_empty());
    }
}
