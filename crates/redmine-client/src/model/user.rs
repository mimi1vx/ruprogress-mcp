//! `GET /users`, `GET /my/account.json`.

use chrono::{DateTime, Utc};
use serde::Deserialize;

use super::{Collection, CustomField, permissive_datetime, permissive_datetime_opt};

/// A Redmine user.
#[non_exhaustive]
#[derive(Debug, Clone, Deserialize)]
pub struct User {
    /// The user id.
    pub id: u64,
    /// The login name. Omitted from some responses depending on permissions.
    #[serde(default)]
    pub login: Option<String>,
    /// First name.
    pub firstname: String,
    /// Last name.
    pub lastname: String,
    /// Email address. Permission-gated: only visible to admins and the user
    /// themselves.
    #[serde(default)]
    pub mail: Option<String>,
    /// When the account was created.
    #[serde(deserialize_with = "permissive_datetime")]
    pub created_on: DateTime<Utc>,
    /// The user's last login time, if they have logged in.
    #[serde(default, deserialize_with = "permissive_datetime_opt")]
    pub last_login_on: Option<DateTime<Utc>>,
    /// Custom field values attached to this user.
    #[serde(default)]
    pub custom_fields: Option<Vec<CustomField>>,
    /// Whether this user is a Redmine administrator. Only present on
    /// `/my/account.json` and admin-only responses.
    #[serde(default)]
    pub admin: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct UserEnvelope {
    pub user: User,
}

/// Filter parameters for `GET /users.json`.
#[derive(Debug, Default, Clone)]
pub struct UserQuery {
    /// Filter by name: matches login, firstname, lastname, or a
    /// `"firstname lastname"` pair.
    pub name: Option<String>,
    /// Restrict to members of this group.
    pub group_id: Option<u64>,
    /// Filter by account status (1 = active, 2 = registered, 3 = locked).
    pub status: Option<u8>,
}

impl UserQuery {
    /// Convert to the query-parameter map sent on the wire.
    #[must_use]
    pub fn to_query(&self) -> crate::client::Query {
        let mut q = crate::client::Query::default();
        if let Some(name) = &self.name {
            q.insert("name", name.clone());
        }
        if let Some(group_id) = self.group_id {
            q.insert("group_id", group_id.to_string());
        }
        if let Some(status) = self.status {
            q.insert("status", status.to_string());
        }
        q
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct UsersEnvelope {
    users: Vec<User>,
    total_count: u64,
    offset: u64,
    limit: u32,
}

impl Collection for UsersEnvelope {
    type Item = User;

    fn total_count(&self) -> u64 {
        self.total_count
    }

    fn offset(&self) -> u64 {
        self.offset
    }

    fn limit(&self) -> u32 {
        self.limit
    }

    fn into_items(self) -> Vec<User> {
        self.users
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    const FIXTURE_6_1: &str = include_str!("../../tests/fixtures/user_6_1.json");
    const FIXTURE_7_0: &str = include_str!("../../tests/fixtures/user_7_0.json");

    #[test]
    fn round_trips_against_6_1_fixture() {
        let env: UserEnvelope =
            serde_json::from_str(FIXTURE_6_1).expect("6.1 fixture should parse");
        assert_eq!(env.user.login.as_deref(), Some("alice"));
    }

    #[test]
    fn round_trips_against_7_0_fixture() {
        let env: UserEnvelope =
            serde_json::from_str(FIXTURE_7_0).expect("7.0 fixture should parse");
        assert_eq!(env.user.login.as_deref(), Some("alice"));
    }

    #[test]
    fn admin_field_defaults_to_none_when_absent() {
        let env: UserEnvelope =
            serde_json::from_str(FIXTURE_6_1).expect("6.1 fixture should parse");
        assert_eq!(env.user.admin, None);
    }

    const FIXTURE_USERS_LIST_6_1: &str = include_str!("../../tests/fixtures/users_list_6_1.json");
    const FIXTURE_USERS_LIST_7_0: &str = include_str!("../../tests/fixtures/users_list_7_0.json");

    #[test]
    fn users_envelope_round_trips_against_6_1_fixture() {
        let env: UsersEnvelope =
            serde_json::from_str(FIXTURE_USERS_LIST_6_1).expect("6.1 fixture should parse");
        assert_eq!(env.users.len(), 1);
        assert_eq!(env.total_count, 1);
    }

    #[test]
    fn users_envelope_round_trips_against_7_0_fixture() {
        let env: UsersEnvelope =
            serde_json::from_str(FIXTURE_USERS_LIST_7_0).expect("7.0 fixture should parse");
        assert_eq!(env.users.first().unwrap().admin, Some(true));
    }

    #[test]
    fn user_query_to_query_omits_unset_fields() {
        let q = UserQuery::default();
        assert_eq!(format!("{:?}", q.to_query()), "Query({})");
    }

    #[test]
    fn user_query_to_query_round_trips_special_characters() {
        let q = UserQuery {
            name: Some("Ale& Ünïcode".to_string()),
            group_id: Some(7),
            status: None,
        };
        let debug = format!("{:?}", q.to_query());
        assert!(debug.contains(r#""name": "Ale& Ünïcode""#), "{debug}");
        assert!(debug.contains(r#""group_id": "7""#), "{debug}");
        assert!(!debug.contains("status"), "{debug}");
    }
}
