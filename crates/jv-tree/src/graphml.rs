//! The GraphML rendering, `-DoutputType=graphml`.
//!
//! A port of `GraphmlDependencyNodeVisitor` (`maven-dependency-plugin`). The
//! header declares the yFiles extension keys `d0` (node graphics) and `d1` (edge
//! graphics); node elements are written in preorder as the walk descends, and
//! edge elements on the way back up, so a subtree's edges follow its nodes.
//!
//! Two upstream quirks survive the port. The header's `<?xml ...?>` and
//! `<graphml>` share a line separated by a space, and both `<key>` lines end in
//! a trailing space. The footer has no line separator at all, so the document
//! does not end in a newline.
//!
//! Node ids are the one place parity is impossible: upstream uses
//! `node.hashCode()`, which for `DefaultDependencyNode` is the JVM's identity
//! hash and differs between runs of the same build. jv numbers nodes in visit
//! order instead, which is stable and equally opaque. Each *occurrence* gets its
//! own number, because upstream's tree holds a separate object per path even
//! where jv's graph shares a subtree.
//!
//! Upstream also writes labels without XML-escaping them. jv does the same: a
//! Maven coordinate contains no markup character, and escaping here would
//! diverge from Maven on every input where it did fire.

use std::fmt::Write as _;

use jv_resolver::{Graph, NodeId};

use crate::coordinates::{node_scope, node_string};
use crate::text::Options;

const HEADER: &str = concat!(
    "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\x20",
    "<graphml xmlns=\"http://graphml.graphdrawing.org/xmlns\" ",
    "xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\" ",
    "xmlns:y=\"http://www.yworks.com/xml/graphml\" ",
    "xsi:schemaLocation=\"http://graphml.graphdrawing.org/xmlns ",
    "http://graphml.graphdrawing.org/xmlns/1.0/graphml.xsd\">\n",
    "  <key for=\"node\" id=\"d0\" yfiles.type=\"nodegraphics\"/>\x20\n",
    "  <key for=\"edge\" id=\"d1\" yfiles.type=\"edgegraphics\"/>\x20\n",
    "<graph id=\"dependencies\" edgedefault=\"directed\">\n",
);

const FOOTER: &str = "</graph></graphml>";

/// Renders `graph` as GraphML.
///
/// Only [`Options::verbose`] is read; the indent tokens have no meaning outside
/// the text renderer.
pub fn render(graph: &Graph, options: Options) -> String {
    let mut out = String::from(HEADER);
    let mut path = Vec::new();
    let mut next_id = 0;
    visit(
        graph,
        graph.root(),
        None,
        &mut next_id,
        &mut path,
        options.verbose,
        &mut out,
    );
    out.push_str(FOOTER);
    out
}

