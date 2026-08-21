//! e2e: `get_redmine_attachment`, `list_files`, `delete_file`, `upload_file`,
//! `cleanup_attachment_files`.
//! `get_redmine_attachment`: happy path per transport (the `uri_type`
//! branch), the `FILE_TOO_LARGE` pre-check and mid-stream enforcement,
//! `STORE_FULL`, and the dominant `redmine_client::Error` passthrough.
//! `list_files`: the Files-module shape, including `digest`/`downloads`/
//! `version`. `delete_file`: the unconditional confirmation guard and the
//! success shape.
//! `upload_file`: the `content_base64` source (the `file_path` source's
//! own path-validation suite lives in `tests/upload_paths.rs`), the source
//! arity/`UNSUPPORTED_SOURCE` checks, and the `create_upload`-only
//! `FILE_TOO_LARGE` remap. `cleanup_attachment_files`: admin-gated
//! registration and the sweep-result shape.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

mod support;

use rmcp::ServiceExt as _;
use rmcp::model::CallToolRequestParams;
use rmcp::transport::StreamableHttpClientTransport;
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, ResponseTemplate};

async fn call_tool(
    h: &support::Harness,
    name: &str,
    args: serde_json::Value,
) -> rmcp::model::CallToolResult {
    let mut request = CallToolRequestParams::new(name.to_string());
    request.arguments = args.as_object().cloned();
    h.client
        .call_tool(request)
        .await
        .expect("call_tool should succeed")
}

async fn call(h: &support::Harness, args: serde_json::Value) -> rmcp::model::CallToolResult {
    call_tool(h, "get_redmine_attachment", args).await
}

async fn mock_attachment(
    redmine: &wiremock::MockServer,
    id: u64,
    filename: &str,
    filesize: u64,
    body: &[u8],
) {
    let content_url = format!("{}/attachments/download/{id}/{filename}", redmine.uri());
    Mock::given(method("GET"))
        .and(path(format!("/attachments/{id}.json")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "attachment": {
                "id": id, "filename": filename, "filesize": filesize,
                "content_type": "application/pdf",
                "content_url": content_url,
                "created_on": "2026-01-01T00:00:00Z"
            }
        })))
        .mount(redmine)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/attachments/download/{id}/{filename}")))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(body.to_vec()))
        .mount(redmine)
        .await;
}

#[tokio::test]
async fn stdio_transport_returns_a_file_path_and_writes_the_real_bytes() {
    let h = support::harness(&[]).await;
    mock_attachment(&h.redmine, 42, "report.pdf", 5, b"hello").await;

    let result = call(&h, json!({"attachment_id": 42})).await;
    assert_eq!(result.is_error, Some(false));
    let structured = result.structured_content.expect("structured content");
    assert_eq!(structured["uri_type"], "file");
    assert!(structured.get("uri").is_none());
    assert_eq!(structured["filename"], "report.pdf");
    assert_eq!(structured["content_type"], "application/pdf");
    assert_eq!(structured["size"], 5);
    assert_eq!(structured["attachment_id"], 42);

    let file_path = structured["file_path"]
        .as_str()
        .expect("file_path should be a string");
    let contents = std::fs::read(file_path).expect("staged file should exist and be readable");
    assert_eq!(contents, b"hello");
}

