//! Reading POMs and descriptors out of repositories.
//!
//! # Why this is synchronous
//!
//! `jv-model-builder` and `jv-resolver` are pure and synchronous on purpose: the
//! parent chain is serial by nature, and the collector's breadth-first walk is
//! far easier to keep faithful to Maven when it is straight-line code. Only the
//! I/O beneath them wants to be concurrent. So this module is the seam: it
//! presents the synchronous [`ModelSource`] and [`DescriptorSource`] the pure
//! crates expect, and gets its bytes by blocking on the async fetcher.
//!
//! The concurrency is not lost, it is moved. Every time a descriptor is read,
//! the POMs of everything it depends on are speculatively fetched in the
//! background. By the time the collector's walk reaches the next level, those
//! downloads have usually already landed in the cache, so the blocking call
//! returns a cache hit. This is what makes a cold resolve fast without making
//! the resolution logic asynchronous.
//!
//! # Blocking rules
//!
//! [`Session`](crate::Session) must be driven from a thread that is *not* a
//! tokio worker, because `Handle::block_on` panics on one. The CLI satisfies
//! this by building a runtime in `main` and never entering it.
//!
//! # Known divergence: repository scope
//!
//! Maven scopes `<repositories>` per node — a dependency's declared repositories
//! apply to its own subtree. [`DescriptorSource`] has no node context to hang
//! that on, so jv accumulates discovered repositories into one ordered list that
//! every later fetch sees. In practice this finds strictly more artifacts than
//! Maven, never fewer, but it does mean a repository declared deep in the graph
//! can serve a sibling subtree that Maven would not have offered it to. Recorded
//! in `ROADMAP.md` rather than hidden here.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::Path;
use std::sync::{Arc, Mutex, RwLock};

use jv_cache::{Fetcher, Origin};
use jv_model::{
    Artifact, Dependency, Metadata, Model, Settings, TypeRegistry, is_snapshot_version,
    parse_metadata, parse_pom,
};
use jv_model_builder::{BuildContext, ModelBuilder, ModelSource, SourcedModel};
use jv_repo::{
    MetadataLocation, Repository, Trust, artifact_path, resolve_repositories, resolve_with_trust,
};
use jv_resolver::{Descriptor, DescriptorSource};
use jv_version::Version;

use crate::error::DriverError;

/// How many artifact downloads `materialize_all` keeps in flight.
///
/// A cold sync is latency-bound rather than bandwidth-bound: measured on
/// spring-petclinic, 3,175 requests of which 95% are under 64 KB and together
/// only 11.5% of the bytes, against a 137 ms time to first byte. That argues
/// for more concurrency.
///
/// It is capped at 32 anyway, because the other end gets a say. Pushing harder
/// earned HTTP 429s from Maven Central, and a throttled sync is slower than a
/// polite one *and* fails: before this was understood, a 429 while reading a
/// parent POM failed the whole resolve. Maven's own resolver defaults to five
/// connections; 32 is already well past that.
///
/// `JV_IN_FLIGHT` overrides it, because the right value depends on the
/// network — a nearby mirror with no rate limit will take far more than a
/// shared runner talking to Central.
const MATERIALIZE_IN_FLIGHT: usize = 32;

/// The bound, overridable with `JV_IN_FLIGHT`.
///
/// A knob rather than a constant because the right value depends on the
/// network: a cold sync is latency-bound on many small POMs, so a high-latency
/// link wants more in flight, while a nearby mirror wants fewer. It also makes
/// the value measurable, which is how the default was chosen.
fn in_flight() -> usize {
    std::env::var("JV_IN_FLIGHT")
        .ok()
        .and_then(|value| value.parse().ok())
        .filter(|value| *value > 0)
        .unwrap_or(MATERIALIZE_IN_FLIGHT)
}

/// The extension a POM is stored under, which is what every descriptor read
/// actually asks the repository for.
const POM: &str = "pom";

/// Repositories to consult, in order, growing as POMs declare more.
///
/// Shared because both the model source and the descriptor source read it, and
/// the descriptor source appends to it.
#[derive(Debug, Default)]
struct Repositories {
    ordered: Vec<Repository>,
}

impl Repositories {
    /// Adds repositories a POM declared, ignoring ids already known.
    ///
    /// Ignoring by id rather than by URL matches Maven: two POMs declaring `id`
    /// with different URLs is a project bug, and the first declaration wins.
    fn extend(&mut self, discovered: Vec<Repository>) {
        for repository in discovered {
            if !self.ordered.iter().any(|held| held.id == repository.id) {
                self.ordered.push(repository);
            }
        }
    }
}

/// Fetched `maven-metadata.xml`, keyed by (repository path, repository id).
type RangeMetadata = HashMap<(String, String), Vec<u8>>;

