//! # Anytype Tags
//!
//! This module provides a fluent builder API for working with property tags.
//! Tags are used for select and multi-select properties.
//!
//! ## Tag methods on AnytypeClient
//!
//! - [tags](AnytypeClient::tags) - list tags for a property
//! - [tag](AnytypeClient::tag) - get a property tag
//! - [new_tag](AnytypeClient::new_tag) - create a property tag
//! - [update_tag](AnytypeClient::update_tag) - update property tag
//! - [lookup_property_tag](AnytypeClient::lookup_property_tag) - find tag for property using keys or ids
//!
//! ## Quick Start
//!
//! ```rust,no_run
//! use anytype::prelude::*;
//!
//! # async fn example(client: &AnytypeClient) -> Result<(), AnytypeError> {
//! let space_id = "your_space_id";
//! let property_id = "property_id";
//!
//! // List all tags for a property
//! let tags = client.tags(space_id, property_id).list().await?;
//!
//! // Get a specific tag
//! let tag = client.tag(space_id, property_id, "tag_id").get().await?;
//!
//! // Create a new tag
//! let tag = client.new_tag(space_id, property_id)
//!     .name("Urgent")
//!     .color(Color::Red)
//!     .create().await?;
//! # Ok(())
//! # }
//! ```

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::{
    Result,
    cache::AnytypeCache,
    client::AnytypeClient,
    filters::Query,
    http_client::{GetPaged, HttpClient},
    prelude::*,
    properties::set_property_tags,
    verify::{VerifyConfig, VerifyPolicy, resolve_verify, verify_available},
};

/// Represents a tag for select/multi-select properties.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Tag {
    /// Unique tag identifier
    pub id: String,
    /// Display name of the tag
    pub name: String,
    /// Key for the tag (snake_case)
    pub key: String,
    /// Tag color
    pub color: Color,
}

/// Request structure for creating a tag (used in property creation).
#[derive(Debug, Serialize, Clone)]
pub struct CreateTagRequest {
    /// Tag name (required)
    pub name: String,
    /// Optional custom key for the tag
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    /// Tag color (required)
    pub color: Color,
}

// ============================================================================
// RESPONSE TYPES (internal)
// ============================================================================

#[derive(Debug, Deserialize)]
struct TagResponse {
    tag: Tag,
}

async fn refresh_cached_property_tags(
    cache: &Arc<AnytypeCache>,
    client: &Arc<HttpClient>,
    limits: &ValidationLimits,
    space_id: &str,
    property_id: &str,
) -> Result<()> {
    if !cache.has_properties(space_id) {
        return Ok(());
    }

    if let Some(property) = cache.get_property(space_id, property_id) {
        let mut property = (*property).clone();
        set_property_tags(client, limits, space_id, &mut property).await?;
        cache.set_property(space_id, property);
    }

    Ok(())
}

// ============================================================================
// REQUEST BODY TYPES (internal)
// ============================================================================

#[derive(Debug, Serialize, Default)]
struct UpdateTagRequestBody {
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    color: Option<Color>,
}

// ============================================================================
// BUILDER STRUCTS (public)
// ============================================================================

/// Request builder for getting or deleting a single tag.
#[derive(Debug)]
pub struct TagRequest {
    client: Arc<HttpClient>,
    limits: ValidationLimits,
    space_id: String,
    property_id: String,
    tag_id: String,
    cache: Arc<AnytypeCache>,
}

impl TagRequest {
    pub(crate) fn new(
        client: Arc<HttpClient>,
        limits: ValidationLimits,
        space_id: impl Into<String>,
        property_id: impl Into<String>,
        tag_id: impl Into<String>,
        cache: Arc<AnytypeCache>,
    ) -> Self {
        Self {
            client,
            limits,
            space_id: space_id.into(),
            property_id: property_id.into(),
            tag_id: tag_id.into(),
            cache,
        }
    }

    /// Retrieves the tag by ID.
    pub async fn get(self) -> Result<Tag> {
        self.limits.validate_id(&self.space_id, "space_id")?;
        self.limits.validate_id(&self.property_id, "property_id")?;
        self.limits.validate_id(&self.tag_id, "tag_id")?;

        let response: TagResponse = self
            .client
            .get_request(
                &format!(
                    "/v1/spaces/{}/properties/{}/tags/{}",
                    self.space_id, self.property_id, self.tag_id
                ),
                Default::default(),
            )
            .await?;
        Ok(response.tag)
    }

