//! Anti-drift tests for per-tool OAuth scope enforcement: every registered
//! tool has a `TOOL_SCOPES` entry, every map key
//! is a registered route or explicitly deferred, every scope the map
//! enforces is advertised, a read-only deployment leaves every surviving
//! tool reachable with `READ_SCOPES` alone, and `admin` is never a
//! requirement. These assertions were each hand-verified to fail when the
//! map was deliberately broken (a scope removed from one `TOOL_SCOPES`
//! entry, an `admin` requirement added), then reverted.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

mod support;

use std::collections::BTreeSet;

use ruprogress_mcp::auth::scope::{
    ADMIN_SCOPE, NOT_YET_IMPLEMENTED, ScopeRule, TOOL_SCOPES, visible_for,
};
use serde_json::Value;

const CLIENT_ID: &str = "introspect-client";
const CLIENT_SECRET: &str = "introspect-secret";

fn oauth_env(extra: &[(&'static str, &'static str)]) -> Vec<(&'static str, &'static str)> {
    let mut env = vec![
        ("REDMINE_AUTH_MODE", "oauth"),
        ("REDMINE_MCP_BASE_URL", "http://localhost:3040"),
        ("REDMINE_INTROSPECT_CLIENT_ID", CLIENT_ID),
        ("REDMINE_INTROSPECT_CLIENT_SECRET", CLIENT_SECRET),
    ];
    env.extend_from_slice(extra);
    env
}

fn raw_client() -> reqwest::Client {
    reqwest::Client::builder()
        .build()
        .expect("build a test HTTP client")
}

/// The `scopes_supported` a discovery document advertises for `harness`'s
/// configuration — the same field `oauth_discovery.rs` exercises, reused
/// here rather than reaching into `oauth::scopes::advertised` directly
/// (that module stays crate-private).
async fn scopes_supported(harness: &support::HttpHarness) -> BTreeSet<String> {
    let doc: Value = raw_client()
        .get(harness.url("/.well-known/oauth-protected-resource/mcp"))
        .send()
        .await
        .expect("request should complete")
        .json()
        .await
        .expect("json body");
    doc["scopes_supported"]
        .as_array()
        .expect("scopes_supported should be an array")
        .iter()
        .map(|v| v.as_str().expect("scope entries are strings").to_string())
        .collect()
}

/// Every scope required anywhere in `rule`, applied via `f`.
fn for_each_scope(rule: &ScopeRule, mut f: impl FnMut(&'static str)) {
    match rule {
        ScopeRule::Fixed(scopes) | ScopeRule::AnyOf(scopes) => {
            for scope in *scopes {
                f(scope);
            }
        }
        ScopeRule::PerAction(actions) => {
            for (_, scopes) in *actions {
                for scope in *scopes {
                    f(scope);
                }
            }
        }
    }
}

#[tokio::test]
async fn every_registered_tool_in_a_fully_enabled_router_has_a_tool_scopes_entry() {
    let h = support::harness(&[
        ("REDMINE_MCP_EXPOSE_ADMIN_TOOLS", "true"),
        ("REDMINE_CHECKLISTS_ENABLED", "true"),
        ("REDMINE_PRODUCTS_ENABLED", "true"),
        ("REDMINE_CRM_ENABLED", "true"),
    ])
    .await;
    let tools = h
        .client
        .list_tools(None)
        .await
        .expect("list_tools should succeed");
    for tool in &tools.tools {
        assert!(
            TOOL_SCOPES
                .iter()
                .any(|(name, _)| *name == tool.name.as_ref()),
            "{} is a registered route with no TOOL_SCOPES entry",
            tool.name
        );
    }
}

#[tokio::test]
async fn every_tool_scopes_key_is_a_registered_route_or_deferred() {
    let h = support::harness(&[
        ("REDMINE_MCP_EXPOSE_ADMIN_TOOLS", "true"),
        ("REDMINE_CHECKLISTS_ENABLED", "true"),
        ("REDMINE_PRODUCTS_ENABLED", "true"),
        ("REDMINE_CRM_ENABLED", "true"),
    ])
    .await;
    let tools = h
        .client
        .list_tools(None)
        .await
        .expect("list_tools should succeed");
    let registered: BTreeSet<&str> = tools.tools.iter().map(|t| t.name.as_ref()).collect();
    for (name, _) in TOOL_SCOPES {
        assert!(
            registered.contains(name) || NOT_YET_IMPLEMENTED.contains(name),
            "{name} is a TOOL_SCOPES entry that is neither a registered route nor listed in \
             NOT_YET_IMPLEMENTED"
        );
    }
}

#[tokio::test]
async fn every_scope_the_map_enforces_is_advertised() {
    let harness = support::http_harness(&oauth_env(&[
        ("REDMINE_AGILE_ENABLED", "true"),
        ("REDMINE_TAGS_ENABLED", "true"),
        ("REDMINE_MCP_EXPOSE_ADMIN_TOOLS", "true"),
    ]))
    .await;
    let advertised = scopes_supported(&harness).await;

    for (name, rule) in TOOL_SCOPES {
        for_each_scope(rule, |scope| {
            assert!(
                advertised.contains(scope),
                "{name} requires {scope}, which is never advertised in a fully-enabled \
                 deployment"
            );
        });
    }
    // `update_redmine_issue`/`create_redmine_issue`'s special-cased
    // requirements (S5, T7) live in code, not in `TOOL_SCOPES`, so they are
    // checked by name here.
    for scope in [
        "edit_issues",
        "add_issue_notes",
        "manage_subtasks",
        "view_agile_queries",
        "create_issue_tags",
        "edit_issue_tags",
    ] {
        assert!(
            advertised.contains(scope),
            "update_redmine_issue/create_redmine_issue's special-cased requirement {scope} is \
             never advertised"
        );
    }
}

#[tokio::test]
async fn read_only_router_keeps_every_surviving_tool_reachable_with_read_scopes_alone() {
    let read_only = support::harness(&[("REDMINE_MCP_READ_ONLY", "true")]).await;
    let tools = read_only
        .client
        .list_tools(None)
        .await
        .expect("list_tools should succeed");

    let scopes_harness =
        support::http_harness(&oauth_env(&[("REDMINE_MCP_READ_ONLY", "true")])).await;
    let read_scopes = scopes_supported(&scopes_harness).await;

    for tool in &tools.tools {
        assert!(
            visible_for(&tool.name, &read_scopes),
            "{} survives read-only mode but is not reachable with READ_SCOPES alone \
             ({read_scopes:?})",
            tool.name
        );
    }
}

#[test]
fn admin_appears_in_no_tool_scopes_entry() {
    for (name, rule) in TOOL_SCOPES {
        for_each_scope(rule, |scope| {
            assert_ne!(
                scope, ADMIN_SCOPE,
                "{name} names \"admin\" as a requirement; admin is a bypass, not a scope to hold"
            );
        });
    }
}
