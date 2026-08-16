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

use crate::config::{CustomFieldConfig, CustomFieldDefaultValue};
use crate::error::to_tool_error;
use crate::tools::output::{self, ErrorCode};

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

// --- recovering from a rejected required custom field ---

/// One field named in a 422 response as blank, invalid, or outside its
/// allowed values — the part of the message before the marker that matched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MissingField {
    pub(crate) label: String,
}

/// Redmine 422 message markers recognized as "this field has a problem
/// autofill might fix", tried in order. Rails' actual blank-field message is
/// `can't be blank`; `cannot be blank` is kept too in case a customized
/// validator uses it. Matching is case-insensitive; the label is everything
/// before the marker.
const REQUIRED_FIELD_MARKERS: &[&str] = &[
    "can't be blank",
    "cannot be blank",
    "is not included in the list",
    "is invalid",
];

/// Extracts every `"<label> <marker>"` message in `errors` as a
/// [`MissingField`], stripping an optional `"Validation failed: "` prefix
/// first. A 422 whose messages match none of `REQUIRED_FIELD_MARKERS` yields
/// an empty vec — not every validation failure names a fillable field.
pub(crate) fn parse_required_field_errors(errors: &[String]) -> Vec<MissingField> {
    errors
        .iter()
        .filter_map(|raw| {
            let text = raw.strip_prefix("Validation failed: ").unwrap_or(raw);
            let lower = text.to_ascii_lowercase();
            REQUIRED_FIELD_MARKERS.iter().find_map(|marker| {
                lower.find(marker).map(|idx| MissingField {
                    label: text[..idx].trim().to_string(),
                })
            })
        })
        .collect()
}

/// Redmine's field labels for the standard issue attributes
/// `create_redmine_issue`/`update_redmine_issue` expose directly — enough to
/// tell "a core field failed" from "a custom field failed" for the error
/// hint below, not an exhaustive Redmine list. A label missing from this
/// list only costs a slightly-off hint, never a wrong write.
const CORE_ISSUE_FIELD_LABELS: &[&str] = &[
    "Subject",
    "Description",
    "Tracker",
    "Status",
    "Priority",
    "Assignee",
    "Category",
    "Target version",
    "Parent task",
    "Start date",
    "Due date",
    "Estimated time",
    "Done ratio",
    "Is private",
    "Spent time",
];

fn looks_like_custom_field(label: &str) -> bool {
    !CORE_ISSUE_FIELD_LABELS
        .iter()
        .any(|core| core.eq_ignore_ascii_case(label))
}

/// The in-band answer to a required-field 422 that autofill did not (or
/// could not) recover: `missing_required_fields` plus a hint that depends on
/// whether any named field looks like a custom field, and — for a custom
/// field, only when autofill is off — a pointer at the env var that would
/// enable it.
fn required_field_error(missing: &[MissingField], autofill_enabled: bool) -> CallToolResult {
    let labels: Vec<&str> = missing.iter().map(|f| f.label.as_str()).collect();
    let any_custom = missing.iter().any(|f| looks_like_custom_field(&f.label));
    let hint = if !any_custom {
        "these are standard issue fields, not custom fields; look up a valid value with the \
         discovery tools (list_redmine_trackers, list_redmine_issue_statuses, ...) and set it \
         explicitly"
    } else if autofill_enabled {
        "these look like custom fields with no usable default; call \
         list_project_issue_custom_fields to see the allowed values and set them explicitly"
    } else {
        "these look like custom fields; set them explicitly (call \
         list_project_issue_custom_fields to see the allowed values), or ask the operator to set \
         REDMINE_AUTOFILL_REQUIRED_CUSTOM_FIELDS=true"
    };
    let mut extra = serde_json::Map::new();
    extra.insert(
        "missing_required_fields".to_string(),
        serde_json::Value::Array(
            labels
                .iter()
                .map(|l| serde_json::Value::String((*l).to_string()))
                .collect(),
        ),
    );
    output::err_with(
        ErrorCode::ValidationFailed,
        format!(
            "Redmine rejected the request: missing or invalid required field(s): {}",
            labels.join(", ")
        ),
        Some(hint),
        extra,
    )
}

/// A value recovered for one required field, ready to merge into the
/// retried write and to report back to the caller.
pub(crate) struct Fill {
    pub(crate) id: u64,
    pub(crate) name: String,
    pub(crate) value: CustomFieldValue,
}

