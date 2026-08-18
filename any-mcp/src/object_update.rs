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
//! and the update. Supported plain-line bodies use separate safe wire and exact
//! canonical hash forms; ambiguous Markdown remains byte-exact.

use std::{borrow::Cow, collections::HashMap, fmt};

use anytype::{
    objects::{Object, plain_markdown_representation},
    prelude::{VerifyConfig, verify_semantic},
    properties::PropertyFormat,
    types::Type,
    validation::looks_like_object_id,
};
use rmcp::{
    model::CallToolResult,
    schemars::{JsonSchema, Schema, SchemaGenerator, json_schema},
};
use serde::{Deserialize, Deserializer, Serialize, de};
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;

use crate::{
    domain::{BoundedText, DomainValueError, EntityId, ObjectId, ObjectSummary, SpaceId, TypeKey},
    error::{ToolError, mutation_rejection_is_definitive},
    handler_support::{
        HandlerError, HandlerOperationError, MutationAccess, MutationProgress,
        execute_mutation_handler, require_mutation_access,
    },
    mutation_value::{
        MutationCompareError, MutationIcon, MutationInputError, MutationProperties,
        MutationProperty, MutationPropertyKey, normalized_properties,
    },
    object_output::object_summary,
    object_read::AnytypeReference,
    protocol::{ToolProfile, WorkflowTool, workflow_tool},
    result::tool_error,
    runtime::{OperationContext, RuntimeContext},
    schema::SchemaContractError,
    validation::{Omittable, optional_non_null_schema},
};

/// Maximum UTF-8 bytes accepted in a replacement markdown body.
pub const MAX_UPDATE_MARKDOWN_BYTES: usize = 10 * 1024 * 1024;
/// Maximum Unicode scalar values accepted in a replacement markdown body.
pub const MAX_UPDATE_MARKDOWN_CHARS: usize = 100_000;
/// Maximum characters accepted in a nonempty object name.
pub const MAX_UPDATE_NAME_CHARS: usize = 512;
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
    properties: Omittable<MutationProperties>,
    /// Type key, name, or id to resolve. Omit to preserve the current type.
    #[serde(default)]
    #[schemars(schema_with = "optional_reference_schema")]
    r#type: Omittable<AnytypeReference>,
    /// Complete icon replacement. Omit to preserve; clearing is unsupported.
    #[serde(default)]
    #[schemars(schema_with = "optional_icon_schema")]
    icon: Omittable<MutationIcon>,
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
    optional_non_null_schema::<MutationProperties>(generator)
}
fn optional_reference_schema(generator: &mut SchemaGenerator) -> Schema {
    optional_non_null_schema::<AnytypeReference>(generator)
}
fn optional_icon_schema(generator: &mut SchemaGenerator) -> Schema {
    optional_non_null_schema::<MutationIcon>(generator)
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
}

impl fmt::Display for UpdateInputError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::EmptyName => "replacement name must not be empty",
            Self::BoundedValue => "update value exceeds its documented bound",
            Self::InvalidHash => "body hash must be 64 lowercase hexadecimal characters",
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

#[derive(Debug, Clone)]
struct EffectiveType {
    id: EntityId,
    key: TypeKey,
    formats: HashMap<MutationPropertyKey, PropertyFormat>,
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
    let properties = match preflight(input) {
        Ok(properties) => properties,
        Err(error) => return tool_error(error.tool_error()),
    };

    let client = runtime.client();
    let verification = client
        .get_config()
        .get_verify_config()
        .cloned()
        .unwrap_or_else(VerifyConfig::default);
    let input = input.clone();
    let progress = MutationProgress::new();
    let operation_progress = progress.clone();
    let operation = execute_mutation_handler(
        runtime,
        contract,
        OperationContext::new("object_update"),
        cancellation,
        &progress,
        async move {
            let resolved_space = client.resolve_space_id(input.space.as_str()).await?;
            let space_id = checked_space_id(resolved_space)?;
            let object_id = input.object_id.clone();

            let current = if input.r#type.is_none() || input.expected_body_sha256.as_ref().is_some()
            {
                Some(
                    client
                        .object(space_id.as_str(), object_id.as_str())
                        .get()
                        .await?,
                )
            } else {
                None
            };
            if let Some(current) = current.as_ref() {
                verify_identity(current, &space_id, &object_id)?;
            }

            if let Some(expected) = input.expected_body_sha256.as_ref() {
                let current = current
                    .as_ref()
                    .ok_or_else(|| HandlerError::new(ToolError::upstream()))?;
                let current_hash = BodySha256::digest(current.markdown.as_deref().unwrap_or(""));
                if &current_hash != expected {
                    return Err(HandlerError::new(ToolError::conflict()).into());
                }
            }

            let effective_type = if let Some(reference) = input.r#type.as_ref() {
                let resolved = client
                    .resolve_type(space_id.as_str(), reference.as_str())
                    .await?;
                checked_effective_type(resolved, Some(reference), true)?
            } else {
                let current = current
                    .as_ref()
                    .ok_or_else(|| HandlerError::new(ToolError::upstream()))?;
                let embedded = current
                    .r#type
                    .as_ref()
                    .ok_or_else(|| HandlerError::new(ToolError::upstream()))?;
                let embedded_id = EntityId::new(embedded.id.clone()).map_err(upstream_domain)?;
                let embedded_key = checked_type_key(embedded.key.clone())?;
                let resolved = client
                    .resolve_type(space_id.as_str(), embedded_id.as_str())
                    .await?;
                let effective = checked_effective_type(resolved, None, false)?;
                if effective.id != embedded_id || effective.key != embedded_key {
                    return Err(HandlerError::new(ToolError::upstream()).into());
                }
                effective
            };
            validate_properties_for_type(&properties, &effective_type)?;

            let mut request = client
                .update_object(space_id.as_str(), object_id.as_str())
                .no_verify();
            if let Some(name) = input.name.as_ref() {
                request = request.name(name.as_str());
            }
            if let Some(body) = input.body_markdown.as_ref() {
                let representation = plain_markdown_representation(body.as_str());
                let wire = representation
                    .as_ref()
                    .map_or(body.as_str(), |representation| representation.wire());
                request = request.body(wire);
            }
            if input.r#type.as_ref().is_some() {
                request = request.type_key(effective_type.key.as_str());
            }
            if let Some(icon) = input.icon.as_ref() {
                request = request.icon(icon.to_anytype());
            }
            for property in &properties {
                request = property.apply(request);
            }

