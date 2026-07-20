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

    async fn dispatch_tool(
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
        reject_static_cursor(request)?;
        Ok(ListToolsResult::with_all_items(self.state.tools.clone()))
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
        self.state.resources.list_resources(request)
    }

    async fn list_resource_templates(
        &self,
        request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourceTemplatesResult, ErrorData> {
        self.state.resources.list_resource_templates(request)
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResult, ErrorData> {
        self.state
            .resources
            .read_resource(request, &context.ct)
            .await
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

    fn assert_schema_node_is_bounded(value: &Value, path: &str) {
        if value == &Value::Bool(false) {
            return;
        }
        let schema = value
            .as_object()
            .unwrap_or_else(|| panic!("{path} must be an object or false schema"));

        if let Some(definitions) = schema.get("$defs").and_then(Value::as_object) {
            for (name, definition) in definitions {
                assert_schema_node_is_bounded(definition, &format!("{path}/$defs/{name}"));
            }
        }
        if schema.contains_key("$ref") {
            return;
        }
        for keyword in ["allOf", "anyOf", "oneOf"] {
            if let Some(branches) = schema.get(keyword).and_then(Value::as_array) {
                for (index, branch) in branches.iter().enumerate() {
                    assert_schema_node_is_bounded(branch, &format!("{path}/{keyword}/{index}"));
                }
            }
        }

        let has_type = |expected: &str| match schema.get("type") {
            Some(Value::String(kind)) => kind == expected,
            Some(Value::Array(kinds)) => kinds.iter().any(|kind| kind == expected),
            _ => false,
        };
        if has_type("object") {
            assert_eq!(
                schema.get("additionalProperties"),
                Some(&Value::Bool(false)),
                "{path} must reject unconstrained map keys"
            );
            if let Some(properties) = schema.get("properties").and_then(Value::as_object) {
                for (name, property) in properties {
                    assert_schema_node_is_bounded(property, &format!("{path}/properties/{name}"));
                }
            }
        }
        if has_type("array") {
            let maximum = schema
                .get("maxItems")
                .and_then(Value::as_u64)
                .unwrap_or_else(|| panic!("{path} array must have maxItems"));
            assert!(maximum <= 10_000, "{path} array bound is impractical");
            let items = schema
                .get("items")
                .unwrap_or_else(|| panic!("{path} array must constrain its items"));
            assert_schema_node_is_bounded(items, &format!("{path}/items"));
        }
        if has_type("string") && !schema.contains_key("const") && !schema.contains_key("enum") {
            let maximum = schema
                .get("maxLength")
                .and_then(Value::as_u64)
                .unwrap_or_else(|| panic!("{path} string must have maxLength"));
            assert!(maximum <= 100_000, "{path} string bound is impractical");
        }
    }

    fn assert_catalog_contracts(server: &AnyMcpServer) {
        for tool in server.tools() {
            assert_schema_node_is_bounded(
                &Value::Object(tool.input_schema.as_ref().clone()),
                &format!("{}/inputSchema", tool.name),
            );
            assert_schema_node_is_bounded(
                &Value::Object(
                    tool.output_schema
                        .as_ref()
                        .unwrap_or_else(|| panic!("{} must declare outputSchema", tool.name))
                        .as_ref()
                        .clone(),
                ),
                &format!("{}/outputSchema", tool.name),
            );

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
