//! `GET/POST /projects/{id}/issue_categories`, `GET/PUT/DELETE
//! /issue_categories/{id}`.

use serde::{Deserialize, Serialize};

use super::IdName;
use crate::ids::UserId;

/// An issue category.
#[non_exhaustive]
#[derive(Debug, Clone, Deserialize)]
pub struct IssueCategory {
    /// The category id.
    pub id: u64,
    /// The owning project. Omitted (not merely `null`) by
    /// `issue_categories/index.api.rsb` when nil — modelled the same way as
    /// every other optional association in this client.
    #[serde(default)]
    pub project: Option<IdName>,
    /// The category name.
    pub name: String,
    /// The default assignee for issues in this category, if any.
    #[serde(default)]
    pub assigned_to: Option<IdName>,
}

/// Payload for `POST /projects/{id}/issue_categories.json`. `name` and
/// `assigned_to_id` are the only two attributes `IssueCategory` accepts
/// (`safe_attributes` in `app/models/issue_category.rb`).
#[derive(Debug, Clone, Serialize)]
pub struct IssueCategoryCreate {
    /// The category name. Required, must not be blank.
    pub name: String,
    /// The default assignee for issues in this category.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assigned_to_id: Option<UserId>,
}

/// Payload for `PUT /issue_categories/{id}.json`. Both fields optional:
/// only those set are changed.
#[derive(Debug, Clone, Default, Serialize)]
pub struct IssueCategoryUpdate {
    /// New name, if changing it. Must not be blank if given.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// New default assignee, if changing it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assigned_to_id: Option<UserId>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct IssueCategoryEnvelope {
    pub issue_category: IssueCategory,
}

#[derive(Debug, Serialize)]
pub(crate) struct IssueCategoryCreateEnvelope<'a> {
    pub issue_category: &'a IssueCategoryCreate,
}

#[derive(Debug, Serialize)]
pub(crate) struct IssueCategoryUpdateEnvelope<'a> {
    pub issue_category: &'a IssueCategoryUpdate,
}

/// `GET /projects/{id}/issue_categories.json` carries a `total_count` but no
/// `offset`/`limit` (`api_meta(:total_count => @categories.size)` over an
/// unconditionally-loaded `@project.issue_categories.to_a`) — deliberately
/// not a [`super::Collection`] or [`super::BareCollection`] impl; see
/// `Scoped::list_issue_categories`, which reads this envelope directly and
/// ignores `total_count` rather than mis-modelling this as either trait.
#[derive(Debug, Deserialize)]
pub(crate) struct IssueCategoriesEnvelope {
    issue_categories: Vec<IssueCategory>,
}

impl IssueCategoriesEnvelope {
    pub(crate) fn into_items(self) -> Vec<IssueCategory> {
        self.issue_categories
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn category_round_trips_with_project_and_assignee() {
        let json = r#"{"issue_category": {
            "id": 2, "project": {"id": 1, "name": "P"}, "name": "Backend",
            "assigned_to": {"id": 3, "name": "Alice"}
        }}"#;
        let env: IssueCategoryEnvelope = serde_json::from_str(json).expect("should parse");
        assert_eq!(env.issue_category.name, "Backend");
        assert_eq!(env.issue_category.assigned_to.unwrap().id, 3);
    }

    #[test]
    fn category_round_trips_without_project_or_assignee() {
        let json = r#"{"issue_category": {"id": 2, "name": "Backend"}}"#;
        let env: IssueCategoryEnvelope = serde_json::from_str(json).expect("should parse");
        assert!(env.issue_category.project.is_none());
        assert!(env.issue_category.assigned_to.is_none());
    }

    #[test]
    fn categories_envelope_ignores_total_count() {
        let json = r#"{"issue_categories": [{"id": 1, "name": "A"}], "total_count": 1}"#;
        let env: IssueCategoriesEnvelope = serde_json::from_str(json).expect("should parse");
        assert_eq!(env.into_items().len(), 1);
    }

    #[test]
    fn create_serializes_only_set_fields() {
        let create = IssueCategoryCreate {
            name: "Backend".to_string(),
            assigned_to_id: None,
        };
        let value = serde_json::to_value(IssueCategoryCreateEnvelope {
            issue_category: &create,
        })
        .unwrap();
        let obj = value
            .get("issue_category")
            .and_then(serde_json::Value::as_object)
            .unwrap();
        assert_eq!(obj["name"], "Backend");
        assert!(!obj.contains_key("assigned_to_id"));
    }
}
