//! `manage_document` (DMSF plugin, `REDMINE_DMSF_ENABLED`).
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

fn dmsf_env() -> Vec<(&'static str, &'static str)> {
    vec![("REDMINE_DMSF_ENABLED", "true")]
}

fn call(name: &str, args: &serde_json::Value) -> CallToolRequestParams {
    let mut request = CallToolRequestParams::new(name.to_string());
    request.arguments = args.as_object().cloned();
    request
}

fn unique_dir(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "ruprogress-mcp-test-dmsf-{name}-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ))
}

// --- list ---

#[tokio::test]
async fn manage_document_list_happy_path() {
    let h = support::harness(&dmsf_env()).await;
    Mock::given(method("GET"))
        .and(path("/projects/1/dmsf.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "dmsf": {"dmsf_nodes": [{"id": 1, "filename": "report.pdf", "description": "Q1"}], "total_count": 1}
        })))
        .mount(&h.redmine)
        .await;

    let result = h
        .client
        .call_tool(call(
            "manage_document",
            &json!({"action": "list", "project_id": 1}),
        ))
        .await
        .expect("manage_document should be callable");
    assert_ne!(result.is_error, Some(true));
    let structured = result.structured_content.expect("structured");
    assert_eq!(structured["documents"][0]["filename"], "report.pdf");
    assert!(
        structured["documents"][0]["description"]
            .as_str()
            .unwrap()
            .contains("Q1"),
        "description should be boundary-wrapped but contain the original text"
    );
    assert_eq!(structured["pagination"]["total"], 1);
}

#[tokio::test]
async fn manage_document_list_without_project_id_is_invalid_params() {
    let h = support::harness(&dmsf_env()).await;
    let result = h
        .client
        .call_tool(call("manage_document", &json!({"action": "list"})))
        .await;
    assert!(result.is_err(), "project_id is required for list");
}

// --- get ---

#[tokio::test]
async fn manage_document_get_happy_path() {
    let h = support::harness(&dmsf_env()).await;
    Mock::given(method("GET"))
        .and(path("/dmsf_files/1.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "dmsf_file": {"id": 1, "project_id": 1, "dmsf_file_revisions": [
                {"title": "Report", "name": "report.pdf", "version_major": 1}
            ]}
        })))
        .mount(&h.redmine)
        .await;

    let result = h
        .client
        .call_tool(call(
            "manage_document",
            &json!({"action": "get", "document_id": 1}),
        ))
        .await
        .expect("manage_document should be callable");
    assert_ne!(result.is_error, Some(true));
    let structured = result.structured_content.expect("structured");
    assert_eq!(structured["document"]["name"], "report.pdf");
    assert_eq!(structured["document"]["title"], "Report");
}

#[tokio::test]
async fn manage_document_get_with_no_revisions_is_not_found() {
    let h = support::harness(&dmsf_env()).await;
    Mock::given(method("GET"))
        .and(path("/dmsf_files/1.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "dmsf_file": {"id": 1, "dmsf_file_revisions": []}
        })))
        .mount(&h.redmine)
        .await;

    let result = h
        .client
        .call_tool(call(
            "manage_document",
            &json!({"action": "get", "document_id": 1}),
        ))
        .await
        .expect("call_tool should succeed at the protocol level");
    assert_eq!(result.is_error, Some(true));
    assert_eq!(
        result.structured_content.expect("structured")["code"],
        "NOT_FOUND"
    );
}

