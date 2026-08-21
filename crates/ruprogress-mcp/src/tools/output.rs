//! The structured-output contract every tool returns: a success
//! envelope built from `CallToolResult::structured`, an in-band error
//! envelope for everything Redmine tells us, and the response-size caps
//! applied to every list payload before it leaves the process.
//!
//! `outputSchema` (declared per tool via `#[tool(output_schema = ...)]`)
//! describes the **success** payload only. Error results carry
//! `isError: true` and are exempt from output-schema validation — do not
//! union [`ErrorEnvelope`] into a tool's output schema.

use rmcp::model::CallToolResult;
use schemars::JsonSchema;
use serde::Serialize;
use serde_json::Value;
use url::Url;

/// Pagination metadata attached to a list tool's response, computed from a
/// [`redmine_client::Page`]. Present unconditionally until a tool exposes
/// `include_pagination_info` (none does yet).
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub(crate) struct Pagination {
    pub(crate) total: u64,
    pub(crate) limit: u32,
    pub(crate) offset: u64,
    pub(crate) has_next: bool,
    pub(crate) has_previous: bool,
    pub(crate) next_offset: Option<u64>,
    pub(crate) previous_offset: Option<u64>,
    /// `true` when a caller- or server-side limit stopped collection before
    /// every matching item was returned. Never a silent cut: see
    /// [`OutputCaps`].
    pub(crate) truncated: bool,
    /// Set only when `truncated` is `true`: what the caller should do about
    /// it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) hint: Option<String>,
}

impl Pagination {
    /// Build pagination metadata from a fetched page. Does not itself know
    /// about [`OutputCaps`] truncation — call [`ok`] afterwards, which
    /// applies caps to the whole payload including this struct's
    /// `truncated`/`hint` fields.
    pub(crate) fn from_page<T>(page: &redmine_client::Page<T>) -> Self {
        let collected_end = page
            .offset
            .saturating_add(u64::try_from(page.items.len()).unwrap_or(u64::MAX));
        let has_next = collected_end < page.total_count;
        let has_previous = page.offset > 0;
        Self {
            total: page.total_count,
            limit: page.limit,
            offset: page.offset,
            has_next,
            has_previous,
            next_offset: has_next.then_some(collected_end),
            previous_offset: has_previous
                .then(|| page.offset.saturating_sub(u64::from(page.limit))),
            truncated: page.truncated,
            hint: None,
        }
    }
}

/// Hard caps on a single tool response, enforced in [`ok`] above and beyond
/// `redmine-client`'s own byte caps. Configured via
/// `REDMINE_MCP_MAX_RESPONSE_ITEMS`/`REDMINE_MCP_MAX_RESPONSE_BYTES`.
#[derive(Debug, Clone, Copy)]
pub(crate) struct OutputCaps {
    pub(crate) max_items: usize,
    pub(crate) max_bytes: usize,
}

const TRUNCATION_HINT: &str = "response truncated to fit REDMINE_MCP_MAX_RESPONSE_ITEMS/_BYTES; \
                                narrow the query if this tool supports filtering";
fn top_level_array(value: &mut Value) -> Option<&mut Vec<Value>> {
    value
        .as_object_mut()?
        .values_mut()
        .find_map(|v| v.as_array_mut())
}

/// Find the first top-level JSON array in an object response — by the
/// envelope convention every list tool has exactly one — and truncate it to
/// `caps.max_items`, then to whatever additionally fits in `caps.max_bytes`.
/// Marks `pagination.truncated`/`pagination.hint` when either cap fires.
/// A no-op for payloads with no top-level array (e.g. `get_current_user`).
fn apply_caps(value: &mut Value, caps: OutputCaps) {
    let mut truncated = false;
    let Some(array) = top_level_array(value) else {
        return;
    };
    if array.len() > caps.max_items {
        array.truncate(caps.max_items);
        truncated = true;
    }

    loop {
        let size = serde_json::to_vec(&*value).map_or(0, |bytes| bytes.len());
        if size <= caps.max_bytes {
            break;
        }
        let Some(array) = top_level_array(value) else {
            break;
        };
        if array.is_empty() {
            break;
        }
        array.pop();
        truncated = true;
    }

    if truncated
        && let Some(pagination) = value
            .as_object_mut()
            .and_then(|obj| obj.get_mut("pagination"))
            .and_then(Value::as_object_mut)
    {
        pagination.insert("truncated".to_string(), Value::Bool(true));
        pagination.insert(
            "hint".to_string(),
            Value::String(TRUNCATION_HINT.to_string()),
        );
    }
}

/// Benchmark seam for `benches/output_caps.rs`: exercises [`apply_caps`]
/// without exposing `OutputCaps`'s fields or `ok`'s own serialisation.
#[doc(hidden)]
pub fn apply_caps_bench(value: &mut Value, max_items: usize, max_bytes: usize) {
    apply_caps(
        value,
        OutputCaps {
            max_items,
            max_bytes,
        },
    );
}

