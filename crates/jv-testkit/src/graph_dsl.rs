//! Maven Resolver's dependency-graph DSL.
//!
//! Port of `DependencyGraphParser` and `NodeDefinition` from
//! `maven-resolver-test-util`. Reading this format is what unlocks the 45 graph
//! files under `maven-resolver-util/src/test/resources/transformer/`, which are
//! the only exhaustive statement of what conflict resolution is supposed to do.
//!
//! A file looks like this:
//!
//! ```text
//! # a comment
//! (null)
//! +- test:b:1
//! |  \- test:a:2[2]
//! \- test:c:1        (c)
//!    \- ^c
//! ```
//!
//! Upstream parses each line with one large regular expression. This port hand
//! parses it instead: the grammar is small, the corpus exercises a known subset,
//! and a hand parser says *which* part of a line it choked on — which matters
//! when a corpus file fails to load and the alternative is a regex that simply
//! reports "bad syntax".
//!
//! # Grammar
//!
//! | Piece | Meaning |
//! |---|---|
//! | `#` | comment, to end of line |
//! | `---` | ends this graph; a file may hold several |
//! | `%s` | replaced, left to right, from the caller's substitutions |
//! | `(null)` | a node with no dependency, used as a synthetic root |
//! | `+- `, `\- `, `\|`, spaces | indent; depth is the prefix length divided by three, rounded up |
//! | `g:a:v`, `g:a:ext:v`, `g:a:ext:cls:v` | the coordinate |
//! | `[1,3]` after the coordinate | the version constraint the node was declared with |
//! | `<0.9` after the coordinate | the version before `<dependencyManagement>` changed it |
//! | `compile`, `test`, … | scope; `compile<test` records the pre-management scope |
//! | `optional`, `!optional` | the optional flag |
//! | `relocations=g:a:v,g:b:v` | coordinates this artifact was relocated from |
//! | `props=k:v,k:v` | artifact properties |
//! | `(name)` | names this node |
//! | `^name` | a whole line: attaches the named node again, forming a cycle or a shared subtree |

use std::collections::BTreeMap;

use jv_model::{Artifact, Dependency, Scope};
use jv_resolver::{Graph, Node, NodeId};

/// A corpus file that could not be read.
#[derive(Debug, thiserror::Error)]
pub enum DslError {
    #[error("line {line}: bad syntax: {text}")]
    Syntax { line: usize, text: String },
    #[error("line {line}: {coordinates:?} is not a coordinate")]
    Coordinates { line: usize, coordinates: String },
    #[error("line {line}: unknown scope {scope:?}")]
    Scope { line: usize, scope: String },
    #[error("line {line}: undefined reference ^{name}")]
    UndefinedReference { line: usize, name: String },
    #[error("line {line}: a node at depth {depth} has no parent")]
    DanglingNode { line: usize, depth: usize },
    #[error("not enough substitutions to fill the %s placeholders")]
    MissingSubstitution,
    #[error("line {line}: artifact properties are not modelled yet: {text}")]
    UnsupportedProperties { line: usize, text: String },
}

/// Parses the first graph in `text`.
pub fn parse(text: &str) -> Result<Graph, DslError> {
    parse_with(text, &[])
}

/// Parses the first graph in `text`, filling `%s` placeholders in order.
pub fn parse_with(text: &str, substitutions: &[&str]) -> Result<Graph, DslError> {
    let mut graphs = parse_all(text, substitutions)?;
    if graphs.is_empty() {
        return Ok(Graph::new(Node::root()));
    }
    Ok(graphs.remove(0))
}

/// Parses every graph in `text`. Graphs are separated by a line starting `---`.
pub fn parse_all(text: &str, substitutions: &[&str]) -> Result<Vec<Graph>, DslError> {
    let mut substitutions = substitutions.iter();
    let mut graphs = Vec::new();
    let mut builder: Option<Builder> = None;

    for (index, raw) in text.lines().enumerate() {
        let line_number = index + 1;
        // A comment runs to the end of the line, and may be the whole line.
        let line = raw.split('#').next().unwrap_or("");
        if line.trim().is_empty() {
            continue;
        }
        if line.trim_start().starts_with("---") {
            if let Some(finished) = builder.take() {
                graphs.push(finished.finish());
            }
            continue;
        }

        // Placeholders are filled left to right across the whole file, not per
        // line, so one iterator spans the graph.
        let mut line = line.to_owned();
        while let Some(position) = line.find("%s") {
            let value = substitutions.next().ok_or(DslError::MissingSubstitution)?;
            line.replace_range(position..position + 2, value);
        }

        let (depth, definition) = split_indent(&line);
        let builder = builder.get_or_insert_with(Builder::new);
        builder.push(line_number, depth, definition)?;
    }

    if let Some(finished) = builder.take() {
        graphs.push(finished.finish());
    }
    Ok(graphs)
}

