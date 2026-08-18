//! Checks conflict-id ordering and optionality against Maven's corpus.
//!
//! Two small suites that pin behaviour the version and scope corpora do not
//! reach: the order competitions are settled in, and how an optional flag
//! survives conflict resolution.
//!
//! Skips itself when `_reference/` is absent; `JV_REQUIRE_ORACLE=1` makes that a
//! failure, as CI does.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use jv_resolver::{
    ConflictId, Graph, NodeId, Verbosity, mark_conflict_ids, resolve_conflicts, sort_conflict_ids,
};
use jv_testkit::graph_dsl;

fn load(directory: &str, name: &str) -> Option<Graph> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../_reference/maven-resolver")
        .join("maven-resolver-util/src/test/resources/transformer")
        .join(directory);
    let path: PathBuf = root.join(name);
    if !path.is_file() {
        if std::env::var_os("JV_REQUIRE_ORACLE").is_some() {
            panic!("JV_REQUIRE_ORACLE is set but {} is missing", path.display());
        }
        eprintln!("skipping: _reference/ not present (see docs/development.md)");
        return None;
    }
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("cannot read {name}: {error}"));
    Some(graph_dsl::parse(&text).unwrap_or_else(|error| panic!("cannot parse {name}: {error}")))
}

/// Renders a conflict id the way upstream's test marker spells it:
/// `groupId:artifactId:classifier:extension`.
fn label(graph: &Graph, ids: &HashMap<NodeId, ConflictId>, id: ConflictId) -> String {
    for (node, candidate) in ids {
        if *candidate != id {
            continue;
        }
        if let Some(artifact) = &graph.node(*node).artifact {
            return format!(
                "{}:{}:{}:{}",
                artifact.group_id, artifact.artifact_id, artifact.classifier, artifact.extension
            );
        }
    }
    String::new()
}

/// Asserts the sorted order, where `*` accepts any id at that position.
fn expect_order(name: &str, expected: &[&str]) {
    let Some(graph) = load("conflict-id-sorter", name) else {
        return;
    };
    let ids = mark_conflict_ids(&graph);
    let order = sort_conflict_ids(&graph, &ids);
    let actual: Vec<String> = order
        .sorted
        .iter()
        .map(|id| label(&graph, &ids, *id))
        .collect();

    assert_eq!(
        actual.len(),
        expected.len(),
        "{name}: expected {} conflict groups, got {actual:?}",
        expected.len()
    );
    for (index, wanted) in expected.iter().enumerate() {
        if *wanted != "*" {
            assert_eq!(&actual[index], wanted, "{name}: position {index}");
        }
    }
}

fn expect_cycle(name: &str, cyclic: bool) {
    let Some(graph) = load("conflict-id-sorter", name) else {
        return;
    };
    let ids = mark_conflict_ids(&graph);
    let order = sort_conflict_ids(&graph, &ids);
    assert_eq!(
        !order.cycles.is_empty(),
        cyclic,
        "{name}: cycles were {:?}",
        order.cycles
    );
}

#[test]
fn simple_graphs_sort_nearest_first() {
    expect_order(
        "simple.txt",
        &["gid2:aid::jar", "gid:aid::jar", "gid:aid2::jar"],
    );
    expect_cycle("simple.txt", false);
}

#[test]
fn a_cyclic_pair_is_reported() {
    expect_order("cycle.txt", &["gid:aid::jar", "gid2:aid::jar"]);
    expect_cycle("cycle.txt", true);
}

#[test]
fn several_cycles_still_produce_a_complete_order() {
    // Upstream only pins the last position here; the rest may legitimately fall
    // out in any order once cycles are being broken.
    expect_order("cycles.txt", &["*", "*", "*", "gid:aid::jar"]);
    expect_cycle("cycles.txt", true);
}

#[test]
fn a_graph_with_no_conflicts_still_orders_every_id() {
    expect_order(
        "no-conflicts.txt",
        &[
            "gid:aid::jar",
            "gid3:aid::jar",
            "gid2:aid::jar",
            "gid4:aid::jar",
        ],
    );
    expect_cycle("no-conflicts.txt", false);
}

/// Follows child indices from the root.
fn at(graph: &Graph, coordinates: &[usize]) -> NodeId {
    let mut node = graph.root();
    for index in coordinates {
        node = graph.children(node)[*index];
    }
    node
}

fn resolved_optionality(name: &str) -> Option<Graph> {
    let mut graph = load("optionality-selector", name)?;
    resolve_conflicts(&mut graph, Verbosity::None)
        .unwrap_or_else(|error| panic!("{name} did not resolve: {error}"));
    Some(graph)
}

#[test]
fn optionality_is_inherited_down_a_path() {
    let Some(graph) = resolved_optionality("derive.txt") else {
        return;
    };
    assert_eq!(graph.children(graph.root()).len(), 2);
    // Everything under an optional dependency is optional...
    assert!(graph.node(at(&graph, &[0])).is_optional());
    assert!(graph.node(at(&graph, &[0, 0])).is_optional());
    // ...and nothing under a required one is.
    assert!(!graph.node(at(&graph, &[1])).is_optional());
    assert!(!graph.node(at(&graph, &[1, 0])).is_optional());
}

#[test]
fn one_required_occurrence_makes_the_winner_required() {
    let Some(graph) = resolved_optionality("conflict.txt") else {
        return;
    };
    assert_eq!(graph.children(graph.root()).len(), 2);
    assert!(graph.node(at(&graph, &[0])).is_optional());
    // Reached as optional on one path and required on another: required wins.
    assert!(!graph.node(at(&graph, &[0, 0])).is_optional());
}

#[test]
fn a_direct_declaration_decides_optionality() {
    let Some(graph) = resolved_optionality("conflict-direct-dep.txt") else {
        return;
    };
    assert_eq!(graph.children(graph.root()).len(), 2);
    // A direct dependency's own flag is authoritative, exactly as its scope is.
    assert!(graph.node(at(&graph, &[1])).is_optional());
}

// --------------------------------------------------------- conflict marking

fn marked(name: &str) -> Option<(Graph, HashMap<NodeId, ConflictId>)> {
    let graph = load("conflict-marker", name)?;
    let ids = mark_conflict_ids(&graph);
    Some((graph, ids))
}

#[test]
fn unrelated_artifacts_get_separate_ids() {
    let Some((graph, ids)) = marked("simple.txt") else {
        return;
    };
    let root = graph.root();
    // The root competes with nothing.
    assert!(!ids.contains_key(&root));
    let children = graph.children(root).to_vec();
    assert_ne!(ids[&children[0]], ids[&children[1]]);
}

#[test]
fn a_relocation_joins_the_coordinates_it_moved_between() {
    // Three shapes of the same idea: whichever node carries the relocation, and
    // whichever order the coordinates appear in, they end up competing.
    for name in ["relocation1.txt", "relocation2.txt"] {
        let Some((graph, ids)) = marked(name) else {
            return;
        };
        let children = graph.children(graph.root()).to_vec();
        assert_eq!(ids[&children[0]], ids[&children[1]], "{name}");
    }
}

#[test]
fn one_relocation_can_merge_two_existing_groups() {
    // `test:c:1` relocates from both `test:a:1` and `test:b:1`, which were
    // already separate groups by the time it is seen.
    let Some((graph, ids)) = marked("relocation3.txt") else {
        return;
    };
    let children = graph.children(graph.root()).to_vec();
    assert_eq!(children.len(), 3);
    assert_eq!(ids[&children[0]], ids[&children[1]]);
    assert_eq!(ids[&children[1]], ids[&children[2]]);
}
