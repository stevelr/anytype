//! # Anytype Properties
//!
//! This module provides a fluent builder API for working with Anytype properties.
//!
//! - [`properties`](AnytypeClient::properties) - list properties in the space
//! - [`property`](AnytypeClient::property) - get property for retrieval or deletion
//! - [`new_property`](AnytypeClient::new_property) - create a new property
//! - [`update_property`](AnytypeClient::update_property) - update a property
//! - [`lookup_property_by_key`](AnytypeClient::lookup_property_by_key) - find property using key
//! - [`lookup_property_tag`](AnytypeClient::lookup_property_tag) - find tag for property, using keys or ids
//!
//! ## Quick Start
//!
//! ```rust,no_run
//! use anytype::prelude::*;
//!
//! # async fn example(client: &AnytypeClient) -> Result<(), AnytypeError> {
//! let space_id = "your_space_id";
//!
//! // List all properties
//! let properties = client.properties(space_id).list().await?;
//!
//! // Get a specific property
//! let prop = client.property(space_id, "property_id").get().await?;
//!
//! // Create a new property
//! let prop = client
//!     .new_property(space_id, "Priority", PropertyFormat::Select)
//!     .key("priority")
//!     .create().await?;
//!
//! // Update a property
//! let prop = client.update_property(space_id, "property_id")
//!     .name("Updated Name")
//!     .update().await?;
//!
//! // Delete a property
//! client.property(space_id, "property_id").delete().await?;
//! # Ok(())
//! # }
//! ```
//!
//! ## Types
//!
//! - [`Property`] - Object Property info
//! - [`PropertyWithValue`] - Property with its value (from objects)
//! - [`PropertyValue`] - Enum representing different property value types
//! - [`PropertyFormat`] - Enum of available property formats
//! - [`SetProperty`] - Trait for setting property values on objects

/*
  # Notes on Property protocols and serialization

  The REST protocol differs in how properties are sent and received.
  When receiving Objects, such as in response to get_object, search,
  list_objects, etc., we receive and deserialize Object struct containing
  an array of PropertyWithValue. However, we don't ever Serialize Object or
  PropertyWithValue. Objects are created with CreateObjectRequest builder,
  and updated with the UpdateObjectRequest builder, and properties are set and
  updated with the SetProperty trait, which creates json dynamically
  based on the property type.

  Object, PropertyWithValue, and PropertyValue are never Serialized
  into json requests to the server, but Serialize is derived for them
  so the cli can generate json output.

  Another protocol quirk is that when PropertyWithValue is received from
  an Anytype server, the select-format property value is a Tag object
  (json map containing {id, name, key, color}), and multi-select value
  is an array of Tag objects. When sending _to_ the server , select is
  a string tag id and multi-select is an array of string tag ids.
*/
use std::sync::Arc;

use chrono::{DateTime, FixedOffset};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Number, Value, json};
use snafu::prelude::*;
use tracing::error;

use crate::resolve::{MAX_RESOLVE_SCAN_ITEMS, RESOLVE_PAGE_SIZE, resolution_limit};
use crate::{
    Result,
    cache::AnytypeCache,
    client::AnytypeClient,
    error::OtherSnafu,
    filters::{Query, QueryWithFilters},
    http_client::{GetPaged, HttpClient},
    prelude::*,
    tags::{CreateTagRequest, ListTagsRequest},
    validation::looks_like_object_id,
    verify::{VerifyConfig, VerifyPolicy, resolve_verify, verify_available},
};

/// Available property formats.
///
/// Determines how a property value is stored and displayed.
#[derive(
    Debug,
    Default,
    Copy,
    Serialize,
    Deserialize,
    Clone,
    Eq,
    PartialEq,
    strum::Display,
    strum::EnumString,
)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum PropertyFormat {
    /// Plain text
    #[default]
    Text,
    /// Numeric value
    Number,
    /// Single selection from Tag options
    Select,
    /// Multiple selections from Tag options
    MultiSelect,
    /// Date/time value
    Date,
    /// File attachments
    Files,
    /// Boolean checkbox
    Checkbox,
    /// URL/web address
    Url,
    /// Email address
    Email,
    /// Phone number
    Phone,
    /// References to other objects
    Objects,
}

/// Property definition
///
/// This represents the schema/definition of a property, not its value.
/// For Select and `MultiSelect` properties, may optionally include Tags, if `with_tags` set when it was fetched,
/// or if it was cached)
#[derive(Debug, Deserialize, Clone, Serialize)]
pub struct Property {
    /// Display name of the property
    pub name: String,

    /// Property key in `snake_case`, e.g., "`last_modified_date`"
    pub key: String,

    /// Unique property identifier
    pub id: String,

    /// Property format (text, number, select, etc.)
    format: PropertyFormat,

    /// optional tags, if property is Select or `MultiSelect`, and tags have been fetched
    tags: Option<Vec<Tag>>,
}

/// Property with its value, as returned in Object.properties.
///
/// Contains both the property definition and its current value.
/// The format is determined by the `PropertyValue` variant.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PropertyWithValue {
    /// Property display name
    pub name: String,

    /// Property key
    pub key: String,

    /// Property identifier
    pub id: String,

    /// The property's value (includes format as the enum tag)
    #[serde(flatten)]
    pub value: PropertyValue,
}

impl PropertyWithValue {
    /// Returns the format of this property's value.
    pub fn format(&self) -> PropertyFormat {
        self.value.format()
    }
}

impl Property {
    /// Constructs a `Property` from `PropertyWithValue`.
    pub fn new_from(other: &PropertyWithValue) -> Self {
        Self {
            format: other.format(),
            id: other.id.clone(),
            key: other.key.clone(),
            name: other.name.clone(),
            tags: None,
        }
    }

    /// Returns the format.
    pub fn format(&self) -> PropertyFormat {
        self.format
    }

    /// Returns tags if they have been fetched, or None if the tags were not retrieved.
    pub fn tags(&self) -> Option<&[Tag]> {
        self.tags.as_deref()
    }

    /// Searches for property tag using id, key, or case-insensitive name match.
    /// Error:
    ///  - `NotFound` if tags are not pre-loaded or there is no match
    pub fn lookup_tag(&self, value: impl AsRef<str>) -> Result<Tag> {
        let check = value.as_ref().to_lowercase();
        self.tags()
            .and_then(|tags| {
                tags.iter()
                    .find(|tag| {
                        tag.id == check || tag.name.to_lowercase() == check || tag.key == check
                    })
                    .cloned()
            })
            .map_or_else(
                || {
                    Err(AnytypeError::NotFound {
                        obj_type: "Tag".into(),
                        key: value.as_ref().to_string(),
                    })
                },
                Ok,
            )
    }

    /// Gets the tag with the id, or None if not found.
    pub fn tag_by_id(&self, tag_id: impl AsRef<str>) -> Option<&Tag> {
        let id = tag_id.as_ref();
        if !looks_like_object_id(id) {
            return None;
        }
        self.tags()
            .and_then(|tags| tags.iter().find(|tag| tag.id == id))
    }

    /// Gets the tag with the key, or None if not found.
    pub fn tag_by_key(&self, tag_key: impl AsRef<str>) -> Option<&Tag> {
        let key = tag_key.as_ref();
        self.tags()
            .and_then(|tags| tags.iter().find(|tag| tag.key == key))
    }

    /// Gets the tag with the name, or None if not found.
    pub fn tag_by_name(&self, tag_name: impl AsRef<str>) -> Option<&Tag> {
        let name = tag_name.as_ref();
        self.tags()
            .and_then(|tags| tags.iter().find(|tag| tag.name == name))
    }
}

/// Property value variants.
///
/// Represents the actual value of a property. The variant type
/// corresponds to the property's format. The `format` field in the JSON
/// acts as the discriminant tag.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "format", rename_all = "snake_case")]
pub enum PropertyValue {
    /// Plain text value
    Text { text: String },
    /// Numeric value
    Number { number: Number },
    /// Single selected option
    Select { select: Tag },
    /// Multiple selected options
    MultiSelect {
        #[serde(default, deserialize_with = "deserialize_vec_tag_or_null")]
        multi_select: Vec<Tag>,
    },
    /// Date/time string
    Date { date: String },
    /// List of file references
    Files {
        #[serde(default, deserialize_with = "deserialize_vec_string_or_null")]
        files: Vec<String>,
    },
    /// Boolean value
    Checkbox { checkbox: bool },
    /// URL string
    Url { url: String },
    /// Email address
    Email { email: String },
    /// Phone number
    Phone { phone: String },
    /// List of object references
    Objects {
        #[serde(default, deserialize_with = "deserialize_vec_string_or_null")]
        objects: Vec<String>,
    },
}

