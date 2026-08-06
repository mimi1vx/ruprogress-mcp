//! e2e: the discovery-tool family (4a) — happy path and dominant error path
//! per tool, plus the two behaviours specific to this family: `limit`
//! clamping on `list_redmine_users` (E4) and `list_project_trackers`
//! rejecting a hostile project identifier as an argument error (D5).
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

fn content_text(result: &rmcp::model::CallToolResult) -> String {
    result
        .content
        .iter()
        .filter_map(|c| c.as_text())
        .map(|t| t.text.clone())
        .collect::<Vec<_>>()
        .join("\n")
}

async fn call(h: &support::Harness, name: &str) -> rmcp::model::CallToolResult {
    h.client
        .call_tool(CallToolRequestParams::new(name.to_string()))
        .await
        .expect("call_tool should succeed")
}

fn body_of(result: &rmcp::model::CallToolResult) -> Value {
    let text = content_text(result);
    text.lines()
        .last()
        .and_then(|l| serde_json::from_str(l).ok())
        .expect("last content block should be the JSON body")
}

// --- list_redmine_trackers ---

#[tokio::test]
async fn list_redmine_trackers_happy_path() {
    let h = support::harness(&[]).await;
    Mock::given(method("GET"))
        .and(path("/trackers.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "trackers": [{"id": 1, "name": "Bug", "description": "Software defect"}]
        })))
        .mount(&h.redmine)
        .await;

    let body = body_of(&call(&h, "list_redmine_trackers").await);
    let trackers = body["trackers"].as_array().unwrap();
    assert_eq!(trackers.len(), 1);
    assert_eq!(trackers[0]["id"], 1);
    assert!(trackers[0]["name"].as_str().unwrap().contains("Bug"));
    assert!(
        trackers[0]["name"]
            .as_str()
            .unwrap()
            .starts_with("<<<untrusted:")
    );
}

#[tokio::test]
async fn list_redmine_trackers_dominant_error_is_in_band() {
    let h = support::harness(&[]).await;
    Mock::given(method("GET"))
        .and(path("/trackers.json"))
        .respond_with(ResponseTemplate::new(403))
        .mount(&h.redmine)
        .await;

    let result = call(&h, "list_redmine_trackers").await;
    assert_eq!(result.is_error, Some(true));
    let structured = result.structured_content.unwrap();
    assert_eq!(structured["code"], "FORBIDDEN");
    assert_eq!(structured["retryable"], false);
}

// --- list_project_trackers ---

#[tokio::test]
async fn list_project_trackers_happy_path_distinguishes_requested_from_enabled() {
    let h = support::harness(&[]).await;
    Mock::given(method("GET"))
        .and(path("/projects/5.json"))
        .and(query_param("include", "trackers"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "project": {
                "id": 5, "name": "P", "identifier": "p",
                "created_on": "2026-01-01T00:00:00Z", "updated_on": "2026-01-01T00:00:00Z",
                "trackers": [{"id": 1, "name": "Bug"}]
            }
        })))
        .mount(&h.redmine)
        .await;

    let result = h
        .client
        .call_tool({
            let mut request = CallToolRequestParams::new("list_project_trackers");
            request.arguments = json!({"project_id": 5}).as_object().cloned();
            request
        })
        .await
        .expect("call_tool should succeed");
    let body = body_of(&result);
    let trackers = body["trackers"].as_array().unwrap();
    assert_eq!(trackers.len(), 1);
    assert_eq!(trackers[0]["id"], 1);
}

