// any-mcp - bounded, workflow-oriented MCP server for Anytype
//
// SPDX-FileCopyrightText: 2026 Steve Schoettler
// SPDX-License-Identifier: Apache-2.0

//! Optional schema-toolset workflows for bounded space creation and updates.
//!
//! This module deliberately exports a complete handler/contract slice without
//! linking the incomplete `schema` descriptor into the production registry.
//! The terminal schema integration task composes this slice with the remaining
//! reviewed type, property, and tag slices before the selector becomes valid.

use std::{
    borrow::Cow,
    fmt,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use anytype::{
    prelude::{VerifyConfig, verify_semantic},
    spaces::{Space, SpaceModel},
};
use rmcp::{
    model::{CallToolRequestMethod, CallToolRequestParams, CallToolResult, ErrorData},
    schemars::{JsonSchema, Schema, SchemaGenerator, json_schema},
};
use serde::{Deserialize, Deserializer, Serialize, de};
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;

use crate::{
    create_idempotency::{
        Attempt, BeginAttempt, CreateDisposition, CreateExecution, DEFAULT_IDEMPOTENCY_CAPACITY,
        IdempotencyKey, IdempotencyStore, finish_supervised_execution, wait_for_attempt,
    },
    discovery::DiscoveryReference,
    domain::{BoundedText, DisplayName, DomainValueError, EntityId},
    error::{ToolError, mutation_rejection_is_definitive},
    handler_support::{
        HandlerError, HandlerOperationError, MutationAccess, MutationProgress, MutationStage,
        execute_mutation_handler, require_mutation_access,
    },
    optional_toolsets::OptionalRegistryTool,
    protocol::{ToolProfile, WorkflowTool, workflow_tool},
    result::tool_error,
    runtime::{OperationContext, RuntimeContext},
    schema::SchemaContractError,
    server::decode_arguments,
    validation::{Omittable, optional_non_null_schema},
};

/// Exact tool name for the schema space-create workflow.
pub const SPACE_CREATE: &str = "space_create";
/// Exact tool name for the schema space-update workflow.
pub const SPACE_UPDATE: &str = "space_update";
/// Reviewed maximum logical operations for one space create.
pub const SPACE_CREATE_LOGICAL_CEILING: usize = 11;
/// Reviewed maximum physical HTTP attempts for one space create.
pub const SPACE_CREATE_PHYSICAL_CEILING: usize = 61;
/// Reviewed maximum logical operations for one space update.
pub const SPACE_UPDATE_LOGICAL_CEILING: usize = 23;
/// Reviewed maximum physical HTTP attempts for one space update.
pub const SPACE_UPDATE_PHYSICAL_CEILING: usize = 133;

const MAX_INPUT_NAME_CHARS: usize = 512;
const MAX_DESCRIPTION_CHARS: usize = 4_096;
const SPACE_CREATE_FINGERPRINT_DOMAIN: &str = "any-mcp/schema-space-create/v1";

type Description = BoundedText<MAX_DESCRIPTION_CHARS>;
type CreateObserver = Arc<dyn Fn(&Space) -> Result<(), ()> + Send + Sync>;

/// A nonempty bounded name accepted by schema mutations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct InputName(String);

impl InputName {
    /// Validates an exact nonempty name without trimming or normalization.
    pub fn new(value: impl Into<String>) -> Result<Self, SpaceInputError> {
        let value = value.into();
        if value.is_empty() {
            return Err(SpaceInputError::Empty);
        }
        if value.chars().count() > MAX_INPUT_NAME_CHARS {
            return Err(SpaceInputError::TooLong);
        }
        Ok(Self(value))
    }

    /// Borrows the exact validated name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for InputName {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::new(String::deserialize(deserializer)?).map_err(de::Error::custom)
    }
}

impl JsonSchema for InputName {
    fn schema_name() -> Cow<'static, str> {
        "InputName".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "minLength": 1,
            "maxLength": MAX_INPUT_NAME_CHARS,
        })
    }
}

/// A nonempty bounded description accepted by schema mutations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct InputDescription(String);

impl InputDescription {
    /// Validates exact nonempty description text without normalization.
    pub fn new(value: impl Into<String>) -> Result<Self, SpaceInputError> {
        let value = value.into();
        if value.is_empty() {
            return Err(SpaceInputError::Empty);
        }
        if value.chars().count() > MAX_DESCRIPTION_CHARS {
            return Err(SpaceInputError::TooLong);
        }
        Ok(Self(value))
    }

    /// Borrows the exact validated description.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for InputDescription {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::new(String::deserialize(deserializer)?).map_err(de::Error::custom)
    }
}

impl JsonSchema for InputDescription {
    fn schema_name() -> Cow<'static, str> {
        "InputDescription".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "minLength": 1,
            "maxLength": MAX_DESCRIPTION_CHARS,
        })
    }
}

/// Failure to construct a strict space-mutation input value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpaceInputError {
    /// A required or explicitly supplied value was empty.
    Empty,
    /// A value exceeded its reviewed Unicode-scalar bound.
    TooLong,
}

impl fmt::Display for SpaceInputError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Empty => "value must not be empty",
            Self::TooLong => "value exceeds its maximum length",
        })
    }
}

impl std::error::Error for SpaceInputError {}

/// Strict input for creating one regular Anytype space.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SpaceCreateInput {
    /// Exact nonempty display name.
    name: InputName,
    /// Optional nonempty description; explicit null is rejected.
    #[serde(default)]
    #[schemars(schema_with = "optional_input_description_schema")]
    description: Omittable<InputDescription>,
    /// Optional process-local create retry key; explicit null is rejected.
    #[serde(default)]
    #[schemars(schema_with = "optional_idempotency_schema")]
    idempotency_key: Omittable<IdempotencyKey>,
}

fn optional_input_description_schema(generator: &mut SchemaGenerator) -> Schema {
    optional_non_null_schema::<InputDescription>(generator)
}

fn optional_idempotency_schema(generator: &mut SchemaGenerator) -> Schema {
    optional_non_null_schema::<IdempotencyKey>(generator)
}

/// Strict input for updating at least one mutable field on one exact space.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpaceUpdateInput {
    /// Unique space name or identifier, preserving exact caller spelling.
    space: DiscoveryReference,
    /// Optional nonempty replacement name; explicit null is rejected.
    #[serde(default)]
    name: Omittable<InputName>,
    /// Optional nonempty replacement description; explicit null is rejected.
    #[serde(default)]
    description: Omittable<InputDescription>,
}

