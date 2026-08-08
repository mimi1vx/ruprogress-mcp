//! e2e: the project-management tool family — happy path and dominant
//! error path per tool, plus behaviours specific to this family:
//! `summarize_project_status`'s fixed 6-request fan-out,
//! `manage_redmine_version`/`manage_project_member`'s read-only removal,
//! and `manage_project_member`'s `user_id`/`group_id` validation.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

mod support;

use rmcp::model::CallToolRequestParams;
use serde_json::{Value, json};
use wiremock::matchers::{method, path, query_param, query_param_contains};
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

// --- list_project_issue_custom_fields ---

#[tokio::test]
async fn list_project_issue_custom_fields_filters_by_project_and_tracker() {
    let h = support::harness(&[]).await;
    Mock::given(method("GET"))
        .and(path("/projects/5.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "project": {
                "id": 5, "name": "P", "identifier": "p",
                "created_on": "2026-01-01T00:00:00Z", "updated_on": "2026-01-01T00:00:00Z"
            }
        })))
        .mount(&h.redmine)
        .await;
    Mock::given(method("GET"))
        .and(path("/custom_fields.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "custom_fields": [
                {
                    "id": 6, "name": "Size", "field_format": "list",
                    "possible_values": [{"value": "S", "label": "S"}],
                    "customized_type": "issue", "is_for_all": false,
                    "projects": [{"id": 5, "name": "P"}],
                    "trackers": [{"id": 1, "name": "Bug"}]
                },
                {
                    "id": 7, "name": "Other project field", "field_format": "string",
                    "customized_type": "issue", "is_for_all": false,
                    "projects": [{"id": 99, "name": "Other"}]
                },
                {
                    "id": 8, "name": "Wrong tracker", "field_format": "string",
                    "customized_type": "issue", "is_for_all": true,
                    "trackers": [{"id": 2, "name": "Feature"}]
                },
                {
                    "id": 9, "name": "Not an issue field", "field_format": "string",
                    "customized_type": "project", "is_for_all": true
                }
            ]
        })))
        .mount(&h.redmine)
        .await;

    let body = body_of(
        &call(
            &h,
            "list_project_issue_custom_fields",
            json!({"project_id": 5, "tracker_id": 1}),
        )
        .await,
    );
    let fields = body["custom_fields"].as_array().unwrap();
    assert_eq!(
        fields.len(),
        1,
        "expected only field 6 to match project 5 + tracker 1: {fields:?}"
    );
    assert_eq!(fields[0]["id"], 6);
    assert_eq!(fields[0]["possible_values"], json!(["S"]));
}

#[tokio::test]
async fn list_project_issue_custom_fields_forbidden_names_admin_requirement() {
    let h = support::harness(&[]).await;
    Mock::given(method("GET"))
        .and(path("/projects/5.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "project": {
                "id": 5, "name": "P", "identifier": "p",
                "created_on": "2026-01-01T00:00:00Z", "updated_on": "2026-01-01T00:00:00Z"
            }
        })))
        .mount(&h.redmine)
        .await;
    Mock::given(method("GET"))
        .and(path("/custom_fields.json"))
        .respond_with(ResponseTemplate::new(403))
        .mount(&h.redmine)
        .await;

    let result = call(
        &h,
        "list_project_issue_custom_fields",
        json!({"project_id": 5}),
    )
    .await;
    assert_eq!(result.is_error, Some(true));
    let structured = result.structured_content.unwrap();
    assert_eq!(structured["code"], "FORBIDDEN");
    assert_eq!(structured["retryable"], false);
    assert!(structured["hint"].as_str().unwrap().contains("admin"));
}

// --- summarize_project_status ---

fn summary_issue(id: u64, status: &str, priority: &str, assignee: Option<(u64, &str)>) -> Value {
    json!({
        "id": id, "project": {"id": 1, "name": "Demo"}, "tracker": {"id": 1, "name": "Bug"},
        "status": {"id": 1, "name": status}, "priority": {"id": 1, "name": priority},
        "author": {"id": 1, "name": "Alice"},
        "assigned_to": assignee.map(|(id, name)| json!({"id": id, "name": name})),
        "subject": "S",
        "created_on": "2026-01-01T00:00:00Z", "updated_on": "2026-01-01T00:00:00Z"
    })
}

