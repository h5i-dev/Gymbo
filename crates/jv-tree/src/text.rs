//! The text rendering `mvn dependency:tree` produces.
//!
//! Byte-for-byte parity with Maven is a launch requirement, so this is a port
//! rather than an approximation. Three upstream pieces decide the output, and
//! all three live in different artifacts:
//!
//! - the indent art: `SerializingDependencyNodeVisitor` and its `GraphTokens`
//!   (`maven-dependency-tree`);
//! - the node text: `DefaultDependencyNode.toNodeString()`, which is
//!   `DefaultArtifact.toString()` plus an optional marker (`maven-artifact`);
//! - the verbose annotations: `VerboseDependencyNode.toNodeString()`.
//!
//! Two details are easy to get wrong and both are load-bearing. The coordinate
//! prints the declared **type**, not the file extension, so a `test-jar`
//! dependency shows as `test-jar` while living on disk as a `.jar`. And it
//! prints the **base** version, so a resolved snapshot appears as
//! `1.0-SNAPSHOT`, never as `1.0-20240115.103000-7`.

use std::fmt::Write as _;

use jv_model::{Artifact, Scope};
use jv_resolver::{Graph, Node, NodeId};

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

/// Why a node was omitted, which verbose output reports and plain output never
/// shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Omission {
    /// A node with the same coordinates already won.
    Duplicate,
    /// Another version won; this names it.
    Conflict { winner: String },
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
    for level in 0..depth.saturating_sub(1) {
        out.push_str(options.tokens.fill_indent(last[level]));
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

/// One node's text, without indent.
fn node_string(node: &Node, verbose: bool) -> String {
    let coordinates = coordinates(node);
    if !verbose {
        let mut text = coordinates;
        if node.is_optional() {
            text.push_str(" (optional)");
        }
        return text;
    }
    verbose_string(node, &coordinates)
}

/// `groupId:artifactId:type[:classifier]:baseVersion[:scope]`.
fn coordinates(node: &Node) -> String {
    let Some(artifact) = &node.artifact else {
        // A synthetic root has nothing to print; Maven never renders one.
        return String::new();
    };
    let mut text = String::with_capacity(64);
    text.push_str(&artifact.group_id);
    text.push(':');
    text.push_str(&artifact.artifact_id);
    text.push(':');
    // The declared type, which is not always the file extension.
    text.push_str(node_type(node, artifact));
    if !artifact.classifier.is_empty() {
        text.push(':');
        text.push_str(&artifact.classifier);
    }
    text.push(':');
    // The base version: a resolved snapshot prints as -SNAPSHOT.
    text.push_str(&artifact.base_version());
    if let Some(scope) = node_scope(node) {
        text.push(':');
        text.push_str(scope.as_str());
    }
    text
}

/// The type to print: what the dependency declared, falling back to the
/// artifact's extension for a node that has no declaration, such as the project
/// at the root of the tree.
fn node_type<'a>(node: &'a Node, artifact: &'a Artifact) -> &'a str {
    node.dependency
        .as_ref()
        .map_or(artifact.extension.as_str(), |dependency| {
            dependency.type_or_default()
        })
}

/// The scope to print. The project at the root has none, and Maven omits the
/// field entirely rather than printing a default.
fn node_scope(node: &Node) -> Option<Scope> {
    node.dependency
        .as_ref()
        .and_then(|dependency| dependency.scope)
}

/// The verbose form: annotations in parentheses, and omitted nodes wrapped
/// whole.
fn verbose_string(node: &Node, coordinates: &str) -> String {
    let mut notes: Vec<String> = Vec::new();
    if let Some(version) = &node.premanaged.version {
        notes.push(format!("version managed from {version}"));
    }
    if let Some(scope) = node.premanaged.scope {
        notes.push(format!("scope managed from {scope}"));
    }
    // Conflict resolution's own scope changes, distinct from management's.
    if let Some(scope) = node.original_scope {
        notes.push(format!("scope updated from {scope}"));
    }
    if let Some(scope) = node.ignored_scope {
        notes.push(format!("scope not updated to {scope}"));
    }

    let omission = omission_of(node);
    if let Some(omission) = &omission {
        notes.push(match omission {
            Omission::Duplicate => "omitted for duplicate".to_owned(),
            Omission::Conflict { winner } => format!("omitted for conflict with {winner}"),
        });
    }

    let mut text = String::with_capacity(coordinates.len() + 48);
    // An omitted node is wrapped in parentheses and separated with " - ";
    // an included one appends its notes in parentheses.
    if omission.is_some() {
        text.push('(');
    }
    text.push_str(coordinates);
    if node.is_optional() {
        text.push_str(" (optional)");
    }
    if !notes.is_empty() {
        text.push_str(if omission.is_some() { " - " } else { " (" });
        text.push_str(&notes.join("; "));
        if omission.is_none() {
            text.push(')');
        }
    }
    if omission.is_some() {
        text.push(')');
    }
    text
}

/// Reads the omission a graph recorded on a losing node.
///
/// Conflict resolution stores the winner's version on nodes it kept only for
/// verbose output; a graph built without verbosity has none, so plain rendering
/// never consults this.
fn omission_of(node: &Node) -> Option<Omission> {
    let winner = node.omitted_for.as_ref()?;
    let own = node.artifact.as_ref().map(Artifact::base_version);
    if own.as_deref() == Some(winner.as_str()) {
        Some(Omission::Duplicate)
    } else {
        Some(Omission::Conflict {
            winner: winner.clone(),
        })
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
    fn verbose_reports_scope_changes_from_conflict_resolution() {
        let mut graph = Graph::new(project_root("com.example", "demo", "1.0"));
        let root = graph.root();
        let mut node = dependency_node("g", "a", "1.0", Some(Scope::Compile));
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
            rendered.contains(
                "g:a:jar:1.0:compile (scope updated from runtime; scope not updated to test)"
            ),
            "got {rendered}"
        );
    }

    #[test]
    fn verbose_orders_notes_the_way_maven_does() {
        let mut graph = Graph::new(project_root("com.example", "demo", "1.0"));
        let root = graph.root();
        let mut node = dependency_node("g", "a", "2.0", Some(Scope::Runtime));
        node.premanaged.version = Some("1.0".to_owned());
        node.premanaged.scope = Some(Scope::Compile);
        node.original_scope = Some(Scope::Test);
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
                "(version managed from 1.0; scope managed from compile; scope updated from test)"
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
