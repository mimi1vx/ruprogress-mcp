//! Read-only mode: the list of tools to hide when `REDMINE_MCP_READ_ONLY` is
//! set, plus `RedmineMcp::new` removing them from the router (see
//! `server.rs`). `ToolRouter::remove_route` hides a name from `tools/list`
//! **and** makes `tools/call` fail with "tool not found" — one choke point.

pub mod write_tools {
    /// Every tool that mutates Redmine. Removed from the router in read-only
    /// mode. Empty for now: only read-only tools exist so far — this gets
    /// populated as write tools land, and the tests in `tests/readonly.rs`
    /// turn a stale or missing name here into a build/test failure rather
    /// than a silent read-only-mode bypass.
    pub const ALL: &[&str] = &[];
}