#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "six near-identical wiremock mock registrations, one per fan-out request"
)]
async fn summarize_project_status_issues_exactly_six_requests() {
    let h = support::harness(&[]).await;

    Mock::given(method("GET"))
        .and(path("/projects/1.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "project": {
                "id": 1, "name": "Demo", "identifier": "demo",
                "created_on": "2026-01-01T00:00:00Z", "updated_on": "2026-01-01T00:00:00Z"
            }
        })))
        .expect(1)
        .mount(&h.redmine)
        .await;

    // Sample: status_id=*, sort=updated_on:desc, limit=100.
    Mock::given(method("GET"))
        .and(path("/issues.json"))
        .and(query_param("status_id", "*"))
        .and(query_param("sort", "updated_on:desc"))
        .and(query_param("limit", "100"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "issues": [
                summary_issue(1, "New", "Normal", None),
                summary_issue(2, "New", "High", Some((9, "Bob"))),
            ],
            "total_count": 2, "offset": 0, "limit": 100
        })))
        .expect(1)
        .mount(&h.redmine)
        .await;

    // Open count: status_id=open, limit=1.
    Mock::given(method("GET"))
        .and(path("/issues.json"))
        .and(query_param("status_id", "open"))
        .and(query_param("limit", "1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "issues": [], "total_count": 5, "offset": 0, "limit": 1
        })))
        .expect(1)
        .mount(&h.redmine)
        .await;

    // Closed count: status_id=closed, limit=1.
    Mock::given(method("GET"))
        .and(path("/issues.json"))
        .and(query_param("status_id", "closed"))
        .and(query_param("limit", "1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "issues": [], "total_count": 3, "offset": 0, "limit": 1
        })))
        .expect(1)
        .mount(&h.redmine)
        .await;

    // Created-in-period count: status_id=*, created_on>=..., limit=1.
    Mock::given(method("GET"))
        .and(path("/issues.json"))
        .and(query_param("status_id", "*"))
        .and(query_param_contains("created_on", ">="))
        .and(query_param("limit", "1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "issues": [], "total_count": 4, "offset": 0, "limit": 1
        })))
        .expect(1)
        .mount(&h.redmine)
        .await;

    // Updated-in-period count: status_id=*, updated_on>=..., limit=1.
    Mock::given(method("GET"))
        .and(path("/issues.json"))
        .and(query_param("status_id", "*"))
        .and(query_param_contains("updated_on", ">="))
        .and(query_param("limit", "1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "issues": [], "total_count": 10, "offset": 0, "limit": 1
        })))
        .expect(1)
        .mount(&h.redmine)
        .await;

    let body = body_of(&call(&h, "summarize_project_status", json!({"project_id": 1})).await);
    assert_eq!(body["project_id"], 1);
    assert_eq!(body["analysis_period_days"], 30);
    assert_eq!(body["totals"]["total"], 2);
    assert_eq!(body["totals"]["open"], 5);
    assert_eq!(body["totals"]["closed"], 3);
    assert_eq!(body["recent_activity"]["created_count"], 4);
    assert_eq!(body["recent_activity"]["updated_count"], 10);
    assert_eq!(body["sample_size"], 2);
    assert_eq!(body["sample_truncated"], false);
    let status_breakdown = body["status_breakdown"].as_array().unwrap();
    assert_eq!(status_breakdown.len(), 1);
    assert_eq!(status_breakdown[0]["name"], "New");
    assert_eq!(status_breakdown[0]["count"], 2);
    let assignee_breakdown = body["assignee_breakdown"].as_array().unwrap();
    assert!(
        assignee_breakdown
            .iter()
            .any(|e| e["name"] == "Unassigned" && e["count"] == 1)
    );
    assert!(
        assignee_breakdown
            .iter()
            .any(|e| e["name"].as_str().unwrap().starts_with("<<<untrusted:") && e["count"] == 1)
    );

    // Explicit request-count assertion beyond the six `.expect(1)` mocks
    // above (which already fail the test if under- or over-called): the
    // received-requests log must contain exactly 6 entries.
    let received = h.redmine.received_requests().await.unwrap();
    assert_eq!(
        received.len(),
        6,
        "expected exactly 6 upstream requests, got {}: {:?}",
        received.len(),
        received
            .iter()
            .map(|r| r.url.to_string())
            .collect::<Vec<_>>()
    );
}