/// Wrap a successful tool payload in a `CallToolResult` with structured
/// content, applying the response-size caps.
///
/// Every output type in this module is a plain struct of
/// primitives/strings/nested structs, so `T`'s `Serialize` implementation
/// failing would be a bug, not a reachable runtime condition — but a bug
/// here must not take the whole server down, so it degrades to an empty
/// object rather than panicking.
pub(crate) fn ok<T: Serialize>(value: &T, caps: OutputCaps) -> CallToolResult {
    let mut json = serde_json::to_value(value).unwrap_or_else(|e| {
        tracing::error!(error = %e, "tool output failed to serialize; returning an empty object");
        Value::Object(serde_json::Map::new())
    });
    apply_caps(&mut json, caps);
    CallToolResult::structured(json)
}

/// Rewrites `content_url` values whose scheme+host+port matches
/// `REDMINE_URL`'s to sit behind `REDMINE_PUBLIC_URL` instead, for
/// a Redmine reachable internally at one address but fronted by a reverse
/// proxy at another. Built once per tool call via
/// `RedmineMcp::content_url_rewrite` and threaded alongside `&Boundary`
/// through `attachment_out`/`file_entry_out`/`wiki_page_out`.
pub(crate) struct ContentUrlRewrite<'a> {
    redmine: &'a Url,
    public: Option<&'a Url>,
}

impl<'a> ContentUrlRewrite<'a> {
    pub(crate) const fn new(redmine: &'a Url, public: Option<&'a Url>) -> Self {
        Self { redmine, public }
    }

    /// A `content_url` that fails to parse, or whose origin does not match
    /// `REDMINE_URL`'s, is returned unchanged — this only ever narrows an
    /// already-valid URL's authority, never invents one from untrusted
    /// Redmine-authored data. A matching URL keeps `REDMINE_PUBLIC_URL`'s own
    /// path as a prefix (so a reverse-proxy sub-path baked into it survives)
    /// and keeps the original URL's query and fragment.
    pub(crate) fn apply(&self, content_url: &str) -> String {
        let Some(public) = self.public else {
            return content_url.to_string();
        };
        let Ok(parsed) = Url::parse(content_url) else {
            return content_url.to_string();
        };
        let origin_matches = parsed.scheme() == self.redmine.scheme()
            && parsed.host_str() == self.redmine.host_str()
            && parsed.port_or_known_default() == self.redmine.port_or_known_default();
        if !origin_matches {
            return content_url.to_string();
        }

        let mut rewritten = public.clone();
        let mut path = public.path().trim_end_matches('/').to_string();
        path.push_str(parsed.path());
        rewritten.set_path(&path);
        rewritten.set_query(parsed.query());
        rewritten.set_fragment(parsed.fragment());
        rewritten.to_string()
    }
}

/// The closed set of machine-readable error codes every tool's in-band error
/// envelope carries. `#[non_exhaustive]`.
///
/// A `FEATURE_DISABLED` code was considered for plugin-gated tools whose
/// backing Redmine plugin is not installed, and rejected: those tools are
/// de-registered from the router instead (`server.rs`'s `PLUGIN_TOOLS`
/// removal loop), so calling one with its plugin disabled fails
/// `tools/call` with rmcp's own "tool not found" rather than an in-band
/// error a model might retry around. `get_mcp_server_info`'s `plugin_flags`
/// is the discoverability answer for "why is this tool missing".
///
/// `ReadOnly`/`ConfirmationRequired`/`ChildrenPresent` are used slightly
/// differently from the rest: `ReadOnly` goes through
/// [`err`] like every other code (`isError: true` — the model asked for a
/// write the server administrator has disabled, an argument-shaped mistake
/// it should not retry). `ConfirmationRequired`/`ChildrenPresent` are
/// `delete_redmine_issue`-specific and are **not** sent via [`err`]: the
/// reference contract treats a delete refusal as a normal, non-error result
/// carrying `{success: false, code, hint, impact}\` so the model can inspect
/// the impact preview — see `tools/issues.rs::DeleteRedmineIssueOutput`.
///
/// `FileTooLarge`/`StoreFull` (`tools/files.rs`) are
/// local-storage conditions with no `redmine_client::Error` equivalent: the
/// former means a Redmine attachment (or the bytes actually streamed for
/// one, regardless of what its metadata or a response header claimed) is
/// bigger than `ATTACHMENT_MAX_DOWNLOAD_BYTES`; the latter means the whole
/// local store is at `ATTACHMENT_STORE_MAX_BYTES` even after a sweep.
///
/// `SourceRequired`/`UnsupportedSource`/`PathNotAllowed` (`upload_file`) are
/// `content_base64`/`file_path`/`source_url` argument
/// conditions that depend on more than one field at once (so they cannot be
/// caught by `deny_unknown_fields` alone) or on server-side path validation
/// whose failure must never distinguish "outside the roots" from
/// "does not exist" from "not a regular file".
///
/// `InsufficientScope` (`auth::scope`, `oauth` mode only) is the in-band
/// denial for a `tools/call` whose bearer token lacks a required scope,
/// naming the missing scope(s) in the error message.
///
/// `Internal` means a bug in this server — a tool handler panicked — not a
/// problem with Redmine or with the caller's arguments (`panic_guard.rs`).
/// Not retryable: the same call will panic again.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub(crate) enum ErrorCode {
    Unauthorized,
    Forbidden,
    NotFound,
    ValidationFailed,
    RateLimited,
    Unreachable,
    UnexpectedResponse,
    LimitExceeded,
    Misconfigured,
    ReadOnly,
    ConfirmationRequired,
    ChildrenPresent,
    FileTooLarge,
    StoreFull,
    SourceRequired,
    UnsupportedSource,
    PathNotAllowed,
    InsufficientScope,
    Internal,
}

