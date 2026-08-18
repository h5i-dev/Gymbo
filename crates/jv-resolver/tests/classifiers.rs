//! Two dependencies that differ only by classifier are two dependencies.
//!
//! `org.xmlresolver:xmlresolver` ships its data files as a `data`-classified
//! jar and depends on it from its own POM, so checkstyle 12 pulls both
//! `xmlresolver:jar` and `xmlresolver:jar:data`. Maven resolves both. jv
//! resolved one and dropped the other as a duplicate, which is a `jv tree`
//! divergence as well as a `jv sync` gap — the missing jar is what made
//! `mvn -o` fail on spring-petclinic.
//!
//! The shape is not exotic: `tests`, `sources`, `linux-x86_64`, `data`, and
//! every native-library classifier produce it. Ring 3's five projects simply
//! never happened to contain one.

use std::collections::HashMap;

use jv_model::{Artifact, Dependency, Scope, TypeRegistry};
use jv_resolver::{
    CollectRequest, Descriptor, DescriptorSource, Graph, NodeId, Verbosity, collect,
    resolve_conflicts,
};

/// Descriptors from a fixed table, keyed the way a repository is: the POM is
/// shared between an artifact and its classified siblings, because a classifier
/// selects a *file*, not a different module.
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
        // Keyed without the classifier on purpose: one POM serves every
        // classified file of a version, which is exactly the situation that
        // made the two look interchangeable.
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

/// `g:a:v` or `g:a:v:classifier`, compile-scoped.
fn dependency(spec: &str) -> Dependency {
    let mut fields = spec.split(':');
    let group_id = fields.next().unwrap_or_default().to_owned();
    let artifact_id = fields.next().unwrap_or_default().to_owned();
    let version = fields.next().map(str::to_owned);
    let classifier = fields.next().map(str::to_owned);
    Dependency {
        group_id,
        artifact_id,
        version,
        classifier,
        scope: Some(Scope::Compile),
        ..Dependency::default()
    }
}

/// Surviving nodes as `group:artifact:extension[:classifier]:version`.
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
            let mut rendered = format!(
                "{}:{}:{}",
                artifact.group_id, artifact.artifact_id, artifact.extension
            );
            if !artifact.classifier.is_empty() {
                rendered.push(':');
                rendered.push_str(&artifact.classifier);
            }
            rendered.push(':');
            rendered.push_str(&artifact.version);
            found.push(rendered);
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
fn a_classified_sibling_is_not_a_duplicate() {
    let source = Table::new(&[]);
    let request = CollectRequest {
        root_artifact: Some(Artifact::new("com.example", "root", "1")),
        dependencies: vec![dependency("g:a:1"), dependency("g:a:1:data")],
        ..CollectRequest::default()
    };

    let surviving = resolve(&source, &request);
    assert_eq!(
        surviving,
        ["g:a:jar:1", "g:a:jar:data:1"],
        "a classifier selects a different file, so both must survive"
    );
}

#[test]
fn the_order_of_the_two_does_not_matter() {
    // The classified one first: whichever is seen first must not absorb the
    // other, so this is not a one-way fix.
    let source = Table::new(&[]);
    let request = CollectRequest {
        root_artifact: Some(Artifact::new("com.example", "root", "1")),
        dependencies: vec![dependency("g:a:1:data"), dependency("g:a:1")],
        ..CollectRequest::default()
    };
    assert_eq!(resolve(&source, &request), ["g:a:jar:data:1", "g:a:jar:1"]);
}

#[test]
fn a_classified_sibling_reached_transitively_also_survives() {
    // The real shape: checkstyle depends on xmlresolver, whose own POM adds the
    // data-classified file beside it.
    let source = Table::new(&[("g:lib:1", &["g:a:1", "g:a:1:data"][..])]);
    let request = CollectRequest {
        root_artifact: Some(Artifact::new("com.example", "root", "1")),
        dependencies: vec![dependency("g:lib:1")],
        ..CollectRequest::default()
    };

    let surviving = resolve(&source, &request);
    assert!(
        surviving.contains(&"g:a:jar:data:1".to_owned()),
        "the classified file was dropped: {surviving:?}"
    );
    assert!(
        surviving.contains(&"g:a:jar:1".to_owned()),
        "the plain file was dropped: {surviving:?}"
    );
}

#[test]
fn different_classifiers_of_the_same_artifact_all_survive() {
    // Native libraries do this with one classifier per platform.
    let source = Table::new(&[]);
    let request = CollectRequest {
        root_artifact: Some(Artifact::new("com.example", "root", "1")),
        dependencies: vec![
            dependency("g:native:1:linux-x86_64"),
            dependency("g:native:1:osx-aarch64"),
            dependency("g:native:1:windows-x86_64"),
        ],
        ..CollectRequest::default()
    };
    assert_eq!(resolve(&source, &request).len(), 3);
}
