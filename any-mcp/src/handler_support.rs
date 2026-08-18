// any-mcp - bounded, workflow-oriented MCP server for Anytype
//
// SPDX-FileCopyrightText: 2026 Steve Schoettler
// SPDX-License-Identifier: Apache-2.0

//! Transport-neutral execution, encoding, and checked continuation helpers.

use std::{
    fmt,
    future::Future,
    sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
    },
    time::Instant,
};

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
    runtime::{
        ControlledFailureKind, ControlledOperationError, OperationContext,
        OperationFailureDiagnostic, RuntimeContext,
    },
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

/// A handler's secret-safe preflight or upstream operation failure.
///
/// This wrapper lets one controlled operation combine resolver/API failures
/// with checks that must happen after resolution, such as binding a cursor to
/// resolved identifiers. Debug and display output never reveal the source.
pub struct HandlerOperationError(HandlerOperationErrorKind);

enum HandlerOperationErrorKind {
    Upstream(AnytypeError),
    Handler(HandlerError),
}

impl From<AnytypeError> for HandlerOperationError {
    fn from(error: AnytypeError) -> Self {
        Self(HandlerOperationErrorKind::Upstream(error))
    }
}

impl From<HandlerError> for HandlerOperationError {
    fn from(error: HandlerError) -> Self {
        Self(HandlerOperationErrorKind::Handler(error))
    }
}

impl fmt::Debug for HandlerOperationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("HandlerOperationError(<redacted>)")
    }
}

impl fmt::Display for HandlerOperationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("MCP workflow operation failed")
    }
}

impl std::error::Error for HandlerOperationError {}

/// Defense-in-depth policy checked by mutation handlers before resolution or
/// upstream I/O.
///
/// Catalog filtering remains the primary read-only control. Passing this
/// policy into each mutation handler ensures that a stale, already-discovered
/// direct call still fails closed at the handler seam.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MutationAccess {
    /// Mutating workflows may proceed.
    Allowed,
    /// Mutating workflows must be rejected before any upstream work.
    ReadOnly,
}

/// One-way stage of a controlled mutation operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MutationStage {
    /// No write future has been polled yet.
    PreDispatch,
    /// A write may have reached Anytype.
    Dispatched,
}

/// Cloneable one-way marker for whether a write may have been dispatched.
///
/// Create a fresh marker for each mutation invocation. Move a clone into the
/// controlled operation and call [`mark_dispatched`](Self::mark_dispatched)
/// immediately before the first poll of the first write future. Once marked,
/// all clones remain `Dispatched`; reusing a marker therefore fails safe by
/// treating later controlled failures as indeterminate.
#[derive(Debug, Clone, Default)]
pub struct MutationProgress {
    stage: Arc<AtomicU8>,
}

impl MutationProgress {
    const PRE_DISPATCH: u8 = 0;
    const DISPATCHED: u8 = 1;

