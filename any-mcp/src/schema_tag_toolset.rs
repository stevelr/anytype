// any-mcp - bounded, workflow-oriented MCP server for Anytype
//
// SPDX-FileCopyrightText: 2026 Steve Schoettler
// SPDX-License-Identifier: Apache-2.0

//! Optional schema-toolset workflows for bounded tag creation and updates.
//!
//! The production `schema` descriptor composes this reviewed slice with the
//! space, type, and property slices.

use std::{
    borrow::Cow,
    fmt,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use anytype::{
    error::AnytypeError,
    objects::Color,
    paged::PaginatedResponse,
    prelude::{AnytypeClient, VerifyConfig, verify_semantic},
    properties::{Property, PropertyFormat as ApiPropertyFormat},
    tags::Tag,
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
    domain::{DisplayName, DomainValueError, EntityId, TypeKey},
    error::ToolError,
    handler_support::{
        HandlerError, HandlerOperationError, MutationAccess, MutationProgress, MutationStage,
        execute_mutation_handler, require_mutation_access,
    },
    object_output::ProjectedColor,
    optional_toolsets::{OptionalRegistryFuture, OptionalRegistryTool},
    protocol::{ToolProfile, WorkflowTool, workflow_tool},
    result::tool_error,
    runtime::{OperationContext, RuntimeContext},
    schema::SchemaContractError,
    schema_space_toolset::InputName,
    schema_type_toolset::SchemaKey,
    server::decode_arguments,
    validation::{Omittable, optional_non_null_schema},
};

/// Exact tool name for bounded tag creation.
pub const TAG_CREATE: &str = "tag_create";
/// Exact tool name for bounded tag updates.
pub const TAG_UPDATE: &str = "tag_update";
/// Reviewed maximum logical HTTP operations for one tag create.
pub const TAG_CREATE_LOGICAL_CEILING: usize = 34;
/// Reviewed maximum physical HTTP attempts for one tag create.
pub const TAG_CREATE_PHYSICAL_CEILING: usize = 199;
/// Reviewed maximum logical HTTP operations for one tag update.
pub const TAG_UPDATE_LOGICAL_CEILING: usize = 35;
/// Reviewed maximum physical HTTP attempts for one tag update.
pub const TAG_UPDATE_PHYSICAL_CEILING: usize = 205;

const TAG_CREATE_FINGERPRINT_DOMAIN: &str = "any-mcp/schema-tag-create/v1";
const MAX_PROPERTY_REFERENCE_CHARS: usize = 256;
#[cfg(test)]
const TAG_RESULT_MAX_BYTES: usize = 5_320;
#[cfg(test)]
const TAG_RESULT_MAX_TOKENS: usize = 3_381;

type DispatchHook = Arc<dyn Fn(&CancellationToken) + Send + Sync>;

#[derive(Clone, Default)]
struct DispatchHooks {
    before_create: Option<DispatchHook>,
    after_create_mark: Option<DispatchHook>,
    before_update: Option<DispatchHook>,
    after_update_mark: Option<DispatchHook>,
}

/// A bounded property key or stable identifier.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
struct PropertyReference(String);

impl PropertyReference {
    fn new(value: impl Into<String>) -> Result<Self, PropertyReferenceError> {
        let value = value.into();
        if value.trim().is_empty() || value.chars().count() > MAX_PROPERTY_REFERENCE_CHARS {
            return Err(PropertyReferenceError);
        }
        Ok(Self(value))
    }

    fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for PropertyReference {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::new(String::deserialize(deserializer)?).map_err(de::Error::custom)
    }
}

impl JsonSchema for PropertyReference {
    fn schema_name() -> Cow<'static, str> {
        "PropertyRef".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "minLength": 1,
            "maxLength": MAX_PROPERTY_REFERENCE_CHARS,
        })
    }
}

#[derive(Debug, Clone, Copy)]
struct PropertyReferenceError;

impl fmt::Display for PropertyReferenceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("property reference must contain 1..=256 Unicode scalars")
    }
}

/// Strict input for creating one option on a select or multi-select property.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TagCreateInput {
    /// Unique space name or identifier, preserving exact caller spelling.
    space: DiscoveryReference,
    /// Unique property key or identifier within the resolved space.
    property: PropertyReference,
    /// Exact nonempty tag display name.
    name: InputName,
    /// Optional lower-snake-case tag key; explicit null is rejected.
    #[serde(default)]
    #[schemars(schema_with = "optional_schema_key_schema")]
    key: Omittable<SchemaKey>,
    /// Optional closed color; omission selects `grey` and null is rejected.
    #[serde(default)]
    #[schemars(schema_with = "optional_color_schema")]
    color: Omittable<ProjectedColor>,
    /// Optional process-local retry key; explicit null is rejected.
    #[serde(default)]
    #[schemars(schema_with = "optional_idempotency_schema")]
    idempotency_key: Omittable<IdempotencyKey>,
}

fn optional_schema_key_schema(generator: &mut SchemaGenerator) -> Schema {
    optional_non_null_schema::<SchemaKey>(generator)
}

fn optional_color_schema(generator: &mut SchemaGenerator) -> Schema {
    optional_non_null_schema::<ProjectedColor>(generator)
}

fn optional_idempotency_schema(generator: &mut SchemaGenerator) -> Schema {
    optional_non_null_schema::<IdempotencyKey>(generator)
}

/// Strict input for updating at least one field on one exact scoped tag.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TagUpdateInput {
    /// Unique space name or identifier, preserving exact caller spelling.
    space: DiscoveryReference,
    /// Unique property key or identifier within the resolved space.
    property: PropertyReference,
    /// Exact tag identifier within the resolved property.
    tag_id: EntityId,
    /// Optional nonempty replacement display name; explicit null is rejected.
    #[serde(default)]
    name: Omittable<InputName>,
    /// Optional lower-snake-case replacement key; explicit null is rejected.
    #[serde(default)]
    key: Omittable<SchemaKey>,
    /// Optional closed replacement color; explicit null is rejected.
    #[serde(default)]
    color: Omittable<ProjectedColor>,
}

#[allow(dead_code)]
#[derive(JsonSchema)]
#[serde(deny_unknown_fields)]
struct TagUpdateInputSchema {
    /// Unique space name or identifier, preserving exact caller spelling.
    space: DiscoveryReference,
    /// Unique property key or identifier within the resolved space.
    property: PropertyReference,
    /// Exact tag identifier within the resolved property.
    tag_id: EntityId,
    /// Optional nonempty replacement display name; explicit null is rejected.
    #[serde(default)]
    #[schemars(schema_with = "optional_input_name_schema")]
    name: Omittable<InputName>,
    /// Optional lower-snake-case replacement key; explicit null is rejected.
    #[serde(default)]
    #[schemars(schema_with = "optional_schema_key_schema")]
    key: Omittable<SchemaKey>,
    /// Optional closed replacement color; explicit null is rejected.
    #[serde(default)]
    #[schemars(schema_with = "optional_color_schema")]
    color: Omittable<ProjectedColor>,
}

impl JsonSchema for TagUpdateInput {
    fn schema_name() -> Cow<'static, str> {
        "TagUpdateInput".into()
    }

    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        let mut schema = TagUpdateInputSchema::json_schema(generator);
        schema
            .ensure_object()
            .insert("minProperties".to_owned(), 4_u64.into());
        schema
    }
}

fn optional_input_name_schema(generator: &mut SchemaGenerator) -> Schema {
    optional_non_null_schema::<InputName>(generator)
}

impl TagUpdateInput {
    fn has_mutation(&self) -> bool {
        !self.name.is_none() || !self.key.is_none() || !self.color.is_none()
    }
}

/// Exact bounded output shared by tag create and update.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TagSummary {
    /// Stable tag identifier.
    id: EntityId,
    /// Stable nonempty tag key.
    key: TypeKey,
    /// Bounded tag display name.
    name: DisplayName,
    /// Closed Anytype tag color.
    color: ProjectedColor,
}

/// Exact result envelope shared by tag create and update.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TagOutput {
    /// Verified bounded tag metadata.
    tag: TagSummary,
}

impl From<TagSummary> for TagOutput {
    fn from(tag: TagSummary) -> Self {
        Self { tag }
    }
}

/// Constructs the approved `tag_create` contract.
pub fn tag_create_tool() -> Result<WorkflowTool<TagOutput>, SchemaContractError> {
    workflow_tool::<TagCreateInput, TagOutput>(
        TAG_CREATE,
        "Create one tag on an exact select or multi-select property, default color to grey, verify the scoped identity and requested fields, and return only bounded tag metadata. A retry key deduplicates identical verified creates for this server process.",
        ToolProfile::Create,
    )
}

/// Constructs the approved `tag_update` contract.
pub fn tag_update_tool() -> Result<WorkflowTool<TagOutput>, SchemaContractError> {
    workflow_tool::<TagUpdateInput, TagOutput>(
        TAG_UPDATE,
        "Update at least one supplied field on one exact tag within an exact select or multi-select property and verify every supplied value. Omitted fields are preserved.",
        ToolProfile::Update,
    )
}

