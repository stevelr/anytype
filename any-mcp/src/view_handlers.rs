// any-mcp - bounded, workflow-oriented MCP server for Anytype
//
// SPDX-FileCopyrightText: 2026 Steve Schoettler
// SPDX-License-Identifier: Apache-2.0

//! Bounded view discovery and selected-view object listing workflows.
//!
//! These handlers are transport-neutral. The production tool catalog owns
//! their rmcp routing; this module owns the strict typed contracts,
//! resolver calls, one-page upstream reads, bounded conversion, and cursor
//! integrity checks.

use std::{borrow::Cow, sync::Arc};

use anytype::{
    error::AnytypeError,
    objects::Object,
    paged::PaginatedResponse,
    views::{View, ViewLayout as AnytypeViewLayout},
};
use rmcp::{
    model::CallToolResult,
    schemars::{JsonSchema, Schema, SchemaGenerator, json_schema},
};
use serde::{Deserialize, Deserializer, Serialize, de};
use tokio_util::sync::CancellationToken;

use crate::{
    cursor::{CursorStore, CursorToken},
    domain::{DisplayName, DomainValueError, EntityId, ObjectId, TypeKey},
    handler_support::{
        HandlerError, PageRequest, UpstreamPagination, begin_page, execute_handler, finish_page,
    },
    object_output::{ProjectionMode, normalized_projection_keys, object_output},
    pagination::{Page, PageLimit},
    protocol::{ToolProfile, WorkflowTool, workflow_tool},
    result::tool_error,
    runtime::{OperationContext, RuntimeContext},
    schema::SchemaContractError,
    validation::ProjectionList,
};

/// Maximum characters accepted for a resolvable space or view name/id.
pub const MAX_RESOLVABLE_REFERENCE_CHARS: usize = 512;

/// A nonempty bounded name or identifier resolved by `anytype-api`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct ResolvableReference(String);

impl ResolvableReference {
    /// Validates a caller-supplied name or identifier.
    pub fn new(value: impl Into<String>) -> Result<Self, DomainValueError> {
        let value = value.into();
        if value.is_empty() {
            return Err(DomainValueError::Empty);
        }
        if value.chars().count() > MAX_RESOLVABLE_REFERENCE_CHARS {
            return Err(DomainValueError::TooLong {
                max_chars: MAX_RESOLVABLE_REFERENCE_CHARS,
            });
        }
        Ok(Self(value))
    }

    /// Borrows the validated reference.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for ResolvableReference {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::new(String::deserialize(deserializer)?).map_err(de::Error::custom)
    }
}

impl JsonSchema for ResolvableReference {
    fn schema_name() -> Cow<'static, str> {
        "ResolvableReference".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "minLength": 1,
            "maxLength": MAX_RESOLVABLE_REFERENCE_CHARS,
        })
    }
}

/// Input for one page of views belonging to a collection or query object.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ViewListInput {
    /// Unique space name or identifier.
    pub space: ResolvableReference,
    /// Stable collection or query object identifier.
    pub list_id: ObjectId,
    /// Requested item limit, defaulting to 20.
    #[serde(default)]
    pub limit: PageLimit,
    /// Opaque continuation cursor from a preceding `view_list` call.
    #[serde(default)]
    pub cursor: Option<CursorToken>,
}

/// Input for one page of objects in one resolved collection/query view.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ViewObjectListInput {
    /// Unique space name or identifier.
    pub space: ResolvableReference,
    /// Stable collection or query object identifier.
    pub list_id: ObjectId,
    /// Unique view name or identifier within `list_id`.
    pub view: ResolvableReference,
    /// Optional bounded property keys to project; absence returns summaries only.
    #[serde(default)]
    pub property_keys: Option<ProjectionList<TypeKey>>,
    /// Requested item limit, defaulting to 20.
    #[serde(default)]
    pub limit: PageLimit,
    /// Opaque continuation cursor from a preceding `view_object_list` call.
    #[serde(default)]
    pub cursor: Option<CursorToken>,
}

