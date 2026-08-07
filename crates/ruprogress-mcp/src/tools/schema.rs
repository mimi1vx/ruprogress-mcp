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

/// Resolve every `{"$ref": "#/$defs/Name", …siblings}` node in `value`
/// against `defs`, in place. Sibling keys (e.g. `description`) override the
/// same key in the resolved def. Resolution recurses into the substituted
/// value, so a def that itself contains a `$ref` to another def (the only
/// nesting this server's schemas have — `ImportEntryParams` -> `ProjectRef`)
/// resolves too.
///
/// Every input schema's `$defs` are non-recursive (no cycles), so this
/// cannot loop forever; a `$ref` pointing anywhere other than `#/$defs/<name
/// present in defs>` is left untouched rather than silently mangled (`debug_assert`
/// catches it in tests/dev builds).
fn resolve_refs(value: &mut Value, defs: &serde_json::Map<String, Value>) {
    match value {
        Value::Object(map) => {
            if let Some(reference) = map.get("$ref").and_then(Value::as_str) {
                let target = reference
                    .strip_prefix("#/$defs/")
                    .and_then(|name| defs.get(name));
                let Some(target) = target else {
                    debug_assert!(
                        false,
                        "to_portable: $ref {reference:?} does not resolve against $defs"
                    );
                    return;
                };
                let mut resolved = target.clone();
                map.remove("$ref");
                if let Value::Object(resolved_map) = &mut resolved {
                    for (key, sibling_value) in std::mem::take(map) {
                        resolved_map.insert(key, sibling_value);
                    }
                }
                *value = resolved;
                resolve_refs(value, defs);
                return;
            }
            for v in map.values_mut() {
                resolve_refs(v, defs);
            }
        }
        Value::Array(items) => {
            for v in items {
                resolve_refs(v, defs);
            }
        }
        _ => {}
    }
}

/// Remove the root `$defs` object, inlining every reference to it throughout
/// `schema` (see [`resolve_refs`]). A no-op if `schema` has no `$defs`.
fn inline_defs(schema: &mut Value) {
    let Some(root) = schema.as_object_mut() else {
        return;
    };
    let Some(defs_value) = root.remove("$defs") else {
        return;
    };
    let Value::Object(defs) = defs_value else {
        debug_assert!(false, "to_portable: $defs is not a JSON object");
        root.insert("$defs".to_string(), defs_value);
        return;
    };
    resolve_refs(schema, &defs);
}

/// Rewrite `{"type": ["T", "null"]}` (exactly two elements, one of them
/// `"null"`) to `{"type": "T"}` throughout `schema`, dropping the portable
/// dialect's ability to represent an explicit-null field. Any other array
/// shape under `"type"` (there are none today) is left untouched.
fn collapse_nullable_types(schema: &mut Value) {
    match schema {
        Value::Object(map) => {
            if let Some(Value::Array(types)) = map.get("type")
                && let [a, b] = types.as_slice()
                && (a == "null") != (b == "null")
            {
                let non_null = if a == "null" { b.clone() } else { a.clone() };
                map.insert("type".to_string(), non_null);
            }
            for v in map.values_mut() {
                collapse_nullable_types(v);
            }
        }
        Value::Array(items) => {
            for v in items {
                collapse_nullable_types(v);
            }
        }
        _ => {}
    }
}

/// Convert a rich JSON Schema 2020-12 `inputSchema` (as normalized by
/// [`input`]) to the lossy, `Portable`-dialect form some clients require
/// (`REDMINE_MCP_SCHEMA_DIALECT=portable`; see `crate::config::SchemaDialect`
/// and `docs/adr/0007-json-schema-format-normalization.md`). Only inlines
/// `$ref`/`$defs` and collapses nullable `type` arrays — `additionalProperties`,
/// `default`, `enum`, `format`, `$schema`, and `anyOf` unions all survive.
pub(crate) fn to_portable(schema: &JsonObject) -> JsonObject {
    let mut value = Value::Object(schema.clone());
    inline_defs(&mut value);
    collapse_nullable_types(&mut value);
    match value {
        Value::Object(object) => object,
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

    // --- `to_portable` (stage 1) --------------------------------------

    /// A nested `$ref` (a `$def` — `Outer` — containing a `$ref` to another
    /// `$def` — `Inner`, matching `ImportEntryParams` -> `ProjectRef`)
    /// resolves fully: no `$ref`/`$defs` survive anywhere in the result.
    #[test]
    fn to_portable_inlines_a_nested_ref() {
        let schema = serde_json::json!({
            "type": "object",
            "$defs": {
                "Inner": {"type": "string", "description": "inner"},
                "Outer": {
                    "type": "object",
                    "properties": {"inner": {"$ref": "#/$defs/Inner"}}
                }
            },
            "properties": {
                "items": {"type": "array", "items": {"$ref": "#/$defs/Outer"}}
            }
        })
        .as_object()
        .unwrap()
        .clone();

        let portable = to_portable(&schema);
        let text = serde_json::to_string(&portable).unwrap();
        assert!(!text.contains("$ref"), "expected no $ref, got {text}");
        assert!(!text.contains("$defs"), "expected no $defs, got {text}");
        assert_eq!(
            portable["properties"]["items"]["items"]["properties"]["inner"]["type"],
            "string"
        );
    }

    /// A sibling key next to a `$ref` (e.g. a property-level `description`)
    /// overrides the same key on the resolved `$def`.
    #[test]
    fn to_portable_sibling_description_overrides_the_defs_own() {
        let schema = serde_json::json!({
            "type": "object",
            "$defs": {
                "Named": {"type": "string", "description": "the def's own description"}
            },
            "properties": {
                "name": {"$ref": "#/$defs/Named", "description": "the property's own description"}
            }
        })
        .as_object()
        .unwrap()
        .clone();

        let portable = to_portable(&schema);
        assert_eq!(
            portable["properties"]["name"]["description"],
            "the property's own description"
        );
        assert_eq!(portable["properties"]["name"]["type"], "string");
    }

    #[test]
    fn to_portable_collapses_a_two_element_nullable_type_array() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {"name": {"type": ["string", "null"]}}
        })
        .as_object()
        .unwrap()
        .clone();

        let portable = to_portable(&schema);
        assert_eq!(portable["properties"]["name"]["type"], "string");
    }

    /// Only an exactly-two-element array with one `"null"` member collapses;
    /// any other array shape under `"type"` is left alone.
    #[test]
    fn to_portable_leaves_a_non_nullable_type_array_alone() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {"value": {"type": ["string", "integer"]}}
        })
        .as_object()
        .unwrap()
        .clone();

        let portable = to_portable(&schema);
        assert_eq!(
            portable["properties"]["value"]["type"],
            serde_json::json!(["string", "integer"])
        );
    }

    /// Stage 1's deliberate limit: an untagged-union `$def` (`ProjectRef`'s
    /// shape) keeps its `anyOf` — union collapsing is stage 2, gated on
    /// whether stage 1 alone satisfies Vertex.
    #[test]
    fn to_portable_keeps_an_untagged_ref_anyof() {
        let schema = input::<RefParams>();
        let portable = to_portable(&schema);
        let text = serde_json::to_string(&portable).unwrap();
        assert!(!text.contains("$ref"), "expected no $ref, got {text}");
        assert!(!text.contains("$defs"), "expected no $defs, got {text}");
        assert!(
            text.contains("anyOf"),
            "expected the untagged union's anyOf to survive stage 1, got {text}"
        );
    }
}
