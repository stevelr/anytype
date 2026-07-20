// any-mcp - bounded, workflow-oriented MCP server for Anytype
//
// SPDX-FileCopyrightText: 2026 Steve Schoettler
// SPDX-License-Identifier: Apache-2.0

//! Conflict-aware, whole-object update workflow.
//!
//! `object_update` preserves every omitted field. A supplied body is a complete
//! markdown replacement, including the empty string (the documented body-clear
//! form). Supplying `expected_body_sha256` checks the complete current body
//! before the single update request. Anytype does not expose an atomic
//! compare-and-swap operation, so a best-effort race remains between that read
//! and the update.

use std::{borrow::Cow, collections::HashSet, fmt};

use anytype::{
    objects::{Color, Icon, Object},
    prelude::SetProperty,
    properties::PropertyValue,
};
use rmcp::{
    model::CallToolResult,
    schemars::{JsonSchema, Schema, SchemaGenerator, json_schema},
};
use serde::{Deserialize, Deserializer, Serialize, de};
use serde_json::Number;
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;

use crate::{
    domain::{BoundedText, DomainValueError, EntityId, ObjectId, ObjectSummary, SpaceId, TypeKey},
    error::ToolError,
    handler_support::{
        HandlerError, HandlerOperationError, MutationAccess, execute_prepared_handler,
        require_mutation_access,
    },
    object_output::object_summary,
    object_read::AnytypeReference,
    protocol::{ToolProfile, WorkflowTool, workflow_tool},
    result::tool_error,
    runtime::{OperationContext, RuntimeContext},
    schema::SchemaContractError,
    validation::{BoundedList, Omittable, optional_non_null_schema},
};

/// Maximum UTF-8 bytes accepted in a replacement markdown body.
pub const MAX_UPDATE_MARKDOWN_BYTES: usize = 10 * 1024 * 1024;
/// Maximum Unicode scalar values accepted in a replacement markdown body.
pub const MAX_UPDATE_MARKDOWN_CHARS: usize = 100_000;
/// Maximum characters accepted in a nonempty object name.
pub const MAX_UPDATE_NAME_CHARS: usize = 512;
/// Maximum property mutations in one update.
pub const MAX_UPDATE_PROPERTIES: usize = 50;
/// Maximum characters in a scalar property value or icon text.
pub const MAX_UPDATE_TEXT_CHARS: usize = 4_096;
/// Maximum identifiers in one files, objects, or multi-select value.
pub const MAX_UPDATE_VALUE_ITEMS: usize = 100;
/// Maximum absolute finite property number.
pub const MAX_UPDATE_NUMBER_ABS: f64 = 1_000_000_000_000_000.0;
/// Maximum serialized characters in one property number.
pub const MAX_UPDATE_NUMBER_CHARS: usize = 128;

type UpdateText = BoundedText<MAX_UPDATE_TEXT_CHARS>;
type UpdateIds = BoundedList<EntityId, MAX_UPDATE_VALUE_ITEMS>;
type UpdateProperties = BoundedList<UpdateProperty, MAX_UPDATE_PROPERTIES>;

/// A nonempty bounded replacement name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct UpdateName(BoundedText<MAX_UPDATE_NAME_CHARS>);

impl UpdateName {
    /// Validates a replacement name. Names cannot be cleared because the
    /// upstream API rejects an empty name and exposes no distinct clear form.
    pub fn new(value: impl Into<String>) -> Result<Self, UpdateInputError> {
        let value = value.into();
        if value.is_empty() {
            return Err(UpdateInputError::EmptyName);
        }
        BoundedText::new(value)
            .map(Self)
            .map_err(|_| UpdateInputError::BoundedValue)
    }

    /// Borrows the validated name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl<'de> Deserialize<'de> for UpdateName {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::new(String::deserialize(deserializer)?).map_err(de::Error::custom)
    }
}

impl JsonSchema for UpdateName {
    fn schema_name() -> Cow<'static, str> {
        "UpdateName".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "minLength": 1,
            "maxLength": MAX_UPDATE_NAME_CHARS,
        })
    }
}

/// A replacement markdown body bounded by UTF-8 bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct UpdateMarkdown(String);

impl UpdateMarkdown {
    /// Validates a complete replacement body. The empty string explicitly
    /// clears the body.
    pub fn new(value: impl Into<String>) -> Result<Self, UpdateInputError> {
        let value = value.into();
        if value.len() > MAX_UPDATE_MARKDOWN_BYTES
            || value.chars().count() > MAX_UPDATE_MARKDOWN_CHARS
        {
            return Err(UpdateInputError::BoundedValue);
        }
        Ok(Self(value))
    }

    /// Borrows the complete replacement body.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for UpdateMarkdown {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::new(String::deserialize(deserializer)?).map_err(de::Error::custom)
    }
}

impl JsonSchema for UpdateMarkdown {
    fn schema_name() -> Cow<'static, str> {
        "UpdateMarkdown".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "maxLength": MAX_UPDATE_MARKDOWN_CHARS,
        })
    }
}

/// A canonical lowercase SHA-256 digest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct BodySha256(String);

impl BodySha256 {
    /// Validates a 64-character lowercase hexadecimal digest.
    pub fn new(value: impl Into<String>) -> Result<Self, UpdateInputError> {
        let value = value.into();
        if value.len() != 64
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
        {
            return Err(UpdateInputError::InvalidHash);
        }
        Ok(Self(value))
    }

    /// Hashes a complete markdown body.
    #[must_use]
    pub fn digest(body: &str) -> Self {
        let digest = Sha256::digest(body.as_bytes());
        Self(digest.iter().map(|byte| format!("{byte:02x}")).collect())
    }

    /// Borrows the lowercase digest.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for BodySha256 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::new(String::deserialize(deserializer)?).map_err(de::Error::custom)
    }
}

impl JsonSchema for BodySha256 {
    fn schema_name() -> Cow<'static, str> {
        "BodySha256".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "minLength": 64,
            "maxLength": 64,
            "pattern": "^[0-9a-f]{64}$",
        })
    }
}

/// A finite, practically bounded JSON number used in a property mutation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct UpdateNumber(Number);

impl UpdateNumber {
    /// Validates a property number without losing integer precision.
    pub fn new(number: Number) -> Result<Self, UpdateInputError> {
        let rendered = number.to_string();
        let Some(value) = number.as_f64() else {
            return Err(UpdateInputError::BoundedValue);
        };
        if rendered.len() > MAX_UPDATE_NUMBER_CHARS
            || !value.is_finite()
            || value.abs() > MAX_UPDATE_NUMBER_ABS
        {
            return Err(UpdateInputError::BoundedValue);
        }
        Ok(Self(number))
    }
}

