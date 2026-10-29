//! Benchmark harness.
//!
//! Primary target: LongMemEval (https://github.com/xiaowu0162/LongMemEval).
//! The standard public benchmark for agent memory, so numbers compare with
//! published results.

pub mod beir;
pub mod biogen;
pub mod citation;
pub mod error;
pub mod judge;
pub mod litqa2;
pub mod locomo;
pub mod locomo_plus;
pub mod longmemeval;
pub mod memoryagentbench;
pub mod metrics;
pub mod mini;
pub mod personal;
pub mod qa;
pub mod ragas;

pub use error::BenchError;
pub use metrics::{Metrics, Recall};