#[tokio::test]
async fn http_transport_returns_a_files_uri_that_serves_the_same_bytes() {
    let harness = support::http_harness(&[]).await;
    mock_attachment(&harness.redmine, 7, "notes.txt", 6, b"abcdef").await;

    let transport = StreamableHttpClientTransport::from_uri(harness.mcp_url());
    let client = ().serve(transport).await.expect("client should connect over streamable HTTP");

    let mut request = CallToolRequestParams::new("get_redmine_attachment".to_string());
    request.arguments = json!({"attachment_id": 7}).as_object().cloned();
    let result = client
        .call_tool(request)
        .await
        .expect("call_tool should succeed");
    let structured = result.structured_content.expect("structured content");
    assert_eq!(structured["uri_type"], "http");
    assert!(structured.get("file_path").is_none());
    let uri = structured["uri"].as_str().expect("uri should be a string");
    // `public_base` is derived from the configured `SERVER_PORT` (default
    // 8000), not the OS-assigned ephemeral port `http_harness` actually
    // listens on (see `tests/support/mod.rs`) — a test-harness mismatch, not
    // a product bug. Fetch the same `/files/{uuid}` path against the real
    // ephemeral address instead of the literal returned URI.
    let uuid = uri
        .rsplit('/')
        .next()
        .expect("uri should have a /files/{uuid} path segment");
    let response = reqwest::Client::new()
        .get(harness.url(&format!("/files/{uuid}")))
        .send()
        .await
        .expect("fetching the /files uri should succeed");
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let body = response.bytes().await.expect("read response body");
    assert_eq!(&body[..], b"abcdef");
}

#[tokio::test]
async fn an_attachment_reported_larger_than_the_cap_is_refused_without_downloading() {
    let h = support::harness(&[("ATTACHMENT_MAX_DOWNLOAD_BYTES", "10")]).await;
    Mock::given(method("GET"))
        .and(path("/attachments/1.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "attachment": {
                "id": 1, "filename": "big.bin", "filesize": 1000,
                "content_url": format!("{}/attachments/download/1/big.bin", h.redmine.uri()),
                "created_on": "2026-01-01T00:00:00Z"
            }
        })))
        .mount(&h.redmine)
        .await;
    // No mock for the download endpoint at all: if the tool tried to fetch
    // it, wiremock would 404 rather than silently succeed, and this test
    // would still pass — assert the call count directly instead.
    let download_mock = Mock::given(method("GET"))
        .and(path("/attachments/download/1/big.bin"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![0u8; 1000]))
        .expect(0);
    h.redmine.register(download_mock).await;

    let result = call(&h, json!({"attachment_id": 1})).await;
    assert_eq!(result.is_error, Some(true));
    let structured = result.structured_content.expect("structured error");
    assert_eq!(structured["code"], "FILE_TOO_LARGE");
    assert_eq!(structured["retryable"], false);
}

