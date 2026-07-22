// any-mcp - bounded, workflow-oriented MCP server for Anytype
//
// SPDX-FileCopyrightText: 2026 Steve Schoettler
// SPDX-License-Identifier: Apache-2.0

//! Canonical, presentation-independent collection membership workflows.
//!
//! This module provides the production-unlinked read slice for the eventual
//! `views-write` optional registry. It enumerates direct manual-collection
//! membership through `anytype-api`; it never reads a saved view, filter,
//! layout, sort, or Kanban column.

use std::borrow::Cow;

use anytype::views::{
    CollectionMembershipContinuation, CollectionMembershipPage as ApiCollectionMembershipPage,
};
use rmcp::{
    model::{CallToolRequestMethod, CallToolRequestParams, CallToolResult, ErrorData},
    schemars::{JsonSchema, Schema, SchemaGenerator, json_schema},
};
use serde::{Deserialize, Deserializer, Serialize, de};
use tokio_util::sync::CancellationToken;

use crate::{
    cursor::{CursorStore, CursorToken, EvidenceCursorState, QueryFingerprint},
    discovery::DiscoveryReference,
    domain::{DomainValueError, EntityId},
    error::ToolError,
    handler_support::{
        HandlerError, HandlerOperationError, execute_prepared_handler, page_query_fingerprint,
        validate_page_binding_size,
    },
    optional_toolsets::OptionalRegistryTool,
    pagination::{DEFAULT_PAGE_LIMIT, PageLimit, PageOffset},
    protocol::{ToolProfile, WorkflowTool, workflow_tool},
    result::tool_error,
    runtime::{OperationContext, RuntimeContext},
    schema::SchemaContractError,
    server::decode_arguments,
    validation::{Omittable, optional_non_null_schema},
};

/// Exact tool name for canonical collection membership enumeration.
pub const COLLECTION_MEMBER_LIST: &str = "collection_member_list";
/// Reviewed maximum number of collection members returned by one call.
pub const MAX_COLLECTION_MEMBER_PAGE_LIMIT: u16 = 61;
/// Reviewed maximum logical HTTP operations for one list page.
pub const COLLECTION_MEMBER_LIST_HTTP_LOGICAL_CEILING: usize = 12;
/// Reviewed maximum physical HTTP attempts for one list page.
pub const COLLECTION_MEMBER_LIST_HTTP_PHYSICAL_CEILING: usize = 72;
/// Reviewed maximum gRPC calls including cleanup fallback for one list page.
pub const COLLECTION_MEMBER_LIST_GRPC_CEILING: usize = 3;
/// Reviewed incremental catalog ceiling for the complete future registry.
pub const VIEWS_WRITE_CATALOG_TOKEN_CEILING: usize = 3_000;

const VIEWS_WRITE_REGISTRY: &str = "views-write";

/// Domain-bounded page limit between one and 61, defaulting to 20.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct CollectionMemberPageLimit(u16);

impl CollectionMemberPageLimit {
    /// Validates a collection-membership page limit.
    pub fn new(value: u16) -> Result<Self, CollectionMemberPageLimitError> {
        if (1..=MAX_COLLECTION_MEMBER_PAGE_LIMIT).contains(&value) {
            Ok(Self(value))
        } else {
            Err(CollectionMemberPageLimitError)
        }
    }

    /// Returns the validated limit.
    #[must_use]
    pub const fn get(self) -> u16 {
        self.0
    }

    fn common(self) -> Result<PageLimit, HandlerError> {
        PageLimit::new(self.0).map_err(HandlerError::from)
    }
}

impl Default for CollectionMemberPageLimit {
    fn default() -> Self {
        Self(DEFAULT_PAGE_LIMIT)
    }
}

impl<'de> Deserialize<'de> for CollectionMemberPageLimit {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::new(u16::deserialize(deserializer)?).map_err(de::Error::custom)
    }
}

impl JsonSchema for CollectionMemberPageLimit {
    fn schema_name() -> Cow<'static, str> {
        "PageLimit".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "integer",
            "minimum": 1,
            "maximum": MAX_COLLECTION_MEMBER_PAGE_LIMIT,
        })
    }
}

/// Failure to construct the reviewed collection-membership page limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CollectionMemberPageLimitError;

impl std::fmt::Display for CollectionMemberPageLimitError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("limit must be between 1 and 61; omit it to use 20")
    }
}

impl std::error::Error for CollectionMemberPageLimitError {}

/// Strict input for one canonical page of direct collection members.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CollectionMemberListInput {
    /// Unique space name or identifier.
    space: CollectionSpaceRef,
    /// Exact manual collection identifier.
    collection_id: EntityId,
    /// Requested item limit, defaulting to 20 and capped at 61.
    #[serde(default)]
    limit: CollectionMemberPageLimit,
    /// Opaque continuation cursor for the same resolved identities and limit.
    #[serde(default)]
    #[schemars(schema_with = "optional_cursor_schema")]
    cursor: Omittable<CursorToken>,
}

/// Shared discovery semantics exposed under the design's `SpaceRef` schema name.
#[derive(Debug, Clone, Deserialize)]
#[serde(transparent)]
struct CollectionSpaceRef(DiscoveryReference);

impl CollectionSpaceRef {
    fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl JsonSchema for CollectionSpaceRef {
    fn schema_name() -> Cow<'static, str> {
        "SpaceRef".into()
    }

    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        DiscoveryReference::json_schema(generator)
    }
}

fn optional_cursor_schema(generator: &mut SchemaGenerator) -> Schema {
    optional_non_null_schema::<CursorToken>(generator)
}

/// Minimal identity returned for one direct collection member.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CollectionMemberSummary {
    /// Stable object identifier in canonical collection order.
    object_id: EntityId,
}

impl CollectionMemberSummary {
    /// Borrows the stable direct-member object identifier.
    #[must_use]
    pub const fn object_id(&self) -> &EntityId {
        &self.object_id
    }
}

/// One bounded page of minimized direct collection-member identities.
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CollectionMemberListPage {
    /// Member identities in canonical collection order, never more than 61.
    #[schemars(length(max=MAX_COLLECTION_MEMBER_PAGE_LIMIT))]
    items: Vec<CollectionMemberSummary>,
    /// Opaque continuation cursor, absent on the terminal page.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(default, schema_with = "non_null_cursor_schema")]
    next_cursor: Option<CursorToken>,
}

fn non_null_cursor_schema(generator: &mut SchemaGenerator) -> Schema {
    generator.subschema_for::<CursorToken>()
}

impl CollectionMemberListPage {
    fn new(
        items: Vec<CollectionMemberSummary>,
        next_cursor: Option<CursorToken>,
    ) -> Result<Self, HandlerError> {
        if items.len() > usize::from(MAX_COLLECTION_MEMBER_PAGE_LIMIT) {
            Err(HandlerError::new(ToolError::bounded_result()))
        } else {
            Ok(Self { items, next_cursor })
        }
    }

    /// Borrows the minimized member summaries.
    #[must_use]
    pub fn items(&self) -> &[CollectionMemberSummary] {
        &self.items
    }

    /// Borrows the continuation cursor when another page exists.
    #[must_use]
    pub fn next_cursor(&self) -> Option<&CursorToken> {
        self.next_cursor.as_ref()
    }
}

