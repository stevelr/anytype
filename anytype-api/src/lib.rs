/*
 * Anytype rust api client
 * github.com/stevelr/anytype
 *
 * SPDX-FileCopyrightText: 2025-2026 Steve Schoettler
 * SPDX-License-Identifier: Apache-2.0
 */
//! # Anytype Rust API Client
//!
//! An ergonomic Anytype API client in Rust.
//!
//! ## Features
//!
//! - supports Anytype API 2025-11-08
//! - paginated responses and async Streams
//! - authentication
//! - integrates with OS Keyring for secure key storage
//! - http middleware with retry logic and rate limit handling
//! - client caching (spaces, properties, types)
//! - nested filter expression builder
//! - parameter validation
//! - metrics
//! - companion cli tool
//!
//!
//! ## Quick Start
//!
//! ```rust,no_run
//!
//! use anytype::prelude::*;
//! # async fn example() -> Result<(), AnytypeError> {
//!
//! // Initialize the client with file-based keystore.
//! let mut config = ClientConfig::default().app_name("my-app");
//! config.keystore = Some("file".to_string());
//! let client = AnytypeClient::with_config(config)?;
//! if !client.auth_status()?.http.is_authenticated() {
//!     println!("Not authenticated. Please log in.");
//! }
//!
//! // List spaces
//! let spaces: PagedResult<Space> = client.spaces().list().await?;
//! for space in spaces.iter() {
//!     println!("{}", &space.name);
//! }
//! // Get the first space
//! let space1 = spaces.iter().next().unwrap();
//!
//! // Create an object
//! let obj = client.new_object(&space1.id, "page")
//!     .name("My Document")
//!     .body("# Hello World")
//!     .create().await?;
//!
//! // Search, with filtering and sorting
//! let results: PagedResult<Object> = client.search_in(&space1.id)
//!     .text("meeting notes")
//!     .types(["page", "note"])
//!     .sort_desc("last_modified_date")
//!     .limit(10)
//!     .execute().await?;
//! for doc in results.iter() {
//!     println!("{} {}",
//!         doc.get_property_date("last_modified_date").unwrap_or_default(),
//!         doc.name.as_deref().unwrap_or("(unnamed)"));
//! }
//!
//! // delete object
//! client.object(&space1.id, &obj.id).delete().await?;
//! # Ok(())
//! # }
//! ```
//!
//! ## API Structure
//!
//! The API uses a fluent builder pattern. Methods on `AnytypeClient` return
//! request builders that are configured with chained method calls and then
//! executed with a terminal method like `get()`, `create()`, `update()`, `delete()`,
//! `list()`, or `search()`.
//!
//! Applies to all entity types: - Member, Object, Property, Space, Tag, Template, Type, View,
//! (not all CRUD methods are supported for all types, for example, you can't delete spaces or members).
//!
//! ### Pattern Examples
//!
//! ```rust,no_run
//! use anytype::prelude::*;
//! # async fn example(client: &AnytypeClient) -> Result<(), AnytypeError> {
//!
//! // Get/Delete single item: client.<entity>(ids...).get/delete()
//! let obj = client.object("space_id", "obj_id").get().await?;
//! client.object("space_id", "obj_id").delete().await?;
//!
//! // Create: client.new_<entity>(required_args).optional_args().create()
//! let space = client.new_space("My Space")
//!     .description("Description")
//!     .create().await?;
//!
//! // Update: client.update_<entity>(ids...).fields().update()
//! let space = client.update_space("space_id")
//!     .name("New Name")
//!     .update().await?;
//!
//! // List: client.<entities>(ids...).limit().filter().list()
//! let objects = client.objects("space_id")
//!     .filter(Filter::type_in(vec!["page"]))
//!     .limit(50)
//!     .list().await?;
//! # Ok(())
//! # }
//! ```
//!
//! ### Notes on API Design
//!
//! - Similar structs are combined to keep the API surface small and consistent.
//!   Example: Object and `ObjectWithBody` are unified as `Object { markdown: Option<String>, ... }`.
//! - All methods use a consistent builder flow:
//!   `things(..)`, `thing(..)`, `new_thing(..)`, `update_thing(..)` + optional setters +
//!   terminal verbs like `list()`, `get()`, `create()`, `update()`, or `delete()`.
//! - Single-field response wrappers are unwrapped so callers get the inner type directly.
//! - Parameters accept flexible input types via `Into<String>` and `IntoIterator` where useful.
//! - Property and type keys converted to ids if upstream api requires ids.
//! - Filter/Condition constructors prevent invalid operator combinations, with escape hatches
//!   available for advanced use cases.
//! - Filters default to AND semantics: `.filter()` chains into AND, and `Vec<Filter>.into()`
//!   yields an AND `FilterExpression`.
//! - Enums represent token types like Color and Layout.
//! - A single HTTP pipeline handles validation, logging, serialization, retries, and rate limits.
//! - Pagination uses `PaginatedResponse<T>` and `PagedResult<T>` with `into_stream()` and
//!   `collect_all()` helpers.
//! - Naming exceptions to avoid confusion:
//!   - `get_type()` avoids the `type` keyword (`object()` and `space()` keep the simple name).
//!   - View-related APIs use `view_*` to disambiguate list/collection/query objects
//!     (`list_views`, `view_list_objects`, `view_add_objects`, `view_remove_object`).
//!
//#![warn(clippy::pedantic)] // experimental
//#![warn(clippy::nursery)] // experimental
#![allow(clippy::missing_errors_doc)] // pedantic
#![allow(clippy::missing_const_for_fn)] //  nursery function
#![allow(clippy::must_use_candidate)] // pedantic
#![warn(clippy::default_trait_access)]
#![warn(clippy::doc_markdown)]
#![warn(clippy::explicit_iter_loop)]
#![warn(clippy::future_not_send)]
#![warn(clippy::implicit_clone)]
#![warn(clippy::literal_string_with_formatting_args)]
#![warn(clippy::match_same_arms)]
#![warn(clippy::min_ident_chars)]
#![warn(clippy::needless_raw_strings)]
#![warn(clippy::option_if_let_else)]
#![warn(clippy::redundant_clone)]
#![warn(clippy::ref_option)]
#![warn(clippy::redundant_closure)]
#![warn(clippy::uninlined_format_args)]
#![warn(clippy::unnecessary_wraps)]
#![warn(clippy::unused_async)]

