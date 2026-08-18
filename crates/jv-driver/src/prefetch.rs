//! Downloading POMs before anyone asks for them.
//!
//! # The problem this solves
//!
//! Collection is synchronous and exact: it reads one descriptor, decides what
//! that implies, and reads the next. Every read is a network round trip, and a
//! round trip to Maven Central is around 90 ms. A hundred-node graph needs a few
//! hundred POMs — its own, plus every one of their parents, plus the BOMs those
//! import — so at one round trip at a time a cold resolve takes twenty seconds
//! while using almost no CPU.
//!
//! Parent chains are the worst of it and the least obvious. A
//! `spring-boot-starter-web` POM is nearly empty; what it *means* comes from
//! `spring-boot-starters`, then `spring-boot-parent`, then
//! `spring-boot-dependencies`. Each is a separate request and none can be
//! predicted from the one before without reading it first, so the chain is
//! strictly serial on the critical path.
//!
//! # What it does instead
//!
//! A background walker starts from the project's declared dependencies and
//! crawls outward as fast as the network allows: fetch a POM, parse it, and
//! enqueue its parent, its imported BOMs, and its own dependencies. It writes
//! nothing but the cache. By the time collection asks for a POM, it is usually
//! already there.
//!
//! The walker is **allowed to be wrong**. It does not apply management,
//! interpolation, exclusions or conflict resolution, so it fetches POMs the real
//! resolve will never look at and misses ones it will. That is the whole point:
//! being approximate is what lets it run ahead. Correctness stays entirely with
//! the collector, which reads through the same cache and would fetch anything
//! the walker missed.
//!
//! Two bounds keep the approximation from becoming waste: a total budget on how
//! many POMs it may fetch, and a cap on how many requests are in flight. Both
//! are per session.
//!
//! # It parses, too
//!
//! The crawler has to parse each POM anyway to know what to fetch next, so it
//! hands the parsed model to the same memo the collector reads. That turns out
//! to matter more than the fetching on a *warm* run, where there is no network
//! to hide: reading and parsing a few hundred cached POMs was 42 ms of a 60 ms
//! resolve, all of it on the synchronous critical path, and all of it work the
//! crawler was already doing and discarding.

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use std::collections::HashMap;
use std::sync::{Mutex, RwLock};

use jv_cache::Fetcher;
use jv_model::{Artifact, Model, Scope, parse_pom};
use jv_repo::Repository;
use tokio::sync::Semaphore;

/// Where the crawler leaves what it has parsed.
///
/// The same maps `RepositorySource` reads from, shared rather than copied, so a
/// POM the crawler reached first is never parsed again.
#[derive(Clone, Default)]
pub struct Sink {
    /// Parsed POMs by `g:a:v`. `None` records a coordinate no repository has.
    pub poms: Arc<RwLock<HashMap<String, Option<Arc<Model>>>>>,
    /// Problems the parser reported, to show once at the end.
    pub warnings: Arc<Mutex<Vec<String>>>,
}

impl std::fmt::Debug for Sink {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("Sink").finish_non_exhaustive()
    }
}

/// How many POM requests may be in flight at once.
///
/// These are small files whose cost is latency, not bandwidth, and Maven Central
/// speaks HTTP/2, so they share one connection and the server sees one stream
/// each. High enough to hide the round trip, low enough to stay a well-behaved
/// client of a free service.
const IN_FLIGHT: usize = 32;

/// How many POMs one session may speculate on.
///
/// A generous ceiling on a normal project and a hard stop on a pathological one.
/// Reaching it costs nothing but the speculation: collection carries on and
/// fetches what it needs itself.
const BUDGET: usize = 4000;

/// A background POM crawler.
#[derive(Clone)]
pub struct Prefetcher {
    fetcher: Arc<Fetcher>,
    runtime: tokio::runtime::Handle,
    /// Coordinates already queued, so a diamond is walked once. Also the reason
    /// a cycle in a parent chain cannot loop forever.
    seen: Arc<std::sync::Mutex<HashSet<String>>>,
    remaining: Arc<AtomicUsize>,
    permits: Arc<Semaphore>,
    sink: Sink,
    enabled: bool,
}