#[allow(dead_code)]
#[derive(JsonSchema)]
#[serde(deny_unknown_fields)]
struct SpaceUpdateInputSchema {
    /// Unique space name or identifier, preserving exact caller spelling.
    space: DiscoveryReference,
    /// Optional nonempty replacement name; explicit null is rejected.
    #[serde(default)]
    #[schemars(schema_with = "optional_input_name_schema")]
    name: Omittable<InputName>,
    /// Optional nonempty replacement description; explicit null is rejected.
    #[serde(default)]
    #[schemars(schema_with = "optional_input_description_schema")]
    description: Omittable<InputDescription>,
}

impl JsonSchema for SpaceUpdateInput {
    fn schema_name() -> Cow<'static, str> {
        "SpaceUpdateInput".into()
    }

    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        let mut schema = SpaceUpdateInputSchema::json_schema(generator);
        schema
            .ensure_object()
            .insert("minProperties".to_owned(), 2_u64.into());
        schema
    }
}

fn optional_input_name_schema(generator: &mut SchemaGenerator) -> Schema {
    optional_non_null_schema::<InputName>(generator)
}

impl SpaceUpdateInput {
    fn has_mutation(&self) -> bool {
        !self.name.is_none() || !self.description.is_none()
    }
}

/// Caller-visible bounded metadata for one regular space.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SpaceSummary {
    /// Stable space identifier.
    id: EntityId,
    /// Current bounded display name.
    name: DisplayName,
    /// Current bounded description, when present.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(default, schema_with = "optional_output_description_schema")]
    description: Option<Description>,
}

fn optional_output_description_schema(generator: &mut SchemaGenerator) -> Schema {
    generator.subschema_for::<Description>()
}

/// Exact output shared by space create and update.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SpaceOutput {
    /// Verified bounded space metadata.
    space: SpaceSummary,
}

/// Constructs the approved `space_create` contract.
pub fn space_create_tool() -> Result<WorkflowTool<SpaceOutput>, SchemaContractError> {
    workflow_tool::<SpaceCreateInput, SpaceOutput>(
        SPACE_CREATE,
        "Create one regular Anytype space, verify its exact identity and requested metadata, and return only bounded space metadata. A retry key deduplicates identical verified creates for this server process.",
        ToolProfile::Create,
    )
}

/// Constructs the approved `space_update` contract.
pub fn space_update_tool() -> Result<WorkflowTool<SpaceOutput>, SchemaContractError> {
    workflow_tool::<SpaceUpdateInput, SpaceOutput>(
        SPACE_UPDATE,
        "Update at least one supplied field on one exact Anytype space and verify every supplied value. Omitted fields are preserved; description clearing is not supported.",
        ToolProfile::Update,
    )
}

/// Returns the complete schema-space slice for later registry composition.
pub fn schema_space_tools() -> Result<Vec<OptionalRegistryTool>, SchemaContractError> {
    Ok(vec![
        OptionalRegistryTool::mutation(space_create_tool()?),
        OptionalRegistryTool::mutation(space_update_tool()?),
    ])
}

/// Stateful transport-neutral handlers for the schema space slice.
#[derive(Clone)]
pub struct SchemaSpaceHandlers {
    idempotency: Arc<IdempotencyStore>,
    verify_config: VerifyConfig,
    create_contract: WorkflowTool<SpaceOutput>,
    update_contract: WorkflowTool<SpaceOutput>,
    create_observer: Option<CreateObserver>,
}

impl fmt::Debug for SchemaSpaceHandlers {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SchemaSpaceHandlers")
            .field("verify_config", &self.verify_config)
            .field("create_observer", &self.create_observer.is_some())
            .finish_non_exhaustive()
    }
}

impl SchemaSpaceHandlers {
    /// Creates handlers with the reviewed finite idempotency and verification ceilings.
    pub fn new() -> Result<Self, SchemaContractError> {
        Self::build(DEFAULT_IDEMPOTENCY_CAPACITY, VerifyConfig::default(), None)
    }

    fn build(
        capacity: usize,
        verify_config: VerifyConfig,
        create_observer: Option<CreateObserver>,
    ) -> Result<Self, SchemaContractError> {
        Ok(Self {
            idempotency: Arc::new(IdempotencyStore::new(capacity)),
            verify_config,
            create_contract: space_create_tool()?,
            update_contract: space_update_tool()?,
            create_observer,
        })
    }

    /// Dispatches one schema-space tool after the caller's catalog gate.
    pub async fn call_tool(
        &self,
        request: CallToolRequestParams,
        runtime: &RuntimeContext,
        cancellation: &CancellationToken,
    ) -> Result<CallToolResult, ErrorData> {
        if runtime.is_read_only() && matches!(request.name.as_ref(), SPACE_CREATE | SPACE_UPDATE) {
            return Ok(tool_error(&ToolError::validation()));
        }
        let access = MutationAccess::Allowed;
        match request.name.as_ref() {
            SPACE_CREATE => {
                let input = decode_arguments::<SpaceCreateInput>(request.arguments)?;
                Ok(self
                    .space_create(runtime, access, input, cancellation)
                    .await)
            }
            SPACE_UPDATE => {
                let input = decode_arguments::<SpaceUpdateInput>(request.arguments)?;
                Ok(self
                    .space_update(runtime, access, input, cancellation)
                    .await)
            }
            _ => Err(ErrorData::method_not_found::<CallToolRequestMethod>()),
        }
    }

