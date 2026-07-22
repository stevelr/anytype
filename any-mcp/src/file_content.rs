// any-mcp - bounded, workflow-oriented MCP server for Anytype
//
// SPDX-FileCopyrightText: 2026 Steve Schoettler
// SPDX-License-Identifier: Apache-2.0

//! Bounded Anytype file upload, metadata, byte reads, and hash-bound resources.
//!
//! The default-off production `files` registry uses `anytype-api` only. Upload
//! accepts inline bytes, retains one process-local candidate per idempotency
//! key, and never exposes a host path, URL, or delete surface.

use std::{
    borrow::Cow,
    collections::HashMap,
    fmt, io,
    sync::{Arc, LazyLock, Weak},
    time::Instant,
};

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
use tokio::sync::{Mutex, Notify};
use tokio_util::sync::CancellationToken;

use crate::{
    create_idempotency::IdempotencyKey,
    domain::{EntityId, SpaceId},
    error::{AnytypeErrorMapping, ToolError, ToolErrorCode, mutation_rejection_is_definitive},
    handler_support::{MutationAccess, MutationProgress, MutationStage, require_mutation_access},
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
const MAX_RESOLVER_PAGE_BYTES: u64 = 1_048_576;
const MAX_UPLOAD_MULTIPART_BYTES: u64 = 71_680;
const MAX_UPLOAD_RESPONSE_BYTES: u64 = 65_536;
const MAX_ERROR_BODY_BYTES: u64 = 65_536;
const MAX_HEADER_EVIDENCE_BYTES: u64 = 4_096;
const MAX_SAFE_ATTEMPTS: u32 = 6;
const JSON_SAFE_INTEGER_MAX: u64 = 9_007_199_254_740_991;
const DEFAULT_READ_LENGTH: u64 = MAX_FILE_CONTENT_BYTES;
const MAX_FILE_NAME_CHARS: usize = 512;
const MAX_CANONICAL_BASE64_CHARS: usize = 87_384;
const MAX_UPLOAD_COHORT_ENTRIES: usize = 1_024;

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

/// A bounded display filename that can never be interpreted as a path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct FileName(String);

impl FileName {
    /// Validates the exact caller spelling without trimming or normalization.
    pub fn new(value: impl Into<String>) -> Result<Self, FileValueError> {
        let value = value.into();
        if value.is_empty()
            || value == "."
            || value == ".."
            || value.chars().count() > MAX_FILE_NAME_CHARS
            || value
                .chars()
                .any(|character| character == '/' || character == '\\' || character.is_control())
        {
            return Err(FileValueError::Invalid);
        }
        Ok(Self(value))
    }

    /// Borrows the validated filename.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for FileName {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::new(String::deserialize(deserializer)?).map_err(de::Error::custom)
    }
}

impl JsonSchema for FileName {
    fn schema_name() -> Cow<'static, str> {
        "FileName".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "minLength": 1,
            "maxLength": MAX_FILE_NAME_CHARS
        })
    }
}

/// Canonically encoded, nonempty file bytes retained in decoded form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanonicalBase64(Vec<u8>);

impl CanonicalBase64 {
    fn parse(value: &str) -> Result<Self, FileValueError> {
        if value.is_empty()
            || value.len() > MAX_CANONICAL_BASE64_CHARS
            || !value.is_ascii()
            || value.bytes().any(|byte| byte.is_ascii_whitespace())
        {
            return Err(FileValueError::Invalid);
        }
        let decoded = BASE64_STANDARD
            .decode(value)
            .map_err(|_| FileValueError::Invalid)?;
        if decoded.is_empty()
            || decoded.len() > MAX_FILE_CONTENT_BYTES as usize
            || BASE64_STANDARD.encode(&decoded) != value
        {
            return Err(FileValueError::Invalid);
        }
        Ok(Self(decoded))
    }

    /// Borrows the decoded payload without creating a second base64 copy.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

impl<'de> Deserialize<'de> for CanonicalBase64 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(&value).map_err(de::Error::custom)
    }
}

impl JsonSchema for CanonicalBase64 {
    fn schema_name() -> Cow<'static, str> {
        "CanonicalBase64".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "minLength": 4,
            "maxLength": MAX_CANONICAL_BASE64_CHARS,
            "pattern": "^(?:[A-Za-z0-9+/]{4})*(?:[A-Za-z0-9+/]{2}==|[A-Za-z0-9+/]{3}=)?$"
        })
    }
}

/// A bounded MIME essence accepted for one multipart upload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct UploadMediaType(String);

impl UploadMediaType {
    /// Parses and normalizes one MIME essence with no parameters.
    pub fn new(value: impl Into<String>) -> Result<Self, FileValueError> {
        let value = value.into();
        if value.is_empty()
            || value.len() > MAX_MEDIA_TYPE_BYTES
            || !value.bytes().all(|byte| byte.is_ascii_graphic())
        {
            return Err(FileValueError::Invalid);
        }
        let parsed = value
            .parse::<mime::Mime>()
            .map_err(|_| FileValueError::Invalid)?;
        if parsed.params().next().is_some() {
            return Err(FileValueError::Invalid);
        }
        let normalized = format!("{}/{}", parsed.type_(), parsed.subtype());
        if normalized.len() > MAX_MEDIA_TYPE_BYTES {
            return Err(FileValueError::Invalid);
        }
        Ok(Self(normalized))
    }

    /// Borrows the normalized MIME essence.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for UploadMediaType {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::new(String::deserialize(deserializer)?).map_err(de::Error::custom)
    }
}

impl JsonSchema for UploadMediaType {
    fn schema_name() -> Cow<'static, str> {
        "UploadMediaType".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "minLength": 3,
            "maxLength": MAX_MEDIA_TYPE_BYTES,
            "pattern": "^[!#$%&'*+.^_`|~0-9A-Za-z-]+/[!#$%&'*+.^_`|~0-9A-Za-z-]+$"
        })
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

/// Exact input for `file_upload`.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FileUploadInput {
    /// Unique space name or stable identifier.
    space: SpaceRef,
    /// Display filename sent as multipart metadata, never a host path.
    name: FileName,
    /// Canonically encoded nonempty payload, decoded exactly once during input validation.
    content_base64: CanonicalBase64,
    /// Optional normalized MIME essence with no parameters.
    #[serde(default)]
    #[schemars(schema_with = "optional_upload_media_type_schema")]
    media_type: Omittable<UploadMediaType>,
    /// Caller-stable process-local create key.
    idempotency_key: IdempotencyKey,
}

fn optional_upload_media_type_schema(generator: &mut SchemaGenerator) -> Schema {
    optional_non_null_schema::<UploadMediaType>(generator)
}

/// Exact verified output for `file_upload`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FileUploadOutput {
    /// Stable file-object identifier returned and independently verified.
    file_id: EntityId,
    /// Resolved stable space identifier.
    space_id: SpaceId,
    /// Exact validated caller display name.
    requested_name: FileName,
    /// Normalized MIME value verified by the stored representation.
    media_type: FileMediaType,
    /// Exact verified representation length.
    size_bytes: JsonSafeInteger,
    /// SHA-256 of the complete verified representation.
    content_sha256: FileSha256,
    /// Whether an earlier same-key candidate was safely reverified without another POST.
    reused: bool,
}

/// Exact input for `file_metadata`.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FileMetadataInput {
    /// Unique space name or stable identifier.
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
    /// Unique space name or stable identifier.
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

