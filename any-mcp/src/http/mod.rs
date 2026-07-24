// any-mcp - bounded, workflow-oriented MCP server for Anytype
//
// SPDX-FileCopyrightText: 2026 Steve Schoettler
// SPDX-License-Identifier: Apache-2.0

//! Explicitly selected Streamable HTTP transport.
//!
//! Stdio remains the production default. `ANY_MCP_TRANSPORT=streamable-http`
//! opts one process into the authenticated loopback HTTP listener described in
//! the approved transport design. This module owns every HTTP concern:
//! configuration, secret handling, request admission, authentication, and the
//! listener. Domain handlers and tool schemas are shared with stdio unchanged
//! and never observe raw headers or bearer credentials.

pub mod config;
pub mod secret;

pub use config::{HttpAuthConfig, HttpConfig, HttpConfigError, TransportSelection};
pub use secret::{StaticToken, StaticTokenError};
