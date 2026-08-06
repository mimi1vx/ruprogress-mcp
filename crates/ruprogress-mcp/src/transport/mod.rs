//! Transport wiring: one module per `TransportConfig` variant.
//!
//! The two have genuinely different shutdown semantics — see the module docs
//! on each — so they are kept apart rather than unified behind a common trait.

pub mod http;
pub mod stdio;
