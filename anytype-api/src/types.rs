//! # Anytype Types
//!
//! This module provides a fluent builder API for working with Anytype object types.
//!
//! ## Type methods on AnytypeClient
//!
//! - [types](AnytypeClient::types) - list types in the space
//! - [get_type](AnytypeClient::get_type) - get type for retrieval or deletion
//! - [new_type](AnytypeClient::new_type) - create a new type
//! - [update_type](AnytypeClient::update_type) - update type properties
//! - [lookup_type_by_key](AnytypeClient::lookup_type_by_key) - find type using key
//!
//! ## Quick Start
//!
//! ```rust
//! use anytype::prelude::*;
//!
//! # async fn example() -> Result<(), AnytypeError> {
//! #   let client = AnytypeClient::new("doc test")?;
//! #   let space_id = anytype::test_util::example_space_id(&client).await?;
//!
//! // List all types
//! let types = client.types(&space_id).list().await?;
//! let some_type = types.iter().next().unwrap().clone();
//!
//! // Get a type by id
//! let typ = client.get_type(&space_id, &some_type.id).get().await?;
//!
//! // Get a type by key
//! let typ = client.lookup_type_by_key(&space_id, "page").await?;
//!
//! // Create a new type
//! let project = client.new_type(&space_id, "Project")
//!     .key("project")
//!     .create().await?;
//!
//! // Update a type: change name and add a property
//! let project = client.update_type(&space_id, &project.id)
//!     .name("My New Project")
//!     .property("Location", "location", PropertyFormat::Text)
//!     .update().await?;
//!
//! // Delete a type
//! client.get_type(&space_id, &project.id).delete().await?;
//!
//! # Ok(())
//! # }
//! ```
//!
//! ## Types
//!
//! - [`Type`] - Represents an Anytype object type
//! - [`TypeLayout`] - Layout variants for types (Basic, Profile, Action, Note)
//! - [`TypeRequest`] - Builder for get/delete operations
//! - [`NewTypeRequest`] - Builder for creating types
//! - [`ListTypesRequest`] - Builder for listing types

use std::sync::Arc;

use serde::{Deserialize, Deserializer, Serialize};

use crate::{
    Result,
    cache::AnytypeCache,
    client::AnytypeClient,
    error::*,
    filters::Query,
    http_client::{GetPaged, HttpClient},
    prelude::*,
    verify::{VerifyConfig, VerifyPolicy, resolve_verify, verify_available},
};

/// Layout variants for types.
///
/// Determines the default appearance and behavior of objects of this type.
/// Note: This differs from [`ObjectLayout`] which has additional variants
/// (Bookmark, Set, Collection, Participant).
#[derive(
    Debug, Deserialize, Serialize, Clone, Default, PartialEq, Eq, strum::Display, strum::EnumString,
)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum TypeLayout {
    /// Standard object layout with full editing capabilities
    #[default]
    Basic,
    /// Profile layout for user/contact information
    Profile,
    /// Action/task layout
    Action,
    /// Note layout - simplified, name is optional
    Note,
}

/// Property definition for type creation.
///
/// Defines a property to be associated with a new type.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct CreateTypeProperty {
    /// The format of the property (text, number, date, etc.)
    pub format: PropertyFormat,
    /// Unique key for the property
    pub key: String,
    /// Display name for the property
    pub name: String,
}

/// Represents an Anytype object type.
///
/// Types define the structure and default behavior for objects. Each type
/// has a unique key, a display name, and a default layout. Built-in types
/// include Page, Note, Task, and Bookmark.
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct Type {
    /// Whether the type is archived
    pub archived: bool,

    /// Type icon (emoji, file, or colored icon)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<Icon>,

    /// Unique type identifier (unique across all spaces)
    pub id: String,

    /// Key of the type (can be the same across spaces for known types, e.g., "page")
    pub key: String,

    /// Default layout for objects of this type
    #[serde(default)]
    pub layout: ObjectLayout,

    /// Display name of the type
    #[serde(default)]
    pub name: Option<String>,

    /// Plural form of the name
    #[serde(default)]
    pub plural_name: Option<String>,

    /// Properties linked to the type
    #[serde(default, deserialize_with = "deserialize_vec_properties_or_null")]
    pub properties: Vec<Property>,
}

fn deserialize_vec_properties_or_null<'de, D>(deserializer: D) -> Result<Vec<Property>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Option::<Vec<Property>>::deserialize(deserializer)?;
    Ok(value.unwrap_or_default())
}

