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
    "list_redmine_trackers",
    "list_project_trackers",
    "list_redmine_issue_statuses",
    "list_redmine_issue_priorities",
    "list_redmine_users",
    "list_redmine_queries",
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
    "list_subtasks",
    "get_private_notes",
    "list_time_entries",
    "manage_time_entry",
    "list_time_entry_activities",
    "import_time_entries",
    "create_redmine_issue",
    "update_redmine_issue",
    "delete_redmine_issue",
    "copy_issue",
    "manage_issue_relation",
    "manage_issue_watcher",
    "manage_issue_note",
    "manage_issue_category",
    "search_entire_redmine",
    "manage_redmine_wiki_page",
    "get_gantt_chart",
    "get_redmine_attachment",
    "list_files",
    "delete_file",
    "upload_file",
];

/// Every tool name in `docs/tool-contract.md`. The router's tools must be a
/// *subset* of this list — not equal, until the full tool surface is built
/// out — so a typo'd or made-up tool name is a build/test failure rather
/// than a silent divergence from the documented contract.
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

/// Tools implemented so far that take parameters (everything else must have
/// an empty `properties` object).
const TOOLS_WITH_PARAMETERS: &[&str] = &[
    "list_project_trackers",
    "list_redmine_users",
    "list_project_issue_custom_fields",
    "summarize_project_status",
    "list_redmine_versions",
    "manage_redmine_version",
    "list_project_members",
    "get_project_modules",
    "manage_project_member",
    "get_redmine_issue",
    "list_redmine_issues",
    "search_redmine_issues",
    "list_subtasks",
    "get_private_notes",
    "list_time_entries",
    "manage_time_entry",
    "list_time_entry_activities",
    "import_time_entries",
    "create_redmine_issue",
    "update_redmine_issue",
    "delete_redmine_issue",
    "copy_issue",
    "manage_issue_relation",
    "manage_issue_watcher",
    "manage_issue_note",
    "manage_issue_category",
    "search_entire_redmine",
    "manage_redmine_wiki_page",
    "get_gantt_chart",
    "get_redmine_attachment",
    "list_files",
    "delete_file",
    "upload_file",
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

    // Every tool implemented so far takes no parameters, except the two
    // discovery tools with a documented input contract.
    for tool in &tools.tools {
        if TOOLS_WITH_PARAMETERS.contains(&tool.name.as_ref()) {
            continue;
        }
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

/// Plugin-gated tools implemented so far (`RedmineUP` Checklists Pro), added
/// to [`IMPLEMENTED_TOOLS`] only when their flag is on. As the remaining
/// plugin families are implemented this list grows towards the full set of
/// plugin-gated tools this server supports.
const CHECKLIST_TOOLS: &[&str] = &[
    "get_checklist",
    "create_checklist_item",
    "update_checklist_item",
];

#[tokio::test]
async fn tools_list_with_checklists_enabled_adds_exactly_the_checklist_tools() {
    let h = support::harness(&[("REDMINE_CHECKLISTS_ENABLED", "true")]).await;
    let tools = h
        .client
        .list_tools(None)
        .await
        .expect("list_tools should succeed");
    let mut names: Vec<&str> = tools.tools.iter().map(|t| t.name.as_ref()).collect();
    names.sort_unstable();
    let mut expected: Vec<&str> = IMPLEMENTED_TOOLS.to_vec();
    expected.extend_from_slice(CHECKLIST_TOOLS);
    expected.sort_unstable();
    assert_eq!(names, expected);
    assert_eq!(names.len(), 44, "41 base tools + 3 checklist tools");
}

/// Plugin-gated tools implemented so far (`RedmineUP` Products/CRM), added
/// to [`IMPLEMENTED_TOOLS`] only when their respective flag is on.
const PRODUCTS_CRM_TOOLS: &[&str] = &["manage_product", "manage_contact"];

/// The DMSF plugin's tool, added to [`IMPLEMENTED_TOOLS`] only when
/// `REDMINE_DMSF_ENABLED` is on.
const DMSF_TOOLS: &[&str] = &["manage_document"];

#[tokio::test]
async fn tools_list_with_dmsf_enabled_adds_exactly_manage_document() {
    let h = support::harness(&[("REDMINE_DMSF_ENABLED", "true")]).await;
    let tools = h
        .client
        .list_tools(None)
        .await
        .expect("list_tools should succeed");
    let mut names: Vec<&str> = tools.tools.iter().map(|t| t.name.as_ref()).collect();
    names.sort_unstable();
    let mut expected: Vec<&str> = IMPLEMENTED_TOOLS.to_vec();
    expected.extend_from_slice(DMSF_TOOLS);
    expected.sort_unstable();
    assert_eq!(names, expected);
    assert_eq!(names.len(), 42, "41 base tools + manage_document");
}

#[tokio::test]
async fn tools_list_with_products_and_crm_enabled_adds_exactly_those_tools() {
    let h = support::harness(&[
        ("REDMINE_PRODUCTS_ENABLED", "true"),
        ("REDMINE_CRM_ENABLED", "true"),
    ])
    .await;
    let tools = h
        .client
        .list_tools(None)
        .await
        .expect("list_tools should succeed");
    let mut names: Vec<&str> = tools.tools.iter().map(|t| t.name.as_ref()).collect();
    names.sort_unstable();
    let mut expected: Vec<&str> = IMPLEMENTED_TOOLS.to_vec();
    expected.extend_from_slice(PRODUCTS_CRM_TOOLS);
    expected.sort_unstable();
    assert_eq!(names, expected);
    assert_eq!(
        names.len(),
        43,
        "41 base tools + manage_product + manage_contact"
    );
}

/// Every plugin flag implemented so far, all at once.
#[tokio::test]
async fn tools_list_with_every_implemented_plugin_flag_enabled() {
    let h = support::harness(&[
        ("REDMINE_CHECKLISTS_ENABLED", "true"),
        ("REDMINE_PRODUCTS_ENABLED", "true"),
        ("REDMINE_CRM_ENABLED", "true"),
        ("REDMINE_DMSF_ENABLED", "true"),
    ])
    .await;
    let tools = h
        .client
        .list_tools(None)
        .await
        .expect("list_tools should succeed");
    let mut names: Vec<&str> = tools.tools.iter().map(|t| t.name.as_ref()).collect();
    names.sort_unstable();
    let mut expected: Vec<&str> = IMPLEMENTED_TOOLS.to_vec();
    expected.extend_from_slice(CHECKLIST_TOOLS);
    expected.extend_from_slice(PRODUCTS_CRM_TOOLS);
    expected.extend_from_slice(DMSF_TOOLS);
    expected.sort_unstable();
    assert_eq!(names, expected);
    assert_eq!(
        names.len(),
        47,
        "41 base + 3 checklist + manage_product + manage_contact + manage_document"
    );
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
async fn get_mcp_server_info_reports_the_resolved_current_user_when_reachable() {
    let h = support::harness(&[]).await;
    support::mock_current_user(&h.redmine, None).await;
    let result = h
        .client
        .call_tool(CallToolRequestParams::new("get_mcp_server_info"))
        .await
        .expect("call_tool should succeed");
    let text = content_text(&result);
    let body: Value = text
        .lines()
        .last()
        .and_then(|l| serde_json::from_str(l).ok())
        .expect("last content block should be the JSON body");

    assert_eq!(body["current_user"]["id"], 5);
    assert_eq!(body["current_user"]["login"], "alice");
    // `name` is `firstname lastname`, boundary-wrapped like every other
    // free-text field derived from Redmine content.
    let name = body["current_user"]["name"]
        .as_str()
        .expect("current_user.name should be a string");
    assert!(name.starts_with("<<<untrusted:"), "name: {name}");
    assert!(name.contains("Alice Example"), "name: {name}");
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

// --- Structured output, schemas, and annotations ---

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
            "{}'s outputSchema root must be \"type\": \"object\"",
            tool.name
        );
        let annotations = tool
            .annotations
            .as_ref()
            .unwrap_or_else(|| panic!("{} is missing annotations", tool.name));
        // A tool "mutates Redmine" (for annotation purposes) if it is
        // removed from the router entirely in read-only mode (`ALL`), or if
        // it has at least one write `action` gated internally rather than
        // by router removal (`PARTIAL_WRITE`) — either way
        // `read_only_hint` describes what the tool *can* do, not whether
        // read-only mode currently allows all of it.
        let is_write_tool = ruprogress_mcp::readonly::write_tools::ALL
            .contains(&tool.name.as_ref())
            || ruprogress_mcp::readonly::write_tools::PARTIAL_WRITE.contains(&tool.name.as_ref());
        assert_eq!(
            annotations.read_only_hint,
            Some(!is_write_tool),
            "{} should be annotated read_only_hint = {}",
            tool.name,
            !is_write_tool
        );
        if is_write_tool {
            assert_eq!(
                annotations.destructive_hint,
                Some(true),
                "{} mutates Redmine and should declare destructive_hint",
                tool.name
            );
        }
    }
}

#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "mostly repetitive wiremock mock registration, one per implemented tool"
)]
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
    Mock::given(method("GET"))
        .and(path("/trackers.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"trackers": []})))
        .mount(&h.redmine)
        .await;
    Mock::given(method("GET"))
        .and(path("/projects/1.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "project": {
                "id": 1, "name": "P", "identifier": "p",
                "created_on": "2026-01-01T00:00:00Z", "updated_on": "2026-01-01T00:00:00Z",
                "trackers": []
            }
        })))
        .mount(&h.redmine)
        .await;
    Mock::given(method("GET"))
        .and(path("/issue_statuses.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"issue_statuses": []})))
        .mount(&h.redmine)
        .await;
    Mock::given(method("GET"))
        .and(path("/enumerations/issue_priorities.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"issue_priorities": []})))
        .mount(&h.redmine)
        .await;
    Mock::given(method("GET"))
        .and(path("/users.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "users": [], "total_count": 0, "offset": 0, "limit": 25
        })))
        .mount(&h.redmine)
        .await;
    Mock::given(method("GET"))
        .and(path("/queries.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "queries": [], "total_count": 0, "offset": 0, "limit": 100
        })))
        .mount(&h.redmine)
        .await;
    Mock::given(method("GET"))
        .and(path("/custom_fields.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"custom_fields": []})))
        .mount(&h.redmine)
        .await;
    Mock::given(method("GET"))
        .and(path("/projects/1/versions.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "versions": [], "total_count": 0
        })))
        .mount(&h.redmine)
        .await;
    Mock::given(method("GET"))
        .and(path("/projects/1/memberships.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "memberships": [], "total_count": 0, "offset": 0, "limit": 100
        })))
        .mount(&h.redmine)
        .await;
    Mock::given(method("GET"))
        .and(path("/roles.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"roles": []})))
        .mount(&h.redmine)
        .await;
    Mock::given(method("GET"))
        .and(path("/projects/1/files.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"files": []})))
        .mount(&h.redmine)
        .await;
    // Covers every `summarize_project_status` sub-query (sample, open,
    // closed, created-in-period, updated-in-period) and `list_subtasks`/
    // `list_redmine_issues`: this test does not constrain query parameters,
    // so one mock serves all of them.
    Mock::given(method("GET"))
        .and(path("/issues.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "issues": [], "total_count": 0, "offset": 0, "limit": 100
        })))
        .mount(&h.redmine)
        .await;
    Mock::given(method("GET"))
        .and(path("/issues/1.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "issue": {
                "id": 1, "project": {"id": 1, "name": "P"}, "tracker": {"id": 1, "name": "Bug"},
                "status": {"id": 1, "name": "New"}, "priority": {"id": 1, "name": "Normal"},
                "author": {"id": 1, "name": "A"}, "subject": "s",
                "created_on": "2026-01-01T00:00:00Z", "updated_on": "2026-01-01T00:00:00Z"
            }
        })))
        .mount(&h.redmine)
        .await;
    // Empty results: `search_redmine_issues` short-circuits before hydrating,
    // so no `/issues.json?issue_id=...` mock is needed here.
    Mock::given(method("GET"))
        .and(path("/search.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "results": [], "total_count": 0, "offset": 0, "limit": 25
        })))
        .mount(&h.redmine)
        .await;
    Mock::given(method("GET"))
        .and(path("/time_entries.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "time_entries": [], "total_count": 0, "offset": 0, "limit": 25
        })))
        .mount(&h.redmine)
        .await;
    Mock::given(method("GET"))
        .and(path("/enumerations/time_entry_activities.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "time_entry_activities": []
        })))
        .mount(&h.redmine)
        .await;
    Mock::given(method("GET"))
        .and(path("/attachments/1.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "attachment": {
                "id": 1, "filename": "report.pdf", "filesize": 5,
                "content_url": format!("{}/attachments/download/1/report.pdf", h.redmine.uri()),
                "created_on": "2026-01-01T00:00:00Z"
            }
        })))
        .mount(&h.redmine)
        .await;
    Mock::given(method("GET"))
        .and(path("/attachments/download/1/report.pdf"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"hello".to_vec()))
        .mount(&h.redmine)
        .await;

    let tools = h
        .client
        .list_tools(None)
        .await
        .expect("list_tools should succeed");
    for tool in &tools.tools {
        // Write tools (`manage_*` with no read-only action) are exercised
        // in `tests/tools_projects.rs`/`tests/tools_time.rs`/
        // `tests/tools_issues_write.rs` instead, with real request/response
        // bodies per action. `PARTIAL_WRITE` tools (the mixed-action case)
        // are exercised in `tests/tools_issues_write.rs` too, since their
        // `action` parameter needs a value this loop does not supply.
        if ruprogress_mcp::readonly::write_tools::ALL.contains(&tool.name.as_ref())
            || ruprogress_mcp::readonly::write_tools::PARTIAL_WRITE.contains(&tool.name.as_ref())
        {
            continue;
        }
        let mut request = CallToolRequestParams::new(tool.name.clone());
        let project_id_only_tools = [
            "list_project_trackers",
            "list_project_issue_custom_fields",
            "summarize_project_status",
            "list_redmine_versions",
            "list_project_members",
            "get_project_modules",
            "get_gantt_chart",
            "list_files",
        ];
        let issue_id_only_tools = ["get_redmine_issue", "list_subtasks", "get_private_notes"];
        if project_id_only_tools.contains(&tool.name.as_ref()) {
            request.arguments = json!({"project_id": 1}).as_object().cloned();
        } else if issue_id_only_tools.contains(&tool.name.as_ref()) {
            request.arguments = json!({"issue_id": 1}).as_object().cloned();
        } else if tool.name.as_ref() == "get_redmine_attachment" {
            request.arguments = json!({"attachment_id": 1}).as_object().cloned();
        } else if ["search_redmine_issues", "search_entire_redmine"].contains(&tool.name.as_ref()) {
            request.arguments = json!({"query": "test"}).as_object().cloned();
        }
        let result = h
            .client
            .call_tool(request)
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

/// `tools/list` JSON (description + input schema + output schema per tool)
/// is a material share of a client's context budget. This is
/// a generous, not a tight, threshold on the total serialized size, so it
/// fails loud and early if a tool's descriptions or schemas balloon rather
/// than silently degrading context budgets. Revised upward each time new
/// tools or wider input schemas (like `manage_redmine_wiki_page`'s
/// six-action parameter set or `upload_file`'s/`uploads[]`'s multi-field
/// shapes) push the observed size close to the current limit; currently
/// 139 500 bytes for 41 tools (~138 557 observed after `update_redmine_issue`
/// gained `clear_fields` and its `ClearableField` enum), leaving headroom
/// for future growth at a similar per-tool rate.
#[tokio::test]
async fn tools_list_serialized_size_stays_under_the_baseline_threshold() {
    let h = support::harness(&[]).await;
    let tools = h
        .client
        .list_tools(None)
        .await
        .expect("list_tools should succeed");
    let bytes = serde_json::to_vec(&tools.tools).expect("tools/list result should serialize");
    assert!(
        bytes.len() < 139_500,
        "tools/list is {} bytes for {} tools; over the 139500 baseline threshold",
        bytes.len(),
        tools.tools.len()
    );
}

/// Same budget check with every implemented plugin flag on. A separate,
/// wider threshold: the default-flags test above is the one that guards the
/// common no-plugins deployment, and must not move as plugin families are
/// added.
#[tokio::test]
async fn tools_list_serialized_size_with_every_plugin_enabled_stays_under_a_wider_threshold() {
    let h = support::harness(&[
        ("REDMINE_CHECKLISTS_ENABLED", "true"),
        ("REDMINE_PRODUCTS_ENABLED", "true"),
        ("REDMINE_CRM_ENABLED", "true"),
        ("REDMINE_DMSF_ENABLED", "true"),
    ])
    .await;
    let tools = h
        .client
        .list_tools(None)
        .await
        .expect("list_tools should succeed");
    let bytes = serde_json::to_vec(&tools.tools).expect("tools/list result should serialize");
    // `manage_product`/`manage_contact` are the two widest schemas in the
    // server; `manage_document` adds a third action-dispatched schema.
    // Threshold raised from 160 000 to fit it —
    // re-measure and adjust here if a future change is a real regression.
    assert!(
        bytes.len() < 170_000,
        "tools/list is {} bytes for {} tools with every implemented plugin enabled; over the \
         170000 threshold",
        bytes.len(),
        tools.tools.len()
    );
}

/// The `format` vocabulary `ajv-formats` recognizes (the set MCP clients
/// such as opencode ship). Anything outside this list is either unknown to
/// Ajv strict mode (schemars' Rust-specific `uint*`/`int128`) or simply not
/// produced by this server's schemas; either way a new occurrence needs a
/// human decision, not a silent pass.
const ALLOWED_FORMATS: &[&str] = &[
    "date",
    "date-time",
    "time",
    "duration",
    "uri",
    "uri-reference",
    "email",
    "hostname",
    "ipv4",
    "ipv6",
    "uuid",
    "regex",
    "int32",
    "int64",
    "float",
    "double",
];

/// Recursively collect every `"format"` string found anywhere under `value`,
/// tagging each with a JSON-pointer-ish path for a useful failure message.
fn collect_formats(value: &Value, path: &str, out: &mut Vec<(String, String)>) {
    match value {
        Value::Object(map) => {
            if let Some(Value::String(format)) = map.get("format") {
                out.push((path.to_string(), format.clone()));
            }
            for (key, v) in map {
                collect_formats(v, &format!("{path}/{key}"), out);
            }
        }
        Value::Array(items) => {
            for (i, v) in items.iter().enumerate() {
                collect_formats(v, &format!("{path}/{i}"), out);
            }
        }
        _ => {}
    }
}

/// Regression test for `tools::schema`: every `format` string served in any
/// tool's `inputSchema`/`outputSchema` must be one Ajv strict mode (and thus
/// opencode) already understands. This is what actually catches a future
/// tool author who forgets to route a new struct through
/// `tools::schema::output`/`input` — reverting either call site in
/// `tools/discovery.rs` (in particular `list_project_trackers`'s
/// `input_schema` override, since the macro would otherwise auto-derive an
/// un-normalized schema) must fail this test.
#[tokio::test]
async fn every_served_schema_format_is_in_the_ajv_strict_mode_allowlist() {
    let h = support::harness(&[]).await;
    let tools = h
        .client
        .list_tools(None)
        .await
        .expect("list_tools should succeed");
    for tool in &tools.tools {
        let mut formats = Vec::new();
        collect_formats(
            &Value::Object(tool.input_schema.as_ref().clone()),
            "inputSchema",
            &mut formats,
        );
        if let Some(output_schema) = &tool.output_schema {
            collect_formats(
                &Value::Object(output_schema.as_ref().clone()),
                "outputSchema",
                &mut formats,
            );
        }
        for (path, format) in formats {
            assert!(
                ALLOWED_FORMATS.contains(&format.as_str()),
                "{}: {path} declares non-standard format {format:?}, not in ALLOWED_FORMATS",
                tool.name
            );
        }
    }
}

/// Recursively assert that no node under `value` has a `"$ref"` key, a
/// `"$defs"` key, or a `"type"` whose value is a JSON array — the three
/// shapes `REDMINE_MCP_SCHEMA_DIALECT=portable` (`tools::schema::to_portable`)
/// must remove from every served `inputSchema`. `path` names the offending
/// location for a useful failure message.
fn assert_portable(value: &Value, tool: &str, path: &str) {
    match value {
        Value::Object(map) => {
            assert!(
                !map.contains_key("$ref"),
                "{tool}: {path} still has \"$ref\""
            );
            assert!(
                !map.contains_key("$defs"),
                "{tool}: {path} still has \"$defs\""
            );
            if let Some(t) = map.get("type") {
                assert!(!t.is_array(), "{tool}: {path}/type is a JSON array: {t:?}");
            }
            for (key, v) in map {
                assert_portable(v, tool, &format!("{path}/{key}"));
            }
        }
        Value::Array(items) => {
            for (i, v) in items.iter().enumerate() {
                assert_portable(v, tool, &format!("{path}/{i}"));
            }
        }
        _ => {}
    }
}

#[tokio::test]
async fn portable_schema_dialect_serves_no_refs_defs_or_nullable_type_arrays() {
    let h = support::harness(&[("REDMINE_MCP_SCHEMA_DIALECT", "portable")]).await;
    let tools = h
        .client
        .list_tools(None)
        .await
        .expect("list_tools should succeed");
    assert!(!tools.tools.is_empty(), "expected at least one tool");
    for tool in &tools.tools {
        assert_portable(
            &Value::Object(tool.input_schema.as_ref().clone()),
            &tool.name,
            "inputSchema",
        );
    }
}

/// The flag is a flag, not a rename: the **default** dialect must still
/// serve the rich form (at least one tool's `inputSchema` still has
/// `$defs`), so the portable test above is proven to be testing something
/// that the default dialect does not already satisfy.
#[tokio::test]
async fn default_schema_dialect_still_serves_defs() {
    let h = support::harness(&[]).await;
    let tools = h
        .client
        .list_tools(None)
        .await
        .expect("list_tools should succeed");
    let any_has_defs = tools
        .tools
        .iter()
        .any(|tool| tool.input_schema.contains_key("$defs"));
    assert!(
        any_has_defs,
        "expected at least one tool's default-dialect inputSchema to still have $defs"
    );
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
