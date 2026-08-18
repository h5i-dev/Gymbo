//! Building the dependency graph, before any conflict is resolved.
//!
//! Port of `BfDependencyCollector` and the selector, manager and traverser
//! Maven installs around it. The output is the "dirty" graph: every declaration
//! that survived selection appears, including several versions of the same
//! artifact. Choosing between them is [`crate::resolve_conflicts`]'s job.
//!
//! Breadth-first, because that is what Maven 3.9 runs and what the corpus
//! goldens were recorded from. The order is visible in the result: a range
//! expands to siblings newest-first, and when two paths reach the same artifact
//! the shallower one owns the subtree.
//!
//! # The three depth counters
//!
//! Upstream has three, and they disagree. This module uses one — the **graph
//! level**, where the root's direct dependencies are level 1 — and states the
//! thresholds in those terms:
//!
//! | Rule | Applies from |
//! |---|---|
//! | `<dependencyManagement>` version, scope, optional | level 2 |
//! | managed exclusions | level 1 |
//! | optional dependencies dropped | level 2 |
//! | `test` / `provided` dropped | level 2 with a root artifact, level 1 with a root dependency |
//!
//! The management threshold is the one that surprises people: a POM's own
//! `<dependencyManagement>` does not manage its own direct dependencies here,
//! because the model builder already did that. What the resolver adds is
//! reaching *transitive* dependencies, which the model builder cannot see.

use std::collections::{HashMap, HashSet};

use jv_model::{Artifact, Dependency, Exclusion, Scope, TypeRegistry};
use jv_version::{Constraint, Version};

use crate::graph::{Graph, ManagedFlags, Node, NodeId, Premanaged};

/// What the collector needs to know about one artifact.
#[derive(Clone, Debug, Default)]
pub struct Descriptor {
    /// The artifact this really is, which differs from the one asked for when a
    /// relocation applied.
    pub artifact: Artifact,
    pub dependencies: Vec<Dependency>,
    /// Only the root's is ever consulted; see the module docs.
    pub managed_dependencies: Vec<Dependency>,
    /// Coordinates this was relocated from, oldest first.
    pub relocations: Vec<Artifact>,
}

/// Where descriptors come from.
pub trait DescriptorSource {
    /// Reads an artifact's descriptor.
    fn descriptor(&self, artifact: &Artifact) -> Result<Descriptor, String>;

    /// The versions a repository holds, ascending. Consulted only for ranges.
    fn versions(&self, _group_id: &str, _artifact_id: &str) -> Result<Vec<String>, String> {
        Ok(Vec::new())
    }
}

/// What to collect.
#[derive(Clone, Debug, Default)]
pub struct CollectRequest {
    /// The project being resolved, when it is not itself a dependency. This is
    /// the shape Maven uses, and it is why a project's own `test` dependencies
    /// survive while a *dependency's* test dependencies never enter the graph.
    pub root_artifact: Option<Artifact>,
    /// The root as a dependency, when resolving one artifact's subtree.
    pub root_dependency: Option<Dependency>,
    pub dependencies: Vec<Dependency>,
    pub managed_dependencies: Vec<Dependency>,
}

/// Collection failed outright. A missing descriptor does not: it yields a
/// childless node, the way Maven carries on past an unreadable POM.
#[derive(Clone, Debug, thiserror::Error)]
pub enum CollectError {
    #[error("cannot list versions of {group_id}:{artifact_id}: {reason}")]
    Versions {
        group_id: String,
        artifact_id: String,
        reason: String,
    },
    #[error("no version of {group_id}:{artifact_id} satisfies {range}")]
    NoMatchingVersion {
        group_id: String,
        artifact_id: String,
        range: String,
    },
}

/// The result of collecting: the graph, plus what went wrong along the way.
#[derive(Debug)]
pub struct Collected {
    pub graph: Graph,
    /// Artifacts whose descriptor could not be read. Their nodes are present and
    /// childless, matching Maven's tolerance of an unreadable POM.
    pub missing_descriptors: Vec<String>,
    /// Cycles found, rendered as the versionless path that closes each one.
    pub cycles: Vec<String>,
}

