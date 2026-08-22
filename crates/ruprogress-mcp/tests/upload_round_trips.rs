//! Finding 11: measures the upstream round trips `create_redmine_issue`'s
//! and `update_redmine_issue`'s `uploads[]` flows issue, after the fix —
//! `create_redmine_issue` folds attachment metadata into its issue POST via
//! `include=attachments` (`create_issue_with_attachments`, no per-id GETs
//! at all); `update_redmine_issue` cannot do that (the include would also
//! return the issue's pre-existing attachments) and instead runs
//! `fetch_attachments`' per-id GETs up to `MAX_CONCURRENT_ATTACHMENT_
//! FETCHES` at a time. Also freezes the current partial-failure output
//! shape as a golden the follow-up implementation must not silently
//! change. See `plans/finding-09-11-performance-baselines.md` and
//! `plans/finding-11-concurrent-attachment-fetch.md`.
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
use wiremock::matchers::{method, path, path_regex, query_param};
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

fn uploads_of(n: usize) -> Vec<Value> {
    (0..n)
        .map(|i| json!({"content_base64": base64_of(b"x"), "filename": format!("f{i}.txt")}))
        .collect()
}

/// Every upstream call `create_redmine_issue`'s upload flow can make
/// responds after a fixed 20ms delay — enough to tell sequential execution
/// from concurrent by wall-clock alone. The issue POST returns 201 with no
/// `attachments` key; that's fine here, since these tests only count and
/// order requests.
async fn mount_delayed_create(h: &support::Harness) {
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
        .and(query_param("include", "attachments"))
        .respond_with(
            ResponseTemplate::new(201)
                .set_delay(RTT)
                .set_body_json(issue_json(42)),
        )
        .mount(&h.redmine)
        .await;
}

/// Not a CI gate — a measurement. Run explicitly:
/// `cargo test -p ruprogress-mcp --test upload_round_trips -- --ignored --nocapture`.
#[tokio::test]
#[ignore = "prints measured round-trip counts and wall-clock; not a pass/fail gate"]
async fn create_issue_upload_flow_is_n_plus_one_round_trips_with_no_attachment_gets() {
    for n in [1usize, 5, 10] {
        let h = support::harness(&[]).await;
        mount_delayed_create(&h).await;

        let start = Instant::now();
        let result = call(
            &h,
            "create_redmine_issue",
            json!({"project_id": 1, "subject": "s", "uploads": uploads_of(n)}),
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
        // issue_uploads's minting loop, unchanged), then exactly one issue
        // POST with attachments folded in — no attachment GETs at all
        // (issues.rs's create_redmine_issue/create_issue_with_attachments).
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
            paths.get(n),
            Some(&"/issues.json".to_string()),
            "expected the issue POST right after the uploads: {paths:?}"
        );
        assert_eq!(
            paths.len(),
            n + 1,
            "expected no attachment GETs after the issue POST: {paths:?}"
        );

        let expected_round_trips = n + 1;
        assert_eq!(
            requests.len(),
            expected_round_trips,
            "round-trip count for {n} files"
        );

        // Sequential minting, then one combined write: wall-clock should
        // scale linearly with (uploads + 1) x RTT.
        let expected_wall_clock = RTT * u32::try_from(expected_round_trips).unwrap();
        println!(
            "create n={n}: {expected_round_trips} round trips, wall-clock={elapsed:?} \
             (arithmetic expectation {expected_wall_clock:?})"
        );
        assert!(
            elapsed >= expected_wall_clock.mul_f64(0.8),
            "n={n}: wall-clock {elapsed:?} is far below the sequential expectation \
             {expected_wall_clock:?} — uploads may be running concurrently"
        );
    }
}

