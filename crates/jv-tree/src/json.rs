//! The JSON rendering, `-DoutputType=json`.
//!
//! A port of `JsonDependencyNodeVisitor` (`maven-dependency-plugin`). Unlike the
//! other output types this one does not print node labels at all: it emits the
//! artifact's fields as seven string-valued keys in a fixed order — `groupId`,
//! `artifactId`, `version`, `type`, `scope`, `classifier`, `optional` — with a
//! `children` array appended when the node has any. Every value is a JSON
//! string, `optional` included, so it reads as `"optional": "false"`.
//!
//! Because nothing is labelled, `-Dverbose` has no effect here and this renderer
//! takes no options: upstream's visitor has no state beyond its writer.
//!
//! Three upstream details are deliberate ports rather than oversights.
//!
//! The `version` is the artifact's *resolved* version, not the base version the
//! node labels print, so a resolved snapshot appears here with its timestamp.
//!
//! The `children` array closes at the indent of its elements rather than of its
//! key, so the closing `]` sits two columns right of where a formatter would put
//! it.
//!
//! Nothing is escaped. Upstream concatenates values into the document as-is,
//! which is safe for Maven coordinates and which jv keeps, because escaping here
//! would diverge from Maven on every input where it fired.

use std::fmt::Write as _;

use jv_resolver::{Graph, Node, NodeId};

use crate::coordinates::{node_scope, node_type};

/// Renders `graph` as JSON.
///
/// The result ends with a newline, matching upstream's final `System
/// .lineSeparator()`.
pub fn render(graph: &Graph) -> String {
    let mut out = String::from("{\n");
    let mut path = Vec::new();
    write_node(graph, graph.root(), 2, &mut path, &mut out);
    out.push_str("}\n");
    out
}

/// Writes one node's fields into an object whose braces the caller wrote.
fn write_node(graph: &Graph, id: NodeId, indent: usize, path: &mut Vec<NodeId>, out: &mut String) {
    // A node already on this path would recurse forever. Upstream bails the same
    // way and leaves the enclosing braces empty, so a cycle shows up as `{}`.
    if path.contains(&id) {
        return;
    }
    path.push(id);
    let has_children = !graph.children(id).is_empty();
    append_node_values(out, indent, graph.node(id), has_children);
    if has_children {
        write_children(graph, id, indent, path, out);
    }
    path.pop();
}

fn write_children(
    graph: &Graph,
    id: NodeId,
    indent: usize,
    path: &mut Vec<NodeId>,
    out: &mut String,
) {
    let _ = writeln!(out, "{}\"children\": [", spaces(indent));
    // Elements sit one level in; upstream then closes the array at this same
    // width instead of the key's.
    let indent = indent + 2;
    let children = graph.children(id).to_vec();
    for (index, child) in children.iter().enumerate() {
        let _ = writeln!(out, "{}{{", spaces(indent));
        write_node(graph, *child, indent + 2, path, out);
        let _ = write!(out, "{}}}", spaces(indent));
        if index + 1 != children.len() {
            out.push(',');
        }
        out.push('\n');
    }
    let _ = writeln!(out, "{}]", spaces(indent));
}

/// The seven artifact keys, in upstream's order.
///
/// `has_children` decides only whether the last of them carries a trailing
/// comma, because a `children` array follows it.
fn append_node_values(out: &mut String, indent: usize, node: &Node, has_children: bool) {
    let artifact = node.artifact.as_ref();
    let group_id = artifact.map_or("", |artifact| artifact.group_id.as_str());
    let artifact_id = artifact.map_or("", |artifact| artifact.artifact_id.as_str());
    // The resolved version, which for a snapshot is the timestamped one.
    let version = artifact.map_or("", |artifact| artifact.version.as_str());
    let type_ = artifact.map_or("", |artifact| node_type(node, artifact));
    let scope = node_scope(node).map_or("", |scope| scope.as_str());
    let classifier = artifact.map_or("", |artifact| artifact.classifier.as_str());
    let optional = if node.is_optional() { "true" } else { "false" };

    append_key_value(out, indent, "groupId", group_id, true);
    append_key_value(out, indent, "artifactId", artifact_id, true);
    append_key_value(out, indent, "version", version, true);
    append_key_value(out, indent, "type", type_, true);
    append_key_value(out, indent, "scope", scope, true);
    append_key_value(out, indent, "classifier", classifier, true);
    append_key_value(out, indent, "optional", optional, has_children);
}

fn append_key_value(out: &mut String, indent: usize, key: &str, value: &str, comma: bool) {
    let _ = write!(out, "{}\"{key}\": \"{value}\"", spaces(indent));
    if comma {
        out.push(',');
    }
    out.push('\n');
}

fn spaces(width: usize) -> String {
    " ".repeat(width)
}

#[cfg(test)]
mod tests {
    use super::*;
    use jv_model::{Artifact, Dependency, Scope};
    use jv_resolver::Node as GraphNode;

    fn dependency_node(
        group: &str,
        artifact: &str,
        version: &str,
        scope: Option<Scope>,
    ) -> GraphNode {
        let mut dependency = Dependency::new(group, artifact, version);
        dependency.scope = scope;
        GraphNode::dependency(dependency, Artifact::new(group, artifact, version))
    }

