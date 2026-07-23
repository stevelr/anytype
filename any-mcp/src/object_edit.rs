// any-mcp - bounded, workflow-oriented MCP server for Anytype
//
// SPDX-FileCopyrightText: 2026 Steve Schoettler
// SPDX-License-Identifier: Apache-2.0

//! Conflict-safe exact-match document editing.
//!
//! `object_edit` reads one complete bounded Markdown body, verifies the
//! caller's SHA-256 precondition, and applies a bounded list of literal edits
//! in order before sending one whole-body update. Match counting and
//! replacement use the same left-to-right, non-overlapping semantics. Anytype
//! has no atomic compare-and-swap operation, so a best-effort race remains
//! between the precondition read and the update. Hashes and verification stay
//! over the exact canonical GET body; a closed plain-line representation is
//! inverted only for the PATCH wire form so escaped underscores are not
//! double-escaped.

use std::{borrow::Cow, fmt};

use anytype::{
    objects::{Object, plain_markdown_representation},
    prelude::{VerifyConfig, verify_semantic},
};
use rmcp::{
    model::CallToolResult,
    schemars::{JsonSchema, Schema, SchemaGenerator, json_schema},
};
use serde::{Deserialize, Deserializer, Serialize, de};
use tokio_util::sync::CancellationToken;

use crate::{
    domain::{DomainValueError, ObjectId, ObjectSummary, SpaceId},
    error::{ToolError, mutation_rejection_is_definitive},
    handler_support::{
        HandlerError, HandlerOperationError, MutationAccess, MutationProgress,
        execute_mutation_handler, require_mutation_access,
    },
    object_output::object_summary,
    object_read::AnytypeReference,
    object_update::{BodySha256, MAX_UPDATE_MARKDOWN_BYTES, MAX_UPDATE_MARKDOWN_CHARS},
    protocol::{ToolProfile, WorkflowTool, workflow_tool},
    result::tool_error,
    runtime::{OperationContext, RuntimeContext},
    schema::SchemaContractError,
};

/// Maximum number of ordered exact-match edits in one call.
pub const MAX_EXACT_EDITS: usize = 100;
/// Maximum non-overlapping occurrences one edit may replace.
pub const MAX_EXPECTED_MATCHES: usize = 1_000;
/// Maximum Unicode scalar values in either edit fragment.
pub const MAX_EDIT_TEXT_CHARS: usize = MAX_UPDATE_MARKDOWN_CHARS;
/// Maximum UTF-8 bytes in either edit fragment.
pub const MAX_EDIT_TEXT_BYTES: usize = MAX_UPDATE_MARKDOWN_BYTES;

/// A nonempty literal fragment to find in the current intermediate body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct OldText(String);

impl OldText {
    /// Validates a bounded, nonempty literal match fragment.
    pub fn new(value: impl Into<String>) -> Result<Self, EditInputError> {
        let value = value.into();
        if value.is_empty() {
            return Err(EditInputError::EmptyOldText);
        }
        validate_fragment(&value)?;
        Ok(Self(value))
    }

    /// Borrows the literal fragment.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for OldText {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::new(String::deserialize(deserializer)?).map_err(de::Error::custom)
    }
}

impl JsonSchema for OldText {
    fn schema_name() -> Cow<'static, str> {
        "OldText".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "minLength": 1,
            "maxLength": MAX_EDIT_TEXT_CHARS,
        })
    }
}

/// A bounded literal replacement fragment. The empty string deletes matches.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct NewText(String);

impl NewText {
    /// Validates a bounded replacement fragment, including the empty string.
    pub fn new(value: impl Into<String>) -> Result<Self, EditInputError> {
        let value = value.into();
        validate_fragment(&value)?;
        Ok(Self(value))
    }

    /// Borrows the replacement fragment.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for NewText {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::new(String::deserialize(deserializer)?).map_err(de::Error::custom)
    }
}

impl JsonSchema for NewText {
    fn schema_name() -> Cow<'static, str> {
        "NewText".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "maxLength": MAX_EDIT_TEXT_CHARS,
        })
    }
}

/// One literal edit applied after every preceding edit in the request.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ExactEdit {
    /// Nonempty literal text to match using non-overlapping semantics.
    old_text: OldText,
    /// Literal replacement; an empty string deletes every matched occurrence.
    new_text: NewText,
    /// Required occurrence count in the current intermediate body. Defaults to one.
    #[serde(default = "default_expected_matches")]
    #[schemars(schema_with = "expected_matches_schema")]
    expected_matches: usize,
}

const fn default_expected_matches() -> usize {
    1
}

fn expected_matches_schema(_: &mut SchemaGenerator) -> Schema {
    json_schema!({
        "type": "integer",
        "minimum": 1,
        "maximum": MAX_EXPECTED_MATCHES,
        "default": 1,
    })
}

/// Strict exact-edit input.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ObjectEditInput {
    /// Unique space name or safe id.
    space: AnytypeReference,
    /// Stable object id; names are never guessed.
    object_id: ObjectId,
    /// Ordered literal edits. Each edit sees the result of its predecessors.
    #[schemars(length(min = 1, max = MAX_EXACT_EDITS))]
    edits: Vec<ExactEdit>,
    /// SHA-256 of the exact complete body returned by the preceding read.
    expected_body_sha256: BodySha256,
}

/// Verified result of one exact-edit operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ObjectEditOutput {
    /// Bounded read-after-write object summary and canonical resource link.
    object: ObjectSummary,
    /// SHA-256 of the verified complete body after all edits.
    body_sha256: BodySha256,
}

impl ObjectEditOutput {
    /// Borrows the verified updated summary.
    #[must_use]
    pub const fn object(&self) -> &ObjectSummary {
        &self.object
    }

    /// Borrows the verified new complete-body digest.
    #[must_use]
    pub const fn body_sha256(&self) -> &BodySha256 {
        &self.body_sha256
    }
}

/// Invalid typed exact-edit input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditInputError {
    /// An empty find fragment has ambiguous insertion-point semantics.
    EmptyOldText,
    /// A fragment exceeded the documented body bounds.
    BoundedValue,
}

