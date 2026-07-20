// any-mcp - bounded, workflow-oriented MCP server for Anytype
//
// SPDX-FileCopyrightText: 2026 Steve Schoettler
// SPDX-License-Identifier: Apache-2.0

//! Shared foundations for the `any-mcp` binary.
//!
//! The crate exposes authenticated Anytype client startup, bounded upstream
//! execution controls, strict MCP schemas, typed results and errors, and the
//! stdio service lifecycle. It depends on `anytype-api` through the `anytype`
//! crate and never directly on generated `anytype-rpc` support.

pub mod config;
pub mod cursor;
pub mod discovery;
pub mod domain;
pub mod error;
pub mod handler_support;
pub mod logging;
pub mod mutation_value;
pub mod object_archive;
pub mod object_create;
pub mod object_edit;
pub mod object_output;
pub mod object_read;
pub mod object_update;
pub mod pagination;
pub mod protocol;
pub mod resources;
pub mod result;
pub mod runtime;
pub mod schema;
pub mod server;
mod stdio;
pub mod validation;
pub mod view_handlers;

pub use config::{ApplicationProfile, ProtocolMode, RuntimeConfig};
pub use runtime::{
    OperationContext, RuntimeContext, RuntimeError, ServeError, StartupError, StartupStatus,
    serve_stdio,
};
pub use server::{AnyMcpServer, ServerBuildError};
