//! Finding 11 baseline: counts and orders the sequential upstream round
//! trips `create_redmine_issue`'s `uploads[]` flow issues, and freezes the
//! current partial-failure output shape as a golden the follow-up
//! implementation must not silently change. See
//! `plans/finding-09-11-performance-baselines.md`.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    // This is a measurement harness, not the stdio transport; the
    // `--ignored --nocapture` run this test exists for needs its output.
    clippy::print_stdout
)]

mod support;

use std::time::{Duration, Instant};

use rmcp::model::CallToolRequestParams;
use serde_json::{Value, json};
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, ResponseTemplate};

fn base64_of(bytes: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

async fn call(h: &support::Harness, name: &str, args: Value) -> rmcp::model::CallToolResult {
    let mut request = CallToolRequestParams::new(name.to_string());
    request.arguments = args.as_object().cloned();
    h.client
        .call_tool(request)
        .await
        .expect("call_tool should succeed")
}

fn issue_json(id: u64) -> Value {
    json!({
        "issue": {
            "id": id,
            "project": {"id": 1, "name": "P"},
            "tracker": {"id": 1, "name": "Bug"},
            "status": {"id": 1, "name": "New"},
            "priority": {"id": 1, "name": "Normal"},
            "author": {"id": 1, "name": "A"},
            "subject": "s",
            "created_on": "2026-01-01T00:00:00Z",
            "updated_on": "2026-01-01T00:00:00Z"
        }
    })
}

const RTT: Duration = Duration::from_millis(20);

/// Every upstream call this flow can make responds after a fixed 20ms
/// delay, regardless of which file it belongs to — enough to tell
/// sequential execution from concurrent by wall-clock alone.
async fn mount_delayed(h: &support::Harness) {
    Mock::given(method("POST"))
        .and(path("/uploads.json"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(RTT)
                .set_body_json(json!({"upload": {"id": 900, "token": "900.token"}})),
        )
        .mount(&h.redmine)
        .await;
    Mock::given(method("POST"))
        .and(path("/issues.json"))
        .respond_with(
            ResponseTemplate::new(201)
                .set_delay(RTT)
                .set_body_json(issue_json(42)),
        )
        .mount(&h.redmine)
        .await;
    Mock::given(method("GET"))
        .and(path("/attachments/900.json"))
        .respond_with(ResponseTemplate::new(200).set_delay(RTT).set_body_json(json!({
            "attachment": {
                "id": 900, "filename": "f.txt", "filesize": 1,
                "content_type": "text/plain",
                "content_url": format!("{}/attachments/download/900/f.txt", h.redmine.uri()),
                "created_on": "2026-01-01T00:00:00Z"
            }
        })))
        .mount(&h.redmine)
        .await;
}

/// Not a CI gate — a measurement. Run explicitly:
/// `cargo test -p ruprogress-mcp --test upload_round_trips -- --ignored --nocapture`.
#[tokio::test]
#[ignore = "prints measured round-trip counts and wall-clock; not a pass/fail gate"]
async fn upload_flow_round_trips_scale_linearly_and_run_sequentially() {
    for n in [1usize, 5, 10] {
        let h = support::harness(&[]).await;
        mount_delayed(&h).await;

        let uploads: Vec<Value> = (0..n)
            .map(|i| json!({"content_base64": base64_of(b"x"), "filename": format!("f{i}.txt")}))
            .collect();

        let start = Instant::now();
        let result = call(
            &h,
            "create_redmine_issue",
            json!({"project_id": 1, "subject": "s", "uploads": uploads}),
        )
        .await;
        let elapsed = start.elapsed();
        assert_ne!(
            result.is_error,
            Some(true),
            "{:?}",
            result.structured_content
        );

        let requests = h.redmine.received_requests().await.unwrap_or_default();
        let paths: Vec<String> = requests.iter().map(|r| r.url.path().to_string()).collect();

        // Ordering: n sequential POST /uploads.json (resolve_and_mint_
        // issue_uploads's minting loop), then the one issue POST, then n
        // sequential GET /attachments/*.json (fetch_attachments) —
        // issues.rs:1568-1625.
        let uploads_seen = paths
            .iter()
            .take(n)
            .filter(|p| p.as_str() == "/uploads.json")
            .count();
        assert_eq!(
            uploads_seen, n,
            "expected {n} /uploads.json calls first: {paths:?}"
        );
        assert_eq!(
            paths[n], "/issues.json",
            "expected the issue POST right after the uploads: {paths:?}"
        );
        let attachment_gets = paths[n + 1..]
            .iter()
            .filter(|p| p.starts_with("/attachments/"))
            .count();
        assert_eq!(
            attachment_gets, n,
            "expected {n} attachment GETs after the issue POST: {paths:?}"
        );

        let expected_round_trips = 2 * n + 1;
        assert_eq!(
            requests.len(),
            expected_round_trips,
            "round-trip count for {n} files"
        );

        // Sequential, not concurrent: every round trip pays the mock's
        // fixed 20ms delay, so wall-clock should scale linearly with the
        // count — (uploads + 1 + attachments) x RTT — rather than staying
        // flat as it would if the mint/fetch loops ran concurrently.
        let expected_wall_clock = RTT * u32::try_from(expected_round_trips).unwrap();
        println!(
            "n={n}: {expected_round_trips} round trips, wall-clock={elapsed:?} \
             (arithmetic expectation {expected_wall_clock:?})"
        );
        assert!(
            elapsed >= expected_wall_clock.mul_f64(0.8),
            "n={n}: wall-clock {elapsed:?} is far below the sequential expectation \
             {expected_wall_clock:?} — uploads or attachment fetches may be running concurrently"
        );
    }
}

/// Golden: a mint failure partway through the sequential loop
/// (`resolve_and_mint_issue_uploads`, `issues.rs:1568`) surfaces as this
/// exact `FILE_TOO_LARGE` envelope, sends no request for any upload after
/// the failing one, and never creates the issue. Any follow-up that
/// parallelises the mint loop must keep this shape unchanged (or this test
/// must be updated deliberately, not incidentally).
#[tokio::test]
async fn a_mint_failure_partway_sends_no_later_requests_and_creates_no_issue() {
    let h = support::harness(&[]).await;
    for i in 0..2u64 {
        Mock::given(method("POST"))
            .and(path("/uploads.json"))
            .and(query_param("filename", format!("f{i}.txt")))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "upload": {"id": 900 + i, "token": format!("{}.token", 900 + i)}
            })))
            .mount(&h.redmine)
            .await;
    }
    Mock::given(method("POST"))
        .and(path("/uploads.json"))
        .and(query_param("filename", "f2.txt"))
        .respond_with(ResponseTemplate::new(422))
        .mount(&h.redmine)
        .await;

    let uploads: Vec<Value> = (0..5)
        .map(|i| json!({"content_base64": base64_of(b"x"), "filename": format!("f{i}.txt")}))
        .collect();
    let result = call(
        &h,
        "create_redmine_issue",
        json!({"project_id": 1, "subject": "s", "uploads": uploads}),
    )
    .await;

    assert_eq!(result.is_error, Some(true));
    let structured = result.structured_content.expect("structured error body");
    assert_eq!(structured["code"], "FILE_TOO_LARGE");
    assert_eq!(
        structured["error"],
        "Redmine rejected the upload as too large"
    );

    let requests = h.redmine.received_requests().await.unwrap_or_default();
    let paths: Vec<String> = requests.iter().map(|r| r.url.path().to_string()).collect();
    assert_eq!(
        paths,
        vec!["/uploads.json", "/uploads.json", "/uploads.json"],
        "exactly 3 upload attempts (2 succeed, the 3rd fails), nothing after: {paths:?}"
    );
}