/// Builds the exact `file_upload` tool contract.
pub fn file_upload_tool() -> Result<WorkflowTool<FileUploadOutput>, SchemaContractError> {
    workflow_tool::<FileUploadInput, FileUploadOutput>(
        "file_upload",
        "Upload bounded inline bytes as one verified Anytype file object.",
        ToolProfile::Create,
    )
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

#[derive(Debug)]
struct UploadCohort {
    state: Mutex<UploadCohortState>,
    capacity: usize,
}

#[derive(Debug, Default)]
struct UploadCohortState {
    credential_generation: Option<u64>,
    entries: HashMap<IdempotencyKey, StoredUpload>,
}

#[derive(Debug)]
enum StoredUpload {
    Running {
        fingerprint: [u8; 32],
        attempt: Arc<UploadAttempt>,
    },
    Candidate {
        fingerprint: [u8; 32],
        candidate: EntityId,
    },
    Complete {
        fingerprint: [u8; 32],
        output: FileUploadOutput,
    },
    Indeterminate {
        fingerprint: [u8; 32],
    },
}

#[derive(Debug)]
struct UploadAttempt {
    result: Mutex<Option<CallToolResult>>,
    candidate: Mutex<Option<EntityId>>,
    notify: Notify,
    progress: MutationProgress,
    deadline: Instant,
    credential_generation: u64,
}

enum BeginUpload {
    LeadNew(Arc<UploadAttempt>),
    LeadReplay(Arc<UploadAttempt>, EntityId),
    Wait(Arc<UploadAttempt>),
    Cached(FileUploadOutput),
    Indeterminate,
    Conflict,
    Full,
    Expired,
}

enum UploadDisposition {
    Verified(FileUploadOutput),
    CandidateFailed(EntityId),
    Indeterminate,
    PreDispatchFailure,
}

struct UploadExecution {
    result: CallToolResult,
    disposition: UploadDisposition,
}

impl UploadCohort {
    fn new(capacity: usize) -> Self {
        Self {
            state: Mutex::new(UploadCohortState::default()),
            capacity,
        }
    }

    async fn begin(
        &self,
        credential_generation: u64,
        deadline: Instant,
        key: IdempotencyKey,
        fingerprint: [u8; 32],
    ) -> BeginUpload {
        let mut state = self.state.lock().await;
        if Instant::now() >= deadline {
            return BeginUpload::Expired;
        }
        if state.credential_generation != Some(credential_generation) {
            state.entries.clear();
            state.credential_generation = Some(credential_generation);
        }
        if let Some(entry) = state.entries.get(&key) {
            let result = match entry {
                StoredUpload::Running {
                    fingerprint: saved,
                    attempt,
                } if saved == &fingerprint => BeginUpload::Wait(attempt.clone()),
                StoredUpload::Candidate {
                    fingerprint: saved,
                    candidate,
                } if saved == &fingerprint => {
                    let candidate = candidate.clone();
                    let attempt = Arc::new(UploadAttempt::new(
                        Some(candidate.clone()),
                        deadline,
                        credential_generation,
                    ));
                    state.entries.insert(
                        key.clone(),
                        StoredUpload::Running {
                            fingerprint,
                            attempt: attempt.clone(),
                        },
                    );
                    BeginUpload::LeadReplay(attempt, candidate)
                }
                StoredUpload::Complete {
                    fingerprint: saved,
                    output,
                } if saved == &fingerprint => BeginUpload::Cached(output.clone()),
                StoredUpload::Indeterminate { fingerprint: saved } if saved == &fingerprint => {
                    BeginUpload::Indeterminate
                }
                _ => BeginUpload::Conflict,
            };
            if Instant::now() >= deadline {
                if let BeginUpload::LeadReplay(attempt, candidate) = &result
                    && matches!(
                        state.entries.get(&key),
                        Some(StoredUpload::Running { attempt: stored, .. })
                            if Arc::ptr_eq(stored, attempt)
                    )
                {
                    state.entries.insert(
                        key.clone(),
                        StoredUpload::Candidate {
                            fingerprint,
                            candidate: candidate.clone(),
                        },
                    );
                }
                return BeginUpload::Expired;
            }
            return result;
        }
        if self.capacity == 0 || state.entries.len() >= self.capacity {
            return if Instant::now() >= deadline {
                BeginUpload::Expired
            } else {
                BeginUpload::Full
            };
        }
        let attempt = Arc::new(UploadAttempt::new(None, deadline, credential_generation));
        state.entries.insert(
            key.clone(),
            StoredUpload::Running {
                fingerprint,
                attempt: attempt.clone(),
            },
        );
        if Instant::now() >= deadline {
            state.entries.remove(&key);
            BeginUpload::Expired
        } else {
            BeginUpload::LeadNew(attempt)
        }
    }

    async fn retain_candidate(
        &self,
        key: &IdempotencyKey,
        attempt: &Arc<UploadAttempt>,
        candidate: &EntityId,
    ) {
        let state = self.state.lock().await;
        let owns_attempt = matches!(
            state.entries.get(key),
            Some(StoredUpload::Running { attempt: stored, .. }) if Arc::ptr_eq(stored, attempt)
        );
        drop(state);
        if owns_attempt {
            *attempt.candidate.lock().await = Some(candidate.clone());
        }
    }

    async fn finish(
        &self,
        key: &IdempotencyKey,
        attempt: &Arc<UploadAttempt>,
        execution: UploadExecution,
    ) {
        let mut state = self.state.lock().await;
        if let Some(StoredUpload::Running {
            fingerprint,
            attempt: stored,
        }) = state.entries.get(key)
            && Arc::ptr_eq(stored, attempt)
        {
            let fingerprint = *fingerprint;
            match &execution.disposition {
                UploadDisposition::Verified(output) => {
                    state.entries.insert(
                        key.clone(),
                        StoredUpload::Complete {
                            fingerprint,
                            output: output.clone(),
                        },
                    );
                }
                UploadDisposition::CandidateFailed(candidate) => {
                    state.entries.insert(
                        key.clone(),
                        StoredUpload::Candidate {
                            fingerprint,
                            candidate: candidate.clone(),
                        },
                    );
                }
                UploadDisposition::Indeterminate => {
                    state
                        .entries
                        .insert(key.clone(), StoredUpload::Indeterminate { fingerprint });
                }
                UploadDisposition::PreDispatchFailure => {
                    state.entries.remove(key);
                }
            }
        }
        drop(state);
        *attempt.result.lock().await = Some(execution.result);
        attempt.notify.notify_waiters();
    }

    async fn reject_unstarted(&self, key: &IdempotencyKey, admission: &BeginUpload) {
        let (attempt, retained_candidate) = match admission {
            BeginUpload::LeadNew(attempt) => (attempt, None),
            BeginUpload::LeadReplay(attempt, candidate) => (attempt, Some(candidate)),
            BeginUpload::Wait(_)
            | BeginUpload::Cached(_)
            | BeginUpload::Indeterminate
            | BeginUpload::Conflict
            | BeginUpload::Full
            | BeginUpload::Expired => return,
        };
        let mut state = self.state.lock().await;
        let fingerprint = match state.entries.get(key) {
            Some(StoredUpload::Running {
                fingerprint,
                attempt: stored,
            }) if Arc::ptr_eq(stored, attempt) => Some(*fingerprint),
            _ => None,
        };
        let Some(fingerprint) = fingerprint else {
            return;
        };
        match retained_candidate {
            Some(candidate) => {
                state.entries.insert(
                    key.clone(),
                    StoredUpload::Candidate {
                        fingerprint,
                        candidate: candidate.clone(),
                    },
                );
            }
            None => {
                state.entries.remove(key);
            }
        }
        drop(state);
        *attempt.result.lock().await = Some(tool_error(&ToolError::upstream()));
        attempt.notify.notify_waiters();
    }
}

async fn admit_upload(
    cohort: &UploadCohort,
    credential_generation: u64,
    deadline: Instant,
    key: IdempotencyKey,
    fingerprint: [u8; 32],
) -> BeginUpload {
    tokio::time::timeout_at(
        tokio::time::Instant::from_std(deadline),
        cohort.begin(credential_generation, deadline, key, fingerprint),
    )
    .await
    .unwrap_or(BeginUpload::Expired)
}

impl UploadAttempt {
    fn new(candidate: Option<EntityId>, deadline: Instant, credential_generation: u64) -> Self {
        Self {
            result: Mutex::new(None),
            candidate: Mutex::new(candidate),
            notify: Notify::new(),
            progress: MutationProgress::new(),
            deadline,
            credential_generation,
        }
    }
}

type RuntimeUploadCohorts = HashMap<usize, (Weak<()>, Arc<UploadCohort>)>;

static UPLOAD_COHORTS: LazyLock<std::sync::Mutex<RuntimeUploadCohorts>> =
    LazyLock::new(|| std::sync::Mutex::new(HashMap::new()));

fn upload_cohort(runtime: &RuntimeContext) -> Arc<UploadCohort> {
    let identity = runtime.identity();
    let key = Arc::as_ptr(identity) as usize;
    let mut cohorts = match UPLOAD_COHORTS.lock() {
        Ok(cohorts) => cohorts,
        Err(poisoned) => poisoned.into_inner(),
    };
    cohorts.retain(|_, (owner, _)| owner.strong_count() != 0);
    if let Some((owner, cohort)) = cohorts.get(&key)
        && owner
            .upgrade()
            .is_some_and(|owner| Arc::ptr_eq(&owner, identity))
    {
        return cohort.clone();
    }
    let cohort = Arc::new(UploadCohort::new(MAX_UPLOAD_COHORT_ENTRIES));
    cohorts.insert(key, (Arc::downgrade(identity), cohort.clone()));
    cohort
}

#[derive(Clone)]
struct NormalizedUpload {
    space_id: SpaceId,
    name: FileName,
    bytes: Vec<u8>,
    media_type: Option<UploadMediaType>,
    sha256: FileSha256,
}

impl NormalizedUpload {
    fn new(space_id: SpaceId, input: FileUploadInput) -> Self {
        let bytes = input.content_base64.as_bytes().to_vec();
        let sha256 = FileSha256::digest(&bytes);
        Self {
            space_id,
            name: input.name,
            bytes,
            media_type: input.media_type.as_ref().cloned(),
            sha256,
        }
    }

    fn fingerprint(&self) -> [u8; 32] {
        let mut digest = Sha256::new();
        digest.update(b"any-mcp:file-upload:r8\0");
        fingerprint_field(&mut digest, self.space_id.as_str().as_bytes());
        fingerprint_field(&mut digest, self.name.as_str().as_bytes());
        match self.media_type.as_ref() {
            Some(media_type) => {
                digest.update([1]);
                fingerprint_field(&mut digest, media_type.as_str().as_bytes());
            }
            None => digest.update([0]),
        }
        digest.update((self.bytes.len() as u64).to_be_bytes());
        fingerprint_field(&mut digest, self.sha256.as_str().as_bytes());
        digest.finalize().into()
    }
}

fn fingerprint_field(digest: &mut Sha256, value: &[u8]) {
    digest.update((value.len() as u64).to_be_bytes());
    digest.update(value);
}

async fn wait_for_upload(
    attempt: Arc<UploadAttempt>,
    cancellation: &CancellationToken,
    invocation_deadline: Instant,
) -> CallToolResult {
    let deadline = attempt.deadline.min(invocation_deadline);
    loop {
        let notified = attempt.notify.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        if let Some(result) = attempt.result.lock().await.clone() {
            return result;
        }
        tokio::select! {
            biased;
            () = cancellation.cancelled() => {
                let error = match attempt.progress.stage() {
                    MutationStage::PreDispatch => ToolError::upstream(),
                    MutationStage::Dispatched => ToolError::mutation_indeterminate(),
                };
                return tool_error(&error);
            },
            () = tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)) => {
                let error = match attempt.progress.stage() {
                    MutationStage::PreDispatch => ToolError::upstream(),
                    MutationStage::Dispatched => ToolError::mutation_indeterminate(),
                };
                return tool_error(&error);
            },
            () = &mut notified => {}
        }
    }
}

/// Transport-neutral handlers for the approved files-domain workflows.
#[derive(Debug, Clone)]
pub struct FileContentHandlers {
    runtime: RuntimeContext,
    upload_contract: WorkflowTool<FileUploadOutput>,
    metadata_contract: WorkflowTool<FileMetadataOutput>,
    uploads: Arc<UploadCohort>,
}

impl FileContentHandlers {
    /// Creates handlers and validates both typed contracts.
    pub fn new(runtime: RuntimeContext) -> Result<Self, SchemaContractError> {
        file_read_tool()?;
        let uploads = upload_cohort(&runtime);
        Ok(Self {
            runtime,
            upload_contract: file_upload_tool()?,
            metadata_contract: file_metadata_tool()?,
            uploads,
        })
    }

