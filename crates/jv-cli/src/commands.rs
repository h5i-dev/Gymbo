//! What each subcommand does.

use std::io::Write as _;
use std::path::Path;

use anyhow::{Context, Result, bail};
use jv_driver::{Config, Project, Session};
use jv_model::{Artifact, Scope};
use jv_resolver::{Graph, Verbosity};
use jv_tree::{Options, render};
use owo_colors::OwoColorize;

use crate::args::{CommonArgs, ResolveArgs, TreeArgs};

/// Builds the driver config the flags describe.
fn config(common: &CommonArgs) -> Config {
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
        ..Config::new()
    };
    if common.no_local_repository {
        config = config.without_local_repository();
    }
    config
}

/// Loads the project the flags point at.
fn project(session: &Session, common: &CommonArgs) -> Result<Project> {
    match &common.file {
        Some(path) => {
            // A directory is accepted where a POM is expected, because `-f
            // ../other-module` is what people type.
            let pom = if path.is_dir() {
                path.join("pom.xml")
            } else {
                path.clone()
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
    let root = project(&session, &args.common)?;

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
    let root = project(&session, &args.common)?;

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
            let Some((_origin, path)) = session.source().materialize(artifact)? else {
                bail!(
                    "{}:{}:{} is not available in any configured repository",
                    artifact.group_id,
                    artifact.artifact_id,
                    artifact.version
                );
            };
            paths.push(path);
        }
        if args.classpath {
            let joined: Vec<String> = paths
                .iter()
                .map(|path| path.display().to_string())
                .collect();
            // The platform separator, because this is meant to be pasted into a
            // `java -cp` invocation.
            out.push_str(&joined.join(if cfg!(windows) { ";" } else { ":" }));
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

/// The artifacts a resolved graph selected, in preorder, excluding the root.
///
/// Preorder rather than sorted, because that is the order Maven puts on a
/// classpath and the order changes behaviour when two jars carry the same class.
fn resolved_artifacts(graph: &Graph, scope: Option<Scope>) -> Vec<Artifact> {
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
fn report(warnings: &[String]) {
    for warning in warnings {
        eprintln!("{} {warning}", "warning:".yellow().bold());
    }
}

#[cfg(test)]
mod tests {
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
