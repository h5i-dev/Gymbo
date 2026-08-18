//! Conflict resolution: picking one version per artifact.
//!
//! Port of `ClassicConflictResolver`, the algorithm Maven 3 uses and the one
//! `mvn dependency:tree` prints the result of. It is not a simple sweep, and the
//! shape is worth stating before the code:
//!
//! For each conflict id, in the order [`crate::sort_conflict_ids`] produced:
//!
//! 1. walk the graph from the root, gathering every occurrence of that id as a
//!    *conflict item* — and, on the way, splicing out leftovers of ids already
//!    settled;
//! 2. give each item a depth, which is the **minimum** over every path the walk
//!    took, not the depth of the path it happened to be found on;
//! 3. pick a winner: nearest wins, with siblings compared by version instead;
//! 4. give the winner its effective scope and optional flag;
//! 5. remove or annotate the losers, depending on how much detail was asked for.
//!
//! Two things make it intricate rather than long. The walk deliberately does
//! **not** descend into a node carrying the id being resolved, so losers survive
//! below conflicting nodes and are cleaned up by later rounds — which is why the
//! order of rounds matters and why a final cleanup walk exists for cyclic
//! graphs. And the unit of identity throughout is the *child list*, not the
//! node, because the collector shares one list between nodes for the same
//! artifact whose derived scopes differ.

use std::collections::{HashMap, HashSet};

use jv_model::Scope;
use jv_version::Version;

use crate::conflict_id::{ConflictId, mark_conflict_ids};
use crate::conflict_order::{ConflictOrder, sort_conflict_ids};
use crate::graph::{Graph, Node, NodeId};
use crate::scope_rules::{choose_effective_scope, derive_scope};

/// How much of the losing side to keep.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Verbosity {
    /// Losers are removed. The result is a resolvable graph, and what
    /// `dependency:tree` prints by default.
    #[default]
    None,
    /// Losers are kept as childless markers saying what beat them, which is
    /// `dependency:tree -Dverbose`. The result is annotated, not resolvable.
    Standard,
    /// Every loser and every cycle is kept. Nothing is removed, so a naive
    /// recursive consumer will not terminate.
    Full,
}

/// Resolution could not produce a graph.
#[derive(Clone, Debug, thiserror::Error)]
pub enum ResolveError {
    #[error("no version could be selected for conflict id {conflict_id}")]
    NoWinner { conflict_id: ConflictId },
    #[error(
        "version ranges leave no acceptable version for {coordinates}; candidates were {candidates}"
    )]
    Unsolvable {
        coordinates: String,
        candidates: String,
    },
    #[error("a node has no conflict id, so the graph was not marked before resolving")]
    MissingConflictId,
}

/// Resolves every conflict in the graph, in place.
///
/// # Examples
///
/// ```
/// use jv_model::{Artifact, Dependency};
/// use jv_resolver::{Graph, Node, Verbosity, resolve_conflicts};
///
/// // root -> a:2, and root -> b -> a:1. The nearer declaration wins.
/// let mut graph = Graph::new(Node::root());
/// let root = graph.root();
/// let near = graph.add(Node::dependency(
///     Dependency::new("g", "a", "2"),
///     Artifact::new("g", "a", "2"),
/// ));
/// let b = graph.add(Node::dependency(
///     Dependency::new("g", "b", "1"),
///     Artifact::new("g", "b", "1"),
/// ));
/// let far = graph.add(Node::dependency(
///     Dependency::new("g", "a", "1"),
///     Artifact::new("g", "a", "1"),
/// ));
/// graph.add_child(root, near);
/// graph.add_child(root, b);
/// graph.add_child(b, far);
///
/// resolve_conflicts(&mut graph, Verbosity::None).unwrap();
///
/// // The far one is gone; the near one stands.
/// assert_eq!(graph.children(root), &[near, b]);
/// assert!(graph.children(b).is_empty());
/// ```
pub fn resolve_conflicts(graph: &mut Graph, verbosity: Verbosity) -> Result<(), ResolveError> {
    let conflict_ids = mark_conflict_ids(graph);
    let order = sort_conflict_ids(graph, &conflict_ids);
    resolve_with(graph, verbosity, &conflict_ids, &order)
}

