//! Conflict resolution when two paths reach the same child list.
//!
//! Upstream's unit of identity is the child *list*, not the node, and the
//! collector shares one list between a cycle node and its ancestor and between a
//! pool hit and the subtree it hit. Everything about depth accounting and scope
//! derivation then depends on the two ends being recognised as one graph node.
//!
//! No corpus case exercises this — the graph DSL builds trees, so every node has
//! its own list — which is why two real defects lived here for a while.
//!
//! A caveat worth recording, because it cost time: these tests pin the *outcome*,
//! not the mechanism. A node reached by two paths is necessarily one conflict id
//! with two items, and `sort_conflict_ids` puts it before anything beneath it, so
//! by the time a deeper artifact is resolved nearest-wins has usually already
//! pruned one of the two edges — which masks a depth-accounting error rather than
//! exposing it. Do not read a pass here as proof that the accounting is right.

use jv_model::{Artifact, Dependency, Scope};
use jv_resolver::{Graph, Node, NodeId, Verbosity, resolve_conflicts};

/// A node for `g:<id>:<version>` at some scope.
fn dependency(id: &str, version: &str, scope: Scope) -> Node {
    let artifact = Artifact::new("g", id, version);
    Node::dependency(
        Dependency {
            group_id: "g".to_owned(),
            artifact_id: id.to_owned(),
            version: Some(version.to_owned()),
            scope: Some(scope),
            ..Dependency::default()
        },
        artifact,
    )
}

fn add(graph: &mut Graph, parent: NodeId, id: &str, version: &str, scope: Scope) -> NodeId {
    let node = graph.add(dependency(id, version, scope));
    graph.add_child(parent, node);
    node
}

/// The surviving version of an artifact, or `None` when it was pruned.
fn surviving(graph: &Graph, artifact_id: &str) -> Option<String> {
    let mut found = None;
    graph.walk(|id, _depth| {
        let node = graph.node(id);
        if node.omitted_for.is_some() {
            return;
        }
        if let Some(artifact) = &node.artifact {
            if artifact.artifact_id == artifact_id {
                found = Some(artifact.version.clone());
            }
        }
    });
    found
}

#[test]
fn a_nearer_second_path_to_a_shared_subtree_wins() {
    // Two paths reach `s`'s child list: a long one through m→n→o and a short one
    // straight from the root. The short one has to be recognised, because the
    // depth it gives `x` decides which version of `x` survives.
    //
    //   root ├─ deep(1) ─ mid(2) ─ far(3) ─ s(4, shared) ─ x:1(5)
    //        ├─ near(1) ─────────────────── s(2, shared) ─ x:1(3)
    //        └─ o1(1) ─── o2(2) ─── o3(3) ─ x:2(4)
    //
    // Through the short path `x:1` sits at depth 3 and beats `x:2` at 4.
    let mut graph = Graph::new(Node::root());
    let root = graph.root();

    let deep = add(&mut graph, root, "deep", "1", Scope::Compile);
    let mid = add(&mut graph, deep, "mid", "1", Scope::Compile);
    let far = add(&mut graph, mid, "far", "1", Scope::Compile);
    let owner = add(&mut graph, far, "s", "1", Scope::Compile);
    add(&mut graph, owner, "x", "1", Scope::Compile);

    let near = add(&mut graph, root, "near", "1", Scope::Compile);
    let sharer = add(&mut graph, near, "s", "1", Scope::Compile);
    graph.share_children(owner, sharer);

    let first = add(&mut graph, root, "o1", "1", Scope::Compile);
    let second = add(&mut graph, first, "o2", "1", Scope::Compile);
    let third = add(&mut graph, second, "o3", "1", Scope::Compile);
    add(&mut graph, third, "x", "2", Scope::Compile);

    resolve_conflicts(&mut graph, Verbosity::None).expect("resolution");
    assert_eq!(
        surviving(&graph, "x").as_deref(),
        Some("1"),
        "the nearer path to the shared subtree was not recognised"
    );
}

#[test]
fn sharing_is_symmetric() {
    // Both ends of a shared list must report the same identity. Setting the key
    // on only one of them — which is what happened before — left conflict
    // resolution treating them as two graph nodes however the list was shared, so
    // each got its own depth and scope accounting.
    let mut graph = Graph::new(Node::root());
    let root = graph.root();
    let owner = add(&mut graph, root, "a", "1", Scope::Compile);
    let sharer = add(&mut graph, root, "a", "1", Scope::Compile);
    assert_ne!(
        graph.children_identity(owner),
        graph.children_identity(sharer)
    );

    graph.share_children(owner, sharer);
    assert_eq!(
        graph.children_identity(owner),
        graph.children_identity(sharer)
    );
}

#[test]
fn a_shared_subtree_does_not_stop_an_unrelated_second_visit() {
    // The path stack must be per-path, not a global visited set: a diamond
    // reaches `leaf` twice by two different routes and both have to be walked.
    //
    //   root ├─ left ── shared(shared) ─ leaf:1
    //        └─ right ─ shared(shared)
    let mut graph = Graph::new(Node::root());
    let root = graph.root();

    let left = add(&mut graph, root, "left", "1", Scope::Compile);
    let owner = add(&mut graph, left, "shared", "1", Scope::Compile);
    add(&mut graph, owner, "leaf", "1", Scope::Compile);

    let right = add(&mut graph, root, "right", "1", Scope::Runtime);
    let sharer = add(&mut graph, right, "shared", "1", Scope::Runtime);
    graph.share_children(owner, sharer);

    resolve_conflicts(&mut graph, Verbosity::None).expect("resolution");
    // Reached through a compile path, so compile is what it keeps — the runtime
    // path must not have been the only one accounted for.
    assert_eq!(surviving(&graph, "leaf").as_deref(), Some("1"));
    let mut leaf_scope = None;
    graph.walk(|id, _| {
        let node = graph.node(id);
        if node.omitted_for.is_none()
            && node
                .artifact
                .as_ref()
                .is_some_and(|a| a.artifact_id == "leaf")
        {
            leaf_scope = Some(node.scope());
        }
    });
    assert_eq!(leaf_scope, Some(Scope::Compile));
}