    /// Deletes the tag.
    pub async fn delete(self) -> Result<Tag> {
        self.limits.validate_id(&self.space_id, "space_id")?;
        self.limits.validate_id(&self.property_id, "property_id")?;
        self.limits.validate_id(&self.tag_id, "tag_id")?;

        let response: TagResponse = self
            .client
            .delete_request(&format!(
                "/v1/spaces/{}/properties/{}/tags/{}",
                self.space_id, self.property_id, self.tag_id
            ))
            .await?;
        refresh_cached_property_tags(
            &self.cache,
            &self.client,
            &self.limits,
            &self.space_id,
            &self.property_id,
        )
        .await?;
        Ok(response.tag)
    }
}

/// Request builder for creating a new tag.
#[derive(Debug)]
pub struct NewTagRequest {
    client: Arc<HttpClient>,
    limits: ValidationLimits,
    space_id: String,
    property_id: String,
    name: Option<String>,
    key: Option<String>,
    color: Color,
    verify_policy: VerifyPolicy,
    verify_config: Option<VerifyConfig>,
    cache: Arc<AnytypeCache>,
}

impl NewTagRequest {
    pub(crate) fn new(
        client: Arc<HttpClient>,
        limits: ValidationLimits,
        space_id: impl Into<String>,
        property_id: impl Into<String>,
        verify_config: Option<VerifyConfig>,
        cache: Arc<AnytypeCache>,
    ) -> Self {
        Self {
            client,
            limits,
            space_id: space_id.into(),
            property_id: property_id.into(),
            name: None,
            key: None,
            color: Color::Grey,
            verify_policy: VerifyPolicy::Default,
            verify_config,
            cache,
        }
    }

    /// Sets the tag name.
    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Sets the tag key.
    pub fn key(mut self, key: impl Into<String>) -> Self {
        self.key = Some(key.into());
        self
    }

    /// Sets the tag color.
    pub fn color(mut self, color: Color) -> Self {
        self.color = color;
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

    /// Creates the tag.
    pub async fn create(self) -> Result<Tag> {
        self.limits.validate_id(&self.space_id, "space_id")?;
        self.limits.validate_id(&self.property_id, "property_id")?;

        let name = self.name.ok_or_else(|| AnytypeError::Validation {
            message: "new_tag: name is required".to_string(),
        })?;

        self.limits.validate_name(&name, "tag")?;

        let request = CreateTagRequest {
            name,
            key: self.key,
            color: self.color,
        };

        let response: TagResponse = self
            .client
            .post_request(
                &format!(
                    "/v1/spaces/{}/properties/{}/tags",
                    self.space_id, self.property_id
                ),
                &request,
                Default::default(),
            )
            .await?;
        let tag = response.tag;
        refresh_cached_property_tags(
            &self.cache,
            &self.client,
            &self.limits,
            &self.space_id,
            &self.property_id,
        )
        .await?;
        if let Some(config) = resolve_verify(self.verify_policy, &self.verify_config) {
            return verify_available(&config, "Tag", &tag.id, || async {
                let response: TagResponse = self
                    .client
                    .get_request(
                        &format!(
                            "/v1/spaces/{}/properties/{}/tags/{}",
                            self.space_id, self.property_id, tag.id
                        ),
                        Default::default(),
                    )
                    .await?;
                Ok(response.tag)
            })
            .await;
        }
        Ok(tag)
    }
}

/// Request builder for updating an existing tag.
#[derive(Debug)]
pub struct UpdateTagRequest {
    client: Arc<HttpClient>,
    limits: ValidationLimits,
    space_id: String,
    property_id: String,
    tag_id: String,
    name: Option<String>,
    key: Option<String>,
    color: Option<Color>,
    verify_policy: VerifyPolicy,
    verify_config: Option<VerifyConfig>,
    cache: Arc<AnytypeCache>,
}

impl UpdateTagRequest {
    pub(crate) fn new(
        client: Arc<HttpClient>,
        limits: ValidationLimits,
        space_id: impl Into<String>,
        property_id: impl Into<String>,
        tag_id: impl Into<String>,
        verify_config: Option<VerifyConfig>,
        cache: Arc<AnytypeCache>,
    ) -> Self {
        Self {
            client,
            limits,
            space_id: space_id.into(),
            property_id: property_id.into(),
            tag_id: tag_id.into(),
            name: None,
            key: None,
            color: None,
            verify_policy: VerifyPolicy::Default,
            verify_config,
            cache,
        }
    }

    /// Updates the tag name.
    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Updates the tag key.
    pub fn key(mut self, key: impl Into<String>) -> Self {
        self.key = Some(key.into());
        self
    }

