//! e2e: `get_redmine_attachment`. Happy path per transport (the `uri_type`
//! branch), the `FILE_TOO_LARGE` pre-check and mid-stream enforcement,
//! `STORE_FULL`, and the dominant `redmine_client::Error` passthrough.
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

async fn call(h: &support::Harness, args: serde_json::Value) -> rmcp::model::CallToolResult {
    let mut request = CallToolRequestParams::new("get_redmine_attachment".to_string());
    request.arguments = args.as_object().cloned();
    h.client
        .call_tool(request)
        .await
        .expect("call_tool should succeed")
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