impl<'de> Deserialize<'de> for UpdateNumber {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::new(Number::deserialize(deserializer)?).map_err(de::Error::custom)
    }
}

impl JsonSchema for UpdateNumber {
    fn schema_name() -> Cow<'static, str> {
        "UpdateNumber".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "number",
            "minimum": -MAX_UPDATE_NUMBER_ABS,
            "maximum": MAX_UPDATE_NUMBER_ABS,
        })
    }
}

/// A bounded RFC 3339 date property value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct UpdateDate(BoundedText<64>);

impl UpdateDate {
    /// Validates an RFC 3339 property date.
    pub fn new(value: impl Into<String>) -> Result<Self, UpdateInputError> {
        let value = value.into();
        chrono::DateTime::parse_from_rfc3339(&value).map_err(|_| UpdateInputError::InvalidDate)?;
        BoundedText::new(value)
            .map(Self)
            .map_err(|_| UpdateInputError::BoundedValue)
    }

    fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl<'de> Deserialize<'de> for UpdateDate {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::new(String::deserialize(deserializer)?).map_err(de::Error::custom)
    }
}

impl JsonSchema for UpdateDate {
    fn schema_name() -> Cow<'static, str> {
        "UpdateDate".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({"type":"string","minLength":1,"maxLength":64,"format":"date-time"})
    }
}

/// Closed Anytype color values for colored icons.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum UpdateColor {
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

/// Closed, bounded object icon mutation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "format", rename_all = "snake_case", deny_unknown_fields)]
pub enum UpdateIcon {
    /// Emoji icon text.
    Emoji {
        /// Bounded nonempty emoji representation.
        emoji: UpdateText,
    },
    /// Existing Anytype file object identifier.
    File {
        /// Safe file identifier; arbitrary host paths are never accepted.
        file: EntityId,
    },
    /// Named built-in icon with a closed color.
    Icon {
        /// Bounded nonempty icon name.
        name: UpdateText,
        /// Closed Anytype color.
        color: UpdateColor,
    },
}

/// One closed typed property mutation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "format", rename_all = "snake_case", deny_unknown_fields)]
pub enum UpdateProperty {
    /// Text value; an empty value clears the text property.
    Text {
        /// Stable property key.
        key: TypeKey,
        /// Replacement text.
        text: UpdateText,
    },
    /// Finite number value.
    Number {
        /// Stable property key.
        key: TypeKey,
        /// Replacement number.
        number: UpdateNumber,
    },
    /// One existing select-option identifier.
    Select {
        /// Stable property key.
        key: TypeKey,
        /// Selected tag identifier.
        select: EntityId,
    },
    /// Existing select-option identifiers; an empty list clears the property.
    MultiSelect {
        /// Stable property key.
        key: TypeKey,
        /// Selected tag identifiers.
        multi_select: UpdateIds,
    },
    /// RFC 3339 date value.
    Date {
        /// Stable property key.
        key: TypeKey,
        /// Replacement date.
        date: UpdateDate,
    },
    /// Existing file identifiers; an empty list clears the property.
    Files {
        /// Stable property key.
        key: TypeKey,
        /// Replacement file identifiers.
        files: UpdateIds,
    },
    /// Checkbox state.
    Checkbox {
        /// Stable property key.
        key: TypeKey,
        /// Replacement checkbox state.
        checkbox: bool,
    },
    /// URL text; an empty value clears the property.
    Url {
        /// Stable property key.
        key: TypeKey,
        /// Replacement URL.
        url: UpdateText,
    },
    /// Email text; an empty value clears the property.
    Email {
        /// Stable property key.
        key: TypeKey,
        /// Replacement email.
        email: UpdateText,
    },
    /// Phone text; an empty value clears the property.
    Phone {
        /// Stable property key.
        key: TypeKey,
        /// Replacement phone number.
        phone: UpdateText,
    },
    /// Existing object identifiers; an empty list clears the relation.
    Objects {
        /// Stable property key.
        key: TypeKey,
        /// Replacement object identifiers.
        objects: UpdateIds,
    },
}

impl UpdateProperty {
    fn key(&self) -> &TypeKey {
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
}

/// Strict update input. Every optional field rejects explicit JSON `null`.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ObjectUpdateInput {
    /// Unique space name or safe id.
    space: AnytypeReference,
    /// Stable object id; names are never guessed.
    object_id: ObjectId,
    /// Replacement nonempty name. Omit to preserve the current name.
    #[serde(default)]
    #[schemars(schema_with = "optional_update_name_schema")]
    name: Omittable<UpdateName>,
    /// Complete markdown replacement. Empty string clears; omit to preserve.
    #[serde(default)]
    #[schemars(schema_with = "optional_markdown_schema")]
    body_markdown: Omittable<UpdateMarkdown>,
    /// Current complete-body SHA-256 precondition. It may guard any mutation.
    #[serde(default)]
    #[schemars(schema_with = "optional_hash_schema")]
    expected_body_sha256: Omittable<BodySha256>,
    /// Typed property replacements. An empty array alone is not a mutation.
    #[serde(default)]
    #[schemars(schema_with = "optional_properties_schema")]
    properties: Omittable<UpdateProperties>,
    /// Type key, name, or id to resolve. Omit to preserve the current type.
    #[serde(default)]
    #[schemars(schema_with = "optional_reference_schema")]
    r#type: Omittable<AnytypeReference>,
    /// Complete icon replacement. Omit to preserve; clearing is unsupported.
    #[serde(default)]
    #[schemars(schema_with = "optional_icon_schema")]
    icon: Omittable<UpdateIcon>,
}

fn optional_update_name_schema(generator: &mut SchemaGenerator) -> Schema {
    optional_non_null_schema::<UpdateName>(generator)
}
fn optional_markdown_schema(generator: &mut SchemaGenerator) -> Schema {
    optional_non_null_schema::<UpdateMarkdown>(generator)
}
fn optional_hash_schema(generator: &mut SchemaGenerator) -> Schema {
    optional_non_null_schema::<BodySha256>(generator)
}
fn optional_properties_schema(generator: &mut SchemaGenerator) -> Schema {
    optional_non_null_schema::<UpdateProperties>(generator)
}
fn optional_reference_schema(generator: &mut SchemaGenerator) -> Schema {
    optional_non_null_schema::<AnytypeReference>(generator)
}
fn optional_icon_schema(generator: &mut SchemaGenerator) -> Schema {
    optional_non_null_schema::<UpdateIcon>(generator)
}

impl ObjectUpdateInput {
    fn has_mutation(&self) -> bool {
        !self.name.is_none()
            || !self.body_markdown.is_none()
            || !self.r#type.is_none()
            || !self.icon.is_none()
            || self
                .properties
                .as_ref()
                .is_some_and(|properties| !properties.as_slice().is_empty())
    }
}