    fn project_root(group: &str, artifact: &str, version: &str) -> GraphNode {
        GraphNode {
            artifact: Some(Artifact::new(group, artifact, version)),
            ..GraphNode::default()
        }
    }

    #[test]
    fn a_small_graph_renders_exactly_as_maven_writes_it() {
        let mut graph = Graph::new(project_root("com.example", "demo", "1.0"));
        let root = graph.root();
        let a = graph.add(dependency_node("g", "a", "1.0", Some(Scope::Compile)));
        let b = graph.add(dependency_node("g", "b", "2.0", Some(Scope::Test)));
        graph.add_child(root, a);
        graph.add_child(root, b);

        assert_eq!(
            render(&graph),
            r#"{
  "groupId": "com.example",
  "artifactId": "demo",
  "version": "1.0",
  "type": "jar",
  "scope": "",
  "classifier": "",
  "optional": "false",
  "children": [
    {
      "groupId": "g",
      "artifactId": "a",
      "version": "1.0",
      "type": "jar",
      "scope": "compile",
      "classifier": "",
      "optional": "false"
    },
    {
      "groupId": "g",
      "artifactId": "b",
      "version": "2.0",
      "type": "jar",
      "scope": "test",
      "classifier": "",
      "optional": "false"
    }
    ]
}
"#
        );
    }

    #[test]
    fn a_leaf_root_has_no_children_key() {
        assert_eq!(
            render(&Graph::new(project_root("com.example", "demo", "1.0"))),
            "\
{
  \"groupId\": \"com.example\",
  \"artifactId\": \"demo\",
  \"version\": \"1.0\",
  \"type\": \"jar\",
  \"scope\": \"\",
  \"classifier\": \"\",
  \"optional\": \"false\"
}
"
        );
    }

    #[test]
    fn the_version_is_the_resolved_one_not_the_base_version() {
        // The node labels every other output type prints would say
        // `1.0-SNAPSHOT` here; this renderer reads the artifact directly.
        let mut graph = Graph::new(project_root("com.example", "demo", "1.0"));
        let root = graph.root();
        let mut dependency = Dependency::new("g", "a", "1.0-SNAPSHOT");
        dependency.scope = Some(Scope::Compile);
        let a = graph.add(GraphNode::dependency(
            dependency,
            Artifact::new("g", "a", "1.0-20240115.103000-7"),
        ));
        graph.add_child(root, a);

        let rendered = render(&graph);
        assert!(
            rendered.contains("\"version\": \"1.0-20240115.103000-7\""),
            "{rendered}"
        );
    }

    #[test]
    fn the_declared_type_and_the_optional_flag_are_reported() {
        let mut graph = Graph::new(project_root("com.example", "demo", "1.0"));
        let root = graph.root();
        let mut dependency = Dependency::new("g", "a", "1.0");
        dependency.type_ = Some("test-jar".to_owned());
        dependency.scope = Some(Scope::Test);
        dependency.optional = Some(true);
        let artifact = Artifact::new("g", "a", "1.0")
            .with_classifier("tests")
            .with_extension("jar");
        let a = graph.add(GraphNode::dependency(dependency, artifact));
        graph.add_child(root, a);

        let rendered = render(&graph);
        assert!(rendered.contains("\"type\": \"test-jar\""), "{rendered}");
        assert!(rendered.contains("\"classifier\": \"tests\""), "{rendered}");
        assert!(rendered.contains("\"optional\": \"true\""), "{rendered}");
    }

    #[test]
    fn a_cycle_closes_the_repeated_node_as_an_empty_object() {
        let mut graph = Graph::new(project_root("com.example", "demo", "1.0"));
        let root = graph.root();
        let a = graph.add(dependency_node("g", "a", "1.0", Some(Scope::Compile)));
        let b = graph.add(dependency_node("g", "b", "1.0", Some(Scope::Compile)));
        graph.add_child(root, a);
        graph.add_child(a, b);
        graph.add_child(b, a);

        let rendered = render(&graph);
        // The innermost object is the second occurrence of `a`, which upstream
        // writes braces for and then declines to fill.
        assert!(rendered.contains("{\n            }\n"), "{rendered}");
        assert_eq!(rendered.matches("\"artifactId\": \"a\"").count(), 1);
    }

    #[test]
    fn nested_children_indent_one_level_per_generation() {
        let mut graph = Graph::new(project_root("com.example", "demo", "1.0"));
        let root = graph.root();
        let a = graph.add(dependency_node("g", "a", "1.0", Some(Scope::Compile)));
        let c = graph.add(dependency_node("g", "c", "1.0", Some(Scope::Compile)));
        graph.add_child(root, a);
        graph.add_child(a, c);

        let rendered = render(&graph);
        assert!(rendered.contains("      \"children\": [\n"), "{rendered}");
        // The grandchild's array closes at the element indent, not the key's.
        assert!(rendered.contains("\n        ]\n"), "{rendered}");
    }
}
