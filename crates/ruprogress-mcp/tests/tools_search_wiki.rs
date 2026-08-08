//! e2e: the search & wiki tool family — `search_entire_redmine`,
//! `manage_redmine_wiki_page`. Happy path and dominant error path per tool,
//! plus behaviours specific to this family: resource-flag selection and
//! bucket re-labelling, the `rename` PUT-then-GET confirmation dance
//! and its silent-permission-drop failure mode, and the
//! `redirect_existing_links` string-not-bool wire format. Per-action
//! read-only gating is covered in `tests/readonly.rs` instead.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

mod support;

use rmcp::model::CallToolRequestParams;
use serde_json::{Value, json};
use wiremock::matchers::{body_json, method, path, query_param};
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

// --- search_entire_redmine ---

#[tokio::test]
async fn search_entire_redmine_defaults_to_both_resource_flags() {
    let h = support::harness(&[]).await;
    Mock::given(method("GET"))
        .and(path("/search.json"))
        .and(query_param("issues", "1"))
        .and(query_param("wiki_pages", "1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "results": [], "total_count": 0, "offset": 0, "limit": 100
        })))
        .expect(1)
        .mount(&h.redmine)
        .await;

    let result = call(&h, "search_entire_redmine", json!({"query": "install"})).await;
    assert_ne!(result.is_error, Some(true));
}

#[tokio::test]
async fn search_entire_redmine_resources_wiki_pages_sends_only_that_flag() {
    let h = support::harness(&[]).await;
    Mock::given(method("GET"))
        .and(path("/search.json"))
        .and(query_param("wiki_pages", "1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "results": [], "total_count": 0, "offset": 0, "limit": 100
        })))
        .expect(1)
        .mount(&h.redmine)
        .await;

    let result = call(
        &h,
        "search_entire_redmine",
        json!({"query": "setup", "resources": ["wiki_pages"]}),
    )
    .await;
    assert_ne!(result.is_error, Some(true));
}

#[tokio::test]
async fn search_entire_redmine_rebuckets_raw_types_and_tallies_them() {
    let h = support::harness(&[]).await;
    // "z", not "x": the boundary wrapper's own `kind` label
    // ("search_result.excerpt") contains an "x", which would pollute a
    // naive character count below.
    let long_description = "z".repeat(250);
    Mock::given(method("GET"))
        .and(path("/search.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "results": [
                {"id": 1, "title": "Bug in login page", "type": "issue",
                 "url": "https://x/issues/1", "description": long_description,
                 "datetime": "2026-01-01T00:00:00Z"},
                {"id": 2, "title": "Installation Guide", "type": "wiki-page",
                 "url": "https://x/wiki/Installation", "description": "short",
                 "datetime": "2026-01-01T00:00:00Z"}
            ],
            "total_count": 2, "offset": 0, "limit": 100
        })))
        .mount(&h.redmine)
        .await;

    let result = call(&h, "search_entire_redmine", json!({"query": "install"})).await;
    let body = body_of(&result);
    let results = body["results"].as_array().expect("results array");
    assert_eq!(results.len(), 2);
    assert_eq!(results[0]["type"], "issues");
    assert_eq!(results[1]["type"], "wiki_pages");
    // Excerpt truncated to 200 characters, never the full 250 — checked by
    // counting the marker character rather than parsing the boundary
    // wrapper's own delimiters back out.
    let excerpt = results[0]["excerpt"].as_str().expect("excerpt string");
    let z_count = excerpt.chars().filter(|c| *c == 'z').count();
    assert!(
        z_count > 0 && z_count <= 200,
        "excerpt should contain 1-200 'z' characters (truncated from 250), got {z_count}"
    );
    assert_eq!(body["results_by_type"]["issues"], 1);
    assert_eq!(body["results_by_type"]["wiki_pages"], 1);
}

#[tokio::test]
async fn search_entire_redmine_rejects_an_empty_query_as_an_argument_error() {
    let h = support::harness(&[]).await;
    let mut request = CallToolRequestParams::new("search_entire_redmine".to_string());
    request.arguments = json!({"query": "   "}).as_object().cloned();
    let result = h.client.call_tool(request).await;
    assert!(
        result.is_err(),
        "an empty query should be a protocol-level argument error"
    );
}

