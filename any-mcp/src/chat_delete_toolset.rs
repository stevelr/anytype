// any-mcp - bounded, workflow-oriented MCP server for Anytype
//
// SPDX-FileCopyrightText: 2026 Steve Schoettler
// SPDX-License-Identifier: Apache-2.0

//! Verified deletion of one exact chat message.
//!
//! The complete production `chats` registry composes this reviewed slice.

use std::{borrow::Cow, fmt, future::Future, pin::Pin, time::Instant};

use anytype::{
    chats::{ChatMessage, ChatTimestampField, canonical_chat_timestamp},
    error::AnytypeError,
    prelude::VerifyConfig,
};
use rmcp::{
    model::{CallToolRequestMethod, CallToolRequestParams, CallToolResult, ErrorData},
    schemars::{JsonSchema, Schema, SchemaGenerator, json_schema},
};
use serde::{Deserialize, Deserializer, Serialize, de};
use tokio::time::{sleep, timeout};
use tokio_util::sync::CancellationToken;

use crate::{
    discovery::DiscoveryReference,
    domain::EntityId,
    error::{ToolError, mutation_rejection_is_definitive},
    handler_support::{
        HandlerError, HandlerOperationError, MutationAccess, MutationProgress,
        execute_mutation_handler_until, require_mutation_access,
    },
    optional_toolsets::OptionalRegistryTool,
    protocol::{ToolProfile, WorkflowTool, workflow_tool},
    result::tool_error,
    runtime::{OperationContext, RuntimeContext},
    schema::SchemaContractError,
    server::decode_arguments,
    space_policy::PolicyClient,
};

/// Exact verified chat-message-delete tool name.
pub const CHAT_MESSAGE_DELETE: &str = "chat_message_delete";
/// Reviewed logical HTTP ceiling for a stable-ID delete.
pub const CHAT_DELETE_STABLE_LOGICAL_CEILING: usize = 12;
/// Reviewed physical HTTP ceiling for a stable-ID delete.
pub const CHAT_DELETE_STABLE_PHYSICAL_CEILING: usize = 67;
/// Reviewed logical HTTP ceiling for a name-resolved delete.
pub const CHAT_DELETE_RESOLVER_LOGICAL_CEILING: usize = 23;
/// Reviewed physical HTTP ceiling for a name-resolved delete.
pub const CHAT_DELETE_RESOLVER_PHYSICAL_CEILING: usize = 133;

const CONFIRM_DELETE: &str = "delete_message";

/// Canonical UTC-millisecond timestamp accepted as a delete precondition.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExpectedModifiedAt(String);

impl ExpectedModifiedAt {
    /// Validates the exact 24-byte chat read timestamp representation.
    pub fn new(value: impl Into<String>) -> Result<Self, ExpectedModifiedAtError> {
        let value = value.into();
        if !canonical_timestamp_shape(&value) {
            return Err(ExpectedModifiedAtError);
        }
        let parsed =
            chrono::DateTime::parse_from_rfc3339(&value).map_err(|_| ExpectedModifiedAtError)?;
        let canonical = canonical_chat_timestamp(parsed, ChatTimestampField::ModifiedAt)
            .map_err(|_| ExpectedModifiedAtError)?;
        if canonical != value {
            return Err(ExpectedModifiedAtError);
        }
        Ok(Self(value))
    }

    /// Borrows the exact accepted wire timestamp.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

fn canonical_timestamp_shape(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 24
        && bytes.iter().all(u8::is_ascii)
        && bytes.iter().enumerate().all(|(index, byte)| match index {
            4 | 7 => *byte == b'-',
            10 => *byte == b'T',
            13 | 16 => *byte == b':',
            19 => *byte == b'.',
            23 => *byte == b'Z',
            _ => byte.is_ascii_digit(),
        })
}

impl<'de> Deserialize<'de> for ExpectedModifiedAt {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::new(String::deserialize(deserializer)?).map_err(de::Error::custom)
    }
}

impl JsonSchema for ExpectedModifiedAt {
    fn schema_name() -> Cow<'static, str> {
        "ExpectedModifiedAt".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type":"string",
            "minLength":24,
            "maxLength":24,
            "pattern":r"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d{3}Z$"
        })
    }
}

/// Fixed validation failure for a noncanonical chat timestamp.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExpectedModifiedAtError;

impl fmt::Display for ExpectedModifiedAtError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("expected_modified_at must be a canonical UTC-millisecond timestamp")
    }
}

impl std::error::Error for ExpectedModifiedAtError {}

/// Exact destructive confirmation literal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DeleteMessageConfirmation;

impl<'de> Deserialize<'de> for DeleteMessageConfirmation {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        if value == CONFIRM_DELETE {
            Ok(Self)
        } else {
            Err(de::Error::custom(
                "invalid chat message delete confirmation",
            ))
        }
    }
}

impl JsonSchema for DeleteMessageConfirmation {
    fn schema_name() -> Cow<'static, str> {
        "DeleteMessageConfirmation".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({"type":"string","const":CONFIRM_DELETE})
    }
}

/// Strict input for deleting one exact chat message.
#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ChatMessageDeleteInput {
    /// Unique space name or identifier.
    pub space: DiscoveryReference,
    /// Exact chat identifier.
    pub chat_id: EntityId,
    /// Exact message identifier.
    pub message_id: EntityId,
    /// Canonical current timestamp copied from an exact message read.
    pub expected_modified_at: ExpectedModifiedAt,
    /// Required exact `delete_message` destructive confirmation.
    pub confirm_delete: DeleteMessageConfirmation,
}

/// Verified output for deleting one exact chat message.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ChatMessageDeleteOutput {
    /// Stable resolved space identifier.
    space_id: EntityId,
    /// Exact requested chat identifier.
    chat_id: EntityId,
    /// Exact deleted message identifier.
    message_id: EntityId,
    /// Always true after an accepted DELETE and authoritative absence read.
    deleted: bool,
    /// Exact timestamp precondition accepted by the preflight read.
    #[schemars(
        length(min = 24, max = 24),
        regex(pattern = r"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d{3}Z$")
    )]
    previous_modified_at: String,
}

