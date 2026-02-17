//! # Anytype Spaces
//!
//! This module provides a fluent builder API for working with Anytype spaces.
//!
//! ## Space methods on `AnytypeClient`
//!
//! - [`spaces`](AnytypeClient::spaces) - list spaces the authenticated user can access
//! - [`space`](AnytypeClient::space) - get space
//! - [`new_space`](AnytypeClient::new_space) - create a new space
//! - [`update_space`](AnytypeClient::space) - update space properties
//! - [`backup_space`](AnytypeClient::backup_space) - back up a space (requires `grpc` feature)
//!
//! ## Quick Start
//!
//! ```rust,no_run
//! use anytype::prelude::*;
//!
//! # async fn example(client: &AnytypeClient) -> Result<(), AnytypeError> {
//! // List all spaces
//! let spaces = client.spaces().list().await?;
//!
//! // Get a specific space
//! let space = client.space("space_id").get().await?;
//!
//! // Create a new space
//! let space = client.new_space("My Space")
//!     .description("A workspace for my projects")
//!     .create().await?;
//!
//! // Update a space
//! let space = client.update_space("space_id")
//!     .name("Updated Name")
//!     .update().await?;
//! # Ok(())
//! # }
//! ```
//!
//! ## Types
//!
//! - [`Space`] - Represents an Anytype space (workspace)
//! - [`SpaceModel`] - Model type (Space or Chat)
//! - [`SpaceRequest`] - Builder for getting a space
//! - [`NewSpaceRequest`] - Builder for creating a space
//! - [`UpdateSpaceRequest`] - Builder for updating a space
//! - [`ListSpacesRequest`] - Builder for listing spaces
//! - [`BackupSpaceRequest`] - Builder for backing up a space (requires `grpc` feature)
//! - [`BackupExportFormat`] - Export format for backups (requires `grpc` feature)

#[cfg(feature = "grpc")]
use std::collections::HashSet;
use std::sync::Arc;

use futures::StreamExt;
use serde::{Deserialize, Serialize};
use snafu::prelude::*;
#[cfg(feature = "grpc")]
use tracing::debug;

#[cfg(feature = "grpc")]
use std::path::{Path, PathBuf};
#[cfg(feature = "grpc")]
use std::time::Duration;

#[cfg(feature = "grpc")]
use anytype_rpc::anytype::rpc::object::list_delete;
#[cfg(feature = "grpc")]
pub use anytype_rpc::backup::SpaceBackupResult;
#[cfg(feature = "grpc")]
use anytype_rpc::backup::{ExportFormat, SpaceBackupOptions};
#[cfg(feature = "grpc")]
use anytype_rpc::{anytype::rpc::object::search_with_meta, model};
#[cfg(feature = "grpc")]
use prost_types::{ListValue, Value};
#[cfg(feature = "grpc")]
use tonic::Request;

#[cfg(feature = "grpc")]
use crate::grpc_util::{ensure_error_ok, grpc_status, with_token_request};
use crate::{
    Result,
    cache::AnytypeCache,
    client::AnytypeClient,
    filters::{Query, QueryWithFilters},
    http_client::{GetPaged, HttpClient},
    prelude::*,
    verify::{VerifyConfig, VerifyPolicy, resolve_verify, verify_available},
};

/// Model type for spaces.
///
/// Determines whether this is a regular workspace or a chat space.
#[derive(
    Debug, Deserialize, Serialize, Clone, PartialEq, Eq, Default, strum::Display, strum::EnumString,
)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum SpaceModel {
    /// Regular workspace for organizing objects
    #[default]
    Space,
    /// Chat-based space for messaging
    Chat,
}

/// Represents an Anytype space (workspace).
///
/// Spaces are top-level containers that hold objects, types, properties, and members.
/// Each space has its own isolated data and can be shared with other users.
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct Space {
    /// Unique space identifier
    pub id: String,

    /// Display name of the space
    pub name: String,

    /// Data model type (Space or Chat)
    pub object: SpaceModel,

    /// Optional description of the space
    pub description: Option<String>,

    /// Space icon (emoji, file, or colored icon)
    pub icon: Option<Icon>,

    /// Gateway URL for serving files and media
    /// Example: "<http://127.0.0.1:31006>"
    pub gateway_url: Option<String>,

    /// Network ID of the space
    /// Example: `N83gJpVd9MuNRZAuJLZ7LiMntTThhPc6DtzWWVjb1M3PouVU`
    pub network_id: Option<String>,
}

impl Space {
    /// Returns true if this is a Chat space.
    pub fn is_chat(&self) -> bool {
        self.object == SpaceModel::Chat
    }

    /// Returns true if this is a regular Space (not a Chat).
    pub fn is_space(&self) -> bool {
        self.object == SpaceModel::Space
    }
}

// ============================================================================
// RESPONSE TYPES (internal)
// ============================================================================

/// Response wrapper for single space operations
#[derive(Deserialize)]
struct SpaceResponse {
    space: Space,
}

// ============================================================================
// REQUEST BODY TYPES (internal)
// ============================================================================

/// Internal request body for creating a space
#[derive(Debug, Serialize)]
struct CreateSpaceRequestBody {
    name: String,

    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
}

/// Internal request body for updating a space
#[derive(Debug, Serialize, Default)]
struct UpdateSpaceRequestBody {
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
}

// ============================================================================
// BUILDER STRUCTS (public)
// ============================================================================

