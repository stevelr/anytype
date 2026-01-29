//! # Anytype Spaces
//!
//! This module provides a fluent builder API for working with Anytype spaces.
//!
//! ## Space methods on AnytypeClient
//!
//! - [spaces](AnytypeClient::spaces) - list spaces the authenticated user can access
//! - [space](AnytypeClient::space) - get space
//! - [new_space](AnytypeClient::new_space) - create a new space
//! - [update_space](AnytypeClient::space) - update space properties
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

use std::sync::Arc;

use futures::StreamExt;
use serde::{Deserialize, Serialize};
use snafu::prelude::*;

use crate::{
    Result,
    cache::AnytypeCache,
    client::AnytypeClient,
    filters::Query,
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
    /// Example: "http://127.0.0.1:31006"
    pub gateway_url: Option<String>,

    /// Network ID of the space
    /// Example: "N83gJpVd9MuNRZAuJLZ7LiMntTThhPc6DtzWWVjb1M3PouVU"
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
    /// Creates a new SpaceRequest.
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
            .get_request(&format!("/v1/spaces/{}", self.space_id), Default::default())
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
    /// Creates a new NewSpaceRequest.
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
    pub fn description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    /// Enables read-after-write verification for this request.
    pub fn ensure_available(mut self) -> Self {
        self.verify_policy = VerifyPolicy::Enabled;
        self
    }

    /// Enables verification using a custom config for this request.
    pub fn ensure_available_with(mut self, config: VerifyConfig) -> Self {
        self.verify_policy = VerifyPolicy::Enabled;
        self.verify_config = Some(config);
        self
    }

    /// Disables verification for this request.
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
            .post_request("/v1/spaces", &request_body, Default::default())
            .await?;

        let space = response.space;
        if let Some(config) = resolve_verify(self.verify_policy, &self.verify_config) {
            return verify_available(&config, "Space", &space.id, || async {
                let response: SpaceResponse = self
                    .client
                    .get_request(&format!("/v1/spaces/{}", space.id), Default::default())
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
    /// Creates a new UpdateSpaceRequest.
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
    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Updates the space description.
    ///
    /// # Arguments
    /// * `description` - New description text for the space
    pub fn description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    /// Enables read-after-write verification for this request.
    pub fn ensure_available(mut self) -> Self {
        self.verify_policy = VerifyPolicy::Enabled;
        self
    }

    /// Enables verification using a custom config for this request.
    pub fn ensure_available_with(mut self, config: VerifyConfig) -> Self {
        self.verify_policy = VerifyPolicy::Enabled;
        self.verify_config = Some(config);
        self
    }

    /// Disables verification for this request.
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
        if let Some(config) = resolve_verify(self.verify_policy, &self.verify_config) {
            return verify_available(&config, "Space", &space.id, || async {
                let response: SpaceResponse = self
                    .client
                    .get_request(&format!("/v1/spaces/{}", space.id), Default::default())
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
    limit: Option<usize>,
    offset: Option<usize>,
    filters: Vec<Filter>,
    cache: Arc<AnytypeCache>,
}

impl ListSpacesRequest {
    /// Creates a new ListSpacesRequest.
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
    pub fn limit(mut self, limit: usize) -> Self {
        self.limit = Some(limit);
        self
    }

    /// Sets the pagination offset (starting position).
    ///
    /// # Arguments
    /// * `offset` - Number of items to skip
    pub fn offset(mut self, offset: usize) -> Self {
        self.offset = Some(offset);
        self
    }

    /// Adds a filter condition.
    ///
    /// Multiple filters are combined with AND logic.
    ///
    /// # Arguments
    /// * `filter` - Filter condition to add
    pub fn filter(mut self, filter: Filter) -> Self {
        self.filters.push(filter);
        self
    }

    /// Adds multiple filter conditions.
    ///
    /// # Arguments
    /// * `filters` - Iterator of filters to add
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
            .set_limit_opt(&self.limit)
            .set_offset_opt(&self.offset)
            .add_filters(&self.filters);

        self.client.get_request_paged("/v1/spaces", query).await
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
}

// ============================================================================
// TESTS
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_space_model_default() {
        let model: SpaceModel = Default::default();
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
