//! `HttpClient` middleware used by `AnytypeClient`
//!
//! Responsible for
//!  - handing all HTTP api requests
//!  - logging/tracing
//!  - retries and backoff (for timeouts and connection errors)
//!  - rate limiting

use std::{
    fmt,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use bytes::Bytes;
use parking_lot::Mutex;
use reqwest::{ClientBuilder, Method, Response, StatusCode, header::HeaderMap};
use serde::{Serialize, de::DeserializeOwned};
use snafu::prelude::*;
use tracing::{debug, error, info, trace, warn};

use crate::{
    Result,
    client::{
        MAX_DOCUMENT_RESPONSE_BYTES, MAX_ERROR_RESPONSE_BYTES, MAX_FILE_RESPONSE_BYTES,
        MAX_JSON_RESPONSE_BYTES, ResponseLimits,
    },
    config::{
        ANYTYPE_API_HEADER, MAX_RETRIES, RATE_LIMIT_WAIT_MAX_SECS, RATE_LIMIT_WAIT_WARN_SECS,
    },
    filters::QueryWithFilters,
    prelude::*,
};

/// HTTP metrics tracked using atomic counters for thread-safe access.
/// These counters are cumulative and never reset during the client's lifetime.
#[derive(Debug, Default)]
pub struct HttpMetrics {
    /// Total number of HTTP requests sent to the server (excludes cached responses)
    total_requests: AtomicU64,
    /// Total number of successful responses (2xx status codes)
    successful_responses: AtomicU64,
    /// Total number of error responses (non-2xx status codes, excluding rate limit errors)
    errors: AtomicU64,
    /// Total number of retry attempts (connection failures, timeouts, 5xx errors)
    retries: AtomicU64,
    /// Total bytes sent in request bodies
    bytes_sent: AtomicU64,
    /// Total bytes received in response bodies
    bytes_received: AtomicU64,
    /// Total number of rate limit errors (429 responses)
    rate_limit_errors: AtomicU64,
    /// Total seconds spent waiting for rate limit backoff
    rate_limit_delay_secs: AtomicU64,
}

impl HttpMetrics {
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns a snapshot of current metrics as plain u64 values
    pub fn snapshot(&self) -> HttpMetricsSnapshot {
        HttpMetricsSnapshot {
            total_requests: self.total_requests.load(Ordering::Relaxed),
            successful_responses: self.successful_responses.load(Ordering::Relaxed),
            errors: self.errors.load(Ordering::Relaxed),
            retries: self.retries.load(Ordering::Relaxed),
            bytes_sent: self.bytes_sent.load(Ordering::Relaxed),
            bytes_received: self.bytes_received.load(Ordering::Relaxed),
            rate_limit_errors: self.rate_limit_errors.load(Ordering::Relaxed),
            rate_limit_delay_secs: self.rate_limit_delay_secs.load(Ordering::Relaxed),
        }
    }

    fn increment_requests(&self) {
        self.total_requests.fetch_add(1, Ordering::Relaxed);
    }

    fn increment_success(&self) {
        self.successful_responses.fetch_add(1, Ordering::Relaxed);
    }

    fn increment_errors(&self) {
        self.errors.fetch_add(1, Ordering::Relaxed);
    }

    fn increment_retries(&self) {
        self.retries.fetch_add(1, Ordering::Relaxed);
    }

    fn add_bytes_sent(&self, bytes: u64) {
        self.bytes_sent.fetch_add(bytes, Ordering::Relaxed);
    }

    fn add_bytes_received(&self, bytes: u64) {
        self.bytes_received.fetch_add(bytes, Ordering::Relaxed);
    }

    fn increment_rate_limit_errors(&self) {
        self.rate_limit_errors.fetch_add(1, Ordering::Relaxed);
    }

    fn add_rate_limit_delay(&self, secs: u64) {
        self.rate_limit_delay_secs
            .fetch_add(secs, Ordering::Relaxed);
    }
}

/// A point-in-time snapshot of HTTP metrics with plain u64 values.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HttpMetricsSnapshot {
    /// Total number of HTTP requests sent to the server
    pub total_requests: u64,
    /// Total number of successful responses (2xx status codes)
    pub successful_responses: u64,
    /// Total number of error responses (non-2xx status codes, excluding rate limit errors)
    pub errors: u64,
    /// Total number of retry attempts
    pub retries: u64,
    /// Total bytes sent in request bodies
    pub bytes_sent: u64,
    /// Total bytes received in response bodies
    pub bytes_received: u64,
    /// Total number of rate limit errors (429 responses)
    pub rate_limit_errors: u64,
    /// Total seconds spent waiting for rate limit backoff
    pub rate_limit_delay_secs: u64,
}

impl std::fmt::Display for HttpMetricsSnapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "requests={} success={} errors={} retries={} rate_limit={}/{}s sent={} recv={}",
            self.total_requests,
            self.successful_responses,
            self.errors,
            self.retries,
            self.rate_limit_errors,
            self.rate_limit_delay_secs,
            format_bytes(self.bytes_sent),
            format_bytes(self.bytes_received),
        )
    }
}

#[allow(clippy::cast_precision_loss)]
fn format_bytes(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes}B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1}KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1}MB", (bytes / (1024 * 1024)) as f64)
    }
}

/// status codes where it's ok to retry and backoff
fn retry_for_status(code: StatusCode) -> bool {
    match code {
      StatusCode::TOO_MANY_REQUESTS /* 429 */ |
      StatusCode::GATEWAY_TIMEOUT /* 504 */ |
      StatusCode::REQUEST_TIMEOUT /* 408 */ => true,
      _ => false,
    }
}

#[derive(Clone, Default)]
pub struct HttpRequest {
    pub method: Method,
    pub path: String,
    pub query: Vec<(String, String)>,
    pub body: Option<Bytes>,
}

/// Raw response data for endpoints whose status and headers are part of the
/// public contract, such as ranged and conditional file downloads.
pub(crate) struct RawHttpResponse {
    pub(crate) status: StatusCode,
    pub(crate) headers: HeaderMap,
    pub(crate) body: Bytes,
}

impl fmt::Debug for HttpRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HttpRequest")
            .field("method", &self.method)
            .field("path", &self.path)
            .field("query_fields", &self.query.len())
            .field("body", &self.body.as_ref().map_or(0, Bytes::len))
            .finish()
    }
}