/// Request builder for getting a single space.
///
/// Obtained via [`AnytypeClient::space`].
///
/// # Example
///
/// ```rust
/// # use anytype::prelude::*;
/// # async fn example() -> Result<(), AnytypeError> {
/// #   let client = AnytypeClient::new("doc test")?;
/// #   let space_id = anytype::test_util::example_space_id(&client).await?;
/// let space = client.space(&space_id).get().await?;
/// println!("Space: {} ({})", space.name, space.id);
/// # Ok(())
/// # }
/// ```
#[derive(Debug)]
pub struct SpaceRequest {
    client: Arc<HttpClient>,
    space_id: String,
    cache: Arc<AnytypeCache>,
}

impl SpaceRequest {
    /// Creates a new `SpaceRequest`.
    pub(crate) fn new(
        client: Arc<HttpClient>,
        space_id: impl Into<String>,
        cache: Arc<AnytypeCache>,
    ) -> Self {
        Self {
            client,
            space_id: space_id.into(),
            cache,
        }
    }

    /// Retrieves the space by ID.
    ///
    /// # Returns
    /// The space with all its metadata.
    ///
    /// # Errors
    /// - [`AnytypeError::NotFound`] if the space doesn't exist
    pub async fn get(self) -> Result<Space> {
        if self.cache.is_enabled() {
            if let Some(space) = self.cache.get_space(&self.space_id) {
                return Ok(space);
            }
            if !self.cache.has_spaces() {
                prime_cache_spaces(&self.client, &self.cache).await?;
                if let Some(space) = self.cache.get_space(&self.space_id) {
                    return Ok(space);
                }
            }
            return NotFoundSnafu {
                obj_type: "Space".to_string(),
                key: self.space_id.clone(),
            }
            .fail();
        }

        let response: SpaceResponse = self
            .client
            .get_request(
                &format!("/v1/spaces/{}", self.space_id),
                QueryWithFilters::default(),
            )
            .await?;
        Ok(response.space)
    }
}

/// Request builder for creating a new space.
///
/// Obtained via [`AnytypeClient::new_space`].
///
/// # Example
///
/// ```rust,no_run
/// # use anytype::prelude::*;
/// # async fn example(client: &AnytypeClient) -> Result<(), AnytypeError> {
/// let space = client.new_space("My Workspace")
///     .description("A place for my projects")
///     .create().await?;
/// # Ok(())
/// # }
/// ```
#[derive(Debug)]
pub struct NewSpaceRequest {
    client: Arc<HttpClient>,
    name: String,
    description: Option<String>,
    verify_policy: VerifyPolicy,
    verify_config: Option<VerifyConfig>,
}

impl NewSpaceRequest {
    /// Creates a new `NewSpaceRequest`.
    pub(crate) fn new(
        client: Arc<HttpClient>,
        name: impl Into<String>,
        verify_config: Option<VerifyConfig>,
    ) -> Self {
        Self {
            client,
            name: name.into(),
            description: None,
            verify_policy: VerifyPolicy::Default,
            verify_config,
        }
    }

    /// Sets the space description.
    ///
    /// # Arguments
    /// * `description` - Description text for the space
    #[must_use]
    pub fn description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    /// Enables read-after-write verification for this request.
    #[must_use]
    pub fn ensure_available(mut self) -> Self {
        self.verify_policy = VerifyPolicy::Enabled;
        self
    }

    /// Enables verification using a custom config for this request.
    #[must_use]
    pub fn ensure_available_with(mut self, config: VerifyConfig) -> Self {
        self.verify_policy = VerifyPolicy::Enabled;
        self.verify_config = Some(config);
        self
    }

    /// Disables verification for this request.
    #[must_use]
    pub fn no_verify(mut self) -> Self {
        self.verify_policy = VerifyPolicy::Disabled;
        self
    }

    /// Creates the space with the configured settings.
    ///
    /// # Returns
    /// The newly created space.
    ///
    /// # Errors
    /// - [`AnytypeError::Validation`] if required fields are invalid
    pub async fn create(self) -> Result<Space> {
        let request_body = CreateSpaceRequestBody {
            name: self.name,
            description: self.description,
        };

        let response: SpaceResponse = self
            .client
            .post_request("/v1/spaces", &request_body, QueryWithFilters::default())
            .await?;

        let space = response.space;
        if let Some(config) = resolve_verify(self.verify_policy, self.verify_config.as_ref()) {
            return verify_available(&config, "Space", &space.id, || async {
                let response: SpaceResponse = self
                    .client
                    .get_request(
                        &format!("/v1/spaces/{}", space.id),
                        QueryWithFilters::default(),
                    )
                    .await?;
                Ok(response.space)
            })
            .await;
        }
        Ok(space)
    }
}

/// Request builder for updating an existing space.
///
/// Obtained via [`AnytypeClient::update_space`].
///
/// # Example
///
/// ```rust,no_run
/// # use anytype::prelude::*;
/// # async fn example(client: &AnytypeClient) -> Result<(), AnytypeError> {
/// let space = client.update_space("space_id")
///     .name("New Name")
///     .description("Updated description")
///     .update().await?;
/// # Ok(())
/// # }
/// ```
#[derive(Debug)]
pub struct UpdateSpaceRequest {
    client: Arc<HttpClient>,
    space_id: String,
    name: Option<String>,
    description: Option<String>,
    verify_policy: VerifyPolicy,
    verify_config: Option<VerifyConfig>,
}

impl UpdateSpaceRequest {
    /// Creates a new `UpdateSpaceRequest`.
    pub(crate) fn new(
        client: Arc<HttpClient>,
        space_id: impl Into<String>,
        verify_config: Option<VerifyConfig>,
    ) -> Self {
        Self {
            client,
            space_id: space_id.into(),
            name: None,
            description: None,
            verify_policy: VerifyPolicy::Default,
            verify_config,
        }
    }

