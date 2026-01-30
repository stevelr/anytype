/*
 * Anytype gRPC client
 * github.com/stevelr/anytype
 *
 * SPDX-FileCopyrightText: 2025-2026 Steve Schoettler
 * SPDX-License-Identifier: Apache-2.0
 */
//! Anytype gRPC client
//!
//! The gRPC api isn't officially supported (by Anytype) for third party clients. However, it's used heavily
//! by the Anytype applications, including the desktop app and headless cli, and it's the only way
//! for applications to access certain functionality that is not available over the HTTP api,
//! such as Files, Chats, Blocks, and Relations.
//!
//! # See also
//!
//! - [anytype](https://crates.io/crates/anytype) An ergonomic Anytype API client in Rust.
//!   Includes http rest api plus gRPC backend using this crate, for access to Files and Chats.
//!
//! - [anyr](https://crates.io/crates/anyr) a CLI tool for listing, searching, and performing
//!   CRUD operations on anytype objects. Via `anytype`, also includes operations on Files and Chats.
//!
//!
// some protoc files after parsing have comment formatting that clippy doesn't like
//   #![allow(clippy::doc_lazy_continuation)]

/// Model types from anytype.model proto package
#[allow(clippy::style)]
pub mod model {
    include!("gen/anytype.model.rs");
}

/// Storage types from anytype.storage proto package
#[allow(clippy::style)]
pub mod storage {
    include!("gen/anytype.storage.rs");
}

/// Anytype service, events, and RPC types
#[allow(clippy::style)]
pub mod anytype {
    include!("gen/anytype.rs");

    pub use client_commands_client::ClientCommandsClient;
}

/// Authentication helpers for creating sessions and attaching tokens.
pub mod auth;
/// gRPC client configuration and helpers.
pub mod client;
/// Helpers for headless config-based auth.
pub mod config;
/// Error types for gRPC operations.
pub mod error;
/// Helpers for dataview view metadata.
pub mod views;