    /// Updates the tag color.
    pub fn color(mut self, color: Color) -> Self {
        self.color = Some(color);
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

    /// Applies the update.
    pub async fn update(self) -> Result<Tag> {
        self.limits.validate_id(&self.space_id, "space_id")?;
        self.limits.validate_id(&self.property_id, "property_id")?;
        self.limits.validate_id(&self.tag_id, "tag_id")?;

        if self.name.is_none() && self.key.is_none() && self.color.is_none() {
            return Err(AnytypeError::Validation {
                message: "update_tag: must set at least one field to update".to_string(),
            });
        }

        if let Some(ref name) = self.name {
            self.limits.validate_name(name, "tag")?;
        }

        let request = UpdateTagRequestBody {
            name: self.name,
            key: self.key,
            color: self.color,
        };

        let response: TagResponse = self
            .client
            .patch_request(
                &format!(
                    "/v1/spaces/{}/properties/{}/tags/{}",
                    self.space_id, self.property_id, self.tag_id
                ),
                &request,
            )
            .await?;
        let tag = response.tag;
        refresh_cached_property_tags(
            &self.cache,
            &self.client,
            &self.limits,
            &self.space_id,
            &self.property_id,
        )
        .await?;
        if let Some(config) = resolve_verify(self.verify_policy, &self.verify_config) {
            return verify_available(&config, "Tag", &tag.id, || async {
                let response: TagResponse = self
                    .client
                    .get_request(
                        &format!(
                            "/v1/spaces/{}/properties/{}/tags/{}",
                            self.space_id, self.property_id, tag.id
                        ),
                        Default::default(),
                    )
                    .await?;
                Ok(response.tag)
            })
            .await;
        }
        Ok(tag)
    }
}

/// Request builder for listing tags.
#[derive(Debug)]
pub struct ListTagsRequest {
    client: Arc<HttpClient>,
    limits: ValidationLimits,
    space_id: String,
    property_id: String,
    limit: Option<usize>,
    offset: Option<usize>,
    filters: Vec<Filter>,
}

impl ListTagsRequest {
    pub(crate) fn new(
        client: Arc<HttpClient>,
        limits: ValidationLimits,
        space_id: impl Into<String>,
        property_id: impl Into<String>,
    ) -> Self {
        Self {
            client,
            limits,
            space_id: space_id.into(),
            property_id: property_id.into(),
            limit: None,
            offset: None,
            filters: Vec::new(),
        }
    }

    /// Sets the pagination limit.
    pub fn limit(mut self, limit: usize) -> Self {
        self.limit = Some(limit);
        self
    }

    /// Sets the pagination offset.
    pub fn offset(mut self, offset: usize) -> Self {
        self.offset = Some(offset);
        self
    }

    /// Adds a filter condition.
    pub fn filter(mut self, filter: Filter) -> Self {
        self.filters.push(filter);
        self
    }

    /// Executes the list request.
    pub async fn list(self) -> Result<PagedResult<Tag>> {
        self.limits.validate_id(&self.space_id, "space_id")?;
        self.limits.validate_id(&self.property_id, "property_id")?;

        let query = Query::default()
            .set_limit_opt(&self.limit)
            .set_offset_opt(&self.offset)
            .add_filters(&self.filters);

        self.client
            .get_request_paged(
                &format!(
                    "/v1/spaces/{}/properties/{}/tags",
                    self.space_id, self.property_id
                ),
                query,
            )
            .await
    }
}

// ============================================================================
// ANYTYPECLIENT METHODS
// ============================================================================

impl AnytypeClient {
    /// Creates a request builder for getting or deleting a single tag.
    pub fn tag(
        &self,
        space_id: impl Into<String>,
        property_id: impl Into<String>,
        tag_id: impl Into<String>,
    ) -> TagRequest {
        TagRequest::new(
            self.client.clone(),
            self.config.limits.clone(),
            space_id,
            property_id,
            tag_id,
            self.cache.clone(),
        )
    }

    /// Creates a request builder for creating a new tag.
    pub fn new_tag(
        &self,
        space_id: impl Into<String>,
        property_id: impl Into<String>,
    ) -> NewTagRequest {
        NewTagRequest::new(
            self.client.clone(),
            self.config.limits.clone(),
            space_id,
            property_id,
            self.config.verify.clone(),
            self.cache.clone(),
        )
    }

    /// Creates a request builder for updating an existing tag.
    pub fn update_tag(
        &self,
        space_id: impl Into<String>,
        property_id: impl Into<String>,
        tag_id: impl Into<String>,
    ) -> UpdateTagRequest {
        UpdateTagRequest::new(
            self.client.clone(),
            self.config.limits.clone(),
            space_id,
            property_id,
            tag_id,
            self.config.verify.clone(),
            self.cache.clone(),
        )
    }

    /// Creates a request builder for listing tags.
    pub fn tags(
        &self,
        space_id: impl Into<String>,
        property_id: impl Into<String>,
    ) -> ListTagsRequest {
        ListTagsRequest::new(
            self.client.clone(),
            self.config.limits.clone(),
            space_id,
            property_id,
        )
    }
}