#[tokio::test]
async fn download_aborts_when_actual_bytes_exceed_the_cap_despite_a_smaller_reported_filesize() {
    let dir = std::env::temp_dir().join(format!(
        "ruprogress-mcp-test-files-abort-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let dir_str = dir.to_string_lossy().into_owned();
    let h = support::harness(&[
        ("ATTACHMENTS_DIR", dir_str.as_str()),
        ("ATTACHMENT_MAX_DOWNLOAD_BYTES", "5"),
    ])
    .await;
    // The metadata under-reports the size (3 <= the 5-byte cap, so the cheap
    // pre-check passes), but the actual stream is 50 bytes: only the
    // mid-stream byte counter, not this field, can catch it.
    mock_attachment(&h.redmine, 1, "sneaky.bin", 3, &[0u8; 50]).await;

    let result = call(&h, json!({"attachment_id": 1})).await;
    assert_eq!(result.is_error, Some(true));
    let structured = result.structured_content.expect("structured error");
    assert_eq!(structured["code"], "FILE_TOO_LARGE");

    // The reservation's UUID directory must not survive an aborted download.
    let remaining = std::fs::read_dir(&dir).map_or(0, Iterator::count);
    assert_eq!(
        remaining, 0,
        "an aborted download must leave no UUID directory behind"
    );
}

#[tokio::test]
async fn store_full_refuses_a_download_that_would_exceed_the_store_cap() {
    let h = support::harness(&[
        ("ATTACHMENT_MAX_DOWNLOAD_BYTES", "1000"),
        ("ATTACHMENT_STORE_MAX_BYTES", "1200"),
    ])
    .await;
    mock_attachment(&h.redmine, 1, "first.bin", 1000, &[1u8; 1000]).await;
    mock_attachment(&h.redmine, 2, "second.bin", 300, &[2u8; 300]).await;

    let first = call(&h, json!({"attachment_id": 1})).await;
    assert_eq!(first.is_error, Some(false), "first download should fit");

    let second = call(&h, json!({"attachment_id": 2})).await;
    assert_eq!(second.is_error, Some(true));
    let structured = second.structured_content.expect("structured error");
    assert_eq!(structured["code"], "STORE_FULL");
    assert_eq!(structured["retryable"], false);
}

#[tokio::test]
async fn concurrent_downloads_cannot_together_exceed_the_store_cap() {
    use std::time::Duration;

    let dir = std::env::temp_dir().join(format!(
        "ruprogress-mcp-test-files-concurrent-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let dir_str = dir.to_string_lossy().into_owned();
    let h = support::harness(&[
        ("ATTACHMENTS_DIR", dir_str.as_str()),
        ("ATTACHMENT_MAX_DOWNLOAD_BYTES", "500"),
        ("ATTACHMENT_STORE_MAX_BYTES", "500"),
    ])
    .await;

    // attachment 1's own download body is deliberately slow, so its
    // reservation stays uncommitted for the whole window attachment 2's
    // call runs in — proving admission counts in-flight bytes, not just
    // committed ones.
    let content_url_1 = format!("{}/attachments/download/1/first.bin", h.redmine.uri());
    Mock::given(method("GET"))
        .and(path("/attachments/1.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "attachment": {
                "id": 1, "filename": "first.bin", "filesize": 300,
                "content_url": content_url_1,
                "created_on": "2026-01-01T00:00:00Z"
            }
        })))
        .mount(&h.redmine)
        .await;
    Mock::given(method("GET"))
        .and(path("/attachments/download/1/first.bin"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(Duration::from_millis(400))
                .set_body_bytes(vec![1u8; 300]),
        )
        .mount(&h.redmine)
        .await;
    mock_attachment(&h.redmine, 2, "second.bin", 300, &[2u8; 300]).await;

    let (first, second) = tokio::join!(
        call(&h, json!({"attachment_id": 1})),
        call(&h, json!({"attachment_id": 2}))
    );

    let successes = [&first, &second]
        .into_iter()
        .filter(|r| r.is_error == Some(false))
        .count();
    assert_eq!(
        successes, 1,
        "exactly one of the two concurrent downloads should fit under the cap"
    );
    let failed = if first.is_error == Some(true) {
        &first
    } else {
        &second
    };
    assert_eq!(
        failed
            .structured_content
            .as_ref()
            .expect("structured error")["code"],
        "STORE_FULL"
    );

    let remaining = std::fs::read_dir(&dir).map_or(0, Iterator::count);
    assert_eq!(
        remaining, 1,
        "only the successful download's uuid directory should remain"
    );
}

#[tokio::test]
async fn quota_is_reusable_after_an_aborted_download() {
    let h = support::harness(&[
        ("ATTACHMENT_MAX_DOWNLOAD_BYTES", "3"),
        ("ATTACHMENT_STORE_MAX_BYTES", "3"),
    ])
    .await;
    // Declared 3 bytes (fits the whole store), actual stream 50 bytes: the
    // download cap trips first and the reservation is aborted.
    mock_attachment(&h.redmine, 1, "sneaky.bin", 3, &[0u8; 50]).await;
    let first = call(&h, json!({"attachment_id": 1})).await;
    assert_eq!(first.is_error, Some(true));
    assert_eq!(
        first.structured_content.expect("structured error")["code"],
        "FILE_TOO_LARGE"
    );

    // A second, honest same-size download must still fit exactly — if the
    // aborted reservation's quota had leaked, this would be STORE_FULL.
    mock_attachment(&h.redmine, 2, "clean.bin", 3, b"abc").await;
    let second = call(&h, json!({"attachment_id": 2})).await;
    assert_eq!(second.is_error, Some(false));
}

#[tokio::test]
async fn a_404_from_redmine_surfaces_as_not_found() {
    let h = support::harness(&[]).await;
    Mock::given(method("GET"))
        .and(path("/attachments/999.json"))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({"errors": ["not found"]})))
        .mount(&h.redmine)
        .await;

    let result = call(&h, json!({"attachment_id": 999})).await;
    assert_eq!(result.is_error, Some(true));
    assert_eq!(
        result.structured_content.expect("structured error")["code"],
        "NOT_FOUND"
    );
}