/// Verified result of one object update.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ObjectUpdateOutput {
    /// Bounded read-after-write object summary and canonical resource link.
    object: ObjectSummary,
    /// Complete verified body hash when a body or hash precondition was supplied.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(default, schema_with = "optional_output_hash_schema")]
    body_sha256: Option<BodySha256>,
}

fn optional_output_hash_schema(generator: &mut SchemaGenerator) -> Schema {
    generator.subschema_for::<BodySha256>()
}

impl ObjectUpdateOutput {
    /// Borrows the verified updated summary.
    #[must_use]
    pub const fn object(&self) -> &ObjectSummary {
        &self.object
    }

    /// Borrows the verified complete body hash when relevant.
    #[must_use]
    pub const fn body_sha256(&self) -> Option<&BodySha256> {
        self.body_sha256.as_ref()
    }
}

/// Invalid typed update input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateInputError {
    /// A replacement name was empty.
    EmptyName,
    /// A bounded wire value exceeded its limit.
    BoundedValue,
    /// A body hash was not canonical lowercase SHA-256.
    InvalidHash,
    /// A date was not valid RFC 3339.
    InvalidDate,
}

impl fmt::Display for UpdateInputError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::EmptyName => "replacement name must not be empty",
            Self::BoundedValue => "update value exceeds its documented bound",
            Self::InvalidHash => "body hash must be 64 lowercase hexadecimal characters",
            Self::InvalidDate => "date property must be RFC 3339",
        })
    }
}

impl std::error::Error for UpdateInputError {}

/// Builds the strict destructive `object_update` contract.
pub fn object_update_tool() -> Result<WorkflowTool<ObjectUpdateOutput>, SchemaContractError> {
    workflow_tool::<ObjectUpdateInput, ObjectUpdateOutput>(
        "object_update",
        "Replace only supplied object fields and verify the result. body_markdown replaces the whole body; without expected_body_sha256 it can overwrite a concurrent edit. Empty body_markdown clears the body. Omitted fields stay unchanged.",
        ToolProfile::Update,
    )
}

struct UpdateExecution {
    object: Object,
    body_sha256: Option<BodySha256>,
}

/// Applies exactly one update and verifies it with an explicit read-after-write.
///
/// The mutation-access gate and structural preflight run before resolver or
/// upstream I/O. When supplied, `expected_body_sha256` is checked against a
/// complete document-cap-bounded GET before the PATCH. A mismatch returns the
/// stable conflict error without updating. Because Anytype has no atomic CAS,
/// another writer can still race between that GET and PATCH. A body replacement
/// without the hash is allowed deliberately and may overwrite concurrent edits.
pub async fn object_update(
    runtime: &RuntimeContext,
    contract: &WorkflowTool<ObjectUpdateOutput>,
    access: MutationAccess,
    input: &ObjectUpdateInput,
    cancellation: &CancellationToken,
) -> CallToolResult {
    if let Err(error) = require_mutation_access(access) {
        return tool_error(error.tool_error());
    }
    if let Err(error) = preflight(input) {
        return tool_error(error.tool_error());
    }

    let client = runtime.client();
    let input = input.clone();
    execute_prepared_handler(
        runtime,
        contract,
        OperationContext::new("object_update"),
        cancellation,
        async move {
            let resolved_space = client.resolve_space_id(input.space.as_str()).await?;
            let space_id = checked_space_id(resolved_space)?;
            let object_id = input.object_id.clone();
            let resolved_type = match input.r#type.as_ref() {
                Some(reference) => {
                    let key = client
                        .resolve_type_key(space_id.as_str(), reference.as_str())
                        .await?;
                    Some(checked_type_key(key)?)
                }
                None => None,
            };

            if let Some(expected) = input.expected_body_sha256.as_ref() {
                let current = client
                    .object(space_id.as_str(), object_id.as_str())
                    .get()
                    .await?;
                verify_identity(&current, &space_id, &object_id)?;
                let current_hash = BodySha256::digest(current.markdown.as_deref().unwrap_or(""));
                if &current_hash != expected {
                    return Err(HandlerError::new(ToolError::conflict()).into());
                }
            }

            let mut request = client
                .update_object(space_id.as_str(), object_id.as_str())
                .no_verify();
            if let Some(name) = input.name.as_ref() {
                request = request.name(name.as_str());
            }
            if let Some(body) = input.body_markdown.as_ref() {
                request = request.body(body.as_str());
            }
            if let Some(key) = resolved_type.as_ref() {
                request = request.type_key(key.as_str());
            }
            if let Some(icon) = input.icon.as_ref() {
                request = request.icon(api_icon(icon));
            }
            if let Some(properties) = input.properties.as_ref() {
                for property in properties.as_slice() {
                    request = apply_property(request, property);
                }
            }

            let returned = request.update().await?;
            verify_identity(&returned, &space_id, &object_id)?;

            let verified = client
                .object(space_id.as_str(), object_id.as_str())
                .get()
                .await?;
            verify_identity(&verified, &space_id, &object_id)?;
            let hash = verify_requested_state(&verified, &input, resolved_type.as_ref())?;
            Ok::<_, HandlerOperationError>(UpdateExecution {
                object: verified,
                body_sha256: hash,
            })
        },
        |execution| async move {
            let object = object_summary(&execution.object).map_err(HandlerError::from)?;
            Ok(ObjectUpdateOutput {
                object,
                body_sha256: execution.body_sha256,
            })
        },
    )
    .await
}

fn preflight(input: &ObjectUpdateInput) -> Result<(), HandlerError> {
    if !input.has_mutation() {
        return Err(HandlerError::new(ToolError::validation()));
    }
    if let Some(reference) = input.r#type.as_ref()
        && let Some(explicit) = reference.as_str().strip_prefix('@')
    {
        TypeKey::new(explicit).map_err(|_| HandlerError::new(ToolError::validation()))?;
    }
    if let Some(icon) = input.icon.as_ref() {
        match icon {
            UpdateIcon::Emoji { emoji } if emoji.as_str().is_empty() => {
                return Err(HandlerError::new(ToolError::validation()));
            }
            UpdateIcon::Icon { name, .. } if name.as_str().is_empty() => {
                return Err(HandlerError::new(ToolError::validation()));
            }
            _ => {}
        }
    }
    if let Some(properties) = input.properties.as_ref() {
        let mut keys = HashSet::with_capacity(properties.as_slice().len());
        for property in properties.as_slice() {
            if !keys.insert(property.key().as_str()) {
                return Err(HandlerError::new(ToolError::validation()));
            }
        }
    }
    Ok(())
}

fn checked_space_id(value: String) -> Result<SpaceId, HandlerError> {
    SpaceId::new(value).map_err(upstream_domain)
}

