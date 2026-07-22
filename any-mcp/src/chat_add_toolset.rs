// any-mcp - bounded, workflow-oriented MCP server for Anytype
//
// SPDX-FileCopyrightText: 2026 Steve Schoettler
// SPDX-License-Identifier: Apache-2.0

//! Verified, process-idempotent plain chat-message creation.
//!
//! The complete production `chats` registry composes this reviewed slice.

use std::{
    borrow::Cow,
    collections::HashMap,
    fmt,
    sync::{
        Arc, Mutex as StdMutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Instant,
};

use anytype::{
    chats::{ChatMessage, MessageContent, MessageTextStyle},
    error::AnytypeError,
};
use rmcp::{
    model::{
        CallToolRequestMethod, CallToolRequestParams, CallToolResult, ContentBlock, ErrorData,
    },
    schemars::{JsonSchema, Schema, SchemaGenerator, json_schema},
};
use serde::{Deserialize, Deserializer, Serialize, de};
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;

#[cfg(test)]
use tokio::sync::Semaphore;

use crate::{
    chat_read_toolset::{
        MessageDetail, bounded_output, convert_message_detail, validate_message_reference,
    },
    create_idempotency::{
        Attempt, BeginAttempt, CreateDisposition, CreateExecution, DEFAULT_IDEMPOTENCY_CAPACITY,
        IdempotencyKey, IdempotencyStore, wait_for_attempt_until,
    },
    discovery::DiscoveryReference,
    domain::EntityId,
    error::{AnytypeErrorMapping, ToolError, mutation_rejection_is_definitive},
    handler_support::{
        HandlerError, HandlerOperationError, MutationAccess, MutationProgress, MutationStage,
        execute_mutation_handler_until, execute_prepared_handler_until, require_mutation_access,
    },
    optional_toolsets::OptionalRegistryTool,
    protocol::{ToolProfile, WorkflowTool, workflow_tool},
    result::tool_error,
    runtime::{
        ControlledOperationError, OperationContext, OperationFailureDiagnostic, RuntimeContext,
    },
    schema::SchemaContractError,
    server::decode_arguments,
    validation::{Omittable, optional_non_null_schema},
};

/// Exact chat-message creation tool name.
pub const CHAT_MESSAGE_ADD: &str = "chat_message_add";
/// Reviewed logical HTTP ceiling for a stable-ID add without a reply.
pub const CHAT_ADD_STABLE_LOGICAL_CEILING: usize = 2;
/// Reviewed physical HTTP ceiling for a stable-ID add without a reply.
pub const CHAT_ADD_STABLE_PHYSICAL_CEILING: usize = 7;
/// Reviewed logical HTTP ceiling for a stable-ID add with a reply.
pub const CHAT_ADD_REPLY_STABLE_LOGICAL_CEILING: usize = 3;
/// Reviewed physical HTTP ceiling for a stable-ID add with a reply.
pub const CHAT_ADD_REPLY_STABLE_PHYSICAL_CEILING: usize = 13;

const MAX_MESSAGE_TEXT_CHARS: usize = 8_192;
const MESSAGE_RESULT_BYTES: usize = 48 * 1024;
const CHAT_ADD_FINGERPRINT_DOMAIN: &str = "any-mcp/chat-message-add/v1";

/// Bounded non-whitespace plain paragraph text for one new message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct ChatMessageText(String);

impl ChatMessageText {
    /// Validates exact text without trimming or normalization.
    pub fn new(value: impl Into<String>) -> Result<Self, ChatMessageTextError> {
        let value = value.into();
        let count = value.chars().count();
        if !(1..=MAX_MESSAGE_TEXT_CHARS).contains(&count) {
            return Err(ChatMessageTextError::Length);
        }
        if value.trim().is_empty() {
            return Err(ChatMessageTextError::WhitespaceOnly);
        }
        if value
            .chars()
            .any(|character| character.is_control() && !matches!(character, '\t' | '\n'))
        {
            return Err(ChatMessageTextError::Control);
        }
        Ok(Self(value))
    }

    /// Borrows the exact validated text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for ChatMessageText {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::new(String::deserialize(deserializer)?).map_err(de::Error::custom)
    }
}

impl JsonSchema for ChatMessageText {
    fn schema_name() -> Cow<'static, str> {
        "ChatMessageText".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type":"string",
            "minLength":1,
            "maxLength":MAX_MESSAGE_TEXT_CHARS
        })
    }
}

/// Stable input-validation failure for message text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatMessageTextError {
    /// The Unicode-scalar length was outside 1 through 8,192.
    Length,
    /// The text contained no non-whitespace scalar.
    WhitespaceOnly,
    /// The text contained a control other than tab or newline.
    Control,
}

impl fmt::Display for ChatMessageTextError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Length => "message text length is outside the supported range",
            Self::WhitespaceOnly => "message text must contain non-whitespace content",
            Self::Control => "message text contains an unsupported control character",
        })
    }
}

impl std::error::Error for ChatMessageTextError {}

/// Strict input for one verified plain-message creation.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ChatMessageAddInput {
    /// Unique space name or identifier.
    pub space: DiscoveryReference,
    /// Exact chat identifier.
    pub chat_id: EntityId,
    /// Exact plain paragraph text.
    pub text: ChatMessageText,
    /// Optional exact reply target in the same chat.
    #[serde(default)]
    #[schemars(schema_with = "optional_entity_id_schema")]
    pub reply_to_message_id: Omittable<EntityId>,
    /// Required process-local duplicate-control key.
    pub idempotency_key: IdempotencyKey,
}

fn optional_entity_id_schema(generator: &mut SchemaGenerator) -> Schema {
    optional_non_null_schema::<EntityId>(generator)
}

/// Process-local duplicate-control result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ChatAddIdempotency {
    /// Whether this call reused a running or retained key.
    key_reused: bool,
    /// Fixed lifetime of this duplicate-control guarantee.
    scope: ProcessScope,
}

/// The exact process-local idempotency scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcessScope;

impl Serialize for ProcessScope {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str("process")
    }
}

impl JsonSchema for ProcessScope {
    fn schema_name() -> Cow<'static, str> {
        "ProcessScope".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({"type":"string","const":"process"})
    }
}

/// Verified output for one new or retained chat message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ChatMessageAddOutput {
    /// Stable resolved space identifier.
    space_id: EntityId,
    /// Exact requested chat identifier.
    chat_id: EntityId,
    /// Fresh or leader-verified exact message detail.
    message: MessageDetail,
    /// Process-local duplicate-control metadata.
    idempotency: ChatAddIdempotency,
}

/// Builds the exact chat-message-add contract.
pub fn chat_message_add_tool() -> Result<WorkflowTool<ChatMessageAddOutput>, SchemaContractError> {
    workflow_tool::<ChatMessageAddInput, ChatMessageAddOutput>(
        CHAT_MESSAGE_ADD,
        "Send one bounded plain paragraph chat message and verify its exact server-assigned ID. The required retry key prevents another POST for the same normalized request during this process; once Anytype assigns an ID, every retry performs only a fresh exact read.",
        ToolProfile::Create,
    )
}

/// Returns the mutation slice for terminal chats-registry composition.
pub fn chat_add_tools() -> Result<Vec<OptionalRegistryTool>, SchemaContractError> {
    Ok(vec![OptionalRegistryTool::mutation(
        chat_message_add_tool()?
    )])
}

#[derive(Clone)]
struct CandidateRecord {
    space_id: EntityId,
    chat_id: EntityId,
    message_id: EntityId,
}

fn retain_candidate(
    candidates: &StdMutex<HashMap<IdempotencyKey, CandidateRecord>>,
    key: IdempotencyKey,
    candidate: CandidateRecord,
) {
    match candidates.lock() {
        Ok(mut candidates) => {
            candidates.insert(key, candidate);
        }
        Err(poisoned) => {
            poisoned.into_inner().insert(key, candidate);
        }
    }
}

fn retained_candidate(
    candidates: &StdMutex<HashMap<IdempotencyKey, CandidateRecord>>,
    key: &IdempotencyKey,
) -> Option<CandidateRecord> {
    match candidates.lock() {
        Ok(candidates) => candidates.get(key).cloned(),
        Err(poisoned) => poisoned.into_inner().get(key).cloned(),
    }
}

#[cfg(test)]
#[derive(Debug)]
struct CohortGate {
    leader_seen: AtomicBool,
    waiter_seen: AtomicBool,
    leader_admitted: Semaphore,
    waiter_admitted: Semaphore,
    release_leader: Semaphore,
}

#[cfg(test)]
impl CohortGate {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            leader_seen: AtomicBool::new(false),
            waiter_seen: AtomicBool::new(false),
            leader_admitted: Semaphore::new(0),
            waiter_admitted: Semaphore::new(0),
            release_leader: Semaphore::new(0),
        })
    }
}

/// Stateful transport-neutral handler for verified plain-message creation.
#[derive(Clone)]
pub struct ChatMessageAddHandlers {
    idempotency: Arc<IdempotencyStore>,
    candidates: Arc<StdMutex<HashMap<IdempotencyKey, CandidateRecord>>>,
    contract: WorkflowTool<ChatMessageAddOutput>,
    #[cfg(test)]
    cohort_gate: Option<Arc<CohortGate>>,
}

impl fmt::Debug for ChatMessageAddHandlers {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ChatMessageAddHandlers")
            .finish_non_exhaustive()
    }
}

