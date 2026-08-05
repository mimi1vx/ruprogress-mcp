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
}

#[derive(Debug, Deserialize)]
pub(crate) struct UserEnvelope {
    pub user: User,
}

#[derive(Debug, Deserialize)]
#[allow(
    dead_code,
    reason = "model exists for round-trip tests; no phase-1 API method uses it yet"
)]
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
}