/// The field's own `default_value` first, then the configured defaults map
/// — matched against `def`'s normalized name the same way every other
/// lookup in this module matches names. Returns the candidate as a
/// `Vec<String>` (one entry for a single-valued field) so the caller can
/// check `possible_values` membership uniformly for single and multi-value
/// fields. `None` means neither source has anything usable.
fn pick_candidate(def: &CustomFieldDefinition, cfg: &CustomFieldConfig) -> Option<Vec<String>> {
    if let Some(default) = &def.default_value
        && !default.is_empty()
    {
        return Some(vec![default.clone()]);
    }
    let normalized = normalize_field_name(&def.name);
    let (_, value) = cfg
        .defaults
        .iter()
        .find(|(name, _)| normalize_field_name(name) == normalized)?;
    match value {
        CustomFieldDefaultValue::Single(s) if !s.is_empty() => Some(vec![s.clone()]),
        CustomFieldDefaultValue::Multiple(items) if !items.is_empty() => Some(items.clone()),
        CustomFieldDefaultValue::Single(_) | CustomFieldDefaultValue::Multiple(_) => None,
    }
}

/// Computes a recovery value for each field in `missing` that resolves to a
/// known issue custom field definition and has a usable default. A field
/// with no matching definition (e.g. a standard field, or one this
/// project/tracker does not carry), or whose only candidate value is empty
/// or outside a restricted `possible_values` list, produces no [`Fill`] —
/// an empty result means there is nothing to retry with.
pub(crate) fn compute_autofill(
    defs: &[CustomFieldDefinition],
    missing: &[MissingField],
    cfg: &CustomFieldConfig,
) -> Vec<Fill> {
    missing
        .iter()
        .filter_map(|field| {
            let normalized = normalize_field_name(&field.label);
            let def = defs
                .iter()
                .find(|d| normalize_field_name(&d.name) == normalized)?;
            let candidate = pick_candidate(def, cfg)?;
            if let Some(possible) = &def.possible_values
                && !possible.is_empty()
                && !candidate.iter().all(|v| possible.contains(v))
            {
                return None;
            }
            let value = if def.multiple.unwrap_or(false) {
                CustomFieldValue::Multiple(candidate)
            } else {
                CustomFieldValue::Single(candidate.into_iter().next())
            };
            Some(Fill {
                id: def.id,
                name: def.name.clone(),
                value,
            })
        })
        .collect()
}

/// Overlays `fills` onto `existing` by field id: a field already present is
/// updated in place (it was just proven invalid, so its prior value — set by
/// the caller or absent — is replaced), everything else is appended.
fn merge_fills(existing: Option<Vec<CustomFieldWrite>>, fills: &[Fill]) -> Vec<CustomFieldWrite> {
    let mut merged = existing.unwrap_or_default();
    for fill in fills {
        if let Some(entry) = merged.iter_mut().find(|e| e.id == fill.id) {
            entry.value = fill.value.clone();
        } else {
            merged.push(CustomFieldWrite {
                id: fill.id,
                value: fill.value.clone(),
            });
        }
    }
    merged
}

/// Which Redmine resource anchors the definitions lookup a retry needs: a
/// new issue already carries its project reference; an existing issue's
/// parameters carry only its id, so its project is learned via a dedicated
/// read first.
pub(crate) enum AutofillTarget<'a> {
    Create(&'a ProjectIdent),
    Update(IssueId),
}

/// Always fetches fresh, even when resolving `custom_fields` by name just
/// fetched the same project's definitions moments earlier — reusing that
/// result would couple the two paths together to save one request on a
/// failure path.
async fn fetch_definitions_for_autofill(
    scoped: &Scoped<'_>,
    target: &AutofillTarget<'_>,
) -> redmine_client::Result<Vec<CustomFieldDefinition>> {
    let project_id = match target {
        AutofillTarget::Create(id) => (*id).clone(),
        AutofillTarget::Update(issue_id) => {
            let issue = scoped.get_issue(*issue_id, &[]).await?;
            ProjectIdent::Id(ProjectId(issue.project.id))
        }
    };
    let project = scoped
        .get_project(&project_id, &[ProjectInclude::IssueCustomFields])
        .await?;
    Ok(project.issue_custom_fields.unwrap_or_default())
}

/// What to do about a write that just failed, decided once and handed back
/// to the caller — which performs the actual retry itself. Keeping the
/// second HTTP call at the two tool call sites (rather than behind a generic
/// retry-callback abstraction here) sidesteps a `Send`-bound conflict
/// between an async closure's borrowed captures and the `#[tool]` macro's
/// generated future; the caller still only ever issues the second write
/// inside the single `Retry` arm below, so "exactly one retry, never a
/// loop" stays a property you can see by reading that one call site.
pub(crate) enum RequiredFieldRecovery {
    /// Not recoverable, or autofill is off: return this in-band error as
    /// the tool's result.
    GiveUp(CallToolResult),
    /// Retry the write with these merged custom fields; if it succeeds,
    /// report `fills` as `autofilled_custom_fields`.
    Retry {
        merged: Vec<CustomFieldWrite>,
        fills: Vec<Fill>,
    },
}

