//! # Anytype Objects
//!
//! This module provides a fluent builder API for working with Anytype objects.
//!
//! ## Object methods on `AnytypeClient`
//!
//! - [`objects`](AnytypeClient::objects) - list objects in the space
//! - [`object`](AnytypeClient::object) - get or delete object
//! - [`new_object`](AnytypeClient::new_object) - create a new object
//! - [`update_object`](AnytypeClient::object) - update object properties
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
//! // Create an object
//! let obj = client.new_object(&space_id, "page")
//!     .name("My Document")
//!     .body("# Hello World")
//!     .create().await?;
//!
//! // Get an object
//! let obj = client.object(&space_id, &obj.id).get().await?;
//!
//! // Update an object
//! let obj = client.update_object(&space_id, &obj.id)
//!     .name("Updated Name")
//!     .update().await?;
//!
//! // List pages in space
//! let results = client.objects(&space_id)
//!     .filter(Filter::type_in(vec!["page"]))
//!     .list().await?.collect_all().await?;
//!
//! for page in results {
//!     println!("{} {}", &page.id, page.name.as_deref().unwrap_or("(unnamed)"));
//! }
//!
//! // Delete an object
//! client.object(&space_id, &obj.id).delete().await?;
//! # Ok(())
//! # }
//! ```
//!
//! ## Types
//!
//! - [`Object`] - Represents an Anytype object with its properties
//! - [`ObjectLayout`] - Layout variants for objects (Basic, Profile, Note, etc.)
//! - [`ObjectRequest`] - Builder for get/delete operations
//! - [`NewObjectRequest`] - Builder for creating new objects
//! - [`UpdateObjectRequest`] - Builder for updating existing objects
//! - [`ListObjectsRequest`] - Builder for listing objects with filters

use std::sync::Arc;

#[cfg(feature = "grpc")]
use anytype_rpc::{anytype::rpc::object::share_by_link, auth::with_token};
use chrono::{DateTime, FixedOffset};
use serde::{Deserialize, Serialize};
use serde_json::{Number, Value};
use snafu::prelude::*;
#[cfg(feature = "grpc")]
use tonic::Request;

use crate::{
    Result,
    client::AnytypeClient,
    filters::{Query, QueryWithFilters},
    http_client::{GetPaged, HttpClient},
    prelude::*,
    verify::{VerifyConfig, VerifyPolicy, resolve_verify, verify_available},
};

/// returns web url to object
pub fn object_link(space_id: &str, object_id: &str) -> String {
    format!("https://object.any.coop/{object_id}?spaceId={space_id}")
}

/// returns web url to object with invite
pub fn object_link_shared(space_id: &str, object_id: &str, cid: &str, key: &str) -> String {
    format!("https://object.any.coop/{object_id}?spaceId={space_id}&inviteId={cid}#{key}")
}

/// Layout variants for objects.
///
/// Determines how an object is displayed and what features are available.
#[derive(
    Debug, Deserialize, Serialize, Clone, Default, PartialEq, Eq, strum::Display, strum::EnumString,
)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum ObjectLayout {
    /// Standard object layout with full editing capabilities
    #[default]
    Basic,
    /// Profile layout for user/contact information
    Profile,
    /// Action/task layout
    Action,
    /// Note layout - simplified, name is optional
    Note,
    /// Bookmark layout - for saved web links
    Bookmark,
    /// Set layout - a query-based collection
    Set,
    /// Collection layout - a manually curated list
    Collection,
    /// Participant layout - space member representation
    Participant,
    //
    // undocumented: seen in go source (models.pb.go)
    // Should we add an Other(String) variant to handle deserialization?
    // ============
    //
    // todo
    // objectType
    // relation
    // file
    // dashboard
    // image
    // space
    // relationOptionsList
    // relationOption
    // audio
    // video
    // date
    // spaceView
    // pdf
    // chatDerived
    // tag
    // notification
    // missingObject
    // devices
}

/// Object data model
#[derive(Debug, Deserialize, Clone, PartialEq, Eq, strum::Display, Default, Serialize)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum DataModel {
    Chat, // Space
    Error,
    Member,
    #[default]
    Object,
    Property,
    Space,
    Tag,
    Type,
}

/// Color for Tags and Icons
#[derive(
    Debug, Serialize, Deserialize, Clone, PartialEq, strum::Display, Eq, strum::EnumString,
)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum Color {
    Grey,
    Yellow,
    Orange,
    Red,
    Pink,
    Purple,
    Blue,
    Ice,
    Teal,
    Lime,
}

