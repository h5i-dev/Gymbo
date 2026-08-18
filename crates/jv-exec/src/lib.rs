//! Running a JVM tool straight from its Maven coordinates.
//!
//! This is the logic behind `jvx` and `jv exec`. Everything here is either pure
//! or reads a single file, so the interesting decisions — what an endpoint
//! means, which version "no version" resolves to, which class gets run — are
//! testable without a network, a cache, or a JVM.
//!
//! The resolve itself is not here. `jv-driver`'s `Session` already knows how to
//! turn one artifact into a resolved graph, and duplicating any of that would
//! mean `jvx` and `jv tree` could disagree about the same coordinates.
//!
//! # Prior art
//!
//! The endpoint grammar is jgo's (`src/jgo/parse/_endpoint.py` and
//! `_coordinate.py`), narrowed — see [`endpoint`] for what was dropped and why.
//! The main-class ladder is jgo's `env/_jar.py` with coursier's
//! `modules/install/.../MainClass.scala` as the second opinion; coursier is
//! where the "which jar's manifest do you believe" question is taken seriously,
//! and its answer — the *first* jar on the classpath, which is the one the user
//! named — is what [`main_class`] implements.

pub mod endpoint;
pub mod error;
pub mod java;
pub mod launch;
pub mod main_class;
pub mod manifest;
pub mod release;

pub use endpoint::{Endpoint, EndpointError};
pub use error::ExecError;
pub use launch::{Launch, class_path};
