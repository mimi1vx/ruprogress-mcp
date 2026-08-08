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
    ///
    /// 4b-write adds `create_redmine_issue`, `update_redmine_issue`,
    /// `delete_redmine_issue`, `copy_issue` (each always mutates when
    /// called), and `manage_issue_watcher`/`manage_issue_note` (both
    /// `action` enums are exclusively mutating — same "no read action to
    /// preserve" reasoning as F1/H5). `manage_issue_relation` and
    /// `manage_issue_category` are deliberately **not** here: both have a
    /// genuine `action="list"` that must survive read-only mode per the
    /// parent plan's D8 ("read-only mode gates per action, not per tool") —
    /// see [`PARTIAL_WRITE`] instead.
    ///
    /// 5d adds `delete_file` (always mutates when called). `list_files` is
    /// not here: it is a read, matching `get_redmine_attachment` (5c).
    pub const ALL: &[&str] = &[
        "manage_redmine_version",
        "manage_project_member",
        "manage_time_entry",
        "import_time_entries",
        "create_redmine_issue",
        "update_redmine_issue",
        "delete_redmine_issue",
        "copy_issue",
        "manage_issue_watcher",
        "manage_issue_note",
        "delete_file",
    ];

    /// Tools with a mix of read and write `action`s (D8): never removed from
    /// the router by read-only mode (unlike [`ALL`]), but the write actions
    /// inside them check `config.read_only` themselves and refuse with
    /// `code: "READ_ONLY"`. Both declare `read_only_hint = false` /
    /// `destructive_hint = true` in their tool annotations, same as an
    /// `ALL` tool — the annotation describes what the tool *can* do, not
    /// what read-only mode currently allows.
    ///
    /// 4e adds `manage_redmine_wiki_page`: `list`/`get` are reads,
    /// `create`/`update`/`delete`/`rename` are writes — matching the
    /// reference contract's own documented read-only behavior for this
    /// tool exactly. See `plans/phase-4e-search-wiki.md` decision I12.
    pub const PARTIAL_WRITE: &[&str] = &[
        "manage_issue_relation",
        "manage_issue_category",
        "manage_redmine_wiki_page",
    ];
}