/// Splits a line into its depth and its node definition.
///
/// Upstream splits on the literal `"- "` and measures the part before it, so the
/// art itself is never interpreted: `+- `, `\- ` and `|  ` all contribute only
/// their length. Dividing by three and rounding up turns that length into a
/// depth.
fn split_indent(line: &str) -> (usize, &str) {
    // Faithful to upstream's `split("- ")`, which keeps only the first two parts
    // and would truncate a definition containing "- ". No corpus line does.
    match line.split_once("- ") {
        None => (0, line.trim()),
        Some((prefix, rest)) => {
            let depth = prefix.len().div_ceil(3);
            let definition = rest.split("- ").next().unwrap_or(rest);
            (depth, definition.trim())
        }
    }
}

/// Builds one graph from successive lines.
struct Builder {
    graph: Graph,
    /// The ancestor at each depth, so a line at depth d attaches to `stack[d-1]`.
    stack: Vec<NodeId>,
    /// The most recently built node, which is what a `^name` line attaches to.
    current: Option<NodeId>,
    named: BTreeMap<String, NodeId>,
    started: bool,
}

impl Builder {
    fn new() -> Self {
        Self {
            graph: Graph::new(Node::root()),
            stack: Vec::new(),
            current: None,
            named: BTreeMap::new(),
            started: false,
        }
    }

    fn push(&mut self, line: usize, depth: usize, definition: &str) -> Result<(), DslError> {
        // A `^name` line attaches an existing node to the node most recently
        // built, which is how the corpus spells cycles and shared subtrees.
        if let Some(name) = definition.strip_prefix('^') {
            let name = name.trim();
            let target = *self
                .named
                .get(name)
                .ok_or_else(|| DslError::UndefinedReference {
                    line,
                    name: name.to_owned(),
                })?;
            let parent = self.current.ok_or(DslError::DanglingNode { line, depth })?;
            self.graph.add_child(parent, target);
            return Ok(());
        }

        let node = if definition.eq_ignore_ascii_case("(null)") {
            Node::root()
        } else {
            build_node(line, definition)?
        };
        let name = node_id_of(definition);

        if !self.started {
            // The first node is the root; the graph already has a placeholder
            // for it, so overwrite rather than add.
            *self.graph.node_mut(self.graph.root()) = node;
            self.started = true;
            let root = self.graph.root();
            self.stack = vec![root];
            self.current = Some(root);
            if let Some(name) = name {
                self.named.insert(name, root);
            }
            return Ok(());
        }

        if depth == 0 || depth > self.stack.len() {
            return Err(DslError::DanglingNode { line, depth });
        }
        let parent = self.stack[depth - 1];
        let id = self.graph.add(node);
        self.graph.add_child(parent, id);

        // This node becomes the ancestor for depth+1.
        self.stack.truncate(depth);
        self.stack.push(id);
        self.current = Some(id);
        if let Some(name) = name {
            self.named.insert(name, id);
        }
        Ok(())
    }

    fn finish(self) -> Graph {
        self.graph
    }
}

/// The `(name)` at the end of a definition, if any.
fn node_id_of(definition: &str) -> Option<String> {
    definition
        .split_whitespace()
        .skip(1)
        .filter_map(|token| token.strip_prefix('(')?.strip_suffix(')'))
        .next()
        .map(str::to_owned)
}

