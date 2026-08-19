//! What each subcommand does.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use jv_driver::{Config, Project, Session, SyncRequest};
use jv_model::{Artifact, Scope};
use jv_resolver::{Graph, Verbosity};
use jv_tree::{Format, Options, render};
use jv_version::Version;
use owo_colors::OwoColorize;

use crate::args::{
    AddArgs, CommonArgs, OutdatedArgs, ProfileArgs, RemoveArgs, ResolveArgs, SyncArgs, TreeArgs,
};

/// Builds the driver config the flags describe.
pub(crate) fn config(common: &CommonArgs) -> Config {
    let mut config = Config {
        user_settings: common.settings.clone(),
        global_settings: common.global_settings.clone(),
        cache: common.cache_dir.clone(),
        offline: common.offline,
        update: common.update_policy(),
        user_properties: common.properties(),
        active_profiles: common.active_profiles(),
        inactive_profiles: common.inactive_profiles(),
        java_version: common.java_version.clone(),
        allow_insecure_http: common.allow_insecure_http,
        ..Config::new()
    };
    if common.no_local_repository {
        config = config.without_local_repository();
    }
    config
}

/// Loads the project the flags point at.
///
/// `file` is passed separately rather than living in `CommonArgs`, because
/// `jvx` and `jv exec` share those args and resolve an endpoint rather than a
/// project — `-f` there accepted a path and silently did nothing with it.
fn project(session: &Session, file: Option<&Path>) -> Result<Project> {
    match file {
        Some(path) => {
            // A directory is accepted where a POM is expected, because `-f
            // ../other-module` is what people type.
            let pom = if path.is_dir() {
                path.join("pom.xml")
            } else {
                path.to_path_buf()
            };
            session
                .project_at(&pom)
                .with_context(|| format!("cannot read {}", pom.display()))
        }
        None => {
            let here = std::env::current_dir().context("cannot read the working directory")?;
            session
                .project(&here)
                .with_context(|| format!("cannot resolve a project from {}", here.display()))
        }
    }
}

/// `jv tree`.
pub fn tree(args: &TreeArgs) -> Result<()> {
    let session = Session::new(&config(&args.common))?;
    let root = project(&session, args.file.as_deref())?;

    // Verbose output needs the losing nodes kept, which is a different
    // resolution, not just a different renderer.
    let verbosity = if args.verbose {
        Verbosity::Full
    } else {
        Verbosity::None
    };
    let options = Options {
        tokens: args.tokens.into(),
        verbose: args.verbose,
    };

    let targets: Vec<&Project> = if args.recursive {
        root.reactor()
    } else {
        vec![&root]
    };

    // Several trees need separating, but only `text` tolerates a separator: a
    // heading line between two JSON documents is not JSON, and the same holds
    // for dot, graphml and tgf. Maven writes one document per module rather than
    // interleaving them, and cannot express this at all.
    if targets.len() > 1 && args.output_type != Format::Text {
        bail!(
            "--recursive cannot be combined with --output-type {}: {} modules would produce {} \
             documents in one stream, which no reader of that format accepts. Resolve one module \
             at a time with -f, or use --output-type text.",
            args.output_type,
            targets.len(),
            targets.len()
        );
    }

    let mut out = String::new();
    for (index, target) in targets.iter().enumerate() {
        let resolution = session.resolve_project(target, verbosity)?;
        if targets.len() > 1 {
            if index > 0 {
                out.push('\n');
            }
            // A recursive run produces several trees; without a heading they run
            // together into one unreadable block.
            out.push_str(&format!("{}\n", target.path.display()));
        }
        out.push_str(&render(
            &resolution.collected.graph,
            args.output_type,
            options,
        ));
    }

    write_output(args.output_file.as_deref(), &out)?;
    report(&session.warnings());
    Ok(())
}

