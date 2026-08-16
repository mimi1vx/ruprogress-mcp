//! `get_checklist`, `create_checklist_item`, `update_checklist_item`
//! (`RedmineUP` Checklists Pro plugin). Requires `REDMINE_CHECKLISTS_ENABLED`.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

mod support;

use rmcp::model::CallToolRequestParams;
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, ResponseTemplate};

fn checklists_harness_env() -> Vec<(&'static str, &'static str)> {
    vec![("REDMINE_CHECKLISTS_ENABLED", "true")]
}

#[tokio::test]
async fn get_checklist_happy_path() {
    let h = support::harness(&checklists_harness_env()).await;
    Mock::given(method("GET"))
        .and(path("/issues/1/checklists.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "checklists": [
                {"id": 1, "subject": "Write tests", "is_done": false, "is_section": false, "position": 1}
            ]
        })))
        .mount(&h.redmine)
        .await;

    let mut request = CallToolRequestParams::new("get_checklist");
    request.arguments = json!({"issue_id": 1}).as_object().cloned();
    let result = h
        .client
        .call_tool(request)
        .await
        .expect("get_checklist should be callable");
    assert_ne!(result.is_error, Some(true));
    let tools = h
        .client
        .list_tools(None)
        .await
        .expect("list_tools should succeed");
    let tool = tools
        .tools
        .iter()
        .find(|t| t.name == "get_checklist")
        .expect("get_checklist should be registered");
    support::assert_structured_content_matches_schema(
        &result,
        tool.output_schema.as_ref().expect("has an outputSchema"),
    );
    let structured = result.structured_content.expect("structured");
    assert_eq!(structured["total_count"], 1);
    assert!(
        structured["items"][0]["subject"]
            .as_str()
            .expect("subject should be a string")
            .contains("Write tests"),
        "subject should be boundary-wrapped but still contain the original text: {}",
        structured["items"][0]["subject"]
    );
}

#[tokio::test]
async fn get_checklist_not_found() {
    let h = support::harness(&checklists_harness_env()).await;
    Mock::given(method("GET"))
        .and(path("/issues/1/checklists.json"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&h.redmine)
        .await;

    let mut request = CallToolRequestParams::new("get_checklist");
    request.arguments = json!({"issue_id": 1}).as_object().cloned();
    let result = h
        .client
        .call_tool(request)
        .await
        .expect("call_tool should succeed at the protocol level");
    assert_eq!(result.is_error, Some(true));
    assert_eq!(
        result.structured_content.expect("structured")["code"],
        "NOT_FOUND"
    );
}

#[tokio::test]
async fn create_checklist_item_happy_path() {
    let h = support::harness(&checklists_harness_env()).await;
    Mock::given(method("POST"))
        .and(path("/issues/1/checklists.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"checklist": {"id": 7}})))
        .mount(&h.redmine)
        .await;

    let mut request = CallToolRequestParams::new("create_checklist_item");
    request.arguments = json!({"issue_id": 1, "subject": "Write tests"})
        .as_object()
        .cloned();
    let result = h
        .client
        .call_tool(request)
        .await
        .expect("create_checklist_item should be callable");
    assert_ne!(result.is_error, Some(true));
    let structured = result.structured_content.expect("structured");
    assert_eq!(structured["success"], true);
    assert_eq!(structured["checklist_item_id"], 7);
    assert!(
        structured["subject"]
            .as_str()
            .expect("subject should be a string")
            .contains("Write tests"),
        "subject should be boundary-wrapped but still contain the original text: {}",
        structured["subject"]
    );
    assert_eq!(structured["is_section"], false);
    assert_eq!(structured["is_done"], false);
}

#[tokio::test]
async fn update_checklist_item_happy_path() {
    let h = support::harness(&checklists_harness_env()).await;
    Mock::given(method("PUT"))
        .and(path("/checklists/5.json"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&h.redmine)
        .await;

    let mut request = CallToolRequestParams::new("update_checklist_item");
    request.arguments = json!({"checklist_item_id": 5, "is_done": true})
        .as_object()
        .cloned();
    let result = h
        .client
        .call_tool(request)
        .await
        .expect("update_checklist_item should be callable");
    assert_ne!(result.is_error, Some(true));
    let structured = result.structured_content.expect("structured");
    assert_eq!(structured["success"], true);
    assert_eq!(structured["checklist_item_id"], 5);
    assert_eq!(structured["updated_fields"], json!(["is_done"]));
}

#[tokio::test]
async fn create_checklist_item_rejects_a_blank_subject() {
    let h = support::harness(&checklists_harness_env()).await;
    let mut request = CallToolRequestParams::new("create_checklist_item");
    request.arguments = json!({"issue_id": 1, "subject": "   "})
        .as_object()
        .cloned();
    let result = h.client.call_tool(request).await;
    assert!(
        result.is_err(),
        "a blank subject should be a protocol-level argument error"
    );
}

#[tokio::test]
async fn create_checklist_item_rejects_position_zero() {
    let h = support::harness(&checklists_harness_env()).await;
    let mut request = CallToolRequestParams::new("create_checklist_item");
    request.arguments = json!({"issue_id": 1, "subject": "Write tests", "position": 0})
        .as_object()
        .cloned();
    let result = h.client.call_tool(request).await;
    assert!(
        result.is_err(),
        "position: 0 should be a protocol-level argument error"
    );
}

#[tokio::test]
async fn update_checklist_item_rejects_position_zero() {
    let h = support::harness(&checklists_harness_env()).await;
    let mut request = CallToolRequestParams::new("update_checklist_item");
    request.arguments = json!({"checklist_item_id": 5, "position": 0})
        .as_object()
        .cloned();
    let result = h.client.call_tool(request).await;
    assert!(
        result.is_err(),
        "position: 0 should be a protocol-level argument error"
    );
}

#[tokio::test]
async fn update_checklist_item_rejects_no_fields_to_change() {
    let h = support::harness(&checklists_harness_env()).await;
    let mut request = CallToolRequestParams::new("update_checklist_item");
    request.arguments = json!({"checklist_item_id": 5}).as_object().cloned();
    let result = h.client.call_tool(request).await;
    assert!(
        result.is_err(),
        "an argument-less update should be a protocol-level argument error"
    );
}
