//! JSON envelope and TTY-aware output routing.

use serde::Serialize;
use serde_json::{json, Value};
use std::io::{IsTerminal, Write};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputFormat {
    Json,
    Human,
}

impl OutputFormat {
    pub fn detect(json_flag: bool) -> Self {
        if json_flag || !std::io::stdout().is_terminal() {
            OutputFormat::Json
        } else {
            OutputFormat::Human
        }
    }
}

/// Set once by `dispatch` when the command starts. `Metadata::default()`
/// reads it, so every command reports real elapsed time without each call
/// site carrying its own timer. Before this, only the commands that happened
/// to time themselves did — `edit` re-embedded over the network and reported
/// `elapsed_ms: 0`, which agents read as "nothing ran". Process-global (not
/// thread-local) because one process runs one command, and tokio's runtime
/// may finish the command on a different thread than the one that started it.
static COMMAND_START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();

/// Called by `dispatch` at command entry. First call wins; later calls
/// (nested dispatch in tests) keep the original start, which is the honest
/// outermost measurement.
pub fn mark_command_start() {
    let _ = COMMAND_START.set(std::time::Instant::now());
}

pub struct Metadata {
    pub elapsed_ms: u64,
    pub fields: serde_json::Map<String, Value>,
}

impl Default for Metadata {
    fn default() -> Self {
        let elapsed_ms = COMMAND_START
            .get()
            .map(|t| t.elapsed().as_millis() as u64)
            .unwrap_or(0);
        Metadata {
            elapsed_ms,
            fields: serde_json::Map::new(),
        }
    }
}

impl Metadata {
    pub fn add(&mut self, key: &str, value: impl Serialize) {
        if let Ok(v) = serde_json::to_value(value) {
            self.fields.insert(key.to_string(), v);
        }
    }

    /// Which spelling the caller used, when a command has aliases
    /// (`memory search` vs `recall`). Blessed by D2: an agent replaying a
    /// transcript needs to know which surface answered it.
    pub fn command_alias(&mut self, alias: &str) {
        self.add("command_alias", alias);
    }

    /// How many conflict rows carried a stored `suggested_action` that the
    /// live safety rule overrode (R7). Blessed by D2 and emitted only when
    /// non-zero, so a silent write-side regression cannot hide behind the
    /// read-side guard that corrects it.
    pub fn suggested_action_overridden(&mut self, n: usize) {
        if n > 0 {
            self.add("suggested_action_overridden", n);
        }
    }

    /// `{ elapsed_ms, ...fields }`. One definition, shared by the stdout
    /// envelope and `respond::success`, so the CLI and the daemon cannot grow
    /// two spellings of the same metadata block.
    pub fn into_value(self) -> Value {
        let mut map = serde_json::Map::new();
        map.insert("elapsed_ms".to_string(), json!(self.elapsed_ms));
        for (k, v) in self.fields {
            map.insert(k, v);
        }
        Value::Object(map)
    }
}

/// Listing counters, IN-23 / D2. `count` is written too and always equals
/// `returned`, because agents in the wild read it. `total` is the TRUE total
/// after scope filtering, not the page length — the field whose absence made
/// agents report 3 open conflicts when 953 were queued. `i64` because it comes
/// from a SQL `COUNT(*)` (D2, taking the consumer's side).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listing {
    pub returned: usize,
    pub total: i64,
    pub cursor: Option<String>,
}

impl Listing {
    /// Everything that matched is in this response: no page, no cursor.
    pub fn full(items: usize) -> Self {
        Listing {
            returned: items,
            total: items as i64,
            cursor: None,
        }
    }

    pub fn page(returned: usize, total: i64, cursor: Option<String>) -> Self {
        Listing {
            returned,
            total,
            cursor,
        }
    }

    /// Writes `returned`/`total`/`count`, and `cursor` only when there is one —
    /// a null cursor and an absent cursor mean the same thing, and emitting
    /// both spellings teaches agents to check two.
    pub fn attach(&self, obj: &mut serde_json::Map<String, Value>) {
        obj.insert("returned".to_string(), json!(self.returned));
        obj.insert("total".to_string(), json!(self.total));
        obj.insert("count".to_string(), json!(self.returned));
        if let Some(c) = &self.cursor {
            obj.insert("cursor".to_string(), json!(c));
        }
    }
}

/// `metadata.scope` — what the read actually looked at, IN-23 / D2. The
/// answer to "did you search the namespace I meant?", which was previously
/// unanswerable from a response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scope {
    pub kbs: Vec<String>,
    pub diary: String,
    /// `None` on surfaces that score no chunks; the KEY IS OMITTED, never 0.
    /// Reporting `chunks_considered: 0` from `facts list` is the same class of
    /// lie as a fabricated threshold.
    pub chunks_considered: Option<usize>,
    /// `KbScopeOrigin::as_str()` — `flag` | `config` | `all_kbs`. No second
    /// vocabulary.
    pub origin: &'static str,
    /// False where the surface has no diary column at all (entities, graph),
    /// so `diary: "*"` reads as "not applicable" rather than "searched all".
    pub diary_applicable: bool,
}

