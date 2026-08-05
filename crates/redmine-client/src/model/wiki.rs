//! `GET/PUT /projects/{id}/wiki/{title}`.

use chrono::{DateTime, Utc};
use serde::Deserialize;

use super::{IdName, permissive_datetime};

/// A Redmine wiki page.
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
    /// When this revision was created.
    #[serde(deserialize_with = "permissive_datetime")]
    pub created_on: DateTime<Utc>,
    /// When the page was last updated.
    #[serde(default, deserialize_with = "super::permissive_datetime_opt")]
    pub updated_on: Option<DateTime<Utc>>,
}

/// A reference to a wiki page by title, as used in `parent`.
#[non_exhaustive]
#[derive(Debug, Clone, Deserialize)]
pub struct WikiPageRef {
    /// The referenced page's title.
    pub title: String,
}

#[derive(Debug, Deserialize)]
#[allow(
    dead_code,
    reason = "model exists for round-trip tests; no phase-1 API method uses it yet"
)]
pub(crate) struct WikiPageEnvelope {
    pub wiki_page: WikiPage,
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    // Inline fixture: see tests/fixtures/README.md for the policy that
    // applies to models with a real API method in phase 1's surface.
    const JSON: &str = r#"{"wiki_page": {
        "title": "Home", "text": "Welcome", "version": 3,
        "created_on": "2026-01-01T00:00:00Z"
    }}"#;

    #[test]
    fn round_trips() {
        let env: WikiPageEnvelope = serde_json::from_str(JSON).expect("should parse");
        assert_eq!(env.wiki_page.title, "Home");
        assert_eq!(env.wiki_page.updated_on, None);
    }
}
