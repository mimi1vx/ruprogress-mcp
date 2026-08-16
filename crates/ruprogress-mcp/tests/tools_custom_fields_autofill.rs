//! e2e: required-custom-field autofill on `create_redmine_issue`/
//! `update_redmine_issue` — recovering from a 422 that names a blank or
//! invalid required field by retrying exactly once with a filled value.
//! Writing `custom_fields` itself (by id/by name, validation, the plain
//! project-lookup failure path) is covered in `tests/tools_custom_fields.rs`;
//! this file is scoped to the retry.
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

fn required_422(message: &str) -> ResponseTemplate {
    ResponseTemplate::new(422).set_body_json(json!({"errors": [message]}))
}

/// A project carrying one `issue_custom_fields` definition.
fn project_json(
    id: u64,
    name: &str,
    default_value: Option<&str>,
    possible_values: Option<&[&str]>,
) -> Value {
    json!({
        "project": {
            "id": 1,
            "name": "P",
            "identifier": "p",
            "created_on": "2026-01-01T00:00:00Z",
            "updated_on": "2026-01-01T00:00:00Z",
            "issue_custom_fields": [{
                "id": id,
                "name": name,
                "field_format": "string",
                "default_value": default_value,
                "possible_values": possible_values.map(|values| {
                    values.iter().map(|v| json!({"value": v})).collect::<Vec<_>>()
                }),
            }]
        }
    })
}

