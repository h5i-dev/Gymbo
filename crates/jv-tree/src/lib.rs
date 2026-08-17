//! Renders dependency graphs the way Maven does.
//!
//! `mvn dependency:tree`'s text output is the one jv must match byte for byte,
//! so [`text`] is a port of Maven's renderer rather than a lookalike. The other
//! output types the plugin supports (json, dot, tgf, graphml) follow.

pub mod text;

pub use text::{Omission, Options, Tokens, render};