/// Builds the dependency graph.
pub fn collect(
    source: &dyn DescriptorSource,
    request: &CollectRequest,
    types: &TypeRegistry,
) -> Result<Collected, CollectError> {
    // With a root dependency, the level-1 list comes from the root's own
    // descriptor merged under anything the request stated; with a root artifact,
    // the request is the whole story and no descriptor is read.
    let (root_node, dependencies, managed_dependencies) =
        match (&request.root_dependency, &request.root_artifact) {
            (Some(dependency), _) => {
                let mut dependency = dependency.clone();
                // A range on the root collapses to one node at the highest version,
                // unlike a range anywhere else.
                if let Some(highest) = candidate_versions(source, &dependency, types)?.first() {
                    dependency.version = Some(highest.clone());
                }
                let artifact = dependency.to_artifact(types);
                let descriptor = source.descriptor(&artifact).unwrap_or(Descriptor {
                    artifact: artifact.clone(),
                    ..Default::default()
                });
                let mut node = Node::dependency(dependency.clone(), descriptor.artifact.clone());
                node.relocations = descriptor.relocations.clone();
                (
                    node,
                    merge_dependencies(&request.dependencies, &descriptor.dependencies),
                    merge_dependencies(
                        &request.managed_dependencies,
                        &descriptor.managed_dependencies,
                    ),
                )
            }
            (None, Some(artifact)) => (
                Node {
                    artifact: Some(artifact.clone()),
                    ..Node::root()
                },
                request.dependencies.clone(),
                request.managed_dependencies.clone(),
            ),
            (None, None) => (
                Node::root(),
                request.dependencies.clone(),
                request.managed_dependencies.clone(),
            ),
        };

    let management = Management::from(&managed_dependencies);
    let graph = Graph::new(root_node);
    let root = graph.root();

    // The level-1 selector, derived from the session one. Scope filtering starts
    // a level later when the root is an artifact rather than a dependency.
    let selector = Selector::session(request.root_dependency.is_none()).derive(None);

    let mut collected = Collected {
        graph,
        missing_descriptors: Vec::new(),
        cycles: Vec::new(),
    };
    let mut queue: Vec<Work> = dependencies
        .iter()
        .map(|dependency| Work {
            parent: root,
            dependency: dependency.clone(),
            selector: selector.clone(),
            level: 1,
            path: vec![root],
            relocations: Vec::new(),
            disable_version_management: false,
        })
        .collect();
    // A node that shares another's child list — because it closed a cycle, or
    // because the pool already had that subtree — can only be linked once the
    // whole graph is built, since the list it shares is still filling in.
    let mut cycle_links: Vec<(NodeId, NodeId)> = Vec::new();
    let mut pool_links: Vec<(NodeId, NodeId)> = Vec::new();
    let mut pool: HashMap<PoolKey, NodeId> = HashMap::new();
    let mut skipper = Skipper::default();

    while !queue.is_empty() {
        let mut next = Vec::new();
        for work in queue {
            process(
                source,
                types,
                &management,
                &work,
                &mut collected,
                &mut next,
                &mut cycle_links,
                &mut pool,
                &mut pool_links,
                &mut skipper,
            )?;
        }
        queue = next;
    }

    // Sharing the owner's list is what makes conflict resolution treat the two as
    // one graph node — which is the point, for a cycle and for a pool hit alike.
    for (sharer, owner) in cycle_links.into_iter().chain(pool_links) {
        collected.graph.share_children(owner, sharer);
    }

    Ok(collected)
}

/// Bounds the walk by refusing to expand an artifact that already won.
///
/// Port of `DependencyResolutionSkipper`. Without it a graph like the corpus's
/// `cycle-big` — 556 descriptors that reference each other heavily — expands
/// combinatorially: cycle detection bounds any single path, but not the number
/// of paths.
///
/// It is not purely an optimization. A skipped node stays in the graph as a
/// childless leaf, so what it skips is visible in the result, and the "leftmost"
/// rule below decides which of several paths owns an artifact's subtree.
#[derive(Debug, Default)]
struct Skipper {
    /// Artifacts that have been expanded, by full equality.
    winners: HashMap<Artifact, ()>,
    /// The winning version per `groupId:artifactId:extension:classifier`.
    winner_versions: HashMap<Gace, String>,
    /// Per-depth counter, incremented in walk order.
    sequence: HashMap<usize, usize>,
    /// Where each node sits in the walk.
    coordinates: HashMap<NodeId, (usize, usize)>,
    /// Where each artifact was most recently expanded.
    leftmost: HashMap<Artifact, (usize, usize)>,
    /// Nodes expanded despite being duplicates, and everything below them, which
    /// must not displace the original winner.
    /// A set, not a list: `cache` asks whether a node *or any of its ancestors*
    /// is in here, once per node, so a list makes it O(forced x depth) per node
    /// and quadratic over a deep graph. Upstream uses an identity map.
    forced: HashSet<NodeId>,
}