async fn mock_project(server: &wiremock::MockServer, project: Value) {
    Mock::given(method("GET"))
        .and(path("/projects/1.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(project))
        .mount(server)
        .await;
}

// --- create_redmine_issue ---

#[tokio::test]
async fn autofill_off_required_field_422_sends_exactly_one_write() {
    let h = support::harness(&[]).await;
    Mock::given(method("POST"))
        .and(path("/issues.json"))
        .respond_with(required_422("Department can't be blank"))
        .mount(&h.redmine)
        .await;

    let result = call(
        &h,
        "create_redmine_issue",
        json!({"project_id": 1, "subject": "New issue"}),
    )
    .await;

    assert_eq!(result.is_error, Some(true));
    let body = result.structured_content.unwrap();
    assert_eq!(body["code"], "VALIDATION_FAILED");
    assert_eq!(body["missing_required_fields"], json!(["Department"]));
    assert!(
        body["hint"]
            .as_str()
            .unwrap()
            .contains("REDMINE_AUTOFILL_REQUIRED_CUSTOM_FIELDS")
    );

    let requests = h.redmine.received_requests().await.unwrap_or_default();
    let posts = requests
        .iter()
        .filter(|r| r.url.path() == "/issues.json")
        .count();
    assert_eq!(posts, 1, "autofill off must never retry: {requests:?}");
}

#[tokio::test]
async fn autofill_on_uses_the_definitions_default_value_and_reports_it() {
    let h = support::harness(&[("REDMINE_AUTOFILL_REQUIRED_CUSTOM_FIELDS", "true")]).await;
    Mock::given(method("POST"))
        .and(path("/issues.json"))
        .respond_with(required_422("Department can't be blank"))
        .up_to_n_times(1)
        .mount(&h.redmine)
        .await;
    mock_project(
        &h.redmine,
        project_json(3, "Department", Some("Engineering"), None),
    )
    .await;
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

    assert_ne!(
        result.is_error,
        Some(true),
        "{:?}",
        result.structured_content
    );
    let body = result.structured_content.unwrap();
    let filled = &body["autofilled_custom_fields"][0];
    assert_eq!(filled["id"], 3);
    assert!(filled["name"].as_str().unwrap().contains("Department"));
    assert!(filled["value"].as_str().unwrap().contains("Engineering"));

    let requests = h.redmine.received_requests().await.unwrap_or_default();
    let posts = requests
        .iter()
        .filter(|r| r.url.path() == "/issues.json")
        .count();
    assert_eq!(posts, 2, "{requests:?}");
}

#[tokio::test]
async fn autofill_on_falls_back_to_the_configured_map_when_no_default_value() {
    let h = support::harness(&[
        ("REDMINE_AUTOFILL_REQUIRED_CUSTOM_FIELDS", "true"),
        (
            "REDMINE_REQUIRED_CUSTOM_FIELD_DEFAULTS",
            r#"{"Department": "Sales"}"#,
        ),
    ])
    .await;
    Mock::given(method("POST"))
        .and(path("/issues.json"))
        .respond_with(required_422("Department can't be blank"))
        .up_to_n_times(1)
        .mount(&h.redmine)
        .await;
    mock_project(&h.redmine, project_json(3, "Department", None, None)).await;
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

    assert_ne!(
        result.is_error,
        Some(true),
        "{:?}",
        result.structured_content
    );
    let body = result.structured_content.unwrap();
    assert!(
        body["autofilled_custom_fields"][0]["value"]
            .as_str()
            .unwrap()
            .contains("Sales")
    );
}

#[tokio::test]
async fn autofill_on_nothing_fillable_sends_exactly_one_write() {
    let h = support::harness(&[("REDMINE_AUTOFILL_REQUIRED_CUSTOM_FIELDS", "true")]).await;
    Mock::given(method("POST"))
        .and(path("/issues.json"))
        .respond_with(required_422("Department can't be blank"))
        .mount(&h.redmine)
        .await;
    // The definition exists but has no default, and the operator configured
    // no map entry for it — nothing to fill with.
    mock_project(&h.redmine, project_json(3, "Department", None, None)).await;

    let result = call(
        &h,
        "create_redmine_issue",
        json!({"project_id": 1, "subject": "New issue"}),
    )
    .await;

    assert_eq!(result.is_error, Some(true));
    let body = result.structured_content.unwrap();
    assert_eq!(body["code"], "VALIDATION_FAILED");

    let requests = h.redmine.received_requests().await.unwrap_or_default();
    let posts = requests
        .iter()
        .filter(|r| r.url.path() == "/issues.json")
        .count();
    assert_eq!(posts, 1, "nothing fillable must never retry: {requests:?}");
}

#[tokio::test]
async fn autofill_on_second_attempt_also_422s_sends_exactly_two_writes_never_a_third() {
    let h = support::harness(&[("REDMINE_AUTOFILL_REQUIRED_CUSTOM_FIELDS", "true")]).await;
    Mock::given(method("POST"))
        .and(path("/issues.json"))
        .respond_with(required_422("Department can't be blank"))
        .up_to_n_times(1)
        .mount(&h.redmine)
        .await;
    mock_project(
        &h.redmine,
        project_json(3, "Department", Some("Engineering"), None),
    )
    .await;
    // The retry still fails, for whatever reason Redmine has: no third
    // attempt should ever be made regardless.
    Mock::given(method("POST"))
        .and(path("/issues.json"))
        .respond_with(required_422("Department can't be blank"))
        .mount(&h.redmine)
        .await;

    let result = call(
        &h,
        "create_redmine_issue",
        json!({"project_id": 1, "subject": "New issue"}),
    )
    .await;

    assert_eq!(result.is_error, Some(true));
    let body = result.structured_content.unwrap();
    assert_eq!(body["code"], "VALIDATION_FAILED");
    assert_eq!(body["missing_required_fields"], json!(["Department"]));

    let requests = h.redmine.received_requests().await.unwrap_or_default();
    let posts = requests
        .iter()
        .filter(|r| r.url.path() == "/issues.json")
        .count();
    assert_eq!(posts, 2, "never a third attempt: {requests:?}");
}

#[tokio::test]
async fn autofill_on_replaces_a_caller_value_redmine_rejected_as_out_of_range() {
    let h = support::harness(&[("REDMINE_AUTOFILL_REQUIRED_CUSTOM_FIELDS", "true")]).await;
    Mock::given(method("POST"))
        .and(path("/issues.json"))
        .respond_with(required_422("Severity is not included in the list"))
        .up_to_n_times(1)
        .mount(&h.redmine)
        .await;
    mock_project(
        &h.redmine,
        project_json(3, "Severity", Some("High"), Some(&["Low", "High"])),
    )
    .await;
    Mock::given(method("POST"))
        .and(path("/issues.json"))
        .respond_with(ResponseTemplate::new(201).set_body_json(issue_json(42, "New issue")))
        .mount(&h.redmine)
        .await;

    let result = call(
        &h,
        "create_redmine_issue",
        json!({
            "project_id": 1,
            "subject": "New issue",
            "custom_fields": [{"id": 3, "value": "NotAllowed"}]
        }),
    )
    .await;

    assert_ne!(
        result.is_error,
        Some(true),
        "{:?}",
        result.structured_content
    );
    let body = result.structured_content.unwrap();
    assert!(
        body["autofilled_custom_fields"][0]["value"]
            .as_str()
            .unwrap()
            .contains("High"),
        "the rejected caller value must be replaced by the default: {body:?}"
    );
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

fn base64_of(bytes: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

#[tokio::test]
async fn autofill_on_a_create_with_uploads_retries_with_the_same_upload_token() {
    let h = support::harness(&[("REDMINE_AUTOFILL_REQUIRED_CUSTOM_FIELDS", "true")]).await;
    // The upload is resolved and minted once, before the first write is even
    // attempted — the retry reuses that same token rather than re-minting.
    mock_upload_token(&h.redmine, 99, "99.token").await;
    mock_attachment_metadata(&h.redmine, 99, "notes.txt").await;
    Mock::given(method("POST"))
        .and(path("/issues.json"))
        .respond_with(required_422("Department can't be blank"))
        .up_to_n_times(1)
        .mount(&h.redmine)
        .await;
    mock_project(
        &h.redmine,
        project_json(3, "Department", Some("Engineering"), None),
    )
    .await;
    Mock::given(method("POST"))
        .and(path("/issues.json"))
        .respond_with(ResponseTemplate::new(201).set_body_json(issue_json(42, "New issue")))
        .mount(&h.redmine)
        .await;

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
    let requests = h.redmine.received_requests().await.unwrap_or_default();
    let posts: Vec<_> = requests
        .iter()
        .filter(|r| r.url.path() == "/issues.json")
        .collect();
    assert_eq!(posts.len(), 2, "{requests:?}");
    for post in &posts {
        let body: Value = post.body_json().expect("issues.json body should be JSON");
        assert_eq!(
            body["issue"]["uploads"],
            json!([{"token": "99.token"}]),
            "the upload token must be unchanged across the retry"
        );
    }
    // Exactly one mint: the token is resolved once, before either attempt.
    let mints = requests
        .iter()
        .filter(|r| r.url.path() == "/uploads.json")
        .count();
    assert_eq!(mints, 1, "{requests:?}");
}

// --- update_redmine_issue ---

#[tokio::test]
async fn update_retry_path_is_get_issue_then_get_project_then_put_then_its_own_follow_up_get() {
    let h = support::harness(&[("REDMINE_AUTOFILL_REQUIRED_CUSTOM_FIELDS", "true")]).await;
    Mock::given(method("PUT"))
        .and(path("/issues/7.json"))
        .respond_with(required_422("Department can't be blank"))
        .up_to_n_times(1)
        .mount(&h.redmine)
        .await;
    Mock::given(method("GET"))
        .and(path("/issues/7.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(issue_json(7, "s")))
        .mount(&h.redmine)
        .await;
    mock_project(
        &h.redmine,
        project_json(3, "Department", Some("Engineering"), None),
    )
    .await;
    Mock::given(method("PUT"))
        .and(path("/issues/7.json"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&h.redmine)
        .await;

    let result = call(
        &h,
        "update_redmine_issue",
        json!({"issue_id": 7, "subject": "renamed"}),
    )
    .await;

    assert_ne!(
        result.is_error,
        Some(true),
        "{:?}",
        result.structured_content
    );
    let body = result.structured_content.unwrap();
    assert_eq!(body["autofilled_custom_fields"][0]["id"], 3);

    let requests = h.redmine.received_requests().await.unwrap_or_default();
    let seq: Vec<(String, String)> = requests
        .iter()
        .map(|r| (r.method.to_string(), r.url.path().to_string()))
        .collect();
    assert_eq!(
        seq,
        vec![
            ("PUT".to_string(), "/issues/7.json".to_string()),
            ("GET".to_string(), "/issues/7.json".to_string()),
            ("GET".to_string(), "/projects/1.json".to_string()),
            ("PUT".to_string(), "/issues/7.json".to_string()),
            ("GET".to_string(), "/issues/7.json".to_string()),
        ],
        "{requests:?}"
    );
}

#[tokio::test]
async fn update_definitions_lookup_404_returns_the_original_422_not_not_found() {
    let h = support::harness(&[("REDMINE_AUTOFILL_REQUIRED_CUSTOM_FIELDS", "true")]).await;
    Mock::given(method("PUT"))
        .and(path("/issues/7.json"))
        .respond_with(required_422("Department can't be blank"))
        .mount(&h.redmine)
        .await;
    Mock::given(method("GET"))
        .and(path("/issues/7.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(issue_json(7, "s")))
        .mount(&h.redmine)
        .await;
    Mock::given(method("GET"))
        .and(path("/projects/1.json"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&h.redmine)
        .await;

    let result = call(
        &h,
        "update_redmine_issue",
        json!({"issue_id": 7, "subject": "renamed"}),
    )
    .await;

    assert_eq!(result.is_error, Some(true));
    let body = result.structured_content.unwrap();
    assert_eq!(
        body["code"], "VALIDATION_FAILED",
        "a lookup failure must not mask the original validation error: {body:?}"
    );
    assert_eq!(body["missing_required_fields"], json!(["Department"]));

    let requests = h.redmine.received_requests().await.unwrap_or_default();
    let puts = requests.iter().filter(|r| r.method == "PUT").count();
    assert_eq!(
        puts, 1,
        "no retry should be attempted once the definitions lookup fails: {requests:?}"
    );
}

#[tokio::test]
async fn update_with_notes_that_422s_then_succeeds_sends_exactly_two_puts() {
    let h = support::harness(&[("REDMINE_AUTOFILL_REQUIRED_CUSTOM_FIELDS", "true")]).await;
    Mock::given(method("PUT"))
        .and(path("/issues/7.json"))
        .respond_with(required_422("Department can't be blank"))
        .up_to_n_times(1)
        .mount(&h.redmine)
        .await;
    Mock::given(method("GET"))
        .and(path("/issues/7.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(issue_json(7, "s")))
        .mount(&h.redmine)
        .await;
    mock_project(
        &h.redmine,
        project_json(3, "Department", Some("Engineering"), None),
    )
    .await;
    Mock::given(method("PUT"))
        .and(path("/issues/7.json"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&h.redmine)
        .await;

    let result = call(
        &h,
        "update_redmine_issue",
        json!({"issue_id": 7, "notes": "closing this out"}),
    )
    .await;

    assert_ne!(
        result.is_error,
        Some(true),
        "{:?}",
        result.structured_content
    );
    let requests = h.redmine.received_requests().await.unwrap_or_default();
    let puts: Vec<_> = requests.iter().filter(|r| r.method == "PUT").collect();
    assert_eq!(puts.len(), 2, "{requests:?}");
    let last_body: Value = puts[1].body_json().expect("PUT body should be JSON");
    assert_eq!(
        last_body["issue"]["notes"]
            .as_str()
            .unwrap()
            .matches("closing this out")
            .count(),
        1,
        "the note must appear exactly once in the successful attempt's body"
    );
}

#[tokio::test]
async fn agile_only_update_never_triggers_a_definitions_fetch() {
    let h = support::harness(&[
        ("REDMINE_AGILE_ENABLED", "true"),
        ("REDMINE_AUTOFILL_REQUIRED_CUSTOM_FIELDS", "true"),
    ])
    .await;
    Mock::given(method("GET"))
        .and(path("/issues/7.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(issue_json(7, "s")))
        .mount(&h.redmine)
        .await;
    Mock::given(method("GET"))
        .and(path("/issues/7/agile_data.json"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&h.redmine)
        .await;
    // Agile writes go through their own `PUT /issues/{id}.json` with a
    // nested `agile_data_attributes` — a legitimate write this test expects,
    // distinct from the core-field PUT that never happens because
    // `has_core_change` is false here.
    Mock::given(method("PUT"))
        .and(path("/issues/7.json"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&h.redmine)
        .await;

    let result = call(
        &h,
        "update_redmine_issue",
        json!({"issue_id": 7, "story_points": 5}),
    )
    .await;

    assert_ne!(
        result.is_error,
        Some(true),
        "{:?}",
        result.structured_content
    );
    let requests = h.redmine.received_requests().await.unwrap_or_default();
    assert!(
        requests.iter().all(|r| r.url.path() != "/projects/1.json"),
        "an agile-only update must never fetch custom-field definitions: {requests:?}"
    );
}
