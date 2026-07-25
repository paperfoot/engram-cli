//! Deterministic offline chat LLM for tests, CI, and dry runs.
//!
//! Sibling of `engram_embed::stub::StubEmbedder`: it lets every code path that
//! would otherwise need a network LLM run offline and deterministically. The
//! script is a tiny substring router — the first rule whose key appears in the
//! concatenated user text wins, otherwise the default response is returned —
//! which is enough to drive fact extraction over several different memories in
//! one process without inventing a second extractor implementation.

use crate::{ChatLlm, ChatMessage, ChatResponse, LlmError};
use async_trait::async_trait;

/// A scripted, offline `ChatLlm`.
pub struct ScriptedChatLlm {
    /// (substring to match against the user text, response content).
    rules: Vec<(String, String)>,
    /// Returned when no rule matches.
    default: String,
    model: String,
}

#[derive(Debug, thiserror::Error)]
#[error("invalid chat script: {0}")]
pub struct ScriptError(String);

impl ScriptedChatLlm {
    /// Always answer with `response`.
    pub fn constant(response: impl Into<String>) -> Self {
        Self {
            rules: Vec::new(),
            default: response.into(),
            model: "stub-chat".to_string(),
        }
    }

    /// Parse a script.
    ///
    /// Accepted shapes:
    /// - any JSON array (`[{"subject": ...}]`) — used verbatim as the single
    ///   response, which is the common "extract these facts" case;
    /// - a JSON object of `{"<substring>": <response>}` rules, where the key
    ///   `"*"` (or `"default"`) is the fallback and each response is either a
    ///   raw string or a JSON value that is re-serialized;
    /// - anything else — used verbatim as the single response.
    pub fn parse(spec: &str) -> Result<Self, ScriptError> {
        let trimmed = spec.trim();
        if trimmed.is_empty() {
            return Err(ScriptError("script is empty".into()));
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) else {
            return Ok(Self::constant(trimmed));
        };
        let serde_json::Value::Object(map) = value else {
            return Ok(Self::constant(trimmed));
        };
        let mut rules = Vec::new();
        let mut default = String::from("[]");
        for (key, response) in map {
            let content = match response {
                serde_json::Value::String(s) => s,
                other => serde_json::to_string(&other)
                    .map_err(|e| ScriptError(format!("rule '{key}' is not serializable: {e}")))?,
            };
            if key == "*" || key == "default" {
                default = content;
            } else {
                rules.push((key, content));
            }
        }
        // Longest key first: a specific marker must win over a shorter one
        // that happens to be a prefix of it.
        rules.sort_by_key(|(key, _)| std::cmp::Reverse(key.len()));
        Ok(Self {
            rules,
            default,
            model: "stub-chat".to_string(),
        })
    }

    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = model.into();
        self
    }

    /// The response this script would give for `text`.
    pub fn response_for(&self, text: &str) -> &str {
        self.rules
            .iter()
            .find(|(key, _)| text.contains(key.as_str()))
            .map(|(_, response)| response.as_str())
            .unwrap_or(&self.default)
    }
}

#[async_trait]
impl ChatLlm for ScriptedChatLlm {
    fn name(&self) -> &'static str {
        "stub"
    }

    fn model_id(&self) -> &str {
        &self.model
    }

    async fn chat(&self, messages: &[ChatMessage]) -> Result<ChatResponse, LlmError> {
        let user_text: String = messages
            .iter()
            .filter(|m| m.role == "user")
            .map(|m| m.content.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        Ok(ChatResponse {
            content: self.response_for(&user_text).to_string(),
            prompt_tokens: None,
            completion_tokens: None,
            model: self.model.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn array_script_is_used_verbatim() {
        let llm = ScriptedChatLlm::parse(r#"[{"subject":"Ana"}]"#).unwrap();
        let resp = llm
            .chat(&[ChatMessage::user("anything at all")])
            .await
            .unwrap();
        assert_eq!(resp.content, r#"[{"subject":"Ana"}]"#);
    }

    #[tokio::test]
    async fn object_script_routes_on_substring() {
        let llm = ScriptedChatLlm::parse(
            r#"{"Lisbon": [{"subject":"Ana","predicate":"lives_in","object":"Lisbon"}],
                "*": []}"#,
        )
        .unwrap();
        let hit = llm
            .chat(&[ChatMessage::user("Ana moved to Lisbon last spring")])
            .await
            .unwrap();
        assert!(hit.content.contains("lives_in"));
        let miss = llm
            .chat(&[ChatMessage::user("unrelated content")])
            .await
            .unwrap();
        assert_eq!(miss.content, "[]");
    }

    #[test]
    fn longest_rule_wins() {
        let llm = ScriptedChatLlm::parse(r#"{"ab": "short", "abcd": "long", "*": "none"}"#).unwrap();
        assert_eq!(llm.response_for("xx abcd yy"), "long");
        assert_eq!(llm.response_for("xx abz yy"), "short");
        assert_eq!(llm.response_for("nothing"), "none");
    }

    #[test]
    fn default_is_empty_array_when_unspecified() {
        let llm = ScriptedChatLlm::parse(r#"{"x": "y"}"#).unwrap();
        assert_eq!(llm.response_for("no match here"), "[]");
    }

    #[test]
    fn empty_script_is_rejected() {
        assert!(ScriptedChatLlm::parse("   ").is_err());
    }

    #[test]
    fn non_json_script_is_verbatim() {
        let llm = ScriptedChatLlm::parse("not json at all").unwrap();
        assert_eq!(llm.response_for("whatever"), "not json at all");
    }
}
