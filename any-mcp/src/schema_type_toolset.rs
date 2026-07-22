// any-mcp - bounded, workflow-oriented MCP server for Anytype
//
// SPDX-FileCopyrightText: 2026 Steve Schoettler
// SPDX-License-Identifier: Apache-2.0

//! Optional schema-toolset workflows for bounded type reads and mutations.
//!
//! This module exports a complete, reviewed type slice without linking the
//! incomplete `schema` registry into production. Terminal schema integration
//! composes it with the independently landed space, property, and tag slices.

use std::{
    borrow::Cow,
    collections::HashSet,
    fmt,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use anytype::{
    error::AnytypeError,
    objects::ObjectLayout,
    prelude::{AnytypeClient, VerifyConfig, verify_semantic, verify_semantic_with_remaining},
    properties::{Property, PropertyFormat as ApiPropertyFormat},
    types::{
        CreateTypeProperty, MAX_TYPE_PROPERTY_RPC_TIMEOUT, Type, TypeLayout as ApiTypeLayout,
        TypePropertyClassification,
    },
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
        execute_mutation_handler, execute_prepared_handler, require_mutation_access,
    },
    optional_toolsets::OptionalRegistryTool,
    protocol::{ToolProfile, WorkflowTool, workflow_tool},
    result::tool_error,
    runtime::{OperationContext, RuntimeContext},
    schema::SchemaContractError,
    schema_space_toolset::InputName,
    server::decode_arguments,
    validation::{Omittable, optional_non_null_schema},
};

/// Exact tool name for cache-independent type metadata reads.
pub const TYPE_GET: &str = "type_get";
/// Exact tool name for bounded type creation.
pub const TYPE_CREATE: &str = "type_create";
/// Exact tool name for bounded type metadata/recommendation updates.
pub const TYPE_UPDATE: &str = "type_update";

/// Reviewed HTTP logical ceiling for `type_get`.
pub const TYPE_GET_LOGICAL_CEILING: usize = 23;
/// Reviewed HTTP physical ceiling for `type_get`.
pub const TYPE_GET_PHYSICAL_CEILING: usize = 138;
/// Reviewed HTTP logical ceiling for `type_create`.
pub const TYPE_CREATE_LOGICAL_CEILING: usize = 22;
/// Reviewed HTTP physical ceiling for `type_create`.
pub const TYPE_CREATE_PHYSICAL_CEILING: usize = 127;
/// Reviewed HTTP logical ceiling for metadata-only `type_update`.
pub const TYPE_UPDATE_METADATA_LOGICAL_CEILING: usize = 34;
/// Reviewed HTTP physical ceiling for metadata-only `type_update`.
pub const TYPE_UPDATE_METADATA_PHYSICAL_CEILING: usize = 199;
/// Successful explicit-recommendation no-op logical ceiling across transports.
pub const TYPE_UPDATE_NOOP_LOGICAL_CEILING: usize = 26;
/// Successful explicit-recommendation no-op physical ceiling across transports.
pub const TYPE_UPDATE_NOOP_PHYSICAL_CEILING: usize = 146;
/// HTTP-only logical ceiling for an explicit-recommendation semantic no-op.
pub const TYPE_UPDATE_NOOP_HTTP_LOGICAL_CEILING: usize = 24;
/// HTTP-only physical ceiling for an explicit-recommendation semantic no-op.
pub const TYPE_UPDATE_NOOP_HTTP_PHYSICAL_CEILING: usize = 144;
/// Successful explicit-recommendation write logical ceiling across transports.
pub const TYPE_UPDATE_RECOMMENDATION_LOGICAL_CEILING: usize = 67;
/// Successful explicit-recommendation write physical ceiling across transports.
pub const TYPE_UPDATE_RECOMMENDATION_PHYSICAL_CEILING: usize = 287;
/// HTTP-only logical ceiling for an explicit-recommendation write and readback.
pub const TYPE_UPDATE_RECOMMENDATION_HTTP_LOGICAL_CEILING: usize = 45;
/// HTTP-only physical ceiling for an explicit-recommendation write and readback.
pub const TYPE_UPDATE_RECOMMENDATION_HTTP_PHYSICAL_CEILING: usize = 265;
/// Absolute logical ceiling when terminal close fallback is required.
pub const TYPE_UPDATE_FALLBACK_LOGICAL_CEILING: usize = 68;
/// Absolute physical ceiling when terminal close fallback is required.
pub const TYPE_UPDATE_FALLBACK_PHYSICAL_CEILING: usize = 288;

const MAX_SCHEMA_KEY_CHARS: usize = 256;
const MAX_PROPERTIES: usize = 20;
const TYPE_CREATE_FINGERPRINT_DOMAIN: &str = "any-mcp/schema-type-create/v1";

type TypeCreateObserver = Arc<dyn Fn(&Type) -> Result<(), ()> + Send + Sync>;
type BeforePatchHook = Arc<dyn Fn(&CancellationToken) + Send + Sync>;

/// Mutation-safe schema key accepted only on type/property input.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct SchemaKey(String);

impl SchemaKey {
    /// Validates the exact lower-snake-case mutation key.
    pub fn new(value: impl Into<String>) -> Result<Self, TypeInputError> {
        let value = value.into();
        let mut bytes = value.bytes();
        let Some(first) = bytes.next() else {
            return Err(TypeInputError::InvalidKey);
        };
        if value.chars().count() > MAX_SCHEMA_KEY_CHARS
            || !first.is_ascii_lowercase()
            || !bytes.all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
        {
            return Err(TypeInputError::InvalidKey);
        }
        Ok(Self(value))
    }

    /// Borrows the exact validated key.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for SchemaKey {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::new(String::deserialize(deserializer)?).map_err(de::Error::custom)
    }
}

impl JsonSchema for SchemaKey {
    fn schema_name() -> Cow<'static, str> {
        "SchemaKey".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "minLength": 1,
            "maxLength": MAX_SCHEMA_KEY_CHARS,
            "pattern": "^[a-z][a-z0-9_]*$"
        })
    }
}

/// Failure to construct a strict type workflow value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypeInputError {
    /// A mutation key is outside the exact reviewed grammar.
    InvalidKey,
    /// A property batch is outside its reviewed item bounds.
    PropertyCount,
    /// A property batch repeats a key.
    DuplicatePropertyKey,
}

impl fmt::Display for TypeInputError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidKey => "key must match the schema-key grammar",
            Self::PropertyCount => "property batch is outside its item bounds",
            Self::DuplicatePropertyKey => "property keys must be unique",
        })
    }
}

impl std::error::Error for TypeInputError {}

/// Closed property format accepted by schema type workflows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PropertyFormat {
    /// Plain text.
    Text,
    /// Number.
    Number,
    /// Single select.
    Select,
    /// Multiple select.
    MultiSelect,
    /// Date/time.
    Date,
    /// File attachments.
    Files,
    /// Boolean checkbox.
    Checkbox,
    /// URL.
    Url,
    /// Email address.
    Email,
    /// Phone number.
    Phone,
    /// Object references.
    Objects,
}

impl From<PropertyFormat> for ApiPropertyFormat {
    fn from(value: PropertyFormat) -> Self {
        match value {
            PropertyFormat::Text => Self::Text,
            PropertyFormat::Number => Self::Number,
            PropertyFormat::Select => Self::Select,
            PropertyFormat::MultiSelect => Self::MultiSelect,
            PropertyFormat::Date => Self::Date,
            PropertyFormat::Files => Self::Files,
            PropertyFormat::Checkbox => Self::Checkbox,
            PropertyFormat::Url => Self::Url,
            PropertyFormat::Email => Self::Email,
            PropertyFormat::Phone => Self::Phone,
            PropertyFormat::Objects => Self::Objects,
        }
    }
}

impl From<ApiPropertyFormat> for PropertyFormat {
    fn from(value: ApiPropertyFormat) -> Self {
        match value {
            ApiPropertyFormat::Text => Self::Text,
            ApiPropertyFormat::Number => Self::Number,
            ApiPropertyFormat::Select => Self::Select,
            ApiPropertyFormat::MultiSelect => Self::MultiSelect,
            ApiPropertyFormat::Date => Self::Date,
            ApiPropertyFormat::Files => Self::Files,
            ApiPropertyFormat::Checkbox => Self::Checkbox,
            ApiPropertyFormat::Url => Self::Url,
            ApiPropertyFormat::Email => Self::Email,
            ApiPropertyFormat::Phone => Self::Phone,
            ApiPropertyFormat::Objects => Self::Objects,
        }
    }
}

/// One complete property definition in a type create/update request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PropertySpec {
    /// Exact nonempty display name.
    name: InputName,
    /// Exact lower-snake-case property key.
    key: SchemaKey,
    /// Closed Anytype property format.
    format: PropertyFormat,
}

impl PropertySpec {
    fn to_api(&self) -> CreateTypeProperty {
        CreateTypeProperty {
            name: self.name.as_str().to_owned(),
            key: self.key.as_str().to_owned(),
            format: self.format.into(),
        }
    }
}

#[derive(Debug, Clone)]
struct CreatePropertyBatch(Vec<PropertySpec>);

impl<'de> Deserialize<'de> for CreatePropertyBatch {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        validate_property_batch(Vec::<PropertySpec>::deserialize(deserializer)?, false)
            .map(Self)
            .map_err(de::Error::custom)
    }
}

impl JsonSchema for CreatePropertyBatch {
    fn schema_name() -> Cow<'static, str> {
        "CreatePropertyBatch".into()
    }

    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        let item = generator.subschema_for::<PropertySpec>();
        json_schema!({
            "type": "array",
            "minItems": 1,
            "maxItems": MAX_PROPERTIES,
            "items": item
        })
    }
}

#[derive(Debug, Clone)]
struct RecommendedPropertyBatch(Vec<PropertySpec>);

impl<'de> Deserialize<'de> for RecommendedPropertyBatch {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        validate_property_batch(Vec::<PropertySpec>::deserialize(deserializer)?, true)
            .map(Self)
            .map_err(de::Error::custom)
    }
}

impl JsonSchema for RecommendedPropertyBatch {
    fn schema_name() -> Cow<'static, str> {
        "RecommendedPropertyBatch".into()
    }

    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        let item = generator.subschema_for::<PropertySpec>();
        json_schema!({
            "type": "array",
            "minItems": 0,
            "maxItems": MAX_PROPERTIES,
            "items": item
        })
    }
}

fn validate_property_batch(
    properties: Vec<PropertySpec>,
    empty_allowed: bool,
) -> Result<Vec<PropertySpec>, TypeInputError> {
    if properties.len() > MAX_PROPERTIES || (!empty_allowed && properties.is_empty()) {
        return Err(TypeInputError::PropertyCount);
    }
    let mut keys = HashSet::with_capacity(properties.len());
    if properties
        .iter()
        .any(|property| !keys.insert(property.key.as_str()))
    {
        return Err(TypeInputError::DuplicatePropertyKey);
    }
    Ok(properties)
}

/// Closed layout accepted by create/update mutations.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TypeLayout {
    /// Standard type.
    #[default]
    Basic,
    /// Profile type.
    Profile,
    /// Action type.
    Action,
    /// Note type.
    Note,
}

