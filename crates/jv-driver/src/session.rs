//! One resolve, from a directory to a dependency graph.
//!
//! This is the whole pipeline in one place: settings, repositories, cache,
//! transport, POM fetching, effective models, collection, conflict resolution.
//! Everything above it — the CLI, `jv sync`, `jvx` — works in terms of a
//! [`Session`] and never assembles the parts itself.

use std::path::Path;
use std::sync::Arc;

use jv_cache::{Fetcher, HttpTransport, OfflineTransport, Store, Transport};
use jv_model::{Artifact, Dependency};
use jv_model_builder::BuildContext;
use jv_repo::Repository;
use jv_resolver::{CollectRequest, Collected, Verbosity, collect, resolve_conflicts};

use crate::config::Config;
use crate::error::DriverError;
use crate::project::{Project, find_pom, load_project};
use crate::source::RepositorySource;

/// A configured resolve.
///
/// # Threading
///
/// A session blocks on async work, so it must be used from a thread that is not
/// a tokio worker. Build the runtime in `main` and keep the session on the main
/// thread, or run it inside `spawn_blocking`.
#[derive(Debug)]
pub struct Session {
    source: RepositorySource,
    // Kept alive for as long as the session: dropping it would shut down the
    // background prefetch tasks the source spawns.
    _runtime: RuntimeHold,
}

/// Either a runtime this session owns, or a handle to one it does not.
#[derive(Debug)]
enum RuntimeHold {
    /// Never read: it is held so that dropping the session shuts the runtime
    /// down, which is what stops the background prefetch tasks.
    Owned(#[expect(dead_code, reason = "held for its Drop")] tokio::runtime::Runtime),
    Borrowed,
}

/// A resolved dependency graph, with everything worth reporting alongside it.
#[derive(Debug)]
pub struct Resolution {
    pub collected: Collected,
    /// Non-fatal problems: unreadable metadata, checksum mismatches under a
    /// `warn` policy, POM problems. Shown once, after the tree.
    pub warnings: Vec<String>,
}

impl Session {
    /// Builds a session that owns its own tokio runtime.
    ///
    /// This is what the CLI uses. Call it from the main thread.
    pub fn new(config: &Config) -> Result<Self, DriverError> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .thread_name("jv-io")
            .build()
            .map_err(|source| DriverError::Io {
                path: std::path::PathBuf::from("<tokio runtime>"),
                source,
            })?;
        let source = build_source(config, runtime.handle().clone())?;
        Ok(Self {
            source,
            _runtime: RuntimeHold::Owned(runtime),
        })
    }

    /// Builds a session on an existing runtime.
    ///
    /// The caller is responsible for not driving it from a worker thread.
    pub fn on_runtime(
        config: &Config,
        runtime: tokio::runtime::Handle,
    ) -> Result<Self, DriverError> {
        Ok(Self {
            source: build_source(config, runtime)?,
            _runtime: RuntimeHold::Borrowed,
        })
    }

    /// The repository source, for callers that need it directly.
    pub fn source(&self) -> &RepositorySource {
        &self.source
    }

    /// Loads the project governing a directory.
    pub fn project(&self, directory: &Path) -> Result<Project, DriverError> {
        let pom =
            find_pom(directory).ok_or_else(|| DriverError::NoProject(directory.to_owned()))?;
        load_project(&self.source, &pom)
    }

    /// Loads a project from a specific `pom.xml`.
    pub fn project_at(&self, pom: &Path) -> Result<Project, DriverError> {
        load_project(&self.source, pom)
    }

    /// Resolves a project's dependencies.
    pub fn resolve_project(
        &self,
        project: &Project,
        verbosity: Verbosity,
    ) -> Result<Resolution, DriverError> {
        self.resolve_request(&project.collect_request(), verbosity)
    }

    /// Resolves one artifact's dependencies, as `jvx` and `jv add` need.
    ///
    /// The artifact goes in as a *dependency* rather than as the root project,
    /// which is what stops its own `test` dependencies from entering the graph.
    pub fn resolve_artifact(
        &self,
        artifact: &Artifact,
        verbosity: Verbosity,
    ) -> Result<Resolution, DriverError> {
        let request = CollectRequest {
            root_dependency: Some(Dependency {
                group_id: artifact.group_id.clone(),
                artifact_id: artifact.artifact_id.clone(),
                version: Some(artifact.version.clone()),
                classifier: (!artifact.classifier.is_empty()).then(|| artifact.classifier.clone()),
                type_: Some(artifact.extension.clone()),
                ..Dependency::default()
            }),
            ..CollectRequest::default()
        };
        self.resolve_request(&request, verbosity)
    }

