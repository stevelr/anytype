// any-mcp - bounded, workflow-oriented MCP server for Anytype
//
// SPDX-FileCopyrightText: 2026 Steve Schoettler
// SPDX-License-Identifier: Apache-2.0

//! Optional schema-toolset workflows for bounded property mutations.
//!
//! The production `schema` descriptor composes this reviewed slice with the
//! space, type, and tag slices.

use std::{
    borrow::Cow,
    collections::{HashMap, HashSet},
    fmt,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use anytype::{
    prelude::{AnytypeClient, Color, CreateTagRequest, VerifyConfig, verify_semantic},
    properties::Property,
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
    error::{ToolError, mutation_rejection_is_definitive},
    handler_support::{
        HandlerError, HandlerOperationError, MutationAccess, MutationProgress, MutationStage,
        execute_mutation_handler, require_mutation_access,
    },
    optional_toolsets::{OptionalRegistryFuture, OptionalRegistryTool},
    protocol::{ToolProfile, WorkflowTool, workflow_tool},
    result::tool_error,
    runtime::{OperationContext, RuntimeContext},
    schema::SchemaContractError,
    schema_space_toolset::InputName,
    schema_type_toolset::{PropertyFormat, SchemaKey},
    server::decode_arguments,
    validation::{Omittable, optional_non_null_schema},
};

/// Exact property creation tool name.
pub const PROPERTY_CREATE: &str = "property_create";
/// Exact property update tool name.
pub const PROPERTY_UPDATE: &str = "property_update";
/// Reviewed logical HTTP ceiling for property creation.
pub const PROPERTY_CREATE_LOGICAL_CEILING: usize = 23;
/// Reviewed physical HTTP ceiling for property creation.
pub const PROPERTY_CREATE_PHYSICAL_CEILING: usize = 133;
/// Reviewed logical HTTP ceiling for property update.
pub const PROPERTY_UPDATE_LOGICAL_CEILING: usize = 34;
/// Reviewed physical HTTP ceiling for property update.
pub const PROPERTY_UPDATE_PHYSICAL_CEILING: usize = 199;

const MAX_PROPERTY_REFERENCE_CHARS: usize = 256;
const MAX_TAGS: usize = 20;
const PROPERTY_CREATE_FINGERPRINT_DOMAIN: &str = "any-mcp/schema-property-create/v1";

type PropertyCreateObserver = Arc<dyn Fn(&Property) -> Result<(), ()> + Send + Sync>;
type BeforePostHook = Arc<dyn Fn(&CancellationToken) + Send + Sync>;
type BeforePatchHook = Arc<dyn Fn(&CancellationToken) + Send + Sync>;

#[derive(Clone, Default)]
struct PropertyCreateHooks {
    observer: Option<PropertyCreateObserver>,
    before_post: Option<BeforePostHook>,
}

/// A bounded property key or stable identifier.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct PropertyReference(String);

impl PropertyReference {
    /// Validates a nonempty property reference without normalization.
    pub fn new(value: impl Into<String>) -> Result<Self, PropertyInputError> {
        let value = value.into();
        if value.is_empty() || value.trim().is_empty() {
            return Err(PropertyInputError::InvalidReference);
        }
        if value.chars().count() > MAX_PROPERTY_REFERENCE_CHARS {
            return Err(PropertyInputError::InvalidReference);
        }
        Ok(Self(value))
    }

    /// Borrows the exact validated spelling.
    #[must_use]
    pub fn as_str(&self) -> &str {
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
        json_schema!({"type":"string","minLength":1,"maxLength":MAX_PROPERTY_REFERENCE_CHARS})
    }
}

/// Closed tag color accepted and emitted by schema workflows.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SchemaColor {
    /// Neutral grey.
    #[default]
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
    /// Ice blue.
    Ice,
    /// Teal.
    Teal,
    /// Lime.
    Lime,
}

impl From<SchemaColor> for Color {
    fn from(value: SchemaColor) -> Self {
        match value {
            SchemaColor::Grey => Self::Grey,
            SchemaColor::Yellow => Self::Yellow,
            SchemaColor::Orange => Self::Orange,
            SchemaColor::Red => Self::Red,
            SchemaColor::Pink => Self::Pink,
            SchemaColor::Purple => Self::Purple,
            SchemaColor::Blue => Self::Blue,
            SchemaColor::Ice => Self::Ice,
            SchemaColor::Teal => Self::Teal,
            SchemaColor::Lime => Self::Lime,
        }
    }
}

impl From<Color> for SchemaColor {
    fn from(value: Color) -> Self {
        match value {
            Color::Grey => Self::Grey,
            Color::Yellow => Self::Yellow,
            Color::Orange => Self::Orange,
            Color::Red => Self::Red,
            Color::Pink => Self::Pink,
            Color::Purple => Self::Purple,
            Color::Blue => Self::Blue,
            Color::Ice => Self::Ice,
            Color::Teal => Self::Teal,
            Color::Lime => Self::Lime,
        }
    }
}

/// One bounded tag definition embedded in property creation.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PropertyTagSpec {
    /// Exact nonempty display name.
    name: InputName,
    /// Optional explicit mutation-safe key.
    #[serde(default)]
    #[schemars(schema_with = "optional_schema_key_schema")]
    key: Omittable<SchemaKey>,
    /// Required closed color.
    color: SchemaColor,
}

#[derive(Debug, Clone)]
struct PropertyTagBatch(Vec<PropertyTagSpec>);

impl<'de> Deserialize<'de> for PropertyTagBatch {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let tags = Vec::<PropertyTagSpec>::deserialize(deserializer)?;
        if !(1..=MAX_TAGS).contains(&tags.len()) {
            return Err(de::Error::custom(PropertyInputError::TagCount));
        }
        let mut keys = HashSet::with_capacity(tags.len());
        if tags
            .iter()
            .filter_map(|tag| tag.key.as_ref())
            .any(|key| !keys.insert(key.as_str()))
        {
            return Err(de::Error::custom(PropertyInputError::DuplicateTagKey));
        }
        Ok(Self(tags))
    }
}

impl JsonSchema for PropertyTagBatch {
    fn schema_name() -> Cow<'static, str> {
        "PropertyTagBatch".into()
    }

    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        let items = generator.subschema_for::<PropertyTagSpec>();
        json_schema!({"type":"array","minItems":1,"maxItems":MAX_TAGS,"items":items})
    }
}

/// Fixed property-input validation failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PropertyInputError {
    /// A property reference was empty or too long.
    InvalidReference,
    /// A tag batch was empty or exceeded twenty items.
    TagCount,
    /// Explicit tag keys were duplicated.
    DuplicateTagKey,
}

impl fmt::Display for PropertyInputError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidReference => "invalid bounded property reference",
            Self::TagCount => "tag batch is outside its item bounds",
            Self::DuplicateTagKey => "explicit tag keys must be unique",
        })
    }
}

impl std::error::Error for PropertyInputError {}

/// Exact input for `property_create`.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PropertyCreateInput {
    /// Unique space name or stable identifier.
    space: DiscoveryReference,
    /// Exact nonempty property name.
    name: InputName,
    /// Closed property format.
    format: PropertyFormat,
    /// Optional explicit property key.
    #[serde(default)]
    #[schemars(schema_with = "optional_schema_key_schema")]
    key: Omittable<SchemaKey>,
    /// Optional 1..20 tag definitions, legal only for select formats.
    #[serde(default)]
    #[schemars(schema_with = "optional_tag_batch_schema")]
    tags: Omittable<PropertyTagBatch>,
    /// Optional process-local create retry key.
    #[serde(default)]
    #[schemars(schema_with = "optional_idempotency_schema")]
    idempotency_key: Omittable<IdempotencyKey>,
}

/// Exact input for `property_update`.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PropertyUpdateInput {
    /// Unique space name or stable identifier.
    space: DiscoveryReference,
    /// Property key or stable identifier.
    property: PropertyReference,
    /// Required replacement name.
    name: InputName,
    /// Optional replacement key.
    #[serde(default)]
    #[schemars(schema_with = "optional_schema_key_schema")]
    key: Omittable<SchemaKey>,
}

fn optional_schema_key_schema(generator: &mut SchemaGenerator) -> Schema {
    optional_non_null_schema::<SchemaKey>(generator)
}

fn optional_tag_batch_schema(generator: &mut SchemaGenerator) -> Schema {
    optional_non_null_schema::<PropertyTagBatch>(generator)
}

fn optional_idempotency_schema(generator: &mut SchemaGenerator) -> Schema {
    optional_non_null_schema::<IdempotencyKey>(generator)
}

/// Minimized exact property output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PropertySummary {
    /// Stable property identifier.
    id: EntityId,
    /// Bounded display name.
    name: DisplayName,
    /// Bounded upstream key without mutation grammar claims.
    key: crate::domain::TypeKey,
    /// Closed property format.
    format: PropertyFormat,
}

/// Minimized exact tag output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TagSummary {
    /// Stable tag identifier.
    id: EntityId,
    /// Bounded display name.
    name: DisplayName,
    /// Bounded upstream key without mutation grammar claims.
    key: crate::domain::TypeKey,
    /// Closed tag color.
    color: SchemaColor,
}

/// Exact output for property creation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PropertyCreateOutput {
    /// Verified property projection.
    property: PropertySummary,
    /// Exact verified tag set, empty when tags were omitted.
    #[schemars(schema_with = "output_tags_schema")]
    tags: Vec<TagSummary>,
}

fn output_tags_schema(generator: &mut SchemaGenerator) -> Schema {
    let items = generator.subschema_for::<TagSummary>();
    json_schema!({"type":"array","minItems":0,"maxItems":MAX_TAGS,"items":items})
}

/// Exact output for property update.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PropertyUpdateOutput {
    /// Verified property projection.
    property: PropertySummary,
}

