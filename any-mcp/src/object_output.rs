// any-mcp - bounded, workflow-oriented MCP server for Anytype
//
// SPDX-FileCopyrightText: 2026 Steve Schoettler
// SPDX-License-Identifier: Apache-2.0

//! Deterministic, bounded object summaries and projected property values.

use std::{borrow::Cow, collections::HashSet, fmt};

use anytype::{
    objects::{Color, Object},
    properties::{PropertyValue, PropertyWithValue},
    tags::Tag,
};
use rmcp::schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Deserializer, Serialize, de};
use serde_json::Number;

use crate::{
    domain::{
        BoundedText, DisplayName, DomainValueError, EntityId, LastModified, MAX_TIMESTAMP_CHARS,
        ObjectId, ObjectSummary, SpaceId, TypeKey,
    },
    error::ToolError,
    validation::{BoundedList, MAX_PROJECTIONS},
};

/// Maximum characters retained in one scalar projected property value.
pub const MAX_PROPERTY_TEXT_CHARS: usize = 4_096;
/// Maximum references or tags retained in one projected property value.
pub const MAX_PROPERTY_VALUE_ITEMS: usize = 100;
/// Maximum characters in one projected tag name.
pub const MAX_PROJECTED_TAG_NAME_CHARS: usize = 256;
/// Maximum serialized characters in one projected JSON number.
pub const MAX_PROJECTED_NUMBER_CHARS: usize = 128;
/// Maximum absolute finite projected number.
pub const MAX_PROJECTED_NUMBER_ABS: f64 = 1_000_000_000_000_000.0;

type PropertyText = BoundedText<MAX_PROPERTY_TEXT_CHARS>;
type PropertyItems<T> = BoundedList<T, MAX_PROPERTY_VALUE_ITEMS>;
type ProjectedProperties = BoundedList<ProjectedProperty, MAX_PROJECTIONS>;

/// Projection policy for converting an upstream Anytype object.
#[derive(Debug, Clone, Copy)]
pub enum ProjectionMode<'a> {
    /// Return metadata only and do not inspect property values.
    SummaryOnly,
    /// Return only the explicitly requested keys, in first-request order.
    Selected(&'a [TypeKey]),
    /// Return every property only when the complete set fits the finite cap.
    AllBounded,
}

/// A bounded object together with an explicit property projection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ObjectOutput {
    /// Stable object metadata without a document body.
    summary: ObjectSummary,
    /// Explicitly projected properties; never an implicit full object body.
    properties: ProjectedProperties,
}

impl ObjectOutput {
    /// Borrows the stable summary.
    #[must_use]
    pub const fn summary(&self) -> &ObjectSummary {
        &self.summary
    }

    /// Borrows the deterministic projected properties.
    #[must_use]
    pub fn properties(&self) -> &[ProjectedProperty] {
        self.properties.as_slice()
    }
}

/// One closed projected property entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProjectedProperty {
    /// Stable Anytype property key.
    key: TypeKey,
    /// Bounded value retaining the upstream property format.
    value: ProjectedValue,
}

impl ProjectedProperty {
    /// Borrows the property key.
    #[must_use]
    pub const fn key(&self) -> &TypeKey {
        &self.key
    }

    /// Borrows the typed projected value.
    #[must_use]
    pub const fn value(&self) -> &ProjectedValue {
        &self.value
    }
}

