// any-mcp - bounded, workflow-oriented MCP server for Anytype
//
// SPDX-FileCopyrightText: 2026 Steve Schoettler
// SPDX-License-Identifier: Apache-2.0

//! Shared bounded values for object create and update mutations.
//!
//! The wire types in this module deliberately mirror every current Anytype
//! property format while retaining one deterministic representation for values
//! that Anytype may reorder or reformat. Property handlers remain responsible
//! for validating keys and formats against the effective object type before a
//! write. In particular, only after that validation may a missing returned
//! property be interpreted as a successfully cleared empty value.

use std::{borrow::Cow, fmt};

use anytype::{
    objects::{Color, Icon},
    properties::{PropertyFormat, PropertyValue, SetProperty},
};
use chrono::{SecondsFormat, Utc};
use rmcp::schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Deserializer, Serialize, de};
use serde_json::Number;

use crate::{
    domain::{BoundedText, DomainValueError, EntityId, MAX_TYPE_KEY_CHARS},
    validation::BoundedList,
};

/// Maximum property assignments accepted by one object mutation.
pub const MAX_MUTATION_PROPERTIES: usize = 50;
/// Maximum raw identifiers accepted by one set-valued property.
pub const MAX_MUTATION_IDS: usize = 100;
/// Maximum Unicode scalar values accepted by a scalar property value.
pub const MAX_MUTATION_TEXT_CHARS: usize = 4_096;
/// Maximum Unicode scalar values accepted by an emoji or built-in icon name.
pub const MAX_MUTATION_ICON_TEXT_CHARS: usize = 128;
/// Maximum serialized characters accepted in one property number.
pub const MAX_MUTATION_NUMBER_CHARS: usize = 128;
/// Maximum absolute finite property number.
pub const MAX_MUTATION_NUMBER_ABS: f64 = 1_000_000_000_000_000.0;
/// Maximum Unicode scalar values accepted by an RFC 3339 property timestamp.
pub const MAX_MUTATION_DATE_CHARS: usize = 64;

/// Bounded scalar property text. Empty text is retained as an explicit clear.
pub type MutationText = BoundedText<MAX_MUTATION_TEXT_CHARS>;
/// Bounded list of property assignments before deterministic key ordering.
pub type MutationProperties = BoundedList<MutationProperty, MAX_MUTATION_PROPERTIES>;

/// A nonempty, bounded ASCII property key safe for an Anytype mutation payload.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct MutationPropertyKey(String);

impl MutationPropertyKey {
    /// Validates an exact Anytype property key without trimming or case folding.
    pub fn new(value: impl Into<String>) -> Result<Self, MutationInputError> {
        let value = value.into();
        if value.is_empty() {
            return Err(MutationInputError::Empty);
        }
        if value.chars().count() > MAX_TYPE_KEY_CHARS {
            return Err(MutationInputError::TooLong);
        }
        if !value
            .bytes()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, b'_' | b'-'))
        {
            return Err(MutationInputError::UnsafePropertyKey);
        }
        Ok(Self(value))
    }

    /// Borrows the validated key exactly as supplied.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for MutationPropertyKey {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::new(String::deserialize(deserializer)?).map_err(de::Error::custom)
    }
}

impl JsonSchema for MutationPropertyKey {
    fn schema_name() -> Cow<'static, str> {
        "MutationPropertyKey".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "minLength": 1,
            "maxLength": MAX_TYPE_KEY_CHARS,
            "pattern": "^[A-Za-z0-9_-]+$",
        })
    }
}

/// A finite number in the canonical numeric representation used by mutations.
///
/// Integral floats are stored as JSON integers and every zero representation is
/// stored as integer zero. This lets create fingerprints and read-after-write
/// verification treat `1`, `1.0`, and `1e0` as the same value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct MutationNumber(Number);

impl MutationNumber {
    /// Validates and canonicalizes a bounded JSON number.
    pub fn new(number: Number) -> Result<Self, MutationInputError> {
        let rendered = number.to_string();
        let Some(value) = number.as_f64() else {
            return Err(MutationInputError::InvalidNumber);
        };
        if rendered.len() > MAX_MUTATION_NUMBER_CHARS
            || !value.is_finite()
            || value.abs() > MAX_MUTATION_NUMBER_ABS
        {
            return Err(MutationInputError::InvalidNumber);
        }

        let canonical = if value == 0.0 {
            Number::from(0)
        } else if value.fract() == 0.0 {
            // The practical bound is below 2^53 and far inside i64, so every
            // accepted integral value is exactly representable here.
            Number::from(value as i64)
        } else {
            Number::from_f64(value).ok_or(MutationInputError::InvalidNumber)?
        };
        Ok(Self(canonical))
    }

    /// Borrows the canonical JSON number.
    #[must_use]
    pub const fn as_number(&self) -> &Number {
        &self.0
    }
}

impl<'de> Deserialize<'de> for MutationNumber {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::new(Number::deserialize(deserializer)?).map_err(de::Error::custom)
    }
}

impl JsonSchema for MutationNumber {
    fn schema_name() -> Cow<'static, str> {
        "MutationNumber".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "number",
            "minimum": -MAX_MUTATION_NUMBER_ABS,
            "maximum": MAX_MUTATION_NUMBER_ABS,
        })
    }
}

/// A canonical UTC RFC 3339 property timestamp.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct MutationDate(BoundedText<MAX_MUTATION_DATE_CHARS>);

impl MutationDate {
    /// Parses a bounded RFC 3339 timestamp and canonicalizes its instant to UTC.
    pub fn new(value: impl Into<String>) -> Result<Self, MutationInputError> {
        let value = value.into();
        if value.chars().count() > MAX_MUTATION_DATE_CHARS {
            return Err(MutationInputError::TooLong);
        }
        let parsed = chrono::DateTime::parse_from_rfc3339(&value)
            .map_err(|_| MutationInputError::InvalidDate)?;
        let canonical = parsed
            .with_timezone(&Utc)
            .to_rfc3339_opts(SecondsFormat::AutoSi, true);
        BoundedText::new(canonical)
            .map(Self)
            .map_err(|_| MutationInputError::TooLong)
    }

