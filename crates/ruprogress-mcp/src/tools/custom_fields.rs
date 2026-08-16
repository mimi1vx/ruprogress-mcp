//! Shared write-side custom-field shapes.
//!
//! `CustomFieldValueInput`/`CustomFieldEntry`/`custom_field_entries_to_write`
//! were first needed by `manage_product` (moved here from
//! `tools::plugins::products`, F14): values are accepted by id only, since
//! there is no discovery tool to resolve a name against for products,
//! contacts, or DMSF documents. `manage_contact` and `manage_document` reuse
//! the same shape.
//!
//! Issues are different: `create_redmine_issue`/`update_redmine_issue` can
//! resolve a custom field by *name* via the project's
//! `include=issue_custom_fields`, and `null` needs to mean "clear this
//! field" rather than "field absent". Widening the shared type above to
//! carry an optional `id` and a nullable value would leak that semantics
//! into three shipped tools that cannot use either (F15) — so
//! `IssueCustomFieldEntry`/`IssueCustomFieldValueInput` are their own,
//! smaller types, deliberately not merged with the ones above.

use redmine_client::model::custom_field::{
    CustomFieldDefinition, CustomFieldValue, CustomFieldWrite,
};
use redmine_client::model::project::ProjectInclude;
use redmine_client::{IssueId, ProjectId, ProjectIdent, Scoped};
use rmcp::ErrorData as McpError;
use rmcp::model::CallToolResult;
use schemars::JsonSchema;
use serde::Deserialize;

use crate::error::to_tool_error;

// --- shared write-side shape (products/contacts/dmsf): id-only, no null ---

/// A custom field value as given by the caller: a single string, or an
/// array of strings for a `multiple = true` field. Mirrors how Redmine
/// itself represents the value on the wire (see
/// `redmine_client::model::custom_field::CustomFieldValue`'s doc comment);
/// unlike that type this is a tool-input shape the caller controls
/// directly, so `#[serde(untagged)]` is the right tool here (same pattern
/// as `tools::discovery::ProjectRef`).
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(untagged)]
pub(crate) enum CustomFieldValueInput {
    Single(String),
    Multiple(Vec<String>),
}

impl From<CustomFieldValueInput> for CustomFieldValue {
    fn from(v: CustomFieldValueInput) -> Self {
        match v {
            CustomFieldValueInput::Single(s) => Self::Single(Some(s)),
            CustomFieldValueInput::Multiple(values) => Self::Multiple(values),
        }
    }
}

/// One entry of a write-side `custom_fields` array (products, contacts,
/// DMSF).
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct CustomFieldEntry {
    /// The custom field's id. Values are accepted by id only.
    pub(crate) id: u64,
    /// The value to set.
    pub(crate) value: CustomFieldValueInput,
}

/// Shared by `tools::plugins::products` and `tools::plugins::dmsf`: the
/// same `{id, value}` write-side shape, spelled `custom_fields` on
/// products'/contacts' wire but `custom_field_values` on DMSF's (trap 3) —
/// the field name difference is each call site's own envelope, not this
/// conversion.
pub(crate) fn custom_field_entries_to_write(
    entries: Vec<CustomFieldEntry>,
) -> Vec<CustomFieldWrite> {
    entries
        .into_iter()
        .map(|e| CustomFieldWrite {
            id: e.id,
            value: e.value.into(),
        })
        .collect()
}

// --- issue-side shape: id-or-name, nullable value (F1/F2/F15) ---

/// A custom field value as given by the caller on an issue tool: a single
/// string (or `null` to clear the field), or an array of strings for a
/// `multiple = true` field. Unlike [`CustomFieldValueInput`], `null` is
/// meaningful here — issues have no "existing set" question the way a
/// fresh product/contact does, since `update_redmine_issue` can genuinely
/// clear a previously-set value.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(untagged)]
pub(crate) enum IssueCustomFieldValueInput {
    Single(Option<String>),
    Multiple(Vec<String>),
}

impl From<IssueCustomFieldValueInput> for CustomFieldValue {
    fn from(v: IssueCustomFieldValueInput) -> Self {
        match v {
            IssueCustomFieldValueInput::Single(s) => Self::Single(s),
            IssueCustomFieldValueInput::Multiple(values) => Self::Multiple(values),
        }
    }
}

