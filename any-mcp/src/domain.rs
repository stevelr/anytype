// any-mcp - bounded, workflow-oriented MCP server for Anytype
//
// SPDX-FileCopyrightText: 2026 Steve Schoettler
// SPDX-License-Identifier: Apache-2.0

//! Bounded domain values shared by MCP tools and resources.

use std::{borrow::Cow, fmt};

use rmcp::schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Deserializer, Serialize, de};

/// Maximum number of Unicode scalar values in an Anytype entity identifier.
pub const MAX_ENTITY_ID_CHARS: usize = 256;
/// Maximum number of Unicode scalar values in a displayed object name.
pub const MAX_DISPLAY_NAME_CHARS: usize = 512;
/// Maximum number of Unicode scalar values in an Anytype type key.
pub const MAX_TYPE_KEY_CHARS: usize = 256;
/// Maximum number of Unicode scalar values in a serialized timestamp.
pub const MAX_TIMESTAMP_CHARS: usize = 64;
/// Maximum number of Unicode scalar values in an Anytype resource URI.
pub const MAX_RESOURCE_URI_CHARS: usize = 1024;

/// Error returned when a bounded wire value violates its declared constraints.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DomainValueError {
    /// The value was empty even though at least one character is required.
    Empty,
    /// The value exceeded the field's maximum character count.
    TooLong { max_chars: usize },
    /// An identifier contained a character unsafe for an MCP resource path.
    InvalidIdentifierCharacter,
}

impl fmt::Display for DomainValueError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => formatter.write_str("value must not be empty"),
            Self::TooLong { max_chars } => {
                write!(
                    formatter,
                    "value must contain at most {max_chars} characters"
                )
            }
            Self::InvalidIdentifierCharacter => {
                formatter.write_str("identifier contains an unsafe character or path segment")
            }
        }
    }
}

impl std::error::Error for DomainValueError {}

/// A string whose serialized and deserialized forms contain at most `MAX`
/// Unicode scalar values.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct BoundedText<const MAX: usize>(String);

impl<const MAX: usize> BoundedText<MAX> {
    /// Validates and constructs a bounded string.
    pub fn new(value: impl Into<String>) -> Result<Self, DomainValueError> {
        let value = value.into();
        if value.chars().count() > MAX {
            return Err(DomainValueError::TooLong { max_chars: MAX });
        }
        Ok(Self(value))
    }

    /// Borrows the validated string value.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Consumes the wrapper and returns the validated string.
    #[must_use]
    pub fn into_inner(self) -> String {
        self.0
    }
}

impl<const MAX: usize> AsRef<str> for BoundedText<MAX> {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl<const MAX: usize> fmt::Display for BoundedText<MAX> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl<'de, const MAX: usize> Deserialize<'de> for BoundedText<MAX> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(de::Error::custom)
    }
}

impl<const MAX: usize> JsonSchema for BoundedText<MAX> {
    fn schema_name() -> Cow<'static, str> {
        Cow::Owned(format!("BoundedText{MAX}"))
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "maxLength": MAX,
        })
    }
}

macro_rules! entity_id {
    ($name:ident, $doc:literal) => {
        #[doc = $doc]
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            /// Validates and constructs the identifier.
            pub fn new(value: impl Into<String>) -> Result<Self, DomainValueError> {
                let value = value.into();
                validate_identifier(&value)?;
                Ok(Self(value))
            }

            /// Borrows the validated identifier.
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                self.as_str()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(self.as_str())
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                let value = String::deserialize(deserializer)?;
                Self::new(value).map_err(de::Error::custom)
            }
        }

        impl JsonSchema for $name {
            fn schema_name() -> Cow<'static, str> {
                stringify!($name).into()
            }

            fn json_schema(_: &mut SchemaGenerator) -> Schema {
                json_schema!({
                    "type": "string",
                    "minLength": 1,
                    "maxLength": MAX_ENTITY_ID_CHARS,
                    "pattern": "^(?!\\.{1,2}$)[A-Za-z0-9._~-]+$",
                })
            }
        }
    };
}

entity_id!(EntityId, "A validated identifier for any Anytype entity.");
entity_id!(ObjectId, "A validated Anytype object identifier.");
entity_id!(SpaceId, "A validated Anytype space identifier.");

fn validate_identifier(value: &str) -> Result<(), DomainValueError> {
    if value.is_empty() {
        return Err(DomainValueError::Empty);
    }
    if matches!(value, "." | "..") {
        return Err(DomainValueError::InvalidIdentifierCharacter);
    }
    if value.chars().count() > MAX_ENTITY_ID_CHARS {
        return Err(DomainValueError::TooLong {
            max_chars: MAX_ENTITY_ID_CHARS,
        });
    }
    if !value
        .bytes()
        .all(|character| character.is_ascii_alphanumeric() || b"._~-".contains(&character))
    {
        return Err(DomainValueError::InvalidIdentifierCharacter);
    }
    Ok(())
}

