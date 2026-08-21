//! `GET /files/{uuid}`: serving, expiry, and the `/files`-scoped
//! `Host` allowlist check, asserted with raw `reqwest`
//! against the real router — same style as `tests/http_edge.rs`.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

mod support;

use reqwest::StatusCode;
use uuid::Uuid;

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .build()
        .expect("build a test HTTP client")
}

#[tokio::test]
async fn an_unknown_uuid_is_404() {
    let harness = support::http_harness(&[]).await;
    let response = client()
        .get(harness.url(&format!("/files/{}", Uuid::new_v4())))
        .send()
        .await
        .expect("request should complete");
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_stored_file_is_served_with_the_expected_headers() {
    let harness = support::http_harness(&[]).await;
    let reservation = harness
        .attachments
        .reserve(7, "../../etc/report.pdf", 9)
        .await
        .expect("reserve should succeed");
    tokio::fs::write(&reservation.path, b"pdf-bytes")
        .await
        .expect("write the reserved file");
    let uuid = reservation.uuid;
    harness
        .attachments
        .commit(reservation, Some("application/pdf".to_string()), 9);

    let response = client()
        .get(harness.url(&format!("/files/{uuid}")))
        .send()
        .await
        .expect("request should complete");
    assert_eq!(response.status(), StatusCode::OK);
    let headers = response.headers().clone();
    assert_eq!(
        headers.get("content-type").and_then(|v| v.to_str().ok()),
        Some("application/pdf")
    );
    assert_eq!(
        headers.get("content-length").and_then(|v| v.to_str().ok()),
        Some("9")
    );
    assert_eq!(
        headers.get("cache-control").and_then(|v| v.to_str().ok()),
        Some("no-store")
    );
    assert_eq!(
        headers
            .get("x-content-type-options")
            .and_then(|v| v.to_str().ok()),
        Some("nosniff")
    );
    let disposition = headers
        .get("content-disposition")
        .and_then(|v| v.to_str().ok())
        .expect("content-disposition header");
    assert!(disposition.starts_with("attachment;"));
    // The sanitised basename (last path segment only), not the traversal
    // string the "Redmine filename" contained.
    assert!(disposition.contains("report.pdf"));
    assert!(!disposition.contains(".."));

    let body = response.bytes().await.expect("read body");
    assert_eq!(&body[..], b"pdf-bytes");
}

#[tokio::test]
async fn a_hostile_content_type_falls_back_to_octet_stream() {
    let harness = support::http_harness(&[]).await;
    let reservation = harness
        .attachments
        .reserve(1, "f.bin", 1)
        .await
        .expect("reserve should succeed");
    tokio::fs::write(&reservation.path, b"x")
        .await
        .expect("write");
    let uuid = reservation.uuid;
    // A CRLF-bearing content type could not form a valid header value.
    harness
        .attachments
        .commit(reservation, Some("text/plain\r\nX-Evil: 1".to_string()), 1);

    let response = client()
        .get(harness.url(&format!("/files/{uuid}")))
        .send()
        .await
        .expect("request should complete");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok()),
        Some("application/octet-stream")
    );
}

#[tokio::test]
async fn a_removed_entry_is_404_through_the_route() {
    // The store's own TTL-expiry logic is covered exhaustively in
    // `src/attachments.rs`'s unit tests (which can use a millisecond TTL);
    // this test only needs to prove the route reflects "the store no longer
    // has it" as a 404, using `abort` to reach that state without waiting
    // out a real (whole-minute-granularity) TTL.
    let harness = support::http_harness(&[]).await;
    let reservation = harness
        .attachments
        .reserve(1, "f.txt", 1)
        .await
        .expect("reserve should succeed");
    tokio::fs::write(&reservation.path, b"x")
        .await
        .expect("write");
    let uuid = reservation.uuid;
    harness.attachments.abort(&reservation).await;
    assert!(harness.attachments.get(uuid).await.is_none());

    let response = client()
        .get(harness.url(&format!("/files/{uuid}")))
        .send()
        .await
        .expect("request should complete");
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_disallowed_host_header_is_403() {
    let harness = support::http_harness(&[]).await;
    let reservation = harness
        .attachments
        .reserve(1, "f.txt", 1)
        .await
        .expect("reserve should succeed");
    tokio::fs::write(&reservation.path, b"x")
        .await
        .expect("write");
    let uuid = reservation.uuid;
    harness.attachments.commit(reservation, None, 1);

    let response = client()
        .get(harness.url(&format!("/files/{uuid}")))
        .header("host", "evil.example.com")
        .send()
        .await
        .expect("request should complete");
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn a_malformed_uuid_segment_is_not_a_500() {
    let harness = support::http_harness(&[]).await;
    let response = client()
        .get(harness.url("/files/not-a-uuid"))
        .send()
        .await
        .expect("request should complete");
    assert!(response.status().is_client_error());
}