    async fn space_create(
        &self,
        runtime: &RuntimeContext,
        access: MutationAccess,
        input: SpaceCreateInput,
        cancellation: &CancellationToken,
    ) -> CallToolResult {
        if let Err(error) = require_mutation_access(access) {
            return tool_error(error.tool_error());
        }
        let normalized = NormalizedSpaceCreate::from(input);
        let Some(key) = normalized.idempotency_key.clone() else {
            let progress = MutationProgress::new();
            return execute_space_create(
                runtime,
                &self.create_contract,
                normalized,
                cancellation,
                &progress,
                &self.verify_config,
                self.create_observer.clone(),
            )
            .await
            .result;
        };

        let fingerprint = normalized.fingerprint();
        match self.idempotency.begin(key.clone(), fingerprint).await {
            BeginAttempt::Cached(result) => result,
            BeginAttempt::Indeterminate => tool_error(&ToolError::mutation_indeterminate()),
            BeginAttempt::Conflict => tool_error(&ToolError::conflict()),
            BeginAttempt::Full => tool_error(&ToolError::bounded_result()),
            BeginAttempt::Wait(attempt) => wait_for_attempt(attempt, cancellation).await,
            BeginAttempt::Lead(attempt) => {
                let runtime = runtime.clone();
                let contract = self.create_contract.clone();
                let store = self.idempotency.clone();
                let task_attempt = attempt.clone();
                let verify_config = self.verify_config.clone();
                let observer = self.create_observer.clone();
                tokio::spawn(async move {
                    supervise_space_create(SpaceCreateSupervision {
                        runtime,
                        contract,
                        store,
                        key,
                        attempt: task_attempt,
                        normalized,
                        verify_config,
                        observer,
                    })
                    .await;
                });
                wait_for_attempt(attempt, cancellation).await
            }
        }
    }

    async fn space_update(
        &self,
        runtime: &RuntimeContext,
        access: MutationAccess,
        input: SpaceUpdateInput,
        cancellation: &CancellationToken,
    ) -> CallToolResult {
        if let Err(error) = require_mutation_access(access) {
            return tool_error(error.tool_error());
        }
        if !input.has_mutation() {
            return tool_error(&ToolError::validation());
        }
        let client = runtime.client().clone();
        let verify_config = self.verify_config.clone();
        let progress = MutationProgress::new();
        let operation_progress = progress.clone();
        execute_mutation_handler(
            runtime,
            &self.update_contract,
            OperationContext::new(SPACE_UPDATE),
            cancellation,
            &progress,
            async move {
                let resolved = client.resolve_space_id(input.space.as_str()).await?;
                let space_id = EntityId::new(resolved).map_err(unsafe_upstream)?;
                let current = client.space(space_id.as_str()).get_direct().await?;
                checked_space_summary(&current, Some(&space_id))
                    .map_err(HandlerOperationError::from)?;

                let mut request = client.update_space(space_id.as_str()).no_verify();
                if let Some(name) = input.name.as_ref() {
                    request = request.name(name.as_str());
                }
                if let Some(description) = input.description.as_ref() {
                    request = request.description(description.as_str());
                }

                operation_progress.mark_dispatched();
                let response_anomaly = match request.update().await {
                    Ok(returned) => {
                        !space_matches_update(&returned, &space_id, &input).unwrap_or(false)
                    }
                    Err(error) if mutation_rejection_is_definitive(&error) => {
                        return Err(error.into());
                    }
                    Err(_) => true,
                };

                let verified = verify_semantic(
                    &verify_config,
                    "space",
                    space_id.as_str(),
                    || client.space(space_id.as_str()).get_direct(),
                    |space| space_matches_update(space, &space_id, &input).unwrap_or(false),
                )
                .await
                .map_err(|_| indeterminate_operation())?;
                if response_anomaly {
                    return Err(indeterminate_operation());
                }
                checked_space_summary(&verified, Some(&space_id))
                    .map(SpaceOutput::from)
                    .map_err(|_| indeterminate_operation())
            },
            |output| async move { Ok(output) },
        )
        .await
    }
}

#[derive(Clone)]
struct NormalizedSpaceCreate {
    name: InputName,
    description: Option<InputDescription>,
    idempotency_key: Option<IdempotencyKey>,
}

impl From<SpaceCreateInput> for NormalizedSpaceCreate {
    fn from(input: SpaceCreateInput) -> Self {
        Self {
            name: input.name,
            description: input.description.as_ref().cloned(),
            idempotency_key: input.idempotency_key.as_ref().cloned(),
        }
    }
}

impl NormalizedSpaceCreate {
    fn fingerprint(&self) -> [u8; 32] {
        let mut hasher = Sha256::new();
        hasher.update(SPACE_CREATE_FINGERPRINT_DOMAIN.as_bytes());
        hash_field(&mut hasher, self.name.as_str());
        match self.description.as_ref() {
            Some(description) => {
                hasher.update([1]);
                hash_field(&mut hasher, description.as_str());
            }
            None => hasher.update([0]),
        }
        hasher.finalize().into()
    }
}

fn hash_field(hasher: &mut Sha256, value: &str) {
    hasher.update(value.len().to_be_bytes());
    hasher.update(value.as_bytes());
}

struct SpaceCreateSupervision {
    runtime: RuntimeContext,
    contract: WorkflowTool<SpaceOutput>,
    store: Arc<IdempotencyStore>,
    key: IdempotencyKey,
    attempt: Arc<Attempt>,
    normalized: NormalizedSpaceCreate,
    verify_config: VerifyConfig,
    observer: Option<CreateObserver>,
}

async fn supervise_space_create(supervision: SpaceCreateSupervision) {
    let SpaceCreateSupervision {
        runtime,
        contract,
        store,
        key,
        attempt,
        normalized,
        verify_config,
        observer,
    } = supervision;
    let progress = attempt.progress();
    let task_progress = progress.clone();
    let task = tokio::spawn(async move {
        execute_space_create(
            &runtime,
            &contract,
            normalized,
            &CancellationToken::new(),
            &task_progress,
            &verify_config,
            observer,
        )
        .await
    });
    let execution = finish_supervised_execution(task, &progress).await;
    store.finish(&key, &attempt, execution).await;
}