            operation_progress.mark_dispatched(runtime)?;
            let patch_anomaly = match request.update().await {
                Ok(returned) => !requested_state_matches(
                    &returned,
                    &space_id,
                    &object_id,
                    &effective_type,
                    &input,
                    &properties,
                )
                .unwrap_or(false),
                Err(error) if mutation_rejection_is_definitive(&error) => {
                    return Err(error.into());
                }
                Err(_) => true,
            };

            let verified = verify_semantic(
                &verification,
                "object",
                object_id.as_str(),
                || async {
                    client
                        .object(space_id.as_str(), object_id.as_str())
                        .get()
                        .await
                },
                |object| {
                    requested_state_matches(
                        object,
                        &space_id,
                        &object_id,
                        &effective_type,
                        &input,
                        &properties,
                    )
                    .unwrap_or(false)
                },
            )
            .await
            .map_err(|_| HandlerError::new(ToolError::mutation_indeterminate()))?;

            if patch_anomaly {
                return Err(HandlerError::new(ToolError::mutation_indeterminate()).into());
            }
            let hash = verified_body_hash(&verified, &input);
            Ok::<_, HandlerOperationError>(UpdateExecution {
                object: verified,
                body_sha256: hash,
            })
        },
        |execution| async move {
            let object = object_summary(&execution.object)
                .map_err(|_| HandlerError::new(ToolError::mutation_indeterminate()))?;
            Ok(ObjectUpdateOutput {
                object,
                body_sha256: execution.body_sha256,
            })
        },
    );
    let routed = runtime
        .run_routed_invocation("object_update", cancellation, Box::pin(operation))
        .await;
    match routed {
        Ok(result) => result,
        Err(failure) if failure.dispatched => tool_error(&ToolError::mutation_indeterminate()),
        Err(_) => tool_error(&ToolError::upstream()),
    }
}

fn preflight(input: &ObjectUpdateInput) -> Result<Vec<MutationProperty>, HandlerError> {
    if !input.has_mutation() {
        return Err(HandlerError::new(ToolError::validation()));
    }
    if let Some(reference) = input.r#type.as_ref()
        && let Some(explicit) = reference.as_str().strip_prefix('@')
    {
        TypeKey::new(explicit).map_err(|_| HandlerError::new(ToolError::validation()))?;
    }
    if input.body_markdown.as_ref().is_some_and(|body| {
        plain_markdown_representation(body.as_str()).is_some_and(|representation| {
            representation.canonical().len() > MAX_UPDATE_MARKDOWN_BYTES
                || representation.canonical().chars().count() > MAX_UPDATE_MARKDOWN_CHARS
        })
    }) {
        return Err(HandlerError::new(ToolError::validation()));
    }
    input
        .properties
        .as_ref()
        .map(normalized_properties)
        .transpose()
        .map(|properties| properties.unwrap_or_default())
        .map_err(mutation_input_validation)
}

fn checked_space_id(value: String) -> Result<SpaceId, HandlerError> {
    SpaceId::new(value).map_err(upstream_domain)
}

fn checked_type_key(value: String) -> Result<TypeKey, HandlerError> {
    TypeKey::new(value).map_err(upstream_domain)
}

fn checked_effective_type(
    resolved: Type,
    reference: Option<&AnytypeReference>,
    caller_selected: bool,
) -> Result<EffectiveType, HandlerError> {
    if resolved.archived {
        return Err(HandlerError::new(if caller_selected {
            ToolError::validation()
        } else {
            ToolError::upstream()
        }));
    }
    let id = EntityId::new(resolved.id).map_err(upstream_domain)?;
    let key = checked_type_key(resolved.key)?;
    if let Some(reference) = reference {
        if let Some(explicit_key) = reference.as_str().strip_prefix('@') {
            if key.as_str() != explicit_key {
                return Err(HandlerError::new(ToolError::upstream()));
            }
        } else if looks_like_object_id(reference.as_str()) && id.as_str() != reference.as_str() {
            return Err(HandlerError::new(ToolError::upstream()));
        }
    }

    let mut formats = HashMap::with_capacity(resolved.properties.len());
    for property in resolved.properties {
        let format = property.format();
        EntityId::new(property.id).map_err(upstream_domain)?;
        let property_key =
            MutationPropertyKey::new(property.key).map_err(mutation_input_upstream)?;
        if formats.insert(property_key, format).is_some() {
            return Err(HandlerError::new(ToolError::upstream()));
        }
    }
    Ok(EffectiveType { id, key, formats })
}

fn validate_properties_for_type(
    properties: &[MutationProperty],
    effective_type: &EffectiveType,
) -> Result<(), HandlerError> {
    for property in properties {
        if effective_type.formats.get(property.key()) != Some(&property.format()) {
            return Err(HandlerError::new(ToolError::validation()));
        }
    }
    Ok(())
}

fn upstream_domain(error: DomainValueError) -> HandlerError {
    match error {
        DomainValueError::TooLong { .. } => HandlerError::new(ToolError::bounded_result()),
        DomainValueError::Empty | DomainValueError::InvalidIdentifierCharacter => {
            HandlerError::new(ToolError::upstream())
        }
    }
}

fn mutation_input_validation(_: MutationInputError) -> HandlerError {
    HandlerError::new(ToolError::validation())
}

