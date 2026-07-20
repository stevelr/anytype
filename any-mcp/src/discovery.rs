// any-mcp - bounded, workflow-oriented MCP server for Anytype
//
// SPDX-FileCopyrightText: 2026 Steve Schoettler
// SPDX-License-Identifier: Apache-2.0

//! Typed, bounded handlers for status and Anytype schema discovery.

use std::{borrow::Cow, collections::HashSet, fmt, sync::Arc};

use anytype::{
    error::AnytypeError,
    objects::{Color, ObjectLayout},
    paged::{PagedResult, PaginatedResponse, PaginationMeta},
    properties::{Property, PropertyFormat},
    spaces::{Space, SpaceModel},
    tags::Tag,
    types::Type,
};
use rmcp::{
    model::CallToolResult,
    schemars::{JsonSchema, Schema, SchemaGenerator, json_schema},
};
use serde::{Deserialize, Deserializer, Serialize, de};
use tokio_util::sync::CancellationToken;
use url::Url;

use crate::{
    cursor::{CursorStore, CursorStoreError, CursorToken},
    domain::{
        BoundedText, DisplayName, DomainValueError, EntityId, ObjectSummary, SpaceId, TypeKey,
    },
    error::ToolError,
    handler_support::{
        HandlerError, PageRequest, UpstreamPagination, begin_page, execute_handler, finish_page,
    },
    object_output::{ProjectedColor, object_summary},
    pagination::{Page, PageLimit},
    protocol::{ToolProfile, WorkflowTool, workflow_tool},
    result::tool_error,
    runtime::{OperationContext, RuntimeContext},
    schema::SchemaContractError,
    validation::BoundedList,
};

const MAX_REFERENCE_CHARS: usize = 512;
const MAX_ENDPOINT_CHARS: usize = 2_048;
const MAX_API_VERSION_CHARS: usize = 64;
const MAX_ENABLED_TOOLSETS: usize = 8;
const MAX_TAG_COUNT: u64 = 1_000_000_000;

type Endpoint = BoundedText<MAX_ENDPOINT_CHARS>;
type ApiVersion = BoundedText<MAX_API_VERSION_CHARS>;
type EnabledToolsets = BoundedList<EnabledToolset, MAX_ENABLED_TOOLSETS>;

/// A nonempty, bounded space, type, or property reference supplied by a caller.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct DiscoveryReference(String);

impl DiscoveryReference {
    /// Validates a discovery reference without changing its matching semantics.
    pub fn new(value: impl Into<String>) -> Result<Self, ReferenceError> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(ReferenceError::Empty);
        }
        if value.chars().count() > MAX_REFERENCE_CHARS {
            return Err(ReferenceError::TooLong);
        }
        Ok(Self(value))
    }

    /// Borrows the reference exactly as supplied.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for DiscoveryReference {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::new(String::deserialize(deserializer)?).map_err(de::Error::custom)
    }
}

impl JsonSchema for DiscoveryReference {
    fn schema_name() -> Cow<'static, str> {
        "DiscoveryReference".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "minLength": 1,
            "maxLength": MAX_REFERENCE_CHARS,
        })
    }
}

/// Failure to construct a bounded discovery reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReferenceError {
    /// The value was empty or contained only whitespace.
    Empty,
    /// The value exceeded the finite reference bound.
    TooLong,
}

impl fmt::Display for ReferenceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Empty => "reference must not be empty",
            Self::TooLong => "reference exceeds its maximum length",
        })
    }
}

impl std::error::Error for ReferenceError {}

/// Empty input for [`DiscoveryHandlers::server_status`].
#[derive(Debug, Clone, Copy, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ServerStatusInput {}

/// Input for [`DiscoveryHandlers::space_list`].
#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SpaceListInput {
    /// Requested item limit, defaulting to 20.
    #[serde(default)]
    pub limit: PageLimit,
    /// Opaque continuation cursor, when continuing the same request.
    #[serde(default)]
    pub cursor: Option<CursorToken>,
}

/// Input for [`DiscoveryHandlers::type_list`].
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TypeListInput {
    /// Unique space name or identifier.
    pub space: DiscoveryReference,
    /// Requested item limit, defaulting to 20.
    #[serde(default)]
    pub limit: PageLimit,
    /// Opaque continuation cursor, when continuing the same request.
    #[serde(default)]
    pub cursor: Option<CursorToken>,
}

/// Input for [`DiscoveryHandlers::property_list`].
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PropertyListInput {
    /// Unique space name or identifier.
    pub space: DiscoveryReference,
    /// Optional type key, name, or identifier used to scope linked properties.
    #[serde(default, rename = "type")]
    pub type_reference: Option<DiscoveryReference>,
    /// Requested item limit, defaulting to 20.
    #[serde(default)]
    pub limit: PageLimit,
    /// Opaque continuation cursor, when continuing the same request.
    #[serde(default)]
    pub cursor: Option<CursorToken>,
}

/// Input for [`DiscoveryHandlers::tag_list`].
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TagListInput {
    /// Unique space name or identifier.
    pub space: DiscoveryReference,
    /// Select-property key or identifier.
    pub property: DiscoveryReference,
    /// Requested item limit, defaulting to 20.
    #[serde(default)]
    pub limit: PageLimit,
    /// Opaque continuation cursor, when continuing the same request.
    #[serde(default)]
    pub cursor: Option<CursorToken>,
}

/// Input for [`DiscoveryHandlers::template_list`].
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TemplateListInput {
    /// Unique space name or identifier.
    pub space: DiscoveryReference,
    /// Type key, name, or identifier whose templates should be listed.
    #[serde(rename = "type")]
    pub type_reference: DiscoveryReference,
    /// Requested item limit, defaulting to 20.
    #[serde(default)]
    pub limit: PageLimit,
    /// Opaque continuation cursor, when continuing the same request.
    #[serde(default)]
    pub cursor: Option<CursorToken>,
}

/// Redacted startup snapshot returned by `server_status`.
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ServerStatusOutput {
    /// HTTP endpoint with user information, query, and fragment removed.
    endpoint: Endpoint,
    /// Anytype HTTP API revision used by the client.
    api_version: ApiVersion,
    /// Whether the authenticated HTTP startup probe succeeded.
    http_available: bool,
    /// Whether configured gRPC credentials and its startup probe succeeded.
    grpc_available: bool,
    /// Startup-selected MCP toolsets; Phase 1 enables only the default set.
    enabled_toolsets: EnabledToolsets,
}

/// Startup-selected toolset names returned by `server_status`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EnabledToolset {
    /// The bounded Phase 1 document workflow catalog.
    Default,
    /// Optional schema-management workflows.
    Schema,
    /// Optional member discovery workflows.
    Members,
    /// Optional collection and view mutation workflows.
    ViewsWrite,
    /// Optional file workflows.
    Files,
    /// Optional chat workflows.
    Chats,
    /// Optional narrowly scoped administrative workflows.
    Admin,
}

/// Concise Anytype space summary.
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SpaceSummary {
    /// Stable space identifier.
    id: SpaceId,
    /// Bounded display name.
    name: DisplayName,
    /// Closed Anytype space model.
    model: SpaceKind,
}

/// Closed Anytype space models.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SpaceKind {
    /// Standard object workspace.
    Space,
    /// Chat-oriented workspace.
    Chat,
    /// Direct one-to-one workspace.
    OneToOne,
    /// Internal account bookkeeping workspace.
    TechSpace,
}

/// Concise active Anytype type summary.
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TypeSummary {
    /// Stable type identifier.
    id: EntityId,
    /// Stable type key.
    key: TypeKey,
    /// Bounded display name, falling back to the key when absent.
    name: DisplayName,
    /// Closed default object layout.
    layout: TypeLayoutSummary,
}

/// Closed Anytype object layouts used by type discovery.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TypeLayoutSummary {
    /// Standard object layout.
    Basic,
    /// Profile layout.
    Profile,
    /// Action or task layout.
    Action,
    /// Note layout.
    Note,
    /// Bookmark layout.
    Bookmark,
    /// Query-set layout.
    Set,
    /// Collection layout.
    Collection,
    /// Participant layout.
    Participant,
    /// Chat layout.
    Chat,
}

/// Concise property definition without tag option expansion.
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PropertySummary {
    /// Stable property identifier.
    id: EntityId,
    /// Stable property key.
    key: TypeKey,
    /// Bounded display name.
    name: DisplayName,
    /// Closed property format.
    format: PropertyFormatSummary,
    /// Total tag options, obtained from a separate one-item page for select formats.
    tag_count: TagCount,
}

/// Closed Anytype property formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PropertyFormatSummary {
    /// Plain text.
    Text,
    /// Number.
    Number,
    /// Single select.
    Select,
    /// Multiple select.
    MultiSelect,
    /// Date and time.
    Date,
    /// File references.
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

/// Practically bounded number of tags attached to one property.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct TagCount(u64);

impl JsonSchema for TagCount {
    fn schema_name() -> Cow<'static, str> {
        "TagCount".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "integer",
            "minimum": 0,
            "maximum": MAX_TAG_COUNT,
        })
    }
}