/// Parses one node definition into a graph node.
fn build_node(line: usize, definition: &str) -> Result<Node, DslError> {
    let mut tokens = definition.split_whitespace();
    let head = tokens.next().ok_or_else(|| DslError::Syntax {
        line,
        text: definition.to_owned(),
    })?;

    let (coordinates, range, premanaged_version) = split_head(head);
    let artifact = parse_coordinates(line, coordinates)?;

    let mut dependency = Dependency {
        group_id: artifact.group_id.clone(),
        artifact_id: artifact.artifact_id.clone(),
        version: Some(artifact.version.clone()),
        classifier: (!artifact.classifier.is_empty()).then(|| artifact.classifier.clone()),
        // The coordinate's extension travels as the declared type, so that
        // rebuilding the artifact from the dependency reproduces it. Upstream
        // keeps the extension on the artifact and the type as a property of it;
        // jv derives one from the other, so the type has to carry it.
        type_: Some(artifact.extension.clone()),
        ..Default::default()
    };
    let mut node = Node {
        version_constraint: Some(range.unwrap_or(coordinates_version(coordinates)).to_owned()),
        ..Default::default()
    };
    if let Some(previous) = premanaged_version {
        node.managed.version = true;
        node.premanaged.version = Some(previous.to_owned());
    }

    for token in tokens {
        if let Some(list) = token.strip_prefix("relocations=") {
            for coordinates in list.split(',') {
                node.relocations
                    .push(parse_coordinates(line, coordinates.trim())?);
            }
        } else if token.starts_with("props=") {
            // Artifact properties have no home in jv's model yet; failing loudly
            // beats dropping a corpus file's meaning on the floor.
            return Err(DslError::UnsupportedProperties {
                line,
                text: token.to_owned(),
            });
        } else if token.starts_with('(') && token.ends_with(')') {
            // The node's name, already read.
        } else if token == "optional" {
            dependency.optional = Some(true);
        } else if token == "!optional" {
            dependency.optional = Some(false);
        } else {
            let scope = token.strip_prefix("scope=").unwrap_or(token);
            // `compile<test` records what management changed the scope from.
            let (scope, premanaged_scope) = match scope.split_once('<') {
                Some((scope, previous)) => (scope, Some(previous)),
                None => (scope, None),
            };
            dependency.scope = Some(parse_scope(line, scope)?);
            if let Some(previous) = premanaged_scope {
                node.managed.scope = true;
                node.premanaged.scope = Some(parse_scope(line, previous)?);
            }
        }
    }

    node.dependency = Some(dependency);
    node.artifact = Some(artifact);
    Ok(node)
}

/// Splits `coords[range][<premanaged]` into its three parts.
fn split_head(head: &str) -> (&str, Option<&str>, Option<&str>) {
    // The coordinate's version cannot contain `[`, `(` or `<`, so the first of
    // those ends it.
    let end = head.find(['[', '(', '<']).unwrap_or(head.len());
    let (coordinates, mut rest) = head.split_at(end);

    let mut range = None;
    if rest.starts_with('[') || rest.starts_with('(') {
        let close = rest.find([']', ')']).map_or(rest.len(), |index| index + 1);
        let (found, remainder) = rest.split_at(close);
        range = Some(found);
        rest = remainder;
    }

    let premanaged = rest.strip_prefix('<').filter(|text| !text.is_empty());
    (coordinates, range, premanaged)
}

/// The version segment of a coordinate, which is always the last.
fn coordinates_version(coordinates: &str) -> &str {
    coordinates.rsplit(':').next().unwrap_or(coordinates)
}

/// Parses `g:a:v`, `g:a:ext:v` or `g:a:ext:cls:v`.
fn parse_coordinates(line: usize, coordinates: &str) -> Result<Artifact, DslError> {
    let parts: Vec<&str> = coordinates.split(':').collect();
    let bad = || DslError::Coordinates {
        line,
        coordinates: coordinates.to_owned(),
    };
    let (group, artifact, extension, classifier, version) = match parts.as_slice() {
        [group, artifact, version] => (*group, *artifact, "jar", "", *version),
        [group, artifact, extension, version] => (*group, *artifact, *extension, "", *version),
        [group, artifact, extension, classifier, version] => {
            (*group, *artifact, *extension, *classifier, *version)
        }
        _ => return Err(bad()),
    };
    if group.is_empty() || artifact.is_empty() || version.is_empty() {
        return Err(bad());
    }
    Ok(Artifact {
        group_id: group.to_owned(),
        artifact_id: artifact.to_owned(),
        version: version.to_owned(),
        classifier: classifier.to_owned(),
        // An empty extension is spelled `g:a::cls:v` and means the default.
        extension: if extension.is_empty() {
            "jar".to_owned()
        } else {
            extension.to_owned()
        },
    })
}

fn parse_scope(line: usize, scope: &str) -> Result<Scope, DslError> {
    scope.parse().map_err(|_| DslError::Scope {
        line,
        scope: scope.to_owned(),
    })
}