    /// Collects and resolves an arbitrary request.
    pub fn resolve_request(
        &self,
        request: &CollectRequest,
        verbosity: Verbosity,
    ) -> Result<Resolution, DriverError> {
        // Start the POM crawler before collection, not during: its whole value
        // is being a level or more ahead, and the root's own model build is
        // several round trips during which the network is otherwise idle.
        self.source.prefetch_from(&request.dependencies);
        let mut collected = collect(&self.source, request, self.source.types())?;
        resolve_conflicts(&mut collected.graph, verbosity)?;
        Ok(Resolution {
            collected,
            warnings: self.warnings(),
        })
    }

    /// Collects without resolving conflicts, for callers that want the raw graph.
    pub fn collect_request(&self, request: &CollectRequest) -> Result<Collected, DriverError> {
        Ok(collect(&self.source, request, self.source.types())?)
    }

    /// The repositories in play, in the order they are consulted.
    pub fn repositories(&self) -> Vec<Repository> {
        self.source.repositories()
    }

    /// Everything non-fatal seen so far.
    pub fn warnings(&self) -> Vec<String> {
        self.source.warnings()
    }
}

fn build_source(
    config: &Config,
    runtime: tokio::runtime::Handle,
) -> Result<RepositorySource, DriverError> {
    let settings = config.load_settings()?;
    // `settings.xml` can put the whole build offline, and a flag can too. Either
    // one is enough.
    let offline = config.offline || settings.is_offline();

    let store = match &config.cache {
        Some(path) => Store::new(path),
        None => Store::default_location().ok_or(DriverError::NoCacheDirectory)?,
    };

    // Offline mode gets a transport that refuses everything rather than one that
    // merely isn't called: a code path that forgot to check cannot then reach the
    // network by accident.
    let transport: Box<dyn Transport> = if offline {
        Box::new(OfflineTransport)
    } else {
        Box::new(
            HttpTransport::with_proxies(jv_repo::ProxySelector::from_settings(&settings))
                .map_err(|error| DriverError::Other(error.to_string()))?,
        )
    };

    let mut fetcher = Fetcher::new(store, transport).offline(offline);
    if !config.ignore_local_repository {
        let local = config
            .local_repository
            .clone()
            .or_else(|| jv_cache::maven_local_repository(&settings));
        if let Some(local) = local {
            fetcher = fetcher.with_local_repository(local);
        }
    }

    let mut context = BuildContext::from_environment();
    // Without this, every `<jdk>` activator in every POM silently fails to
    // match, and jv builds a different effective model than Maven does on the
    // same machine.
    if let Some(version) = config
        .java_version
        .clone()
        .or_else(crate::java::detect_version)
    {
        context = context.with_java_version(version);
    }
    for (key, value) in &config.user_properties {
        context.user_properties.insert(key.clone(), value.clone());
    }
    context.active_profiles = config.active_profiles.clone();
    // `<activeProfiles>` in settings.xml turns a profile on unconditionally, and
    // it is how most people attach a corporate repository. It was being parsed
    // and then ignored, so such a repository was never contacted and its
    // artifacts came back "not in any configured repository".
    for id in &settings.active_profiles {
        if !context.active_profiles.contains(id) {
            context.active_profiles.push(id.clone());
        }
    }
    context.inactive_profiles = config.inactive_profiles.clone();

    let settings = Arc::new(settings);
    // Central unless told otherwise; the super POM declares it, and every POM
    // read afterwards may add more.
    let declared = config
        .repositories
        .clone()
        .unwrap_or_else(|| vec![Repository::central()]);
    let source = RepositorySource::new(
        Arc::new(fetcher),
        runtime,
        Arc::clone(&settings),
        context,
        &[],
    );

    let source = source
        .with_insecure_http(config.allow_insecure_http)
        .with_forced_update(config.update)
        .with_lifecycle_bindings(config.lifecycle_bindings);
    // Applied after the switch is set, so the starting repositories are filtered
    // by it too rather than only the ones POMs add later.
    source.add_repositories(&declared);
    Ok(source)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_offline_session_resolves_nothing_it_has_not_cached() {
        let cache = tempfile::tempdir().unwrap();
        let config = Config {
            cache: Some(cache.path().to_path_buf()),
            user_settings: None,
            offline: true,
            ..Config::new().without_local_repository()
        };
        let session = Session::new(&config).expect("a session");
        let artifact = Artifact::new("org.example", "absent", "1.0");
        let resolution = session
            .resolve_artifact(&artifact, Verbosity::None)
            .expect("collection tolerates a missing descriptor");
        // Maven carries on past a POM it cannot read, leaving a childless node,
        // and so does jv — the failure surfaces when the jar is needed, not here.
        assert_eq!(resolution.collected.graph.len(), 1);
    }
}