/// Icon type
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "format", rename_all = "lowercase")]
pub enum Icon {
    /// icon emoji
    Emoji {
        emoji: String,
    },

    /// Icon file path (must be utf8 string)
    File {
        file: String,
    },

    Icon {
        color: Color,
        name: String,
    },
}

impl Icon {
    /// Returns icon emoji
    pub fn as_emoji(&self) -> Option<&str> {
        if let Self::Emoji { emoji } = self {
            Some(emoji.as_str())
        } else {
            None
        }
    }

    /// Returns icon file path
    pub fn as_file(&self) -> Option<&str> {
        if let Self::File { file } = self {
            Some(file.as_str())
        } else {
            None
        }
    }

    /// Returns (Name, Color) if icon is type Icon.
    pub fn as_icon(&self) -> Option<(&str, Color)> {
        if let Self::Icon { name, color } = self {
            Some((name.as_str(), color.clone()))
        } else {
            None
        }
    }
}

/// Objects are the core data unit in Anytype. Each object has a type,
/// properties, and optional markdown body content.
///
/// This structure represents Objects returned from various api functions.
/// Objects are created with `AnytypeClient::new_object()`.
///
/// ## Markdown (Object body)
///
/// The object body is a markdown-formatted string defined as `markdown: Option<String>`.
///
/// - When a function returns one Object, the markdown field contains `Some(content)`.
///    - object { create, get, delete, update }
///    - template { get }
///    
/// - When a function returns a list of objects, the `Object`s do not
///   include the body (`markdown` is `None`):
///   - `object list()`, `search_global.execute()`, `search_in.execute()`.
///
/// If you get a list of objects and need markdown, you'll need to
/// make an extra call per `object_id`:
///    `client::object(space_id,object_id).get().await`
//
// Implementation note:
// - In the anytype api, this struct is only received, never sent.
//   Why do we derive Serialize? So the cli can generate json output.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Object {
    /// Whether the object is archived (soft-deleted)
    pub archived: bool,

    /// Object icon (emoji, file, or icon with color)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<Icon>,

    /// Unique object identifier
    /// Example: "bafyreie6n5l5nkbjal37su54cha4coy7qzuhrnajluzv5qd5jvtsrxkequ"
    pub id: String,

    /// Layout of the object
    #[serde(default)]
    pub layout: ObjectLayout,

    /// Markdown body content of the object
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub markdown: Option<String>,

    /// Display name of the object
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,

    /// Data model type (always "object" for objects)
    #[serde(default)]
    pub object: DataModel,

    /// Object properties with their values
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub properties: Vec<PropertyWithValue>,

    /// Content snippet, especially useful for notes without names
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snippet: Option<String>,

    /// ID of the space containing this object
    pub space_id: String,

    /// Type of the object (may be None if type was deleted or object is itself a Type)
    #[serde(rename = "type")]
    pub r#type: Option<Type>,
}

impl Object {
    /// Returns the object's type information, if available.
    pub fn get_type(&self) -> Option<Type> {
        self.r#type.clone()
    }

    /// Finds a property by its key.
    ///
    /// # Arguments
    /// * `key` - The property key to search for
    ///
    /// # Returns
    /// The property with its value, or None if not found
    pub fn get_property(&self, key: &str) -> Option<&PropertyWithValue> {
        self.properties.iter().find(|prop| prop.key == key)
    }

