// any-mcp - bounded, workflow-oriented MCP server for Anytype
//
// SPDX-FileCopyrightText: 2026 Steve Schoettler
// SPDX-License-Identifier: Apache-2.0

//! Single-object soft-archive workflow.
//!
//! This module deliberately exposes only Anytype's ordinary object DELETE,
//! which archives one object. Every successful call uses bounded independent
//! active and archived reads after dispatch; it never calls permanent batch
//! deletion, delete-all, or space mutation APIs.

use std::{borrow::Cow, fmt, future::Future, time::Duration};

use anytype::{
    error::AnytypeError,
    objects::Object,
    paged::PagedResult,
    prelude::{AnytypeClient, VerifyConfig, verify_semantic},
};
use rmcp::{
    model::CallToolResult,
    schemars::{JsonSchema, Schema, SchemaGenerator, json_schema},
};
use serde::{Deserialize, Deserializer, Serialize, de};
use tokio_util::sync::CancellationToken;

use crate::{
    domain::{DomainValueError, EntityId, ObjectId, ObjectResourceUri, SpaceId, TypeKey},
    error::{ToolError, mutation_rejection_is_definitive},
    handler_support::{
        HandlerError, HandlerOperationError, MutationAccess, MutationProgress,
        execute_mutation_handler, require_mutation_access,
    },
    protocol::{ToolProfile, WorkflowTool, workflow_tool},
    result::tool_error,
    runtime::{OperationContext, RuntimeContext},
    schema::SchemaContractError,
};

/// Maximum Unicode scalar values accepted in a space id or display name.
pub const MAX_SPACE_REFERENCE_CHARS: usize = 512;

const ACTIVE_PAGE_SIZE: u32 = 100;
const MAX_ACTIVE_ITEMS: u32 = 1_000;
const ARCHIVED_PAGE_SIZE: u32 = 1_000;
const MAX_ARCHIVED_ITEMS: u32 = 10_000;
const MAX_ARCHIVE_VERIFY_ATTEMPTS: usize = 10;
const MAX_ARCHIVE_VERIFY_TIME: Duration = Duration::from_secs(3);
const MAX_ARCHIVE_VERIFY_DELAY: Duration = Duration::from_millis(300);

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
    /// Stable identifier confirmed by independent stored-state verification.
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
/// The mutation gate runs before resolution or upstream I/O. The handler reads
/// and validates the exact active object's space, object, and type identities,
/// then marks and sends one non-replayed DELETE. Its response is dispatch
/// evidence only. Every success requires finite independent read-after-write
/// confirmation: the exact object must be absent from the bounded active
/// surface and present in the bounded, original-type-scoped archived surface.
/// Failure to prove both facts returns fixed mutation-indeterminate guidance
/// and never replays DELETE.
pub async fn object_archive(
    runtime: &RuntimeContext,
    contract: &WorkflowTool<ObjectArchiveOutput>,
    access: MutationAccess,
    input: &ObjectArchiveInput,
    cancellation: &CancellationToken,
) -> CallToolResult {
    object_archive_with_verifier(
        runtime,
        contract,
        access,
        input,
        cancellation,
        |client, verification, identity| async move {
            verify_archive_state(&client, &verification, &identity).await
        },
    )
    .await
}

