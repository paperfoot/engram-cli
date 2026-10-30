//! Chat LLM providers for answer generation and judging, plus the calibrated
//! judge (`judge`): typed questions in, calibrated probabilities out.
//!
//! Currently supports OpenRouter which proxies 350+ frontier models behind
//! one OpenAI-compatible API. Use this for:
//!  - Generating answers from retrieved context (LongMemEval QA track)
//!  - Judging answer correctness vs gold (LLM-as-judge)
//!  - Computing RAGAS metrics (faithfulness, relevance, etc.)

pub mod anthropic;
pub mod error;
pub mod judge;
pub mod openrouter;
pub mod stub;

use async_trait::async_trait;
pub use error::LlmError;

#[derive(Debug, Clone)]
pub struct ChatMessage {
    pub role: String, // "system" | "user" | "assistant"
    pub content: String,
}

impl ChatMessage {
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: "system".into(),
            content: content.into(),
        }
    }
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: "user".into(),
            content: content.into(),
        }
    }
    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: "assistant".into(),
            content: content.into(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ChatResponse {
    pub content: String,
    pub prompt_tokens: Option<u32>,
    pub completion_tokens: Option<u32>,
    pub model: String,
}

#[async_trait]
pub trait ChatLlm: Send + Sync {
    fn name(&self) -> &'static str;
    fn model_id(&self) -> &str;
    async fn chat(&self, messages: &[ChatMessage]) -> Result<ChatResponse, LlmError>;
}
