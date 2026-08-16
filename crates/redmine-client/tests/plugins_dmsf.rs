//! DMSF: `GET /projects/{pid}/dmsf.json`, `GET /dmsf_files/{id}.json`,
//! `POST /projects/{pid}/dmsf/commit.json`,
//! `POST /dmsf/files/{id}/revision/create.json`. Synthetic fixtures — see
//! `tests/fixtures/README.md`'s plugin fixtures section.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

mod support;

use std::str::FromStr as _;

use redmine_client::model::custom_field::{CustomFieldValue, CustomFieldWrite};
use redmine_client::model::plugins::dmsf::{
    DmsfCommitRequest, DmsfRevisionWrite, DmsfUploadedFile, DmsfVersion,
};
use redmine_client::{Credential, DmsfFolderId, DocumentId, Error, ProjectId, ProjectIdent};
use secrecy::SecretString;
use wiremock::matchers::{body_json, method, path, query_param};
use wiremock::{Mock, ResponseTemplate};

fn cred() -> Credential {
    Credential::ApiKey(SecretString::from("k"))
}

fn project() -> ProjectIdent {
    ProjectIdent::Id(ProjectId(1))
}

#[tokio::test]
async fn list_dmsf_nodes_canonical_shape() {
    let (server, client) = support::mock_redmine().await;
    Mock::given(method("GET"))
        .and(path("/projects/1/dmsf.json"))
        .and(query_param("limit", "50"))
        .and(query_param("offset", "0"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string(support::fixture("dmsf_list_canonical")),
        )
        .mount(&server)
        .await;

    let page = client
        .as_user(&cred())
        .list_dmsf_nodes(&project(), None, 50, 0)
        .await
        .unwrap();
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.total_count, 1);
    assert_eq!(page.items[0].filename.as_deref(), Some("report.pdf"));
}

#[tokio::test]
async fn list_dmsf_nodes_bare_array_shape_falls_back_to_item_count() {
    let (server, client) = support::mock_redmine().await;
    Mock::given(method("GET"))
        .and(path("/projects/1/dmsf.json"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string(support::fixture("dmsf_list_bare")),
        )
        .mount(&server)
        .await;

    let page = client
        .as_user(&cred())
        .list_dmsf_nodes(&project(), None, 50, 0)
        .await
        .unwrap();
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.total_count, 1);
}

#[tokio::test]
async fn list_dmsf_nodes_sends_folder_id_when_given() {
    let (server, client) = support::mock_redmine().await;
    Mock::given(method("GET"))
        .and(path("/projects/1/dmsf.json"))
        .and(query_param("folder_id", "9"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string(support::fixture("dmsf_list_bare")),
        )
        .mount(&server)
        .await;

    client
        .as_user(&cred())
        .list_dmsf_nodes(&project(), Some(DmsfFolderId(9)), 50, 0)
        .await
        .unwrap();
}

#[tokio::test]
async fn list_dmsf_nodes_rejects_a_third_shape() {
    let (server, client) = support::mock_redmine().await;
    Mock::given(method("GET"))
        .and(path("/projects/1/dmsf.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "dmsf": {"nodes": []}
        })))
        .mount(&server)
        .await;

    let err = client
        .as_user(&cred())
        .list_dmsf_nodes(&project(), None, 50, 0)
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Decode { .. }));
}

#[tokio::test]
async fn get_dmsf_file_merges_the_latest_revision() {
    let (server, client) = support::mock_redmine().await;
    Mock::given(method("GET"))
        .and(path("/dmsf_files/42.json"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string(support::fixture("dmsf_file_show")),
        )
        .mount(&server)
        .await;

    let node = client
        .as_user(&cred())
        .get_dmsf_file(DocumentId(42))
        .await
        .unwrap()
        .expect("has a revision");
    assert_eq!(node.name.as_deref(), Some("report.pdf"));
    assert_eq!(node.title.as_deref(), Some("Report"));
    assert_eq!(node.version.as_deref(), Some("1.2.0"));
    assert_eq!(node.folder_id, Some(3));
}

#[tokio::test]
async fn get_dmsf_file_with_no_revisions_is_none() {
    let (server, client) = support::mock_redmine().await;
    Mock::given(method("GET"))
        .and(path("/dmsf_files/7.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "dmsf_file": {"id": 7, "dmsf_file_revisions": []}
        })))
        .mount(&server)
        .await;

    let node = client
        .as_user(&cred())
        .get_dmsf_file(DocumentId(7))
        .await
        .unwrap();
    assert!(node.is_none());
}