/// Resolves using ids and an order already computed, which the corpus tests use
/// to exercise one stage at a time.
pub fn resolve_with(
    graph: &mut Graph,
    verbosity: Verbosity,
    conflict_ids: &HashMap<NodeId, ConflictId>,
    order: &ConflictOrder,
) -> Result<(), ResolveError> {
    // An id in a cycle may be descended through while its own cycle peers are
    // still unresolved; without this the walk could not reach its own items.
    let mut cyclic_peers: HashMap<ConflictId, Vec<ConflictId>> = HashMap::new();
    for cycle in &order.cycles {
        for id in cycle {
            cyclic_peers
                .entry(*id)
                .or_default()
                .extend(cycle.iter().copied());
        }
    }

    let mut resolver = Resolver {
        verbosity,
        conflict_ids,
        // Presence means "already processed"; the value may be `None` when the
        // id had no items at all, and the two cases behave differently.
        resolved_ids: HashMap::new(),
        potential_ancestor_ids: HashSet::new(),
    };

    let mut last_winner = None;
    for (index, id) in order.sorted.iter().enumerate() {
        let peers = cyclic_peers.get(id).cloned().unwrap_or_default();
        let winner = resolver.round(graph, Some(*id), &peers)?;
        resolver.resolved_ids.insert(*id, winner);
        resolver.potential_ancestor_ids.insert(*id);
        if index + 1 == order.sorted.len() {
            last_winner = winner;
        }
    }

    // With cycles, losers can survive below the last winner because no later
    // round walks through it. One more pass with an id no node carries removes
    // them without gathering anything.
    if !order.cycles.is_empty() {
        if let Some(winner) = last_winner {
            resolver.cleanup_walk(graph, winner)?;
        }
    }

    Ok(())
}

struct Resolver<'a> {
    verbosity: Verbosity,
    conflict_ids: &'a HashMap<NodeId, ConflictId>,
    resolved_ids: HashMap<ConflictId, Option<NodeId>>,
    potential_ancestor_ids: HashSet<ConflictId>,
}

/// One occurrence of the conflict id being resolved.
#[derive(Clone, Debug)]
struct Item {
    /// The identity of the parent's child list, or `None` when the root itself
    /// carries the id.
    parent: Option<u64>,
    /// The parent node, needed to edit the list this item sits in.
    parent_node: Option<NodeId>,
    node: NodeId,
    depth: usize,
    /// Every scope this occurrence was derived to, across all walked paths.
    scopes: Vec<Option<Scope>>,
    optional_false: bool,
    optional_true: bool,
}

impl Item {
    fn is_sibling(&self, other: &Item) -> bool {
        self.parent == other.parent
    }

    fn add_scope(&mut self, scope: Option<Scope>) {
        if !self.scopes.contains(&scope) {
            self.scopes.push(scope);
        }
    }

    fn add_optional(&mut self, optional: bool) {
        if optional {
            self.optional_true = true;
        } else {
            self.optional_false = true;
        }
    }
}

/// What the walk remembers about one child list it has entered.
#[derive(Clone, Debug)]
struct Info {
    /// The shallowest depth this list has been reached at, over every path.
    min_depth: usize,
    derived_scopes: Vec<Option<Scope>>,
    optional_false: bool,
    optional_true: bool,
    /// Indices into the round's items, for the retro-update when this list is
    /// re-entered with a different derived scope or optionality.
    children: Vec<usize>,
}

const CHANGE_SCOPE: u8 = 0x01;
const CHANGE_OPTIONAL: u8 = 0x02;

impl Info {
    fn update(&mut self, depth: usize, scope: Option<Scope>, optional: bool) -> u8 {
        // Always lowered, even when nothing else changed: this is what makes
        // item depths minima over paths rather than depths of one path.
        if depth < self.min_depth {
            self.min_depth = depth;
        }
        let mut changes = 0;
        if !self.derived_scopes.contains(&scope) {
            self.derived_scopes.push(scope);
            changes |= CHANGE_SCOPE;
        }
        let seen = if optional {
            &mut self.optional_true
        } else {
            &mut self.optional_false
        };
        if !*seen {
            *seen = true;
            changes |= CHANGE_OPTIONAL;
        }
        changes
    }
}

