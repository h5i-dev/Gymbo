//! Renders dependency graphs the way Maven does.
//!
//! `mvn dependency:tree` writes one of five output types, selected with
//! `-DoutputType`, and jv must match all five byte for byte. Each is a port of
//! the corresponding upstream visitor rather than a lookalike:
//!
//! - [`text`] — `SerializingDependencyNodeVisitor` (`maven-dependency-tree`);
//! - [`dot`] — `DOTDependencyNodeVisitor`;
//! - [`graphml`] — `GraphmlDependencyNodeVisitor`;
//! - [`tgf`] — `TGFDependencyNodeVisitor`;
//! - [`json`] — `JsonDependencyNodeVisitor`.
//!
//! All but json label their nodes with the same string, which the private
//! `coordinates` module owns.

mod coordinates;
pub mod dot;
pub mod graphml;
pub mod json;
pub mod text;
pub mod tgf;

use std::fmt;
use std::str::FromStr;

use jv_resolver::Graph;

pub use coordinates::Omission;
pub use text::{Options, Tokens};

/// Which output type to render, spelled as `-DoutputType` accepts it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Format {
    #[default]
    Text,
    Dot,
    Graphml,
    Tgf,
    Json,
}

impl Format {
    /// The `-DoutputType` spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Format::Text => "text",
            Format::Dot => "dot",
            Format::Graphml => "graphml",
            Format::Tgf => "tgf",
            Format::Json => "json",
        }
    }
}

impl fmt::Display for Format {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Format {
    type Err = UnknownFormat;

    /// Parses a `-DoutputType` value.
    ///
    /// The comparison is exact, as upstream's chain of `"graphml".equals(..)` is.
    /// Upstream falls back to text for anything unrecognised, silently; jv
    /// reports it instead and leaves the fallback to the caller, so a typo in a
    /// build script is not answered with the wrong format.
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        match text {
            "text" => Ok(Format::Text),
            "dot" => Ok(Format::Dot),
            "graphml" => Ok(Format::Graphml),
            "tgf" => Ok(Format::Tgf),
            "json" => Ok(Format::Json),
            other => Err(UnknownFormat(other.to_owned())),
        }
    }
}

/// A `-DoutputType` value that names no known output type.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnknownFormat(pub String);

impl fmt::Display for UnknownFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "unknown output type `{}`", self.0)
    }
}

impl std::error::Error for UnknownFormat {}

/// Renders `graph` in `format`.
///
/// `options` is ignored for [`Format::Json`], whose upstream visitor has no
/// settings at all — not even a verbose mode.
pub fn render(graph: &Graph, format: Format, options: Options) -> String {
    match format {
        Format::Text => text::render(graph, options),
        Format::Dot => dot::render(graph, options),
        Format::Graphml => graphml::render(graph, options),
        Format::Tgf => tgf::render(graph, options),
        Format::Json => json::render(graph),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jv_model::Artifact;
    use jv_resolver::Node;

    fn graph() -> Graph {
        Graph::new(Node {
            artifact: Some(Artifact::new("com.example", "demo", "1.0")),
            ..Node::default()
        })
    }

    #[test]
    fn every_format_round_trips_through_its_output_type_spelling() {
        for format in [
            Format::Text,
            Format::Dot,
            Format::Graphml,
            Format::Tgf,
            Format::Json,
        ] {
            assert_eq!(format.to_string().parse(), Ok(format));
        }
    }

    #[test]
    fn an_unknown_output_type_is_reported_rather_than_falling_back() {
        // Upstream would quietly render text here, which turns a typo into
        // output that looks deliberate.
        assert_eq!(
            "Text".parse::<Format>(),
            Err(UnknownFormat("Text".to_owned()))
        );
        assert_eq!(
            "xml".parse::<Format>(),
            Err(UnknownFormat("xml".to_owned()))
        );
    }

    #[test]
    fn the_dispatcher_picks_the_same_renderer_the_module_does() {
        let graph = graph();
        let options = Options::default();
        assert_eq!(
            render(&graph, Format::Text, options),
            text::render(&graph, options)
        );
        assert_eq!(
            render(&graph, Format::Dot, options),
            dot::render(&graph, options)
        );
        assert_eq!(
            render(&graph, Format::Graphml, options),
            graphml::render(&graph, options)
        );
        assert_eq!(
            render(&graph, Format::Tgf, options),
            tgf::render(&graph, options)
        );
        assert_eq!(render(&graph, Format::Json, options), json::render(&graph));
    }
}