impl HttpRequest {
    /// Create a new request with updated pagination parameters.
    /// This replaces any existing limit/offset query parameters.
    pub(crate) fn with_pagination(&self, offset: u32, limit: u32) -> Self {
        let mut new_query: Vec<(String, String)> = self
            .query
            .iter()
            .filter(|(key, _)| key != "offset" && key != "limit")
            .cloned()
            .collect();

        new_query.push(("limit".to_string(), limit.to_string()));
        new_query.push(("offset".to_string(), offset.to_string()));

        Self {
            method: self.method.clone(),
            path: self.path.clone(),
            query: new_query,
            body: self.body.clone(),
        }
    }
}

#[derive(Clone)]
pub struct HttpClient {
    pub client: reqwest::Client,

    /// Base URL for API requests (e.g., "<http://localhost:31009>")
    pub base_url: String,

    pub api_key: Arc<Mutex<HttpCredentials>>,

    limits: ValidationLimits,

    response_limits: ResponseLimits,

    // Max consecutive 429 retries before failing; 0 disables cap.
    rate_limit_max_retries: u32,

    /// HTTP request/response metrics
    pub metrics: Arc<HttpMetrics>,
}

impl fmt::Debug for HttpClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HttpClient")
            .field("base_url", &self.base_url)
            .field("api_key", &String::from("(MASKED)"))
            .field("rate_limit_max_retries", &self.rate_limit_max_retries)
            .field("metrics", &self.metrics)
            .finish_non_exhaustive()
    }
}

struct ParsedRetry {
    header: String,
    duration: Duration,
}

/// Parse rate limit headers from a 429 response to determine retry duration.
/// Anytype Heart uses github.com/didip/tollbooth/v8 (v8.0.1), which sets
/// RateLimit-Reset and X-Rate-Limit-Duration as seconds to wait.
fn parse_retry_after(headers: &HeaderMap) -> Result<ParsedRetry> {
    for header_name in ["ratelimit-reset", "x-rate-limit-duration"] {
        if let Some(header_value) = headers.get(header_name)
            && let Ok(header) = header_value.to_str()
        {
            if let Ok(secs) = header.parse::<u64>() {
                return Ok(ParsedRetry {
                    duration: Duration::from_secs(secs),
                    header: header.to_string(),
                });
            }
            error!(header_name, "Could not parse HTTP 429 response header");
        }
    }

    // couldn't parse header
    Err(AnytypeError::RateLimitExceeded {
        header: "Received 429 response but couldn't parse rate limit headers. See logs".to_string(),
        duration: Duration::from_secs(0),
    })
}

impl HttpClient {
    pub fn new(
        builder: ClientBuilder,
        base_url: String,
        limits: ValidationLimits,
        response_limits: ResponseLimits,
        rate_limit_max_retries: u32,
        http_creds: HttpCredentials,
    ) -> Result<Self> {
        let client = builder.build().context(HttpSnafu {
            method: "client-init",
            url: "",
        })?;
        for (name, limit, maximum) in [
            (
                "json_bytes",
                response_limits.json_bytes,
                MAX_JSON_RESPONSE_BYTES,
            ),
            (
                "document_bytes",
                response_limits.document_bytes,
                MAX_DOCUMENT_RESPONSE_BYTES,
            ),
            (
                "error_bytes",
                response_limits.error_bytes,
                MAX_ERROR_RESPONSE_BYTES,
            ),
            (
                "file_bytes",
                response_limits.file_bytes,
                MAX_FILE_RESPONSE_BYTES,
            ),
        ] {
            if limit == 0 || limit > maximum || usize::try_from(limit).is_err() {
                return Err(AnytypeError::Validation {
                    message: format!(
                        "response_limits.{name} must be between 1 and {maximum} bytes"
                    ),
                });
            }
        }
        Ok(Self {
            client,
            base_url,
            api_key: Arc::new(Mutex::new(http_creds)),
            limits,
            response_limits,
            rate_limit_max_retries,
            metrics: Arc::new(HttpMetrics::new()),
        })
    }

    /// Returns a snapshot of current HTTP metrics
    pub fn metrics_snapshot(&self) -> HttpMetricsSnapshot {
        self.metrics.snapshot()
    }

    pub(crate) const fn document_response_limit(&self) -> u64 {
        self.response_limits.document_bytes
    }

    /// Incrementally buffers one response up to `limit` bytes.
    ///
    /// A truthful oversized `Content-Length` is rejected before reading. The
    /// streamed total is still checked with overflow-safe arithmetic because
    /// the header may be absent or misleading. Capacity starts small rather
    /// than trusting an attacker-controlled advertised length.
    async fn read_bounded(
        &self,
        mut response: Response,
        limit: u64,
        method: &str,
        path: &str,
    ) -> Result<Bytes> {
        let declared = response.content_length();
        if declared.is_some_and(|length| length > limit) {
            return Err(AnytypeError::ResponseTooLarge { limit, declared });
        }

        const INITIAL_CAPACITY: u64 = 8 * 1024;
        let initial_capacity = declared.unwrap_or(INITIAL_CAPACITY).min(INITIAL_CAPACITY);
        let mut body = Vec::with_capacity(initial_capacity as usize);
        while let Some(chunk) = response
            .chunk()
            .await
            .context(HttpSnafu { method, url: path })?
        {
            self.metrics.add_bytes_received(chunk.len() as u64);
            let next_len = (body.len() as u64)
                .checked_add(chunk.len() as u64)
                .ok_or(AnytypeError::ResponseTooLarge { limit, declared })?;
            if next_len > limit {
                return Err(AnytypeError::ResponseTooLarge { limit, declared });
            }
            body.extend_from_slice(&chunk);
        }
        Ok(Bytes::from(body))
    }

    async fn read_error_body(
        &self,
        response: Response,
        method: &str,
        path: &str,
    ) -> Result<String> {
        let body = self
            .read_bounded(response, self.response_limits.error_bytes, method, path)
            .await?;
        Ok(String::from_utf8_lossy(&body).into_owned())
    }

    /// Returns true if `api_key` has been initialized.
    pub fn has_key(&self) -> bool {
        self.api_key.lock().has_creds()
    }

    /// Sets the API key for authenticated requests.
    pub fn set_api_key(&self, api_key: HttpCredentials) {
        let mut write_key = self.api_key.lock();
        *write_key = api_key;
    }