/// Closed and bounded wire representation of every current `PropertyValue` variant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "format", rename_all = "snake_case", deny_unknown_fields)]
pub enum ProjectedValue {
    /// Plain bounded text.
    Text {
        /// Bounded text value.
        text: PropertyText,
    },
    /// Validated finite practical JSON number.
    Number {
        /// Finite bounded numeric value.
        number: ProjectedNumber,
    },
    /// One closed select option.
    Select {
        /// Selected tag option.
        select: ProjectedTag,
    },
    /// A bounded list of closed select options.
    MultiSelect {
        /// Selected tag options.
        multi_select: PropertyItems<ProjectedTag>,
    },
    /// Bounded RFC 3339-like upstream date text.
    Date {
        /// Bounded date text.
        date: ProjectedDate,
    },
    /// Bounded file references.
    Files {
        /// Validated file identifiers.
        files: PropertyItems<EntityId>,
    },
    /// Boolean checkbox value.
    Checkbox {
        /// Checkbox state.
        checkbox: bool,
    },
    /// Bounded URL value.
    Url {
        /// Bounded URL text.
        url: PropertyText,
    },
    /// Bounded email value.
    Email {
        /// Bounded email text.
        email: PropertyText,
    },
    /// Bounded phone value.
    Phone {
        /// Bounded phone text.
        phone: PropertyText,
    },
    /// Bounded validated object identifiers.
    Objects {
        /// Validated referenced object identifiers.
        objects: PropertyItems<EntityId>,
    },
}

/// Closed projected tag option.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProjectedTag {
    /// Stable tag identifier.
    id: EntityId,
    /// Stable tag key.
    key: TypeKey,
    /// Bounded tag display name.
    name: BoundedText<MAX_PROJECTED_TAG_NAME_CHARS>,
    /// Closed Anytype color enumeration.
    color: ProjectedColor,
}

/// Closed Anytype tag colors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProjectedColor {
    /// Grey.
    Grey,
    /// Yellow.
    Yellow,
    /// Orange.
    Orange,
    /// Red.
    Red,
    /// Pink.
    Pink,
    /// Purple.
    Purple,
    /// Blue.
    Blue,
    /// Ice.
    Ice,
    /// Teal.
    Teal,
    /// Lime.
    Lime,
}

/// A finite, practically bounded JSON number that preserves integer precision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct ProjectedNumber(Number);

/// A bounded RFC 3339 date-time retained from an Anytype property.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct ProjectedDate(BoundedText<MAX_TIMESTAMP_CHARS>);

impl ProjectedDate {
    fn new(value: impl Into<String>) -> Result<Self, ObjectOutputError> {
        let value = value.into();
        chrono::DateTime::parse_from_rfc3339(&value)
            .map_err(|_| ObjectOutputError::InvalidProperty)?;
        BoundedText::new(value).map(Self).map_err(property_domain)
    }
}

impl<'de> Deserialize<'de> for ProjectedDate {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::new(String::deserialize(deserializer)?).map_err(de::Error::custom)
    }
}

impl JsonSchema for ProjectedDate {
    fn schema_name() -> Cow<'static, str> {
        "ProjectedDate".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "minLength": 1,
            "maxLength": MAX_TIMESTAMP_CHARS,
            "format": "date-time",
        })
    }
}

impl ProjectedNumber {
    fn new(number: Number) -> Result<Self, ObjectOutputError> {
        let rendered = number.to_string();
        let Some(value) = number.as_f64() else {
            return Err(ObjectOutputError::BoundedValue);
        };
        if rendered.len() > MAX_PROJECTED_NUMBER_CHARS
            || !value.is_finite()
            || value.abs() > MAX_PROJECTED_NUMBER_ABS
        {
            return Err(ObjectOutputError::BoundedValue);
        }
        Ok(Self(number))
    }
}

impl<'de> Deserialize<'de> for ProjectedNumber {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::new(Number::deserialize(deserializer)?).map_err(de::Error::custom)
    }
}

impl JsonSchema for ProjectedNumber {
    fn schema_name() -> Cow<'static, str> {
        "ProjectedNumber".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "number",
            "minimum": -MAX_PROJECTED_NUMBER_ABS,
            "maximum": MAX_PROJECTED_NUMBER_ABS,
        })
    }
}