// --- list_redmine_versions ---

#[tokio::test]
async fn list_redmine_versions_filters_by_status_client_side() {
    let h = support::harness(&[]).await;
    Mock::given(method("GET"))
        .and(path("/projects/5/versions.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "versions": [
                {
                    "id": 1, "project": {"id": 5, "name": "P"}, "name": "1.0",
                    "status": "closed",
                    "created_on": "2026-01-01T00:00:00Z", "updated_on": "2026-01-01T00:00:00Z"
                },
                {
                    "id": 2, "project": {"id": 5, "name": "P"}, "name": "2.0",
                    "status": "open",
                    "created_on": "2026-01-01T00:00:00Z", "updated_on": "2026-01-01T00:00:00Z"
                }
            ],
            "total_count": 2
        })))
        .mount(&h.redmine)
        .await;

    let body = body_of(
        &call(
            &h,
            "list_redmine_versions",
            json!({"project_id": 5, "status_filter": "open"}),
        )
        .await,
    );
    let versions = body["versions"].as_array().unwrap();
    assert_eq!(versions.len(), 1);
    assert_eq!(versions[0]["id"], 2);
}

#[tokio::test]
async fn list_redmine_versions_dominant_error_is_in_band() {
    let h = support::harness(&[]).await;
    Mock::given(method("GET"))
        .and(path("/projects/5/versions.json"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&h.redmine)
        .await;

    let result = call(&h, "list_redmine_versions", json!({"project_id": 5})).await;
    assert_eq!(result.is_error, Some(true));
    assert_eq!(result.structured_content.unwrap()["code"], "NOT_FOUND");
}

// --- manage_redmine_version ---

fn version_json(id: u64, name: &str, status: &str) -> Value {
    json!({
        "version": {
            "id": id, "project": {"id": 5, "name": "P"}, "name": name, "status": status,
            "created_on": "2026-01-01T00:00:00Z", "updated_on": "2026-01-01T00:00:00Z"
        }
    })
}

#[tokio::test]
async fn manage_redmine_version_create_happy_path() {
    let h = support::harness(&[]).await;
    Mock::given(method("POST"))
        .and(path("/projects/5/versions.json"))
        .respond_with(ResponseTemplate::new(201).set_body_json(version_json(42, "v2.0", "open")))
        .mount(&h.redmine)
        .await;

    let body = body_of(
        &call(
            &h,
            "manage_redmine_version",
            json!({"action": "create", "project_id": 5, "name": "v2.0"}),
        )
        .await,
    );
    assert_eq!(body["success"], true);
    assert_eq!(body["version"]["id"], 42);
    assert!(body["deleted_version_id"].is_null());
}

#[tokio::test]
async fn manage_redmine_version_update_happy_path() {
    let h = support::harness(&[]).await;
    Mock::given(method("PUT"))
        .and(path("/versions/42.json"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&h.redmine)
        .await;
    Mock::given(method("GET"))
        .and(path("/versions/42.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(version_json(42, "v2.0", "locked")))
        .mount(&h.redmine)
        .await;

    let body = body_of(
        &call(
            &h,
            "manage_redmine_version",
            json!({"action": "update", "version_id": 42, "status": "locked"}),
        )
        .await,
    );
    assert_eq!(body["version"]["status"], "locked");
}

#[tokio::test]
async fn manage_redmine_version_delete_happy_path() {
    let h = support::harness(&[]).await;
    Mock::given(method("DELETE"))
        .and(path("/versions/42.json"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&h.redmine)
        .await;

    let body = body_of(
        &call(
            &h,
            "manage_redmine_version",
            json!({"action": "delete", "version_id": 42}),
        )
        .await,
    );
    assert_eq!(body["success"], true);
    assert_eq!(body["deleted_version_id"], 42);
    assert!(body["version"].is_null());
}

#[tokio::test]
async fn manage_redmine_version_rejects_missing_required_fields_per_action() {
    let h = support::harness(&[]).await;
    let cases = [
        json!({"action": "create"}),                  // missing project_id/name
        json!({"action": "create", "project_id": 5}), // missing name
        json!({"action": "update"}),                  // missing version_id
        json!({"action": "delete"}),                  // missing version_id
    ];
    for args in cases {
        let mut request = CallToolRequestParams::new("manage_redmine_version".to_string());
        request.arguments = args.as_object().cloned();
        let result = h.client.call_tool(request).await;
        assert!(
            result.is_err(),
            "{args:?} should be rejected as an argument error"
        );
    }
}

#[tokio::test]
async fn manage_redmine_version_is_absent_and_rejected_in_read_only_mode() {
    let h = support::harness(&[("REDMINE_MCP_READ_ONLY", "true")]).await;
    let tools = h.client.list_tools(None).await.unwrap();
    assert!(
        !tools
            .tools
            .iter()
            .any(|t| t.name.as_ref() == "manage_redmine_version")
    );

    let result = h
        .client
        .call_tool(CallToolRequestParams::new(
            "manage_redmine_version".to_string(),
        ))
        .await;
    assert!(result.is_err());
}

// --- list_project_members ---

#[tokio::test]
async fn list_project_members_happy_path() {
    let h = support::harness(&[]).await;
    Mock::given(method("GET"))
        .and(path("/projects/5/memberships.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "memberships": [
                {
                    "id": 1, "project": {"id": 5, "name": "P"},
                    "user": {"id": 2, "name": "Alice"},
                    "roles": [{"id": 3, "name": "Manager"}]
                },
                {
                    "id": 2, "project": {"id": 5, "name": "P"},
                    "group": {"id": 20, "name": "Dev Team"},
                    "roles": [{"id": 4, "name": "Developer"}]
                }
            ],
            "total_count": 2, "offset": 0, "limit": 100
        })))
        .mount(&h.redmine)
        .await;

    let body = body_of(&call(&h, "list_project_members", json!({"project_id": 5})).await);
    let memberships = body["memberships"].as_array().unwrap();
    assert_eq!(memberships.len(), 2);
    assert!(
        memberships[0]["user"]["name"]
            .as_str()
            .unwrap()
            .starts_with("<<<untrusted:")
    );
    assert_eq!(memberships[0]["roles"][0]["name"], "Manager");
    assert!(memberships[1]["user"].is_null());
    assert!(
        memberships[1]["group"]["name"]
            .as_str()
            .unwrap()
            .starts_with("<<<untrusted:")
    );
}

// --- list_redmine_roles ---

#[tokio::test]
async fn list_redmine_roles_does_not_require_admin() {
    let h = support::harness(&[]).await;
    Mock::given(method("GET"))
        .and(path("/roles.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "roles": [{"id": 3, "name": "Manager"}, {"id": 4, "name": "Developer"}]
        })))
        .mount(&h.redmine)
        .await;

    let body = body_of(&call(&h, "list_redmine_roles", json!({})).await);
    let roles = body["roles"].as_array().unwrap();
    assert_eq!(roles.len(), 2);
}

