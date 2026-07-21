//! # Anytype Files
//!
//! File transfers use REST when the HTTP API has equivalent functionality.
//! Metadata, search, preload, URL upload, and uploads with rich placement/style
//! options continue to use gRPC.
//!

use std::path::{Path, PathBuf};

use anytype_rpc::{
    anytype::rpc::{
        file::{discard_preload, download, upload},
        object::search_with_meta,
    },
    model,
};
use bytes::Bytes;
use chrono::{DateTime, FixedOffset};
use prost_types::{ListValue, Struct, Value};
use reqwest::{
    Method, StatusCode,
    header::{
        ACCEPT_RANGES, CACHE_CONTROL, CONTENT_LENGTH, CONTENT_RANGE, CONTENT_TYPE, ETAG, HeaderMap,
        HeaderName, HeaderValue, IF_MATCH, IF_MODIFIED_SINCE, IF_NONE_MATCH, IF_RANGE,
        IF_UNMODIFIED_SINCE, LAST_MODIFIED, RANGE,
    },
};
use serde::{Deserialize, Serialize};
use serde_json::Number;
use tonic::Request;
use tracing::{debug, error, info};

use crate::{
    Result,
    client::AnytypeClient,
    error::AnytypeError,
    filters::{Filter, Sort, SortDirection},
    grpc_util::{ensure_error_ok, grpc_status, with_token_request},
    paged::{PagedResult, PaginatedResponse, PaginationMeta},
};

// ============================================================================
// Public types
// ============================================================================

/// Hard ceiling for retained allowlisted file-response header evidence.
pub const MAX_FILE_HEADER_EVIDENCE_BYTES: u64 = 1024 * 1024;
/// Hard ceiling for physical attempts made by one file request.
pub const MAX_FILE_REQUEST_ATTEMPTS: u32 = 6;

pub(crate) const DEFAULT_FILE_HEADER_EVIDENCE_BYTES: u64 = 64 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileObject {
    pub id: String,
    pub space_id: String,
    pub name: Option<String>,
    pub size: Option<i64>,
    pub mime: Option<String>,
    pub added_at: Option<DateTime<FixedOffset>>,
    #[serde(default)]
    pub file_type: FileType,
    pub style: FileStyle,
    pub target_object_id: Option<String>,
    pub details: serde_json::Value,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, strum::EnumString, strum::Display)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum FileType {
    #[default]
    File,
    Image,
    Video,
    Audio,
    Pdf,
    /// catch-all in case other types added in the future
    #[serde(untagged)]
    Other(String),
}

#[derive(Debug, Clone, Serialize, Deserialize, strum::EnumString, strum::Display)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum FileStyle {
    Auto,
    Link,
    Embed,
}

/// Response from an HTTP file upload (`POST /v1/spaces/{space_id}/files`).
///
/// This is the subset of file metadata the REST upload endpoint returns. The
/// unified [`FilesClient::upload`] builder normalizes this into [`FileObject`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileUploadResponse {
    /// File object ID
    pub object_id: String,
    /// Original file name as stored
    #[serde(default)]
    pub name: Option<String>,
    /// File extension without the leading dot, when known
    #[serde(default)]
    pub extension: Option<String>,
    /// MIME type (for example `image/png`)
    #[serde(default)]
    pub media: Option<String>,
    /// Size of the uploaded file, in bytes
    #[serde(default)]
    pub size_in_bytes: Option<i64>,
}

/// HTTP metadata returned for a REST file download or `HEAD` request.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileHttpMetadata {
    /// Media type of the selected file or image variant.
    pub content_type: Option<String>,
    /// Response body length. For a ranged response this is the partial length.
    pub content_length: Option<u64>,
    /// Byte range selected by the server, such as `bytes 0-499/1200`.
    pub content_range: Option<String>,
    /// Range units supported by the server, normally `bytes`.
    pub accept_ranges: Option<String>,
    /// Server-provided modification timestamp in HTTP-date form.
    pub last_modified: Option<String>,
    /// Server-provided entity tag, when available.
    pub etag: Option<String>,
    /// Cache policy supplied by the file endpoint.
    pub cache_control: Option<String>,
    /// Total bytes retained across the allowlisted response headers.
    ///
    /// Header names, separators, and values all count toward this total.
    pub retained_header_bytes: u64,
}

/// Result of a configurable REST file download.
#[derive(Debug, Clone)]
pub struct FileContentResponse {
    /// HTTP status, including `206`, `304`, `412`, or `416` control responses.
    pub status: StatusCode,
    /// File-related response headers.
    pub metadata: FileHttpMetadata,
    /// Response body. Conditional and `HEAD` responses normally have no body.
    pub bytes: Bytes,
}

impl FileContentResponse {
    /// Returns true when a conditional request found the cached representation current.
    #[must_use]
    pub fn is_not_modified(&self) -> bool {
        self.status == StatusCode::NOT_MODIFIED
    }

    /// Returns true when the server fulfilled a byte-range request.
    #[must_use]
    pub fn is_partial(&self) -> bool {
        self.status == StatusCode::PARTIAL_CONTENT
    }
}

// ============================================================================
// Client entry point
// ============================================================================

#[derive(Debug)]
pub struct FilesClient<'a> {
    client: &'a AnytypeClient,
}

impl AnytypeClient {
    #[must_use]
    pub fn files(&self) -> FilesClient<'_> {
        FilesClient { client: self }
    }
}

impl<'a> FilesClient<'a> {
    pub fn list(&self, space_id: impl Into<String>) -> FileListRequest<'a> {
        FileListRequest {
            client: self.client,
            space_id: space_id.into(),
            filters: Vec::new(),
            limit: None,
            offset: None,
        }
    }

    pub fn search(&self, space_id: impl Into<String>) -> FileSearchRequest<'a> {
        FileSearchRequest {
            client: self.client,
            space_id: space_id.into(),
            text: None,
            filters: Vec::new(),
            sorts: Vec::new(),
            limit: None,
            offset: None,
        }
    }

    pub fn get(
        &self,
        space_id: impl Into<String>,
        object_id: impl Into<String>,
    ) -> FileGetRequest<'a> {
        FileGetRequest {
            client: self.client,
            space_id: space_id.into(),
            object_id: object_id.into(),
        }
    }

    /// Download through the legacy gRPC API.
    ///
    /// New code should prefer [`download_bytes`](Self::download_bytes), which
    /// uses the REST file endpoint. This method remains available for callers
    /// that rely on the server writing directly to a destination path.
    pub fn download(&self, object_id: impl Into<String>) -> FileDownloadRequest<'a> {
        FileDownloadRequest {
            client: self.client,
            object_id: object_id.into(),
            destination: None,
        }
    }

    pub fn upload(&self, space_id: impl Into<String>) -> FileUploadRequest<'a> {
        FileUploadRequest {
            client: self.client,
            space_id: space_id.into(),
            source: None,
            file_type: None,
            style: None,
            details: None,
            created_in_context: None,
            created_in_context_ref: None,
            file_name: None,
            mime: None,
        }
    }

    pub fn preload(&self, space_id: impl Into<String>) -> FilePreloadRequest<'a> {
        FilePreloadRequest {
            client: self.client,
            space_id: space_id.into(),
            source: None,
            file_type: None,
            created_in_context: None,
            created_in_context_ref: None,
        }
    }

    pub fn discard_preload(
        &self,
        space_id: impl Into<String>,
        file_id: impl Into<String>,
    ) -> FileDiscardPreloadRequest<'a> {
        FileDiscardPreloadRequest {
            client: self.client,
            space_id: space_id.into(),
            file_id: file_id.into(),
        }
    }
}

// ============================================================================
// HTTP (REST) file transfer
//
// Added in the 2025-11-08 / anytype-heart 0.50.15 REST surface. These wrap the
// REST endpoints directly (no gRPC channel required). See the capability
// mapping and combined-API recommendation in `docs/http-grpc-overlap.md`.
// ============================================================================

impl<'a> FilesClient<'a> {
    /// Download a file's raw bytes over REST.
    pub async fn download_bytes(
        &self,
        space_id: impl Into<String>,
        file_id: impl Into<String>,
    ) -> Result<Bytes> {
        Ok(self
            .download_request(space_id, file_id)
            .download()
            .await?
            .bytes)
    }

    /// Delete a file over REST.
    pub async fn delete(
        &self,
        space_id: impl Into<String>,
        file_id: impl Into<String>,
    ) -> Result<()> {
        self.delete_request(space_id, file_id).delete().await
    }

    /// Configure a REST file download or `HEAD` metadata request.
    #[must_use]
    pub fn download_request(
        &self,
        space_id: impl Into<String>,
        file_id: impl Into<String>,
    ) -> FileContentRequest<'a> {
        FileContentRequest {
            client: self.client,
            space_id: space_id.into(),
            file_id: file_id.into(),
            width: None,
            range: None,
            invalid_range: false,
            if_match: None,
            if_none_match: None,
            if_modified_since: None,
            if_unmodified_since: None,
            if_range: None,
            response_limit_bytes: None,
            error_limit_bytes: None,
            header_evidence_limit_bytes: None,
            max_attempts: None,
        }
    }

    /// Fetch file metadata with an HTTP `HEAD` request.
    ///
    /// Use [`download_request`](Self::download_request) when image width or
    /// conditional headers are needed.
    pub async fn metadata(
        &self,
        space_id: impl Into<String>,
        file_id: impl Into<String>,
    ) -> Result<FileContentResponse> {
        self.download_request(space_id, file_id).head().await
    }

    /// Configure a REST file deletion, including permanent deletion.
    #[must_use]
    pub fn delete_request(
        &self,
        space_id: impl Into<String>,
        file_id: impl Into<String>,
    ) -> FileDeleteRequest<'a> {
        FileDeleteRequest {
            client: self.client,
            space_id: space_id.into(),
            file_id: file_id.into(),
            skip_bin: false,
        }
    }

    /// Upload a file over the REST API (`POST /v1/spaces/{space_id}/files`).
    ///
    /// This is the REST equivalent of the gRPC [`upload`](Self::upload): simpler
    /// (multipart bytes in, minimal metadata out) and it needs no gRPC channel.
    /// For richer uploads (`style`, `details`, created-in-context), use the gRPC
    /// [`upload`](Self::upload) path.
    ///
    /// Provide the bytes with [`FileHttpUploadRequest::bytes`] or a filesystem
    /// path with [`FileHttpUploadRequest::path`], then call
    /// [`FileHttpUploadRequest::upload`].
    #[must_use]
    #[deprecated(since = "0.5.0", note = "use upload for automatic backend selection")]
    pub fn http_upload(&self, space_id: impl Into<String>) -> FileHttpUploadRequest<'a> {
        FileHttpUploadRequest {
            client: self.client,
            space_id: space_id.into(),
            file_name: None,
            mime: None,
            data: None,
            source_path: None,
        }
    }

    /// Download a file's raw bytes over the REST API
    /// (`GET /v1/spaces/{space_id}/files/{file_id}`).
    ///
    /// Returns the file contents. This is the REST equivalent of the gRPC
    /// [`download`](Self::download); both stream the same raw bytes.
    #[deprecated(since = "0.5.0", note = "use download_bytes")]
    pub async fn http_download(
        &self,
        space_id: impl Into<String>,
        file_id: impl Into<String>,
    ) -> Result<Bytes> {
        self.download_bytes(space_id, file_id).await
    }

    /// Delete a file over the REST API
    /// (`DELETE /v1/spaces/{space_id}/files/{file_id}`).
    ///
    /// The REST API is the only transport with a first-class file delete; the
    /// gRPC path removes files via generic object deletion.
    #[deprecated(since = "0.5.0", note = "use delete")]
    pub async fn http_delete(
        &self,
        space_id: impl Into<String>,
        file_id: impl Into<String>,
    ) -> Result<()> {
        self.delete(space_id, file_id).await
    }
}