fn deserialize_vec_string_or_null<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Option::<Vec<String>>::deserialize(deserializer)?;
    Ok(value.unwrap_or_default())
}

fn deserialize_vec_tag_or_null<'de, D>(deserializer: D) -> Result<Vec<Tag>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Option::<Vec<Tag>>::deserialize(deserializer)?;
    Ok(value.unwrap_or_default())
}

impl PropertyValue {
    /// Returns the value as a string.
    ///
    /// Works for Text, Date, Url, Email, Phone, and Checkbox formats.
    /// For select properties, returns the tag key
    /// Returns None for array types (Files, `MultiSelect`, Objects).
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::Text { text } => Some(text.as_str()),
            Self::Select { select } => Some(&select.key),
            Self::Date { date } => Some(date.as_str()),
            Self::Url { url } => Some(url.as_str()),
            Self::Email { email } => Some(email.as_str()),
            Self::Phone { phone } => Some(phone.as_str()),
            Self::Checkbox { checkbox } => Some(if *checkbox { "true" } else { "false" }),
            _ => None,
        }
    }

    /// Returns the value as a boolean.
    ///
    /// Property must be Checkbox format
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Checkbox { checkbox } => Some(*checkbox),
            _ => None,
        }
    }

    /// Returns the value as a Number.
    ///
    /// Property must be Number format
    pub fn as_number(&self) -> Option<&Number> {
        match self {
            Self::Number { number } => Some(number),
            _ => None,
        }
    }

    /// Returns the date value as `DateTime` object.
    ///
    /// Returns None if the property is not defined to have format Date, or could not be parsed as a date.
    pub fn as_date(&self) -> Option<DateTime<FixedOffset>> {
        match self {
            Self::Date { date } => match DateTime::parse_from_rfc3339(date) {
                Err(err) => {
                    error!(?err, "Date property has invalid format \"{date}\"");
                    None
                }
                Ok(date) => Some(date),
            },
            _ => None,
        }
    }

    /// Returns the value as an array of strings.
    /// For multi-select (array of tags), returns the tags' keys.
    ///
    /// Property must be Files, `MultiSelect`, or Objects
    pub fn as_array(&self) -> Option<Vec<String>> {
        match self {
            Self::Files { files } => Some(files.clone()),
            Self::MultiSelect { multi_select } => {
                Some(multi_select.iter().map(|tag| tag.key.clone()).collect())
            }
            Self::Objects { objects } => Some(objects.clone()),
            _ => None,
        }
    }

    /// Returns select value as a tag
    pub fn as_tag(&self) -> Option<&Tag> {
        match self {
            Self::Select { select } => Some(select),
            _ => None,
        }
    }

    /// Returns multi-select value as an array of tags
    pub fn as_tags(&self) -> Option<&[Tag]> {
        match self {
            Self::MultiSelect { multi_select } => Some(multi_select),
            _ => None,
        }
    }

    /// Returns the format corresponding to this value variant.
    pub fn format(&self) -> PropertyFormat {
        match self {
            Self::Text { .. } => PropertyFormat::Text,
            Self::Number { .. } => PropertyFormat::Number,
            Self::Select { .. } => PropertyFormat::Select,
            Self::MultiSelect { .. } => PropertyFormat::MultiSelect,
            Self::Date { .. } => PropertyFormat::Date,
            Self::Files { .. } => PropertyFormat::Files,
            Self::Checkbox { .. } => PropertyFormat::Checkbox,
            Self::Url { .. } => PropertyFormat::Url,
            Self::Email { .. } => PropertyFormat::Email,
            Self::Phone { .. } => PropertyFormat::Phone,
            Self::Objects { .. } => PropertyFormat::Objects,
        }
    }
}

fn try_parse_num(key: &str, value: &str) -> Result<serde_json::Number> {
    // first try int
    value.parse::<u64>().map_or_else(
        |_| {
            value.parse::<i64>().map_or_else(
                |_| {
                    value.parse::<f64>().map_or_else(
                        |_| {
                            Err(AnytypeError::Validation {
                                message: format!("Invalid number for property {key}: {value}"),
                            })
                        },
                        |num| Ok(serde_json::Number::from_f64(num).unwrap()),
                    )
                },
                |num| Ok(Number::from(num)),
            )
        },
        |num| Ok(Number::from(num)),
    )
}

fn try_tag(prop: &Property, key: &str, value: &str) -> Result<String> {
    let value = if looks_like_object_id(value) {
        value
    } else if let Some(tag) = prop.tag_by_name(value) {
        &tag.id
    } else if let Some(tag) = prop.tag_by_key(value) {
        &tag.id
    } else {
        return NotFoundSnafu {
            obj_type: "Tag".to_string(),
            key: format!("property {key} tag: {value}"),
        }
        .fail();
    };
    Ok(value.to_string())
}

impl AnytypeClient {
    // Convenience method to set properties on an object (NewObjectRequest or UpdateObjectRequest)
    // using string values. Returns error if the value cannot be converted to the applicable type.
    // When setting select and multi-select values, the value can be an id, name, or key.
    pub async fn set_properties<
        K: AsRef<str> + Sync,
        V: AsRef<str> + Sync,
        SP: SetProperty + Send,
    >(
        &self,
        space_id: &str,
        obj: SP,
        typ: &Type,
        props: &[(K, V)],
    ) -> Result<SP> {
        let mut obj = obj;
        for (key, value) in props {
            let key = key.as_ref();
            let value = value.as_ref();

            if let Some(prop) = typ.get_property_by_key(key) {
                match prop.format() {
                    PropertyFormat::Text => {
                        obj = obj.set_text(key, value);
                    }
                    PropertyFormat::Number => {
                        obj = obj.set_number(key, try_parse_num(key, value)?);
                    }
                    PropertyFormat::Select => {
                        // get property from cache with its tags
                        let prop = self.property(space_id, &prop.id).get().await?;
                        obj = obj.set_select(key, &try_tag(&prop, key, value)?);
                    }
                    PropertyFormat::MultiSelect => {
                        // get property from cache with its tags
                        let prop = self.property(space_id, &prop.id).get().await?;
                        let mut values = Vec::new();
                        for id_or_tag in value.split(',') {
                            values.push(try_tag(&prop, key, id_or_tag)?);
                        }
                        obj = obj.set_multi_select(key, values);
                    }
                    PropertyFormat::Date => {
                        obj = obj.set_date(key, value);
                    }
                    PropertyFormat::Files => {
                        let files = value.split(',').collect::<Vec<&str>>();
                        obj = obj.set_files(key, files);
                    }
                    PropertyFormat::Checkbox => {
                        if let Ok(val) = value.parse::<bool>() {
                            obj = obj.set_checkbox(key, val);
                        } else {
                            return ValidationSnafu {
                                message: format!("Invalid bool value for property {key}: {value}"),
                            }
                            .fail();
                        }
                    }
                    PropertyFormat::Url => {
                        obj = obj.set_url(key, value);
                    }
                    PropertyFormat::Email => {
                        obj = obj.set_email(key, value);
                    }
                    PropertyFormat::Phone => {
                        obj = obj.set_phone(key, value);
                    }
                    PropertyFormat::Objects => {
                        let ids = value.split(',').collect::<Vec<&str>>();
                        obj = obj.set_objects(key, ids);
                    }
                }
            } else {
                return ValidationSnafu {
                    message: format!("invalid property {key} for type {}", typ.key),
                }
                .fail();
            }
        }
        Ok(obj)
    }
}

// ============================================================================
// SetProperty TRAIT
// ============================================================================

/// Trait for setting property values on objects. Used by `CreateObjectRequest` and `UpdateObjectRequest`.
///
/// To set a property on an object, the property must already be defined in the object's type.
///
pub trait SetProperty: Sized {
    /// Adds a raw property value.
    ///
    /// Base method that all typed setters must implement.
    #[must_use]
    fn add_property(self, property: Value) -> Self;

    /// Sets a text property value.
    ///
    /// # Arguments
    /// * `key` - Property key
    /// * `value` - Text value
    #[must_use]
    fn set_text(self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.add_property(json!({
            "key": key.into(),
            "text": value.into(),
        }))
    }

    /// Sets a number property value.
    ///
    /// # Arguments
    /// * `key` - Property key
    /// * `value` - Numeric value
    #[must_use]
    fn set_number(self, key: impl Into<String>, value: impl Into<Number>) -> Self {
        self.add_property(json!({
            "key": key.into(),
            "number": value.into(),
        }))
    }