/// Builds the exact property-create contract.
pub fn property_create_tool() -> Result<WorkflowTool<PropertyCreateOutput>, SchemaContractError> {
    workflow_tool::<PropertyCreateInput, PropertyCreateOutput>(
        PROPERTY_CREATE,
        "Create one bounded Anytype property with an optional finite tag set.",
        ToolProfile::Create,
    )
}

/// Builds the exact property-update contract.
pub fn property_update_tool() -> Result<WorkflowTool<PropertyUpdateOutput>, SchemaContractError> {
    workflow_tool::<PropertyUpdateInput, PropertyUpdateOutput>(
        PROPERTY_UPDATE,
        "Update one exact Anytype property name and optional key.",
        ToolProfile::Update,
    )
}

/// Returns the complete property slice for terminal registry composition.
pub fn schema_property_tools() -> Result<Vec<OptionalRegistryTool>, SchemaContractError> {
    Ok(vec![
        OptionalRegistryTool::mutation(property_create_tool()?),
        OptionalRegistryTool::mutation(property_update_tool()?),
    ])
}

/// Stateful transport-neutral handlers for the schema property slice.
#[derive(Clone)]
pub struct SchemaPropertyHandlers {
    idempotency: Arc<IdempotencyStore>,
    verify_config: VerifyConfig,
    create_contract: WorkflowTool<PropertyCreateOutput>,
    update_contract: WorkflowTool<PropertyUpdateOutput>,
    create_observer: Option<PropertyCreateObserver>,
    before_post: Option<BeforePostHook>,
    before_patch: Option<BeforePatchHook>,
}

impl fmt::Debug for SchemaPropertyHandlers {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SchemaPropertyHandlers")
            .field("verify_config", &self.verify_config)
            .field("create_observer", &self.create_observer.is_some())
            .field("before_post", &self.before_post.is_some())
            .field("before_patch", &self.before_patch.is_some())
            .finish_non_exhaustive()
    }
}

impl SchemaPropertyHandlers {
    /// Creates handlers with the reviewed finite verification and idempotency bounds.
    pub fn new() -> Result<Self, SchemaContractError> {
        Self::build(DEFAULT_IDEMPOTENCY_CAPACITY, VerifyConfig::default(), None)
    }

    fn build(
        capacity: usize,
        verify_config: VerifyConfig,
        create_observer: Option<PropertyCreateObserver>,
    ) -> Result<Self, SchemaContractError> {
        Ok(Self {
            idempotency: Arc::new(IdempotencyStore::new(capacity)),
            verify_config,
            create_contract: property_create_tool()?,
            update_contract: property_update_tool()?,
            create_observer,
            before_post: None,
            before_patch: None,
        })
    }

    #[cfg(test)]
    fn with_before_patch_hook(mut self, hook: BeforePatchHook) -> Self {
        self.before_patch = Some(hook);
        self
    }

    #[cfg(test)]
    fn with_before_post_hook(mut self, hook: BeforePostHook) -> Self {
        self.before_post = Some(hook);
        self
    }

    /// Dispatches one schema-property tool after the caller's catalog gate.
    pub fn call_tool<'a>(
        &'a self,
        request: CallToolRequestParams,
        runtime: &'a RuntimeContext,
        cancellation: &'a CancellationToken,
    ) -> OptionalRegistryFuture<'a, Result<CallToolResult, ErrorData>> {
        Box::pin(async move {
            if runtime.is_read_only()
                && matches!(request.name.as_ref(), PROPERTY_CREATE | PROPERTY_UPDATE)
            {
                return Ok(tool_error(&ToolError::validation()));
            }
            match request.name.as_ref() {
                PROPERTY_CREATE => {
                    let input = decode_arguments::<PropertyCreateInput>(request.arguments)?;
                    Ok(Box::pin(self.property_create(
                        runtime,
                        MutationAccess::Allowed,
                        input,
                        cancellation,
                    ))
                    .await)
                }
                PROPERTY_UPDATE => {
                    let input = decode_arguments::<PropertyUpdateInput>(request.arguments)?;
                    Ok(Box::pin(self.property_update(
                        runtime,
                        MutationAccess::Allowed,
                        input,
                        cancellation,
                    ))
                    .await)
                }
                _ => Err(ErrorData::method_not_found::<CallToolRequestMethod>()),
            }
        })
    }

    async fn property_create(
        &self,
        runtime: &RuntimeContext,
        access: MutationAccess,
        input: PropertyCreateInput,
        cancellation: &CancellationToken,
    ) -> CallToolResult {
        if let Err(error) = require_mutation_access(access) {
            return tool_error(error.tool_error());
        }
        if input.tags.as_ref().is_some()
            && !matches!(
                input.format,
                PropertyFormat::Select | PropertyFormat::MultiSelect
            )
        {
            return tool_error(&ToolError::validation());
        }
        let normalized = NormalizedPropertyCreate::from(input);
        let Some(key) = normalized.idempotency_key.clone() else {
            let progress = MutationProgress::new();
            return execute_property_create(
                runtime,
                &self.create_contract,
                normalized,
                cancellation,
                &progress,
                &self.verify_config,
                PropertyCreateHooks {
                    observer: self.create_observer.clone(),
                    before_post: self.before_post.clone(),
                },
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
            BeginAttempt::Expired => tool_error(&ToolError::upstream()),
            BeginAttempt::Wait(attempt) => wait_for_attempt(attempt, cancellation).await,
            BeginAttempt::Lead(attempt) => {
                let runtime = runtime.clone();
                let supervisor_runtime = runtime.clone();
                let contract = self.create_contract.clone();
                let store = self.idempotency.clone();
                let task_attempt = attempt.clone();
                let verify_config = self.verify_config.clone();
                let observer = self.create_observer.clone();
                let before_post = self.before_post.clone();
                runtime.spawn_invocation_controller("schema_property_create", move || async move {
                    supervise_property_create(PropertyCreateSupervision {
                        runtime: supervisor_runtime,
                        contract,
                        store,
                        key,
                        attempt: task_attempt,
                        normalized,
                        verify_config,
                        observer,
                        before_post,
                    })
                    .await;
                });
                wait_for_attempt(attempt, cancellation).await
            }
        }
    }

    async fn property_update(
        &self,
        runtime: &RuntimeContext,
        access: MutationAccess,
        input: PropertyUpdateInput,
        cancellation: &CancellationToken,
    ) -> CallToolResult {
        if let Err(error) = require_mutation_access(access) {
            return tool_error(error.tool_error());
        }
        let client = runtime.client().clone();
        let verify_config = self.verify_config.clone();
        let progress = MutationProgress::new();
        let operation_progress = progress.clone();
        let operation_cancellation = cancellation.clone();
        let before_patch = self.before_patch.clone();
        execute_mutation_handler(
            runtime,
            &self.update_contract,
            OperationContext::new(PROPERTY_UPDATE),
            cancellation,
            &progress,
            Box::pin(async move {
                let (space_id, property_id) =
                    resolve_property(&client, &input.space, &input.property).await?;
                let current = client
                    .property(space_id.as_str(), property_id.as_str())
                    .get_direct()
                    .await?;
                let current_summary = checked_property_summary(&current, Some(&property_id))
                    .map_err(HandlerOperationError::from)?;
                if property_matches_update(&current, &property_id, &input, current.format()) {
                    return Ok(PropertyUpdateOutput {
                        property: current_summary,
                    });
                }

                let mut request = client
                    .update_property(space_id.as_str(), property_id.as_str())
                    .name(input.name.as_str())
                    .no_verify()
                    .no_cache_refresh();
                if let Some(key) = input.key.as_ref() {
                    request = request.key(key.as_str());
                }
                if let Some(hook) = before_patch {
                    hook(&operation_cancellation);
                }
                if operation_cancellation.is_cancelled() {
                    return Err(HandlerError::new(ToolError::upstream()).into());
                }
                operation_progress.mark_dispatched(runtime)?;
                let response_anomaly = match request.update().await {
                    Ok(returned) => {
                        !property_matches_update(&returned, &property_id, &input, current.format())
                    }
                    Err(error) if mutation_rejection_is_definitive(&error) => {
                        return Err(error.into());
                    }
                    Err(_) => true,
                };
                let verified = verify_semantic(
                    &verify_config,
                    "property",
                    property_id.as_str(),
                    || {
                        client
                            .property(space_id.as_str(), property_id.as_str())
                            .get_direct()
                    },
                    |property| {
                        property_matches_update(property, &property_id, &input, current.format())
                    },
                )
                .await
                .map_err(|_| indeterminate_operation())?;
                if response_anomaly {
                    return Err(indeterminate_operation());
                }
                checked_property_summary(&verified, Some(&property_id))
                    .map(|property| PropertyUpdateOutput { property })
                    .map_err(|_| indeterminate_operation())
            }),
            |output| Box::pin(async move { Ok(output) }),
        )
        .await
    }
}

#[derive(Clone)]
struct NormalizedPropertyCreate {
    space: DiscoveryReference,
    name: InputName,
    format: PropertyFormat,
    key: Option<SchemaKey>,
    tags: Vec<PropertyTagSpec>,
    idempotency_key: Option<IdempotencyKey>,
}

impl From<PropertyCreateInput> for NormalizedPropertyCreate {
    fn from(input: PropertyCreateInput) -> Self {
        Self {
            space: input.space,
            name: input.name,
            format: input.format,
            key: input.key.as_ref().cloned(),
            tags: input
                .tags
                .as_ref()
                .map_or_else(Vec::new, |tags| tags.0.clone()),
            idempotency_key: input.idempotency_key.as_ref().cloned(),
        }
    }
}

impl NormalizedPropertyCreate {
    fn fingerprint(&self) -> [u8; 32] {
        let mut hasher = Sha256::new();
        hash_field(&mut hasher, PROPERTY_CREATE_FINGERPRINT_DOMAIN);
        hash_field(&mut hasher, self.space.as_str());
        hash_field(&mut hasher, self.name.as_str());
        hash_field(&mut hasher, property_format_name(self.format));
        hash_optional(&mut hasher, self.key.as_ref().map(SchemaKey::as_str));
        hasher.update(self.tags.len().to_be_bytes());
        for tag in &self.tags {
            hash_field(&mut hasher, tag.name.as_str());
            hash_optional(&mut hasher, tag.key.as_ref().map(SchemaKey::as_str));
            hash_field(&mut hasher, schema_color_name(tag.color));
        }
        hasher.finalize().into()
    }
}

