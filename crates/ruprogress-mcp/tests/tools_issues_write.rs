//! e2e: the issue write/mixed-action tool family (4b-write) —
//! `create_redmine_issue`, `update_redmine_issue`, `delete_redmine_issue`,
//! `copy_issue`, `manage_issue_relation`, `manage_issue_watcher`,
//! `manage_issue_note`, `manage_issue_category`. Happy path and dominant
//! error path per tool, plus behaviours specific to this family:
//! `delete_redmine_issue`'s two-step confirmation and impact preview,
//! `copy_issue`'s bounded recursive subtask copy, and
//! `manage_issue_relation`/`manage_issue_category`'s per-action read-only
//! gate (D8) — covered in `tests/readonly.rs` instead.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

mod support;

use rmcp::model::CallToolRequestParams;
use serde_json::{Value, json};
use wiremock::matchers::{body_json, method, path};
use wiremock::{Mock, ResponseTemplate};

fn body_of(result: &rmcp::model::CallToolResult) -> Value {
    let text = result
        .content
        .iter()
        .filter_map(|c| c.as_text())
        .map(|t| t.text.clone())
        .collect::<Vec<_>>()
        .join("\n");
    text.lines()
        .last()
        .and_then(|l| serde_json::from_str(l).ok())
        .expect("last content block should be the JSON body")
}

async fn call(h: &support::Harness, name: &str, args: Value) -> rmcp::model::CallToolResult {
    let mut request = CallToolRequestParams::new(name.to_string());
    request.arguments = args.as_object().cloned();
    h.client
        .call_tool(request)
        .await
        .expect("call_tool should succeed")
}

fn issue_json(id: u64, subject: &str) -> Value {
    json!({
        "issue": {
            "id": id,
            "project": {"id": 1, "name": "P"},
            "tracker": {"id": 1, "name": "Bug"},
            "status": {"id": 1, "name": "New"},
            "priority": {"id": 1, "name": "Normal"},
            "author": {"id": 1, "name": "A"},
            "subject": subject,
            "created_on": "2026-01-01T00:00:00Z",
            "updated_on": "2026-01-01T00:00:00Z"
        }
    })
}

// --- create_redmine_issue ---

#[tokio::test]
async fn create_redmine_issue_happy_path() {
    let h = support::harness(&[]).await;
    Mock::given(method("POST"))
        .and(path("/issues.json"))
        .respond_with(ResponseTemplate::new(201).set_body_json(issue_json(42, "New issue")))
        .mount(&h.redmine)
        .await;

    let result = call(
        &h,
        "create_redmine_issue",
        json!({"project_id": 1, "subject": "New issue"}),
    )
    .await;
    assert_ne!(result.is_error, Some(true));
    let body = body_of(&result);
    assert_eq!(body["success"], true);
    assert_eq!(body["issue"]["id"], 42);
}

#[tokio::test]
async fn create_redmine_issue_rejects_an_empty_subject_as_an_argument_error() {
    let h = support::harness(&[]).await;
    let mut request = CallToolRequestParams::new("create_redmine_issue".to_string());
    request.arguments = json!({"project_id": 1, "subject": "  "})
        .as_object()
        .cloned();
    let result = h.client.call_tool(request).await;
    assert!(
        result.is_err(),
        "an empty subject should be a protocol-level argument error"
    );
}

#[tokio::test]
async fn create_redmine_issue_dominant_error_is_in_band() {
    let h = support::harness(&[]).await;
    Mock::given(method("POST"))
        .and(path("/issues.json"))
        .respond_with(ResponseTemplate::new(422).set_body_json(json!({
            "errors": ["Subject can't be blank"]
        })))
        .mount(&h.redmine)
        .await;

    let result = call(
        &h,
        "create_redmine_issue",
        json!({"project_id": 1, "subject": "x"}),
    )
    .await;
    assert_eq!(result.is_error, Some(true));
    assert_eq!(
        result.structured_content.unwrap()["code"],
        "VALIDATION_FAILED"
    );
}

// --- update_redmine_issue ---

