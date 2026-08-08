//! e2e: the time-tracking tool family — happy path and dominant error
//! path per tool, plus behaviours specific to this family: `list_time_entries`'
//! `from_date`/`to_date` → `spent_on` translation, `manage_time_entry`'s
//! `comments: ""` clearing an existing value, `list_time_entry_activities`'
//! two different wire shapes, and `import_time_entries`'
//! continue-vs-stop-on-error semantics.
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

async fn call(h: &support::Harness, name: &str, args: Value) -> rmcp::model::CallToolResult {
    let mut request = CallToolRequestParams::new(name.to_string());
    request.arguments = args.as_object().cloned();
    h.client
        .call_tool(request)
        .await
        .expect("call_tool should succeed")
}

fn time_entry_json(id: u64, hours: f64, comments: Option<&str>) -> Value {
    json!({
        "time_entry": {
            "id": id, "project": {"id": 5, "name": "P"},
            "user": {"id": 2, "name": "Alice"}, "activity": {"id": 9, "name": "Development"},
            "hours": hours, "comments": comments, "spent_on": "2026-01-15",
            "created_on": "2026-01-15T00:00:00Z", "updated_on": "2026-01-15T00:00:00Z"
        }
    })
}

// --- list_time_entries ---

#[tokio::test]
async fn list_time_entries_happy_path() {
    let h = support::harness(&[]).await;
    Mock::given(method("GET"))
        .and(path("/time_entries.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "time_entries": [
                {
                    "id": 1, "project": {"id": 5, "name": "P"},
                    "issue": {"id": 42},
                    "user": {"id": 2, "name": "Alice"}, "activity": {"id": 9, "name": "Development"},
                    "hours": 2.5, "comments": "did stuff", "spent_on": "2026-01-15",
                    "created_on": "2026-01-15T00:00:00Z", "updated_on": "2026-01-15T00:00:00Z"
                }
            ],
            "total_count": 1, "offset": 0, "limit": 25
        })))
        .mount(&h.redmine)
        .await;

    let body = body_of(&call(&h, "list_time_entries", json!({})).await);
    let entries = body["time_entries"].as_array().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["id"], 1);
    assert_eq!(entries[0]["issue_id"], 42);
    assert!(
        entries[0]["comments"]
            .as_str()
            .unwrap()
            .starts_with("<<<untrusted:")
    );
    assert_eq!(body["pagination"]["total"], 1);
}

#[tokio::test]
async fn list_time_entries_translates_from_and_to_date_into_spent_on() {
    let h = support::harness(&[]).await;
    Mock::given(method("GET"))
        .and(path("/time_entries.json"))
        .and(query_param("spent_on", "><2026-01-01|2026-01-31"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "time_entries": [], "total_count": 0, "offset": 0, "limit": 25
        })))
        .expect(1)
        .mount(&h.redmine)
        .await;

    call(
        &h,
        "list_time_entries",
        json!({"from_date": "2026-01-01", "to_date": "2026-01-31"}),
    )
    .await;
}

#[tokio::test]
async fn list_time_entries_from_date_only_sends_a_greater_than_or_equal_operator() {
    let h = support::harness(&[]).await;
    Mock::given(method("GET"))
        .and(path("/time_entries.json"))
        .and(query_param("spent_on", ">=2026-01-01"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "time_entries": [], "total_count": 0, "offset": 0, "limit": 25
        })))
        .expect(1)
        .mount(&h.redmine)
        .await;

    call(&h, "list_time_entries", json!({"from_date": "2026-01-01"})).await;
}

#[tokio::test]
async fn list_time_entries_user_id_me_sends_the_literal_me() {
    let h = support::harness(&[]).await;
    Mock::given(method("GET"))
        .and(path("/time_entries.json"))
        .and(query_param("user_id", "me"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "time_entries": [], "total_count": 0, "offset": 0, "limit": 25
        })))
        .expect(1)
        .mount(&h.redmine)
        .await;

    call(&h, "list_time_entries", json!({"user_id": "me"})).await;
}

#[tokio::test]
async fn list_time_entries_clamps_limit_to_1_100() {
    let h = support::harness(&[]).await;
    Mock::given(method("GET"))
        .and(path("/time_entries.json"))
        .and(query_param("limit", "100"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "time_entries": [], "total_count": 0, "offset": 0, "limit": 100
        })))
        .expect(1)
        .mount(&h.redmine)
        .await;

    call(&h, "list_time_entries", json!({"limit": 5000})).await;
}