#[tokio::test]
async fn a_content_url_on_a_foreign_origin_is_refused_and_leaves_no_reservation() {
    let dir = std::env::temp_dir().join(format!(
        "ruprogress-mcp-test-files-foreign-origin-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let dir_str = dir.to_string_lossy().into_owned();
    let h = support::harness(&[("ATTACHMENTS_DIR", dir_str.as_str())]).await;
    let foreign = wiremock::MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/attachments/1.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "attachment": {
                "id": 1, "filename": "report.pdf", "filesize": 5,
                "content_url": format!("{}/attachments/download/1/report.pdf", foreign.uri()),
                "created_on": "2026-01-01T00:00:00Z"
            }
        })))
        .mount(&h.redmine)
        .await;
    let download_mock = Mock::given(method("GET"))
        .and(path("/attachments/download/1/report.pdf"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"hello".to_vec()))
        .expect(0);
    foreign.register(download_mock).await;

    let result = call(&h, json!({"attachment_id": 1})).await;
    assert_eq!(result.is_error, Some(true));
    let structured = result.structured_content.expect("structured error");
    assert_eq!(structured["code"], "UNEXPECTED_RESPONSE");
    assert_eq!(structured["retryable"], false);

    assert!(
        foreign
            .received_requests()
            .await
            .unwrap_or_default()
            .is_empty(),
        "the foreign server must receive zero requests"
    );
    let remaining = std::fs::read_dir(&dir).map_or(0, Iterator::count);
    assert_eq!(
        remaining, 0,
        "a refused foreign-origin download must leave no UUID directory behind"
    );
}

#[tokio::test]
async fn list_files_returns_the_files_module_shape_including_version() {
    let h = support::harness(&[]).await;
    Mock::given(method("GET"))
        .and(path("/projects/1/files.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"files": [
            {
                "id": 11, "filename": "plan.pdf", "filesize": 1024,
                "content_type": "application/pdf",
                "description": "the plan",
                "content_url": format!("{}/attachments/download/11/plan.pdf", h.redmine.uri()),
                "digest": "d41d8cd98f00b204e9800998ecf8427e", "downloads": 3,
                "author": {"id": 1, "name": "Alice"},
                "created_on": "2026-01-01T00:00:00Z"
            },
            {
                "id": 12, "filename": "release.zip", "filesize": 2048,
                "content_url": format!("{}/attachments/download/12/release.zip", h.redmine.uri()),
                "digest": "d41d8cd98f00b204e9800998ecf8427e", "downloads": 0,
                "version": {"id": 2, "name": "1.0"},
                "created_on": "2026-01-01T00:00:00Z"
            }
        ]})))
        .mount(&h.redmine)
        .await;

    let result = call_tool(&h, "list_files", json!({"project_id": 1})).await;
    assert_eq!(result.is_error, Some(false));
    let structured = result.structured_content.expect("structured content");
    let files = structured["files"].as_array().expect("files array");
    assert_eq!(files.len(), 2);
    assert_eq!(files[0]["filename"], "plan.pdf");
    // `description` is Redmine-authored free text and is boundary-wrapped;
    // `filename` is structured metadata and is returned verbatim.
    assert!(
        files[0]["description"]
            .as_str()
            .expect("description should be a string")
            .contains("the plan")
    );
    assert_eq!(files[0]["digest"], "d41d8cd98f00b204e9800998ecf8427e");
    assert_eq!(files[0]["downloads"], 3);
    assert!(
        files[0]["author"]["name"]
            .as_str()
            .expect("author name should be a string")
            .contains("Alice")
    );
    assert!(files[0]["version"].is_null());
    assert!(
        files[1]["version"]["name"]
            .as_str()
            .expect("version name should be a string")
            .contains("1.0")
    );
}

