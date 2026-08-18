//! Checks collection against Maven Resolver's `.ini` corpus.
//!
//! The corpus replaces repositories with files: an artifact's dependencies come
//! from `gid_aid_version.ini` rather than from a POM, so the whole collector can
//! be exercised with no network and no local repository. The expected results
//! are graphs in the same DSL the transformer corpus uses.
//!
//! Comparison is structural, as upstream's is: dependencies must match, child
//! counts must match, and recursion stops where a node repeats an ancestor —
//! otherwise a cyclic result could not be compared at all.
//!
//! Skips itself when `_reference/` is absent; `JV_REQUIRE_ORACLE=1` makes that a
//! failure, as CI does.

use std::path::{Path, PathBuf};

use jv_model::{Artifact, Dependency, Scope, TypeRegistry};
use jv_resolver::{CollectRequest, Descriptor, DescriptorSource, Graph, NodeId, collect};
use jv_testkit::{graph_dsl, ini_descriptors};

fn corpus(subdirectory: &str) -> Option<PathBuf> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../_reference/maven-resolver")
        .join("maven-resolver-impl/src/test/resources/artifact-descriptions")
        .join(subdirectory);
    path.is_dir().then_some(path)
}

fn skip_or_corpus(subdirectory: &str) -> Option<PathBuf> {
    let found = corpus(subdirectory);
    if found.is_none() {
        if std::env::var_os("JV_REQUIRE_ORACLE").is_some() {
            panic!("JV_REQUIRE_ORACLE is set but the collection corpus is missing");
        }
        eprintln!("skipping: _reference/ not present (see docs/development.md)");
    }
    found
}

/// Serves descriptors out of the corpus directory.
struct IniSource {
    reader: ini_descriptors::DescriptorReader,
}

impl DescriptorSource for IniSource {
    fn descriptor(&self, artifact: &Artifact) -> Result<Descriptor, String> {
        let resolved = self
            .reader
            .resolve(artifact)
            .map_err(|error| error.to_string())?;
        Ok(Descriptor {
            // The coordinates the artifact really lives under, which differ from
            // the ones asked for exactly when a relocation applied.
            artifact: resolved.artifact,
            dependencies: resolved.description.dependencies,
            managed_dependencies: resolved.description.managed_dependencies,
            relocations: resolved.relocations,
        })
    }

    fn versions(&self, group_id: &str, artifact_id: &str) -> Result<Vec<String>, String> {
        Ok(self.reader.versions(group_id, artifact_id))
    }
}

/// The root dependency a golden graph describes.
fn root_dependency(graph: &Graph) -> Dependency {
    let root = graph.root();
    graph
        .node(root)
        .dependency
        .clone()
        .expect("the golden's root is a dependency")
}

fn load_golden(directory: &str, name: &str) -> Option<Graph> {
    let corpus = skip_or_corpus(directory)?;
    let text = std::fs::read_to_string(corpus.join(name))
        .unwrap_or_else(|error| panic!("cannot read {name}: {error}"));
    Some(graph_dsl::parse(&text).unwrap_or_else(|error| panic!("cannot parse {name}: {error}")))
}

fn collected(directory: &str, root: Dependency) -> Option<Graph> {
    let corpus = skip_or_corpus(directory)?;
    let source = IniSource {
        reader: ini_descriptors::DescriptorReader::new(corpus),
    };
    let request = CollectRequest {
        root_dependency: Some(root),
        ..Default::default()
    };
    let result = collect(&source, &request, &TypeRegistry::new())
        .unwrap_or_else(|error| panic!("collection failed: {error}"));
    Some(result.graph)
}

/// Compares two graphs the way upstream's `assertEqualSubtree` does.
fn assert_same_subtree(expected: &Graph, actual: &Graph) {
    let mut ancestors = Vec::new();
    compare(
        expected,
        expected.root(),
        actual,
        actual.root(),
        &mut ancestors,
    );
}

fn describe(graph: &Graph, id: NodeId) -> String {
    let node = graph.node(id);
    match (&node.artifact, &node.dependency) {
        (Some(artifact), Some(dependency)) => format!(
            "{}:{}:{}:{} {}{}",
            artifact.group_id,
            artifact.artifact_id,
            artifact.extension,
            artifact.version,
            dependency
                .scope
                .map_or_else(|| "-".to_owned(), |scope| scope.to_string()),
            if dependency.is_optional() {
                " optional"
            } else {
                ""
            }
        ),
        (Some(artifact), None) => format!("{artifact} (no dependency)"),
        _ => "(null)".to_owned(),
    }
}

