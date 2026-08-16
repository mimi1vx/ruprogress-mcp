//! Models for optional third-party Redmine plugin integrations.
//!
//! These are inert `serde` types like every other model in this crate: this
//! crate has no notion of a plugin being "enabled". Whether a family's tools
//! are registered at all is a server-side policy decision (`ruprogress-mcp`'s
//! `PluginFlags`), not something modelled here.

pub mod agile;
pub mod checklists;
pub mod crm;
pub mod dmsf;
pub mod products;
pub mod tags;