    /// Sets a date property value.
    ///
    /// # Arguments
    /// * `key` - Property key
    /// * `value` - Date string (ISO 3339 format recommended)
    #[must_use]
    fn set_date(self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.add_property(json!({
            "key": key.into(),
            "date": value.into(),
        }))
    }

    /// Sets a URL property value.
    ///
    /// # Arguments
    /// * `key` - Property key
    /// * `value` - URL string
    #[must_use]
    fn set_url(self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.add_property(json!({
            "key": key.into(),
            "url": value.into(),
        }))
    }

    /// Sets an email property value.
    ///
    /// # Arguments
    /// * `key` - Property key
    /// * `value` - Email address
    #[must_use]
    fn set_email(self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.add_property(json!({
            "key": key.into(),
            "email": value.into(),
        }))
    }

    /// Sets a phone property value.
    ///
    /// # Arguments
    /// * `key` - Property key
    /// * `value` - Phone number
    #[must_use]
    fn set_phone(self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.add_property(json!({
            "key": key.into(),
            "phone": value.into(),
        }))
    }

    /// Sets a checkbox (boolean) property value.
    ///
    /// # Arguments
    /// * `key` - Property key
    /// * `value` - Boolean value
    #[must_use]
    fn set_checkbox(self, key: impl Into<String>, value: bool) -> Self {
        self.add_property(json!({
            "key": key.into(),
            "checkbox": value,
        }))
    }

    /// Sets a select property to the tag id.
    ///
    /// # Arguments
    /// * `key` - Property key
    /// * `tag_id` - id of tag
    #[must_use]
    fn set_select(self, key: impl Into<String>, tag_id: impl Into<String>) -> Self {
        let key = key.into();
        let tag_id = tag_id.into();
        if !looks_like_object_id(&tag_id) {
            error!("set_select({key},...): invalid tag id: {tag_id}");
        }
        self.add_property(json!({
            "key": key,
            "select": tag_id,
        }))
    }

    /// Sets an objects (relation) property value.
    ///
    /// # Arguments
    /// * `key` - Property key
    /// * `objects` - Iterator of object IDs
    #[must_use]
    fn set_objects(
        self,
        key: impl Into<String>,
        objects: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        self.add_property(json!({
            "key": key.into(),
            "objects": objects.into_iter().map(Into::into).collect::<Vec<String>>(),
        }))
    }

    /// Sets a files property value.
    ///
    /// # Arguments
    /// * `key` - Property key
    /// * `files` - Iterator of file references
    #[must_use]
    fn set_files(
        self,
        key: impl Into<String>,
        files: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        self.add_property(json!({
            "key": key.into(),
            "files": files.into_iter().map(Into::into).collect::<Vec<String>>(),
        }))
    }

    /// Sets a multi-select property value. (multiple tag ids)
    ///
    /// # Arguments
    /// * `key` - Property key
    /// * `values` - Iterator of tag ids
    #[must_use]
    fn set_multi_select(
        self,
        key: impl Into<String>,
        values: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        let key = key.into();
        let values = values.into_iter().map(Into::into).collect::<Vec<String>>();
        for value in &values {
            if !looks_like_object_id(value) {
                error!("set_multi_select({key}, ...) invalid tag id: {value}");
            }
        }
        self.add_property(json!({
            "key": key,
            "multi_select": values
        }))
    }
}

/// Response wrapper for single property operations
#[derive(Debug, Deserialize)]
struct PropertyResponse {
    property: Property,
}

/// Internal request body for creating a property
#[derive(Debug, Serialize)]
struct CreatePropertyRequestBody {
    // name (required)
    name: String,

    // format (required)
    format: PropertyFormat,

    #[serde(skip_serializing_if = "Option::is_none")]
    key: Option<String>,

    #[serde(skip_serializing_if = "Vec::is_empty")]
    tags: Vec<CreateTagRequest>,
}

/// Internal request body for updating a property
#[derive(Debug, Serialize)]
struct UpdatePropertyRequestBody {
    name: String,

    #[serde(skip_serializing_if = "Option::is_none")]
    key: Option<String>,
}

/// Request builder for getting or deleting a single property.
///
/// Obtained via [`AnytypeClient::property`].
///
/// # Example
///
/// ```rust,no_run
/// # use anytype::prelude::*;
/// # async fn example(client: &AnytypeClient) -> Result<(), AnytypeError> {
/// // Get a property
/// let prop = client.property("space_id", "property_id").get().await?;
///
/// // Delete a property
/// let deleted = client.property("space_id", "property_id").delete().await?;
/// # Ok(())
/// # }
/// ```
#[derive(Debug)]
pub struct PropertyRequest {
    client: Arc<HttpClient>,
    limits: ValidationLimits,
    space_id: String,
    property_id: String,
    with_tags: bool,
    cache: Arc<AnytypeCache>,
}

pub(crate) async fn set_property_tags(
    client: &Arc<HttpClient>,
    limits: &ValidationLimits,
    space_id: &str,
    property: &mut Property,
) -> Result<(), AnytypeError> {
    if property.format == PropertyFormat::Select || property.format == PropertyFormat::MultiSelect {
        let tags = ListTagsRequest::new(client.clone(), limits.clone(), space_id, &property.id)
            .list()
            .await?
            .collect_all()
            .await?;
        property.tags = Some(tags);
    }
    Ok(())
}

/// Load all space properties into cache.
/// Always fetches tags for Select and `MultiSelect` properties
async fn prime_cache_properties(
    client: &Arc<HttpClient>,
    cache: &Arc<AnytypeCache>,
    limits: &ValidationLimits,
    space_id: &str,
) -> Result<()> {
    let mut properties: Vec<Property> = client
        .get_request_paged(
            &format!("/v1/spaces/{space_id}/properties"),
            QueryWithFilters::default(),
        )
        .await?
        .collect_all()
        .await?;

    for prop in &mut properties {
        // if property is select or multi-select, update tags
        set_property_tags(client, limits, space_id, prop).await?;
    }
    cache.set_properties(space_id, properties);
    Ok(())
}

impl PropertyRequest {
    /// Creates a new `PropertyRequest`.
    pub(crate) fn new(
        client: Arc<HttpClient>,
        limits: ValidationLimits,
        space_id: impl Into<String>,
        property_id: impl Into<String>,
        with_tags: bool,
        cache: Arc<AnytypeCache>,
    ) -> Self {
        Self {
            client,
            limits,
            space_id: space_id.into(),
            property_id: property_id.into(),
            with_tags,
            cache,
        }
    }

    /// Also fetches tags when this request is executed with [`get`](Self::get).
    ///
    /// [`get_direct`](Self::get_direct) is intentionally metadata-only and
    /// never expands tags, even when this option was selected.
    #[must_use]
    pub fn with_tags(mut self) -> Self {
        self.with_tags = true;
        self
    }

    /// Retrieves the property by ID.
    ///
    /// # Returns
    /// The property definition.
    /// If property has format select or multi-select, call `with_tags()` to also fetch the tag options for the property.
    ///
    /// # Errors
    /// - [`AnytypeError::NotFound`] if the property doesn't exist
    /// - [`AnytypeError::Validation`] if IDs are invalid
    pub async fn get(self) -> Result<Property> {
        self.limits.validate_id(&self.space_id, "space_id")?;
        self.limits.validate_id(&self.property_id, "property_id")?;

        if self.cache.is_enabled() {
            if let Some(property) = self.cache.get_property(&self.space_id, &self.property_id) {
                return Ok((*property).clone());
            }
            // see note on locking design in cache.rs
            if !self.cache.has_properties(&self.space_id) {
                prime_cache_properties(&self.client, &self.cache, &self.limits, &self.space_id)
                    .await?;
                if let Some(property) = self.cache.get_property(&self.space_id, &self.property_id) {
                    let mut property = (*property).clone();
                    if !self.with_tags {
                        property.tags = None;
                    }
                    return Ok(property);
                }
            }
            return NotFoundSnafu {
                obj_type: "Property".to_string(),
                key: self.property_id,
            }
            .fail();
        }

        // cache disabled, fetch directly
        let mut property = self.fetch_direct_metadata().await?;
        if self.with_tags {
            set_property_tags(&self.client, &self.limits, &self.space_id, &mut property).await?;
        }
        Ok(property)
    }