// --- get_project_modules ---

#[tokio::test]
async fn get_project_modules_happy_path() {
    let h = support::harness(&[]).await;
    Mock::given(method("GET"))
        .and(path("/projects/5.json"))
        .and(query_param("include", "enabled_modules"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "project": {
                "id": 5, "name": "Demo", "identifier": "demo",
                "created_on": "2026-01-01T00:00:00Z", "updated_on": "2026-01-01T00:00:00Z",
                "enabled_modules": [{"id": 1, "name": "issue_tracking"}, {"id": 2, "name": "wiki"}]
            }
        })))
        .mount(&h.redmine)
        .await;

    let body = body_of(&call(&h, "get_project_modules", json!({"project_id": 5})).await);
    assert_eq!(body["project_id"], 5);
    assert_eq!(body["enabled_modules"], json!(["issue_tracking", "wiki"]));
    assert!(
        body["project_name"]
            .as_str()
            .unwrap()
            .starts_with("<<<untrusted:")
    );
}

// --- manage_project_member ---

fn membership_json(id: u64, principal_key: &str, principal: &Value) -> Value {
    json!({
        "membership": {
            "id": id, "project": {"id": 5, "name": "P"},
            principal_key: principal,
            "roles": [{"id": 3, "name": "Manager"}]
        }
    })
}