/// `jv resolve`.
pub fn resolve(args: &ResolveArgs) -> Result<()> {
    let session = Session::new(&config(&args.common))?;
    let root = project(&session, args.file.as_deref())?;

    let scope = match &args.scope {
        Some(text) => Some(parse_scope(text)?),
        None => None,
    };

    let targets: Vec<&Project> = if args.recursive {
        root.reactor()
    } else {
        vec![&root]
    };

    let mut artifacts: Vec<Artifact> = Vec::new();
    for target in &targets {
        let resolution = session.resolve_project(target, Verbosity::None)?;
        for artifact in resolved_artifacts(&resolution.collected.graph, scope) {
            if !artifacts.contains(&artifact) {
                artifacts.push(artifact);
            }
        }
    }

    let mut out = String::new();
    if args.paths || args.classpath {
        let mut paths = Vec::new();
        for artifact in &artifacts {
            let Some(materialized) = session.source().materialize(artifact)? else {
                bail!(
                    "{}:{}:{} is not available in any configured repository",
                    artifact.group_id,
                    artifact.artifact_id,
                    artifact.version
                );
            };
            paths.push(materialized.path);
        }
        if args.classpath {
            // The same joining `jv exec` hands to the JVM, so a classpath pasted
            // out of `jv resolve` and one built by `jvx` cannot disagree about
            // the separator.
            out.push_str(&jv_exec::class_path(&paths).to_string_lossy());
            out.push('\n');
        } else {
            for path in paths {
                out.push_str(&format!("{}\n", path.display()));
            }
        }
    } else {
        for artifact in &artifacts {
            out.push_str(&format!("{}\n", coordinates(artifact)));
        }
    }

    write_output(None, &out)?;
    report(&session.warnings());
    Ok(())
}

/// `jv sync`.
pub fn sync(args: &SyncArgs) -> Result<()> {
    let mut config = config(&args.common);
    // The plugins the lifecycle binds appear in no POM, and `mvn -o` stops at
    // the first phase without them — so a sync that skips them produces a
    // repository that looks complete and is not.
    config.lifecycle_bindings = !args.no_plugins;

    let session = Session::new(&config)?;
    let root = project(&session, args.file.as_deref())?;
    let targets: Vec<&Project> = if args.no_recursive {
        vec![&root]
    } else {
        root.reactor()
    };

    let local_repository = if args.cache_only {
        None
    } else {
        match args.local_repository.clone() {
            Some(path) => Some(path),
            None => Some(default_local_repository(&args.common)?),
        }
    };

    let synced = jv_driver::sync(
        &session,
        &targets,
        &SyncRequest {
            plugins: !args.no_plugins,
            plugin_dependencies: !args.no_plugins,
            local_repository: local_repository.clone(),
            managed_plugin_dependencies: args.all_plugins,
            toolchains: config.load_toolchains(),
            also: also(&args.also)?,
            ..SyncRequest::default()
        },
    )?;

    report(&synced.warnings);
    for missing in &synced.missing {
        // Not an error: an optional dependency's jar and a plugin that lives in
        // a repository this machine cannot see both land here, and neither
        // should abort a sync of a thousand files.
        eprintln!(
            "{} {missing} is not in any configured repository",
            "missing:".yellow()
        );
    }

    match &local_repository {
        Some(path) => println!(
            "synced {} artifacts into {}",
            synced.artifacts.len(),
            path.display()
        ),
        None => println!(
            "synced {} artifacts into jv's cache",
            synced.artifacts.len()
        ),
    }
    Ok(())
}

/// Where Maven keeps its local repository, according to the same settings the
/// session read.
fn default_local_repository(common: &CommonArgs) -> Result<PathBuf> {
    let settings = config(common).load_settings()?;
    match settings.local_repository.as_deref() {
        Some(declared) if !declared.trim().is_empty() => Ok(PathBuf::from(declared.trim())),
        _ => Ok(dirs_home()?.join(".m2").join("repository")),
    }
}

fn dirs_home() -> Result<PathBuf> {
    // The one place jv writes outside its own cache, so failing to find the home
    // directory has to be an error rather than a silent no-op.
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|home| !home.as_os_str().is_empty())
        .context("cannot find your home directory; pass --local-repository")
}

/// The artifacts a resolved graph selected, in preorder, excluding the root.
///
/// Preorder rather than sorted, because that is the order Maven puts on a
/// classpath and the order changes behaviour when two jars carry the same class.
pub(crate) fn resolved_artifacts(graph: &Graph, scope: Option<Scope>) -> Vec<Artifact> {
    let mut artifacts = Vec::new();
    for (id, _depth) in graph.preorder() {
        if id == graph.root() {
            continue;
        }
        let node = graph.node(id);
        // A losing node has no artifact of its own to contribute.
        if node.omitted_for.is_some() {
            continue;
        }
        if let Some(wanted) = scope {
            if !includes(wanted, node.scope()) {
                continue;
            }
        }
        if let Some(artifact) = &node.artifact {
            artifacts.push(artifact.clone());
        }
    }
    artifacts
}