fn checked_type_key(value: String) -> Result<TypeKey, HandlerError> {
    TypeKey::new(value).map_err(upstream_domain)
}

fn upstream_domain(error: DomainValueError) -> HandlerError {
    match error {
        DomainValueError::TooLong { .. } => HandlerError::new(ToolError::bounded_result()),
        DomainValueError::Empty | DomainValueError::InvalidIdentifierCharacter => {
            HandlerError::new(ToolError::upstream())
        }
    }
}

fn verify_identity(
    object: &Object,
    space_id: &SpaceId,
    object_id: &ObjectId,
) -> Result<(), HandlerError> {
    let returned_id = ObjectId::new(object.id.clone()).map_err(upstream_domain)?;
    let returned_space = SpaceId::new(object.space_id.clone()).map_err(upstream_domain)?;
    if &returned_id != object_id || &returned_space != space_id {
        return Err(HandlerError::new(ToolError::upstream()));
    }
    Ok(())
}

fn verify_requested_state(
    object: &Object,
    input: &ObjectUpdateInput,
    resolved_type: Option<&TypeKey>,
) -> Result<Option<BodySha256>, HandlerError> {
    if let Some(name) = input.name.as_ref()
        && object.name.as_deref() != Some(name.as_str())
    {
        return Err(HandlerError::new(ToolError::upstream()));
    }
    if let Some(expected_type) = resolved_type {
        let returned = object
            .r#type
            .as_ref()
            .ok_or_else(|| HandlerError::new(ToolError::upstream()))?;
        let returned = TypeKey::new(returned.key.clone()).map_err(upstream_domain)?;
        if &returned != expected_type {
            return Err(HandlerError::new(ToolError::upstream()));
        }
    }
    if let Some(icon) = input.icon.as_ref()
        && !icon_matches(object.icon.as_ref(), icon)?
    {
        return Err(HandlerError::new(ToolError::upstream()));
    }
    if let Some(properties) = input.properties.as_ref() {
        for expected in properties.as_slice() {
            let mut matching = object
                .properties
                .iter()
                .filter(|property| property.key == expected.key().as_str());
            let returned = matching
                .next()
                .ok_or_else(|| HandlerError::new(ToolError::upstream()))?;
            if matching.next().is_some() || !property_matches(&returned.value, expected)? {
                return Err(HandlerError::new(ToolError::upstream()));
            }
        }
    }

    let relevant =
        input.body_markdown.as_ref().is_some() || input.expected_body_sha256.as_ref().is_some();
    if !relevant {
        return Ok(None);
    }
    let actual = BodySha256::digest(object.markdown.as_deref().unwrap_or(""));
    let expected = if let Some(body) = input.body_markdown.as_ref() {
        BodySha256::digest(body.as_str())
    } else if let Some(expected) = input.expected_body_sha256.as_ref() {
        expected.clone()
    } else {
        return Ok(None);
    };
    if actual != expected {
        return Err(HandlerError::new(ToolError::conflict()));
    }
    Ok(Some(actual))
}

fn api_icon(icon: &UpdateIcon) -> Icon {
    match icon {
        UpdateIcon::Emoji { emoji } => Icon::Emoji {
            emoji: emoji.as_str().to_owned(),
        },
        UpdateIcon::File { file } => Icon::File {
            file: file.as_str().to_owned(),
        },
        UpdateIcon::Icon { name, color } => Icon::Icon {
            name: name.as_str().to_owned(),
            color: api_color(*color),
        },
    }
}

const fn api_color(color: UpdateColor) -> Color {
    match color {
        UpdateColor::Grey => Color::Grey,
        UpdateColor::Yellow => Color::Yellow,
        UpdateColor::Orange => Color::Orange,
        UpdateColor::Red => Color::Red,
        UpdateColor::Pink => Color::Pink,
        UpdateColor::Purple => Color::Purple,
        UpdateColor::Blue => Color::Blue,
        UpdateColor::Ice => Color::Ice,
        UpdateColor::Teal => Color::Teal,
        UpdateColor::Lime => Color::Lime,
    }
}

fn icon_matches(returned: Option<&Icon>, expected: &UpdateIcon) -> Result<bool, HandlerError> {
    Ok(match (returned, expected) {
        (Some(Icon::Emoji { emoji: returned }), UpdateIcon::Emoji { emoji }) => {
            returned == emoji.as_str()
        }
        (Some(Icon::File { file: returned }), UpdateIcon::File { file }) => {
            EntityId::new(returned.clone()).map_err(upstream_domain)? == *file
        }
        (
            Some(Icon::Icon {
                name: returned_name,
                color: returned_color,
            }),
            UpdateIcon::Icon { name, color },
        ) => returned_name == name.as_str() && returned_color == &api_color(*color),
        _ => false,
    })
}

fn apply_property(
    request: anytype::objects::UpdateObjectRequest,
    property: &UpdateProperty,
) -> anytype::objects::UpdateObjectRequest {
    match property {
        UpdateProperty::Text { key, text } => request.set_text(key.as_str(), text.as_str()),
        UpdateProperty::Number { key, number } => {
            request.set_number(key.as_str(), number.0.clone())
        }
        UpdateProperty::Select { key, select } => request.set_select(key.as_str(), select.as_str()),
        UpdateProperty::MultiSelect { key, multi_select } => request.set_multi_select(
            key.as_str(),
            multi_select.as_slice().iter().map(EntityId::as_str),
        ),
        UpdateProperty::Date { key, date } => request.set_date(key.as_str(), date.as_str()),
        UpdateProperty::Files { key, files } => {
            request.set_files(key.as_str(), files.as_slice().iter().map(EntityId::as_str))
        }
        UpdateProperty::Checkbox { key, checkbox } => request.set_checkbox(key.as_str(), *checkbox),
        UpdateProperty::Url { key, url } => request.set_url(key.as_str(), url.as_str()),
        UpdateProperty::Email { key, email } => request.set_email(key.as_str(), email.as_str()),
        UpdateProperty::Phone { key, phone } => request.set_phone(key.as_str(), phone.as_str()),
        UpdateProperty::Objects { key, objects } => request.set_objects(
            key.as_str(),
            objects.as_slice().iter().map(EntityId::as_str),
        ),
    }
}

