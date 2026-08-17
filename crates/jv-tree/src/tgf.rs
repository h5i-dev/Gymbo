//! The Trivial Graph Format rendering, `-DoutputType=tgf`.
//!
//! A port of `TGFDependencyNodeVisitor` (`maven-dependency-plugin`). TGF is two
//! sections separated by a lone `#`: `id label` per node, then `from to [label]`
//! per edge. Upstream writes the node lines as it descends and collects edges to
//! flush on the root's `endVisit`, so the edge section comes out in postorder —
//! a subtree's edges precede the edge that reaches it.
//!
//! Edges are labelled with the target's scope, which upstream takes straight
//! from the artifact and prints unquoted; a node with no declared scope
//! contributes a two-token edge line.
//!
//! Node ids are the one place parity is impossible: upstream uses
//! `node.hashCode()`, the JVM's identity hash, which differs between runs of the
//! same build. jv numbers nodes in visit order instead, one number per
//! *occurrence*, because upstream's tree holds a separate object per path even
//! where jv's graph shares a subtree.

use std::fmt::Write as _;

use jv_resolver::{Graph, NodeId};

use crate::coordinates::{node_scope, node_string};
use crate::text::Options;

/// Renders `graph` as TGF.
///
/// Only [`Options::verbose`] is read; the indent tokens have no meaning outside
/// the text renderer.
pub fn render(graph: &Graph, options: Options) -> String {
    let mut nodes = String::new();
    let mut edges = String::new();
    let mut path = Vec::new();
    let mut next_id = 0;
    visit(
        graph,
        graph.root(),
        None,
        &mut next_id,
        &mut path,
        options.verbose,
        &mut nodes,
        &mut edges,
    );
    nodes.push_str("#\n");
    nodes.push_str(&edges);
    nodes
}

#[expect(
    clippy::too_many_arguments,
    reason = "one walk feeding two output sections; splitting the state into a \
              struct would hide that the sections are written at different times"
)]
fn visit(
    graph: &Graph,
    id: NodeId,
    parent: Option<usize>,
    next_id: &mut usize,
    path: &mut Vec<NodeId>,
    verbose: bool,
    nodes: &mut String,
    edges: &mut String,
) {
    let element_id = *next_id;
    *next_id += 1;
    let _ = writeln!(
        nodes,
        "{element_id} {}",
        node_string(graph.node(id), verbose)
    );

    // A node already on this path would recurse forever; it still gets its own
    // line and its edge back to the parent.
    if !path.contains(&id) {
        path.push(id);
        for child in graph.children(id).to_vec() {
            visit(
                graph,
                child,
                Some(element_id),
                next_id,
                path,
                verbose,
                nodes,
                edges,
            );
        }
        path.pop();
    }

    if let Some(parent) = parent {
        let _ = write!(edges, "{parent} {element_id}");
        if let Some(scope) = node_scope(graph.node(id)) {
            let _ = write!(edges, " {}", scope.as_str());
        }
        edges.push('\n');
    }
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
        let c = graph.add(dependency_node("g", "c", "3.0", Some(Scope::Runtime)));
        graph.add_child(root, a);
        graph.add_child(root, b);
        graph.add_child(a, c);

        assert_eq!(
            render(&graph, Options::default()),
            "\
0 com.example:demo:jar:1.0
1 g:a:jar:1.0:compile
2 g:c:jar:3.0:runtime
3 g:b:jar:2.0:test
#
1 2 runtime
0 1 compile
0 3 test
"
        );
    }

    #[test]
    fn a_lone_root_still_gets_the_separator() {
        // The `#` is written unconditionally on the root's endVisit, so even a
        // graph with no edges has both sections.
        let graph = Graph::new(project_root("com.example", "demo", "1.0"));
        assert_eq!(
            render(&graph, Options::default()),
            "0 com.example:demo:jar:1.0\n#\n"
        );
    }

    #[test]
    fn an_undeclared_scope_leaves_the_edge_with_two_tokens() {
        let mut graph = Graph::new(project_root("com.example", "demo", "1.0"));
        let root = graph.root();
        let a = graph.add(dependency_node("g", "a", "1.0", None));
        graph.add_child(root, a);
        assert_eq!(
            render(&graph, Options::default()),
            "0 com.example:demo:jar:1.0\n1 g:a:jar:1.0\n#\n0 1\n"
        );
    }

    #[test]
    fn verbose_annotations_reach_the_node_lines() {
        let mut graph = Graph::new(project_root("com.example", "demo", "1.0"));
        let root = graph.root();
        let mut node = dependency_node("g", "a", "1.0", Some(Scope::Compile));
        node.omitted_for = Some("2.0".to_owned());
        let a = graph.add(node);
        graph.add_child(root, a);

        let rendered = render(
            &graph,
            Options {
                verbose: true,
                ..Options::default()
            },
        );
        assert!(
            rendered.contains("1 (g:a:jar:1.0:compile - omitted for conflict with 2.0)\n"),
            "{rendered}"
        );
    }

    #[test]
    fn a_cycle_terminates() {
        let mut graph = Graph::new(project_root("com.example", "demo", "1.0"));
        let root = graph.root();
        let a = graph.add(dependency_node("g", "a", "1.0", Some(Scope::Compile)));
        let b = graph.add(dependency_node("g", "b", "1.0", Some(Scope::Compile)));
        graph.add_child(root, a);
        graph.add_child(a, b);
        graph.add_child(b, a);

        let rendered = render(&graph, Options::default());
        let (nodes, edges) = rendered.split_once("#\n").expect("both sections");
        assert_eq!(nodes.lines().count(), 4);
        assert_eq!(edges.lines().count(), 3);
    }
}