/// Secret-safe failure while validating an untrusted upstream object.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObjectOutputError {
    /// Required object metadata was absent or invalid.
    InvalidMetadata,
    /// A selected property was duplicated or invalid.
    InvalidProperty,
    /// A valid-shaped metadata or property value exceeded a wire bound.
    BoundedValue,
    /// A requested or returned property collection exceeded its hard cap.
    TooManyProperties,
}

impl ObjectOutputError {
    /// Maps the failure to a fixed caller-visible error without upstream data.
    #[must_use]
    pub const fn tool_error(self) -> ToolError {
        match self {
            Self::BoundedValue | Self::TooManyProperties => ToolError::bounded_result(),
            Self::InvalidMetadata | Self::InvalidProperty => ToolError::upstream(),
        }
    }
}

impl fmt::Display for ObjectOutputError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidMetadata => "object metadata is invalid",
            Self::InvalidProperty => "projected property is invalid",
            Self::BoundedValue => "projected value exceeds its bound",
            Self::TooManyProperties => "property projection exceeds its bound",
        })
    }
}

impl std::error::Error for ObjectOutputError {}

/// Converts only the stable metadata portion of an Anytype object.
pub fn object_summary(object: &Object) -> Result<ObjectSummary, ObjectOutputError> {
    let id = ObjectId::new(object.id.clone()).map_err(metadata_domain)?;
    let space_id = SpaceId::new(object.space_id.clone()).map_err(metadata_domain)?;
    let type_key = object
        .r#type
        .as_ref()
        .ok_or(ObjectOutputError::InvalidMetadata)
        .and_then(|value| TypeKey::new(value.key.clone()).map_err(metadata_domain))?;
    let name =
        DisplayName::new(object.name.clone().unwrap_or_default()).map_err(metadata_domain)?;
    let last_modified = last_modified(object)?;
    Ok(ObjectSummary::new(
        id,
        name,
        type_key,
        space_id,
        last_modified,
    ))
}

/// Converts an Anytype object under an explicit finite projection policy.
pub fn object_output(
    object: &Object,
    mode: ProjectionMode<'_>,
) -> Result<ObjectOutput, ObjectOutputError> {
    let summary = object_summary(object)?;
    let properties = match mode {
        ProjectionMode::SummaryOnly => ProjectedProperties::new(Vec::new())
            .map_err(|_| ObjectOutputError::TooManyProperties)?,
        ProjectionMode::Selected(keys) => selected_properties(&object.properties, keys)?,
        ProjectionMode::AllBounded => all_properties(&object.properties)?,
    };
    Ok(ObjectOutput {
        summary,
        properties,
    })
}

/// Produces the order-insensitive canonical projection used in cursor bindings.
pub fn normalized_projection_keys(keys: &[TypeKey]) -> Result<Vec<TypeKey>, ObjectOutputError> {
    if keys.len() > MAX_PROJECTIONS {
        return Err(ObjectOutputError::TooManyProperties);
    }
    let mut normalized = keys.to_vec();
    normalized.sort();
    normalized.dedup();
    Ok(normalized)
}

fn selected_properties(
    properties: &[PropertyWithValue],
    requested: &[TypeKey],
) -> Result<ProjectedProperties, ObjectOutputError> {
    if requested.len() > MAX_PROJECTIONS {
        return Err(ObjectOutputError::TooManyProperties);
    }
    let mut seen = HashSet::new();
    let mut result = Vec::new();
    for key in requested {
        if !seen.insert(key.as_str()) {
            continue;
        }
        let mut matches = properties
            .iter()
            .filter(|property| property.key == key.as_str());
        let Some(property) = matches.next() else {
            continue;
        };
        if matches.next().is_some() {
            return Err(ObjectOutputError::InvalidProperty);
        }
        result.push(project_property(property)?);
    }
    ProjectedProperties::new(result).map_err(|_| ObjectOutputError::TooManyProperties)
}

