//! Tool implementations, one module per area. Each module owns a
//! `#[tool_router(router = ..., vis = "pub(crate)")]` block on `RedmineMcp`;
//! `server.rs` merges them into the router served to clients.

pub(crate) mod discovery;
pub(crate) mod meta;
pub(crate) mod output;
pub(crate) mod projects;
pub(crate) mod schema;
