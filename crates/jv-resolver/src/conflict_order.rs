//! Ordering conflict ids, and finding the cycles among them.
//!
//! Port of `ConflictIdSorter`. Conflict resolution processes one competition at
//! a time, and the order matters: resolving an artifact's version can change
//! which of its dependencies are reachable at all, so an id has to be settled
//! before the ids beneath it. This pass produces that order.
//!
//! The order is topological over "id A has a node with a child carrying id B",
//! biased towards the root: among ids that are ready to be emitted, the one
//! nearest the root goes first. That is what makes nearest-wins mean what people
//! expect.
//!
//! Conflict ids can form cycles even when the dependency graph does not, so the
//! sort is not always possible. When it stalls, the nearest remaining id is
//! forced through and the cycle is recorded — the resolver uses cycle membership
//! to widen the set of subtrees it is willing to walk into.

use std::collections::HashMap;

use crate::conflict_id::ConflictId;
use crate::graph::{Graph, NodeId};

/// The order conflict ids should be resolved in, and which of them are cyclic.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ConflictOrder {
    /// Every conflict id, nearest the root first.
    pub sorted: Vec<ConflictId>,
    /// Each entry is the set of ids on one cycle. Unordered by nature: the only
    /// consumer asks whether an id is in some cycle.
    pub cycles: Vec<Vec<ConflictId>>,
}

impl ConflictOrder {
    /// Whether this id takes part in any cycle.
    pub fn is_cyclic(&self, id: ConflictId) -> bool {
        self.cycles.iter().any(|cycle| cycle.contains(&id))
    }
}

/// One id in the conflict-id graph.
#[derive(Clone, Debug)]
struct Entry {
    key: ConflictId,
    /// Ids reachable in one step, in first-seen order.
    ///
    /// Upstream keeps these in a `HashSet`, whose iteration order can decide
    /// tie-breaks between ids at the same depth. jv uses insertion order
    /// instead: it is deterministic, and reproducing Java's hash iteration would
    /// mean emulating `HashMap`'s bucket layout for a difference no corpus case
    /// has shown.
    children: Vec<usize>,
    in_degree: usize,
    /// The smallest depth any node with this id appears at.
    min_depth: usize,
}

/// Orders the conflict ids of a graph.
///
/// # Examples
///
/// ```
/// use jv_model::{Artifact, Dependency};
/// use jv_resolver::{Graph, Node, mark_conflict_ids, sort_conflict_ids};
///
/// let mut graph = Graph::new(Node::root());
/// let root = graph.root();
/// let outer = graph.add(Node::dependency(
///     Dependency::new("g", "outer", "1"),
///     Artifact::new("g", "outer", "1"),
/// ));
/// let inner = graph.add(Node::dependency(
///     Dependency::new("g", "inner", "1"),
///     Artifact::new("g", "inner", "1"),
/// ));
/// graph.add_child(root, outer);
/// graph.add_child(outer, inner);
///
/// let ids = mark_conflict_ids(&graph);
/// let order = sort_conflict_ids(&graph, &ids);
/// // A dependency is settled before the things it depends on.
/// assert_eq!(order.sorted, vec![ids[&outer], ids[&inner]]);
/// assert!(order.cycles.is_empty());
/// ```
pub fn sort_conflict_ids(
    graph: &Graph,
    conflict_ids: &HashMap<NodeId, ConflictId>,
) -> ConflictOrder {
    let mut entries: Vec<Entry> = Vec::new();
    // Insertion-ordered lookup from a conflict id to its position in `entries`.
    let mut position: HashMap<ConflictId, usize> = HashMap::new();

    // The root may itself carry a conflict id, in which case it is the first
    // entry and sits at depth 0.
    let root = graph.root();
    let current = conflict_ids.get(&root).map(|key| {
        entries.push(Entry {
            key: *key,
            children: Vec::new(),
            in_degree: 0,
            min_depth: 0,
        });
        position.insert(*key, 0);
        0usize
    });

    let mut visited = vec![false; graph.len()];
    build(
        graph,
        conflict_ids,
        root,
        current,
        0,
        &mut visited,
        &mut entries,
        &mut position,
    );

    let (sorted, stalled) = topological_sort(&mut entries);
    // Whether cycles exist is decided by the *first* pass stalling, not by the
    // final result: the forcing loop below always finishes the list, so asking
    // afterwards would always answer "no cycles".
    let cycles = if stalled {
        find_cycles(&entries)
    } else {
        Vec::new()
    };
    ConflictOrder { sorted, cycles }
}

