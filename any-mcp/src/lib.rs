// any-mcp - bounded, workflow-oriented MCP server for Anytype
//
// SPDX-FileCopyrightText: 2026 Steve Schoettler
// SPDX-License-Identifier: Apache-2.0

//! Shared foundations for the `any-mcp` binary.
//!
//! The crate exposes bounded domain models, strict MCP schemas, response and
//! error helpers, tool annotations, and the server protocol declaration while
//! authenticated workflow handlers are added in later phases. It depends on
//! `anytype-api` through the `anytype` crate and never directly on generated
//! `anytype-rpc` support.

pub mod domain;
pub mod error;
pub mod protocol;
pub mod result;
pub mod schema;
pub mod server;

pub use server::AnyMcpServer;
