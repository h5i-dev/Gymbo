//! `jvx` / `jv exec`: coordinates in, running tool out.
//!
//! The whole command is five steps — read the endpoint, pick a version, resolve
//! and download, find the main class, hand the process to the JVM — and every
//! one of them lives somewhere else. `jv-exec` owns the decisions, `jv-driver`
//! owns the resolve. What is left here is the order they happen in and the
//! errors that only make sense with the user's own words in them.

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result, anyhow, bail};
use jv_driver::Session;
use jv_exec::{Endpoint, Launch, java, main_class, release};
use jv_model::{Artifact, Scope};
use jv_resolver::{DescriptorSource as _, Verbosity};

use crate::args::ExecArgs;
use crate::commands::{config, report, resolved_artifacts};

pub fn run(args: &ExecArgs) -> Result<ExitCode> {
    let endpoint = match args.main {
        // `--main` has already answered the only question the heuristic exists
        // to answer, so the endpoint's last field goes back to being positional.
        Some(_) => Endpoint::parse_positional(&args.endpoint),
        None => Endpoint::parse(&args.endpoint),
    }?;

    let session = Session::new(&config(&args.common))?;
    let version = version(&session, &endpoint)?;
    let artifact = endpoint.artifact(&version);

    // Resolved as a dependency rather than as a project, which is what keeps the
    // tool's own test dependencies off the classpath it is about to run on.
    let resolution = session.resolve_artifact(&artifact, Verbosity::None)?;
    let graph = &resolution.collected.graph;
    // The graph's root, not the artifact that went in: a relocated tool resolves
    // to different coordinates, and the manifest jvx must read is the one it
    // actually downloaded.
    let root = graph
        .node(graph.root())
        .artifact
        .clone()
        .unwrap_or(artifact.clone());

    let mut wanted = vec![root.clone()];
    for dependency in resolved_artifacts(graph, Some(Scope::Runtime)) {
        if !wanted.contains(&dependency) {
            wanted.push(dependency);
        }
    }

    let mut class_path: Vec<PathBuf> = Vec::with_capacity(wanted.len());
    for artifact in &wanted {
        let Some(materialized) = session
            .source()
            .materialize(artifact)
            .with_context(|| format!("cannot download {}", coordinates(artifact)))?
        else {
            bail!(
                "{} is not available in any configured repository",
                coordinates(artifact)
            );
        };
        class_path.push(materialized.path);
    }

    let requested = args.main.as_deref().or(endpoint.main_class.as_deref());
    let (main, _source) = main_class::detect(
        requested,
        class_path.first().map(PathBuf::as_path),
        &endpoint.coordinates(),
    )?;
    let java = java::locate()?;

    // Everything that has anything to say must say it now: on Unix the next call
    // replaces this process, so nothing after it runs — no Drop, no flush, no
    // deferred warning.
    report(&session.warnings());

    let launch = Launch {
        java,
        class_path,
        main_class: main,
        args: args.args.clone(),
    };
    let code = launch.run()?;
    // Exit codes are a byte; a JVM that returns something wider has already had
    // it truncated by the kernel, so truncating the same way is the honest
    // report rather than a rounding error.
    Ok(ExitCode::from(code as u8))
}

/// The version to run: the endpoint's, or the greatest release published.
fn version(session: &Session, endpoint: &Endpoint) -> Result<String> {
    if let Some(version) = &endpoint.version {
        return Ok(version.clone());
    }
    let published = session
        .source()
        .versions(&endpoint.group_id, &endpoint.artifact_id)
        .map_err(|reason| {
            anyhow!(
                "cannot list the versions of {}:{}: {reason}",
                endpoint.group_id,
                endpoint.artifact_id
            )
        })?;
    release::select(&published)
        .map(str::to_owned)
        .ok_or_else(|| {
            anyhow!(jv_exec::ExecError::NoVersion {
                group_id: endpoint.group_id.clone(),
                artifact_id: endpoint.artifact_id.clone(),
            })
        })
}

fn coordinates(artifact: &Artifact) -> String {
    let mut text = format!(
        "{}:{}:{}",
        artifact.group_id, artifact.artifact_id, artifact.version
    );
    if !artifact.classifier.is_empty() {
        text.push(':');
        text.push_str(&artifact.classifier);
    }
    text
}