impl ChatMessageAddHandlers {
    /// Creates a handler with the reviewed finite idempotency capacity.
    pub fn new() -> Result<Self, SchemaContractError> {
        Self::build(DEFAULT_IDEMPOTENCY_CAPACITY)
    }

    fn build(capacity: usize) -> Result<Self, SchemaContractError> {
        Ok(Self {
            idempotency: Arc::new(IdempotencyStore::new(capacity)),
            candidates: Arc::new(StdMutex::new(HashMap::new())),
            contract: chat_message_add_tool()?,
            #[cfg(test)]
            cohort_gate: None,
        })
    }

    #[cfg(test)]
    fn with_capacity(capacity: usize) -> Result<Self, SchemaContractError> {
        Self::build(capacity)
    }

    #[cfg(test)]
    fn with_cohort_gate(mut self, gate: Arc<CohortGate>) -> Self {
        self.cohort_gate = Some(gate);
        self
    }

    fn candidate(&self, key: &IdempotencyKey) -> Option<CandidateRecord> {
        retained_candidate(self.candidates.as_ref(), key)
    }

    /// Dispatches the add slice after the caller's catalog gate.
    pub async fn call_tool(
        &self,
        request: CallToolRequestParams,
        runtime: &RuntimeContext,
        cancellation: &CancellationToken,
    ) -> Result<CallToolResult, ErrorData> {
        if runtime.is_read_only() && request.name.as_ref() == CHAT_MESSAGE_ADD {
            return Ok(tool_error(&ToolError::validation()));
        }
        match request.name.as_ref() {
            CHAT_MESSAGE_ADD => {
                let input = decode_arguments::<ChatMessageAddInput>(request.arguments)?;
                Ok(self
                    .chat_message_add(runtime, MutationAccess::Allowed, input, cancellation)
                    .await)
            }
            _ => Err(ErrorData::method_not_found::<CallToolRequestMethod>()),
        }
    }

    async fn chat_message_add(
        &self,
        runtime: &RuntimeContext,
        access: MutationAccess,
        input: ChatMessageAddInput,
        cancellation: &CancellationToken,
    ) -> CallToolResult {
        let deadline = runtime.request_deadline();
        if let Err(error) = require_mutation_access(access) {
            return tool_error(error.tool_error());
        }

        let client = runtime.client().clone();
        let resolved = runtime
            .execute_classified_until(
                deadline,
                OperationContext::new(CHAT_MESSAGE_ADD),
                cancellation,
                client.resolve_space_id(input.space.as_str()),
                OperationFailureDiagnostic::from_anytype,
            )
            .await;
        let resolved = match resolved {
            Ok(resolved) => resolved,
            Err(error) => return tool_error(&controlled_runtime_error(error)),
        };
        let space_id = match EntityId::new(resolved) {
            Ok(space_id) => space_id,
            Err(_) => return tool_error(&ToolError::upstream()),
        };
        let normalized = NormalizedChatAdd::new(space_id, input);
        let key = normalized.idempotency_key.clone();
        let fingerprint = normalized.fingerprint();

        match self
            .idempotency
            .begin_until(deadline, key.clone(), fingerprint)
            .await
        {
            BeginAttempt::Cached(result) => {
                if self.candidate(&key).is_some() {
                    self.replay(runtime, &key, cancellation, deadline).await
                } else {
                    result
                }
            }
            BeginAttempt::Indeterminate => tool_error(&ToolError::mutation_indeterminate()),
            BeginAttempt::Conflict => tool_error(&ToolError::conflict()),
            BeginAttempt::Full => tool_error(&ToolError::bounded_result()),
            BeginAttempt::Expired => tool_error(&ToolError::upstream()),
            BeginAttempt::Wait(attempt) => {
                #[cfg(test)]
                if let Some(gate) = self.cohort_gate.as_ref()
                    && !gate.waiter_seen.swap(true, Ordering::AcqRel)
                {
                    gate.waiter_admitted.add_permits(1);
                }
                mark_reused(wait_for_attempt_until(attempt, cancellation, deadline).await)
            }
            BeginAttempt::Lead(attempt) => {
                #[cfg(test)]
                if let Some(gate) = self.cohort_gate.as_ref()
                    && !gate.leader_seen.swap(true, Ordering::AcqRel)
                {
                    gate.leader_admitted.add_permits(1);
                    let Ok(permit) = gate.release_leader.acquire().await else {
                        return tool_error(&ToolError::upstream());
                    };
                    permit.forget();
                }
                let supervision = ChatAddSupervision {
                    runtime: runtime.clone(),
                    contract: self.contract.clone(),
                    store: self.idempotency.clone(),
                    candidates: self.candidates.clone(),
                    key,
                    attempt: attempt.clone(),
                    normalized,
                };
                tokio::spawn(supervise_chat_add(supervision));
                wait_for_attempt_until(attempt, cancellation, deadline).await
            }
        }
    }

    async fn replay(
        &self,
        runtime: &RuntimeContext,
        key: &IdempotencyKey,
        cancellation: &CancellationToken,
        deadline: Instant,
    ) -> CallToolResult {
        let Some(replay) = self.candidate(key) else {
            return tool_error(&ToolError::mutation_indeterminate());
        };
        let client = runtime.client().clone();
        let expected_id = replay.message_id.clone();
        let chat_id = replay.chat_id.clone();
        let space_id = replay.space_id.clone();
        let contract = self.contract.clone();
        execute_prepared_handler_until(
            runtime,
            deadline,
            &contract,
            OperationContext::new(CHAT_MESSAGE_ADD),
            cancellation,
            async move {
                let message = client
                    .chats()
                    .in_space(space_id.as_str())
                    .get_message(chat_id.as_str(), expected_id.as_str())
                    .get()
                    .await?;
                Ok::<_, HandlerOperationError>((space_id, chat_id, expected_id, message))
            },
            |(space_id, chat_id, expected_id, message)| async move {
                checked_output(space_id, chat_id, &expected_id, &message, true)
            },
        )
        .await
    }
}

fn controlled_runtime_error(error: ControlledOperationError<AnytypeError>) -> ToolError {
    match error {
        ControlledOperationError::Operation(source) => match ToolError::from_anytype(&source) {
            AnytypeErrorMapping::Ready(error) => error,
            AnytypeErrorMapping::AmbiguityRequiresCandidates => ToolError::upstream(),
        },
        ControlledOperationError::Cancelled
        | ControlledOperationError::TimedOut
        | ControlledOperationError::ShuttingDown => ToolError::upstream(),
    }
}

#[derive(Clone)]
struct NormalizedChatAdd {
    space_id: EntityId,
    chat_id: EntityId,
    text: ChatMessageText,
    reply_to_message_id: Option<EntityId>,
    idempotency_key: IdempotencyKey,
}

impl NormalizedChatAdd {
    fn new(space_id: EntityId, input: ChatMessageAddInput) -> Self {
        Self {
            space_id,
            chat_id: input.chat_id,
            text: input.text,
            reply_to_message_id: input.reply_to_message_id.as_ref().cloned(),
            idempotency_key: input.idempotency_key,
        }
    }

    fn fingerprint(&self) -> [u8; 32] {
        let mut hasher = Sha256::new();
        hash_field(&mut hasher, CHAT_ADD_FINGERPRINT_DOMAIN);
        hash_field(&mut hasher, self.space_id.as_str());
        hash_field(&mut hasher, self.chat_id.as_str());
        hash_field(&mut hasher, self.text.as_str());
        match self.reply_to_message_id.as_ref() {
            Some(reply) => {
                hasher.update([1]);
                hash_field(&mut hasher, reply.as_str());
            }
            None => hasher.update([0]),
        }
        hasher.finalize().into()
    }
}

fn hash_field(hasher: &mut Sha256, value: &str) {
    hasher.update(value.len().to_be_bytes());
    hasher.update(value.as_bytes());
}

struct ChatAddSupervision {
    runtime: RuntimeContext,
    contract: WorkflowTool<ChatMessageAddOutput>,
    store: Arc<IdempotencyStore>,
    candidates: Arc<StdMutex<HashMap<IdempotencyKey, CandidateRecord>>>,
    key: IdempotencyKey,
    attempt: Arc<Attempt>,
    normalized: NormalizedChatAdd,
}

async fn supervise_chat_add(supervision: ChatAddSupervision) {
    let ChatAddSupervision {
        runtime,
        contract,
        store,
        candidates,
        key,
        attempt,
        normalized,
    } = supervision;
    let progress = attempt.progress();
    let Some(deadline) = attempt.deadline() else {
        store
            .finish(
                &key,
                &attempt,
                CreateExecution::supervisor_failed(progress.stage()),
            )
            .await;
        return;
    };
    let task_progress = progress.clone();
    let task_candidates = candidates.clone();
    let task = tokio::spawn(async move {
        execute_chat_add(
            &runtime,
            &contract,
            normalized,
            &CancellationToken::new(),
            &task_progress,
            deadline,
            &task_candidates,
        )
        .await
    });
    let execution = match task.await {
        Ok(execution) => execution,
        Err(_) if retained_candidate(candidates.as_ref(), &key).is_some() => CreateExecution::new(
            tool_error(&ToolError::mutation_indeterminate()),
            CreateDisposition::Verified,
        ),
        Err(_) => CreateExecution::supervisor_failed(progress.stage()),
    };
    store.finish(&key, &attempt, execution).await;
}