    /// Borrows the canonical UTC timestamp.
    #[must_use]
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl<'de> Deserialize<'de> for MutationDate {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::new(String::deserialize(deserializer)?).map_err(de::Error::custom)
    }
}

impl JsonSchema for MutationDate {
    fn schema_name() -> Cow<'static, str> {
        "MutationDate".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "minLength": 1,
            "maxLength": MAX_MUTATION_DATE_CHARS,
            "format": "date-time",
        })
    }
}

/// Bounded nonempty emoji text or built-in icon name.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct MutationIconText(BoundedText<MAX_MUTATION_ICON_TEXT_CHARS>);

impl MutationIconText {
    /// Validates icon text without Unicode or case normalization.
    pub fn new(value: impl Into<String>) -> Result<Self, MutationInputError> {
        let value = value.into();
        if value.is_empty() {
            return Err(MutationInputError::Empty);
        }
        BoundedText::new(value)
            .map(Self)
            .map_err(|_| MutationInputError::TooLong)
    }

    /// Borrows the exact bounded icon text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl<'de> Deserialize<'de> for MutationIconText {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::new(String::deserialize(deserializer)?).map_err(de::Error::custom)
    }
}

impl JsonSchema for MutationIconText {
    fn schema_name() -> Cow<'static, str> {
        "MutationIconText".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "minLength": 1,
            "maxLength": MAX_MUTATION_ICON_TEXT_CHARS,
        })
    }
}

/// A bounded canonical set of stable Anytype identifiers.
///
/// The raw input count is checked before sorting and deduplication, preventing
/// a large duplicate-filled request from bypassing the input cap.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct MutationIds(Vec<EntityId>);

impl MutationIds {
    /// Validates the raw count, then sorts and deduplicates identifiers.
    pub fn new(mut values: Vec<EntityId>) -> Result<Self, MutationInputError> {
        if values.len() > MAX_MUTATION_IDS {
            return Err(MutationInputError::TooManyIds);
        }
        values.sort();
        values.dedup();
        Ok(Self(values))
    }

    /// Borrows the canonical sorted unique identifiers.
    #[must_use]
    pub fn as_slice(&self) -> &[EntityId] {
        &self.0
    }

    /// Returns whether the canonical set is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl<'de> Deserialize<'de> for MutationIds {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::new(Vec::deserialize(deserializer)?).map_err(de::Error::custom)
    }
}

impl JsonSchema for MutationIds {
    fn schema_name() -> Cow<'static, str> {
        "MutationIds".into()
    }

    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "array",
            "items": generator.subschema_for::<EntityId>(),
            "maxItems": MAX_MUTATION_IDS,
        })
    }
}

/// Closed Anytype color values accepted for built-in icons.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MutationColor {
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

impl From<MutationColor> for Color {
    fn from(value: MutationColor) -> Self {
        match value {
            MutationColor::Grey => Self::Grey,
            MutationColor::Yellow => Self::Yellow,
            MutationColor::Orange => Self::Orange,
            MutationColor::Red => Self::Red,
            MutationColor::Pink => Self::Pink,
            MutationColor::Purple => Self::Purple,
            MutationColor::Blue => Self::Blue,
            MutationColor::Ice => Self::Ice,
            MutationColor::Teal => Self::Teal,
            MutationColor::Lime => Self::Lime,
        }
    }
}

/// Closed, bounded icon replacement accepted by object mutations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "format", rename_all = "snake_case", deny_unknown_fields)]
pub enum MutationIcon {
    /// Exact bounded emoji representation.
    Emoji {
        /// Nonempty emoji text, including possible joined sequences.
        emoji: MutationIconText,
    },
    /// Existing Anytype file object used as an icon.
    File {
        /// Stable file object identifier, never a path or URL.
        file: EntityId,
    },
    /// Named built-in icon with a closed color.
    Icon {
        /// Exact nonempty built-in icon name.
        name: MutationIconText,
        /// Closed Anytype icon color.
        color: MutationColor,
    },
}

impl MutationIcon {
    /// Converts the bounded wire icon to the Anytype API representation.
    #[must_use]
    pub fn to_anytype(&self) -> Icon {
        match self {
            Self::Emoji { emoji } => Icon::Emoji {
                emoji: emoji.as_str().to_owned(),
            },
            Self::File { file } => Icon::File {
                file: file.as_str().to_owned(),
            },
            Self::Icon { name, color } => Icon::Icon {
                name: name.as_str().to_owned(),
                color: (*color).into(),
            },
        }
    }

    /// Semantically compares a returned Anytype icon.
    ///
    /// An absent icon never matches because object mutation exposes no icon
    /// clear form.
    pub fn matches_returned(&self, returned: Option<&Icon>) -> Result<bool, MutationCompareError> {
        Ok(match (self, returned) {
            (Self::Emoji { emoji }, Some(Icon::Emoji { emoji: actual })) => {
                validate_returned_icon_text(actual)?;
                emoji.as_str() == actual
            }
            (Self::File { file }, Some(Icon::File { file: actual })) => {
                returned_id(actual)? == *file
            }
            (
                Self::Icon { name, color },
                Some(Icon::Icon {
                    name: actual_name,
                    color: actual_color,
                }),
            ) => {
                validate_returned_icon_text(actual_name)?;
                name.as_str() == actual_name && actual_color == &Color::from(*color)
            }
            _ => false,
        })
    }
}

impl From<&MutationIcon> for Icon {
    fn from(value: &MutationIcon) -> Self {
        value.to_anytype()
    }
}