    /// Retrieves the property with one cache-independent scoped HTTP GET.
    ///
    /// Unlike [`get`](Self::get), this method neither reads nor primes the
    /// in-memory property cache. It validates the space and property IDs
    /// before dispatch and rejects a successful response whose property ID
    /// differs from the requested ID. This method is always metadata-only:
    /// [`with_tags`](Self::with_tags) affects [`get`](Self::get), but is
    /// intentionally ignored here so a direct read is exactly one request and
    /// can never expand into an unbounded tag scan. Any tags included
    /// unexpectedly in the direct response are discarded.
    ///
    /// # Returns
    /// The property returned for the exact scoped property endpoint.
    ///
    /// # Errors
    /// - [`AnytypeError::NotFound`] if the property doesn't exist
    /// - [`AnytypeError::Validation`] if either ID is invalid
    /// - [`AnytypeError::Other`] if the upstream response identity does not
    ///   match the scoped request
    pub async fn get_direct(self) -> Result<Property> {
        self.limits.validate_id(&self.space_id, "space_id")?;
        self.limits.validate_id(&self.property_id, "property_id")?;
        let mut property = self.fetch_direct_metadata().await?;
        property.tags = None;
        Ok(property)
    }

    async fn fetch_direct_metadata(&self) -> Result<Property> {
        let response: PropertyResponse = self
            .client
            .get_request(
                &format!(
                    "/v1/spaces/{}/properties/{}",
                    self.space_id, self.property_id
                ),
                QueryWithFilters::default(),
            )
            .await?;

        let property = response.property;
        if property.id != self.property_id {
            return OtherSnafu {
                message: "Anytype returned a mismatched property identity".to_string(),
            }
            .fail();
        }

        Ok(property)
    }

    /// Deletes (archives) the property.
    ///
    /// # Returns
    /// The deleted property.
    ///
    /// # Errors
    /// - [`AnytypeError::NotFound`] if the property doesn't exist
    /// - [`AnytypeError::Forbidden`] if you don't have permission
    pub async fn delete(self) -> Result<Property> {
        self.limits.validate_id(&self.space_id, "space_id")?;
        self.limits.validate_id(&self.property_id, "property_id")?;

        let response: PropertyResponse = self
            .client
            .delete_request(&format!(
                "/v1/spaces/{}/properties/{}",
                self.space_id, self.property_id
            ))
            .await?;
        self.cache
            .delete_property(&self.space_id, &self.property_id);
        Ok(response.property)
    }
}

/// Request builder for creating a new property.
///
/// Obtained via [`AnytypeClient::new_property`].
///
/// # Example
///
/// ```rust,no_run
/// # use anytype::prelude::*;
/// # async fn example(client: &AnytypeClient) -> Result<(), AnytypeError> {
/// let prop = client
///     .new_property("space_id", "Priority", PropertyFormat::Select)
///     .key("priority")
///     .create().await?;
/// # Ok(())
/// # }
/// ```
#[derive(Debug)]
pub struct NewPropertyRequest {
    client: Arc<HttpClient>,
    limits: ValidationLimits,
    space_id: String,
    name: String,
    format: PropertyFormat,
    key: Option<String>,
    tags: Vec<CreateTagRequest>,
    cache: Arc<AnytypeCache>,
    verify_policy: VerifyPolicy,
    verify_config: Option<VerifyConfig>,
}

impl NewPropertyRequest {
    /// Creates a new `NewPropertyRequest`.
    pub(crate) fn new(
        client: Arc<HttpClient>,
        limits: ValidationLimits,
        space_id: impl Into<String>,
        name: impl Into<String>,
        format: PropertyFormat,
        cache: Arc<AnytypeCache>,
        verify_config: Option<VerifyConfig>,
    ) -> Self {
        Self {
            client,
            limits,
            space_id: space_id.into(),
            name: name.into(),
            format,
            key: None,
            tags: Vec::new(),
            cache,
            verify_policy: VerifyPolicy::Default,
            verify_config,
        }
    }

    /// Sets the property key.
    ///
    /// Should be in `snake_case` format.
    ///
    /// # Arguments
    /// * `key` - Unique key for the property
    #[must_use]
    pub fn key(mut self, key: impl Into<String>) -> Self {
        self.key = Some(key.into());
        self
    }

    /// Adds a tag for select/multi-select properties.
    ///
    /// # Arguments
    /// * `name` - tag name
    /// * `key` - optional key
    /// * `color` - tag color
    #[must_use]
    pub fn tag(mut self, name: &str, key: Option<String>, color: Color) -> Self {
        self.tags.push(CreateTagRequest {
            name: name.into(),
            key,
            color,
        });
        self
    }

    /// Adds multiple tags for select/multi-select properties.
    ///
    /// # Arguments
    /// * `tags` - Iterator of tags to add
    #[must_use]
    pub fn tags(mut self, tags: impl IntoIterator<Item = CreateTagRequest>) -> Self {
        self.tags.extend(tags);
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

    /// Creates the property with the configured settings.
    ///
    /// # Returns
    /// The newly created property.
    ///
    /// # Errors
    /// - [`AnytypeError::Validation`] if name is not provided or invalid
    pub async fn create(self) -> Result<Property> {
        self.limits.validate_id(&self.space_id, "space_id")?;
        self.limits.validate_name(&self.name, "property")?;
        let create_with_tags = !self.tags.is_empty();
        if let Some(ref key) = self.key {
            self.limits.validate_name(key, "property key")?;
        }
        ensure!(
            self.tags.is_empty()
                || self.format == PropertyFormat::Select
                || self.format == PropertyFormat::MultiSelect,
            ValidationSnafu {
                message: format!(
                    "Property {} format {} cannot be created with tags, because tags are only supported for formats Select and MultiSelect",
                    self.name, self.format
                ),
            }
        );

        let request_body = CreatePropertyRequestBody {
            name: self.name,
            key: self.key,
            format: self.format,
            tags: self.tags,
        };

        let response: PropertyResponse = self
            .client
            .post_request(
                &format!("/v1/spaces/{}/properties", self.space_id),
                &request_body,
                QueryWithFilters::default(),
            )
            .await?;

        // replace cached property, including tags
        if self.cache.has_properties(&self.space_id) {
            let mut property = response.property.clone();
            if create_with_tags {
                set_property_tags(&self.client, &self.limits, &self.space_id, &mut property)
                    .await?;
            }
            self.cache.set_property(&self.space_id, property);
        }

        let property = response.property;
        if let Some(config) = resolve_verify(self.verify_policy, self.verify_config.as_ref()) {
            return verify_available(&config, "Property", &property.id, || async {
                let response: PropertyResponse = self
                    .client
                    .get_request(
                        &format!("/v1/spaces/{}/properties/{}", self.space_id, property.id),
                        QueryWithFilters::default(),
                    )
                    .await?;
                Ok(response.property)
            })
            .await;
        }
        Ok(property)
    }
}

/// Request builder for updating an existing property.
///
/// Obtained via [`AnytypeClient::update_property`].
///
/// # Example
///
/// ```rust,no_run
/// # use anytype::prelude::*;
/// # async fn example(client: &AnytypeClient) -> Result<(), AnytypeError> {
/// let prop = client.update_property("space_id", "property_id")
///     .name("Updated Priority")
///     .key("updated_priority")
///     .update().await?;
/// # Ok(())
/// # }
/// ```
#[derive(Debug)]
pub struct UpdatePropertyRequest {
    client: Arc<HttpClient>,
    limits: ValidationLimits,
    space_id: String,
    property_id: String,
    name: Option<String>,
    key: Option<String>,
    cache: Arc<AnytypeCache>,
    verify_policy: VerifyPolicy,
    verify_config: Option<VerifyConfig>,
}

impl UpdatePropertyRequest {
    /// Creates a new `UpdatePropertyRequest`.
    pub(crate) fn new(
        client: Arc<HttpClient>,
        limits: ValidationLimits,
        space_id: impl Into<String>,
        property_id: impl Into<String>,
        cache: Arc<AnytypeCache>,
        verify_config: Option<VerifyConfig>,
    ) -> Self {
        Self {
            client,
            limits,
            space_id: space_id.into(),
            property_id: property_id.into(),
            name: None,
            key: None,
            cache,
            verify_policy: VerifyPolicy::Default,
            verify_config,
        }
    }