    /// Clears the api key if set. (in memory, does not change keystore)
    pub fn clear_api_key(&self) {
        let mut write_key = self.api_key.lock();
        *write_key = HttpCredentials::default();
    }

    /// Returns http token from memory (Does not refresh from keystore)
    pub(crate) fn get_api_key(&self) -> HttpCredentials {
        self.api_key.lock().clone()
    }

    /// Makes an authenticated DELETE request.
    pub(crate) async fn delete_request<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        let req = HttpRequest {
            method: Method::DELETE,
            path: path.into(),
            query: Vec::default(),
            body: None,
        };
        self.send(req).await
    }

    /// Makes an authenticated DELETE whose successful JSON may contain a
    /// complete document body.
    pub(crate) async fn delete_document_request<T: DeserializeOwned>(
        &self,
        path: &str,
    ) -> Result<T> {
        let req = HttpRequest {
            method: Method::DELETE,
            path: path.into(),
            query: Vec::default(),
            body: None,
        };
        self.send_with_limit(req, self.response_limits.document_bytes)
            .await
    }

    pub(crate) async fn get_request<T: DeserializeOwned>(
        &self,
        path: &str,
        query: QueryWithFilters,
    ) -> Result<T> {
        query.validate().map_err(|err| AnytypeError::Validation {
            message: format!("get_request {path} {err}"),
        })?;
        let req = HttpRequest {
            method: Method::GET,
            path: path.into(),
            query: query.params,
            body: None,
        };
        self.send(req).await
    }

    /// Makes an authenticated GET with an explicit finite response ceiling.
    pub(crate) async fn get_request_with_limit<T: DeserializeOwned>(
        &self,
        path: &str,
        query: QueryWithFilters,
        response_limit: u64,
    ) -> Result<T> {
        query.validate().map_err(|err| AnytypeError::Validation {
            message: format!("get_request_with_limit {path} {err}"),
        })?;
        if response_limit == 0 || response_limit > self.response_limits.document_bytes {
            return Err(AnytypeError::Validation {
                message: format!(
                    "response limit must be between 1 and {} bytes",
                    self.response_limits.document_bytes
                ),
            });
        }
        let req = HttpRequest {
            method: Method::GET,
            path: path.into(),
            query: query.params,
            body: None,
        };
        self.send_with_limit(req, response_limit).await
    }

    /// Opens an authenticated streaming GET request.
    ///
    /// Unlike [`get_request`](Self::get_request), this returns the live
    /// response without buffering its body so callers can incrementally
    /// consume endpoints such as Server-Sent Events.
    pub(crate) async fn get_streaming_request(
        &self,
        path: &str,
        query: QueryWithFilters,
        headers: HeaderMap,
    ) -> Result<reqwest::Response> {
        query.validate().map_err(|err| AnytypeError::Validation {
            message: format!("get_streaming_request {path} {err}"),
        })?;
        self.limits.validate_query(&query.params)?;

        let api_key = self.get_api_key();
        let Some(token) = api_key.token() else {
            return Err(AnytypeError::Auth {
                message: format!(
                    "HTTP credentials missing token. Client is not authenticated. url={}",
                    self.base_url,
                ),
            });
        };
        let full_url = format!("{}{}", self.base_url, path);
        debug!(path, "get_streaming_request");
        self.metrics.increment_requests();
        let response = self
            .client
            .get(&full_url)
            .query(&query.params)
            .header(ANYTYPE_API_HEADER, ANYTYPE_API_VERSION)
            .bearer_auth(token)
            .headers(headers)
            .send()
            .await
            .context(HttpSnafu {
                method: "get",
                url: &full_url,
            })?;

        if !response.status().is_success() {
            self.metrics.increment_errors();
            let code = response.status().as_u16();
            let message = self.read_error_body(response, "get", path).await?;
            return Err(AnytypeError::ApiError {
                code,
                method: "get".to_string(),
                url: path.to_string(),
                message,
            });
        }

        self.metrics.increment_success();
        Ok(response)
    }

    /// Makes an authenticated PATCH request with JSON body.
    pub(crate) async fn patch_request<T: DeserializeOwned, B: Serialize + Sync>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T, AnytypeError> {
        let req = HttpRequest {
            method: Method::PATCH,
            path: path.into(),
            query: Vec::default(),
            body: Some(Bytes::from(
                serde_json::to_vec(body).context(SerializationSnafu)?,
            )),
        };
        self.send(req).await
    }

    /// Makes an authenticated PATCH whose successful JSON may contain a
    /// complete document body.
    pub(crate) async fn patch_document_request<T: DeserializeOwned, B: Serialize + Sync>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T, AnytypeError> {
        let req = HttpRequest {
            method: Method::PATCH,
            path: path.into(),
            query: Vec::default(),
            body: Some(Bytes::from(
                serde_json::to_vec(body).context(SerializationSnafu)?,
            )),
        };
        self.send_with_limit(req, self.response_limits.document_bytes)
            .await
    }

    pub(crate) async fn post_request<T: DeserializeOwned, B: Serialize + Sync>(
        &self,
        path: &str,
        body: &B,
        query: QueryWithFilters,
    ) -> Result<T> {
        let req = HttpRequest {
            method: Method::POST,
            path: path.into(),
            query: query.params,
            body: Some(Bytes::from(
                serde_json::to_vec(body).context(SerializationSnafu)?,
            )),
        };
        self.send(req).await
    }

    /// Makes an authenticated POST whose successful JSON may contain a
    /// complete document body.
    pub(crate) async fn post_document_request<T: DeserializeOwned, B: Serialize + Sync>(
        &self,
        path: &str,
        body: &B,
        query: QueryWithFilters,
    ) -> Result<T> {
        let req = HttpRequest {
            method: Method::POST,
            path: path.into(),
            query: query.params,
            body: Some(Bytes::from(
                serde_json::to_vec(body).context(SerializationSnafu)?,
            )),
        };
        self.send_with_limit(req, self.response_limits.document_bytes)
            .await
    }

    /// Makes an unauthenticated POST request (for auth endpoints).
    pub(crate) async fn post_unauthenticated<Resp: DeserializeOwned, Req: Serialize + Sync>(
        &self,
        path: &str,
        body: &Req,
    ) -> Result<Resp> {
        let full_url = format!("{}{}", self.base_url, path);
        debug!(path, "post_unauthenticated");
        self.metrics.increment_requests();
        let response = self
            .client
            .post(&full_url)
            .header(ANYTYPE_API_HEADER, ANYTYPE_API_VERSION)
            .json(body)
            .send()
            .await
            .context(HttpSnafu {
                method: "post",
                url: &full_url,
            })?;
        if !response.status().is_success() {
            self.metrics.increment_errors();
            let code = response.status().as_u16();
            let message = self.read_error_body(response, "post", path).await?;
            return Err(AnytypeError::ApiError {
                code,
                method: "post".to_string(),
                url: path.to_string(),
                message,
            });
        }
        let data = match self
            .read_bounded(response, self.response_limits.json_bytes, "post", path)
            .await
        {
            Ok(data) => data,
            Err(error) => {
                self.metrics.increment_errors();
                return Err(error);
            }
        };
        self.metrics.increment_success();
        deserialize_json(&data)
    }

    /// Makes an authenticated DELETE request that expects an empty
    /// (`204 No Content`) response body.
    ///
    /// The JSON [`delete_request`](Self::delete_request) helper deserializes a
    /// response entity; file deletion (`DELETE /v1/spaces/{space_id}/files/{file_id}`)
    /// returns `204` with no body, so it needs this no-content variant.
    pub(crate) async fn delete_no_content(&self, path: &str) -> Result<()> {
        let api_key = self.get_api_key();
        let Some(token) = api_key.token() else {
            return Err(AnytypeError::Auth {
                message: format!(
                    "HTTP credentials missing token. Client is not authenticated. url={}",
                    self.base_url,
                ),
            });
        };
        let full_url = format!("{}{}", self.base_url, path);
        debug!(path, "delete_no_content");
        self.metrics.increment_requests();
        let response = self
            .client
            .delete(&full_url)
            .header(ANYTYPE_API_HEADER, ANYTYPE_API_VERSION)
            .bearer_auth(token)
            .send()
            .await
            .context(HttpSnafu {
                method: "delete",
                url: &full_url,
            })?;
        if !response.status().is_success() {
            self.metrics.increment_errors();
            let code = response.status().as_u16();
            let message = self.read_error_body(response, "delete", path).await?;
            return Err(AnytypeError::ApiError {
                code,
                method: "delete".to_string(),
                url: path.to_string(),
                message,
            });
        }
        self.metrics.increment_success();
        Ok(())
    }

    /// Makes an authenticated file request while preserving response metadata.
    ///
    /// In addition to successful responses, the statuses produced by HTTP
    /// range and precondition handling are returned to the caller. Other
    /// non-success statuses retain the client's usual [`AnytypeError::ApiError`]
    /// behavior.
    pub(crate) async fn file_request(
        &self,
        method: Method,
        path: &str,
        query: &[(String, String)],
        headers: HeaderMap,
    ) -> Result<RawHttpResponse> {
        let api_key = self.get_api_key();
        let Some(token) = api_key.token() else {
            return Err(AnytypeError::Auth {
                message: format!(
                    "HTTP credentials missing token. Client is not authenticated. url={}",
                    self.base_url,
                ),
            });
        };
        let full_url = format!("{}{}", self.base_url, path);
        debug!(method = %method, path, "file_request");
        self.metrics.increment_requests();
        let response = self
            .client
            .request(method.clone(), &full_url)
            .query(query)
            .header(ANYTYPE_API_HEADER, ANYTYPE_API_VERSION)
            .bearer_auth(token)
            .headers(headers)
            .send()
            .await
            .context(HttpSnafu {
                method: method.as_str(),
                url: &full_url,
            })?;
        let status = response.status();
        let response_headers = response.headers().clone();
        let allowed_control_status = matches!(
            status,
            StatusCode::NOT_MODIFIED
                | StatusCode::PRECONDITION_FAILED
                | StatusCode::RANGE_NOT_SATISFIABLE
        );
        let body = if method == Method::HEAD || status == StatusCode::NOT_MODIFIED {
            // HEAD and 304 legitimately carry representation metadata such as
            // a Content-Length while having no response body to buffer.
            Bytes::new()
        } else {
            let body_limit = if status.is_success() {
                self.response_limits.file_bytes
            } else {
                self.response_limits.error_bytes
            };
            match self
                .read_bounded(response, body_limit, method.as_str(), path)
                .await
            {
                Ok(body) => body,
                Err(error) => {
                    self.metrics.increment_errors();
                    return Err(error);
                }
            }
        };

        if status.is_success() {
            self.metrics.increment_success();
        } else {
            self.metrics.increment_errors();
        }

        if !(status.is_success() || allowed_control_status) {
            return Err(AnytypeError::ApiError {
                code: status.as_u16(),
                method: method.as_str().to_ascii_lowercase(),
                url: path.to_string(),
                message: String::from_utf8_lossy(&body).into_owned(),
            });
        }

        Ok(RawHttpResponse {
            status,
            headers: response_headers,
            body,
        })
    }

    /// Makes an authenticated `multipart/form-data` POST request.
    ///
    /// Used for file upload (`POST /v1/spaces/{space_id}/files`). The JSON
    /// response body is deserialized into `T`.
    pub(crate) async fn post_multipart<T: DeserializeOwned>(
        &self,
        path: &str,
        form: reqwest::multipart::Form,
    ) -> Result<T> {
        let api_key = self.get_api_key();
        let Some(token) = api_key.token() else {
            return Err(AnytypeError::Auth {
                message: format!(
                    "HTTP credentials missing token. Client is not authenticated. url={}",
                    self.base_url,
                ),
            });
        };
        let full_url = format!("{}{}", self.base_url, path);
        debug!(path, "post_multipart");
        self.metrics.increment_requests();
        let response = self
            .client
            .post(&full_url)
            .header(ANYTYPE_API_HEADER, ANYTYPE_API_VERSION)
            .bearer_auth(token)
            .multipart(form)
            .send()
            .await
            .context(HttpSnafu {
                method: "post",
                url: &full_url,
            })?;
        if !response.status().is_success() {
            self.metrics.increment_errors();
            let code = response.status().as_u16();
            let message = self.read_error_body(response, "post", path).await?;
            return Err(AnytypeError::ApiError {
                code,
                method: "post".to_string(),
                url: path.to_string(),
                message,
            });
        }
        let data = match self
            .read_bounded(response, self.response_limits.json_bytes, "post", path)
            .await
        {
            Ok(data) => data,
            Err(error) => {
                self.metrics.increment_errors();
                return Err(error);
            }
        };
        self.metrics.increment_success();
        deserialize_json(&data)
    }

    /// This function handles all authenticated anytype rest api requests (http: get,post,patch,delete)
    /// - handles 429 rate limit feedback
    /// - retries up to N(=3) times for connection failures or server timeout
    /// - maps http error codes into `AnytypeErrors`
    /// - deserializes json response body into return type T
    pub(crate) async fn send<T: DeserializeOwned>(&self, req: HttpRequest) -> Result<T> {
        self.send_with_limit(req, self.response_limits.json_bytes)
            .await
    }

    #[allow(clippy::too_many_lines)]
    async fn send_with_limit<T: DeserializeOwned>(
        &self,
        req: HttpRequest,
        response_limit: u64,
    ) -> Result<T> {
        // attempt counter is for server busy and connection drop errors
        // counter is reset to 0 whenever we wait based on 429 rate limit response
        let mut attempt = 0u32;
        let mut rate_limit_retries = 0u32;

        // time to wait on next iteration
        let mut retry_wait: Option<Duration> = None;

        // check for excessive request size or invalid query
        self.limits.validate_query(&req.query)?;
        if let Some(ref body) = req.body {
            self.limits
                .validate_body(body, &format!("http {} {}", req.method, req.path))?;
        }
        let api_key = self.get_api_key();
        if api_key.token().is_none() {
            return Err(AnytypeError::Auth {
                message: format!(
                    "HTTP credentials missing token. Client is not authenticated. url={}",
                    self.base_url,
                ),
            });
        }
        let full_url = format!("{}{}", self.base_url, req.path);
        let req_builder = self
            .client
            .request(req.method.clone(), &full_url)
            .query(&req.query)
            .header(ANYTYPE_API_HEADER, ANYTYPE_API_VERSION)
            // SAFETY: unwrap ok because we excluded token().is_none() above
            .bearer_auth(api_key.token().unwrap());

        // debug log (if tracing enabled)
        log_request(&req);

        // Track bytes to be sent (body size)
        let body_size = req.body.as_ref().map_or(0, |bytes| bytes.len() as u64);

        loop {
            if let Some(wait_time) = retry_wait {
                info!("RateLimit: pausing for {} sec", wait_time.as_secs());
                tokio::time::sleep(wait_time).await;
                retry_wait = None;
                attempt = 0;
            }
            let request = req_builder
                .try_clone()
                .ok_or_else(|| {
                    // try_clone with no body should never return None
                    AnytypeError::Other {
                        message: "reqwest::RequestBuilder internal error".into(),
                    }
                })?
                .body(req.body.clone().unwrap_or_default());

            // Track request metrics
            self.metrics.increment_requests();
            self.metrics.add_bytes_sent(body_size);

            match request.send().await {
                Ok(response) => {
                    let code = response.status();
                    if code != StatusCode::TOO_MANY_REQUESTS {
                        rate_limit_retries = 0;
                    }
                    match code {
                        // 2xx
                        // 201 (Object Created)
                        ok if ok.is_success() => {
                            // success - get the response body.
                            // If we fail to fully read the response, don't retry. The server might
                            // believe the request succeeded, and the request may not be idempotent.
                            // Most transient failures where we could have reasonably retried
                            // would have already occurred.
                            let body = match self
                                .read_bounded(
                                    response,
                                    response_limit,
                                    req.method.as_str(),
                                    &req.path,
                                )
                                .await
                            {
                                Ok(body) => body,
                                Err(error) => {
                                    self.metrics.increment_errors();
                                    return Err(error);
                                }
                            };
                            self.metrics.increment_success();

                            log_response(&req.path, &body);

                            // deserialization failure should not be retried
                            let resp_obj = deserialize_json(&body)?;
                            return Ok(resp_obj)
                        },
                        StatusCode::TOO_MANY_REQUESTS /* 429 */ => {
                            self.metrics.increment_rate_limit_errors();
                            rate_limit_retries = rate_limit_retries.saturating_add(1);
                            let headers = response.headers();
                            match parse_retry_after(headers) {
                                Err(err) => {
                                    error!("{err:?}");
                                    // couldn't parse header.
                                    return Err(err)
                                }
                                Ok(ParsedRetry{ header, duration}) => {
                                    if self.rate_limit_max_retries > 0
                                        && rate_limit_retries > self.rate_limit_max_retries
                                    {
                                        error!(
                                            attempt,
                                            ?req,
                                            "http 429 Rate-limit retries exceeded max={}",
                                            self.rate_limit_max_retries
                                        );
                                        return Err(AnytypeError::RateLimitExceeded {
                                            header,
                                            duration,
                                        });
                                    }
                                    if duration > Duration::from_secs(RATE_LIMIT_WAIT_MAX_SECS) {
                                        error!(
                                            attempt,
                                            ?req,
                                            "http 429 Rate-limit backoff={}s exceeds max",
                                            duration.as_secs()
                                        );
                                        return Err(AnytypeError::RateLimitExceeded {
                                            header,
                                            duration,
                                        });
                                    }
                                    if duration > Duration::from_secs(RATE_LIMIT_WAIT_WARN_SECS) {
                                        warn!(
                                            attempt,
                                            "http 429 Rate-limit backoff={}s",
                                            duration.as_secs()
                                        );
                                    }
                                    self.metrics.increment_retries();
                                    self.metrics.add_rate_limit_delay(duration.as_secs());
                                    retry_wait = Some(duration);
                                    // continue to try again
                                }
                            }
                        }
                        StatusCode::BAD_REQUEST /* 400 */ => {
                            self.metrics.increment_errors();
                            let message = self.read_error_body(response, req.method.as_str(), &req.path).await?;
                            error!(?code, ?req, "http");
                            return Err(AnytypeError::Validation { message })
                        }
                        StatusCode::NOT_FOUND /* 404 */ |
                        StatusCode::GONE /* 410 */
                         => {
                            self.metrics.increment_errors();
                            self.read_error_body(response, req.method.as_str(), &req.path).await?;
                            error!(?code, ?req, "http");
                            return Err(AnytypeError::NotFound{
                                // too generic here - we don't know whether the query
                                // needs to be reported at higher level
                                obj_type: "Object".into(),
                                key: String::default()
                            })
                        },
                        StatusCode::UNAUTHORIZED /* 401 */ => {
                            // client is not authenticated
                            self.metrics.increment_errors();
                            self.read_error_body(response, req.method.as_str(), &req.path).await?;
                            error!(?code, ?req, "http");
                            return Err(AnytypeError::Unauthorized)
                        }
                        StatusCode::FORBIDDEN /* 403 */ => {
                            // client is authenticated, but does not have permission to access the object
                            self.metrics.increment_errors();
                            self.read_error_body(response, req.method.as_str(), &req.path).await?;
                            error!(?code, ?req, "http");
                            return Err(AnytypeError::Forbidden)
                        }
                        _ => {
                            self.metrics.increment_errors();
                            let message = self.read_error_body(response, req.method.as_str(), &req.path).await?;
                            error!(?code, ?req, attempt, "http");
                            if attempt < MAX_RETRIES && retry_for_status(code) && is_idempotent_method(&req.method)
                            {
                              log_and_backoff(attempt, "retryable HTTP status").await;
                              self.metrics.increment_retries();
                              attempt += 1;
                              continue;
                            }
                            return Err(AnytypeError::ApiError{
                                code: code.as_u16(),
                                method: req.method.to_string(),
                                url: req.path,
                                message,
                            });
                        },
                    }
                }
                Err(err) => {
                    error!(?req, "HTTP transport failure");
                    // Check for connection or timeout errors
                    if (err.is_connect() || err.is_timeout()) && is_idempotent_method(&req.method) {
                        rate_limit_retries = 0;
                        if attempt < MAX_RETRIES {
                            log_and_backoff(attempt, "transport failure").await;
                            self.metrics.increment_retries();
                            attempt += 1;
                            continue;
                        }
                        self.metrics.increment_errors();
                        return Err(AnytypeError::Http {
                            method: req.method.to_string(),
                            url: req.path,
                            source: err,
                        });
                    }
                    // Other non-recoverable errors (e.g., DNS error, invalid URL, etc.)
                    self.metrics.increment_errors();
                    return Err(AnytypeError::Http {
                        method: req.method.to_string(),
                        url: req.path,
                        source: err,
                    });
                }
            }
        }
    }
}