/// Closed Anytype view layouts exposed by `view_list`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ViewLayout {
    /// Calendar view.
    Calendar,
    /// Gallery view.
    Gallery,
    /// Graph view.
    Graph,
    /// Grid/table view.
    Grid,
    /// Kanban view.
    Kanban,
    /// List view.
    List,
}

impl From<AnytypeViewLayout> for ViewLayout {
    fn from(value: AnytypeViewLayout) -> Self {
        match value {
            AnytypeViewLayout::Calendar => Self::Calendar,
            AnytypeViewLayout::Gallery => Self::Gallery,
            AnytypeViewLayout::Graph => Self::Graph,
            AnytypeViewLayout::Grid => Self::Grid,
            AnytypeViewLayout::Kanban => Self::Kanban,
            AnytypeViewLayout::List => Self::List,
        }
    }
}

/// Bounded metadata for one view of a collection or query.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ViewSummary {
    /// Stable view identifier accepted by `view_object_list`.
    id: EntityId,
    /// Bounded display name; unnamed upstream views use an empty name.
    name: DisplayName,
    /// Closed layout classification.
    layout: ViewLayout,
}

impl ViewSummary {
    /// Returns the stable view identifier.
    #[must_use]
    pub const fn id(&self) -> &EntityId {
        &self.id
    }

    /// Returns the bounded display name.
    #[must_use]
    pub const fn name(&self) -> &DisplayName {
        &self.name
    }

    /// Returns the view layout.
    #[must_use]
    pub const fn layout(&self) -> ViewLayout {
        self.layout
    }
}

/// Typed, transport-neutral view read workflows sharing one runtime and cursor store.
pub struct ViewReadHandlers {
    runtime: RuntimeContext,
    cursors: Arc<CursorStore>,
    view_list_contract: WorkflowTool<Page<ViewSummary>>,
    view_object_list_contract: WorkflowTool<Page<crate::object_output::ObjectOutput>>,
}

impl ViewReadHandlers {
    /// Constructs both strict read-tool contracts consumed by the static catalog.
    ///
    /// # Errors
    ///
    /// Returns an error if either input/output schema violates the global
    /// strict finite-wire contract.
    pub fn new(
        runtime: RuntimeContext,
        cursors: Arc<CursorStore>,
    ) -> Result<Self, SchemaContractError> {
        Ok(Self {
            runtime,
            cursors,
            view_list_contract: workflow_tool::<ViewListInput, Page<ViewSummary>>(
                "view_list",
                "List one bounded page of views for a collection or query; returns no objects.",
                ToolProfile::Read,
            )?,
            view_object_list_contract: workflow_tool::<
                ViewObjectListInput,
                Page<crate::object_output::ObjectOutput>,
            >(
                "view_object_list",
                "List bounded object summaries for one resolved view; bodies are never returned.",
                ToolProfile::Read,
            )?,
        })
    }

    /// Returns the typed `view_list` tool contract used by the static catalog.
    #[must_use]
    pub const fn view_list_contract(&self) -> &WorkflowTool<Page<ViewSummary>> {
        &self.view_list_contract
    }

    /// Returns the typed `view_object_list` contract used by the static catalog.
    #[must_use]
    pub const fn view_object_list_contract(
        &self,
    ) -> &WorkflowTool<Page<crate::object_output::ObjectOutput>> {
        &self.view_object_list_contract
    }

    /// Resolves the space and returns exactly one requested upstream view page.
    pub async fn view_list(
        &self,
        input: ViewListInput,
        cancellation: &CancellationToken,
    ) -> CallToolResult {
        let request = match begin_page(
            &self.cursors,
            input.cursor.as_ref(),
            "view_list",
            input.limit,
            &ViewListBinding {
                space: input.space.as_str(),
                list_id: input.list_id.as_str(),
            },
        ) {
            Ok(request) => request,
            Err(error) => return tool_error(error.tool_error()),
        };

        let client = self.runtime.client();
        execute_handler(
            &self.runtime,
            &self.view_list_contract,
            OperationContext::new("view_list"),
            cancellation,
            async {
                let space_id = client.resolve_space_id(input.space.as_str()).await?;
                client
                    .list_views(&space_id, input.list_id.as_str())
                    .limit(u32::from(input.limit.get()))
                    .offset(request.offset().get())
                    .list()
                    .await
                    .map(anytype::paged::PagedResult::into_response)
            },
            |response| async move { convert_view_page(&self.cursors, request, response) },
        )
        .await
    }

