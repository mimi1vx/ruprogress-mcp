//! Read-only mode: the list of tools to hide when `REDMINE_MCP_READ_ONLY` is
//! set, plus `RedmineMcp::new` removing them from the router (see
//! `server.rs`). `ToolRouter::remove_route` hides a name from `tools/list`
//! **and** makes `tools/call` fail with "tool not found" — one choke point.

pub mod write_tools {
    /// Every tool that mutates Redmine. Removed from the router in read-only
    /// mode. The tests in `tests/readonly.rs` turn a stale or missing name
    /// here into a build/test failure rather than a silent read-only-mode
    /// bypass.
    ///
    /// The 4a discovery-tool sub-phase deliberately added nothing here: all
    /// seven of its tools (including `list_redmine_users`, which merely
    /// *requires* an admin credential) are reads.
    ///
    /// 4c adds `manage_redmine_version` and `manage_project_member` in full
    /// — not per-action, unlike the parent plan's general `manage_*` gating
    /// (D8). Both tools' `action` enums are exclusively mutating
    /// (`create`/`update`/`delete` and `add`/`update`/`remove`); listing
    /// versions/members is a separate tool for each
    /// (`list_redmine_versions`/`list_project_members`), so there is no read
    /// action inside either `manage_*` tool to preserve. See
    /// `plans/phase-4c-projects.md` decision F1.
    ///
    /// 4b-read added nothing here (all five of its tools are reads).
    ///
    /// 4d adds `manage_time_entry` (action enum is `create`/`update` only —
    /// same "no read action to preserve" reasoning as F1) and
    /// `import_time_entries` (always writes when called, by construction).
    /// See `plans/phase-4d-time.md` decisions H5/H6.
    pub const ALL: &[&str] = &[
        "manage_redmine_version",
        "manage_project_member",
        "manage_time_entry",
        "import_time_entries",
    ];
}