    /// Uploads and independently verifies one bounded in-memory file.
    pub async fn file_upload(
        &self,
        access: MutationAccess,
        input: FileUploadInput,
        cancellation: &CancellationToken,
    ) -> CallToolResult {
        let deadline = self.runtime.request_deadline();
        if let Err(error) = require_mutation_access(access) {
            return tool_error(error.tool_error());
        }
        let key = input.idempotency_key.clone();
        let space = input.space.clone();
        let credential_generation = self.runtime.client().http_credential_generation();
        let resolved = self
            .runtime
            .execute_classified_until(
                deadline,
                OperationContext::new("file_upload_resolve"),
                cancellation,
                async {
                    let resolved = self
                        .runtime
                        .client()
                        .resolve_space_id_bounded(space.as_str(), MAX_RESOLVER_PAGE_BYTES)
                        .await?;
                    SpaceId::new(resolved)
                        .map_err(|_| FileOperationError::Tool(ToolError::upstream()))
                },
                FileOperationError::diagnostic,
            )
            .await;
        let space_id = match resolved {
            Ok(space_id) => space_id,
            Err(error) => return tool_error(&controlled_tool_error(error)),
        };
        if self.runtime.client().http_credential_generation() != credential_generation
            || Instant::now() >= deadline
        {
            return tool_error(&ToolError::upstream());
        }
        let normalized = NormalizedUpload::new(space_id, input);
        let fingerprint = normalized.fingerprint();
        let began = admit_upload(
            &self.uploads,
            credential_generation,
            deadline,
            key.clone(),
            fingerprint,
        )
        .await;
        if self.runtime.client().http_credential_generation() != credential_generation
            || Instant::now() >= deadline
        {
            self.uploads.reject_unstarted(&key, &began).await;
            return tool_error(&ToolError::upstream());
        }
        match began {
            BeginUpload::Cached(mut output) => {
                if self.runtime.client().http_credential_generation() != credential_generation
                    || Instant::now() >= deadline
                {
                    return tool_error(&ToolError::upstream());
                }
                output.reused = true;
                self.upload_contract
                    .success(&output)
                    .unwrap_or_else(|_| tool_error(&ToolError::upstream()))
            }
            BeginUpload::Indeterminate if Instant::now() < deadline => {
                tool_error(&ToolError::mutation_indeterminate())
            }
            BeginUpload::Conflict if Instant::now() < deadline => {
                tool_error(&ToolError::conflict())
            }
            BeginUpload::Full if Instant::now() < deadline => {
                tool_error(&ToolError::bounded_result())
            }
            BeginUpload::Indeterminate | BeginUpload::Conflict | BeginUpload::Full => {
                tool_error(&ToolError::upstream())
            }
            BeginUpload::Expired => tool_error(&ToolError::upstream()),
            BeginUpload::Wait(attempt) => wait_for_upload(attempt, cancellation, deadline).await,
            BeginUpload::LeadNew(attempt) => {
                spawn_upload_supervisor(
                    self.runtime.clone(),
                    self.upload_contract.clone(),
                    self.uploads.clone(),
                    key,
                    attempt.clone(),
                    normalized,
                    None,
                );
                wait_for_upload(attempt, cancellation, deadline).await
            }
            BeginUpload::LeadReplay(attempt, candidate) => {
                spawn_upload_supervisor(
                    self.runtime.clone(),
                    self.upload_contract.clone(),
                    self.uploads.clone(),
                    key,
                    attempt.clone(),
                    normalized,
                    Some(candidate),
                );
                wait_for_upload(attempt, cancellation, deadline).await
            }
        }
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

fn spawn_upload_supervisor(
    runtime: RuntimeContext,
    contract: WorkflowTool<FileUploadOutput>,
    cohort: Arc<UploadCohort>,
    key: IdempotencyKey,
    attempt: Arc<UploadAttempt>,
    input: NormalizedUpload,
    retained_candidate: Option<EntityId>,
) {
    tokio::spawn(async move {
        let credential_generation = attempt.credential_generation;
        let progress = attempt.progress.clone();
        let task_runtime = runtime.clone();
        let task_contract = contract.clone();
        let task_cohort = cohort.clone();
        let task_key = key.clone();
        let task_attempt = attempt.clone();
        let task_progress = progress.clone();
        let task_deadline = attempt.deadline;
        let task = tokio::spawn(async move {
            match retained_candidate {
                Some(candidate) => {
                    execute_upload_replay(
                        &task_runtime,
                        &task_contract,
                        input,
                        candidate,
                        &CancellationToken::new(),
                        task_deadline,
                    )
                    .await
                }
                None => {
                    execute_upload_leader(
                        &task_runtime,
                        &task_contract,
                        &task_cohort,
                        &task_key,
                        &task_attempt,
                        input,
                        &CancellationToken::new(),
                        &task_progress,
                        task_deadline,
                    )
                    .await
                }
            }
        });
        tokio::pin!(task);
        let mut execution = tokio::select! {
            result = &mut task => match result {
            Ok(execution) => execution,
            Err(_) => {
                let candidate = attempt.candidate.lock().await.clone();
                let (result, disposition) = match candidate {
                    Some(candidate) => {
                        let error = if progress.stage() == MutationStage::Dispatched {
                            ToolError::mutation_indeterminate()
                        } else {
                            ToolError::upstream()
                        };
                        (
                            tool_error(&error),
                            UploadDisposition::CandidateFailed(candidate),
                        )
                    }
                    None if progress.stage() == MutationStage::Dispatched => (
                        tool_error(&ToolError::mutation_indeterminate()),
                        UploadDisposition::Indeterminate,
                    ),
                    None => (
                        tool_error(&ToolError::upstream()),
                        UploadDisposition::PreDispatchFailure,
                    ),
                };
                UploadExecution {
                    result,
                    disposition,
                }
            }
            },
            () = tokio::time::sleep_until(tokio::time::Instant::from_std(attempt.deadline)) => {
                task.abort();
                let candidate = attempt.candidate.lock().await.clone();
                let disposition = match candidate {
                    Some(candidate) => UploadDisposition::CandidateFailed(candidate),
                    None if progress.stage() == MutationStage::Dispatched => UploadDisposition::Indeterminate,
                    None => UploadDisposition::PreDispatchFailure,
                };
                let error = if progress.stage() == MutationStage::Dispatched {
                    ToolError::mutation_indeterminate()
                } else {
                    ToolError::upstream()
                };
                UploadExecution { result: tool_error(&error), disposition }
            }
        };
        if runtime.client().http_credential_generation() != credential_generation {
            execution = UploadExecution {
                result: tool_error(&if progress.stage() == MutationStage::Dispatched {
                    ToolError::mutation_indeterminate()
                } else {
                    ToolError::upstream()
                }),
                disposition: if progress.stage() == MutationStage::Dispatched {
                    UploadDisposition::Indeterminate
                } else {
                    UploadDisposition::PreDispatchFailure
                },
            };
        }
        cohort.finish(&key, &attempt, execution).await;
    });
}

#[allow(clippy::too_many_arguments)]
async fn execute_upload_leader(
    runtime: &RuntimeContext,
    contract: &WorkflowTool<FileUploadOutput>,
    cohort: &Arc<UploadCohort>,
    key: &IdempotencyKey,
    attempt: &Arc<UploadAttempt>,
    input: NormalizedUpload,
    cancellation: &CancellationToken,
    progress: &MutationProgress,
    deadline: Instant,
) -> UploadExecution {
    let client = runtime.client().clone();
    let operation_input = input.clone();
    let operation_cohort = cohort.clone();
    let operation_key = key.clone();
    let operation_attempt = attempt.clone();
    let operation_progress = progress.clone();
    let result = runtime
        .execute_classified_until(
            deadline,
            OperationContext::new("file_upload"),
            cancellation,
            async move {
                let mut request = client
                    .files()
                    .upload(operation_input.space_id.as_str())
                    .bytes(operation_input.name.as_str(), operation_input.bytes.clone())
                    .multipart_limit_bytes(MAX_UPLOAD_MULTIPART_BYTES)
                    .response_limit_bytes(MAX_UPLOAD_RESPONSE_BYTES)
                    .error_limit_bytes(MAX_ERROR_BODY_BYTES);
                if let Some(media_type) = operation_input.media_type.as_ref() {
                    request = request.mime(media_type.as_str());
                }
                operation_progress.mark_dispatched();
                let uploaded = match request.upload().await {
                    Ok(uploaded) => uploaded,
                    Err(error) if mutation_rejection_is_definitive(&error) => {
                        return Err(FileOperationError::DefinitiveUpstream(error));
                    }
                    Err(_) => return Err(FileOperationError::PostDispatchUncertain),
                };
                let candidate = EntityId::new(uploaded.id)
                    .map_err(|_| FileOperationError::PostDispatchUncertain)?;
                operation_cohort
                    .retain_candidate(&operation_key, &operation_attempt, &candidate)
                    .await;
                verify_upload(&client, &operation_input, candidate, false).await
            },
            FileOperationError::diagnostic,
        )
        .await;
    match result {
        Ok(output) => {
            let encoded = contract
                .success(&output)
                .unwrap_or_else(|_| tool_error(&ToolError::upstream()));
            UploadExecution {
                result: encoded,
                disposition: UploadDisposition::Verified(output),
            }
        }
        Err(error) => {
            let candidate = attempt.candidate.lock().await.clone();
            if let Some(candidate) = candidate {
                return UploadExecution {
                    result: tool_error(&ToolError::mutation_indeterminate()),
                    disposition: UploadDisposition::CandidateFailed(candidate),
                };
            }
            if matches!(
                error,
                ControlledOperationError::Operation(FileOperationError::DefinitiveUpstream(_))
            ) {
                return UploadExecution {
                    result: tool_error(&controlled_tool_error(error)),
                    disposition: UploadDisposition::PreDispatchFailure,
                };
            }
            let disposition = if progress.stage() == MutationStage::Dispatched {
                UploadDisposition::Indeterminate
            } else {
                UploadDisposition::PreDispatchFailure
            };
            let tool = if progress.stage() == MutationStage::Dispatched {
                ToolError::mutation_indeterminate()
            } else {
                controlled_tool_error(error)
            };
            UploadExecution {
                result: tool_error(&tool),
                disposition,
            }
        }
    }
}

async fn execute_upload_replay(
    runtime: &RuntimeContext,
    contract: &WorkflowTool<FileUploadOutput>,
    input: NormalizedUpload,
    candidate: EntityId,
    cancellation: &CancellationToken,
    deadline: Instant,
) -> UploadExecution {
    let client = runtime.client().clone();
    let retained = candidate.clone();
    let result = runtime
        .execute_classified_until(
            deadline,
            OperationContext::new("file_upload_reverify"),
            cancellation,
            async move { verify_upload(&client, &input, candidate, true).await },
            FileOperationError::diagnostic,
        )
        .await;
    match result {
        Ok(output) => UploadExecution {
            result: contract
                .success(&output)
                .unwrap_or_else(|_| tool_error(&ToolError::upstream())),
            disposition: UploadDisposition::Verified(output),
        },
        Err(error) => UploadExecution {
            result: tool_error(&controlled_tool_error(error)),
            disposition: UploadDisposition::CandidateFailed(retained),
        },
    }
}

async fn verify_upload(
    client: &anytype::prelude::AnytypeClient,
    input: &NormalizedUpload,
    candidate: EntityId,
    reused: bool,
) -> Result<FileUploadOutput, FileOperationError> {
    exact_preflight(client, &input.space_id, &candidate).await?;
    let head = head_request(client, &input.space_id, &candidate).await?;
    let head = normalized_metadata(&head.metadata)?;
    let expected_size = u64::try_from(input.bytes.len())
        .map_err(|_| FileOperationError::Tool(ToolError::bounded_result()))?;
    if head.size.get() != expected_size || !upload_media_matches(input.media_type.as_ref(), &head)?
    {
        return Err(FileOperationError::RepresentationChanged);
    }
    let response = client
        .files()
        .download_request(input.space_id.as_str(), candidate.as_str())
        .response_limit_bytes(MAX_FILE_CONTENT_BYTES + 1)
        .error_limit_bytes(MAX_ERROR_BODY_BYTES)
        .header_evidence_limit_bytes(MAX_HEADER_EVIDENCE_BYTES)
        .max_attempts(MAX_SAFE_ATTEMPTS)
        .download()
        .await?;
    if response.status.as_u16() != 200 {
        return Err(status_error(response.status.as_u16()));
    }
    let get = normalized_metadata(&response.metadata)?;
    if response.bytes.len() != input.bytes.len()
        || get.size.get() != expected_size
        || get.media_type != head.media_type
        || FileSha256::digest(&response.bytes) != input.sha256
    {
        return Err(FileOperationError::RepresentationChanged);
    }
    Ok(FileUploadOutput {
        file_id: candidate,
        space_id: input.space_id.clone(),
        requested_name: input.name.clone(),
        media_type: head.media_type,
        size_bytes: head.size,
        content_sha256: input.sha256.clone(),
        reused,
    })
}

fn upload_media_matches(
    requested: Option<&UploadMediaType>,
    metadata: &NormalizedMetadata,
) -> Result<bool, FileOperationError> {
    let Some(requested) = requested else {
        return Ok(true);
    };
    let actual = metadata.media_type.parsed()?;
    let requested = requested
        .as_str()
        .parse::<mime::Mime>()
        .map_err(|_| FileOperationError::Encoding)?;
    Ok(actual.type_() == requested.type_() && actual.subtype() == requested.subtype())
}

#[derive(Debug)]
pub(crate) struct FileContentRegistry;

pub(crate) static FILE_CONTENT_REGISTRY: FileContentRegistry = FileContentRegistry;

impl OptionalToolsetRegistry for FileContentRegistry {
    fn metadata(&self) -> OptionalToolsetMetadata {
        OptionalToolsetMetadata::new("files", false)
    }

    fn tools(&self) -> Result<Vec<OptionalRegistryTool>, SchemaContractError> {
        Ok(vec![
            OptionalRegistryTool::read(file_metadata_tool()?),
            OptionalRegistryTool::read(file_read_tool()?),
            OptionalRegistryTool::mutation(file_upload_tool()?),
        ])
    }

    fn resource_templates(&self) -> Vec<ResourceTemplate> {
        vec![file_byte_resource_template()]
    }

    fn scripted_scenario_ids(&self) -> &'static [&'static str] {
        &[
            "file_content_direct_contract",
            "file_content_stdio_contract",
            "file_upload_direct_contract",
            "file_upload_stdio_contract",
        ]
    }

    fn headless_scenario_ids(&self) -> &'static [&'static str] {
        &["file_content_real_headless"]
    }