struct PropertyCreateSupervision {
    runtime: RuntimeContext,
    contract: WorkflowTool<PropertyCreateOutput>,
    store: Arc<IdempotencyStore>,
    key: IdempotencyKey,
    attempt: Arc<Attempt>,
    normalized: NormalizedPropertyCreate,
    verify_config: VerifyConfig,
    observer: Option<PropertyCreateObserver>,
    before_post: Option<BeforePostHook>,
}

async fn supervise_property_create(supervision: PropertyCreateSupervision) {
    let PropertyCreateSupervision {
        runtime,
        contract,
        store,
        key,
        attempt,
        normalized,
        verify_config,
        observer,
        before_post,
    } = supervision;
    let progress = attempt.progress();
    let task_progress = progress.clone();
    let task_runtime = runtime.clone();
    let task = runtime.spawn_invocation_supervisor(async move {
        execute_property_create(
            &task_runtime,
            &contract,
            normalized,
            &CancellationToken::new(),
            &task_progress,
            &verify_config,
            PropertyCreateHooks {
                observer,
                before_post,
            },
        )
        .await
    });
    let execution = finish_supervised_execution(task, &progress).await;
    store.finish(&key, &attempt, execution).await;
}

async fn execute_property_create(
    runtime: &RuntimeContext,
    contract: &WorkflowTool<PropertyCreateOutput>,
    input: NormalizedPropertyCreate,
    cancellation: &CancellationToken,
    progress: &MutationProgress,
    verify_config: &VerifyConfig,
    hooks: PropertyCreateHooks,
) -> CreateExecution {
    let client = runtime.client().clone();
    let definitive_rejection = Arc::new(AtomicBool::new(false));
    let operation_rejection = definitive_rejection.clone();
    let operation_progress = progress.clone();
    let verify_config = verify_config.clone();
    let result = execute_mutation_handler(
        runtime,
        contract,
        OperationContext::new(PROPERTY_CREATE),
        cancellation,
        progress,
        Box::pin(async move {
            let resolved = client.resolve_space_id(input.space.as_str()).await?;
            let space_id = EntityId::new(resolved).map_err(unsafe_upstream)?;
            let mut request = client
                .new_property(space_id.as_str(), input.name.as_str(), input.format.into())
                .tags(input.tags.iter().map(PropertyTagSpec::to_api))
                .no_verify()
                .no_cache_refresh();
            if let Some(key) = input.key.as_ref() {
                request = request.key(key.as_str());
            }

            if let Some(hook) = hooks.before_post {
                hook(cancellation);
            }
            if cancellation.is_cancelled() {
                return Err(HandlerError::new(ToolError::upstream()).into());
            }
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
            if let Some(observer) = hooks.observer.as_ref() {
                observer(&created).map_err(|()| indeterminate_operation())?;
            }
            let property_id =
                EntityId::new(created.id.clone()).map_err(|_| indeterminate_operation())?;
            let response_matches = property_matches_create(&created, &property_id, &input);
            let verified = verify_semantic(
                &verify_config,
                "property",
                property_id.as_str(),
                || {
                    client
                        .property(space_id.as_str(), property_id.as_str())
                        .get_direct()
                },
                |property| property_matches_create(property, &property_id, &input),
            )
            .await
            .map_err(|_| indeterminate_operation())?;
            if !response_matches {
                return Err(indeterminate_operation());
            }
            let property = checked_property_summary(&verified, Some(&property_id))
                .map_err(|_| indeterminate_operation())?;
            let page = client
                .tags(space_id.as_str(), property_id.as_str())
                .limit(MAX_TAGS as u32)
                .offset(0)
                .list()
                .await
                .map_err(|_| indeterminate_operation())?;
            let tags = checked_tag_page(&page, &input.tags)?;
            Ok(PropertyCreateOutput { property, tags })
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

impl PropertyTagSpec {
    fn to_api(&self) -> CreateTagRequest {
        CreateTagRequest {
            name: self.name.as_str().to_owned(),
            key: self.key.as_ref().map(|key| key.as_str().to_owned()),
            color: self.color.into(),
        }
    }
}

async fn resolve_property(
    client: &AnytypeClient,
    space: &DiscoveryReference,
    property: &PropertyReference,
) -> Result<(EntityId, EntityId), HandlerOperationError> {
    let resolved_space = client.resolve_space_id(space.as_str()).await?;
    let space_id = EntityId::new(resolved_space).map_err(unsafe_upstream)?;
    let resolved_property = client
        .resolve_property_id(space_id.as_str(), property.as_str())
        .await?;
    let property_id = EntityId::new(resolved_property).map_err(unsafe_upstream)?;
    Ok((space_id, property_id))
}

fn checked_property_summary(
    property: &Property,
    expected_id: Option<&EntityId>,
) -> Result<PropertySummary, HandlerError> {
    let id = EntityId::new(property.id.clone()).map_err(unsafe_domain)?;
    if expected_id.is_some_and(|expected| expected != &id) {
        return Err(HandlerError::new(ToolError::upstream()));
    }
    Ok(PropertySummary {
        id,
        name: DisplayName::new(property.name.clone()).map_err(unsafe_domain)?,
        key: TypeKey::new(property.key.clone()).map_err(unsafe_domain)?,
        format: property.format().into(),
    })
}

fn checked_tag_summary(tag: &Tag) -> Result<TagSummary, HandlerError> {
    Ok(TagSummary {
        id: EntityId::new(tag.id.clone()).map_err(unsafe_domain)?,
        name: DisplayName::new(tag.name.clone()).map_err(unsafe_domain)?,
        key: TypeKey::new(tag.key.clone()).map_err(unsafe_domain)?,
        color: tag.color.clone().into(),
    })
}

fn checked_tag_page(
    page: &anytype::paged::PagedResult<Tag>,
    expected: &[PropertyTagSpec],
) -> Result<Vec<TagSummary>, HandlerOperationError> {
    checked_tag_evidence(
        page.pagination.offset,
        page.pagination.limit,
        page.pagination.has_more,
        page.pagination.total,
        &page.items,
        expected,
    )
    .map_err(|error| match error {
        TagPageError::Bounded => HandlerError::new(ToolError::bounded_result()).into(),
        TagPageError::Indeterminate => indeterminate_operation(),
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TagPageError {
    Bounded,
    Indeterminate,
}

fn checked_tag_evidence(
    offset: u32,
    limit: u32,
    has_more: bool,
    total: usize,
    items: &[Tag],
    expected: &[PropertyTagSpec],
) -> Result<Vec<TagSummary>, TagPageError> {
    if offset != 0 || limit != MAX_TAGS as u32 {
        return Err(TagPageError::Indeterminate);
    }
    if has_more
        || total > MAX_TAGS
        || items.len() > MAX_TAGS
        || total > expected.len()
        || items.len() > expected.len()
    {
        return Err(TagPageError::Bounded);
    }
    if total != items.len() || items.len() != expected.len() {
        return Err(TagPageError::Indeterminate);
    }
    let actual_by_key = items
        .iter()
        .enumerate()
        .map(|(index, tag)| (tag.key.as_str(), (index, tag)))
        .collect::<HashMap<_, _>>();
    let mut consumed = HashSet::with_capacity(items.len());
    let mut checked = Vec::with_capacity(expected.len());
    for requested in expected {
        let Some(requested_key) = requested.key.as_ref() else {
            let matches = items.iter().enumerate().find(|(index, tag)| {
                !consumed.contains(index)
                    && tag.name == requested.name.as_str()
                    && tag.color == requested.color.into()
            });
            let Some((index, actual)) = matches else {
                return Err(TagPageError::Indeterminate);
            };
            consumed.insert(index);
            checked.push(checked_tag_summary(actual).map_err(|_| TagPageError::Indeterminate)?);
            continue;
        };
        let Some((index, actual)) = actual_by_key.get(requested_key.as_str()).copied() else {
            return Err(TagPageError::Indeterminate);
        };
        if consumed.contains(&index) {
            return Err(TagPageError::Indeterminate);
        }
        if actual.name != requested.name.as_str() || actual.color != requested.color.into() {
            return Err(TagPageError::Indeterminate);
        }
        consumed.insert(index);
        checked.push(checked_tag_summary(actual).map_err(|_| TagPageError::Indeterminate)?);
    }
    if consumed.len() != items.len() {
        return Err(TagPageError::Indeterminate);
    }
    let unique_ids = checked
        .iter()
        .map(|tag| tag.id.as_str())
        .collect::<HashSet<_>>();
    if unique_ids.len() != checked.len() {
        return Err(TagPageError::Indeterminate);
    }
    Ok(checked)
}

fn property_matches_create(
    property: &Property,
    expected_id: &EntityId,
    input: &NormalizedPropertyCreate,
) -> bool {
    let Ok(summary) = checked_property_summary(property, Some(expected_id)) else {
        return false;
    };
    summary.name.as_str() == input.name.as_str()
        && summary.format == input.format
        && input
            .key
            .as_ref()
            .is_none_or(|expected| summary.key.as_str() == expected.as_str())
}

fn property_matches_update(
    property: &Property,
    expected_id: &EntityId,
    input: &PropertyUpdateInput,
    original_format: anytype::properties::PropertyFormat,
) -> bool {
    let Ok(summary) = checked_property_summary(property, Some(expected_id)) else {
        return false;
    };
    summary.name.as_str() == input.name.as_str()
        && property.format() == original_format
        && input
            .key
            .as_ref()
            .is_none_or(|expected| summary.key.as_str() == expected.as_str())
}

fn hash_optional(hasher: &mut Sha256, value: Option<&str>) {
    match value {
        Some(value) => {
            hasher.update([1]);
            hash_field(hasher, value);
        }
        None => hasher.update([0]),
    }
}

fn hash_field(hasher: &mut Sha256, value: &str) {
    hasher.update(value.len().to_be_bytes());
    hasher.update(value.as_bytes());
}

const fn property_format_name(format: PropertyFormat) -> &'static str {
    match format {
        PropertyFormat::Text => "text",
        PropertyFormat::Number => "number",
        PropertyFormat::Select => "select",
        PropertyFormat::MultiSelect => "multi_select",
        PropertyFormat::Date => "date",
        PropertyFormat::Files => "files",
        PropertyFormat::Checkbox => "checkbox",
        PropertyFormat::Url => "url",
        PropertyFormat::Email => "email",
        PropertyFormat::Phone => "phone",
        PropertyFormat::Objects => "objects",
    }
}

const fn schema_color_name(color: SchemaColor) -> &'static str {
    match color {
        SchemaColor::Grey => "grey",
        SchemaColor::Yellow => "yellow",
        SchemaColor::Orange => "orange",
        SchemaColor::Red => "red",
        SchemaColor::Pink => "pink",
        SchemaColor::Purple => "purple",
        SchemaColor::Blue => "blue",
        SchemaColor::Ice => "ice",
        SchemaColor::Teal => "teal",
        SchemaColor::Lime => "lime",
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
    use std::{
        collections::BTreeMap,
        sync::{Arc, atomic::AtomicUsize},
        time::Duration,
    };

    use anytype::{
        prelude::{AnytypeClient, ClientConfig, HttpCredentials},
        test_util::{DisposableRun, unique_suffix, with_disposable_space_context},
    };
    use serde_json::{Map, Value, json};
    use tiktoken_rs::{CoreBPE, o200k_base};
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
    const TOKEN_BUDGET_SNAPSHOT: &str =
        include_str!("../tests/snapshots/schema-property-token-budget.json");

    struct TestRegistry {
        handlers: SchemaPropertyHandlers,
    }

    impl fmt::Debug for TestRegistry {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("TestSchemaPropertyRegistry")
        }
    }

    impl OptionalToolsetRegistry for TestRegistry {
        fn metadata(&self) -> OptionalToolsetMetadata {
            OptionalToolsetMetadata::new("schema", true)
        }

        fn tools(&self) -> Result<Vec<OptionalRegistryTool>, SchemaContractError> {
            schema_property_tools()
        }

        fn scripted_scenario_ids(&self) -> &'static [&'static str] {
            &["schema_property_direct", "schema_property_stdio"]
        }

        fn headless_scenario_ids(&self) -> &'static [&'static str] {
            &["schema_property_headless"]
        }

        fn catalog_token_ceiling(&self) -> usize {
            8_000
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

    fn runtime(client: AnytypeClient, read_only: bool) -> RuntimeContext {
        let selection = OptionalToolsetSelection::parse(
            Some("schema".to_owned()),
            &[OptionalToolsetMetadata::new("schema", true)],
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
            keystore_service: Some("schema-property-no-io".to_owned()),
            app_name: "schema-property-no-io".to_owned(),
            ..ClientConfig::default()
        })
        .expect("schema-property no-I/O client");
        client.set_api_key(HttpCredentials::new("unused-no-io-token"));
        runtime(client, read_only)
    }

    fn server(
        client: AnytypeClient,
        read_only: bool,
        handlers: SchemaPropertyHandlers,
    ) -> AnyMcpServer {
        let registry: &'static TestRegistry = Box::leak(Box::new(TestRegistry { handlers }));
        let registries: &'static [&'static dyn OptionalToolsetRegistry] =
            Box::leak(vec![registry as &dyn OptionalToolsetRegistry].into_boxed_slice());
        AnyMcpServer::new_with_optional_registries(runtime(client, read_only), registries)
            .expect("schema-property test server")
    }

    fn snapshot_client() -> AnytypeClient {
        let client = AnytypeClient::with_config(ClientConfig {
            base_url: Some("http://127.0.0.1:1".to_owned()),
            keystore: Some("env".to_owned()),
            keystore_service: Some("schema-property-snapshot".to_owned()),
            app_name: "schema-property-snapshot".to_owned(),
            disable_cache: true,
            ..ClientConfig::default()
        })
        .expect("schema-property snapshot client");
        client.set_api_key(HttpCredentials::new("snapshot-token"));
        client
    }

    fn snapshot_server(
        profile: ApplicationProfile,
        read_only: bool,
        selected: Option<&str>,
    ) -> AnyMcpServer {
        let registry: &'static TestRegistry = Box::leak(Box::new(TestRegistry {
            handlers: SchemaPropertyHandlers::new().expect("snapshot handlers"),
        }));
        let registries: &'static [&'static dyn OptionalToolsetRegistry] = Box::leak(
            vec![
                crate::member_toolset::MEMBERS_REGISTRY,
                registry as &dyn OptionalToolsetRegistry,
            ]
            .into_boxed_slice(),
        );
        let metadata = registries
            .iter()
            .map(|candidate| candidate.metadata())
            .collect::<Vec<_>>();
        let selection = OptionalToolsetSelection::parse(selected.map(str::to_owned), &metadata)
            .expect("snapshot optional selection");
        let runtime = RuntimeContext::from_parts_with_profile_and_optional_toolsets(
            snapshot_client(),
            4,
            Duration::from_secs(30),
            StartupStatus {
                http_available: true,
                grpc_available: true,
            },
            profile,
            read_only,
            selection,
        );
        AnyMcpServer::new_with_optional_registries(runtime, registries)
            .expect("schema-property snapshot server")
    }

    fn args(value: Value) -> Map<String, Value> {
        value.as_object().cloned().expect("object arguments")
    }

    async fn direct(router: &AnyMcpServer, name: &'static str, arguments: Value) -> CallToolResult {
        router
            .dispatch_tool(
                CallToolRequestParams::new(name).with_arguments(args(arguments)),
                &CancellationToken::new(),
            )
            .await
            .expect("schema-property direct dispatch")
    }

    async fn preview_stdio_exchange(
        server: AnyMcpServer,
        name: &'static str,
        arguments: Value,
    ) -> Option<Value> {
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
                    "io.modelcontextprotocol/clientInfo":{
                        "name":"schema-property-test","version":"1"
                    },
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
        if line.is_empty() {
            None
        } else {
            Some(serde_json::from_str(&line).expect("decode stdio response"))
        }
    }

    async fn preview_stdio_call(
        server: AnyMcpServer,
        name: &'static str,
        arguments: Value,
    ) -> Value {
        preview_stdio_exchange(server, name, arguments)
            .await
            .expect("stdio response")
    }

    async fn preview_stdio_tools(server: AnyMcpServer) -> Value {
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
            "id":8,
            "method":"tools/list",
            "params":{
                "_meta":{
                    "io.modelcontextprotocol/protocolVersion":"2026-07-28",
                    "io.modelcontextprotocol/clientInfo":{
                        "name":"schema-property-schema-test","version":"1"
                    },
                    "io.modelcontextprotocol/clientCapabilities":{}
                }
            }
        });
        client_writer
            .write_all(format!("{frame}\n").as_bytes())
            .await
            .expect("write stdio tools request");
        let mut client_reader = BufReader::new(client_reader);
        let mut line = String::new();
        client_reader
            .read_line(&mut line)
            .await
            .expect("read stdio tools response");
        drop(client_writer);
        drop(client_reader);
        task.await
            .expect("spawned stdio tools task")
            .expect("stdio tools transport");
        serde_json::from_str(&line).expect("decode stdio tools response")
    }

    #[derive(Clone, Copy, Debug)]
    enum Transport {
        Direct,
        Stdio,
    }

    async fn transport_call(
        router: &AnyMcpServer,
        client: &AnytypeClient,
        handlers: &SchemaPropertyHandlers,
        transport: Transport,
        name: &'static str,
        arguments: Value,
    ) -> CallToolResult {
        match transport {
            Transport::Direct => direct(router, name, arguments).await,
            Transport::Stdio => {
                let response = preview_stdio_call(
                    server(client.clone(), false, handlers.clone()),
                    name,
                    arguments,
                )
                .await;
                assert!(response.get("error").is_none(), "stdio failure: {response}");
                serde_json::from_value(response["result"].clone())
                    .expect("decode stdio call result")
            }
        }
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

    fn assert_success(result: &CallToolResult) -> &Value {
        assert_eq!(
            result.is_error,
            Some(false),
            "unexpected result: {result:?}"
        );
        let value = result.structured_content.as_ref().expect("typed success");
        let expected_text = serde_json::to_string(value).expect("compact success");
        assert_eq!(
            result
                .content
                .first()
                .and_then(|content| content.as_text())
                .map(|text| text.text.as_str()),
            Some(expected_text.as_str())
        );
        value
    }

    fn result_code(result: &CallToolResult) -> Option<&str> {
        result
            .structured_content
            .as_ref()
            .and_then(|value| value.get("code"))
            .and_then(Value::as_str)
    }

    #[test]
    fn exact_contracts_reject_null_unknown_duplicate_and_twenty_first_tags() {
        let create = property_create_tool().expect("property create contract");
        let update = property_update_tool().expect("property update contract");
        for tool in [create.as_tool(), update.as_tool()] {
            let input = serde_json::to_value(tool.input_schema.as_ref())
                .expect("input schema")
                .to_string();
            let output = serde_json::to_value(tool.output_schema.as_ref())
                .expect("output schema")
                .to_string();
            assert!(input.contains("additionalProperties\":false"));
            assert!(output.contains("additionalProperties\":false"));
        }
        assert_eq!(
            create
                .as_tool()
                .annotations
                .as_ref()
                .and_then(|annotations| annotations.idempotent_hint),
            Some(false)
        );
        assert_eq!(
            update
                .as_tool()
                .annotations
                .as_ref()
                .and_then(|annotations| annotations.destructive_hint),
            Some(true)
        );
        for invalid in [
            json!({"space":SPACE_ID,"name":"P","format":"select","tags":null}),
            json!({"space":SPACE_ID,"name":"P","format":"text","unknown":1}),
            json!({
                "space":SPACE_ID,"name":"P","format":"select",
                "tags":[
                    {"name":"A","key":"same","color":"grey"},
                    {"name":"B","key":"same","color":"blue"}
                ]
            }),
        ] {
            assert!(serde_json::from_value::<PropertyCreateInput>(invalid).is_err());
        }
        let twenty_one = (0..21)
            .map(|index| json!({"name":format!("T{index}"),"color":"grey"}))
            .collect::<Vec<_>>();
        assert!(
            serde_json::from_value::<PropertyCreateInput>(json!({
                "space":SPACE_ID,"name":"P","format":"select","tags":twenty_one
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<PropertyUpdateInput>(json!({
                "space":SPACE_ID,"property":"p","key":"new_key"
            }))
            .is_err()
        );
    }

    #[tokio::test]
    async fn tag_format_and_read_only_rejections_do_no_io() {
        let handlers = SchemaPropertyHandlers::new().expect("property handlers");
        let runtime = no_io_runtime(false);
        let before = metric_counts(runtime.client());
        let result = handlers
            .call_tool(
                CallToolRequestParams::new(PROPERTY_CREATE).with_arguments(args(json!({
                    "space":SPACE_ID,
                    "name":"SECRET_INVALID_PROPERTY_NAME",
                    "format":"text",
                    "tags":[{"name":"SECRET_INVALID_TAG_NAME","color":"grey"}]
                }))),
                &runtime,
                &CancellationToken::new(),
            )
            .await
            .expect("handler result");
        assert_eq!(result_code(&result), Some("validation"));
        let encoded = serde_json::to_string(&result).expect("redacted validation result");
        assert!(!encoded.contains("SECRET"));
        assert_eq!(metric_counts(runtime.client()), before);

        let read_only = no_io_runtime(true);
        let before = metric_counts(read_only.client());
        let result = handlers
            .call_tool(
                CallToolRequestParams::new(PROPERTY_UPDATE).with_arguments(args(json!({
                    "space":SPACE_ID,"property":"safe_key","name":"Name"
                }))),
                &read_only,
                &CancellationToken::new(),
            )
            .await
            .expect("read-only handler result");
        assert_eq!(result_code(&result), Some("validation"));
        assert_eq!(metric_counts(read_only.client()), before);
    }

    #[test]
    fn ambiguity_mapping_retains_only_valid_fixed_wire_evidence() {
        let source = anytype::error::AnytypeError::Ambiguous {
            obj_type: "property".to_owned(),
            key: "SECRET_AMBIGUOUS_QUERY".to_owned(),
            candidates: vec![
                anytype::resolve::ResolveCandidate::new("candidate-a", "First"),
                anytype::resolve::ResolveCandidate::new("candidate-b", "Second"),
            ],
        };
        let crate::error::AnytypeErrorMapping::Ready(error) = ToolError::from_anytype(&source)
        else {
            panic!("valid ambiguity candidates must map");
        };
        let encoded = serde_json::to_string(&error).expect("ambiguity wire error");
        assert!(!encoded.contains("SECRET"));
        assert_eq!(
            serde_json::to_value(error).expect("ambiguity value"),
            json!({
                "code":"ambiguous",
                "message":"The reference is ambiguous. Retry with one of the candidate identifiers.",
                "candidates":[
                    {"id":"candidate-a","name":"First"},
                    {"id":"candidate-b","name":"Second"}
                ]
            })
        );
    }

    #[test]
    fn formats_colors_fingerprints_and_result_shape_are_closed() {
        for format in [
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
        ] {
            assert_eq!(
                PropertyFormat::from(anytype::properties::PropertyFormat::from(format)),
                format
            );
        }
        for color in [
            SchemaColor::Grey,
            SchemaColor::Yellow,
            SchemaColor::Orange,
            SchemaColor::Red,
            SchemaColor::Pink,
            SchemaColor::Purple,
            SchemaColor::Blue,
            SchemaColor::Ice,
            SchemaColor::Teal,
            SchemaColor::Lime,
        ] {
            assert_eq!(SchemaColor::from(Color::from(color)), color);
        }
        let input = serde_json::from_value::<PropertyCreateInput>(json!({
            "space":SPACE_ID,"name":"Priority","format":"select","key":"priority",
            "tags":[{"name":"High","key":"high","color":"red"}],
            "idempotency_key":"same"
        }))
        .expect("create input");
        let first = NormalizedPropertyCreate::from(input).fingerprint();
        let changed = serde_json::from_value::<PropertyCreateInput>(json!({
            "space":SPACE_ID,"name":"Priority","format":"select","key":"priority",
            "tags":[{"name":"High","key":"high","color":"blue"}],
            "idempotency_key":"same"
        }))
        .expect("changed input");
        assert_ne!(first, NormalizedPropertyCreate::from(changed).fingerprint());

        let output = PropertyCreateOutput {
            property: PropertySummary {
                id: EntityId::new("p").expect("id"),
                name: DisplayName::new("Priority").expect("name"),
                key: TypeKey::new("priority").expect("key"),
                format: PropertyFormat::Select,
            },
            tags: vec![TagSummary {
                id: EntityId::new("t").expect("tag id"),
                name: DisplayName::new("High").expect("tag name"),
                key: TypeKey::new("high").expect("tag key"),
                color: SchemaColor::Red,
            }],
        };
        let result = property_create_tool()
            .expect("contract")
            .success(&output)
            .expect("success");
        assert_eq!(
            assert_success(&result),
            &json!({
                "property":{"id":"p","name":"Priority","key":"priority","format":"select"},
                "tags":[{"id":"t","name":"High","key":"high","color":"red"}]
            })
        );
    }

    fn tag_fixture(id: &str, name: &str, key: &str, color: &str) -> Tag {
        serde_json::from_value(json!({"id":id,"name":name,"key":key,"color":color}))
            .expect("tag fixture")
    }

    fn tag_spec(name: &str, key: Option<&str>, color: &str) -> PropertyTagSpec {
        let mut value = json!({"name":name,"color":color});
        if let Some(key) = key {
            value["key"] = json!(key);
        }
        serde_json::from_value(value).expect("tag specification")
    }

    #[test]
    fn tag_page_evidence_fails_closed_at_every_adversarial_boundary() {
        assert_eq!(
            checked_tag_evidence(0, 20, false, 0, &[], &[]),
            Ok(Vec::new())
        );
        let expected = vec![
            tag_spec("Red", Some("red"), "red"),
            tag_spec("Blue", None, "blue"),
        ];
        let valid = vec![
            tag_fixture("tag-blue", "Blue", "generated_blue", "blue"),
            tag_fixture("tag-red", "Red", "red", "red"),
        ];
        let checked = checked_tag_evidence(0, 20, false, 2, &valid, &expected)
            .expect("exact terminal tag page");
        assert_eq!(
            checked
                .iter()
                .map(|tag| tag.id.as_str())
                .collect::<Vec<_>>(),
            vec!["tag-red", "tag-blue"]
        );

        let extra = vec![tag_fixture("extra", "Extra", "extra", "grey")];
        assert_eq!(
            checked_tag_evidence(0, 20, false, 1, &extra, &[]),
            Err(TagPageError::Bounded)
        );
        assert_eq!(
            checked_tag_evidence(0, 20, true, 2, &valid, &expected),
            Err(TagPageError::Bounded)
        );
        assert_eq!(
            checked_tag_evidence(1, 20, false, 2, &valid, &expected),
            Err(TagPageError::Indeterminate)
        );
        assert_eq!(
            checked_tag_evidence(0, 19, false, 2, &valid, &expected),
            Err(TagPageError::Indeterminate)
        );
        assert_eq!(
            checked_tag_evidence(0, 20, false, 1, &valid, &expected),
            Err(TagPageError::Indeterminate)
        );
        assert_eq!(
            checked_tag_evidence(0, 20, false, 1, &valid[..1], &expected),
            Err(TagPageError::Indeterminate)
        );
        let wrong = vec![
            tag_fixture("tag-blue", "Blue", "generated_blue", "red"),
            tag_fixture("tag-red", "Wrong", "red", "red"),
        ];
        assert_eq!(
            checked_tag_evidence(0, 20, false, 2, &wrong, &expected),
            Err(TagPageError::Indeterminate)
        );
        let duplicate_ids = vec![
            tag_fixture("same", "Blue", "generated_blue", "blue"),
            tag_fixture("same", "Red", "red", "red"),
        ];
        assert_eq!(
            checked_tag_evidence(0, 20, false, 2, &duplicate_ids, &expected),
            Err(TagPageError::Indeterminate)
        );
        let twenty_one = (0..21)
            .map(|index| tag_fixture(&format!("t{index}"), "T", &format!("t{index}"), "grey"))
            .collect::<Vec<_>>();
        assert_eq!(
            checked_tag_evidence(0, 20, false, 21, &twenty_one, &[]),
            Err(TagPageError::Bounded)
        );
    }

    #[test]
    fn registry_slice_and_work_ceilings_are_locked() {
        assert_eq!(schema_property_tools().expect("property slice").len(), 2);
        assert_eq!(
            (
                PROPERTY_CREATE_LOGICAL_CEILING,
                PROPERTY_CREATE_PHYSICAL_CEILING
            ),
            (23, 133)
        );
        assert_eq!(
            (
                PROPERTY_UPDATE_LOGICAL_CEILING,
                PROPERTY_UPDATE_PHYSICAL_CEILING
            ),
            (34, 199)
        );
        let mut names = vec![
            property_create_tool()
                .expect("create contract")
                .as_tool()
                .name
                .to_string(),
            property_update_tool()
                .expect("update contract")
                .as_tool()
                .name
                .to_string(),
        ];
        names.sort();
        assert_eq!(names, vec![PROPERTY_CREATE, PROPERTY_UPDATE]);
        let production_names = crate::optional_toolsets::production_optional_registries()
            .iter()
            .map(|registry| registry.metadata().name.to_owned())
            .collect::<Vec<_>>();
        assert_eq!(
            production_names
                .iter()
                .filter(|name| name.as_str() == "schema")
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn direct_and_stdio_catalog_schemas_are_byte_identical() {
        let handlers = SchemaPropertyHandlers::new().expect("schema parity handlers");
        let direct_server = server(
            no_io_runtime(false).client().raw_clone(),
            false,
            handlers.clone(),
        );
        let expected = serde_json::to_value(crate::server::stable_list_tools_result(
            direct_server.tools().to_vec(),
        ))
        .expect("direct tools value");
        let stdio = preview_stdio_tools(server(
            no_io_runtime(false).client().raw_clone(),
            false,
            handlers,
        ))
        .await;
        assert_eq!(stdio["result"]["tools"], expected["tools"]);
        assert_eq!(stdio["result"]["resultType"], "complete");
        assert_eq!(stdio["result"]["cacheScope"], "public");
        let tool_names = expected["tools"]
            .as_array()
            .expect("tool array")
            .iter()
            .filter_map(|tool| tool["name"].as_str())
            .collect::<HashSet<_>>();
        assert!(tool_names.contains(PROPERTY_CREATE));
        assert!(tool_names.contains(PROPERTY_UPDATE));
    }

    #[test]
    fn observer_seam_is_thread_safe() {
        let observed = Arc::new(AtomicUsize::new(0));
        let counter = observed.clone();
        let handlers = SchemaPropertyHandlers::build(
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

    fn canonical_json(value: Value) -> Value {
        match value {
            Value::Object(values) => Value::Object(
                values
                    .into_iter()
                    .map(|(key, value)| (key, canonical_json(value)))
                    .collect::<BTreeMap<_, _>>()
                    .into_iter()
                    .collect(),
            ),
            Value::Array(values) => Value::Array(values.into_iter().map(canonical_json).collect()),
            scalar => scalar,
        }
    }

    fn canonical_compact(value: Value) -> String {
        serde_json::to_string(&canonical_json(value)).expect("canonical JSON")
    }

    fn token_count(tokenizer: &CoreBPE, value: Value) -> usize {
        tokenizer
            .encode_with_special_tokens(&canonical_compact(value))
            .len()
    }

    fn tools_list_value(server: &AnyMcpServer) -> Value {
        serde_json::to_value(crate::server::stable_list_tools_result(
            server.tools().to_vec(),
        ))
        .expect("tools list value")
    }

    fn adversarial_text(seed: usize, length: usize) -> String {
        const ALPHABET: &[char] = &[
            '\0', '\u{001f}', '"', '\\', '\n', '\r', '\t', '界', '🚀', '𐍈', 'Ω', 'א', 'ق', 'क',
            'あ', '가', '\u{2028}', '\u{2029}',
        ];
        (0..length)
            .map(|position| ALPHABET[(seed + position) % ALPHABET.len()])
            .collect()
    }

    fn dense_safe_id(prefix: &str, seed: usize) -> String {
        const ALPHABET: &[u8] =
            b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ_abcdefghijklmnopqrstuvwxyz~-";
        prefix
            .chars()
            .chain((prefix.chars().count()..256).map(|position| {
                char::from(ALPHABET[(seed.saturating_mul(17) + position) % ALPHABET.len()])
            }))
            .collect()
    }

    fn dense_schema_key(seed: usize) -> String {
        const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789_";
        let prefix = format!("k{seed}_");
        prefix
            .chars()
            .chain((prefix.len()..256).map(|position| {
                char::from(ALPHABET[(seed.saturating_mul(13) + position) % ALPHABET.len()])
            }))
            .collect()
    }

    fn maximum_property_result() -> CallToolResult {
        let tags = (0..20)
            .map(|index| TagSummary {
                id: EntityId::new(dense_safe_id(&format!("t{index:02}"), index))
                    .expect("maximum tag id"),
                name: DisplayName::new(adversarial_text(index, 512)).expect("maximum tag name"),
                key: TypeKey::new(adversarial_text(index + 31, 256)).expect("maximum tag key"),
                color: SchemaColor::Lime,
            })
            .collect();
        let output = PropertyCreateOutput {
            property: PropertySummary {
                id: EntityId::new(dense_safe_id("property", 91)).expect("maximum property id"),
                name: DisplayName::new(adversarial_text(47, 512)).expect("maximum property name"),
                key: TypeKey::new(adversarial_text(83, 256)).expect("maximum property key"),
                format: PropertyFormat::MultiSelect,
            },
            tags,
        };
        property_create_tool()
            .expect("property create contract")
            .success(&output)
            .expect("maximum property result")
    }

    fn maximum_property_input() -> Value {
        let colors = [
            "grey", "yellow", "orange", "red", "pink", "purple", "blue", "ice", "teal", "lime",
        ];
        let tags = (0..20)
            .map(|index| {
                json!({
                    "name":adversarial_text(index, 512),
                    "key":dense_schema_key(index),
                    "color":colors[index % colors.len()]
                })
            })
            .collect::<Vec<_>>();
        json!({
            "space":adversarial_text(101, 512),
            "name":adversarial_text(131, 512),
            "format":"multi_select",
            "key":dense_schema_key(97),
            "tags":tags,
            "idempotency_key":adversarial_text(151, 256)
        })
    }

    fn schema_property_token_budget() -> Value {
        let tokenizer = o200k_base().expect("o200k tokenizer");
        let base = snapshot_server(ApplicationProfile::Compact, false, None);
        let compact = snapshot_server(ApplicationProfile::Compact, false, Some("schema"));
        let compact_read_only = snapshot_server(ApplicationProfile::Compact, true, Some("schema"));
        let standard = snapshot_server(ApplicationProfile::Standard, false, Some("schema"));
        let standard_read_only =
            snapshot_server(ApplicationProfile::Standard, true, Some("schema"));
        let with_members =
            snapshot_server(ApplicationProfile::Compact, false, Some("members,schema"));
        let base_value = tools_list_value(&base);
        let base_json = canonical_compact(base_value.clone());
        let base_hash = Sha256::digest(base_json.as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let per_tool = compact
            .tools()
            .iter()
            .filter(|tool| matches!(tool.name.as_ref(), PROPERTY_CREATE | PROPERTY_UPDATE))
            .map(|tool| {
                (
                    tool.name.to_string(),
                    token_count(&tokenizer, serde_json::to_value(tool).expect("tool value")),
                )
            })
            .collect::<BTreeMap<_, _>>();
        let maximum_input = maximum_property_input();
        let maximum_result =
            serde_json::to_value(maximum_property_result()).expect("maximum property result value");
        let maximum_result_json = canonical_compact(maximum_result.clone());
        let maximum_result_hash = Sha256::digest(maximum_result_json.as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        json!({
            "tokenizer":"tiktoken o200k_base (tiktoken-rs 0.12.0)",
            "base_catalog_sha256":base_hash,
            "base_catalog_tokens":token_count(&tokenizer, base_value),
            "selected":["schema"],
            "schema_domain_ceiling_tokens":9500,
            "schema_selected_ceiling_tokens":10000,
            "per_tool_tokens":per_tool,
            "compact_composed_total_tokens":token_count(&tokenizer, tools_list_value(&compact)),
            "compact_read_only_total_tokens":token_count(
                &tokenizer,
                tools_list_value(&compact_read_only)
            ),
            "standard_composed_total_tokens":token_count(&tokenizer, tools_list_value(&standard)),
            "standard_read_only_total_tokens":token_count(
                &tokenizer,
                tools_list_value(&standard_read_only)
            ),
            "members_schema_compact_total_tokens":token_count(
                &tokenizer,
                tools_list_value(&with_members)
            ),
            "adversarial_twenty_tag_input_tokens":token_count(&tokenizer, maximum_input),
            "representative_max_result_bytes":maximum_result_json.len(),
            "representative_max_result_tokens":token_count(&tokenizer, maximum_result),
            "representative_max_result_sha256":maximum_result_hash
        })
    }

    #[test]
    fn maximum_result_serialization_is_finite_and_deterministic() {
        let tags = (0..20)
            .map(|index| TagSummary {
                id: EntityId::new(format!("t{index}{}", "a".repeat(253))).expect("maximum tag id"),
                name: DisplayName::new("界".repeat(512)).expect("maximum tag name"),
                key: TypeKey::new("k".repeat(256)).expect("maximum tag key"),
                color: SchemaColor::Lime,
            })
            .collect();
        let output = PropertyCreateOutput {
            property: PropertySummary {
                id: EntityId::new("p".repeat(256)).expect("maximum property id"),
                name: DisplayName::new("語".repeat(512)).expect("maximum property name"),
                key: TypeKey::new("k".repeat(256)).expect("maximum property key"),
                format: PropertyFormat::MultiSelect,
            },
            tags,
        };
        let result = property_create_tool()
            .expect("create contract")
            .success(&output)
            .expect("maximum result");
        let canonical = serde_json::to_vec(&canonical_json(
            serde_json::to_value(&result).expect("maximum result value"),
        ))
        .expect("maximum result bytes");
        let repeated = serde_json::to_vec(&canonical_json(
            serde_json::to_value(&result).expect("repeated maximum result value"),
        ))
        .expect("repeated maximum result bytes");
        assert!(canonical.len() < 100_000);
        assert_eq!(canonical, repeated);
        let digest: [u8; 32] = Sha256::digest(&canonical).into();
        assert_ne!(digest, [0_u8; 32]);
    }

    #[test]
    fn schema_property_catalog_input_and_result_match_reviewed_token_snapshot() {
        let actual = canonical_json(schema_property_token_budget());
        let reviewed = canonical_json(
            serde_json::from_str(TOKEN_BUDGET_SNAPSHOT).expect("schema-property token snapshot"),
        );
        assert_eq!(actual, reviewed, "schema-property token budget drifted");
        assert_eq!(actual["selected"], json!(["schema"]));
        let domain_tokens = actual["per_tool_tokens"]
            .as_object()
            .expect("per-tool token object")
            .values()
            .map(|value| value.as_u64().expect("token count") as usize)
            .sum::<usize>();
        assert!(domain_tokens <= 9_500);
        let selected_added = actual["compact_composed_total_tokens"]
            .as_u64()
            .expect("composed tokens")
            .saturating_sub(actual["base_catalog_tokens"].as_u64().expect("base tokens"));
        assert!(selected_added <= 10_000);
        assert_eq!(
            actual["adversarial_twenty_tag_input_tokens"],
            reviewed["adversarial_twenty_tag_input_tokens"]
        );
        assert_eq!(
            actual["representative_max_result_bytes"],
            reviewed["representative_max_result_bytes"]
        );
        assert_eq!(
            actual["representative_max_result_tokens"],
            reviewed["representative_max_result_tokens"]
        );
        assert_eq!(
            actual["representative_max_result_sha256"],
            reviewed["representative_max_result_sha256"]
        );
    }

    #[test]
    #[ignore = "requires env-only disposable credentials and an authenticated headless Anytype server"]
    fn headless_property_create_update_direct_stdio_and_cache_bounds() {
        std::thread::Builder::new()
            .name("schema-property-live".to_owned())
            .stack_size(16 * 1024 * 1024)
            .spawn(|| {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("schema-property live runtime");
                runtime.block_on(async {
                    let outcome = Box::pin(with_disposable_space_context(
                        "any-mcp-schema-property",
                        |ctx| {
                            Box::pin(async move {
                                ctx.client.ping_http().await.expect("authenticated HTTP");
                                let suffix = unique_suffix().replace('-', "_");
                                let observer_ctx = ctx.clone();
                                let handlers = SchemaPropertyHandlers::build(
                                    DEFAULT_IDEMPOTENCY_CAPACITY,
                                    VerifyConfig::default(),
                                    Some(Arc::new(move |property| {
                                        observer_ctx.register_property(&property.id);
                                        Ok(())
                                    })),
                                )
                                .expect("live property handlers");
                                let router =
                                    server(ctx.client.clone(), false, handlers.clone());
                                let mut created_properties = Vec::new();

                                for (index, transport) in
                                    [Transport::Direct, Transport::Stdio].into_iter().enumerate()
                                {
                                    ctx.client.cache().clear_properties(Some(&ctx.space_id));
                                    if index == 1 {
                                        let _primed = ctx.client.properties(&ctx.space_id).list().await?;
                                    }
                                    let label = match transport {
                                        Transport::Direct => "direct",
                                        Transport::Stdio => "stdio",
                                    };
                                    let tag_count = if index == 0 { 20 } else { 2 };
                                    let tags = (0..tag_count)
                                        .map(|tag_index| {
                                            json!({
                                                "name":format!("{label} tag {tag_index}"),
                                                "key":format!("{label}_tag_{tag_index}_{suffix}"),
                                                "color":if tag_index % 2 == 0 { "red" } else { "blue" }
                                            })
                                        })
                                        .collect::<Vec<_>>();
                                    let property_key = format!("mcp_{label}_{suffix}");
                                    let create_input = json!({
                                        "space":ctx.space_id,
                                        "name":format!("MCP {label} property"),
                                        "format":if index == 0 { "multi_select" } else { "select" },
                                        "key":property_key,
                                        "tags":tags,
                                        "idempotency_key":format!("property-{label}-{suffix}")
                                    });
                                    let before = metric_counts(&ctx.client);
                                    let created = transport_call(
                                        &router,
                                        &ctx.client,
                                        &handlers,
                                        transport,
                                        PROPERTY_CREATE,
                                        create_input.clone(),
                                    )
                                    .await;
                                    let after = metric_counts(&ctx.client);
                                    assert_eq!(metric_delta(before, after), (3, 3));
                                    let value = assert_success(&created);
                                    assert_eq!(value["tags"].as_array().map(Vec::len), Some(tag_count));
                                    let property_id = value["property"]["id"]
                                        .as_str()
                                        .expect("created property id")
                                        .to_owned();
                                    created_properties.push((
                                        property_id.clone(),
                                        format!("MCP {label} property"),
                                    ));

                                    let before = metric_counts(&ctx.client);
                                    let replay = transport_call(
                                        &router,
                                        &ctx.client,
                                        &handlers,
                                        transport,
                                        PROPERTY_CREATE,
                                        create_input,
                                    )
                                    .await;
                                    assert_eq!(replay.structured_content, created.structured_content);
                                    assert_eq!(metric_delta(before, metric_counts(&ctx.client)), (0, 0));

                                    let original_tags = ctx
                                        .client
                                        .tags(&ctx.space_id, &property_id)
                                        .limit(20)
                                        .offset(0)
                                        .list()
                                        .await?;
                                    let original_tag_ids = original_tags
                                        .items
                                        .iter()
                                        .map(|tag| tag.id.clone())
                                        .collect::<Vec<_>>();

                                    let current_name = format!("MCP {label} property");
                                    let before = metric_counts(&ctx.client);
                                    let no_op = transport_call(
                                        &router,
                                        &ctx.client,
                                        &handlers,
                                        transport,
                                        PROPERTY_UPDATE,
                                        json!({
                                            "space":ctx.space_id,
                                            "property":property_id,
                                            "name":current_name,
                                            "key":property_key
                                        }),
                                    )
                                    .await;
                                    assert_success(&no_op);
                                    assert_eq!(metric_delta(before, metric_counts(&ctx.client)), (1, 1));

                                    let updated_name = format!("MCP {label} updated");
                                    let updated_key = format!("mcp_{label}_updated_{suffix}");
                                    let before = metric_counts(&ctx.client);
                                    let updated = transport_call(
                                        &router,
                                        &ctx.client,
                                        &handlers,
                                        transport,
                                        PROPERTY_UPDATE,
                                        json!({
                                            "space":ctx.space_id,
                                            "property":property_id,
                                            "name":updated_name,
                                            "key":updated_key
                                        }),
                                    )
                                    .await;
                                    assert_eq!(metric_delta(before, metric_counts(&ctx.client)), (3, 3));
                                    let updated_value = assert_success(&updated);
                                    assert_eq!(updated_value["property"]["name"], updated_name);
                                    assert_eq!(updated_value["property"]["key"], updated_key);
                                    assert!(updated_value.get("tags").is_none());
                                    created_properties[index].1 = updated_name.clone();
                                    let preserved_tags = ctx
                                        .client
                                        .tags(&ctx.space_id, &property_id)
                                        .limit(20)
                                        .offset(0)
                                        .list()
                                        .await?;
                                    assert_eq!(
                                        preserved_tags
                                            .items
                                            .iter()
                                            .map(|tag| tag.id.clone())
                                            .collect::<Vec<_>>(),
                                        original_tag_ids
                                    );
                                }

                                for transport in [Transport::Direct, Transport::Stdio] {
                                    let label = match transport {
                                        Transport::Direct => "empty_direct",
                                        Transport::Stdio => "empty_stdio",
                                    };
                                    let input = json!({
                                        "space":ctx.space_id,
                                        "name":format!("Empty tags {label}"),
                                        "format":"text",
                                        "key":format!("{label}_{suffix}"),
                                        "idempotency_key":format!("empty-{label}-{suffix}")
                                    });
                                    let before = metric_counts(&ctx.client);
                                    let result = transport_call(
                                        &router,
                                        &ctx.client,
                                        &handlers,
                                        transport,
                                        PROPERTY_CREATE,
                                        input,
                                    )
                                    .await;
                                    assert_eq!(metric_delta(before, metric_counts(&ctx.client)), (3, 3));
                                    let value = assert_success(&result);
                                    assert_eq!(value["tags"], json!([]));
                                    let property_id = value["property"]["id"]
                                        .as_str()
                                        .expect("empty-tag property id");
                                    let page = ctx
                                        .client
                                        .tags(&ctx.space_id, property_id)
                                        .limit(20)
                                        .offset(0)
                                        .list()
                                        .await?;
                                    assert_eq!(
                                        (page.pagination.offset, page.pagination.limit),
                                        (0, 20)
                                    );
                                    assert!(!page.pagination.has_more);
                                    assert_eq!(page.pagination.total, 0);
                                    assert!(page.items.is_empty());
                                }

                                let concurrent_input = json!({
                                    "space":ctx.space_id,
                                    "name":"Concurrent property",
                                    "format":"text",
                                    "key":format!("concurrent_{suffix}"),
                                    "idempotency_key":format!("concurrent-{suffix}")
                                });
                                let before = metric_counts(&ctx.client);
                                let (concurrent_a, concurrent_b) = tokio::join!(
                                    direct(&router, PROPERTY_CREATE, concurrent_input.clone()),
                                    direct(&router, PROPERTY_CREATE, concurrent_input.clone()),
                                );
                                assert_eq!(concurrent_a.structured_content, concurrent_b.structured_content);
                                assert_success(&concurrent_a);
                                assert_eq!(metric_delta(before, metric_counts(&ctx.client)), (3, 3));

                                let before = metric_counts(&ctx.client);
                                let changed = direct(
                                    &router,
                                    PROPERTY_CREATE,
                                    json!({
                                        "space":ctx.space_id,
                                        "name":"Changed concurrent property",
                                        "format":"text",
                                        "key":format!("changed_concurrent_{suffix}"),
                                        "idempotency_key":format!("concurrent-{suffix}")
                                    }),
                                )
                                .await;
                                assert_eq!(result_code(&changed), Some("conflict"));
                                assert_eq!(metric_delta(before, metric_counts(&ctx.client)), (0, 0));

                                let capacity_ctx = ctx.clone();
                                let capacity_handlers = SchemaPropertyHandlers::build(
                                    1,
                                    VerifyConfig::default(),
                                    Some(Arc::new(move |property| {
                                        capacity_ctx.register_property(&property.id);
                                        Ok(())
                                    })),
                                )
                                .expect("capacity handlers");
                                let capacity_router = server(
                                    ctx.client.clone(),
                                    false,
                                    capacity_handlers,
                                );
                                let retained = direct(
                                    &capacity_router,
                                    PROPERTY_CREATE,
                                    json!({
                                        "space":ctx.space_id,
                                        "name":"Capacity retained",
                                        "format":"text",
                                        "key":format!("capacity_retained_{suffix}"),
                                        "idempotency_key":format!("capacity-retained-{suffix}")
                                    }),
                                )
                                .await;
                                assert_success(&retained);
                                let before = metric_counts(&ctx.client);
                                let full = direct(
                                    &capacity_router,
                                    PROPERTY_CREATE,
                                    json!({
                                        "space":ctx.space_id,
                                        "name":"Capacity rejected",
                                        "format":"text",
                                        "key":format!("capacity_rejected_{suffix}"),
                                        "idempotency_key":format!("capacity-rejected-{suffix}")
                                    }),
                                )
                                .await;
                                assert_eq!(result_code(&full), Some("bounded_result"));
                                assert_eq!(metric_delta(before, metric_counts(&ctx.client)), (0, 0));

                                let uncertainty_ctx = ctx.clone();
                                let uncertain_handlers = SchemaPropertyHandlers::build(
                                    1,
                                    VerifyConfig::default(),
                                    Some(Arc::new(move |property| {
                                        uncertainty_ctx.register_property(&property.id);
                                        Err(())
                                    })),
                                )
                                .expect("uncertainty handlers");
                                let uncertainty_router = server(
                                    ctx.client.clone(),
                                    false,
                                    uncertain_handlers,
                                );
                                let uncertain_input = json!({
                                    "space":ctx.space_id,
                                    "name":"Retained uncertainty",
                                    "format":"text",
                                    "key":format!("retained_uncertainty_{suffix}"),
                                    "idempotency_key":format!("retained-uncertainty-{suffix}")
                                });
                                let uncertain = direct(
                                    &uncertainty_router,
                                    PROPERTY_CREATE,
                                    uncertain_input.clone(),
                                )
                                .await;
                                assert_eq!(result_code(&uncertain), Some("conflict"));
                                let before = metric_counts(&ctx.client);
                                let retained_uncertain = direct(
                                    &uncertainty_router,
                                    PROPERTY_CREATE,
                                    uncertain_input,
                                )
                                .await;
                                assert_eq!(retained_uncertain.structured_content, uncertain.structured_content);
                                assert_eq!(metric_delta(before, metric_counts(&ctx.client)), (0, 0));

                                for transport in [Transport::Direct, Transport::Stdio] {
                                    let twenty_one = (0..21)
                                        .map(|index| json!({"name":format!("T{index}"),"color":"grey"}))
                                        .collect::<Vec<_>>();
                                    let before = metric_counts(&ctx.client);
                                    match transport {
                                        Transport::Direct => {
                                            let error = router
                                                .dispatch_tool(
                                                    CallToolRequestParams::new(PROPERTY_CREATE)
                                                        .with_arguments(args(json!({
                                                            "space":ctx.space_id,"name":"Rejected",
                                                            "format":"select","tags":twenty_one
                                                        }))),
                                                    &CancellationToken::new(),
                                                )
                                                .await
                                                .expect_err("direct oversized decode");
                                            assert_eq!(error.code.0, -32602);
                                        }
                                        Transport::Stdio => {
                                            let response = preview_stdio_call(
                                                server(ctx.client.clone(), false, handlers.clone()),
                                                PROPERTY_CREATE,
                                                json!({
                                                    "space":ctx.space_id,"name":"Rejected",
                                                    "format":"select","tags":twenty_one
                                                }),
                                            )
                                            .await;
                                            assert_eq!(response["error"]["code"], -32602);
                                        }
                                    }
                                    assert_eq!(before, metric_counts(&ctx.client));
                                }

                                for transport in [Transport::Direct, Transport::Stdio] {
                                    let label = match transport {
                                        Transport::Direct => "cancel_create_direct",
                                        Transport::Stdio => "cancel_create_stdio",
                                    };
                                    let property_key = format!("{label}_{suffix}");
                                    let reached = Arc::new(AtomicBool::new(false));
                                    let hook_reached = reached.clone();
                                    let cancel_handlers = SchemaPropertyHandlers::build(
                                        DEFAULT_IDEMPOTENCY_CAPACITY,
                                        VerifyConfig::default(),
                                        None,
                                    )
                                    .expect("create cancellation handlers")
                                    .with_before_post_hook(Arc::new(move |cancellation| {
                                        hook_reached.store(true, Ordering::SeqCst);
                                        cancellation.cancel();
                                    }));
                                    let input = json!({
                                        "space":ctx.space_id,
                                        "name":format!("Must not create {label}"),
                                        "format":"text",
                                        "key":property_key
                                    });
                                    let before = metric_counts(&ctx.client);
                                    match transport {
                                        Transport::Direct => {
                                            let result = direct(
                                                &server(
                                                    ctx.client.clone(),
                                                    false,
                                                    cancel_handlers,
                                                ),
                                                PROPERTY_CREATE,
                                                input,
                                            )
                                            .await;
                                            assert_eq!(result_code(&result), Some("upstream"));
                                        }
                                        Transport::Stdio => {
                                            let response = preview_stdio_exchange(
                                                server(
                                                    ctx.client.clone(),
                                                    false,
                                                    cancel_handlers,
                                                ),
                                                PROPERTY_CREATE,
                                                input,
                                            )
                                            .await;
                                            if let Some(response) = response {
                                                assert_eq!(
                                                    response["result"]["structuredContent"]["code"],
                                                    "upstream"
                                                );
                                            }
                                        }
                                    }
                                    assert!(reached.load(Ordering::SeqCst));
                                    assert_eq!(metric_delta(before, metric_counts(&ctx.client)), (0, 0));
                                    let missing = ctx
                                        .client
                                        .resolve_property_id(&ctx.space_id, &property_key)
                                        .await
                                        .expect_err("cancelled create must not exist");
                                    assert!(matches!(
                                        missing,
                                        anytype::error::AnytypeError::NotFound { .. }
                                    ));
                                }

                                for (transport, (property_id, current_name)) in [
                                    Transport::Direct,
                                    Transport::Stdio,
                                ]
                                .into_iter()
                                .zip(created_properties.iter())
                                {
                                    let reached = Arc::new(AtomicBool::new(false));
                                    let hook_reached = reached.clone();
                                    let cancel_handlers = SchemaPropertyHandlers::build(
                                        DEFAULT_IDEMPOTENCY_CAPACITY,
                                        VerifyConfig::default(),
                                        None,
                                    )
                                    .expect("cancellation handlers")
                                    .with_before_patch_hook(Arc::new(move |cancellation| {
                                        hook_reached.store(true, Ordering::SeqCst);
                                        cancellation.cancel();
                                    }));
                                    let input = json!({
                                        "space":ctx.space_id,
                                        "property":property_id,
                                        "name":format!("must not replace {current_name}")
                                    });
                                    let before = metric_counts(&ctx.client);
                                    match transport {
                                        Transport::Direct => {
                                            let cancel_router = server(
                                                ctx.client.clone(),
                                                false,
                                                cancel_handlers,
                                            );
                                            let result = direct(
                                                &cancel_router,
                                                PROPERTY_UPDATE,
                                                input,
                                            )
                                            .await;
                                            assert_eq!(result_code(&result), Some("upstream"));
                                        }
                                        Transport::Stdio => {
                                            let response = preview_stdio_exchange(
                                                server(
                                                    ctx.client.clone(),
                                                    false,
                                                    cancel_handlers,
                                                ),
                                                PROPERTY_UPDATE,
                                                input,
                                            )
                                            .await;
                                            if let Some(response) = response {
                                                assert_eq!(
                                                    response["result"]["structuredContent"]["code"],
                                                    "upstream"
                                                );
                                            }
                                        }
                                    }
                                    assert!(reached.load(Ordering::SeqCst));
                                    assert_eq!(
                                        metric_delta(before, metric_counts(&ctx.client)),
                                        (1, 1)
                                    );
                                    let unchanged = ctx
                                        .client
                                        .property(&ctx.space_id, property_id)
                                        .get_direct()
                                        .await?;
                                    assert_eq!(unchanged.name, *current_name);
                                }

                                let bad_auth_client =
                                    AnytypeClient::with_config(ctx.client.get_config().clone())?;
                                bad_auth_client.set_api_key(HttpCredentials::new(format!(
                                    "invalid-schema-property-{suffix}"
                                )));
                                let bad_handlers = SchemaPropertyHandlers::new()
                                    .expect("bad-auth property handlers");
                                let bad_router = server(
                                    bad_auth_client.clone(),
                                    false,
                                    bad_handlers.clone(),
                                );
                                for transport in [Transport::Direct, Transport::Stdio] {
                                    let result = transport_call(
                                        &bad_router,
                                        &bad_auth_client,
                                        &bad_handlers,
                                        transport,
                                        PROPERTY_CREATE,
                                        json!({
                                            "space":ctx.space_id,
                                            "name":"Unauthorized",
                                            "format":"text"
                                        }),
                                    )
                                    .await;
                                    assert_eq!(result_code(&result), Some("authentication"));
                                    let encoded = serde_json::to_string(&result)
                                        .expect("authentication result");
                                    assert!(!encoded.contains("invalid-schema-property"));
                                    assert!(!encoded.contains("Unauthorized"));
                                }
                                Ok(())
                            })
                        },
                    ))
                    .await
                    .expect("cleanup-safe live property workflow");
                    match outcome {
                        DisposableRun::Completed(()) => {}
                        DisposableRun::Skipped(reason) => {
                            eprintln!("schema-property suite skipped: {reason:?}");
                        }
                    }
                });
            })
            .expect("spawn schema-property live thread")
            .join()
            .expect("schema-property live thread");
    }
}