#[tokio::test]
async fn search_entire_redmine_dominant_error_is_in_band() {
    let h = support::harness(&[]).await;
    Mock::given(method("GET"))
        .and(path("/search.json"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&h.redmine)
        .await;

    let result = call(&h, "search_entire_redmine", json!({"query": "bug"})).await;
    assert_eq!(result.is_error, Some(true));
}

// --- manage_redmine_wiki_page ---

fn wiki_page_json(title: &str, text: &str, version: u32) -> Value {
    json!({
        "wiki_page": {
            "title": title, "text": text, "version": version,
            "created_on": "2026-01-01T00:00:00Z"
        }
    })
}

#[tokio::test]
async fn manage_redmine_wiki_page_list_happy_path() {
    let h = support::harness(&[]).await;
    Mock::given(method("GET"))
        .and(path("/projects/my-project/wiki/index.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "wiki_pages": [
                {"title": "Home", "version": 1, "created_on": "2026-01-01T00:00:00Z"}
            ]
        })))
        .mount(&h.redmine)
        .await;

    let result = call(
        &h,
        "manage_redmine_wiki_page",
        json!({"action": "list", "project_id": "my-project"}),
    )
    .await;
    assert_ne!(result.is_error, Some(true));
    let body = body_of(&result);
    assert_eq!(body["pages"].as_array().expect("pages").len(), 1);
}