/// One entry of an issue's `custom_fields` array: exactly one of `id`/`name`
/// must be given (F1, validated in [`resolve_entries`] rather than typed as
/// a union, so the schema stays a plain object).
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct IssueCustomFieldEntry {
    /// The custom field's id. Free — costs no extra request. Exactly one of
    /// `id`/`name` must be given.
    #[serde(default)]
    pub(crate) id: Option<u64>,
    /// The custom field's display name, matched case- and
    /// punctuation-insensitively (e.g. `"Story Points"` matches
    /// `"story_points"`). Costs one extra project lookup per call (shared
    /// across every `name` entry in the same call). Exactly one of
    /// `id`/`name` must be given.
    #[serde(default)]
    pub(crate) name: Option<String>,
    /// The value to set. `null` clears the field.
    pub(crate) value: IssueCustomFieldValueInput,
}

/// Lowercase and strip every non-alphanumeric character, so `"Story
/// Points"`, `"story_points"` and `"storypoints"` all normalize equal
/// (reference parity, F4).
pub(crate) fn normalize_field_name(name: &str) -> String {
    name.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// Validates exactly-one-of `id`/`name` (F1) on every entry — pure, and
/// deliberately called before any definitions fetch is even considered, so
/// a malformed entry costs zero requests even when it also happens to set
/// `name` (which would otherwise look like it needs a fetch).
pub(crate) fn validate_entry_shapes(entries: &[IssueCustomFieldEntry]) -> Result<(), McpError> {
    for entry in entries {
        match (&entry.id, &entry.name) {
            (Some(_), Some(_)) => {
                return Err(McpError::invalid_params(
                    "each custom_fields entry must give exactly one of id or name, not both",
                    None,
                ));
            }
            (None, None) => {
                return Err(McpError::invalid_params(
                    "each custom_fields entry must give exactly one of id or name",
                    None,
                ));
            }
            (Some(_), None) | (None, Some(_)) => {}
        }
    }
    Ok(())
}

/// Resolves a caller-supplied `custom_fields` array against a project's
/// issue custom field definitions, rejecting unknown (F5) and ambiguous
/// (F4) names, and rejecting two entries that address the same field (F17,
/// checked after resolution so an `id` entry colliding with a resolved
/// `name` entry is caught too). Assumes [`validate_entry_shapes`] already
/// passed — callers that skip it get an internal-error message from the
/// unreachable both/neither arms instead of a real diagnosis.
///
/// `defs` is `None` when no entry uses `name` — the caller then skips the
/// definitions lookup entirely (F6) and this function never needs a
/// definition to resolve an `id`-only entry.
pub(crate) fn resolve_entries(
    defs: Option<&[CustomFieldDefinition]>,
    entries: Vec<IssueCustomFieldEntry>,
) -> Result<Vec<CustomFieldWrite>, McpError> {
    validate_entry_shapes(&entries)?;

    let mut out = Vec::with_capacity(entries.len());
    let mut seen_ids: Vec<u64> = Vec::with_capacity(entries.len());

    for entry in entries {
        let id = match (entry.id, entry.name) {
            (Some(id), _) => id,
            (None, Some(name)) => resolve_name(
                defs.ok_or_else(|| {
                    McpError::invalid_params(
                        "internal error: a name entry requires definitions to be fetched first",
                        None,
                    )
                })?,
                &name,
            )?,
            (None, None) => unreachable!("validate_entry_shapes already rejected this"),
        };

        if seen_ids.contains(&id) {
            return Err(McpError::invalid_params(
                format!("custom_fields has more than one entry for field id {id}"),
                None,
            ));
        }
        seen_ids.push(id);

        out.push(CustomFieldWrite {
            id,
            value: entry.value.into(),
        });
    }

    Ok(out)
}

fn resolve_name(defs: &[CustomFieldDefinition], name: &str) -> Result<u64, McpError> {
    let normalized = normalize_field_name(name);
    let matches: Vec<&CustomFieldDefinition> = defs
        .iter()
        .filter(|d| normalize_field_name(&d.name) == normalized)
        .collect();
    match matches.as_slice() {
        [] => Err(McpError::invalid_params(
            format!(
                "no issue custom field named {name:?} on this project; use \
                 list_project_issue_custom_fields to see the available fields, or address it \
                 by id"
            ),
            None,
        )),
        [single] => Ok(single.id),
        multiple => {
            let ids: Vec<String> = multiple.iter().map(|d| d.id.to_string()).collect();
            Err(McpError::invalid_params(
                format!(
                    "custom field name {name:?} is ambiguous on this project (ids {}); \
                     address it by id instead",
                    ids.join(", ")
                ),
                None,
            ))
        }
    }
}

/// Whether any entry in `entries` addresses its field by `name`, meaning
/// the caller must fetch the project's `issue_custom_fields` definitions
/// before resolving (F6).
pub(crate) fn needs_definitions(entries: &[IssueCustomFieldEntry]) -> bool {
    entries.iter().any(|e| e.name.is_some())
}

/// A failure while resolving an issue's `custom_fields`: either a
/// caller-input problem worth surfacing as a protocol-level error (an
/// unresolvable/ambiguous/duplicate entry), or a transport/permission
/// failure on the definitions lookup, reported in-band via the same
/// envelope every other Redmine error uses (F21).
pub(crate) enum IssueCustomFieldsOutcome {
    Protocol(McpError),
    InBand(CallToolResult),
}

/// Resolves a `custom_fields` parameter end to end: `None`/`Some(vec![])`
/// short-circuit to `Ok(None)` with no request at all (F18); an all-`id`
/// array resolves with no request (F6); an array with at least one `name`
/// entry fetches `project`'s `issue_custom_fields` definitions first (F21:
/// any error from that fetch aborts here, before any write).
pub(crate) async fn resolve_issue_custom_fields(
    scoped: &Scoped<'_>,
    project_id: &ProjectIdent,
    entries: Option<Vec<IssueCustomFieldEntry>>,
) -> Result<Option<Vec<CustomFieldWrite>>, IssueCustomFieldsOutcome> {
    let Some(entries) = entries else {
        return Ok(None);
    };
    if entries.is_empty() {
        return Ok(None);
    }
    validate_entry_shapes(&entries).map_err(IssueCustomFieldsOutcome::Protocol)?;
    let defs = if needs_definitions(&entries) {
        match scoped
            .get_project(project_id, &[ProjectInclude::IssueCustomFields])
            .await
        {
            Ok(project) => Some(project.issue_custom_fields.unwrap_or_default()),
            Err(e) => return Err(IssueCustomFieldsOutcome::InBand(to_tool_error(e))),
        }
    } else {
        None
    };
    resolve_entries(defs.as_deref(), entries)
        .map(Some)
        .map_err(IssueCustomFieldsOutcome::Protocol)
}

/// `update_redmine_issue`'s variant of [`resolve_issue_custom_fields`]:
/// its parameters carry only `issue_id`, not a project, so a `name` entry
/// costs a dedicated `GET /issues/{id}.json` first to learn the project —
/// on top of the project lookup itself (F16, two extra reads total). An
/// all-`id` array still costs nothing: [`resolve_entries`] is called
/// directly, without ever fetching the issue.
pub(crate) async fn resolve_issue_custom_fields_for_update(
    scoped: &Scoped<'_>,
    issue_id: IssueId,
    entries: Option<Vec<IssueCustomFieldEntry>>,
) -> Result<Option<Vec<CustomFieldWrite>>, IssueCustomFieldsOutcome> {
    let Some(entries) = entries else {
        return Ok(None);
    };
    if entries.is_empty() {
        return Ok(None);
    }
    validate_entry_shapes(&entries).map_err(IssueCustomFieldsOutcome::Protocol)?;
    if !needs_definitions(&entries) {
        return resolve_entries(None, entries)
            .map(Some)
            .map_err(IssueCustomFieldsOutcome::Protocol);
    }
    let issue = match scoped.get_issue(issue_id, &[]).await {
        Ok(issue) => issue,
        Err(e) => return Err(IssueCustomFieldsOutcome::InBand(to_tool_error(e))),
    };
    let project_id = ProjectIdent::Id(ProjectId(issue.project.id));
    resolve_issue_custom_fields(scoped, &project_id, Some(entries)).await
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]
mod tests {
    use super::*;

    fn def(id: u64, name: &str) -> CustomFieldDefinition {
        serde_json::from_value(serde_json::json!({
            "id": id, "name": name, "field_format": "string"
        }))
        .unwrap()
    }

    fn entry_by_id(id: u64, value: &str) -> IssueCustomFieldEntry {
        IssueCustomFieldEntry {
            id: Some(id),
            name: None,
            value: IssueCustomFieldValueInput::Single(Some(value.to_string())),
        }
    }

    fn entry_by_name(name: &str, value: &str) -> IssueCustomFieldEntry {
        IssueCustomFieldEntry {
            id: None,
            name: Some(name.to_string()),
            value: IssueCustomFieldValueInput::Single(Some(value.to_string())),
        }
    }

    #[test]
    fn normalizes_case_and_punctuation() {
        assert_eq!(normalize_field_name("Story Points"), "storypoints");
        assert_eq!(normalize_field_name("story_points"), "storypoints");
        assert_eq!(normalize_field_name("storypoints"), "storypoints");
    }

    #[test]
    fn neither_id_nor_name_is_rejected() {
        let entry = IssueCustomFieldEntry {
            id: None,
            name: None,
            value: IssueCustomFieldValueInput::Single(Some("x".to_string())),
        };
        let err = resolve_entries(None, vec![entry]).unwrap_err();
        assert!(err.message.contains("exactly one of id or name"));
    }

    #[test]
    fn both_id_and_name_is_rejected() {
        let entry = IssueCustomFieldEntry {
            id: Some(1),
            name: Some("Severity".to_string()),
            value: IssueCustomFieldValueInput::Single(Some("x".to_string())),
        };
        let err = resolve_entries(None, vec![entry]).unwrap_err();
        assert!(err.message.contains("not both"));
    }

    #[test]
    fn all_id_entries_need_no_definitions() {
        let entries = vec![entry_by_id(1, "a"), entry_by_id(2, "b")];
        assert!(!needs_definitions(&entries));
        let out = resolve_entries(None, entries).unwrap();
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].id, 1);
    }

    #[test]
    fn unknown_name_is_rejected_pointing_at_the_discovery_tool() {
        let defs = vec![def(1, "Severity")];
        let entries = vec![entry_by_name("Nonexistent", "x")];
        assert!(needs_definitions(&entries));
        let err = resolve_entries(Some(&defs), entries).unwrap_err();
        assert!(err.message.contains("list_project_issue_custom_fields"));
    }

    #[test]
    fn ambiguous_name_names_both_ids() {
        let defs = vec![def(1, "Story Points"), def(2, "story_points")];
        let entries = vec![entry_by_name("StoryPoints", "5")];
        let err = resolve_entries(Some(&defs), entries).unwrap_err();
        assert!(err.message.contains('1'));
        assert!(err.message.contains('2'));
        assert!(err.message.contains("ambiguous"));
    }

    #[test]
    fn name_matches_case_and_punctuation_insensitively() {
        let defs = vec![def(9, "Story Points")];
        let entries = vec![entry_by_name("story_points", "5")];
        let out = resolve_entries(Some(&defs), entries).unwrap();
        assert_eq!(out[0].id, 9);
    }

    #[test]
    fn duplicate_ids_across_an_id_entry_and_a_resolved_name_entry_are_rejected() {
        let defs = vec![def(1, "Severity")];
        let entries = vec![entry_by_id(1, "a"), entry_by_name("Severity", "b")];
        let err = resolve_entries(Some(&defs), entries).unwrap_err();
        assert!(err.message.contains("more than one entry"));
        assert!(err.message.contains('1'));
    }

    #[test]
    fn null_value_reaches_single_none() {
        let entry = IssueCustomFieldEntry {
            id: Some(1),
            name: None,
            value: IssueCustomFieldValueInput::Single(None),
        };
        let out = resolve_entries(None, vec![entry]).unwrap();
        assert_eq!(out[0].value, CustomFieldValue::Single(None));
    }
}