impl From<TypeLayout> for ApiTypeLayout {
    fn from(value: TypeLayout) -> Self {
        match value {
            TypeLayout::Basic => Self::Basic,
            TypeLayout::Profile => Self::Profile,
            TypeLayout::Action => Self::Action,
            TypeLayout::Note => Self::Note,
        }
    }
}

impl From<TypeLayout> for ExistingTypeLayout {
    fn from(value: TypeLayout) -> Self {
        match value {
            TypeLayout::Basic => Self::Basic,
            TypeLayout::Profile => Self::Profile,
            TypeLayout::Action => Self::Action,
            TypeLayout::Note => Self::Note,
        }
    }
}

/// Closed layout emitted for existing types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ExistingTypeLayout {
    /// Standard type.
    Basic,
    /// Profile type.
    Profile,
    /// Action type.
    Action,
    /// Note type.
    Note,
    /// Bookmark type.
    Bookmark,
    /// Query set.
    Set,
    /// Manual collection.
    Collection,
    /// Space participant.
    Participant,
    /// Chat type.
    Chat,
}

impl From<ObjectLayout> for ExistingTypeLayout {
    fn from(value: ObjectLayout) -> Self {
        match value {
            ObjectLayout::Basic => Self::Basic,
            ObjectLayout::Profile => Self::Profile,
            ObjectLayout::Action => Self::Action,
            ObjectLayout::Note => Self::Note,
            ObjectLayout::Bookmark => Self::Bookmark,
            ObjectLayout::Set => Self::Set,
            ObjectLayout::Collection => Self::Collection,
            ObjectLayout::Participant => Self::Participant,
            ObjectLayout::Chat => Self::Chat,
        }
    }
}

/// Strict input for one cache-independent type metadata read.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TypeGetInput {
    /// Unique space name or identifier.
    space: DiscoveryReference,
    /// Unique type name, `@key`, key, or identifier.
    #[serde(rename = "type")]
    type_ref: DiscoveryReference,
}

/// Strict input for one bounded type create.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TypeCreateInput {
    /// Unique space name or identifier.
    space: DiscoveryReference,
    /// Exact nonempty type name.
    name: InputName,
    /// Optional plural display name; omission uses the API default.
    #[serde(default)]
    #[schemars(schema_with = "optional_input_name_schema")]
    plural_name: Omittable<InputName>,
    /// Optional exact mutation-safe key.
    #[serde(default)]
    #[schemars(schema_with = "optional_schema_key_schema")]
    key: Omittable<SchemaKey>,
    /// Optional layout; omission defaults to `basic`.
    #[serde(default)]
    #[schemars(schema_with = "optional_layout_schema")]
    layout: Omittable<TypeLayout>,
    /// Optional complete 1..20 property batch.
    #[serde(default)]
    #[schemars(schema_with = "optional_create_properties_schema")]
    properties: Omittable<CreatePropertyBatch>,
    /// Optional process-local create retry key.
    #[serde(default)]
    #[schemars(schema_with = "optional_idempotency_schema")]
    idempotency_key: Omittable<IdempotencyKey>,
}

fn optional_input_name_schema(generator: &mut SchemaGenerator) -> Schema {
    optional_non_null_schema::<InputName>(generator)
}

fn optional_schema_key_schema(generator: &mut SchemaGenerator) -> Schema {
    optional_non_null_schema::<SchemaKey>(generator)
}

fn optional_layout_schema(generator: &mut SchemaGenerator) -> Schema {
    optional_non_null_schema::<TypeLayout>(generator)
}

fn optional_create_properties_schema(generator: &mut SchemaGenerator) -> Schema {
    optional_non_null_schema::<CreatePropertyBatch>(generator)
}

fn optional_idempotency_schema(generator: &mut SchemaGenerator) -> Schema {
    optional_non_null_schema::<IdempotencyKey>(generator)
}

/// Strict input for a one-write type metadata/recommendation update.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TypeUpdateInput {
    /// Unique space name or identifier.
    space: DiscoveryReference,
    /// Unique type name, `@key`, key, or identifier.
    #[serde(rename = "type")]
    type_ref: DiscoveryReference,
    /// Optional replacement name.
    #[serde(default)]
    name: Omittable<InputName>,
    /// Optional replacement key.
    #[serde(default)]
    key: Omittable<SchemaKey>,
    /// Optional replacement plural name.
    #[serde(default)]
    plural_name: Omittable<InputName>,
    /// Optional replacement layout.
    #[serde(default)]
    layout: Omittable<TypeLayout>,
    /// Optional complete ordered 0..20 replaceable recommendation set.
    #[serde(default)]
    recommended_properties: Omittable<RecommendedPropertyBatch>,
}

#[allow(dead_code)]
#[derive(JsonSchema)]
#[serde(deny_unknown_fields)]
struct TypeUpdateInputSchema {
    /// Unique space name or identifier.
    space: DiscoveryReference,
    /// Unique type name, `@key`, key, or identifier.
    #[serde(rename = "type")]
    type_ref: DiscoveryReference,
    /// Optional replacement name.
    #[serde(default)]
    #[schemars(schema_with = "optional_input_name_schema")]
    name: Omittable<InputName>,
    /// Optional replacement key.
    #[serde(default)]
    #[schemars(schema_with = "optional_schema_key_schema")]
    key: Omittable<SchemaKey>,
    /// Optional replacement plural name.
    #[serde(default)]
    #[schemars(schema_with = "optional_input_name_schema")]
    plural_name: Omittable<InputName>,
    /// Optional replacement layout.
    #[serde(default)]
    #[schemars(schema_with = "optional_layout_schema")]
    layout: Omittable<TypeLayout>,
    /// Optional complete ordered 0..20 replaceable recommendation set.
    #[serde(default)]
    #[schemars(schema_with = "optional_recommended_properties_schema")]
    recommended_properties: Omittable<RecommendedPropertyBatch>,
}

fn optional_recommended_properties_schema(generator: &mut SchemaGenerator) -> Schema {
    optional_non_null_schema::<RecommendedPropertyBatch>(generator)
}

impl JsonSchema for TypeUpdateInput {
    fn schema_name() -> Cow<'static, str> {
        "TypeUpdateInput".into()
    }

    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        let mut schema = TypeUpdateInputSchema::json_schema(generator);
        schema
            .ensure_object()
            .insert("minProperties".to_owned(), 3_u64.into());
        schema
    }
}

impl TypeUpdateInput {
    fn has_mutation(&self) -> bool {
        !self.name.is_none()
            || !self.key.is_none()
            || !self.plural_name.is_none()
            || !self.layout.is_none()
            || !self.recommended_properties.is_none()
    }
}

/// Caller-visible bounded type metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TypeSummary {
    /// Stable type identifier.
    id: EntityId,
    /// Exact bounded type key.
    key: TypeKey,
    /// Optional bounded display name.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(default, schema_with = "optional_output_name_schema")]
    name: Option<DisplayName>,
    /// Optional bounded plural display name.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(default, schema_with = "optional_output_name_schema")]
    plural_name: Option<DisplayName>,
    /// Existing Anytype layout.
    layout: ExistingTypeLayout,
    /// Whether the type is archived.
    archived: bool,
}

fn optional_output_name_schema(generator: &mut SchemaGenerator) -> Schema {
    generator.subschema_for::<DisplayName>()
}

/// Exact result shared by type get/create/update.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TypeOutput {
    /// Verified type metadata; properties never enter the result.
    #[serde(rename = "type")]
    type_summary: TypeSummary,
}

/// Constructs the approved `type_get` contract.
pub fn type_get_tool() -> Result<WorkflowTool<TypeOutput>, SchemaContractError> {
    workflow_tool::<TypeGetInput, TypeOutput>(
        TYPE_GET,
        "Read one exact Anytype type through a cache-independent scoped GET and return bounded metadata without property expansion.",
        ToolProfile::Read,
    )
}

/// Constructs the approved `type_create` contract.
pub fn type_create_tool() -> Result<WorkflowTool<TypeOutput>, SchemaContractError> {
    workflow_tool::<TypeCreateInput, TypeOutput>(
        TYPE_CREATE,
        "Create one Anytype type with optional complete bounded properties, verify requested metadata and properties, and return metadata only. A retry key deduplicates identical verified creates for this process.",
        ToolProfile::Create,
    )
}

/// Constructs the approved `type_update` contract.
pub fn type_update_tool() -> Result<WorkflowTool<TypeOutput>, SchemaContractError> {
    workflow_tool::<TypeUpdateInput, TypeOutput>(
        TYPE_UPDATE,
        "Update supplied metadata and optionally preserve, clear, or completely replace the ordered non-featured recommendation set with one non-replayed PATCH and exact featured-safe readback.",
        ToolProfile::Update,
    )
}

/// Returns the complete schema-type slice for later registry composition.
pub fn schema_type_tools() -> Result<Vec<OptionalRegistryTool>, SchemaContractError> {
    Ok(vec![
        OptionalRegistryTool::read(type_get_tool()?),
        OptionalRegistryTool::mutation(type_create_tool()?),
        OptionalRegistryTool::mutation(type_update_tool()?),
    ])
}

/// Stateful transport-neutral handlers for the schema type slice.
#[derive(Clone)]
pub struct SchemaTypeHandlers {
    idempotency: Arc<IdempotencyStore>,
    verify_config: VerifyConfig,
    get_contract: WorkflowTool<TypeOutput>,
    create_contract: WorkflowTool<TypeOutput>,
    update_contract: WorkflowTool<TypeOutput>,
    create_observer: Option<TypeCreateObserver>,
    before_patch: Option<BeforePatchHook>,
}

impl fmt::Debug for SchemaTypeHandlers {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SchemaTypeHandlers")
            .field("verify_config", &self.verify_config)
            .field("create_observer", &self.create_observer.is_some())
            .field("before_patch", &self.before_patch.is_some())
            .finish_non_exhaustive()
    }
}

impl SchemaTypeHandlers {
    /// Creates handlers with reviewed finite verification and idempotency bounds.
    pub fn new() -> Result<Self, SchemaContractError> {
        Self::build(DEFAULT_IDEMPOTENCY_CAPACITY, VerifyConfig::default(), None)
    }

    fn build(
        capacity: usize,
        verify_config: VerifyConfig,
        create_observer: Option<TypeCreateObserver>,
    ) -> Result<Self, SchemaContractError> {
        Ok(Self {
            idempotency: Arc::new(IdempotencyStore::new(capacity)),
            verify_config,
            get_contract: type_get_tool()?,
            create_contract: type_create_tool()?,
            update_contract: type_update_tool()?,
            create_observer,
            before_patch: None,
        })
    }

    #[cfg(test)]
    fn with_before_patch_hook(mut self, hook: BeforePatchHook) -> Self {
        self.before_patch = Some(hook);
        self
    }