/// Whether a resolution for `wanted` includes a dependency in `actual`.
///
/// This is Maven's classpath composition: the test classpath is everything, the
/// runtime classpath drops `provided` and `test`, and the compile classpath
/// drops `runtime` and `test`.
fn includes(wanted: Scope, actual: Scope) -> bool {
    match wanted {
        Scope::Compile => matches!(actual, Scope::Compile | Scope::Provided | Scope::System),
        Scope::Runtime => matches!(actual, Scope::Compile | Scope::Runtime),
        Scope::Test => true,
        Scope::Provided => matches!(actual, Scope::Compile | Scope::Provided | Scope::System),
        Scope::System => actual == Scope::System,
        Scope::Import => false,
    }
}

fn parse_scope(text: &str) -> Result<Scope> {
    text.parse::<Scope>().map_err(|_| {
        anyhow::anyhow!(
            "`{text}` is not a scope; expected compile, runtime, test, provided or system"
        )
    })
}

fn coordinates(artifact: &Artifact) -> String {
    let mut text = format!(
        "{}:{}:{}",
        artifact.group_id, artifact.artifact_id, artifact.extension
    );
    if !artifact.classifier.is_empty() {
        text.push(':');
        text.push_str(&artifact.classifier);
    }
    text.push(':');
    text.push_str(&artifact.version);
    text
}

fn write_output(path: Option<&Path>, text: &str) -> Result<()> {
    match path {
        Some(path) => {
            std::fs::write(path, text).with_context(|| format!("cannot write {}", path.display()))
        }
        None => {
            let stdout = std::io::stdout();
            let mut handle = stdout.lock();
            handle.write_all(text.as_bytes())?;
            handle.flush()?;
            Ok(())
        }
    }
}

/// Prints warnings to standard error.
///
/// Standard error, not standard output, so `jv tree > file` still produces a
/// file that is only the tree.
pub(crate) fn report(warnings: &[String]) {
    for warning in warnings {
        eprintln!("{} {warning}", "warning:".yellow().bold());
    }
}

/// Parses `--also` coordinates.
///
/// `group:artifact:version`, optionally `group:artifact:extension:version` or
/// `group:artifact:extension:classifier:version`, which is Maven's own spelling
/// for the same thing on a command line.
fn also(arguments: &[String]) -> anyhow::Result<Vec<Artifact>> {
    let mut found = Vec::new();
    for argument in arguments {
        let parts: Vec<&str> = argument.split(':').collect();
        let artifact = match parts.as_slice() {
            [group, artifact, version] => Artifact::new(*group, *artifact, *version),
            [group, artifact, extension, version] => Artifact {
                extension: (*extension).to_owned(),
                ..Artifact::new(*group, *artifact, *version)
            },
            [group, artifact, extension, classifier, version] => Artifact {
                extension: (*extension).to_owned(),
                classifier: (*classifier).to_owned(),
                ..Artifact::new(*group, *artifact, *version)
            },
            _ => {
                anyhow::bail!(
                    "--also {argument}: expected group:artifact:version, \
                     group:artifact:extension:version or \
                     group:artifact:extension:classifier:version"
                )
            }
        };
        if artifact.group_id.is_empty()
            || artifact.artifact_id.is_empty()
            || artifact.version.is_empty()
        {
            anyhow::bail!("--also {argument}: no part may be empty");
        }
        found.push(artifact);
    }
    Ok(found)
}

/// Runs a build under the `EventSpy` that reports where its time went.
///
/// Maven loads an extension named by `maven.ext.class.path` before the build
/// starts, so this is a pass-through: the command runs exactly as the user
/// wrote it, with one property added, and its exit code is forwarded. Anything
/// jv printed would otherwise be indistinguishable from the build's own output,
/// so the report comes from the spy itself, at the end.
pub fn profile(args: &ProfileArgs) -> Result<ExitCode> {
    let jar = profiler_jar(args.profiler_jar.clone())?;

    let mut command = args.command.clone();
    if command.is_empty() {
        // The command people are trying to understand, nine times in ten.
        command = vec!["mvn".to_owned(), "test".to_owned()];
    }
    let (program, rest) = command.split_first().expect("a non-empty command");

    let mut child = std::process::Command::new(program);
    child.arg(format!("-Dmaven.ext.class.path={}", jar.display()));
    child.args(rest);

    let status = child
        .status()
        .with_context(|| format!("cannot run {program}"))?;
    Ok(ExitCode::from(
        u8::try_from(status.code().unwrap_or(1)).unwrap_or(1),
    ))
}