#[tokio::test]
async fn manage_document_get_404_is_not_found() {
    let h = support::harness(&dmsf_env()).await;
    Mock::given(method("GET"))
        .and(path("/dmsf_files/999.json"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&h.redmine)
        .await;

    let result = h
        .client
        .call_tool(call(
            "manage_document",
            &json!({"action": "get", "document_id": 999}),
        ))
        .await
        .expect("call_tool should succeed at the protocol level");
    assert_eq!(result.is_error, Some(true));
    assert_eq!(
        result.structured_content.expect("structured")["code"],
        "NOT_FOUND"
    );
}

#[tokio::test]
async fn manage_document_list_and_get_produce_identical_output_for_the_same_document() {
    let h = support::harness(&dmsf_env()).await;
    Mock::given(method("GET"))
        .and(path("/projects/1/dmsf.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "dmsf": {"dmsf_nodes": [{
                "id": 1, "filename": "report.pdf", "name": "report.pdf",
                "title": "Report", "description": "Q1", "size": 2048,
                "content_type": "application/pdf", "project_id": 1, "version": "1.2.0"
            }], "total_count": 1}
        })))
        .mount(&h.redmine)
        .await;
    Mock::given(method("GET"))
        .and(path("/dmsf_files/1.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "dmsf_file": {"id": 1, "project_id": 1, "dmsf_file_revisions": [
                {"title": "Report", "name": "report.pdf", "description": "Q1",
                 "size": 2048, "content_type": "application/pdf",
                 "version_major": 1, "version_minor": 2, "version_patch": 0}
            ]}
        })))
        .mount(&h.redmine)
        .await;

    let list_result = h
        .client
        .call_tool(call(
            "manage_document",
            &json!({"action": "list", "project_id": 1}),
        ))
        .await
        .expect("list should be callable");
    let get_result = h
        .client
        .call_tool(call(
            "manage_document",
            &json!({"action": "get", "document_id": 1}),
        ))
        .await
        .expect("get should be callable");

    let list_doc = &list_result.structured_content.expect("structured")["documents"][0];
    let get_doc = &get_result.structured_content.expect("structured")["document"];
    for field in [
        "id",
        "name",
        "title",
        "size",
        "content_type",
        "project_id",
        "version",
    ] {
        assert_eq!(
            list_doc[field], get_doc[field],
            "field {field} differs between list and get"
        );
    }
    // `description` is boundary-wrapped with a per-call random nonce, so the
    // wrapped strings differ even though the underlying text is identical.
    assert!(list_doc["description"].as_str().unwrap().contains("Q1"));
    assert!(get_doc["description"].as_str().unwrap().contains("Q1"));
}

// --- create ---

async fn mock_create_flow(redmine: &wiremock::MockServer) {
    Mock::given(method("POST"))
        .and(path("/uploads.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "upload": {"id": 1, "token": "1.token"}
        })))
        .mount(redmine)
        .await;
    Mock::given(method("POST"))
        .and(path("/projects/1/dmsf/commit.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "dmsf_files": [{"id": 43, "name": "report.pdf"}], "total_count": 1
        })))
        .mount(redmine)
        .await;
}

#[tokio::test]
async fn manage_document_create_with_content_base64_happy_path() {
    let h = support::harness(&dmsf_env()).await;
    mock_create_flow(&h.redmine).await;

    let result = h
        .client
        .call_tool(call(
            "manage_document",
            &json!({
                "action": "create", "project_id": 1, "name": "report.pdf",
                "content_base64": base64_encode(b"hello"), "title": "Report",
                "version": "1.2.0"
            }),
        ))
        .await
        .expect("manage_document should be callable");
    assert_ne!(result.is_error, Some(true));
    let structured = result.structured_content.expect("structured");
    assert_eq!(structured["document_id"], 43);
    assert!(
        structured["note"]
            .as_str()
            .unwrap()
            .contains("action=\"get\""),
        "note should point at action=\"get\": {}",
        structured["note"]
    );
}