    /// Returns a string value for string-like properties (text, date, url, phone, email, select).
    ///
    /// # Arguments
    /// * `key` - The property key
    ///
    /// # Returns
    /// The string value, or None if property not found or not a string type
    pub fn get_property_str<'a>(&'a self, key: &str) -> Option<&'a str> {
        self.get_property(key).and_then(|prop| prop.value.as_str())
    }

    /// Returns a numeric property value as a JSON Number.
    /// See also
    ///  - [`get_property_u64`](Object::get_property_u64)
    ///  - [`get_property_i64`](Object::get_property_i64)
    ///  - [`get_property_f64`](Object::get_property_f64)
    ///
    /// # Arguments
    /// * `key` - The property key
    ///
    /// # Returns
    /// The Number value, or None if property not found or not numeric
    pub fn get_property_number<'a>(&'a self, key: &str) -> Option<&'a Number> {
        self.get_property(key)
            .and_then(|prop| prop.value.as_number())
    }

    /// Returns the property as a `chrono::DateTime`, in the stored time zone.
    /// If the property is not defined, or is not a Date, returns None.
    pub fn get_property_date(&self, key: &str) -> Option<DateTime<FixedOffset>> {
        self.get_property(key).and_then(|prop| prop.value.as_date())
    }

    /// Returns a numeric property value as f64.
    ///
    /// # Arguments
    /// * `key` - The property key
    ///
    /// # Returns
    /// The f64 value, or None if property was not found or cannot be converted
    pub fn get_property_f64(&self, key: &str) -> Option<f64> {
        self.get_property_number(key).and_then(Number::as_f64)
    }

    /// Returns a numeric property value as u64.
    ///
    /// # Arguments
    /// * `key` - The property key
    ///
    /// # Returns
    /// The u64 value, or None if property was not found or cannot be converted
    pub fn get_property_u64(&self, key: &str) -> Option<u64> {
        self.get_property_number(key).and_then(Number::as_u64)
    }

    /// Returns a numeric property value as u64.
    ///
    /// # Arguments
    /// * `key` - The property key
    ///
    /// # Returns
    /// The i64 value, or None if property was not found or cannot be converted
    pub fn get_property_i64(&self, key: &str) -> Option<i64> {
        self.get_property_number(key).and_then(Number::as_i64)
    }

    /// Returns a checkbox (boolean) property value.
    ///
    /// # Arguments
    /// * `key` - The property key
    ///
    /// # Returns
    /// The boolean value, or None if property not found or not a checkbox
    pub fn get_property_bool(&self, key: &str) -> Option<bool> {
        self.get_property(key).and_then(|prop| prop.value.as_bool())
    }

    /// Returns an array property value (`multi_select`, files, objects).
    ///
    /// # Arguments
    /// * `key` - The property key
    ///
    /// # Returns
    /// The array of strings, or None if property not found or not an array type
    pub fn get_property_array(&self, key: &str) -> Option<Vec<String>> {
        self.get_property(key)
            .and_then(|prop| prop.value.as_array())
    }

    /// Checks if a property exists (useful for empty properties).
    ///
    /// # Arguments
    /// * `key` - The property key
    ///
    /// # Returns
    /// Some(()) if the property exists, None otherwise
    pub fn get_property_empty(&self, key: &str) -> Option<()> {
        self.get_property(key).map(|_| ())
    }

    /// Returns Tag value of select property
    ///
    /// # Arguments
    /// * `key` - The property key
    ///
    /// # Returns
    /// The select value, or None if property not found or not Select type
    pub fn get_property_select(&self, key: &str) -> Option<&Tag> {
        self.get_property(key).and_then(|prop| prop.value.as_tag())
    }

    /// Returns Tags value of multi-select property
    ///
    /// # Arguments
    /// * `key` - The property key
    ///
    /// # Returns
    /// The select values, or None if property not found or not `MultiSelect` type
    pub fn get_property_multi_select(&self, key: &str) -> Option<&[Tag]> {
        self.get_property(key).and_then(|prop| prop.value.as_tags())
    }

    /// Returns web link to object
    pub fn get_link(&self) -> String {
        object_link(&self.space_id, &self.id)
    }

    /// Returns web link to object with share invite
    pub fn get_link_shared(&self, cid: &str, key: &str) -> Result<String> {
        ensure!(
            !cid.is_empty() && !key.is_empty(),
            ValidationSnafu {
                message: "Invalid share link".to_string()
            }
        );
        Ok(object_link_shared(&self.space_id, &self.id, cid, key))
    }
}

// ============================================================================
// RESPONSE TYPES (internal)
// ============================================================================

/// Response wrapper for single object operations
#[derive(Debug, Deserialize)]
pub(crate) struct ObjectResponse {
    pub object: Object,
}

// ============================================================================
// REQUEST BODY TYPES (internal)
// ============================================================================

/// Internal request body for creating an object
#[derive(Debug, Serialize)]
struct CreateObjectRequestBody {
    type_key: String,

    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    body: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    icon: Option<Icon>,

    #[serde(skip_serializing_if = "Option::is_none")]
    template_id: Option<String>,

    #[serde(skip_serializing_if = "Vec::is_empty")]
    properties: Vec<Value>,
}

/// Internal request body for updating an object
#[derive(Debug, Serialize, Default)]
struct UpdateObjectRequestBody {
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    markdown: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    icon: Option<Icon>,