/// Builder for ranged, conditional, resized-image, and metadata file requests.
pub struct FileContentRequest<'a> {
    client: &'a AnytypeClient,
    space_id: String,
    file_id: String,
    width: Option<u32>,
    range: Option<String>,
    invalid_range: bool,
    if_match: Option<String>,
    if_none_match: Option<String>,
    if_modified_since: Option<String>,
    if_unmodified_since: Option<String>,
    if_range: Option<String>,
    response_limit_bytes: Option<u64>,
    error_limit_bytes: Option<u64>,
    header_evidence_limit_bytes: Option<u64>,
    max_attempts: Option<u32>,
}

impl FileContentRequest<'_> {
    /// Select a pre-rendered image variant at the given pixel width.
    ///
    /// The server ignores this option for non-image files. A width of zero
    /// requests the original image.
    #[must_use]
    pub fn width(mut self, width: u32) -> Self {
        self.width = Some(width);
        self
    }

    /// Set an HTTP byte range, for example `bytes=0-499` or `bytes=-500`.
    #[must_use]
    pub fn range(mut self, range: impl Into<String>) -> Self {
        self.range = Some(range.into());
        self.invalid_range = false;
        self
    }

    /// Select a checked inclusive byte range from `offset` for at most `length` bytes.
    ///
    /// A zero length and arithmetic overflow are rejected before network I/O.
    /// The response is still bounded independently with
    /// [`response_limit_bytes`](Self::response_limit_bytes), so callers that
    /// need an overrun sentinel can request `length + 1` body bytes there.
    #[must_use]
    pub fn byte_range(mut self, offset: u64, length: u64) -> Self {
        self.range = offset
            .checked_add(length.saturating_sub(1))
            .filter(|_| length != 0)
            .map(|end| format!("bytes={offset}-{end}"));
        self.invalid_range = self.range.is_none();
        self
    }

    /// Set the maximum successful response-body bytes buffered for this request.
    ///
    /// The value must be nonzero and cannot exceed the client's configured
    /// [`ResponseLimits::file_bytes`](crate::client::ResponseLimits::file_bytes).
    /// It does not change the client-wide default or any other request.
    #[must_use]
    pub const fn response_limit_bytes(mut self, limit: u64) -> Self {
        self.response_limit_bytes = Some(limit);
        self
    }

    /// Set the maximum error-response bytes buffered for this request.
    ///
    /// The value must be nonzero and cannot exceed the client's configured
    /// [`ResponseLimits::error_bytes`](crate::client::ResponseLimits::error_bytes).
    #[must_use]
    pub const fn error_limit_bytes(mut self, limit: u64) -> Self {
        self.error_limit_bytes = Some(limit);
        self
    }

    /// Set the retained evidence ceiling for allowlisted file response headers.
    ///
    /// Values are limited to [`MAX_FILE_HEADER_EVIDENCE_BYTES`]. The ceiling
    /// is enforced independently on every physical response before retry or
    /// body processing. Unrelated headers are never copied into the public
    /// result.
    #[must_use]
    pub const fn header_evidence_limit_bytes(mut self, limit: u64) -> Self {
        self.header_evidence_limit_bytes = Some(limit);
        self
    }

    /// Set the cumulative physical-attempt ceiling for this safe request.
    ///
    /// One through [`MAX_FILE_REQUEST_ATTEMPTS`] attempts are accepted. The
    /// initial send and every 429, retryable-status, connection, or timeout
    /// replay share this one counter. `POST` file uploads are unaffected.
    #[must_use]
    pub const fn max_attempts(mut self, max_attempts: u32) -> Self {
        self.max_attempts = Some(max_attempts);
        self
    }

    /// Set the `If-Match` precondition.
    #[must_use]
    pub fn if_match(mut self, value: impl Into<String>) -> Self {
        self.if_match = Some(value.into());
        self
    }

    /// Set the `If-None-Match` cache validator.
    #[must_use]
    pub fn if_none_match(mut self, value: impl Into<String>) -> Self {
        self.if_none_match = Some(value.into());
        self
    }

    /// Set the `If-Modified-Since` cache validator using an HTTP-date string.
    #[must_use]
    pub fn if_modified_since(mut self, value: impl Into<String>) -> Self {
        self.if_modified_since = Some(value.into());
        self
    }

    /// Set the `If-Unmodified-Since` precondition using an HTTP-date string.
    #[must_use]
    pub fn if_unmodified_since(mut self, value: impl Into<String>) -> Self {
        self.if_unmodified_since = Some(value.into());
        self
    }

    /// Set the `If-Range` validator for a ranged request.
    #[must_use]
    pub fn if_range(mut self, value: impl Into<String>) -> Self {
        self.if_range = Some(value.into());
        self
    }

    /// Execute an HTTP `GET`, preserving range and conditional statuses.
    pub async fn download(self) -> Result<FileContentResponse> {
        self.send(Method::GET).await
    }

    /// Execute an HTTP `HEAD`, returning headers without downloading the body.
    pub async fn head(self) -> Result<FileContentResponse> {
        self.send(Method::HEAD).await
    }

    async fn send(self, method: Method) -> Result<FileContentResponse> {
        let path = file_path(&self.space_id, &self.file_id);
        let query = self
            .width
            .map(|width| vec![("width".to_string(), width.to_string())])
            .unwrap_or_default();
        let mut headers = HeaderMap::new();
        if self.invalid_range {
            return invalid_range();
        }
        let requested_range = self.range.as_deref().map(parse_request_range).transpose()?;
        insert_optional_header(&mut headers, RANGE, self.range)?;
        insert_optional_header(&mut headers, IF_MATCH, self.if_match)?;
        insert_optional_header(&mut headers, IF_NONE_MATCH, self.if_none_match)?;
        insert_optional_header(&mut headers, IF_MODIFIED_SINCE, self.if_modified_since)?;
        insert_optional_header(&mut headers, IF_UNMODIFIED_SINCE, self.if_unmodified_since)?;
        insert_optional_header(&mut headers, IF_RANGE, self.if_range)?;

        let response_limit = self
            .response_limit_bytes
            .unwrap_or_else(|| self.client.client.file_response_limit());
        let error_limit = self
            .error_limit_bytes
            .unwrap_or_else(|| self.client.client.error_response_limit());
        let header_limit = self
            .header_evidence_limit_bytes
            .unwrap_or(DEFAULT_FILE_HEADER_EVIDENCE_BYTES);
        if header_limit == 0 || header_limit > MAX_FILE_HEADER_EVIDENCE_BYTES {
            return Err(AnytypeError::Validation {
                message: format!(
                    "file header evidence limit must be between 1 and {MAX_FILE_HEADER_EVIDENCE_BYTES} bytes"
                ),
            });
        }
        let max_attempts = self.max_attempts.unwrap_or(1);
        if max_attempts == 0 || max_attempts > MAX_FILE_REQUEST_ATTEMPTS {
            return Err(AnytypeError::Validation {
                message: format!(
                    "file request attempts must be between 1 and {MAX_FILE_REQUEST_ATTEMPTS}"
                ),
            });
        }

        let response = self
            .client
            .client
            .file_request_with_limits(
                method.clone(),
                &path,
                &query,
                headers,
                response_limit,
                error_limit,
                header_limit,
                max_attempts,
            )
            .await?;
        let metadata = file_http_metadata(
            &response.headers,
            response.status,
            method,
            requested_range,
            response.body.len() as u64,
            header_limit,
        )?;
        Ok(FileContentResponse {
            status: response.status,
            metadata,
            bytes: response.body,
        })
    }
}

/// Builder for soft or permanent REST file deletion.
pub struct FileDeleteRequest<'a> {
    client: &'a AnytypeClient,
    space_id: String,
    file_id: String,
    skip_bin: bool,
}

impl FileDeleteRequest<'_> {
    /// Set whether deletion bypasses the bin and permanently removes the file.
    #[must_use]
    pub fn skip_bin(mut self, skip_bin: bool) -> Self {
        self.skip_bin = skip_bin;
        self
    }

    /// Permanently remove the file instead of moving it to the bin.
    #[must_use]
    pub fn permanently(mut self) -> Self {
        self.skip_bin = true;
        self
    }

    /// Execute the deletion.
    pub async fn delete(self) -> Result<()> {
        let path = file_path(&self.space_id, &self.file_id);
        let query = if self.skip_bin {
            vec![("skip_bin".to_string(), "true".to_string())]
        } else {
            Vec::new()
        };
        self.client
            .client
            .file_request(Method::DELETE, &path, &query, HeaderMap::new())
            .await?;
        Ok(())
    }
}

fn file_path(space_id: &str, file_id: &str) -> String {
    format!("/v1/spaces/{space_id}/files/{file_id}")
}

fn insert_optional_header(
    headers: &mut HeaderMap,
    name: HeaderName,
    value: Option<String>,
) -> Result<()> {
    if let Some(value) = value {
        let value = HeaderValue::from_str(&value).map_err(|error| AnytypeError::Validation {
            message: format!("invalid {name} header: {error}"),
        })?;
        headers.insert(name, value);
    }
    Ok(())
}

#[derive(Debug, Clone, Copy)]
enum RequestRange {
    From {
        start: u64,
        inclusive_end: Option<u64>,
    },
    Suffix {
        length: u64,
    },
}

fn parse_request_range(value: &str) -> Result<RequestRange> {
    let Some(spec) = value.strip_prefix("bytes=") else {
        return invalid_range();
    };
    if spec.is_empty() || spec.contains(',') || spec.bytes().any(|byte| byte.is_ascii_whitespace())
    {
        return invalid_range();
    }
    let Some((start, end)) = spec.split_once('-') else {
        return invalid_range();
    };
    let range = match (start.is_empty(), end.is_empty()) {
        (false, false) => {
            let start = parse_canonical_u64(start).ok_or_else(invalid_range_error)?;
            let end = parse_canonical_u64(end).ok_or_else(invalid_range_error)?;
            if start > end {
                return invalid_range();
            }
            RequestRange::From {
                start,
                inclusive_end: Some(end),
            }
        }
        (false, true) => RequestRange::From {
            start: parse_canonical_u64(start).ok_or_else(invalid_range_error)?,
            inclusive_end: None,
        },
        (true, false) => {
            let suffix = parse_canonical_u64(end).ok_or_else(invalid_range_error)?;
            if suffix == 0 {
                return invalid_range();
            }
            RequestRange::Suffix { length: suffix }
        }
        (true, true) => return invalid_range(),
    };
    Ok(range)
}