#[tokio::test]
async fn manage_document_create_commit_body_avoids_every_field_name_trap() {
    let h = support::harness(&dmsf_env()).await;
    mock_create_flow(&h.redmine).await;

    h.client
        .call_tool(call(
            "manage_document",
            &json!({
                "action": "create", "project_id": 1, "name": "report.pdf",
                "content_base64": base64_encode(b"hello"), "title": "Report",
                "version": "1.2.0"
            }),
        ))
        .await
        .expect("manage_document should be callable");

    let requests = h.redmine.received_requests().await.unwrap();
    let commit_request = requests
        .iter()
        .find(|r| r.url.path() == "/projects/1/dmsf/commit.json")
        .expect("commit request should have been sent");
    let body: serde_json::Value = serde_json::from_slice(&commit_request.body).unwrap();
    let uploaded_file = &body["attachments"]["uploaded_file"];
    // Trap 2: `name`, not `filename`.
    assert_eq!(uploaded_file["name"], "report.pdf");
    assert!(uploaded_file.get("filename").is_none());
    // Trap 5: version fields nested inside `uploaded_file` on commit.
    assert_eq!(uploaded_file["version_major"], 1);
    assert_eq!(uploaded_file["version_minor"], 2);
    assert_eq!(uploaded_file["version_patch"], 0);
}

#[tokio::test]
async fn manage_document_create_spells_custom_field_values_not_custom_fields() {
    let h = support::harness(&dmsf_env()).await;
    mock_create_flow(&h.redmine).await;

    h.client
        .call_tool(call(
            "manage_document",
            &json!({
                "action": "create", "project_id": 1, "name": "report.pdf",
                "content_base64": base64_encode(b"hello"),
                "custom_fields": [{"id": 1, "value": "x"}]
            }),
        ))
        .await
        .expect("manage_document should be callable");

    let requests = h.redmine.received_requests().await.unwrap();
    let commit_request = requests
        .iter()
        .find(|r| r.url.path() == "/projects/1/dmsf/commit.json")
        .expect("commit request should have been sent");
    let body: serde_json::Value = serde_json::from_slice(&commit_request.body).unwrap();
    let uploaded_file = &body["attachments"]["uploaded_file"];
    assert!(uploaded_file.get("custom_field_values").is_some());
    assert!(uploaded_file.get("custom_fields").is_none());
}

#[tokio::test]
async fn manage_document_create_with_content_base64_requires_name() {
    let h = support::harness(&dmsf_env()).await;
    let result = h
        .client
        .call_tool(call(
            "manage_document",
            &json!({"action": "create", "project_id": 1, "content_base64": base64_encode(b"hi")}),
        ))
        .await;
    assert!(result.is_err(), "name is required with content_base64");
}

#[tokio::test]
async fn manage_document_create_with_no_source_is_source_required() {
    let h = support::harness(&dmsf_env()).await;
    let result = h
        .client
        .call_tool(call(
            "manage_document",
            &json!({"action": "create", "project_id": 1, "name": "report.pdf"}),
        ))
        .await
        .expect("call_tool should succeed at the protocol level");
    assert_eq!(result.is_error, Some(true));
    assert_eq!(
        result.structured_content.expect("structured")["code"],
        "SOURCE_REQUIRED"
    );
}

#[tokio::test]
async fn manage_document_create_with_source_url_is_unsupported_source() {
    let h = support::harness(&dmsf_env()).await;
    let result = h
        .client
        .call_tool(call(
            "manage_document",
            &json!({
                "action": "create", "project_id": 1, "name": "report.pdf",
                "source_url": "https://example.com/report.pdf"
            }),
        ))
        .await
        .expect("call_tool should succeed at the protocol level");
    assert_eq!(result.is_error, Some(true));
    assert_eq!(
        result.structured_content.expect("structured")["code"],
        "UNSUPPORTED_SOURCE"
    );
}