fn all_properties(
    properties: &[PropertyWithValue],
) -> Result<ProjectedProperties, ObjectOutputError> {
    if properties.len() > MAX_PROJECTIONS {
        return Err(ObjectOutputError::TooManyProperties);
    }
    let mut ordered: Vec<_> = properties.iter().collect();
    ordered.sort_by(|left, right| left.key.cmp(&right.key));
    if ordered.windows(2).any(|pair| pair[0].key == pair[1].key) {
        return Err(ObjectOutputError::InvalidProperty);
    }
    let projected = ordered
        .into_iter()
        .map(project_property)
        .collect::<Result<Vec<_>, _>>()?;
    ProjectedProperties::new(projected).map_err(|_| ObjectOutputError::TooManyProperties)
}

fn project_property(property: &PropertyWithValue) -> Result<ProjectedProperty, ObjectOutputError> {
    Ok(ProjectedProperty {
        key: TypeKey::new(property.key.clone()).map_err(property_domain)?,
        value: ProjectedValue::try_from(&property.value)?,
    })
}

impl TryFrom<&PropertyValue> for ProjectedValue {
    type Error = ObjectOutputError;

    fn try_from(value: &PropertyValue) -> Result<Self, Self::Error> {
        Ok(match value {
            PropertyValue::Text { text } => Self::Text {
                text: bounded(text)?,
            },
            PropertyValue::Number { number } => Self::Number {
                number: ProjectedNumber::new(number.clone())?,
            },
            PropertyValue::Select { select } => Self::Select {
                select: project_tag(select)?,
            },
            PropertyValue::MultiSelect { multi_select } => Self::MultiSelect {
                multi_select: bounded_values(multi_select, project_tag)?,
            },
            PropertyValue::Date { date } => Self::Date {
                date: bounded_date(date)?,
            },
            PropertyValue::Files { files } => Self::Files {
                files: bounded_values(files, |value| {
                    EntityId::new(value.clone()).map_err(property_domain)
                })?,
            },
            PropertyValue::Checkbox { checkbox } => Self::Checkbox {
                checkbox: *checkbox,
            },
            PropertyValue::Url { url } => Self::Url { url: bounded(url)? },
            PropertyValue::Email { email } => Self::Email {
                email: bounded(email)?,
            },
            PropertyValue::Phone { phone } => Self::Phone {
                phone: bounded(phone)?,
            },
            PropertyValue::Objects { objects } => Self::Objects {
                objects: bounded_values(objects, |value| {
                    EntityId::new(value.clone()).map_err(property_domain)
                })?,
            },
        })
    }
}

fn bounded(value: &str) -> Result<PropertyText, ObjectOutputError> {
    PropertyText::new(value).map_err(property_domain)
}

fn bounded_date(value: &str) -> Result<ProjectedDate, ObjectOutputError> {
    ProjectedDate::new(value)
}

fn bounded_values<T, U>(
    values: &[T],
    convert: impl Fn(&T) -> Result<U, ObjectOutputError>,
) -> Result<PropertyItems<U>, ObjectOutputError> {
    if values.len() > MAX_PROPERTY_VALUE_ITEMS {
        return Err(ObjectOutputError::BoundedValue);
    }
    let projected = values.iter().map(convert).collect::<Result<Vec<_>, _>>()?;
    PropertyItems::new(projected).map_err(|_| ObjectOutputError::BoundedValue)
}

fn project_tag(tag: &Tag) -> Result<ProjectedTag, ObjectOutputError> {
    Ok(ProjectedTag {
        id: EntityId::new(tag.id.clone()).map_err(property_domain)?,
        key: TypeKey::new(tag.key.clone()).map_err(property_domain)?,
        name: BoundedText::new(tag.name.clone()).map_err(property_domain)?,
        color: match tag.color {
            Color::Grey => ProjectedColor::Grey,
            Color::Yellow => ProjectedColor::Yellow,
            Color::Orange => ProjectedColor::Orange,
            Color::Red => ProjectedColor::Red,
            Color::Pink => ProjectedColor::Pink,
            Color::Purple => ProjectedColor::Purple,
            Color::Blue => ProjectedColor::Blue,
            Color::Ice => ProjectedColor::Ice,
            Color::Teal => ProjectedColor::Teal,
            Color::Lime => ProjectedColor::Lime,
        },
    })
}

