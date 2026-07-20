// any-mcp - bounded, workflow-oriented MCP server for Anytype
//
// SPDX-FileCopyrightText: 2026 Steve Schoettler
// SPDX-License-Identifier: Apache-2.0

//! Single-object soft-archive workflow.
//!
//! This module deliberately exposes only Anytype's ordinary object DELETE,
//! which archives one object. It does not call the archived-object listing,
//! permanent batch deletion, delete-all, or space mutation APIs.

use std::{borrow::Cow, fmt};

use anytype::{error::AnytypeError, objects::Object};
use rmcp::{
    model::CallToolResult,
    schemars::{JsonSchema, Schema, SchemaGenerator, json_schema},
};
use serde::{Deserialize, Deserializer, Serialize, de};
use tokio_util::sync::CancellationToken;

use crate::{
    domain::{DomainValueError, ObjectId, ObjectResourceUri, SpaceId},
    error::ToolError,
    handler_support::{HandlerError, MutationAccess, execute_handler, require_mutation_access},
    protocol::{ToolProfile, WorkflowTool, workflow_tool},
    result::tool_error,
    runtime::{OperationContext, RuntimeContext},
    schema::SchemaContractError,
};

/// Maximum Unicode scalar values accepted in a space id or display name.
pub const MAX_SPACE_REFERENCE_CHARS: usize = 512;

/// A nonempty, bounded space id or unique display name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct SpaceReference(String);

impl SpaceReference {
    /// Validates a space reference supplied by an MCP caller.
    pub fn new(value: impl Into<String>) -> Result<Self, SpaceReferenceError> {
        let value = value.into();
        if value.is_empty() {
            return Err(SpaceReferenceError::Empty);
        }
        if value.chars().count() > MAX_SPACE_REFERENCE_CHARS {
            return Err(SpaceReferenceError::TooLong);
        }
        Ok(Self(value))
    }

    /// Borrows the validated id or name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl AsRef<str> for SpaceReference {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl<'de> Deserialize<'de> for SpaceReference {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::new(String::deserialize(deserializer)?).map_err(de::Error::custom)
    }
}

impl JsonSchema for SpaceReference {
    fn schema_name() -> Cow<'static, str> {
        "SpaceReference".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "minLength": 1,
            "maxLength": MAX_SPACE_REFERENCE_CHARS,
        })
    }
}

/// Failure to construct a bounded space reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpaceReferenceError {
    /// A space id or name cannot be empty.
    Empty,
    /// The reference exceeded [`MAX_SPACE_REFERENCE_CHARS`].
    TooLong,
}

impl fmt::Display for SpaceReferenceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Empty => "space must not be empty",
            Self::TooLong => "space exceeds its maximum length",
        })
    }
}

impl std::error::Error for SpaceReferenceError {}

/// Strict input for archiving exactly one object.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ObjectArchiveInput {
    /// Unique space name or stable space identifier.
    space: SpaceReference,
    /// Stable identifier of the object to archive; object names are not accepted.
    object_id: ObjectId,
}

impl ObjectArchiveInput {
    /// Creates validated archive input for a transport-neutral direct call.
    #[must_use]
    pub const fn new(space: SpaceReference, object_id: ObjectId) -> Self {
        Self { space, object_id }
    }

    /// Borrows the unique space name or identifier.
    #[must_use]
    pub const fn space(&self) -> &SpaceReference {
        &self.space
    }

    /// Borrows the object identifier.
    #[must_use]
    pub const fn object_id(&self) -> &ObjectId {
        &self.object_id
    }
}

/// Verified `archived=true` state returned after a successful object archive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArchivedState;

impl Serialize for ArchivedState {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_bool(true)
    }
}

impl JsonSchema for ArchivedState {
    fn schema_name() -> Cow<'static, str> {
        "ArchivedState".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({"type": "boolean", "const": true})
    }
}

