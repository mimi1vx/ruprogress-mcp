//! e2e: the Gantt tool family — `get_gantt_chart`'s happy path,
//! `include_closed`/date-filter query params, `limit` clamping, and
//! the dominant error path.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

mod support;

use rmcp::model::CallToolRequestParams;
use serde_json::{Value, json};
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, ResponseTemplate};

fn body_of(result: &rmcp::model::CallToolResult) -> Value {
    let text = result
        .content
        .iter()
        .filter_map(|c| c.as_text())
        .map(|t| t.text.clone())
        .collect::<Vec<_>>()
        .join("\n");
    text.lines()
        .last()
        .and_then(|l| serde_json::from_str(l).ok())
        .expect("last content block should be the JSON body")
}

async fn call(h: &support::Harness, args: Value) -> rmcp::model::CallToolResult {
    let mut request = CallToolRequestParams::new("get_gantt_chart");
    request.arguments = args.as_object().cloned();
    h.client
        .call_tool(request)
        .await
        .expect("call_tool should succeed")
}

async fn mount_project(h: &support::Harness) {
    Mock::given(method("GET"))
        .and(path("/projects/5.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "project": {
                "id": 5, "name": "Demo", "identifier": "demo",
                "created_on": "2026-01-01T00:00:00Z", "updated_on": "2026-01-01T00:00:00Z"
            }
        })))
        .mount(&h.redmine)
        .await;
}

#[tokio::test]
async fn happy_path_round_trips_dates_hierarchy_and_milestones() {
    let h = support::harness(&[]).await;
    mount_project(&h).await;
    Mock::given(method("GET"))
        .and(path("/issues.json"))
        .and(query_param("project_id", "5"))
        .and(query_param("status_id", "open"))
        .and(query_param("limit", "100"))
        .and(query_param("offset", "0"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "issues": [
                {
                    "id": 10, "project": {"id": 5, "name": "Demo"},
                    "tracker": {"id": 1, "name": "Bug"}, "status": {"id": 1, "name": "New"},
                    "priority": {"id": 1, "name": "Normal"}, "author": {"id": 1, "name": "A"},
                    "subject": "Parent task", "start_date": "2026-01-01", "due_date": "2026-01-10",
                    "done_ratio": 50,
                    "created_on": "2026-01-01T00:00:00Z", "updated_on": "2026-01-01T00:00:00Z"
                },
                {
                    "id": 11, "project": {"id": 5, "name": "Demo"},
                    "tracker": {"id": 1, "name": "Bug"}, "status": {"id": 1, "name": "New"},
                    "priority": {"id": 1, "name": "Normal"}, "author": {"id": 1, "name": "A"},
                    "subject": "Child task", "parent": {"id": 10},
                    "start_date": "2026-01-02", "due_date": "2026-01-05", "done_ratio": 0,
                    "created_on": "2026-01-01T00:00:00Z", "updated_on": "2026-01-01T00:00:00Z"
                }
            ],
            "total_count": 2, "offset": 0, "limit": 100
        })))
        .mount(&h.redmine)
        .await;
    Mock::given(method("GET"))
        .and(path("/projects/5/versions.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "versions": [
                {
                    "id": 3, "name": "1.0", "status": "open", "due_date": "2026-02-01",
                    "project": {"id": 5, "name": "Demo"},
                    "created_on": "2026-01-01T00:00:00Z", "updated_on": "2026-01-01T00:00:00Z"
                }
            ]
        })))
        .mount(&h.redmine)
        .await;

    let body = body_of(&call(&h, json!({"project_id": 5})).await);
    assert_eq!(body["project_id"], 5);
    assert!(
        body["project_name"]
            .as_str()
            .unwrap()
            .starts_with("<<<untrusted:")
    );

    let issues = body["issues"].as_array().unwrap();
    assert_eq!(issues.len(), 2);
    assert_eq!(issues[0]["id"], 10);
    assert_eq!(issues[0]["start_date"], "2026-01-01");
    assert_eq!(issues[0]["due_date"], "2026-01-10");
    assert_eq!(issues[0]["done_ratio"], 50);
    assert!(issues[0]["parent_id"].is_null());
    assert_eq!(issues[1]["id"], 11);
    assert_eq!(issues[1]["parent_id"], 10);
    assert!(
        issues[1]["subject"]
            .as_str()
            .unwrap()
            .starts_with("<<<untrusted:")
    );

    let milestones = body["milestones"].as_array().unwrap();
    assert_eq!(milestones.len(), 1);
    assert_eq!(milestones[0]["id"], 3);
    assert_eq!(milestones[0]["due_date"], "2026-02-01");
    assert!(
        milestones[0]["name"]
            .as_str()
            .unwrap()
            .starts_with("<<<untrusted:")
    );

    assert_eq!(body["pagination"]["total"], 2);
    assert_eq!(body["pagination"]["limit"], 100);
}