fn invalid_range<T>() -> Result<T> {
    Err(invalid_range_error())
}

fn invalid_range_error() -> AnytypeError {
    AnytypeError::Validation {
        message: "file range must be one canonical bytes range".to_owned(),
    }
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

#[derive(Debug, Clone, Copy)]
struct ParsedContentRange {
    start: u64,
    end: u64,
    total: u64,
}

fn parse_content_range(value: &str) -> Option<ParsedContentRange> {
    let spec = value.strip_prefix("bytes ")?;
    let (range, total) = spec.split_once('/')?;
    let (start, end) = range.split_once('-')?;
    let start = parse_canonical_u64(start)?;
    let end = parse_canonical_u64(end)?;
    let total = parse_canonical_u64(total)?;
    if start > end || end >= total {
        return None;
    }
    Some(ParsedContentRange { start, end, total })
}

fn file_http_metadata(
    headers: &HeaderMap,
    status: StatusCode,
    method: Method,
    requested_range: Option<RequestRange>,
    body_len: u64,
    evidence_limit: u64,
) -> Result<FileHttpMetadata> {
    let retained_header_bytes = retained_file_header_bytes(headers, status, evidence_limit)?;
    let content_type = single_header(headers, status, CONTENT_TYPE, "content-type")?;
    let content_length = single_header(headers, status, CONTENT_LENGTH, "content-length")?
        .map(|value| {
            parse_canonical_u64(&value).ok_or(AnytypeError::InvalidFileResponseHeader {
                status: status.as_u16(),
                header: "content-length",
                issue: "malformed",
            })
        })
        .transpose()?;
    let content_range = single_header(headers, status, CONTENT_RANGE, "content-range")?;
    let accept_ranges = single_header(headers, status, ACCEPT_RANGES, "accept-ranges")?;
    let last_modified = single_header(headers, status, LAST_MODIFIED, "last-modified")?
        .map(|value| {
            httpdate::parse_http_date(&value)
                .map(httpdate::fmt_http_date)
                .map_err(|_| AnytypeError::InvalidFileResponseHeader {
                    status: status.as_u16(),
                    header: "last-modified",
                    issue: "malformed",
                })
        })
        .transpose()?;
    let etag = single_header(headers, status, ETAG, "etag")?
        .map(|value| validate_etag(value, status))
        .transpose()?;
    let cache_control = single_header(headers, status, CACHE_CONTROL, "cache-control")?;

    if let Some(value) = content_type.as_deref()
        && (value.len() > 255
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_graphic() || byte == b' ')
            || value.parse::<mime::Mime>().is_err())
    {
        return Err(AnytypeError::InvalidFileResponseHeader {
            status: status.as_u16(),
            header: "content-type",
            issue: "malformed",
        });
    }
    if let Some(value) = accept_ranges.as_deref()
        && value != "bytes"
        && value != "none"
    {
        return Err(AnytypeError::InvalidFileResponseHeader {
            status: status.as_u16(),
            header: "accept-ranges",
            issue: "unsupported",
        });
    }

    if method == Method::GET && status.is_success() {
        let declared = content_length.ok_or(AnytypeError::InvalidFileResponseHeader {
            status: status.as_u16(),
            header: "content-length",
            issue: "missing",
        })?;
        if declared != body_len {
            return Err(AnytypeError::InvalidFileResponseHeader {
                status: status.as_u16(),
                header: "content-length",
                issue: "body-length-mismatch",
            });
        }
    }

    if status == StatusCode::PARTIAL_CONTENT {
        let parsed = content_range
            .as_deref()
            .and_then(parse_content_range)
            .ok_or(AnytypeError::InvalidFileResponseHeader {
                status: status.as_u16(),
                header: "content-range",
                issue: if content_range.is_some() {
                    "malformed"
                } else {
                    "missing"
                },
            })?;
        let span = parsed
            .end
            .checked_sub(parsed.start)
            .and_then(|value| value.checked_add(1));
        if span != Some(body_len) {
            return Err(AnytypeError::InvalidFileResponseHeader {
                status: status.as_u16(),
                header: "content-range",
                issue: "body-length-mismatch",
            });
        }
        let Some(requested) = requested_range else {
            return Err(AnytypeError::InvalidFileResponseHeader {
                status: status.as_u16(),
                header: "content-range",
                issue: "unexpected",
            });
        };
        let request_matches = match requested {
            RequestRange::From {
                start,
                inclusive_end,
            } => {
                parsed.start == start
                    && inclusive_end.is_none_or(|requested_end| parsed.end <= requested_end)
            }
            RequestRange::Suffix { length } => {
                span.is_some_and(|span| span <= length)
                    && parsed.end.checked_add(1) == Some(parsed.total)
            }
        };
        if !request_matches || parsed.total == 0 {
            return Err(AnytypeError::InvalidFileResponseHeader {
                status: status.as_u16(),
                header: "content-range",
                issue: "request-mismatch",
            });
        }
    } else if content_range.is_some() && status.is_success() {
        return Err(AnytypeError::InvalidFileResponseHeader {
            status: status.as_u16(),
            header: "content-range",
            issue: "unexpected",
        });
    }

    Ok(FileHttpMetadata {
        content_type,
        content_length,
        content_range,
        accept_ranges,
        last_modified,
        etag,
        cache_control,
        retained_header_bytes,
    })
}

fn validate_etag(value: String, status: StatusCode) -> Result<String> {
    let opaque = value.strip_prefix("W/").unwrap_or(&value);
    if opaque.len() < 2
        || !opaque.starts_with('"')
        || !opaque.ends_with('"')
        || opaque[1..opaque.len() - 1]
            .bytes()
            .any(|byte| byte == b'"' || byte < 0x21 || byte == 0x7f)
    {
        return Err(AnytypeError::InvalidFileResponseHeader {
            status: status.as_u16(),
            header: "etag",
            issue: "malformed",
        });
    }
    Ok(value)
}

fn single_header(
    headers: &HeaderMap,
    status: StatusCode,
    name: HeaderName,
    display_name: &'static str,
) -> Result<Option<String>> {
    let values: Vec<_> = headers.get_all(&name).iter().collect();
    if values.len() > 1 {
        return Err(AnytypeError::InvalidFileResponseHeader {
            status: status.as_u16(),
            header: display_name,
            issue: "duplicate",
        });
    }
    values
        .first()
        .map(|value| {
            value
                .to_str()
                .map(str::to_owned)
                .map_err(|_| AnytypeError::InvalidFileResponseHeader {
                    status: status.as_u16(),
                    header: display_name,
                    issue: "non-utf8",
                })
        })
        .transpose()
}

pub(crate) fn retained_file_header_bytes(
    headers: &HeaderMap,
    status: StatusCode,
    limit: u64,
) -> Result<u64> {
    let allowlist = [
        CONTENT_LENGTH,
        CONTENT_RANGE,
        CONTENT_TYPE,
        ETAG,
        LAST_MODIFIED,
        ACCEPT_RANGES,
        CACHE_CONTROL,
    ];
    let mut retained = 0_u64;
    for name in allowlist {
        for value in headers.get_all(&name) {
            retained = retained
                .checked_add(name.as_str().len() as u64)
                .and_then(|value_len| value_len.checked_add(value.as_bytes().len() as u64 + 2))
                .ok_or(AnytypeError::FileHeaderEvidenceTooLarge {
                    limit,
                    status: status.as_u16(),
                })?;
            if retained > limit {
                return Err(AnytypeError::FileHeaderEvidenceTooLarge {
                    limit,
                    status: status.as_u16(),
                });
            }
        }
    }
    Ok(retained)
}

/// Builder for an HTTP (REST) file upload. Created by
/// [`FilesClient::http_upload`].
pub struct FileHttpUploadRequest<'a> {
    client: &'a AnytypeClient,
    space_id: String,
    file_name: Option<String>,
    mime: Option<String>,
    data: Option<Bytes>,
    source_path: Option<PathBuf>,
}

impl FileHttpUploadRequest<'_> {
    /// Set the file name reported to the server. When uploading from a
    /// [`path`](Self::path) this defaults to the path's file name.
    #[must_use]
    pub fn file_name(mut self, name: impl Into<String>) -> Self {
        self.file_name = Some(name.into());
        self
    }

    /// Set an explicit MIME type for the multipart part. When omitted the
    /// server infers it from the content and file name.
    #[must_use]
    pub fn mime(mut self, mime: impl Into<String>) -> Self {
        self.mime = Some(mime.into());
        self
    }

    /// Upload from an in-memory byte buffer.
    #[must_use]
    pub fn bytes(mut self, data: impl Into<Bytes>) -> Self {
        self.data = Some(data.into());
        self
    }

    /// Upload from a file on disk. The bytes are read when
    /// [`upload`](Self::upload) is called.
    #[must_use]
    pub fn path(mut self, path: impl AsRef<Path>) -> Self {
        self.source_path = Some(path.as_ref().to_path_buf());
        self
    }

    /// Perform the upload, returning the server's [`FileUploadResponse`].
    ///
    /// # Errors
    ///
    /// Returns an error if neither [`bytes`](Self::bytes) nor [`path`](Self::path)
    /// was set, if the source file cannot be read, or if the request fails.
    pub async fn upload(self) -> Result<FileUploadResponse> {
        http_upload_file(
            self.client,
            &self.space_id,
            self.data,
            self.source_path,
            self.file_name,
            self.mime,
        )
        .await
    }
}

// ============================================================================
// Request builders
// ============================================================================

pub struct FileListRequest<'a> {
    client: &'a AnytypeClient,
    space_id: String,
    filters: Vec<Filter>,
    limit: Option<u32>,
    offset: Option<u32>,
}

