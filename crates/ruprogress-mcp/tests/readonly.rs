//! Read-only mode. `write_tools::ALL` is empty for now — only read-only
//! tools exist so far — so (i) and (ii) below are currently vacuous, but
//! they turn a future stale/missing name in `ALL` into a build/test failure
//! the moment it gets populated, rather than a silent read-only-mode
//! bypass. (iii) proves the router mechanism itself
//! (`ToolRouter::remove_route`/`call`) returns a clean error rather than
//! panicking, using a tool name from `docs/tool-contract.md` that is not
//! registered yet — behaviourally identical to a route read-only mode will
//! eventually remove.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use rmcp::model::CallToolRequestParams;
use ruprogress_mcp::readonly::write_tools;

#[tokio::test]
async fn every_write_tool_name_exists_in_a_normal_router() {
    let h = support::harness(&[]).await;
    let tools = h
        .client
        .list_tools(None)
        .await
        .expect("list_tools should succeed");
    let names: Vec<&str> = tools.tools.iter().map(|t| t.name.as_ref()).collect();
    for name in write_tools::ALL {
        assert!(
            names.contains(name),
            "{name} in write_tools::ALL but missing from the router"
        );
    }
}

#[tokio::test]
async fn no_write_tool_name_exists_in_a_read_only_router() {
    let h = support::harness(&[("REDMINE_MCP_READ_ONLY", "true")]).await;
    let tools = h
        .client
        .list_tools(None)
        .await
        .expect("list_tools should succeed");
    let names: Vec<&str> = tools.tools.iter().map(|t| t.name.as_ref()).collect();
    for name in write_tools::ALL {
        assert!(
            !names.contains(name),
            "{name} still present in a read-only router"
        );
    }
}

#[tokio::test]
async fn calling_an_unregistered_tool_returns_a_clean_error_not_a_panic() {
    let h = support::harness(&[]).await;
    // `delete_redmine_issue` is a real future write-tool name (see
    // docs/tool-contract.md) that does not exist in the router yet — the
    // router's response to it is identical to what it will return for a
    // route removed by read-only mode.
    let result = h
        .client
        .call_tool(CallToolRequestParams::new("delete_redmine_issue"))
        .await;
    assert!(
        result.is_err(),
        "calling an unknown tool should fail cleanly, not succeed"
    );
}
