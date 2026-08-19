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
//! - [`create_chat_space`](AnytypeClient::create_chat_space) - create a chat space
//! - [`delete_space`](AnytypeClient::delete_space) - permanently delete a space
//! - [`list_space_invites`](AnytypeClient::list_space_invites) - list active invitations
//! - [`create_space_invite`](AnytypeClient::create_space_invite) - create an invitation
//! - [`revoke_space_invite`](AnytypeClient::revoke_space_invite) - revoke an invitation
//! - [`enable_space_sharing`](AnytypeClient::enable_space_sharing) and
//!   [`disable_space_sharing`](AnytypeClient::disable_space_sharing) - control sharing
//! - [`backup_space`](AnytypeClient::backup_space) - back up a space (gRPC-backed; requires gRPC credentials)
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
//! - [`BackupSpaceRequest`] - Builder for backing up a space (gRPC-backed; requires gRPC credentials)
//! - [`BackupExportFormat`] - Export format for backups (gRPC-backed)
//! - [`SpaceInvite`] - A generated space invitation
//! - [`SpaceInviteType`] - Invitation approval and guest mode
//! - [`SpaceInvitePermission`] - Access granted by an invitation

use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;

use futures::StreamExt;
use serde::{Deserialize, Serialize};
use snafu::prelude::*;
use tracing::debug;

use std::path::{Path, PathBuf};
use std::time::Duration;

use anytype_rpc::anytype::rpc;
use anytype_rpc::anytype::rpc::object::list_delete;
pub use anytype_rpc::backup::SpaceBackupResult;
use anytype_rpc::backup::{ExportFormat, SpaceBackupOptions};
use anytype_rpc::{anytype::rpc::object::search_with_meta, model};
use prost_types::{ListValue, Value};
use tonic::Request;

use crate::grpc_util::{ensure_error_ok, grpc_status, with_token_request};
use crate::{
    Result,
    cache::AnytypeCache,
    client::AnytypeClient,
    error::AnytypeError,
    filters::{Query, QueryWithFilters},
    http_client::{GetPaged, HttpClient},
    prelude::*,
    verify::{VerifyConfig, VerifyPolicy, resolve_verify, verify_available},
};

const ARCHIVED_PAGE_DEFAULT_LIMIT: u32 = 100;
const SPACE_SHARING_ADMISSION_ATTEMPTS: usize = 60;
const SPACE_SHARING_ADMISSION_DELAY: Duration = Duration::from_millis(500);
const ARCHIVED_COUNT_PAGE_SIZE: u32 = 500;
const SPACE_UX_TYPE_KEY: &str = "spaceUxType";

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
    #[serde(alias = "anytype.space")]
    Space,
    /// Chat-based space for messaging
    #[serde(alias = "anytype.chatspace", alias = "chatspace")]
    Chat,
    /// Direct one-to-one messaging space
    #[serde(alias = "anytype.onetoone", alias = "onetoone")]
    OneToOne,
    /// Technical/system space used for account bookkeeping (not user-facing)
    #[serde(alias = "anytype.techspace", alias = "techspace")]
    TechSpace,
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

    /// Description of the space.
    ///
    /// Current servers (anytype-cli v0.3.6, API 2025-11-08) always return this
    /// field as a string: a space that has no description — whether it never
    /// had one or was cleared with
    /// [`UpdateSpaceRequest::clear_description`] — reports `Some("")`, never
    /// `null` or an absent key. `None` can only arise from a server that omits
    /// the field; treat `None` and `Some("")` identically, or use
    /// [`Space::description_text`].
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
    /// Returns the description when the space has one.
    ///
    /// Normalizes the two wire representations of "no description" — an empty
    /// string (what current servers return for never-set and cleared
    /// descriptions alike) and an omitted field — to `None`.
    #[must_use]
    pub fn description_text(&self) -> Option<&str> {
        self.description
            .as_deref()
            .filter(|description| !description.is_empty())
    }
}

/// The kind of invitation generated for a space.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpaceInviteType {
    /// A member invitation which may require approval.
    Member,
    /// An invitation for a guest account.
    Guest,
    /// A member invitation that does not require approval.
    AutoApprove,
}

impl SpaceInviteType {
    fn as_rpc(self) -> i32 {
        match self {
            Self::Member => model::InviteType::Member as i32,
            Self::Guest => model::InviteType::Guest as i32,
            Self::AutoApprove => model::InviteType::WithoutApprove as i32,
        }
    }
}

/// Permissions granted by a generated member invitation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpaceInvitePermission {
    /// Read-only access.
    Reader,
    /// Read and write access.
    Writer,
    /// Owner access.
    Owner,
}

impl SpaceInvitePermission {
    fn as_rpc(self) -> i32 {
        match self {
            Self::Reader => model::ParticipantPermissions::Reader as i32,
            Self::Writer => model::ParticipantPermissions::Writer as i32,
            Self::Owner => model::ParticipantPermissions::Owner as i32,
        }
    }
}

/// A currently active invitation for a space.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SpaceInvite {
    /// Human-readable invitation kind.
    #[serde(rename = "type")]
    pub invite_type: String,
    /// Human-readable permissions, when the invitation has them.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub permissions: Option<String>,
    /// Content identifier for the invitation file.
    pub cid: String,
    /// Encryption key for the invitation file.
    pub key: String,
    /// Shareable invitation URL.
    pub url: String,
}

fn space_rpc_error(action: &str, code: i32, description: &str) -> AnytypeError {
    AnytypeError::Other {
        message: format!("{action} failed: {description} (code {code})"),
    }
}

fn invite_type_name(value: i32) -> String {
    match value {
        value if value == model::InviteType::Member as i32 => "member".to_owned(),
        value if value == model::InviteType::Guest as i32 => "guest".to_owned(),
        value if value == model::InviteType::WithoutApprove as i32 => "auto-approve".to_owned(),
        value => format!("unknown({value})"),
    }
}

fn invite_permissions_name(value: i32) -> String {
    match value {
        value if value == model::ParticipantPermissions::Reader as i32 => "reader".to_owned(),
        value if value == model::ParticipantPermissions::Writer as i32 => "writer".to_owned(),
        value if value == model::ParticipantPermissions::Owner as i32 => "owner".to_owned(),
        value => format!("unknown({value})"),
    }
}