/// The per-conflict-id walk state.
struct Round {
    current_id: Option<ConflictId>,
    items: Vec<Item>,
    infos: HashMap<u64, Info>,
    /// Child lists on the current path, for cycle detection.
    stack: HashSet<u64>,
    parent_nodes: Vec<NodeId>,
    parent_scopes: Vec<Option<Scope>>,
    parent_optionals: Vec<bool>,
    /// The info key each ancestor contributed, or `None` when that ancestor was
    /// being re-visited and must not create new items.
    parent_infos: Vec<Option<u64>>,
    /// Children to splice out, applied after each parent's loop.
    removals: Vec<(NodeId, NodeId)>,
}

impl Resolver<'_> {
    /// Resolves one conflict id, returning the winning node.
    fn round(
        &mut self,
        graph: &mut Graph,
        id: Option<ConflictId>,
        cyclic_peers: &[ConflictId],
    ) -> Result<Option<NodeId>, ResolveError> {
        self.potential_ancestor_ids
            .extend(cyclic_peers.iter().copied());

        let mut round = Round {
            current_id: id,
            items: Vec::new(),
            infos: HashMap::new(),
            stack: HashSet::new(),
            parent_nodes: Vec::new(),
            parent_scopes: Vec::new(),
            parent_optionals: Vec::new(),
            parent_infos: Vec::new(),
            removals: Vec::new(),
        };
        let root = graph.root();
        self.gather(graph, &mut round, root)?;
        apply_removals(graph, &round.removals);

        assign_depths(&mut round);

        if round.items.is_empty() {
            return Ok(None);
        }

        let winner = select_version(graph, &round.items)?;
        let winner_node = round.items[winner].node;

        let scope = select_scope(graph, &round.items, winner);
        let optional = select_optionality(graph, &round.items);

        if self.verbosity != Verbosity::None {
            let previous_scope = graph.node(winner_node).scope_of_dependency();
            let previous_optional = graph.node(winner_node).is_optional();
            let node = graph.node_mut(winner_node);
            node.original_scope = previous_scope;
            node.ignored_scope = None;
            let _ = previous_optional;
        }
        set_scope(graph, winner_node, scope);
        set_optional(graph, winner_node, optional);

        self.remove_losers(graph, &mut round, winner);

        Ok(Some(winner_node))
    }

    /// The final pass over a cyclic graph: gathers nothing, removes leftovers.
    fn cleanup_walk(&mut self, graph: &mut Graph, from: NodeId) -> Result<(), ResolveError> {
        let mut round = Round {
            // No node carries "no id", so nothing is gathered.
            current_id: None,
            items: Vec::new(),
            infos: HashMap::new(),
            stack: HashSet::new(),
            parent_nodes: Vec::new(),
            parent_scopes: Vec::new(),
            parent_optionals: Vec::new(),
            parent_infos: Vec::new(),
            removals: Vec::new(),
        };
        self.gather(graph, &mut round, from)?;
        apply_removals(graph, &round.removals);
        Ok(())
    }

    /// Walks the graph, collecting items and marking leftover losers for
    /// removal. `Ok(false)` means "the caller should drop this child".
    fn gather(&self, graph: &Graph, round: &mut Round, node: NodeId) -> Result<bool, ResolveError> {
        let id = self.conflict_ids.get(&node).copied();

        if round.current_id.is_some() && id == round.current_id {
            // Found an occurrence. Deliberately not descended into: anything
            // below it belongs to a competition that is not settled yet.
            self.add_item(graph, round, node);
            return Ok(true);
        }

        if let Some(id) = id {
            // A leftover from an id already settled, under a node that round
            // could not reach.
            if let Some(Some(winner)) = self.resolved_ids.get(&id) {
                if *winner != node {
                    return Ok(false);
                }
            }
        }

        if !self.push(graph, round, node, id)? {
            return Ok(true);
        }

        // By index rather than through a copy: this runs once per node *per
        // conflict id*, so the allocation was O(ids x edges) for a list nothing
        // mutates — `gather` only ever adds to `round`.
        for index in 0..graph.children(node).len() {
            let child = graph.children(node)[index];
            if !self.gather(graph, round, child)? {
                round.removals.push((node, child));
            }
        }
        self.pop(graph, round);
        Ok(true)
    }

    /// Decides whether to descend into a node, and records what the walk learns
    /// about it. `Ok(false)` prunes without removing.
    fn push(
        &self,
        graph: &Graph,
        round: &mut Round,
        node: NodeId,
        id: Option<ConflictId>,
    ) -> Result<bool, ResolveError> {
        match id {
            None => {
                if graph.node(node).dependency.is_some() {
                    // A loser copy has no conflict id and is not walked into.
                    if graph.node(node).omitted_for.is_some() {
                        return Ok(false);
                    }
                    return Err(ResolveError::MissingConflictId);
                }
                // The root has no dependency, and falls through.
            }
            Some(id) => {
                // Descent is allowed only through ids already settled, or peers
                // of the current one in a cycle. The topological order is what
                // makes that sufficient.
                if !self.potential_ancestor_ids.contains(&id) {
                    return Ok(false);
                }
            }
        }

        let identity = graph.children_identity(node);
        if !round.stack.insert(identity) {
            // Already on this path.
            return Ok(false);
        }

        let depth = round.parent_nodes.len();
        let scope = self.derive_scope_of(graph, round, node, id);
        let optional = self.derive_optional_of(graph, round, node, id);

        match round.infos.get_mut(&identity) {
            None => {
                round.infos.insert(
                    identity,
                    Info {
                        min_depth: depth,
                        derived_scopes: vec![scope],
                        optional_false: !optional,
                        optional_true: optional,
                        children: Vec::new(),
                    },
                );
                round.parent_infos.push(Some(identity));
            }
            Some(info) => {
                let changes = info.update(depth, scope, optional);
                if changes == 0 {
                    // Nothing new: re-walking would only repeat work.
                    round.stack.remove(&identity);
                    return Ok(false);
                }
                let children = info.children.clone();
                // `None` stops this visit from creating duplicate items; the
                // existing ones are updated instead.
                round.parent_infos.push(None);
                round.parent_nodes.push(node);
                round.parent_scopes.push(scope);
                round.parent_optionals.push(optional);

                // Derivation below must see the newly pushed parent context.
                for index in children.into_iter().rev() {
                    let item_node = round.items[index].node;
                    if changes & CHANGE_SCOPE != 0 {
                        let derived = self.derive_scope_of(graph, round, item_node, None);
                        round.items[index].add_scope(derived);
                    }
                    if changes & CHANGE_OPTIONAL != 0 {
                        let derived = self.derive_optional_of(graph, round, item_node, None);
                        round.items[index].add_optional(derived);
                    }
                }
                return Ok(true);
            }
        }

        round.parent_nodes.push(node);
        round.parent_scopes.push(scope);
        round.parent_optionals.push(optional);
        Ok(true)
    }

    fn pop(&self, graph: &Graph, round: &mut Round) {
        if let Some(node) = round.parent_nodes.pop() {
            round.parent_scopes.pop();
            round.parent_optionals.pop();
            round.parent_infos.pop();
            // Upstream's `pop` ends with `stack.remove(node.getChildren())`, and
            // leaving it out turns per-path cycle detection into a global visited
            // set. A second, *different* path to the same child list then bails at
            // the `stack` check instead of reaching the branch that lowers
            // `min_depth` and retro-updates the scope and optionality of items
            // already collected under it — so a nearer path is never recognised
            // and a different version wins.
            round.stack.remove(&graph.children_identity(node));
        }
    }

    fn add_item(&self, graph: &Graph, round: &mut Round, node: NodeId) {
        let scope = self.derive_scope_of(graph, round, node, None);
        let optional = self.derive_optional_of(graph, round, node, None);

        let Some(parent_node) = round.parent_nodes.last().copied() else {
            round.items.push(Item {
                parent: None,
                parent_node: None,
                node,
                depth: 0,
                scopes: vec![scope],
                optional_false: !optional,
                optional_true: optional,
            });
            return;
        };

        // `None` means this parent is being re-visited, and its items already
        // exist.
        let Some(key) = round.parent_infos.last().copied().flatten() else {
            return;
        };
        round.items.push(Item {
            parent: Some(key),
            parent_node: Some(parent_node),
            node,
            depth: 0,
            scopes: vec![scope],
            optional_false: !optional,
            optional_true: optional,
        });
        let index = round.items.len() - 1;
        if let Some(info) = round.infos.get_mut(&key) {
            info.children.push(index);
        }
    }

    /// The scope a node has in the current walk context.
    fn derive_scope_of(
        &self,
        graph: &Graph,
        round: &Round,
        node: NodeId,
        id: Option<ConflictId>,
    ) -> Option<Scope> {
        let own = graph.node(node).scope_of_dependency();
        // Management is explicit and wins; so does an id already settled, whose
        // nodes keep whatever they ended up with.
        if graph.node(node).managed.scope
            || id.is_some_and(|id| self.resolved_ids.contains_key(&id))
        {
            return own;
        }
        let depth = round.parent_scopes.len();
        if depth == 0 {
            return own;
        }
        derive_scope(round.parent_scopes[depth - 1], own)
    }

    /// The optional flag a node has in the current walk context.
    fn derive_optional_of(
        &self,
        graph: &Graph,
        round: &Round,
        node: NodeId,
        id: Option<ConflictId>,
    ) -> bool {
        let node_ref = graph.node(node);
        let optional = node_ref.is_optional();
        if optional
            || node_ref.managed.optional
            || id.is_some_and(|id| self.resolved_ids.contains_key(&id))
        {
            return optional;
        }
        // Otherwise optionality is inherited: anything under an optional
        // dependency is itself optional.
        round.parent_optionals.last().copied().unwrap_or(false)
    }

    /// Removes or annotates the losers, according to verbosity.
    fn remove_losers(&self, graph: &mut Graph, round: &mut Round, winner: usize) {
        let winner_node = round.items[winner].node;
        let winner_id = artifact_id_of(graph, winner_node);

        // Pass 1: remove outright, or replace with an annotated childless copy.
        let mut to_remove_ids: Vec<String> = Vec::new();
        for index in 0..round.items.len() {
            if index == winner {
                continue;
            }
            let Some(parent) = round.items[index].parent_node else {
                // Only the root can have no parent, and it cannot lose: its
                // depth is 0 and nothing is nearer.
                debug_assert!(false, "a root item lost its own conflict");
                continue;
            };
            let node = round.items[index].node;
            if !graph.children(parent).contains(&node) {
                continue;
            }

            if self.verbosity == Verbosity::None {
                let children: Vec<NodeId> = graph
                    .children(parent)
                    .iter()
                    .copied()
                    .filter(|child| *child != node)
                    .collect();
                graph.set_children(parent, children);
                continue;
            }

            let child_id = artifact_id_of(graph, node);
            if self.verbosity == Verbosity::Standard && child_id != winner_id {
                to_remove_ids.push(child_id.clone());
            }

            // The copy keeps the coordinates and loses the children, so a
            // verbose tree shows the loser without re-printing its subtree.
            let mut copy = graph.node(node).clone();
            copy.children = Vec::new();
            // The clone brought the original's child-list identity with it, but
            // not its children — so the copy would claim to *be* a list it does
            // not have, and anything keying on that identity would find an empty
            // subtree where the real one is.
            copy.children_key = None;
            copy.original_scope = copy.scope_of_dependency();
            copy.omitted_for = graph
                .node(winner_node)
                .artifact
                .as_ref()
                .map(|artifact| artifact.base_version());
            // An item can carry several derived scopes; upstream takes an
            // arbitrary one, so the first is as good as any.
            let loser_scope = round.items[index].scopes.first().copied().flatten();
            if let Some(dependency) = &mut copy.dependency {
                dependency.scope = loser_scope;
            }
            let copy_id = graph.add(copy);
            replace_child(graph, parent, node, copy_id);
            round.items[index].node = copy_id;
        }

        // Pass 2, standard only: drop copies that would misrepresent the tree.
        if self.verbosity != Verbosity::Standard || to_remove_ids.is_empty() {
            return;
        }
        for index in 0..round.items.len() {
            if index == winner {
                continue;
            }
            let Some(parent) = round.items[index].parent_node else {
                continue;
            };
            let node = round.items[index].node;
            if !graph.children(parent).contains(&node) {
                continue;
            }
            let child_id = artifact_id_of(graph, node);
            // Kept when the parent has only one node for these coordinates:
            // that is the ordinary conflict, and the marker is the point. Dropped
            // when the parent has several — a range the collector expanded —
            // because keeping them would claim a divergence that is not there.
            if to_remove_ids.contains(&child_id) && related_siblings(graph, parent, node) > 1 {
                let children: Vec<NodeId> = graph
                    .children(parent)
                    .iter()
                    .copied()
                    .filter(|child| *child != node)
                    .collect();
                graph.set_children(parent, children);
            }
        }
    }
}