fn mutation_input_upstream(error: MutationInputError) -> HandlerError {
    match error {
        MutationInputError::TooLong | MutationInputError::TooManyIds => {
            HandlerError::new(ToolError::bounded_result())
        }
        MutationInputError::Empty
        | MutationInputError::UnsafePropertyKey
        | MutationInputError::InvalidNumber
        | MutationInputError::InvalidDate
        | MutationInputError::DuplicatePropertyKey => HandlerError::new(ToolError::upstream()),
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

fn requested_state_matches(
    object: &Object,
    space_id: &SpaceId,
    object_id: &ObjectId,
    effective_type: &EffectiveType,
    input: &ObjectUpdateInput,
    properties: &[MutationProperty],
) -> Result<bool, HandlerError> {
    verify_identity(object, space_id, object_id)?;
    if let Some(name) = input.name.as_ref()
        && object.name.as_deref() != Some(name.as_str())
    {
        return Ok(false);
    }
    let returned_type = object
        .r#type
        .as_ref()
        .ok_or_else(|| HandlerError::new(ToolError::upstream()))?;
    let returned_type_id = EntityId::new(returned_type.id.clone()).map_err(upstream_domain)?;
    let returned_type_key = checked_type_key(returned_type.key.clone())?;
    if returned_type_id != effective_type.id || returned_type_key != effective_type.key {
        return Ok(false);
    }
    if let Some(icon) = input.icon.as_ref()
        && !icon
            .matches_returned(object.icon.as_ref())
            .map_err(mutation_compare_error)?
    {
        return Ok(false);
    }
    for expected in properties {
        let mut matching = object
            .properties
            .iter()
            .filter(|property| property.key == expected.key().as_str());
        let returned = matching.next();
        if matching.next().is_some()
            || !expected
                .matches_returned(returned.map(|property| &property.value))
                .map_err(mutation_compare_error)?
        {
            return Ok(false);
        }
    }

    if let Some(expected) = expected_final_body_hash(input)
        && BodySha256::digest(object.markdown.as_deref().unwrap_or("")) != expected
    {
        return Ok(false);
    }
    Ok(true)
}

fn expected_final_body_hash(input: &ObjectUpdateInput) -> Option<BodySha256> {
    input
        .body_markdown
        .as_ref()
        .map(|body| {
            let representation = plain_markdown_representation(body.as_str());
            let canonical = representation
                .as_ref()
                .map_or(body.as_str(), |representation| representation.canonical());
            BodySha256::digest(canonical)
        })
        .or_else(|| input.expected_body_sha256.as_ref().cloned())
}

fn verified_body_hash(object: &Object, input: &ObjectUpdateInput) -> Option<BodySha256> {
    expected_final_body_hash(input)
        .map(|_| BodySha256::digest(object.markdown.as_deref().unwrap_or("")))
}

fn mutation_compare_error(error: MutationCompareError) -> HandlerError {
    match error {
        MutationCompareError::Bounded => HandlerError::new(ToolError::bounded_result()),
        MutationCompareError::Malformed => HandlerError::new(ToolError::upstream()),
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use anytype::prelude::{AnytypeClient, ClientConfig, HttpCredentials, ResponseLimits};
    use serde_json::{Value, json};
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::{TcpListener, TcpStream},
        sync::oneshot,
        task::JoinHandle,
    };

    use super::*;
    use crate::{error::ToolErrorCode, runtime::StartupStatus};

    const SPACE_ID: &str =
        "bafyreid5fvqlnsobih2keakcxjrrlpmly6kf37klzjzen4ibfdgalcdp4y.2tq5w93cr6oe7";
    const OBJECT_ID: &str = "bafyreie6n5l5nkbjal37su54cha4coy7qzuhrnajluzv5qd5jvtsrxkequ";
    const OTHER_OBJECT_ID: &str = "bafyreiaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const TYPE_ID: &str = "bafyreibbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const TAG_ID: &str = "bafyreitttttttttttttttttttttttttttttttttttttttttttttttt";
    const TAG_TWO_ID: &str = "bafyreiuuuuuuuuuuuuuuuuuuuuuuuuuuuuuuuuuuuuuuuuuuuuuuuu";
    const FILE_ID: &str = "bafyreifffffffffffffffffffffffffffffffffffffffffffffffffffff";
    const REF_ID: &str = "bafyreirrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrr";

    struct FixtureReply {
        status: &'static str,
        body: String,
        headers: String,
        delay: Duration,
    }

    impl FixtureReply {
        fn json(body: Value) -> Self {
            Self {
                status: "200 OK",
                body: body.to_string(),
                headers: String::new(),
                delay: Duration::ZERO,
            }
        }

        fn error(status: &'static str, body: &str) -> Self {
            Self {
                status,
                body: body.to_owned(),
                headers: String::new(),
                delay: Duration::ZERO,
            }
        }

        fn redirect(status: &'static str, location: &str) -> Self {
            Self {
                status,
                body: "{}".to_owned(),
                headers: format!("Location: {location}\r\n"),
                delay: Duration::ZERO,
            }
        }

        fn delayed(mut self, delay: Duration) -> Self {
            self.delay = delay;
            self
        }
    }

    async fn fixture(replies: Vec<FixtureReply>) -> (String, JoinHandle<Vec<String>>) {
        let (base_url, server, _) = fixture_with_signal(replies, None).await;
        (base_url, server)
    }

    async fn fixture_with_signal(
        replies: Vec<FixtureReply>,
        signal_request: Option<usize>,
    ) -> (
        String,
        JoinHandle<Vec<String>>,
        Option<oneshot::Receiver<()>>,
    ) {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind update fixture");
        let address = listener.local_addr().expect("update fixture address");
        let (signal_tx, signal_rx) = oneshot::channel();
        let mut signal_tx = signal_request.map(|_| signal_tx);
        let server = tokio::spawn(async move {
            let mut requests = Vec::with_capacity(replies.len());
            for (index, reply) in replies.into_iter().enumerate() {
                let (mut socket, _) = listener.accept().await.expect("accept update request");
                requests.push(read_request(&mut socket).await);
                if signal_request == Some(index + 1)
                    && let Some(signal_tx) = signal_tx.take()
                {
                    let _ = signal_tx.send(());
                }
                tokio::time::sleep(reply.delay).await;
                let response = format!(
                    "HTTP/1.1 {}\r\nContent-Type: application/json\r\n{}Content-Length: {}\r\nConnection: close\r\n\r\n{}",
                    reply.status,
                    reply.headers,
                    reply.body.len(),
                    reply.body,
                );
                let _ = socket.write_all(response.as_bytes()).await;
            }
            requests
        });
        (
            format!("http://{address}"),
            server,
            signal_request.map(|_| signal_rx),
        )
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

    async fn monitored_no_request_fixture() -> (String, oneshot::Sender<()>, JoinHandle<bool>) {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind monitored no-request fixture");
        let address = listener
            .local_addr()
            .expect("monitored no-request fixture address");
        let (done_tx, done_rx) = oneshot::channel();
        let server = tokio::spawn(async move {
            tokio::select! {
                _ = listener.accept() => false,
                _ = done_rx => true,
            }
        });
        (format!("http://{address}"), done_tx, server)
    }

    fn runtime(base_url: String, timeout: Duration) -> RuntimeContext {
        runtime_with_limits(base_url, timeout, ResponseLimits::default())
    }

    fn runtime_with_limits(
        base_url: String,
        timeout: Duration,
        response_limits: ResponseLimits,
    ) -> RuntimeContext {
        runtime_with_options(base_url, timeout, response_limits, None)
    }

    fn runtime_with_options(
        base_url: String,
        timeout: Duration,
        response_limits: ResponseLimits,
        verify: Option<VerifyConfig>,
    ) -> RuntimeContext {
        let client = AnytypeClient::with_config(ClientConfig {
            base_url: Some(base_url),
            keystore: Some("env".to_owned()),
            keystore_service: Some("object-update-test".to_owned()),
            app_name: "object-update-test".to_owned(),
            response_limits,
            verify,
            disable_cache: true,
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

    fn type_definition(type_key: &str, properties: Value) -> Value {
        json!({
            "archived": false,
            "id": TYPE_ID,
            "key": type_key,
            "name": type_key,
            "properties": properties
        })
    }

    fn type_response(type_key: &str, properties: Value) -> Value {
        json!({"type": type_definition(type_key, properties)})
    }

    fn type_page(type_key: &str, properties: Value) -> Value {
        json!({
            "items": [type_definition(type_key, properties)],
            "pagination": {"has_more":false,"limit":100,"offset":0,"total":1}
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

    fn result_message(result: &CallToolResult) -> &str {
        result
            .structured_content
            .as_ref()
            .and_then(|value| value.get("message"))
            .and_then(Value::as_str)
            .expect("error message")
    }

    fn fast_verify(max_attempts: usize) -> VerifyConfig {
        VerifyConfig {
            timeout: Duration::from_secs(1),
            initial_delay: Duration::ZERO,
            max_delay: Duration::ZERO,
            max_attempts,
        }
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
        for properties in [
            json!([{"format":"select","key":"choice","select":null}]),
            json!([{"format":"number","key":"amount","number":null}]),
            json!([{"format":"date","key":"when","date":""}]),
        ] {
            assert!(
                serde_json::from_value::<ObjectUpdateInput>(json!({
                    "space": SPACE_ID,
                    "object_id": OBJECT_ID,
                    "properties": properties
                }))
                .is_err()
            );
        }
        assert!(
            serde_json::from_value::<ObjectUpdateInput>(json!({
                "space": SPACE_ID,
                "object_id": OBJECT_ID,
                "name": ""
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
                    "type":"@"
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
        for (input, reply, expected_code) in [
            (
                input(json!({
                    "space":"Workspace",
                    "object_id":OBJECT_ID,
                    "name":"Changed"
                })),
                unsafe_space,
                "authentication",
            ),
            (
                input(json!({
                    "space":SPACE_ID,
                    "object_id":OBJECT_ID,
                    "name":"Changed",
                    "type":"Task"
                })),
                unsafe_type,
                "upstream",
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
            assert_eq!(result_code(&result), expected_code);
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
            FixtureReply::json(type_response("page", json!([]))),
            FixtureReply::json(changed.clone()),
            FixtureReply::json(changed),
        ])
        .await;
        let result = object_update(
            &runtime(base_url, Duration::from_secs(1)),
            &object_update_tool().unwrap(),
            MutationAccess::Allowed,
            &input(json!({
                "space":SPACE_ID,
                "object_id":OBJECT_ID,
                "name":"Changed",
                "properties":[]
            })),
            &CancellationToken::new(),
        )
        .await;
        assert_eq!(
            result.is_error,
            Some(false),
            "result={:?}",
            result.structured_content
        );
        assert!(
            result
                .structured_content
                .as_ref()
                .unwrap()
                .get("body_sha256")
                .is_none()
        );

        let requests = server.await.expect("update fixture");
        assert_eq!(requests.len(), 4);
        assert!(requests[0].starts_with(&format!(
            "GET /v1/spaces/{SPACE_ID}/objects/{OBJECT_ID} HTTP/1.1\r\n"
        )));
        assert!(requests[1].starts_with(&format!(
            "GET /v1/spaces/{SPACE_ID}/types/{TYPE_ID} HTTP/1.1\r\n"
        )));
        assert!(requests[2].starts_with(&format!(
            "PATCH /v1/spaces/{SPACE_ID}/objects/{OBJECT_ID} HTTP/1.1\r\n"
        )));
        assert_eq!(request_body(&requests[2]), json!({"name":"Changed"}));
        assert!(requests[3].starts_with(&format!(
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
            FixtureReply::json(type_response("page", json!([]))),
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
        assert_eq!(requests.len(), 4);
        assert!(requests[0].starts_with("GET "));
        assert!(requests[1].contains(&format!("/types/{TYPE_ID}")));
        assert!(requests[2].starts_with("PATCH "));
        assert_eq!(
            request_body(&requests[2]),
            json!({"markdown":"new 🦀 body"})
        );
        assert!(requests[3].starts_with("GET "));
    }

    #[tokio::test]
    async fn exact_hash_can_guard_a_non_body_mutation() {
        let current = object(SPACE_ID, OBJECT_ID, "Before", "preserved body", "page");
        let updated = object(SPACE_ID, OBJECT_ID, "After", "preserved body", "page");
        let expected = BodySha256::digest("preserved body");
        let (base_url, server) = fixture(vec![
            FixtureReply::json(current),
            FixtureReply::json(type_response("page", json!([]))),
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
        assert_eq!(requests.len(), 4);
        assert_eq!(request_body(&requests[2]), json!({"name":"After"}));
    }

    #[tokio::test]
    async fn no_hash_body_replacement_and_empty_body_clear_are_allowed() {
        for (body, canonical) in [
            ("unguarded replacement", "unguarded replacement   \n"),
            ("", ""),
        ] {
            let updated = object(SPACE_ID, OBJECT_ID, "Name", canonical, "page");
            let (base_url, server) = fixture(vec![
                FixtureReply::json(updated.clone()),
                FixtureReply::json(type_response("page", json!([]))),
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
                BodySha256::digest(canonical).as_str()
            );
            let requests = server.await.expect("unguarded fixture");
            assert_eq!(requests.len(), 4);
            assert_eq!(request_body(&requests[2]), json!({"markdown":body}));
        }
    }

    #[tokio::test]
    async fn canonical_underscore_body_replay_uses_raw_wire_and_exact_hash() {
        let current_body = "alpha current body   \n";
        let requested = "alpha unique\\_0   \n";
        let current = object(SPACE_ID, OBJECT_ID, "Before", current_body, "page");
        let updated = object(SPACE_ID, OBJECT_ID, "Before", requested, "page");
        let expected = BodySha256::digest(current_body);
        let (base_url, server) = fixture(vec![
            FixtureReply::json(current),
            FixtureReply::json(type_response("page", json!([]))),
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
                "body_markdown":requested,
                "expected_body_sha256":expected.as_str()
            })),
            &CancellationToken::new(),
        )
        .await;
        assert_eq!(result.is_error, Some(false), "{result:?}");
        assert_eq!(
            result.structured_content.as_ref().unwrap()["body_sha256"],
            BodySha256::digest(requested).as_str()
        );
        let requests = server.await.expect("canonical update fixture");
        assert_eq!(
            request_body(&requests[2]),
            json!({"markdown":"alpha unique_0"})
        );
    }

    #[tokio::test]
    async fn closed_property_type_and_icon_values_are_sent_and_verified() {
        let property_schema = json!([
            {"name":"Text","key":"text_key","id":"prop-text","format":"text"},
            {"name":"Number","key":"number_key","id":"prop-number","format":"number"},
            {"name":"Select","key":"select_key","id":"prop-select","format":"select"},
            {"name":"Multi","key":"multi_key","id":"prop-multi","format":"multi_select"},
            {"name":"Date","key":"date_key","id":"prop-date","format":"date"},
            {"name":"Files","key":"files_key","id":"prop-files","format":"files"},
            {"name":"Done","key":"done_key","id":"prop-done","format":"checkbox"},
            {"name":"URL","key":"url_key","id":"prop-url","format":"url"},
            {"name":"Email","key":"email_key","id":"prop-email","format":"email"},
            {"name":"Phone","key":"phone_key","id":"prop-phone","format":"phone"},
            {"name":"Objects","key":"objects_key","id":"prop-objects","format":"objects"}
        ]);
        let properties = json!([
            {"name":"Text","key":"text_key","id":"prop-text","format":"text","text":"hello"},
            {"name":"Number","key":"number_key","id":"prop-number","format":"number","number":1},
            {"name":"Select","key":"select_key","id":"prop-select","format":"select","select":{"id":TAG_ID,"key":"one","name":"One","color":"blue"}},
            {"name":"Multi","key":"multi_key","id":"prop-multi","format":"multi_select","multi_select":[{"id":TAG_TWO_ID,"key":"two","name":"Two","color":"red"},{"id":TAG_ID,"key":"one","name":"One","color":"blue"}]},
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
            {"format":"number","key":"number_key","number":1.0},
            {"format":"select","key":"select_key","select":TAG_ID},
            {"format":"multi_select","key":"multi_key","multi_select":[TAG_TWO_ID,TAG_ID,TAG_ID]},
            {"format":"date","key":"date_key","date":"2026-07-20T12:00:00+02:00"},
            {"format":"files","key":"files_key","files":[FILE_ID]},
            {"format":"checkbox","key":"done_key","checkbox":true},
            {"format":"url","key":"url_key","url":"https://example.test"},
            {"format":"email","key":"email_key","email":"agent@example.test"},
            {"format":"phone","key":"phone_key","phone":"+1-555-0100"},
            {"format":"objects","key":"objects_key","objects":[REF_ID]}
        ]);
        let (base_url, server) = fixture(vec![
            FixtureReply::json(type_page("task", property_schema)),
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
        assert_eq!(requests.len(), 3);
        assert!(requests[0].contains("/types?"));
        let body = request_body(&requests[1]);
        assert_eq!(body["type_key"], "task");
        assert_eq!(
            body["icon"],
            json!({"format":"icon","name":"check","color":"teal"})
        );
        assert_eq!(body["properties"].as_array().unwrap().len(), 11);
        let sent = body["properties"].as_array().unwrap();
        assert!(sent.contains(&json!({"key":"text_key","text":"hello"})));
        assert!(sent.contains(&json!({"key":"select_key","select":TAG_ID})));
        assert!(
            sent.windows(2).all(|pair| {
                pair[0]["key"].as_str().unwrap() < pair[1]["key"].as_str().unwrap()
            })
        );
    }

    #[test]
    fn hashes_and_markdown_bounds_are_exact_and_unicode_based() {
        assert_eq!(
            BodySha256::digest("abc").as_str(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert!(BodySha256::new("A".repeat(64)).is_err());
        assert!(BodySha256::new("a".repeat(63)).is_err());
        assert!(UpdateMarkdown::new("🦀".repeat(MAX_UPDATE_MARKDOWN_CHARS)).is_ok());
        assert!(UpdateMarkdown::new("🦀".repeat(MAX_UPDATE_MARKDOWN_CHARS + 1)).is_err());

        let raw_boundary = format!("{}_", "a".repeat(MAX_UPDATE_MARKDOWN_CHARS - 1));
        let boundary_input = input(json!({
            "space": SPACE_ID,
            "object_id": OBJECT_ID,
            "body_markdown": raw_boundary
        }));
        assert_eq!(
            preflight(&boundary_input)
                .expect_err("canonical escape and suffix cross the body ceiling")
                .tool_error()
                .code(),
            ToolErrorCode::Validation
        );
    }

    #[tokio::test]
    async fn effective_type_schema_is_exact_and_rejected_before_patch() {
        let archived = json!({
            "type": {
                "archived": true,
                "id": TYPE_ID,
                "key": "task",
                "name": "Task",
                "properties": []
            }
        });
        let duplicate = json!([
            {"name":"A","key":"same","id":"prop-a","format":"text"},
            {"name":"B","key":"same","id":"prop-b","format":"url"}
        ]);
        let wrong_direct = {
            let mut value = type_response("task", json!([]));
            value["type"]["id"] = json!(OTHER_OBJECT_ID);
            value
        };
        let cases = [
            (
                input(json!({"space":SPACE_ID,"object_id":OBJECT_ID,"type":TYPE_ID})),
                archived,
                "validation",
            ),
            (
                input(json!({
                    "space":SPACE_ID,
                    "object_id":OBJECT_ID,
                    "type":"@task",
                    "properties":[{"format":"text","key":"missing","text":"x"}]
                })),
                type_page("task", json!([])),
                "validation",
            ),
            (
                input(json!({
                    "space":SPACE_ID,
                    "object_id":OBJECT_ID,
                    "type":"@task",
                    "properties":[{"format":"url","key":"value","url":"x"}]
                })),
                type_page(
                    "task",
                    json!([
                        {"name":"Value","key":"value","id":"prop-value","format":"text"}
                    ]),
                ),
                "validation",
            ),
            (
                input(json!({"space":SPACE_ID,"object_id":OBJECT_ID,"type":"@task"})),
                type_page("task", duplicate),
                "upstream",
            ),
            (
                input(json!({"space":SPACE_ID,"object_id":OBJECT_ID,"type":TYPE_ID})),
                wrong_direct,
                "upstream",
            ),
        ];
        for (input, reply, code) in cases {
            let (base_url, server) = fixture(vec![FixtureReply::json(reply)]).await;
            let result = object_update(
                &runtime(base_url, Duration::from_secs(1)),
                &object_update_tool().unwrap(),
                MutationAccess::Allowed,
                &input,
                &CancellationToken::new(),
            )
            .await;
            assert_eq!(result_code(&result), code);
            let requests = server.await.expect("pre-patch schema fixture");
            assert_eq!(requests.len(), 1);
            assert!(requests[0].starts_with("GET "));
        }
    }

    #[tokio::test]
    async fn documented_empty_forms_clear_body_and_clearable_properties() {
        let schema = json!([
            {"name":"Text","key":"text","id":"prop-text","format":"text"},
            {"name":"Multi","key":"multi","id":"prop-multi","format":"multi_select"},
            {"name":"Files","key":"files","id":"prop-files","format":"files"},
            {"name":"URL","key":"url","id":"prop-url","format":"url"},
            {"name":"Email","key":"email","id":"prop-email","format":"email"},
            {"name":"Phone","key":"phone","id":"prop-phone","format":"phone"},
            {"name":"Objects","key":"objects","id":"prop-objects","format":"objects"}
        ]);
        let cleared = object(SPACE_ID, OBJECT_ID, "Name", "", "task");
        let (base_url, server) = fixture(vec![
            FixtureReply::json(type_page("task", schema)),
            FixtureReply::json(cleared.clone()),
            FixtureReply::json(cleared),
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
                "body_markdown":"",
                "properties":[
                    {"format":"text","key":"text","text":""},
                    {"format":"multi_select","key":"multi","multi_select":[]},
                    {"format":"files","key":"files","files":[]},
                    {"format":"url","key":"url","url":""},
                    {"format":"email","key":"email","email":""},
                    {"format":"phone","key":"phone","phone":""},
                    {"format":"objects","key":"objects","objects":[]}
                ]
            })),
            &CancellationToken::new(),
        )
        .await;
        assert_eq!(result.is_error, Some(false));
        assert_eq!(
            result.structured_content.as_ref().unwrap()["body_sha256"],
            BodySha256::digest("").as_str()
        );
        let requests = server.await.expect("clear fixture");
        assert_eq!(requests.len(), 3);
        let body = request_body(&requests[1]);
        assert_eq!(body["markdown"], "");
        assert_eq!(body["type_key"], "task");
        assert_eq!(body["properties"].as_array().unwrap().len(), 7);
    }

    #[tokio::test]
    async fn semantic_verification_retries_stale_state_then_converges() {
        let current = object(SPACE_ID, OBJECT_ID, "Before", "body", "page");
        let updated = object(SPACE_ID, OBJECT_ID, "After", "body", "page");
        let (base_url, server) = fixture(vec![
            FixtureReply::json(current.clone()),
            FixtureReply::json(type_response("page", json!([]))),
            FixtureReply::json(updated.clone()),
            FixtureReply::json(current),
            FixtureReply::json(updated),
        ])
        .await;
        let runtime = runtime_with_options(
            base_url,
            Duration::from_secs(1),
            ResponseLimits::default(),
            Some(fast_verify(2)),
        );
        let result = object_update(
            &runtime,
            &object_update_tool().unwrap(),
            MutationAccess::Allowed,
            &input(json!({"space":SPACE_ID,"object_id":OBJECT_ID,"name":"After"})),
            &CancellationToken::new(),
        )
        .await;
        assert_eq!(result.is_error, Some(false));
        assert_eq!(
            server.await.expect("eventual verification fixture").len(),
            5
        );
    }

    #[tokio::test]
    async fn exhausted_or_malformed_verification_is_indeterminate() {
        let current = object(SPACE_ID, OBJECT_ID, "Before", "body", "page");
        let updated = object(SPACE_ID, OBJECT_ID, "After", "body", "page");
        let cases = [
            vec![
                FixtureReply::json(current.clone()),
                FixtureReply::json(type_response("page", json!([]))),
                FixtureReply::json(updated.clone()),
                FixtureReply::json(current.clone()),
                FixtureReply::json(current.clone()),
            ],
            vec![
                FixtureReply::json(current.clone()),
                FixtureReply::json(type_response("page", json!([]))),
                FixtureReply::json(updated.clone()),
                FixtureReply::error("200 OK", "not-json"),
            ],
        ];
        for replies in cases {
            let expected = replies.len();
            let (base_url, server) = fixture(replies).await;
            let runtime = runtime_with_options(
                base_url,
                Duration::from_secs(1),
                ResponseLimits::default(),
                Some(fast_verify(2)),
            );
            let result = object_update(
                &runtime,
                &object_update_tool().unwrap(),
                MutationAccess::Allowed,
                &input(json!({"space":SPACE_ID,"object_id":OBJECT_ID,"name":"After"})),
                &CancellationToken::new(),
            )
            .await;
            assert_eq!(result_code(&result), "conflict");
            assert_eq!(
                result_message(&result),
                ToolError::mutation_indeterminate().message()
            );
            assert_eq!(
                server.await.expect("failed verification fixture").len(),
                expected
            );
        }
    }

    #[tokio::test]
    async fn patch_anomalies_stay_indeterminate_even_after_matching_recovery_read() {
        let current = object(SPACE_ID, OBJECT_ID, "Before", "body", "page");
        let updated = object(SPACE_ID, OBJECT_ID, "After", "body", "page");
        let mismatch = object(SPACE_ID, OBJECT_ID, "Not updated", "body", "page");
        let cases = [
            FixtureReply::error("200 OK", "not-json"),
            FixtureReply::json(mismatch),
            FixtureReply::error("500 Internal Server Error", "private failure"),
        ];
        for patch_reply in cases {
            let (base_url, server) = fixture(vec![
                FixtureReply::json(current.clone()),
                FixtureReply::json(type_response("page", json!([]))),
                patch_reply,
                FixtureReply::json(updated.clone()),
            ])
            .await;
            let result = object_update(
                &runtime_with_options(
                    base_url,
                    Duration::from_secs(1),
                    ResponseLimits::default(),
                    Some(fast_verify(1)),
                ),
                &object_update_tool().unwrap(),
                MutationAccess::Allowed,
                &input(json!({"space":SPACE_ID,"object_id":OBJECT_ID,"name":"After"})),
                &CancellationToken::new(),
            )
            .await;
            assert_eq!(result_code(&result), "conflict");
            assert_eq!(
                result_message(&result),
                ToolError::mutation_indeterminate().message()
            );
            assert!(
                !serde_json::to_string(&result)
                    .unwrap()
                    .contains("private failure")
            );
            assert_eq!(server.await.expect("patch anomaly fixture").len(), 4);
        }
    }

    #[tokio::test]
    async fn definitive_patch_4xx_is_ordinary_and_skips_verification() {
        let current = object(SPACE_ID, OBJECT_ID, "Before", "body", "page");
        let (base_url, server) = fixture(vec![
            FixtureReply::json(current),
            FixtureReply::json(type_response("page", json!([]))),
            FixtureReply::error("400 Bad Request", "private validation detail"),
        ])
        .await;
        let result = object_update(
            &runtime(base_url, Duration::from_secs(1)),
            &object_update_tool().unwrap(),
            MutationAccess::Allowed,
            &input(json!({"space":SPACE_ID,"object_id":OBJECT_ID,"name":"After"})),
            &CancellationToken::new(),
        )
        .await;
        assert_ne!(result_code(&result), "conflict");
        assert!(
            !serde_json::to_string(&result)
                .unwrap()
                .contains("private validation")
        );
        assert_eq!(server.await.expect("definitive patch fixture").len(), 3);
    }

    #[tokio::test]
    async fn patch_429_is_sent_once_and_maps_as_terminal_indeterminate() {
        // A mutation 429 is indeterminate under the HTTP timeout policy: the
        // server may have applied the update before rate-limiting the
        // response, so the fixed conflict error demands a fresh observation.
        let current = object(SPACE_ID, OBJECT_ID, "Before", "body", "page");
        let (base_url, server) = fixture(vec![
            FixtureReply::json(current),
            FixtureReply::json(type_response("page", json!([]))),
            FixtureReply::error("429 Too Many Requests", "private rate-limit detail"),
        ])
        .await;
        let result = object_update(
            &runtime(base_url, Duration::from_secs(1)),
            &object_update_tool().unwrap(),
            MutationAccess::Allowed,
            &input(json!({"space":SPACE_ID,"object_id":OBJECT_ID,"name":"After"})),
            &CancellationToken::new(),
        )
        .await;
        assert_eq!(result_code(&result), "conflict");
        assert_eq!(
            result_message(&result),
            ToolError::mutation_indeterminate().message()
        );
        assert!(
            !serde_json::to_string(&result)
                .unwrap()
                .contains("private rate-limit")
        );
        let requests = server.await.expect("429 patch fixture");
        assert_eq!(requests.len(), 3);
        assert_eq!(
            requests
                .iter()
                .filter(|request| request.starts_with("PATCH "))
                .count(),
            1
        );
        assert_eq!(
            requests
                .iter()
                .filter(|request| request.starts_with("GET "))
                .count(),
            2
        );
    }

    #[tokio::test]
    async fn patch_408_is_sent_once_and_stays_indeterminate_after_matching_recovery() {
        let current = object(SPACE_ID, OBJECT_ID, "Before", "body", "page");
        let updated = object(SPACE_ID, OBJECT_ID, "After", "body", "page");
        let (base_url, server) = fixture(vec![
            FixtureReply::json(current),
            FixtureReply::json(type_response("page", json!([]))),
            FixtureReply::error("408 Request Timeout", "private timeout detail"),
            FixtureReply::json(updated),
        ])
        .await;
        let result = object_update(
            &runtime_with_options(
                base_url,
                Duration::from_secs(1),
                ResponseLimits::default(),
                Some(fast_verify(1)),
            ),
            &object_update_tool().unwrap(),
            MutationAccess::Allowed,
            &input(json!({"space":SPACE_ID,"object_id":OBJECT_ID,"name":"After"})),
            &CancellationToken::new(),
        )
        .await;
        assert_eq!(result_code(&result), "conflict");
        assert_eq!(
            result_message(&result),
            ToolError::mutation_indeterminate().message()
        );
        assert!(
            !serde_json::to_string(&result)
                .unwrap()
                .contains("private timeout")
        );
        let requests = server.await.expect("408 patch fixture");
        assert_eq!(requests.len(), 4);
        assert_eq!(
            requests
                .iter()
                .filter(|request| request.starts_with("PATCH "))
                .count(),
            1
        );
        assert_eq!(
            requests
                .iter()
                .filter(|request| request.starts_with("GET "))
                .count(),
            3
        );
    }

    #[tokio::test]
    async fn patch_redirect_is_not_followed_and_remains_indeterminate_after_recovery() {
        let current = object(SPACE_ID, OBJECT_ID, "Before", "body", "page");
        let updated = object(SPACE_ID, OBJECT_ID, "After", "body", "page");
        let (redirect_target, target_done, target_server) = monitored_no_request_fixture().await;
        let (base_url, server) = fixture(vec![
            FixtureReply::json(current),
            FixtureReply::json(type_response("page", json!([]))),
            FixtureReply::redirect("307 Temporary Redirect", &redirect_target),
            FixtureReply::json(updated),
        ])
        .await;
        let result = object_update(
            &runtime_with_options(
                base_url,
                Duration::from_secs(1),
                ResponseLimits::default(),
                Some(fast_verify(1)),
            ),
            &object_update_tool().unwrap(),
            MutationAccess::Allowed,
            &input(json!({"space":SPACE_ID,"object_id":OBJECT_ID,"name":"After"})),
            &CancellationToken::new(),
        )
        .await;
        assert_eq!(result_code(&result), "conflict");
        assert_eq!(
            result_message(&result),
            ToolError::mutation_indeterminate().message()
        );
        let requests = server.await.expect("redirect patch fixture");
        assert_eq!(requests.len(), 4);
        assert_eq!(
            requests
                .iter()
                .filter(|request| request.starts_with("PATCH "))
                .count(),
            1
        );
        let _ = target_done.send(());
        assert!(target_server.await.expect("redirect target fixture"));
    }

    #[tokio::test]
    async fn post_dispatch_timeout_cancellation_and_shutdown_are_indeterminate() {
        for mode in 0..3 {
            let current = object(SPACE_ID, OBJECT_ID, "Before", "body", "page");
            let updated = object(SPACE_ID, OBJECT_ID, "After", "body", "page");
            let (base_url, server, patch_seen) = fixture_with_signal(
                vec![
                    FixtureReply::json(current),
                    FixtureReply::json(type_response("page", json!([]))),
                    FixtureReply::json(updated).delayed(Duration::from_secs(1)),
                ],
                Some(3),
            )
            .await;
            // The mode-0 deadline must expire after the PATCH is dispatched
            // but before the delayed reply. It needs real slack on both
            // sides: a too-tight budget can expire before the connection is
            // even established on a slow runner, and then the fixture waits
            // forever for a request that never comes.
            let runtime = runtime(
                base_url,
                if mode == 0 {
                    Duration::from_millis(250)
                } else {
                    Duration::from_secs(5)
                },
            );
            let cancellation = CancellationToken::new();
            let control_runtime = runtime.clone();
            let control_cancellation = cancellation.clone();
            let patch_seen = patch_seen.expect("patch signal");
            let control = tokio::spawn(async move {
                let _ = patch_seen.await;
                match mode {
                    1 => control_cancellation.cancel(),
                    2 => control_runtime.begin_shutdown(),
                    _ => {}
                }
            });
            let result = object_update(
                &runtime,
                &object_update_tool().unwrap(),
                MutationAccess::Allowed,
                &input(json!({"space":SPACE_ID,"object_id":OBJECT_ID,"name":"After"})),
                &cancellation,
            )
            .await;
            control.await.expect("control task");
            assert_eq!(result_code(&result), "conflict");
            assert_eq!(
                result_message(&result),
                ToolError::mutation_indeterminate().message()
            );
            // Bounded so a missing request fails the test instead of hanging
            // the whole suite at the fixture's accept loop.
            let requests = tokio::time::timeout(Duration::from_secs(60), server)
                .await
                .expect("post-dispatch control fixture deadline")
                .expect("post-dispatch control fixture");
            assert_eq!(requests.len(), 3);
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

        let (base_url, server) = no_request_fixture().await;
        let runtime = runtime(base_url, Duration::from_secs(1));
        runtime.begin_shutdown();
        let result = object_update(
            &runtime,
            &object_update_tool().unwrap(),
            MutationAccess::Allowed,
            &input(json!({"space":SPACE_ID,"object_id":OBJECT_ID,"name":"Changed"})),
            &CancellationToken::new(),
        )
        .await;
        assert_eq!(result_code(&result), "upstream");
        assert!(server.await.expect("shutdown fixture"));
    }

    #[tokio::test]
    async fn timeout_and_document_response_ceiling_fail_safely() {
        let changed = object(SPACE_ID, OBJECT_ID, "Changed", "body", "page");
        let (base_url, server) = fixture(vec![
            FixtureReply::json(changed.clone()).delayed(Duration::from_millis(500)),
        ])
        .await;
        let result = object_update(
            &runtime(base_url, Duration::from_millis(100)),
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
            chat_sse_event_bytes: 64,
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