/// One closed typed property assignment shared by create and update.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "format", rename_all = "snake_case", deny_unknown_fields)]
pub enum MutationProperty {
    /// Plain text; an empty string is an explicit clear.
    Text {
        /// Stable property key.
        key: MutationPropertyKey,
        /// Exact bounded replacement text.
        text: MutationText,
    },
    /// Finite bounded numeric value.
    Number {
        /// Stable property key.
        key: MutationPropertyKey,
        /// Canonical number.
        number: MutationNumber,
    },
    /// One existing select-option identifier.
    Select {
        /// Stable property key.
        key: MutationPropertyKey,
        /// Selected tag identifier.
        select: EntityId,
    },
    /// Existing select-option identifiers; empty explicitly clears the set.
    MultiSelect {
        /// Stable property key.
        key: MutationPropertyKey,
        /// Canonical selected tag identifiers.
        multi_select: MutationIds,
    },
    /// RFC 3339 timestamp stored as a canonical UTC instant.
    Date {
        /// Stable property key.
        key: MutationPropertyKey,
        /// Canonical timestamp.
        date: MutationDate,
    },
    /// Existing file identifiers; empty explicitly clears the set.
    Files {
        /// Stable property key.
        key: MutationPropertyKey,
        /// Canonical file identifiers.
        files: MutationIds,
    },
    /// Checkbox state; false is a scalar value, not a clear marker.
    Checkbox {
        /// Stable property key.
        key: MutationPropertyKey,
        /// Replacement checkbox state.
        checkbox: bool,
    },
    /// URL text; an empty string is an explicit clear.
    Url {
        /// Stable property key.
        key: MutationPropertyKey,
        /// Exact bounded URL text.
        url: MutationText,
    },
    /// Email text; an empty string is an explicit clear.
    Email {
        /// Stable property key.
        key: MutationPropertyKey,
        /// Exact bounded email text.
        email: MutationText,
    },
    /// Phone text; an empty string is an explicit clear.
    Phone {
        /// Stable property key.
        key: MutationPropertyKey,
        /// Exact bounded phone text.
        phone: MutationText,
    },
    /// Existing object identifiers; empty explicitly clears the relation set.
    Objects {
        /// Stable property key.
        key: MutationPropertyKey,
        /// Canonical related object identifiers.
        objects: MutationIds,
    },
}

impl MutationProperty {
    /// Borrows the stable property key.
    #[must_use]
    pub const fn key(&self) -> &MutationPropertyKey {
        match self {
            Self::Text { key, .. }
            | Self::Number { key, .. }
            | Self::Select { key, .. }
            | Self::MultiSelect { key, .. }
            | Self::Date { key, .. }
            | Self::Files { key, .. }
            | Self::Checkbox { key, .. }
            | Self::Url { key, .. }
            | Self::Email { key, .. }
            | Self::Phone { key, .. }
            | Self::Objects { key, .. } => key,
        }
    }

    /// Returns the Anytype property format represented by this assignment.
    #[must_use]
    pub const fn format(&self) -> PropertyFormat {
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

    /// Returns whether this exact assignment is a supported property clear.
    #[must_use]
    pub fn is_clear(&self) -> bool {
        match self {
            Self::Text { text, .. } => text.as_str().is_empty(),
            Self::MultiSelect { multi_select, .. } => multi_select.is_empty(),
            Self::Files { files, .. } => files.is_empty(),
            Self::Url { url, .. } => url.as_str().is_empty(),
            Self::Email { email, .. } => email.as_str().is_empty(),
            Self::Phone { phone, .. } => phone.as_str().is_empty(),
            Self::Objects { objects, .. } => objects.is_empty(),
            Self::Number { .. }
            | Self::Select { .. }
            | Self::Date { .. }
            | Self::Checkbox { .. } => false,
        }
    }

    /// Applies this assignment through the Anytype API's typed property trait.
    ///
    /// The outgoing payload intentionally omits the input-only `format` tag.
    #[must_use]
    pub fn apply<R: SetProperty>(&self, request: R) -> R {
        let key = self.key().as_str();
        match self {
            Self::Text { text, .. } => request.set_text(key, text.as_str()),
            Self::Number { number, .. } => request.set_number(key, number.as_number().clone()),
            Self::Select { select, .. } => request.set_select(key, select.as_str()),
            Self::MultiSelect { multi_select, .. } => {
                request.set_multi_select(key, multi_select.as_slice().iter().map(EntityId::as_str))
            }
            Self::Date { date, .. } => request.set_date(key, date.as_str()),
            Self::Files { files, .. } => {
                request.set_files(key, files.as_slice().iter().map(EntityId::as_str))
            }
            Self::Checkbox { checkbox, .. } => request.set_checkbox(key, *checkbox),
            Self::Url { url, .. } => request.set_url(key, url.as_str()),
            Self::Email { email, .. } => request.set_email(key, email.as_str()),
            Self::Phone { phone, .. } => request.set_phone(key, phone.as_str()),
            Self::Objects { objects, .. } => {
                request.set_objects(key, objects.as_slice().iter().map(EntityId::as_str))
            }
        }
    }

    /// Semantically compares one returned Anytype property value.
    ///
    /// A missing returned value compares equal only to a supported clear. The
    /// caller must first prove that this key and format belong to the effective
    /// object type; otherwise accepting absence could hide an ignored write.
    pub fn matches_returned(
        &self,
        returned: Option<&PropertyValue>,
    ) -> Result<bool, MutationCompareError> {
        let Some(returned) = returned else {
            return Ok(self.is_clear());
        };
        Ok(match (self, returned) {
            (Self::Text { text, .. }, PropertyValue::Text { text: actual }) => {
                validate_returned_text(actual)?;
                text.as_str() == actual
            }
            (Self::Number { number, .. }, PropertyValue::Number { number: actual }) => {
                canonical_returned_number(actual)? == *number
            }
            (Self::Select { select, .. }, PropertyValue::Select { select: actual }) => {
                returned_id(&actual.id)? == *select
            }
            (
                Self::MultiSelect { multi_select, .. },
                PropertyValue::MultiSelect {
                    multi_select: actual,
                },
            ) => returned_tag_ids(actual)? == *multi_select,
            (Self::Date { date, .. }, PropertyValue::Date { date: actual }) => {
                canonical_returned_date(actual)? == *date
            }
            (Self::Files { files, .. }, PropertyValue::Files { files: actual }) => {
                returned_ids(actual)? == *files
            }
            (Self::Checkbox { checkbox, .. }, PropertyValue::Checkbox { checkbox: actual }) => {
                checkbox == actual
            }
            (Self::Url { url, .. }, PropertyValue::Url { url: actual }) => {
                validate_returned_text(actual)?;
                url.as_str() == actual
            }
            (Self::Email { email, .. }, PropertyValue::Email { email: actual }) => {
                validate_returned_text(actual)?;
                email.as_str() == actual
            }
            (Self::Phone { phone, .. }, PropertyValue::Phone { phone: actual }) => {
                validate_returned_text(actual)?;
                phone.as_str() == actual
            }
            (Self::Objects { objects, .. }, PropertyValue::Objects { objects: actual }) => {
                returned_ids(actual)? == *objects
            }
            _ => false,
        })
    }
}

/// Returns cloned property assignments in deterministic key order.
///
/// Duplicate keys are rejected rather than silently applying multiple values
/// of possibly different formats. Each contained numeric, date, and ID-set
/// value is already canonical, so the result is suitable for an explicit
/// versioned create-fingerprint representation.
pub fn normalized_properties(
    properties: &MutationProperties,
) -> Result<Vec<MutationProperty>, MutationInputError> {
    let mut properties = properties.as_slice().to_vec();
    properties.sort_by(|left, right| left.key().cmp(right.key()));
    if properties
        .windows(2)
        .any(|pair| pair[0].key() == pair[1].key())
    {
        return Err(MutationInputError::DuplicatePropertyKey);
    }
    Ok(properties)
}

/// Invalid bounded mutation input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MutationInputError {
    /// A required value was empty.
    Empty,
    /// A string value exceeded its declared character bound.
    TooLong,
    /// A property key contained a character outside the safe ASCII grammar.
    UnsafePropertyKey,
    /// A number was non-finite, oversized, or outside the practical range.
    InvalidNumber,
    /// A timestamp was not valid RFC 3339.
    InvalidDate,
    /// A raw set-valued property exceeded its identifier count.
    TooManyIds,
    /// More than one property assignment used the same stable key.
    DuplicatePropertyKey,
}

