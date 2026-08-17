//! Checks scope selection against Maven Resolver's own corpus.
//!
//! The cases live in
//! `maven-resolver-util/src/test/resources/transformer/scope-calculator/`, and
//! several are templates: the same graph is run once per scope, or once per
//! ordered pair of scopes, so a handful of files cover the whole matrix.
//!
//! Scope is where a resolver is most likely to be *nearly* right. A direct
//! dependency's declared scope has to beat every transitive path, a conflict
//! winner's scope has to propagate to children that were reached through the
//! loser, and a cycle must not make either of those diverge.
//!
//! Skips itself when `_reference/` is absent; `JV_REQUIRE_ORACLE=1` makes that a
//! failure, as CI does.

use std::path::{Path, PathBuf};

use jv_model::Scope;
use jv_resolver::{Graph, NodeId, Verbosity, resolve_conflicts};
use jv_testkit::graph_dsl;

/// The four scopes the corpus substitutes, weakest first. Upstream's test relies
/// on this ordering: where two paths disagree, the later one wins.
const SCOPES: [Scope; 4] = [Scope::Test, Scope::Provided, Scope::Runtime, Scope::Compile];

fn corpus() -> Option<PathBuf> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../_reference/maven-resolver")
        .join("maven-resolver-util/src/test/resources/transformer/scope-calculator");
    path.is_dir().then_some(path)
}

/// Loads and resolves one case, or `None` when the corpus is absent.
fn resolved(name: &str, substitutions: &[&str]) -> Option<Graph> {
    let Some(corpus) = corpus() else {
        if std::env::var_os("JV_REQUIRE_ORACLE").is_some() {
            panic!("JV_REQUIRE_ORACLE is set but _reference/maven-resolver is missing");
        }
        eprintln!("skipping: _reference/ not present (see docs/development.md)");
        return None;
    };
    let text = std::fs::read_to_string(corpus.join(name))
        .unwrap_or_else(|error| panic!("cannot read {name}: {error}"));
    let mut graph = graph_dsl::parse_with(&text, substitutions)
        .unwrap_or_else(|error| panic!("cannot parse {name}: {error}"));
    resolve_conflicts(&mut graph, Verbosity::None)
        .unwrap_or_else(|error| panic!("{name} did not resolve: {error}"));
    Some(graph)
}

/// Follows child indices from the root, the way upstream's `path` helper does.
fn at(graph: &Graph, coordinates: &[usize]) -> NodeId {
    let mut node = graph.root();
    for index in coordinates {
        let children = graph.children(node);
        assert!(
            *index < children.len(),
            "no child {index} under {node}; the graph has {} there",
            children.len()
        );
        node = children[*index];
    }
    node
}

fn expect_scope(graph: &Graph, coordinates: &[usize], expected: Scope, context: &str) {
    let node = at(graph, coordinates);
    assert_eq!(
        graph.node(node).scope(),
        expected,
        "{context}: expected {expected} at {coordinates:?}, graph was\n{}",
        graph_dsl::dump(graph)
    );
}

#[test]
fn a_provided_parent_narrows_its_child() {
    let Some(graph) = resolved("inheritance.txt", &["provided", "test"]) else {
        return;
    };
    expect_scope(&graph, &[0, 0], Scope::Test, "inheritance");
}

#[test]
fn the_winning_scope_propagates_to_children() {
    // The child was reached through the loser, and must still take the winner's
    // scope.
    let Some(graph) = resolved("conflict-and-inheritance.txt", &[]) else {
        return;
    };
    expect_scope(&graph, &[0, 0], Scope::Compile, "conflict-and-inheritance");
    expect_scope(
        &graph,
        &[0, 0, 0],
        Scope::Compile,
        "conflict-and-inheritance",
    );
}

#[test]
fn a_direct_dependencys_scope_reaches_the_whole_graph() {
    let Some(graph) = resolved("direct-with-conflict-and-inheritance.txt", &[]) else {
        return;
    };
    expect_scope(&graph, &[0, 0], Scope::Test, "direct-with-conflict");
}

/// A corpus file and the scopes expected at given child coordinates.
type ScopeCase = (&'static str, Vec<(&'static [usize], Scope)>);

#[test]
fn cycles_do_not_disturb_scopes() {
    let cases: [ScopeCase; 4] = [
        (
            "cycle-a.txt",
            vec![(&[0][..], Scope::Compile), (&[1][..], Scope::Runtime)],
        ),
        (
            "cycle-b.txt",
            vec![(&[0][..], Scope::Runtime), (&[1][..], Scope::Compile)],
        ),
        (
            "cycle-c.txt",
            vec![
                (&[0][..], Scope::Runtime),
                (&[0, 0][..], Scope::Runtime),
                (&[1][..], Scope::Runtime),
                (&[1, 0][..], Scope::Runtime),
            ],
        ),
        (
            "cycle-d.txt",
            vec![(&[0][..], Scope::Compile), (&[0, 0][..], Scope::Compile)],
        ),
    ];

    for (name, expectations) in cases {
        let Some(graph) = resolved(name, &[]) else {
            return;
        };
        for (coordinates, expected) in expectations {
            expect_scope(&graph, coordinates, expected, name);
        }
    }
}

#[test]
fn a_direct_node_sets_the_scope_whatever_it_is() {
    for scope in SCOPES {
        let Some(graph) = resolved("direct-nodes-winning.txt", &[scope.as_str()]) else {
            return;
        };
        expect_scope(&graph, &[0], scope, &format!("direct {scope}"));
    }
}

#[test]
fn multiple_inheritance_takes_the_widest_scope() {
    for first in SCOPES {
        for second in SCOPES {
            let Some(graph) = resolved(
                "multiple-inheritance.txt",
                &[first.as_str(), second.as_str()],
            ) else {
                return;
            };
            expect_scope(
                &graph,
                &[0, 0],
                widest(first, second),
                &format!("multiple-inheritance {first}/{second}"),
            );
        }
    }
}

#[test]
fn duelling_scopes_take_the_widest() {
    for first in SCOPES {
        for second in SCOPES {
            let Some(graph) = resolved("dueling-scopes.txt", &[first.as_str(), second.as_str()])
            else {
                return;
            };
            expect_scope(
                &graph,
                &[0, 0],
                widest(first, second),
                &format!("dueling-scopes {first}/{second}"),
            );
        }
    }
}

#[test]
fn a_conflicting_direct_node_wins_regardless_of_width() {
    // Unlike the transitive cases, the *first* direct declaration wins even when
    // the other is wider: a direct scope is authoritative, not a candidate.
    for first in SCOPES {
        for second in SCOPES {
            let Some(graph) = resolved(
                "conflicting-direct-nodes.txt",
                &[first.as_str(), second.as_str()],
            ) else {
                return;
            };
            expect_scope(
                &graph,
                &[0],
                first,
                &format!("conflicting-direct-nodes {first}/{second}"),
            );
        }
    }
}

/// The wider of two scopes, under the corpus's ordering.
fn widest(first: Scope, second: Scope) -> Scope {
    let rank = |scope: Scope| {
        SCOPES
            .iter()
            .position(|candidate| *candidate == scope)
            .expect("a corpus scope")
    };
    if rank(first) >= rank(second) {
        first
    } else {
        second
    }
}