fn property_matches(
    returned: &PropertyValue,
    expected: &UpdateProperty,
) -> Result<bool, HandlerError> {
    Ok(match (returned, expected) {
        (PropertyValue::Text { text: returned }, UpdateProperty::Text { text, .. }) => {
            returned == text.as_str()
        }
        (PropertyValue::Number { number: returned }, UpdateProperty::Number { number, .. }) => {
            returned == &number.0
        }
        (PropertyValue::Select { select: returned }, UpdateProperty::Select { select, .. }) => {
            EntityId::new(returned.id.clone()).map_err(upstream_domain)? == *select
        }
        (
            PropertyValue::MultiSelect {
                multi_select: returned,
            },
            UpdateProperty::MultiSelect {
                multi_select: expected,
                ..
            },
        ) => checked_tag_ids(returned)? == expected.as_slice(),
        (PropertyValue::Date { date: returned }, UpdateProperty::Date { date, .. }) => {
            returned == date.as_str()
        }
        (PropertyValue::Files { files: returned }, UpdateProperty::Files { files, .. }) => {
            checked_ids(returned)? == files.as_slice()
        }
        (
            PropertyValue::Checkbox { checkbox: returned },
            UpdateProperty::Checkbox {
                checkbox: expected, ..
            },
        ) => returned == expected,
        (PropertyValue::Url { url: returned }, UpdateProperty::Url { url, .. }) => {
            returned == url.as_str()
        }
        (PropertyValue::Email { email: returned }, UpdateProperty::Email { email, .. }) => {
            returned == email.as_str()
        }
        (PropertyValue::Phone { phone: returned }, UpdateProperty::Phone { phone, .. }) => {
            returned == phone.as_str()
        }
        (
            PropertyValue::Objects { objects: returned },
            UpdateProperty::Objects {
                objects: expected, ..
            },
        ) => checked_ids(returned)? == expected.as_slice(),
        _ => false,
    })
}

fn checked_ids(values: &[String]) -> Result<Vec<EntityId>, HandlerError> {
    values
        .iter()
        .cloned()
        .map(|value| EntityId::new(value).map_err(upstream_domain))
        .collect()
}