/// Every upstream call `update_redmine_issue`'s upload flow can make
/// responds after a fixed 20ms delay: the PUT (204), the follow-up GET
/// `update_issue` always does to return the full resource, and every
/// attachment GET (`fetch_attachments`).
async fn mount_delayed_update(h: &support::Harness, issue_id: u64) {
    Mock::given(method("POST"))
        .and(path("/uploads.json"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(RTT)
                .set_body_json(json!({"upload": {"id": 900, "token": "900.token"}})),
        )
        .mount(&h.redmine)
        .await;
    Mock::given(method("PUT"))
        .and(path(format!("/issues/{issue_id}.json")))
        .respond_with(ResponseTemplate::new(204).set_delay(RTT))
        .mount(&h.redmine)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/issues/{issue_id}.json")))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(RTT)
                .set_body_json(issue_json(issue_id)),
        )
        .mount(&h.redmine)
        .await;
    Mock::given(method("GET"))
        .and(path_regex(r"^/attachments/\d+\.json$"))
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
async fn update_issue_attachment_fetches_are_bounded_concurrent() {
    for n in [1usize, 5, 10] {
        let h = support::harness(&[]).await;
        mount_delayed_update(&h, 7).await;

        let start = Instant::now();
        let result = call(
            &h,
            "update_redmine_issue",
            json!({"issue_id": 7, "uploads": uploads_of(n)}),
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
        let calls: Vec<(String, String)> = requests
            .iter()
            .map(|r| (r.method.to_string(), r.url.path().to_string()))
            .collect();

        // Ordering: n sequential POST /uploads.json, then the PUT, then the
        // GET `update_issue` always does, then n attachment GETs (run
        // concurrently, but `buffered` still delivers them in id order —
        // here that's just "last" since they all share one id/path).
        let uploads_seen = calls
            .iter()
            .take(n)
            .filter(|(m, p)| m == "POST" && p == "/uploads.json")
            .count();
        assert_eq!(
            uploads_seen, n,
            "expected {n} /uploads.json calls first: {calls:?}"
        );
        assert_eq!(
            calls.get(n),
            Some(&("PUT".to_string(), "/issues/7.json".to_string())),
            "expected the PUT right after the uploads: {calls:?}"
        );
        assert_eq!(
            calls.get(n + 1),
            Some(&("GET".to_string(), "/issues/7.json".to_string())),
            "expected update_issue's follow-up GET right after the PUT: {calls:?}"
        );
        let attachment_gets = calls[n + 2..]
            .iter()
            .filter(|(m, p)| m == "GET" && p.starts_with("/attachments/"))
            .count();
        assert_eq!(
            attachment_gets, n,
            "expected {n} attachment GETs after the follow-up GET: {calls:?}"
        );

        let expected_round_trips = n + 2 + n;
        assert_eq!(
            requests.len(),
            expected_round_trips,
            "round-trip count for {n} files"
        );

        // n sequential uploads, then PUT, then GET, then
        // ceil(n / MAX_CONCURRENT_ATTACHMENT_FETCHES) buffered phases —
        // issues.rs's fetch_attachments/MAX_CONCURRENT_ATTACHMENT_FETCHES.
        let attachment_phases = n.div_ceil(4);
        let expected_phases = n + 2 + attachment_phases;
        let expected_wall_clock = RTT * u32::try_from(expected_phases).unwrap();
        println!(
            "update n={n}: {expected_round_trips} round trips over {expected_phases} phases, \
             wall-clock={elapsed:?} (arithmetic expectation {expected_wall_clock:?})"
        );
        assert!(
            elapsed >= expected_wall_clock.mul_f64(0.8),
            "n={n}: wall-clock {elapsed:?} is far below the expected {expected_phases} \
             sequential phases ({expected_wall_clock:?}) — the concurrency bound may be gone"
        );
        assert!(
            elapsed <= expected_wall_clock.mul_f64(1.35),
            "n={n}: wall-clock {elapsed:?} is far above the expected {expected_phases} \
             phases ({expected_wall_clock:?}) — attachment fetches may be re-serialised"
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
