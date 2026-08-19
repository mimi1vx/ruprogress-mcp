//! Generates `docs/tool-reference.md` from the live router and fails when
//! it drifts. Build a `RedmineMcp` with every plugin/admin flag on (so the
//! reference covers every tool, gated or not), render one section per tool
//! from its real `tools/list` schema, and either write the file
//! (`UPDATE_DOCS=1`) or compare against what is already on disk.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

mod support;

use std::fmt::Write as _;

use ruprogress_mcp::auth::scope::{ScopeRule, TOOL_SCOPES};
use ruprogress_mcp::readonly::write_tools;
use serde_json::Value;

/// Tool name -> the family heading it renders under, in the order families
/// should appear. Mirrors `RedmineMcp::new`'s router-merge order
/// (`server.rs`), which in turn mirrors one heading per `tools/` module.
const FAMILIES: &[(&str, &[&str])] = &[
    ("Meta", &["get_mcp_server_info"]),
    (
        "Discovery",
        &[
            "list_redmine_trackers",
            "list_project_trackers",
            "list_redmine_issue_statuses",
            "list_redmine_issue_priorities",
            "list_redmine_users",
            "get_current_user",
            "list_redmine_queries",
        ],
    ),
    (
        "Projects",
        &[
            "list_redmine_projects",
            "list_project_issue_custom_fields",
            "summarize_project_status",
            "list_redmine_versions",
            "manage_redmine_version",
            "list_project_members",
            "list_redmine_roles",
            "get_project_modules",
            "manage_project_member",
        ],
    ),
    (
        "Issues",
        &[
            "get_redmine_issue",
            "list_redmine_issues",
            "search_redmine_issues",
            "list_subtasks",
            "get_private_notes",
            "create_redmine_issue",
            "update_redmine_issue",
            "delete_redmine_issue",
            "copy_issue",
            "manage_issue_relation",
            "manage_issue_watcher",
            "manage_issue_note",
            "manage_issue_category",
        ],
    ),
    (
        "Time tracking",
        &[
            "list_time_entries",
            "manage_time_entry",
            "list_time_entry_activities",
            "import_time_entries",
        ],
    ),
    (
        "Search & wiki",
        &["search_entire_redmine", "manage_redmine_wiki_page"],
    ),
    ("Gantt", &["get_gantt_chart"]),
    (
        "Files",
        &[
            "get_redmine_attachment",
            "list_files",
            "delete_file",
            "upload_file",
            "cleanup_attachment_files",
        ],
    ),
    (
        "Plugins: RedmineUP Checklists",
        &[
            "get_checklist",
            "create_checklist_item",
            "update_checklist_item",
        ],
    ),
    ("Plugins: RedmineUP Products", &["manage_product"]),
    ("Plugins: RedmineUP CRM", &["manage_contact"]),
    ("Plugins: DMSF", &["manage_document"]),
];

/// The env flag that registers a gated tool, for the "gated by" column.
/// Mirrors `server.rs`'s `PLUGIN_TOOLS` table plus the
/// `REDMINE_MCP_EXPOSE_ADMIN_TOOLS` check right after it — kept in one
/// place here rather than derived from the router, since a `fn(&PluginFlags)
/// -> bool` predicate cannot be inspected at runtime to recover the env var
/// name it reads.
fn gating_flag(tool: &str) -> Option<&'static str> {
    match tool {
        "get_checklist" | "create_checklist_item" | "update_checklist_item" => {
            Some("REDMINE_CHECKLISTS_ENABLED")
        }
        "manage_product" => Some("REDMINE_PRODUCTS_ENABLED"),
        "manage_contact" => Some("REDMINE_CRM_ENABLED"),
        "manage_document" => Some("REDMINE_DMSF_ENABLED"),
        "cleanup_attachment_files" => Some("REDMINE_MCP_EXPOSE_ADMIN_TOOLS"),
        _ => None,
    }
}

fn write_kind(tool: &str) -> &'static str {
    if write_tools::ALL.contains(&tool) {
        "write"
    } else if write_tools::PARTIAL_WRITE.contains(&tool) {
        "partial (per `action`)"
    } else {
        "read"
    }
}