/// Applies the removals a walk collected.
fn apply_removals(graph: &mut Graph, removals: &[(NodeId, NodeId)]) {
    for (parent, child) in removals {
        let children: Vec<NodeId> = graph
            .children(*parent)
            .iter()
            .copied()
            .filter(|candidate| candidate != child)
            .collect();
        graph.set_children(*parent, children);
    }
}

fn replace_child(graph: &mut Graph, parent: NodeId, old: NodeId, new: NodeId) {
    let children: Vec<NodeId> = graph
        .children(parent)
        .iter()
        .map(|child| if *child == old { new } else { *child })
        .collect();
    graph.set_children(parent, children);
}

/// `groupId:artifactId:extension[:classifier]:version`, the identity upstream
/// compares losers by.
fn artifact_id_of(graph: &Graph, node: NodeId) -> String {
    graph
        .node(node)
        .artifact
        .as_ref()
        .map(ToString::to_string)
        .unwrap_or_default()
}

/// How many of a parent's children share a node's group and artifact.
fn related_siblings(graph: &Graph, parent: NodeId, node: NodeId) -> usize {
    let Some(target) = graph.node(node).artifact.as_ref() else {
        return 0;
    };
    graph
        .children(parent)
        .iter()
        .filter(|child| {
            graph
                .node(**child)
                .artifact
                .as_ref()
                .is_some_and(|artifact| {
                    artifact.group_id == target.group_id
                        && artifact.artifact_id == target.artifact_id
                })
        })
        .count()
}