/// Reads POMs from repositories and from disk.
///
/// Cloneable and shareable: the caches are behind `Arc`, so a prefetch task and
/// the collector share both the memo table and the discovered repository list.
#[derive(Clone)]
pub struct RepositorySource {
    fetcher: Arc<Fetcher>,
    runtime: tokio::runtime::Handle,
    settings: Arc<Settings>,
    context: BuildContext,
    types: Arc<TypeRegistry>,
    repositories: Arc<RwLock<Repositories>>,
    /// POMs parsed once, by `g:a:v`. `None` records a coordinate no repository
    /// has, so an absence is not re-requested either.
    ///
    /// Shared with the crawler, which fills it in from background threads — see
    /// `prefetch.rs`.
    poms: Arc<RwLock<HashMap<String, Option<Arc<Model>>>>>,
    /// Built descriptors by `g:a:v`, which is the expensive half.
    descriptors: Arc<RwLock<HashMap<String, Descriptor>>>,
    /// Version lists by `g:a`, for ranges.
    versions: Arc<RwLock<HashMap<String, Vec<String>>>>,
    /// Repositories that could not be reached, so they are asked once.
    ///
    /// A dead repository named in a transitive POM — `maven.java.net` is the
    /// classic — costs a full connection timeout on *every* lookup that
    /// consults it, and version-range resolution consults every repository in
    /// scope for every ranged artifact. Ignoring the failure without recording
    /// it turned one dead host into minutes of waiting.
    unreachable: Arc<Mutex<HashSet<String>>>,
    /// Artifact-level `maven-metadata.xml` as fetched, by (path, repository id).
    ///
    /// Kept so `jv sync` can place it. Maven re-resolves a version range at
    /// build time and reads this file to do it, so a repository holding the
    /// jar a range selected but not the metadata behind it fails offline with
    /// "No versions available … within specified range" — which names neither
    /// the file nor the reason.
    ///
    /// Only the artifact-level file is recorded. Version-level metadata is the
    /// snapshot case, and `LocalSnapshot` already writes the
    /// `maven-metadata-local.xml` that Maven accepts from any configuration.
    range_metadata: Arc<Mutex<RangeMetadata>>,
    /// Resolved timestamped versions by `g:a:baseVersion`.
    snapshots: Arc<Mutex<HashMap<String, String>>>,
    /// POMs of projects loaded from disk, by `g:a:v`. Consulted before any
    /// repository, so a dependency on a sibling module in a multi-module build
    /// resolves against the working tree rather than against a repository that
    /// has never heard of it.
    reactor: Arc<RwLock<HashMap<String, String>>>,
    /// Warnings gathered along the way, to show once at the end.
    warnings: Arc<Mutex<Vec<String>>>,
    /// Crawls POMs ahead of collection. See `prefetch.rs`.
    prefetcher: crate::prefetch::Prefetcher,
    /// An update policy forced on every repository, as Maven's `-U` does. It
    /// applies to repositories discovered later too, which is why it lives here
    /// rather than being baked into the list once.
    forced_update: Option<jv_repo::UpdatePolicy>,
    /// Whether effective models should carry the lifecycle's plugins. Only
    /// `jv sync` wants them.
    lifecycle_bindings: bool,
    /// Whether plaintext HTTP repositories may be contacted.
    allow_insecure_http: bool,
}

impl std::fmt::Debug for RepositorySource {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RepositorySource")
            .field("repositories", &self.repositories)
            .field("prefetcher", &self.prefetcher)
            .finish_non_exhaustive()
    }
}

impl RepositorySource {
    /// Builds a source over a fetcher.
    ///
    /// `declared` is the repository list to start from — normally just Maven
    /// Central, since the super POM supplies it — and is put through the
    /// settings' mirrors before anything is contacted.
    pub fn new(
        fetcher: Arc<Fetcher>,
        runtime: tokio::runtime::Handle,
        settings: Arc<Settings>,
        context: BuildContext,
        declared: &[Repository],
    ) -> Self {
        let sink = crate::prefetch::Sink::default();
        Self {
            prefetcher: crate::prefetch::Prefetcher::new(
                Arc::clone(&fetcher),
                runtime.clone(),
                sink.clone(),
                true,
            ),
            poms: Arc::clone(&sink.poms),
            warnings: Arc::clone(&sink.warnings),
            fetcher,
            runtime,
            repositories: Arc::new(RwLock::new(Repositories {
                ordered: resolve_repositories(declared, &settings),
            })),
            settings,
            context,
            types: Arc::new(TypeRegistry::default()),
            descriptors: Arc::default(),
            versions: Arc::default(),
            range_metadata: Arc::default(),
            unreachable: Arc::default(),
            snapshots: Arc::default(),
            reactor: Arc::default(),
            forced_update: None,
            lifecycle_bindings: false,
            allow_insecure_http: false,
        }
    }

    /// Allows plaintext HTTP repositories.
    pub fn with_insecure_http(mut self, allowed: bool) -> Self {
        self.allow_insecure_http = allowed;
        self
    }

    /// Injects the plugins the packaging's lifecycle binds into every model this
    /// source builds.
    pub fn with_lifecycle_bindings(mut self, enabled: bool) -> Self {
        self.lifecycle_bindings = enabled;
        self
    }

    /// Whether lifecycle bindings are injected.
    pub fn lifecycle_bindings(&self) -> bool {
        self.lifecycle_bindings
    }

    /// Forces an update policy on every repository, present and future.
    pub fn with_forced_update(self, update: Option<jv_repo::UpdatePolicy>) -> Self {
        let source = Self {
            forced_update: update,
            ..self
        };
        if update.is_some() {
            let mut repositories = source.repositories.write().expect("repositories");
            let existing = std::mem::take(&mut repositories.ordered);
            repositories.ordered = existing
                .into_iter()
                .map(|repository| source.apply_forced_update(repository))
                .collect();
        }
        source
    }

    fn apply_forced_update(&self, mut repository: Repository) -> Repository {
        if let Some(update) = self.forced_update {
            repository.releases.update = update;
            repository.snapshots.update = update;
        }
        repository
    }

    /// Turns off speculative prefetching.
    ///
    /// Only useful for tests that count requests, where background work would
    /// make the count depend on timing.
    pub fn without_prefetch(self) -> Self {
        let prefetcher = crate::prefetch::Prefetcher::new(
            Arc::clone(&self.fetcher),
            self.runtime.clone(),
            crate::prefetch::Sink {
                poms: Arc::clone(&self.poms),
                warnings: Arc::clone(&self.warnings),
            },
            false,
        );
        Self { prefetcher, ..self }
    }