/// Constructs the approved `collection_member_list` contract.
pub fn collection_member_list_tool()
-> Result<WorkflowTool<CollectionMemberListPage>, SchemaContractError> {
    workflow_tool::<CollectionMemberListInput, CollectionMemberListPage>(
        COLLECTION_MEMBER_LIST,
        "List one bounded page of direct manual-collection member IDs in canonical collection order. Saved views, filters, sorts, layouts, and Kanban columns are ignored.",
        ToolProfile::Read,
    )
}

/// Returns the read slice for later composition into `views-write`.
pub fn collection_member_list_tools() -> Result<Vec<OptionalRegistryTool>, SchemaContractError> {
    Ok(vec![OptionalRegistryTool::read(
        collection_member_list_tool()?,
    )])
}

/// Transport-neutral handler for the production-unlinked membership list slice.
#[derive(Clone, Debug)]
pub struct CollectionMemberListHandlers {
    contract: WorkflowTool<CollectionMemberListPage>,
}

impl CollectionMemberListHandlers {
    /// Creates the reviewed canonical collection-member list handler.
    pub fn new() -> Result<Self, SchemaContractError> {
        Ok(Self {
            contract: collection_member_list_tool()?,
        })
    }

    /// Dispatches `collection_member_list` after the caller's catalog gate.
    pub async fn call_tool(
        &self,
        request: CallToolRequestParams,
        runtime: &RuntimeContext,
        cursors: &CursorStore,
        cancellation: &CancellationToken,
    ) -> Result<CallToolResult, ErrorData> {
        if request.name != COLLECTION_MEMBER_LIST {
            return Err(ErrorData::method_not_found::<CallToolRequestMethod>());
        }
        let input = decode_arguments::<CollectionMemberListInput>(request.arguments)?;
        Ok(self
            .collection_member_list(runtime, cursors, input, cancellation)
            .await)
    }

    async fn collection_member_list(
        &self,
        runtime: &RuntimeContext,
        cursors: &CursorStore,
        input: CollectionMemberListInput,
        cancellation: &CancellationToken,
    ) -> CallToolResult {
        let common_limit = match input.limit.common() {
            Ok(limit) => limit,
            Err(error) => return tool_error(error.tool_error()),
        };
        if let Err(error) = validate_page_binding_size(
            COLLECTION_MEMBER_LIST,
            common_limit,
            &RawPageParams {
                registry: VIEWS_WRITE_REGISTRY,
                space: input.space.as_str(),
                collection_id: input.collection_id.as_str(),
            },
        ) {
            return tool_error(error.tool_error());
        }

        let client = runtime.client().clone();
        let collection_id = input.collection_id.as_str().to_owned();
        let cursor = input.cursor.as_ref().cloned();
        let limit = input.limit;
        execute_prepared_handler(
            runtime,
            &self.contract,
            OperationContext::new(COLLECTION_MEMBER_LIST),
            cancellation,
            async move {
                let space_id = client.resolve_space_id(input.space.as_str()).await?;
                let binding = page_query_fingerprint(
                    COLLECTION_MEMBER_LIST,
                    common_limit,
                    &ResolvedPageParams {
                        registry: VIEWS_WRITE_REGISTRY,
                        space_id: &space_id,
                        collection_id: &collection_id,
                    },
                )?;
                let continuation = cursor
                    .as_ref()
                    .map(|cursor| resolve_continuation(cursors, cursor, binding))
                    .transpose()?;
                let expected_offset = continuation.as_ref().map_or(0, |state| state.next_offset);
                let page = client
                    .collection_membership_page(
                        &space_id,
                        &collection_id,
                        u32::from(limit.get()),
                        continuation,
                    )
                    .await?;
                Ok::<_, HandlerOperationError>((
                    page,
                    binding,
                    space_id,
                    collection_id,
                    expected_offset,
                ))
            },
            |(page, binding, space_id, collection_id, expected_offset)| async move {
                convert_page(
                    cursors,
                    page,
                    &space_id,
                    &collection_id,
                    expected_offset,
                    limit,
                    binding,
                )
            },
        )
        .await
    }
}

#[derive(Serialize)]
struct RawPageParams<'a> {
    registry: &'static str,
    space: &'a str,
    collection_id: &'a str,
}

#[derive(Serialize)]
struct ResolvedPageParams<'a> {
    registry: &'static str,
    space_id: &'a str,
    collection_id: &'a str,
}

fn resolve_continuation(
    cursors: &CursorStore,
    cursor: &CursorToken,
    binding: QueryFingerprint,
) -> Result<CollectionMembershipContinuation, HandlerError> {
    let state = cursors.resolve_evidence(cursor, binding)?;
    Ok(CollectionMembershipContinuation {
        next_offset: u64::from(state.offset().get()),
        total: state.total(),
        final_object_id: state.boundary_id().to_owned(),
    })
}

fn convert_page(
    cursors: &CursorStore,
    page: ApiCollectionMembershipPage,
    expected_space_id: &str,
    expected_collection_id: &str,
    expected_offset: u64,
    limit: CollectionMemberPageLimit,
    binding: QueryFingerprint,
) -> Result<CollectionMemberListPage, HandlerError> {
    if page.space_id != expected_space_id
        || page.collection_id != expected_collection_id
        || page.offset != expected_offset
        || page.object_ids.len() > usize::from(limit.get())
    {
        return Err(HandlerError::new(ToolError::upstream()));
    }

    let item_count = u64::try_from(page.object_ids.len())
        .map_err(|_| HandlerError::new(ToolError::bounded_result()))?;
    let consumed_end = page
        .offset
        .checked_add(item_count)
        .ok_or_else(|| HandlerError::new(ToolError::bounded_result()))?;
    if page.offset > page.total
        || consumed_end > page.total
        || (expected_offset > 0 && page.object_ids.is_empty())
    {
        return Err(HandlerError::new(ToolError::upstream()));
    }
    if page.offset > u64::from(crate::pagination::MAX_PAGE_OFFSET) {
        return Err(HandlerError::new(ToolError::bounded_result()));
    }
    let mut summaries = Vec::with_capacity(page.object_ids.len());
    for object_id in &page.object_ids {
        let object_id = EntityId::new(object_id.clone()).map_err(domain_error)?;
        if summaries
            .iter()
            .any(|summary: &CollectionMemberSummary| summary.object_id == object_id)
        {
            return Err(HandlerError::new(ToolError::upstream()));
        }
        summaries.push(CollectionMemberSummary { object_id });
    }

    let next_cursor = match page.continuation.as_ref() {
        Some(continuation) => {
            validate_next_continuation(&page, &summaries, continuation, limit)?;
            let next_offset = u32::try_from(continuation.next_offset)
                .ok()
                .and_then(|offset| PageOffset::new(offset).ok())
                .ok_or_else(|| HandlerError::new(ToolError::bounded_result()))?;
            let state = EvidenceCursorState::new(
                next_offset,
                continuation.total,
                continuation.final_object_id.clone(),
            );
            cursors
                .issue_evidence(state, binding)
                .map_err(HandlerError::from)
                .map(Some)?
        }
        None if consumed_end == page.total => None,
        None => return Err(HandlerError::new(ToolError::upstream())),
    };
    CollectionMemberListPage::new(summaries, next_cursor)
}