#[tokio::test]
async fn list_project_trackers_accepts_a_slug_identifier() {
    let h = support::harness(&[]).await;
    Mock::given(method("GET"))
        .and(path("/projects/my-project.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "project": {
                "id": 5, "name": "P", "identifier": "my-project",
                "created_on": "2026-01-01T00:00:00Z", "updated_on": "2026-01-01T00:00:00Z",
                "trackers": []
            }
        })))
        .mount(&h.redmine)
        .await;

    let result = h
        .client
        .call_tool({
            let mut request = CallToolRequestParams::new("list_project_trackers");
            request.arguments = json!({"project_id": "my-project"}).as_object().cloned();
            request
        })
        .await
        .expect("call_tool should succeed");
    let body = body_of(&result);
    assert_eq!(body["trackers"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn list_project_trackers_rejects_hostile_identifiers_as_argument_errors() {
    let h = support::harness(&[]).await;
    let cases = ["../admin", "a/b", "%2e%2e", "", &"a".repeat(101)];
    for case in cases {
        let result = h
            .client
            .call_tool({
                let mut request = CallToolRequestParams::new("list_project_trackers");
                request.arguments = json!({"project_id": case}).as_object().cloned();
                request
            })
            .await;
        assert!(
            result.is_err(),
            "{case:?} should be rejected as an argument error, not a tool result"
        );
    }
}

#[tokio::test]
async fn list_project_trackers_dominant_error_is_in_band() {
    let h = support::harness(&[]).await;
    Mock::given(method("GET"))
        .and(path("/projects/5.json"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&h.redmine)
        .await;

    let result = h
        .client
        .call_tool({
            let mut request = CallToolRequestParams::new("list_project_trackers");
            request.arguments = json!({"project_id": 5}).as_object().cloned();
            request
        })
        .await
        .expect("call_tool should succeed");
    assert_eq!(result.is_error, Some(true));
    assert_eq!(result.structured_content.unwrap()["code"], "NOT_FOUND");
}

// --- list_redmine_issue_statuses ---

#[tokio::test]
async fn list_redmine_issue_statuses_happy_path() {
    let h = support::harness(&[]).await;
    Mock::given(method("GET"))
        .and(path("/issue_statuses.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "issue_statuses": [{"id": 1, "name": "New", "is_closed": false}]
        })))
        .mount(&h.redmine)
        .await;

    let body = body_of(&call(&h, "list_redmine_issue_statuses").await);
    let statuses = body["issue_statuses"].as_array().unwrap();
    assert_eq!(statuses.len(), 1);
    assert_eq!(statuses[0]["is_closed"], false);
}

#[tokio::test]
async fn list_redmine_issue_statuses_dominant_error_is_in_band() {
    let h = support::harness(&[]).await;
    Mock::given(method("GET"))
        .and(path("/issue_statuses.json"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&h.redmine)
        .await;

    let result = call(&h, "list_redmine_issue_statuses").await;
    assert_eq!(result.is_error, Some(true));
    assert_eq!(result.structured_content.unwrap()["code"], "UNAUTHORIZED");
}

// --- list_redmine_issue_priorities ---

#[tokio::test]
async fn list_redmine_issue_priorities_happy_path() {
    let h = support::harness(&[]).await;
    Mock::given(method("GET"))
        .and(path("/enumerations/issue_priorities.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "issue_priorities": [{"id": 2, "name": "Normal", "is_default": true, "active": true}]
        })))
        .mount(&h.redmine)
        .await;

    let body = body_of(&call(&h, "list_redmine_issue_priorities").await);
    let priorities = body["issue_priorities"].as_array().unwrap();
    assert_eq!(priorities.len(), 1);
    assert_eq!(priorities[0]["is_default"], true);
}

#[tokio::test]
async fn list_redmine_issue_priorities_dominant_error_is_in_band() {
    let h = support::harness(&[]).await;
    // No mock mounted: the request fails to match anything wiremock knows
    // about, which wiremock answers with a 404.
    let result = call(&h, "list_redmine_issue_priorities").await;
    assert_eq!(result.is_error, Some(true));
    assert_eq!(result.structured_content.unwrap()["code"], "NOT_FOUND");
}

// --- list_redmine_users ---