    /// Updates the space name.
    ///
    /// # Arguments
    /// * `name` - New display name for the space
    #[must_use]
    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Updates the space description.
    ///
    /// # Arguments
    /// * `description` - New description text for the space
    #[must_use]
    pub fn description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    /// Enables read-after-write verification for this request.
    #[must_use]
    pub fn ensure_available(mut self) -> Self {
        self.verify_policy = VerifyPolicy::Enabled;
        self
    }

    /// Enables verification using a custom config for this request.
    #[must_use]
    pub fn ensure_available_with(mut self, config: VerifyConfig) -> Self {
        self.verify_policy = VerifyPolicy::Enabled;
        self.verify_config = Some(config);
        self
    }

    /// Disables verification for this request.
    #[must_use]
    pub fn no_verify(mut self) -> Self {
        self.verify_policy = VerifyPolicy::Disabled;
        self
    }

    /// Applies the update to the space.
    ///
    /// # Returns
    /// The updated space.
    ///
    /// # Errors
    /// - [`AnytypeError::Validation`] if called without setting any fields
    /// - [`AnytypeError::NotFound`] if the space doesn't exist
    pub async fn update(self) -> Result<Space> {
        // Check that at least one field is being updated
        ensure!(
            self.name.is_some() || self.description.is_some(),
            ValidationSnafu {
                message:
                    "update_space: must set at least one field to update (name or description)"
                        .to_string(),
            }
        );

        let request_body = UpdateSpaceRequestBody {
            name: self.name,
            description: self.description,
        };

        let response: SpaceResponse = self
            .client
            .patch_request(&format!("/v1/spaces/{}", self.space_id), &request_body)
            .await?;

        let space = response.space;
        if let Some(config) = resolve_verify(self.verify_policy, self.verify_config.as_ref()) {
            return verify_available(&config, "Space", &space.id, || async {
                let response: SpaceResponse = self
                    .client
                    .get_request(
                        &format!("/v1/spaces/{}", space.id),
                        QueryWithFilters::default(),
                    )
                    .await?;
                Ok(response.space)
            })
            .await;
        }
        Ok(space)
    }
}

/// Request builder for listing spaces.
///
/// Obtained via [`AnytypeClient::spaces`].
///
/// # Example
///
/// ```rust
/// # use anytype::prelude::*;
/// # async fn example() -> Result<(), AnytypeError> {
/// #   let client = AnytypeClient::new("doc test")?;
/// // List all spaces
/// let spaces = client.spaces().list().await?;
///
/// // List with filters
/// let spaces = client.spaces()
///     .limit(10)
///     .filter(Filter::text_not_contains("name", "Demo"))
///     .list().await?;
///
/// // Collect all spaces across pages
/// let all_spaces = client.spaces().list().await?.collect_all().await?;
/// # Ok(())
/// # }
/// ```
#[derive(Debug)]
pub struct ListSpacesRequest {
    client: Arc<HttpClient>,
    limit: Option<u32>,
    offset: Option<u32>,
    filters: Vec<Filter>,
    cache: Arc<AnytypeCache>,
}

impl ListSpacesRequest {
    /// Creates a new `ListSpacesRequest`.
    pub(crate) fn new(client: Arc<HttpClient>, cache: Arc<AnytypeCache>) -> Self {
        Self {
            client,
            limit: None,
            offset: None,
            filters: Vec::new(),
            cache,
        }
    }

    /// Sets the pagination limit (max items per page).
    ///
    /// Default is 100, maximum is 1000.
    ///
    /// # Arguments
    /// * `limit` - Number of items to return per page
    #[must_use]
    pub fn limit(mut self, limit: u32) -> Self {
        self.limit = Some(limit);
        self
    }

    /// Sets the pagination offset (starting position).
    ///
    /// # Arguments
    /// * `offset` - Number of items to skip
    #[must_use]
    pub fn offset(mut self, offset: u32) -> Self {
        self.offset = Some(offset);
        self
    }

    /// Adds a filter condition.
    ///
    /// Multiple filters are combined with AND logic.
    ///
    /// # Arguments
    /// * `filter` - Filter condition to add
    #[must_use]
    pub fn filter(mut self, filter: Filter) -> Self {
        self.filters.push(filter);
        self
    }

    /// Adds multiple filter conditions.
    ///
    /// # Arguments
    /// * `filters` - Iterator of filters to add
    #[must_use]
    pub fn filters(mut self, filters: impl IntoIterator<Item = Filter>) -> Self {
        self.filters.extend(filters);
        self
    }

    /// Executes the list request.
    ///
    /// # Returns
    /// A paginated result containing the matching spaces.
    pub async fn list(self) -> Result<PagedResult<Space>> {
        if self.cache.is_enabled()
            && self.limit.is_none()
            && self.offset.unwrap_or_default() == 0
            && self.filters.is_empty()
        {
            if let Some(spaces) = self.cache.spaces() {
                return Ok(PagedResult::from_items(spaces));
            }
            prime_cache_spaces(&self.client, &self.cache).await?;
            let spaces = self.cache.spaces().unwrap_or_default();
            return Ok(PagedResult::from_items(spaces));
        }

        let query = Query::default()
            .set_limit_opt(self.limit)
            .set_offset_opt(self.offset)
            .add_filters(&self.filters);

        self.client.get_request_paged("/v1/spaces", query).await
    }
}

