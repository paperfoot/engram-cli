//! Values the contract cannot know because they depend on this machine, or on
//! crates `engram-contract` deliberately does not depend on.
//!
//! All borrowed. Building one is allocation-free at the call site except for
//! the paths, and the fixture is machine-independent so a generated artefact
//! never carries somebody's home directory into the repository.

pub struct RuntimeContext<'a> {
    pub version: &'a str,
    pub config_path: &'a str,
    pub data_path: &'a str,
    pub vector_path: &'a str,
    pub cache_path: &'a str,
    pub answer_context_format: &'a str,
    pub extraction_model: &'a str,
    pub synthesis_model: &'a str,
}

impl RuntimeContext<'_> {
    /// Deterministic stand-in used by the generator and by tests: every path
    /// is a fixed sentinel, so a generated file diffs against itself on any
    /// machine.
    pub fn fixture() -> RuntimeContext<'static> {
        RuntimeContext {
            version: "0.0.0-fixture",
            config_path: "<config>/config.toml",
            data_path: "<data>/db.sqlite",
            vector_path: "<data>/vectors",
            cache_path: "<cache>",
            answer_context_format: "engram-result/2",
            extraction_model: "<extraction-model>",
            synthesis_model: "<synthesis-model>",
        }
    }
}