#[tokio::test]
async fn list_time_entries_dominant_error_is_in_band() {
    let h = support::harness(&[]).await;
    Mock::given(method("GET"))
        .and(path("/time_entries.json"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&h.redmine)
        .await;

    let result = call(&h, "list_time_entries", json!({"issue_id": 1})).await;
    assert_eq!(result.is_error, Some(true));
    assert_eq!(result.structured_content.unwrap()["code"], "NOT_FOUND");
}

// --- manage_time_entry ---

#[tokio::test]
async fn manage_time_entry_create_happy_path() {
    let h = support::harness(&[]).await;
    Mock::given(method("POST"))
        .and(path("/time_entries.json"))
        .respond_with(ResponseTemplate::new(201).set_body_json(time_entry_json(
            7,
            2.5,
            Some("worked"),
        )))
        .mount(&h.redmine)
        .await;

    let body = body_of(
        &call(
            &h,
            "manage_time_entry",
            json!({"action": "create", "issue_id": 42, "hours": 2.5}),
        )
        .await,
    );
    assert_eq!(body["success"], true);
    assert_eq!(body["time_entry"]["id"], 7);
}

#[tokio::test]
async fn manage_time_entry_create_rejects_missing_required_fields() {
    let h = support::harness(&[]).await;
    let cases = [
        json!({"action": "create"}),               // missing hours, project/issue
        json!({"action": "create", "hours": 2.0}), // missing project_id/issue_id
        json!({"action": "create", "issue_id": 1}), // missing hours
        json!({"action": "create", "issue_id": 1, "hours": 0.0}), // non-positive hours
        json!({"action": "update"}),               // missing time_entry_id
    ];
    for args in cases {
        let mut request = CallToolRequestParams::new("manage_time_entry".to_string());
        request.arguments = args.as_object().cloned();
        let result = h.client.call_tool(request).await;
        assert!(
            result.is_err(),
            "{args:?} should be rejected as an argument error"
        );
    }
}

#[tokio::test]
async fn manage_time_entry_update_can_clear_comments_with_an_empty_string() {
    let h = support::harness(&[]).await;
    Mock::given(method("PUT"))
        .and(path("/time_entries/7.json"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&h.redmine)
        .await;
    Mock::given(method("GET"))
        .and(path("/time_entries/7.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(time_entry_json(7, 2.5, None)))
        .mount(&h.redmine)
        .await;

    let body = body_of(
        &call(
            &h,
            "manage_time_entry",
            json!({"action": "update", "time_entry_id": 7, "comments": ""}),
        )
        .await,
    );
    assert_eq!(body["time_entry"]["id"], 7);

    let received = h.redmine.received_requests().await.unwrap();
    let put_request = received
        .iter()
        .find(|r| r.method.as_str() == "PUT")
        .expect("a PUT request should have been made");
    let put_body: Value = serde_json::from_slice(&put_request.body).unwrap();
    assert_eq!(put_body["time_entry"]["comments"], "");
}

// --- list_time_entry_activities ---

#[tokio::test]
async fn list_time_entry_activities_global_includes_active_and_is_default() {
    let h = support::harness(&[]).await;
    Mock::given(method("GET"))
        .and(path("/enumerations/time_entry_activities.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "time_entry_activities": [
                {"id": 8, "name": "Design", "active": true, "is_default": false},
                {"id": 9, "name": "Development", "active": true, "is_default": true}
            ]
        })))
        .mount(&h.redmine)
        .await;

    let body = body_of(&call(&h, "list_time_entry_activities", json!({})).await);
    let activities = body["time_entry_activities"].as_array().unwrap();
    assert_eq!(activities.len(), 2);
    assert_eq!(activities[1]["is_default"], true);
}

#[tokio::test]
async fn list_time_entry_activities_project_scoped_has_null_active_and_is_default() {
    let h = support::harness(&[]).await;
    Mock::given(method("GET"))
        .and(path("/projects/5.json"))
        .and(query_param("include", "time_entry_activities"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "project": {
                "id": 5, "name": "Demo", "identifier": "demo",
                "created_on": "2026-01-01T00:00:00Z", "updated_on": "2026-01-01T00:00:00Z",
                "time_entry_activities": [{"id": 9, "name": "Development"}]
            }
        })))
        .mount(&h.redmine)
        .await;

    let body = body_of(&call(&h, "list_time_entry_activities", json!({"project_id": 5})).await);
    let activities = body["time_entry_activities"].as_array().unwrap();
    assert_eq!(activities.len(), 1);
    assert!(activities[0]["active"].is_null());
    assert!(activities[0]["is_default"].is_null());
}

// --- import_time_entries ---