impl std::fmt::Debug for Prefetcher {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Prefetcher")
            .field("enabled", &self.enabled)
            .field("remaining", &self.remaining.load(Ordering::Relaxed))
            .finish_non_exhaustive()
    }
}

impl Prefetcher {
    pub fn new(
        fetcher: Arc<Fetcher>,
        runtime: tokio::runtime::Handle,
        sink: Sink,
        enabled: bool,
    ) -> Self {
        Self {
            fetcher,
            runtime,
            seen: Arc::default(),
            remaining: Arc::new(AtomicUsize::new(BUDGET)),
            permits: Arc::new(Semaphore::new(
                std::env::var("JV_IN_FLIGHT")
                    .ok()
                    .and_then(|value| value.parse().ok())
                    .filter(|value| *value > 0)
                    .unwrap_or(IN_FLIGHT),
            )),
            sink,
            enabled,
        }
    }

    /// Starts crawling from some coordinates.
    ///
    /// Returns immediately. Nothing downstream waits on the result — a POM that
    /// does not arrive in time is simply fetched by the collector instead.
    pub fn seed<I>(&self, repositories: &[Repository], coordinates: I)
    where
        I: IntoIterator<Item = Artifact>,
    {
        if !self.enabled {
            return;
        }
        for artifact in coordinates {
            self.spawn(repositories.to_vec(), artifact);
        }
    }

    /// Claims one unit of budget for a coordinate not yet seen.
    fn claim(&self, artifact: &Artifact) -> bool {
        // A snapshot's real file name comes from a metadata round trip, which
        // the crawler does not do. Fetching `a-1.0-SNAPSHOT.pom` looks like it
        // works — `~/.m2` serves exactly that name, because Maven installs
        // snapshots untimestamped — and then memoizes a locally installed build
        // in place of the deployed one, silently resolving its dependencies
        // instead.
        if jv_model::is_snapshot_version(&artifact.version) {
            return false;
        }
        // A version that still holds a `${...}` cannot be addressed: the walker
        // does no interpolation, and guessing would fetch a 404 per node.
        if artifact.version.is_empty() || artifact.version.contains('$') {
            return false;
        }
        if artifact.group_id.is_empty() || artifact.artifact_id.is_empty() {
            return false;
        }
        let key = format!(
            "{}:{}:{}",
            artifact.group_id, artifact.artifact_id, artifact.version
        );
        if !self.seen.lock().expect("seen").insert(key) {
            return false;
        }
        // `fetch_update` rather than a decrement-and-test, so the budget cannot
        // go negative when several walkers claim at once.
        self.remaining
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |left| {
                (left > 0).then(|| left - 1)
            })
            .is_ok()
    }

    fn spawn(&self, repositories: Vec<Repository>, artifact: Artifact) {
        if !self.claim(&artifact) {
            return;
        }
        let walker = self.clone();
        self.runtime.spawn(async move {
            walker.walk(repositories, artifact).await;
        });
    }

    /// Fetches one POM and enqueues everything it points at.
    async fn walk(self, repositories: Vec<Repository>, artifact: Artifact) {
        let Ok(permit) = self.permits.clone().acquire_owned().await else {
            return;
        };
        let fetched = self.fetcher.artifact(&repositories, &artifact).await;
        // Released before recursing, so a deep parent chain does not hold a
        // permit per level and starve the pool.
        drop(permit);

        let key = format!(
            "{}:{}:{}",
            artifact.group_id, artifact.artifact_id, artifact.version
        );
        let Ok(fetched) = fetched else { return };
        let Ok(text) = std::str::from_utf8(&fetched.bytes) else {
            return;
        };
        let Ok(parsed) = parse_pom(text) else { return };

        // Published before recursing, so the collector can pick it up while this
        // task is still working out what to fetch next.
        {
            let mut warnings = self.sink.warnings.lock().expect("warnings");
            for warning in &parsed.warnings {
                let message = format!("{key}: {warning}");
                if !warnings.contains(&message) {
                    warnings.push(message);
                }
            }
        }
        let model = Arc::new(parsed.model);
        self.sink
            .poms
            .write()
            .expect("poms")
            .entry(key)
            .or_insert_with(|| Some(Arc::clone(&model)));

        for next in follow(&model) {
            self.spawn(repositories.clone(), next);
        }
    }
}