#[tokio::test]
async fn list_redmine_users_happy_path() {
    let h = support::harness(&[]).await;
    Mock::given(method("GET"))
        .and(path("/users.json"))
        .and(query_param("limit", "25"))
        .and(query_param("offset", "0"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "users": [{
                "id": 1, "login": "alice", "firstname": "Alice", "lastname": "Example",
                "created_on": "2026-01-01T00:00:00Z"
            }],
            "total_count": 1, "offset": 0, "limit": 25
        })))
        .mount(&h.redmine)
        .await;

    let body = body_of(&call(&h, "list_redmine_users").await);
    let users = body["users"].as_array().unwrap();
    assert_eq!(users.len(), 1);
    assert_eq!(users[0]["login"], "alice");
    assert!(
        users[0]["firstname"]
            .as_str()
            .unwrap()
            .starts_with("<<<untrusted:")
    );
    assert_eq!(body["pagination"]["limit"], 25);
}

#[tokio::test]
async fn list_redmine_users_clamps_limit_to_1_100_and_echoes_the_effective_value() {
    let h = support::harness(&[]).await;
    Mock::given(method("GET"))
        .and(path("/users.json"))
        .and(query_param("limit", "1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "users": [], "total_count": 0, "offset": 0, "limit": 1
        })))
        .mount(&h.redmine)
        .await;

    let result = h
        .client
        .call_tool({
            let mut request = CallToolRequestParams::new("list_redmine_users");
            request.arguments = json!({"limit": 0}).as_object().cloned();
            request
        })
        .await
        .expect("call_tool should succeed");
    let body = body_of(&result);
    assert_eq!(body["pagination"]["limit"], 1);
}

#[tokio::test]
async fn list_redmine_users_clamps_an_over_large_limit_to_100() {
    let h = support::harness(&[]).await;
    Mock::given(method("GET"))
        .and(path("/users.json"))
        .and(query_param("limit", "100"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "users": [], "total_count": 0, "offset": 0, "limit": 100
        })))
        .mount(&h.redmine)
        .await;

    let result = h
        .client
        .call_tool({
            let mut request = CallToolRequestParams::new("list_redmine_users");
            request.arguments = json!({"limit": 5000}).as_object().cloned();
            request
        })
        .await
        .expect("call_tool should succeed");
    let body = body_of(&result);
    assert_eq!(body["pagination"]["limit"], 100);
}

#[tokio::test]
async fn list_redmine_users_forbidden_names_a_concrete_alternative() {
    let h = support::harness(&[]).await;
    Mock::given(method("GET"))
        .and(path("/users.json"))
        .respond_with(ResponseTemplate::new(403))
        .mount(&h.redmine)
        .await;

    let result = call(&h, "list_redmine_users").await;
    assert_eq!(result.is_error, Some(true));
    let structured = result.structured_content.unwrap();
    assert_eq!(structured["code"], "FORBIDDEN");
    assert_eq!(structured["retryable"], false);
    let hint = structured["hint"].as_str().unwrap();
    assert!(
        hint.contains("get_current_user"),
        "hint should name a concrete alternative tool: {hint}"
    );
}

// --- list_redmine_queries ---

#[tokio::test]
async fn list_redmine_queries_happy_path() {
    let h = support::harness(&[]).await;
    Mock::given(method("GET"))
        .and(path("/queries.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "queries": [{"id": 1, "name": "My open issues", "is_public": false, "project_id": 1}],
            "total_count": 1, "offset": 0, "limit": 100
        })))
        .mount(&h.redmine)
        .await;

    let body = body_of(&call(&h, "list_redmine_queries").await);
    let queries = body["queries"].as_array().unwrap();
    assert_eq!(queries.len(), 1);
    assert!(
        queries[0]["name"]
            .as_str()
            .unwrap()
            .starts_with("<<<untrusted:")
    );
}

#[tokio::test]
async fn list_redmine_queries_dominant_error_is_in_band() {
    let h = support::harness(&[]).await;
    Mock::given(method("GET"))
        .and(path("/queries.json"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&h.redmine)
        .await;

    let result = call(&h, "list_redmine_queries").await;
    assert_eq!(result.is_error, Some(true));
    assert_eq!(result.structured_content.unwrap()["code"], "UNAUTHORIZED");
}
