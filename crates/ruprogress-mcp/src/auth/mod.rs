//! Per-auth-mode credential resolution, dispatched from
//! `RedmineMcp::scoped`. One module per `AuthMode` variant; future work adds
//! `legacy_per_user`/`oauth` siblings alongside `legacy`.

pub(crate) mod legacy;
pub(crate) mod oauth;
pub(crate) mod per_user;
pub(crate) mod proxy;
/// Public so `tests/oauth_scopes.rs` (an anti-drift check, same shape as
/// `readonly::write_tools`) can inspect `TOOL_SCOPES` directly — the only
/// reason this one auth submodule is not `pub(crate)` like its siblings.
pub mod scope;
