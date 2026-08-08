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
    /// `manage_redmine_version` and `manage_project_member` are listed here
    /// in full — not gated per action like `manage_issue_relation`/
    /// `manage_issue_category`/`manage_redmine_wiki_page` below. Both tools'
    /// `action` enums are exclusively mutating (`create`/`update`/`delete`
    /// and `add`/`update`/`remove`); listing versions/members is a separate
    /// tool for each (`list_redmine_versions`/`list_project_members`), so
    /// there is no read action inside either `manage_*` tool to preserve.
    /// The same reasoning applies to `manage_time_entry` (`action` is
    /// `create`/`update` only) and to `manage_issue_watcher`/
    /// `manage_issue_note` (both `action` enums are exclusively mutating).
    ///
    /// `import_time_entries` always writes when called, by construction, as
    /// do `create_redmine_issue`, `update_redmine_issue`,
    /// `delete_redmine_issue`, and `copy_issue`.
    ///
    /// `manage_issue_relation` and `manage_issue_category` are deliberately
    /// **not** here: both have a genuine `action="list"` that must survive
    /// read-only mode (read-only mode gates per action, not per tool) — see
    /// [`PARTIAL_WRITE`] instead.
    ///
    /// `delete_file` always mutates when called; `list_files` is not here —
    /// it is a read, matching `get_redmine_attachment`. `upload_file` always
    /// mutates Redmine when called; `uploads[]` on `create_redmine_issue`/
    /// `update_redmine_issue` needs no entry of its own, since both tools
    /// are already unconditionally in this list.
    ///
    /// `cleanup_attachment_files` is deliberately **not** here: it mutates
    /// only the local attachment store, never Redmine, so read-only mode
    /// does not gate it — it is instead removed from the router entirely
    /// unless `REDMINE_MCP_EXPOSE_ADMIN_TOOLS=true` (see `server.rs`).
    ///
    /// Every tool not mentioned above (discovery, issue reads, time entry
    /// reads, wiki reads outside `manage_redmine_wiki_page`) is a read,
    /// including `list_redmine_users`, which merely *requires* an admin
    /// credential rather than mutating anything.
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
        "upload_file",
    ];

    /// Tools with a mix of read and write `action`s: never removed from
    /// the router by read-only mode (unlike [`ALL`]), but the write actions
    /// inside them check `config.read_only` themselves and refuse with
    /// `code: "READ_ONLY"`. Both declare `read_only_hint = false` /
    /// `destructive_hint = true` in their tool annotations, same as an
    /// `ALL` tool — the annotation describes what the tool *can* do, not
    /// what read-only mode currently allows.
    ///
    /// `manage_redmine_wiki_page`: `list`/`get` are reads,
    /// `create`/`update`/`delete`/`rename` are writes.
    pub const PARTIAL_WRITE: &[&str] = &[
        "manage_issue_relation",
        "manage_issue_category",
        "manage_redmine_wiki_page",
    ];
}
