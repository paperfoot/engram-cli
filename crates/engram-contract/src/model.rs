//! The schema-as-data types. Every one of them is `&'static` data, because
//! the contract is a constant of the binary, not a thing assembled at runtime.
//!
//! The rule this crate defends: `engram-contract` describes shapes as `Ty`, it
//! never imports a shape. The first dependency on `engram-core` would make it
//! a second definition of the types it exists to describe.

use serde::Serialize;

/// The type of a flag, an argument, or an output field, as an agent sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ty {
    Str,
    Bool,
    Int,
    Uuid,
    Rfc3339,
    Float,
    Enum(&'static [&'static str]),
    Array(&'static Ty),
    Object,
    Nullable(&'static Ty),
}

impl Ty {
    /// The one-word spelling the manifest publishes.
    pub fn name(&self) -> &'static str {
        match self {
            Ty::Str => "string",
            Ty::Bool => "boolean",
            Ty::Int => "integer",
            Ty::Uuid => "uuid",
            Ty::Rfc3339 => "rfc3339",
            Ty::Float => "number",
            Ty::Enum(_) => "enum",
            Ty::Array(_) => "array",
            Ty::Object => "object",
            Ty::Nullable(_) => "nullable",
        }
    }

    /// The rendered form: `enum(a|b|c)`, `array<string>`, `string|null`.
    pub fn render(&self) -> String {
        match self {
            Ty::Enum(vs) => format!("enum({})", vs.join("|")),
            Ty::Array(inner) => format!("array<{}>", inner.render()),
            Ty::Nullable(inner) => format!("{}|null", inner.render()),
            other => other.name().to_string(),
        }
    }
}

impl Serialize for Ty {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.render())
    }
}

/// One flag as the agent sees it. `name` is the canonical long form WITHOUT
/// dashes and in snake_case — the JSON and daemon spelling. `cli_long` is what
/// clap registers; they differ only in `-` vs `_`, and a contract test proves
/// it, because one vocabulary across the CLI and the HTTP body is the whole
/// point of writing them down twice.
#[derive(Debug, Clone, Serialize)]
pub struct FlagSpec {
    pub name: &'static str,
    pub cli_long: &'static str,
    pub aliases: &'static [&'static str],
    pub ty: Ty,
    pub default: Option<&'static str>,
    pub required: bool,
    pub doc: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub struct ArgSpec {
    pub name: &'static str,
    pub ty: Ty,
    pub required: bool,
    pub doc: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub struct FieldSpec {
    pub name: &'static str,
    pub ty: Ty,
    pub doc: &'static str,
    /// False when the field is present only in some responses
    /// (`embedder_degraded`, `temporal_scope`, `absence`). A field documented
    /// as always-present that is sometimes absent is worse than an undocumented
    /// one: an agent will index into it.
    pub always: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct CommandSpec {
    /// `"recall"`, `"facts resolve"`, `"memory add"`.
    pub path: &'static str,
    pub aliases: &'static [&'static str],
    pub summary: &'static str,
    pub destructive: bool,
    pub requires: &'static [&'static str],
    /// False for `agent-info`, `--help` and clap failures: the discovery
    /// surface must answer without creating or migrating a store.
    pub opens_store: bool,
    pub args: &'static [ArgSpec],
    pub flags: &'static [FlagSpec],
    pub data: &'static [FieldSpec],
    pub metadata: &'static [FieldSpec],
    pub statuses: &'static [&'static str],
    pub exit_codes: &'static [u8],
    pub rules: &'static [&'static str],
    pub examples: &'static [&'static str],
}

impl CommandSpec {
    pub fn flag(&self, name: &str) -> Option<&'static FlagSpec> {
        self.flags.iter().find(|f| f.name == name)
    }
}

/// A command the binary has and the contract deliberately does not describe:
/// a surface whose removal is under review, where transcribing its prose would
/// polish something that may go. Discovery says so — `schema_available: false`
/// and a usage line — rather than letting an agent learn it from an exit 3.
#[derive(Debug, Clone, Serialize)]
pub struct UnavailableSpec {
    pub path: &'static str,
    pub summary: &'static str,
    pub usage: &'static str,
    pub destructive: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct EnvVarSpec {
    pub name: &'static str,
    pub doc: &'static str,
    pub implemented: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ErrorCodeSpec {
    pub code: &'static str,
    pub exit: u8,
    pub retryable: bool,
    pub suggestion: &'static str,
    pub doc: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub struct EnvelopeSpec {
    pub version: &'static str,
    pub success_shape: &'static str,
    pub error_shape: &'static str,
    pub listing_fields: &'static [FieldSpec],
    pub scope_fields: &'static [FieldSpec],
    pub verdict_values: &'static [&'static str],
    pub absence_fields: &'static [FieldSpec],
}

#[derive(Debug, Clone, Serialize)]
pub struct Contract {
    pub contract_version: &'static str,
    /// The store schema this binary expects. Derived from the storage crate's
    /// constant by the caller, never retyped here.
    pub schema_version: u32,
    pub envelope: EnvelopeSpec,
    pub commands: &'static [CommandSpec],
    pub schema_unavailable: &'static [UnavailableSpec],
    pub global_flags: &'static [FlagSpec],
    pub exit_codes: &'static [(u8, &'static str)],
    pub error_codes: &'static [ErrorCodeSpec],
    pub vocabularies: &'static [(&'static str, &'static [&'static str])],
    pub env_vars: &'static [EnvVarSpec],
    pub invariants: &'static [&'static str],
}