/// Renders a graph in the DSL's shape, for failure messages.
///
/// This is a diagnostic rendering, not a round trip: it always draws `+-` and
/// omits the names and back-references the input may have used.
pub fn dump(graph: &Graph) -> String {
    let mut out = String::new();
    for (id, depth) in graph.preorder() {
        if depth > 0 {
            out.push_str(&" ".repeat((depth - 1) * 3));
            out.push_str("+- ");
        }
        let node = graph.node(id);
        match &node.artifact {
            None => out.push_str("(null)"),
            Some(artifact) => {
                out.push_str(&format!(
                    "{}:{}:{}",
                    artifact.group_id, artifact.artifact_id, artifact.version
                ));
                if let Some(dependency) = &node.dependency {
                    if let Some(scope) = dependency.scope {
                        out.push(' ');
                        out.push_str(scope.as_str());
                    }
                    if dependency.is_optional() {
                        out.push_str(" optional");
                    }
                }
            }
        }
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn coordinates_of(graph: &Graph, id: NodeId) -> String {
        graph
            .node(id)
            .artifact
            .as_ref()
            .map_or_else(|| "(null)".to_owned(), |artifact| artifact.to_string())
    }

    #[test]
    fn depth_comes_from_the_prefix_length() {
        assert_eq!(split_indent("test:a:1"), (0, "test:a:1"));
        assert_eq!(split_indent("+- test:a:1"), (1, "test:a:1"));
        assert_eq!(split_indent("|  \\- test:a:1"), (2, "test:a:1"));
        assert_eq!(split_indent("   +- test:a:1"), (2, "test:a:1"));
        assert_eq!(split_indent("|  |  \\- test:a:1"), (3, "test:a:1"));
    }

    #[test]
    fn a_synthetic_root_has_no_artifact() {
        let graph = parse("(null)\n").unwrap();
        assert!(graph.node(graph.root()).is_synthetic_root());
    }

    #[test]
    fn a_simple_tree() {
        let graph = parse(
            "(null)\n\
             +- test:b:1\n\
             |  \\- test:a:2\n\
             \\- test:c:1\n",
        )
        .unwrap();
        let order: Vec<String> = graph
            .preorder()
            .into_iter()
            .map(|(id, _)| coordinates_of(&graph, id))
            .collect();
        assert_eq!(
            order,
            vec!["(null)", "test:b:jar:1", "test:a:jar:2", "test:c:jar:1"]
        );
    }

    #[test]
    fn ranges_become_version_constraints() {
        let graph = parse(
            "(null)\n\
             +- test:b:1\n\
             |  \\- test:a:2[2]\n\
             \\- test:c:1\n\
             \x20  +- test:a:1[1,3]\n",
        )
        .unwrap();
        let constraints: Vec<Option<String>> = graph
            .preorder()
            .into_iter()
            .map(|(id, _)| graph.node(id).version_constraint.clone())
            .collect();
        assert!(constraints.contains(&Some("[2]".to_owned())));
        assert!(constraints.contains(&Some("[1,3]".to_owned())));
        // A node without a range keeps its plain version as the constraint.
        assert!(constraints.contains(&Some("1".to_owned())));
    }

    #[test]
    fn scopes_and_optional_flags() {
        let graph = parse(
            "(null)\n\
             +- test:a:1 compile\n\
             +- test:b:1 test optional\n\
             \\- test:c:1 runtime !optional\n",
        )
        .unwrap();
        let children = graph.children(graph.root()).to_vec();
        assert_eq!(graph.node(children[0]).scope(), Scope::Compile);
        assert_eq!(graph.node(children[1]).scope(), Scope::Test);
        assert!(graph.node(children[1]).is_optional());
        assert_eq!(graph.node(children[2]).scope(), Scope::Runtime);
        assert!(!graph.node(children[2]).is_optional());
    }

    #[test]
    fn premanaged_version_and_scope() {
        let graph = parse("(null)\n\\- test:a:2<1 compile<test\n").unwrap();
        let child = graph.children(graph.root())[0];
        let node = graph.node(child);
        assert!(node.managed.version);
        assert_eq!(node.premanaged.version.as_deref(), Some("1"));
        assert!(node.managed.scope);
        assert_eq!(node.premanaged.scope, Some(Scope::Test));
        assert_eq!(node.scope(), Scope::Compile);
    }

    #[test]
    fn relocations_are_read() {
        let graph = parse("(null)\n\\- test:c:1 relocations=test:a:1,test:b:1\n").unwrap();
        let child = graph.children(graph.root())[0];
        let relocations = &graph.node(child).relocations;
        assert_eq!(relocations.len(), 2);
        assert_eq!(relocations[0].artifact_id, "a");
        assert_eq!(relocations[1].artifact_id, "b");
    }

    #[test]
    fn names_and_back_references_form_cycles() {
        // The shape of version-resolver/unsolvable-with-cycle.txt.
        let graph = parse(
            "cycle:root:1\n\
             +- cycle:x:1       (x)\n\
             |  \\- ^x\n\
             \\- cycle:b:1\n",
        )
        .unwrap();
        let root = graph.root();
        let x = graph.children(root)[0];
        // `^x` attached the named node to itself.
        assert_eq!(graph.children(x), &[x]);
        // The walk still terminates.
        assert_eq!(graph.preorder().len(), 4);
    }

    #[test]
    fn a_reference_can_share_a_subtree() {
        let graph = parse(
            "(null)\n\
             +- test:a:1     (a)\n\
             |  \\- test:leaf:1\n\
             \\- test:b:1\n\
             \x20  \\- ^a\n",
        )
        .unwrap();
        let root = graph.root();
        let a = graph.children(root)[0];
        let b = graph.children(root)[1];
        assert_eq!(graph.children(b), &[a]);
        // Shared, not copied: the same node appears under both parents.
        assert_eq!(graph.children(a).len(), 1);
    }

    #[test]
    fn substitutions_fill_placeholders_in_order() {
        let graph = parse_with(
            "(null)\n\
             +- gid:aid2:ver\n\
             |  \\- gid:aid:ver %s\n\
             \\- gid:aid3:ver\n\
             \x20  \\- gid:aid:ver %s\n",
            &["compile", "test"],
        )
        .unwrap();
        let scopes: Vec<Scope> = graph
            .preorder()
            .into_iter()
            .filter(|(id, _)| {
                graph
                    .node(*id)
                    .artifact
                    .as_ref()
                    .is_some_and(|a| a.artifact_id == "aid")
            })
            .map(|(id, _)| graph.node(id).scope())
            .collect();
        assert_eq!(scopes, vec![Scope::Compile, Scope::Test]);
    }

    #[test]
    fn too_few_substitutions_is_an_error() {
        let error = parse_with("(null)\n\\- g:a:1 %s\n", &[]).unwrap_err();
        assert!(matches!(error, DslError::MissingSubstitution));
    }

    #[test]
    fn comments_and_blank_lines_are_ignored() {
        let graph = parse(
            "# a leading comment\n\
             \n\
             (null)\n\
             +- test:a:1   # trailing comment\n",
        )
        .unwrap();
        assert_eq!(graph.preorder().len(), 2);
    }

    #[test]
    fn coordinates_accept_extension_and_classifier() {
        let graph = parse(
            "(null)\n\
             +- g:a:1\n\
             +- g:b:war:2\n\
             \\- g:c:jar:tests:3\n",
        )
        .unwrap();
        let children = graph.children(graph.root()).to_vec();
        assert_eq!(coordinates_of(&graph, children[0]), "g:a:jar:1");
        assert_eq!(coordinates_of(&graph, children[1]), "g:b:war:2");
        assert_eq!(coordinates_of(&graph, children[2]), "g:c:jar:tests:3");
    }

    #[test]
    fn several_graphs_separated_by_a_marker() {
        let graphs = parse_all(
            "(null)\n\
             \\- g:a:1\n\
             ---\n\
             (null)\n\
             \\- g:b:1\n",
            &[],
        )
        .unwrap();
        assert_eq!(graphs.len(), 2);
        assert_eq!(graphs[0].preorder().len(), 2);
        assert_eq!(graphs[1].preorder().len(), 2);
    }

    #[test]
    fn an_undefined_reference_is_an_error() {
        let error = parse("(null)\n\\- ^nope\n").unwrap_err();
        assert!(matches!(error, DslError::UndefinedReference { .. }));
    }

    #[test]
    fn an_unknown_scope_is_an_error() {
        let error = parse("(null)\n\\- g:a:1 bogus\n").unwrap_err();
        assert!(matches!(error, DslError::Scope { .. }));
    }

    #[test]
    fn properties_are_rejected_rather_than_dropped() {
        let error = parse("(null)\n\\- g:a:1 props=k:v\n").unwrap_err();
        assert!(matches!(error, DslError::UnsupportedProperties { .. }));
    }

    #[test]
    fn dump_is_readable() {
        let graph = parse("(null)\n+- g:a:1 compile\n\\- g:b:1 test optional\n").unwrap();
        assert_eq!(
            dump(&graph),
            "(null)\n+- g:a:1 compile\n+- g:b:1 test optional\n"
        );
    }
}
