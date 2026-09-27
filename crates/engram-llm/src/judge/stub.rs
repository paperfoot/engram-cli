//! Deterministic offline judge for tests and dry runs (`ENGRAM_JUDGE_STUB`).
//!
//! Sibling of [`crate::stub::ScriptedChatLlm`]. The spec is JSON:
//! - a number: every noul answers it, e.g. `0.9`;
//! - an object of rules: each key is a substring matched against a question's
//!   `instructions` serialized as JSON; the longest matching key wins and
//!   `"*"` is the fallback. A value is a number (a noul's P(yes), a score's
//!   level index) or a string (a choice's option name), e.g.
//!   `{"penicillin": 0.1, "same attribute": 0.95, "*": 0.8}`.
//!
//! A choice or score with no scripted value answers its first option or
//! lowest level. A noul with no matching rule and no `"*"` gets NO answer,
//! exactly like a provider that dropped one, so callers' missing-answer paths
//! are testable offline.

use super::{Answer, Judge, JudgeError, JudgeRequest, JudgeResponse, Question};
use async_trait::async_trait;
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Debug, thiserror::Error)]
#[error("invalid judge stub spec: {0}")]
pub struct StubSpecError(String);

#[derive(Debug, Clone)]
enum Rule {
    Number(f64),
    Option(String),
}

/// A scripted, offline [`Judge`].
#[derive(Debug, Clone)]
pub struct StubJudge {
    /// (substring, answer), longest substring first.
    rules: Vec<(String, Rule)>,
    default: Option<Rule>,
}

impl StubJudge {
    pub fn parse(spec: &str) -> Result<Self, StubSpecError> {
        let value: Value = serde_json::from_str(spec.trim())
            .map_err(|_| StubSpecError("expected a JSON number or object".into()))?;
        match value {
            Value::Number(_) => Ok(Self {
                rules: Vec::new(),
                default: Some(rule(&value, "the spec")?),
            }),
            Value::Object(map) => {
                let mut rules = Vec::new();
                let mut default = None;
                for (key, v) in &map {
                    let r = rule(v, key)?;
                    if key == "*" {
                        default = Some(r);
                    } else {
                        rules.push((key.clone(), r));
                    }
                }
                rules.sort_by_key(|(key, _)| std::cmp::Reverse(key.len()));
                Ok(Self { rules, default })
            }
            _ => Err(StubSpecError("expected a JSON number or object".into())),
        }
    }

    fn rule_for(&self, question: &Question) -> Option<&Rule> {
        let text = serde_json::to_string(question.instructions()).unwrap_or_default();
        self.rules
            .iter()
            .find(|(key, _)| text.contains(key.as_str()))
            .map(|(_, r)| r)
            .or(self.default.as_ref())
    }

    fn answer(&self, id: &str, question: &Question) -> Result<Option<Answer>, JudgeError> {
        let rule = self.rule_for(question);
        Ok(match question {
            Question::Noul { .. } => match rule {
                None => None,
                // A number rule also scripts score levels, so the spec
                // cannot be range-checked when parsed; a noul it answers must
                // be a probability, as the provider's must.
                Some(Rule::Number(p)) if *p <= 1.0 => Some(Answer::Noul(*p as f32)),
                Some(Rule::Number(_)) => {
                    return Err(JudgeError::InvalidResponse {
                        provider: "stub",
                        message: format!("the rule for noul `{id}` is not a probability in [0, 1]"),
                    })
                }
                Some(Rule::Option(_)) => {
                    return Err(JudgeError::InvalidResponse {
                        provider: "stub",
                        message: format!("the rule for noul `{id}` is not a number"),
                    })
                }
            },
            Question::Choice { options, .. } => {
                let scripted = match rule {
                    Some(Rule::Option(name)) => options.iter().position(|(o, _)| o == name),
                    _ => None,
                };
                let pick = scripted.unwrap_or(0);
                Some(Answer::Choice {
                    choice: options[pick].0.clone(),
                    confidence: 1.0,
                    probabilities: options
                        .iter()
                        .enumerate()
                        .map(|(i, (o, _))| (o.clone(), if i == pick { 1.0 } else { 0.0 }))
                        .collect::<BTreeMap<_, _>>(),
                })
            }
            Question::Score { levels, .. } => {
                let level = match rule {
                    Some(Rule::Number(n)) => (n.round().max(0.0) as usize).min(levels.len() - 1),
                    _ => 0,
                };
                Some(Answer::Score {
                    value: level as f32,
                    confidence: 1.0,
                    probabilities: (0..levels.len())
                        .map(|i| if i == level { 1.0 } else { 0.0 })
                        .collect(),
                })
            }
        })
    }
}

fn rule(value: &Value, key: &str) -> Result<Rule, StubSpecError> {
    match value {
        Value::Number(n) => {
            let n = n
                .as_f64()
                .filter(|n| n.is_finite() && *n >= 0.0)
                .ok_or_else(|| StubSpecError(format!("`{key}`: a number must be >= 0")))?;
            Ok(Rule::Number(n))
        }
        Value::String(s) => Ok(Rule::Option(s.clone())),
        _ => Err(StubSpecError(format!(
            "`{key}`: a rule is a number or an option name"
        ))),
    }
}