#[tokio::test]
async fn manage_redmine_wiki_page_get_with_version_requests_the_version_path_segment() {
    let h = support::harness(&[]).await;
    Mock::given(method("GET"))
        .and(path("/projects/my-project/wiki/Home/3.json"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(wiki_page_json("Home", "old text", 3)),
        )
        .expect(1)
        .mount(&h.redmine)
        .await;

    let result = call(
        &h,
        "manage_redmine_wiki_page",
        json!({"action": "get", "project_id": "my-project", "wiki_page_title": "Home", "version": 3}),
    )
    .await;
    assert_ne!(result.is_error, Some(true));
    let body = body_of(&result);
    assert_eq!(body["page"]["version"], 3);
}

#[tokio::test]
async fn manage_redmine_wiki_page_get_rewrites_attachment_content_url_when_redmine_public_url_is_set()
 {
    let h = support::harness(&[("REDMINE_PUBLIC_URL", "https://public.example.com")]).await;
    let mut page = wiki_page_json("Home", "old text", 3);
    page["wiki_page"]["attachments"] = json!([{
        "id": 5, "filename": "a.pdf", "filesize": 10,
        "content_url": format!("{}/attachments/download/5/a.pdf", h.redmine.uri()),
        "created_on": "2026-01-01T00:00:00Z"
    }]);
    Mock::given(method("GET"))
        .and(path("/projects/my-project/wiki/Home.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(page))
        .mount(&h.redmine)
        .await;

    let result = call(
        &h,
        "manage_redmine_wiki_page",
        json!({"action": "get", "project_id": "my-project", "wiki_page_title": "Home"}),
    )
    .await;
    assert_ne!(result.is_error, Some(true));
    let body = body_of(&result);
    assert_eq!(
        body["page"]["attachments"][0]["content_url"],
        "https://public.example.com/attachments/download/5/a.pdf"
    );
}

#[tokio::test]
async fn manage_redmine_wiki_page_create_sends_expected_body() {
    let h = support::harness(&[]).await;
    Mock::given(method("PUT"))
        .and(path("/projects/my-project/wiki/Home.json"))
        .and(body_json(json!({"wiki_page": {"text": "Welcome"}})))
        .respond_with(
            ResponseTemplate::new(201).set_body_json(wiki_page_json("Home", "Welcome", 1)),
        )
        .expect(1)
        .mount(&h.redmine)
        .await;
    Mock::given(method("GET"))
        .and(path("/projects/my-project/wiki/Home.json"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(wiki_page_json("Home", "Welcome", 1)),
        )
        .expect(1)
        .mount(&h.redmine)
        .await;

    let result = call(
        &h,
        "manage_redmine_wiki_page",
        json!({
            "action": "create", "project_id": "my-project",
            "wiki_page_title": "Home", "text": "Welcome"
        }),
    )
    .await;
    assert_ne!(result.is_error, Some(true));
    let body = body_of(&result);
    assert_eq!(body["page"]["version"], 1);
}

#[tokio::test]
async fn manage_redmine_wiki_page_delete_reports_children_are_unparented_not_deleted() {
    let h = support::harness(&[]).await;
    Mock::given(method("DELETE"))
        .and(path("/projects/my-project/wiki/Obsolete.json"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&h.redmine)
        .await;

    let result = call(
        &h,
        "manage_redmine_wiki_page",
        json!({"action": "delete", "project_id": "my-project", "wiki_page_title": "Obsolete"}),
    )
    .await;
    assert_ne!(result.is_error, Some(true));
    let body = body_of(&result);
    assert_eq!(body["deleted_title"], "Obsolete");
    assert!(
        body["message"]
            .as_str()
            .expect("message")
            .contains("un-parented")
    );
}

#[tokio::test]
async fn manage_redmine_wiki_page_rename_issues_a_put_at_the_old_title_then_a_get_at_the_new_title()
{
    let h = support::harness(&[]).await;
    Mock::given(method("GET"))
        .and(path("/projects/my-project/wiki/Old_Title.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(wiki_page_json(
            "Old_Title",
            "body",
            2,
        )))
        .expect(1)
        .mount(&h.redmine)
        .await;
    Mock::given(method("PUT"))
        .and(path("/projects/my-project/wiki/Old_Title.json"))
        .and(body_json(json!({
            "wiki_page": {"text": "body", "title": "New_Title"}
        })))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&h.redmine)
        .await;
    Mock::given(method("GET"))
        .and(path("/projects/my-project/wiki/New_Title.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(wiki_page_json(
            "New_Title",
            "body",
            3,
        )))
        .expect(1)
        .mount(&h.redmine)
        .await;

    let result = call(
        &h,
        "manage_redmine_wiki_page",
        json!({
            "action": "rename", "project_id": "my-project",
            "wiki_page_title": "Old_Title", "new_title": "New_Title"
        }),
    )
    .await;
    assert_ne!(result.is_error, Some(true));
    let body = body_of(&result);
    assert_eq!(body["page"]["title"], "New_Title");
}

#[tokio::test]
async fn manage_redmine_wiki_page_rename_silently_dropped_by_redmine_surfaces_as_forbidden() {
    let h = support::harness(&[]).await;
    Mock::given(method("GET"))
        .and(path("/projects/my-project/wiki/Old_Title.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(wiki_page_json(
            "Old_Title",
            "body",
            2,
        )))
        .mount(&h.redmine)
        .await;
    Mock::given(method("PUT"))
        .and(path("/projects/my-project/wiki/Old_Title.json"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&h.redmine)
        .await;
    // Redmine silently dropped the title change (lacking rename_wiki_pages):
    // the page still does not exist at the new title.
    Mock::given(method("GET"))
        .and(path("/projects/my-project/wiki/New_Title.json"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&h.redmine)
        .await;

    let result = call(
        &h,
        "manage_redmine_wiki_page",
        json!({
            "action": "rename", "project_id": "my-project",
            "wiki_page_title": "Old_Title", "new_title": "New_Title"
        }),
    )
    .await;
    assert_eq!(result.is_error, Some(true));
    let body = body_of(&result);
    assert_eq!(body["code"], "FORBIDDEN");
    assert!(
        body["hint"]
            .as_str()
            .expect("hint")
            .contains("rename_wiki_pages")
    );
}

#[tokio::test]
async fn manage_redmine_wiki_page_rename_sends_redirect_existing_links_as_the_string_zero() {
    let h = support::harness(&[]).await;
    Mock::given(method("GET"))
        .and(path("/projects/my-project/wiki/Old_Title.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(wiki_page_json(
            "Old_Title",
            "body",
            2,
        )))
        .mount(&h.redmine)
        .await;
    Mock::given(method("PUT"))
        .and(path("/projects/my-project/wiki/Old_Title.json"))
        .and(body_json(json!({
            "wiki_page": {"text": "body", "title": "New_Title", "redirect_existing_links": "0"}
        })))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&h.redmine)
        .await;
    Mock::given(method("GET"))
        .and(path("/projects/my-project/wiki/New_Title.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(wiki_page_json(
            "New_Title",
            "body",
            3,
        )))
        .mount(&h.redmine)
        .await;

    let result = call(
        &h,
        "manage_redmine_wiki_page",
        json!({
            "action": "rename", "project_id": "my-project",
            "wiki_page_title": "Old_Title", "new_title": "New_Title",
            "redirect_existing_links": false
        }),
    )
    .await;
    assert_ne!(result.is_error, Some(true));
}

