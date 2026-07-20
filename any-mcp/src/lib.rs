// any-mcp - bounded, workflow-oriented MCP server for Anytype
//
// SPDX-FileCopyrightText: 2026 Steve Schoettler
// SPDX-License-Identifier: Apache-2.0

//! Shared foundations for the `any-mcp` binary.
//!
//! The crate exposes authenticated Anytype client startup, bounded upstream
//! execution controls, and the stdio MCP service lifecycle. It depends on
//! `anytype-api` through the `anytype` crate and never directly on generated
//! `anytype-rpc` support.

pub mod config;
pub mod logging;
pub mod runtime;
pub mod server;

pub use config::RuntimeConfig;
pub use runtime::{RuntimeContext, RuntimeError, StartupStatus, serve_stdio};
pub use server::AnyMcpServer;
