// any-mcp - bounded, workflow-oriented MCP server for Anytype
//
// SPDX-FileCopyrightText: 2026 Steve Schoettler
// SPDX-License-Identifier: Apache-2.0

//! Verified, process-idempotent single-object creation.

use std::{
    borrow::Cow,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use anytype::{
    objects::{Object, plain_markdown_representation},
    prelude::{VerifyConfig, verify_semantic},
};
use rmcp::{
    model::CallToolResult,
    schemars::{JsonSchema, Schema, SchemaGenerator, json_schema},
};
use serde::{Deserialize, Deserializer, Serialize, de};
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;

use crate::{
    create_idempotency::{
        Attempt, BeginAttempt, CreateDisposition, CreateExecution, DEFAULT_IDEMPOTENCY_CAPACITY,
        IdempotencyStore, finish_supervised_execution, wait_for_attempt,
    },
    domain::{
        BoundedText, DomainValueError, EntityId, MAX_DISPLAY_NAME_CHARS, ObjectId, ObjectSummary,
        SpaceId, TypeKey,
    },
    error::{ToolError, mutation_rejection_is_definitive},
    handler_support::{
        HandlerError, HandlerOperationError, MutationAccess, MutationProgress, MutationStage,
        execute_mutation_handler, require_mutation_access,
    },
    mutation_value::{
        MutationCompareError, MutationIcon, MutationProperties, MutationProperty,
        normalized_properties,
    },
    object_output::object_summary,
    protocol::{ToolProfile, WorkflowTool, workflow_tool},
    result::tool_error,
    runtime::{OperationContext, RuntimeContext},
    schema::SchemaContractError,
    validation::{Omittable, optional_non_null_schema},
};

pub use crate::create_idempotency::{CreateInputError, IdempotencyKey, MAX_IDEMPOTENCY_KEY_CHARS};

/// Maximum Unicode scalar values accepted in a resolvable reference.
pub const MAX_CREATE_REFERENCE_CHARS: usize = 512;
/// Maximum Unicode scalar values accepted in one document body.
pub const MAX_CREATE_BODY_CHARS: usize = 100_000;
type CreateBody = BoundedText<MAX_CREATE_BODY_CHARS>;

/// A nonempty bounded space, type, or template reference.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct CreateReference(String);

impl CreateReference {
    /// Validates a reference while retaining its exact matching spelling.
    pub fn new(value: impl Into<String>) -> Result<Self, CreateInputError> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(CreateInputError::Empty);
        }
        if value.chars().count() > MAX_CREATE_REFERENCE_CHARS {
            return Err(CreateInputError::TooLong);
        }
        Ok(Self(value))
    }

    /// Borrows the reference exactly as supplied.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for CreateReference {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::new(String::deserialize(deserializer)?).map_err(de::Error::custom)
    }
}

impl JsonSchema for CreateReference {
    fn schema_name() -> Cow<'static, str> {
        "CreateReference".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "minLength": 1,
            "maxLength": MAX_CREATE_REFERENCE_CHARS,
        })
    }
}

/// A nonempty bounded object name accepted specifically by create.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct CreateName(BoundedText<MAX_DISPLAY_NAME_CHARS>);

impl CreateName {
    /// Validates an exact nonempty object name without trimming it.
    pub fn new(value: impl Into<String>) -> Result<Self, CreateInputError> {
        let value = value.into();
        if value.is_empty() {
            return Err(CreateInputError::Empty);
        }
        BoundedText::new(value)
            .map(Self)
            .map_err(|_| CreateInputError::TooLong)
    }

    /// Borrows the exact bounded name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl<'de> Deserialize<'de> for CreateName {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::new(String::deserialize(deserializer)?).map_err(de::Error::custom)
    }
}

impl JsonSchema for CreateName {
    fn schema_name() -> Cow<'static, str> {
        "CreateName".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "minLength": 1,
            "maxLength": MAX_DISPLAY_NAME_CHARS,
        })
    }
}

/// Strict input for creating one object.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ObjectCreateInput {
    /// Unique space name or stable identifier.
    space: CreateReference,
    /// Type key, display name, or stable identifier.
    #[serde(rename = "type")]
    type_reference: CreateReference,
    /// Optional nonempty object name. Explicit null and an empty string are rejected.
    #[serde(default)]
    #[schemars(schema_with = "optional_name_schema")]
    name: Omittable<CreateName>,
    /// Optional complete Markdown body. Explicit null is rejected. Supported
    /// plain-line input is fingerprinted and verified in Anytype's exact
    /// canonical stored form, then its unescaped wire form is derived for the
    /// POST. Other Markdown and whitespace remain byte-exact.
    #[serde(default)]
    #[schemars(schema_with = "optional_body_schema")]
    body_markdown: Omittable<CreateBody>,
    /// Optional closed property assignments. Explicit null is rejected.
    #[serde(default)]
    #[schemars(schema_with = "optional_properties_schema")]
    properties: Omittable<MutationProperties>,
    /// Optional template id or unique name. Explicit null is rejected.
    #[serde(default)]
    #[schemars(schema_with = "optional_template_schema")]
    template: Omittable<CreateReference>,
    /// Optional closed icon. Explicit null is rejected.
    #[serde(default)]
    #[schemars(schema_with = "optional_icon_schema")]
    icon: Omittable<MutationIcon>,
    /// Optional process-lifetime retry key. Explicit null is rejected.
    #[serde(default)]
    #[schemars(schema_with = "optional_idempotency_schema")]
    idempotency_key: Omittable<IdempotencyKey>,
}

fn optional_name_schema(generator: &mut SchemaGenerator) -> Schema {
    optional_non_null_schema::<CreateName>(generator)
}

fn optional_body_schema(generator: &mut SchemaGenerator) -> Schema {
    optional_non_null_schema::<CreateBody>(generator)
}

fn optional_properties_schema(generator: &mut SchemaGenerator) -> Schema {
    optional_non_null_schema::<MutationProperties>(generator)
}

fn optional_template_schema(generator: &mut SchemaGenerator) -> Schema {
    optional_non_null_schema::<CreateReference>(generator)
}

fn optional_icon_schema(generator: &mut SchemaGenerator) -> Schema {
    optional_non_null_schema::<MutationIcon>(generator)
}

fn optional_idempotency_schema(generator: &mut SchemaGenerator) -> Schema {
    optional_non_null_schema::<IdempotencyKey>(generator)
}

/// Verified bounded result of `object_create`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ObjectCreateOutput {
    /// Verified metadata and canonical body resource link.
    object: ObjectSummary,
}

/// Builds the strict create contract consumed by the static catalog.
pub fn object_create_tool() -> Result<WorkflowTool<ObjectCreateOutput>, SchemaContractError> {
    workflow_tool::<ObjectCreateInput, ObjectCreateOutput>(
        "object_create",
        "Create one object, verify it by reading it back, and return only bounded metadata. Optional fields must be omitted rather than null. Supported plain-line bodies are fingerprinted and verified in Anytype's exact canonical stored form, with an unescaped POST wire form; other Markdown and whitespace remain byte-exact. A retry key deduplicates identical verified creates for this server process; timeout or cancellation can leave mutation outcome uncertain.",
        ToolProfile::Create,
    )
}

/// Stateful transport-neutral object-create handler.
#[derive(Clone)]
pub struct ObjectCreateHandlers {
    runtime: RuntimeContext,
    idempotency: Arc<IdempotencyStore>,
    verify_config: VerifyConfig,
    contract: WorkflowTool<ObjectCreateOutput>,
}

impl ObjectCreateHandlers {
    /// Creates a handler with the documented finite idempotency capacity.
    pub fn new(runtime: RuntimeContext) -> Result<Self, SchemaContractError> {
        Self::build(
            runtime,
            DEFAULT_IDEMPOTENCY_CAPACITY,
            VerifyConfig::default(),
        )
    }

    fn build(
        runtime: RuntimeContext,
        capacity: usize,
        verify_config: VerifyConfig,
    ) -> Result<Self, SchemaContractError> {
        Ok(Self {
            runtime,
            idempotency: Arc::new(IdempotencyStore::new(capacity)),
            verify_config,
            contract: object_create_tool()?,
        })
    }

    #[cfg(test)]
    fn with_idempotency_capacity(
        runtime: RuntimeContext,
        capacity: usize,
    ) -> Result<Self, SchemaContractError> {
        Self::build(runtime, capacity, test_verify_config())
    }

    #[cfg(test)]
    fn with_verify_config(
        runtime: RuntimeContext,
        verify_config: VerifyConfig,
    ) -> Result<Self, SchemaContractError> {
        Self::build(runtime, DEFAULT_IDEMPOTENCY_CAPACITY, verify_config)
    }

    /// Creates and verifies one object, applying the mutation gate before cache lookup or I/O.
    pub async fn object_create(
        &self,
        access: MutationAccess,
        input: ObjectCreateInput,
        cancellation: &CancellationToken,
    ) -> CallToolResult {
        if let Err(error) = require_mutation_access(access) {
            return tool_error(error.tool_error());
        }
        let normalized = match NormalizedCreate::new(input) {
            Ok(input) => input,
            Err(error) => return tool_error(error.tool_error()),
        };
        let Some(key) = normalized.idempotency_key.clone() else {
            let progress = MutationProgress::new();
            let execution = self
                .runtime
                .run_routed_invocation(
                    "object_create",
                    cancellation,
                    Box::pin(execute_create(
                        &self.runtime,
                        &self.contract,
                        normalized,
                        cancellation,
                        &progress,
                        &self.verify_config,
                    )),
                )
                .await;
            return match execution {
                Ok(execution) => execution.result,
                Err(failure) if failure.dispatched => {
                    tool_error(&ToolError::mutation_indeterminate())
                }
                Err(_) => tool_error(&ToolError::upstream()),
            };
        };

        let fingerprint = normalized.fingerprint();
        match self.idempotency.begin(key.clone(), fingerprint).await {
            BeginAttempt::Cached(result) => result,
            BeginAttempt::Indeterminate => tool_error(&ToolError::mutation_indeterminate()),
            BeginAttempt::Conflict => tool_error(&ToolError::conflict()),
            BeginAttempt::Full => tool_error(&ToolError::bounded_result()),
            BeginAttempt::Expired => tool_error(&ToolError::upstream()),
            BeginAttempt::Wait(attempt) => wait_for_attempt(attempt, cancellation).await,
            BeginAttempt::Lead(attempt) => {
                let runtime = self.runtime.clone();
                let contract = self.contract.clone();
                let store = self.idempotency.clone();
                let task_attempt = attempt.clone();
                let verify_config = self.verify_config.clone();
                self.runtime
                    .spawn_invocation_controller("object_create", move || async move {
                        supervise_create(
                            runtime,
                            contract,
                            store,
                            key,
                            task_attempt,
                            normalized,
                            verify_config,
                        )
                        .await;
                    });
                wait_for_attempt(attempt, cancellation).await
            }
        }
    }
}