#[tokio::test]
async fn manage_document_create_with_a_malformed_version_sends_zero_requests() {
    let h = support::harness(&dmsf_env()).await;
    let upload_mock = Mock::given(method("POST"))
        .and(path("/uploads.json"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0);
    h.redmine.register(upload_mock).await;

    let result = h
        .client
        .call_tool(call(
            "manage_document",
            &json!({
                "action": "create", "project_id": 1, "name": "report.pdf",
                "content_base64": base64_encode(b"hi"), "version": "1.2.3.4"
            }),
        ))
        .await;
    assert!(result.is_err(), "a malformed version is a protocol error");
}

#[tokio::test]
async fn manage_document_create_with_file_path_infers_the_name() {
    let root = unique_dir("root");
    std::fs::create_dir_all(&root).unwrap();
    let file = root.join("report.pdf");
    std::fs::write(&file, b"hello").unwrap();

    let mut env = dmsf_env();
    let root_str = root.to_string_lossy().into_owned();
    env.push(("REDMINE_MCP_UPLOAD_FILE_ROOTS", root_str.as_str()));
    let h = support::harness(&env).await;
    mock_create_flow(&h.redmine).await;

    let result = h
        .client
        .call_tool(call(
            "manage_document",
            &json!({"action": "create", "project_id": 1, "file_path": file.to_string_lossy()}),
        ))
        .await
        .expect("manage_document should be callable");
    assert_ne!(result.is_error, Some(true));

    let requests = h.redmine.received_requests().await.unwrap();
    let commit_request = requests
        .iter()
        .find(|r| r.url.path() == "/projects/1/dmsf/commit.json")
        .expect("commit request should have been sent");
    let body: serde_json::Value = serde_json::from_slice(&commit_request.body).unwrap();
    assert_eq!(body["attachments"]["uploaded_file"]["name"], "report.pdf");

    std::fs::remove_dir_all(&root).ok();
}

#[tokio::test]
async fn manage_document_create_with_a_file_path_outside_the_roots_is_path_not_allowed() {
    let h = support::harness(&dmsf_env()).await;
    let upload_mock = Mock::given(method("POST"))
        .and(path("/uploads.json"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0);
    h.redmine.register(upload_mock).await;

    let result = h
        .client
        .call_tool(call(
            "manage_document",
            &json!({"action": "create", "project_id": 1, "file_path": "/etc/hosts"}),
        ))
        .await
        .expect("call_tool should succeed at the protocol level");
    assert_eq!(result.is_error, Some(true));
    assert_eq!(
        result.structured_content.expect("structured")["code"],
        "PATH_NOT_ALLOWED"
    );
}

#[tokio::test]
async fn manage_document_create_with_an_over_cap_file_path_is_file_too_large() {
    let root = unique_dir("big-file-root");
    std::fs::create_dir_all(&root).unwrap();
    let big = root.join("big.bin");
    let file = std::fs::File::create(&big).unwrap();
    file.set_len(50 * 1024 * 1024 + 1).unwrap();
    drop(file);

    let mut env = dmsf_env();
    let root_str = root.to_string_lossy().into_owned();
    env.push(("REDMINE_MCP_UPLOAD_FILE_ROOTS", root_str.as_str()));
    let h = support::harness(&env).await;
    let upload_mock = Mock::given(method("POST"))
        .and(path("/uploads.json"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0);
    h.redmine.register(upload_mock).await;

    let result = h
        .client
        .call_tool(call(
            "manage_document",
            &json!({"action": "create", "project_id": 1, "file_path": big.to_string_lossy()}),
        ))
        .await
        .expect("call_tool should succeed at the protocol level");
    assert_eq!(result.is_error, Some(true));
    assert_eq!(
        result.structured_content.expect("structured")["code"],
        "FILE_TOO_LARGE"
    );

    std::fs::remove_dir_all(&root).ok();
}

#[tokio::test]
async fn manage_document_create_commit_failure_after_a_successful_upload_says_so() {
    let h = support::harness(&dmsf_env()).await;
    Mock::given(method("POST"))
        .and(path("/uploads.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "upload": {"id": 1, "token": "1.token"}
        })))
        .mount(&h.redmine)
        .await;
    Mock::given(method("POST"))
        .and(path("/projects/1/dmsf/commit.json"))
        .respond_with(ResponseTemplate::new(422).set_body_json(json!({
            "errors": ["folder_id is invalid"]
        })))
        .mount(&h.redmine)
        .await;

    let result = h
        .client
        .call_tool(call(
            "manage_document",
            &json!({
                "action": "create", "project_id": 1, "name": "report.pdf",
                "content_base64": base64_encode(b"hello")
            }),
        ))
        .await
        .expect("call_tool should succeed at the protocol level");
    assert_eq!(result.is_error, Some(true));
    let structured = result.structured_content.expect("structured");
    assert!(
        structured["error"]
            .as_str()
            .unwrap()
            .contains("uploaded successfully"),
        "error should say the upload succeeded before the commit failed: {}",
        structured["error"]
    );
}

// --- update ---

async fn mock_show(redmine: &wiremock::MockServer, title: &str, name: &str) {
    Mock::given(method("GET"))
        .and(path("/dmsf_files/1.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "dmsf_file": {"id": 1, "dmsf_file_revisions": [
                {"title": title, "name": name}
            ]}
        })))
        .mount(redmine)
        .await;
}

