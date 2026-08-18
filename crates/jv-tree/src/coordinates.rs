//! The string one node prints as, shared by every output type.
//!
//! `dependency:tree` labels a node the same way whatever the output type: dot
//! quotes it, graphml wraps it in a `<y:NodeLabel>`, tgf puts it after the node
//! id, and the text renderer indents it. Upstream that string is
//! `DefaultDependencyNode.toNodeString()` — `DefaultArtifact.toString()` plus an
//! optional marker (`maven-dependency-tree`) — or
//! `VerboseDependencyNode.toNodeString()` when `-Dverbose` is on.
//!
//! Two details are easy to get wrong and both are load-bearing. The coordinate
//! prints the declared **type**, not the file extension, so a `test-jar`
//! dependency shows as `test-jar` while living on disk as a `.jar`. And it
//! prints the **base** version, so a resolved snapshot appears as
//! `1.0-SNAPSHOT`, never as `1.0-20240115.103000-7`.

use jv_model::{Artifact, Scope};
use jv_resolver::Node;

/// Why a node was omitted, which verbose output reports and plain output never
/// shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Omission {
    /// A node with the same coordinates already won.
    Duplicate,
    /// Another version won; this names it.
    Conflict { winner: String },
}

/// One node's label, without indent or surrounding syntax.
pub(crate) fn node_string(node: &Node, verbose: bool) -> String {
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
pub(crate) fn node_type<'a>(node: &'a Node, artifact: &'a Artifact) -> &'a str {
    node.dependency
        .as_ref()
        .map_or(artifact.extension.as_str(), |dependency| {
            dependency.type_or_default()
        })
}

/// The scope to print. The project at the root has none, and Maven omits the
/// field entirely rather than printing a default.
///
/// Graphml and tgf label their edges with this same value, so a node whose
/// coordinate carries no scope contributes an unlabelled edge.
pub(crate) fn node_scope(node: &Node) -> Option<Scope> {
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
    // `scope updated from <s>` is deliberately *not* rendered, though
    // `Node::original_scope` carries what it would say.
    //
    // maven-resolver sets `NODE_DATA_ORIGINAL_SCOPE` on every conflict winner,
    // and jv mirrors that faithfully — but what renders `-Dverbose` is
    // maven-dependency-tree, whose `ConflictData` is built from the winner
    // version and the reduced scope alone. Nothing ever calls
    // `setOriginalScope`, so `getOriginalScope()` is always null and the line
    // is unreachable in real output. `docs/spec/conflict-resolution.md` records
    // it as dead upstream code; rendering it here put an annotation on every
    // node of a tree where Maven prints none.
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
    use jv_model::Dependency;

    #[test]
    fn a_synthetic_root_has_no_label_at_all() {
        // Maven's tree is always rooted at the project artifact, so there is no
        // upstream spelling for a root without one; the empty string keeps the
        // surrounding syntax of every format well-formed.
        assert_eq!(node_string(&Node::root(), false), "");
    }

    #[test]
    fn the_declared_type_wins_over_the_extension() {
        let mut dependency = Dependency::new("g", "a", "1.0");
        dependency.type_ = Some("test-jar".to_owned());
        dependency.scope = Some(Scope::Test);
        let artifact = Artifact::new("g", "a", "1.0")
            .with_classifier("tests")
            .with_extension("jar");
        let node = Node::dependency(dependency, artifact);
        assert_eq!(node_string(&node, false), "g:a:test-jar:tests:1.0:test");
    }

    #[test]
    fn a_resolved_snapshot_prints_its_base_version() {
        let mut dependency = Dependency::new("g", "a", "1.0-SNAPSHOT");
        dependency.scope = Some(Scope::Compile);
        // Resolution replaced the version with the deployed timestamp.
        let node = Node::dependency(dependency, Artifact::new("g", "a", "1.0-20240115.103000-7"));
        assert_eq!(node_string(&node, false), "g:a:jar:1.0-SNAPSHOT:compile");
    }

    #[test]
    fn a_node_without_a_declared_scope_contributes_no_edge_label() {
        let node = Node::dependency(
            Dependency::new("g", "a", "1.0"),
            Artifact::new("g", "a", "1"),
        );
        assert_eq!(node_scope(&node), None);
        assert_eq!(node_scope(&Node::root()), None);
    }
}