async fn object_archive_with_verifier<Verify, VerifyFuture>(
    runtime: &RuntimeContext,
    contract: &WorkflowTool<ObjectArchiveOutput>,
    access: MutationAccess,
    input: &ObjectArchiveInput,
    cancellation: &CancellationToken,
    verify: Verify,
) -> CallToolResult
where
    Verify: FnOnce(AnytypeClient, VerifyConfig, ArchiveIdentity) -> VerifyFuture + Send,
    VerifyFuture: Future<Output = Result<(), AnytypeError>> + Send,
{
    if let Err(error) = require_mutation_access(access) {
        return tool_error(error.tool_error());
    }

    let client = runtime.client().clone();
    let verifier_client = client.raw_clone();
    let verification = archive_verification_config(
        client
            .get_config()
            .get_verify_config()
            .cloned()
            .unwrap_or_else(VerifyConfig::default),
    );
    let input = input.clone();
    let progress = MutationProgress::new();
    let operation_progress = progress.clone();
    let operation = Box::pin(async move {
        let resolved = client.resolve_space_id(input.space.as_str()).await?;
        let space_id = SpaceId::new(resolved).map_err(upstream_domain)?;
        let object_id = input.object_id;
        let current = client
            .object(space_id.as_str(), object_id.as_str())
            .get()
            .await?;
        let identity = checked_preflight_identity(current, space_id, object_id)?;

        operation_progress.mark_dispatched(runtime)?;
        match client
            .object(identity.space_id.as_str(), identity.object_id.as_str())
            .delete_once()
            .await
        {
            Err(error) if mutation_rejection_is_definitive(&error) => {
                return Err(error.into());
            }
            Ok(_) | Err(_) => {}
        }

        verify(verifier_client, verification, identity.clone())
            .await
            .map_err(|_| HandlerError::new(ToolError::mutation_indeterminate()))?;
        Ok::<_, HandlerOperationError>(archive_output(&identity))
    });
    let operation = execute_mutation_handler(
        runtime,
        contract,
        OperationContext::new("object_archive"),
        cancellation,
        &progress,
        operation,
        |output| async move { Ok(output) },
    );
    let routed = runtime
        .run_routed_invocation("object_archive", cancellation, Box::pin(operation))
        .await;
    match routed {
        Ok(result) => result,
        Err(failure) if failure.dispatched => tool_error(&ToolError::mutation_indeterminate()),
        Err(_) => tool_error(&ToolError::upstream()),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ArchiveIdentity {
    space_id: SpaceId,
    object_id: ObjectId,
    type_id: EntityId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ArchiveEvidence {
    active_absent: bool,
    archived_present: bool,
}

impl ArchiveEvidence {
    const fn proven(self) -> bool {
        self.active_absent && self.archived_present
    }
}

#[derive(Debug)]
struct EvidencePage {
    items: Vec<Object>,
    offset: u32,
    limit: u32,
    has_more: bool,
}

impl EvidencePage {
    fn from_paged(page: PagedResult<Object>) -> Self {
        let page = page.into_response();
        Self {
            items: page.items,
            offset: page.pagination.offset,
            limit: page.pagination.limit,
            has_more: page.pagination.has_more,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScanResult {
    Present,
    Absent,
    Incomplete,
}

fn archive_verification_config(configured: VerifyConfig) -> VerifyConfig {
    VerifyConfig {
        timeout: configured.timeout.min(MAX_ARCHIVE_VERIFY_TIME),
        initial_delay: configured.initial_delay.min(MAX_ARCHIVE_VERIFY_DELAY),
        max_delay: configured.max_delay.min(MAX_ARCHIVE_VERIFY_DELAY),
        max_attempts: configured
            .effective_max_attempts()
            .min(MAX_ARCHIVE_VERIFY_ATTEMPTS),
    }
}

fn checked_preflight_identity(
    object: Object,
    space_id: SpaceId,
    object_id: ObjectId,
) -> Result<ArchiveIdentity, HandlerError> {
    validate_object_identity(&object, &space_id, &object_id, None, true)?;
    if object.archived {
        return Err(HandlerError::new(ToolError::not_found()));
    }
    let object_type = object
        .r#type
        .ok_or_else(|| HandlerError::new(ToolError::upstream()))?;
    if object_type.archived {
        return Err(HandlerError::new(ToolError::upstream()));
    }
    let type_id = EntityId::new(object_type.id).map_err(upstream_domain)?;
    TypeKey::new(object_type.key).map_err(upstream_domain)?;
    Ok(ArchiveIdentity {
        space_id,
        object_id,
        type_id,
    })
}

fn archive_output(identity: &ArchiveIdentity) -> ObjectArchiveOutput {
    let resource_uri = ObjectResourceUri::new(&identity.space_id, &identity.object_id);
    ObjectArchiveOutput {
        id: identity.object_id.clone(),
        archived: ArchivedState,
        resource_uri,
    }
}

fn validate_object_identity(
    object: &Object,
    space_id: &SpaceId,
    object_id: &ObjectId,
    type_id: Option<&EntityId>,
    require_type: bool,
) -> Result<(), HandlerError> {
    let returned_object_id = ObjectId::new(object.id.clone()).map_err(upstream_domain)?;
    let returned_space_id = SpaceId::new(object.space_id.clone()).map_err(upstream_domain)?;
    if &returned_object_id != object_id || &returned_space_id != space_id {
        return Err(HandlerError::new(ToolError::upstream()));
    }
    match (&object.r#type, type_id) {
        (Some(returned_type), Some(type_id)) => {
            let returned_type_id =
                EntityId::new(returned_type.id.clone()).map_err(upstream_domain)?;
            TypeKey::new(returned_type.key.clone()).map_err(upstream_domain)?;
            if &returned_type_id != type_id || returned_type.archived {
                return Err(HandlerError::new(ToolError::upstream()));
            }
        }
        (None, _) if require_type => return Err(HandlerError::new(ToolError::upstream())),
        _ => {}
    }
    Ok(())
}

fn upstream_domain(error: DomainValueError) -> HandlerError {
    match error {
        DomainValueError::TooLong { .. } => HandlerError::new(ToolError::bounded_result()),
        DomainValueError::Empty | DomainValueError::InvalidIdentifierCharacter => {
            HandlerError::new(ToolError::upstream())
        }
    }
}

async fn verify_archive_state(
    client: &AnytypeClient,
    config: &VerifyConfig,
    identity: &ArchiveIdentity,
) -> Result<(), AnytypeError> {
    verify_archive_state_with(config, identity, || archive_evidence(client, identity)).await
}

async fn verify_archive_state_with<Fetch, Fut>(
    config: &VerifyConfig,
    identity: &ArchiveIdentity,
    fetch: Fetch,
) -> Result<(), AnytypeError>
where
    Fetch: FnMut() -> Fut,
    Fut: Future<Output = Result<ArchiveEvidence, AnytypeError>>,
{
    verify_semantic(
        config,
        "archived object",
        identity.object_id.as_str(),
        fetch,
        |evidence| evidence.proven(),
    )
    .await
    .map(|_| ())
}

async fn archive_evidence(
    client: &AnytypeClient,
    identity: &ArchiveIdentity,
) -> Result<ArchiveEvidence, AnytypeError> {
    let active = scan_active_with(identity, |limit, offset| async move {
        client
            .objects(identity.space_id.as_str())
            .limit(limit)
            .offset(offset)
            .list()
            .await
            .map(EvidencePage::from_paged)
    })
    .await?;
    let archived = scan_archived_with(identity, |limit, offset| async move {
        client
            .list_archived(identity.space_id.as_str())
            .types([identity.type_id.as_str()])
            .limit(limit)
            .offset(offset)
            .list()
            .await
            .map(EvidencePage::from_paged)
    })
    .await?;
    Ok(ArchiveEvidence {
        active_absent: active == ScanResult::Absent,
        archived_present: archived == ScanResult::Present,
    })
}

async fn scan_active_with<Fetch, Fut>(
    identity: &ArchiveIdentity,
    mut fetch: Fetch,
) -> Result<ScanResult, AnytypeError>
where
    Fetch: FnMut(u32, u32) -> Fut,
    Fut: Future<Output = Result<EvidencePage, AnytypeError>>,
{
    for offset in (0..MAX_ACTIVE_ITEMS).step_by(ACTIVE_PAGE_SIZE as usize) {
        let page = fetch(ACTIVE_PAGE_SIZE, offset).await?;
        validate_page(&page, offset, ACTIVE_PAGE_SIZE)?;
        for object in page
            .items
            .iter()
            .filter(|object| object.id == identity.object_id.as_str())
        {
            validate_object_identity(
                object,
                &identity.space_id,
                &identity.object_id,
                Some(&identity.type_id),
                true,
            )
            .map_err(recovery_validation)?;
            if !object.archived {
                return Ok(ScanResult::Present);
            }
        }
        if !page.has_more {
            return Ok(ScanResult::Absent);
        }
    }
    Ok(ScanResult::Incomplete)
}

async fn scan_archived_with<Fetch, Fut>(
    identity: &ArchiveIdentity,
    mut fetch: Fetch,
) -> Result<ScanResult, AnytypeError>
where
    Fetch: FnMut(u32, u32) -> Fut,
    Fut: Future<Output = Result<EvidencePage, AnytypeError>>,
{
    for offset in (0..MAX_ARCHIVED_ITEMS).step_by(ARCHIVED_PAGE_SIZE as usize) {
        let page = fetch(ARCHIVED_PAGE_SIZE, offset).await?;
        validate_page(&page, offset, ARCHIVED_PAGE_SIZE)?;
        for object in page
            .items
            .iter()
            .filter(|object| object.id == identity.object_id.as_str())
        {
            validate_object_identity(
                object,
                &identity.space_id,
                &identity.object_id,
                Some(&identity.type_id),
                false,
            )
            .map_err(recovery_validation)?;
            if object.archived {
                return Ok(ScanResult::Present);
            }
        }
        if !page.has_more {
            return Ok(ScanResult::Absent);
        }
    }
    Ok(ScanResult::Incomplete)
}

fn validate_page(page: &EvidencePage, offset: u32, limit: u32) -> Result<(), AnytypeError> {
    if page.offset != offset || page.limit != limit || page.items.len() > limit as usize {
        return Err(recovery_error());
    }
    Ok(())
}

fn recovery_validation(_: HandlerError) -> AnytypeError {
    recovery_error()
}

fn recovery_error() -> AnytypeError {
    AnytypeError::Other {
        message: "archive confirmation returned malformed bounded evidence".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{
            Arc, Mutex,
            atomic::{AtomicUsize, Ordering},
        },
        time::Duration,
    };

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
    const TYPE_ID: &str = "bafyreityyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyy";
    const OTHER_TYPE_ID: &str = "bafyreizzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz";

    #[derive(Clone)]
    struct FixtureReply {
        status: &'static str,
        body: String,
        delay: Duration,
        disconnect: bool,
    }

    impl FixtureReply {
        fn json(body: Value) -> Self {
            Self {
                status: "200 OK",
                body: body.to_string(),
                delay: Duration::ZERO,
                disconnect: false,
            }
        }

        fn error(status: &'static str, body: &str) -> Self {
            Self {
                status,
                body: body.to_owned(),
                delay: Duration::ZERO,
                disconnect: false,
            }
        }

        fn malformed(body: &str) -> Self {
            Self {
                status: "200 OK",
                body: body.to_owned(),
                delay: Duration::ZERO,
                disconnect: false,
            }
        }

        fn disconnect() -> Self {
            Self {
                status: "200 OK",
                body: String::new(),
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
                if reply.disconnect {
                    continue;
                }
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

    fn object_value(
        space_id: &str,
        object_id: &str,
        archived: bool,
        type_id: Option<&str>,
    ) -> Value {
        json!({
            "archived": archived,
            "id": object_id,
            "space_id": space_id,
            "type": type_id.map(|type_id| json!({
                "archived": false,
                "id": type_id,
                "key": "page"
            }))
        })
    }

    fn object_response(
        space_id: &str,
        object_id: &str,
        archived: bool,
        type_id: Option<&str>,
    ) -> Value {
        json!({"object": object_value(space_id, object_id, archived, type_id)})
    }

    fn object(space_id: &str, object_id: &str, archived: bool, type_id: Option<&str>) -> Object {
        serde_json::from_value(object_value(space_id, object_id, archived, type_id))
            .expect("valid fixture object")
    }

    fn identity() -> ArchiveIdentity {
        ArchiveIdentity {
            space_id: SpaceId::new(SPACE_ID).unwrap(),
            object_id: ObjectId::new(OBJECT_ID).unwrap(),
            type_id: EntityId::new(TYPE_ID).unwrap(),
        }
    }

    fn page(items: Vec<Object>, offset: u32, limit: u32, has_more: bool) -> EvidencePage {
        EvidencePage {
            items,
            offset,
            limit,
            has_more,
        }
    }

    fn preflight_reply() -> FixtureReply {
        FixtureReply::json(object_response(SPACE_ID, OBJECT_ID, false, Some(TYPE_ID)))
    }

    fn assert_one_delete(requests: &[String]) {
        assert_eq!(
            requests
                .iter()
                .filter(|request| request.starts_with("DELETE "))
                .count(),
            1,
            "archive mutation must dispatch DELETE exactly once"
        );
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
    async fn applied_archive_requires_independent_verification_after_one_delete() {
        let (base_url, server) = fixture(vec![
            preflight_reply(),
            FixtureReply::json(object_response(SPACE_ID, OBJECT_ID, true, Some(TYPE_ID))),
        ])
        .await;
        let runtime = runtime(base_url, Duration::from_secs(1));
        let verification_calls = Arc::new(AtomicUsize::new(0));
        let recorded_calls = verification_calls.clone();
        let result = object_archive_with_verifier(
            &runtime,
            &object_archive_tool().unwrap(),
            MutationAccess::Allowed,
            &input(SPACE_ID, OBJECT_ID),
            &CancellationToken::new(),
            move |_, _, verified_identity| async move {
                recorded_calls.fetch_add(1, Ordering::SeqCst);
                assert_eq!(verified_identity, identity());
                verify_archive_state_with(
                    &VerifyConfig {
                        timeout: Duration::from_secs(1),
                        initial_delay: Duration::ZERO,
                        max_delay: Duration::ZERO,
                        max_attempts: 2,
                    },
                    &verified_identity,
                    || async {
                        Ok(ArchiveEvidence {
                            active_absent: true,
                            archived_present: true,
                        })
                    },
                )
                .await
            },
        )
        .await;

        assert_eq!(result.is_error, Some(false));
        assert_eq!(verification_calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            result.structured_content,
            Some(json!({
                "id": OBJECT_ID,
                "archived": true,
                "resource_uri": format!("anytype://spaces/{SPACE_ID}/objects/{OBJECT_ID}")
            }))
        );
        let requests = server.await.expect("archive fixture task");
        assert_eq!(requests.len(), 2);
        assert!(requests[0].starts_with(&format!(
            "GET /v1/spaces/{SPACE_ID}/objects/{OBJECT_ID} HTTP/1.1\r\n"
        )));
        assert!(requests[1].starts_with(&format!(
            "DELETE /v1/spaces/{SPACE_ID}/objects/{OBJECT_ID} HTTP/1.1\r\n"
        )));
        assert_one_delete(&requests);
        assert!(!requests[1].contains("archived"));
        assert!(!requests[1].contains("delete_all"));
        assert!(!requests[1].contains("object_list_delete"));
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
        assert_eq!(result_code(&result), "authentication");
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
    async fn matching_delete_body_without_applied_state_is_indeterminate() {
        let (base_url, server) = fixture(vec![
            preflight_reply(),
            FixtureReply::json(object_response(SPACE_ID, OBJECT_ID, true, Some(TYPE_ID))),
        ])
        .await;
        let runtime = runtime(base_url, Duration::from_secs(1));
        let attempts = Arc::new(AtomicUsize::new(0));
        let recorded_attempts = attempts.clone();
        let result = object_archive_with_verifier(
            &runtime,
            &object_archive_tool().unwrap(),
            MutationAccess::Allowed,
            &input(SPACE_ID, OBJECT_ID),
            &CancellationToken::new(),
            move |_, _, verified_identity| async move {
                verify_archive_state_with(
                    &VerifyConfig {
                        timeout: Duration::from_secs(1),
                        initial_delay: Duration::ZERO,
                        max_delay: Duration::ZERO,
                        max_attempts: 3,
                    },
                    &verified_identity,
                    move || {
                        recorded_attempts.fetch_add(1, Ordering::SeqCst);
                        async {
                            Ok(ArchiveEvidence {
                                active_absent: false,
                                archived_present: false,
                            })
                        }
                    },
                )
                .await
            },
        )
        .await;

        assert_eq!(result.is_error, Some(true));
        assert_eq!(result_code(&result), "conflict");
        assert_eq!(attempts.load(Ordering::SeqCst), 3);
        let requests = server.await.expect("matching response fixture task");
        assert_eq!(requests.len(), 2);
        assert_one_delete(&requests);
    }

    #[tokio::test]
    async fn mismatched_and_malformed_delete_bodies_are_dispatch_evidence_only() {
        let cases = [
            FixtureReply::json(object_response(
                SPACE_ID,
                OTHER_OBJECT_ID,
                true,
                Some(TYPE_ID),
            )),
            FixtureReply::json(object_response(
                OTHER_SPACE_ID,
                OBJECT_ID,
                true,
                Some(TYPE_ID),
            )),
            FixtureReply::json(object_response(SPACE_ID, OBJECT_ID, false, Some(TYPE_ID))),
            FixtureReply::json(object_response(
                SPACE_ID,
                OBJECT_ID,
                true,
                Some(OTHER_TYPE_ID),
            )),
            FixtureReply::malformed("{"),
        ];
        for reply in cases {
            let (base_url, server) = fixture(vec![preflight_reply(), reply]).await;
            let runtime = runtime(base_url, Duration::from_millis(100));
            let result = object_archive_with_verifier(
                &runtime,
                &object_archive_tool().unwrap(),
                MutationAccess::Allowed,
                &input(SPACE_ID, OBJECT_ID),
                &CancellationToken::new(),
                |_, _, _| async { Ok(()) },
            )
            .await;
            assert_eq!(result.is_error, Some(false));
            let requests = server.await.expect("dispatch-evidence fixture task");
            assert_eq!(requests.len(), 2);
            assert_one_delete(&requests);
        }
    }

    #[tokio::test]
    async fn preflight_already_missing_and_wrong_identity_never_dispatch_delete() {
        let cases = [
            (
                FixtureReply::json(object_response(SPACE_ID, OBJECT_ID, true, Some(TYPE_ID))),
                "not_found",
            ),
            (
                FixtureReply::error("404 Not Found", "missing before mutation"),
                "not_found",
            ),
            (
                FixtureReply::json(object_response(
                    SPACE_ID,
                    OTHER_OBJECT_ID,
                    false,
                    Some(TYPE_ID),
                )),
                "upstream",
            ),
            (
                FixtureReply::json(object_response(
                    OTHER_SPACE_ID,
                    OBJECT_ID,
                    false,
                    Some(TYPE_ID),
                )),
                "upstream",
            ),
            (
                FixtureReply::json(object_response(SPACE_ID, OBJECT_ID, false, None)),
                "upstream",
            ),
            (
                FixtureReply::json(object_response(
                    SPACE_ID,
                    OBJECT_ID,
                    false,
                    Some("../unsafe"),
                )),
                "upstream",
            ),
        ];
        for (reply, expected_code) in cases {
            let (base_url, server) = fixture(vec![reply]).await;
            let result = object_archive(
                &runtime(base_url, Duration::from_secs(1)),
                &object_archive_tool().unwrap(),
                MutationAccess::Allowed,
                &input(SPACE_ID, OBJECT_ID),
                &CancellationToken::new(),
            )
            .await;
            assert_eq!(result.is_error, Some(true));
            assert_eq!(result_code(&result), expected_code);
            let requests = server.await.expect("preflight fixture task");
            assert_eq!(requests.len(), 1);
            assert!(requests[0].starts_with("GET "));
            assert!(!requests[0].contains("DELETE"));
        }
    }

    #[tokio::test]
    async fn definitive_delete_rejections_use_fixed_errors_without_replay() {
        // The 403/404/401 statuses are definitive rejections. A mutation 429
        // is indeterminate under the HTTP timeout policy: the server may
        // have applied the delete before rate-limiting the response, so it
        // maps to the fixed mutation-indeterminate conflict error instead.
        for (status, expected_code) in [
            ("403 Forbidden", "authentication"),
            ("404 Not Found", "not_found"),
            ("401 Unauthorized", "authentication"),
            ("429 Too Many Requests", "conflict"),
        ] {
            let (base_url, server) = fixture(vec![
                preflight_reply(),
                FixtureReply::error(status, "Bearer secret-token private upstream body"),
            ])
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
            let requests = server.await.expect("error fixture task");
            assert_eq!(requests.len(), 2);
            assert_one_delete(&requests);
        }
    }

    #[tokio::test]
    async fn cancellation_after_delete_dispatch_is_indeterminate_and_never_replays() {
        let (base_url, server) = fixture(vec![
            preflight_reply(),
            FixtureReply::json(object_response(SPACE_ID, OBJECT_ID, true, Some(TYPE_ID)))
                .delayed(Duration::from_secs(1)),
        ])
        .await;
        // The cancel timer must land after the mutation dispatches and
        // before the delayed reply; both margins need slack on slow runners.
        let runtime = runtime(base_url, Duration::from_secs(5));
        let cancellation = CancellationToken::new();
        let trigger = cancellation.clone();
        let cancel_task = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(250)).await;
            trigger.cancel();
        });
        let result = object_archive(
            &runtime,
            &object_archive_tool().unwrap(),
            MutationAccess::Allowed,
            &input(SPACE_ID, OBJECT_ID),
            &cancellation,
        )
        .await;
        cancel_task.await.expect("cancellation task");

        assert_eq!(result.is_error, Some(true));
        assert_eq!(result_code(&result), "conflict");
        let requests = server.await.expect("timeout fixture task");
        assert_eq!(requests.len(), 2);
        assert_one_delete(&requests);
    }

    #[tokio::test]
    async fn independent_verification_timeout_is_indeterminate_after_one_delete() {
        let (base_url, server) = fixture(vec![
            preflight_reply(),
            FixtureReply::json(object_response(SPACE_ID, OBJECT_ID, true, Some(TYPE_ID))),
        ])
        .await;
        let runtime = runtime(base_url, Duration::from_millis(40));
        let result = object_archive_with_verifier(
            &runtime,
            &object_archive_tool().unwrap(),
            MutationAccess::Allowed,
            &input(SPACE_ID, OBJECT_ID),
            &CancellationToken::new(),
            |_, _, _| async {
                tokio::time::sleep(Duration::from_secs(1)).await;
                Ok(())
            },
        )
        .await;

        assert_eq!(result.is_error, Some(true));
        assert_eq!(result_code(&result), "conflict");
        let requests = server.await.expect("verification timeout fixture task");
        assert_eq!(requests.len(), 2);
        assert_one_delete(&requests);
    }

    #[tokio::test]
    async fn uncertain_delete_outcomes_are_reconciled_without_replay() {
        let mut oversized = object_response(SPACE_ID, OBJECT_ID, true, Some(TYPE_ID));
        oversized["object"]["name"] = json!("x".repeat(2_000));
        let cases = [
            FixtureReply::error("408 Request Timeout", "request timeout"),
            FixtureReply::error("307 Temporary Redirect", "redirect target"),
            FixtureReply::json(oversized),
            FixtureReply::disconnect(),
        ];
        for reply in cases {
            for independently_applied in [true, false] {
                let (base_url, server) = fixture(vec![preflight_reply(), reply.clone()]).await;
                let runtime = runtime_with_limits(
                    base_url,
                    Duration::from_millis(100),
                    ResponseLimits {
                        json_bytes: 512,
                        document_bytes: 512,
                        error_bytes: 64,
                        file_bytes: 64,
                        chat_sse_event_bytes: 64,
                    },
                );
                let result = object_archive_with_verifier(
                    &runtime,
                    &object_archive_tool().unwrap(),
                    MutationAccess::Allowed,
                    &input(SPACE_ID, OBJECT_ID),
                    &CancellationToken::new(),
                    move |_, _, _| async move {
                        if independently_applied {
                            Ok(())
                        } else {
                            Err(recovery_error())
                        }
                    },
                )
                .await;

                assert_eq!(result.is_error, Some(!independently_applied));
                if !independently_applied {
                    assert_eq!(result_code(&result), "conflict");
                }
                let requests = server.await.expect("uncertain failure fixture task");
                assert_eq!(requests.len(), 2);
                assert_one_delete(&requests);
            }
        }
    }

    #[tokio::test]
    async fn stale_archive_evidence_converges_within_finite_attempts() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let fetch_attempts = attempts.clone();
        let config = VerifyConfig {
            timeout: Duration::from_secs(1),
            initial_delay: Duration::ZERO,
            max_delay: Duration::ZERO,
            max_attempts: 3,
        };
        verify_archive_state_with(&config, &identity(), move || {
            let attempt = fetch_attempts.fetch_add(1, Ordering::SeqCst);
            async move {
                Ok(ArchiveEvidence {
                    active_absent: true,
                    archived_present: attempt > 0,
                })
            }
        })
        .await
        .expect("stale archive evidence converges");
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn archive_verification_honors_hard_attempt_and_time_caps() {
        let configured = VerifyConfig {
            timeout: Duration::from_secs(60),
            initial_delay: Duration::ZERO,
            max_delay: Duration::from_secs(60),
            max_attempts: 10_000,
        };
        let bounded = archive_verification_config(configured);
        assert_eq!(bounded.timeout, MAX_ARCHIVE_VERIFY_TIME);
        assert_eq!(bounded.max_delay, MAX_ARCHIVE_VERIFY_DELAY);
        assert_eq!(bounded.max_attempts, MAX_ARCHIVE_VERIFY_ATTEMPTS);

        let attempts = Arc::new(AtomicUsize::new(0));
        let fetch_attempts = attempts.clone();
        let fast = VerifyConfig {
            timeout: Duration::from_secs(1),
            initial_delay: Duration::ZERO,
            max_delay: Duration::ZERO,
            max_attempts: 3,
        };
        let error = verify_archive_state_with(&fast, &identity(), move || {
            fetch_attempts.fetch_add(1, Ordering::SeqCst);
            async {
                Ok(ArchiveEvidence {
                    active_absent: false,
                    archived_present: true,
                })
            }
        })
        .await
        .expect_err("unproven evidence must exhaust the finite verifier");
        assert!(matches!(
            error,
            AnytypeError::VerifyTimeout { attempts: 3, .. }
        ));
        assert_eq!(attempts.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn active_and_archived_scans_stop_at_explicit_page_and_item_bounds() {
        let active_calls = Arc::new(Mutex::new(Vec::new()));
        let recorded_active = active_calls.clone();
        let active = scan_active_with(&identity(), move |limit, offset| {
            recorded_active.lock().unwrap().push((limit, offset));
            async move {
                Ok(page(
                    vec![object(SPACE_ID, OTHER_OBJECT_ID, false, Some(TYPE_ID))],
                    offset,
                    limit,
                    true,
                ))
            }
        })
        .await
        .expect("bounded active scan");
        assert_eq!(active, ScanResult::Incomplete);
        {
            let active_calls = active_calls.lock().unwrap();
            assert_eq!(active_calls.len(), 10);
            assert_eq!(active_calls.first(), Some(&(ACTIVE_PAGE_SIZE, 0)));
            assert_eq!(active_calls.last(), Some(&(ACTIVE_PAGE_SIZE, 900)));
        }

        let archived_calls = Arc::new(Mutex::new(Vec::new()));
        let recorded_archived = archived_calls.clone();
        let archived = scan_archived_with(&identity(), move |limit, offset| {
            recorded_archived.lock().unwrap().push((limit, offset));
            async move {
                Ok(page(
                    vec![object(SPACE_ID, OTHER_OBJECT_ID, true, None)],
                    offset,
                    limit,
                    true,
                ))
            }
        })
        .await
        .expect("bounded archived scan");
        assert_eq!(archived, ScanResult::Incomplete);
        let archived_calls = archived_calls.lock().unwrap();
        assert_eq!(archived_calls.len(), 10);
        assert_eq!(archived_calls.first(), Some(&(ARCHIVED_PAGE_SIZE, 0)));
        assert_eq!(archived_calls.last(), Some(&(ARCHIVED_PAGE_SIZE, 9_000)));
    }

    #[tokio::test]
    async fn scans_require_exact_safe_identity_and_coherent_pagination() {
        let active = scan_active_with(&identity(), |limit, offset| async move {
            Ok(page(
                vec![object(SPACE_ID, OBJECT_ID, false, Some(TYPE_ID))],
                offset,
                limit,
                false,
            ))
        })
        .await
        .unwrap();
        assert_eq!(active, ScanResult::Present);

        let inactive = scan_active_with(&identity(), |limit, offset| async move {
            Ok(page(
                vec![object(SPACE_ID, OBJECT_ID, true, Some(TYPE_ID))],
                offset,
                limit,
                false,
            ))
        })
        .await
        .unwrap();
        assert_eq!(inactive, ScanResult::Absent);

        let archived = scan_archived_with(&identity(), |limit, offset| async move {
            Ok(page(
                vec![object(SPACE_ID, OBJECT_ID, true, None)],
                offset,
                limit,
                false,
            ))
        })
        .await
        .unwrap();
        assert_eq!(archived, ScanResult::Present);

        let wrong_space = scan_archived_with(&identity(), |limit, offset| async move {
            Ok(page(
                vec![object(OTHER_SPACE_ID, OBJECT_ID, true, None)],
                offset,
                limit,
                false,
            ))
        })
        .await;
        assert!(wrong_space.is_err());

        let malformed_page = scan_active_with(&identity(), |limit, offset| async move {
            Ok(page(Vec::new(), offset + limit, limit, false))
        })
        .await;
        assert!(malformed_page.is_err());
    }
}