// The purpose of this trait is to define methods for Arc<HttpClient>
pub trait GetPaged {
    async fn get_request_paged<T: DeserializeOwned + Send + 'static>(
        &self,
        path: &str,
        query: QueryWithFilters,
    ) -> Result<super::paged::PagedResult<T>>;

    async fn post_request_paged<T: DeserializeOwned + Send + 'static, B: Serialize + Sync>(
        &self,
        path: &str,
        body: &B,
        query: QueryWithFilters,
    ) -> Result<super::paged::PagedResult<T>>;
}

impl GetPaged for Arc<HttpClient> {
    /// Makes an authenticated GET request that returns a `PagedResult` for pagination support.
    async fn get_request_paged<T: DeserializeOwned + Send + 'static>(
        &self,
        path: &str,
        query: QueryWithFilters,
    ) -> Result<super::paged::PagedResult<T>> {
        query.validate().map_err(|err| AnytypeError::Validation {
            message: format!("get_request_paged {path} {err}"),
        })?;
        let req = HttpRequest {
            method: Method::GET,
            path: path.into(),
            query: query.params,
            body: None,
        };
        let response: PaginatedResponse<T> = self.send(req.clone()).await?;
        Ok(super::paged::PagedResult::new(response, self.clone(), req))
    }

    /// Makes an authenticated POST request that returns a `PagedResult` for pagination support.
    async fn post_request_paged<T: DeserializeOwned + Send + 'static, B: Serialize + Sync>(
        &self,
        path: &str,
        body: &B,
        query: QueryWithFilters,
    ) -> Result<super::paged::PagedResult<T>> {
        query.validate().map_err(|err| AnytypeError::Validation {
            message: format!("post_request_paged {path} {err}"),
        })?;
        let req = HttpRequest {
            method: Method::POST,
            path: path.into(),
            query: query.params,
            body: Some(Bytes::from(
                serde_json::to_vec(body).context(SerializationSnafu)?,
            )),
        };
        let response: PaginatedResponse<T> = self.send(req.clone()).await?;
        Ok(super::paged::PagedResult::new(response, self.clone(), req))
    }
}

