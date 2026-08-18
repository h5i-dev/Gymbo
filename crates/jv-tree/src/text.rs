//! The text rendering `mvn dependency:tree` produces, `-DoutputType=text`.
//!
//! Byte-for-byte parity with Maven is a launch requirement, so this is a port
//! rather than an approximation. The indent art comes from
//! `SerializingDependencyNodeVisitor` and its `GraphTokens`
//! (`maven-dependency-tree`); the node text itself lives in the crate's private
//! `coordinates` module, because every other output type embeds the same string.

use std::fmt::Write as _;

use jv_resolver::{Graph, NodeId};

use crate::coordinates::node_string;

/// The indent art to draw the tree with.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Tokens {
    /// `+- `, `\- `, `|  ` — Maven's default, and what the `text` output type
    /// produces.
    #[default]
    Standard,
    /// Spaces only.
    Whitespace,
    /// The same shape in box-drawing characters.
    Extended,
}

impl Tokens {
    /// The prefix drawn immediately before a node.
    fn node_indent(self, last: bool) -> &'static str {
        match (self, last) {
            (Tokens::Standard, false) => "+- ",
            (Tokens::Standard, true) => "\\- ",
            (Tokens::Whitespace, _) => "   ",
            (Tokens::Extended, false) => "\u{251C}\u{2500} ",
            (Tokens::Extended, true) => "\u{2514}\u{2500} ",
        }
    }

    /// The prefix drawn for each ancestor level, continuing or closing its line.
    fn fill_indent(self, last: bool) -> &'static str {
        match (self, last) {
            (Tokens::Standard, false) => "|  ",
            (Tokens::Standard, true) => "   ",
            (Tokens::Whitespace, _) => "   ",
            (Tokens::Extended, false) => "\u{2502}  ",
            (Tokens::Extended, true) => "   ",
        }
    }
}

/// How to render a tree.
#[derive(Clone, Copy, Debug, Default)]
pub struct Options {
    pub tokens: Tokens,
    /// Annotate managed and omitted nodes, as `-Dverbose` does. Requires a graph
    /// that kept its losers.
    pub verbose: bool,
}

/// Renders `graph` as text.
///
/// The result ends with a newline after every line, including the last, matching
/// Maven's use of `println` per node.
pub fn render(graph: &Graph, options: Options) -> String {
    let mut out = String::new();
    // `last[k]` records whether the ancestor at depth k is its parent's final
    // child, which is what decides between a continuing `|` and blank fill.
    let mut last = Vec::new();
    let mut path = Vec::new();
    render_node(
        graph,
        graph.root(),
        true,
        &mut last,
        &mut path,
        &options,
        &mut out,
    );
    out
}