impl FileListRequest<'_> {
    /// list files with the text in the name
    #[must_use]
    pub fn name_contains(mut self, text: impl Into<String>) -> Self {
        self.filters.push(Filter::Text {
            condition: crate::filters::Condition::Contains,
            property_key: "name".to_string(),
            text: text.into(),
        });
        self
    }

    /// list files of a specific type
    #[must_use]
    pub fn file_type(mut self, file_type: &FileType) -> Self {
        if let Some(filter) = file_type_filter(file_type) {
            self.filters.push(filter);
        }
        self
    }

    /// List files with the extension
    #[must_use]
    pub fn extension(mut self, ext: impl Into<String>) -> Self {
        self.filters.push(Filter::Text {
            condition: crate::filters::Condition::Equal,
            property_key: "fileExt".to_string(),
            text: ext.into(),
        });
        self
    }

    /// List files with one of the extensions
    #[must_use]
    pub fn extension_in(mut self, extensions: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.filters.push(Filter::Select {
            condition: crate::filters::Condition::In,
            property_key: "fileExt".to_string(),
            select: extensions.into_iter().map(Into::into).collect(),
        });
        self
    }

    /// List files that don't have one of these extensions
    #[must_use]
    pub fn extension_not_in(
        mut self,
        extensions: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        self.filters.push(Filter::Select {
            condition: crate::filters::Condition::NotIn,
            property_key: "fileExt".to_string(),
            select: extensions.into_iter().map(Into::into).collect(),
        });
        self
    }

    /// list files with size
    #[must_use]
    pub fn size_eq(mut self, size: i64) -> Self {
        self.filters
            .push(size_filter(crate::filters::Condition::Equal, size));
        self
    }

    #[must_use]
    pub fn size_neq(mut self, size: i64) -> Self {
        self.filters
            .push(size_filter(crate::filters::Condition::NotEqual, size));
        self
    }

    #[must_use]
    pub fn size_lt(mut self, size: i64) -> Self {
        self.filters
            .push(size_filter(crate::filters::Condition::Less, size));
        self
    }

    #[must_use]
    pub fn size_lte(mut self, size: i64) -> Self {
        self.filters
            .push(size_filter(crate::filters::Condition::LessOrEqual, size));
        self
    }

    #[must_use]
    pub fn size_gt(mut self, size: i64) -> Self {
        self.filters
            .push(size_filter(crate::filters::Condition::Greater, size));
        self
    }

    #[must_use]
    pub fn size_gte(mut self, size: i64) -> Self {
        self.filters
            .push(size_filter(crate::filters::Condition::GreaterOrEqual, size));
        self
    }

    #[must_use]
    pub fn filter(mut self, filter: Filter) -> Self {
        self.filters.push(filter);
        self
    }

    #[must_use]
    pub fn limit(mut self, limit: u32) -> Self {
        self.limit = Some(limit);
        self
    }

    #[must_use]
    pub fn offset(mut self, offset: u32) -> Self {
        self.offset = Some(offset);
        self
    }

    pub async fn list(self) -> Result<PagedResult<FileObject>> {
        search_files(
            self.client,
            &self.space_id,
            None,
            self.filters,
            Vec::new(),
            self.limit,
            self.offset,
        )
        .await
    }
}

pub struct FileSearchRequest<'a> {
    client: &'a AnytypeClient,
    space_id: String,
    text: Option<String>,
    filters: Vec<Filter>,
    sorts: Vec<Sort>,
    limit: Option<u32>,
    offset: Option<u32>,
}

impl FileSearchRequest<'_> {
    #[must_use]
    pub fn text(mut self, text: impl Into<String>) -> Self {
        self.text = Some(text.into());
        self
    }

    #[must_use]
    pub fn name_contains(mut self, text: impl Into<String>) -> Self {
        self.filters.push(Filter::Text {
            condition: crate::filters::Condition::Contains,
            property_key: "name".to_string(),
            text: text.into(),
        });
        self
    }

    #[must_use]
    pub fn file_type(mut self, file_type: &FileType) -> Self {
        if let Some(filter) = file_type_filter(file_type) {
            self.filters.push(filter);
        }
        self
    }

    #[must_use]
    pub fn extension(mut self, ext: impl Into<String>) -> Self {
        self.filters.push(Filter::Text {
            condition: crate::filters::Condition::Equal,
            property_key: "fileExt".to_string(),
            text: ext.into(),
        });
        self
    }

    #[must_use]
    pub fn extension_in(mut self, extensions: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.filters.push(Filter::Select {
            condition: crate::filters::Condition::In,
            property_key: "fileExt".to_string(),
            select: extensions.into_iter().map(Into::into).collect(),
        });
        self
    }

    #[must_use]
    pub fn extension_not_in(
        mut self,
        extensions: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        self.filters.push(Filter::Select {
            condition: crate::filters::Condition::NotIn,
            property_key: "fileExt".to_string(),
            select: extensions.into_iter().map(Into::into).collect(),
        });
        self
    }

    #[must_use]
    pub fn size_eq(mut self, size: i64) -> Self {
        self.filters
            .push(size_filter(crate::filters::Condition::Equal, size));
        self
    }

    #[must_use]
    pub fn size_neq(mut self, size: i64) -> Self {
        self.filters
            .push(size_filter(crate::filters::Condition::NotEqual, size));
        self
    }

    #[must_use]
    pub fn size_lt(mut self, size: i64) -> Self {
        self.filters
            .push(size_filter(crate::filters::Condition::Less, size));
        self
    }

    #[must_use]
    pub fn size_lte(mut self, size: i64) -> Self {
        self.filters
            .push(size_filter(crate::filters::Condition::LessOrEqual, size));
        self
    }

    #[must_use]
    pub fn size_gt(mut self, size: i64) -> Self {
        self.filters
            .push(size_filter(crate::filters::Condition::Greater, size));
        self
    }

    #[must_use]
    pub fn size_gte(mut self, size: i64) -> Self {
        self.filters
            .push(size_filter(crate::filters::Condition::GreaterOrEqual, size));
        self
    }

    #[must_use]
    pub fn filter(mut self, filter: Filter) -> Self {
        self.filters.push(filter);
        self
    }

    #[must_use]
    pub fn sort_asc(mut self, key: impl Into<String>) -> Self {
        self.sorts.push(Sort::asc(key));
        self
    }

    #[must_use]
    pub fn sort_desc(mut self, key: impl Into<String>) -> Self {
        self.sorts.push(Sort::desc(key));
        self
    }

    #[must_use]
    pub fn limit(mut self, limit: u32) -> Self {
        self.limit = Some(limit);
        self
    }

    #[must_use]
    pub fn offset(mut self, offset: u32) -> Self {
        self.offset = Some(offset);
        self
    }

    pub async fn search(self) -> Result<PagedResult<FileObject>> {
        search_files(
            self.client,
            &self.space_id,
            self.text,
            self.filters,
            self.sorts,
            self.limit,
            self.offset,
        )
        .await
    }
}

pub struct FileGetRequest<'a> {
    client: &'a AnytypeClient,
    space_id: String,
    object_id: String,
}

impl FileGetRequest<'_> {
    pub async fn get(self) -> Result<FileObject> {
        let grpc = self.client.grpc_client().await?;
        let mut commands = grpc.client_commands();
        let request = search_with_meta::Request {
            space_id: self.space_id.clone(),
            filters: vec![filter_id_equal(&self.object_id)],
            sorts: Vec::new(),
            full_text: String::new(),
            offset: 0,
            limit: 1,
            object_type_filter: Vec::new(),
            keys: Vec::new(),
            return_meta: false,
            return_meta_relation_details: false,
            return_html_highlights_instead_of_ranges: false,
        };
        let request = with_token_request(Request::new(request), grpc.token())?;
        let response = commands
            .object_search_with_meta(request)
            .await
            .map_err(grpc_status)?
            .into_inner();
        ensure_error_ok(response.error.as_ref(), "file get")?;
        let result = response
            .results
            .first()
            .ok_or_else(|| AnytypeError::Other {
                message: "file not found".to_string(),
            })?;
        let details = result.details.as_ref().ok_or_else(|| AnytypeError::Other {
            message: "file result missing details".to_string(),
        })?;
        Ok(file_from_details(
            &self.space_id,
            &result.object_id,
            details,
        ))
    }
}

pub struct FileDownloadRequest<'a> {
    client: &'a AnytypeClient,
    object_id: String,
    destination: Option<FileDownloadDestination>,
}

#[derive(Debug, Clone)]
enum FileDownloadDestination {
    Dir(PathBuf),
    File(PathBuf),
}

impl FileDownloadRequest<'_> {
    /// set the destination directory for the download
    #[must_use]
    pub fn to_path(mut self, path: impl AsRef<Path>) -> Self {
        self.destination = Some(FileDownloadDestination::Dir(path.as_ref().to_path_buf()));
        self
    }

    /// set the destination directory for the download
    #[must_use]
    pub fn to_dir(mut self, path: impl AsRef<Path>) -> Self {
        self.destination = Some(FileDownloadDestination::Dir(path.as_ref().to_path_buf()));
        self
    }

    /// set the destination file path for the download
    #[must_use]
    pub fn to_file(mut self, path: impl AsRef<Path>) -> Self {
        self.destination = Some(FileDownloadDestination::File(path.as_ref().to_path_buf()));
        self
    }

    /// Download the file. Returns the path to the file
    pub async fn download(self) -> Result<PathBuf> {
        debug!("enter download execute");
        let (request_path, target_file) = match self.destination {
            Some(FileDownloadDestination::Dir(path)) => (path, None),
            Some(FileDownloadDestination::File(path)) => {
                if path.is_dir() {
                    return Err(AnytypeError::Validation {
                        message: format!("download destination is a directory: {}", path.display()),
                    });
                }
                let parent = path
                    .parent()
                    .filter(|value| !value.as_os_str().is_empty())
                    .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
                if let Err(err) = std::fs::create_dir_all(&parent) {
                    return Err(AnytypeError::Other {
                        message: format!("create download directory {}: {err}", parent.display()),
                    });
                }
                (parent, Some(path))
            }
            None => (PathBuf::new(), None),
        };
        let grpc = self.client.grpc_client().await?;
        let mut commands = grpc.client_commands();
        let request = download::Request {
            object_id: self.object_id.clone(),
            path: if request_path.as_os_str().is_empty() {
                String::new()
            } else {
                request_path.to_string_lossy().to_string()
            },
        };
        let request = with_token_request(Request::new(request), grpc.token()).map_err(|err| {
            error!("download rpc error: {err}");
            err
        })?;

        let response = commands
            .file_download(request)
            .await
            .map_err(|err| {
                error!("download error grpc_status {err:?}");
                grpc_status(err)
            })?
            .into_inner();

        // remove partial files if there was an error
        if let Err(err) = ensure_error_ok(response.error.as_ref(), "file download") {
            let local = PathBuf::from(response.local_path);
            if local.is_file() {
                info!("download error {err}. Removing incomplete download {local:?}");
                if let Err(delete_err) = std::fs::remove_file(&local) {
                    error!(
                        "failed to remove incomplete download {local:?} (err={delete_err}) after download error {err}"
                    );
                }
            } else {
                error!("download error {err}");
            }
            return Err(err);
        }
        let mut local_path = PathBuf::from(response.local_path);
        if let Some(target_path) = target_file {
            if target_path.is_dir() {
                return Err(AnytypeError::Validation {
                    message: format!(
                        "download file path points to a directory: {}",
                        target_path.display()
                    ),
                });
            }
            if local_path != target_path {
                if let Err(err) = std::fs::rename(&local_path, &target_path) {
                    if let Err(copy_err) = std::fs::copy(&local_path, &target_path) {
                        return Err(AnytypeError::Other {
                            message: format!(
                                "move download to {}: {err} (copy error: {copy_err})",
                                target_path.display()
                            ),
                        });
                    }
                    if let Err(remove_err) = std::fs::remove_file(&local_path) {
                        error!(
                            "failed to remove original download {local_path:?} after copy: {remove_err}"
                        );
                    }
                }
                local_path = target_path;
            }
        }
        debug!("download complete 536 {}", &local_path.display());
        Ok(local_path)
    }
}