impl fmt::Display for MutationInputError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Empty => "value must not be empty",
            Self::TooLong => "value exceeds its maximum length",
            Self::UnsafePropertyKey => "property key contains unsafe characters",
            Self::InvalidNumber => "number is outside the supported finite range",
            Self::InvalidDate => "date must be RFC 3339",
            Self::TooManyIds => "property identifier list exceeds its maximum length",
            Self::DuplicatePropertyKey => "property keys must be unique",
        })
    }
}

impl std::error::Error for MutationInputError {}

/// Failure while validating an untrusted returned mutation value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MutationCompareError {
    /// A valid-shaped returned value exceeded the MCP safety bound.
    Bounded,
    /// A returned identifier, date, icon, or value was structurally malformed.
    Malformed,
}

impl fmt::Display for MutationCompareError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Bounded => "returned mutation value exceeds its bound",
            Self::Malformed => "returned mutation value is malformed",
        })
    }
}

impl std::error::Error for MutationCompareError {}

fn validate_returned_text(value: &str) -> Result<(), MutationCompareError> {
    if value.chars().count() > MAX_MUTATION_TEXT_CHARS {
        Err(MutationCompareError::Bounded)
    } else {
        Ok(())
    }
}

fn validate_returned_icon_text(value: &str) -> Result<(), MutationCompareError> {
    if value.is_empty() {
        return Err(MutationCompareError::Malformed);
    }
    if value.chars().count() > MAX_MUTATION_ICON_TEXT_CHARS {
        return Err(MutationCompareError::Bounded);
    }
    Ok(())
}

fn returned_id(value: &str) -> Result<EntityId, MutationCompareError> {
    EntityId::new(value.to_owned()).map_err(returned_domain_error)
}

fn returned_domain_error(error: DomainValueError) -> MutationCompareError {
    match error {
        DomainValueError::TooLong { .. } => MutationCompareError::Bounded,
        DomainValueError::Empty | DomainValueError::InvalidIdentifierCharacter => {
            MutationCompareError::Malformed
        }
    }
}

fn returned_ids(values: &[String]) -> Result<MutationIds, MutationCompareError> {
    if values.len() > MAX_MUTATION_IDS {
        return Err(MutationCompareError::Bounded);
    }
    let values = values
        .iter()
        .map(|value| returned_id(value))
        .collect::<Result<Vec<_>, _>>()?;
    MutationIds::new(values).map_err(|_| MutationCompareError::Bounded)
}

fn returned_tag_ids(values: &[anytype::tags::Tag]) -> Result<MutationIds, MutationCompareError> {
    if values.len() > MAX_MUTATION_IDS {
        return Err(MutationCompareError::Bounded);
    }
    let values = values
        .iter()
        .map(|value| returned_id(&value.id))
        .collect::<Result<Vec<_>, _>>()?;
    MutationIds::new(values).map_err(|_| MutationCompareError::Bounded)
}

fn canonical_returned_number(value: &Number) -> Result<MutationNumber, MutationCompareError> {
    MutationNumber::new(value.clone()).map_err(|_| MutationCompareError::Bounded)
}

fn canonical_returned_date(value: &str) -> Result<MutationDate, MutationCompareError> {
    MutationDate::new(value.to_owned()).map_err(|error| match error {
        MutationInputError::TooLong => MutationCompareError::Bounded,
        MutationInputError::InvalidDate => MutationCompareError::Malformed,
        MutationInputError::Empty
        | MutationInputError::UnsafePropertyKey
        | MutationInputError::InvalidNumber
        | MutationInputError::TooManyIds
        | MutationInputError::DuplicatePropertyKey => MutationCompareError::Malformed,
    })
}

#[cfg(test)]
mod tests {
    use anytype::{
        objects::{Color, DataModel},
        properties::{PropertyValue, SetProperty},
    };
    use rmcp::schemars::schema_for;
    use serde_json::{Value, json};

