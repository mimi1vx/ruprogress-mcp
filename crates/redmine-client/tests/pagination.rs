//! Pagination termination: full walk, cap truncation, and the zero-progress
//! guard against a misbehaving/proxied server.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use redmine_client::model::issue::IssueQuery as IQ;
use redmine_client::{Credential, Limits, RedmineClientBuilder};
use secrecy::SecretString;
use serde_json::json;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, ResponseTemplate};

fn minimal_issue(id: u64) -> serde_json::Value {
    json!({
        "id": id,
        "project": {"id": 1, "name": "P"},
        "tracker": {"id": 1, "name": "Bug"},
        "status": {"id": 1, "name": "New"},
        "priority": {"id": 1, "name": "Normal"},
        "author": {"id": 1, "name": "A"},
        "subject": format!("issue {id}"),
        "created_on": "2026-01-01T00:00:00Z",
        "updated_on": "2026-01-01T00:00:00Z"
    })
}

fn page(
    items: &[serde_json::Value],
    total_count: u64,
    offset: u64,
    limit: u32,
) -> serde_json::Value {
    json!({ "issues": items, "total_count": total_count, "offset": offset, "limit": limit })
}

#[tokio::test]
async fn three_page_walk_collects_every_item_and_is_not_truncated() {
    let server = wiremock::MockServer::start().await;
    let base = server.uri().parse().unwrap();
    let client = RedmineClientBuilder::new(base)
        .credential(Credential::ApiKey(SecretString::from("k")))
        .limits(Limits {
            page_size: 2,
            ..Limits::default()
        })
        .build()
        .unwrap();

    Mock::given(method("GET"))
        .and(path("/issues.json"))
        .and(query_param("offset", "0"))
        .respond_with(ResponseTemplate::new(200).set_body_json(page(
            &[minimal_issue(1), minimal_issue(2)],
            5,
            0,
            2,
        )))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/issues.json"))
        .and(query_param("offset", "2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(page(
            &[minimal_issue(3), minimal_issue(4)],
            5,
            2,
            2,
        )))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/issues.json"))
        .and(query_param("offset", "4"))
        .respond_with(ResponseTemplate::new(200).set_body_json(page(&[minimal_issue(5)], 5, 4, 2)))
        .mount(&server)
        .await;

    let cred = Credential::ApiKey(SecretString::from("k"));
    let result = client
        .as_user(&cred)
        .list_issues(&IQ::default())
        .await
        .expect("pagination walk should succeed");

    assert_eq!(result.items.len(), 5);
    assert_eq!(result.total_count, 5);
    assert!(!result.truncated);
}

#[tokio::test]
async fn max_pages_cap_truncates_without_erroring() {
    let server = wiremock::MockServer::start().await;
    let base = server.uri().parse().unwrap();
    let client = RedmineClientBuilder::new(base)
        .credential(Credential::ApiKey(SecretString::from("k")))
        .limits(Limits {
            page_size: 1,
            max_pages: 2,
            ..Limits::default()
        })
        .build()
        .unwrap();

    // Always return a full page with a huge total_count: the walk should
    // stop at max_pages, not error, and report truncated.
    Mock::given(method("GET"))
        .and(path("/issues.json"))
        .and(query_param("offset", "0"))
        .respond_with(ResponseTemplate::new(200).set_body_json(page(
            &[minimal_issue(1)],
            1_000,
            0,
            1,
        )))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/issues.json"))
        .and(query_param("offset", "1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(page(
            &[minimal_issue(2)],
            1_000,
            1,
            1,
        )))
        .mount(&server)
        .await;

    let cred = Credential::ApiKey(SecretString::from("k"));
    let result = client
        .as_user(&cred)
        .list_issues(&IQ::default())
        .await
        .expect("capped pagination must not error");

    assert_eq!(
        result.items.len(),
        2,
        "should stop after max_pages=2 pages of 1 item"
    );
    assert!(result.truncated);
}

#[tokio::test]
async fn zero_progress_server_terminates_instead_of_looping_forever() {
    let server = wiremock::MockServer::start().await;
    let base = server.uri().parse().unwrap();
    let client = RedmineClientBuilder::new(base)
        .credential(Credential::ApiKey(SecretString::from("k")))
        .limits(Limits {
            page_size: 10,
            max_pages: 1000,
            ..Limits::default()
        })
        .build()
        .unwrap();

    // A misbehaving/proxied server: every request (regardless of offset)
    // returns the same non-empty page with offset stuck at 0.
    Mock::given(method("GET"))
        .and(path("/issues.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(page(
            &[minimal_issue(1), minimal_issue(2)],
            1_000,
            0,
            10,
        )))
        .mount(&server)
        .await;

    let cred = Credential::ApiKey(SecretString::from("k"));
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        client.as_user(&cred).list_issues(&IQ::default()),
    )
    .await
    .expect("zero-progress guard must terminate quickly, not hang")
    .expect("zero-progress must not be an error");

    assert!(result.truncated);
}

#[tokio::test]
async fn total_count_u64_max_terminates_at_the_page_cap() {
    let server = wiremock::MockServer::start().await;
    let base = server.uri().parse().unwrap();
    let client = RedmineClientBuilder::new(base)
        .credential(Credential::ApiKey(SecretString::from("k")))
        .limits(Limits {
            page_size: 5,
            max_pages: 3,
            ..Limits::default()
        })
        .build()
        .unwrap();

    // Any offset: return 5 fresh items and claim an effectively unbounded
    // total_count, so only the page cap can stop the walk.
    Mock::given(method("GET"))
        .and(path("/issues.json"))
        .respond_with(move |req: &wiremock::Request| {
            let offset: u64 = req
                .url
                .query_pairs()
                .find(|(k, _)| k == "offset")
                .and_then(|(_, v)| v.parse().ok())
                .unwrap_or(0);
            let items: Vec<_> = (0..5).map(|i| minimal_issue(offset + i + 1)).collect();
            ResponseTemplate::new(200).set_body_json(page(&items, u64::MAX, offset, 5))
        })
        .mount(&server)
        .await;

    let cred = Credential::ApiKey(SecretString::from("k"));
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        client.as_user(&cred).list_issues(&IQ::default()),
    )
    .await
    .expect("must terminate at the page cap, not run away")
    .expect("cap termination must not be an error");

    assert_eq!(result.items.len(), 15, "3 pages x 5 items");
    assert!(result.truncated);
}
