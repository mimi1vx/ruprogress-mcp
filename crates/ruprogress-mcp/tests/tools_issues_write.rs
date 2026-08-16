//! e2e: the issue write/mixed-action tool family —
//! `create_redmine_issue`, `update_redmine_issue`, `delete_redmine_issue`,
//! `copy_issue`, `manage_issue_relation`, `manage_issue_watcher`,
//! `manage_issue_note`, `manage_issue_category`. Happy path and dominant
//! error path per tool, plus behaviours specific to this family:
//! `delete_redmine_issue`'s two-step confirmation and impact preview,
//! `copy_issue`'s bounded recursive subtask copy, and
//! `manage_issue_relation`/`manage_issue_category`'s per-action read-only
//! gate — covered in `tests/readonly.rs` instead.
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

fn base64_of(bytes: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

async fn mock_upload_token(server: &wiremock::MockServer, id: u64, token: &str) {
    Mock::given(method("POST"))
        .and(path("/uploads.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "upload": {"id": id, "token": token}
        })))
        .mount(server)
        .await;
}

async fn mock_attachment_metadata(server: &wiremock::MockServer, id: u64, filename: &str) {
    Mock::given(method("GET"))
        .and(path(format!("/attachments/{id}.json")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "attachment": {
                "id": id, "filename": filename, "filesize": 11,
                "content_type": "text/plain",
                "content_url": format!("{}/attachments/download/{id}/{filename}", server.uri()),
                "created_on": "2026-01-01T00:00:00Z"
            }
        })))
        .mount(server)
        .await;
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
async fn create_redmine_issue_without_uploads_sends_a_byte_identical_body() {
    let h = support::harness(&[]).await;
    Mock::given(method("POST"))
        .and(path("/issues.json"))
        .and(body_json(json!({
            "issue": {"project_id": "1", "subject": "New issue"}
        })))
        .respond_with(ResponseTemplate::new(201).set_body_json(issue_json(42, "New issue")))
        .mount(&h.redmine)
        .await;

    let result = call(
        &h,
        "create_redmine_issue",
        json!({"project_id": 1, "subject": "New issue"}),
    )
    .await;
    assert_ne!(
        result.is_error,
        Some(true),
        "{:?}",
        result.structured_content
    );
}

#[tokio::test]
async fn create_redmine_issue_with_uploads_happy_path() {
    let h = support::harness(&[]).await;
    mock_upload_token(&h.redmine, 99, "99.token").await;
    Mock::given(method("POST"))
        .and(path("/issues.json"))
        .and(body_json(json!({
            "issue": {
                "project_id": "1",
                "subject": "New issue",
                "uploads": [{"token": "99.token"}]
            }
        })))
        .respond_with(ResponseTemplate::new(201).set_body_json(issue_json(42, "New issue")))
        .mount(&h.redmine)
        .await;
    mock_attachment_metadata(&h.redmine, 99, "notes.txt").await;

    let result = call(
        &h,
        "create_redmine_issue",
        json!({
            "project_id": 1,
            "subject": "New issue",
            "uploads": [{"filename": "notes.txt", "content_base64": base64_of(b"hello world")}]
        }),
    )
    .await;
    assert_ne!(
        result.is_error,
        Some(true),
        "{:?}",
        result.structured_content
    );
    let body = body_of(&result);
    assert_eq!(body["issue"]["attachments"][0]["id"], 99);
    assert!(
        body["issue"]["attachments"][0]["filename"]
            .as_str()
            .unwrap()
            .contains("notes.txt")
    );
}

#[tokio::test]
async fn create_redmine_issue_uploads_with_invalid_arity_sends_no_upload_requests() {
    let h = support::harness(&[]).await;
    let result = call(
        &h,
        "create_redmine_issue",
        json!({
            "project_id": 1,
            "subject": "New issue",
            "uploads": [{
                "content_base64": base64_of(b"x"),
                "file_path": "/tmp/x",
                "filename": "x.txt"
            }]
        }),
    )
    .await;
    assert_eq!(result.is_error, Some(true));
    assert_eq!(
        result.structured_content.unwrap()["code"],
        "SOURCE_REQUIRED"
    );
    let requests = h.redmine.received_requests().await.unwrap_or_default();
    assert!(
        requests.is_empty(),
        "no request should reach Redmine when upload validation fails: {requests:?}"
    );
}

