//! OAuth scope catalogue (D1): the single source of truth for the
//! `scopes_supported` list served by both discovery documents
//! (`oauth::metadata`).
//!
//! Scope identifiers are Redmine Doorkeeper scope names — in stock Redmine
//! 6.x they match the permission name from
//! `lib/redmine/access_control.rb`, which the per-scope comments below cite.
//!
//! `admin` is never advertised: tokens with admin scope bypass per-permission
//! checks, so default-advertising it would make every consent screen ask for
//! full administrative access. Vendor plugin scopes outside `agile`/`tags`
//! (Easy Redmine, `RedmineUP`, checklists, CRM, products, DMSF) are excluded
//! because they vary by deployment, and advertising a scope Redmine doesn't
//! recognize causes consent errors.

/// Redmine permissions used by the read-only MCP tools.
const READ_SCOPES: &[&str] = &[
    "view_project", // list_redmine_projects, summarize_project_status,
    // get_project_modules
    "search_project", // search_entire_redmine, search_redmine_issues
    "view_members",   // list_project_members
    "view_issues",    // list_redmine_issues, get_redmine_issue, list_subtasks,
    // get_gantt_chart, list_redmine_queries, list_redmine_versions,
    // summarize_project_status (issue queries).
    // Note: list_redmine_versions uses view_issues because Redmine gates
    // GET /projects/.../versions.json on view_issues, not manage_versions
    // (which is a write permission).
    "view_documents",      // manage_document(action=list|get)
    "view_files",          // get_redmine_attachment, list_files
    "view_wiki_pages",     // manage_redmine_wiki_page(action=get|list)
    "view_time_entries",   // list_time_entries, list_time_entry_activities
    "view_private_notes",  // get_private_notes
    "view_issue_watchers", // get_redmine_issue(include_watchers=true)
];

/// Redmine permissions used by the mutation MCP tools.
const WRITE_SCOPES: &[&str] = &[
    "add_issues",             // create_redmine_issue, copy_issue
    "edit_issues",            // update_redmine_issue
    "delete_issues",          // delete_redmine_issue
    "manage_subtasks",        // update_redmine_issue when parent_issue_id changes
    "manage_issue_relations", // manage_issue_relation
    "add_issue_watchers",     // manage_issue_watcher(action=add)
    "delete_issue_watchers",  // manage_issue_watcher(action=remove)
    "add_issue_notes",        // update_redmine_issue notes-only carve-out
    "edit_issue_notes",       // manage_issue_note(action=edit)
    "set_notes_private",      // manage_issue_note(action=set_private)
    "log_time",               // manage_time_entry(action=create), import_time_entries
    "edit_time_entries",      // manage_time_entry(action=update)
    "manage_versions",        // manage_redmine_version
    "manage_categories",      // manage_issue_category
    "manage_wiki",            // wiki administration; not required by any MCP tool today
    "edit_wiki_pages",        // manage_redmine_wiki_page(action=create|update)
    "rename_wiki_pages",      // manage_redmine_wiki_page(action=rename)
    "delete_wiki_pages",      // manage_redmine_wiki_page(action=delete)
    "add_documents",          // manage_document(action=create)
    "edit_documents",         // manage_document(action=update)
    "delete_documents",       // advertised for parity; manage_document has no
    // delete action yet
    "manage_files",   // upload_file, delete_file
    "manage_members", // manage_project_member
];

/// `RedmineUP` Agile plugin permissions, advertised only when the agile
/// feature is explicitly enabled (`REDMINE_AGILE_ENABLED`). Kept out of
/// `READ_SCOPES` so a non-agile deployment never advertises a scope Redmine
/// can't resolve.
const AGILE_READ_SCOPES: &[&str] = &[
    "view_agile_queries", // get_redmine_issue agile fetch: AgileBoardsController#agile_data
                          // (GET /issues/{id}/agile_data.json)
];

/// `AlphaNodes` `additional_tags` plugin read permission, advertised only when
/// the tags feature is explicitly enabled (`REDMINE_TAGS_ENABLED`). Kept out
/// of `READ_SCOPES` so a deployment without the plugin never advertises a
/// scope Redmine can't resolve.
const TAGS_READ_SCOPES: &[&str] = &[
    "view_issue_tags", // get_redmine_issue tags array: the plugin injects
                       // `tags` into GET /issues/{id}.json only when the
                       // caller holds this permission.
];

/// `AlphaNodes` `additional_tags` write permissions, advertised only when the
/// tags feature is enabled AND the server is not read-only.
/// `create_redmine_issue`/`update_redmine_issue` accept a `tag_list`; the
/// plugin's `safe_attributes` gate requires `create_issue_tags` (may add new
/// tags) or `edit_issue_tags` (existing tags only), so both are advertised
/// to cover either grant.
const TAGS_WRITE_SCOPES: &[&str] = &["create_issue_tags", "edit_issue_tags"];

/// Returns the OAuth scopes to advertise in discovery documents, before
/// `REDMINE_MCP_SCOPES` narrowing (D1).
///
/// Returns `READ_SCOPES` only when `read_only` is set; otherwise
/// `READ_SCOPES + WRITE_SCOPES`. When `agile` is set, the read-only
/// `AGILE_READ_SCOPES` are appended in both modes so an OAuth token can
/// reach the agile endpoints — gating on the same flag that gates the agile
/// tools means a non-agile Redmine never sees an unrecognized plugin scope.
/// The same applies to `TAGS_READ_SCOPES` under `tags`; `TAGS_WRITE_SCOPES`
/// are additionally appended unless read-only, since they gate `tag_list`
/// writes.
pub(crate) fn advertised(read_only: bool, agile: bool, tags: bool) -> Vec<&'static str> {
    let mut scopes: Vec<&'static str> = READ_SCOPES.to_vec();
    if !read_only {
        scopes.extend_from_slice(WRITE_SCOPES);
    }
    if agile {
        scopes.extend_from_slice(AGILE_READ_SCOPES);
    }
    if tags {
        scopes.extend_from_slice(TAGS_READ_SCOPES);
        if !read_only {
            scopes.extend_from_slice(TAGS_WRITE_SCOPES);
        }
    }
    scopes
}

