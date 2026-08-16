//! Plugin-gated tool families: tools that only make sense against a Redmine
//! with a specific third-party plugin installed. Every family's router is
//! merged unconditionally in `server.rs`; whether its tools stay registered
//! is decided immediately after by removing routes per the operator's
//! `PluginFlags` — see `server.rs`'s `PLUGIN_TOOLS` table. A plugin family
//! adds no cargo feature and no conditional merge: gating is that one table,
//! not two mechanisms.

pub(crate) mod checklists;