impl ErrorCode {
    /// `true` only for [`Self::RateLimited`] and [`Self::Unreachable`]:
    /// every tool description must say that `retryable: false` means do not
    /// call again with the same arguments.
    pub(crate) const fn is_retryable(self) -> bool {
        matches!(self, Self::RateLimited | Self::Unreachable)
    }
}

/// Build an in-band error result (`isError: true`): `{error, code, retryable,
/// hint}`. Never a protocol-level `McpError`.
pub(crate) fn err(
    code: ErrorCode,
    message: impl Into<String>,
    hint: Option<&str>,
) -> CallToolResult {
    let payload = serde_json::json!({
        "error": message.into(),
        "code": code,
        "retryable": code.is_retryable(),
        "hint": hint,
    });
    CallToolResult::structured_error(payload)
}

/// Like [`err`], but merges `extra` keys into the same four-key envelope.
/// The base shape (`error`, `code`, `retryable`, `hint`) is a documented
/// contract; this is the one place that extends it, so a tool that needs to
/// say more does it here rather than with an ad-hoc `json!` at the call
/// site. `outputSchema` still describes the success payload only — error
/// results, extended or not, are exempt from output-schema validation.
pub(crate) fn err_with(
    code: ErrorCode,
    message: impl Into<String>,
    hint: Option<&str>,
    extra: serde_json::Map<String, Value>,
) -> CallToolResult {
    let mut payload = serde_json::json!({
        "error": message.into(),
        "code": code,
        "retryable": code.is_retryable(),
        "hint": hint,
    });
    if let Some(obj) = payload.as_object_mut() {
        obj.extend(extra);
    }
    CallToolResult::structured_error(payload)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]
mod tests {
    use super::*;

    #[derive(Debug, Serialize, JsonSchema)]
    struct Widget {
        id: u64,
    }

    #[derive(Debug, Serialize, JsonSchema)]
    struct WidgetsOutput {
        widgets: Vec<Widget>,
        pagination: Pagination,
    }

    fn caps(max_items: usize, max_bytes: usize) -> OutputCaps {
        OutputCaps {
            max_items,
            max_bytes,
        }
    }

    fn widgets(n: usize) -> WidgetsOutput {
        let widgets: Vec<Widget> = (0..n as u64).map(|id| Widget { id }).collect();
        WidgetsOutput {
            pagination: Pagination {
                total: widgets.len() as u64,
                limit: 100,
                offset: 0,
                has_next: false,
                has_previous: false,
                next_offset: None,
                previous_offset: None,
                truncated: false,
                hint: None,
            },
            widgets,
        }
    }

    #[test]
    fn ok_sets_structured_content_and_a_matching_text_block() {
        let result = ok(&widgets(1), caps(200, 256 * 1024));
        let structured = result
            .structured_content
            .clone()
            .expect("ok() must set structured_content");
        let text = result.content[0]
            .as_text()
            .expect("content[0] must be a text block")
            .text
            .clone();
        assert_eq!(text, structured.to_string());
        assert_eq!(result.is_error, Some(false));
    }

    #[test]
    fn err_sets_is_error_and_the_envelope_shape() {
        let result = err(
            ErrorCode::Forbidden,
            "no permission",
            Some("try another tool"),
        );
        assert_eq!(result.is_error, Some(true));
        let structured = result.structured_content.expect("err() must be structured");
        assert_eq!(structured["code"], "FORBIDDEN");
        assert_eq!(structured["retryable"], false);
        assert_eq!(structured["hint"], "try another tool");
        assert_eq!(structured["error"], "no permission");
    }