    /// Creates a fresh pre-dispatch marker.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Marks that the first write future is about to be polled.
    ///
    /// This transition is idempotent and cannot be reset.
    pub(crate) fn mark_dispatched(
        &self,
        runtime: &RuntimeContext,
    ) -> Result<(), HandlerOperationError> {
        crate::runtime::mark_invocation_dispatched(runtime)
            .map_err(|_| HandlerError::new(ToolError::upstream()))?;
        self.stage.store(Self::DISPATCHED, Ordering::SeqCst);
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn mark_dispatched_for_test(&self) {
        self.stage.store(Self::DISPATCHED, Ordering::SeqCst);
    }

    /// Returns the current one-way mutation stage.
    #[must_use]
    pub fn stage(&self) -> MutationStage {
        match self.stage.load(Ordering::SeqCst) {
            Self::PRE_DISPATCH => MutationStage::PreDispatch,
            _ => MutationStage::Dispatched,
        }
    }
}

/// Rejects a mutation when the current handler invocation is read-only.
///
/// Call this before resolving names or constructing an upstream future.
pub fn require_mutation_access(access: MutationAccess) -> Result<(), HandlerError> {
    match access {
        MutationAccess::Allowed => Ok(()),
        MutationAccess::ReadOnly => Err(HandlerError::new(ToolError::read_only())),
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
    execute_prepared_handler(
        runtime,
        contract,
        context,
        cancellation,
        async { operation.await.map_err(HandlerOperationError::from) },
        convert,
    )
    .await
}

/// Executes one operation that can combine upstream and post-resolution
/// handler preflight failures, followed by bounded conversion and encoding.
///
/// Use this variant when cursor binding or another validation step must occur
/// after an asynchronous resolver but before the primary upstream request.
pub async fn execute_prepared_handler<U, O, F, C, CF, E>(
    runtime: &RuntimeContext,
    contract: &WorkflowTool<O>,
    context: OperationContext,
    cancellation: &CancellationToken,
    operation: F,
    convert: C,
) -> CallToolResult
where
    O: Serialize,
    F: Future<Output = Result<U, E>>,
    E: Into<HandlerOperationError>,
    C: FnOnce(U) -> CF,
    CF: Future<Output = Result<O, HandlerError>>,
{
    execute_prepared_with_policy(
        runtime,
        contract,
        context,
        cancellation,
        operation,
        convert,
        ControlledFailurePolicy::Ordinary,
    )
    .await
}

/// Executes one prepared operation under a caller-supplied absolute deadline.
pub async fn execute_prepared_handler_until<U, O, F, C, CF, E>(
    runtime: &RuntimeContext,
    deadline: Instant,
    contract: &WorkflowTool<O>,
    context: OperationContext,
    cancellation: &CancellationToken,
    operation: F,
    convert: C,
) -> CallToolResult
where
    O: Serialize,
    F: Future<Output = Result<U, E>>,
    E: Into<HandlerOperationError>,
    C: FnOnce(U) -> CF,
    CF: Future<Output = Result<O, HandlerError>>,
{
    let control_policy = ControlledFailurePolicy::Ordinary;
    let result = runtime
        .execute_classified_with_control_until(
            deadline,
            context,
            cancellation,
            async {
                let upstream = operation
                    .await
                    .map_err(|error| HandlerExecutionError::Operation(error.into()))?;
                let output = convert(upstream)
                    .await
                    .map_err(HandlerExecutionError::Conversion)?;
                contract
                    .success(&output)
                    .map_err(|_| HandlerExecutionError::Encoding)
            },
            HandlerExecutionError::diagnostic,
            |failure| control_policy.diagnostic(failure),
        )
        .await;

    match result {
        Ok(output) => output,
        Err(error) => tool_error(&execution_tool_error(error, control_policy)),
    }
}

/// Executes a mutation with stage-aware cancellation, timeout, and shutdown
/// handling.
///
/// Controlled failures before `progress` is marked retain the ordinary fixed
/// `upstream` result. At or after dispatch they return the fixed `conflict`
/// result from [`ToolError::mutation_indeterminate`], directing the caller to
/// reread before any retry. Errors returned normally by `operation` are not
/// guessed from stage: the handler must classify them explicitly as an
/// [`AnytypeError`] or [`HandlerError`] through [`HandlerOperationError`].
/// Existing execution helpers do not opt into this policy and are unchanged.
///
/// A handler should use a fresh marker and place
/// `progress.mark_dispatched(runtime)` directly before awaiting its write builder.
pub async fn execute_mutation_handler<U, O, F, C, CF, E>(
    runtime: &RuntimeContext,
    contract: &WorkflowTool<O>,
    context: OperationContext,
    cancellation: &CancellationToken,
    progress: &MutationProgress,
    operation: F,
    convert: C,
) -> CallToolResult
where
    O: Serialize,
    F: Future<Output = Result<U, E>>,
    E: Into<HandlerOperationError>,
    C: FnOnce(U) -> CF,
    CF: Future<Output = Result<O, HandlerError>>,
{
    execute_prepared_with_policy(
        runtime,
        contract,
        context,
        cancellation,
        operation,
        convert,
        ControlledFailurePolicy::Mutation(progress),
    )
    .await
}

/// Executes a mutation under one caller-supplied absolute request deadline.
///
/// Detached create supervisors use this variant so resolution, admission,
/// dispatch, verification, and waiting share the leader's original deadline.
#[allow(clippy::too_many_arguments)]
pub async fn execute_mutation_handler_until<U, O, F, C, CF, E>(
    runtime: &RuntimeContext,
    deadline: Instant,
    contract: &WorkflowTool<O>,
    context: OperationContext,
    cancellation: &CancellationToken,
    progress: &MutationProgress,
    operation: F,
    convert: C,
) -> CallToolResult
where
    O: Serialize,
    F: Future<Output = Result<U, E>>,
    E: Into<HandlerOperationError>,
    C: FnOnce(U) -> CF,
    CF: Future<Output = Result<O, HandlerError>>,
{
    let control_policy = ControlledFailurePolicy::Mutation(progress);
    let result = runtime
        .execute_classified_with_control_until(
            deadline,
            context,
            cancellation,
            async {
                let upstream = operation
                    .await
                    .map_err(|error| HandlerExecutionError::Operation(error.into()))?;
                let output = convert(upstream)
                    .await
                    .map_err(HandlerExecutionError::Conversion)?;
                contract
                    .success(&output)
                    .map_err(|_| HandlerExecutionError::Encoding)
            },
            HandlerExecutionError::diagnostic,
            |failure| control_policy.diagnostic(failure),
        )
        .await;

    match result {
        Ok(output) => output,
        Err(error) => tool_error(&execution_tool_error(error, control_policy)),
    }
}

async fn execute_prepared_with_policy<U, O, F, C, CF, E>(
    runtime: &RuntimeContext,
    contract: &WorkflowTool<O>,
    context: OperationContext,
    cancellation: &CancellationToken,
    operation: F,
    convert: C,
    control_policy: ControlledFailurePolicy<'_>,
) -> CallToolResult
where
    O: Serialize,
    F: Future<Output = Result<U, E>>,
    E: Into<HandlerOperationError>,
    C: FnOnce(U) -> CF,
    CF: Future<Output = Result<O, HandlerError>>,
{
    let result = runtime
        .execute_classified_with_control(
            context,
            cancellation,
            async {
                let upstream = operation
                    .await
                    .map_err(|error| HandlerExecutionError::Operation(error.into()))?;
                let output = convert(upstream)
                    .await
                    .map_err(HandlerExecutionError::Conversion)?;
                contract
                    .success(&output)
                    .map_err(|_| HandlerExecutionError::Encoding)
            },
            HandlerExecutionError::diagnostic,
            |failure| control_policy.diagnostic(failure),
        )
        .await;

    match result {
        Ok(output) => output,
        Err(error) => tool_error(&execution_tool_error(error, control_policy)),
    }
}

#[derive(Clone, Copy)]
enum ControlledFailurePolicy<'a> {
    Ordinary,
    Mutation(&'a MutationProgress),
}

impl ControlledFailurePolicy<'_> {
    fn mutation_indeterminate(self) -> bool {
        matches!(self, Self::Mutation(progress) if progress.stage() == MutationStage::Dispatched)
    }

    fn diagnostic(self, failure: ControlledFailureKind) -> OperationFailureDiagnostic {
        if self.mutation_indeterminate() {
            return OperationFailureDiagnostic::classified(
                "mutation_indeterminate",
                "mutation_indeterminate",
            );
        }
        match failure {
            ControlledFailureKind::Cancelled => {
                OperationFailureDiagnostic::classified("cancelled", "not_observed")
            }
            ControlledFailureKind::TimedOut => {
                OperationFailureDiagnostic::classified("timeout", "not_observed")
            }
            ControlledFailureKind::ShuttingDown => {
                OperationFailureDiagnostic::classified("shutdown", "not_observed")
            }
        }
    }
}

fn execution_tool_error(
    error: ControlledOperationError<HandlerExecutionError>,
    control_policy: ControlledFailurePolicy<'_>,
) -> ToolError {
    match error {
        ControlledOperationError::Operation(error) => error.into_tool_error(),
        ControlledOperationError::Cancelled
        | ControlledOperationError::TimedOut
        | ControlledOperationError::ShuttingDown => {
            if control_policy.mutation_indeterminate() {
                ToolError::mutation_indeterminate()
            } else {
                ToolError::upstream()
            }
        }
    }
}

enum HandlerExecutionError {
    Operation(HandlerOperationError),
    Conversion(HandlerError),
    Encoding,
}

impl HandlerExecutionError {
    fn diagnostic(&self) -> OperationFailureDiagnostic {
        match self {
            Self::Operation(HandlerOperationError(HandlerOperationErrorKind::Upstream(error))) => {
                OperationFailureDiagnostic::from_anytype(error)
            }
            Self::Operation(HandlerOperationError(HandlerOperationErrorKind::Handler(_))) => {
                OperationFailureDiagnostic::classified("preflight_error", "handler_preflight")
            }
            Self::Conversion(_) => {
                OperationFailureDiagnostic::classified("conversion_error", "handler_conversion")
            }
            Self::Encoding => {
                OperationFailureDiagnostic::classified("encoding_error", "result_encoding")
            }
        }
    }

