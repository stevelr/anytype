// any-mcp - bounded, workflow-oriented MCP server for Anytype
//
// SPDX-FileCopyrightText: 2026 Steve Schoettler
// SPDX-License-Identifier: Apache-2.0

//! Transport-neutral execution, encoding, and checked continuation helpers.

use std::{fmt, future::Future};

use anytype::error::AnytypeError;
use rmcp::{model::CallToolResult, schemars::JsonSchema};
use serde::{Serialize, ser::SerializeMap};
use tokio_util::sync::CancellationToken;

use crate::{
    cursor::{CursorStore, CursorStoreError, CursorToken, QueryFingerprint},
    error::{AnytypeErrorMapping, ToolError},
    object_output::ObjectOutputError,
    pagination::{MAX_PAGE_LIMIT, Page, PageLimit, PageOffset},
    protocol::WorkflowTool,
    result::tool_error,
    runtime::{OperationContext, RuntimeContext, RuntimeError},
    validation::ValidationError,
};

/// Secret-safe handler-layer failure ready for MCP result encoding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HandlerError(ToolError);

impl HandlerError {
    /// Creates a handler failure from an already stable tool error.
    #[must_use]
    pub const fn new(error: ToolError) -> Self {
        Self(error)
    }

    /// Borrows the stable caller-visible error.
    #[must_use]
    pub const fn tool_error(&self) -> &ToolError {
        &self.0
    }
}

impl fmt::Display for HandlerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("MCP workflow handler failed")
    }
}

impl std::error::Error for HandlerError {}

impl From<ToolError> for HandlerError {
    fn from(error: ToolError) -> Self {
        Self(error)
    }
}

impl From<ValidationError> for HandlerError {
    fn from(error: ValidationError) -> Self {
        Self(error.tool_error())
    }
}

impl From<ObjectOutputError> for HandlerError {
    fn from(error: ObjectOutputError) -> Self {
        Self(error.tool_error())
    }
}

impl From<CursorStoreError> for HandlerError {
    fn from(_: CursorStoreError) -> Self {
        Self(ToolError::upstream())
    }
}

/// Executes one upstream call and its conversion under runtime controls, then
/// encodes the typed result through its declared workflow contract.
///
/// The conversion future runs inside the runtime timeout/cancellation future,
/// allowing handlers to keep traversal of untrusted upstream values within the
/// same operational boundary. Every failure is converted to fixed text before
/// reaching MCP; neither Anytype errors nor encoder diagnostics are copied.
pub async fn execute_handler<U, O, F, C, CF>(
    runtime: &RuntimeContext,
    contract: &WorkflowTool<O>,
    context: OperationContext,
    cancellation: &CancellationToken,
    operation: F,
    convert: C,
) -> CallToolResult
where
    O: Serialize,
    F: Future<Output = Result<U, AnytypeError>>,
    C: FnOnce(U) -> CF,
    CF: Future<Output = Result<O, HandlerError>>,
{
    let result = runtime
        .execute(context, cancellation, async {
            let upstream = operation.await?;
            Ok(convert(upstream).await)
        })
        .await;

    match result {
        Ok(Ok(output)) => contract
            .success(&output)
            .unwrap_or_else(|_| tool_error(&ToolError::upstream())),
        Ok(Err(error)) => tool_error(error.tool_error()),
        Err(error) => tool_error(&runtime_tool_error(&error)),
    }
}

fn runtime_tool_error(error: &RuntimeError) -> ToolError {
    match error {
        RuntimeError::Upstream(source) => match ToolError::from_anytype(source) {
            AnytypeErrorMapping::Ready(error) => error,
            AnytypeErrorMapping::AmbiguityRequiresCandidates => ToolError::upstream(),
        },
        RuntimeError::Cancelled | RuntimeError::TimedOut | RuntimeError::ShuttingDown => {
            ToolError::upstream()
        }
    }
}

/// Checked upstream pagination metadata used to advance an opaque cursor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UpstreamPagination {
    offset: PageOffset,
    limit: PageLimit,
    has_more: bool,
}