// dump request
// requires RUST_LOG=anytype::http_json=trace
fn log_request(request: &HttpRequest) {
    if tracing::enabled!(target: "anytype::http_json", tracing::Level::TRACE) {
        trace!(
            target: "anytype::http_json",
            method = %request.method,
            path = request.path,
            query_fields = request.query.len(),
            body_bytes = request.body.as_ref().map_or(0, Bytes::len),
            "HTTP request"
        );
    }
}

// Log response metadata only. Anytype JSON may contain private document data.
fn log_response(path: &str, body: &Bytes) {
    if tracing::enabled!(target: "anytype::http_json", tracing::Level::TRACE) {
        trace!(target: "anytype::http_json", path, body_bytes = body.len(), "HTTP response");
    }
}

// deserialize, reporting errors with 'serde_path_to_error', which provides
// detailed json path to the error
fn deserialize_json<T: DeserializeOwned>(body: &[u8]) -> Result<T> {
    // Successful mutation endpoints in anytype-heart commonly return an empty
    // 200 response. Treat that as JSON null so callers can deserialize it as
    // `()` while response types that require an entity still fail normally.
    let body = if body.is_empty() { b"null" } else { body };
    let mut deserializer = serde_json::Deserializer::from_slice(body);
    match serde_path_to_error::deserialize(&mut deserializer) {
        Ok(value) => Ok(value),
        Err(err) => {
            error!("Deserialization failed at {}: {}", err.path(), err);
            Err(AnytypeError::Deserialization {
                source: err.into_inner(),
            })
        }
    }
}