    #[serde(skip_serializing_if = "Option::is_none")]
    type_key: Option<String>,

    #[serde(skip_serializing_if = "Vec::is_empty")]
    properties: Vec<Value>,
}

// ============================================================================
// BUILDER STRUCTS (public)
// ============================================================================

/// Request builder for getting or deleting a single object.
///
/// Obtained via [`AnytypeClient::object`].
///
/// # Example
///
/// ```rust
/// # use anytype::prelude::*;
/// # async fn example() -> Result<(), AnytypeError> {
/// #   let client = AnytypeClient::new("doc test")?;
/// #   let space_id = anytype::test_util::example_space_id(&client).await?;
/// #   // get an object id we know exists
/// #   let obj_id = client.lookup_property_by_key(&space_id, "page").await.unwrap().id;
///
/// // Get an object
/// let obj = client.object(&space_id, &obj_id).get().await?;
///
/// # // create dummy object for deletion
/// # let obj = client.new_object(&space_id, "page")
/// #    .name("throwaway-object").body("# Hello World").create().await?;
///
/// // Delete an object
/// let archived = client.object(&space_id, &obj.id).delete().await?;
/// # Ok(())
/// # }
/// ```
#[derive(Debug)]
pub struct ObjectRequest {
    client: Arc<HttpClient>,
    limits: ValidationLimits,
    space_id: String,
    object_id: String,
}

impl ObjectRequest {
    /// Creates a new `ObjectRequest`.
    pub(crate) fn new(
        client: Arc<HttpClient>,
        limits: ValidationLimits,
        space_id: impl Into<String>,
        object_id: impl Into<String>,
    ) -> Self {
        Self {
            client,
            limits,
            space_id: space_id.into(),
            object_id: object_id.into(),
        }
    }

    /// Retrieves the object by ID.
    ///
    /// # Returns
    /// The object with all its properties and content.
    ///
    /// # Errors
    /// - [`AnytypeError::NotFound`] if the object doesn't exist
    /// - [`AnytypeError::Validation`] if IDs are invalid
    pub async fn get(self) -> Result<Object> {
        self.limits.validate_id(&self.space_id, "space_id")?;
        self.limits.validate_id(&self.object_id, "object_id")?;

        let response: ObjectResponse = self
            .client
            .get_request(
                &format!("/v1/spaces/{}/objects/{}", self.space_id, self.object_id),
                QueryWithFilters::default(),
            )
            .await?;
        Ok(response.object)
    }

    /// Deletes (archives) the object.
    ///
    /// Objects are soft-deleted by marking them as archived.
    ///
    /// # Returns
    /// The archived object with `archived: true`.
    ///
    /// # Errors
    /// - [`AnytypeError::NotFound`] if the object doesn't exist
    /// - [`AnytypeError::Forbidden`] if you don't have permission
    pub async fn delete(self) -> Result<Object> {
        self.limits.validate_id(&self.space_id, "space_id")?;
        self.limits.validate_id(&self.object_id, "object_id")?;

        let response: ObjectResponse = self
            .client
            .delete_request(&format!(
                "/v1/spaces/{}/objects/{}",
                self.space_id, self.object_id
            ))
            .await?;
        Ok(response.object)
    }
}

/// Request builder for creating a new object.
///
/// Obtained via [`AnytypeClient::new_object`].
///
/// # Example
///
/// ```rust
/// use anytype::prelude::*;
/// # async fn example() -> Result<(), AnytypeError> {
/// #   let client = AnytypeClient::new("doc test")?;
/// #   let space_id = anytype::test_util::example_space_id(&client).await?;
///
/// let obj = client.new_object(&space_id, "page")
///     .name("My Document")
///     .body("# Hello World\n\nThis is my document.")
///     .description("A sample document")
///     .create().await?;
///
/// # client.object(&space_id, &obj.id).delete().await?;
/// # Ok(())
/// # }
/// ```
#[derive(Debug)]
pub struct NewObjectRequest {
    client: Arc<HttpClient>,
    limits: ValidationLimits,
    space_id: String,
    type_key: String,
    name: Option<String>,
    body: Option<String>,
    icon: Option<Icon>,
    template_id: Option<String>,
    properties: Vec<Value>,
    verify_policy: VerifyPolicy,
    verify_config: Option<VerifyConfig>,
}

