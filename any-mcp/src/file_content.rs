// any-mcp - bounded, workflow-oriented MCP server for Anytype
//
// SPDX-FileCopyrightText: 2026 Steve Schoettler
// SPDX-License-Identifier: Apache-2.0

//! Bounded Anytype file metadata, byte reads, and hash-bound MCP resources.
//!
//! This module implements the approved read side of the optional `files`
//! registry without linking that registry into the production catalog. The
//! terminal files task adds upload, real-headless coverage, and atomic
//! production linkage after all slices are complete.

use std::{borrow::Cow, fmt, io};

use anytype::{
    error::AnytypeError,
    files::{FileContentResponse, FileHttpMetadata},
    objects::Object,
};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use rmcp::{
    model::{
        Annotations, CallToolRequestMethod, CallToolRequestParams, CallToolResult, ContentBlock,
        ErrorData, ProtocolVersion, ReadResourceRequestMethod, ReadResourceRequestParams,
        ReadResourceResult, ResourceContents, ResourceTemplate, Role,
    },
    schemars::{JsonSchema, Schema, SchemaGenerator, json_schema},
};
use serde::{Deserialize, Deserializer, Serialize, de};
use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;

use crate::{
    domain::{EntityId, SpaceId},
    error::{AnytypeErrorMapping, ToolError, ToolErrorCode},
    optional_toolsets::{
        OptionalRegistryFuture, OptionalRegistryTool, OptionalToolsetMetadata,
        OptionalToolsetRegistry,
    },
    protocol::{ToolProfile, WorkflowTool, workflow_tool},
    result::tool_error,
    runtime::{
        ControlledOperationError, OperationContext, OperationFailureDiagnostic, RuntimeContext,
    },
    schema::SchemaContractError,
    validation::{Omittable, optional_non_null_schema},
};

/// Exact resource template for a previously returned Anytype file byte chunk.
pub const FILE_BYTE_RESOURCE_TEMPLATE: &str =
    "anytype-file://bytes/{space_id}/{file_id}/{offset}/{length}/{sha256}";
/// Maximum decoded bytes returned by one file operation.
pub const MAX_FILE_CONTENT_BYTES: u64 = 65_536;
/// Maximum complete encoded frame admitted for embedded text.
pub const MAX_TEXT_FRAME_BYTES: usize = 70_000;
/// Maximum complete encoded file result frame.
pub const MAX_FILE_RESULT_FRAME_BYTES: usize = 96 * 1024;
/// Maximum bytes in one canonical file resource URI.
pub const MAX_FILE_RESOURCE_URI_BYTES: usize = 768;

const MAX_SPACE_REFERENCE_CHARS: usize = 512;
const MAX_MEDIA_TYPE_BYTES: usize = 255;
const MAX_STRONG_ETAG_BYTES: usize = 256;
const MAX_OBJECT_PREFLIGHT_BYTES: u64 = 262_144;
const MAX_ERROR_BODY_BYTES: u64 = 65_536;
const MAX_HEADER_EVIDENCE_BYTES: u64 = 4_096;
const MAX_SAFE_ATTEMPTS: u32 = 6;
const JSON_SAFE_INTEGER_MAX: u64 = 9_007_199_254_740_991;
const DEFAULT_READ_LENGTH: u64 = MAX_FILE_CONTENT_BYTES;

const INVALID_RESOURCE_URI: &str = "Invalid Anytype file resource URI.";
const MISSING_RESOURCE: &str = "Resource not found.";
const RESOURCE_UPSTREAM: &str = "Anytype could not complete the resource read.";

/// A unique space name or stable identifier with the common optional-toolset bound.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct SpaceRef(String);

impl SpaceRef {
    /// Validates a reference without trimming or normalizing its spelling.
    pub fn new(value: impl Into<String>) -> Result<Self, FileValueError> {
        let value = value.into();
        if value.is_empty() || value.trim().is_empty() {
            return Err(FileValueError::Invalid);
        }
        if value.chars().count() > MAX_SPACE_REFERENCE_CHARS {
            return Err(FileValueError::TooLong);
        }
        Ok(Self(value))
    }

    /// Borrows the exact validated spelling.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for SpaceRef {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::new(String::deserialize(deserializer)?).map_err(de::Error::custom)
    }
}

impl JsonSchema for SpaceRef {
    fn schema_name() -> Cow<'static, str> {
        "SpaceRef".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "minLength": 1,
            "maxLength": MAX_SPACE_REFERENCE_CHARS
        })
    }
}

/// A nonnegative integer exactly representable by JavaScript-number MCP hosts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct JsonSafeInteger(u64);

impl JsonSafeInteger {
    /// Constructs a JSON-safe integer.
    pub fn new(value: u64) -> Result<Self, FileValueError> {
        if value > JSON_SAFE_INTEGER_MAX {
            return Err(FileValueError::TooLarge);
        }
        Ok(Self(value))
    }

    /// Returns the primitive value.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

impl<'de> Deserialize<'de> for JsonSafeInteger {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::new(u64::deserialize(deserializer)?).map_err(de::Error::custom)
    }
}

impl JsonSchema for JsonSafeInteger {
    fn schema_name() -> Cow<'static, str> {
        "JsonSafeInteger".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "integer",
            "minimum": 0,
            "maximum": JSON_SAFE_INTEGER_MAX
        })
    }
}

/// A requested file-read length from 1 through 65,536 bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct FileReadLength(u64);

impl FileReadLength {
    /// Validates the exact read-length range.
    pub fn new(value: u64) -> Result<Self, FileValueError> {
        if !(1..=MAX_FILE_CONTENT_BYTES).contains(&value) {
            return Err(FileValueError::Invalid);
        }
        Ok(Self(value))
    }

    /// Returns the byte count.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

impl<'de> Deserialize<'de> for FileReadLength {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::new(u64::deserialize(deserializer)?).map_err(de::Error::custom)
    }
}

impl JsonSchema for FileReadLength {
    fn schema_name() -> Cow<'static, str> {
        "FileReadLength".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "integer",
            "minimum": 1,
            "maximum": MAX_FILE_CONTENT_BYTES,
            "default": DEFAULT_READ_LENGTH
        })
    }
}

/// A bounded, normalized MIME value returned by Anytype.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct FileMediaType(String);

impl FileMediaType {
    fn from_evidence(value: Option<&str>) -> Result<Self, FileOperationError> {
        let value = value.unwrap_or("application/octet-stream");
        if value.len() > MAX_MEDIA_TYPE_BYTES
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_graphic() || byte == b' ')
        {
            return Err(FileOperationError::Tool(ToolError::upstream()));
        }
        let parsed = value
            .parse::<mime::Mime>()
            .map_err(|_| FileOperationError::Tool(ToolError::upstream()))?;
        let normalized = parsed.to_string();
        if normalized.len() > MAX_MEDIA_TYPE_BYTES {
            return Err(FileOperationError::Tool(ToolError::bounded_result()));
        }
        Ok(Self(normalized))
    }

    /// Borrows the normalized MIME spelling.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    fn parsed(&self) -> Result<mime::Mime, FileOperationError> {
        self.0.parse().map_err(|_| FileOperationError::Encoding)
    }
}

impl JsonSchema for FileMediaType {
    fn schema_name() -> Cow<'static, str> {
        "FileMediaType".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "minLength": 1,
            "maxLength": MAX_MEDIA_TYPE_BYTES,
            "pattern": "^[\\x21-\\x7E](?:[\\x20-\\x7E]{0,253}[\\x21-\\x7E])?$"
        })
    }
}

/// A quoted strong HTTP entity tag suitable for `If-Range`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct StrongEntityTag(String);

impl StrongEntityTag {
    /// Validates a bounded strong ETag without changing its bytes.
    pub fn new(value: impl Into<String>) -> Result<Self, FileValueError> {
        let value = value.into();
        let bytes = value.as_bytes();
        let inner = bytes
            .strip_prefix(b"\"")
            .and_then(|value| value.strip_suffix(b"\""))
            .ok_or(FileValueError::Invalid)?;
        if value.starts_with("W/")
            || value.len() > MAX_STRONG_ETAG_BYTES
            || inner
                .iter()
                .any(|byte| *byte == b'"' || *byte < 0x21 || *byte == 0x7f)
        {
            return Err(FileValueError::Invalid);
        }
        Ok(Self(value))
    }

    /// Borrows the exact validator.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    fn from_optional_evidence(value: Option<&str>) -> Result<Option<Self>, FileOperationError> {
        match value {
            Some(value) if value.starts_with("W/") => Ok(None),
            Some(value) => Self::new(value.to_owned())
                .map(Some)
                .map_err(|_| FileOperationError::Tool(ToolError::upstream())),
            None => Ok(None),
        }
    }
}

impl<'de> Deserialize<'de> for StrongEntityTag {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::new(String::deserialize(deserializer)?).map_err(de::Error::custom)
    }
}

impl JsonSchema for StrongEntityTag {
    fn schema_name() -> Cow<'static, str> {
        "StrongEntityTag".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "minLength": 2,
            "maxLength": MAX_STRONG_ETAG_BYTES,
            "pattern": "^\"[^\"\\x00-\\x20\\x7F]*\"$"
        })
    }
}

/// A canonical 29-byte IMF-fixdate supplied by Anytype.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct FileHttpDate(String);

impl FileHttpDate {
    fn from_evidence(value: impl Into<String>) -> Result<Self, FileOperationError> {
        let value = value.into();
        let canonical = httpdate::parse_http_date(&value)
            .map(httpdate::fmt_http_date)
            .map_err(|_| FileOperationError::Tool(ToolError::upstream()))?;
        if value != canonical || value.len() != 29 {
            return Err(FileOperationError::Tool(ToolError::upstream()));
        }
        Ok(Self(value))
    }
}

impl JsonSchema for FileHttpDate {
    fn schema_name() -> Cow<'static, str> {
        "FileHttpDate".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({"type": "string", "minLength": 29, "maxLength": 29})
    }
}

/// A lowercase SHA-256 digest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct FileSha256(String);

impl FileSha256 {
    fn digest(bytes: &[u8]) -> Self {
        let digest = Sha256::digest(bytes);
        let mut value = String::with_capacity(64);
        for byte in digest {
            value.push(char::from_digit(u32::from(byte >> 4), 16).unwrap_or('0'));
            value.push(char::from_digit(u32::from(byte & 0x0f), 16).unwrap_or('0'));
        }
        Self(value)
    }

    fn parse(value: &str) -> Result<Self, FileValueError> {
        if value.len() == 64
            && value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            Ok(Self(value.to_owned()))
        } else {
            Err(FileValueError::Invalid)
        }
    }

    /// Borrows the lowercase digest.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl JsonSchema for FileSha256 {
    fn schema_name() -> Cow<'static, str> {
        "FileSha256".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "minLength": 64,
            "maxLength": 64,
            "pattern": "^[0-9a-f]{64}$"
        })
    }
}

/// Canonical hash-bound URI for one exact Anytype file chunk.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct FileResourceUri(String);

impl FileResourceUri {
    /// Constructs the exact canonical URI from validated components.
    pub fn new(
        space_id: &SpaceId,
        file_id: &EntityId,
        offset: JsonSafeInteger,
        length: u64,
        sha256: &FileSha256,
    ) -> Result<Self, FileValueError> {
        if length > MAX_FILE_CONTENT_BYTES || (length == 0 && offset.get() != 0) {
            return Err(FileValueError::Invalid);
        }
        let value = format!(
            "anytype-file://bytes/{}/{}/{}/{}/{}",
            space_id.as_str(),
            file_id.as_str(),
            offset.get(),
            length,
            sha256.as_str()
        );
        if value.len() > MAX_FILE_RESOURCE_URI_BYTES || !value.is_ascii() {
            return Err(FileValueError::TooLong);
        }
        Ok(Self(value))
    }

    /// Parses only an already-canonical file resource URI.
    pub fn parse(value: &str) -> Result<ParsedFileResourceUri, FileValueError> {
        if value.len() > MAX_FILE_RESOURCE_URI_BYTES || !value.is_ascii() {
            return Err(FileValueError::Invalid);
        }
        let suffix = value
            .strip_prefix("anytype-file://bytes/")
            .ok_or(FileValueError::Invalid)?;
        let mut segments = suffix.split('/');
        let space_id = SpaceId::new(segments.next().ok_or(FileValueError::Invalid)?)
            .map_err(|_| FileValueError::Invalid)?;
        let file_id = EntityId::new(segments.next().ok_or(FileValueError::Invalid)?)
            .map_err(|_| FileValueError::Invalid)?;
        let offset = parse_canonical_u64(segments.next().ok_or(FileValueError::Invalid)?)
            .and_then(|value| JsonSafeInteger::new(value).ok())
            .ok_or(FileValueError::Invalid)?;
        let length = parse_canonical_u64(segments.next().ok_or(FileValueError::Invalid)?)
            .filter(|value| *value <= MAX_FILE_CONTENT_BYTES)
            .ok_or(FileValueError::Invalid)?;
        let sha256 = FileSha256::parse(segments.next().ok_or(FileValueError::Invalid)?)?;
        if segments.next().is_some() || (length == 0 && offset.get() != 0) {
            return Err(FileValueError::Invalid);
        }
        let uri = Self::new(&space_id, &file_id, offset, length, &sha256)?;
        if uri.as_str() != value {
            return Err(FileValueError::Invalid);
        }
        Ok(ParsedFileResourceUri {
            uri,
            space_id,
            file_id,
            offset,
            length,
            sha256,
        })
    }

    /// Borrows the canonical URI.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl JsonSchema for FileResourceUri {
    fn schema_name() -> Cow<'static, str> {
        "FileResourceUri".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "minLength": 1,
            "maxLength": MAX_FILE_RESOURCE_URI_BYTES,
            "pattern": "^anytype-file://bytes/(?!\\.{1,2}/)[A-Za-z0-9._~-]+/(?!\\.{1,2}/)[A-Za-z0-9._~-]+/(?:0|[1-9][0-9]*)/(?:0|[1-9][0-9]*)/[0-9a-f]{64}$"
        })
    }
}

/// Components extracted from a canonical file resource URI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedFileResourceUri {
    uri: FileResourceUri,
    space_id: SpaceId,
    file_id: EntityId,
    offset: JsonSafeInteger,
    length: u64,
    sha256: FileSha256,
}

/// Failure to construct one bounded files-domain value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileValueError {
    /// The value does not have the exact required shape.
    Invalid,
    /// The value exceeds a scalar or byte limit.
    TooLong,
    /// The number exceeds the JSON-safe integer ceiling.
    TooLarge,
}

impl fmt::Display for FileValueError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("invalid bounded files-domain value")
    }
}

impl std::error::Error for FileValueError {}

/// Exact input for `file_metadata`.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FileMetadataInput {
    /// Stable space identifier; production linkage awaits bounded name resolution.
    space: SpaceRef,
    /// Stable file object identifier; names and CIDs are not accepted.
    file_id: EntityId,
}

/// Exact output for `file_metadata`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FileMetadataOutput {
    /// Stable file object identifier.
    file_id: EntityId,
    /// Resolved stable space identifier.
    space_id: SpaceId,
    /// Normalized bounded response MIME.
    media_type: FileMediaType,
    /// Complete representation size.
    size_bytes: JsonSafeInteger,
    /// Whether the endpoint advertises byte-range support.
    accepts_byte_ranges: bool,
    /// Strong validator when supplied.
    #[serde(skip_serializing_if = "Option::is_none")]
    strong_etag: Option<StrongEntityTag>,
    /// Canonical HTTP modification date when supplied.
    #[serde(skip_serializing_if = "Option::is_none")]
    last_modified: Option<FileHttpDate>,
}