/// A bounded object display name.
pub type DisplayName = BoundedText<MAX_DISPLAY_NAME_CHARS>;
/// A bounded RFC 3339 timestamp string.
pub type LastModified = BoundedText<MAX_TIMESTAMP_CHARS>;

/// A nonempty, bounded Anytype type key.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct TypeKey(String);

impl TypeKey {
    /// Validates and constructs a type key.
    pub fn new(value: impl Into<String>) -> Result<Self, DomainValueError> {
        let value = value.into();
        if value.is_empty() {
            return Err(DomainValueError::Empty);
        }
        if value.chars().count() > MAX_TYPE_KEY_CHARS {
            return Err(DomainValueError::TooLong {
                max_chars: MAX_TYPE_KEY_CHARS,
            });
        }
        Ok(Self(value))
    }

    /// Borrows the validated type key.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl AsRef<str> for TypeKey {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl fmt::Display for TypeKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for TypeKey {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(de::Error::custom)
    }
}

impl JsonSchema for TypeKey {
    fn schema_name() -> Cow<'static, str> {
        "TypeKey".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "minLength": 1,
            "maxLength": MAX_TYPE_KEY_CHARS,
        })
    }
}

/// Canonical MCP resource URI for one Anytype object.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct ObjectResourceUri(String);

impl ObjectResourceUri {
    /// Builds the canonical URI from already validated identifiers.
    #[must_use]
    pub fn new(space_id: &SpaceId, object_id: &ObjectId) -> Self {
        Self(format!(
            "anytype://spaces/{}/objects/{}",
            space_id.as_str(),
            object_id.as_str()
        ))
    }

    /// Borrows the canonical resource URI.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ObjectResourceUri {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for ObjectResourceUri {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        let suffix = value
            .strip_prefix("anytype://spaces/")
            .ok_or_else(|| de::Error::custom("invalid Anytype object resource URI"))?;
        let (space_id, object_id) = suffix
            .split_once("/objects/")
            .ok_or_else(|| de::Error::custom("invalid Anytype object resource URI"))?;
        let space_id = SpaceId::new(space_id).map_err(de::Error::custom)?;
        let object_id = ObjectId::new(object_id).map_err(de::Error::custom)?;
        Ok(Self::new(&space_id, &object_id))
    }
}

impl JsonSchema for ObjectResourceUri {
    fn schema_name() -> Cow<'static, str> {
        "ObjectResourceUri".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "minLength": 28,
            "maxLength": MAX_RESOURCE_URI_CHARS,
            "pattern": "^anytype://spaces/(?!\\.{1,2}/objects/)[A-Za-z0-9._~-]+/objects/(?!\\.{1,2}$)[A-Za-z0-9._~-]+$",
        })
    }
}

/// Compact metadata returned by object discovery and write workflows.
///
/// Fields are private so callers cannot mutate identifiers independently of
/// the canonical resource URI. Construct summaries with [`ObjectSummary::new`]
/// and inspect them through their getters.
///
/// ```compile_fail
/// use any_mcp::domain::{DisplayName, ObjectId, ObjectResourceUri, ObjectSummary, SpaceId, TypeKey};
///
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// let object_id = ObjectId::new("object-1")?;
/// let space_id = SpaceId::new("space-1")?;
/// let summary = ObjectSummary {
///     id: object_id.clone(),
///     name: DisplayName::new("Roadmap")?,
///     type_key: TypeKey::new("page")?,
///     space_id: space_id.clone(),
///     last_modified: None,
///     resource_uri: ObjectResourceUri::new(&space_id, &object_id),
/// };
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ObjectSummary {
    /// Stable Anytype object identifier.
    id: ObjectId,
    /// Display name, which may be empty for unnamed notes.
    name: DisplayName,
    /// Stable key of the object's Anytype type.
    type_key: TypeKey,
    /// Stable identifier of the containing Anytype space.
    space_id: SpaceId,
    /// RFC 3339 last-modified value when Anytype supplies it.
    #[serde(skip_serializing_if = "Option::is_none")]
    last_modified: Option<LastModified>,
    /// Canonical MCP resource URI for reading this object's body.
    resource_uri: ObjectResourceUri,
}

impl ObjectSummary {
    /// Creates a summary and derives its canonical resource URI.
    #[must_use]
    pub fn new(
        id: ObjectId,
        name: DisplayName,
        type_key: TypeKey,
        space_id: SpaceId,
        last_modified: Option<LastModified>,
    ) -> Self {
        let resource_uri = ObjectResourceUri::new(&space_id, &id);
        Self {
            id,
            name,
            type_key,
            space_id,
            last_modified,
            resource_uri,
        }
    }

    /// Returns the stable Anytype object identifier.
    #[must_use]
    pub const fn id(&self) -> &ObjectId {
        &self.id
    }

    /// Returns the bounded display name.
    #[must_use]
    pub const fn name(&self) -> &DisplayName {
        &self.name
    }

