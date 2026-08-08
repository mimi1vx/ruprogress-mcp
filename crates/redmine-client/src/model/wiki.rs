//! `GET/PUT/DELETE /projects/{id}/wiki/{title}`, `GET
//! /projects/{id}/wiki/index`.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::attachment::Attachment;
use super::{BareCollection, IdName, permissive_datetime};

/// A Redmine wiki page (`get`/`create`/`update`/`rename` shape —
/// `wiki/show.api.rsb`). Not used for `list`: see [`WikiPageListItem`].
#[non_exhaustive]
#[derive(Debug, Clone, Deserialize)]
pub struct WikiPage {
    /// The page title.
    pub title: String,
    /// The page's Textile/Markdown source, if included.
    #[serde(default)]
    pub text: Option<String>,
    /// The revision number.
    pub version: u32,
    /// Who last edited this revision.
    #[serde(default)]
    pub author: Option<IdName>,
    /// Edit comment for this revision.
    #[serde(default)]
    pub comments: Option<String>,
    /// The parent page, if any.
    #[serde(default)]
    pub parent: Option<WikiPageRef>,
    /// The owning project. `None` when not requested — mirrors this
    /// codebase's "requested vs. absent" convention elsewhere (e.g.
    /// `Project.trackers`); the reference contract additionally notes older
    /// Redmine versions (pre-7.0) omit this key outright.
    #[serde(default)]
    pub project: Option<IdName>,
    /// When this revision was created.
    #[serde(deserialize_with = "permissive_datetime")]
    pub created_on: DateTime<Utc>,
    /// When the page was last updated.
    #[serde(default, deserialize_with = "super::permissive_datetime_opt")]
    pub updated_on: Option<DateTime<Utc>>,
    /// Attachment metadata. `None` when not requested (`include=attachments`
    /// was not sent), `Some(vec![])` when requested but empty.
    #[serde(default)]
    pub attachments: Option<Vec<Attachment>>,
}

/// A reference to a wiki page by title, as used in `parent`.
#[non_exhaustive]
#[derive(Debug, Clone, Deserialize)]
pub struct WikiPageRef {
    /// The referenced page's title.
    pub title: String,
}

/// One entry from `GET /projects/{id}/wiki/index.json`
/// (`wiki/index.api.rsb`) — deliberately thinner than [`WikiPage`]: the
/// index view never renders `text`/`author`/`comments`/`project`, so this is
/// a distinct type rather than a `WikiPage` with those fields always `None`
/// (which would misrepresent "never sent by this endpoint" as "requested
/// but absent").
#[non_exhaustive]
#[derive(Debug, Clone, Deserialize)]
pub struct WikiPageListItem {
    /// The page title.
    pub title: String,
    /// The parent page, if any.
    #[serde(default)]
    pub parent: Option<WikiPageRef>,
    /// The latest revision number.
    pub version: u32,
    /// When the page was first created.
    #[serde(deserialize_with = "permissive_datetime")]
    pub created_on: DateTime<Utc>,
    /// When the page was last updated.
    #[serde(default, deserialize_with = "super::permissive_datetime_opt")]
    pub updated_on: Option<DateTime<Utc>>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct WikiPagesEnvelope {
    pub wiki_pages: Vec<WikiPageListItem>,
}

impl BareCollection for WikiPagesEnvelope {
    type Item = WikiPageListItem;