/// Exact input for `file_read`.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FileReadInput {
    /// Stable space identifier; production linkage awaits bounded name resolution.
    space: SpaceRef,
    /// Stable file object identifier; names and CIDs are not accepted.
    file_id: EntityId,
    /// Starting byte offset; omission uses zero.
    #[serde(default)]
    #[schemars(schema_with = "optional_offset_schema")]
    offset: Omittable<JsonSafeInteger>,
    /// Maximum returned bytes; omission uses 65,536.
    #[serde(default)]
    #[schemars(schema_with = "optional_length_schema")]
    length: Omittable<FileReadLength>,
    /// Optional exact strong validator sent as `If-Range` after HEAD preflight.
    #[serde(default)]
    #[schemars(schema_with = "optional_etag_schema")]
    expected_strong_etag: Omittable<StrongEntityTag>,
}

fn optional_offset_schema(generator: &mut SchemaGenerator) -> Schema {
    optional_non_null_schema::<JsonSafeInteger>(generator)
}

fn optional_length_schema(generator: &mut SchemaGenerator) -> Schema {
    optional_non_null_schema::<FileReadLength>(generator)
}

fn optional_etag_schema(generator: &mut SchemaGenerator) -> Schema {
    optional_non_null_schema::<StrongEntityTag>(generator)
}

/// Actual native content representation emitted for a file read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FileContentKind {
    /// MCP image content.
    Image,
    /// MCP audio content.
    Audio,
    /// Embedded text resource content.
    TextResource,
    /// Embedded base64 blob resource content.
    BlobResource,
}

/// Exact structured metadata for a successful `file_read`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FileReadOutput {
    /// Stable file object identifier.
    file_id: EntityId,
    /// Resolved stable space identifier.
    space_id: SpaceId,
    /// Normalized bounded response MIME.
    media_type: FileMediaType,
    /// Actual starting offset.
    offset: JsonSafeInteger,
    /// Caller-requested byte count.
    requested_bytes: JsonSafeInteger,
    /// Actual returned byte count.
    returned_bytes: JsonSafeInteger,
    /// Complete representation byte count.
    total_bytes: JsonSafeInteger,
    /// Whether the returned bytes end exactly at the representation end.
    complete: bool,
    /// SHA-256 of only the returned bytes.
    content_sha256: FileSha256,
    /// Actual second MCP content-block representation.
    content_kind: FileContentKind,
    /// Canonical hash-bound URI for the returned chunk.
    resource_uri: FileResourceUri,
    /// Reconciled strong validator when supplied by HEAD and GET.
    #[serde(skip_serializing_if = "Option::is_none")]
    strong_etag: Option<StrongEntityTag>,
    /// Reconciled modification date when supplied by HEAD and GET.
    #[serde(skip_serializing_if = "Option::is_none")]
    last_modified: Option<FileHttpDate>,
}

/// Builds the exact `file_metadata` tool contract.
pub fn file_metadata_tool() -> Result<WorkflowTool<FileMetadataOutput>, SchemaContractError> {
    workflow_tool::<FileMetadataInput, FileMetadataOutput>(
        "file_metadata",
        "Inspect bounded HTTP metadata for one exact Anytype file object.",
        ToolProfile::Read,
    )
}

/// Builds the exact `file_read` tool contract.
pub fn file_read_tool() -> Result<WorkflowTool<FileReadOutput>, SchemaContractError> {
    workflow_tool::<FileReadInput, FileReadOutput>(
        "file_read",
        "Read one bounded Anytype file byte range as metadata and native MCP content.",
        ToolProfile::Read,
    )
}

/// Builds the exact hash-bound Anytype file resource template.
#[must_use]
pub fn file_byte_resource_template() -> ResourceTemplate {
    ResourceTemplate::new(FILE_BYTE_RESOURCE_TEMPLATE, "anytype_file_bytes")
        .with_title("Anytype file byte chunk")
        .with_description("Read one previously identified, hash-bound Anytype file byte chunk.")
        .with_annotations(
            Annotations::default()
                .with_audience(vec![Role::User, Role::Assistant])
                .with_priority(0.5),
        )
}

/// Transport-neutral handlers for the approved files-domain read workflows.
#[derive(Debug, Clone)]
pub struct FileContentHandlers {
    runtime: RuntimeContext,
    metadata_contract: WorkflowTool<FileMetadataOutput>,
}

impl FileContentHandlers {
    /// Creates handlers and validates both typed contracts.
    pub fn new(runtime: RuntimeContext) -> Result<Self, SchemaContractError> {
        file_read_tool()?;
        Ok(Self {
            runtime,
            metadata_contract: file_metadata_tool()?,
        })
    }

    /// Executes one bounded metadata workflow.
    pub async fn file_metadata(
        &self,
        input: &FileMetadataInput,
        cancellation: &CancellationToken,
    ) -> CallToolResult {
        let client = self.runtime.client();
        let space = input.space.clone();
        let file_id = input.file_id.clone();
        let result = self
            .runtime
            .execute_classified(
                OperationContext::new("file_metadata"),
                cancellation,
                async move {
                    let (space_id, file_id) =
                        exact_space_and_preflight(client, &space, &file_id).await?;
                    metadata_flow(client, space_id, file_id).await
                },
                FileOperationError::diagnostic,
            )
            .await;
        match result {
            Ok(output) => self
                .metadata_contract
                .success(&output)
                .unwrap_or_else(|_| tool_error(&ToolError::upstream())),
            Err(error) => tool_error(&controlled_tool_error(error)),
        }
    }

    /// Executes one bounded byte read for the negotiated MCP revision.
    pub async fn file_read(
        &self,
        input: &FileReadInput,
        protocol_version: &ProtocolVersion,
        cancellation: &CancellationToken,
    ) -> CallToolResult {
        let client = self.runtime.client();
        let space = input.space.clone();
        let file_id = input.file_id.clone();
        let offset = input.offset.as_ref().copied().unwrap_or(JsonSafeInteger(0));
        let length = input
            .length
            .as_ref()
            .copied()
            .unwrap_or(FileReadLength(DEFAULT_READ_LENGTH));
        let expected = input.expected_strong_etag.as_ref().cloned();
        let result = self
            .runtime
            .execute_classified(
                OperationContext::new("file_read"),
                cancellation,
                async move {
                    let (space_id, file_id) =
                        exact_space_and_preflight(client, &space, &file_id).await?;
                    read_flow(client, space_id, file_id, offset, length, expected).await
                },
                FileOperationError::diagnostic,
            )
            .await;
        match result {
            Ok(observation) => encode_file_read(observation, protocol_version)
                .unwrap_or_else(|_| tool_error(&ToolError::upstream())),
            Err(error) => tool_error(&controlled_tool_error(error)),
        }
    }

    /// Reads one exact hash-bound resource URI without name resolution.
    pub async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        cancellation: &CancellationToken,
    ) -> Result<ReadResourceResult, ErrorData> {
        let parsed = FileResourceUri::parse(&request.uri)
            .map_err(|_| ErrorData::invalid_params(INVALID_RESOURCE_URI, None))?;
        let client = self.runtime.client();
        let result = self
            .runtime
            .execute_classified(
                OperationContext::new("file_resource_read"),
                cancellation,
                async move { resource_read_flow(client, parsed).await },
                FileOperationError::diagnostic,
            )
            .await;
        match result {
            Ok(observation) => encode_resource_read(observation),
            Err(error) => Err(controlled_resource_error(error)),
        }
    }
}

#[cfg(test)]
#[derive(Debug)]
pub(crate) struct FileContentRegistry;

#[cfg(test)]
pub(crate) static FILE_CONTENT_REGISTRY: FileContentRegistry = FileContentRegistry;
#[cfg(test)]
pub(crate) static FILE_CONTENT_LINKED: [&dyn OptionalToolsetRegistry; 1] = [&FILE_CONTENT_REGISTRY];

#[cfg(test)]
impl OptionalToolsetRegistry for FileContentRegistry {
    fn metadata(&self) -> OptionalToolsetMetadata {
        OptionalToolsetMetadata::new("files", false)
    }

    fn tools(&self) -> Result<Vec<OptionalRegistryTool>, SchemaContractError> {
        Ok(vec![
            OptionalRegistryTool::read(file_metadata_tool()?),
            OptionalRegistryTool::read(file_read_tool()?),
        ])
    }

    fn resource_templates(&self) -> Vec<ResourceTemplate> {
        vec![file_byte_resource_template()]
    }

    fn scripted_scenario_ids(&self) -> &'static [&'static str] {
        &[
            "file_content_direct_contract",
            "file_content_stdio_contract",
        ]
    }

    fn headless_scenario_ids(&self) -> &'static [&'static str] {
        &["file_content_real_headless"]
    }

    fn catalog_token_ceiling(&self) -> usize {
        2_600
    }

    fn call_tool<'a>(
        &'a self,
        request: CallToolRequestParams,
        runtime: &'a RuntimeContext,
        _cursors: &'a crate::cursor::CursorStore,
        protocol_version: &'a ProtocolVersion,
        cancellation: &'a CancellationToken,
    ) -> OptionalRegistryFuture<'a, Result<CallToolResult, ErrorData>> {
        Box::pin(async move {
            let handlers = FileContentHandlers::new(runtime.clone())
                .map_err(|_| ErrorData::internal_error("Files contracts unavailable.", None))?;
            match request.name.as_ref() {
                "file_metadata" => {
                    let input = decode_arguments::<FileMetadataInput>(request.arguments)?;
                    Ok(handlers.file_metadata(&input, cancellation).await)
                }
                "file_read" => {
                    let input = decode_arguments::<FileReadInput>(request.arguments)?;
                    Ok(handlers
                        .file_read(&input, protocol_version, cancellation)
                        .await)
                }
                _ => Err(ErrorData::method_not_found::<CallToolRequestMethod>()),
            }
        })
    }

    fn owns_resource_uri(&self, uri: &str) -> bool {
        uri.split_once(':')
            .is_some_and(|(scheme, _)| scheme.eq_ignore_ascii_case("anytype-file"))
    }

    fn owns_resource_template(&self, uri_template: &str) -> bool {
        uri_template == FILE_BYTE_RESOURCE_TEMPLATE
    }

    fn read_resource<'a>(
        &'a self,
        request: ReadResourceRequestParams,
        runtime: &'a RuntimeContext,
        cancellation: &'a CancellationToken,
    ) -> OptionalRegistryFuture<'a, Result<ReadResourceResult, ErrorData>> {
        Box::pin(async move {
            if !self.owns_resource_uri(&request.uri) {
                return Err(ErrorData::method_not_found::<ReadResourceRequestMethod>());
            }
            let handlers = FileContentHandlers::new(runtime.clone())
                .map_err(|_| ErrorData::internal_error("Files contracts unavailable.", None))?;
            handlers.read_resource(request, cancellation).await
        })
    }
}

#[cfg(test)]
fn decode_arguments<T: for<'de> Deserialize<'de>>(
    arguments: Option<rmcp::model::JsonObject>,
) -> Result<T, ErrorData> {
    serde_json::from_value(Value::Object(arguments.unwrap_or_default())).map_err(|_| {
        ErrorData::invalid_params(
            "Tool arguments do not match the declared schema.",
            Some(serde_json::json!({"code": "validation"})),
        )
    })
}

async fn exact_space_and_preflight(
    client: &anytype::prelude::AnytypeClient,
    space: &SpaceRef,
    file_id: &EntityId,
) -> Result<(SpaceId, EntityId), FileOperationError> {
    if !anytype::validation::looks_like_object_id(space.as_str()) {
        return Err(FileOperationError::Tool(ToolError::validation()));
    }
    let space_id = SpaceId::new(space.as_str())
        .map_err(|_| FileOperationError::Tool(ToolError::validation()))?;
    exact_preflight(client, &space_id, file_id).await?;
    Ok((space_id, file_id.clone()))
}

async fn exact_preflight(
    client: &anytype::prelude::AnytypeClient,
    space_id: &SpaceId,
    file_id: &EntityId,
) -> Result<(), FileOperationError> {
    let object = client
        .object(space_id.as_str(), file_id.as_str())
        .response_limit_bytes(MAX_OBJECT_PREFLIGHT_BYTES)
        .get()
        .await?;
    verify_object_identity(&object, space_id, file_id)
}

fn verify_object_identity(
    object: &Object,
    space_id: &SpaceId,
    file_id: &EntityId,
) -> Result<(), FileOperationError> {
    if object.id != file_id.as_str() || object.space_id != space_id.as_str() {
        return Err(FileOperationError::IdentityMismatch);
    }
    Ok(())
}

async fn metadata_flow(
    client: &anytype::prelude::AnytypeClient,
    space_id: SpaceId,
    file_id: EntityId,
) -> Result<FileMetadataOutput, FileOperationError> {
    let response = head_request(client, &space_id, &file_id).await?;
    metadata_output(space_id, file_id, &response)
}

async fn head_request(
    client: &anytype::prelude::AnytypeClient,
    space_id: &SpaceId,
    file_id: &EntityId,
) -> Result<FileContentResponse, FileOperationError> {
    let response = client
        .files()
        .download_request(space_id.as_str(), file_id.as_str())
        .response_limit_bytes(1)
        .error_limit_bytes(MAX_ERROR_BODY_BYTES)
        .header_evidence_limit_bytes(MAX_HEADER_EVIDENCE_BYTES)
        .max_attempts(MAX_SAFE_ATTEMPTS)
        .head()
        .await?;
    if response.status.as_u16() != 200 || !response.bytes.is_empty() {
        return Err(status_error(response.status.as_u16()));
    }
    Ok(response)
}

fn metadata_output(
    space_id: SpaceId,
    file_id: EntityId,
    response: &FileContentResponse,
) -> Result<FileMetadataOutput, FileOperationError> {
    let metadata = normalized_metadata(&response.metadata)?;
    Ok(FileMetadataOutput {
        file_id,
        space_id,
        media_type: metadata.media_type,
        size_bytes: metadata.size,
        accepts_byte_ranges: metadata.accepts_byte_ranges,
        strong_etag: metadata.strong_etag,
        last_modified: metadata.last_modified,
    })
}

#[derive(Debug, Clone)]
struct NormalizedMetadata {
    media_type: FileMediaType,
    size: JsonSafeInteger,
    accepts_byte_ranges: bool,
    strong_etag: Option<StrongEntityTag>,
    last_modified: Option<FileHttpDate>,
}

fn normalized_metadata(
    metadata: &FileHttpMetadata,
) -> Result<NormalizedMetadata, FileOperationError> {
    let size = metadata
        .content_length
        .ok_or_else(|| FileOperationError::Tool(ToolError::upstream()))
        .and_then(|value| {
            JsonSafeInteger::new(value)
                .map_err(|_| FileOperationError::Tool(ToolError::bounded_result()))
        })?;
    let accepts_byte_ranges = match metadata.accept_ranges.as_deref() {
        Some("bytes") => true,
        Some("none") | None => false,
        Some(_) => return Err(FileOperationError::Tool(ToolError::upstream())),
    };
    Ok(NormalizedMetadata {
        media_type: FileMediaType::from_evidence(metadata.content_type.as_deref())?,
        size,
        accepts_byte_ranges,
        strong_etag: StrongEntityTag::from_optional_evidence(metadata.etag.as_deref())?,
        last_modified: metadata
            .last_modified
            .as_ref()
            .map(|value| FileHttpDate::from_evidence(value.clone()))
            .transpose()?,
    })
}

#[derive(Debug, Clone)]
struct FileReadObservation {
    output: FileReadOutput,
    bytes: Vec<u8>,
}

async fn read_flow(
    client: &anytype::prelude::AnytypeClient,
    space_id: SpaceId,
    file_id: EntityId,
    offset: JsonSafeInteger,
    length: FileReadLength,
    expected: Option<StrongEntityTag>,
) -> Result<FileReadObservation, FileOperationError> {
    let head = head_request(client, &space_id, &file_id).await?;
    let head_metadata = normalized_metadata(&head.metadata)?;
    if let Some(expected) = expected.as_ref()
        && head_metadata.strong_etag.as_ref() != Some(expected)
    {
        return Err(FileOperationError::Tool(ToolError::conflict()));
    }

    let body_limit = length
        .get()
        .checked_add(1)
        .ok_or_else(|| FileOperationError::Tool(ToolError::bounded_result()))?;
    let mut request = client
        .files()
        .download_request(space_id.as_str(), file_id.as_str())
        .byte_range(offset.get(), length.get())
        .response_limit_bytes(body_limit)
        .error_limit_bytes(MAX_ERROR_BODY_BYTES)
        .header_evidence_limit_bytes(MAX_HEADER_EVIDENCE_BYTES)
        .max_attempts(MAX_SAFE_ATTEMPTS);
    if let Some(expected) = expected.as_ref() {
        request = request.if_range(expected.as_str());
    }
    let response = request.download().await?;
    read_observation(
        space_id,
        file_id,
        offset,
        length,
        expected.as_ref(),
        &head_metadata,
        response,
    )
}