    /// Sets the property name required by the REST update endpoint.
    ///
    /// This must be supplied even when only changing the property key. Use the
    /// property's current name to leave it unchanged.
    ///
    /// # Arguments
    /// * `name` - New display name for the property
    #[must_use]
    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Updates the property key.
    ///
    /// # Arguments
    /// * `key` - New key for the property (`snake_case`)
    #[must_use]
    pub fn key(mut self, key: impl Into<String>) -> Self {
        self.key = Some(key.into());
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

    /// Applies the update to the property.
    ///
    /// Note: Property format cannot be changed after creation.
    ///
    /// # Returns
    /// The updated property.
    ///
    /// # Errors
    /// - [`AnytypeError::Validation`] if called without setting a name
    /// - [`AnytypeError::NotFound`] if the property doesn't exist
    pub async fn update(self) -> Result<Property> {
        self.limits.validate_id(&self.space_id, "space_id")?;
        self.limits.validate_id(&self.property_id, "property_id")?;

        // anytype-heart's REST binding requires name even when only the key changes.
        ensure!(
            self.name.is_some(),
            ValidationSnafu {
                message:
                    "update_property: name is required by the REST API (including for key changes)"
                        .to_string(),
            }
        );

        let name = self.name.expect("property name checked above");
        self.limits.validate_name(&name, "property name")?;
        if let Some(ref key) = self.key {
            self.limits.validate_name(key, "property key")?;
        }

        let request_body = UpdatePropertyRequestBody {
            name,
            key: self.key,
        };

        let response: PropertyResponse = self
            .client
            .patch_request(
                &format!(
                    "/v1/spaces/{}/properties/{}",
                    self.space_id, self.property_id
                ),
                &request_body,
            )
            .await?;

        // update property in cache
        if self.cache.has_properties(&self.space_id) {
            let mut property = response.property.clone();
            set_property_tags(&self.client, &self.limits, &self.space_id, &mut property).await?;
            self.cache.set_property(&self.space_id, property);
        }

        let property = response.property;
        if let Some(config) = resolve_verify(self.verify_policy, self.verify_config.as_ref()) {
            return verify_available(&config, "Property", &property.id, || async {
                let response: PropertyResponse = self
                    .client
                    .get_request(
                        &format!("/v1/spaces/{}/properties/{}", self.space_id, property.id),
                        QueryWithFilters::default(),
                    )
                    .await?;
                Ok(response.property)
            })
            .await;
        }
        Ok(property)
    }
}

/// Request builder for listing properties in a space.
///
/// Obtained via [`AnytypeClient::properties`].
///
/// # Example
///
/// ```rust
/// # use anytype::prelude::*;
/// # async fn example() -> Result<(), AnytypeError> {
/// #   let client = AnytypeClient::new("doc test")?;
/// #   let space_id = anytype::test_util::example_space_id(&client).await?;
/// let properties = client.properties(&space_id)
///     .limit(50)
///     .list().await?;
///
/// for prop in properties.iter() {
///     println!("{}: {} ({})", prop.key, prop.name, prop.format());
/// }
/// # Ok(())
/// # }
/// ```
#[derive(Debug)]
pub struct ListPropertiesRequest {
    client: Arc<HttpClient>,
    limits: ValidationLimits,
    space_id: String,
    limit: Option<u32>,
    offset: Option<u32>,
    filters: Vec<Filter>,
    cache: Arc<AnytypeCache>,
}

impl ListPropertiesRequest {
    /// Creates a new `ListPropertiesRequest`.
    #[must_use]
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
    /// A paginated result containing the matching properties.
    ///
    /// To take advantage of cached properties for the `list()` method,
    /// the cache must be enabled, and the query
    /// parameter must not contain filters or pagination limits or offsets.
    ///
    /// # Errors
    /// - [`AnytypeError::Validation`] if `space_id` is invalid
    pub async fn list(self) -> Result<PagedResult<Property>> {
        self.limits.validate_id(&self.space_id, "space_id")?;

        if self.cache.is_enabled()
            && self.limit.is_none()
            && (self.offset.unwrap_or_default() == 0)
            && self.filters.is_empty()
        {
            // see note on locking design in cache.rs
            if !self.cache.has_properties(&self.space_id) {
                prime_cache_properties(&self.client, &self.cache, &self.limits, &self.space_id)
                    .await?;
            }
            return Ok(PagedResult::from_items(
                self.cache
                    .properties_for_space(&self.space_id)
                    .unwrap_or_default(),
            ));
        }
        let query = Query::default()
            .set_limit_opt(self.limit)
            .set_offset_opt(self.offset)
            .add_filters(&self.filters);

        self.client
            .get_request_paged(&format!("/v1/spaces/{}/properties", self.space_id), query)
            .await
    }
}

// ============================================================================
// ANYTYPECLIENT METHODS
// ============================================================================

impl AnytypeClient {
    /// Creates a request builder for getting or deleting a single property by its id.
    /// To look up a property by its key,
    /// use [`lookup_property_by_key`](AnytypeClient::lookup_property_by_key)
    ///
    /// # Arguments
    /// * `space_id` - ID of the space containing the property
    /// * `property_id` - ID of the property
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use anytype::prelude::*;
    /// # async fn example(client: &AnytypeClient) -> Result<(), AnytypeError> {
    /// let prop = client.property("space_id", "property_id").get().await?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn property(
        &self,
        space_id: impl Into<String>,
        property_id: impl Into<String>,
    ) -> PropertyRequest {
        PropertyRequest::new(
            self.client.clone(),
            self.config.limits.clone(),
            space_id,
            property_id,
            false,
            self.cache.clone(),
        )
    }

    /// Creates a request builder for creating a new property.
    ///
    /// # Arguments
    /// * `space_id` - ID of the space to create the property in
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use anytype::prelude::*;
    /// # async fn example(client: &AnytypeClient) -> Result<(), AnytypeError> {
    /// let prop = client.new_property("space_id", "Priority", PropertyFormat::Number)
    ///     .create().await?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn new_property(
        &self,
        space_id: impl Into<String>,
        name: impl Into<String>,
        format: PropertyFormat,
    ) -> NewPropertyRequest {
        NewPropertyRequest::new(
            self.client.clone(),
            self.config.limits.clone(),
            space_id,
            name,
            format,
            self.cache.clone(),
            self.config.verify.clone(),
        )
    }