/// Finds the `EventSpy` jar.
///
/// Beside the executable first, because that is where an installed jv keeps it,
/// then in a build tree, so `cargo run` works without an install step.
fn profiler_jar(given: Option<PathBuf>) -> Result<PathBuf> {
    const JAR: &str = "jv-profiler.jar";

    let mut tried = Vec::new();
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(given) = given {
        candidates.push(given);
    } else if let Some(from_environment) = std::env::var_os("JV_PROFILER_JAR") {
        candidates.push(PathBuf::from(from_environment));
    } else {
        if let Ok(executable) = std::env::current_exe()
            && let Some(directory) = executable.parent()
        {
            candidates.push(directory.join(JAR));
            // A cargo build tree: target/{debug,release}/jv, with the jar built
            // by java/jv-profiler/build.sh.
            if let Some(root) = directory.parent().and_then(Path::parent) {
                candidates.push(root.join("java/jv-profiler/target").join(JAR));
            }
        }
        candidates.push(PathBuf::from("java/jv-profiler/target").join(JAR));
    }

    for candidate in candidates {
        if candidate.is_file() {
            return Ok(candidate);
        }
        tried.push(candidate.display().to_string());
    }
    bail!(
        "cannot find {JAR}; build it with java/jv-profiler/build.sh, or point \
         --profiler-jar or $JV_PROFILER_JAR at it. Looked in:\n  {}",
        tried.join("\n  ")
    )
}

/// Adds a dependency to a POM.
///
/// The edit itself is `jv-edit`'s problem — it rewrites one span and copies the
/// rest byte for byte. What is decided here is *what to write*, and the part
/// that matters is the version.
pub fn add(args: &AddArgs) -> Result<()> {
    let (group_id, artifact_id, given_version) = split_coordinates(&args.coordinates)?;

    let config = config(&args.common);
    let session = Session::new(&config)?;
    let root = project(&session, args.file.as_deref())?;
    let target = pick_module(&root, args.module.as_deref())?;

    let version = match given_version {
        Some(version) => Some(version),
        // Managed already — by `<dependencyManagement>`, or by a BOM the
        // project imported. Writing a version here would pin what the project
        // deliberately left for its management to decide, and quietly diverge
        // from every other module the next time the BOM moves. This is the
        // single behaviour that decides whether a tool like this is trusted.
        None if managed(target, &group_id, &artifact_id) => {
            eprintln!(
                "{} {group_id}:{artifact_id} is already managed; adding it without a version",
                "note:".cyan()
            );
            None
        }
        None => Some(newest_release(&session, &group_id, &artifact_id)?),
    };

    let dependency = jv_edit::Dependency {
        group_id: group_id.clone(),
        artifact_id: artifact_id.clone(),
        version,
        scope: if args.test {
            Some("test".to_owned())
        } else {
            args.scope.clone()
        },
        classifier: args.classifier.clone(),
        type_: args.type_.clone(),
        optional: args.optional,
    };

    let before = std::fs::read_to_string(&target.path)
        .with_context(|| format!("cannot read {}", target.path.display()))?;
    match jv_edit::add_dependency(&before, &dependency)? {
        jv_edit::Added::AlreadyPresent { line, version } => {
            let declared = version.unwrap_or_else(|| "no version".to_owned());
            println!(
                "{group_id}:{artifact_id} is already a dependency ({} at {}:{line}); nothing to do",
                declared,
                target.path.display()
            );
        }
        jv_edit::Added::Inserted(after) if args.dry_run => {
            print!("{after}");
        }
        jv_edit::Added::Inserted(after) => {
            std::fs::write(&target.path, &after)
                .with_context(|| format!("cannot write {}", target.path.display()))?;
            let shown = dependency
                .version
                .as_deref()
                .map_or_else(String::new, |version| format!(":{version}"));
            println!(
                "added {group_id}:{artifact_id}{shown} to {}",
                target.path.display()
            );
        }
    }
    Ok(())
}