impl UpstreamPagination {
    /// Validates metadata returned by Anytype.
    pub fn new(offset: u32, limit: u32, has_more: bool) -> Result<Self, HandlerError> {
        let limit = u16::try_from(limit)
            .ok()
            .and_then(|value| PageLimit::new(value).ok())
            .ok_or_else(|| HandlerError::new(ToolError::upstream()))?;
        Ok(Self {
            offset: PageOffset::new(offset)
                .map_err(|_| HandlerError::new(ToolError::upstream()))?,
            limit,
            has_more,
        })
    }

    /// Returns whether Anytype reports another page.
    #[must_use]
    pub const fn has_more(self) -> bool {
        self.has_more
    }
}

impl TryFrom<&anytype::paged::PaginationMeta> for UpstreamPagination {
    type Error = HandlerError;

    fn try_from(value: &anytype::paged::PaginationMeta) -> Result<Self, Self::Error> {
        Self::new(value.offset, value.limit, value.has_more)
    }
}

impl TryFrom<&anytype::paged::PaginationResponse> for UpstreamPagination {
    type Error = HandlerError;

    fn try_from(value: &anytype::paged::PaginationResponse) -> Result<Self, Self::Error> {
        let offset = u32::try_from(value.offset)
            .map_err(|_| HandlerError::new(ToolError::bounded_result()))?;
        let limit = u32::try_from(value.limit)
            .map_err(|_| HandlerError::new(ToolError::bounded_result()))?;
        Self::new(offset, limit, value.has_more)
    }
}

/// Validated start state and query binding for one upstream page.
#[derive(Debug, Clone, Copy)]
pub struct PageRequest {
    offset: PageOffset,
    binding: QueryFingerprint,
}

impl PageRequest {
    /// Returns the upstream offset to send with the request.
    #[must_use]
    pub const fn offset(self) -> PageOffset {
        self.offset
    }
}

/// Resolves an optional cursor against an explicit tool, limit, and normalized
/// non-cursor parameter set.
///
/// Top-level `cursor`, `offset`, and duplicate `limit` fields are removed
/// defensively before the fingerprint is built. The explicit `limit` argument
/// is always included, so a handler cannot accidentally omit it from the
/// continuation binding.
pub fn begin_page<P: Serialize>(
    cursors: &CursorStore,
    cursor: Option<&CursorToken>,
    tool: &'static str,
    limit: PageLimit,
    normalized_params: &P,
) -> Result<PageRequest, HandlerError> {
    if !valid_tool_discriminator(tool) {
        return Err(HandlerError::new(ToolError::upstream()));
    }
    let mut params = serde_json::to_value(normalized_params)
        .map_err(|_| HandlerError::new(ToolError::upstream()))?;
    if let serde_json::Value::Object(fields) = &mut params {
        fields.remove("cursor");
        fields.remove("offset");
        fields.remove("limit");
    }
    let binding = QueryFingerprint::from_normalized(&CursorBinding {
        tool,
        limit: limit.get(),
        params: &params,
    })?;
    let offset = cursor.map_or_else(
        || PageOffset::new(0),
        |cursor| cursors.resolve(cursor, binding),
    )?;
    Ok(PageRequest { offset, binding })
}

/// Builds a bounded page and, only when upstream reports more data, issues a
/// cursor advanced by checked upstream `offset + limit` metadata.
///
/// The upstream offset must equal the offset that was requested. Advancement
/// deliberately does not use returned item count: sparse pages must not repeat
/// data when Anytype's page window is larger than the returned vector.
pub fn finish_page<T: JsonSchema>(
    cursors: &CursorStore,
    request: PageRequest,
    upstream: UpstreamPagination,
    items: Vec<T>,
) -> Result<Page<T>, HandlerError> {
    if upstream.offset != request.offset {
        return Err(HandlerError::new(ToolError::upstream()));
    }
    if items.len() > MAX_PAGE_LIMIT as usize {
        return Err(HandlerError::new(ToolError::bounded_result()));
    }
    let next_cursor = if upstream.has_more {
        let next = upstream
            .offset
            .get()
            .checked_add(u32::from(upstream.limit.get()))
            .ok_or_else(|| HandlerError::new(ToolError::bounded_result()))?;
        let next =
            PageOffset::new(next).map_err(|_| HandlerError::new(ToolError::bounded_result()))?;
        Some(cursors.issue(next, request.binding)?)
    } else {
        None
    };
    Page::new(items, next_cursor).map_err(|_| HandlerError::new(ToolError::bounded_result()))
}