    /// Creates a request builder for updating an existing property.
    ///
    /// The builder must be given [`UpdatePropertyRequest::name`] before
    /// [`UpdatePropertyRequest::update`] is called, including for key-only
    /// changes, because the REST endpoint requires the property's name.
    ///
    /// # Arguments
    /// * `space_id` - ID of the space containing the property
    /// * `property_id` - ID of the property to update
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use anytype::prelude::*;
    /// # async fn example(client: &AnytypeClient) -> Result<(), AnytypeError> {
    /// let prop = client.update_property("space_id", "property_id")
    ///     .name("New Name")
    ///     .update().await?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn update_property(
        &self,
        space_id: impl Into<String>,
        property_id: impl Into<String>,
    ) -> UpdatePropertyRequest {
        UpdatePropertyRequest::new(
            self.client.clone(),
            self.config.limits.clone(),
            space_id,
            property_id,
            self.cache.clone(),
            self.config.verify.clone(),
        )
    }

    /// Creates a request builder for listing properties in a space.
    ///
    /// # Arguments
    /// * `space_id` - ID of the space to list properties from
    ///
    /// # Example
    ///
    /// ```rust
    /// # use anytype::prelude::*;
    /// # async fn example() -> Result<(), AnytypeError> {
    /// #   let client = AnytypeClient::new("doc test")?;
    /// #   let space_id = anytype::test_util::example_space_id(&client).await?;
    /// let properties = client.properties(&space_id)
    ///     .limit(50)
    ///     .list().await?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn properties(&self, space_id: impl Into<String>) -> ListPropertiesRequest {
        ListPropertiesRequest::new(
            self.client.clone(),
            self.config.limits.clone(),
            space_id,
            self.cache.clone(),
        )
    }

    /// Searches for properties in space by id, key, or name, using case-insensitive match.
    /// If the property is type select or multi-select, the property includes the tags.
    ///
    /// This method requires cache to be enabled (the default).
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use anytype::prelude::*;
    /// # async fn example(client: &AnytypeClient) -> Result<(), AnytypeError> {
    /// let props = client.lookup_properties("space_id", "status").await?;
    /// for prop in props {
    ///     println!("Property {} format {}", &prop.name, &prop.format());
    ///     // display tags, for Select and MultiSelect properties
    ///     if let Some(tags) = prop.tags() {
    ///         println!("Values:");
    ///         for tag in tags {
    ///             println!("    {} {}", &tag.key, &tag.name);
    ///         }
    ///     }
    /// }
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// Errors:
    /// - `AnytypeError::NotFound` if no property in the space matched
    /// - `AnytypeError::CacheDisabled` if cache is disabled
    /// - `AnytypeError::*` any other error
    ///
    pub async fn lookup_properties(
        &self,
        space_id: &str,
        text: impl AsRef<str>,
    ) -> Result<Vec<Property>> {
        ensure!(self.cache.is_enabled(), CacheDisabledSnafu);
        // see note on locking design in cache.rs
        if !self.cache.has_properties(space_id) {
            prime_cache_properties(&self.client, &self.cache, &self.config.limits, space_id)
                .await?;
        }
        match self.cache.lookup_property(space_id, text.as_ref()) {
            Some(properties) if !properties.is_empty() => {
                Ok(properties.into_iter().map(|arc| (*arc).clone()).collect())
            }
            _ => Err(AnytypeError::NotFound {
                obj_type: "Property".into(),
                key: text.as_ref().to_string(),
            }),
        }
    }

    /// Searches for properties in space by key using case-insensitive match.
    /// If a property is type select or multi-select, the tags are included.
    ///
    /// This method requires cache to be enabled (the default).
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use anytype::prelude::*;
    /// # async fn example(client: &AnytypeClient) -> Result<(), AnytypeError> {
    /// let prop = client.lookup_property_by_key("space_id", "status").await?;
    /// println!("Property {} format {}", &prop.name, &prop.format());
    /// // display tags, for Select and MultiSelect properties
    /// if let Some(tags) = prop.tags() {
    ///   println!("Values:");
    ///   for tag in tags {
    ///     println!("    {} {}", &tag.key, &tag.name);
    ///   }
    /// }
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// Errors:
    /// - `AnytypeError::NotFound` if no property in the space matched
    /// - `AnytypeError::CacheDisabled` if cache is disabled
    /// - `AnytypeError::*` any other error
    ///
    pub async fn lookup_property_by_key(
        &self,
        space_id: &str,
        text: impl AsRef<str>,
    ) -> Result<Property> {
        ensure!(self.cache.is_enabled(), CacheDisabledSnafu);
        // see note on locking design in cache.rs
        if !self.cache.has_properties(space_id) {
            prime_cache_properties(&self.client, &self.cache, &self.config.limits, space_id)
                .await?;
        }
        self.cache
            .lookup_property_by_key(space_id, text.as_ref())
            .map_or_else(
                || {
                    Err(AnytypeError::NotFound {
                        obj_type: "Property".into(),
                        key: text.as_ref().to_string(),
                    })
                },
                |property| Ok((*property).clone()),
            )
    }

    /// Searches for property and tag combination.
    /// `property_key` can be a property key or id
    /// `tag_name` can be tag id, name, or key
    ///
    /// An explicit property ID is fetched with one metadata-only,
    /// cache-independent scoped GET before tag options are scanned in explicit
    /// 99-row pages. The scan examines at most
    /// [`MAX_RESOLVE_SCAN_ITEMS`] options and returns
    /// [`AnytypeError::ResolutionLimitExceeded`] rather than guessing when
    /// completeness exceeds that bound. A property key retains the cached key
    /// lookup behavior and therefore requires cache to be enabled (the
    /// default).
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use anytype::prelude::*;
    /// # async fn example(client: &AnytypeClient) -> Result<(), AnytypeError> {
    /// let in_progress = client
    ///     .lookup_property_tag("space_id", "status", "In Progress")
    ///     .await?;
    /// println!("Tag:'{}' id:'{}'", &in_progress.name, &in_progress.id);
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// Errors:
    /// - `AnytypeError::NotFound` if no property in the space matched, or tag doesn't match
    /// - `AnytypeError::CacheDisabled` if cache is disabled and a property key
    ///   was supplied
    /// - `AnytypeError::*` any other error
    ///
    pub async fn lookup_property_tag(
        &self,
        space_id: &str,
        property_key: impl AsRef<str>,
        tag_name: impl AsRef<str>,
    ) -> Result<Tag> {
        let prop_key_or_id = property_key.as_ref();
        let tag_key_or_id = tag_name.as_ref();
        if looks_like_object_id(prop_key_or_id) {
            let property = self.property(space_id, prop_key_or_id).get_direct().await?;
            if !matches!(
                property.format(),
                PropertyFormat::Select | PropertyFormat::MultiSelect
            ) {
                return NotFoundSnafu {
                    obj_type: "Tag".to_owned(),
                    key: tag_key_or_id.to_owned(),
                }
                .fail();
            }
            return self
                .lookup_property_tag_bounded(space_id, prop_key_or_id, tag_key_or_id)
                .await;
        }

        self.lookup_property_by_key(space_id, prop_key_or_id)
            .await?
            .lookup_tag(tag_key_or_id)
    }

    async fn lookup_property_tag_bounded(
        &self,
        space_id: &str,
        property_id: &str,
        tag_key_or_id: &str,
    ) -> Result<Tag> {
        const MAX_PAGES: usize = MAX_RESOLVE_SCAN_ITEMS.div_ceil(RESOLVE_PAGE_SIZE as usize);

        let needle = tag_key_or_id.to_lowercase();
        let mut offset = 0_u32;
        let mut scanned = 0_usize;
        for _ in 0..MAX_PAGES {
            let remaining = MAX_RESOLVE_SCAN_ITEMS.saturating_sub(scanned);
            if remaining == 0 {
                return Err(resolution_limit("tag", tag_key_or_id));
            }
            let requested_limit = RESOLVE_PAGE_SIZE.min(remaining as u32);
            let page = self
                .tags(space_id, property_id)
                .limit(requested_limit)
                .offset(offset)
                .list()
                .await?
                .into_response();
            if page.pagination.offset != offset
                || page.pagination.limit != requested_limit
                || page.items.len() > remaining
                || page.items.len() > requested_limit as usize
            {
                return OtherSnafu {
                    message: "Anytype returned malformed tag pagination".to_owned(),
                }
                .fail();
            }

            for tag in &page.items {
                if tag.id == needle || tag.key == needle || tag.name.to_lowercase() == needle {
                    return Ok(tag.clone());
                }
            }
            scanned += page.items.len();

            if page.pagination.total > MAX_RESOLVE_SCAN_ITEMS
                || (scanned == MAX_RESOLVE_SCAN_ITEMS && page.pagination.has_more)
            {
                return Err(resolution_limit("tag", tag_key_or_id));
            }
            if !page.pagination.has_more {
                return NotFoundSnafu {
                    obj_type: "Tag".to_owned(),
                    key: tag_key_or_id.to_owned(),
                }
                .fail();
            }
            offset = offset
                .checked_add(requested_limit)
                .ok_or_else(|| resolution_limit("tag", tag_key_or_id))?;
        }
        Err(resolution_limit("tag", tag_key_or_id))
    }
}