#[tokio::test]
async fn list_files_rewrites_content_url_when_redmine_public_url_is_set() {
    let h = support::harness(&[("REDMINE_PUBLIC_URL", "https://public.example.com/redmine")]).await;
    Mock::given(method("GET"))
        .and(path("/projects/1/files.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"files": [
            {
                "id": 11, "filename": "plan.pdf", "filesize": 1024,
                "content_url": format!("{}/attachments/download/11/plan.pdf", h.redmine.uri()),
                "created_on": "2026-01-01T00:00:00Z"
            }
        ]})))
        .mount(&h.redmine)
        .await;

    let result = call_tool(&h, "list_files", json!({"project_id": 1})).await;
    assert_eq!(result.is_error, Some(false));
    let structured = result.structured_content.expect("structured content");
    assert_eq!(
        structured["files"][0]["content_url"],
        "https://public.example.com/redmine/attachments/download/11/plan.pdf"
    );
}

#[tokio::test]
async fn delete_file_without_the_confirm_flag_refuses_without_calling_redmine() {
    let h = support::harness(&[]).await;
    let delete_mock = Mock::given(method("DELETE"))
        .and(path("/attachments/1.json"))
        .respond_with(ResponseTemplate::new(204))
        .expect(0);
    h.redmine.register(delete_mock).await;

    let result = call_tool(&h, "delete_file", json!({"file_id": 1})).await;
    assert_eq!(result.is_error, Some(true));
    let structured = result.structured_content.expect("structured error");
    assert_eq!(structured["code"], "CONFIRMATION_REQUIRED");
    assert_eq!(structured["retryable"], false);
}

#[tokio::test]
async fn delete_file_with_the_confirm_flag_deletes_and_returns_success() {
    let h = support::harness(&[]).await;
    Mock::given(method("DELETE"))
        .and(path("/attachments/1.json"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&h.redmine)
        .await;

    let result = call_tool(
        &h,
        "delete_file",
        json!({"file_id": 1, "confirm_delete_any_attachment": true}),
    )
    .await;
    assert_eq!(result.is_error, Some(false));
    let structured = result.structured_content.expect("structured content");
    assert_eq!(structured["success"], true);
    assert_eq!(structured["deleted_file_id"], 1);
}

#[tokio::test]
async fn delete_file_404_surfaces_as_not_found() {
    let h = support::harness(&[]).await;
    Mock::given(method("DELETE"))
        .and(path("/attachments/999.json"))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({"errors": ["not found"]})))
        .mount(&h.redmine)
        .await;

    let result = call_tool(
        &h,
        "delete_file",
        json!({"file_id": 999, "confirm_delete_any_attachment": true}),
    )
    .await;
    assert_eq!(result.is_error, Some(true));
    assert_eq!(
        result.structured_content.expect("structured error")["code"],
        "NOT_FOUND"
    );
}

async fn mock_upload_flow(redmine: &wiremock::MockServer, id: u64, filename: &str) {
    Mock::given(method("POST"))
        .and(path("/uploads.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "upload": {"id": id, "token": format!("{id}.token")}
        })))
        .mount(redmine)
        .await;
    Mock::given(method("POST"))
        .and(path("/projects/1/files.json"))
        .respond_with(ResponseTemplate::new(204))
        .mount(redmine)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/attachments/{id}.json")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "attachment": {
                "id": id, "filename": filename, "filesize": 11,
                "content_type": "text/plain",
                "content_url": format!("{}/attachments/download/{id}/{filename}", redmine.uri()),
                "created_on": "2026-01-01T00:00:00Z"
            }
        })))
        .mount(redmine)
        .await;
}