/// One bounded tag option for a select property.
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TagSummary {
    /// Stable tag identifier.
    id: EntityId,
    /// Stable tag key.
    key: TypeKey,
    /// Bounded display name.
    name: DisplayName,
    /// Closed Anytype color.
    color: ProjectedColor,
}

/// Constructs the `server_status` contract without registering it.
pub fn server_status_tool() -> Result<WorkflowTool<ServerStatusOutput>, SchemaContractError> {
    workflow_tool::<ServerStatusInput, ServerStatusOutput>(
        "server_status",
        "Inspect the redacted Anytype endpoint, API revision, startup availability, and enabled toolsets. Returns no credentials.",
        ToolProfile::Read,
    )
}

/// Constructs the `space_list` contract without registering it.
pub fn space_list_tool() -> Result<WorkflowTool<Page<SpaceSummary>>, SchemaContractError> {
    workflow_tool::<SpaceListInput, Page<SpaceSummary>>(
        "space_list",
        "List one bounded page of concise Anytype space summaries.",
        ToolProfile::Read,
    )
}

/// Constructs the `type_list` contract without registering it.
pub fn type_list_tool() -> Result<WorkflowTool<Page<TypeSummary>>, SchemaContractError> {
    workflow_tool::<TypeListInput, Page<TypeSummary>>(
        "type_list",
        "List one bounded page of active type identifiers, keys, names, and layouts in a resolved space.",
        ToolProfile::Read,
    )
}

/// Constructs the `property_list` contract without registering it.
pub fn property_list_tool() -> Result<WorkflowTool<Page<PropertySummary>>, SchemaContractError> {
    workflow_tool::<PropertyListInput, Page<PropertySummary>>(
        "property_list",
        "List one bounded page of property definitions and tag counts. Does not return tag options.",
        ToolProfile::Read,
    )
}

/// Constructs the `tag_list` contract without registering it.
pub fn tag_list_tool() -> Result<WorkflowTool<Page<TagSummary>>, SchemaContractError> {
    workflow_tool::<TagListInput, Page<TagSummary>>(
        "tag_list",
        "List one bounded page of tag options for one resolved select property.",
        ToolProfile::Read,
    )
}

/// Constructs the `template_list` contract without registering it.
pub fn template_list_tool() -> Result<WorkflowTool<Page<ObjectSummary>>, SchemaContractError> {
    workflow_tool::<TemplateListInput, Page<ObjectSummary>>(
        "template_list",
        "List one bounded page of template summaries for one resolved type. Returns no template bodies.",
        ToolProfile::Read,
    )
}

/// Transport-neutral discovery handlers sharing one runtime and cursor registry.
#[derive(Clone)]
pub struct DiscoveryHandlers {
    runtime: RuntimeContext,
    cursors: Arc<CursorStore>,
}

impl DiscoveryHandlers {
    /// Creates discovery handlers with the process-wide cursor registry.
    #[must_use]
    pub fn new(runtime: RuntimeContext, cursors: Arc<CursorStore>) -> Self {
        Self { runtime, cursors }
    }

    /// Creates discovery handlers and a fresh process-lifetime cursor registry.
    pub fn with_new_cursor_store(runtime: RuntimeContext) -> Result<Self, CursorStoreError> {
        Ok(Self::new(runtime, Arc::new(CursorStore::new()?)))
    }

    /// Returns the redacted startup snapshot without repeating upstream probes.
    pub async fn server_status(
        &self,
        _input: ServerStatusInput,
        cancellation: &CancellationToken,
    ) -> CallToolResult {
        let Ok(contract) = server_status_tool() else {
            return tool_error(&ToolError::upstream());
        };
        let runtime = self.runtime.clone();
        let status = runtime.startup_status();
        let endpoint = runtime.client().get_http_endpoint().to_owned();
        let api_version = runtime.client().api_version();
        execute_handler(
            &runtime,
            &contract,
            OperationContext::new("server_status"),
            cancellation,
            async move { Ok::<_, AnytypeError>((endpoint, api_version, status)) },
            |(endpoint, api_version, status)| async move {
                Ok(ServerStatusOutput {
                    endpoint: redact_endpoint(&endpoint)?,
                    api_version: ApiVersion::new(api_version).map_err(domain_handler_error)?,
                    http_available: status.http_available,
                    grpc_available: status.grpc_available,
                    enabled_toolsets: EnabledToolsets::new(vec![EnabledToolset::Default])?,
                })
            },
        )
        .await
    }

    /// Lists exactly one upstream page of spaces.
    pub async fn space_list(
        &self,
        input: SpaceListInput,
        cancellation: &CancellationToken,
    ) -> CallToolResult {
        let Ok(contract) = space_list_tool() else {
            return tool_error(&ToolError::upstream());
        };
        let request = match begin_page(
            &self.cursors,
            input.cursor.as_ref(),
            "space_list",
            input.limit,
            &EmptyPageParams {},
        ) {
            Ok(request) => request,
            Err(error) => return tool_error(error.tool_error()),
        };
        let client = self.runtime.client().clone();
        let cursors = self.cursors.clone();
        execute_handler(
            &self.runtime,
            &contract,
            OperationContext::new("space_list"),
            cancellation,
            async move {
                client
                    .spaces()
                    .limit(u32::from(input.limit.get()))
                    .offset(request.offset().get())
                    .list()
                    .await
            },
            move |page| async move {
                finish_api_page(&cursors, request, page, convert_space_summary)
            },
        )
        .await
    }

    /// Resolves a space and lists exactly one upstream page of active types.
    pub async fn type_list(
        &self,
        input: TypeListInput,
        cancellation: &CancellationToken,
    ) -> CallToolResult {
        let Ok(contract) = type_list_tool() else {
            return tool_error(&ToolError::upstream());
        };
        let params = SpacePageParams {
            space: input.space.as_str(),
        };
        let request = match begin_page(
            &self.cursors,
            input.cursor.as_ref(),
            "type_list",
            input.limit,
            &params,
        ) {
            Ok(request) => request,
            Err(error) => return tool_error(error.tool_error()),
        };
        let client = self.runtime.client().clone();
        let cursors = self.cursors.clone();
        execute_handler(
            &self.runtime,
            &contract,
            OperationContext::new("type_list"),
            cancellation,
            async move {
                let space_id = client.resolve_space_id(input.space.as_str()).await?;
                client
                    .types(space_id)
                    .limit(u32::from(input.limit.get()))
                    .offset(request.offset().get())
                    .list()
                    .await
            },
            move |page| async move {
                finish_api_page(&cursors, request, page, |typ| {
                    if typ.archived {
                        Ok(None)
                    } else {
                        convert_type_summary(typ).map(Some)
                    }
                })
            },
        )
        .await
    }

    /// Resolves optional scope and lists exactly one upstream property page.
    ///
    /// Select tag counts use separate `limit=1, offset=0` tag pages and never
    /// call `PropertyRequest::with_tags`, so option values cannot be expanded.
    pub async fn property_list(
        &self,
        input: PropertyListInput,
        cancellation: &CancellationToken,
    ) -> CallToolResult {
        let Ok(contract) = property_list_tool() else {
            return tool_error(&ToolError::upstream());
        };
        let params = PropertyPageParams {
            space: input.space.as_str(),
            type_reference: input
                .type_reference
                .as_ref()
                .map(DiscoveryReference::as_str),
        };
        let request = match begin_page(
            &self.cursors,
            input.cursor.as_ref(),
            "property_list",
            input.limit,
            &params,
        ) {
            Ok(request) => request,
            Err(error) => return tool_error(error.tool_error()),
        };
        let client = self.runtime.client().clone();
        let cursors = self.cursors.clone();
        execute_handler(
            &self.runtime,
            &contract,
            OperationContext::new("property_list"),
            cancellation,
            async move {
                let space_id = client.resolve_space_id(input.space.as_str()).await?;
                let linked_ids = if let Some(type_reference) = input.type_reference {
                    let typ = client
                        .resolve_type(&space_id, type_reference.as_str())
                        .await?;
                    Some(
                        typ.properties
                            .into_iter()
                            .map(|property| property.id)
                            .collect::<HashSet<_>>(),
                    )
                } else {
                    None
                };
                let response = client
                    .properties(&space_id)
                    .limit(u32::from(input.limit.get()))
                    .offset(request.offset().get())
                    .list()
                    .await?
                    .into_response();
                let pagination = response.pagination;
                let mut items = Vec::with_capacity(response.items.len());
                for property in response.items {
                    if linked_ids
                        .as_ref()
                        .is_some_and(|ids| !ids.contains(&property.id))
                    {
                        continue;
                    }
                    let tag_page = if matches!(
                        property.format(),
                        PropertyFormat::Select | PropertyFormat::MultiSelect
                    ) {
                        Some(
                            client
                                .tags(&space_id, &property.id)
                                .limit(1)
                                .offset(0)
                                .list()
                                .await?
                                .into_response(),
                        )
                    } else {
                        None
                    };
                    items.push(PropertyPageItem { property, tag_page });
                }
                Ok(PropertyPageSource { items, pagination })
            },
            move |page| async move { finish_property_page(&cursors, request, page) },
        )
        .await
    }

