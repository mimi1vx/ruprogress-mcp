//! OAuth discovery documents (D1–D3): the scope catalogue and the RFC
//! 9728/8414 metadata rendered from it. Distinct from `auth::oauth`, which
//! holds the bearer-token verifier and middleware — this module has no
//! knowledge of requests or tokens, only of `Config`.

pub(crate) mod metadata;
pub(crate) mod proxy;
pub(crate) mod scopes;