impl Skipper {
    /// Whether to leave this node childless. Records the node either way, so it
    /// must be called exactly once per node, in walk order.
    fn skip(&mut self, node: NodeId, artifact: &Artifact, parents: &[NodeId]) -> bool {
        let depth = parents.len() + 1;
        let sequence = self.sequence.entry(depth).or_insert(0);
        *sequence += 1;
        let coordinate = (depth, *sequence);
        self.coordinates.insert(node, coordinate);

        let key = gace_of(artifact);
        // A different version of the same artifact already won: this one cannot
        // survive conflict resolution, so its subtree is not worth walking.
        if self
            .winner_versions
            .get(&key)
            .is_some_and(|version| *version != artifact.version)
        {
            return true;
        }

        if self.winners.contains_key(artifact) {
            if !self.is_leftmost(artifact, parents) {
                return true;
            }
            // Left of where it won: expand anyway, because the leftmost path's
            // scope has to reach conflict resolution.
            self.forced.insert(node);
        }

        self.leftmost.insert(artifact.clone(), coordinate);
        false
    }

    /// Whether this node branches off to the left of where the artifact won.
    fn is_leftmost(&self, artifact: &Artifact, parents: &[NodeId]) -> bool {
        let Some((winner_depth, winner_sequence)) = self.leftmost.get(artifact) else {
            return false;
        };
        if *winner_depth > parents.len() {
            return false;
        }
        let ancestor = parents[*winner_depth - 1];
        self.coordinates
            .get(&ancestor)
            .is_some_and(|(_, sequence)| sequence < winner_sequence)
    }

    /// Records an expanded node as the winner for its artifact.
    ///
    /// A force-resolved node is deliberately not recorded, and neither is
    /// anything beneath one: that subtree exists only to carry scope information
    /// and must not displace the original winner.
    fn cache(&mut self, node: NodeId, artifact: &Artifact, parents: &[NodeId]) {
        if self.forced.contains(&node) || parents.iter().any(|id| self.forced.contains(id)) {
            return;
        }
        self.winners.insert(artifact.clone(), ());
        self.winner_versions
            .insert(gace_of(artifact), artifact.version.clone());
    }
}

fn gace_of(artifact: &Artifact) -> Gace {
    (
        artifact.group_id.clone(),
        artifact.artifact_id.clone(),
        artifact.extension.clone(),
        artifact.classifier.clone(),
    )
}

/// Merges two dependency lists, the first dominant.
///
/// Keyed by `groupId:artifactId:classifier:extension` — note the classifier
/// before the extension, which is not the order artifact ids use elsewhere.
fn merge_dependencies(dominant: &[Dependency], recessive: &[Dependency]) -> Vec<Dependency> {
    if dominant.is_empty() {
        return recessive.to_vec();
    }
    if recessive.is_empty() {
        return dominant.to_vec();
    }
    let key = |dependency: &Dependency| {
        format!(
            "{}:{}:{}:{}",
            dependency.group_id,
            dependency.artifact_id,
            dependency.classifier_or_default(),
            dependency.type_or_default()
        )
    };
    let mut merged = dominant.to_vec();
    let present: Vec<String> = dominant.iter().map(&key).collect();
    for dependency in recessive {
        if !present.contains(&key(dependency)) {
            merged.push(dependency.clone());
        }
    }
    merged
}

/// One dependency waiting to be expanded.
#[derive(Clone, Debug)]
struct Work {
    parent: NodeId,
    dependency: Dependency,
    selector: Selector,
    level: usize,
    /// Ancestors, for cycle detection.
    path: Vec<NodeId>,
    /// Coordinates this dependency was relocated from, oldest first. Empty for
    /// almost everything; carried so the node can report where it came from.
    relocations: Vec<Artifact>,
    /// Set when a relocation kept the same `groupId:artifactId`.
    ///
    /// Upstream's `disableVersionManagementSubsequently`: management already
    /// applied a version under those coordinates, and applying it again to the
    /// relocated dependency would overwrite the version the relocation chose.
    disable_version_management: bool,
}