    /// The repositories currently known, in the order they are consulted.
    /// Where jv's own cache lives, for callers that keep something alongside it.
    pub fn cache_root(&self) -> &Path {
        self.fetcher.store().root()
    }

    /// Whether repositories will be contacted at all.
    pub fn is_offline(&self) -> bool {
        self.fetcher.is_offline()
    }

    /// The update policy forced over every repository, as `-U` does.
    pub fn forced_update(&self) -> Option<jv_repo::UpdatePolicy> {
        self.forced_update
    }

    pub fn repositories(&self) -> Vec<Repository> {
        self.repositories
            .read()
            .expect("repositories")
            .ordered
            .clone()
    }

    /// Adds repositories the user configured or declared themselves.
    pub fn add_repositories(&self, declared: &[Repository]) {
        self.add_with_trust(declared, Trust::Configured);
    }

    /// Adds repositories, recording whether the declaration is the user's.
    fn add_with_trust(&self, declared: &[Repository], trust: Trust) {
        let mut resolved = Vec::new();
        for repository in resolve_with_trust(declared, &self.settings, trust) {
            // A scheme jv has no client for. Maven reaches these through wagons
            // supplied by build extensions, which jv does not load — jetty's
            // parent declares `mavengem:https://rubygems.org`. Skipped with a
            // warning rather than attempted: attempting it failed the whole
            // sync, so a repository jv merely cannot use took down artifacts
            // every other repository could serve.
            if !repository.is_supported() {
                self.warn(format!(
                    "{} ({}) uses a scheme jv has no client for and was skipped; \
                     Maven reaches it through a build extension, which jv does not load",
                    repository.id, repository.url
                ));
                continue;
            }
            if repository.is_insecure() && !self.allow_insecure_http {
                // Blocked rather than dropped, so the repository still appears in
                // `jv tree`'s reasoning and the message says what to do.
                self.warn(format!(
                    "{} ({}) is plaintext HTTP and was blocked; pass \
                     --allow-insecure-http to contact it anyway",
                    repository.id, repository.url
                ));
                continue;
            }
            resolved.push(self.apply_forced_update(repository));
        }
        self.repositories
            .write()
            .expect("repositories")
            .extend(resolved);
    }

    /// Everything worth telling the user that did not stop the resolve.
    ///
    /// Sorted, because the order they were discovered in is not stable: plugin
    /// closures resolve in parallel, so which thread first reads the POM that
    /// declares a blocked repository varies from run to run. Two identical runs
    /// were printing the same warnings in three different orders, which turns
    /// every CI log comparison into noise. Sorting also groups warnings of the
    /// same kind, since they share a prefix.
    pub fn warnings(&self) -> Vec<String> {
        let mut warnings = self.warnings.lock().expect("warnings").clone();
        warnings.sort();
        warnings
    }

    /// The build context profile activation and interpolation run against.
    pub fn context(&self) -> &BuildContext {
        &self.context
    }

    /// The `settings.xml` profiles, which contribute properties and repositories.
    pub fn settings_profiles(&self) -> &[jv_model::SettingsProfile] {
        &self.settings.profiles
    }

    /// Registers a POM read from the working tree.
    ///
    /// Everything afterwards sees these before any repository, which is what
    /// makes a multi-module build resolve its own modules.
    pub fn register_reactor_pom(
        &self,
        group_id: &str,
        artifact_id: &str,
        version: &str,
        pom: String,
    ) {
        self.reactor
            .write()
            .expect("reactor")
            .insert(format!("{group_id}:{artifact_id}:{version}"), pom);
    }

    /// Records a warning to show once, at the end.
    pub fn record_warning(&self, message: impl Into<String>) {
        self.warn(message);
    }

    /// Records the repositories the project being built declares.
    ///
    /// Trusted, unlike a dependency's: this POM is the user's own.
    pub fn register_project_repositories(&self, model: &Model) {
        let declared = declared_repositories(model);
        if !declared.is_empty() {
            self.add_with_trust(&declared, Trust::Configured);
        }
    }

    fn warn(&self, message: impl Into<String>) {
        let message = message.into();
        let mut warnings = self.warnings.lock().expect("warnings");
        // Repeating the same warning once per node would bury everything else;
        // a resolve touches the same repository hundreds of times.
        if !warnings.contains(&message) {
            warnings.push(message);
        }
    }