    fn into_items(self) -> Vec<WikiPageListItem> {
        self.wiki_pages
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct WikiPageEnvelope {
    pub wiki_page: WikiPage,
}

/// The body of a `PUT /projects/{id}/wiki/{title}.json` request — Redmine's
/// single upsert endpoint for `create`, `update`, and (via `title`)
/// `rename`.
#[derive(Debug, Clone, Default, Serialize)]
pub struct WikiPageWrite {
    /// Page content. Required on every write: `WikiContent` validates its
    /// presence, and Redmine has no partial-patch form for wiki pages the
    /// way `TimeEntryUpdate` has for time entries.
    pub text: String,
    /// Change-log comment for this revision.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub comments: Option<String>,
    /// A new title — the `rename` mechanism. Silently dropped by
    /// Redmine if the credential lacks `rename_wiki_pages`: callers
    /// must re-fetch at the new title to confirm the rename actually took
    /// effect.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// `Some("0")` to suppress the `WikiRedirect` `rename` would otherwise
    /// leave behind; `None` (Redmine's own default) to create it.
    ///
    /// **Must** be the string `"0"`, never a JSON boolean:
    /// Redmine's `handle_rename_or_move` checks `redirect_existing_links ==
    /// "0"`, and a JSON-decoded `false` is not `== "0"` in Ruby, so sending
    /// `false` would silently fail to suppress the redirect.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub redirect_existing_links: Option<&'static str>,
}

#[derive(Debug, Serialize)]
pub(crate) struct WikiPageWriteEnvelope<'a> {
    pub wiki_page: &'a WikiPageWrite,
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]
mod tests {
    use super::*;

    // Inline fixture: see tests/fixtures/README.md for the policy that
    // applies to models with a real API method.
    const JSON: &str = r#"{"wiki_page": {
        "title": "Home", "text": "Welcome", "version": 3,
        "created_on": "2026-01-01T00:00:00Z"
    }}"#;

    #[test]
    fn round_trips() {
        let env: WikiPageEnvelope = serde_json::from_str(JSON).expect("should parse");
        assert_eq!(env.wiki_page.title, "Home");
        assert_eq!(env.wiki_page.updated_on, None);
        assert!(env.wiki_page.project.is_none());
        assert!(env.wiki_page.attachments.is_none());
    }

    #[test]
    fn round_trips_a_page_with_project_and_attachments() {
        let json = r#"{"wiki_page": {
            "title": "Home", "text": "Welcome", "version": 3,
            "project": {"id": 1, "name": "My Project"},
            "created_on": "2026-01-01T00:00:00Z",
            "attachments": [
                {"id": 1, "filename": "a.png", "filesize": 10,
                 "content_url": "https://x/attachments/1", "created_on": "2026-01-01T00:00:00Z"}
            ]
        }}"#;
        let env: WikiPageEnvelope = serde_json::from_str(json).expect("should parse");
        assert_eq!(env.wiki_page.project.expect("project").id, 1);
        assert_eq!(env.wiki_page.attachments.expect("attachments").len(), 1);
    }

    #[test]
    fn wiki_page_list_item_round_trips_with_and_without_parent() {
        let env: WikiPagesEnvelope = serde_json::from_str(
            r#"{"wiki_pages": [
                {"title": "Home", "version": 1, "created_on": "2026-01-01T00:00:00Z"},
                {"title": "Child", "parent": {"title": "Home"}, "version": 2,
                 "created_on": "2026-01-01T00:00:00Z", "updated_on": "2026-01-02T00:00:00Z"}
            ]}"#,
        )
        .expect("should parse");
        assert_eq!(env.wiki_pages.len(), 2);
        assert!(env.wiki_pages[0].parent.is_none());
        assert_eq!(
            env.wiki_pages[1].parent.as_ref().expect("parent").title,
            "Home"
        );
    }

    #[test]
    fn wiki_page_write_serializes_redirect_existing_links_as_the_string_zero_not_a_bool() {
        let write = WikiPageWrite {
            text: "hello".to_string(),
            title: Some("New_Title".to_string()),
            redirect_existing_links: Some("0"),
            ..WikiPageWrite::default()
        };
        let json = serde_json::to_value(&write).expect("should serialize");
        assert_eq!(json["redirect_existing_links"], "0");
        assert_ne!(json["redirect_existing_links"], serde_json::json!(false));
    }

    #[test]
    fn wiki_page_write_omits_optional_fields_when_none() {
        let write = WikiPageWrite {
            text: "hello".to_string(),
            ..WikiPageWrite::default()
        };
        let json = serde_json::to_value(&write).expect("should serialize");
        assert_eq!(json.as_object().expect("object").len(), 1);
        assert_eq!(json["text"], "hello");
    }
}