impl NewObjectRequest {
    /// Creates a new `NewObjectRequest`.
    pub(crate) fn new(
        client: Arc<HttpClient>,
        limits: ValidationLimits,
        space_id: impl Into<String>,
        type_key: impl Into<String>,
        verify_config: Option<VerifyConfig>,
    ) -> Self {
        Self {
            client,
            limits,
            space_id: space_id.into(),
            type_key: type_key.into(),
            name: None,
            body: None,
            icon: None,
            template_id: None,
            properties: Vec::new(),
            verify_policy: VerifyPolicy::Default,
            verify_config,
        }
    }

    /// Returns the `type_key` for this request
    pub fn get_type_key(&self) -> &str {
        &self.type_key
    }

    ///
    /// Sets the object name.
    ///
    /// # Arguments
    /// * `name` - Display name for the object
    #[must_use]
    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Sets the object body content (markdown format).
    ///
    /// # Arguments
    /// * `body` - Markdown content for the object body
    #[must_use]
    pub fn body(mut self, body: impl Into<String>) -> Self {
        self.body = Some(body.into());
        self
    }

    /// Sets the object icon.
    ///
    /// # Arguments
    /// * `icon` - Icon for the object (emoji, file, or colored icon)
    #[must_use]
    pub fn icon(mut self, icon: Icon) -> Self {
        self.icon = Some(icon);
        self
    }

    /// Sets the template to use for object creation.
    ///
    /// # Arguments
    /// * `template_id` - ID of the template to apply
    #[must_use]
    pub fn template(mut self, template_id: impl Into<String>) -> Self {
        self.template_id = Some(template_id.into());
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

    /// Sets the description property.
    ///
    /// This is a convenience method for setting the "description" text property.
    ///
    /// # Arguments
    /// * `description` - Description text
    #[must_use]
    pub fn description(self, description: impl Into<String>) -> Self {
        self.set_text("description", description)
    }

    /// Sets the URL property (required for bookmark objects).
    ///
    /// # Arguments
    /// * `url` - URL for the bookmark
    #[must_use]
    pub fn url(self, url: impl Into<String>) -> Self {
        self.set_url("url", url)
    }

    /// Creates the object with the configured settings.
    ///
    /// # Returns
    /// The newly created object.
    ///
    /// # Errors
    /// - [`AnytypeError::Validation`] if required fields are missing or invalid
    /// - [`AnytypeError::Forbidden`] if you don't have permission
    pub async fn create(self) -> Result<Object> {
        self.limits.validate_id(&self.space_id, "space_id")?;
        if let Some(ref name) = self.name {
            self.limits.validate_name(name, "name")?;
        }
        if let Some(ref body) = self.body {
            self.limits.validate_markdown(body, "body")?;
        }

        let request_body = CreateObjectRequestBody {
            type_key: self.type_key,
            name: self.name,
            body: self.body,
            icon: self.icon,
            template_id: self.template_id,
            properties: self.properties,
        };

        let response: ObjectResponse = self
            .client
            .post_request(
                &format!("/v1/spaces/{}/objects", self.space_id),
                &request_body,
                QueryWithFilters::default(),
            )
            .await?;

        let object = response.object;
        if let Some(config) = resolve_verify(self.verify_policy, self.verify_config.as_ref()) {
            return verify_available(&config, "Object", &object.id, || async {
                let response: ObjectResponse = self
                    .client
                    .get_request(
                        &format!("/v1/spaces/{}/objects/{}", self.space_id, object.id),
                        QueryWithFilters::default(),
                    )
                    .await?;
                Ok(response.object)
            })
            .await;
        }
        Ok(object)
    }
}

impl SetProperty for NewObjectRequest {
    fn add_property(mut self, property: Value) -> Self {
        self.properties.push(property);
        self
    }
}

/// Request builder for updating an existing object.
/// You can change then name, type, description, or other properties on the object.
///
/// Note that to set a property on an object, the property must be defined in the object's type.
///
/// Obtained via [`AnytypeClient::update_object`].
///
/// # Example
///
/// ```rust,no_run
/// use anytype::prelude::*;
/// # async fn example() -> Result<(), AnytypeError> {
/// #   let client = AnytypeClient::new("doc test")?;
/// #   let space_id = anytype::test_util::example_space_id(&client).await?;
/// # let obj = client.new_object(&space_id, "page")
/// #     .name("My Document")
/// #     .body("# Hello World")
/// #     .create().await?;
///
/// client.update_object(&space_id, &obj.id)
///     .name("Updated Name")
///     .body("# Updated Content")
///     .update().await?;
///
/// # client.object(&space_id, &obj.id).delete().await?;
/// # Ok(())
/// # }
/// ```
#[derive(Debug)]
pub struct UpdateObjectRequest {
    client: Arc<HttpClient>,
    limits: ValidationLimits,
    space_id: String,
    object_id: String,
    name: Option<String>,
    body: Option<String>,
    icon: Option<Icon>,
    type_key: Option<String>,
    properties: Vec<Value>,
    verify_policy: VerifyPolicy,
    verify_config: Option<VerifyConfig>,
}

impl UpdateObjectRequest {
    /// Creates a new `UpdateObjectRequest`.
    pub(crate) fn new(
        client: Arc<HttpClient>,
        limits: ValidationLimits,
        space_id: impl Into<String>,
        object_id: impl Into<String>,
        verify_config: Option<VerifyConfig>,
    ) -> Self {
        Self {
            client,
            limits,
            space_id: space_id.into(),
            object_id: object_id.into(),
            name: None,
            body: None,
            icon: None,
            type_key: None,
            properties: Vec::new(),
            verify_policy: VerifyPolicy::Default,
            verify_config,
        }
    }