async fn execute_chat_add(
    runtime: &RuntimeContext,
    contract: &WorkflowTool<ChatMessageAddOutput>,
    input: NormalizedChatAdd,
    cancellation: &CancellationToken,
    progress: &MutationProgress,
    deadline: Instant,
    candidates: &Arc<StdMutex<HashMap<IdempotencyKey, CandidateRecord>>>,
) -> CreateExecution {
    let client = runtime.client().clone();
    let definitive_rejection = Arc::new(AtomicBool::new(false));
    let operation_rejection = definitive_rejection.clone();
    let operation_progress = progress.clone();
    let candidate_store = candidates.clone();
    let replay_key = input.idempotency_key.clone();
    let candidate_retained = Arc::new(AtomicBool::new(false));
    let operation_candidate_retained = candidate_retained.clone();
    let result = execute_mutation_handler_until(
        runtime,
        deadline,
        contract,
        OperationContext::new(CHAT_MESSAGE_ADD),
        cancellation,
        progress,
        async move {
            let chats = client.chats().in_space(input.space_id.as_str());
            if let Some(reply_id) = input.reply_to_message_id.as_ref() {
                let reply = chats
                    .get_message(input.chat_id.as_str(), reply_id.as_str())
                    .get()
                    .await?;
                if reply.id != reply_id.as_str() {
                    return Err(HandlerError::new(ToolError::upstream()).into());
                }
                validate_message_reference(&reply, input.chat_id.as_str(), reply_id.as_str())?;
            }

            if cancellation.is_cancelled() {
                return Err(HandlerError::new(ToolError::upstream()).into());
            }
            let content = MessageContent::new().text(input.text.as_str());
            let mut request = chats.add_message(input.chat_id.as_str(), content);
            if let Some(reply_id) = input.reply_to_message_id.as_ref() {
                request = request.reply_to(reply_id.as_str());
            }
            operation_progress.mark_dispatched();
            let assigned = match request.send().await {
                Ok(assigned) => assigned,
                Err(error) if mutation_rejection_is_definitive(&error) => {
                    operation_rejection.store(true, Ordering::Release);
                    return Err(error.into());
                }
                Err(_) => return Err(indeterminate_operation()),
            };
            let message_id = EntityId::new(assigned).map_err(|_| indeterminate_operation())?;
            retain_candidate(
                candidate_store.as_ref(),
                replay_key,
                CandidateRecord {
                    space_id: input.space_id.clone(),
                    chat_id: input.chat_id.clone(),
                    message_id: message_id.clone(),
                },
            );
            operation_candidate_retained.store(true, Ordering::Release);
            let message = chats
                .get_message(input.chat_id.as_str(), message_id.as_str())
                .get()
                .await?;
            if !created_message_matches(&message, &message_id, &input) {
                return Err(indeterminate_operation());
            }
            let output = checked_output(
                input.space_id.clone(),
                input.chat_id.clone(),
                &message_id,
                &message,
                false,
            )?;
            Ok::<_, HandlerOperationError>(output)
        },
        |output| async move { Ok(output) },
    )
    .await;
    let disposition = if result.is_error == Some(false) {
        CreateDisposition::Verified
    } else if definitive_rejection.load(Ordering::Acquire) {
        CreateDisposition::Terminal
    } else if candidate_retained.load(Ordering::Acquire) {
        CreateDisposition::Verified
    } else if progress.stage() == MutationStage::PreDispatch {
        CreateDisposition::PreDispatchFailure
    } else {
        CreateDisposition::Indeterminate
    };
    CreateExecution::new(result, disposition)
}

fn created_message_matches(
    message: &ChatMessage,
    message_id: &EntityId,
    input: &NormalizedChatAdd,
) -> bool {
    message.id == message_id.as_str()
        && message.content.text == input.text.as_str()
        && matches!(message.content.style, MessageTextStyle::Paragraph)
        && message.content.marks.is_empty()
        && message.attachments.is_empty()
        && message.reply_to_message_id.as_deref()
            == input.reply_to_message_id.as_ref().map(EntityId::as_str)
}

fn checked_output(
    space_id: EntityId,
    chat_id: EntityId,
    expected_message_id: &EntityId,
    message: &ChatMessage,
    key_reused: bool,
) -> Result<ChatMessageAddOutput, HandlerError> {
    if message.id != expected_message_id.as_str() {
        return Err(HandlerError::new(ToolError::upstream()));
    }
    let detail = convert_message_detail(message, chat_id.as_str())?;
    bounded_output(
        ChatMessageAddOutput {
            space_id,
            chat_id,
            message: detail,
            idempotency: ChatAddIdempotency {
                key_reused,
                scope: ProcessScope,
            },
        },
        MESSAGE_RESULT_BYTES,
    )
}

fn mark_reused(mut result: CallToolResult) -> CallToolResult {
    if result.is_error != Some(false) {
        return result;
    }
    let Some(mut structured) = result.structured_content.take() else {
        return tool_error(&ToolError::upstream());
    };
    let Some(idempotency) = structured
        .as_object_mut()
        .and_then(|object| object.get_mut("idempotency"))
        .and_then(serde_json::Value::as_object_mut)
    else {
        return tool_error(&ToolError::upstream());
    };
    idempotency.insert("key_reused".to_owned(), serde_json::Value::Bool(true));
    let compact = structured.to_string();
    result.structured_content = Some(structured);
    result.content = vec![ContentBlock::text(compact)];
    result
}

fn indeterminate_operation() -> HandlerOperationError {
    HandlerError::new(ToolError::mutation_indeterminate()).into()
}

#[cfg(test)]
mod tests {
    use std::{
        collections::{BTreeMap, HashSet},
        io::{BufRead, Write},
        process::{Child, ChildStdin, ChildStdout, Command, Stdio},
        sync::Arc,
        thread,
        time::Duration,
    };

    use anytype::{
        objects::Icon,
        prelude::{AnytypeClient, ClientConfig, HttpCredentials},
        test_util::{DisposableRun, unique_suffix, with_disposable_space_context},
    };
    use chrono::TimeZone;
    use rmcp::model::{CallToolRequestParams, ListToolsResult};
    use serde_json::{Map, Value, json};
    use tokio::{
        io::{AsyncBufReadExt, AsyncWriteExt, BufReader, duplex, split},
        sync::Barrier,
    };

