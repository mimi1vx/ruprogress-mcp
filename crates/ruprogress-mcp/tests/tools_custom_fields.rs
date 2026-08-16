//! e2e: `custom_fields` on `create_redmine_issue`/`update_redmine_issue`
//! (7f1) — writing by id and by name, validation before any request, and
//! the project-lookup failure path. Happy-path core behaviour for the two
//! tools otherwise (uploads, tags, agile) is covered in
//! `tests/tools_issues_write.rs`; this file is scoped to `custom_fields`.
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

fn project_with_definitions_json() -> Value {
    json!({
        "project": {
            "id": 1,
            "name": "P",
            "identifier": "p",
            "created_on": "2026-01-01T00:00:00Z",
            "updated_on": "2026-01-01T00:00:00Z",
            "issue_custom_fields": [
                {"id": 3, "name": "Severity", "field_format": "string"},
                {"id": 4, "name": "Severity Level", "field_format": "string"}
            ]
        }
    })
}

async fn mock_project_with_definitions(server: &wiremock::MockServer) {
    Mock::given(method("GET"))
        .and(path("/projects/1.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(project_with_definitions_json()))
        .mount(server)
        .await;
}

fn project_with_ambiguous_definitions_json() -> Value {
    json!({
        "project": {
            "id": 1,
            "name": "P",
            "identifier": "p",
            "created_on": "2026-01-01T00:00:00Z",
            "updated_on": "2026-01-01T00:00:00Z",
            "issue_custom_fields": [
                {"id": 3, "name": "Story Points", "field_format": "string"},
                {"id": 4, "name": "story_points", "field_format": "string"}
            ]
        }
    })
}

async fn mock_project_with_ambiguous_definitions(server: &wiremock::MockServer) {
    Mock::given(method("GET"))
        .and(path("/projects/1.json"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(project_with_ambiguous_definitions_json()),
        )
        .mount(server)
        .await;
}

// --- create_redmine_issue ---

#[tokio::test]
async fn create_redmine_issue_custom_fields_by_id_sends_expected_body_with_one_request() {
    let h = support::harness(&[]).await;
    Mock::given(method("POST"))
        .and(path("/issues.json"))
        .and(body_json(json!({
            "issue": {
                "project_id": "1",
                "subject": "New issue",
                "custom_fields": [
                    {"id": 3, "value": "blue"},
                    {"id": 4, "value": null},
                    {"id": 5, "value": ["a", "b"]}
                ]
            }
        })))
        .respond_with(ResponseTemplate::new(201).set_body_json(issue_json(42, "New issue")))
        .mount(&h.redmine)
        .await;

    let result = call(
        &h,
        "create_redmine_issue",
        json!({
            "project_id": 1,
            "subject": "New issue",
            "custom_fields": [
                {"id": 3, "value": "blue"},
                {"id": 4, "value": null},
                {"id": 5, "value": ["a", "b"]}
            ]
        }),
    )
    .await;
    assert_ne!(
        result.is_error,
        Some(true),
        "{:?}",
        result.structured_content
    );
    let requests = h.redmine.received_requests().await.unwrap_or_default();
    assert_eq!(
        requests.len(),
        1,
        "an all-id custom_fields array must send no definitions request: {requests:?}"
    );
}

#[tokio::test]
async fn create_redmine_issue_custom_fields_by_name_resolves_via_one_project_lookup() {
    let h = support::harness(&[]).await;
    mock_project_with_definitions(&h.redmine).await;
    Mock::given(method("POST"))
        .and(path("/issues.json"))
        .and(body_json(json!({
            "issue": {
                "project_id": "1",
                "subject": "New issue",
                "custom_fields": [{"id": 3, "value": "high"}]
            }
        })))
        .respond_with(ResponseTemplate::new(201).set_body_json(issue_json(42, "New issue")))
        .mount(&h.redmine)
        .await;

    let result = call(
        &h,
        "create_redmine_issue",
        json!({
            "project_id": 1,
            "subject": "New issue",
            "custom_fields": [{"name": "Severity", "value": "high"}]
        }),
    )
    .await;
    assert_ne!(
        result.is_error,
        Some(true),
        "{:?}",
        result.structured_content
    );
    let requests = h.redmine.received_requests().await.unwrap_or_default();
    assert_eq!(
        requests.len(),
        2,
        "a name entry costs exactly one extra project lookup: {requests:?}"
    );
}

#[tokio::test]
async fn create_redmine_issue_custom_fields_neither_id_nor_name_is_a_protocol_error() {
    let h = support::harness(&[]).await;
    let mut request = CallToolRequestParams::new("create_redmine_issue".to_string());
    request.arguments = json!({
        "project_id": 1, "subject": "New issue",
        "custom_fields": [{"value": "x"}]
    })
    .as_object()
    .cloned();
    let result = h.client.call_tool(request).await;
    assert!(result.is_err());
    let requests = h.redmine.received_requests().await.unwrap_or_default();
    assert!(requests.is_empty(), "no request should reach Redmine");
}

#[tokio::test]
async fn create_redmine_issue_custom_fields_both_id_and_name_is_a_protocol_error() {
    let h = support::harness(&[]).await;
    let mut request = CallToolRequestParams::new("create_redmine_issue".to_string());
    request.arguments = json!({
        "project_id": 1, "subject": "New issue",
        "custom_fields": [{"id": 1, "name": "Severity", "value": "x"}]
    })
    .as_object()
    .cloned();
    let result = h.client.call_tool(request).await;
    assert!(result.is_err());
}

#[tokio::test]
async fn create_redmine_issue_custom_fields_unknown_name_is_a_protocol_error_with_no_write() {
    let h = support::harness(&[]).await;
    mock_project_with_definitions(&h.redmine).await;
    let mut request = CallToolRequestParams::new("create_redmine_issue".to_string());
    request.arguments = json!({
        "project_id": 1, "subject": "New issue",
        "custom_fields": [{"name": "Nonexistent", "value": "x"}]
    })
    .as_object()
    .cloned();
    let result = h.client.call_tool(request).await;
    assert!(result.is_err());
    let requests = h.redmine.received_requests().await.unwrap_or_default();
    assert!(
        requests.iter().all(|r| r.method.as_str() != "POST"),
        "no write should reach Redmine when name resolution fails: {requests:?}"
    );
}

#[tokio::test]
async fn create_redmine_issue_custom_fields_ambiguous_name_is_a_protocol_error() {
    let h = support::harness(&[]).await;
    mock_project_with_ambiguous_definitions(&h.redmine).await;
    let mut request = CallToolRequestParams::new("create_redmine_issue".to_string());
    request.arguments = json!({
        "project_id": 1, "subject": "New issue",
        "custom_fields": [{"name": "StoryPoints", "value": "5"}]
    })
    .as_object()
    .cloned();
    let result = h.client.call_tool(request).await;
    assert!(result.is_err());
    let requests = h.redmine.received_requests().await.unwrap_or_default();
    assert!(
        requests.iter().all(|r| r.method.as_str() != "POST"),
        "no write should reach Redmine when a name is ambiguous: {requests:?}"
    );
}

#[tokio::test]
async fn create_redmine_issue_custom_fields_duplicate_id_is_a_protocol_error_with_no_write() {
    let h = support::harness(&[]).await;
    let mut request = CallToolRequestParams::new("create_redmine_issue".to_string());
    request.arguments = json!({
        "project_id": 1, "subject": "New issue",
        "custom_fields": [{"id": 3, "value": "a"}, {"id": 3, "value": "b"}]
    })
    .as_object()
    .cloned();
    let result = h.client.call_tool(request).await;
    assert!(result.is_err());
    let requests = h.redmine.received_requests().await.unwrap_or_default();
    assert!(requests.is_empty(), "no request should reach Redmine");
}

#[tokio::test]
async fn create_redmine_issue_custom_fields_project_lookup_403_is_in_band_with_no_write() {
    let h = support::harness(&[]).await;
    Mock::given(method("GET"))
        .and(path("/projects/1.json"))
        .respond_with(ResponseTemplate::new(403))
        .mount(&h.redmine)
        .await;

    let result = call(
        &h,
        "create_redmine_issue",
        json!({
            "project_id": 1,
            "subject": "New issue",
            "custom_fields": [{"name": "Severity", "value": "high"}]
        }),
    )
    .await;
    assert_eq!(result.is_error, Some(true));
    assert_eq!(result.structured_content.unwrap()["code"], "FORBIDDEN");
    let requests = h.redmine.received_requests().await.unwrap_or_default();
    assert!(
        requests.iter().all(|r| r.method.as_str() != "POST"),
        "no write should reach Redmine after a definitions-lookup failure: {requests:?}"
    );
}

// --- update_redmine_issue ---

#[tokio::test]
async fn update_redmine_issue_custom_fields_by_id_sends_exactly_one_request() {
    let h = support::harness(&[]).await;
    Mock::given(method("PUT"))
        .and(path("/issues/7.json"))
        .and(body_json(json!({
            "issue": {
                "custom_fields": [
                    {"id": 3, "value": "blue"},
                    {"id": 4, "value": null}
                ]
            }
        })))
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
        json!({
            "issue_id": 7,
            "custom_fields": [
                {"id": 3, "value": "blue"},
                {"id": 4, "value": null}
            ]
        }),
    )
    .await;
    assert_ne!(
        result.is_error,
        Some(true),
        "{:?}",
        result.structured_content
    );
    let requests = h.redmine.received_requests().await.unwrap_or_default();
    assert_eq!(
        requests.len(),
        2,
        "an all-id update is one PUT plus the existing follow-up GET, no more: {requests:?}"
    );
}

#[tokio::test]
async fn update_redmine_issue_custom_fields_by_name_costs_two_extra_reads() {
    let h = support::harness(&[]).await;
    Mock::given(method("GET"))
        .and(path("/issues/7.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(issue_json(7, "s")))
        .mount(&h.redmine)
        .await;
    mock_project_with_definitions(&h.redmine).await;
    Mock::given(method("PUT"))
        .and(path("/issues/7.json"))
        .and(body_json(json!({
            "issue": {"custom_fields": [{"id": 3, "value": "high"}]}
        })))
        .respond_with(ResponseTemplate::new(204))
        .mount(&h.redmine)
        .await;

    let result = call(
        &h,
        "update_redmine_issue",
        json!({
            "issue_id": 7,
            "custom_fields": [{"name": "Severity", "value": "high"}]
        }),
    )
    .await;
    assert_ne!(
        result.is_error,
        Some(true),
        "{:?}",
        result.structured_content
    );
    let requests = h.redmine.received_requests().await.unwrap_or_default();
    // GET /issues/7 (project lookup) + GET /projects/1 (definitions) + PUT
    // /issues/7 (the write) + GET /issues/7 (update_issue's own follow-up
    // read) = 2 extra reads on top of the all-id baseline's PUT+GET.
    assert_eq!(requests.len(), 4, "{requests:?}");
}

#[tokio::test]
async fn update_redmine_issue_custom_fields_only_is_accepted_not_rejected_as_a_no_op() {
    let h = support::harness(&[]).await;
    Mock::given(method("PUT"))
        .and(path("/issues/7.json"))
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
        json!({"issue_id": 7, "custom_fields": [{"id": 3, "value": "x"}]}),
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
async fn update_redmine_issue_empty_custom_fields_only_is_still_rejected_as_a_no_op() {
    let h = support::harness(&[]).await;
    let mut request = CallToolRequestParams::new("update_redmine_issue".to_string());
    request.arguments = json!({"issue_id": 7, "custom_fields": []})
        .as_object()
        .cloned();
    let result = h.client.call_tool(request).await;
    assert!(result.is_err());
    let requests = h.redmine.received_requests().await.unwrap_or_default();
    assert!(requests.is_empty(), "no request should reach Redmine");
}

#[tokio::test]
async fn update_redmine_issue_custom_fields_neither_id_nor_name_is_a_protocol_error_with_no_write()
{
    let h = support::harness(&[]).await;
    let mut request = CallToolRequestParams::new("update_redmine_issue".to_string());
    request.arguments = json!({
        "issue_id": 7,
        "custom_fields": [{"value": "x"}]
    })
    .as_object()
    .cloned();
    let result = h.client.call_tool(request).await;
    assert!(result.is_err());
    let requests = h.redmine.received_requests().await.unwrap_or_default();
    assert!(requests.is_empty(), "no request should reach Redmine");
}

#[tokio::test]
async fn update_redmine_issue_custom_fields_project_lookup_403_is_in_band_with_no_write() {
    let h = support::harness(&[]).await;
    Mock::given(method("GET"))
        .and(path("/issues/7.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(issue_json(7, "s")))
        .mount(&h.redmine)
        .await;
    Mock::given(method("GET"))
        .and(path("/projects/1.json"))
        .respond_with(ResponseTemplate::new(403))
        .mount(&h.redmine)
        .await;

    let result = call(
        &h,
        "update_redmine_issue",
        json!({
            "issue_id": 7,
            "custom_fields": [{"name": "Severity", "value": "high"}]
        }),
    )
    .await;
    assert_eq!(result.is_error, Some(true));
    assert_eq!(result.structured_content.unwrap()["code"], "FORBIDDEN");
    let requests = h.redmine.received_requests().await.unwrap_or_default();
    assert!(
        requests.iter().all(|r| r.method.as_str() != "PUT"),
        "no write should reach Redmine after a definitions-lookup failure: {requests:?}"
    );
}
