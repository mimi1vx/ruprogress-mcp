//! Tool implementations, one module per area. Each module owns a
//! `#[tool_router(router = ..., vis = "pub(crate)")]` block on `RedmineMcp`;
//! `server.rs` merges them into the router served to clients.

pub(crate) mod custom_fields;
pub(crate) mod discovery;
pub(crate) mod files;
pub(crate) mod gantt;
pub(crate) mod issues;
pub(crate) mod meta;
// `#[doc(hidden)] pub`, not `pub(crate)`: `benches/output_caps.rs` needs
// `output::apply_caps_bench`, and `unreachable_pub` requires the containing
// module to be `pub` too for a `pub` item inside it to be reachable.
#[doc(hidden)]
pub mod output;
pub(crate) mod plugins;
pub(crate) mod projects;
pub(crate) mod schema;
pub(crate) mod search_wiki;
pub(crate) mod time;
