//! The MCP server: tool router assembly, read-only gating, and the
//! credential choke point every tool starts with.

use std::sync::Arc;

use redmine_client::{RedmineClient, Scoped};
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::model::{Implementation, ServerCapabilities, ServerInfo};
use rmcp::service::RequestContext;
use rmcp::{ErrorData as McpError, RoleServer, ServerHandler, tool_handler};

use crate::config::{AuthMode, Config};
use crate::readonly::write_tools;

#[derive(Clone, Debug)]
pub struct RedmineMcp {
    pub(crate) inner: Arc<ServerInner>,
    tool_router: ToolRouter<RedmineMcp>,
}

#[derive(Debug)]
pub(crate) struct ServerInner {
    pub(crate) client: RedmineClient,
    pub(crate) config: Config,
}

impl RedmineMcp {
    /// Assemble the server: merge every tool module's router (see
    /// `tools/{meta,users,projects}.rs`, each its own `#[tool_router]` block),
    /// then remove write-tool routes if configured read-only.
    #[must_use]
    pub fn new(client: RedmineClient, config: Config) -> Self {
        let mut router = ToolRouter::new();
        router.merge(Self::meta_tool_router());
        router.merge(Self::users_tool_router());
        router.merge(Self::projects_tool_router());
        if config.read_only {
            for name in write_tools::ALL {
                router.remove_route(name);
            }
        }
        Self {
            inner: Arc::new(ServerInner { client, config }),
            tool_router: router,
        }
    }

    /// THE credential choke point. Every tool starts with this line: since
    /// `redmine-client` exposes the Redmine API only on `Scoped`, a tool
    /// physically cannot reach Redmine without going through here.
    pub(crate) fn scoped(&self, _ctx: &RequestContext<RoleServer>) -> Result<Scoped<'_>, McpError> {
        match &self.inner.config.auth {
            AuthMode::Legacy { .. } => crate::auth::legacy::scoped(&self.inner.client),
            AuthMode::LegacyPerUser { .. } => Err(McpError::internal_error(
                "legacy-per-user auth is not yet implemented",
                None,
            )),
            AuthMode::OAuth(_) => Err(McpError::internal_error(
                "oauth auth is not yet implemented",
                None,
            )),
        }
    }

    /// The credential the *server itself* owns, for work not driven by a
    /// client request (the readiness probe). `None` in the auth modes where
    /// the credential arrives per request and there is therefore nothing to
    /// probe with — distinct from `Some(Err(..))`, which means we do own a
    /// credential and it did not work.
    ///
    /// Deliberately not routed through [`Self::scoped`]: that is the
    /// *request* choke point and takes a `RequestContext` precisely so no
    /// caller can reach Redmine on a client's behalf without one.
    pub(crate) fn server_scoped(&self) -> Option<Result<Scoped<'_>, McpError>> {
        match &self.inner.config.auth {
            AuthMode::Legacy { .. } => Some(crate::auth::legacy::scoped(&self.inner.client)),
            AuthMode::LegacyPerUser { .. } | AuthMode::OAuth(_) => None,
        }
    }
}

#[tool_handler(router = self.tool_router.clone())]
impl ServerHandler for RedmineMcp {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::from_build_env())
            .with_instructions(
                "Read-only Redmine access: current user, projects, and server metadata."
                    .to_string(),
            )
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    const fn assert_send_sync_clone<T: Send + Sync + Clone>() {}

    #[test]
    fn redmine_mcp_is_clone_send_sync() {
        assert_send_sync_clone::<RedmineMcp>();
    }
}
