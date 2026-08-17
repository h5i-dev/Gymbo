//! Grouping nodes into conflict ids.
//!
//! Port of `ConflictMarker`, the first transformer in Maven's chain. It answers
//! one question: which nodes are competing with each other? Everything after it —
//! ordering, nearest-wins, scope selection — operates within one group at a time.
//!
//! The key is `groupId:artifactId:extension:classifier`, deliberately **without
//! the version**: two versions of the same artifact are exactly what a conflict
//! is. Classifier and extension are part of it, so `g:a:jar` and `g:a:jar:tests`
//! are separate competitions rather than rivals.
//!
//! Relocations and aliases are why this is grouping rather than a plain lookup.
//! A node that was relocated carries the keys of both its old and new
//! coordinates, and any node sharing *either* key belongs to the same group — so
//! an artifact that moved still conflicts with references to where it used to
//! live.

use std::collections::HashMap;

use jv_model::Artifact;

use crate::graph::{Graph, NodeId};

/// Identifies one competition. Only equality and the iteration order matter; the
/// value itself is opaque.
pub type ConflictId = usize;

/// A node's identity for conflict purposes: everything but the version.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct Key {
    group_id: String,
    artifact_id: String,
    extension: String,
    classifier: String,
}

impl Key {
    fn of(artifact: &Artifact) -> Self {
        Self {
            group_id: artifact.group_id.clone(),
            artifact_id: artifact.artifact_id.clone(),
            extension: artifact.extension.clone(),
            classifier: artifact.classifier.clone(),
        }
    }
}

/// A set of keys that all denote the same artifact, and the id assigned to them.
#[derive(Clone, Debug)]
struct Group {
    keys: Vec<Key>,
    index: ConflictId,
}

impl Group {
    fn contains_all(&self, keys: &[Key]) -> bool {
        keys.iter().all(|key| self.keys.contains(key))
    }
}

/// Assigns every node with an artifact to a conflict group.
///
/// Nodes without an artifact — the synthetic root — get no id, because they are
/// not competing with anything.
///
/// # Examples
///
/// ```
/// use jv_model::{Artifact, Dependency};
/// use jv_resolver::{Graph, Node, mark_conflict_ids};
///
/// let mut graph = Graph::new(Node::root());
/// let root = graph.root();
/// let a1 = graph.add(Node::dependency(
///     Dependency::new("g", "a", "1"),
///     Artifact::new("g", "a", "1"),
/// ));
/// let a2 = graph.add(Node::dependency(
///     Dependency::new("g", "a", "2"),
///     Artifact::new("g", "a", "2"),
/// ));
/// let b = graph.add(Node::dependency(
///     Dependency::new("g", "b", "1"),
///     Artifact::new("g", "b", "1"),
/// ));
/// graph.add_child(root, a1);
/// graph.add_child(root, a2);
/// graph.add_child(root, b);
///
/// let ids = mark_conflict_ids(&graph);
/// // Two versions of the same artifact are one competition.
/// assert_eq!(ids[&a1], ids[&a2]);
/// assert_ne!(ids[&a1], ids[&b]);
/// // The synthetic root competes with nothing.
/// assert!(!ids.contains_key(&root));
/// ```
pub fn mark_conflict_ids(graph: &Graph) -> HashMap<NodeId, ConflictId> {
    let mut groups: Vec<Group> = Vec::new();
    // Which group each key currently belongs to, as an index into `groups`.
    let mut by_key: HashMap<Key, usize> = HashMap::new();
    let mut next_index: ConflictId = 0;

    // Upstream analyses in depth-first order and visits each node once. The
    // order decides the numeric ids, so it is preserved rather than optimized.
    let mut visited = vec![false; graph.len()];
    let mut stack = vec![graph.root()];
    let mut order = Vec::new();
    while let Some(id) = stack.pop() {
        if visited[id.index()] {
            continue;
        }
        visited[id.index()] = true;
        order.push(id);
        for child in graph.children(id).iter().rev() {
            stack.push(*child);
        }
    }

    for id in &order {
        let keys = keys_of(graph, *id);
        if keys.is_empty() {
            continue;
        }
        assign(&keys, &mut groups, &mut by_key, &mut next_index);
    }

    // A node's id is the group holding its *own* coordinates; a relocated node's
    // old keys are in the same group, so either would do.
    let mut ids = HashMap::new();
    for id in &order {
        let node = graph.node(*id);
        // Upstream's `mark` tests the *dependency*, not the artifact, and the
        // difference is load-bearing. A root given as an artifact — the shape
        // `CollectRequest::root_artifact` produces, which is how a project
        // resolves its own dependencies — has coordinates but no dependency. Give
        // it a conflict id and it becomes a competitor in its own conflict group
        // at depth 0, wins, and prunes any transitive dependency on the project
        // itself along with everything beneath it.
        if node.dependency.is_none() {
            continue;
        }
        let Some(artifact) = &node.artifact else {
            continue;
        };
        if let Some(group) = by_key.get(&Key::of(artifact)) {
            ids.insert(*id, groups[*group].index);
        }
    }
    ids
}