fn last_modified(object: &Object) -> Result<Option<LastModified>, ObjectOutputError> {
    let mut matches = object
        .properties
        .iter()
        .filter(|property| property.key == "last_modified_date");
    let Some(property) = matches.next() else {
        return Ok(None);
    };
    if matches.next().is_some() {
        return Err(ObjectOutputError::InvalidMetadata);
    }
    let PropertyValue::Date { date } = &property.value else {
        return Err(ObjectOutputError::InvalidMetadata);
    };
    if chrono::DateTime::parse_from_rfc3339(date).is_err() {
        return Err(ObjectOutputError::InvalidMetadata);
    }
    LastModified::new(date.clone())
        .map(Some)
        .map_err(metadata_domain)
}

fn metadata_domain(error: DomainValueError) -> ObjectOutputError {
    match error {
        DomainValueError::TooLong { .. } => ObjectOutputError::BoundedValue,
        DomainValueError::Empty | DomainValueError::InvalidIdentifierCharacter => {
            ObjectOutputError::InvalidMetadata
        }
    }
}

fn property_domain(error: DomainValueError) -> ObjectOutputError {
    match error {
        DomainValueError::TooLong { .. } => ObjectOutputError::BoundedValue,
        DomainValueError::Empty | DomainValueError::InvalidIdentifierCharacter => {
            ObjectOutputError::InvalidProperty
        }
    }
}

#[cfg(test)]
mod tests {
    use anytype::{
        objects::{DataModel, ObjectLayout},
        properties::PropertyValue,
        types::Type,
    };
    use serde_json::json;

    use super::*;
    use crate::schema::output_schema;

    fn property(key: &str, value: PropertyValue) -> PropertyWithValue {
        PropertyWithValue {
            name: key.to_owned(),
            key: key.to_owned(),
            id: format!("id-{key}"),
            value,
        }
    }