/// Result of [`AnytypeClient::delete_all_archived`].
#[cfg(feature = "grpc")]
#[derive(Debug, Clone)]
pub struct DeleteAllArchivedResult {
    /// Number of objects successfully deleted.
    pub deleted: u64,
    /// Object IDs that could not be deleted (backend errors).
    pub failed_ids: Vec<String>,
}

/// Request builder for listing archived objects in a space.
///
/// Obtained via [`AnytypeClient::list_archived`].
#[derive(Debug)]
pub struct ListArchivedRequest<'a> {
    client: &'a AnytypeClient,
    limits: ValidationLimits,
    space_id: String,
    limit: Option<u32>,
    offset: Option<u32>,
    type_ids: Vec<String>,
}

impl<'a> ListArchivedRequest<'a> {
    pub(crate) fn new(
        client: &'a AnytypeClient,
        limits: ValidationLimits,
        space_id: impl Into<String>,
    ) -> Self {
        Self {
            client,
            limits,
            space_id: space_id.into(),
            limit: None,
            offset: None,
            type_ids: Vec::new(),
        }
    }

    /// Sets the pagination limit (max items per page).
    #[must_use]
    pub fn limit(mut self, limit: u32) -> Self {
        self.limit = Some(limit);
        self
    }

    /// Sets the pagination offset (starting position).
    #[must_use]
    pub fn offset(mut self, offset: u32) -> Self {
        self.offset = Some(offset);
        self
    }

    /// Filters archived objects by type ids.
    #[must_use]
    pub fn types(mut self, type_ids: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.type_ids = type_ids.into_iter().map(Into::into).collect();
        self
    }

    /// Executes the archived-list request.
    pub async fn list(self) -> Result<PagedResult<Object>> {
        self.limits.validate_id(&self.space_id, "space_id")?;
        #[cfg(feature = "grpc")]
        {
            return search_archived_objects(
                self.client,
                &self.space_id,
                self.limit,
                self.offset,
                &self.type_ids,
            )
            .await;
        }

        #[cfg(not(feature = "grpc"))]
        {
            return GrpcUnavailableSnafu {
                message: "list_archived requires grpc feature".to_string(),
            }
            .fail();
        }
    }
}

/// Export format for space backups.
///
// This exposes a subset of the internal export formats that are suitable for backups.
#[cfg(feature = "grpc")]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, strum::EnumString)]
pub enum BackupExportFormat {
    /// Markdown format
    #[strum(ascii_case_insensitive)]
    Markdown,

    /// Protobuf binary format
    #[strum(ascii_case_insensitive, serialize = "proto")]
    Protobuf,

    /// JSON format
    #[strum(ascii_case_insensitive)]
    #[default]
    Json,
}

#[cfg(feature = "grpc")]
impl BackupExportFormat {
    /// Converts to the internal gRPC export format.
    fn to_export_format(self) -> ExportFormat {
        match self {
            Self::Markdown => ExportFormat::Markdown,
            Self::Protobuf => ExportFormat::Protobuf,
            Self::Json => ExportFormat::Json,
        }
    }
}

/// Request builder for backing up a space.
///
/// Obtained via [`AnytypeClient::backup_space`].
///
/// # Example
///
/// ```rust,no_run
/// # use anytype::prelude::*;
/// # async fn example(client: &AnytypeClient) -> Result<(), AnytypeError> {
/// let result = client.backup_space("space_id")
///     .format(BackupExportFormat::Json)
///     .backup_dir("/tmp/backups")
///     .include_files(true)
///     .backup().await?;
/// println!("Backup saved to: {}", result.output_path.display());
/// # Ok(())
/// # }
/// ```
#[cfg(feature = "grpc")]
#[derive(Debug)]
pub struct BackupSpaceRequest<'a> {
    client: &'a AnytypeClient,
    space_id: String,
    backup_dir: Option<PathBuf>,
    filename_prefix: Option<String>,
    object_ids: Vec<String>,
    format: BackupExportFormat,
    zip: Option<bool>,
    include_nested: Option<bool>,
    include_files: Option<bool>,
    is_json: Option<bool>,
    include_archived: Option<bool>,
    include_backlinks: Option<bool>,
    include_space: Option<bool>,
    md_include_properties_and_schema: Option<bool>,
}