    use super::*;
    use crate::{
        config::{ApplicationProfile, ProtocolMode},
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
    const TOKEN_BUDGET_SNAPSHOT: &str =
        include_str!("../tests/snapshots/chats-add-token-budget.json");

    fn arguments(value: Value) -> Map<String, Value> {
        value.as_object().cloned().expect("object arguments")
    }

    fn input(key: &str) -> Value {
        json!({
            "space":SPACE_ID,
            "chat_id":CHAT_ID,
            "text":"bounded plain text",
            "idempotency_key":key,
        })
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

    fn chat_add_snapshot() -> Value {
        let tokenizer = tiktoken_rs::o200k_base().expect("o200k tokenizer");
        let catalog = canonical(
            serde_json::to_value(ListToolsResult::with_all_items(vec![
                chat_message_add_tool().unwrap().into_tool(),
            ]))
            .unwrap(),
        );
        let catalog_json = catalog.to_string();
        let message = message(
            "message-1",
            &"\"\\".repeat(MAX_MESSAGE_TEXT_CHARS / 2),
            None,
        );
        let output = checked_output(
            EntityId::new(SPACE_ID).unwrap(),
            EntityId::new(CHAT_ID).unwrap(),
            &EntityId::new("message-1").unwrap(),
            &message,
            false,
        )
        .expect("maximum reviewed output");
        let result = chat_message_add_tool()
            .unwrap()
            .success(&output)
            .expect("maximum encoded result");
        let structured = canonical(result.structured_content.clone().unwrap()).to_string();
        let encoded_result = canonical(serde_json::to_value(result).unwrap()).to_string();
        json!({
            "tokenizer":"tiktoken o200k_base (tiktoken-rs 0.12.0)",
            "catalog_ceiling_tokens":2_000,
            "catalog":{
                "tools":[CHAT_MESSAGE_ADD],
                "sha256":sha256(&catalog_json),
                "tokens":tokenizer.encode_with_special_tokens(&catalog_json).len(),
            },
            "maximum_result":{
                "sha256":sha256(&encoded_result),
                "structured_bytes":structured.len(),
                "encoded_result_tokens":tokenizer.encode_with_special_tokens(&encoded_result).len(),
                "ceiling_bytes":MESSAGE_RESULT_BYTES,
            }
        })
    }

    #[test]
    fn text_runtime_and_schema_boundaries_match() {
        for admitted in [
            "x".to_owned(),
            "\tmessage\n".to_owned(),
            "🦀".repeat(MAX_MESSAGE_TEXT_CHARS),
            "e\u{301}".repeat(MAX_MESSAGE_TEXT_CHARS / 2),
        ] {
            assert_eq!(
                ChatMessageText::new(admitted.clone()).unwrap().as_str(),
                admitted
            );
        }
        for rejected in [
            String::new(),
            " \t\n ".to_owned(),
            "private\0text".to_owned(),
            "x".repeat(MAX_MESSAGE_TEXT_CHARS + 1),
        ] {
            assert!(ChatMessageText::new(rejected).is_err());
        }

        let contract = chat_message_add_tool().expect("chat add contract");
        let schema = serde_json::to_value(contract.as_tool().input_schema.as_ref())
            .expect("input schema JSON");
        let text_ref = schema["properties"]["text"]["$ref"]
            .as_str()
            .expect("message text reference");
        let text_name = text_ref
            .strip_prefix("#/$defs/")
            .expect("local message text reference");
        let text = &schema["$defs"][text_name];
        assert_eq!(text["minLength"], 1);
        assert_eq!(text["maxLength"], MAX_MESSAGE_TEXT_CHARS);
        assert_eq!(schema["additionalProperties"], false);
        let required = schema["required"].as_array().expect("required fields");
        for field in ["space", "chat_id", "text", "idempotency_key"] {
            assert!(required.iter().any(|value| value == field));
        }
        assert!(!required.iter().any(|value| value == "reply_to_message_id"));
        let annotations = contract
            .as_tool()
            .annotations
            .as_ref()
            .expect("annotations");
        assert_eq!(annotations.title, None);
        assert_eq!(annotations.read_only_hint, Some(false));
        assert_eq!(annotations.destructive_hint, Some(false));
        assert_eq!(annotations.idempotent_hint, Some(false));
        assert_eq!(annotations.open_world_hint, Some(false));
    }

    #[test]
    fn strict_input_rejects_null_unknown_and_out_of_domain_values() {
        for value in [
            json!({}),
            json!({"space":SPACE_ID,"chat_id":CHAT_ID,"text":"x","idempotency_key":null}),
            json!({"space":SPACE_ID,"chat_id":CHAT_ID,"text":"x","idempotency_key":"k","reply_to_message_id":null}),
            json!({"space":SPACE_ID,"chat_id":CHAT_ID,"text":"x","idempotency_key":"k","unknown":true}),
            json!({"space":SPACE_ID,"chat_id":"x".repeat(257),"text":"x","idempotency_key":"k"}),
            json!({"space":SPACE_ID,"chat_id":CHAT_ID,"text":" ","idempotency_key":"k"}),
            json!({"space":SPACE_ID,"chat_id":CHAT_ID,"text":"x","idempotency_key":""}),
            json!({"space":SPACE_ID,"chat_id":CHAT_ID,"text":"x","idempotency_key":"k".repeat(257)}),
        ] {
            assert!(serde_json::from_value::<ChatMessageAddInput>(value).is_err());
        }
        let decoded: ChatMessageAddInput = serde_json::from_value(json!({
            "space":SPACE_ID,
            "chat_id":CHAT_ID,
            "text":"exact \t text\n",
            "reply_to_message_id":"message-1",
            "idempotency_key":"private-key",
        }))
        .expect("valid strict input");
        assert_eq!(decoded.text.as_str(), "exact \t text\n");
        assert_eq!(
            decoded.reply_to_message_id.as_ref().map(EntityId::as_str),
            Some("message-1")
        );
    }

    #[test]
    fn fingerprint_binds_resolved_scope_text_and_reply_presence() {
        fn normalized(text: &str, reply: Option<&str>) -> NormalizedChatAdd {
            let mut value = input("private-key");
            value["text"] = Value::String(text.to_owned());
            if let Some(reply) = reply {
                value["reply_to_message_id"] = Value::String(reply.to_owned());
            }
            NormalizedChatAdd::new(
                EntityId::new(SPACE_ID).unwrap(),
                serde_json::from_value(value).unwrap(),
            )
        }
        let base = normalized("text", None);
        assert_eq!(base.fingerprint(), normalized("text", None).fingerprint());
        assert_ne!(base.fingerprint(), normalized("text ", None).fingerprint());
        assert_ne!(
            base.fingerprint(),
            normalized("text", Some("message-1")).fingerprint()
        );
        let mut other = normalized("text", None);
        other.chat_id = EntityId::new("chat-2").unwrap();
        assert_ne!(base.fingerprint(), other.fingerprint());
    }

    fn message(id: &str, text: &str, reply: Option<&str>) -> ChatMessage {
        ChatMessage {
            id: id.to_owned(),
            order_id: "order-1".to_owned(),
            state_id: "state-1".to_owned(),
            creator: "author-1".to_owned(),
            creator_name: Some("Author".to_owned()),
            created_at: chrono::DateTime::parse_from_rfc3339("2026-07-22T12:00:00.001Z").unwrap(),
            modified_at: chrono::DateTime::parse_from_rfc3339("2026-07-22T12:00:00.002Z").unwrap(),
            reply_to_message_id: reply.map(str::to_owned),
            content: MessageContent::new().text(text),
            attachments: Vec::new(),
            reactions: Vec::new(),
            read: false,
            mention_read: false,
            has_mention: false,
            synced: true,
            pinned: false,
            unread_reaction: false,
            blocks: Vec::new(),
        }
    }

    #[test]
    fn creation_match_is_exact_and_output_is_allowlisted() {
        let normalized = NormalizedChatAdd::new(
            EntityId::new(SPACE_ID).unwrap(),
            serde_json::from_value(input("private-key")).unwrap(),
        );
        let exact = message("message-1", normalized.text.as_str(), None);
        let id = EntityId::new("message-1").unwrap();
        assert!(created_message_matches(&exact, &id, &normalized));
        let mut wrong = exact.clone();
        wrong.content = MessageContent::new().bold(normalized.text.as_str());
        assert!(!created_message_matches(&wrong, &id, &normalized));
        wrong = exact.clone();
        wrong.reply_to_message_id = Some("message-2".to_owned());
        assert!(!created_message_matches(&wrong, &id, &normalized));

        let output = checked_output(
            EntityId::new(SPACE_ID).unwrap(),
            EntityId::new(CHAT_ID).unwrap(),
            &id,
            &exact,
            false,
        )
        .unwrap();
        let value = serde_json::to_value(output).unwrap();
        assert_eq!(
            value
                .as_object()
                .unwrap()
                .keys()
                .cloned()
                .collect::<HashSet<_>>(),
            HashSet::from([
                "space_id".to_owned(),
                "chat_id".to_owned(),
                "message".to_owned(),
                "idempotency".to_owned(),
            ])
        );
        assert_eq!(
            value["idempotency"],
            json!({"key_reused":false,"scope":"process"})
        );
        assert_eq!(value["message"]["structured_blocks_observable"], false);
        assert!(value["message"].get("order_id").is_none());
        assert!(value["message"].get("reactions").is_none());
    }

    #[test]
    fn reply_reference_accepts_unreturned_long_text_but_validates_identity_and_timestamp() {
        let mut reply = message("message-1", &"x".repeat(MAX_MESSAGE_TEXT_CHARS + 1), None);
        validate_message_reference(&reply, CHAT_ID, "message-1")
            .expect("unreturned reply text has no detail projection ceiling");
        assert!(validate_message_reference(&reply, CHAT_ID, "message-2").is_err());
        reply.created_at = chrono::FixedOffset::east_opt(0)
            .expect("UTC offset")
            .with_ymd_and_hms(10_000, 1, 1, 0, 0, 0)
            .single()
            .expect("chrono extended year");
        assert!(validate_message_reference(&reply, CHAT_ID, "message-1").is_err());
    }

    #[tokio::test]
    async fn deadline_terminal_capacity_and_reply_predispatch_states_are_locked() {
        let key = IdempotencyKey::new("terminal-key").unwrap();
        let fingerprint = [7; 32];
        let store = IdempotencyStore::new(1);
        let leader_deadline = Instant::now() + Duration::from_millis(100);
        let BeginAttempt::Lead(attempt) = store
            .begin_until(leader_deadline, key.clone(), fingerprint)
            .await
        else {
            panic!("first key must lead");
        };
        assert_eq!(attempt.deadline(), Some(leader_deadline));
        store
            .finish(
                &key,
                &attempt,
                CreateExecution::new(
                    tool_error(&ToolError::authentication()),
                    CreateDisposition::Terminal,
                ),
            )
            .await;
        assert!(matches!(
            store.begin_until(leader_deadline, key, fingerprint).await,
            BeginAttempt::Cached(result) if result.is_error == Some(true)
        ));
        assert!(matches!(
            store
                .begin_until(
                    leader_deadline,
                    IdempotencyKey::new("capacity-key").unwrap(),
                    [8; 32],
                )
                .await,
            BeginAttempt::Full
        ));
        assert!(mutation_rejection_is_definitive(&AnytypeError::Forbidden));
        assert!(matches!(
            ToolError::from_anytype(&AnytypeError::Forbidden),
            AnytypeErrorMapping::Ready(error)
                if tool_error(&error).structured_content.unwrap()["code"] == "authentication"
        ));

        let release_store = IdempotencyStore::new(1);
        let reply_key = IdempotencyKey::new("reply-key").unwrap();
        let BeginAttempt::Lead(reply_attempt) = release_store
            .begin_until(leader_deadline, reply_key.clone(), [9; 32])
            .await
        else {
            panic!("reply preflight must lead");
        };
        release_store
            .finish(
                &reply_key,
                &reply_attempt,
                CreateExecution::new(
                    tool_error(&ToolError::not_found()),
                    CreateDisposition::PreDispatchFailure,
                ),
            )
            .await;
        assert!(matches!(
            release_store
                .begin_until(leader_deadline, reply_key, [9; 32])
                .await,
            BeginAttempt::Lead(_)
        ));

        let wait_store = IdempotencyStore::new(1);
        let wait_key = IdempotencyKey::new("wait-key").unwrap();
        let leader_deadline = Instant::now() + Duration::from_millis(80);
        let BeginAttempt::Lead(leader) = wait_store
            .begin_until(leader_deadline, wait_key.clone(), [10; 32])
            .await
        else {
            panic!("deadline leader");
        };
        let BeginAttempt::Wait(waiter) = wait_store
            .begin_until(Instant::now() + Duration::from_secs(1), wait_key, [10; 32])
            .await
        else {
            panic!("same key must wait");
        };
        assert!(Arc::ptr_eq(&leader, &waiter));
        let started = Instant::now();
        let result = wait_for_attempt_until(
            waiter,
            &CancellationToken::new(),
            Instant::now() + Duration::from_secs(1),
        )
        .await;
        assert_eq!(result.structured_content.unwrap()["code"], "upstream");
        assert!(started.elapsed() < Duration::from_millis(300));

        let earlier_store = IdempotencyStore::new(1);
        let earlier_key = IdempotencyKey::new("earlier-caller").unwrap();
        let BeginAttempt::Lead(earlier) = earlier_store
            .begin_until(
                Instant::now() + Duration::from_secs(1),
                earlier_key,
                [11; 32],
            )
            .await
        else {
            panic!("earlier caller leader");
        };
        let started = Instant::now();
        let _ = wait_for_attempt_until(
            earlier,
            &CancellationToken::new(),
            Instant::now() + Duration::from_millis(30),
        )
        .await;
        assert!(started.elapsed() < Duration::from_millis(200));
        assert!(matches!(
            earlier_store
                .begin_until(
                    Instant::now(),
                    IdempotencyKey::new("expired-key").unwrap(),
                    [12; 32],
                )
                .await,
            BeginAttempt::Expired
        ));
    }

    #[tokio::test]
    async fn assigned_candidate_is_retained_for_get_only_retry_after_every_safe_get_failure() {
        for (index, failure) in [
            ToolError::not_found(),
            ToolError::authentication(),
            ToolError::bounded_result(),
            ToolError::upstream(),
        ]
        .into_iter()
        .enumerate()
        {
            let handlers = ChatMessageAddHandlers::with_capacity(1).unwrap();
            let key = IdempotencyKey::new(format!("candidate-{index}")).unwrap();
            let fingerprint = [u8::try_from(index).unwrap(); 32];
            let deadline = Instant::now() + Duration::from_secs(1);
            let BeginAttempt::Lead(attempt) = handlers
                .idempotency
                .begin_until(deadline, key.clone(), fingerprint)
                .await
            else {
                panic!("candidate must lead");
            };
            retain_candidate(
                handlers.candidates.as_ref(),
                key.clone(),
                CandidateRecord {
                    space_id: EntityId::new(SPACE_ID).unwrap(),
                    chat_id: EntityId::new(CHAT_ID).unwrap(),
                    message_id: EntityId::new(format!("message-{index}")).unwrap(),
                },
            );
            handlers
                .idempotency
                .finish(
                    &key,
                    &attempt,
                    CreateExecution::new(tool_error(&failure), CreateDisposition::Verified),
                )
                .await;

            assert!(handlers.candidate(&key).is_some());
            assert!(matches!(
                handlers
                    .idempotency
                    .begin_until(deadline, key.clone(), fingerprint)
                    .await,
                BeginAttempt::Cached(result) if result.is_error == Some(true)
            ));
            assert!(matches!(
                handlers
                    .idempotency
                    .begin_until(deadline, key, [99; 32])
                    .await,
                BeginAttempt::Conflict
            ));
        }
    }

    #[tokio::test]
    async fn mutation_supervisor_uses_the_supplied_absolute_deadline() {
        for dispatched in [false, true] {
            let runtime = runtime(no_io_client(), false);
            let contract = chat_message_add_tool().unwrap();
            let progress = MutationProgress::new();
            if dispatched {
                progress.mark_dispatched();
            }
            let started = Instant::now();
            let result = execute_mutation_handler_until(
                &runtime,
                started + Duration::from_millis(25),
                &contract,
                OperationContext::new("chat_add_deadline_test"),
                &CancellationToken::new(),
                &progress,
                std::future::pending::<Result<ChatMessageAddOutput, HandlerOperationError>>(),
                |output| async move { Ok(output) },
            )
            .await;
            assert_eq!(result.is_error, Some(true));
            assert_eq!(
                result.structured_content.unwrap()["code"],
                if dispatched { "conflict" } else { "upstream" }
            );
            assert!(started.elapsed() < Duration::from_millis(200));
            assert_eq!(runtime.client().http_metrics().logical_operations, 0);
            assert_eq!(runtime.client().http_metrics().physical_attempts, 0);
        }
    }

    #[tokio::test]
    async fn concurrent_cohort_admission_and_completion_use_one_attempt_without_timing() {
        let store = Arc::new(IdempotencyStore::new(1));
        let barrier = Arc::new(Barrier::new(3));
        let key = IdempotencyKey::new("barrier-key").unwrap();
        let fingerprint = [13; 32];
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut tasks = Vec::new();
        for _ in 0..2 {
            let store = store.clone();
            let barrier = barrier.clone();
            let key = key.clone();
            tasks.push(tokio::spawn(async move {
                barrier.wait().await;
                store.begin_until(deadline, key, fingerprint).await
            }));
        }
        barrier.wait().await;
        let first = tasks.remove(0).await.expect("first admission task");
        let second = tasks.remove(0).await.expect("second admission task");
        let (leader, waiter) = match (first, second) {
            (BeginAttempt::Lead(leader), BeginAttempt::Wait(waiter))
            | (BeginAttempt::Wait(waiter), BeginAttempt::Lead(leader)) => (leader, waiter),
            _ => panic!("barrier admissions must produce one leader and one waiter"),
        };
        assert!(Arc::ptr_eq(&leader, &waiter));

        let exact = message("message-1", "barrier text", None);
        let output = checked_output(
            EntityId::new(SPACE_ID).unwrap(),
            EntityId::new(CHAT_ID).unwrap(),
            &EntityId::new("message-1").unwrap(),
            &exact,
            false,
        )
        .unwrap();
        store
            .finish(
                &key,
                &leader,
                CreateExecution::new(
                    chat_message_add_tool().unwrap().success(&output).unwrap(),
                    CreateDisposition::Verified,
                ),
            )
            .await;
        let leader_result = wait_for_attempt_until(
            leader,
            &CancellationToken::new(),
            Instant::now() + Duration::from_secs(1),
        )
        .await;
        let waiter_result = mark_reused(
            wait_for_attempt_until(
                waiter,
                &CancellationToken::new(),
                Instant::now() + Duration::from_secs(1),
            )
            .await,
        );
        assert_eq!(
            leader_result.structured_content.as_ref().unwrap()["message"],
            waiter_result.structured_content.as_ref().unwrap()["message"]
        );
        assert_eq!(
            leader_result.structured_content.as_ref().unwrap()["idempotency"]["key_reused"],
            false
        );
        assert_eq!(
            waiter_result.structured_content.as_ref().unwrap()["idempotency"]["key_reused"],
            true
        );
    }

    #[test]
    fn waiter_reuse_reencodes_identical_structured_and_text_results() {
        let exact = message("message-1", "text", None);
        let output = checked_output(
            EntityId::new(SPACE_ID).unwrap(),
            EntityId::new(CHAT_ID).unwrap(),
            &EntityId::new("message-1").unwrap(),
            &exact,
            false,
        )
        .unwrap();
        let contract = chat_message_add_tool().unwrap();
        let result = mark_reused(contract.success(&output).unwrap());
        assert_eq!(result.is_error, Some(false));
        let structured = result.structured_content.as_ref().unwrap();
        assert_eq!(structured["idempotency"]["key_reused"], true);
        assert_eq!(
            result.content[0].as_text().unwrap().text,
            structured.to_string()
        );
    }

    #[derive(Debug)]
    struct AddRegistry {
        handlers: ChatMessageAddHandlers,
    }

    impl OptionalToolsetRegistry for AddRegistry {
        fn metadata(&self) -> OptionalToolsetMetadata {
            OptionalToolsetMetadata::new("chats", false)
        }

        fn tools(&self) -> Result<Vec<OptionalRegistryTool>, SchemaContractError> {
            chat_add_tools()
        }

        fn scripted_scenario_ids(&self) -> &'static [&'static str] {
            &["chat_add_direct", "chat_add_stdio"]
        }

        fn headless_scenario_ids(&self) -> &'static [&'static str] {
            &["chat_add_headless"]
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

    fn production_unselected_runtime(client: AnytypeClient) -> RuntimeContext {
        RuntimeContext::from_parts_with_profile_and_optional_toolsets(
            client,
            8,
            Duration::from_secs(30),
            StartupStatus {
                http_available: true,
                grpc_available: true,
            },
            ApplicationProfile::Compact,
            false,
            OptionalToolsetSelection::parse(None, &[]).unwrap(),
        )
    }

    fn server(
        client: AnytypeClient,
        read_only: bool,
        handlers: ChatMessageAddHandlers,
    ) -> AnyMcpServer {
        let registry = Box::leak(Box::new(AddRegistry { handlers }));
        let registries: &'static [&'static dyn OptionalToolsetRegistry] =
            Box::leak(vec![registry as &dyn OptionalToolsetRegistry].into_boxed_slice());
        AnyMcpServer::new_with_optional_registries(runtime(client, read_only), registries)
            .expect("chat add test server")
    }

    fn no_io_client() -> AnytypeClient {
        let client = AnytypeClient::with_config(ClientConfig {
            base_url: Some("http://127.0.0.1:1".to_owned()),
            keystore: Some("env".to_owned()),
            keystore_service: Some("chat-add-no-io".to_owned()),
            app_name: "chat-add-no-io".to_owned(),
            ..ClientConfig::default()
        })
        .unwrap();
        client.set_api_key(HttpCredentials::new("unused-no-io-token"));
        client
    }

    #[tokio::test]
    async fn capacity_and_read_only_reject_before_http() {
        let client = no_io_client();
        let before = client.http_metrics();
        let full_server = server(
            client.clone(),
            false,
            ChatMessageAddHandlers::with_capacity(0).unwrap(),
        );
        let full = Box::pin(
            full_server.dispatch_tool(
                CallToolRequestParams::new(CHAT_MESSAGE_ADD)
                    .with_arguments(arguments(input("private-capacity-key"))),
                &CancellationToken::new(),
            ),
        )
        .await
        .unwrap();
        assert_eq!(full.is_error, Some(true));
        assert_eq!(full.structured_content.unwrap()["code"], "bounded_result");

        let read_only_server = server(client.clone(), true, ChatMessageAddHandlers::new().unwrap());
        let read_only = Box::pin(
            read_only_server.dispatch_tool(
                CallToolRequestParams::new(CHAT_MESSAGE_ADD)
                    .with_arguments(arguments(input("private-read-only-key"))),
                &CancellationToken::new(),
            ),
        )
        .await
        .unwrap();
        assert_eq!(read_only.is_error, Some(true));
        assert_eq!(read_only.structured_content.unwrap()["code"], "validation");
        let after = client.http_metrics();
        assert_eq!(after.logical_operations, before.logical_operations);
        assert_eq!(after.physical_attempts, before.physical_attempts);
    }

    async fn direct(server: &AnyMcpServer, value: Value) -> CallToolResult {
        Box::pin(server.dispatch_tool(
            CallToolRequestParams::new(CHAT_MESSAGE_ADD).with_arguments(arguments(value)),
            &CancellationToken::new(),
        ))
        .await
        .expect("direct chat add")
    }

    struct PreviewStdioSession {
        reader: BufReader<tokio::io::ReadHalf<tokio::io::DuplexStream>>,
        writer: tokio::io::WriteHalf<tokio::io::DuplexStream>,
        task: tokio::task::JoinHandle<()>,
        next_id: u64,
    }

    impl PreviewStdioSession {
        fn start(server: AnyMcpServer) -> Self {
            let (client_io, server_io) = duplex(64 * 1024);
            let (server_reader, server_writer) = split(server_io);
            let task = tokio::spawn(async move {
                crate::stdio::serve_preview(server, BufReader::new(server_reader), server_writer)
                    .await
                    .expect("preview stdio transport");
            });
            let (client_reader, writer) = split(client_io);
            Self {
                reader: BufReader::new(client_reader),
                writer,
                task,
                next_id: 1,
            }
        }

        async fn call(&mut self, value: Value) -> Value {
            self.call_named(CHAT_MESSAGE_ADD, value).await
        }

        async fn call_named(&mut self, name: &str, value: Value) -> Value {
            let request_id = self.next_id;
            self.next_id = self.next_id.checked_add(1).expect("small request ID");
            let frame = json!({
                "jsonrpc":"2.0",
                "id":request_id,
                "method":"tools/call",
                "params":{
                    "name":name,
                    "arguments":value,
                    "_meta":{
                        "io.modelcontextprotocol/protocolVersion":"2026-07-28",
                        "io.modelcontextprotocol/clientInfo":{"name":"chat-add-test","version":"1"},
                        "io.modelcontextprotocol/clientCapabilities":{}
                    }
                }
            });
            self.writer
                .write_all(format!("{frame}\n").as_bytes())
                .await
                .expect("write preview frame");
            let mut line = String::new();
            self.reader
                .read_line(&mut line)
                .await
                .expect("read preview frame");
            let response: Value = serde_json::from_str(&line).expect("preview JSON");
            assert_eq!(response["id"], request_id);
            response
        }

        async fn list_tools(&mut self) -> Value {
            let request_id = self.next_id;
            self.next_id = self.next_id.checked_add(1).expect("small request ID");
            let frame = json!({
                "jsonrpc":"2.0",
                "id":request_id,
                "method":"tools/list",
                "params":{
                    "_meta":{
                        "io.modelcontextprotocol/protocolVersion":"2026-07-28",
                        "io.modelcontextprotocol/clientInfo":{"name":"chat-add-test","version":"1"},
                        "io.modelcontextprotocol/clientCapabilities":{}
                    }
                }
            });
            self.writer
                .write_all(format!("{frame}\n").as_bytes())
                .await
                .expect("write preview list frame");
            let mut line = String::new();
            self.reader
                .read_line(&mut line)
                .await
                .expect("read preview list frame");
            let response: Value = serde_json::from_str(&line).expect("preview list JSON");
            assert_eq!(response["id"], request_id);
            response
        }

        async fn finish(mut self) {
            self.writer
                .shutdown()
                .await
                .expect("shutdown preview input");
            drop(self.writer);
            drop(self.reader);
            self.task.await.expect("preview task");
        }
    }

    struct SpawnedStdioSession {
        child: Child,
        stdin: Option<ChildStdin>,
        stdout: std::io::BufReader<ChildStdout>,
        next_id: u64,
    }

    impl SpawnedStdioSession {
        fn start(entrypoint: &str) -> Self {
            let mut child = Command::new(std::env::current_exe().expect("current test executable"))
                .arg(entrypoint)
                .arg("--exact")
                .arg("--nocapture")
                .arg("--test-threads=1")
                .env("ANY_MCP_CHAT_ADD_PROCESS_CHILD", "1")
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
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
            let frame = json!({
                "jsonrpc":"2.0",
                "id":id,
                "method":method,
                "params":params,
            });
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

        fn list_tools(&mut self) -> Value {
            self.request(
                "tools/list",
                json!({
                    "_meta":{
                        "io.modelcontextprotocol/protocolVersion":"2026-07-28",
                        "io.modelcontextprotocol/clientInfo":{"name":"spawned-chat-add-test","version":"1"},
                        "io.modelcontextprotocol/clientCapabilities":{}
                    }
                }),
            )
        }

        fn call(&mut self, value: Value) -> Value {
            self.request(
                "tools/call",
                json!({
                    "name":CHAT_MESSAGE_ADD,
                    "arguments":value,
                    "_meta":{
                        "io.modelcontextprotocol/protocolVersion":"2026-07-28",
                        "io.modelcontextprotocol/clientInfo":{"name":"spawned-chat-add-test","version":"1"},
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

    fn run_spawned_stdio_child(server: AnyMcpServer) {
        if std::env::var_os("ANY_MCP_CHAT_ADD_PROCESS_CHILD").is_none() {
            return;
        }
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("spawned stdio runtime")
            .block_on(crate::stdio::serve_stdio(
                server,
                ProtocolMode::Experimental20260728,
            ))
            .expect("spawned stdio server");
    }

    #[test]
    fn spawned_reviewed_chat_add_process_entrypoint() {
        run_spawned_stdio_child(server(
            no_io_client(),
            false,
            ChatMessageAddHandlers::new().unwrap(),
        ));
    }

    #[test]
    fn spawned_shipped_unselected_process_entrypoint() {
        run_spawned_stdio_child(
            AnyMcpServer::new(production_unselected_runtime(no_io_client()))
                .expect("shipped production composition"),
        );
    }

    #[test]
    fn spawned_process_stdio_covers_reviewed_registry_and_unselected_rejection() {
        let mut reviewed = SpawnedStdioSession::start(
            "chat_add_toolset::tests::spawned_reviewed_chat_add_process_entrypoint",
        );
        let catalog = reviewed.list_tools();
        assert!(
            catalog["result"]["tools"]
                .as_array()
                .expect("reviewed catalog")
                .iter()
                .any(|tool| tool["name"] == CHAT_MESSAGE_ADD),
            "{catalog}"
        );
        let mut invalid = input("spawned-invalid");
        invalid["reply_to_message_id"] = Value::Null;
        let rejected = reviewed.call(invalid);
        assert_eq!(rejected["error"]["code"], -32602);
        reviewed.finish();

        let mut shipped = SpawnedStdioSession::start(
            "chat_add_toolset::tests::spawned_shipped_unselected_process_entrypoint",
        );
        let catalog = shipped.list_tools();
        assert!(
            !catalog["result"]["tools"]
                .as_array()
                .expect("shipped catalog")
                .iter()
                .any(|tool| tool["name"] == CHAT_MESSAGE_ADD)
        );
        let rejected = shipped.call(input("spawned-unlinked"));
        assert_eq!(rejected["error"]["code"], -32601);
        shipped.finish();
    }

    fn outcome(result: Result<CallToolResult, ErrorData>) -> Value {
        match result {
            Ok(result) => json!({"result":result}),
            Err(error) => json!({"error":error}),
        }
    }

    fn preview_outcome(response: Value) -> Value {
        match (response.get("result"), response.get("error")) {
            (Some(result), None) => json!({"result":result}),
            (None, Some(error)) => json!({"error":error}),
            _ => panic!("preview response must contain exactly one outcome"),
        }
    }

    fn result_semantics(outcome: &Value) -> Value {
        let mut result = outcome["result"].clone();
        result
            .as_object_mut()
            .expect("tool result object")
            .remove("resultType");
        result
    }

    async fn assert_contract_parity() {
        let schema_server = server(
            no_io_client(),
            false,
            ChatMessageAddHandlers::new().unwrap(),
        );
        let direct_tool = schema_server
            .tools()
            .iter()
            .find(|tool| tool.name == CHAT_MESSAGE_ADD)
            .expect("direct chat-add tool");
        let mut stdio = PreviewStdioSession::start(server(
            no_io_client(),
            false,
            ChatMessageAddHandlers::new().unwrap(),
        ));
        let listed = stdio.list_tools().await;
        let stdio_tool = listed["result"]["tools"]
            .as_array()
            .expect("stdio tools")
            .iter()
            .find(|tool| tool["name"] == CHAT_MESSAGE_ADD)
            .expect("stdio chat-add tool");
        assert_eq!(
            canonical(serde_json::to_value(direct_tool).unwrap()),
            canonical(stdio_tool.clone())
        );
        stdio.finish().await;
    }

    async fn assert_validation_parity() {
        for value in [
            {
                let mut value = input("explicit-null");
                value["reply_to_message_id"] = Value::Null;
                value
            },
            {
                let mut value = input("unknown-field");
                value["unknown"] = Value::Bool(true);
                value
            },
        ] {
            let direct_client = no_io_client();
            let direct_server = server(
                direct_client.clone(),
                false,
                ChatMessageAddHandlers::new().unwrap(),
            );
            let direct = outcome(
                Box::pin(
                    direct_server.dispatch_tool(
                        CallToolRequestParams::new(CHAT_MESSAGE_ADD)
                            .with_arguments(arguments(value.clone())),
                        &CancellationToken::new(),
                    ),
                )
                .await,
            );
            let stdio_client = no_io_client();
            let mut stdio = PreviewStdioSession::start(server(
                stdio_client.clone(),
                false,
                ChatMessageAddHandlers::new().unwrap(),
            ));
            let preview = preview_outcome(stdio.call(value).await);
            stdio.finish().await;
            assert_eq!(direct, preview);
            assert_eq!(direct["error"]["code"], -32602);
            assert_eq!(metric_counts(&direct_client), (0, 0));
            assert_eq!(metric_counts(&stdio_client), (0, 0));
        }

        let value = input("unknown-tool");
        let direct_client = no_io_client();
        let direct_server = server(
            direct_client.clone(),
            false,
            ChatMessageAddHandlers::new().unwrap(),
        );
        let stdio_client = no_io_client();
        let mut stdio = PreviewStdioSession::start(server(
            stdio_client.clone(),
            false,
            ChatMessageAddHandlers::new().unwrap(),
        ));
        let direct = outcome(
            Box::pin(
                direct_server.dispatch_tool(
                    CallToolRequestParams::new("chat_message_unknown")
                        .with_arguments(arguments(value.clone())),
                    &CancellationToken::new(),
                ),
            )
            .await,
        );
        let preview = preview_outcome(stdio.call_named("chat_message_unknown", value).await);
        stdio.finish().await;
        assert_eq!(direct, preview);
        assert_eq!(direct["error"]["code"], -32601);
        assert_eq!(metric_counts(&direct_client), (0, 0));
        assert_eq!(metric_counts(&stdio_client), (0, 0));
    }

    async fn assert_read_only_parity() {
        let value = input("read-only");
        let direct_client = no_io_client();
        let direct_server = server(
            direct_client.clone(),
            true,
            ChatMessageAddHandlers::new().unwrap(),
        );
        let direct = outcome(
            Box::pin(
                direct_server.dispatch_tool(
                    CallToolRequestParams::new(CHAT_MESSAGE_ADD)
                        .with_arguments(arguments(value.clone())),
                    &CancellationToken::new(),
                ),
            )
            .await,
        );
        let stdio_client = no_io_client();
        let mut stdio = PreviewStdioSession::start(server(
            stdio_client.clone(),
            true,
            ChatMessageAddHandlers::new().unwrap(),
        ));
        let preview = preview_outcome(stdio.call(value).await);
        stdio.finish().await;
        assert_eq!(result_semantics(&direct), result_semantics(&preview));
        assert_eq!(preview["result"]["resultType"], "complete");
        assert_eq!(direct["result"]["isError"], true);
        assert_eq!(direct["result"]["structuredContent"]["code"], "validation");
        assert_eq!(metric_counts(&direct_client), (0, 0));
        assert_eq!(metric_counts(&stdio_client), (0, 0));
    }

    async fn assert_precancel_parity() {
        let value = input("pre-cancelled");
        let direct_client = no_io_client();
        let direct_server = server(
            direct_client.clone(),
            false,
            ChatMessageAddHandlers::new().unwrap(),
        );
        let cancelled = CancellationToken::new();
        cancelled.cancel();
        let direct = outcome(
            Box::pin(
                direct_server.dispatch_tool(
                    CallToolRequestParams::new(CHAT_MESSAGE_ADD)
                        .with_arguments(arguments(value.clone())),
                    &cancelled,
                ),
            )
            .await,
        );

        let preview_client = no_io_client();
        let preview_server = server(
            preview_client.clone(),
            false,
            ChatMessageAddHandlers::new().unwrap(),
        );
        let preview_cancelled = CancellationToken::new();
        preview_cancelled.cancel();
        let preview = preview_outcome(
            crate::stdio::dispatch_modern(
                &preview_server,
                json!(71),
                "tools/call",
                arguments(json!({
                    "name":CHAT_MESSAGE_ADD,
                    "arguments":value,
                })),
                &preview_cancelled,
            )
            .await,
        );
        assert_eq!(result_semantics(&direct), result_semantics(&preview));
        assert_eq!(preview["result"]["resultType"], "complete");
        assert_eq!(direct["result"]["isError"], true);
        assert_eq!(direct["result"]["structuredContent"]["code"], "upstream");
        assert_eq!(metric_counts(&direct_client), (0, 0));
        assert_eq!(metric_counts(&preview_client), (0, 0));
    }

    #[tokio::test]
    async fn direct_and_preview_stdio_catalogs_are_exactly_identical() {
        Box::pin(assert_contract_parity()).await;
    }

    #[tokio::test]
    async fn direct_and_preview_stdio_validation_errors_are_exactly_identical() {
        Box::pin(assert_validation_parity()).await;
    }

    #[tokio::test]
    async fn direct_and_preview_stdio_read_only_results_are_exactly_identical() {
        Box::pin(assert_read_only_parity()).await;
    }

    #[tokio::test]
    async fn direct_and_preview_dispatch_precancel_results_are_exactly_identical() {
        Box::pin(assert_precancel_parity()).await;
    }

    #[test]
    fn parity_regressions_fit_the_reviewed_two_mib_stack() {
        std::thread::Builder::new()
            .name("chat-add-two-mib".to_owned())
            .stack_size(2 * 1024 * 1024)
            .spawn(|| {
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("two-MiB test runtime")
                    .block_on(async {
                        Box::pin(assert_contract_parity()).await;
                        Box::pin(assert_validation_parity()).await;
                        Box::pin(assert_read_only_parity()).await;
                        Box::pin(assert_precancel_parity()).await;
                    });
            })
            .expect("spawn two-MiB chat-add regression")
            .join()
            .expect("join two-MiB chat-add regression");
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

    #[test]
    #[ignore = "requires env-only disposable credentials and an authenticated headless Anytype server"]
    #[serial_test::serial(disposable_anytype_api)]
    fn headless_direct_and_preview_stdio_add_concurrent_replay_and_capacity_paths() {
        std::thread::Builder::new()
            .name("chat-add-live".to_owned())
            .stack_size(16 * 1024 * 1024)
            .spawn(|| {
                tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(4)
                    .thread_stack_size(16 * 1024 * 1024)
                    .enable_all()
                    .build()
                    .expect("chat add live runtime")
                    .block_on(async {
                        let outcome = Box::pin(with_disposable_space_context("any-mcp-chat-add", |ctx| {
                            Box::pin(async move {
                                ctx.client.ping_http().await.expect("authenticated HTTP");
                                let suffix = unique_suffix();
                                let chat = ctx
                                    .client
                                    .chats()
                                    .in_space(&ctx.space_id)
                                    .create(
                                        format!("mcp-chat-add-{suffix}"),
                                        Icon::Emoji {
                                            emoji: "✉️".to_owned(),
                                        },
                                    )
                                    .create()
                                    .await
                                    .expect("create disposable chat");
                                ctx.register_object(&chat.id);

                                let cohort_gate = CohortGate::new();
                                let direct_server = Arc::new(server(
                                    ctx.client.clone(),
                                    false,
                                    ChatMessageAddHandlers::new()
                                        .unwrap()
                                        .with_cohort_gate(cohort_gate.clone()),
                                ));
                                let direct_input = json!({
                                    "space":ctx.space_id,
                                    "chat_id":chat.id,
                                    "text":format!("direct {suffix} 🦀 e\u{301} \"\\"),
                                    "idempotency_key":format!("direct-key-{suffix}"),
                                });
                                let before = metric_counts(&ctx.client);
                                let leader_server = direct_server.clone();
                                let leader_input = direct_input.clone();
                                let leader = tokio::spawn(async move {
                                    direct(&leader_server, leader_input).await
                                });
                                cohort_gate
                                    .leader_admitted
                                    .acquire()
                                    .await
                                    .expect("leader admission")
                                    .forget();
                                let waiter_server = direct_server.clone();
                                let waiter_input = direct_input.clone();
                                let waiter = tokio::spawn(async move {
                                    direct(&waiter_server, waiter_input).await
                                });
                                cohort_gate
                                    .waiter_admitted
                                    .acquire()
                                    .await
                                    .expect("waiter admission")
                                    .forget();
                                cohort_gate.release_leader.add_permits(1);
                                let first = leader.await.expect("leader protocol call");
                                let waited = waiter.await.expect("waiter protocol call");
                                assert_eq!(first.is_error, Some(false), "{first:?}");
                                assert_eq!(waited.is_error, Some(false), "{waited:?}");
                                let first_value = first.structured_content.as_ref().unwrap();
                                let waited_value = waited.structured_content.as_ref().unwrap();
                                assert_eq!(first_value["idempotency"], json!({"key_reused":false,"scope":"process"}));
                                assert_eq!(waited_value["idempotency"], json!({"key_reused":true,"scope":"process"}));
                                assert_eq!(first_value["message"], waited_value["message"]);
                                let direct_id = first_value["message"]["id"]
                                    .as_str()
                                    .expect("direct message ID")
                                    .to_owned();
                                ctx.register_chat_message(&chat.id, &direct_id)?;
                                assert_eq!(metric_delta(before, metric_counts(&ctx.client)), (2, 2));

                                let changed = format!("changed presentation {suffix}");
                                ctx.client
                                    .chats()
                                    .in_space(&ctx.space_id)
                                    .edit_message(
                                        &chat.id,
                                        &direct_id,
                                        MessageContent::new().bold(&changed),
                                    )
                                    .send()
                                    .await
                                    .expect("independent message change");
                                let before = metric_counts(&ctx.client);
                                let replay = direct(&direct_server, direct_input.clone()).await;
                                assert_eq!(replay.is_error, Some(false));
                                let replay_value = replay.structured_content.as_ref().unwrap();
                                assert_eq!(replay_value["message"]["id"], direct_id);
                                assert_eq!(replay_value["message"]["text"], changed);
                                assert_eq!(replay_value["message"]["rest_has_formatting"], true);
                                assert_eq!(replay_value["idempotency"]["key_reused"], true);
                                assert_eq!(metric_delta(before, metric_counts(&ctx.client)), (1, 1));

                                let conflict = {
                                    let mut changed_input = direct_input.clone();
                                    changed_input["text"] = Value::String("different".to_owned());
                                    let before = metric_counts(&ctx.client);
                                    let result = direct(&direct_server, changed_input).await;
                                    assert_eq!(metric_delta(before, metric_counts(&ctx.client)), (0, 0));
                                    result
                                };
                                assert_eq!(conflict.structured_content.unwrap()["code"], "conflict");

                                let missing_reply_input = json!({
                                    "space":ctx.space_id,
                                    "chat_id":chat.id,
                                    "text":format!("missing reply {suffix}"),
                                    "reply_to_message_id":format!("missing-message-{suffix}"),
                                    "idempotency_key":format!("missing-reply-key-{suffix}"),
                                });
                                for _ in 0..2 {
                                    let before = metric_counts(&ctx.client);
                                    let failed = direct(&direct_server, missing_reply_input.clone()).await;
                                    assert_eq!(failed.is_error, Some(true));
                                    assert_eq!(failed.structured_content.unwrap()["code"], "not_found");
                                    assert_eq!(metric_delta(before, metric_counts(&ctx.client)).0, 1);
                                }

                                let capacity_server = server(
                                    ctx.client.clone(),
                                    false,
                                    ChatMessageAddHandlers::with_capacity(1).unwrap(),
                                );
                                let capacity_first = direct(
                                    &capacity_server,
                                    json!({
                                        "space":ctx.space_id,
                                        "chat_id":chat.id,
                                        "text":format!("capacity retained {suffix}"),
                                        "idempotency_key":format!("capacity-first-{suffix}"),
                                    }),
                                )
                                .await;
                                assert_eq!(capacity_first.is_error, Some(false));
                                let capacity_id = capacity_first.structured_content.as_ref().unwrap()
                                    ["message"]["id"]
                                    .as_str()
                                    .expect("capacity message ID");
                                ctx.register_chat_message(&chat.id, capacity_id)?;
                                let before = metric_counts(&ctx.client);
                                let capacity_full = direct(
                                    &capacity_server,
                                    json!({
                                        "space":ctx.space_id,
                                        "chat_id":chat.id,
                                        "text":format!("capacity rejected {suffix}"),
                                        "idempotency_key":format!("capacity-second-{suffix}"),
                                    }),
                                )
                                .await;
                                assert_eq!(capacity_full.is_error, Some(true));
                                assert_eq!(capacity_full.structured_content.unwrap()["code"], "bounded_result");
                                assert_eq!(metric_delta(before, metric_counts(&ctx.client)), (0, 0));

                                let stdio_server = server(
                                    ctx.client.clone(),
                                    false,
                                    ChatMessageAddHandlers::new().unwrap(),
                                );
                                let mut stdio = PreviewStdioSession::start(stdio_server);
                                let stdio_input = json!({
                                    "space":ctx.space_id,
                                    "chat_id":chat.id,
                                    "text":format!("reply {suffix}"),
                                    "reply_to_message_id":direct_id,
                                    "idempotency_key":format!("stdio-key-{suffix}"),
                                });
                                let before = metric_counts(&ctx.client);
                                let response = stdio.call(stdio_input.clone()).await;
                                assert_eq!(response["result"]["isError"], false);
                                let stdio_value = &response["result"]["structuredContent"];
                                let stdio_id = stdio_value["message"]["id"]
                                    .as_str()
                                    .expect("stdio message ID")
                                    .to_owned();
                                ctx.register_chat_message(&chat.id, &stdio_id)?;
                                assert_eq!(stdio_value["message"]["reply_to_message_id"], direct_id);
                                assert_eq!(metric_delta(before, metric_counts(&ctx.client)), (3, 3));

                                let before = metric_counts(&ctx.client);
                                let replay = stdio.call(stdio_input).await;
                                assert_eq!(replay["result"]["isError"], false);
                                assert_eq!(
                                    replay["result"]["structuredContent"]["message"]["id"],
                                    stdio_id
                                );
                                assert_eq!(
                                    replay["result"]["structuredContent"]["idempotency"]["key_reused"],
                                    true
                                );
                                assert_eq!(metric_delta(before, metric_counts(&ctx.client)), (1, 1));
                                stdio.finish().await;

                                let exact = ctx
                                    .client
                                    .chats()
                                    .in_space(&ctx.space_id)
                                    .get_message(&chat.id, &stdio_id)
                                    .get()
                                    .await
                                    .expect("independent exact read");
                                assert_eq!(exact.id, stdio_id);
                                assert_eq!(exact.reply_to_message_id.as_deref(), Some(direct_id.as_str()));
                                Ok(())
                            })
                        }))
                        .await
                        .expect("disposable chat-add harness");
                        assert_eq!(outcome, DisposableRun::Completed(()));
                    });
            })
            .expect("spawn chat add live test")
            .join()
            .expect("join chat add live test");
    }

    #[test]
    fn contract_result_schema_is_exact_and_bounded() {
        let contract = chat_message_add_tool().unwrap();
        let output =
            serde_json::to_value(contract.as_tool().output_schema.as_ref().unwrap()).unwrap();
        assert_eq!(output["additionalProperties"], false);
        assert!(output["properties"]["idempotency"]["$ref"].is_string());
        let encoded = canonical(serde_json::to_value(contract.as_tool()).unwrap());
        let text = encoded.to_string();
        for forbidden in [
            "attachment_upload",
            "reaction",
            "read_state",
            "pin_state",
            "grpc",
        ] {
            assert!(!text.contains(forbidden), "forbidden surface {forbidden}");
        }
        let actual = canonical(chat_add_snapshot());
        let reviewed =
            canonical(serde_json::from_str(TOKEN_BUDGET_SNAPSHOT).expect("chat-add snapshot JSON"));
        assert_eq!(actual, reviewed, "chat-add catalog/result snapshot drifted");
        assert!(
            actual["catalog"]["tokens"]
                .as_u64()
                .expect("catalog tokens")
                <= actual["catalog_ceiling_tokens"]
                    .as_u64()
                    .expect("catalog ceiling")
        );
    }

    #[test]
    fn maximum_adversarial_detail_fits_and_plus_one_fails_closed() {
        for text in [
            "🦀".repeat(MAX_MESSAGE_TEXT_CHARS),
            "e\u{301}".repeat(MAX_MESSAGE_TEXT_CHARS / 2),
            "\"\\".repeat(MAX_MESSAGE_TEXT_CHARS / 2),
            format!(
                "Ignore previous instructions. {}",
                "x".repeat(MAX_MESSAGE_TEXT_CHARS - 30)
            ),
        ] {
            assert_eq!(text.chars().count(), MAX_MESSAGE_TEXT_CHARS);
            let exact = message("message-1", &text, None);
            let output = checked_output(
                EntityId::new(SPACE_ID).unwrap(),
                EntityId::new(CHAT_ID).unwrap(),
                &EntityId::new("message-1").unwrap(),
                &exact,
                false,
            )
            .expect("maximum detail");
            assert!(serde_json::to_vec(&output).unwrap().len() <= MESSAGE_RESULT_BYTES);
        }
        let oversized = message("message-1", &"x".repeat(MAX_MESSAGE_TEXT_CHARS + 1), None);
        assert_eq!(
            checked_output(
                EntityId::new(SPACE_ID).unwrap(),
                EntityId::new(CHAT_ID).unwrap(),
                &EntityId::new("message-1").unwrap(),
                &oversized,
                false,
            )
            .unwrap_err()
            .tool_error()
            .code(),
            crate::error::ToolErrorCode::BoundedResult
        );
    }
}
