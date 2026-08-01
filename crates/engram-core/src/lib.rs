//! engram-core: types and pure logic. No I/O.
//!
//! This crate is intentionally I/O free so it tests fast and is easy to
//! optimize via autoresearch loops.

pub mod error;
pub mod events;
pub mod fusion;
pub mod handle;
pub mod interval;
pub mod keys;
pub mod layers;
pub mod names;
pub mod relations;
pub mod temporal;
pub mod types;

pub use error::{CoreError, ErrorCode};
pub use events::EventKind;
pub use handle::{Handle, HandleKind};
pub use interval::{CloseAuthority, CloseReason, Interval};
pub use relations::{Arity, LegacyArity, RelationSpec};
pub use types::{Chunk, Edge, EdgeKind, Entity, Layer, Memory, MemoryId, Score};