/// Builds the conflict-id graph by walking the dependency graph once.
#[allow(clippy::too_many_arguments)]
fn build(
    graph: &Graph,
    conflict_ids: &HashMap<NodeId, ConflictId>,
    node: NodeId,
    current: Option<usize>,
    depth: usize,
    visited: &mut [bool],
    entries: &mut Vec<Entry>,
    position: &mut HashMap<ConflictId, usize>,
) {
    // Each node's children are walked once. The edge below is still recorded for
    // an already-walked child, so the id graph stays complete.
    if visited[node.index()] {
        return;
    }
    visited[node.index()] = true;
    let depth = depth + 1;

    // By index: `pull_up` mutates `entries`, never the graph.
    for index in 0..graph.children(node).len() {
        let child = graph.children(node)[index];
        let Some(key) = conflict_ids.get(&child) else {
            continue;
        };
        let child_entry = match position.get(key) {
            Some(index) => {
                pull_up(entries, *index, depth);
                *index
            }
            None => {
                entries.push(Entry {
                    key: *key,
                    children: Vec::new(),
                    in_degree: 0,
                    min_depth: depth,
                });
                let index = entries.len() - 1;
                position.insert(*key, index);
                index
            }
        };

        if let Some(parent) = current {
            // Parallel edges between the same pair count once. A self edge is
            // kept: it is what makes an id its own cycle.
            if !entries[parent].children.contains(&child_entry) {
                entries[parent].children.push(child_entry);
                entries[child_entry].in_degree += 1;
            }
        }

        build(
            graph,
            conflict_ids,
            child,
            Some(child_entry),
            depth,
            visited,
            entries,
            position,
        );
    }
}

/// Lowers an id's recorded depth, and its descendants' with it.
///
/// Terminates because it only recurses on a strict decrease.
fn pull_up(entries: &mut Vec<Entry>, index: usize, depth: usize) {
    if depth >= entries[index].min_depth {
        return;
    }
    entries[index].min_depth = depth;
    for child in entries[index].children.clone() {
        pull_up(entries, child, depth + 1);
    }
}

/// A queue that always yields the id nearest the root, FIFO within one depth.
///
/// A deque rather than a vector: every id is popped from the front exactly once,
/// and `Vec::remove(0)` shifts the whole queue each time, which is O(n^2) over a
/// sort of n ids. Upstream uses an index-based ring buffer for the same reason.
#[derive(Default)]
struct RootQueue {
    items: std::collections::VecDeque<usize>,
}

impl RootQueue {
    fn push(&mut self, entries: &[Entry], index: usize) {
        let depth = entries[index].min_depth;
        // Land after every element at the same depth or nearer, so equal depths
        // stay in insertion order.
        let mut at = self.items.len();
        while at > 0 && entries[self.items[at - 1]].min_depth > depth {
            at -= 1;
        }
        self.items.insert(at, index);
    }

    fn pop(&mut self) -> Option<usize> {
        self.items.pop_front()
    }
}

/// Kahn's algorithm, breaking a stall by forcing the nearest remaining id.
///
/// Returns the order and whether the first pass stalled, which is how upstream
/// decides that cycles are present.
fn topological_sort(entries: &mut [Entry]) -> (Vec<ConflictId>, bool) {
    let mut sorted = Vec::with_capacity(entries.len());
    let mut queue = RootQueue::default();

    for index in 0..entries.len() {
        if entries[index].in_degree == 0 {
            queue.push(entries, index);
        }
    }
    process(entries, &mut queue, &mut sorted);
    let stalled = sorted.len() < entries.len();

    while sorted.len() < entries.len() {
        // Stalled: everything left is in a cycle. Force through the id nearest
        // the root, breaking ties on the fewest unresolved dependents and then
        // on discovery order.
        let Some(nearest) = (0..entries.len())
            .filter(|index| entries[*index].in_degree > 0)
            .min_by_key(|index| (entries[*index].min_depth, entries[*index].in_degree))
        else {
            break;
        };
        entries[nearest].in_degree = 0;
        queue.push(entries, nearest);
        process(entries, &mut queue, &mut sorted);
    }

    (sorted, stalled)
}