fn invite_url(cid: &str, key: &str) -> String {
    format!("https://invite.any.coop/{cid}#{key}")
}

fn is_no_active_member_invite(code: i32) -> bool {
    code == rpc::space::invite_get_current::response::error::Code::NoActiveInvite as i32
}

fn is_guest_invite_unavailable(code: i32) -> bool {
    code == rpc::space::invite_get_guest::response::error::Code::InvalidSpaceType as i32
}

fn space_sharing_admission_is_pending(code: i32) -> bool {
    code == rpc::space::make_shareable::response::error::Code::NoSuchSpace as i32
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

    /// Retrieves the exact space without consulting or populating the cache.
    ///
    /// This is intended for bounded preflight and read-after-write workflows
    /// that must observe current server state even when the client's ordinary
    /// space cache has already been populated.
    ///
    /// # Errors
    ///
    /// Returns [`AnytypeError::NotFound`] when the server reports no such
    /// space, or [`AnytypeError::Other`] when a malformed response identifies
    /// a different space than the one requested.
    pub async fn get_direct(self) -> Result<Space> {
        let response: SpaceResponse = self
            .client
            .get_request(
                &format!("/v1/spaces/{}", self.space_id),
                QueryWithFilters::default(),
            )
            .await?;
        exact_space_response(response, &self.space_id)
    }
}

fn exact_space_response(response: SpaceResponse, expected_id: &str) -> Result<Space> {
    ensure!(
        response.space.id == expected_id,
        OtherSnafu {
            message: "REST space response id did not match the requested id".to_owned(),
        }
    );
    Ok(response.space)
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
    limits: ValidationLimits,
    name: String,
    description: Option<String>,
    verify_policy: VerifyPolicy,
    verify_config: Option<VerifyConfig>,
}