    fn object(properties: Vec<PropertyWithValue>) -> Object {
        Object {
            archived: false,
            icon: None,
            id: "object-1".to_owned(),
            layout: ObjectLayout::default(),
            markdown: Some("secret body".to_owned()),
            name: Some("Roadmap".to_owned()),
            object: DataModel::default(),
            properties,
            snippet: Some("secret snippet".to_owned()),
            space_id: "space-1".to_owned(),
            r#type: Some(Type {
                archived: false,
                icon: None,
                id: "type-id".to_owned(),
                key: "page".to_owned(),
                layout: ObjectLayout::default(),
                name: Some("Page".to_owned()),
                plural_name: None,
                properties: Vec::new(),
            }),
        }
    }

    #[test]
    fn summary_validates_required_metadata_uri_and_optional_timestamp() {
        let value = object(vec![property(
            "last_modified_date",
            PropertyValue::Date {
                date: "2026-07-20T08:00:00Z".to_owned(),
            },
        )]);
        let summary = object_summary(&value).unwrap();
        assert_eq!(summary.id().as_str(), "object-1");
        assert_eq!(summary.type_key().as_str(), "page");
        assert_eq!(
            summary.last_modified().unwrap().as_str(),
            "2026-07-20T08:00:00Z"
        );
        assert_eq!(
            summary.resource_uri().as_str(),
            "anytype://spaces/space-1/objects/object-1"
        );

        let mut invalid = object(Vec::new());
        invalid.id = "../unsafe".to_owned();
        assert_eq!(
            object_summary(&invalid),
            Err(ObjectOutputError::InvalidMetadata)
        );
        invalid = object(Vec::new());
        invalid.r#type = None;
        assert_eq!(
            object_summary(&invalid),
            Err(ObjectOutputError::InvalidMetadata)
        );
        invalid = object(vec![property(
            "last_modified_date",
            PropertyValue::Date {
                date: "nope".to_owned(),
            },
        )]);
        assert_eq!(
            object_summary(&invalid),
            Err(ObjectOutputError::InvalidMetadata)
        );
    }

    #[test]
    fn selected_projection_is_requested_order_deduped_and_body_free() {
        let value = object(vec![
            property(
                "alpha",
                PropertyValue::Text {
                    text: "a".to_owned(),
                },
            ),
            property("beta", PropertyValue::Checkbox { checkbox: true }),
            property(
                "unrequested",
                PropertyValue::Text {
                    text: "private".to_owned(),
                },
            ),
        ]);
        let keys = [
            TypeKey::new("beta").unwrap(),
            TypeKey::new("missing").unwrap(),
            TypeKey::new("alpha").unwrap(),
            TypeKey::new("beta").unwrap(),
        ];
        let output = object_output(&value, ProjectionMode::Selected(&keys)).unwrap();
        assert_eq!(
            output
                .properties()
                .iter()
                .map(|p| p.key().as_str())
                .collect::<Vec<_>>(),
            ["beta", "alpha"]
        );
        let encoded = serde_json::to_string(&output).unwrap();
        assert!(!encoded.contains("secret body"));
        assert!(!encoded.contains("secret snippet"));
        assert!(!encoded.contains("unrequested"));
        assert!(!encoded.contains("private"));
    }

    #[test]
    fn all_property_variants_have_closed_bounded_wire_forms() {
        let tag = Tag {
            id: "tag-1".to_owned(),
            name: "Urgent".to_owned(),
            key: "urgent".to_owned(),
            color: Color::Red,
        };
        let values = vec![
            property(
                "text",
                PropertyValue::Text {
                    text: "x".to_owned(),
                },
            ),
            property(
                "number",
                PropertyValue::Number {
                    number: Number::from(42),
                },
            ),
            property(
                "select",
                PropertyValue::Select {
                    select: tag.clone(),
                },
            ),
            property(
                "multi",
                PropertyValue::MultiSelect {
                    multi_select: vec![tag],
                },
            ),
            property(
                "date",
                PropertyValue::Date {
                    date: "2026-07-20T08:00:00Z".to_owned(),
                },
            ),
            property(
                "files",
                PropertyValue::Files {
                    files: vec!["file-1".to_owned()],
                },
            ),
            property("checkbox", PropertyValue::Checkbox { checkbox: true }),
            property(
                "url",
                PropertyValue::Url {
                    url: "https://example.invalid".to_owned(),
                },
            ),
            property(
                "email",
                PropertyValue::Email {
                    email: "a@example.invalid".to_owned(),
                },
            ),
            property(
                "phone",
                PropertyValue::Phone {
                    phone: "+1".to_owned(),
                },
            ),
            property(
                "objects",
                PropertyValue::Objects {
                    objects: vec!["object-2".to_owned()],
                },
            ),
        ];
        let output = object_output(&object(values), ProjectionMode::AllBounded).unwrap();
        assert_eq!(output.properties().len(), 11);
        assert!(output_schema::<ObjectOutput>().is_ok());
        let schema = serde_json::to_string(&output_schema::<ObjectOutput>().unwrap()).unwrap();
        assert!(!schema.contains("additionalProperties\":true"));
    }

    #[test]
    fn malformed_oversized_and_duplicate_selected_values_fail_closed() {
        let huge = "x".repeat(MAX_PROPERTY_TEXT_CHARS + 1);
        assert_eq!(
            object_output(
                &object(vec![property("x", PropertyValue::Text { text: huge })]),
                ProjectionMode::AllBounded,
            ),
            Err(ObjectOutputError::BoundedValue)
        );
        let duplicates = object(vec![
            property("x", PropertyValue::Checkbox { checkbox: true }),
            property("x", PropertyValue::Checkbox { checkbox: false }),
        ]);
        assert_eq!(
            object_output(
                &duplicates,
                ProjectionMode::Selected(&[TypeKey::new("x").unwrap()])
            ),
            Err(ObjectOutputError::InvalidProperty)
        );
        let too_many = (0..=MAX_PROJECTIONS)
            .map(|index| {
                property(
                    &format!("p{index}"),
                    PropertyValue::Checkbox { checkbox: true },
                )
            })
            .collect();
        assert_eq!(
            object_output(&object(too_many), ProjectionMode::AllBounded),
            Err(ObjectOutputError::TooManyProperties)
        );
        assert_eq!(
            ObjectOutputError::BoundedValue.tool_error().code(),
            crate::error::ToolErrorCode::BoundedResult
        );

        let unsafe_file = object(vec![property(
            "files",
            PropertyValue::Files {
                files: vec!["../unsafe".to_owned()],
            },
        )]);
        assert_eq!(
            object_output(&unsafe_file, ProjectionMode::AllBounded),
            Err(ObjectOutputError::InvalidProperty)
        );

        let oversized_list = object(vec![property(
            "files",
            PropertyValue::Files {
                files: vec!["file-1".to_owned(); MAX_PROPERTY_VALUE_ITEMS + 1],
            },
        )]);
        assert_eq!(
            object_output(&oversized_list, ProjectionMode::AllBounded),
            Err(ObjectOutputError::BoundedValue)
        );

        let oversized_number = object(vec![property(
            "number",
            PropertyValue::Number {
                number: Number::from_f64(MAX_PROJECTED_NUMBER_ABS * 10.0).unwrap(),
            },
        )]);
        assert_eq!(
            object_output(&oversized_number, ProjectionMode::AllBounded),
            Err(ObjectOutputError::BoundedValue)
        );

        let malformed_date = object(vec![property(
            "date",
            PropertyValue::Date {
                date: "not-rfc3339".to_owned(),
            },
        )]);
        assert_eq!(
            object_output(&malformed_date, ProjectionMode::AllBounded),
            Err(ObjectOutputError::InvalidProperty)
        );
    }

    #[test]
    fn cursor_projection_normalization_is_order_insensitive() {
        let a = [
            TypeKey::new("b").unwrap(),
            TypeKey::new("a").unwrap(),
            TypeKey::new("b").unwrap(),
        ];
        let b = [TypeKey::new("a").unwrap(), TypeKey::new("b").unwrap()];
        assert_eq!(
            normalized_projection_keys(&a).unwrap(),
            normalized_projection_keys(&b).unwrap()
        );
    }

    #[test]
    fn output_deserialization_rejects_unknown_or_oversized_nested_values() {
        let mut encoded = serde_json::to_value(
            object_output(&object(Vec::new()), ProjectionMode::SummaryOnly).unwrap(),
        )
        .unwrap();
        encoded["extra"] = json!(true);
        assert!(serde_json::from_value::<ObjectOutput>(encoded).is_err());

        let bad_number = json!({"format":"number","number":1e16});
        assert!(serde_json::from_value::<ProjectedValue>(bad_number).is_err());
        for boundary in [
            json!({"format":"number","number":-MAX_PROJECTED_NUMBER_ABS}),
            json!({"format":"number","number":MAX_PROJECTED_NUMBER_ABS}),
        ] {
            assert!(serde_json::from_value::<ProjectedValue>(boundary).is_ok());
        }
        assert!(
            serde_json::from_value::<ProjectedValue>(json!({"format":"date","date":"not-rfc3339"}))
                .is_err()
        );
        let bad_list = json!({"format":"files","files":vec!["x"; MAX_PROPERTY_VALUE_ITEMS + 1]});
        assert!(serde_json::from_value::<ProjectedValue>(bad_list).is_err());
    }
}