    #[test]
    fn rate_limited_and_unreachable_are_the_only_retryable_codes() {
        for code in [ErrorCode::RateLimited, ErrorCode::Unreachable] {
            assert!(code.is_retryable());
        }
        for code in [
            ErrorCode::Unauthorized,
            ErrorCode::Forbidden,
            ErrorCode::NotFound,
            ErrorCode::ValidationFailed,
            ErrorCode::UnexpectedResponse,
            ErrorCode::LimitExceeded,
            ErrorCode::Misconfigured,
            ErrorCode::ReadOnly,
            ErrorCode::ConfirmationRequired,
            ErrorCode::ChildrenPresent,
            ErrorCode::FileTooLarge,
            ErrorCode::StoreFull,
            ErrorCode::SourceRequired,
            ErrorCode::UnsupportedSource,
            ErrorCode::PathNotAllowed,
            ErrorCode::InsufficientScope,
        ] {
            assert!(!code.is_retryable());
        }
    }

    #[test]
    fn a_500_item_payload_is_capped_to_200_with_truncated_and_a_hint() {
        let result = ok(&widgets(500), caps(200, 256 * 1024));
        let structured = result.structured_content.expect("structured");
        assert_eq!(structured["widgets"].as_array().unwrap().len(), 200);
        assert_eq!(structured["pagination"]["truncated"], true);
        assert!(structured["pagination"]["hint"].is_string());
    }

    #[test]
    fn a_payload_under_both_caps_is_not_marked_truncated() {
        let result = ok(&widgets(5), caps(200, 256 * 1024));
        let structured = result.structured_content.expect("structured");
        assert_eq!(structured["widgets"].as_array().unwrap().len(), 5);
        assert_eq!(structured["pagination"]["truncated"], false);
        assert!(structured["pagination"].get("hint").is_none());
    }

    #[test]
    fn a_single_oversized_item_yields_a_valid_empty_collection_not_a_panic() {
        // One "item" whose serialized size alone exceeds the byte cap.
        let huge = Widget { id: 0 };
        let mut json = serde_json::to_value(WidgetsOutput {
            widgets: vec![huge],
            pagination: Pagination {
                total: 1,
                limit: 100,
                offset: 0,
                has_next: false,
                has_previous: false,
                next_offset: None,
                previous_offset: None,
                truncated: false,
                hint: None,
            },
        })
        .unwrap();
        // A byte cap smaller than even a single serialized item.
        apply_caps(&mut json, caps(200, 5));
        assert_eq!(json["widgets"].as_array().unwrap().len(), 0);
        assert_eq!(json["pagination"]["truncated"], true);
    }

    fn url(s: &str) -> Url {
        s.parse().unwrap()
    }

    #[test]
    fn content_url_rewrite_is_a_no_op_when_public_is_unset() {
        let redmine = url("http://redmine.internal:3000");
        let rewrite = ContentUrlRewrite::new(&redmine, None);
        assert_eq!(
            rewrite.apply("http://redmine.internal:3000/attachments/download/1/a.pdf"),
            "http://redmine.internal:3000/attachments/download/1/a.pdf"
        );
    }

    #[test]
    fn content_url_rewrite_swaps_a_matching_origin() {
        let redmine = url("http://redmine.internal:3000");
        let public = url("https://redmine.example.com");
        let rewrite = ContentUrlRewrite::new(&redmine, Some(&public));
        assert_eq!(
            rewrite.apply("http://redmine.internal:3000/attachments/download/1/a.pdf?x=1#f"),
            "https://redmine.example.com/attachments/download/1/a.pdf?x=1#f"
        );
    }

    #[test]
    fn content_url_rewrite_preserves_a_reverse_proxy_sub_path() {
        let redmine = url("http://redmine.internal:3000");
        let public = url("https://example.com/redmine");
        let rewrite = ContentUrlRewrite::new(&redmine, Some(&public));
        assert_eq!(
            rewrite.apply("http://redmine.internal:3000/attachments/download/1/a.pdf"),
            "https://example.com/redmine/attachments/download/1/a.pdf"
        );
    }

    #[test]
    fn content_url_rewrite_leaves_a_non_matching_origin_untouched() {
        let redmine = url("http://redmine.internal:3000");
        let public = url("https://redmine.example.com");
        let rewrite = ContentUrlRewrite::new(&redmine, Some(&public));
        assert_eq!(
            rewrite.apply("http://some-other-host/attachments/download/1/a.pdf"),
            "http://some-other-host/attachments/download/1/a.pdf"
        );
    }

    #[test]
    fn content_url_rewrite_leaves_unparseable_input_untouched() {
        let redmine = url("http://redmine.internal:3000");
        let public = url("https://redmine.example.com");
        let rewrite = ContentUrlRewrite::new(&redmine, Some(&public));
        assert_eq!(rewrite.apply("not a url"), "not a url");
    }
}
