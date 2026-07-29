// any-mcp - bounded, workflow-oriented MCP server for Anytype
//
// SPDX-FileCopyrightText: 2026 Steve Schoettler
// SPDX-License-Identifier: Apache-2.0

//! Static Phase 1 MCP catalog, routing, and protocol configuration.

use std::{fmt, sync::Arc};

use rmcp::{
    RoleServer, ServerHandler,
    model::{
        CallToolRequestMethod, CallToolRequestParams, CallToolResult, ErrorData, Implementation,
        ListResourceTemplatesResult, ListResourcesResult, ListToolsResult, PaginatedRequestParams,
        ProtocolVersion, ReadResourceRequestParams, ReadResourceResult, ServerCapabilities,
        ServerInfo, Tool,
    },
    service::RequestContext,
};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::{
    config::ApplicationProfile,
    cursor::{CursorStore, CursorStoreError},
    discovery::{
        DiscoveryHandlers, PropertyListInput, ServerStatusInput, SpaceListInput, TagListInput,
        TemplateListInput, TypeListInput, property_list_tool, server_status_tool, space_list_tool,
        tag_list_tool, template_list_tool, type_list_tool,
    },
    error::ToolError,
    handler_support::MutationAccess,
    object_archive::{
        ObjectArchiveInput, ObjectArchiveOutput, object_archive, object_archive_tool,
    },
    object_create::{ObjectCreateHandlers, ObjectCreateInput, object_create_tool},
    object_edit::{ObjectEditInput, ObjectEditOutput, object_edit, object_edit_tool},
    object_read::{ObjectGetInput, ObjectReadHandlers, ObjectSearchInput},
    object_update::{ObjectUpdateInput, ObjectUpdateOutput, object_update, object_update_tool},
    optional_toolsets::{
        OptionalCatalog, OptionalRegistryFuture, OptionalToolsetRegistry,
        OptionalToolsetStatusInput, OptionalToolsetStatusOutput, compose_optional_catalog,
        optional_toolset_status_tool, production_optional_registries,
    },
    protocol::WorkflowTool,
    resources::AnytypeResources,
    result::tool_error,
    runtime::RuntimeContext,
    schema::SchemaContractError,
    view_handlers::{ViewListInput, ViewObjectListInput, ViewReadHandlers},
};

/// Latest released MCP protocol revision advertised by production `any-mcp`.
///
/// This is intentionally rmcp's stable `LATEST`, with an exact assertion in
/// tests so a dependency upgrade cannot silently promote an unreleased wire
/// contract.
pub const PROTOCOL_VERSION: ProtocolVersion = ProtocolVersion::LATEST;

const SERVER_STATUS: &str = "server_status";
const SPACE_LIST: &str = "space_list";
const TYPE_LIST: &str = "type_list";
const PROPERTY_LIST: &str = "property_list";
const TAG_LIST: &str = "tag_list";
const TEMPLATE_LIST: &str = "template_list";
const OBJECT_SEARCH: &str = "object_search";
const OBJECT_GET: &str = "object_get";
const VIEW_LIST: &str = "view_list";
const VIEW_OBJECT_LIST: &str = "view_object_list";
const OBJECT_CREATE: &str = "object_create";
const OBJECT_UPDATE: &str = "object_update";
const OBJECT_EDIT: &str = "object_edit";
const OBJECT_ARCHIVE: &str = "object_archive";

const READ_TOOL_NAMES: [&str; 10] = [
    OBJECT_GET,
    OBJECT_SEARCH,
    PROPERTY_LIST,
    SERVER_STATUS,
    SPACE_LIST,
    TAG_LIST,
    TEMPLATE_LIST,
    TYPE_LIST,
    VIEW_LIST,
    VIEW_OBJECT_LIST,
];

const ALL_TOOL_NAMES: [&str; 14] = [
    OBJECT_ARCHIVE,
    OBJECT_CREATE,
    OBJECT_EDIT,
    OBJECT_GET,
    OBJECT_SEARCH,
    OBJECT_UPDATE,
    PROPERTY_LIST,
    SERVER_STATUS,
    SPACE_LIST,
    TAG_LIST,
    TEMPLATE_LIST,
    TYPE_LIST,
    VIEW_LIST,
    VIEW_OBJECT_LIST,
];

const COMPACT_TOOL_NAMES: [&str; 4] = [OBJECT_EDIT, OBJECT_GET, OBJECT_SEARCH, SERVER_STATUS];
const COMPACT_READ_TOOL_NAMES: [&str; 3] = [OBJECT_GET, OBJECT_SEARCH, SERVER_STATUS];

struct ServerState {
    tools: Vec<Tool>,
    access: MutationAccess,
    discovery: DiscoveryHandlers,
    object_read: ObjectReadHandlers,
    view_read: ViewReadHandlers,
    object_create: ObjectCreateHandlers,
    object_update_contract: WorkflowTool<ObjectUpdateOutput>,
    object_edit_contract: WorkflowTool<ObjectEditOutput>,
    object_archive_contract: WorkflowTool<ObjectArchiveOutput>,
    resources: AnytypeResources,
    resource_instances: Vec<rmcp::model::Resource>,
    resource_templates: Vec<rmcp::model::ResourceTemplate>,
    optional_catalog: OptionalCatalog,
    optional_status_contract: Option<WorkflowTool<OptionalToolsetStatusOutput>>,
    linked_optional_registries: &'static [&'static dyn OptionalToolsetRegistry],
    cursors: Arc<CursorStore>,
    #[cfg(test)]
    phase1_dispatch_polls: std::sync::atomic::AtomicUsize,
}

/// MCP handler backed by one authenticated runtime and one static catalog.
#[derive(Clone)]
pub struct AnyMcpServer {
    runtime: RuntimeContext,
    state: Arc<ServerState>,
}

impl fmt::Debug for AnyMcpServer {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AnyMcpServer")
            .field("runtime", &self.runtime)
            .field("tool_count", &self.state.tools.len())
            .field("read_only", &self.runtime.is_read_only())
            .finish_non_exhaustive()
    }
}

impl AnyMcpServer {
    /// Builds the selected static Phase 1 catalog over one authenticated runtime.
    ///
    /// Compact and standard profiles retain complete, byte-identical contracts
    /// for every shared tool. Read-only runtime configuration then omits every
    /// selected mutation while resources remain available. Catalog construction
    /// validates each exact profile/access inventory and refuses duplicate or
    /// disconnected contracts. All four serialized inventories are locked by a
    /// deterministic, reviewed `o200k_base` token-budget regression so schema
    /// growth cannot silently consume model context.
    ///
    /// # Errors
    ///
    /// Returns [`ServerBuildError`] if required startup availability is absent,
    /// or if a typed schema, cursor store, or exact static inventory cannot be
    /// constructed safely.
    pub fn new(runtime: RuntimeContext) -> Result<Self, ServerBuildError> {
        Self::build_with_optional_registries(runtime, production_optional_registries(), true)
    }

