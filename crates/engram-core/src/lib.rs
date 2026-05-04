//! engram-core: types and pure logic. No I/O.
//!
//! This crate is intentionally I/O free so it tests fast and is easy to
//! optimize via autoresearch loops.

pub mod error;
pub mod fusion;
pub mod layers;
pub mod temporal;
pub mod types;

pub use error::CoreError;
pub use types::{Chunk, Edge, EdgeKind, Entity, Layer, Memory, MemoryId, Score};