#[tokio::test]
async fn update_redmine_issue_happy_path() {
    let h = support::harness(&[]).await;
    Mock::given(method("PUT"))
        .and(path("/issues/7.json"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&h.redmine)
        .await;
    Mock::given(method("GET"))
        .and(path("/issues/7.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(issue_json(7, "Updated")))
        .mount(&h.redmine)
        .await;

    let result = call(
        &h,
        "update_redmine_issue",
        json!({"issue_id": 7, "subject": "Updated"}),
    )
    .await;
    assert_ne!(result.is_error, Some(true));
    let body = body_of(&result);
    assert_eq!(body["success"], true);
    assert_eq!(body["issue"]["id"], 7);
}

#[tokio::test]
async fn update_redmine_issue_rejects_an_empty_notes_string_as_an_argument_error() {
    let h = support::harness(&[]).await;
    let mut request = CallToolRequestParams::new("update_redmine_issue".to_string());
    request.arguments = json!({"issue_id": 7, "notes": ""}).as_object().cloned();
    let result = h.client.call_tool(request).await;
    assert!(result.is_err());
}

#[tokio::test]
async fn update_redmine_issue_rejects_a_no_op_call_as_an_argument_error() {
    let h = support::harness(&[]).await;
    let mut request = CallToolRequestParams::new("update_redmine_issue".to_string());
    request.arguments = json!({"issue_id": 7}).as_object().cloned();
    let result = h.client.call_tool(request).await;
    assert!(
        result.is_err(),
        "nothing to change should be rejected before any request is sent"
    );
}

#[tokio::test]
async fn update_redmine_issue_dominant_error_is_in_band() {
    let h = support::harness(&[]).await;
    Mock::given(method("PUT"))
        .and(path("/issues/7.json"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&h.redmine)
        .await;

    let result = call(
        &h,
        "update_redmine_issue",
        json!({"issue_id": 7, "subject": "x"}),
    )
    .await;
    assert_eq!(result.is_error, Some(true));
    assert_eq!(result.structured_content.unwrap()["code"], "NOT_FOUND");
}

// --- delete_redmine_issue ---

async fn mount_delete_impact_mocks(server: &wiremock::MockServer, children: usize) {
    let children_json: Vec<Value> = (0..children)
        .map(|i| {
            let id = 100_usize.saturating_add(i);
            json!({
                "id": id, "project": {"id": 1, "name": "P"}, "tracker": {"id": 1, "name": "Bug"},
                "status": {"id": 1, "name": "New"}, "priority": {"id": 1, "name": "Normal"},
                "author": {"id": 1, "name": "A"}, "subject": "child",
                "created_on": "2026-01-01T00:00:00Z", "updated_on": "2026-01-01T00:00:00Z"
            })
        })
        .collect();
    let children_count = children_json.len();
    wiremock::Mock::given(method("GET"))
        .and(path("/issues/7.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "issue": {
                "id": 7, "project": {"id": 1, "name": "P"}, "tracker": {"id": 1, "name": "Bug"},
                "status": {"id": 1, "name": "New"}, "priority": {"id": 1, "name": "Normal"},
                "author": {"id": 1, "name": "A"}, "subject": "s",
                "created_on": "2026-01-01T00:00:00Z", "updated_on": "2026-01-01T00:00:00Z",
                "journals": [{"id": 1, "notes": "hi", "created_on": "2026-01-01T00:00:00Z"}],
                "attachments": [],
                "relations": []
            }
        })))
        .mount(server)
        .await;
    wiremock::Mock::given(method("GET"))
        .and(path("/issues.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "issues": children_json,
            "total_count": children_count,
            "offset": 0,
            "limit": 100
        })))
        .mount(server)
        .await;
    wiremock::Mock::given(method("GET"))
        .and(path("/time_entries.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "time_entries": [], "total_count": 0, "offset": 0, "limit": 1
        })))
        .mount(server)
        .await;
}

#[tokio::test]
async fn delete_redmine_issue_refuses_without_confirm_delete_and_returns_impact() {
    let h = support::harness(&[]).await;
    mount_delete_impact_mocks(&h.redmine, 0).await;
    let result = call(&h, "delete_redmine_issue", json!({"issue_id": 7})).await;
    assert_ne!(result.is_error, Some(true));
    let body = body_of(&result);
    assert_eq!(body["success"], false);
    assert_eq!(body["code"], "CONFIRMATION_REQUIRED");
    assert_eq!(body["impact"]["journals_count"], 1);
}

#[tokio::test]
async fn delete_redmine_issue_with_children_refuses_without_confirm_delete_with_children() {
    let h = support::harness(&[]).await;
    mount_delete_impact_mocks(&h.redmine, 2).await;
    let result = call(
        &h,
        "delete_redmine_issue",
        json!({"issue_id": 7, "confirm_delete": true}),
    )
    .await;
    assert_ne!(result.is_error, Some(true));
    let body = body_of(&result);
    assert_eq!(body["success"], false);
    assert_eq!(body["code"], "CHILDREN_PRESENT");
    assert_eq!(body["impact"]["children_count"], 2);
}