    #[cfg(any(test, feature = "acceptance-harness"))]
    pub(crate) fn new_with_optional_registries(
        runtime: RuntimeContext,
        linked_optional_registries: &'static [&'static dyn OptionalToolsetRegistry],
    ) -> Result<Self, ServerBuildError> {
        Self::build_with_optional_registries(runtime, linked_optional_registries, false)
    }

    fn build_with_optional_registries(
        runtime: RuntimeContext,
        linked_optional_registries: &'static [&'static dyn OptionalToolsetRegistry],
        validate_production_space_policy: bool,
    ) -> Result<Self, ServerBuildError> {
        let availability = runtime.startup_status();
        if !availability.http_available
            || ((runtime.profile().requires_grpc(runtime.is_read_only())
                || runtime.optional_toolsets().requires_grpc())
                && !availability.grpc_available)
        {
            return Err(ServerBuildError);
        }
        let cursors = Arc::new(CursorStore::new().map_err(ServerBuildError::cursor)?);
        let discovery = DiscoveryHandlers::new(runtime.clone(), cursors.clone());
        let object_read = ObjectReadHandlers::new(runtime.clone(), cursors.clone())
            .map_err(ServerBuildError::schema)?;
        let view_read = ViewReadHandlers::new(runtime.clone(), cursors.clone())
            .map_err(ServerBuildError::schema)?;
        let object_create =
            ObjectCreateHandlers::new(runtime.clone()).map_err(ServerBuildError::schema)?;
        let object_update_contract = object_update_tool().map_err(ServerBuildError::schema)?;
        let object_edit_contract = object_edit_tool().map_err(ServerBuildError::schema)?;
        let object_archive_contract = object_archive_tool().map_err(ServerBuildError::schema)?;

        let mut tools = vec![
            server_status_tool()
                .map_err(ServerBuildError::schema)?
                .into_tool(),
            space_list_tool()
                .map_err(ServerBuildError::schema)?
                .into_tool(),
            type_list_tool()
                .map_err(ServerBuildError::schema)?
                .into_tool(),
            property_list_tool()
                .map_err(ServerBuildError::schema)?
                .into_tool(),
            tag_list_tool()
                .map_err(ServerBuildError::schema)?
                .into_tool(),
            template_list_tool()
                .map_err(ServerBuildError::schema)?
                .into_tool(),
            object_read.search_contract().as_tool().clone(),
            object_read.get_contract().as_tool().clone(),
            view_read.view_list_contract().as_tool().clone(),
            view_read.view_object_list_contract().as_tool().clone(),
        ];
        tools.extend([
            object_create_tool()
                .map_err(ServerBuildError::schema)?
                .into_tool(),
            object_update_contract.as_tool().clone(),
            object_edit_contract.as_tool().clone(),
            object_archive_contract.as_tool().clone(),
        ]);
        let access = if runtime.is_read_only() {
            MutationAccess::ReadOnly
        } else {
            MutationAccess::Allowed
        };
        tools.retain(|tool| {
            let name = tool.name.as_ref();
            let selected = match runtime.profile() {
                ApplicationProfile::Compact => COMPACT_TOOL_NAMES.contains(&name),
                ApplicationProfile::Standard => ALL_TOOL_NAMES.contains(&name),
            };
            selected && !(runtime.is_read_only() && !READ_TOOL_NAMES.contains(&name))
        });
        let resources = AnytypeResources::new(runtime.clone());
        let mut resource_instances = resources
            .list_resources(None)
            .map_err(ServerBuildError::resource)?
            .resources;
        let mut resource_templates = resources
            .list_resource_templates(None)
            .map_err(ServerBuildError::resource)?
            .resource_templates;
        let reserved_resources = resource_instances
            .iter()
            .map(|resource| resource.uri.as_ref())
            .collect::<Vec<_>>();
        let reserved_templates = resource_templates
            .iter()
            .map(|template| template.uri_template.as_ref())
            .collect::<Vec<_>>();
        let optional_catalog = compose_optional_catalog(
            runtime.optional_toolsets(),
            linked_optional_registries,
            runtime.is_read_only(),
            &ALL_TOOL_NAMES,
            &reserved_resources,
            &reserved_templates,
        )
        .map_err(ServerBuildError::optional)?;
        let optional_tool_names = optional_catalog
            .tools
            .iter()
            .map(|tool| tool.name.to_string())
            .collect::<Vec<_>>();
        tools.extend(optional_catalog.tools.iter().cloned());
        let optional_status_contract = if optional_catalog.is_selected() {
            let contract = optional_toolset_status_tool().map_err(ServerBuildError::schema)?;
            tools.push(contract.as_tool().clone());
            Some(contract)
        } else {
            None
        };
        resource_instances.extend(optional_catalog.resources.iter().cloned());
        resource_instances.sort_by(|left, right| left.uri.cmp(&right.uri));
        resource_templates.extend(optional_catalog.resource_templates.iter().cloned());
        resource_templates.sort_by(|left, right| left.uri_template.cmp(&right.uri_template));

        tools.sort_by(|left, right| left.name.cmp(&right.name));
        validate_catalog(
            &tools,
            runtime.profile(),
            runtime.is_read_only(),
            &optional_tool_names,
            optional_catalog.is_selected(),
        )?;
        let resource_uris = resource_instances
            .iter()
            .map(|resource| resource.uri.as_ref())
            .collect::<Vec<_>>();
        let resource_template_uris = resource_templates
            .iter()
            .map(|template| template.uri_template.as_ref())
            .collect::<Vec<_>>();
        if validate_production_space_policy {
            validate_space_policy_ownership(&tools, &resource_uris, &resource_template_uris)?;
        }

        Ok(Self {
            runtime: runtime.clone(),
            state: Arc::new(ServerState {
                tools,
                access,
                discovery,
                object_read,
                view_read,
                object_create,
                object_update_contract,
                object_edit_contract,
                object_archive_contract,
                resources,
                resource_instances,
                resource_templates,
                optional_catalog,
                optional_status_contract,
                linked_optional_registries,
                cursors,
                #[cfg(test)]
                phase1_dispatch_polls: std::sync::atomic::AtomicUsize::new(0),
            }),
        })
    }

    /// Returns the shared authenticated runtime used by workflow handlers.
    #[must_use]
    pub const fn runtime(&self) -> &RuntimeContext {
        &self.runtime
    }

    /// Borrows the exact static tool catalog advertised by `tools/list`.
    #[must_use]
    pub fn tools(&self) -> &[Tool] {
        &self.state.tools
    }

    /// Returns the initialization metadata advertised to MCP clients.
    #[must_use]
    pub fn info(&self) -> ServerInfo {
        <Self as ServerHandler>::get_info(self)
    }

    fn reject_read_only_mutation(&self) -> Option<CallToolResult> {
        (self.state.access == MutationAccess::ReadOnly).then(|| tool_error(&ToolError::read_only()))
    }

    #[cfg_attr(
        not(any(test, feature = "acceptance-harness")),
        expect(dead_code, reason = "stable-version dispatch seam is used by tests")
    )]
    pub(crate) fn dispatch_tool<'a>(
        &'a self,
        request: CallToolRequestParams,
        cancellation: &'a tokio_util::sync::CancellationToken,
    ) -> OptionalRegistryFuture<'a, Result<CallToolResult, ErrorData>> {
        self.dispatch_tool_for_protocol(request, &PROTOCOL_VERSION, cancellation)
    }

    pub(crate) fn dispatch_tool_for_protocol<'a>(
        &'a self,
        request: CallToolRequestParams,
        protocol_version: &'a ProtocolVersion,
        cancellation: &'a tokio_util::sync::CancellationToken,
    ) -> OptionalRegistryFuture<'a, Result<CallToolResult, ErrorData>> {
        let name = request.name.as_ref();
        let selected_by_profile = match self.runtime.profile() {
            ApplicationProfile::Compact => COMPACT_TOOL_NAMES.contains(&name),
            ApplicationProfile::Standard => ALL_TOOL_NAMES.contains(&name),
        };
        let optional_selected = self
            .state
            .optional_catalog
            .registry_for_tool(name)
            .is_some()
            || self.state.optional_catalog.is_read_only_mutation(name)
            || (name == "optional_toolset_status" && self.state.optional_catalog.is_selected());
        if !selected_by_profile && !optional_selected {
            return Box::pin(std::future::ready(Err(ErrorData::method_not_found::<
                CallToolRequestMethod,
            >())));
        }
        if request.task.is_some() {
            return Box::pin(std::future::ready(Err(invalid_arguments())));
        }
        if self.state.optional_catalog.is_read_only_mutation(name) {
            return Box::pin(std::future::ready(Ok(tool_error(&ToolError::read_only()))));
        }
        if name == "optional_toolset_status" {
            return Box::pin(async move {
                let _input = decode_arguments::<OptionalToolsetStatusInput>(request.arguments)?;
                let contract = self
                    .state
                    .optional_status_contract
                    .as_ref()
                    .ok_or_else(|| {
                        ErrorData::internal_error("Optional status contract unavailable.", None)
                    })?;
                contract
                    .success(self.state.optional_catalog.status())
                    .map_err(|_| {
                        ErrorData::internal_error("Optional status encoding failed.", None)
                    })
            });
        }
        if let Some(registry) = self.state.optional_catalog.registry_for_tool(name) {
            return registry.call_tool(
                request,
                &self.runtime,
                &self.state.cursors,
                protocol_version,
                cancellation,
            );
        }
        self.dispatch_phase1_tool(request, cancellation)
    }

    fn dispatch_phase1_tool<'a>(
        &'a self,
        request: CallToolRequestParams,
        cancellation: &'a tokio_util::sync::CancellationToken,
    ) -> OptionalRegistryFuture<'a, Result<CallToolResult, ErrorData>> {
        // Select before constructing the async route so each handler has its
        // own erased heap frame. One aggregate async match exceeds the default
        // debug-build worker stack even when that aggregate future is boxed.
        let name = request.name;
        let arguments = request.arguments;
        match name.as_ref() {
            SERVER_STATUS => self.phase1_route(async move {
                let input = decode_arguments::<ServerStatusInput>(arguments)?;
                Ok(self.discovery().server_status(input, cancellation).await)
            }),
            SPACE_LIST => self.phase1_route(async move {
                let input = decode_arguments::<SpaceListInput>(arguments)?;
                Ok(self.discovery().space_list(input, cancellation).await)
            }),
            TYPE_LIST => self.phase1_route(async move {
                let input = decode_arguments::<TypeListInput>(arguments)?;
                Ok(self.discovery().type_list(input, cancellation).await)
            }),
            PROPERTY_LIST => self.phase1_route(async move {
                let input = decode_arguments::<PropertyListInput>(arguments)?;
                Ok(self.discovery().property_list(input, cancellation).await)
            }),
            TAG_LIST => self.phase1_route(async move {
                let input = decode_arguments::<TagListInput>(arguments)?;
                Ok(self.discovery().tag_list(input, cancellation).await)
            }),
            TEMPLATE_LIST => self.phase1_route(async move {
                let input = decode_arguments::<TemplateListInput>(arguments)?;
                Ok(self.discovery().template_list(input, cancellation).await)
            }),
            OBJECT_SEARCH => self.phase1_route(async move {
                let input = decode_arguments::<ObjectSearchInput>(arguments)?;
                Ok(self
                    .state
                    .object_read
                    .object_search(input, cancellation)
                    .await)
            }),
            OBJECT_GET => self.phase1_route(async move {
                let input = decode_arguments::<ObjectGetInput>(arguments)?;
                Ok(self.state.object_read.object_get(input, cancellation).await)
            }),
            VIEW_LIST => self.phase1_route(async move {
                let input = decode_arguments::<ViewListInput>(arguments)?;
                Ok(self.state.view_read.view_list(input, cancellation).await)
            }),
            VIEW_OBJECT_LIST => self.phase1_route(async move {
                let input = decode_arguments::<ViewObjectListInput>(arguments)?;
                Ok(self
                    .state
                    .view_read
                    .view_object_list(input, cancellation)
                    .await)
            }),
            OBJECT_CREATE => self.phase1_route(async move {
                if let Some(error) = self.reject_read_only_mutation() {
                    return Ok(error);
                }
                let input = decode_arguments::<ObjectCreateInput>(arguments)?;
                Ok(self
                    .state
                    .object_create
                    .object_create(self.state.access, input, cancellation)
                    .await)
            }),
            OBJECT_UPDATE => self.phase1_route(async move {
                if let Some(error) = self.reject_read_only_mutation() {
                    return Ok(error);
                }
                let input = decode_arguments::<ObjectUpdateInput>(arguments)?;
                Ok(object_update(
                    &self.runtime,
                    &self.state.object_update_contract,
                    self.state.access,
                    &input,
                    cancellation,
                )
                .await)
            }),
            OBJECT_EDIT => self.phase1_route(async move {
                if let Some(error) = self.reject_read_only_mutation() {
                    return Ok(error);
                }
                let input = decode_arguments::<ObjectEditInput>(arguments)?;
                Ok(object_edit(
                    &self.runtime,
                    &self.state.object_edit_contract,
                    self.state.access,
                    &input,
                    cancellation,
                )
                .await)
            }),
            OBJECT_ARCHIVE => self.phase1_route(async move {
                if let Some(error) = self.reject_read_only_mutation() {
                    return Ok(error);
                }
                let input = decode_arguments::<ObjectArchiveInput>(arguments)?;
                Ok(object_archive(
                    &self.runtime,
                    &self.state.object_archive_contract,
                    self.state.access,
                    &input,
                    cancellation,
                )
                .await)
            }),
            _ => self.phase1_route(async {
                Err(ErrorData::method_not_found::<CallToolRequestMethod>())
            }),
        }
    }

    fn phase1_route<'a, F>(
        &'a self,
        route: F,
    ) -> OptionalRegistryFuture<'a, Result<CallToolResult, ErrorData>>
    where
        F: std::future::Future<Output = Result<CallToolResult, ErrorData>> + Send + 'a,
    {
        #[cfg(test)]
        {
            // Keep decoding, read-only admission, and test poll accounting lazy.
            let state = Arc::clone(&self.state);
            let mut route = Box::pin(route);
            let mut first_poll = true;
            Box::pin(std::future::poll_fn(move |context| {
                if first_poll {
                    state
                        .phase1_dispatch_polls
                        .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
                    first_poll = false;
                }
                std::future::Future::poll(route.as_mut(), context)
            }))
        }
        #[cfg(not(test))]
        {
            Box::pin(route)
        }
    }

    #[cfg(test)]
    pub(crate) fn phase1_dispatch_polls(&self) -> usize {
        self.state
            .phase1_dispatch_polls
            .load(std::sync::atomic::Ordering::Acquire)
    }

    pub(crate) fn list_tools_wire(
        &self,
        request: Option<PaginatedRequestParams>,
    ) -> Result<ListToolsResult, ErrorData> {
        reject_static_cursor(request)?;
        Ok(ListToolsResult::with_all_items(self.state.tools.clone()))
    }

    pub(crate) fn list_resources_wire(
        &self,
        request: Option<PaginatedRequestParams>,
    ) -> Result<ListResourcesResult, ErrorData> {
        reject_static_cursor(request)?;
        Ok(ListResourcesResult::with_all_items(
            self.state.resource_instances.clone(),
        ))
    }

    pub(crate) fn list_resource_templates_wire(
        &self,
        request: Option<PaginatedRequestParams>,
    ) -> Result<ListResourceTemplatesResult, ErrorData> {
        reject_static_cursor(request)?;
        Ok(ListResourceTemplatesResult::with_all_items(
            self.state.resource_templates.clone(),
        ))
    }

    pub(crate) async fn read_resource_wire(
        &self,
        request: ReadResourceRequestParams,
        cancellation: &tokio_util::sync::CancellationToken,
    ) -> Result<ReadResourceResult, ErrorData> {
        if let Some(registry) = self
            .state
            .optional_catalog
            .registry_for_resource(&request.uri)
        {
            return registry
                .read_resource(request, &self.runtime, cancellation)
                .await;
        }
        if self
            .state
            .linked_optional_registries
            .iter()
            .any(|registry| registry.owns_resource_uri(&request.uri))
        {
            return Err(ErrorData::method_not_found::<
                rmcp::model::ReadResourceRequestMethod,
            >());
        }
        self.state
            .resources
            .read_resource(request, cancellation)
            .await
    }
}