fn process(entries: &mut [Entry], queue: &mut RootQueue, sorted: &mut Vec<ConflictId>) {
    while let Some(index) = queue.pop() {
        sorted.push(entries[index].key);
        for child in entries[index].children.clone() {
            if entries[child].in_degree > 0 {
                entries[child].in_degree -= 1;
                if entries[child].in_degree == 0 {
                    queue.push(entries, child);
                }
            }
        }
    }
}

/// Finds cycles among the conflict ids.
///
/// Deliberately a heuristic, as upstream is: an id is expanded once, so a
/// reported set can be a superset of a minimal cycle and some cycles can be
/// missed. That is fine because the only consumer uses membership to *widen*
/// what it is willing to walk — being generous costs time, not correctness.
fn find_cycles(entries: &[Entry]) -> Vec<Vec<ConflictId>> {
    let mut cycles: Vec<Vec<ConflictId>> = Vec::new();
    let mut visited = vec![false; entries.len()];
    // The current path, as ids in visit order.
    let mut stack: Vec<usize> = Vec::new();
    for index in 0..entries.len() {
        walk_cycles(entries, index, &mut visited, &mut stack, &mut cycles);
    }
    cycles
}

fn walk_cycles(
    entries: &[Entry],
    index: usize,
    visited: &mut [bool],
    stack: &mut Vec<usize>,
    cycles: &mut Vec<Vec<ConflictId>>,
) {
    if let Some(at) = stack.iter().position(|held| *held == index) {
        // A back edge: everything from the first occurrence onwards is a cycle.
        let mut cycle: Vec<ConflictId> = stack[at..].iter().map(|i| entries[*i].key).collect();
        cycle.sort_unstable();
        if !cycles.contains(&cycle) {
            cycles.push(cycle);
        }
        return;
    }
    stack.push(index);
    if !visited[index] {
        visited[index] = true;
        for child in &entries[index].children {
            walk_cycles(entries, *child, visited, stack, cycles);
        }
    }
    stack.pop();
}

#[cfg(test)]
mod tests {
    use super::*;
    use jv_model::{Artifact, Dependency};

    use crate::conflict_id::mark_conflict_ids;
    use crate::graph::Node;

    fn node(name: &str, version: &str) -> Node {
        Node::dependency(
            Dependency::new("g", name, version),
            Artifact::new("g", name, version),
        )
    }

    /// Builds a graph from `(parent, child)` names, rooted at a synthetic root.
    fn graph_of(edges: &[(&str, &str)]) -> (Graph, HashMap<String, NodeId>) {
        let mut graph = Graph::new(Node::root());
        let mut named: HashMap<String, NodeId> = HashMap::new();
        for (parent, child) in edges {
            let parent_id = if *parent == "root" {
                graph.root()
            } else {
                *named
                    .entry((*parent).to_owned())
                    .or_insert_with(|| graph.add(node(parent, "1")))
            };
            let child_id = *named
                .entry((*child).to_owned())
                .or_insert_with(|| graph.add(node(child, "1")));
            graph.add_child(parent_id, child_id);
        }
        (graph, named)
    }

    fn order_of(graph: &Graph) -> (ConflictOrder, HashMap<NodeId, ConflictId>) {
        let ids = mark_conflict_ids(graph);
        let order = sort_conflict_ids(graph, &ids);
        (order, ids)
    }

    #[test]
    fn parents_come_before_children() {
        let (graph, named) = graph_of(&[("root", "a"), ("a", "b"), ("b", "c")]);
        let (order, ids) = order_of(&graph);
        assert_eq!(
            order.sorted,
            vec![ids[&named["a"]], ids[&named["b"]], ids[&named["c"]]]
        );
        assert!(order.cycles.is_empty());
    }

    #[test]
    fn nearer_ids_come_first() {
        // `deep` is reachable only through `a`; `b` is direct. Both are ready
        // once `a` is emitted, and the nearer one wins.
        let (graph, named) = graph_of(&[("root", "a"), ("a", "deep"), ("root", "b")]);
        let (order, ids) = order_of(&graph);
        let positions: HashMap<ConflictId, usize> = order
            .sorted
            .iter()
            .enumerate()
            .map(|(index, id)| (*id, index))
            .collect();
        assert!(positions[&ids[&named["b"]]] < positions[&ids[&named["deep"]]]);
    }