/// Splits `group:artifact` or `group:artifact:version`.
fn split_coordinates(text: &str) -> Result<(String, String, Option<String>)> {
    let parts: Vec<&str> = text.split(':').collect();
    let (group_id, artifact_id, version) = match parts.as_slice() {
        [group, artifact] => (*group, *artifact, None),
        [group, artifact, version] => (*group, *artifact, Some((*version).to_owned())),
        _ => bail!("expected group:artifact or group:artifact:version, got {text}"),
    };
    if group_id.is_empty() || artifact_id.is_empty() {
        bail!("{text}: neither the group nor the artifact may be empty");
    }
    Ok((group_id.to_owned(), artifact_id.to_owned(), version))
}

/// Whether the project already manages a version for these coordinates.
///
/// Read from the *effective* model, so a version supplied by a parent or by an
/// imported BOM counts, which is the whole point — those are exactly the cases
/// a raw read of this one POM would miss.
fn managed(project: &Project, group_id: &str, artifact_id: &str) -> bool {
    project.model.dependency_management.iter().any(|managed| {
        managed.group_id == group_id
            && managed.artifact_id == artifact_id
            && managed.version.is_some()
    })
}

/// The newest released version, as Maven would pick it for `RELEASE`.
fn newest_release(session: &Session, group_id: &str, artifact_id: &str) -> Result<String> {
    session
        .source()
        .plugin_version(group_id, artifact_id)
        .with_context(|| format!("cannot read the versions of {group_id}:{artifact_id}"))?
        .with_context(|| {
            format!(
                "{group_id}:{artifact_id} has no released version in any configured repository; \
                 give one explicitly as {group_id}:{artifact_id}:VERSION"
            )
        })
}

/// Removes a dependency from a POM.
///
/// No repository is consulted: what to remove is fully determined by the
/// coordinates and the file, so this works offline and instantly. A version in
/// the argument is accepted and ignored, so a line copied from `jv add` works
/// rather than failing on an argument that reads as correct.
pub fn remove(args: &RemoveArgs) -> Result<()> {
    let (group_id, artifact_id, _) = split_coordinates(&args.coordinates)?;

    let config = config(&args.common);
    let session = Session::new(&config)?;
    let root = project(&session, args.file.as_deref())?;
    let target = pick_module(&root, args.module.as_deref())?;

    let before = std::fs::read_to_string(&target.path)
        .with_context(|| format!("cannot read {}", target.path.display()))?;
    match jv_edit::remove_dependency(&before, &group_id, &artifact_id)? {
        jv_edit::Removed::NotPresent => {
            println!(
                "{group_id}:{artifact_id} is not a dependency of {}; nothing to do",
                target.path.display()
            );
        }
        jv_edit::Removed::Removed(after) if args.dry_run => print!("{after}"),
        jv_edit::Removed::Removed(after) => {
            std::fs::write(&target.path, &after)
                .with_context(|| format!("cannot write {}", target.path.display()))?;
            println!(
                "removed {group_id}:{artifact_id} from {}",
                target.path.display()
            );
        }
    }
    Ok(())
}

/// The module to edit, or the root when none was named.
fn pick_module<'a>(root: &'a Project, module: Option<&str>) -> Result<&'a Project> {
    let Some(module) = module else {
        return Ok(root);
    };
    root.reactor()
        .into_iter()
        .find(|project| project.model.artifact_id.as_deref() == Some(module))
        .with_context(|| {
            format!(
                "no module named {module}; this build has: {}",
                root.reactor()
                    .iter()
                    .filter_map(|project| project.model.artifact_id.as_deref())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })
}

/// Something declared that could have a newer version.
#[derive(Clone)]
struct Candidate {
    group_id: String,
    artifact_id: String,
    version: String,
    kind: Kind,
}

/// Where a version was declared, which decides whether the reader can act on it.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// `<dependencies>` in this project.
    Dependency,
    /// `<dependencyManagement>` in this project — including a BOM import, where
    /// bumping the import is usually the whole upgrade.
    Managed,
    /// `<build><plugins>` or `<pluginManagement>`.
    Plugin,
}

impl Kind {
    fn marker(self) -> &'static str {
        match self {
            Kind::Dependency => "",
            Kind::Managed => " (managed)",
            Kind::Plugin => " (plugin)",
        }
    }
}

/// What asking about one candidate produced.
enum Check {
    Outdated {
        candidate: Candidate,
        newest: String,
        bump: Bump,
    },
    Current,
    /// The question could not be answered — offline with nothing cached, a
    /// repository that would not serve it, an artifact with no metadata. Kept
    /// distinct from `Current` because the two are indistinguishable to a
    /// reader and only one of them means what it says.
    Unknown(Candidate, String),
}