    fn into_tool_error(self) -> ToolError {
        match self {
            Self::Operation(HandlerOperationError(HandlerOperationErrorKind::Upstream(source))) => {
                match ToolError::from_anytype(&source) {
                    AnytypeErrorMapping::Ready(error) => error,
                    AnytypeErrorMapping::AmbiguityRequiresCandidates => ToolError::upstream(),
                }
            }
            Self::Operation(HandlerOperationError(HandlerOperationErrorKind::Handler(error))) => {
                error.0
            }
            Self::Conversion(error) => error.0,
            Self::Encoding => ToolError::upstream(),
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
    ///
    /// Offsets beyond the supported finite continuation range map to
    /// `bounded_result`. A zero or over-100 upstream page limit is malformed
    /// metadata and maps to the fixed `upstream` error instead.
    pub fn new(offset: u32, limit: u32, has_more: bool) -> Result<Self, HandlerError> {
        let limit = u16::try_from(limit)
            .ok()
            .and_then(|value| PageLimit::new(value).ok())
            .ok_or_else(|| HandlerError::new(ToolError::upstream()))?;
        Ok(Self {
            offset: PageOffset::new(offset)
                .map_err(|_| HandlerError::new(ToolError::bounded_result()))?,
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
        let limit =
            u32::try_from(value.limit).map_err(|_| HandlerError::new(ToolError::upstream()))?;
        Self::new(offset, limit, value.has_more)
    }
}

/// Validated start state and query binding for one upstream page.
#[derive(Debug, Clone, Copy)]
pub struct PageRequest {
    offset: PageOffset,
    limit: PageLimit,
    binding: QueryFingerprint,
}

impl PageRequest {
    /// Returns the upstream offset to send with the request.
    #[must_use]
    pub const fn offset(self) -> PageOffset {
        self.offset
    }

    /// Returns the exact requested upstream page limit.
    #[must_use]
    pub const fn limit(self) -> PageLimit {
        self.limit
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
    let binding = page_query_fingerprint(tool, limit, normalized_params)?;
    let offset = cursor.map_or_else(
        || PageOffset::new(0),
        |cursor| cursors.resolve(cursor, binding),
    )?;
    Ok(PageRequest {
        offset,
        limit,
        binding,
    })
}

/// Applies the exact page-binding size limit to an alternate presentation.
///
/// Handlers that use a smaller semantic cursor representation call this on
/// the original bounded request first, preserving the existing raw normalized
/// query ceiling without using that presentation as the cursor identity.
pub(crate) fn validate_page_binding_size<P: Serialize>(
    tool: &'static str,
    limit: PageLimit,
    normalized_params: &P,
) -> Result<(), HandlerError> {
    page_query_fingerprint(tool, limit, normalized_params).map(drop)
}

pub(crate) fn page_query_fingerprint<P: Serialize>(
    tool: &'static str,
    limit: PageLimit,
    normalized_params: &P,
) -> Result<QueryFingerprint, HandlerError> {
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
    Ok(QueryFingerprint::from_normalized(&CursorBinding {
        tool,
        limit: limit.get(),
        params: &params,
    })?)
}

/// Builds a bounded page and, only when upstream reports more data, issues a
/// cursor advanced by checked upstream `offset + limit` metadata.
///
/// The upstream offset and limit must equal the values that were requested,
/// and the returned item count must not exceed that requested limit. All
/// integrity checks happen before cursor issuance. Advancement deliberately
/// does not use returned item count: sparse pages must not repeat data when
/// Anytype's page window is larger than the returned vector.
pub fn finish_page<T: JsonSchema>(
    cursors: &CursorStore,
    request: PageRequest,
    upstream: UpstreamPagination,
    items: Vec<T>,
) -> Result<Page<T>, HandlerError> {
    if upstream.offset != request.offset {
        return Err(HandlerError::new(ToolError::upstream()));
    }
    if upstream.limit != request.limit {
        return Err(HandlerError::new(ToolError::upstream()));
    }
    if items.len() > usize::from(request.limit.get()) || items.len() > MAX_PAGE_LIMIT as usize {
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

/// Builds a page whose continuation was derived by a bounded policy-aware
/// upstream scan.
///
/// `next_offset` identifies the first permitted upstream row not returned in
/// `items`. The caller has already hidden disallowed rows and ignored upstream
/// totals, so cursor presence reveals only another permitted result.
pub(crate) fn finish_filtered_page<T: JsonSchema>(
    cursors: &CursorStore,
    request: PageRequest,
    items: Vec<T>,
    next_offset: Option<u32>,
) -> Result<Page<T>, HandlerError> {
    if items.len() > usize::from(request.limit.get()) || items.len() > MAX_PAGE_LIMIT as usize {
        return Err(HandlerError::new(ToolError::bounded_result()));
    }
    let next_cursor = next_offset
        .map(|offset| {
            if offset <= request.offset.get() {
                return Err(HandlerError::new(ToolError::upstream()));
            }
            let offset = PageOffset::new(offset)
                .map_err(|_| HandlerError::new(ToolError::bounded_result()))?;
            cursors
                .issue(offset, request.binding)
                .map_err(HandlerError::from)
        })
        .transpose()?;
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
    use tracing::instrument::WithSubscriber;

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
        runtime_with_timeout(Duration::from_secs(1))
    }

    fn runtime_with_timeout(timeout: Duration) -> RuntimeContext {
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
            timeout,
            StartupStatus {
                http_available: true,
                grpc_available: false,
            },
        )
    }

    fn run_trace_test<F>(future: F) -> F::Output
    where
        F: Future,
    {
        let _guard = crate::logging::test_support::trace_test_guard();
        tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("handler trace test runtime")
            .block_on(future)
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
    fn page_integrity_failures_precede_cursor_issuance() {
        let store = CursorStore::new().unwrap();
        let request = begin_page(
            &store,
            None,
            "space_list",
            PageLimit::new(1).unwrap(),
            &json!({}),
        )
        .unwrap();
        let mismatched_limit = finish_page(
            &store,
            request,
            UpstreamPagination::new(0, 100, true).unwrap(),
            vec![true],
        )
        .unwrap_err();
        assert_eq!(
            mismatched_limit.tool_error().code(),
            ToolErrorCode::Upstream
        );
        assert_eq!(store.entry_count(), 0);

        let too_many_for_request = finish_page(
            &store,
            request,
            UpstreamPagination::new(0, 1, true).unwrap(),
            vec![true, false],
        )
        .unwrap_err();
        assert_eq!(
            too_many_for_request.tool_error().code(),
            ToolErrorCode::BoundedResult
        );
        assert_eq!(store.entry_count(), 0);

        let page = finish_page(
            &store,
            request,
            UpstreamPagination::new(0, 1, true).unwrap(),
            vec![true],
        )
        .unwrap();
        assert!(page.next_cursor().is_some());
        assert_eq!(store.entry_count(), 1);
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
            limit: PageLimit::new(1).unwrap(),
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
                ..ordinary.clone()
            };
            let error = UpstreamPagination::try_from(&oversized).unwrap_err();
            assert_eq!(error.tool_error().code(), ToolErrorCode::BoundedResult);

            let malformed_limit = anytype::paged::PaginationResponse {
                limit: usize::MAX,
                ..ordinary
            };
            let error = UpstreamPagination::try_from(&malformed_limit).unwrap_err();
            assert_eq!(error.tool_error().code(), ToolErrorCode::Upstream);
        }
    }

    #[test]
    fn upstream_metadata_error_categories_are_stable() {
        let too_far = UpstreamPagination::new(MAX_PAGE_OFFSET + 1, 1, true).unwrap_err();
        assert_eq!(too_far.tool_error().code(), ToolErrorCode::BoundedResult);

        for invalid_limit in [0, u32::from(MAX_PAGE_LIMIT) + 1] {
            let malformed = UpstreamPagination::new(0, invalid_limit, true).unwrap_err();
            assert_eq!(malformed.tool_error().code(), ToolErrorCode::Upstream);
        }
    }

    #[test]
    fn entropy_and_runtime_control_failures_map_to_fixed_errors() {
        let entropy = HandlerError::from(CursorStoreError);
        assert_eq!(entropy.tool_error().code(), ToolErrorCode::Upstream);
        for controlled_error in [
            ControlledOperationError::Cancelled,
            ControlledOperationError::TimedOut,
            ControlledOperationError::ShuttingDown,
        ] {
            assert_eq!(
                execution_tool_error(controlled_error, ControlledFailurePolicy::Ordinary).code(),
                ToolErrorCode::Upstream
            );
        }
    }

    #[tokio::test]
    async fn execution_encodes_exact_success_and_stable_errors() {
        let runtime_context = runtime();
        let cancellation = CancellationToken::new();
        let output_contract = contract::<Output>();
        let success = execute_handler(
            &runtime_context,
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
            &runtime_context,
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

    fn assert_error(result: &CallToolResult, code: &str, message: &str) {
        assert_eq!(result.is_error, Some(true));
        assert_eq!(
            result.structured_content,
            Some(json!({"code": code, "message": message}))
        );
        assert_eq!(
            result.content[0].as_text().unwrap().text,
            json!({"code": code, "message": message}).to_string()
        );
    }

    const UPSTREAM_MESSAGE: &str = "Anytype could not complete the request. Retry later or inspect redacted server diagnostics.";
    const INDETERMINATE_MESSAGE: &str = "The mutation may have applied. Reread the object before retrying to avoid applying it twice.";

    #[tokio::test]
    async fn mutation_cancellation_is_stage_aware_at_the_dispatch_marker() {
        let output_contract = contract::<Output>();

        let pre_runtime = runtime();
        let cancellation = CancellationToken::new();
        cancellation.cancel();
        let pre_dispatch = MutationProgress::new();
        let operation_marker = pre_dispatch.clone();
        let result = execute_mutation_handler(
            &pre_runtime,
            &output_contract,
            OperationContext::new("mutation_cancel_pre"),
            &cancellation,
            &pre_dispatch,
            async move {
                operation_marker.mark_dispatched_for_test();
                std::future::pending::<Result<(), HandlerOperationError>>().await
            },
            |_| async {
                Ok(Output {
                    value: BoundedResult("unused".to_owned()),
                })
            },
        )
        .await;
        assert_eq!(pre_dispatch.stage(), MutationStage::PreDispatch);
        assert_error(&result, "upstream", UPSTREAM_MESSAGE);

        let post_runtime = runtime();
        let cancellation = CancellationToken::new();
        let progress = MutationProgress::new();
        let operation_progress = progress.clone();
        let operation_cancellation = cancellation.clone();
        let result = execute_mutation_handler(
            &post_runtime,
            &output_contract,
            OperationContext::new("mutation_cancel_post"),
            &cancellation,
            &progress,
            async move {
                operation_progress.mark_dispatched_for_test();
                operation_cancellation.cancel();
                std::future::pending::<Result<(), HandlerOperationError>>().await
            },
            |_| async {
                Ok(Output {
                    value: BoundedResult("unused".to_owned()),
                })
            },
        )
        .await;
        assert_eq!(progress.stage(), MutationStage::Dispatched);
        assert_error(&result, "conflict", INDETERMINATE_MESSAGE);
    }

    #[tokio::test]
    async fn mutation_timeout_is_stage_aware() {
        let output_contract = contract::<Output>();

        let runtime = runtime_with_timeout(Duration::from_millis(5));
        let progress = MutationProgress::new();
        let result = execute_mutation_handler(
            &runtime,
            &output_contract,
            OperationContext::new("mutation_timeout_pre"),
            &CancellationToken::new(),
            &progress,
            std::future::pending::<Result<(), HandlerOperationError>>(),
            |_| async {
                Ok(Output {
                    value: BoundedResult("unused".to_owned()),
                })
            },
        )
        .await;
        assert_error(&result, "upstream", UPSTREAM_MESSAGE);

        let runtime = runtime_with_timeout(Duration::from_millis(5));
        let progress = MutationProgress::new();
        let operation_progress = progress.clone();
        let result = execute_mutation_handler(
            &runtime,
            &output_contract,
            OperationContext::new("mutation_timeout_post"),
            &CancellationToken::new(),
            &progress,
            async move {
                operation_progress.mark_dispatched_for_test();
                std::future::pending::<Result<(), HandlerOperationError>>().await
            },
            |_| async {
                Ok(Output {
                    value: BoundedResult("unused".to_owned()),
                })
            },
        )
        .await;
        assert_error(&result, "conflict", INDETERMINATE_MESSAGE);
    }

    #[tokio::test]
    async fn mutation_shutdown_is_stage_aware() {
        let output_contract = contract::<Output>();

        let pre_runtime = runtime();
        pre_runtime.begin_shutdown();
        let progress = MutationProgress::new();
        let result = execute_mutation_handler(
            &pre_runtime,
            &output_contract,
            OperationContext::new("mutation_shutdown_pre"),
            &CancellationToken::new(),
            &progress,
            std::future::pending::<Result<(), HandlerOperationError>>(),
            |_| async {
                Ok(Output {
                    value: BoundedResult("unused".to_owned()),
                })
            },
        )
        .await;
        assert_error(&result, "upstream", UPSTREAM_MESSAGE);

        let post_runtime = runtime();
        let progress = MutationProgress::new();
        let operation_progress = progress.clone();
        let operation_runtime = post_runtime.clone();
        let result = execute_mutation_handler(
            &post_runtime,
            &output_contract,
            OperationContext::new("mutation_shutdown_post"),
            &CancellationToken::new(),
            &progress,
            async move {
                operation_progress.mark_dispatched_for_test();
                operation_runtime.begin_shutdown();
                std::future::pending::<Result<(), HandlerOperationError>>().await
            },
            |_| async {
                Ok(Output {
                    value: BoundedResult("unused".to_owned()),
                })
            },
        )
        .await;
        assert_error(&result, "conflict", INDETERMINATE_MESSAGE);
    }

    #[tokio::test]
    async fn ordinary_operation_errors_remain_handler_classified_after_dispatch() {
        let runtime = runtime();
        let cancellation = CancellationToken::new();
        let output_contract = contract::<Output>();
        let progress = MutationProgress::new();
        let operation_progress = progress.clone();
        let result = execute_mutation_handler(
            &runtime,
            &output_contract,
            OperationContext::new("mutation_operation_error"),
            &cancellation,
            &progress,
            async move {
                operation_progress.mark_dispatched_for_test();
                Err::<(), _>(HandlerOperationError::from(HandlerError::new(
                    ToolError::validation(),
                )))
            },
            |_| async {
                Ok(Output {
                    value: BoundedResult("unused".to_owned()),
                })
            },
        )
        .await;
        assert_error(
            &result,
            "validation",
            "Input validation failed. Correct the supplied fields and retry.",
        );
    }

    #[tokio::test]
    async fn mutation_progress_is_clone_shared_sticky_and_reuse_fails_safe() {
        let progress = MutationProgress::new();
        let clone = progress.clone();
        assert_eq!(progress.stage(), MutationStage::PreDispatch);
        clone.mark_dispatched_for_test();
        clone.mark_dispatched_for_test();
        assert_eq!(progress.stage(), MutationStage::Dispatched);

        let cancellation = CancellationToken::new();
        cancellation.cancel();
        let result = execute_mutation_handler(
            &runtime(),
            &contract::<Output>(),
            OperationContext::new("mutation_reused_marker"),
            &cancellation,
            &progress,
            std::future::pending::<Result<(), HandlerOperationError>>(),
            |_| async {
                Ok(Output {
                    value: BoundedResult("unused".to_owned()),
                })
            },
        )
        .await;
        assert_error(&result, "conflict", INDETERMINATE_MESSAGE);
    }

    #[test]
    fn mutation_indeterminate_diagnostic_is_static_and_has_no_false_success() {
        run_trace_test(async {
            let runtime = runtime();
            let cancellation = CancellationToken::new();
            let progress = MutationProgress::new();
            let operation_progress = progress.clone();
            let operation_cancellation = cancellation.clone();
            let secret = "SECRET_MUTATION_VALUE";
            let (dispatch, captured) =
                crate::logging::test_support::capture("any_mcp::operation=trace");
            let result = execute_mutation_handler(
                &runtime,
                &contract::<Output>(),
                OperationContext::new("mutation_diagnostic"),
                &cancellation,
                &progress,
                async move {
                    let _payload = secret;
                    operation_progress.mark_dispatched_for_test();
                    operation_cancellation.cancel();
                    std::future::pending::<Result<(), HandlerOperationError>>().await
                },
                |_| async {
                    Ok(Output {
                        value: BoundedResult("unused".to_owned()),
                    })
                },
            )
            .with_subscriber(dispatch)
            .await;
            assert_error(&result, "conflict", INDETERMINATE_MESSAGE);
            let output = captured.contents();
            assert_eq!(output.matches("Anytype operation completed").count(), 1);
            assert!(output.contains("outcome=\"mutation_indeterminate\""));
            assert!(output.contains("upstream_status=\"mutation_indeterminate\""));
            assert!(!output.contains("outcome=\"success\""));
            assert!(!output.contains(secret));
        });
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

    #[test]
    fn conversion_and_encoding_failures_emit_one_safe_failure_diagnostic() {
        run_trace_test(async {
            let runtime = runtime();
            let cancellation = CancellationToken::new();
            let output_contract = contract::<Output>();
            let secret = "SECRET_CONVERSION_INPUT";
            let (dispatch, captured) =
                crate::logging::test_support::capture("any_mcp::operation=trace");
            let conversion = execute_handler(
                &runtime,
                &output_contract,
                OperationContext::new("conversion_probe"),
                &cancellation,
                async { Ok::<_, AnytypeError>(secret) },
                |_| async { Err::<Output, _>(HandlerError::new(ToolError::bounded_result())) },
            )
            .with_subscriber(dispatch)
            .await;
            assert_eq!(conversion.is_error, Some(true));
            assert_eq!(
                conversion.structured_content.as_ref().unwrap()["code"],
                "bounded_result"
            );
            let output = captured.contents();
            assert_eq!(output.matches("Anytype operation completed").count(), 1);
            assert!(output.contains("operation=\"conversion_probe\""));
            assert!(output.contains("outcome=\"conversion_error\""));
            assert!(output.contains("upstream_status=\"handler_conversion\""));
            assert!(!output.contains("outcome=\"success\""));
            assert!(!output.contains(secret));

            #[derive(JsonSchema)]
            #[serde(transparent)]
            #[expect(dead_code, reason = "serializer intentionally fails before reading")]
            struct SecretFailing(#[schemars(length(max = 1))] String);
            impl Serialize for SecretFailing {
                fn serialize<S>(&self, _: S) -> Result<S::Ok, S::Error>
                where
                    S: Serializer,
                {
                    Err(serde::ser::Error::custom("SECRET_ENCODER_DETAIL"))
                }
            }
            #[derive(Serialize, JsonSchema)]
            #[serde(deny_unknown_fields)]
            struct FailingOutput {
                /// Value whose serializer intentionally fails.
                value: SecretFailing,
            }

            let encoding_contract = contract::<FailingOutput>();
            let (dispatch, captured) =
                crate::logging::test_support::capture("any_mcp::operation=trace");
            let encoding = execute_handler(
                &runtime,
                &encoding_contract,
                OperationContext::new("encoding_probe"),
                &cancellation,
                async { Ok::<_, AnytypeError>(()) },
                |_| async {
                    Ok(FailingOutput {
                        value: SecretFailing("x".to_owned()),
                    })
                },
            )
            .with_subscriber(dispatch)
            .await;
            assert_eq!(encoding.is_error, Some(true));
            assert_eq!(
                encoding.structured_content.as_ref().unwrap()["code"],
                "upstream"
            );
            let output = captured.contents();
            assert_eq!(output.matches("Anytype operation completed").count(), 1);
            assert!(output.contains("operation=\"encoding_probe\""));
            assert!(output.contains("outcome=\"encoding_error\""));
            assert!(output.contains("upstream_status=\"result_encoding\""));
            assert!(!output.contains("outcome=\"success\""));
            assert!(!output.contains("SECRET_ENCODER_DETAIL"));
        });
    }
}