impl Type {
    /// Returns true if this is a built-in system type.
    ///
    /// System types like "page" and "note" cannot be deleted.
    pub fn is_system_type(&self) -> bool {
        matches!(self.key.as_str(), "page" | "note" | "task" | "bookmark")
    }

    /// Returns the name of the type, or the key if name is not set.
    pub fn display_name(&self) -> &str {
        self.name.as_deref().unwrap_or(&self.key)
    }

    pub fn get_property_by_key(&self, property_key: &str) -> Option<&Property> {
        self.properties.iter().find(|prop| prop.key == property_key)
    }
}

// ============================================================================
// RESPONSE TYPES (internal)
// ============================================================================

/// Response wrapper for single type operations
#[derive(Debug, Deserialize)]
struct TypeResponse {
    #[serde(rename = "type")]
    type_: Type,
}

// ============================================================================
// REQUEST BODY TYPES (internal)
// ============================================================================

/// Internal request body for creating a type
#[derive(Debug, Serialize)]
struct CreateTypeRequestBody {
    name: String,

    #[serde(skip_serializing_if = "Option::is_none")]
    key: Option<String>,

    plural_name: String,

    #[serde(skip_serializing_if = "Option::is_none")]
    icon: Option<Icon>,

    layout: TypeLayout,

    #[serde(skip_serializing_if = "Vec::is_empty")]
    properties: Vec<CreateTypeProperty>,
}

/// Internal request body for updating a type
#[derive(Debug, Serialize, Default)]
struct UpdateTypeRequestBody {
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    key: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    plural_name: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    icon: Option<Icon>,

    #[serde(skip_serializing_if = "Option::is_none")]
    layout: Option<TypeLayout>,

    #[serde(skip_serializing_if = "Vec::is_empty")]
    properties: Vec<CreateTypeProperty>,
}

// ============================================================================
// BUILDER STRUCTS (public)
// ============================================================================

/// Request builder for getting or deleting a single type.
///
/// Obtained via [`AnytypeClient::get_type`].
#[derive(Debug)]
pub struct TypeRequest {
    client: Arc<HttpClient>,
    limits: ValidationLimits,
    space_id: String,
    type_id: String,
    cache: Arc<AnytypeCache>,
}

impl TypeRequest {
    /// Creates a new TypeRequest.
    pub(crate) fn new(
        client: Arc<HttpClient>,
        limits: ValidationLimits,
        space_id: impl Into<String>,
        type_id: impl Into<String>,
        cache: Arc<AnytypeCache>,
    ) -> Self {
        Self {
            client,
            limits,
            space_id: space_id.into(),
            type_id: type_id.into(),
            cache,
        }
    }

    /// Retrieves the type by ID.
    ///
    /// # Returns
    /// The type with all its metadata and properties.
    ///
    /// # Errors
    /// - [`AnytypeError::NotFound`] if the type doesn't exist
    /// - [`AnytypeError::Validation`] if IDs are invalid
    pub async fn get(self) -> Result<Type> {
        self.limits.validate_id(&self.space_id, "space_id")?;
        self.limits.validate_id(&self.type_id, "type_id")?;

        if self.cache.is_enabled() {
            if let Some(typ) = self.cache.get_type(&self.space_id, &self.type_id) {
                return Ok((*typ).clone());
            }
            // see note on locking design in cache.rs
            if !self.cache.has_types(&self.space_id) {
                prime_cache_types(&self.client, &self.cache, &self.space_id).await?;
                if let Some(type_) = self.cache.get_type(&self.space_id, &self.type_id) {
                    return Ok((*type_).clone());
                }
            }
            return Err(AnytypeError::NotFound {
                obj_type: "Type".to_string(),
                key: self.type_id.clone(),
            });
        }
        let response: TypeResponse = self
            .client
            .get_request(
                &format!("/v1/spaces/{}/types/{}", self.space_id, self.type_id),
                Default::default(),
            )
            .await?;
        Ok(response.type_)
    }