impl NewSpaceRequest {
    /// Creates a new `NewSpaceRequest`.
    pub(crate) fn new(
        client: Arc<HttpClient>,
        limits: ValidationLimits,
        name: impl Into<String>,
        verify_config: Option<VerifyConfig>,
    ) -> Self {
        Self {
            client,
            limits,
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
    /// - [`AnytypeError::Validation`] if the name is empty or exceeds the configured limit
    pub async fn create(self) -> Result<Space> {
        self.limits.validate_name(&self.name, "space")?;

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

    /// Replaces the space description.
    ///
    /// Omitting this call leaves the current description untouched. An empty
    /// string clears the description exactly like
    /// [`clear_description`](Self::clear_description); prefer that method to
    /// make the intent explicit.
    ///
    /// # Arguments
    /// * `description` - New description text for the space
    #[must_use]
    pub fn description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    /// Clears the space description.
    ///
    /// The request carries `"description": ""`, the only wire form that clears
    /// on current servers (a JSON `null` is silently ignored upstream, so it is
    /// never sent). The response and later reads report the cleared
    /// description as `Some("")` — identical to a space that never had one.
    /// This counts as an updated field for [`update`](Self::update).
    #[must_use]
    pub fn clear_description(mut self) -> Self {
        self.description = Some(String::new());
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

    /// Validates the builder and produces the wire body: fields that were not
    /// set are omitted (no change), while a cleared description is sent as an
    /// empty string.
    fn request_body(&self) -> Result<UpdateSpaceRequestBody> {
        // Check that at least one field is being updated
        ensure!(
            self.name.is_some() || self.description.is_some(),
            ValidationSnafu {
                message:
                    "update_space: must set at least one field to update (name or description)"
                        .to_string(),
            }
        );
        Ok(UpdateSpaceRequestBody {
            name: self.name.clone(),
            description: self.description.clone(),
        })
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
        let request_body = self.request_body()?;

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
    response_limit_bytes: Option<u64>,
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
            response_limit_bytes: None,
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

    /// Sets a finite response-body ceiling applied independently to every page.
    #[must_use]
    pub const fn response_limit_bytes(mut self, limit: u64) -> Self {
        self.response_limit_bytes = Some(limit);
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

        match self.response_limit_bytes {
            Some(limit) => {
                self.client
                    .get_request_paged_with_limit("/v1/spaces", query, limit)
                    .await
            }
            None => self.client.get_request_paged("/v1/spaces", query).await,
        }
    }
}

/// Result of [`AnytypeClient::delete_all_archived`].
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
///
/// The gRPC search response validates its `SetString` type IDs, but does not
/// include the type key or display metadata needed for [`Object`] `r#type`.
/// Archived results therefore leave that field as `None`.
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
    ///
    /// [`Self::list`] rejects values outside `1..=1000` before opening a gRPC
    /// connection.
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

    /// Filters archived objects by type IDs.
    #[must_use]
    pub fn types(mut self, type_ids: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.type_ids = type_ids.into_iter().map(Into::into).collect();
        self
    }

    /// Executes the archived-list request.
    pub async fn list(self) -> Result<PagedResult<Object>> {
        self.limits.validate_id(&self.space_id, "space_id")?;
        validate_archived_page_input(self.limit, self.offset)?;
        for type_id in &self.type_ids {
            self.limits.validate_id(type_id, "type_id")?;
        }
        search_archived_objects(
            self.client,
            &self.space_id,
            self.limit,
            self.offset,
            &self.type_ids,
        )
        .await
    }
}

/// Export format for space backups.
///
// This exposes a subset of the internal export formats that are suitable for backups.
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
        NewSpaceRequest::new(
            self.client.clone(),
            self.config.limits.clone(),
            name,
            self.config.verify.clone(),
        )
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

    /// Creates a chat space through the authenticated gRPC workspace API.
    ///
    /// The ordinary [`Self::new_space`] builder creates regular spaces through
    /// the REST API. Chat-space creation is currently only exposed by gRPC.
    ///
    /// # Errors
    ///
    /// Returns an error when the name is invalid, gRPC credentials are absent,
    /// or the workspace service rejects the request.
    pub async fn create_chat_space(&self, name: impl Into<String>) -> Result<Space> {
        let name = name.into();
        self.config.limits.validate_name(&name, "space")?;

        let grpc = self.grpc_client().await?;
        let request = chat_space_create_request(name.clone());
        let request = with_token_request(Request::new(request), grpc.token())?;
        let response = grpc
            .client_commands()
            .workspace_create(request)
            .await
            .map_err(grpc_status)?
            .into_inner();

        if let Some(error) = response.error.as_ref().filter(|error| error.code != 0) {
            return Err(space_rpc_error(
                "workspace create",
                error.code,
                &error.description,
            ));
        }
        if response.space_id.is_empty() {
            return Err(AnytypeError::Other {
                message: "workspace create returned an empty space id".to_owned(),
            });
        }

        self.cache.clear_spaces();
        Ok(Space {
            id: response.space_id,
            name,
            object: SpaceModel::Chat,
            description: None,
            icon: None,
            gateway_url: None,
            network_id: None,
        })
    }

    /// Permanently deletes a space through the authenticated gRPC space API.
    ///
    /// This operation is intentionally not exposed through the ordinary
    /// `SpaceRequest` builder because it is irreversible and requires an
    /// explicit caller-side confirmation policy.
    ///
    /// # Errors
    ///
    /// Returns an error when the id is empty, gRPC credentials are absent, or
    /// the space service rejects the request.
    pub async fn delete_space(&self, space_id: impl AsRef<str>) -> Result<()> {
        let space_id = space_id.as_ref();
        self.config.limits.validate_id(space_id, "space_id")?;

        let grpc = self.grpc_client().await?;
        let request = rpc::space::delete::Request {
            space_id: space_id.to_owned(),
        };
        let request = with_token_request(Request::new(request), grpc.token())?;
        let response = grpc
            .client_commands()
            .space_delete(request)
            .await
            .map_err(grpc_status)?
            .into_inner();

        if let Some(error) = response.error.as_ref().filter(|error| error.code != 0) {
            return Err(space_rpc_error(
                "space delete",
                error.code,
                &error.description,
            ));
        }
        self.cache.clear_spaces();
        Ok(())
    }

    /// Lists active member and guest invitations for a space.
    ///
    /// A space with no active invitation, or a regular space without a guest
    /// invitation, returns an empty entry for that invitation kind rather than
    /// an error.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid credentials, transport failures, or an
    /// upstream error other than the documented no-invite conditions.
    pub async fn list_space_invites(&self, space_id: impl AsRef<str>) -> Result<Vec<SpaceInvite>> {
        let space_id = space_id.as_ref();
        self.config.limits.validate_id(space_id, "space_id")?;

        let grpc = self.grpc_client().await?;
        let mut commands = grpc.client_commands();
        let mut invites = Vec::new();

        let current_request = rpc::space::invite_get_current::Request {
            space_id: space_id.to_owned(),
        };
        let current_request = with_token_request(Request::new(current_request), grpc.token())?;
        let current = commands
            .space_invite_get_current(current_request)
            .await
            .map_err(grpc_status)?
            .into_inner();
        if let Some(error) = current.error.as_ref().filter(|error| error.code != 0) {
            if !is_no_active_member_invite(error.code) {
                return Err(space_rpc_error(
                    "space invite get current",
                    error.code,
                    &error.description,
                ));
            }
        } else if !current.invite_cid.is_empty() {
            invites.push(SpaceInvite {
                invite_type: invite_type_name(current.invite_type),
                permissions: Some(invite_permissions_name(current.permissions)),
                url: invite_url(&current.invite_cid, &current.invite_file_key),
                cid: current.invite_cid,
                key: current.invite_file_key,
            });
        }

        let guest_request = rpc::space::invite_get_guest::Request {
            space_id: space_id.to_owned(),
        };
        let guest_request = with_token_request(Request::new(guest_request), grpc.token())?;
        let guest = commands
            .space_invite_get_guest(guest_request)
            .await
            .map_err(grpc_status)?
            .into_inner();
        if let Some(error) = guest.error.as_ref().filter(|error| error.code != 0) {
            if !is_guest_invite_unavailable(error.code) {
                return Err(space_rpc_error(
                    "space invite get guest",
                    error.code,
                    &error.description,
                ));
            }
        } else if !guest.invite_cid.is_empty() {
            invites.push(SpaceInvite {
                invite_type: "guest".to_owned(),
                permissions: None,
                url: invite_url(&guest.invite_cid, &guest.invite_file_key),
                cid: guest.invite_cid,
                key: guest.invite_file_key,
            });
        }

        Ok(invites)
    }

    /// Generates a new member or guest invitation for a space.
    ///
    /// # Errors
    ///
    /// Returns an error when the request is rejected, credentials are absent,
    /// or the generated invitation is incomplete.
    pub async fn create_space_invite(
        &self,
        space_id: impl AsRef<str>,
        invite_type: SpaceInviteType,
        permissions: SpaceInvitePermission,
    ) -> Result<SpaceInvite> {
        let space_id = space_id.as_ref();
        self.config.limits.validate_id(space_id, "space_id")?;

        let grpc = self.grpc_client().await?;
        let request = rpc::space::invite_generate::Request {
            space_id: space_id.to_owned(),
            invite_type: invite_type.as_rpc(),
            permissions: permissions.as_rpc(),
        };
        let request = with_token_request(Request::new(request), grpc.token())?;
        let response = grpc
            .client_commands()
            .space_invite_generate(request)
            .await
            .map_err(grpc_status)?
            .into_inner();

        if let Some(error) = response.error.as_ref().filter(|error| error.code != 0) {
            return Err(space_rpc_error(
                "space invite generate",
                error.code,
                &error.description,
            ));
        }
        if response.invite_cid.is_empty() || response.invite_file_key.is_empty() {
            return Err(AnytypeError::Other {
                message: "space invite generate returned incomplete invitation data".to_owned(),
            });
        }

        Ok(SpaceInvite {
            invite_type: invite_type_name(response.invite_type),
            permissions: Some(invite_permissions_name(response.permissions)),
            url: invite_url(&response.invite_cid, &response.invite_file_key),
            cid: response.invite_cid,
            key: response.invite_file_key,
        })
    }

    /// Revokes the active invitation for a space.
    ///
    /// # Errors
    ///
    /// Returns an error when the request is rejected, credentials are absent,
    /// or the space has no revocable invitation.
    pub async fn revoke_space_invite(&self, space_id: impl AsRef<str>) -> Result<()> {
        let space_id = space_id.as_ref();
        self.config.limits.validate_id(space_id, "space_id")?;

        let grpc = self.grpc_client().await?;
        let request = rpc::space::invite_revoke::Request {
            space_id: space_id.to_owned(),
        };
        let request = with_token_request(Request::new(request), grpc.token())?;
        let response = grpc
            .client_commands()
            .space_invite_revoke(request)
            .await
            .map_err(grpc_status)?
            .into_inner();

        if let Some(error) = response.error.as_ref().filter(|error| error.code != 0) {
            return Err(space_rpc_error(
                "space invite revoke",
                error.code,
                &error.description,
            ));
        }
        Ok(())
    }

    /// Enables sharing for a space.
    ///
    /// A newly REST-created space can briefly be absent from Heart's sharing
    /// service. This method retries only that definitive `NO_SUCH_SPACE`
    /// response within a bounded admission window.
    ///
    /// # Errors
    ///
    /// Returns an error when the request is rejected or credentials are absent.
    pub async fn enable_space_sharing(&self, space_id: impl AsRef<str>) -> Result<()> {
        self.set_space_sharing(space_id.as_ref(), true).await
    }

    /// Disables sharing for a space.
    ///
    /// # Errors
    ///
    /// Returns an error when the request is rejected or credentials are absent.
    pub async fn disable_space_sharing(&self, space_id: impl AsRef<str>) -> Result<()> {
        self.set_space_sharing(space_id.as_ref(), false).await
    }

    async fn set_space_sharing(&self, space_id: &str, enabled: bool) -> Result<()> {
        self.config.limits.validate_id(space_id, "space_id")?;

        let grpc = self.grpc_client().await?;
        if enabled {
            let mut attempts_remaining = SPACE_SHARING_ADMISSION_ATTEMPTS;
            loop {
                let request = rpc::space::make_shareable::Request {
                    space_id: space_id.to_owned(),
                };
                let request = with_token_request(Request::new(request), grpc.token())?;
                let response = grpc
                    .client_commands()
                    .space_make_shareable(request)
                    .await
                    .map_err(grpc_status)?
                    .into_inner();
                if let Some(error) = response.error.as_ref().filter(|error| error.code != 0) {
                    attempts_remaining = attempts_remaining.saturating_sub(1);
                    if space_sharing_admission_is_pending(error.code) && attempts_remaining > 0 {
                        tokio::time::sleep(SPACE_SHARING_ADMISSION_DELAY).await;
                        continue;
                    }
                    return Err(space_rpc_error(
                        "space sharing enable",
                        error.code,
                        &error.description,
                    ));
                }
                return Ok(());
            }
        } else {
            let request = rpc::space::stop_sharing::Request {
                space_id: space_id.to_owned(),
            };
            let request = with_token_request(Request::new(request), grpc.token())?;
            let response = grpc
                .client_commands()
                .space_stop_sharing(request)
                .await
                .map_err(grpc_status)?
                .into_inner();
            if let Some(error) = response.error.as_ref().filter(|error| error.code != 0) {
                return Err(space_rpc_error(
                    "space sharing disable",
                    error.code,
                    &error.description,
                ));
            }
        }
        Ok(())
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

        loop {
            let page = self
                .list_archived(space_id)
                .limit(ARCHIVED_COUNT_PAGE_SIZE)
                .offset(offset)
                .list()
                .await?;

            count = count.saturating_add(page.items.len() as u64);
            if !page.pagination.has_more || page.items.is_empty() {
                break;
            }
            offset = offset.saturating_add(ARCHIVED_COUNT_PAGE_SIZE);
        }

        Ok(count)
    }

    /// Counts archived objects in a space within an explicit page budget.
    ///
    /// Each page requests at most 500 rows. `max_pages` must be nonzero. The
    /// method uses at most `2 * max_pages` gRPC requests because each page can
    /// retry once with the legacy archive relation key. It returns an error when
    /// the budget cannot prove that the final full page is exhausted, which
    /// prevents a truncated count from being reported as exact.
    pub async fn count_archived_bounded(
        &self,
        space_id: impl AsRef<str>,
        max_pages: u32,
    ) -> Result<u64> {
        let space_id = space_id.as_ref();
        self.config.limits.validate_id(space_id, "space_id")?;
        if max_pages == 0 {
            return Err(archived_validation_error(
                "archived count page budget must be at least one",
            ));
        }

        let mut state = ArchivedCountState::new(max_pages);

        loop {
            let page = self
                .list_archived(space_id)
                .limit(ARCHIVED_COUNT_PAGE_SIZE)
                .offset(state.offset)
                .list()
                .await?;

            if let Some(count) = state.record_page(page.items.len())? {
                return Ok(count);
            }
        }
    }

    /// Permanently deletes archived objects by object id in batches of 200.
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
    /// This method is gRPC-backed: it requires gRPC credentials in the
    /// keystore. There is no `grpc` Cargo feature to enable.
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

fn chat_space_create_request(name: String) -> rpc::workspace::create::Request {
    let mut fields = BTreeMap::new();
    fields.insert(
        "name".to_owned(),
        Value {
            kind: Some(prost_types::value::Kind::StringValue(name)),
        },
    );
    fields.insert(
        SPACE_UX_TYPE_KEY.to_owned(),
        Value {
            kind: Some(prost_types::value::Kind::NumberValue(f64::from(
                model::SpaceUxType::Chat as i32,
            ))),
        },
    );
    rpc::workspace::create::Request {
        details: Some(prost_types::Struct { fields }),
        use_case: rpc::object::import_use_case::request::UseCase::None as i32,
        // Retain the legacy protocol flag alongside the detail. Heart 0.50.10
        // selects chat UX from `spaceUxType` during workspace creation.
        with_chat: true,
    }
}

async fn search_archived_objects(
    client: &AnytypeClient,
    space_id: &str,
    limit: Option<u32>,
    offset: Option<u32>,
    type_ids: &[String],
) -> Result<PagedResult<Object>> {
    validate_archived_page_input(limit, offset)?;
    let limit = limit.unwrap_or(ARCHIVED_PAGE_DEFAULT_LIMIT);
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
    validate_archived_page_result_count(result_count, limit)?;
    let items = response
        .results
        .into_iter()
        .map(|result| archived_object_from_search_result(space_id, result))
        .collect::<Result<Vec<_>>>()?;

    let has_more = result_count == limit as usize;
    let total = (offset as usize)
        .checked_add(result_count)
        .ok_or_else(|| archived_page_error("archived search pagination total overflow"))?;
    let response = PaginatedResponse {
        items,
        pagination: PaginationMeta {
            has_more,
            limit,
            offset,
            // SearchWithMeta does not expose upstream pagination metadata. This
            // is the number of rows observed through this page, not a total.
            total,
        },
    };
    Ok(PagedResult::from_response(response))
}

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

fn archived_relation_not_found(err: &AnytypeError, key: &str) -> bool {
    match err {
        AnytypeError::Other { message } => {
            message.contains("failed to resolve relation")
                && (message.contains(&format!("\"{key}\"")) || message.contains(key))
        }
        _ => false,
    }
}

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

fn archived_object_from_search_result(
    space_id: &str,
    result: model::search::Result,
) -> Result<Object> {
    let details = result.details.unwrap_or_default();
    let id = normalized_search_result_id(result.object_id, &details)?;
    crate::validation::ValidationLimits::default()
        .validate_id(&id, "archived search result object_id")?;
    let archived = struct_bool_field(&details, "isArchived")
        .or_else(|| struct_bool_field(&details, "archived"))
        .unwrap_or(true);
    let name = struct_string_field(&details, "name");

    Ok(Object {
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
        r#type: archived_type_from_search_details(&details)?,
    })
}

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

fn normalized_search_result_id(object_id: String, details: &prost_types::Struct) -> Result<String> {
    if !object_id.is_empty() {
        return Ok(object_id);
    }
    let fallback = struct_string_field(details, "id")
        .ok_or_else(|| archived_page_error("archived search result has no object id"))?;
    if fallback.is_empty() {
        Err(archived_page_error(
            "archived search result has an empty object id",
        ))
    } else {
        Ok(fallback)
    }
}

fn archived_type_from_search_details(details: &prost_types::Struct) -> Result<Option<Type>> {
    let Some(type_value) = details.fields.get("type") else {
        return Ok(None);
    };
    let id = archived_type_id_from_value(type_value)?;
    crate::validation::ValidationLimits::default()
        .validate_id(&id, "archived search type metadata id")?;

    // SearchWithMeta exposes the type relation as a SetString containing only
    // the type ID. A Type additionally requires a valid key, so returning a
    // partial Type would violate the public Object contract.
    Ok(None)
}

fn archived_type_id_from_value(value: &Value) -> Result<String> {
    let Some(kind) = value.kind.as_ref() else {
        return Err(archived_page_error(
            "archived search type metadata has no value",
        ));
    };
    let type_id = match kind {
        prost_types::value::Kind::ListValue(values) => match values.values.as_slice() {
            [
                Value {
                    kind: Some(prost_types::value::Kind::StringValue(type_id)),
                },
            ] => type_id.clone(),
            _ => {
                return Err(archived_page_error(
                    "archived search type metadata must contain exactly one type id",
                ));
            }
        },
        // Retain compatibility with heart versions that encode the singleton
        // SetString relation directly rather than as a protobuf ListValue.
        prost_types::value::Kind::StringValue(type_id) => type_id.clone(),
        _ => {
            return Err(archived_page_error(
                "archived search type metadata has an invalid value",
            ));
        }
    };
    if type_id.is_empty() {
        return Err(archived_page_error(
            "archived search type metadata has an empty type id",
        ));
    }
    Ok(type_id)
}

fn validate_archived_page_input(limit: Option<u32>, offset: Option<u32>) -> Result<()> {
    if limit.is_some_and(|value| value == 0 || value > crate::config::MAX_PAGINATION_LIMIT) {
        return Err(archived_validation_error(&format!(
            "archived search limit must be between 1 and {}",
            crate::config::MAX_PAGINATION_LIMIT
        )));
    }
    if offset.is_some_and(|value| value > i32::MAX as u32) {
        return Err(archived_validation_error(
            "archived search offset exceeds the gRPC i32 range",
        ));
    }
    Ok(())
}

fn archived_count_continuation_offset(
    offset: u32,
    returned: usize,
    page_size: u32,
    remaining_pages: u32,
) -> Result<Option<u32>> {
    validate_archived_page_result_count(returned, page_size)?;
    if returned < page_size as usize {
        return Ok(None);
    }
    if remaining_pages == 0 {
        return Err(archived_page_error(
            "archived count page budget cannot prove that a full final page is exhausted",
        ));
    }
    offset
        .checked_add(page_size)
        .map(Some)
        .ok_or_else(|| archived_page_error("archived count offset overflow"))
}

#[derive(Debug)]
struct ArchivedCountState {
    count: u64,
    offset: u32,
    logical_pages: u32,
    max_pages: u32,
}

impl ArchivedCountState {
    fn new(max_pages: u32) -> Self {
        Self {
            count: 0,
            offset: 0,
            logical_pages: 0,
            max_pages,
        }
    }

    fn record_page(&mut self, returned: usize) -> Result<Option<u64>> {
        if self.logical_pages >= self.max_pages {
            return Err(archived_page_error(
                "archived count page budget was exhausted before reading a page",
            ));
        }
        self.count = self.count.saturating_add(returned as u64);
        self.logical_pages += 1;
        let remaining_pages = self.max_pages - self.logical_pages;
        let Some(next_offset) = archived_count_continuation_offset(
            self.offset,
            returned,
            ARCHIVED_COUNT_PAGE_SIZE,
            remaining_pages,
        )?
        else {
            return Ok(Some(self.count));
        };
        self.offset = next_offset;
        Ok(None)
    }
}

fn validate_archived_page_result_count(result_count: usize, limit: u32) -> Result<()> {
    if result_count > limit as usize {
        return Err(archived_page_error(
            "archived search returned more rows than its requested page limit",
        ));
    }
    Ok(())
}

fn archived_page_error(message: &str) -> AnytypeError {
    AnytypeError::Other {
        message: message.to_owned(),
    }
}

fn archived_validation_error(message: &str) -> AnytypeError {
    AnytypeError::Validation {
        message: message.to_owned(),
    }
}

#[derive(Debug)]
struct DeleteBestEffortResult {
    deleted: u64,
    failed_ids: Vec<String>,
}

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
    fn only_definitive_missing_space_is_retryable_for_sharing_admission() {
        use rpc::space::make_shareable::response::error::Code;

        assert!(space_sharing_admission_is_pending(Code::NoSuchSpace as i32));
        for code in [
            Code::UnknownError,
            Code::BadInput,
            Code::SpaceIsDeleted,
            Code::RequestFailed,
            Code::LimitReached,
        ] {
            assert!(!space_sharing_admission_is_pending(code as i32));
        }
    }

    #[test]
    fn chat_space_request_sends_ux_detail_and_legacy_flag() {
        let request = chat_space_create_request("Chat name".to_owned());
        let fields = request.details.expect("chat details").fields;
        assert!(matches!(
            fields.get("name").and_then(|value| value.kind.as_ref()),
            Some(prost_types::value::Kind::StringValue(name)) if name == "Chat name"
        ));
        assert!(matches!(
            fields
                .get(SPACE_UX_TYPE_KEY)
                .and_then(|value| value.kind.as_ref()),
            Some(prost_types::value::Kind::NumberValue(value))
                if *value == f64::from(model::SpaceUxType::Chat as i32)
        ));
        assert!(request.with_chat);
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
    fn test_update_space_request_body_preserves_omissions() {
        let name_only = UpdateSpaceRequestBody {
            name: Some("Renamed".to_owned()),
            description: None,
        };
        assert_eq!(
            serde_json::to_string(&name_only).unwrap(),
            r#"{"name":"Renamed"}"#
        );

        let description_only = UpdateSpaceRequestBody {
            name: None,
            description: Some("Updated".to_owned()),
        };
        assert_eq!(
            serde_json::to_string(&description_only).unwrap(),
            r#"{"description":"Updated"}"#
        );
    }

    fn offline_client() -> crate::client::AnytypeClient {
        let mut config =
            crate::client::ClientConfig::default().app_name("space-description-clearing");
        config.base_url = Some("http://127.0.0.1:1".to_owned());
        config.keystore = Some("env".to_owned());
        crate::client::AnytypeClient::with_config(config).expect("offline client")
    }

    #[test]
    fn update_space_omission_replacement_and_clearing_produce_distinct_bodies() {
        let client = offline_client();

        let omitted = client.update_space("space").name("Renamed");
        assert_eq!(
            serde_json::to_string(&omitted.request_body().unwrap()).unwrap(),
            r#"{"name":"Renamed"}"#
        );

        let replaced = client.update_space("space").description("Updated");
        assert_eq!(
            serde_json::to_string(&replaced.request_body().unwrap()).unwrap(),
            r#"{"description":"Updated"}"#
        );

        let cleared = client.update_space("space").clear_description();
        assert_eq!(
            serde_json::to_string(&cleared.request_body().unwrap()).unwrap(),
            r#"{"description":""}"#
        );

        // An explicit empty string is the same wire form as clearing.
        let empty = client.update_space("space").description("");
        assert_eq!(
            serde_json::to_string(&empty.request_body().unwrap()).unwrap(),
            r#"{"description":""}"#
        );

        let cleared_and_renamed = client
            .update_space("space")
            .name("Renamed")
            .clear_description();
        assert_eq!(
            serde_json::to_string(&cleared_and_renamed.request_body().unwrap()).unwrap(),
            r#"{"name":"Renamed","description":""}"#
        );

        assert!(matches!(
            client.update_space("space").request_body(),
            Err(AnytypeError::Validation { .. })
        ));
    }

    #[test]
    fn space_description_text_normalizes_empty_and_absent() {
        let mut space = Space {
            id: "space".to_string(),
            name: "Space".to_string(),
            object: SpaceModel::Space,
            description: Some(String::new()),
            icon: None,
            gateway_url: None,
            network_id: None,
        };
        assert_eq!(space.description_text(), None);
        space.description = None;
        assert_eq!(space.description_text(), None);
        space.description = Some("About this space".to_string());
        assert_eq!(space.description_text(), Some("About this space"));
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

    #[test]
    fn space_invite_values_have_stable_names_and_url() {
        assert_eq!(
            SpaceInviteType::Member.as_rpc(),
            model::InviteType::Member as i32
        );
        assert_eq!(
            SpaceInviteType::Guest.as_rpc(),
            model::InviteType::Guest as i32
        );
        assert_eq!(
            SpaceInviteType::AutoApprove.as_rpc(),
            model::InviteType::WithoutApprove as i32
        );
        assert_eq!(
            SpaceInvitePermission::Reader.as_rpc(),
            model::ParticipantPermissions::Reader as i32
        );
        assert_eq!(
            SpaceInvitePermission::Writer.as_rpc(),
            model::ParticipantPermissions::Writer as i32
        );
        assert_eq!(
            SpaceInvitePermission::Owner.as_rpc(),
            model::ParticipantPermissions::Owner as i32
        );
        assert_eq!(invite_type_name(model::InviteType::Member as i32), "member");
        assert_eq!(invite_type_name(model::InviteType::Guest as i32), "guest");
        assert_eq!(
            invite_type_name(model::InviteType::WithoutApprove as i32),
            "auto-approve"
        );
        assert_eq!(
            invite_permissions_name(model::ParticipantPermissions::Reader as i32),
            "reader"
        );
        assert_eq!(
            invite_permissions_name(model::ParticipantPermissions::Writer as i32),
            "writer"
        );
        assert_eq!(
            invite_permissions_name(model::ParticipantPermissions::Owner as i32),
            "owner"
        );
        assert_eq!(invite_url("cid", "key"), "https://invite.any.coop/cid#key");
    }

    #[test]
    fn space_invite_serialization_uses_public_wire_names() {
        let invite = SpaceInvite {
            invite_type: "member".to_owned(),
            permissions: Some("writer".to_owned()),
            cid: "cid".to_owned(),
            key: "key".to_owned(),
            url: "https://invite.any.coop/cid#key".to_owned(),
        };
        assert_eq!(
            serde_json::to_string(&invite).expect("serialize invitation"),
            r#"{"type":"member","permissions":"writer","cid":"cid","key":"key","url":"https://invite.any.coop/cid#key"}"#
        );

        let guest = SpaceInvite {
            invite_type: "guest".to_owned(),
            permissions: None,
            cid: "guest-cid".to_owned(),
            key: "guest-key".to_owned(),
            url: "https://invite.any.coop/guest-cid#guest-key".to_owned(),
        };
        assert_eq!(
            serde_json::to_string(&guest).expect("serialize guest invitation"),
            r#"{"type":"guest","cid":"guest-cid","key":"guest-key","url":"https://invite.any.coop/guest-cid#guest-key"}"#
        );
    }

    #[test]
    fn direct_space_response_requires_exact_identity() {
        let matching = SpaceResponse {
            space: Space {
                id: "space-direct-id".to_owned(),
                name: "Fresh".to_owned(),
                object: SpaceModel::Space,
                description: None,
                icon: None,
                gateway_url: None,
                network_id: None,
            },
        };
        assert_eq!(
            exact_space_response(matching, "space-direct-id")
                .expect("matching direct-space response")
                .name,
            "Fresh"
        );

        let mismatched = SpaceResponse {
            space: Space {
                id: "different-space-id".to_owned(),
                name: "Wrong".to_owned(),
                object: SpaceModel::Space,
                description: None,
                icon: None,
                gateway_url: None,
                network_id: None,
            },
        };
        let error = exact_space_response(mismatched, "space-direct-id")
            .expect_err("mismatched space identity");
        assert!(matches!(error, AnytypeError::Other { .. }));
        assert!(!error.to_string().contains("different-space-id"));
    }

    const ARCHIVED_TEST_ID: &str = "bafyreid5fvqlnsobih2keakcxjrrlpmly6kf37klzjzen4ibfdgalcdp4y";

    fn string_value(value: &str) -> Value {
        Value {
            kind: Some(prost_types::value::Kind::StringValue(value.to_owned())),
        }
    }

    #[test]
    fn archived_page_input_rejects_invalid_boundaries() {
        for limit in [Some(0), Some(crate::config::MAX_PAGINATION_LIMIT + 1)] {
            assert!(matches!(
                validate_archived_page_input(limit, None),
                Err(AnytypeError::Validation { .. })
            ));
        }
        assert!(matches!(
            validate_archived_page_input(None, Some(i32::MAX as u32 + 1)),
            Err(AnytypeError::Validation { .. })
        ));
        assert!(
            validate_archived_page_input(
                Some(crate::config::MAX_PAGINATION_LIMIT),
                Some(i32::MAX as u32)
            )
            .is_ok()
        );
    }

    #[test]
    fn archived_page_result_count_rejects_rows_beyond_the_requested_limit() {
        assert!(validate_archived_page_result_count(500, 500).is_ok());
        assert!(validate_archived_page_result_count(501, 500).is_err());
    }

    #[test]
    fn archived_count_requires_a_continuation_probe_after_a_full_page() {
        assert_eq!(
            archived_count_continuation_offset(0, 0, 500, 0)
                .expect("an empty page proves exhaustion"),
            None
        );
        assert_eq!(
            archived_count_continuation_offset(0, 499, 500, 0)
                .expect("short page proves exhaustion"),
            None
        );
        assert_eq!(
            archived_count_continuation_offset(0, 500, 500, 1)
                .expect("remaining page can probe a full page"),
            Some(500)
        );
        assert!(matches!(
            archived_count_continuation_offset(0, 500, 500, 0),
            Err(AnytypeError::Other { .. })
        ));
        assert!(matches!(
            archived_count_continuation_offset(500, 500, 500, 0),
            Err(AnytypeError::Other { .. })
        ));
    }

    #[derive(Clone, Copy)]
    struct ArchivedCountScriptStep {
        returned: usize,
        used_relation_fallback: bool,
    }

    #[derive(Debug, PartialEq, Eq)]
    struct ArchivedCountScriptResult {
        count: u64,
        logical_pages: u32,
        upstream_requests: u32,
    }

    fn run_archived_count_script(
        max_pages: u32,
        steps: &[ArchivedCountScriptStep],
    ) -> Result<ArchivedCountScriptResult> {
        let mut state = ArchivedCountState::new(max_pages);
        let mut upstream_requests = 0;
        for step in steps {
            upstream_requests += if step.used_relation_fallback { 2 } else { 1 };
            if let Some(count) = state.record_page(step.returned)? {
                return Ok(ArchivedCountScriptResult {
                    count,
                    logical_pages: state.logical_pages,
                    upstream_requests,
                });
            }
        }
        Err(archived_page_error(
            "archived count script ended before it proved exhaustion",
        ))
    }

    #[test]
    fn archived_count_state_machine_proves_exact_counts_within_request_caps() {
        let zero = run_archived_count_script(
            1,
            &[ArchivedCountScriptStep {
                returned: 0,
                used_relation_fallback: false,
            }],
        )
        .expect("empty first page is exact");
        assert_eq!(
            zero,
            ArchivedCountScriptResult {
                count: 0,
                logical_pages: 1,
                upstream_requests: 1,
            }
        );

        let partial = run_archived_count_script(
            1,
            &[ArchivedCountScriptStep {
                returned: 499,
                used_relation_fallback: true,
            }],
        )
        .expect("short first page is exact");
        assert_eq!(partial.count, 499);
        assert_eq!(partial.logical_pages, 1);
        assert_eq!(partial.upstream_requests, 2);

        let exact_500 = run_archived_count_script(
            2,
            &[
                ArchivedCountScriptStep {
                    returned: 500,
                    used_relation_fallback: false,
                },
                ArchivedCountScriptStep {
                    returned: 0,
                    used_relation_fallback: true,
                },
            ],
        )
        .expect("empty continuation probe proves an exact full page");
        assert_eq!(exact_500.count, 500);
        assert_eq!(exact_500.logical_pages, 2);
        assert_eq!(exact_500.upstream_requests, 3);

        let exact_1000 = run_archived_count_script(
            3,
            &[
                ArchivedCountScriptStep {
                    returned: 500,
                    used_relation_fallback: true,
                },
                ArchivedCountScriptStep {
                    returned: 500,
                    used_relation_fallback: false,
                },
                ArchivedCountScriptStep {
                    returned: 0,
                    used_relation_fallback: true,
                },
            ],
        )
        .expect("second continuation probe proves an exact multiple");
        assert_eq!(exact_1000.count, 1000);
        assert_eq!(exact_1000.logical_pages, 3);
        assert_eq!(exact_1000.upstream_requests, 5);
        assert!(exact_1000.upstream_requests <= 2 * exact_1000.logical_pages);
    }

    #[test]
    fn archived_count_state_machine_rejects_bad_pages_and_unproven_budgets() {
        assert!(
            run_archived_count_script(
                1,
                &[ArchivedCountScriptStep {
                    returned: 501,
                    used_relation_fallback: false,
                }],
            )
            .is_err()
        );
        assert!(
            run_archived_count_script(
                1,
                &[ArchivedCountScriptStep {
                    returned: 500,
                    used_relation_fallback: false,
                }],
            )
            .is_err()
        );
        assert!(
            run_archived_count_script(
                2,
                &[
                    ArchivedCountScriptStep {
                        returned: 500,
                        used_relation_fallback: true,
                    },
                    ArchivedCountScriptStep {
                        returned: 500,
                        used_relation_fallback: true,
                    },
                ],
            )
            .is_err()
        );
    }

    #[test]
    fn archived_search_request_preserves_page_and_type_filters() {
        let request = archived_search_request(
            ARCHIVED_TEST_ID,
            "isArchived",
            500,
            12,
            &[ARCHIVED_TEST_ID.to_owned()],
        );

        assert_eq!(request.limit, 500);
        assert_eq!(request.offset, 12);
        assert_eq!(request.filters.len(), 2);
        assert_eq!(request.filters[0].relation_key, "isArchived");
        assert_eq!(request.filters[1].relation_key, "type");
    }

    fn set_string_value(values: &[&str]) -> Value {
        Value {
            kind: Some(prost_types::value::Kind::ListValue(ListValue {
                values: values.iter().map(|value| string_value(value)).collect(),
            })),
        }
    }

    #[test]
    fn archived_search_result_validates_but_omits_set_string_type_metadata() {
        let details = prost_types::Struct {
            fields: BTreeMap::from([("type".to_owned(), set_string_value(&[ARCHIVED_TEST_ID]))]),
        };
        let object = archived_object_from_search_result(
            ARCHIVED_TEST_ID,
            model::search::Result {
                object_id: ARCHIVED_TEST_ID.to_owned(),
                details: Some(details),
                meta: Vec::new(),
            },
        )
        .expect("the real SetString type relation must be validated");

        assert!(object.r#type.is_none());
    }

    #[test]
    fn archived_search_result_validates_but_omits_scalar_set_string_type_metadata() {
        let details = prost_types::Struct {
            fields: BTreeMap::from([("type".to_owned(), string_value(ARCHIVED_TEST_ID))]),
        };
        let object = archived_object_from_search_result(
            ARCHIVED_TEST_ID,
            model::search::Result {
                object_id: ARCHIVED_TEST_ID.to_owned(),
                details: Some(details),
                meta: Vec::new(),
            },
        )
        .expect("the scalar SetString type relation must be validated");

        assert!(object.r#type.is_none());
    }

    #[test]
    fn archived_search_result_rejects_malformed_set_string_type_ids() {
        let details = prost_types::Struct {
            fields: BTreeMap::from([("type".to_owned(), set_string_value(&["not-an-object-id"]))]),
        };

        assert!(
            archived_object_from_search_result(
                ARCHIVED_TEST_ID,
                model::search::Result {
                    object_id: ARCHIVED_TEST_ID.to_owned(),
                    details: Some(details),
                    meta: Vec::new(),
                },
            )
            .is_err()
        );
    }

    #[test]
    fn archived_search_result_rejects_non_singleton_set_string_type_ids() {
        let details = prost_types::Struct {
            fields: BTreeMap::from([(
                "type".to_owned(),
                set_string_value(&[ARCHIVED_TEST_ID, ARCHIVED_TEST_ID]),
            )]),
        };

        assert!(
            archived_object_from_search_result(
                ARCHIVED_TEST_ID,
                model::search::Result {
                    object_id: ARCHIVED_TEST_ID.to_owned(),
                    details: Some(details),
                    meta: Vec::new(),
                },
            )
            .is_err()
        );
    }

    #[test]
    fn archived_search_result_rejects_missing_or_malformed_ids() {
        let missing = archived_object_from_search_result(
            ARCHIVED_TEST_ID,
            model::search::Result {
                object_id: String::new(),
                details: None,
                meta: Vec::new(),
            },
        );
        assert!(missing.is_err());

        let malformed = archived_object_from_search_result(
            ARCHIVED_TEST_ID,
            model::search::Result {
                object_id: "not-an-object-id".to_owned(),
                details: None,
                meta: Vec::new(),
            },
        );
        assert!(malformed.is_err());
    }

    #[tokio::test]
    async fn archived_count_rejects_a_zero_page_budget_without_upstream_work() {
        let client = AnytypeClient::new("archived-count-boundary-test")
            .expect("client construction must not require a connection");
        let error = client
            .count_archived_bounded(ARCHIVED_TEST_ID, 0)
            .await
            .expect_err("zero page budget must fail before opening a connection");

        assert!(matches!(
            error,
            AnytypeError::Validation { ref message } if message.contains("page budget")
        ));
    }
}