#[allow(clippy::too_many_arguments)]
fn process(
    source: &dyn DescriptorSource,
    types: &TypeRegistry,
    management: &Management,
    work: &Work,
    collected: &mut Collected,
    next: &mut Vec<Work>,
    cycle_links: &mut Vec<(NodeId, NodeId)>,
    pool: &mut HashMap<PoolKey, NodeId>,
    pool_links: &mut Vec<(NodeId, NodeId)>,
    skipper: &mut Skipper,
) -> Result<(), CollectError> {
    process_at(
        source,
        types,
        management,
        work,
        collected,
        next,
        cycle_links,
        pool,
        pool_links,
        skipper,
        0,
    )
}

/// How many times a relocation may point at another relocation.
///
/// Chains are one or two links in practice. The bound exists so a published POM
/// that relocates to itself cannot recurse forever.
const MAX_RELOCATIONS: usize = 16;

/// One step, `depth` relocations deep.
#[allow(clippy::too_many_arguments)]
fn process_at(
    source: &dyn DescriptorSource,
    types: &TypeRegistry,
    management: &Management,
    work: &Work,
    collected: &mut Collected,
    next: &mut Vec<Work>,
    cycle_links: &mut Vec<(NodeId, NodeId)>,
    pool: &mut HashMap<PoolKey, NodeId>,
    pool_links: &mut Vec<(NodeId, NodeId)>,
    skipper: &mut Skipper,
    depth: usize,
) -> Result<(), CollectError> {
    // Selection happens before management, so a managed `<optional>` cannot
    // rescue a dependency the selector already dropped.
    if !work.selector.select(&work.dependency) {
        return Ok(());
    }

    let mut dependency = work.dependency.clone();
    let (managed, premanaged) =
        management.apply(&mut dependency, work.level, work.disable_version_management);

    for version in candidate_versions(source, &dependency, types)? {
        let mut requested = dependency.to_artifact(types);
        requested.version = version.clone();

        let descriptor = match source.descriptor(&requested) {
            Ok(descriptor) => descriptor,
            Err(reason) => {
                collected
                    .missing_descriptors
                    .push(format!("{requested}: {reason}"));
                Descriptor {
                    artifact: requested.clone(),
                    ..Default::default()
                }
            }
        };
        // The descriptor names what the artifact *is*, which differs from what
        // was asked for when a relocation applied. Everything downstream — the
        // cycle check included — works from the real coordinates, as upstream's
        // `d = d.setArtifact(descriptorResult.getArtifact())` does.
        let artifact = descriptor.artifact.clone();

        // Version is deliberately ignored: two versions of one artifact still
        // mean the producing projects form a cycle.
        if let Some(ancestor) = find_cycle(&collected.graph, &work.path, &artifact) {
            collected
                .cycles
                .push(format!("{}:{}", artifact.group_id, artifact.artifact_id));
            // The cycle is recorded either way, but it only becomes a *node* when
            // the ancestor it closed on is a dependency. Upstream checks
            // `cycleNode.getDependency() != null` and otherwise falls through to
            // ordinary expansion — which matters for a root given as an artifact,
            // where the project's own coordinates reappearing transitively would
            // otherwise borrow the root's child list and never be expanded,
            // silently dropping everything the real subtree contains.
            if collected.graph.node(ancestor).dependency.is_some() {
                let mut node =
                    Node::dependency(relocated(&dependency, &artifact), artifact.clone());
                node.managed = managed;
                node.premanaged = premanaged.clone();
                node.relocations = work.relocations.clone();
                let id = collected.graph.add(node);
                collected.graph.add_child(work.parent, id);
                cycle_links.push((id, ancestor));
                continue;
            }
        }

        // A relocation is not a label on the node: upstream restarts the whole
        // step with the relocated coordinates, so selection and management are
        // re-decided against where the artifact actually lives. An exclusion or a
        // managed version naming the *new* coordinates has to take effect, and
        // neither would if the relocation were only recorded.
        if !descriptor.relocations.is_empty() {
            let moved = relocated(&dependency, &artifact);
            // Re-selected, because an exclusion on the new coordinates applies.
            if !work.selector.select(&moved) {
                return Ok(());
            }
            let mut relocations = work.relocations.clone();
            relocations.extend(descriptor.relocations.iter().cloned());
            let step = Work {
                parent: work.parent,
                dependency: moved,
                selector: work.selector.clone(),
                level: work.level,
                path: work.path.clone(),
                relocations,
                // Only when the coordinates are unchanged but for the version:
                // management already spoke under this `groupId:artifactId`, and
                // applying it again would undo the version the relocation chose.
                disable_version_management: dependency.group_id == artifact.group_id
                    && dependency.artifact_id == artifact.artifact_id,
            };
            if depth > MAX_RELOCATIONS {
                collected
                    .missing_descriptors
                    .push(format!("{artifact}: relocations are chained too deeply"));
                return Ok(());
            }
            process_at(
                source,
                types,
                management,
                &step,
                collected,
                next,
                cycle_links,
                pool,
                pool_links,
                skipper,
                depth + 1,
            )?;
            // Upstream returns here rather than continuing the version loop, so a
            // relocated range collapses to the one version it relocated from —
            // which is what `transitiveDepsUseRangesAndRelocationDirtyTree`
            // expects: three versions in, one node out.
            return Ok(());
        }

        let mut node = Node::dependency(dependency.clone(), artifact.clone());
        node.version_constraint = dependency.version.clone();
        node.managed = managed;
        node.premanaged = premanaged.clone();
        node.relocations = work.relocations.clone();
        let id = collected.graph.add(node);
        collected.graph.add_child(work.parent, id);

        // A container format already holds its dependencies, so the graph stops
        // here rather than repeating their contents. Upstream's
        // `recurse = traverse && !dependencies.isEmpty()` — and its `else` branch
        // *guards on* the skipper's answer and then caches, which is what records
        // a leaf as the winner for its coordinates. Discarding the answer here,
        // as jv did, let a later version of the same artifact expand a subtree
        // upstream skips.
        let descriptor_type = dependency.type_or_default();
        if types.get(descriptor_type).includes_dependencies || descriptor.dependencies.is_empty() {
            if !skipper.skip(id, &descriptor.artifact, &work.path) {
                let mut path = work.path.clone();
                path.push(id);
                skipper.cache(id, &descriptor.artifact, &path);
            }
            continue;
        }

        let child_selector = work.selector.derive(Some(&dependency));

        // Upstream's `DataPool`, and it is consulted *before* the skipper — which
        // is the whole reason the ordering is spelled out here. A pool hit shares
        // the already-expanded node's child list and returns immediately, so it
        // never reaches the skipper, never takes a sequence number, and therefore
        // never shifts the numbering the skipper's `isLeftmost` rule depends on.
        // Getting this order wrong does not merely lose the sharing: it changes
        // which nodes the skipper decides to skip.
        //
        // Both ends get one child-list *identity*, which is what conflict
        // resolution keys on. The list itself is copied at link time rather than
        // shared the way upstream's mutable `List` is; that is only safe because
        // linking happens after the walk, when the owner's list is final. See
        // `Graph::share_children`.
        let pooled = PoolKey {
            artifact: descriptor.artifact.clone(),
            selector: child_selector.clone(),
            management_stage: management.stage_at(work.level),
        };
        if let Some(owner) = pool.get(&pooled).copied() {
            pool_links.push((id, owner));
            continue;
        }

        // Consulted in walk order, because it records where each node sits
        // whether or not it skips.
        if skipper.skip(id, &descriptor.artifact, &work.path) {
            continue;
        }

        let mut path = work.path.clone();
        path.push(id);
        for child in &descriptor.dependencies {
            next.push(Work {
                parent: id,
                dependency: child.clone(),
                selector: child_selector.clone(),
                level: work.level + 1,
                path: path.clone(),
                relocations: Vec::new(),
                disable_version_management: false,
            });
        }
        pool.insert(pooled, id);
        skipper.cache(id, &descriptor.artifact, &path);
    }
    Ok(())
}

