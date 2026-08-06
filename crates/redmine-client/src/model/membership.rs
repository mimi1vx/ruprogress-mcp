//! `GET /projects/{id}/memberships`.

use serde::Deserialize;

use super::IdName;

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
#[allow(
    dead_code,
    reason = "model exists for round-trip tests; no API method uses it yet"
)]
pub(crate) struct MembershipEnvelope {
    pub membership: Membership,
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