    /// Resolves one property and lists exactly one upstream tag page.
    pub async fn tag_list(
        &self,
        input: TagListInput,
        cancellation: &CancellationToken,
    ) -> CallToolResult {
        let Ok(contract) = tag_list_tool() else {
            return tool_error(&ToolError::upstream());
        };
        let params = TagPageParams {
            space: input.space.as_str(),
            property: input.property.as_str(),
        };
        let request = match begin_page(
            &self.cursors,
            input.cursor.as_ref(),
            "tag_list",
            input.limit,
            &params,
        ) {
            Ok(request) => request,
            Err(error) => return tool_error(error.tool_error()),
        };
        let client = self.runtime.client().clone();
        let cursors = self.cursors.clone();
        execute_handler(
            &self.runtime,
            &contract,
            OperationContext::new("tag_list"),
            cancellation,
            async move {
                let space_id = client.resolve_space_id(input.space.as_str()).await?;
                let property_id = client
                    .resolve_property_id(&space_id, input.property.as_str())
                    .await?;
                let property = client
                    .property(&space_id, &property_id)
                    .get_direct()
                    .await?;
                if !matches!(
                    property.format(),
                    PropertyFormat::Select | PropertyFormat::MultiSelect
                ) {
                    return Err(AnytypeError::Validation {
                        message: "tag_list requires a select or multi-select property".to_owned(),
                    });
                }
                client
                    .tags(space_id, property_id)
                    .limit(u32::from(input.limit.get()))
                    .offset(request.offset().get())
                    .list()
                    .await
            },
            move |page| async move {
                finish_api_page(&cursors, request, page, |tag| {
                    convert_tag_summary(tag).map(Some)
                })
            },
        )
        .await
    }

    /// Resolves a space and type and lists exactly one upstream template page.
    pub async fn template_list(
        &self,
        input: TemplateListInput,
        cancellation: &CancellationToken,
    ) -> CallToolResult {
        let Ok(contract) = template_list_tool() else {
            return tool_error(&ToolError::upstream());
        };
        let params = TemplatePageParams {
            space: input.space.as_str(),
            type_reference: input.type_reference.as_str(),
        };
        let request = match begin_page(
            &self.cursors,
            input.cursor.as_ref(),
            "template_list",
            input.limit,
            &params,
        ) {
            Ok(request) => request,
            Err(error) => return tool_error(error.tool_error()),
        };
        let client = self.runtime.client().clone();
        let cursors = self.cursors.clone();
        execute_handler(
            &self.runtime,
            &contract,
            OperationContext::new("template_list"),
            cancellation,
            async move {
                let space_id = client.resolve_space_id(input.space.as_str()).await?;
                let type_id = client
                    .resolve_type_id(&space_id, input.type_reference.as_str())
                    .await?;
                client
                    .templates(space_id, type_id)
                    .limit(u32::from(input.limit.get()))
                    .offset(request.offset().get())
                    .list()
                    .await
            },
            move |page| async move {
                finish_api_page(&cursors, request, page, |template| {
                    object_summary(template)
                        .map(Some)
                        .map_err(HandlerError::from)
                })
            },
        )
        .await
    }
}

#[derive(Serialize)]
struct EmptyPageParams {}

#[derive(Serialize)]
struct SpacePageParams<'a> {
    space: &'a str,
}

#[derive(Serialize)]
struct PropertyPageParams<'a> {
    space: &'a str,
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    type_reference: Option<&'a str>,
}

#[derive(Serialize)]
struct TagPageParams<'a> {
    space: &'a str,
    property: &'a str,
}

#[derive(Serialize)]
struct TemplatePageParams<'a> {
    space: &'a str,
    #[serde(rename = "type")]
    type_reference: &'a str,
}

struct PropertyPageSource {
    items: Vec<PropertyPageItem>,
    pagination: PaginationMeta,
}

struct PropertyPageItem {
    property: Property,
    tag_page: Option<PaginatedResponse<Tag>>,
}

fn finish_api_page<T, O>(
    cursors: &CursorStore,
    request: PageRequest,
    page: PagedResult<T>,
    convert: impl Fn(&T) -> Result<Option<O>, HandlerError>,
) -> Result<Page<O>, HandlerError>
where
    O: JsonSchema,
{
    let upstream = UpstreamPagination::try_from(&page.pagination)?;
    let items = page
        .items
        .iter()
        .map(convert)
        .filter_map(Result::transpose)
        .collect::<Result<Vec<_>, _>>()?;
    finish_page(cursors, request, upstream, items)
}

fn finish_property_page(
    cursors: &CursorStore,
    request: PageRequest,
    page: PropertyPageSource,
) -> Result<Page<PropertySummary>, HandlerError> {
    let upstream = UpstreamPagination::try_from(&page.pagination)?;
    let items = page
        .items
        .into_iter()
        .map(convert_property_summary)
        .collect::<Result<Vec<_>, _>>()?;
    finish_page(cursors, request, upstream, items)
}

fn convert_space_summary(space: &Space) -> Result<Option<SpaceSummary>, HandlerError> {
    Ok(Some(SpaceSummary {
        id: SpaceId::new(space.id.clone()).map_err(domain_handler_error)?,
        name: DisplayName::new(space.name.clone()).map_err(domain_handler_error)?,
        model: match space.object {
            SpaceModel::Space => SpaceKind::Space,
            SpaceModel::Chat => SpaceKind::Chat,
            SpaceModel::OneToOne => SpaceKind::OneToOne,
            SpaceModel::TechSpace => SpaceKind::TechSpace,
        },
    }))
}

fn convert_type_summary(typ: &Type) -> Result<TypeSummary, HandlerError> {
    let key = TypeKey::new(typ.key.clone()).map_err(domain_handler_error)?;
    Ok(TypeSummary {
        id: EntityId::new(typ.id.clone()).map_err(domain_handler_error)?,
        name: DisplayName::new(typ.name.clone().unwrap_or_else(|| typ.key.clone()))
            .map_err(domain_handler_error)?,
        key,
        layout: match typ.layout {
            ObjectLayout::Basic => TypeLayoutSummary::Basic,
            ObjectLayout::Profile => TypeLayoutSummary::Profile,
            ObjectLayout::Action => TypeLayoutSummary::Action,
            ObjectLayout::Note => TypeLayoutSummary::Note,
            ObjectLayout::Bookmark => TypeLayoutSummary::Bookmark,
            ObjectLayout::Set => TypeLayoutSummary::Set,
            ObjectLayout::Collection => TypeLayoutSummary::Collection,
            ObjectLayout::Participant => TypeLayoutSummary::Participant,
            ObjectLayout::Chat => TypeLayoutSummary::Chat,
        },
    })
}

fn convert_property_summary(item: PropertyPageItem) -> Result<PropertySummary, HandlerError> {
    let format = property_format(item.property.format());
    let tag_count = match item.property.format() {
        PropertyFormat::Select | PropertyFormat::MultiSelect => {
            let page = item
                .tag_page
                .ok_or_else(|| HandlerError::new(ToolError::upstream()))?;
            checked_tag_count(&page)?
        }
        _ => {
            if item.tag_page.is_some() {
                return Err(HandlerError::new(ToolError::upstream()));
            }
            TagCount(0)
        }
    };
    Ok(PropertySummary {
        id: EntityId::new(item.property.id).map_err(domain_handler_error)?,
        key: TypeKey::new(item.property.key).map_err(domain_handler_error)?,
        name: DisplayName::new(item.property.name).map_err(domain_handler_error)?,
        format,
        tag_count,
    })
}

fn checked_tag_count(page: &PaginatedResponse<Tag>) -> Result<TagCount, HandlerError> {
    if page.pagination.offset != 0 || page.pagination.limit != 1 || page.items.len() > 1 {
        return Err(HandlerError::new(ToolError::upstream()));
    }
    let total = u64::try_from(page.pagination.total)
        .map_err(|_| HandlerError::new(ToolError::bounded_result()))?;
    if total > MAX_TAG_COUNT {
        return Err(HandlerError::new(ToolError::bounded_result()));
    }
    let page_is_consistent = match total {
        0 => page.items.is_empty() && !page.pagination.has_more,
        1 => page.items.len() == 1 && !page.pagination.has_more,
        _ => page.items.len() == 1 && page.pagination.has_more,
    };
    if !page_is_consistent {
        return Err(HandlerError::new(ToolError::upstream()));
    }
    Ok(TagCount(total))
}

fn property_format(format: PropertyFormat) -> PropertyFormatSummary {
    match format {
        PropertyFormat::Text => PropertyFormatSummary::Text,
        PropertyFormat::Number => PropertyFormatSummary::Number,
        PropertyFormat::Select => PropertyFormatSummary::Select,
        PropertyFormat::MultiSelect => PropertyFormatSummary::MultiSelect,
        PropertyFormat::Date => PropertyFormatSummary::Date,
        PropertyFormat::Files => PropertyFormatSummary::Files,
        PropertyFormat::Checkbox => PropertyFormatSummary::Checkbox,
        PropertyFormat::Url => PropertyFormatSummary::Url,
        PropertyFormat::Email => PropertyFormatSummary::Email,
        PropertyFormat::Phone => PropertyFormatSummary::Phone,
        PropertyFormat::Objects => PropertyFormatSummary::Objects,
    }
}