/// Minimal, verified result of archiving one object.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ObjectArchiveOutput {
    /// Stable identifier Anytype confirmed in the archive response.
    id: ObjectId,
    /// Verified archived state; this value is always true.
    archived: ArchivedState,
    /// Canonical resource identity for the archived object.
    resource_uri: ObjectResourceUri,
}

impl ObjectArchiveOutput {
    /// Returns the archived object identifier.
    #[must_use]
    pub const fn id(&self) -> &ObjectId {
        &self.id
    }

    /// Returns the confirmed archive state.
    #[must_use]
    pub const fn archived(&self) -> ArchivedState {
        self.archived
    }

    /// Returns the canonical resource identity.
    #[must_use]
    pub const fn resource_uri(&self) -> &ObjectResourceUri {
        &self.resource_uri
    }
}

/// Builds the strict `object_archive` contract for catalog registration.
///
/// The annotations mark this workflow as destructive, non-idempotent,
/// read-write, and closed-world. They are hints only; [`object_archive`]
/// separately applies the supplied [`MutationAccess`] gate.
pub fn object_archive_tool() -> Result<WorkflowTool<ObjectArchiveOutput>, SchemaContractError> {
    workflow_tool::<ObjectArchiveInput, ObjectArchiveOutput>(
        "object_archive",
        "Soft-archive exactly one object. This never permanently or bulk deletes objects.",
        ToolProfile::Update,
    )
}

/// Soft-archives exactly one Anytype object under shared runtime controls.
///
/// The mutation gate runs before resolution or upstream I/O. The space is
/// resolved through `anytype-api`, then revalidated before it can enter an HTTP
/// path. The successful DELETE response must contain safe ids matching the
/// requested object and resolved space and must explicitly report
/// `archived=true`; malformed or mismatched upstream data fails closed.
pub async fn object_archive(
    runtime: &RuntimeContext,
    contract: &WorkflowTool<ObjectArchiveOutput>,
    access: MutationAccess,
    input: &ObjectArchiveInput,
    cancellation: &CancellationToken,
) -> CallToolResult {
    if let Err(error) = require_mutation_access(access) {
        return tool_error(error.tool_error());
    }

    let client = runtime.client();
    let space = input.space.as_str();
    let requested_object_id = input.object_id.clone();
    execute_handler(
        runtime,
        contract,
        OperationContext::new("object_archive"),
        cancellation,
        async {
            let resolved = client.resolve_space_id(space).await?;
            let resolved_space_id = SpaceId::new(resolved).map_err(unsafe_upstream_id)?;
            let object = client
                .object(resolved_space_id.as_str(), requested_object_id.as_str())
                .delete()
                .await?;
            Ok((resolved_space_id, requested_object_id, object))
        },
        |(resolved_space_id, requested_object_id, object)| async move {
            verified_archive_output(resolved_space_id, requested_object_id, object)
        },
    )
    .await
}

fn unsafe_upstream_id(_: DomainValueError) -> AnytypeError {
    AnytypeError::Other {
        message: "Anytype returned an unsafe identifier".to_owned(),
    }
}