#[tokio::test]
async fn manage_project_member_add_routes_group_id_through_user_id_field() {
    let h = support::harness(&[]).await;
    Mock::given(method("POST"))
        .and(path("/projects/5/memberships.json"))
        .respond_with(ResponseTemplate::new(201).set_body_json(membership_json(
            7,
            "group",
            &json!({"id": 20, "name": "Dev Team"}),
        )))
        .mount(&h.redmine)
        .await;

    let body = body_of(
        &call(
            &h,
            "manage_project_member",
            json!({"action": "add", "project_id": 5, "group_id": 20, "role_ids": [3]}),
        )
        .await,
    );
    assert_eq!(body["success"], true);
    assert_eq!(body["membership"]["id"], 7);
}

#[tokio::test]
async fn manage_project_member_update_happy_path() {
    let h = support::harness(&[]).await;
    Mock::given(method("PUT"))
        .and(path("/memberships/7.json"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&h.redmine)
        .await;
    Mock::given(method("GET"))
        .and(path("/memberships/7.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(membership_json(
            7,
            "user",
            &json!({"id": 2, "name": "Alice"}),
        )))
        .mount(&h.redmine)
        .await;

    let body = body_of(
        &call(
            &h,
            "manage_project_member",
            json!({"action": "update", "membership_id": 7, "role_ids": [3]}),
        )
        .await,
    );
    assert_eq!(body["membership"]["id"], 7);
}

#[tokio::test]
async fn manage_project_member_remove_happy_path() {
    let h = support::harness(&[]).await;
    Mock::given(method("DELETE"))
        .and(path("/memberships/7.json"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&h.redmine)
        .await;

    let body = body_of(
        &call(
            &h,
            "manage_project_member",
            json!({"action": "remove", "membership_id": 7}),
        )
        .await,
    );
    assert_eq!(body["success"], true);
    assert_eq!(body["deleted_membership_id"], 7);
}

#[tokio::test]
async fn manage_project_member_add_validates_exactly_one_of_user_id_or_group_id() {
    let h = support::harness(&[]).await;
    let cases = [
        json!({"action": "add", "project_id": 5, "role_ids": [3]}), // neither
        json!({"action": "add", "project_id": 5, "user_id": 1, "group_id": 2, "role_ids": [3]}), // both
        json!({"action": "add", "project_id": 5, "user_id": 1, "role_ids": []}), // empty role_ids
    ];
    for args in cases {
        let mut request = CallToolRequestParams::new("manage_project_member".to_string());
        request.arguments = args.as_object().cloned();
        let result = h.client.call_tool(request).await;
        assert!(
            result.is_err(),
            "{args:?} should be rejected as an argument error"
        );
    }
}

#[tokio::test]
async fn manage_project_member_is_absent_and_rejected_in_read_only_mode() {
    let h = support::harness(&[("REDMINE_MCP_READ_ONLY", "true")]).await;
    let tools = h.client.list_tools(None).await.unwrap();
    assert!(
        !tools
            .tools
            .iter()
            .any(|t| t.name.as_ref() == "manage_project_member")
    );
}
