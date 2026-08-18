//! The dependency graph.
//!
//! Nodes live in an arena and refer to each other by index. That is not an
//! optimization: the graph genuinely is not a tree. Breadth-first collection
//! shares subtrees between paths that reach the same artifact, and a POM cycle
//! makes a node its own descendant, so an owning `Vec<Node>` of children would
//! either duplicate work or fail to represent the shape at all.
//!
//! Mirrors `org.eclipse.aether.graph.DependencyNode`, including the bookkeeping
//! that only verbose output reads: what a version or scope was *before*
//! `<dependencyManagement>` changed it, and which coordinates an artifact was
//! relocated from.

use jv_model::{Artifact, Dependency, Ga, Scope};

/// An index into a [`Graph`]'s arena.
///
/// Indices are only meaningful for the graph that produced them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NodeId(u32);

impl NodeId {
    /// The arena position, for sibling modules that need to key a dense array by
    /// node. Deliberately not public: outside the crate an id is opaque.
    pub(crate) fn index(self) -> usize {
        self.0 as usize
    }
}

impl std::fmt::Display for NodeId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "#{}", self.0)
    }
}

/// Which of a dependency's fields `<dependencyManagement>` supplied.
///
/// Verbose tree output annotates managed values, so the fact that a version was
/// managed has to survive the merge that applied it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ManagedFlags {
    pub version: bool,
    pub scope: bool,
    pub optional: bool,
    pub exclusions: bool,
    pub properties: bool,
}

impl ManagedFlags {
    /// Whether management touched anything at all.
    pub fn any(&self) -> bool {
        self.version || self.scope || self.optional || self.exclusions || self.properties
    }
}

/// What a dependency looked like before management rewrote it.
///
/// Each field is `Some` only when management actually replaced a *different*
/// value, which is what lets verbose output say "version managed from 1.0".
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Premanaged {
    pub version: Option<String>,
    pub scope: Option<Scope>,
    pub optional: Option<bool>,
}

impl Premanaged {
    pub fn is_empty(&self) -> bool {
        self.version.is_none() && self.scope.is_none() && self.optional.is_none()
    }
}

/// One node of the graph.
#[derive(Clone, Debug, Default)]
pub struct Node {
    /// The dependency that led here. `None` for a synthetic root, which is how
    /// the corpus spells a graph whose root is not itself an artifact.
    pub dependency: Option<Dependency>,
    /// The artifact this node resolved to. `None` alongside a `None`
    /// `dependency` marks the synthetic root.
    pub artifact: Option<Artifact>,
    /// The version or range exactly as declared, before a range was narrowed to
    /// a concrete version.
    pub version_constraint: Option<String>,
    pub children: Vec<NodeId>,
    pub managed: ManagedFlags,
    pub premanaged: Premanaged,
    /// Coordinates this artifact was relocated *from*, oldest first.
    pub relocations: Vec<Artifact>,
    /// Other coordinates that denote the same artifact, which conflict grouping
    /// treats as equivalent.
    pub aliases: Vec<Artifact>,
    /// The version that beat this node, set only on a loser the graph kept for
    /// verbose output.
    ///
    /// Upstream stores this as the `winner` node datum and decides between
    /// "omitted for duplicate" and "omitted for conflict with X" by comparing it
    /// against the loser's own version, which is why the winner's *version* is
    /// recorded rather than a flag.
    pub omitted_for: Option<String>,
    /// The scope this node had before conflict resolution changed it, reported
    /// as "scope updated from".
    pub original_scope: Option<Scope>,
    /// A scope conflict resolution declined to apply, reported as "scope not
    /// updated to".
    pub ignored_scope: Option<Scope>,
    /// Which shared child list this node uses, when the collector reused one.
    ///
    /// Maven's collector gives two nodes for the same resolved artifact the
    /// *same* children list object while leaving them separate nodes, because
    /// their derived scopes can differ. Conflict resolution then keys its cycle
    /// detection and memoisation on that list rather than on the node — so two
    /// such nodes count as one graph node. Recording the sharing explicitly is
    /// what lets [`Graph::children_identity`] reproduce that.
    pub children_key: Option<u32>,
}

