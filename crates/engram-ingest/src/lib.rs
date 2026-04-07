//! Mining modes — convert source content into Memories + Chunks.
//!
//! Each mode is a separate module so autoresearch loops can target one
//! at a time without rebuilding the others.

pub mod chunker;
pub mod conversations;
pub mod error;
pub mod general;
pub mod papers;
pub mod repos;

pub use error::IngestError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Papers,
    Conversations,
    Repos,
    General,
    Auto,
}
