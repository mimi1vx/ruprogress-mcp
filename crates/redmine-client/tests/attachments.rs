//! Happy-path and dominant-error-path tests for the attachment/Files-module
//! client primitives.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use bytes::Bytes;
use futures_util::TryStreamExt as _;
use redmine_client::model::upload::ProjectFileCreate;
use redmine_client::{AttachmentId, Credential, Error, ProjectIdent};
use secrecy::SecretString;
use wiremock::matchers::{body_json, header, method, path, query_param};
use wiremock::{Mock, ResponseTemplate};

fn cred() -> Credential {
    Credential::ApiKey(SecretString::from("k"))
}

// --- get_attachment ---

#[tokio::test]
async fn get_attachment_happy_path() {
    let (server, client) = support::mock_redmine().await;
    Mock::given(method("GET"))
        .and(path("/attachments/6243.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "attachment": {
                "id": 6243, "filename": "test.txt", "filesize": 124,
                "content_url": "https://example.com/attachments/download/6243/test.txt",
                "created_on": "2026-01-01T00:00:00Z"
            }
        })))
        .mount(&server)
        .await;

    let cred = cred();
    let attachment = client
        .as_user(&cred)
        .get_attachment(AttachmentId(6243))
        .await
        .expect("get_attachment should succeed");
    assert_eq!(attachment.id, 6243);
    assert_eq!(attachment.filename, "test.txt");
    // Never populated by GET /attachments/{id}.json — see the doc comment
    // on `Attachment` for why these are not modeled as container fields.
    assert!(attachment.digest.is_none());
    assert!(attachment.downloads.is_none());
}

#[tokio::test]
async fn get_attachment_not_found() {
    let (server, client) = support::mock_redmine().await;
    Mock::given(method("GET"))
        .and(path("/attachments/999.json"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;

    let cred = cred();
    let err = client
        .as_user(&cred)
        .get_attachment(AttachmentId(999))
        .await
        .expect_err("an unknown id should 404");
    assert!(matches!(err, Error::NotFound));
}

// --- delete_attachment ---

#[tokio::test]
async fn delete_attachment_succeeds_on_204() {
    let (server, client) = support::mock_redmine().await;
    Mock::given(method("DELETE"))
        .and(path("/attachments/6243.json"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&server)
        .await;

    let cred = cred();
    client
        .as_user(&cred)
        .delete_attachment(AttachmentId(6243))
        .await
        .expect("delete_attachment should succeed");
}

#[tokio::test]
async fn delete_attachment_forbidden() {
    let (server, client) = support::mock_redmine().await;
    Mock::given(method("DELETE"))
        .and(path("/attachments/6243.json"))
        .respond_with(ResponseTemplate::new(403))
        .mount(&server)
        .await;

    let cred = cred();
    let err = client
        .as_user(&cred)
        .delete_attachment(AttachmentId(6243))
        .await
        .expect_err("a non-deletable attachment should 403");
    assert!(matches!(err, Error::Forbidden));
}

// --- list_project_files ---

#[tokio::test]
async fn list_project_files_sends_no_pagination_params() {
    let (server, client) = support::mock_redmine().await;
    Mock::given(method("GET"))
        .and(path("/projects/demo/files.json"))
        .and(wiremock::matchers::query_param_is_missing("limit"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "files": [{
                "id": 12, "filename": "foo-1.0-setup.exe", "filesize": 74_753_799,
                "content_url": "https://example.com/attachments/download/12/foo-1.0-setup.exe",
                "created_on": "2026-01-04T09:12:32Z",
                "version": {"id": 2, "name": "1.0"},
                "digest": "1276481102f218c981e0324180bafd9f", "downloads": 12
            }]
        })))
        .mount(&server)
        .await;

    let cred = cred();
    let project = ProjectIdent::Identifier("demo".parse().unwrap());
    let files = client
        .as_user(&cred)
        .list_project_files(&project)
        .await
        .expect("list_project_files should succeed");
    assert_eq!(files.len(), 1);
    let file = files.first().unwrap();
    assert_eq!(file.downloads, Some(12));
    assert_eq!(file.version.as_ref().unwrap().name, "1.0");
}

#[tokio::test]
async fn list_project_files_errors_loudly_on_a_paginated_envelope() {
    let (server, client) = support::mock_redmine().await;
    Mock::given(method("GET"))
        .and(path("/projects/demo/files.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "files": [], "total_count": 0
        })))
        .mount(&server)
        .await;

    let cred = cred();
    let project = ProjectIdent::Identifier("demo".parse().unwrap());
    let err = client
        .as_user(&cred)
        .list_project_files(&project)
        .await
        .expect_err("a total_count field must not be silently treated as page one");
    assert!(matches!(err, Error::Decode { .. }));
}