/// How far apart two versions are, which is what decides whether an upgrade is
/// a five-minute job or a project.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Bump {
    Major,
    Minor,
    Patch,
}

impl Bump {
    fn label(self) -> &'static str {
        match self {
            Bump::Major => "major",
            Bump::Minor => "minor",
            Bump::Patch => "patch",
        }
    }
}

/// Reports dependencies with newer versions available.
///
/// The equivalent is `versions:display-dependency-updates`, which takes tens of
/// seconds on a real project because every lookup is serial inside a Maven that
/// costs a second to start. Nothing here is cleverer than that plugin — the
/// lookups just run in parallel, in a process that starts in milliseconds.
pub fn outdated(args: &OutdatedArgs) -> Result<ExitCode> {
    use rayon::prelude::*;

    let config = config(&args.common);
    let session = Session::new(&config)?;
    let root = project(&session, args.file.as_deref())?;
    let targets: Vec<&Project> = if args.no_recursive {
        vec![&root]
    } else {
        root.reactor()
    };

    // Declared dependencies only, deduplicated across modules. A transitive
    // dependency's version is not something this POM can change, so listing it
    // would be noise nobody can act on.
    let mut wanted: Vec<Candidate> = Vec::new();
    let mut seen: std::collections::HashSet<(String, String)> = std::collections::HashSet::new();
    for target in &targets {
        for dependency in &target.model.dependencies {
            let Some(version) = dependency.version.as_deref().filter(|v| !v.is_empty()) else {
                continue;
            };
            let key = (dependency.group_id.clone(), dependency.artifact_id.clone());
            if seen.insert(key) {
                wanted.push(Candidate {
                    group_id: dependency.group_id.clone(),
                    artifact_id: dependency.artifact_id.clone(),
                    version: version.to_owned(),
                    kind: Kind::Dependency,
                });
            }
        }
        // Managed versions this POM declares itself.
        //
        // Read from the raw POM to decide *which* to report, and from the
        // effective model to learn what version they resolved to. An effective
        // model carries everything the parent chain manages — nineteen entries
        // from the Apache parent on commons-io — and none of those are
        // something this project can bump. What it declares itself is, and a
        // `<scope>import</scope>` BOM is the most actionable entry of all,
        // since bumping the import usually *is* the upgrade.
        for (group_id, artifact_id, declared_version) in declared_management(target) {
            // The effective model first, because it has properties resolved.
            // Then the raw version, because an imported BOM does *not* survive
            // into the effective model — model building expands it into the
            // entries it contributes and the import itself is gone. Skipping it
            // would drop the one entry most worth reporting: for a project that
            // gets its versions from a BOM, bumping the import is the upgrade.
            let Some(version) = target
                .model
                .dependency_management
                .iter()
                .find(|effective| {
                    effective.group_id == group_id && effective.artifact_id == artifact_id
                })
                .and_then(|effective| effective.version.clone())
                .or(declared_version)
                .filter(|version| !version.is_empty() && !version.contains("${"))
            else {
                continue;
            };
            if seen.insert((group_id.clone(), artifact_id.clone())) {
                wanted.push(Candidate {
                    group_id,
                    artifact_id,
                    version,
                    kind: Kind::Managed,
                });
            }
        }

        if args.plugins
            && let Some(build) = &target.model.build
        {
            for plugin in build.plugins.iter().chain(&build.plugin_management) {
                let (Some(artifact_id), Some(version)) = (
                    plugin.artifact_id.as_deref(),
                    plugin.version.as_deref().filter(|v| !v.is_empty()),
                ) else {
                    continue;
                };
                let group_id = plugin.group_id_or_default().to_owned();
                let key = (group_id.clone(), artifact_id.to_owned());
                if seen.insert(key) {
                    wanted.push(Candidate {
                        group_id,
                        artifact_id: artifact_id.to_owned(),
                        version: version.to_owned(),
                        kind: Kind::Plugin,
                    });
                }
            }
        }
    }

    let source = session.source();
    let checked: Vec<Check> = wanted
        .par_iter()
        .map(|candidate| {
            // A lookup that failed is *not* "up to date". Reporting it as such
            // is the failure mode this command has to avoid above all others:
            // an answer nobody can distinguish from a real all-clear, on the
            // one question people run it to be sure about.
            let published =
                match source.published_versions(&candidate.group_id, &candidate.artifact_id) {
                    Ok(published) => published,
                    Err(error) => return Check::Unknown(candidate.clone(), error),
                };
            let newest = published
                .iter()
                .filter(|version| args.pre_release || !is_pre_release(version))
                .max_by(|left, right| Version::parse(left).cmp(&Version::parse(right)));
            match newest {
                // Two different situations, and telling them apart is what
                // makes the line worth printing. An empty list means no
                // repository jv could consult knows the artifact — which
                // offline includes "not cached", not just "not published". A
                // list that emptied under the filter means the only versions
                // out there are previews, and `--pre-release` will show them.
                None if published.is_empty() => Check::Unknown(
                    candidate.clone(),
                    "no versions are listed for it in the repositories jv could consult".to_owned(),
                ),
                None => Check::Unknown(
                    candidate.clone(),
                    format!(
                        "only pre-release versions exist ({}); pass --pre-release to see them",
                        published.last().map_or("", String::as_str)
                    ),
                ),
                Some(newest) if Version::parse(newest) > Version::parse(&candidate.version) => {
                    Check::Outdated {
                        candidate: candidate.clone(),
                        newest: newest.clone(),
                        bump: bump(&candidate.version, newest),
                    }
                }
                Some(_) => Check::Current,
            }
        })
        .collect();

    let mut outdated: Vec<_> = checked
        .iter()
        .filter_map(|check| match check {
            Check::Outdated {
                candidate,
                newest,
                bump,
            } => Some((candidate, newest, *bump)),
            _ => None,
        })
        .collect();
    outdated.sort_by_key(|(candidate, _, _)| {
        (candidate.group_id.clone(), candidate.artifact_id.clone())
    });

    let unknown: Vec<_> = checked
        .iter()
        .filter_map(|check| match check {
            Check::Unknown(candidate, why) => Some((candidate, why)),
            _ => None,
        })
        .collect();

    let answered = wanted.len() - unknown.len();
    if outdated.is_empty() {
        // "Up to date" is a claim about checks that happened. With none, the
        // only honest thing to say is that nothing was established — the
        // warning below then says why.
        if answered == 0 && !wanted.is_empty() {
            println!(
                "nothing could be checked, so nothing is known about {} declared",
                wanted.len()
            );
        } else {
            println!(
                "everything is up to date ({answered} of {} checked)",
                wanted.len()
            );
        }
    } else {
        let width = outdated
            .iter()
            .map(|(candidate, _, _)| candidate.group_id.len() + candidate.artifact_id.len() + 1)
            .max()
            .unwrap_or(40);
        for (candidate, newest, bump) in &outdated {
            let coordinates = format!("{}:{}", candidate.group_id, candidate.artifact_id);
            let how = match bump {
                Bump::Major => bump.label().red().to_string(),
                Bump::Minor => bump.label().yellow().to_string(),
                Bump::Patch => bump.label().green().to_string(),
            };
            let marker = candidate.kind.marker();
            println!(
                "{coordinates:width$}  {:>14} -> {newest:<14} {how}{marker}",
                candidate.version
            );
        }
        println!();
        println!("{} of {answered} outdated", outdated.len());
    }

    if !unknown.is_empty() {
        eprintln!();
        eprintln!(
            "{} {} could not be checked:",
            "warning:".yellow(),
            unknown.len()
        );
        for (candidate, why) in unknown.iter().take(5) {
            eprintln!("  {}:{}: {why}", candidate.group_id, candidate.artifact_id);
        }
        if unknown.len() > 5 {
            eprintln!("  ... and {} more", unknown.len() - 5);
        }
    }

    Ok(if args.exit_code && !outdated.is_empty() {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    })
}