impl Node {
    /// A synthetic root: no artifact of its own, only children.
    pub fn root() -> Self {
        Self::default()
    }

    /// A node for a declared dependency.
    pub fn dependency(dependency: Dependency, artifact: Artifact) -> Self {
        Self {
            version_constraint: dependency.version.clone(),
            dependency: Some(dependency),
            artifact: Some(artifact),
            ..Default::default()
        }
    }

    /// The effective scope, or `compile` when this node has no dependency.
    pub fn scope(&self) -> Scope {
        self.dependency
            .as_ref()
            .map_or(Scope::Compile, Dependency::scope_or_default)
    }

    pub fn is_optional(&self) -> bool {
        self.dependency
            .as_ref()
            .is_some_and(Dependency::is_optional)
    }

    /// The group and artifact, when this node has an artifact.
    pub fn ga(&self) -> Option<Ga<'_>> {
        self.artifact.as_ref().map(Artifact::ga)
    }

    /// Whether this is the synthetic root.
    pub fn is_synthetic_root(&self) -> bool {
        self.artifact.is_none()
    }
}

/// A dependency graph, rooted at one node.
#[derive(Clone, Debug)]
pub struct Graph {
    nodes: Vec<Node>,
    root: NodeId,
    /// The next child-list identity to hand out. See [`Graph::share_children`].
    next_children_key: u32,
}

impl Graph {
    /// Creates a graph containing only `root`.
    pub fn new(root: Node) -> Self {
        Self {
            nodes: vec![root],
            root: NodeId(0),
            next_children_key: 0,
        }
    }

    pub fn root(&self) -> NodeId {
        self.root
    }

    /// Adds a node, returning its id. It starts unattached; use
    /// [`Graph::add_child`] to link it.
    pub fn add(&mut self, node: Node) -> NodeId {
        let id = NodeId(
            u32::try_from(self.nodes.len()).expect("a dependency graph never approaches 4G nodes"),
        );
        self.nodes.push(node);
        id
    }

    /// Appends `child` to `parent`'s children.
    pub fn add_child(&mut self, parent: NodeId, child: NodeId) {
        self.nodes[parent.index()].children.push(child);
    }

    pub fn node(&self, id: NodeId) -> &Node {
        &self.nodes[id.index()]
    }

    pub fn node_mut(&mut self, id: NodeId) -> &mut Node {
        &mut self.nodes[id.index()]
    }

    pub fn children(&self, id: NodeId) -> &[NodeId] {
        &self.nodes[id.index()].children
    }

    /// Replaces a node's children wholesale, which is how conflict resolution
    /// prunes losers.
    pub fn set_children(&mut self, id: NodeId, children: Vec<NodeId>) {
        self.nodes[id.index()].children = children;
    }

    /// The identity conflict resolution treats as "the same graph node".
    ///
    /// That identity is the *child list*, not the node: the collector shares one
    /// list between nodes for the same resolved artifact, and upstream's cycle
    /// detection and scope memoisation both key on it. A node with no shared key
    /// is its own identity.
    pub fn children_identity(&self, id: NodeId) -> u64 {
        match self.nodes[id.index()].children_key {
            Some(key) => u64::from(key),
            // Disjoint from any allocated key, which is a plain u32.
            None => (1u64 << 32) | id.index() as u64,
        }
    }

    /// Makes two nodes share one child-list identity.
    ///
    /// Both ends get the *same* key. Setting it on only one of them — which is
    /// what happened before — left the two with different identities however the
    /// list was shared, so conflict resolution gave each its own depth and scope
    /// accounting and produced a different answer than Maven for any graph with a
    /// cycle or a shared subtree.
    pub fn share_children(&mut self, owner: NodeId, sharer: NodeId) {
        let key = match self.nodes[owner.index()].children_key {
            Some(key) => key,
            None => {
                let key = self.next_children_key;
                self.next_children_key += 1;
                self.nodes[owner.index()].children_key = Some(key);
                key
            }
        };
        self.nodes[sharer.index()].children_key = Some(key);
        let children = self.nodes[owner.index()].children.clone();
        self.nodes[sharer.index()].children = children;
    }