pub mod auth;
pub mod cache;
#[cfg(feature = "grpc")]
pub mod chat_stream;
#[cfg(feature = "grpc")]
pub mod chats;
pub mod client;
pub mod error;
#[cfg(feature = "grpc")]
pub mod files;
pub mod filters;
#[cfg(feature = "grpc")]
pub(crate) mod grpc_util;
mod http_client;
pub mod keystore;
pub mod members;
#[cfg(feature = "grpc")]
#[doc(hidden)]
pub mod mock;
pub mod objects;
pub mod paged;
#[cfg(feature = "grpc")]
pub mod process_watcher;
pub mod properties;
pub mod search;
pub mod spaces;
pub mod tags;
pub mod templates;
pub mod types;
pub mod validation;
pub mod verify;
pub mod views;

pub mod test_util;

/// Result type alias using `AnytypeError` as the default error.
pub type Result<T, E = crate::error::AnytypeError> = std::result::Result<T, E>;

/// Prelude module - import (nearly) all the things with `use anytype::prelude::*;`
pub mod prelude {
    pub use super::{ANYTYPE_API_VERSION, ANYTYPE_DESKTOP_URL, ANYTYPE_HEADLESS_URL};
    // Error types
    pub use crate::error::*;
    pub use crate::{
        // HTTP metrics
        cache::AnytypeCache,
        client::{AnytypeClient, ClientConfig},
        // Filters, Query parameters, and sorting
        filters::{Condition, Filter, FilterExpression, FilterOperator, Sort, SortDirection},
        // HTTP server metrics
        http_client::HttpMetricsSnapshot,
        // Key storage
        keystore::{HttpCredentials, KeyStore, KeyStoreType},
        // Space members
        members::{Member, MemberRole, MemberStatus},
        // Objects
        objects::{Color, DataModel, Icon, Object, ObjectLayout, object_link, object_link_shared},
        // Pagination
        paged::{PagedResult, PaginatedResponse, PaginationMeta},
        // Properties
        properties::{Property, PropertyFormat, PropertyValue, PropertyWithValue, SetProperty},
        // Spaces
        spaces::{Space, SpaceModel},
        // Property tags
        tags::{CreateTagRequest, Tag},
        // Type objects
        types::{CreateTypeProperty, Type, TypeLayout},
        // Validation
        validation::ValidationLimits,
        // Verify
        verify::VerifyConfig,
        // Views (Lists, Collections, Queries)
        views::{View, ViewLayout},
    };
    #[cfg(feature = "grpc")]
    pub use crate::{
        chat_stream::{
            BackoffPolicy, ChatEvent, ChatEventStream, ChatStreamBuilder, ChatStreamControl,
            ChatStreamHandle,
        },
        chats::{
            ChatClient, ChatGetRequest, ChatListRequest, ChatListResult, ChatMessage,
            ChatMessagesPage, ChatReadAllRequest, ChatReadType, ChatResolveRequest,
            ChatSearchRequest, ChatSpaceRequest, ChatState, ChatToggleReactionRequest,
            MessageAttachment, MessageAttachmentType, MessageContent, MessageReaction,
            MessageTextMark, MessageTextMarkType, MessageTextRange, MessageTextStyle,
        },
        client::find_grpc,
        files::{FileObject, FileStyle, FileType, FilesClient},
        keystore::GrpcCredentials,
        process_watcher::{
            ProcessCompletionFallback, ProcessKind, ProcessWatchCancelToken, ProcessWatchProgress,
            ProcessWatchRequest, ProcessWatcher, ProcessWatcherTimeouts,
        },
        spaces::{
            BackupExportFormat, BackupSpaceRequest, DeleteAllArchivedResult, SpaceBackupResult,
        },
    };
}

