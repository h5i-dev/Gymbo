//! Maven-compatible dependency collection and conflict resolution.
//!
//! Work in progress: the graph representation is in place, and the collection
//! and conflict-resolution passes land next. See `ROADMAP.md` M3.

mod conflict_id;
mod conflict_order;
mod graph;
mod scope_rules;

pub use conflict_id::{ConflictId, mark_conflict_ids};
pub use conflict_order::{ConflictOrder, sort_conflict_ids};
pub use graph::{Graph, ManagedFlags, Node, NodeId, Premanaged};
pub use scope_rules::{choose_effective_scope, derive_scope};
