//! The plugin-gate mechanism (`server.rs`'s `PLUGIN_TOOLS` removal loop):
//! flags-off leaves the router untouched, a flag on adds exactly its
//! family's tools, the flag interacts cleanly with read-only mode, and
//! `ToolRouter::remove_route` tolerates being asked to remove a name twice
//! (a plugin write tool, disabled, in a read-only deployment) without
//! panicking.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

mod support;

use rmcp::model::CallToolRequestParams;

const CHECKLIST_TOOLS: &[&str] = &[
    "get_checklist",
    "create_checklist_item",
    "update_checklist_item",
];

#[tokio::test]
async fn flags_off_the_router_carries_no_checklist_tools() {
    let h = support::harness(&[]).await;
    let tools = h
        .client
        .list_tools(None)
        .await
        .expect("list_tools should succeed");
    let names: Vec<&str> = tools.tools.iter().map(|t| t.name.as_ref()).collect();
    for name in CHECKLIST_TOOLS {
        assert!(
            !names.contains(name),
            "{name} present with REDMINE_CHECKLISTS_ENABLED unset"
        );
    }
}

#[tokio::test]
async fn checklists_flag_adds_exactly_the_three_checklist_tools() {
    let base = support::harness(&[]).await;
    let base_tools = base
        .client
        .list_tools(None)
        .await
        .expect("list_tools should succeed");
    let mut expected: Vec<&str> = base_tools.tools.iter().map(|t| t.name.as_ref()).collect();
    expected.extend_from_slice(CHECKLIST_TOOLS);
    expected.sort_unstable();

    let h = support::harness(&[("REDMINE_CHECKLISTS_ENABLED", "true")]).await;
    let tools = h
        .client
        .list_tools(None)
        .await
        .expect("list_tools should succeed");
    let mut names: Vec<&str> = tools.tools.iter().map(|t| t.name.as_ref()).collect();
    names.sort_unstable();

    assert_eq!(names, expected);
}

#[tokio::test]
async fn checklists_flag_plus_read_only_keeps_only_the_read_tool() {
    let h = support::harness(&[
        ("REDMINE_CHECKLISTS_ENABLED", "true"),
        ("REDMINE_MCP_READ_ONLY", "true"),
    ])
    .await;
    let tools = h
        .client
        .list_tools(None)
        .await
        .expect("list_tools should succeed");
    let names: Vec<&str> = tools.tools.iter().map(|t| t.name.as_ref()).collect();
    assert!(names.contains(&"get_checklist"));
    assert!(!names.contains(&"create_checklist_item"));
    assert!(!names.contains(&"update_checklist_item"));
}

#[tokio::test]
async fn calling_a_gated_tool_with_its_flag_off_fails_like_an_unknown_tool() {
    let h = support::harness(&[]).await;
    let result = h
        .client
        .call_tool(CallToolRequestParams::new("get_checklist"))
        .await;
    assert!(
        result.is_err(),
        "a plugin-gated tool with its flag off should fail cleanly, not succeed"
    );
}

/// With the checklists flag off *and* read-only mode on, both the plugin
/// gate loop and the read-only loop try to remove `create_checklist_item` —
/// proving `remove_route` on an already-absent name is a no-op rather than
/// a panic (the harness itself would fail to build otherwise).
#[tokio::test]
async fn a_tool_removed_by_both_the_plugin_gate_and_read_only_mode_is_still_just_absent() {
    let h = support::harness(&[("REDMINE_MCP_READ_ONLY", "true")]).await;
    let tools = h
        .client
        .list_tools(None)
        .await
        .expect("list_tools should succeed");
    let names: Vec<&str> = tools.tools.iter().map(|t| t.name.as_ref()).collect();
    assert!(!names.contains(&"create_checklist_item"));
}