    /// Dispatches one schema-type tool after the caller's catalog gate.
    pub async fn call_tool(
        &self,
        request: CallToolRequestParams,
        runtime: &RuntimeContext,
        cancellation: &CancellationToken,
    ) -> Result<CallToolResult, ErrorData> {
        if runtime.is_read_only() && matches!(request.name.as_ref(), TYPE_CREATE | TYPE_UPDATE) {
            return Ok(tool_error(&ToolError::validation()));
        }
        match request.name.as_ref() {
            TYPE_GET => {
                let input = decode_arguments::<TypeGetInput>(request.arguments)?;
                Ok(self.type_get(runtime, input, cancellation).await)
            }
            TYPE_CREATE => {
                let input = decode_arguments::<TypeCreateInput>(request.arguments)?;
                Ok(self
                    .type_create(runtime, MutationAccess::Allowed, input, cancellation)
                    .await)
            }
            TYPE_UPDATE => {
                let input = decode_arguments::<TypeUpdateInput>(request.arguments)?;
                Ok(self
                    .type_update(runtime, MutationAccess::Allowed, input, cancellation)
                    .await)
            }
            _ => Err(ErrorData::method_not_found::<CallToolRequestMethod>()),
        }
    }

    async fn type_get(
        &self,
        runtime: &RuntimeContext,
        input: TypeGetInput,
        cancellation: &CancellationToken,
    ) -> CallToolResult {
        let client = runtime.client().clone();
        execute_prepared_handler(
            runtime,
            &self.get_contract,
            OperationContext::new(TYPE_GET),
            cancellation,
            async move {
                let (space_id, type_id) =
                    resolve_type(&client, &input.space, &input.type_ref).await?;
                let typ = client
                    .get_type(space_id.as_str(), type_id.as_str())
                    .get_direct()
                    .await?;
                checked_type_summary(&typ, Some(&type_id))
                    .map(TypeOutput::from)
                    .map_err(HandlerOperationError::from)
            },
            |output| async move { Ok(output) },
        )
        .await
    }

    async fn type_create(
        &self,
        runtime: &RuntimeContext,
        access: MutationAccess,
        input: TypeCreateInput,
        cancellation: &CancellationToken,
    ) -> CallToolResult {
        if let Err(error) = require_mutation_access(access) {
            return tool_error(error.tool_error());
        }
        let normalized = NormalizedTypeCreate::from(input);
        let Some(key) = normalized.idempotency_key.clone() else {
            let progress = MutationProgress::new();
            return execute_type_create(
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
                let supervision = TypeCreateSupervision {
                    runtime: runtime.clone(),
                    contract: self.create_contract.clone(),
                    store: self.idempotency.clone(),
                    key,
                    attempt: attempt.clone(),
                    normalized,
                    verify_config: self.verify_config.clone(),
                    observer: self.create_observer.clone(),
                };
                tokio::spawn(async move { supervise_type_create(supervision).await });
                wait_for_attempt(attempt, cancellation).await
            }
        }
    }

    async fn type_update(
        &self,
        runtime: &RuntimeContext,
        access: MutationAccess,
        input: TypeUpdateInput,
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
        let operation_cancellation = cancellation.clone();
        let before_patch = self.before_patch.clone();
        execute_mutation_handler(
            runtime,
            &self.update_contract,
            OperationContext::new(TYPE_UPDATE),
            cancellation,
            &progress,
            async move {
                let (space_id, type_id) =
                    resolve_type(&client, &input.space, &input.type_ref).await?;
                let current = client
                    .get_type(space_id.as_str(), type_id.as_str())
                    .get_direct()
                    .await?;
                checked_type_summary(&current, Some(&type_id))
                    .map_err(HandlerOperationError::from)?;

                let baseline = if input.recommended_properties.is_none() {
                    None
                } else {
                    let classification = client
                        .get_type(space_id.as_str(), type_id.as_str())
                        .classify_properties_with_deadline(MAX_TYPE_PROPERTY_RPC_TIMEOUT)
                        .await?;
                    Some(
                        checked_classification(&classification)
                            .map_err(classification_preflight)?,
                    )
                };

                if operation_cancellation.is_cancelled() {
                    return Err(HandlerError::new(ToolError::upstream()).into());
                }
                if update_already_satisfied(&current, &type_id, &input, baseline.as_ref()) {
                    return checked_type_summary(&current, Some(&type_id))
                        .map(TypeOutput::from)
                        .map_err(HandlerOperationError::from);
                }

                let mut request = client
                    .update_type(space_id.as_str(), type_id.as_str())
                    .no_verify();
                if let Some(name) = input.name.as_ref() {
                    request = request.name(name.as_str());
                }
                if let Some(key) = input.key.as_ref() {
                    request = request.key(key.as_str());
                }
                if let Some(plural_name) = input.plural_name.as_ref() {
                    request = request.plural_name(plural_name.as_str());
                }
                if let Some(layout) = input.layout.as_ref() {
                    request = request.layout((*layout).into());
                }
                if let Some(properties) = input.recommended_properties.as_ref() {
                    request = request.properties(properties.0.iter().map(PropertySpec::to_api));
                }

                if let Some(hook) = before_patch {
                    hook(&operation_cancellation);
                }
                if operation_cancellation.is_cancelled() {
                    return Err(HandlerError::new(ToolError::upstream()).into());
                }
                operation_progress.mark_dispatched();
                let response_anomaly = match request.update().await {
                    Ok(returned) => !type_matches_update_metadata(&returned, &type_id, &input),
                    Err(error) if type_patch_rejection_is_definitive(&error) => {
                        return Err(error.into());
                    }
                    Err(_) => true,
                };

                let verify_client = client.clone();
                let verify_space_id = space_id.as_str().to_owned();
                let verify_type_id = type_id.as_str().to_owned();
                let verify_recommendations = baseline.is_some();
                let verified = verify_semantic_with_remaining(
                    &verify_config,
                    "type",
                    type_id.as_str(),
                    move |remaining| {
                        let client = verify_client.clone();
                        let space_id = verify_space_id.clone();
                        let type_id = verify_type_id.clone();
                        async move {
                            let typ = client.get_type(&space_id, &type_id).get_direct().await?;
                            let classification = if verify_recommendations {
                                let timeout = remaining.min(MAX_TYPE_PROPERTY_RPC_TIMEOUT);
                                let raw = client
                                    .get_type(&space_id, &type_id)
                                    .classify_properties_with_deadline(timeout)
                                    .await?;
                                Some(checked_classification(&raw).map_err(|_| {
                                    AnytypeError::Other {
                                        message: "type verification classification was invalid"
                                            .to_owned(),
                                    }
                                })?)
                            } else {
                                None
                            };
                            Ok(TypeEvidence {
                                typ,
                                classification,
                            })
                        }
                    },
                    |evidence| {
                        evidence_matches_update(evidence, &type_id, &input, baseline.as_ref())
                    },
                )
                .await
                .map_err(|_| indeterminate_operation())?;

                if response_anomaly {
                    return Err(indeterminate_operation());
                }
                checked_type_summary(&verified.typ, Some(&type_id))
                    .map(TypeOutput::from)
                    .map_err(|_| indeterminate_operation())
            },
            |output| async move { Ok(output) },
        )
        .await
    }
}

#[derive(Clone)]
struct NormalizedTypeCreate {
    space: DiscoveryReference,
    name: InputName,
    plural_name: Option<InputName>,
    key: Option<SchemaKey>,
    layout: TypeLayout,
    properties: Vec<PropertySpec>,
    idempotency_key: Option<IdempotencyKey>,
}

impl From<TypeCreateInput> for NormalizedTypeCreate {
    fn from(input: TypeCreateInput) -> Self {
        Self {
            space: input.space,
            name: input.name,
            plural_name: input.plural_name.as_ref().cloned(),
            key: input.key.as_ref().cloned(),
            layout: input.layout.as_ref().copied().unwrap_or_default(),
            properties: input
                .properties
                .as_ref()
                .map_or_else(Vec::new, |properties| properties.0.clone()),
            idempotency_key: input.idempotency_key.as_ref().cloned(),
        }
    }
}