fn validate_next_continuation(
    page: &ApiCollectionMembershipPage,
    summaries: &[CollectionMemberSummary],
    continuation: &CollectionMembershipContinuation,
    limit: CollectionMemberPageLimit,
) -> Result<(), HandlerError> {
    let item_count = u64::try_from(summaries.len())
        .map_err(|_| HandlerError::new(ToolError::bounded_result()))?;
    let expected_next = page
        .offset
        .checked_add(item_count)
        .ok_or_else(|| HandlerError::new(ToolError::bounded_result()))?;
    let final_id = summaries.last().map(|summary| summary.object_id.as_str());
    if summaries.is_empty()
        || summaries.len() > usize::from(limit.get())
        || continuation.next_offset != expected_next
        || continuation.next_offset >= continuation.total
        || continuation.total != page.total
        || final_id != Some(continuation.final_object_id.as_str())
    {
        return Err(HandlerError::new(ToolError::upstream()));
    }
    Ok(())
}

fn domain_error(_: DomainValueError) -> HandlerError {
    HandlerError::new(ToolError::upstream())
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, fmt, future::Future, time::Duration};

    use anytype::prelude::{AnytypeClient, ClientConfig, HttpCredentials};
    use anytype::test_util::{
        DisposableRun, retry_definitive_rate_limit, unique_suffix, with_disposable_space_context,
    };
    use rmcp::model::ToolAnnotations;
    use serde_json::{Value, json};
    use sha2::{Digest, Sha256};
    use tiktoken_rs::o200k_base;
    use tokio::io::{
        AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader, duplex, split,
    };

    use super::*;
    use crate::{
        ApplicationProfile,
        error::ToolErrorCode,
        optional_toolsets::{
            OptionalRegistryFuture, OptionalToolsetMetadata, OptionalToolsetRegistry,
            OptionalToolsetSelection, production_optional_metadata,
        },
        runtime::{RuntimeContext, StartupStatus},
        schema::{input_schema, output_schema},
        server::AnyMcpServer,
    };

    const SPACE_ID: &str = "bafyreic-space";
    const COLLECTION_ID: &str = "bafyreic-collection";
    const OTHER_COLLECTION_ID: &str = "bafyreic-other";
    const SUFFIX_ALPHABET: &[u8] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789._~-";

    fn binding(space_id: &str, collection_id: &str, limit: u16) -> QueryFingerprint {
        page_query_fingerprint(
            COLLECTION_MEMBER_LIST,
            PageLimit::new(limit).expect("valid common limit"),
            &ResolvedPageParams {
                registry: VIEWS_WRITE_REGISTRY,
                space_id,
                collection_id,
            },
        )
        .expect("bounded fingerprint")
    }

    fn api_page(
        offset: u64,
        total: u64,
        object_ids: &[&str],
        continuation: Option<CollectionMembershipContinuation>,
    ) -> ApiCollectionMembershipPage {
        ApiCollectionMembershipPage {
            space_id: SPACE_ID.to_owned(),
            collection_id: COLLECTION_ID.to_owned(),
            offset,
            total,
            object_ids: object_ids.iter().map(|id| (*id).to_owned()).collect(),
            continuation,
        }
    }

    fn continued_page() -> ApiCollectionMembershipPage {
        api_page(
            0,
            2,
            &["object-a"],
            Some(CollectionMembershipContinuation {
                next_offset: 1,
                total: 2,
                final_object_id: "object-a".to_owned(),
            }),
        )
    }

    fn error_code(error: &HandlerError) -> ToolErrorCode {
        error.tool_error().code()
    }

    #[test]
    fn wire_contract_is_exact_strict_and_domain_bounded() {
        let tool = collection_member_list_tool().expect("valid contract");
        let metadata = tool.as_tool();
        assert_eq!(metadata.name, COLLECTION_MEMBER_LIST);
        assert_eq!(
            metadata.annotations,
            Some(
                ToolAnnotations::new()
                    .read_only(true)
                    .destructive(false)
                    .open_world(false)
            )
        );

        let input = input_schema::<CollectionMemberListInput>().expect("input schema");
        assert_eq!(input["additionalProperties"], false);
        assert_eq!(input["required"], json!(["space", "collection_id"]));
        assert_eq!(input["$defs"]["PageLimit"]["minimum"], 1);
        assert_eq!(input["$defs"]["PageLimit"]["maximum"], 61);
        assert_eq!(input["$defs"]["SpaceRef"]["minLength"], 1);
        assert_eq!(input["$defs"]["SpaceRef"]["maxLength"], 512);
        assert_eq!(input["properties"]["space"]["$ref"], "#/$defs/SpaceRef");
        let cursor_schema = &input["properties"]["cursor"];
        assert_eq!(cursor_schema["$ref"], "#/$defs/CursorToken");
        assert!(!cursor_schema.to_string().contains("null"));

        let output = output_schema::<CollectionMemberListPage>().expect("output schema");
        assert_eq!(output["additionalProperties"], false);
        assert_eq!(output["properties"]["items"]["maxItems"], 61);
        assert_eq!(
            output["$defs"]["CollectionMemberSummary"]["properties"]
                .as_object()
                .expect("summary properties")
                .keys()
                .collect::<Vec<_>>(),
            ["object_id"]
        );
        assert_eq!(collection_member_list_tools().expect("tool slice").len(), 1);
    }

    #[test]
    fn input_rejects_null_unknown_query_fields_and_limit_boundaries() {
        for (value, accepted_limit) in [
            (
                json!({"space":SPACE_ID,"collection_id":COLLECTION_ID}),
                Some(20),
            ),
            (
                json!({"space":SPACE_ID,"collection_id":COLLECTION_ID,"limit":1}),
                Some(1),
            ),
            (
                json!({"space":SPACE_ID,"collection_id":COLLECTION_ID,"limit":61}),
                Some(61),
            ),
            (
                json!({"space":SPACE_ID,"collection_id":COLLECTION_ID,"limit":0}),
                None,
            ),
            (
                json!({"space":SPACE_ID,"collection_id":COLLECTION_ID,"limit":62}),
                None,
            ),
        ] {
            let decoded = serde_json::from_value::<CollectionMemberListInput>(value);
            match accepted_limit {
                Some(limit) => assert_eq!(decoded.expect("accepted input").limit.get(), limit),
                None => assert!(decoded.is_err()),
            }
        }

        for value in [
            json!({"space":SPACE_ID,"collection_id":COLLECTION_ID,"cursor":null}),
            json!({"space":SPACE_ID,"collection_id":COLLECTION_ID,"filter":{}}),
            json!({"space":SPACE_ID,"collection_id":COLLECTION_ID,"view_id":"view"}),
            json!({"space":SPACE_ID,"collection_id":COLLECTION_ID,"sort":[]}),
            json!({"space":SPACE_ID,"collection_id":COLLECTION_ID,"layout":"kanban"}),
            json!({"space":"   ","collection_id":COLLECTION_ID}),
            json!({"space":SPACE_ID,"collection_id":"bad/id"}),
        ] {
            assert!(
                serde_json::from_value::<CollectionMemberListInput>(value).is_err(),
                "unexpectedly accepted strict input"
            );
        }
    }

    #[test]
    fn converter_minimizes_order_and_round_trips_evidence_cursor() {
        let cursors = CursorStore::new().expect("cursor store");
        let fingerprint = binding(SPACE_ID, COLLECTION_ID, 1);
        let first = convert_page(
            &cursors,
            continued_page(),
            SPACE_ID,
            COLLECTION_ID,
            0,
            CollectionMemberPageLimit::new(1).expect("limit"),
            fingerprint,
        )
        .expect("valid first page");
        assert_eq!(
            serde_json::to_value(&first).expect("serialize page")["items"],
            json!([{"object_id":"object-a"}])
        );
        let token = first.next_cursor().expect("continuation");
        let resolved =
            resolve_continuation(&cursors, token, fingerprint).expect("resolve evidence");
        assert_eq!(
            resolved,
            CollectionMembershipContinuation {
                next_offset: 1,
                total: 2,
                final_object_id: "object-a".to_owned(),
            }
        );

        let terminal = convert_page(
            &cursors,
            api_page(1, 2, &["object-b"], None),
            SPACE_ID,
            COLLECTION_ID,
            resolved.next_offset,
            CollectionMemberPageLimit::new(1).expect("limit"),
            fingerprint,
        )
        .expect("valid terminal page");
        assert_eq!(
            terminal
                .items()
                .iter()
                .map(|item| item.object_id.as_str())
                .collect::<Vec<_>>(),
            ["object-b"]
        );
        assert!(terminal.next_cursor().is_none());
        assert_eq!(cursors.entry_count(), 1);
    }

    #[test]
    fn cursor_binds_operation_registry_space_collection_limit_and_state_kind() {
        let cursors = CursorStore::new().expect("cursor store");
        let exact = binding(SPACE_ID, COLLECTION_ID, 1);
        let page = convert_page(
            &cursors,
            continued_page(),
            SPACE_ID,
            COLLECTION_ID,
            0,
            CollectionMemberPageLimit::new(1).expect("limit"),
            exact,
        )
        .expect("page");
        let token = page.next_cursor().expect("cursor");
        for mismatch in [
            binding("other-space", COLLECTION_ID, 1),
            binding(SPACE_ID, OTHER_COLLECTION_ID, 1),
            binding(SPACE_ID, COLLECTION_ID, 2),
            QueryFingerprint::from_normalized(&json!({
                "tool":"different",
                "registry":VIEWS_WRITE_REGISTRY,
                "space_id":SPACE_ID,
                "collection_id":COLLECTION_ID,
                "limit":1
            }))
            .expect("different operation"),
        ] {
            let error = resolve_continuation(&cursors, token, mismatch).expect_err("mismatch");
            assert_eq!(error_code(&error), ToolErrorCode::Validation);
        }
        assert!(cursors.resolve(token, exact).is_err());

        let ordinary = cursors
            .issue(PageOffset::new(1).expect("offset"), exact)
            .expect("ordinary cursor");
        assert!(resolve_continuation(&cursors, &ordinary, exact).is_err());
    }

    #[test]
    fn converter_fails_closed_on_malformed_or_shifted_evidence() {
        let limit = CollectionMemberPageLimit::new(2).expect("limit");
        let cases = [
            api_page(1, 1, &["object-a"], None),
            api_page(0, 2, &["object-a"], None),
            api_page(0, 1, &["object-a", "object-b"], None),
            api_page(0, 2, &["object-a", "object-a"], None),
            api_page(0, 1, &["bad/id"], None),
            api_page(
                0,
                2,
                &["object-a"],
                Some(CollectionMembershipContinuation {
                    next_offset: 2,
                    total: 2,
                    final_object_id: "object-a".to_owned(),
                }),
            ),
            api_page(
                0,
                2,
                &["object-a"],
                Some(CollectionMembershipContinuation {
                    next_offset: 1,
                    total: 3,
                    final_object_id: "object-a".to_owned(),
                }),
            ),
            api_page(
                0,
                2,
                &["object-a"],
                Some(CollectionMembershipContinuation {
                    next_offset: 1,
                    total: 2,
                    final_object_id: "object-b".to_owned(),
                }),
            ),
        ];
        for page in cases {
            let cursors = CursorStore::new().expect("cursor store");
            assert!(
                convert_page(
                    &cursors,
                    page,
                    SPACE_ID,
                    COLLECTION_ID,
                    0,
                    limit,
                    binding(SPACE_ID, COLLECTION_ID, 2),
                )
                .is_err()
            );
            assert_eq!(cursors.entry_count(), 0);
        }

        let cursors = CursorStore::new().expect("cursor store");
        let mut wrong_identity = api_page(0, 0, &[], None);
        wrong_identity.space_id = "other-space".to_owned();
        assert!(
            convert_page(
                &cursors,
                wrong_identity,
                SPACE_ID,
                COLLECTION_ID,
                0,
                limit,
                binding(SPACE_ID, COLLECTION_ID, 2),
            )
            .is_err()
        );

        let cursors = CursorStore::new().expect("cursor store");
        assert!(
            convert_page(
                &cursors,
                api_page(2, 2, &[], None),
                SPACE_ID,
                COLLECTION_ID,
                2,
                limit,
                binding(SPACE_ID, COLLECTION_ID, 2),
            )
            .is_err(),
            "continued pages cannot manufacture terminality from an empty window"
        );
    }

    fn maximum_id(index: usize) -> String {
        let left = SUFFIX_ALPHABET[index / SUFFIX_ALPHABET.len()];
        let right = SUFFIX_ALPHABET[index % SUFFIX_ALPHABET.len()];
        format!(
            "{}{}{}",
            "~z".repeat(127),
            char::from(left),
            char::from(right)
        )
    }

    fn maximum_page(count: usize) -> CollectionMemberListPage {
        let mut ids = (0..count).map(maximum_id).collect::<Vec<_>>();
        ids.sort();
        CollectionMemberListPage {
            items: ids
                .into_iter()
                .map(|id| CollectionMemberSummary {
                    object_id: EntityId::new(id).expect("valid maximum ID"),
                })
                .collect(),
            next_cursor: None,
        }
    }

    fn canonical_json(value: Value) -> Value {
        match value {
            Value::Object(map) => Value::Object(
                map.into_iter()
                    .map(|(key, value)| (key, canonical_json(value)))
                    .collect::<BTreeMap<_, _>>()
                    .into_iter()
                    .collect(),
            ),
            Value::Array(values) => Value::Array(values.into_iter().map(canonical_json).collect()),
            scalar => scalar,
        }
    }

    #[test]
    fn sixty_one_item_adversarial_result_is_inside_locked_byte_and_token_budget() {
        let page = maximum_page(61);
        assert!(
            page.items()
                .windows(2)
                .all(|window| window[0].object_id < window[1].object_id)
        );
        assert!(
            page.items()
                .iter()
                .all(|item| item.object_id.as_str().len() == 256)
        );
        let result = collection_member_list_tool()
            .expect("contract")
            .success(&page)
            .expect("encoded maximum result");
        let compact = serde_json::to_string(&canonical_json(
            serde_json::to_value(result).expect("result JSON"),
        ))
        .expect("compact canonical JSON");
        assert_eq!(compact.len(), 33_650);
        let tokenizer = o200k_base().expect("pinned o200k_base");
        let token_count = tokenizer.encode_with_special_tokens(&compact).len();
        assert_eq!(token_count, 31_770);
        assert!(compact.len() <= 65_536);
        assert!(token_count <= 32_000);

        let over_limit = maximum_page(62);
        let over_result = collection_member_list_tool()
            .expect("contract")
            .success(&over_limit)
            .expect("hypothetical over-limit encoding");
        let over_compact = serde_json::to_string(&canonical_json(
            serde_json::to_value(over_result).expect("over-limit result JSON"),
        ))
        .expect("compact over-limit canonical JSON");
        let over_tokens = tokenizer.encode_with_special_tokens(&over_compact).len();
        assert_eq!(
            json!({
                "tokenizer":"tiktoken-rs 0.12.0",
                "encoding":"o200k_base",
                "suffix_alphabet":String::from_utf8_lossy(SUFFIX_ALPHABET),
                "maximum":{"items":61,"bytes":compact.len(),"tokens":token_count},
                "rejected":{"items":62,"bytes":over_compact.len(),"tokens":over_tokens},
                "duplicates":"text and structured content"
            }),
            json!({
                "tokenizer":"tiktoken-rs 0.12.0",
                "encoding":"o200k_base",
                "suffix_alphabet":"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789._~-",
                "maximum":{"items":61,"bytes":33_650,"tokens":31_770},
                "rejected":{"items":62,"bytes":34_200,"tokens":32_292},
                "duplicates":"text and structured content"
            })
        );
        assert_eq!(
            CollectionMemberListPage::new(over_limit.items, None)
                .expect_err("62-item page must fail")
                .tool_error()
                .code(),
            ToolErrorCode::BoundedResult
        );
    }

    #[test]
    fn reviewed_work_and_catalog_ceilings_are_locked() {
        assert_eq!(COLLECTION_MEMBER_LIST_HTTP_LOGICAL_CEILING, 12);
        assert_eq!(COLLECTION_MEMBER_LIST_HTTP_PHYSICAL_CEILING, 72);
        assert_eq!(COLLECTION_MEMBER_LIST_GRPC_CEILING, 3);
        assert_eq!(VIEWS_WRITE_CATALOG_TOKEN_CEILING, 3_000);
        assert_eq!(
            Sha256::digest(SUFFIX_ALPHABET)
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>(),
            "cc16002a9aaa4e9b1c89a511f124cc518387b128c5eec79b39dc75ecab16a4ab"
        );
    }

    struct TestRegistry {
        handlers: CollectionMemberListHandlers,
    }

    impl fmt::Debug for TestRegistry {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("TestViewsWriteCollectionListRegistry")
        }
    }

    impl OptionalToolsetRegistry for TestRegistry {
        fn metadata(&self) -> OptionalToolsetMetadata {
            OptionalToolsetMetadata::new(VIEWS_WRITE_REGISTRY, true)
        }

        fn tools(&self) -> Result<Vec<OptionalRegistryTool>, SchemaContractError> {
            collection_member_list_tools()
        }

        fn scripted_scenario_ids(&self) -> &'static [&'static str] {
            &[
                "collection_member_list_direct",
                "collection_member_list_stdio",
            ]
        }

        fn headless_scenario_ids(&self) -> &'static [&'static str] {
            &["collection_member_list_headless"]
        }

        fn catalog_token_ceiling(&self) -> usize {
            VIEWS_WRITE_CATALOG_TOKEN_CEILING
        }

        fn call_tool<'a>(
            &'a self,
            request: CallToolRequestParams,
            runtime: &'a RuntimeContext,
            cursors: &'a CursorStore,
            _protocol_version: &'a rmcp::model::ProtocolVersion,
            cancellation: &'a CancellationToken,
        ) -> OptionalRegistryFuture<'a, Result<CallToolResult, ErrorData>> {
            Box::pin(
                self.handlers
                    .call_tool(request, runtime, cursors, cancellation),
            )
        }
    }

    fn no_io_runtime(selected: bool, read_only: bool) -> RuntimeContext {
        let client = AnytypeClient::with_config(ClientConfig {
            base_url: Some("http://127.0.0.1:1".to_owned()),
            keystore: Some("env".to_owned()),
            keystore_service: Some("collection-list-no-io".to_owned()),
            app_name: "collection-list-no-io".to_owned(),
            ..ClientConfig::default()
        })
        .expect("no-I/O client");
        client.set_api_key(HttpCredentials::new("unused-no-io-token"));
        let available = [OptionalToolsetMetadata::new(VIEWS_WRITE_REGISTRY, true)];
        let selection = OptionalToolsetSelection::parse(
            selected.then(|| VIEWS_WRITE_REGISTRY.to_owned()),
            &available,
        )
        .expect("views-write selection");
        RuntimeContext::from_parts_with_profile_and_optional_toolsets(
            client,
            1,
            Duration::from_secs(2),
            StartupStatus {
                http_available: true,
                grpc_available: true,
            },
            ApplicationProfile::Compact,
            read_only,
            selection,
        )
    }

    fn test_server(selected: bool, read_only: bool) -> AnyMcpServer {
        server_with_runtime(no_io_runtime(selected, read_only))
    }

    fn server_with_runtime(runtime: RuntimeContext) -> AnyMcpServer {
        let registry: &'static TestRegistry = Box::leak(Box::new(TestRegistry {
            handlers: CollectionMemberListHandlers::new().expect("handlers"),
        }));
        let registries: &'static [&'static dyn OptionalToolsetRegistry] =
            Box::leak(vec![registry as &dyn OptionalToolsetRegistry].into_boxed_slice());
        AnyMcpServer::new_with_optional_registries(runtime, registries).expect("test server")
    }

    fn live_runtime(client: AnytypeClient, read_only: bool) -> RuntimeContext {
        let selection = OptionalToolsetSelection::parse(
            Some(VIEWS_WRITE_REGISTRY.to_owned()),
            &[OptionalToolsetMetadata::new(VIEWS_WRITE_REGISTRY, true)],
        )
        .expect("views-write selection");
        RuntimeContext::from_parts_with_profile_and_optional_toolsets(
            client,
            2,
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

    fn arguments(value: Value) -> serde_json::Map<String, Value> {
        value.as_object().cloned().expect("object arguments")
    }

    fn metric_counts(client: &AnytypeClient) -> (u64, u64) {
        let metrics = client.http_metrics();
        (metrics.logical_operations, metrics.physical_attempts)
    }

    fn assert_list_work(before: (u64, u64), after: (u64, u64)) {
        assert!(after.0 > before.0);
        assert_list_ceiling(before, after);
    }

    fn assert_list_ceiling(before: (u64, u64), after: (u64, u64)) {
        assert!(after.0 - before.0 <= COLLECTION_MEMBER_LIST_HTTP_LOGICAL_CEILING as u64);
        assert!(after.1 - before.1 <= COLLECTION_MEMBER_LIST_HTTP_PHYSICAL_CEILING as u64);
    }

    async fn direct_call(server: &AnyMcpServer, value: Value) -> CallToolResult {
        server
            .dispatch_tool(
                CallToolRequestParams::new(COLLECTION_MEMBER_LIST).with_arguments(arguments(value)),
                &CancellationToken::new(),
            )
            .await
            .expect("direct router dispatch")
    }

    async fn preview_stdio_call(server: AnyMcpServer, value: Value) -> Value {
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
            "id":9,
            "method":"tools/call",
            "params":{
                "name":COLLECTION_MEMBER_LIST,
                "arguments":value,
                "_meta":{
                    "io.modelcontextprotocol/protocolVersion":"2026-07-28",
                    "io.modelcontextprotocol/clientInfo":{
                        "name":"collection-list-test",
                        "version":"1"
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
        serde_json::from_str(&line).expect("decode stdio response")
    }

    async fn write_stdio_frame<W, R>(
        writer: &mut W,
        reader: &mut BufReader<R>,
        id: u64,
        arguments: Value,
    ) -> Value
    where
        W: AsyncWrite + Unpin,
        R: AsyncRead + Unpin,
    {
        let frame = json!({
            "jsonrpc":"2.0",
            "id":id,
            "method":"tools/call",
            "params":{
                "name":COLLECTION_MEMBER_LIST,
                "arguments":arguments,
                "_meta":{
                    "io.modelcontextprotocol/protocolVersion":"2026-07-28",
                    "io.modelcontextprotocol/clientInfo":{
                        "name":"collection-list-walk-test",
                        "version":"1"
                    },
                    "io.modelcontextprotocol/clientCapabilities":{}
                }
            }
        });
        writer
            .write_all(format!("{frame}\n").as_bytes())
            .await
            .expect("write stdio walk request");
        let mut line = String::new();
        reader
            .read_line(&mut line)
            .await
            .expect("read stdio walk response");
        serde_json::from_str(&line).expect("decode stdio walk response")
    }

    async fn preview_stdio_walk(
        server: AnyMcpServer,
        metrics_client: &AnytypeClient,
        space_id: &str,
        collection_id: &str,
        other_collection_id: &str,
    ) -> Vec<Value> {
        let (client_io, server_io) = duplex(64 * 1024);
        let (server_reader, server_writer) = split(server_io);
        let task = tokio::spawn(crate::stdio::serve_preview(
            server,
            BufReader::new(server_reader),
            server_writer,
        ));
        let (client_reader, mut client_writer) = split(client_io);
        let mut client_reader = BufReader::new(client_reader);

        let rejected_before = metric_counts(metrics_client);
        let rejected = write_stdio_frame(
            &mut client_writer,
            &mut client_reader,
            21,
            json!({"space":space_id,"collection_id":other_collection_id,"limit":1}),
        )
        .await;
        assert_list_work(rejected_before, metric_counts(metrics_client));

        let first_before = metric_counts(metrics_client);
        let first = write_stdio_frame(
            &mut client_writer,
            &mut client_reader,
            22,
            json!({"space":space_id,"collection_id":collection_id,"limit":1}),
        )
        .await;
        assert_list_work(first_before, metric_counts(metrics_client));
        let cursor = first["result"]["structuredContent"]["next_cursor"]
            .as_str()
            .expect("stdio walk continuation")
            .to_owned();

        let mismatch_before = metric_counts(metrics_client);
        let mismatch = write_stdio_frame(
            &mut client_writer,
            &mut client_reader,
            23,
            json!({
                "space":space_id,
                "collection_id":other_collection_id,
                "limit":1,
                "cursor":cursor
            }),
        )
        .await;
        assert_list_ceiling(mismatch_before, metric_counts(metrics_client));

        let second_before = metric_counts(metrics_client);
        let second = write_stdio_frame(
            &mut client_writer,
            &mut client_reader,
            24,
            json!({
                "space":space_id,
                "collection_id":collection_id,
                "limit":1,
                "cursor":cursor
            }),
        )
        .await;
        assert_list_work(second_before, metric_counts(metrics_client));
        drop(client_writer);
        drop(client_reader);
        task.await
            .expect("spawned stdio walk task")
            .expect("stdio walk transport");
        vec![rejected, first, mismatch, second]
    }

    fn run_large_future<F, Fut>(test: F)
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        std::thread::Builder::new()
            .name("collection-list-live".to_owned())
            .stack_size(8 * 1024 * 1024)
            .spawn(move || {
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("collection-list runtime")
                    .block_on(test());
            })
            .expect("spawn collection-list test thread")
            .join()
            .expect("collection-list test thread");
    }

    #[test]
    fn registry_is_grpc_gated_read_only_and_production_unlinked() {
        let metadata = TestRegistry {
            handlers: CollectionMemberListHandlers::new().expect("handlers"),
        }
        .metadata();
        assert_eq!(metadata.name, VIEWS_WRITE_REGISTRY);
        assert!(metadata.requires_grpc);
        assert!(
            !production_optional_metadata()
                .iter()
                .any(|entry| entry.name == VIEWS_WRITE_REGISTRY)
        );

        let base = test_server(false, false);
        let selected = test_server(true, false);
        let read_only = test_server(true, true);
        let names = |server: &AnyMcpServer| {
            server
                .tools()
                .iter()
                .map(|tool| tool.name.to_string())
                .collect::<Vec<_>>()
        };
        assert!(
            !names(&base)
                .iter()
                .any(|name| name == COLLECTION_MEMBER_LIST)
        );
        assert!(
            names(&selected)
                .iter()
                .any(|name| name == COLLECTION_MEMBER_LIST)
        );
        assert!(
            names(&read_only)
                .iter()
                .any(|name| name == COLLECTION_MEMBER_LIST)
        );
        assert_eq!(selected.tools().len(), read_only.tools().len() + 1);

        let tokenizer = o200k_base().expect("pinned o200k_base");
        let tool = selected
            .tools()
            .iter()
            .find(|tool| tool.name == COLLECTION_MEMBER_LIST)
            .expect("selected list tool");
        let catalog_contribution = tokenizer
            .encode_with_special_tokens(
                &serde_json::to_string(&canonical_json(
                    serde_json::to_value(tool).expect("tool JSON"),
                ))
                .expect("compact tool JSON"),
            )
            .len();
        assert_eq!(catalog_contribution, 579);
        assert!(catalog_contribution <= VIEWS_WRITE_CATALOG_TOKEN_CEILING);
    }

    #[test]
    #[ignore = "requires env-only disposable credentials and an authenticated headless Anytype server"]
    fn live_direct_and_production_stdio_ignore_saved_view_presentation() {
        run_large_future(|| async {
            let outcome = Box::pin(with_disposable_space_context(
                "any-mcp-collection-list",
                |ctx| {
                    Box::pin(async move {
                        ctx.client.ping_http().await?;
                        ctx.client.ping_grpc().await?;
                        let suffix = unique_suffix();
                        let collection_type = ctx
                            .create_collection_type_fixture(format!("MCP Collection {suffix}"))
                            .await?;
                        let collection = ctx
                            .create_collection_fixture(
                                &collection_type,
                                format!("MCP Members {suffix}"),
                            )
                            .await?;
                        let name_a = format!("MCP Member A {suffix}");
                        let object_a = retry_definitive_rate_limit("MCP member A", || async {
                            ctx.client
                                .new_object(&ctx.space_id, "page")
                                .name(&name_a)
                                .create()
                                .await
                        })
                        .await?;
                        ctx.register_object(&object_a.id);
                        let object_b = retry_definitive_rate_limit("MCP member B", || async {
                            ctx.client
                                .new_object(&ctx.space_id, "page")
                                .name(format!("MCP Member B {suffix}"))
                                .create()
                                .await
                        })
                        .await?;
                        ctx.register_object(&object_b.id);
                        let object_c = retry_definitive_rate_limit("MCP nonmember C", || async {
                            ctx.client
                                .new_object(&ctx.space_id, "page")
                                .name(format!("MCP Nonmember C {suffix}"))
                                .create()
                                .await
                        })
                        .await?;
                        ctx.register_object(&object_c.id);
                        let set_type = ctx
                            .client
                            .types(&ctx.space_id)
                            .list()
                            .await?
                            .items
                            .iter()
                            .find(|typ| typ.layout == anytype::objects::ObjectLayout::Set)
                            .cloned()
                            .ok_or_else(|| anytype::test_util::TestError::Assertion {
                                message: "disposable space has no Set-layout type".to_owned(),
                            })?;
                        let query = retry_definitive_rate_limit("MCP set rejection", || async {
                            ctx.client
                                .new_object(&ctx.space_id, &set_type.key)
                                .name(format!("MCP Query {suffix}"))
                                .create()
                                .await
                        })
                        .await?;
                        ctx.register_object(&query.id);

                        ctx.client
                            .view_add_objects(
                                &ctx.space_id,
                                &collection.id,
                                [&object_a.id, &object_b.id],
                            )
                            .await?;
                        let saved_view = ctx
                            .create_collection_view_fixture(
                                &collection.id,
                                format!("Only A {suffix}"),
                            )
                            .await?;
                        ctx.add_collection_name_filter_fixture(
                            &collection.id,
                            &saved_view.id,
                            &name_a,
                        )
                        .await?;
                        let visible = ctx
                            .client
                            .view_list_objects(&ctx.space_id, &collection.id)
                            .view(&saved_view.id)
                            .limit(61)
                            .list()
                            .await?;
                        let visible_ids = visible
                            .items
                            .iter()
                            .map(|object| object.id.as_str())
                            .collect::<Vec<_>>();
                        assert!(visible_ids.contains(&object_a.id.as_str()));
                        assert!(!visible_ids.contains(&object_b.id.as_str()));
                        let kanban = ctx
                            .create_kanban_fixture(format!("MCP Kanban {suffix}"))
                            .await?;
                        let kanban_reference = ctx
                            .client
                            .collection_membership_page(
                                &ctx.space_id,
                                &kanban.collection.id,
                                61,
                                None,
                            )
                            .await?;
                        assert_eq!(kanban_reference.object_ids.len(), kanban.items.len());
                        assert!(
                            kanban
                                .items
                                .iter()
                                .all(|item| kanban_reference.object_ids.contains(&item.object.id))
                        );

                        for (object_id, expected) in [
                            (
                                object_a.id.as_str(),
                                anytype::views::CollectionMembershipState::Present,
                            ),
                            (
                                object_b.id.as_str(),
                                anytype::views::CollectionMembershipState::Present,
                            ),
                            (
                                object_c.id.as_str(),
                                anytype::views::CollectionMembershipState::Absent,
                            ),
                        ] {
                            let observed = ctx
                                .client
                                .observe_collection_membership(
                                    &ctx.space_id,
                                    &collection.id,
                                    object_id,
                                )
                                .await?;
                            assert_eq!(observed.state, expected);
                        }

                        let reference = ctx
                            .client
                            .collection_membership_page(&ctx.space_id, &collection.id, 61, None)
                            .await?;
                        assert!(reference.continuation.is_none());
                        assert_eq!(reference.object_ids.len(), 2);

                        let direct_server =
                            server_with_runtime(live_runtime(ctx.client.clone(), false));
                        let query_before = metric_counts(&ctx.client);
                        let rejected_query = direct_call(
                            &direct_server,
                            json!({
                                "space":ctx.space_id,
                                "collection_id":query.id,
                                "limit":1
                            }),
                        )
                        .await;
                        assert_eq!(rejected_query.is_error, Some(true));
                        assert_eq!(
                            rejected_query
                                .structured_content
                                .as_ref()
                                .and_then(|value| value.get("code"))
                                .and_then(Value::as_str),
                            Some("upstream")
                        );
                        assert_list_work(query_before, metric_counts(&ctx.client));

                        let kanban_before = metric_counts(&ctx.client);
                        let kanban_direct = direct_call(
                            &direct_server,
                            json!({
                                "space":ctx.space_id,
                                "collection_id":kanban.collection.id,
                                "limit":61
                            }),
                        )
                        .await;
                        assert_list_work(kanban_before, metric_counts(&ctx.client));
                        assert_eq!(kanban_direct.is_error, Some(false));
                        let kanban_direct_ids = kanban_direct
                            .structured_content
                            .as_ref()
                            .and_then(|value| value["items"].as_array())
                            .into_iter()
                            .flatten()
                            .filter_map(|item| item["object_id"].as_str().map(str::to_owned))
                            .collect::<Vec<_>>();
                        assert_eq!(
                            kanban_direct_ids.as_slice(),
                            kanban_reference.object_ids.as_slice()
                        );

                        let before = metric_counts(&ctx.client);
                        let first = direct_call(
                            &direct_server,
                            json!({
                                "space":ctx.space_id,
                                "collection_id":collection.id,
                                "limit":1
                            }),
                        )
                        .await;
                        assert_eq!(first.is_error, Some(false));
                        assert_list_work(before, metric_counts(&ctx.client));
                        let first_value = first
                            .structured_content
                            .as_ref()
                            .expect("direct first structured content");
                        let cursor = first_value["next_cursor"]
                            .as_str()
                            .expect("direct continuation")
                            .to_owned();
                        let mismatch = direct_call(
                            &direct_server,
                            json!({
                                "space":ctx.space_id,
                                "collection_id":collection.id,
                                "limit":2,
                                "cursor":cursor
                            }),
                        )
                        .await;
                        assert_eq!(mismatch.is_error, Some(true));
                        assert_eq!(
                            mismatch
                                .structured_content
                                .as_ref()
                                .and_then(|value| { value.get("code").and_then(Value::as_str) }),
                            Some("validation")
                        );
                        let second_before = metric_counts(&ctx.client);
                        let second = direct_call(
                            &direct_server,
                            json!({
                                "space":ctx.space_id,
                                "collection_id":collection.id,
                                "limit":1,
                                "cursor":cursor
                            }),
                        )
                        .await;
                        assert_eq!(second.is_error, Some(false));
                        assert_list_work(second_before, metric_counts(&ctx.client));
                        assert!(
                            second
                                .structured_content
                                .as_ref()
                                .and_then(|value| value.get("next_cursor"))
                                .is_none()
                        );
                        let walked = [first, second]
                            .iter()
                            .flat_map(|result| {
                                result
                                    .structured_content
                                    .as_ref()
                                    .and_then(|value| value["items"].as_array())
                                    .into_iter()
                                    .flatten()
                                    .filter_map(|item| {
                                        item["object_id"].as_str().map(str::to_owned)
                                    })
                            })
                            .collect::<Vec<_>>();
                        assert_eq!(walked, reference.object_ids);
                        assert!(walked.contains(&object_b.id));

                        let stdio_walk = preview_stdio_walk(
                            server_with_runtime(live_runtime(ctx.client.clone(), false)),
                            &ctx.client,
                            &ctx.space_id,
                            &collection.id,
                            &query.id,
                        )
                        .await;
                        assert_eq!(
                            stdio_walk[0]["result"]["structuredContent"]["code"],
                            "upstream"
                        );
                        assert_eq!(stdio_walk[1]["result"]["isError"], false);
                        assert_eq!(
                            stdio_walk[2]["result"]["structuredContent"]["code"],
                            "validation"
                        );
                        assert_eq!(stdio_walk[3]["result"]["isError"], false);
                        assert!(
                            stdio_walk[3]["result"]["structuredContent"]
                                .get("next_cursor")
                                .is_none()
                        );
                        let restarted = [&stdio_walk[1], &stdio_walk[3]]
                            .into_iter()
                            .flat_map(|response| {
                                response["result"]["structuredContent"]["items"]
                                    .as_array()
                                    .into_iter()
                                    .flatten()
                                    .filter_map(|item| {
                                        item["object_id"].as_str().map(str::to_owned)
                                    })
                            })
                            .collect::<Vec<_>>();
                        assert_eq!(restarted.as_slice(), reference.object_ids.as_slice());

                        let stdio_before = metric_counts(&ctx.client);
                        let stdio = preview_stdio_call(
                            server_with_runtime(live_runtime(ctx.client.clone(), false)),
                            json!({
                                "space":ctx.space_id,
                                "collection_id":collection.id,
                                "limit":61
                            }),
                        )
                        .await;
                        assert_list_work(stdio_before, metric_counts(&ctx.client));
                        assert_eq!(stdio["result"]["isError"], false, "{stdio}");
                        let stdio_ids = stdio["result"]["structuredContent"]["items"]
                            .as_array()
                            .expect("stdio items")
                            .iter()
                            .filter_map(|item| item["object_id"].as_str().map(str::to_owned))
                            .collect::<Vec<_>>();
                        assert_eq!(stdio_ids.as_slice(), reference.object_ids.as_slice());
                        assert!(stdio_ids.contains(&object_b.id));

                        let kanban_stdio_before = metric_counts(&ctx.client);
                        let kanban_stdio = preview_stdio_call(
                            server_with_runtime(live_runtime(ctx.client.clone(), false)),
                            json!({
                                "space":ctx.space_id,
                                "collection_id":kanban.collection.id,
                                "limit":61
                            }),
                        )
                        .await;
                        assert_list_work(kanban_stdio_before, metric_counts(&ctx.client));
                        assert_eq!(kanban_stdio["result"]["isError"], false);
                        let kanban_stdio_ids = kanban_stdio["result"]["structuredContent"]["items"]
                            .as_array()
                            .expect("Kanban stdio items")
                            .iter()
                            .filter_map(|item| item["object_id"].as_str().map(str::to_owned))
                            .collect::<Vec<_>>();
                        assert_eq!(
                            kanban_stdio_ids.as_slice(),
                            kanban_reference.object_ids.as_slice()
                        );

                        let read_only_before = metric_counts(&ctx.client);
                        let read_only = preview_stdio_call(
                            server_with_runtime(live_runtime(ctx.client.clone(), true)),
                            json!({
                                "space":ctx.space_id,
                                "collection_id":collection.id,
                                "limit":61
                            }),
                        )
                        .await;
                        assert_list_work(read_only_before, metric_counts(&ctx.client));
                        assert_eq!(read_only["result"]["isError"], false, "{read_only}");
                        let read_only_ids = read_only["result"]["structuredContent"]["items"]
                            .as_array()
                            .expect("read-only stdio items")
                            .iter()
                            .filter_map(|item| item["object_id"].as_str().map(str::to_owned))
                            .collect::<Vec<_>>();
                        assert_eq!(read_only_ids.as_slice(), reference.object_ids.as_slice());

                        let prefix = std::env::var("ANYTYPE_TEST_SPACE_PREFIX")
                            .expect("disposable prefix admitted before callback");
                        let ambiguous_name = format!("{prefix}-mcp-list-ambiguous-{suffix}");
                        let ambiguous_first = ctx.create_space_fixture(&ambiguous_name).await?;
                        let ambiguous_second = ctx.create_space_fixture(&ambiguous_name).await?;
                        assert_ne!(ambiguous_first.id, ambiguous_second.id);
                        ctx.client.cache().clear_spaces();
                        let ambiguity_before = metric_counts(&ctx.client);
                        let ambiguity = direct_call(
                            &direct_server,
                            json!({
                                "space":ambiguous_name,
                                "collection_id":collection.id,
                                "limit":1
                            }),
                        )
                        .await;
                        assert_list_work(ambiguity_before, metric_counts(&ctx.client));
                        assert_eq!(
                            ambiguity
                                .structured_content
                                .as_ref()
                                .and_then(|value| value.get("code"))
                                .and_then(Value::as_str),
                            Some("ambiguous")
                        );
                        ctx.client.cache().clear_spaces();
                        let ambiguity_stdio_before = metric_counts(&ctx.client);
                        let ambiguity_stdio = preview_stdio_call(
                            server_with_runtime(live_runtime(ctx.client.clone(), false)),
                            json!({
                                "space":ambiguous_name,
                                "collection_id":collection.id,
                                "limit":1
                            }),
                        )
                        .await;
                        assert_list_work(ambiguity_stdio_before, metric_counts(&ctx.client));
                        assert_eq!(
                            ambiguity_stdio["result"]["structuredContent"]["code"],
                            "ambiguous"
                        );

                        let mut rejected_config = ctx.client.get_config().clone();
                        rejected_config.app_name = "collection-list-auth-rejection".to_owned();
                        let rejected_client = AnytypeClient::with_config(rejected_config)?;
                        rejected_client.set_api_key(HttpCredentials::new(format!(
                            "invalid-collection-list-{suffix}"
                        )));
                        let auth_before = metric_counts(&rejected_client);
                        let authentication = direct_call(
                            &server_with_runtime(live_runtime(rejected_client.clone(), false)),
                            json!({
                                "space":ctx.space_id,
                                "collection_id":collection.id,
                                "limit":1
                            }),
                        )
                        .await;
                        assert_list_work(auth_before, metric_counts(&rejected_client));
                        assert_eq!(
                            authentication
                                .structured_content
                                .as_ref()
                                .and_then(|value| value.get("code"))
                                .and_then(Value::as_str),
                            Some("authentication")
                        );
                        let auth_stdio_before = metric_counts(&rejected_client);
                        let authentication_stdio = preview_stdio_call(
                            server_with_runtime(live_runtime(rejected_client.clone(), false)),
                            json!({
                                "space":ctx.space_id,
                                "collection_id":collection.id,
                                "limit":1
                            }),
                        )
                        .await;
                        assert_list_work(auth_stdio_before, metric_counts(&rejected_client));
                        assert_eq!(
                            authentication_stdio["result"]["structuredContent"]["code"],
                            "authentication"
                        );
                        Ok(())
                    })
                },
            ))
            .await
            .expect("cleanup-safe live collection-list workflow");
            match outcome {
                DisposableRun::Completed(()) => {}
                DisposableRun::Skipped(reason) => {
                    eprintln!("disposable collection-list suite skipped: {reason:?}");
                }
            }
        });
    }
}