    /// Returns the update `type_key` for this request, if defined
    pub fn get_type_key(&self) -> Option<String> {
        self.type_key.clone()
    }

    /// Updates the object name.
    ///
    /// # Arguments
    /// * `name` - New display name for the object
    #[must_use]
    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Updates the object body content (markdown format).
    ///
    /// # Arguments
    /// * `body` - New markdown content for the object body
    #[must_use]
    pub fn body(mut self, body: impl Into<String>) -> Self {
        self.body = Some(body.into());
        self
    }

    /// Updates the object icon.
    ///
    /// # Arguments
    /// * `icon` - New icon for the object
    #[must_use]
    pub fn icon(mut self, icon: Icon) -> Self {
        self.icon = Some(icon);
        self
    }

    /// Changes the object's type.
    ///
    /// # Arguments
    /// * `type_key` - Key of the new type (e.g., "page", "task")
    #[must_use]
    pub fn type_key(mut self, type_key: impl Into<String>) -> Self {
        self.type_key = Some(type_key.into());
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

    /// Applies the update to the object.
    ///
    /// # Returns
    /// The updated object.
    ///
    /// # Errors
    /// - [`AnytypeError::Validation`] if called without setting any fields
    /// - [`AnytypeError::NotFound`] if the object doesn't exist
    /// - [`AnytypeError::Forbidden`] if you don't have permission
    pub async fn update(self) -> Result<Object> {
        self.limits.validate_id(&self.space_id, "space_id")?;
        self.limits.validate_id(&self.object_id, "object_id")?;

        // Check that at least one field is being updated
        ensure!(
            self.name.is_some()
                || self.body.is_some()
                || self.icon.is_some()
                || self.type_key.is_some()
                || !self.properties.is_empty(),
            ValidationSnafu {
                message: "update_object: must set at least one field to update".to_string(),
            }
        );

        if let Some(ref name) = self.name {
            self.limits.validate_name(name, "name")?;
        }
        if let Some(ref body) = self.body {
            self.limits.validate_markdown(body, "body")?;
        }

        let request_body = UpdateObjectRequestBody {
            name: self.name,
            markdown: self.body,
            icon: self.icon,
            type_key: self.type_key,
            properties: self.properties,
        };

        let response: ObjectResponse = self
            .client
            .patch_request(
                &format!("/v1/spaces/{}/objects/{}", self.space_id, self.object_id),
                &request_body,
            )
            .await?;

        let object = response.object;
        if let Some(config) = resolve_verify(self.verify_policy, self.verify_config.as_ref()) {
            return verify_available(&config, "Object", &object.id, || async {
                let response: ObjectResponse = self
                    .client
                    .get_request(
                        &format!("/v1/spaces/{}/objects/{}", self.space_id, object.id),
                        QueryWithFilters::default(),
                    )
                    .await?;
                Ok(response.object)
            })
            .await;
        }
        Ok(object)
    }
}

impl SetProperty for UpdateObjectRequest {
    fn add_property(mut self, property: Value) -> Self {
        self.properties.push(property);
        self
    }
}

/// Request builder for listing objects in a space.
///
/// Obtained via [`AnytypeClient::objects`].
///
/// # Example
///
/// ```rust
/// # use anytype::prelude::*;
/// # async fn example() -> Result<(), AnytypeError> {
/// #   let client = AnytypeClient::new("doc test")?;
/// #   let space_id = anytype::test_util::example_space_id(&client).await?;
/// let results = client.objects(&space_id)
///     .limit(50)
///     .list().await?;
///
/// for obj in results.iter() {
///     println!("{}", obj.name.as_deref().unwrap_or("(unnamed)"));
/// }
/// # Ok(())
/// # }
/// ```
#[derive(Debug)]
pub struct ListObjectsRequest {
    client: Arc<HttpClient>,
    limits: ValidationLimits,
    space_id: String,
    limit: Option<u32>,
    offset: Option<u32>,
    filters: Vec<Filter>,
}

impl ListObjectsRequest {
    /// Creates a new `ListObjectsRequest`.
    pub(crate) fn new(
        client: Arc<HttpClient>,
        limits: ValidationLimits,
        space_id: impl Into<String>,
    ) -> Self {
        Self {
            client,
            limits,
            space_id: space_id.into(),
            limit: None,
            offset: None,
            filters: Vec::new(),
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
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use anytype::prelude::*;
    /// # async fn example(client: &AnytypeClient) -> Result<(), AnytypeError> {
    /// let results = client.objects("space_id")
    ///     .filter(Filter::select_in("status", vec!["open"]))
    ///     .filter(Filter::text_contains("name", "task"))
    ///     .list().await?;
    /// # Ok(())
    /// # }
    /// ```
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
    /// A paginated result containing the matching objects.
    ///
    /// Note: the response may include archived objects,
    /// To exclude, filter returned values with `.filter(|obj| !obj.archived)`
    ///
    /// # Errors
    /// - [`AnytypeError::Validation`] if `space_id` is invalid
    pub async fn list(self) -> Result<PagedResult<Object>> {
        self.limits.validate_id(&self.space_id, "space_id")?;

        let query = Query::default()
            .set_limit_opt(self.limit)
            .set_offset_opt(self.offset)
            .add_filters(&self.filters);

        self.client
            .get_request_paged(&format!("/v1/spaces/{}/objects", self.space_id), query)
            .await
    }
}

// ============================================================================
// ANYTYPECLIENT METHODS
// ============================================================================

impl AnytypeClient {
    /// Creates a request builder for getting or deleting a single object.
    ///
    /// # Arguments
    /// * `space_id` - ID of the space containing the object
    /// * `object_id` - ID of the object
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use anytype::prelude::*;
    /// # async fn example(client: &AnytypeClient) -> Result<(), AnytypeError> {
    /// // Get an object
    /// let obj = client.object("space_id", "object_id").get().await?;
    ///
    /// // Delete an object
    /// client.object("space_id", "object_id").delete().await?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn object(
        &self,
        space_id: impl Into<String>,
        object_id: impl Into<String>,
    ) -> ObjectRequest {
        ObjectRequest::new(
            self.client.clone(),
            self.config.limits.clone(),
            space_id,
            object_id,
        )
    }

    /// Creates a request builder for creating a new object.
    ///
    /// # Arguments
    /// * `space_id` - ID of the space to create the object in
    /// * `type_key` - Type of object to create (e.g., "page", "task", "bookmark")
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use anytype::prelude::*;
    /// # async fn example(client: &AnytypeClient) -> Result<(), AnytypeError> {
    /// let obj = client.new_object("space_id", "page")
    ///     .name("My Document")
    ///     .body("# Hello World")
    ///     .create().await?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn new_object(
        &self,
        space_id: impl Into<String>,
        type_key: impl Into<String>,
    ) -> NewObjectRequest {
        NewObjectRequest::new(
            self.client.clone(),
            self.config.limits.clone(),
            space_id,
            type_key,
            self.config.verify.clone(),
        )
    }