/// Unified file-upload builder.
///
/// Path and byte uploads without rich options use REST. URL uploads and
/// requests with file type, style, details, or creation context use gRPC.
pub struct FileUploadRequest<'a> {
    client: &'a AnytypeClient,
    space_id: String,
    source: Option<FileSource>,
    file_type: Option<FileType>,
    style: Option<FileStyle>,
    details: Option<serde_json::Value>,
    created_in_context: Option<String>,
    created_in_context_ref: Option<String>,
    file_name: Option<String>,
    mime: Option<String>,
}

impl FileUploadRequest<'_> {
    #[must_use]
    pub fn from_path(mut self, path: impl AsRef<Path>) -> Self {
        self.source = Some(FileSource::Path(path.as_ref().to_path_buf()));
        self
    }

    #[must_use]
    pub fn from_url(mut self, url: impl Into<String>) -> Self {
        self.source = Some(FileSource::Url(url.into()));
        self
    }

    /// Upload an in-memory file. Simple byte and path uploads use REST.
    #[must_use]
    pub fn bytes(mut self, file_name: impl Into<String>, data: impl Into<Bytes>) -> Self {
        self.file_name = Some(file_name.into());
        self.source = Some(FileSource::Bytes(data.into()));
        self
    }

    /// Set the MIME type used by a REST upload.
    #[must_use]
    pub fn mime(mut self, mime: impl Into<String>) -> Self {
        self.mime = Some(mime.into());
        self
    }

    #[must_use]
    pub fn file_type(mut self, file_type: FileType) -> Self {
        self.file_type = Some(file_type);
        self
    }

    #[must_use]
    pub fn style(mut self, style: FileStyle) -> Self {
        self.style = Some(style);
        self
    }

    #[must_use]
    pub fn details(mut self, details: serde_json::Value) -> Self {
        self.details = Some(details);
        self
    }

    #[must_use]
    pub fn created_in_context(mut self, object_id: impl Into<String>) -> Self {
        self.created_in_context = Some(object_id.into());
        self
    }

    #[must_use]
    pub fn created_in_context_ref(mut self, block_id: impl Into<String>) -> Self {
        self.created_in_context_ref = Some(block_id.into());
        self
    }

    /// Upload the file through the least-capable backend that preserves every
    /// requested option, returning a normalized [`FileObject`].
    pub async fn upload(self) -> Result<FileObject> {
        if self.uses_rest() {
            let (data, source_path) = match self.source {
                Some(FileSource::Bytes(data)) => (Some(data), None),
                Some(FileSource::Path(path)) => (None, Some(path)),
                Some(FileSource::Url(_)) | None => unreachable!("REST backend selection"),
            };
            let response = http_upload_file(
                self.client,
                &self.space_id,
                data,
                source_path,
                self.file_name,
                self.mime,
            )
            .await?;
            return Ok(file_from_http_upload(&self.space_id, response));
        }

        let result = upload_file(
            self.client,
            &self.space_id,
            self.source,
            self.file_type,
            self.style,
            self.details,
            self.created_in_context,
            self.created_in_context_ref,
            false,
            None,
        )
        .await?;
        Ok(file_from_details(
            &self.space_id,
            &result.object_id,
            &result.details,
        ))
    }

    fn uses_rest(&self) -> bool {
        upload_uses_rest(
            self.source.as_ref(),
            self.file_type.is_some()
                || self.style.is_some()
                || self.details.is_some()
                || self.created_in_context.is_some()
                || self.created_in_context_ref.is_some(),
        )
    }
}

pub struct FilePreloadRequest<'a> {
    client: &'a AnytypeClient,
    space_id: String,
    source: Option<FileSource>,
    file_type: Option<FileType>,
    created_in_context: Option<String>,
    created_in_context_ref: Option<String>,
}

impl FilePreloadRequest<'_> {
    #[must_use]
    pub fn from_path(mut self, path: impl AsRef<Path>) -> Self {
        self.source = Some(FileSource::Path(path.as_ref().to_path_buf()));
        self
    }

    /// Preload a file fetched from a remote URL.
    ///
    /// Preloading always runs over gRPC, so a URL source is uploaded the same
    /// way the unified upload builder handles [`FileUploadRequest::from_url`].
    #[must_use]
    pub fn from_url(mut self, url: impl Into<String>) -> Self {
        self.source = Some(FileSource::Url(url.into()));
        self
    }

    #[must_use]
    pub fn file_type(mut self, file_type: FileType) -> Self {
        self.file_type = Some(file_type);
        self
    }

    #[must_use]
    pub fn created_in_context(mut self, object_id: impl Into<String>) -> Self {
        self.created_in_context = Some(object_id.into());
        self
    }

    #[must_use]
    pub fn created_in_context_ref(mut self, block_id: impl Into<String>) -> Self {
        self.created_in_context_ref = Some(block_id.into());
        self
    }

    pub async fn preload(self) -> Result<String> {
        let result = upload_file(
            self.client,
            &self.space_id,
            self.source,
            self.file_type,
            None,
            None,
            self.created_in_context,
            self.created_in_context_ref,
            true,
            None,
        )
        .await?;
        Ok(result.preload_file_id)
    }
}

pub struct FileDiscardPreloadRequest<'a> {
    client: &'a AnytypeClient,
    space_id: String,
    file_id: String,
}

impl FileDiscardPreloadRequest<'_> {
    pub async fn discard(self) -> Result<()> {
        let grpc = self.client.grpc_client().await?;
        let mut commands = grpc.client_commands();
        let request = discard_preload::Request {
            file_id: self.file_id,
            space_id: self.space_id,
        };
        let request = with_token_request(Request::new(request), grpc.token())?;
        let response = commands
            .file_discard_preload(request)
            .await
            .map_err(grpc_status)?
            .into_inner();
        ensure_error_ok(response.error.as_ref(), "file discard preload")?;
        Ok(())
    }
}

// ============================================================================
// Internal helpers
// ============================================================================

#[derive(Debug)]
enum FileSource {
    Url(String),
    Path(PathBuf),
    Bytes(Bytes),
}

fn upload_uses_rest(source: Option<&FileSource>, has_rich_options: bool) -> bool {
    matches!(source, Some(FileSource::Path(_) | FileSource::Bytes(_))) && !has_rich_options
}

async fn http_upload_file(
    client: &AnytypeClient,
    space_id: &str,
    data: Option<Bytes>,
    source_path: Option<PathBuf>,
    file_name: Option<String>,
    mime: Option<String>,
) -> Result<FileUploadResponse> {
    let (bytes, name) = match (data, source_path) {
        (Some(data), path) => (
            data,
            file_name.or_else(|| {
                path.as_ref()
                    .and_then(|path| path.file_name())
                    .and_then(|name| name.to_str())
                    .map(String::from)
            }),
        ),
        (None, Some(path)) => {
            let data = tokio::fs::read(&path)
                .await
                .map_err(|err| AnytypeError::Other {
                    message: format!("failed to read {}: {err}", path.display()),
                })?;
            let name = file_name.or_else(|| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .map(String::from)
            });
            (Bytes::from(data), name)
        }
        (None, None) => {
            return Err(AnytypeError::Validation {
                message: "file upload requires bytes or a path".to_string(),
            });
        }
    };

    let mut part = reqwest::multipart::Part::bytes(bytes.to_vec())
        .file_name(name.unwrap_or_else(|| "file".to_string()));
    if let Some(mime) = mime {
        part = part
            .mime_str(&mime)
            .map_err(|err| AnytypeError::Validation {
                message: format!("invalid mime type: {err}"),
            })?;
    }
    let form = reqwest::multipart::Form::new().part("file", part);
    let path = format!("/v1/spaces/{space_id}/files");
    client.client.post_multipart(&path, form).await
}

fn file_from_http_upload(space_id: &str, response: FileUploadResponse) -> FileObject {
    let file_type = response
        .media
        .as_deref()
        .map(file_type_from_mime)
        .unwrap_or_default();
    FileObject {
        id: response.object_id,
        space_id: space_id.to_string(),
        name: response.name,
        size: response.size_in_bytes,
        mime: response.media,
        added_at: None,
        file_type,
        style: FileStyle::Auto,
        target_object_id: None,
        details: serde_json::Value::Null,
    }
}

async fn search_files(
    client: &AnytypeClient,
    space_id: &str,
    text: Option<String>,
    filters: Vec<Filter>,
    sorts: Vec<Sort>,
    limit: Option<u32>,
    offset: Option<u32>,
) -> Result<PagedResult<FileObject>> {
    let grpc = client.grpc_client().await?;
    let mut commands = grpc.client_commands();

    let mut grpc_filters = Vec::with_capacity(filters.len() + 1);
    grpc_filters.push(filter_not_empty("fileId"));
    for filter in filters {
        grpc_filters.push(filter_to_dataview(filter)?);
    }

    let mut grpc_sorts = Vec::with_capacity(sorts.len());
    for sort in sorts {
        grpc_sorts.push(sort_to_dataview(sort));
    }

    #[allow(clippy::cast_possible_wrap)] // u32 to i32 for offset and limit
    let request = search_with_meta::Request {
        space_id: space_id.to_string(),
        filters: grpc_filters,
        sorts: grpc_sorts,
        full_text: text.unwrap_or_default(),
        offset: offset.unwrap_or_default() as i32,
        limit: limit.unwrap_or(100) as i32,
        object_type_filter: Vec::new(),
        keys: Vec::new(),
        return_meta: false,
        return_meta_relation_details: false,
        return_html_highlights_instead_of_ranges: false,
    };

    let request = with_token_request(Request::new(request), grpc.token())?;
    let response = commands
        .object_search_with_meta(request)
        .await
        .map_err(grpc_status)?
        .into_inner();
    ensure_error_ok(response.error.as_ref(), "file search")?;

    let items: Vec<FileObject> = response
        .results
        .into_iter()
        .filter_map(|result| {
            let details = result.details.as_ref()?;
            Some(file_from_details(space_id, &result.object_id, details))
        })
        .collect();

    let limit_value = limit.unwrap_or(100);
    let has_more = items.len() == limit_value as usize;
    let total = offset.unwrap_or_default() as usize + items.len();
    let response = PaginatedResponse {
        items,
        pagination: PaginationMeta {
            has_more,
            limit: limit_value,
            offset: offset.unwrap_or_default(),
            total,
        },
    };
    Ok(PagedResult::from_response(response))
}

struct UploadResult {
    object_id: String,
    preload_file_id: String,
    details: Struct,
}