#[allow(clippy::too_many_arguments)]
fn read_observation(
    space_id: SpaceId,
    file_id: EntityId,
    offset: JsonSafeInteger,
    length: FileReadLength,
    expected: Option<&StrongEntityTag>,
    head: &NormalizedMetadata,
    response: FileContentResponse,
) -> Result<FileReadObservation, FileOperationError> {
    let status = response.status.as_u16();
    let returned = u64::try_from(response.bytes.len())
        .map_err(|_| FileOperationError::Tool(ToolError::bounded_result()))?;
    let total = match status {
        206 => {
            if returned > length.get() {
                return Err(FileOperationError::Tool(ToolError::bounded_result()));
            }
            if returned == 0 {
                return Err(FileOperationError::RepresentationChanged);
            }
            let range = response
                .metadata
                .content_range
                .as_deref()
                .and_then(parse_content_range)
                .ok_or(FileOperationError::RepresentationChanged)?;
            let expected_end = offset
                .get()
                .checked_add(returned)
                .and_then(|value| value.checked_sub(1))
                .ok_or_else(|| FileOperationError::Tool(ToolError::bounded_result()))?;
            if range.start != offset.get()
                || range.end != expected_end
                || range.total != head.size.get()
            {
                return Err(FileOperationError::RepresentationChanged);
            }
            range.total
        }
        200 if expected.is_none() && offset.get() == 0 => {
            if returned > length.get() {
                return Err(FileOperationError::Tool(ToolError::bounded_result()));
            }
            if returned != head.size.get() {
                return Err(FileOperationError::RepresentationChanged);
            }
            returned
        }
        200 | 412 => return Err(FileOperationError::RepresentationChanged),
        416 => return Err(FileOperationError::Tool(ToolError::validation())),
        _ => return Err(status_error(status)),
    };

    let get = normalized_metadata(&response.metadata)?;
    if get.media_type != head.media_type || get.size.get() != returned {
        return Err(FileOperationError::RepresentationChanged);
    }
    let strong_etag = reconcile_validator(
        head.strong_etag.as_ref(),
        get.strong_etag.as_ref(),
        expected,
    )?;
    let last_modified = reconcile_date(head.last_modified.as_ref(), get.last_modified.as_ref())?;
    let total_bytes = JsonSafeInteger::new(total)
        .map_err(|_| FileOperationError::Tool(ToolError::bounded_result()))?;
    let returned_bytes = JsonSafeInteger::new(returned)
        .map_err(|_| FileOperationError::Tool(ToolError::bounded_result()))?;
    let complete = offset
        .get()
        .checked_add(returned)
        .is_some_and(|end| end == total);
    let sha256 = FileSha256::digest(&response.bytes);
    let resource_uri = FileResourceUri::new(&space_id, &file_id, offset, returned, &sha256)
        .map_err(|_| FileOperationError::Tool(ToolError::bounded_result()))?;
    Ok(FileReadObservation {
        output: FileReadOutput {
            file_id,
            space_id,
            media_type: get.media_type,
            offset,
            requested_bytes: JsonSafeInteger(length.get()),
            returned_bytes,
            total_bytes,
            complete,
            content_sha256: sha256,
            content_kind: FileContentKind::BlobResource,
            resource_uri,
            strong_etag,
            last_modified,
        },
        bytes: response.bytes.to_vec(),
    })
}

fn reconcile_validator(
    head: Option<&StrongEntityTag>,
    get: Option<&StrongEntityTag>,
    expected: Option<&StrongEntityTag>,
) -> Result<Option<StrongEntityTag>, FileOperationError> {
    if let Some(expected) = expected
        && (head != Some(expected) || get != Some(expected))
    {
        return Err(FileOperationError::RepresentationChanged);
    }
    match (head, get) {
        (Some(left), Some(right)) if left == right => Ok(Some(left.clone())),
        (Some(_), Some(_)) => Err(FileOperationError::RepresentationChanged),
        _ => Ok(None),
    }
}

fn reconcile_date(
    head: Option<&FileHttpDate>,
    get: Option<&FileHttpDate>,
) -> Result<Option<FileHttpDate>, FileOperationError> {
    match (head, get) {
        (Some(left), Some(right)) if left == right => Ok(Some(left.clone())),
        (Some(_), Some(_)) => Err(FileOperationError::RepresentationChanged),
        _ => Ok(None),
    }
}

fn encode_file_read(
    mut observation: FileReadObservation,
    protocol_version: &ProtocolVersion,
) -> Result<CallToolResult, FileOperationError> {
    let parsed = observation.output.media_type.parsed()?;
    if parsed.type_() == mime::IMAGE {
        observation.output.content_kind = FileContentKind::Image;
        let payload = ContentBlock::image(
            BASE64_STANDARD.encode(&observation.bytes),
            observation.output.media_type.as_str(),
        );
        return native_tool_result(&observation.output, payload);
    }
    if parsed.type_() == mime::AUDIO && protocol_version >= &ProtocolVersion::V_2025_03_26 {
        observation.output.content_kind = FileContentKind::Audio;
        let payload = ContentBlock::audio(
            BASE64_STANDARD.encode(&observation.bytes),
            observation.output.media_type.as_str(),
        );
        return native_tool_result(&observation.output, payload);
    }
    if eligible_text(&parsed, &observation.bytes) {
        let text = std::str::from_utf8(&observation.bytes)
            .map_err(|_| FileOperationError::Encoding)?
            .to_owned();
        observation.output.content_kind = FileContentKind::TextResource;
        let payload = ContentBlock::resource(
            ResourceContents::text(text, observation.output.resource_uri.as_str())
                .with_mime_type(observation.output.media_type.as_str()),
        );
        let candidate = native_tool_result_unchecked(&observation.output, payload)?;
        if encoded_within_limit(&candidate, MAX_TEXT_FRAME_BYTES)? {
            return Ok(candidate);
        }
    }
    observation.output.content_kind = FileContentKind::BlobResource;
    let payload = ContentBlock::resource(
        ResourceContents::blob(
            BASE64_STANDARD.encode(&observation.bytes),
            observation.output.resource_uri.as_str(),
        )
        .with_mime_type(observation.output.media_type.as_str()),
    );
    native_tool_result(&observation.output, payload)
}

fn native_tool_result(
    output: &FileReadOutput,
    payload: ContentBlock,
) -> Result<CallToolResult, FileOperationError> {
    let result = native_tool_result_unchecked(output, payload)?;
    if !encoded_within_limit(&result, MAX_FILE_RESULT_FRAME_BYTES)? {
        return Err(FileOperationError::Tool(ToolError::bounded_result()));
    }
    Ok(result)
}

fn native_tool_result_unchecked(
    output: &FileReadOutput,
    payload: ContentBlock,
) -> Result<CallToolResult, FileOperationError> {
    let structured = serde_json::to_value(output).map_err(|_| FileOperationError::Encoding)?;
    let mut result =
        CallToolResult::success(vec![ContentBlock::text(structured.to_string()), payload]);
    result.structured_content = Some(structured);
    Ok(result)
}

fn eligible_text(media_type: &mime::Mime, bytes: &[u8]) -> bool {
    if media_type.type_() != mime::TEXT {
        return false;
    }
    let charset = media_type
        .get_param(mime::CHARSET)
        .map(|value| value.as_str());
    let charset_supported = charset.is_none_or(|value| {
        value.eq_ignore_ascii_case("utf-8") || value.eq_ignore_ascii_case("us-ascii")
    });
    if !charset_supported || std::str::from_utf8(bytes).is_err() {
        return false;
    }
    !charset.is_some_and(|value| value.eq_ignore_ascii_case("us-ascii") && !bytes.is_ascii())
}

async fn resource_read_flow(
    client: &anytype::prelude::AnytypeClient,
    parsed: ParsedFileResourceUri,
) -> Result<FileReadObservation, FileOperationError> {
    exact_preflight(client, &parsed.space_id, &parsed.file_id).await?;
    let head = head_request(client, &parsed.space_id, &parsed.file_id).await?;
    let head_metadata = normalized_metadata(&head.metadata)?;
    let length = if parsed.length == 0 {
        if parsed.offset.get() != 0 || head_metadata.size.get() != 0 {
            return Err(FileOperationError::RepresentationChanged);
        }
        FileReadLength(1)
    } else {
        FileReadLength(parsed.length)
    };
    let response = if parsed.length == 0 {
        client
            .files()
            .download_request(parsed.space_id.as_str(), parsed.file_id.as_str())
            .response_limit_bytes(1)
            .error_limit_bytes(MAX_ERROR_BODY_BYTES)
            .header_evidence_limit_bytes(MAX_HEADER_EVIDENCE_BYTES)
            .max_attempts(MAX_SAFE_ATTEMPTS)
            .download()
            .await?
    } else {
        client
            .files()
            .download_request(parsed.space_id.as_str(), parsed.file_id.as_str())
            .byte_range(parsed.offset.get(), parsed.length)
            .response_limit_bytes(parsed.length.saturating_add(1))
            .error_limit_bytes(MAX_ERROR_BODY_BYTES)
            .header_evidence_limit_bytes(MAX_HEADER_EVIDENCE_BYTES)
            .max_attempts(MAX_SAFE_ATTEMPTS)
            .download()
            .await?
    };
    let mut observation = read_observation(
        parsed.space_id,
        parsed.file_id,
        parsed.offset,
        length,
        None,
        &head_metadata,
        response,
    )?;
    if observation.output.returned_bytes.get() != parsed.length
        || observation.output.content_sha256 != parsed.sha256
        || observation.output.resource_uri != parsed.uri
    {
        return Err(FileOperationError::RepresentationChanged);
    }
    observation.output.requested_bytes = JsonSafeInteger(parsed.length);
    Ok(observation)
}

fn encode_resource_read(observation: FileReadObservation) -> Result<ReadResourceResult, ErrorData> {
    let parsed = observation
        .output
        .media_type
        .parsed()
        .map_err(|_| ErrorData::internal_error(RESOURCE_UPSTREAM, None))?;
    if eligible_text(&parsed, &observation.bytes) {
        let text = std::str::from_utf8(&observation.bytes)
            .map_err(|_| ErrorData::internal_error(RESOURCE_UPSTREAM, None))?;
        let candidate = ReadResourceResult::new(vec![
            ResourceContents::text(text, observation.output.resource_uri.as_str())
                .with_mime_type(observation.output.media_type.as_str()),
        ]);
        if encoded_within_limit(&candidate, MAX_TEXT_FRAME_BYTES)
            .map_err(|_| ErrorData::internal_error(RESOURCE_UPSTREAM, None))?
        {
            return Ok(candidate);
        }
    }
    let result = ReadResourceResult::new(vec![
        ResourceContents::blob(
            BASE64_STANDARD.encode(&observation.bytes),
            observation.output.resource_uri.as_str(),
        )
        .with_mime_type(observation.output.media_type.as_str()),
    ]);
    if !encoded_within_limit(&result, MAX_FILE_RESULT_FRAME_BYTES)
        .map_err(|_| ErrorData::internal_error(RESOURCE_UPSTREAM, None))?
    {
        return Err(ErrorData::internal_error(RESOURCE_UPSTREAM, None));
    }
    Ok(result)
}

#[cfg(test)]
fn encoded_len(value: &impl Serialize) -> Result<usize, FileOperationError> {
    serde_json::to_vec(value)
        .map(|bytes| bytes.len())
        .map_err(|_| FileOperationError::Encoding)
}

fn encoded_within_limit(value: &impl Serialize, limit: usize) -> Result<bool, FileOperationError> {
    let mut sink = BoundedCountingSink::new(limit);
    match serde_json::to_writer(&mut sink, value) {
        Ok(()) => Ok(true),
        Err(_) if sink.exceeded => Ok(false),
        Err(_) => Err(FileOperationError::Encoding),
    }
}

#[derive(Debug)]
struct BoundedCountingSink {
    limit: usize,
    written: usize,
    exceeded: bool,
}

impl BoundedCountingSink {
    const fn new(limit: usize) -> Self {
        Self {
            limit,
            written: 0,
            exceeded: false,
        }
    }
}

