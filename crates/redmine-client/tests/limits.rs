//! `Limits::max_response_bytes`, enforced while streaming: declared
//! `Content-Length` rejected before any body byte is read, a body exactly at
//! the limit still decodes, and an over-limit chunked body with no
//! `Content-Length` is aborted mid-stream (see `chunked_over_limit_body...`
//! below).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use redmine_client::{Credential, Error, IssueId, JournalId};
use secrecy::SecretString;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpListener;
use wiremock::matchers::{method, path};
use wiremock::{Mock, ResponseTemplate};

fn api_key() -> Credential {
    Credential::ApiKey(SecretString::from("test-api-key"))
}

fn oauth_client_credential() -> Credential {
    Credential::Basic {
        user: "upstream-client".to_string(),
        pass: SecretString::from("upstream-secret"),
    }
}

#[tokio::test]
async fn oversized_declared_content_length_is_rejected_before_reading_a_body_byte() {
    let (server, client) = support::mock_redmine_limited(50).await;
    Mock::given(method("GET"))
        .and(path("/my/account.json"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(vec![b'x'; 100], "application/json"))
        .mount(&server)
        .await;

    let cred = api_key();
    let err = client
        .as_user(&cred)
        .current_user()
        .await
        .expect_err("oversized body should be rejected");
    match err {
        Error::LimitExceeded {
            what,
            limit,
            actual,
        } => {
            assert_eq!(what, "response bytes");
            assert_eq!(limit, 50);
            assert_eq!(actual, 100);
        }
        other => panic!("expected Error::LimitExceeded, got {other:?}"),
    }
}

#[tokio::test]
async fn body_exactly_at_the_limit_still_decodes() {
    let json_body =
        br#"{"user":{"id":1,"firstname":"A","lastname":"B","created_on":"2020-01-01T00:00:00Z"}}"#
            .to_vec();
    let limit = json_body.len() as u64;
    let (server, client) = support::mock_redmine_limited(limit).await;
    Mock::given(method("GET"))
        .and(path("/my/account.json"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(json_body, "application/json"))
        .mount(&server)
        .await;

    let cred = api_key();
    let user = client
        .as_user(&cred)
        .current_user()
        .await
        .expect("a body exactly at the limit should still decode");
    assert_eq!(user.id, 1);
}

#[tokio::test]
async fn oversized_status_error_body_is_limit_exceeded_and_not_retried() {
    let (server, client) = support::mock_redmine_limited(50).await;
    Mock::given(method("GET"))
        .and(path("/my/account.json"))
        .respond_with(ResponseTemplate::new(500).set_body_raw(vec![b'x'; 200], "application/json"))
        .mount(&server)
        .await;

    let cred = api_key();
    let err = client
        .as_user(&cred)
        .current_user()
        .await
        .expect_err("oversized 500 body should be rejected");
    assert!(
        matches!(err, Error::LimitExceeded { .. }),
        "expected Error::LimitExceeded, got {err:?}"
    );

    let requests = server.received_requests().await.expect("requests recorded");
    assert_eq!(
        requests.len(),
        1,
        "a LimitExceeded status-error must not be retried"
    );
}

#[tokio::test]
async fn oversized_oauth_error_body_is_limit_exceeded() {
    let (server, client) = support::mock_redmine_limited(50).await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(ResponseTemplate::new(400).set_body_raw(vec![b'x'; 200], "application/json"))
        .mount(&server)
        .await;

    let cred = oauth_client_credential();
    let err = client
        .as_user(&cred)
        .exchange_authorization_code("code", "https://mcp.example.com/callback", "verifier")
        .await
        .expect_err("oversized oauth error body should be rejected");
    assert!(
        matches!(err, Error::LimitExceeded { .. }),
        "expected Error::LimitExceeded, got {err:?}"
    );
}

#[tokio::test]
async fn under_limit_oauth_error_body_is_unchanged() {
    let (server, client) = support::mock_redmine().await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
            "error": "invalid_grant",
        })))
        .mount(&server)
        .await;

    let cred = oauth_client_credential();
    let err = client
        .as_user(&cred)
        .refresh_access_token(&SecretString::from("stale-refresh-token"))
        .await
        .expect_err("400 should be an error");
    match err {
        Error::OAuth { status, error, .. } => {
            assert_eq!(status, 400);
            assert_eq!(error, "invalid_grant");
        }
        other => panic!("expected Error::OAuth, got {other:?}"),
    }
}

