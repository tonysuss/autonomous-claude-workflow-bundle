use std::collections::HashMap;

use jsonschema::{Registry, Validator};
use serde_json::Value;

use crate::{COMMON_SCHEMA, RecordKind, SCHEMA_BASE};

#[derive(Debug)]
pub enum SchemaError {
    /// A shipped schema file failed to load. This is a build defect.
    Load(String),
    /// A record does not match its schema.
    Invalid { kind: &'static str, errors: Vec<String> },
}

impl std::fmt::Display for SchemaError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SchemaError::Load(msg) => write!(f, "schema failed to load: {msg}"),
            SchemaError::Invalid { kind, errors } => {
                write!(f, "{kind} record does not match its schema: {}", errors.join("; "))
            }
        }
    }
}

impl std::error::Error for SchemaError {}

/// One compiled validator per record kind. Every reference resolves locally.
pub struct Validators {
    by_kind: HashMap<RecordKind, Validator>,
}

impl Validators {
    pub fn new() -> Result<Self, SchemaError> {
        let parse = |src: &str| -> Result<Value, SchemaError> {
            serde_json::from_str(src).map_err(|e| SchemaError::Load(e.to_string()))
        };
        let mut builder = Registry::new()
            .add(format!("{SCHEMA_BASE}common.schema.json"), parse(COMMON_SCHEMA)?)
            .map_err(|e| SchemaError::Load(e.to_string()))?;
        for kind in RecordKind::ALL {
            builder = builder
                .add(kind.schema_uri(), parse(kind.schema_source())?)
                .map_err(|e| SchemaError::Load(e.to_string()))?;
        }
        let registry = builder.prepare().map_err(|e| SchemaError::Load(e.to_string()))?;

        let mut by_kind = HashMap::new();
        for kind in RecordKind::ALL {
            let root = serde_json::json!({ "$ref": kind.schema_uri() });
            let validator = jsonschema::options()
                .offline()
                .should_validate_formats(true)
                .with_registry(&registry)
                .build(&root)
                .map_err(|e| SchemaError::Load(format!("{}: {e}", kind.name())))?;
            by_kind.insert(kind, validator);
        }
        Ok(Validators { by_kind })
    }

    pub fn validate(&self, kind: RecordKind, value: &Value) -> Result<(), SchemaError> {
        let validator = &self.by_kind[&kind];
        let errors: Vec<String> =
            validator.iter_errors(value).map(|e| format!("{} at {}", e, e.instance_path())).collect();
        if errors.is_empty() { Ok(()) } else { Err(SchemaError::Invalid { kind: kind.name(), errors }) }
    }

    /// Serializes a record and validates it.
    pub fn check<T: serde::Serialize>(&self, kind: RecordKind, record: &T) -> Result<(), SchemaError> {
        let value = serde_json::to_value(record)
            .map_err(|e| SchemaError::Invalid { kind: kind.name(), errors: vec![e.to_string()] })?;
        self.validate(kind, &value)
    }
}