    use super::*;

    const ID_A: &str = "bafyreiaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const ID_B: &str = "bafyreibbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    fn property(value: Value) -> MutationProperty {
        serde_json::from_value(value).expect("valid mutation property")
    }

    fn ids(values: &[&str]) -> MutationIds {
        MutationIds::new(
            values
                .iter()
                .map(|value| EntityId::new(*value).unwrap())
                .collect(),
        )
        .unwrap()
    }

    #[derive(Default)]
    struct PropertyCollector(Vec<Value>);

    #[allow(dead_code)]
    #[derive(Deserialize, JsonSchema)]
    #[serde(deny_unknown_fields)]
    struct MutationContractInput {
        /// Bounded mutation property values.
        properties: MutationProperties,
        /// Closed mutation icon value.
        icon: MutationIcon,
    }

    impl SetProperty for PropertyCollector {
        fn add_property(mut self, property: Value) -> Self {
            self.0.push(property);
            self
        }
    }

    #[test]
    fn property_keys_are_bounded_ascii_and_exact() {
        assert_eq!(
            MutationPropertyKey::new("A_key-1").unwrap().as_str(),
            "A_key-1"
        );
        for invalid in ["", "with space", "../path", "café", "a/b"] {
            assert!(
                MutationPropertyKey::new(invalid).is_err(),
                "accepted {invalid:?}"
            );
        }
        assert!(MutationPropertyKey::new("a".repeat(MAX_TYPE_KEY_CHARS)).is_ok());
        assert!(MutationPropertyKey::new("a".repeat(MAX_TYPE_KEY_CHARS + 1)).is_err());
        let schema = serde_json::to_value(schema_for!(MutationPropertyKey)).unwrap();
        assert_eq!(schema["pattern"], "^[A-Za-z0-9_-]+$");
        assert_eq!(schema["minLength"], 1);
        assert_eq!(schema["maxLength"], MAX_TYPE_KEY_CHARS);
    }

    #[test]
    fn numbers_are_bounded_and_canonical() {
        for spelling in ["0", "-0.0", "0e10"] {
            let value: MutationNumber = serde_json::from_str(spelling).unwrap();
            assert_eq!(serde_json::to_value(value).unwrap(), json!(0));
        }
        for spelling in ["1", "1.0", "1e0"] {
            let value: MutationNumber = serde_json::from_str(spelling).unwrap();
            assert_eq!(serde_json::to_value(value).unwrap(), json!(1));
        }
        let decimal: MutationNumber = serde_json::from_str("1.25").unwrap();
        assert_eq!(serde_json::to_value(decimal).unwrap(), json!(1.25));
        for value in [-MAX_MUTATION_NUMBER_ABS, MAX_MUTATION_NUMBER_ABS] {
            assert!(MutationNumber::new(Number::from_f64(value).unwrap()).is_ok());
        }
        assert!(
            MutationNumber::new(Number::from_f64(MAX_MUTATION_NUMBER_ABS * 2.0).unwrap()).is_err()
        );
        let schema = serde_json::to_value(schema_for!(MutationNumber)).unwrap();
        assert_eq!(schema["minimum"], -MAX_MUTATION_NUMBER_ABS);
        assert_eq!(schema["maximum"], MAX_MUTATION_NUMBER_ABS);
    }

    #[test]
    fn dates_are_bounded_rfc3339_and_canonical_utc() {
        let utc = MutationDate::new("2026-07-20T10:00:00Z").unwrap();
        let offset = MutationDate::new("2026-07-20T12:00:00+02:00").unwrap();
        assert_eq!(utc, offset);
        assert_eq!(utc.as_str(), "2026-07-20T10:00:00Z");
        assert_eq!(
            MutationDate::new("2026-07-20T12:00:00.123400+02:00")
                .unwrap()
                .as_str(),
            "2026-07-20T10:00:00.123400Z"
        );
        assert!(MutationDate::new("not-a-date").is_err());
        assert!(MutationDate::new("x".repeat(MAX_MUTATION_DATE_CHARS + 1)).is_err());
        let schema = serde_json::to_value(schema_for!(MutationDate)).unwrap();
        assert_eq!(schema["format"], "date-time");
        assert_eq!(schema["maxLength"], MAX_MUTATION_DATE_CHARS);
    }

    #[test]
    fn ids_cap_raw_input_before_sorting_and_deduplication() {
        let normalized = MutationIds::new(vec![
            EntityId::new(ID_B).unwrap(),
            EntityId::new(ID_A).unwrap(),
            EntityId::new(ID_B).unwrap(),
        ])
        .unwrap();
        assert_eq!(normalized.as_slice(), ids(&[ID_A, ID_B]).as_slice());
        let oversized = vec![EntityId::new(ID_A).unwrap(); MAX_MUTATION_IDS + 1];
        assert_eq!(
            MutationIds::new(oversized),
            Err(MutationInputError::TooManyIds)
        );
        assert!(
            serde_json::from_value::<MutationIds>(json!(vec![ID_A; MAX_MUTATION_IDS + 1])).is_err()
        );
        let schema = serde_json::to_value(schema_for!(MutationIds)).unwrap();
        assert_eq!(schema["maxItems"], MAX_MUTATION_IDS);
        assert!(schema.to_string().contains(&format!(
            "\"maxLength\":{}",
            crate::domain::MAX_ENTITY_ID_CHARS
        )));
    }

