//! e2e: `upload_file`'s `file_path` source (J7/N4 in
//! `plans/phase-5e-upload-and-cleanup.md`) — the table-driven traversal/
//! symlink/FIFO/device suite the parent plan's risk #2 asks for. Every
//! rejection must come back as `PATH_NOT_ALLOWED` with no raw path in the
//! message; every acceptance must round-trip the real bytes through the
//! two-step upload flow.
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

async fn call_tool(h: &support::Harness, args: serde_json::Value) -> rmcp::model::CallToolResult {
    let mut request = CallToolRequestParams::new("upload_file".to_string());
    request.arguments = args.as_object().cloned();
    h.client
        .call_tool(request)
        .await
        .expect("call_tool should succeed at the protocol level")
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
                "id": id, "filename": filename, "filesize": 5,
                "content_url": format!("{}/attachments/download/{id}/{filename}", redmine.uri()),
                "created_on": "2026-01-01T00:00:00Z"
            }
        })))
        .mount(redmine)
        .await;
}

fn unique_dir(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "ruprogress-mcp-test-uploadpaths-{name}-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ))
}

fn assert_is_path_not_allowed(result: &rmcp::model::CallToolResult) -> serde_json::Value {
    assert_eq!(result.is_error, Some(true));
    let structured = result.structured_content.clone().expect("structured error");
    assert_eq!(structured["code"], "PATH_NOT_ALLOWED");
    structured
}

fn assert_path_not_allowed(result: &rmcp::model::CallToolResult, raw_path: &str) {
    let structured = assert_is_path_not_allowed(result);
    let message = structured.to_string();
    assert!(
        !message.contains(raw_path),
        "PATH_NOT_ALLOWED must never echo the rejected path verbatim: {message}"
    );
}

#[tokio::test]
async fn a_path_inside_an_allowed_root_is_accepted() {
    let root = unique_dir("root");
    std::fs::create_dir_all(&root).unwrap();
    let file = root.join("report.pdf");
    std::fs::write(&file, b"hello").unwrap();

    let h = support::harness(&[(
        "REDMINE_MCP_UPLOAD_FILE_ROOTS",
        root.to_string_lossy().as_ref(),
    )])
    .await;
    mock_upload_flow(&h.redmine, 1, "report.pdf").await;

    let result = call_tool(
        &h,
        json!({"project_id": 1, "file_path": file.to_string_lossy()}),
    )
    .await;
    assert_eq!(result.is_error, Some(false));
    let structured = result.structured_content.expect("structured content");
    assert_eq!(structured["filename"], "report.pdf");

    std::fs::remove_dir_all(&root).ok();
}

#[tokio::test]
async fn a_path_inside_attachments_dir_is_accepted_with_no_roots_configured() {
    let attachments_dir = unique_dir("attachments-dir");
    std::fs::create_dir_all(&attachments_dir).unwrap();
    let file = attachments_dir.join("staged.txt");
    std::fs::write(&file, b"hello").unwrap();

    let h = support::harness(&[(
        "ATTACHMENTS_DIR",
        attachments_dir.to_string_lossy().as_ref(),
    )])
    .await;
    mock_upload_flow(&h.redmine, 1, "staged.txt").await;

    let result = call_tool(
        &h,
        json!({"project_id": 1, "file_path": file.to_string_lossy()}),
    )
    .await;
    assert_eq!(
        result.is_error,
        Some(false),
        "ATTACHMENTS_DIR is an implicit allowed root (N3): {:?}",
        result.structured_content
    );

    std::fs::remove_dir_all(&attachments_dir).ok();
}

#[tokio::test]
async fn a_path_outside_every_configured_root_is_rejected() {
    let outside = unique_dir("outside");
    std::fs::create_dir_all(&outside).unwrap();
    let file = outside.join("secret.txt");
    std::fs::write(&file, b"secret").unwrap();

    let h = support::harness(&[]).await;
    let result = call_tool(
        &h,
        json!({"project_id": 1, "file_path": file.to_string_lossy()}),
    )
    .await;
    assert_path_not_allowed(&result, &file.to_string_lossy());

    std::fs::remove_dir_all(&outside).ok();
}

#[tokio::test]
async fn a_relative_path_is_rejected() {
    let h = support::harness(&[]).await;
    let result = call_tool(
        &h,
        json!({"project_id": 1, "file_path": "relative/path.txt"}),
    )
    .await;
    assert_path_not_allowed(&result, "relative/path.txt");
}

#[tokio::test]
async fn a_nonexistent_path_under_an_allowed_root_is_rejected_the_same_way() {
    let root = unique_dir("missing-root");
    std::fs::create_dir_all(&root).unwrap();
    let missing = root.join("does-not-exist.txt");

    let h = support::harness(&[(
        "REDMINE_MCP_UPLOAD_FILE_ROOTS",
        root.to_string_lossy().as_ref(),
    )])
    .await;
    let result = call_tool(
        &h,
        json!({"project_id": 1, "file_path": missing.to_string_lossy()}),
    )
    .await;
    // Same code and no existence oracle: a caller cannot distinguish
    // "outside the roots" from "does not exist" (N4).
    assert_path_not_allowed(&result, &missing.to_string_lossy());

    std::fs::remove_dir_all(&root).ok();
}