#[cfg(feature = "grpc")]
impl BackupSpaceRequest<'_> {
    /// Sets the backup output directory.
    ///
    /// Defaults to the current working directory.
    #[must_use]
    pub fn backup_dir(mut self, path: impl AsRef<Path>) -> Self {
        self.backup_dir = Some(path.as_ref().to_path_buf());
        self
    }

    /// Sets the filename prefix for the backup file.
    ///
    /// Defaults to `"backup"`.
    #[must_use]
    pub fn filename_prefix(mut self, prefix: impl Into<String>) -> Self {
        self.filename_prefix = Some(prefix.into());
        self
    }

    /// Sets specific object IDs to export.
    ///
    /// If empty (the default), exports the full space.
    #[must_use]
    pub fn object_ids(mut self, ids: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.object_ids = ids.into_iter().map(Into::into).collect();
        self
    }

    /// Sets the export format.
    ///
    /// Defaults to [`BackupExportFormat::Json`].
    #[must_use]
    pub fn format(mut self, format: BackupExportFormat) -> Self {
        self.format = format;
        self
    }

    /// Whether to produce a zip archive.
    ///
    /// Defaults to `true`.
    #[must_use]
    pub fn zip(mut self, zip: bool) -> Self {
        self.zip = Some(zip);
        self
    }

    /// Whether to include linked (nested) objects.
    ///
    /// Defaults to `true`.
    #[must_use]
    pub fn include_nested(mut self, include: bool) -> Self {
        self.include_nested = Some(include);
        self
    }

    /// Whether to include attached files.
    ///
    /// Defaults to `true`.
    #[must_use]
    pub fn include_files(mut self, include: bool) -> Self {
        self.include_files = Some(include);
        self
    }

    /// For protobuf export, whether to use JSON payload format.
    ///
    /// Defaults to `false`.
    #[must_use]
    pub fn is_json(mut self, is_json: bool) -> Self {
        self.is_json = Some(is_json);
        self
    }

    /// Whether to include archived objects.
    ///
    /// Defaults to `false`.
    #[must_use]
    pub fn include_archived(mut self, include: bool) -> Self {
        self.include_archived = Some(include);
        self
    }

    /// Whether to include backlinks.
    ///
    /// Defaults to `false`.
    #[must_use]
    pub fn include_backlinks(mut self, include: bool) -> Self {
        self.include_backlinks = Some(include);
        self
    }

    /// Whether to include space metadata.
    ///
    /// Defaults to `false`.
    #[must_use]
    pub fn include_space(mut self, include: bool) -> Self {
        self.include_space = Some(include);
        self
    }

    /// Whether to include properties frontmatter and schema for markdown export.
    ///
    /// Defaults to `true`.
    #[must_use]
    pub fn md_include_properties_and_schema(mut self, include: bool) -> Self {
        self.md_include_properties_and_schema = Some(include);
        self
    }

    /// Executes the backup.
    ///
    /// # Returns
    /// The backup result including the output path and number of exported objects.
    ///
    /// # Errors
    /// - [`AnytypeError::Other`] if the backup fails
    pub async fn backup(self) -> Result<SpaceBackupResult> {
        let mut options = SpaceBackupOptions::new(&self.space_id);
        if let Some(dir) = self.backup_dir {
            options.backup_dir = dir;
        }
        if let Some(prefix) = self.filename_prefix {
            options.filename_prefix = prefix;
        }
        if !self.object_ids.is_empty() {
            options.object_ids = self.object_ids;
        }
        options.format = self.format.to_export_format();
        if let Some(zip) = self.zip {
            options.zip = zip;
        }
        if let Some(include_nested) = self.include_nested {
            options.include_nested = include_nested;
        }
        if let Some(include_files) = self.include_files {
            options.include_files = include_files;
        }
        if let Some(is_json) = self.is_json {
            options.is_json = is_json;
        }
        if let Some(include_archived) = self.include_archived {
            options.include_archived = include_archived;
        }
        options.no_progress = true;
        if let Some(include_backlinks) = self.include_backlinks {
            options.include_backlinks = include_backlinks;
        }
        if let Some(include_space) = self.include_space {
            options.include_space = include_space;
        }
        if let Some(md) = self.md_include_properties_and_schema {
            options.md_include_properties_and_schema = md;
        }

        let grpc = self.client.grpc_client().await?;
        grpc.backup_space(options)
            .await
            .map_err(|err| AnytypeError::Grpc { source: err.into() })
    }
}

/// Load all spaces into cache.
async fn prime_cache_spaces(client: &Arc<HttpClient>, cache: &Arc<AnytypeCache>) -> Result<()> {
    let query = Query::default().add_filters(&[]);
    let spaces = client
        .get_request_paged("/v1/spaces", query)
        .await?
        .collect_all()
        .await?;
    cache.set_spaces(spaces);
    Ok(())
}

// ============================================================================
// ANYTYPECLIENT METHODS
// ============================================================================

impl AnytypeClient {
    /// Creates a request builder for getting a single space.
    ///
    /// # Arguments
    /// * `space_id` - ID of the space to retrieve
    ///
    /// # Example
    ///
    /// ```rust
    /// # use anytype::prelude::*;
    /// # async fn example() -> Result<(), AnytypeError> {
    /// #   let client = AnytypeClient::new("doc test")?;
    /// #   let space_id = anytype::test_util::example_space_id(&client).await?;
    /// let space = client.space(&space_id).get().await?;
    /// println!("Space: {}", space.name);
    /// # Ok(())
    /// # }
    /// ```
    pub fn space(&self, space_id: impl Into<String>) -> SpaceRequest {
        SpaceRequest::new(self.client.clone(), space_id, self.cache.clone())
    }