/// Gives every item its depth: the minimum over every path the walk descended.
fn assign_depths(round: &mut Round) {
    let mut previous_parent: Option<u64> = None;
    let mut previous_depth = 0usize;
    for index in (0..round.items.len()).rev() {
        let parent = round.items[index].parent;
        if parent == previous_parent {
            round.items[index].depth = previous_depth;
        } else if let Some(key) = parent {
            previous_parent = Some(key);
            previous_depth = round.infos.get(&key).map_or(0, |info| info.min_depth) + 1;
            round.items[index].depth = previous_depth;
        }
    }
}

/// Nearest wins, with siblings compared by version.
fn select_version(graph: &Graph, items: &[Item]) -> Result<usize, ResolveError> {
    let mut constraints: Vec<jv_version::Constraint> = Vec::new();
    let mut candidates: Vec<usize> = Vec::new();
    let mut winner: Option<usize> = None;

    for (index, item) in items.iter().enumerate() {
        let constraint = constraint_of(graph, item.node);
        let mut backtrack = false;
        let hard = constraint
            .as_ref()
            .is_some_and(|constraint| constraint.range_set().is_some());

        if hard {
            let constraint = constraint.clone().expect("hard implies present");
            if !constraints.contains(&constraint) {
                constraints.push(constraint.clone());
                // A range discovered late can rule out the incumbent.
                if let Some(current) = winner {
                    if !constraint.contains(&version_of(graph, items[current].node)) {
                        backtrack = true;
                    }
                }
            }
        }

        let acceptable = is_acceptable(&constraints, &version_of(graph, item.node));
        if acceptable {
            candidates.push(index);
        }
        if backtrack {
            winner = redo(graph, items, &constraints, &mut candidates)?;
        } else if acceptable
            && (winner.is_none() || is_nearer(graph, item, &items[winner.unwrap()]))
        {
            winner = Some(index);
        }
    }

    winner.ok_or(ResolveError::NoWinner { conflict_id: 0 })
}