fn scopes_for(tool: &str) -> String {
    match TOOL_SCOPES.iter().find(|(name, _)| *name == tool) {
        None => "*(no table entry — denied by default under OAuth scope enforcement)*".to_string(),
        Some((_, ScopeRule::Fixed([]))) => "any authenticated token".to_string(),
        Some((_, ScopeRule::Fixed(s))) => s
            .iter()
            .map(|s| format!("`{s}`"))
            .collect::<Vec<_>>()
            .join(", "),
        Some((_, ScopeRule::AnyOf(s))) => format!(
            "any of: {}",
            s.iter()
                .map(|s| format!("`{s}`"))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Some((_, ScopeRule::PerAction(actions))) => actions
            .iter()
            .map(|(action, s)| {
                format!(
                    "`{action}`: {}",
                    s.iter()
                        .map(|s| format!("`{s}`"))
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            })
            .collect::<Vec<_>>()
            .join("; "),
    }
}

/// Resolves `{"$ref": "#/$defs/Name"}` against `root`'s `$defs`, one level
/// deep — enough to describe a nested parameter type without inlining whole
/// schemas (see the phase's own size-blowup risk note).
fn resolve_ref<'a>(root: &'a Value, schema: &'a Value) -> &'a Value {
    let Some(ptr) = schema.get("$ref").and_then(Value::as_str) else {
        return schema;
    };
    let Some(name) = ptr.strip_prefix("#/$defs/") else {
        return schema;
    };
    root.get("$defs")
        .and_then(|d| d.get(name))
        .unwrap_or(schema)
}

fn type_summary(root: &Value, schema: &Value) -> String {
    let schema = resolve_ref(root, schema);
    if let Some(ty) = schema.get("type") {
        let names: Vec<String> = match ty {
            Value::String(s) => vec![s.clone()],
            Value::Array(arr) => arr
                .iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect(),
            _ => vec![],
        };
        if !names.is_empty() {
            if let Some(items) = schema.get("items")
                && names.iter().any(|n| n == "array")
            {
                let item_ty = type_summary(root, items);
                return format!("array<{item_ty}>");
            }
            return names.join(" \\| ");
        }
    }
    if schema.get("enum").is_some() {
        return "enum".to_string();
    }
    if schema.get("$ref").is_some() {
        return "object".to_string();
    }
    if let Some(variants) = schema
        .get("anyOf")
        .or_else(|| schema.get("oneOf"))
        .and_then(Value::as_array)
    {
        let names: Vec<String> = variants.iter().map(|v| type_summary(root, v)).collect();
        return names.join(" \\| ");
    }
    "any".to_string()
}

fn doc_summary(root: &Value, schema: &Value) -> String {
    let resolved = resolve_ref(root, schema);
    let text = schema
        .get("description")
        .or_else(|| resolved.get("description"))
        .and_then(Value::as_str)
        .unwrap_or("");
    // One line: the doc is a table cell.
    text.lines().next().unwrap_or("").replace('|', "\\|")
}

/// Renders a `Parameter | Type | Required | Description` table from a tool's
/// input schema, or a one-line note if it takes no parameters.
fn params_table(schema: &Value) -> String {
    let Some(props) = schema.get("properties").and_then(Value::as_object) else {
        return "*(no parameters)*\n".to_string();
    };
    if props.is_empty() {
        return "*(no parameters)*\n".to_string();
    }
    let required: Vec<&str> = schema
        .get("required")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();

    let mut names: Vec<&String> = props.keys().collect();
    names.sort();

    let mut out = String::new();
    out.push_str("| Parameter | Type | Required | Description |\n");
    out.push_str("|---|---|---|---|\n");
    for name in names {
        let field_schema = &props[name];
        let ty = type_summary(schema, field_schema);
        let req = if required.contains(&name.as_str()) {
            "yes"
        } else {
            "no"
        };
        let doc = doc_summary(schema, field_schema);
        let _ = writeln!(out, "| `{name}` | {ty} | {req} | {doc} |");
    }
    out
}

/// The output schema's top-level shape, one line — not a full recursive
/// table, per the phase's own "do not inline whole JSON schemas" risk.
fn output_summary(schema: Option<&Value>) -> String {
    let Some(schema) = schema else {
        return "*(no structured output)*".to_string();
    };
    let Some(props) = schema.get("properties").and_then(Value::as_object) else {
        return type_summary(schema, schema);
    };
    let mut names: Vec<&String> = props.keys().collect();
    names.sort();
    let fields = names
        .iter()
        .map(|n| format!("`{n}`"))
        .collect::<Vec<_>>()
        .join(", ");
    format!("object: {fields}")
}

fn render(tools: &[rmcp::model::Tool]) -> String {
    let mut out = String::new();
    out.push_str("# Tool reference\n\n");
    out.push_str(
        "Generated from the live router by `cargo test -p ruprogress-mcp docs_reference` \
         (`tests/tool_reference_doc.rs`) — do not hand-edit. Run with `UPDATE_DOCS=1` to \
         regenerate after a tool's schema changes.\n\n\
         Every tool this build can register is listed, including the ones behind a plugin \
         or admin flag; a tool without a \"Gated by\" line is registered unconditionally. \
         \"Kind\" is `write` for tools read-only mode always hides, `partial` for tools with \
         a mix of read/write `action`s, and `read` otherwise. \"Required scopes\" is this \
         server's `oauth`/`oauth-proxy` scope-enforcement requirement for the common case; \
         see `docs/tool-contract.md` for the argument-sensitive exceptions.\n\n",
    );

    for (family, names) in FAMILIES {
        let _ = writeln!(out, "## {family}\n");
        for name in *names {
            let Some(tool) = tools.iter().find(|t| t.name == *name) else {
                continue;
            };
            let description = tool.description.as_deref().unwrap_or("");
            let _ = writeln!(out, "### `{name}`\n\n{description}\n");
            let mut meta = vec![format!("- **Kind:** {}", write_kind(name))];
            if let Some(flag) = gating_flag(name) {
                meta.push(format!("- **Gated by:** `{flag}`"));
            }
            meta.push(format!("- **Required scopes:** {}", scopes_for(name)));
            out.push_str(&meta.join("\n"));
            out.push_str("\n\n");
            out.push_str("**Parameters**\n\n");
            let input = Value::Object((*tool.input_schema).clone());
            out.push_str(&params_table(&input));
            out.push('\n');
            let output = output_summary(
                tool.output_schema
                    .as_ref()
                    .map(|s| Value::Object((**s).clone()))
                    .as_ref(),
            );
            let _ = writeln!(out, "**Output:** {output}\n");
        }
    }

    let covered: usize = FAMILIES.iter().map(|(_, names)| names.len()).sum();
    assert_eq!(
        covered,
        tools.len(),
        "a tool is registered but missing from FAMILIES in tests/tool_reference_doc.rs \
         (or vice versa) — add it to the right family section"
    );

    out
}

#[tokio::test]
async fn docs_reference() {
    let h = support::harness(&[
        ("REDMINE_CHECKLISTS_ENABLED", "true"),
        ("REDMINE_PRODUCTS_ENABLED", "true"),
        ("REDMINE_CRM_ENABLED", "true"),
        ("REDMINE_DMSF_ENABLED", "true"),
        ("REDMINE_MCP_EXPOSE_ADMIN_TOOLS", "true"),
    ])
    .await;
    let tools = h
        .client
        .list_tools(None)
        .await
        .expect("list_tools should succeed")
        .tools;

    let rendered = render(&tools);

    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/tool-reference.md");

    if std::env::var("UPDATE_DOCS").as_deref() == Ok("1") {
        std::fs::write(&path, &rendered).expect("write docs/tool-reference.md");
        return;
    }

    let on_disk = std::fs::read_to_string(&path).unwrap_or_default();
    assert_eq!(
        on_disk, rendered,
        "docs/tool-reference.md is stale — regenerate with \
         `UPDATE_DOCS=1 cargo test -p ruprogress-mcp docs_reference`"
    );
}
