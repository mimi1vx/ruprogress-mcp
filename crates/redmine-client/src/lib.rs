#![warn(missing_docs)]
//! Pure Redmine REST client. No MCP dependencies.
//!
//! Every public response struct is `#[non_exhaustive]`: it can only be
//! obtained by deserializing a real Redmine response, never built as a
//! struct literal by downstream code (including this crate's own tests).

pub mod auth;
pub mod client;
pub mod error;
pub mod ids;
pub mod model;
pub mod page;
pub mod retry;

pub use auth::Credential;
pub use client::{Query, RedmineClient, RedmineClientBuilder, Scoped};
pub use error::{Error, Result};
pub use ids::{
    IssueCategoryId, IssueId, JournalId, MembershipId, ProjectId, ProjectIdent, ProjectIdentifier,
    RelationId, TimeEntryId, UserId, VersionId, WikiTitle,
};
pub use page::{Limits, Page};
pub use retry::RetryPolicy;