/// Re-runs the nearest-wins scan after the constraint set grew.
fn redo(
    graph: &Graph,
    items: &[Item],
    constraints: &[jv_version::Constraint],
    candidates: &mut Vec<usize>,
) -> Result<Option<usize>, ResolveError> {
    let mut winner: Option<usize> = None;
    candidates.retain(|index| is_acceptable(constraints, &version_of(graph, items[*index].node)));
    for index in candidates.iter() {
        if winner.is_none() || is_nearer(graph, &items[*index], &items[winner.unwrap()]) {
            winner = Some(*index);
        }
    }
    if winner.is_none() {
        return Err(ResolveError::Unsolvable {
            coordinates: items
                .first()
                .map(|item| artifact_id_of(graph, item.node))
                .unwrap_or_default(),
            candidates: items
                .iter()
                .map(|item| version_of(graph, item.node).to_string())
                .collect::<Vec<_>>()
                .join(", "),
        });
    }
    Ok(winner)
}

fn is_acceptable(constraints: &[jv_version::Constraint], version: &Version) -> bool {
    constraints
        .iter()
        .all(|constraint| constraint.contains(version))
}

/// Strictly nearer, so a tie keeps the incumbent — which is the first in walk
/// order, and therefore the first declared.
fn is_nearer(graph: &Graph, candidate: &Item, incumbent: &Item) -> bool {
    if candidate.is_sibling(incumbent) {
        version_of(graph, candidate.node) > version_of(graph, incumbent.node)
    } else {
        candidate.depth < incumbent.depth
    }
}

