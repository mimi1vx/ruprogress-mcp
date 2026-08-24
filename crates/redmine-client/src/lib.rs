#![warn(missing_docs)]
// The README is the crate docs, which makes its Rust examples doctests —
// the only thing that keeps them compiling as this API changes.
#![doc = include_str!("../README.md")]

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
    AttachmentId, ChecklistItemId, ContactId, DmsfFolderId, DocumentId, IssueCategoryId, IssueId,
    JournalId, MembershipId, ProductId, ProjectId, ProjectIdent, ProjectIdentifier, RelationId,
    TimeEntryId, UserId, VersionId, WikiTitle,
};
pub use page::{Limits, Page};
pub use retry::RetryPolicy;
