//! Record types for interlock.
//!
//! The JSON Schema files under `interlock/schemas` are the source of truth. The
//! types here are written by hand for now; `tests/conformance.rs` validates
//! every serialized record against its schema so the two cannot drift silently.

mod records;
#[cfg(feature = "validate")]
mod validate;

pub use records::*;
#[cfg(feature = "validate")]
pub use validate::{SchemaError, Validators};

/// The kinds of record defined by a schema file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RecordKind {
    Task,
    Criterion,
    Attempt,
    Result,
    Claim,
    Assessment,
    Event,
    Grant,
    Operation,
    CheckRun,
}

impl RecordKind {
    pub const ALL: [RecordKind; 10] = [
        RecordKind::Task,
        RecordKind::Criterion,
        RecordKind::Attempt,
        RecordKind::Result,
        RecordKind::Claim,
        RecordKind::Assessment,
        RecordKind::Event,
        RecordKind::Grant,
        RecordKind::Operation,
        RecordKind::CheckRun,
    ];

    pub fn name(self) -> &'static str {
        match self {
            RecordKind::Task => "task",
            RecordKind::Criterion => "criterion",
            RecordKind::Attempt => "attempt",
            RecordKind::Result => "result",
            RecordKind::Claim => "claim",
            RecordKind::Assessment => "assessment",
            RecordKind::Event => "event",
            RecordKind::Grant => "grant",
            RecordKind::Operation => "operation",
            RecordKind::CheckRun => "check_run",
        }
    }

    pub fn parse(name: &str) -> Option<RecordKind> {
        RecordKind::ALL.into_iter().find(|k| k.name() == name)
    }

    pub fn schema_uri(self) -> String {
        format!("{SCHEMA_BASE}{}.schema.json", self.name())
    }

    /// The schema file's contents, as shipped in this build.
    pub fn schema_source(self) -> &'static str {
        match self {
            RecordKind::Task => include_str!("../../../schemas/task.schema.json"),
            RecordKind::Criterion => include_str!("../../../schemas/criterion.schema.json"),
            RecordKind::Attempt => include_str!("../../../schemas/attempt.schema.json"),
            RecordKind::Result => include_str!("../../../schemas/result.schema.json"),
            RecordKind::Claim => include_str!("../../../schemas/claim.schema.json"),
            RecordKind::Assessment => include_str!("../../../schemas/assessment.schema.json"),
            RecordKind::Event => include_str!("../../../schemas/event.schema.json"),
            RecordKind::Grant => include_str!("../../../schemas/grant.schema.json"),
            RecordKind::Operation => include_str!("../../../schemas/operation.schema.json"),
            RecordKind::CheckRun => include_str!("../../../schemas/check_run.schema.json"),
        }
    }
}

pub const SCHEMA_BASE: &str = "https://schemas.interlock.dev/v1/";
pub const COMMON_SCHEMA: &str = include_str!("../../../schemas/common.schema.json");