    /// Fetches an artifact's bytes, blocking.
    fn bytes(&self, artifact: &Artifact) -> Result<Option<Vec<u8>>, DriverError> {
        let repositories = self.repositories();
        let fetched = self
            .runtime
            .block_on(self.fetcher.artifact(&repositories, artifact));
        match fetched {
            Ok(fetched) => {
                for warning in &fetched.warnings {
                    self.warn(warning.clone());
                }
                Ok(Some(fetched.bytes))
            }
            Err(jv_cache::FetchError::NotFound { .. }) => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    /// A POM, parsed once and shared.
    ///
    /// Memoizing the *parsed* model rather than its text is worth a good deal
    /// more than it looks. A parent POM is read once per child that inherits from
    /// it — `spring-boot-dependencies` by every Spring module in the graph — and
    /// the old memo handed back a fresh `String` copy of the file each time for
    /// the caller to re-parse. Parsing several hundred kilobytes of XML fifty
    /// times over was the largest cost of a warm resolve after JVM startup.
    fn cached_pom(&self, artifact: &Artifact) -> Result<Option<Arc<Model>>, DriverError> {
        let key = coordinates(artifact);

        // The reactor is consulted *before* the memo, not after. The crawler
        // writes into that memo from background threads and knows nothing about
        // the working tree, so with the checks the other way round a published
        // sibling module could beat the one being built — decided by whichever
        // won a race, which made it appear only on projects with enough
        // dependencies to give the crawler a head start.
        let from_reactor = self.reactor.read().expect("reactor").get(&key).cloned();
        if from_reactor.is_none() {
            if let Some(cached) = self.poms.read().expect("poms").get(&key) {
                return Ok(cached.clone());
            }
        }

        let text = match from_reactor {
            Some(text) => Some(text),
            None => {
                let pom_artifact = Artifact {
                    classifier: String::new(),
                    extension: POM.to_owned(),
                    version: self.resolved_version(artifact)?,
                    ..artifact.clone()
                };
                self.bytes(&pom_artifact)?
                    .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
            }
        };

        let parsed = match &text {
            Some(text) => {
                let parsed = parse_pom(text).map_err(|source| DriverError::Pom {
                    source_name: key.clone(),
                    source,
                })?;
                // Reported here, and only here: the memo means this runs once per
                // POM however many times it is inherited from. Discarding them —
                // which is what happened before — hid every problem the parser
                // knows how to report.
                for warning in parsed.warnings {
                    self.warn(format!("{key}: {warning}"));
                }
                Some(Arc::new(parsed.model))
            }
            None => None,
        };

        self.poms.write().expect("poms").insert(key, parsed.clone());
        Ok(parsed)
    }

    /// The concrete version to request from a repository.
    ///
    /// For a release this is the version itself. For a `-SNAPSHOT` it is the
    /// timestamped build the repository's metadata currently points at, because
    /// that is the file name on disk — the *directory* keeps the `-SNAPSHOT`
    /// spelling, which [`artifact_path`] already knows.
    fn resolved_version(&self, artifact: &Artifact) -> Result<String, DriverError> {
        if !is_snapshot_version(&artifact.version) {
            return Ok(artifact.version.clone());
        }
        let key = coordinates(artifact);
        if let Some(cached) = self.snapshots.lock().expect("snapshots").get(&key) {
            return Ok(cached.clone());
        }

        let location = MetadataLocation::Version {
            group_id: &artifact.group_id,
            artifact_id: &artifact.artifact_id,
            version: &artifact.version,
        };
        // The newest build across repositories wins, which is what Maven's
        // metadata merge amounts to for a single snapshot.
        let resolved = self
            .metadata(&location.path(), &artifact.version)?
            .into_iter()
            .filter_map(|metadata| metadata.snapshot_version(POM, ""))
            .max_by(|left, right| Version::parse(left).cmp(&Version::parse(right)))
            .unwrap_or_else(|| artifact.version.clone());

        self.snapshots
            .lock()
            .expect("snapshots")
            .insert(key, resolved.clone());
        Ok(resolved)
    }

    /// Reads one metadata path from every repository that has it.
    ///
    /// Unlike an artifact, metadata is *merged* across repositories rather than
    /// taken from the first hit: each repository knows only about the versions it
    /// holds, and a range must see all of them.
    fn metadata(&self, path: &str, version_hint: &str) -> Result<Vec<Metadata>, DriverError> {
        self.metadata_recording(path, version_hint, false)
    }

    /// As [`Self::metadata`], optionally keeping the bytes for `jv sync`.
    fn metadata_recording(
        &self,
        path: &str,
        version_hint: &str,
        record: bool,
    ) -> Result<Vec<Metadata>, DriverError> {
        let all = self.repositories();
        // A repository already known to be unreachable is not asked again.
        let repositories: Vec<_> = {
            let unreachable = self.unreachable.lock().expect("unreachable");
            all.iter()
                .filter(|repository| !unreachable.contains(&repository.url))
                .cloned()
                .collect()
        };
        // All at once, not one after another. This sits on the synchronous
        // critical path — every version range and every snapshot resolution goes
        // through it — so asking three repositories in turn spent three round
        // trips where one would do, and unlike a POM fetch there is no
        // prefetcher running ahead to hide them.
        let fetched: Vec<_> =
            self.runtime
                .block_on(futures_util::future::join_all(repositories.iter().map(
                    |repository| {
                        self.fetcher
                            .optional(std::slice::from_ref(repository), path, version_hint)
                    },
                )));

        let mut found = Vec::new();
        // Whether any repository actually answered — a 404 counts, a refused
        // connection does not. Ignoring an unreachable repository is right when
        // another can answer; when *none* can, an empty result is not "this
        // artifact is unpublished", it is "nobody was asked". Reporting the
        // first as the second is how a rate-limited machine came back with
        // "no published version of com.google.googlejavaformat:google-java-format
        // was found" for an artifact that has been on Central for years.
        let mut consulted = 0usize;
        // Why each repository refused, kept here rather than only in the
        // warnings bag. Warnings are printed after a resolve finishes, so a
        // resolve that *fails* never shows them — the error below used to say
        // "the failures are reported above" when nothing had been reported at
        // all, leaving a rate-limited run looking like a missing artifact.
        let mut refusals: Vec<String> = Vec::new();
        for (repository, fetched) in repositories.iter().zip(fetched) {
            // An unreachable repository must not stop a resolve the others can
            // complete — the same rule the corrupt-metadata arm below already
            // follows, and the one Maven follows.
            //
            // This used to propagate. A single dead repository declared by some
            // transitive POM — `maven.java.net` is the classic, offline for
            // years and still named in POMs on Central — failed the whole
            // version-range resolution, which in `jv sync` abandoned every
            // dependency of the plugin that needed it. The build then failed
            // offline on a list of artifacts with nothing to connect them to a
            // repository nobody meant to use.
            let fetched = match fetched {
                Ok(fetched) => fetched,
                Err(error) => {
                    // Warned once, then never asked again for the rest of the
                    // session: the next range would otherwise pay the same
                    // timeout, and there are many ranges.
                    if self
                        .unreachable
                        .lock()
                        .expect("unreachable")
                        .insert(repository.url.clone())
                    {
                        self.warn(format!(
                            "{} could not be reached and will be skipped: {error}",
                            repository.url
                        ));
                    }
                    refusals.push(format!("{}: {error}", repository.url));
                    continue;
                }
            };
            consulted += 1;
            let Some(fetched) = fetched else { continue };
            match parse_metadata(&String::from_utf8_lossy(&fetched.bytes)) {
                Ok(metadata) => {
                    if record {
                        self.range_metadata.lock().expect("range metadata").insert(
                            (path.to_owned(), repository.id.clone()),
                            fetched.bytes.clone(),
                        );
                    }
                    found.push(metadata);
                }
                // Corrupt metadata in one repository must not stop a resolve that
                // another repository can complete.
                Err(error) => self.warn(format!(
                    "{}/{path} is not readable metadata and was ignored: {error}",
                    repository.url
                )),
            }
        }
        // Every repository refused. See `consulted` above.
        if consulted == 0 && !repositories.is_empty() {
            return Err(DriverError::Other(format!(
                "{path} could not be read from any of the {} configured repositories, so it is \
                 unknown whether the artifact exists:\n{}",
                repositories.len(),
                refusals
                    .iter()
                    .map(|refusal| format!("  {refusal}"))
                    .collect::<Vec<_>>()
                    .join("\n")
            )));
        }
        Ok(found)
    }

    /// Builds the effective model for an artifact, or `None` when no repository
    /// has its POM.
    pub fn effective_model(&self, artifact: &Artifact) -> Result<Option<Model>, DriverError> {
        let Some(model) = self.cached_pom(artifact)? else {
            return Ok(None);
        };
        let source_name = coordinates(artifact);

        let built = ModelBuilder::new(self, self.context.clone())
            .with_settings_profiles(&self.settings.profiles)
            .with_lifecycle_bindings(self.lifecycle_bindings)
            .build(SourcedModel::new((*model).clone(), source_name.clone()))
            .map_err(|source| DriverError::Model {
                source_name,
                source,
            })?;

        for problem in built.errors() {
            // The message alone: `Problem`'s own `Display` prefixes a severity,
            // and the CLI adds one too, so using it here reads as "warning:
            // error: ...".
            self.warn(format!("{} ({})", problem.message, problem.source));
        }
        self.register_repositories(&built.model);
        Ok(Some(built.model))
    }

    /// Every POM this source has successfully read.
    ///
    /// `jv sync` needs this and nothing else needs it. Maven re-reads every POM
    /// in the local repository and walks its parents and its imported BOMs, so a
    /// jar whose grandparent POM or whose surefire BOM is absent fails to
    /// resolve offline even though the jar itself is right there. jv already
    /// fetched every one of those POMs during resolution — the model builder
    /// reaches parents and BOMs through the same `ModelSource::get` that fills
    /// this memo — so the memo is exactly the set Maven will look for, and is a
    /// superset of any per-artifact parent walk.
    /// The published versions of an artifact, recording the metadata behind
    /// them so `jv sync` can place it.
    ///
    /// The same lookup the range resolver uses; exposed because `jv sync` needs
    /// it for a plugin whose version Maven resolves at execution time.
    pub fn published_versions(
        &self,
        group_id: &str,
        artifact_id: &str,
    ) -> Result<Vec<String>, String> {
        <Self as DescriptorSource>::versions(self, group_id, artifact_id)
    }

    /// The version Maven would choose for a plugin declared without one.
    ///
    /// A `<plugin>` with no `<version>` is legal, and Maven resolves one at
    /// build time from the artifact's `maven-metadata.xml`: `<release>` if the
    /// file has one, otherwise `<latest>`, otherwise the greatest version
    /// listed. jv used to warn and sync nothing, which left `mvn -o` to fail
    /// with "Error resolving version for plugin" — three corpus projects died
    /// that way, on `maven-javadoc-plugin`, `maven-install-plugin` and
    /// `javancss-maven-plugin`.
    ///
    /// Reading the metadata also records it, so the same file is placed into the
    /// local repository and Maven reaches this answer for itself rather than
    /// taking jv's word for it. That matters: if the two ever disagree, Maven
    /// wins, and it can only win if the file is there.
    pub fn plugin_version(
        &self,
        group_id: &str,
        artifact_id: &str,
    ) -> Result<Option<String>, DriverError> {
        let location = MetadataLocation::Artifact {
            group_id,
            artifact_id,
        };
        let metadata = self.metadata_recording(&location.path(), "", true)?;

        let mut release: Option<String> = None;
        let mut latest: Option<String> = None;
        let mut greatest: Option<String> = None;
        for entry in &metadata {
            if let Some(versioning) = &entry.versioning {
                take_greater(&mut release, versioning.release.as_deref());
                take_greater(&mut latest, versioning.latest.as_deref());
            }
            for version in entry.versions() {
                take_greater(&mut greatest, Some(version));
            }
        }
        Ok(release.or(latest).or(greatest))
    }

    /// Fetches and records the version list for every coordinate any POM jv
    /// read names with a *range*.
    ///
    /// Resolving a range records its metadata, but that only covers the ranges
    /// jv itself had to resolve. Maven re-resolves the whole plugin classpath
    /// on its own terms and can reach a range down a path jv never took —
    /// bouncycastle's POMs cross-reference each other with `[1.81,1.82)`, and
    /// which of them jv expands depends on which versions won. A repository
    /// missing one of those files fails offline with "No versions available …
    /// within specified range", which names neither the file nor the reason.
    ///
    /// So this sweeps the parsed POMs rather than relying on jv's own path.
    /// Fetching a version list already read is a memo hit, so the sweep costs
    /// requests only for coordinates jv genuinely never looked up.
    pub fn fetch_ranged_metadata(&self) {
        let ranged: BTreeSet<(String, String)> = {
            let poms = self.poms.read().expect("poms");
            poms.values()
                .flatten()
                .flat_map(|model| {
                    model
                        .dependencies
                        .iter()
                        .chain(model.dependency_management.iter())
                })
                .filter(|dependency| {
                    dependency
                        .version
                        .as_deref()
                        .is_some_and(|version| version.starts_with('[') || version.starts_with('('))
                })
                .map(|dependency| (dependency.group_id.clone(), dependency.artifact_id.clone()))
                .collect()
        };
        for (group_id, artifact_id) in ranged {
            // Failure is already reported by the fetch itself, and a version
            // list jv cannot get is not a reason to fail the sync.
            let _ = self.versions(&group_id, &artifact_id);
        }
    }

    /// Artifact-level metadata read during resolution, for `jv sync` to place.
    ///
    /// Returned as `(repository path, repository id, bytes)`, where the path
    /// still ends in `maven-metadata.xml` — the caller renames it to the
    /// `-<id>` form Maven looks for, because only the caller knows whether it
    /// is writing into a local repository at all.
    pub fn read_range_metadata(&self) -> Vec<(String, String, Vec<u8>)> {
        self.range_metadata
            .lock()
            .expect("range metadata")
            .iter()
            .map(|((path, id), bytes)| (path.clone(), id.clone(), bytes.clone()))
            .collect()
    }

    pub fn read_poms(&self) -> Vec<Artifact> {
        self.poms
            .read()
            .expect("poms")
            .iter()
            .filter(|(_, parsed)| parsed.is_some())
            .filter_map(|(key, _)| parse_coordinates(key))
            .collect()
    }

    /// Records the repositories a *dependency's* POM declares.
    ///
    /// Contacted but never authenticated to — see [`Trust::Untrusted`]. A
    /// dependency four levels down should not be able to name an id the user has
    /// a password for and be handed it.
    fn register_repositories(&self, model: &Model) {
        let declared = declared_repositories(model);
        if !declared.is_empty() {
            self.add_with_trust(&declared, Trust::Untrusted);
        }
    }

    /// Starts background fetches for the POMs of a descriptor's dependencies.
    ///
    /// Fire-and-forget: a failure here is not reported, because the blocking read
    /// that follows will hit the same URL and report it properly. The only thing
    /// this is allowed to change is how long that read takes.
    /// Points the crawler at everything a descriptor leads to.
    ///
    /// The crawler follows parents and BOMs from here, so one call per
    /// descriptor is enough to keep it a level or more ahead of collection.
    fn prefetch_children(&self, dependencies: &[Dependency]) {
        let repositories = self.repositories();
        self.prefetcher.seed(
            &repositories,
            dependencies.iter().filter_map(|dependency| {
                Some(Artifact {
                    group_id: dependency.group_id.clone(),
                    artifact_id: dependency.artifact_id.clone(),
                    version: dependency.version.clone()?,
                    classifier: String::new(),
                    extension: POM.to_owned(),
                })
            }),
        );
    }

    /// Starts the crawler from a project's own dependencies.
    ///
    /// Called before collection begins, so the first level is already arriving
    /// while the root's own model is still being built.
    pub fn prefetch_from(&self, dependencies: &[Dependency]) {
        self.prefetch_children(dependencies);
    }

    /// Points the crawler at artifacts already known by coordinate.
    ///
    /// `jv sync` needs this for plugins. The crawler was only ever seeded from
    /// *dependencies*, so every plugin's POM chain was fetched cold — and since
    /// plugin dependencies are resolved one plugin at a time, those chains went
    /// out one after another. A project with twenty plugins paid twenty
    /// sequential POM walks before the first jar was requested.
    ///
    /// Seeding them all up front turns that into one concurrent crawl that
    /// overlaps the dependency resolve which is happening anyway.
    pub fn prefetch_artifacts(&self, artifacts: impl IntoIterator<Item = Artifact>) {
        let repositories = self.repositories();
        self.prefetcher.seed(
            &repositories,
            artifacts.into_iter().map(|artifact| Artifact {
                classifier: String::new(),
                extension: POM.to_owned(),
                ..artifact
            }),
        );
    }
}

impl ModelSource for RepositorySource {
    fn get(
        &self,
        group_id: &str,
        artifact_id: &str,
        version: &str,
    ) -> Result<SourcedModel, String> {
        let artifact = Artifact::new(group_id, artifact_id, version);
        let model = self
            .cached_pom(&artifact)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| {
                format!("{group_id}:{artifact_id}:{version} is not in any configured repository")
            })?;
        Ok(SourcedModel::new(
            (*model).clone(),
            format!("{group_id}:{artifact_id}:{version}"),
        ))
    }

    fn get_at_path(&self, path: &std::path::Path) -> Result<Option<SourcedModel>, String> {
        // A `<relativePath>` may name either a POM or the directory holding one,
        // and Maven accepts both.
        let file = if path.is_dir() {
            path.join("pom.xml")
        } else {
            path.to_path_buf()
        };
        let text = match std::fs::read_to_string(&file) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(format!("{}: {error}", file.display())),
        };
        let parsed = parse_pom(&text).map_err(|error| format!("{}: {error}", file.display()))?;
        let basedir = file.parent().map(std::path::Path::to_path_buf);
        let mut sourced = SourcedModel::new(parsed.model, file.display().to_string());
        sourced.basedir = basedir;
        Ok(Some(sourced))
    }
}

