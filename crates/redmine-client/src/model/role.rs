//! `GET /roles.json`.

use serde::Deserialize;

use super::BareCollection;

/// A Redmine role. The `/roles.json` list endpoint only ever returns `id`
/// and `name` — the fuller per-role permission set is only available from
/// `GET /roles/{id}.json`, which no tool needs yet.
#[non_exhaustive]
#[derive(Debug, Clone, Deserialize)]
pub struct Role {
    /// The role id.
    pub id: u64,
    /// The role's display name.
    pub name: String,
}

/// `GET /roles.json` carries no pagination envelope at all — confirmed
/// against `roles/index.api.rsb`, which has no `api_meta` call.
#[derive(Debug, Deserialize)]
pub(crate) struct RolesEnvelope {
    roles: Vec<Role>,
}

impl BareCollection for RolesEnvelope {
    type Item = Role;

    fn into_items(self) -> Vec<Role> {
        self.roles
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        let json = r#"{"roles": [{"id": 3, "name": "Manager"}]}"#;
        let env: RolesEnvelope = serde_json::from_str(json).expect("should parse");
        assert_eq!(env.roles.first().unwrap().name, "Manager");
    }

    #[test]
    fn unknown_field_does_not_fail() {
        let json = r#"{"roles": [{"id": 3, "name": "Manager", "assignable": true}]}"#;
        let env: RolesEnvelope = serde_json::from_str(json).expect("unknown field must not fail");
        assert_eq!(env.roles.len(), 1);
    }
}
