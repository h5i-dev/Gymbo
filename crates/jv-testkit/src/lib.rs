//! Readers for Maven Resolver's own test corpora.
//!
//! jv's compatibility claims rest on being checked against Maven's tests rather
//! than against jv's expectations, which means being able to read the formats
//! those tests are written in. This crate holds those readers, kept out of the
//! shipped crates because nothing but a test has any use for them.

pub mod graph_dsl;

pub use graph_dsl::{DslError, dump, parse, parse_all, parse_with};