// ============================================================================
// TESTS
// ============================================================================

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    const TEST_SPACE_ID: &str =
        "bafyreid5fvqlnsobih2keakcxjrrlpmly6kf37klzjzen4ibfdgalcdp4y.2tq5w93cr6oe7";
    const TEST_PROPERTY_ID: &str = "bafyreid5fvqlnsobih2keakcxjrrlpmly6kf37klzjzen4ibfdgalcdp4y";
    const OTHER_PROPERTY_ID: &str = "bafyreid5fvqlnsobih2keakcxjrrlpmly6kf37klzjzen4ibfdgalcdp4z";
    const TEST_TAG_ID: &str = "bafyreid5fvqlnsobih2keakcxjrrlpmly6kf37klzjzen4ibfdgalcdp4x";

    #[derive(Debug, Default)]
    struct PropertyRouteTraffic {
        requests: Vec<String>,
        property_list_pages: usize,
        direct_property_gets: usize,
        tag_list_pages: usize,
    }

    #[derive(Clone, Copy)]
    enum TagRoute {
        Single,
        TargetSecondPage,
        BeyondLimit,
    }

    async fn route_aware_property_server(
        returned_property_id: &'static str,
        tag_route: TagRoute,
    ) -> (
        String,
        tokio::sync::oneshot::Sender<()>,
        tokio::task::JoinHandle<PropertyRouteTraffic>,
    ) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind route-aware property fixture");
        let address = listener.local_addr().expect("property fixture address");
        let (shutdown_tx, mut shutdown_rx) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            let mut traffic = PropertyRouteTraffic::default();
            loop {
                let accepted = tokio::select! {
                    _ = &mut shutdown_rx => break,
                    accepted = listener.accept() => accepted,
                };
                let (mut stream, _) = accepted.expect("accept property fixture request");
                let mut request = Vec::new();
                let mut buffer = [0_u8; 1024];
                loop {
                    let read = stream
                        .read(&mut buffer)
                        .await
                        .expect("read property fixture request");
                    assert!(
                        read > 0,
                        "property fixture connection closed before headers"
                    );
                    request.extend_from_slice(&buffer[..read]);
                    assert!(
                        request.len() <= 64 * 1024,
                        "property fixture headers too large"
                    );
                    if request.windows(4).any(|window| window == b"\r\n\r\n") {
                        break;
                    }
                }
                let request = String::from_utf8(request).expect("property request is utf-8");
                let request_line = request.lines().next().expect("property request line");
                let mut parts = request_line.split_ascii_whitespace();
                assert_eq!(parts.next(), Some("GET"));
                let target = parts.next().expect("property request target");
                assert_eq!(parts.next(), Some("HTTP/1.1"));
                assert_eq!(parts.next(), None, "extra property request-line field");
                let (path, raw_query) = target
                    .split_once('?')
                    .map_or((target, ""), |(path, query)| (path, query));
                let mut query = BTreeMap::new();
                for (key, value) in url::form_urlencoded::parse(raw_query.as_bytes()) {
                    let previous = query.insert(key.into_owned(), value.into_owned());
                    assert!(previous.is_none(), "duplicate property query key");
                }
                let collection_path = format!("/v1/spaces/{TEST_SPACE_ID}/properties");
                let direct_path = format!("{collection_path}/{TEST_PROPERTY_ID}");
                let tags_path = format!("{direct_path}/tags");
                let body = if path == collection_path {
                    let page = traffic.property_list_pages;
                    traffic.property_list_pages += 1;
                    if page == 0 {
                        assert_eq!(query, BTreeMap::new(), "first property-list query");
                        serde_json::json!({
                            "data": [{
                                "id": OTHER_PROPERTY_ID,
                                "key": "other",
                                "name": "Other",
                                "format": "text",
                                "tags": null
                            }],
                            "pagination": {
                                "has_more": true,
                                "limit": 100,
                                "offset": 0,
                                "total": 101
                            }
                        })
                        .to_string()
                    } else {
                        assert_eq!(
                            query,
                            BTreeMap::from([
                                ("limit".to_owned(), "100".to_owned()),
                                ("offset".to_owned(), "100".to_owned()),
                            ]),
                            "continued property-list query"
                        );
                        serde_json::json!({
                            "data": [{
                                "id": TEST_PROPERTY_ID,
                                "key": "status",
                                "name": "Status",
                                "format": "select",
                                "tags": null
                            }],
                            "pagination": {
                                "has_more": false,
                                "limit": 100,
                                "offset": 100,
                                "total": 101
                            }
                        })
                        .to_string()
                    }
                } else if path == direct_path {
                    assert_eq!(query, BTreeMap::new(), "direct property query");
                    traffic.direct_property_gets += 1;
                    serde_json::json!({
                        "property": {
                            "id": returned_property_id,
                            "key": "status",
                            "name": "private-property-body-marker",
                            "format": "select",
                            "tags": [{
                                "id": OTHER_PROPERTY_ID,
                                "key": "embedded-private-tag",
                                "name": "embedded private tag",
                                "color": "red"
                            }]
                        }
                    })
                    .to_string()
                } else if path == tags_path {
                    let page = traffic.tag_list_pages;
                    traffic.tag_list_pages += 1;
                    let expected_query = if page == 0 {
                        BTreeMap::from([("limit".to_owned(), "99".to_owned())])
                    } else {
                        BTreeMap::from([
                            ("limit".to_owned(), "99".to_owned()),
                            ("offset".to_owned(), "99".to_owned()),
                        ])
                    };
                    assert_eq!(query, expected_query, "tag-list query for page {page}");
                    match (tag_route, page) {
                        (TagRoute::Single, 0) => serde_json::json!({
                            "data": [{
                                "id": TEST_TAG_ID,
                                "key": "open",
                                "name": "Open",
                                "color": "blue"
                            }],
                            "pagination": {
                                "has_more": false,
                                "limit": 99,
                                "offset": 0,
                                "total": 1
                            }
                        })
                        .to_string(),
                        (TagRoute::TargetSecondPage, 0) => {
                            let tags = (0..99)
                                .map(|index| {
                                    serde_json::json!({
                                        "id": format!("other-tag-{index}"),
                                        "key": format!("other_{index}"),
                                        "name": format!("Other {index}"),
                                        "color": "grey"
                                    })
                                })
                                .collect::<Vec<_>>();
                            serde_json::json!({
                                "data": tags,
                                "pagination": {
                                    "has_more": true,
                                    "limit": 99,
                                    "offset": 0,
                                    "total": 100
                                }
                            })
                            .to_string()
                        }
                        (TagRoute::TargetSecondPage, 1) => serde_json::json!({
                            "data": [{
                                "id": TEST_TAG_ID,
                                "key": "open",
                                "name": "Open",
                                "color": "blue"
                            }],
                            "pagination": {
                                "has_more": false,
                                "limit": 99,
                                "offset": 99,
                                "total": 100
                            }
                        })
                        .to_string(),
                        (TagRoute::BeyondLimit, 0) => serde_json::json!({
                            "data": [{
                                "id": "other-tag",
                                "key": "other",
                                "name": "Other",
                                "color": "grey"
                            }],
                            "pagination": {
                                "has_more": true,
                                "limit": 99,
                                "offset": 0,
                                "total": 1001
                            }
                        })
                        .to_string(),
                        _ => panic!("unexpected tag fixture page {page}"),
                    }
                } else {
                    panic!("unexpected property fixture route: {request_line}");
                };
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                traffic.requests.push(request);
                stream
                    .write_all(response.as_bytes())
                    .await
                    .expect("write property fixture response");
                stream
                    .shutdown()
                    .await
                    .expect("shutdown property fixture response");
            }
            traffic
        });
        (format!("http://{address}"), shutdown_tx, task)
    }

    fn route_fixture_client(base_url: String) -> AnytypeClient {
        let mut config = crate::client::ClientConfig::default().app_name("property-route-fixture");
        config.base_url = Some(base_url);
        config.keystore = Some("env".to_owned());
        let client = AnytypeClient::with_config(config).expect("property fixture client");
        client.set_api_key(crate::keystore::HttpCredentials::new("fixture-token"));
        client
    }

    #[tokio::test]
    async fn direct_property_get_is_cache_independent_and_exactly_scoped() {
        let (base_url, shutdown, traffic) =
            route_aware_property_server(TEST_PROPERTY_ID, TagRoute::Single).await;
        let client = route_fixture_client(base_url);
        assert!(
            client.cache().is_enabled(),
            "fixture must exercise cache-on behavior"
        );

        let property = client
            .property(TEST_SPACE_ID, TEST_PROPERTY_ID)
            .with_tags()
            .get_direct()
            .await
            .expect("direct property");
        assert_eq!(property.id, TEST_PROPERTY_ID);
        assert!(
            property.tags().is_none(),
            "direct metadata must discard embedded tags"
        );

        shutdown.send(()).expect("stop property fixture");
        let traffic = traffic.await.expect("property fixture task");
        assert_eq!(
            traffic.property_list_pages, 0,
            "must not prime property cache"
        );
        assert_eq!(traffic.direct_property_gets, 1);
        assert_eq!(traffic.tag_list_pages, 0);
        assert_eq!(traffic.requests.len(), 1);
        assert_eq!(
            traffic.requests[0].lines().next().unwrap(),
            format!("GET /v1/spaces/{TEST_SPACE_ID}/properties/{TEST_PROPERTY_ID} HTTP/1.1")
        );
    }

    #[tokio::test]
    async fn explicit_id_tag_lookup_uses_direct_property_get_with_cold_cache() {
        let (base_url, shutdown, traffic) =
            route_aware_property_server(TEST_PROPERTY_ID, TagRoute::Single).await;
        let client = route_fixture_client(base_url);
        assert!(
            client.cache().is_enabled(),
            "fixture must exercise cache-on behavior"
        );

        let tag = client
            .lookup_property_tag(TEST_SPACE_ID, TEST_PROPERTY_ID, "Open")
            .await
            .expect("explicit-id tag lookup");
        assert_eq!(tag.id, TEST_TAG_ID);

        shutdown.send(()).expect("stop property fixture");
        let traffic = traffic.await.expect("property fixture task");
        assert_eq!(
            traffic.property_list_pages, 0,
            "must not prime property cache"
        );
        assert_eq!(traffic.direct_property_gets, 1);
        assert_eq!(traffic.tag_list_pages, 1);
        assert_eq!(traffic.requests.len(), 2);
    }

    #[tokio::test]
    async fn explicit_id_tag_lookup_finds_second_page_target_within_budget() {
        let (base_url, shutdown, traffic) =
            route_aware_property_server(TEST_PROPERTY_ID, TagRoute::TargetSecondPage).await;
        let client = route_fixture_client(base_url);

        let tag = client
            .lookup_property_tag(TEST_SPACE_ID, TEST_PROPERTY_ID, "open")
            .await
            .expect("bounded second-page tag lookup");
        assert_eq!(tag.id, TEST_TAG_ID);

        shutdown.send(()).expect("stop property fixture");
        let traffic = traffic.await.expect("property fixture task");
        assert_eq!(traffic.property_list_pages, 0);
        assert_eq!(traffic.direct_property_gets, 1);
        assert_eq!(traffic.tag_list_pages, 2);
        assert_eq!(traffic.requests.len(), 3);
    }

    #[tokio::test]
    async fn explicit_id_tag_lookup_reports_incomplete_scan_beyond_budget() {
        let (base_url, shutdown, traffic) =
            route_aware_property_server(TEST_PROPERTY_ID, TagRoute::BeyondLimit).await;
        let client = route_fixture_client(base_url);

        let error = client
            .lookup_property_tag(TEST_SPACE_ID, TEST_PROPERTY_ID, "absent")
            .await
            .expect_err("incomplete tag scan must not report not-found");
        assert!(matches!(
            error,
            AnytypeError::ResolutionLimitExceeded {
                obj_type,
                limit: MAX_RESOLVE_SCAN_ITEMS,
                ..
            } if obj_type == "tag"
        ));

        shutdown.send(()).expect("stop property fixture");
        let traffic = traffic.await.expect("property fixture task");
        assert_eq!(traffic.property_list_pages, 0);
        assert_eq!(traffic.direct_property_gets, 1);
        assert_eq!(
            traffic.tag_list_pages, 1,
            "known over-budget total stops immediately"
        );
        assert_eq!(traffic.requests.len(), 2);
    }

    #[tokio::test]
    async fn direct_property_identity_mismatch_is_secret_safe_and_skips_tags() {
        let (base_url, shutdown, traffic) =
            route_aware_property_server(OTHER_PROPERTY_ID, TagRoute::Single).await;
        let client = route_fixture_client(base_url);

        let error = client
            .lookup_property_tag(TEST_SPACE_ID, TEST_PROPERTY_ID, "Open")
            .await
            .expect_err("mismatched direct property must fail closed");
        let AnytypeError::Other { message } = &error else {
            panic!("identity mismatch must be an upstream error: {error}");
        };
        assert_eq!(message, "Anytype returned a mismatched property identity");
        let display = error.to_string();
        for private in [
            TEST_SPACE_ID,
            TEST_PROPERTY_ID,
            OTHER_PROPERTY_ID,
            "private-property-body-marker",
        ] {
            assert!(
                !display.contains(private),
                "error display leaked fixture data"
            );
        }

        shutdown.send(()).expect("stop property fixture");
        let traffic = traffic.await.expect("property fixture task");
        assert_eq!(traffic.property_list_pages, 0);
        assert_eq!(traffic.direct_property_gets, 1);
        assert_eq!(
            traffic.tag_list_pages, 0,
            "mismatch must stop before tag lookup"
        );
        assert_eq!(traffic.requests.len(), 1);
    }

    #[tokio::test]
    async fn direct_property_get_validates_both_ids_before_io() {
        let client = route_fixture_client("http://127.0.0.1:1".to_owned());
        for (space_id, property_id) in [
            ("unsafe/space", TEST_PROPERTY_ID),
            (TEST_SPACE_ID, "unsafe/property"),
        ] {
            let error = client
                .property(space_id, property_id)
                .get_direct()
                .await
                .expect_err("unsafe scoped id must fail before transport");
            assert!(matches!(error, AnytypeError::Validation { .. }));
        }
    }

    #[tokio::test]
    async fn property_key_tag_lookup_retains_documented_cache_requirement() {
        let mut config =
            crate::client::ClientConfig::default().app_name("property-key-cache-fixture");
        config.base_url = Some("http://127.0.0.1:1".to_owned());
        config.keystore = Some("env".to_owned());
        config.disable_cache = true;
        let client = AnytypeClient::with_config(config).expect("cache-disabled fixture client");
        client.set_api_key(crate::keystore::HttpCredentials::new("fixture-token"));

        let error = client
            .lookup_property_tag(TEST_SPACE_ID, "status", "Open")
            .await
            .expect_err("property-key lookup requires enabled cache");
        assert!(matches!(error, AnytypeError::CacheDisabled));
    }

    #[test]
    fn test_property_format_default() {
        let format: PropertyFormat = PropertyFormat::default();
        assert_eq!(format, PropertyFormat::Text);
    }

    #[test]
    fn test_property_format_display() {
        assert_eq!(PropertyFormat::Text.to_string(), "text");
        assert_eq!(PropertyFormat::Select.to_string(), "select");
        assert_eq!(PropertyFormat::MultiSelect.to_string(), "multi_select");
    }

    #[test]
    fn test_property_format_from_string() {
        use std::str::FromStr;
        assert_eq!(
            PropertyFormat::from_str("text").unwrap(),
            PropertyFormat::Text
        );
        assert_eq!(
            PropertyFormat::from_str("number").unwrap(),
            PropertyFormat::Number
        );
        assert_eq!(
            PropertyFormat::from_str("multi_select").unwrap(),
            PropertyFormat::MultiSelect
        );
    }

    #[test]
    fn test_property_value_as_str() {
        let text_val = PropertyValue::Text {
            text: "hello".to_string(),
        };
        assert_eq!(text_val.as_str(), Some("hello"));

        let url_val = PropertyValue::Url {
            url: "https://example.com".to_string(),
        };
        assert_eq!(url_val.as_str(), Some("https://example.com"));

        let files_val = PropertyValue::Files { files: vec![] };
        assert_eq!(files_val.as_str(), None);
    }

    #[test]
    fn test_property_value_as_bool() {
        let checkbox_true = PropertyValue::Checkbox { checkbox: true };
        assert_eq!(checkbox_true.as_bool(), Some(true));

        let checkbox_false = PropertyValue::Checkbox { checkbox: false };
        assert_eq!(checkbox_false.as_bool(), Some(false));

        let text_val = PropertyValue::Text {
            text: "true".to_string(),
        };
        assert_eq!(text_val.as_bool(), None);
    }

    #[test]
    fn test_property_value_as_array() {
        let files = PropertyValue::Files {
            files: vec!["file1".to_string(), "file2".to_string()],
        };
        assert_eq!(
            files.as_array(),
            Some(vec!["file1".to_string(), "file2".to_string()])
        );

        let text = PropertyValue::Text {
            text: "hello".to_string(),
        };
        assert_eq!(text.as_array(), None);
    }

    #[test]
    fn test_create_property_request_body_serialization() {
        let body = CreatePropertyRequestBody {
            name: "Priority".to_string(),
            key: Some("priority".to_string()),
            format: PropertyFormat::Select,
            tags: vec![],
        };

        let json = serde_json::to_string(&body).unwrap();
        assert!(json.contains("\"name\":\"Priority\""));
        assert!(json.contains("\"key\":\"priority\""));
        assert!(json.contains("\"format\":\"select\""));
    }

    #[test]
    fn test_update_property_request_body_requires_name_on_wire() {
        let body = UpdatePropertyRequestBody {
            name: "Priority".to_string(),
            key: None,
        };
        let json = serde_json::to_string(&body).unwrap();
        assert_eq!(json, r#"{"name":"Priority"}"#);
    }

    #[tokio::test]
    async fn test_update_property_rejects_key_without_name() {
        let mut config = crate::client::ClientConfig::default().app_name("property-update-unit");
        config.keystore = Some("env".to_string());
        let client = AnytypeClient::with_config(config).expect("client");
        let valid_id = "bafyreie6n5l5nkbjal37su54cha4coy7qzuhrnajluzv5qd5jvtsrxkequ";

        let error = client
            .update_property(valid_id, valid_id)
            .key("new_key")
            .update()
            .await
            .expect_err("a key-only REST update must fail validation");

        assert!(
            matches!(error, AnytypeError::Validation { ref message } if message.contains("name is required"))
        );
    }

    #[test]
    fn test_property_info_deserialization() {
        let json = r#"{
            "name": "Status",
            "format": "select",
            "id": "prop123",
            "key": "status"
        }"#;

        let prop: Property = serde_json::from_str(json).unwrap();
        assert_eq!(prop.name, "Status");
        assert_eq!(prop.format, PropertyFormat::Select);
        assert_eq!(prop.id, "prop123");
        assert_eq!(prop.key, "status");
    }
}