/// What makes two subtrees the same subtree.
///
/// Upstream keys on `(artifact, repositories, selector, manager, traverser,
/// filter)`. jv does not model per-node repositories (recorded in
/// `jv-driver/src/source.rs`) and has no traverser or version filter, so what is
/// left is the artifact, the selector, and which management would apply — see
/// [`Management::stage_at`].
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct PoolKey {
    artifact: Artifact,
    selector: Selector,
    management_stage: usize,
}

/// A dependency rewritten onto the coordinates it was relocated to.
fn relocated(dependency: &Dependency, artifact: &Artifact) -> Dependency {
    Dependency {
        group_id: artifact.group_id.clone(),
        artifact_id: artifact.artifact_id.clone(),
        version: Some(artifact.version.clone()),
        classifier: (!artifact.classifier.is_empty()).then(|| artifact.classifier.clone()),
        ..dependency.clone()
    }
}

/// The nearest ancestor with the same coordinates, ignoring the version.
fn find_cycle(graph: &Graph, path: &[NodeId], artifact: &Artifact) -> Option<NodeId> {
    for ancestor in path.iter().rev() {
        let Some(other) = graph.node(*ancestor).artifact.as_ref() else {
            // The walk stops at a node with no artifact, which is the root.
            return None;
        };
        if other.group_id == artifact.group_id
            && other.artifact_id == artifact.artifact_id
            && other.extension == artifact.extension
            && other.classifier == artifact.classifier
        {
            return Some(*ancestor);
        }
    }
    None
}

