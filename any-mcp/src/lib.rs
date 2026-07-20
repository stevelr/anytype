// any-mcp - bounded, workflow-oriented MCP server for Anytype
//
// SPDX-FileCopyrightText: 2026 Steve Schoettler
// SPDX-License-Identifier: Apache-2.0

//! Shared foundations for the `any-mcp` binary.
//!
//! The crate exposes the MCP server identity and protocol declaration while the
//! authenticated stdio runtime and workflow handlers are added in later phases.
//! It depends on `anytype-api` through the `anytype` crate and never directly on
//! generated `anytype-rpc` support.

pub mod server;

pub use server::AnyMcpServer;