#[derive(Debug)]
struct CursorBinding<'a> {
    tool: &'static str,
    limit: u16,
    params: &'a serde_json::Value,
}

impl Serialize for CursorBinding<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut map = serializer.serialize_map(Some(3))?;
        map.serialize_entry("tool", self.tool)?;
        map.serialize_entry("limit", &self.limit)?;
        map.serialize_entry("params", self.params)?;
        map.end()
    }
}

fn valid_tool_discriminator(tool: &str) -> bool {
    !tool.is_empty()
        && tool.len() <= 64
        && tool
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use anytype::prelude::{AnytypeClient, ClientConfig};
    use rmcp::schemars::JsonSchema;
    use serde::{Deserialize, Serializer};
    use serde_json::json;

    use super::*;
    use crate::{
        error::ToolErrorCode,
        pagination::MAX_PAGE_OFFSET,
        protocol::{ToolProfile, workflow_tool},
        runtime::StartupStatus,
    };

    #[derive(Debug, Serialize)]
    struct Params<'a> {
        space: &'a str,
        projection: &'a [&'a str],
        cursor: &'a str,
        offset: u32,
    }

    #[derive(Deserialize, JsonSchema)]
    #[serde(deny_unknown_fields)]
    #[expect(dead_code, reason = "schema-only input")]
    struct Input {
        /// Object identifier.
        object: crate::domain::ObjectId,
    }

    #[derive(Debug, Serialize, JsonSchema)]
    #[serde(deny_unknown_fields)]
    struct Output {
        /// Bounded result.
        value: BoundedResult,
    }

    #[derive(Debug, Serialize, JsonSchema)]
    #[serde(transparent)]
    struct BoundedResult(#[schemars(length(max = 16))] String);

    fn contract<O: JsonSchema + Serialize + 'static>() -> WorkflowTool<O> {
        workflow_tool::<Input, O>("object_get", "Get bounded data.", ToolProfile::Read).unwrap()
    }

    fn runtime() -> RuntimeContext {
        let client = AnytypeClient::with_config(ClientConfig {
            base_url: Some("http://127.0.0.1:1".to_owned()),
            keystore: Some("env".to_owned()),
            keystore_service: Some("handler-support-test".to_owned()),
            app_name: "handler-support-test".to_owned(),
            ..ClientConfig::default()
        })
        .unwrap();
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

    #[test]
    fn cursor_binding_includes_tool_limit_and_normalized_non_cursor_params() {
        let store = CursorStore::new().unwrap();
        let limit = PageLimit::new(20).unwrap();
        let params = Params {
            space: "space-1",
            projection: &["a", "b"],
            cursor: "first",
            offset: 0,
        };
        let request = begin_page(&store, None, "object_search", limit, &params).unwrap();
        let page = finish_page(
            &store,
            request,
            UpstreamPagination::new(0, 20, true).unwrap(),
            vec![true],
        )
        .unwrap();
        let cursor = page.next_cursor().unwrap();

        let changed_reserved = Params {
            cursor: "different",
            offset: 999,
            ..params
        };
        assert_eq!(
            begin_page(
                &store,
                Some(cursor),
                "object_search",
                limit,
                &changed_reserved,
            )
            .unwrap()
            .offset()
            .get(),
            20
        );
        assert!(
            begin_page(
                &store,
                Some(cursor),
                "object_search",
                PageLimit::new(21).unwrap(),
                &changed_reserved,
            )
            .is_err()
        );
        assert!(begin_page(&store, Some(cursor), "space_list", limit, &changed_reserved).is_err());
        let changed_params = Params {
            space: "space-2",
            ..changed_reserved
        };
        assert!(
            begin_page(
                &store,
                Some(cursor),
                "object_search",
                limit,
                &changed_params
            )
            .is_err()
        );
    }

    #[test]
    fn continuation_advances_by_upstream_window_not_sparse_item_count() {
        let store = CursorStore::new().unwrap();
        let request = begin_page(
            &store,
            None,
            "object_search",
            PageLimit::new(20).unwrap(),
            &json!({"space":"space-1"}),
        )
        .unwrap();
        let page = finish_page(
            &store,
            request,
            UpstreamPagination::new(0, 20, true).unwrap(),
            vec!["one sparse item"],
        )
        .unwrap();
        let continued = begin_page(
            &store,
            page.next_cursor(),
            "object_search",
            PageLimit::new(20).unwrap(),
            &json!({"space":"space-1"}),
        )
        .unwrap();
        assert_eq!(continued.offset().get(), 20);
    }

    #[test]
    fn terminal_mismatch_expiry_and_overflow_are_checked() {
        let store = CursorStore::new().unwrap();
        let request = begin_page(
            &store,
            None,
            "space_list",
            PageLimit::new(20).unwrap(),
            &json!({}),
        )
        .unwrap();
        let terminal = finish_page(
            &store,
            request,
            UpstreamPagination::new(0, 20, false).unwrap(),
            vec![true],
        )
        .unwrap();
        assert!(terminal.next_cursor().is_none());
        assert!(
            finish_page(
                &store,
                request,
                UpstreamPagination::new(1, 20, true).unwrap(),
                vec![true],
            )
            .is_err()
        );

        let too_many = finish_page(
            &store,
            request,
            UpstreamPagination::new(0, 20, false).unwrap(),
            vec![true; 101],
        )
        .unwrap_err();
        assert_eq!(too_many.tool_error().code(), ToolErrorCode::BoundedResult);

        let binding = QueryFingerprint::from_normalized(&json!({"overflow":true})).unwrap();
        let overflow = PageRequest {
            offset: PageOffset::new(MAX_PAGE_OFFSET).unwrap(),
            binding,
        };
        let error = finish_page(
            &store,
            overflow,
            UpstreamPagination::new(MAX_PAGE_OFFSET, 1, true).unwrap(),
            vec![true],
        )
        .unwrap_err();
        assert_eq!(error.tool_error().code(), ToolErrorCode::BoundedResult);

        let first = begin_page(
            &store,
            None,
            "space_list",
            PageLimit::new(1).unwrap(),
            &json!({}),
        )
        .unwrap();
        let page = finish_page(
            &store,
            first,
            UpstreamPagination::new(0, 1, true).unwrap(),
            vec![true],
        )
        .unwrap();
        let replacement = CursorStore::new().unwrap();
        assert!(
            begin_page(
                &replacement,
                page.next_cursor(),
                "space_list",
                PageLimit::new(1).unwrap(),
                &json!({}),
            )
            .is_err()
        );
    }

    #[test]
    fn upstream_usize_metadata_conversion_is_checked() {
        let ordinary = anytype::paged::PaginationResponse {
            has_more: true,
            limit: 20,
            offset: 40,
            total: 100,
        };
        let converted = UpstreamPagination::try_from(&ordinary).unwrap();
        assert_eq!(converted.offset.get(), 40);
        assert_eq!(converted.limit.get(), 20);

        if usize::BITS > u32::BITS {
            let oversized = anytype::paged::PaginationResponse {
                offset: usize::MAX,
                ..ordinary
            };
            let error = UpstreamPagination::try_from(&oversized).unwrap_err();
            assert_eq!(error.tool_error().code(), ToolErrorCode::BoundedResult);
        }
    }

    #[test]
    fn entropy_and_runtime_control_failures_map_to_fixed_errors() {
        let entropy = HandlerError::from(CursorStoreError);
        assert_eq!(entropy.tool_error().code(), ToolErrorCode::Upstream);
        for runtime_error in [
            RuntimeError::Cancelled,
            RuntimeError::TimedOut,
            RuntimeError::ShuttingDown,
        ] {
            assert_eq!(
                runtime_tool_error(&runtime_error).code(),
                ToolErrorCode::Upstream
            );
        }
    }

    #[tokio::test]
    async fn execution_encodes_exact_success_and_stable_errors() {
        let runtime = runtime();
        let cancellation = CancellationToken::new();
        let output_contract = contract::<Output>();
        let success = execute_handler(
            &runtime,
            &output_contract,
            OperationContext::new("object_get"),
            &cancellation,
            async { Ok::<_, AnytypeError>("ok") },
            |value| async move {
                Ok(Output {
                    value: BoundedResult(value.to_owned()),
                })
            },
        )
        .await;
        let expected = json!({"value":"ok"});
        assert_eq!(success.structured_content, Some(expected.clone()));
        assert_eq!(
            success.content[0].as_text().unwrap().text,
            expected.to_string()
        );
        assert_eq!(success.is_error, Some(false));

        let source = AnytypeError::ApiError {
            code: 500,
            method: "GET secret".to_owned(),
            url: "http://secret.invalid/token".to_owned(),
            message: "private body".to_owned(),
        };
        let failed = execute_handler(
            &runtime,
            &output_contract,
            OperationContext::new("object_get"),
            &cancellation,
            async { Err::<(), _>(source) },
            |_| async {
                Ok(Output {
                    value: BoundedResult("unused".to_owned()),
                })
            },
        )
        .await;
        assert_eq!(failed.is_error, Some(true));
        let encoded = failed.content[0].as_text().unwrap().text.as_str();
        assert!(!encoded.contains("secret"));
        assert!(!encoded.contains("private"));
        assert_eq!(
            failed.structured_content.as_ref().unwrap()["code"],
            "upstream"
        );
    }

    #[tokio::test]
    async fn incomplete_ambiguity_and_encoding_failure_are_fixed_upstream_errors() {
        let runtime = runtime();
        let cancellation = CancellationToken::new();
        let output_contract = contract::<Output>();
        let ambiguity = AnytypeError::Ambiguous {
            obj_type: "space".to_owned(),
            key: "private name".to_owned(),
            candidates: Vec::new(),
        };
        let result = execute_handler(
            &runtime,
            &output_contract,
            OperationContext::new("object_get"),
            &cancellation,
            async { Err::<(), _>(ambiguity) },
            |_| async {
                Ok(Output {
                    value: BoundedResult("unused".to_owned()),
                })
            },
        )
        .await;
        assert_eq!(
            result.structured_content.as_ref().unwrap()["code"],
            "upstream"
        );

        #[derive(JsonSchema)]
        #[serde(transparent)]
        #[expect(dead_code, reason = "serializer intentionally fails before reading")]
        struct Failing(#[schemars(length(max = 1))] String);
        impl Serialize for Failing {
            fn serialize<S>(&self, _: S) -> Result<S::Ok, S::Error>
            where
                S: Serializer,
            {
                Err(serde::ser::Error::custom("must remain private"))
            }
        }
        #[derive(Serialize, JsonSchema)]
        #[serde(deny_unknown_fields)]
        struct FailingOutput {
            /// Value whose serializer intentionally fails.
            value: Failing,
        }
        let failing_contract = contract::<FailingOutput>();
        let result = execute_handler(
            &runtime,
            &failing_contract,
            OperationContext::new("object_get"),
            &cancellation,
            async { Ok::<_, AnytypeError>(()) },
            |_| async {
                Ok(FailingOutput {
                    value: Failing("x".to_owned()),
                })
            },
        )
        .await;
        assert_eq!(
            result.structured_content.as_ref().unwrap()["code"],
            "upstream"
        );
        assert!(
            !result.content[0]
                .as_text()
                .unwrap()
                .text
                .contains("private")
        );
    }
}