    /// Creates a request builder for creating a new space.
    ///
    /// # Arguments
    /// * `name` - Name for the new space
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use anytype::prelude::*;
    /// # async fn example(client: &AnytypeClient) -> Result<(), AnytypeError> {
    /// let space = client.new_space("My Workspace")
    ///     .description("Description here")
    ///     .create().await?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn new_space(&self, name: impl Into<String>) -> NewSpaceRequest {
        NewSpaceRequest::new(self.client.clone(), name, self.config.verify.clone())
    }

    /// Searches for a space by name.
    ///
    /// # Errors
    /// - [`AnytypeError::NotFound`] if no space of that name was found
    ///
    pub async fn lookup_space_by_name(&self, name: impl AsRef<str>) -> Result<Space> {
        let name = name.as_ref();
        if self.cache.is_enabled() {
            if !self.cache.has_spaces() {
                prime_cache_spaces(&self.client, &self.cache).await?;
            }
            return self
                .cache
                .lookup_space_by_name(name)
                .ok_or(AnytypeError::NotFound {
                    obj_type: "Space".to_string(),
                    key: name.to_string(),
                });
        }
        let mut stream = self.spaces().list().await?.into_stream();
        while let Some(space) = stream.next().await {
            let space = space?;
            if space.name == name {
                return Ok(space);
            }
        }
        NotFoundSnafu {
            obj_type: "Space".to_string(),
            key: name.to_string(),
        }
        .fail()
    }

    /// Creates a request builder for updating an existing space.
    ///
    /// # Arguments
    /// * `space_id` - ID of the space to update
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use anytype::prelude::*;
    /// # async fn example(client: &AnytypeClient) -> Result<(), AnytypeError> {
    /// let space = client.update_space("space_id")
    ///     .name("New Name")
    ///     .update().await?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn update_space(&self, space_id: impl Into<String>) -> UpdateSpaceRequest {
        UpdateSpaceRequest::new(self.client.clone(), space_id, self.config.verify.clone())
    }

    /// Creates a request builder for listing spaces.
    ///
    /// # Example
    ///
    /// ```rust
    /// # use anytype::prelude::*;
    /// # async fn example() -> Result<(), AnytypeError> {
    /// #   let client = AnytypeClient::new("doc test")?;
    /// let spaces = client.spaces()
    ///     .limit(10)
    ///     .list().await?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn spaces(&self) -> ListSpacesRequest {
        ListSpacesRequest::new(self.client.clone(), self.cache.clone())
    }

    /// Creates a request builder for listing archived objects in a space.
    pub fn list_archived(&self, space_id: impl Into<String>) -> ListArchivedRequest<'_> {
        ListArchivedRequest::new(self, self.config.limits.clone(), space_id)
    }

    /// Counts archived objects in a space.
    pub async fn count_archived(&self, space_id: impl AsRef<str>) -> Result<u64> {
        let space_id = space_id.as_ref();
        let mut offset = 0_u32;
        let mut count = 0_u64;
        const BATCH: u32 = 500;

        loop {
            let page = self
                .list_archived(space_id)
                .limit(BATCH)
                .offset(offset)
                .list()
                .await?;

            count = count.saturating_add(page.items.len() as u64);
            if !page.pagination.has_more || page.items.is_empty() {
                break;
            }
            offset = offset.saturating_add(BATCH);
        }

        Ok(count)
    }

    /// Permanently deletes archived objects by object id in batches of 200.
    #[cfg(feature = "grpc")]
    pub async fn delete_archived(
        &self,
        space_id: impl AsRef<str>,
        object_ids: &[String],
    ) -> Result<u64> {
        const BATCH: usize = 200;
        self.config
            .limits
            .validate_id(space_id.as_ref(), "space_id")?;

        if object_ids.is_empty() {
            return Ok(0);
        }

        let grpc = self.grpc_client().await?;
        let mut commands = grpc.client_commands();
        let mut total_deleted = 0_u64;

        for chunk in object_ids.chunks(BATCH) {
            let request = list_delete::Request {
                object_ids: chunk.to_vec(),
            };
            let request = with_token_request(Request::new(request), grpc.token())?;
            let response = commands
                .object_list_delete(request)
                .await
                .map_err(grpc_status)?
                .into_inner();

            ensure_error_ok(response.error.as_ref(), "grpc object_list_delete")?;

            total_deleted = total_deleted.saturating_add(chunk.len() as u64);
        }

        Ok(total_deleted)
    }

    /// Deletes all archived objects in a space.
    ///
    /// Fetches up to 500 archived object IDs per round and deletes them in
    /// sub-batches of 200 via [`Self::delete_archived`].
    ///
    /// Between batches, waits 2 seconds to allow server-side state to settle.
    #[cfg(feature = "grpc")]
    pub async fn delete_all_archived(
        &self,
        space_id: impl AsRef<str>,
    ) -> Result<DeleteAllArchivedResult> {
        let space_id = space_id.as_ref();
        const BATCH: usize = 500;

        let mut total_deleted = 0_u64;
        let mut known_failed_ids: HashSet<String> = HashSet::new();
        loop {
            let page = self
                .list_archived(space_id)
                .limit(BATCH as u32)
                .offset(0)
                .list()
                .await?;

            if page.items.is_empty() {
                debug!(
                    space_id,
                    total_deleted, "delete_all_archived complete: no archived objects remain"
                );
                break;
            }

            let mut seen = HashSet::with_capacity(page.items.len());
            let mut ids: Vec<String> = Vec::with_capacity(page.items.len());
            for id in page.items.iter().map(|obj| obj.id.clone()) {
                if id.is_empty() {
                    continue;
                }
                if known_failed_ids.contains(&id) {
                    continue;
                }
                if seen.insert(id.clone()) {
                    ids.push(id);
                }
            }

            if ids.is_empty() {
                debug!(
                    space_id,
                    failed = known_failed_ids.len(),
                    "delete_all_archived: page contains only known failing ids; stopping"
                );
                break;
            }

            let result = delete_archived_best_effort(self, space_id, &ids).await?;
            total_deleted = total_deleted.saturating_add(result.deleted);
            for id in result.failed_ids {
                known_failed_ids.insert(id);
            }

            if result.deleted == 0 {
                debug!(
                    space_id,
                    failed = known_failed_ids.len(),
                    "delete_all_archived: no progress in this round; stopping"
                );
                break;
            }

            if total_deleted.is_multiple_of(500) {
                debug!(
                    space_id,
                    total_deleted, "delete_all_archived progress: deleted archived objects"
                );
            }

            tokio::time::sleep(Duration::from_secs(2)).await;
        }

        if !known_failed_ids.is_empty() {
            debug!(
                space_id,
                total_deleted,
                failed = known_failed_ids.len(),
                "delete_all_archived: some objects could not be deleted"
            );
        }

        Ok(DeleteAllArchivedResult {
            deleted: total_deleted,
            failed_ids: known_failed_ids.into_iter().collect(),
        })
    }

    /// Creates a request builder for backing up a space.
    ///
    /// Requires the `grpc` feature.
    ///
    /// # Arguments
    /// * `space_id` - ID of the space to back up
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use anytype::prelude::*;
    /// # async fn example(client: &AnytypeClient) -> Result<(), AnytypeError> {
    /// let result = client.backup_space("space_id")
    ///     .format(BackupExportFormat::Json)
    ///     .backup_dir("/tmp/backups")
    ///     .backup().await?;
    /// println!("Backup: {}", result.output_path.display());
    /// # Ok(())
    /// # }
    /// ```
    #[cfg(feature = "grpc")]
    pub fn backup_space(&self, space_id: impl Into<String>) -> BackupSpaceRequest<'_> {
        BackupSpaceRequest {
            client: self,
            space_id: space_id.into(),
            backup_dir: None,
            filename_prefix: None,
            object_ids: Vec::new(),
            format: BackupExportFormat::default(),
            zip: None,
            include_nested: None,
            include_files: None,
            is_json: None,
            include_archived: None,
            include_backlinks: None,
            include_space: None,
            md_include_properties_and_schema: None,
        }
    }
}