impl Scope {
    /// A retrieval surface: it scored chunks, so it says how many.
    pub fn retrieval(
        kbs: Vec<String>,
        diary: impl Into<String>,
        origin: &'static str,
        chunks_considered: usize,
    ) -> Self {
        Scope {
            kbs,
            diary: diary.into(),
            chunks_considered: Some(chunks_considered),
            origin,
            diary_applicable: true,
        }
    }

    /// An enumerating surface: no chunks were scored, so the key is absent.
    pub fn listing(kbs: Vec<String>, diary: impl Into<String>, origin: &'static str) -> Self {
        Scope {
            kbs,
            diary: diary.into(),
            chunks_considered: None,
            origin,
            diary_applicable: true,
        }
    }

    /// A surface with no diary column; `diary` is reported as `*`.
    pub fn undiaried(kbs: Vec<String>, origin: &'static str) -> Self {
        Scope {
            kbs,
            diary: "*".to_string(),
            chunks_considered: None,
            origin,
            diary_applicable: false,
        }
    }

    pub fn attach(&self, meta: &mut Metadata) {
        let mut obj = serde_json::Map::new();
        obj.insert("kbs".to_string(), json!(self.kbs));
        obj.insert("diary".to_string(), json!(self.diary));
        obj.insert("origin".to_string(), json!(self.origin));
        if let Some(c) = self.chunks_considered {
            obj.insert("chunks_considered".to_string(), json!(c));
        }
        if !self.diary_applicable {
            obj.insert("diary_applicable".to_string(), json!(false));
        }
        meta.add("scope", Value::Object(obj));
    }
}

/// A computed score or confidence, as published: four decimal places.
/// Scores are `f32`, and widening one to `f64` for `json!` prints
/// `0.9200000166893005` — fourteen digits of conversion noise around two of
/// signal, paid for by every reader and mistaken by some for precision.
pub fn round_score(x: impl Into<f64>) -> f64 {
    (x.into() * 1e4).round() / 1e4
}

fn serialize_score<S: serde::Serializer>(x: &f64, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_f64(round_score(*x))
}

/// A subject the store nearly matched. Reasoning-layer's near-miss list, so
/// "no such subject" can say which subjects do exist.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct NearestSubject {
    pub subject: String,
    #[serde(serialize_with = "serialize_score")]
    pub score: f64,
}

/// The absence certificate: proof that the store looked, how hard, and how
/// close it got. Without it, "no results" and "you searched the wrong
/// namespace" are the same response.
#[derive(Debug, Clone, PartialEq)]
pub struct Absence {
    pub top_score: Option<f32>,
    /// `None` until a real cutoff rejected a real candidate. Never a
    /// fabricated `0.0` — a threshold nobody applied is worse than no
    /// threshold, because an agent will tune against it.
    pub threshold: Option<f32>,
    pub searched_kbs: Vec<String>,
    pub searched_chunks: usize,
    /// `no_chunks_in_scope` | `filtered_by_window` | `filtered_by_diary` |
    /// `no_candidate_matched` | `no_candidate_above_cutoff` |
    /// `trimmed_by_layer_budget` | `subject_not_found`
    /// | `no_open_values`
    pub reason: &'static str,
    pub nearest_subjects: Vec<NearestSubject>,
}

impl Absence {
    /// The common case: nothing scored above the bar, and we can say what the
    /// bar and the best candidate were.
    pub fn new(reason: &'static str, searched_kbs: Vec<String>, searched_chunks: usize) -> Self {
        Absence {
            top_score: None,
            threshold: None,
            searched_kbs,
            searched_chunks,
            reason,
            nearest_subjects: Vec::new(),
        }
    }

    pub fn with_scores(mut self, top_score: Option<f32>, threshold: Option<f32>) -> Self {
        self.top_score = top_score;
        self.threshold = threshold;
        self
    }

    pub fn with_nearest(mut self, nearest: Vec<NearestSubject>) -> Self {
        self.nearest_subjects = nearest;
        self
    }

    fn to_value(&self) -> Value {
        let mut obj = serde_json::Map::new();
        if let Some(s) = self.top_score {
            obj.insert("top_score".to_string(), json!(round_score(s)));
        }
        if let Some(t) = self.threshold {
            obj.insert("threshold".to_string(), json!(round_score(t)));
        }
        obj.insert("searched_kbs".to_string(), json!(self.searched_kbs));
        obj.insert("searched_chunks".to_string(), json!(self.searched_chunks));
        obj.insert("reason".to_string(), json!(self.reason));
        if !self.nearest_subjects.is_empty() {
            obj.insert("nearest_subjects".to_string(), json!(self.nearest_subjects));
        }
        Value::Object(obj)
    }
}