/// Returns the complete schema-tag slice for registry composition.
pub fn schema_tag_tools() -> Result<Vec<OptionalRegistryTool>, SchemaContractError> {
    Ok(vec![
        OptionalRegistryTool::mutation_http(tag_create_tool()?),
        OptionalRegistryTool::mutation_http(tag_update_tool()?),
    ])
}

/// Stateful transport-neutral handlers for the schema tag slice.
#[derive(Clone)]
pub struct SchemaTagHandlers {
    idempotency: Arc<IdempotencyStore>,
    verify_config: VerifyConfig,
    create_contract: WorkflowTool<TagOutput>,
    update_contract: WorkflowTool<TagOutput>,
    dispatch_hooks: DispatchHooks,
}

impl fmt::Debug for SchemaTagHandlers {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SchemaTagHandlers")
            .field("verify_config", &self.verify_config)
            .field("has_dispatch_hooks", &self.dispatch_hooks_present())
            .finish_non_exhaustive()
    }
}

impl SchemaTagHandlers {
    /// Creates handlers with the reviewed finite idempotency and verification ceilings.
    pub fn new() -> Result<Self, SchemaContractError> {
        Self::build(DEFAULT_IDEMPOTENCY_CAPACITY, VerifyConfig::default())
    }

    fn build(capacity: usize, verify_config: VerifyConfig) -> Result<Self, SchemaContractError> {
        Ok(Self {
            idempotency: Arc::new(IdempotencyStore::new(capacity)),
            verify_config,
            create_contract: tag_create_tool()?,
            update_contract: tag_update_tool()?,
            dispatch_hooks: DispatchHooks::default(),
        })
    }

    fn dispatch_hooks_present(&self) -> bool {
        self.dispatch_hooks.before_create.is_some()
            || self.dispatch_hooks.after_create_mark.is_some()
            || self.dispatch_hooks.before_update.is_some()
            || self.dispatch_hooks.after_update_mark.is_some()
    }

    #[cfg(test)]
    fn with_dispatch_hooks(mut self, hooks: DispatchHooks) -> Self {
        self.dispatch_hooks = hooks;
        self
    }

    /// Dispatches one schema-tag tool after the caller's catalog gate.
    pub fn call_tool<'a>(
        &'a self,
        request: CallToolRequestParams,
        runtime: &'a RuntimeContext,
        cancellation: &'a CancellationToken,
    ) -> OptionalRegistryFuture<'a, Result<CallToolResult, ErrorData>> {
        Box::pin(async move {
            if runtime.is_read_only() && matches!(request.name.as_ref(), TAG_CREATE | TAG_UPDATE) {
                return Ok(tool_error(&ToolError::validation()));
            }
            let access = MutationAccess::Allowed;
            match request.name.as_ref() {
                TAG_CREATE => {
                    let input = decode_arguments::<TagCreateInput>(request.arguments)?;
                    Ok(Box::pin(self.tag_create(runtime, access, input, cancellation)).await)
                }
                TAG_UPDATE => {
                    let input = decode_arguments::<TagUpdateInput>(request.arguments)?;
                    Ok(Box::pin(self.tag_update(runtime, access, input, cancellation)).await)
                }
                _ => Err(ErrorData::method_not_found::<CallToolRequestMethod>()),
            }
        })
    }

    fn tag_create<'a>(
        &'a self,
        runtime: &'a RuntimeContext,
        access: MutationAccess,
        input: TagCreateInput,
        cancellation: &'a CancellationToken,
    ) -> OptionalRegistryFuture<'a, CallToolResult> {
        Box::pin(async move {
            if let Err(error) = require_mutation_access(access) {
                return tool_error(error.tool_error());
            }
            let normalized = NormalizedTagCreate::from(input);
            let Some(key) = normalized.idempotency_key.clone() else {
                let progress = MutationProgress::new();
                return Box::pin(execute_tag_create(
                    runtime,
                    &self.create_contract,
                    normalized,
                    cancellation,
                    &progress,
                    &self.verify_config,
                    &self.dispatch_hooks,
                ))
                .await
                .result;
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
                    let supervision = TagCreateSupervision {
                        runtime: runtime.clone(),
                        contract: self.create_contract.clone(),
                        store: self.idempotency.clone(),
                        key,
                        attempt: attempt.clone(),
                        normalized,
                        verify_config: self.verify_config.clone(),
                        dispatch_hooks: self.dispatch_hooks.clone(),
                    };
                    runtime.spawn_invocation_controller("schema_tag_create", move || {
                        supervise_tag_create(supervision)
                    });
                    wait_for_attempt(attempt, cancellation).await
                }
            }
        })
    }

    async fn tag_update(
        &self,
        runtime: &RuntimeContext,
        access: MutationAccess,
        input: TagUpdateInput,
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
        let dispatch_hooks = self.dispatch_hooks.clone();
        execute_mutation_handler(
            runtime,
            &self.update_contract,
            OperationContext::new(TAG_UPDATE),
            cancellation,
            &progress,
            Box::pin(async move {
                let scope = resolve_property_scope(&client, &input.space, &input.property).await?;
                ensure_select_property(&scope.property)?;
                let current = scoped_tag_preflight(&client, &scope, &input.tag_id).await?;
                checked_tag_summary(&current, Some(&input.tag_id))?;

                let mut request = client
                    .update_tag(
                        scope.space_id.as_str(),
                        scope.property_id.as_str(),
                        input.tag_id.as_str(),
                    )
                    .no_verify()
                    .no_cache_refresh();
                if let Some(name) = input.name.as_ref() {
                    request = request.name(name.as_str());
                }
                if let Some(key) = input.key.as_ref() {
                    request = request.key(key.as_str());
                }
                if let Some(color) = input.color.as_ref() {
                    request = request.color(api_color(*color));
                }

                run_dispatch_hook(dispatch_hooks.before_update.as_ref(), cancellation).await;
                operation_progress.mark_dispatched(runtime)?;
                run_dispatch_hook(dispatch_hooks.after_update_mark.as_ref(), cancellation).await;
                let response_anomaly = match request.update().await {
                    Ok(returned) => !tag_matches_update(&returned, &input).unwrap_or(false),
                    Err(error) if schema_mutation_rejection_is_definitive(&error) => {
                        return Err(error.into());
                    }
                    Err(_) => true,
                };

                let verified = verify_semantic(
                    &verify_config,
                    "tag",
                    input.tag_id.as_str(),
                    || {
                        scoped_tag_verification_read(
                            &client,
                            scope.space_id.as_str(),
                            scope.property_id.as_str(),
                            &input.tag_id,
                        )
                    },
                    |tag| tag_matches_update(tag, &input).unwrap_or(false),
                )
                .await
                .map_err(|_| indeterminate_operation())?;
                if response_anomaly {
                    return Err(indeterminate_operation());
                }
                checked_tag_summary(&verified, Some(&input.tag_id))
                    .map(TagOutput::from)
                    .map_err(|_| indeterminate_operation())
            }),
            |output| Box::pin(async move { Ok(output) }),
        )
        .await
    }
}

#[derive(Clone)]
struct NormalizedTagCreate {
    space: DiscoveryReference,
    property: PropertyReference,
    name: InputName,
    key: Option<SchemaKey>,
    color: ProjectedColor,
    idempotency_key: Option<IdempotencyKey>,
}

impl From<TagCreateInput> for NormalizedTagCreate {
    fn from(input: TagCreateInput) -> Self {
        Self {
            space: input.space,
            property: input.property,
            name: input.name,
            key: input.key.as_ref().cloned(),
            color: input
                .color
                .as_ref()
                .copied()
                .unwrap_or(ProjectedColor::Grey),
            idempotency_key: input.idempotency_key.as_ref().cloned(),
        }
    }
}

impl NormalizedTagCreate {
    fn fingerprint(&self) -> [u8; 32] {
        let mut hasher = Sha256::new();
        hasher.update(TAG_CREATE_FINGERPRINT_DOMAIN.as_bytes());
        hash_field(&mut hasher, self.space.as_str());
        hash_field(&mut hasher, self.property.as_str());
        hash_field(&mut hasher, self.name.as_str());
        match self.key.as_ref() {
            Some(key) => {
                hasher.update([1]);
                hash_field(&mut hasher, key.as_str());
            }
            None => hasher.update([0]),
        }
        hash_field(&mut hasher, color_name(self.color));
        hasher.finalize().into()
    }
}

fn hash_field(hasher: &mut Sha256, value: &str) {
    hasher.update(value.len().to_be_bytes());
    hasher.update(value.as_bytes());
}

struct TagCreateSupervision {
    runtime: RuntimeContext,
    contract: WorkflowTool<TagOutput>,
    store: Arc<IdempotencyStore>,
    key: IdempotencyKey,
    attempt: Arc<Attempt>,
    normalized: NormalizedTagCreate,
    verify_config: VerifyConfig,
    dispatch_hooks: DispatchHooks,
}

async fn supervise_tag_create(supervision: TagCreateSupervision) {
    let TagCreateSupervision {
        runtime,
        contract,
        store,
        key,
        attempt,
        normalized,
        verify_config,
        dispatch_hooks,
    } = supervision;
    let progress = attempt.progress();
    let task_progress = progress.clone();
    let task_runtime = runtime.clone();
    let task = runtime.spawn_invocation_supervisor(async move {
        execute_tag_create(
            &task_runtime,
            &contract,
            normalized,
            &CancellationToken::new(),
            &task_progress,
            &verify_config,
            &dispatch_hooks,
        )
        .await
    });
    let execution = finish_supervised_execution(task, &progress).await;
    store.finish(&key, &attempt, execution).await;
}