fn convert_tag_summary(tag: &Tag) -> Result<TagSummary, HandlerError> {
    Ok(TagSummary {
        id: EntityId::new(tag.id.clone()).map_err(domain_handler_error)?,
        key: TypeKey::new(tag.key.clone()).map_err(domain_handler_error)?,
        name: DisplayName::new(tag.name.clone()).map_err(domain_handler_error)?,
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

fn redact_endpoint(raw: &str) -> Result<Endpoint, HandlerError> {
    let mut endpoint = Url::parse(raw).map_err(|_| HandlerError::new(ToolError::upstream()))?;
    if !matches!(endpoint.scheme(), "http" | "https") || endpoint.host_str().is_none() {
        return Err(HandlerError::new(ToolError::upstream()));
    }
    endpoint
        .set_username("")
        .map_err(|_| HandlerError::new(ToolError::upstream()))?;
    endpoint
        .set_password(None)
        .map_err(|_| HandlerError::new(ToolError::upstream()))?;
    endpoint.set_query(None);
    endpoint.set_fragment(None);
    Endpoint::new(endpoint.to_string()).map_err(domain_handler_error)
}

fn domain_handler_error(error: DomainValueError) -> HandlerError {
    match error {
        DomainValueError::TooLong { .. } => HandlerError::new(ToolError::bounded_result()),
        DomainValueError::Empty | DomainValueError::InvalidIdentifierCharacter => {
            HandlerError::new(ToolError::upstream())
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::{BTreeMap, BTreeSet},
        time::Duration,
    };

    use anytype::{
        objects::Object,
        prelude::{AnytypeClient, ClientConfig, HttpCredentials, ResponseLimits},
    };
    use serde_json::json;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::{TcpListener, TcpStream},
        task::JoinHandle,
        time::timeout,
    };

    use super::*;
    use crate::{
        error::ToolErrorCode,
        handler_support::finish_page,
        runtime::StartupStatus,
        schema::{input_schema, output_schema},
    };

    const SPACE_ID: &str =
        "bafyreid5fvqlnsobih2keakcxjrrlpmly6kf37klzjzen4ibfdgalcdp4y.2tq5w93cr6oe7";
    const ID_A: &str = "bafyreid5fvqlnsobih2keakcxjrrlpmly6kf37klzjzen4ibfdgalcdp4y";
    const ID_B: &str = "bafyreid5fvqlnsobih2keakcxjrrlpmly6kf37klzjzen4ibfdgalcdp4z";
    const ID_C: &str = "bafyreid5fvqlnsobih2keakcxjrrlpmly6kf37klzjzen4ibfdgalcdp4x";
    const ID_D: &str = "bafyreid5fvqlnsobih2keakcxjrrlpmly6kf37klzjzen4ibfdgalcdp4w";

    struct ExpectedRequest {
        path: String,
        query: BTreeMap<String, String>,
        status: u16,
        body: String,
    }

    impl ExpectedRequest {
        fn json(path: impl Into<String>, query: &[(&str, &str)], value: serde_json::Value) -> Self {
            Self {
                path: path.into(),
                query: query
                    .iter()
                    .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
                    .collect(),
                status: 200,
                body: value.to_string(),
            }
        }

        fn error(path: impl Into<String>, status: u16, body: impl Into<String>) -> Self {
            Self {
                path: path.into(),
                query: BTreeMap::new(),
                status,
                body: body.into(),
            }
        }
    }

    struct HttpFixture {
        endpoint: String,
        task: JoinHandle<()>,
    }

    impl HttpFixture {
        async fn start(expected: Vec<ExpectedRequest>) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let endpoint = format!("http://{}", listener.local_addr().unwrap());
            let task = tokio::spawn(async move {
                for expected in expected {
                    let (mut socket, _) = timeout(Duration::from_secs(5), listener.accept())
                        .await
                        .expect("fixture request timed out")
                        .expect("fixture accept");
                    let mut request = Vec::new();
                    loop {
                        let mut chunk = [0_u8; 1024];
                        let read = socket.read(&mut chunk).await.expect("fixture request read");
                        assert!(read > 0, "fixture connection closed before headers");
                        request.extend_from_slice(&chunk[..read]);
                        assert!(request.len() <= 64 * 1024, "fixture headers exceeded bound");
                        if request.windows(4).any(|window| window == b"\r\n\r\n") {
                            break;
                        }
                    }
                    let request = std::str::from_utf8(&request).expect("ASCII fixture request");
                    let first_line = request.lines().next().expect("fixture request line");
                    let mut parts = first_line.split_ascii_whitespace();
                    assert_eq!(parts.next(), Some("GET"));
                    let target = parts.next().expect("fixture request target");
                    assert_eq!(parts.next(), Some("HTTP/1.1"));
                    assert_eq!(parts.next(), None, "extra fixture request-line field");
                    let (path, raw_query) = target
                        .split_once('?')
                        .map_or((target, ""), |(path, query)| (path, query));
                    assert_eq!(path, expected.path, "raw request path");
                    let mut query = BTreeMap::new();
                    for (key, value) in url::form_urlencoded::parse(raw_query.as_bytes()) {
                        let previous = query.insert(key.into_owned(), value.into_owned());
                        assert!(previous.is_none(), "duplicate decoded query key");
                    }
                    assert_eq!(query, expected.query, "query for {}", expected.path);
                    let mut header_names = BTreeSet::new();
                    for header in request.lines().skip(1).take_while(|line| !line.is_empty()) {
                        let (name, _) = header
                            .split_once(':')
                            .expect("fixture header must contain a colon");
                        let name = name.to_ascii_lowercase();
                        assert!(header_names.insert(name), "duplicate fixture header name");
                    }

                    let reason = if expected.status == 200 {
                        "OK"
                    } else {
                        "Error"
                    };
                    let response = format!(
                        "HTTP/1.1 {} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        expected.status,
                        expected.body.len(),
                        expected.body
                    );
                    socket
                        .write_all(response.as_bytes())
                        .await
                        .expect("fixture response write");
                    socket.shutdown().await.expect("fixture response shutdown");
                }
            });
            Self { endpoint, task }
        }

        async fn finish(self) {
            self.task.await.expect("fixture task");
        }

        async fn finish_rejected(self) {
            assert!(
                self.task.await.is_err(),
                "fixture unexpectedly accepted request"
            );
        }
    }

    #[derive(Debug, Default)]
    struct PropertyRouteTraffic {
        requests: usize,
        property_list_pages: usize,
        direct_property_gets: usize,
        tag_list_pages: usize,
    }

    struct PropertyRouteFixture {
        endpoint: String,
        shutdown: tokio::sync::oneshot::Sender<()>,
        task: JoinHandle<PropertyRouteTraffic>,
    }

    impl PropertyRouteFixture {
        async fn start(returned_property_id: &'static str) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let endpoint = format!("http://{}", listener.local_addr().unwrap());
            let (shutdown, mut shutdown_rx) = tokio::sync::oneshot::channel();
            let task = tokio::spawn(async move {
                let mut traffic = PropertyRouteTraffic::default();
                loop {
                    let accepted = tokio::select! {
                        _ = &mut shutdown_rx => break,
                        accepted = listener.accept() => accepted,
                    };
                    let (mut socket, _) = accepted.expect("accept property route fixture");
                    let mut request = Vec::new();
                    loop {
                        let mut chunk = [0_u8; 1024];
                        let read = socket
                            .read(&mut chunk)
                            .await
                            .expect("read property route request");
                        assert!(read > 0, "property route connection closed before headers");
                        request.extend_from_slice(&chunk[..read]);
                        assert!(
                            request.len() <= 64 * 1024,
                            "property route headers too large"
                        );
                        if request.windows(4).any(|window| window == b"\r\n\r\n") {
                            break;
                        }
                    }
                    let request =
                        std::str::from_utf8(&request).expect("ASCII property route request");
                    let request_line = request.lines().next().expect("property route request line");
                    let mut parts = request_line.split_ascii_whitespace();
                    assert_eq!(parts.next(), Some("GET"));
                    let target = parts.next().expect("property route target");
                    assert_eq!(parts.next(), Some("HTTP/1.1"));
                    assert_eq!(
                        parts.next(),
                        None,
                        "extra property route request-line field"
                    );
                    let (path, raw_query) = target
                        .split_once('?')
                        .map_or((target, ""), |(path, query)| (path, query));
                    let mut query = BTreeMap::new();
                    for (key, value) in url::form_urlencoded::parse(raw_query.as_bytes()) {
                        let previous = query.insert(key.into_owned(), value.into_owned());
                        assert!(previous.is_none(), "duplicate property route query key");
                    }
                    let collection_path = format!("/v1/spaces/{SPACE_ID}/properties");
                    let direct_path = format!("{collection_path}/{ID_B}");
                    let tags_path = format!("{direct_path}/tags");
                    let body = if path == collection_path {
                        let page = traffic.property_list_pages;
                        traffic.property_list_pages += 1;
                        if page == 0 {
                            assert_eq!(query, BTreeMap::new(), "first property-list query");
                            paged(
                                vec![property_value(ID_A, "other", "Other", "text")],
                                0,
                                100,
                                101,
                            )
                            .to_string()
                        } else {
                            assert_eq!(
                                query,
                                BTreeMap::from([
                                    ("limit".to_owned(), "100".to_owned()),
                                    ("offset".to_owned(), "100".to_owned()),
                                ]),
                                "continued property-list query"
                            );
                            paged(
                                vec![property_value(ID_B, "status", "Status", "select")],
                                100,
                                100,
                                101,
                            )
                            .to_string()
                        }
                    } else if path == direct_path {
                        assert_eq!(query, BTreeMap::new(), "direct property query");
                        traffic.direct_property_gets += 1;
                        json!({
                            "property": property_value(
                                returned_property_id,
                                "status",
                                "private-property-body-marker",
                                "select"
                            )
                        })
                        .to_string()
                    } else if path == tags_path {
                        assert_eq!(
                            query,
                            BTreeMap::from([("limit".to_owned(), "2".to_owned())]),
                            "bounded MCP tag-list query"
                        );
                        traffic.tag_list_pages += 1;
                        paged(vec![tag_value(ID_A, "open", "Open")], 0, 2, 1).to_string()
                    } else {
                        panic!("unexpected property route: {request_line}");
                    };
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    traffic.requests += 1;
                    socket
                        .write_all(response.as_bytes())
                        .await
                        .expect("write property route response");
                    socket
                        .shutdown()
                        .await
                        .expect("shutdown property route response");
                }
                traffic
            });
            Self {
                endpoint,
                shutdown,
                task,
            }
        }

        async fn finish(self) -> PropertyRouteTraffic {
            self.shutdown.send(()).expect("stop property route fixture");
            self.task.await.expect("property route fixture task")
        }
    }

    fn runtime(endpoint: &str) -> RuntimeContext {
        runtime_with_limits(endpoint, ResponseLimits::default())
    }

    fn runtime_with_cache(endpoint: &str) -> RuntimeContext {
        let client = AnytypeClient::with_config(ClientConfig {
            base_url: Some(endpoint.to_owned()),
            keystore: Some("env".to_owned()),
            keystore_service: Some("discovery-cache-test".to_owned()),
            app_name: "discovery-cache-test".to_owned(),
            disable_cache: false,
            ..ClientConfig::default()
        })
        .unwrap();
        client.set_api_key(HttpCredentials::new("fixture-token"));
        assert!(
            client.cache().is_enabled(),
            "fixture must exercise cache-on behavior"
        );
        RuntimeContext::from_parts(
            client,
            1,
            Duration::from_secs(1),
            StartupStatus {
                http_available: true,
                grpc_available: false,
            },
        )
    }

    fn runtime_with_limits(endpoint: &str, response_limits: ResponseLimits) -> RuntimeContext {
        let client = AnytypeClient::with_config(ClientConfig {
            base_url: Some(endpoint.to_owned()),
            keystore: Some("env".to_owned()),
            keystore_service: Some("discovery-test".to_owned()),
            app_name: "discovery-test".to_owned(),
            disable_cache: true,
            response_limits,
            ..ClientConfig::default()
        })
        .unwrap();
        client.set_api_key(HttpCredentials::new("fixture-token"));
        RuntimeContext::from_parts(
            client,
            1,
            Duration::from_secs(1),
            StartupStatus {
                http_available: true,
                grpc_available: false,
            },
        )
    }

    fn paged(
        items: Vec<serde_json::Value>,
        offset: u32,
        limit: u32,
        total: usize,
    ) -> serde_json::Value {
        json!({
            "data": items,
            "pagination": {
                "offset": offset,
                "limit": limit,
                "total": total,
                "has_more": usize::try_from(offset).unwrap() + items.len() < total
            }
        })
    }

    fn type_value(
        id: &str,
        key: &str,
        name: &str,
        archived: bool,
        properties: Vec<serde_json::Value>,
    ) -> serde_json::Value {
        json!({
            "archived": archived,
            "id": id,
            "key": key,
            "name": name,
            "layout": "basic",
            "properties": properties
        })
    }

    fn property_value(id: &str, key: &str, name: &str, format: &str) -> serde_json::Value {
        json!({
            "id": id,
            "key": key,
            "name": name,
            "format": format,
            "tags": null
        })
    }

    fn tag_value(id: &str, key: &str, name: &str) -> serde_json::Value {
        json!({"id":id,"key":key,"name":name,"color":"blue"})
    }

    fn template_value(id: &str, name: &str, markdown: &str) -> serde_json::Value {
        json!({
            "archived": false,
            "id": id,
            "name": name,
            "space_id": SPACE_ID,
            "type": type_value(ID_A, "page", "Page", false, Vec::new()),
            "properties": [],
            "markdown": markdown
        })
    }

    fn cursor_from(result: &CallToolResult) -> CursorToken {
        serde_json::from_value(result.structured_content.as_ref().unwrap()["next_cursor"].clone())
            .unwrap()
    }

    fn assert_error(result: &CallToolResult, code: &str) {
        assert_eq!(result.is_error, Some(true));
        assert_eq!(result.structured_content.as_ref().unwrap()["code"], code);
    }

    fn property(value: serde_json::Value) -> Property {
        serde_json::from_value(value).unwrap()
    }

    fn sample_tag() -> Tag {
        serde_json::from_value(json!({
            "id":"tag-1",
            "key":"open",
            "name":"Open",
            "color":"blue"
        }))
        .unwrap()
    }

    fn tag_count_page(total: usize, item_count: usize, has_more: bool) -> PaginatedResponse<Tag> {
        PaginatedResponse {
            items: (0..item_count).map(|_| sample_tag()).collect(),
            pagination: PaginationMeta {
                offset: 0,
                limit: 1,
                total,
                has_more,
            },
        }
    }

    #[test]
    fn all_discovery_contracts_are_strict_bounded_read_tools() {
        let tools = [
            server_status_tool().unwrap().into_tool(),
            space_list_tool().unwrap().into_tool(),
            type_list_tool().unwrap().into_tool(),
            property_list_tool().unwrap().into_tool(),
            tag_list_tool().unwrap().into_tool(),
            template_list_tool().unwrap().into_tool(),
        ];
        assert_eq!(
            tools
                .iter()
                .map(|tool| tool.name.as_ref())
                .collect::<Vec<_>>(),
            [
                "server_status",
                "space_list",
                "type_list",
                "property_list",
                "tag_list",
                "template_list"
            ]
        );
        for tool in tools {
            assert_eq!(tool.input_schema["additionalProperties"], false);
            assert_eq!(
                tool.output_schema.as_ref().unwrap()["additionalProperties"],
                false
            );
            let annotations = tool.annotations.unwrap();
            assert_eq!(annotations.read_only_hint, Some(true));
            assert_eq!(annotations.destructive_hint, Some(false));
            assert_eq!(annotations.open_world_hint, Some(false));
        }
        assert!(input_schema::<PropertyListInput>().is_ok());
        assert!(output_schema::<Page<PropertySummary>>().is_ok());
        assert!(output_schema::<Page<ObjectSummary>>().is_ok());
    }

    #[test]
    fn references_and_unknown_input_fields_fail_closed() {
        assert_eq!(
            DiscoveryReference::new("  ").unwrap_err(),
            ReferenceError::Empty
        );
        assert_eq!(
            DiscoveryReference::new("x".repeat(MAX_REFERENCE_CHARS + 1)).unwrap_err(),
            ReferenceError::TooLong
        );
        assert!(
            serde_json::from_value::<TypeListInput>(json!({
                "space":"space-1",
                "unexpected":true
            }))
            .is_err()
        );
        assert!(serde_json::from_value::<SpaceListInput>(json!({"limit":101})).is_err());
    }

    #[test]
    fn endpoint_redaction_removes_all_authority_secrets_and_suffixes() {
        let endpoint = redact_endpoint(
            "https://alice:p%40ss@example.com:8443/private/path?token=secret#credential",
        )
        .unwrap();
        assert_eq!(endpoint.as_str(), "https://example.com:8443/private/path");
        for secret in ["alice", "p%40ss", "token", "secret", "credential"] {
            assert!(!endpoint.as_str().contains(secret));
        }
        assert!(redact_endpoint("file:///tmp/anytype?token=secret").is_err());
        assert!(redact_endpoint("not a url?token=secret").is_err());
    }

    #[tokio::test]
    async fn status_wire_result_is_redacted_and_uses_startup_snapshot() {
        let handlers = DiscoveryHandlers::with_new_cursor_store(runtime(
            "https://alice:secret@example.com:8443/api?token=secret#fragment",
        ))
        .unwrap();
        let result = handlers
            .server_status(ServerStatusInput {}, &CancellationToken::new())
            .await;
        assert_eq!(result.is_error, Some(false));
        let value = result.structured_content.unwrap();
        assert_eq!(value["endpoint"], "https://example.com:8443/api");
        assert_eq!(value["api_version"], anytype::ANYTYPE_API_VERSION);
        assert_eq!(value["http_available"], true);
        assert_eq!(value["grpc_available"], false);
        assert_eq!(value["enabled_toolsets"], json!(["default"]));
        let wire = result.content[0].as_text().unwrap().text.as_str();
        for secret in ["alice", "secret", "token", "fragment"] {
            assert!(!wire.contains(secret));
        }
    }

    #[tokio::test]
    async fn handler_rejects_cursor_reuse_with_changed_params_before_upstream() {
        let cursors = Arc::new(CursorStore::new().unwrap());
        let old_params = SpacePageParams { space: "space-a" };
        let request = begin_page(
            &cursors,
            None,
            "type_list",
            PageLimit::new(20).unwrap(),
            &old_params,
        )
        .unwrap();
        let page = finish_page(
            &cursors,
            request,
            UpstreamPagination::new(0, 20, true).unwrap(),
            Vec::<TypeSummary>::new(),
        )
        .unwrap();
        let handlers = DiscoveryHandlers::new(runtime("http://127.0.0.1:1"), cursors);
        let result = handlers
            .type_list(
                TypeListInput {
                    space: DiscoveryReference::new("space-b").unwrap(),
                    limit: PageLimit::new(20).unwrap(),
                    cursor: page.next_cursor().cloned(),
                },
                &CancellationToken::new(),
            )
            .await;
        assert_eq!(result.is_error, Some(true));
        assert_eq!(result.structured_content.unwrap()["code"], "validation");
    }

    #[tokio::test]
    async fn space_list_uses_exact_single_pages_and_checked_continuation() {
        let fixture = HttpFixture::start(vec![
            ExpectedRequest::json(
                "/v1/spaces",
                &[("limit", "2")],
                paged(
                    vec![
                        json!({"id":SPACE_ID,"name":"Work","object":"space"}),
                        json!({"id":ID_A,"name":"Chat","object":"chat"}),
                    ],
                    0,
                    2,
                    3,
                ),
            ),
            ExpectedRequest::json(
                "/v1/spaces",
                &[("limit", "2"), ("offset", "2")],
                paged(
                    vec![json!({"id":ID_B,"name":"Direct","object":"one_to_one"})],
                    2,
                    2,
                    3,
                ),
            ),
        ])
        .await;
        let handlers =
            DiscoveryHandlers::with_new_cursor_store(runtime(&fixture.endpoint)).unwrap();
        let cancellation = CancellationToken::new();
        let first = handlers
            .space_list(
                SpaceListInput {
                    limit: PageLimit::new(2).unwrap(),
                    cursor: None,
                },
                &cancellation,
            )
            .await;
        assert_eq!(first.is_error, Some(false));
        assert_eq!(
            first.structured_content.as_ref().unwrap()["items"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        let cursor = cursor_from(&first);

        let mismatch = handlers
            .space_list(
                SpaceListInput {
                    limit: PageLimit::new(3).unwrap(),
                    cursor: Some(cursor.clone()),
                },
                &cancellation,
            )
            .await;
        assert_error(&mismatch, "validation");

        let second = handlers
            .space_list(
                SpaceListInput {
                    limit: PageLimit::new(2).unwrap(),
                    cursor: Some(cursor),
                },
                &cancellation,
            )
            .await;
        assert_eq!(second.is_error, Some(false));
        let second_value = second.structured_content.as_ref().unwrap();
        assert_eq!(second_value["items"].as_array().unwrap().len(), 1);
        assert!(second_value.get("next_cursor").is_none());
        fixture.finish().await;
    }

    #[tokio::test]
    async fn type_list_preserves_sparse_archived_windows_across_http_pages() {
        let path = format!("/v1/spaces/{SPACE_ID}/types");
        let fixture = HttpFixture::start(vec![
            ExpectedRequest::json(
                &path,
                &[("limit", "2")],
                paged(
                    vec![
                        type_value(ID_A, "old", "Archived", true, Vec::new()),
                        type_value(ID_B, "page", "Page", false, Vec::new()),
                    ],
                    0,
                    2,
                    4,
                ),
            ),
            ExpectedRequest::json(
                &path,
                &[("limit", "2"), ("offset", "2")],
                paged(
                    vec![
                        type_value(ID_C, "task", "Task", false, Vec::new()),
                        type_value(ID_D, "note", "Note", false, Vec::new()),
                    ],
                    2,
                    2,
                    4,
                ),
            ),
        ])
        .await;
        let handlers =
            DiscoveryHandlers::with_new_cursor_store(runtime(&fixture.endpoint)).unwrap();
        let cancellation = CancellationToken::new();
        let first = handlers
            .type_list(
                TypeListInput {
                    space: DiscoveryReference::new(SPACE_ID).unwrap(),
                    limit: PageLimit::new(2).unwrap(),
                    cursor: None,
                },
                &cancellation,
            )
            .await;
        assert_eq!(first.is_error, Some(false));
        assert_eq!(
            first.structured_content.as_ref().unwrap()["items"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        let cursor = cursor_from(&first);
        let mismatch = handlers
            .type_list(
                TypeListInput {
                    space: DiscoveryReference::new(ID_A).unwrap(),
                    limit: PageLimit::new(2).unwrap(),
                    cursor: Some(cursor.clone()),
                },
                &cancellation,
            )
            .await;
        assert_error(&mismatch, "validation");
        let second = handlers
            .type_list(
                TypeListInput {
                    space: DiscoveryReference::new(SPACE_ID).unwrap(),
                    limit: PageLimit::new(2).unwrap(),
                    cursor: Some(cursor),
                },
                &cancellation,
            )
            .await;
        assert_eq!(second.is_error, Some(false));
        assert_eq!(
            second.structured_content.as_ref().unwrap()["items"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        assert!(
            second
                .structured_content
                .as_ref()
                .unwrap()
                .get("next_cursor")
                .is_none()
        );
        fixture.finish().await;
    }

    #[tokio::test]
    async fn property_list_scopes_sparse_pages_and_counts_tags_with_limit_one() {
        let type_path = format!("/v1/spaces/{SPACE_ID}/types/{ID_A}");
        let properties_path = format!("/v1/spaces/{SPACE_ID}/properties");
        let tags_path = format!("/v1/spaces/{SPACE_ID}/properties/{ID_B}/tags");
        let linked = vec![
            property_value(ID_B, "status", "Status", "select"),
            property_value(ID_D, "owner", "Owner", "objects"),
        ];
        let type_response = json!({"type":type_value(ID_A,"page","Page",false,linked)});
        let fixture = HttpFixture::start(vec![
            ExpectedRequest::json(&type_path, &[], type_response.clone()),
            ExpectedRequest::json(
                &properties_path,
                &[("limit", "2")],
                paged(
                    vec![
                        property_value(ID_B, "status", "Status", "select"),
                        property_value(ID_C, "unlinked", "Unlinked", "text"),
                    ],
                    0,
                    2,
                    3,
                ),
            ),
            ExpectedRequest::json(
                &tags_path,
                &[("limit", "1")],
                paged(vec![tag_value(ID_C, "open", "Open")], 0, 1, 2),
            ),
            ExpectedRequest::json(&type_path, &[], type_response),
            ExpectedRequest::json(
                &properties_path,
                &[("limit", "2"), ("offset", "2")],
                paged(
                    vec![property_value(ID_D, "owner", "Owner", "objects")],
                    2,
                    2,
                    3,
                ),
            ),
        ])
        .await;
        let handlers =
            DiscoveryHandlers::with_new_cursor_store(runtime(&fixture.endpoint)).unwrap();
        let cancellation = CancellationToken::new();
        let first = handlers
            .property_list(
                PropertyListInput {
                    space: DiscoveryReference::new(SPACE_ID).unwrap(),
                    type_reference: Some(DiscoveryReference::new(ID_A).unwrap()),
                    limit: PageLimit::new(2).unwrap(),
                    cursor: None,
                },
                &cancellation,
            )
            .await;
        assert_eq!(first.is_error, Some(false));
        let items = first.structured_content.as_ref().unwrap()["items"]
            .as_array()
            .unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["tag_count"], 2);
        assert!(items[0].get("tags").is_none());
        let cursor = cursor_from(&first);
        let mismatch = handlers
            .property_list(
                PropertyListInput {
                    space: DiscoveryReference::new(SPACE_ID).unwrap(),
                    type_reference: Some(DiscoveryReference::new(ID_B).unwrap()),
                    limit: PageLimit::new(2).unwrap(),
                    cursor: Some(cursor.clone()),
                },
                &cancellation,
            )
            .await;
        assert_error(&mismatch, "validation");
        let second = handlers
            .property_list(
                PropertyListInput {
                    space: DiscoveryReference::new(SPACE_ID).unwrap(),
                    type_reference: Some(DiscoveryReference::new(ID_A).unwrap()),
                    limit: PageLimit::new(2).unwrap(),
                    cursor: Some(cursor),
                },
                &cancellation,
            )
            .await;
        assert_eq!(second.is_error, Some(false));
        let items = second.structured_content.as_ref().unwrap()["items"]
            .as_array()
            .unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["key"], "owner");
        assert_eq!(items[0]["tag_count"], 0);
        assert!(
            second
                .structured_content
                .as_ref()
                .unwrap()
                .get("next_cursor")
                .is_none()
        );
        fixture.finish().await;
    }

    #[tokio::test]
    async fn tag_list_gets_select_property_and_uses_exact_http_pages() {
        let property_path = format!("/v1/spaces/{SPACE_ID}/properties/{ID_B}");
        let tags_path = format!("/v1/spaces/{SPACE_ID}/properties/{ID_B}/tags");
        let property_response = json!({
            "property":property_value(ID_B,"status","Status","select")
        });
        let fixture = HttpFixture::start(vec![
            ExpectedRequest::json(&property_path, &[], property_response.clone()),
            ExpectedRequest::json(
                &tags_path,
                &[("limit", "2")],
                paged(
                    vec![
                        tag_value(ID_A, "open", "Open"),
                        tag_value(ID_C, "closed", "Closed"),
                    ],
                    0,
                    2,
                    3,
                ),
            ),
            ExpectedRequest::json(&property_path, &[], property_response),
            ExpectedRequest::json(
                &tags_path,
                &[("limit", "2"), ("offset", "2")],
                paged(vec![tag_value(ID_D, "blocked", "Blocked")], 2, 2, 3),
            ),
        ])
        .await;
        let handlers =
            DiscoveryHandlers::with_new_cursor_store(runtime(&fixture.endpoint)).unwrap();
        let cancellation = CancellationToken::new();
        let first = handlers
            .tag_list(
                TagListInput {
                    space: DiscoveryReference::new(SPACE_ID).unwrap(),
                    property: DiscoveryReference::new(ID_B).unwrap(),
                    limit: PageLimit::new(2).unwrap(),
                    cursor: None,
                },
                &cancellation,
            )
            .await;
        assert_eq!(first.is_error, Some(false));
        assert_eq!(
            first.structured_content.as_ref().unwrap()["items"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        let cursor = cursor_from(&first);
        let mismatch = handlers
            .tag_list(
                TagListInput {
                    space: DiscoveryReference::new(SPACE_ID).unwrap(),
                    property: DiscoveryReference::new(ID_C).unwrap(),
                    limit: PageLimit::new(2).unwrap(),
                    cursor: Some(cursor.clone()),
                },
                &cancellation,
            )
            .await;
        assert_error(&mismatch, "validation");
        let second = handlers
            .tag_list(
                TagListInput {
                    space: DiscoveryReference::new(SPACE_ID).unwrap(),
                    property: DiscoveryReference::new(ID_B).unwrap(),
                    limit: PageLimit::new(2).unwrap(),
                    cursor: Some(cursor),
                },
                &cancellation,
            )
            .await;
        assert_eq!(second.is_error, Some(false));
        assert_eq!(
            second.structured_content.as_ref().unwrap()["items"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert!(
            second
                .structured_content
                .as_ref()
                .unwrap()
                .get("next_cursor")
                .is_none()
        );
        fixture.finish().await;
    }

    #[tokio::test]
    async fn tag_list_uses_one_direct_property_get_with_cold_cache() {
        let fixture = PropertyRouteFixture::start(ID_B).await;
        let handlers =
            DiscoveryHandlers::with_new_cursor_store(runtime_with_cache(&fixture.endpoint))
                .unwrap();
        let result = handlers
            .tag_list(
                TagListInput {
                    space: DiscoveryReference::new(SPACE_ID).unwrap(),
                    property: DiscoveryReference::new(ID_B).unwrap(),
                    limit: PageLimit::new(2).unwrap(),
                    cursor: None,
                },
                &CancellationToken::new(),
            )
            .await;
        assert_eq!(result.is_error, Some(false));
        assert_eq!(
            result.structured_content.as_ref().unwrap()["items"]
                .as_array()
                .unwrap()
                .len(),
            1
        );

        let traffic = fixture.finish().await;
        assert_eq!(
            traffic.property_list_pages, 0,
            "must not prime property cache"
        );
        assert_eq!(traffic.direct_property_gets, 1);
        assert_eq!(traffic.tag_list_pages, 1);
        assert_eq!(traffic.requests, 2);
    }

    #[tokio::test]
    async fn tag_list_rejects_property_identity_mismatch_before_tag_page() {
        let fixture = PropertyRouteFixture::start(ID_C).await;
        let handlers =
            DiscoveryHandlers::with_new_cursor_store(runtime_with_cache(&fixture.endpoint))
                .unwrap();
        let result = handlers
            .tag_list(
                TagListInput {
                    space: DiscoveryReference::new(SPACE_ID).unwrap(),
                    property: DiscoveryReference::new(ID_B).unwrap(),
                    limit: PageLimit::new(2).unwrap(),
                    cursor: None,
                },
                &CancellationToken::new(),
            )
            .await;
        assert_error(&result, "upstream");
        let encoded = serde_json::to_string(&result).unwrap();
        for private in [SPACE_ID, ID_B, ID_C, "private-property-body-marker"] {
            assert!(!encoded.contains(private), "tool error leaked fixture data");
        }

        let traffic = fixture.finish().await;
        assert_eq!(traffic.property_list_pages, 0);
        assert_eq!(traffic.direct_property_gets, 1);
        assert_eq!(
            traffic.tag_list_pages, 0,
            "mismatch must stop before tag list"
        );
        assert_eq!(traffic.requests, 1);
    }

    #[tokio::test]
    async fn template_list_returns_summary_only_across_exact_http_pages() {
        let path = format!("/v1/spaces/{SPACE_ID}/types/{ID_A}/templates");
        let fixture = HttpFixture::start(vec![
            ExpectedRequest::json(
                &path,
                &[("limit", "1")],
                paged(
                    vec![template_value(ID_B, "Meeting", "private body one")],
                    0,
                    1,
                    2,
                ),
            ),
            ExpectedRequest::json(
                &path,
                &[("limit", "1"), ("offset", "1")],
                paged(
                    vec![template_value(ID_C, "Review", "private body two")],
                    1,
                    1,
                    2,
                ),
            ),
        ])
        .await;
        let handlers =
            DiscoveryHandlers::with_new_cursor_store(runtime(&fixture.endpoint)).unwrap();
        let cancellation = CancellationToken::new();
        let first = handlers
            .template_list(
                TemplateListInput {
                    space: DiscoveryReference::new(SPACE_ID).unwrap(),
                    type_reference: DiscoveryReference::new(ID_A).unwrap(),
                    limit: PageLimit::new(1).unwrap(),
                    cursor: None,
                },
                &cancellation,
            )
            .await;
        assert_eq!(first.is_error, Some(false));
        let first_wire = first.content[0].as_text().unwrap().text.as_str();
        assert!(!first_wire.contains("private body one"));
        assert!(!first_wire.contains("markdown"));
        let cursor = cursor_from(&first);
        let mismatch = handlers
            .template_list(
                TemplateListInput {
                    space: DiscoveryReference::new(SPACE_ID).unwrap(),
                    type_reference: DiscoveryReference::new(ID_B).unwrap(),
                    limit: PageLimit::new(1).unwrap(),
                    cursor: Some(cursor.clone()),
                },
                &cancellation,
            )
            .await;
        assert_error(&mismatch, "validation");
        let second = handlers
            .template_list(
                TemplateListInput {
                    space: DiscoveryReference::new(SPACE_ID).unwrap(),
                    type_reference: DiscoveryReference::new(ID_A).unwrap(),
                    limit: PageLimit::new(1).unwrap(),
                    cursor: Some(cursor),
                },
                &cancellation,
            )
            .await;
        assert_eq!(second.is_error, Some(false));
        let second_wire = second.content[0].as_text().unwrap().text.as_str();
        assert!(!second_wire.contains("private body two"));
        assert!(!second_wire.contains("markdown"));
        assert!(
            second
                .structured_content
                .as_ref()
                .unwrap()
                .get("next_cursor")
                .is_none()
        );
        fixture.finish().await;
    }

    #[tokio::test]
    async fn resolver_ambiguity_and_not_found_are_classified_from_real_http() {
        let ambiguous_fixture = HttpFixture::start(vec![ExpectedRequest::json(
            "/v1/spaces",
            &[("limit", "99")],
            paged(
                vec![
                    json!({"id":ID_A,"name":"Work","object":"space"}),
                    json!({"id":ID_B,"name":"Work","object":"space"}),
                ],
                0,
                99,
                2,
            ),
        )])
        .await;
        let handlers =
            DiscoveryHandlers::with_new_cursor_store(runtime(&ambiguous_fixture.endpoint)).unwrap();
        let result = handlers
            .type_list(
                TypeListInput {
                    space: DiscoveryReference::new("Work").unwrap(),
                    limit: PageLimit::new(2).unwrap(),
                    cursor: None,
                },
                &CancellationToken::new(),
            )
            .await;
        assert_error(&result, "ambiguous");
        let candidates = result.structured_content.as_ref().unwrap()["candidates"]
            .as_array()
            .unwrap();
        assert_eq!(candidates.len(), 2);
        assert_eq!(candidates[0]["id"], ID_A);
        assert_eq!(candidates[1]["id"], ID_B);
        ambiguous_fixture.finish().await;

        let missing_fixture = HttpFixture::start(vec![ExpectedRequest::json(
            "/v1/spaces",
            &[("limit", "99")],
            paged(Vec::new(), 0, 99, 0),
        )])
        .await;
        let handlers =
            DiscoveryHandlers::with_new_cursor_store(runtime(&missing_fixture.endpoint)).unwrap();
        let result = handlers
            .type_list(
                TypeListInput {
                    space: DiscoveryReference::new("Missing").unwrap(),
                    limit: PageLimit::new(2).unwrap(),
                    cursor: None,
                },
                &CancellationToken::new(),
            )
            .await;
        assert_error(&result, "not_found");
        missing_fixture.finish().await;
    }

    #[tokio::test]
    async fn inconsistent_http_tag_count_fails_as_secret_safe_upstream() {
        let properties_path = format!("/v1/spaces/{SPACE_ID}/properties");
        let tags_path = format!("/v1/spaces/{SPACE_ID}/properties/{ID_B}/tags");
        let fixture = HttpFixture::start(vec![
            ExpectedRequest::json(
                &properties_path,
                &[("limit", "1")],
                paged(
                    vec![property_value(ID_B, "status", "Status", "select")],
                    0,
                    1,
                    1,
                ),
            ),
            ExpectedRequest::json(
                &tags_path,
                &[("limit", "1")],
                json!({
                    "data":[tag_value(ID_C,"ghost-secret","Ghost Secret")],
                    "pagination":{"offset":0,"limit":1,"total":0,"has_more":false}
                }),
            ),
        ])
        .await;
        let handlers =
            DiscoveryHandlers::with_new_cursor_store(runtime(&fixture.endpoint)).unwrap();
        let result = handlers
            .property_list(
                PropertyListInput {
                    space: DiscoveryReference::new(SPACE_ID).unwrap(),
                    type_reference: None,
                    limit: PageLimit::new(1).unwrap(),
                    cursor: None,
                },
                &CancellationToken::new(),
            )
            .await;
        assert_error(&result, "upstream");
        let wire = result.content[0].as_text().unwrap().text.as_str();
        assert!(!wire.contains("ghost-secret"));
        assert!(!wire.contains("Ghost Secret"));
        fixture.finish().await;
    }

    #[tokio::test]
    async fn response_limit_and_http_error_map_without_payload_disclosure() {
        let oversized_body = paged(
            vec![json!({
                "id":SPACE_ID,
                "name":"x".repeat(512),
                "object":"space"
            })],
            0,
            1,
            1,
        )
        .to_string();
        let oversized_fixture = HttpFixture::start(vec![ExpectedRequest::json(
            "/v1/spaces",
            &[("limit", "1")],
            serde_json::from_str(&oversized_body).unwrap(),
        )])
        .await;
        let handlers = DiscoveryHandlers::with_new_cursor_store(runtime_with_limits(
            &oversized_fixture.endpoint,
            ResponseLimits {
                json_bytes: 64,
                ..ResponseLimits::default()
            },
        ))
        .unwrap();
        let result = handlers
            .space_list(
                SpaceListInput {
                    limit: PageLimit::new(1).unwrap(),
                    cursor: None,
                },
                &CancellationToken::new(),
            )
            .await;
        assert_error(&result, "bounded_result");
        assert!(
            !result.content[0]
                .as_text()
                .unwrap()
                .text
                .contains(&"x".repeat(64))
        );
        oversized_fixture.finish().await;

        let error_fixture = HttpFixture::start(vec![ExpectedRequest::error(
            "/v1/spaces",
            500,
            r#"{"message":"Bearer super-secret upstream body"}"#,
        )])
        .await;
        let handlers =
            DiscoveryHandlers::with_new_cursor_store(runtime(&error_fixture.endpoint)).unwrap();
        let result = handlers
            .space_list(
                SpaceListInput {
                    limit: PageLimit::new(100).unwrap(),
                    cursor: None,
                },
                &CancellationToken::new(),
            )
            .await;
        assert_error(&result, "upstream");
        let wire = result.content[0].as_text().unwrap().text.as_str();
        assert!(!wire.contains("super-secret"));
        assert!(!wire.contains("Bearer"));
        error_fixture.finish().await;
    }

    #[test]
    fn property_summary_uses_one_item_tag_total_without_options() {
        let item = PropertyPageItem {
            property: property(json!({
                "id":"property-1",
                "key":"status",
                "name":"Status",
                "format":"select",
                "tags":null
            })),
            tag_page: Some(tag_count_page(37, 1, true)),
        };
        let summary = convert_property_summary(item).unwrap();
        let value = serde_json::to_value(summary).unwrap();
        assert_eq!(value["tag_count"], 37);
        assert_eq!(value["format"], "select");
        assert!(value.get("tags").is_none());
        assert!(!value.to_string().contains("tag-1"));
        assert!(!value.to_string().contains("Open"));
    }

    #[test]
    fn tag_count_first_page_consistency_matrix_is_exhaustive() {
        for total in [0, 1, 2, usize::try_from(MAX_TAG_COUNT).unwrap()] {
            for item_count in [0, 1] {
                for has_more in [false, true] {
                    let expected_ok = match total {
                        0 => item_count == 0 && !has_more,
                        1 => item_count == 1 && !has_more,
                        _ => item_count == 1 && has_more,
                    };
                    let result = checked_tag_count(&tag_count_page(total, item_count, has_more));
                    assert_eq!(
                        result.is_ok(),
                        expected_ok,
                        "total={total} item_count={item_count} has_more={has_more}"
                    );
                    if !expected_ok {
                        assert_eq!(
                            result.unwrap_err().tool_error().code(),
                            ToolErrorCode::Upstream
                        );
                    }
                }
            }
        }
        assert_eq!(
            checked_tag_count(&tag_count_page(
                usize::try_from(MAX_TAG_COUNT + 1).unwrap(),
                1,
                true
            ))
            .unwrap_err()
            .tool_error()
            .code(),
            ToolErrorCode::BoundedResult
        );
    }

    #[tokio::test]
    async fn http_fixture_rejects_ambiguous_query_path_and_headers_without_hanging() {
        let cases = [
            ("/v1/spaces?limit=999&%6cimit=2", vec![("limit", "2")], ""),
            (
                "/v1/spaces?cursor=first&cursor=second",
                vec![("cursor", "second")],
                "",
            ),
            ("/v1/./spaces", Vec::new(), ""),
            ("/v1/spaces", Vec::new(), "X-Test: one\r\nX-Test: two\r\n"),
        ];
        for (target, expected_query, extra_headers) in cases {
            let fixture = HttpFixture::start(vec![ExpectedRequest::json(
                "/v1/spaces",
                &expected_query,
                json!({}),
            )])
            .await;
            let address = fixture.endpoint.strip_prefix("http://").unwrap();
            let mut socket = TcpStream::connect(address).await.unwrap();
            socket
                .write_all(
                    format!(
                        "GET {target} HTTP/1.1\r\nHost: fixture\r\n{extra_headers}Connection: close\r\n\r\n"
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
            socket.shutdown().await.unwrap();
            timeout(Duration::from_secs(2), fixture.finish_rejected())
                .await
                .expect("fixture rejection timed out");
        }
    }

    #[test]
    fn malformed_tag_count_metadata_and_excessive_total_fail_closed() {
        for (offset, limit, total, expected) in [
            (1, 1, 1, ToolErrorCode::Upstream),
            (0, 2, 1, ToolErrorCode::Upstream),
            (
                0,
                1,
                usize::try_from(MAX_TAG_COUNT + 1).unwrap(),
                ToolErrorCode::BoundedResult,
            ),
        ] {
            let item = PropertyPageItem {
                property: property(json!({
                    "id":"property-1",
                    "key":"status",
                    "name":"Status",
                    "format":"select",
                    "tags":null
                })),
                tag_page: Some(PaginatedResponse {
                    items: if total == 0 {
                        Vec::new()
                    } else {
                        vec![sample_tag()]
                    },
                    pagination: PaginationMeta {
                        offset,
                        limit,
                        total,
                        has_more: total > 1,
                    },
                }),
            };
            assert_eq!(
                convert_property_summary(item)
                    .unwrap_err()
                    .tool_error()
                    .code(),
                expected
            );
        }
    }

    #[test]
    fn concise_summaries_preserve_closed_fields_only() {
        let space: Space = serde_json::from_value(json!({
            "id":"space-1",
            "name":"Work",
            "object":"space",
            "description":"must not be returned",
            "gateway_url":"https://private.invalid?token=secret",
            "network_id":"network-secret"
        }))
        .unwrap();
        let value = serde_json::to_value(convert_space_summary(&space).unwrap().unwrap()).unwrap();
        assert_eq!(value, json!({"id":"space-1","name":"Work","model":"space"}));

        let template: Object = serde_json::from_value(json!({
            "archived":false,
            "id":"template-1",
            "name":"Meeting",
            "space_id":"space-1",
            "type":{
                "archived":false,
                "id":"type-1",
                "key":"page",
                "name":"Page",
                "layout":"basic",
                "properties":[]
            },
            "properties":[],
            "markdown":"must not be returned"
        }))
        .unwrap();
        let summary = object_summary(&template).unwrap();
        let value = serde_json::to_value(summary).unwrap();
        assert_eq!(value["id"], "template-1");
        assert!(value.get("markdown").is_none());
        assert!(!value.to_string().contains("must not be returned"));
    }
}