#[tokio::test]
async fn bodiless_head_404_still_maps_to_not_found() {
    let (server, client) = support::mock_redmine().await;
    Mock::given(method("GET"))
        .and(path("/issues/1.json"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;

    let cred = api_key();
    let err = client
        .as_user(&cred)
        .get_issue(IssueId(1), &[])
        .await
        .expect_err("404 should be an error");
    assert!(matches!(err, Error::NotFound));
}

#[tokio::test]
async fn no_content_length_204_still_succeeds_through_put_json() {
    let (server, client) = support::mock_redmine().await;
    Mock::given(method("PUT"))
        .and(path("/journals/1.json"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&server)
        .await;

    let cred = api_key();
    client
        .as_user(&cred)
        .update_journal(
            JournalId(1),
            &redmine_client::model::journal::JournalUpdate::default(),
        )
        .await
        .expect("a bodiless 204 should still succeed");
}

/// Proves the abort is real, not just a check after `Content-Length`: a raw
/// `chunked` (no `Content-Length`) response streamed by hand, well past the
/// client's tiny configured limit. The client must give up and drop the
/// connection long before the fake server finishes writing.
#[tokio::test]
async fn chunked_over_limit_body_with_no_content_length_is_aborted_mid_stream() {
    const LIMIT: u64 = 4 * 1024; // 4 KiB
    const TOTAL_BODY: usize = 4 * 1024 * 1024; // 4 MiB, far above LIMIT
    const CHUNK: usize = 16 * 1024;

    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind loopback listener");
    let addr = listener.local_addr().expect("local addr");

    let written = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let written_writer = written.clone();

    let server_task = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.expect("accept connection");

        // Drain the request so the client's write side doesn't stall.
        let mut buf = [0_u8; 1024];
        let _ = socket.read(&mut buf).await;

        let header =
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nContent-Type: application/octet-stream\r\n\r\n";
        if socket.write_all(header).await.is_err() {
            return;
        }

        let chunk_data = vec![b'a'; CHUNK];
        let mut total_sent = 0_usize;
        while total_sent < TOTAL_BODY {
            let framed = format!("{:x}\r\n", chunk_data.len());
            if socket.write_all(framed.as_bytes()).await.is_err() {
                break;
            }
            if socket.write_all(&chunk_data).await.is_err() {
                break;
            }
            if socket.write_all(b"\r\n").await.is_err() {
                break;
            }
            total_sent = total_sent.saturating_add(chunk_data.len());
            written_writer.store(total_sent, std::sync::atomic::Ordering::SeqCst);
        }
        // Terminating chunk, if the client is still connected.
        let _ = socket.write_all(b"0\r\n\r\n").await;
    });

    let base: url::Url = format!("http://{addr}/").parse().expect("valid url");
    let client = redmine_client::RedmineClientBuilder::new(base)
        .credential(api_key())
        .limits(redmine_client::Limits {
            max_response_bytes: LIMIT,
            ..redmine_client::Limits::default()
        })
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .expect("client should build");

    let cred = api_key();
    let err = client
        .as_user(&cred)
        .current_user()
        .await
        .expect_err("an over-limit chunked body should be rejected");
    assert!(
        matches!(err, Error::LimitExceeded { .. }),
        "expected Error::LimitExceeded, got {err:?}"
    );

    let _ = server_task.await;
    let sent = written.load(std::sync::atomic::Ordering::SeqCst);
    assert!(
        sent < TOTAL_BODY / 4,
        "client should have aborted well before the server finished writing \
         the full body (sent {sent} of {TOTAL_BODY} bytes)"
    );
}