fn version_of(graph: &Graph, node: NodeId) -> Version {
    Version::parse(
        graph
            .node(node)
            .artifact
            .as_ref()
            .map_or("", |artifact| artifact.version.as_str()),
    )
}

fn constraint_of(graph: &Graph, node: NodeId) -> Option<jv_version::Constraint> {
    let text = graph.node(node).version_constraint.as_deref()?;
    jv_version::Constraint::parse(text).ok()
}

/// The winner's effective scope.
fn select_scope(graph: &Graph, items: &[Item], winner: usize) -> Option<Scope> {
    let own = graph.node(items[winner].node).scope_of_dependency();
    // A system dependency stays system, whatever else it was reached as.
    if own == Some(Scope::System) {
        return own;
    }
    let mut scopes: Vec<Option<Scope>> = Vec::new();
    for item in items {
        // A direct dependency's declared scope is authoritative and is never
        // widened by a transitive path.
        if item.depth <= 1 {
            return graph.node(item.node).scope_of_dependency();
        }
        for scope in &item.scopes {
            scopes.push(*scope);
        }
    }
    choose_effective_scope(&scopes)
}

/// The winner's optional flag.
fn select_optionality(graph: &Graph, items: &[Item]) -> bool {
    let mut optional = true;
    for item in items {
        if item.depth <= 1 {
            return graph.node(item.node).is_optional();
        }
        // One non-optional occurrence anywhere makes the winner non-optional.
        if item.optional_false {
            optional = false;
        }
    }
    optional
}

fn set_scope(graph: &mut Graph, node: NodeId, scope: Option<Scope>) {
    if let Some(dependency) = &mut graph.node_mut(node).dependency {
        dependency.scope = scope;
    }
}

fn set_optional(graph: &mut Graph, node: NodeId, optional: bool) {
    if let Some(dependency) = &mut graph.node_mut(node).dependency {
        dependency.optional = Some(optional);
    }
}

impl Node {
    /// The dependency's declared scope, or `None` when it has no dependency or
    /// declared none.
    fn scope_of_dependency(&self) -> Option<Scope> {
        self.dependency
            .as_ref()
            .and_then(|dependency| dependency.scope)
    }
}