    /// Deletes (archives) the type.
    ///
    /// # Returns
    /// The deleted type.
    ///
    /// # Errors
    /// - [`AnytypeError::NotFound`] if the type doesn't exist
    /// - [`AnytypeError::Forbidden`] if you don't have permission
    pub async fn delete(self) -> Result<Type> {
        self.limits.validate_id(&self.space_id, "space_id")?;
        self.limits.validate_id(&self.type_id, "type_id")?;

        let response: TypeResponse = self
            .client
            .delete_request(&format!(
                "/v1/spaces/{}/types/{}",
                self.space_id, self.type_id
            ))
            .await?;

        if self.cache.has_types(&self.space_id) {
            self.cache.delete_type(&self.space_id, &self.type_id);
        }
        Ok(response.type_)
    }
}

/// Request builder for creating a new type.
///
/// Obtained via [`AnytypeClient::new_type`].
///
#[derive(Debug)]
pub struct NewTypeRequest {
    client: Arc<HttpClient>,
    limits: ValidationLimits,
    space_id: String,
    name: String,
    key: Option<String>,
    plural_name: String,
    icon: Option<Icon>,
    layout: TypeLayout,
    properties: Vec<CreateTypeProperty>,
    cache: Arc<AnytypeCache>,
    verify_policy: VerifyPolicy,
    verify_config: Option<VerifyConfig>,
}

impl NewTypeRequest {
    /// Creates a new NewTypeRequest. You must specify the name and plural_name.
    /// Defaults to Basic Layout
    pub(crate) fn new(
        client: Arc<HttpClient>,
        limits: ValidationLimits,
        space_id: impl Into<String>,
        name: String,
        plural_name: String,
        cache: Arc<AnytypeCache>,
        verify_config: Option<VerifyConfig>,
    ) -> Self {
        Self {
            client,
            limits,
            space_id: space_id.into(),
            name,
            key: None,
            plural_name,
            icon: None,
            layout: TypeLayout::Basic,
            properties: Vec::new(),
            cache,
            verify_policy: VerifyPolicy::Default,
            verify_config,
        }
    }

    /// Sets the plural name.
    ///
    /// Default plural name is the name + 's'.
    ///
    /// # Arguments
    /// * `plural_name` - plural display name for the type
    pub fn plural_name(mut self, plural_name: impl Into<String>) -> Self {
        self.plural_name = plural_name.into();
        self
    }

    /// Sets the type key.
    ///
    /// The key is a unique identifier for the type, typically lowercase
    /// with underscores (e.g., "project", "meeting_note").
    ///
    /// # Arguments
    /// * `key` - Unique key for the type
    pub fn key(mut self, key: impl Into<String>) -> Self {
        self.key = Some(key.into());
        self
    }

    /// Sets the type icon.
    ///
    /// # Arguments
    /// * `icon` - Icon for the type
    pub fn icon(mut self, icon: Icon) -> Self {
        self.icon = Some(icon);
        self
    }

