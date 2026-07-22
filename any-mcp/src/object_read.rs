// any-mcp - bounded, workflow-oriented MCP server for Anytype
//
// SPDX-FileCopyrightText: 2026 Steve Schoettler
// SPDX-License-Identifier: Apache-2.0

//! Bounded object discovery and document-read workflow handlers.

use std::sync::Arc;

use anytype::{
    objects::Object,
    paged::{PagedResult, PaginatedResponse},
};
use rmcp::{
    model::CallToolResult,
    schemars::{JsonSchema, Schema, SchemaGenerator},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::{
    cursor::{CursorStore, CursorToken},
    domain::{BoundedText, DomainValueError, ObjectId, TypeKey},
    error::ToolError,
    filters::McpFilterExpression,
    handler_support::{
        HandlerError, HandlerOperationError, UpstreamPagination, begin_page, execute_handler,
        execute_prepared_handler, finish_page, validate_page_binding_size,
    },
    object_output::{ObjectOutput, ProjectionMode, normalized_projection_keys, object_output},
    pagination::{Page, PageLimit},
    protocol::{ToolProfile, WorkflowTool, workflow_tool},
    runtime::{OperationContext, RuntimeContext},
    schema::SchemaContractError,
    validation::{
        BodyChunk, BodyChunkInput, IdList, Omittable, ProjectionList, chunk_body,
        optional_non_null_schema,
    },
};

pub use crate::domain::AnytypeReference;

/// Maximum characters accepted in search text or a scalar filter value.
pub const MAX_SEARCH_TEXT_CHARS: usize = 4_096;

type SearchText = BoundedText<MAX_SEARCH_TEXT_CHARS>;

type Reference = AnytypeReference;

/// Search sort direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SearchSortDirection {
    /// Sort from the smallest/oldest value to the largest/newest.
    Asc,
    /// Sort from the largest/newest value to the smallest/oldest.
    Desc,
}

/// One explicit search sort.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SearchSort {
    /// Anytype property key used for sorting.
    property_key: TypeKey,
    /// Sort direction.
    direction: SearchSortDirection,
}

/// Strict input for `object_search`.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ObjectSearchInput {
    /// Optional unique space name or id. Omit for a global search.
    #[serde(default)]
    #[schemars(schema_with = "optional_reference_schema")]
    space: Omittable<Reference>,
    /// Optional text searched by Anytype in names and content.
    #[serde(default)]
    #[schemars(schema_with = "optional_search_text_schema")]
    text: Omittable<SearchText>,
    /// Type keys, names, or ids for a space search; global searches accept keys.
    #[serde(default)]
    types: IdList<Reference>,
    /// Optional bounded nested filter expression.
    #[serde(default)]
    #[schemars(schema_with = "optional_filter_schema")]
    filters: Omittable<McpFilterExpression>,
    /// Optional single property sort.
    #[serde(default)]
    #[schemars(schema_with = "optional_sort_schema")]
    sort: Omittable<SearchSort>,
    /// Properties to project into each summary; omitted keys are never returned.
    #[serde(default)]
    property_keys: ProjectionList<TypeKey>,
    /// Requested item limit, defaulting to 20.
    #[serde(default)]
    limit: PageLimit,
    /// Opaque continuation cursor.
    #[serde(default)]
    #[schemars(schema_with = "optional_cursor_schema")]
    cursor: Omittable<CursorToken>,
}

/// Strict input for `object_get`.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ObjectGetInput {
    /// Unique space name or id.
    space: Reference,
    /// Stable object id; titles are deliberately not resolved.
    object_id: ObjectId,
    /// Explicit property projection. Omit to return all properties only when bounded.
    #[serde(default)]
    #[schemars(schema_with = "optional_projection_schema")]
    property_keys: Omittable<ProjectionList<TypeKey>>,
    /// Optional Unicode-character-indexed body chunk request.
    #[serde(default)]
    #[schemars(schema_with = "optional_body_input_schema")]
    body: Omittable<BodyChunkInput>,
}

/// Bounded `object_get` result.
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ObjectGetOutput {
    /// Stable metadata and the explicit bounded property projection.
    object: ObjectOutput,
    /// Requested body chunk, absent when no body was requested.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(default, schema_with = "optional_body_output_schema")]
    body: Option<BodyChunk>,
}

/// Typed contracts and execution state for object read workflows.
#[derive(Clone)]
pub struct ObjectReadHandlers {
    runtime: RuntimeContext,
    cursors: Arc<CursorStore>,
    search_contract: WorkflowTool<Page<ObjectOutput>>,
    get_contract: WorkflowTool<ObjectGetOutput>,
}