    /// Returns the stable Anytype type key.
    #[must_use]
    pub const fn type_key(&self) -> &TypeKey {
        &self.type_key
    }

    /// Returns the identifier of the containing Anytype space.
    #[must_use]
    pub const fn space_id(&self) -> &SpaceId {
        &self.space_id
    }

    /// Returns the last-modified value when Anytype supplied one.
    #[must_use]
    pub const fn last_modified(&self) -> Option<&LastModified> {
        self.last_modified.as_ref()
    }

    /// Returns the canonical MCP resource URI derived at construction.
    #[must_use]
    pub const fn resource_uri(&self) -> &ObjectResourceUri {
        &self.resource_uri
    }
}

impl<'de> Deserialize<'de> for ObjectSummary {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct WireSummary {
            id: ObjectId,
            name: DisplayName,
            type_key: TypeKey,
            space_id: SpaceId,
            last_modified: Option<LastModified>,
            resource_uri: ObjectResourceUri,
        }

        let wire = WireSummary::deserialize(deserializer)?;
        let expected_uri = ObjectResourceUri::new(&wire.space_id, &wire.id);
        if wire.resource_uri != expected_uri {
            return Err(de::Error::custom(
                "resource_uri does not match the summary identifiers",
            ));
        }
        Ok(Self {
            id: wire.id,
            name: wire.name,
            type_key: wire.type_key,
            space_id: wire.space_id,
            last_modified: wire.last_modified,
            resource_uri: wire.resource_uri,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn bounded_text_rejects_oversized_construction_and_deserialization() {
        assert_eq!(
            BoundedText::<3>::new("four"),
            Err(DomainValueError::TooLong { max_chars: 3 })
        );
        assert!(serde_json::from_value::<BoundedText<3>>(json!("four")).is_err());
    }

    #[test]
    fn identifiers_reject_resource_path_injection() {
        assert_eq!(
            ObjectId::new("object/other"),
            Err(DomainValueError::InvalidIdentifierCharacter)
        );
        assert_eq!(SpaceId::new(""), Err(DomainValueError::Empty));
        assert_eq!(
            SpaceId::new("."),
            Err(DomainValueError::InvalidIdentifierCharacter)
        );
        assert_eq!(
            ObjectId::new(".."),
            Err(DomainValueError::InvalidIdentifierCharacter)
        );
    }

    #[test]
    fn type_key_is_nonempty_and_bounded_at_runtime() {
        assert_eq!(TypeKey::new(""), Err(DomainValueError::Empty));
        assert!(serde_json::from_value::<TypeKey>(json!("")).is_err());
        assert_eq!(TypeKey::new("page").unwrap().as_str(), "page");

        let schema = rmcp::handler::server::tool::schema_for_type::<TypeKey>();
        assert_eq!(schema["minLength"], json!(1));
        assert_eq!(schema["maxLength"], json!(MAX_TYPE_KEY_CHARS));
    }

    #[test]
    fn object_summary_serializes_with_canonical_resource_uri() {
        let summary = ObjectSummary::new(
            ObjectId::new("obj-1").unwrap(),
            DisplayName::new("Roadmap").unwrap(),
            TypeKey::new("page").unwrap(),
            SpaceId::new("space-1").unwrap(),
            Some(LastModified::new("2026-07-20T06:00:00Z").unwrap()),
        );

        assert_eq!(
            serde_json::to_value(&summary).unwrap(),
            json!({
                "id": "obj-1",
                "name": "Roadmap",
                "type_key": "page",
                "space_id": "space-1",
                "last_modified": "2026-07-20T06:00:00Z",
                "resource_uri": "anytype://spaces/space-1/objects/obj-1"
            })
        );
        assert_eq!(summary.id().as_str(), "obj-1");
        assert_eq!(summary.type_key().as_str(), "page");
        assert_eq!(
            summary.resource_uri().as_str(),
            "anytype://spaces/space-1/objects/obj-1"
        );
    }

    #[test]
    fn resource_uri_deserialization_requires_canonical_shape() {
        let uri: ObjectResourceUri =
            serde_json::from_value(json!("anytype://spaces/space-1/objects/obj-1")).unwrap();
        assert_eq!(uri.as_str(), "anytype://spaces/space-1/objects/obj-1");
        assert!(
            serde_json::from_value::<ObjectResourceUri>(json!(
                "anytype://spaces/space-1/objects/obj-1/extra"
            ))
            .is_err()
        );
        assert!(
            serde_json::from_value::<ObjectResourceUri>(json!("anytype://spaces/../objects/obj-1"))
                .is_err()
        );
    }

    #[test]
    fn object_summary_deserialization_rejects_mismatched_resource_uri() {
        let value = json!({
            "id": "obj-1",
            "name": "Roadmap",
            "type_key": "page",
            "space_id": "space-1",
            "resource_uri": "anytype://spaces/other-space/objects/obj-1"
        });

        assert!(serde_json::from_value::<ObjectSummary>(value).is_err());
    }
}
