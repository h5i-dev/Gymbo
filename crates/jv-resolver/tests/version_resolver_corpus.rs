//! Checks conflict resolution against Maven Resolver's own corpus.
//!
//! Each case is one graph from
//! `maven-resolver-util/src/test/resources/transformer/version-resolver/`, with
//! the assertion its Java test makes ported alongside. Those assertions are the
//! point: they pin behaviour that is easy to implement plausibly and wrongly —
//! which node survives a cycle, whether a far range prunes a near soft version,
//! what happens when a hard constraint appears after a winner was already
//! chosen.
//!
//! Skips itself when `_reference/` is absent; `JV_REQUIRE_ORACLE=1` makes that a
//! failure, as CI does.

use std::path::{Path, PathBuf};

use jv_resolver::{Graph, NodeId, Verbosity, resolve_conflicts};
use jv_testkit::graph_dsl;

fn corpus() -> Option<PathBuf> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../_reference/maven-resolver")
        .join("maven-resolver-util/src/test/resources/transformer/version-resolver");
    path.is_dir().then_some(path)
}

/// Loads one case, or skips the test when the corpus is not present.
macro_rules! case {
    ($name:expr) => {
        match load($name) {
            Some(graph) => graph,
            None => return,
        }
    };
}

fn load(name: &str) -> Option<Graph> {
    let Some(corpus) = corpus() else {
        if std::env::var_os("JV_REQUIRE_ORACLE").is_some() {
            panic!("JV_REQUIRE_ORACLE is set but _reference/maven-resolver is missing");
        }
        eprintln!("skipping: _reference/ not present (see docs/development.md)");
        return None;
    };
    let text = std::fs::read_to_string(corpus.join(name))
        .unwrap_or_else(|error| panic!("cannot read {name}: {error}"));
    Some(graph_dsl::parse(&text).unwrap_or_else(|error| panic!("cannot parse {name}: {error}")))
}

/// The artifact id of a node, for readable assertions.
fn artifact_id(graph: &Graph, id: NodeId) -> String {
    graph
        .node(id)
        .artifact
        .as_ref()
        .map(|artifact| artifact.artifact_id.clone())
        .unwrap_or_default()
}

fn version(graph: &Graph, id: NodeId) -> String {
    graph
        .node(id)
        .artifact
        .as_ref()
        .map(|artifact| artifact.version.clone())
        .unwrap_or_default()
}

/// The path from the root down to the first node with this artifact id,
/// deepest first — the shape upstream's `find` helper returns.
fn trail(graph: &Graph, artifact: &str) -> Vec<NodeId> {
    let mut path = Vec::new();
    let mut guard = Vec::new();
    if search(graph, graph.root(), artifact, &mut path, &mut guard) {
        path
    } else {
        Vec::new()
    }
}

fn search(
    graph: &Graph,
    node: NodeId,
    artifact: &str,
    path: &mut Vec<NodeId>,
    guard: &mut Vec<NodeId>,
) -> bool {
    path.insert(0, node);
    if artifact_id(graph, node) == artifact {
        return true;
    }
    if !guard.contains(&node) {
        guard.push(node);
        for child in graph.children(node).to_vec() {
            if search(graph, child, artifact, path, guard) {
                return true;
            }
        }
        guard.pop();
    }
    path.remove(0);
    false
}

fn children_of(graph: &Graph, id: NodeId) -> Vec<String> {
    graph
        .children(id)
        .iter()
        .map(|child| artifact_id(graph, *child))
        .collect()
}

#[test]
fn highest_version_wins_among_siblings() {
    let mut graph = case!("sibling-versions.txt");
    resolve_conflicts(&mut graph, Verbosity::None).expect("resolves");
    let root = graph.root();
    assert_eq!(graph.children(root).len(), 1);
    assert_eq!(version(&graph, graph.children(root)[0]), "3");
}

#[test]
fn the_nearest_version_can_sit_below_a_loser() {
    let mut graph = case!("nearest-underneath-loser-a.txt");
    resolve_conflicts(&mut graph, Verbosity::None).expect("resolves");
    assert_eq!(trail(&graph, "j").len(), 5);
}

#[test]
fn the_nearest_version_survives_a_removed_ancestor() {
    let mut graph = case!("nearest-underneath-loser-b.txt");
    resolve_conflicts(&mut graph, Verbosity::None).expect("resolves");
    assert_eq!(trail(&graph, "j").len(), 5);
}

#[test]
fn a_late_range_falls_back_to_the_nearest_acceptable_version() {
    // A hard constraint discovered after a winner was chosen invalidates it, and
    // the rescan must pick the nearest *acceptable* one rather than the first.
    let mut graph = case!("range-backtracking.txt");
    resolve_conflicts(&mut graph, Verbosity::None).expect("resolves");
    let found = trail(&graph, "x");
    assert_eq!(found.len(), 3);
    assert_eq!(version(&graph, found[0]), "2");
}