#[allow(clippy::too_many_arguments)]
async fn upload_file(
    client: &AnytypeClient,
    space_id: &str,
    source: Option<FileSource>,
    file_type: Option<FileType>,
    style: Option<FileStyle>,
    details: Option<serde_json::Value>,
    created_in_context: Option<String>,
    created_in_context_ref: Option<String>,
    preload_only: bool,
    preload_file_id: Option<String>,
) -> Result<UploadResult> {
    let source = source.ok_or_else(|| AnytypeError::Validation {
        message: "file upload requires a source (path or url)".to_string(),
    })?;

    let grpc = client.grpc_client().await?;
    let mut commands = grpc.client_commands();
    let (url, local_path) = match source {
        FileSource::Url(url) => (url, String::new()),
        FileSource::Path(path) => (String::new(), path.to_string_lossy().to_string()),
        FileSource::Bytes(_) => {
            return Err(AnytypeError::Validation {
                message: "in-memory uploads are only supported by the REST backend".to_string(),
            });
        }
    };

    let request = upload::Request {
        space_id: space_id.to_string(),
        url,
        local_path,
        r#type: grpc_file_type(&file_type.unwrap_or(FileType::File)),
        disable_encryption: false,
        style: grpc_file_style(&style.unwrap_or(FileStyle::Auto)),
        details: details.map(json_to_struct).transpose()?,
        origin: 0,
        image_kind: 0,
        preload_only,
        preload_file_id: preload_file_id.unwrap_or_default(),
        created_in_context: created_in_context.unwrap_or_default(),
        created_in_context_ref: created_in_context_ref.unwrap_or_default(),
    };
    let request = with_token_request(Request::new(request), grpc.token())?;
    let response = commands
        .file_upload(request)
        .await
        .map_err(grpc_status)?
        .into_inner();
    ensure_error_ok(response.error.as_ref(), "file upload")?;
    let details = response.details.unwrap_or_default();
    Ok(UploadResult {
        object_id: response.object_id,
        preload_file_id: response.preload_file_id,
        details,
    })
}

fn grpc_file_type(file_type: &FileType) -> i32 {
    match file_type {
        &FileType::File | &FileType::Other(_) => model::block::content::file::Type::File as i32,
        &FileType::Image => model::block::content::file::Type::Image as i32,
        &FileType::Video => model::block::content::file::Type::Video as i32,
        &FileType::Audio => model::block::content::file::Type::Audio as i32,
        &FileType::Pdf => model::block::content::file::Type::Pdf as i32,
    }
}

fn grpc_file_style(style: &FileStyle) -> i32 {
    match style {
        FileStyle::Auto => model::block::content::file::Style::Auto as i32,
        FileStyle::Link => model::block::content::file::Style::Link as i32,
        FileStyle::Embed => model::block::content::file::Style::Embed as i32,
    }
}

fn filter_not_empty(key: &str) -> model::block::content::dataview::Filter {
    model::block::content::dataview::Filter {
        id: String::new(),
        operator: model::block::content::dataview::filter::Operator::No as i32,
        relation_key: key.to_string(),
        relation_property: String::new(),
        condition: model::block::content::dataview::filter::Condition::NotEmpty as i32,
        value: None,
        quick_option: model::block::content::dataview::filter::QuickOption::ExactDate as i32,
        format: 0,
        include_time: false,
        nested_filters: Vec::new(),
    }
}

fn filter_id_equal(id: &str) -> model::block::content::dataview::Filter {
    model::block::content::dataview::Filter {
        id: String::new(),
        operator: model::block::content::dataview::filter::Operator::No as i32,
        relation_key: "id".to_string(),
        relation_property: String::new(),
        condition: model::block::content::dataview::filter::Condition::Equal as i32,
        value: Some(value_string(id.to_string())),
        quick_option: model::block::content::dataview::filter::QuickOption::ExactDate as i32,
        format: 0,
        include_time: false,
        nested_filters: Vec::new(),
    }
}

#[allow(clippy::too_many_lines)]
fn filter_to_dataview(filter: Filter) -> Result<model::block::content::dataview::Filter> {
    let (relation_key, condition, value) = match filter {
        Filter::Text {
            condition,
            property_key,
            text: str,
        }
        | Filter::Date {
            condition,
            property_key,
            date: str,
        }
        | Filter::Url {
            condition,
            property_key,
            url: str,
        }
        | Filter::Email {
            condition,
            property_key,
            email: str,
        }
        | Filter::Phone {
            condition,
            property_key,
            phone: str,
        } => (property_key, condition, Some(value_string(str))),
        Filter::Number {
            condition,
            property_key,
            number,
        } => {
            let number = number.as_f64().ok_or_else(|| AnytypeError::Validation {
                message: "number filter must be numeric".to_string(),
            })?;
            (property_key, condition, Some(value_number(number)))
        }
        Filter::Select {
            condition,
            property_key,
            select,
        } => (
            property_key,
            condition,
            Some(value_list(select.into_iter().map(value_string).collect())),
        ),
        Filter::MultiSelect {
            condition,
            property_key,
            multi_select,
        } => (
            property_key,
            condition,
            Some(value_list(
                multi_select.into_iter().map(value_string).collect(),
            )),
        ),
        Filter::Checkbox {
            condition,
            property_key,
            checkbox,
        } => (property_key, condition, Some(value_bool(checkbox))),
        Filter::Files {
            condition,
            property_key,
            files,
        } => (
            property_key,
            condition,
            Some(value_list(files.into_iter().map(value_string).collect())),
        ),
        Filter::Objects {
            condition,
            property_key,
            objects,
        } => (
            property_key,
            condition,
            Some(value_list(objects.into_iter().map(value_string).collect())),
        ),
        Filter::Empty {
            condition,
            property_key,
        }
        | Filter::NotEmpty {
            condition,
            property_key,
        } => (property_key, condition, None),
        Filter::Value {
            condition,
            property_key,
            value,
        } => (
            property_key,
            condition,
            value.map(json_value_to_prost).transpose()?,
        ),
    };

    Ok(model::block::content::dataview::Filter {
        id: String::new(),
        operator: model::block::content::dataview::filter::Operator::No as i32,
        relation_key,
        relation_property: String::new(),
        condition: grpc_filter_condition(condition),
        value,
        quick_option: model::block::content::dataview::filter::QuickOption::ExactDate as i32,
        format: 0,
        include_time: false,
        nested_filters: Vec::new(),
    })
}

fn grpc_filter_condition(condition: crate::filters::Condition) -> i32 {
    use model::block::content::dataview::filter::Condition as GrpcCondition;

    use crate::filters::Condition;

    match condition {
        Condition::None => GrpcCondition::None as i32,
        Condition::Equal => GrpcCondition::Equal as i32,
        Condition::NotEqual => GrpcCondition::NotEqual as i32,
        Condition::Greater => GrpcCondition::Greater as i32,
        Condition::Less => GrpcCondition::Less as i32,
        Condition::GreaterOrEqual => GrpcCondition::GreaterOrEqual as i32,
        Condition::LessOrEqual => GrpcCondition::LessOrEqual as i32,
        Condition::Contains => GrpcCondition::Like as i32,
        Condition::NotContains => GrpcCondition::NotLike as i32,
        Condition::In => GrpcCondition::In as i32,
        Condition::NotIn => GrpcCondition::NotIn as i32,
        Condition::Empty => GrpcCondition::Empty as i32,
        Condition::NotEmpty => GrpcCondition::NotEmpty as i32,
        Condition::All | Condition::AllIn => GrpcCondition::AllIn as i32,
        Condition::NotAllIn => GrpcCondition::NotAllIn as i32,
        Condition::ExactIn => GrpcCondition::ExactIn as i32,
        Condition::NotExactIn => GrpcCondition::NotExactIn as i32,
        Condition::Exists => GrpcCondition::Exists as i32,
    }
}

fn sort_to_dataview(sort: Sort) -> model::block::content::dataview::Sort {
    let sort_type = match sort.direction {
        SortDirection::Asc => model::block::content::dataview::sort::Type::Asc,
        SortDirection::Desc => model::block::content::dataview::sort::Type::Desc,
    };

    model::block::content::dataview::Sort {
        relation_key: sort.property_key,
        r#type: sort_type as i32,
        custom_order: Vec::new(),
        format: 0,
        include_time: false,
        id: String::new(),
        empty_placement: 0,
        no_collate: false,
    }
}

fn file_from_details(space_id: &str, object_id: &str, details: &Struct) -> FileObject {
    let name = string_field(details, "name");
    #[allow(clippy::cast_possible_truncation)]
    let size = number_field(details, "sizeInBytes").map(|val| val as i64);
    let mime = string_field(details, "fileMimeType");
    let added_at = string_field(details, "addedDate")
        .and_then(|value| DateTime::parse_from_rfc3339(&value).ok());
    let target_object_id = string_field(details, "targetObjectId");
    let file_type = mime.as_deref().map(file_type_from_mime).unwrap_or_default();

    FileObject {
        id: object_id.to_string(),
        space_id: space_id.to_string(),
        name,
        size,
        mime,
        added_at,
        file_type,
        style: FileStyle::Auto,
        target_object_id,
        details: struct_to_json(details),
    }
}

fn file_type_from_mime(mime: &str) -> FileType {
    if mime.starts_with("image/") {
        return FileType::Image;
    }
    if mime.starts_with("video/") {
        return FileType::Video;
    }
    if mime.starts_with("audio/") {
        return FileType::Audio;
    }
    if mime == "application/pdf" {
        return FileType::Pdf;
    }
    FileType::File
}

fn size_filter(condition: crate::filters::Condition, size: i64) -> Filter {
    Filter::Number {
        condition,
        property_key: "sizeInBytes".to_string(),
        number: serde_json::Number::from(size),
    }
}

fn file_type_filter(file_type: &FileType) -> Option<Filter> {
    let (condition, value) = match file_type {
        FileType::Image => (crate::filters::Condition::Contains, "image/".to_string()),
        FileType::Video => (crate::filters::Condition::Contains, "video/".to_string()),
        FileType::Audio => (crate::filters::Condition::Contains, "audio/".to_string()),
        FileType::Pdf => (
            crate::filters::Condition::Equal,
            "application/pdf".to_string(),
        ),
        FileType::File | FileType::Other(_) => return None,
    };

    Some(Filter::Text {
        condition,
        property_key: "fileMimeType".to_string(),
        text: value,
    })
}

fn string_field(details: &Struct, key: &str) -> Option<String> {
    details.fields.get(key).and_then(|value| match &value.kind {
        Some(prost_types::value::Kind::StringValue(value)) => Some(value.clone()),
        _ => None,
    })
}

fn number_field(details: &Struct, key: &str) -> Option<f64> {
    details.fields.get(key).and_then(|value| match &value.kind {
        Some(prost_types::value::Kind::NumberValue(value)) => Some(*value),
        _ => None,
    })
}

fn struct_to_json(details: &Struct) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    for (key, value) in &details.fields {
        map.insert(key.clone(), prost_value_to_json(value));
    }
    serde_json::Value::Object(map)
}