fn render_node(
    graph: &Graph,
    id: NodeId,
    is_last: bool,
    last: &mut Vec<bool>,
    path: &mut Vec<NodeId>,
    options: &Options,
    out: &mut String,
) {
    let depth = last.len();
    // Ancestors first: one fill per level above this node.
    for ancestor_is_last in last.iter().take(depth.saturating_sub(1)) {
        out.push_str(options.tokens.fill_indent(*ancestor_is_last));
    }
    if depth > 0 {
        out.push_str(options.tokens.node_indent(is_last));
    }
    let _ = writeln!(out, "{}", node_string(graph.node(id), options.verbose));

    // A node already on this path would recurse forever.
    if path.contains(&id) {
        return;
    }
    path.push(id);
    let children = graph.children(id).to_vec();
    for (index, child) in children.iter().enumerate() {
        let child_is_last = index + 1 == children.len();
        last.push(child_is_last);
        render_node(graph, *child, child_is_last, last, path, options, out);
        last.pop();
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
    fn a_lone_root_prints_its_coordinates() {
        let graph = Graph::new(project_root("com.example", "demo", "1.0"));
        assert_eq!(
            render(&graph, Options::default()),
            "com.example:demo:jar:1.0\n"
        );
    }

    #[test]
    fn standard_tokens_match_maven() {
        let mut graph = Graph::new(project_root("com.example", "demo", "1.0"));
        let root = graph.root();
        let a = graph.add(dependency_node("g", "a", "1.0", Some(Scope::Compile)));
        let b = graph.add(dependency_node("g", "b", "2.0", Some(Scope::Test)));
        let a_child = graph.add(dependency_node("g", "a-child", "3.0", Some(Scope::Compile)));
        graph.add_child(root, a);
        graph.add_child(a, a_child);
        graph.add_child(root, b);

        assert_eq!(
            render(&graph, Options::default()),
            "\
com.example:demo:jar:1.0
+- g:a:jar:1.0:compile
|  \\- g:a-child:jar:3.0:compile
\\- g:b:jar:2.0:test
"
        );
    }

    #[test]
    fn fill_closes_under_a_last_child() {
        // A grandchild under the *last* child gets blank fill, not `|`.
        let mut graph = Graph::new(project_root("com.example", "demo", "1.0"));
        let root = graph.root();
        let a = graph.add(dependency_node("g", "a", "1.0", Some(Scope::Compile)));
        let b = graph.add(dependency_node("g", "b", "2.0", Some(Scope::Compile)));
        let deep = graph.add(dependency_node("g", "deep", "3.0", Some(Scope::Compile)));
        let deeper = graph.add(dependency_node("g", "deeper", "4.0", Some(Scope::Compile)));
        graph.add_child(root, a);
        graph.add_child(root, b);
        graph.add_child(b, deep);
        graph.add_child(deep, deeper);

        assert_eq!(
            render(&graph, Options::default()),
            "\
com.example:demo:jar:1.0
+- g:a:jar:1.0:compile
\\- g:b:jar:2.0:compile
   \\- g:deep:jar:3.0:compile
      \\- g:deeper:jar:4.0:compile
"
        );
    }

    #[test]
    fn deep_fill_keeps_ancestor_lines_open() {
        let mut graph = Graph::new(project_root("com.example", "demo", "1.0"));
        let root = graph.root();
        let a = graph.add(dependency_node("g", "a", "1.0", Some(Scope::Compile)));
        let child = graph.add(dependency_node("g", "child", "1.0", Some(Scope::Compile)));
        let grandchild = graph.add(dependency_node(
            "g",
            "grandchild",
            "1.0",
            Some(Scope::Compile),
        ));
        let b = graph.add(dependency_node("g", "b", "1.0", Some(Scope::Compile)));
        graph.add_child(root, a);
        graph.add_child(a, child);
        graph.add_child(child, grandchild);
        graph.add_child(root, b);

        assert_eq!(
            render(&graph, Options::default()),
            "\
com.example:demo:jar:1.0
+- g:a:jar:1.0:compile
|  \\- g:child:jar:1.0:compile
|     \\- g:grandchild:jar:1.0:compile
\\- g:b:jar:1.0:compile
"
        );
    }

    #[test]
    fn type_and_classifier_use_the_declaration() {
        let mut graph = Graph::new(project_root("com.example", "demo", "1.0"));
        let root = graph.root();
        let mut dependency = Dependency::new("g", "a", "1.0");
        dependency.type_ = Some("test-jar".to_owned());
        dependency.classifier = Some("tests".to_owned());
        dependency.scope = Some(Scope::Test);
        // On disk it is a plain jar with a classifier.
        let artifact = Artifact::new("g", "a", "1.0")
            .with_classifier("tests")
            .with_extension("jar");
        let node = graph.add(GraphNode::dependency(dependency, artifact));
        graph.add_child(root, node);

        assert_eq!(
            render(&graph, Options::default()),
            "com.example:demo:jar:1.0\n\\- g:a:test-jar:tests:1.0:test\n"
        );
    }

    #[test]
    fn snapshots_print_their_base_version() {
        let mut graph = Graph::new(project_root("com.example", "demo", "1.0"));
        let root = graph.root();
        let dependency = Dependency::new("g", "a", "1.0-SNAPSHOT");
        // Resolution replaced the version with the deployed timestamp.
        let artifact = Artifact::new("g", "a", "1.0-20240115.103000-7");
        let mut node = GraphNode::dependency(dependency, artifact);
        node.dependency.as_mut().unwrap().scope = Some(Scope::Compile);
        let id = graph.add(node);
        graph.add_child(root, id);

        assert!(
            render(&graph, Options::default()).contains("g:a:jar:1.0-SNAPSHOT:compile"),
            "got {}",
            render(&graph, Options::default())
        );
    }

    #[test]
    fn optional_is_marked() {
        let mut graph = Graph::new(project_root("com.example", "demo", "1.0"));
        let root = graph.root();
        let mut dependency = Dependency::new("g", "a", "1.0");
        dependency.scope = Some(Scope::Compile);
        dependency.optional = Some(true);
        let node = graph.add(GraphNode::dependency(
            dependency,
            Artifact::new("g", "a", "1.0"),
        ));
        graph.add_child(root, node);
        assert!(render(&graph, Options::default()).contains("g:a:jar:1.0:compile (optional)"));
    }

    #[test]
    fn whitespace_and_extended_tokens() {
        let mut graph = Graph::new(project_root("com.example", "demo", "1.0"));
        let root = graph.root();
        let a = graph.add(dependency_node("g", "a", "1.0", Some(Scope::Compile)));
        graph.add_child(root, a);

        let whitespace = render(
            &graph,
            Options {
                tokens: Tokens::Whitespace,
                verbose: false,
            },
        );
        assert!(whitespace.contains("   g:a:jar:1.0:compile"));
        assert!(!whitespace.contains('\\'));

        let extended = render(
            &graph,
            Options {
                tokens: Tokens::Extended,
                verbose: false,
            },
        );
        assert!(extended.contains("\u{2514}\u{2500} g:a:jar:1.0:compile"));
    }

    #[test]
    fn verbose_reports_managed_values() {
        let mut graph = Graph::new(project_root("com.example", "demo", "1.0"));
        let root = graph.root();
        let mut node = dependency_node("g", "a", "2.0", Some(Scope::Runtime));
        node.premanaged.version = Some("1.0".to_owned());
        node.premanaged.scope = Some(Scope::Compile);
        let id = graph.add(node);
        graph.add_child(root, id);

        let rendered = render(
            &graph,
            Options {
                tokens: Tokens::Standard,
                verbose: true,
            },
        );
        assert!(
            rendered.contains(
                "g:a:jar:2.0:runtime (version managed from 1.0; scope managed from compile)"
            ),
            "got {rendered}"
        );
    }

    #[test]
    fn verbose_wraps_omitted_nodes() {
        let mut graph = Graph::new(project_root("com.example", "demo", "1.0"));
        let root = graph.root();
        let mut loser = dependency_node("g", "a", "1.0", Some(Scope::Compile));
        loser.omitted_for = Some("2.0".to_owned());
        let mut duplicate = dependency_node("g", "b", "1.0", Some(Scope::Compile));
        duplicate.omitted_for = Some("1.0".to_owned());
        let loser = graph.add(loser);
        let duplicate = graph.add(duplicate);
        graph.add_child(root, loser);
        graph.add_child(root, duplicate);

        let rendered = render(
            &graph,
            Options {
                tokens: Tokens::Standard,
                verbose: true,
            },
        );
        assert!(
            rendered.contains("(g:a:jar:1.0:compile - omitted for conflict with 2.0)"),
            "got {rendered}"
        );
        assert!(
            rendered.contains("(g:b:jar:1.0:compile - omitted for duplicate)"),
            "got {rendered}"
        );
    }

    #[test]
    fn verbose_reports_a_scope_conflict_resolution_declined_to_apply() {
        let mut graph = Graph::new(project_root("com.example", "demo", "1.0"));
        let root = graph.root();
        let mut node = dependency_node("g", "a", "1.0", Some(Scope::Compile));
        // Set, and deliberately not rendered: maven-dependency-tree never
        // populates its equivalent, so `scope updated from` cannot appear in
        // real `-Dverbose` output. See `verbose_string`.
        node.original_scope = Some(Scope::Runtime);
        node.ignored_scope = Some(Scope::Test);
        let id = graph.add(node);
        graph.add_child(root, id);

        let rendered = render(
            &graph,
            Options {
                tokens: Tokens::Standard,
                verbose: true,
            },
        );
        assert!(
            rendered.contains("g:a:jar:1.0:compile (scope not updated to test)"),
            "got {rendered}"
        );
        assert!(
            !rendered.contains("scope updated from"),
            "the dead upstream annotation came back: {rendered}"
        );
    }

    #[test]
    fn verbose_orders_notes_the_way_maven_does() {
        let mut graph = Graph::new(project_root("com.example", "demo", "1.0"));
        let root = graph.root();
        let mut node = dependency_node("g", "a", "2.0", Some(Scope::Runtime));
        node.premanaged.version = Some("1.0".to_owned());
        node.premanaged.scope = Some(Scope::Compile);
        node.ignored_scope = Some(Scope::Test);
        let id = graph.add(node);
        graph.add_child(root, id);

        let rendered = render(
            &graph,
            Options {
                tokens: Tokens::Standard,
                verbose: true,
            },
        );
        assert!(
            rendered.contains(
                "(version managed from 1.0; scope managed from compile; scope not updated to test)"
            ),
            "got {rendered}"
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
        assert_eq!(rendered.lines().count(), 4);
    }
}