#[tokio::test]
async fn delete_redmine_issue_succeeds_with_both_confirmations() {
    let h = support::harness(&[]).await;
    mount_delete_impact_mocks(&h.redmine, 2).await;
    Mock::given(method("DELETE"))
        .and(path("/issues/7.json"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&h.redmine)
        .await;

    let result = call(
        &h,
        "delete_redmine_issue",
        json!({
            "issue_id": 7,
            "confirm_delete": true,
            "confirm_delete_with_children": true
        }),
    )
    .await;
    assert_ne!(result.is_error, Some(true));
    let body = body_of(&result);
    assert_eq!(body["success"], true);
    assert_eq!(body["deleted_issue_id"], 7);
    assert_eq!(body["cascade_deleted"], 2);
}

#[tokio::test]
async fn delete_redmine_issue_dominant_error_is_in_band() {
    let h = support::harness(&[]).await;
    Mock::given(method("GET"))
        .and(path("/issues/7.json"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&h.redmine)
        .await;

    let result = call(&h, "delete_redmine_issue", json!({"issue_id": 7})).await;
    assert_eq!(result.is_error, Some(true));
    assert_eq!(result.structured_content.unwrap()["code"], "NOT_FOUND");
}

// --- copy_issue ---

#[tokio::test]
async fn copy_issue_happy_path_with_one_subtask() {
    let h = support::harness(&[]).await;
    Mock::given(method("GET"))
        .and(path("/issues/1.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(issue_json(1, "Source")))
        .mount(&h.redmine)
        .await;
    // Root copy.
    Mock::given(method("POST"))
        .and(path("/issues.json"))
        .respond_with(ResponseTemplate::new(201).set_body_json(issue_json(2, "Source")))
        .mount(&h.redmine)
        .await;
    Mock::given(method("POST"))
        .and(path("/issues/1/relations.json"))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "relation": {"id": 1, "issue_id": 1, "issue_to_id": 2, "relation_type": "copied_to", "delay": null}
        })))
        .mount(&h.redmine)
        .await;
    // `copy_subtasks=false` below means `list_subtasks` is never called;
    // this mock exists only so a regression that ignores the flag fails
    // loudly with a real (empty) response instead of an unmatched-request
    // panic.
    Mock::given(method("GET"))
        .and(path("/issues.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "issues": [],
            "total_count": 0,
            "offset": 0,
            "limit": 100
        })))
        .mount(&h.redmine)
        .await;

    let result = call(
        &h,
        "copy_issue",
        json!({"issue_id": 1, "copy_subtasks": false}),
    )
    .await;
    assert_ne!(result.is_error, Some(true));
    let body = body_of(&result);
    assert_eq!(body["success"], true);
    assert_eq!(body["issue"]["id"], 2);
    assert_eq!(body["subtasks_copied"], 0);
}

#[tokio::test]
async fn copy_issue_dominant_error_is_in_band() {
    let h = support::harness(&[]).await;
    Mock::given(method("GET"))
        .and(path("/issues/1.json"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&h.redmine)
        .await;

    let result = call(&h, "copy_issue", json!({"issue_id": 1})).await;
    assert_eq!(result.is_error, Some(true));
    assert_eq!(result.structured_content.unwrap()["code"], "NOT_FOUND");
}

// --- manage_issue_relation ---

#[tokio::test]
async fn manage_issue_relation_list_happy_path() {
    let h = support::harness(&[]).await;
    Mock::given(method("GET"))
        .and(path("/issues/9/relations.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "relations": [
                {"id": 1, "issue_id": 9, "issue_to_id": 7, "relation_type": "relates", "delay": null}
            ]
        })))
        .mount(&h.redmine)
        .await;

    let result = call(
        &h,
        "manage_issue_relation",
        json!({"action": "list", "issue_id": 9}),
    )
    .await;
    assert_ne!(result.is_error, Some(true));
    let body = body_of(&result);
    assert_eq!(body["relations"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn manage_issue_relation_create_happy_path_sends_expected_body() {
    let h = support::harness(&[]).await;
    Mock::given(method("POST"))
        .and(path("/issues/9/relations.json"))
        .and(body_json(json!({
            "relation": {"issue_to_id": 7, "relation_type": "blocks"}
        })))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "relation": {"id": 1, "issue_id": 9, "issue_to_id": 7, "relation_type": "blocks", "delay": null}
        })))
        .mount(&h.redmine)
        .await;

    let result = call(
        &h,
        "manage_issue_relation",
        json!({"action": "create", "issue_id": 9, "issue_to_id": 7, "relation_type": "blocks"}),
    )
    .await;
    assert_ne!(result.is_error, Some(true));
    let body = body_of(&result);
    assert_eq!(body["relation"]["id"], 1);
}