/// The `<dependencyManagement>` entries a POM declares itself.
///
/// The effective model cannot answer this: by the time it exists, everything a
/// parent manages looks exactly like something this project manages, and only
/// one of those is a version the reader can change here. So the raw file is
/// read again — cheaply, and only to learn which coordinates are local.
///
/// A POM that cannot be re-read contributes nothing rather than failing the
/// command: this is an enrichment, not the answer.
fn declared_management(project: &Project) -> Vec<(String, String, Option<String>)> {
    let Ok(text) = std::fs::read_to_string(&project.path) else {
        return Vec::new();
    };
    let Ok(parsed) = jv_model::parse_pom(&text) else {
        return Vec::new();
    };
    parsed
        .model
        .dependency_management
        .into_iter()
        .map(|managed| (managed.group_id, managed.artifact_id, managed.version))
        .collect()
}

/// Whether a version looks like a preview rather than a release.
///
/// A heuristic on the string, not a judgement `jv-version` makes: Maven's
/// ordering already ranks `1.0-alpha` below `1.0`, but it has no notion of
/// "this is not something to recommend". Publishers are inconsistent enough
/// that anything more clever than a label match would be wrong more often.
fn is_pre_release(version: &str) -> bool {
    let lowered = version.to_ascii_lowercase();
    [
        "alpha",
        "beta",
        "milestone",
        "-rc",
        ".rc",
        "-cr",
        ".cr",
        "snapshot",
        "-m1",
        "-m2",
        "-m3",
        "-preview",
        "-dev",
        "-incubating",
    ]
    .iter()
    .any(|marker| lowered.contains(marker))
}