    #[test]
    fn icon_forms_colors_and_bounds_are_closed() {
        let colors = [
            ("grey", Color::Grey),
            ("yellow", Color::Yellow),
            ("orange", Color::Orange),
            ("red", Color::Red),
            ("pink", Color::Pink),
            ("purple", Color::Purple),
            ("blue", Color::Blue),
            ("ice", Color::Ice),
            ("teal", Color::Teal),
            ("lime", Color::Lime),
        ];
        for (color, expected) in colors {
            let icon: MutationIcon = serde_json::from_value(json!({
                "format":"icon", "name":"check", "color":color
            }))
            .unwrap();
            assert_eq!(
                icon.to_anytype(),
                Icon::Icon {
                    name: "check".to_owned(),
                    color: expected,
                }
            );
        }
        let emoji: MutationIcon =
            serde_json::from_value(json!({"format":"emoji","emoji":"👨‍👩‍👧"})).unwrap();
        assert!(matches!(emoji.to_anytype(), Icon::Emoji { .. }));
        let file: MutationIcon =
            serde_json::from_value(json!({"format":"file","file":ID_A})).unwrap();
        assert!(matches!(file.to_anytype(), Icon::File { .. }));
        for invalid in [
            json!({"format":"emoji","emoji":""}),
            json!({"format":"file","file":"../unsafe"}),
            json!({"format":"icon","name":"check","color":"unknown"}),
            json!({"format":"icon","name":"check","color":"red","extra":true}),
            Value::Null,
        ] {
            assert!(serde_json::from_value::<MutationIcon>(invalid).is_err());
        }
        assert!(MutationIconText::new("x".repeat(MAX_MUTATION_ICON_TEXT_CHARS)).is_ok());
        assert!(MutationIconText::new("x".repeat(MAX_MUTATION_ICON_TEXT_CHARS + 1)).is_err());
        let schema = serde_json::to_value(schema_for!(MutationIcon)).unwrap();
        let encoded = schema.to_string();
        assert!(encoded.contains(&format!("\"maxLength\":{MAX_MUTATION_ICON_TEXT_CHARS}")));
        for color in [
            "grey", "yellow", "orange", "red", "pink", "purple", "blue", "ice", "teal", "lime",
        ] {
            assert!(encoded.contains(&format!("\"{color}\"")));
        }
    }

    #[test]
    fn property_union_has_all_strict_bounded_forms() {
        let values = [
            json!({"format":"text","key":"text","text":"hello"}),
            json!({"format":"number","key":"number","number":42}),
            json!({"format":"select","key":"select","select":ID_A}),
            json!({"format":"multi_select","key":"multi","multi_select":[ID_A]}),
            json!({"format":"date","key":"date","date":"2026-07-20T10:00:00Z"}),
            json!({"format":"files","key":"files","files":[ID_A]}),
            json!({"format":"checkbox","key":"done","checkbox":true}),
            json!({"format":"url","key":"url","url":"https://example.test"}),
            json!({"format":"email","key":"email","email":"a@example.test"}),
            json!({"format":"phone","key":"phone","phone":"+1"}),
            json!({"format":"objects","key":"objects","objects":[ID_A]}),
        ];
        for (value, format) in values.into_iter().zip([
            PropertyFormat::Text,
            PropertyFormat::Number,
            PropertyFormat::Select,
            PropertyFormat::MultiSelect,
            PropertyFormat::Date,
            PropertyFormat::Files,
            PropertyFormat::Checkbox,
            PropertyFormat::Url,
            PropertyFormat::Email,
            PropertyFormat::Phone,
            PropertyFormat::Objects,
        ]) {
            assert_eq!(property(value).format(), format);
        }
        for invalid in [
            json!({"format":"text","key":"text","text":null}),
            json!({"format":"text","key":"text","text":"x","extra":true}),
            json!({"format":"unknown","key":"text","text":"x"}),
            Value::Null,
        ] {
            assert!(serde_json::from_value::<MutationProperty>(invalid).is_err());
        }
        assert!(
            serde_json::from_value::<MutationProperty>(json!({
                "format":"text", "key":"text", "text":"x".repeat(MAX_MUTATION_TEXT_CHARS + 1)
            }))
            .is_err()
        );

        let schema = serde_json::to_value(schema_for!(MutationProperty)).unwrap();
        let encoded = schema.to_string();
        for format in [
            "text",
            "number",
            "select",
            "multi_select",
            "date",
            "files",
            "checkbox",
            "url",
            "email",
            "phone",
            "objects",
        ] {
            assert!(encoded.contains(&format!("\"{format}\"")));
        }
        assert!(encoded.contains(&format!("\"maxLength\":{MAX_MUTATION_TEXT_CHARS}")));
        assert!(!encoded.contains("additionalProperties\":true"));
        crate::schema::input_schema::<MutationContractInput>()
            .expect("embedded mutation values satisfy the strict MCP schema contract");
    }

    #[test]
    fn properties_are_bounded_sorted_and_duplicate_keys_reject() {
        let values = MutationProperties::new(vec![
            property(json!({"format":"text","key":"z","text":"last"})),
            property(json!({"format":"text","key":"a","text":"first"})),
        ])
        .unwrap();
        let normalized = normalized_properties(&values).unwrap();
        assert_eq!(normalized[0].key().as_str(), "a");
        assert_eq!(normalized[1].key().as_str(), "z");

        let duplicate = MutationProperties::new(vec![
            property(json!({"format":"text","key":"same","text":"one"})),
            property(json!({"format":"url","key":"same","url":"two"})),
        ])
        .unwrap();
        assert_eq!(
            normalized_properties(&duplicate),
            Err(MutationInputError::DuplicatePropertyKey)
        );
        assert!(
            MutationProperties::new(vec![
                property(json!({"format":"text","key":"x","text":"x"}));
                MAX_MUTATION_PROPERTIES + 1
            ])
            .is_err()
        );
        let schema = serde_json::to_value(schema_for!(MutationProperties)).unwrap();
        assert_eq!(schema["maxItems"], MAX_MUTATION_PROPERTIES);
    }

