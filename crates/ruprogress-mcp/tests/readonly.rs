//! Read-only mode: (i)/(ii) turn a stale or missing name in `write_tools::
//! ALL` into a build/test failure rather than a silent read-only-mode
//! bypass. (iii) proves the router mechanism itself
//! (`ToolRouter::remove_route`/`call`) returns a clean error rather than
//! panicking, using a tool name from `docs/tool-contract.md` that is not
//! registered yet — behaviourally identical to a route read-only mode will
//! eventually remove. (iv)/(v) cover per-action gating:
//! `manage_issue_relation`/`manage_issue_category` stay in the router in
//! read-only mode (their `list` action is a read), but their write actions
//! refuse with `code: "READ_ONLY"`.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

mod support;

use rmcp::model::CallToolRequestParams;
use ruprogress_mcp::readonly::write_tools;
use serde_json::json;

#[tokio::test]
async fn every_write_tool_name_exists_in_a_normal_router() {
    let h = support::harness(&[]).await;
    let tools = h
        .client
        .list_tools(None)
        .await
        .expect("list_tools should succeed");
    let names: Vec<&str> = tools.tools.iter().map(|t| t.name.as_ref()).collect();
    for name in write_tools::ALL {
        assert!(
            names.contains(name),
            "{name} in write_tools::ALL but missing from the router"
        );
    }
}

#[tokio::test]
async fn no_write_tool_name_exists_in_a_read_only_router() {
    let h = support::harness(&[("REDMINE_MCP_READ_ONLY", "true")]).await;
    let tools = h
        .client
        .list_tools(None)
        .await
        .expect("list_tools should succeed");
    let names: Vec<&str> = tools.tools.iter().map(|t| t.name.as_ref()).collect();
    for name in write_tools::ALL {
        assert!(
            !names.contains(name),
            "{name} still present in a read-only router"
        );
    }
}

#[tokio::test]
async fn calling_an_unregistered_tool_returns_a_clean_error_not_a_panic() {
    let h = support::harness(&[]).await;
    // `get_checklist` is a real future tool name (see
    // docs/tool-contract.md, the Checklist plugin family, not among the
    // tools implemented here) that does not exist in the router yet — the router's
    // response to it is identical to what it will return for a route
    // removed by read-only mode.
    let result = h
        .client
        .call_tool(CallToolRequestParams::new("get_checklist"))
        .await;
    assert!(
        result.is_err(),
        "calling an unknown tool should fail cleanly, not succeed"
    );
}

#[tokio::test]
async fn partial_write_tool_names_survive_a_read_only_router() {
    let h = support::harness(&[("REDMINE_MCP_READ_ONLY", "true")]).await;
    let tools = h
        .client
        .list_tools(None)
        .await
        .expect("list_tools should succeed");
    let names: Vec<&str> = tools.tools.iter().map(|t| t.name.as_ref()).collect();
    for name in write_tools::PARTIAL_WRITE {
        assert!(
            names.contains(name),
            "{name} is a partial-write tool and must stay in a read-only router"
        );
    }
}

#[tokio::test]
async fn manage_issue_relation_list_works_but_create_is_blocked_in_read_only_mode() {
    let h = support::harness(&[("REDMINE_MCP_READ_ONLY", "true")]).await;
    wiremock::Mock::given(wiremock::matchers::method("GET"))
        .and(wiremock::matchers::path("/issues/1/relations.json"))
        .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(json!({"relations": []})))
        .mount(&h.redmine)
        .await;

    let mut list_request = CallToolRequestParams::new("manage_issue_relation");
    list_request.arguments = json!({"action": "list", "issue_id": 1})
        .as_object()
        .cloned();
    let list_result = h
        .client
        .call_tool(list_request)
        .await
        .expect("action=\"list\" should be callable in read-only mode");
    assert_ne!(list_result.is_error, Some(true));

    let mut create_request = CallToolRequestParams::new("manage_issue_relation");
    create_request.arguments = json!({"action": "create", "issue_id": 1, "issue_to_id": 2})
        .as_object()
        .cloned();
    let create_result = h
        .client
        .call_tool(create_request)
        .await
        .expect("call_tool should succeed at the protocol level");
    assert_eq!(create_result.is_error, Some(true));
    assert_eq!(
        create_result.structured_content.expect("structured")["code"],
        "READ_ONLY"
    );
}

#[tokio::test]
async fn manage_redmine_wiki_page_list_works_but_create_is_blocked_in_read_only_mode() {
    let h = support::harness(&[("REDMINE_MCP_READ_ONLY", "true")]).await;
    wiremock::Mock::given(wiremock::matchers::method("GET"))
        .and(wiremock::matchers::path(
            "/projects/my-project/wiki/index.json",
        ))
        .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(json!({"wiki_pages": []})))
        .mount(&h.redmine)
        .await;

    let mut list_request = CallToolRequestParams::new("manage_redmine_wiki_page");
    list_request.arguments = json!({"action": "list", "project_id": "my-project"})
        .as_object()
        .cloned();
    let list_result = h
        .client
        .call_tool(list_request)
        .await
        .expect("action=\"list\" should be callable in read-only mode");
    assert_ne!(list_result.is_error, Some(true));

    let mut create_request = CallToolRequestParams::new("manage_redmine_wiki_page");
    create_request.arguments = json!({
        "action": "create", "project_id": "my-project",
        "wiki_page_title": "Home", "text": "hello"
    })
    .as_object()
    .cloned();
    let create_result = h
        .client
        .call_tool(create_request)
        .await
        .expect("call_tool should succeed at the protocol level");
    assert_eq!(create_result.is_error, Some(true));
    assert_eq!(
        create_result.structured_content.expect("structured")["code"],
        "READ_ONLY"
    );
}