fn visit(
    graph: &Graph,
    id: NodeId,
    parent: Option<usize>,
    next_id: &mut usize,
    path: &mut Vec<NodeId>,
    verbose: bool,
    out: &mut String,
) {
    let element_id = *next_id;
    *next_id += 1;
    let _ = writeln!(
        out,
        "<node id=\"{element_id}\"><data key=\"d0\"><y:ShapeNode><y:NodeLabel>{}</y:NodeLabel></y:ShapeNode></data></node>",
        node_string(graph.node(id), verbose)
    );

    // A node already on this path would recurse forever; it still gets its own
    // element and its edge back to the parent.
    if !path.contains(&id) {
        path.push(id);
        for child in graph.children(id).to_vec() {
            visit(graph, child, Some(element_id), next_id, path, verbose, out);
        }
        path.pop();
    }

    let Some(parent) = parent else {
        // The root writes the footer where every other node writes its edge.
        return;
    };
    let _ = write!(out, "<edge source=\"{parent}\" target=\"{element_id}\">");
    if let Some(scope) = node_scope(graph.node(id)) {
        let _ = write!(
            out,
            "<data key=\"d1\"><y:PolyLineEdge><y:EdgeLabel>{}</y:EdgeLabel></y:PolyLineEdge></data>",
            scope.as_str()
        );
    }
    out.push_str("</edge>\n");
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
        let c = graph.add(dependency_node("g", "c", "3.0", Some(Scope::Test)));
        graph.add_child(root, a);
        graph.add_child(a, c);

        assert_eq!(
            render(&graph, Options::default()),
            concat!(
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\x20",
                "<graphml xmlns=\"http://graphml.graphdrawing.org/xmlns\" ",
                "xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\" ",
                "xmlns:y=\"http://www.yworks.com/xml/graphml\" ",
                "xsi:schemaLocation=\"http://graphml.graphdrawing.org/xmlns ",
                "http://graphml.graphdrawing.org/xmlns/1.0/graphml.xsd\">\n",
                "  <key for=\"node\" id=\"d0\" yfiles.type=\"nodegraphics\"/>\x20\n",
                "  <key for=\"edge\" id=\"d1\" yfiles.type=\"edgegraphics\"/>\x20\n",
                "<graph id=\"dependencies\" edgedefault=\"directed\">\n",
                "<node id=\"0\"><data key=\"d0\"><y:ShapeNode><y:NodeLabel>com.example:demo:jar:1.0</y:NodeLabel></y:ShapeNode></data></node>\n",
                "<node id=\"1\"><data key=\"d0\"><y:ShapeNode><y:NodeLabel>g:a:jar:1.0:compile</y:NodeLabel></y:ShapeNode></data></node>\n",
                "<node id=\"2\"><data key=\"d0\"><y:ShapeNode><y:NodeLabel>g:c:jar:3.0:test</y:NodeLabel></y:ShapeNode></data></node>\n",
                "<edge source=\"1\" target=\"2\"><data key=\"d1\"><y:PolyLineEdge><y:EdgeLabel>test</y:EdgeLabel></y:PolyLineEdge></data></edge>\n",
                "<edge source=\"0\" target=\"1\"><data key=\"d1\"><y:PolyLineEdge><y:EdgeLabel>compile</y:EdgeLabel></y:PolyLineEdge></data></edge>\n",
                "</graph></graphml>",
            )
        );
    }

    #[test]
    fn edges_are_written_after_the_subtree_they_close() {
        // Upstream writes edges from `endVisit`, so a deep edge precedes the
        // shallow one that leads to it. Tools read GraphML as a set, but parity
        // is byte-for-byte.
        let mut graph = Graph::new(project_root("com.example", "demo", "1.0"));
        let root = graph.root();
        let a = graph.add(dependency_node("g", "a", "1.0", Some(Scope::Compile)));
        let c = graph.add(dependency_node("g", "c", "3.0", Some(Scope::Compile)));
        graph.add_child(root, a);
        graph.add_child(a, c);

        let rendered = render(&graph, Options::default());
        let deep = rendered.find("<edge source=\"1\"").expect("a -> c edge");
        let shallow = rendered.find("<edge source=\"0\"").expect("root -> a edge");
        assert!(deep < shallow, "{rendered}");
    }

    #[test]
    fn an_undeclared_scope_leaves_the_edge_unlabelled() {
        // The label is the node's declared scope, the same value the node's own
        // coordinate omits when absent.
        let mut graph = Graph::new(project_root("com.example", "demo", "1.0"));
        let root = graph.root();
        let a = graph.add(dependency_node("g", "a", "1.0", None));
        graph.add_child(root, a);

        let rendered = render(&graph, Options::default());
        assert!(
            rendered.contains("<edge source=\"0\" target=\"1\"></edge>"),
            "{rendered}"
        );
    }

    #[test]
    fn the_document_does_not_end_in_a_newline() {
        // `GRAPHML_FOOTER` is written without a line separator; a tool diffing
        // jv against Maven would see a trailing newline as a difference.
        let graph = Graph::new(project_root("com.example", "demo", "1.0"));
        let rendered = render(&graph, Options::default());
        assert!(rendered.ends_with("</graph></graphml>"), "{rendered}");
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
        // The revisited `a` is a fourth occurrence with its own id, not a
        // back-edge to id 1.
        assert_eq!(rendered.matches("<node id=").count(), 4);
        assert_eq!(rendered.matches("<edge source=").count(), 3);
    }
}