#[cfg(feature = "grpc")]
async fn search_archived_objects(
    client: &AnytypeClient,
    space_id: &str,
    limit: Option<u32>,
    offset: Option<u32>,
    type_ids: &[String],
) -> Result<PagedResult<Object>> {
    let limit = limit.unwrap_or(100);
    let offset = offset.unwrap_or(0);

    // Some anytype-heart builds use "isArchived", others may expose "archived".
    // Try the preferred key first, then fallback.
    let preferred = archived_search_request(space_id, "isArchived", limit, offset, type_ids);
    let response = match run_archived_search(client, preferred).await {
        Ok(response) => response,
        Err(err) if archived_relation_not_found(&err, "isArchived") => {
            let fallback = archived_search_request(space_id, "archived", limit, offset, type_ids);
            run_archived_search(client, fallback).await?
        }
        Err(err) => return Err(err),
    };

    let result_count = response.results.len();
    let items: Vec<Object> = response
        .results
        .into_iter()
        .filter_map(|result| archived_object_from_search_result(space_id, result))
        .collect();

    let has_more = result_count == limit as usize;
    let response = PaginatedResponse {
        items,
        pagination: PaginationMeta {
            has_more,
            limit,
            offset,
            total: offset as usize + result_count,
        },
    };
    Ok(PagedResult::from_response(response))
}

#[cfg(feature = "grpc")]
fn archived_search_request(
    space_id: &str,
    archived_relation_key: &str,
    limit: u32,
    offset: u32,
    type_ids: &[String],
) -> search_with_meta::Request {
    let mut filters = vec![dataview_filter_checkbox_equal(archived_relation_key, true)];
    if !type_ids.is_empty() {
        filters.push(dataview_filter_type_in(type_ids));
    }

    #[allow(clippy::cast_possible_wrap, clippy::cast_possible_truncation)]
    search_with_meta::Request {
        space_id: space_id.to_string(),
        filters,
        sorts: Vec::new(),
        full_text: String::new(),
        offset: offset as i32,
        limit: limit as i32,
        object_type_filter: Vec::new(),
        keys: Vec::new(),
        return_meta: false,
        return_meta_relation_details: false,
        return_html_highlights_instead_of_ranges: false,
    }
}

#[cfg(feature = "grpc")]
async fn run_archived_search(
    client: &AnytypeClient,
    request: search_with_meta::Request,
) -> Result<search_with_meta::Response> {
    let grpc = client.grpc_client().await?;
    let mut commands = grpc.client_commands();
    let request = with_token_request(Request::new(request), grpc.token())?;
    let response = commands
        .object_search_with_meta(request)
        .await
        .map_err(grpc_status)?
        .into_inner();

    ensure_error_ok(response.error.as_ref(), "grpc archived search")?;

    Ok(response)
}

#[cfg(feature = "grpc")]
fn archived_relation_not_found(err: &AnytypeError, key: &str) -> bool {
    match err {
        AnytypeError::Other { message } => {
            message.contains("failed to resolve relation")
                && (message.contains(&format!("\"{key}\"")) || message.contains(key))
        }
        _ => false,
    }
}

#[cfg(feature = "grpc")]
fn dataview_filter_checkbox_equal(
    key: &str,
    value: bool,
) -> model::block::content::dataview::Filter {
    model::block::content::dataview::Filter {
        id: String::new(),
        operator: model::block::content::dataview::filter::Operator::No as i32,
        relation_key: key.to_string(),
        relation_property: String::new(),
        condition: model::block::content::dataview::filter::Condition::Equal as i32,
        value: Some(Value {
            kind: Some(prost_types::value::Kind::BoolValue(value)),
        }),
        quick_option: model::block::content::dataview::filter::QuickOption::ExactDate as i32,
        format: 0,
        include_time: false,
        nested_filters: Vec::new(),
    }
}

#[cfg(feature = "grpc")]
fn dataview_filter_type_in(type_ids: &[String]) -> model::block::content::dataview::Filter {
    model::block::content::dataview::Filter {
        id: String::new(),
        operator: model::block::content::dataview::filter::Operator::No as i32,
        relation_key: "type".to_string(),
        relation_property: String::new(),
        condition: model::block::content::dataview::filter::Condition::In as i32,
        value: Some(Value {
            kind: Some(prost_types::value::Kind::ListValue(ListValue {
                values: type_ids
                    .iter()
                    .map(|id| Value {
                        kind: Some(prost_types::value::Kind::StringValue(id.clone())),
                    })
                    .collect(),
            })),
        }),
        quick_option: model::block::content::dataview::filter::QuickOption::ExactDate as i32,
        format: 0,
        include_time: false,
        nested_filters: Vec::new(),
    }
}