    #[test]
    fn generic_set_property_application_omits_format_and_preserves_canonical_values() {
        let properties = [
            property(json!({"format":"text","key":"text","text":"hello"})),
            property(json!({"format":"number","key":"number","number":1.0})),
            property(json!({"format":"select","key":"select","select":ID_A})),
            property(
                json!({"format":"multi_select","key":"multi","multi_select":[ID_B,ID_A,ID_B]}),
            ),
            property(json!({"format":"date","key":"date","date":"2026-07-20T12:00:00+02:00"})),
            property(json!({"format":"files","key":"files","files":[]})),
            property(json!({"format":"checkbox","key":"done","checkbox":false})),
            property(json!({"format":"url","key":"url","url":""})),
            property(json!({"format":"email","key":"email","email":"a@example.test"})),
            property(json!({"format":"phone","key":"phone","phone":"+1"})),
            property(json!({"format":"objects","key":"objects","objects":[ID_A]})),
        ];
        let collector = properties
            .iter()
            .fold(PropertyCollector::default(), |request, value| {
                value.apply(request)
            });
        assert_eq!(
            collector.0,
            vec![
                json!({"key":"text","text":"hello"}),
                json!({"key":"number","number":1}),
                json!({"key":"select","select":ID_A}),
                json!({"key":"multi","multi_select":[ID_A,ID_B]}),
                json!({"key":"date","date":"2026-07-20T10:00:00Z"}),
                json!({"key":"files","files":[]}),
                json!({"key":"done","checkbox":false}),
                json!({"key":"url","url":""}),
                json!({"key":"email","email":"a@example.test"}),
                json!({"key":"phone","phone":"+1"}),
                json!({"key":"objects","objects":[ID_A]}),
            ]
        );
        assert!(
            collector
                .0
                .iter()
                .all(|value| value.get("format").is_none())
        );
    }

    #[test]
    fn semantic_comparison_handles_numbers_dates_tags_and_sets() {
        let number = property(json!({"format":"number","key":"n","number":1}));
        assert!(
            number
                .matches_returned(Some(&PropertyValue::Number {
                    number: Number::from_f64(1.0).unwrap(),
                }))
                .unwrap()
        );

        let date = property(json!({
            "format":"date", "key":"d", "date":"2026-07-20T12:00:00+02:00"
        }));
        assert!(
            date.matches_returned(Some(&PropertyValue::Date {
                date: "2026-07-20T10:00:00Z".to_owned(),
            }))
            .unwrap()
        );

        let select = property(json!({"format":"select","key":"s","select":ID_A}));
        assert!(
            select
                .matches_returned(Some(&PropertyValue::Select {
                    select: anytype::tags::Tag {
                        object: DataModel::Tag,
                        id: ID_A.to_owned(),
                        name: "server name".to_owned(),
                        key: "server-key".to_owned(),
                        color: Color::Purple,
                    },
                }))
                .unwrap()
        );

        let multi = property(json!({
            "format":"multi_select", "key":"m", "multi_select":[ID_A,ID_B]
        }));
        let tag = |id: &str| anytype::tags::Tag {
            object: DataModel::Tag,
            id: id.to_owned(),
            name: "ignored".to_owned(),
            key: "ignored".to_owned(),
            color: Color::Grey,
        };
        assert!(
            multi
                .matches_returned(Some(&PropertyValue::MultiSelect {
                    multi_select: vec![tag(ID_B), tag(ID_A), tag(ID_B)],
                }))
                .unwrap()
        );

        for (expected, returned) in [
            (
                property(json!({"format":"files","key":"f","files":[ID_A,ID_B]})),
                PropertyValue::Files {
                    files: vec![ID_B.to_owned(), ID_A.to_owned()],
                },
            ),
            (
                property(json!({"format":"objects","key":"o","objects":[ID_A,ID_B]})),
                PropertyValue::Objects {
                    objects: vec![ID_B.to_owned(), ID_A.to_owned(), ID_A.to_owned()],
                },
            ),
        ] {
            assert!(expected.matches_returned(Some(&returned)).unwrap());
        }
    }

    #[test]
    fn scalar_comparison_is_exact_and_wrong_formats_mismatch() {
        let cases = [
            (
                property(json!({"format":"text","key":"k","text":"Exact"})),
                PropertyValue::Text {
                    text: "Exact".to_owned(),
                },
            ),
            (
                property(json!({"format":"url","key":"k","url":"https://e.test"})),
                PropertyValue::Url {
                    url: "https://e.test".to_owned(),
                },
            ),
            (
                property(json!({"format":"email","key":"k","email":"A@e.test"})),
                PropertyValue::Email {
                    email: "A@e.test".to_owned(),
                },
            ),
            (
                property(json!({"format":"phone","key":"k","phone":"+1 2"})),
                PropertyValue::Phone {
                    phone: "+1 2".to_owned(),
                },
            ),
            (
                property(json!({"format":"checkbox","key":"k","checkbox":false})),
                PropertyValue::Checkbox { checkbox: false },
            ),
        ];
        for (expected, returned) in cases {
            assert!(expected.matches_returned(Some(&returned)).unwrap());
        }
        let text = property(json!({"format":"text","key":"k","text":"Exact"}));
        assert!(
            !text
                .matches_returned(Some(&PropertyValue::Text {
                    text: "exact".to_owned(),
                }))
                .unwrap()
        );
        assert!(
            !text
                .matches_returned(Some(&PropertyValue::Url {
                    url: "Exact".to_owned(),
                }))
                .unwrap()
        );
    }

    #[test]
    fn only_documented_empty_values_compare_as_missing_clears() {
        for value in [
            json!({"format":"text","key":"k","text":""}),
            json!({"format":"multi_select","key":"k","multi_select":[]}),
            json!({"format":"files","key":"k","files":[]}),
            json!({"format":"url","key":"k","url":""}),
            json!({"format":"email","key":"k","email":""}),
            json!({"format":"phone","key":"k","phone":""}),
            json!({"format":"objects","key":"k","objects":[]}),
        ] {
            let value = property(value);
            assert!(value.is_clear());
            assert!(value.matches_returned(None).unwrap());
        }
        for value in [
            json!({"format":"number","key":"k","number":0}),
            json!({"format":"select","key":"k","select":ID_A}),
            json!({"format":"date","key":"k","date":"2026-07-20T10:00:00Z"}),
            json!({"format":"checkbox","key":"k","checkbox":false}),
        ] {
            let value = property(value);
            assert!(!value.is_clear());
            assert!(!value.matches_returned(None).unwrap());
        }
    }

