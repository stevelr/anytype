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

// The admission shell is fully exercised by its tests; the production
// transport entry wires it in the session-integration child, which removes
// these allowances.
#[allow(dead_code, reason = "wired by the transport entry child")]
pub(crate) mod admission;
#[allow(dead_code, reason = "wired by the transport entry child")]
pub(crate) mod auth;
pub mod config;
#[allow(dead_code, reason = "wired by the transport entry child")]
pub(crate) mod listener;
pub mod secret;

pub use config::{HttpAuthConfig, HttpConfig, HttpConfigError, TransportSelection};
pub use secret::{StaticToken, StaticTokenError};
