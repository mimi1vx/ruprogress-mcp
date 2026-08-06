//! e2e: `tools/list` returns exactly the tools implemented so far, and each
//! returns fixture-derived content.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

mod support;

use rmcp::model::CallToolRequestParams;
use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, ResponseTemplate};

/// The tools implemented so far.
const IMPLEMENTED_TOOLS: &[&str] = &[
    "get_current_user",
    "get_mcp_server_info",
    "list_redmine_projects",
];

/// Every tool name in `docs/tool-contract.md` (vendored from the upstream
/// reference server, captured 2026-08-06). The router's tools must be a
/// *subset* of this list — not equal, until the full tool surface is built
/// out — so a typo'd or made-up tool name is a build/test failure rather
/// than a silent divergence from the reference contract.
const EXPECTED_TOOLS: &[&str] = &[
    "list_redmine_projects",
    "list_project_issue_custom_fields",
    "summarize_project_status",
    "list_redmine_versions",
    "manage_redmine_version",
    "list_project_members",
    "list_redmine_roles",
    "get_project_modules",
    "manage_project_member",
    "get_redmine_issue",
    "list_redmine_issues",
    "search_redmine_issues",
    "create_redmine_issue",
    "update_redmine_issue",
    "delete_redmine_issue",
    "copy_issue",
    "manage_issue_relation",
    "list_subtasks",
    "manage_issue_watcher",
    "manage_issue_note",
    "get_private_notes",
    "manage_issue_category",
    "show_triage_board",
    "get_triage_board_data",
    "show_project_dashboard",
    "get_project_dashboard_data",
    "list_time_entries",
    "manage_time_entry",
    "list_time_entry_activities",
    "list_redmine_trackers",
    "list_project_trackers",
    "list_redmine_issue_statuses",
    "list_redmine_issue_priorities",
    "list_redmine_users",
    "get_current_user",
    "list_redmine_queries",
    "import_time_entries",
    "search_entire_redmine",
    "manage_redmine_wiki_page",
    "list_files",
    "upload_file",
    "delete_file",
    "get_redmine_attachment",
    "cleanup_attachment_files",
    "get_checklist",
    "update_checklist_item",
    "create_checklist_item",
    "get_gantt_chart",
    "manage_product",
    "manage_contact",
    "manage_document",
    "get_mcp_server_info",
];

fn content_text(result: &rmcp::model::CallToolResult) -> String {
    result
        .content
        .iter()
        .filter_map(|c| c.as_text())
        .map(|t| t.text.clone())
        .collect::<Vec<_>>()
        .join("\n")
}

#[tokio::test]
async fn tools_list_returns_exactly_the_implemented_tools() {
    let h = support::harness(&[]).await;
    let tools = h
        .client
        .list_tools(None)
        .await
        .expect("list_tools should succeed");
    let mut names: Vec<&str> = tools.tools.iter().map(|t| t.name.as_ref()).collect();
    names.sort_unstable();
    let mut expected = IMPLEMENTED_TOOLS.to_vec();
    expected.sort_unstable();
    assert_eq!(names, expected);

    // Every tool implemented so far takes no parameters.
    for tool in &tools.tools {
        let props = tool
            .input_schema
            .get("properties")
            .and_then(Value::as_object);
        assert!(
            props.is_none_or(serde_json::Map::is_empty),
            "{} should have no parameters, got {:?}",
            tool.name,
            tool.input_schema
        );
    }
}

#[tokio::test]
async fn router_tools_are_a_subset_of_the_vendored_tool_contract() {
    let h = support::harness(&[]).await;
    let tools = h
        .client
        .list_tools(None)
        .await
        .expect("list_tools should succeed");
    for tool in &tools.tools {
        assert!(
            EXPECTED_TOOLS.contains(&tool.name.as_ref()),
            "{} is not a name from docs/tool-contract.md (typo, or the vendored contract is stale)",
            tool.name
        );
    }
}

#[tokio::test]
async fn get_current_user_returns_fixture_derived_content() {
    let h = support::harness(&[]).await;
    Mock::given(method("GET"))
        .and(path("/my/account.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "user": {
                "id": 5,
                "login": "alice",
                "firstname": "Alice",
                "lastname": "Example",
                "mail": "alice@example.com",
                "created_on": "2025-06-01T08:00:00Z",
                "last_login_on": "2026-01-05T09:15:00Z"
            }
        })))
        .mount(&h.redmine)
        .await;

    let result = h
        .client
        .call_tool(CallToolRequestParams::new("get_current_user"))
        .await
        .expect("call_tool should succeed");
    let text = content_text(&result);
    let body: Value = text
        .lines()
        .last()
        .and_then(|l| serde_json::from_str(l).ok())
        .expect("last content block should be the JSON body");

    assert_eq!(body["id"], 5);
    assert_eq!(body["login"], "alice");
    assert_eq!(body["mail"], "alice@example.com");
    // Display-name fields are wrapped in the prompt-injection boundary.
    assert!(body["firstname"].as_str().unwrap().contains("Alice"));
    assert!(
        body["firstname"]
            .as_str()
            .unwrap()
            .starts_with("<<<untrusted:")
    );
}