/// `data.verdict` — the authoritative absence signal. `status: "no_results"`
/// stays as the legacy envelope-level shorthand.
#[derive(Debug, Clone, PartialEq)]
pub enum Verdict {
    Results,
    NoMatch(Absence),
}

impl Verdict {
    pub fn as_str(&self) -> &'static str {
        match self {
            Verdict::Results => "results",
            Verdict::NoMatch(_) => "no_match",
        }
    }

    /// Writes `verdict`, and `absence` only on `NoMatch` — an absence
    /// certificate beside results would be a certificate of nothing.
    pub fn attach(&self, obj: &mut serde_json::Map<String, Value>) {
        obj.insert("verdict".to_string(), json!(self.as_str()));
        if let Verdict::NoMatch(a) = self {
            obj.insert("absence".to_string(), a.to_value());
        }
    }
}

/// Print a success envelope to stdout. JSON or human, decided by `format`.
pub fn print_success<T: Serialize, F: FnOnce(&T)>(
    format: OutputFormat,
    data: T,
    metadata: Metadata,
    human: F,
) {
    print_envelope(format, "success", data, metadata, human)
}

/// Print a JSON envelope with an explicit status.
pub fn print_envelope<T: Serialize, F: FnOnce(&T)>(
    format: OutputFormat,
    status: &str,
    data: T,
    metadata: Metadata,
    human: F,
) {
    match format {
        OutputFormat::Json => {
            let envelope = json!({
                "version": "1",
                "status": status,
                "data": data,
                "metadata": metadata.into_value(),
            });
            let mut out = std::io::stdout().lock();
            let _ = writeln!(out, "{}", serde_json::to_string(&envelope).unwrap());
        }
        OutputFormat::Human => {
            human(&data);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obj() -> serde_json::Map<String, Value> {
        serde_json::Map::new()
    }

    #[test]
    fn listing_writes_the_legacy_count_alias() {
        let mut o = obj();
        Listing::page(3, 953, Some("c1".into())).attach(&mut o);
        assert_eq!(o["returned"], json!(3));
        assert_eq!(o["total"], json!(953));
        assert_eq!(o["count"], o["returned"]);
        assert_eq!(o["cursor"], json!("c1"));
    }

    #[test]
    fn a_full_listing_has_no_cursor_key() {
        let mut o = obj();
        Listing::full(2).attach(&mut o);
        assert!(!o.contains_key("cursor"));
        assert_eq!(o["total"], json!(2));
    }

    #[test]
    fn a_listing_scope_omits_chunks_considered() {
        // Reporting 0 chunks from a surface that scores none is a lie an
        // agent will tune retrieval against.
        let mut m = Metadata::default();
        Scope::listing(vec!["default".into()], "work", "flag").attach(&mut m);
        let scope = &m.fields["scope"];
        assert!(scope.get("chunks_considered").is_none());
        assert!(scope.get("diary_applicable").is_none());
    }

    #[test]
    fn an_undiaried_scope_says_so() {
        let mut m = Metadata::default();
        Scope::undiaried(vec!["*".into()], "all_kbs").attach(&mut m);
        assert_eq!(m.fields["scope"]["diary"], json!("*"));
        assert_eq!(m.fields["scope"]["diary_applicable"], json!(false));
    }

    #[test]
    fn absence_never_fabricates_a_threshold() {
        let mut o = obj();
        Verdict::NoMatch(Absence::new(
            "no_chunks_in_scope",
            vec!["default".into()],
            0,
        ))
        .attach(&mut o);
        assert_eq!(o["verdict"], json!("no_match"));
        assert!(o["absence"].get("threshold").is_none());
        assert!(o["absence"].get("top_score").is_none());
        assert_eq!(o["absence"]["reason"], json!("no_chunks_in_scope"));
    }

    #[test]
    fn results_carry_no_absence_certificate() {
        let mut o = obj();
        Verdict::Results.attach(&mut o);
        assert_eq!(o["verdict"], json!("results"));
        assert!(!o.contains_key("absence"));
    }

    #[test]
    fn overridden_count_is_omitted_when_nothing_was_overridden() {
        let mut m = Metadata::default();
        m.suggested_action_overridden(0);
        assert!(!m.fields.contains_key("suggested_action_overridden"));
        m.suggested_action_overridden(2);
        assert_eq!(m.fields["suggested_action_overridden"], json!(2));
    }

    #[test]
    fn metadata_value_always_carries_elapsed_ms() {
        let mut m = Metadata::default();
        m.command_alias("memory search");
        let v = m.into_value();
        assert!(v["elapsed_ms"].is_u64());
        assert_eq!(v["command_alias"], json!("memory search"));
    }
}