#[tokio::test]
async fn get_dmsf_file_404_is_not_found() {
    let (server, client) = support::mock_redmine().await;
    Mock::given(method("GET"))
        .and(path("/dmsf_files/999.json"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;

    let err = client
        .as_user(&cred())
        .get_dmsf_file(DocumentId(999))
        .await
        .unwrap_err();
    assert!(matches!(err, Error::NotFound));
}

#[tokio::test]
async fn commit_dmsf_upload_sends_the_documented_envelope_including_field_name_traps() {
    let (server, client) = support::mock_redmine().await;
    Mock::given(method("POST"))
        .and(path("/projects/1/dmsf/commit.json"))
        .and(body_json(serde_json::json!({
            "attachments": {"uploaded_file": {
                "token": "43.abcdef", "name": "report.pdf", "title": "Report",
                "version_major": 1, "version_minor": 2, "version_patch": 0
            }},
            "folder_id": 3
        })))
        .respond_with(
            ResponseTemplate::new(200).set_body_string(support::fixture("dmsf_commit_response")),
        )
        .mount(&server)
        .await;

    let req = DmsfCommitRequest {
        uploaded_file: DmsfUploadedFile {
            token: "43.abcdef".to_string(),
            name: "report.pdf".to_string(),
            title: Some("Report".to_string()),
            description: None,
            comment: None,
            version_major: Some(1),
            version_minor: Some(2),
            version_patch: Some(0),
            custom_field_values: None,
        },
        folder_id: Some(3),
    };
    let nodes = client
        .as_user(&cred())
        .commit_dmsf_upload(&project(), &req)
        .await
        .unwrap();
    assert_eq!(nodes.len(), 1);
    assert_eq!(nodes[0].id, 43);
    assert_eq!(nodes[0].name.as_deref(), Some("report.pdf"));
    // The commit response is deliberately sparse: no version/description.
    assert_eq!(nodes[0].version, None);
}

#[tokio::test]
async fn commit_dmsf_upload_spells_custom_field_values_not_custom_fields() {
    let (server, client) = support::mock_redmine().await;
    Mock::given(method("POST"))
        .and(path("/projects/1/dmsf/commit.json"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string(support::fixture("dmsf_commit_response")),
        )
        .mount(&server)
        .await;

    let req = DmsfCommitRequest {
        uploaded_file: DmsfUploadedFile {
            token: "43.abcdef".to_string(),
            name: "report.pdf".to_string(),
            title: None,
            description: None,
            comment: None,
            version_major: None,
            version_minor: None,
            version_patch: None,
            custom_field_values: Some(vec![CustomFieldWrite {
                id: 1,
                value: CustomFieldValue::Single(Some("x".to_string())),
            }]),
        },
        folder_id: None,
    };
    client
        .as_user(&cred())
        .commit_dmsf_upload(&project(), &req)
        .await
        .unwrap();

    let requests = server.received_requests().await.unwrap();
    let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
    let uploaded_file = &body["attachments"]["uploaded_file"];
    assert!(uploaded_file.get("custom_field_values").is_some());
    assert!(uploaded_file.get("custom_fields").is_none());
}

#[tokio::test]
async fn create_dmsf_revision_uses_the_slash_route_not_the_underscore_show_route() {
    let (server, client) = support::mock_redmine().await;
    Mock::given(method("POST"))
        .and(path("/dmsf/files/42/revision/create.json"))
        .and(body_json(serde_json::json!({
            "dmsf_file_revision": {"title": "Report", "name": "report.pdf"}
        })))
        .respond_with(ResponseTemplate::new(204))
        .mount(&server)
        .await;

    let req = DmsfRevisionWrite {
        title: "Report".to_string(),
        name: "report.pdf".to_string(),
        description: None,
        comment: None,
        custom_field_values: None,
    };
    client
        .as_user(&cred())
        .create_dmsf_revision(DocumentId(42), &req)
        .await
        .unwrap();
}

#[tokio::test]
async fn create_dmsf_revision_never_posts_to_the_underscore_form() {
    let (server, client) = support::mock_redmine().await;
    // Only the slash-route mock is registered; a request to the underscore
    // form (`dmsf_files/{id}` with no `/revision/create`) would 404 against
    // wiremock's default "no matching mock" response, proving the client
    // never sends it.
    Mock::given(method("POST"))
        .and(path("/dmsf/files/42/revision/create.json"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&server)
        .await;

    let req = DmsfRevisionWrite {
        title: "Report".to_string(),
        name: "report.pdf".to_string(),
        description: Some("Updated".to_string()),
        comment: None,
        custom_field_values: None,
    };
    client
        .as_user(&cred())
        .create_dmsf_revision(DocumentId(42), &req)
        .await
        .unwrap();
}

#[test]
fn version_from_str_matches_the_model_parser() {
    assert_eq!(
        DmsfVersion::from_str("1.2.3").unwrap(),
        DmsfVersion {
            major: 1,
            minor: 2,
            patch: 3
        }
    );
    assert!(DmsfVersion::from_str("1.2.3.4").is_err());
}