    /// Creates a request builder for updating an existing object.
    ///
    /// # Arguments
    /// * `space_id` - ID of the space containing the object
    /// * `object_id` - ID of the object to update
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use anytype::prelude::*;
    /// # async fn example(client: &AnytypeClient) -> Result<(), AnytypeError> {
    /// let obj = client.update_object("space_id", "object_id")
    ///     .name("Updated Name")
    ///     .update().await?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn update_object(
        &self,
        space_id: impl Into<String>,
        object_id: impl Into<String>,
    ) -> UpdateObjectRequest {
        UpdateObjectRequest::new(
            self.client.clone(),
            self.config.limits.clone(),
            space_id,
            object_id,
            self.config.verify.clone(),
        )
    }

    /// Creates a request builder for listing objects in a space.
    ///
    /// # Arguments
    /// * `space_id` - ID of the space to list objects from
    ///
    /// # Example
    ///
    /// ```rust
    /// # use anytype::prelude::*;
    /// # async fn example() -> Result<(), AnytypeError> {
    /// #   let client = AnytypeClient::new("doc test")?;
    /// #   let space_id = anytype::test_util::example_space_id(&client).await?;
    /// let results = client.objects(&space_id)
    ///     .limit(50)
    ///     .list().await?.collect_all().await?;
    /// for page in results.iter() {
    ///     println!("{} {}", page.id, page.name.as_deref().unwrap_or("(unnamed)"));
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub fn objects(&self, space_id: impl Into<String>) -> ListObjectsRequest {
        ListObjectsRequest::new(self.client.clone(), self.config.limits.clone(), space_id)
    }

