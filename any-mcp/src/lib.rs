// any-mcp - bounded, workflow-oriented MCP server for Anytype
//
// SPDX-FileCopyrightText: 2026 Steve Schoettler
// SPDX-License-Identifier: Apache-2.0

//! Bounded, workflow-oriented MCP server foundations for Anytype.
//!
//! The `any-mcp` binary owns one authenticated [`anytype`](https://docs.rs/anytype)
//! client and serves strict tools and document resources over stdio. It is a
//! curated workflow surface, not a one-for-one Anytype API mirror, and never
//! depends directly on generated `anytype-rpc` support.
//!
//! # Production defaults
//!
//! - [`ProtocolMode::Stable`] uses the released MCP `2025-11-25`
//!   initialize/initialized lifecycle. The compiled stateless `2026-07-28`
//!   adapter requires an explicit experimental environment selector.
//! - [`ApplicationProfile::Compact`] advertises `server_status`,
//!   `object_search`, `object_get`, and `object_edit`; read-only compact omits
//!   `object_edit`.
//! - [`ApplicationProfile::Standard`] advertises the complete fourteen-tool
//!   Phase 1 catalog; read-only standard retains its ten read tools.
//! - [`OptionalToolsetSelection`] is empty unless `ANY_MCP_TOOLSETS` names a
//!   complete registry linked into the binary. The default-off `members`,
//!   `members`, `files`, nine-tool `schema`, three-tool `views-write`, and
//!   six-tool `chats` registries are linked; Phase 1 remains the default catalog.
//! - Resources advertise only the canonical
//!   `anytype://spaces/{space_id}/objects/{object_id}` template. Instance
//!   listing is empty and document discovery remains paginated through
//!   `object_search`.
//!
//! ```
//! use any_mcp::{ApplicationProfile, ProtocolMode};
//!
//! assert_eq!(ApplicationProfile::default(), ApplicationProfile::Compact);
//! assert_eq!(ProtocolMode::default(), ProtocolMode::Stable);
//! ```
//!
//! # Startup and safety
//!
//! [`RuntimeConfig::from_env`] validates exact protocol, profile, read-only,
//! timeout, concurrency, response-budget, and optional-registry settings
//! without echoing invalid values. [`RuntimeContext::start`] loads existing Anytype credentials and
//! performs bounded authenticated health checks. HTTP is always required;
//! standard read-write also requires gRPC for verified archive readback, while
//! compact and read-only selections may run HTTP-only unless `schema` is
//! or `views-write` is selected, because their bounded type classification or
//! canonical membership evidence requires gRPC through `anytype-api`.
//! [`serve_stdio`] reserves
//! stdout for protocol frames; redacted diagnostics go to stderr.
//!
//! Every handler runs under shared concurrency, timeout, cancellation, response
//! byte, schema, and result bounds. Read-only mode removes mutation tools and
//! rejects stale direct calls before decoding or I/O. Once a mutation may have
//! been dispatched, uncertain outcomes require rereading before retry rather
//! than claiming failure or replaying a write.
//!
//! The crate README contains current host registration, complete tool semantics,
//! protocol compatibility, token baselines, and operational guidance.

pub mod chat_add_toolset;
pub mod chat_delete_toolset;
pub mod chat_read_toolset;
pub mod chats_toolset;
pub mod collection_member_toolset;
pub mod config;
mod create_idempotency;
pub mod cursor;
pub mod discovery;
pub mod domain;
pub mod error;
mod file_content;
pub mod filters;
pub mod handler_support;
pub mod logging;
pub mod member_toolset;
pub mod mutation_value;
pub mod object_archive;
pub mod object_create;
pub mod object_edit;
pub mod object_output;
pub mod object_read;
pub mod object_update;
pub mod optional_toolsets;
pub mod pagination;
pub mod protocol;
pub mod resources;
pub mod result;
pub mod runtime;
pub mod schema;
pub mod schema_property_toolset;
pub mod schema_space_toolset;
pub mod schema_tag_toolset;
pub mod schema_toolset;
pub mod schema_type_toolset;
pub mod server;
mod stdio;
pub mod validation;
pub mod view_handlers;

pub use config::{ApplicationProfile, ProtocolMode, RuntimeConfig};
pub use optional_toolsets::{OptionalToolsetSelection, ToolsetName};
pub use runtime::{
    OperationContext, RuntimeContext, RuntimeError, ServeError, StartupError, StartupStatus,
    serve_stdio,
};
pub use server::{AnyMcpServer, ServerBuildError};