#[tokio::test]
async fn manage_redmine_wiki_page_rename_rejects_a_new_title_equal_to_the_old_one() {
    let h = support::harness(&[]).await;
    let mut request = CallToolRequestParams::new("manage_redmine_wiki_page".to_string());
    request.arguments = json!({
        "action": "rename", "project_id": "my-project",
        "wiki_page_title": "Home", "new_title": "Home"
    })
    .as_object()
    .cloned();
    let result = h.client.call_tool(request).await;
    assert!(
        result.is_err(),
        "new_title == wiki_page_title should be a protocol-level argument error"
    );
}

#[tokio::test]
async fn manage_redmine_wiki_page_rejects_a_traversal_title_before_any_http_request() {
    let h = support::harness(&[]).await;
    // No mocks mounted: a request reaching the mock server with no matcher
    // would panic wiremock, proving no HTTP call was made if this succeeds.
    let mut request = CallToolRequestParams::new("manage_redmine_wiki_page".to_string());
    request.arguments = json!({
        "action": "get", "project_id": "my-project",
        "wiki_page_title": "../../etc/passwd"
    })
    .as_object()
    .cloned();
    let result = h.client.call_tool(request).await;
    assert!(
        result.is_err(),
        "a path-traversal wiki_page_title should be a protocol-level argument error"
    );
}

#[tokio::test]
async fn manage_redmine_wiki_page_rejects_a_traversal_new_title_before_any_http_request() {
    let h = support::harness(&[]).await;
    let mut request = CallToolRequestParams::new("manage_redmine_wiki_page".to_string());
    request.arguments = json!({
        "action": "rename", "project_id": "my-project",
        "wiki_page_title": "Home", "new_title": "../../etc/passwd"
    })
    .as_object()
    .cloned();
    let result = h.client.call_tool(request).await;
    assert!(
        result.is_err(),
        "a path-traversal new_title should be a protocol-level argument error"
    );
}

#[tokio::test]
async fn manage_redmine_wiki_page_missing_required_field_per_action_is_an_argument_error() {
    let h = support::harness(&[]).await;
    let cases: &[(&str, Value)] = &[
        (
            "get without wiki_page_title",
            json!({"action": "get", "project_id": "my-project"}),
        ),
        (
            "create without wiki_page_title",
            json!({"action": "create", "project_id": "my-project", "text": "x"}),
        ),
        (
            "create without text",
            json!({"action": "create", "project_id": "my-project", "wiki_page_title": "Home"}),
        ),
        (
            "update without text",
            json!({"action": "update", "project_id": "my-project", "wiki_page_title": "Home"}),
        ),
        (
            "delete without wiki_page_title",
            json!({"action": "delete", "project_id": "my-project"}),
        ),
        (
            "rename without wiki_page_title",
            json!({"action": "rename", "project_id": "my-project", "new_title": "New"}),
        ),
        (
            "rename without new_title",
            json!({"action": "rename", "project_id": "my-project", "wiki_page_title": "Home"}),
        ),
    ];
    for (why, args) in cases {
        let mut request = CallToolRequestParams::new("manage_redmine_wiki_page".to_string());
        request.arguments = args.as_object().cloned();
        let result = h.client.call_tool(request).await;
        assert!(result.is_err(), "expected an argument error for: {why}");
    }
}

#[tokio::test]
async fn manage_redmine_wiki_page_dominant_error_is_in_band() {
    let h = support::harness(&[]).await;
    Mock::given(method("GET"))
        .and(path("/projects/my-project/wiki/Missing.json"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&h.redmine)
        .await;

    let result = call(
        &h,
        "manage_redmine_wiki_page",
        json!({"action": "get", "project_id": "my-project", "wiki_page_title": "Missing"}),
    )
    .await;
    assert_eq!(result.is_error, Some(true));
    assert_eq!(result.structured_content.unwrap()["code"], "NOT_FOUND");
}