    #[test]
    fn upstream_null_arrays_decode_and_compare_as_empty_clears() {
        for (expected, returned) in [
            (
                property(json!({"format":"multi_select","key":"k","multi_select":[]})),
                serde_json::from_value::<PropertyValue>(json!({
                    "format":"multi_select", "multi_select":null
                }))
                .unwrap(),
            ),
            (
                property(json!({"format":"files","key":"k","files":[]})),
                serde_json::from_value::<PropertyValue>(json!({"format":"files","files":null}))
                    .unwrap(),
            ),
            (
                property(json!({"format":"objects","key":"k","objects":[]})),
                serde_json::from_value::<PropertyValue>(json!({
                    "format":"objects", "objects":null
                }))
                .unwrap(),
            ),
        ] {
            assert!(expected.matches_returned(Some(&returned)).unwrap());
        }
    }

    #[test]
    fn comparison_classifies_bounded_and_malformed_upstream_values() {
        let text = property(json!({"format":"text","key":"k","text":"x"}));
        assert_eq!(
            text.matches_returned(Some(&PropertyValue::Text {
                text: "x".repeat(MAX_MUTATION_TEXT_CHARS + 1),
            })),
            Err(MutationCompareError::Bounded)
        );
        let date = property(json!({
            "format":"date","key":"k","date":"2026-07-20T10:00:00Z"
        }));
        assert_eq!(
            date.matches_returned(Some(&PropertyValue::Date {
                date: "not-a-date".to_owned(),
            })),
            Err(MutationCompareError::Malformed)
        );
        assert_eq!(
            date.matches_returned(Some(&PropertyValue::Date {
                date: "x".repeat(MAX_MUTATION_DATE_CHARS + 1),
            })),
            Err(MutationCompareError::Bounded)
        );
        let number = property(json!({"format":"number","key":"k","number":1}));
        assert_eq!(
            number.matches_returned(Some(&PropertyValue::Number {
                number: Number::from_f64(MAX_MUTATION_NUMBER_ABS * 2.0).unwrap(),
            })),
            Err(MutationCompareError::Bounded)
        );
        let files = property(json!({"format":"files","key":"k","files":[]}));
        assert_eq!(
            files.matches_returned(Some(&PropertyValue::Files {
                files: vec![ID_A.to_owned(); MAX_MUTATION_IDS + 1],
            })),
            Err(MutationCompareError::Bounded)
        );
        assert_eq!(
            files.matches_returned(Some(&PropertyValue::Files {
                files: vec!["../unsafe".to_owned()],
            })),
            Err(MutationCompareError::Malformed)
        );
        let select = property(json!({"format":"select","key":"k","select":ID_A}));
        let malformed_tag = anytype::tags::Tag {
            object: DataModel::Tag,
            id: "../unsafe".to_owned(),
            name: "name".to_owned(),
            key: "key".to_owned(),
            color: Color::Blue,
        };
        assert_eq!(
            select.matches_returned(Some(&PropertyValue::Select {
                select: malformed_tag,
            })),
            Err(MutationCompareError::Malformed)
        );
    }

    #[test]
    fn icon_comparison_is_exact_bounded_and_has_no_clear() {
        let expected: MutationIcon = serde_json::from_value(json!({
            "format":"icon", "name":"check", "color":"teal"
        }))
        .unwrap();
        assert!(
            expected
                .matches_returned(Some(&Icon::Icon {
                    name: "check".to_owned(),
                    color: Color::Teal,
                }))
                .unwrap()
        );
        assert!(!expected.matches_returned(None).unwrap());
        assert!(
            !expected
                .matches_returned(Some(&Icon::Icon {
                    name: "Check".to_owned(),
                    color: Color::Teal,
                }))
                .unwrap()
        );
        assert_eq!(
            expected.matches_returned(Some(&Icon::Icon {
                name: "x".repeat(MAX_MUTATION_ICON_TEXT_CHARS + 1),
                color: Color::Teal,
            })),
            Err(MutationCompareError::Bounded)
        );
        assert_eq!(
            expected.matches_returned(Some(&Icon::Icon {
                name: String::new(),
                color: Color::Teal,
            })),
            Err(MutationCompareError::Malformed)
        );
        let file: MutationIcon =
            serde_json::from_value(json!({"format":"file","file":ID_A})).unwrap();
        assert_eq!(
            file.matches_returned(Some(&Icon::File {
                file: "../unsafe".to_owned(),
            })),
            Err(MutationCompareError::Malformed)
        );
    }

    #[test]
    fn normalized_values_serialize_deterministically_for_future_fingerprints() {
        let left = MutationProperties::new(vec![
            property(json!({
                "format":"date","key":"date","date":"2026-07-20T12:00:00+02:00"
            })),
            property(json!({
                "format":"multi_select","key":"ids","multi_select":[ID_B,ID_A,ID_B]
            })),
            property(json!({"format":"number","key":"number","number":1.0})),
        ])
        .unwrap();
        let right = MutationProperties::new(vec![
            property(json!({"format":"number","key":"number","number":1})),
            property(json!({
                "format":"multi_select","key":"ids","multi_select":[ID_A,ID_B]
            })),
            property(json!({
                "format":"date","key":"date","date":"2026-07-20T10:00:00Z"
            })),
        ])
        .unwrap();
        let left = serde_json::to_vec(&normalized_properties(&left).unwrap()).unwrap();
        let right = serde_json::to_vec(&normalized_properties(&right).unwrap()).unwrap();
        assert_eq!(left, right);
        assert_eq!(
            String::from_utf8(left).unwrap(),
            format!(
                "[{{\"format\":\"date\",\"key\":\"date\",\"date\":\"2026-07-20T10:00:00Z\"}},{{\"format\":\"multi_select\",\"key\":\"ids\",\"multi_select\":[\"{ID_A}\",\"{ID_B}\"]}},{{\"format\":\"number\",\"key\":\"number\",\"number\":1}}]"
            )
        );
    }
}
