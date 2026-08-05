//! Pagination types.

/// One page — or, via [`crate::client::Scoped`]'s auto-paging helper, the
/// concatenation of several pages — of a Redmine list endpoint.
#[non_exhaustive]
#[derive(Debug, Clone)]
pub struct Page<T> {
    /// The items collected so far.
    pub items: Vec<T>,
    /// `total_count` as reported by Redmine on the last page fetched.
    pub total_count: u64,
    /// Offset of the first item in `items`, relative to the full result set.
    pub offset: u64,
    /// The `limit` (page size) used for the last request.
    pub limit: u32,
    /// `true` when a [`Limits`] cap stopped the walk before `total_count`
    /// items were collected. Not an error: a big project should stay
    /// browsable rather than fail outright.
    pub truncated: bool,
}

/// Caller-controlled bounds on how much a single logical request is allowed
/// to fetch, independent of what Redmine itself would otherwise hand back.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    /// Page size requested per HTTP call. Redmine's server-side max is 100.
    pub page_size: u32,
    /// Maximum number of pages [`crate::client::Scoped`]'s auto-paging
    /// helper will fetch before giving up and returning a truncated page.
    pub max_pages: u32,
    /// Maximum number of items collected across all pages.
    pub max_items: usize,
    /// Maximum size, in bytes, of any single HTTP response body.
    pub max_response_bytes: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            page_size: 100,
            max_pages: 20,
            max_items: 2_000,
            max_response_bytes: 32 * 1024 * 1024,
        }
    }
}
