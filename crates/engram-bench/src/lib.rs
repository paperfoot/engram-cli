//! Benchmark harness.
//!
//! Primary target: LongMemEval (https://github.com/xiaowu0162/LongMemEval).
//! Same benchmark MemPalace uses, so numbers are directly comparable.

pub mod error;
pub mod longmemeval;
pub mod metrics;
pub mod mini;

pub use error::BenchError;
pub use metrics::{Metrics, Recall};