impl NormalizedTypeCreate {
    fn fingerprint(&self) -> [u8; 32] {
        let mut hasher = Sha256::new();
        hash_field(&mut hasher, TYPE_CREATE_FINGERPRINT_DOMAIN);
        hash_field(&mut hasher, self.space.as_str());
        hash_field(&mut hasher, self.name.as_str());
        hash_optional(
            &mut hasher,
            self.plural_name.as_ref().map(InputName::as_str),
        );
        hash_optional(&mut hasher, self.key.as_ref().map(SchemaKey::as_str));
        hash_field(&mut hasher, type_layout_name(self.layout));
        hasher.update(self.properties.len().to_be_bytes());
        for property in &self.properties {
            hash_field(&mut hasher, property.name.as_str());
            hash_field(&mut hasher, property.key.as_str());
            hash_field(&mut hasher, property_format_name(property.format));
        }
        hasher.finalize().into()
    }
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

struct TypeCreateSupervision {
    runtime: RuntimeContext,
    contract: WorkflowTool<TypeOutput>,
    store: Arc<IdempotencyStore>,
    key: IdempotencyKey,
    attempt: Arc<Attempt>,
    normalized: NormalizedTypeCreate,
    verify_config: VerifyConfig,
    observer: Option<TypeCreateObserver>,
}

async fn supervise_type_create(supervision: TypeCreateSupervision) {
    let TypeCreateSupervision {
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
        execute_type_create(
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

async fn execute_type_create(
    runtime: &RuntimeContext,
    contract: &WorkflowTool<TypeOutput>,
    input: NormalizedTypeCreate,
    cancellation: &CancellationToken,
    progress: &MutationProgress,
    verify_config: &VerifyConfig,
    observer: Option<TypeCreateObserver>,
) -> CreateExecution {
    let client = runtime.client().clone();
    let definitive_rejection = Arc::new(AtomicBool::new(false));
    let operation_rejection = definitive_rejection.clone();
    let operation_progress = progress.clone();
    let verify_config = verify_config.clone();
    let result = execute_mutation_handler(
        runtime,
        contract,
        OperationContext::new(TYPE_CREATE),
        cancellation,
        progress,
        async move {
            let resolved = client.resolve_space_id(input.space.as_str()).await?;
            let space_id = EntityId::new(resolved).map_err(unsafe_upstream)?;
            let mut request = client
                .new_type(space_id.as_str(), input.name.as_str())
                .layout(input.layout.into())
                .properties(input.properties.iter().map(PropertySpec::to_api))
                .no_verify();
            if let Some(plural_name) = input.plural_name.as_ref() {
                request = request.plural_name(plural_name.as_str());
            }
            if let Some(key) = input.key.as_ref() {
                request = request.key(key.as_str());
            }

            operation_progress.mark_dispatched();
            let created = match request.create().await {
                Ok(created) => created,
                Err(error) => {
                    if crate::error::mutation_rejection_is_definitive(&error) {
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
            let response_matches = type_matches_create(&created, &id, &input);
            let verified = verify_semantic(
                &verify_config,
                "type",
                id.as_str(),
                || client.get_type(space_id.as_str(), id.as_str()).get_direct(),
                |typ| type_matches_create(typ, &id, &input),
            )
            .await
            .map_err(|_| indeterminate_operation())?;
            if !response_matches {
                return Err(indeterminate_operation());
            }
            checked_type_summary(&verified, Some(&id))
                .map(TypeOutput::from)
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

impl From<TypeSummary> for TypeOutput {
    fn from(type_summary: TypeSummary) -> Self {
        Self { type_summary }
    }
}

async fn resolve_type(
    client: &AnytypeClient,
    space: &DiscoveryReference,
    type_ref: &DiscoveryReference,
) -> Result<(EntityId, EntityId), HandlerOperationError> {
    let resolved_space = client.resolve_space_id(space.as_str()).await?;
    let space_id = EntityId::new(resolved_space).map_err(unsafe_upstream)?;
    let resolved_type = client
        .resolve_type_id(space_id.as_str(), type_ref.as_str())
        .await?;
    let type_id = EntityId::new(resolved_type).map_err(unsafe_upstream)?;
    Ok((space_id, type_id))
}

fn checked_type_summary(
    typ: &Type,
    expected_id: Option<&EntityId>,
) -> Result<TypeSummary, HandlerError> {
    let id = EntityId::new(typ.id.clone()).map_err(unsafe_domain)?;
    if expected_id.is_some_and(|expected| expected != &id) {
        return Err(HandlerError::new(ToolError::upstream()));
    }
    let key = TypeKey::new(typ.key.clone()).map_err(unsafe_domain)?;
    let name = typ
        .name
        .clone()
        .map(DisplayName::new)
        .transpose()
        .map_err(unsafe_domain)?;
    let plural_name = typ
        .plural_name
        .clone()
        .map(DisplayName::new)
        .transpose()
        .map_err(unsafe_domain)?;
    Ok(TypeSummary {
        id,
        key,
        name,
        plural_name,
        layout: typ.layout.clone().into(),
        archived: typ.archived,
    })
}

fn type_matches_create(typ: &Type, expected_id: &EntityId, input: &NormalizedTypeCreate) -> bool {
    let Ok(summary) = checked_type_summary(typ, Some(expected_id)) else {
        return false;
    };
    summary.name.as_ref().map(DisplayName::as_str) == Some(input.name.as_str())
        && input.plural_name.as_ref().is_none_or(|expected| {
            summary.plural_name.as_ref().map(DisplayName::as_str) == Some(expected.as_str())
        })
        && input
            .key
            .as_ref()
            .is_none_or(|expected| summary.key.as_str() == expected.as_str())
        && summary.layout == ExistingTypeLayout::from(input.layout)
        && requested_properties_match(&typ.properties, &input.properties)
}

fn requested_properties_match(actual: &[Property], expected: &[PropertySpec]) -> bool {
    if expected.is_empty() {
        return true;
    }
    let expected_keys = expected
        .iter()
        .map(|property| property.key.as_str())
        .collect::<HashSet<_>>();
    let matched = actual
        .iter()
        .filter(|property| expected_keys.contains(property.key.as_str()))
        .collect::<Vec<_>>();
    matched.len() == expected.len()
        && matched.iter().zip(expected).all(|(actual, expected)| {
            actual.name == expected.name.as_str()
                && actual.key == expected.key.as_str()
                && PropertyFormat::from(actual.format()) == expected.format
        })
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PropertyDefinition {
    id: EntityId,
    key: TypeKey,
    name: DisplayName,
    format: PropertyFormat,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RecommendedDefinition {
    key: TypeKey,
    name: DisplayName,
    format: PropertyFormat,
}

#[derive(Debug, Clone)]
struct CheckedClassification {
    featured_ids: Vec<EntityId>,
    featured: Vec<PropertyDefinition>,
    recommended: Vec<RecommendedDefinition>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClassificationCheckError {
    Oversized,
    Malformed,
}

fn checked_classification(
    classification: &TypePropertyClassification,
) -> Result<CheckedClassification, ClassificationCheckError> {
    if classification.recommended.len() > MAX_PROPERTIES {
        return Err(ClassificationCheckError::Oversized);
    }
    let featured_ids = classification
        .featured_ids
        .iter()
        .cloned()
        .map(EntityId::new)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| ClassificationCheckError::Malformed)?;
    let mut source_ids =
        HashSet::with_capacity(featured_ids.len() + classification.recommended.len());
    if featured_ids
        .iter()
        .any(|id| !source_ids.insert(id.as_str().to_owned()))
    {
        return Err(ClassificationCheckError::Malformed);
    }

    let featured = classification
        .featured
        .iter()
        .map(checked_property_definition)
        .collect::<Result<Vec<_>, _>>()?;
    let positions = featured
        .iter()
        .map(|property| {
            featured_ids
                .iter()
                .position(|id| id == &property.id)
                .ok_or(ClassificationCheckError::Malformed)
        })
        .collect::<Result<Vec<_>, _>>()?;
    if positions
        .windows(2)
        .any(|window| matches!(window, [first, second] if first >= second))
    {
        return Err(ClassificationCheckError::Malformed);
    }

    let mut recommended = Vec::with_capacity(classification.recommended.len());
    for property in &classification.recommended {
        let definition = checked_property_definition(property)?;
        if !source_ids.insert(definition.id.as_str().to_owned()) {
            return Err(ClassificationCheckError::Malformed);
        }
        recommended.push(RecommendedDefinition {
            key: definition.key,
            name: definition.name,
            format: definition.format,
        });
    }
    Ok(CheckedClassification {
        featured_ids,
        featured,
        recommended,
    })
}

fn checked_property_definition(
    property: &Property,
) -> Result<PropertyDefinition, ClassificationCheckError> {
    Ok(PropertyDefinition {
        id: EntityId::new(property.id.clone()).map_err(|_| ClassificationCheckError::Malformed)?,
        key: TypeKey::new(property.key.clone()).map_err(|_| ClassificationCheckError::Malformed)?,
        name: DisplayName::new(property.name.clone())
            .map_err(|_| ClassificationCheckError::Malformed)?,
        format: property.format().into(),
    })
}

fn classification_preflight(error: ClassificationCheckError) -> HandlerOperationError {
    HandlerError::new(match error {
        ClassificationCheckError::Oversized => ToolError::bounded_result(),
        ClassificationCheckError::Malformed => ToolError::upstream(),
    })
    .into()
}

fn update_already_satisfied(
    typ: &Type,
    expected_id: &EntityId,
    input: &TypeUpdateInput,
    classification: Option<&CheckedClassification>,
) -> bool {
    type_matches_update_metadata(typ, expected_id, input)
        && input
            .recommended_properties
            .as_ref()
            .is_none_or(|expected| {
                classification
                    .is_some_and(|actual| recommended_matches(&actual.recommended, &expected.0))
            })
}

fn type_matches_update_metadata(
    typ: &Type,
    expected_id: &EntityId,
    input: &TypeUpdateInput,
) -> bool {
    let Ok(summary) = checked_type_summary(typ, Some(expected_id)) else {
        return false;
    };
    input.name.as_ref().is_none_or(|expected| {
        summary.name.as_ref().map(DisplayName::as_str) == Some(expected.as_str())
    }) && input
        .key
        .as_ref()
        .is_none_or(|expected| summary.key.as_str() == expected.as_str())
        && input.plural_name.as_ref().is_none_or(|expected| {
            summary.plural_name.as_ref().map(DisplayName::as_str) == Some(expected.as_str())
        })
        && input
            .layout
            .as_ref()
            .is_none_or(|expected| summary.layout == ExistingTypeLayout::from(*expected))
}

const fn type_layout_name(layout: TypeLayout) -> &'static str {
    match layout {
        TypeLayout::Basic => "basic",
        TypeLayout::Profile => "profile",
        TypeLayout::Action => "action",
        TypeLayout::Note => "note",
    }
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

fn recommended_matches(actual: &[RecommendedDefinition], expected: &[PropertySpec]) -> bool {
    actual.len() == expected.len()
        && actual.iter().zip(expected).all(|(actual, expected)| {
            actual.name.as_str() == expected.name.as_str()
                && actual.key.as_str() == expected.key.as_str()
                && actual.format == expected.format
        })
}

struct TypeEvidence {
    typ: Type,
    classification: Option<CheckedClassification>,
}

fn evidence_matches_update(
    evidence: &TypeEvidence,
    expected_id: &EntityId,
    input: &TypeUpdateInput,
    baseline: Option<&CheckedClassification>,
) -> bool {
    if !type_matches_update_metadata(&evidence.typ, expected_id, input) {
        return false;
    }
    match (
        input.recommended_properties.as_ref(),
        baseline,
        evidence.classification.as_ref(),
    ) {
        (None, None, None) => true,
        (Some(expected), Some(baseline), Some(actual)) => {
            recommended_matches(&actual.recommended, &expected.0)
                && actual.featured_ids == baseline.featured_ids
                && actual.featured == baseline.featured
        }
        _ => false,
    }
}

fn type_patch_rejection_is_definitive(error: &AnytypeError) -> bool {
    matches!(
        error,
        AnytypeError::Unauthorized | AnytypeError::Forbidden | AnytypeError::NotFound { .. }
    ) || matches!(
        error,
        AnytypeError::ApiError {
            code: 400 | 401 | 403 | 404 | 409 | 422,
            ..
        }
    )
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
    use rmcp::model::ListToolsResult;
    use serde_json::{Map, Value, json};
    use sha2::{Digest, Sha256};
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
    const TYPE_ID: &str = "bafyreiaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaab";
    const FEATURED_ID: &str = "bafyreiaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaac";
    const TOKEN_BUDGET_SNAPSHOT: &str =
        include_str!("../tests/snapshots/schema-type-token-budget.json");

    struct TestRegistry {
        handlers: SchemaTypeHandlers,
    }

    impl fmt::Debug for TestRegistry {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("TestSchemaTypeRegistry")
        }
    }

    impl OptionalToolsetRegistry for TestRegistry {
        fn metadata(&self) -> OptionalToolsetMetadata {
            OptionalToolsetMetadata::new("schema", true)
        }

        fn tools(&self) -> Result<Vec<OptionalRegistryTool>, SchemaContractError> {
            schema_type_tools()
        }

        fn scripted_scenario_ids(&self) -> &'static [&'static str] {
            &["schema_type_direct", "schema_type_stdio"]
        }

        fn headless_scenario_ids(&self) -> &'static [&'static str] {
            &["schema_type_headless"]
        }

        fn catalog_token_ceiling(&self) -> usize {
            9_500
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

    fn property(id: &str, name: &str, key: &str, format: &str) -> Value {
        json!({"id":id,"name":name,"key":key,"format":format})
    }

    fn api_property(id: &str, name: &str, key: &str, format: &str) -> Property {
        serde_json::from_value(property(id, name, key, format)).expect("property fixture")
    }

    fn classification(recommended: &[(&str, &str, &str, &str)]) -> TypePropertyClassification {
        TypePropertyClassification {
            featured_ids: vec![FEATURED_ID.to_owned()],
            featured: vec![api_property(FEATURED_ID, "Created", "created_date", "date")],
            recommended: recommended
                .iter()
                .map(|(id, name, key, format)| api_property(id, name, key, format))
                .collect(),
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

    fn server(
        client: AnytypeClient,
        read_only: bool,
        handlers: SchemaTypeHandlers,
    ) -> AnyMcpServer {
        let registry: &'static TestRegistry = Box::leak(Box::new(TestRegistry { handlers }));
        let registries: &'static [&'static dyn OptionalToolsetRegistry] =
            Box::leak(vec![registry as &dyn OptionalToolsetRegistry].into_boxed_slice());
        AnyMcpServer::new_with_optional_registries(runtime(client, read_only), registries)
            .expect("schema-type test server")
    }

    fn snapshot_client() -> AnytypeClient {
        let client = AnytypeClient::with_config(ClientConfig {
            base_url: Some("http://127.0.0.1:1".to_owned()),
            keystore: Some("env".to_owned()),
            keystore_service: Some("schema-type-snapshot".to_owned()),
            app_name: "schema-type-snapshot".to_owned(),
            disable_cache: true,
            ..ClientConfig::default()
        })
        .expect("schema-type snapshot client");
        client.set_api_key(HttpCredentials::new("snapshot-token"));
        client
    }

    fn snapshot_server(
        profile: ApplicationProfile,
        read_only: bool,
        selected: Option<&str>,
    ) -> AnyMcpServer {
        let registry: &'static TestRegistry = Box::leak(Box::new(TestRegistry {
            handlers: SchemaTypeHandlers::new().expect("snapshot handlers"),
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
            .expect("schema-type snapshot server")
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
        serde_json::to_value(ListToolsResult::with_all_items(server.tools().to_vec()))
            .expect("tools list value")
    }

    fn maximum_type_result() -> CallToolResult {
        let output = TypeOutput {
            type_summary: TypeSummary {
                id: EntityId::new("a".repeat(256)).expect("maximum entity id"),
                key: TypeKey::new("a".repeat(256)).expect("maximum type key"),
                name: Some(DisplayName::new("界".repeat(512)).expect("maximum type name")),
                plural_name: Some(
                    DisplayName::new("語".repeat(512)).expect("maximum plural type name"),
                ),
                layout: ExistingTypeLayout::Collection,
                archived: true,
            },
        };
        type_get_tool()
            .expect("type get contract")
            .success(&output)
            .expect("maximum type result")
    }

    fn schema_type_token_budget() -> Value {
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
            .filter(|tool| matches!(tool.name.as_ref(), TYPE_GET | TYPE_CREATE | TYPE_UPDATE))
            .map(|tool| {
                (
                    tool.name.to_string(),
                    token_count(&tokenizer, serde_json::to_value(tool).expect("tool value")),
                )
            })
            .collect::<BTreeMap<_, _>>();
        let maximum_properties = (0..20)
            .map(|index| {
                json!({
                    "name":"界".repeat(512),
                    "key":format!("p{index}_{}", "a".repeat(250)),
                    "format":"objects"
                })
            })
            .collect::<Vec<_>>();
        let maximum_input = json!({
            "space":"a".repeat(256),
            "type":"b".repeat(256),
            "name":"界".repeat(512),
            "key":"a".repeat(256),
            "plural_name":"語".repeat(512),
            "layout":"participant",
            "recommended_properties":maximum_properties
        });
        let maximum_result =
            serde_json::to_value(maximum_type_result()).expect("maximum type result value");
        let maximum_result_sha256 = Sha256::digest(canonical_compact(maximum_result.clone()))
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        json!({
            "tokenizer":"tiktoken o200k_base (tiktoken-rs 0.12.0)",
            "base_catalog_sha256":base_hash,
            "base_catalog_tokens":token_count(&tokenizer, base_value),
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
            "adversarial_twenty_item_input_tokens":token_count(&tokenizer, maximum_input),
            "representative_max_result_tokens":token_count(&tokenizer, maximum_result.clone()),
            "representative_max_result_sha256":maximum_result_sha256
        })
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
            .expect("schema-type direct dispatch")
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
                    "io.modelcontextprotocol/clientInfo":{"name":"schema-type-test","version":"1"},
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

    #[derive(Clone, Copy, Debug)]
    enum Transport {
        Direct,
        Stdio,
    }

    async fn transport_call(
        router: &AnyMcpServer,
        client: &AnytypeClient,
        handlers: &SchemaTypeHandlers,
        transport: Transport,
        name: &'static str,
        arguments: Value,
    ) -> CallToolResult {
        transport_call_with_access(router, client, handlers, false, transport, name, arguments)
            .await
    }

    async fn transport_call_with_access(
        router: &AnyMcpServer,
        client: &AnytypeClient,
        handlers: &SchemaTypeHandlers,
        read_only: bool,
        transport: Transport,
        name: &'static str,
        arguments: Value,
    ) -> CallToolResult {
        match transport {
            Transport::Direct => direct(router, name, arguments).await,
            Transport::Stdio => {
                let response = preview_stdio_call(
                    server(client.clone(), read_only, handlers.clone()),
                    name,
                    arguments,
                )
                .await;
                assert!(
                    response.get("error").is_none(),
                    "stdio call failed: {response}"
                );
                serde_json::from_value(response["result"].clone())
                    .expect("decode stdio call result")
            }
        }
    }

    #[derive(Debug, Clone, Copy)]
    struct WorkMeasurement {
        http_logical: u64,
        http_physical: u64,
        grpc_show: u64,
        grpc_close: u64,
        grpc_close_fallbacks: u64,
        grpc_cleanup_successes: u64,
        grpc_cleanup_failures: u64,
    }

    impl WorkMeasurement {
        fn combined_logical(self) -> u64 {
            self.http_logical
                .saturating_add(self.grpc_show)
                .saturating_add(self.grpc_close)
        }

        fn combined_physical(self) -> u64 {
            self.http_physical
                .saturating_add(self.grpc_show)
                .saturating_add(self.grpc_close)
        }
    }

    async fn measured_call(
        router: &AnyMcpServer,
        client: &AnytypeClient,
        handlers: &SchemaTypeHandlers,
        transport: Transport,
        name: &'static str,
        arguments: Value,
    ) -> (CallToolResult, WorkMeasurement) {
        let before_http = client.http_metrics();
        let before_grpc = client.type_property_classification_metrics();
        let result = transport_call(router, client, handlers, transport, name, arguments).await;
        let after_http = client.http_metrics();
        let after_grpc = client.type_property_classification_metrics();
        (
            result,
            WorkMeasurement {
                http_logical: after_http
                    .logical_operations
                    .saturating_sub(before_http.logical_operations),
                http_physical: after_http
                    .physical_attempts
                    .saturating_sub(before_http.physical_attempts),
                grpc_show: after_grpc
                    .show_attempts
                    .saturating_sub(before_grpc.show_attempts),
                grpc_close: after_grpc
                    .close_attempts
                    .saturating_sub(before_grpc.close_attempts),
                grpc_close_fallbacks: after_grpc
                    .close_fallbacks
                    .saturating_sub(before_grpc.close_fallbacks),
                grpc_cleanup_successes: after_grpc
                    .cleanup_successes
                    .saturating_sub(before_grpc.cleanup_successes),
                grpc_cleanup_failures: after_grpc
                    .cleanup_failures
                    .saturating_sub(before_grpc.cleanup_failures),
            },
        )
    }

    fn assert_success(result: &CallToolResult) -> &Value {
        assert_eq!(
            result.is_error,
            Some(false),
            "unexpected tool result: {:?}",
            result.structured_content
        );
        let value = result.structured_content.as_ref().expect("typed result");
        let type_value = value.get("type").expect("type result");
        let fields = type_value
            .as_object()
            .expect("type result object")
            .keys()
            .map(String::as_str)
            .collect::<HashSet<_>>();
        assert_eq!(
            fields,
            HashSet::from(["id", "key", "name", "plural_name", "layout", "archived"])
        );
        assert!(type_value.get("properties").is_none());
        value
    }

    fn assert_tool_error(result: &CallToolResult, code: &str) -> Value {
        assert_eq!(
            result.is_error,
            Some(true),
            "unexpected tool result: {result:?}"
        );
        let value = result
            .structured_content
            .as_ref()
            .expect("typed tool error");
        assert_eq!(value["code"], code, "unexpected tool result: {result:?}");
        value.clone()
    }

    fn assert_work_within(
        work: WorkMeasurement,
        logical_ceiling: usize,
        physical_ceiling: usize,
        minimum_classifications: u64,
    ) {
        assert!(work.http_logical <= logical_ceiling as u64, "{work:?}");
        assert!(work.http_physical <= physical_ceiling as u64, "{work:?}");
        assert!(work.grpc_show >= minimum_classifications, "{work:?}");
        assert!(
            work.grpc_show <= 11,
            "verification exceeded one preflight plus ten readbacks: {work:?}"
        );
        assert_eq!(work.grpc_close, work.grpc_show, "{work:?}");
        assert_eq!(work.grpc_close_fallbacks, 0, "{work:?}");
        assert_eq!(work.grpc_cleanup_successes, work.grpc_close, "{work:?}");
        assert_eq!(work.grpc_cleanup_failures, 0, "{work:?}");
    }

    fn assert_classification_work_exact(
        work: WorkMeasurement,
        http_logical: u64,
        http_physical: u64,
        grpc_show: u64,
        grpc_close: u64,
    ) {
        assert_eq!(work.http_logical, http_logical, "{work:?}");
        assert_eq!(work.http_physical, http_physical, "{work:?}");
        assert_eq!(work.grpc_show, grpc_show, "{work:?}");
        assert_eq!(work.grpc_close, grpc_close, "{work:?}");
        assert_eq!(work.grpc_close_fallbacks, 0, "{work:?}");
        assert_eq!(work.grpc_cleanup_successes, grpc_close, "{work:?}");
        assert_eq!(work.grpc_cleanup_failures, 0, "{work:?}");
    }

    async fn assert_rejected_before_io(
        router: &AnyMcpServer,
        client: &AnytypeClient,
        handlers: &SchemaTypeHandlers,
        transport: Transport,
        name: &'static str,
        arguments: Value,
    ) {
        let before_http = client.http_metrics();
        let before_grpc = client.type_property_classification_metrics();
        match transport {
            Transport::Direct => {
                let error = router
                    .dispatch_tool(
                        CallToolRequestParams::new(name).with_arguments(args(arguments)),
                        &CancellationToken::new(),
                    )
                    .await
                    .expect_err("direct decoder must reject oversized input");
                assert_eq!(error.code.0, -32602);
            }
            Transport::Stdio => {
                let response = preview_stdio_call(
                    server(client.clone(), false, handlers.clone()),
                    name,
                    arguments,
                )
                .await;
                assert_eq!(response["error"]["code"], -32602);
            }
        }
        let after_http = client.http_metrics();
        assert_eq!(
            after_http.logical_operations,
            before_http.logical_operations
        );
        assert_eq!(after_http.physical_attempts, before_http.physical_attempts);
        assert_eq!(client.type_property_classification_metrics(), before_grpc);
    }

    async fn assert_direct_stdio_error_parity(
        router: &AnyMcpServer,
        client: &AnytypeClient,
        handlers: &SchemaTypeHandlers,
        name: &'static str,
        arguments: Value,
        code: &str,
    ) {
        let before_grpc = client.type_property_classification_metrics();
        let direct_result = transport_call(
            router,
            client,
            handlers,
            Transport::Direct,
            name,
            arguments.clone(),
        )
        .await;
        let direct_error = assert_tool_error(&direct_result, code);
        let stdio_result =
            transport_call(router, client, handlers, Transport::Stdio, name, arguments).await;
        let stdio_error = assert_tool_error(&stdio_result, code);
        assert_eq!(stdio_error, direct_error);
        assert_eq!(client.type_property_classification_metrics(), before_grpc);
    }

    async fn assert_cancelled_before_patch(
        client: &AnytypeClient,
        space_id: &str,
        type_id: &str,
        original_name: &str,
        transport: Transport,
    ) {
        let reached = Arc::new(AtomicBool::new(false));
        let hook_reached = Arc::clone(&reached);
        let handlers =
            SchemaTypeHandlers::build(DEFAULT_IDEMPOTENCY_CAPACITY, VerifyConfig::default(), None)
                .expect("cancellation handlers")
                .with_before_patch_hook(Arc::new(move |cancellation| {
                    hook_reached.store(true, Ordering::SeqCst);
                    cancellation.cancel();
                }));
        let input = json!({
            "space":space_id,
            "type":type_id,
            "name":format!("must not patch {original_name}")
        });
        let before_http = client.http_metrics();
        let before_grpc = client.type_property_classification_metrics();
        match transport {
            Transport::Direct => {
                let router = server(client.clone(), false, handlers);
                let token = CancellationToken::new();
                let result = router
                    .dispatch_tool(
                        CallToolRequestParams::new(TYPE_UPDATE).with_arguments(args(input)),
                        &token,
                    )
                    .await
                    .expect("cancelled direct dispatch");
                assert_eq!(
                    result
                        .structured_content
                        .as_ref()
                        .and_then(|value| value.get("code"))
                        .and_then(Value::as_str),
                    Some("upstream")
                );
            }
            Transport::Stdio => {
                let response = preview_stdio_exchange(
                    server(client.clone(), false, handlers),
                    TYPE_UPDATE,
                    input,
                )
                .await;
                if let Some(response) = response {
                    assert_eq!(response["result"]["structuredContent"]["code"], "upstream");
                }
            }
        }
        assert!(reached.load(Ordering::SeqCst));
        let after_http = client.http_metrics();
        assert_eq!(
            after_http
                .logical_operations
                .saturating_sub(before_http.logical_operations),
            1
        );
        assert_eq!(
            after_http
                .physical_attempts
                .saturating_sub(before_http.physical_attempts),
            1
        );
        assert_eq!(client.type_property_classification_metrics(), before_grpc);
        let unchanged = client
            .get_type(space_id, type_id)
            .get_direct()
            .await
            .expect("cancelled update readback");
        assert_eq!(unchanged.name.as_deref(), Some(original_name));
    }

    fn featured_signature(
        classification: &TypePropertyClassification,
    ) -> Vec<(String, String, String, ApiPropertyFormat)> {
        classification
            .featured
            .iter()
            .map(|property| {
                (
                    property.id.clone(),
                    property.key.clone(),
                    property.name.clone(),
                    property.format(),
                )
            })
            .collect()
    }

    async fn exercise_live_transport(
        router: &AnyMcpServer,
        client: &AnytypeClient,
        handlers: &SchemaTypeHandlers,
        space_id: &str,
        suffix: &str,
        transport: Transport,
    ) -> anytype::Result<(String, String)> {
        let transport_key = match transport {
            Transport::Direct => "direct",
            Transport::Stdio => "stdio",
        };
        let type_key = format!("mcp_{transport_key}_{suffix}");
        let first_key = format!("first_{transport_key}_{suffix}");
        let second_key = format!("second_{transport_key}_{suffix}");
        let create_input = json!({
            "space":space_id,
            "name":format!("MCP {transport_key}"),
            "key":type_key,
            "layout":"basic",
            "properties":[
                {"name":"First","key":first_key,"format":"text"},
                {"name":"Second","key":second_key,"format":"number"}
            ],
            "idempotency_key":format!("mcp-{transport_key}-{suffix}")
        });
        let (created, create_work) = measured_call(
            router,
            client,
            handlers,
            transport,
            TYPE_CREATE,
            create_input.clone(),
        )
        .await;
        let created_wire = assert_success(&created).clone();
        assert_work_within(
            create_work,
            TYPE_CREATE_LOGICAL_CEILING,
            TYPE_CREATE_PHYSICAL_CEILING,
            0,
        );
        assert_eq!(create_work.grpc_show, 0);
        let id = created_wire["type"]["id"]
            .as_str()
            .expect("created type id")
            .to_owned();

        let (cached, cached_work) = measured_call(
            router,
            client,
            handlers,
            transport,
            TYPE_CREATE,
            create_input,
        )
        .await;
        assert_eq!(cached.structured_content, created.structured_content);
        assert_eq!(cached_work.http_logical, 0, "{cached_work:?}");
        assert_eq!(cached_work.http_physical, 0, "{cached_work:?}");
        assert_eq!(cached_work.grpc_show, 0, "{cached_work:?}");

        let (got, get_work) = measured_call(
            router,
            client,
            handlers,
            transport,
            TYPE_GET,
            json!({"space":space_id,"type":id}),
        )
        .await;
        assert_eq!(assert_success(&got), &created_wire);
        assert_eq!(get_work.http_logical, 1, "{get_work:?}");
        assert_work_within(
            get_work,
            TYPE_GET_LOGICAL_CEILING,
            TYPE_GET_PHYSICAL_CEILING,
            0,
        );
        assert_eq!(get_work.grpc_show, 0);

        let initial = client.get_type(space_id, &id).classify_properties().await?;
        let featured_ids = initial.featured_ids.clone();
        let featured = featured_signature(&initial);
        assert_eq!(
            initial
                .replaceable()
                .iter()
                .map(|property| property.key.as_str())
                .collect::<Vec<_>>(),
            vec![first_key.as_str(), second_key.as_str()]
        );

        let (no_op, no_op_work) = measured_call(
            router,
            client,
            handlers,
            transport,
            TYPE_UPDATE,
            json!({
                "space":space_id,"type":id,
                "recommended_properties":[
                    {"name":"First","key":first_key,"format":"text"},
                    {"name":"Second","key":second_key,"format":"number"}
                ]
            }),
        )
        .await;
        assert_success(&no_op);
        assert_classification_work_exact(no_op_work, 2, 2, 1, 1);
        assert_work_within(
            no_op_work,
            TYPE_UPDATE_NOOP_HTTP_LOGICAL_CEILING,
            TYPE_UPDATE_NOOP_HTTP_PHYSICAL_CEILING,
            1,
        );

        let (preserved, preserve_work) = measured_call(
            router,
            client,
            handlers,
            transport,
            TYPE_UPDATE,
            json!({
                "space":space_id,"type":id,
                "plural_name":format!("MCP {transport_key} types")
            }),
        )
        .await;
        assert_success(&preserved);
        assert_eq!(preserve_work.http_logical, 3, "{preserve_work:?}");
        assert_eq!(preserve_work.http_physical, 3, "{preserve_work:?}");
        assert_eq!(preserve_work.grpc_show, 0, "{preserve_work:?}");
        assert_work_within(
            preserve_work,
            TYPE_UPDATE_METADATA_LOGICAL_CEILING,
            TYPE_UPDATE_METADATA_PHYSICAL_CEILING,
            0,
        );
        let after_preserve = client.get_type(space_id, &id).classify_properties().await?;
        assert_eq!(
            after_preserve
                .replaceable()
                .iter()
                .map(|property| property.key.as_str())
                .collect::<Vec<_>>(),
            vec![first_key.as_str(), second_key.as_str()]
        );

        let replace_a = format!("replace_a_{transport_key}_{suffix}");
        let replace_b = format!("replace_b_{transport_key}_{suffix}");
        let replacement_name = format!("MCP {transport_key} replaced");
        let replacement_plural_name = format!("MCP {transport_key} replaced types");
        let (replaced, replace_work) = measured_call(
            router,
            client,
            handlers,
            transport,
            TYPE_UPDATE,
            json!({
                "space":space_id,"type":id,
                "name":replacement_name,
                "plural_name":replacement_plural_name,
                "recommended_properties":[
                    {"name":"Replace A","key":replace_a,"format":"checkbox"},
                    {"name":"Replace B","key":replace_b,"format":"date"}
                ]
            }),
        )
        .await;
        let replaced_output = assert_success(&replaced);
        assert_eq!(replaced_output["type"]["name"], replacement_name);
        assert_eq!(
            replaced_output["type"]["plural_name"],
            replacement_plural_name
        );
        assert_classification_work_exact(replace_work, 5, 5, 2, 2);
        assert_work_within(
            replace_work,
            TYPE_UPDATE_RECOMMENDATION_HTTP_LOGICAL_CEILING,
            TYPE_UPDATE_RECOMMENDATION_HTTP_PHYSICAL_CEILING,
            2,
        );
        let after_replace = client.get_type(space_id, &id).classify_properties().await?;
        let after_replace_type = client.get_type(space_id, &id).get_direct().await?;
        assert_eq!(
            after_replace_type.name.as_deref(),
            Some(replacement_name.as_str())
        );
        assert_eq!(
            after_replace_type.plural_name.as_deref(),
            Some(replacement_plural_name.as_str())
        );
        assert_eq!(
            after_replace
                .replaceable()
                .iter()
                .map(|property| property.key.as_str())
                .collect::<Vec<_>>(),
            vec![replace_a.as_str(), replace_b.as_str()]
        );
        assert_eq!(after_replace.featured_ids, featured_ids);
        assert_eq!(featured_signature(&after_replace), featured);

        let (cleared, clear_work) = measured_call(
            router,
            client,
            handlers,
            transport,
            TYPE_UPDATE,
            json!({"space":space_id,"type":id,"recommended_properties":[]}),
        )
        .await;
        assert_success(&cleared);
        assert_classification_work_exact(clear_work, 5, 5, 2, 2);
        assert_work_within(
            clear_work,
            TYPE_UPDATE_RECOMMENDATION_HTTP_LOGICAL_CEILING,
            TYPE_UPDATE_RECOMMENDATION_HTTP_PHYSICAL_CEILING,
            2,
        );
        let after_clear = client.get_type(space_id, &id).classify_properties().await?;
        assert!(after_clear.replaceable().is_empty());
        assert_eq!(after_clear.featured_ids, featured_ids);
        assert_eq!(featured_signature(&after_clear), featured);

        let twenty = (0..20)
            .map(|index| {
                json!({
                    "name":format!("Update Twenty {index}"),
                    "key":format!("update_twenty_{transport_key}_{index}_{suffix}"),
                    "format":"text"
                })
            })
            .collect::<Vec<_>>();
        let (twenty_result, twenty_work) = measured_call(
            router,
            client,
            handlers,
            transport,
            TYPE_UPDATE,
            json!({"space":space_id,"type":id,"recommended_properties":twenty}),
        )
        .await;
        assert_success(&twenty_result);
        assert_classification_work_exact(twenty_work, 5, 5, 2, 2);
        assert_work_within(
            twenty_work,
            TYPE_UPDATE_RECOMMENDATION_HTTP_LOGICAL_CEILING,
            TYPE_UPDATE_RECOMMENDATION_HTTP_PHYSICAL_CEILING,
            2,
        );
        let after_twenty = client.get_type(space_id, &id).classify_properties().await?;
        assert_eq!(after_twenty.replaceable().len(), 20);
        assert_eq!(after_twenty.featured_ids, featured_ids);
        assert_eq!(featured_signature(&after_twenty), featured);

        let twenty_one = (0..21)
            .map(|index| {
                json!({
                    "name":format!("Rejected {index}"),
                    "key":format!("rejected_{transport_key}_{index}_{suffix}"),
                    "format":"text"
                })
            })
            .collect::<Vec<_>>();
        assert_rejected_before_io(
            router,
            client,
            handlers,
            transport,
            TYPE_UPDATE,
            json!({"space":space_id,"type":id,"recommended_properties":twenty_one}),
        )
        .await;
        Ok((id, replacement_name))
    }

    #[test]
    fn exact_contracts_reject_null_unknown_duplicate_and_twenty_first_values() {
        let get = type_get_tool().expect("type get contract");
        let create = type_create_tool().expect("type create contract");
        let update = type_update_tool().expect("type update contract");
        assert_eq!(
            get.as_tool()
                .annotations
                .as_ref()
                .and_then(|a| a.read_only_hint),
            Some(true)
        );
        assert_eq!(
            create
                .as_tool()
                .annotations
                .as_ref()
                .and_then(|a| a.idempotent_hint),
            Some(false)
        );
        assert_eq!(
            update
                .as_tool()
                .annotations
                .as_ref()
                .and_then(|a| a.idempotent_hint),
            Some(false)
        );
        for tool in [get.as_tool(), create.as_tool(), update.as_tool()] {
            let schema = serde_json::to_value(tool.input_schema.as_ref())
                .expect("input schema")
                .to_string();
            assert!(schema.contains("additionalProperties\":false"));
        }
        assert!(
            serde_json::from_value::<TypeCreateInput>(json!({
                "space":SPACE_ID,"name":"T","properties":null
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<TypeCreateInput>(json!({
                "space":SPACE_ID,"name":"T","unknown":true
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<TypeUpdateInput>(json!({
                "space":SPACE_ID,"type":TYPE_ID,"recommended_properties":[
                    {"name":"A","key":"same","format":"text"},
                    {"name":"B","key":"same","format":"number"}
                ]
            }))
            .is_err()
        );
        let twenty_one = (0..21)
            .map(|index| json!({"name":format!("P {index}"),"key":format!("p_{index}"),"format":"text"}))
            .collect::<Vec<_>>();
        assert!(
            serde_json::from_value::<TypeUpdateInput>(json!({
                "space":SPACE_ID,"type":TYPE_ID,"recommended_properties":twenty_one
            }))
            .is_err()
        );
        assert_eq!(schema_type_tools().expect("type slice").len(), 3);
    }

    #[tokio::test]
    async fn read_only_catalog_and_dispatch_gate_mutations_before_decode_or_io() {
        let client = snapshot_client();
        let handlers = SchemaTypeHandlers::new().expect("read-only handlers");
        let router = server(client.clone(), true, handlers.clone());
        let names = router
            .tools()
            .iter()
            .map(|tool| tool.name.as_ref())
            .collect::<HashSet<_>>();
        assert!(names.contains(TYPE_GET));
        assert!(!names.contains(TYPE_CREATE));
        assert!(!names.contains(TYPE_UPDATE));

        for transport in [Transport::Direct, Transport::Stdio] {
            let before_http = client.http_metrics();
            let before_grpc = client.type_property_classification_metrics();
            let result = transport_call_with_access(
                &router,
                &client,
                &handlers,
                true,
                transport,
                TYPE_UPDATE,
                json!({"unknown":"must not decode"}),
            )
            .await;
            let error = assert_tool_error(&result, "validation");
            assert_eq!(
                error["message"],
                "This Anytype server is read-only. Mutating workflows are disabled."
            );
            assert_eq!(client.http_metrics(), before_http);
            assert_eq!(client.type_property_classification_metrics(), before_grpc);
        }
    }

    #[test]
    fn classification_bounds_and_patch_rejection_classes_are_deterministic() {
        let mut oversized = classification(&[]);
        oversized.recommended = (0..21)
            .map(|index| {
                api_property(
                    &format!("property-{index}"),
                    &format!("Property {index}"),
                    &format!("property_{index}"),
                    "text",
                )
            })
            .collect();
        assert!(matches!(
            checked_classification(&oversized),
            Err(ClassificationCheckError::Oversized)
        ));
        for error in [
            AnytypeError::Unauthorized,
            AnytypeError::Forbidden,
            AnytypeError::NotFound {
                obj_type: "type".to_owned(),
                key: "redacted".to_owned(),
            },
            AnytypeError::ApiError {
                code: 409,
                method: "patch".to_owned(),
                url: "/redacted".to_owned(),
                message: "redacted".to_owned(),
            },
        ] {
            assert!(type_patch_rejection_is_definitive(&error));
        }
        assert!(!type_patch_rejection_is_definitive(
            &AnytypeError::RateLimitExceeded {
                header: "redacted".to_owned(),
                duration: Duration::ZERO,
            }
        ));
    }

    #[test]
    fn schema_type_catalog_input_and_result_match_reviewed_token_snapshot() {
        let actual = canonical_json(schema_type_token_budget());
        let reviewed = canonical_json(
            serde_json::from_str(TOKEN_BUDGET_SNAPSHOT).expect("schema-type token snapshot"),
        );
        assert_eq!(actual, reviewed, "schema-type token budget drifted");
        let domain_tokens = actual["per_tool_tokens"]
            .as_object()
            .expect("per-tool token object")
            .values()
            .map(|value| value.as_u64().expect("token count") as usize)
            .sum::<usize>();
        assert!(domain_tokens <= 9_500);
        let added = actual["compact_composed_total_tokens"]
            .as_u64()
            .expect("composed tokens")
            .saturating_sub(actual["base_catalog_tokens"].as_u64().expect("base tokens"));
        assert!(added <= 10_000);
        assert_eq!(
            actual["adversarial_twenty_item_input_tokens"],
            reviewed["adversarial_twenty_item_input_tokens"]
        );
        assert_eq!(
            actual["representative_max_result_sha256"],
            reviewed["representative_max_result_sha256"]
        );
    }

    #[test]
    #[ignore = "requires env-only disposable credentials and an authenticated headless Anytype server"]
    fn headless_type_preserve_replace_clear_and_featured_stability() {
        std::thread::Builder::new()
            .name("schema-type-live".to_owned())
            .stack_size(16 * 1024 * 1024)
            .spawn(|| {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("schema-type live runtime");
                runtime.block_on(async {
                    let outcome = Box::pin(with_disposable_space_context(
                        "any-mcp-schema-type",
                        |ctx| {
                            Box::pin(async move {
                                ctx.client.ping_http().await.expect("authenticated HTTP");
                                ctx.client.ping_grpc().await.expect("authenticated gRPC");
                                let suffix = unique_suffix().replace('-', "_");
                                let observer_ctx = ctx.clone();
                                let handlers = SchemaTypeHandlers::build(
                                    DEFAULT_IDEMPOTENCY_CAPACITY,
                                    VerifyConfig::default(),
                                    Some(Arc::new(move |typ| {
                                        observer_ctx.register_type(&typ.id);
                                        Ok(())
                                    })),
                                )
                                .expect("live schema-type handlers");
                                let server = server(ctx.client.clone(), false, handlers.clone());

                                let (direct_id, direct_name) = exercise_live_transport(
                                    &server,
                                    &ctx.client,
                                    &handlers,
                                    &ctx.space_id,
                                    &suffix,
                                    Transport::Direct,
                                )
                                .await?;
                                let (stdio_id, stdio_name) = exercise_live_transport(
                                    &server,
                                    &ctx.client,
                                    &handlers,
                                    &ctx.space_id,
                                    &suffix,
                                    Transport::Stdio,
                                )
                                .await?;

                                let read_only_server =
                                    self::server(ctx.client.clone(), true, handlers.clone());
                                let read_only_names = read_only_server
                                    .tools()
                                    .iter()
                                    .map(|tool| tool.name.as_ref())
                                    .collect::<HashSet<_>>();
                                assert!(read_only_names.contains(TYPE_GET));
                                assert!(!read_only_names.contains(TYPE_CREATE));
                                assert!(!read_only_names.contains(TYPE_UPDATE));
                                for transport in [Transport::Direct, Transport::Stdio] {
                                    let result = transport_call_with_access(
                                        &read_only_server,
                                        &ctx.client,
                                        &handlers,
                                        true,
                                        transport,
                                        TYPE_GET,
                                        json!({"space":ctx.space_id,"type":direct_id}),
                                    )
                                    .await;
                                    assert_success(&result);
                                }

                                let duplicate_name = format!("MCP type ambiguity {suffix}");
                                let first_ambiguous = ctx
                                    .client
                                    .new_type(&ctx.space_id, &duplicate_name)
                                    .key(format!("ambiguous_a_{suffix}"))
                                    .ensure_available()
                                    .create()
                                    .await?;
                                ctx.register_type(&first_ambiguous.id);
                                let second_ambiguous = ctx
                                    .client
                                    .new_type(&ctx.space_id, &duplicate_name)
                                    .key(format!("ambiguous_b_{suffix}"))
                                    .ensure_available()
                                    .create()
                                    .await?;
                                ctx.register_type(&second_ambiguous.id);
                                assert_direct_stdio_error_parity(
                                    &server,
                                    &ctx.client,
                                    &handlers,
                                    TYPE_GET,
                                    json!({"space":ctx.space_id,"type":duplicate_name}),
                                    "ambiguous",
                                )
                                .await;

                                assert_direct_stdio_error_parity(
                                    &server,
                                    &ctx.client,
                                    &handlers,
                                    TYPE_GET,
                                    json!({"space":SPACE_ID,"type":direct_id}),
                                    "upstream",
                                )
                                .await;

                                let page = ctx
                                    .client
                                    .new_object(&ctx.space_id, "page")
                                    .name(format!("MCP wrong type layout {suffix}"))
                                    .create()
                                    .await?;
                                ctx.register_object(&page.id);
                                assert_direct_stdio_error_parity(
                                    &server,
                                    &ctx.client,
                                    &handlers,
                                    TYPE_GET,
                                    json!({"space":ctx.space_id,"type":page.id}),
                                    "upstream",
                                )
                                .await;

                                let bad_auth_client =
                                    AnytypeClient::with_config(ctx.client.get_config().clone())?;
                                bad_auth_client.set_api_key(HttpCredentials::new(format!(
                                    "invalid-schema-type-{suffix}"
                                )));
                                let bad_auth_handlers = SchemaTypeHandlers::new()
                                    .expect("bad-auth schema type handlers");
                                let bad_auth_server = self::server(
                                    bad_auth_client.clone(),
                                    false,
                                    bad_auth_handlers.clone(),
                                );
                                assert_direct_stdio_error_parity(
                                    &bad_auth_server,
                                    &bad_auth_client,
                                    &bad_auth_handlers,
                                    TYPE_GET,
                                    json!({"space":ctx.space_id,"type":direct_id}),
                                    "authentication",
                                )
                                .await;

                                assert_cancelled_before_patch(
                                    &ctx.client,
                                    &ctx.space_id,
                                    &direct_id,
                                    &direct_name,
                                    Transport::Direct,
                                )
                                .await;
                                assert_cancelled_before_patch(
                                    &ctx.client,
                                    &ctx.space_id,
                                    &stdio_id,
                                    &stdio_name,
                                    Transport::Stdio,
                                )
                                .await;

                                for transport in [Transport::Direct, Transport::Stdio] {
                                    let label = match transport {
                                        Transport::Direct => "direct",
                                        Transport::Stdio => "stdio",
                                    };
                                    let twenty = (0..20)
                                .map(|index| {
                                    json!({
                                        "name":format!("Create Twenty {index}"),
                                        "key":format!("create_twenty_{label}_{index}_{suffix}"),
                                        "format":"text"
                                    })
                                })
                                .collect::<Vec<_>>();
                                    let input = json!({
                                        "space":ctx.space_id,
                                        "name":format!("Twenty {label}"),
                                        "key":format!("twenty_{label}_{suffix}"),
                                        "properties":twenty,
                                        "idempotency_key":format!("twenty-{label}-{suffix}")
                                    });
                                    let (created, work) = measured_call(
                                        &server,
                                        &ctx.client,
                                        &handlers,
                                        transport,
                                        TYPE_CREATE,
                                        input,
                                    )
                                    .await;
                                    let value = assert_success(&created);
                                    assert_work_within(
                                        work,
                                        TYPE_CREATE_LOGICAL_CEILING,
                                        TYPE_CREATE_PHYSICAL_CEILING,
                                        0,
                                    );
                                    assert_eq!(work.grpc_show, 0);
                                    let id = value["type"]["id"]
                                        .as_str()
                                        .expect("twenty-property type id");
                                    let classified = ctx
                                        .client
                                        .get_type(&ctx.space_id, id)
                                        .classify_properties()
                                        .await?;
                                    assert_eq!(classified.replaceable().len(), 20);
                                    assert_eq!(
                                        classified
                                            .replaceable()
                                            .iter()
                                            .map(|property| property.key.as_str())
                                            .collect::<Vec<_>>(),
                                        (0..20)
                                            .map(|index| {
                                                format!("create_twenty_{label}_{index}_{suffix}")
                                            })
                                            .collect::<Vec<_>>()
                                            .iter()
                                            .map(String::as_str)
                                            .collect::<Vec<_>>()
                                    );

                                    let twenty_one = (0..21)
                                .map(|index| {
                                    json!({
                                        "name":format!("Rejected Create {index}"),
                                        "key":format!("rejected_create_{label}_{index}_{suffix}"),
                                        "format":"text"
                                    })
                                })
                                .collect::<Vec<_>>();
                                    assert_rejected_before_io(
                                        &server,
                                        &ctx.client,
                                        &handlers,
                                        transport,
                                        TYPE_CREATE,
                                        json!({
                                            "space":ctx.space_id,
                                            "name":"Rejected",
                                            "properties":twenty_one
                                        }),
                                    )
                                    .await;
                                }

                                let mut cancelled_after_show = false;
                                let mut after_cancel_metrics = None;
                                for _ in 0..10 {
                                    let before = ctx.client.type_property_classification_metrics();
                                    let cancel_client = ctx.client.clone();
                                    let cancel_space = ctx.space_id.clone();
                                    let cancel_type = direct_id.clone();
                                    let task = tokio::spawn(async move {
                                        cancel_client
                                            .get_type(&cancel_space, &cancel_type)
                                            .classify_properties()
                                            .await
                                    });
                                    while ctx
                                        .client
                                        .type_property_classification_metrics()
                                        .show_attempts
                                        == before.show_attempts
                                        && !task.is_finished()
                                    {
                                        tokio::task::yield_now().await;
                                    }
                                    if !task.is_finished() {
                                        task.abort();
                                        let joined = task
                                            .await
                                            .expect_err("classification task cancellation");
                                        assert!(joined.is_cancelled());
                                        tokio::time::timeout(Duration::from_secs(5), async {
                                            loop {
                                                let current = ctx
                                                    .client
                                                    .type_property_classification_metrics();
                                                if current.close_fallbacks > before.close_fallbacks
                                                    && current.cleanup_successes
                                                        > before.cleanup_successes
                                                {
                                                    break;
                                                }
                                                tokio::task::yield_now().await;
                                            }
                                        })
                                        .await
                                        .expect("detached classification cleanup fallback");
                                        let after_cancel =
                                            ctx.client.type_property_classification_metrics();
                                        assert_eq!(
                                            after_cancel.show_attempts - before.show_attempts,
                                            1
                                        );
                                        assert_eq!(
                                            after_cancel.close_attempts - before.close_attempts,
                                            1
                                        );
                                        assert_eq!(
                                            after_cancel.close_fallbacks - before.close_fallbacks,
                                            1
                                        );
                                        assert_eq!(
                                            after_cancel.cleanup_successes
                                                - before.cleanup_successes,
                                            1
                                        );
                                        assert_eq!(
                                            after_cancel.cleanup_failures - before.cleanup_failures,
                                            0
                                        );
                                        after_cancel_metrics = Some(after_cancel);
                                        cancelled_after_show = true;
                                        break;
                                    }
                                    let _ = task.await;
                                }
                                assert!(
                                    cancelled_after_show,
                                    "could not observe a live classification after Show dispatch"
                                );
                                let after_cancel =
                                    after_cancel_metrics.expect("cancelled classification metrics");
                                let reclassified = ctx
                                    .client
                                    .get_type(&ctx.space_id, &direct_id)
                                    .classify_properties()
                                    .await?;
                                assert_eq!(reclassified.replaceable().len(), 20);
                                let after_reclassify =
                                    ctx.client.type_property_classification_metrics();
                                assert_eq!(
                                    after_reclassify.show_attempts - after_cancel.show_attempts,
                                    1
                                );
                                assert_eq!(
                                    after_reclassify.close_attempts - after_cancel.close_attempts,
                                    1
                                );
                                assert_eq!(
                                    after_reclassify.close_fallbacks - after_cancel.close_fallbacks,
                                    0
                                );
                                assert_eq!(
                                    after_reclassify.cleanup_successes
                                        - after_cancel.cleanup_successes,
                                    1
                                );
                                assert_eq!(
                                    after_reclassify.cleanup_failures
                                        - after_cancel.cleanup_failures,
                                    0
                                );
                                Ok(())
                            })
                        },
                    ))
                    .await
                    .expect("cleanup-safe live schema-type workflow");
                    match outcome {
                        DisposableRun::Completed(()) => {}
                        DisposableRun::Skipped(reason) => {
                            eprintln!(
                                "disposable schema-type suite skipped before callback: {reason:?}"
                            );
                        }
                    }
                });
            })
            .expect("spawn schema-type live thread")
            .join()
            .expect("schema-type live thread");
    }

    #[test]
    fn reviewed_work_ceilings_are_locked() {
        assert_eq!(
            (TYPE_GET_LOGICAL_CEILING, TYPE_GET_PHYSICAL_CEILING),
            (23, 138)
        );
        assert_eq!(
            (TYPE_CREATE_LOGICAL_CEILING, TYPE_CREATE_PHYSICAL_CEILING),
            (22, 127)
        );
        assert_eq!(
            (
                TYPE_UPDATE_METADATA_LOGICAL_CEILING,
                TYPE_UPDATE_METADATA_PHYSICAL_CEILING
            ),
            (34, 199)
        );
        assert_eq!(
            (
                TYPE_UPDATE_NOOP_LOGICAL_CEILING,
                TYPE_UPDATE_NOOP_PHYSICAL_CEILING
            ),
            (26, 146)
        );
        assert_eq!(
            (
                TYPE_UPDATE_NOOP_HTTP_LOGICAL_CEILING,
                TYPE_UPDATE_NOOP_HTTP_PHYSICAL_CEILING
            ),
            (24, 144)
        );
        assert_eq!(
            (
                TYPE_UPDATE_RECOMMENDATION_LOGICAL_CEILING,
                TYPE_UPDATE_RECOMMENDATION_PHYSICAL_CEILING
            ),
            (67, 287)
        );
        assert_eq!(
            (
                TYPE_UPDATE_RECOMMENDATION_HTTP_LOGICAL_CEILING,
                TYPE_UPDATE_RECOMMENDATION_HTTP_PHYSICAL_CEILING
            ),
            (45, 265)
        );
        assert_eq!(
            (
                TYPE_UPDATE_FALLBACK_LOGICAL_CEILING,
                TYPE_UPDATE_FALLBACK_PHYSICAL_CEILING
            ),
            (68, 288)
        );

        let no_op = WorkMeasurement {
            http_logical: TYPE_UPDATE_NOOP_HTTP_LOGICAL_CEILING as u64,
            http_physical: TYPE_UPDATE_NOOP_HTTP_PHYSICAL_CEILING as u64,
            grpc_show: 1,
            grpc_close: 1,
            grpc_close_fallbacks: 0,
            grpc_cleanup_successes: 1,
            grpc_cleanup_failures: 0,
        };
        assert_eq!(no_op.combined_logical(), 26);
        assert_eq!(no_op.combined_physical(), 146);

        let complete_write = WorkMeasurement {
            http_logical: TYPE_UPDATE_RECOMMENDATION_HTTP_LOGICAL_CEILING as u64,
            http_physical: TYPE_UPDATE_RECOMMENDATION_HTTP_PHYSICAL_CEILING as u64,
            grpc_show: 11,
            grpc_close: 11,
            grpc_close_fallbacks: 0,
            grpc_cleanup_successes: 11,
            grpc_cleanup_failures: 0,
        };
        assert_eq!(complete_write.combined_logical(), 67);
        assert_eq!(complete_write.combined_physical(), 287);

        let terminal_fallback = WorkMeasurement {
            grpc_close: 12,
            grpc_close_fallbacks: 1,
            grpc_cleanup_successes: 11,
            grpc_cleanup_failures: 1,
            ..complete_write
        };
        assert_eq!(terminal_fallback.grpc_show, 11);
        assert_eq!(terminal_fallback.grpc_close, 12);
        assert_eq!(terminal_fallback.grpc_close_fallbacks, 1);
        assert_eq!(terminal_fallback.combined_logical(), 68);
        assert_eq!(terminal_fallback.combined_physical(), 288);
    }

    #[test]
    fn production_registry_does_not_link_partial_schema_slice() {
        let names = crate::optional_toolsets::production_optional_registries()
            .iter()
            .map(|registry| registry.metadata().name.to_owned())
            .collect::<Vec<_>>();
        assert!(!names.iter().any(|name| name == "schema"));
    }

    #[test]
    fn observer_seam_is_thread_safe() {
        let observed = Arc::new(AtomicUsize::new(0));
        let counter = observed.clone();
        let handlers = SchemaTypeHandlers::build(
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