/// The versions a dependency expands to: one, or every version a range admits.
fn candidate_versions(
    source: &dyn DescriptorSource,
    dependency: &Dependency,
    _types: &TypeRegistry,
) -> Result<Vec<String>, CollectError> {
    let declared = dependency.version.as_deref().unwrap_or_default();
    let Ok(constraint) = Constraint::parse(declared) else {
        return Ok(vec![declared.to_owned()]);
    };
    let Some(range) = constraint.range_set() else {
        // A plain version is a soft constraint and expands to itself.
        return Ok(vec![declared.to_owned()]);
    };

    let available = source
        .versions(&dependency.group_id, &dependency.artifact_id)
        .map_err(|reason| CollectError::Versions {
            group_id: dependency.group_id.clone(),
            artifact_id: dependency.artifact_id.clone(),
            reason,
        })?;
    let mut matching: Vec<String> = available
        .into_iter()
        .filter(|version| range.contains(&Version::parse(version)))
        .collect();
    if matching.is_empty() {
        return Err(CollectError::NoMatchingVersion {
            group_id: dependency.group_id.clone(),
            artifact_id: dependency.artifact_id.clone(),
            range: declared.to_owned(),
        });
    }
    // Breadth-first resolves newer first, which is why a range's siblings appear
    // highest-version-first in the graph.
    matching.sort_by(|left, right| Version::parse(right).cmp(&Version::parse(left)));
    Ok(matching)
}

/// The key dependency management is indexed by: everything but the version.
type Gace = (String, String, String, String);

fn gace(dependency: &Dependency) -> Gace {
    (
        dependency.group_id.clone(),
        dependency.artifact_id.clone(),
        dependency.type_or_default().to_owned(),
        dependency.classifier_or_default().to_owned(),
    )
}

/// The root's `<dependencyManagement>`, which is the only source Maven 3
/// collects from.
#[derive(Debug, Default)]
struct Management {
    versions: HashMap<Gace, String>,
    scopes: HashMap<Gace, Scope>,
    optionals: HashMap<Gace, bool>,
    exclusions: HashMap<Gace, Vec<Exclusion>>,
}

impl Management {
    /// Whether there is anything to manage at all.
    fn is_empty(&self) -> bool {
        self.versions.is_empty()
            && self.scopes.is_empty()
            && self.optionals.is_empty()
            && self.exclusions.is_empty()
    }

    /// How this management behaves at a level, for pooling purposes.
    ///
    /// Upstream derives a *new* manager per level up to `deriveUntil` (2) and
    /// then returns itself, and the manager is part of the pool key — so a
    /// subtree collected at level 1 is never shared with one at level 3, because
    /// management applies from level 2 and their children would differ.
    ///
    /// With no rules to apply, every derived manager behaves identically and the
    /// distinction disappears. That is not a shortcut: it is why upstream's own
    /// collector tests, whose session leaves the manager null, pool freely across
    /// depths — and it is what `cycle.txt` depends on.
    fn stage_at(&self, level: usize) -> usize {
        if self.is_empty() { 0 } else { level.min(2) }
    }

    fn from(managed: &[Dependency]) -> Self {
        let mut management = Management::default();
        for entry in managed {
            let key = gace(entry);
            // First rule wins, so a nearer declaration is not overridden by a
            // later one.
            if let Some(version) = &entry.version {
                if !version.is_empty() {
                    management
                        .versions
                        .entry(key.clone())
                        .or_insert_with(|| version.clone());
                }
            }
            if let Some(scope) = entry.scope {
                management.scopes.entry(key.clone()).or_insert(scope);
            }
            if let Some(optional) = entry.optional {
                management.optionals.entry(key.clone()).or_insert(optional);
            }
            if !entry.exclusions.is_empty() {
                // Exclusions are additive rather than first-wins.
                management
                    .exclusions
                    .entry(key)
                    .or_default()
                    .extend(entry.exclusions.iter().cloned());
            }
        }
        management
    }

