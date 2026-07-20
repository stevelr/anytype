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
    protocol::WorkflowTool,
    resources::AnytypeResources,
    result::tool_error,
    runtime::RuntimeContext,
    schema::SchemaContractError,
    view_handlers::{ViewListInput, ViewObjectListInput, ViewReadHandlers},
};

/// Upcoming MCP protocol revision advertised by `any-mcp`.
///
/// `rmcp` 2.2.0 models this draft revision ahead of its stable default, so the
/// server selects it explicitly to follow the SDK's upcoming API direction.
pub const PROTOCOL_VERSION: ProtocolVersion = ProtocolVersion::V_2026_07_28;

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
    /// Builds the complete static Phase 1 catalog over one authenticated runtime.
    ///
    /// Read-only runtime configuration omits every mutating tool while retaining
    /// all read tools and resources. Catalog construction validates the exact
    /// canonical inventory and refuses duplicate or disconnected contracts.
    ///
    /// # Errors
    ///
    /// Returns [`ServerBuildError`] if a typed schema, cursor store, or exact
    /// static inventory cannot be constructed safely.
    pub fn new(runtime: RuntimeContext) -> Result<Self, ServerBuildError> {
        let cursors = Arc::new(CursorStore::new().map_err(ServerBuildError::cursor)?);
        let discovery = DiscoveryHandlers::new(runtime.clone(), cursors.clone());
        let object_read = ObjectReadHandlers::new(runtime.clone(), cursors.clone())
            .map_err(ServerBuildError::schema)?;
        let view_read =
            ViewReadHandlers::new(runtime.clone(), cursors).map_err(ServerBuildError::schema)?;
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
        let access = if runtime.is_read_only() {
            MutationAccess::ReadOnly
        } else {
            tools.extend([
                object_create_tool()
                    .map_err(ServerBuildError::schema)?
                    .into_tool(),
                object_update_contract.as_tool().clone(),
                object_edit_contract.as_tool().clone(),
                object_archive_contract.as_tool().clone(),
            ]);
            MutationAccess::Allowed
        };
        tools.sort_by(|left, right| left.name.cmp(&right.name));
        validate_catalog(&tools, runtime.is_read_only())?;

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
                resources: AnytypeResources::new(runtime),
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

    pub(crate) async fn dispatch_tool(
        &self,
        request: CallToolRequestParams,
        cancellation: &tokio_util::sync::CancellationToken,
    ) -> Result<CallToolResult, ErrorData> {
        if request.task.is_some() {
            return Err(invalid_arguments());
        }
        let arguments = request.arguments;
        match request.name.as_ref() {
            SERVER_STATUS => {
                let input = decode_arguments::<ServerStatusInput>(arguments)?;
                Ok(self.discovery().server_status(input, cancellation).await)
            }
            SPACE_LIST => {
                let input = decode_arguments::<SpaceListInput>(arguments)?;
                Ok(self.discovery().space_list(input, cancellation).await)
            }
            TYPE_LIST => {
                let input = decode_arguments::<TypeListInput>(arguments)?;
                Ok(self.discovery().type_list(input, cancellation).await)
            }
            PROPERTY_LIST => {
                let input = decode_arguments::<PropertyListInput>(arguments)?;
                Ok(self.discovery().property_list(input, cancellation).await)
            }
            TAG_LIST => {
                let input = decode_arguments::<TagListInput>(arguments)?;
                Ok(self.discovery().tag_list(input, cancellation).await)
            }
            TEMPLATE_LIST => {
                let input = decode_arguments::<TemplateListInput>(arguments)?;
                Ok(self.discovery().template_list(input, cancellation).await)
            }
            OBJECT_SEARCH => {
                let input = decode_arguments::<ObjectSearchInput>(arguments)?;
                Ok(self
                    .state
                    .object_read
                    .object_search(input, cancellation)
                    .await)
            }
            OBJECT_GET => {
                let input = decode_arguments::<ObjectGetInput>(arguments)?;
                Ok(self.state.object_read.object_get(input, cancellation).await)
            }
            VIEW_LIST => {
                let input = decode_arguments::<ViewListInput>(arguments)?;
                Ok(self.state.view_read.view_list(input, cancellation).await)
            }
            VIEW_OBJECT_LIST => {
                let input = decode_arguments::<ViewObjectListInput>(arguments)?;
                Ok(self
                    .state
                    .view_read
                    .view_object_list(input, cancellation)
                    .await)
            }
            OBJECT_CREATE => {
                if let Some(error) = self.reject_read_only_mutation() {
                    return Ok(error);
                }
                let input = decode_arguments::<ObjectCreateInput>(arguments)?;
                Ok(self
                    .state
                    .object_create
                    .object_create(self.state.access, input, cancellation)
                    .await)
            }
            OBJECT_UPDATE => {
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
            }
            OBJECT_EDIT => {
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
            }
            OBJECT_ARCHIVE => {
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
            }
            _ => Err(ErrorData::method_not_found::<CallToolRequestMethod>()),
        }
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
        self.state.resources.list_resources(request)
    }

    pub(crate) fn list_resource_templates_wire(
        &self,
        request: Option<PaginatedRequestParams>,
    ) -> Result<ListResourceTemplatesResult, ErrorData> {
        self.state.resources.list_resource_templates(request)
    }

    pub(crate) async fn read_resource_wire(
        &self,
        request: ReadResourceRequestParams,
        cancellation: &tokio_util::sync::CancellationToken,
    ) -> Result<ReadResourceResult, ErrorData> {
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
        self.dispatch_tool(request, &context.ct).await
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

fn decode_arguments<T: DeserializeOwned>(
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

fn validate_catalog(tools: &[Tool], read_only: bool) -> Result<(), ServerBuildError> {
    let expected: &[&str] = if read_only {
        &READ_TOOL_NAMES
    } else {
        &ALL_TOOL_NAMES
    };
    if tools.len() != expected.len()
        || tools
            .iter()
            .map(|tool| tool.name.as_ref())
            .ne(expected.iter().copied())
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
mod tests {
    use std::{fs, path::Path, time::Duration};

    use anytype::prelude::{AnytypeClient, ClientConfig, HttpCredentials};
    use serde_json::{Value, json};
    use tokio::{
        io::{
            AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader, DuplexStream, ReadHalf,
            WriteHalf, split,
        },
        net::TcpListener,
    };
    use tokio_util::sync::CancellationToken;

    use super::*;
    use crate::{
        resources::OBJECT_RESOURCE_TEMPLATE,
        runtime::{StartupStatus, serve_transport},
    };

    const SPACE_ID: &str =
        "bafyreid5fvqlnsobih2keakcxjrrlpmly6kf37klzjzen4ibfdgalcdp4y.2tq5w93cr6oe7";
    const OBJECT_ID: &str = "bafyreie6n5l5nkbjal37su54cha4coy7qzuhrnajluzv5qd5jvtsrxkequ";
    const RESOURCE_URI: &str = "anytype://spaces/bafyreid5fvqlnsobih2keakcxjrrlpmly6kf37klzjzen4ibfdgalcdp4y.2tq5w93cr6oe7/objects/bafyreie6n5l5nkbjal37su54cha4coy7qzuhrnajluzv5qd5jvtsrxkequ";
    const NORMAL_CATALOG_SNAPSHOT: &str = include_str!("../tests/snapshots/catalog-normal.json");
    const READ_ONLY_CATALOG_SNAPSHOT: &str =
        include_str!("../tests/snapshots/catalog-read-only.json");

    fn runtime(read_only: bool) -> RuntimeContext {
        runtime_at("http://127.0.0.1:1".to_owned(), read_only)
    }

    fn runtime_at(base_url: String, read_only: bool) -> RuntimeContext {
        let client = AnytypeClient::with_config(ClientConfig {
            base_url: Some(base_url),
            keystore: Some("env".to_string()),
            keystore_service: Some("any-mcp-server-test".to_string()),
            app_name: "any-mcp-server-test".to_string(),
            ..ClientConfig::default()
        })
        .expect("in-memory test client");
        client.set_api_key(HttpCredentials::new("fixture-token"));
        RuntimeContext::from_parts_with_read_only(
            client,
            1,
            Duration::from_secs(1),
            StartupStatus {
                http_available: true,
                grpc_available: false,
            },
            read_only,
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

    fn catalog_snapshot(read_only: bool) -> String {
        let server = AnyMcpServer::new(runtime(read_only)).expect("static catalog");
        let value = canonical_json(json!({
            "read_only": read_only,
            "tools": server.tools(),
        }));
        format!(
            "{}\n",
            serde_json::to_string_pretty(&value).expect("serialize static catalog")
        )
    }

    const MAX_AUDITED_STRING_CHARS: u64 = 100_000;
    const MAX_AUDITED_ARRAY_ITEMS: u64 = 10_000;
    const MAX_AUDITED_ENUM_VALUES: usize = 128;
    const MAX_AUDITED_NUMBER_ABS: f64 = 1_000_000_000_000_000.0;
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
                self.require_allowed_keys(schema, path, &[keyword])?;
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
                &["type", "properties", "required", "additionalProperties"],
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

            let expected = if READ_TOOL_NAMES.contains(&tool.name.as_ref()) {
                json!({
                    "readOnlyHint": true,
                    "destructiveHint": false,
                    "openWorldHint": false
                })
            } else if tool.name == OBJECT_CREATE {
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
                    "protocolVersion": "2026-07-28",
                    "capabilities": {},
                    "clientInfo": {"name": "catalog-test", "version": "0.0.0"}
                }
            }),
        )
        .await;
        let response = read_frame(reader).await;
        assert_eq!(response["id"], 1);
        assert_eq!(response["result"]["protocolVersion"], "2026-07-28");
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

        assert_eq!(info.protocol_version, ProtocolVersion::V_2026_07_28);
        assert_eq!(info.protocol_version.as_str(), "2026-07-28");
        assert_eq!(info.server_info.name, env!("CARGO_PKG_NAME"));
        assert_eq!(info.server_info.version, env!("CARGO_PKG_VERSION"));
        let tools = info.capabilities.tools.expect("tools capability");
        assert_eq!(tools.list_changed, None);
        let resources = info.capabilities.resources.expect("resources capability");
        assert_eq!(resources.list_changed, None);
        assert_eq!(resources.subscribe, None);
    }

    #[test]
    fn normal_and_read_only_catalogs_have_exact_canonical_inventories() {
        let normal = AnyMcpServer::new(runtime(false)).unwrap();
        assert_eq!(tool_names(&normal), ALL_TOOL_NAMES);
        let read_only = AnyMcpServer::new(runtime(true)).unwrap();
        assert_eq!(tool_names(&read_only), READ_TOOL_NAMES);
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
    fn serialized_catalog_snapshots_are_exact_and_deterministic() {
        let normal = catalog_snapshot(false);
        assert!(
            normal == NORMAL_CATALOG_SNAPSHOT,
            "normal catalog snapshot drifted; review and run the documented explicit updater"
        );
        let read_only = catalog_snapshot(true);
        assert!(
            read_only == READ_ONLY_CATALOG_SNAPSHOT,
            "read-only catalog snapshot drifted; review and run the documented explicit updater"
        );
    }

    #[test]
    fn every_catalog_schema_is_recursively_bounded_and_annotations_are_exact() {
        let normal = AnyMcpServer::new(runtime(false)).expect("normal static catalog");
        assert_catalog_contracts(&normal);
        let read_only = AnyMcpServer::new(runtime(true)).expect("read-only static catalog");
        assert_catalog_contracts(&read_only);
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
                "number missing bounds",
                json!({"type": "number"}),
                "exactly one of minimum or exclusiveMinimum",
            ),
            (
                "impractical number",
                json!({
                    "type": "number",
                    "minimum": -1_000_000_000_000_001_f64,
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
        write_snapshot("catalog-normal.json", &catalog_snapshot(false));
        write_snapshot("catalog-read-only.json", &catalog_snapshot(true));
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
            Some(1)
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