impl DescriptorSource for RepositorySource {
    fn descriptor(&self, artifact: &Artifact) -> Result<Descriptor, String> {
        let key = descriptor_key(artifact);
        if let Some(cached) = self.descriptors.read().expect("descriptors").get(&key) {
            return Ok(cached.clone());
        }

        let descriptor = self
            .read_descriptor(artifact, &mut Vec::new())
            .map_err(|error| error.to_string())?;
        self.descriptors
            .write()
            .expect("descriptors")
            .insert(key, descriptor.clone());
        self.prefetch_children(&descriptor.dependencies);
        Ok(descriptor)
    }

    fn versions(&self, group_id: &str, artifact_id: &str) -> Result<Vec<String>, String> {
        let key = format!("{group_id}:{artifact_id}");
        if let Some(cached) = self.versions.read().expect("versions").get(&key) {
            return Ok(cached.clone());
        }

        let location = MetadataLocation::Artifact {
            group_id,
            artifact_id,
        };
        // Recorded: this is the version list a range or `LATEST` resolves
        // against, and Maven needs the same file to resolve it again offline.
        let metadata = self
            .metadata_recording(&location.path(), "", true)
            .map_err(|error| error.to_string())?;

        let mut versions: Vec<String> = Vec::new();
        for entry in metadata {
            for version in entry.versions() {
                if !versions.iter().any(|held| held == version) {
                    versions.push(version.clone());
                }
            }
        }
        // Repositories append on deploy, so the file's order says nothing about
        // which version is greatest. Ranges need them sorted.
        versions.sort_by(|left, right| Version::parse(left).cmp(&Version::parse(right)));

        self.versions
            .write()
            .expect("versions")
            .insert(key, versions.clone());
        Ok(versions)
    }
}