/// The POMs reading `model` will lead to.
///
/// Parents first, then imported BOMs, then dependencies — the order collection
/// needs them in, so the queue drains roughly along the critical path rather
/// than breadth-first across the whole graph.
fn follow(model: &Model) -> Vec<Artifact> {
    let mut next = Vec::new();

    if let Some(parent) = &model.parent {
        if let (Some(group_id), Some(artifact_id), Some(version)) = (
            parent.group_id.as_deref(),
            parent.artifact_id.as_deref(),
            parent.version.as_deref(),
        ) {
            next.push(pom(group_id, artifact_id, version));
        }
    }

    for entry in &model.dependency_management {
        let is_import = entry.type_.as_deref() == Some("pom") && entry.scope == Some(Scope::Import);
        if !is_import {
            continue;
        }
        if let Some(version) = entry.version.as_deref() {
            next.push(pom(&entry.group_id, &entry.artifact_id, version));
        }
    }

    for entry in &model.dependencies {
        // A dependency of a dependency in these scopes never enters the graph,
        // so its POM is never read and fetching it is pure waste.
        if matches!(entry.scope, Some(Scope::Test) | Some(Scope::Provided)) {
            continue;
        }
        if entry.optional == Some(true) {
            continue;
        }
        // No version means management will supply one, which the walker does not
        // model. Collection will ask for it properly.
        if let Some(version) = entry.version.as_deref() {
            next.push(pom(&entry.group_id, &entry.artifact_id, version));
        }
    }

    next
}