    /// Sets the default layout for objects of this type.
    ///
    /// # Arguments
    /// * `layout` - Default layout for new objects
    pub fn layout(mut self, layout: TypeLayout) -> Self {
        self.layout = layout;
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

    /// Adds a property definition to the type.
    ///
    /// # Arguments
    /// * `name` - name of property to add
    /// * `key` - property key
    /// * `format` - property format
    pub fn property(
        mut self,
        name: impl Into<String>,
        key: impl Into<String>,
        format: PropertyFormat,
    ) -> Self {
        self.properties.push({
            CreateTypeProperty {
                name: name.into(),
                key: key.into(),
                format,
            }
        });
        self
    }

    /// Adds multiple property definitions to the type.
    ///
    /// # Arguments
    /// * `properties` - Iterator of property definitions
    pub fn properties(mut self, properties: impl IntoIterator<Item = CreateTypeProperty>) -> Self {
        self.properties.extend(properties);
        self
    }

    /// Creates the type with the configured settings.
    ///
    /// # Returns
    /// The newly created type.
    ///
    /// # Errors
    /// - [`AnytypeError::Validation`] if name is not provided or invalid
    pub async fn create(self) -> Result<Type> {
        self.limits.validate_id(&self.space_id, "space_id")?;
        self.limits.validate_name(&self.name, "type name")?;

        let request_body = CreateTypeRequestBody {
            name: self.name,
            key: self.key,
            plural_name: self.plural_name,
            icon: self.icon,
            layout: self.layout,
            properties: self.properties,
        };

        let response: TypeResponse = self
            .client
            .post_request(
                &format!("/v1/spaces/{}/types", self.space_id),
                &request_body,
                Default::default(),
            )
            .await?;

        if self.cache.has_types(&self.space_id) {
            self.cache.set_type(&self.space_id, response.type_.clone());
        }
        let typ = response.type_;
        if let Some(config) = resolve_verify(self.verify_policy, &self.verify_config) {
            return verify_available(&config, "Type", &typ.id, || async {
                let response: TypeResponse = self
                    .client
                    .get_request(
                        &format!("/v1/spaces/{}/types/{}", self.space_id, typ.id),
                        Default::default(),
                    )
                    .await?;
                Ok(response.type_)
            })
            .await;
        }
        Ok(typ)
    }
}

/// Request builder for updating an existing type.
///
/// Obtained via [`AnytypeClient::update_type`].
///
#[derive(Debug)]
pub struct UpdateTypeRequest {
    client: Arc<HttpClient>,
    limits: ValidationLimits,
    space_id: String,
    type_id: String,
    name: Option<String>,
    key: Option<String>,
    plural_name: Option<String>,
    icon: Option<Icon>,
    layout: Option<TypeLayout>,
    properties: Vec<CreateTypeProperty>,
    cache: Arc<AnytypeCache>,
    verify_policy: VerifyPolicy,
    verify_config: Option<VerifyConfig>,
}

impl UpdateTypeRequest {
    /// Creates a new UpdateTypeRequest.
    pub(crate) fn new(
        client: Arc<HttpClient>,
        limits: ValidationLimits,
        space_id: impl Into<String>,
        type_id: impl Into<String>,
        cache: Arc<AnytypeCache>,
        verify_config: Option<VerifyConfig>,
    ) -> Self {
        Self {
            client,
            limits,
            space_id: space_id.into(),
            type_id: type_id.into(),
            name: None,
            key: None,
            plural_name: None,
            icon: None,
            layout: None,
            properties: Vec::new(),
            cache,
            verify_policy: VerifyPolicy::Default,
            verify_config,
        }
    }

    /// Updates the type name.
    ///
    /// # Arguments
    /// * `name` - New display name for the type
    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Updates the type key.
    ///
    /// # Arguments
    /// * `key` - New key for the type
    pub fn key(mut self, key: impl Into<String>) -> Self {
        self.key = Some(key.into());
        self
    }

    /// Updates the plural name.
    ///
    /// # Arguments
    /// * `plural_name` - New plural form of the type name
    pub fn plural_name(mut self, plural_name: impl Into<String>) -> Self {
        self.plural_name = Some(plural_name.into());
        self
    }

    /// Updates the type icon.
    ///
    /// # Arguments
    /// * `icon` - New icon for the type
    pub fn icon(mut self, icon: Icon) -> Self {
        self.icon = Some(icon);
        self
    }