impl ServerHandler for AnyMcpServer {
    fn get_info(&self) -> ServerInfo {
        let capabilities = ServerCapabilities::builder()
            .enable_resources()
            .enable_tools()
            .build();
        ServerInfo::new(capabilities)
            .with_protocol_version(PROTOCOL_VERSION)
            .with_server_info(Implementation::new(
                env!("CARGO_PKG_NAME"),
                env!("CARGO_PKG_VERSION"),
            ))
            .with_instructions("Bounded, workflow-oriented access to Anytype")
    }

    async fn list_tools(
        &self,
        request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        self.list_tools_wire(request)
    }

    fn get_tool(&self, name: &str) -> Option<Tool> {
        self.state
            .tools
            .iter()
            .find(|tool| tool.name == name)
            .cloned()
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        // Optional registries are selected before the exhaustive Phase-1
        // future is constructed, so this erased await contains only the
        // chosen route's state.
        let protocol_version = context.protocol_version().unwrap_or(PROTOCOL_VERSION);
        Box::pin(self.dispatch_tool_for_protocol(request, &protocol_version, &context.ct)).await
    }

    async fn list_resources(
        &self,
        request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, ErrorData> {
        self.list_resources_wire(request)
    }

    async fn list_resource_templates(
        &self,
        request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourceTemplatesResult, ErrorData> {
        self.list_resource_templates_wire(request)
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResult, ErrorData> {
        self.read_resource_wire(request, &context.ct).await
    }
}

impl AnyMcpServer {
    fn discovery(&self) -> &DiscoveryHandlers {
        &self.state.discovery
    }
}

pub(crate) fn decode_arguments<T: DeserializeOwned>(
    arguments: Option<rmcp::model::JsonObject>,
) -> Result<T, ErrorData> {
    serde_json::from_value(Value::Object(arguments.unwrap_or_default()))
        .map_err(|_| invalid_arguments())
}

fn invalid_arguments() -> ErrorData {
    ErrorData::invalid_params(
        "Tool arguments do not match the declared schema.",
        Some(json!({"code":"validation"})),
    )
}

fn reject_static_cursor(request: Option<PaginatedRequestParams>) -> Result<(), ErrorData> {
    if request.and_then(|request| request.cursor).is_some() {
        return Err(ErrorData::invalid_params(
            "The static tool catalog does not use cursors.",
            Some(json!({"code":"validation"})),
        ));
    }
    Ok(())
}

fn validate_catalog(
    tools: &[Tool],
    profile: ApplicationProfile,
    read_only: bool,
    optional_tool_names: &[String],
    include_optional_status: bool,
) -> Result<(), ServerBuildError> {
    let phase_one: &[&str] = match (profile, read_only) {
        (ApplicationProfile::Compact, true) => &COMPACT_READ_TOOL_NAMES,
        (ApplicationProfile::Compact, false) => &COMPACT_TOOL_NAMES,
        (ApplicationProfile::Standard, true) => &READ_TOOL_NAMES,
        (ApplicationProfile::Standard, false) => &ALL_TOOL_NAMES,
    };
    let mut expected = phase_one
        .iter()
        .map(|name| (*name).to_owned())
        .collect::<Vec<_>>();
    expected.extend(optional_tool_names.iter().cloned());
    if include_optional_status {
        expected.push("optional_toolset_status".to_owned());
    }
    expected.sort();
    if tools.len() != expected.len()
        || tools
            .iter()
            .map(|tool| tool.name.as_ref())
            .ne(expected.iter().map(String::as_str))
    {
        return Err(ServerBuildError);
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SpacePolicyOwnership {
    Global,
    FilteredGlobalDiscovery,
    OptionalResolvedSpace,
    ResolvedSpace,
    OpaqueBoundSpace,
    ConditionalSpaceCreation,
}

fn tool_space_policy_ownership(name: &str) -> Option<SpacePolicyOwnership> {
    match name {
        "server_status" | "optional_toolset_status" | "artifact_status" => {
            Some(SpacePolicyOwnership::Global)
        }
        "space_list" => Some(SpacePolicyOwnership::FilteredGlobalDiscovery),
        "object_search" => Some(SpacePolicyOwnership::OptionalResolvedSpace),
        "space_create" => Some(SpacePolicyOwnership::ConditionalSpaceCreation),
        "artifact_release" => Some(SpacePolicyOwnership::OpaqueBoundSpace),
        "object_archive"
        | "object_create"
        | "object_edit"
        | "object_get"
        | "object_update"
        | "property_list"
        | "tag_list"
        | "template_list"
        | "type_list"
        | "view_list"
        | "view_object_list"
        | "body_block_create"
        | "body_block_delete"
        | "body_block_list"
        | "body_block_move"
        | "body_block_update"
        | "rich_page_create"
        | "chat_list"
        | "chat_message_add"
        | "chat_message_delete"
        | "chat_message_get"
        | "chat_message_list"
        | "chat_message_search"
        | "member_get"
        | "member_list"
        | "file_metadata"
        | "file_read"
        | "file_upload"
        | "file_import"
        | "file_export"
        | "artifact_stage_upload"
        | "document_export"
        | "document_import_create"
        | "document_import_update"
        | "collection_member_add"
        | "collection_member_list"
        | "collection_member_remove"
        | "property_create"
        | "property_update"
        | "space_update"
        | "tag_create"
        | "tag_update"
        | "type_create"
        | "type_get"
        | "type_update" => Some(SpacePolicyOwnership::ResolvedSpace),
        _ => None,
    }
}

fn validate_space_policy_ownership(
    tools: &[Tool],
    resource_uris: &[&str],
    resource_template_uris: &[&str],
) -> Result<(), ServerBuildError> {
    if tools
        .iter()
        .any(|tool| tool_space_policy_ownership(tool.name.as_ref()).is_none())
        || !resource_uris.is_empty()
        || resource_template_uris.iter().any(|uri| {
            !matches!(
                *uri,
                "anytype://spaces/{space_id}/objects/{object_id}"
                    | "anytype-file://bytes/{space_id}/{file_id}/{offset}/{length}/{sha256}"
            )
        })
    {
        return Err(ServerBuildError);
    }
    Ok(())
}

/// Fixed startup failure returned when the static catalog cannot be built.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ServerBuildError;

impl ServerBuildError {
    fn schema(_: SchemaContractError) -> Self {
        Self
    }

    fn cursor(_: CursorStoreError) -> Self {
        Self
    }

    fn optional(_: crate::optional_toolsets::OptionalCatalogError) -> Self {
        Self
    }

    fn resource(_: ErrorData) -> Self {
        Self
    }
}

impl fmt::Display for ServerBuildError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("unable to build the static MCP catalog")
    }
}

impl std::error::Error for ServerBuildError {}

#[cfg(test)]
#[path = "server/headless_integration.rs"]
mod headless_integration;

#[cfg(test)]
#[path = "server/optional_registry.rs"]
mod optional_registry;

#[cfg(test)]
mod tests {
    use std::{fs, path::Path, time::Duration};

    use anytype::prelude::{AnytypeClient, ClientConfig, HttpCredentials};
    use rmcp::ServiceExt;
    use serde::Deserialize;
    use serde_json::{Value, json};
    use sha2::{Digest, Sha256};
    use tiktoken_rs::{CoreBPE, o200k_base};
    use tokio::{
        io::{
            AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader, DuplexStream, ReadHalf,
            WriteHalf, duplex, split,
        },
        net::TcpListener,
    };
    use tokio_util::sync::CancellationToken;

    use super::*;
    use crate::{
        optional_toolsets::{OptionalToolsetSelection, production_optional_metadata},
        resources::OBJECT_RESOURCE_TEMPLATE,
        runtime::{StartupStatus, serve_transport},
    };

    const SPACE_ID: &str =
        "bafyreid5fvqlnsobih2keakcxjrrlpmly6kf37klzjzen4ibfdgalcdp4y.2tq5w93cr6oe7";
    const OBJECT_ID: &str = "bafyreie6n5l5nkbjal37su54cha4coy7qzuhrnajluzv5qd5jvtsrxkequ";
    const RESOURCE_URI: &str = "anytype://spaces/bafyreid5fvqlnsobih2keakcxjrrlpmly6kf37klzjzen4ibfdgalcdp4y.2tq5w93cr6oe7/objects/bafyreie6n5l5nkbjal37su54cha4coy7qzuhrnajluzv5qd5jvtsrxkequ";
    const NORMAL_CATALOG_SNAPSHOT: &str = include_str!("../tests/snapshots/catalog-normal.snap");
    const READ_ONLY_CATALOG_SNAPSHOT: &str =
        include_str!("../tests/snapshots/catalog-read-only.snap");
    const COMPACT_CATALOG_SNAPSHOT: &str = include_str!("../tests/snapshots/catalog-compact.snap");
    const COMPACT_READ_ONLY_CATALOG_SNAPSHOT: &str =
        include_str!("../tests/snapshots/catalog-compact-read-only.snap");
    const REPRESENTATIVE_RESULTS_SNAPSHOT: &str =
        include_str!("../tests/snapshots/result-representatives.json");
    const TOKEN_BUDGET_SNAPSHOT: &str = include_str!("../tests/snapshots/token-budget.json");
    const OPTIONAL_READ_TOOL_NAMES: [&str; 13] = [
        "artifact_status",
        "body_block_list",
        "chat_list",
        "chat_message_get",
        "chat_message_list",
        "chat_message_search",
        "collection_member_list",
        "file_metadata",
        "file_read",
        "member_get",
        "member_list",
        "optional_toolset_status",
        "type_get",
    ];
    const OPTIONAL_CREATE_TOOL_NAMES: [&str; 11] = [
        "artifact_stage_upload",
        "body_block_create",
        "chat_message_add",
        "document_import_create",
        "file_import",
        "file_upload",
        "property_create",
        "rich_page_create",
        "space_create",
        "tag_create",
        "type_create",
    ];
    const OPTIONAL_UPDATE_TOOL_NAMES: [&str; 14] = [
        "artifact_release",
        "body_block_delete",
        "body_block_move",
        "body_block_update",
        "chat_message_delete",
        "collection_member_add",
        "collection_member_remove",
        "document_export",
        "document_import_update",
        "file_export",
        "property_update",
        "space_update",
        "tag_update",
        "type_update",
    ];

    #[derive(Debug, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct ReviewedTokenBudget {
        tokenizer: String,
        smallest_supported_context_tokens: usize,
        catalog_context_limit_percent: usize,
        material_growth_review_percent: usize,
        compact_catalog_tokens: usize,
        compact_read_only_catalog_tokens: usize,
        standard_catalog_tokens: usize,
        standard_read_only_catalog_tokens: usize,
        object_search_result_tokens: usize,
        object_get_result_tokens: usize,
    }

    fn runtime(read_only: bool) -> RuntimeContext {
        runtime_with_profile(ApplicationProfile::Standard, read_only)
    }

    fn runtime_with_profile(profile: ApplicationProfile, read_only: bool) -> RuntimeContext {
        runtime_at_with_profile("http://127.0.0.1:1".to_owned(), profile, read_only)
    }

    fn runtime_at(base_url: String, read_only: bool) -> RuntimeContext {
        runtime_at_with_profile(base_url, ApplicationProfile::Standard, read_only)
    }

    fn runtime_at_with_profile(
        base_url: String,
        profile: ApplicationProfile,
        read_only: bool,
    ) -> RuntimeContext {
        runtime_at_with_availability(
            base_url,
            profile,
            read_only,
            StartupStatus {
                http_available: true,
                grpc_available: profile.requires_grpc(read_only),
            },
        )
    }

    fn runtime_at_with_availability(
        base_url: String,
        profile: ApplicationProfile,
        read_only: bool,
        startup_status: StartupStatus,
    ) -> RuntimeContext {
        runtime_at_with_availability_and_optional_toolsets(
            base_url,
            profile,
            read_only,
            startup_status,
            OptionalToolsetSelection::default(),
        )
    }

    fn runtime_at_with_availability_and_optional_toolsets(
        base_url: String,
        profile: ApplicationProfile,
        read_only: bool,
        startup_status: StartupStatus,
        optional_toolsets: OptionalToolsetSelection,
    ) -> RuntimeContext {
        let client = AnytypeClient::with_config(ClientConfig {
            base_url: Some(base_url),
            keystore: Some("env".to_string()),
            keystore_service: Some("any-mcp-server-test".to_string()),
            app_name: "any-mcp-server-test".to_string(),
            ..ClientConfig::default()
        })
        .expect("in-memory test client");
        client.set_api_key(HttpCredentials::new("fixture-token"));
        RuntimeContext::from_parts_with_profile_and_optional_toolsets(
            client,
            1,
            Duration::from_secs(1),
            startup_status,
            profile,
            read_only,
            optional_toolsets,
        )
    }

    fn runtime_with_all_optional_toolsets(
        profile: ApplicationProfile,
        read_only: bool,
    ) -> RuntimeContext {
        let metadata = production_optional_metadata();
        let selector = metadata
            .iter()
            .map(|entry| entry.name)
            .collect::<Vec<_>>()
            .join(",");
        let selection = OptionalToolsetSelection::parse(Some(selector), &metadata)
            .expect("complete production optional selection");
        runtime_at_with_availability_and_optional_toolsets(
            "http://127.0.0.1:1".to_owned(),
            profile,
            read_only,
            StartupStatus {
                http_available: true,
                grpc_available: true,
            },
            selection,
        )
    }

    fn tool_names(server: &AnyMcpServer) -> Vec<&str> {
        server
            .tools()
            .iter()
            .map(|tool| tool.name.as_ref())
            .collect()
    }

    fn canonical_json(value: Value) -> Value {
        match value {
            Value::Array(values) => Value::Array(values.into_iter().map(canonical_json).collect()),
            Value::Object(object) => {
                let mut entries = object.into_iter().collect::<Vec<_>>();
                entries.sort_by(|left, right| left.0.cmp(&right.0));
                Value::Object(
                    entries
                        .into_iter()
                        .map(|(key, value)| (key, canonical_json(value)))
                        .collect(),
                )
            }
            scalar => scalar,
        }
    }

    fn catalog_snapshot(profile: ApplicationProfile, read_only: bool) -> String {
        let server =
            AnyMcpServer::new(runtime_with_profile(profile, read_only)).expect("static catalog");
        let value = canonical_json(json!({
            "read_only": read_only,
            "tools": server.tools(),
        }));
        format!(
            "{}\n",
            serde_json::to_string_pretty(&value).expect("serialize static catalog")
        )
    }

    #[test]
    fn server_build_admission_matches_profile_access_transport_requirements() {
        for protocol_profile in [ApplicationProfile::Compact, ApplicationProfile::Standard] {
            for read_only in [false, true] {
                for grpc_available in [false, true] {
                    let runtime = runtime_at_with_availability(
                        "http://127.0.0.1:1".to_owned(),
                        protocol_profile,
                        read_only,
                        StartupStatus {
                            http_available: true,
                            grpc_available,
                        },
                    );
                    let expected = !protocol_profile.requires_grpc(read_only) || grpc_available;
                    assert_eq!(
                        AnyMcpServer::new(runtime).is_ok(),
                        expected,
                        "profile={protocol_profile:?} read_only={read_only} grpc={grpc_available}"
                    );
                }

                let missing_http = runtime_at_with_availability(
                    "http://127.0.0.1:1".to_owned(),
                    protocol_profile,
                    read_only,
                    StartupStatus {
                        http_available: false,
                        grpc_available: true,
                    },
                );
                assert!(AnyMcpServer::new(missing_http).is_err());
            }
        }
    }

    fn compact_canonical_json(value: Value) -> String {
        serde_json::to_string(&canonical_json(value)).expect("serialize compact canonical JSON")
    }

    fn token_count(tokenizer: &CoreBPE, value: Value) -> usize {
        tokenizer
            .encode_with_special_tokens(&compact_canonical_json(value))
            .len()
    }

    fn tools_list_value(profile: ApplicationProfile, read_only: bool) -> Value {
        let server =
            AnyMcpServer::new(runtime_with_profile(profile, read_only)).expect("static catalog");
        serde_json::to_value(ListToolsResult::with_all_items(server.tools().to_vec()))
            .expect("serialize complete tools/list result")
    }

    fn assert_valid_representative(server: &AnyMcpServer, name: &str, result: &Value) {
        let schema = server
            .tools()
            .iter()
            .find(|tool| tool.name == name)
            .and_then(|tool| tool.output_schema.as_ref())
            .map(|schema| Value::Object(schema.as_ref().clone()))
            .unwrap_or_else(|| panic!("production {name} output schema"));
        let validator = jsonschema::draft202012::options()
            .build(&schema)
            .unwrap_or_else(|error| panic!("compile production {name} output schema: {error}"));
        assert!(
            validator.is_valid(result),
            "representative {name} result must satisfy the production output schema"
        );
    }

    const MAX_AUDITED_STRING_CHARS: u64 = 100_000;
    const MAX_AUDITED_ARRAY_ITEMS: u64 = 10_000;
    const MAX_AUDITED_ENUM_VALUES: usize = 128;
    const MAX_AUDITED_NUMBER_ABS: f64 = 9_007_199_254_740_991.0;
    const MAX_AUDITED_ANNOTATION_CHARS: usize = 4_096;

    struct SchemaAudit<'root> {
        root: &'root Value,
        visited_refs: std::collections::HashSet<String>,
        active_refs: std::collections::HashSet<String>,
        active_ref_depths: std::collections::HashMap<String, usize>,
        guard_depth: usize,
    }

    impl<'root> SchemaAudit<'root> {
        fn new(root: &'root Value) -> Self {
            Self {
                root,
                visited_refs: std::collections::HashSet::new(),
                active_refs: std::collections::HashSet::new(),
                active_ref_depths: std::collections::HashMap::new(),
                guard_depth: 0,
            }
        }

        fn validate(mut self) -> Result<(), String> {
            self.validate_node(self.root, "#")
        }

        fn validate_node(&mut self, value: &Value, path: &str) -> Result<(), String> {
            match value {
                Value::Bool(false) => Ok(()),
                Value::Bool(true) => Err(format!("{path}: true schema is unconstrained")),
                Value::Object(schema) => self.validate_schema_object(schema, path),
                _ => Err(format!("{path}: schema must be an object or false")),
            }
        }

        fn validate_schema_object(
            &mut self,
            schema: &serde_json::Map<String, Value>,
            path: &str,
        ) -> Result<(), String> {
            if schema.is_empty() {
                return Err(format!("{path}: empty schema is unconstrained"));
            }
            self.validate_annotations(schema, path)?;
            self.validate_root_definitions(schema, path)?;

            if let Some(reference) = schema.get("$ref") {
                self.require_allowed_keys(schema, path, &["$ref"])?;
                let reference = reference
                    .as_str()
                    .ok_or_else(|| format!("{path}/$ref: reference must be a string"))?;
                return self.validate_reference(reference, path);
            }

            let compositions = ["allOf", "anyOf", "oneOf"]
                .into_iter()
                .filter(|keyword| schema.contains_key(*keyword))
                .collect::<Vec<_>>();
            if !compositions.is_empty() {
                if compositions.len() != 1 {
                    return Err(format!(
                        "{path}: exactly one composition keyword is permitted"
                    ));
                }
                let keyword = compositions[0];
                let typed_object_union = keyword == "oneOf"
                    && schema.get("type").and_then(Value::as_str) == Some("object");
                if typed_object_union {
                    self.require_allowed_keys(schema, path, &["type", keyword])?;
                } else {
                    self.require_allowed_keys(schema, path, &[keyword])?;
                }
                let branches = schema[keyword]
                    .as_array()
                    .ok_or_else(|| format!("{path}/{keyword}: composition must be an array"))?;
                let minimum = if keyword == "allOf" { 1 } else { 2 };
                if branches.len() < minimum {
                    return Err(format!(
                        "{path}/{keyword}: composition requires at least {minimum} branches"
                    ));
                }
                for (index, branch) in branches.iter().enumerate() {
                    self.validate_node(branch, &format!("{path}/{keyword}/{index}"))?;
                }
                return Ok(());
            }

            let Some(raw_type) = schema.get("type") else {
                self.require_allowed_keys(schema, path, &["const", "enum"])?;
                if schema.contains_key("const") == schema.contains_key("enum") {
                    return Err(format!(
                        "{path}: untyped schema requires exactly one finite const or enum"
                    ));
                }
                self.validate_finite_values(schema, path, None)?;
                return Ok(());
            };
            let kind = self.normalized_type(raw_type, path)?;
            match kind {
                "object" => self.validate_object(schema, path),
                "array" => self.validate_array(schema, path),
                "string" => self.validate_string(schema, path),
                "number" | "integer" => self.validate_number(schema, path, kind),
                "boolean" | "null" => self.validate_scalar_type(schema, path, kind),
                _ => Err(format!("{path}/type: unknown schema type {kind:?}")),
            }
        }

        fn validate_root_definitions(
            &mut self,
            schema: &serde_json::Map<String, Value>,
            path: &str,
        ) -> Result<(), String> {
            let Some(definitions) = schema.get("$defs") else {
                return Ok(());
            };
            if path != "#" {
                return Err(format!("{path}/$defs: definitions are root-only"));
            }
            let definitions = definitions
                .as_object()
                .filter(|definitions| !definitions.is_empty())
                .ok_or_else(|| format!("{path}/$defs: definitions must be a nonempty object"))?;
            for name in definitions.keys() {
                let pointer = format!("#/$defs/{}", json_pointer_token(name));
                self.validate_reference(&pointer, path)?;
            }
            Ok(())
        }

        fn validate_reference(&mut self, reference: &str, path: &str) -> Result<(), String> {
            if !reference
                .strip_prefix("#/$defs/")
                .is_some_and(|suffix| !suffix.is_empty())
            {
                return Err(format!(
                    "{path}/$ref: reference must be local under #/$defs"
                ));
            }
            let target = self
                .root
                .pointer(reference.strip_prefix('#').expect("local reference"))
                .ok_or_else(|| format!("{path}/$ref: dangling reference {reference}"))?;
            if self.visited_refs.contains(reference) {
                return Ok(());
            }
            if self.active_refs.contains(reference) {
                let opening_depth = self
                    .active_ref_depths
                    .get(reference)
                    .copied()
                    .expect("active references retain their opening depth");
                return if self.guard_depth > opening_depth {
                    Ok(())
                } else {
                    Err(format!(
                        "{path}/$ref: unguarded cyclic reference {reference}"
                    ))
                };
            }

            self.active_refs.insert(reference.to_owned());
            self.active_ref_depths
                .insert(reference.to_owned(), self.guard_depth);
            let result = self.validate_node(target, reference);
            self.active_refs.remove(reference);
            self.active_ref_depths.remove(reference);
            if result.is_ok() {
                self.visited_refs.insert(reference.to_owned());
            }
            result
        }

        fn validate_object(
            &mut self,
            schema: &serde_json::Map<String, Value>,
            path: &str,
        ) -> Result<(), String> {
            self.require_allowed_keys(
                schema,
                path,
                &[
                    "type",
                    "properties",
                    "required",
                    "additionalProperties",
                    "minProperties",
                    "maxProperties",
                ],
            )?;
            if schema.get("additionalProperties") != Some(&Value::Bool(false)) {
                return Err(format!(
                    "{path}/additionalProperties: object schema must reject open maps"
                ));
            }
            let properties = match schema.get("properties") {
                None => None,
                Some(Value::Object(properties)) => Some(properties),
                Some(_) => return Err(format!("{path}/properties: must be an object")),
            };
            if let Some(properties) = properties {
                for (name, property) in properties {
                    self.validate_guarded_node(
                        property,
                        &format!("{path}/properties/{}", json_pointer_token(name)),
                    )?;
                }
            }
            if let Some(required) = schema.get("required") {
                let required = required
                    .as_array()
                    .ok_or_else(|| format!("{path}/required: must be an array"))?;
                let mut names = std::collections::HashSet::new();
                for name in required {
                    let name = name
                        .as_str()
                        .ok_or_else(|| format!("{path}/required: names must be strings"))?;
                    if !names.insert(name) {
                        return Err(format!("{path}/required: duplicate property {name:?}"));
                    }
                    if properties.is_none_or(|properties| !properties.contains_key(name)) {
                        return Err(format!(
                            "{path}/required: unknown required property {name:?}"
                        ));
                    }
                }
            }
            let property_count = properties.map_or(0, serde_json::Map::len);
            let minimum = match schema.get("minProperties") {
                Some(value) => value.as_u64().ok_or_else(|| {
                    format!("{path}/minProperties: must be a nonnegative integer")
                })?,
                None => 0,
            };
            let maximum = match schema.get("maxProperties") {
                Some(value) => value.as_u64().ok_or_else(|| {
                    format!("{path}/maxProperties: must be a nonnegative integer")
                })?,
                None => u64::try_from(property_count)
                    .map_err(|_| format!("{path}/properties: property count is impractical"))?,
            };
            if minimum > maximum {
                return Err(format!("{path}/minProperties: exceeds maxProperties"));
            }
            if usize::try_from(maximum)
                .ok()
                .is_none_or(|maximum| maximum > property_count)
            {
                return Err(format!(
                    "{path}/maxProperties: exceeds declared property count"
                ));
            }
            Ok(())
        }

        fn validate_array(
            &mut self,
            schema: &serde_json::Map<String, Value>,
            path: &str,
        ) -> Result<(), String> {
            self.require_allowed_keys(
                schema,
                path,
                &["type", "items", "minItems", "maxItems", "uniqueItems"],
            )?;
            let maximum = schema
                .get("maxItems")
                .and_then(Value::as_u64)
                .filter(|maximum| *maximum <= MAX_AUDITED_ARRAY_ITEMS)
                .ok_or_else(|| format!("{path}/maxItems: missing or impractical array bound"))?;
            if schema
                .get("minItems")
                .and_then(Value::as_u64)
                .is_some_and(|minimum| minimum > maximum)
            {
                return Err(format!("{path}/minItems: exceeds maxItems"));
            }
            if schema.get("minItems").is_some_and(|value| !value.is_u64()) {
                return Err(format!("{path}/minItems: must be a nonnegative integer"));
            }
            if schema
                .get("uniqueItems")
                .is_some_and(|value| !value.is_boolean())
            {
                return Err(format!("{path}/uniqueItems: must be boolean"));
            }
            let items = schema
                .get("items")
                .ok_or_else(|| format!("{path}/items: array items must be constrained"))?;
            self.validate_guarded_node(items, &format!("{path}/items"))
        }

        fn validate_string(
            &mut self,
            schema: &serde_json::Map<String, Value>,
            path: &str,
        ) -> Result<(), String> {
            let finite = self.validate_finite_values(schema, path, Some("string"))?;
            if finite {
                self.require_allowed_keys(schema, path, &["type", "const", "enum"])?;
                return Ok(());
            }
            self.require_allowed_keys(
                schema,
                path,
                &["type", "minLength", "maxLength", "pattern", "format"],
            )?;
            let maximum = schema
                .get("maxLength")
                .and_then(Value::as_u64)
                .filter(|maximum| *maximum <= MAX_AUDITED_STRING_CHARS)
                .ok_or_else(|| format!("{path}/maxLength: missing or impractical string bound"))?;
            if schema
                .get("minLength")
                .and_then(Value::as_u64)
                .is_some_and(|minimum| minimum > maximum)
            {
                return Err(format!("{path}/minLength: exceeds maxLength"));
            }
            if schema.get("minLength").is_some_and(|value| !value.is_u64()) {
                return Err(format!("{path}/minLength: must be a nonnegative integer"));
            }
            if schema
                .get("pattern")
                .is_some_and(|value| !value.as_str().is_some_and(|pattern| pattern.len() <= 1_024))
            {
                return Err(format!("{path}/pattern: must be a bounded string"));
            }
            if schema.get("format").is_some_and(|value| !value.is_string()) {
                return Err(format!("{path}/format: must be a string"));
            }
            Ok(())
        }

        fn validate_number(
            &mut self,
            schema: &serde_json::Map<String, Value>,
            path: &str,
            kind: &str,
        ) -> Result<(), String> {
            if self.validate_finite_values(schema, path, Some(kind))? {
                self.require_allowed_keys(schema, path, &["type", "const", "enum"])?;
                return Ok(());
            }
            self.require_allowed_keys(
                schema,
                path,
                &[
                    "type",
                    "minimum",
                    "maximum",
                    "exclusiveMinimum",
                    "exclusiveMaximum",
                    "multipleOf",
                ],
            )?;
            let minimum = numeric_boundary(schema, "minimum", "exclusiveMinimum", path)?;
            let maximum = numeric_boundary(schema, "maximum", "exclusiveMaximum", path)?;
            if minimum > maximum {
                return Err(format!("{path}: numeric minimum exceeds maximum"));
            }
            if schema.get("multipleOf").is_some_and(|value| {
                !value
                    .as_f64()
                    .is_some_and(|number| number.is_finite() && number > 0.0)
            }) {
                return Err(format!("{path}/multipleOf: must be finite and positive"));
            }
            Ok(())
        }

        fn validate_scalar_type(
            &self,
            schema: &serde_json::Map<String, Value>,
            path: &str,
            kind: &str,
        ) -> Result<(), String> {
            self.require_allowed_keys(schema, path, &["type", "const", "enum"])?;
            self.validate_finite_values(schema, path, Some(kind))?;
            Ok(())
        }

        fn validate_finite_values(
            &self,
            schema: &serde_json::Map<String, Value>,
            path: &str,
            kind: Option<&str>,
        ) -> Result<bool, String> {
            if schema.contains_key("const") && schema.contains_key("enum") {
                return Err(format!("{path}: const and enum cannot be combined"));
            }
            if let Some(value) = schema.get("const") {
                validate_finite_scalar(value, kind, &format!("{path}/const"))?;
                return Ok(true);
            }
            if let Some(values) = schema.get("enum") {
                let values = values
                    .as_array()
                    .filter(|values| !values.is_empty() && values.len() <= MAX_AUDITED_ENUM_VALUES)
                    .ok_or_else(|| format!("{path}/enum: must be a nonempty bounded array"))?;
                let mut seen = std::collections::HashSet::new();
                for (index, value) in values.iter().enumerate() {
                    validate_finite_scalar(value, kind, &format!("{path}/enum/{index}"))?;
                    if !seen.insert(value.to_string()) {
                        return Err(format!("{path}/enum: duplicate value"));
                    }
                }
                return Ok(true);
            }
            Ok(false)
        }

        fn normalized_type<'schema>(
            &self,
            value: &'schema Value,
            path: &str,
        ) -> Result<&'schema str, String> {
            match value {
                Value::String(kind) => Ok(kind),
                Value::Array(kinds) if kinds.len() == 2 => {
                    let kinds = kinds
                        .iter()
                        .map(|kind| {
                            kind.as_str()
                                .ok_or_else(|| format!("{path}/type: types must be strings"))
                        })
                        .collect::<Result<Vec<_>, _>>()?;
                    if kinds.iter().filter(|kind| **kind == "null").count() != 1 {
                        return Err(format!(
                            "{path}/type: nullable type requires exactly one null branch"
                        ));
                    }
                    kinds
                        .into_iter()
                        .find(|kind| *kind != "null")
                        .ok_or_else(|| format!("{path}/type: missing non-null type"))
                }
                _ => Err(format!(
                    "{path}/type: must be one type or one nullable type pair"
                )),
            }
        }