fn checked_tag_ids(values: &[anytype::tags::Tag]) -> Result<Vec<EntityId>, HandlerError> {
    values
        .iter()
        .map(|value| EntityId::new(value.id.clone()).map_err(upstream_domain))
        .collect()
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use anytype::prelude::{AnytypeClient, ClientConfig, HttpCredentials, ResponseLimits};
    use serde_json::{Value, json};
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::{TcpListener, TcpStream},
        task::JoinHandle,
    };

    use super::*;
    use crate::runtime::StartupStatus;

    const SPACE_ID: &str =
        "bafyreid5fvqlnsobih2keakcxjrrlpmly6kf37klzjzen4ibfdgalcdp4y.2tq5w93cr6oe7";
    const OTHER_SPACE_ID: &str =
        "bafyreid5fvqlnsobih2keakcxjrrlpmly6kf37klzjzen4ibfdgalcdp4y.abc123";
    const OBJECT_ID: &str = "bafyreie6n5l5nkbjal37su54cha4coy7qzuhrnajluzv5qd5jvtsrxkequ";
    const OTHER_OBJECT_ID: &str = "bafyreiaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const TYPE_ID: &str = "bafyreityyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyy";
    const TAG_ID: &str = "bafyreitttttttttttttttttttttttttttttttttttttttttttttttt";
    const TAG_TWO_ID: &str = "bafyreiuuuuuuuuuuuuuuuuuuuuuuuuuuuuuuuuuuuuuuuuuuuuuuuu";
    const FILE_ID: &str = "bafyreifffffffffffffffffffffffffffffffffffffffffffffffffffff";
    const REF_ID: &str = "bafyreirrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrr";

    struct FixtureReply {
        status: &'static str,
        body: String,
        delay: Duration,
    }

    impl FixtureReply {
        fn json(body: Value) -> Self {
            Self {
                status: "200 OK",
                body: body.to_string(),
                delay: Duration::ZERO,
            }
        }

        fn error(status: &'static str, body: &str) -> Self {
            Self {
                status,
                body: body.to_owned(),
                delay: Duration::ZERO,
            }
        }

        fn delayed(mut self, delay: Duration) -> Self {
            self.delay = delay;
            self
        }
    }

    async fn fixture(replies: Vec<FixtureReply>) -> (String, JoinHandle<Vec<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind update fixture");
        let address = listener.local_addr().expect("update fixture address");
        let server = tokio::spawn(async move {
            let mut requests = Vec::with_capacity(replies.len());
            for reply in replies {
                let (mut socket, _) = listener.accept().await.expect("accept update request");
                requests.push(read_request(&mut socket).await);
                tokio::time::sleep(reply.delay).await;
                let response = format!(
                    "HTTP/1.1 {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    reply.status,
                    reply.body.len(),
                    reply.body,
                );
                let _ = socket.write_all(response.as_bytes()).await;
            }
            requests
        });
        (format!("http://{address}"), server)
    }

    async fn read_request(socket: &mut TcpStream) -> String {
        let mut request = Vec::new();
        let mut buffer = [0_u8; 2048];
        let (header_end, content_length) = loop {
            let read = socket.read(&mut buffer).await.expect("read update request");
            assert_ne!(read, 0, "request ended before its headers");
            request.extend_from_slice(&buffer[..read]);
            if let Some(index) = request.windows(4).position(|window| window == b"\r\n\r\n") {
                let header_end = index + 4;
                let headers = String::from_utf8_lossy(&request[..header_end]);
                let content_length = headers
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .map(str::trim)
                            .and_then(|value| value.parse::<usize>().ok())
                    })
                    .unwrap_or(0);
                break (header_end, content_length);
            }
        };
        while request.len() < header_end + content_length {
            let read = socket.read(&mut buffer).await.expect("read request body");
            assert_ne!(read, 0, "request ended before its body");
            request.extend_from_slice(&buffer[..read]);
        }
        String::from_utf8(request).expect("request is utf-8")
    }

    async fn no_request_fixture() -> (String, JoinHandle<bool>) {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind no-request fixture");
        let address = listener.local_addr().expect("no-request fixture address");
        let server = tokio::spawn(async move {
            tokio::time::timeout(Duration::from_millis(150), listener.accept())
                .await
                .is_err()
        });
        (format!("http://{address}"), server)
    }

    fn runtime(base_url: String, timeout: Duration) -> RuntimeContext {
        runtime_with_limits(base_url, timeout, ResponseLimits::default())
    }

    fn runtime_with_limits(
        base_url: String,
        timeout: Duration,
        response_limits: ResponseLimits,
    ) -> RuntimeContext {
        let client = AnytypeClient::with_config(ClientConfig {
            base_url: Some(base_url),
            keystore: Some("env".to_owned()),
            keystore_service: Some("object-update-test".to_owned()),
            app_name: "object-update-test".to_owned(),
            response_limits,
            ..ClientConfig::default()
        })
        .expect("update fixture client");
        client.set_api_key(HttpCredentials::new("fixture-token"));
        RuntimeContext::from_parts(
            client,
            1,
            timeout,
            StartupStatus {
                http_available: true,
                grpc_available: false,
            },
        )
    }

    fn input(value: Value) -> ObjectUpdateInput {
        serde_json::from_value(value).expect("valid update input")
    }

    fn object(space_id: &str, object_id: &str, name: &str, body: &str, type_key: &str) -> Value {
        json!({
            "object": {
                "archived": false,
                "id": object_id,
                "space_id": space_id,
                "name": name,
                "markdown": body,
                "type": {
                    "archived": false,
                    "id": TYPE_ID,
                    "key": type_key
                }
            }
        })
    }

    fn request_body(request: &str) -> Value {
        let (_, body) = request.split_once("\r\n\r\n").expect("request body");
        serde_json::from_str(body).expect("JSON request body")
    }

    fn result_code(result: &CallToolResult) -> &str {
        result
            .structured_content
            .as_ref()
            .and_then(|value| value.get("code"))
            .and_then(Value::as_str)
            .expect("error code")
    }

    #[test]
    fn contract_is_destructive_closed_and_null_is_never_omission() {
        let tool = object_update_tool().expect("valid update contract");
        let annotations = serde_json::to_value(tool.as_tool().annotations.as_ref().unwrap())
            .expect("serialize annotations");
        assert_eq!(
            annotations,
            json!({
                "readOnlyHint": false,
                "destructiveHint": true,
                "idempotentHint": false,
                "openWorldHint": false
            })
        );
        assert_eq!(tool.as_tool().name, "object_update");
        for field in [
            "name",
            "body_markdown",
            "expected_body_sha256",
            "properties",
            "type",
            "icon",
        ] {
            let mut value = json!({"space": SPACE_ID, "object_id": OBJECT_ID, "name": "x"});
            value
                .as_object_mut()
                .unwrap()
                .insert(field.to_owned(), Value::Null);
            assert!(serde_json::from_value::<ObjectUpdateInput>(value).is_err());
        }
        assert!(
            serde_json::from_value::<ObjectUpdateInput>(json!({
                "space": SPACE_ID,
                "object_id": OBJECT_ID,
                "name": "x",
                "extra": true
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<ObjectUpdateInput>(json!({
                "space": SPACE_ID,
                "object_id": OBJECT_ID,
                "icon": {"format":"file", "file":"/tmp/secret"}
            }))
            .is_err()
        );
    }

    #[tokio::test]
    async fn read_only_empty_and_duplicate_updates_reject_before_any_io() {
        let cases = [
            (
                MutationAccess::ReadOnly,
                input(json!({"space":SPACE_ID,"object_id":OBJECT_ID,"name":"Changed"})),
            ),
            (
                MutationAccess::Allowed,
                input(json!({"space":SPACE_ID,"object_id":OBJECT_ID})),
            ),
            (
                MutationAccess::Allowed,
                input(json!({
                    "space":SPACE_ID,
                    "object_id":OBJECT_ID,
                    "expected_body_sha256":BodySha256::digest("body").as_str()
                })),
            ),
            (
                MutationAccess::Allowed,
                input(json!({
                    "space":SPACE_ID,
                    "object_id":OBJECT_ID,
                    "properties":[
                        {"format":"text","key":"same","text":"a"},
                        {"format":"url","key":"same","url":"b"}
                    ]
                })),
            ),
        ];
        for (access, input) in cases {
            let (base_url, server) = no_request_fixture().await;
            let result = object_update(
                &runtime(base_url, Duration::from_secs(1)),
                &object_update_tool().unwrap(),
                access,
                &input,
                &CancellationToken::new(),
            )
            .await;
            assert_eq!(result.is_error, Some(true));
            assert_eq!(result_code(&result), "validation");
            assert!(server.await.expect("no-request fixture"));
        }
    }

    #[tokio::test]
    async fn unsafe_resolver_results_are_rejected_before_patch_io() {
        let unsafe_space = json!({
            "items": [{
                "id": "../unsafe",
                "name": "Workspace",
                "object": "space",
                "description": null,
                "icon": null,
                "gateway_url": null,
                "network_id": null
            }],
            "pagination": {"has_more":false,"limit":100,"offset":0,"total":1}
        });
        let unsafe_type = json!({
            "items": [{
                "archived": false,
                "id": TYPE_ID,
                "key": "",
                "name": "Task"
            }],
            "pagination": {"has_more":false,"limit":100,"offset":0,"total":1}
        });
        for (input, reply) in [
            (
                input(json!({
                    "space":"Workspace",
                    "object_id":OBJECT_ID,
                    "name":"Changed"
                })),
                unsafe_space,
            ),
            (
                input(json!({
                    "space":SPACE_ID,
                    "object_id":OBJECT_ID,
                    "name":"Changed",
                    "type":"Task"
                })),
                unsafe_type,
            ),
        ] {
            let (base_url, server) = fixture(vec![FixtureReply::json(reply)]).await;
            let result = object_update(
                &runtime(base_url, Duration::from_secs(1)),
                &object_update_tool().unwrap(),
                MutationAccess::Allowed,
                &input,
                &CancellationToken::new(),
            )
            .await;
            assert_eq!(result.is_error, Some(true));
            assert_eq!(result_code(&result), "upstream");
            let requests = server.await.expect("unsafe resolver fixture");
            assert_eq!(requests.len(), 1);
            assert!(requests[0].starts_with("GET "));
            assert!(!requests[0].contains("PATCH "));
        }
    }

    #[tokio::test]
    async fn omitted_fields_are_absent_from_the_single_patch_body() {
        let changed = object(SPACE_ID, OBJECT_ID, "Changed", "preserved", "page");
        let (base_url, server) = fixture(vec![
            FixtureReply::json(changed.clone()),
            FixtureReply::json(changed),
        ])
        .await;
        let result = object_update(
            &runtime(base_url, Duration::from_secs(1)),
            &object_update_tool().unwrap(),
            MutationAccess::Allowed,
            &input(json!({"space":SPACE_ID,"object_id":OBJECT_ID,"name":"Changed"})),
            &CancellationToken::new(),
        )
        .await;
        assert_eq!(result.is_error, Some(false));
        assert!(
            result
                .structured_content
                .as_ref()
                .unwrap()
                .get("body_sha256")
                .is_none()
        );

        let requests = server.await.expect("update fixture");
        assert_eq!(requests.len(), 2);
        assert!(requests[0].starts_with(&format!(
            "PATCH /v1/spaces/{SPACE_ID}/objects/{OBJECT_ID} HTTP/1.1\r\n"
        )));
        assert_eq!(request_body(&requests[0]), json!({"name":"Changed"}));
        assert!(requests[1].starts_with(&format!(
            "GET /v1/spaces/{SPACE_ID}/objects/{OBJECT_ID} HTTP/1.1\r\n"
        )));
    }

    #[tokio::test]
    async fn stale_hash_conflicts_after_get_without_patch_or_put() {
        let (base_url, server) = fixture(vec![FixtureReply::json(object(
            SPACE_ID,
            OBJECT_ID,
            "Before",
            "current body",
            "page",
        ))])
        .await;
        let stale = BodySha256::digest("stale body");
        let result = object_update(
            &runtime(base_url, Duration::from_secs(1)),
            &object_update_tool().unwrap(),
            MutationAccess::Allowed,
            &input(json!({
                "space":SPACE_ID,
                "object_id":OBJECT_ID,
                "name":"Changed",
                "expected_body_sha256":stale.as_str()
            })),
            &CancellationToken::new(),
        )
        .await;
        assert_eq!(result.is_error, Some(true));
        assert_eq!(result_code(&result), "conflict");
        let requests = server.await.expect("stale fixture");
        assert_eq!(requests.len(), 1);
        assert!(requests[0].starts_with("GET "));
        assert!(!requests[0].contains("PATCH "));
        assert!(!requests[0].contains("PUT "));
    }

    #[tokio::test]
    async fn exact_hash_runs_get_patch_get_and_returns_new_whole_body_hash() {
        let current = object(SPACE_ID, OBJECT_ID, "Before", "old body", "page");
        let updated = object(SPACE_ID, OBJECT_ID, "Before", "new 🦀 body", "page");
        let expected = BodySha256::digest("old body");
        let new_hash = BodySha256::digest("new 🦀 body");
        let (base_url, server) = fixture(vec![
            FixtureReply::json(current),
            FixtureReply::json(updated.clone()),
            FixtureReply::json(updated),
        ])
        .await;
        let result = object_update(
            &runtime(base_url, Duration::from_secs(1)),
            &object_update_tool().unwrap(),
            MutationAccess::Allowed,
            &input(json!({
                "space":SPACE_ID,
                "object_id":OBJECT_ID,
                "body_markdown":"new 🦀 body",
                "expected_body_sha256":expected.as_str()
            })),
            &CancellationToken::new(),
        )
        .await;
        assert_eq!(result.is_error, Some(false));
        assert_eq!(
            result.structured_content.as_ref().unwrap()["body_sha256"],
            new_hash.as_str()
        );
        assert!(
            !serde_json::to_string(&result)
                .unwrap()
                .contains("new 🦀 body")
        );
        let requests = server.await.expect("hash update fixture");
        assert_eq!(requests.len(), 3);
        assert!(requests[0].starts_with("GET "));
        assert!(requests[1].starts_with("PATCH "));
        assert_eq!(
            request_body(&requests[1]),
            json!({"markdown":"new 🦀 body"})
        );
        assert!(requests[2].starts_with("GET "));
    }

    #[tokio::test]
    async fn exact_hash_can_guard_a_non_body_mutation() {
        let current = object(SPACE_ID, OBJECT_ID, "Before", "preserved body", "page");
        let updated = object(SPACE_ID, OBJECT_ID, "After", "preserved body", "page");
        let expected = BodySha256::digest("preserved body");
        let (base_url, server) = fixture(vec![
            FixtureReply::json(current),
            FixtureReply::json(updated.clone()),
            FixtureReply::json(updated),
        ])
        .await;
        let result = object_update(
            &runtime(base_url, Duration::from_secs(1)),
            &object_update_tool().unwrap(),
            MutationAccess::Allowed,
            &input(json!({
                "space":SPACE_ID,
                "object_id":OBJECT_ID,
                "name":"After",
                "expected_body_sha256":expected.as_str()
            })),
            &CancellationToken::new(),
        )
        .await;
        assert_eq!(result.is_error, Some(false));
        assert_eq!(
            result.structured_content.as_ref().unwrap()["body_sha256"],
            expected.as_str()
        );
        let requests = server.await.expect("non-body guard fixture");
        assert_eq!(requests.len(), 3);
        assert_eq!(request_body(&requests[1]), json!({"name":"After"}));
    }

    #[tokio::test]
    async fn no_hash_body_replacement_and_empty_body_clear_are_allowed() {
        for body in ["unguarded replacement", ""] {
            let updated = object(SPACE_ID, OBJECT_ID, "Name", body, "page");
            let (base_url, server) = fixture(vec![
                FixtureReply::json(updated.clone()),
                FixtureReply::json(updated),
            ])
            .await;
            let result = object_update(
                &runtime(base_url, Duration::from_secs(1)),
                &object_update_tool().unwrap(),
                MutationAccess::Allowed,
                &input(json!({
                    "space":SPACE_ID,
                    "object_id":OBJECT_ID,
                    "body_markdown":body
                })),
                &CancellationToken::new(),
            )
            .await;
            assert_eq!(result.is_error, Some(false));
            assert_eq!(
                result.structured_content.as_ref().unwrap()["body_sha256"],
                BodySha256::digest(body).as_str()
            );
            let requests = server.await.expect("unguarded fixture");
            assert_eq!(requests.len(), 2);
            assert_eq!(request_body(&requests[0]), json!({"markdown":body}));
        }
    }

    #[tokio::test]
    async fn closed_property_type_and_icon_values_are_sent_and_verified() {
        let properties = json!([
            {"name":"Text","key":"text_key","id":"prop-text","format":"text","text":"hello"},
            {"name":"Number","key":"number_key","id":"prop-number","format":"number","number":42},
            {"name":"Select","key":"select_key","id":"prop-select","format":"select","select":{"id":TAG_ID,"key":"one","name":"One","color":"blue"}},
            {"name":"Multi","key":"multi_key","id":"prop-multi","format":"multi_select","multi_select":[{"id":TAG_ID,"key":"one","name":"One","color":"blue"},{"id":TAG_TWO_ID,"key":"two","name":"Two","color":"red"}]},
            {"name":"Date","key":"date_key","id":"prop-date","format":"date","date":"2026-07-20T10:00:00Z"},
            {"name":"Files","key":"files_key","id":"prop-files","format":"files","files":[FILE_ID]},
            {"name":"Done","key":"done_key","id":"prop-done","format":"checkbox","checkbox":true},
            {"name":"URL","key":"url_key","id":"prop-url","format":"url","url":"https://example.test"},
            {"name":"Email","key":"email_key","id":"prop-email","format":"email","email":"agent@example.test"},
            {"name":"Phone","key":"phone_key","id":"prop-phone","format":"phone","phone":"+1-555-0100"},
            {"name":"Objects","key":"objects_key","id":"prop-objects","format":"objects","objects":[REF_ID]}
        ]);
        let mut updated = object(SPACE_ID, OBJECT_ID, "Name", "body", "task");
        let object_value = updated["object"].as_object_mut().unwrap();
        object_value.insert(
            "icon".to_owned(),
            json!({"format":"icon","name":"check","color":"teal"}),
        );
        object_value.insert("properties".to_owned(), properties);

        let property_input = json!([
            {"format":"text","key":"text_key","text":"hello"},
            {"format":"number","key":"number_key","number":42},
            {"format":"select","key":"select_key","select":TAG_ID},
            {"format":"multi_select","key":"multi_key","multi_select":[TAG_ID,TAG_TWO_ID]},
            {"format":"date","key":"date_key","date":"2026-07-20T10:00:00Z"},
            {"format":"files","key":"files_key","files":[FILE_ID]},
            {"format":"checkbox","key":"done_key","checkbox":true},
            {"format":"url","key":"url_key","url":"https://example.test"},
            {"format":"email","key":"email_key","email":"agent@example.test"},
            {"format":"phone","key":"phone_key","phone":"+1-555-0100"},
            {"format":"objects","key":"objects_key","objects":[REF_ID]}
        ]);
        let (base_url, server) = fixture(vec![
            FixtureReply::json(updated.clone()),
            FixtureReply::json(updated),
        ])
        .await;
        let result = object_update(
            &runtime(base_url, Duration::from_secs(1)),
            &object_update_tool().unwrap(),
            MutationAccess::Allowed,
            &input(json!({
                "space":SPACE_ID,
                "object_id":OBJECT_ID,
                "type":"@task",
                "icon":{"format":"icon","name":"check","color":"teal"},
                "properties":property_input
            })),
            &CancellationToken::new(),
        )
        .await;
        assert_eq!(result.is_error, Some(false));
        assert_eq!(
            result.structured_content.as_ref().unwrap()["object"]["type_key"],
            "task"
        );
        let requests = server.await.expect("property fixture");
        assert_eq!(requests.len(), 2);
        let body = request_body(&requests[0]);
        assert_eq!(body["type_key"], "task");
        assert_eq!(
            body["icon"],
            json!({"format":"icon","name":"check","color":"teal"})
        );
        assert_eq!(body["properties"].as_array().unwrap().len(), 11);
        assert_eq!(
            body["properties"][0],
            json!({"key":"text_key","text":"hello"})
        );
        assert_eq!(
            body["properties"][2],
            json!({"key":"select_key","select":TAG_ID})
        );
    }

    #[tokio::test]
    async fn update_and_verification_identity_or_state_mismatches_fail_closed() {
        let cases = vec![
            vec![FixtureReply::json(object(
                SPACE_ID,
                OTHER_OBJECT_ID,
                "Changed",
                "body",
                "page",
            ))],
            vec![
                FixtureReply::json(object(SPACE_ID, OBJECT_ID, "Changed", "body", "page")),
                FixtureReply::json(object(OTHER_SPACE_ID, OBJECT_ID, "Changed", "body", "page")),
            ],
            vec![
                FixtureReply::json(object(SPACE_ID, OBJECT_ID, "Changed", "body", "page")),
                FixtureReply::json(object(SPACE_ID, OBJECT_ID, "Not changed", "body", "page")),
            ],
        ];
        for replies in cases {
            let expected_requests = replies.len();
            let (base_url, server) = fixture(replies).await;
            let result = object_update(
                &runtime(base_url, Duration::from_secs(1)),
                &object_update_tool().unwrap(),
                MutationAccess::Allowed,
                &input(json!({"space":SPACE_ID,"object_id":OBJECT_ID,"name":"Changed"})),
                &CancellationToken::new(),
            )
            .await;
            assert_eq!(result.is_error, Some(true));
            assert_eq!(result_code(&result), "upstream");
            assert_eq!(
                server.await.expect("mismatch fixture").len(),
                expected_requests
            );
        }
    }

    #[tokio::test]
    async fn upstream_errors_are_redacted_and_cancellation_is_pre_io() {
        let secret = "Bearer secret-token private upstream body";
        let (base_url, server) = fixture(vec![FixtureReply::error(
            "500 Internal Server Error",
            secret,
        )])
        .await;
        let result = object_update(
            &runtime(base_url, Duration::from_secs(1)),
            &object_update_tool().unwrap(),
            MutationAccess::Allowed,
            &input(json!({"space":SPACE_ID,"object_id":OBJECT_ID,"name":"Changed"})),
            &CancellationToken::new(),
        )
        .await;
        assert_eq!(result_code(&result), "upstream");
        let encoded = serde_json::to_string(&result).unwrap();
        assert!(!encoded.contains("secret-token"));
        assert!(!encoded.contains("private upstream"));
        assert_eq!(server.await.expect("error fixture").len(), 1);

        let (base_url, server) = no_request_fixture().await;
        let cancellation = CancellationToken::new();
        cancellation.cancel();
        let result = object_update(
            &runtime(base_url, Duration::from_secs(1)),
            &object_update_tool().unwrap(),
            MutationAccess::Allowed,
            &input(json!({"space":SPACE_ID,"object_id":OBJECT_ID,"name":"Changed"})),
            &cancellation,
        )
        .await;
        assert_eq!(result_code(&result), "upstream");
        assert!(server.await.expect("cancel fixture"));
    }

    #[tokio::test]
    async fn timeout_and_document_response_ceiling_fail_safely() {
        let changed = object(SPACE_ID, OBJECT_ID, "Changed", "body", "page");
        let (base_url, server) = fixture(vec![
            FixtureReply::json(changed.clone()).delayed(Duration::from_millis(100)),
        ])
        .await;
        let result = object_update(
            &runtime(base_url, Duration::from_millis(20)),
            &object_update_tool().unwrap(),
            MutationAccess::Allowed,
            &input(json!({"space":SPACE_ID,"object_id":OBJECT_ID,"name":"Changed"})),
            &CancellationToken::new(),
        )
        .await;
        assert_eq!(result_code(&result), "upstream");
        assert_eq!(server.await.expect("timeout fixture").len(), 1);

        let (base_url, server) = fixture(vec![FixtureReply::json(changed)]).await;
        let limits = ResponseLimits {
            json_bytes: 64,
            document_bytes: 64,
            error_bytes: 64,
            file_bytes: 64,
        };
        let result = object_update(
            &runtime_with_limits(base_url, Duration::from_secs(1), limits),
            &object_update_tool().unwrap(),
            MutationAccess::Allowed,
            &input(json!({"space":SPACE_ID,"object_id":OBJECT_ID,"name":"Changed"})),
            &CancellationToken::new(),
        )
        .await;
        assert_eq!(result_code(&result), "bounded_result");
        assert_eq!(server.await.expect("ceiling fixture").len(), 1);
    }
}