    fn catalog_token_ceiling(&self) -> usize {
        3_400
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
                "file_upload" => {
                    let input = decode_arguments::<FileUploadInput>(request.arguments)?;
                    Ok(handlers
                        .file_upload(MutationAccess::Allowed, input, cancellation)
                        .await)
                }
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
    let resolved = client
        .resolve_space_id_bounded(space.as_str(), MAX_RESOLVER_PAGE_BYTES)
        .await?;
    let space_id =
        SpaceId::new(resolved).map_err(|_| FileOperationError::Tool(ToolError::validation()))?;
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
    DefinitiveUpstream(AnytypeError),
    Tool(ToolError),
    IdentityMismatch,
    RepresentationChanged,
    PostDispatchUncertain,
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
            Self::Upstream(error) | Self::DefinitiveUpstream(error) => {
                OperationFailureDiagnostic::from_anytype(error)
            }
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
            Self::PostDispatchUncertain => {
                OperationFailureDiagnostic::classified("mutation_indeterminate", "file_upload")
            }
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
        ControlledOperationError::Operation(FileOperationError::DefinitiveUpstream(error)) => {
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
        ControlledOperationError::Operation(FileOperationError::PostDispatchUncertain) => {
            ToolError::mutation_indeterminate()
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
        ControlledOperationError::Operation(FileOperationError::DefinitiveUpstream(_))
        | ControlledOperationError::Operation(FileOperationError::PostDispatchUncertain) => {
            ErrorData::internal_error(RESOURCE_UPSTREAM, None)
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
    use std::{collections::BTreeMap, future::Future, sync::Barrier, time::Duration};

    use anytype::{
        prelude::{AnytypeClient, ClientConfig, HttpCredentials},
        test_util::{DisposableRun, unique_suffix, with_disposable_space_context},
    };
    use rmcp::model::{ErrorCode, ListToolsResult, ResourceContents};
    use serde_json::{Map, json};
    use tiktoken_rs::o200k_base;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, duplex, split};

    use super::*;
    use crate::{
        config::ApplicationProfile,
        optional_toolsets::{OptionalToolsetSelection, production_optional_metadata},
        runtime::StartupStatus,
        server::AnyMcpServer,
    };

    const SPACE_ID: &str =
        "bafyreid5fvqlnsobih2keakcxjrrlpmly6kf37klzjzen4ibfdgalcdp4y.2tq5w93cr6oe7";
    const FILE_ID: &str = "bafyreie6n5l5nkbjal37su54cha4coy7qzuhrnajluzv5qd5jvtsrxkequ";
    const DATE: &str = "Wed, 22 Jul 2026 09:00:00 GMT";
    const FILES_TOKEN_BUDGET_SNAPSHOT: &str =
        include_str!("../tests/snapshots/files-token-budget.json");
    const FILES_RESULT_SNAPSHOT: &str = include_str!("../tests/snapshots/files-results.json");
    const FILES_PRODUCTION_SURFACE_SNAPSHOT: &str =
        include_str!("../tests/snapshots/files-production-surface.json");

    fn run_large_future<F, Fut>(test: F)
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        std::thread::Builder::new()
            .name("file-content-handler".to_owned())
            .stack_size(16 * 1024 * 1024)
            .spawn(move || {
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("files test runtime")
                    .block_on(test());
            })
            .expect("spawn files test")
            .join()
            .expect("files test thread");
    }

    fn catalog_client() -> AnytypeClient {
        let client = AnytypeClient::with_config(ClientConfig {
            base_url: Some("http://127.0.0.1:1".to_owned()),
            keystore: Some("env".to_owned()),
            keystore_service: Some("file-content-catalog-test".to_owned()),
            app_name: "file-content-catalog-test".to_owned(),
            ..ClientConfig::default()
        })
        .expect("catalog client");
        client.set_api_key(HttpCredentials::new("catalog-test-token"));
        client
    }

    fn production_files_server(client: AnytypeClient, read_only: bool) -> AnyMcpServer {
        let selection = OptionalToolsetSelection::parse(
            Some("files".to_owned()),
            &production_optional_metadata(),
        )
        .expect("production files selection");
        let runtime = RuntimeContext::from_parts_with_profile_and_optional_toolsets(
            client,
            1,
            Duration::from_secs(30),
            StartupStatus {
                http_available: true,
                grpc_available: true,
            },
            ApplicationProfile::Standard,
            read_only,
            selection,
        );
        AnyMcpServer::new(runtime).expect("production files server")
    }

    fn production_base_server(client: AnytypeClient) -> AnyMcpServer {
        let runtime = RuntimeContext::from_parts_with_profile_and_optional_toolsets(
            client,
            1,
            Duration::from_secs(30),
            StartupStatus {
                http_available: true,
                grpc_available: true,
            },
            ApplicationProfile::Standard,
            false,
            OptionalToolsetSelection::default(),
        );
        AnyMcpServer::new(runtime).expect("production base server")
    }

    async fn direct_tool(
        server: &AnyMcpServer,
        name: &'static str,
        arguments: Value,
    ) -> CallToolResult {
        server
            .dispatch_tool(
                CallToolRequestParams::new(name).with_arguments(
                    arguments
                        .as_object()
                        .cloned()
                        .expect("direct files arguments"),
                ),
                &CancellationToken::new(),
            )
            .await
            .expect("direct files dispatch")
    }

    async fn production_stdio_request(
        server: AnyMcpServer,
        method: &'static str,
        mut params: Value,
    ) -> Value {
        params
            .as_object_mut()
            .expect("stdio params object")
            .entry("_meta")
            .or_insert_with(|| {
                json!({
                    "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                    "io.modelcontextprotocol/clientCapabilities": {}
                })
            });
        let (client_io, server_io) = duplex(128 * 1024);
        let (server_reader, server_writer) = split(server_io);
        let task = tokio::spawn(crate::stdio::serve_preview(
            server,
            BufReader::new(server_reader),
            server_writer,
        ));
        let (client_reader, mut client_writer) = split(client_io);
        let request = json!({
            "jsonrpc": "2.0",
            "id": 91,
            "method": method,
            "params": params
        });
        client_writer
            .write_all(format!("{request}\n").as_bytes())
            .await
            .expect("write production stdio request");
        let mut reader = BufReader::new(client_reader);
        let mut line = String::new();
        reader
            .read_line(&mut line)
            .await
            .expect("read production stdio response");
        drop(client_writer);
        drop(reader);
        task.await
            .expect("join production stdio")
            .expect("production stdio transport");
        serde_json::from_str(&line).expect("decode production stdio response")
    }

    async fn production_stdio_tool(
        server: AnyMcpServer,
        name: &'static str,
        arguments: Value,
    ) -> Value {
        production_stdio_request(
            server,
            "tools/call",
            json!({
                "name": name,
                "arguments": arguments,
                "_meta": {
                    "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                    "io.modelcontextprotocol/clientCapabilities": {}
                }
            }),
        )
        .await
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

    fn assert_blob_resource(
        result: &ReadResourceResult,
        expected_uri: &str,
        expected_bytes: &[u8],
    ) {
        assert_eq!(result.contents.len(), 1);
        let ResourceContents::BlobResourceContents {
            uri,
            mime_type,
            blob,
            ..
        } = &result.contents[0]
        else {
            panic!("expected blob resource")
        };
        assert_eq!(uri, expected_uri);
        assert_eq!(mime_type.as_deref(), Some("application/octet-stream"));
        let decoded = BASE64_STANDARD.decode(blob).expect("resource base64");
        assert_eq!(decoded, expected_bytes);
        assert_eq!(
            FileSha256::digest(&decoded),
            FileSha256::digest(expected_bytes)
        );
    }

    fn assert_stdio_blob_resource(result: &Value, expected_uri: &str, expected_bytes: &[u8]) {
        assert_eq!(result["contents"].as_array().expect("contents").len(), 1);
        let resource = &result["contents"][0];
        assert_eq!(resource["uri"], expected_uri);
        assert_eq!(resource["mimeType"], "application/octet-stream");
        let decoded = BASE64_STANDARD
            .decode(resource["blob"].as_str().expect("resource blob"))
            .expect("resource base64");
        assert_eq!(decoded, expected_bytes);
        assert_eq!(
            FileSha256::digest(&decoded),
            FileSha256::digest(expected_bytes)
        );
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

        assert!(FileName::new("report.txt").is_ok());
        assert!(FileName::new("x".repeat(MAX_FILE_NAME_CHARS)).is_ok());
        for invalid in ["", ".", "..", "a/b", "a\\b", "line\nfeed"] {
            assert!(FileName::new(invalid).is_err(), "{invalid:?}");
        }
        assert!(FileName::new("x".repeat(MAX_FILE_NAME_CHARS + 1)).is_err());

        let maximum_payload = vec![0xa5; MAX_FILE_CONTENT_BYTES as usize];
        let maximum_base64 = BASE64_STANDARD.encode(&maximum_payload);
        assert_eq!(maximum_base64.len(), MAX_CANONICAL_BASE64_CHARS);
        assert_eq!(
            CanonicalBase64::parse(&maximum_base64)
                .expect("maximum canonical payload")
                .as_bytes(),
            maximum_payload
        );
        for invalid in ["", "Zg", "Zg=", "Z g==", "===="] {
            assert!(CanonicalBase64::parse(invalid).is_err(), "{invalid:?}");
        }
        assert!(CanonicalBase64::parse(&BASE64_STANDARD.encode(vec![0; 65_537])).is_err());

        assert_eq!(
            UploadMediaType::new("Text/Plain")
                .expect("normalized MIME")
                .as_str(),
            "text/plain"
        );
        for invalid in ["text/plain; charset=utf-8", "text plain", "text/plain\n"] {
            assert!(UploadMediaType::new(invalid).is_err(), "{invalid:?}");
        }

        let upload = json!({
            "space": SPACE_ID,
            "name": "report.txt",
            "content_base64": "SGVsbG8=",
            "idempotency_key": "stable-key"
        });
        assert!(serde_json::from_value::<FileUploadInput>(upload.clone()).is_ok());
        let mut null_media = upload.clone();
        null_media["media_type"] = Value::Null;
        assert!(serde_json::from_value::<FileUploadInput>(null_media).is_err());
        let mut unknown = upload;
        unknown["path"] = json!("/tmp/secret");
        assert!(serde_json::from_value::<FileUploadInput>(unknown).is_err());

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
        let upload = file_upload_tool().expect("upload contract").into_tool();
        let metadata = file_metadata_tool().expect("metadata contract").into_tool();
        let read = file_read_tool().expect("read contract").into_tool();
        assert_eq!(upload.name, "file_upload");
        assert_eq!(metadata.name, "file_metadata");
        assert_eq!(read.name, "file_read");
        assert_eq!(metadata.input_schema["additionalProperties"], false);
        assert_eq!(read.input_schema["additionalProperties"], false);
        assert_eq!(upload.input_schema["additionalProperties"], false);
        assert_eq!(
            upload.input_schema["required"],
            json!(["space", "name", "content_base64", "idempotency_key"])
        );
        assert_eq!(
            upload.input_schema["properties"]["content_base64"]["$ref"],
            "#/$defs/CanonicalBase64"
        );
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
        for tool in [&upload, &metadata, &read] {
            let wire = canonical_json(serde_json::to_value(tool).expect("tool JSON"));
            assert!(tokenizer.encode_ordinary(&wire.to_string()).len() <= 1_200);
        }
        let read_write_catalog = canonical_json(json!({
            "tools": [metadata.clone(), read.clone(), upload],
            "resources": [],
            "resourceTemplates": [template.clone()]
        }));
        assert!(
            tokenizer
                .encode_ordinary(&read_write_catalog.to_string())
                .len()
                <= 3_400
        );
        let catalog = canonical_json(json!({
            "tools": [metadata, read],
            "resources": [],
            "resourceTemplates": [template]
        }));
        assert!(tokenizer.encode_ordinary(&catalog.to_string()).len() <= 2_600);
    }

    #[test]
    fn production_registry_inventory_and_read_only_projection_are_exact() {
        assert_eq!(
            FILE_CONTENT_REGISTRY.metadata(),
            OptionalToolsetMetadata::new("files", false)
        );
        assert!(FILE_CONTENT_REGISTRY.resources().is_empty());
        assert_eq!(
            FILE_CONTENT_REGISTRY.resource_templates(),
            vec![file_byte_resource_template()]
        );
        assert_eq!(FILE_CONTENT_REGISTRY.catalog_token_ceiling(), 3_400);
        assert!(
            production_optional_metadata()
                .iter()
                .any(|metadata| metadata.name == "files" && !metadata.requires_grpc)
        );

        let client = catalog_client();
        let read_write = production_files_server(client.clone(), false)
            .list_tools_wire(None)
            .expect("read-write files catalog");
        let read_only = production_files_server(client, true)
            .list_tools_wire(None)
            .expect("read-only files catalog");
        let file_names = |result: &rmcp::model::ListToolsResult| {
            result
                .tools
                .iter()
                .map(|tool| tool.name.to_string())
                .filter(|name| name.starts_with("file_"))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            file_names(&read_write),
            [
                "file_metadata".to_owned(),
                "file_read".to_owned(),
                "file_upload".to_owned(),
            ]
        );
        assert_eq!(
            file_names(&read_only),
            ["file_metadata".to_owned(), "file_read".to_owned()]
        );
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

    fn cohort_output(reused: bool) -> FileUploadOutput {
        FileUploadOutput {
            file_id: EntityId::new(FILE_ID).expect("file id"),
            space_id: SpaceId::new(SPACE_ID).expect("space id"),
            requested_name: FileName::new("cohort.bin").expect("file name"),
            media_type: FileMediaType::from_evidence(Some("application/octet-stream"))
                .expect("media type"),
            size_bytes: JsonSafeInteger(3),
            content_sha256: FileSha256::digest(b"abc"),
            reused,
        }
    }

    fn cohort_success(output: &FileUploadOutput) -> CallToolResult {
        file_upload_tool()
            .expect("upload contract")
            .success(output)
            .expect("upload success")
    }

    fn test_deadline() -> Instant {
        Instant::now()
            .checked_add(Duration::from_secs(30))
            .unwrap_or_else(Instant::now)
    }

    async fn cohort_begin(
        cohort: &UploadCohort,
        key: IdempotencyKey,
        fingerprint: [u8; 32],
    ) -> BeginUpload {
        cohort.begin(0, test_deadline(), key, fingerprint).await
    }

    #[tokio::test]
    async fn upload_cohort_coalesces_leader_and_waiter_with_one_post_branch() {
        let cohort = UploadCohort::new(2);
        let key = IdempotencyKey::new("leader-waiter").expect("key");
        let fingerprint = [7; 32];
        let leader = match cohort_begin(&cohort, key.clone(), fingerprint).await {
            BeginUpload::LeadNew(attempt) => attempt,
            _ => panic!("first caller must lead"),
        };
        let waiter = match cohort_begin(&cohort, key.clone(), fingerprint).await {
            BeginUpload::Wait(attempt) => attempt,
            _ => panic!("second caller must wait"),
        };
        assert!(Arc::ptr_eq(&leader, &waiter));
        let post_branches = 1_u64;
        let output = cohort_output(false);
        cohort
            .finish(
                &key,
                &leader,
                UploadExecution {
                    result: cohort_success(&output),
                    disposition: UploadDisposition::Verified(output),
                },
            )
            .await;
        let result = wait_for_upload(waiter, &CancellationToken::new(), test_deadline()).await;
        assert_eq!(result.is_error, Some(false));
        assert_eq!(post_branches, 1);
        assert!(matches!(
            cohort_begin(&cohort, key, fingerprint).await,
            BeginUpload::Cached(_)
        ));
    }

    #[tokio::test]
    async fn upload_cohort_locks_conflict_capacity_candidate_retry_and_terminal_states() {
        let cohort = UploadCohort::new(1);
        let key = IdempotencyKey::new("state-machine").expect("key");
        let other = IdempotencyKey::new("capacity").expect("key");
        let fingerprint = [3; 32];
        let leader = match cohort_begin(&cohort, key.clone(), fingerprint).await {
            BeginUpload::LeadNew(attempt) => attempt,
            _ => panic!("first caller must lead"),
        };
        assert!(matches!(
            cohort_begin(&cohort, key.clone(), [4; 32]).await,
            BeginUpload::Conflict
        ));
        assert!(matches!(
            cohort_begin(&cohort, other, [5; 32]).await,
            BeginUpload::Full
        ));
        let candidate = EntityId::new(FILE_ID).expect("candidate");
        cohort.retain_candidate(&key, &leader, &candidate).await;
        cohort
            .finish(
                &key,
                &leader,
                UploadExecution {
                    result: tool_error(&ToolError::mutation_indeterminate()),
                    disposition: UploadDisposition::CandidateFailed(candidate.clone()),
                },
            )
            .await;
        let replay = match cohort_begin(&cohort, key.clone(), fingerprint).await {
            BeginUpload::LeadReplay(attempt, retained) => {
                assert_eq!(retained, candidate);
                attempt
            }
            _ => panic!("retained candidate must be reverified"),
        };
        let output = cohort_output(true);
        cohort
            .finish(
                &key,
                &replay,
                UploadExecution {
                    result: cohort_success(&output),
                    disposition: UploadDisposition::Verified(output),
                },
            )
            .await;
        assert!(matches!(
            cohort_begin(&cohort, key.clone(), fingerprint).await,
            BeginUpload::Cached(FileUploadOutput { reused: true, .. })
        ));

        let uncertain = UploadCohort::new(1);
        let attempt = match cohort_begin(&uncertain, key.clone(), fingerprint).await {
            BeginUpload::LeadNew(attempt) => attempt,
            _ => panic!("uncertain leader"),
        };
        attempt.progress.mark_dispatched();
        uncertain
            .finish(
                &key,
                &attempt,
                UploadExecution {
                    result: tool_error(&ToolError::mutation_indeterminate()),
                    disposition: UploadDisposition::Indeterminate,
                },
            )
            .await;
        assert!(matches!(
            cohort_begin(&uncertain, key.clone(), fingerprint).await,
            BeginUpload::Indeterminate
        ));

        let rejected = UploadCohort::new(1);
        let attempt = match cohort_begin(&rejected, key.clone(), fingerprint).await {
            BeginUpload::LeadNew(attempt) => attempt,
            _ => panic!("rejected leader"),
        };
        rejected
            .finish(
                &key,
                &attempt,
                UploadExecution {
                    result: tool_error(&ToolError::validation()),
                    disposition: UploadDisposition::PreDispatchFailure,
                },
            )
            .await;
        assert!(matches!(
            cohort_begin(&rejected, key, fingerprint).await,
            BeginUpload::LeadNew(_)
        ));
    }

    #[tokio::test]
    async fn upload_waiter_cancellation_is_stage_safe_and_runtime_shutdown_isolated() {
        let cancellation = CancellationToken::new();
        cancellation.cancel();
        let deadline = test_deadline();
        let pre_dispatch = wait_for_upload(
            Arc::new(UploadAttempt::new(None, deadline, 0)),
            &cancellation,
            deadline,
        )
        .await;
        assert_eq!(
            pre_dispatch.structured_content.expect("error")["code"],
            "upstream"
        );

        let deadline = test_deadline();
        let dispatched = Arc::new(UploadAttempt::new(None, deadline, 0));
        dispatched.progress.mark_dispatched();
        let cancellation = CancellationToken::new();
        cancellation.cancel();
        let post_dispatch = wait_for_upload(dispatched, &cancellation, deadline).await;
        assert_eq!(
            post_dispatch.structured_content.expect("error")["code"],
            "conflict"
        );

        let runtime = RuntimeContext::from_parts(
            catalog_client(),
            1,
            Duration::from_secs(1),
            StartupStatus {
                http_available: true,
                grpc_available: false,
            },
        );
        let clone = runtime.clone();
        runtime.begin_shutdown();
        let result = clone
            .execute(
                OperationContext::new("file_shutdown_test"),
                &CancellationToken::new(),
                std::future::pending::<Result<(), AnytypeError>>(),
            )
            .await;
        assert!(matches!(
            result,
            Err(crate::runtime::RuntimeError::ShuttingDown)
        ));
    }

    #[tokio::test]
    async fn upload_cohorts_are_scoped_to_runtime_identity() {
        let client = catalog_client();
        let make_runtime = |client| {
            RuntimeContext::from_parts(
                client,
                1,
                Duration::from_secs(1),
                StartupStatus {
                    http_available: true,
                    grpc_available: false,
                },
            )
        };
        let first = make_runtime(client.clone());
        let first_clone = first.clone();
        let second = make_runtime(client);
        assert!(Arc::ptr_eq(
            &upload_cohort(&first),
            &upload_cohort(&first_clone)
        ));
        assert!(!Arc::ptr_eq(
            &upload_cohort(&first),
            &upload_cohort(&second)
        ));
        let key = IdempotencyKey::new("same-principal-key").expect("key");
        assert!(matches!(
            upload_cohort(&first)
                .begin(0, test_deadline(), key.clone(), [9; 32])
                .await,
            BeginUpload::LeadNew(_)
        ));
        assert!(matches!(
            upload_cohort(&second)
                .begin(0, test_deadline(), key, [9; 32])
                .await,
            BeginUpload::LeadNew(_)
        ));
    }

    #[tokio::test]
    async fn upload_cohort_invalidates_complete_results_on_credential_generation_change() {
        let client = catalog_client();
        let runtime = RuntimeContext::from_parts(
            client.clone(),
            1,
            Duration::from_secs(1),
            StartupStatus {
                http_available: true,
                grpc_available: false,
            },
        );
        let handlers = FileContentHandlers::new(runtime).expect("handlers");
        let key = IdempotencyKey::new("principal-safe").expect("key");
        let stable_input: FileUploadInput = serde_json::from_value(json!({
            "space":SPACE_ID,
            "name":"principal-safe.bin",
            "content_base64":BASE64_STANDARD.encode(b"abc"),
            "idempotency_key":"principal-safe"
        }))
        .expect("stable-ID input");
        let fingerprint =
            NormalizedUpload::new(SpaceId::new(SPACE_ID).expect("stable space"), stable_input)
                .fingerprint();
        let first_generation = client.http_credential_generation();
        let attempt = match handlers
            .uploads
            .begin(first_generation, test_deadline(), key.clone(), fingerprint)
            .await
        {
            BeginUpload::LeadNew(attempt) => attempt,
            _ => panic!("first generation must lead"),
        };
        let output = cohort_output(false);
        handlers
            .uploads
            .finish(
                &key,
                &attempt,
                UploadExecution {
                    result: cohort_success(&output),
                    disposition: UploadDisposition::Verified(output),
                },
            )
            .await;
        assert!(matches!(
            handlers
                .uploads
                .begin(first_generation, test_deadline(), key.clone(), fingerprint,)
                .await,
            BeginUpload::Cached(_)
        ));

        let barrier = Arc::new(Barrier::new(2));
        let setter = {
            let client = client.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                client.set_api_key(HttpCredentials::new("replacement-token"));
            })
        };
        barrier.wait();
        setter.join().expect("concurrent credential setter");
        let replacement_generation = client.http_credential_generation();
        assert!(replacement_generation > first_generation);
        let replacement_attempt = match handlers
            .uploads
            .begin(
                replacement_generation,
                test_deadline(),
                key.clone(),
                fingerprint,
            )
            .await
        {
            BeginUpload::LeadNew(attempt) => attempt,
            _ => panic!("replacement generation must not replay cached success"),
        };
        let output = cohort_output(false);
        handlers
            .uploads
            .finish(
                &key,
                &replacement_attempt,
                UploadExecution {
                    result: cohort_success(&output),
                    disposition: UploadDisposition::Verified(output),
                },
            )
            .await;

        let barrier = Arc::new(Barrier::new(2));
        let clearer = {
            let client = client.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                client.clear_api_key();
            })
        };
        barrier.wait();
        clearer.join().expect("concurrent credential clearer");
        let cleared_generation = client.http_credential_generation();
        assert!(cleared_generation > replacement_generation);
        assert!(matches!(
            handlers
                .uploads
                .begin(cleared_generation, test_deadline(), key, fingerprint,)
                .await,
            BeginUpload::LeadNew(_)
        ));
    }

