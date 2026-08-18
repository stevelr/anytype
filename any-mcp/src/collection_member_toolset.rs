// any-mcp - bounded, workflow-oriented MCP server for Anytype
//
// SPDX-FileCopyrightText: 2026 Steve Schoettler
// SPDX-License-Identifier: Apache-2.0

//! Canonical, presentation-independent collection membership workflows.
//!
//! This module provides the complete default-off `views-write` production
//! registry. It enumerates and changes
//! direct manual-collection membership through `anytype-api`; it never reads
//! or mutates a saved view, filter, layout, sort, or Kanban column.

use std::borrow::Cow;

#[cfg(test)]
use anytype::error::AnytypeError;
use anytype::{
    prelude::{AnytypeClient, CollectionMemberAddOutcome, VerifyConfig, verify_semantic},
    views::{
        CollectionMembershipContinuation,
        CollectionMembershipObservation as ApiCollectionMembershipObservation,
        CollectionMembershipPage as ApiCollectionMembershipPage, CollectionMembershipState,
    },
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
        HandlerError, HandlerOperationError, MutationAccess, MutationProgress,
        execute_mutation_handler, execute_prepared_handler, page_query_fingerprint,
        require_mutation_access, validate_page_binding_size,
    },
    optional_toolsets::{
        OptionalRegistryFuture, OptionalRegistryTool, OptionalToolsetMetadata,
        OptionalToolsetRegistry,
    },
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
/// Exact tool name for ensuring that one object is a direct collection member.
pub const COLLECTION_MEMBER_ADD: &str = "collection_member_add";
/// Exact tool name for ensuring that one object is not a direct collection member.
pub const COLLECTION_MEMBER_REMOVE: &str = "collection_member_remove";
/// Reviewed maximum number of collection members returned by one call.
pub const MAX_COLLECTION_MEMBER_PAGE_LIMIT: u16 = 61;
/// Reviewed maximum logical HTTP operations for one list page.
pub const COLLECTION_MEMBER_LIST_HTTP_LOGICAL_CEILING: usize = 12;
/// Reviewed maximum physical HTTP attempts for one list page.
pub const COLLECTION_MEMBER_LIST_HTTP_PHYSICAL_CEILING: usize = 72;
/// Reviewed maximum gRPC calls including cleanup fallback for one list page.
pub const COLLECTION_MEMBER_LIST_GRPC_CEILING: usize = 3;
/// Reviewed maximum logical HTTP operations for one collection-member add.
pub const COLLECTION_MEMBER_ADD_HTTP_LOGICAL_CEILING: usize = 34;
/// Reviewed maximum physical HTTP attempts for one collection-member add.
pub const COLLECTION_MEMBER_ADD_HTTP_PHYSICAL_CEILING: usize = 199;
/// Reviewed maximum gRPC calls for one collection-member add.
pub const COLLECTION_MEMBER_ADD_GRPC_CEILING: usize = 99;
/// Reviewed maximum logical HTTP operations for one collection-member removal.
pub const COLLECTION_MEMBER_REMOVE_HTTP_LOGICAL_CEILING: usize = 34;
/// Reviewed maximum physical HTTP attempts for one collection-member removal.
pub const COLLECTION_MEMBER_REMOVE_HTTP_PHYSICAL_CEILING: usize = 204;
/// Reviewed maximum gRPC calls for one collection-member removal.
pub const COLLECTION_MEMBER_REMOVE_GRPC_CEILING: usize = 96;
/// Reviewed incremental catalog ceiling for the complete production registry.
pub const VIEWS_WRITE_CATALOG_TOKEN_CEILING: usize = 3_000;

/// Exact production selector for collection membership workflows.
pub const VIEWS_WRITE_TOOLSET_NAME: &str = "views-write";

const SCRIPTED_SCENARIOS: &[&str] = &[
    "collection_member_acceptance_direct",
    "collection_member_acceptance_stdio",
];
const HEADLESS_SCENARIOS: &[&str] = &["collection_member_acceptance_headless"];

#[derive(Debug)]
struct ViewsWriteRegistry;

static VIEWS_WRITE_REGISTRY_IMPL: ViewsWriteRegistry = ViewsWriteRegistry;

/// Complete production descriptor for the `views-write` registry.
pub static VIEWS_WRITE_REGISTRY: &dyn OptionalToolsetRegistry = &VIEWS_WRITE_REGISTRY_IMPL;

/// Returns the complete production `views-write` registry.
#[must_use]
pub fn views_write_registry() -> &'static dyn OptionalToolsetRegistry {
    VIEWS_WRITE_REGISTRY
}

impl OptionalToolsetRegistry for ViewsWriteRegistry {
    fn metadata(&self) -> OptionalToolsetMetadata {
        OptionalToolsetMetadata::new(VIEWS_WRITE_TOOLSET_NAME, true)
    }

    fn tools(&self) -> Result<Vec<OptionalRegistryTool>, SchemaContractError> {
        collection_member_tools()
    }

    fn scripted_scenario_ids(&self) -> &'static [&'static str] {
        SCRIPTED_SCENARIOS
    }

    fn headless_scenario_ids(&self) -> &'static [&'static str] {
        HEADLESS_SCENARIOS
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
        Box::pin(async move {
            if !matches!(
                request.name.as_ref(),
                COLLECTION_MEMBER_LIST | COLLECTION_MEMBER_ADD | COLLECTION_MEMBER_REMOVE
            ) {
                return Err(ErrorData::method_not_found::<CallToolRequestMethod>());
            }
            let handlers = CollectionMemberHandlers::new().map_err(|_| {
                ErrorData::internal_error("Collection membership contracts unavailable.", None)
            })?;
            Box::pin(handlers.call_tool(request, runtime, cursors, cancellation)).await
        })
    }
}

#[cfg(feature = "acceptance-harness")]
struct ViewsWriteAcceptanceRegistry {
    handlers: CollectionMemberHandlers,
    recorder: Option<std::sync::Arc<AcceptanceMetricsRecorder>>,
    forced_add_rejection: Option<u16>,
    isolate_cancellation: bool,
}

/// Test-only mutation mode shared by direct and spawned acceptance drivers.
#[cfg(feature = "acceptance-harness")]
#[doc(hidden)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AcceptanceMutationMode {
    /// Run the production handler without an acceptance seam.
    #[default]
    Normal,
    /// Cancel after add preflight but before the dispatch marker.
    CancelAddBeforeMark,
    /// Cancel immediately after the add dispatch marker.
    CancelAddAfterMark,
    /// Cancel after remove preflight but before the dispatch marker.
    CancelRemoveBeforeMark,
    /// Cancel immediately after the remove dispatch marker.
    CancelRemoveAfterMark,
    /// Exercise only the production 403 classifier without upstream I/O.
    ClassifyAdd403,
    /// Hold two absent-state add calls at the pre-dispatch boundary.
    ConcurrentAdd,
}

#[cfg(feature = "acceptance-harness")]
impl AcceptanceMutationMode {
    const fn isolates_injected_cancellation(self) -> bool {
        matches!(
            self,
            Self::CancelAddBeforeMark
                | Self::CancelAddAfterMark
                | Self::CancelRemoveBeforeMark
                | Self::CancelRemoveAfterMark
        )
    }
}

/// Payload-free counter snapshot emitted by acceptance drivers.
#[cfg(feature = "acceptance-harness")]
#[doc(hidden)]
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct AcceptanceMetricsSnapshot {
    pub http_logical: u64,
    pub http_physical: u64,
    pub observer_attempts: u64,
    pub query_rounds: u64,
    pub subscribe_attempts: u64,
    pub foreground_close_attempts: u64,
    pub foreground_close_successes: u64,
    pub fallback_close_attempts: u64,
    pub add_dispatches: u64,
    pub remove_dispatches: u64,
}

#[cfg(feature = "acceptance-harness")]
impl AcceptanceMetricsSnapshot {
    fn capture(client: &AnytypeClient) -> Self {
        let http = client.http_metrics();
        let membership = client.collection_membership_metrics();
        Self {
            http_logical: http.logical_operations,
            http_physical: http.physical_attempts,
            observer_attempts: membership.observer_attempts,
            query_rounds: membership.query_rounds,
            subscribe_attempts: membership.subscribe_attempts,
            foreground_close_attempts: membership.foreground_close_attempts,
            foreground_close_successes: membership.foreground_close_successes,
            fallback_close_attempts: membership.fallback_close_attempts,
            add_dispatches: membership.add_dispatches,
            remove_dispatches: membership.remove_dispatches,
        }
    }
}

#[cfg(feature = "acceptance-harness")]
struct AcceptanceMetricsRecorder {
    output: std::sync::Mutex<std::fs::File>,
}

#[cfg(feature = "acceptance-harness")]
impl AcceptanceMetricsRecorder {
    fn create(path: &std::path::Path) -> std::io::Result<Self> {
        use std::fs::OpenOptions;

        let output = OpenOptions::new().create_new(true).write(true).open(path)?;
        Ok(Self {
            output: std::sync::Mutex::new(output),
        })
    }

    fn record(&self, client: &AnytypeClient) {
        use std::io::Write as _;

        let Ok(encoded) = serde_json::to_vec(&AcceptanceMetricsSnapshot::capture(client)) else {
            tracing::error!("acceptance metrics encoding failed");
            return;
        };
        let Ok(mut output) = self.output.lock() else {
            tracing::error!("acceptance metrics lock failed");
            return;
        };
        if output
            .write_all(&encoded)
            .and_then(|()| output.write_all(b"\n"))
            .and_then(|()| output.flush())
            .is_err()
        {
            tracing::error!("acceptance metrics write failed");
        }
    }
}

#[cfg(feature = "acceptance-harness")]
impl std::fmt::Debug for ViewsWriteAcceptanceRegistry {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ViewsWriteAcceptanceRegistry")
    }
}

#[cfg(feature = "acceptance-harness")]
impl crate::optional_toolsets::OptionalToolsetRegistry for ViewsWriteAcceptanceRegistry {
    fn metadata(&self) -> crate::optional_toolsets::OptionalToolsetMetadata {
        crate::optional_toolsets::OptionalToolsetMetadata::new(VIEWS_WRITE_TOOLSET_NAME, true)
    }

    fn tools(&self) -> Result<Vec<OptionalRegistryTool>, SchemaContractError> {
        collection_member_tools()
    }

    fn scripted_scenario_ids(&self) -> &'static [&'static str] {
        &["collection_member_acceptance_stdio"]
    }

    fn headless_scenario_ids(&self) -> &'static [&'static str] {
        &["collection_member_acceptance_headless"]
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
    ) -> crate::optional_toolsets::OptionalRegistryFuture<'a, Result<CallToolResult, ErrorData>>
    {
        Box::pin(async move {
            let result = if request.name.as_ref() == COLLECTION_MEMBER_ADD
                && let Some(status) = self.forced_add_rejection
            {
                let error = definitive_add_rejection_tool(status)
                    .unwrap_or_else(ToolError::mutation_indeterminate);
                Ok(tool_error(&error))
            } else {
                let isolated_cancellation = CancellationToken::new();
                let handler_cancellation = if self.isolate_cancellation {
                    &isolated_cancellation
                } else {
                    cancellation
                };
                Box::pin(
                    self.handlers
                        .call_tool(request, runtime, cursors, handler_cancellation),
                )
                .await
            };
            if let Some(recorder) = self.recorder.as_ref() {
                recorder.record(runtime.client());
            }
            result
        })
    }
}