#[test]
fn cyclic_conflict_ids_still_resolve() {
    let mut graph = case!("conflict-id-cycle.txt");
    resolve_conflicts(&mut graph, Verbosity::None).expect("resolves");
    let root = graph.root();
    assert_eq!(children_of(&graph, root), vec!["a", "b"]);
    for child in graph.children(root).to_vec() {
        assert!(graph.children(child).is_empty());
    }
}

#[test]
fn compatible_hard_constraints_resolve() {
    let mut graph = case!("ranges.txt");
    resolve_conflicts(&mut graph, Verbosity::None).expect("resolves");
}

#[test]
fn incompatible_hard_constraints_fail() {
    let mut graph = case!("unsolvable.txt");
    assert!(resolve_conflicts(&mut graph, Verbosity::None).is_err());
}

#[test]
fn incompatible_hard_constraints_fail_even_with_a_cycle() {
    let mut graph = case!("unsolvable-with-cycle.txt");
    assert!(resolve_conflicts(&mut graph, Verbosity::None).is_err());
}

#[test]
fn a_whole_conflict_group_can_vanish() {
    let mut graph = case!("dead-conflict-group.txt");
    resolve_conflicts(&mut graph, Verbosity::None).expect("resolves");
    let root = graph.root();
    assert_eq!(children_of(&graph, root), vec!["a", "b"]);
    for child in graph.children(root).to_vec() {
        assert!(graph.children(child).is_empty());
    }
}

#[test]
fn a_farther_range_prunes_a_nearer_soft_version() {
    let mut graph = case!("soft-vs-range.txt");
    resolve_conflicts(&mut graph, Verbosity::None).expect("resolves");
    let root = graph.root();
    let children = graph.children(root).to_vec();
    assert_eq!(children_of(&graph, root), vec!["a", "b"]);
    assert!(graph.children(children[0]).is_empty());
    assert_eq!(graph.children(children[1]).len(), 1);
}

#[test]
fn a_cyclic_graph_resolves_to_a_finite_tree() {
    let mut graph = case!("cycle.txt");
    resolve_conflicts(&mut graph, Verbosity::None).expect("resolves");
    let root = graph.root();
    let children = graph.children(root).to_vec();
    assert_eq!(children.len(), 2);
    assert_eq!(graph.children(children[0]).len(), 1);
    let grandchild = graph.children(children[0])[0];
    assert!(graph.children(grandchild).is_empty());
    assert!(graph.children(children[1]).is_empty());
}

#[test]
fn a_self_loop_leaves_nothing() {
    let mut graph = case!("loop.txt");
    resolve_conflicts(&mut graph, Verbosity::None).expect("resolves");
    assert!(graph.children(graph.root()).is_empty());
}

#[test]
fn overlapping_cycles_resolve() {
    let mut graph = case!("overlapping-cycles.txt");
    resolve_conflicts(&mut graph, Verbosity::None).expect("resolves");
    assert_eq!(graph.children(graph.root()).len(), 2);
}

#[test]
fn scope_derivation_waits_for_version_selection() {
    // The case the file is named for: scopes cannot be settled before versions
    // are, because choosing a version changes which paths exist.
    let mut graph = case!("scope-vs-version.txt");
    resolve_conflicts(&mut graph, Verbosity::None).expect("resolves");
    let found = trail(&graph, "y");
    assert_eq!(found.len(), 3);
    for node in &found[..2] {
        assert_eq!(
            graph.node(*node).scope(),
            jv_model::Scope::Test,
            "expected test scope on {}",
            artifact_id(&graph, *node)
        );
    }
}

#[test]
fn verbose_mode_keeps_the_loser_as_a_marker() {
    let mut graph = case!("verbose.txt");
    resolve_conflicts(&mut graph, Verbosity::Standard).expect("resolves");
    let root = graph.root();
    let children = graph.children(root).to_vec();
    assert_eq!(children.len(), 2);

    assert_eq!(graph.children(children[0]).len(), 1);
    let winner = graph.children(children[0])[0];
    assert_eq!(graph.node(winner).scope(), jv_model::Scope::Test);
    // The scope it had before resolution changed it.
    assert_eq!(
        graph.node(winner).original_scope,
        Some(jv_model::Scope::Compile)
    );

    assert_eq!(graph.children(children[1]).len(), 1);
    let loser = graph.children(children[1])[0];
    assert!(
        graph.children(loser).is_empty(),
        "a loser marker keeps no children"
    );
    assert!(
        graph.node(loser).omitted_for.is_some(),
        "a loser records what beat it"
    );
    assert_eq!(
        graph.node(loser).original_scope,
        Some(jv_model::Scope::Compile)
    );
}