async fn supervise_create(
    runtime: RuntimeContext,
    contract: WorkflowTool<ObjectCreateOutput>,
    store: Arc<IdempotencyStore>,
    key: IdempotencyKey,
    attempt: Arc<Attempt>,
    input: NormalizedCreate,
    verify_config: VerifyConfig,
) {
    let progress = attempt.progress();
    let supervisor_cancellation = CancellationToken::new();
    let execution_progress = progress.clone();
    let execution_runtime = runtime.clone();
    let execution_task = runtime.spawn_invocation_supervisor(async move {
        Box::pin(execute_create(
            &execution_runtime,
            &contract,
            input,
            &supervisor_cancellation,
            &execution_progress,
            &verify_config,
        ))
        .await
    });
    let execution = finish_supervised_execution(execution_task, &progress).await;
    store.finish(&key, &attempt, execution).await;
}

#[derive(Clone, Serialize)]
struct NormalizedCreate {
    space: CreateReference,
    type_reference: CreateReference,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<CreateName>,
    #[serde(skip_serializing_if = "Option::is_none")]
    body_markdown: Option<CreateBody>,
    #[serde(skip_serializing_if = "Option::is_none")]
    properties: Option<Vec<MutationProperty>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    template: Option<CreateReference>,
    #[serde(skip_serializing_if = "Option::is_none")]
    icon: Option<MutationIcon>,
    #[serde(skip)]
    idempotency_key: Option<IdempotencyKey>,
}

impl NormalizedCreate {
    fn new(input: ObjectCreateInput) -> Result<Self, HandlerError> {
        let properties = input
            .properties
            .as_ref()
            .map(normalized_properties)
            .transpose()
            .map_err(|_| HandlerError::new(ToolError::validation()))?;
        let body_markdown = input
            .body_markdown
            .as_ref()
            .cloned()
            .map(normalize_create_body)
            .transpose()?;
        Ok(Self {
            space: input.space,
            type_reference: input.type_reference,
            name: input.name.as_ref().cloned(),
            body_markdown,
            properties,
            template: input.template.as_ref().cloned(),
            icon: input.icon.as_ref().cloned(),
            idempotency_key: input.idempotency_key.as_ref().cloned(),
        })
    }

    fn fingerprint(&self) -> [u8; 32] {
        let fingerprint = CreateFingerprintV1 {
            domain: CREATE_FINGERPRINT_DOMAIN,
            version: 1,
            space: self.space.as_str(),
            type_reference: self.type_reference.as_str(),
            name: FingerprintField::from_option(self.name.as_ref()),
            body_markdown: FingerprintField::from_option(self.body_markdown.as_ref()),
            properties: FingerprintField::from_option(self.properties.as_ref()),
            template: FingerprintField::from_option(self.template.as_ref()),
            icon: FingerprintField::from_option(self.icon.as_ref()),
        };
        let encoded = serde_json::to_vec(&fingerprint)
            .expect("versioned normalized create fingerprint is serializable");
        let mut hasher = Sha256::new();
        hasher.update(CREATE_FINGERPRINT_DOMAIN.as_bytes());
        hasher.update([0]);
        hasher.update(encoded);
        hasher.finalize().into()
    }
}

fn normalize_create_body(body: CreateBody) -> Result<CreateBody, HandlerError> {
    let Some(representation) = plain_markdown_representation(body.as_str()) else {
        return Ok(body);
    };
    BoundedText::new(representation.canonical())
        .map_err(|_| HandlerError::new(ToolError::validation()))
}

const CREATE_FINGERPRINT_DOMAIN: &str = "any-mcp/object-create";

#[derive(Serialize)]
struct CreateFingerprintV1<'a> {
    domain: &'static str,
    version: u8,
    space: &'a str,
    type_reference: &'a str,
    name: FingerprintField<'a, CreateName>,
    body_markdown: FingerprintField<'a, CreateBody>,
    properties: FingerprintField<'a, Vec<MutationProperty>>,
    template: FingerprintField<'a, CreateReference>,
    icon: FingerprintField<'a, MutationIcon>,
}

#[derive(Serialize)]
#[serde(tag = "presence", content = "value", rename_all = "snake_case")]
enum FingerprintField<'a, T> {
    Absent,
    Present(&'a T),
}

impl<'a, T> FingerprintField<'a, T> {
    fn from_option(value: Option<&'a T>) -> Self {
        value.map_or(Self::Absent, Self::Present)
    }
}

async fn execute_create(
    runtime: &RuntimeContext,
    contract: &WorkflowTool<ObjectCreateOutput>,
    input: NormalizedCreate,
    cancellation: &CancellationToken,
    progress: &MutationProgress,
    verify_config: &VerifyConfig,
) -> CreateExecution {
    let client = runtime.client().clone();
    let definitive_rejection = Arc::new(AtomicBool::new(false));
    let operation_rejection = definitive_rejection.clone();
    let operation_progress = progress.clone();
    let verify_config = verify_config.clone();
    let result = execute_mutation_handler(
        runtime,
        contract,
        OperationContext::new("object_create"),
        cancellation,
        progress,
        async move {
            let resolved_space = client.resolve_space_id(input.space.as_str()).await?;
            let space_id = SpaceId::new(resolved_space).map_err(unsafe_upstream)?;
            let typ = client
                .resolve_type(space_id.as_str(), input.type_reference.as_str())
                .await?;
            let (type_id, type_key) = validate_resolved_type(&typ)?;
            validate_properties(&typ, input.properties.as_deref().unwrap_or_default())?;

            let template_id = if let Some(reference) = &input.template {
                let template = client
                    .resolve_template(space_id.as_str(), type_id.as_str(), reference.as_str())
                    .await?;
                Some(ObjectId::new(template.id).map_err(unsafe_upstream)?)
            } else {
                None
            };

            let mut request = client
                .new_object(space_id.as_str(), type_key.as_str())
                .no_verify();
            if let Some(name) = &input.name {
                request = request.name(name.as_str());
            }
            if let Some(body) = &input.body_markdown {
                let wire = plain_markdown_representation(body.as_str())
                    .map_or(Cow::Borrowed(body.as_str()), |representation| {
                        Cow::Owned(representation.wire().to_owned())
                    });
                request = request.body(wire.as_ref());
            }
            if let Some(icon) = &input.icon {
                request = request.icon(icon.to_anytype());
            }
            if let Some(template_id) = &template_id {
                request = request.template(template_id.as_str());
            }
            if let Some(properties) = &input.properties {
                for property in properties {
                    request = property.apply(request);
                }
            }

            // This is immediately before the first poll of the one and only
            // non-idempotent POST future.
            operation_progress.mark_dispatched(runtime)?;
            let created = match request.create().await {
                Ok(created) => created,
                Err(error) => {
                    if mutation_rejection_is_definitive(&error) {
                        operation_rejection.store(true, Ordering::Release);
                        return Err(error.into());
                    }
                    return Err(indeterminate_operation());
                }
            };
            validate_created_response(&created, &space_id, &type_id, &type_key)
                .map_err(|_| indeterminate_operation())?;
            let object_id =
                ObjectId::new(created.id.clone()).map_err(|_| indeterminate_operation())?;
            let created_matches = verify_object_semantics(
                &created, &object_id, &space_id, &type_id, &type_key, &input,
            )
            .map_err(|_| indeterminate_operation())?;
            let verified = verify_semantic(
                &verify_config,
                "object",
                object_id.as_str(),
                || client.object(space_id.as_str(), object_id.as_str()).get(),
                |object| {
                    verify_object_semantics(
                        object, &object_id, &space_id, &type_id, &type_key, &input,
                    )
                    .unwrap_or(false)
                },
            )
            .await
            .map_err(|_| indeterminate_operation())?;
            if !created_matches {
                return Err(indeterminate_operation());
            }
            let object = object_summary(&verified).map_err(|_| indeterminate_operation())?;
            Ok::<_, HandlerOperationError>(ObjectCreateOutput { object })
        },
        |output| async move { Ok(output) },
    )
    .await;
    let disposition = if result.is_error == Some(false) {
        CreateDisposition::Verified
    } else if definitive_rejection.load(Ordering::Acquire)
        || progress.stage() == MutationStage::PreDispatch
    {
        CreateDisposition::PreDispatchFailure
    } else {
        CreateDisposition::Indeterminate
    };
    CreateExecution {
        result,
        disposition,
    }
}

fn validate_properties(
    typ: &anytype::types::Type,
    requested: &[MutationProperty],
) -> Result<(), HandlerOperationError> {
    for property in requested {
        let matches: Vec<_> = typ
            .properties
            .iter()
            .filter(|schema| schema.key == property.key().as_str())
            .collect();
        if matches.len() > 1 {
            return Err(HandlerError::new(ToolError::upstream()).into());
        }
        if matches.len() != 1 || matches[0].format() != property.format() {
            return Err(HandlerError::new(ToolError::validation()).into());
        }
    }
    Ok(())
}