impl io::Write for BoundedCountingSink {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let Some(next) = self.written.checked_add(bytes.len()) else {
            self.exceeded = true;
            return Err(io::Error::other("encoded frame exceeds bound"));
        };
        if next > self.limit {
            self.exceeded = true;
            return Err(io::Error::other("encoded frame exceeds bound"));
        }
        self.written = next;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[derive(Debug, Clone, Copy)]
struct ParsedContentRange {
    start: u64,
    end: u64,
    total: u64,
}

fn parse_content_range(value: &str) -> Option<ParsedContentRange> {
    let value = value.strip_prefix("bytes ")?;
    let (span, total) = value.split_once('/')?;
    let (start, end) = span.split_once('-')?;
    let start = parse_canonical_u64(start)?;
    let end = parse_canonical_u64(end)?;
    let total = parse_canonical_u64(total)?;
    (start <= end && end < total).then_some(ParsedContentRange { start, end, total })
}

fn parse_canonical_u64(value: &str) -> Option<u64> {
    if value.is_empty()
        || !value.bytes().all(|byte| byte.is_ascii_digit())
        || (value.len() > 1 && value.starts_with('0'))
    {
        return None;
    }
    value.parse().ok()
}

fn status_error(status: u16) -> FileOperationError {
    match status {
        401 | 403 => FileOperationError::Tool(ToolError::authentication()),
        404 | 410 => FileOperationError::Tool(ToolError::not_found()),
        412 => FileOperationError::Tool(ToolError::conflict()),
        416 => FileOperationError::Tool(ToolError::validation()),
        _ => FileOperationError::Tool(ToolError::upstream()),
    }
}

#[derive(Debug)]
enum FileOperationError {
    Upstream(AnytypeError),
    Tool(ToolError),
    IdentityMismatch,
    RepresentationChanged,
    Encoding,
}

impl From<AnytypeError> for FileOperationError {
    fn from(error: AnytypeError) -> Self {
        Self::Upstream(error)
    }
}

impl FileOperationError {
    fn diagnostic(&self) -> OperationFailureDiagnostic {
        match self {
            Self::Upstream(error) => OperationFailureDiagnostic::from_anytype(error),
            Self::Tool(_) => {
                OperationFailureDiagnostic::classified("workflow_error", "file_workflow")
            }
            Self::IdentityMismatch => {
                OperationFailureDiagnostic::classified("identity_error", "file_identity")
            }
            Self::RepresentationChanged => OperationFailureDiagnostic::classified(
                "representation_changed",
                "file_representation",
            ),
            Self::Encoding => {
                OperationFailureDiagnostic::classified("encoding_error", "file_encoding")
            }
        }
    }
}

fn controlled_tool_error(error: ControlledOperationError<FileOperationError>) -> ToolError {
    match error {
        ControlledOperationError::Operation(FileOperationError::Upstream(
            AnytypeError::InvalidFileResponseHeader {
                issue: "request-mismatch",
                ..
            },
        )) => ToolError::conflict(),
        ControlledOperationError::Operation(FileOperationError::Upstream(error)) => {
            match ToolError::from_anytype(&error) {
                AnytypeErrorMapping::Ready(error) => error,
                AnytypeErrorMapping::AmbiguityRequiresCandidates => ToolError::upstream(),
            }
        }
        ControlledOperationError::Operation(FileOperationError::Tool(error)) => error,
        ControlledOperationError::Operation(
            FileOperationError::IdentityMismatch | FileOperationError::Encoding,
        )
        | ControlledOperationError::Cancelled
        | ControlledOperationError::TimedOut
        | ControlledOperationError::ShuttingDown => ToolError::upstream(),
        ControlledOperationError::Operation(FileOperationError::RepresentationChanged) => {
            ToolError::conflict()
        }
    }
}

fn controlled_resource_error(error: ControlledOperationError<FileOperationError>) -> ErrorData {
    match error {
        ControlledOperationError::Operation(FileOperationError::RepresentationChanged)
        | ControlledOperationError::Operation(FileOperationError::IdentityMismatch) => {
            ErrorData::resource_not_found(MISSING_RESOURCE, None)
        }
        ControlledOperationError::Operation(FileOperationError::Tool(tool)) => match tool.code() {
            ToolErrorCode::NotFound | ToolErrorCode::Conflict | ToolErrorCode::Validation => {
                ErrorData::resource_not_found(MISSING_RESOURCE, None)
            }
            ToolErrorCode::Authentication
            | ToolErrorCode::Ambiguous
            | ToolErrorCode::BoundedResult
            | ToolErrorCode::Upstream => ErrorData::internal_error(RESOURCE_UPSTREAM, None),
        },
        ControlledOperationError::Operation(FileOperationError::Upstream(error)) => {
            if matches!(
                error,
                AnytypeError::InvalidFileResponseHeader {
                    issue: "request-mismatch",
                    ..
                }
            ) {
                return ErrorData::resource_not_found(MISSING_RESOURCE, None);
            }
            match ToolError::from_anytype(&error) {
                AnytypeErrorMapping::Ready(tool) if tool.code() == ToolErrorCode::NotFound => {
                    ErrorData::resource_not_found(MISSING_RESOURCE, None)
                }
                AnytypeErrorMapping::Ready(_)
                | AnytypeErrorMapping::AmbiguityRequiresCandidates => {
                    ErrorData::internal_error(RESOURCE_UPSTREAM, None)
                }
            }
        }
        ControlledOperationError::Operation(FileOperationError::Encoding)
        | ControlledOperationError::Cancelled
        | ControlledOperationError::TimedOut
        | ControlledOperationError::ShuttingDown => {
            ErrorData::internal_error(RESOURCE_UPSTREAM, None)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, time::Duration};

    use anytype::prelude::{AnytypeClient, ClientConfig, HttpCredentials};
    use rmcp::model::{ErrorCode, ResourceContents};
    use serde_json::{Map, json};
    use tiktoken_rs::o200k_base;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
        task::JoinHandle,
    };
    use tracing::instrument::WithSubscriber;

    use super::*;
    use crate::{
        config::ApplicationProfile, optional_toolsets::OptionalToolsetSelection,
        runtime::StartupStatus, server::AnyMcpServer,
    };

    const SPACE_ID: &str =
        "bafyreid5fvqlnsobih2keakcxjrrlpmly6kf37klzjzen4ibfdgalcdp4y.2tq5w93cr6oe7";
    const FILE_ID: &str = "bafyreie6n5l5nkbjal37su54cha4coy7qzuhrnajluzv5qd5jvtsrxkequ";
    const DATE: &str = "Wed, 22 Jul 2026 09:00:00 GMT";

    struct ScriptedReply {
        status: &'static str,
        headers: Vec<(&'static str, String)>,
        body: Vec<u8>,
    }

    impl ScriptedReply {
        fn object() -> Self {
            Self::object_identity(FILE_ID, SPACE_ID)
        }

        fn object_identity(file_id: &str, space_id: &str) -> Self {
            let body = json!({
                "object": {
                    "archived": false,
                    "id": file_id,
                    "layout": "basic",
                    "object": "object",
                    "properties": [],
                    "space_id": space_id,
                    "type": null
                }
            })
            .to_string()
            .into_bytes();
            Self {
                status: "200 OK",
                headers: vec![
                    ("Content-Type", "application/json".to_owned()),
                    ("Content-Length", body.len().to_string()),
                ],
                body,
            }
        }

        fn head(media_type: &str, length: usize) -> Self {
            Self {
                status: "200 OK",
                headers: file_headers(media_type, length, None),
                body: Vec::new(),
            }
        }

        fn partial(media_type: &str, offset: u64, total: u64, body: &[u8]) -> Self {
            let end = offset + body.len() as u64 - 1;
            Self {
                status: "206 Partial Content",
                headers: file_headers(
                    media_type,
                    body.len(),
                    Some(format!("bytes {offset}-{end}/{total}")),
                ),
                body: body.to_vec(),
            }
        }

        fn full(media_type: &str, body: &[u8]) -> Self {
            Self {
                status: "200 OK",
                headers: file_headers(media_type, body.len(), None),
                body: body.to_vec(),
            }
        }

        fn partial_with_range(media_type: &str, range: &str, body: &[u8]) -> Self {
            Self {
                status: "206 Partial Content",
                headers: file_headers(media_type, body.len(), Some(range.to_owned())),
                body: body.to_vec(),
            }
        }

        fn range_not_satisfiable(total: u64) -> Self {
            Self {
                status: "416 Range Not Satisfiable",
                headers: vec![
                    ("Content-Length", "0".to_owned()),
                    ("Content-Range", format!("bytes */{total}")),
                ],
                body: Vec::new(),
            }
        }

        fn control_with_body(status: &'static str, body: &[u8]) -> Self {
            Self {
                status,
                headers: vec![("Content-Length", body.len().to_string())],
                body: body.to_vec(),
            }
        }

        fn status(status: &'static str) -> Self {
            Self {
                status,
                headers: vec![("Content-Length", "0".to_owned())],
                body: Vec::new(),
            }
        }

        fn rate_limited() -> Self {
            Self {
                status: "429 Too Many Requests",
                headers: vec![
                    ("RateLimit-Reset", "0".to_owned()),
                    ("Content-Length", "0".to_owned()),
                ],
                body: Vec::new(),
            }
        }

        fn transport_close() -> Self {
            Self {
                status: "",
                headers: Vec::new(),
                body: Vec::new(),
            }
        }

        fn without_header(mut self, name: &str) -> Self {
            self.headers
                .retain(|(header, _)| !header.eq_ignore_ascii_case(name));
            self
        }
    }

    fn file_headers(
        media_type: &str,
        length: usize,
        content_range: Option<String>,
    ) -> Vec<(&'static str, String)> {
        let mut headers = vec![
            ("Content-Type", media_type.to_owned()),
            ("Content-Length", length.to_string()),
            ("Accept-Ranges", "bytes".to_owned()),
            ("ETag", "\"file-v1\"".to_owned()),
            ("Last-Modified", DATE.to_owned()),
        ];
        if let Some(content_range) = content_range {
            headers.push(("Content-Range", content_range));
        }
        headers
    }

    async fn scripted_http(replies: Vec<ScriptedReply>) -> (String, JoinHandle<Vec<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind scripted file endpoint");
        let address = listener.local_addr().expect("scripted endpoint address");
        let task = tokio::spawn(async move {
            let mut requests = Vec::with_capacity(replies.len());
            for reply in replies {
                let (mut socket, _) = listener.accept().await.expect("accept request");
                let mut request = Vec::new();
                let mut buffer = [0_u8; 4096];
                loop {
                    let read = socket.read(&mut buffer).await.expect("read request");
                    if read == 0 {
                        break;
                    }
                    request.extend_from_slice(&buffer[..read]);
                    if request.windows(4).any(|window| window == b"\r\n\r\n") {
                        break;
                    }
                }
                requests.push(String::from_utf8(request).expect("request headers"));
                if reply.status.is_empty() {
                    continue;
                }
                let mut response = format!("HTTP/1.1 {}\r\n", reply.status).into_bytes();
                for (name, value) in reply.headers {
                    response.extend_from_slice(name.as_bytes());
                    response.extend_from_slice(b": ");
                    response.extend_from_slice(value.as_bytes());
                    response.extend_from_slice(b"\r\n");
                }
                response.extend_from_slice(b"Connection: close\r\n\r\n");
                response.extend_from_slice(&reply.body);
                socket.write_all(&response).await.expect("write response");
            }
            requests
        });
        (format!("http://{address}"), task)
    }

    async fn scripted_http_then_hang(
        replies: Vec<ScriptedReply>,
    ) -> (
        String,
        std::sync::Arc<tokio::sync::Notify>,
        JoinHandle<Vec<String>>,
    ) {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind hanging file endpoint");
        let address = listener.local_addr().expect("hanging endpoint address");
        let started = std::sync::Arc::new(tokio::sync::Notify::new());
        let task_started = started.clone();
        let task = tokio::spawn(async move {
            let mut requests = Vec::with_capacity(replies.len() + 1);
            for reply in replies {
                let (mut socket, _) = listener.accept().await.expect("accept request");
                let request = read_request_headers(&mut socket).await;
                requests.push(request);
                if reply.status.is_empty() {
                    continue;
                }
                let mut response = format!("HTTP/1.1 {}\r\n", reply.status).into_bytes();
                for (name, value) in reply.headers {
                    response.extend_from_slice(name.as_bytes());
                    response.extend_from_slice(b": ");
                    response.extend_from_slice(value.as_bytes());
                    response.extend_from_slice(b"\r\n");
                }
                response.extend_from_slice(b"Connection: close\r\n\r\n");
                response.extend_from_slice(&reply.body);
                socket.write_all(&response).await.expect("write response");
            }
            let (mut socket, _) = listener.accept().await.expect("accept hanging request");
            requests.push(read_request_headers(&mut socket).await);
            task_started.notify_one();
            let mut byte = [0_u8; 1];
            let _ = socket.read(&mut byte).await;
            requests
        });
        (format!("http://{address}"), started, task)
    }

    async fn read_request_headers(socket: &mut tokio::net::TcpStream) -> String {
        let mut request = Vec::new();
        let mut buffer = [0_u8; 4096];
        loop {
            let read = socket.read(&mut buffer).await.expect("read request");
            if read == 0 {
                break;
            }
            request.extend_from_slice(&buffer[..read]);
            if request.windows(4).any(|window| window == b"\r\n\r\n") {
                break;
            }
        }
        String::from_utf8(request).expect("request headers")
    }

    fn client(base_url: String) -> AnytypeClient {
        let client = AnytypeClient::with_config(ClientConfig {
            base_url: Some(base_url),
            keystore: Some("env".to_owned()),
            keystore_service: Some("file-content-test".to_owned()),
            app_name: "file-content-test".to_owned(),
            ..ClientConfig::default()
        })
        .expect("scripted client");
        client.set_api_key(HttpCredentials::new("scripted-secret-token"));
        client
    }

    fn runtime(base_url: String) -> RuntimeContext {
        runtime_with_timeout(base_url, Duration::from_secs(3))
    }

    fn runtime_with_timeout(base_url: String, timeout: Duration) -> RuntimeContext {
        RuntimeContext::from_parts(
            client(base_url),
            1,
            timeout,
            StartupStatus {
                http_available: true,
                grpc_available: false,
            },
        )
    }

    fn selected_runtime(base_url: String) -> RuntimeContext {
        let selection = OptionalToolsetSelection::parse(
            Some("files".to_owned()),
            &[FILE_CONTENT_REGISTRY.metadata()],
        )
        .expect("files selection");
        RuntimeContext::from_parts_with_profile_and_optional_toolsets(
            client(base_url),
            1,
            Duration::from_secs(3),
            StartupStatus {
                http_available: true,
                grpc_available: false,
            },
            ApplicationProfile::Compact,
            false,
            selection,
        )
    }

    fn metadata_input() -> FileMetadataInput {
        serde_json::from_value(json!({"space": SPACE_ID, "file_id": FILE_ID}))
            .expect("metadata input")
    }

    fn read_input(length: usize) -> FileReadInput {
        serde_json::from_value(json!({
            "space": SPACE_ID,
            "file_id": FILE_ID,
            "offset": 0,
            "length": length,
            "expected_strong_etag": "\"file-v1\""
        }))
        .expect("read input")
    }

    fn observation(media_type: &str, bytes: &[u8]) -> FileReadObservation {
        let space_id = SpaceId::new(SPACE_ID).expect("space ID");
        let file_id = EntityId::new(FILE_ID).expect("file ID");
        let offset = JsonSafeInteger::new(0).expect("offset");
        let sha256 = FileSha256::digest(bytes);
        let uri = FileResourceUri::new(&space_id, &file_id, offset, bytes.len() as u64, &sha256)
            .expect("resource URI");
        FileReadObservation {
            output: FileReadOutput {
                file_id,
                space_id,
                media_type: FileMediaType::from_evidence(Some(media_type)).expect("MIME"),
                offset,
                requested_bytes: JsonSafeInteger::new(bytes.len() as u64).expect("requested"),
                returned_bytes: JsonSafeInteger::new(bytes.len() as u64).expect("returned"),
                total_bytes: JsonSafeInteger::new(bytes.len() as u64).expect("total"),
                complete: true,
                content_sha256: sha256,
                content_kind: FileContentKind::BlobResource,
                resource_uri: uri,
                strong_etag: Some(StrongEntityTag::new("\"file-v1\"").expect("ETag")),
                last_modified: Some(FileHttpDate::from_evidence(DATE).expect("date")),
            },
            bytes: bytes.to_vec(),
        }
    }

    fn maximum_observation(bytes: &[u8]) -> FileReadObservation {
        let space_id = SpaceId::new("s".repeat(256)).expect("maximum space ID");
        let file_id = EntityId::new("f".repeat(256)).expect("maximum file ID");
        let offset = JsonSafeInteger(JSON_SAFE_INTEGER_MAX - MAX_FILE_CONTENT_BYTES);
        let hash = FileSha256::digest(bytes);
        let uri = FileResourceUri::new(&space_id, &file_id, offset, MAX_FILE_CONTENT_BYTES, &hash)
            .expect("maximum-field URI");
        let media_type = format!("application/{}", "x".repeat(243));
        assert_eq!(media_type.len(), MAX_MEDIA_TYPE_BYTES);
        FileReadObservation {
            output: FileReadOutput {
                file_id,
                space_id,
                media_type: FileMediaType::from_evidence(Some(&media_type)).expect("maximum MIME"),
                offset,
                requested_bytes: JsonSafeInteger(MAX_FILE_CONTENT_BYTES),
                returned_bytes: JsonSafeInteger(MAX_FILE_CONTENT_BYTES),
                total_bytes: JsonSafeInteger(JSON_SAFE_INTEGER_MAX),
                complete: false,
                content_sha256: hash,
                content_kind: FileContentKind::BlobResource,
                resource_uri: uri,
                strong_etag: Some(
                    StrongEntityTag::new(format!("\"{}\"", "e".repeat(254))).expect("maximum ETag"),
                ),
                last_modified: Some(FileHttpDate::from_evidence(DATE).expect("date")),
            },
            bytes: bytes.to_vec(),
        }
    }

    fn content_kind(result: &CallToolResult) -> &str {
        result
            .structured_content
            .as_ref()
            .and_then(|value| value.get("content_kind"))
            .and_then(Value::as_str)
            .expect("content kind")
    }

    fn payload_resource(result: &CallToolResult) -> &ResourceContents {
        &result.content[1]
            .as_resource()
            .expect("embedded resource")
            .resource
    }

    #[test]
    fn domain_values_lock_exact_boundaries_and_canonical_resource_grammar() {
        assert!(SpaceRef::new(" ").is_err());
        assert_eq!(
            SpaceRef::new(" exact space ").expect("preserved").as_str(),
            " exact space "
        );
        assert!(SpaceRef::new("x".repeat(512)).is_ok());
        assert!(SpaceRef::new("x".repeat(513)).is_err());

        assert!(JsonSafeInteger::new(JSON_SAFE_INTEGER_MAX).is_ok());
        assert!(JsonSafeInteger::new(JSON_SAFE_INTEGER_MAX + 1).is_err());
        assert!(FileReadLength::new(1).is_ok());
        assert!(FileReadLength::new(MAX_FILE_CONTENT_BYTES).is_ok());
        assert!(FileReadLength::new(0).is_err());
        assert!(FileReadLength::new(MAX_FILE_CONTENT_BYTES + 1).is_err());

        let etag = format!("\"{}\"", "x".repeat(254));
        assert!(StrongEntityTag::new(etag).is_ok());
        assert!(StrongEntityTag::new(format!("\"{}\"", "x".repeat(255))).is_err());
        for invalid in ["W/\"weak\"", "\"bad quote\"", "\"line\nfeed\""] {
            assert!(StrongEntityTag::new(invalid).is_err(), "{invalid}");
        }

        let bytes = b"canonical";
        let space = SpaceId::new(SPACE_ID).expect("space");
        let file = EntityId::new(FILE_ID).expect("file");
        let hash = FileSha256::digest(bytes);
        let uri = FileResourceUri::new(
            &space,
            &file,
            JsonSafeInteger::new(7).expect("offset"),
            bytes.len() as u64,
            &hash,
        )
        .expect("URI");
        let parsed = FileResourceUri::parse(uri.as_str()).expect("round trip");
        assert_eq!(parsed.offset.get(), 7);
        assert_eq!(parsed.length, bytes.len() as u64);
        assert_eq!(parsed.sha256, hash);

        let invalid = [
            uri.as_str().replace("/7/", "/07/"),
            uri.as_str().replace("/9/", "/65537/"),
            format!("{}?query=1", uri.as_str()),
            format!("{}#fragment", uri.as_str()),
            uri.as_str()
                .replace("anytype-file://bytes", "anytype-file://user@bytes"),
            uri.as_str().replace(FILE_ID, "file%2Did"),
            format!("{}/", uri.as_str()),
            uri.as_str()
                .replace(hash.as_str(), &hash.as_str().to_uppercase()),
        ];
        for invalid in invalid {
            assert!(FileResourceUri::parse(&invalid).is_err(), "{invalid}");
        }
        let empty_hash = FileSha256::digest(&[]);
        assert!(FileResourceUri::new(&space, &file, JsonSafeInteger(0), 0, &empty_hash).is_ok());
        assert!(FileResourceUri::new(&space, &file, JsonSafeInteger(1), 0, &empty_hash).is_err());
    }

    #[test]
    fn contracts_are_closed_exact_and_within_reviewed_token_ceilings() {
        let metadata = file_metadata_tool().expect("metadata contract").into_tool();
        let read = file_read_tool().expect("read contract").into_tool();
        assert_eq!(metadata.name, "file_metadata");
        assert_eq!(read.name, "file_read");
        assert_eq!(metadata.input_schema["additionalProperties"], false);
        assert_eq!(read.input_schema["additionalProperties"], false);
        assert_eq!(
            read.input_schema["properties"]["length"]["$ref"],
            "#/$defs/FileReadLength"
        );
        assert_eq!(
            read.output_schema.as_ref().expect("output")["properties"]["content_kind"]["$ref"],
            "#/$defs/FileContentKind"
        );

        let template = file_byte_resource_template();
        assert_eq!(template.uri_template, FILE_BYTE_RESOURCE_TEMPLATE);
        assert_eq!(template.name, "anytype_file_bytes");
        assert!(template.mime_type.is_none());
        assert!(template.icons.is_none());
        assert!(template.meta.is_none());

        let tokenizer = o200k_base().expect("tokenizer");
        for tool in [&metadata, &read] {
            let wire = canonical_json(serde_json::to_value(tool).expect("tool JSON"));
            assert!(tokenizer.encode_ordinary(&wire.to_string()).len() <= 1_200);
        }
        let catalog = canonical_json(json!({
            "tools": [metadata, read],
            "resources": [],
            "resourceTemplates": [template]
        }));
        assert!(tokenizer.encode_ordinary(&catalog.to_string()).len() <= 2_600);
    }

    #[test]
    fn native_encoder_maps_mime_charset_revision_and_size_without_duplication() {
        let image_bytes = [0_u8, 1, 2, 3, 255];
        let image = encode_file_read(
            observation("image/png", &image_bytes),
            &ProtocolVersion::V_2025_11_25,
        )
        .expect("image result");
        assert_eq!(content_kind(&image), "image");
        assert!(image.content[1].as_image().is_some());
        assert_payload_once(&image, &BASE64_STANDARD.encode(image_bytes));

        let audio_bytes = b"audio";
        let old_audio = encode_file_read(
            observation("audio/wav", audio_bytes),
            &ProtocolVersion::V_2024_11_05,
        )
        .expect("old audio fallback");
        assert_eq!(content_kind(&old_audio), "blob_resource");
        assert!(matches!(
            payload_resource(&old_audio),
            ResourceContents::BlobResourceContents { .. }
        ));
        let new_audio = encode_file_read(
            observation("audio/wav", audio_bytes),
            &ProtocolVersion::V_2025_03_26,
        )
        .expect("native audio");
        assert_eq!(content_kind(&new_audio), "audio");
        assert!(new_audio.content[1].as_audio().is_some());

        let text = encode_file_read(
            observation("text/plain; charset=UTF-8", b"hello \"MCP\""),
            &ProtocolVersion::V_2025_11_25,
        )
        .expect("text result");
        assert_eq!(content_kind(&text), "text_resource");
        assert!(matches!(
            payload_resource(&text),
            ResourceContents::TextResourceContents { .. }
        ));
        assert!(
            !serde_json::to_string(text.structured_content.as_ref().expect("metadata"))
                .expect("metadata JSON")
                .contains("hello")
        );

        for (mime, bytes) in [
            ("text/plain; charset=iso-8859-1", b"plain".as_slice()),
            ("text/plain; charset=us-ascii", "é".as_bytes()),
            ("text/plain", [0xff_u8].as_slice()),
            ("application/x-unknown", b"unknown".as_slice()),
        ] {
            let result = encode_file_read(observation(mime, bytes), &ProtocolVersion::V_2025_11_25)
                .expect("blob fallback");
            assert_eq!(content_kind(&result), "blob_resource", "{mime}");
        }

        let escaped = encode_file_read(
            observation("text/plain", &vec![0_u8; MAX_FILE_CONTENT_BYTES as usize]),
            &ProtocolVersion::V_2025_11_25,
        )
        .expect("oversized escaped text falls back");
        assert_eq!(content_kind(&escaped), "blob_resource");
        assert!(encoded_len(&escaped).expect("encoded result") <= MAX_FILE_RESULT_FRAME_BYTES);
        let tokenizer = o200k_base().expect("tokenizer");
        let escaped_wire = serde_json::to_string(&escaped).expect("encoded result");
        assert!(tokenizer.encode_ordinary(&escaped_wire).len() <= 70_000);
    }

    #[test]
    fn bounded_counter_admits_exact_text_limits_without_allocating_encoded_frames() {
        let mut output = observation("text/plain", b"x").output;
        output.content_kind = FileContentKind::TextResource;
        let candidate = |text: String| {
            native_tool_result_unchecked(
                &output,
                ContentBlock::resource(
                    ResourceContents::text(text, output.resource_uri.as_str())
                        .with_mime_type(output.media_type.as_str()),
                ),
            )
            .expect("candidate")
        };
        let empty = candidate(String::new());
        let overhead = encoded_len(&empty).expect("empty frame");
        let exact = candidate("a".repeat(MAX_TEXT_FRAME_BYTES - overhead));
        let one_over = candidate("a".repeat(MAX_TEXT_FRAME_BYTES + 1 - overhead));
        assert_eq!(
            encoded_len(&exact).expect("exact frame"),
            MAX_TEXT_FRAME_BYTES
        );
        assert_eq!(
            encoded_len(&one_over).expect("one-over frame"),
            MAX_TEXT_FRAME_BYTES + 1
        );
        assert!(encoded_within_limit(&exact, MAX_TEXT_FRAME_BYTES).expect("exact admission"));
        assert!(
            !encoded_within_limit(&one_over, MAX_TEXT_FRAME_BYTES).expect("one-over rejection")
        );

        let resource_candidate = |text: String| {
            ReadResourceResult::new(vec![
                ResourceContents::text(text, output.resource_uri.as_str())
                    .with_mime_type(output.media_type.as_str()),
            ])
        };
        let empty = resource_candidate(String::new());
        let overhead = encoded_len(&empty).expect("empty resource frame");
        let exact = resource_candidate("a".repeat(MAX_TEXT_FRAME_BYTES - overhead));
        let one_over = resource_candidate("a".repeat(MAX_TEXT_FRAME_BYTES + 1 - overhead));
        assert_eq!(
            encoded_len(&exact).expect("exact resource"),
            MAX_TEXT_FRAME_BYTES
        );
        assert_eq!(
            encoded_len(&one_over).expect("one-over resource"),
            MAX_TEXT_FRAME_BYTES + 1
        );
        assert!(encoded_within_limit(&exact, MAX_TEXT_FRAME_BYTES).expect("exact resource"));
        assert!(!encoded_within_limit(&one_over, MAX_TEXT_FRAME_BYTES).expect("one-over resource"));
    }

    #[test]
    fn maximum_fields_and_adversarial_file_bytes_stay_within_frame_and_token_caps() {
        let bytes = (0..MAX_FILE_CONTENT_BYTES)
            .map(|index| ((index.wrapping_mul(73).wrapping_add(19)) & 0xff) as u8)
            .collect::<Vec<_>>();
        let space_id = SpaceId::new("s".repeat(256)).expect("maximum space ID");
        let file_id = EntityId::new("f".repeat(256)).expect("maximum file ID");
        let offset = JsonSafeInteger(JSON_SAFE_INTEGER_MAX - MAX_FILE_CONTENT_BYTES);
        let hash = FileSha256::digest(&bytes);
        let uri = FileResourceUri::new(&space_id, &file_id, offset, MAX_FILE_CONTENT_BYTES, &hash)
            .expect("maximum-field URI");
        let media_type = format!("application/{}", "x".repeat(243));
        assert_eq!(media_type.len(), MAX_MEDIA_TYPE_BYTES);
        let observation = FileReadObservation {
            output: FileReadOutput {
                file_id,
                space_id,
                media_type: FileMediaType::from_evidence(Some(&media_type)).expect("maximum MIME"),
                offset,
                requested_bytes: JsonSafeInteger(MAX_FILE_CONTENT_BYTES),
                returned_bytes: JsonSafeInteger(MAX_FILE_CONTENT_BYTES),
                total_bytes: JsonSafeInteger(JSON_SAFE_INTEGER_MAX),
                complete: false,
                content_sha256: hash,
                content_kind: FileContentKind::BlobResource,
                resource_uri: uri,
                strong_etag: Some(
                    StrongEntityTag::new(format!("\"{}\"", "e".repeat(254))).expect("maximum ETag"),
                ),
                last_modified: Some(FileHttpDate::from_evidence(DATE).expect("date")),
            },
            bytes,
        };
        let result = encode_file_read(observation, &ProtocolVersion::V_2025_11_25)
            .expect("maximum bounded result");
        assert_eq!(content_kind(&result), "blob_resource");
        let wire = serde_json::to_string(&result).expect("result JSON");
        assert!(wire.len() <= MAX_FILE_RESULT_FRAME_BYTES);
        assert!(
            o200k_base()
                .expect("tokenizer")
                .encode_ordinary(&wire)
                .len()
                <= 70_000
        );
    }

    #[test]
    fn every_max_field_adversarial_64k_pattern_round_trips_without_duplication() {
        let length = MAX_FILE_CONTENT_BYTES as usize;
        let mut random_state = 0x0A11_F17E_u32;
        let random = (0..length)
            .map(|_| {
                random_state ^= random_state << 13;
                random_state ^= random_state >> 17;
                random_state ^= random_state << 5;
                random_state as u8
            })
            .collect::<Vec<_>>();
        let patterns = [
            ("all-zero", vec![0_u8; length]),
            ("all-ff", vec![0xff_u8; length]),
            (
                "sequential",
                (0..length).map(|index| index as u8).collect::<Vec<_>>(),
            ),
            ("random-0x0A11F17E", random),
        ];
        let tokenizer = o200k_base().expect("tokenizer");
        for (name, bytes) in patterns {
            let expected_hash = FileSha256::digest(&bytes);
            let maximum = maximum_observation(&bytes);
            assert_eq!(maximum.output.file_id.as_str().len(), 256, "{name}");
            assert_eq!(maximum.output.space_id.as_str().len(), 256, "{name}");
            assert_eq!(maximum.output.media_type.as_str().len(), 255, "{name}");
            assert_eq!(
                maximum
                    .output
                    .strong_etag
                    .as_ref()
                    .expect("ETag")
                    .as_str()
                    .len(),
                256,
                "{name}"
            );
            assert!(maximum.output.resource_uri.as_str().len() <= MAX_FILE_RESOURCE_URI_BYTES);
            FileResourceUri::parse(maximum.output.resource_uri.as_str())
                .expect("maximum canonical URI");

            let tool = encode_file_read(maximum.clone(), &ProtocolVersion::V_2025_11_25)
                .expect("bounded tool result");
            assert_eq!(tool.content.len(), 2, "{name}");
            assert!(tool.content[0].as_text().is_some(), "{name}");
            let ResourceContents::BlobResourceContents {
                blob,
                uri,
                mime_type,
                ..
            } = payload_resource(&tool)
            else {
                panic!("{name}: expected blob tool payload")
            };
            assert_eq!(uri, maximum.output.resource_uri.as_str(), "{name}");
            assert_eq!(
                mime_type.as_deref(),
                Some(maximum.output.media_type.as_str()),
                "{name}"
            );
            let decoded = BASE64_STANDARD.decode(blob).expect("tool base64");
            assert_eq!(decoded, bytes, "{name}");
            assert_eq!(FileSha256::digest(&decoded), expected_hash, "{name}");
            let structured = tool.structured_content.as_ref().expect("metadata");
            assert_eq!(
                structured["content_sha256"],
                expected_hash.as_str(),
                "{name}"
            );
            assert!(!structured.to_string().contains(blob), "{name}");
            assert_eq!(
                tool.content[0].as_text().expect("metadata first").text,
                structured.to_string(),
                "{name}"
            );
            let tool_wire = serde_json::to_string(&tool).expect("tool JSON");
            assert_eq!(tool_wire.matches(blob).count(), 1, "{name}");
            assert!(tool_wire.len() <= MAX_FILE_RESULT_FRAME_BYTES, "{name}");
            assert!(
                tokenizer.encode_ordinary(&tool_wire).len() <= 70_000,
                "{name}"
            );

            let resource = encode_resource_read(maximum).expect("bounded resource result");
            assert_eq!(resource.contents.len(), 1, "{name}");
            let ResourceContents::BlobResourceContents {
                blob: resource_blob,
                uri: resource_uri,
                mime_type: resource_mime,
                ..
            } = &resource.contents[0]
            else {
                panic!("{name}: expected blob resource payload")
            };
            assert_eq!(resource_uri, uri, "{name}");
            assert_eq!(resource_mime, mime_type, "{name}");
            let resource_decoded = BASE64_STANDARD
                .decode(resource_blob)
                .expect("resource base64");
            assert_eq!(resource_decoded, bytes, "{name}");
            assert_eq!(
                FileSha256::digest(&resource_decoded),
                expected_hash,
                "{name}"
            );
            let resource_wire = serde_json::to_string(&resource).expect("resource JSON");
            assert_eq!(resource_wire.matches(resource_blob).count(), 1, "{name}");
            assert!(resource_wire.len() <= MAX_FILE_RESULT_FRAME_BYTES, "{name}");
            assert!(
                tokenizer.encode_ordinary(&resource_wire).len() <= 70_000,
                "{name}"
            );
        }
    }

    #[test]
    fn maximum_file_resource_fields_have_separate_frame_and_token_evidence() {
        let bytes = (0..MAX_FILE_CONTENT_BYTES)
            .map(|index| ((index.wrapping_mul(73).wrapping_add(19)) & 0xff) as u8)
            .collect::<Vec<_>>();
        let space_id = SpaceId::new("s".repeat(256)).expect("maximum space ID");
        let file_id = EntityId::new("f".repeat(256)).expect("maximum file ID");
        let offset = JsonSafeInteger(JSON_SAFE_INTEGER_MAX - MAX_FILE_CONTENT_BYTES);
        let hash = FileSha256::digest(&bytes);
        let uri = FileResourceUri::new(&space_id, &file_id, offset, MAX_FILE_CONTENT_BYTES, &hash)
            .expect("maximum-field URI");
        let media_type = format!("application/{}", "x".repeat(243));
        let resource = encode_resource_read(FileReadObservation {
            output: FileReadOutput {
                file_id,
                space_id,
                media_type: FileMediaType::from_evidence(Some(&media_type)).expect("maximum MIME"),
                offset,
                requested_bytes: JsonSafeInteger(MAX_FILE_CONTENT_BYTES),
                returned_bytes: JsonSafeInteger(MAX_FILE_CONTENT_BYTES),
                total_bytes: JsonSafeInteger(JSON_SAFE_INTEGER_MAX),
                complete: false,
                content_sha256: hash,
                content_kind: FileContentKind::BlobResource,
                resource_uri: uri,
                strong_etag: Some(
                    StrongEntityTag::new(format!("\"{}\"", "e".repeat(254))).expect("maximum ETag"),
                ),
                last_modified: Some(FileHttpDate::from_evidence(DATE).expect("date")),
            },
            bytes,
        })
        .expect("maximum resource result");
        let wire = serde_json::to_string(&resource).expect("resource JSON");
        assert!(wire.len() <= MAX_FILE_RESULT_FRAME_BYTES);
        assert!(
            o200k_base()
                .expect("tokenizer")
                .encode_ordinary(&wire)
                .len()
                <= 70_000
        );
    }

    #[test]
    fn actual_tool_and_resource_encoders_switch_at_exact_text_boundary() {
        fn tool_candidate_len(bytes: &[u8]) -> usize {
            let observation = observation("text/plain; charset=utf-8", bytes);
            let mut output = observation.output;
            output.content_kind = FileContentKind::TextResource;
            let text = String::from_utf8(observation.bytes).expect("UTF-8 fixture");
            let candidate = native_tool_result_unchecked(
                &output,
                ContentBlock::resource(
                    ResourceContents::text(text, output.resource_uri.as_str())
                        .with_mime_type(output.media_type.as_str()),
                ),
            )
            .expect("tool candidate");
            encoded_len(&candidate).expect("tool frame")
        }

        fn resource_candidate_len(bytes: &[u8]) -> usize {
            let observation = observation("text/plain; charset=utf-8", bytes);
            let text = String::from_utf8(observation.bytes).expect("UTF-8 fixture");
            let candidate = ReadResourceResult::new(vec![
                ResourceContents::text(text, observation.output.resource_uri.as_str())
                    .with_mime_type(observation.output.media_type.as_str()),
            ]);
            encoded_len(&candidate).expect("resource frame")
        }

        fn exact_bytes(target: usize, measure: fn(&[u8]) -> usize) -> Vec<u8> {
            let mut bytes = vec![b'a'; 12_000];
            let baseline = measure(&bytes);
            let delta = target.checked_sub(baseline).expect("target above baseline");
            let escaped = delta / 5;
            let quoted = delta % 5;
            assert!(escaped + quoted <= bytes.len());
            bytes[..escaped].fill(0);
            bytes[escaped..escaped + quoted].fill(b'"');
            assert_eq!(measure(&bytes), target);
            bytes
        }

        let tool_exact = exact_bytes(MAX_TEXT_FRAME_BYTES, tool_candidate_len);
        let tool_over = exact_bytes(MAX_TEXT_FRAME_BYTES + 1, tool_candidate_len);
        let result = encode_file_read(
            observation("text/plain; charset=utf-8", &tool_exact),
            &ProtocolVersion::V_2025_11_25,
        )
        .expect("exact tool frame");
        assert_eq!(content_kind(&result), "text_resource");
        let result = encode_file_read(
            observation("text/plain; charset=utf-8", &tool_over),
            &ProtocolVersion::V_2025_11_25,
        )
        .expect("one-over tool frame");
        assert_eq!(content_kind(&result), "blob_resource");

        let resource_exact = exact_bytes(MAX_TEXT_FRAME_BYTES, resource_candidate_len);
        let resource_over = exact_bytes(MAX_TEXT_FRAME_BYTES + 1, resource_candidate_len);
        let result =
            encode_resource_read(observation("text/plain; charset=utf-8", &resource_exact))
                .expect("exact resource frame");
        assert!(matches!(
            &result.contents[0],
            ResourceContents::TextResourceContents { .. }
        ));
        let result = encode_resource_read(observation("text/plain; charset=utf-8", &resource_over))
            .expect("one-over resource frame");
        assert!(matches!(
            &result.contents[0],
            ResourceContents::BlobResourceContents { .. }
        ));
    }

    #[test]
    fn resource_error_mapping_distinguishes_truncation_from_changed_identity() {
        let tool = controlled_tool_error(ControlledOperationError::Operation(
            FileOperationError::Upstream(AnytypeError::InvalidFileResponseHeader {
                status: 206,
                header: "content-length",
                issue: "body-length-mismatch",
            }),
        ));
        assert_eq!(tool.code(), ToolErrorCode::Upstream);
        let resource = controlled_resource_error(ControlledOperationError::Operation(
            FileOperationError::Upstream(AnytypeError::InvalidFileResponseHeader {
                status: 206,
                header: "content-length",
                issue: "body-length-mismatch",
            }),
        ));
        assert_eq!(resource.code, ErrorCode::INTERNAL_ERROR);
        assert_eq!(resource.message, RESOURCE_UPSTREAM);
        assert!(resource.data.is_none());

        let changed = AnytypeError::InvalidFileResponseHeader {
            status: 206,
            header: "content-range",
            issue: "request-mismatch",
        };
        let resource = controlled_resource_error(ControlledOperationError::Operation(
            FileOperationError::Upstream(changed),
        ));
        assert_eq!(resource.code, ErrorCode::RESOURCE_NOT_FOUND);
        assert_eq!(resource.message, MISSING_RESOURCE);
        assert!(resource.data.is_none());
        let wire = serde_json::to_string(&resource).expect("error JSON");
        assert!(!wire.contains("content-range"));
        assert!(!wire.contains("request-mismatch"));
    }

    #[test]
    fn range_validator_and_status_classification_reject_ambiguous_evidence() {
        let range = parse_content_range("bytes 7-9/10").expect("canonical range");
        assert_eq!((range.start, range.end, range.total), (7, 9, 10));
        for invalid in [
            "bytes 07-9/10",
            "bytes 7-10/10",
            "bytes 9-7/10",
            "bytes */10",
            "items 7-9/10",
        ] {
            assert!(parse_content_range(invalid).is_none(), "{invalid}");
        }

        let current = StrongEntityTag::new("\"current\"").expect("ETag");
        let stale = StrongEntityTag::new("\"stale\"").expect("ETag");
        assert_eq!(
            reconcile_validator(Some(&current), Some(&current), Some(&current))
                .expect("matching validator"),
            Some(current.clone())
        );
        assert!(reconcile_validator(Some(&current), Some(&stale), None).is_err());
        assert!(reconcile_validator(Some(&current), None, Some(&current)).is_err());
        assert!(matches!(
            status_error(416),
            FileOperationError::Tool(ref error) if error.code() == ToolErrorCode::Validation
        ));
    }

    #[tokio::test]
    async fn metadata_handler_preflights_identity_and_normalizes_exact_headers() {
        let (base_url, endpoint) = scripted_http(vec![
            ScriptedReply::object(),
            ScriptedReply::head("text/plain; charset=utf-8", 4),
        ])
        .await;
        let handlers = FileContentHandlers::new(runtime(base_url)).expect("handlers");
        let result = handlers
            .file_metadata(&metadata_input(), &CancellationToken::new())
            .await;
        assert_eq!(result.is_error, Some(false));
        let output = result.structured_content.expect("structured metadata");
        assert_eq!(output["file_id"], FILE_ID);
        assert_eq!(output["space_id"], SPACE_ID);
        assert_eq!(output["media_type"], "text/plain; charset=utf-8");
        assert_eq!(output["size_bytes"], 4);
        assert_eq!(output["accepts_byte_ranges"], true);
        assert_eq!(output["strong_etag"], "\"file-v1\"");
        assert_eq!(output["last_modified"], DATE);

        let requests = endpoint.await.expect("scripted endpoint");
        assert_eq!(requests.len(), 2);
        assert!(requests[0].starts_with(&format!(
            "GET /v1/spaces/{SPACE_ID}/objects/{FILE_ID} HTTP/1.1\r\n"
        )));
        assert!(requests[1].starts_with(&format!(
            "HEAD /v1/spaces/{SPACE_ID}/files/{FILE_ID} HTTP/1.1\r\n"
        )));
    }

    #[tokio::test]
    async fn inactive_slice_rejects_names_and_bounds_preflight_and_header_evidence() {
        let name_input = serde_json::from_value(json!({
            "space": "human-readable-name",
            "file_id": FILE_ID
        }))
        .expect("name input");
        let handlers =
            FileContentHandlers::new(runtime("http://127.0.0.1:1".to_owned())).expect("handlers");
        let result = handlers
            .file_metadata(&name_input, &CancellationToken::new())
            .await;
        assert_eq!(result.is_error, Some(true));
        assert_eq!(
            result.structured_content.as_ref().expect("error")["code"],
            "validation"
        );

        let oversized = vec![b'x'; MAX_OBJECT_PREFLIGHT_BYTES as usize + 1];
        let (base_url, endpoint) = scripted_http(vec![ScriptedReply {
            status: "200 OK",
            headers: vec![
                ("Content-Type", "application/json".to_owned()),
                ("Content-Length", oversized.len().to_string()),
            ],
            body: oversized,
        }])
        .await;
        let handlers = FileContentHandlers::new(runtime(base_url)).expect("handlers");
        let result = handlers
            .file_metadata(&metadata_input(), &CancellationToken::new())
            .await;
        assert_eq!(result.is_error, Some(true));
        assert_eq!(
            result.structured_content.as_ref().expect("error")["code"],
            "bounded_result"
        );
        assert_eq!(endpoint.await.expect("endpoint").len(), 1);

        let mut oversized_head = ScriptedReply::head("application/octet-stream", 0);
        oversized_head.headers.push((
            "Cache-Control",
            "private,".repeat((MAX_HEADER_EVIDENCE_BYTES / 8 + 2) as usize),
        ));
        let (base_url, endpoint) =
            scripted_http(vec![ScriptedReply::object(), oversized_head]).await;
        let handlers = FileContentHandlers::new(runtime(base_url)).expect("handlers");
        let result = handlers
            .file_metadata(&metadata_input(), &CancellationToken::new())
            .await;
        assert_eq!(result.is_error, Some(true));
        assert_eq!(
            result.structured_content.as_ref().expect("error")["code"],
            "bounded_result"
        );
        assert_eq!(endpoint.await.expect("endpoint").len(), 2);
    }

    #[tokio::test]
    async fn preview_stdio_dispatch_returns_native_image_and_no_second_base64_copy() {
        let bytes = [0_u8, 1, 2, 3];
        let (base_url, endpoint) = scripted_http(vec![
            ScriptedReply::object(),
            ScriptedReply::head("image/png", bytes.len()),
            ScriptedReply::partial("image/png", 0, bytes.len() as u64, &bytes),
        ])
        .await;
        let server = AnyMcpServer::new_with_optional_registries(
            selected_runtime(base_url),
            &FILE_CONTENT_LINKED,
        )
        .expect("files test server");
        let params = json!({
            "name": "file_read",
            "arguments": {
                "space": SPACE_ID,
                "file_id": FILE_ID,
                "offset": 0,
                "length": bytes.len(),
                "expected_strong_etag": "\"file-v1\""
            },
            "_meta": {
                "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                "io.modelcontextprotocol/clientCapabilities": {}
            }
        })
        .as_object()
        .expect("params")
        .clone();
        let response = crate::stdio::dispatch_modern(
            &server,
            json!(7),
            "tools/call",
            params,
            &CancellationToken::new(),
        )
        .await;
        assert_eq!(response["result"]["content"][1]["type"], "image");
        assert_eq!(
            response["result"]["structuredContent"]["content_kind"],
            "image"
        );
        let encoded = BASE64_STANDARD.encode(bytes);
        assert_eq!(response.to_string().matches(&encoded).count(), 1);
        assert!(
            response["result"]["structuredContent"]
                .get("data")
                .is_none()
        );
        assert!(
            response["result"]["structuredContent"]
                .get("blob")
                .is_none()
        );
        let requests = endpoint.await.expect("endpoint");
        assert_eq!(requests.len(), 3);
        assert!(requests[2].contains("range: bytes=0-3\r\n"));
        assert!(requests[2].contains("if-range: \"file-v1\"\r\n"));
    }

    #[tokio::test]
    async fn preview_resource_read_is_private_zero_ttl_and_templates_are_public_cached() {
        let bytes = b"ab";
        let space = SpaceId::new(SPACE_ID).expect("space");
        let file = EntityId::new(FILE_ID).expect("file");
        let hash = FileSha256::digest(bytes);
        let uri = FileResourceUri::new(&space, &file, JsonSafeInteger(0), 2, &hash)
            .expect("resource URI");
        let (base_url, endpoint) = scripted_http(vec![
            ScriptedReply::object(),
            ScriptedReply::head("application/octet-stream", 2),
            ScriptedReply::partial("application/octet-stream", 0, 2, bytes),
        ])
        .await;
        let server = AnyMcpServer::new_with_optional_registries(
            selected_runtime(base_url),
            &FILE_CONTENT_LINKED,
        )
        .expect("files test server");
        let templates = crate::stdio::dispatch_modern(
            &server,
            json!(31),
            "resources/templates/list",
            Map::new(),
            &CancellationToken::new(),
        )
        .await;
        assert_eq!(templates["result"]["cacheScope"], "public");
        assert!(
            templates["result"]["ttlMs"]
                .as_u64()
                .is_some_and(|ttl| ttl > 0)
        );
        assert_eq!(templates["result"]["resultType"], "complete");
        assert!(
            templates["result"]["resourceTemplates"]
                .as_array()
                .expect("templates")
                .iter()
                .any(|template| template["uriTemplate"] == FILE_BYTE_RESOURCE_TEMPLATE)
        );

        let params = json!({"uri": uri.as_str()})
            .as_object()
            .expect("resource params")
            .clone();
        let read = crate::stdio::dispatch_modern(
            &server,
            json!(32),
            "resources/read",
            params,
            &CancellationToken::new(),
        )
        .await;
        assert_eq!(read["result"]["cacheScope"], "private");
        assert_eq!(read["result"]["ttlMs"], 0);
        assert_eq!(read["result"]["resultType"], "complete");
        assert_eq!(
            read["result"]["contents"]
                .as_array()
                .expect("contents")
                .len(),
            1
        );
        assert_eq!(endpoint.await.expect("endpoint").len(), 3);
    }

    #[tokio::test]
    async fn stale_expected_validator_stops_before_get_with_conflict() {
        let (base_url, endpoint) = scripted_http(vec![
            ScriptedReply::object(),
            ScriptedReply::head("application/octet-stream", 4),
        ])
        .await;
        let handlers = FileContentHandlers::new(runtime(base_url)).expect("handlers");
        let mut input = read_input(4);
        input.expected_strong_etag = Omittable::Present(
            StrongEntityTag::new("\"stale\"").expect("stale expected validator"),
        );
        let result = handlers
            .file_read(
                &input,
                &ProtocolVersion::V_2025_11_25,
                &CancellationToken::new(),
            )
            .await;
        assert_eq!(result.is_error, Some(true));
        assert_eq!(
            result.structured_content.as_ref().expect("error")["code"],
            "conflict"
        );
        assert_eq!(endpoint.await.expect("endpoint").len(), 2);
    }

    #[tokio::test]
    async fn cumulative_http_metrics_lock_logical_retry_and_physical_attempt_ceilings() {
        let bytes = b"ab";
        let (base_url, endpoint) = scripted_http(vec![
            ScriptedReply::object(),
            ScriptedReply::rate_limited(),
            ScriptedReply::head("application/octet-stream", bytes.len()),
            ScriptedReply::partial("application/octet-stream", 0, bytes.len() as u64, bytes),
        ])
        .await;
        let handlers =
            FileContentHandlers::new(runtime_with_timeout(base_url, Duration::from_secs(100)))
                .expect("handlers");
        let result = handlers
            .file_read(
                &read_input(bytes.len()),
                &ProtocolVersion::V_2025_11_25,
                &CancellationToken::new(),
            )
            .await;
        assert_eq!(result.is_error, Some(false));
        let metrics = handlers.runtime.client().http_metrics();
        assert_eq!(metrics.logical_operations, 3);
        assert_eq!(metrics.physical_attempts, 4);
        assert_eq!(metrics.total_requests, 4);
        assert_eq!(metrics.retries, 1);
        assert_eq!(endpoint.await.expect("endpoint").len(), 4);

        let mut replies = vec![ScriptedReply::object()];
        replies.extend((0..MAX_SAFE_ATTEMPTS).map(|_| ScriptedReply::rate_limited()));
        let (base_url, endpoint) = scripted_http(replies).await;
        let handlers =
            FileContentHandlers::new(runtime_with_timeout(base_url, Duration::from_secs(100)))
                .expect("handlers");
        let result = handlers
            .file_metadata(&metadata_input(), &CancellationToken::new())
            .await;
        assert_eq!(result.is_error, Some(true));
        let metrics = handlers.runtime.client().http_metrics();
        assert_eq!(metrics.logical_operations, 2);
        assert_eq!(metrics.physical_attempts, u64::from(MAX_SAFE_ATTEMPTS) + 1);
        assert_eq!(metrics.total_requests, u64::from(MAX_SAFE_ATTEMPTS) + 1);
        assert_eq!(metrics.retries, u64::from(MAX_SAFE_ATTEMPTS - 1));
        assert_eq!(
            endpoint.await.expect("endpoint").len(),
            MAX_SAFE_ATTEMPTS as usize + 1
        );

        let mut replies = vec![
            ScriptedReply::object(),
            ScriptedReply::head("application/octet-stream", bytes.len()),
        ];
        replies.extend((0..MAX_SAFE_ATTEMPTS).map(|_| ScriptedReply::rate_limited()));
        let (base_url, endpoint) = scripted_http(replies).await;
        let handlers =
            FileContentHandlers::new(runtime_with_timeout(base_url, Duration::from_secs(100)))
                .expect("handlers");
        let result = handlers
            .file_read(
                &read_input(bytes.len()),
                &ProtocolVersion::V_2025_11_25,
                &CancellationToken::new(),
            )
            .await;
        assert_eq!(result.is_error, Some(true));
        let metrics = handlers.runtime.client().http_metrics();
        assert_eq!(metrics.logical_operations, 3);
        assert_eq!(metrics.physical_attempts, u64::from(MAX_SAFE_ATTEMPTS) + 2);
        assert_eq!(metrics.total_requests, u64::from(MAX_SAFE_ATTEMPTS) + 2);
        assert_eq!(metrics.retries, u64::from(MAX_SAFE_ATTEMPTS - 1));
        assert_eq!(
            endpoint.await.expect("endpoint").len(),
            MAX_SAFE_ATTEMPTS as usize + 2
        );

        let (base_url, endpoint) = scripted_http(vec![
            ScriptedReply::object(),
            ScriptedReply::head("application/octet-stream", bytes.len()),
            ScriptedReply::rate_limited(),
            ScriptedReply::status("504 Gateway Timeout"),
            ScriptedReply::rate_limited(),
            ScriptedReply::status("504 Gateway Timeout"),
            ScriptedReply::rate_limited(),
            ScriptedReply::transport_close(),
        ])
        .await;
        let handlers =
            FileContentHandlers::new(runtime_with_timeout(base_url, Duration::from_secs(100)))
                .expect("handlers");
        let result = handlers
            .file_read(
                &read_input(bytes.len()),
                &ProtocolVersion::V_2025_11_25,
                &CancellationToken::new(),
            )
            .await;
        assert_eq!(result.is_error, Some(true));
        let metrics = handlers.runtime.client().http_metrics();
        assert_eq!(metrics.logical_operations, 3);
        assert_eq!(metrics.physical_attempts, u64::from(MAX_SAFE_ATTEMPTS) + 2);
        assert_eq!(metrics.total_requests, u64::from(MAX_SAFE_ATTEMPTS) + 2);
        assert_eq!(metrics.retries, u64::from(MAX_SAFE_ATTEMPTS - 1));
        assert_eq!(endpoint.await.expect("endpoint").len(), 8);
    }

    #[tokio::test]
    async fn read_handler_locks_range_status_and_cross_response_evidence_matrix() {
        let cases = [
            (
                "bounded eof truncation",
                ScriptedReply::head("application/octet-stream", 2),
                ScriptedReply::partial("application/octet-stream", 0, 2, b"ab"),
                0,
                4,
                None,
            ),
            (
                "complete 200",
                ScriptedReply::head("application/octet-stream", 2),
                ScriptedReply::full("application/octet-stream", b"ab"),
                0,
                4,
                None,
            ),
        ];
        for (name, head, get, offset, length, expected_code) in cases {
            let (base_url, endpoint) =
                scripted_http(vec![ScriptedReply::object(), head, get]).await;
            let handlers = FileContentHandlers::new(runtime(base_url)).expect("handlers");
            let input = serde_json::from_value(json!({
                "space": SPACE_ID,
                "file_id": FILE_ID,
                "offset": offset,
                "length": length
            }))
            .expect("read input");
            let result = handlers
                .file_read(
                    &input,
                    &ProtocolVersion::V_2025_11_25,
                    &CancellationToken::new(),
                )
                .await;
            assert_eq!(
                result
                    .structured_content
                    .as_ref()
                    .and_then(|value| value.get("code"))
                    .and_then(Value::as_str),
                expected_code,
                "{name}"
            );
            assert_eq!(result.is_error, Some(expected_code.is_some()), "{name}");
            assert_eq!(endpoint.await.expect("endpoint").len(), 3, "{name}");
        }

        let failures = [
            (
                "truncated content range",
                ScriptedReply::head("application/octet-stream", 4),
                ScriptedReply::partial_with_range("application/octet-stream", "bytes 0-3/4", b"ab"),
                0,
                4,
                "upstream",
            ),
            (
                "range overrun",
                ScriptedReply::head("application/octet-stream", 5),
                ScriptedReply::partial("application/octet-stream", 0, 5, b"abcde"),
                0,
                4,
                "conflict",
            ),
            (
                "ignored nonzero range",
                ScriptedReply::head("application/octet-stream", 3),
                ScriptedReply::full("application/octet-stream", b"abc"),
                1,
                2,
                "conflict",
            ),
            (
                "contradictory total",
                ScriptedReply::head("application/octet-stream", 4),
                ScriptedReply::partial_with_range("application/octet-stream", "bytes 0-1/5", b"ab"),
                0,
                2,
                "conflict",
            ),
            (
                "MIME mismatch",
                ScriptedReply::head("text/plain", 2),
                ScriptedReply::partial("image/png", 0, 2, b"ab"),
                0,
                2,
                "conflict",
            ),
        ];
        for (name, head, get, offset, length, expected_code) in failures {
            let (base_url, endpoint) =
                scripted_http(vec![ScriptedReply::object(), head, get]).await;
            let handlers = FileContentHandlers::new(runtime(base_url)).expect("handlers");
            let input = serde_json::from_value(json!({
                "space": SPACE_ID,
                "file_id": FILE_ID,
                "offset": offset,
                "length": length
            }))
            .expect("read input");
            let result = handlers
                .file_read(
                    &input,
                    &ProtocolVersion::V_2025_11_25,
                    &CancellationToken::new(),
                )
                .await;
            assert_eq!(result.is_error, Some(true), "{name}");
            assert_eq!(
                result.structured_content.as_ref().expect("error")["code"],
                expected_code,
                "{name}"
            );
            assert_eq!(endpoint.await.expect("endpoint").len(), 3, "{name}");
        }

        let (base_url, endpoint) = scripted_http(vec![
            ScriptedReply::object(),
            ScriptedReply::head("application/octet-stream", 4),
            ScriptedReply::range_not_satisfiable(4),
        ])
        .await;
        let handlers = FileContentHandlers::new(runtime(base_url)).expect("handlers");
        let result = handlers
            .file_read(
                &read_input(4),
                &ProtocolVersion::V_2025_11_25,
                &CancellationToken::new(),
            )
            .await;
        assert_eq!(result.is_error, Some(true));
        assert_eq!(
            result.structured_content.as_ref().expect("error")["code"],
            "validation"
        );
        assert_eq!(endpoint.await.expect("endpoint").len(), 3);

        for (name, control, expected_code) in [
            (
                "oversized 416 sentinel",
                ScriptedReply::control_with_body("416 Range Not Satisfiable", b"abc"),
                "validation",
            ),
            (
                "oversized 412 sentinel",
                ScriptedReply::control_with_body("412 Precondition Failed", b"abc"),
                "conflict",
            ),
        ] {
            let (base_url, endpoint) = scripted_http(vec![
                ScriptedReply::object(),
                ScriptedReply::head("application/octet-stream", 4),
                control,
            ])
            .await;
            let handlers = FileContentHandlers::new(runtime(base_url)).expect("handlers");
            let result = handlers
                .file_read(
                    &read_input(2),
                    &ProtocolVersion::V_2025_11_25,
                    &CancellationToken::new(),
                )
                .await;
            assert_eq!(result.is_error, Some(true), "{name}");
            assert_eq!(result.content.len(), 1, "{name}");
            assert!(result.content[0].as_text().is_some(), "{name}");
            let structured = result.structured_content.as_ref().expect("error");
            assert_eq!(structured["code"], expected_code, "{name}");
            assert!(structured.get("content_kind").is_none(), "{name}");
            assert!(structured.get("resource_uri").is_none(), "{name}");
            assert_eq!(endpoint.await.expect("endpoint").len(), 3, "{name}");
        }

        let (base_url, endpoint) = scripted_http(vec![
            ScriptedReply::object(),
            ScriptedReply::head("application/octet-stream", 2),
            ScriptedReply::partial("application/octet-stream", 0, 2, b"ab")
                .without_header("etag")
                .without_header("last-modified"),
        ])
        .await;
        let handlers = FileContentHandlers::new(runtime(base_url)).expect("handlers");
        let input = serde_json::from_value(json!({
            "space": SPACE_ID,
            "file_id": FILE_ID,
            "offset": 0,
            "length": 2
        }))
        .expect("read input");
        let result = handlers
            .file_read(
                &input,
                &ProtocolVersion::V_2025_11_25,
                &CancellationToken::new(),
            )
            .await;
        assert_eq!(result.is_error, Some(false));
        let output = result.structured_content.expect("output");
        assert!(output.get("strong_etag").is_none());
        assert!(output.get("last_modified").is_none());
        assert_eq!(endpoint.await.expect("endpoint").len(), 3);
    }

    #[tokio::test]
    async fn canonical_resource_reader_returns_strict_text_and_rejects_changed_hash() {
        let bytes = b"hello";
        let space = SpaceId::new(SPACE_ID).expect("space");
        let file = EntityId::new(FILE_ID).expect("file");
        let hash = FileSha256::digest(bytes);
        let uri = FileResourceUri::new(&space, &file, JsonSafeInteger(0), 5, &hash)
            .expect("resource URI");
        let (base_url, endpoint) = scripted_http(vec![
            ScriptedReply::object(),
            ScriptedReply::head("text/plain; charset=us-ascii", bytes.len()),
            ScriptedReply::partial("text/plain; charset=us-ascii", 0, 5, bytes),
        ])
        .await;
        let handlers = FileContentHandlers::new(runtime(base_url)).expect("handlers");
        let result = handlers
            .read_resource(
                ReadResourceRequestParams::new(uri.as_str()),
                &CancellationToken::new(),
            )
            .await
            .expect("resource read");
        assert_eq!(result.contents.len(), 1);
        assert!(result.meta.is_none());
        match &result.contents[0] {
            ResourceContents::TextResourceContents {
                uri: returned_uri,
                mime_type,
                text,
                meta,
            } => {
                assert_eq!(returned_uri, uri.as_str());
                assert_eq!(mime_type.as_deref(), Some("text/plain; charset=us-ascii"));
                assert_eq!(text, "hello");
                assert!(meta.is_none());
            }
            ResourceContents::BlobResourceContents { .. } => panic!("unexpected blob"),
            _ => panic!("unexpected future resource content"),
        }
        assert_eq!(endpoint.await.expect("endpoint").len(), 3);

        let changed_hash = FileSha256::digest(b"other");
        let changed_uri = FileResourceUri::new(&space, &file, JsonSafeInteger(0), 5, &changed_hash)
            .expect("changed URI");
        let (base_url, endpoint) = scripted_http(vec![
            ScriptedReply::object(),
            ScriptedReply::head("text/plain", bytes.len()),
            ScriptedReply::partial("text/plain", 0, 5, bytes),
        ])
        .await;
        let handlers = FileContentHandlers::new(runtime(base_url)).expect("handlers");
        let error = handlers
            .read_resource(
                ReadResourceRequestParams::new(changed_uri.as_str()),
                &CancellationToken::new(),
            )
            .await
            .expect_err("changed hash");
        assert_eq!(error.code, ErrorCode::RESOURCE_NOT_FOUND);
        assert_eq!(error.message, MISSING_RESOURCE);
        assert!(error.data.is_none());
        assert_eq!(endpoint.await.expect("endpoint").len(), 3);
    }

    #[tokio::test]
    async fn resource_reader_maps_missing_changed_truncated_and_private_upstream_errors() {
        let bytes = b"ab";
        let space = SpaceId::new(SPACE_ID).expect("space");
        let file = EntityId::new(FILE_ID).expect("file");
        let hash = FileSha256::digest(bytes);
        let uri = FileResourceUri::new(&space, &file, JsonSafeInteger(0), 2, &hash)
            .expect("resource URI");

        for (name, reply, code, message) in [
            (
                "missing",
                ScriptedReply::status("404 Not Found"),
                ErrorCode::RESOURCE_NOT_FOUND,
                MISSING_RESOURCE,
            ),
            (
                "authentication",
                ScriptedReply::status("401 Unauthorized"),
                ErrorCode::INTERNAL_ERROR,
                RESOURCE_UPSTREAM,
            ),
        ] {
            let (base_url, endpoint) = scripted_http(vec![reply]).await;
            let handlers = FileContentHandlers::new(runtime(base_url)).expect("handlers");
            let error = handlers
                .read_resource(
                    ReadResourceRequestParams::new(uri.as_str()),
                    &CancellationToken::new(),
                )
                .await
                .expect_err(name);
            assert_eq!(error.code, code, "{name}");
            assert_eq!(error.message, message, "{name}");
            assert!(error.data.is_none(), "{name}");
            let wire = serde_json::to_string(&error).expect("error JSON");
            assert!(!wire.contains(FILE_ID), "{name}");
            assert!(!wire.contains(hash.as_str()), "{name}");
            assert_eq!(endpoint.await.expect("endpoint").len(), 1, "{name}");
        }

        let (base_url, endpoint) = scripted_http(vec![
            ScriptedReply::object(),
            ScriptedReply::head("application/octet-stream", 2),
            ScriptedReply::partial_with_range("application/octet-stream", "bytes 0-1/2", b"a"),
        ])
        .await;
        let handlers = FileContentHandlers::new(runtime(base_url)).expect("handlers");
        let error = handlers
            .read_resource(
                ReadResourceRequestParams::new(uri.as_str()),
                &CancellationToken::new(),
            )
            .await
            .expect_err("truncated response");
        assert_eq!(error.code, ErrorCode::INTERNAL_ERROR);
        assert_eq!(error.message, RESOURCE_UPSTREAM);
        assert!(error.data.is_none());
        assert_eq!(endpoint.await.expect("endpoint").len(), 3);

        let (base_url, endpoint) = scripted_http(vec![
            ScriptedReply::object(),
            ScriptedReply::head("application/octet-stream", 2),
            ScriptedReply::partial_with_range("application/octet-stream", "bytes 0-1/3", bytes),
        ])
        .await;
        let handlers = FileContentHandlers::new(runtime(base_url)).expect("handlers");
        let error = handlers
            .read_resource(
                ReadResourceRequestParams::new(uri.as_str()),
                &CancellationToken::new(),
            )
            .await
            .expect_err("changed total");
        assert_eq!(error.code, ErrorCode::RESOURCE_NOT_FOUND);
        assert_eq!(error.message, MISSING_RESOURCE);
        assert!(error.data.is_none());
        assert_eq!(endpoint.await.expect("endpoint").len(), 3);
    }

    #[tokio::test]
    async fn resource_reader_rejects_cross_identity_and_refreshes_current_mime() {
        let bytes = b"ab";
        let space = SpaceId::new(SPACE_ID).expect("space");
        let file = EntityId::new(FILE_ID).expect("file");
        let hash = FileSha256::digest(bytes);
        let uri = FileResourceUri::new(&space, &file, JsonSafeInteger(0), 2, &hash)
            .expect("resource URI");

        for (name, object) in [
            (
                "cross-object",
                ScriptedReply::object_identity(&format!("{FILE_ID}x"), SPACE_ID),
            ),
            (
                "cross-space",
                ScriptedReply::object_identity(FILE_ID, "different-space"),
            ),
        ] {
            let (base_url, endpoint) = scripted_http(vec![object]).await;
            let handlers = FileContentHandlers::new(runtime(base_url)).expect("handlers");
            let error = handlers
                .read_resource(
                    ReadResourceRequestParams::new(uri.as_str()),
                    &CancellationToken::new(),
                )
                .await
                .expect_err(name);
            assert_eq!(error.code, ErrorCode::RESOURCE_NOT_FOUND, "{name}");
            assert_eq!(error.message, MISSING_RESOURCE, "{name}");
            assert!(error.data.is_none(), "{name}");
            assert_eq!(endpoint.await.expect("endpoint").len(), 1, "{name}");
        }

        let (base_url, endpoint) = scripted_http(vec![
            ScriptedReply::object(),
            ScriptedReply::head("image/png", 2),
            ScriptedReply::partial("image/png", 0, 2, bytes),
        ])
        .await;
        let handlers = FileContentHandlers::new(runtime(base_url)).expect("handlers");
        let result = handlers
            .read_resource(
                ReadResourceRequestParams::new(uri.as_str()),
                &CancellationToken::new(),
            )
            .await
            .expect("current MIME refresh");
        match &result.contents[0] {
            ResourceContents::BlobResourceContents { mime_type, .. } => {
                assert_eq!(mime_type.as_deref(), Some("image/png"));
            }
            _ => panic!("non-text resource MIME must use a blob"),
        }
        assert_eq!(endpoint.await.expect("endpoint").len(), 3);

        let (base_url, endpoint) = scripted_http(vec![
            ScriptedReply::object(),
            ScriptedReply::head("image/png", 2),
            ScriptedReply::partial("text/plain", 0, 2, bytes),
        ])
        .await;
        let handlers = FileContentHandlers::new(runtime(base_url)).expect("handlers");
        let error = handlers
            .read_resource(
                ReadResourceRequestParams::new(uri.as_str()),
                &CancellationToken::new(),
            )
            .await
            .expect_err("MIME changed during read");
        assert_eq!(error.code, ErrorCode::RESOURCE_NOT_FOUND);
        assert_eq!(error.message, MISSING_RESOURCE);
        assert!(error.data.is_none());
        assert_eq!(endpoint.await.expect("endpoint").len(), 3);

        let mut oversized_head = ScriptedReply::head("application/octet-stream", 2);
        oversized_head.headers.push((
            "Cache-Control",
            "private,".repeat((MAX_HEADER_EVIDENCE_BYTES / 8 + 2) as usize),
        ));
        let (base_url, endpoint) =
            scripted_http(vec![ScriptedReply::object(), oversized_head]).await;
        let handlers = FileContentHandlers::new(runtime(base_url)).expect("handlers");
        let error = handlers
            .read_resource(
                ReadResourceRequestParams::new(uri.as_str()),
                &CancellationToken::new(),
            )
            .await
            .expect_err("bounded header evidence");
        assert_eq!(error.code, ErrorCode::INTERNAL_ERROR);
        assert_eq!(error.message, RESOURCE_UPSTREAM);
        assert!(error.data.is_none());
        assert_eq!(endpoint.await.expect("endpoint").len(), 2);
    }

    #[tokio::test]
    async fn resource_body_overrun_is_bounded_evidence_not_changed_identity() {
        let bytes = b"ab";
        let space = SpaceId::new(SPACE_ID).expect("space");
        let file = EntityId::new(FILE_ID).expect("file");
        let hash = FileSha256::digest(bytes);
        let uri = FileResourceUri::new(&space, &file, JsonSafeInteger(0), 2, &hash)
            .expect("resource URI");
        let (base_url, endpoint) = scripted_http(vec![
            ScriptedReply::object(),
            ScriptedReply::head("application/octet-stream", 3),
            ScriptedReply::full("application/octet-stream", b"abc"),
        ])
        .await;
        let handlers = FileContentHandlers::new(runtime(base_url)).expect("handlers");
        let error = handlers
            .read_resource(
                ReadResourceRequestParams::new(uri.as_str()),
                &CancellationToken::new(),
            )
            .await
            .expect_err("body overrun");
        assert_eq!(error.code, ErrorCode::INTERNAL_ERROR);
        assert_eq!(error.message, RESOURCE_UPSTREAM);
        assert!(error.data.is_none());
        assert_eq!(endpoint.await.expect("endpoint").len(), 3);
    }

    #[tokio::test]
    async fn resource_cancellation_and_timeout_are_bounded_private_errors() {
        let bytes = b"ab";
        let space = SpaceId::new(SPACE_ID).expect("space");
        let file = EntityId::new(FILE_ID).expect("file");
        let hash = FileSha256::digest(bytes);
        let uri = FileResourceUri::new(&space, &file, JsonSafeInteger(0), 2, &hash)
            .expect("resource URI");

        let (base_url, started, endpoint) =
            scripted_http_then_hang(vec![ScriptedReply::object()]).await;
        let handlers = FileContentHandlers::new(runtime(base_url)).expect("handlers");
        let cancellation = CancellationToken::new();
        let cancel = cancellation.clone();
        let request = ReadResourceRequestParams::new(uri.as_str());
        let (cancel_dispatch, cancel_output) =
            crate::logging::test_support::capture("any_mcp::operation=trace");
        let task = tokio::spawn(async move {
            handlers
                .read_resource(request, &cancellation)
                .with_subscriber(cancel_dispatch)
                .await
        });
        tokio::time::timeout(Duration::from_secs(1), started.notified())
            .await
            .expect("HEAD started");
        cancel.cancel();
        let error = task
            .await
            .expect("cancellation task")
            .expect_err("cancelled read");
        assert_eq!(error.code, ErrorCode::INTERNAL_ERROR);
        assert_eq!(error.message, RESOURCE_UPSTREAM);
        assert!(error.data.is_none());
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), endpoint)
                .await
                .expect("cancelled socket closes")
                .expect("endpoint")
                .len(),
            2
        );
        let cancel_diagnostics = cancel_output.contents();
        assert!(cancel_diagnostics.contains("outcome=\"cancelled\""));
        for private in [
            hash.as_str(),
            uri.as_str(),
            "scripted-secret-token",
            FILE_ID,
            SPACE_ID,
        ] {
            assert!(
                !cancel_diagnostics.contains(private),
                "cancel leaked {private}"
            );
        }