    /// Updates the default layout.
    ///
    /// # Arguments
    /// * `layout` - New default layout for objects of this type
    pub fn layout(mut self, layout: TypeLayout) -> Self {
        self.layout = Some(layout);
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

    /// Adds a property definition to the type.
    ///
    /// # Arguments
    /// * `name` - name of property to add
    /// * `key` - property key
    /// * `format` - property format
    pub fn property(
        mut self,
        name: impl Into<String>,
        key: impl Into<String>,
        format: PropertyFormat,
    ) -> Self {
        self.properties.push({
            CreateTypeProperty {
                name: name.into(),
                key: key.into(),
                format,
            }
        });
        self
    }

    /// Applies the update to the type.
    ///
    /// # Returns
    /// The updated type.
    ///
    /// # Errors
    /// - [`AnytypeError::Validation`] if called without setting any fields
    /// - [`AnytypeError::NotFound`] if the type doesn't exist
    pub async fn update(self) -> Result<Type> {
        self.limits.validate_id(&self.space_id, "space_id")?;
        self.limits.validate_id(&self.type_id, "type_id")?;

        // Check that at least one field is being updated
        if self.name.is_none()
            && self.key.is_none()
            && self.plural_name.is_none()
            && self.icon.is_none()
            && self.layout.is_none()
            && self.properties.is_empty()
        {
            return Err(AnytypeError::Validation {
                message: "update_type: must set at least one field to update".to_string(),
            });
        }

        if let Some(ref name) = self.name {
            self.limits.validate_name(name, "type")?;
        }

        let request_body = UpdateTypeRequestBody {
            name: self.name,
            key: self.key,
            plural_name: self.plural_name,
            icon: self.icon,
            layout: self.layout,
            properties: self.properties,
        };

        let response: TypeResponse = self
            .client
            .patch_request(
                &format!("/v1/spaces/{}/types/{}", self.space_id, self.type_id),
                &request_body,
            )
            .await?;

        if self.cache.has_types(&self.space_id) {
            self.cache.set_type(&self.space_id, response.type_.clone())
        }

        let typ = response.type_;
        if let Some(config) = resolve_verify(self.verify_policy, &self.verify_config) {
            return verify_available(&config, "Type", &typ.id, || async {
                let response: TypeResponse = self
                    .client
                    .get_request(
                        &format!("/v1/spaces/{}/types/{}", self.space_id, typ.id),
                        Default::default(),
                    )
                    .await?;
                Ok(response.type_)
            })
            .await;
        }
        Ok(typ)
    }
}

/// Request builder for listing types in a space.
///
/// Obtained via [`AnytypeClient::types`].
///
#[derive(Debug)]
pub struct ListTypesRequest {
    client: Arc<HttpClient>,
    limits: ValidationLimits,
    space_id: String,
    limit: Option<usize>,
    offset: Option<usize>,
    filters: Vec<Filter>,
    cache: Arc<AnytypeCache>,
}

impl ListTypesRequest {
    /// Creates a new ListTypesRequest.
    pub(crate) fn new(
        client: Arc<HttpClient>,
        limits: ValidationLimits,
        space_id: impl Into<String>,
        cache: Arc<AnytypeCache>,
    ) -> Self {
        Self {
            client,
            limits,
            space_id: space_id.into(),
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
    /// A paginated result containing the matching types.
    ///
    /// To take advantage of cached properties for the `list()` method,
    /// the cache must be enabled, and  the query
    /// parameter must not contain filters or pagination limits or offsets.
    ///
    /// The response may include archived types,
    /// To exclude, filter returned values with `.filter(|typ| !typ.archived)`
    ///
    /// # Errors
    /// - [`AnytypeError::Validation`] if space_id is invalid
    pub async fn list(self) -> Result<PagedResult<Type>> {
        self.limits.validate_id(&self.space_id, "space_id")?;

        if self.cache.is_enabled()
            && self.limit.is_none()
            && self.offset.unwrap_or_default() == 0
            && self.filters.is_empty()
        {
            // see note on locking design in cache.rs
            if !self.cache.has_types(&self.space_id) {
                prime_cache_types(&self.client, &self.cache, &self.space_id).await?;
            }
            return Ok(PagedResult::from_items(
                self.cache
                    .types_for_space(&self.space_id)
                    .unwrap_or_default(),
            ));
        }

        // cache disabled, or query has limits or filters that need to be evaluated on the server
        let query = Query::default()
            .set_limit_opt(&self.limit)
            .set_offset_opt(&self.offset)
            .add_filters(&self.filters);

        self.client
            .get_request_paged(&format!("/v1/spaces/{}/types", self.space_id), query)
            .await
    }
}

/// Load all space types into cache.
async fn prime_cache_types(
    client: &Arc<HttpClient>,
    cache: &Arc<AnytypeCache>,
    space_id: &str,
) -> Result<()> {
    let types: Vec<Type> = client
        .get_request_paged(&format!("/v1/spaces/{space_id}/types"), Default::default())
        .await?
        .collect_all()
        .await?
        .into_iter()
        .filter(|t: &Type| !t.archived)
        .collect();
    cache.set_types(space_id, types);
    Ok(())
}

// ============================================================================
// ANYTYPECLIENT METHODS
// ============================================================================

impl AnytypeClient {
    /// Creates a request builder for getting or deleting a single type by id.
    /// To get by key, use [lookup_type_by_key](AnytypeClient::lookup_type_by_key)
    ///
    /// # Arguments
    /// * `space_id` - ID of the space containing the type
    /// * `type_id` - ID of the type
    ///
    /// # Example
    ///
    /// ```rust
    /// # use anytype::prelude::*;
    /// # async fn example() -> Result<(), AnytypeError> {
    /// #   let client = AnytypeClient::new("doc test")?;
    /// #   let space_id = anytype::test_util::example_space_id(&client).await?;
    /// #   let typ = client.lookup_type_by_key(&space_id, "page").await?;
    /// #   let type_id = &typ.id;
    /// let typ = client.get_type(&space_id, type_id).get().await?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn get_type(&self, space_id: impl Into<String>, type_id: impl Into<String>) -> TypeRequest {
        TypeRequest::new(
            self.client.clone(),
            self.config.limits.clone(),
            space_id,
            type_id,
            self.cache.clone(),
        )
    }

    /// Creates a request builder for creating a new type.
    /// - default plural name is name + 's'. Override with ".plural_name()"
    /// - default layout is Basic. Override with '.layout()"
    ///
    /// # Arguments
    /// * `space_id` - ID of the space to create the type in
    /// * `name` - type name
    ///
    /// # Example
    ///
    /// ```rust
    /// # use anytype::prelude::*;
    /// # async fn example() -> Result<(), AnytypeError> {
    /// #   let client = AnytypeClient::new("doc test")?;
    /// #   let space_id = anytype::test_util::example_space_id(&client).await?;
    ///
    /// let project = client.new_type(&space_id, "My Project")
    ///     .key("my_project")
    ///     .create().await?;
    ///
    /// # client.get_type(&space_id, &project.id).delete().await?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn new_type(&self, space_id: impl Into<String>, name: impl Into<String>) -> NewTypeRequest {
        let name = name.into();
        let plural_name = format!("{}s", &name);
        NewTypeRequest::new(
            self.client.clone(),
            self.config.limits.clone(),
            space_id,
            name,
            plural_name,
            self.cache.clone(),
            self.config.verify.clone(),
        )
    }