    #[test]
    fn a_diamond_settles_the_shared_id_after_both_parents() {
        let (graph, named) = graph_of(&[
            ("root", "left"),
            ("root", "right"),
            ("left", "shared"),
            ("right", "shared"),
        ]);
        let (order, ids) = order_of(&graph);
        let positions: HashMap<ConflictId, usize> = order
            .sorted
            .iter()
            .enumerate()
            .map(|(index, id)| (*id, index))
            .collect();
        assert!(positions[&ids[&named["left"]]] < positions[&ids[&named["shared"]]]);
        assert!(positions[&ids[&named["right"]]] < positions[&ids[&named["shared"]]]);
        assert!(order.cycles.is_empty());
    }

    #[test]
    fn every_id_appears_exactly_once() {
        let (graph, _) = graph_of(&[
            ("root", "a"),
            ("root", "b"),
            ("a", "c"),
            ("b", "c"),
            ("c", "d"),
        ]);
        let (order, ids) = order_of(&graph);
        let distinct: std::collections::BTreeSet<ConflictId> = ids.values().copied().collect();
        assert_eq!(order.sorted.len(), distinct.len());
        for id in &distinct {
            assert_eq!(order.sorted.iter().filter(|s| *s == id).count(), 1);
        }
    }

    #[test]
    fn a_cycle_is_reported_and_still_ordered() {
        // a -> b -> a, both reachable from the root.
        let (graph, named) = graph_of(&[("root", "a"), ("a", "b"), ("b", "a")]);
        let (order, ids) = order_of(&graph);
        // Nothing is dropped even though the sort could not complete on its own.
        assert_eq!(order.sorted.len(), 2);
        assert!(!order.cycles.is_empty());
        assert!(order.is_cyclic(ids[&named["a"]]));
        assert!(order.is_cyclic(ids[&named["b"]]));
    }

    #[test]
    fn an_acyclic_graph_with_cyclic_conflict_ids() {
        // The shape the corpus calls out: the dependency graph is a tree, but
        // two versions of the same artifact make the id graph cyclic.
        let mut graph = Graph::new(Node::root());
        let root = graph.root();
        let a1 = graph.add(node("a", "1"));
        let b = graph.add(node("b", "1"));
        let a2 = graph.add(node("a", "2"));
        graph.add_child(root, a1);
        graph.add_child(a1, b);
        graph.add_child(b, a2);

        let (order, ids) = order_of(&graph);
        assert_eq!(order.sorted.len(), 2);
        assert!(order.is_cyclic(ids[&a1]));
        assert!(order.is_cyclic(ids[&b]));
    }

    #[test]
    fn a_self_cycle_is_reported() {
        let mut graph = Graph::new(Node::root());
        let root = graph.root();
        let a = graph.add(node("a", "1"));
        graph.add_child(root, a);
        graph.add_child(a, a);

        let (order, ids) = order_of(&graph);
        assert_eq!(order.sorted.len(), 1);
        assert!(order.is_cyclic(ids[&a]));
    }

    #[test]
    fn an_empty_graph_orders_nothing() {
        let graph = Graph::new(Node::root());
        let (order, _) = order_of(&graph);
        assert!(order.sorted.is_empty());
        assert!(order.cycles.is_empty());
    }

    #[test]
    fn a_root_with_its_own_id_comes_first() {
        let mut graph = Graph::new(node("project", "1"));
        let root = graph.root();
        let a = graph.add(node("a", "1"));
        graph.add_child(root, a);

        let (order, ids) = order_of(&graph);
        assert_eq!(order.sorted.first(), Some(&ids[&root]));
        assert_eq!(order.sorted.len(), 2);
    }

    #[test]
    fn the_queue_prefers_shallower_then_insertion_order() {
        let entries = vec![
            Entry {
                key: 0,
                children: Vec::new(),
                in_degree: 0,
                min_depth: 2,
            },
            Entry {
                key: 1,
                children: Vec::new(),
                in_degree: 0,
                min_depth: 1,
            },
            Entry {
                key: 2,
                children: Vec::new(),
                in_degree: 0,
                min_depth: 1,
            },
        ];
        let mut queue = RootQueue::default();
        queue.push(&entries, 0);
        queue.push(&entries, 1);
        queue.push(&entries, 2);
        // Depth 1 before depth 2, and the two depth-1 entries keep their order.
        assert_eq!(queue.pop(), Some(1));
        assert_eq!(queue.pop(), Some(2));
        assert_eq!(queue.pop(), Some(0));
        assert_eq!(queue.pop(), None);
    }
}
