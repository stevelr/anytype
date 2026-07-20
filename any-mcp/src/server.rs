// any-mcp - bounded, workflow-oriented MCP server for Anytype
//
// SPDX-FileCopyrightText: 2026 Steve Schoettler
// SPDX-License-Identifier: Apache-2.0

//! MCP server identity and protocol-level configuration.

use rmcp::{
    ServerHandler,
    model::{Implementation, ProtocolVersion, ServerCapabilities, ServerInfo},
};

/// Upcoming MCP protocol revision advertised by `any-mcp`.
///
/// `rmcp` 2.2.0 models this draft revision ahead of its stable default, so the
/// server selects it explicitly to follow the SDK's upcoming API direction.
pub const PROTOCOL_VERSION: ProtocolVersion = ProtocolVersion::V_2026_07_28;

use crate::runtime::RuntimeContext;

/// MCP handler backed by one authenticated, process-long runtime.
#[derive(Debug, Clone)]
pub struct AnyMcpServer {
    runtime: RuntimeContext,
}

impl AnyMcpServer {
    /// Creates an MCP handler using the authenticated runtime.
    #[must_use]
    pub const fn new(runtime: RuntimeContext) -> Self {
        Self { runtime }
    }

    /// Returns the shared authenticated runtime used by workflow handlers.
    #[must_use]
    pub const fn runtime(&self) -> &RuntimeContext {
        &self.runtime
    }

    /// Returns the initialization metadata advertised to MCP clients.
    #[must_use]
    pub fn info(&self) -> ServerInfo {
        <Self as ServerHandler>::get_info(self)
    }
}

impl ServerHandler for AnyMcpServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::default())
            .with_protocol_version(PROTOCOL_VERSION)
            .with_server_info(Implementation::new(
                env!("CARGO_PKG_NAME"),
                env!("CARGO_PKG_VERSION"),
            ))
            .with_instructions("Bounded, workflow-oriented access to Anytype")
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use anytype::prelude::{AnytypeClient, ClientConfig};

    use super::*;

    fn server() -> AnyMcpServer {
        let client = AnytypeClient::with_config(ClientConfig {
            base_url: Some("http://127.0.0.1:1".to_string()),
            keystore: Some("env".to_string()),
            keystore_service: Some("any-mcp-server-test".to_string()),
            app_name: "any-mcp-server-test".to_string(),
            ..ClientConfig::default()
        })
        .expect("in-memory test client");
        let runtime = RuntimeContext::from_parts(
            client,
            1,
            Duration::from_secs(1),
            crate::runtime::StartupStatus {
                http_available: true,
                grpc_available: false,
            },
        );
        AnyMcpServer::new(runtime)
    }

    #[test]
    fn advertises_upcoming_protocol_revision_and_server_identity() {
        let info = server().info();

        assert_eq!(info.protocol_version, ProtocolVersion::V_2026_07_28);
        assert_eq!(info.protocol_version.as_str(), "2026-07-28");
        assert_eq!(info.server_info.name, env!("CARGO_PKG_NAME"));
        assert_eq!(info.server_info.version, env!("CARGO_PKG_VERSION"));
    }
}