    /// Creates a request builder for updating an existing type.
    ///
    /// # Arguments
    /// * `space_id` - ID of the space containing the type
    /// * `type_id` - ID of the type to update
    ///
    /// # Example
    ///
    /// ```rust
    /// # use anytype::prelude::*;
    /// # async fn example() -> Result<(), AnytypeError> {
    /// #   let client = AnytypeClient::new("doc test")?;
    /// #   let space_id = anytype::test_util::example_space_id(&client).await?;
    ///
    /// let project = client.new_type(&space_id, "My Project")
    ///     .key("my_project")
    ///     .create().await?;
    ///
    /// // change name and add a text field "Location"
    /// let typ = client.update_type(&space_id, &project.id)
    ///     .name("My New Project")
    ///     .property("Location", "location", PropertyFormat::Text)
    ///     .update().await?;
    ///
    /// # client.get_type(&space_id, &project.id).delete().await?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn update_type(
        &self,
        space_id: impl Into<String>,
        type_id: impl Into<String>,
    ) -> UpdateTypeRequest {
        UpdateTypeRequest::new(
            self.client.clone(),
            self.config.limits.clone(),
            space_id,
            type_id,
            self.cache.clone(),
            self.config.verify.clone(),
        )
    }

    /// Creates a request builder for listing types in a space.
    ///
    /// # Arguments
    /// * `space_id` - ID of the space to list types from
    ///
    /// # Example
    ///
    /// ```rust
    /// use anytype::prelude::*;
    /// # async fn example() -> Result<(), AnytypeError> {
    /// #   let client = AnytypeClient::new("doc test")?;
    /// #   let space_id = anytype::test_util::example_space_id(&client).await?;
    ///
    /// let types = client.types(&space_id)
    ///     .limit(50)
    ///     .list().await?.collect_all().await?;
    /// for typ in types.iter() {
    ///     println!("{:20} {:20} {}", &typ.display_name(), &typ.key, &typ.id);
    /// }
    ///
    /// # Ok(())
    /// # }
    /// ```
    pub fn types(&self, space_id: impl Into<String>) -> ListTypesRequest {
        ListTypesRequest::new(
            self.client.clone(),
            self.config.limits.clone(),
            space_id,
            self.cache.clone(),
        )
    }