/// Inspects a failed write's error for a recoverable required-field
/// rejection and decides what to do. Returns `None` when autofill has
/// nothing to say about this error at all (not a 422, or a 422 that names no
/// fillable field) — the caller falls back to the ordinary error envelope.
pub(crate) async fn recover_required_fields(
    scoped: &Scoped<'_>,
    target: AutofillTarget<'_>,
    custom_fields: Option<Vec<CustomFieldWrite>>,
    cfg: &CustomFieldConfig,
    error: &redmine_client::Error,
) -> Option<RequiredFieldRecovery> {
    let redmine_client::Error::Api { status, errors } = error else {
        return None;
    };
    if *status != http::StatusCode::UNPROCESSABLE_ENTITY {
        return None;
    }
    let missing = parse_required_field_errors(errors);
    if missing.is_empty() {
        return None;
    }
    if !cfg.autofill_required {
        return Some(RequiredFieldRecovery::GiveUp(required_field_error(
            &missing, false,
        )));
    }

    let Ok(defs) = fetch_definitions_for_autofill(scoped, &target).await else {
        return Some(RequiredFieldRecovery::GiveUp(required_field_error(
            &missing, true,
        )));
    };
    let fills = compute_autofill(&defs, &missing, cfg);
    if fills.is_empty() {
        return Some(RequiredFieldRecovery::GiveUp(required_field_error(
            &missing, true,
        )));
    }

    let merged = merge_fills(custom_fields, &fills);
    Some(RequiredFieldRecovery::Retry { merged, fills })
}