impl fmt::Display for EditInputError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::EmptyOldText => "old_text must not be empty",
            Self::BoundedValue => "edit text exceeds its documented bound",
        })
    }
}

impl std::error::Error for EditInputError {}

/// Builds the strict destructive `object_edit` contract.
pub fn object_edit_tool() -> Result<WorkflowTool<ObjectEditOutput>, SchemaContractError> {
    workflow_tool::<ObjectEditInput, ObjectEditOutput>(
        "object_edit",
        "Apply bounded literal edits in order after verifying the exact complete-body SHA-256. Matches are left-to-right and non-overlapping; expected_matches defaults to 1. Any mismatch writes nothing. A best-effort race remains because Anytype has no atomic compare-and-swap. Returns a summary and new hash, never the body.",
        ToolProfile::Update,
    )
}

struct EditExecution {
    object: Object,
    body_sha256: BodySha256,
}

/// Applies one preflighted whole-body patch and verifies the exact new body.
///
/// The access gate and structural bounds are checked before upstream I/O. The
/// current complete body is bounded and hashed before every ordered match
/// count is checked and every replacement is computed. A stale hash, count
/// mismatch, or oversized intermediate result returns `conflict` or
/// `bounded_result` without polling a write. Anytype lacks atomic CAS, so a
/// concurrent writer can still race between the GET and PATCH.
pub async fn object_edit(
    runtime: &RuntimeContext,
    contract: &WorkflowTool<ObjectEditOutput>,
    access: MutationAccess,
    input: &ObjectEditInput,
    cancellation: &CancellationToken,
) -> CallToolResult {
    if let Err(error) = require_mutation_access(access) {
        return tool_error(error.tool_error());
    }
    if let Err(error) = structural_preflight(input) {
        return tool_error(error.tool_error());
    }

    let client = runtime.client();
    let verification = client
        .get_config()
        .get_verify_config()
        .cloned()
        .unwrap_or_else(VerifyConfig::default);
    let input = input.clone();
    let progress = MutationProgress::new();
    let operation_progress = progress.clone();
    execute_mutation_handler(
        runtime,
        contract,
        OperationContext::new("object_edit"),
        cancellation,
        &progress,
        async move {
            let resolved_space = client.resolve_space_id(input.space.as_str()).await?;
            let space_id = checked_space_id(resolved_space)?;
            let object_id = input.object_id.clone();
            let current = client
                .object(space_id.as_str(), object_id.as_str())
                .get()
                .await?;
            verify_identity(&current, &space_id, &object_id)?;
            if current.archived {
                return Err(HandlerError::new(ToolError::not_found()).into());
            }

            let current_body = current.markdown.as_deref().unwrap_or("");
            validate_complete_body(current_body)?;
            if BodySha256::digest(current_body) != input.expected_body_sha256 {
                return Err(HandlerError::new(ToolError::conflict()).into());
            }
            let edited_body = apply_edits(current_body, &input.edits)?;
            let representation = plain_markdown_representation(&edited_body);
            let expected_body = representation
                .as_ref()
                .map_or(edited_body.as_str(), |representation| {
                    representation.canonical()
                });
            validate_complete_body(expected_body)?;
            let write_body = representation
                .as_ref()
                .map_or(edited_body.as_str(), |representation| representation.wire());
            let expected_hash = BodySha256::digest(expected_body);

            let request = client
                .update_object(space_id.as_str(), object_id.as_str())
                .body(write_body)
                .no_verify();
            operation_progress.mark_dispatched();
            let patch_anomaly = match request.update().await {
                Ok(returned) => {
                    !edited_state_matches(&returned, &space_id, &object_id, &expected_hash)
                        .unwrap_or(false)
                }
                Err(error) if mutation_rejection_is_definitive(&error) => {
                    return Err(error.into());
                }
                Err(_) => true,
            };

            let verified = verify_semantic(
                &verification,
                "object",
                object_id.as_str(),
                || async {
                    client
                        .object(space_id.as_str(), object_id.as_str())
                        .get()
                        .await
                },
                |object| {
                    edited_state_matches(object, &space_id, &object_id, &expected_hash)
                        .unwrap_or(false)
                },
            )
            .await
            .map_err(|_| HandlerError::new(ToolError::mutation_indeterminate()))?;
            if patch_anomaly {
                return Err(HandlerError::new(ToolError::mutation_indeterminate()).into());
            }
            Ok::<_, HandlerOperationError>(EditExecution {
                object: verified,
                body_sha256: expected_hash,
            })
        },
        |execution| async move {
            let object = object_summary(&execution.object)
                .map_err(|_| HandlerError::new(ToolError::mutation_indeterminate()))?;
            Ok(ObjectEditOutput {
                object,
                body_sha256: execution.body_sha256,
            })
        },
    )
    .await
}

fn validate_fragment(value: &str) -> Result<(), EditInputError> {
    if value.len() > MAX_EDIT_TEXT_BYTES || value.chars().count() > MAX_EDIT_TEXT_CHARS {
        return Err(EditInputError::BoundedValue);
    }
    Ok(())
}

fn structural_preflight(input: &ObjectEditInput) -> Result<(), HandlerError> {
    if input.edits.is_empty() || input.edits.len() > MAX_EXACT_EDITS {
        return Err(HandlerError::new(ToolError::validation()));
    }
    if input
        .edits
        .iter()
        .any(|edit| edit.expected_matches == 0 || edit.expected_matches > MAX_EXPECTED_MATCHES)
    {
        return Err(HandlerError::new(ToolError::validation()));
    }
    Ok(())
}