    /// Searches for type in space by id, key, or name using case-insensitive match
    /// Excludes archived types.
    ///
    /// # Example
    ///
    /// ```rust
    /// use anytype::prelude::*;
    /// # async fn example() -> Result<(), AnytypeError> {
    /// #   let client = AnytypeClient::new("doc test")?;
    /// #   let space_id = anytype::test_util::example_space_id(&client).await?;
    ///
    /// let types = client.lookup_types(&space_id, "page").await?;
    /// for typ in types.iter() {
    ///     println!("Type {}", &typ.display_name());
    /// }
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// Errors:
    /// - AnytypeError::NotFound if no type in the space matched
    /// - AnytypeError::CacheDisabled if cache is disabled
    /// - AnytypeError::* any other error
    pub async fn lookup_types(&self, space_id: &str, text: impl AsRef<str>) -> Result<Vec<Type>> {
        if self.cache.is_enabled() {
            // see note on locking design in cache.rs
            if !self.cache.has_types(space_id) {
                prime_cache_types(&self.client, &self.cache, space_id).await?;
            }
            match self.cache.lookup_types(space_id, text.as_ref()) {
                Some(types) if !types.is_empty() => {
                    Ok(types.into_iter().map(|arc| (*arc).clone()).collect())
                }
                _ => Err(AnytypeError::NotFound {
                    obj_type: "Type".to_string(),
                    key: text.as_ref().to_string(),
                }),
            }
        } else {
            Err(AnytypeError::CacheDisabled)
        }
    }

    /// Searches for type in space by key.
    /// Excludes archived types.
    ///
    /// # Example
    ///
    /// ```rust
    /// # use anytype::prelude::*;
    /// # async fn example() -> Result<(), AnytypeError> {
    /// #   let client = AnytypeClient::new("doc test")?;
    /// #   let space_id = anytype::test_util::example_space_id(&client).await?;
    ///
    /// let typ = client.lookup_type_by_key(&space_id, "page").await?;
    /// println!("Type {} key:{} id:{}", &typ.display_name(), &typ.key, &typ.id);
    ///
    /// # Ok(())
    /// # }
    /// ```
    /// Errors:
    /// - AnytypeError::NotFound if no type in the space matched
    /// - AnytypeError::CacheDisabled if cache is disabled
    /// - AnytypeError::* any other error
    ///
    pub async fn lookup_type_by_key(&self, space_id: &str, text: impl AsRef<str>) -> Result<Type> {
        if self.cache.is_enabled() {
            // see note on locking design in cache.rs
            if !self.cache.has_types(space_id) {
                prime_cache_types(&self.client, &self.cache, space_id).await?;
            }
            match self.cache.lookup_type_by_key(space_id, text.as_ref()) {
                Some(typ) => Ok((*typ).clone()),
                None => Err(AnytypeError::NotFound {
                    obj_type: "Type".to_string(),
                    key: text.as_ref().to_string(),
                }),
            }
        } else {
            Err(AnytypeError::CacheDisabled)
        }
    }
}

// ============================================================================
// TESTS
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_type_layout_default() {
        let layout: TypeLayout = Default::default();
        assert_eq!(layout, TypeLayout::Basic);
    }

    #[test]
    fn test_type_layout_display() {
        assert_eq!(TypeLayout::Basic.to_string(), "basic");
        assert_eq!(TypeLayout::Note.to_string(), "note");
        assert_eq!(TypeLayout::Action.to_string(), "action");
    }

    #[test]
    fn test_type_layout_from_string() {
        use std::str::FromStr;
        assert_eq!(TypeLayout::from_str("basic").unwrap(), TypeLayout::Basic);
        assert_eq!(TypeLayout::from_str("note").unwrap(), TypeLayout::Note);
    }

    #[test]
    fn test_type_is_system_type() {
        let page_type = Type {
            archived: false,
            id: "id".to_string(),
            key: "page".to_string(),
            name: Some("Page".to_string()),
            plural_name: None,
            icon: None,
            layout: ObjectLayout::Basic,
            properties: vec![],
        };
        assert!(page_type.is_system_type());

        let custom_type = Type {
            archived: false,
            id: "id".to_string(),
            key: "project".to_string(),
            name: Some("Project".to_string()),
            plural_name: None,
            icon: None,
            layout: ObjectLayout::Basic,
            properties: vec![],
        };
        assert!(!custom_type.is_system_type());
    }

    #[test]
    fn test_type_display_name() {
        let with_name = Type {
            archived: false,
            id: "id".to_string(),
            key: "page".to_string(),
            name: Some("Page".to_string()),
            plural_name: None,
            icon: None,
            layout: ObjectLayout::Basic,
            properties: vec![],
        };
        assert_eq!(with_name.display_name(), "Page");

        let without_name = Type {
            archived: false,
            id: "id".to_string(),
            key: "custom_type".to_string(),
            name: None,
            plural_name: None,
            icon: None,
            layout: ObjectLayout::Basic,
            properties: vec![],
        };
        assert_eq!(without_name.display_name(), "custom_type");
    }
}