/// Builds the exact chat-message-delete contract.
pub fn chat_message_delete_tool()
-> Result<WorkflowTool<ChatMessageDeleteOutput>, SchemaContractError> {
    workflow_tool::<ChatMessageDeleteInput, ChatMessageDeleteOutput>(
        CHAT_MESSAGE_DELETE,
        "Delete one exact chat message after an exact current-timestamp read and explicit confirmation. The millisecond timestamp is advisory, not an atomic revision: concurrent or same-millisecond edits can still race. DELETE is attempted once and success requires authoritative absence; an uncertain dispatch remains indeterminate even if absence is later observed.",
        ToolProfile::Update,
    )
}

/// Returns the mutation slice for terminal chats-registry composition.
pub fn chat_delete_tools() -> Result<Vec<OptionalRegistryTool>, SchemaContractError> {
    Ok(vec![OptionalRegistryTool::mutation(
        chat_message_delete_tool()?,
    )])
}

/// Transport-neutral handler for verified exact-message deletion.
#[derive(Clone)]
pub struct ChatMessageDeleteHandlers {
    contract: WorkflowTool<ChatMessageDeleteOutput>,
}

impl fmt::Debug for ChatMessageDeleteHandlers {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ChatMessageDeleteHandlers")
            .finish_non_exhaustive()
    }
}

impl ChatMessageDeleteHandlers {
    /// Creates the exact verified-delete handler.
    pub fn new() -> Result<Self, SchemaContractError> {
        Ok(Self {
            contract: chat_message_delete_tool()?,
        })
    }

    /// Dispatches the delete slice after the caller's catalog gate.
    pub async fn call_tool(
        &self,
        request: CallToolRequestParams,
        runtime: &RuntimeContext,
        cancellation: &CancellationToken,
    ) -> Result<CallToolResult, ErrorData> {
        if runtime.is_read_only() && request.name.as_ref() == CHAT_MESSAGE_DELETE {
            return Ok(tool_error(&ToolError::validation()));
        }
        match request.name.as_ref() {
            CHAT_MESSAGE_DELETE => {
                let input = decode_arguments::<ChatMessageDeleteInput>(request.arguments)?;
                Ok(self
                    .chat_message_delete(runtime, MutationAccess::Allowed, input, cancellation)
                    .await)
            }
            _ => Err(ErrorData::method_not_found::<CallToolRequestMethod>()),
        }
    }

    async fn chat_message_delete(
        &self,
        runtime: &RuntimeContext,
        access: MutationAccess,
        input: ChatMessageDeleteInput,
        cancellation: &CancellationToken,
    ) -> CallToolResult {
        if let Err(error) = require_mutation_access(access) {
            return tool_error(error.tool_error());
        }
        let deadline = runtime.request_deadline();
        let progress = MutationProgress::new();
        let client = runtime.client().clone();
        let contract = self.contract.clone();
        execute_mutation_handler_until(
            runtime,
            deadline,
            &contract,
            OperationContext::new(CHAT_MESSAGE_DELETE),
            cancellation,
            &progress,
            execute_delete(client, input, progress.clone(), deadline),
            |output| async move { Ok(output) },
        )
        .await
    }
}

type DeleteFuture<'a> = Pin<Box<dyn Future<Output = Result<(), AnytypeError>> + Send + 'a>>;
type ReadFuture<'a> = Pin<Box<dyn Future<Output = Result<ChatMessage, AnytypeError>> + Send + 'a>>;