#[tokio::test]
async fn manage_document_update_supplying_only_description_still_sends_title_and_name() {
    let h = support::harness(&dmsf_env()).await;
    mock_show(&h.redmine, "Report", "report.pdf").await;
    Mock::given(method("POST"))
        .and(path("/dmsf/files/1/revision/create.json"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&h.redmine)
        .await;

    let result = h
        .client
        .call_tool(call(
            "manage_document",
            &json!({"action": "update", "document_id": 1, "description": "Updated"}),
        ))
        .await
        .expect("manage_document should be callable");
    assert_ne!(result.is_error, Some(true));
    let structured = result.structured_content.expect("structured");
    assert_eq!(structured["updated_fields"], json!(["description"]));

    let requests = h.redmine.received_requests().await.unwrap();
    let revision_request = requests
        .iter()
        .find(|r| r.url.path() == "/dmsf/files/1/revision/create.json")
        .expect("revision request should have been sent");
    let body: serde_json::Value = serde_json::from_slice(&revision_request.body).unwrap();
    let revision = &body["dmsf_file_revision"];
    assert_eq!(revision["title"], "Report");
    assert_eq!(revision["name"], "report.pdf");
    assert_eq!(revision["description"], "Updated");
}

#[tokio::test]
async fn manage_document_update_uses_the_slash_route_not_the_underscore_show_route() {
    let h = support::harness(&dmsf_env()).await;
    mock_show(&h.redmine, "Report", "report.pdf").await;
    Mock::given(method("POST"))
        .and(path("/dmsf/files/1/revision/create.json"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&h.redmine)
        .await;

    let result = h
        .client
        .call_tool(call(
            "manage_document",
            &json!({"action": "update", "document_id": 1, "comment": "v2"}),
        ))
        .await
        .expect("manage_document should be callable");
    assert_ne!(result.is_error, Some(true));
}

#[tokio::test]
async fn manage_document_update_with_no_recognisable_document_is_not_found_with_no_write() {
    let h = support::harness(&dmsf_env()).await;
    Mock::given(method("GET"))
        .and(path("/dmsf_files/1.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "dmsf_file": {"id": 1, "dmsf_file_revisions": []}
        })))
        .mount(&h.redmine)
        .await;
    let revision_mock = Mock::given(method("POST"))
        .and(path("/dmsf/files/1/revision/create.json"))
        .respond_with(ResponseTemplate::new(204))
        .expect(0);
    h.redmine.register(revision_mock).await;

    let result = h
        .client
        .call_tool(call(
            "manage_document",
            &json!({"action": "update", "document_id": 1, "comment": "v2"}),
        ))
        .await
        .expect("call_tool should succeed at the protocol level");
    assert_eq!(result.is_error, Some(true));
    assert_eq!(
        result.structured_content.expect("structured")["code"],
        "NOT_FOUND"
    );
}

#[tokio::test]
async fn manage_document_update_with_no_fields_is_invalid_params() {
    let h = support::harness(&dmsf_env()).await;
    let result = h
        .client
        .call_tool(call(
            "manage_document",
            &json!({"action": "update", "document_id": 1}),
        ))
        .await;
    assert!(result.is_err(), "at least one field to update is required");
}

fn base64_encode(bytes: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}