        fn validate_annotations(
            &self,
            schema: &serde_json::Map<String, Value>,
            path: &str,
        ) -> Result<(), String> {
            if schema.get("$schema").is_some_and(|dialect| {
                path != "#"
                    || dialect.as_str() != Some("https://json-schema.org/draft/2020-12/schema")
            }) {
                return Err(format!("{path}/$schema: invalid or non-root dialect"));
            }
            for keyword in ["title", "description", "$comment"] {
                if schema.get(keyword).is_some_and(|value| {
                    !value
                        .as_str()
                        .is_some_and(|value| value.chars().count() <= MAX_AUDITED_ANNOTATION_CHARS)
                }) {
                    return Err(format!("{path}/{keyword}: invalid annotation"));
                }
            }
            for keyword in ["deprecated", "readOnly", "writeOnly"] {
                if schema.get(keyword).is_some_and(|value| !value.is_boolean()) {
                    return Err(format!("{path}/{keyword}: must be boolean"));
                }
            }
            if schema.get("examples").is_some_and(|value| {
                !value
                    .as_array()
                    .is_some_and(|examples| examples.len() <= 10)
            }) {
                return Err(format!("{path}/examples: invalid annotation"));
            }
            Ok(())
        }

        fn require_allowed_keys(
            &self,
            schema: &serde_json::Map<String, Value>,
            path: &str,
            structural: &[&str],
        ) -> Result<(), String> {
            const ANNOTATIONS: &[&str] = &[
                "$schema",
                "$defs",
                "title",
                "description",
                "$comment",
                "default",
                "examples",
                "deprecated",
                "readOnly",
                "writeOnly",
            ];
            for key in schema.keys() {
                if structural.contains(&key.as_str()) || ANNOTATIONS.contains(&key.as_str()) {
                    continue;
                }
                return Err(format!(
                    "{path}/{key}: keyword is not allowed for this form"
                ));
            }
            if path != "#" && (schema.contains_key("$schema") || schema.contains_key("$defs")) {
                return Err(format!(
                    "{path}: $schema and $defs are permitted only at the root"
                ));
            }
            Ok(())
        }