#[tokio::test]
async fn import_time_entries_continues_past_a_failing_middle_entry_by_default() {
    let h = support::harness(&[]).await;
    // Match on the request body's issue_id to distinguish the three POSTs.
    Mock::given(method("POST"))
        .and(path("/time_entries.json"))
        .and(wiremock::matchers::body_partial_json(
            json!({"time_entry": {"issue_id": 1}}),
        ))
        .respond_with(ResponseTemplate::new(201).set_body_json(time_entry_json(101, 1.0, None)))
        .mount(&h.redmine)
        .await;
    Mock::given(method("POST"))
        .and(path("/time_entries.json"))
        .and(wiremock::matchers::body_partial_json(
            json!({"time_entry": {"issue_id": 2}}),
        ))
        .respond_with(ResponseTemplate::new(422).set_body_json(json!({
            "errors": ["Hours can't be blank"]
        })))
        .mount(&h.redmine)
        .await;
    Mock::given(method("POST"))
        .and(path("/time_entries.json"))
        .and(wiremock::matchers::body_partial_json(
            json!({"time_entry": {"issue_id": 3}}),
        ))
        .respond_with(ResponseTemplate::new(201).set_body_json(time_entry_json(103, 3.0, None)))
        .mount(&h.redmine)
        .await;

    let body = body_of(
        &call(
            &h,
            "import_time_entries",
            json!({
                "entries": [
                    {"issue_id": 1, "hours": 1.0},
                    {"issue_id": 2, "hours": 2.0},
                    {"issue_id": 3, "hours": 3.0}
                ],
                "stop_on_error": false
            }),
        )
        .await,
    );
    assert_eq!(body["total"], 3);
    assert_eq!(body["attempted"], 3);
    assert_eq!(body["succeeded"], 2);
    assert_eq!(body["failed"], 1);
    let results = body["results"].as_array().unwrap();
    assert_eq!(results[0]["success"], true);
    assert_eq!(results[1]["success"], false);
    assert!(
        results[1]["error"]
            .as_str()
            .unwrap()
            .contains("VALIDATION_FAILED")
    );
    assert_eq!(results[2]["success"], true);
    assert_eq!(results[2]["attempted"], true);
}

#[tokio::test]
async fn import_time_entries_stop_on_error_halts_and_marks_later_entries_not_attempted() {
    let h = support::harness(&[]).await;
    Mock::given(method("POST"))
        .and(path("/time_entries.json"))
        .and(wiremock::matchers::body_partial_json(
            json!({"time_entry": {"issue_id": 1}}),
        ))
        .respond_with(ResponseTemplate::new(201).set_body_json(time_entry_json(101, 1.0, None)))
        .mount(&h.redmine)
        .await;
    Mock::given(method("POST"))
        .and(path("/time_entries.json"))
        .and(wiremock::matchers::body_partial_json(
            json!({"time_entry": {"issue_id": 2}}),
        ))
        .respond_with(ResponseTemplate::new(422).set_body_json(json!({
            "errors": ["Hours can't be blank"]
        })))
        .mount(&h.redmine)
        .await;

    let body = body_of(
        &call(
            &h,
            "import_time_entries",
            json!({
                "entries": [
                    {"issue_id": 1, "hours": 1.0},
                    {"issue_id": 2, "hours": 2.0},
                    {"issue_id": 3, "hours": 3.0}
                ],
                "stop_on_error": true
            }),
        )
        .await,
    );
    assert_eq!(body["attempted"], 2);
    assert_eq!(body["succeeded"], 1);
    assert_eq!(body["failed"], 1);
    let results = body["results"].as_array().unwrap();
    assert_eq!(results[2]["attempted"], false);
    assert!(results[2]["error"].is_null());

    // Entry 3 (issue_id: 3) must never have been sent.
    let received = h.redmine.received_requests().await.unwrap();
    let post_count = received
        .iter()
        .filter(|r| r.method.as_str() == "POST")
        .count();
    assert_eq!(post_count, 2, "entry 3 should never have been attempted");
}

#[tokio::test]
async fn import_time_entries_rejects_more_than_500_entries_before_any_request() {
    let h = support::harness(&[]).await;
    // No mock mounted: any request would fail this test.
    let entries: Vec<Value> = (0..501)
        .map(|i| json!({"issue_id": i, "hours": 1.0}))
        .collect();
    let mut request = CallToolRequestParams::new("import_time_entries".to_string());
    request.arguments = json!({"entries": entries}).as_object().cloned();
    let result = h.client.call_tool(request).await;
    assert!(result.is_err());
}

#[tokio::test]
async fn import_time_entries_rejects_an_entry_with_neither_project_id_nor_issue_id() {
    let h = support::harness(&[]).await;
    let mut request = CallToolRequestParams::new("import_time_entries".to_string());
    request.arguments = json!({"entries": [{"hours": 1.0}]}).as_object().cloned();
    let result = h.client.call_tool(request).await;
    assert!(result.is_err());
}

#[tokio::test]
async fn manage_time_entry_is_absent_and_rejected_in_read_only_mode() {
    let h = support::harness(&[("REDMINE_MCP_READ_ONLY", "true")]).await;
    let tools = h.client.list_tools(None).await.unwrap();
    assert!(
        !tools
            .tools
            .iter()
            .any(|t| t.name.as_ref() == "manage_time_entry")
    );
    assert!(
        !tools
            .tools
            .iter()
            .any(|t| t.name.as_ref() == "import_time_entries")
    );

    let result = h
        .client
        .call_tool(CallToolRequestParams::new("manage_time_entry".to_string()))
        .await;
    assert!(result.is_err());
}