#[cfg(feature = "acceptance-harness")]
fn acceptance_handlers(
    mode: AcceptanceMutationMode,
) -> Result<CollectionMemberHandlers, SchemaContractError> {
    let cancel = cancellation_hook();
    let hooks = match mode {
        AcceptanceMutationMode::CancelAddBeforeMark => CollectionMutationHooks {
            before_add: Some(cancel),
            ..CollectionMutationHooks::default()
        },
        AcceptanceMutationMode::CancelAddAfterMark => CollectionMutationHooks {
            after_add_mark: Some(cancel),
            ..CollectionMutationHooks::default()
        },
        AcceptanceMutationMode::CancelRemoveBeforeMark => CollectionMutationHooks {
            before_remove: Some(cancel),
            ..CollectionMutationHooks::default()
        },
        AcceptanceMutationMode::CancelRemoveAfterMark => CollectionMutationHooks {
            after_remove_mark: Some(cancel),
            ..CollectionMutationHooks::default()
        },
        AcceptanceMutationMode::ConcurrentAdd => {
            let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(2));
            let before_add: MutationDispatchHook = std::sync::Arc::new(move |_| {
                let barrier = std::sync::Arc::clone(&barrier);
                Box::pin(async move {
                    barrier.wait().await;
                })
            });
            CollectionMutationHooks {
                before_add: Some(before_add),
                ..CollectionMutationHooks::default()
            }
        }
        AcceptanceMutationMode::Normal | AcceptanceMutationMode::ClassifyAdd403 => {
            CollectionMutationHooks::default()
        }
    };
    Ok(CollectionMemberHandlers {
        list: CollectionMemberListHandlers::new()?,
        mutations: CollectionMemberMutationHandlers::new()?.with_hooks(hooks),
    })
}

/// Direct reviewed-registry driver used by the shared acceptance scenario.
#[cfg(feature = "acceptance-harness")]
#[doc(hidden)]
pub struct ViewsWriteAcceptanceDirect {
    server: crate::server::AnyMcpServer,
    client: AnytypeClient,
}

#[cfg(feature = "acceptance-harness")]
impl ViewsWriteAcceptanceDirect {
    pub fn new(
        client: AnytypeClient,
        read_only: bool,
        mode: AcceptanceMutationMode,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        use std::time::Duration;

        use crate::{
            config::ApplicationProfile,
            optional_toolsets::{
                OptionalToolsetMetadata, OptionalToolsetRegistry, OptionalToolsetSelection,
            },
            runtime::StartupStatus,
        };

        let metadata = [OptionalToolsetMetadata::new(VIEWS_WRITE_TOOLSET_NAME, true)];
        let selection =
            OptionalToolsetSelection::parse(Some(VIEWS_WRITE_TOOLSET_NAME.to_owned()), &metadata)?;
        let runtime = RuntimeContext::from_parts_with_profile_and_optional_toolsets(
            client.clone(),
            2,
            Duration::from_secs(30),
            StartupStatus {
                http_available: true,
                grpc_available: true,
            },
            ApplicationProfile::Compact,
            read_only,
            selection,
        );
        let registry: &'static ViewsWriteAcceptanceRegistry =
            Box::leak(Box::new(ViewsWriteAcceptanceRegistry {
                handlers: acceptance_handlers(mode)?,
                recorder: None,
                forced_add_rejection: (mode == AcceptanceMutationMode::ClassifyAdd403)
                    .then_some(403),
                isolate_cancellation: mode.isolates_injected_cancellation(),
            }));
        let registries: &'static [&'static dyn OptionalToolsetRegistry] =
            Box::leak(vec![registry as &dyn OptionalToolsetRegistry].into_boxed_slice());
        let server =
            crate::server::AnyMcpServer::new_with_optional_registries(runtime, registries)?;
        Ok(Self { server, client })
    }

    pub async fn call(&self, name: &'static str, arguments: serde_json::Value) -> CallToolResult {
        let arguments = arguments.as_object().cloned().unwrap_or_default();
        Box::pin(self.server.dispatch_tool(
            CallToolRequestParams::new(name).with_arguments(arguments),
            &CancellationToken::new(),
        ))
        .await
        .unwrap_or_else(|_| tool_error(&ToolError::upstream()))
    }

    /// Dispatches two calls concurrently through the actual reviewed router.
    pub async fn call_pair(
        &self,
        name: &'static str,
        first: serde_json::Value,
        second: serde_json::Value,
    ) -> [CallToolResult; 2] {
        let (first, second) = tokio::join!(self.call(name, first), self.call(name, second));
        [first, second]
    }

    #[must_use]
    pub fn metrics(&self) -> AcceptanceMetricsSnapshot {
        AcceptanceMetricsSnapshot::capture(&self.client)
    }
}

/// Runs the reviewed collection-membership slice in a test-only stdio child.
///
/// This entrypoint exists only behind the non-default `acceptance-harness`
/// feature. It does not add `views-write` to the production registry inventory.
#[cfg(feature = "acceptance-harness")]
pub async fn serve_acceptance_stdio_from_env() -> Result<(), Box<dyn std::error::Error>> {
    use crate::{
        config::{ProtocolMode, RuntimeConfig},
        optional_toolsets::OptionalToolsetMetadata,
        server::AnyMcpServer,
    };

    let mut arguments = std::env::args_os().skip(1);
    let metrics_path = arguments
        .next()
        .map(std::path::PathBuf::from)
        .ok_or("acceptance harness requires a metrics path")?;
    let mode = arguments
        .next()
        .and_then(|value| value.into_string().ok())
        .ok_or("acceptance harness requires one exact mode")?;
    if arguments.next().is_some() {
        return Err("acceptance harness rejects extra arguments".into());
    }
    let (protocol, read_only, mutation_mode) = match mode.as_str() {
        "stable-normal" => (ProtocolMode::Stable, false, AcceptanceMutationMode::Normal),
        "preview-normal" => (
            ProtocolMode::Experimental20260728,
            false,
            AcceptanceMutationMode::Normal,
        ),
        "stable-read-only" => (ProtocolMode::Stable, true, AcceptanceMutationMode::Normal),
        "preview-read-only" => (
            ProtocolMode::Experimental20260728,
            true,
            AcceptanceMutationMode::Normal,
        ),
        "stable-add-before" => (
            ProtocolMode::Stable,
            false,
            AcceptanceMutationMode::CancelAddBeforeMark,
        ),
        "preview-add-before" => (
            ProtocolMode::Experimental20260728,
            false,
            AcceptanceMutationMode::CancelAddBeforeMark,
        ),
        "stable-add-after" => (
            ProtocolMode::Stable,
            false,
            AcceptanceMutationMode::CancelAddAfterMark,
        ),
        "preview-add-after" => (
            ProtocolMode::Experimental20260728,
            false,
            AcceptanceMutationMode::CancelAddAfterMark,
        ),
        "stable-remove-before" => (
            ProtocolMode::Stable,
            false,
            AcceptanceMutationMode::CancelRemoveBeforeMark,
        ),
        "preview-remove-before" => (
            ProtocolMode::Experimental20260728,
            false,
            AcceptanceMutationMode::CancelRemoveBeforeMark,
        ),
        "stable-remove-after" => (
            ProtocolMode::Stable,
            false,
            AcceptanceMutationMode::CancelRemoveAfterMark,
        ),
        "preview-remove-after" => (
            ProtocolMode::Experimental20260728,
            false,
            AcceptanceMutationMode::CancelRemoveAfterMark,
        ),
        "stable-classify-403" => (
            ProtocolMode::Stable,
            false,
            AcceptanceMutationMode::ClassifyAdd403,
        ),
        "preview-classify-403" => (
            ProtocolMode::Experimental20260728,
            false,
            AcceptanceMutationMode::ClassifyAdd403,
        ),
        "stable-concurrent-add" => (
            ProtocolMode::Stable,
            false,
            AcceptanceMutationMode::ConcurrentAdd,
        ),
        "preview-concurrent-add" => (
            ProtocolMode::Experimental20260728,
            false,
            AcceptanceMutationMode::ConcurrentAdd,
        ),
        _ => return Err("acceptance harness mode is invalid".into()),
    };
    let metadata = [OptionalToolsetMetadata::new(VIEWS_WRITE_TOOLSET_NAME, true)];
    let mut config = RuntimeConfig::from_env_with_optional_metadata(&metadata)?;
    if !config.optional_toolsets.is_empty() {
        return Err("acceptance harness does not accept a registry selector".into());
    }
    config.optional_toolsets = crate::optional_toolsets::OptionalToolsetSelection::parse(
        Some(VIEWS_WRITE_TOOLSET_NAME.to_owned()),
        &metadata,
    )?;
    config.protocol_mode = protocol;
    config.read_only = read_only;
    let runtime = if mutation_mode == AcceptanceMutationMode::ClassifyAdd403 {
        use std::time::Duration;

        use anytype::prelude::{ClientConfig, HttpCredentials};

        use crate::{config::ApplicationProfile, runtime::StartupStatus};

        let client = AnytypeClient::with_config(ClientConfig {
            base_url: Some("http://127.0.0.1:1".to_owned()),
            keystore: Some("env".to_owned()),
            keystore_service: Some("views-write-offline-classifier".to_owned()),
            app_name: "views-write-offline-classifier".to_owned(),
            disable_cache: true,
            ..ClientConfig::default()
        })?;
        client.set_api_key(HttpCredentials::new("offline-classifier-token"));
        RuntimeContext::from_parts_with_profile_and_optional_toolsets(
            client,
            1,
            Duration::from_secs(2),
            StartupStatus {
                http_available: true,
                grpc_available: true,
            },
            ApplicationProfile::Compact,
            false,
            config.optional_toolsets.clone(),
        )
    } else {
        RuntimeContext::start(&config).await?
    };
    let recorder = std::sync::Arc::new(AcceptanceMetricsRecorder::create(&metrics_path)?);
    recorder.record(runtime.client());
    let registry: &'static ViewsWriteAcceptanceRegistry =
        Box::leak(Box::new(ViewsWriteAcceptanceRegistry {
            handlers: acceptance_handlers(mutation_mode)?,
            recorder: Some(recorder),
            forced_add_rejection: (mutation_mode == AcceptanceMutationMode::ClassifyAdd403)
                .then_some(403),
            isolate_cancellation: mutation_mode.isolates_injected_cancellation(),
        }));
    let registries: &'static [&'static dyn crate::optional_toolsets::OptionalToolsetRegistry] =
        Box::leak(
            vec![registry as &dyn crate::optional_toolsets::OptionalToolsetRegistry]
                .into_boxed_slice(),
        );
    let server = AnyMcpServer::new_with_optional_registries(runtime, registries)?;
    crate::stdio::serve_stdio(server, protocol).await?;
    Ok(())
}

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