fn prost_value_to_json(value: &Value) -> serde_json::Value {
    match &value.kind {
        Some(prost_types::value::Kind::NullValue(_)) | None => serde_json::Value::Null,
        Some(prost_types::value::Kind::NumberValue(value)) => {
            Number::from_f64(*value).map_or(serde_json::Value::Null, serde_json::Value::Number)
        }
        Some(prost_types::value::Kind::StringValue(value)) => {
            serde_json::Value::String(value.clone())
        }
        Some(prost_types::value::Kind::BoolValue(value)) => serde_json::Value::Bool(*value),
        Some(prost_types::value::Kind::StructValue(value)) => struct_to_json(value),
        Some(prost_types::value::Kind::ListValue(value)) => {
            serde_json::Value::Array(value.values.iter().map(prost_value_to_json).collect())
        }
    }
}

fn json_to_struct(value: serde_json::Value) -> Result<Struct> {
    match json_value_to_prost(value)? {
        Value {
            kind: Some(prost_types::value::Kind::StructValue(value)),
        } => Ok(value),
        _ => Err(AnytypeError::Validation {
            message: "details must be an object".to_string(),
        }),
    }
}

fn json_value_to_prost(value: serde_json::Value) -> Result<Value> {
    Ok(match value {
        serde_json::Value::Null => Value {
            kind: Some(prost_types::value::Kind::NullValue(0)),
        },
        serde_json::Value::Bool(value) => Value {
            kind: Some(prost_types::value::Kind::BoolValue(value)),
        },
        serde_json::Value::Number(value) => Value {
            kind: Some(prost_types::value::Kind::NumberValue(
                value.as_f64().unwrap_or_default(),
            )),
        },
        serde_json::Value::String(value) => Value {
            kind: Some(prost_types::value::Kind::StringValue(value)),
        },
        serde_json::Value::Array(values) => Value {
            kind: Some(prost_types::value::Kind::ListValue(ListValue {
                values: values
                    .into_iter()
                    .map(json_value_to_prost)
                    .collect::<Result<Vec<_>>>()?,
            })),
        },
        serde_json::Value::Object(map) => Value {
            kind: Some(prost_types::value::Kind::StructValue(Struct {
                fields: map
                    .into_iter()
                    .map(|(key, value)| Ok((key, json_value_to_prost(value)?)))
                    .collect::<Result<_>>()?,
            })),
        },
    })
}

fn value_string(value: impl Into<String>) -> Value {
    Value {
        kind: Some(prost_types::value::Kind::StringValue(value.into())),
    }
}

fn value_number(value: f64) -> Value {
    Value {
        kind: Some(prost_types::value::Kind::NumberValue(value)),
    }
}

fn value_bool(value: bool) -> Value {
    Value {
        kind: Some(prost_types::value::Kind::BoolValue(value)),
    }
}

fn value_list(values: Vec<Value>) -> Value {
    Value {
        kind: Some(prost_types::value::Kind::ListValue(ListValue { values })),
    }
}

#[cfg(test)]
mod tests {
    use std::{
        path::PathBuf,
        sync::atomic::{AtomicU64, Ordering},
    };