/// Every coordinate a node answers to: its own, plus anywhere it was relocated
/// from, plus its aliases.
fn keys_of(graph: &Graph, id: NodeId) -> Vec<Key> {
    let node = graph.node(id);
    let Some(artifact) = &node.artifact else {
        return Vec::new();
    };
    // A node with no dependency is not a competitor even when it names an
    // artifact, matching upstream's test on the dependency rather than the
    // artifact.
    if node.dependency.is_none() {
        return Vec::new();
    }
    let mut keys = vec![Key::of(artifact)];
    for other in node.relocations.iter().chain(&node.aliases) {
        let key = Key::of(other);
        if !keys.contains(&key) {
            keys.push(key);
        }
    }
    keys
}

/// Folds one node's keys into the group structure, merging any groups they span.
fn assign(
    keys: &[Key],
    groups: &mut Vec<Group>,
    by_key: &mut HashMap<Key, usize>,
    next_index: &mut ConflictId,
) {
    // Which existing groups these keys already touch.
    let mut touched: Vec<usize> = Vec::new();
    for key in keys {
        if let Some(group) = by_key.get(key) {
            if !touched.contains(group) {
                touched.push(*group);
            }
        }
    }

    // Nothing seen before: a fresh group, and a fresh id.
    if touched.is_empty() {
        let index = *next_index;
        *next_index += 1;
        groups.push(Group {
            keys: keys.to_vec(),
            index,
        });
        let slot = groups.len() - 1;
        for key in keys {
            by_key.insert(key.clone(), slot);
        }
        return;
    }

    // One group that already covers these keys: nothing to do. This is the
    // common case — every node whose artifact was seen before.
    if touched.len() == 1 && groups[touched[0]].contains_all(keys) {
        return;
    }

    // Otherwise the keys span groups, or add to one. Upstream allocates a new
    // group with a new index whenever the key set actually grows, so the id
    // numbering follows the same rule here.
    let mut merged: Vec<Key> = Vec::new();
    for group in &touched {
        for key in &groups[*group].keys {
            if !merged.contains(key) {
                merged.push(key.clone());
            }
        }
    }
    for key in keys {
        if !merged.contains(key) {
            merged.push(key.clone());
        }
    }

    let index = *next_index;
    *next_index += 1;
    groups.push(Group {
        keys: merged.clone(),
        index,
    });
    let slot = groups.len() - 1;
    for key in &merged {
        by_key.insert(key.clone(), slot);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jv_model::{Artifact, Dependency};

    use crate::graph::Node;

    fn node(group: &str, artifact: &str, version: &str) -> Node {
        Node::dependency(
            Dependency::new(group, artifact, version),
            Artifact::new(group, artifact, version),
        )
    }

    #[test]
    fn versions_of_one_artifact_share_an_id() {
        let mut graph = Graph::new(Node::root());
        let root = graph.root();
        let a1 = graph.add(node("g", "a", "1"));
        let a2 = graph.add(node("g", "a", "2"));
        graph.add_child(root, a1);
        graph.add_child(root, a2);

        let ids = mark_conflict_ids(&graph);
        assert_eq!(ids[&a1], ids[&a2]);
    }

    #[test]
    fn different_artifacts_do_not() {
        let mut graph = Graph::new(Node::root());
        let root = graph.root();
        let a = graph.add(node("g", "a", "1"));
        let b = graph.add(node("g", "b", "1"));
        let other_group = graph.add(node("h", "a", "1"));
        graph.add_child(root, a);
        graph.add_child(root, b);
        graph.add_child(root, other_group);

        let ids = mark_conflict_ids(&graph);
        assert_ne!(ids[&a], ids[&b]);
        assert_ne!(ids[&a], ids[&other_group]);
    }

    #[test]
    fn classifier_and_extension_separate_competitions() {
        let mut graph = Graph::new(Node::root());
        let root = graph.root();
        let jar = graph.add(node("g", "a", "1"));

        let tests = graph.add(Node::dependency(
            Dependency::new("g", "a", "1"),
            Artifact::new("g", "a", "1").with_classifier("tests"),
        ));
        let war = graph.add(Node::dependency(
            Dependency::new("g", "a", "1"),
            Artifact::new("g", "a", "1").with_extension("war"),
        ));
        graph.add_child(root, jar);
        graph.add_child(root, tests);
        graph.add_child(root, war);

        let ids = mark_conflict_ids(&graph);
        assert_ne!(ids[&jar], ids[&tests]);
        assert_ne!(ids[&jar], ids[&war]);
        assert_ne!(ids[&tests], ids[&war]);
    }

    #[test]
    fn a_relocation_joins_both_coordinates() {
        let mut graph = Graph::new(Node::root());
        let root = graph.root();
        // Someone still depends on the old coordinates.
        let old = graph.add(node("old.group", "a", "1"));
        // And someone on the new ones, via a POM that relocated.
        let mut moved = node("new.group", "a", "2");
        moved.relocations = vec![Artifact::new("old.group", "a", "1")];
        let moved = graph.add(moved);
        graph.add_child(root, old);
        graph.add_child(root, moved);

        let ids = mark_conflict_ids(&graph);
        // The two are the same artifact, so they compete.
        assert_eq!(ids[&old], ids[&moved]);
    }

    #[test]
    fn an_alias_joins_coordinates_too() {
        let mut graph = Graph::new(Node::root());
        let root = graph.root();
        let plain = graph.add(node("g", "a", "1"));
        let mut aliased = node("h", "b", "1");
        aliased.aliases = vec![Artifact::new("g", "a", "1")];
        let aliased = graph.add(aliased);
        graph.add_child(root, plain);
        graph.add_child(root, aliased);

        let ids = mark_conflict_ids(&graph);
        assert_eq!(ids[&plain], ids[&aliased]);
    }

    #[test]
    fn a_relocation_can_merge_two_existing_groups() {
        let mut graph = Graph::new(Node::root());
        let root = graph.root();
        // Two groups form first...
        let old = graph.add(node("old.group", "a", "1"));
        let new = graph.add(node("new.group", "a", "2"));
        graph.add_child(root, old);
        graph.add_child(root, new);
        // ...then a node arrives that belongs to both.
        let mut bridge = node("new.group", "a", "3");
        bridge.relocations = vec![Artifact::new("old.group", "a", "1")];
        let bridge = graph.add(bridge);
        graph.add_child(root, bridge);

        let ids = mark_conflict_ids(&graph);
        assert_eq!(ids[&old], ids[&new]);
        assert_eq!(ids[&old], ids[&bridge]);
    }

    #[test]
    fn the_synthetic_root_gets_no_id() {
        let mut graph = Graph::new(Node::root());
        let root = graph.root();
        let a = graph.add(node("g", "a", "1"));
        graph.add_child(root, a);

        let ids = mark_conflict_ids(&graph);
        assert!(!ids.contains_key(&root));
        assert!(ids.contains_key(&a));
    }

    #[test]
    fn a_shared_node_is_analysed_once() {
        let mut graph = Graph::new(Node::root());
        let root = graph.root();
        let shared = graph.add(node("g", "shared", "1"));
        let a = graph.add(node("g", "a", "1"));
        let b = graph.add(node("g", "b", "1"));
        graph.add_child(root, a);
        graph.add_child(root, b);
        graph.add_child(a, shared);
        graph.add_child(b, shared);

        let ids = mark_conflict_ids(&graph);
        assert_eq!(ids.len(), 3);
    }

    #[test]
    fn a_cycle_does_not_hang() {
        let mut graph = Graph::new(Node::root());
        let root = graph.root();
        let a = graph.add(node("g", "a", "1"));
        let b = graph.add(node("g", "b", "1"));
        graph.add_child(root, a);
        graph.add_child(a, b);
        graph.add_child(b, a);

        let ids = mark_conflict_ids(&graph);
        assert_eq!(ids.len(), 2);
    }

    #[test]
    fn ids_follow_depth_first_discovery() {
        let mut graph = Graph::new(Node::root());
        let root = graph.root();
        let first = graph.add(node("g", "first", "1"));
        let nested = graph.add(node("g", "nested", "1"));
        let second = graph.add(node("g", "second", "1"));
        graph.add_child(root, first);
        graph.add_child(first, nested);
        graph.add_child(root, second);

        let ids = mark_conflict_ids(&graph);
        // Depth-first: the nested node is discovered before the second sibling.
        assert_eq!(ids[&first], 0);
        assert_eq!(ids[&nested], 1);
        assert_eq!(ids[&second], 2);
    }
}