        let (base_url, started, endpoint) =
            scripted_http_then_hang(vec![ScriptedReply::object()]).await;
        let handlers =
            FileContentHandlers::new(runtime_with_timeout(base_url, Duration::from_millis(30)))
                .expect("handlers");
        let started_wait = started.notified();
        let timeout_cancellation = CancellationToken::new();
        let (timeout_dispatch, timeout_output) =
            crate::logging::test_support::capture("any_mcp::operation=trace");
        let result = handlers
            .read_resource(
                ReadResourceRequestParams::new(uri.as_str()),
                &timeout_cancellation,
            )
            .with_subscriber(timeout_dispatch);
        let ((), error) = tokio::join!(
            async {
                tokio::time::timeout(Duration::from_secs(1), started_wait)
                    .await
                    .expect("HEAD started");
            },
            async { result.await.expect_err("timed out read") }
        );
        assert_eq!(error.code, ErrorCode::INTERNAL_ERROR);
        assert_eq!(error.message, RESOURCE_UPSTREAM);
        assert!(error.data.is_none());
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), endpoint)
                .await
                .expect("timed-out socket closes")
                .expect("endpoint")
                .len(),
            2
        );
        let timeout_diagnostics = timeout_output.contents();
        assert!(timeout_diagnostics.contains("outcome=\"timeout\""));
        for private in [
            hash.as_str(),
            uri.as_str(),
            "scripted-secret-token",
            FILE_ID,
            SPACE_ID,
        ] {
            assert!(
                !timeout_diagnostics.contains(private),
                "timeout leaked {private}"
            );
        }
    }

    #[tokio::test]
    async fn file_operation_diagnostics_redact_ids_urls_headers_and_bodies() {
        let secret = "SECRET_FILE_RESPONSE_BODY_AND_CREDENTIAL";
        let body = json!({"message": secret}).to_string().into_bytes();
        let (base_url, endpoint) = scripted_http(vec![ScriptedReply {
            status: "400 Bad Request",
            headers: vec![
                ("Content-Type", "application/json".to_owned()),
                ("Content-Length", body.len().to_string()),
                ("Cache-Control", format!("private,{secret}")),
            ],
            body,
        }])
        .await;
        let handlers = FileContentHandlers::new(runtime(base_url)).expect("handlers");
        let (dispatch, output) = crate::logging::test_support::capture("any_mcp::operation=trace");
        let result = handlers
            .file_metadata(&metadata_input(), &CancellationToken::new())
            .with_subscriber(dispatch)
            .await;
        assert_eq!(result.is_error, Some(true));
        let diagnostics = output.contents();
        assert!(diagnostics.contains("operation=\"file_metadata\""));
        assert!(diagnostics.contains("upstream_http_status=400"));
        for private in [secret, FILE_ID, SPACE_ID, "cache-control", "127.0.0.1"] {
            assert!(!diagnostics.contains(private), "leaked {private}");
        }
        assert_eq!(endpoint.await.expect("endpoint").len(), 1);
    }

    #[tokio::test]
    async fn resource_header_failure_diagnostics_redact_every_seeded_field() {
        let bytes = b"ab";
        let space = SpaceId::new(SPACE_ID).expect("space");
        let file = EntityId::new(FILE_ID).expect("file");
        let hash = FileSha256::digest(bytes);
        let uri = FileResourceUri::new(&space, &file, JsonSafeInteger(0), 2, &hash)
            .expect("resource URI");
        let secret_mime = "application/x-secret-mime-seed";
        let secret_etag = "\"secret-etag-seed\"";
        let secret_token_cursor = "SECRET_TOKEN_CURSOR_SEED";
        let mut head = ScriptedReply::head(secret_mime, 2);
        for (name, value) in &mut head.headers {
            if name.eq_ignore_ascii_case("etag") {
                *value = secret_etag.to_owned();
            }
        }
        head.headers.push((
            "Cache-Control",
            secret_token_cursor.repeat((MAX_HEADER_EVIDENCE_BYTES as usize / 8) + 1),
        ));
        let (base_url, endpoint) = scripted_http(vec![ScriptedReply::object(), head]).await;
        let handlers = FileContentHandlers::new(runtime(base_url)).expect("handlers");
        let (dispatch, output) = crate::logging::test_support::capture("any_mcp::operation=trace");
        let error = handlers
            .read_resource(
                ReadResourceRequestParams::new(uri.as_str()),
                &CancellationToken::new(),
            )
            .with_subscriber(dispatch)
            .await
            .expect_err("bounded header evidence");
        assert_eq!(error.code, ErrorCode::INTERNAL_ERROR);
        assert_eq!(error.message, RESOURCE_UPSTREAM);
        assert!(error.data.is_none());
        let diagnostics = output.contents();
        assert!(diagnostics.contains("operation=\"file_resource_read\""));
        for private in [
            secret_mime,
            secret_etag,
            DATE,
            hash.as_str(),
            uri.as_str(),
            secret_token_cursor,
            "scripted-secret-token",
            FILE_ID,
            SPACE_ID,
        ] {
            assert!(!diagnostics.contains(private), "leaked {private}");
        }
        let error_wire = serde_json::to_string(&error).expect("error JSON");
        for private in [
            secret_mime,
            secret_etag,
            DATE,
            hash.as_str(),
            secret_token_cursor,
        ] {
            assert!(!error_wire.contains(private), "error leaked {private}");
        }
        assert_eq!(endpoint.await.expect("endpoint").len(), 2);
    }

    #[tokio::test]
    async fn malformed_resource_uri_fails_before_io_with_exact_stable_error() {
        let server = AnyMcpServer::new_with_optional_registries(
            selected_runtime("http://127.0.0.1:1".to_owned()),
            &FILE_CONTENT_LINKED,
        )
        .expect("files test server");
        let error = server
            .read_resource_wire(
                ReadResourceRequestParams::new("ANYTYPE-FILE://bytes/not-canonical"),
                &CancellationToken::new(),
            )
            .await
            .expect_err("invalid URI");
        assert_eq!(error.code, ErrorCode::INVALID_PARAMS);
        assert_eq!(error.message, INVALID_RESOURCE_URI);
        assert!(error.data.is_none());
    }

    #[tokio::test]
    async fn empty_resource_is_canonical_and_reads_exactly_zero_bytes() {
        let space = SpaceId::new(SPACE_ID).expect("space");
        let file = EntityId::new(FILE_ID).expect("file");
        let hash = FileSha256::digest(&[]);
        let uri = FileResourceUri::new(&space, &file, JsonSafeInteger(0), 0, &hash)
            .expect("empty resource URI");
        let (base_url, endpoint) = scripted_http(vec![
            ScriptedReply::object(),
            ScriptedReply::head("text/plain; charset=utf-8", 0),
            ScriptedReply::full("text/plain; charset=utf-8", &[]),
        ])
        .await;
        let handlers = FileContentHandlers::new(runtime(base_url)).expect("handlers");
        let result = handlers
            .read_resource(
                ReadResourceRequestParams::new(uri.as_str()),
                &CancellationToken::new(),
            )
            .await
            .expect("empty resource");
        match &result.contents[0] {
            ResourceContents::TextResourceContents {
                uri: returned_uri,
                text,
                ..
            } => {
                assert_eq!(returned_uri, uri.as_str());
                assert!(text.is_empty());
            }
            _ => panic!("empty UTF-8 text must remain native text"),
        }
        let requests = endpoint.await.expect("endpoint");
        assert_eq!(requests.len(), 3);
        assert!(!requests[2].contains("range:"));
    }

    fn assert_payload_once(result: &CallToolResult, payload: &str) {
        let wire = serde_json::to_string(result).expect("result JSON");
        assert_eq!(wire.matches(payload).count(), 1);
        let structured = serde_json::to_string(
            result
                .structured_content
                .as_ref()
                .expect("structured metadata"),
        )
        .expect("metadata JSON");
        assert!(!structured.contains(payload));
        assert_eq!(result.content.len(), 2);
        assert_eq!(
            result.content[0].as_text().expect("metadata text").text,
            result
                .structured_content
                .as_ref()
                .expect("structured metadata")
                .to_string()
        );
    }

    fn canonical_json(value: Value) -> Value {
        match value {
            Value::Object(object) => {
                let sorted = object
                    .into_iter()
                    .map(|(key, value)| (key, canonical_json(value)))
                    .collect::<BTreeMap<_, _>>();
                Value::Object(sorted.into_iter().collect::<Map<_, _>>())
            }
            Value::Array(values) => Value::Array(values.into_iter().map(canonical_json).collect()),
            scalar => scalar,
        }
    }
}