// --- create_upload ---

#[tokio::test]
async fn create_upload_sets_octet_stream_content_type_and_sends_the_body() {
    let (server, client) = support::mock_redmine().await;
    Mock::given(method("POST"))
        .and(path("/uploads.json"))
        .and(header("Content-Type", "application/octet-stream"))
        .and(query_param("filename", "report.pdf"))
        .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
            "upload": {"id": 42, "token": "42.abcdef0123456789"}
        })))
        .expect(1)
        .mount(&server)
        .await;

    let cred = cred();
    let upload = client
        .as_user(&cred)
        .create_upload(
            Bytes::from_static(b"%PDF-1.4 fake"),
            Some("report.pdf"),
            None,
        )
        .await
        .expect("create_upload should succeed");
    assert_eq!(upload.id, 42);
    assert_eq!(upload.token, "42.abcdef0123456789");
}

#[tokio::test]
async fn create_upload_over_size_limit_maps_to_api_error() {
    let (server, client) = support::mock_redmine().await;
    Mock::given(method("POST"))
        .and(path("/uploads.json"))
        .respond_with(ResponseTemplate::new(422).set_body_json(serde_json::json!({
            "errors": ["This file cannot be uploaded because it exceeds the maximum allowed file size (1024000)"]
        })))
        .mount(&server)
        .await;

    let cred = cred();
    let err = client
        .as_user(&cred)
        .create_upload(Bytes::from_static(b"too big"), None, None)
        .await
        .expect_err("an over-cap upload should fail");
    match err {
        Error::Api { status, errors } => {
            assert_eq!(status, http::StatusCode::UNPROCESSABLE_ENTITY);
            assert!(
                errors
                    .first()
                    .unwrap()
                    .contains("maximum allowed file size")
            );
        }
        other => panic!("expected Api, got {other:?}"),
    }
}

// --- create_project_file ---

#[tokio::test]
async fn create_project_file_sends_expected_body_and_no_follow_up_get() {
    let (server, client) = support::mock_redmine().await;
    Mock::given(method("POST"))
        .and(path("/projects/demo/files.json"))
        .and(body_json(serde_json::json!({
            "file": {"token": "42.abcdef0123456789", "filename": "report.pdf"}
        })))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;

    let cred = cred();
    let project = ProjectIdent::Identifier("demo".parse().unwrap());
    let new = ProjectFileCreate {
        token: "42.abcdef0123456789".to_string(),
        filename: Some("report.pdf".to_string()),
        ..Default::default()
    };
    client
        .as_user(&cred)
        .create_project_file(&project, &new)
        .await
        .expect("create_project_file should succeed");
}

// --- download_attachment ---

#[tokio::test]
async fn download_attachment_streams_the_body() {
    let (server, client) = support::mock_redmine().await;
    let content_url = format!("{}/attachments/download/6243/test.txt", server.uri());
    Mock::given(method("GET"))
        .and(path("/attachments/download/6243/test.txt"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_bytes(b"hello attachment bytes".to_vec())
                .insert_header("x-checksum", "deadbeef"),
        )
        .mount(&server)
        .await;

    let cred = cred();
    let (headers, stream) = client
        .as_user(&cred)
        .download_attachment(&content_url)
        .await
        .expect("download_attachment should succeed");
    assert_eq!(headers.get("x-checksum").unwrap(), "deadbeef");

    let chunks: Vec<Bytes> = stream
        .try_collect()
        .await
        .expect("stream should yield all chunks without error");
    let body: Vec<u8> = chunks.into_iter().flatten().collect();
    assert_eq!(body, b"hello attachment bytes");
}

#[tokio::test]
async fn download_attachment_rejects_an_unparseable_url() {
    let (_server, client) = support::mock_redmine().await;
    let cred = cred();
    // Not `.expect_err(...)`: the `Ok` type embeds a non-`Debug` stream, so
    // a manual match is used instead.
    match client.as_user(&cred).download_attachment("not a url").await {
        Ok(_) => panic!("an unparseable content_url must be rejected before any request is sent"),
        Err(err) => assert!(matches!(err, Error::Config { .. })),
    }
}

#[tokio::test]
async fn download_attachment_not_found() {
    let (server, client) = support::mock_redmine().await;
    let content_url = format!("{}/attachments/download/999/gone.txt", server.uri());
    Mock::given(method("GET"))
        .and(path("/attachments/download/999/gone.txt"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;

    let cred = cred();
    match client
        .as_user(&cred)
        .download_attachment(&content_url)
        .await
    {
        Ok(_) => panic!("a 404 should surface before any stream item is produced"),
        Err(err) => assert!(matches!(err, Error::NotFound)),
    }
}
