// any-mcp - bounded, workflow-oriented MCP server for Anytype
//
// SPDX-FileCopyrightText: 2026 Steve Schoettler
// SPDX-License-Identifier: Apache-2.0

//! Token-free local artifact import and export workflows.

use std::{
    borrow::Cow,
    collections::HashMap,
    fmt,
    fs::File,
    future::Future,
    io::{Read, Seek, SeekFrom, Write},
    sync::{Arc, Mutex, MutexGuard},
};

use anytype::{
    error::AnytypeError,
    files::FileObject,
    objects::{Object, plain_markdown_representation},
};
use rmcp::{
    model::{
        CallToolRequestMethod, CallToolRequestParams, CallToolResult, ErrorData, ProtocolVersion,
    },
    schemars::{JsonSchema, Schema, SchemaGenerator, json_schema},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio::io::AsyncReadExt as _;
use tokio_util::sync::CancellationToken;

#[cfg(any(test, feature = "acceptance-harness"))]
use crate::artifact_acceptance_gates::{ArtifactAcceptanceGatePoint, FirstChunkGateReader};

use crate::{
    artifact_config::RelativeNativePath,
    artifact_roots::{
        AnchoredImport, AtomicExport, EffectiveRootRegistry, PositionalReader,
        ROOTS_REQUIRED_GUIDANCE, RootAccessError, RootAccessErrorKind, StagingPayload,
    },
    artifact_staging::{
        ArtifactStaging, RetainedStageImport, STAGING_REQUIRED_GUIDANCE, StageAllocation,
        StageDirection, StageSource, StageWriteLease, StagingError,
    },
    artifact_validators::ValidatorFinding,
    domain::{EntityId, SpaceId},
    error::{ToolError, mutation_rejection_is_definitive},
    mutation_value::{MutationProperties, MutationProperty, normalized_properties},
    optional_toolsets::{
        OptionalRegistryFuture, OptionalRegistryTool, OptionalToolsetMetadata,
        OptionalToolsetRegistry,
    },
    protocol::{ToolProfile, WorkflowTool, workflow_tool},
    result::tool_error,
    runtime::{ControlledFailureKind, OperationContext, RuntimeContext},
    schema::SchemaContractError,
    space_policy::PolicyClient,
    validation::Omittable,
};

/// Optional registry selector for token-free artifact workflows.
pub const ARTIFACTS_TOOLSET_NAME: &str = "artifacts";
/// Inspect effective artifact capabilities without touching configured paths.
pub const ARTIFACT_STATUS: &str = "artifact_status";
/// Upload one authorized local artifact to Anytype.
pub const FILE_IMPORT: &str = "file_import";
/// Export one exact Anytype file to an authorized local destination.
pub const FILE_EXPORT: &str = "file_export";
/// Allocate one exact-size remote import stage.
pub const ARTIFACT_STAGE_ALLOCATE: &str = "artifact_stage_upload";
/// Release one authenticated staging record.
pub const ARTIFACT_STAGE_RELEASE: &str = "artifact_release";
/// Create one Anytype document from an authorized UTF-8 artifact.
pub const DOCUMENT_IMPORT_CREATE: &str = "document_import_create";
/// Replace one Anytype document body from an authorized UTF-8 artifact.
pub const DOCUMENT_IMPORT_UPDATE: &str = "document_import_update";
/// Export one exact canonical Anytype Markdown body to an authorized destination.
pub const DOCUMENT_EXPORT: &str = "document_export";

const MULTIPART_ALLOWANCE: u64 = 1024 * 1024;
const RESPONSE_BYTES: u64 = 256 * 1024;
const ERROR_BYTES: u64 = 64 * 1024;
const HEADER_BYTES: u64 = 64 * 1024;

#[cfg(any(test, feature = "acceptance-harness"))]
fn artifact_upload_reader(
    runtime: &RuntimeContext,
    file: File,
    length: u64,
    key: [u8; 32],
) -> FirstChunkGateReader<PositionalReader> {
    FirstChunkGateReader::new(
        PositionalReader::new(file, length),
        runtime.artifact_acceptance_gates().clone(),
        key,
    )
}

#[cfg(not(any(test, feature = "acceptance-harness")))]
fn artifact_upload_reader(
    _: &RuntimeContext,
    file: File,
    length: u64,
    _: [u8; 32],
) -> PositionalReader {
    PositionalReader::new(file, length)
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LocalLocation {
    root: String,
    #[serde(default)]
    path: Omittable<String>,
    #[serde(default)]
    path_native: Omittable<NativePathInput>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ArtifactSource {
    /// Authorized local source, mutually exclusive with `staged_handle`.
    #[serde(default)]
    #[schemars(schema_with = "optional_local_location_schema")]
    local: Omittable<LocalLocation>,
    /// Opaque ready staging bearer, mutually exclusive with `local`.
    #[serde(default)]
    #[schemars(schema_with = "optional_handle_schema")]
    staged_handle: Omittable<String>,
}

enum ResolvedSource {
    Local(LocalLocation),
    Staged(String),
}

impl ArtifactSource {
    fn resolve(self) -> Result<ResolvedSource, ArtifactToolError> {
        match (self.local, self.staged_handle) {
            (Omittable::Present(local), Omittable::Missing) => Ok(ResolvedSource::Local(local)),
            (Omittable::Missing, Omittable::Present(handle)) => Ok(ResolvedSource::Staged(handle)),
            (Omittable::Present(_), Omittable::Present(_))
            | (Omittable::Missing, Omittable::Missing) => Err(ArtifactToolError::Validation),
        }
    }
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ArtifactDestination {
    /// Authorized local create-new destination, mutually exclusive with `remote`.
    #[serde(default)]
    #[schemars(schema_with = "optional_local_location_schema")]
    local: Omittable<LocalLocation>,
    /// Whether to allocate a remote immutable download stage.
    #[serde(default)]
    #[schemars(schema_with = "optional_boolean_schema")]
    remote: Omittable<bool>,
}

enum ResolvedDestination {
    Local(LocalLocation),
    Remote,
}

enum ValidatedDestination {
    Local {
        root: String,
        path: RelativeNativePath,
    },
    Remote,
}

impl ResolvedDestination {
    fn validate(self) -> Result<ValidatedDestination, ArtifactToolError> {
        match self {
            Self::Local(location) => Ok(ValidatedDestination::Local {
                path: location.relative_path()?,
                root: location.root,
            }),
            Self::Remote => Ok(ValidatedDestination::Remote),
        }
    }
}

async fn reserve_file_export_operation(
    operations: &ArtifactOperationState,
    key: [u8; 32],
    fingerprint: [u8; 32],
    destination: ResolvedDestination,
) -> Result<(ValidatedDestination, ExportIdempotency), ArtifactToolError> {
    let destination = destination.validate()?;
    let reservation = operations.reserve_export(key, fingerprint).await?;
    Ok((destination, reservation))
}

async fn reserve_document_export_operation(
    operations: &ArtifactOperationState,
    key: [u8; 32],
    fingerprint: [u8; 32],
    destination: ResolvedDestination,
) -> Result<(ValidatedDestination, DocumentExportIdempotency), ArtifactToolError> {
    let destination = destination.validate()?;
    let reservation = operations.reserve_document_export(key, fingerprint).await?;
    Ok((destination, reservation))
}

async fn settle_export_failure(
    operations: &ArtifactOperationState,
    key: [u8; 32],
    error: ArtifactToolError,
) -> ArtifactToolError {
    if error == ArtifactToolError::Indeterminate {
        operations
            .set_outcome(key, OperationOutcome::Indeterminate)
            .await;
    } else {
        operations.remove(key).await;
    }
    error
}

fn should_release_failed_export_stage(error: ArtifactToolError) -> bool {
    error != ArtifactToolError::Indeterminate
}

impl ArtifactDestination {
    fn resolve(self) -> Result<ResolvedDestination, ArtifactToolError> {
        match (self.local, self.remote) {
            (Omittable::Present(local), Omittable::Missing) => {
                Ok(ResolvedDestination::Local(local))
            }
            (Omittable::Missing, Omittable::Present(true)) => Ok(ResolvedDestination::Remote),
            (Omittable::Present(_), Omittable::Present(_))
            | (Omittable::Missing, Omittable::Missing | Omittable::Present(false)) => {
                Err(ArtifactToolError::Validation)
            }
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct NativePathInput {
    encoding: String,
    value: String,
}

impl LocalLocation {
    fn relative_path(&self) -> Result<RelativeNativePath, ArtifactToolError> {
        match (self.path.as_ref(), self.path_native.as_ref()) {
            (Some(path), None) => {
                RelativeNativePath::from_utf8(path).map_err(|_| ArtifactToolError::Validation)
            }
            (None, Some(path)) => RelativeNativePath::from_native(&path.encoding, &path.value)
                .map_err(|_| ArtifactToolError::Validation),
            (Some(_), Some(_)) | (None, None) => Err(ArtifactToolError::Validation),
        }
    }
}

fn optional_local_location_schema(generator: &mut SchemaGenerator) -> Schema {
    generator.subschema_for::<LocalLocation>()
}

fn optional_boolean_schema(_: &mut SchemaGenerator) -> Schema {
    json_schema!({"type": "boolean"})
}

impl JsonSchema for LocalLocation {
    fn schema_name() -> Cow<'static, str> {
        "ArtifactLocalLocation".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "root": {
                    "description": "Canonical logical ID of one configured local artifact root.",
                    "type": "string",
                    "minLength": 1,
                    "maxLength": 128
                },
                "path": {
                    "description": "Portable UTF-8 path relative to the selected root; exactly one path form is required.",
                    "type": "string",
                    "minLength": 1,
                    "maxLength": 4096
                },
                "path_native": {
                    "description": "Platform-native encoded path relative to the selected root; exactly one path form is required.",
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "encoding": {
                            "description": "Closed encoding for the current platform's native path representation.",
                            "type": "string",
                            "enum": [
                                "unix-bytes-base64url",
                                "windows-wtf16le-base64url"
                            ]
                        },
                        "value": {
                            "description": "Canonical unpadded base64url native path bytes or code units.",
                            "type": "string",
                            "minLength": 1,
                            "maxLength": 5462,
                            "pattern": "^[A-Za-z0-9_-]+$"
                        }
                    },
                    "required": ["encoding", "value"]
                }
            },
            "required": ["root"]
        })
    }
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct FileImportInput {
    /// Exact Anytype space ID or configured resolvable space name.
    #[schemars(length(min = 1, max = 512))]
    space: String,
    /// Authorized root-relative local source.
    source: ArtifactSource,
    /// Bounded display name assigned to the Anytype file.
    #[schemars(length(min = 1, max = 255))]
    name: String,
    /// Optional canonical MIME essence asserted by the caller.
    #[serde(default)]
    #[schemars(schema_with = "optional_media_type_schema")]
    media_type: Omittable<String>,
    /// Stable caller key preventing an accidental duplicate mutation.
    #[schemars(length(min = 1, max = 256))]
    idempotency_key: String,
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct FileExportInput {
    /// Exact Anytype space ID or configured resolvable space name.
    #[schemars(length(min = 1, max = 512))]
    space: String,
    /// Exact Anytype file object identifier.
    #[schemars(length(min = 1, max = 255))]
    file_id: String,
    /// Authorized create-new root-relative local destination.
    destination: ArtifactDestination,
    /// Optional strong representation validator required by the caller.
    #[serde(default)]
    #[schemars(schema_with = "optional_etag_schema")]
    expected_strong_etag: Omittable<String>,
    /// Stable caller key binding this publication attempt.
    #[schemars(length(min = 1, max = 256))]
    idempotency_key: String,
}

#[derive(Clone, Copy, Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum DocumentSourceFormat {
    /// Interpret the artifact as complete Markdown.
    Markdown,
    /// Escape the artifact as literal plain text before dispatch.
    PlainText,
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct DocumentImportCreateInput {
    /// Exact Anytype space ID or configured resolvable space name.
    #[schemars(length(min = 1, max = 512))]
    space: String,
    /// Authorized local or staged UTF-8 source.
    source: ArtifactSource,
    /// Whether source bytes are complete Markdown or literal plain text.
    source_format: DocumentSourceFormat,
    /// Exact Anytype type key, ID, or unambiguous name.
    #[schemars(length(min = 1, max = 512))]
    object_type: String,
    /// Bounded display name for the new object.
    #[schemars(length(min = 1, max = 512))]
    name: String,
    /// Optional bounded typed property assignments validated against the resolved type.
    #[serde(default)]
    #[schemars(schema_with = "optional_mutation_properties_schema")]
    properties: Omittable<MutationProperties>,
    /// Stable caller key preventing an accidental duplicate create.
    #[schemars(length(min = 1, max = 256))]
    idempotency_key: String,
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct DocumentImportUpdateInput {
    /// Exact Anytype space ID or configured resolvable space name.
    #[schemars(length(min = 1, max = 512))]
    space: String,
    /// Exact Anytype object identifier; names are never guessed.
    #[schemars(length(min = 1, max = 255))]
    object_id: String,
    /// Authorized local or staged UTF-8 replacement body.
    source: ArtifactSource,
    /// Whether source bytes are complete Markdown or literal plain text.
    source_format: DocumentSourceFormat,
    /// Required SHA-256 of the current complete canonical Markdown body.
    #[schemars(length(equal = 64), regex(pattern = "^[0-9a-f]{64}$"))]
    expected_body_sha256: String,
    /// Stable caller key preventing an accidental second replacement.
    #[schemars(length(min = 1, max = 256))]
    idempotency_key: String,
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct DocumentExportInput {
    /// Exact Anytype space ID or configured resolvable space name.
    #[schemars(length(min = 1, max = 512))]
    space: String,
    /// Exact Anytype object identifier; names are never guessed.
    #[schemars(length(min = 1, max = 255))]
    object_id: String,
    /// Authorized create-new local or remote destination.
    destination: ArtifactDestination,
    /// Optional SHA-256 precondition for the canonical Markdown body.
    #[serde(default)]
    #[schemars(schema_with = "optional_sha256_schema")]
    expected_body_sha256: Omittable<String>,
    /// Stable caller key binding this create-new publication attempt.
    #[schemars(length(min = 1, max = 256))]
    idempotency_key: String,
}

fn optional_media_type_schema(_: &mut SchemaGenerator) -> Schema {
    json_schema!({
        "type": "string",
        "minLength": 3,
        "maxLength": 255,
        "pattern": "^[A-Za-z0-9!#$&^_.+-]+/[A-Za-z0-9!#$&^_.+-]+$"
    })
}

fn optional_mutation_properties_schema(generator: &mut SchemaGenerator) -> Schema {
    generator.subschema_for::<MutationProperties>()
}

fn optional_etag_schema(_: &mut SchemaGenerator) -> Schema {
    json_schema!({
        "type": "string",
        "minLength": 2,
        "maxLength": 256,
        "pattern": "^\"[\\x21\\x23-\\x7e]*\"$"
    })
}

fn optional_handle_schema(_: &mut SchemaGenerator) -> Schema {
    json_schema!({
        "type": "string",
        "minLength": 64,
        "maxLength": 128,
        "pattern": "^[A-Za-z0-9_-]+$"
    })
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct StageAllocateInput {
    /// Exact Anytype space ID or configured resolvable space name.
    #[schemars(length(min = 1, max = 512))]
    space: String,
    /// Exact complete upload length reserved against staging quota.
    #[schemars(schema_with = "nonempty_artifact_size_schema")]
    size_bytes: u64,
    /// Optional canonical MIME essence asserted by the caller.
    #[serde(default)]
    #[schemars(schema_with = "optional_media_type_schema")]
    media_type: Omittable<String>,
    /// Optional expected lowercase SHA-256 checked before the stage becomes ready.
    #[serde(default)]
    #[schemars(schema_with = "optional_sha256_schema")]
    expected_sha256: Omittable<String>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct StageHandleInput {
    /// Opaque staging bearer returned by allocation or remote export.
    #[schemars(length(min = 64, max = 128))]
    handle: String,
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct StageAllocationOutput {
    /// Bounded non-secret record identifier used only in the staging route.
    #[schemars(length(equal = 32))]
    record: String,
    /// Opaque staging bearer credential; never place it in a URL.
    #[schemars(length(min = 64, max = 128))]
    handle: String,
    /// Fixed operator-configured upload URL containing only the record ID.
    #[schemars(length(min = 1, max = 2048))]
    upload_url: String,
    /// Presentation expiry timestamp; monotonic time governs authority.
    #[schemars(length(min = 20, max = 64))]
    expires_at: String,
    /// Exact complete upload length reserved for this record.
    #[schemars(schema_with = "nonempty_artifact_size_schema")]
    size_bytes: u64,
    /// Initial or resumable committed byte offset.
    #[schemars(schema_with = "artifact_offset_schema")]
    offset: u64,
}

#[derive(Clone, Copy, Debug, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct StageReleaseOutput {
    /// Whether the exact authenticated record was released.
    released: bool,
}

fn optional_sha256_schema(_: &mut SchemaGenerator) -> Schema {
    json_schema!({
        "type": "string",
        "minLength": 64,
        "maxLength": 64,
        "pattern": "^[0-9a-f]{64}$"
    })
}

fn artifact_offset_schema(_: &mut SchemaGenerator) -> Schema {
    json_schema!({
        "type": "integer",
        "minimum": 0,
        "maximum": 1_073_741_824
    })
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ArtifactStatusOutput {
    /// Whether local root capabilities were activated for this process.
    local_roots_active: bool,
    /// Number of effective local import roots, without revealing their IDs.
    #[schemars(schema_with = "root_count_schema")]
    import_root_count: u32,
    /// Number of effective local export roots, without revealing their IDs.
    #[schemars(schema_with = "root_count_schema")]
    export_root_count: u32,
    /// Whether the selected startup policy declares remote staging.
    staging_configured: bool,
    /// Whether the supervised staging service is accepting requests.
    staging_active: bool,
    /// Remaining aggregate staging byte capacity, without record metadata.
    #[schemars(schema_with = "staging_quota_bytes_schema")]
    staging_available_bytes: u64,
    /// Remaining staging record capacity, without record metadata.
    #[schemars(schema_with = "staging_quota_entries_schema")]
    staging_available_entries: u32,
    /// Number of configured validator policies, without revealing their IDs.
    #[schemars(schema_with = "root_count_schema")]
    validator_count: u32,
    /// Number of validators whose executable and platform boundary were admitted.
    #[schemars(schema_with = "root_count_schema")]
    validator_available_count: u32,
}

fn root_count_schema(_: &mut SchemaGenerator) -> Schema {
    json_schema!({
        "type": "integer",
        "minimum": 0,
        "maximum": 64
    })
}

fn staging_quota_bytes_schema(_: &mut SchemaGenerator) -> Schema {
    json_schema!({
        "type": "integer",
        "minimum": 0,
        "maximum": 17_179_869_184_u64
    })
}

fn staging_quota_entries_schema(_: &mut SchemaGenerator) -> Schema {
    json_schema!({
        "type": "integer",
        "minimum": 0,
        "maximum": 4_096
    })
}

#[derive(Clone, Copy, Debug, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum ArtifactDirection {
    Import,
    Export,
}

#[derive(Clone, Copy, Debug, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum ArtifactState {
    Consumed,
    Available,
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ArtifactReceipt {
    /// Direction in which bytes moved relative to Anytype.
    direction: ArtifactDirection,
    /// Terminal state proven by this operation.
    state: ArtifactState,
    /// Canonical Anytype space identity bound to the receipt.
    #[schemars(length(min = 1, max = 512))]
    space_id: String,
    /// Exact byte length of the verified artifact.
    #[schemars(schema_with = "artifact_size_schema")]
    size_bytes: u64,
    /// Lowercase SHA-256 of the verified bytes.
    #[schemars(length(equal = 64), regex(pattern = "^[0-9a-f]{64}$"))]
    sha256: String,
    /// Canonical MIME essence asserted at import, when supplied.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(length(min = 3, max = 255))]
    declared_media_type: Option<String>,
    /// Canonical MIME essence reported by the completed representation.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(length(min = 3, max = 255))]
    stored_media_type: Option<String>,
    /// Logical root ID used without exposing a physical path.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(length(min = 1, max = 128))]
    root_id: Option<String>,
    /// Non-secret remote record identifier when staged.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(length(equal = 32))]
    staging_record: Option<String>,
    /// Opaque remote bearer returned separately from the staging URL.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(length(min = 64, max = 128))]
    staging_handle: Option<String>,
    /// Fixed remote download URL containing no bearer credential.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(length(min = 1, max = 2048))]
    staging_url: Option<String>,
    /// Bounded results from startup-configured validator policies.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    #[schemars(length(max = 16))]
    validators: Vec<ValidatorFinding>,
}

fn artifact_size_schema(_: &mut SchemaGenerator) -> Schema {
    json_schema!({
        "type": "integer",
        "minimum": 0,
        "maximum": 1_073_741_824
    })
}

fn nonempty_artifact_size_schema(_: &mut SchemaGenerator) -> Schema {
    json_schema!({
        "type": "integer",
        "minimum": 1,
        "maximum": 1_073_741_824
    })
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct FileImportOutput {
    /// Canonical Anytype file object created or safely reused.
    #[schemars(length(min = 1, max = 255))]
    file_id: String,
    /// Bounded proof of verified import completion.
    receipt: ArtifactReceipt,
    /// Whether the result came from verified idempotency state.
    reused: bool,
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct FileExportOutput {
    /// Canonical Anytype file object exported.
    #[schemars(length(min = 1, max = 255))]
    file_id: String,
    /// Bounded proof of verified create-new publication.
    receipt: ArtifactReceipt,
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct DocumentMutationOutput {
    /// Canonical Anytype space identity.
    #[schemars(length(min = 1, max = 512))]
    space_id: String,
    /// Exact created or updated Anytype object identity.
    #[schemars(length(min = 1, max = 255))]
    object_id: String,
    /// SHA-256 of the exact authorized source bytes.
    #[schemars(length(equal = 64), regex(pattern = "^[0-9a-f]{64}$"))]
    source_sha256: String,
    /// SHA-256 of the complete Markdown dispatched to Anytype.
    #[schemars(length(equal = 64), regex(pattern = "^[0-9a-f]{64}$"))]
    dispatched_sha256: String,
    /// SHA-256 of the complete canonical Markdown read back from Anytype.
    #[schemars(length(equal = 64), regex(pattern = "^[0-9a-f]{64}$"))]
    canonical_sha256: String,
    /// Exact source byte count.
    #[schemars(schema_with = "document_size_schema")]
    source_bytes: u64,
    /// Exact dispatched Unicode scalar count.
    #[schemars(schema_with = "document_chars_schema")]
    dispatched_chars: usize,
    /// Whether update verification proved that no mutation was necessary.
    no_op: bool,
    /// Whether a ready remote source was consumed after verified success.
    source_consumed: bool,
    /// Whether a completed process-generation idempotency result was reused.
    reused: bool,
    /// Bounded results from startup-configured validator policies.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    #[schemars(length(max = 16))]
    validators: Vec<ValidatorFinding>,
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct DocumentExportOutput {
    /// Canonical Anytype space identity.
    #[schemars(length(min = 1, max = 512))]
    space_id: String,
    /// Exact exported Anytype object identity.
    #[schemars(length(min = 1, max = 255))]
    object_id: String,
    /// Exact canonical UTF-8 byte count.
    #[schemars(schema_with = "document_size_schema")]
    size_bytes: u64,
    /// Exact canonical Unicode scalar count.
    #[schemars(schema_with = "document_chars_schema")]
    chars: usize,
    /// SHA-256 of the exact canonical bytes written.
    #[schemars(length(equal = 64), regex(pattern = "^[0-9a-f]{64}$"))]
    sha256: String,
    /// Bounded proof of create-new publication.
    receipt: ArtifactReceipt,
    /// Whether a completed process-generation idempotency result was reused.
    reused: bool,
}

fn document_size_schema(_: &mut SchemaGenerator) -> Schema {
    json_schema!({
        "type": "integer",
        "minimum": 0,
        "maximum": 67_108_864
    })
}

fn document_chars_schema(_: &mut SchemaGenerator) -> Schema {
    json_schema!({
        "type": "integer",
        "minimum": 0,
        "maximum": 1_000_000
    })
}

fn status_tool() -> Result<WorkflowTool<ArtifactStatusOutput>, SchemaContractError> {
    workflow_tool::<EmptyInput, ArtifactStatusOutput>(
        ARTIFACT_STATUS,
        "Inspect bounded artifact capability status. Returns counts and activation categories only; never paths, handles, credentials, or payloads.",
        ToolProfile::Read,
    )
}

fn import_tool() -> Result<WorkflowTool<FileImportOutput>, SchemaContractError> {
    workflow_tool::<FileImportInput, FileImportOutput>(
        FILE_IMPORT,
        "Stream one complete authorized local file into one Anytype space, verify exact stored bytes, and return only bounded identity and receipt metadata.",
        ToolProfile::Create,
    )
}

fn export_tool() -> Result<WorkflowTool<FileExportOutput>, SchemaContractError> {
    workflow_tool::<FileExportInput, FileExportOutput>(
        FILE_EXPORT,
        "Stream one exact Anytype file to an authorized local create-new destination, verify size and SHA-256, and return only bounded receipt metadata.",
        ToolProfile::Update,
    )
}

fn document_import_create_tool() -> Result<WorkflowTool<DocumentMutationOutput>, SchemaContractError>
{
    workflow_tool::<DocumentImportCreateInput, DocumentMutationOutput>(
        DOCUMENT_IMPORT_CREATE,
        "Create one Anytype document from a complete authorized UTF-8 Markdown or plain-text artifact. Payload bytes never enter MCP arguments or results; source and canonical hashes are reported separately because Anytype may canonicalize Markdown.",
        ToolProfile::Create,
    )
}

fn document_import_update_tool() -> Result<WorkflowTool<DocumentMutationOutput>, SchemaContractError>
{
    workflow_tool::<DocumentImportUpdateInput, DocumentMutationOutput>(
        DOCUMENT_IMPORT_UPDATE,
        "Replace one exact Anytype document body from a complete authorized UTF-8 artifact after checking the required current-body hash. A canonical no-op performs no mutation.",
        ToolProfile::Update,
    )
}

fn document_export_tool() -> Result<WorkflowTool<DocumentExportOutput>, SchemaContractError> {
    workflow_tool::<DocumentExportInput, DocumentExportOutput>(
        DOCUMENT_EXPORT,
        "Write one exact complete canonical Anytype Markdown body to an authorized create-new destination and return only bounded identity, counts, hashes, and receipt metadata.",
        ToolProfile::Update,
    )
}

fn stage_allocate_tool() -> Result<WorkflowTool<StageAllocationOutput>, SchemaContractError> {
    workflow_tool::<StageAllocateInput, StageAllocationOutput>(
        ARTIFACT_STAGE_ALLOCATE,
        "Allocate one exact-size authenticated remote import stage. Returns an opaque bearer separately from the fixed upload URL and never carries artifact bytes.",
        ToolProfile::Create,
    )
}

fn stage_release_tool() -> Result<WorkflowTool<StageReleaseOutput>, SchemaContractError> {
    workflow_tool::<StageHandleInput, StageReleaseOutput>(
        ARTIFACT_STAGE_RELEASE,
        "Release one exact authenticated staging record and its private bytes.",
        ToolProfile::Update,
    )
}

#[derive(Clone, Copy, Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct EmptyInput {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ArtifactToolError {
    Validation,
    MissingRoots,
    MissingStaging,
    ReadOnly,
    Authentication,
    NotFound,
    Bounded,
    Conflict,
    Upstream,
    Indeterminate,
}

#[derive(Clone, Debug)]
pub(crate) enum ImportIdempotency {
    Dispatch,
    VerifyCandidate {
        candidate: EntityId,
        validator_findings: Vec<ValidatorFinding>,
    },
    Reuse(Box<FileImportOutput>),
}

#[derive(Clone, Debug)]
enum ExportIdempotency {
    Dispatch,
    Reuse(Box<FileExportOutput>),
}

#[derive(Clone, Debug)]
enum DocumentMutationIdempotency {
    Dispatch,
    VerifyCandidate {
        object_id: EntityId,
        canonical_sha256: String,
    },
    Reuse(DocumentMutationOutput),
}

#[derive(Clone, Debug)]
enum DocumentExportIdempotency {
    Dispatch,
    Reuse(Box<DocumentExportOutput>),
}

#[derive(Clone, Debug)]
enum OperationOutcome {
    ImportInFlight,
    ImportVerifying {
        candidate: EntityId,
        validator_findings: Vec<ValidatorFinding>,
    },
    ImportCleaning(EntityId),
    ImportCandidate {
        candidate: EntityId,
        validator_findings: Vec<ValidatorFinding>,
    },
    ImportIndeterminate(EntityId),
    ImportComplete(FileImportOutput),
    ExportInFlight,
    ExportComplete(FileExportOutput),
    DocumentMutationInFlight,
    DocumentMutationCandidate {
        object_id: EntityId,
        canonical_sha256: String,
    },
    DocumentMutationComplete(DocumentMutationOutput),
    DocumentExportInFlight,
    DocumentExportComplete(DocumentExportOutput),
    Indeterminate,
}

impl OperationOutcome {
    fn retained_import_candidate(&self) -> Option<&EntityId> {
        match self {
            Self::ImportVerifying { candidate, .. }
            | Self::ImportCleaning(candidate)
            | Self::ImportCandidate { candidate, .. }
            | Self::ImportIndeterminate(candidate) => Some(candidate),
            _ => None,
        }
    }
}

#[derive(Clone, Debug)]
struct OperationEntry {
    fingerprint: [u8; 32],
    outcome: OperationOutcome,
}

/// Process-generation idempotency ledger for artifact mutations.
///
/// Keys and fingerprints are retained only as SHA-256 values. The ledger
/// prevents a second upload or publication after an uncertain first dispatch.
#[derive(Clone, Debug, Default)]
pub(crate) struct ArtifactOperationState {
    entries: Arc<Mutex<HashMap<[u8; 32], OperationEntry>>>,
}

impl ArtifactOperationState {
    fn entries(&self) -> MutexGuard<'_, HashMap<[u8; 32], OperationEntry>> {
        match self.entries.lock() {
            Ok(entries) => entries,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    pub(crate) fn settle_import_timeout_now(&self, key: [u8; 32]) {
        if let Some(entry) = self.entries().get_mut(&key) {
            entry.outcome = match &entry.outcome {
                OperationOutcome::ImportVerifying {
                    candidate,
                    validator_findings,
                } => OperationOutcome::ImportCandidate {
                    candidate: candidate.clone(),
                    validator_findings: validator_findings.clone(),
                },
                OperationOutcome::ImportCleaning(candidate) => {
                    OperationOutcome::ImportIndeterminate(candidate.clone())
                }
                OperationOutcome::ImportInFlight => OperationOutcome::Indeterminate,
                outcome => outcome.clone(),
            };
        }
    }

    pub(crate) async fn settle_import_timeout(&self, key: [u8; 32]) {
        self.settle_import_timeout_now(key);
    }

    pub(crate) fn mark_indeterminate(&self, key: [u8; 32]) {
        if let Some(entry) = self.entries().get_mut(&key) {
            entry.outcome = OperationOutcome::Indeterminate;
        }
    }

    #[cfg(test)]
    pub(crate) async fn reserve_import(
        &self,
        key: [u8; 32],
        fingerprint: [u8; 32],
    ) -> Result<ImportIdempotency, ArtifactToolError> {
        self.reserve_import_now(key, fingerprint)
    }

    pub(crate) fn reserve_import_now(
        &self,
        key: [u8; 32],
        fingerprint: [u8; 32],
    ) -> Result<ImportIdempotency, ArtifactToolError> {
        let mut entries = self.entries();
        let Some(entry) = entries.get(&key) else {
            entries.insert(
                key,
                OperationEntry {
                    fingerprint,
                    outcome: OperationOutcome::ImportInFlight,
                },
            );
            return Ok(ImportIdempotency::Dispatch);
        };
        if entry.fingerprint != fingerprint {
            return Err(ArtifactToolError::Conflict);
        }
        let _ = entry.outcome.retained_import_candidate();
        match &entry.outcome {
            OperationOutcome::ImportCandidate {
                candidate,
                validator_findings,
            } => Ok(ImportIdempotency::VerifyCandidate {
                candidate: candidate.clone(),
                validator_findings: validator_findings.clone(),
            }),
            OperationOutcome::ImportComplete(output) => {
                Ok(ImportIdempotency::Reuse(Box::new(output.clone())))
            }
            OperationOutcome::ImportInFlight
            | OperationOutcome::ImportVerifying { .. }
            | OperationOutcome::ImportCleaning(_)
            | OperationOutcome::ImportIndeterminate(_)
            | OperationOutcome::ExportInFlight
            | OperationOutcome::ExportComplete(_)
            | OperationOutcome::DocumentMutationInFlight
            | OperationOutcome::DocumentMutationCandidate { .. }
            | OperationOutcome::DocumentMutationComplete(_)
            | OperationOutcome::DocumentExportInFlight
            | OperationOutcome::DocumentExportComplete(_)
            | OperationOutcome::Indeterminate => Err(ArtifactToolError::Indeterminate),
        }
    }

    async fn reserve_export(
        &self,
        key: [u8; 32],
        fingerprint: [u8; 32],
    ) -> Result<ExportIdempotency, ArtifactToolError> {
        let mut entries = self.entries();
        let Some(entry) = entries.get(&key) else {
            entries.insert(
                key,
                OperationEntry {
                    fingerprint,
                    outcome: OperationOutcome::ExportInFlight,
                },
            );
            return Ok(ExportIdempotency::Dispatch);
        };
        if entry.fingerprint != fingerprint {
            return Err(ArtifactToolError::Conflict);
        }
        match &entry.outcome {
            OperationOutcome::ExportComplete(output) => {
                Ok(ExportIdempotency::Reuse(Box::new(output.clone())))
            }
            OperationOutcome::ImportInFlight
            | OperationOutcome::ImportVerifying { .. }
            | OperationOutcome::ImportCleaning(_)
            | OperationOutcome::ImportCandidate { .. }
            | OperationOutcome::ImportIndeterminate(_)
            | OperationOutcome::ImportComplete(_)
            | OperationOutcome::ExportInFlight
            | OperationOutcome::DocumentMutationInFlight
            | OperationOutcome::DocumentMutationCandidate { .. }
            | OperationOutcome::DocumentMutationComplete(_)
            | OperationOutcome::DocumentExportInFlight
            | OperationOutcome::DocumentExportComplete(_)
            | OperationOutcome::Indeterminate => Err(ArtifactToolError::Indeterminate),
        }
    }

    async fn reserve_document_mutation(
        &self,
        key: [u8; 32],
        fingerprint: [u8; 32],
    ) -> Result<DocumentMutationIdempotency, ArtifactToolError> {
        let mut entries = self.entries();
        let Some(entry) = entries.get(&key) else {
            entries.insert(
                key,
                OperationEntry {
                    fingerprint,
                    outcome: OperationOutcome::DocumentMutationInFlight,
                },
            );
            return Ok(DocumentMutationIdempotency::Dispatch);
        };
        if entry.fingerprint != fingerprint {
            return Err(ArtifactToolError::Conflict);
        }
        match &entry.outcome {
            OperationOutcome::DocumentMutationCandidate {
                object_id,
                canonical_sha256,
            } => Ok(DocumentMutationIdempotency::VerifyCandidate {
                object_id: object_id.clone(),
                canonical_sha256: canonical_sha256.clone(),
            }),
            OperationOutcome::DocumentMutationComplete(output) => {
                Ok(DocumentMutationIdempotency::Reuse(output.clone()))
            }
            _ => Err(ArtifactToolError::Indeterminate),
        }
    }

    async fn reserve_document_export(
        &self,
        key: [u8; 32],
        fingerprint: [u8; 32],
    ) -> Result<DocumentExportIdempotency, ArtifactToolError> {
        let mut entries = self.entries();
        let Some(entry) = entries.get(&key) else {
            entries.insert(
                key,
                OperationEntry {
                    fingerprint,
                    outcome: OperationOutcome::DocumentExportInFlight,
                },
            );
            return Ok(DocumentExportIdempotency::Dispatch);
        };
        if entry.fingerprint != fingerprint {
            return Err(ArtifactToolError::Conflict);
        }
        match &entry.outcome {
            OperationOutcome::DocumentExportComplete(output) => {
                Ok(DocumentExportIdempotency::Reuse(Box::new(output.clone())))
            }
            _ => Err(ArtifactToolError::Indeterminate),
        }
    }

    async fn set_outcome(&self, key: [u8; 32], outcome: OperationOutcome) {
        self.set_outcome_now(key, outcome);
    }

    fn set_outcome_now(&self, key: [u8; 32], outcome: OperationOutcome) {
        if let Some(entry) = self.entries().get_mut(&key) {
            entry.outcome = outcome;
        }
    }

    async fn remove(&self, key: [u8; 32]) {
        self.entries().remove(&key);
    }
}

impl ArtifactToolError {
    fn tool_error(self) -> ToolError {
        match self {
            Self::Validation => ToolError::validation(),
            Self::MissingRoots => ToolError::validation_message(ROOTS_REQUIRED_GUIDANCE),
            Self::MissingStaging => ToolError::validation_message(STAGING_REQUIRED_GUIDANCE),
            Self::ReadOnly => ToolError::read_only(),
            Self::Authentication => ToolError::authentication(),
            Self::NotFound => ToolError::not_found(),
            Self::Bounded => ToolError::bounded_result(),
            Self::Conflict => ToolError::conflict(),
            Self::Upstream => ToolError::upstream(),
            Self::Indeterminate => ToolError::mutation_indeterminate(),
        }
    }
}

fn staging(runtime: &RuntimeContext) -> Result<&ArtifactStaging, ArtifactToolError> {
    runtime
        .artifact_staging()
        .ok_or(ArtifactToolError::MissingStaging)
}

fn classify_staging_error(error: StagingError) -> ArtifactToolError {
    match error {
        StagingError::Disabled => ArtifactToolError::MissingStaging,
        StagingError::InvalidPolicy | StagingError::Reconciliation => ArtifactToolError::Upstream,
        StagingError::NotFound => ArtifactToolError::NotFound,
        StagingError::BadRequest => ArtifactToolError::Validation,
        StagingError::Conflict => ArtifactToolError::Conflict,
        StagingError::Bounded => ArtifactToolError::Bounded,
        StagingError::Timeout => ArtifactToolError::Upstream,
        StagingError::Upstream => ArtifactToolError::Upstream,
        StagingError::Indeterminate => ArtifactToolError::Indeterminate,
    }
}

impl fmt::Display for ArtifactToolError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("artifact operation failed")
    }
}

fn validate_name(name: &str) -> Result<(), ArtifactToolError> {
    if name.is_empty()
        || name.len() > 255
        || name.chars().any(char::is_control)
        || name.contains(['/', '\\'])
        || matches!(name, "." | "..")
    {
        return Err(ArtifactToolError::Validation);
    }
    Ok(())
}

fn normalize_media_type(value: Option<&str>) -> Result<Option<String>, ArtifactToolError> {
    value
        .map(|value| {
            if value.len() > 255 || value.contains(';') {
                return Err(ArtifactToolError::Validation);
            }
            let parsed = value
                .parse::<mime::Mime>()
                .map_err(|_| ArtifactToolError::Validation)?;
            let normalized = format!("{}/{}", parsed.type_(), parsed.subtype());
            if normalized != value.to_ascii_lowercase() {
                return Err(ArtifactToolError::Validation);
            }
            Ok(normalized)
        })
        .transpose()
}

fn stored_media_type(value: Option<&str>) -> Result<Option<String>, ArtifactToolError> {
    value
        .map(|value| {
            if value.len() > 255 {
                return Err(ArtifactToolError::Upstream);
            }
            let parsed = value
                .parse::<mime::Mime>()
                .map_err(|_| ArtifactToolError::Upstream)?;
            Ok(format!("{}/{}", parsed.type_(), parsed.subtype()))
        })
        .transpose()
}

/// Resolves effective local root authority for one operation.
///
/// On a transport that carries one terminal client session, the configured
/// static roots are narrowed by a single bounded `roots/list` snapshot; every
/// other transport keeps the static policy unchanged. A snapshot that cannot
/// be securely frozen disables local roots for the session instead of falling
/// back to the broader static policy.
async fn roots(runtime: &RuntimeContext) -> Result<EffectiveRootRegistry, ArtifactToolError> {
    let registry = runtime
        .artifact_roots()
        .ok_or(ArtifactToolError::MissingRoots)?;
    runtime
        .client_roots()
        .effective(registry, runtime.request_timeout())
        .await
        .map_err(|error| classify_root_error(&error))
}

fn classify_root_error(error: &RootAccessError) -> ArtifactToolError {
    match error.kind() {
        RootAccessErrorKind::Missing => ArtifactToolError::MissingRoots,
        RootAccessErrorKind::Unauthorized | RootAccessErrorKind::Containment => {
            ArtifactToolError::NotFound
        }
        RootAccessErrorKind::TooLarge => ArtifactToolError::Bounded,
        RootAccessErrorKind::Collision | RootAccessErrorKind::Changed => {
            ArtifactToolError::Conflict
        }
        RootAccessErrorKind::Indeterminate => ArtifactToolError::Indeterminate,
        RootAccessErrorKind::Activation | RootAccessErrorKind::ClientRoots => {
            ArtifactToolError::Upstream
        }
        #[cfg(not(any(unix, windows)))]
        RootAccessErrorKind::Platform => ArtifactToolError::Upstream,
    }
}

fn sha256_hex(hasher: Sha256) -> String {
    let digest = hasher.finalize();
    let mut encoded = String::with_capacity(64);
    for byte in digest {
        let _ = std::fmt::Write::write_fmt(&mut encoded, format_args!("{byte:02x}"));
    }
    encoded
}

fn digest_fields(domain: &[u8], fields: &[&[u8]]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(domain);
    for field in fields {
        hasher.update((field.len() as u64).to_be_bytes());
        hasher.update(field);
    }
    hasher.finalize().into()
}

fn idempotency_key(direction: &[u8], key: &str) -> [u8; 32] {
    digest_fields(
        b"any-mcp/artifact/idempotency/v1",
        &[direction, key.as_bytes()],
    )
}

fn location_fingerprint(location: &LocalLocation) -> [u8; 32] {
    match (location.path.as_ref(), location.path_native.as_ref()) {
        (Some(path), None) => digest_fields(
            b"any-mcp/artifact/location/utf8/v1",
            &[location.root.as_bytes(), path.as_bytes()],
        ),
        (None, Some(path)) => digest_fields(
            b"any-mcp/artifact/location/native/v1",
            &[
                location.root.as_bytes(),
                path.encoding.as_bytes(),
                path.value.as_bytes(),
            ],
        ),
        (Some(_), Some(_)) | (None, None) => {
            digest_fields(b"any-mcp/artifact/location/invalid/v1", &[])
        }
    }
}

fn import_fingerprint(
    space_id: &SpaceId,
    size: u64,
    sha256: &str,
    name: &str,
    media_type: Option<&str>,
) -> [u8; 32] {
    digest_fields(
        b"any-mcp/artifact/import/v1",
        &[
            space_id.as_str().as_bytes(),
            &size.to_be_bytes(),
            sha256.as_bytes(),
            name.as_bytes(),
            media_type.unwrap_or("").as_bytes(),
        ],
    )
}

fn export_fingerprint(
    space_id: &SpaceId,
    file_id: &EntityId,
    destination: &ResolvedDestination,
    expected_etag: Option<&str>,
) -> [u8; 32] {
    let location = match destination {
        ResolvedDestination::Local(location) => location_fingerprint(location),
        ResolvedDestination::Remote => digest_fields(b"any-mcp/artifact/location/remote/v1", &[]),
    };
    digest_fields(
        b"any-mcp/artifact/export/v1",
        &[
            space_id.as_str().as_bytes(),
            file_id.as_str().as_bytes(),
            &location,
            expected_etag.unwrap_or("").as_bytes(),
        ],
    )
}

fn hash_import(
    mut source: AnchoredImport,
    chunk_bytes: u64,
) -> Result<(AnchoredImport, String), ArtifactToolError> {
    let capacity = usize::try_from(chunk_bytes).map_err(|_| ArtifactToolError::Validation)?;
    let mut buffer = vec![0_u8; capacity];
    let mut hasher = Sha256::new();
    let mut observed = 0_u64;
    loop {
        let read = source
            .reader()
            .read(&mut buffer)
            .map_err(|_| ArtifactToolError::NotFound)?;
        if read == 0 {
            break;
        }
        observed = observed
            .checked_add(read as u64)
            .ok_or(ArtifactToolError::Validation)?;
        if observed > source.length {
            return Err(ArtifactToolError::Conflict);
        }
        hasher.update(&buffer[..read]);
    }
    if observed != source.length {
        return Err(ArtifactToolError::Conflict);
    }
    source
        .reader()
        .seek(SeekFrom::Start(0))
        .map_err(|_| ArtifactToolError::NotFound)?;
    source
        .verify_unchanged()
        .map_err(|error| classify_root_error(&error))?;
    Ok((source, sha256_hex(hasher)))
}

enum PreparedImport {
    Local {
        source: AnchoredImport,
        sha256: String,
        root_id: String,
    },
    Staged(StageSource),
    StagedReplay(RetainedStageImport),
}

impl PreparedImport {
    fn length(&self) -> u64 {
        match self {
            Self::Local { source, .. } => source.length,
            Self::Staged(source) => source.length,
            Self::StagedReplay(source) => source.length,
        }
    }

    fn sha256(&self) -> &str {
        match self {
            Self::Local { sha256, .. } => sha256,
            Self::Staged(source) => &source.sha256,
            Self::StagedReplay(source) => &source.sha256,
        }
    }

    fn stored_media_type(&self) -> Option<&str> {
        match self {
            Self::Local { .. } => None,
            Self::Staged(source) => source.media_type.as_deref(),
            Self::StagedReplay(source) => source.media_type.as_deref(),
        }
    }

    fn try_clone_reader(&self) -> Result<File, ArtifactToolError> {
        match self {
            Self::Local { source, .. } => source
                .try_clone_reader()
                .map_err(|error| classify_root_error(&error)),
            Self::Staged(source) => source.try_clone_reader().map_err(classify_staging_error),
            Self::StagedReplay(_) => Err(ArtifactToolError::NotFound),
        }
    }

    fn verify_unchanged(&self) -> Result<(), ArtifactToolError> {
        match self {
            Self::Local { source, .. } => source
                .verify_unchanged()
                .map_err(|error| classify_root_error(&error)),
            Self::Staged(_) | Self::StagedReplay(_) => Ok(()),
        }
    }

    fn verify_before_dispatch(&self) -> Result<(), ArtifactToolError> {
        match self {
            Self::Local { source, .. } => source
                .verify_unchanged()
                .map_err(|error| classify_root_error(&error)),
            Self::Staged(_) | Self::StagedReplay(_) => Ok(()),
        }
    }

    async fn mark_import_dispatched(
        &mut self,
        runtime: &RuntimeContext,
    ) -> Result<(), ArtifactToolError> {
        match self {
            Self::Staged(source) => staging(runtime)?
                .mark_import_dispatched(source)
                .await
                .map_err(classify_staging_error),
            Self::Local { .. } => Ok(()),
            Self::StagedReplay(_) => Err(ArtifactToolError::NotFound),
        }
    }

    async fn restore_after_definitive_rejection(
        &mut self,
        runtime: &RuntimeContext,
    ) -> Result<(), ArtifactToolError> {
        match self {
            Self::Staged(source) => staging(runtime)?
                .restore_import_operation(source)
                .await
                .map_err(classify_staging_error),
            Self::Local { .. } => Ok(()),
            Self::StagedReplay(_) => Err(ArtifactToolError::NotFound),
        }
    }

    async fn retain_import_candidate(
        &self,
        runtime: &RuntimeContext,
        candidate: &EntityId,
    ) -> Result<(), ArtifactToolError> {
        match self {
            Self::Staged(source) => staging(runtime)?
                .retain_import_candidate(source, candidate)
                .await
                .map_err(classify_staging_error),
            Self::Local { .. } => Ok(()),
            Self::StagedReplay(_) => Err(ArtifactToolError::NotFound),
        }
    }

    async fn retain_candidate_cleanup(
        &self,
        runtime: &RuntimeContext,
        category: &'static str,
    ) -> Result<(), ArtifactToolError> {
        match self {
            Self::Staged(source) => staging(runtime)?
                .retain_candidate_cleanup(source, category)
                .await
                .map_err(classify_staging_error),
            Self::Local { .. } => Ok(()),
            Self::StagedReplay(_) => Err(ArtifactToolError::NotFound),
        }
    }

    fn root_id(&self) -> Option<String> {
        match self {
            Self::Local { root_id, .. } => Some(root_id.clone()),
            Self::Staged(_) | Self::StagedReplay(_) => None,
        }
    }

    fn staging_record(&self) -> Option<String> {
        match self {
            Self::Local { .. } => None,
            Self::Staged(source) => Some(source.record()),
            Self::StagedReplay(source) => Some(source.record.clone()),
        }
    }
}

async fn run_configured_validators(
    runtime: &RuntimeContext,
    source: &PreparedImport,
    media_type: Option<&str>,
) -> Result<Vec<ValidatorFinding>, ArtifactToolError> {
    let Some(validators) = runtime.artifact_validators() else {
        return Ok(Vec::new());
    };
    let reader = source.try_clone_reader()?;
    validators
        .validate(&reader, source.length(), media_type)
        .await
}

async fn resolve_space(
    client: &PolicyClient,
    reference: &str,
) -> Result<SpaceId, ArtifactToolError> {
    let resolved = client.resolve_space_id(reference).await.map_err(|error| {
        if matches!(error, AnytypeError::Forbidden) {
            ArtifactToolError::Authentication
        } else {
            ArtifactToolError::Upstream
        }
    })?;
    SpaceId::new(resolved).map_err(|_| ArtifactToolError::Upstream)
}

fn classify_anytype_error(error: &AnytypeError) -> ArtifactToolError {
    if error.is_authentication() {
        return ArtifactToolError::Authentication;
    }
    match error {
        AnytypeError::NotFound { .. } | AnytypeError::Forbidden => ArtifactToolError::NotFound,
        AnytypeError::Validation { .. } => ArtifactToolError::Validation,
        AnytypeError::ResponseTooLarge { .. } | AnytypeError::FileHeaderEvidenceTooLarge { .. } => {
            ArtifactToolError::Bounded
        }
        _ => ArtifactToolError::Upstream,
    }
}

fn strong_etag(value: Option<String>) -> Result<Option<String>, ArtifactToolError> {
    value
        .map(|value| {
            if value.len() > 256
                || value.starts_with("W/")
                || !value.starts_with('"')
                || !value.ends_with('"')
            {
                Err(ArtifactToolError::Upstream)
            } else {
                Ok(value)
            }
        })
        .transpose()
}

struct FilePreflight {
    total: u64,
    media_type: Option<String>,
    etag: Option<String>,
}

struct FileStreamRequest<'a> {
    client: &'a PolicyClient,
    space_id: &'a SpaceId,
    file_id: &'a EntityId,
    maximum_bytes: u64,
    chunk_bytes: u64,
    expected_etag: Option<&'a str>,
    expected_sha256: Option<&'a str>,
    cancellation: &'a CancellationToken,
}

fn stream_consistency_proven(
    total: u64,
    chunk_bytes: u64,
    etag: Option<&str>,
    expected_sha256: Option<&str>,
) -> bool {
    total <= chunk_bytes || etag.is_some() || expected_sha256.is_some()
}

async fn preflight_anytype_file(
    client: &PolicyClient,
    space_id: &SpaceId,
    file_id: &EntityId,
    maximum_bytes: u64,
    chunk_bytes: u64,
    expected_etag: Option<&str>,
    expected_sha256: Option<&str>,
) -> Result<FilePreflight, ArtifactToolError> {
    let head = client
        .files()
        .download_request(space_id.as_str(), file_id.as_str())
        .response_limit_bytes(1)
        .error_limit_bytes(ERROR_BYTES)
        .header_evidence_limit_bytes(HEADER_BYTES)
        .max_attempts(6)
        .head()
        .await
        .map_err(|error| classify_anytype_error(&error))?;
    let total = head
        .metadata
        .content_length
        .filter(|size| *size > 0 && *size <= maximum_bytes)
        .ok_or(ArtifactToolError::Bounded)?;
    let etag = strong_etag(head.metadata.etag)?;
    if expected_etag.is_some_and(|expected| etag.as_deref() != Some(expected)) {
        return Err(ArtifactToolError::Conflict);
    }
    if !stream_consistency_proven(total, chunk_bytes, etag.as_deref(), expected_sha256) {
        return Err(ArtifactToolError::Upstream);
    }
    Ok(FilePreflight {
        total,
        media_type: stored_media_type(head.metadata.content_type.as_deref())?,
        etag,
    })
}

async fn stream_anytype_file<W>(
    request: FileStreamRequest<'_>,
    output: W,
) -> Result<(W, u64, String, Option<String>, Option<String>), ArtifactToolError>
where
    W: Write + Send + 'static,
{
    let FileStreamRequest {
        client,
        space_id,
        file_id,
        maximum_bytes,
        chunk_bytes,
        expected_etag,
        expected_sha256,
        cancellation,
    } = request;
    let preflight = preflight_anytype_file(
        client,
        space_id,
        file_id,
        maximum_bytes,
        chunk_bytes,
        expected_etag,
        expected_sha256,
    )
    .await?;
    let FilePreflight {
        total,
        media_type,
        etag,
    } = preflight;
    let (sender, mut receiver) = tokio::sync::mpsc::channel::<bytes::Bytes>(1);
    let writer = tokio::task::spawn_blocking(move || {
        let mut output = output;
        let mut hasher = Sha256::new();
        let mut written = 0_u64;
        while let Some(chunk) = receiver.blocking_recv() {
            output
                .write_all(&chunk)
                .map_err(|_| ArtifactToolError::NotFound)?;
            written = written
                .checked_add(chunk.len() as u64)
                .ok_or(ArtifactToolError::Bounded)?;
            hasher.update(&chunk);
        }
        Ok::<_, ArtifactToolError>((output, written, sha256_hex(hasher)))
    });
    let mut offset = 0_u64;
    let mut transfer_error = None;
    while offset < total {
        if cancellation.is_cancelled() {
            transfer_error = Some(ArtifactToolError::Upstream);
            break;
        }
        let length = total.saturating_sub(offset).min(chunk_bytes);
        let mut request = client
            .files()
            .download_request(space_id.as_str(), file_id.as_str())
            .response_limit_bytes(length.saturating_add(1))
            .error_limit_bytes(ERROR_BYTES)
            .header_evidence_limit_bytes(HEADER_BYTES)
            .max_attempts(6);
        if total > chunk_bytes {
            request = request.byte_range(offset, length);
        }
        if let Some(etag) = etag.as_ref() {
            request = request.if_match(etag);
        }
        let response = match request.download().await {
            Ok(response) => response,
            Err(_) => {
                transfer_error = Some(ArtifactToolError::Upstream);
                break;
            }
        };
        let response_media_type = match stored_media_type(response.metadata.content_type.as_deref())
        {
            Ok(media_type) => media_type,
            Err(error) => {
                transfer_error = Some(error);
                break;
            }
        };
        if response.bytes.len() as u64 != length
            || response.metadata.etag.as_deref() != etag.as_deref()
            || response_media_type != media_type
        {
            transfer_error = Some(ArtifactToolError::Conflict);
            break;
        }
        if sender.send(response.bytes).await.is_err() {
            transfer_error = Some(ArtifactToolError::NotFound);
            break;
        }
        let Some(next_offset) = offset.checked_add(length) else {
            transfer_error = Some(ArtifactToolError::Upstream);
            break;
        };
        offset = next_offset;
    }
    drop(sender);
    let (output, written, sha256) = writer.await.map_err(|_| ArtifactToolError::Upstream)??;
    if let Some(error) = transfer_error {
        return Err(error);
    }
    if written != total {
        return Err(ArtifactToolError::Conflict);
    }
    if expected_sha256.is_some_and(|expected| sha256 != expected) {
        return Err(ArtifactToolError::Conflict);
    }
    Ok((output, total, sha256, media_type, etag))
}

async fn file_import(
    runtime: &RuntimeContext,
    input: FileImportInput,
    cancellation: &CancellationToken,
) -> Result<FileImportOutput, ArtifactToolError> {
    if runtime.is_read_only() {
        return Err(ArtifactToolError::ReadOnly);
    }
    validate_name(&input.name)?;
    let declared_media_type = normalize_media_type(input.media_type.as_ref().map(String::as_str))?;
    let space_id = resolve_space(runtime.client(), &input.space).await?;
    let resolved_source = input.source.resolve()?;
    // Staged metadata is readable without acquiring its one-use authority.
    // This deliberately lets the ledger reject a different operation before
    // any caller can obtain a source reader.
    let staged_handle = match &resolved_source {
        ResolvedSource::Staged(handle) => Some(handle.clone()),
        ResolvedSource::Local(_) => None,
    };
    let mut source = match &staged_handle {
        Some(handle) => match staging(runtime)?.import_source(handle, &space_id).await {
            // Fresh staged imports acquire and clone their one-use authority
            // before reservation.  An unbound lease drops back to Ready if a
            // later ledger decision rejects it.
            Ok(source) => PreparedImport::Staged(source),
            Err(StagingError::NotFound) => staging(runtime)?
                .import_metadata(handle, &space_id)
                .await
                .map(PreparedImport::StagedReplay)
                .map_err(classify_staging_error)?,
            Err(error) => return Err(classify_staging_error(error)),
        },
        None => prepare_import_source(runtime, resolved_source, &space_id).await?,
    };
    if declared_media_type
        .as_deref()
        .zip(source.stored_media_type())
        .is_some_and(|(declared, staged)| declared != staged)
    {
        return Err(ArtifactToolError::Conflict);
    }
    let content_sha256 = source.sha256().to_owned();
    let source_length = source.length();
    if source_length == 0 {
        return Err(ArtifactToolError::Validation);
    }
    let root_id = source.root_id();
    let staging_record = source.staging_record();
    // Candidate and complete replay use retained staged identity only; they do
    // not reopen the consumed source merely to re-run advisory validators.
    let validator_findings = if matches!(source, PreparedImport::StagedReplay(_)) {
        Vec::new()
    } else {
        run_configured_validators(runtime, &source, declared_media_type.as_deref()).await?
    };
    let key = idempotency_key(b"import", &input.idempotency_key);
    let fingerprint = import_fingerprint(
        &space_id,
        source_length,
        &content_sha256,
        &input.name,
        declared_media_type.as_deref(),
    );
    #[cfg(any(test, feature = "acceptance-harness"))]
    if !runtime
        .artifact_acceptance_gates()
        .reach(ArtifactAcceptanceGatePoint::ImportBeforeDispatch, key)
        .await
    {
        return Err(ArtifactToolError::Indeterminate);
    }
    let mut settlement_permit = runtime
        .admit_import_settlement(runtime.request_deadline())
        .await?;
    match settlement_permit.reserve_import(runtime.artifact_operations(), key, fingerprint)? {
        ImportIdempotency::Reuse(mut output) => {
            if let Some(handle) = staged_handle.as_deref() {
                let retained = staging(runtime)?
                    .reconciliation_import(handle, &space_id, key)
                    .await
                    .map_err(classify_staging_error)?;
                if retained.length != source_length || retained.sha256 != content_sha256 {
                    return Err(ArtifactToolError::Conflict);
                }
            }
            output.reused = true;
            drop(settlement_permit);
            return Ok(*output);
        }
        ImportIdempotency::VerifyCandidate {
            candidate,
            validator_findings,
        } => {
            if let Some(handle) = staged_handle.as_deref() {
                let retained = staging(runtime)?
                    .reconciliation_import(handle, &space_id, key)
                    .await
                    .map_err(classify_staging_error)?;
                if retained.length != source_length || retained.sha256 != content_sha256 {
                    return Err(ArtifactToolError::Conflict);
                }
            }
            let stored_media_type = verify_import_candidate(
                runtime,
                &space_id,
                &candidate,
                source_length,
                &content_sha256,
                cancellation,
            )
            .await?;
            if let Some(handle) = staged_handle.as_deref() {
                staging(runtime)?
                    .consume_reconciliation(handle, &space_id, key)
                    .await
                    .map_err(classify_staging_error)?;
            }
            let output = import_output(
                &space_id,
                &candidate,
                source_length,
                content_sha256,
                declared_media_type,
                stored_media_type,
                root_id,
                staging_record,
                validator_findings,
                true,
            );
            runtime
                .artifact_operations()
                .set_outcome(key, OperationOutcome::ImportComplete(output.clone()))
                .await;
            drop(settlement_permit);
            return Ok(output);
        }
        ImportIdempotency::Dispatch => {
            if let Some(handle) = staged_handle {
                let PreparedImport::Staged(staged) = &mut source else {
                    // A different operation held reconciliation authority.
                    // Undo the provisional reservation before reporting the
                    // fixed staged-source failure.
                    runtime.artifact_operations().remove(key).await;
                    return Err(ArtifactToolError::NotFound);
                };
                let _ = handle;
                staging(runtime)?
                    .bind_import_operation(staged, key)
                    .await
                    .map_err(classify_staging_error)?;
            }
        }
    }
    let owned_runtime = runtime.clone();
    let owned_cancellation = CancellationToken::new();
    let operation_timeout = runtime.artifact_config().limits.operation_timeout;
    let receiver = runtime.supervise_import_settlement(key, settlement_permit, async move {
        match tokio::time::timeout(
            operation_timeout,
            settle_reserved_import(
                owned_runtime.clone(),
                source,
                space_id,
                input.name,
                declared_media_type,
                content_sha256,
                source_length,
                root_id,
                staging_record,
                validator_findings,
                key,
                owned_cancellation.clone(),
            ),
        )
        .await
        {
            Ok(result) => result,
            Err(_) => {
                owned_cancellation.cancel();
                owned_runtime
                    .artifact_operations()
                    .settle_import_timeout(key)
                    .await;
                Err(ArtifactToolError::Indeterminate)
            }
        }
    });
    tokio::select! {
        () = cancellation.cancelled() => Err(ArtifactToolError::Indeterminate),
        result = receiver => result.unwrap_or(Err(ArtifactToolError::Indeterminate)),
    }
}

#[allow(clippy::too_many_arguments)]
async fn settle_reserved_import(
    runtime: RuntimeContext,
    mut source: PreparedImport,
    space_id: SpaceId,
    name: String,
    declared_media_type: Option<String>,
    content_sha256: String,
    source_length: u64,
    root_id: Option<String>,
    staging_record: Option<String>,
    validator_findings: Vec<ValidatorFinding>,
    key: [u8; 32],
    cancellation: CancellationToken,
) -> Result<FileImportOutput, ArtifactToolError> {
    let upload_file = match source.try_clone_reader() {
        Ok(file) => file,
        Err(error) => {
            runtime.artifact_operations().remove(key).await;
            return Err(error);
        }
    };
    let multipart_limit = match source_length.checked_add(MULTIPART_ALLOWANCE) {
        Some(limit) => limit,
        None => {
            runtime.artifact_operations().remove(key).await;
            return Err(ArtifactToolError::Validation);
        }
    };
    let mut request = runtime
        .client()
        .files()
        .upload(space_id.as_str())
        .reader(
            name,
            artifact_upload_reader(&runtime, upload_file, source_length, key),
            source_length,
        )
        .multipart_limit_bytes(multipart_limit)
        .response_limit_bytes(RESPONSE_BYTES)
        .error_limit_bytes(ERROR_BYTES);
    if let Some(media_type) = declared_media_type.as_ref() {
        request = request.mime(media_type);
    }
    if let Err(error) = source.mark_import_dispatched(&runtime).await {
        runtime.artifact_operations().remove(key).await;
        return Err(error);
    }
    let uploaded = match request.upload().await {
        Ok(uploaded) => uploaded,
        Err(error) if mutation_rejection_is_definitive(&error) => {
            if source
                .restore_after_definitive_rejection(&runtime)
                .await
                .is_err()
            {
                runtime
                    .artifact_operations()
                    .set_outcome(key, OperationOutcome::Indeterminate)
                    .await;
                return Err(ArtifactToolError::Indeterminate);
            }
            runtime.artifact_operations().remove(key).await;
            return Err(classify_anytype_error(&error));
        }
        Err(_) => {
            runtime
                .artifact_operations()
                .set_outcome(key, OperationOutcome::Indeterminate)
                .await;
            return Err(ArtifactToolError::Indeterminate);
        }
    };
    let candidate = match validated_uploaded_candidate(&uploaded, &space_id, source_length) {
        Ok(candidate) => candidate,
        Err(_) => {
            runtime
                .artifact_operations()
                .set_outcome(key, OperationOutcome::Indeterminate)
                .await;
            return Err(ArtifactToolError::Indeterminate);
        }
    };
    if source
        .retain_import_candidate(&runtime, &candidate)
        .await
        .is_err()
    {
        runtime
            .artifact_operations()
            .set_outcome(key, OperationOutcome::ImportIndeterminate(candidate))
            .await;
        return Err(ArtifactToolError::Indeterminate);
    }
    #[cfg(any(test, feature = "acceptance-harness"))]
    if !runtime
        .artifact_acceptance_gates()
        .reach(ArtifactAcceptanceGatePoint::ImportPostDispatch, key)
        .await
    {
        runtime
            .artifact_operations()
            .set_outcome(key, OperationOutcome::ImportIndeterminate(candidate))
            .await;
        return Err(ArtifactToolError::Indeterminate);
    }
    runtime
        .artifact_operations()
        .set_outcome(
            key,
            OperationOutcome::ImportVerifying {
                candidate: candidate.clone(),
                validator_findings: validator_findings.clone(),
            },
        )
        .await;
    if let Err(source_error) = source.verify_unchanged() {
        if source_error == ArtifactToolError::Conflict {
            runtime
                .artifact_operations()
                .set_outcome(key, OperationOutcome::ImportCleaning(candidate.clone()))
                .await;
            if source
                .retain_candidate_cleanup(&runtime, "delete_dispatched")
                .await
                .is_err()
            {
                drop(source);
                runtime
                    .artifact_operations()
                    .set_outcome(
                        key,
                        OperationOutcome::ImportIndeterminate(candidate.clone()),
                    )
                    .await;
                return Err(ArtifactToolError::Indeterminate);
            }
            if cleanup_changed_import_candidate(&runtime, &space_id, &candidate).await {
                if source
                    .restore_after_definitive_rejection(&runtime)
                    .await
                    .is_err()
                {
                    drop(source);
                    runtime
                        .artifact_operations()
                        .set_outcome(
                            key,
                            OperationOutcome::ImportIndeterminate(candidate.clone()),
                        )
                        .await;
                    return Err(ArtifactToolError::Indeterminate);
                }
                drop(source);
                runtime.artifact_operations().remove(key).await;
                return Err(source_error);
            }
            let _ = source
                .retain_candidate_cleanup(&runtime, "absence_ambiguous")
                .await;
        }
        drop(source);
        runtime
            .artifact_operations()
            .set_outcome(
                key,
                OperationOutcome::ImportIndeterminate(candidate.clone()),
            )
            .await;
        return Err(ArtifactToolError::Indeterminate);
    }
    let stored_media_type = match verify_import_candidate(
        &runtime,
        &space_id,
        &candidate,
        source_length,
        &content_sha256,
        &cancellation,
    )
    .await
    {
        Ok(stored_media_type) => stored_media_type,
        Err(_) => {
            drop(source);
            runtime
                .artifact_operations()
                .set_outcome(
                    key,
                    OperationOutcome::ImportCandidate {
                        candidate: candidate.clone(),
                        validator_findings,
                    },
                )
                .await;
            return Err(ArtifactToolError::Indeterminate);
        }
    };
    // Candidate verification is replay evidence. Publish it before consuming
    // staged authority so cancellation, timeout, or panic in the following
    // gap can resume verification without issuing a second upload.
    runtime
        .artifact_operations()
        .set_outcome(
            key,
            OperationOutcome::ImportCandidate {
                candidate: candidate.clone(),
                validator_findings: validator_findings.clone(),
            },
        )
        .await;
    if let PreparedImport::Staged(staged) = &mut source {
        let consumed = match staging(&runtime) {
            Ok(staging) => staging
                .consume(staged)
                .await
                .map_err(classify_staging_error),
            Err(error) => Err(error),
        };
        if consumed.is_err() {
            runtime
                .artifact_operations()
                .set_outcome(
                    key,
                    OperationOutcome::ImportIndeterminate(candidate.clone()),
                )
                .await;
            return Err(ArtifactToolError::Indeterminate);
        }
    }
    // Release the exclusive staged authority before publishing a replayable
    // terminal ledger state.  Replays authenticate only the retained
    // Reconciliation/Consumed identity, never a readable lease.
    drop(source);
    let output = import_output(
        &space_id,
        &candidate,
        source_length,
        content_sha256,
        declared_media_type,
        stored_media_type,
        root_id,
        staging_record,
        validator_findings,
        false,
    );
    runtime
        .artifact_operations()
        .set_outcome(key, OperationOutcome::ImportComplete(output.clone()))
        .await;
    Ok(output)
}

async fn prepare_import_source(
    runtime: &RuntimeContext,
    source: ResolvedSource,
    space_id: &SpaceId,
) -> Result<PreparedImport, ArtifactToolError> {
    match source {
        ResolvedSource::Local(location) => {
            let path = location.relative_path()?;
            let root_id = location.root;
            let roots = roots(runtime).await?;
            let maximum = runtime.artifact_config().limits.artifact_bytes;
            let chunk = runtime.artifact_config().limits.transfer_chunk_bytes;
            tokio::task::spawn_blocking(move || {
                let source = roots
                    .open_import(&root_id, &path, maximum)
                    .map_err(|error| classify_root_error(&error))?;
                let (source, sha256) = hash_import(source, chunk)?;
                Ok(PreparedImport::Local {
                    source,
                    sha256,
                    root_id,
                })
            })
            .await
            .map_err(|_| ArtifactToolError::Upstream)?
        }
        ResolvedSource::Staged(handle) => staging(runtime)?
            .import_source(&handle, space_id)
            .await
            .map(PreparedImport::Staged)
            .map_err(classify_staging_error),
    }
}

fn validated_uploaded_candidate(
    uploaded: &FileObject,
    space_id: &SpaceId,
    length: u64,
) -> Result<EntityId, ArtifactToolError> {
    let candidate =
        EntityId::new(uploaded.id.clone()).map_err(|_| ArtifactToolError::Indeterminate)?;
    let size = uploaded
        .size
        .and_then(|value| u64::try_from(value).ok())
        .ok_or(ArtifactToolError::Indeterminate)?;
    if uploaded.id != candidate.as_str() || uploaded.space_id != space_id.as_str() || size != length
    {
        return Err(ArtifactToolError::Indeterminate);
    }
    Ok(candidate)
}

async fn verify_import_candidate(
    runtime: &RuntimeContext,
    space_id: &SpaceId,
    candidate: &EntityId,
    expected_size: u64,
    expected_sha256: &str,
    cancellation: &CancellationToken,
) -> Result<Option<String>, ArtifactToolError> {
    let limits = &runtime.artifact_config().limits;
    let (_, stored_size, stored_hash, stored_media_type, _) = stream_anytype_file(
        FileStreamRequest {
            client: runtime.client(),
            space_id,
            file_id: candidate,
            maximum_bytes: limits.artifact_bytes,
            chunk_bytes: limits.transfer_chunk_bytes,
            expected_etag: None,
            expected_sha256: Some(expected_sha256),
            cancellation,
        },
        std::io::sink(),
    )
    .await
    .map_err(|_| ArtifactToolError::Indeterminate)?;
    if stored_size != expected_size {
        return Err(ArtifactToolError::Indeterminate);
    }
    if stored_hash != expected_sha256 {
        return Err(ArtifactToolError::Indeterminate);
    }
    Ok(stored_media_type)
}

/// Deletes a candidate only after a definitive local source conflict and
/// proves its absence through a separate metadata request.  Any ambiguity is
/// deliberately retained for idempotency reconciliation.
async fn cleanup_changed_import_candidate(
    runtime: &RuntimeContext,
    space_id: &SpaceId,
    candidate: &EntityId,
) -> bool {
    let deletion = runtime
        .client()
        .files()
        .delete_request(space_id.as_str(), candidate.as_str())
        .permanently()
        .delete();
    let _ = tokio::time::timeout(runtime.request_timeout(), deletion).await;
    let absence = runtime
        .client()
        .files()
        .download_request(space_id.as_str(), candidate.as_str())
        .response_limit_bytes(1)
        .error_limit_bytes(ERROR_BYTES)
        .header_evidence_limit_bytes(HEADER_BYTES)
        .max_attempts(1)
        .head();
    matches!(
        tokio::time::timeout(runtime.request_timeout(), absence).await,
        Ok(Err(AnytypeError::NotFound { .. }))
    )
}

#[allow(clippy::too_many_arguments)]
fn import_output(
    space_id: &SpaceId,
    candidate: &EntityId,
    length: u64,
    content_sha256: String,
    declared_media_type: Option<String>,
    stored_media_type: Option<String>,
    root_id: Option<String>,
    staging_record: Option<String>,
    validators: Vec<ValidatorFinding>,
    reused: bool,
) -> FileImportOutput {
    FileImportOutput {
        file_id: candidate.as_str().to_owned(),
        receipt: ArtifactReceipt {
            direction: ArtifactDirection::Import,
            state: ArtifactState::Consumed,
            space_id: space_id.as_str().to_owned(),
            size_bytes: length,
            sha256: content_sha256,
            declared_media_type,
            stored_media_type,
            root_id,
            staging_record,
            staging_handle: None,
            staging_url: None,
            validators,
        },
        reused,
    }
}

enum ExportCompletion {
    Local {
        root_id: String,
    },
    Remote {
        lease: Box<StageWriteLease>,
        allocation: StageAllocation,
    },
}

enum ExportDestination {
    Local(AtomicExport),
    Remote(StagingPayload),
}

impl Write for ExportDestination {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        match self {
            Self::Local(destination) => destination.write(bytes),
            Self::Remote(destination) => destination.write(bytes),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            Self::Local(destination) => destination.flush(),
            Self::Remote(destination) => destination.flush(),
        }
    }
}

async fn file_export(
    runtime: &RuntimeContext,
    input: FileExportInput,
    cancellation: &CancellationToken,
) -> Result<FileExportOutput, ArtifactToolError> {
    if runtime.is_read_only() {
        return Err(ArtifactToolError::ReadOnly);
    }
    let space_id = resolve_space(runtime.client(), &input.space).await?;
    let file_id = EntityId::new(input.file_id).map_err(|_| ArtifactToolError::Validation)?;
    let destination = input.destination.resolve()?;
    let limits = &runtime.artifact_config().limits;
    let key = idempotency_key(b"export", &input.idempotency_key);
    let fingerprint = export_fingerprint(
        &space_id,
        &file_id,
        &destination,
        input.expected_strong_etag.as_ref().map(String::as_str),
    );
    let (destination, reservation) =
        reserve_file_export_operation(runtime.artifact_operations(), key, fingerprint, destination)
            .await?;
    match reservation {
        ExportIdempotency::Reuse(output) => return Ok(*output),
        ExportIdempotency::Dispatch => {}
    }
    let (destination, completion, stream_etag) = match destination {
        ValidatedDestination::Local { root, path } => {
            let root_id = root;
            let roots = match roots(runtime).await {
                Ok(roots) => roots,
                Err(error) => {
                    runtime.artifact_operations().remove(key).await;
                    return Err(error);
                }
            };
            let maximum = limits.artifact_bytes;
            let destination = match tokio::task::spawn_blocking(move || {
                roots
                    .begin_atomic_export(&root_id, &path, maximum)
                    .map(|destination| (destination, root_id))
            })
            .await
            {
                Ok(result) => result.map_err(|error| classify_root_error(&error)),
                Err(_) => Err(ArtifactToolError::Upstream),
            };
            let (destination, root_id) = match destination {
                Ok(destination) => destination,
                Err(error) => {
                    runtime.artifact_operations().remove(key).await;
                    return Err(error);
                }
            };
            (
                ExportDestination::Local(destination),
                ExportCompletion::Local { root_id },
                input.expected_strong_etag.as_ref().cloned(),
            )
        }
        ValidatedDestination::Remote => {
            let preflight = match preflight_anytype_file(
                runtime.client(),
                &space_id,
                &file_id,
                limits.artifact_bytes,
                limits.transfer_chunk_bytes,
                input.expected_strong_etag.as_ref().map(String::as_str),
                None,
            )
            .await
            {
                Ok(preflight) => preflight,
                Err(error) => {
                    runtime.artifact_operations().remove(key).await;
                    return Err(error);
                }
            };
            let staging = match staging(runtime) {
                Ok(staging) => staging,
                Err(error) => {
                    runtime.artifact_operations().remove(key).await;
                    return Err(error);
                }
            };
            let allocation = match staging
                .allocate_export(
                    space_id.clone(),
                    preflight.total,
                    preflight.media_type.clone(),
                )
                .await
                .map_err(classify_staging_error)
            {
                Ok(allocation) => allocation,
                Err(error) => {
                    runtime.artifact_operations().remove(key).await;
                    return Err(error);
                }
            };
            let mut lease = match staging
                .begin_write(
                    &allocation.handle,
                    Some(&allocation.record),
                    StageDirection::Export,
                    0,
                )
                .await
                .map_err(classify_staging_error)
            {
                Ok(lease) => lease,
                Err(error) => {
                    let _ = staging.release(&allocation.handle).await;
                    runtime.artifact_operations().remove(key).await;
                    return Err(error);
                }
            };
            let destination = match lease.take_destination().map_err(classify_staging_error) {
                Ok(destination) => destination,
                Err(error) => {
                    let _ = staging.abort_write(lease, &allocation.handle).await;
                    runtime.artifact_operations().remove(key).await;
                    return Err(error);
                }
            };
            (
                ExportDestination::Remote(destination),
                ExportCompletion::Remote {
                    lease: Box::new(lease),
                    allocation,
                },
                preflight.etag,
            )
        }
    };
    let transfer = stream_anytype_file(
        FileStreamRequest {
            client: runtime.client(),
            space_id: &space_id,
            file_id: &file_id,
            maximum_bytes: limits.artifact_bytes,
            chunk_bytes: limits.transfer_chunk_bytes,
            expected_etag: stream_etag.as_deref(),
            expected_sha256: None,
            cancellation,
        },
        destination,
    )
    .await;
    let (destination, size, sha256, stored_media_type, _) = match transfer {
        Ok(transfer) => transfer,
        Err(error) => {
            if let ExportCompletion::Remote { lease, allocation } = completion
                && let Ok(staging) = staging(runtime)
            {
                let _ = staging.abort_write(*lease, &allocation.handle).await;
            }
            runtime.artifact_operations().remove(key).await;
            return Err(error);
        }
    };
    let receipt = match completion {
        ExportCompletion::Local { root_id } => {
            let ExportDestination::Local(destination) = destination else {
                return Err(ArtifactToolError::Indeterminate);
            };
            #[cfg(any(test, feature = "acceptance-harness"))]
            let destination =
                destination.with_acceptance_gate(runtime.artifact_acceptance_gates().clone(), key);
            // A vanished waiter still owns this commit; a proven success is
            // recorded as the terminal completed outcome rather than blanket
            // indeterminate. The replay output below is exactly what the
            // waiter would have recorded.
            let abandoned_operations = runtime.artifact_operations().clone();
            let abandoned_output = FileExportOutput {
                file_id: file_id.as_str().to_owned(),
                receipt: ArtifactReceipt {
                    direction: ArtifactDirection::Export,
                    state: ArtifactState::Available,
                    space_id: space_id.as_str().to_owned(),
                    size_bytes: size,
                    sha256: sha256.clone(),
                    declared_media_type: None,
                    stored_media_type: stored_media_type.clone(),
                    root_id: Some(root_id.clone()),
                    staging_record: None,
                    staging_handle: None,
                    staging_url: None,
                    validators: Vec::new(),
                },
            };
            let committed = match runtime
                .supervise_artifact_blocking(
                    move || {
                        destination.commit().map_err(|error| {
                            if error.kind() == RootAccessErrorKind::Indeterminate {
                                ArtifactToolError::Indeterminate
                            } else {
                                classify_root_error(&error)
                            }
                        })
                    },
                    move |result| match result {
                        Ok(committed) if committed == size => abandoned_operations.set_outcome_now(
                            key,
                            OperationOutcome::ExportComplete(abandoned_output),
                        ),
                        Ok(_) | Err(_) => abandoned_operations.mark_indeterminate(key),
                    },
                )
                .await
            {
                Ok(Ok(committed)) => committed,
                Ok(Err(ArtifactToolError::Indeterminate)) | Err(_) => {
                    runtime
                        .artifact_operations()
                        .set_outcome(key, OperationOutcome::Indeterminate)
                        .await;
                    return Err(ArtifactToolError::Indeterminate);
                }
                Ok(Err(error)) => {
                    runtime.artifact_operations().remove(key).await;
                    return Err(error);
                }
            };
            if committed != size {
                runtime
                    .artifact_operations()
                    .set_outcome(key, OperationOutcome::Indeterminate)
                    .await;
                return Err(ArtifactToolError::Indeterminate);
            }
            ArtifactReceipt {
                direction: ArtifactDirection::Export,
                state: ArtifactState::Available,
                space_id: space_id.as_str().to_owned(),
                size_bytes: size,
                sha256: sha256.clone(),
                declared_media_type: None,
                stored_media_type: stored_media_type.clone(),
                root_id: Some(root_id),
                staging_record: None,
                staging_handle: None,
                staging_url: None,
                validators: Vec::new(),
            }
        }
        ExportCompletion::Remote { lease, allocation } => {
            let ExportDestination::Remote(destination) = destination else {
                return Err(ArtifactToolError::Indeterminate);
            };
            let staging = match staging(runtime) {
                Ok(staging) => staging,
                Err(error) => {
                    return Err(
                        settle_export_failure(runtime.artifact_operations(), key, error).await,
                    );
                }
            };
            if let Err(error) = staging
                .finish_export(*lease, destination, size, sha256.clone())
                .await
                .map_err(classify_staging_error)
            {
                if should_release_failed_export_stage(error) {
                    let _ = staging.release(&allocation.handle).await;
                }
                return Err(settle_export_failure(runtime.artifact_operations(), key, error).await);
            }
            ArtifactReceipt {
                direction: ArtifactDirection::Export,
                state: ArtifactState::Available,
                space_id: space_id.as_str().to_owned(),
                size_bytes: size,
                sha256: sha256.clone(),
                declared_media_type: None,
                stored_media_type: stored_media_type.clone(),
                root_id: None,
                staging_record: Some(allocation.record),
                staging_handle: Some(allocation.handle),
                staging_url: Some(allocation.url),
                validators: Vec::new(),
            }
        }
    };
    let output = FileExportOutput {
        file_id: file_id.as_str().to_owned(),
        receipt,
    };
    runtime
        .artifact_operations()
        .set_outcome(key, OperationOutcome::ExportComplete(output.clone()))
        .await;
    Ok(output)
}

struct PreparedDocument {
    source: PreparedImport,
    source_sha256: String,
    dispatched: String,
    dispatched_sha256: String,
    source_bytes: u64,
    dispatched_chars: usize,
}

impl PreparedDocument {
    fn verify_before_dispatch(&self) -> Result<(), ArtifactToolError> {
        self.source.verify_before_dispatch()
    }

    async fn consume_staged(
        &mut self,
        runtime: &RuntimeContext,
    ) -> Result<bool, ArtifactToolError> {
        let PreparedImport::Staged(source) = &mut self.source else {
            return Ok(false);
        };
        staging(runtime)?
            .consume(source)
            .await
            .map_err(classify_staging_error)?;
        Ok(true)
    }
}

async fn verify_document_source_before_dispatch(
    #[cfg(any(test, feature = "acceptance-harness"))] runtime: &RuntimeContext,
    #[cfg(not(any(test, feature = "acceptance-harness")))] _runtime: &RuntimeContext,
    operations: &ArtifactOperationState,
    key: [u8; 32],
    #[cfg(any(test, feature = "acceptance-harness"))] acceptance_key: [u8; 32],
    #[cfg(not(any(test, feature = "acceptance-harness")))] _acceptance_key: [u8; 32],
    source: &PreparedDocument,
) -> Result<(), ArtifactToolError> {
    #[cfg(any(test, feature = "acceptance-harness"))]
    let released = runtime
        .artifact_acceptance_gates()
        .reach(
            ArtifactAcceptanceGatePoint::DocumentFinalRevalidation,
            acceptance_key,
        )
        .await;
    #[cfg(any(test, feature = "acceptance-harness"))]
    if !released {
        return Err(ArtifactToolError::Indeterminate);
    }
    settle_document_source_revalidation(operations, key, source.verify_before_dispatch()).await
}

async fn settle_document_source_revalidation(
    operations: &ArtifactOperationState,
    key: [u8; 32],
    result: Result<(), ArtifactToolError>,
) -> Result<(), ArtifactToolError> {
    if let Err(error) = result {
        operations.remove(key).await;
        return Err(error);
    }
    Ok(())
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

fn digest_bytes(bytes: &[u8]) -> String {
    sha256_hex(Sha256::new_with_prefix(bytes))
}

fn plain_text_markdown(value: &str) -> String {
    let mut markdown = String::with_capacity(value.len());
    for character in value.chars() {
        if matches!(
            character,
            '\\' | '`'
                | '*'
                | '_'
                | '{'
                | '}'
                | '['
                | ']'
                | '<'
                | '>'
                | '('
                | ')'
                | '#'
                | '+'
                | '-'
                | '.'
                | '!'
                | '|'
        ) {
            markdown.push('\\');
        }
        markdown.push(character);
    }
    markdown
}

fn validate_document_text(bytes: Vec<u8>) -> Result<String, ArtifactToolError> {
    if bytes.starts_with(&[0xef, 0xbb, 0xbf])
        || bytes.starts_with(&[0xff, 0xfe])
        || bytes.starts_with(&[0xfe, 0xff])
        || bytes.starts_with(&[0, 0, 0xfe, 0xff])
        || bytes.starts_with(&[0xff, 0xfe, 0, 0])
    {
        return Err(ArtifactToolError::Validation);
    }
    let text = String::from_utf8(bytes).map_err(|_| ArtifactToolError::Validation)?;
    let mut previous_cr = false;
    for character in text.chars() {
        if previous_cr && character != '\n' {
            return Err(ArtifactToolError::Validation);
        }
        previous_cr = character == '\r';
        if character == '\0' || (character.is_control() && !matches!(character, '\t' | '\n' | '\r'))
        {
            return Err(ArtifactToolError::Validation);
        }
    }
    if previous_cr {
        return Err(ArtifactToolError::Validation);
    }
    Ok(text)
}

async fn prepare_document(
    runtime: &RuntimeContext,
    source: ArtifactSource,
    source_format: DocumentSourceFormat,
    space_id: &SpaceId,
) -> Result<PreparedDocument, ArtifactToolError> {
    let source = prepare_import_source(runtime, source.resolve()?, space_id).await?;
    let limits = &runtime.artifact_config().limits;
    if source.length() > limits.markdown_bytes {
        return Err(ArtifactToolError::Bounded);
    }
    let source_bytes = source.length();
    let source_sha256 = source.sha256().to_owned();
    let maximum = limits.markdown_bytes;
    let reader = source.try_clone_reader()?;
    let capacity = usize::try_from(source_bytes).map_err(|_| ArtifactToolError::Bounded)?;
    let mut bytes = Vec::with_capacity(capacity);
    PositionalReader::new(reader, source_bytes)
        .read_to_end(&mut bytes)
        .await
        .map_err(|_| ArtifactToolError::NotFound)?;
    if bytes.len() as u64 != source_bytes || bytes.len() as u64 > maximum {
        return Err(ArtifactToolError::Conflict);
    }
    source.verify_unchanged()?;
    let text = validate_document_text(bytes)?;
    let dispatched = match source_format {
        DocumentSourceFormat::Markdown => text,
        DocumentSourceFormat::PlainText => plain_text_markdown(&text),
    };
    let dispatched_chars = dispatched.chars().count();
    if dispatched.len() as u64 > limits.markdown_bytes || dispatched_chars > limits.markdown_chars {
        return Err(ArtifactToolError::Bounded);
    }
    let dispatched_sha256 = digest_bytes(dispatched.as_bytes());
    Ok(PreparedDocument {
        source,
        source_sha256,
        dispatched,
        dispatched_sha256,
        source_bytes,
        dispatched_chars,
    })
}

fn checked_document<'a>(
    object: &'a Object,
    space_id: &SpaceId,
    object_id: &EntityId,
    limits: &crate::artifact_config::ArtifactLimits,
) -> Result<&'a str, ArtifactToolError> {
    if object.id != object_id.as_str() || object.space_id != space_id.as_str() || object.archived {
        return Err(ArtifactToolError::Upstream);
    }
    let body = object.markdown.as_deref().unwrap_or("");
    if body.len() as u64 > limits.markdown_bytes || body.chars().count() > limits.markdown_chars {
        return Err(ArtifactToolError::Bounded);
    }
    Ok(body)
}

async fn verify_document_candidate(
    runtime: &RuntimeContext,
    space_id: &SpaceId,
    object_id: &EntityId,
    expected_sha256: &str,
    expected_name: Option<&str>,
    expected_type_key: Option<&str>,
    expected_properties: &[MutationProperty],
) -> Result<String, ArtifactToolError> {
    let object = runtime
        .client()
        .object(space_id.as_str(), object_id.as_str())
        .get()
        .await
        .map_err(|error| classify_anytype_error(&error))?;
    let body = checked_document(
        &object,
        space_id,
        object_id,
        &runtime.artifact_config().limits,
    )?;
    let hash = digest_bytes(body.as_bytes());
    if hash != expected_sha256
        || expected_name.is_some_and(|name| object.name.as_deref() != Some(name))
        || expected_type_key.is_some_and(|key| {
            object
                .r#type
                .as_ref()
                .is_none_or(|typ| typ.archived || typ.key != key)
        })
        || !document_properties_match(&object, expected_properties)?
    {
        return Err(ArtifactToolError::Indeterminate);
    }
    Ok(hash)
}

fn validate_document_properties(
    typ: &anytype::types::Type,
    requested: &[MutationProperty],
) -> Result<(), ArtifactToolError> {
    for property in requested {
        let mut schemas = typ
            .properties
            .iter()
            .filter(|schema| schema.key == property.key().as_str());
        let Some(schema) = schemas.next() else {
            return Err(ArtifactToolError::Validation);
        };
        if schemas.next().is_some() {
            return Err(ArtifactToolError::Upstream);
        }
        if schema.format() != property.format() {
            return Err(ArtifactToolError::Validation);
        }
    }
    Ok(())
}

fn document_properties_match(
    object: &Object,
    expected: &[MutationProperty],
) -> Result<bool, ArtifactToolError> {
    for property in expected {
        let mut returned = object
            .properties
            .iter()
            .filter(|candidate| candidate.key == property.key().as_str());
        let actual = returned.next();
        if returned.next().is_some() {
            return Err(ArtifactToolError::Indeterminate);
        }
        if !property
            .matches_returned(actual.map(|candidate| &candidate.value))
            .map_err(|_| ArtifactToolError::Indeterminate)?
        {
            return Ok(false);
        }
    }
    Ok(true)
}

fn document_mutation_fingerprint(
    domain: &[u8],
    space_id: &SpaceId,
    object_or_type: &str,
    source: &PreparedDocument,
    precondition: &str,
    name: &str,
    properties: &[u8],
) -> [u8; 32] {
    digest_fields(
        domain,
        &[
            space_id.as_str().as_bytes(),
            object_or_type.as_bytes(),
            source.source_sha256.as_bytes(),
            source.dispatched_sha256.as_bytes(),
            precondition.as_bytes(),
            name.as_bytes(),
            properties,
        ],
    )
}

#[derive(Clone, Copy)]
struct DocumentMutationDisposition {
    no_op: bool,
    source_consumed: bool,
    reused: bool,
}

fn document_output(
    space_id: &SpaceId,
    object_id: &EntityId,
    source: &PreparedDocument,
    canonical_sha256: String,
    disposition: DocumentMutationDisposition,
    validators: Vec<ValidatorFinding>,
) -> DocumentMutationOutput {
    DocumentMutationOutput {
        space_id: space_id.as_str().to_owned(),
        object_id: object_id.as_str().to_owned(),
        source_sha256: source.source_sha256.clone(),
        dispatched_sha256: source.dispatched_sha256.clone(),
        canonical_sha256,
        source_bytes: source.source_bytes,
        dispatched_chars: source.dispatched_chars,
        no_op: disposition.no_op,
        source_consumed: disposition.source_consumed,
        reused: disposition.reused,
        validators,
    }
}

async fn document_import_create(
    runtime: &RuntimeContext,
    input: DocumentImportCreateInput,
) -> Result<DocumentMutationOutput, ArtifactToolError> {
    if runtime.is_read_only() {
        return Err(ArtifactToolError::ReadOnly);
    }
    if input.name.is_empty()
        || input.name.chars().count() > 512
        || input.name.chars().any(char::is_control)
    {
        return Err(ArtifactToolError::Validation);
    }
    let space_id = resolve_space(runtime.client(), &input.space).await?;
    let typ = runtime
        .client()
        .resolve_type(space_id.as_str(), &input.object_type)
        .await
        .map_err(|error| classify_anytype_error(&error))?;
    if typ.archived {
        return Err(ArtifactToolError::Validation);
    }
    let properties = input
        .properties
        .as_ref()
        .map(normalized_properties)
        .transpose()
        .map_err(|_| ArtifactToolError::Validation)?
        .unwrap_or_default();
    validate_document_properties(&typ, &properties)?;
    let type_key = typ.key;
    EntityId::new(typ.id).map_err(|_| ArtifactToolError::Upstream)?;
    if type_key.is_empty() || type_key.len() > 255 {
        return Err(ArtifactToolError::Upstream);
    }
    let mut source =
        prepare_document(runtime, input.source, input.source_format, &space_id).await?;
    let validator_findings =
        run_configured_validators(runtime, &source.source, Some("text/markdown")).await?;
    let key = idempotency_key(b"document-create", &input.idempotency_key);
    let acceptance_key = idempotency_key(b"document", &input.idempotency_key);
    let encoded_properties =
        serde_json::to_vec(&properties).map_err(|_| ArtifactToolError::Upstream)?;
    let fingerprint = document_mutation_fingerprint(
        b"any-mcp/artifact/document-create/v1",
        &space_id,
        &type_key,
        &source,
        "",
        &input.name,
        &encoded_properties,
    );
    match runtime
        .artifact_operations()
        .reserve_document_mutation(key, fingerprint)
        .await?
    {
        DocumentMutationIdempotency::Reuse(mut output) => {
            output.reused = true;
            return Ok(output);
        }
        DocumentMutationIdempotency::VerifyCandidate {
            object_id,
            canonical_sha256,
        } => {
            verify_document_candidate(
                runtime,
                &space_id,
                &object_id,
                &canonical_sha256,
                Some(&input.name),
                Some(&type_key),
                &properties,
            )
            .await?;
            let consumed = source.consume_staged(runtime).await?;
            let output = document_output(
                &space_id,
                &object_id,
                &source,
                canonical_sha256,
                DocumentMutationDisposition {
                    no_op: false,
                    source_consumed: consumed,
                    reused: true,
                },
                validator_findings,
            );
            runtime
                .artifact_operations()
                .set_outcome(
                    key,
                    OperationOutcome::DocumentMutationComplete(output.clone()),
                )
                .await;
            return Ok(output);
        }
        DocumentMutationIdempotency::Dispatch => {}
    }

    let representation = plain_markdown_representation(&source.dispatched);
    let expected_canonical = representation
        .as_ref()
        .map(|value| value.canonical().to_owned());
    let wire = representation
        .as_ref()
        .map_or(source.dispatched.as_str(), |value| value.wire());
    let mut request = runtime
        .client()
        .new_object(space_id.as_str(), &type_key)
        .name(&input.name)
        .body(wire)
        .no_verify();
    for property in &properties {
        request = property.apply(request);
    }
    verify_document_source_before_dispatch(
        runtime,
        runtime.artifact_operations(),
        key,
        acceptance_key,
        &source,
    )
    .await?;
    let created = match request.create().await {
        Ok(created) => created,
        Err(error) if mutation_rejection_is_definitive(&error) => {
            runtime.artifact_operations().remove(key).await;
            return Err(classify_anytype_error(&error));
        }
        Err(_) => {
            runtime
                .artifact_operations()
                .set_outcome(key, OperationOutcome::Indeterminate)
                .await;
            return Err(ArtifactToolError::Indeterminate);
        }
    };
    let object_id =
        EntityId::new(created.id.clone()).map_err(|_| ArtifactToolError::Indeterminate)?;
    let returned = checked_document(
        &created,
        &space_id,
        &object_id,
        &runtime.artifact_config().limits,
    )
    .map_err(|_| ArtifactToolError::Indeterminate)?;
    if expected_canonical
        .as_deref()
        .is_some_and(|expected| returned != expected)
        || (expected_canonical.is_none() && !source.dispatched.is_empty() && returned.is_empty())
    {
        return Err(ArtifactToolError::Indeterminate);
    }
    let canonical_sha256 = digest_bytes(returned.as_bytes());
    runtime
        .artifact_operations()
        .set_outcome(
            key,
            OperationOutcome::DocumentMutationCandidate {
                object_id: object_id.clone(),
                canonical_sha256: canonical_sha256.clone(),
            },
        )
        .await;
    verify_document_candidate(
        runtime,
        &space_id,
        &object_id,
        &canonical_sha256,
        Some(&input.name),
        Some(&type_key),
        &properties,
    )
    .await?;
    let consumed = source.consume_staged(runtime).await?;
    let output = document_output(
        &space_id,
        &object_id,
        &source,
        canonical_sha256,
        DocumentMutationDisposition {
            no_op: false,
            source_consumed: consumed,
            reused: false,
        },
        validator_findings,
    );
    runtime
        .artifact_operations()
        .set_outcome(
            key,
            OperationOutcome::DocumentMutationComplete(output.clone()),
        )
        .await;
    Ok(output)
}

async fn document_import_update(
    runtime: &RuntimeContext,
    input: DocumentImportUpdateInput,
) -> Result<DocumentMutationOutput, ArtifactToolError> {
    if runtime.is_read_only() {
        return Err(ArtifactToolError::ReadOnly);
    }
    if !valid_sha256(&input.expected_body_sha256) {
        return Err(ArtifactToolError::Validation);
    }
    let space_id = resolve_space(runtime.client(), &input.space).await?;
    let object_id = EntityId::new(input.object_id).map_err(|_| ArtifactToolError::Validation)?;
    let current = runtime
        .client()
        .object(space_id.as_str(), object_id.as_str())
        .get()
        .await
        .map_err(|error| classify_anytype_error(&error))?;
    let current_body = checked_document(
        &current,
        &space_id,
        &object_id,
        &runtime.artifact_config().limits,
    )?;
    let current_hash = digest_bytes(current_body.as_bytes());
    if current_hash != input.expected_body_sha256 {
        return Err(ArtifactToolError::Conflict);
    }
    let mut source =
        prepare_document(runtime, input.source, input.source_format, &space_id).await?;
    let validator_findings =
        run_configured_validators(runtime, &source.source, Some("text/markdown")).await?;
    let key = idempotency_key(b"document-update", &input.idempotency_key);
    let acceptance_key = idempotency_key(b"document", &input.idempotency_key);
    let fingerprint = document_mutation_fingerprint(
        b"any-mcp/artifact/document-update/v1",
        &space_id,
        object_id.as_str(),
        &source,
        &input.expected_body_sha256,
        "",
        &[],
    );
    match runtime
        .artifact_operations()
        .reserve_document_mutation(key, fingerprint)
        .await?
    {
        DocumentMutationIdempotency::Reuse(mut output) => {
            output.reused = true;
            return Ok(output);
        }
        DocumentMutationIdempotency::VerifyCandidate {
            object_id: candidate,
            canonical_sha256,
        } => {
            if candidate != object_id {
                return Err(ArtifactToolError::Indeterminate);
            }
            verify_document_candidate(
                runtime,
                &space_id,
                &object_id,
                &canonical_sha256,
                None,
                None,
                &[],
            )
            .await?;
            let consumed = source.consume_staged(runtime).await?;
            let output = document_output(
                &space_id,
                &object_id,
                &source,
                canonical_sha256,
                DocumentMutationDisposition {
                    no_op: false,
                    source_consumed: consumed,
                    reused: true,
                },
                validator_findings,
            );
            runtime
                .artifact_operations()
                .set_outcome(
                    key,
                    OperationOutcome::DocumentMutationComplete(output.clone()),
                )
                .await;
            return Ok(output);
        }
        DocumentMutationIdempotency::Dispatch => {}
    }
    if source.dispatched == current_body {
        verify_document_source_before_dispatch(
            runtime,
            runtime.artifact_operations(),
            key,
            acceptance_key,
            &source,
        )
        .await?;
        let consumed = source.consume_staged(runtime).await?;
        let output = document_output(
            &space_id,
            &object_id,
            &source,
            current_hash,
            DocumentMutationDisposition {
                no_op: true,
                source_consumed: consumed,
                reused: false,
            },
            validator_findings,
        );
        runtime
            .artifact_operations()
            .set_outcome(
                key,
                OperationOutcome::DocumentMutationComplete(output.clone()),
            )
            .await;
        return Ok(output);
    }
    let representation = plain_markdown_representation(&source.dispatched);
    let expected_canonical = representation
        .as_ref()
        .map(|value| value.canonical().to_owned());
    let wire = representation
        .as_ref()
        .map_or(source.dispatched.as_str(), |value| value.wire());
    verify_document_source_before_dispatch(
        runtime,
        runtime.artifact_operations(),
        key,
        acceptance_key,
        &source,
    )
    .await?;
    let updated = match runtime
        .client()
        .update_object(space_id.as_str(), object_id.as_str())
        .body(wire)
        .no_verify()
        .update()
        .await
    {
        Ok(updated) => updated,
        Err(error) if mutation_rejection_is_definitive(&error) => {
            runtime.artifact_operations().remove(key).await;
            return Err(classify_anytype_error(&error));
        }
        Err(_) => {
            runtime
                .artifact_operations()
                .set_outcome(key, OperationOutcome::Indeterminate)
                .await;
            return Err(ArtifactToolError::Indeterminate);
        }
    };
    #[cfg(any(test, feature = "acceptance-harness"))]
    if !runtime
        .artifact_acceptance_gates()
        .reach(
            ArtifactAcceptanceGatePoint::DocumentPostDispatch,
            acceptance_key,
        )
        .await
    {
        runtime
            .artifact_operations()
            .set_outcome(key, OperationOutcome::Indeterminate)
            .await;
        return Err(ArtifactToolError::Indeterminate);
    }
    let returned = checked_document(
        &updated,
        &space_id,
        &object_id,
        &runtime.artifact_config().limits,
    )
    .map_err(|_| ArtifactToolError::Indeterminate)?;
    if expected_canonical
        .as_deref()
        .is_some_and(|expected| returned != expected)
        || (expected_canonical.is_none() && returned == current_body)
    {
        return Err(ArtifactToolError::Indeterminate);
    }
    let canonical_sha256 = digest_bytes(returned.as_bytes());
    runtime
        .artifact_operations()
        .set_outcome(
            key,
            OperationOutcome::DocumentMutationCandidate {
                object_id: object_id.clone(),
                canonical_sha256: canonical_sha256.clone(),
            },
        )
        .await;
    verify_document_candidate(
        runtime,
        &space_id,
        &object_id,
        &canonical_sha256,
        None,
        None,
        &[],
    )
    .await?;
    let consumed = source.consume_staged(runtime).await?;
    let output = document_output(
        &space_id,
        &object_id,
        &source,
        canonical_sha256,
        DocumentMutationDisposition {
            no_op: false,
            source_consumed: consumed,
            reused: false,
        },
        validator_findings,
    );
    runtime
        .artifact_operations()
        .set_outcome(
            key,
            OperationOutcome::DocumentMutationComplete(output.clone()),
        )
        .await;
    Ok(output)
}

async fn document_export(
    runtime: &RuntimeContext,
    input: DocumentExportInput,
) -> Result<DocumentExportOutput, ArtifactToolError> {
    if runtime.is_read_only() {
        return Err(ArtifactToolError::ReadOnly);
    }
    if input
        .expected_body_sha256
        .as_ref()
        .map(String::as_str)
        .is_some_and(|value| !valid_sha256(value))
    {
        return Err(ArtifactToolError::Validation);
    }
    let space_id = resolve_space(runtime.client(), &input.space).await?;
    let object_id = EntityId::new(input.object_id).map_err(|_| ArtifactToolError::Validation)?;
    let object = runtime
        .client()
        .object(space_id.as_str(), object_id.as_str())
        .get()
        .await
        .map_err(|error| classify_anytype_error(&error))?;
    let body = checked_document(
        &object,
        &space_id,
        &object_id,
        &runtime.artifact_config().limits,
    )?
    .to_owned();
    let sha256 = digest_bytes(body.as_bytes());
    if input
        .expected_body_sha256
        .as_ref()
        .map(String::as_str)
        .is_some_and(|expected| expected != sha256)
    {
        return Err(ArtifactToolError::Conflict);
    }
    let chars = body.chars().count();
    let size = body.len() as u64;
    let destination = input.destination.resolve()?;
    let destination_hash = match &destination {
        ResolvedDestination::Local(location) => location_fingerprint(location),
        ResolvedDestination::Remote => digest_fields(b"any-mcp/artifact/location/remote/v1", &[]),
    };
    let fingerprint = digest_fields(
        b"any-mcp/artifact/document-export/v1",
        &[
            space_id.as_str().as_bytes(),
            object_id.as_str().as_bytes(),
            sha256.as_bytes(),
            &destination_hash,
        ],
    );
    let key = idempotency_key(b"document-export", &input.idempotency_key);
    let (destination, reservation) = reserve_document_export_operation(
        runtime.artifact_operations(),
        key,
        fingerprint,
        destination,
    )
    .await?;
    match reservation {
        DocumentExportIdempotency::Reuse(mut output) => {
            output.reused = true;
            return Ok(*output);
        }
        DocumentExportIdempotency::Dispatch => {}
    }

    let receipt = match destination {
        ValidatedDestination::Local { root, path } => {
            let root_id = root;
            let roots = match roots(runtime).await {
                Ok(roots) => roots,
                Err(error) => {
                    runtime.artifact_operations().remove(key).await;
                    return Err(error);
                }
            };
            let maximum = runtime.artifact_config().limits.markdown_bytes;
            let bytes = body.into_bytes();
            #[cfg(any(test, feature = "acceptance-harness"))]
            let gates = runtime.artifact_acceptance_gates().clone();
            // Mirror of the waiter's terminal recording for a commit whose
            // waiter vanished: a proven full-length publication replays as
            // completed, anything else as indeterminate.
            let abandoned_operations = runtime.artifact_operations().clone();
            let abandoned_space = space_id.as_str().to_owned();
            let abandoned_object = object_id.as_str().to_owned();
            let abandoned_sha256 = sha256.clone();
            let written = match runtime
                .supervise_artifact_blocking(
                    move || {
                        let mut destination = roots
                            .begin_atomic_export(&root_id, &path, maximum)
                            .map_err(|error| classify_root_error(&error))?;
                        destination
                            .write_all(&bytes)
                            .map_err(|_| ArtifactToolError::NotFound)?;
                        #[cfg(any(test, feature = "acceptance-harness"))]
                        let destination = destination.with_acceptance_gate(gates, key);
                        let committed = destination
                            .commit()
                            .map_err(|error| classify_root_error(&error))?;
                        Ok::<_, ArtifactToolError>((committed, root_id))
                    },
                    move |result| match result {
                        Ok((committed, root_id)) if committed == size => {
                            abandoned_operations.set_outcome_now(
                                key,
                                OperationOutcome::DocumentExportComplete(DocumentExportOutput {
                                    space_id: abandoned_space.clone(),
                                    object_id: abandoned_object,
                                    size_bytes: size,
                                    chars,
                                    sha256: abandoned_sha256.clone(),
                                    receipt: ArtifactReceipt {
                                        direction: ArtifactDirection::Export,
                                        state: ArtifactState::Available,
                                        space_id: abandoned_space,
                                        size_bytes: size,
                                        sha256: abandoned_sha256,
                                        declared_media_type: None,
                                        stored_media_type: Some("text/markdown".to_owned()),
                                        root_id: Some(root_id),
                                        staging_record: None,
                                        staging_handle: None,
                                        staging_url: None,
                                        validators: Vec::new(),
                                    },
                                    reused: false,
                                }),
                            );
                        }
                        Ok(_) | Err(_) => abandoned_operations.mark_indeterminate(key),
                    },
                )
                .await
            {
                Ok(Ok(written)) => written,
                Ok(Err(error)) => {
                    return Err(
                        settle_export_failure(runtime.artifact_operations(), key, error).await,
                    );
                }
                Err(_) => {
                    return Err(settle_export_failure(
                        runtime.artifact_operations(),
                        key,
                        ArtifactToolError::Indeterminate,
                    )
                    .await);
                }
            };
            if written.0 != size {
                runtime
                    .artifact_operations()
                    .set_outcome(key, OperationOutcome::Indeterminate)
                    .await;
                return Err(ArtifactToolError::Indeterminate);
            }
            ArtifactReceipt {
                direction: ArtifactDirection::Export,
                state: ArtifactState::Available,
                space_id: space_id.as_str().to_owned(),
                size_bytes: size,
                sha256: sha256.clone(),
                declared_media_type: None,
                stored_media_type: Some("text/markdown".to_owned()),
                root_id: Some(written.1),
                staging_record: None,
                staging_handle: None,
                staging_url: None,
                validators: Vec::new(),
            }
        }
        ValidatedDestination::Remote => {
            let staging = match staging(runtime) {
                Ok(staging) => staging,
                Err(error) => {
                    runtime.artifact_operations().remove(key).await;
                    return Err(error);
                }
            };
            let allocation = match staging
                .allocate_export(space_id.clone(), size, Some("text/markdown".to_owned()))
                .await
                .map_err(classify_staging_error)
            {
                Ok(allocation) => allocation,
                Err(error) => {
                    runtime.artifact_operations().remove(key).await;
                    return Err(error);
                }
            };
            let mut lease = match staging
                .begin_write(
                    &allocation.handle,
                    Some(&allocation.record),
                    StageDirection::Export,
                    0,
                )
                .await
                .map_err(classify_staging_error)
            {
                Ok(lease) => lease,
                Err(error) => {
                    let _ = staging.release(&allocation.handle).await;
                    runtime.artifact_operations().remove(key).await;
                    return Err(error);
                }
            };
            let mut destination = match lease.take_destination().map_err(classify_staging_error) {
                Ok(destination) => destination,
                Err(error) => {
                    let _ = staging.abort_write(lease, &allocation.handle).await;
                    runtime.artifact_operations().remove(key).await;
                    return Err(error);
                }
            };
            let bytes = body.into_bytes();
            destination = match tokio::task::spawn_blocking(move || {
                destination
                    .write_all(&bytes)
                    .map_err(|_| ArtifactToolError::Upstream)?;
                Ok::<_, ArtifactToolError>(destination)
            })
            .await
            {
                Ok(Ok(destination)) => destination,
                Ok(Err(error)) => {
                    let _ = staging.abort_write(lease, &allocation.handle).await;
                    runtime.artifact_operations().remove(key).await;
                    return Err(error);
                }
                Err(_) => {
                    let _ = staging.abort_write(lease, &allocation.handle).await;
                    runtime.artifact_operations().remove(key).await;
                    return Err(ArtifactToolError::Upstream);
                }
            };
            if let Err(error) = staging
                .finish_export(lease, destination, size, sha256.clone())
                .await
                .map_err(classify_staging_error)
            {
                if should_release_failed_export_stage(error) {
                    let _ = staging.release(&allocation.handle).await;
                }
                return Err(settle_export_failure(runtime.artifact_operations(), key, error).await);
            }
            ArtifactReceipt {
                direction: ArtifactDirection::Export,
                state: ArtifactState::Available,
                space_id: space_id.as_str().to_owned(),
                size_bytes: size,
                sha256: sha256.clone(),
                declared_media_type: None,
                stored_media_type: Some("text/markdown".to_owned()),
                root_id: None,
                staging_record: Some(allocation.record),
                staging_handle: Some(allocation.handle),
                staging_url: Some(allocation.url),
                validators: Vec::new(),
            }
        }
    };
    let output = DocumentExportOutput {
        space_id: space_id.as_str().to_owned(),
        object_id: object_id.as_str().to_owned(),
        size_bytes: size,
        chars,
        sha256,
        receipt,
        reused: false,
    };
    runtime
        .artifact_operations()
        .set_outcome(
            key,
            OperationOutcome::DocumentExportComplete(output.clone()),
        )
        .await;
    Ok(output)
}

async fn stage_allocate(
    runtime: &RuntimeContext,
    input: StageAllocateInput,
) -> Result<StageAllocationOutput, ArtifactToolError> {
    if runtime.is_read_only() {
        return Err(ArtifactToolError::ReadOnly);
    }
    let media_type = normalize_media_type(input.media_type.as_ref().map(String::as_str))?;
    let space_id = resolve_space(runtime.client(), &input.space).await?;
    let allocation = staging(runtime)?
        .allocate_import(
            space_id,
            input.size_bytes,
            media_type,
            input.expected_sha256.as_ref().cloned(),
        )
        .await
        .map_err(classify_staging_error)?;
    Ok(StageAllocationOutput {
        record: allocation.record,
        handle: allocation.handle,
        upload_url: allocation.url,
        expires_at: allocation.expires_at.to_rfc3339(),
        size_bytes: allocation.size_bytes,
        offset: 0,
    })
}

async fn stage_release(
    runtime: &RuntimeContext,
    input: StageHandleInput,
) -> Result<StageReleaseOutput, ArtifactToolError> {
    if runtime.is_read_only() {
        return Err(ArtifactToolError::ReadOnly);
    }
    staging(runtime)?
        .release(&input.handle)
        .await
        .map_err(classify_staging_error)?;
    Ok(StageReleaseOutput { released: true })
}

async fn artifact_status(runtime: &RuntimeContext) -> ArtifactStatusOutput {
    let (staging_available_bytes, staging_available_entries) = match runtime.artifact_staging() {
        Some(staging) => staging.available_quota().await,
        None => (0, 0),
    };
    ArtifactStatusOutput {
        local_roots_active: runtime.artifact_roots().is_some(),
        import_root_count: runtime
            .artifact_roots()
            .map_or(0, |roots| bounded_root_count(roots.import_root_count())),
        export_root_count: runtime
            .artifact_roots()
            .map_or(0, |roots| bounded_root_count(roots.export_root_count())),
        staging_configured: runtime
            .artifact_config()
            .staging()
            .is_some_and(|config| config.enabled),
        staging_active: runtime
            .artifact_staging()
            .is_some_and(ArtifactStaging::is_active),
        staging_available_bytes,
        staging_available_entries,
        validator_count: runtime
            .artifact_validators()
            .map_or(0, |runner| bounded_root_count(runner.configured_count())),
        validator_available_count: runtime
            .artifact_validators()
            .map_or(0, |runner| bounded_root_count(runner.available_count())),
    }
}

fn bounded_root_count(count: usize) -> u32 {
    match u32::try_from(count) {
        Ok(count) => count.min(64),
        Err(_) => 64,
    }
}

#[derive(Debug)]
pub(crate) struct ArtifactRegistry;

pub(crate) static ARTIFACT_REGISTRY: ArtifactRegistry = ArtifactRegistry;

impl OptionalToolsetRegistry for ArtifactRegistry {
    fn metadata(&self) -> OptionalToolsetMetadata {
        OptionalToolsetMetadata::new(ARTIFACTS_TOOLSET_NAME, false)
    }

    fn tools(&self) -> Result<Vec<OptionalRegistryTool>, SchemaContractError> {
        Ok(vec![
            OptionalRegistryTool::read(status_tool()?),
            OptionalRegistryTool::mutation(stage_allocate_tool()?),
            OptionalRegistryTool::mutation(stage_release_tool()?),
            OptionalRegistryTool::mutation(document_export_tool()?),
            OptionalRegistryTool::mutation(document_import_create_tool()?),
            OptionalRegistryTool::mutation(document_import_update_tool()?),
            OptionalRegistryTool::mutation(export_tool()?),
            OptionalRegistryTool::mutation(import_tool()?),
        ])
    }

    fn scripted_scenario_ids(&self) -> &'static [&'static str] {
        &["artifact_local_direct", "artifact_local_stdio"]
    }

    fn headless_scenario_ids(&self) -> &'static [&'static str] {
        &[
            "artifact_local_real_headless",
            "artifact_remote_staging_real_headless",
            "artifact_direct_real_headless",
        ]
    }

    fn catalog_token_ceiling(&self) -> usize {
        12_000
    }

    fn call_tool<'a>(
        &'a self,
        request: CallToolRequestParams,
        runtime: &'a RuntimeContext,
        _cursors: &'a crate::cursor::CursorStore,
        _protocol_version: &'a ProtocolVersion,
        cancellation: &'a CancellationToken,
    ) -> OptionalRegistryFuture<'a, Result<CallToolResult, ErrorData>> {
        Box::pin(async move {
            match request.name.as_ref() {
                ARTIFACT_STATUS => {
                    decode_arguments::<EmptyInput>(request.arguments)?;
                    status_tool()
                        .map_err(|_| {
                            ErrorData::internal_error("Artifact contract unavailable.", None)
                        })?
                        .success(&artifact_status(runtime).await)
                        .map_err(|_| {
                            ErrorData::internal_error("Artifact result unavailable.", None)
                        })
                }
                ARTIFACT_STAGE_ALLOCATE => {
                    let input = decode_arguments::<StageAllocateInput>(request.arguments)?;
                    Ok(
                        match run_artifact_operation(
                            runtime,
                            cancellation,
                            ARTIFACT_STAGE_ALLOCATE,
                            stage_allocate(runtime, input),
                        )
                        .await
                        {
                            Ok(output) => encode_success(stage_allocate_tool(), &output),
                            Err(error) => tool_error(&error.tool_error()),
                        },
                    )
                }
                ARTIFACT_STAGE_RELEASE => {
                    let input = decode_arguments::<StageHandleInput>(request.arguments)?;
                    Ok(
                        match run_artifact_operation(
                            runtime,
                            cancellation,
                            ARTIFACT_STAGE_RELEASE,
                            stage_release(runtime, input),
                        )
                        .await
                        {
                            Ok(output) => encode_success(stage_release_tool(), &output),
                            Err(error) => tool_error(&error.tool_error()),
                        },
                    )
                }
                DOCUMENT_IMPORT_CREATE => {
                    let input = decode_arguments::<DocumentImportCreateInput>(request.arguments)?;
                    Ok(
                        match run_artifact_operation(
                            runtime,
                            cancellation,
                            DOCUMENT_IMPORT_CREATE,
                            document_import_create(runtime, input),
                        )
                        .await
                        {
                            Ok(output) => encode_success(document_import_create_tool(), &output),
                            Err(error) => tool_error(&error.tool_error()),
                        },
                    )
                }
                DOCUMENT_IMPORT_UPDATE => {
                    let input = decode_arguments::<DocumentImportUpdateInput>(request.arguments)?;
                    Ok(
                        match run_artifact_operation(
                            runtime,
                            cancellation,
                            DOCUMENT_IMPORT_UPDATE,
                            document_import_update(runtime, input),
                        )
                        .await
                        {
                            Ok(output) => encode_success(document_import_update_tool(), &output),
                            Err(error) => tool_error(&error.tool_error()),
                        },
                    )
                }
                DOCUMENT_EXPORT => {
                    let input = decode_arguments::<DocumentExportInput>(request.arguments)?;
                    Ok(
                        match run_artifact_operation(
                            runtime,
                            cancellation,
                            DOCUMENT_EXPORT,
                            document_export(runtime, input),
                        )
                        .await
                        {
                            Ok(output) => encode_success(document_export_tool(), &output),
                            Err(error) => tool_error(&error.tool_error()),
                        },
                    )
                }
                FILE_IMPORT => {
                    let input = decode_arguments::<FileImportInput>(request.arguments)?;
                    Ok(
                        match run_artifact_operation(
                            runtime,
                            cancellation,
                            FILE_IMPORT,
                            file_import(runtime, input, cancellation),
                        )
                        .await
                        {
                            Ok(output) => encode_success(import_tool(), &output),
                            Err(error) => tool_error(&error.tool_error()),
                        },
                    )
                }
                FILE_EXPORT => {
                    let input = decode_arguments::<FileExportInput>(request.arguments)?;
                    Ok(
                        match run_artifact_operation(
                            runtime,
                            cancellation,
                            FILE_EXPORT,
                            file_export(runtime, input, cancellation),
                        )
                        .await
                        {
                            Ok(output) => encode_success(export_tool(), &output),
                            Err(error) => tool_error(&error.tool_error()),
                        },
                    )
                }
                _ => Err(ErrorData::method_not_found::<CallToolRequestMethod>()),
            }
        })
    }
}

async fn run_artifact_operation<T, F>(
    runtime: &RuntimeContext,
    cancellation: &CancellationToken,
    operation_name: &'static str,
    operation: F,
) -> Result<T, ArtifactToolError>
where
    F: Future<Output = Result<T, ArtifactToolError>>,
{
    let started = std::time::Instant::now();
    tokio::select! {
        biased;
        () = cancellation.cancelled() => {
            runtime.record_controlled_failure(
                OperationContext::new(operation_name),
                started.elapsed(),
                ControlledFailureKind::Cancelled,
            );
            Err(ArtifactToolError::Indeterminate)
        },
        result = tokio::time::timeout(
            runtime.artifact_config().limits.operation_timeout,
            operation,
        ) => match result {
            Ok(result) => result,
            Err(_) => {
                runtime.record_controlled_failure(
                    OperationContext::new(operation_name),
                    started.elapsed(),
                    ControlledFailureKind::TimedOut,
                );
                Err(ArtifactToolError::Indeterminate)
            }
        },
    }
}

fn encode_success<O: Serialize>(
    contract: Result<WorkflowTool<O>, SchemaContractError>,
    output: &O,
) -> CallToolResult {
    let Ok(contract) = contract else {
        return tool_error(&ToolError::upstream());
    };
    match contract.success(output) {
        Ok(result) => result,
        Err(_) => tool_error(&ToolError::upstream()),
    }
}

fn decode_arguments<T: for<'de> Deserialize<'de>>(
    arguments: Option<rmcp::model::JsonObject>,
) -> Result<T, ErrorData> {
    let arguments = arguments.unwrap_or_default();
    serde_json::from_value(Value::Object(arguments)).map_err(|_| {
        ErrorData::invalid_params(
            "Tool arguments do not match the declared schema.",
            Some(serde_json::json!({"code": "validation"})),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contracts_are_closed_and_payload_free() {
        for tool in [
            status_tool().expect("status").into_tool(),
            stage_allocate_tool().expect("stage allocate").into_tool(),
            stage_release_tool().expect("stage release").into_tool(),
            document_export_tool().expect("document export").into_tool(),
            document_import_create_tool()
                .expect("document create")
                .into_tool(),
            document_import_update_tool()
                .expect("document update")
                .into_tool(),
            import_tool().expect("import").into_tool(),
            export_tool().expect("export").into_tool(),
        ] {
            let encoded = serde_json::to_string(&tool).expect("tool JSON");
            assert!(!encoded.contains("content_base64"));
            assert!(!encoded.contains("absolute"));
            assert_eq!(tool.input_schema["additionalProperties"], false);
        }
    }

    #[test]
    fn names_and_media_types_are_strict() {
        assert!(validate_name("report.bin").is_ok());
        assert!(validate_name("../report").is_err());
        assert_eq!(
            normalize_media_type(Some("application/octet-stream")).expect("MIME"),
            Some("application/octet-stream".to_owned())
        );
        assert!(normalize_media_type(Some("text/plain; charset=utf-8")).is_err());
    }

    #[test]
    fn multi_chunk_streams_require_a_strong_etag_or_complete_expected_hash() {
        assert!(stream_consistency_proven(64, 64, None, None));
        assert!(stream_consistency_proven(65, 64, Some("\"strong\""), None));
        assert!(stream_consistency_proven(
            65,
            64,
            None,
            Some("expected-sha256")
        ));
        assert!(!stream_consistency_proven(65, 64, None, None));
    }

    #[test]
    fn document_text_is_strict_and_plain_text_is_escaped() {
        assert_eq!(
            validate_document_text(b"first\r\nsecond".to_vec()).expect("UTF-8"),
            "first\r\nsecond"
        );
        assert!(validate_document_text(vec![0xef, 0xbb, 0xbf, b'x']).is_err());
        assert!(validate_document_text(b"first\rsecond".to_vec()).is_err());
        assert!(validate_document_text(b"x\0y".to_vec()).is_err());
        assert_eq!(plain_text_markdown("# x_"), r"\# x\_");
    }

    #[test]
    fn optional_artifact_fields_reject_explicit_null() {
        let file_import = serde_json::json!({
            "space": "space",
            "source": {"local": {"root": "root", "path": "input.bin"}},
            "name": "input.bin",
            "media_type": null,
            "idempotency_key": "key"
        });
        assert!(serde_json::from_value::<FileImportInput>(file_import).is_err());

        let null_local = serde_json::json!({
            "space": "space",
            "source": {"local": null},
            "name": "input.bin",
            "idempotency_key": "key"
        });
        assert!(serde_json::from_value::<FileImportInput>(null_local).is_err());

        let null_path = serde_json::json!({
            "space": "space",
            "source": {"local": {"root": "root", "path": null}},
            "name": "input.bin",
            "idempotency_key": "key"
        });
        assert!(serde_json::from_value::<FileImportInput>(null_path).is_err());

        let file_export = serde_json::json!({
            "space": "space",
            "file_id": "file",
            "destination": {"remote": true},
            "expected_strong_etag": null,
            "idempotency_key": "key"
        });
        assert!(serde_json::from_value::<FileExportInput>(file_export).is_err());

        let document_create = serde_json::json!({
            "space": "space",
            "source": {"staged_handle": "a".repeat(64)},
            "source_format": "markdown",
            "object_type": "page",
            "name": "name",
            "properties": null,
            "idempotency_key": "key"
        });
        assert!(serde_json::from_value::<DocumentImportCreateInput>(document_create).is_err());

        let document_export = serde_json::json!({
            "space": "space",
            "object_id": "object",
            "destination": {"remote": true},
            "expected_body_sha256": null,
            "idempotency_key": "key"
        });
        assert!(serde_json::from_value::<DocumentExportInput>(document_export).is_err());

        let stage = serde_json::json!({
            "space": "space",
            "size_bytes": 1,
            "media_type": null
        });
        assert!(serde_json::from_value::<StageAllocateInput>(stage).is_err());
    }

    #[tokio::test]
    async fn import_idempotency_never_redispatches_a_candidate() {
        let state = ArtifactOperationState::default();
        let key = digest_fields(b"key", &[b"same"]);
        let fingerprint = digest_fields(b"fingerprint", &[b"same"]);
        assert!(matches!(
            state.reserve_import(key, fingerprint).await,
            Ok(ImportIdempotency::Dispatch)
        ));
        let candidate =
            EntityId::new("bafyreie6n5l5nkbjal37su54cha4coy7qzuhrnajluzv5qd5jvtsrxkequ".to_owned())
                .expect("candidate");
        let findings = vec![ValidatorFinding {
            id: "retained".to_owned(),
            status: crate::artifact_validators::ValidatorStatus::Accepted,
            detected_media_type: Some("text/plain".to_owned()),
        }];
        state
            .set_outcome(
                key,
                OperationOutcome::ImportCandidate {
                    candidate: candidate.clone(),
                    validator_findings: findings.clone(),
                },
            )
            .await;
        assert!(matches!(
            state.reserve_import(key, fingerprint).await,
            Ok(ImportIdempotency::VerifyCandidate { candidate: actual, validator_findings })
                if actual == candidate && validator_findings.len() == 1 && validator_findings[0].id == "retained"
        ));
        assert!(matches!(
            state
                .reserve_import(key, digest_fields(b"fingerprint", &[b"different"]))
                .await,
            Err(ArtifactToolError::Conflict)
        ));
    }

    #[tokio::test]
    async fn import_timeout_preserves_candidate_only_while_verifying() {
        let state = ArtifactOperationState::default();
        let key = digest_fields(b"key", &[b"phase"]);
        let fingerprint = digest_fields(b"fingerprint", &[b"phase"]);
        let candidate =
            EntityId::new("bafyreie6n5l5nkbjal37su54cha4coy7qzuhrnajluzv5qd5jvtsrxkequ".to_owned())
                .expect("candidate");

        assert!(matches!(
            state.reserve_import(key, fingerprint).await,
            Ok(ImportIdempotency::Dispatch)
        ));
        state
            .set_outcome(
                key,
                OperationOutcome::ImportVerifying {
                    candidate: candidate.clone(),
                    validator_findings: Vec::new(),
                },
            )
            .await;
        state.settle_import_timeout(key).await;
        assert!(matches!(
            state.reserve_import(key, fingerprint).await,
            Ok(ImportIdempotency::VerifyCandidate { candidate: actual, .. }) if actual == candidate
        ));

        state
            .set_outcome(key, OperationOutcome::ImportCleaning(candidate))
            .await;
        state.settle_import_timeout(key).await;
        assert!(matches!(
            state.reserve_import(key, fingerprint).await,
            Err(ArtifactToolError::Indeterminate)
        ));
    }

    #[tokio::test]
    async fn import_candidate_survives_initial_and_replay_completion_gaps() {
        let state = ArtifactOperationState::default();
        let key = digest_fields(b"key", &[b"consume-complete-gap"]);
        let fingerprint = digest_fields(b"fingerprint", &[b"consume-complete-gap"]);
        let candidate =
            EntityId::new("bafyreie6n5l5nkbjal37su54cha4coy7qzuhrnajluzv5qd5jvtsrxkequ".to_owned())
                .expect("candidate");
        let findings = vec![ValidatorFinding {
            id: "preserved-evidence".to_owned(),
            status: crate::artifact_validators::ValidatorStatus::Accepted,
            detected_media_type: Some("text/plain".to_owned()),
        }];

        assert!(matches!(
            state.reserve_import(key, fingerprint).await,
            Ok(ImportIdempotency::Dispatch)
        ));
        state
            .set_outcome(
                key,
                OperationOutcome::ImportCandidate {
                    candidate: candidate.clone(),
                    validator_findings: findings.clone(),
                },
            )
            .await;

        // Initial settlement cancellation after staged consumption leaves the
        // verified candidate replayable with its validator evidence.
        state.settle_import_timeout(key).await;
        assert!(matches!(
            state.reserve_import(key, fingerprint).await,
            Ok(ImportIdempotency::VerifyCandidate {
                candidate: actual,
                validator_findings,
            }) if actual == candidate
                && validator_findings.first().is_some_and(|finding| finding.id == "preserved-evidence")
        ));

        // A second cancellation in candidate replay's consume/Complete gap is
        // equally replayable and never returns Dispatch.
        state.settle_import_timeout(key).await;
        assert!(matches!(
            state.reserve_import(key, fingerprint).await,
            Ok(ImportIdempotency::VerifyCandidate { candidate: actual, .. })
                if actual == candidate
        ));
    }

    #[tokio::test]
    async fn definitive_import_failure_removes_reservation_for_same_key_retry() {
        let state = ArtifactOperationState::default();
        let key = digest_fields(b"key", &[b"definitive-rejection"]);
        let fingerprint = digest_fields(b"fingerprint", &[b"definitive-rejection"]);
        assert!(matches!(
            state.reserve_import(key, fingerprint).await,
            Ok(ImportIdempotency::Dispatch)
        ));
        state.remove(key).await;
        assert!(matches!(
            state.reserve_import(key, fingerprint).await,
            Ok(ImportIdempotency::Dispatch)
        ));
        assert!(matches!(
            state
                .reserve_import(key, digest_fields(b"fingerprint", &[b"wrong-operation"]))
                .await,
            Err(ArtifactToolError::Conflict)
        ));
    }

    #[test]
    fn malformed_upload_response_never_yields_cleanup_authority() {
        let space = SpaceId::new(
            "bafyreid5fvqlnsobih2keakcxjrrlpmly6kf37klzjzen4ibfdgalcdp4y.2tq5w93cr6oe7",
        )
        .expect("space id");
        let valid_id = "bafyreie6n5l5nkbjal37su54cha4coy7qzuhrnajluzv5qd5jvtsrxkequ";
        let response = |id: &str, space_id: &str, size| FileObject {
            id: id.to_owned(),
            space_id: space_id.to_owned(),
            name: Some("fixture.bin".to_owned()),
            size,
            mime: Some("application/octet-stream".to_owned()),
            added_at: None,
            file_type: anytype::files::FileType::default(),
            style: anytype::files::FileStyle::Auto,
            target_object_id: None,
            details: Value::Null,
        };

        assert!(
            validated_uploaded_candidate(&response(valid_id, space.as_str(), Some(5)), &space, 5)
                .is_ok()
        );
        for malformed in [
            response("", space.as_str(), Some(5)),
            response(valid_id, "wrong-space", Some(5)),
            response(valid_id, space.as_str(), Some(4)),
            response(valid_id, space.as_str(), None),
            response(valid_id, space.as_str(), Some(-1)),
        ] {
            assert_eq!(
                validated_uploaded_candidate(&malformed, &space, 5),
                Err(ArtifactToolError::Indeterminate)
            );
        }
    }

    #[tokio::test]
    async fn in_flight_document_retry_is_indeterminate() {
        let state = ArtifactOperationState::default();
        let key = digest_fields(b"key", &[b"document"]);
        let fingerprint = digest_fields(b"fingerprint", &[b"document"]);
        assert!(matches!(
            state.reserve_document_mutation(key, fingerprint).await,
            Ok(DocumentMutationIdempotency::Dispatch)
        ));
        assert!(matches!(
            state.reserve_document_mutation(key, fingerprint).await,
            Err(ArtifactToolError::Indeterminate)
        ));
    }

    #[tokio::test]
    async fn document_predispatch_conflict_releases_reservation() {
        let base = std::env::temp_dir().join(format!(
            "any-mcp-document-predispatch-{}-{}",
            std::process::id(),
            getrandom::u64().unwrap_or(0)
        ));
        let import = base.join("import");
        let export = base.join("export");
        std::fs::create_dir_all(&import).expect("create import fixture");
        std::fs::create_dir_all(&export).expect("create export fixture");
        let source_path = import.join("document.md");
        let retained_path = import.join("retained.md");
        let replacement_path = import.join("replacement.md");
        std::fs::write(&source_path, b"original\n").expect("write original source");
        std::fs::write(&replacement_path, b"replaced\n").expect("write replacement source");
        let config = crate::artifact_config::ArtifactConfig::from_toml(&format!(
            "schema_version = 1\n[spaces]\nread_only = false\n\
             [[roots.import]]\nid = \"inbox\"\npath = {import:?}\n\
             [[roots.export]]\nid = \"outbox\"\npath = {export:?}\n"
        ))
        .expect("parse fixture config");
        let roots =
            crate::artifact_roots::RootRegistry::activate(&config).expect("activate fixture roots");
        let relative = RelativeNativePath::from_utf8("document.md").expect("relative source");
        let prepared = PreparedImport::Local {
            source: roots
                .static_policy()
                .open_import("inbox", &relative, 64)
                .expect("open source"),
            sha256: digest_bytes(b"original\n"),
            root_id: "inbox".to_owned(),
        };

        let state = ArtifactOperationState::default();
        let key = digest_fields(b"key", &[b"document-predispatch"]);
        let fingerprint = digest_fields(b"fingerprint", &[b"document-predispatch"]);
        assert!(matches!(
            state.reserve_document_mutation(key, fingerprint).await,
            Ok(DocumentMutationIdempotency::Dispatch)
        ));

        std::fs::rename(&source_path, &retained_path).expect("retain original name");
        std::fs::rename(&replacement_path, &source_path).expect("replace source name");
        assert_eq!(
            settle_document_source_revalidation(&state, key, prepared.verify_before_dispatch())
                .await,
            Err(ArtifactToolError::Conflict)
        );
        assert!(!state.entries().contains_key(&key));
        assert!(matches!(
            state.reserve_document_mutation(key, fingerprint).await,
            Ok(DocumentMutationIdempotency::Dispatch)
        ));

        let fresh_key = digest_fields(b"key", &[b"document-predispatch-retry"]);
        assert!(matches!(
            state
                .reserve_document_mutation(fresh_key, fingerprint)
                .await,
            Ok(DocumentMutationIdempotency::Dispatch)
        ));

        drop(prepared);
        std::fs::remove_dir_all(base).expect("clean fixture");
    }

    fn traversal_destination() -> ResolvedDestination {
        ResolvedDestination::Local(LocalLocation {
            root: "outbox".to_owned(),
            path: Omittable::Present("../escape.bin".to_owned()),
            path_native: Omittable::Missing,
        })
    }

    #[tokio::test]
    async fn invalid_export_destinations_never_reserve_idempotency_entries() {
        let state = ArtifactOperationState::default();
        for index in 0_u64..64 {
            let unique = index.to_le_bytes();
            let key = digest_fields(b"key", &[b"invalid-export", &unique]);
            let fingerprint = digest_fields(b"fingerprint", &[b"invalid-export", &unique]);

            for _ in 0..2 {
                assert!(matches!(
                    reserve_file_export_operation(
                        &state,
                        key,
                        fingerprint,
                        traversal_destination()
                    )
                    .await,
                    Err(ArtifactToolError::Validation)
                ));
                assert!(matches!(
                    reserve_document_export_operation(
                        &state,
                        key,
                        fingerprint,
                        traversal_destination()
                    )
                    .await,
                    Err(ArtifactToolError::Validation)
                ));
            }
        }

        assert!(state.entries().is_empty());
    }

    #[tokio::test]
    async fn definite_export_preflight_failures_release_reservations() {
        let state = ArtifactOperationState::default();
        let file_key = digest_fields(b"key", &[b"file-preflight"]);
        let file_fingerprint = digest_fields(b"fingerprint", &[b"file-preflight"]);
        let (_, file_reservation) = reserve_file_export_operation(
            &state,
            file_key,
            file_fingerprint,
            ResolvedDestination::Remote,
        )
        .await
        .expect("reserve file export");
        assert!(matches!(file_reservation, ExportIdempotency::Dispatch));
        assert_eq!(
            settle_export_failure(&state, file_key, ArtifactToolError::NotFound).await,
            ArtifactToolError::NotFound
        );

        let document_key = digest_fields(b"key", &[b"document-preflight"]);
        let document_fingerprint = digest_fields(b"fingerprint", &[b"document-preflight"]);
        let (_, document_reservation) = reserve_document_export_operation(
            &state,
            document_key,
            document_fingerprint,
            ResolvedDestination::Remote,
        )
        .await
        .expect("reserve document export");
        assert!(matches!(
            document_reservation,
            DocumentExportIdempotency::Dispatch
        ));
        assert_eq!(
            settle_export_failure(&state, document_key, ArtifactToolError::Conflict).await,
            ArtifactToolError::Conflict
        );

        assert!(state.entries().is_empty());
        assert!(matches!(
            state.reserve_export(file_key, file_fingerprint).await,
            Ok(ExportIdempotency::Dispatch)
        ));
        assert_eq!(
            settle_export_failure(&state, file_key, ArtifactToolError::Indeterminate).await,
            ArtifactToolError::Indeterminate
        );
        assert!(matches!(
            state.reserve_export(file_key, file_fingerprint).await,
            Err(ArtifactToolError::Indeterminate)
        ));
        assert_eq!(state.entries().len(), 1);
    }

    #[test]
    fn indeterminate_stage_publication_retains_cleanup_ownership() {
        assert!(!should_release_failed_export_stage(
            ArtifactToolError::Indeterminate
        ));
        for error in [
            ArtifactToolError::NotFound,
            ArtifactToolError::Conflict,
            ArtifactToolError::Bounded,
            ArtifactToolError::Upstream,
        ] {
            assert!(should_release_failed_export_stage(error));
        }
    }
}