#[tokio::test]
async fn create_redmine_issue_uploads_with_source_url_is_unsupported_source() {
    let h = support::harness(&[]).await;
    let result = call(
        &h,
        "create_redmine_issue",
        json!({
            "project_id": 1,
            "subject": "New issue",
            "uploads": [{"source_url": "https://example.com/a.pdf"}]
        }),
    )
    .await;
    assert_eq!(result.is_error, Some(true));
    assert_eq!(
        result.structured_content.unwrap()["code"],
        "UNSUPPORTED_SOURCE"
    );
}

#[tokio::test]
async fn create_redmine_issue_uploads_content_base64_without_filename_is_a_protocol_error() {
    let h = support::harness(&[]).await;
    let mut request = CallToolRequestParams::new("create_redmine_issue".to_string());
    request.arguments = json!({
        "project_id": 1,
        "subject": "New issue",
        "uploads": [{"content_base64": base64_of(b"x")}]
    })
    .as_object()
    .cloned();
    let result = h.client.call_tool(request).await;
    assert!(result.is_err());
}

#[tokio::test]
async fn create_redmine_issue_uploads_over_ten_items_is_a_protocol_error() {
    let h = support::harness(&[]).await;
    let uploads: Vec<Value> = (0..11)
        .map(|i| json!({"content_base64": base64_of(b"x"), "filename": format!("f{i}.txt")}))
        .collect();
    let mut request = CallToolRequestParams::new("create_redmine_issue".to_string());
    request.arguments = json!({
        "project_id": 1,
        "subject": "New issue",
        "uploads": uploads
    })
    .as_object()
    .cloned();
    let result = h.client.call_tool(request).await;
    assert!(result.is_err());
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

// --- create_redmine_issue: AlphaNodes additional_tags plugin fields ---

#[tokio::test]
async fn create_redmine_issue_tag_list_with_flag_off_is_misconfigured_with_zero_requests() {
    let h = support::harness(&[]).await;

    let result = call(
        &h,
        "create_redmine_issue",
        json!({"project_id": 1, "subject": "New issue", "tag_list": ["a"]}),
    )
    .await;
    assert_eq!(result.is_error, Some(true));
    let structured = result.structured_content.unwrap();
    assert_eq!(structured["code"], "MISCONFIGURED");
    assert!(
        structured["hint"]
            .as_str()
            .unwrap()
            .contains("REDMINE_TAGS_ENABLED")
    );
    let requests = h.redmine.received_requests().await.unwrap_or_default();
    assert!(
        requests.is_empty(),
        "no request should reach Redmine: {requests:?}"
    );
}

#[tokio::test]
async fn create_redmine_issue_sends_tag_list_when_flag_is_on() {
    let h = support::harness(&[("REDMINE_TAGS_ENABLED", "true")]).await;
    Mock::given(method("POST"))
        .and(path("/issues.json"))
        .and(body_json(json!({
            "issue": {"project_id": "1", "subject": "New issue", "tag_list": ["a", "b"]}
        })))
        .respond_with(ResponseTemplate::new(201).set_body_json(issue_json(42, "New issue")))
        .mount(&h.redmine)
        .await;

    let result = call(
        &h,
        "create_redmine_issue",
        json!({"project_id": 1, "subject": "New issue", "tag_list": ["a", "b"]}),
    )
    .await;
    assert_ne!(
        result.is_error,
        Some(true),
        "{:?}",
        result.structured_content
    );
}

#[tokio::test]
async fn create_redmine_issue_rejects_a_comma_containing_tag_naming_the_array_form() {
    let h = support::harness(&[("REDMINE_TAGS_ENABLED", "true")]).await;
    let mut request = CallToolRequestParams::new("create_redmine_issue".to_string());
    request.arguments = json!({
        "project_id": 1, "subject": "New issue", "tag_list": ["a,b"]
    })
    .as_object()
    .cloned();
    let result = h.client.call_tool(request).await;
    assert!(result.is_err());
}

#[tokio::test]
async fn create_redmine_issue_rejects_a_blank_after_trim_tag() {
    let h = support::harness(&[("REDMINE_TAGS_ENABLED", "true")]).await;
    let mut request = CallToolRequestParams::new("create_redmine_issue".to_string());
    request.arguments = json!({
        "project_id": 1, "subject": "New issue", "tag_list": [" "]
    })
    .as_object()
    .cloned();
    let result = h.client.call_tool(request).await;
    assert!(result.is_err());
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
async fn update_redmine_issue_without_uploads_sends_a_byte_identical_body() {
    let h = support::harness(&[]).await;
    Mock::given(method("PUT"))
        .and(path("/issues/7.json"))
        .and(body_json(json!({"issue": {"subject": "Updated"}})))
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
    assert_ne!(
        result.is_error,
        Some(true),
        "{:?}",
        result.structured_content
    );
}

#[tokio::test]
async fn update_redmine_issue_with_uploads_only_is_not_a_no_op() {
    let h = support::harness(&[]).await;
    mock_upload_token(&h.redmine, 55, "55.token").await;
    Mock::given(method("PUT"))
        .and(path("/issues/7.json"))
        .and(body_json(json!({
            "issue": {"uploads": [{"token": "55.token"}]}
        })))
        .respond_with(ResponseTemplate::new(204))
        .mount(&h.redmine)
        .await;
    Mock::given(method("GET"))
        .and(path("/issues/7.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(issue_json(7, "Existing")))
        .mount(&h.redmine)
        .await;
    mock_attachment_metadata(&h.redmine, 55, "report.pdf").await;

    let result = call(
        &h,
        "update_redmine_issue",
        json!({
            "issue_id": 7,
            "uploads": [{"filename": "report.pdf", "content_base64": base64_of(b"hello world")}]
        }),
    )
    .await;
    assert_ne!(
        result.is_error,
        Some(true),
        "{:?}",
        result.structured_content
    );
    let body = body_of(&result);
    assert_eq!(body["issue"]["attachments"][0]["id"], 55);
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

// --- update_redmine_issue: RedmineUP Agile plugin fields ---

#[tokio::test]
async fn update_redmine_issue_agile_param_with_flag_off_is_misconfigured_with_zero_requests() {
    let h = support::harness(&[]).await;

    let result = call(
        &h,
        "update_redmine_issue",
        json!({"issue_id": 7, "story_points": 8}),
    )
    .await;
    assert_eq!(result.is_error, Some(true));
    let structured = result.structured_content.unwrap();
    assert_eq!(structured["code"], "MISCONFIGURED");
    assert!(
        structured["hint"]
            .as_str()
            .unwrap()
            .contains("REDMINE_AGILE_ENABLED")
    );
    let requests = h.redmine.received_requests().await.unwrap_or_default();
    assert!(
        requests.is_empty(),
        "no request should reach Redmine: {requests:?}"
    );
}

#[tokio::test]
async fn update_redmine_issue_agile_only_update_skips_the_core_put_and_preserves_other_fields() {
    let h = support::harness(&[("REDMINE_AGILE_ENABLED", "true")]).await;
    Mock::given(method("GET"))
        .and(path("/issues/7/agile_data.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "agile_data": {"id": 9, "story_points": 8, "agile_sprint_id": 3, "position": 2}
        })))
        .up_to_n_times(1)
        .mount(&h.redmine)
        .await;
    // The load-bearing assertion: `story_points`/`position` survive a
    // sprint-only change. A payload that dropped them here would be the
    // replace-vs-merge bug this whole feature exists to avoid.
    Mock::given(method("PUT"))
        .and(path("/issues/7.json"))
        .and(body_json(json!({
            "issue": {"agile_data_attributes": {
                "id": 9, "story_points": 8, "agile_sprint_id": 7, "position": 2
            }}
        })))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&h.redmine)
        .await;
    Mock::given(method("GET"))
        .and(path("/issues/7/agile_data.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "agile_data": {"id": 9, "story_points": 8, "agile_sprint_id": 7, "position": 2}
        })))
        .mount(&h.redmine)
        .await;
    Mock::given(method("GET"))
        .and(path("/issues/7.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(issue_json(7, "Existing")))
        .mount(&h.redmine)
        .await;

    let result = call(
        &h,
        "update_redmine_issue",
        json!({"issue_id": 7, "agile_sprint_id": 7}),
    )
    .await;
    assert_ne!(
        result.is_error,
        Some(true),
        "{:?}",
        result.structured_content
    );
    let body = body_of(&result);
    assert_eq!(body["issue"]["story_points"], 8);
    assert_eq!(body["issue"]["agile_sprint_id"], 7);
    assert_eq!(body["issue"]["agile_position"], 2);
}

#[tokio::test]
async fn update_redmine_issue_combined_core_and_agile_change_sends_both_puts() {
    let h = support::harness(&[("REDMINE_AGILE_ENABLED", "true")]).await;
    Mock::given(method("PUT"))
        .and(path("/issues/7.json"))
        .and(body_json(json!({"issue": {"subject": "Updated"}})))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&h.redmine)
        .await;
    Mock::given(method("GET"))
        .and(path("/issues/7.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(issue_json(7, "Updated")))
        .mount(&h.redmine)
        .await;
    Mock::given(method("GET"))
        .and(path("/issues/7/agile_data.json"))
        .respond_with(ResponseTemplate::new(404))
        .up_to_n_times(1)
        .mount(&h.redmine)
        .await;
    Mock::given(method("PUT"))
        .and(path("/issues/7.json"))
        .and(body_json(json!({
            "issue": {"agile_data_attributes": {"story_points": 5}}
        })))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&h.redmine)
        .await;
    Mock::given(method("GET"))
        .and(path("/issues/7/agile_data.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "agile_data": {"id": 1, "story_points": 5}
        })))
        .mount(&h.redmine)
        .await;

    let result = call(
        &h,
        "update_redmine_issue",
        json!({"issue_id": 7, "subject": "Updated", "story_points": 5}),
    )
    .await;
    assert_ne!(
        result.is_error,
        Some(true),
        "{:?}",
        result.structured_content
    );
    let body = body_of(&result);
    assert_eq!(body["issue"]["story_points"], 5);
}

