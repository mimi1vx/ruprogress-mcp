//! `AuthMode::OAuthProxy` support: the redirect-URI allowlist, the DCR
//! client registry, and (once built) the authorization/token
//! endpoints and their stores. Distinct from `oauth::metadata`, which only
//! renders discovery documents.

pub(crate) mod endpoints;
pub(crate) mod pkce;
pub(crate) mod redirect;
pub(crate) mod store;