/// After a retried write also fails, decides whether that second error is
/// itself an (unrecoverable) required-field 422 worth the enriched envelope,
/// or an ordinary failure the caller should map with [`to_tool_error`]
/// instead. There is no third attempt under either outcome.
pub(crate) fn retry_still_missing_required_fields(
    error: &redmine_client::Error,
) -> Option<CallToolResult> {
    let redmine_client::Error::Api { status, errors } = error else {
        return None;
    };
    if *status != http::StatusCode::UNPROCESSABLE_ENTITY {
        return None;
    }
    let missing = parse_required_field_errors(errors);
    if missing.is_empty() {
        return None;
    }
    Some(required_field_error(&missing, true))
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

    // --- parse_required_field_errors ---

    #[test]
    fn parses_rails_actual_blank_wording() {
        let missing = parse_required_field_errors(&["Subject can't be blank".to_string()]);
        assert_eq!(
            missing,
            vec![MissingField {
                label: "Subject".to_string()
            }]
        );
    }

    #[test]
    fn parses_the_umbrella_spelling_too() {
        let missing = parse_required_field_errors(&["Department cannot be blank".to_string()]);
        assert_eq!(
            missing,
            vec![MissingField {
                label: "Department".to_string()
            }]
        );
    }

    #[test]
    fn parses_not_included_in_the_list() {
        let missing =
            parse_required_field_errors(&["Severity is not included in the list".to_string()]);
        assert_eq!(
            missing,
            vec![MissingField {
                label: "Severity".to_string()
            }]
        );
    }

    #[test]
    fn parses_is_invalid() {
        let missing = parse_required_field_errors(&["Start date is invalid".to_string()]);
        assert_eq!(
            missing,
            vec![MissingField {
                label: "Start date".to_string()
            }]
        );
    }

    #[test]
    fn strips_a_validation_failed_prefix() {
        let missing =
            parse_required_field_errors(&["Validation failed: Subject can't be blank".to_string()]);
        assert_eq!(
            missing,
            vec![MissingField {
                label: "Subject".to_string()
            }]
        );
    }

    #[test]
    fn parses_a_multi_word_label() {
        let missing = parse_required_field_errors(&["Story Points can't be blank".to_string()]);
        assert_eq!(
            missing,
            vec![MissingField {
                label: "Story Points".to_string()
            }]
        );
    }

    #[test]
    fn a_422_that_names_no_fillable_field_yields_an_empty_vec() {
        let missing = parse_required_field_errors(&[
            "Issue relations conflict with an existing relation".to_string(),
        ]);
        assert!(missing.is_empty());
    }

    // --- compute_autofill ---

    fn def_with(
        id: u64,
        name: &str,
        default_value: Option<&str>,
        possible_values: Option<&[&str]>,
        multiple: bool,
    ) -> CustomFieldDefinition {
        serde_json::from_value(serde_json::json!({
            "id": id,
            "name": name,
            "field_format": "string",
            "default_value": default_value,
            "possible_values": possible_values.map(|values| {
                values.iter().map(|v| serde_json::json!({"value": v})).collect::<Vec<_>>()
            }),
            "multiple": multiple,
        }))
        .unwrap()
    }

    fn missing(label: &str) -> MissingField {
        MissingField {
            label: label.to_string(),
        }
    }

    fn cfg_with_defaults(pairs: &[(&str, CustomFieldDefaultValue)]) -> CustomFieldConfig {
        CustomFieldConfig {
            autofill_required: true,
            defaults: pairs
                .iter()
                .map(|(k, v)| ((*k).to_string(), v.clone()))
                .collect(),
        }
    }

    #[test]
    fn uses_the_definitions_own_default_value() {
        let defs = vec![def_with(1, "Department", Some("Engineering"), None, false)];
        let cfg = cfg_with_defaults(&[]);
        let fills = compute_autofill(&defs, &[missing("Department")], &cfg);
        assert_eq!(fills.len(), 1);
        assert_eq!(fills[0].id, 1);
        assert_eq!(
            fills[0].value,
            CustomFieldValue::Single(Some("Engineering".to_string()))
        );
    }

    #[test]
    fn falls_back_to_the_configured_map_when_no_default_value() {
        let defs = vec![def_with(1, "Department", None, None, false)];
        let cfg = cfg_with_defaults(&[(
            "Department",
            CustomFieldDefaultValue::Single("Sales".to_string()),
        )]);
        let fills = compute_autofill(&defs, &[missing("Department")], &cfg);
        assert_eq!(
            fills[0].value,
            CustomFieldValue::Single(Some("Sales".to_string()))
        );
    }

    #[test]
    fn the_definitions_own_default_wins_over_the_configured_map() {
        let defs = vec![def_with(1, "Department", Some("Engineering"), None, false)];
        let cfg = cfg_with_defaults(&[(
            "Department",
            CustomFieldDefaultValue::Single("Sales".to_string()),
        )]);
        let fills = compute_autofill(&defs, &[missing("Department")], &cfg);
        assert_eq!(
            fills[0].value,
            CustomFieldValue::Single(Some("Engineering".to_string()))
        );
    }

    #[test]
    fn a_candidate_outside_possible_values_produces_no_fill() {
        let defs = vec![def_with(
            1,
            "Severity",
            Some("Unlisted"),
            Some(&["Low", "High"]),
            false,
        )];
        let cfg = cfg_with_defaults(&[]);
        let fills = compute_autofill(&defs, &[missing("Severity")], &cfg);
        assert!(fills.is_empty());
    }

    #[test]
    fn a_multiple_field_wraps_a_single_candidate_into_an_array() {
        let defs = vec![def_with(1, "Tags", Some("blue"), None, true)];
        let cfg = cfg_with_defaults(&[]);
        let fills = compute_autofill(&defs, &[missing("Tags")], &cfg);
        assert_eq!(
            fills[0].value,
            CustomFieldValue::Multiple(vec!["blue".to_string()])
        );
    }

    #[test]
    fn a_field_with_no_source_produces_no_fill() {
        let defs = vec![def_with(1, "Department", None, None, false)];
        let cfg = cfg_with_defaults(&[]);
        let fills = compute_autofill(&defs, &[missing("Department")], &cfg);
        assert!(fills.is_empty());
    }

    #[test]
    fn a_label_matching_no_definition_produces_no_fill() {
        let defs = vec![def_with(1, "Department", Some("Engineering"), None, false)];
        let cfg = cfg_with_defaults(&[]);
        let fills = compute_autofill(&defs, &[missing("Subject")], &cfg);
        assert!(fills.is_empty());
    }

    #[test]
    fn nothing_fillable_yields_an_empty_vec() {
        let defs = vec![def_with(1, "Department", None, None, false)];
        let cfg = cfg_with_defaults(&[]);
        let fills = compute_autofill(&defs, &[missing("Subject"), missing("Department")], &cfg);
        assert!(fills.is_empty());
    }

    // --- required_field_error ---

    #[test]
    fn core_field_only_gets_the_standard_field_hint() {
        let result = required_field_error(&[missing("Subject")], false);
        let structured = result.structured_content.unwrap();
        assert_eq!(
            structured["missing_required_fields"],
            serde_json::json!(["Subject"])
        );
        assert!(
            structured["hint"]
                .as_str()
                .unwrap()
                .contains("discovery tools")
        );
    }

    #[test]
    fn custom_field_with_autofill_off_names_the_env_var() {
        let result = required_field_error(&[missing("Department")], false);
        let structured = result.structured_content.unwrap();
        assert!(
            structured["hint"]
                .as_str()
                .unwrap()
                .contains("REDMINE_AUTOFILL_REQUIRED_CUSTOM_FIELDS")
        );
    }

    #[test]
    fn custom_field_with_autofill_on_does_not_repeat_the_env_var() {
        let result = required_field_error(&[missing("Department")], true);
        let structured = result.structured_content.unwrap();
        assert!(
            !structured["hint"]
                .as_str()
                .unwrap()
                .contains("REDMINE_AUTOFILL_REQUIRED_CUSTOM_FIELDS")
        );
    }
}
