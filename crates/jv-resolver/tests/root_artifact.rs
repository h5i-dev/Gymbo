//! Collection when the root is an *artifact* rather than a dependency.
//!
//! This is the shape `CollectRequest::root_artifact` produces — a project
//! resolving its own dependencies, which is what `jv tree` does on every
//! invocation and therefore the single most-used entry point in the tool. It is
//! also the shape none of upstream's collector corpora exercise, because every
//! golden there is rooted at a dependency.
//!
//! What makes it different is that the root node has coordinates but *no*
//! dependency. Two places in the resolver have to test for that specifically, and
//! neither failure is visible without a graph where the project depends,
//! transitively, on itself — which is unusual but entirely legal, and is what a
//! module depending on a sibling that depends back produces.

use std::collections::HashMap;

use jv_model::{Artifact, Dependency, Scope, TypeRegistry};
use jv_resolver::{
    CollectRequest, Descriptor, DescriptorSource, Graph, NodeId, Verbosity, collect,
    resolve_conflicts,
};

/// Descriptors from a fixed table.
struct Table {
    entries: HashMap<String, Vec<Dependency>>,
}

impl Table {
    fn new(entries: &[(&str, &[&str])]) -> Self {
        Self {
            entries: entries
                .iter()
                .map(|(coordinates, dependencies)| {
                    (
                        (*coordinates).to_owned(),
                        dependencies.iter().map(|spec| dependency(spec)).collect(),
                    )
                })
                .collect(),
        }
    }
}

impl DescriptorSource for Table {
    fn descriptor(&self, artifact: &Artifact) -> Result<Descriptor, String> {
        let key = format!(
            "{}:{}:{}",
            artifact.group_id, artifact.artifact_id, artifact.version
        );
        Ok(Descriptor {
            artifact: artifact.clone(),
            dependencies: self.entries.get(&key).cloned().unwrap_or_default(),
            managed_dependencies: Vec::new(),
            relocations: Vec::new(),
        })
    }
}

/// `g:a:v` as a compile-scoped dependency.
fn dependency(spec: &str) -> Dependency {
    let mut fields = spec.split(':');
    Dependency {
        group_id: fields.next().unwrap_or_default().to_owned(),
        artifact_id: fields.next().unwrap_or_default().to_owned(),
        version: fields.next().map(str::to_owned),
        scope: Some(Scope::Compile),
        ..Dependency::default()
    }
}

/// Every surviving node's coordinates, in walk order, excluding the root.
fn surviving(graph: &Graph) -> Vec<String> {
    let mut found = Vec::new();
    graph.walk(|id: NodeId, _depth| {
        if id == graph.root() {
            return;
        }
        let node = graph.node(id);
        if node.omitted_for.is_some() {
            return;
        }
        if let Some(artifact) = &node.artifact {
            found.push(format!(
                "{}:{}:{}",
                artifact.group_id, artifact.artifact_id, artifact.version
            ));
        }
    });
    found
}

fn resolve(source: &Table, request: &CollectRequest) -> Vec<String> {
    let types = TypeRegistry::default();
    let mut collected = collect(source, request, &types).expect("collection");
    resolve_conflicts(&mut collected.graph, Verbosity::None).expect("resolution");
    surviving(&collected.graph)
}

#[test]
fn a_transitive_dependency_on_the_project_itself_is_not_pruned_by_the_root() {
    // `app` depends on `lib`, which depends back on an older `app`, which brings
    // `extra`.
    //
    // The root has `app`'s coordinates but no dependency, so it must not be given
    // a conflict id. Giving it one made it a competitor in `app`'s own conflict
    // group at depth 0, where it wins by construction — and winning removed
    // `app:0.9` along with `extra` beneath it.
    let source = Table::new(&[
        ("g:lib:1", &["g:app:0.9"][..]),
        ("g:app:0.9", &["g:extra:1"][..]),
    ]);
    let request = CollectRequest {
        root_artifact: Some(Artifact::new("g", "app", "1")),
        dependencies: vec![dependency("g:lib:1")],
        ..CollectRequest::default()
    };

    let surviving = resolve(&source, &request);
    assert!(
        surviving.contains(&"g:extra:1".to_owned()),
        "the project's own coordinates pruned a real subtree: {surviving:?}"
    );
}

#[test]
fn the_project_reappearing_transitively_is_expanded_rather_than_closed_as_a_cycle() {
    // Same shape, looked at from the collector's side. `find_cycle` walks the
    // ancestor path and finds the root, whose coordinates match — but a cycle
    // *node* borrows the ancestor's child list instead of being expanded, and the
    // root's child list is the project's direct dependencies, not `app:0.9`'s.
    //
    // Upstream tests `cycleNode.getDependency() != null` before creating the node
    // and otherwise falls through to ordinary expansion, which is what puts
    // `deeper` in the graph at all.
    let source = Table::new(&[
        ("g:lib:1", &["g:app:0.9"][..]),
        ("g:app:0.9", &["g:deeper:1"][..]),
        ("g:deeper:1", &[][..]),
    ]);
    let request = CollectRequest {
        root_artifact: Some(Artifact::new("g", "app", "1")),
        dependencies: vec![dependency("g:lib:1")],
        ..CollectRequest::default()
    };

    let types = TypeRegistry::default();
    let collected = collect(&source, &request, &types).expect("collection");
    let all = surviving(&collected.graph);
    assert!(
        all.contains(&"g:deeper:1".to_owned()),
        "the re-occurrence borrowed the root's children instead of its own: {all:?}"
    );
}

#[test]
fn an_ordinary_root_artifact_project_still_resolves_normally() {
    // The guard above must not cost anything in the common case, which is every
    // project that does not depend on itself.
    let source = Table::new(&[("g:a:1", &["g:b:1"][..]), ("g:b:1", &["g:c:1"][..])]);
    let request = CollectRequest {
        root_artifact: Some(Artifact::new("g", "app", "1")),
        dependencies: vec![dependency("g:a:1")],
        ..CollectRequest::default()
    };
    assert_eq!(
        resolve(&source, &request),
        ["g:a:1", "g:b:1", "g:c:1"],
        "a plain transitive chain changed shape"
    );
}
