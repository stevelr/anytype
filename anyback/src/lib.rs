/*
 * anyback - archive reader library for Anytype backups
 * github.com/stevelr/anytype
 *
 * SPDX-FileCopyrightText: 2025-2026 Steve Schoettler
 * SPDX-License-Identifier: Apache-2.0
 */
#![warn(clippy::pedantic)]
#![warn(clippy::nursery)]
#![allow(clippy::missing_errors_doc)]
#![allow(clippy::missing_const_for_fn)]
#![allow(clippy::must_use_candidate)]
#![allow(clippy::struct_excessive_bools)]
#![allow(clippy::format_push_string)]
#![warn(clippy::default_trait_access)]
#![warn(clippy::doc_markdown)]
#![warn(clippy::explicit_iter_loop)]
#![warn(clippy::future_not_send)]
#![warn(clippy::implicit_clone)]
#![warn(clippy::literal_string_with_formatting_args)]
#![warn(clippy::match_same_arms)]
#![warn(clippy::option_if_let_else)]
#![warn(clippy::redundant_clone)]
#![warn(clippy::ref_option)]
#![warn(clippy::redundant_closure)]
#![warn(clippy::uninlined_format_args)]
#![warn(clippy::unnecessary_wraps)]
#![warn(clippy::unused_async)]

//! Reusable backup archive APIs live in [`archive`], `metadata`, and `markdown`.
//! These modules are intended to remain stable. CLI integration APIs are
//! implementation details and may change as command ownership moves to `anyr`.

/// Traverses ZIP archives and unpacked backup directories.
pub mod archive;
/// Embedded command integration for `anyr`; this API is subject to change.
#[doc(hidden)]
#[cfg(feature = "cli")]
pub mod cli;
/// Renders archived object snapshots as Markdown and extracts raw file payloads.
#[cfg(feature = "cli")]
pub mod markdown;
#[cfg(feature = "metadata")]
pub mod metadata;