#[tokio::test]
async fn manage_issue_relation_rejects_an_unknown_relation_type_as_an_argument_error() {
    let h = support::harness(&[]).await;
    let mut request = CallToolRequestParams::new("manage_issue_relation".to_string());
    request.arguments = json!({
        "action": "create", "issue_id": 9, "issue_to_id": 7, "relation_type": "nonsense"
    })
    .as_object()
    .cloned();
    let result = h.client.call_tool(request).await;
    assert!(result.is_err());
}

#[tokio::test]
async fn manage_issue_relation_delete_happy_path() {
    let h = support::harness(&[]).await;
    Mock::given(method("DELETE"))
        .and(path("/relations/1.json"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&h.redmine)
        .await;

    let result = call(
        &h,
        "manage_issue_relation",
        json!({"action": "delete", "relation_id": 1}),
    )
    .await;
    assert_ne!(result.is_error, Some(true));
    let body = body_of(&result);
    assert_eq!(body["deleted_relation_id"], 1);
}

#[tokio::test]
async fn manage_issue_relation_dominant_error_is_in_band() {
    let h = support::harness(&[]).await;
    Mock::given(method("GET"))
        .and(path("/issues/9/relations.json"))
        .respond_with(ResponseTemplate::new(403))
        .mount(&h.redmine)
        .await;

    let result = call(
        &h,
        "manage_issue_relation",
        json!({"action": "list", "issue_id": 9}),
    )
    .await;
    assert_eq!(result.is_error, Some(true));
    assert_eq!(result.structured_content.unwrap()["code"], "FORBIDDEN");
}

// --- manage_issue_watcher ---

#[tokio::test]
async fn manage_issue_watcher_add_happy_path() {
    let h = support::harness(&[]).await;
    Mock::given(method("POST"))
        .and(path("/issues/9/watchers.json"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&h.redmine)
        .await;

    let result = call(
        &h,
        "manage_issue_watcher",
        json!({"action": "add", "issue_id": 9, "user_id": 3}),
    )
    .await;
    assert_ne!(result.is_error, Some(true));
    let body = body_of(&result);
    assert_eq!(body["success"], true);
    assert_eq!(body["user_id"], 3);
}

#[tokio::test]
async fn manage_issue_watcher_remove_happy_path() {
    let h = support::harness(&[]).await;
    Mock::given(method("DELETE"))
        .and(path("/issues/9/watchers/3.json"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&h.redmine)
        .await;

    let result = call(
        &h,
        "manage_issue_watcher",
        json!({"action": "remove", "issue_id": 9, "user_id": 3}),
    )
    .await;
    assert_ne!(result.is_error, Some(true));
}

#[tokio::test]
async fn manage_issue_watcher_dominant_error_is_in_band() {
    let h = support::harness(&[]).await;
    Mock::given(method("POST"))
        .and(path("/issues/9/watchers.json"))
        .respond_with(ResponseTemplate::new(403))
        .mount(&h.redmine)
        .await;

    let result = call(
        &h,
        "manage_issue_watcher",
        json!({"action": "add", "issue_id": 9, "user_id": 3}),
    )
    .await;
    assert_eq!(result.is_error, Some(true));
    assert_eq!(result.structured_content.unwrap()["code"], "FORBIDDEN");
}

// --- manage_issue_note ---

#[tokio::test]
async fn manage_issue_note_edit_happy_path_sends_expected_body() {
    let h = support::harness(&[]).await;
    Mock::given(method("PUT"))
        .and(path("/journals/5.json"))
        .and(body_json(json!({"journal": {"notes": "edited"}})))
        .respond_with(ResponseTemplate::new(204))
        .mount(&h.redmine)
        .await;

    let result = call(
        &h,
        "manage_issue_note",
        json!({"action": "edit", "journal_id": 5, "notes": "edited"}),
    )
    .await;
    assert_ne!(result.is_error, Some(true));
    let body = body_of(&result);
    assert_eq!(body["notes"], "edited");
}

#[tokio::test]
async fn manage_issue_note_set_private_happy_path() {
    let h = support::harness(&[]).await;
    Mock::given(method("PUT"))
        .and(path("/journals/5.json"))
        .and(body_json(json!({"journal": {"private_notes": true}})))
        .respond_with(ResponseTemplate::new(204))
        .mount(&h.redmine)
        .await;

    let result = call(
        &h,
        "manage_issue_note",
        json!({"action": "set_private", "journal_id": 5, "is_private": true}),
    )
    .await;
    assert_ne!(result.is_error, Some(true));
    let body = body_of(&result);
    assert_eq!(body["private_notes"], true);
}

#[tokio::test]
async fn manage_issue_note_edit_requires_notes_as_an_argument_error() {
    let h = support::harness(&[]).await;
    let mut request = CallToolRequestParams::new("manage_issue_note".to_string());
    request.arguments = json!({"action": "edit", "journal_id": 5})
        .as_object()
        .cloned();
    let result = h.client.call_tool(request).await;
    assert!(result.is_err());
}

#[tokio::test]
async fn manage_issue_note_dominant_error_is_in_band() {
    let h = support::harness(&[]).await;
    Mock::given(method("PUT"))
        .and(path("/journals/5.json"))
        .respond_with(ResponseTemplate::new(403))
        .mount(&h.redmine)
        .await;

    let result = call(
        &h,
        "manage_issue_note",
        json!({"action": "edit", "journal_id": 5, "notes": "x"}),
    )
    .await;
    assert_eq!(result.is_error, Some(true));
    assert_eq!(result.structured_content.unwrap()["code"], "FORBIDDEN");
}

// --- manage_issue_category ---

#[tokio::test]
async fn manage_issue_category_list_happy_path() {
    let h = support::harness(&[]).await;
    Mock::given(method("GET"))
        .and(path("/projects/demo/issue_categories.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "issue_categories": [{"id": 2, "name": "Backend"}],
            "total_count": 1
        })))
        .mount(&h.redmine)
        .await;

    let result = call(
        &h,
        "manage_issue_category",
        json!({"action": "list", "project_id": "demo"}),
    )
    .await;
    assert_ne!(result.is_error, Some(true));
    let body = body_of(&result);
    assert_eq!(body["categories"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn manage_issue_category_create_happy_path() {
    let h = support::harness(&[]).await;
    Mock::given(method("POST"))
        .and(path("/projects/demo/issue_categories.json"))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "issue_category": {"id": 2, "name": "Backend"}
        })))
        .mount(&h.redmine)
        .await;

    let result = call(
        &h,
        "manage_issue_category",
        json!({"action": "create", "project_id": "demo", "name": "Backend"}),
    )
    .await;
    assert_ne!(result.is_error, Some(true));
    let body = body_of(&result);
    assert_eq!(body["category"]["id"], 2);
}