#[async_trait]
impl Judge for StubJudge {
    fn name(&self) -> &'static str {
        "stub"
    }

    fn model_id(&self) -> &str {
        "stub"
    }

    async fn ask(&self, request: &JudgeRequest) -> Result<JudgeResponse, JudgeError> {
        request.validate()?;
        let mut answers = BTreeMap::new();
        for (id, question) in &request.questions {
            if let Some(answer) = self.answer(id, question)? {
                answers.insert(id.clone(), answer);
            }
        }
        Ok(JudgeResponse {
            provider: "stub",
            model: "stub".to_string(),
            answers,
            input_tokens: 0,
            cost_usd: 0.0,
            latency_ms: 0,
            requests: 1,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::judge::questions::{self, FactTriple, PairAnswers};
    use serde_json::json;

    #[tokio::test]
    async fn a_number_answers_every_noul() {
        let judge = StubJudge::parse("0.9").unwrap();
        let req = questions::grounding_request(
            "passage",
            &[("a".into(), "x".into()), ("b".into(), "y".into())],
        );
        let resp = judge.ask(&req).await.unwrap();
        assert_eq!(resp.noul("a").unwrap(), 0.9);
        assert_eq!(resp.noul("b").unwrap(), 0.9);
        assert_eq!(resp.model, "stub");
        assert_eq!(resp.provider, "stub");
    }

    #[tokio::test]
    async fn rules_match_on_instructions_longest_key_first() {
        let judge = StubJudge::parse(
            r#"{"penicillin": 0.1, "not allergic to penicillin": 0.05, "*": 0.8}"#,
        )
        .unwrap();
        let req = questions::grounding_request(
            "passage",
            &[
                ("pen".into(), "Ana allergic to penicillin".into()),
                ("neg".into(), "Ana is not allergic to penicillin".into()),
                ("other".into(), "Ana lives in Lisbon".into()),
            ],
        );
        let resp = judge.ask(&req).await.unwrap();
        assert_eq!(resp.noul("pen").unwrap(), 0.1);
        assert_eq!(resp.noul("neg").unwrap(), 0.05);
        assert_eq!(resp.noul("other").unwrap(), 0.8);
    }

    #[tokio::test]
    async fn pair_signals_are_scriptable_by_question_text() {
        let judge = StubJudge::parse(
            r#"{"same attribute": 0.98, "contradict": 0.9, "stopped being true": 0.1, "*": 0.0}"#,
        )
        .unwrap();
        let req = questions::pair_request(
            "Ana moved to Berlin.",
            &FactTriple::new("Ana", "lives_in", "Berlin"),
            &[("f1".into(), FactTriple::new("Ana", "lives_in", "Lisbon"))],
        );
        let resp = judge.ask(&req).await.unwrap();
        let a = PairAnswers::from_response(&resp, "f1").unwrap();
        assert_eq!(
            (a.restates, a.same_slot, a.contradicts, a.old_ended),
            (0.0, 0.98, 0.9, 0.1)
        );
    }

    #[tokio::test]
    async fn a_noul_rule_above_one_is_an_invalid_response_not_a_clamp() {
        let judge = StubJudge::parse(r#"{"penicillin": 2, "*": 0.5}"#).unwrap();
        let req =
            questions::grounding_request("p", &[("x".into(), "Ana allergic to penicillin".into())]);
        assert!(matches!(
            judge.ask(&req).await,
            Err(JudgeError::InvalidResponse { .. })
        ));
        let req = questions::grounding_request("p", &[("y".into(), "Ana lives in Lisbon".into())]);
        assert_eq!(judge.ask(&req).await.unwrap().noul("y").unwrap(), 0.5);
    }

    #[tokio::test]
    async fn an_unscripted_noul_gets_no_answer() {
        let judge = StubJudge::parse(r#"{"penicillin": 0.1}"#).unwrap();
        let req = questions::grounding_request("p", &[("x".into(), "Ana lives in Lisbon".into())]);
        let resp = judge.ask(&req).await.unwrap();
        assert!(matches!(
            resp.noul("x"),
            Err(JudgeError::MissingAnswer { .. })
        ));
    }

    #[tokio::test]
    async fn choice_and_score_default_simple_unless_scripted() {
        let judge = StubJudge::parse(r#"{"design": "rct", "certainty": 2}"#).unwrap();
        let mut req = JudgeRequest::new(json!("s"));
        let options = |names: &[&str]| names.iter().map(|n| (n.to_string(), None)).collect();
        req.push(
            "design",
            Question::Choice {
                instructions: json!("What study design?"),
                options: options(&["review", "rct", "other"]),
            },
        );
        req.push(
            "organism",
            Question::Choice {
                instructions: json!("Which organism?"),
                options: options(&["humans", "animals"]),
            },
        );
        req.push(
            "certainty",
            Question::Score {
                instructions: json!("How much certainty?"),
                levels: vec![json!("low"), json!("mid"), json!("high")],
            },
        );
        req.push(
            "tone",
            Question::Score {
                instructions: json!("How formal?"),
                levels: vec![json!("casual"), json!("formal")],
            },
        );
        let resp = judge.ask(&req).await.unwrap();
        let choice = |id| match resp.answer(id).unwrap() {
            Answer::Choice { choice, .. } => choice.clone(),
            other => panic!("{other:?}"),
        };
        assert_eq!(choice("design"), "rct");
        assert_eq!(choice("organism"), "humans");
        let score = |id| match resp.answer(id).unwrap() {
            Answer::Score { value, .. } => *value,
            other => panic!("{other:?}"),
        };
        assert_eq!(score("certainty"), 2.0);
        assert_eq!(score("tone"), 0.0);
    }

    #[test]
    fn malformed_specs_are_rejected() {
        for bad in ["", "not json", "[0.5]", "true", r#"{"x": [1]}"#, "-0.5"] {
            assert!(StubJudge::parse(bad).is_err(), "{bad}");
        }
    }
}