// ============================================================================
// CONSTANTS
// ============================================================================

/// API version
pub const ANYTYPE_API_VERSION: &str = "2025-11-08";

/// API endpoint (localhost desktop client)
pub const ANYTYPE_DESKTOP_URL: &str = "http://127.0.0.1:31009";

/// API endpoint (CLI/headless server)
pub const ANYTYPE_HEADLESS_URL: &str = "http://127.0.0.1:31012";

pub(crate) mod config {
    /// Environment variable for default endpoint URL
    pub const ANYTYPE_URL_ENV: &str = "ANYTYPE_URL";

    /// API version header
    pub const ANYTYPE_API_HEADER: &str = "Anytype-Version";

    /// Service name for keystore
    pub const DEFAULT_SERVICE_NAME: &str = "anytype_rust";

    /// Warn when the rate-limit wait exceeds this duration (seconds).
    pub const RATE_LIMIT_WAIT_WARN_SECS: u64 = 5;

    /// Fail when the rate-limit wait exceeds this duration (seconds).
    pub const RATE_LIMIT_WAIT_MAX_SECS: u64 = 30;

    /// Environment variable to override rate-limit retry cap (0 disables the cap).
    pub const RATE_LIMIT_MAX_RETRIES_ENV: &str = "ANYTYPE_RATE_LIMIT_MAX_RETRIES";

    /// Maximum consecutive 429 retries before failing.
    pub const RATE_LIMIT_MAX_RETRIES_DEFAULT: u32 = 5;

    /// Maximum pagination limit (API spec: 1000)
    pub const MAX_PAGINATION_LIMIT: u32 = 1000;

    /// Default pagination limit (API spec: 100)
    pub const DEFAULT_PAGINATION_LIMIT: u32 = 100;

    /// Max retries for HTTP client
    pub const MAX_RETRIES: u32 = 3;

    // Validation limits
    pub const VALIDATION_MARKDOWN_MAX_LEN: u64 = 10 * 1024 * 1024;
    pub const VALIDATION_NAME_MAX_LEN: u32 = 4096;
    pub const VALIDATION_TAG_MAX_COUNT: u32 = 4096;
    pub const VALIDATION_TAG_MAX_LEN: u32 = 1024;
    pub const VALIDATION_OID_MIN_LEN: u32 = 20;
    pub const VALIDATION_OID_MAX_LEN: u32 = 200;
    pub const VALIDATION_MAX_QUERY_LEN: u32 = 4000;

    #[doc(hidden)]
    pub const ANYTYPE_TEST_URL_ENV: &str = "ANYTYPE_TEST_URL";

    #[doc(hidden)]
    pub const ANYTYPE_TEST_URL: &str = super::ANYTYPE_HEADLESS_URL;

    #[doc(hidden)]
    #[allow(dead_code)]
    pub const ANYTYPE_TEST_KEY_SERVICE: &str = "anytype_test";
}

// =============================================================================
// Macros
// =============================================================================

/// Assert helper that returns a TestError instead of panicking
#[doc(hidden)]
#[macro_export]
macro_rules! test_assert {
    ($cond:expr, $msg:expr) => {
        if !$cond {
            return Err($crate::test_util::TestError::Assertion {
                message: $msg.to_string(),
            });
        }
    };
}

/// Assert equality helper
#[doc(hidden)]
//#[cfg(test)]
#[macro_export]
macro_rules! test_assert_eq {
    ($left:expr, $right:expr, $msg:expr) => {
        if $left != $right {
            return Err($crate::test_util::TestError::Assertion {
                message: format!("{}: expected {:?}, got {:?}", $msg, $right, $left),
            });
        }
    };
}