        fn validate_guarded_node(&mut self, value: &Value, path: &str) -> Result<(), String> {
            self.guard_depth += 1;
            let result = self.validate_node(value, path);
            self.guard_depth -= 1;
            result
        }
    }

    fn audit_schema(root: &Value) -> Result<(), String> {
        SchemaAudit::new(root).validate()
    }

    fn json_pointer_token(value: &str) -> String {
        value.replace('~', "~0").replace('/', "~1")
    }

    fn numeric_boundary(
        schema: &serde_json::Map<String, Value>,
        inclusive: &str,
        exclusive: &str,
        path: &str,
    ) -> Result<f64, String> {
        if schema.contains_key(inclusive) == schema.contains_key(exclusive) {
            return Err(format!(
                "{path}: exactly one of {inclusive} or {exclusive} is required"
            ));
        }
        schema
            .get(inclusive)
            .or_else(|| schema.get(exclusive))
            .and_then(Value::as_f64)
            .filter(|number| number.is_finite() && number.abs() <= MAX_AUDITED_NUMBER_ABS)
            .ok_or_else(|| format!("{path}: missing or impractical numeric boundary"))
    }

    fn validate_finite_scalar(value: &Value, kind: Option<&str>, path: &str) -> Result<(), String> {
        let matches_kind = match (kind, value) {
            (None, Value::Null | Value::Bool(_)) => true,
            (None, Value::String(value)) => {
                value.chars().count() <= MAX_AUDITED_STRING_CHARS as usize
            }
            (None, Value::Number(value)) => value
                .as_f64()
                .is_some_and(|number| number.is_finite() && number.abs() <= MAX_AUDITED_NUMBER_ABS),
            (Some("null"), Value::Null) | (Some("boolean"), Value::Bool(_)) => true,
            (Some("string"), Value::String(value)) => {
                value.chars().count() <= MAX_AUDITED_STRING_CHARS as usize
            }
            (Some("number"), Value::Number(value)) => value
                .as_f64()
                .is_some_and(|number| number.is_finite() && number.abs() <= MAX_AUDITED_NUMBER_ABS),
            (Some("integer"), Value::Number(value)) => value.as_f64().is_some_and(|number| {
                number.is_finite()
                    && number.fract() == 0.0
                    && number.abs() <= MAX_AUDITED_NUMBER_ABS
            }),
            _ => false,
        };
        if matches_kind {
            Ok(())
        } else {
            Err(format!("{path}: value is not a finite {kind:?} scalar"))
        }
    }

    fn assert_catalog_contracts(server: &AnyMcpServer) {
        for tool in server.tools() {
            let input = Value::Object(tool.input_schema.as_ref().clone());
            audit_schema(&input)
                .unwrap_or_else(|error| panic!("{}/inputSchema: {error}", tool.name));
            let output = Value::Object(
                tool.output_schema
                    .as_ref()
                    .unwrap_or_else(|| panic!("{} must declare outputSchema", tool.name))
                    .as_ref()
                    .clone(),
            );
            audit_schema(&output)
                .unwrap_or_else(|error| panic!("{}/outputSchema: {error}", tool.name));

            let name = tool.name.as_ref();
            let is_read =
                READ_TOOL_NAMES.contains(&name) || OPTIONAL_READ_TOOL_NAMES.contains(&name);
            let is_create = name == OBJECT_CREATE || OPTIONAL_CREATE_TOOL_NAMES.contains(&name);
            let is_update = [OBJECT_UPDATE, OBJECT_EDIT, OBJECT_ARCHIVE].contains(&name)
                || OPTIONAL_UPDATE_TOOL_NAMES.contains(&name);
            assert!(
                is_read || is_create || is_update,
                "{name} has no independently reviewed annotation class"
            );
            let expected = if is_read {
                json!({
                    "readOnlyHint": true,
                    "destructiveHint": false,
                    "openWorldHint": false
                })
            } else if is_create {
                json!({
                    "readOnlyHint": false,
                    "destructiveHint": false,
                    "idempotentHint": false,
                    "openWorldHint": false
                })
            } else {
                json!({
                    "readOnlyHint": false,
                    "destructiveHint": true,
                    "idempotentHint": false,
                    "openWorldHint": false
                })
            };
            assert_eq!(
                serde_json::to_value(
                    tool.annotations
                        .as_ref()
                        .unwrap_or_else(|| panic!("{} must declare annotations", tool.name))
                )
                .expect("serialize annotations"),
                expected,
                "{} annotation profile drifted",
                tool.name
            );
        }
    }

    fn write_snapshot(name: &str, contents: &str) {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("snapshots")
            .join(name);
        fs::write(path, contents).expect("write reviewed catalog snapshot");
    }

    async fn write_frame(writer: &mut WriteHalf<DuplexStream>, frame: Value) {
        let mut encoded = serde_json::to_vec(&frame).expect("encode protocol frame");
        encoded.push(b'\n');
        writer
            .write_all(&encoded)
            .await
            .expect("write protocol frame");
        writer.flush().await.expect("flush protocol frame");
    }

    async fn read_frame(reader: &mut BufReader<ReadHalf<DuplexStream>>) -> Value {
        let mut response = String::new();
        tokio::time::timeout(Duration::from_secs(1), reader.read_line(&mut response))
            .await
            .expect("protocol response deadline")
            .expect("read protocol response");
        assert!(
            !response.is_empty(),
            "protocol transport closed unexpectedly"
        );
        serde_json::from_str(&response).expect("valid protocol JSON")
    }

    async fn initialize_protocol(
        reader: &mut BufReader<ReadHalf<DuplexStream>>,
        writer: &mut WriteHalf<DuplexStream>,
    ) {
        write_frame(
            writer,
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": "2025-11-25",
                    "capabilities": {},
                    "clientInfo": {"name": "catalog-test", "version": "0.0.0"}
                }
            }),
        )
        .await;
        let response = read_frame(reader).await;
        assert_eq!(response["id"], 1);
        assert_eq!(response["result"]["protocolVersion"], "2025-11-25");
    }

    async fn negotiate_protocol_version(requested: &str) -> Value {
        let server = AnyMcpServer::new(runtime(false)).expect("static test catalog");
        let (client_transport, server_transport) = duplex(4096);
        let server_task = tokio::spawn(async move {
            server
                .serve(server_transport)
                .await
                .expect("server initializes")
        });
        let (reader, mut writer) = split(client_transport);
        let mut reader = BufReader::new(reader);
        write_frame(
            &mut writer,
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": requested,
                    "capabilities": {},
                    "clientInfo": {"name": "compatibility-test", "version": "0.0.0"}
                }
            }),
        )
        .await;
        let response = read_frame(&mut reader).await;
        drop(writer);
        drop(reader);
        server_task
            .await
            .expect("server task")
            .cancel()
            .await
            .expect("cancel server");
        response["result"]["protocolVersion"].clone()
    }

    async fn request(
        reader: &mut BufReader<ReadHalf<DuplexStream>>,
        writer: &mut WriteHalf<DuplexStream>,
        id: u64,
        method: &str,
        params: Value,
    ) -> Value {
        write_frame(
            writer,
            json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}),
        )
        .await;
        let response = read_frame(reader).await;
        assert_eq!(response["id"], id);
        response
    }

    #[test]
    fn capabilities_are_static_complete_and_never_advertise_list_changed() {
        let server = AnyMcpServer::new(runtime(false)).unwrap();
        let info = server.info();

        assert_eq!(ProtocolVersion::LATEST, ProtocolVersion::V_2025_11_25);
        assert_eq!(info.protocol_version, ProtocolVersion::V_2025_11_25);
        assert_eq!(info.protocol_version.as_str(), "2025-11-25");
        assert_eq!(info.server_info.name, env!("CARGO_PKG_NAME"));
        assert_eq!(info.server_info.version, env!("CARGO_PKG_VERSION"));
        let tools = info.capabilities.tools.expect("tools capability");
        assert_eq!(tools.list_changed, None);
        let resources = info.capabilities.resources.expect("resources capability");
        assert_eq!(resources.list_changed, None);
        assert_eq!(resources.subscribe, None);
    }

    #[tokio::test]
    async fn stable_protocol_negotiates_every_supported_released_revision() {
        for version in ["2024-11-05", "2025-03-26", "2025-06-18", "2025-11-25"] {
            assert_eq!(negotiate_protocol_version(version).await, version);
        }
        assert_eq!(negotiate_protocol_version("2099-99-99").await, "2025-11-25");
    }

    #[test]
    fn profiles_and_read_only_mode_have_exact_canonical_inventories() {
        for (profile, read_only, expected) in [
            (
                ApplicationProfile::Compact,
                false,
                COMPACT_TOOL_NAMES.as_slice(),
            ),
            (
                ApplicationProfile::Compact,
                true,
                COMPACT_READ_TOOL_NAMES.as_slice(),
            ),
            (
                ApplicationProfile::Standard,
                false,
                ALL_TOOL_NAMES.as_slice(),
            ),
            (
                ApplicationProfile::Standard,
                true,
                READ_TOOL_NAMES.as_slice(),
            ),
        ] {
            let server = AnyMcpServer::new(runtime_with_profile(profile, read_only)).unwrap();
            assert_eq!(tool_names(&server), expected);
        }
    }

    #[test]
    fn catalog_entries_equal_the_original_typed_contracts() {
        let server = AnyMcpServer::new(runtime(false)).unwrap();
        let mut expected = vec![
            server_status_tool().unwrap().into_tool(),
            space_list_tool().unwrap().into_tool(),
            type_list_tool().unwrap().into_tool(),
            property_list_tool().unwrap().into_tool(),
            tag_list_tool().unwrap().into_tool(),
            template_list_tool().unwrap().into_tool(),
            object_create_tool().unwrap().into_tool(),
            object_update_tool().unwrap().into_tool(),
            object_edit_tool().unwrap().into_tool(),
            object_archive_tool().unwrap().into_tool(),
        ];
        let cursors = Arc::new(CursorStore::new().unwrap());
        let reads = ObjectReadHandlers::new(runtime(false), cursors.clone()).unwrap();
        expected.extend([
            reads.search_contract().as_tool().clone(),
            reads.get_contract().as_tool().clone(),
        ]);
        let views = ViewReadHandlers::new(runtime(false), cursors).unwrap();
        expected.extend([
            views.view_list_contract().as_tool().clone(),
            views.view_object_list_contract().as_tool().clone(),
        ]);
        expected.sort_by(|left, right| left.name.cmp(&right.name));
        assert_eq!(server.tools(), expected);
        assert!(server.tools().iter().all(|tool| {
            tool.input_schema["additionalProperties"] == json!(false)
                && tool
                    .output_schema
                    .as_ref()
                    .is_some_and(|schema| schema["additionalProperties"] == json!(false))
                && tool.annotations.is_some()
        }));
    }

    #[test]
    fn shared_tool_names_have_identical_contracts_across_profiles() {
        for read_only in [false, true] {
            let compact =
                AnyMcpServer::new(runtime_with_profile(ApplicationProfile::Compact, read_only))
                    .unwrap();
            let standard = AnyMcpServer::new(runtime_with_profile(
                ApplicationProfile::Standard,
                read_only,
            ))
            .unwrap();
            for compact_tool in compact.tools() {
                let standard_tool = standard
                    .tools()
                    .iter()
                    .find(|tool| tool.name == compact_tool.name)
                    .expect("compact tool must exist in standard profile");
                assert_eq!(
                    compact_tool, standard_tool,
                    "{} contract",
                    compact_tool.name
                );
            }
        }
    }

    #[test]
    fn serialized_catalog_snapshots_are_exact_and_deterministic() {
        let normal = catalog_snapshot(ApplicationProfile::Standard, false);
        assert!(
            normal == NORMAL_CATALOG_SNAPSHOT,
            "normal catalog snapshot drifted; review and run the documented explicit updater"
        );
        let read_only = catalog_snapshot(ApplicationProfile::Standard, true);
        assert!(
            read_only == READ_ONLY_CATALOG_SNAPSHOT,
            "read-only catalog snapshot drifted; review and run the documented explicit updater"
        );
        let compact = catalog_snapshot(ApplicationProfile::Compact, false);
        assert!(
            compact == COMPACT_CATALOG_SNAPSHOT,
            "compact catalog snapshot drifted; review and run the documented explicit updater"
        );
        let compact_read_only = catalog_snapshot(ApplicationProfile::Compact, true);
        assert!(
            compact_read_only == COMPACT_READ_ONLY_CATALOG_SNAPSHOT,
            "compact read-only catalog snapshot drifted; review and run the documented explicit updater"
        );
    }

    #[test]
    fn profile_catalogs_and_representative_results_match_reviewed_token_budget() {
        let tokenizer = o200k_base().expect("construct pinned o200k_base tokenizer");
        let representatives: Value = serde_json::from_str(REPRESENTATIVE_RESULTS_SNAPSHOT)
            .expect("reviewed representative results");
        let reviewed: ReviewedTokenBudget =
            serde_json::from_str(TOKEN_BUDGET_SNAPSHOT).expect("reviewed token budget");
        let normal_server = AnyMcpServer::new(runtime(false)).expect("normal static catalog");

        assert_valid_representative(
            &normal_server,
            "object_search",
            &representatives["object_search"],
        );
        assert_valid_representative(&normal_server, "object_get", &representatives["object_get"]);
        let body = &representatives["object_get"]["body"];
        let text = body["text"].as_str().expect("representative body text");
        assert_eq!(
            body["total_chars"].as_u64(),
            Some(text.chars().count() as u64),
            "representative body character count"
        );
        let body_hash = Sha256::digest(text.as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        assert_eq!(
            body["sha256"].as_str(),
            Some(body_hash.as_str()),
            "representative complete-body hash"
        );

        let actual = json!({
            "compact_catalog_tokens": token_count(
                &tokenizer,
                tools_list_value(ApplicationProfile::Compact, false),
            ),
            "compact_read_only_catalog_tokens": token_count(
                &tokenizer,
                tools_list_value(ApplicationProfile::Compact, true),
            ),
            "standard_catalog_tokens": token_count(
                &tokenizer,
                tools_list_value(ApplicationProfile::Standard, false),
            ),
            "standard_read_only_catalog_tokens": token_count(
                &tokenizer,
                tools_list_value(ApplicationProfile::Standard, true),
            ),
            "object_search_result_tokens": token_count(
                &tokenizer,
                representatives["object_search"].clone(),
            ),
            "object_get_result_tokens": token_count(
                &tokenizer,
                representatives["object_get"].clone(),
            ),
        });
        let expected = json!({
            "compact_catalog_tokens": reviewed.compact_catalog_tokens,
            "compact_read_only_catalog_tokens": reviewed.compact_read_only_catalog_tokens,
            "standard_catalog_tokens": reviewed.standard_catalog_tokens,
            "standard_read_only_catalog_tokens": reviewed.standard_read_only_catalog_tokens,
            "object_search_result_tokens": reviewed.object_search_result_tokens,
            "object_get_result_tokens": reviewed.object_get_result_tokens,
        });
        assert_eq!(
            actual, expected,
            "token counts changed; review the complete catalog/result diff and update the audited baseline explicitly"
        );

        assert_eq!(
            reviewed.tokenizer,
            "tiktoken o200k_base (tiktoken-rs 0.12.0)"
        );
        assert_eq!(reviewed.catalog_context_limit_percent, 5);
        assert_eq!(reviewed.material_growth_review_percent, 2);
        let catalog_limit = reviewed
            .smallest_supported_context_tokens
            .checked_mul(reviewed.catalog_context_limit_percent)
            .expect("context percentage multiplication is bounded")
            / 100;
        let material_growth_tokens = reviewed
            .compact_catalog_tokens
            .checked_mul(reviewed.material_growth_review_percent)
            .expect("material growth multiplication is bounded")
            .div_ceil(100);
        assert!(
            reviewed.compact_catalog_tokens < catalog_limit,
            "default catalog must remain below 5% of the internal compatibility-policy context floor: {} >= {}",
            reviewed.compact_catalog_tokens,
            catalog_limit,
        );
        assert!(
            reviewed
                .compact_catalog_tokens
                .checked_add(material_growth_tokens)
                .is_some_and(|review_boundary| review_boundary < catalog_limit),
            "the reviewed material-growth boundary must retain context-limit headroom"
        );
    }

    #[test]
    #[ignore = "diagnostic report for explicit catalog-growth review"]
    fn report_compact_catalog_token_breakdown() {
        let tokenizer = o200k_base().expect("construct pinned o200k_base tokenizer");
        let server = AnyMcpServer::new(runtime_with_profile(ApplicationProfile::Compact, false))
            .expect("compact static catalog");
        let response =
            serde_json::to_value(ListToolsResult::with_all_items(server.tools().to_vec()))
                .expect("serialize complete tools/list result");
        eprintln!(
            "complete_tools_list_result={}",
            token_count(&tokenizer, response)
        );
        for (profile, read_only) in [
            (ApplicationProfile::Compact, false),
            (ApplicationProfile::Compact, true),
            (ApplicationProfile::Standard, false),
            (ApplicationProfile::Standard, true),
        ] {
            eprintln!(
                "profile={} read_only={read_only} complete_tools_list_result={}",
                profile.as_str(),
                token_count(&tokenizer, tools_list_value(profile, read_only)),
            );
        }
        let mut description_total = 0;
        let mut input_total = 0;
        let mut output_total = 0;
        let mut output_removal_total = 0;
        for tool in server.tools() {
            let full_value = serde_json::to_value(tool).expect("serialize production tool");
            let full_tokens = token_count(&tokenizer, full_value.clone());
            let description_tokens = tool.description.as_ref().map_or(0, |description| {
                tokenizer.encode_ordinary(description.as_ref()).len()
            });
            let input_tokens = token_count(
                &tokenizer,
                Value::Object(tool.input_schema.as_ref().clone()),
            );
            let output_tokens = tool.output_schema.as_ref().map_or(0, |schema| {
                token_count(&tokenizer, Value::Object(schema.as_ref().clone()))
            });
            let mut without_description = full_value.clone();
            without_description
                .as_object_mut()
                .expect("serialized tool object")
                .remove("description");
            let description_removal =
                full_tokens.saturating_sub(token_count(&tokenizer, without_description));
            let mut without_output = full_value;
            without_output
                .as_object_mut()
                .expect("serialized tool object")
                .remove("outputSchema");
            let output_removal =
                full_tokens.saturating_sub(token_count(&tokenizer, without_output));
            description_total += description_removal;
            input_total += input_tokens;
            output_total += output_tokens;
            output_removal_total += output_removal;
            eprintln!(
                "{}: full={full_tokens} description_text={description_tokens} description_field_delta={description_removal} input={input_tokens} output={output_tokens} output_field_delta={output_removal}",
                tool.name,
            );
        }
        eprintln!(
            "totals: description_field_delta={description_total} input={input_total} output={output_total} output_field_delta={output_removal_total}"
        );
    }

    #[test]
    fn every_catalog_schema_is_recursively_bounded_and_annotations_are_exact() {
        for profile in [ApplicationProfile::Compact, ApplicationProfile::Standard] {
            for read_only in [false, true] {
                let server = AnyMcpServer::new(runtime_with_profile(profile, read_only))
                    .expect("base static catalog");
                assert_catalog_contracts(&server);
            }
        }
    }

    #[test]
    fn every_all_selected_optional_schema_is_recursively_bounded_and_annotations_are_exact() {
        for profile in [ApplicationProfile::Compact, ApplicationProfile::Standard] {
            for read_only in [false, true] {
                let server =
                    AnyMcpServer::new(runtime_with_all_optional_toolsets(profile, read_only))
                        .expect("all-selected production catalog");
                assert_catalog_contracts(&server);
            }
        }
    }

    #[test]
    fn independent_schema_audit_accepts_local_cycles_refs_and_compositions() {
        let schema = json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "$defs": {
                "Node": {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "next": {
                            "anyOf": [
                                {"$ref": "#/$defs/Node"},
                                {"type": "null"}
                            ]
                        }
                    }
                },
                "Label": {"type": "string", "maxLength": 12},
                "a/b~c": {"type": "boolean"}
            },
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "node": {
                    "$ref": "#/$defs/Node",
                    "description": "A guarded recursive node."
                },
                "choice": {
                    "oneOf": [
                        {"$ref": "#/$defs/Label"},
                        {"type": "integer", "minimum": 0, "maximum": 4}
                    ]
                },
                "both": {
                    "allOf": [
                        {"type": "string", "maxLength": 12},
                        {"type": "string", "minLength": 1, "maxLength": 8}
                    ]
                },
                "maybe": {
                    "type": ["array", "null"],
                    "maxItems": 2,
                    "items": {"$ref": "#/$defs/Label"}
                },
                "escaped": {"$ref": "#/$defs/a~1b~0c"},
                "finite": {"enum": [null, true, "bounded", 3]}
            }
        });

        audit_schema(&schema).expect("finite recursive synthetic schema");
    }

    #[test]
    fn independent_schema_audit_rejects_every_open_or_malformed_form() {
        let cases = vec![
            ("boolean true", json!(true), "true schema"),
            ("non-schema scalar", json!(null), "object or false"),
            ("empty", json!({}), "empty schema"),
            (
                "annotation only",
                json!({"description": "not a constraint"}),
                "exactly one finite const or enum",
            ),
            (
                "unknown type",
                json!({"type": "mystery"}),
                "unknown schema type",
            ),
            (
                "non-string ref",
                json!({"$ref": 7}),
                "reference must be a string",
            ),
            (
                "external ref",
                json!({"$ref": "https://example.invalid/schema"}),
                "local under #/$defs",
            ),
            (
                "dangling ref",
                json!({
                    "$defs": {"Present": {"type": "boolean"}},
                    "$ref": "#/$defs/Missing"
                }),
                "dangling reference",
            ),
            (
                "illegal ref sibling",
                json!({
                    "$defs": {"Text": {"type": "string", "maxLength": 4}},
                    "$ref": "#/$defs/Text",
                    "type": "string"
                }),
                "keyword is not allowed",
            ),
            (
                "unguarded alias cycle",
                json!({
                    "$defs": {"Loop": {"$ref": "#/$defs/Loop"}},
                    "$ref": "#/$defs/Loop"
                }),
                "unguarded cyclic reference",
            ),
            (
                "unguarded composition cycle",
                json!({
                    "$defs": {
                        "Loop": {"allOf": [{"$ref": "#/$defs/Loop"}]}
                    },
                    "$ref": "#/$defs/Loop"
                }),
                "unguarded cyclic reference",
            ),
            (
                "invalid node after guarded cycle",
                json!({
                    "$defs": {
                        "Node": {
                            "type": "object",
                            "additionalProperties": false,
                            "properties": {
                                "next": {"$ref": "#/$defs/Node"},
                                "bad": {}
                            }
                        }
                    },
                    "$ref": "#/$defs/Node"
                }),
                "empty schema",
            ),
            (
                "empty composition",
                json!({"oneOf": []}),
                "at least 2 branches",
            ),
            (
                "malformed composition",
                json!({"allOf": {}}),
                "composition must be an array",
            ),
            (
                "multiple compositions",
                json!({
                    "oneOf": [{"type": "null"}, {"type": "boolean"}],
                    "anyOf": [{"type": "null"}, {"type": "boolean"}]
                }),
                "exactly one composition keyword",
            ),
            (
                "composition sibling",
                json!({
                    "oneOf": [{"type": "null"}, {"type": "boolean"}],
                    "type": "boolean"
                }),
                "keyword is not allowed",
            ),
            (
                "unbounded string",
                json!({"type": "string"}),
                "missing or impractical string bound",
            ),
            (
                "impractical string",
                json!({"type": "string", "maxLength": 100_001}),
                "missing or impractical string bound",
            ),
            (
                "array missing max",
                json!({"type": "array", "items": {"type": "boolean"}}),
                "missing or impractical array bound",
            ),
            (
                "array missing items",
                json!({"type": "array", "maxItems": 1}),
                "array items must be constrained",
            ),
            (
                "impractical array",
                json!({
                    "type": "array",
                    "maxItems": 10_001,
                    "items": {"type": "boolean"}
                }),
                "missing or impractical array bound",
            ),
            (
                "open object",
                json!({"type": "object", "additionalProperties": true}),
                "reject open maps",
            ),
            (
                "object missing closure",
                json!({"type": "object"}),
                "reject open maps",
            ),
            (
                "malformed object minimum",
                json!({
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {},
                    "minProperties": "0"
                }),
                "minProperties: must be a nonnegative integer",
            ),
            (
                "object minimum exceeds maximum",
                json!({
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {"value": {"type": "boolean"}},
                    "minProperties": 1,
                    "maxProperties": 0
                }),
                "minProperties: exceeds maxProperties",
            ),
            (
                "object maximum exceeds fields",
                json!({
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {"value": {"type": "boolean"}},
                    "maxProperties": 2
                }),
                "maxProperties: exceeds declared property count",
            ),
            (
                "number missing bounds",
                json!({"type": "number"}),
                "exactly one of minimum or exclusiveMinimum",
            ),
            (
                "impractical number",
                json!({
                    "type": "number",
                    "minimum": -9_007_199_254_740_992_f64,
                    "maximum": 1
                }),
                "missing or impractical numeric boundary",
            ),
            (
                "non-scalar const",
                json!({"const": {"open": "value"}}),
                "not a finite",
            ),
            (
                "finite const with structural sibling",
                json!({"type": "string", "const": "x", "maxLength": 1}),
                "keyword is not allowed",
            ),
            ("empty enum", json!({"enum": []}), "nonempty bounded array"),
            (
                "unknown keyword",
                json!({"type": "boolean", "additionalProperties": false}),
                "keyword is not allowed",
            ),
            (
                "malformed nullable type",
                json!({"type": ["string", "null", "boolean"], "maxLength": 4}),
                "one nullable type pair",
            ),
            (
                "nested definitions",
                json!({
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "nested": {
                            "$defs": {"Inner": {"type": "boolean"}},
                            "type": "boolean"
                        }
                    }
                }),
                "definitions are root-only",
            ),
            (
                "empty definitions",
                json!({
                    "$defs": {},
                    "type": "object",
                    "additionalProperties": false
                }),
                "nonempty object",
            ),
        ];

        for (name, schema, expected) in cases {
            let error = audit_schema(&schema)
                .expect_err("synthetic open or malformed schema must fail closed");
            assert!(
                error.contains(expected),
                "{name}: expected {expected:?}, got {error:?}"
            );
        }
    }

    #[test]
    #[ignore = "manual updater; review every schema and annotation diff before committing"]
    fn write_catalog_snapshots() {
        write_snapshot(
            "catalog-normal.snap",
            &catalog_snapshot(ApplicationProfile::Standard, false),
        );
        write_snapshot(
            "catalog-read-only.snap",
            &catalog_snapshot(ApplicationProfile::Standard, true),
        );
        write_snapshot(
            "catalog-compact.snap",
            &catalog_snapshot(ApplicationProfile::Compact, false),
        );
        write_snapshot(
            "catalog-compact-read-only.snap",
            &catalog_snapshot(ApplicationProfile::Compact, true),
        );
    }

    #[tokio::test]
    async fn read_only_direct_mutation_dispatch_rejects_before_decode_or_io() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind no-I/O fixture");
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let server = AnyMcpServer::new(runtime_at(base_url, true)).unwrap();

        for name in [OBJECT_CREATE, OBJECT_UPDATE, OBJECT_EDIT, OBJECT_ARCHIVE] {
            let result = server
                .dispatch_tool(CallToolRequestParams::new(name), &CancellationToken::new())
                .await
                .expect("tool-level read-only result");
            assert_eq!(result.is_error, Some(true));
            let structured = result.structured_content.expect("read-only error body");
            assert_eq!(structured["code"], "validation");
            assert!(
                structured["message"]
                    .as_str()
                    .is_some_and(|message| message.contains("read-only"))
            );
        }
        assert!(
            tokio::time::timeout(Duration::from_millis(150), listener.accept())
                .await
                .is_err()
        );
    }

    #[test]
    fn phase_one_route_selection_remains_lazy_until_polled() {
        let server = AnyMcpServer::new(runtime(false)).expect("standard static catalog");
        let cancellation = CancellationToken::new();
        let route = server.dispatch_tool(CallToolRequestParams::new(SPACE_LIST), &cancellation);

        assert_eq!(server.phase1_dispatch_polls(), 0);
        drop(route);
        assert_eq!(server.phase1_dispatch_polls(), 0);
    }

    #[tokio::test]
    async fn compact_omissions_are_unknown_while_read_only_edit_fails_closed() {
        let writable =
            AnyMcpServer::new(runtime_with_profile(ApplicationProfile::Compact, false)).unwrap();
        let error = writable
            .dispatch_tool(
                CallToolRequestParams::new(SPACE_LIST),
                &CancellationToken::new(),
            )
            .await
            .expect_err("standard-only tool is absent from compact");
        assert_eq!(error.code, rmcp::model::ErrorCode::METHOD_NOT_FOUND);

        let read_only =
            AnyMcpServer::new(runtime_with_profile(ApplicationProfile::Compact, true)).unwrap();
        let result = read_only
            .dispatch_tool(
                CallToolRequestParams::new(OBJECT_EDIT),
                &CancellationToken::new(),
            )
            .await
            .expect("selected mutation retains read-only defense in depth");
        assert_eq!(result.is_error, Some(true));
        assert_eq!(result.structured_content.unwrap()["code"], "validation");
    }

    #[tokio::test]
    async fn compact_status_reports_profile_read_only_and_stable_toolsets() {
        let server =
            AnyMcpServer::new(runtime_with_profile(ApplicationProfile::Compact, true)).unwrap();
        let result = server
            .dispatch_tool(
                CallToolRequestParams::new(SERVER_STATUS),
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        let status = result.structured_content.unwrap();
        assert_eq!(status["profile"], "compact");
        assert_eq!(status["read_only"], true);
        assert_eq!(status["enabled_toolsets"], json!(["core", "documents"]));
    }

    #[tokio::test]
    async fn duplex_protocol_exposes_exact_tools_resources_and_clean_eof() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind resource fixture");
        let address = listener.local_addr().expect("resource fixture address");
        let upstream = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("accept resource GET");
            let mut request = Vec::new();
            let mut buffer = [0_u8; 2048];
            loop {
                let read = socket.read(&mut buffer).await.expect("read resource GET");
                assert_ne!(read, 0, "request ended before headers");
                request.extend_from_slice(&buffer[..read]);
                if request.windows(4).any(|window| window == b"\r\n\r\n") {
                    break;
                }
            }
            let body = json!({
                "object": {
                    "archived": false,
                    "id": OBJECT_ID,
                    "space_id": SPACE_ID,
                    "name": "Protocol document",
                    "markdown": "# protocol body",
                    "type": {
                        "archived": false,
                        "id": "bafyreibbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                        "key": "page"
                    }
                }
            })
            .to_string();
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            socket
                .write_all(response.as_bytes())
                .await
                .expect("write resource response");
            String::from_utf8(request).expect("request headers UTF-8")
        });

        let server = AnyMcpServer::new(runtime_at(format!("http://{address}"), false)).unwrap();
        let (server_io, client_io) = tokio::io::duplex(2 * 1024 * 1024);
        let server_task = tokio::spawn(async move { serve_transport(server, server_io).await });
        let (reader, mut writer) = split(client_io);
        let mut reader = BufReader::new(reader);
        initialize_protocol(&mut reader, &mut writer).await;

        let listed = request(&mut reader, &mut writer, 2, "tools/list", json!({})).await;
        let names = listed["result"]["tools"]
            .as_array()
            .expect("tools array")
            .iter()
            .map(|tool| tool["name"].as_str().expect("tool name"))
            .collect::<Vec<_>>();
        assert_eq!(names, ALL_TOOL_NAMES);
        let resources = request(&mut reader, &mut writer, 3, "resources/list", json!({})).await;
        assert_eq!(resources["result"]["resources"], json!([]));
        let templates = request(
            &mut reader,
            &mut writer,
            4,
            "resources/templates/list",
            json!({}),
        )
        .await;
        let templates = templates["result"]["resourceTemplates"]
            .as_array()
            .expect("resource templates");
        assert_eq!(templates.len(), 1);
        assert_eq!(templates[0]["uriTemplate"], OBJECT_RESOURCE_TEMPLATE);
        let read = request(
            &mut reader,
            &mut writer,
            5,
            "resources/read",
            json!({"uri": RESOURCE_URI}),
        )
        .await;
        assert_eq!(read["result"]["contents"][0]["text"], "# protocol body");
        assert_eq!(read["result"]["contents"][0]["uri"], RESOURCE_URI);
        let status = request(
            &mut reader,
            &mut writer,
            6,
            "tools/call",
            json!({"name": SERVER_STATUS, "arguments": {}}),
        )
        .await;
        assert_eq!(status["result"]["isError"], false);
        assert_eq!(
            status["result"]["structuredContent"]["enabled_toolsets"]
                .as_array()
                .map(Vec::len),
            Some(8)
        );

        drop(writer);
        drop(reader);
        server_task
            .await
            .expect("join protocol server")
            .expect("clean server EOF");
        let request = upstream.await.expect("resource fixture");
        assert!(request.starts_with(&format!("GET /v1/spaces/{SPACE_ID}/objects/{OBJECT_ID} ")));
    }

    #[tokio::test]
    async fn duplex_read_only_tools_list_omits_only_mutations() {
        let server = AnyMcpServer::new(runtime(true)).unwrap();
        let (server_io, client_io) = tokio::io::duplex(2 * 1024 * 1024);
        let server_task = tokio::spawn(async move { serve_transport(server, server_io).await });
        let (reader, mut writer) = split(client_io);
        let mut reader = BufReader::new(reader);
        initialize_protocol(&mut reader, &mut writer).await;

        let listed = request(&mut reader, &mut writer, 2, "tools/list", json!({})).await;
        let names = listed["result"]["tools"]
            .as_array()
            .expect("tools array")
            .iter()
            .map(|tool| tool["name"].as_str().expect("tool name"))
            .collect::<Vec<_>>();
        assert_eq!(names, READ_TOOL_NAMES);
        let templates = request(
            &mut reader,
            &mut writer,
            3,
            "resources/templates/list",
            json!({}),
        )
        .await;
        assert_eq!(
            templates["result"]["resourceTemplates"]
                .as_array()
                .map(Vec::len),
            Some(1)
        );

        drop(writer);
        drop(reader);
        server_task
            .await
            .expect("join read-only server")
            .expect("clean read-only EOF");
    }
}
