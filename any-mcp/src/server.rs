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

/// Minimal server implementation used to establish the MCP protocol boundary.
///
/// Workflow tools, resources, authentication, and stdio transport are layered
/// onto this type in later Phase 1 work.
#[derive(Debug, Clone, Copy, Default)]
pub struct AnyMcpServer;

impl AnyMcpServer {
    /// Creates an MCP server scaffold with no registered tools or resources.
    #[must_use]
    pub const fn new() -> Self {
        Self
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
    use super::*;

    #[test]
    fn advertises_upcoming_protocol_revision_and_server_identity() {
        let info = AnyMcpServer::new().info();

        assert_eq!(info.protocol_version, ProtocolVersion::V_2026_07_28);
        assert_eq!(info.protocol_version.as_str(), "2026-07-28");
        assert_eq!(info.server_info.name, env!("CARGO_PKG_NAME"));
        assert_eq!(info.server_info.version, env!("CARGO_PKG_VERSION"));
    }
}