    /// Applies management to a dependency, reporting what it changed.
    fn apply(
        &self,
        dependency: &mut Dependency,
        level: usize,
        disable_version_management: bool,
    ) -> (ManagedFlags, Premanaged) {
        let key = gace(dependency);
        let mut flags = ManagedFlags::default();
        let mut premanaged = Premanaged::default();

        // Exclusions apply from level 1; everything else from level 2, because
        // the model builder already applied a POM's own management to its own
        // dependencies.
        // Throughout: the trigger is *management supplying a value*, never the
        // value differing from what the dependency already had.
        //
        // `PremanagedDependency.create` guards each field on
        // `depMngt.getX() != null` alone, so management that resolves to the
        // same version still records a premanaged version and still sets the
        // managed bit. Guarding on inequality instead loses the
        // `version managed from <v>` annotation that Maven prints whenever a
        // BOM manages a dependency to the version it already declared — which
        // is the common case, not an edge case — and, for scope and optional,
        // silently changes resolution, because those bits suppress derivation
        // and inheritance.
        if let Some(managed) = self.exclusions.get(&key) {
            let mut merged = dependency.exclusions.clone();
            for exclusion in managed {
                if !merged.contains(exclusion) {
                    merged.push(exclusion.clone());
                }
            }
            dependency.exclusions = merged;
            flags.exclusions = true;
        }

        if level < 2 {
            return (flags, premanaged);
        }

        if let Some(version) = self
            .versions
            .get(&key)
            .filter(|_| !disable_version_management)
        {
            premanaged.version = dependency.version.clone();
            dependency.version = Some(version.clone());
            flags.version = true;
        }
        if let Some(scope) = self.scopes.get(&key) {
            premanaged.scope = dependency.scope;
            dependency.scope = Some(*scope);
            flags.scope = true;
        }
        if let Some(optional) = self.optionals.get(&key) {
            premanaged.optional = dependency.optional;
            dependency.optional = Some(*optional);
            flags.optional = true;
        }
        (flags, premanaged)
    }
}

/// Which dependencies are admitted at a given level.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Selector {
    /// Accumulated down the path, never removed.
    exclusions: Vec<Exclusion>,
    optional_depth: usize,
    scope_depth: usize,
    scope_apply_from: usize,
    /// Whether the level-1 derivation should push scope filtering a level later,
    /// which it does when the root is an artifact rather than a dependency.
    shift_on_first_derive: bool,
}

impl Selector {
    /// The session-level selector, before any derivation.
    fn session(root_is_artifact: bool) -> Self {
        Self {
            exclusions: Vec::new(),
            optional_depth: 0,
            scope_depth: 0,
            scope_apply_from: 1,
            shift_on_first_derive: root_is_artifact,
        }
    }

    fn select(&self, dependency: &Dependency) -> bool {
        if self
            .exclusions
            .iter()
            .any(|exclusion| exclusion.matches(&dependency.group_id, &dependency.artifact_id))
        {
            return false;
        }
        // Optional direct dependencies are kept; deeper ones are not.
        if self.optional_depth >= 2 && dependency.is_optional() {
            return false;
        }
        if self.scope_depth >= self.scope_apply_from
            && matches!(dependency.scope, Some(Scope::Test | Scope::Provided))
        {
            return false;
        }
        true
    }