    /// Resolves the space and view, sets the resolved view on the API builder,
    /// and returns exactly one requested upstream object page.
    pub async fn view_object_list(
        &self,
        input: ViewObjectListInput,
        cancellation: &CancellationToken,
    ) -> CallToolResult {
        let projection = input
            .property_keys
            .as_ref()
            .map_or(&[][..], |keys| keys.as_slice());
        let normalized_projection = match normalized_projection_keys(projection) {
            Ok(keys) => keys,
            Err(error) => return tool_error(&error.tool_error()),
        };
        let request = match begin_page(
            &self.cursors,
            input.cursor.as_ref(),
            "view_object_list",
            input.limit,
            &ViewObjectListBinding {
                space: input.space.as_str(),
                list_id: input.list_id.as_str(),
                view: input.view.as_str(),
                property_keys: &normalized_projection,
            },
        ) {
            Ok(request) => request,
            Err(error) => return tool_error(error.tool_error()),
        };

        let client = self.runtime.client();
        execute_handler(
            &self.runtime,
            &self.view_object_list_contract,
            OperationContext::new("view_object_list"),
            cancellation,
            async {
                let space_id = client.resolve_space_id(input.space.as_str()).await?;
                let view_id = client
                    .resolve_view_id(&space_id, input.list_id.as_str(), input.view.as_str())
                    .await?;
                let view_id = EntityId::new(view_id).map_err(|_| AnytypeError::Other {
                    message: "resolved view identifier is unsafe".to_owned(),
                })?;
                client
                    .view_list_objects(&space_id, input.list_id.as_str())
                    .view(view_id.as_str())
                    .limit(u32::from(input.limit.get()))
                    .offset(request.offset().get())
                    .list()
                    .await
                    .map(anytype::paged::PagedResult::into_response)
            },
            |response| async move {
                convert_view_object_page(&self.cursors, request, response, projection)
            },
        )
        .await
    }
}

#[derive(Serialize)]
struct ViewListBinding<'a> {
    space: &'a str,
    list_id: &'a str,
}

#[derive(Serialize)]
struct ViewObjectListBinding<'a> {
    space: &'a str,
    list_id: &'a str,
    view: &'a str,
    property_keys: &'a [TypeKey],
}

fn convert_view_page(
    cursors: &CursorStore,
    request: PageRequest,
    response: PaginatedResponse<View>,
) -> Result<Page<ViewSummary>, HandlerError> {
    let pagination = UpstreamPagination::try_from(&response.pagination)?;
    let items = response
        .items
        .into_iter()
        .map(|view| {
            Ok(ViewSummary {
                id: EntityId::new(view.id)
                    .map_err(|_| HandlerError::new(crate::error::ToolError::upstream()))?,
                name: DisplayName::new(view.name.unwrap_or_default())
                    .map_err(|_| HandlerError::new(crate::error::ToolError::upstream()))?,
                layout: view.layout.into(),
            })
        })
        .collect::<Result<Vec<_>, HandlerError>>()?;
    finish_page(cursors, request, pagination, items)
}