fn pom(group_id: &str, artifact_id: &str, version: &str) -> Artifact {
    Artifact {
        group_id: group_id.to_owned(),
        artifact_id: artifact_id.to_owned(),
        version: version.to_owned(),
        classifier: String::new(),
        extension: "pom".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jv_model::{Dependency, Parent};

    fn model(xml: &str) -> Model {
        parse_pom(xml).expect("a POM").model
    }

    #[test]
    fn a_parent_is_followed_first() {
        let model = model(
            r#"<project>
                 <parent><groupId>g</groupId><artifactId>p</artifactId><version>1.0</version></parent>
                 <artifactId>a</artifactId>
                 <dependencies><dependency>
                   <groupId>g</groupId><artifactId>d</artifactId><version>2.0</version>
                 </dependency></dependencies>
               </project>"#,
        );
        let next = follow(&model);
        // Parent first: nothing about the POM can be read until it is resolved,
        // so it is what the collector will block on.
        assert_eq!(next[0].artifact_id, "p");
        assert_eq!(next[1].artifact_id, "d");
    }

    #[test]
    fn imported_boms_are_followed() {
        let model = model(
            r#"<project><artifactId>a</artifactId>
                 <dependencyManagement><dependencies>
                   <dependency><groupId>g</groupId><artifactId>bom</artifactId>
                     <version>1.0</version><type>pom</type><scope>import</scope></dependency>
                   <dependency><groupId>g</groupId><artifactId>managed</artifactId>
                     <version>3.0</version></dependency>
                 </dependencies></dependencyManagement>
               </project>"#,
        );
        let next = follow(&model);
        // The BOM is read; a plain managed entry is not, because management only
        // supplies a version to a dependency that is declared elsewhere.
        assert_eq!(next.len(), 1);
        assert_eq!(next[0].artifact_id, "bom");
        assert_eq!(next[0].extension, "pom");
    }

    #[test]
    fn scopes_that_never_propagate_are_skipped() {
        let model = model(
            r#"<project><artifactId>a</artifactId><dependencies>
                 <dependency><groupId>g</groupId><artifactId>t</artifactId>
                   <version>1.0</version><scope>test</scope></dependency>
                 <dependency><groupId>g</groupId><artifactId>p</artifactId>
                   <version>1.0</version><scope>provided</scope></dependency>
                 <dependency><groupId>g</groupId><artifactId>o</artifactId>
                   <version>1.0</version><optional>true</optional></dependency>
                 <dependency><groupId>g</groupId><artifactId>c</artifactId>
                   <version>1.0</version></dependency>
               </dependencies></project>"#,
        );
        let next = follow(&model);
        // Only `c` can end up in someone else's graph.
        assert_eq!(next.len(), 1);
        assert_eq!(next[0].artifact_id, "c");
    }

    #[test]
    fn an_uninterpolated_version_is_not_guessed_at() {
        let prefetcher = Prefetcher::new(
            Arc::new(Fetcher::new(
                jv_cache::Store::new("/nonexistent"),
                Box::new(jv_cache::MapTransport::new()),
            )),
            dummy_handle(),
            Sink::default(),
            true,
        );
        // `${spring.version}` addresses nothing; fetching it would 404 once per
        // node in a project that uses a property for its versions, which is most
        // of them.
        assert!(!prefetcher.claim(&pom("g", "a", "${spring.version}")));
        assert!(!prefetcher.claim(&pom("g", "a", "")));
        assert!(prefetcher.claim(&pom("g", "a", "1.0")));
    }

    #[test]
    fn one_coordinate_is_walked_once() {
        let prefetcher = Prefetcher::new(
            Arc::new(Fetcher::new(
                jv_cache::Store::new("/nonexistent"),
                Box::new(jv_cache::MapTransport::new()),
            )),
            dummy_handle(),
            Sink::default(),
            true,
        );
        assert!(prefetcher.claim(&pom("g", "a", "1.0")));
        // A diamond reaches the same POM from two sides, and a `<parent>` cycle
        // reaches it from itself; both would otherwise never terminate.
        assert!(!prefetcher.claim(&pom("g", "a", "1.0")));
    }

    #[test]
    fn the_budget_is_a_hard_stop() {
        let prefetcher = Prefetcher::new(
            Arc::new(Fetcher::new(
                jv_cache::Store::new("/nonexistent"),
                Box::new(jv_cache::MapTransport::new()),
            )),
            dummy_handle(),
            Sink::default(),
            true,
        );
        prefetcher.remaining.store(2, Ordering::Relaxed);
        assert!(prefetcher.claim(&pom("g", "a", "1.0")));
        assert!(prefetcher.claim(&pom("g", "b", "1.0")));
        // Past the budget speculation stops; collection still resolves, it just
        // pays for the round trips itself.
        assert!(!prefetcher.claim(&pom("g", "c", "1.0")));
        assert_eq!(prefetcher.remaining.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn a_model_with_nothing_to_follow_yields_nothing() {
        assert!(follow(&Model::default()).is_empty());
        // And a parent missing a field is not half-followed.
        let half_a_parent = Model {
            parent: Some(Parent {
                group_id: Some("g".to_owned()),
                ..Parent::default()
            }),
            ..Model::default()
        };
        assert!(follow(&half_a_parent).is_empty());
        // Nor is a dependency that states no version.
        let unversioned = Model {
            dependencies: vec![Dependency {
                group_id: "g".to_owned(),
                artifact_id: "a".to_owned(),
                ..Dependency::default()
            }],
            ..Model::default()
        };
        assert!(follow(&unversioned).is_empty());
    }

    /// A runtime handle for tests that never actually spawn.
    fn dummy_handle() -> tokio::runtime::Handle {
        static RUNTIME: std::sync::OnceLock<tokio::runtime::Runtime> = std::sync::OnceLock::new();
        RUNTIME
            .get_or_init(|| tokio::runtime::Runtime::new().expect("a runtime"))
            .handle()
            .clone()
    }
}