fn apply_edits(initial: &str, edits: &[ExactEdit]) -> Result<String, HandlerError> {
    let mut body = initial.to_owned();
    for edit in edits {
        let old = edit.old_text.as_str();
        let new = edit.new_text.as_str();
        let actual_matches = body
            .match_indices(old)
            .take(edit.expected_matches.saturating_add(1))
            .count();
        if actual_matches != edit.expected_matches {
            return Err(HandlerError::new(ToolError::conflict()));
        }

        let removed_bytes = old
            .len()
            .checked_mul(edit.expected_matches)
            .ok_or_else(bounded_result)?;
        let added_bytes = new
            .len()
            .checked_mul(edit.expected_matches)
            .ok_or_else(bounded_result)?;
        let next_bytes = body
            .len()
            .checked_sub(removed_bytes)
            .and_then(|value| value.checked_add(added_bytes))
            .ok_or_else(bounded_result)?;

        let current_chars = body.chars().count();
        let removed_chars = old
            .chars()
            .count()
            .checked_mul(edit.expected_matches)
            .ok_or_else(bounded_result)?;
        let added_chars = new
            .chars()
            .count()
            .checked_mul(edit.expected_matches)
            .ok_or_else(bounded_result)?;
        let next_chars = current_chars
            .checked_sub(removed_chars)
            .and_then(|value| value.checked_add(added_chars))
            .ok_or_else(bounded_result)?;
        if next_bytes > MAX_UPDATE_MARKDOWN_BYTES || next_chars > MAX_UPDATE_MARKDOWN_CHARS {
            return Err(bounded_result());
        }
        body = body.replace(old, new);
    }
    Ok(body)
}

fn validate_complete_body(body: &str) -> Result<(), HandlerError> {
    if body.len() > MAX_UPDATE_MARKDOWN_BYTES || body.chars().count() > MAX_UPDATE_MARKDOWN_CHARS {
        return Err(HandlerError::new(ToolError::bounded_result()));
    }
    Ok(())
}

fn bounded_result() -> HandlerError {
    HandlerError::new(ToolError::bounded_result())
}

fn checked_space_id(value: String) -> Result<SpaceId, HandlerError> {
    SpaceId::new(value).map_err(upstream_domain)
}

fn upstream_domain(error: DomainValueError) -> HandlerError {
    match error {
        DomainValueError::TooLong { .. } => HandlerError::new(ToolError::bounded_result()),
        DomainValueError::Empty | DomainValueError::InvalidIdentifierCharacter => {
            HandlerError::new(ToolError::upstream())
        }
    }
}

fn verify_identity(
    object: &Object,
    space_id: &SpaceId,
    object_id: &ObjectId,
) -> Result<(), HandlerError> {
    let returned_id = ObjectId::new(object.id.clone()).map_err(upstream_domain)?;
    let returned_space = SpaceId::new(object.space_id.clone()).map_err(upstream_domain)?;
    if &returned_id != object_id || &returned_space != space_id {
        return Err(HandlerError::new(ToolError::upstream()));
    }
    Ok(())
}