    use bytes::Bytes;
    use reqwest::StatusCode;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
        task::JoinHandle,
    };

    use super::{
        FileSource, FileStyle, FileType, FileUploadResponse, file_from_http_upload,
        upload_uses_rest,
    };
    use crate::{
        client::{AnytypeClient, ClientConfig},
        keystore::HttpCredentials,
    };

    static NEXT_MOCK_ID: AtomicU64 = AtomicU64::new(1);

    async fn mock_file_client(response: &'static str) -> (AnytypeClient, JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock file server");
        let address = listener.local_addr().expect("mock server address");
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept mock request");
            let mut request = Vec::new();
            let mut chunk = [0_u8; 1024];
            loop {
                let read = stream.read(&mut chunk).await.expect("read mock request");
                if read == 0 {
                    break;
                }
                request.extend_from_slice(&chunk[..read]);
                if request.windows(4).any(|window| window == b"\r\n\r\n") {
                    break;
                }
            }
            stream
                .write_all(response.as_bytes())
                .await
                .expect("write mock response");
            String::from_utf8(request).expect("HTTP request is UTF-8")
        });

        let id = NEXT_MOCK_ID.fetch_add(1, Ordering::Relaxed);
        let key_path = std::env::temp_dir().join(format!(
            "anytype-file-http-unit-{}-{id}.db",
            std::process::id()
        ));
        let mut config = ClientConfig::default().app_name("file-http-unit");
        config.base_url = Some(format!("http://{address}"));
        config.keystore = Some(format!("file:path={}", key_path.display()));
        config.keystore_service = Some(format!("file-http-unit-{id}"));
        let client = AnytypeClient::with_config(config).expect("create mock client");
        client.set_api_key(HttpCredentials::new("test-token"));
        (client, server)
    }

    async fn mock_file_client_sequence(
        responses: Vec<&'static str>,
    ) -> (AnytypeClient, JoinHandle<Vec<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock file server");
        let address = listener.local_addr().expect("mock server address");
        let server = tokio::spawn(async move {
            let mut requests = Vec::with_capacity(responses.len());
            for response in responses {
                let (mut stream, _) = listener.accept().await.expect("accept mock request");
                let mut request = Vec::new();
                let mut chunk = [0_u8; 1024];
                loop {
                    let read = stream.read(&mut chunk).await.expect("read mock request");
                    if read == 0 {
                        break;
                    }
                    request.extend_from_slice(&chunk[..read]);
                    if request.windows(4).any(|window| window == b"\r\n\r\n") {
                        break;
                    }
                }
                stream
                    .write_all(response.as_bytes())
                    .await
                    .expect("write mock response");
                requests.push(String::from_utf8(request).expect("HTTP request is UTF-8"));
            }
            requests
        });

        let id = NEXT_MOCK_ID.fetch_add(1, Ordering::Relaxed);
        let key_path = std::env::temp_dir().join(format!(
            "anytype-file-http-unit-{}-{id}.db",
            std::process::id()
        ));
        let mut config = ClientConfig::default().app_name("file-http-unit");
        config.base_url = Some(format!("http://{address}"));
        config.keystore = Some(format!("file:path={}", key_path.display()));
        config.keystore_service = Some(format!("file-http-unit-{id}"));
        let client = AnytypeClient::with_config(config).expect("create mock client");
        client.set_api_key(HttpCredentials::new("test-token"));
        (client, server)
    }

    #[test]
    fn simple_path_and_byte_uploads_select_rest() {
        let path = FileSource::Path(PathBuf::from("example.txt"));
        let bytes = FileSource::Bytes(Bytes::from_static(b"hello"));

        assert!(upload_uses_rest(Some(&path), false));
        assert!(upload_uses_rest(Some(&bytes), false));
    }

    #[test]
    fn rich_and_url_uploads_select_grpc() {
        let path = FileSource::Path(PathBuf::from("example.txt"));
        let url = FileSource::Url("https://example.invalid/file".to_string());

        assert!(!upload_uses_rest(Some(&path), true));
        assert!(!upload_uses_rest(Some(&url), false));
        assert!(!upload_uses_rest(None, false));
    }

    #[test]
    fn preload_source_tracks_url_and_path() {
        let id = NEXT_MOCK_ID.fetch_add(1, Ordering::Relaxed);
        let key_path = std::env::temp_dir().join(format!(
            "anytype-preload-unit-{}-{id}.db",
            std::process::id()
        ));
        let mut config = ClientConfig::default().app_name("preload-unit");
        config.keystore = Some(format!("file:path={}", key_path.display()));
        config.keystore_service = Some(format!("preload-unit-{id}"));
        let client = AnytypeClient::with_config(config).expect("create client");

        let url_request = client
            .files()
            .preload("space")
            .from_url("https://example.invalid/file");
        assert!(matches!(url_request.source, Some(FileSource::Url(_))));

        let path_request = client.files().preload("space").from_path("example.txt");
        assert!(matches!(path_request.source, Some(FileSource::Path(_))));
    }

    #[test]
    fn rest_upload_response_normalizes_to_file_object() {
        let file = file_from_http_upload(
            "space-id",
            FileUploadResponse {
                object_id: "file-id".to_string(),
                name: Some("report.txt".to_string()),
                extension: Some("txt".to_string()),
                media: Some("text/plain".to_string()),
                size_in_bytes: Some(5),
            },
        );

        assert_eq!(file.id, "file-id");
        assert_eq!(file.space_id, "space-id");
        assert_eq!(file.name.as_deref(), Some("report.txt"));
        assert_eq!(file.mime.as_deref(), Some("text/plain"));
        assert_eq!(file.size, Some(5));
        assert!(matches!(file.file_type, FileType::File));
        assert!(matches!(file.style, FileStyle::Auto));
        assert!(file.details.is_null());
    }

    #[test]
    fn current_http_upload_schema_deserializes() {
        let response: FileUploadResponse = serde_json::from_value(serde_json::json!({
            "object_id": "file-id",
            "name": "photo.png",
            "extension": "png",
            "media": "image/png",
            "size_in_bytes": 42
        }))
        .expect("deserialize current anytype-heart file response");

        assert_eq!(response.object_id, "file-id");
        assert_eq!(response.extension.as_deref(), Some("png"));
    }

    #[tokio::test]
    async fn ranged_download_sends_width_and_conditional_headers() {
        let (client, server) = mock_file_client(
            "HTTP/1.1 206 Partial Content\r\n\
             Content-Type: text/plain\r\n\
             Content-Length: 5\r\n\
             Content-Range: bytes 0-4/11\r\n\
             Accept-Ranges: bytes\r\n\
             Last-Modified: Sun, 19 Jul 2026 12:00:00 GMT\r\n\
             Cache-Control: max-age=31536000, private\r\n\
             Connection: close\r\n\r\nhello",
        )
        .await;

        let response = client
            .files()
            .download_request("space-1", "file-1")
            .width(320)
            .range("bytes=0-4")
            .if_match("\"current\"")
            .if_none_match("\"stale\"")
            .if_modified_since("Sun, 19 Jul 2026 11:00:00 GMT")
            .if_unmodified_since("Sun, 19 Jul 2026 13:00:00 GMT")
            .if_range("Sun, 19 Jul 2026 12:00:00 GMT")
            .download()
            .await
            .expect("ranged download");

        assert_eq!(response.status, StatusCode::PARTIAL_CONTENT);
        assert!(response.is_partial());
        assert_eq!(response.bytes, Bytes::from_static(b"hello"));
        assert_eq!(response.metadata.content_length, Some(5));
        assert_eq!(
            response.metadata.content_range.as_deref(),
            Some("bytes 0-4/11")
        );
        assert_eq!(response.metadata.accept_ranges.as_deref(), Some("bytes"));
        assert_eq!(
            response.metadata.content_type.as_deref(),
            Some("text/plain")
        );

        let request = server.await.expect("mock server task").to_ascii_lowercase();
        assert!(request.starts_with("get /v1/spaces/space-1/files/file-1?width=320 http/1.1"));
        assert!(request.contains("\r\nrange: bytes=0-4\r\n"));
        assert!(request.contains("\r\nif-match: \"current\"\r\n"));
        assert!(request.contains("\r\nif-none-match: \"stale\"\r\n"));
        assert!(request.contains("\r\nif-modified-since: sun, 19 jul 2026 11:00:00 gmt\r\n"));
        assert!(request.contains("\r\nif-unmodified-since: sun, 19 jul 2026 13:00:00 gmt\r\n"));
        assert!(request.contains("\r\nif-range: sun, 19 jul 2026 12:00:00 gmt\r\n"));
    }

    #[tokio::test]
    async fn head_returns_file_metadata_without_a_body() {
        let (client, server) = mock_file_client(
            "HTTP/1.1 200 OK\r\n\
             Content-Type: image/png\r\n\
             Content-Length: 1234\r\n\
             Accept-Ranges: bytes\r\n\
             ETag: \"image-v1\"\r\n\
             Last-Modified: Sun, 19 Jul 2026 12:00:00 GMT\r\n\
             Connection: close\r\n\r\n",
        )
        .await;

        let response = client
            .files()
            .metadata("space-1", "image-1")
            .await
            .expect("HEAD metadata");

        assert_eq!(response.status, StatusCode::OK);
        assert!(response.bytes.is_empty());
        assert_eq!(response.metadata.content_length, Some(1234));
        assert_eq!(response.metadata.content_type.as_deref(), Some("image/png"));
        assert_eq!(response.metadata.etag.as_deref(), Some("\"image-v1\""));
        assert_eq!(
            response.metadata.last_modified.as_deref(),
            Some("Sun, 19 Jul 2026 12:00:00 GMT")
        );

        let request = server.await.expect("mock server task");
        assert!(request.starts_with("HEAD /v1/spaces/space-1/files/image-1 HTTP/1.1"));
    }

    #[tokio::test]
    async fn conditional_not_modified_status_is_preserved() {
        let (client, server) = mock_file_client(
            "HTTP/1.1 304 Not Modified\r\n\
             Last-Modified: Sun, 19 Jul 2026 12:00:00 GMT\r\n\
             Connection: close\r\n\r\n",
        )
        .await;

        let response = client
            .files()
            .download_request("space-1", "file-1")
            .if_modified_since("Sun, 19 Jul 2026 12:00:00 GMT")
            .download()
            .await
            .expect("conditional response");

        assert_eq!(response.status, StatusCode::NOT_MODIFIED);
        assert!(response.is_not_modified());
        assert!(response.bytes.is_empty());
        server.await.expect("mock server task");
    }

    #[tokio::test]
    async fn per_request_body_limit_accepts_exact_boundary_and_rejects_one_over() {
        let responses = vec![
            "HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nContent-Length: 5\r\nConnection: close\r\n\r\nbytes",
            "HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nContent-Length: 5\r\nConnection: close\r\n\r\nbytes",
        ];
        let (client, server) = mock_file_client_sequence(responses).await;

        let response = client
            .files()
            .download_request("space-1", "file-1")
            .response_limit_bytes(5)
            .download()
            .await
            .expect("exact limit is accepted");
        assert_eq!(response.bytes, Bytes::from_static(b"bytes"));

        let error = client
            .files()
            .download_request("space-1", "file-1")
            .response_limit_bytes(4)
            .download()
            .await
            .expect_err("one byte over the request limit must fail");
        assert!(matches!(
            error,
            crate::error::AnytypeError::ResponseTooLarge {
                limit: 4,
                declared: Some(5)
            }
        ));
        assert_eq!(server.await.expect("mock server task").len(), 2);
    }

    #[tokio::test]
    async fn malformed_or_contradictory_range_evidence_fails_closed() {
        let responses = vec![
            "HTTP/1.1 206 Partial Content\r\nContent-Type: text/plain\r\nContent-Length: 5\r\nContent-Range: bytes 1-5/11\r\nConnection: close\r\n\r\nhello",
            "HTTP/1.1 206 Partial Content\r\nContent-Type: text/plain\r\nContent-Length: 5\r\nContent-Range: bytes nonsense\r\nConnection: close\r\n\r\nhello",
        ];
        let (client, server) = mock_file_client_sequence(responses).await;

        for expected_issue in ["request-mismatch", "malformed"] {
            let error = client
                .files()
                .download_request("space-1", "file-1")
                .range("bytes=0-4")
                .response_limit_bytes(5)
                .download()
                .await
                .expect_err("bad range evidence must fail");
            assert!(matches!(
                error,
                crate::error::AnytypeError::InvalidFileResponseHeader {
                    header: "content-range",
                    issue,
                    ..
                } if issue == expected_issue
            ));
        }
        assert_eq!(server.await.expect("mock server task").len(), 2);
    }

    #[tokio::test]
    async fn duplicate_validator_and_header_budget_fail_with_typed_evidence() {
        let responses = vec![
            "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 1\r\nETag: \"one\"\r\nETag: \"two\"\r\nConnection: close\r\n\r\nx",
            "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 1\r\nETag: W/\"unterminated\r\nConnection: close\r\n\r\nx",
            "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 1\r\nConnection: close\r\n\r\nx",
        ];
        let (client, server) = mock_file_client_sequence(responses).await;

        let duplicate = client
            .files()
            .download_request("space-1", "file-1")
            .download()
            .await
            .expect_err("duplicate ETag must fail");
        assert!(matches!(
            duplicate,
            crate::error::AnytypeError::InvalidFileResponseHeader {
                status: 200,
                header: "etag",
                issue: "duplicate"
            }
        ));

        let malformed = client
            .files()
            .download_request("space-1", "file-1")
            .download()
            .await
            .expect_err("malformed ETag must fail");
        assert!(matches!(
            malformed,
            crate::error::AnytypeError::InvalidFileResponseHeader {
                status: 200,
                header: "etag",
                issue: "malformed"
            }
        ));

        let bounded = client
            .files()
            .download_request("space-1", "file-1")
            .header_evidence_limit_bytes(8)
            .download()
            .await
            .expect_err("allowlisted headers over their request budget must fail");
        assert!(matches!(
            bounded,
            crate::error::AnytypeError::FileHeaderEvidenceTooLarge {
                limit: 8,
                status: 200
            }
        ));
        assert_eq!(server.await.expect("mock server task").len(), 3);
    }

    #[tokio::test]
    async fn truncated_body_and_malformed_metadata_fail_closed() {
        let responses = vec![
            "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 5\r\nConnection: close\r\n\r\nfour",
            "HTTP/1.1 200 OK\r\nContent-Type: not a mime\r\nContent-Length: 1\r\nConnection: close\r\n\r\nx",
            "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 1\r\nLast-Modified: yesterday\r\nConnection: close\r\n\r\nx",
        ];
        let (client, server) = mock_file_client_sequence(responses).await;

        let truncated = client
            .files()
            .download_request("space-1", "file-1")
            .response_limit_bytes(5)
            .download()
            .await
            .expect_err("truncated response must fail");
        assert!(matches!(truncated, crate::error::AnytypeError::Http { .. }));

        for expected_header in ["content-type", "last-modified"] {
            let malformed = client
                .files()
                .download_request("space-1", "file-1")
                .download()
                .await
                .expect_err("malformed metadata must fail");
            assert!(matches!(
                malformed,
                crate::error::AnytypeError::InvalidFileResponseHeader {
                    header,
                    issue: "malformed",
                    ..
                } if header == expected_header
            ));
        }
        assert_eq!(server.await.expect("mock server task").len(), 3);
    }

    #[tokio::test]
    async fn safe_retries_share_one_physical_attempt_ceiling() {
        let responses = vec![
            "HTTP/1.1 429 Too Many Requests\r\nRateLimit-Reset: 0\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            "HTTP/1.1 504 Gateway Timeout\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok",
        ];
        let (client, server) = mock_file_client_sequence(responses).await;
        let response = client
            .files()
            .download_request("space-1", "file-1")
            .max_attempts(3)
            .response_limit_bytes(2)
            .header_evidence_limit_bytes(128)
            .download()
            .await
            .expect("third and final physical attempt succeeds");
        assert_eq!(response.bytes, Bytes::from_static(b"ok"));
        let requests = server.await.expect("mock server task");
        assert_eq!(requests.len(), 3);
        assert_eq!(client.http_metrics().total_requests, 3);
        assert_eq!(client.http_metrics().retries, 2);
    }

    #[tokio::test]
    async fn intermediate_retry_header_evidence_overflow_stops_without_replay() {
        let responses = vec![
            "HTTP/1.1 429 Too Many Requests\r\nRateLimit-Reset: 0\r\nContent-Length: 0\r\nETag: \"xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx\"\r\nConnection: close\r\n\r\n",
        ];
        let (client, server) = mock_file_client_sequence(responses).await;

        let error = client
            .files()
            .download_request("space-1", "file-1")
            .max_attempts(3)
            .header_evidence_limit_bytes(64)
            .download()
            .await
            .expect_err("intermediate retry headers must be bounded before replay");
        assert!(matches!(
            error,
            crate::error::AnytypeError::FileHeaderEvidenceTooLarge {
                limit: 64,
                status: 429
            }
        ));
        assert_eq!(server.await.expect("mock server task").len(), 1);
        assert_eq!(client.http_metrics().total_requests, 1);
        assert_eq!(client.http_metrics().retries, 0);
    }

    #[tokio::test]
    async fn retry_ceiling_never_sends_one_attempt_over() {
        let responses = vec![
            "HTTP/1.1 429 Too Many Requests\r\nRateLimit-Reset: 0\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            "HTTP/1.1 429 Too Many Requests\r\nRateLimit-Reset: 0\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        ];
        let (client, server) = mock_file_client_sequence(responses).await;
        let error = client
            .files()
            .download_request("space-1", "file-1")
            .max_attempts(2)
            .download()
            .await
            .expect_err("second physical attempt exhausts the request ceiling");
        assert!(matches!(
            error,
            crate::error::AnytypeError::ApiError { code: 429, .. }
        ));
        assert_eq!(server.await.expect("mock server task").len(), 2);
        assert_eq!(client.http_metrics().total_requests, 2);
        assert_eq!(client.http_metrics().retries, 1);
    }

    #[test]
    fn request_range_grammar_is_canonical_and_checked() {
        assert!(super::parse_request_range("bytes=0-4").is_ok());
        assert!(super::parse_request_range("bytes=4-").is_ok());
        assert!(super::parse_request_range("bytes=-4").is_ok());
        for invalid in [
            "bytes=00-4",
            "bytes=4-3",
            "bytes=-0",
            "bytes=0-1,3-4",
            "items=0-4",
            "bytes=0 - 4",
        ] {
            assert!(
                super::parse_request_range(invalid).is_err(),
                "accepted {invalid}"
            );
        }
    }

    #[tokio::test]
    async fn permanent_delete_sets_skip_bin_query() {
        let (client, server) =
            mock_file_client("HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n").await;

        client
            .files()
            .delete_request("space-1", "file-1")
            .permanently()
            .delete()
            .await
            .expect("permanent delete");

        let request = server.await.expect("mock server task");
        assert!(
            request.starts_with("DELETE /v1/spaces/space-1/files/file-1?skip_bin=true HTTP/1.1")
        );
    }
}
