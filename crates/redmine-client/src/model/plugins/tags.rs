//! `AlphaNodes` `additional_tags`: the plugin injects a `tags` array into
//! `GET /issues/{id}.json` and accepts `tag_list` on `POST`/`PUT
//! /issues.json`.
//!
//! Synthetic models derived from the reference implementation's handling of
//! this plugin, not a live capture — see `tests/fixtures/README.md`'s
//! plugin fixtures section. Unlike the commercial `RedmineUP` plugins,
//! `additional_tags` is open-source, but the wire shape is still unverified
//! against a running instance.

use serde::Deserialize;

/// One tag on an issue. `id` is present only when the issue's tag set
/// exactly matches the plugin's own tag records and the caller holds
/// `view_issue_tags` — in practice frequently absent. `name` is the stable
/// identifier.
#[non_exhaustive]
#[derive(Debug, Clone, Deserialize)]
pub struct IssueTag {
    /// The plugin's internal tag id, when known.
    #[serde(default)]
    pub id: Option<u64>,
    /// The tag's name — the stable identifier for this tag.
    pub name: String,
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_a_mix_of_named_and_id_backed_tags() {
        let json = r#"[{"id":3,"name":"a"},{"name":"b"}]"#;
        let tags: Vec<IssueTag> = serde_json::from_str(json).expect("should parse");
        assert_eq!(tags.len(), 2);
        let with_id = tags.first().expect("first tag");
        assert_eq!(with_id.id, Some(3));
        assert_eq!(with_id.name, "a");
        let without_id = tags.get(1).expect("second tag");
        assert_eq!(without_id.id, None);
        assert_eq!(without_id.name, "b");
    }
}