impl RepositorySource {
    /// Reads a descriptor, following relocations.
    ///
    /// `seen` guards against a relocation cycle, which is rare but has happened
    /// in published POMs and would otherwise recurse forever.
    fn read_descriptor(
        &self,
        artifact: &Artifact,
        seen: &mut Vec<String>,
    ) -> Result<Descriptor, DriverError> {
        let key = coordinates(artifact);
        if seen.contains(&key) {
            self.warn(format!(
                "{key} relocates in a cycle; the relocation was ignored"
            ));
            return Ok(Descriptor {
                artifact: artifact.clone(),
                ..Default::default()
            });
        }
        seen.push(key);

        // A POM no repository has yields an empty descriptor rather than an
        // error, the way Maven carries on past an unreadable POM.
        let Some(model) = self.effective_model(artifact)? else {
            return Ok(Descriptor {
                artifact: artifact.clone(),
                ..Default::default()
            });
        };

        if let Some(target) = relocation_target(artifact, &model) {
            let mut relocated = self.read_descriptor(&target, seen)?;
            relocated.relocations.insert(0, artifact.clone());
            if let Some(message) = relocation_message(&model) {
                self.warn(format!("{}: {message}", coordinates(artifact)));
            }
            return Ok(relocated);
        }

        Ok(Descriptor {
            artifact: artifact.clone(),
            dependencies: model.dependencies,
            managed_dependencies: model.dependency_management,
            relocations: Vec::new(),
        })
    }

