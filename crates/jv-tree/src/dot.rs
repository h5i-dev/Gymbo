//! The Graphviz rendering, `-DoutputType=dot`.
//!
//! A port of `DOTDependencyNodeVisitor` (`maven-dependency-plugin`). The visitor
//! writes the header on the root, then one `parent -> child` line per edge in
//! preorder, then the footer; nodes are named by their label rather than by an
//! id, so the whole graph is spelled out in coordinates.
//!
//! The trailing spaces are Maven's, not a typo: the header ends `{ `, every edge
//! ends ` ; `, and the footer is ` } `. Parity means keeping them, so they are
//! written as `\x20` where an editor or a lint could otherwise trim them away.
//!
//! Upstream escapes nothing — the label goes between quotes verbatim — so a
//! quote in a coordinate would produce broken dot. That is not reachable in
//! practice, because Maven coordinates admit no quote character, and inventing
//! an escape here would put jv's output out of step with Maven's on every other
//! input.

use std::fmt::Write as _;

use jv_resolver::{Graph, NodeId};

use crate::coordinates::node_string;
use crate::text::Options;

/// Renders `graph` as dot.
///
/// Only [`Options::verbose`] is read; the indent tokens have no meaning outside
/// the text renderer.
pub fn render(graph: &Graph, options: Options) -> String {
    let mut out = String::new();
    let root = graph.root();
    let _ = writeln!(
        out,
        "digraph \"{}\" {{\x20",
        node_string(graph.node(root), options.verbose)
    );
    let mut path = Vec::new();
    write_edges(graph, root, &mut path, options.verbose, &mut out);
    // No newline after the footer. Upstream's `endVisit` appends a line
    // separator, but the plugin writes the visitor's output verbatim and every
    // released version through 3.8.1 produces a file whose last byte is the
    // space in `" } "` — verified by running 3.6.1, 3.7.0 and 3.8.1. Adding one
    // is a one-byte difference, and byte parity is the whole claim.
    out.push_str(" }\x20");
    out
}

/// Writes the edges leaving `id`, then recurses.
fn write_edges(graph: &Graph, id: NodeId, path: &mut Vec<NodeId>, verbose: bool, out: &mut String) {
    let from = node_string(graph.node(id), verbose);
    for child in graph.children(id) {
        let _ = writeln!(
            out,
            "\t\"{from}\" -> \"{}\" ;\x20",
            node_string(graph.node(*child), verbose)
        );
    }

    // A node already on this path would recurse forever. Its edges are still
    // written above, matching upstream's `visit` running before the descent.
    if path.contains(&id) {
        return;
    }
    path.push(id);
    for child in graph.children(id).to_vec() {
        write_edges(graph, child, path, verbose, out);
    }
    path.pop();
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
        let c = graph.add(dependency_node("g", "c", "3.0", Some(Scope::Compile)));
        graph.add_child(root, a);
        graph.add_child(root, b);
        graph.add_child(a, c);

        // Every line here ends in a space that upstream emits; they are spelled
        // as `\x20` so a stray editor trim cannot silently break parity.
        assert_eq!(
            render(&graph, Options::default()),
            concat!(
                "digraph \"com.example:demo:jar:1.0\" {\x20\n",
                "\t\"com.example:demo:jar:1.0\" -> \"g:a:jar:1.0:compile\" ;\x20\n",
                "\t\"com.example:demo:jar:1.0\" -> \"g:b:jar:2.0:test\" ;\x20\n",
                "\t\"g:a:jar:1.0:compile\" -> \"g:c:jar:3.0:compile\" ;\x20\n",
                // No trailing newline: verified against plugin 3.6.1, 3.7.0 and
                // 3.8.1, whose output's last byte is this space.
                " }\x20",
            )
        );
    }

    #[test]
    fn a_lone_root_still_gets_a_header_and_footer() {
        let graph = Graph::new(project_root("com.example", "demo", "1.0"));
        assert_eq!(
            render(&graph, Options::default()),
            "digraph \"com.example:demo:jar:1.0\" {\x20\n }\x20"
        );
    }

    #[test]
    fn edges_carry_no_scope_label() {
        // Unlike graphml and tgf, dot puts nothing on the edge: the scope is
        // already part of both endpoints' labels.
        let mut graph = Graph::new(project_root("com.example", "demo", "1.0"));
        let root = graph.root();
        let a = graph.add(dependency_node("g", "a", "1.0", Some(Scope::Runtime)));
        graph.add_child(root, a);
        let rendered = render(&graph, Options::default());
        assert!(
            rendered.contains("-> \"g:a:jar:1.0:runtime\" ;\x20"),
            "{rendered}"
        );
        assert!(!rendered.contains("label"), "{rendered}");
    }

    #[test]
    fn verbose_annotations_reach_the_labels() {
        let mut graph = Graph::new(project_root("com.example", "demo", "1.0"));
        let root = graph.root();
        let mut node = dependency_node("g", "a", "2.0", Some(Scope::Compile));
        node.premanaged.version = Some("1.0".to_owned());
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
            rendered.contains("\"g:a:jar:2.0:compile (version managed from 1.0)\""),
            "{rendered}"
        );
    }

    #[test]
    fn a_cycle_terminates_after_writing_its_closing_edge() {
        let mut graph = Graph::new(project_root("com.example", "demo", "1.0"));
        let root = graph.root();
        let a = graph.add(dependency_node("g", "a", "1.0", Some(Scope::Compile)));
        let b = graph.add(dependency_node("g", "b", "1.0", Some(Scope::Compile)));
        graph.add_child(root, a);
        graph.add_child(a, b);
        graph.add_child(b, a);

        let rendered = render(&graph, Options::default());
        // root->a, a->b, b->a, and then the revisited `a` re-states a->b before
        // the guard stops it.
        assert_eq!(
            rendered.lines().filter(|line| line.contains("->")).count(),
            4
        );
    }
}