/// Which component moved, comparing leading numeric parts.
fn bump(current: &str, newest: &str) -> Bump {
    let parts = |version: &str| -> Vec<u64> {
        version
            .split(|character: char| !character.is_ascii_digit())
            .filter(|piece| !piece.is_empty())
            .filter_map(|piece| piece.parse().ok())
            .collect()
    };
    let (left, right) = (parts(current), parts(newest));
    for index in 0..2 {
        if left.get(index).copied().unwrap_or(0) != right.get(index).copied().unwrap_or(0) {
            return if index == 0 { Bump::Major } else { Bump::Minor };
        }
    }
    Bump::Patch
}

#[cfg(test)]
mod tests {

    #[test]
    fn a_bump_is_classified_by_which_component_moved() {
        assert!(matches!(super::bump("1.2.3", "2.0.0"), super::Bump::Major));
        assert!(matches!(super::bump("1.2.3", "1.3.0"), super::Bump::Minor));
        assert!(matches!(super::bump("1.2.3", "1.2.4"), super::Bump::Patch));
        // Maven versions are not semver, and plenty carry qualifiers.
        assert!(matches!(
            super::bump("33.4.8-jre", "33.7.1-jre"),
            super::Bump::Minor
        ));
        assert!(matches!(super::bump("2.0.9", "3.0.0"), super::Bump::Major));
    }

    #[test]
    fn previews_are_recognised_so_they_are_not_recommended() {
        // A project on a stable release is not outdated because a release
        // candidate exists, and a tool that says otherwise gets ignored.
        for preview in [
            "1.0-alpha1",
            "2.0.0-beta",
            "3.0.0-M1",
            "1.2-rc1",
            "5.0.0.CR2",
            "1.0-SNAPSHOT",
            "7.0-milestone3",
            "1.0-preview",
        ] {
            assert!(super::is_pre_release(preview), "{preview} is a preview");
        }
        for release in [
            "1.0",
            "33.4.8-jre",
            "2.0.9",
            "5.10.2",
            "1.14.12",
            "3.0.0.Final",
        ] {
            assert!(!super::is_pre_release(release), "{release} is a release");
        }
    }
    use super::*;

    #[test]
    fn the_compile_classpath_excludes_runtime_and_test() {
        assert!(includes(Scope::Compile, Scope::Compile));
        assert!(includes(Scope::Compile, Scope::Provided));
        assert!(!includes(Scope::Compile, Scope::Runtime));
        assert!(!includes(Scope::Compile, Scope::Test));
    }

    #[test]
    fn the_runtime_classpath_excludes_provided() {
        // `provided` means the container supplies it, so putting it on the
        // runtime classpath is exactly what the scope exists to prevent.
        assert!(includes(Scope::Runtime, Scope::Compile));
        assert!(includes(Scope::Runtime, Scope::Runtime));
        assert!(!includes(Scope::Runtime, Scope::Provided));
        assert!(!includes(Scope::Runtime, Scope::Test));
    }

    #[test]
    fn the_test_classpath_holds_everything() {
        for scope in [
            Scope::Compile,
            Scope::Runtime,
            Scope::Provided,
            Scope::Test,
            Scope::System,
        ] {
            assert!(
                includes(Scope::Test, scope),
                "test should include {scope:?}"
            );
        }
    }

    #[test]
    fn coordinates_include_a_classifier_only_when_there_is_one() {
        let plain = Artifact::new("g", "a", "1.0");
        assert_eq!(coordinates(&plain), "g:a:jar:1.0");
        assert_eq!(
            coordinates(&plain.clone().with_classifier("sources")),
            "g:a:jar:sources:1.0"
        );
    }
}