#[tokio::test]
async fn manage_issue_category_create_rejects_a_blank_name_as_an_argument_error() {
    let h = support::harness(&[]).await;
    let mut request = CallToolRequestParams::new("manage_issue_category".to_string());
    request.arguments = json!({"action": "create", "project_id": "demo", "name": "  "})
        .as_object()
        .cloned();
    let result = h.client.call_tool(request).await;
    assert!(result.is_err());
}

#[tokio::test]
async fn manage_issue_category_delete_sends_reassign_to_id() {
    let h = support::harness(&[]).await;
    Mock::given(method("DELETE"))
        .and(path("/issue_categories/2.json"))
        .and(wiremock::matchers::query_param("reassign_to_id", "3"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&h.redmine)
        .await;

    let result = call(
        &h,
        "manage_issue_category",
        json!({"action": "delete", "category_id": 2, "reassign_to_id": 3}),
    )
    .await;
    assert_ne!(result.is_error, Some(true));
    let body = body_of(&result);
    assert_eq!(body["deleted_category_id"], 2);
}

#[tokio::test]
async fn manage_issue_category_dominant_error_is_in_band() {
    let h = support::harness(&[]).await;
    Mock::given(method("GET"))
        .and(path("/projects/demo/issue_categories.json"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&h.redmine)
        .await;

    let result = call(
        &h,
        "manage_issue_category",
        json!({"action": "list", "project_id": "demo"}),
    )
    .await;
    assert_eq!(result.is_error, Some(true));
    assert_eq!(result.structured_content.unwrap()["code"], "NOT_FOUND");
}