    /// Get a share link for an object by id.
    #[cfg(feature = "grpc")]
    pub async fn get_share_link(&self, object_id: impl AsRef<str>) -> Result<String> {
        let object_id = object_id.as_ref();
        self.config.limits.validate_id(object_id, "object_id")?;

        let grpc = self.grpc_client().await?;
        let mut commands = grpc.client_commands();
        let request = share_by_link::Request {
            object_id: object_id.to_string(),
        };
        let request = with_token_request(Request::new(request), grpc.token())?;
        let response = commands
            .object_share_by_link(request)
            .await
            .map_err(grpc_status)?
            .into_inner();

        if let Some(error) = response.error
            && error.code != 0
        {
            return Err(AnytypeError::Other {
                message: format!(
                    "grpc share by link failed: {} (code {})",
                    error.description, error.code
                ),
            });
        }

        Ok(response.link)
    }
}

#[cfg(feature = "grpc")]
fn with_token_request<T>(request: Request<T>, token: &str) -> Result<Request<T>> {
    with_token(request, token).map_err(|err| AnytypeError::Auth {
        message: err.to_string(),
    })
}

#[cfg(feature = "grpc")]
#[allow(clippy::needless_pass_by_value)]
fn grpc_status(status: tonic::Status) -> AnytypeError {
    AnytypeError::Other {
        message: format!("gRPC request failed: {status}"),
    }
}

// ============================================================================
// TESTS
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_object_layout_default() {
        let layout: ObjectLayout = ObjectLayout::default();
        assert_eq!(layout, ObjectLayout::Basic);
    }

    #[test]
    fn test_object_layout_display() {
        assert_eq!(ObjectLayout::Basic.to_string(), "basic");
        assert_eq!(ObjectLayout::Note.to_string(), "note");
        assert_eq!(ObjectLayout::Bookmark.to_string(), "bookmark");
    }

    #[test]
    fn test_object_layout_from_string() {
        use std::str::FromStr;
        assert_eq!(
            ObjectLayout::from_str("basic").unwrap(),
            ObjectLayout::Basic
        );
        assert_eq!(ObjectLayout::from_str("note").unwrap(), ObjectLayout::Note);
    }

    #[test]
    fn test_create_object_request_body_serialization() {
        let body = CreateObjectRequestBody {
            type_key: "page".to_string(),
            name: Some("Test".to_string()),
            body: None,
            icon: None,
            template_id: None,
            properties: vec![],
        };

        let json = serde_json::to_string(&body).unwrap();
        assert!(json.contains("\"type_key\":\"page\""));
        assert!(json.contains("\"name\":\"Test\""));
        // body should be skipped since it's None
        assert!(!json.contains("\"body\""));
    }

    #[test]
    fn test_update_object_request_body_empty_fields_skipped() {
        let body = UpdateObjectRequestBody::default();
        let json = serde_json::to_string(&body).unwrap();
        // Empty struct should serialize to {}
        assert_eq!(json, "{}");
    }

    #[test]
    fn test_update_object_request_body_with_values() {
        let body = UpdateObjectRequestBody {
            name: Some("Updated".to_string()),
            markdown: Some("# Content".to_string()),
            icon: None,
            type_key: None,
            properties: vec![],
        };

        let json = serde_json::to_string(&body).unwrap();
        assert!(json.contains("\"name\":\"Updated\""));
        assert!(json.contains("\"markdown\":\"# Content\""));
    }
}