fn validate_resolved_type(
    typ: &anytype::types::Type,
) -> Result<(EntityId, TypeKey), HandlerOperationError> {
    let id = EntityId::new(typ.id.clone()).map_err(unsafe_upstream)?;
    let key = TypeKey::new(typ.key.clone()).map_err(unsafe_upstream)?;
    if typ.archived {
        return Err(HandlerError::new(ToolError::upstream()).into());
    }
    Ok((id, key))
}

fn validate_created_response(
    created: &Object,
    space_id: &SpaceId,
    type_id: &EntityId,
    type_key: &TypeKey,
) -> Result<(), HandlerOperationError> {
    let id = ObjectId::new(created.id.clone()).map_err(unsafe_upstream)?;
    let returned_space = SpaceId::new(created.space_id.clone()).map_err(unsafe_upstream)?;
    let returned_type = created
        .r#type
        .as_ref()
        .ok_or_else(|| HandlerOperationError::from(HandlerError::new(ToolError::upstream())))?;
    let returned_type_id = EntityId::new(returned_type.id.clone()).map_err(unsafe_upstream)?;
    let returned_type_key = TypeKey::new(returned_type.key.clone()).map_err(unsafe_upstream)?;
    if id.as_str().is_empty()
        || returned_space != *space_id
        || returned_type_id != *type_id
        || returned_type_key != *type_key
        || returned_type.archived
        || created.archived
    {
        return Err(HandlerError::new(ToolError::upstream()).into());
    }
    Ok(())
}

fn verify_object_semantics(
    object: &Object,
    expected_id: &ObjectId,
    expected_space: &SpaceId,
    expected_type_id: &EntityId,
    expected_type_key: &TypeKey,
    input: &NormalizedCreate,
) -> Result<bool, MutationCompareError> {
    let id = ObjectId::new(object.id.clone()).map_err(compare_domain_error)?;
    let space = SpaceId::new(object.space_id.clone()).map_err(compare_domain_error)?;
    let typ = object
        .r#type
        .as_ref()
        .ok_or(MutationCompareError::Malformed)?;
    let type_id = EntityId::new(typ.id.clone()).map_err(compare_domain_error)?;
    let type_key = TypeKey::new(typ.key.clone()).map_err(compare_domain_error)?;
    if object
        .name
        .as_ref()
        .is_some_and(|name| name.chars().count() > MAX_DISPLAY_NAME_CHARS)
        || object
            .markdown
            .as_ref()
            .is_some_and(|body| body.chars().count() > MAX_CREATE_BODY_CHARS)
    {
        return Err(MutationCompareError::Bounded);
    }
    if &id != expected_id
        || &space != expected_space
        || &type_id != expected_type_id
        || &type_key != expected_type_key
        || typ.archived
        || object.archived
        || input
            .name
            .as_ref()
            .is_some_and(|expected| object.name.as_deref() != Some(expected.as_str()))
        || input
            .body_markdown
            .as_ref()
            .is_some_and(|expected| object.markdown.as_deref() != Some(expected.as_str()))
    {
        return Ok(false);
    }
    if let Some(icon) = &input.icon
        && !icon.matches_returned(object.icon.as_ref())?
    {
        return Ok(false);
    }
    if let Some(properties) = &input.properties {
        for expected in properties {
            let mut matches = object
                .properties
                .iter()
                .filter(|actual| actual.key == expected.key().as_str());
            let actual = matches.next();
            if matches.next().is_some() {
                return Err(MutationCompareError::Malformed);
            }
            if !expected.matches_returned(actual.map(|property| &property.value))? {
                return Ok(false);
            }
        }
    }
    Ok(true)
}

fn compare_domain_error(error: DomainValueError) -> MutationCompareError {
    match error {
        DomainValueError::TooLong { .. } => MutationCompareError::Bounded,
        DomainValueError::Empty | DomainValueError::InvalidIdentifierCharacter => {
            MutationCompareError::Malformed
        }
    }
}

fn indeterminate_operation() -> HandlerOperationError {
    HandlerError::new(ToolError::mutation_indeterminate()).into()
}

fn unsafe_upstream(_: DomainValueError) -> HandlerOperationError {
    HandlerError::new(ToolError::upstream()).into()
}