impl ObjectReadHandlers {
    /// Constructs object read handlers and validates both strict wire contracts.
    pub fn new(
        runtime: RuntimeContext,
        cursors: Arc<CursorStore>,
    ) -> Result<Self, SchemaContractError> {
        Ok(Self {
            runtime,
            cursors,
            search_contract: workflow_tool::<ObjectSearchInput, Page<ObjectOutput>>(
                "object_search",
                "Search one bounded Anytype page. Returns summaries and only explicitly projected properties, never document bodies. Boolean and numeric filters are passed through unchanged and pagination follows the checked upstream page.",
                ToolProfile::Read,
            )?,
            get_contract: workflow_tool::<ObjectGetInput, ObjectGetOutput>(
                "object_get",
                "Read bounded object metadata and properties, with an optional Unicode-safe body chunk whose SHA-256 covers the complete current body.",
                ToolProfile::Read,
            )?,
        })
    }

    /// Borrows the typed `object_search` contract.
    #[must_use]
    pub const fn search_contract(&self) -> &WorkflowTool<Page<ObjectOutput>> {
        &self.search_contract
    }

    /// Borrows the typed `object_get` contract.
    #[must_use]
    pub const fn get_contract(&self) -> &WorkflowTool<ObjectGetOutput> {
        &self.get_contract
    }

    /// Resolves references, executes exactly one upstream search page, validates
    /// pagination integrity, and returns body-free bounded object outputs.
    pub async fn object_search(
        &self,
        input: ObjectSearchInput,
        cancellation: &CancellationToken,
    ) -> CallToolResult {
        let client = self.runtime.client();
        let cursors = self.cursors.as_ref();
        execute_prepared_handler(
            &self.runtime,
            &self.search_contract,
            OperationContext::new("object_search"),
            cancellation,
            async move {
                let projection = normalized_projection_keys(input.property_keys.as_slice())
                    .map_err(HandlerError::from)?;
                let filters = input
                    .filters
                    .as_ref()
                    .map(McpFilterExpression::to_anytype)
                    .transpose()?;
                let filter_binding = input
                    .filters
                    .as_ref()
                    .map(McpFilterExpression::cursor_binding_value)
                    .transpose()?;
                let raw_filter_binding = input
                    .filters
                    .as_ref()
                    .map(serde_json::to_value)
                    .transpose()
                    .map_err(|_| HandlerError::new(ToolError::upstream()))?;
                let mut normalized_types = input.types.as_slice().to_vec();
                normalized_types.sort_by(|left, right| left.as_str().cmp(right.as_str()));
                normalized_types.dedup_by(|left, right| left.as_str() == right.as_str());
                let space_id = match input.space.as_ref() {
                    Some(space) => Some(client.resolve_space_id(space.as_str()).await?),
                    None => None,
                };
                let resolved_types = if let Some(space_id) = space_id.as_deref() {
                    let mut resolved = Vec::with_capacity(normalized_types.len());
                    for reference in &normalized_types {
                        validate_explicit_type_reference(reference)?;
                        let key = client
                            .resolve_type_key(space_id, reference.as_str())
                            .await?;
                        resolved.push(validate_resolved_type_key(key)?);
                    }
                    resolved.sort();
                    resolved.dedup();
                    resolved
                } else {
                    normalized_types
                        .iter()
                        .map(|value| {
                            TypeKey::new(value.as_str())
                                .map(|key| key.as_str().to_owned())
                                .map_err(|_| HandlerError::new(ToolError::validation()))
                        })
                        .collect::<Result<Vec<_>, _>>()?
                };
                let raw_binding = SearchBinding {
                    space_id: space_id.as_deref(),
                    text: input.text.as_ref(),
                    types: &resolved_types,
                    filters: raw_filter_binding.as_ref(),
                    sort: input.sort.as_ref(),
                    property_keys: &projection,
                };
                validate_page_binding_size("object_search", input.limit, &raw_binding)?;
                let binding = SearchBinding {
                    filters: filter_binding.as_ref(),
                    ..raw_binding
                };
                let page_request = begin_page(
                    cursors,
                    input.cursor.as_ref(),
                    "object_search",
                    input.limit,
                    &binding,
                )?;
                let mut request = match space_id.as_deref() {
                    Some(space_id) => client.search_in(space_id),
                    None => client.search_global(),
                }
                .limit(u32::from(input.limit.get()))
                .offset(page_request.offset().get())
                .types(resolved_types);
                if let Some(text) = input.text.as_ref() {
                    request = request.text(text.as_str());
                }
                if let Some(filters) = filters {
                    request = request.filters(filters);
                }
                if let Some(sort) = input.sort.as_ref() {
                    request = match sort.direction {
                        SearchSortDirection::Asc => request.sort_asc(sort.property_key.as_str()),
                        SearchSortDirection::Desc => request.sort_desc(sort.property_key.as_str()),
                    };
                }
                let page = request.execute().await?;
                Ok::<_, HandlerOperationError>((page, page_request, projection))
            },
            |(page, page_request, projection)| async move {
                convert_search_page(cursors, page_request, page, projection.as_slice())
            },
        )
        .await
    }

