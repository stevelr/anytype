/*
 * Anytype gRPC client
 * github.com/stevelr/anytype
 *
 * SPDX-FileCopyrightText: 2025-2026 Steve Schoettler
 * SPDX-License-Identifier: Apache-2.0
 */
//! Anytype gRPC client
//!
//! The gRPC api is subject to change and isn't officially supported for third party clients.
//!
//! This crate is experimental. If you need to use gRPC because some functionality isn't available in the
//! [anytype](https://crates.io/crates/anytype) REST api, this crate may help.
//!
//! A very limited cli can list spaces and import and export objects.
//!
//! # See also
//! - [anytype](https://crates.io/crates/anytype) a supported Anytype client that uses Anytype's official REST API.
//! - [anyr](https://crates.io/crates/anyr) a CLI tool for listing, searching, and performing CRUD operations on anytype objects.
//!
//!
// some protoc files after parsing have comment formatting that clippy doesn't like
#![allow(clippy::doc_lazy_continuation)]

/// Model types from anytype.model proto package
pub mod model {
    tonic::include_proto!("anytype.model");
}

/// Anytype service, events, and RPC types
pub mod anytype {
    tonic::include_proto!("anytype");

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