/// Strict identity-only input shared by collection membership mutations.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CollectionMemberMutationInput {
    /// Unique space name or stable identifier.
    space: CollectionSpaceRef,
    /// Exact manual collection identifier.
    collection_id: EntityId,
    /// Exact object identifier; names and queries are not accepted.
    object_id: EntityId,
}

/// Fixed wire value proving that direct membership is present.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MembershipPresent;

impl Serialize for MembershipPresent {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str("present")
    }
}

impl JsonSchema for MembershipPresent {
    fn schema_name() -> Cow<'static, str> {
        "MembershipPresent".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({"type":"string","const":"present"})
    }
}

/// Fixed wire value proving that direct membership is absent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MembershipAbsent;

impl Serialize for MembershipAbsent {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str("absent")
    }
}

impl JsonSchema for MembershipAbsent {
    fn schema_name() -> Cow<'static, str> {
        "MembershipAbsent".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({"type":"string","const":"absent"})
    }
}

/// Exact verified result of `collection_member_add`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CollectionMembershipPresent {
    /// Exact manual collection whose membership was observed.
    collection_id: EntityId,
    /// Exact object observed in the collection.
    object_id: EntityId,
    /// Verified desired state, always `present`.
    membership: MembershipPresent,
}

/// Exact verified result of `collection_member_remove`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CollectionMembershipAbsent {
    /// Exact manual collection whose membership was observed.
    collection_id: EntityId,
    /// Exact object observed outside the collection.
    object_id: EntityId,
    /// Verified desired state, always `absent`.
    membership: MembershipAbsent,
}

/// Constructs the approved `collection_member_add` contract.
pub fn collection_member_add_tool()
-> Result<WorkflowTool<CollectionMembershipPresent>, SchemaContractError> {
    workflow_tool::<CollectionMemberMutationInput, CollectionMembershipPresent>(
        COLLECTION_MEMBER_ADD,
        "Ensure that one exact object is a direct member of one exact manual collection, with independent bounded verification and no saved-view behavior.",
        ToolProfile::Update,
    )
}

/// Constructs the approved `collection_member_remove` contract.
pub fn collection_member_remove_tool()
-> Result<WorkflowTool<CollectionMembershipAbsent>, SchemaContractError> {
    workflow_tool::<CollectionMemberMutationInput, CollectionMembershipAbsent>(
        COLLECTION_MEMBER_REMOVE,
        "Ensure that one exact object is not a direct member of one exact manual collection, without deleting the object or changing saved views.",
        ToolProfile::Update,
    )
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

/// Returns all three collection-membership tools for terminal registry composition.
pub fn collection_member_tools() -> Result<Vec<OptionalRegistryTool>, SchemaContractError> {
    Ok(vec![
        OptionalRegistryTool::read(collection_member_list_tool()?),
        OptionalRegistryTool::mutation(collection_member_add_tool()?),
        OptionalRegistryTool::mutation(collection_member_remove_tool()?),
    ])
}

/// Transport-neutral handler for the production membership-list workflow.
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
                registry: VIEWS_WRITE_TOOLSET_NAME,
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
        let operation = Box::pin(async move {
            let space_id = client.resolve_space_id(input.space.as_str()).await?;
            let binding = page_query_fingerprint(
                COLLECTION_MEMBER_LIST,
                common_limit,
                &ResolvedPageParams {
                    registry: VIEWS_WRITE_TOOLSET_NAME,
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
        });
        Box::pin(execute_prepared_handler(
            runtime,
            &self.contract,
            OperationContext::new(COLLECTION_MEMBER_LIST),
            cancellation,
            operation,
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
        ))
        .await
    }
}

#[cfg(any(test, feature = "acceptance-harness"))]
type MutationDispatchHook = std::sync::Arc<
    dyn Fn(CancellationToken) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>
        + Send
        + Sync,
>;

#[cfg(any(test, feature = "acceptance-harness"))]
fn cancellation_hook() -> MutationDispatchHook {
    std::sync::Arc::new(|cancellation| {
        Box::pin(async move {
            cancellation.cancel();
        })
    })
}

#[derive(Clone, Default)]
struct CollectionMutationHooks {
    #[cfg(any(test, feature = "acceptance-harness"))]
    before_add: Option<MutationDispatchHook>,
    #[cfg(any(test, feature = "acceptance-harness"))]
    after_add_mark: Option<MutationDispatchHook>,
    #[cfg(any(test, feature = "acceptance-harness"))]
    before_remove: Option<MutationDispatchHook>,
    #[cfg(any(test, feature = "acceptance-harness"))]
    after_remove_mark: Option<MutationDispatchHook>,
}

/// Transport-neutral desired-state handlers for collection membership writes.
#[derive(Clone)]
pub struct CollectionMemberMutationHandlers {
    verify_config: VerifyConfig,
    add_contract: WorkflowTool<CollectionMembershipPresent>,
    remove_contract: WorkflowTool<CollectionMembershipAbsent>,
    hooks: CollectionMutationHooks,
}

impl std::fmt::Debug for CollectionMemberMutationHandlers {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CollectionMemberMutationHandlers")
            .field("verify_config", &self.verify_config)
            .finish_non_exhaustive()
    }
}

impl CollectionMemberMutationHandlers {
    /// Creates mutation handlers with the reviewed ten-attempt, three-second verifier.
    pub fn new() -> Result<Self, SchemaContractError> {
        Self::build(VerifyConfig::default())
    }

    fn build(verify_config: VerifyConfig) -> Result<Self, SchemaContractError> {
        Ok(Self {
            verify_config,
            add_contract: collection_member_add_tool()?,
            remove_contract: collection_member_remove_tool()?,
            hooks: CollectionMutationHooks::default(),
        })
    }

    #[cfg(any(test, feature = "acceptance-harness"))]
    fn with_hooks(mut self, hooks: CollectionMutationHooks) -> Self {
        self.hooks = hooks;
        self
    }

    /// Dispatches one collection membership mutation after the catalog gate.
    pub async fn call_tool(
        &self,
        request: CallToolRequestParams,
        runtime: &RuntimeContext,
        cancellation: &CancellationToken,
    ) -> Result<CallToolResult, ErrorData> {
        if runtime.is_read_only()
            && matches!(
                request.name.as_ref(),
                COLLECTION_MEMBER_ADD | COLLECTION_MEMBER_REMOVE
            )
        {
            return Ok(tool_error(&ToolError::validation()));
        }
        match request.name.as_ref() {
            COLLECTION_MEMBER_ADD => {
                let input = decode_arguments::<CollectionMemberMutationInput>(request.arguments)?;
                Ok(self
                    .collection_member_add(runtime, MutationAccess::Allowed, input, cancellation)
                    .await)
            }
            COLLECTION_MEMBER_REMOVE => {
                let input = decode_arguments::<CollectionMemberMutationInput>(request.arguments)?;
                Ok(self
                    .collection_member_remove(runtime, MutationAccess::Allowed, input, cancellation)
                    .await)
            }
            _ => Err(ErrorData::method_not_found::<CallToolRequestMethod>()),
        }
    }

    async fn collection_member_add(
        &self,
        runtime: &RuntimeContext,
        access: MutationAccess,
        input: CollectionMemberMutationInput,
        cancellation: &CancellationToken,
    ) -> CallToolResult {
        if let Err(error) = require_mutation_access(access) {
            return tool_error(error.tool_error());
        }
        let client = runtime.client().clone();
        let verify_config = self.verify_config.clone();
        let progress = MutationProgress::new();
        let operation_progress = progress.clone();
        let operation_cancellation = cancellation.clone();
        let hooks = self.hooks.clone();
        let operation = Box::pin(async move {
            let identity = resolve_membership_identity(&client, input).await?;
            let observed = client
                .observe_collection_membership(
                    identity.space_id.as_str(),
                    identity.collection_id.as_str(),
                    identity.object_id.as_str(),
                )
                .await?;
            let state = checked_membership_state(&observed, &identity)?;
            if state == CollectionMembershipState::Present {
                return Ok(present_output(&identity));
            }

            run_add_before_hook(&hooks, &operation_cancellation).await;
            if operation_cancellation.is_cancelled() {
                return Err(HandlerError::new(ToolError::upstream()).into());
            }
            operation_progress.mark_dispatched(runtime)?;
            run_add_after_mark_hook(&hooks, &operation_cancellation).await;
            match client
                .collection_member_add(
                    identity.space_id.as_str(),
                    identity.collection_id.as_str(),
                    identity.object_id.as_str(),
                )
                .await
            {
                Ok(CollectionMemberAddOutcome::Acknowledged) => {}
                Ok(CollectionMemberAddOutcome::Rejected { status }) => {
                    if let Some(error) = definitive_add_rejection(status) {
                        return Err(error);
                    }
                    return Err(indeterminate_membership_operation());
                }
                Ok(CollectionMemberAddOutcome::Indeterminate { .. }) => {
                    return Err(indeterminate_membership_operation());
                }
                Err(_) => return Err(indeterminate_membership_operation()),
            }

            let verified = verify_membership(
                &client,
                &verify_config,
                &identity,
                CollectionMembershipState::Present,
            )
            .await?;
            checked_membership_state(&verified, &identity)
                .map(|_| present_output(&identity))
                .map_err(|_| indeterminate_membership_operation())
        });
        Box::pin(execute_mutation_handler(
            runtime,
            &self.add_contract,
            OperationContext::new(COLLECTION_MEMBER_ADD),
            cancellation,
            &progress,
            operation,
            |output| async move { Ok(output) },
        ))
        .await
    }

    async fn collection_member_remove(
        &self,
        runtime: &RuntimeContext,
        access: MutationAccess,
        input: CollectionMemberMutationInput,
        cancellation: &CancellationToken,
    ) -> CallToolResult {
        if let Err(error) = require_mutation_access(access) {
            return tool_error(error.tool_error());
        }
        let client = runtime.client().clone();
        let verify_config = self.verify_config.clone();
        let progress = MutationProgress::new();
        let operation_progress = progress.clone();
        let operation_cancellation = cancellation.clone();
        let hooks = self.hooks.clone();
        let operation = Box::pin(async move {
            let identity = resolve_membership_identity(&client, input).await?;
            let observed = client
                .observe_collection_membership(
                    identity.space_id.as_str(),
                    identity.collection_id.as_str(),
                    identity.object_id.as_str(),
                )
                .await?;
            let state = checked_membership_state(&observed, &identity)?;
            if state == CollectionMembershipState::Absent {
                return Ok(absent_output(&identity));
            }

            run_remove_before_hook(&hooks, &operation_cancellation).await;
            if operation_cancellation.is_cancelled() {
                return Err(HandlerError::new(ToolError::upstream()).into());
            }
            operation_progress.mark_dispatched(runtime)?;
            run_remove_after_mark_hook(&hooks, &operation_cancellation).await;
            if client
                .view_remove_object(
                    identity.space_id.as_str(),
                    identity.collection_id.as_str(),
                    identity.object_id.as_str(),
                )
                .await
                .is_err()
            {
                return Err(indeterminate_membership_operation());
            }

            let verified = verify_membership(
                &client,
                &verify_config,
                &identity,
                CollectionMembershipState::Absent,
            )
            .await?;
            checked_membership_state(&verified, &identity)
                .map(|_| absent_output(&identity))
                .map_err(|_| indeterminate_membership_operation())
        });
        Box::pin(execute_mutation_handler(
            runtime,
            &self.remove_contract,
            OperationContext::new(COLLECTION_MEMBER_REMOVE),
            cancellation,
            &progress,
            operation,
            |output| async move { Ok(output) },
        ))
        .await
    }
}