    /// The type registry the collector should use.
    pub fn types(&self) -> &TypeRegistry {
        &self.types
    }

    /// Where an artifact's file is, once resolved. Used by `jv sync` and by
    /// anything that reports paths.
    pub fn repository_path(&self, artifact: &Artifact) -> Result<String, DriverError> {
        let resolved = Artifact {
            version: self.resolved_version(artifact)?,
            ..artifact.clone()
        };
        Ok(artifact_path(&resolved))
    }

    /// Downloads many artifacts at once.
    ///
    /// `jv sync` used to call [`Self::materialize`] in a loop, and each call
    /// blocks on its own request — so a cold sync of four hundred artifacts
    /// paid four hundred sequential round trips, which was almost all of its
    /// wall clock. Resolution had long since been made concurrent; the
    /// download half had not.
    ///
    /// Results come back in the order asked for, so the caller's ordering,
    /// tracking-file writes and snapshot bookkeeping are unaffected — only the
    /// waiting is shared. Concurrency is bounded for the same reason the POM
    /// crawler bounds it: a few hundred simultaneous connections to one host
    /// is not faster, and is rude.
    pub fn materialize_all(
        &self,
        artifacts: &[Artifact],
    ) -> Result<Vec<Option<Materialized>>, DriverError> {
        if artifacts.is_empty() {
            return Ok(Vec::new());
        }

        // Snapshot version resolution reads metadata and memoises it, and doing
        // it inside the concurrent block would have every task racing for the
        // same lock on the same key. It is cheap and usually a cache hit.
        let resolved: Vec<Artifact> = artifacts
            .iter()
            .map(|artifact| {
                Ok(Artifact {
                    version: self.resolved_version(artifact)?,
                    ..artifact.clone()
                })
            })
            .collect::<Result<_, DriverError>>()?;

        let repositories = self.repositories();
        let permits = Arc::new(tokio::sync::Semaphore::new(in_flight()));
        let fetched = self
            .runtime
            .block_on(futures_util::future::join_all(resolved.iter().map(
                |artifact| {
                    let permits = Arc::clone(&permits);
                    let repositories = &repositories;
                    async move {
                        let _permit = permits
                            .acquire()
                            .await
                            .expect("the semaphore is not closed");
                        self.fetcher.locate(repositories, artifact).await
                    }
                },
            )));

        let mut materialized = Vec::with_capacity(fetched.len());
        for outcome in fetched {
            match outcome {
                Ok(found) => {
                    for warning in &found.warnings {
                        self.warn(warning.clone());
                    }
                    materialized.push(Some(Materialized {
                        origin: found.origin,
                        path: found.path,
                        repository: found.repository,
                    }));
                }
                // A 404 is the ordinary answer for an artifact a repository
                // does not carry; the caller reports it as missing.
                Err(jv_cache::FetchError::NotFound { .. }) => materialized.push(None),
                Err(error) => return Err(error.into()),
            }
        }
        Ok(materialized)
    }