    /// Resolves the space and returns bounded object metadata/properties and,
    /// only when requested, one body chunk plus the complete body hash.
    pub async fn object_get(
        &self,
        input: ObjectGetInput,
        cancellation: &CancellationToken,
    ) -> CallToolResult {
        let client = self.runtime.client();
        let projection = input
            .property_keys
            .as_ref()
            .map(|keys| keys.as_slice().to_vec());
        let space = input.space;
        let request_object_id = input.object_id;
        let conversion_object_id = request_object_id.clone();
        let body_request = input.body;
        execute_handler(
            &self.runtime,
            &self.get_contract,
            OperationContext::new("object_get"),
            cancellation,
            async move {
                let space_id = client.resolve_space_id(space.as_str()).await?;
                let object = client
                    .object(&space_id, request_object_id.as_str())
                    .get()
                    .await?;
                Ok::<_, anytype::error::AnytypeError>((object, space_id))
            },
            |(object, resolved_space_id)| async move {
                if object.id != conversion_object_id.as_str()
                    || object.space_id != resolved_space_id
                {
                    return Err(HandlerError::new(ToolError::upstream()));
                }
                let mode = projection
                    .as_deref()
                    .map_or(ProjectionMode::AllBounded, ProjectionMode::Selected);
                let projected = object_output(&object, mode)?;
                let body = body_request
                    .as_ref()
                    .map(|request| {
                        let markdown = object
                            .markdown
                            .as_deref()
                            .ok_or_else(|| HandlerError::new(ToolError::upstream()))?;
                        chunk_body(markdown, request.offset, request.max_chars)
                            .map_err(HandlerError::from)
                    })
                    .transpose()?;
                Ok(ObjectGetOutput {
                    object: projected,
                    body,
                })
            },
        )
        .await
    }
}

#[derive(Serialize)]
struct SearchBinding<'a> {
    space_id: Option<&'a str>,
    text: Option<&'a SearchText>,
    types: &'a [String],
    filters: Option<&'a Value>,
    sort: Option<&'a SearchSort>,
    property_keys: &'a [TypeKey],
}

fn convert_search_page(
    cursors: &CursorStore,
    request: crate::handler_support::PageRequest,
    page: PagedResult<Object>,
    projection: &[TypeKey],
) -> Result<Page<ObjectOutput>, HandlerError> {
    convert_search_response(cursors, request, page.into_response(), projection)
}

fn convert_search_response(
    cursors: &CursorStore,
    request: crate::handler_support::PageRequest,
    response: PaginatedResponse<Object>,
    projection: &[TypeKey],
) -> Result<Page<ObjectOutput>, HandlerError> {
    if response.items.len() > usize::from(request.limit().get()) {
        return Err(HandlerError::new(ToolError::bounded_result()));
    }
    let upstream = UpstreamPagination::try_from(&response.pagination)?;
    let mode = if projection.is_empty() {
        ProjectionMode::SummaryOnly
    } else {
        ProjectionMode::Selected(projection)
    };
    let items = response
        .items
        .iter()
        .filter(|object| !object.archived)
        .map(|object| object_output(object, mode).map_err(HandlerError::from))
        .collect::<Result<Vec<_>, _>>()?;
    finish_page(cursors, request, upstream, items)
}

fn validate_explicit_type_reference(reference: &Reference) -> Result<(), HandlerError> {
    if let Some(key) = reference.as_str().strip_prefix('@') {
        TypeKey::new(key).map_err(|_| HandlerError::new(ToolError::validation()))?;
    }
    Ok(())
}

fn validate_resolved_type_key(value: String) -> Result<String, HandlerError> {
    TypeKey::new(value)
        .map(|key| key.as_str().to_owned())
        .map_err(|error| match error {
            DomainValueError::TooLong { .. } => HandlerError::new(ToolError::bounded_result()),
            DomainValueError::Empty | DomainValueError::InvalidIdentifierCharacter => {
                HandlerError::new(ToolError::upstream())
            }
        })
}

fn optional_reference_schema(generator: &mut SchemaGenerator) -> Schema {
    optional_non_null_schema::<Reference>(generator)
}

fn optional_search_text_schema(generator: &mut SchemaGenerator) -> Schema {
    optional_non_null_schema::<SearchText>(generator)
}

fn optional_filter_schema(generator: &mut SchemaGenerator) -> Schema {
    optional_non_null_schema::<McpFilterExpression>(generator)
}

fn optional_sort_schema(generator: &mut SchemaGenerator) -> Schema {
    optional_non_null_schema::<SearchSort>(generator)
}

fn optional_cursor_schema(generator: &mut SchemaGenerator) -> Schema {
    optional_non_null_schema::<CursorToken>(generator)
}

fn optional_projection_schema(generator: &mut SchemaGenerator) -> Schema {
    optional_non_null_schema::<ProjectionList<TypeKey>>(generator)
}

fn optional_body_input_schema(generator: &mut SchemaGenerator) -> Schema {
    optional_non_null_schema::<BodyChunkInput>(generator)
}