/// Combined list and mutation dispatcher for the production registry.
#[derive(Clone, Debug)]
pub struct CollectionMemberHandlers {
    list: CollectionMemberListHandlers,
    mutations: CollectionMemberMutationHandlers,
}

impl CollectionMemberHandlers {
    /// Creates the complete production collection-membership dispatcher.
    pub fn new() -> Result<Self, SchemaContractError> {
        Ok(Self {
            list: CollectionMemberListHandlers::new()?,
            mutations: CollectionMemberMutationHandlers::new()?,
        })
    }

    /// Dispatches one exact collection membership tool after catalog selection.
    pub async fn call_tool(
        &self,
        request: CallToolRequestParams,
        runtime: &RuntimeContext,
        cursors: &CursorStore,
        cancellation: &CancellationToken,
    ) -> Result<CallToolResult, ErrorData> {
        match request.name.as_ref() {
            COLLECTION_MEMBER_LIST => {
                Box::pin(self.list.call_tool(request, runtime, cursors, cancellation)).await
            }
            COLLECTION_MEMBER_ADD | COLLECTION_MEMBER_REMOVE => {
                Box::pin(self.mutations.call_tool(request, runtime, cancellation)).await
            }
            _ => Err(ErrorData::method_not_found::<CallToolRequestMethod>()),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct MembershipIdentity {
    space_id: EntityId,
    collection_id: EntityId,
    object_id: EntityId,
}

async fn resolve_membership_identity(
    client: &AnytypeClient,
    input: CollectionMemberMutationInput,
) -> Result<MembershipIdentity, HandlerOperationError> {
    let resolved_space = client.resolve_space_id(input.space.as_str()).await?;
    let space_id = EntityId::new(resolved_space).map_err(unsafe_membership_identity)?;
    Ok(MembershipIdentity {
        space_id,
        collection_id: input.collection_id,
        object_id: input.object_id,
    })
}

fn checked_membership_state(
    observation: &ApiCollectionMembershipObservation,
    identity: &MembershipIdentity,
) -> Result<CollectionMembershipState, HandlerOperationError> {
    if observation.space_id != identity.space_id.as_str()
        || observation.collection_id != identity.collection_id.as_str()
        || observation.object_id != identity.object_id.as_str()
    {
        return Err(HandlerError::new(ToolError::upstream()).into());
    }
    Ok(observation.state)
}

async fn verify_membership(
    client: &AnytypeClient,
    verify_config: &VerifyConfig,
    identity: &MembershipIdentity,
    desired: CollectionMembershipState,
) -> Result<ApiCollectionMembershipObservation, HandlerOperationError> {
    verify_semantic(
        verify_config,
        "collection membership",
        "identity-redacted",
        || {
            client.observe_collection_membership(
                identity.space_id.as_str(),
                identity.collection_id.as_str(),
                identity.object_id.as_str(),
            )
        },
        |observation| {
            observation.space_id == identity.space_id.as_str()
                && observation.collection_id == identity.collection_id.as_str()
                && observation.object_id == identity.object_id.as_str()
                && observation.state == desired
        },
    )
    .await
    .map_err(|_| indeterminate_membership_operation())
}

fn definitive_add_rejection(status: u16) -> Option<HandlerOperationError> {
    definitive_add_rejection_tool(status).map(|error| HandlerError::new(error).into())
}

fn definitive_add_rejection_tool(status: u16) -> Option<ToolError> {
    let error = match status {
        400 | 422 => ToolError::validation(),
        401 | 403 => ToolError::authentication(),
        404 => ToolError::not_found(),
        409 => ToolError::conflict(),
        _ => return None,
    };
    Some(error)
}

fn present_output(identity: &MembershipIdentity) -> CollectionMembershipPresent {
    CollectionMembershipPresent {
        collection_id: identity.collection_id.clone(),
        object_id: identity.object_id.clone(),
        membership: MembershipPresent,
    }
}

fn absent_output(identity: &MembershipIdentity) -> CollectionMembershipAbsent {
    CollectionMembershipAbsent {
        collection_id: identity.collection_id.clone(),
        object_id: identity.object_id.clone(),
        membership: MembershipAbsent,
    }
}

fn indeterminate_membership_operation() -> HandlerOperationError {
    HandlerError::new(ToolError::mutation_indeterminate()).into()
}

fn unsafe_membership_identity(_: DomainValueError) -> HandlerOperationError {
    HandlerError::new(ToolError::upstream()).into()
}

async fn run_add_before_hook(hooks: &CollectionMutationHooks, cancellation: &CancellationToken) {
    #[cfg(any(test, feature = "acceptance-harness"))]
    if let Some(hook) = hooks.before_add.as_ref() {
        hook(cancellation.clone()).await;
        tokio::task::yield_now().await;
    }
    #[cfg(not(any(test, feature = "acceptance-harness")))]
    let _ = (hooks, cancellation);
}

async fn run_add_after_mark_hook(
    hooks: &CollectionMutationHooks,
    cancellation: &CancellationToken,
) {
    #[cfg(any(test, feature = "acceptance-harness"))]
    if let Some(hook) = hooks.after_add_mark.as_ref() {
        hook(cancellation.clone()).await;
        tokio::task::yield_now().await;
    }
    #[cfg(not(any(test, feature = "acceptance-harness")))]
    let _ = (hooks, cancellation);
}

async fn run_remove_before_hook(hooks: &CollectionMutationHooks, cancellation: &CancellationToken) {
    #[cfg(any(test, feature = "acceptance-harness"))]
    if let Some(hook) = hooks.before_remove.as_ref() {
        hook(cancellation.clone()).await;
        tokio::task::yield_now().await;
    }
    #[cfg(not(any(test, feature = "acceptance-harness")))]
    let _ = (hooks, cancellation);
}

async fn run_remove_after_mark_hook(
    hooks: &CollectionMutationHooks,
    cancellation: &CancellationToken,
) {
    #[cfg(any(test, feature = "acceptance-harness"))]
    if let Some(hook) = hooks.after_remove_mark.as_ref() {
        hook(cancellation.clone()).await;
        tokio::task::yield_now().await;
    }
    #[cfg(not(any(test, feature = "acceptance-harness")))]
    let _ = (hooks, cancellation);
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
    use rmcp::model::{ListToolsResult, ToolAnnotations};
    use serde_json::{Value, json};
    use sha2::{Digest, Sha256};
    use tiktoken_rs::{CoreBPE, o200k_base};
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, duplex, split};

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
    const TOKEN_BUDGET_SNAPSHOT: &str =
        include_str!("../tests/snapshots/collection-membership-token-budget.json");
    const SUFFIX_ALPHABET: &[u8] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789._~-";

    fn binding(space_id: &str, collection_id: &str, limit: u16) -> QueryFingerprint {
        page_query_fingerprint(
            COLLECTION_MEMBER_LIST,
            PageLimit::new(limit).expect("valid common limit"),
            &ResolvedPageParams {
                registry: VIEWS_WRITE_TOOLSET_NAME,
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
        let add = collection_member_add_tool().expect("add contract");
        let remove = collection_member_remove_tool().expect("remove contract");
        for tool in [add.as_tool(), remove.as_tool()] {
            assert_eq!(
                tool.annotations,
                Some(
                    ToolAnnotations::new()
                        .read_only(false)
                        .destructive(true)
                        .idempotent(false)
                        .open_world(false)
                )
            );
            let input = serde_json::to_value(tool.input_schema.as_ref()).expect("mutation input");
            assert_eq!(input["additionalProperties"], false);
            assert_eq!(
                input["required"],
                json!(["space", "collection_id", "object_id"])
            );
            assert_eq!(
                input["properties"]
                    .as_object()
                    .expect("mutation properties")
                    .keys()
                    .collect::<Vec<_>>(),
                ["collection_id", "object_id", "space"]
            );
        }
        let add_output =
            serde_json::to_value(add.as_tool().output_schema.as_ref()).expect("add output schema");
        let remove_output = serde_json::to_value(remove.as_tool().output_schema.as_ref())
            .expect("remove output schema");
        assert_eq!(add_output["$defs"]["MembershipPresent"]["const"], "present");
        assert_eq!(
            remove_output["$defs"]["MembershipAbsent"]["const"],
            "absent"
        );
        assert_eq!(collection_member_list_tools().expect("list slice").len(), 1);
        assert_eq!(collection_member_tools().expect("complete slice").len(), 3);
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

        for value in [
            json!({"space":SPACE_ID,"collection_id":COLLECTION_ID}),
            json!({"space":SPACE_ID,"collection_id":COLLECTION_ID,"object_id":"object","view_id":"view"}),
            json!({"space":SPACE_ID,"collection_id":COLLECTION_ID,"object_id":"object","filter":{}}),
            json!({"space":SPACE_ID,"collection_id":COLLECTION_ID,"object_id":"object","query":"x"}),
            json!({"space":SPACE_ID,"collection_id":COLLECTION_ID,"object_id":"object","layout":"kanban"}),
            json!({"space":SPACE_ID,"collection_id":null,"object_id":"object"}),
            json!({"space":SPACE_ID,"collection_id":COLLECTION_ID,"object_id":"bad/id"}),
        ] {
            assert!(
                serde_json::from_value::<CollectionMemberMutationInput>(value).is_err(),
                "unexpectedly accepted strict mutation input"
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
                "registry":VIEWS_WRITE_TOOLSET_NAME,
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

    fn membership_identity() -> MembershipIdentity {
        MembershipIdentity {
            space_id: EntityId::new(SPACE_ID).expect("space ID"),
            collection_id: EntityId::new(COLLECTION_ID).expect("collection ID"),
            object_id: EntityId::new("object-a").expect("object ID"),
        }
    }

    fn membership_observation(
        state: CollectionMembershipState,
    ) -> ApiCollectionMembershipObservation {
        ApiCollectionMembershipObservation {
            space_id: SPACE_ID.to_owned(),
            collection_id: COLLECTION_ID.to_owned(),
            object_id: "object-a".to_owned(),
            state,
        }
    }

    #[test]
    fn mutation_outputs_and_observer_identity_are_exact() {
        let identity = membership_identity();
        assert_eq!(
            serde_json::to_value(present_output(&identity)).expect("present output"),
            json!({
                "collection_id":COLLECTION_ID,
                "object_id":"object-a",
                "membership":"present"
            })
        );
        assert_eq!(
            serde_json::to_value(absent_output(&identity)).expect("absent output"),
            json!({
                "collection_id":COLLECTION_ID,
                "object_id":"object-a",
                "membership":"absent"
            })
        );
        for state in [
            CollectionMembershipState::Present,
            CollectionMembershipState::Absent,
        ] {
            assert_eq!(
                checked_membership_state(&membership_observation(state), &identity)
                    .expect("exact observation"),
                state
            );
        }
        for field in ["space", "collection", "object"] {
            let mut observation = membership_observation(CollectionMembershipState::Present);
            match field {
                "space" => observation.space_id = "other-space".to_owned(),
                "collection" => observation.collection_id = "other-collection".to_owned(),
                "object" => observation.object_id = "other-object".to_owned(),
                _ => unreachable!("fixed test field"),
            }
            assert!(checked_membership_state(&observation, &identity).is_err());
        }
    }

    #[test]
    fn post_definitive_rejection_allowlist_is_exact() {
        for code in 300..=599 {
            let actual = CollectionMemberAddOutcome::Rejected { status: code };
            let CollectionMemberAddOutcome::Rejected { status } = actual else {
                unreachable!("fixed rejection variant");
            };
            assert_eq!(
                definitive_add_rejection(status).is_some(),
                matches!(code, 400 | 401 | 403 | 404 | 409 | 422),
                "{code}"
            );
        }
        assert!(matches!(
            CollectionMemberAddOutcome::Acknowledged,
            CollectionMemberAddOutcome::Acknowledged
        ));
    }

    #[test]
    fn mutation_error_mapping_never_exposes_identity_or_response_text() {
        for code in [400, 401, 403, 404, 409, 422] {
            let source = AnytypeError::ApiError {
                code,
                method: "post".to_owned(),
                url: "/SECRET_SPACE/SECRET_COLLECTION/SECRET_OBJECT".to_owned(),
                message: "SECRET_UPSTREAM_BODY".to_owned(),
            };
            let crate::error::AnytypeErrorMapping::Ready(mapped) = ToolError::from_anytype(&source)
            else {
                panic!("fixed status must map directly");
            };
            let encoded = serde_json::to_string(&mapped).expect("fixed mapped error");
            assert!(!encoded.contains("SECRET"));
        }
        let indeterminate = serde_json::to_string(&ToolError::mutation_indeterminate())
            .expect("indeterminate error");
        assert!(!indeterminate.contains("SECRET"));
        assert_eq!(
            ToolError::mutation_indeterminate().message(),
            "The mutation may have applied. Reread the object before retrying to avoid applying it twice."
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

    fn sha256_hex(bytes: &[u8]) -> String {
        Sha256::digest(bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    fn maximum_mutation_input() -> Value {
        let space = [
            "界", "🚀", "𐍈", "\0", "\u{001f}", "\"", "\\", "\n", "\r", "\t",
        ]
        .into_iter()
        .cycle()
        .take(512)
        .collect::<String>();
        json!({
            "space":space,
            "collection_id":maximum_id(0),
            "object_id":maximum_id(1)
        })
    }

    fn collection_membership_token_budget() -> Value {
        let tokenizer = o200k_base().expect("o200k tokenizer");
        let base = snapshot_server(ApplicationProfile::Compact, false, None);
        let compact = snapshot_server(
            ApplicationProfile::Compact,
            false,
            Some(VIEWS_WRITE_TOOLSET_NAME),
        );
        let compact_read_only = snapshot_server(
            ApplicationProfile::Compact,
            true,
            Some(VIEWS_WRITE_TOOLSET_NAME),
        );
        let standard = snapshot_server(
            ApplicationProfile::Standard,
            false,
            Some(VIEWS_WRITE_TOOLSET_NAME),
        );
        let standard_read_only = snapshot_server(
            ApplicationProfile::Standard,
            true,
            Some(VIEWS_WRITE_TOOLSET_NAME),
        );
        let with_members = snapshot_server(
            ApplicationProfile::Compact,
            false,
            Some("members,views-write"),
        );
        let per_tool = compact
            .tools()
            .iter()
            .filter(|tool| {
                matches!(
                    tool.name.as_ref(),
                    COLLECTION_MEMBER_LIST | COLLECTION_MEMBER_ADD | COLLECTION_MEMBER_REMOVE
                )
            })
            .map(|tool| {
                (
                    tool.name.to_string(),
                    token_count(&tokenizer, serde_json::to_value(tool).expect("tool value")),
                )
            })
            .collect::<BTreeMap<_, _>>();
        let maximum_result = collection_member_list_tool()
            .expect("list contract")
            .success(&maximum_page(61))
            .expect("maximum result");
        let maximum_result_value =
            serde_json::to_value(maximum_result).expect("maximum result value");
        let maximum_result_json = canonical_compact(maximum_result_value.clone());
        let protocol_composition = |render: fn(&AnyMcpServer) -> Value| {
            let base_value = render(&base);
            let base_json = canonical_compact(base_value.clone());
            json!({
                "base_catalog_sha256":sha256_hex(base_json.as_bytes()),
                "base_catalog_tokens":token_count(&tokenizer, base_value),
                "compact_composed_total_tokens":token_count(&tokenizer, render(&compact)),
                "compact_read_only_total_tokens":token_count(&tokenizer, render(&compact_read_only)),
                "standard_composed_total_tokens":token_count(&tokenizer, render(&standard)),
                "standard_read_only_total_tokens":token_count(&tokenizer, render(&standard_read_only)),
                "members_views_write_compact_total_tokens":token_count(&tokenizer, render(&with_members))
            })
        };
        json!({
            "tokenizer":"tiktoken o200k_base (tiktoken-rs 0.12.0)",
            "selected":[VIEWS_WRITE_TOOLSET_NAME],
            "views_write_domain_ceiling_tokens":VIEWS_WRITE_CATALOG_TOKEN_CEILING,
            "views_write_selected_ceiling_tokens":3500,
            "per_tool_tokens":per_tool,
            "protocol_compositions":{
                "stable_2025_11_25":protocol_composition(tools_list_value),
                "preview_2026_07_28":protocol_composition(preview_tools_list_value)
            },
            "adversarial_maximum_mutation_input_tokens":token_count(
                &tokenizer,
                maximum_mutation_input()
            ),
            "representative_max_result_items":61,
            "representative_max_result_bytes":maximum_result_json.len(),
            "representative_max_result_tokens":token_count(&tokenizer, maximum_result_value),
            "representative_max_result_sha256":sha256_hex(maximum_result_json.as_bytes())
        })
    }

    #[test]
    fn collection_membership_catalog_and_results_match_reviewed_token_snapshot() {
        let actual = canonical_json(collection_membership_token_budget());
        let reviewed = canonical_json(
            serde_json::from_str(TOKEN_BUDGET_SNAPSHOT)
                .expect("collection-membership token snapshot"),
        );
        assert_eq!(
            actual, reviewed,
            "collection-membership token budget drifted"
        );
        assert_eq!(actual["selected"], json!([VIEWS_WRITE_TOOLSET_NAME]));
        let domain_tokens = actual["per_tool_tokens"]
            .as_object()
            .expect("per-tool tokens")
            .values()
            .map(|value| value.as_u64().expect("token count") as usize)
            .sum::<usize>();
        assert!(domain_tokens <= VIEWS_WRITE_CATALOG_TOKEN_CEILING);
        for protocol in ["stable_2025_11_25", "preview_2026_07_28"] {
            let composition = &actual["protocol_compositions"][protocol];
            let selected_added = composition["compact_composed_total_tokens"]
                .as_u64()
                .expect("composed tokens")
                .saturating_sub(
                    composition["base_catalog_tokens"]
                        .as_u64()
                        .expect("base tokens"),
                );
            assert!(selected_added <= 3_500, "{protocol}");
        }
        assert_eq!(actual["representative_max_result_items"], 61);
        assert!(
            actual["representative_max_result_bytes"]
                .as_u64()
                .expect("bytes")
                <= 65_536
        );
        assert!(
            actual["representative_max_result_tokens"]
                .as_u64()
                .expect("tokens")
                <= 32_000
        );
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
        assert_eq!(COLLECTION_MEMBER_ADD_HTTP_LOGICAL_CEILING, 34);
        assert_eq!(COLLECTION_MEMBER_ADD_HTTP_PHYSICAL_CEILING, 199);
        assert_eq!(COLLECTION_MEMBER_ADD_GRPC_CEILING, 99);
        assert_eq!(COLLECTION_MEMBER_REMOVE_HTTP_LOGICAL_CEILING, 34);
        assert_eq!(COLLECTION_MEMBER_REMOVE_HTTP_PHYSICAL_CEILING, 204);
        assert_eq!(COLLECTION_MEMBER_REMOVE_GRPC_CEILING, 96);
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
        handlers: CollectionMemberHandlers,
    }

    impl fmt::Debug for TestRegistry {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("TestViewsWriteCollectionListRegistry")
        }
    }

    impl OptionalToolsetRegistry for TestRegistry {
        fn metadata(&self) -> OptionalToolsetMetadata {
            OptionalToolsetMetadata::new(VIEWS_WRITE_TOOLSET_NAME, true)
        }

        fn tools(&self) -> Result<Vec<OptionalRegistryTool>, SchemaContractError> {
            collection_member_tools()
        }

        fn scripted_scenario_ids(&self) -> &'static [&'static str] {
            &[
                "collection_member_list_direct",
                "collection_member_list_stdio",
                "collection_member_mutation_direct",
                "collection_member_mutation_stdio",
            ]
        }

        fn headless_scenario_ids(&self) -> &'static [&'static str] {
            &[
                "collection_member_list_headless",
                "collection_member_mutation_headless",
            ]
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
        let available = [OptionalToolsetMetadata::new(VIEWS_WRITE_TOOLSET_NAME, true)];
        let selection = OptionalToolsetSelection::parse(
            selected.then(|| VIEWS_WRITE_TOOLSET_NAME.to_owned()),
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

    fn production_server(selected: bool, read_only: bool) -> AnyMcpServer {
        let client = snapshot_client();
        let selection = OptionalToolsetSelection::parse(
            selected.then(|| VIEWS_WRITE_TOOLSET_NAME.to_owned()),
            &production_optional_metadata(),
        )
        .expect("production views-write selection");
        let runtime = RuntimeContext::from_parts_with_profile_and_optional_toolsets(
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
        );
        AnyMcpServer::new(runtime).expect("production views-write server")
    }

    fn server_with_runtime(runtime: RuntimeContext) -> AnyMcpServer {
        server_with_handlers(runtime, CollectionMemberHandlers::new().expect("handlers"))
    }

    fn server_with_handlers(
        runtime: RuntimeContext,
        handlers: CollectionMemberHandlers,
    ) -> AnyMcpServer {
        let registry: &'static TestRegistry = Box::leak(Box::new(TestRegistry { handlers }));
        let registries: &'static [&'static dyn OptionalToolsetRegistry] =
            Box::leak(vec![registry as &dyn OptionalToolsetRegistry].into_boxed_slice());
        AnyMcpServer::new_with_optional_registries(runtime, registries).expect("test server")
    }

    fn snapshot_client() -> AnytypeClient {
        let client = AnytypeClient::with_config(ClientConfig {
            base_url: Some("http://127.0.0.1:1".to_owned()),
            keystore: Some("env".to_owned()),
            keystore_service: Some("collection-membership-snapshot".to_owned()),
            app_name: "collection-membership-snapshot".to_owned(),
            disable_cache: true,
            ..ClientConfig::default()
        })
        .expect("snapshot client");
        client.set_api_key(HttpCredentials::new("snapshot-token"));
        client
    }

    fn snapshot_server(
        profile: ApplicationProfile,
        read_only: bool,
        selected: Option<&str>,
    ) -> AnyMcpServer {
        static REGISTRIES: [&dyn OptionalToolsetRegistry; 2] = [
            crate::member_toolset::MEMBERS_REGISTRY,
            VIEWS_WRITE_REGISTRY,
        ];
        let registries: &'static [&'static dyn OptionalToolsetRegistry] = &REGISTRIES;
        let metadata = registries
            .iter()
            .map(|candidate| candidate.metadata())
            .collect::<Vec<_>>();
        let selection = OptionalToolsetSelection::parse(selected.map(str::to_owned), &metadata)
            .expect("snapshot selection");
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
        AnyMcpServer::new_with_optional_registries(runtime, registries).expect("snapshot server")
    }

    fn tools_list_value(server: &AnyMcpServer) -> Value {
        serde_json::to_value(ListToolsResult::with_all_items(server.tools().to_vec()))
            .expect("tools list value")
    }

    fn preview_tools_list_value(server: &AnyMcpServer) -> Value {
        let mut value = tools_list_value(server);
        let object = value.as_object_mut().expect("tools list object");
        object.insert("resultType".to_owned(), json!("complete"));
        object.insert("cacheScope".to_owned(), json!("public"));
        value
    }

    fn canonical_compact(value: Value) -> String {
        serde_json::to_string(&canonical_json(value)).expect("canonical compact JSON")
    }

    fn token_count(tokenizer: &CoreBPE, value: Value) -> usize {
        tokenizer
            .encode_with_special_tokens(&canonical_compact(value))
            .len()
    }

    fn live_runtime(client: AnytypeClient, read_only: bool) -> RuntimeContext {
        let selection = OptionalToolsetSelection::parse(
            Some(VIEWS_WRITE_TOOLSET_NAME.to_owned()),
            &[OptionalToolsetMetadata::new(VIEWS_WRITE_TOOLSET_NAME, true)],
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

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    struct WorkCounts {
        http_logical: u64,
        http_physical: u64,
        membership: anytype::views::CollectionMembershipMetricsSnapshot,
    }

    fn metric_counts(client: &AnytypeClient) -> WorkCounts {
        let http = client.http_metrics();
        WorkCounts {
            http_logical: http.logical_operations,
            http_physical: http.physical_attempts,
            membership: client.collection_membership_metrics(),
        }
    }

    fn assert_resolver_rejection(before: WorkCounts, after: WorkCounts) {
        assert!(after.http_logical > before.http_logical);
        assert_list_ceiling(before, after);
        assert_eq!(
            after.membership, before.membership,
            "{before:?} -> {after:?}"
        );
    }

    fn assert_list_ceiling(before: WorkCounts, after: WorkCounts) {
        assert!(
            after.http_logical - before.http_logical
                <= COLLECTION_MEMBER_LIST_HTTP_LOGICAL_CEILING as u64
        );
        assert!(
            after.http_physical - before.http_physical
                <= COLLECTION_MEMBER_LIST_HTTP_PHYSICAL_CEILING as u64
        );
    }

    fn metric_delta(after: u64, before: u64) -> u64 {
        after.checked_sub(before).expect("metrics are monotonic")
    }

    struct ExpectedMutationWork {
        http_logical: u64,
        http_physical: u64,
        observer_attempts: u64,
        query_rounds: u64,
        add_dispatches: u64,
        remove_dispatches: u64,
        logical_ceiling: usize,
        physical_ceiling: usize,
        grpc_ceiling: usize,
    }

    fn assert_mutation_work(before: WorkCounts, after: WorkCounts, expected: ExpectedMutationWork) {
        let logical = metric_delta(after.http_logical, before.http_logical);
        let physical = metric_delta(after.http_physical, before.http_physical);
        let observed = metric_delta(
            after.membership.observer_attempts,
            before.membership.observer_attempts,
        );
        let queries = metric_delta(
            after.membership.query_rounds,
            before.membership.query_rounds,
        );
        assert_eq!(logical, expected.http_logical, "{before:?} -> {after:?}");
        assert_eq!(physical, expected.http_physical, "{before:?} -> {after:?}");
        assert!(
            logical <= expected.logical_ceiling as u64,
            "{before:?} -> {after:?}"
        );
        assert!(
            physical <= expected.physical_ceiling as u64,
            "{before:?} -> {after:?}"
        );
        assert_eq!(
            observed, expected.observer_attempts,
            "{before:?} -> {after:?}"
        );
        assert_eq!(queries, expected.query_rounds, "{before:?} -> {after:?}");
        assert_eq!(
            metric_delta(
                after.membership.subscribe_attempts,
                before.membership.subscribe_attempts
            ),
            queries,
            "{before:?} -> {after:?}"
        );
        let grpc_calls = metric_delta(
            after.membership.subscribe_attempts,
            before.membership.subscribe_attempts,
        )
        .saturating_add(metric_delta(
            after.membership.foreground_close_attempts,
            before.membership.foreground_close_attempts,
        ))
        .saturating_add(metric_delta(
            after.membership.fallback_close_attempts,
            before.membership.fallback_close_attempts,
        ));
        assert!(
            grpc_calls <= expected.grpc_ceiling as u64,
            "{before:?} -> {after:?}"
        );
        assert_eq!(
            metric_delta(
                after.membership.foreground_close_attempts,
                before.membership.foreground_close_attempts
            ),
            queries,
            "{before:?} -> {after:?}"
        );
        assert_eq!(
            metric_delta(
                after.membership.foreground_close_successes,
                before.membership.foreground_close_successes
            ),
            queries,
            "{before:?} -> {after:?}"
        );
        assert_eq!(
            metric_delta(
                after.membership.fallback_close_attempts,
                before.membership.fallback_close_attempts
            ),
            0,
            "{before:?} -> {after:?}"
        );
        assert_eq!(
            metric_delta(
                after.membership.add_dispatches,
                before.membership.add_dispatches
            ),
            expected.add_dispatches,
            "{before:?} -> {after:?}"
        );
        assert_eq!(
            metric_delta(
                after.membership.remove_dispatches,
                before.membership.remove_dispatches
            ),
            expected.remove_dispatches,
            "{before:?} -> {after:?}"
        );
    }

    fn assert_membership_result(
        result: &CallToolResult,
        collection_id: &str,
        object_id: &str,
        membership: &str,
    ) {
        assert_eq!(result.is_error, Some(false), "{result:?}");
        assert_eq!(
            result.structured_content.as_ref(),
            Some(&json!({
                "collection_id":collection_id,
                "object_id":object_id,
                "membership":membership
            }))
        );
        let expected_text = serde_json::to_string(
            result
                .structured_content
                .as_ref()
                .expect("membership result"),
        )
        .expect("membership text");
        assert_eq!(
            result
                .content
                .first()
                .and_then(|content| content.as_text())
                .map(|text| text.text.as_str()),
            Some(expected_text.as_str())
        );
    }

    #[derive(Clone, Copy, Debug)]
    enum MutationTransport {
        Direct,
    }

    async fn call_membership_mutation(
        direct_server: &AnyMcpServer,
        transport: MutationTransport,
        name: &'static str,
        args: Value,
    ) -> CallToolResult {
        match transport {
            MutationTransport::Direct => direct_named_call(direct_server, name, args).await,
        }
    }

    async fn exercise_membership_mutation_cycle(
        direct_server: &AnyMcpServer,
        client: &AnytypeClient,
        transport: MutationTransport,
        space_id: &str,
        collection_id: &str,
        object_id: &str,
        saved_view_id: &str,
    ) {
        let args = json!({
            "space":space_id,
            "collection_id":collection_id,
            "object_id":object_id
        });

        let before = metric_counts(client);
        let added = call_membership_mutation(
            direct_server,
            transport,
            COLLECTION_MEMBER_ADD,
            args.clone(),
        )
        .await;
        assert_membership_result(&added, collection_id, object_id, "present");
        assert_mutation_work(
            before,
            metric_counts(client),
            ExpectedMutationWork {
                http_logical: 5,
                http_physical: 5,
                observer_attempts: 2,
                query_rounds: 5,
                add_dispatches: 1,
                remove_dispatches: 0,
                logical_ceiling: COLLECTION_MEMBER_ADD_HTTP_LOGICAL_CEILING,
                physical_ceiling: COLLECTION_MEMBER_ADD_HTTP_PHYSICAL_CEILING,
                grpc_ceiling: COLLECTION_MEMBER_ADD_GRPC_CEILING,
            },
        );
        let canonical_after_add = client
            .collection_membership_page(space_id, collection_id, 61, None)
            .await
            .expect("canonical membership immediately after add");
        assert!(
            canonical_after_add
                .object_ids
                .contains(&object_id.to_owned())
        );
        let filtered_after_add = client
            .view_list_objects(space_id, collection_id)
            .view(saved_view_id)
            .limit(61)
            .list()
            .await
            .expect("saved-view presentation immediately after add");
        assert!(
            !filtered_after_add
                .items
                .iter()
                .any(|item| item.id == object_id)
        );

        for after_mark in [false, true] {
            exercise_remove_cancellation_boundary(
                client,
                transport,
                space_id,
                collection_id,
                object_id,
                after_mark,
            )
            .await;
        }

        let before = metric_counts(client);
        let add_noop = call_membership_mutation(
            direct_server,
            transport,
            COLLECTION_MEMBER_ADD,
            args.clone(),
        )
        .await;
        assert_membership_result(&add_noop, collection_id, object_id, "present");
        assert_mutation_work(
            before,
            metric_counts(client),
            ExpectedMutationWork {
                http_logical: 2,
                http_physical: 2,
                observer_attempts: 1,
                query_rounds: 2,
                add_dispatches: 0,
                remove_dispatches: 0,
                logical_ceiling: COLLECTION_MEMBER_ADD_HTTP_LOGICAL_CEILING,
                physical_ceiling: COLLECTION_MEMBER_ADD_HTTP_PHYSICAL_CEILING,
                grpc_ceiling: COLLECTION_MEMBER_ADD_GRPC_CEILING,
            },
        );
        let canonical_before_remove = client
            .collection_membership_page(space_id, collection_id, 61, None)
            .await
            .expect("canonical membership immediately before remove");
        assert!(
            canonical_before_remove
                .object_ids
                .contains(&object_id.to_owned())
        );

        let before = metric_counts(client);
        let removed = call_membership_mutation(
            direct_server,
            transport,
            COLLECTION_MEMBER_REMOVE,
            args.clone(),
        )
        .await;
        assert_membership_result(&removed, collection_id, object_id, "absent");
        assert_mutation_work(
            before,
            metric_counts(client),
            ExpectedMutationWork {
                http_logical: 5,
                http_physical: 5,
                observer_attempts: 2,
                query_rounds: 5,
                add_dispatches: 0,
                remove_dispatches: 1,
                logical_ceiling: COLLECTION_MEMBER_REMOVE_HTTP_LOGICAL_CEILING,
                physical_ceiling: COLLECTION_MEMBER_REMOVE_HTTP_PHYSICAL_CEILING,
                grpc_ceiling: COLLECTION_MEMBER_REMOVE_GRPC_CEILING,
            },
        );
        let canonical_after_remove = client
            .collection_membership_page(space_id, collection_id, 61, None)
            .await
            .expect("canonical membership immediately after remove");
        assert!(
            !canonical_after_remove
                .object_ids
                .contains(&object_id.to_owned())
        );

        let before = metric_counts(client);
        let remove_noop =
            call_membership_mutation(direct_server, transport, COLLECTION_MEMBER_REMOVE, args)
                .await;
        assert_membership_result(&remove_noop, collection_id, object_id, "absent");
        assert_mutation_work(
            before,
            metric_counts(client),
            ExpectedMutationWork {
                http_logical: 2,
                http_physical: 2,
                observer_attempts: 1,
                query_rounds: 3,
                add_dispatches: 0,
                remove_dispatches: 0,
                logical_ceiling: COLLECTION_MEMBER_REMOVE_HTTP_LOGICAL_CEILING,
                physical_ceiling: COLLECTION_MEMBER_REMOVE_HTTP_PHYSICAL_CEILING,
                grpc_ceiling: COLLECTION_MEMBER_REMOVE_GRPC_CEILING,
            },
        );
    }

    async fn exercise_add_cancellation_boundary(
        client: &AnytypeClient,
        transport: MutationTransport,
        space_id: &str,
        collection_id: &str,
        object_id: &str,
        after_mark: bool,
    ) {
        let cancel = cancellation_hook();
        let hooks = if after_mark {
            CollectionMutationHooks {
                after_add_mark: Some(cancel),
                ..CollectionMutationHooks::default()
            }
        } else {
            CollectionMutationHooks {
                before_add: Some(cancel),
                ..CollectionMutationHooks::default()
            }
        };
        let handlers = CollectionMemberHandlers {
            list: CollectionMemberListHandlers::new().expect("list handlers"),
            mutations: CollectionMemberMutationHandlers::new()
                .expect("mutation handlers")
                .with_hooks(hooks),
        };
        let server = server_with_handlers(live_runtime(client.clone(), false), handlers);
        let args = json!({
            "space":space_id,
            "collection_id":collection_id,
            "object_id":object_id
        });
        let before = metric_counts(client);
        let result = match transport {
            MutationTransport::Direct => {
                direct_named_call(&server, COLLECTION_MEMBER_ADD, args).await
            }
        };
        assert_eq!(result.is_error, Some(true));
        assert_eq!(
            result
                .structured_content
                .as_ref()
                .and_then(|value| value.get("code"))
                .and_then(Value::as_str),
            Some(if after_mark { "conflict" } else { "upstream" })
        );
        assert_mutation_work(
            before,
            metric_counts(client),
            ExpectedMutationWork {
                http_logical: 2,
                http_physical: 2,
                observer_attempts: 1,
                query_rounds: 3,
                add_dispatches: 0,
                remove_dispatches: 0,
                logical_ceiling: COLLECTION_MEMBER_ADD_HTTP_LOGICAL_CEILING,
                physical_ceiling: COLLECTION_MEMBER_ADD_HTTP_PHYSICAL_CEILING,
                grpc_ceiling: COLLECTION_MEMBER_ADD_GRPC_CEILING,
            },
        );
        let observed = client
            .observe_collection_membership(space_id, collection_id, object_id)
            .await
            .expect("observe cancellation state");
        assert_eq!(observed.state, CollectionMembershipState::Absent);
    }

    async fn exercise_remove_cancellation_boundary(
        client: &AnytypeClient,
        transport: MutationTransport,
        space_id: &str,
        collection_id: &str,
        object_id: &str,
        after_mark: bool,
    ) {
        let cancel = cancellation_hook();
        let hooks = if after_mark {
            CollectionMutationHooks {
                after_remove_mark: Some(cancel),
                ..CollectionMutationHooks::default()
            }
        } else {
            CollectionMutationHooks {
                before_remove: Some(cancel),
                ..CollectionMutationHooks::default()
            }
        };
        let handlers = CollectionMemberHandlers {
            list: CollectionMemberListHandlers::new().expect("list handlers"),
            mutations: CollectionMemberMutationHandlers::new()
                .expect("mutation handlers")
                .with_hooks(hooks),
        };
        let server = server_with_handlers(live_runtime(client.clone(), false), handlers);
        let args = json!({
            "space":space_id,
            "collection_id":collection_id,
            "object_id":object_id
        });
        let before = metric_counts(client);
        let result = match transport {
            MutationTransport::Direct => {
                direct_named_call(&server, COLLECTION_MEMBER_REMOVE, args).await
            }
        };
        assert_eq!(result.is_error, Some(true));
        assert_eq!(
            result
                .structured_content
                .as_ref()
                .and_then(|value| value.get("code"))
                .and_then(Value::as_str),
            Some(if after_mark { "conflict" } else { "upstream" })
        );
        assert_mutation_work(
            before,
            metric_counts(client),
            ExpectedMutationWork {
                http_logical: 2,
                http_physical: 2,
                observer_attempts: 1,
                query_rounds: 2,
                add_dispatches: 0,
                remove_dispatches: 0,
                logical_ceiling: COLLECTION_MEMBER_REMOVE_HTTP_LOGICAL_CEILING,
                physical_ceiling: COLLECTION_MEMBER_REMOVE_HTTP_PHYSICAL_CEILING,
                grpc_ceiling: COLLECTION_MEMBER_REMOVE_GRPC_CEILING,
            },
        );
        let observed = client
            .observe_collection_membership(space_id, collection_id, object_id)
            .await
            .expect("observe cancellation state");
        assert_eq!(observed.state, CollectionMembershipState::Present);
    }

    fn assert_stable_list_work(before: WorkCounts, after: WorkCounts) {
        assert_eq!(
            after.http_logical - before.http_logical,
            1,
            "{before:?} -> {after:?}"
        );
        assert_eq!(
            after.http_physical - before.http_physical,
            1,
            "{before:?} -> {after:?}"
        );
        assert_eq!(
            after.membership.query_rounds - before.membership.query_rounds,
            1,
            "{before:?} -> {after:?}"
        );
        assert_eq!(
            after.membership.subscribe_attempts - before.membership.subscribe_attempts,
            1,
            "{before:?} -> {after:?}"
        );
        assert_eq!(
            after.membership.foreground_close_attempts
                - before.membership.foreground_close_attempts,
            1,
            "{before:?} -> {after:?}"
        );
        assert_eq!(
            after.membership.foreground_close_successes
                - before.membership.foreground_close_successes,
            1,
            "{before:?} -> {after:?}"
        );
        assert_eq!(
            after.membership.fallback_close_attempts - before.membership.fallback_close_attempts,
            0,
            "{before:?} -> {after:?}"
        );
    }

    fn assert_stable_preflight_rejection(before: WorkCounts, after: WorkCounts) {
        assert_eq!(
            after.http_logical - before.http_logical,
            1,
            "{before:?} -> {after:?}"
        );
        assert_eq!(
            after.http_physical - before.http_physical,
            1,
            "{before:?} -> {after:?}"
        );
        assert_eq!(
            after.membership, before.membership,
            "{before:?} -> {after:?}"
        );
    }

    fn assert_zero_membership_io(before: WorkCounts, after: WorkCounts) {
        assert_eq!(
            after.http_logical, before.http_logical,
            "{before:?} -> {after:?}"
        );
        assert_eq!(
            after.http_physical, before.http_physical,
            "{before:?} -> {after:?}"
        );
        assert_eq!(
            after.membership, before.membership,
            "{before:?} -> {after:?}"
        );
    }

    async fn direct_call(server: &AnyMcpServer, value: Value) -> CallToolResult {
        direct_named_call(server, COLLECTION_MEMBER_LIST, value).await
    }

    async fn direct_named_call(
        server: &AnyMcpServer,
        name: &'static str,
        value: Value,
    ) -> CallToolResult {
        server
            .dispatch_tool(
                CallToolRequestParams::new(name).with_arguments(arguments(value)),
                &CancellationToken::new(),
            )
            .await
            .expect("direct router dispatch")
    }

    async fn preview_stdio_tools(server: AnyMcpServer) -> Value {
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
            "id":10,
            "method":"tools/list",
            "params":{
                "_meta":{
                    "io.modelcontextprotocol/protocolVersion":"2026-07-28",
                    "io.modelcontextprotocol/clientInfo":{
                        "name":"collection-membership-schema-test",
                        "version":"1"
                    },
                    "io.modelcontextprotocol/clientCapabilities":{}
                }
            }
        });
        client_writer
            .write_all(format!("{frame}\n").as_bytes())
            .await
            .expect("write stdio tools request");
        let mut client_reader = BufReader::new(client_reader);
        let mut line = String::new();
        client_reader
            .read_line(&mut line)
            .await
            .expect("read stdio tools response");
        drop(client_writer);
        drop(client_reader);
        task.await
            .expect("spawned stdio tools task")
            .expect("stdio tools transport");
        serde_json::from_str(&line).expect("decode stdio tools response")
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
    fn production_registry_is_exact_grpc_gated_and_read_only_projected() {
        let metadata = VIEWS_WRITE_REGISTRY.metadata();
        assert_eq!(metadata.name, VIEWS_WRITE_TOOLSET_NAME);
        assert!(metadata.requires_grpc);
        assert!(
            production_optional_metadata()
                .iter()
                .any(|entry| entry == &metadata)
        );
        assert_eq!(
            VIEWS_WRITE_REGISTRY.tools().expect("registry tools").len(),
            3
        );
        assert_eq!(
            VIEWS_WRITE_REGISTRY.scripted_scenario_ids(),
            [
                "collection_member_acceptance_direct",
                "collection_member_acceptance_stdio"
            ]
        );
        assert_eq!(
            VIEWS_WRITE_REGISTRY.headless_scenario_ids(),
            ["collection_member_acceptance_headless"]
        );
        assert_eq!(
            VIEWS_WRITE_REGISTRY.catalog_token_ceiling(),
            VIEWS_WRITE_CATALOG_TOKEN_CEILING
        );

        let base = production_server(false, false);
        let selected = production_server(true, false);
        let read_only = production_server(true, true);
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
        assert_eq!(selected.tools().len(), read_only.tools().len() + 3);
        assert_eq!(
            names(&selected)
                .iter()
                .filter(|name| name.as_str() == "optional_toolset_status")
                .count(),
            1
        );
        for mutation in [COLLECTION_MEMBER_ADD, COLLECTION_MEMBER_REMOVE] {
            assert!(names(&selected).iter().any(|name| name == mutation));
            assert!(!names(&read_only).iter().any(|name| name == mutation));
        }
        for absent in [
            "view_create",
            "view_update",
            "view_delete",
            "view_filter_set",
            "view_sort_set",
            "kanban_column_move",
            "collection_member_reorder",
        ] {
            assert!(!names(&selected).iter().any(|name| name == absent));
        }
        let selected_names = names(&selected);
        assert!(selected_names.windows(2).all(|pair| pair[0] < pair[1]));
        assert_eq!(
            selected_names
                .iter()
                .collect::<std::collections::HashSet<_>>()
                .len(),
            selected_names.len()
        );

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

    #[tokio::test]
    async fn absent_production_registry_rejects_before_decode_or_io() {
        let server = production_server(false, false);
        let client = server.runtime().client().clone();
        let before = metric_counts(&client);
        for name in [
            COLLECTION_MEMBER_LIST,
            COLLECTION_MEMBER_ADD,
            COLLECTION_MEMBER_REMOVE,
        ] {
            let error = server
                .dispatch_tool(
                    CallToolRequestParams::new(name).with_arguments(arguments(json!({
                        "secret-invalid": "must-not-decode"
                    }))),
                    &CancellationToken::new(),
                )
                .await
                .expect_err("absent production tool");
            assert_eq!(
                error.code,
                ErrorData::method_not_found::<CallToolRequestMethod>().code
            );
        }
        assert_zero_membership_io(before, metric_counts(&client));
    }

    #[tokio::test]
    async fn no_selection_is_byte_identical_without_views_write_descriptor() {
        static WITHOUT_VIEWS_WRITE: [&dyn OptionalToolsetRegistry; 3] = [
            crate::member_toolset::MEMBERS_REGISTRY,
            &crate::file_content::FILE_CONTENT_REGISTRY,
            crate::schema_toolset::SCHEMA_REGISTRY,
        ];
        let runtime = RuntimeContext::from_parts_with_profile_and_optional_toolsets(
            snapshot_client(),
            1,
            Duration::from_secs(2),
            StartupStatus {
                http_available: true,
                grpc_available: true,
            },
            ApplicationProfile::Compact,
            false,
            OptionalToolsetSelection::default(),
        );
        let production = AnyMcpServer::new(runtime.clone()).expect("production no-selection");
        let without = AnyMcpServer::new_with_optional_registries(runtime, &WITHOUT_VIEWS_WRITE)
            .expect("pre-views-write no-selection");
        assert_eq!(
            serde_json::to_vec(&ListToolsResult::with_all_items(
                production.tools().to_vec()
            ))
            .expect("production catalog bytes"),
            serde_json::to_vec(&ListToolsResult::with_all_items(without.tools().to_vec()))
                .expect("pre-views-write catalog bytes")
        );
        let production_status = production
            .dispatch_tool(
                CallToolRequestParams::new("server_status"),
                &CancellationToken::new(),
            )
            .await
            .expect("production server status");
        let without_status = without
            .dispatch_tool(
                CallToolRequestParams::new("server_status"),
                &CancellationToken::new(),
            )
            .await
            .expect("pre-views-write server status");
        assert_eq!(
            serde_json::to_vec(&production_status).expect("production status bytes"),
            serde_json::to_vec(&without_status).expect("pre-views-write status bytes")
        );
    }

    #[tokio::test]
    async fn read_only_mutations_reject_before_decode_and_io() {
        let server = production_server(true, true);
        let client = server.runtime().client().clone();
        let before = metric_counts(&client);
        for name in [COLLECTION_MEMBER_ADD, COLLECTION_MEMBER_REMOVE] {
            let result = server
                .dispatch_tool(CallToolRequestParams::new(name), &CancellationToken::new())
                .await
                .expect("read-only direct dispatch");
            assert_eq!(result.is_error, Some(true));
            assert_eq!(
                result
                    .structured_content
                    .as_ref()
                    .and_then(|value| value.get("code"))
                    .and_then(Value::as_str),
                Some("validation")
            );
        }
        assert_zero_membership_io(before, metric_counts(&client));

        let runtime = no_io_runtime(true, true);
        let client = runtime.client().clone();
        let before = metric_counts(&client);
        let handlers = CollectionMemberMutationHandlers::new().expect("mutation handlers");
        for name in [COLLECTION_MEMBER_ADD, COLLECTION_MEMBER_REMOVE] {
            let result = handlers
                .call_tool(
                    CallToolRequestParams::new(name),
                    &runtime,
                    &CancellationToken::new(),
                )
                .await
                .expect("defense-in-depth dispatch");
            assert_eq!(result.is_error, Some(true));
        }
        assert_zero_membership_io(before, metric_counts(&client));
    }

    #[tokio::test]
    async fn production_router_dispatches_all_three_contracts_in_both_protocols() {
        let server = production_server(true, false);
        let client = server.runtime().client().clone();
        let before = metric_counts(&client);
        for protocol in [
            rmcp::model::ProtocolVersion::V_2025_11_25,
            rmcp::model::ProtocolVersion::V_2026_07_28,
        ] {
            for name in [
                COLLECTION_MEMBER_LIST,
                COLLECTION_MEMBER_ADD,
                COLLECTION_MEMBER_REMOVE,
            ] {
                let error = server
                    .dispatch_tool_for_protocol(
                        CallToolRequestParams::new(name).with_arguments(arguments(json!({}))),
                        &protocol,
                        &CancellationToken::new(),
                    )
                    .await
                    .expect_err("selected contract reaches strict decoder");
                assert_eq!(error.code, rmcp::model::ErrorCode::INVALID_PARAMS);
            }
        }
        assert_zero_membership_io(before, metric_counts(&client));
    }

    #[tokio::test]
    async fn direct_and_preview_stdio_catalog_schemas_are_identical() {
        for read_only in [false, true] {
            let direct = production_server(true, read_only);
            let expected = tools_list_value(&direct);
            let stdio = preview_stdio_tools(production_server(true, read_only)).await;
            assert_eq!(stdio["result"]["tools"], expected["tools"]);
            assert_eq!(stdio["result"]["resultType"], "complete");
            assert_eq!(stdio["result"]["cacheScope"], "public");
        }
    }

    #[test]
    #[ignore = "requires env-only disposable credentials and an authenticated headless Anytype server"]
    fn headless_direct_membership_ignores_saved_view_presentation() {
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
                        assert_stable_preflight_rejection(query_before, metric_counts(&ctx.client));

                        for name in [COLLECTION_MEMBER_ADD, COLLECTION_MEMBER_REMOVE] {
                            let args = json!({
                                "space":ctx.space_id,
                                "collection_id":query.id,
                                "object_id":object_c.id
                            });
                            let before = metric_counts(&ctx.client);
                            let rejected =
                                direct_named_call(&direct_server, name, args.clone()).await;
                            assert_stable_preflight_rejection(before, metric_counts(&ctx.client));
                            assert_eq!(rejected.is_error, Some(true));
                            assert_eq!(
                                rejected
                                    .structured_content
                                    .as_ref()
                                    .and_then(|value| value.get("code"))
                                    .and_then(Value::as_str),
                                Some("upstream")
                            );
                        }

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
                        assert_stable_list_work(kanban_before, metric_counts(&ctx.client));
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
                        assert_stable_list_work(before, metric_counts(&ctx.client));
                        let first_value = first
                            .structured_content
                            .as_ref()
                            .expect("direct first structured content");
                        let cursor = first_value["next_cursor"]
                            .as_str()
                            .expect("direct continuation")
                            .to_owned();
                        let mismatch_before = metric_counts(&ctx.client);
                        let mismatch = direct_call(
                            &direct_server,
                            json!({
                                "space":ctx.space_id,
                                "collection_id":query.id,
                                "limit":1,
                                "cursor":cursor
                            }),
                        )
                        .await;
                        assert_zero_membership_io(mismatch_before, metric_counts(&ctx.client));
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
                        assert_stable_list_work(second_before, metric_counts(&ctx.client));
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

                        for transport in [MutationTransport::Direct] {
                            exercise_add_cancellation_boundary(
                                &ctx.client,
                                transport,
                                &ctx.space_id,
                                &collection.id,
                                &object_c.id,
                                false,
                            )
                            .await;
                            exercise_add_cancellation_boundary(
                                &ctx.client,
                                transport,
                                &ctx.space_id,
                                &collection.id,
                                &object_c.id,
                                true,
                            )
                            .await;
                            exercise_membership_mutation_cycle(
                                &direct_server,
                                &ctx.client,
                                transport,
                                &ctx.space_id,
                                &collection.id,
                                &object_c.id,
                                &saved_view.id,
                            )
                            .await;
                            let observed = ctx
                                .client
                                .observe_collection_membership(
                                    &ctx.space_id,
                                    &collection.id,
                                    &object_c.id,
                                )
                                .await?;
                            assert_eq!(
                                observed.state,
                                anytype::views::CollectionMembershipState::Absent
                            );
                            let survived =
                                ctx.client.object(&ctx.space_id, &object_c.id).get().await?;
                            assert_eq!(survived.id, object_c.id);
                            assert_eq!(survived.space_id, ctx.space_id);
                            let canonical = ctx
                                .client
                                .collection_membership_page(&ctx.space_id, &collection.id, 61, None)
                                .await?;
                            assert!(!canonical.object_ids.contains(&object_c.id));
                            let presentation = ctx
                                .client
                                .view_list_objects(&ctx.space_id, &collection.id)
                                .view(&saved_view.id)
                                .limit(61)
                                .list()
                                .await?;
                            assert!(presentation.items.iter().any(|item| item.id == object_a.id));
                            assert!(!presentation.items.iter().any(|item| item.id == object_c.id));
                        }

                        let read_only_arguments = json!({
                            "space":ctx.space_id,
                            "collection_id":collection.id,
                            "limit":61
                        });
                        let read_only_direct_before = metric_counts(&ctx.client);
                        let read_only_direct = direct_call(
                            &server_with_runtime(live_runtime(ctx.client.clone(), true)),
                            read_only_arguments.clone(),
                        )
                        .await;
                        assert_stable_list_work(
                            read_only_direct_before,
                            metric_counts(&ctx.client),
                        );
                        assert_eq!(read_only_direct.is_error, Some(false));

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
                        assert_resolver_rejection(ambiguity_before, metric_counts(&ctx.client));
                        assert_eq!(
                            ambiguity
                                .structured_content
                                .as_ref()
                                .and_then(|value| value.get("code"))
                                .and_then(Value::as_str),
                            Some("ambiguous")
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
                        assert_stable_preflight_rejection(
                            auth_before,
                            metric_counts(&rejected_client),
                        );
                        assert_eq!(
                            authentication
                                .structured_content
                                .as_ref()
                                .and_then(|value| value.get("code"))
                                .and_then(Value::as_str),
                            Some("authentication")
                        );
                        for name in [COLLECTION_MEMBER_ADD, COLLECTION_MEMBER_REMOVE] {
                            let args = json!({
                                "space":ctx.space_id,
                                "collection_id":collection.id,
                                "object_id":object_c.id
                            });
                            let before = metric_counts(&rejected_client);
                            let rejected = direct_named_call(
                                &server_with_runtime(live_runtime(rejected_client.clone(), false)),
                                name,
                                args.clone(),
                            )
                            .await;
                            assert_stable_preflight_rejection(
                                before,
                                metric_counts(&rejected_client),
                            );
                            assert_eq!(
                                rejected
                                    .structured_content
                                    .as_ref()
                                    .and_then(|value| value.get("code"))
                                    .and_then(Value::as_str),
                                Some("authentication")
                            );
                        }
                        Ok(())
                    })
                },
            ))
            .await
            .expect("cleanup-safe live collection-list workflow");
            match outcome {
                DisposableRun::Completed(()) => {}
                DisposableRun::Skipped(reason) => {
                    panic!("required disposable collection-list suite was skipped: {reason:?}");
                }
            }
        });
    }
}
