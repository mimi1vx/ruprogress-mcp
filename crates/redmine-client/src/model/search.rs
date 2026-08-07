//! `GET /search.json` — Redmine's cross-resource text search, used here
//! restricted to issues (`search_redmine_issues`). Genuinely paginated
//! (`SearchController#index` calls `api_offset_and_limit` for the API
//! format), unlike the four bare-collection endpoints from 4a.

use chrono::{DateTime, Utc};
use serde::Deserialize;

use super::{Collection, permissive_datetime};

/// One search hit. Deliberately thin — Redmine's search index never embeds
/// a full issue dict, only enough to identify and rank the match. Callers
/// that need full issue fields hydrate the ids via a second call (see
/// `plans/phase-4b-issues.md` decision G3).
#[non_exhaustive]
#[derive(Debug, Clone, Deserialize)]
pub struct SearchResult {
    /// The matched resource's id (an issue id, when searching issues only).
    pub id: u64,
    /// A short title for the match.
    pub title: String,
    /// The resource type Redmine matched (`"issue"` when restricted to
    /// issues, as this client always does).
    #[serde(rename = "type")]
    pub kind: String,
    /// A browser-facing URL to the resource. Never surfaced to a model
    /// verbatim — see the tool layer's `Boundary`/host-leak conventions.
    pub url: String,
    /// A short excerpt around the match, if any.
    #[serde(default)]
    pub description: Option<String>,
    /// When the matched resource was last touched.
    #[serde(deserialize_with = "permissive_datetime")]
    pub datetime: DateTime<Utc>,
}

/// Which projects `search.json` restricts results to. Redmine's real wire
/// values are `all`/`my_projects`/`bookmarks`/`subprojects`/(project-scoped
/// default); this client only exposes the three the reference tool
/// contract documents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchScope {
    /// No project restriction.
    All,
    /// Restrict to the caller's own projects. Sent on the wire as
    /// `my_projects` (plural) — Redmine has no singular `my_project` value;
    /// sending the reference contract's documented (singular) parameter
    /// name verbatim would silently degrade to `All` instead of restricting
    /// anything. See `plans/phase-4b-issues.md` decision G1.
    MyProject,
    /// Restrict to the current project and its descendants. Degenerate
    /// (behaves like `All`) when no project context exists, which is always
    /// the case for this client: `search_redmine_issues` takes no
    /// `project_id`.
    Subprojects,
}

impl SearchScope {
    const fn as_wire_value(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::MyProject => "my_projects",
            Self::Subprojects => "subprojects",
        }
    }
}

/// Which Redmine search-index scope(s) `search_entire_redmine` restricts to.
/// Wire query flags are additive (`issues=1`, `wiki_pages=1`) — Redmine's
/// `SearchController` ORs together whichever recognized-type flags are
/// present and only falls back to "every type" when none are given at all
/// (`plans/phase-4e-search-wiki.md` decision I1). This client always sends
/// an explicit flag per requested resource rather than relying on that
/// fallback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchResource {
    /// Issues.
    Issues,
    /// Wiki pages.
    WikiPages,
}

impl SearchResource {
    /// The `search.json` query flag for this resource — also used as the
    /// bucket name in `search_entire_redmine`'s output `type`/
    /// `results_by_type` fields (decision I2).
    #[must_use]
    pub const fn wire_param(self) -> &'static str {
        match self {
            Self::Issues => "issues",
            Self::WikiPages => "wiki_pages",
        }
    }

    /// Map Redmine's raw per-result `type` (`"issue"`, `"wiki-page"` —
    /// `Redmine::Acts::Event`'s default `self.class.name.underscore.dasherize`)
    /// back to the resource it belongs to. `None` for any other value: this
    /// client only ever sends `issues=1`/`wiki_pages=1`, so any other type
    /// would be unexpected, not silently bucketed.
    #[must_use]
    pub fn from_raw_type(raw: &str) -> Option<Self> {
        match raw {
            "issue" => Some(Self::Issues),
            "wiki-page" => Some(Self::WikiPages),
            _ => None,
        }
    }
}

/// Filter parameters for `GET /search.json`, restricted to one or more
/// resource types (`search_entire_redmine`). Unlike [`SearchQuery`] (issues
/// only, with `open_issues`/`scope`), this carries no issue-specific
/// filters — the reference contract's `search_entire_redmine` has neither.
#[derive(Debug, Clone)]
pub struct EntireSearchQuery {
    /// The search text.
    pub q: String,
    /// Which resource types to search. Each is sent as its own `=1` flag;
    /// an empty list would (per I1) fall back to Redmine's "every
    /// registered type" default — callers should pass the full set
    /// explicitly instead of relying on that.
    pub resources: Vec<SearchResource>,
}