    /// Downloads an artifact's own file.
    pub fn materialize(&self, artifact: &Artifact) -> Result<Option<Materialized>, DriverError> {
        let resolved = Artifact {
            version: self.resolved_version(artifact)?,
            ..artifact.clone()
        };
        let repositories = self.repositories();
        // `locate`, not `artifact`: the caller wants the file on disk, not its
        // contents, and reading a few hundred jars into memory to throw them away
        // is the whole cost of a large `jv sync`.
        match self
            .runtime
            .block_on(self.fetcher.locate(&repositories, &resolved))
        {
            Ok(fetched) => {
                for warning in &fetched.warnings {
                    self.warn(warning.clone());
                }
                Ok(Some(Materialized {
                    origin: fetched.origin,
                    path: fetched.path,
                    repository: fetched.repository,
                }))
            }
            Err(jv_cache::FetchError::NotFound { .. }) => Ok(None),
            Err(error) => Err(error.into()),
        }
    }
}

/// A file that is now on disk.
#[derive(Clone, Debug)]
pub struct Materialized {
    pub origin: Origin,
    /// Where the bytes are: in jv's cache, or in `~/.m2` when that is where they
    /// were found.
    pub path: std::path::PathBuf,
    /// The repository that served it, when one did. `jv sync` records this in
    /// `_remote.repositories`, which is why it has to survive this far.
    pub repository: Option<String>,
}

/// The coordinates a relocation points at, with absent fields keeping the
/// original's value.
fn relocation_target(artifact: &Artifact, model: &Model) -> Option<Artifact> {
    let relocation = model
        .distribution_management
        .as_ref()?
        .relocation
        .as_ref()?;
    let target = Artifact {
        group_id: relocation
            .group_id
            .clone()
            .unwrap_or_else(|| artifact.group_id.clone()),
        artifact_id: relocation
            .artifact_id
            .clone()
            .unwrap_or_else(|| artifact.artifact_id.clone()),
        version: relocation
            .version
            .clone()
            .unwrap_or_else(|| artifact.version.clone()),
        ..artifact.clone()
    };
    // A relocation that names nothing new is a no-op, and following it would
    // recurse straight back into the same POM.
    (target != *artifact).then_some(target)
}

fn relocation_message(model: &Model) -> Option<&str> {
    model
        .distribution_management
        .as_ref()?
        .relocation
        .as_ref()?
        .message
        .as_deref()
}

/// The repositories a model declares, as ones jv could contact.
fn declared_repositories(model: &Model) -> Vec<Repository> {
    model
        .repositories
        .iter()
        .filter_map(jv_repo::from_model)
        .collect()
}

/// Reads a `g:a:v` string back into an artifact.
///
/// The lineage records coordinates as strings because a POM's source may be a
/// file path rather than coordinates; anything that is not three colon-separated
/// fields is one of those and is skipped.
fn parse_coordinates(text: &str) -> Option<Artifact> {
    let mut fields = text.split(':');
    let (group_id, artifact_id, version) = (fields.next()?, fields.next()?, fields.next()?);
    if fields.next().is_some() || version.contains('$') {
        return None;
    }
    Some(Artifact {
        group_id: group_id.to_owned(),
        artifact_id: artifact_id.to_owned(),
        version: version.to_owned(),
        classifier: String::new(),
        extension: POM.to_owned(),
    })
}

fn coordinates(artifact: &Artifact) -> String {
    format!(
        "{}:{}:{}",
        artifact.group_id, artifact.artifact_id, artifact.version
    )
}

/// The descriptor cache key, which must separate classified siblings.
///
/// One POM serves every classified file of a version — a classifier selects a
/// *file*, not a different module — so `coordinates` is the right key for the
/// parsed model. It is the wrong key for the descriptor, because a
/// `Descriptor` also carries the artifact's own identity: keyed without the
/// classifier, `g:a:1:data` hit the entry cached for `g:a:1` and came back
/// describing the *plain* artifact. The collector then built its node and its
/// pool key from that, so the classified dependency became a second copy of the
/// plain one and conflict resolution dropped it as a duplicate.
///
/// That is how `org.xmlresolver:xmlresolver:jar:data` vanished from
/// spring-petclinic's checkstyle classpath and left `mvn -o` unable to run it.
/// The model itself is memoised separately, so re-reading a descriptor for a
/// classified sibling costs a little assembly and no network.
fn descriptor_key(artifact: &Artifact) -> String {
    format!(
        "{}:{}:{}:{}:{}",
        artifact.group_id,
        artifact.artifact_id,
        artifact.version,
        artifact.extension,
        artifact.classifier
    )
}

/// Keeps whichever of two versions is greater, for merging metadata across
/// repositories that each know a different newest.
fn take_greater(slot: &mut Option<String>, candidate: Option<&str>) {
    let Some(candidate) = candidate.filter(|value| !value.is_empty()) else {
        return;
    };
    let better = match slot.as_deref() {
        Some(held) => Version::parse(candidate) > Version::parse(held),
        None => true,
    };
    if better {
        *slot = Some(candidate.to_owned());
    }
}
