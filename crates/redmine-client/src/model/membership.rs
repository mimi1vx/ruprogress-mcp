//! `GET/POST /projects/{id}/memberships`, `GET/PUT/DELETE /memberships/{id}`.

use serde::{Deserialize, Serialize};

use super::{Collection, IdName};

/// A project membership: a user or group, and the roles they hold on a
/// project.
#[non_exhaustive]
#[derive(Debug, Clone, Deserialize)]
pub struct Membership {
    /// The membership id.
    pub id: u64,
    /// The project this membership belongs to.
    #[serde(default)]
    pub project: Option<IdName>,
    /// The member, if this is a user membership.
    #[serde(default)]
    pub user: Option<IdName>,
    /// The member, if this is a group membership.
    #[serde(default)]
    pub group: Option<IdName>,
    /// Roles held.
    #[serde(default)]
    pub roles: Vec<IdName>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct MembershipEnvelope {
    pub membership: Membership,
}

/// `GET /projects/{id}/memberships.json` — genuinely paginated
/// (`MembersController#index` calls `api_offset_and_limit`), unlike
/// `list_redmine_versions`'s endpoint.
#[derive(Debug, Deserialize)]
pub(crate) struct MembershipsEnvelope {
    memberships: Vec<Membership>,
    total_count: u64,
    offset: u64,
    limit: u32,
}

impl Collection for MembershipsEnvelope {
    type Item = Membership;

    fn total_count(&self) -> u64 {
        self.total_count
    }

    fn offset(&self) -> u64 {
        self.offset
    }

    fn limit(&self) -> u32 {
        self.limit
    }

    fn into_items(self) -> Vec<Membership> {
        self.memberships
    }
}

/// The `membership` hash sent to `POST /projects/{id}/memberships.json`.
/// Redmine's API accepts a group id through the same `user_id` field a user
/// id goes through — there is no separate `group_id` wire field.
#[derive(Debug, Clone, Serialize)]
pub struct MembershipCreate {
    /// The user or group id.
    pub user_id: u64,
    /// Non-empty list of role ids.
    pub role_ids: Vec<u64>,
}

/// The `membership` hash sent to `PUT /memberships/{id}.json`. Only roles
/// can be changed; the project and principal are read-only after creation.
#[derive(Debug, Clone, Serialize)]
pub struct MembershipUpdate {
    /// Non-empty list of role ids.
    pub role_ids: Vec<u64>,
}

#[derive(Debug, Serialize)]
pub(crate) struct MembershipCreateEnvelope<'a> {
    pub membership: &'a MembershipCreate,
}

#[derive(Debug, Serialize)]
pub(crate) struct MembershipUpdateEnvelope<'a> {
    pub membership: &'a MembershipUpdate,
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    // Inline fixture: see tests/fixtures/README.md for the policy that
    // applies to models with a real API method.
    const JSON: &str = r#"{"membership": {
        "id": 1, "project": {"id": 1, "name": "Example"}, "user": {"id": 2, "name": "Alice"},
        "roles": [{"id": 3, "name": "Manager"}]
    }}"#;

    #[test]
    fn round_trips() {
        let env: MembershipEnvelope = serde_json::from_str(JSON).expect("should parse");
        assert_eq!(env.membership.roles.first().unwrap().name, "Manager");
    }
}