fn base64_of(bytes: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

#[tokio::test]
async fn upload_file_with_content_base64_happy_path() {
    let h = support::harness(&[]).await;
    mock_upload_flow(&h.redmine, 99, "notes.txt").await;

    let result = call_tool(
        &h,
        "upload_file",
        json!({
            "project_id": 1,
            "filename": "notes.txt",
            "content_base64": base64_of(b"hello world"),
        }),
    )
    .await;
    assert_eq!(
        result.is_error,
        Some(false),
        "{:?}",
        result.structured_content
    );
    let structured = result.structured_content.expect("structured content");
    assert_eq!(structured["id"], 99);
    assert_eq!(structured["filename"], "notes.txt");
}

#[tokio::test]
async fn upload_file_without_a_source_is_source_required() {
    let h = support::harness(&[]).await;
    let result = call_tool(&h, "upload_file", json!({"project_id": 1})).await;
    assert_eq!(result.is_error, Some(true));
    assert_eq!(
        result.structured_content.expect("structured error")["code"],
        "SOURCE_REQUIRED"
    );
}

#[tokio::test]
async fn upload_file_with_two_sources_is_source_required() {
    let h = support::harness(&[]).await;
    let result = call_tool(
        &h,
        "upload_file",
        json!({
            "project_id": 1,
            "content_base64": base64_of(b"x"),
            "file_path": "/tmp/whatever.txt",
        }),
    )
    .await;
    assert_eq!(result.is_error, Some(true));
    assert_eq!(
        result.structured_content.expect("structured error")["code"],
        "SOURCE_REQUIRED"
    );
}

#[tokio::test]
async fn upload_file_with_source_url_is_unsupported_source() {
    let h = support::harness(&[]).await;
    let result = call_tool(
        &h,
        "upload_file",
        json!({"project_id": 1, "source_url": "https://example.com/report.pdf"}),
    )
    .await;
    assert_eq!(result.is_error, Some(true));
    let structured = result.structured_content.expect("structured error");
    assert_eq!(structured["code"], "UNSUPPORTED_SOURCE");
    let message = structured["error"].as_str().expect("error message");
    assert!(message.contains("content_base64"));
    assert!(message.contains("file_path"));
}

#[tokio::test]
async fn upload_file_content_base64_without_filename_is_a_protocol_error() {
    let h = support::harness(&[]).await;
    let mut request = CallToolRequestParams::new("upload_file".to_string());
    request.arguments = json!({"project_id": 1, "content_base64": base64_of(b"x")})
        .as_object()
        .cloned();
    let result = h.client.call_tool(request).await;
    assert!(
        result.is_err(),
        "a missing filename with content_base64 should be a protocol-level error, not an in-band one"
    );
}

#[tokio::test]
async fn upload_file_malformed_base64_is_a_protocol_error() {
    let h = support::harness(&[]).await;
    let mut request = CallToolRequestParams::new("upload_file".to_string());
    request.arguments = json!({
        "project_id": 1, "filename": "x.txt", "content_base64": "not valid base64!!"
    })
    .as_object()
    .cloned();
    let result = h.client.call_tool(request).await;
    assert!(result.is_err());
}

#[tokio::test]
async fn upload_file_413_from_create_upload_maps_to_file_too_large() {
    let h = support::harness(&[]).await;
    Mock::given(method("POST"))
        .and(path("/uploads.json"))
        .respond_with(ResponseTemplate::new(413))
        .mount(&h.redmine)
        .await;

    let result = call_tool(
        &h,
        "upload_file",
        json!({"project_id": 1, "filename": "big.bin", "content_base64": base64_of(b"x")}),
    )
    .await;
    assert_eq!(result.is_error, Some(true));
    let structured = result.structured_content.expect("structured error");
    assert_eq!(structured["code"], "FILE_TOO_LARGE");
}

#[tokio::test]
async fn upload_file_422_from_create_upload_also_maps_to_file_too_large() {
    let h = support::harness(&[]).await;
    Mock::given(method("POST"))
        .and(path("/uploads.json"))
        .respond_with(ResponseTemplate::new(422).set_body_json(json!({
            "errors": ["File is too big"]
        })))
        .mount(&h.redmine)
        .await;

    let result = call_tool(
        &h,
        "upload_file",
        json!({"project_id": 1, "filename": "big.bin", "content_base64": base64_of(b"x")}),
    )
    .await;
    assert_eq!(result.is_error, Some(true));
    let structured = result.structured_content.expect("structured error");
    assert_eq!(structured["code"], "FILE_TOO_LARGE");
}

#[tokio::test]
async fn upload_file_422_from_create_project_file_is_validation_failed_not_file_too_large() {
    let h = support::harness(&[]).await;
    Mock::given(method("POST"))
        .and(path("/uploads.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "upload": {"id": 1, "token": "1.token"}
        })))
        .mount(&h.redmine)
        .await;
    Mock::given(method("POST"))
        .and(path("/projects/1/files.json"))
        .respond_with(ResponseTemplate::new(422).set_body_json(json!({
            "errors": ["Version does not exist"]
        })))
        .mount(&h.redmine)
        .await;

    let result = call_tool(
        &h,
        "upload_file",
        json!({
            "project_id": 1, "filename": "x.txt", "content_base64": base64_of(b"x"),
            "version_id": 999
        }),
    )
    .await;
    assert_eq!(result.is_error, Some(true));
    let structured = result.structured_content.expect("structured error");
    assert_eq!(structured["code"], "VALIDATION_FAILED");
}