#[cfg(test)]
fn test_verify_config() -> VerifyConfig {
    VerifyConfig {
        timeout: std::time::Duration::from_secs(1),
        initial_delay: std::time::Duration::ZERO,
        max_delay: std::time::Duration::ZERO,
        max_attempts: 3,
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use anytype::prelude::{AnytypeClient, ClientConfig, HttpCredentials, ResponseLimits};
    use serde_json::{Value, json};
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
        task::JoinHandle,
    };

    use super::*;
    use crate::runtime::StartupStatus;

    const SPACE_ID: &str =
        "bafyreid5fvqlnsobih2keakcxjrrlpmly6kf37klzjzen4ibfdgalcdp4y.2tq5w93cr6oe7";
    const TYPE_ID: &str = "bafyreid5fvqlnsobih2keakcxjrrlpmly6kf37klzjzen4ibfdgalcdp4y";
    const OBJECT_ID: &str = "bafyreie6n5l5nkbjal37su54cha4coy7qzuhrnajluzv5qd5jvtsrxkequ";
    const OTHER_OBJECT_ID: &str = "bafyreid5fvqlnsobih2keakcxjrrlpmly6kf37klzjzen4ibfdgalcdp4z";

    struct FixtureReply {
        status: &'static str,
        body: String,
        delay: Duration,
    }

    impl FixtureReply {
        fn json(value: Value) -> Self {
            Self {
                status: "200 OK",
                body: value.to_string(),
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
            .expect("bind create fixture");
        let address = listener.local_addr().expect("create fixture address");
        let server = tokio::spawn(async move {
            let mut requests = Vec::with_capacity(replies.len());
            for reply in replies {
                let Ok(Ok((mut socket, _))) =
                    tokio::time::timeout(Duration::from_secs(2), listener.accept()).await
                else {
                    break;
                };
                let request = read_request(&mut socket).await;
                requests.push(request);
                tokio::time::sleep(reply.delay).await;
                let response = format!(
                    "HTTP/1.1 {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    reply.status,
                    reply.body.len(),
                    reply.body,
                );
                let _ = socket.write_all(response.as_bytes()).await;
            }
            if let Ok(Ok((mut socket, _))) =
                tokio::time::timeout(Duration::from_millis(100), listener.accept()).await
            {
                requests.push(read_request(&mut socket).await);
            }
            requests
        });
        (format!("http://{address}"), server)
    }

    async fn read_request(socket: &mut tokio::net::TcpStream) -> String {
        let mut request = Vec::new();
        let mut expected = None;
        loop {
            let mut buffer = [0_u8; 2048];
            let read = socket.read(&mut buffer).await.expect("read create request");
            if read == 0 {
                break;
            }
            request.extend_from_slice(&buffer[..read]);
            if expected.is_none()
                && let Some(header_end) =
                    request.windows(4).position(|window| window == b"\r\n\r\n")
            {
                let body_start = header_end + 4;
                let headers = std::str::from_utf8(&request[..header_end]).expect("ASCII headers");
                let content_length = headers
                    .lines()
                    .find_map(|line| {
                        line.split_once(':').and_then(|(name, value)| {
                            name.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse::<usize>().expect("content length"))
                        })
                    })
                    .unwrap_or(0);
                expected = Some(body_start + content_length);
            }
            if expected.is_some_and(|length| request.len() >= length) {
                break;
            }
            assert!(
                request.len() <= 2 * 1024 * 1024,
                "fixture request exceeded bound"
            );
        }
        String::from_utf8(request).expect("request is utf-8")
    }

    async fn no_request_fixture() -> (String, JoinHandle<bool>) {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind no-request fixture");
        let address = listener.local_addr().expect("no-request fixture address");
        let task = tokio::spawn(async move {
            tokio::time::timeout(Duration::from_millis(150), listener.accept())
                .await
                .is_err()
        });
        (format!("http://{address}"), task)
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
            keystore_service: Some("object-create-test".to_owned()),
            app_name: "object-create-test".to_owned(),
            disable_cache: true,
            response_limits,
            ..ClientConfig::default()
        })
        .expect("create fixture client");
        client.set_api_key(HttpCredentials::new("fixture-token"));
        RuntimeContext::from_parts(
            client,
            4,
            timeout,
            StartupStatus {
                http_available: true,
                grpc_available: false,
            },
        )
    }

    fn type_value() -> Value {
        json!({
            "type": {
                "archived": false,
                "id": TYPE_ID,
                "key": "page",
                "layout": "basic",
                "name": "Page",
                "plural_name": "Pages",
                "properties": [
                    {"id":"prop-description", "key":"description", "name":"Description", "format":"text"},
                    {"id":"prop-done", "key":"done", "name":"Done", "format":"checkbox"}
                ]
            }
        })
    }

    fn object_value(object_id: &str, body: &str, description: &str) -> Value {
        json!({
            "object": {
                "archived": false,
                "icon": {"format":"emoji", "emoji":"📄"},
                "id": object_id,
                "layout": "basic",
                "markdown": body,
                "name": "Roadmap",
                "object": "object",
                "properties": [
                    {"id":"prop-description", "key":"description", "name":"Description", "format":"text", "text":description},
                    {"id":"prop-done", "key":"done", "name":"Done", "format":"checkbox", "checkbox":true}
                ],
                "space_id": SPACE_ID,
                "type": {
                    "archived": false,
                    "id": TYPE_ID,
                    "key":"page",
                    "layout":"basic",
                    "name":"Page",
                    "plural_name":"Pages",
                    "properties":[]
                }
            }
        })
    }

    fn all_property_type_value() -> Value {
        let formats = [
            ("checkbox", "checkbox"),
            ("date", "date"),
            ("email", "email"),
            ("files", "files"),
            ("multi_select", "multi_select"),
            ("number", "number"),
            ("objects", "objects"),
            ("phone", "phone"),
            ("select", "select"),
            ("text", "text"),
            ("url", "url"),
        ];
        json!({
            "type": {
                "archived": false,
                "id": TYPE_ID,
                "key": "page",
                "name": "Page",
                "properties": formats.into_iter().map(|(key, format)| json!({
                    "id": format!("prop-{key}"),
                    "key": key,
                    "name": key,
                    "format": format
                })).collect::<Vec<_>>()
            }
        })
    }

    fn all_property_object_value() -> Value {
        let tag = |id: &str| {
            json!({
                "id": id, "key": "tag", "name": "Tag", "color": "purple"
            })
        };
        json!({
            "object": {
                "archived": false,
                "icon": {"format":"icon", "name":"check", "color":"blue"},
                "id": OBJECT_ID,
                "markdown": "# All",
                "name": "All values",
                "properties": [
                    {"id":"prop-checkbox", "key":"checkbox", "name":"checkbox", "format":"checkbox", "checkbox":false},
                    {"id":"prop-date", "key":"date", "name":"date", "format":"date", "date":"2026-07-20T10:00:00Z"},
                    {"id":"prop-email", "key":"email", "name":"email", "format":"email", "email":"a@example.test"},
                    {"id":"prop-files", "key":"files", "name":"files", "format":"files", "files":[OTHER_OBJECT_ID,OBJECT_ID,OTHER_OBJECT_ID]},
                    {"id":"prop-multi_select", "key":"multi_select", "name":"multi_select", "format":"multi_select", "multi_select":[tag(OTHER_OBJECT_ID),tag(OBJECT_ID),tag(OBJECT_ID)]},
                    {"id":"prop-number", "key":"number", "name":"number", "format":"number", "number":1.0},
                    {"id":"prop-objects", "key":"objects", "name":"objects", "format":"objects", "objects":[OTHER_OBJECT_ID,OBJECT_ID,OBJECT_ID]},
                    {"id":"prop-phone", "key":"phone", "name":"phone", "format":"phone", "phone":"+1"},
                    {"id":"prop-select", "key":"select", "name":"select", "format":"select", "select":tag(OBJECT_ID)},
                    {"id":"prop-text", "key":"text", "name":"text", "format":"text", "text":"hello"},
                    {"id":"prop-url", "key":"url", "name":"url", "format":"url", "url":"https://example.test"}
                ],
                "space_id": SPACE_ID,
                "type": {
                    "archived": false,
                    "id": TYPE_ID,
                    "key":"page",
                    "name":"Page"
                }
            }
        })
    }

    fn input(key: Option<&str>) -> ObjectCreateInput {
        let mut value = json!({
            "space": SPACE_ID,
            "type": TYPE_ID,
            "name": "Roadmap",
            "body_markdown": "# Plan",
            "icon": {"format":"emoji", "emoji":"📄"},
            "properties": [
                {"format":"checkbox", "key":"done", "checkbox":true},
                {"format":"text", "key":"description", "text":"Q3"}
            ]
        });
        if let Some(key) = key {
            value["idempotency_key"] = json!(key);
        }
        serde_json::from_value(value).expect("valid create input")
    }

    fn input_with_body(key: Option<&str>, body: impl Into<String>) -> ObjectCreateInput {
        let mut request = input(key);
        request.body_markdown =
            Omittable::Present(BoundedText::new(body.into()).expect("bounded create body fixture"));
        request
    }

    fn success_replies() -> Vec<FixtureReply> {
        vec![
            FixtureReply::json(type_value()),
            FixtureReply::json(object_value(OBJECT_ID, "# Plan", "Q3")),
            FixtureReply::json(object_value(OBJECT_ID, "# Plan", "Q3")),
        ]
    }

    fn page(items: Vec<Value>, limit: u32, offset: u32) -> Value {
        let total = items.len();
        json!({
            "items":items,
            "pagination":{
                "has_more":false,
                "limit":limit,
                "offset":offset,
                "total":total
            }
        })
    }

    fn object_inner(object_id: &str, body: &str, description: &str) -> Value {
        object_value(object_id, body, description)["object"].clone()
    }

    fn result_code(result: &CallToolResult) -> &str {
        result
            .structured_content
            .as_ref()
            .and_then(|value| value.get("code"))
            .and_then(Value::as_str)
            .expect("error code")
    }

    fn request_body(request: &str) -> Value {
        let (_, body) = request.split_once("\r\n\r\n").expect("request body");
        serde_json::from_str(body).expect("JSON request body")
    }

    fn fingerprint_hex(input: ObjectCreateInput) -> String {
        NormalizedCreate::new(input)
            .expect("normalized create")
            .fingerprint()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    #[test]
    fn contract_is_strict_bounded_non_null_and_uses_create_annotations() {
        let contract = object_create_tool().expect("valid create tool");
        assert_eq!(
            serde_json::to_value(contract.as_tool().annotations.as_ref().unwrap()).unwrap(),
            json!({
                "readOnlyHint":false,
                "destructiveHint":false,
                "idempotentHint":false,
                "openWorldHint":false
            })
        );
        assert_eq!(contract.as_tool().name, "object_create");

        for field in [
            "name",
            "body_markdown",
            "properties",
            "template",
            "icon",
            "idempotency_key",
        ] {
            let mut value = json!({"space":SPACE_ID, "type":TYPE_ID});
            value[field] = Value::Null;
            assert!(
                serde_json::from_value::<ObjectCreateInput>(value).is_err(),
                "accepted explicit null {field}"
            );
        }
        assert!(
            serde_json::from_value::<ObjectCreateInput>(json!({
                "space":SPACE_ID, "type":TYPE_ID, "extra":true
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<ObjectCreateInput>(json!({
                "space":SPACE_ID,
                "type":TYPE_ID,
                "properties":[{"format":"text", "key":"../unsafe", "text":"x"}]
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<ObjectCreateInput>(json!({
                "space":SPACE_ID,
                "type":TYPE_ID,
                "icon":{"format":"file", "file":"../unsafe"}
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<ObjectCreateInput>(json!({
                "space":SPACE_ID,
                "type":TYPE_ID,
                "properties":[{"format":"number", "key":"score", "number":1e30}]
            }))
            .is_err()
        );

        let schema = serde_json::to_value(contract.as_tool().input_schema.as_ref()).unwrap();
        let encoded = schema.to_string();
        assert!(!encoded.contains("additionalProperties\":true"));
        assert!(!encoded.contains("body_markdown\":[\"null"));
    }

    #[test]
    fn create_name_body_and_shared_mutation_boundaries_are_exact() {
        assert!(CreateName::new("").is_err());
        assert!(CreateName::new("x".repeat(MAX_DISPLAY_NAME_CHARS)).is_ok());
        assert!(CreateName::new("x".repeat(MAX_DISPLAY_NAME_CHARS + 1)).is_err());

        let mut boundary = json!({
            "space": SPACE_ID,
            "type": TYPE_ID,
            "name": "named",
            "body_markdown": "界".repeat(MAX_CREATE_BODY_CHARS)
        });
        assert!(serde_json::from_value::<ObjectCreateInput>(boundary.clone()).is_ok());
        boundary["body_markdown"] = json!("界".repeat(MAX_CREATE_BODY_CHARS + 1));
        assert!(serde_json::from_value::<ObjectCreateInput>(boundary).is_err());

        assert!(
            serde_json::from_value::<ObjectCreateInput>(json!({
                "space": SPACE_ID, "type": TYPE_ID, "name": ""
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<ObjectCreateInput>(json!({
                "space": SPACE_ID,
                "type": TYPE_ID,
                "properties": [{
                    "format":"multi_select",
                    "key":"tags",
                    "multi_select": vec![OBJECT_ID; crate::mutation_value::MAX_MUTATION_IDS + 1]
                }]
            }))
            .is_err()
        );
    }

    #[test]
    fn plain_body_normalization_is_closed_stable_and_bounded() {
        for (requested, expected) in [
            ("", ""),
            ("alpha stable body", "alpha stable body   \n"),
            ("alpha café body", "alpha café body   \n"),
            ("alpha suffix_0", "alpha suffix\\_0   \n"),
            ("alpha stable body   \n", "alpha stable body   \n"),
            ("alpha suffix\\_0   \n", "alpha suffix\\_0   \n"),
        ] {
            let normalized = normalize_create_body(BoundedText::new(requested).unwrap()).unwrap();
            assert_eq!(normalized.as_str(), expected, "requested {requested:?}");
        }

        for exact in [
            " alpha",
            "alpha ",
            "alpha  ",
            "alpha\n",
            "alpha  \n",
            r"under\_score",
            r"\*escaped\*",
            "# Heading",
            "line one\nline two",
            "plain.",
        ] {
            let normalized = normalize_create_body(BoundedText::new(exact).unwrap()).unwrap();
            assert_eq!(normalized.as_str(), exact, "near-miss {exact:?}");
        }

        let expansion_overflow = "a".repeat(MAX_CREATE_BODY_CHARS - 3);
        assert!(
            normalize_create_body(BoundedText::new(expansion_overflow).unwrap()).is_err(),
            "canonical suffix must not exceed the stored-body ceiling"
        );
    }

    #[test]
    fn fingerprint_v1_is_domain_separated_golden_and_semantically_canonical() {
        assert_eq!(
            fingerprint_hex(input(Some("ignored-by-fingerprint"))),
            "ecded1746a5dc8fd6944c584ba354613fac55e457cc8997a0040d0d86e1334bf"
        );

        let left: ObjectCreateInput = serde_json::from_value(json!({
            "space":SPACE_ID,
            "type":TYPE_ID,
            "name":"Canonical",
            "properties":[
                {"format":"objects", "key":"objects", "objects":[OTHER_OBJECT_ID,OBJECT_ID,OTHER_OBJECT_ID]},
                {"format":"date", "key":"date", "date":"2026-07-20T12:00:00+02:00"},
                {"format":"number", "key":"number", "number":1.0}
            ]
        }))
        .unwrap();
        let right: ObjectCreateInput = serde_json::from_value(json!({
            "space":SPACE_ID,
            "type":TYPE_ID,
            "name":"Canonical",
            "properties":[
                {"format":"number", "key":"number", "number":1},
                {"format":"objects", "key":"objects", "objects":[OBJECT_ID,OTHER_OBJECT_ID]},
                {"format":"date", "key":"date", "date":"2026-07-20T10:00:00Z"}
            ]
        }))
        .unwrap();
        assert_eq!(fingerprint_hex(left), fingerprint_hex(right));

        let absent: ObjectCreateInput = serde_json::from_value(json!({
            "space":SPACE_ID, "type":TYPE_ID
        }))
        .unwrap();
        let present_empty: ObjectCreateInput = serde_json::from_value(json!({
            "space":SPACE_ID, "type":TYPE_ID, "properties":[]
        }))
        .unwrap();
        assert_ne!(fingerprint_hex(absent), fingerprint_hex(present_empty));

        let raw_plain = input_with_body(Some("same"), "alpha stable body");
        let canonical_plain = input_with_body(Some("same"), "alpha stable body   \n");
        assert_eq!(fingerprint_hex(raw_plain), fingerprint_hex(canonical_plain));

        let meaningful_newline = input_with_body(Some("same"), "alpha stable body\n");
        assert_ne!(
            fingerprint_hex(input_with_body(Some("same"), "alpha stable body")),
            fingerprint_hex(meaningful_newline)
        );
    }

    #[test]
    fn semantic_verification_accepts_only_shared_supported_missing_clears() {
        let input: ObjectCreateInput = serde_json::from_value(json!({
            "space":SPACE_ID,
            "type":TYPE_ID,
            "properties":[
                {"format":"text", "key":"text", "text":""},
                {"format":"multi_select", "key":"multi_select", "multi_select":[]},
                {"format":"files", "key":"files", "files":[]},
                {"format":"url", "key":"url", "url":""},
                {"format":"email", "key":"email", "email":""},
                {"format":"phone", "key":"phone", "phone":""},
                {"format":"objects", "key":"objects", "objects":[]}
            ]
        }))
        .unwrap();
        let normalized = NormalizedCreate::new(input).unwrap();
        let object: Object =
            serde_json::from_value(object_value(OBJECT_ID, "ignored", "extra")["object"].clone())
                .unwrap();
        assert!(
            verify_object_semantics(
                &object,
                &ObjectId::new(OBJECT_ID).unwrap(),
                &SpaceId::new(SPACE_ID).unwrap(),
                &EntityId::new(TYPE_ID).unwrap(),
                &TypeKey::new("page").unwrap(),
                &normalized,
            )
            .unwrap()
        );

        let unsupported: ObjectCreateInput = serde_json::from_value(json!({
            "space":SPACE_ID,
            "type":TYPE_ID,
            "properties":[{"format":"number", "key":"number", "number":0}]
        }))
        .unwrap();
        assert!(
            !verify_object_semantics(
                &object,
                &ObjectId::new(OBJECT_ID).unwrap(),
                &SpaceId::new(SPACE_ID).unwrap(),
                &EntityId::new(TYPE_ID).unwrap(),
                &TypeKey::new("page").unwrap(),
                &NormalizedCreate::new(unsupported).unwrap(),
            )
            .unwrap()
        );
    }

    #[tokio::test]
    async fn sends_exact_create_payload_then_verifies_by_get_and_returns_only_summary() {
        let (base_url, server) = fixture(success_replies()).await;
        let handlers =
            ObjectCreateHandlers::new(runtime(base_url, Duration::from_secs(2))).unwrap();
        let result = handlers
            .object_create(
                MutationAccess::Allowed,
                input(None),
                &CancellationToken::new(),
            )
            .await;

        let requests = server.await.expect("fixture task");
        assert_eq!(result.is_error, Some(false));
        assert_eq!(
            result.structured_content,
            Some(json!({
                "object": {
                    "id":OBJECT_ID,
                    "name":"Roadmap",
                    "type_key":"page",
                    "space_id":SPACE_ID,
                    "resource_uri":format!("anytype://spaces/{SPACE_ID}/objects/{OBJECT_ID}")
                }
            }))
        );
        assert_eq!(requests.len(), 3);
        assert!(requests[0].starts_with(&format!(
            "GET /v1/spaces/{SPACE_ID}/types/{TYPE_ID} HTTP/1.1\r\n"
        )));
        assert!(
            requests[1].starts_with(&format!("POST /v1/spaces/{SPACE_ID}/objects HTTP/1.1\r\n"))
        );
        assert_eq!(
            request_body(&requests[1]),
            json!({
                "type_key":"page",
                "name":"Roadmap",
                "body":"# Plan",
                "icon":{"format":"emoji", "emoji":"📄"},
                "properties":[
                    {"key":"description", "text":"Q3"},
                    {"key":"done", "checkbox":true}
                ]
            })
        );
        assert!(requests[2].starts_with(&format!(
            "GET /v1/spaces/{SPACE_ID}/objects/{OBJECT_ID} HTTP/1.1\r\n"
        )));
        let encoded = serde_json::to_string(&result).unwrap();
        assert!(!encoded.contains("# Plan"));
        assert!(!encoded.contains("description"));
    }

    #[tokio::test]
    async fn plain_body_normalizes_before_fingerprint_post_and_both_verifications() {
        let canonical = "alpha stable body   \n";
        let replies = vec![
            FixtureReply::json(type_value()),
            FixtureReply::json(object_value(OBJECT_ID, canonical, "Q3")),
            FixtureReply::json(object_value(OBJECT_ID, canonical, "Q3")),
        ];
        let (base_url, server) = fixture(replies).await;
        let handlers = ObjectCreateHandlers::with_verify_config(
            runtime(base_url, Duration::from_secs(2)),
            test_verify_config(),
        )
        .unwrap();

        let first = handlers
            .object_create(
                MutationAccess::Allowed,
                input_with_body(Some("plain-cohort"), "alpha stable body"),
                &CancellationToken::new(),
            )
            .await;
        let canonical_retry = handlers
            .object_create(
                MutationAccess::Allowed,
                input_with_body(Some("plain-cohort"), canonical),
                &CancellationToken::new(),
            )
            .await;

        assert_eq!(first.is_error, Some(false));
        assert_eq!(first, canonical_retry);
        let requests = server.await.expect("plain canonical create fixture");
        assert_eq!(requests.len(), 3);
        assert_eq!(
            requests
                .iter()
                .filter(|request| request.starts_with("POST "))
                .count(),
            1
        );
        assert_eq!(request_body(&requests[1])["body"], "alpha stable body");
    }

    #[tokio::test]
    async fn underscore_plain_body_replay_uses_one_unescaped_wire_form() {
        let raw = "alpha unique_0";
        let canonical = "alpha unique\\_0   \n";
        let replies = vec![
            FixtureReply::json(type_value()),
            FixtureReply::json(object_value(OBJECT_ID, canonical, "Q3")),
            FixtureReply::json(object_value(OBJECT_ID, canonical, "Q3")),
        ];
        let (base_url, server) = fixture(replies).await;
        let handlers = ObjectCreateHandlers::with_verify_config(
            runtime(base_url, Duration::from_secs(2)),
            test_verify_config(),
        )
        .unwrap();

        let first = handlers
            .object_create(
                MutationAccess::Allowed,
                input_with_body(Some("underscore-cohort"), raw),
                &CancellationToken::new(),
            )
            .await;
        let replay = handlers
            .object_create(
                MutationAccess::Allowed,
                input_with_body(Some("underscore-cohort"), canonical),
                &CancellationToken::new(),
            )
            .await;
        assert_eq!(first.is_error, Some(false));
        assert_eq!(first, replay);

        let requests = server.await.expect("underscore canonical create fixture");
        assert_eq!(requests.len(), 3);
        assert_eq!(request_body(&requests[1])["body"], raw);
    }

    #[tokio::test]
    async fn unproven_markdown_rewrites_remain_fixed_indeterminate_and_post_once() {
        let cases = [
            ("alpha stable body\n", "alpha stable body   \n"),
            ("alpha stable body  ", "alpha stable body   \n"),
            (
                r"under_score \*escaped\*",
                "under\\_score \\\\*escaped\\\\*   \n",
            ),
            (
                "# Heading\n\nline one\nline two",
                "Heading   \nline one\nline two   \n",
            ),
        ];
        for (index, (requested, returned)) in cases.into_iter().enumerate() {
            let mut replies = vec![
                FixtureReply::json(type_value()),
                FixtureReply::json(object_value(OBJECT_ID, returned, "Q3")),
            ];
            replies.extend(
                (0..3).map(|_| FixtureReply::json(object_value(OBJECT_ID, returned, "Q3"))),
            );
            let (base_url, server) = fixture(replies).await;
            let handlers = ObjectCreateHandlers::with_verify_config(
                runtime(base_url, Duration::from_secs(2)),
                test_verify_config(),
            )
            .unwrap();
            let result = handlers
                .object_create(
                    MutationAccess::Allowed,
                    input_with_body(Some(&format!("near-miss-{index}")), requested),
                    &CancellationToken::new(),
                )
                .await;

            assert_eq!(result_code(&result), "conflict", "requested {requested:?}");
            assert_eq!(
                result.structured_content.as_ref().unwrap()["message"],
                ToolError::mutation_indeterminate().message()
            );
            let requests = server.await.expect("near-miss canonical create fixture");
            assert_eq!(requests.len(), 5);
            assert_eq!(
                requests
                    .iter()
                    .filter(|request| request.starts_with("POST "))
                    .count(),
                1
            );
            assert_eq!(request_body(&requests[1])["body"], requested);
        }
    }

    #[tokio::test]
    async fn post_response_and_final_get_must_both_match_normalized_semantics() {
        let replies = vec![
            FixtureReply::json(type_value()),
            FixtureReply::json(object_value(OBJECT_ID, "# changed", "Q3")),
            FixtureReply::json(object_value(OBJECT_ID, "# Plan", "Q3")),
        ];
        let (base_url, server) = fixture(replies).await;
        let handlers = ObjectCreateHandlers::with_verify_config(
            runtime(base_url, Duration::from_secs(2)),
            test_verify_config(),
        )
        .unwrap();
        let result = handlers
            .object_create(
                MutationAccess::Allowed,
                input(Some("response-mismatch")),
                &CancellationToken::new(),
            )
            .await;

        assert_eq!(result_code(&result), "conflict");
        let requests = server.await.expect("response/final semantic fixture");
        assert_eq!(requests.len(), 3);
        assert_eq!(
            requests
                .iter()
                .filter(|request| request.starts_with("POST "))
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn plain_canonical_expansion_overflow_fails_before_io() {
        let (base_url, no_request) = no_request_fixture().await;
        let handlers = ObjectCreateHandlers::new(runtime(base_url, Duration::from_secs(1)))
            .expect("create handlers");
        let result = handlers
            .object_create(
                MutationAccess::Allowed,
                input_with_body(None, "a".repeat(MAX_CREATE_BODY_CHARS - 3)),
                &CancellationToken::new(),
            )
            .await;
        assert_eq!(result_code(&result), "validation");
        assert!(no_request.await.expect("no-request fixture"));
    }

    #[tokio::test]
    async fn all_shared_property_and_icon_forms_reach_one_canonical_create_payload() {
        let object = all_property_object_value();
        let (base_url, server) = fixture(vec![
            FixtureReply::json(all_property_type_value()),
            FixtureReply::json(object.clone()),
            FixtureReply::json(object),
        ])
        .await;
        let handlers = ObjectCreateHandlers::with_verify_config(
            runtime(base_url, Duration::from_secs(2)),
            test_verify_config(),
        )
        .unwrap();
        let request: ObjectCreateInput = serde_json::from_value(json!({
            "space": SPACE_ID,
            "type": TYPE_ID,
            "name": "All values",
            "body_markdown": "# All",
            "icon": {"format":"icon", "name":"check", "color":"blue"},
            "properties": [
                {"format":"url", "key":"url", "url":"https://example.test"},
                {"format":"text", "key":"text", "text":"hello"},
                {"format":"select", "key":"select", "select":OBJECT_ID},
                {"format":"phone", "key":"phone", "phone":"+1"},
                {"format":"objects", "key":"objects", "objects":[OTHER_OBJECT_ID,OBJECT_ID,OBJECT_ID]},
                {"format":"number", "key":"number", "number":1.0},
                {"format":"multi_select", "key":"multi_select", "multi_select":[OTHER_OBJECT_ID,OBJECT_ID,OTHER_OBJECT_ID]},
                {"format":"files", "key":"files", "files":[OTHER_OBJECT_ID,OBJECT_ID,OTHER_OBJECT_ID]},
                {"format":"email", "key":"email", "email":"a@example.test"},
                {"format":"date", "key":"date", "date":"2026-07-20T12:00:00+02:00"},
                {"format":"checkbox", "key":"checkbox", "checkbox":false}
            ]
        }))
        .unwrap();
        let result = handlers
            .object_create(MutationAccess::Allowed, request, &CancellationToken::new())
            .await;

        assert_eq!(result.is_error, Some(false));
        let requests = server.await.expect("all property fixture");
        assert_eq!(requests.len(), 3);
        assert_eq!(
            request_body(&requests[1]),
            json!({
                "type_key":"page",
                "name":"All values",
                "body":"# All",
                "icon":{"format":"icon", "name":"check", "color":"blue"},
                "properties":[
                    {"key":"checkbox", "checkbox":false},
                    {"key":"date", "date":"2026-07-20T10:00:00Z"},
                    {"key":"email", "email":"a@example.test"},
                    {"key":"files", "files":[OTHER_OBJECT_ID,OBJECT_ID]},
                    {"key":"multi_select", "multi_select":[OTHER_OBJECT_ID,OBJECT_ID]},
                    {"key":"number", "number":1},
                    {"key":"objects", "objects":[OTHER_OBJECT_ID,OBJECT_ID]},
                    {"key":"phone", "phone":"+1"},
                    {"key":"select", "select":OBJECT_ID},
                    {"key":"text", "text":"hello"},
                    {"key":"url", "url":"https://example.test"}
                ]
            })
        );

        for icon in [
            json!({"format":"emoji", "emoji":"📄"}),
            json!({"format":"file", "file":OBJECT_ID}),
            json!({"format":"icon", "name":"check", "color":"lime"}),
        ] {
            assert!(
                serde_json::from_value::<ObjectCreateInput>(json!({
                    "space":SPACE_ID, "type":TYPE_ID, "icon":icon
                }))
                .is_ok()
            );
        }
    }

    #[tokio::test]
    async fn named_space_type_and_template_are_bounded_and_revalidated_before_create() {
        let space = json!({
            "id":SPACE_ID,
            "name":"Workspace",
            "object":"space",
            "description":null,
            "icon":null,
            "gateway_url":null,
            "network_id":null
        });
        let type_item = type_value()["type"].clone();
        let mut template = object_inner(OTHER_OBJECT_ID, "Template", "Template property");
        template["name"] = json!("Starter");
        template["type"]["id"] = json!(OBJECT_ID);
        template["type"]["key"] = json!("template");
        let replies = vec![
            FixtureReply::json(page(vec![space], 100, 0)),
            FixtureReply::json(page(vec![type_item], 99, 0)),
            FixtureReply::json(page(vec![template.clone()], 99, 0)),
            FixtureReply::json(json!({"template":template})),
            FixtureReply::json(object_value(OBJECT_ID, "# Plan", "Q3")),
            FixtureReply::json(object_value(OBJECT_ID, "# Plan", "Q3")),
        ];
        let (base_url, server) = fixture(replies).await;
        let handlers =
            ObjectCreateHandlers::new(runtime(base_url, Duration::from_secs(2))).unwrap();
        let mut request = input(None);
        request.space = CreateReference::new("Workspace").unwrap();
        request.type_reference = CreateReference::new("Page").unwrap();
        request.template = Omittable::Present(CreateReference::new("Starter").unwrap());
        let result = handlers
            .object_create(MutationAccess::Allowed, request, &CancellationToken::new())
            .await;
        let requests = server.await.expect("named resolver fixture");

        assert_eq!(result.is_error, Some(false));
        assert_eq!(requests.len(), 6);
        assert!(requests[0].starts_with("GET /v1/spaces?"));
        assert!(requests[0].contains("limit=99"));
        assert!(requests[1].starts_with(&format!(
            "GET /v1/spaces/{SPACE_ID}/types?limit=99 HTTP/1.1\r\n"
        )));
        assert!(requests[2].starts_with(&format!(
            "GET /v1/spaces/{SPACE_ID}/types/{TYPE_ID}/templates"
        )));
        assert!(requests[3].starts_with(&format!(
            "GET /v1/spaces/{SPACE_ID}/types/{TYPE_ID}/templates/{OTHER_OBJECT_ID} HTTP/1.1\r\n"
        )));
        assert_eq!(
            request_body(&requests[4])["template_id"],
            json!(OTHER_OBJECT_ID)
        );
    }

    #[tokio::test]
    async fn type_and_property_schema_are_revalidated_once_before_post() {
        let mut wrong_format = type_value();
        wrong_format["type"]["properties"][0]["format"] = json!("number");
        let mut duplicate = type_value();
        let duplicate_property = duplicate["type"]["properties"][0].clone();
        duplicate["type"]["properties"]
            .as_array_mut()
            .unwrap()
            .push(duplicate_property);
        let mut archived = type_value();
        archived["type"]["archived"] = json!(true);
        let mut unsafe_id = type_value();
        unsafe_id["type"]["id"] = json!("../unsafe");

        for (response, expected) in [
            (wrong_format, "validation"),
            (duplicate, "upstream"),
            (archived, "upstream"),
            (unsafe_id, "upstream"),
        ] {
            let (base_url, server) = fixture(vec![FixtureReply::json(response)]).await;
            let handlers =
                ObjectCreateHandlers::new(runtime(base_url, Duration::from_secs(1))).unwrap();
            let result = handlers
                .object_create(
                    MutationAccess::Allowed,
                    input(None),
                    &CancellationToken::new(),
                )
                .await;
            assert_eq!(result_code(&result), expected);
            let requests = server.await.expect("type preflight fixture");
            assert_eq!(requests.len(), 1);
            assert!(requests[0].starts_with("GET "));
        }
    }

    #[tokio::test]
    async fn identical_sequential_and_concurrent_keyed_calls_create_once() {
        for concurrent in [false, true] {
            let mut replies = success_replies();
            if concurrent {
                replies[1] = std::mem::replace(
                    &mut replies[1],
                    FixtureReply::error("500 Internal Server Error", "unused"),
                )
                .delayed(Duration::from_millis(50));
            }
            let (base_url, server) = fixture(replies).await;
            let handlers =
                ObjectCreateHandlers::new(runtime(base_url, Duration::from_secs(2))).unwrap();
            let cancellation_a = CancellationToken::new();
            let cancellation_b = CancellationToken::new();
            let (first, second) = if concurrent {
                tokio::join!(
                    handlers.object_create(
                        MutationAccess::Allowed,
                        input(Some("same")),
                        &cancellation_a
                    ),
                    handlers.object_create(
                        MutationAccess::Allowed,
                        input(Some("same")),
                        &cancellation_b
                    )
                )
            } else {
                let first = handlers
                    .object_create(
                        MutationAccess::Allowed,
                        input(Some("same")),
                        &cancellation_a,
                    )
                    .await;
                let second = handlers
                    .object_create(
                        MutationAccess::Allowed,
                        input(Some("same")),
                        &cancellation_b,
                    )
                    .await;
                (first, second)
            };
            assert_eq!(first.is_error, Some(false));
            assert_eq!(first, second);
            assert_eq!(server.await.expect("dedupe fixture").len(), 3);
        }
    }

    #[tokio::test]
    async fn mismatched_key_reuse_and_read_only_cached_call_do_no_io() {
        let (base_url, server) = fixture(success_replies()).await;
        let handlers =
            ObjectCreateHandlers::new(runtime(base_url, Duration::from_secs(2))).unwrap();
        let first = handlers
            .object_create(
                MutationAccess::Allowed,
                input(Some("stable-key")),
                &CancellationToken::new(),
            )
            .await;
        assert_eq!(first.is_error, Some(false));

        let mut changed = input(Some("stable-key"));
        changed.name = Omittable::Present(CreateName::new("Changed").unwrap());
        let mismatch = handlers
            .object_create(MutationAccess::Allowed, changed, &CancellationToken::new())
            .await;
        assert_eq!(result_code(&mismatch), "conflict");
        assert_eq!(
            mismatch.structured_content.as_ref().unwrap()["message"],
            ToolError::conflict().message()
        );
        let read_only = handlers
            .object_create(
                MutationAccess::ReadOnly,
                input(Some("stable-key")),
                &CancellationToken::new(),
            )
            .await;
        assert_eq!(result_code(&read_only), "validation");
        assert_eq!(server.await.expect("no extra request fixture").len(), 3);
    }

    #[tokio::test]
    async fn predispatch_failure_is_retryable_but_verification_failure_is_terminal() {
        let mut replies = vec![FixtureReply::error(
            "500 Internal Server Error",
            "Bearer secret-key private failed body",
        )];
        replies.extend(success_replies());
        let (base_url, server) = fixture(replies).await;
        let handlers =
            ObjectCreateHandlers::new(runtime(base_url, Duration::from_secs(2))).unwrap();
        let failed = handlers
            .object_create(
                MutationAccess::Allowed,
                input(Some("retryable")),
                &CancellationToken::new(),
            )
            .await;
        assert_eq!(result_code(&failed), "upstream");
        let encoded = serde_json::to_string(&failed).unwrap();
        assert!(!encoded.contains("secret-key"));
        assert!(!encoded.contains("private failed"));
        let retried = handlers
            .object_create(
                MutationAccess::Allowed,
                input(Some("retryable")),
                &CancellationToken::new(),
            )
            .await;
        assert_eq!(retried.is_error, Some(false));
        assert_eq!(server.await.expect("failed retry fixture").len(), 4);

        let mut replies = vec![
            FixtureReply::json(type_value()),
            FixtureReply::error("403 Forbidden", "definitive rejection"),
        ];
        replies.extend(success_replies());
        let (base_url, server) = fixture(replies).await;
        let handlers =
            ObjectCreateHandlers::new(runtime(base_url, Duration::from_secs(2))).unwrap();
        let rejected = handlers
            .object_create(
                MutationAccess::Allowed,
                input(Some("rejected")),
                &CancellationToken::new(),
            )
            .await;
        assert_eq!(result_code(&rejected), "authentication");
        let retried = handlers
            .object_create(
                MutationAccess::Allowed,
                input(Some("rejected")),
                &CancellationToken::new(),
            )
            .await;
        assert_eq!(retried.is_error, Some(false));
        assert_eq!(server.await.expect("rejected retry fixture").len(), 5);

        let replies = vec![
            FixtureReply::json(type_value()),
            FixtureReply::json(object_value(OBJECT_ID, "# Plan", "Q3")),
            FixtureReply::json(object_value(OTHER_OBJECT_ID, "# Plan", "Q3")),
        ];
        let (base_url, server) = fixture(replies).await;
        let handlers =
            ObjectCreateHandlers::new(runtime(base_url, Duration::from_secs(2))).unwrap();
        let mismatch = handlers
            .object_create(
                MutationAccess::Allowed,
                input(Some("verify-retry")),
                &CancellationToken::new(),
            )
            .await;
        assert_eq!(result_code(&mismatch), "conflict");
        let retried = handlers
            .object_create(
                MutationAccess::Allowed,
                input(Some("verify-retry")),
                &CancellationToken::new(),
            )
            .await;
        assert_eq!(result_code(&retried), "conflict");
        assert_eq!(server.await.expect("verification retry fixture").len(), 3);
    }

    #[tokio::test]
    async fn post_429_and_408_are_terminal_indeterminate_and_sent_once() {
        // The HTTP timeout policy classifies a mutation 429 as indeterminate:
        // the server may have applied the write before rate-limiting the
        // response, so recovery starts with a fresh observation.
        let (base_url, server) = fixture(vec![
            FixtureReply::json(type_value()),
            FixtureReply::error("429 Too Many Requests", "private rate-limit body"),
        ])
        .await;
        let handlers =
            ObjectCreateHandlers::new(runtime(base_url, Duration::from_secs(2))).unwrap();
        let rejected = handlers
            .object_create(
                MutationAccess::Allowed,
                input(Some("post-429")),
                &CancellationToken::new(),
            )
            .await;
        assert_eq!(result_code(&rejected), "conflict");
        assert_eq!(
            rejected.structured_content.as_ref().unwrap()["message"],
            ToolError::mutation_indeterminate().message()
        );
        let requests = server.await.expect("429 create fixture");
        assert_eq!(requests.len(), 2);
        assert_eq!(
            requests
                .iter()
                .filter(|request| request.starts_with("POST "))
                .count(),
            1
        );

        // A matching object reread is available after the 408, but create has
        // no trustworthy returned id with which to select it. The keyed
        // terminal result must therefore remain fixed and perform no recovery
        // GET or second POST.
        let (base_url, server) = fixture(vec![
            FixtureReply::json(type_value()),
            FixtureReply::error("408 Request Timeout", "private timeout body"),
            FixtureReply::json(object_value(OBJECT_ID, "# Plan", "Q3")),
        ])
        .await;
        let handlers =
            ObjectCreateHandlers::new(runtime(base_url, Duration::from_secs(2))).unwrap();
        let first = handlers
            .object_create(
                MutationAccess::Allowed,
                input(Some("post-408")),
                &CancellationToken::new(),
            )
            .await;
        let retry = handlers
            .object_create(
                MutationAccess::Allowed,
                input(Some("post-408")),
                &CancellationToken::new(),
            )
            .await;
        assert_eq!(result_code(&first), "conflict");
        assert_eq!(first, retry);
        assert_eq!(
            first.structured_content.as_ref().unwrap()["message"],
            ToolError::mutation_indeterminate().message()
        );
        let requests = server.await.expect("408 create fixture");
        assert_eq!(requests.len(), 2);
        assert_eq!(
            requests
                .iter()
                .filter(|request| request.starts_with("POST "))
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn verification_retries_stale_and_transient_reads_but_posts_once() {
        let replies = vec![
            FixtureReply::json(type_value()),
            FixtureReply::json(object_value(OBJECT_ID, "# Plan", "Q3")),
            FixtureReply::json(object_value(OBJECT_ID, "# stale", "Q3")),
            FixtureReply::error("500 Internal Server Error", "private transient body"),
            FixtureReply::json(object_value(OBJECT_ID, "# Plan", "Q3")),
        ];
        let (base_url, server) = fixture(replies).await;
        let handlers = ObjectCreateHandlers::with_verify_config(
            runtime(base_url, Duration::from_secs(2)),
            test_verify_config(),
        )
        .unwrap();
        let result = handlers
            .object_create(
                MutationAccess::Allowed,
                input(Some("eventual")),
                &CancellationToken::new(),
            )
            .await;

        assert_eq!(result.is_error, Some(false));
        let requests = server.await.expect("eventual verification fixture");
        assert_eq!(requests.len(), 5);
        assert_eq!(
            requests
                .iter()
                .filter(|request| request.starts_with("POST "))
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn verification_exhaustion_is_indeterminate_for_first_unkeyed_call() {
        let mut replies = vec![
            FixtureReply::json(type_value()),
            FixtureReply::json(object_value(OBJECT_ID, "# Plan", "Q3")),
        ];
        replies
            .extend((0..3).map(|_| FixtureReply::json(object_value(OBJECT_ID, "# stale", "Q3"))));
        let (base_url, server) = fixture(replies).await;
        let handlers = ObjectCreateHandlers::with_verify_config(
            runtime(base_url, Duration::from_secs(2)),
            test_verify_config(),
        )
        .unwrap();
        let result = handlers
            .object_create(
                MutationAccess::Allowed,
                input(None),
                &CancellationToken::new(),
            )
            .await;

        assert_eq!(result_code(&result), "conflict");
        assert_eq!(
            result.structured_content.as_ref().unwrap()["message"],
            ToolError::mutation_indeterminate().message()
        );
        let requests = server.await.expect("verification exhaustion fixture");
        assert_eq!(requests.len(), 5);
        assert_eq!(
            requests
                .iter()
                .filter(|request| request.starts_with("POST "))
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn first_postdispatch_failures_are_fixed_and_key_retry_never_posts_twice() {
        let mut mismatched = object_value(OBJECT_ID, "# Plan", "Q3");
        mismatched["object"]["space_id"] = json!("../unsafe");
        let cases = vec![
            FixtureReply::error(
                "500 Internal Server Error",
                "Bearer private-server-secret post failed",
            ),
            FixtureReply {
                status: "200 OK",
                body: "{".to_owned(),
                delay: Duration::ZERO,
            },
            FixtureReply::json(mismatched),
        ];

        for (index, create_reply) in cases.into_iter().enumerate() {
            let (base_url, server) =
                fixture(vec![FixtureReply::json(type_value()), create_reply]).await;
            let handlers =
                ObjectCreateHandlers::new(runtime(base_url, Duration::from_secs(1))).unwrap();
            let key = format!("post-failure-{index}");
            let first = handlers
                .object_create(
                    MutationAccess::Allowed,
                    input(Some(&key)),
                    &CancellationToken::new(),
                )
                .await;
            let retry = handlers
                .object_create(
                    MutationAccess::Allowed,
                    input(Some(&key)),
                    &CancellationToken::new(),
                )
                .await;
            assert_eq!(result_code(&first), "conflict");
            assert_eq!(result_code(&retry), "conflict");
            let encoded = serde_json::to_string(&first).unwrap();
            assert!(!encoded.contains("private-server-secret"));
            assert!(!encoded.contains("Bearer"));
            let requests = server.await.expect("post failure fixture");
            assert_eq!(requests.len(), 2);
            assert_eq!(
                requests
                    .iter()
                    .filter(|request| request.starts_with("POST "))
                    .count(),
                1
            );
        }

        let (base_url, server) = fixture(vec![
            FixtureReply::json(type_value()),
            FixtureReply::json(object_value(OBJECT_ID, "# Plan", "Q3")),
        ])
        .await;
        let handlers = ObjectCreateHandlers::new(runtime_with_limits(
            base_url,
            Duration::from_secs(1),
            ResponseLimits {
                json_bytes: 1024 * 1024,
                document_bytes: 128,
                error_bytes: 1024,
                file_bytes: 1024,
                chat_sse_event_bytes: 1024,
            },
        ))
        .unwrap();
        let first = handlers
            .object_create(
                MutationAccess::Allowed,
                input(Some("post-oversize")),
                &CancellationToken::new(),
            )
            .await;
        let retry = handlers
            .object_create(
                MutationAccess::Allowed,
                input(Some("post-oversize")),
                &CancellationToken::new(),
            )
            .await;
        assert_eq!(result_code(&first), "conflict");
        assert_eq!(result_code(&retry), "conflict");
        assert_eq!(server.await.expect("post oversize fixture").len(), 2);
    }

    #[tokio::test]
    async fn supervisor_panic_and_abort_wake_cohorts_and_retain_by_stage() {
        let store = Arc::new(IdempotencyStore::new(4));
        let fingerprint = [7_u8; 32];
        let key = IdempotencyKey::new("panic-key").unwrap();
        let BeginAttempt::Lead(attempt) = store.begin(key.clone(), fingerprint).await else {
            panic!("first attempt must lead");
        };
        let waiter_attempt = attempt.clone();
        let waiter = tokio::spawn(async move {
            wait_for_attempt(waiter_attempt, &CancellationToken::new()).await
        });
        attempt.progress().mark_dispatched_for_test();
        let panic_task: tokio::task::JoinHandle<CreateExecution> =
            tokio::spawn(async { panic!("injected create panic") });
        let execution = finish_supervised_execution(panic_task, &attempt.progress()).await;
        store.finish(&key, &attempt, execution).await;
        assert_eq!(result_code(&waiter.await.unwrap()), "conflict");
        assert!(matches!(
            store.begin(key, fingerprint).await,
            BeginAttempt::Indeterminate
        ));

        let key = IdempotencyKey::new("abort-key").unwrap();
        let BeginAttempt::Lead(attempt) = store.begin(key.clone(), fingerprint).await else {
            panic!("first abort attempt must lead");
        };
        let waiter_attempt = attempt.clone();
        let waiter = tokio::spawn(async move {
            wait_for_attempt(waiter_attempt, &CancellationToken::new()).await
        });
        let abort_task: tokio::task::JoinHandle<CreateExecution> =
            tokio::spawn(std::future::pending());
        abort_task.abort();
        let execution = finish_supervised_execution(abort_task, &attempt.progress()).await;
        store.finish(&key, &attempt, execution).await;
        assert_eq!(result_code(&waiter.await.unwrap()), "upstream");
        assert!(matches!(
            store.begin(key, fingerprint).await,
            BeginAttempt::Lead(_)
        ));
    }

    #[tokio::test]
    async fn retained_key_capacity_fails_closed_without_growing_or_writing() {
        let (base_url, server) = fixture(success_replies()).await;
        let handlers = ObjectCreateHandlers::with_idempotency_capacity(
            runtime(base_url, Duration::from_secs(2)),
            1,
        )
        .unwrap();
        assert_eq!(
            handlers
                .object_create(
                    MutationAccess::Allowed,
                    input(Some("first")),
                    &CancellationToken::new()
                )
                .await
                .is_error,
            Some(false)
        );
        let full = handlers
            .object_create(
                MutationAccess::Allowed,
                input(Some("second")),
                &CancellationToken::new(),
            )
            .await;
        assert_eq!(result_code(&full), "bounded_result");
        assert_eq!(server.await.expect("capacity fixture").len(), 3);
    }

    #[tokio::test]
    async fn pre_cancel_timeout_permissions_and_document_cap_are_fixed_and_bounded() {
        let (base_url, no_request) = no_request_fixture().await;
        let handlers =
            ObjectCreateHandlers::new(runtime(base_url, Duration::from_secs(1))).unwrap();
        let cancelled = CancellationToken::new();
        cancelled.cancel();
        let result = handlers
            .object_create(MutationAccess::Allowed, input(None), &cancelled)
            .await;
        assert_eq!(result_code(&result), "upstream");
        assert!(no_request.await.expect("cancel no-request fixture"));

        let (base_url, server) = fixture(vec![
            FixtureReply::json(type_value()).delayed(Duration::from_millis(100)),
        ])
        .await;
        let handlers =
            ObjectCreateHandlers::new(runtime(base_url, Duration::from_millis(20))).unwrap();
        let result = handlers
            .object_create(
                MutationAccess::Allowed,
                input(Some("timeout")),
                &CancellationToken::new(),
            )
            .await;
        assert_eq!(result_code(&result), "upstream");
        assert_eq!(server.await.expect("timeout fixture").len(), 1);

        // The post-dispatch deadline must expire after the create POST is
        // dispatched but before the delayed reply; both margins need real
        // slack on a slow runner (a too-tight budget expires during the
        // preceding type fetch and reports upstream instead of conflict).
        let (base_url, server) = fixture(vec![
            FixtureReply::json(type_value()),
            FixtureReply::json(object_value(OBJECT_ID, "# Plan", "Q3"))
                .delayed(Duration::from_secs(1)),
        ])
        .await;
        let handlers =
            ObjectCreateHandlers::new(runtime(base_url, Duration::from_millis(250))).unwrap();
        let result = handlers
            .object_create(
                MutationAccess::Allowed,
                input(Some("post-timeout")),
                &CancellationToken::new(),
            )
            .await;
        assert_eq!(result_code(&result), "conflict");
        let retry = handlers
            .object_create(
                MutationAccess::Allowed,
                input(Some("post-timeout")),
                &CancellationToken::new(),
            )
            .await;
        assert_eq!(result_code(&retry), "conflict");
        assert_eq!(server.await.expect("post timeout fixture").len(), 2);

        let (base_url, server) = fixture(vec![
            FixtureReply::json(type_value()),
            FixtureReply::json(object_value(OBJECT_ID, "# Plan", "Q3"))
                .delayed(Duration::from_secs(1)),
            FixtureReply::json(object_value(OBJECT_ID, "# Plan", "Q3")),
        ])
        .await;
        let handlers =
            ObjectCreateHandlers::new(runtime(base_url, Duration::from_secs(5))).unwrap();
        let cancellation = CancellationToken::new();
        let cancel = cancellation.clone();
        let cancellation_task = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(250)).await;
            cancel.cancel();
        });
        let result = handlers
            .object_create(
                MutationAccess::Allowed,
                input(Some("post-cancel")),
                &cancellation,
            )
            .await;
        cancellation_task.await.expect("cancellation task");
        assert_eq!(result_code(&result), "conflict");
        tokio::time::sleep(Duration::from_millis(20)).await;
        let retry = handlers
            .object_create(
                MutationAccess::Allowed,
                input(Some("post-cancel")),
                &CancellationToken::new(),
            )
            .await;
        assert_eq!(retry.is_error, Some(false));
        assert_eq!(server.await.expect("post cancel fixture").len(), 3);

        for (status, code) in [
            ("401 Unauthorized", "authentication"),
            ("403 Forbidden", "authentication"),
            ("404 Not Found", "not_found"),
        ] {
            let (base_url, server) = fixture(vec![FixtureReply::error(
                status,
                "Bearer credential private response body",
            )])
            .await;
            let handlers =
                ObjectCreateHandlers::new(runtime(base_url, Duration::from_secs(1))).unwrap();
            let result = handlers
                .object_create(
                    MutationAccess::Allowed,
                    input(None),
                    &CancellationToken::new(),
                )
                .await;
            assert_eq!(result_code(&result), code);
            let encoded = serde_json::to_string(&result).unwrap();
            assert!(!encoded.contains("Bearer"));
            assert!(!encoded.contains("private response"));
            assert_eq!(server.await.expect("permission fixture").len(), 1);
        }

        let (base_url, server) = fixture(vec![FixtureReply::json(type_value())]).await;
        let handlers = ObjectCreateHandlers::new(runtime_with_limits(
            base_url,
            Duration::from_secs(1),
            ResponseLimits {
                json_bytes: 64,
                document_bytes: 64,
                error_bytes: 64,
                file_bytes: 64,
                chat_sse_event_bytes: 64,
            },
        ))
        .unwrap();
        let result = handlers
            .object_create(
                MutationAccess::Allowed,
                input(None),
                &CancellationToken::new(),
            )
            .await;
        assert_eq!(result_code(&result), "bounded_result");
        assert_eq!(server.await.expect("response cap fixture").len(), 1);
    }
}