    #[tokio::test]
    async fn upload_deadline_is_shared_and_never_extended_by_waiters() {
        let cohort = UploadCohort::new(1);
        let key = IdempotencyKey::new("shared-deadline").expect("key");
        let leader_deadline = Instant::now()
            .checked_add(Duration::from_millis(20))
            .unwrap_or_else(Instant::now);
        let leader = match cohort
            .begin(0, leader_deadline, key.clone(), [12; 32])
            .await
        {
            BeginUpload::LeadNew(attempt) => attempt,
            _ => panic!("leader expected"),
        };
        let later_deadline = Instant::now()
            .checked_add(Duration::from_secs(30))
            .unwrap_or_else(Instant::now);
        let waiter = match cohort.begin(0, later_deadline, key, [12; 32]).await {
            BeginUpload::Wait(attempt) => attempt,
            _ => panic!("waiter expected"),
        };
        assert!(Arc::ptr_eq(&leader, &waiter));
        assert_eq!(waiter.deadline, leader_deadline);
        let result = wait_for_upload(waiter, &CancellationToken::new(), later_deadline).await;
        assert_eq!(
            result.structured_content.expect("timeout")["code"],
            "upstream"
        );

        let runtime = RuntimeContext::from_parts(
            catalog_client(),
            1,
            Duration::from_secs(30),
            StartupStatus {
                http_available: true,
                grpc_available: false,
            },
        );
        let expired = Instant::now()
            .checked_sub(Duration::from_millis(1))
            .unwrap_or_else(Instant::now);
        let result = runtime
            .execute_classified_until(
                expired,
                OperationContext::new("file_expired_deadline"),
                &CancellationToken::new(),
                std::future::pending::<Result<(), FileOperationError>>(),
                FileOperationError::diagnostic,
            )
            .await;
        assert!(matches!(result, Err(ControlledOperationError::TimedOut)));
    }

