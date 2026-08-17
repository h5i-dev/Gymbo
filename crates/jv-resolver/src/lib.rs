//! Maven-compatible dependency collection and conflict resolution.
//!
//! Work in progress: the graph representation is in place, and the collection
//! and conflict-resolution passes land next. See `ROADMAP.md` M3.

mod graph;

pub use graph::{Graph, ManagedFlags, Node, NodeId, Premanaged};