fn edited_state_matches(
    object: &Object,
    space_id: &SpaceId,
    object_id: &ObjectId,
    expected_hash: &BodySha256,
) -> Result<bool, HandlerError> {
    verify_identity(object, space_id, object_id)?;
    if object.archived {
        return Ok(false);
    }
    let body = object.markdown.as_deref().unwrap_or("");
    validate_complete_body(body)?;
    Ok(BodySha256::digest(body) == *expected_hash)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use anytype::prelude::{AnytypeClient, ClientConfig, HttpCredentials, ResponseLimits};
    use serde_json::{Value, json};
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::{TcpListener, TcpStream},
        sync::oneshot,
        task::JoinHandle,
    };

    use super::*;
    use crate::{error::ToolErrorCode, runtime::StartupStatus};

    const SPACE_ID: &str =
        "bafyreid5fvqlnsobih2keakcxjrrlpmly6kf37klzjzen4ibfdgalcdp4y.2tq5w93cr6oe7";
    const OBJECT_ID: &str = "bafyreie6n5l5nkbjal37su54cha4coy7qzuhrnajluzv5qd5jvtsrxkequ";
    const OTHER_OBJECT_ID: &str = "bafyreiaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const TYPE_ID: &str = "bafyreibbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    struct FixtureReply {
        status: &'static str,
        body: String,
        headers: String,
        delay: Duration,
        disconnect: bool,
    }

    impl FixtureReply {
        fn json(body: Value) -> Self {
            Self {
                status: "200 OK",
                body: body.to_string(),
                headers: String::new(),
                delay: Duration::ZERO,
                disconnect: false,
            }
        }

        fn error(status: &'static str, body: &str) -> Self {
            Self {
                status,
                body: body.to_owned(),
                headers: String::new(),
                delay: Duration::ZERO,
                disconnect: false,
            }
        }

        fn redirect(status: &'static str, location: &str) -> Self {
            Self {
                status,
                body: "{}".to_owned(),
                headers: format!("Location: {location}\r\n"),
                delay: Duration::ZERO,
                disconnect: false,
            }
        }

        fn disconnect() -> Self {
            Self {
                status: "200 OK",
                body: String::new(),
                headers: String::new(),
                delay: Duration::ZERO,
                disconnect: true,
            }
        }

        fn delayed(mut self, delay: Duration) -> Self {
            self.delay = delay;
            self
        }
    }

    async fn fixture(replies: Vec<FixtureReply>) -> (String, JoinHandle<Vec<String>>) {
        let (base_url, server, _) = fixture_with_signal(replies, None).await;
        (base_url, server)
    }

    async fn fixture_with_signal(
        replies: Vec<FixtureReply>,
        signal_request: Option<usize>,
    ) -> (
        String,
        JoinHandle<Vec<String>>,
        Option<oneshot::Receiver<()>>,
    ) {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind edit fixture");
        let address = listener.local_addr().expect("edit fixture address");
        let (signal_tx, signal_rx) = oneshot::channel();
        let mut signal_tx = signal_request.map(|_| signal_tx);
        let server = tokio::spawn(async move {
            let mut requests = Vec::with_capacity(replies.len());
            for (index, reply) in replies.into_iter().enumerate() {
                let (mut socket, _) = listener.accept().await.expect("accept edit request");
                requests.push(read_request(&mut socket).await);
                if signal_request == Some(index + 1)
                    && let Some(signal_tx) = signal_tx.take()
                {
                    let _ = signal_tx.send(());
                }
                tokio::time::sleep(reply.delay).await;
                if reply.disconnect {
                    continue;
                }
                let response = format!(
                    "HTTP/1.1 {}\r\nContent-Type: application/json\r\n{}Content-Length: {}\r\nConnection: close\r\n\r\n{}",
                    reply.status,
                    reply.headers,
                    reply.body.len(),
                    reply.body,
                );
                let _ = socket.write_all(response.as_bytes()).await;
            }
            requests
        });
        (
            format!("http://{address}"),
            server,
            signal_request.map(|_| signal_rx),
        )
    }

    async fn read_request(socket: &mut TcpStream) -> String {
        let mut request = Vec::new();
        let mut buffer = [0_u8; 2048];
        let (header_end, content_length) = loop {
            let read = socket.read(&mut buffer).await.expect("read edit request");
            assert_ne!(read, 0, "request ended before its headers");
            request.extend_from_slice(&buffer[..read]);
            if let Some(index) = request.windows(4).position(|window| window == b"\r\n\r\n") {
                let header_end = index + 4;
                let headers = String::from_utf8_lossy(&request[..header_end]);
                let content_length = headers
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .map(str::trim)
                            .and_then(|value| value.parse::<usize>().ok())
                    })
                    .unwrap_or(0);
                break (header_end, content_length);
            }
        };
        while request.len() < header_end + content_length {
            let read = socket.read(&mut buffer).await.expect("read request body");
            assert_ne!(read, 0, "request ended before its body");
            request.extend_from_slice(&buffer[..read]);
        }
        String::from_utf8(request).expect("request is utf-8")
    }

    async fn no_request_fixture() -> (String, JoinHandle<bool>) {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind no-request fixture");
        let address = listener.local_addr().expect("no-request fixture address");
        let server = tokio::spawn(async move {
            tokio::time::timeout(Duration::from_millis(150), listener.accept())
                .await
                .is_err()
        });
        (format!("http://{address}"), server)
    }

    async fn monitored_no_request_fixture() -> (String, oneshot::Sender<()>, JoinHandle<bool>) {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind monitored target");
        let address = listener.local_addr().expect("target address");
        let (done_tx, done_rx) = oneshot::channel();
        let server = tokio::spawn(async move {
            tokio::select! {
                _ = listener.accept() => false,
                _ = done_rx => true,
            }
        });
        (format!("http://{address}"), done_tx, server)
    }

    fn runtime(base_url: String, timeout: Duration) -> RuntimeContext {
        runtime_with_options(base_url, timeout, ResponseLimits::default(), None)
    }

    fn runtime_with_options(
        base_url: String,
        timeout: Duration,
        response_limits: ResponseLimits,
        verify: Option<VerifyConfig>,
    ) -> RuntimeContext {
        let client = AnytypeClient::with_config(ClientConfig {
            base_url: Some(base_url),
            keystore: Some("env".to_owned()),
            keystore_service: Some("object-edit-test".to_owned()),
            app_name: "object-edit-test".to_owned(),
            response_limits,
            verify,
            disable_cache: true,
            ..ClientConfig::default()
        })
        .expect("edit fixture client");
        client.set_api_key(HttpCredentials::new("fixture-token"));
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

    fn input(value: Value) -> ObjectEditInput {
        serde_json::from_value(value).expect("valid edit input")
    }

    fn edit_input(body: &str, edits: Value) -> ObjectEditInput {
        input(json!({
            "space": SPACE_ID,
            "object_id": OBJECT_ID,
            "edits": edits,
            "expected_body_sha256": BodySha256::digest(body).as_str(),
        }))
    }

    fn object(space_id: &str, object_id: &str, body: &str) -> Value {
        json!({
            "object": {
                "archived": false,
                "id": object_id,
                "space_id": space_id,
                "name": "Document",
                "markdown": body,
                "type": {
                    "archived": false,
                    "id": TYPE_ID,
                    "key": "page"
                }
            }
        })
    }

    fn request_body(request: &str) -> Value {
        let (_, body) = request.split_once("\r\n\r\n").expect("request body");
        serde_json::from_str(body).expect("JSON request body")
    }

    fn result_code(result: &CallToolResult) -> &str {
        result
            .structured_content
            .as_ref()
            .and_then(|value| value.get("code"))
            .and_then(Value::as_str)
            .expect("error code")
    }

    fn result_message(result: &CallToolResult) -> &str {
        result
            .structured_content
            .as_ref()
            .and_then(|value| value.get("message"))
            .and_then(Value::as_str)
            .expect("error message")
    }

    fn fast_verify(max_attempts: usize) -> VerifyConfig {
        VerifyConfig {
            timeout: Duration::from_secs(1),
            initial_delay: Duration::ZERO,
            max_delay: Duration::ZERO,
            max_attempts,
        }
    }

    #[test]
    fn contract_is_closed_bounded_destructive_and_defaults_match_count() {
        let tool = object_edit_tool().expect("valid edit tool");
        assert_eq!(tool.as_tool().name, "object_edit");
        let annotations = serde_json::to_value(tool.as_tool().annotations.as_ref().unwrap())
            .expect("serialize annotations");
        assert_eq!(
            annotations,
            json!({
                "readOnlyHint": false,
                "destructiveHint": true,
                "idempotentHint": false,
                "openWorldHint": false
            })
        );
        let schema = serde_json::to_value(&tool.as_tool().input_schema).unwrap();
        let encoded = schema.to_string();
        assert!(encoded.contains(&format!("\"maxItems\":{MAX_EXACT_EDITS}")));
        assert!(encoded.contains(&format!("\"maximum\":{MAX_EXPECTED_MATCHES}")));
        assert!(encoded.contains("\"additionalProperties\":false"));

        let parsed = edit_input("a", json!([{"old_text":"a","new_text":"b"}]));
        assert_eq!(parsed.edits[0].expected_matches, 1);
        for invalid in [
            json!({
                "space": SPACE_ID,
                "object_id": OBJECT_ID,
                "edits": [{"old_text":"","new_text":"b"}],
                "expected_body_sha256": BodySha256::digest("a").as_str(),
            }),
            json!({
                "space": SPACE_ID,
                "object_id": OBJECT_ID,
                "edits": [{"old_text":"a","new_text":"b","extra":true}],
                "expected_body_sha256": BodySha256::digest("a").as_str(),
            }),
            json!({
                "space": SPACE_ID,
                "object_id": OBJECT_ID,
                "edits": null,
                "expected_body_sha256": BodySha256::digest("a").as_str(),
            }),
            json!({
                "space": SPACE_ID,
                "object_id": OBJECT_ID,
                "edits": [{"old_text":"a","new_text":null}],
                "expected_body_sha256": BodySha256::digest("a").as_str(),
            }),
        ] {
            assert!(serde_json::from_value::<ObjectEditInput>(invalid).is_err());
        }
    }

    #[test]
    fn ordered_non_overlapping_unicode_and_deletion_semantics_are_exact() {
        let edits = vec![
            ExactEdit {
                old_text: OldText::new("aba").unwrap(),
                new_text: NewText::new("X").unwrap(),
                expected_matches: 1,
            },
            ExactEdit {
                old_text: OldText::new("Xba").unwrap(),
                new_text: NewText::new("🦀").unwrap(),
                expected_matches: 1,
            },
            ExactEdit {
                old_text: OldText::new("🦀").unwrap(),
                new_text: NewText::new("").unwrap(),
                expected_matches: 3,
            },
        ];
        assert_eq!(apply_edits("ababa 🦀 🦀", &edits).unwrap(), "  ");

        let overlapping = vec![ExactEdit {
            old_text: OldText::new("aba").unwrap(),
            new_text: NewText::new("X").unwrap(),
            expected_matches: 2,
        }];
        assert_eq!(
            apply_edits("ababa", &overlapping)
                .unwrap_err()
                .tool_error()
                .code(),
            ToolErrorCode::Conflict
        );
    }

    #[test]
    fn structural_and_intermediate_limits_fail_before_allocation() {
        for count in [0, MAX_EXPECTED_MATCHES + 1] {
            let input = edit_input(
                "a",
                json!([{"old_text":"a","new_text":"b","expected_matches":count}]),
            );
            assert_eq!(
                structural_preflight(&input)
                    .unwrap_err()
                    .tool_error()
                    .code(),
                ToolErrorCode::Validation
            );
        }

        let too_many = (0..=MAX_EXACT_EDITS)
            .map(|_| json!({"old_text":"a","new_text":"b"}))
            .collect::<Vec<_>>();
        let input = edit_input("a", Value::Array(too_many));
        assert_eq!(
            structural_preflight(&input)
                .unwrap_err()
                .tool_error()
                .code(),
            ToolErrorCode::Validation
        );

        let expansion = vec![ExactEdit {
            old_text: OldText::new("a").unwrap(),
            new_text: NewText::new("x".repeat(MAX_EDIT_TEXT_CHARS)).unwrap(),
            expected_matches: 2,
        }];
        assert_eq!(
            apply_edits("aa", &expansion)
                .unwrap_err()
                .tool_error()
                .code(),
            ToolErrorCode::BoundedResult
        );
        assert!(OldText::new("").is_err());
        assert!(NewText::new("🦀".repeat(MAX_EDIT_TEXT_CHARS + 1)).is_err());

        let max_count = vec![ExactEdit {
            old_text: OldText::new("a").unwrap(),
            new_text: NewText::new("").unwrap(),
            expected_matches: MAX_EXPECTED_MATCHES,
        }];
        assert_eq!(
            apply_edits(&"a".repeat(MAX_EXPECTED_MATCHES), &max_count).unwrap(),
            ""
        );
        let max_edits = (0..MAX_EXACT_EDITS)
            .map(|_| ExactEdit {
                old_text: OldText::new("a").unwrap(),
                new_text: NewText::new("a").unwrap(),
                expected_matches: 1,
            })
            .collect::<Vec<_>>();
        let input = ObjectEditInput {
            space: AnytypeReference::new(SPACE_ID).unwrap(),
            object_id: ObjectId::new(OBJECT_ID).unwrap(),
            edits: max_edits.clone(),
            expected_body_sha256: BodySha256::digest("a"),
        };
        assert!(structural_preflight(&input).is_ok());
        assert_eq!(apply_edits("a", &max_edits).unwrap(), "a");
        assert!(validate_complete_body(&"x".repeat(MAX_UPDATE_MARKDOWN_CHARS)).is_ok());

        let raw_boundary = format!("{}_", "a".repeat(MAX_UPDATE_MARKDOWN_CHARS - 1));
        let representation =
            plain_markdown_representation(&raw_boundary).expect("closed boundary form");
        assert!(
            validate_complete_body(representation.canonical()).is_err(),
            "canonical underscore escape and suffix must remain inside the body ceiling"
        );
    }

    #[tokio::test]
    async fn read_only_and_empty_edits_reject_before_any_io() {
        let cases = [
            (
                MutationAccess::ReadOnly,
                edit_input("a", json!([{"old_text":"a","new_text":"b"}])),
            ),
            (MutationAccess::Allowed, edit_input("a", json!([]))),
        ];
        for (access, input) in cases {
            let (base_url, server) = no_request_fixture().await;
            let result = object_edit(
                &runtime(base_url, Duration::from_secs(1)),
                &object_edit_tool().unwrap(),
                access,
                &input,
                &CancellationToken::new(),
            )
            .await;
            assert_eq!(result_code(&result), "validation");
            assert!(server.await.expect("no request"));
        }
    }

    #[tokio::test]
    async fn stale_hash_and_match_mismatch_return_conflict_without_patch() {
        for input in [
            edit_input("stale", json!([{"old_text":"old","new_text":"new"}])),
            edit_input(
                "old body",
                json!([{"old_text":"old","new_text":"new","expected_matches":2}]),
            ),
            edit_input(
                "a",
                json!([
                    {"old_text":"a","new_text":"b"},
                    {"old_text":"a","new_text":"c"}
                ]),
            ),
        ] {
            let (base_url, server) = fixture(vec![FixtureReply::json(object(
                SPACE_ID, OBJECT_ID, "old body",
            ))])
            .await;
            let result = object_edit(
                &runtime(base_url, Duration::from_secs(1)),
                &object_edit_tool().unwrap(),
                MutationAccess::Allowed,
                &input,
                &CancellationToken::new(),
            )
            .await;
            assert_eq!(result_code(&result), "conflict");
            assert_ne!(
                result_message(&result),
                ToolError::mutation_indeterminate().message()
            );
            let requests = server.await.expect("preflight fixture");
            assert_eq!(requests.len(), 1);
            assert!(requests[0].starts_with("GET "));
        }
    }

    #[tokio::test]
    async fn success_sends_one_whole_body_patch_and_returns_only_summary_and_hash() {
        let before = "alpha 🦀 alpha";
        let after = "beta 🦀 beta";
        let current = object(SPACE_ID, OBJECT_ID, before);
        let updated = object(SPACE_ID, OBJECT_ID, after);
        let (base_url, server) = fixture(vec![
            FixtureReply::json(current),
            FixtureReply::json(updated.clone()),
            FixtureReply::json(updated),
        ])
        .await;
        let result = object_edit(
            &runtime(base_url, Duration::from_secs(1)),
            &object_edit_tool().unwrap(),
            MutationAccess::Allowed,
            &edit_input(
                before,
                json!([{"old_text":"alpha","new_text":"beta","expected_matches":2}]),
            ),
            &CancellationToken::new(),
        )
        .await;
        assert_eq!(result.is_error, Some(false), "{result:?}");
        let structured = result.structured_content.as_ref().unwrap();
        assert_eq!(
            structured.get("body_sha256").and_then(Value::as_str),
            Some(BodySha256::digest(after).as_str())
        );
        let encoded = structured.to_string();
        assert!(!encoded.contains(before));
        assert!(!encoded.contains(after));
        assert!(!encoded.contains("markdown"));
        assert!(encoded.contains(&format!("anytype://spaces/{SPACE_ID}/objects/{OBJECT_ID}")));

        let requests = server.await.expect("successful edit fixture");
        assert_eq!(requests.len(), 3);
        assert_eq!(
            requests
                .iter()
                .filter(|request| request.starts_with("PATCH "))
                .count(),
            1
        );
        assert_eq!(request_body(&requests[1]), json!({"markdown":after}));
    }

    #[tokio::test]
    async fn canonical_plain_body_with_unique_suffix_round_trips_without_double_escape() {
        let before = "alpha arbitrary body 123\\_0   \n";
        let after = "alpha verified body 123\\_0   \n";
        let current = object(SPACE_ID, OBJECT_ID, before);
        let updated = object(SPACE_ID, OBJECT_ID, after);
        let (base_url, server) = fixture(vec![
            FixtureReply::json(current),
            FixtureReply::json(updated.clone()),
            FixtureReply::json(updated),
        ])
        .await;
        let result = object_edit(
            &runtime(base_url, Duration::from_secs(1)),
            &object_edit_tool().unwrap(),
            MutationAccess::Allowed,
            &edit_input(
                before,
                json!([{"old_text":"arbitrary","new_text":"verified"}]),
            ),
            &CancellationToken::new(),
        )
        .await;
        assert_eq!(result.is_error, Some(false), "{result:?}");
        assert_eq!(
            result
                .structured_content
                .as_ref()
                .and_then(|value| value.get("body_sha256"))
                .and_then(Value::as_str),
            Some(BodySha256::digest(after).as_str())
        );

        let requests = server.await.expect("canonical edit fixture");
        assert_eq!(requests.len(), 3);
        assert_eq!(
            request_body(&requests[1]),
            json!({"markdown":"alpha verified body 123_0"})
        );
    }

    #[tokio::test]
    async fn canonical_plain_body_stale_hash_still_rejects_without_patch() {
        let current = "alpha arbitrary body 123\\_0   \n";
        let (base_url, server) = fixture(vec![FixtureReply::json(object(
            SPACE_ID, OBJECT_ID, current,
        ))])
        .await;
        let result = object_edit(
            &runtime(base_url, Duration::from_secs(1)),
            &object_edit_tool().unwrap(),
            MutationAccess::Allowed,
            &edit_input(
                "stale canonical body",
                json!([{"old_text":"arbitrary","new_text":"verified"}]),
            ),
            &CancellationToken::new(),
        )
        .await;
        assert_eq!(result_code(&result), "conflict");
        assert_ne!(
            result_message(&result),
            ToolError::mutation_indeterminate().message()
        );
        let requests = server.await.expect("canonical stale fixture");
        assert_eq!(requests.len(), 1);
        assert!(requests[0].starts_with("GET "));
    }

    #[tokio::test]
    async fn malformed_identity_and_oversized_current_body_never_patch() {
        let too_large = "x".repeat(MAX_UPDATE_MARKDOWN_CHARS + 1);
        for reply in [
            object(SPACE_ID, OTHER_OBJECT_ID, "old"),
            object(SPACE_ID, OBJECT_ID, &too_large),
        ] {
            let (base_url, server) = fixture(vec![FixtureReply::json(reply)]).await;
            let result = object_edit(
                &runtime(base_url, Duration::from_secs(1)),
                &object_edit_tool().unwrap(),
                MutationAccess::Allowed,
                &edit_input("old", json!([{"old_text":"old","new_text":"new"}])),
                &CancellationToken::new(),
            )
            .await;
            assert_eq!(result.is_error, Some(true));
            assert_eq!(server.await.expect("pre-patch failure").len(), 1);
        }
    }

    #[tokio::test]
    async fn unsafe_resolved_space_is_rejected_before_object_or_patch_io() {
        let unsafe_space = json!({
            "items": [{
                "id": "../unsafe",
                "name": "Workspace",
                "object": "space",
                "description": null,
                "icon": null,
                "gateway_url": null,
                "network_id": null
            }],
            "pagination": {"has_more":false,"limit":100,"offset":0,"total":1}
        });
        let (base_url, server) = fixture(vec![FixtureReply::json(unsafe_space)]).await;
        let input = input(json!({
            "space": "Workspace",
            "object_id": OBJECT_ID,
            "edits": [{"old_text":"old","new_text":"new"}],
            "expected_body_sha256": BodySha256::digest("old").as_str(),
        }));
        let result = object_edit(
            &runtime(base_url, Duration::from_secs(1)),
            &object_edit_tool().unwrap(),
            MutationAccess::Allowed,
            &input,
            &CancellationToken::new(),
        )
        .await;
        assert_eq!(result_code(&result), "authentication");
        let requests = server.await.expect("unsafe resolver fixture");
        assert_eq!(requests.len(), 1);
        assert!(requests[0].starts_with("GET "));
    }

    #[tokio::test]
    async fn verification_converges_but_exhaustion_and_malformed_results_are_indeterminate() {
        let current = object(SPACE_ID, OBJECT_ID, "old");
        let updated = object(SPACE_ID, OBJECT_ID, "new   \n");
        let (base_url, server) = fixture(vec![
            FixtureReply::json(current.clone()),
            FixtureReply::json(updated.clone()),
            FixtureReply::json(current.clone()),
            FixtureReply::json(updated.clone()),
        ])
        .await;
        let result = object_edit(
            &runtime_with_options(
                base_url,
                Duration::from_secs(1),
                ResponseLimits::default(),
                Some(fast_verify(2)),
            ),
            &object_edit_tool().unwrap(),
            MutationAccess::Allowed,
            &edit_input("old", json!([{"old_text":"old","new_text":"new"}])),
            &CancellationToken::new(),
        )
        .await;
        assert_eq!(result.is_error, Some(false));
        assert_eq!(server.await.expect("convergence fixture").len(), 4);

        for replies in [
            vec![
                FixtureReply::json(current.clone()),
                FixtureReply::json(updated.clone()),
                FixtureReply::json(current.clone()),
                FixtureReply::json(current.clone()),
            ],
            vec![
                FixtureReply::json(current.clone()),
                FixtureReply::json(updated.clone()),
                FixtureReply::error("200 OK", "secret malformed body"),
            ],
        ] {
            let expected_requests = replies.len();
            let (base_url, server) = fixture(replies).await;
            let result = object_edit(
                &runtime_with_options(
                    base_url,
                    Duration::from_secs(1),
                    ResponseLimits::default(),
                    Some(fast_verify(2)),
                ),
                &object_edit_tool().unwrap(),
                MutationAccess::Allowed,
                &edit_input("old", json!([{"old_text":"old","new_text":"new"}])),
                &CancellationToken::new(),
            )
            .await;
            assert_eq!(result_code(&result), "conflict");
            assert_eq!(
                result_message(&result),
                ToolError::mutation_indeterminate().message()
            );
            assert!(!serde_json::to_string(&result).unwrap().contains("secret"));
            assert_eq!(
                server.await.expect("failed verify fixture").len(),
                expected_requests
            );
        }
    }

    #[tokio::test]
    async fn patch_429_is_ordinary_and_sent_exactly_once() {
        let current = object(SPACE_ID, OBJECT_ID, "old");
        let (base_url, server) = fixture(vec![
            FixtureReply::json(current),
            FixtureReply::error("429 Too Many Requests", "secret rate detail"),
        ])
        .await;
        let result = object_edit(
            &runtime(base_url, Duration::from_secs(1)),
            &object_edit_tool().unwrap(),
            MutationAccess::Allowed,
            &edit_input("old", json!([{"old_text":"old","new_text":"new"}])),
            &CancellationToken::new(),
        )
        .await;
        assert_eq!(result_code(&result), "upstream");
        assert!(
            !serde_json::to_string(&result)
                .unwrap()
                .contains("secret rate")
        );
        let requests = server.await.expect("429 fixture");
        assert_eq!(requests.len(), 2);
        assert_eq!(
            requests
                .iter()
                .filter(|request| request.starts_with("PATCH "))
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn patch_408_or_error_is_indeterminate_even_when_recovery_matches() {
        let current = object(SPACE_ID, OBJECT_ID, "old");
        let updated = object(SPACE_ID, OBJECT_ID, "new");
        for patch_reply in [
            FixtureReply::error("408 Request Timeout", "secret timeout"),
            FixtureReply::error("500 Internal Server Error", "secret server body"),
            FixtureReply::error("200 OK", "secret malformed response"),
            FixtureReply::disconnect(),
        ] {
            let (base_url, server) = fixture(vec![
                FixtureReply::json(current.clone()),
                patch_reply,
                FixtureReply::json(updated.clone()),
            ])
            .await;
            let result = object_edit(
                &runtime_with_options(
                    base_url,
                    Duration::from_secs(1),
                    ResponseLimits::default(),
                    Some(fast_verify(1)),
                ),
                &object_edit_tool().unwrap(),
                MutationAccess::Allowed,
                &edit_input("old", json!([{"old_text":"old","new_text":"new"}])),
                &CancellationToken::new(),
            )
            .await;
            assert_eq!(result_code(&result), "conflict");
            assert_eq!(
                result_message(&result),
                ToolError::mutation_indeterminate().message()
            );
            assert!(!serde_json::to_string(&result).unwrap().contains("secret"));
            assert_eq!(server.await.expect("patch anomaly fixture").len(), 3);
        }
    }

    #[tokio::test]
    async fn oversized_patch_response_is_indeterminate_after_matching_recovery() {
        let current = object(SPACE_ID, OBJECT_ID, "old");
        let updated = object(SPACE_ID, OBJECT_ID, "new");
        let oversized = object(SPACE_ID, OBJECT_ID, &"x".repeat(2_000));
        let (base_url, server) = fixture(vec![
            FixtureReply::json(current),
            FixtureReply::json(oversized),
            FixtureReply::json(updated),
        ])
        .await;
        let limits = ResponseLimits {
            json_bytes: 512,
            document_bytes: 512,
            error_bytes: 512,
            file_bytes: 512,
            chat_sse_event_bytes: 512,
        };
        let result = object_edit(
            &runtime_with_options(
                base_url,
                Duration::from_secs(1),
                limits,
                Some(fast_verify(1)),
            ),
            &object_edit_tool().unwrap(),
            MutationAccess::Allowed,
            &edit_input("old", json!([{"old_text":"old","new_text":"new"}])),
            &CancellationToken::new(),
        )
        .await;
        assert_eq!(result_code(&result), "conflict");
        assert_eq!(
            result_message(&result),
            ToolError::mutation_indeterminate().message()
        );
        assert_eq!(server.await.expect("oversized patch fixture").len(), 3);
    }

    #[tokio::test]
    async fn patch_redirect_is_not_followed_and_stays_indeterminate() {
        let current = object(SPACE_ID, OBJECT_ID, "old");
        let updated = object(SPACE_ID, OBJECT_ID, "new");
        let (target, target_done, target_server) = monitored_no_request_fixture().await;
        let (base_url, server) = fixture(vec![
            FixtureReply::json(current),
            FixtureReply::redirect("307 Temporary Redirect", &target),
            FixtureReply::json(updated),
        ])
        .await;
        let result = object_edit(
            &runtime_with_options(
                base_url,
                Duration::from_secs(1),
                ResponseLimits::default(),
                Some(fast_verify(1)),
            ),
            &object_edit_tool().unwrap(),
            MutationAccess::Allowed,
            &edit_input("old", json!([{"old_text":"old","new_text":"new"}])),
            &CancellationToken::new(),
        )
        .await;
        assert_eq!(result_code(&result), "conflict");
        assert_eq!(server.await.expect("redirect fixture").len(), 3);
        let _ = target_done.send(());
        assert!(target_server.await.expect("redirect target"));
    }

    #[tokio::test]
    async fn post_dispatch_timeout_cancellation_and_shutdown_are_indeterminate() {
        for mode in 0..3 {
            let current = object(SPACE_ID, OBJECT_ID, "old");
            let updated = object(SPACE_ID, OBJECT_ID, "new");
            let (base_url, server, patch_seen) = fixture_with_signal(
                vec![
                    FixtureReply::json(current),
                    FixtureReply::json(updated).delayed(Duration::from_millis(200)),
                ],
                Some(2),
            )
            .await;
            let runtime = runtime(
                base_url,
                if mode == 0 {
                    Duration::from_millis(20)
                } else {
                    Duration::from_secs(1)
                },
            );
            let cancellation = CancellationToken::new();
            let control_runtime = runtime.clone();
            let control_cancellation = cancellation.clone();
            let control = tokio::spawn(async move {
                let _ = patch_seen.expect("patch signal").await;
                match mode {
                    1 => control_cancellation.cancel(),
                    2 => control_runtime.begin_shutdown(),
                    _ => {}
                }
            });
            let result = object_edit(
                &runtime,
                &object_edit_tool().unwrap(),
                MutationAccess::Allowed,
                &edit_input("old", json!([{"old_text":"old","new_text":"new"}])),
                &cancellation,
            )
            .await;
            control.await.expect("control task");
            assert_eq!(result_code(&result), "conflict");
            assert_eq!(
                result_message(&result),
                ToolError::mutation_indeterminate().message()
            );
            let requests = server.await.expect("controlled fixture");
            assert_eq!(requests.len(), 2);
            assert_eq!(
                requests
                    .iter()
                    .filter(|request| request.starts_with("PATCH "))
                    .count(),
                1
            );
        }
    }

    #[tokio::test]
    async fn pre_io_cancellation_and_document_response_ceiling_fail_safely() {
        let (base_url, server) = no_request_fixture().await;
        let cancellation = CancellationToken::new();
        cancellation.cancel();
        let result = object_edit(
            &runtime(base_url, Duration::from_secs(1)),
            &object_edit_tool().unwrap(),
            MutationAccess::Allowed,
            &edit_input("old", json!([{"old_text":"old","new_text":"new"}])),
            &cancellation,
        )
        .await;
        assert_eq!(result_code(&result), "upstream");
        assert!(server.await.expect("cancelled before I/O"));

        let (base_url, server) =
            fixture(vec![FixtureReply::json(object(SPACE_ID, OBJECT_ID, "old"))]).await;
        let limits = ResponseLimits {
            json_bytes: 64,
            document_bytes: 64,
            error_bytes: 64,
            file_bytes: 64,
            chat_sse_event_bytes: 64,
        };
        let result = object_edit(
            &runtime_with_options(base_url, Duration::from_secs(1), limits, None),
            &object_edit_tool().unwrap(),
            MutationAccess::Allowed,
            &edit_input("old", json!([{"old_text":"old","new_text":"new"}])),
            &CancellationToken::new(),
        )
        .await;
        assert_eq!(result_code(&result), "bounded_result");
        assert_eq!(server.await.expect("ceiling fixture").len(), 1);
    }

    #[tokio::test]
    #[ignore = "requires a configured live Anytype server"]
    async fn live_edit_round_trip_is_cleanup_safe() {
        anytype::test_util::with_test_context_unit(|ctx| async move {
            let before = "live alpha body";
            let created = ctx
                .client
                .new_object(&ctx.space_id, "page")
                .name(format!(
                    "any-mcp object-edit {}",
                    anytype::test_util::unique_suffix()
                ))
                .body(before)
                .create()
                .await
                .expect("create live edit object");
            ctx.register_object(&created.id);

            let current = ctx
                .client
                .object(&ctx.space_id, &created.id)
                .get()
                .await
                .expect("read exact current live body");
            let exact_body = current.markdown.as_deref().unwrap_or("");
            let expected_after = exact_body.replace("alpha", "beta");
            let input = input(json!({
                "space": ctx.space_id.as_str(),
                "object_id": created.id.as_str(),
                "edits": [{"old_text":"alpha","new_text":"beta"}],
                "expected_body_sha256": BodySha256::digest(exact_body).as_str(),
            }));
            let runtime = RuntimeContext::from_parts(
                ctx.client.clone(),
                1,
                Duration::from_secs(10),
                StartupStatus {
                    http_available: true,
                    grpc_available: true,
                },
            );
            let result = object_edit(
                &runtime,
                &object_edit_tool().unwrap(),
                MutationAccess::Allowed,
                &input,
                &CancellationToken::new(),
            )
            .await;
            assert_eq!(result.is_error, Some(false), "{result:?}");
            assert_eq!(
                result
                    .structured_content
                    .as_ref()
                    .and_then(|value| value.get("body_sha256"))
                    .and_then(Value::as_str),
                Some(BodySha256::digest(&expected_after).as_str())
            );
            let verified = ctx
                .client
                .object(&ctx.space_id, &created.id)
                .get()
                .await
                .expect("read edited live body");
            assert_eq!(verified.markdown.as_deref(), Some(expected_after.as_str()));
        })
        .await;
    }
}
