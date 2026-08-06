//! Central JSON Schema post-processor for every tool's `inputSchema`/
//! `outputSchema`.
//!
//! `schemars` 1.2 emits `{"type":"integer","format":"uint64","minimum":0}`
//! for every `u64` field (`uint32` for `u32`). These `uint*` formats are
//! schemars/Rust-specific — not part of the JSON Schema `format` vocabulary,
//! nor the OpenAPI-derived subset `ajv-formats` recognizes. MCP clients that
//! compile schemas with Ajv in strict mode (e.g. opencode) log an "unknown
//! format" warning per field; see `docs/adr/0007-json-schema-format-normalization.md`.
//!
//! `format` is annotation-only for JSON Schema integers — `minimum: 0`
//! (already emitted by schemars alongside it) carries the actual constraint
//! — so stripping the format string loses no information. Rust field types
//! stay `u64`/`u32`.
//!
//! Every tool must build its schemas through [`output`]/[`input`] rather
//! than calling `rmcp::handler::server::tool::schema_for_output`/
//! `schema_for_input` directly; the allowlist test in
//! `tests/tools_basic.rs` enforces this over the whole served `tools/list`.

use std::any::Any;
use std::sync::Arc;

use rmcp::model::JsonObject;
use schemars::JsonSchema;
use serde_json::Value;

/// Non-standard integer `format` values schemars emits for Rust's unsigned
/// and 128-bit integer types. None of these are in JSON Schema's `format`
/// vocabulary or the `ajv-formats` set MCP clients ship.
const NON_STANDARD_INT_FORMATS: &[&str] = &[
    "uint", "uint8", "uint16", "uint32", "uint64", "uint128", "int128",
];

/// Recursively remove `"format"` entries whose value is one of
/// [`NON_STANDARD_INT_FORMATS`] from every object node in `value`. A blind
/// walk over every object/array node covers `$defs`, `properties`, `items`,
/// and `anyOf`/`oneOf`/`allOf` alike — including `ProjectRef`'s untagged
/// `anyOf`, without needing to special-case any of them.
fn strip_non_standard_formats(value: &mut Value) {
    match value {
        Value::Object(map) => {
            let strip_format = matches!(
                map.get("format").and_then(Value::as_str),
                Some(format) if NON_STANDARD_INT_FORMATS.contains(&format)
            );
            if strip_format {
                map.remove("format");
            }
            for v in map.values_mut() {
                strip_non_standard_formats(v);
            }
        }
        Value::Array(items) => {
            for v in items {
                strip_non_standard_formats(v);
            }
        }
        _ => {}
    }
}

/// Normalize a raw rmcp-generated schema object in place, per
/// [`strip_non_standard_formats`].
fn normalize(raw: &JsonObject) -> JsonObject {
    let mut value = Value::Object(raw.clone());
    strip_non_standard_formats(&mut value);
    match value {
        Value::Object(object) => object,
        // `strip_non_standard_formats` never changes a node's variant, only
        // removes map entries, so a `Value::Object` input stays one.
        _ => unreachable!("schema root is always a JSON object"),
    }
}

/// Build a tool's `outputSchema`, normalized to drop non-standard `uint*`
/// formats (see module docs).
pub(crate) fn output<T: JsonSchema + Any>() -> Arc<JsonObject> {
    let raw = rmcp::handler::server::tool::schema_for_output::<T>();
    Arc::new(normalize(&raw))
}

/// Build a tool's `inputSchema`, normalized to drop non-standard `uint*`
/// formats (see module docs).
///
/// # Panics
///
/// Panics if `T`'s schema does not have root type `"object"`, mirroring what
/// `#[tool]` generates by default when no `input_schema` attribute is given
/// (rmcp-macros `tool.rs:194`) — a caller passing a non-object `T` here is a
/// programmer error caught at router-construction time, not a runtime
/// condition.
#[allow(clippy::panic)]
pub(crate) fn input<T: JsonSchema + Any>() -> Arc<JsonObject> {
    let raw = rmcp::handler::server::tool::schema_for_input::<T>().unwrap_or_else(|e| {
        panic!(
            "Invalid input schema for `{}`: {e}",
            std::any::type_name::<T>()
        )
    });
    Arc::new(normalize(&raw))
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    // These structs exist to be fed through `schemars::JsonSchema`, not to
    // have their fields read back.
    dead_code
)]
mod tests {
    use chrono::{DateTime, Utc};
    use serde::{Deserialize, Serialize};

    use super::*;

    /// Nests a `u64` field behind a `Vec<T>` so it lands in `$defs` rather
    /// than inline, and carries a `DateTime<Utc>` field so `format:
    /// "date-time"` has something to prove untouched.
    #[derive(Debug, Serialize, JsonSchema)]
    struct Item {
        id: u64,
        created_on: DateTime<Utc>,
    }

    #[derive(Debug, Serialize, JsonSchema)]
    struct ItemsOutput {
        items: Vec<Item>,
    }

    /// An untagged `u64 | String` enum, matching `ProjectRef`'s shape.
    #[derive(Debug, Deserialize, JsonSchema)]
    #[serde(untagged)]
    enum Ref {
        Id(u64),
        Name(String),
    }

    #[derive(Debug, Deserialize, JsonSchema)]
    struct RefParams {
        r#ref: Ref,
    }

    /// Every `"format"` string found anywhere in `value`.
    fn all_formats(value: &Value, out: &mut Vec<String>) {
        match value {
            Value::Object(map) => {
                if let Some(Value::String(f)) = map.get("format") {
                    out.push(f.clone());
                }
                for v in map.values() {
                    all_formats(v, out);
                }
            }
            Value::Array(items) => {
                for v in items {
                    all_formats(v, out);
                }
            }
            _ => {}
        }
    }

    #[test]
    fn output_strips_uint_formats_nested_in_defs_but_keeps_date_time() {
        let schema = output::<ItemsOutput>();
        let value = Value::Object(schema.as_ref().clone());
        let mut formats = Vec::new();
        all_formats(&value, &mut formats);
        assert!(
            !formats
                .iter()
                .any(|f| NON_STANDARD_INT_FORMATS.contains(&f.as_str())),
            "expected no uint* format, got {formats:?}"
        );
        assert!(
            formats.iter().any(|f| f == "date-time"),
            "expected date-time format to survive, got {formats:?}"
        );

        // minimum: 0 still carries the constraint schemars derived from `u64`.
        let text = serde_json::to_string(&value).unwrap();
        assert!(
            text.contains("\"minimum\":0"),
            "expected minimum:0 to survive stripping, got {text}"
        );
    }

    #[test]
    fn input_strips_uint_format_inside_an_untagged_any_of_enum() {
        let schema = input::<RefParams>();
        let value = Value::Object(schema.as_ref().clone());
        let mut formats = Vec::new();
        all_formats(&value, &mut formats);
        assert!(
            !formats
                .iter()
                .any(|f| NON_STANDARD_INT_FORMATS.contains(&f.as_str())),
            "expected no uint* format inside anyOf, got {formats:?}"
        );
    }

    #[test]
    fn strip_non_standard_formats_leaves_a_string_value_named_uint64_alone() {
        // A blind walk removes `"format"` *keys*, never touches string
        // *values* — an enum variant literally spelled "uint64" (none exist
        // today) must survive untouched.
        let mut value = serde_json::json!({"kind": "uint64", "nested": {"format": "uint64"}});
        strip_non_standard_formats(&mut value);
        assert_eq!(value["kind"], "uint64");
        assert!(value["nested"].get("format").is_none());
    }
}
