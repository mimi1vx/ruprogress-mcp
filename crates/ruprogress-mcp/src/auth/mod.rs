//! Per-auth-mode credential resolution, dispatched from
//! `RedmineMcp::scoped`. One module per `AuthMode` variant; future work adds
//! `legacy_per_user`/`oauth` siblings alongside `legacy`.

pub(crate) mod legacy;
pub(crate) mod per_user;