// log attempt and sleep for exponential backoff
async fn log_and_backoff(attempt: u32, reason: &str) {
    // exponential backoff: 1s, 2s, 4s, with jitter
    #[allow(clippy::cast_precision_loss)]
    let base_delay = 2u64.pow(attempt) as f64;
    let jitter = f64::from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .subsec_nanos(),
    ) / 1_000_000_000.0;
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let jittered_delay = (base_delay * (0.5 + jitter)).round() as u64;
    let delay = if jittered_delay == 0 {
        1
    } else {
        jittered_delay
    };
    warn!("Recoverable {reason}. Attempt {attempt}. Waiting {delay}s before retry");
    tokio::time::sleep(Duration::from_secs(delay)).await;
}

fn is_idempotent_method(method: &Method) -> bool {
    matches!(
        *method,
        Method::GET | Method::HEAD | Method::PUT | Method::DELETE | Method::OPTIONS
    )
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use reqwest::{
        ClientBuilder, StatusCode,
        header::{HeaderMap, HeaderValue},
    };
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
        task::JoinHandle,
    };

    use super::{HttpClient, HttpRequest, deserialize_json, parse_retry_after};
    use crate::prelude::{
        HttpCredentials, MAX_JSON_RESPONSE_BYTES, ResponseLimits, ValidationLimits,
    };

    fn test_limits(json_bytes: u64, document_bytes: u64, error_bytes: u64) -> ResponseLimits {
        ResponseLimits {
            json_bytes,
            document_bytes,
            error_bytes,
            file_bytes: 1024,
        }
    }

    async fn serve_once(response: Vec<u8>) -> (Arc<HttpClient>, JoinHandle<()>) {
        serve_once_with_limits(response, test_limits(4, 8, 4)).await
    }

    async fn serve_once_with_limits(
        response: Vec<u8>,
        response_limits: ResponseLimits,
    ) -> (Arc<HttpClient>, JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test server");
        let address = listener.local_addr().expect("test server address");
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("accept request");
            let mut request = vec![0_u8; 4096];
            let _ = socket.read(&mut request).await.expect("read request");
            socket.write_all(&response).await.expect("write response");
        });
        let client = HttpClient::new(
            ClientBuilder::new().no_proxy(),
            format!("http://{address}"),
            ValidationLimits::default(),
            response_limits,
            1,
            HttpCredentials::new("test-token"),
        )
        .expect("test client");
        (Arc::new(client), server)
    }

    fn get_request() -> HttpRequest {
        HttpRequest {
            method: reqwest::Method::GET,
            path: "/test".to_string(),
            query: Vec::new(),
            body: None,
        }
    }

    #[test]
    fn response_limit_configuration_rejects_zero_and_hard_maximum_bypass() {
        for json_bytes in [0, MAX_JSON_RESPONSE_BYTES + 1] {
            let error = HttpClient::new(
                ClientBuilder::new().no_proxy(),
                "http://127.0.0.1:1".to_string(),
                ValidationLimits::default(),
                ResponseLimits {
                    json_bytes,
                    ..ResponseLimits::default()
                },
                1,
                HttpCredentials::new("test-token"),
            )
            .expect_err("invalid response limit");
            assert!(matches!(
                error,
                crate::error::AnytypeError::Validation { .. }
            ));
        }
    }

    #[tokio::test]
    async fn content_length_exact_limit_succeeds() {
        let response =
            b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\nnull".to_vec();
        let (client, server) = serve_once(response).await;

        client.send::<()>(get_request()).await.expect("exact limit");
        server.await.expect("server task");
        assert_eq!(client.metrics_snapshot().bytes_received, 4);
    }

    #[tokio::test]
    async fn oversized_content_length_fails_before_body_is_buffered() {
        let response =
            b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nnull ".to_vec();
        let (client, server) = serve_once(response).await;

        let error = client
            .send::<()>(get_request())
            .await
            .expect_err("declared over limit");
        assert!(matches!(
            error,
            crate::error::AnytypeError::ResponseTooLarge {
                limit: 4,
                declared: Some(5)
            }
        ));
        server.await.expect("server task");
        assert_eq!(client.metrics_snapshot().bytes_received, 0);
        assert_eq!(client.metrics_snapshot().errors, 1);
    }

    #[tokio::test]
    async fn chunked_exact_limit_succeeds_without_content_length() {
        let response = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n2\r\nnu\r\n2\r\nll\r\n0\r\n\r\n"
            .to_vec();
        let (client, server) = serve_once(response).await;

        client
            .send::<()>(get_request())
            .await
            .expect("exact chunks");
        server.await.expect("server task");
        assert_eq!(client.metrics_snapshot().bytes_received, 4);
    }

    #[tokio::test]
    async fn first_streamed_byte_over_limit_fails() {
        let response = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n4\r\nnull\r\n1\r\n \r\n0\r\n\r\n"
            .to_vec();
        let (client, server) = serve_once(response).await;

        let error = client
            .send::<()>(get_request())
            .await
            .expect_err("streamed byte over limit");
        assert!(matches!(
            error,
            crate::error::AnytypeError::ResponseTooLarge {
                limit: 4,
                declared: None
            }
        ));
        server.await.expect("server task");
        assert_eq!(client.metrics_snapshot().bytes_received, 5);
        assert_eq!(client.metrics_snapshot().errors, 1);
    }

    #[tokio::test]
    async fn chunked_framing_cannot_bypass_limit_with_a_low_length_header() {
        let response = b"HTTP/1.1 200 OK\r\nContent-Length: 1\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n5\r\nnull \r\n0\r\n\r\n"
            .to_vec();
        let (client, server) = serve_once(response).await;

        let error = client
            .send::<()>(get_request())
            .await
            .expect_err("transfer framing must not bypass streamed total");
        assert!(matches!(
            error,
            crate::error::AnytypeError::ResponseTooLarge { limit: 4, .. }
        ));
        server.await.expect("server task");
        assert_eq!(client.metrics_snapshot().bytes_received, 5);
    }

    #[tokio::test]
    async fn per_request_override_is_bounded_by_document_policy() {
        let response =
            b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nnull ".to_vec();
        let (client, server) = serve_once(response).await;
        client
            .get_request_with_limit::<()>("/test", crate::filters::QueryWithFilters::default(), 8)
            .await
            .expect("document override");
        server.await.expect("server task");

        let error = client
            .get_request_with_limit::<()>("/test", crate::filters::QueryWithFilters::default(), 9)
            .await
            .expect_err("override above configured document ceiling");
        assert!(matches!(
            error,
            crate::error::AnytypeError::Validation { .. }
        ));
    }

    #[tokio::test]
    async fn object_get_routes_complete_document_to_document_limit() {
        const OBJECT_ID: &str = "bafyreie6n5l5nkbjal37su54cha4coy7qzuhrnajluzv5qd5jvtsrxkequ";
        const SPACE_ID: &str =
            "bafyreid5fvqlnsobih2keakcxjrrlpmly6kf37klzjzen4ibfdgalcdp4y.2tq5w93cr6oe7";
        let body = format!(
            r#"{{"object":{{"archived":false,"id":"{OBJECT_ID}","space_id":"{SPACE_ID}","type":null}}}}"#
        );
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        )
        .into_bytes();
        let limits = test_limits(4, body.len() as u64, 4);
        let (client, server) = serve_once_with_limits(response, limits).await;

        let object = crate::objects::ObjectRequest::new(
            client,
            ValidationLimits::default(),
            SPACE_ID,
            OBJECT_ID,
        )
        .get()
        .await
        .expect("single-object reads use the document limit");
        server.await.expect("server task");
        assert_eq!(object.id, OBJECT_ID);
    }

    #[tokio::test]
    async fn oversized_error_body_uses_typed_limit_error() {
        let response = b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 5\r\nConnection: close\r\n\r\nerror"
            .to_vec();
        let (client, server) = serve_once(response).await;
        let error = client
            .send::<()>(get_request())
            .await
            .expect_err("error body over limit");

        assert!(matches!(
            error,
            crate::error::AnytypeError::ResponseTooLarge {
                limit: 4,
                declared: Some(5)
            }
        ));
        server.await.expect("server task");
        assert_eq!(client.metrics_snapshot().errors, 1);
        assert_eq!(client.metrics_snapshot().bytes_received, 0);
    }

    #[tokio::test]
    async fn unauthenticated_json_success_uses_generic_limit() {
        let response =
            b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nnull ".to_vec();
        let (client, server) = serve_once(response).await;
        let error = client
            .post_unauthenticated::<(), _>("/test", &serde_json::json!({}))
            .await
            .expect_err("unauthenticated JSON must be bounded");

        assert!(matches!(
            error,
            crate::error::AnytypeError::ResponseTooLarge { limit: 4, .. }
        ));
        server.await.expect("server task");
    }

    #[tokio::test]
    async fn multipart_json_success_uses_generic_limit() {
        let response =
            b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nnull ".to_vec();
        let (client, server) = serve_once(response).await;
        let form = reqwest::multipart::Form::new().text("file", "content");
        let error = client
            .post_multipart::<()>("/test", form)
            .await
            .expect_err("multipart JSON response must be bounded");

        assert!(matches!(
            error,
            crate::error::AnytypeError::ResponseTooLarge { limit: 4, .. }
        ));
        server.await.expect("server task");
    }

    #[tokio::test]
    async fn raw_file_success_uses_separate_file_limit() {
        let response =
            b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nbytes".to_vec();
        let limits = ResponseLimits {
            file_bytes: 4,
            ..test_limits(2, 8, 2)
        };
        let (client, server) = serve_once_with_limits(response, limits).await;
        let error = match client
            .file_request(reqwest::Method::GET, "/test", &[], HeaderMap::new())
            .await
        {
            Ok(_) => panic!("raw file response must use its own limit"),
            Err(error) => error,
        };

        assert!(matches!(
            error,
            crate::error::AnytypeError::ResponseTooLarge { limit: 4, .. }
        ));
        server.await.expect("server task");
    }

    #[tokio::test]
    async fn streamed_over_limit_stops_before_full_response_arrives() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind streaming server");
        let address = listener.local_addr().expect("streaming server address");
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("accept request");
            let mut request = vec![0_u8; 4096];
            let _ = socket.read(&mut request).await.expect("read request");
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n")
                .await
                .expect("write headers");
            let chunk = vec![b'x'; 1024];
            let mut sent = 0;
            for _ in 0..100 {
                if socket.write_all(b"400\r\n").await.is_err()
                    || socket.write_all(&chunk).await.is_err()
                    || socket.write_all(b"\r\n").await.is_err()
                    || socket.flush().await.is_err()
                {
                    break;
                }
                sent += 1;
                tokio::time::sleep(std::time::Duration::from_millis(2)).await;
            }
            sent
        });
        let client = HttpClient::new(
            ClientBuilder::new().no_proxy(),
            format!("http://{address}"),
            ValidationLimits::default(),
            test_limits(1024, 2048, 1024),
            1,
            HttpCredentials::new("test-token"),
        )
        .expect("streaming test client");

        let error = client
            .send::<()>(get_request())
            .await
            .expect_err("second chunk exceeds limit");
        assert!(matches!(
            error,
            crate::error::AnytypeError::ResponseTooLarge { limit: 1024, .. }
        ));
        let sent = server.await.expect("streaming server task");
        assert!(sent < 100, "client should close before all chunks are sent");
        assert!(client.metrics_snapshot().bytes_received < 100 * 1024);
    }

    #[test]
    fn empty_success_body_deserializes_as_unit() {
        deserialize_json::<()>(b"").expect("empty mutation response");
    }

    #[test]
    fn test_retry_for_status() {
        assert!(super::retry_for_status(StatusCode::TOO_MANY_REQUESTS));
        assert!(super::retry_for_status(StatusCode::REQUEST_TIMEOUT));
        assert!(super::retry_for_status(StatusCode::GATEWAY_TIMEOUT));
        assert!(!super::retry_for_status(StatusCode::INTERNAL_SERVER_ERROR));
    }

    #[test]
    fn test_parse_retry_after_ratelimit_reset() {
        let mut headers = HeaderMap::new();
        headers.insert("ratelimit-reset", HeaderValue::from_static("3"));
        let parsed = parse_retry_after(&headers).expect("parse retry header");
        assert_eq!(parsed.duration.as_secs(), 3);
        assert_eq!(parsed.header, "3");
    }

    #[test]
    fn test_parse_retry_after_x_rate_limit_duration() {
        let mut headers = HeaderMap::new();
        headers.insert("x-rate-limit-duration", HeaderValue::from_static("10"));
        let parsed = parse_retry_after(&headers).expect("parse retry header");
        assert_eq!(parsed.duration.as_secs(), 10);
        assert_eq!(parsed.header, "10");
    }
}