#[tokio::test]
async fn update_redmine_issue_story_points_null_clears_and_sprint_zero_clears() {
    let h = support::harness(&[("REDMINE_AGILE_ENABLED", "true")]).await;
    Mock::given(method("GET"))
        .and(path("/issues/7/agile_data.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "agile_data": {"id": 9, "story_points": 8, "agile_sprint_id": 3, "position": 2}
        })))
        .up_to_n_times(1)
        .mount(&h.redmine)
        .await;
    Mock::given(method("PUT"))
        .and(path("/issues/7.json"))
        .and(body_json(json!({
            "issue": {"agile_data_attributes": {
                "id": 9, "story_points": null, "agile_sprint_id": 0, "position": 2
            }}
        })))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&h.redmine)
        .await;
    Mock::given(method("GET"))
        .and(path("/issues/7/agile_data.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "agile_data": {"id": 9, "position": 2}
        })))
        .mount(&h.redmine)
        .await;
    Mock::given(method("GET"))
        .and(path("/issues/7.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(issue_json(7, "Existing")))
        .mount(&h.redmine)
        .await;

    let result = call(
        &h,
        "update_redmine_issue",
        json!({"issue_id": 7, "story_points": null, "agile_sprint_id": 0}),
    )
    .await;
    assert_ne!(
        result.is_error,
        Some(true),
        "{:?}",
        result.structured_content
    );
    let body = body_of(&result);
    assert!(body["issue"]["story_points"].is_null());
    assert!(body["issue"]["agile_sprint_id"].is_null());
}

#[tokio::test]
async fn update_redmine_issue_agile_failure_after_core_success_says_core_already_applied() {
    let h = support::harness(&[("REDMINE_AGILE_ENABLED", "true")]).await;
    Mock::given(method("PUT"))
        .and(path("/issues/7.json"))
        .and(body_json(json!({"issue": {"subject": "Updated"}})))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&h.redmine)
        .await;
    Mock::given(method("GET"))
        .and(path("/issues/7.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(issue_json(7, "Updated")))
        .mount(&h.redmine)
        .await;
    Mock::given(method("GET"))
        .and(path("/issues/7/agile_data.json"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&h.redmine)
        .await;
    Mock::given(method("PUT"))
        .and(path("/issues/7.json"))
        .and(body_json(json!({
            "issue": {"agile_data_attributes": {"story_points": 5}}
        })))
        .respond_with(ResponseTemplate::new(403))
        .mount(&h.redmine)
        .await;

    let result = call(
        &h,
        "update_redmine_issue",
        json!({"issue_id": 7, "subject": "Updated", "story_points": 5}),
    )
    .await;
    assert_eq!(result.is_error, Some(true));
    let message = result.structured_content.unwrap()["error"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        message.contains("core fields were already updated"),
        "{message}"
    );
}

// --- update_redmine_issue: AlphaNodes additional_tags plugin fields ---

#[tokio::test]
async fn update_redmine_issue_tag_list_with_flag_off_is_misconfigured_with_zero_requests() {
    let h = support::harness(&[]).await;

    let result = call(
        &h,
        "update_redmine_issue",
        json!({"issue_id": 7, "tag_list": ["a"]}),
    )
    .await;
    assert_eq!(result.is_error, Some(true));
    let structured = result.structured_content.unwrap();
    assert_eq!(structured["code"], "MISCONFIGURED");
    assert!(
        structured["hint"]
            .as_str()
            .unwrap()
            .contains("REDMINE_TAGS_ENABLED")
    );
    let requests = h.redmine.received_requests().await.unwrap_or_default();
    assert!(
        requests.is_empty(),
        "no request should reach Redmine: {requests:?}"
    );
}

#[tokio::test]
async fn update_redmine_issue_tag_list_only_is_accepted_and_sends_the_full_replacement_set() {
    let h = support::harness(&[("REDMINE_TAGS_ENABLED", "true")]).await;
    Mock::given(method("PUT"))
        .and(path("/issues/7.json"))
        .and(body_json(json!({"issue": {"tag_list": ["x"]}})))
        .respond_with(ResponseTemplate::new(204))
        .mount(&h.redmine)
        .await;
    Mock::given(method("GET"))
        .and(path("/issues/7.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(issue_json(7, "s")))
        .mount(&h.redmine)
        .await;

    let result = call(
        &h,
        "update_redmine_issue",
        json!({"issue_id": 7, "tag_list": ["x"]}),
    )
    .await;
    assert_ne!(
        result.is_error,
        Some(true),
        "{:?}",
        result.structured_content
    );
}

#[tokio::test]
async fn update_redmine_issue_empty_tag_list_clears_and_is_not_rejected_as_a_no_op() {
    let h = support::harness(&[("REDMINE_TAGS_ENABLED", "true")]).await;
    Mock::given(method("PUT"))
        .and(path("/issues/7.json"))
        .and(body_json(json!({"issue": {"tag_list": []}})))
        .respond_with(ResponseTemplate::new(204))
        .mount(&h.redmine)
        .await;
    Mock::given(method("GET"))
        .and(path("/issues/7.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(issue_json(7, "s")))
        .mount(&h.redmine)
        .await;

    let result = call(
        &h,
        "update_redmine_issue",
        json!({"issue_id": 7, "tag_list": []}),
    )
    .await;
    assert_ne!(
        result.is_error,
        Some(true),
        "{:?}",
        result.structured_content
    );
}

#[tokio::test]
async fn update_redmine_issue_rejects_a_comma_containing_tag_naming_the_array_form() {
    let h = support::harness(&[("REDMINE_TAGS_ENABLED", "true")]).await;
    let mut request = CallToolRequestParams::new("update_redmine_issue".to_string());
    request.arguments = json!({"issue_id": 7, "tag_list": ["a,b"]})
        .as_object()
        .cloned();
    let result = h.client.call_tool(request).await;
    assert!(result.is_err());
}

#[tokio::test]
async fn update_redmine_issue_rejects_a_blank_after_trim_tag() {
    let h = support::harness(&[("REDMINE_TAGS_ENABLED", "true")]).await;
    let mut request = CallToolRequestParams::new("update_redmine_issue".to_string());
    request.arguments = json!({"issue_id": 7, "tag_list": [""]})
        .as_object()
        .cloned();
    let result = h.client.call_tool(request).await;
    assert!(result.is_err());
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
