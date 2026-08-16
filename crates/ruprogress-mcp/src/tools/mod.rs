//! Tool implementations, one module per area. Each module owns a
//! `#[tool_router(router = ..., vis = "pub(crate)")]` block on `RedmineMcp`;
//! `server.rs` merges them into the router served to clients.

pub(crate) mod custom_fields;
pub(crate) mod discovery;
pub(crate) mod files;
pub(crate) mod gantt;
pub(crate) mod issues;
pub(crate) mod meta;
pub(crate) mod output;
pub(crate) mod plugins;
pub(crate) mod projects;
pub(crate) mod schema;
pub(crate) mod search_wiki;
pub(crate) mod time;