fn optional_body_output_schema(generator: &mut SchemaGenerator) -> Schema {
    optional_non_null_schema::<BodyChunk>(generator)
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{Arc as StdArc, Mutex},
        time::Duration,
    };

    use anytype::{
        objects::{DataModel, ObjectLayout},
        prelude::{AnytypeClient, ClientConfig, HttpCredentials},
        properties::{PropertyValue, PropertyWithValue},
        types::Type,
    };
    use serde_json::json;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::*;
    use crate::{
        runtime::StartupStatus,
        schema::{input_schema, output_schema},
        validation::{BodyCharLimit, BodyOffset},
    };

    fn property(key: &str, value: PropertyValue) -> PropertyWithValue {
        PropertyWithValue {
            id: format!("id-{key}"),
            key: key.to_owned(),
            name: key.to_owned(),
            value,
        }
    }

    fn object(body: &str) -> Object {
        Object {
            archived: false,
            icon: None,
            id: "object-1".to_owned(),
            layout: ObjectLayout::Basic,
            markdown: Some(body.to_owned()),
            name: Some("Roadmap".to_owned()),
            object: DataModel::Object,
            properties: vec![
                property(
                    "last_modified_date",
                    PropertyValue::Date {
                        date: "2026-07-20T10:00:00Z".to_owned(),
                    },
                ),
                property("done", PropertyValue::Checkbox { checkbox: true }),
                property(
                    "private",
                    PropertyValue::Text {
                        text: "hidden".to_owned(),
                    },
                ),
            ],
            snippet: Some("private snippet".to_owned()),
            space_id: "space-1".to_owned(),
            r#type: Some(Type {
                archived: false,
                icon: None,
                id: "type-1".to_owned(),
                key: "page".to_owned(),
                layout: ObjectLayout::Basic,
                name: Some("Page".to_owned()),
                plural_name: None,
                properties: Vec::new(),
            }),
        }
    }

    #[test]
    fn contracts_are_strict_bounded_and_body_is_optional_non_null() {
        assert!(input_schema::<ObjectSearchInput>().is_ok());
        assert!(input_schema::<ObjectGetInput>().is_ok());
        assert!(output_schema::<Page<ObjectOutput>>().is_ok());
        let get = output_schema::<ObjectGetOutput>().unwrap();
        assert!(!get["required"].as_array().unwrap().contains(&json!("body")));
        assert!(!get["properties"]["body"].to_string().contains("null"));

        let search = input_schema::<ObjectSearchInput>().unwrap();
        assert_eq!(search["properties"]["limit"]["$ref"], "#/$defs/PageLimit");
        assert_eq!(search["$defs"]["BoundedList50OfTypeKey"]["maxItems"], 50);
        for field in [
            "space",
            "text",
            "types",
            "filters",
            "sort",
            "property_keys",
            "limit",
            "cursor",
        ] {
            assert!(
                !search["properties"][field].to_string().contains("null"),
                "object_search schema permits null {field}: {}",
                search["properties"][field]
            );
        }
        let get_input = input_schema::<ObjectGetInput>().unwrap();
        for field in ["property_keys", "body"] {
            assert!(
                !get_input["properties"][field].to_string().contains("null"),
                "object_get schema permits null {field}"
            );
        }
        let serialized = serde_json::to_string(search.as_ref()).unwrap();
        assert!(!serialized.contains("additionalProperties\":true"));
        assert!(serde_json::from_value::<ObjectSearchInput>(json!({"space":""})).is_err());
    }

    #[test]
    fn every_omittable_input_field_rejects_explicit_null() {
        for field in [
            "space",
            "text",
            "types",
            "filters",
            "sort",
            "property_keys",
            "limit",
            "cursor",
        ] {
            let mut value = json!({});
            value
                .as_object_mut()
                .unwrap()
                .insert(field.to_owned(), serde_json::Value::Null);
            assert!(
                serde_json::from_value::<ObjectSearchInput>(value).is_err(),
                "object_search accepted null {field}"
            );
        }
        for field in ["conditions", "filters"] {
            let mut expression = json!({"operator":"and"});
            expression
                .as_object_mut()
                .unwrap()
                .insert(field.to_owned(), serde_json::Value::Null);
            assert!(
                serde_json::from_value::<McpFilterExpression>(expression).is_err(),
                "filter expression accepted null {field}"
            );
        }

        let direct_id = "bafyreiaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        for field in ["property_keys", "body"] {
            let mut value = json!({"space":direct_id,"object_id":direct_id});
            value
                .as_object_mut()
                .unwrap()
                .insert(field.to_owned(), serde_json::Value::Null);
            assert!(
                serde_json::from_value::<ObjectGetInput>(value).is_err(),
                "object_get accepted null {field}"
            );
        }
        for field in ["offset", "max_chars"] {
            let mut value = json!({
                "space":direct_id,
                "object_id":direct_id,
                "body":{}
            });
            value["body"]
                .as_object_mut()
                .unwrap()
                .insert(field.to_owned(), serde_json::Value::Null);
            assert!(
                serde_json::from_value::<ObjectGetInput>(value).is_err(),
                "body chunk accepted null {field}"
            );
        }

        let omitted_search: ObjectSearchInput = serde_json::from_value(json!({})).unwrap();
        assert!(omitted_search.space.is_none());
        assert!(omitted_search.text.is_none());
        assert!(omitted_search.types.as_slice().is_empty());
        assert!(omitted_search.filters.is_none());
        assert!(omitted_search.sort.is_none());
        assert!(omitted_search.property_keys.as_slice().is_empty());
        assert!(omitted_search.cursor.is_none());
        let omitted_get: ObjectGetInput = serde_json::from_value(json!({
            "space":direct_id,
            "object_id":direct_id
        }))
        .unwrap();
        assert!(omitted_get.property_keys.is_none());
        assert!(omitted_get.body.is_none());
    }

    #[test]
    fn chunked_get_hashes_complete_unicode_body_without_leaking_remainder() {
        let body = "aé🦀z";
        let request = BodyChunkInput {
            offset: BodyOffset::new(1).unwrap(),
            max_chars: BodyCharLimit::new(2).unwrap(),
        };
        let chunk = chunk_body(body, request.offset, request.max_chars).unwrap();
        assert_eq!(chunk.text, "é🦀");
        assert_eq!(chunk.offset.get(), 1);
        assert_eq!(chunk.next_offset.unwrap().get(), 3);
        assert_eq!(chunk.total_chars.get(), 4);
        assert_eq!(
            chunk.sha256,
            "ce6aeb3715a4484be929ac6cd04af03e3a925391fb4f7cfed812e8803ec4354f"
        );
        let encoded = serde_json::to_string(&chunk).unwrap();
        assert!(!encoded.contains('z'));
    }

    #[test]
    fn search_projection_is_selected_only_and_never_contains_body_or_snippet() {
        let store = CursorStore::new().unwrap();
        let request = begin_page(
            &store,
            None,
            "object_search",
            PageLimit::new(20).unwrap(),
            &json!({"space":"space-1"}),
        )
        .unwrap();
        let response = anytype::paged::PaginatedResponse {
            items: vec![object("complete secret body")],
            pagination: anytype::paged::PaginationMeta {
                has_more: false,
                limit: 20,
                offset: 0,
                total: 1,
            },
        };
        let projected =
            convert_search_response(&store, request, response, &[TypeKey::new("done").unwrap()])
                .unwrap();
        let encoded = serde_json::to_string(&projected).unwrap();
        assert!(encoded.contains("done"));
        assert!(!encoded.contains("private"));
        assert!(!encoded.contains("hidden"));
        assert!(!encoded.contains("secret body"));
        assert!(!encoded.contains("snippet"));
    }

    #[test]
    fn search_pagination_integrity_fails_closed_and_issues_no_cursor() {
        let store = CursorStore::new().unwrap();
        let request = begin_page(
            &store,
            None,
            "object_search",
            PageLimit::new(20).unwrap(),
            &json!({"space":"space-1"}),
        )
        .unwrap();
        let response = anytype::paged::PaginatedResponse {
            items: vec![object("")],
            pagination: anytype::paged::PaginationMeta {
                has_more: true,
                limit: 19,
                offset: 0,
                total: 100,
            },
        };
        assert!(convert_search_response(&store, request, response, &[]).is_err());
        assert_eq!(store.entry_count(), 0);

        let overflow_request = begin_page(
            &store,
            None,
            "object_search",
            PageLimit::new(20).unwrap(),
            &json!({"space":"space-1"}),
        )
        .unwrap();
        let overflow = anytype::paged::PaginatedResponse {
            items: vec![object(""); 21],
            pagination: anytype::paged::PaginationMeta {
                has_more: false,
                limit: 20,
                offset: 0,
                total: 21,
            },
        };
        assert!(convert_search_response(&store, overflow_request, overflow, &[]).is_err());
        assert_eq!(store.entry_count(), 0);
    }

    fn runtime_at(endpoint: &str) -> RuntimeContext {
        let client = AnytypeClient::with_config(ClientConfig {
            base_url: Some(endpoint.to_owned()),
            keystore: Some("env".to_owned()),
            keystore_service: Some("object-read-test".to_owned()),
            app_name: "object-read-test".to_owned(),
            ..ClientConfig::default()
        })
        .unwrap();
        client.set_api_key(HttpCredentials::new("test-token"));
        RuntimeContext::from_parts(
            client,
            1,
            Duration::from_millis(50),
            StartupStatus {
                http_available: true,
                grpc_available: false,
            },
        )
    }

    fn runtime() -> RuntimeContext {
        runtime_at("http://127.0.0.1:1")
    }

    async fn mock_http(
        responses: Vec<String>,
    ) -> (
        String,
        StdArc<Mutex<Vec<String>>>,
        tokio::task::JoinHandle<()>,
    ) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let captured = StdArc::new(Mutex::new(Vec::new()));
        let captured_for_task = StdArc::clone(&captured);
        let task = tokio::spawn(async move {
            for response in responses {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                let mut buffer = [0_u8; 4096];
                let expected_length = loop {
                    let read = socket.read(&mut buffer).await.unwrap();
                    assert_ne!(read, 0, "client closed before sending request headers");
                    request.extend_from_slice(&buffer[..read]);
                    let Some(header_end) =
                        request.windows(4).position(|value| value == b"\r\n\r\n")
                    else {
                        continue;
                    };
                    let headers = String::from_utf8_lossy(&request[..header_end]);
                    let content_length = headers
                        .lines()
                        .find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            name.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse::<usize>().unwrap())
                        })
                        .unwrap_or(0);
                    break header_end + 4 + content_length;
                };
                while request.len() < expected_length {
                    let read = socket.read(&mut buffer).await.unwrap();
                    assert_ne!(read, 0, "client closed before sending request body");
                    request.extend_from_slice(&buffer[..read]);
                }
                captured_for_task
                    .lock()
                    .unwrap()
                    .push(String::from_utf8(request).unwrap());
                let response = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                    response.len(),
                    response
                );
                socket.write_all(response.as_bytes()).await.unwrap();
            }
        });
        (format!("http://{address}"), captured, task)
    }

    fn paged_response(object: &Object, offset: u32, limit: u32, has_more: bool) -> String {
        json!({
            "items": [object],
            "pagination": {
                "has_more": has_more,
                "limit": limit,
                "offset": offset,
                "total": 2
            }
        })
        .to_string()
    }

    async fn decode_and_dispatch_search(
        handlers: &ObjectReadHandlers,
        value: serde_json::Value,
    ) -> Result<CallToolResult, serde_json::Error> {
        let input = serde_json::from_value::<ObjectSearchInput>(value)?;
        Ok(handlers
            .object_search(input, &CancellationToken::new())
            .await)
    }

    #[tokio::test]
    async fn null_and_unsafe_filter_ids_fail_at_wire_decode_without_io() {
        let runtime = runtime();
        let metrics = runtime.clone();
        let handlers =
            ObjectReadHandlers::new(runtime, Arc::new(CursorStore::new().unwrap())).unwrap();
        assert!(
            decode_and_dispatch_search(&handlers, json!({"space":null}))
                .await
                .is_err()
        );
        for (format, value) in [
            ("files", "path/segment"),
            ("objects", ".."),
            ("objects", "idé"),
        ] {
            assert!(
                decode_and_dispatch_search(
                    &handlers,
                    json!({
                        "filters":{
                            "operator":"and",
                            "conditions":[{
                                "format":format,
                                "property_key":"links",
                                "condition":"in",
                                "values":[value]
                            }]
                        }
                    })
                )
                .await
                .is_err()
            );
        }
        assert_eq!(metrics.client().http_metrics().total_requests, 0);
    }

    #[tokio::test]
    async fn semantic_filter_deduplication_does_not_weaken_raw_query_size_bound() {
        let runtime = runtime();
        let metrics = runtime.clone();
        let handlers =
            ObjectReadHandlers::new(runtime, Arc::new(CursorStore::new().unwrap())).unwrap();
        let repeated = json!({
            "format":"text",
            "property_key":"name",
            "condition":"contains",
            "value":"x".repeat(MAX_SEARCH_TEXT_CHARS)
        });
        let input: ObjectSearchInput = serde_json::from_value(json!({
            "filters":{
                "operator":"and",
                "conditions":vec![repeated; 49],
                "filters":[]
            }
        }))
        .unwrap();
        let result = handlers
            .object_search(input, &CancellationToken::new())
            .await;
        assert_eq!(result.is_error, Some(true));
        assert_eq!(
            result.structured_content.as_ref().unwrap()["code"],
            "validation"
        );
        assert_eq!(metrics.client().http_metrics().total_requests, 0);
    }

    fn paginated_value(
        items: Vec<serde_json::Value>,
        offset: u32,
        limit: u32,
        has_more: bool,
    ) -> String {
        json!({
            "items":items,
            "pagination":{
                "has_more":has_more,
                "limit":limit,
                "offset":offset,
                "total":items.len()
            }
        })
        .to_string()
    }

    #[tokio::test]
    async fn space_name_and_type_name_key_and_explicit_key_resolve_on_wire() {
        let space_id = "bafyreiaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let mut response_object = object("");
        response_object.space_id = space_id.to_owned();
        let type_value = serde_json::to_value(response_object.r#type.as_ref().unwrap()).unwrap();
        let space_value = json!({
            "id":space_id,
            "name":"Workspace",
            "object":"space",
            "description":null,
            "icon":null,
            "gateway_url":null,
            "network_id":null
        });
        let search = paged_response(&response_object, 0, 1, false);
        let (endpoint, requests, server) = mock_http(vec![
            paginated_value(vec![space_value], 0, 99, false),
            paginated_value(vec![type_value.clone()], 0, 99, false),
            search.clone(),
            paginated_value(vec![type_value], 0, 99, false),
            search.clone(),
            search,
        ])
        .await;
        let handlers =
            ObjectReadHandlers::new(runtime_at(&endpoint), Arc::new(CursorStore::new().unwrap()))
                .unwrap();
        for (space, typ) in [
            ("Workspace", "Page"),
            (space_id, "page"),
            (space_id, "@page"),
        ] {
            let input: ObjectSearchInput = serde_json::from_value(json!({
                "space":space,
                "types":[typ],
                "limit":1
            }))
            .unwrap();
            let result = handlers
                .object_search(input, &CancellationToken::new())
                .await;
            assert_eq!(result.is_error, Some(false), "failed {space}/{typ}");
        }
        server.await.unwrap();

        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 6);
        assert!(requests[0].starts_with("GET /v1/spaces?limit=99 HTTP/1.1"));
        assert!(requests[1].starts_with(&format!(
            "GET /v1/spaces/{space_id}/types?limit=99 HTTP/1.1"
        )));
        assert!(requests[2].starts_with(&format!(
            "POST /v1/spaces/{space_id}/search?limit=1 HTTP/1.1"
        )));
        assert!(requests[3].contains(&format!("/v1/spaces/{space_id}/types?limit=99")));
        assert!(requests[4].starts_with(&format!(
            "POST /v1/spaces/{space_id}/search?limit=1 HTTP/1.1"
        )));
        assert!(requests[5].starts_with(&format!(
            "POST /v1/spaces/{space_id}/search?limit=1 HTTP/1.1"
        )));
        for index in [2, 4, 5] {
            let body: serde_json::Value =
                serde_json::from_str(requests[index].split("\r\n\r\n").nth(1).unwrap()).unwrap();
            assert_eq!(body["types"], json!(["page"]));
        }
    }

    #[tokio::test]
    async fn invalid_explicit_and_corrupt_resolved_type_keys_fail_closed() {
        let space_id = "bafyreiaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let runtime = runtime();
        let metrics = runtime.clone();
        let handlers =
            ObjectReadHandlers::new(runtime, Arc::new(CursorStore::new().unwrap())).unwrap();
        for typ in ["@".to_owned(), format!("@{}", "x".repeat(257))] {
            let input: ObjectSearchInput = serde_json::from_value(json!({
                "space":space_id,
                "types":[typ]
            }))
            .unwrap();
            let result = handlers
                .object_search(input, &CancellationToken::new())
                .await;
            assert_eq!(result.is_error, Some(true));
            assert_eq!(
                result.structured_content.as_ref().unwrap()["code"],
                "validation"
            );
        }
        assert_eq!(metrics.client().http_metrics().total_requests, 0);

        let corrupt_type = json!({
            "archived":false,
            "id":"type-1",
            "key":"",
            "layout":"basic",
            "name":"Page",
            "plural_name":null,
            "properties":[]
        });
        let (endpoint, requests, server) =
            mock_http(vec![paginated_value(vec![corrupt_type], 0, 99, false)]).await;
        let handlers =
            ObjectReadHandlers::new(runtime_at(&endpoint), Arc::new(CursorStore::new().unwrap()))
                .unwrap();
        let input: ObjectSearchInput = serde_json::from_value(json!({
            "space":space_id,
            "types":["Page"]
        }))
        .unwrap();
        let result = handlers
            .object_search(input, &CancellationToken::new())
            .await;
        assert_eq!(result.is_error, Some(true));
        assert_eq!(
            result.structured_content.as_ref().unwrap()["code"],
            "upstream"
        );
        server.await.unwrap();
        assert_eq!(requests.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn object_search_wire_path_executes_one_page_and_continues_checked_offset() {
        let response_object = object("body must never appear");
        let (endpoint, requests, server) = mock_http(vec![
            paged_response(&response_object, 0, 1, true),
            paged_response(&response_object, 1, 1, false),
        ])
        .await;
        let handlers =
            ObjectReadHandlers::new(runtime_at(&endpoint), Arc::new(CursorStore::new().unwrap()))
                .unwrap();
        let mut input: ObjectSearchInput = serde_json::from_value(json!({
            "text":"roadmap",
            "types":["page"],
            "filters":{
                "operator":"and",
                "conditions":[
                    {
                        "format":"select",
                        "property_key":"tag",
                        "condition":"in",
                        "values":["beta","alpha","alpha"]
                    },
                    {
                        "format":"checkbox",
                        "property_key":"done",
                        "condition":"eq",
                        "value":true
                    }
                ],
                "filters":[]
            },
            "sort":{"property_key":"last_modified_date","direction":"desc"},
            "property_keys":["done"],
            "limit":1
        }))
        .unwrap();
        let first = handlers
            .object_search(input.clone(), &CancellationToken::new())
            .await;
        assert_eq!(first.is_error, Some(false));
        let first_value = first.structured_content.unwrap();
        assert_eq!(first_value["items"][0]["summary"]["id"], "object-1");
        assert_eq!(first_value["items"][0]["properties"][0]["key"], "done");
        assert!(!first_value.to_string().contains("body must never appear"));
        assert!(!first_value.to_string().contains("hidden"));
        input.cursor = Omittable::Present(
            CursorToken::new(first_value["next_cursor"].as_str().unwrap().to_owned()).unwrap(),
        );
        input.filters = Omittable::Present(
            serde_json::from_value(json!({
                "operator":"and",
                "conditions":[
                    {
                        "format":"checkbox",
                        "property_key":"done",
                        "condition":"eq",
                        "value":true
                    },
                    {
                        "format":"select",
                        "property_key":"tag",
                        "condition":"in",
                        "values":["alpha","beta"]
                    }
                ],
                "filters":[]
            }))
            .unwrap(),
        );
        let second = handlers
            .object_search(input, &CancellationToken::new())
            .await;
        assert_eq!(second.is_error, Some(false));
        assert!(
            second
                .structured_content
                .unwrap()
                .get("next_cursor")
                .is_none()
        );
        server.await.unwrap();

        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert!(requests[0].starts_with("POST /v1/search?limit=1 HTTP/1.1"));
        assert!(requests[1].starts_with("POST /v1/search?limit=1&offset=1 HTTP/1.1"));
        let first_body = requests[0].split("\r\n\r\n").nth(1).unwrap();
        let first_body: serde_json::Value = serde_json::from_str(first_body).unwrap();
        assert_eq!(first_body["query"], "roadmap");
        assert_eq!(first_body["types"], json!(["page"]));
        assert_eq!(
            first_body["filters"]["conditions"][0]["select"],
            "beta,alpha,alpha"
        );
        assert_eq!(first_body["filters"]["conditions"][1]["checkbox"], true);
        assert_eq!(first_body["sort"]["property_key"], "last_modified_date");
        let second_body = requests[1].split("\r\n\r\n").nth(1).unwrap();
        let second_body: serde_json::Value = serde_json::from_str(second_body).unwrap();
        assert_eq!(second_body["filters"]["conditions"][0]["checkbox"], true);
        assert_eq!(
            second_body["filters"]["conditions"][1]["select"],
            "alpha,beta"
        );
    }

    #[tokio::test]
    async fn object_get_wire_path_returns_only_requested_chunk_and_full_hash() {
        let direct_space = "bafyreiaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let mut response_object = object("aé🦀z");
        response_object.space_id = direct_space.to_owned();
        response_object.id = direct_space.to_owned();
        let response = json!({"object": response_object}).to_string();
        let (endpoint, requests, server) = mock_http(vec![response]).await;
        let handlers =
            ObjectReadHandlers::new(runtime_at(&endpoint), Arc::new(CursorStore::new().unwrap()))
                .unwrap();
        let input: ObjectGetInput = serde_json::from_value(json!({
            "space":direct_space,
            "object_id":direct_space,
            "property_keys":["done"],
            "body":{"offset":1,"max_chars":2}
        }))
        .unwrap();
        let result = handlers.object_get(input, &CancellationToken::new()).await;
        assert_eq!(result.is_error, Some(false));
        let value = result.structured_content.unwrap();
        assert_eq!(value["body"]["text"], "é🦀");
        assert_eq!(value["body"]["offset"], 1);
        assert_eq!(value["body"]["next_offset"], 3);
        assert_eq!(value["body"]["total_chars"], 4);
        assert_eq!(
            value["body"]["sha256"],
            "ce6aeb3715a4484be929ac6cd04af03e3a925391fb4f7cfed812e8803ec4354f"
        );
        assert!(!value.to_string().contains('z'));
        assert!(!value.to_string().contains("hidden"));
        server.await.unwrap();
        let requests = requests.lock().unwrap();
        assert!(requests[0].starts_with(&format!(
            "GET /v1/spaces/{direct_space}/objects/{direct_space} HTTP/1.1"
        )));
    }

    #[tokio::test]
    async fn handler_classifies_upstream_failure_without_exposing_endpoint() {
        let handlers =
            ObjectReadHandlers::new(runtime(), Arc::new(CursorStore::new().unwrap())).unwrap();
        let input: ObjectGetInput = serde_json::from_value(json!({
            "space":"bafyreiaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "object_id":"bafyreiaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "property_keys":[]
        }))
        .unwrap();
        let result = handlers.object_get(input, &CancellationToken::new()).await;
        assert_eq!(result.is_error, Some(true));
        assert_eq!(
            result.structured_content.as_ref().unwrap()["code"],
            "upstream"
        );
        let encoded = result.content[0].as_text().unwrap().text.as_str();
        assert!(!encoded.contains("127.0.0.1"));
        assert!(!encoded.contains("test-token"));
    }
}