fn compare(
    expected: &Graph,
    expected_id: NodeId,
    actual: &Graph,
    actual_id: NodeId,
    ancestors: &mut Vec<String>,
) {
    let expected_text = describe(expected, expected_id);
    let actual_text = describe(actual, actual_id);
    assert_eq!(
        expected_text,
        actual_text,
        "node mismatch under {ancestors:?}\nexpected graph:\n{}\nactual graph:\n{}",
        graph_dsl::dump(expected),
        graph_dsl::dump(actual)
    );

    // A node that repeats an ancestor closes a cycle; descending would not
    // terminate, and upstream stops here too.
    if ancestors.contains(&expected_text) {
        return;
    }
    ancestors.push(expected_text.clone());

    let expected_children = expected.children(expected_id).to_vec();
    let actual_children = actual.children(actual_id).to_vec();
    assert_eq!(
        expected_children.len(),
        actual_children.len(),
        "child count under {expected_text}\nexpected graph:\n{}\nactual graph:\n{}",
        graph_dsl::dump(expected),
        graph_dsl::dump(actual)
    );
    for (left, right) in expected_children.iter().zip(&actual_children) {
        compare(expected, *left, actual, *right, ancestors);
    }
    ancestors.pop();
}

#[test]
fn a_single_dependency_is_collected() {
    let Some(graph) = collected(
        "",
        Dependency {
            group_id: "gid".to_owned(),
            artifact_id: "aid".to_owned(),
            version: Some("ver".to_owned()),
            type_: Some("ext".to_owned()),
            scope: Some(Scope::Compile),
            ..Default::default()
        },
    ) else {
        return;
    };
    let root = graph.root();
    assert_eq!(graph.children(root).len(), 1);
    let child = graph.children(root)[0];
    let artifact = graph.node(child).artifact.as_ref().expect("an artifact");
    assert_eq!(artifact.artifact_id, "aid2");
    assert_eq!(graph.node(child).scope(), Scope::Compile);
}

#[test]
fn a_whole_subtree_matches_its_golden() {
    let Some(expected) = load_golden("", "expectedSubtreeComparisonResult.txt") else {
        return;
    };
    let Some(actual) = collected("", root_dependency(&expected)) else {
        return;
    };
    assert_same_subtree(&expected, &actual);
}

/// A transitive version range, expanded newest-first.
#[test]
fn a_transitive_range_expands_to_every_matching_version() {
    let Some(expected) = load_golden("", "transitiveDepsUseRangesDirtyTreeResult_BF.txt") else {
        return;
    };
    let Some(actual) = collected("", root_dependency(&expected)) else {
        return;
    };
    assert_same_subtree(&expected, &actual);
}

/// A range whose versions relocate — three in, one out.
///
/// This golden went unexercised for a long time because the corpus reader
/// followed relocations silently and reported the original coordinates, so the
/// collector never saw a relocation at all. It pins two things at once: that a
/// relocated dependency is rewritten onto its new coordinates, and that
/// upstream *returns* after handling a relocation rather than continuing the
/// version loop, so a relocated range collapses to a single node.
#[test]
fn a_relocated_range_collapses_to_one_node() {
    let Some(expected) = load_golden(
        "",
        "transitiveDepsUseRangesAndRelocationDirtyTreeResult_BF.txt",
    ) else {
        return;
    };
    let Some(actual) = collected("", root_dependency(&expected)) else {
        return;
    };
    assert_same_subtree(&expected, &actual);
}

/// The case that pinned down why upstream's `DataPool` is not an optimization.
///
/// This golden was unreconciled for a while, and the reasoning that failed is
/// worth recording: jv's `isLeftmost` is a faithful transcription, and the
/// arithmetic said the node *should* be skipped. What was wrong was the premise —
/// that upstream's skipper sees the same nodes jv's does. It does not.
/// `BfDependencyCollector.doRecurse` consults the pool *before*
/// `skipResolution`, so a pool hit never reaches the skipper and never takes a
/// sequence number. `b` under `a` is a pool hit, so it never becomes a duplicate
/// candidate, and every number after it differs.
#[test]
fn a_cyclic_graph_matches_its_golden() {
    let Some(expected) = load_golden("", "cycle.txt") else {
        return;
    };
    let Some(actual) = collected("", root_dependency(&expected)) else {
        return;
    };
    assert_same_subtree(&expected, &actual);
}

#[test]
fn a_large_cyclic_graph_terminates() {
    // 556 descriptors that reference each other heavily. The assertion is only
    // that collection finishes: upstream's test makes no claim beyond that, and
    // an implementation without a cycle guard would not get here.
    let Some(corpus) = skip_or_corpus("cycle-big") else {
        return;
    };
    let source = IniSource {
        reader: ini_descriptors::DescriptorReader::new(corpus),
    };
    let request = CollectRequest {
        root_dependency: Some(Dependency {
            group_id: "1".to_owned(),
            artifact_id: "2".to_owned(),
            version: Some("5.50-SNAPSHOT".to_owned()),
            type_: Some("pom".to_owned()),
            ..Default::default()
        }),
        ..Default::default()
    };
    let result = collect(&source, &request, &TypeRegistry::new()).expect("collection finishes");
    assert!(result.graph.len() > 1, "the graph should not be empty");
}