#[cfg(feature = "grpc")]
fn archived_object_from_search_result(
    space_id: &str,
    result: model::search::Result,
) -> Option<Object> {
    let details = result.details.unwrap_or_default();
    let id = normalized_search_result_id(result.object_id, &details)?;
    let archived = struct_bool_field(&details, "isArchived")
        .or_else(|| struct_bool_field(&details, "archived"))
        .unwrap_or(true);
    let name = struct_string_field(&details, "name");

    Some(Object {
        archived,
        icon: None,
        id,
        layout: ObjectLayout::default(),
        markdown: None,
        name,
        object: DataModel::Object,
        properties: Vec::new(),
        snippet: None,
        space_id: space_id.to_string(),
        r#type: None,
    })
}

#[cfg(feature = "grpc")]
fn struct_bool_field(details: &prost_types::Struct, key: &str) -> Option<bool> {
    details
        .fields
        .get(key)
        .and_then(|value| value.kind.as_ref())
        .and_then(|kind| match kind {
            prost_types::value::Kind::BoolValue(value) => Some(*value),
            _ => None,
        })
}

#[cfg(feature = "grpc")]
fn struct_string_field(details: &prost_types::Struct, key: &str) -> Option<String> {
    details
        .fields
        .get(key)
        .and_then(|value| value.kind.as_ref())
        .and_then(|kind| match kind {
            prost_types::value::Kind::StringValue(value) => Some(value.clone()),
            _ => None,
        })
}

#[cfg(feature = "grpc")]
fn normalized_search_result_id(object_id: String, details: &prost_types::Struct) -> Option<String> {
    if !object_id.is_empty() {
        return Some(object_id);
    }
    let fallback = struct_string_field(details, "id")?;
    if fallback.is_empty() {
        None
    } else {
        Some(fallback)
    }
}

#[cfg(feature = "grpc")]
#[derive(Debug)]
struct DeleteBestEffortResult {
    deleted: u64,
    failed_ids: Vec<String>,
}

#[cfg(feature = "grpc")]
async fn delete_archived_best_effort(
    client: &AnytypeClient,
    space_id: &str,
    ids: &[String],
) -> Result<DeleteBestEffortResult> {
    let mut pending: Vec<Vec<String>> = ids.chunks(500).map(|chunk| chunk.to_vec()).collect();
    let mut deleted = 0_u64;
    let mut failed_ids = Vec::new();

    while let Some(batch) = pending.pop() {
        match client.delete_archived(space_id, &batch).await {
            Ok(num_deleted) => {
                deleted = deleted.saturating_add(num_deleted);
            }
            Err(err) => {
                if batch.len() == 1 {
                    debug!(
                        space_id,
                        object_id = batch[0].as_str(),
                        error = %err,
                        "delete_archived_best_effort: skipping undeletable archived object id"
                    );
                    failed_ids.push(batch[0].clone());
                    continue;
                }

                let mid = batch.len() / 2;
                pending.push(batch[mid..].to_vec());
                pending.push(batch[..mid].to_vec());
            }
        }
    }

    Ok(DeleteBestEffortResult {
        deleted,
        failed_ids,
    })
}

// ============================================================================
// TESTS
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_space_model_default() {
        let model: SpaceModel = SpaceModel::default();
        assert_eq!(model, SpaceModel::Space);
    }

    #[test]
    fn test_space_model_display() {
        assert_eq!(SpaceModel::Space.to_string(), "space");
        assert_eq!(SpaceModel::Chat.to_string(), "chat");
    }

    #[test]
    fn test_space_model_from_string() {
        use std::str::FromStr;
        assert_eq!(SpaceModel::from_str("space").unwrap(), SpaceModel::Space);
        assert_eq!(SpaceModel::from_str("chat").unwrap(), SpaceModel::Chat);
    }

    #[test]
    fn test_create_space_request_body_serialization() {
        let body = CreateSpaceRequestBody {
            name: "Test Space".to_string(),
            description: Some("A test space".to_string()),
        };

        let json = serde_json::to_string(&body).unwrap();
        assert!(json.contains("\"name\":\"Test Space\""));
        assert!(json.contains("\"description\":\"A test space\""));
    }

    #[test]
    fn test_create_space_request_body_no_description() {
        let body = CreateSpaceRequestBody {
            name: "Test Space".to_string(),
            description: None,
        };

        let json = serde_json::to_string(&body).unwrap();
        assert!(json.contains("\"name\":\"Test Space\""));
        assert!(!json.contains("description"));
    }

    #[test]
    fn test_update_space_request_body_empty() {
        let body = UpdateSpaceRequestBody::default();
        let json = serde_json::to_string(&body).unwrap();
        assert_eq!(json, "{}");
    }

    #[test]
    fn test_space_is_chat() {
        let space = Space {
            id: "test".to_string(),
            name: "Test".to_string(),
            object: SpaceModel::Chat,
            description: None,
            icon: None,
            gateway_url: None,
            network_id: None,
        };

        assert!(space.is_chat());
        assert!(!space.is_space());
    }

    #[test]
    fn test_space_is_space() {
        let space = Space {
            id: "test".to_string(),
            name: "Test".to_string(),
            object: SpaceModel::Space,
            description: None,
            icon: None,
            gateway_url: None,
            network_id: None,
        };

        assert!(space.is_space());
        assert!(!space.is_chat());
    }
}