#[tokio::test]
async fn upload_file_is_blocked_in_read_only_mode() {
    let h = support::harness(&[("REDMINE_MCP_READ_ONLY", "true")]).await;
    let tools = h
        .client
        .list_tools(None)
        .await
        .expect("list_tools should succeed");
    assert!(!tools.tools.iter().any(|t| t.name == "upload_file"));
}

#[tokio::test]
async fn cleanup_attachment_files_is_not_registered_by_default() {
    let h = support::harness(&[]).await;
    let tools = h
        .client
        .list_tools(None)
        .await
        .expect("list_tools should succeed");
    assert!(
        !tools
            .tools
            .iter()
            .any(|t| t.name == "cleanup_attachment_files")
    );
}

#[tokio::test]
async fn cleanup_attachment_files_reports_zero_when_nothing_is_expired() {
    let h = support::harness(&[("REDMINE_MCP_EXPOSE_ADMIN_TOOLS", "true")]).await;
    let result = call_tool(&h, "cleanup_attachment_files", json!({})).await;
    assert_eq!(result.is_error, Some(false));
    let structured = result.structured_content.expect("structured content");
    assert_eq!(structured["cleaned_files"], 0);
    assert_eq!(structured["cleaned_bytes"], 0);
    assert_eq!(structured["cleaned_mb"], 0.0);
}

#[tokio::test]
async fn cleanup_attachment_files_sweeps_an_expired_download() {
    let h = support::harness(&[
        ("REDMINE_MCP_EXPOSE_ADMIN_TOOLS", "true"),
        ("ATTACHMENT_EXPIRES_MINUTES", "1"),
    ])
    .await;
    mock_attachment(&h.redmine, 1, "old.bin", 5, b"hello").await;
    let download = call(&h, json!({"attachment_id": 1})).await;
    assert_eq!(download.is_error, Some(false));

    // `sweep_expired` reaps by directory mtime; backdate it past the
    // 1-minute TTL instead of sleeping in the test.
    let dir = download.structured_content.expect("structured content")["file_path"]
        .as_str()
        .expect("file_path should be a string")
        .to_string();
    let entry_dir = std::path::Path::new(&dir)
        .parent()
        .expect("staged file has a parent uuid directory");
    let stale = std::time::SystemTime::now() - std::time::Duration::from_mins(2);
    std::fs::File::open(entry_dir)
        .expect("open the uuid directory")
        .set_modified(stale)
        .expect("backdate the uuid directory's mtime");

    let result = call_tool(&h, "cleanup_attachment_files", json!({})).await;
    assert_eq!(result.is_error, Some(false));
    let structured = result.structured_content.expect("structured content");
    assert_eq!(structured["cleaned_files"], 1);
    assert_eq!(structured["cleaned_bytes"], 5);
}

#[tokio::test]
async fn cleanup_attachment_files_still_works_in_read_only_mode() {
    let h = support::harness(&[
        ("REDMINE_MCP_EXPOSE_ADMIN_TOOLS", "true"),
        ("REDMINE_MCP_READ_ONLY", "true"),
    ])
    .await;
    let result = call_tool(&h, "cleanup_attachment_files", json!({})).await;
    assert_eq!(result.is_error, Some(false));
}