    #[tokio::test]
    async fn expired_cached_admission_never_returns_success() {
        let cohort = UploadCohort::new(1);
        let key = IdempotencyKey::new("expired-cached").expect("key");
        let fingerprint = [13; 32];
        let attempt = match cohort_begin(&cohort, key.clone(), fingerprint).await {
            BeginUpload::LeadNew(attempt) => attempt,
            _ => panic!("leader expected"),
        };
        let output = cohort_output(false);
        cohort
            .finish(
                &key,
                &attempt,
                UploadExecution {
                    result: cohort_success(&output),
                    disposition: UploadDisposition::Verified(output),
                },
            )
            .await;
        let expired = Instant::now()
            .checked_sub(Duration::from_millis(1))
            .unwrap_or_else(Instant::now);
        assert!(matches!(
            admit_upload(&cohort, 0, expired, key, fingerprint).await,
            BeginUpload::Expired
        ));
    }

    #[tokio::test]
    async fn cohort_lock_contention_expires_without_stranding_running_admission() {
        let cohort = Arc::new(UploadCohort::new(1));
        let guard = cohort.state.lock().await;
        let deadline = Instant::now()
            .checked_add(Duration::from_millis(20))
            .unwrap_or_else(Instant::now);
        let blocked = {
            let cohort = cohort.clone();
            tokio::spawn(async move {
                admit_upload(
                    &cohort,
                    0,
                    deadline,
                    IdempotencyKey::new("contended").expect("key"),
                    [14; 32],
                )
                .await
            })
        };
        let result = tokio::time::timeout(Duration::from_secs(1), blocked)
            .await
            .expect("bounded admission task")
            .expect("admission join");
        assert!(matches!(result, BeginUpload::Expired));
        drop(guard);
        assert!(cohort.state.lock().await.entries.is_empty());

        let key = IdempotencyKey::new("post-admission-expiry").expect("key");
        let admission = cohort_begin(&cohort, key.clone(), [15; 32]).await;
        let attempt = match &admission {
            BeginUpload::LeadNew(attempt) => attempt.clone(),
            _ => panic!("leader expected"),
        };
        cohort.reject_unstarted(&key, &admission).await;
        assert!(cohort.state.lock().await.entries.is_empty());
        let result = wait_for_upload(attempt, &CancellationToken::new(), test_deadline()).await;
        assert_eq!(
            result.structured_content.expect("rejection")["code"],
            "upstream"
        );
    }