#[tokio::test]
async fn list_redmine_projects_returns_fixture_derived_content() {
    let h = support::harness(&[]).await;
    Mock::given(method("GET"))
        .and(path("/projects.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "projects": [
                {
                    "id": 1,
                    "name": "My Project",
                    "identifier": "my-project",
                    "description": "A test project",
                    "created_on": "2026-01-01T00:00:00Z",
                    "updated_on": "2026-01-01T00:00:00Z"
                }
            ],
            "total_count": 1,
            "offset": 0,
            "limit": 100
        })))
        .mount(&h.redmine)
        .await;

    let result = h
        .client
        .call_tool(CallToolRequestParams::new("list_redmine_projects"))
        .await
        .expect("call_tool should succeed");
    let text = content_text(&result);
    let body: Value = text
        .lines()
        .last()
        .and_then(|l| serde_json::from_str(l).ok())
        .expect("last content block should be the JSON body");

    let projects = body["projects"]
        .as_array()
        .expect("body.projects should be a JSON array");
    assert_eq!(projects.len(), 1);
    assert_eq!(projects[0]["id"], 1);
    assert_eq!(projects[0]["identifier"], "my-project");
    assert!(projects[0]["name"].as_str().unwrap().contains("My Project"));
    assert!(
        projects[0]["name"]
            .as_str()
            .unwrap()
            .starts_with("<<<untrusted:")
    );
    assert_eq!(body["pagination"]["total"], 1);
    assert_eq!(body["pagination"]["truncated"], false);
}

#[tokio::test]
async fn get_mcp_server_info_reports_current_user_null_when_redmine_unreachable() {
    // No mock mounted for /my/account.json: the request fails.
    let h = support::harness(&[]).await;
    let result = h
        .client
        .call_tool(CallToolRequestParams::new("get_mcp_server_info"))
        .await
        .expect("call_tool should succeed even when Redmine is unreachable");
    let text = content_text(&result);
    let body: Value = text
        .lines()
        .last()
        .and_then(|l| serde_json::from_str(l).ok())
        .expect("last content block should be the JSON body");

    assert_eq!(body["current_user"], Value::Null);
    assert_eq!(body["read_only_mode"], false);
    assert_eq!(body["auth_mode"], "legacy");
    assert_eq!(body["server_version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(
        body["plugin_flags"],
        json!({
            "agile": false, "checklists": false, "products": false,
            "crm": false, "dmsf": false, "tags": false
        })
    );
}

#[tokio::test]
async fn get_mcp_server_info_never_leaks_the_redmine_host() {
    let h = support::harness(&[]).await;
    let result = h
        .client
        .call_tool(CallToolRequestParams::new("get_mcp_server_info"))
        .await
        .expect("call_tool should succeed");
    let text = content_text(&result);
    let host = h
        .redmine
        .uri()
        .parse::<url::Url>()
        .expect("mock uri should parse")
        .host_str()
        .expect("mock uri should have a host")
        .to_string();
    assert!(
        !text.contains(&host),
        "get_mcp_server_info leaked the Redmine host: {text}"
    );
}

// --- Sub-phase 4.0: structured output, schemas, and annotations (D1/D2/D7) ---

#[tokio::test]
async fn every_implemented_tool_declares_an_object_output_schema_and_read_only_annotations() {
    let h = support::harness(&[]).await;
    let tools = h
        .client
        .list_tools(None)
        .await
        .expect("list_tools should succeed");
    for tool in &tools.tools {
        let schema = tool
            .output_schema
            .as_ref()
            .unwrap_or_else(|| panic!("{} is missing an outputSchema", tool.name));
        assert_eq!(
            schema.get("type").and_then(Value::as_str),
            Some("object"),
            "{}'s outputSchema root must be \"type\": \"object\" (D2)",
            tool.name
        );
        let annotations = tool
            .annotations
            .as_ref()
            .unwrap_or_else(|| panic!("{} is missing annotations", tool.name));
        assert_eq!(
            annotations.read_only_hint,
            Some(true),
            "{} should be annotated read_only_hint = true",
            tool.name
        );
    }
}

#[tokio::test]
async fn every_implemented_tool_call_returns_structured_content_matching_its_schema() {
    let h = support::harness(&[]).await;
    support::mock_current_user(&h.redmine, None).await;
    Mock::given(method("GET"))
        .and(path("/projects.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "projects": [],
            "total_count": 0,
            "offset": 0,
            "limit": 100
        })))
        .mount(&h.redmine)
        .await;

    let tools = h
        .client
        .list_tools(None)
        .await
        .expect("list_tools should succeed");
    for tool in &tools.tools {
        let result = h
            .client
            .call_tool(CallToolRequestParams::new(tool.name.clone()))
            .await
            .unwrap_or_else(|e| panic!("{} should be callable: {e}", tool.name));
        let schema = tool
            .output_schema
            .as_ref()
            .unwrap_or_else(|| panic!("{} is missing an outputSchema", tool.name));
        support::assert_structured_content_matches_schema(&result, schema);
    }
}

#[tokio::test]
async fn every_tool_description_is_short_and_names_when_to_call_it() {
    let h = support::harness(&[]).await;
    let tools = h
        .client
        .list_tools(None)
        .await
        .expect("list_tools should succeed");
    for tool in &tools.tools {
        let description = tool
            .description
            .as_deref()
            .unwrap_or_else(|| panic!("{} has no description", tool.name));
        assert!(
            !description.is_empty(),
            "{} has an empty description",
            tool.name
        );
        assert!(
            description.len() <= 400,
            "{}'s description is {} chars, over the 400-char budget",
            tool.name,
            description.len()
        );
        let names_when_to_call = ["Use this", "Use when", "Call this", "Call when"]
            .iter()
            .any(|phrase| description.contains(phrase));
        assert!(
            names_when_to_call,
            "{}'s description does not say when to call it: {description:?}",
            tool.name
        );
    }
}

#[test]
#[should_panic(expected = "must be a JSON object")]
fn conformance_helper_rejects_a_bare_array_structured_content() {
    let schema = serde_json::json!({"type": "object"})
        .as_object()
        .unwrap()
        .clone();
    let result = rmcp::model::CallToolResult::structured(serde_json::json!([1, 2, 3]));
    support::assert_structured_content_matches_schema(&result, &schema);
}