fn verified_archive_output(
    resolved_space_id: SpaceId,
    requested_object_id: ObjectId,
    object: Object,
) -> Result<ObjectArchiveOutput, HandlerError> {
    let returned_object_id =
        ObjectId::new(object.id).map_err(|_| HandlerError::new(ToolError::upstream()))?;
    let returned_space_id =
        SpaceId::new(object.space_id).map_err(|_| HandlerError::new(ToolError::upstream()))?;

    if returned_object_id != requested_object_id
        || returned_space_id != resolved_space_id
        || !object.archived
    {
        return Err(HandlerError::new(ToolError::upstream()));
    }

    let resource_uri = ObjectResourceUri::new(&returned_space_id, &returned_object_id);
    Ok(ObjectArchiveOutput {
        id: returned_object_id,
        archived: ArchivedState,
        resource_uri,
    })
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use anytype::prelude::{AnytypeClient, ClientConfig, HttpCredentials, ResponseLimits};
    use serde_json::{Value, json};
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
        task::JoinHandle,
    };

    use super::*;
    use crate::runtime::StartupStatus;

    const SPACE_ID: &str =
        "bafyreid5fvqlnsobih2keakcxjrrlpmly6kf37klzjzen4ibfdgalcdp4y.2tq5w93cr6oe7";
    const OTHER_SPACE_ID: &str =
        "bafyreid5fvqlnsobih2keakcxjrrlpmly6kf37klzjzen4ibfdgalcdp4y.abc123";
    const OBJECT_ID: &str = "bafyreie6n5l5nkbjal37su54cha4coy7qzuhrnajluzv5qd5jvtsrxkequ";
    const OTHER_OBJECT_ID: &str = "bafyreiaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    struct FixtureReply {
        status: &'static str,
        body: String,
        delay: Duration,
    }

    impl FixtureReply {
        fn json(body: Value) -> Self {
            Self {
                status: "200 OK",
                body: body.to_string(),
                delay: Duration::ZERO,
            }
        }

        fn error(status: &'static str, body: &str) -> Self {
            Self {
                status,
                body: body.to_owned(),
                delay: Duration::ZERO,
            }
        }

        fn delayed(mut self, delay: Duration) -> Self {
            self.delay = delay;
            self
        }
    }

    async fn fixture(replies: Vec<FixtureReply>) -> (String, JoinHandle<Vec<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind archive fixture");
        let address = listener.local_addr().expect("archive fixture address");
        let server = tokio::spawn(async move {
            let mut requests = Vec::with_capacity(replies.len());
            for reply in replies {
                let (mut socket, _) = listener.accept().await.expect("accept archive request");
                let mut request = Vec::new();
                let mut buffer = [0_u8; 1024];
                loop {
                    let read = socket
                        .read(&mut buffer)
                        .await
                        .expect("read archive request");
                    if read == 0 {
                        break;
                    }
                    request.extend_from_slice(&buffer[..read]);
                    if request.windows(4).any(|window| window == b"\r\n\r\n") {
                        break;
                    }
                }
                requests.push(String::from_utf8(request).expect("request headers are utf-8"));
                tokio::time::sleep(reply.delay).await;
                let response = format!(
                    "HTTP/1.1 {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    reply.status,
                    reply.body.len(),
                    reply.body,
                );
                let _ = socket.write_all(response.as_bytes()).await;
            }
            requests
        });
        (format!("http://{address}"), server)
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

    fn runtime(base_url: String, timeout: Duration) -> RuntimeContext {
        runtime_with_limits(base_url, timeout, ResponseLimits::default())
    }

    fn runtime_with_limits(
        base_url: String,
        timeout: Duration,
        response_limits: ResponseLimits,
    ) -> RuntimeContext {
        let client = AnytypeClient::with_config(ClientConfig {
            base_url: Some(base_url),
            keystore: Some("env".to_owned()),
            keystore_service: Some("object-archive-test".to_owned()),
            app_name: "object-archive-test".to_owned(),
            response_limits,
            ..ClientConfig::default()
        })
        .expect("archive fixture client");
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

    fn input(space: &str, object_id: &str) -> ObjectArchiveInput {
        ObjectArchiveInput::new(
            SpaceReference::new(space).expect("valid space reference"),
            ObjectId::new(object_id).expect("valid object id"),
        )
    }

    fn archived_object(space_id: &str, object_id: &str, archived: bool) -> Value {
        json!({
            "object": {
                "archived": archived,
                "id": object_id,
                "space_id": space_id,
                "type": null
            }
        })
    }

    fn result_code(result: &CallToolResult) -> &str {
        result
            .structured_content
            .as_ref()
            .and_then(|value| value.get("code"))
            .and_then(Value::as_str)
            .expect("error code")
    }

    #[test]
    fn contract_is_strict_and_uses_the_destructive_update_profile() {
        let tool = object_archive_tool().expect("valid archive contract");
        let encoded_annotations = serde_json::to_value(
            tool.as_tool()
                .annotations
                .as_ref()
                .expect("archive annotations"),
        )
        .expect("serialize annotations");
        assert_eq!(
            encoded_annotations,
            json!({
                "readOnlyHint": false,
                "destructiveHint": true,
                "idempotentHint": false,
                "openWorldHint": false
            })
        );
        assert_eq!(tool.as_tool().name, "object_archive");

        assert!(
            serde_json::from_value::<ObjectArchiveInput>(json!({
                "space": SPACE_ID,
                "object_id": OBJECT_ID,
                "extra": true
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<ObjectArchiveInput>(json!({
                "space": "",
                "object_id": OBJECT_ID
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<ObjectArchiveInput>(json!({
                "space": SPACE_ID,
                "object_id": "../unsafe"
            }))
            .is_err()
        );
    }

    #[tokio::test]
    async fn archives_exactly_one_object_at_the_intended_soft_delete_path() {
        let (base_url, server) = fixture(vec![FixtureReply::json(archived_object(
            SPACE_ID, OBJECT_ID, true,
        ))])
        .await;
        let runtime = runtime(base_url, Duration::from_secs(1));
        let result = object_archive(
            &runtime,
            &object_archive_tool().unwrap(),
            MutationAccess::Allowed,
            &input(SPACE_ID, OBJECT_ID),
            &CancellationToken::new(),
        )
        .await;

        assert_eq!(result.is_error, Some(false));
        assert_eq!(
            result.structured_content,
            Some(json!({
                "id": OBJECT_ID,
                "archived": true,
                "resource_uri": format!("anytype://spaces/{SPACE_ID}/objects/{OBJECT_ID}")
            }))
        );
        let requests = server.await.expect("archive fixture task");
        assert_eq!(requests.len(), 1);
        assert!(requests[0].starts_with(&format!(
            "DELETE /v1/spaces/{SPACE_ID}/objects/{OBJECT_ID} HTTP/1.1\r\n"
        )));
        assert!(!requests[0].contains("archived"));
        assert!(!requests[0].contains("delete_all"));
        assert!(!requests[0].contains("object_list_delete"));
    }

    #[tokio::test]
    async fn unsafe_resolved_space_is_rejected_before_delete_io() {
        let spaces = json!({
            "items": [{
                "id": "../unsafe",
                "name": "Workspace",
                "object": "space",
                "description": null,
                "icon": null,
                "gateway_url": null,
                "network_id": null
            }],
            "pagination": {"has_more": false, "limit": 100, "offset": 0, "total": 1}
        });
        let (base_url, server) = fixture(vec![FixtureReply::json(spaces)]).await;
        let runtime = runtime(base_url, Duration::from_secs(1));
        let result = object_archive(
            &runtime,
            &object_archive_tool().unwrap(),
            MutationAccess::Allowed,
            &input("Workspace", OBJECT_ID),
            &CancellationToken::new(),
        )
        .await;

        assert_eq!(result.is_error, Some(true));
        assert_eq!(result_code(&result), "upstream");
        let requests = server.await.expect("resolver fixture task");
        assert_eq!(requests.len(), 1);
        assert!(requests[0].starts_with("GET /v1/spaces?"));
        assert!(!requests[0].contains("DELETE"));
    }

    #[tokio::test]
    async fn read_only_and_precancelled_calls_reject_before_any_io() {
        for (access, cancelled, expected_code) in [
            (MutationAccess::ReadOnly, false, "validation"),
            (MutationAccess::Allowed, true, "upstream"),
        ] {
            let (base_url, server) = no_request_fixture().await;
            let runtime = runtime(base_url, Duration::from_secs(1));
            let cancellation = CancellationToken::new();
            if cancelled {
                cancellation.cancel();
            }
            let result = object_archive(
                &runtime,
                &object_archive_tool().unwrap(),
                access,
                &input(SPACE_ID, OBJECT_ID),
                &cancellation,
            )
            .await;
            assert_eq!(result.is_error, Some(true));
            assert_eq!(result_code(&result), expected_code);
            assert!(server.await.expect("no-request fixture task"));
        }
    }

    #[tokio::test]
    async fn malformed_or_mismatched_success_responses_fail_closed() {
        let cases = [
            archived_object(SPACE_ID, OTHER_OBJECT_ID, true),
            archived_object(OTHER_SPACE_ID, OBJECT_ID, true),
            archived_object(SPACE_ID, OBJECT_ID, false),
            archived_object(SPACE_ID, "../unsafe", true),
            archived_object("../unsafe", OBJECT_ID, true),
        ];
        for response in cases {
            let (base_url, server) = fixture(vec![FixtureReply::json(response)]).await;
            let runtime = runtime(base_url, Duration::from_secs(1));
            let result = object_archive(
                &runtime,
                &object_archive_tool().unwrap(),
                MutationAccess::Allowed,
                &input(SPACE_ID, OBJECT_ID),
                &CancellationToken::new(),
            )
            .await;
            assert_eq!(result.is_error, Some(true));
            assert_eq!(result_code(&result), "upstream");
            let requests = server.await.expect("malformed fixture task");
            assert_eq!(requests.len(), 1);
            assert!(requests[0].starts_with("DELETE "));
        }
    }

    #[tokio::test]
    async fn permissions_not_found_and_auth_failures_use_fixed_errors() {
        for (status, expected_code) in [
            ("403 Forbidden", "authentication"),
            ("404 Not Found", "not_found"),
            ("401 Unauthorized", "authentication"),
        ] {
            let (base_url, server) = fixture(vec![FixtureReply::error(
                status,
                "Bearer secret-token private upstream body",
            )])
            .await;
            let runtime = runtime(base_url, Duration::from_secs(1));
            let result = object_archive(
                &runtime,
                &object_archive_tool().unwrap(),
                MutationAccess::Allowed,
                &input(SPACE_ID, OBJECT_ID),
                &CancellationToken::new(),
            )
            .await;
            assert_eq!(result.is_error, Some(true));
            assert_eq!(result_code(&result), expected_code);
            let encoded = serde_json::to_string(&result).expect("serialize fixed error");
            assert!(!encoded.contains("secret-token"));
            assert!(!encoded.contains("private upstream"));
            assert_eq!(server.await.expect("error fixture task").len(), 1);
        }
    }

    #[tokio::test]
    async fn runtime_timeout_cancels_the_archive_response_wait() {
        let (base_url, server) = fixture(vec![
            FixtureReply::json(archived_object(SPACE_ID, OBJECT_ID, true))
                .delayed(Duration::from_millis(100)),
        ])
        .await;
        let runtime = runtime(base_url, Duration::from_millis(20));
        let result = object_archive(
            &runtime,
            &object_archive_tool().unwrap(),
            MutationAccess::Allowed,
            &input(SPACE_ID, OBJECT_ID),
            &CancellationToken::new(),
        )
        .await;

        assert_eq!(result.is_error, Some(true));
        assert_eq!(result_code(&result), "upstream");
        let requests = server.await.expect("timeout fixture task");
        assert_eq!(requests.len(), 1);
        assert!(requests[0].starts_with("DELETE "));
    }

    #[tokio::test]
    async fn oversized_archive_response_uses_the_document_ceiling() {
        let (base_url, server) = fixture(vec![FixtureReply::json(archived_object(
            SPACE_ID, OBJECT_ID, true,
        ))])
        .await;
        let runtime = runtime_with_limits(
            base_url,
            Duration::from_secs(1),
            ResponseLimits {
                json_bytes: 64,
                document_bytes: 64,
                error_bytes: 64,
                file_bytes: 64,
            },
        );
        let result = object_archive(
            &runtime,
            &object_archive_tool().unwrap(),
            MutationAccess::Allowed,
            &input(SPACE_ID, OBJECT_ID),
            &CancellationToken::new(),
        )
        .await;

        assert_eq!(result.is_error, Some(true));
        assert_eq!(result_code(&result), "bounded_result");
        let requests = server.await.expect("response-cap fixture task");
        assert_eq!(requests.len(), 1);
        assert!(requests[0].starts_with("DELETE "));
    }
}