fn convert_view_object_page(
    cursors: &CursorStore,
    request: PageRequest,
    response: PaginatedResponse<Object>,
    property_keys: &[TypeKey],
) -> Result<Page<crate::object_output::ObjectOutput>, HandlerError> {
    let pagination = UpstreamPagination::try_from(&response.pagination)?;
    let mode = if property_keys.is_empty() {
        ProjectionMode::SummaryOnly
    } else {
        ProjectionMode::Selected(property_keys)
    };
    let items = response
        .items
        .iter()
        .map(|object| object_output(object, mode).map_err(HandlerError::from))
        .collect::<Result<Vec<_>, _>>()?;
    finish_page(cursors, request, pagination, items)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use anytype::{
        keystore::HttpCredentials,
        paged::PaginationMeta,
        prelude::{AnytypeClient, ClientConfig},
    };
    use serde_json::{Value, json};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::*;
    use crate::{
        error::ToolErrorCode,
        runtime::StartupStatus,
        schema::{input_schema, output_schema},
    };

    const SPACE_ID: &str = "bafyreiaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const LIST_ID: &str = "bafyreicccccccccccccccccccccccccccccccccccccccccccccccccccc";

    fn fixture_client(base_url: String) -> AnytypeClient {
        let mut config = ClientConfig::default().app_name("mcp-view-fixture");
        config.base_url = Some(base_url);
        config.keystore = Some("env".to_owned());
        let client = AnytypeClient::with_config(config).expect("fixture client");
        client.set_api_key(HttpCredentials::new("fixture-token"));
        client
    }

    fn runtime(client: AnytypeClient) -> RuntimeContext {
        RuntimeContext::from_parts(
            client,
            2,
            Duration::from_secs(5),
            StartupStatus {
                http_available: true,
                grpc_available: false,
            },
        )
    }

    async fn fixture_server(bodies: Vec<Value>) -> (String, tokio::task::JoinHandle<Vec<String>>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind fixture server");
        let address = listener.local_addr().expect("fixture address");
        let task = tokio::spawn(async move {
            let mut requests = Vec::with_capacity(bodies.len());
            for value in bodies {
                let (mut stream, _) = listener.accept().await.expect("accept fixture request");
                let request = read_request(&mut stream).await;
                let body = serde_json::to_string(&value).expect("fixture JSON");
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream
                    .write_all(response.as_bytes())
                    .await
                    .expect("write fixture response");
                requests.push(request);
            }
            if let Ok(Ok((mut stream, _))) =
                tokio::time::timeout(Duration::from_millis(100), listener.accept()).await
            {
                requests.push(read_request(&mut stream).await);
                stream
                    .write_all(
                        b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                    )
                    .await
                    .expect("write unexpected-request response");
            }
            requests
        });
        (format!("http://{address}"), task)
    }

    async fn read_request(stream: &mut tokio::net::TcpStream) -> String {
        let mut request = Vec::new();
        let mut buffer = [0_u8; 2048];
        loop {
            let read = stream
                .read(&mut buffer)
                .await
                .expect("read fixture request");
            if read == 0 {
                break;
            }
            request.extend_from_slice(&buffer[..read]);
            if request.windows(4).any(|window| window == b"\r\n\r\n") {
                break;
            }
        }
        String::from_utf8(request).expect("HTTP request is UTF-8")
    }

    fn page(items: Value, limit: u32, offset: u32, has_more: bool) -> Value {
        json!({
            "items": items,
            "pagination": {
                "has_more": has_more,
                "limit": limit,
                "offset": offset,
                "total": 1
            }
        })
    }

    fn view(id: &str, name: &str) -> Value {
        json!({"filters": [], "id": id, "layout": "grid", "name": name, "sorts": []})
    }

    fn object(id: &str) -> Value {
        json!({
            "archived": false,
            "id": id,
            "layout": "basic",
            "markdown": "# must not escape",
            "name": "Roadmap item",
            "object": "object",
            "properties": [
                {"name": "Status", "key": "status", "id": "property-1", "format": "text", "text": "Ready"},
                {"name": "Secret", "key": "secret", "id": "property-2", "format": "text", "text": "Secret projected"},
                {"name": "Hidden", "key": "hidden", "id": "property-3", "format": "text", "text": "not requested"}
            ],
            "snippet": "must not escape",
            "space_id": SPACE_ID,
            "type": {
                "archived": false,
                "id": "type-1",
                "key": "page",
                "layout": "basic",
                "name": "Page",
                "properties": []
            }
        })
    }

    fn view_list_input(limit: u16) -> ViewListInput {
        ViewListInput {
            space: ResolvableReference::new(SPACE_ID).unwrap(),
            list_id: ObjectId::new(LIST_ID).unwrap(),
            limit: PageLimit::new(limit).unwrap(),
            cursor: None,
        }
    }

    fn view_object_input(view: &str, limit: u16) -> ViewObjectListInput {
        ViewObjectListInput {
            space: ResolvableReference::new(SPACE_ID).unwrap(),
            list_id: ObjectId::new(LIST_ID).unwrap(),
            view: ResolvableReference::new(view).unwrap(),
            property_keys: Some(
                ProjectionList::new(vec![TypeKey::new("status").unwrap()]).unwrap(),
            ),
            limit: PageLimit::new(limit).unwrap(),
            cursor: None,
        }
    }

    #[test]
    fn references_and_tool_schemas_are_strict_and_bounded() {
        assert!(ResolvableReference::new("").is_err());
        assert!(ResolvableReference::new("x".repeat(MAX_RESOLVABLE_REFERENCE_CHARS + 1)).is_err());
        assert!(input_schema::<ViewListInput>().is_ok());
        assert!(input_schema::<ViewObjectListInput>().is_ok());
        assert!(output_schema::<Page<ViewSummary>>().is_ok());
        assert!(output_schema::<Page<crate::object_output::ObjectOutput>>().is_ok());

        let handlers = ViewReadHandlers::new(
            runtime(fixture_client("http://127.0.0.1:1".to_owned())),
            Arc::new(CursorStore::new().unwrap()),
        )
        .unwrap();
        for tool in [
            handlers.view_list_contract().as_tool(),
            handlers.view_object_list_contract().as_tool(),
        ] {
            assert_eq!(
                tool.annotations.as_ref().unwrap().read_only_hint,
                Some(true)
            );
            assert_eq!(
                tool.annotations.as_ref().unwrap().open_world_hint,
                Some(false)
            );
        }
    }

    #[tokio::test]
    async fn view_list_continues_one_page_per_call_and_rejects_mismatched_cursor_without_io() {
        let (base_url, server) = fixture_server(vec![
            page(json!([view("view-1", "Roadmap")]), 2, 0, true),
            page(json!([view("view-2", "Backlog")]), 2, 2, false),
        ])
        .await;
        let handlers = ViewReadHandlers::new(
            runtime(fixture_client(base_url)),
            Arc::new(CursorStore::new().unwrap()),
        )
        .unwrap();

        let result = handlers
            .view_list(view_list_input(2), &CancellationToken::new())
            .await;
        assert_eq!(result.is_error, Some(false));
        let encoded = result.structured_content.as_ref().unwrap();
        assert_eq!(
            encoded["items"],
            json!([{"id":"view-1","name":"Roadmap","layout":"grid"}])
        );
        let cursor = CursorToken::new(encoded["next_cursor"].as_str().unwrap()).unwrap();

        let mut second_input = view_list_input(2);
        second_input.cursor = Some(cursor.clone());
        let second = handlers
            .view_list(second_input, &CancellationToken::new())
            .await;
        assert_eq!(second.is_error, Some(false));
        assert_eq!(
            second.structured_content.as_ref().unwrap(),
            &json!({"items":[{"id":"view-2","name":"Backlog","layout":"grid"}]})
        );

        let mut mismatch = view_list_input(2);
        mismatch.list_id = ObjectId::new("different-list").unwrap();
        mismatch.cursor = Some(cursor);
        let mismatch = handlers
            .view_list(mismatch, &CancellationToken::new())
            .await;
        assert_eq!(mismatch.is_error, Some(true));
        assert_eq!(mismatch.structured_content.unwrap()["code"], "validation");

        let requests = server.await.unwrap();
        assert_eq!(requests.len(), 2);
        let first = requests[0].lines().next().unwrap();
        assert!(first.starts_with(&format!("GET /v1/spaces/{SPACE_ID}/lists/{LIST_ID}/views?")));
        assert!(first.contains("limit=2"));
        assert!(!first.contains("offset="));
        let second = requests[1].lines().next().unwrap();
        assert!(second.starts_with(&format!("GET /v1/spaces/{SPACE_ID}/lists/{LIST_ID}/views?")));
        assert!(second.contains("limit=2"));
        assert!(second.contains("offset=2"));
    }

    #[tokio::test]
    async fn view_object_list_sets_resolved_view_and_continues_with_normalized_projection() {
        let (base_url, server) = fixture_server(vec![
            page(json!([view("view-1", "Roadmap")]), 99, 0, false),
            page(json!([object("object-1")]), 2, 0, true),
            page(json!([view("view-1", "Roadmap")]), 99, 0, false),
            page(json!([object("object-2")]), 2, 2, false),
        ])
        .await;
        let handlers = ViewReadHandlers::new(
            runtime(fixture_client(base_url)),
            Arc::new(CursorStore::new().unwrap()),
        )
        .unwrap();

        let mut first_input = view_object_input("Roadmap", 2);
        first_input.property_keys = Some(
            ProjectionList::new(vec![
                TypeKey::new("secret").unwrap(),
                TypeKey::new("status").unwrap(),
            ])
            .unwrap(),
        );
        let result = handlers
            .view_object_list(first_input, &CancellationToken::new())
            .await;
        assert_eq!(result.is_error, Some(false));
        let encoded = result.structured_content.unwrap();
        assert_eq!(encoded["items"][0]["summary"]["id"], "object-1");
        assert_eq!(
            encoded["items"][0]["summary"]["resource_uri"],
            format!("anytype://spaces/{SPACE_ID}/objects/object-1")
        );
        assert_eq!(encoded["items"][0]["properties"][0]["key"], "secret");
        assert_eq!(encoded["items"][0]["properties"][1]["key"], "status");
        let cursor = CursorToken::new(encoded["next_cursor"].as_str().unwrap()).unwrap();
        let rendered = encoded.to_string();
        assert!(!rendered.contains("must not escape"));
        assert!(!rendered.contains("not requested"));

        let mut second_input = view_object_input("Roadmap", 2);
        second_input.property_keys = Some(
            ProjectionList::new(vec![
                TypeKey::new("status").unwrap(),
                TypeKey::new("secret").unwrap(),
            ])
            .unwrap(),
        );
        second_input.cursor = Some(cursor.clone());
        let second = handlers
            .view_object_list(second_input, &CancellationToken::new())
            .await;
        assert_eq!(second.is_error, Some(false));
        let second = second.structured_content.unwrap();
        assert_eq!(second["items"][0]["summary"]["id"], "object-2");
        assert!(second.get("next_cursor").is_none());

        let mut mismatch = view_object_input("Other", 2);
        mismatch.property_keys = Some(
            ProjectionList::new(vec![
                TypeKey::new("secret").unwrap(),
                TypeKey::new("status").unwrap(),
            ])
            .unwrap(),
        );
        mismatch.cursor = Some(cursor);
        let mismatch = handlers
            .view_object_list(mismatch, &CancellationToken::new())
            .await;
        assert_eq!(mismatch.is_error, Some(true));
        assert_eq!(mismatch.structured_content.unwrap()["code"], "validation");

        let requests = server.await.unwrap();
        assert_eq!(requests.len(), 4);
        assert!(requests[0].starts_with(&format!(
            "GET /v1/spaces/{SPACE_ID}/lists/{LIST_ID}/views?limit=99 HTTP/1.1"
        )));
        let first_page = requests[1].lines().next().unwrap();
        assert!(first_page.starts_with(&format!(
            "GET /v1/spaces/{SPACE_ID}/lists/{LIST_ID}/views/view-1/objects?"
        )));
        assert!(first_page.contains("limit=2"));
        assert!(!first_page.contains("offset="));
        assert!(requests[2].starts_with(&format!(
            "GET /v1/spaces/{SPACE_ID}/lists/{LIST_ID}/views?limit=99 HTTP/1.1"
        )));
        let second_page = requests[3].lines().next().unwrap();
        assert!(second_page.starts_with(&format!(
            "GET /v1/spaces/{SPACE_ID}/lists/{LIST_ID}/views/view-1/objects?"
        )));
        assert!(second_page.contains("limit=2"));
        assert!(second_page.contains("offset=2"));
    }

    #[tokio::test]
    async fn ambiguous_view_name_returns_actionable_bounded_candidates() {
        let (base_url, server) = fixture_server(vec![page(
            json!([view("view-a", "Roadmap"), view("view-b", "Roadmap")]),
            99,
            0,
            false,
        )])
        .await;
        let handlers = ViewReadHandlers::new(
            runtime(fixture_client(base_url)),
            Arc::new(CursorStore::new().unwrap()),
        )
        .unwrap();

        let result = handlers
            .view_object_list(view_object_input("Roadmap", 2), &CancellationToken::new())
            .await;
        assert_eq!(result.is_error, Some(true));
        let error = result.structured_content.unwrap();
        assert_eq!(error["code"], "ambiguous");
        assert_eq!(error["candidates"].as_array().unwrap().len(), 2);
        assert_eq!(error["candidates"][0]["id"], "view-a");
        assert_eq!(error["candidates"][1]["id"], "view-b");
        assert_eq!(server.await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn unsafe_unique_resolved_view_id_is_upstream_error_without_object_request() {
        let (base_url, server) = fixture_server(vec![page(
            json!([view("../private?token=secret", "Roadmap")]),
            99,
            0,
            false,
        )])
        .await;
        let handlers = ViewReadHandlers::new(
            runtime(fixture_client(base_url)),
            Arc::new(CursorStore::new().unwrap()),
        )
        .unwrap();

        let result = handlers
            .view_object_list(view_object_input("Roadmap", 2), &CancellationToken::new())
            .await;
        assert_eq!(result.is_error, Some(true));
        let error = result.structured_content.unwrap();
        assert_eq!(error["code"], "upstream");
        let rendered = error.to_string();
        assert!(!rendered.contains("private"));
        assert!(!rendered.contains("secret"));

        let requests = server.await.unwrap();
        assert_eq!(requests.len(), 1);
        assert!(requests[0].starts_with(&format!(
            "GET /v1/spaces/{SPACE_ID}/lists/{LIST_ID}/views?limit=99 HTTP/1.1"
        )));
    }

    #[test]
    fn view_page_rejects_metadata_mismatch_and_over_limit_before_cursor_issue() {
        let cursors = CursorStore::new().unwrap();
        let input = view_list_input(2);
        let request = begin_page(
            &cursors,
            None,
            "view_list",
            input.limit,
            &ViewListBinding {
                space: input.space.as_str(),
                list_id: input.list_id.as_str(),
            },
        )
        .unwrap();
        let mismatch = PaginatedResponse {
            items: vec![],
            pagination: PaginationMeta {
                has_more: true,
                limit: 2,
                offset: 1,
                total: 3,
            },
        };
        assert_eq!(
            convert_view_page(&cursors, request, mismatch)
                .unwrap_err()
                .tool_error()
                .code(),
            ToolErrorCode::Upstream
        );
        assert_eq!(cursors.entry_count(), 0);

        let request = begin_page(
            &cursors,
            None,
            "view_list",
            input.limit,
            &ViewListBinding {
                space: input.space.as_str(),
                list_id: input.list_id.as_str(),
            },
        )
        .unwrap();
        let too_many = PaginatedResponse {
            items: (0..=input.limit.get())
                .map(|index| View {
                    filters: vec![],
                    id: format!("view-{index}"),
                    layout: AnytypeViewLayout::Grid,
                    name: Some(format!("View {index}")),
                    sorts: vec![],
                })
                .collect(),
            pagination: PaginationMeta {
                has_more: true,
                limit: u32::from(input.limit.get()),
                offset: 0,
                total: 3,
            },
        };
        assert_eq!(
            convert_view_page(&cursors, request, too_many)
                .unwrap_err()
                .tool_error()
                .code(),
            ToolErrorCode::BoundedResult
        );
        assert_eq!(cursors.entry_count(), 0);
    }
}