    /// The selector for the children of a node reached through `dependency`.
    fn derive(&self, dependency: Option<&Dependency>) -> Self {
        let mut derived = self.clone();

        if let Some(dependency) = dependency {
            for exclusion in &dependency.exclusions {
                if !derived.exclusions.contains(exclusion) {
                    derived.exclusions.push(exclusion.clone());
                }
            }
            derived.exclusions.sort();
        }

        if derived.optional_depth < 2 {
            derived.optional_depth += 1;
        }

        if derived.scope_depth == 0 && derived.shift_on_first_derive && dependency.is_none() {
            // A project's own test and provided dependencies survive; a
            // dependency's do not.
            derived.scope_depth += 1;
            derived.scope_apply_from += 1;
        } else if derived.scope_depth < derived.scope_apply_from {
            derived.scope_depth += 1;
        }
        derived.shift_on_first_derive = false;
        derived
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dependency(name: &str, version: &str) -> Dependency {
        Dependency::new("g", name, version)
    }

    #[test]
    fn management_rules_take_the_first_declaration() {
        let managed = vec![dependency("a", "1"), dependency("a", "2")];
        let management = Management::from(&managed);
        let mut target = dependency("a", "");
        target.version = None;
        let (flags, premanaged) = management.apply(&mut target, 2, false);
        assert_eq!(target.version.as_deref(), Some("1"));
        assert!(flags.version);
        assert_eq!(premanaged.version, None);
    }

    #[test]
    fn management_does_not_reach_direct_dependencies() {
        let managed = vec![dependency("a", "9")];
        let management = Management::from(&managed);
        let mut direct = dependency("a", "1");
        let (flags, _) = management.apply(&mut direct, 1, false);
        // Level 1 is the model builder's business, not the resolver's.
        assert_eq!(direct.version.as_deref(), Some("1"));
        assert!(!flags.version);

        let mut transitive = dependency("a", "1");
        let (flags, premanaged) = management.apply(&mut transitive, 2, false);
        assert_eq!(transitive.version.as_deref(), Some("9"));
        assert!(flags.version);
        assert_eq!(premanaged.version.as_deref(), Some("1"));
    }

    #[test]
    fn managed_exclusions_reach_direct_dependencies() {
        let mut entry = dependency("a", "1");
        entry.exclusions = vec![Exclusion::new("x", "y")];
        let management = Management::from(&[entry]);

        let mut direct = dependency("a", "1");
        let (flags, _) = management.apply(&mut direct, 1, false);
        // The one thing management does at level 1.
        assert!(flags.exclusions);
        assert_eq!(direct.exclusions, vec![Exclusion::new("x", "y")]);
    }

    #[test]
    fn management_sets_scope_and_optional_from_level_two() {
        let mut entry = dependency("a", "1");
        entry.scope = Some(Scope::Runtime);
        entry.optional = Some(true);
        let management = Management::from(&[entry]);

        let mut target = dependency("a", "1");
        let (flags, premanaged) = management.apply(&mut target, 2, false);
        assert_eq!(target.scope, Some(Scope::Runtime));
        assert!(target.is_optional());
        assert!(flags.scope && flags.optional);
        assert_eq!(premanaged.scope, None);
    }

    #[test]
    fn a_project_keeps_its_own_test_dependencies() {
        // The root is an artifact, so scope filtering starts one level later.
        let level_one = Selector::session(true).derive(None);
        let mut test = dependency("a", "1");
        test.scope = Some(Scope::Test);
        assert!(level_one.select(&test));

        // But a test dependency of a dependency never enters the graph.
        let level_two = level_one.derive(Some(&dependency("b", "1")));
        assert!(!level_two.select(&test));
    }

    #[test]
    fn a_dependencys_own_test_dependencies_are_dropped_immediately() {
        // The root is a dependency, so filtering applies from level 1.
        let level_one = Selector::session(false).derive(None);
        let mut test = dependency("a", "1");
        test.scope = Some(Scope::Test);
        assert!(!level_one.select(&test));
        let mut provided = dependency("a", "1");
        provided.scope = Some(Scope::Provided);
        assert!(!level_one.select(&provided));
    }

    #[test]
    fn optional_dependencies_survive_only_at_level_one() {
        let level_one = Selector::session(true).derive(None);
        let mut optional = dependency("a", "1");
        optional.optional = Some(true);
        assert!(level_one.select(&optional));

        let level_two = level_one.derive(Some(&dependency("b", "1")));
        assert!(!level_two.select(&optional));
    }

    #[test]
    fn exclusions_accumulate_down_a_path() {
        let mut first = dependency("b", "1");
        first.exclusions = vec![Exclusion::new("x", "y")];
        let mut second = dependency("c", "1");
        second.exclusions = vec![Exclusion::new("p", "q")];

        let level_one = Selector::session(true).derive(None);
        let level_two = level_one.derive(Some(&first));
        let level_three = level_two.derive(Some(&second));

        assert!(!level_two.select(&dependency("y", "1").with_group("x")));
        // Both are in force further down; nothing is ever removed.
        assert!(!level_three.select(&dependency("y", "1").with_group("x")));
        assert!(!level_three.select(&dependency("q", "1").with_group("p")));
    }

    /// Test-only helper for building a dependency in another group.
    trait WithGroup {
        fn with_group(self, group: &str) -> Self;
    }

    impl WithGroup for Dependency {
        fn with_group(mut self, group: &str) -> Self {
            self.group_id = group.to_owned();
            self
        }
    }

    #[test]
    fn a_wildcard_exclusion_stops_everything() {
        let mut carrier = dependency("b", "1");
        carrier.exclusions = vec![Exclusion::new("*", "*")];
        let selector = Selector::session(true).derive(None).derive(Some(&carrier));
        assert!(!selector.select(&dependency("anything", "1")));
    }
}