    /// The number of nodes in the arena, including any left unreachable by
    /// pruning.
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// Visits every node reachable from the root in preorder, passing the depth.
    ///
    /// A node reached twice is visited twice, because the two paths are different
    /// facts about the graph; a node reached through itself is visited once and
    /// then cut off, so a cycle cannot hang the walk.
    pub fn walk<F: FnMut(NodeId, usize)>(&self, mut visit: F) {
        let mut path = Vec::new();
        self.walk_from(self.root, 0, &mut path, &mut visit);
    }

    fn walk_from<F: FnMut(NodeId, usize)>(
        &self,
        id: NodeId,
        depth: usize,
        path: &mut Vec<NodeId>,
        visit: &mut F,
    ) {
        visit(id, depth);
        if path.contains(&id) {
            // Already on the current path: descending again would not terminate.
            return;
        }
        path.push(id);
        for child in self.nodes[id.index()].children.clone() {
            self.walk_from(child, depth + 1, path, visit);
        }
        path.pop();
    }

    /// Every node reachable from the root, in preorder, with its depth.
    pub fn preorder(&self) -> Vec<(NodeId, usize)> {
        let mut out = Vec::new();
        self.walk(|id, depth| out.push((id, depth)));
        out
    }

    /// Drops nodes no longer reachable from the root, renumbering the rest.
    ///
    /// Pruning leaves orphans behind; compacting keeps a long-lived graph from
    /// carrying them, and makes two graphs built by different routes comparable.
    ///
    /// **Every [`NodeId`] taken before this call is invalidated.** They are not
    /// merely stale — renumbering makes an old id address a *different* node, so
    /// reusing one reads as success while silently pointing somewhere else.
    pub fn compact(&mut self) {
        let mut mapping = vec![None; self.nodes.len()];
        let mut order = Vec::new();
        let mut stack = vec![self.root];
        while let Some(id) = stack.pop() {
            if mapping[id.index()].is_some() {
                continue;
            }
            mapping[id.index()] = Some(NodeId(
                u32::try_from(order.len()).expect("node count fits in u32"),
            ));
            order.push(id);
            // Reverse so the arena ends up in preorder rather than mirrored.
            for child in self.nodes[id.index()].children.iter().rev() {
                stack.push(*child);
            }
        }

        let mut compacted = Vec::with_capacity(order.len());
        for id in &order {
            let mut node = self.nodes[id.index()].clone();
            node.children = node
                .children
                .iter()
                .filter_map(|child| mapping[child.index()])
                .collect();
            compacted.push(node);
        }
        self.nodes = compacted;
        self.root = NodeId(0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn artifact(name: &str, version: &str) -> Artifact {
        Artifact::new("g", name, version)
    }

    fn node(name: &str, version: &str) -> Node {
        Node::dependency(Dependency::new("g", name, version), artifact(name, version))
    }

    /// root -> a -> b, plus root -> c
    fn sample() -> (Graph, NodeId, NodeId, NodeId) {
        let mut graph = Graph::new(Node::root());
        let root = graph.root();
        let a = graph.add(node("a", "1"));
        let b = graph.add(node("b", "1"));
        let c = graph.add(node("c", "1"));
        graph.add_child(root, a);
        graph.add_child(a, b);
        graph.add_child(root, c);
        (graph, a, b, c)
    }

    #[test]
    fn preorder_reports_depth() {
        let (graph, a, b, c) = sample();
        let order: Vec<(usize, usize)> = graph
            .preorder()
            .into_iter()
            .map(|(id, depth)| (id.index(), depth))
            .collect();
        assert_eq!(
            order,
            vec![(0, 0), (a.index(), 1), (b.index(), 2), (c.index(), 1)]
        );
    }

    #[test]
    fn a_shared_node_is_visited_once_per_path() {
        let mut graph = Graph::new(Node::root());
        let root = graph.root();
        let shared = graph.add(node("shared", "1"));
        let a = graph.add(node("a", "1"));
        let b = graph.add(node("b", "1"));
        graph.add_child(root, a);
        graph.add_child(root, b);
        graph.add_child(a, shared);
        graph.add_child(b, shared);
        let visits = graph
            .preorder()
            .into_iter()
            .filter(|(id, _)| *id == shared)
            .count();
        assert_eq!(visits, 2);
    }

    #[test]
    fn a_cycle_does_not_hang_the_walk() {
        let mut graph = Graph::new(Node::root());
        let root = graph.root();
        let a = graph.add(node("a", "1"));
        let b = graph.add(node("b", "1"));
        graph.add_child(root, a);
        graph.add_child(a, b);
        // b depends back on a.
        graph.add_child(b, a);
        let order = graph.preorder();
        // The second visit to `a` is reported but not descended into.
        assert_eq!(order.len(), 4);
        assert_eq!(order.last().map(|(id, depth)| (*id, *depth)), Some((a, 3)));
    }

    #[test]
    fn a_self_loop_terminates() {
        let mut graph = Graph::new(Node::root());
        let root = graph.root();
        let a = graph.add(node("a", "1"));
        graph.add_child(root, a);
        graph.add_child(a, a);
        assert_eq!(graph.preorder().len(), 3);
    }

    #[test]
    fn compact_drops_orphans_and_renumbers() {
        let (mut graph, a, _b, c) = sample();
        // Prune `a`, orphaning it and its child.
        graph.set_children(graph.root(), vec![c]);
        assert_eq!(graph.len(), 4);
        graph.compact();
        assert_eq!(graph.len(), 2);
        let names: Vec<String> = graph
            .preorder()
            .into_iter()
            .filter_map(|(id, _)| {
                graph
                    .node(id)
                    .artifact
                    .as_ref()
                    .map(|a| a.artifact_id.clone())
            })
            .collect();
        assert_eq!(names, vec!["c"]);
        // Compaction renumbers, so an id held across it silently means something
        // else afterwards — here `a`'s old id now addresses `c`.
        assert_eq!(
            graph
                .node(a)
                .artifact
                .as_ref()
                .map(|artifact| artifact.artifact_id.as_str()),
            Some("c")
        );
    }

    #[test]
    fn compact_keeps_preorder() {
        let (mut graph, _a, _b, _c) = sample();
        graph.compact();
        let names: Vec<String> = graph
            .preorder()
            .into_iter()
            .filter_map(|(id, _)| {
                graph
                    .node(id)
                    .artifact
                    .as_ref()
                    .map(|a| a.artifact_id.clone())
            })
            .collect();
        assert_eq!(names, vec!["a", "b", "c"]);
    }

    #[test]
    fn nodes_sharing_a_child_list_share_an_identity() {
        let mut graph = Graph::new(Node::root());
        let first = graph.add(node("a", "1"));
        let second = graph.add(node("a", "1"));
        // Distinct nodes are distinct identities by default.
        assert_ne!(
            graph.children_identity(first),
            graph.children_identity(second)
        );
        // Until the collector says they share a child list.
        graph.node_mut(first).children_key = Some(7);
        graph.node_mut(second).children_key = Some(7);
        assert_eq!(
            graph.children_identity(first),
            graph.children_identity(second)
        );
        // A shared key never collides with an unshared node's identity.
        let third = graph.add(node("b", "1"));
        assert_ne!(
            graph.children_identity(third),
            graph.children_identity(first)
        );
    }

    #[test]
    fn synthetic_root_has_no_artifact() {
        let graph = Graph::new(Node::root());
        assert!(graph.node(graph.root()).is_synthetic_root());
        assert_eq!(graph.node(graph.root()).scope(), Scope::Compile);
    }

    #[test]
    fn managed_flags_and_premanaged_track_changes() {
        let mut candidate = node("a", "2");
        candidate.managed.version = true;
        candidate.premanaged.version = Some("1".to_owned());
        assert!(candidate.managed.any());
        assert!(!candidate.premanaged.is_empty());
        assert_eq!(candidate.version_constraint.as_deref(), Some("2"));
    }
}