async fn execute_delete(
    client: PolicyClient,
    input: ChatMessageDeleteInput,
    progress: MutationProgress,
    deadline: Instant,
) -> Result<ChatMessageDeleteOutput, HandlerOperationError> {
    let space_id = EntityId::new(client.resolve_space_id(input.space.as_str()).await?)
        .map_err(|_| HandlerError::new(ToolError::upstream()))?;
    let chat_id = input.chat_id;
    let message_id = input.message_id;
    let expected = input.expected_modified_at;
    let preflight_client = client.clone();
    let preflight_space = space_id.clone();
    let preflight_chat = chat_id.clone();
    let preflight_message = message_id.clone();
    let preflight: ReadFuture<'static> = Box::pin(async move {
        preflight_client
            .chats()
            .in_space(preflight_space.as_str())
            .get_message(preflight_chat.as_str(), preflight_message.as_str())
            .get()
            .await
    });
    let delete_client = client.clone();
    let delete_space = space_id.clone();
    let delete_chat = chat_id.clone();
    let delete_message = message_id.clone();
    let delete = move || -> DeleteFuture<'static> {
        Box::pin(async move {
            delete_client
                .chats()
                .in_space(delete_space.as_str())
                .delete_message(delete_chat.as_str(), delete_message.as_str())
                .await
        })
    };
    let read_client = client;
    let read_space = space_id.clone();
    let read_chat = chat_id.clone();
    let read_message = message_id.clone();
    let read = move || -> ReadFuture<'static> {
        let client = read_client.clone();
        let space = read_space.clone();
        let chat = read_chat.clone();
        let message = read_message.clone();
        Box::pin(async move {
            client
                .chats()
                .in_space(space.as_str())
                .get_message(chat.as_str(), message.as_str())
                .get()
                .await
        })
    };
    execute_delete_operation(
        preflight,
        delete,
        read,
        space_id,
        chat_id,
        message_id,
        expected,
        &progress,
        deadline,
        VerifyConfig::default(),
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn execute_delete_operation<D, R>(
    preflight: ReadFuture<'static>,
    delete: D,
    read: R,
    space_id: EntityId,
    chat_id: EntityId,
    message_id: EntityId,
    expected: ExpectedModifiedAt,
    progress: &MutationProgress,
    deadline: Instant,
    verify: VerifyConfig,
) -> Result<ChatMessageDeleteOutput, HandlerOperationError>
where
    D: FnOnce() -> DeleteFuture<'static>,
    R: FnMut() -> ReadFuture<'static>,
{
    let preflight = preflight.await?;
    validate_preflight(&preflight, &message_id, &expected)?;
    run_delete_flow(delete, read, progress, deadline, verify).await?;
    Ok(ChatMessageDeleteOutput {
        space_id,
        chat_id,
        message_id,
        deleted: true,
        previous_modified_at: expected.0,
    })
}

fn validate_preflight(
    message: &ChatMessage,
    expected_id: &EntityId,
    expected_modified_at: &ExpectedModifiedAt,
) -> Result<(), HandlerError> {
    if message.id != expected_id.as_str() {
        return Err(HandlerError::new(ToolError::upstream()));
    }
    let actual = canonical_chat_timestamp(message.modified_at, ChatTimestampField::ModifiedAt)
        .map_err(|_| HandlerError::new(ToolError::upstream()))?;
    if actual != expected_modified_at.as_str() {
        return Err(HandlerError::new(ToolError::conflict()));
    }
    Ok(())
}

async fn run_delete_flow<D, R>(
    delete: D,
    mut read: R,
    progress: &MutationProgress,
    deadline: Instant,
    verify: VerifyConfig,
) -> Result<(), HandlerOperationError>
where
    D: FnOnce() -> DeleteFuture<'static>,
    R: FnMut() -> ReadFuture<'static>,
{
    progress.mark_dispatched();
    let accepted = match delete().await {
        Ok(()) => true,
        Err(error) if mutation_rejection_is_definitive(&error) => return Err(error.into()),
        Err(_) => false,
    };

    let started = Instant::now();
    let mut attempts = 0usize;
    let mut delay = verify.initial_delay;
    loop {
        if attempts >= verify.effective_max_attempts()
            || started.elapsed() >= verify.timeout
            || Instant::now() >= deadline
        {
            return Err(indeterminate());
        }
        if !delay.is_zero() {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let verify_remaining = verify.timeout.saturating_sub(started.elapsed());
            if delay >= remaining || delay >= verify_remaining {
                return Err(indeterminate());
            }
            sleep(delay).await;
        }
        attempts = attempts.saturating_add(1);
        let request_remaining = deadline.saturating_duration_since(Instant::now());
        let verify_remaining = verify.timeout.saturating_sub(started.elapsed());
        let read_budget = request_remaining.min(verify_remaining);
        if read_budget.is_zero() {
            return Err(indeterminate());
        }
        let read_result = match timeout(read_budget, read()).await {
            Ok(result) => result,
            Err(_) => return Err(indeterminate()),
        };
        match read_result {
            Err(error) if is_authoritative_absence(&error) => {
                return if accepted {
                    Ok(())
                } else {
                    Err(indeterminate())
                };
            }
            Ok(message) if !message.id.is_empty() => {}
            Ok(_) | Err(_) => {}
        }
        delay = delay.saturating_mul(2).min(verify.max_delay);
    }
}

fn is_authoritative_absence(error: &AnytypeError) -> bool {
    matches!(
        error,
        AnytypeError::NotFound { .. } | AnytypeError::ApiError { code: 404, .. }
    )
}

fn indeterminate() -> HandlerOperationError {
    HandlerError::new(ToolError::mutation_indeterminate()).into()
}

#[cfg(test)]
mod tests {
    use std::{
        collections::{BTreeMap, VecDeque},
        io::{BufRead, Write},
        process::{Child, ChildStdin, ChildStdout, Command, Stdio},
        sync::{Arc, Mutex},
        thread,
        time::Duration,
    };

    use anytype::{
        chats::{MessageContent, MessageTextStyle},
        objects::Icon,
        prelude::{AnytypeClient, ClientConfig, HttpCredentials},
        test_util::{DisposableRun, unique_suffix, with_disposable_space_context},
    };
    use chrono::DateTime;
    use rmcp::model::ListToolsResult;
    use serde_json::{Map, Value, json};
    use sha2::{Digest, Sha256};
    use tokio::sync::Notify;

    use super::*;
    use crate::{
        config::{ApplicationProfile, ProtocolMode, RuntimeConfig},
        optional_toolsets::{
            OptionalRegistryFuture, OptionalToolsetMetadata, OptionalToolsetRegistry,
            OptionalToolsetSelection,
        },
        runtime::StartupStatus,
        server::AnyMcpServer,
    };

    const SPACE_ID: &str =
        "bafyreid5fvqlnsobih2keakcxjrrlpmly6kf37klzjzen4ibfdgalcdp4y.2tq5w93cr6oe7";
    const CHAT_ID: &str = "chat-1";
    const MESSAGE_ID: &str = "message-1";
    const MODIFIED: &str = "2026-07-22T12:00:00.002Z";
    const TOKEN_BUDGET_SNAPSHOT: &str =
        include_str!("../tests/snapshots/chats-delete-token-budget.json");

    fn input() -> Value {
        json!({
            "space":SPACE_ID,
            "chat_id":CHAT_ID,
            "message_id":MESSAGE_ID,
            "expected_modified_at":MODIFIED,
            "confirm_delete":"delete_message",
        })
    }

    fn arguments(value: Value) -> Map<String, Value> {
        value.as_object().cloned().expect("object arguments")
    }

    fn canonical(value: Value) -> Value {
        match value {
            Value::Object(map) => Value::Object(
                map.into_iter()
                    .map(|(key, value)| (key, canonical(value)))
                    .collect::<BTreeMap<_, _>>()
                    .into_iter()
                    .collect(),
            ),
            Value::Array(values) => Value::Array(values.into_iter().map(canonical).collect()),
            scalar => scalar,
        }
    }

    fn sha256(value: &str) -> String {
        Sha256::digest(value.as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    fn delete_snapshot() -> Value {
        let tokenizer = tiktoken_rs::o200k_base().expect("o200k tokenizer");
        let catalog = canonical(
            serde_json::to_value(ListToolsResult::with_all_items(vec![
                chat_message_delete_tool().unwrap().into_tool(),
            ]))
            .unwrap(),
        );
        let catalog_json = catalog.to_string();
        let output = ChatMessageDeleteOutput {
            space_id: EntityId::new(SPACE_ID).unwrap(),
            chat_id: EntityId::new(CHAT_ID).unwrap(),
            message_id: EntityId::new(MESSAGE_ID).unwrap(),
            deleted: true,
            previous_modified_at: MODIFIED.to_owned(),
        };
        let result = chat_message_delete_tool()
            .unwrap()
            .success(&output)
            .unwrap();
        let structured = canonical(result.structured_content.clone().unwrap()).to_string();
        let encoded = canonical(serde_json::to_value(result).unwrap()).to_string();
        json!({
            "tokenizer":"tiktoken o200k_base (tiktoken-rs 0.12.0)",
            "catalog_ceiling_tokens":2_000,
            "catalog":{
                "tools":[CHAT_MESSAGE_DELETE],
                "sha256":sha256(&catalog_json),
                "tokens":tokenizer.encode_with_special_tokens(&catalog_json).len(),
            },
            "maximum_result":{
                "sha256":sha256(&encoded),
                "structured_bytes":structured.len(),
                "encoded_result_tokens":tokenizer.encode_with_special_tokens(&encoded).len(),
            }
        })
    }

    #[derive(Debug)]
    struct DeleteRegistry {
        handlers: ChatMessageDeleteHandlers,
    }

    impl OptionalToolsetRegistry for DeleteRegistry {
        fn metadata(&self) -> OptionalToolsetMetadata {
            OptionalToolsetMetadata::new("chats", false)
        }

        fn tools(&self) -> Result<Vec<OptionalRegistryTool>, SchemaContractError> {
            chat_delete_tools()
        }

        fn scripted_scenario_ids(&self) -> &'static [&'static str] {
            &["chat_delete_direct", "chat_delete_stdio"]
        }

        fn headless_scenario_ids(&self) -> &'static [&'static str] {
            &["chat_delete_headless"]
        }

        fn catalog_token_ceiling(&self) -> usize {
            2_000
        }

        fn call_tool<'a>(
            &'a self,
            request: CallToolRequestParams,
            runtime: &'a RuntimeContext,
            _cursors: &'a crate::cursor::CursorStore,
            _protocol_version: &'a rmcp::model::ProtocolVersion,
            cancellation: &'a CancellationToken,
        ) -> OptionalRegistryFuture<'a, Result<CallToolResult, ErrorData>> {
            Box::pin(async move {
                self.handlers
                    .call_tool(request, runtime, cancellation)
                    .await
            })
        }
    }

    fn runtime(client: AnytypeClient, read_only: bool) -> RuntimeContext {
        let selection = OptionalToolsetSelection::parse(
            Some("chats".to_owned()),
            &[OptionalToolsetMetadata::new("chats", false)],
        )
        .unwrap();
        RuntimeContext::from_parts_with_profile_and_optional_toolsets(
            client,
            8,
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

    fn server(client: AnytypeClient, read_only: bool) -> AnyMcpServer {
        let registry = Box::leak(Box::new(DeleteRegistry {
            handlers: ChatMessageDeleteHandlers::new().unwrap(),
        }));
        let registries: &'static [&'static dyn OptionalToolsetRegistry] =
            Box::leak(vec![registry as &dyn OptionalToolsetRegistry].into_boxed_slice());
        AnyMcpServer::new_with_optional_registries(runtime(client, read_only), registries)
            .expect("chat delete test server")
    }

    fn no_io_client() -> AnytypeClient {
        let client = AnytypeClient::with_config(ClientConfig {
            base_url: Some("http://127.0.0.1:1".to_owned()),
            keystore: Some("env".to_owned()),
            keystore_service: Some("chat-delete-no-io".to_owned()),
            app_name: "chat-delete-no-io".to_owned(),
            ..ClientConfig::default()
        })
        .unwrap();
        client.set_api_key(HttpCredentials::new("unused-no-io-token"));
        client
    }

    async fn direct(server: &AnyMcpServer, value: Value) -> CallToolResult {
        Box::pin(server.dispatch_tool(
            CallToolRequestParams::new(CHAT_MESSAGE_DELETE).with_arguments(arguments(value)),
            &CancellationToken::new(),
        ))
        .await
        .expect("direct chat delete")
    }

    fn metric_counts(client: &AnytypeClient) -> (u64, u64) {
        let metrics = client.http_metrics();
        (metrics.logical_operations, metrics.physical_attempts)
    }

    fn metric_delta(before: (u64, u64), after: (u64, u64)) -> (u64, u64) {
        (
            after.0.checked_sub(before.0).expect("logical metrics grow"),
            after
                .1
                .checked_sub(before.1)
                .expect("physical metrics grow"),
        )
    }

    fn message(id: &str, modified_at: &str) -> ChatMessage {
        ChatMessage {
            id: id.to_owned(),
            order_id: "order-1".to_owned(),
            state_id: "state-1".to_owned(),
            creator: "participant-1".to_owned(),
            creator_name: None,
            created_at: DateTime::parse_from_rfc3339("2026-07-22T12:00:00.001Z").unwrap(),
            modified_at: DateTime::parse_from_rfc3339(modified_at).unwrap(),
            reply_to_message_id: None,
            content: MessageContent {
                text: "private body".to_owned(),
                style: MessageTextStyle::Paragraph,
                marks: Vec::new(),
            },
            attachments: Vec::new(),
            reactions: Vec::new(),
            read: true,
            mention_read: true,
            has_mention: false,
            synced: true,
            pinned: false,
            unread_reaction: false,
            blocks: Vec::new(),
        }
    }

    fn absent() -> AnytypeError {
        AnytypeError::ApiError {
            code: 404,
            method: "get".to_owned(),
            url: "/private/space/chat/message".to_owned(),
            message: "private response".to_owned(),
        }
    }

    fn immediate_verify() -> VerifyConfig {
        VerifyConfig {
            timeout: std::time::Duration::from_secs(1),
            initial_delay: std::time::Duration::ZERO,
            max_delay: std::time::Duration::ZERO,
            max_attempts: 10,
        }
    }

    async fn execute_test_operation<D, R>(
        preflight: Result<ChatMessage, AnytypeError>,
        delete: D,
        read: R,
        cancellation: &CancellationToken,
        progress: &MutationProgress,
        deadline: Instant,
        verify: VerifyConfig,
    ) -> CallToolResult
    where
        D: FnOnce() -> DeleteFuture<'static>,
        R: FnMut() -> ReadFuture<'static>,
    {
        let runtime = runtime(no_io_client(), false);
        let contract = chat_message_delete_tool().unwrap();
        execute_mutation_handler_until(
            &runtime,
            deadline,
            &contract,
            OperationContext::new(CHAT_MESSAGE_DELETE),
            cancellation,
            progress,
            execute_delete_operation(
                Box::pin(async move { preflight }),
                delete,
                read,
                EntityId::new(SPACE_ID).unwrap(),
                EntityId::new(CHAT_ID).unwrap(),
                EntityId::new(MESSAGE_ID).unwrap(),
                ExpectedModifiedAt::new(MODIFIED).unwrap(),
                progress,
                deadline,
                verify,
            ),
            |output| async move { Ok(output) },
        )
        .await
    }

    fn assert_indeterminate(result: &CallToolResult) {
        assert_eq!(result.is_error, Some(true));
        assert_eq!(
            result.structured_content.as_ref().unwrap()["code"],
            "conflict"
        );
        assert_eq!(
            result.structured_content.as_ref().unwrap()["message"],
            "The mutation may have applied. Reread the object before retrying to avoid applying it twice."
        );
    }

    #[test]
    fn timestamp_runtime_and_schema_are_exact() {
        assert_eq!(
            ExpectedModifiedAt::new(MODIFIED).unwrap().as_str(),
            MODIFIED
        );
        for rejected in [
            "2026-07-22T12:00:00Z",
            "2026-07-22T12:00:00.02Z",
            "2026-07-22T12:00:00.002+00:00",
            "2026-07-22t12:00:00.002z",
            "2026-02-30T12:00:00.002Z",
            "0000-01-01T00:00:00.000Z",
        ] {
            assert!(ExpectedModifiedAt::new(rejected).is_err(), "{rejected}");
        }
        let contract = chat_message_delete_tool().unwrap();
        let schema = serde_json::to_value(contract.as_tool().input_schema.as_ref()).unwrap();
        assert_eq!(schema["additionalProperties"], false);
        assert_eq!(schema["required"].as_array().unwrap().len(), 5);
        let confirm_ref = schema["properties"]["confirm_delete"]["$ref"]
            .as_str()
            .unwrap();
        let confirm = &schema["$defs"][confirm_ref.trim_start_matches("#/$defs/")];
        assert_eq!(confirm["const"], CONFIRM_DELETE);
    }

    #[test]
    fn strict_input_rejects_null_unknown_and_malformed_values() {
        for value in [
            json!({}),
            json!({"space":SPACE_ID,"chat_id":CHAT_ID,"message_id":MESSAGE_ID,"expected_modified_at":MODIFIED,"confirm_delete":null}),
            json!({"space":SPACE_ID,"chat_id":CHAT_ID,"message_id":MESSAGE_ID,"expected_modified_at":MODIFIED,"confirm_delete":"delete"}),
            json!({"space":SPACE_ID,"chat_id":CHAT_ID,"message_id":MESSAGE_ID,"expected_modified_at":"2026-07-22T12:00:00Z","confirm_delete":"delete_message"}),
            json!({"space":SPACE_ID,"chat_id":CHAT_ID,"message_id":MESSAGE_ID,"expected_modified_at":MODIFIED,"confirm_delete":"delete_message","unknown":true}),
        ] {
            assert!(serde_json::from_value::<ChatMessageDeleteInput>(value).is_err());
        }
        assert!(serde_json::from_value::<ChatMessageDeleteInput>(input()).is_ok());
    }

    #[test]
    fn preflight_binds_exact_identity_and_timestamp() {
        let expected_id = EntityId::new(MESSAGE_ID).unwrap();
        let expected_at = ExpectedModifiedAt::new(MODIFIED).unwrap();
        assert!(
            validate_preflight(&message(MESSAGE_ID, MODIFIED), &expected_id, &expected_at).is_ok()
        );
        assert!(
            validate_preflight(&message("other", MODIFIED), &expected_id, &expected_at).is_err()
        );
        assert!(
            validate_preflight(
                &message(MESSAGE_ID, "2026-07-22T12:00:00.003Z"),
                &expected_id,
                &expected_at
            )
            .is_err()
        );
    }

    #[tokio::test]
    async fn accepted_delete_requires_authoritative_absence_and_dispatches_once() {
        let deletes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let delete_count = deletes.clone();
        let reads = Arc::new(Mutex::new(VecDeque::from([
            Ok(message(MESSAGE_ID, MODIFIED)),
            Err(absent()),
        ])));
        let read_results = reads.clone();
        let progress = MutationProgress::new();
        let result = run_delete_flow(
            move || {
                delete_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Box::pin(async { Ok(()) })
            },
            move || {
                let next = read_results.lock().unwrap().pop_front().unwrap();
                Box::pin(async move { next })
            },
            &progress,
            Instant::now() + std::time::Duration::from_secs(1),
            immediate_verify(),
        )
        .await;
        assert!(result.is_ok());
        assert_eq!(deletes.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert!(reads.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn uncertain_delete_is_indeterminate_even_when_absence_is_observed() {
        let progress = MutationProgress::new();
        let result = run_delete_flow(
            || {
                Box::pin(async {
                    Err(AnytypeError::Other {
                        message: "private".to_owned(),
                    })
                })
            },
            || Box::pin(async { Err(absent()) }),
            &progress,
            Instant::now() + std::time::Duration::from_secs(1),
            immediate_verify(),
        )
        .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn definitive_rejection_returns_without_verification() {
        let reads = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let read_count = reads.clone();
        let progress = MutationProgress::new();
        let result = run_delete_flow(
            || Box::pin(async { Err(AnytypeError::Forbidden) }),
            move || {
                read_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Box::pin(async { Err(absent()) })
            },
            &progress,
            Instant::now() + std::time::Duration::from_secs(1),
            immediate_verify(),
        )
        .await;
        assert!(result.is_err());
        assert_eq!(reads.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn bounded_verification_never_redispatches_delete() {
        let deletes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let delete_count = deletes.clone();
        let reads = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let read_count = reads.clone();
        let progress = MutationProgress::new();
        let mut verify = immediate_verify();
        verify.max_attempts = 3;
        let result = run_delete_flow(
            move || {
                delete_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Box::pin(async { Ok(()) })
            },
            move || {
                read_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Box::pin(async { Ok(message(MESSAGE_ID, MODIFIED)) })
            },
            &progress,
            Instant::now() + std::time::Duration::from_secs(1),
            verify,
        )
        .await;
        assert!(result.is_err());
        assert_eq!(deletes.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(reads.load(std::sync::atomic::Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn handler_maps_preflight_not_found_and_definitive_delete_permission() {
        let cancellation = CancellationToken::new();
        let progress = MutationProgress::new();
        let not_found = execute_test_operation(
            Err(AnytypeError::NotFound {
                obj_type: "chat_message".to_owned(),
                key: "private-message-id".to_owned(),
            }),
            || Box::pin(async { Ok(()) }),
            || Box::pin(async { Err(absent()) }),
            &cancellation,
            &progress,
            Instant::now() + Duration::from_secs(1),
            immediate_verify(),
        )
        .await;
        assert_eq!(not_found.is_error, Some(true));
        assert_eq!(
            not_found.structured_content.as_ref().unwrap()["code"],
            "not_found"
        );
        assert_eq!(
            progress.stage(),
            crate::handler_support::MutationStage::PreDispatch
        );

        let progress = MutationProgress::new();
        let reads = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let read_count = reads.clone();
        let forbidden = execute_test_operation(
            Ok(message(MESSAGE_ID, MODIFIED)),
            || Box::pin(async { Err(AnytypeError::Forbidden) }),
            move || {
                read_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Box::pin(async { Err(absent()) })
            },
            &cancellation,
            &progress,
            Instant::now() + Duration::from_secs(1),
            immediate_verify(),
        )
        .await;
        assert_eq!(forbidden.is_error, Some(true));
        assert_eq!(
            forbidden.structured_content.as_ref().unwrap()["code"],
            "authentication"
        );
        assert_eq!(reads.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn handler_post_dispatch_delete_cancellation_is_indeterminate() {
        let entered = Arc::new(Notify::new());
        let delete_entered = entered.clone();
        let cancellation = CancellationToken::new();
        let task_cancellation = cancellation.clone();
        let progress = MutationProgress::new();
        let task_progress = progress.clone();
        let task = tokio::spawn(async move {
            execute_test_operation(
                Ok(message(MESSAGE_ID, MODIFIED)),
                move || {
                    Box::pin(async move {
                        delete_entered.notify_one();
                        std::future::pending().await
                    })
                },
                || Box::pin(async { Err(absent()) }),
                &task_cancellation,
                &task_progress,
                Instant::now() + Duration::from_secs(2),
                immediate_verify(),
            )
            .await
        });
        entered.notified().await;
        cancellation.cancel();
        let result = task.await.expect("delete cancellation task");
        assert_indeterminate(&result);
        assert_eq!(
            progress.stage(),
            crate::handler_support::MutationStage::Dispatched
        );
    }

    #[tokio::test(start_paused = true)]
    async fn handler_delete_deadline_is_post_dispatch_indeterminate() {
        let progress = MutationProgress::new();
        let deadline = Instant::now() + Duration::from_millis(100);
        let result = execute_test_operation(
            Ok(message(MESSAGE_ID, MODIFIED)),
            || Box::pin(std::future::pending()),
            || Box::pin(async { Err(absent()) }),
            &CancellationToken::new(),
            &progress,
            deadline,
            immediate_verify(),
        )
        .await;
        assert_indeterminate(&result);
        assert_eq!(
            progress.stage(),
            crate::handler_support::MutationStage::Dispatched
        );
    }

    #[tokio::test(start_paused = true)]
    async fn handler_verification_cancellation_and_deadline_are_indeterminate() {
        let entered = Arc::new(Notify::new());
        let read_entered = entered.clone();
        let cancellation = CancellationToken::new();
        let task_cancellation = cancellation.clone();
        let progress = MutationProgress::new();
        let task_progress = progress.clone();
        let task = tokio::spawn(async move {
            execute_test_operation(
                Ok(message(MESSAGE_ID, MODIFIED)),
                || Box::pin(async { Ok(()) }),
                move || {
                    let read_entered = read_entered.clone();
                    Box::pin(async move {
                        read_entered.notify_one();
                        std::future::pending().await
                    })
                },
                &task_cancellation,
                &task_progress,
                Instant::now() + Duration::from_secs(2),
                immediate_verify(),
            )
            .await
        });
        entered.notified().await;
        cancellation.cancel();
        let cancelled = task.await.expect("verification cancellation task");
        assert_indeterminate(&cancelled);
        assert_eq!(
            progress.stage(),
            crate::handler_support::MutationStage::Dispatched
        );

        let progress = MutationProgress::new();
        let deadline = Instant::now() + Duration::from_millis(100);
        let timed_out = execute_test_operation(
            Ok(message(MESSAGE_ID, MODIFIED)),
            || Box::pin(async { Ok(()) }),
            || Box::pin(std::future::pending()),
            &CancellationToken::new(),
            &progress,
            deadline,
            immediate_verify(),
        )
        .await;
        assert_indeterminate(&timed_out);
        assert_eq!(
            progress.stage(),
            crate::handler_support::MutationStage::Dispatched
        );
    }

    #[tokio::test]
    async fn each_verification_read_is_wrapped_by_remaining_verify_budget() {
        let entered = Arc::new(Notify::new());
        let read_entered = entered.clone();
        let progress = MutationProgress::new();
        let mut verify = immediate_verify();
        verify.timeout = Duration::from_millis(50);
        let result = run_delete_flow(
            || Box::pin(async { Ok(()) }),
            move || {
                let read_entered = read_entered.clone();
                Box::pin(async move {
                    read_entered.notify_one();
                    std::future::pending().await
                })
            },
            &progress,
            Instant::now() + Duration::from_secs(1),
            verify,
        )
        .await;
        entered.notified().await;
        assert!(result.is_err());
        assert_eq!(
            progress.stage(),
            crate::handler_support::MutationStage::Dispatched
        );
    }

    #[test]
    fn aggregate_delete_work_ceilings_include_resolver_and_one_delete_attempt() {
        const SAFE_GET_PHYSICAL_CEILING: usize = 6;
        const MAX_RESOLVER_GETS: usize = 11;
        const MAX_VERIFY_GETS: usize = 10;
        const ONE_DELETE: usize = 1;
        assert_eq!(
            CHAT_DELETE_STABLE_LOGICAL_CEILING,
            1 + ONE_DELETE + MAX_VERIFY_GETS
        );
        assert_eq!(
            CHAT_DELETE_STABLE_PHYSICAL_CEILING,
            SAFE_GET_PHYSICAL_CEILING * (1 + MAX_VERIFY_GETS) + ONE_DELETE
        );
        assert_eq!(
            CHAT_DELETE_RESOLVER_LOGICAL_CEILING,
            MAX_RESOLVER_GETS + 1 + ONE_DELETE + MAX_VERIFY_GETS
        );
        assert_eq!(
            CHAT_DELETE_RESOLVER_PHYSICAL_CEILING,
            SAFE_GET_PHYSICAL_CEILING * (MAX_RESOLVER_GETS + 1 + MAX_VERIFY_GETS) + ONE_DELETE
        );
    }

    #[tokio::test]
    async fn direct_router_rejects_invalid_and_read_only_calls_without_http() {
        let client = no_io_client();
        let before = client.http_metrics();
        let mut invalid = input();
        invalid["confirm_delete"] = Value::Null;
        let invalid = Box::pin(server(client.clone(), false).dispatch_tool(
            CallToolRequestParams::new(CHAT_MESSAGE_DELETE).with_arguments(arguments(invalid)),
            &CancellationToken::new(),
        ))
        .await;
        assert!(invalid.is_err());

        let read_only = direct(&server(client.clone(), true), input()).await;
        assert_eq!(read_only.is_error, Some(true));
        assert_eq!(read_only.structured_content.unwrap()["code"], "validation");
        let after = client.http_metrics();
        assert_eq!(after.logical_operations, before.logical_operations);
        assert_eq!(after.physical_attempts, before.physical_attempts);
    }

    #[tokio::test]
    async fn direct_router_precancellation_is_predispatch_and_performs_no_http() {
        let client = no_io_client();
        let before = client.http_metrics();
        let cancelled = CancellationToken::new();
        cancelled.cancel();
        let result = Box::pin(server(client.clone(), false).dispatch_tool(
            CallToolRequestParams::new(CHAT_MESSAGE_DELETE).with_arguments(arguments(input())),
            &cancelled,
        ))
        .await
        .expect("pre-cancelled direct result");
        assert_eq!(result.is_error, Some(true));
        assert_eq!(result.structured_content.unwrap()["code"], "upstream");
        let after = client.http_metrics();
        assert_eq!(after.logical_operations, before.logical_operations);
        assert_eq!(after.physical_attempts, before.physical_attempts);
    }

    struct SpawnedStdioSession {
        child: Child,
        stdin: Option<ChildStdin>,
        stdout: std::io::BufReader<ChildStdout>,
        next_id: u64,
    }

    impl SpawnedStdioSession {
        fn start() -> Self {
            let mut child = Command::new(std::env::current_exe().expect("current test executable"))
                .arg("chat_delete_toolset::tests::spawned_real_server_stdio_entrypoint")
                .arg("--exact")
                .arg("--nocapture")
                .arg("--test-threads=1")
                .env("ANY_MCP_CHAT_DELETE_PROCESS_CHILD", "1")
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::inherit())
                .spawn()
                .expect("spawn test-owned MCP process");
            let stdin = child.stdin.take().expect("child stdin");
            let stdout = child.stdout.take().expect("child stdout");
            Self {
                child,
                stdin: Some(stdin),
                stdout: std::io::BufReader::new(stdout),
                next_id: 1,
            }
        }

        fn request(&mut self, method: &str, params: Value) -> Value {
            let id = self.next_id;
            self.next_id = self.next_id.checked_add(1).expect("small request ID");
            let frame = json!({"jsonrpc":"2.0","id":id,"method":method,"params":params});
            let stdin = self.stdin.as_mut().expect("open child stdin");
            writeln!(stdin, "{frame}").expect("write child protocol frame");
            stdin.flush().expect("flush child protocol frame");
            loop {
                let mut line = String::new();
                let count = self
                    .stdout
                    .read_line(&mut line)
                    .expect("read child protocol frame");
                assert_ne!(count, 0, "child exited before response {id}");
                let Some(start) = line.find('{') else {
                    continue;
                };
                let Ok(response) = serde_json::from_str::<Value>(&line[start..]) else {
                    continue;
                };
                if response.get("id") == Some(&json!(id)) {
                    return response;
                }
            }
        }

        fn call(&mut self, value: Value) -> Value {
            self.request(
                "tools/call",
                json!({
                    "name":CHAT_MESSAGE_DELETE,
                    "arguments":value,
                    "_meta":{
                        "io.modelcontextprotocol/protocolVersion":"2026-07-28",
                        "io.modelcontextprotocol/clientInfo":{"name":"spawned-chat-delete-test","version":"1"},
                        "io.modelcontextprotocol/clientCapabilities":{}
                    }
                }),
            )
        }

        fn finish(mut self) {
            drop(self.stdin.take());
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            loop {
                match self.child.try_wait().expect("poll child process") {
                    Some(status) => {
                        assert!(status.success(), "child process failed: {status}");
                        return;
                    }
                    None if std::time::Instant::now() < deadline => {
                        thread::sleep(Duration::from_millis(10));
                    }
                    None => {
                        self.child.kill().expect("terminate stuck child process");
                        let _ = self.child.wait();
                        panic!("child process did not stop after stdin closed");
                    }
                }
            }
        }
    }

    #[test]
    fn spawned_real_server_stdio_entrypoint() {
        if std::env::var_os("ANY_MCP_CHAT_DELETE_PROCESS_CHILD").is_none() {
            return;
        }
        std::thread::Builder::new()
            .name("chat-delete-stdio-child".to_owned())
            .stack_size(16 * 1024 * 1024)
            .spawn(|| {
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("spawned stdio runtime")
                    .block_on(async {
                        let config = RuntimeConfig::from_env().expect("child runtime config");
                        let started = RuntimeContext::start(&config)
                            .await
                            .expect("child authenticated startup");
                        crate::stdio::serve_stdio(
                            server(started.client().raw_clone(), false),
                            ProtocolMode::Experimental20260728,
                        )
                        .await
                        .expect("spawned stdio server");
                    });
            })
            .expect("spawn chat-delete stdio child thread")
            .join()
            .expect("join chat-delete stdio child thread");
    }

    #[test]
    #[ignore = "requires env-only disposable credentials and an authenticated headless Anytype server"]
    #[serial_test::serial(disposable_anytype_api)]
    fn headless_direct_and_spawned_stdio_delete_conflict_and_absence() {
        std::thread::Builder::new()
            .name("chat-delete-live".to_owned())
            .stack_size(16 * 1024 * 1024)
            .spawn(|| {
                tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(4)
                    .thread_stack_size(16 * 1024 * 1024)
                    .enable_all()
                    .build()
                    .expect("chat delete live runtime")
                    .block_on(async {
                        let outcome = with_disposable_space_context("any-mcp-chat-delete", |ctx| {
                            Box::pin(async move {
                                ctx.client.ping_http().await.expect("authenticated HTTP");
                                let suffix = unique_suffix();
                                let chat = ctx
                                    .client
                                    .chats()
                                    .in_space(&ctx.space_id)
                                    .create(
                                        format!("mcp-chat-delete-{suffix}"),
                                        Icon::Emoji {
                                            emoji: "🗑️".to_owned(),
                                        },
                                    )
                                    .create()
                                    .await
                                    .expect("create disposable chat");
                                ctx.register_object(&chat.id);
                                let chats = ctx.client.chats().in_space(&ctx.space_id);

                                let direct_id = chats
                                    .add_message(
                                        &chat.id,
                                        MessageContent::new().text(format!("direct {suffix}")),
                                    )
                                    .send()
                                    .await
                                    .expect("create direct message");
                                ctx.register_chat_message(&chat.id, &direct_id)?;
                                let before = chats
                                    .get_message(&chat.id, &direct_id)
                                    .get()
                                    .await
                                    .expect("read direct sentinel");
                                let evidence = chats
                                    .edit_message(
                                        &chat.id,
                                        &direct_id,
                                        MessageContent::new().text(format!("edited {suffix}")),
                                    )
                                    .send_verified()
                                    .await
                                    .expect("edit advances timestamp");
                                assert!(evidence.after.modified_at > before.modified_at);
                                let stale = canonical_chat_timestamp(
                                    before.modified_at,
                                    ChatTimestampField::ModifiedAt,
                                )?;
                                let current = canonical_chat_timestamp(
                                    evidence.after.modified_at,
                                    ChatTimestampField::ModifiedAt,
                                )?;
                                let direct_server = server(ctx.client.clone(), false);
                                let before_stale = metric_counts(&ctx.client);
                                let stale_result = direct(
                                    &direct_server,
                                    json!({
                                        "space":ctx.space_id,
                                        "chat_id":chat.id,
                                        "message_id":direct_id,
                                        "expected_modified_at":stale,
                                        "confirm_delete":"delete_message",
                                    }),
                                )
                                .await;
                                assert_eq!(stale_result.is_error, Some(true));
                                assert_eq!(
                                    stale_result.structured_content.unwrap()["code"],
                                    "conflict"
                                );
                                assert_eq!(
                                    metric_delta(before_stale, metric_counts(&ctx.client)),
                                    (1, 1)
                                );
                                let before_delete = metric_counts(&ctx.client);
                                let deleted = direct(
                                    &direct_server,
                                    json!({
                                        "space":ctx.space_id,
                                        "chat_id":chat.id,
                                        "message_id":direct_id,
                                        "expected_modified_at":current,
                                        "confirm_delete":"delete_message",
                                    }),
                                )
                                .await;
                                assert_eq!(deleted.is_error, Some(false), "{deleted:?}");
                                assert_eq!(deleted.structured_content.unwrap()["deleted"], true);
                                // One preflight GET, exactly one physical DELETE, and one
                                // authoritative-absence GET. Stable-ID resolution adds no I/O.
                                assert_eq!(
                                    metric_delta(before_delete, metric_counts(&ctx.client)),
                                    (3, 3)
                                );
                                assert!(matches!(
                                    chats.get_message(&chat.id, &direct_id).get().await,
                                    Err(AnytypeError::ApiError { code: 404, .. })
                                        | Err(AnytypeError::NotFound { .. })
                                ));

                                let stdio_id = chats
                                    .add_message(
                                        &chat.id,
                                        MessageContent::new().text(format!("stdio {suffix}")),
                                    )
                                    .send()
                                    .await
                                    .expect("create stdio message");
                                ctx.register_chat_message(&chat.id, &stdio_id)?;
                                let stdio_message = chats
                                    .get_message(&chat.id, &stdio_id)
                                    .get()
                                    .await
                                    .expect("read stdio sentinel");
                                let stdio_modified = canonical_chat_timestamp(
                                    stdio_message.modified_at,
                                    ChatTimestampField::ModifiedAt,
                                )?;
                                let mut stdio = SpawnedStdioSession::start();
                                let response = stdio.call(json!({
                                    "space":ctx.space_id,
                                    "chat_id":chat.id,
                                    "message_id":stdio_id,
                                    "expected_modified_at":stdio_modified,
                                    "confirm_delete":"delete_message",
                                }));
                                assert_eq!(response["result"]["isError"], false, "{response}");
                                assert_eq!(
                                    response["result"]["structuredContent"]["deleted"],
                                    true
                                );
                                stdio.finish();
                                assert!(matches!(
                                    chats.get_message(&chat.id, &stdio_id).get().await,
                                    Err(AnytypeError::ApiError { code: 404, .. })
                                        | Err(AnytypeError::NotFound { .. })
                                ));
                                Ok(())
                            })
                        })
                        .await
                        .expect("disposable chat-delete harness");
                        assert_eq!(outcome, DisposableRun::Completed(()));
                    });
            })
            .expect("spawn chat delete live test")
            .join()
            .expect("join chat delete live test");
    }

    #[test]
    fn output_and_description_expose_only_reviewed_delete_fields() {
        let contract = chat_message_delete_tool().unwrap();
        let output = ChatMessageDeleteOutput {
            space_id: EntityId::new(SPACE_ID).unwrap(),
            chat_id: EntityId::new(CHAT_ID).unwrap(),
            message_id: EntityId::new(MESSAGE_ID).unwrap(),
            deleted: true,
            previous_modified_at: MODIFIED.to_owned(),
        };
        let value = serde_json::to_value(contract.success(&output).unwrap()).unwrap();
        let text = value.to_string();
        assert!(!text.contains("private body"));
        assert_eq!(value["structuredContent"]["deleted"], true);
        let tool = serde_json::to_value(contract.as_tool())
            .unwrap()
            .to_string();
        for forbidden in [
            "reaction",
            "attachment_delete",
            "chat_delete",
            "reply_subtree",
        ] {
            assert!(!tool.contains(forbidden), "forbidden surface: {forbidden}");
        }
        let actual = canonical(delete_snapshot());
        let reviewed = canonical(
            serde_json::from_str(TOKEN_BUDGET_SNAPSHOT).expect("chat-delete snapshot JSON"),
        );
        assert_eq!(
            actual, reviewed,
            "chat-delete catalog/result snapshot drifted"
        );
        assert!(
            actual["catalog"]["tokens"].as_u64().unwrap()
                <= actual["catalog_ceiling_tokens"].as_u64().unwrap()
        );
    }
}