/// Narrows `full` (the current mode's [`advertised`] set) to the entries
/// named by `raw` (`REDMINE_MCP_SCOPES`, whitespace-separated) (D2).
///
/// Every requested scope must be a member of `full`; an out-of-set or
/// unknown entry is an `Err` naming the offending scope(s) and the full
/// accepted set, so a deployment fails fast at boot rather than advertising
/// a scope its own tools cannot use. The result preserves `full`'s ordering
/// and is duplicate-free, so both discovery documents stay identical and
/// deterministic regardless of the order scopes were listed in
/// `REDMINE_MCP_SCOPES`.
///
/// # Errors
///
/// Returns `Err` describing every requested scope not present in `full`,
/// plus the full accepted set.
pub(crate) fn narrow(full: &[&'static str], raw: &str) -> Result<Vec<&'static str>, String> {
    let requested: Vec<&str> = raw.split_whitespace().collect();
    let allowed: std::collections::BTreeSet<&str> = full.iter().copied().collect();

    let mut seen_invalid = std::collections::BTreeSet::new();
    let invalid: Vec<&str> = requested
        .iter()
        .copied()
        .filter(|s| !allowed.contains(s) && seen_invalid.insert(*s))
        .collect();
    if !invalid.is_empty() {
        return Err(format!(
            "contains scope(s) not advertised in this mode: {}. Allowed: {}",
            invalid.join(", "),
            full.join(", "),
        ));
    }

    let requested_set: std::collections::BTreeSet<&str> = requested.into_iter().collect();
    Ok(full
        .iter()
        .copied()
        .filter(|s| requested_set.contains(s))
        .collect())
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]
mod tests {
    use super::*;

    #[test]
    fn admin_is_never_advertised() {
        for (read_only, agile, tags) in [
            (false, false, false),
            (true, false, false),
            (false, true, false),
            (false, false, true),
            (true, true, true),
        ] {
            assert!(!advertised(read_only, agile, tags).contains(&"admin"));
        }
    }

    #[test]
    fn read_only_excludes_write_scopes() {
        let scopes = advertised(true, false, false);
        assert!(scopes.contains(&"view_issues"));
        assert!(!scopes.contains(&"edit_issues"));
    }

    #[test]
    fn writable_includes_write_scopes() {
        let scopes = advertised(false, false, false);
        assert!(scopes.contains(&"view_issues"));
        assert!(scopes.contains(&"edit_issues"));
    }

    #[test]
    fn agile_flag_appends_agile_read_scopes_in_both_modes() {
        assert!(advertised(true, true, false).contains(&"view_agile_queries"));
        assert!(advertised(false, true, false).contains(&"view_agile_queries"));
        assert!(!advertised(true, false, false).contains(&"view_agile_queries"));
    }

    #[test]
    fn tags_flag_appends_read_scope_always_and_write_scopes_only_when_writable() {
        let read_only_scopes = advertised(true, false, true);
        assert!(read_only_scopes.contains(&"view_issue_tags"));
        assert!(!read_only_scopes.contains(&"create_issue_tags"));
        assert!(!read_only_scopes.contains(&"edit_issue_tags"));

        let writable_scopes = advertised(false, false, true);
        assert!(writable_scopes.contains(&"view_issue_tags"));
        assert!(writable_scopes.contains(&"create_issue_tags"));
        assert!(writable_scopes.contains(&"edit_issue_tags"));
    }

    #[test]
    fn tags_and_agile_off_by_default() {
        let scopes = advertised(false, false, false);
        assert!(!scopes.contains(&"view_agile_queries"));
        assert!(!scopes.contains(&"view_issue_tags"));
    }

    #[test]
    fn narrow_preserves_advertised_order_regardless_of_request_order() {
        let full = advertised(false, false, false);
        let raw = "edit_issues view_project"; // reversed vs. full's order
        let narrowed = narrow(&full, raw).expect("both scopes are in the full set");
        let pos_view = narrowed.iter().position(|s| *s == "view_project").unwrap();
        let pos_edit = narrowed.iter().position(|s| *s == "edit_issues").unwrap();
        assert!(pos_view < pos_edit, "order should follow `full`, not `raw`");
    }

    #[test]
    fn narrow_deduplicates_repeated_entries() {
        let full = advertised(false, false, false);
        let narrowed = narrow(&full, "view_project view_project").expect("valid scope");
        assert_eq!(narrowed.iter().filter(|s| **s == "view_project").count(), 1);
    }

    #[test]
    fn narrow_rejects_an_out_of_set_scope_naming_the_accepted_set() {
        let full = advertised(true, false, false);
        let error = narrow(&full, "edit_issues").expect_err("edit_issues is write-only");
        assert!(error.contains("edit_issues"));
        assert!(
            error.contains("view_project"),
            "should list the allowed set: {error}"
        );
    }

    #[test]
    fn narrow_reports_each_invalid_scope_once() {
        let full = advertised(true, false, false);
        let error = narrow(&full, "bogus bogus").expect_err("bogus is not advertised");
        assert_eq!(error.matches("bogus").count(), 1);
    }
}