async fn execute_tag_create(
    runtime: &RuntimeContext,
    contract: &WorkflowTool<TagOutput>,
    input: NormalizedTagCreate,
    cancellation: &CancellationToken,
    progress: &MutationProgress,
    verify_config: &VerifyConfig,
    dispatch_hooks: &DispatchHooks,
) -> CreateExecution {
    let client = runtime.client().clone();
    let definitive_rejection = Arc::new(AtomicBool::new(false));
    let operation_rejection = definitive_rejection.clone();
    let operation_progress = progress.clone();
    let verify_config = verify_config.clone();
    let result = execute_mutation_handler(
        runtime,
        contract,
        OperationContext::new(TAG_CREATE),
        cancellation,
        progress,
        Box::pin(async move {
            let scope = resolve_property_scope(&client, &input.space, &input.property).await?;
            ensure_select_property(&scope.property)?;
            let mut request = client
                .new_tag(scope.space_id.as_str(), scope.property_id.as_str())
                .name(input.name.as_str())
                .color(api_color(input.color))
                .no_verify()
                .no_cache_refresh();
            if let Some(key) = input.key.as_ref() {
                request = request.key(key.as_str());
            }

            run_dispatch_hook(dispatch_hooks.before_create.as_ref(), cancellation).await;
            operation_progress.mark_dispatched(runtime)?;
            run_dispatch_hook(dispatch_hooks.after_create_mark.as_ref(), cancellation).await;
            let created = match request.create().await {
                Ok(created) => created,
                Err(error) => {
                    if schema_mutation_rejection_is_definitive(&error) {
                        operation_rejection.store(true, Ordering::Release);
                        return Err(error.into());
                    }
                    return Err(indeterminate_operation());
                }
            };
            let id = EntityId::new(created.id.clone()).map_err(|_| indeterminate_operation())?;
            let response_matches =
                tag_matches_create(&created, &id, &input).map_err(|_| indeterminate_operation())?;
            let verified = verify_semantic(
                &verify_config,
                "tag",
                id.as_str(),
                || {
                    scoped_tag_verification_read(
                        &client,
                        scope.space_id.as_str(),
                        scope.property_id.as_str(),
                        &id,
                    )
                },
                |tag| tag_matches_create(tag, &id, &input).unwrap_or(false),
            )
            .await
            .map_err(|_| indeterminate_operation())?;
            if !response_matches {
                return Err(indeterminate_operation());
            }
            checked_tag_summary(&verified, Some(&id))
                .map(TagOutput::from)
                .map_err(|_| indeterminate_operation())
        }),
        |output| Box::pin(async move { Ok(output) }),
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

async fn run_dispatch_hook(hook: Option<&DispatchHook>, cancellation: &CancellationToken) {
    if let Some(hook) = hook {
        hook(cancellation);
        tokio::task::yield_now().await;
    }
}

struct PropertyScope {
    space_id: EntityId,
    property_id: EntityId,
    property: Property,
}

async fn resolve_property_scope(
    client: &AnytypeClient,
    space: &DiscoveryReference,
    property: &PropertyReference,
) -> Result<PropertyScope, HandlerOperationError> {
    let resolved_space = client.resolve_space_id(space.as_str()).await?;
    let space_id = EntityId::new(resolved_space).map_err(unsafe_upstream)?;
    let resolved_property = client
        .resolve_property_id(space_id.as_str(), property.as_str())
        .await?;
    let property_id = EntityId::new(resolved_property).map_err(unsafe_upstream)?;
    // The exact-property endpoint accepts globally valid IDs even when the
    // path names a different space. A terminal space-owned page is therefore
    // the bounded evidence that the resolved property belongs to this scope.
    let page = client
        .properties(space_id.as_str())
        .limit(1_000)
        .list()
        .await?
        .into_response();
    if page.pagination.offset != 0
        || page.pagination.has_more
        || page.pagination.total != page.items.len()
    {
        return Err(HandlerError::new(ToolError::bounded_result()).into());
    }
    let mut matches = page
        .items
        .into_iter()
        .filter(|candidate| candidate.id == property_id.as_str());
    let Some(property) = matches.next() else {
        return Err(HandlerError::new(ToolError::not_found()).into());
    };
    if matches.next().is_some() {
        return Err(HandlerError::new(ToolError::upstream()).into());
    }
    Ok(PropertyScope {
        space_id,
        property_id,
        property,
    })
}

fn ensure_select_property(property: &Property) -> Result<(), HandlerOperationError> {
    if matches!(
        property.format(),
        ApiPropertyFormat::Select | ApiPropertyFormat::MultiSelect
    ) {
        Ok(())
    } else {
        Err(HandlerError::new(ToolError::validation()).into())
    }
}

fn schema_mutation_rejection_is_definitive(error: &AnytypeError) -> bool {
    match error {
        AnytypeError::Auth { .. }
        | AnytypeError::Unauthorized
        | AnytypeError::Forbidden
        | AnytypeError::NotFound { .. }
        | AnytypeError::Validation { .. } => true,
        AnytypeError::ApiError { code, .. } => {
            matches!(code, 400 | 401 | 403 | 404 | 409 | 422)
        }
        _ => false,
    }
}

async fn scoped_tag_preflight(
    client: &AnytypeClient,
    scope: &PropertyScope,
    tag_id: &EntityId,
) -> Result<Tag, HandlerOperationError> {
    // The exact-tag endpoint likewise accepts a globally valid tag through a
    // different property's path. Use one terminal property-owned page so a
    // cross-property ID can never reach PATCH.
    let page = client
        .tags(scope.space_id.as_str(), scope.property_id.as_str())
        .limit(1_000)
        .list()
        .await?
        .into_response();
    exact_owned_tag(page, tag_id).map_err(|error| {
        HandlerOperationError::from(HandlerError::new(match error {
            ScopedTagEvidenceError::Incomplete => ToolError::bounded_result(),
            ScopedTagEvidenceError::Missing => ToolError::not_found(),
            ScopedTagEvidenceError::Duplicate => ToolError::upstream(),
        }))
    })
}

async fn scoped_tag_verification_read(
    client: &AnytypeClient,
    space_id: &str,
    property_id: &str,
    tag_id: &EntityId,
) -> anytype::Result<Tag> {
    let page = client
        .tags(space_id, property_id)
        .limit(1_000)
        .list()
        .await?
        .into_response();
    exact_owned_tag(page, tag_id).map_err(|error| match error {
        ScopedTagEvidenceError::Missing => AnytypeError::NotFound {
            obj_type: "Tag".to_owned(),
            key: String::new(),
        },
        ScopedTagEvidenceError::Incomplete | ScopedTagEvidenceError::Duplicate => {
            AnytypeError::Other {
                message: "scoped tag evidence is incomplete".to_owned(),
            }
        }
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScopedTagEvidenceError {
    Incomplete,
    Missing,
    Duplicate,
}

fn exact_owned_tag(
    page: PaginatedResponse<Tag>,
    tag_id: &EntityId,
) -> Result<Tag, ScopedTagEvidenceError> {
    if page.pagination.offset != 0
        || page.pagination.has_more
        || page.pagination.total != page.items.len()
    {
        return Err(ScopedTagEvidenceError::Incomplete);
    }
    let mut matches = page
        .items
        .into_iter()
        .filter(|tag| tag.id == tag_id.as_str());
    let Some(tag) = matches.next() else {
        return Err(ScopedTagEvidenceError::Missing);
    };
    if matches.next().is_some() {
        return Err(ScopedTagEvidenceError::Duplicate);
    }
    Ok(tag)
}

fn checked_tag_summary(
    tag: &Tag,
    expected_id: Option<&EntityId>,
) -> Result<TagSummary, HandlerError> {
    let id = EntityId::new(tag.id.clone()).map_err(unsafe_domain)?;
    if expected_id.is_some_and(|expected| expected != &id) {
        return Err(HandlerError::new(ToolError::upstream()));
    }
    Ok(TagSummary {
        id,
        key: TypeKey::new(tag.key.clone()).map_err(unsafe_domain)?,
        name: DisplayName::new(tag.name.clone()).map_err(unsafe_domain)?,
        color: projected_color(&tag.color),
    })
}

fn tag_matches_create(
    tag: &Tag,
    expected_id: &EntityId,
    input: &NormalizedTagCreate,
) -> Result<bool, HandlerError> {
    let summary = checked_tag_summary(tag, Some(expected_id))?;
    Ok(summary.name.as_str() == input.name.as_str()
        && input
            .key
            .as_ref()
            .is_none_or(|key| summary.key.as_str() == key.as_str())
        && summary.color == input.color)
}

fn tag_matches_update(tag: &Tag, input: &TagUpdateInput) -> Result<bool, HandlerError> {
    let summary = checked_tag_summary(tag, Some(&input.tag_id))?;
    Ok(input
        .name
        .as_ref()
        .is_none_or(|name| summary.name.as_str() == name.as_str())
        && input
            .key
            .as_ref()
            .is_none_or(|key| summary.key.as_str() == key.as_str())
        && input
            .color
            .as_ref()
            .is_none_or(|color| summary.color == *color))
}

const fn api_color(color: ProjectedColor) -> Color {
    match color {
        ProjectedColor::Grey => Color::Grey,
        ProjectedColor::Yellow => Color::Yellow,
        ProjectedColor::Orange => Color::Orange,
        ProjectedColor::Red => Color::Red,
        ProjectedColor::Pink => Color::Pink,
        ProjectedColor::Purple => Color::Purple,
        ProjectedColor::Blue => Color::Blue,
        ProjectedColor::Ice => Color::Ice,
        ProjectedColor::Teal => Color::Teal,
        ProjectedColor::Lime => Color::Lime,
    }
}

const fn projected_color(color: &Color) -> ProjectedColor {
    match color {
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
    }
}

const fn color_name(color: ProjectedColor) -> &'static str {
    match color {
        ProjectedColor::Grey => "grey",
        ProjectedColor::Yellow => "yellow",
        ProjectedColor::Orange => "orange",
        ProjectedColor::Red => "red",
        ProjectedColor::Pink => "pink",
        ProjectedColor::Purple => "purple",
        ProjectedColor::Blue => "blue",
        ProjectedColor::Ice => "ice",
        ProjectedColor::Teal => "teal",
        ProjectedColor::Lime => "lime",
    }
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
    use std::{future::Future, time::Duration};

    use anytype::{
        objects::DataModel,
        prelude::{AnytypeClient, ClientConfig, HttpCredentials},
        properties::PropertyFormat,
        test_util::{DisposableRun, unique_suffix, with_disposable_space_context},
    };
    use rmcp::model::Tool;
    use serde_json::{Map, Value, json};
    use tiktoken_rs::o200k_base;
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
    const PROPERTY_ID: &str = "bafyreid5fvqlnsobih2keakcxjrrlpmly6kf37klzjzen4ibfdgalcdp4y";
    const OTHER_TAG_ID: &str = "bafyreid5fvqlnsobih2keakcxjrrlpmly6kf37klzjzen4ibfdgalcdp4z";

    fn run_large_future<F, Fut>(test: F)
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        std::thread::Builder::new()
            .name("schema-tag-handler".to_owned())
            .stack_size(8 * 1024 * 1024)
            .spawn(move || {
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("schema-tag test runtime")
                    .block_on(test());
            })
            .expect("spawn schema-tag test")
            .join()
            .expect("schema-tag test thread");
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
        handlers: SchemaTagHandlers,
    }

    impl fmt::Debug for TestRegistry {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("TestSchemaTagRegistry")
        }
    }

    impl OptionalToolsetRegistry for TestRegistry {
        fn metadata(&self) -> OptionalToolsetMetadata {
            OptionalToolsetMetadata::new("schema")
        }

        fn tools(&self) -> Result<Vec<OptionalRegistryTool>, SchemaContractError> {
            schema_tag_tools()
        }

        fn scripted_scenario_ids(&self) -> &'static [&'static str] {
            &["schema_tag_direct", "schema_tag_stdio"]
        }

        fn headless_scenario_ids(&self) -> &'static [&'static str] {
            &["schema_tag_headless"]
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

    fn handlers() -> SchemaTagHandlers {
        SchemaTagHandlers::new().expect("schema-tag handlers")
    }

    fn runtime(client: AnytypeClient, read_only: bool) -> RuntimeContext {
        let selection = OptionalToolsetSelection::parse(
            Some("schema".to_owned()),
            &[OptionalToolsetMetadata::new("schema")],
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
            keystore_service: Some("schema-tag-no-io".to_owned()),
            app_name: "schema-tag-no-io".to_owned(),
            ..ClientConfig::default()
        })
        .expect("schema-tag no-I/O client");
        client.set_api_key(HttpCredentials::new("unused-no-io-token"));
        runtime(client, read_only)
    }

    fn server(runtime: RuntimeContext, handlers: SchemaTagHandlers) -> AnyMcpServer {
        let registry: &'static TestRegistry = Box::leak(Box::new(TestRegistry { handlers }));
        let registries: &'static [&'static dyn OptionalToolsetRegistry] =
            Box::leak(vec![registry as &dyn OptionalToolsetRegistry].into_boxed_slice());
        AnyMcpServer::new_with_optional_registries(runtime, registries)
            .expect("schema-tag test server")
    }

    fn args(value: Value) -> Map<String, Value> {
        value.as_object().cloned().expect("object arguments")
    }

    async fn direct(server: &AnyMcpServer, name: &'static str, arguments: Value) -> CallToolResult {
        direct_with_cancellation(server, name, arguments, &CancellationToken::new()).await
    }

    async fn direct_with_cancellation(
        server: &AnyMcpServer,
        name: &'static str,
        arguments: Value,
        cancellation: &CancellationToken,
    ) -> CallToolResult {
        server
            .dispatch_tool(
                CallToolRequestParams::new(name).with_arguments(args(arguments)),
                cancellation,
            )
            .await
            .expect("schema-tag direct dispatch")
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
                    "io.modelcontextprotocol/clientInfo":{"name":"schema-tag-test","version":"1"},
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

    async fn preview_stdio_cancelled_call(
        server: AnyMcpServer,
        name: &'static str,
        arguments: Value,
    ) {
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
                    "io.modelcontextprotocol/clientInfo":{"name":"schema-tag-test","version":"1"},
                    "io.modelcontextprotocol/clientCapabilities":{}
                }
            }
        });
        client_writer
            .write_all(format!("{frame}\n").as_bytes())
            .await
            .expect("write cancelled stdio request");
        let mut client_reader = BufReader::new(client_reader);
        let mut line = String::new();
        assert!(
            tokio::time::timeout(
                Duration::from_millis(250),
                client_reader.read_line(&mut line)
            )
            .await
            .is_err(),
            "cancelled preview request must not emit a response frame"
        );
        drop(client_writer);
        drop(client_reader);
        task.await
            .expect("spawned cancelled stdio task")
            .expect("cancelled stdio transport");
    }

    fn result_code(result: &CallToolResult) -> Option<&str> {
        result
            .structured_content
            .as_ref()
            .and_then(|value| value.get("code"))
            .and_then(Value::as_str)
    }

    fn stdio_result_code(response: &Value) -> Option<&str> {
        response
            .pointer("/result/structuredContent/code")
            .and_then(Value::as_str)
    }

    fn exact_keys(value: &Value, expected: &[&str]) {
        let mut keys = value
            .as_object()
            .expect("exact object")
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>();
        keys.sort_unstable();
        let mut expected = expected.to_vec();
        expected.sort_unstable();
        assert_eq!(keys, expected);
    }

    fn assert_tag_output(value: &Value) {
        exact_keys(value, &["tag"]);
        exact_keys(&value["tag"], &["color", "id", "key", "name"]);
    }

    fn metric_counts(client: &AnytypeClient) -> (u64, u64) {
        let metrics = client.http_metrics();
        (metrics.logical_operations, metrics.physical_attempts)
    }

    fn metric_delta(before: (u64, u64), after: (u64, u64)) -> (u64, u64) {
        (
            after.0.checked_sub(before.0).expect("logical metrics grow"),
            after
                .1
                .checked_sub(before.1)
                .expect("physical metrics grow"),
        )
    }

    async fn tag_snapshot(
        client: &AnytypeClient,
        space_id: &str,
        property_id: &str,
    ) -> anytype::Result<Vec<Tag>> {
        let mut tags = client
            .tags(space_id, property_id)
            .limit(1_000)
            .list()
            .await?
            .into_response()
            .items;
        tags.sort_by(|left, right| left.id.cmp(&right.id));
        Ok(tags)
    }

    fn tag_from_snapshot(tags: &[Tag], tag_id: &str) -> Tag {
        let mut matches = tags.iter().filter(|tag| tag.id == tag_id);
        let tag = matches.next().expect("tag belongs to scoped snapshot");
        assert!(
            matches.next().is_none(),
            "tag identity is unique in snapshot"
        );
        tag.clone()
    }

    fn tag(id: &str, name: &str, key: &str, color: Color) -> Tag {
        Tag {
            object: DataModel::Tag,
            id: id.to_owned(),
            name: name.to_owned(),
            key: key.to_owned(),
            color,
        }
    }

    #[test]
    fn contracts_are_closed_non_null_bounded_and_exact() {
        let create = tag_create_tool().expect("tag_create contract");
        let update = tag_update_tool().expect("tag_update contract");
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
        assert_eq!(
            create_schema["required"],
            json!(["space", "property", "name"])
        );
        assert_eq!(
            create_schema["properties"]
                .as_object()
                .expect("create properties")
                .keys()
                .cloned()
                .collect::<Vec<_>>(),
            [
                "color",
                "idempotency_key",
                "key",
                "name",
                "property",
                "space"
            ]
        );
        let update_schema = serde_json::to_value(update_tool.input_schema.as_ref())
            .expect("serialize update schema");
        assert_eq!(update_schema["additionalProperties"], false);
        assert_eq!(
            update_schema["required"],
            json!(["space", "property", "tag_id"])
        );
        assert_eq!(update_schema["minProperties"], 4);
        assert_eq!(create_schema["$defs"]["PropertyRef"]["minLength"], 1);
        assert_eq!(create_schema["$defs"]["PropertyRef"]["maxLength"], 256);
        assert_eq!(update_schema["$defs"]["PropertyRef"]["minLength"], 1);
        assert_eq!(update_schema["$defs"]["PropertyRef"]["maxLength"], 256);

        let create_validator = jsonschema::draft202012::options()
            .build(&create_schema)
            .expect("compile create input schema");
        let update_validator = jsonschema::draft202012::options()
            .build(&update_schema)
            .expect("compile update input schema");
        for (value, expected) in [
            (
                json!({"space":SPACE_ID,"property":PROPERTY_ID,"name":"x".repeat(512)}),
                true,
            ),
            (
                json!({"space":SPACE_ID,"property":PROPERTY_ID,"name":"x".repeat(513)}),
                false,
            ),
            (
                json!({"space":SPACE_ID,"property":PROPERTY_ID,"name":"n","key":"tag_key","color":"lime"}),
                true,
            ),
            (
                json!({"space":SPACE_ID,"property":PROPERTY_ID,"name":"n","key":"Bad-Key"}),
                false,
            ),
        ] {
            assert_eq!(create_validator.is_valid(&value), expected, "{value}");
            assert_eq!(
                serde_json::from_value::<TagCreateInput>(value).is_ok(),
                expected
            );
        }
        for value in [
            json!({"space":SPACE_ID,"property":PROPERTY_ID,"name":"n","color":null}),
            json!({"space":SPACE_ID,"property":PROPERTY_ID,"name":"n","key":null}),
            json!({"space":SPACE_ID,"property":PROPERTY_ID,"name":"n","idempotency_key":null}),
            json!({"space":SPACE_ID,"property":PROPERTY_ID,"name":"n","unknown":true}),
        ] {
            assert!(!create_validator.is_valid(&value));
            assert!(serde_json::from_value::<TagCreateInput>(value).is_err());
        }
        for (property, expected) in [("p".repeat(256), true), ("p".repeat(257), false)] {
            let create_value = json!({"space":SPACE_ID,"property":property.clone(),"name":"n"});
            assert_eq!(create_validator.is_valid(&create_value), expected);
            assert_eq!(
                serde_json::from_value::<TagCreateInput>(create_value).is_ok(),
                expected
            );
            let update_value = json!({
                "space":SPACE_ID,
                "property":property,
                "tag_id":OTHER_TAG_ID,
                "color":"red"
            });
            assert_eq!(update_validator.is_valid(&update_value), expected);
            assert_eq!(
                serde_json::from_value::<TagUpdateInput>(update_value).is_ok(),
                expected
            );
        }
        for (value, expected) in [
            (
                json!({"space":SPACE_ID,"property":PROPERTY_ID,"tag_id":OTHER_TAG_ID,"name":"new"}),
                true,
            ),
            (
                json!({"space":SPACE_ID,"property":PROPERTY_ID,"tag_id":OTHER_TAG_ID}),
                false,
            ),
            (
                json!({"space":SPACE_ID,"property":PROPERTY_ID,"tag_id":"../unsafe","color":"red"}),
                false,
            ),
        ] {
            assert_eq!(update_validator.is_valid(&value), expected, "{value}");
            let decoded = serde_json::from_value::<TagUpdateInput>(value);
            assert_eq!(
                decoded.as_ref().is_ok_and(TagUpdateInput::has_mutation),
                expected
            );
        }
        for value in [
            json!({"space":SPACE_ID,"property":PROPERTY_ID,"tag_id":OTHER_TAG_ID,"name":null}),
            json!({"space":SPACE_ID,"property":PROPERTY_ID,"tag_id":OTHER_TAG_ID,"color":null}),
            json!({"space":SPACE_ID,"property":PROPERTY_ID,"tag_id":OTHER_TAG_ID,"unknown":true}),
        ] {
            assert!(!update_validator.is_valid(&value));
            assert!(serde_json::from_value::<TagUpdateInput>(value).is_err());
        }

        let output_schema = serde_json::to_value(create_tool.output_schema.as_ref())
            .expect("serialize output schema");
        assert_eq!(output_schema["additionalProperties"], false);
        assert_eq!(output_schema["required"], json!(["tag"]));
        assert_eq!(
            output_schema["properties"]
                .as_object()
                .expect("output properties")
                .keys()
                .cloned()
                .collect::<Vec<_>>(),
            ["tag"]
        );
        let tag_schema = &output_schema["$defs"]["TagSummary"];
        assert_eq!(tag_schema["additionalProperties"], false);
        assert_eq!(
            tag_schema["required"],
            json!(["id", "key", "name", "color"])
        );
        assert_eq!(
            tag_schema["properties"]
                .as_object()
                .expect("tag output properties")
                .keys()
                .cloned()
                .collect::<Vec<_>>(),
            ["color", "id", "key", "name"]
        );
        assert_eq!(
            serde_json::to_value(update_tool.output_schema.as_ref())
                .expect("serialize update output schema"),
            output_schema
        );
        let output_schema = output_schema.to_string();
        for forbidden in ["space", "property", "body", "token"] {
            assert!(!output_schema.contains(forbidden));
        }
        assert_eq!(schema_tag_tools().expect("tag slice").len(), 2);
    }

    #[test]
    fn matching_default_color_output_and_fingerprints_are_pure() {
        let id = EntityId::new(OTHER_TAG_ID).expect("tag id");
        let created = tag(OTHER_TAG_ID, "Urgent", "urgent", Color::Grey);
        let input: TagCreateInput = serde_json::from_value(json!({
            "space":SPACE_ID,
            "property":PROPERTY_ID,
            "name":"Urgent",
            "idempotency_key":"same-request"
        }))
        .expect("create input");
        let normalized = NormalizedTagCreate::from(input);
        assert_eq!(normalized.color, ProjectedColor::Grey);
        assert!(tag_matches_create(&created, &id, &normalized).expect("safe created tag"));
        assert!(
            !tag_matches_create(
                &tag(OTHER_TAG_ID, "Urgent", "urgent", Color::Red),
                &id,
                &normalized,
            )
            .expect("safe color mismatch")
        );
        assert!(
            checked_tag_summary(
                &tag(PROPERTY_ID, "Urgent", "urgent", Color::Grey),
                Some(&id),
            )
            .is_err()
        );
        let summary = checked_tag_summary(&created, Some(&id)).expect("tag summary");
        assert_eq!(
            serde_json::to_value(TagOutput::from(summary)).expect("serialize tag output"),
            json!({"tag":{"id":OTHER_TAG_ID,"key":"urgent","name":"Urgent","color":"grey"}})
        );
        let update: TagUpdateInput = serde_json::from_value(json!({
            "space":SPACE_ID,
            "property":PROPERTY_ID,
            "tag_id":OTHER_TAG_ID,
            "name":"Updated",
            "key":"updated",
            "color":"teal"
        }))
        .expect("update input");
        assert!(
            tag_matches_update(
                &tag(OTHER_TAG_ID, "Updated", "updated", Color::Teal),
                &update,
            )
            .expect("safe updated tag")
        );
        assert!(!tag_matches_update(&created, &update).expect("safe stale tag"));
        let empty_name = tag(OTHER_TAG_ID, "", "urgent", Color::Grey);
        let color_only: TagUpdateInput = serde_json::from_value(json!({
            "space":SPACE_ID,
            "property":PROPERTY_ID,
            "tag_id":OTHER_TAG_ID,
            "color":"grey"
        }))
        .expect("color-only update input");
        assert!(tag_matches_update(&empty_name, &color_only).expect("empty output display name"));

        let select: Property = serde_json::from_value(json!({
            "id":PROPERTY_ID,
            "key":"status",
            "name":"Status",
            "format":"select",
            "tags":null
        }))
        .expect("select property");
        let multi_select: Property = serde_json::from_value(json!({
            "id":PROPERTY_ID,
            "key":"labels",
            "name":"Labels",
            "format":"multi_select",
            "tags":null
        }))
        .expect("multi-select property");
        let text: Property = serde_json::from_value(json!({
            "id":PROPERTY_ID,
            "key":"notes",
            "name":"Notes",
            "format":"text",
            "tags":null
        }))
        .expect("text property");
        assert!(ensure_select_property(&select).is_ok());
        assert!(ensure_select_property(&multi_select).is_ok());
        assert!(ensure_select_property(&text).is_err());

        let same: TagCreateInput = serde_json::from_value(json!({
            "space":SPACE_ID,
            "property":PROPERTY_ID,
            "name":"Urgent",
            "idempotency_key":"other-key"
        }))
        .expect("same create input");
        let different: TagCreateInput = serde_json::from_value(json!({
            "space":SPACE_ID,
            "property":PROPERTY_ID,
            "name":"Urgent",
            "color":"red"
        }))
        .expect("different create input");
        assert_eq!(
            normalized.fingerprint(),
            NormalizedTagCreate::from(same).fingerprint()
        );
        assert_ne!(
            normalized.fingerprint(),
            NormalizedTagCreate::from(different).fingerprint()
        );
    }

    #[test]
    fn schema_mutation_certainty_and_scoped_page_evidence_are_exact() {
        for code in 300_u16..=599 {
            let error = AnytypeError::ApiError {
                code,
                method: "PATCH".to_owned(),
                url: "/redacted".to_owned(),
                message: "must-not-escape".to_owned(),
            };
            assert_eq!(
                schema_mutation_rejection_is_definitive(&error),
                matches!(code, 400 | 401 | 403 | 404 | 409 | 422),
                "HTTP {code}"
            );
        }
        assert!(!schema_mutation_rejection_is_definitive(
            &AnytypeError::RateLimitExceeded {
                header: "must-not-escape".to_owned(),
                duration: Duration::from_secs(1),
            }
        ));
        assert!(schema_mutation_rejection_is_definitive(
            &AnytypeError::Forbidden
        ));
        assert!(schema_mutation_rejection_is_definitive(
            &AnytypeError::Validation {
                message: "must-not-escape".to_owned(),
            }
        ));

        let id = EntityId::new(OTHER_TAG_ID).expect("tag id");
        let expected = tag(OTHER_TAG_ID, "", "tag_key", Color::Grey);
        let page = |items: Vec<Tag>, total: usize, has_more: bool, offset: u32| PaginatedResponse {
            items,
            pagination: anytype::paged::PaginationMeta {
                has_more,
                limit: 1_000,
                offset,
                total,
            },
        };
        assert_eq!(
            exact_owned_tag(page(vec![expected.clone()], 1, false, 0), &id)
                .expect("exact owned tag"),
            expected
        );
        assert_eq!(
            exact_owned_tag(page(Vec::new(), 0, false, 0), &id),
            Err(ScopedTagEvidenceError::Missing)
        );
        assert_eq!(
            exact_owned_tag(
                page(vec![expected.clone(), expected.clone()], 2, false, 0),
                &id,
            ),
            Err(ScopedTagEvidenceError::Duplicate)
        );
        for incomplete in [
            page(vec![expected.clone()], 2, false, 0),
            page(vec![expected.clone()], 1, true, 0),
            page(vec![expected], 1, false, 1),
        ] {
            assert_eq!(
                exact_owned_tag(incomplete, &id),
                Err(ScopedTagEvidenceError::Incomplete)
            );
        }
    }

    #[test]
    fn maximum_tag_result_has_a_locked_byte_and_token_budget() {
        let maximum = TagOutput {
            tag: TagSummary {
                id: EntityId::new("i".repeat(crate::domain::MAX_ENTITY_ID_CHARS))
                    .expect("maximum id"),
                key: TypeKey::new("k".repeat(crate::domain::MAX_TYPE_KEY_CHARS))
                    .expect("maximum key"),
                name: DisplayName::new("🦀".repeat(crate::domain::MAX_DISPLAY_NAME_CHARS))
                    .expect("maximum display name"),
                color: ProjectedColor::Purple,
            },
        };
        let result = tag_create_tool()
            .expect("tag create contract")
            .success(&maximum)
            .expect("maximum CallToolResult");
        let encoded = serde_json::to_string(&result).expect("maximum CallToolResult JSON");
        let tokenizer = o200k_base().expect("o200k tokenizer");
        let tokens = tokenizer.encode_with_special_tokens(&encoded).len();
        assert_eq!(encoded.len(), TAG_RESULT_MAX_BYTES);
        assert_eq!(tokens, TAG_RESULT_MAX_TOKENS);
    }

    large_async_test!(read_only_and_malformed_calls_reject_before_io, {
        for name in [TAG_CREATE, TAG_UPDATE] {
            let read_only = server(no_io_runtime(true), handlers());
            assert!(!read_only.tools().iter().any(|tool| tool.name == name));
            let result = read_only
                .dispatch_tool(
                    CallToolRequestParams::new(name).with_arguments(args(json!({
                        "secret":"must-not-decode"
                    }))),
                    &CancellationToken::new(),
                )
                .await
                .expect("read-only result");
            assert_eq!(result_code(&result), Some("validation"));
            assert!(!format!("{result:?}").contains("must-not-decode"));
            assert_eq!(
                read_only
                    .runtime()
                    .client()
                    .http_metrics()
                    .logical_operations,
                0
            );
        }

        for (name, arguments) in [
            (
                TAG_CREATE,
                json!({"space":SPACE_ID,"property":PROPERTY_ID,"name":"x".repeat(513)}),
            ),
            (
                TAG_UPDATE,
                json!({"space":SPACE_ID,"property":PROPERTY_ID,"tag_id":OTHER_TAG_ID}),
            ),
        ] {
            let invalid = server(no_io_runtime(false), handlers());
            let probe = invalid.clone();
            let response = preview_stdio_call(invalid, name, arguments).await;
            assert!(response.get("error").is_some() || response["result"]["isError"] == true);
            assert_eq!(
                probe.runtime().client().http_metrics().logical_operations,
                0
            );
        }

        let capacity_client = no_io_runtime(false);
        let capacity_server = server(
            capacity_client.clone(),
            SchemaTagHandlers::build(0, VerifyConfig::default()).expect("zero-capacity handlers"),
        );
        let capacity = direct(
            &capacity_server,
            TAG_CREATE,
            json!({
                "space":SPACE_ID,
                "property":PROPERTY_ID,
                "name":"capacity",
                "idempotency_key":"capacity-key"
            }),
        )
        .await;
        assert_eq!(result_code(&capacity), Some("bounded_result"));
        assert_eq!(
            capacity_client.client().http_metrics().logical_operations,
            0
        );
    });

    #[test]
    #[ignore = "requires env-only disposable credentials and an authenticated headless Anytype server"]
    fn headless_direct_stdio_cache_and_scope_use_disposable_real_space() {
        run_large_future(|| async {
            let outcome = Box::pin(with_disposable_space_context("any-mcp-schema-tag", |ctx| {
                Box::pin(async move {
                    ctx.client.ping_http().await.expect("authenticated HTTP");
                    ctx.client.ping_grpc().await.expect("cleanup-capable gRPC");
                    let suffix = unique_suffix().replace('-', "_");
                    let select_key = format!("mcp_tag_select_{suffix}");
                    let select_property = ctx
                        .client
                        .new_property(
                            &ctx.space_id,
                            format!("MCP tag select {suffix}"),
                            PropertyFormat::Select,
                        )
                        .key(&select_key)
                        .no_verify()
                        .no_cache_refresh()
                        .create()
                        .await?;
                    ctx.register_property(&select_property.id);
                    let text_key = format!("mcp_tag_text_{suffix}");
                    let text_property = ctx
                        .client
                        .new_property(
                            &ctx.space_id,
                            format!("MCP tag text {suffix}"),
                            PropertyFormat::Text,
                        )
                        .key(&text_key)
                        .no_verify()
                        .no_cache_refresh()
                        .create()
                        .await?;
                    ctx.register_property(&text_property.id);
                    let other_property = ctx
                        .client
                        .new_property(
                            &ctx.space_id,
                            format!("MCP tag other {suffix}"),
                            PropertyFormat::Select,
                        )
                        .key(format!("mcp_tag_other_{suffix}"))
                        .no_verify()
                        .no_cache_refresh()
                        .create()
                        .await?;
                    ctx.register_property(&other_property.id);
                    let other_tag = ctx
                        .client
                        .new_tag(&ctx.space_id, &other_property.id)
                        .name("Other property tag")
                        .key(format!("other_property_tag_{suffix}"))
                        .color(Color::Blue)
                        .no_verify()
                        .no_cache_refresh()
                        .create()
                        .await?;
                    let space = ctx.client.space(&ctx.space_id).get_direct().await?;

                    let mut cached_config = ctx.client.get_config().clone();
                    cached_config.disable_cache = false;
                    cached_config.app_name = format!("schema-tag-cache-{suffix}");
                    let cached_client = AnytypeClient::with_config(cached_config)?;
                    cached_client
                        .property(&ctx.space_id, &select_property.id)
                        .get()
                        .await?;
                    assert!(cached_client.cache().has_properties(&ctx.space_id));
                    let direct_server = server(runtime(cached_client.clone(), false), handlers());
                    let direct_key = format!("tag_direct_{suffix}");
                    let direct_arguments = json!({
                        "space":ctx.space_id,
                        "property":select_property.id,
                        "name":"Direct tag",
                        "key":direct_key,
                        "idempotency_key":format!("direct-{suffix}")
                    });
                    let before = metric_counts(&cached_client);
                    let created =
                        direct(&direct_server, TAG_CREATE, direct_arguments.clone()).await;
                    assert_eq!(created.is_error, Some(false), "direct create: {created:?}");
                    assert_eq!(metric_delta(before, metric_counts(&cached_client)), (3, 3));
                    assert!(!cached_client.cache().has_properties(&ctx.space_id));
                    let direct_id = created
                        .structured_content
                        .as_ref()
                        .and_then(|value| value.pointer("/tag/id"))
                        .and_then(Value::as_str)
                        .expect("direct created id")
                        .to_owned();
                    assert_eq!(
                        created.structured_content,
                        Some(json!({
                            "tag":{
                                "id":direct_id,
                                "key":direct_key,
                                "name":"Direct tag",
                                "color":"grey"
                            }
                        }))
                    );
                    assert_tag_output(
                        created
                            .structured_content
                            .as_ref()
                            .expect("direct create structured output"),
                    );
                    let repeat_before = metric_counts(&cached_client);
                    assert_eq!(
                        direct(&direct_server, TAG_CREATE, direct_arguments.clone()).await,
                        created
                    );
                    assert_eq!(metric_counts(&cached_client), repeat_before);
                    let changed_key = direct(
                        &direct_server,
                        TAG_CREATE,
                        json!({
                            "space":ctx.space_id,
                            "property":select_property.id,
                            "name":"Different fingerprint",
                            "key":format!("different_{suffix}"),
                            "idempotency_key":format!("direct-{suffix}")
                        }),
                    )
                    .await;
                    assert_eq!(result_code(&changed_key), Some("conflict"));
                    assert_eq!(metric_counts(&cached_client), repeat_before);
                    let independent = tag_from_snapshot(
                        &tag_snapshot(&ctx.client, &ctx.space_id, &select_property.id).await?,
                        &direct_id,
                    );
                    assert_eq!(independent.name, "Direct tag");
                    assert_eq!(independent.color, Color::Grey);

                    cached_client
                        .property(&ctx.space_id, &select_property.id)
                        .get()
                        .await?;
                    assert!(cached_client.cache().has_properties(&ctx.space_id));
                    let before = metric_counts(&cached_client);
                    let updated = direct(
                        &direct_server,
                        TAG_UPDATE,
                        json!({
                            "space":ctx.space_id,
                            "property":select_property.id,
                            "tag_id":direct_id.clone(),
                            "name":"Direct updated",
                            "color":"teal"
                        }),
                    )
                    .await;
                    assert_eq!(updated.is_error, Some(false), "direct update: {updated:?}");
                    assert_eq!(metric_delta(before, metric_counts(&cached_client)), (4, 4));
                    assert!(!cached_client.cache().has_properties(&ctx.space_id));
                    assert_tag_output(
                        updated
                            .structured_content
                            .as_ref()
                            .expect("direct update structured output"),
                    );

                    let concurrent_handlers = handlers();
                    let concurrent_server =
                        server(runtime(ctx.client.clone(), false), concurrent_handlers);
                    let concurrent_arguments = json!({
                        "space":ctx.space_id,
                        "property":select_property.id,
                        "name":"Concurrent tag",
                        "key":format!("concurrent_{suffix}"),
                        "idempotency_key":format!("concurrent-{suffix}")
                    });
                    let before = metric_counts(&ctx.client);
                    let first_server = concurrent_server.clone();
                    let first_arguments = concurrent_arguments.clone();
                    let first = tokio::spawn(async move {
                        direct(&first_server, TAG_CREATE, first_arguments).await
                    });
                    let second_server = concurrent_server.clone();
                    let second = tokio::spawn(async move {
                        direct(&second_server, TAG_CREATE, concurrent_arguments).await
                    });
                    let first = first.await.expect("first concurrent call");
                    let second = second.await.expect("second concurrent call");
                    assert_eq!(first, second);
                    assert_eq!(first.is_error, Some(false));
                    assert_eq!(metric_delta(before, metric_counts(&ctx.client)), (3, 3));

                    let stdio_key = format!("tag_stdio_{suffix}");
                    let stdio_handlers = handlers();
                    let before = metric_counts(&ctx.client);
                    let created = preview_stdio_call(
                        server(runtime(ctx.client.clone(), false), stdio_handlers.clone()),
                        TAG_CREATE,
                        json!({
                            "space":space.name,
                            "property":select_key,
                            "name":"Stdio tag",
                            "key":stdio_key,
                            "color":"purple",
                            "idempotency_key":format!("stdio-{suffix}")
                        }),
                    )
                    .await;
                    assert_eq!(
                        created["result"]["isError"], false,
                        "stdio create: {created}"
                    );
                    assert_tag_output(&created["result"]["structuredContent"]);
                    let create_delta = metric_delta(before, metric_counts(&ctx.client));
                    assert!(create_delta.0 <= TAG_CREATE_LOGICAL_CEILING as u64);
                    assert!(create_delta.1 <= TAG_CREATE_PHYSICAL_CEILING as u64);
                    let stdio_id = created["result"]["structuredContent"]["tag"]["id"]
                        .as_str()
                        .expect("stdio tag id")
                        .to_owned();
                    let stdio_property = ctx
                        .client
                        .property(&ctx.space_id, &select_property.id)
                        .get_direct()
                        .await?;
                    assert_eq!(stdio_property.id, select_property.id);
                    let stdio_tag = tag_from_snapshot(
                        &tag_snapshot(&ctx.client, &ctx.space_id, &select_property.id).await?,
                        &stdio_id,
                    );
                    assert_eq!(stdio_tag.id, stdio_id);
                    let before = metric_counts(&ctx.client);
                    let updated = preview_stdio_call(
                        server(runtime(ctx.client.clone(), false), stdio_handlers),
                        TAG_UPDATE,
                        json!({
                            "space":ctx.space_id,
                            "property":select_property.id,
                            "tag_id":stdio_id.clone(),
                            "name":"Stdio updated",
                            "color":"lime"
                        }),
                    )
                    .await;
                    let update_delta = metric_delta(before, metric_counts(&ctx.client));
                    assert_eq!(
                        updated["result"]["isError"], false,
                        "stdio update: {updated}; delta={update_delta:?}"
                    );
                    assert_eq!(update_delta, (4, 4));
                    assert_tag_output(&updated["result"]["structuredContent"]);

                    let before = metric_counts(&ctx.client);
                    let wrong_format = direct(
                        &server(runtime(ctx.client.clone(), false), handlers()),
                        TAG_CREATE,
                        json!({
                            "space":ctx.space_id,
                            "property":text_property.id,
                            "name":"Must not create"
                        }),
                    )
                    .await;
                    assert_eq!(result_code(&wrong_format), Some("validation"));
                    assert_eq!(metric_delta(before, metric_counts(&ctx.client)), (1, 1));
                    assert!(
                        ctx.client
                            .tags(&ctx.space_id, &text_property.id)
                            .limit(1)
                            .list()
                            .await?
                            .is_empty()
                    );

                    let primary_before = ctx
                        .client
                        .tag(&ctx.space_id, &select_property.id, &direct_id)
                        .get()
                        .await?;
                    let before = metric_counts(&ctx.client);
                    let wrong_format_update = direct(
                        &server(runtime(ctx.client.clone(), false), handlers()),
                        TAG_UPDATE,
                        json!({
                            "space":ctx.space_id,
                            "property":text_property.id,
                            "tag_id":direct_id,
                            "name":"Must not update"
                        }),
                    )
                    .await;
                    assert_eq!(result_code(&wrong_format_update), Some("validation"));
                    assert_eq!(metric_delta(before, metric_counts(&ctx.client)), (1, 1));
                    let wrong_format_stdio = preview_stdio_call(
                        server(runtime(ctx.client.clone(), false), handlers()),
                        TAG_UPDATE,
                        json!({
                            "space":ctx.space_id,
                            "property":text_property.id,
                            "tag_id":direct_id,
                            "name":"Must not update"
                        }),
                    )
                    .await;
                    assert_eq!(stdio_result_code(&wrong_format_stdio), Some("validation"));
                    assert_eq!(
                        ctx.client
                            .tag(&ctx.space_id, &select_property.id, &direct_id)
                            .get()
                            .await?,
                        primary_before
                    );

                    let other_before = ctx
                        .client
                        .tag(&ctx.space_id, &other_property.id, &other_tag.id)
                        .get()
                        .await?;
                    for through_stdio in [false, true] {
                        let arguments = json!({
                            "space":ctx.space_id,
                            "property":select_property.id,
                            "tag_id":other_tag.id,
                            "name":"Cross property must not update"
                        });
                        if through_stdio {
                            let response = preview_stdio_call(
                                server(runtime(ctx.client.clone(), false), handlers()),
                                TAG_UPDATE,
                                arguments,
                            )
                            .await;
                            assert_eq!(stdio_result_code(&response), Some("not_found"));
                        } else {
                            let response = direct(
                                &server(runtime(ctx.client.clone(), false), handlers()),
                                TAG_UPDATE,
                                arguments,
                            )
                            .await;
                            assert_eq!(result_code(&response), Some("not_found"));
                        }
                    }
                    assert_eq!(
                        ctx.client
                            .tag(&ctx.space_id, &other_property.id, &other_tag.id)
                            .get()
                            .await?,
                        other_before
                    );
                    assert_eq!(
                        ctx.client
                            .tag(&ctx.space_id, &select_property.id, &direct_id)
                            .get()
                            .await?,
                        primary_before
                    );

                    let prefix = std::env::var("ANYTYPE_TEST_SPACE_PREFIX")
                        .expect("disposable prefix admitted before callback");
                    let other_space = ctx
                        .create_space_fixture(format!("{prefix}-tag-other-{}", unique_suffix()))
                        .await?;
                    let other_space_property = ctx
                        .client
                        .new_property(
                            &other_space.id,
                            format!("Other space property {suffix}"),
                            PropertyFormat::Select,
                        )
                        .key(format!("other_space_property_{suffix}"))
                        .no_verify()
                        .no_cache_refresh()
                        .create()
                        .await?;
                    let other_space_tag = ctx
                        .client
                        .new_tag(&other_space.id, &other_space_property.id)
                        .name("Other space tag")
                        .key(format!("other_space_tag_{suffix}"))
                        .color(Color::Orange)
                        .no_verify()
                        .no_cache_refresh()
                        .create()
                        .await?;
                    let other_space_before = other_space_tag.clone();
                    for through_stdio in [false, true] {
                        let arguments = json!({
                            "space":ctx.space_id,
                            "property":other_space_property.id,
                            "tag_id":other_space_tag.id,
                            "name":"Cross space must not update"
                        });
                        if through_stdio {
                            let response = preview_stdio_call(
                                server(runtime(ctx.client.clone(), false), handlers()),
                                TAG_UPDATE,
                                arguments,
                            )
                            .await;
                            assert_eq!(stdio_result_code(&response), Some("not_found"));
                        } else {
                            let response = direct(
                                &server(runtime(ctx.client.clone(), false), handlers()),
                                TAG_UPDATE,
                                arguments,
                            )
                            .await;
                            assert_eq!(result_code(&response), Some("not_found"));
                        }
                    }
                    assert_eq!(
                        ctx.client
                            .tag(
                                &other_space.id,
                                &other_space_property.id,
                                &other_space_tag.id,
                            )
                            .get()
                            .await?,
                        other_space_before
                    );

                    let ambiguous_name = format!("{prefix}-tag-ambiguous-{}", unique_suffix());
                    let first_ambiguous = ctx.create_space_fixture(&ambiguous_name).await?;
                    let second_ambiguous = ctx.create_space_fixture(&ambiguous_name).await?;
                    assert_ne!(first_ambiguous.id, second_ambiguous.id);
                    for through_stdio in [false, true] {
                        let arguments = json!({
                            "space":ambiguous_name,
                            "property":select_property.id,
                            "name":"Must not create in ambiguity"
                        });
                        if through_stdio {
                            let response = preview_stdio_call(
                                server(runtime(ctx.client.clone(), false), handlers()),
                                TAG_CREATE,
                                arguments,
                            )
                            .await;
                            assert_eq!(stdio_result_code(&response), Some("ambiguous"));
                        } else {
                            let response = direct(
                                &server(runtime(ctx.client.clone(), false), handlers()),
                                TAG_CREATE,
                                arguments,
                            )
                            .await;
                            assert_eq!(result_code(&response), Some("ambiguous"));
                        }
                    }

                    let invalid_token = "invalid-tag-token-must-stay-redacted";
                    let mut invalid_config = ctx.client.get_config().clone();
                    invalid_config.app_name = format!("schema-tag-invalid-{suffix}");
                    let invalid_client = AnytypeClient::with_config(invalid_config)?;
                    invalid_client.set_api_key(HttpCredentials::new(invalid_token));
                    let invalid_direct = direct(
                        &server(runtime(invalid_client.clone(), false), handlers()),
                        TAG_CREATE,
                        json!({
                            "space":ctx.space_id,
                            "property":select_property.id,
                            "name":"Unauthorized direct"
                        }),
                    )
                    .await;
                    assert_eq!(result_code(&invalid_direct), Some("authentication"));
                    assert!(!format!("{invalid_direct:?}").contains(invalid_token));
                    let invalid_stdio = preview_stdio_call(
                        server(runtime(invalid_client, false), handlers()),
                        TAG_CREATE,
                        json!({
                            "space":ctx.space_id,
                            "property":select_property.id,
                            "name":"Unauthorized stdio"
                        }),
                    )
                    .await;
                    assert_eq!(stdio_result_code(&invalid_stdio), Some("authentication"));
                    assert!(!invalid_stdio.to_string().contains(invalid_token));

                    let before_cancel_snapshot =
                        tag_snapshot(&ctx.client, &ctx.space_id, &select_property.id).await?;
                    let before_create_hooks = DispatchHooks {
                        before_create: Some(Arc::new(CancellationToken::cancel)),
                        ..DispatchHooks::default()
                    };
                    let before_create_token = CancellationToken::new();
                    let before_create = direct_with_cancellation(
                        &server(
                            runtime(ctx.client.clone(), false),
                            handlers().with_dispatch_hooks(before_create_hooks),
                        ),
                        TAG_CREATE,
                        json!({
                            "space":ctx.space_id,
                            "property":select_property.id,
                            "name":"Cancelled before create"
                        }),
                        &before_create_token,
                    )
                    .await;
                    assert_eq!(result_code(&before_create), Some("upstream"));
                    assert_eq!(
                        tag_snapshot(&ctx.client, &ctx.space_id, &select_property.id).await?,
                        before_cancel_snapshot
                    );

                    let after_create_hooks = DispatchHooks {
                        after_create_mark: Some(Arc::new(CancellationToken::cancel)),
                        ..DispatchHooks::default()
                    };
                    let after_create_token = CancellationToken::new();
                    let after_create = direct_with_cancellation(
                        &server(
                            runtime(ctx.client.clone(), false),
                            handlers().with_dispatch_hooks(after_create_hooks),
                        ),
                        TAG_CREATE,
                        json!({
                            "space":ctx.space_id,
                            "property":select_property.id,
                            "name":"Cancelled after create mark"
                        }),
                        &after_create_token,
                    )
                    .await;
                    assert_eq!(result_code(&after_create), Some("conflict"));
                    assert_eq!(
                        tag_snapshot(&ctx.client, &ctx.space_id, &select_property.id).await?,
                        before_cancel_snapshot
                    );

                    let before_update_hooks = DispatchHooks {
                        before_update: Some(Arc::new(CancellationToken::cancel)),
                        ..DispatchHooks::default()
                    };
                    let before_update_token = CancellationToken::new();
                    let before_update = direct_with_cancellation(
                        &server(
                            runtime(ctx.client.clone(), false),
                            handlers().with_dispatch_hooks(before_update_hooks.clone()),
                        ),
                        TAG_UPDATE,
                        json!({
                            "space":ctx.space_id,
                            "property":select_property.id,
                            "tag_id":direct_id,
                            "name":"Cancelled before update"
                        }),
                        &before_update_token,
                    )
                    .await;
                    assert_eq!(result_code(&before_update), Some("upstream"));
                    preview_stdio_cancelled_call(
                        server(
                            runtime(ctx.client.clone(), false),
                            handlers().with_dispatch_hooks(before_update_hooks),
                        ),
                        TAG_UPDATE,
                        json!({
                            "space":ctx.space_id,
                            "property":select_property.id,
                            "tag_id":direct_id,
                            "name":"Cancelled before update"
                        }),
                    )
                    .await;
                    assert_eq!(
                        ctx.client
                            .tag(&ctx.space_id, &select_property.id, &direct_id)
                            .get()
                            .await?,
                        primary_before
                    );

                    let after_update_hooks = DispatchHooks {
                        after_update_mark: Some(Arc::new(CancellationToken::cancel)),
                        ..DispatchHooks::default()
                    };
                    let after_update_token = CancellationToken::new();
                    let after_update = direct_with_cancellation(
                        &server(
                            runtime(ctx.client.clone(), false),
                            handlers().with_dispatch_hooks(after_update_hooks.clone()),
                        ),
                        TAG_UPDATE,
                        json!({
                            "space":ctx.space_id,
                            "property":select_property.id,
                            "tag_id":direct_id,
                            "name":"Cancelled after update mark"
                        }),
                        &after_update_token,
                    )
                    .await;
                    assert_eq!(result_code(&after_update), Some("conflict"));
                    preview_stdio_cancelled_call(
                        server(
                            runtime(ctx.client.clone(), false),
                            handlers().with_dispatch_hooks(after_update_hooks),
                        ),
                        TAG_UPDATE,
                        json!({
                            "space":ctx.space_id,
                            "property":select_property.id,
                            "tag_id":direct_id,
                            "name":"Cancelled after update mark"
                        }),
                    )
                    .await;
                    assert_eq!(
                        ctx.client
                            .tag(&ctx.space_id, &select_property.id, &direct_id)
                            .get()
                            .await?,
                        primary_before
                    );
                    Ok(())
                })
            }))
            .await
            .expect("cleanup-safe live schema-tag workflow");
            match outcome {
                DisposableRun::Completed(()) => {}
                DisposableRun::Skipped(reason) => {
                    eprintln!("disposable schema-tag suite skipped before callback: {reason:?}");
                }
            }
        });
    }

    #[test]
    fn reviewed_work_ceilings_are_locked_without_fault_injection() {
        assert_eq!(TAG_CREATE_LOGICAL_CEILING, 34);
        assert_eq!(TAG_CREATE_PHYSICAL_CEILING, 199);
        assert_eq!(TAG_UPDATE_LOGICAL_CEILING, 35);
        assert_eq!(TAG_UPDATE_PHYSICAL_CEILING, 205);
    }
}