#[tokio::test]
async fn include_closed_true_sends_status_id_star() {
    let h = support::harness(&[]).await;
    mount_project(&h).await;
    Mock::given(method("GET"))
        .and(path("/issues.json"))
        .and(query_param("status_id", "*"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "issues": [], "total_count": 0, "offset": 0, "limit": 100
        })))
        .mount(&h.redmine)
        .await;
    Mock::given(method("GET"))
        .and(path("/projects/5/versions.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"versions": []})))
        .mount(&h.redmine)
        .await;

    let result = call(&h, json!({"project_id": 5, "include_closed": true})).await;
    assert_ne!(result.is_error, Some(true));
}

#[tokio::test]
async fn date_filters_reach_redmine_as_operator_syntax() {
    let h = support::harness(&[]).await;
    mount_project(&h).await;
    Mock::given(method("GET"))
        .and(path("/issues.json"))
        .and(query_param("start_date", ">=2026-01-01"))
        .and(query_param("due_date", "<=2026-06-30"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "issues": [], "total_count": 0, "offset": 0, "limit": 100
        })))
        .mount(&h.redmine)
        .await;
    Mock::given(method("GET"))
        .and(path("/projects/5/versions.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"versions": []})))
        .mount(&h.redmine)
        .await;

    let result = call(
        &h,
        json!({
            "project_id": 5,
            "start_date_after": "2026-01-01",
            "due_date_before": "2026-06-30",
        }),
    )
    .await;
    assert_ne!(result.is_error, Some(true));
}

#[tokio::test]
async fn limit_clamps_to_five_hundred() {
    let h = support::harness(&[]).await;
    mount_project(&h).await;
    Mock::given(method("GET"))
        .and(path("/issues.json"))
        .and(query_param("limit", "500"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "issues": [], "total_count": 0, "offset": 0, "limit": 500
        })))
        .mount(&h.redmine)
        .await;
    Mock::given(method("GET"))
        .and(path("/projects/5/versions.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"versions": []})))
        .mount(&h.redmine)
        .await;

    let body = body_of(&call(&h, json!({"project_id": 5, "limit": 99_999})).await);
    assert_eq!(body["pagination"]["limit"], 500);
}

#[tokio::test]
async fn dominant_error_is_in_band_not_a_protocol_error() {
    let h = support::harness(&[]).await;
    Mock::given(method("GET"))
        .and(path("/projects/5.json"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&h.redmine)
        .await;
    // Mounted so `try_join!`'s other two branches succeed and the project's
    // 404 is unambiguously what surfaces.
    Mock::given(method("GET"))
        .and(path("/issues.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "issues": [], "total_count": 0, "offset": 0, "limit": 100
        })))
        .mount(&h.redmine)
        .await;
    Mock::given(method("GET"))
        .and(path("/projects/5/versions.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"versions": []})))
        .mount(&h.redmine)
        .await;

    let result = call(&h, json!({"project_id": 5})).await;
    assert_eq!(result.is_error, Some(true));
    let structured = result.structured_content.unwrap();
    assert_eq!(structured["code"], "NOT_FOUND");
    assert_eq!(structured["retryable"], false);
}