#[cfg(unix)]
#[tokio::test]
async fn a_symlink_escaping_the_allowed_root_is_rejected() {
    let root = unique_dir("symlink-root");
    let outside = unique_dir("symlink-target");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    let target = outside.join("secret.txt");
    std::fs::write(&target, b"secret").unwrap();
    let link = root.join("link.txt");
    std::os::unix::fs::symlink(&target, &link).unwrap();

    let h = support::harness(&[(
        "REDMINE_MCP_UPLOAD_FILE_ROOTS",
        root.to_string_lossy().as_ref(),
    )])
    .await;
    let result = call_tool(
        &h,
        json!({"project_id": 1, "file_path": link.to_string_lossy()}),
    )
    .await;
    assert_path_not_allowed(&result, &target.to_string_lossy());

    std::fs::remove_dir_all(&root).ok();
    std::fs::remove_dir_all(&outside).ok();
}

#[cfg(unix)]
#[tokio::test]
async fn a_symlinked_parent_directory_is_rejected() {
    let root = unique_dir("symlinked-parent-root");
    let outside = unique_dir("symlinked-parent-target");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("file.txt"), b"secret").unwrap();
    let link_dir = root.join("subdir");
    std::os::unix::fs::symlink(&outside, &link_dir).unwrap();

    let h = support::harness(&[(
        "REDMINE_MCP_UPLOAD_FILE_ROOTS",
        root.to_string_lossy().as_ref(),
    )])
    .await;
    let requested = link_dir.join("file.txt");
    let result = call_tool(
        &h,
        json!({"project_id": 1, "file_path": requested.to_string_lossy()}),
    )
    .await;
    assert_path_not_allowed(&result, &outside.to_string_lossy());

    std::fs::remove_dir_all(&root).ok();
    std::fs::remove_dir_all(&outside).ok();
}

#[cfg(unix)]
#[tokio::test]
async fn a_fifo_is_rejected() {
    let root = unique_dir("fifo-root");
    std::fs::create_dir_all(&root).unwrap();
    let fifo = root.join("pipe");
    let status = std::process::Command::new("mkfifo")
        .arg(&fifo)
        .status()
        .expect("mkfifo should run on this platform");
    assert!(status.success(), "mkfifo failed");

    let h = support::harness(&[(
        "REDMINE_MCP_UPLOAD_FILE_ROOTS",
        root.to_string_lossy().as_ref(),
    )])
    .await;
    let result = call_tool(
        &h,
        json!({"project_id": 1, "file_path": fifo.to_string_lossy()}),
    )
    .await;
    assert_path_not_allowed(&result, &fifo.to_string_lossy());

    std::fs::remove_dir_all(&root).ok();
}

#[cfg(unix)]
#[tokio::test]
async fn a_device_node_is_rejected() {
    // `/dev/null` always exists on Unix and reading it is harmless; it is a
    // character device, not a regular file, which is exactly the condition
    // this test exercises.
    let h = support::harness(&[("REDMINE_MCP_UPLOAD_FILE_ROOTS", "/dev")]).await;
    let result = call_tool(&h, json!({"project_id": 1, "file_path": "/dev/null"})).await;
    assert_is_path_not_allowed(&result);
}

#[tokio::test]
async fn a_file_over_the_50_mib_cap_is_rejected_as_file_too_large_without_reading_it() {
    let root = unique_dir("big-file-root");
    std::fs::create_dir_all(&root).unwrap();
    let big = root.join("big.bin");
    // A sparse file: instant to create, no real 50+ MiB write, but its
    // reported length is what the fstat-based size check must catch.
    let file = std::fs::File::create(&big).unwrap();
    file.set_len(50 * 1024 * 1024 + 1).unwrap();
    drop(file);

    let h = support::harness(&[(
        "REDMINE_MCP_UPLOAD_FILE_ROOTS",
        root.to_string_lossy().as_ref(),
    )])
    .await;
    // No mock for /uploads.json at all: if the tool tried to upload it,
    // wiremock would 404 rather than silently succeed, and this test would
    // still pass — assert the call count directly instead.
    let upload_mock = Mock::given(method("POST"))
        .and(path("/uploads.json"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0);
    h.redmine.register(upload_mock).await;

    let result = call_tool(
        &h,
        json!({"project_id": 1, "file_path": big.to_string_lossy()}),
    )
    .await;
    assert_eq!(result.is_error, Some(true));
    let structured = result.structured_content.expect("structured error");
    assert_eq!(structured["code"], "FILE_TOO_LARGE");

    std::fs::remove_dir_all(&root).ok();
}