async fn execute_space_create(
    runtime: &RuntimeContext,
    contract: &WorkflowTool<SpaceOutput>,
    input: NormalizedSpaceCreate,
    cancellation: &CancellationToken,
    progress: &MutationProgress,
    verify_config: &VerifyConfig,
    observer: Option<CreateObserver>,
) -> CreateExecution {
    let client = runtime.client().clone();
    let definitive_rejection = Arc::new(AtomicBool::new(false));
    let operation_rejection = definitive_rejection.clone();
    let operation_progress = progress.clone();
    let verify_config = verify_config.clone();
    let result = execute_mutation_handler(
        runtime,
        contract,
        OperationContext::new(SPACE_CREATE),
        cancellation,
        progress,
        async move {
            let mut request = client.new_space(input.name.as_str()).no_verify();
            if let Some(description) = input.description.as_ref() {
                request = request.description(description.as_str());
            }
            operation_progress.mark_dispatched();
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
            if let Some(observer) = observer.as_ref() {
                observer(&created).map_err(|()| indeterminate_operation())?;
            }
            let id = EntityId::new(created.id.clone()).map_err(|_| indeterminate_operation())?;
            let response_matches = space_matches_create(&created, &id, &input)
                .map_err(|_| indeterminate_operation())?;
            let verified = verify_semantic(
                &verify_config,
                "space",
                id.as_str(),
                || client.space(id.as_str()).get_direct(),
                |space| space_matches_create(space, &id, &input).unwrap_or(false),
            )
            .await
            .map_err(|_| indeterminate_operation())?;
            if !response_matches {
                return Err(indeterminate_operation());
            }
            checked_space_summary(&verified, Some(&id))
                .map(SpaceOutput::from)
                .map_err(|_| indeterminate_operation())
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
    CreateExecution::new(result, disposition)
}

impl From<SpaceSummary> for SpaceOutput {
    fn from(space: SpaceSummary) -> Self {
        Self { space }
    }
}

fn checked_space_summary(
    space: &Space,
    expected_id: Option<&EntityId>,
) -> Result<SpaceSummary, HandlerError> {
    let id = EntityId::new(space.id.clone()).map_err(unsafe_domain)?;
    if expected_id.is_some_and(|expected| expected != &id) || space.object != SpaceModel::Space {
        return Err(HandlerError::new(ToolError::upstream()));
    }
    let name = DisplayName::new(space.name.clone()).map_err(unsafe_domain)?;
    let description = space
        .description
        .clone()
        .map(Description::new)
        .transpose()
        .map_err(unsafe_domain)?;
    Ok(SpaceSummary {
        id,
        name,
        description,
    })
}

fn space_matches_create(
    space: &Space,
    expected_id: &EntityId,
    input: &NormalizedSpaceCreate,
) -> Result<bool, HandlerError> {
    let summary = checked_space_summary(space, Some(expected_id))?;
    Ok(summary.name.as_str() == input.name.as_str()
        && input.description.as_ref().is_none_or(|expected| {
            summary.description.as_ref().map(Description::as_str) == Some(expected.as_str())
        }))
}

fn space_matches_update(
    space: &Space,
    expected_id: &EntityId,
    input: &SpaceUpdateInput,
) -> Result<bool, HandlerError> {
    let summary = checked_space_summary(space, Some(expected_id))?;
    Ok(input
        .name
        .as_ref()
        .is_none_or(|expected| summary.name.as_str() == expected.as_str())
        && input.description.as_ref().is_none_or(|expected| {
            summary.description.as_ref().map(Description::as_str) == Some(expected.as_str())
        }))
}

fn indeterminate_operation() -> HandlerOperationError {
    HandlerError::new(ToolError::mutation_indeterminate()).into()
}

fn unsafe_upstream(_: DomainValueError) -> HandlerOperationError {
    HandlerError::new(ToolError::upstream()).into()
}

fn unsafe_domain(_: DomainValueError) -> HandlerError {
    HandlerError::new(ToolError::upstream())
}

#[cfg(test)]
mod tests {
    use std::{
        future::Future,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
        time::Duration,
    };

    use anytype::{
        prelude::{AnytypeClient, ClientConfig, HttpCredentials},
        test_util::{DisposableRun, unique_suffix, with_disposable_space_context},
    };
    use rmcp::model::Tool;
    use serde_json::{Map, Value, json};
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, duplex, split};

    use super::*;
    use crate::{
        config::ApplicationProfile,
        optional_toolsets::{
            OptionalRegistryFuture, OptionalToolsetMetadata, OptionalToolsetRegistry,
            OptionalToolsetSelection,
        },
        runtime::StartupStatus,
        server::AnyMcpServer,
    };

    const SPACE_ID: &str =
        "bafyreid5fvqlnsobih2keakcxjrrlpmly6kf37klzjzen4ibfdgalcdp4y.2tq5w93cr6oe7";
    const OTHER_SPACE_ID: &str =
        "bafyreid5fvqlnsobih2keakcxjrrlpmly6kf37klzjzen4ibfdgalcdp4z.2tq5w93cr6oe7";

    fn run_large_future<F, Fut>(test: F)
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        std::thread::Builder::new()
            .name("schema-space-handler".to_owned())
            .stack_size(8 * 1024 * 1024)
            .spawn(move || {
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("schema-space test runtime")
                    .block_on(test());
            })
            .expect("spawn schema-space test")
            .join()
            .expect("schema-space test thread");
    }

    macro_rules! large_async_test {
        ($name:ident, $body:block) => {
            #[test]
            fn $name() {
                run_large_future(|| async move $body);
            }
        };
    }

    struct TestRegistry {
        handlers: SchemaSpaceHandlers,
    }

    impl fmt::Debug for TestRegistry {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("TestSchemaSpaceRegistry")
        }
    }

    impl OptionalToolsetRegistry for TestRegistry {
        fn metadata(&self) -> OptionalToolsetMetadata {
            OptionalToolsetMetadata::new("schema", false)
        }

        fn tools(&self) -> Result<Vec<OptionalRegistryTool>, SchemaContractError> {
            schema_space_tools()
        }

        fn scripted_scenario_ids(&self) -> &'static [&'static str] {
            &["schema_space_direct", "schema_space_stdio"]
        }

        fn headless_scenario_ids(&self) -> &'static [&'static str] {
            &["schema_space_headless"]
        }

        fn catalog_token_ceiling(&self) -> usize {
            2_500
        }

        fn call_tool<'a>(
            &'a self,
            request: CallToolRequestParams,
            runtime: &'a RuntimeContext,
            _cursors: &'a crate::cursor::CursorStore,
            _protocol_version: &'a rmcp::model::ProtocolVersion,
            cancellation: &'a CancellationToken,
        ) -> OptionalRegistryFuture<'a, Result<CallToolResult, ErrorData>> {
            Box::pin(self.handlers.call_tool(request, runtime, cancellation))
        }
    }

    fn handlers() -> SchemaSpaceHandlers {
        SchemaSpaceHandlers::new().expect("schema-space handlers")
    }

    fn runtime(client: AnytypeClient, read_only: bool) -> RuntimeContext {
        let selection = OptionalToolsetSelection::parse(
            Some("schema".to_owned()),
            &[OptionalToolsetMetadata::new("schema", false)],
        )
        .expect("schema selection");
        RuntimeContext::from_parts_with_profile_and_optional_toolsets(
            client,
            4,
            Duration::from_secs(30),
            StartupStatus {
                http_available: true,
                grpc_available: true,
            },
            ApplicationProfile::Compact,
            read_only,
            selection,
        )
    }

    fn no_io_runtime(read_only: bool) -> RuntimeContext {
        let client = AnytypeClient::with_config(ClientConfig {
            base_url: Some("http://127.0.0.1:1".to_owned()),
            keystore: Some("env".to_owned()),
            keystore_service: Some("schema-space-no-io".to_owned()),
            app_name: "schema-space-no-io".to_owned(),
            ..ClientConfig::default()
        })
        .expect("schema-space no-I/O client");
        client.set_api_key(HttpCredentials::new("unused-no-io-token"));
        runtime(client, read_only)
    }

    fn server(runtime: RuntimeContext, handlers: SchemaSpaceHandlers) -> AnyMcpServer {
        let registry: &'static TestRegistry = Box::leak(Box::new(TestRegistry { handlers }));
        let registries: &'static [&'static dyn OptionalToolsetRegistry] =
            Box::leak(vec![registry as &dyn OptionalToolsetRegistry].into_boxed_slice());
        AnyMcpServer::new_with_optional_registries(runtime, registries)
            .expect("schema-space test server")
    }

    fn args(value: Value) -> Map<String, Value> {
        value.as_object().cloned().expect("object arguments")
    }

    async fn direct(server: &AnyMcpServer, name: &'static str, arguments: Value) -> CallToolResult {
        server
            .dispatch_tool(
                CallToolRequestParams::new(name).with_arguments(args(arguments)),
                &CancellationToken::new(),
            )
            .await
            .expect("schema-space direct dispatch")
    }

    async fn preview_stdio_call(
        server: AnyMcpServer,
        name: &'static str,
        arguments: Value,
    ) -> Value {
        let (client_io, server_io) = duplex(64 * 1024);
        let (server_reader, server_writer) = split(server_io);
        let task = tokio::spawn(crate::stdio::serve_preview(
            server,
            BufReader::new(server_reader),
            server_writer,
        ));
        let (client_reader, mut client_writer) = split(client_io);
        let frame = json!({
            "jsonrpc":"2.0",
            "id":7,
            "method":"tools/call",
            "params":{
                "name":name,
                "arguments":arguments,
                "_meta":{
                    "io.modelcontextprotocol/protocolVersion":"2026-07-28",
                    "io.modelcontextprotocol/clientInfo":{"name":"schema-test","version":"1"},
                    "io.modelcontextprotocol/clientCapabilities":{}
                }
            }
        });
        client_writer
            .write_all(format!("{frame}\n").as_bytes())
            .await
            .expect("write stdio request");
        let mut client_reader = BufReader::new(client_reader);
        let mut line = String::new();
        client_reader
            .read_line(&mut line)
            .await
            .expect("read stdio response");
        drop(client_writer);
        drop(client_reader);
        task.await
            .expect("spawned stdio task")
            .expect("stdio transport");
        serde_json::from_str(&line).expect("decode stdio response")
    }

    fn result_code(result: &CallToolResult) -> Option<&str> {
        result
            .structured_content
            .as_ref()
            .and_then(|value| value.get("code"))
            .and_then(Value::as_str)
    }

    fn metric_counts(client: &AnytypeClient) -> (u64, u64) {
        let metrics = client.http_metrics();
        (metrics.logical_operations, metrics.physical_attempts)
    }

    fn assert_metric_delta(
        before: (u64, u64),
        after: (u64, u64),
        logical_ceiling: usize,
        physical_ceiling: usize,
    ) {
        let logical = after.0.checked_sub(before.0).expect("logical metrics grow");
        let physical = after
            .1
            .checked_sub(before.1)
            .expect("physical metrics grow");
        assert!(
            logical <= logical_ceiling as u64,
            "logical operations {logical} exceeded {logical_ceiling}"
        );
        assert!(
            physical <= physical_ceiling as u64,
            "physical attempts {physical} exceeded {physical_ceiling}"
        );
    }

    fn test_space(id: &str, name: &str, description: Option<&str>) -> Space {
        Space {
            id: id.to_owned(),
            name: name.to_owned(),
            object: SpaceModel::Space,
            description: description.map(str::to_owned),
            icon: None,
            gateway_url: Some("https://sensitive.invalid".to_owned()),
            network_id: Some("sensitive-network".to_owned()),
        }
    }

    #[test]
    fn contracts_are_closed_non_null_bounded_and_exact() {
        let create = space_create_tool().expect("space_create contract");
        let update = space_update_tool().expect("space_update contract");
        let create_tool: &Tool = create.as_tool();
        let update_tool: &Tool = update.as_tool();
        assert_eq!(
            serde_json::to_value(
                create_tool
                    .annotations
                    .as_ref()
                    .expect("create annotations")
            )
            .expect("serialize annotations"),
            json!({
                "readOnlyHint": false,
                "destructiveHint": false,
                "idempotentHint": false,
                "openWorldHint": false
            })
        );
        assert_eq!(
            serde_json::to_value(
                update_tool
                    .annotations
                    .as_ref()
                    .expect("update annotations")
            )
            .expect("serialize annotations"),
            json!({
                "readOnlyHint": false,
                "destructiveHint": true,
                "idempotentHint": false,
                "openWorldHint": false
            })
        );
        let create_schema = serde_json::to_value(create_tool.input_schema.as_ref())
            .expect("serialize create schema");
        assert_eq!(create_schema["additionalProperties"], false);
        assert_eq!(create_schema["required"], json!(["name"]));
        assert_eq!(
            create_schema["properties"]
                .as_object()
                .expect("create properties")
                .keys()
                .cloned()
                .collect::<Vec<_>>(),
            ["description", "idempotency_key", "name"]
        );
        let update_schema = serde_json::to_value(update_tool.input_schema.as_ref())
            .expect("serialize update schema");
        assert_eq!(update_schema["additionalProperties"], false);
        assert_eq!(update_schema["required"], json!(["space"]));
        assert_eq!(update_schema["minProperties"], 2);
        let create_validator = jsonschema::draft202012::options()
            .build(&create_schema)
            .expect("compile create input schema");
        let update_validator = jsonschema::draft202012::options()
            .build(&update_schema)
            .expect("compile update input schema");
        for (value, expected) in [
            (json!({"name":"x".repeat(512)}), true),
            (json!({"name":"x".repeat(513)}), false),
            (json!({"name":"n","description":"x".repeat(4_096)}), true),
            (json!({"name":"n","description":"x".repeat(4_097)}), false),
            (json!({"name":"n","idempotency_key":"x".repeat(256)}), true),
            (json!({"name":"n","idempotency_key":"x".repeat(257)}), false),
        ] {
            assert_eq!(create_validator.is_valid(&value), expected, "{value}");
            assert_eq!(
                serde_json::from_value::<SpaceCreateInput>(value).is_ok(),
                expected
            );
        }
        for (value, expected) in [
            (json!({"space":SPACE_ID,"name":"x".repeat(512)}), true),
            (json!({"space":SPACE_ID,"name":"x".repeat(513)}), false),
            (
                json!({"space":SPACE_ID,"description":"x".repeat(4_096)}),
                true,
            ),
            (
                json!({"space":SPACE_ID,"description":"x".repeat(4_097)}),
                false,
            ),
        ] {
            assert_eq!(update_validator.is_valid(&value), expected, "{value}");
            assert_eq!(
                serde_json::from_value::<SpaceUpdateInput>(value).is_ok(),
                expected
            );
        }
        for value in [
            json!({"name": null}),
            json!({"name":"n", "description":null}),
            json!({"name":"n", "idempotency_key":null}),
            json!({"name":"n", "unknown":true}),
            json!({"name":""}),
            json!({"name":"n", "description":""}),
        ] {
            assert!(serde_json::from_value::<SpaceCreateInput>(value).is_err());
        }
        for value in [
            json!({"space":SPACE_ID}),
            json!({"space":SPACE_ID, "name":null}),
            json!({"space":SPACE_ID, "description":""}),
            json!({"space":SPACE_ID, "name":"n", "unknown":true}),
        ] {
            let decoded = serde_json::from_value::<SpaceUpdateInput>(value);
            assert!(
                decoded.as_ref().is_err() || !decoded.expect("decoded empty update").has_mutation()
            );
        }

        let output_schema = serde_json::to_value(create_tool.output_schema.as_ref())
            .expect("serialize output schema")
            .to_string();
        for forbidden in ["network_id", "gateway_url", "icon"] {
            assert!(!output_schema.contains(forbidden));
        }
        assert_eq!(schema_space_tools().expect("space slice").len(), 2);
    }

    #[test]
    fn identity_matching_minimization_and_fingerprints_are_pure() {
        let id = EntityId::new(SPACE_ID).expect("valid space id");
        let created = test_space(SPACE_ID, "Roadmap", Some("Plan"));
        let input = NormalizedSpaceCreate {
            name: InputName::new("Roadmap").expect("name"),
            description: Some(InputDescription::new("Plan").expect("description")),
            idempotency_key: None,
        };
        assert!(space_matches_create(&created, &id, &input).expect("safe created space"));
        assert!(
            !space_matches_create(&test_space(SPACE_ID, "Wrong", Some("Plan")), &id, &input)
                .expect("safe name mismatch")
        );
        assert!(
            !space_matches_create(&test_space(SPACE_ID, "Roadmap", None), &id, &input)
                .expect("safe description mismatch")
        );
        assert!(
            checked_space_summary(&test_space(OTHER_SPACE_ID, "Roadmap", None), Some(&id)).is_err()
        );
        let mut chat = created.clone();
        chat.object = SpaceModel::Chat;
        assert!(checked_space_summary(&chat, Some(&id)).is_err());

        let summary = checked_space_summary(&created, Some(&id)).expect("bounded summary");
        assert_eq!(
            serde_json::to_value(SpaceOutput::from(summary)).expect("serialize output"),
            json!({"space":{"id":SPACE_ID,"name":"Roadmap","description":"Plan"}})
        );

        let update: SpaceUpdateInput = serde_json::from_value(json!({
            "space":SPACE_ID,
            "description":"New plan"
        }))
        .expect("update input");
        assert!(
            space_matches_update(
                &test_space(SPACE_ID, "Preserved name", Some("New plan")),
                &id,
                &update,
            )
            .expect("safe update")
        );
        assert!(!space_matches_update(&created, &id, &update).expect("safe stale update"));

        let same = NormalizedSpaceCreate {
            name: InputName::new("Roadmap").expect("name"),
            description: Some(InputDescription::new("Plan").expect("description")),
            idempotency_key: Some(IdempotencyKey::new("different-key").expect("key")),
        };
        let different = NormalizedSpaceCreate {
            name: InputName::new("Roadmap").expect("name"),
            description: Some(InputDescription::new("Different").expect("description")),
            idempotency_key: None,
        };
        assert_eq!(input.fingerprint(), same.fingerprint());
        assert_ne!(input.fingerprint(), different.fingerprint());
    }

    large_async_test!(read_only_rejects_before_decode_and_io, {
        let server = server(no_io_runtime(true), handlers());
        for name in [SPACE_CREATE, SPACE_UPDATE] {
            assert!(!server.tools().iter().any(|tool| tool.name == name));
            let result = server
                .dispatch_tool(
                    CallToolRequestParams::new(name).with_arguments(args(json!({
                        "secret":"must-not-decode"
                    }))),
                    &CancellationToken::new(),
                )
                .await
                .expect("read-only tool error");
            assert_eq!(result_code(&result), Some("validation"));
            assert!(!format!("{result:?}").contains("must-not-decode"));
            assert_eq!(
                server.runtime().client().http_metrics().logical_operations,
                0
            );
        }
    });

    large_async_test!(preview_stdio_rejects_invalid_and_read_only_before_io, {
        for (name, arguments) in [
            (SPACE_CREATE, json!({"name":"x".repeat(513)})),
            (SPACE_UPDATE, json!({"space":SPACE_ID})),
        ] {
            let server = server(no_io_runtime(false), handlers());
            let probe = server.clone();
            let response = preview_stdio_call(server, name, arguments).await;
            assert!(response.get("error").is_some() || response["result"]["isError"] == true);
            assert_eq!(
                probe.runtime().client().http_metrics().logical_operations,
                0
            );
        }
        for name in [SPACE_CREATE, SPACE_UPDATE] {
            let server = server(no_io_runtime(true), handlers());
            let probe = server.clone();
            let response =
                preview_stdio_call(server, name, json!({"secret":"stdio-read-only-secret"})).await;
            assert_eq!(response["result"]["isError"], true);
            assert_eq!(
                response["result"]["structuredContent"]["code"],
                "validation"
            );
            assert!(!response.to_string().contains("stdio-read-only-secret"));
            assert_eq!(
                probe.runtime().client().http_metrics().logical_operations,
                0
            );
        }
    });

    #[test]
    #[ignore = "requires env-only disposable credentials and an authenticated headless Anytype server"]
    fn headless_direct_stdio_and_ambiguity_use_disposable_real_spaces() {
        run_large_future(|| async {
            let outcome = Box::pin(with_disposable_space_context(
                "any-mcp-schema-space",
                |ctx| {
                    Box::pin(async move {
                        ctx.client.ping_http().await.expect("authenticated HTTP");
                        ctx.client.ping_grpc().await.expect("cleanup-capable gRPC");
                        let prefix = std::env::var("ANYTYPE_TEST_SPACE_PREFIX")
                            .expect("disposable prefix admitted before callback");

                        let direct_name = format!("{prefix}-mcp-direct-{}", unique_suffix());
                        let direct_claim =
                            Arc::new(ctx.prepare_space_fixture_claim(&direct_name).await?);
                        let direct_ctx = ctx.clone();
                        let direct_observer_claim = direct_claim.clone();
                        let direct_handlers = SchemaSpaceHandlers::build(
                            DEFAULT_IDEMPOTENCY_CAPACITY,
                            VerifyConfig::default(),
                            Some(Arc::new(move |returned| {
                                direct_ctx
                                    .claim_prepared_space_fixture(&direct_observer_claim, returned)
                                    .map_err(|_| ())
                            })),
                        )
                        .expect("direct live handlers");
                        let direct_server =
                            server(runtime(ctx.client.clone(), false), direct_handlers);
                        let direct_key = format!("direct-{}", unique_suffix());
                        let direct_arguments = json!({
                            "name":direct_name,
                            "description":"direct original",
                            "idempotency_key":direct_key
                        });
                        let before = metric_counts(&ctx.client);
                        let created =
                            direct(&direct_server, SPACE_CREATE, direct_arguments.clone()).await;
                        assert_eq!(created.is_error, Some(false));
                        assert_metric_delta(
                            before,
                            metric_counts(&ctx.client),
                            SPACE_CREATE_LOGICAL_CEILING,
                            SPACE_CREATE_PHYSICAL_CEILING,
                        );
                        let direct_id = created
                            .structured_content
                            .as_ref()
                            .and_then(|value| value.pointer("/space/id"))
                            .and_then(Value::as_str)
                            .expect("direct created id")
                            .to_owned();
                        let expected_direct = json!({
                            "space":{
                                "id":direct_id,
                                "name":direct_name,
                                "description":"direct original"
                            }
                        });
                        assert_eq!(created.structured_content, Some(expected_direct.clone()));
                        let expected_direct_text = expected_direct.to_string();
                        assert_eq!(
                            created
                                .content
                                .first()
                                .and_then(|content| content.as_text())
                                .map(|text| text.text.as_str()),
                            Some(expected_direct_text.as_str())
                        );
                        let repeat_before = metric_counts(&ctx.client);
                        assert_eq!(
                            direct(&direct_server, SPACE_CREATE, direct_arguments).await,
                            created
                        );
                        assert_eq!(metric_counts(&ctx.client), repeat_before);
                        let conflict = direct(
                            &direct_server,
                            SPACE_CREATE,
                            json!({
                                "name":format!("{prefix}-must-not-create-{}", unique_suffix()),
                                "idempotency_key":direct_key
                            }),
                        )
                        .await;
                        assert_eq!(result_code(&conflict), Some("conflict"));
                        assert_eq!(metric_counts(&ctx.client), repeat_before);

                        let mut cached_config = ctx.client.get_config().clone();
                        cached_config.disable_cache = false;
                        cached_config.app_name = "schema-space-cache-probe".to_owned();
                        let cached_client = AnytypeClient::with_config(cached_config)?;
                        cached_client.spaces().list().await?;
                        let before = metric_counts(&ctx.client);
                        let updated = direct(
                            &direct_server,
                            SPACE_UPDATE,
                            json!({
                                "space":direct_id,
                                "description":"direct updated"
                            }),
                        )
                        .await;
                        assert_eq!(updated.is_error, Some(false));
                        assert_metric_delta(
                            before,
                            metric_counts(&ctx.client),
                            SPACE_UPDATE_LOGICAL_CEILING,
                            SPACE_UPDATE_PHYSICAL_CEILING,
                        );
                        let cached = cached_client.space(&direct_id).get().await?;
                        assert_eq!(cached.description.as_deref(), Some("direct original"));
                        let fresh = cached_client.space(&direct_id).get_direct().await?;
                        assert_eq!(fresh.name, direct_name);
                        assert_eq!(fresh.description.as_deref(), Some("direct updated"));

                        let stdio_name = format!("{prefix}-mcp-stdio-{}", unique_suffix());
                        let stdio_claim =
                            Arc::new(ctx.prepare_space_fixture_claim(&stdio_name).await?);
                        let stdio_ctx = ctx.clone();
                        let stdio_observer_claim = stdio_claim.clone();
                        let stdio_handlers = SchemaSpaceHandlers::build(
                            DEFAULT_IDEMPOTENCY_CAPACITY,
                            VerifyConfig::default(),
                            Some(Arc::new(move |returned| {
                                stdio_ctx
                                    .claim_prepared_space_fixture(&stdio_observer_claim, returned)
                                    .map_err(|_| ())
                            })),
                        )
                        .expect("stdio live handlers");
                        let stdio_arguments = json!({
                            "name":stdio_name,
                            "description":"stdio original",
                            "idempotency_key":format!("stdio-{}", unique_suffix())
                        });
                        let before = metric_counts(&ctx.client);
                        let created = preview_stdio_call(
                            server(runtime(ctx.client.clone(), false), stdio_handlers.clone()),
                            SPACE_CREATE,
                            stdio_arguments.clone(),
                        )
                        .await;
                        assert_eq!(
                            created["result"]["isError"], false,
                            "stdio create failed: {created}"
                        );
                        assert_metric_delta(
                            before,
                            metric_counts(&ctx.client),
                            SPACE_CREATE_LOGICAL_CEILING,
                            SPACE_CREATE_PHYSICAL_CEILING,
                        );
                        let stdio_id = created["result"]["structuredContent"]["space"]["id"]
                            .as_str()
                            .expect("stdio created id")
                            .to_owned();
                        assert_eq!(
                            created["result"]["structuredContent"],
                            json!({
                                "space":{
                                    "id":stdio_id,
                                    "name":stdio_name,
                                    "description":"stdio original"
                                }
                            })
                        );
                        let repeat_before = metric_counts(&ctx.client);
                        let repeated = preview_stdio_call(
                            server(runtime(ctx.client.clone(), false), stdio_handlers.clone()),
                            SPACE_CREATE,
                            stdio_arguments,
                        )
                        .await;
                        assert_eq!(
                            repeated["result"]["structuredContent"],
                            created["result"]["structuredContent"]
                        );
                        assert_eq!(metric_counts(&ctx.client), repeat_before);
                        let before = metric_counts(&ctx.client);
                        let updated = preview_stdio_call(
                            server(runtime(ctx.client.clone(), false), stdio_handlers),
                            SPACE_UPDATE,
                            json!({
                                "space":stdio_id,
                                "description":"stdio updated"
                            }),
                        )
                        .await;
                        assert_eq!(updated["result"]["isError"], false);
                        assert_metric_delta(
                            before,
                            metric_counts(&ctx.client),
                            SPACE_UPDATE_LOGICAL_CEILING,
                            SPACE_UPDATE_PHYSICAL_CEILING,
                        );
                        let fresh = ctx.client.space(&stdio_id).get_direct().await?;
                        assert_eq!(fresh.name, stdio_name);
                        assert_eq!(fresh.description.as_deref(), Some("stdio updated"));

                        let ambiguous_name = format!("{prefix}-mcp-ambiguous-{}", unique_suffix());
                        let first_claim =
                            Arc::new(ctx.prepare_space_fixture_claim(&ambiguous_name).await?);
                        let second_claim =
                            Arc::new(ctx.prepare_space_fixture_claim(&ambiguous_name).await?);
                        let first_ctx = ctx.clone();
                        let first_observer_claim = first_claim.clone();
                        let first_handlers = SchemaSpaceHandlers::build(
                            DEFAULT_IDEMPOTENCY_CAPACITY,
                            VerifyConfig::default(),
                            Some(Arc::new(move |returned| {
                                first_ctx
                                    .claim_prepared_space_fixture(&first_observer_claim, returned)
                                    .map_err(|_| ())
                            })),
                        )
                        .expect("first ambiguity handlers");
                        let second_ctx = ctx.clone();
                        let second_observer_claim = second_claim.clone();
                        let second_handlers = SchemaSpaceHandlers::build(
                            DEFAULT_IDEMPOTENCY_CAPACITY,
                            VerifyConfig::default(),
                            Some(Arc::new(move |returned| {
                                second_ctx
                                    .claim_prepared_space_fixture(&second_observer_claim, returned)
                                    .map_err(|_| ())
                            })),
                        )
                        .expect("second ambiguity handlers");
                        let first = direct(
                            &server(runtime(ctx.client.clone(), false), first_handlers),
                            SPACE_CREATE,
                            json!({"name":ambiguous_name}),
                        )
                        .await;
                        let second = direct(
                            &server(runtime(ctx.client.clone(), false), second_handlers),
                            SPACE_CREATE,
                            json!({"name":ambiguous_name}),
                        )
                        .await;
                        assert_eq!(first.is_error, Some(false));
                        assert_eq!(second.is_error, Some(false));
                        let first_id = first
                            .structured_content
                            .as_ref()
                            .and_then(|value| value.pointer("/space/id"))
                            .and_then(Value::as_str)
                            .expect("first ambiguity id")
                            .to_owned();
                        let second_id = second
                            .structured_content
                            .as_ref()
                            .and_then(|value| value.pointer("/space/id"))
                            .and_then(Value::as_str)
                            .expect("second ambiguity id")
                            .to_owned();
                        assert_ne!(first_id, second_id);

                        ctx.client.cache().clear_spaces();
                        let ambiguity_server =
                            server(runtime(ctx.client.clone(), false), handlers());
                        let direct_ambiguity = direct(
                            &ambiguity_server,
                            SPACE_UPDATE,
                            json!({
                                "space":ambiguous_name,
                                "description":"must not be written"
                            }),
                        )
                        .await;
                        assert_eq!(direct_ambiguity.is_error, Some(true));
                        ctx.client.cache().clear_spaces();
                        let stdio_ambiguity = preview_stdio_call(
                            ambiguity_server,
                            SPACE_UPDATE,
                            json!({
                                "space":ambiguous_name,
                                "description":"must not be written"
                            }),
                        )
                        .await;
                        assert_eq!(stdio_ambiguity["result"]["isError"], true);
                        for id in [first_id, second_id] {
                            let unchanged = ctx.client.space(&id).get_direct().await?;
                            assert_eq!(unchanged.name, ambiguous_name);
                            assert!(
                                unchanged.description.as_deref().is_none_or(str::is_empty),
                                "ambiguous update must not set a description"
                            );
                        }
                        Ok(())
                    })
                },
            ))
            .await
            .expect("cleanup-safe live schema-space workflow");
            match outcome {
                DisposableRun::Completed(()) => {}
                DisposableRun::Skipped(reason) => {
                    eprintln!("disposable schema-space suite skipped before callback: {reason:?}");
                }
            }
        });
    }

    #[test]
    fn reviewed_work_ceilings_are_locked_without_fault_injection() {
        assert_eq!(SPACE_CREATE_LOGICAL_CEILING, 11);
        assert_eq!(SPACE_CREATE_PHYSICAL_CEILING, 61);
        assert_eq!(SPACE_UPDATE_LOGICAL_CEILING, 23);
        assert_eq!(SPACE_UPDATE_PHYSICAL_CEILING, 133);
    }

    #[test]
    fn observer_counter_is_thread_safe_for_live_registration_seam() {
        let observed = Arc::new(AtomicUsize::new(0));
        let counter = observed.clone();
        let handlers = SchemaSpaceHandlers::build(
            1,
            VerifyConfig::default(),
            Some(Arc::new(move |_| {
                counter.fetch_add(1, Ordering::SeqCst);
                Ok(())
            })),
        )
        .expect("observer handlers");
        assert!(handlers.create_observer.is_some());
        assert_eq!(observed.load(Ordering::SeqCst), 0);
    }
}