impl EntireSearchQuery {
    /// Convert to the query-parameter map sent on the wire.
    #[must_use]
    pub fn to_query(&self) -> crate::client::Query {
        let mut query = crate::client::Query::default();
        query.insert("q", self.q.clone());
        for resource in &self.resources {
            query.insert(resource.wire_param(), "1");
        }
        query
    }
}

/// Filter parameters for `GET /search.json`, restricted to issues.
#[derive(Debug, Clone)]
pub struct SearchQuery {
    /// The search text.
    pub q: String,
    /// Restrict to a subset of projects.
    pub scope: Option<SearchScope>,
    /// Restrict to open issues only.
    pub open_issues: bool,
}

impl SearchQuery {
    /// Convert to the query-parameter map sent on the wire. Always sends
    /// `issues=1` — this client only ever searches issues.
    #[must_use]
    pub fn to_query(&self) -> crate::client::Query {
        let mut query = crate::client::Query::default();
        query.insert("q", self.q.clone());
        query.insert("issues", "1");
        if let Some(scope) = self.scope {
            query.insert("scope", scope.as_wire_value());
        }
        if self.open_issues {
            query.insert("open_issues", "1");
        }
        query
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct SearchResultsEnvelope {
    results: Vec<SearchResult>,
    total_count: u64,
    offset: u64,
    limit: u32,
}

impl Collection for SearchResultsEnvelope {
    type Item = SearchResult;

    fn total_count(&self) -> u64 {
        self.total_count
    }

    fn offset(&self) -> u64 {
        self.offset
    }

    fn limit(&self) -> u32 {
        self.limit
    }

    fn into_items(self) -> Vec<SearchResult> {
        self.results
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn scope_wire_values_use_the_plural_my_projects() {
        assert_eq!(SearchScope::All.as_wire_value(), "all");
        assert_eq!(SearchScope::MyProject.as_wire_value(), "my_projects");
        assert_eq!(SearchScope::Subprojects.as_wire_value(), "subprojects");
    }

    #[test]
    fn to_query_always_sends_issues_1() {
        let q = SearchQuery {
            q: "bug".to_string(),
            scope: None,
            open_issues: false,
        };
        let debug = format!("{:?}", q.to_query());
        assert_eq!(debug, r#"Query({"issues": "1", "q": "bug"})"#);
    }

    #[test]
    fn to_query_translates_my_project_scope_to_the_plural_wire_value() {
        let q = SearchQuery {
            q: "bug".to_string(),
            scope: Some(SearchScope::MyProject),
            open_issues: true,
        };
        let debug = format!("{:?}", q.to_query());
        assert!(debug.contains(r#""scope": "my_projects""#), "{debug}");
        assert!(!debug.contains("\"my_project\""), "{debug}");
        assert!(debug.contains(r#""open_issues": "1""#), "{debug}");
    }

    #[test]
    fn entire_search_query_sends_one_flag_per_requested_resource() {
        let q = EntireSearchQuery {
            q: "bug".to_string(),
            resources: vec![SearchResource::Issues, SearchResource::WikiPages],
        };
        let debug = format!("{:?}", q.to_query());
        assert!(debug.contains(r#""issues": "1""#), "{debug}");
        assert!(debug.contains(r#""wiki_pages": "1""#), "{debug}");
    }

    #[test]
    fn entire_search_query_sends_only_the_one_requested_resource() {
        let q = EntireSearchQuery {
            q: "bug".to_string(),
            resources: vec![SearchResource::WikiPages],
        };
        let debug = format!("{:?}", q.to_query());
        assert!(debug.contains(r#""wiki_pages": "1""#), "{debug}");
        assert!(!debug.contains("\"issues\""), "{debug}");
    }

    #[test]
    fn search_resource_from_raw_type_maps_redmines_dasherized_wiki_page_type() {
        assert_eq!(
            SearchResource::from_raw_type("issue"),
            Some(SearchResource::Issues)
        );
        assert_eq!(
            SearchResource::from_raw_type("wiki-page"),
            Some(SearchResource::WikiPages)
        );
        assert_eq!(SearchResource::from_raw_type("news"), None);
    }

    #[test]
    fn round_trips_a_search_envelope() {
        let json = r#"{
            "results": [
                {"id": 1, "title": "Bug ##1", "type": "issue", "url": "https://x/issues/1",
                 "description": "excerpt", "datetime": "2026-01-01T00:00:00Z"}
            ],
            "total_count": 1, "offset": 0, "limit": 25
        }"#;
        let env: SearchResultsEnvelope = serde_json::from_str(json).expect("should parse");
        assert_eq!(env.results.len(), 1);
        assert_eq!(env.results.first().expect("one result").kind, "issue");
    }

    #[test]
    fn unknown_field_does_not_fail_parsing() {
        let json = r#"{
            "results": [], "total_count": 0, "offset": 0, "limit": 25,
            "a_future_field": true
        }"#;
        let env: SearchResultsEnvelope = serde_json::from_str(json).expect("should parse");
        assert_eq!(env.results.len(), 0);
    }
}