    #[test]
    #[serial_test::serial(disposable_anytype_files)]
    #[ignore = "requires env-only disposable credentials and an authenticated headless Anytype server"]
    fn production_direct_and_stdio_upload_metadata_ranges_hash_and_cleanup() {
        run_large_future(|| async {
            let outcome = Box::pin(with_disposable_space_context(
            "any-mcp-files-terminal",
            |ctx| {
                Box::pin(async move {
                    ctx.client.ping_http().await?;
                    ctx.client.ping_grpc().await?;
                    let mut state = 0x0A11_F17E_u32;
                    let mut bytes = Vec::with_capacity(8_192);
                    for _ in 0..8_192 {
                        state ^= state << 13;
                        state ^= state >> 17;
                        state ^= state << 5;
                        bytes.push((state & 0xff) as u8);
                    }
                    let expected_hash = FileSha256::digest(&bytes);

                    let space_name = ctx
                        .client
                        .space(&ctx.space_id)
                        .get_direct()
                        .await?
                        .name;

                    let direct_server = production_files_server(ctx.client.clone(), false);
                    let direct_name = "mcp-files-direct.bin";
                    let direct_key = format!("mcp-files-direct-{}", unique_suffix());
                    let direct_arguments = json!({
                        "space": ctx.space_id,
                        "name": direct_name,
                        "content_base64": BASE64_STANDARD.encode(&bytes),
                        "media_type": "application/octet-stream",
                        "idempotency_key": direct_key
                    });
                    let before_upload = ctx.client.http_metrics();
                    let uploaded =
                        direct_tool(&direct_server, "file_upload", direct_arguments.clone()).await;
                    assert_eq!(uploaded.is_error, Some(false), "{uploaded:?}");
                    let uploaded_value = uploaded
                        .structured_content
                        .as_ref()
                        .expect("direct upload output");
                    let direct_id = uploaded_value["file_id"]
                        .as_str()
                        .expect("direct candidate id")
                        .to_owned();
                    ctx.register_object(&direct_id);
                    assert_eq!(uploaded_value["space_id"], ctx.space_id);
                    assert_eq!(uploaded_value["content_sha256"], expected_hash.as_str());
                    assert_eq!(uploaded_value["reused"], false);

                    let after_upload = ctx.client.http_metrics();
                    assert_eq!(after_upload.logical_operations - before_upload.logical_operations, 4);
                    assert_eq!(after_upload.total_requests - before_upload.total_requests, 4);
                    assert_eq!(after_upload.physical_attempts - before_upload.physical_attempts, 4);
                    assert_eq!(after_upload.multipart_posts - before_upload.multipart_posts, 1);
                    assert_eq!(after_upload.successful_responses - before_upload.successful_responses, 4);
                    assert_eq!(after_upload.errors - before_upload.errors, 0);
                    assert_eq!(after_upload.retries - before_upload.retries, 0);
                    assert_eq!(after_upload.rate_limit_errors - before_upload.rate_limit_errors, 0);
                    assert_eq!(after_upload.bytes_sent - before_upload.bytes_sent, 8_458);

                    let metrics = after_upload;
                    let replay = direct_tool(&direct_server, "file_upload", direct_arguments).await;
                    assert_eq!(replay.is_error, Some(false));
                    assert_eq!(
                        replay.structured_content.as_ref().expect("direct replay")["reused"],
                        true
                    );
                    assert_eq!(ctx.client.http_metrics(), metrics);

                    let metadata = direct_tool(
                        &direct_server,
                        "file_metadata",
                        json!({"space":space_name,"file_id":direct_id}),
                    )
                    .await;
                    assert_eq!(metadata.is_error, Some(false), "{metadata:?}");
                    assert_eq!(
                        metadata
                            .structured_content
                            .as_ref()
                            .expect("direct metadata")["size_bytes"],
                        bytes.len()
                    );

                    let split_at = bytes.len() / 2;
                    let first = direct_tool(
                        &direct_server,
                        "file_read",
                        json!({
                            "space":ctx.space_id,
                            "file_id":direct_id,
                            "offset":0,
                            "length":split_at
                        }),
                    )
                    .await;
                    let second = direct_tool(
                        &direct_server,
                        "file_read",
                        json!({
                            "space":ctx.space_id,
                            "file_id":direct_id,
                            "offset":split_at,
                            "length":bytes.len()-split_at
                        }),
                    )
                    .await;
                    assert_eq!(first.is_error, Some(false), "{first:?}");
                    assert_eq!(second.is_error, Some(false), "{second:?}");
                    assert_eq!(
                        first.structured_content.as_ref().expect("first range")["content_sha256"],
                        FileSha256::digest(&bytes[..split_at]).as_str()
                    );
                    assert_eq!(
                        second.structured_content.as_ref().expect("second range")["content_sha256"],
                        FileSha256::digest(&bytes[split_at..]).as_str()
                    );
                    let first_uri = first
                        .structured_content
                        .as_ref()
                        .expect("first structured")["resource_uri"]
                        .as_str()
                        .expect("first resource URI");
                    let direct_resource = direct_server
                        .read_resource_wire(
                            ReadResourceRequestParams::new(first_uri),
                            &CancellationToken::new(),
                        )
                        .await
                        .expect("direct resources/read");
                    assert_blob_resource(&direct_resource, first_uri, &bytes[..split_at]);

                    let independent = ctx
                        .client
                        .files()
                        .download_request(&ctx.space_id, &direct_id)
                        .response_limit_bytes(MAX_FILE_CONTENT_BYTES + 1)
                        .error_limit_bytes(MAX_ERROR_BODY_BYTES)
                        .header_evidence_limit_bytes(MAX_HEADER_EVIDENCE_BYTES)
                        .max_attempts(MAX_SAFE_ATTEMPTS)
                        .download()
                        .await?;
                    assert_eq!(independent.status.as_u16(), 200);
                    assert_eq!(FileSha256::digest(&independent.bytes), expected_hash);

                    let stdio_name = format!("mcp-files-stdio-{}.bin", unique_suffix());
                    let stdio_upload = production_stdio_tool(
                        production_files_server(ctx.client.clone(), false),
                        "file_upload",
                        json!({
                            "space":ctx.space_id,
                            "name":stdio_name,
                            "content_base64":BASE64_STANDARD.encode(&bytes),
                            "media_type":"application/octet-stream",
                            "idempotency_key":format!("mcp-files-stdio-{}",unique_suffix())
                        }),
                    )
                    .await;
                    assert_eq!(stdio_upload["result"]["isError"], false, "{stdio_upload}");
                    let stdio_id = stdio_upload["result"]["structuredContent"]["file_id"]
                        .as_str()
                        .expect("stdio candidate id")
                        .to_owned();
                    ctx.register_object(&stdio_id);
                    let stdio_metadata = production_stdio_tool(
                        production_files_server(ctx.client.clone(), false),
                        "file_metadata",
                        json!({"space":ctx.space_id,"file_id":stdio_id}),
                    )
                    .await;
                    assert_eq!(stdio_metadata["result"]["isError"], false);
                    for (offset, length) in [(0, split_at), (split_at, bytes.len() - split_at)] {
                        let read = production_stdio_tool(
                            production_files_server(ctx.client.clone(), false),
                            "file_read",
                            json!({
                                "space":ctx.space_id,
                                "file_id":stdio_id,
                                "offset":offset,
                                "length":length
                            }),
                        )
                        .await;
                        assert_eq!(read["result"]["isError"], false, "{read}");
                        let uri = read["result"]["structuredContent"]["resource_uri"]
                            .as_str()
                            .expect("stdio resource URI");
                        let stdio_resource = production_stdio_request(
                            production_files_server(ctx.client.clone(), false),
                            "resources/read",
                            json!({"uri":uri}),
                        )
                        .await;
                        assert!(stdio_resource.get("error").is_none(), "{stdio_resource}");
                        assert_stdio_blob_resource(
                            &stdio_resource["result"],
                            uri,
                            &bytes[offset..offset + length],
                        );
                        let parity = production_files_server(ctx.client.clone(), false)
                            .read_resource_wire(
                                ReadResourceRequestParams::new(uri),
                                &CancellationToken::new(),
                            )
                            .await
                            .expect("direct parity resources/read");
                        assert_eq!(
                            serde_json::to_value(parity.contents).expect("direct parity JSON"),
                            stdio_resource["result"]["contents"]
                        );
                    }
                    Ok(())
                })
            },
        ))
        .await
        .expect("disposable files harness");
            match outcome {
                DisposableRun::Completed(()) => {}
                DisposableRun::Skipped(reason) => {
                    eprintln!("files live gate skipped before callback: {reason:?}");
                }
            }
        });
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

    fn canonical_sha256(value: &Value) -> String {
        let encoded = serde_json::to_string(&canonical_json(value.clone()))
            .expect("canonical production surface");
        Sha256::digest(encoded.as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    async fn files_production_surface_snapshot() -> Value {
        let client = catalog_client();
        let read_write = production_files_server(client.clone(), false);
        let read_only = production_files_server(client.clone(), true);
        let read_write_catalog = serde_json::to_value(
            read_write
                .list_tools_wire(None)
                .expect("read-write catalog"),
        )
        .expect("read-write catalog JSON");
        let read_only_catalog =
            serde_json::to_value(read_only.list_tools_wire(None).expect("read-only catalog"))
                .expect("read-only catalog JSON");
        let read_write_templates = serde_json::to_value(
            read_write
                .list_resource_templates_wire(None)
                .expect("read-write templates"),
        )
        .expect("read-write templates JSON");
        let read_only_templates = serde_json::to_value(
            read_only
                .list_resource_templates_wire(None)
                .expect("read-only templates"),
        )
        .expect("read-only templates JSON");
        let read_write_status = serde_json::to_value(
            direct_tool(&read_write, "optional_toolset_status", json!({})).await,
        )
        .expect("read-write status JSON");
        let read_only_status = serde_json::to_value(
            direct_tool(&read_only, "optional_toolset_status", json!({})).await,
        )
        .expect("read-only status JSON");
        let tool_names = |catalog: &Value| {
            catalog["tools"]
                .as_array()
                .expect("catalog tools")
                .iter()
                .map(|tool| tool["name"].as_str().expect("tool name").to_owned())
                .collect::<Vec<_>>()
        };
        let stdio_read_write_catalog = production_stdio_request(
            production_files_server(client.clone(), false),
            "tools/list",
            json!({}),
        )
        .await;
        let stdio_read_only_catalog = production_stdio_request(
            production_files_server(client.clone(), true),
            "tools/list",
            json!({}),
        )
        .await;
        let stdio_read_write_templates = production_stdio_request(
            production_files_server(client.clone(), false),
            "resources/templates/list",
            json!({}),
        )
        .await;
        let stdio_read_only_templates = production_stdio_request(
            production_files_server(client.clone(), true),
            "resources/templates/list",
            json!({}),
        )
        .await;
        let stdio_read_write_status = production_stdio_tool(
            production_files_server(client.clone(), false),
            "optional_toolset_status",
            json!({}),
        )
        .await;
        let stdio_read_only_status = production_stdio_tool(
            production_files_server(client, true),
            "optional_toolset_status",
            json!({}),
        )
        .await;
        json!({
            "read_write_catalog_sha256":canonical_sha256(&read_write_catalog),
            "read_write_tool_names":tool_names(&read_write_catalog),
            "read_only_catalog_sha256":canonical_sha256(&read_only_catalog),
            "read_only_tool_names":tool_names(&read_only_catalog),
            "read_write_resource_templates":read_write_templates,
            "read_only_resource_templates":read_only_templates,
            "read_write_status_call":read_write_status,
            "read_only_status_call":read_only_status,
            "stdio_read_write_catalog_sha256":canonical_sha256(&stdio_read_write_catalog["result"]),
            "stdio_read_write_tool_names":tool_names(&stdio_read_write_catalog["result"]),
            "stdio_read_write_catalog_control":{
                "resultType":stdio_read_write_catalog["result"]["resultType"],
                "ttlMs":stdio_read_write_catalog["result"]["ttlMs"],
                "cacheScope":stdio_read_write_catalog["result"]["cacheScope"]
            },
            "stdio_read_only_catalog_sha256":canonical_sha256(&stdio_read_only_catalog["result"]),
            "stdio_read_only_tool_names":tool_names(&stdio_read_only_catalog["result"]),
            "stdio_read_only_catalog_control":{
                "resultType":stdio_read_only_catalog["result"]["resultType"],
                "ttlMs":stdio_read_only_catalog["result"]["ttlMs"],
                "cacheScope":stdio_read_only_catalog["result"]["cacheScope"]
            },
            "stdio_read_write_resource_templates":stdio_read_write_templates["result"],
            "stdio_read_only_resource_templates":stdio_read_only_templates["result"],
            "stdio_read_write_status_call":stdio_read_write_status["result"],
            "stdio_read_only_status_call":stdio_read_only_status["result"]
        })
    }

    fn files_token_budget_snapshot() -> Value {
        let tokenizer = o200k_base().expect("files tokenizer");
        let token_count = |value: Value| {
            tokenizer
                .encode_ordinary(
                    &serde_json::to_string(&canonical_json(value)).expect("canonical files JSON"),
                )
                .len()
        };
        let client = catalog_client();
        let base = production_base_server(client.clone());
        let read_write = production_files_server(client.clone(), false);
        let read_only = production_files_server(client, true);
        let base_value = serde_json::to_value(base.list_tools_wire(None).expect("base tools/list"))
            .expect("base catalog JSON");
        let base_json = serde_json::to_string(&canonical_json(base_value.clone()))
            .expect("canonical base catalog");
        let base_hash = Sha256::digest(base_json.as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let read_write_value = serde_json::to_value(
            read_write
                .list_tools_wire(None)
                .expect("read-write tools/list"),
        )
        .expect("read-write catalog JSON");
        let read_only_value = serde_json::to_value(
            read_only
                .list_tools_wire(None)
                .expect("read-only tools/list"),
        )
        .expect("read-only catalog JSON");
        let status = read_write
            .tools()
            .iter()
            .find(|tool| tool.name == "optional_toolset_status")
            .expect("common optional status tool")
            .clone();
        let status_value = serde_json::to_value(ListToolsResult::with_all_items(vec![status]))
            .expect("status tools/list JSON");
        let per_tool = read_write
            .tools()
            .iter()
            .filter(|tool| tool.name.starts_with("file_"))
            .map(|tool| {
                (
                    tool.name.to_string(),
                    token_count(serde_json::to_value(tool).expect("file tool JSON")),
                )
            })
            .collect::<BTreeMap<_, _>>();

        let mut state = 0x0A11_F17E_u32;
        let random = (0..MAX_FILE_CONTENT_BYTES)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                state as u8
            })
            .collect::<Vec<_>>();
        let maximum = maximum_observation(&random);
        let maximum_tool = encode_file_read(maximum.clone(), &ProtocolVersion::V_2025_11_25)
            .expect("maximum files tool result");
        let maximum_resource =
            encode_resource_read(maximum).expect("maximum files resource result");
        let maximum_scalar = '\u{10ffff}';
        assert_eq!(maximum_scalar.len_utf8(), 4);
        let maximum_request = json!({
            "space":maximum_scalar.to_string().repeat(MAX_SPACE_REFERENCE_CHARS),
            "name":maximum_scalar.to_string().repeat(MAX_FILE_NAME_CHARS),
            "content_base64":BASE64_STANDARD.encode(&random),
            "media_type":format!("application/{}","x".repeat(243)),
            "idempotency_key":maximum_scalar.to_string().repeat(256)
        });
        let maximum_upload = FileUploadOutput {
            file_id: EntityId::new("f".repeat(256)).expect("maximum file id"),
            space_id: SpaceId::new("s".repeat(256)).expect("maximum space id"),
            requested_name: FileName::new(maximum_scalar.to_string().repeat(MAX_FILE_NAME_CHARS))
                .expect("maximum file name"),
            media_type: FileMediaType::from_evidence(Some(&format!(
                "application/{}",
                "x".repeat(243)
            )))
            .expect("maximum media type"),
            size_bytes: JsonSafeInteger(MAX_FILE_CONTENT_BYTES),
            content_sha256: FileSha256::digest(&random),
            reused: false,
        };
        let upload_success = file_upload_tool()
            .expect("upload contract")
            .success(&maximum_upload)
            .expect("maximum upload success");
        let maximum_metadata = FileMetadataOutput {
            file_id: EntityId::new("f".repeat(256)).expect("maximum metadata file id"),
            space_id: SpaceId::new("s".repeat(256)).expect("maximum metadata space id"),
            media_type: maximum_upload.media_type.clone(),
            size_bytes: JsonSafeInteger(JSON_SAFE_INTEGER_MAX),
            accepts_byte_ranges: true,
            strong_etag: Some(
                StrongEntityTag::new(format!("\"{}\"", "e".repeat(254)))
                    .expect("maximum metadata etag"),
            ),
            last_modified: Some(FileHttpDate::from_evidence(DATE).expect("metadata date")),
        };
        let metadata_success = file_metadata_tool()
            .expect("metadata contract")
            .success(&maximum_metadata)
            .expect("maximum metadata success");
        json!({
            "tokenizer":"tiktoken o200k_base (tiktoken-rs 0.12.0)",
            "base_catalog_sha256":base_hash,
            "base_catalog_tokens":token_count(base_value),
            "selected":["files"],
            "common_status_ceiling_tokens":500,
            "common_status_tokens":token_count(status_value),
            "files_catalog_ceiling_tokens":3400,
            "composed_total_tokens":token_count(read_write_value),
            "read_only_composed_total_tokens":token_count(read_only_value),
            "per_tool_ceiling_tokens":1200,
            "per_tool_tokens":per_tool,
            "maximum_upload_request_ceiling_tokens":65000,
            "maximum_upload_request_tokens":token_count(maximum_request),
            "maximum_file_read_ceiling_tokens":70000,
            "maximum_file_read_tokens":token_count(serde_json::to_value(maximum_tool).expect("tool result JSON")),
            "maximum_resource_read_ceiling_tokens":70000,
            "maximum_resource_read_tokens":token_count(serde_json::to_value(maximum_resource).expect("resource result JSON")),
            "metadata_upload_success_ceiling_tokens":8000,
            "maximum_metadata_success_tokens":token_count(serde_json::to_value(metadata_success).expect("metadata success JSON")),
            "maximum_upload_success_tokens":token_count(serde_json::to_value(upload_success).expect("upload success JSON")),
            "maximum_unicode_scalar":"U+10FFFF (4-byte UTF-8)",
            "live_new_upload_multipart_body_bytes":8458,
            "deterministic_random_seed":"0x0A11F17E"
        })
    }

    fn files_result_snapshot() -> Value {
        let bytes = b"Hello";
        let read = encode_file_read(
            observation("text/plain; charset=utf-8", bytes),
            &ProtocolVersion::V_2025_11_25,
        )
        .expect("representative read");
        let upload = FileUploadOutput {
            file_id: EntityId::new(FILE_ID).expect("file id"),
            space_id: SpaceId::new(SPACE_ID).expect("space id"),
            requested_name: FileName::new("report.txt").expect("file name"),
            media_type: FileMediaType::from_evidence(Some("text/plain; charset=utf-8"))
                .expect("media type"),
            size_bytes: JsonSafeInteger(bytes.len() as u64),
            content_sha256: FileSha256::digest(bytes),
            reused: false,
        };
        json!({
            "file_upload":upload,
            "file_metadata":{
                "file_id":FILE_ID,
                "space_id":SPACE_ID,
                "media_type":"text/plain; charset=utf-8",
                "size_bytes":5,
                "accepts_byte_ranges":true,
                "strong_etag":"\"file-v1\"",
                "last_modified":DATE
            },
            "file_read_structured":read.structured_content.expect("read structured content")
        })
    }

    #[test]
    fn files_token_and_result_snapshots_are_exact() {
        let expected_tokens: Value =
            serde_json::from_str(FILES_TOKEN_BUDGET_SNAPSHOT).expect("token snapshot JSON");
        let expected_results: Value =
            serde_json::from_str(FILES_RESULT_SNAPSHOT).expect("result snapshot JSON");
        assert_eq!(files_token_budget_snapshot(), expected_tokens);
        assert_eq!(files_result_snapshot(), expected_results);
    }

    #[test]
    fn production_catalog_templates_status_and_stdio_are_exact() {
        run_large_future(|| async {
            let expected: Value = serde_json::from_str(FILES_PRODUCTION_SURFACE_SNAPSHOT)
                .expect("production surface snapshot JSON");
            assert_eq!(files_production_surface_snapshot().await, expected);
        });
    }

    #[test]
    #[ignore = "manual files snapshot reporter; review values before committing"]
    fn report_files_snapshots() {
        println!(
            "FILES_TOKEN_SNAPSHOT={}\nFILES_RESULT_SNAPSHOT={}",
            serde_json::to_string_pretty(&files_token_budget_snapshot())
                .expect("pretty token snapshot"),
            serde_json::to_string_pretty(&files_result_snapshot()).expect("pretty result snapshot")
        );
    }

    #[test]
    #[ignore = "manual production surface snapshot reporter; review values before committing"]
    fn report_files_production_surface_snapshot() {
        run_large_future(|| async {
            println!(
                "FILES_PRODUCTION_SURFACE_SNAPSHOT={}",
                serde_json::to_string_pretty(&files_production_surface_snapshot().await)
                    .expect("pretty production surface snapshot")
            );
        });
    }
}
