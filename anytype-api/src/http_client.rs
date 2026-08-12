//! `HttpClient` middleware used by `AnytypeClient`
//!
//! Responsible for
//!  - handing all HTTP api requests
//!  - logging/tracing
//!  - retries and backoff (for timeouts and connection errors)
//!  - rate limiting

use std::{
    fmt,
    future::Future,
    sync::{
        Arc,
        atomic::{AtomicU32, AtomicU64, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use bytes::Bytes;
use futures::{StreamExt, stream::BoxStream};
use parking_lot::Mutex;
use reqwest::{ClientBuilder, Method, Response, StatusCode, Url, header::HeaderMap};
use serde::{Serialize, de::DeserializeOwned};
use tracing::{debug, error, info, trace, warn};

use crate::{
    Result,
    client::{
        MAX_DOCUMENT_RESPONSE_BYTES, MAX_ERROR_RESPONSE_BYTES, MAX_FILE_RESPONSE_BYTES,
        MAX_JSON_RESPONSE_BYTES, ResponseLimits,
    },
    config::{
        ANYTYPE_API_HEADER, MAX_HTTP_REQUEST_ATTEMPTS, MAX_RETRIES, RATE_LIMIT_WAIT_MAX_SECS,
        RATE_LIMIT_WAIT_WARN_SECS,
    },
    filters::QueryWithFilters,
    http_timeout::{HttpTimeoutClass, HttpTimeoutPolicy, TimeoutOutcome, timeout_outcome},
    prelude::*,
};

/// HTTP metrics tracked using atomic counters for thread-safe access.
/// These counters are cumulative and never reset during the client's lifetime.
#[derive(Debug, Default)]
pub struct HttpMetrics {
    /// Total number of logical HTTP operations entering the request pipeline
    logical_operations: AtomicU64,
    /// Total number of HTTP requests sent to the server (excludes cached responses)
    total_requests: AtomicU64,
    /// Total number of physical HTTP attempts, including automatic replays
    physical_attempts: AtomicU64,
    /// Total number of multipart POST requests dispatched
    multipart_posts: AtomicU64,
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
    timeout_counts: [AtomicU64; HttpTimeoutClass::COUNT],
    timeout_elapsed_millis: [AtomicU64; HttpTimeoutClass::COUNT],
    transport_timeout_count: AtomicU64,
    transport_timeout_elapsed_millis: AtomicU64,
    timeout_outcomes: [AtomicU64; TimeoutOutcome::COUNT],
}

impl HttpMetrics {
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns a snapshot of current metrics as plain u64 values
    pub fn snapshot(&self) -> HttpMetricsSnapshot {
        HttpMetricsSnapshot {
            logical_operations: self.logical_operations.load(Ordering::Relaxed),
            total_requests: self.total_requests.load(Ordering::Relaxed),
            physical_attempts: self.physical_attempts.load(Ordering::Relaxed),
            multipart_posts: self.multipart_posts.load(Ordering::Relaxed),
            successful_responses: self.successful_responses.load(Ordering::Relaxed),
            errors: self.errors.load(Ordering::Relaxed),
            retries: self.retries.load(Ordering::Relaxed),
            bytes_sent: self.bytes_sent.load(Ordering::Relaxed),
            bytes_received: self.bytes_received.load(Ordering::Relaxed),
            rate_limit_errors: self.rate_limit_errors.load(Ordering::Relaxed),
            rate_limit_delay_secs: self.rate_limit_delay_secs.load(Ordering::Relaxed),
            timeout_counts: std::array::from_fn(|index| {
                self.timeout_counts[index].load(Ordering::Relaxed)
            }),
            timeout_elapsed_millis: std::array::from_fn(|index| {
                self.timeout_elapsed_millis[index].load(Ordering::Relaxed)
            }),
            transport_timeout_count: self.transport_timeout_count.load(Ordering::Relaxed),
            transport_timeout_elapsed_millis: self
                .transport_timeout_elapsed_millis
                .load(Ordering::Relaxed),
            timeout_outcomes: std::array::from_fn(|index| {
                self.timeout_outcomes[index].load(Ordering::Relaxed)
            }),
        }
    }

    fn increment_logical_operations(&self) {
        self.logical_operations.fetch_add(1, Ordering::Relaxed);
    }

    fn increment_requests(&self) {
        self.total_requests.fetch_add(1, Ordering::Relaxed);
        self.physical_attempts.fetch_add(1, Ordering::Relaxed);
    }

    fn increment_multipart_posts(&self) {
        self.multipart_posts.fetch_add(1, Ordering::Relaxed);
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

    fn record_timeout(&self, class: HttpTimeoutClass, outcome: TimeoutOutcome, elapsed: Duration) {
        saturating_increment(&self.timeout_counts[class.index()]);
        saturating_add(
            &self.timeout_elapsed_millis[class.index()],
            duration_millis_saturating(elapsed),
        );
        saturating_increment(&self.timeout_outcomes[outcome.index()]);
    }

    fn record_transport_timeout(&self, outcome: TimeoutOutcome, elapsed: Duration) {
        saturating_increment(&self.transport_timeout_count);
        saturating_add(
            &self.transport_timeout_elapsed_millis,
            duration_millis_saturating(elapsed),
        );
        saturating_increment(&self.timeout_outcomes[outcome.index()]);
    }
}

fn saturating_increment(counter: &AtomicU64) {
    saturating_add(counter, 1);
}

fn saturating_add(counter: &AtomicU64, amount: u64) {
    let _ = counter.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
        Some(current.saturating_add(amount))
    });
}

fn duration_millis_saturating(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

/// A point-in-time snapshot of HTTP metrics with plain u64 values.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HttpMetricsSnapshot {
    /// Total number of logical HTTP operations entering the request pipeline
    pub logical_operations: u64,
    /// Total number of HTTP requests sent to the server
    pub total_requests: u64,
    /// Total number of physical HTTP attempts, including automatic replays
    pub physical_attempts: u64,
    /// Total number of multipart POST requests dispatched
    pub multipart_posts: u64,
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
    timeout_counts: [u64; HttpTimeoutClass::COUNT],
    timeout_elapsed_millis: [u64; HttpTimeoutClass::COUNT],
    transport_timeout_count: u64,
    transport_timeout_elapsed_millis: u64,
    timeout_outcomes: [u64; TimeoutOutcome::COUNT],
}

/// Cumulative observations for one timeout class.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TimeoutMetricSnapshot {
    /// Number of observations.
    pub count: u64,
    /// Saturating cumulative elapsed milliseconds.
    pub elapsed_millis: u64,
}

impl HttpMetricsSnapshot {
    /// Returns observations for one library logical timeout class.
    #[must_use]
    pub const fn timeout(self, class: HttpTimeoutClass) -> TimeoutMetricSnapshot {
        TimeoutMetricSnapshot {
            count: self.timeout_counts[class.index()],
            elapsed_millis: self.timeout_elapsed_millis[class.index()],
        }
    }

    /// Returns caller transport timeout observations.
    #[must_use]
    pub const fn transport_timeouts(self) -> TimeoutMetricSnapshot {
        TimeoutMetricSnapshot {
            count: self.transport_timeout_count,
            elapsed_millis: self.transport_timeout_elapsed_millis,
        }
    }

    /// Returns observations for one timeout outcome.
    #[must_use]
    pub const fn timeout_outcome_count(self, outcome: TimeoutOutcome) -> u64 {
        self.timeout_outcomes[outcome.index()]
    }
}

impl std::fmt::Display for HttpMetricsSnapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "logical_operations={} requests={} physical_attempts={} multipart_posts={} success={} errors={} retries={} rate_limit={}/{}s sent={} recv={}",
            self.logical_operations,
            self.total_requests,
            self.physical_attempts,
            self.multipart_posts,
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

fn mutation_status_is_indeterminate(status: StatusCode) -> bool {
    matches!(
        status,
        StatusCode::REQUEST_TIMEOUT | StatusCode::TOO_MANY_REQUESTS
    ) || status.is_server_error()
}

fn log_http_status(
    request: &HttpRequest,
    status: StatusCode,
    variant: &'static str,
    physical_attempt: u32,
) {
    error!(
        target: "anytype::http",
        error_variant = variant,
        http_status = status.as_u16(),
        http_method = %request.method,
        http_path = %diagnostic_path(&request.path),
        physical_attempt,
        "HTTP request failed"
    );
}

fn log_http_transport(request: &HttpRequest, physical_attempt: u32) {
    error!(
        target: "anytype::http",
        error_variant = "transport",
        http_method = %request.method,
        http_path = %diagnostic_path(&request.path),
        physical_attempt,
        "HTTP request failed"
    );
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

/// Successful HTTP streaming response with established-stream deadlines.
pub(crate) struct StreamingHttpResponse {
    chunks: BoxStream<'static, Result<Bytes>>,
}

impl StreamingHttpResponse {
    pub(crate) fn bytes_stream(self) -> BoxStream<'static, Result<Bytes>> {
        self.chunks
    }
}

struct EstablishedSseState {
    chunks: BoxStream<'static, std::result::Result<Bytes, reqwest::Error>>,
    metrics: Arc<HttpMetrics>,
    path: String,
    started: tokio::time::Instant,
    last_progress: tokio::time::Instant,
    idle: Option<Duration>,
    lifetime: Option<Duration>,
    terminated: bool,
}

impl EstablishedSseState {
    async fn next(mut self) -> Option<(Result<Bytes>, Self)> {
        if self.terminated {
            return None;
        }
        let idle_deadline = self
            .idle
            .and_then(|idle| self.last_progress.checked_add(idle));
        let lifetime_deadline = self
            .lifetime
            .and_then(|lifetime| self.started.checked_add(lifetime));
        let now = tokio::time::Instant::now();
        if lifetime_deadline.is_some_and(|deadline| now >= deadline) {
            return Some(self.terminate_with_timeout(HttpTimeoutClass::SseLifetime));
        }
        if idle_deadline.is_some_and(|deadline| now >= deadline) {
            return Some(self.terminate_with_timeout(HttpTimeoutClass::SseIdle));
        }
        tokio::select! {
            biased;
            () = wait_until(lifetime_deadline) => {
                Some(self.terminate_with_timeout(HttpTimeoutClass::SseLifetime))
            },
            () = wait_until(idle_deadline) => {
                Some(self.terminate_with_timeout(HttpTimeoutClass::SseIdle))
            },
            chunk = self.chunks.next() => match chunk {
                Some(Ok(chunk)) => {
                    if !chunk.is_empty() {
                        self.last_progress = tokio::time::Instant::now();
                    }
                    Some((Ok(chunk), self))
                }
                Some(Err(source)) => {
                    self.terminated = true;
                    self.metrics.increment_errors();
                    let error = if source.is_timeout() {
                        let elapsed = self.started.elapsed();
                        self.metrics.record_transport_timeout(
                            TimeoutOutcome::StreamTerminated,
                            elapsed,
                        );
                        warn!(
                            target: "anytype::http",
                            error_variant = "transport_timeout",
                            timeout_outcome = %TimeoutOutcome::StreamTerminated,
                            elapsed_millis = duration_millis_saturating(elapsed),
                            http_method = "GET",
                            http_path = %diagnostic_path(&self.path),
                            physical_attempt = 1_u32,
                            "HTTP established stream transport timeout"
                        );
                        AnytypeError::Http {
                            method: "GET".to_owned(),
                            url: self.path.clone(),
                            source: reqwest::Error::without_url(source),
                            outcome: Some(TimeoutOutcome::StreamTerminated),
                            elapsed: Some(elapsed),
                            attempts: Some(1),
                        }
                    } else {
                        AnytypeError::ChatSseTransport {
                            path: diagnostic_path(&self.path),
                        }
                    };
                    self.terminate();
                    Some((Err(error), self))
                }
                None => None,
            },
        }
    }

    fn terminate_with_timeout(mut self, class: HttpTimeoutClass) -> (Result<Bytes>, Self) {
        let error = self.logical_timeout(class);
        self.terminate();
        (Err(error), self)
    }

    fn terminate(&mut self) {
        self.terminated = true;
        self.chunks = futures::stream::empty().boxed();
    }

    fn logical_timeout(&self, class: HttpTimeoutClass) -> AnytypeError {
        let elapsed = self.started.elapsed();
        self.metrics.increment_errors();
        self.metrics
            .record_timeout(class, TimeoutOutcome::StreamTerminated, elapsed);
        warn!(
            target: "anytype::http",
            error_variant = "logical_timeout",
            timeout_class = %class,
            timeout_outcome = %TimeoutOutcome::StreamTerminated,
            elapsed_millis = duration_millis_saturating(elapsed),
            http_method = "GET",
            http_path = %diagnostic_path(&self.path),
            physical_attempt = 1_u32,
            "HTTP established stream deadline expired"
        );
        AnytypeError::HttpTimeout {
            class,
            outcome: TimeoutOutcome::StreamTerminated,
            method: "GET".to_owned(),
            path: self.path.clone(),
            elapsed,
            attempts: 1,
        }
    }
}

async fn wait_until(deadline: Option<tokio::time::Instant>) {
    if let Some(deadline) = deadline {
        tokio::time::sleep_until(deadline).await;
    } else {
        std::future::pending::<()>().await;
    }
}

/// Result of a completed mutation whose exact HTTP rejection status is part of
/// the caller's safety decision.
pub(crate) enum PreservedStatusResponse<T> {
    /// The server returned a successful status and a valid response body.
    Success(T),
    /// The server definitively rejected the mutation with this exact status.
    Rejected { status: u16 },
    /// The server returned a status that cannot prove whether the mutation ran.
    Indeterminate { status: u16 },
}

impl fmt::Debug for HttpRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HttpRequest")
            .field("method", &self.method)
            .field("path", &diagnostic_path(&self.path))
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

    credential_state: Arc<Mutex<HttpCredentialState>>,

    limits: ValidationLimits,

    response_limits: ResponseLimits,

    // Max consecutive 429 retries before failing; 0 disables cap.
    rate_limit_max_retries: u32,

    timeout_policy: HttpTimeoutPolicy,

    /// HTTP request/response metrics
    pub metrics: Arc<HttpMetrics>,
}

#[derive(Clone)]
struct HttpCredentialState {
    credentials: HttpCredentials,
    generation: u64,
}

impl fmt::Debug for HttpClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HttpClient")
            .field("base_path", &diagnostic_path(&self.base_url))
            .field("api_key", &String::from("(MASKED)"))
            .field("rate_limit_max_retries", &self.rate_limit_max_retries)
            .field("timeout_policy", &self.timeout_policy)
            .field("metrics", &self.metrics)
            .finish_non_exhaustive()
    }
}

const MAX_DIAGNOSTIC_PATH_CHARS: usize = 512;
const REDACTED_DIAGNOSTIC_PATH: &str = "/[redacted]";

/// Returns bounded path-only context for HTTP diagnostics.
///
/// Valid absolute and scheme-relative HTTP URLs lose their scheme, authority,
/// userinfo, query, and fragment. Valid origin-form request targets lose query
/// and fragment values. Malformed, non-HTTP, control-bearing, and other target
/// forms fail closed to a fixed redaction marker rather than echoing input.
pub(crate) fn diagnostic_path(value: &str) -> String {
    let parsed = parse_diagnostic_target(value);
    let Some(path) = parsed.as_ref().map(Url::path) else {
        return REDACTED_DIAGNOSTIC_PATH.to_owned();
    };
    let mut redacted = String::with_capacity(path.len().min(MAX_DIAGNOSTIC_PATH_CHARS));
    for character in path.chars().take(MAX_DIAGNOSTIC_PATH_CHARS) {
        redacted.push(if character.is_control() {
            '�'
        } else {
            character
        });
    }
    if path.chars().count() > MAX_DIAGNOSTIC_PATH_CHARS {
        redacted.push('…');
    }
    if redacted.is_empty() {
        redacted.push('/');
    }
    redacted
}

fn parse_diagnostic_target(value: &str) -> Option<Url> {
    if value.is_empty()
        || value.chars().any(char::is_control)
        || value.chars().any(char::is_whitespace)
        || value.contains('\\')
        || has_invalid_percent_encoding(value)
    {
        return None;
    }
    let base = Url::parse("http://redacted.invalid/").ok()?;
    if let Some(remainder) = value.strip_prefix("//") {
        if remainder.starts_with('/') {
            return None;
        }
        let parsed = base.join(value).ok()?;
        return parsed.has_host().then_some(parsed);
    }
    if value.starts_with('/') {
        return base.join(value).ok();
    }
    let parsed = Url::parse(value).ok()?;
    let (_, remainder) = value.split_once(':')?;
    (matches!(parsed.scheme(), "http" | "https")
        && remainder.starts_with("//")
        && !remainder[2..].starts_with('/')
        && parsed.has_host())
    .then_some(parsed)
}

fn has_invalid_percent_encoding(value: &str) -> bool {
    let bytes = value.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            if index + 2 >= bytes.len()
                || !bytes[index + 1].is_ascii_hexdigit()
                || !bytes[index + 2].is_ascii_hexdigit()
            {
                return true;
            }
            index += 3;
        } else {
            index += 1;
        }
    }
    false
}

struct ParsedRetry {
    header: String,
    duration: Duration,
}

#[derive(Clone)]
struct OperationTiming {
    started: tokio::time::Instant,
    attempts: Arc<AtomicU32>,
}

impl OperationTiming {
    fn start() -> Self {
        Self {
            started: tokio::time::Instant::now(),
            attempts: Arc::new(AtomicU32::new(0)),
        }
    }

    fn record_attempt(&self) -> u32 {
        self.attempts
            .fetch_add(1, Ordering::Relaxed)
            .saturating_add(1)
    }

    fn attempts(&self) -> u32 {
        self.attempts.load(Ordering::Relaxed)
    }

    fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }
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
        timeout_policy: HttpTimeoutPolicy,
        http_creds: HttpCredentials,
    ) -> Result<Self> {
        // Keep this middleware as the sole retry authority. Reqwest follows
        // redirects and retries protocol NACKs by default, and callers may
        // install an even broader policy on ClientBuilder. Either path sits
        // below send(), where method safety, attempt metrics, and mutation
        // dispatch state cannot be enforced. A redirect is surfaced as its
        // original 3xx response so callers can reject it without forwarding
        // credentials or replaying a body to another endpoint.
        let client = builder
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .build()
            .map_err(reqwest::Error::without_url)
            .map_err(|source| AnytypeError::Http {
                method: "client-init".to_owned(),
                url: String::new(),
                source,
                outcome: None,
                elapsed: None,
                attempts: None,
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
            (
                "chat_sse_event_bytes",
                response_limits.chat_sse_event_bytes,
                crate::client::MAX_CHAT_SSE_EVENT_BYTES,
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
            credential_state: Arc::new(Mutex::new(HttpCredentialState {
                credentials: http_creds,
                generation: 0,
            })),
            limits,
            response_limits,
            rate_limit_max_retries,
            timeout_policy,
            metrics: Arc::new(HttpMetrics::new()),
        })
    }

    /// Returns a snapshot of current HTTP metrics
    pub fn metrics_snapshot(&self) -> HttpMetricsSnapshot {
        self.metrics.snapshot()
    }

    async fn with_deadline<T>(
        &self,
        class: HttpTimeoutClass,
        outcome: TimeoutOutcome,
        method: &Method,
        path: &str,
        timing: &OperationTiming,
        operation: impl Future<Output = Result<T>>,
    ) -> Result<T> {
        // Boxing bounds every helper's composed future at one pointer, so
        // deep call chains (pagination, resolution, discovery) cannot grow
        // caller stack frames with the whole deadline-wrapped request state.
        // One heap allocation per logical request is negligible next to the
        // wire round trip.
        let operation = Box::pin(operation);
        let Some(duration) = self.timeout_policy.duration(class) else {
            return match operation.await {
                Err(AnytypeError::Http { source, .. }) => {
                    Err(self.classify_transport_error(source, method, path, timing))
                }
                result => result,
            };
        };
        let deadline =
            timing
                .started
                .checked_add(duration)
                .ok_or_else(|| AnytypeError::Validation {
                    message: format!("{class} HTTP deadline cannot be represented"),
                })?;
        match tokio::time::timeout_at(deadline, operation).await {
            Ok(Err(AnytypeError::Http { source, .. })) => {
                Err(self.classify_transport_error(source, method, path, timing))
            }
            // A completed result in hand is returned as-is, even when it
            // becomes ready on the same tick the deadline expires: the
            // deadline converts waiting into a timeout, never a completed
            // outcome, so success and typed errors are not discarded and
            // metrics count exactly one outcome per logical operation.
            // `ensure_before_dispatch` still prevents a new physical send at
            // or after the deadline instant.
            Ok(result) => result,
            Err(_) => Err(self.logical_timeout_error(class, outcome, method, path, timing)),
        }
    }

    fn logical_timeout_error(
        &self,
        class: HttpTimeoutClass,
        outcome: TimeoutOutcome,
        method: &Method,
        path: &str,
        timing: &OperationTiming,
    ) -> AnytypeError {
        let elapsed = timing.elapsed();
        let attempts = timing.attempts();
        self.metrics.increment_errors();
        self.metrics.record_timeout(class, outcome, elapsed);
        warn!(
            target: "anytype::http",
            error_variant = "logical_timeout",
            timeout_class = %class,
            timeout_outcome = %outcome,
            elapsed_millis = duration_millis_saturating(elapsed),
            http_method = %method,
            http_path = %diagnostic_path(path),
            physical_attempt = attempts,
            "HTTP logical deadline expired"
        );
        AnytypeError::HttpTimeout {
            class,
            outcome,
            method: method.as_str().to_owned(),
            path: path.to_owned(),
            elapsed,
            attempts,
        }
    }

    fn ensure_before_dispatch(
        &self,
        class: HttpTimeoutClass,
        outcome: TimeoutOutcome,
        method: &Method,
        path: &str,
        timing: &OperationTiming,
    ) -> Result<()> {
        let Some(duration) = self.timeout_policy.duration(class) else {
            return Ok(());
        };
        let deadline =
            timing
                .started
                .checked_add(duration)
                .ok_or_else(|| AnytypeError::Validation {
                    message: format!("{class} HTTP deadline cannot be represented"),
                })?;
        if tokio::time::Instant::now() >= deadline {
            return Err(self.logical_timeout_error(class, outcome, method, path, timing));
        }
        Ok(())
    }

    fn transport_error(
        &self,
        source: reqwest::Error,
        method: &Method,
        path: &str,
        _timing: &OperationTiming,
    ) -> AnytypeError {
        AnytypeError::Http {
            method: method.as_str().to_owned(),
            url: path.to_owned(),
            source,
            outcome: None,
            elapsed: None,
            attempts: None,
        }
    }

    fn classify_transport_error(
        &self,
        source: reqwest::Error,
        method: &Method,
        path: &str,
        timing: &OperationTiming,
    ) -> AnytypeError {
        let outcome = timeout_outcome(method);
        if source.is_timeout() {
            let elapsed = timing.elapsed();
            self.metrics.record_transport_timeout(outcome, elapsed);
            warn!(
                target: "anytype::http",
                error_variant = "transport_timeout",
                timeout_outcome = %outcome,
                elapsed_millis = duration_millis_saturating(elapsed),
                http_method = %method,
                http_path = %diagnostic_path(path),
                physical_attempt = timing.attempts(),
                "HTTP caller transport timeout"
            );
            AnytypeError::Http {
                method: method.as_str().to_owned(),
                url: path.to_owned(),
                source,
                outcome: Some(outcome),
                elapsed: Some(elapsed),
                attempts: Some(timing.attempts()),
            }
        } else if outcome == TimeoutOutcome::MutationIndeterminate
            && timing.attempts() > 0
            && !source.is_connect()
            && !source.is_builder()
        {
            // Connect and builder failures happen before any bytes reach the
            // peer, so the mutation was provably never dispatched and the
            // typed transport error (with its reqwest source) is returned
            // instead of an indeterminate outcome.
            AnytypeError::HttpMutationIndeterminate {
                method: method.as_str().to_owned(),
                path: path.to_owned(),
                attempts: timing.attempts(),
                status: None,
            }
        } else {
            AnytypeError::Http {
                method: method.as_str().to_owned(),
                url: path.to_owned(),
                source,
                outcome: None,
                elapsed: None,
                attempts: None,
            }
        }
    }

    fn mutation_indeterminate(
        method: &Method,
        path: &str,
        attempts: u32,
        status: StatusCode,
    ) -> AnytypeError {
        AnytypeError::HttpMutationIndeterminate {
            method: method.as_str().to_owned(),
            path: path.to_owned(),
            attempts,
            status: Some(status.as_u16()),
        }
    }

    pub(crate) const fn document_response_limit(&self) -> u64 {
        self.response_limits.document_bytes
    }

    pub(crate) const fn file_response_limit(&self) -> u64 {
        self.response_limits.file_bytes
    }

    pub(crate) const fn error_response_limit(&self) -> u64 {
        self.response_limits.error_bytes
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
        let initial_capacity = limit
            .min(declared.unwrap_or(INITIAL_CAPACITY))
            .min(INITIAL_CAPACITY);
        let mut body = Vec::with_capacity(initial_capacity as usize);
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(reqwest::Error::without_url)
            .map_err(|source| AnytypeError::Http {
                method: method.to_owned(),
                url: path.to_owned(),
                source,
                outcome: None,
                elapsed: None,
                attempts: None,
            })?
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
        self.credential_state.lock().credentials.has_creds()
    }

    /// Sets the API key for authenticated requests.
    pub fn set_api_key(&self, api_key: HttpCredentials) {
        let mut state = self.credential_state.lock();
        state.credentials = api_key;
        state.generation = state.generation.saturating_add(1);
    }

    /// Clears the api key if set. (in memory, does not change keystore)
    pub fn clear_api_key(&self) {
        let mut state = self.credential_state.lock();
        state.credentials = HttpCredentials::default();
        state.generation = state.generation.saturating_add(1);
    }

    /// Returns the non-secret generation of the in-memory HTTP credentials.
    pub fn credential_generation(&self) -> u64 {
        self.credential_state.lock().generation
    }

    /// Returns http token from memory (Does not refresh from keystore)
    pub(crate) fn get_api_key(&self) -> HttpCredentials {
        self.credential_state.lock().credentials.clone()
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

    /// Makes one authenticated JSON DELETE without middleware retries.
    pub(crate) async fn delete_request_once<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        let req = HttpRequest {
            method: Method::DELETE,
            path: path.into(),
            query: Vec::default(),
            body: None,
        };
        self.send_with_limit_and_retries(req, self.response_limits.json_bytes, false)
            .await
    }

    /// Makes an authenticated DELETE whose successful JSON may contain a
    /// complete document body.
    pub(crate) async fn delete_document_request<T: DeserializeOwned>(
        &self,
        path: &str,
    ) -> Result<T> {
        self.delete_document_request_with_retries(path, true).await
    }

    /// Makes one authenticated DELETE whose successful JSON may contain a
    /// complete document body, without replaying the request in middleware.
    pub(crate) async fn delete_document_request_once<T: DeserializeOwned>(
        &self,
        path: &str,
    ) -> Result<T> {
        self.delete_document_request_with_retries(path, false).await
    }

    async fn delete_document_request_with_retries<T: DeserializeOwned>(
        &self,
        path: &str,
        allow_retries: bool,
    ) -> Result<T> {
        let req = HttpRequest {
            method: Method::DELETE,
            path: path.into(),
            query: Vec::default(),
            body: None,
        };
        self.send_with_limit_and_retries(req, self.response_limits.document_bytes, allow_retries)
            .await
    }

    pub(crate) async fn get_request<T: DeserializeOwned>(
        &self,
        path: &str,
        query: QueryWithFilters,
    ) -> Result<T> {
        query.validate().map_err(|err| AnytypeError::Validation {
            message: format!("get_request {} {err}", diagnostic_path(path)),
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
            message: format!("get_request_with_limit {} {err}", diagnostic_path(path)),
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
    ) -> Result<StreamingHttpResponse> {
        query.validate().map_err(|err| AnytypeError::Validation {
            message: format!("get_streaming_request {} {err}", diagnostic_path(path)),
        })?;
        self.limits.validate_query(&query.params)?;

        let api_key = self.get_api_key();
        let Some(token) = api_key.token() else {
            return Err(AnytypeError::Auth {
                message: "HTTP credentials missing token. Client is not authenticated.".to_owned(),
            });
        };
        let full_url = format!("{}{}", self.base_url, path);
        debug!(path = %diagnostic_path(path), "get_streaming_request");
        self.metrics.increment_logical_operations();
        let open_timing = OperationTiming::start();
        let response = self
            .with_deadline(
                HttpTimeoutClass::SseOpen,
                TimeoutOutcome::ReadAborted,
                &Method::GET,
                path,
                &open_timing,
                async {
                    self.ensure_before_dispatch(
                        HttpTimeoutClass::SseOpen,
                        TimeoutOutcome::ReadAborted,
                        &Method::GET,
                        path,
                        &open_timing,
                    )?;
                    open_timing.record_attempt();
                    self.metrics.increment_requests();
                    self.client
                        .get(&full_url)
                        .query(&query.params)
                        .header(ANYTYPE_API_HEADER, ANYTYPE_API_VERSION)
                        .bearer_auth(token)
                        .headers(headers)
                        .send()
                        .await
                        .map_err(reqwest::Error::without_url)
                        .map_err(|source| {
                            if source.is_timeout() {
                                self.transport_error(source, &Method::GET, path, &open_timing)
                            } else {
                                AnytypeError::ChatSseTransport {
                                    path: diagnostic_path(path),
                                }
                            }
                        })
                },
            )
            .await?;

        if !response.status().is_success() {
            self.metrics.increment_errors();
            let code = response.status().as_u16();
            let error_timing = OperationTiming::start();
            self.ensure_before_dispatch(
                HttpTimeoutClass::SseErrorBody,
                TimeoutOutcome::ReadAborted,
                &Method::GET,
                path,
                &error_timing,
            )?;
            error_timing.record_attempt();
            let message = self
                .with_deadline(
                    HttpTimeoutClass::SseErrorBody,
                    TimeoutOutcome::ReadAborted,
                    &Method::GET,
                    path,
                    &error_timing,
                    self.read_error_body(response, "get", path),
                )
                .await?;
            return Err(AnytypeError::ApiError {
                code,
                method: "get".to_string(),
                url: path.to_string(),
                message,
            });
        }

        self.metrics.increment_success();
        let established = tokio::time::Instant::now();
        let state = EstablishedSseState {
            chunks: response.bytes_stream().boxed(),
            metrics: self.metrics.clone(),
            path: path.to_owned(),
            started: established,
            last_progress: established,
            idle: self.timeout_policy.sse_idle,
            lifetime: self.timeout_policy.sse_total_lifetime,
            terminated: false,
        };
        Ok(StreamingHttpResponse {
            chunks: futures::stream::unfold(state, EstablishedSseState::next).boxed(),
        })
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
                serde_json::to_vec(body)
                    .map_err(|source| AnytypeError::Serialization { source })?,
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
                serde_json::to_vec(body)
                    .map_err(|source| AnytypeError::Serialization { source })?,
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
                serde_json::to_vec(body)
                    .map_err(|source| AnytypeError::Serialization { source })?,
            )),
        };
        self.send(req).await
    }

    /// Makes one authenticated POST attempt while preserving its exact
    /// completed non-success status.
    ///
    /// This path is intentionally narrow: it never follows redirects, retries,
    /// or maps statuses into broader [`AnytypeError`] variants. Transport,
    /// response-read, and deserialization failures remain errors because they
    /// do not prove the mutation outcome.
    pub(crate) async fn post_request_preserve_status<T: DeserializeOwned, B: Serialize + Sync>(
        &self,
        path: &str,
        body: &B,
        query: QueryWithFilters,
    ) -> Result<PreservedStatusResponse<T>> {
        let req = HttpRequest {
            method: Method::POST,
            path: path.into(),
            query: query.params,
            body: Some(Bytes::from(
                serde_json::to_vec(body)
                    .map_err(|source| AnytypeError::Serialization { source })?,
            )),
        };
        self.send_preserving_status(req).await
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
                serde_json::to_vec(body)
                    .map_err(|source| AnytypeError::Serialization { source })?,
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
        let timing = OperationTiming::start();
        self.with_deadline(
            HttpTimeoutClass::StandardOperation,
            TimeoutOutcome::MutationIndeterminate,
            &Method::POST,
            path,
            &timing,
            self.post_unauthenticated_inner(path, body, timing.clone()),
        )
        .await
    }

    async fn post_unauthenticated_inner<Resp: DeserializeOwned, Req: Serialize + Sync>(
        &self,
        path: &str,
        body: &Req,
        timing: OperationTiming,
    ) -> Result<Resp> {
        let full_url = format!("{}{}", self.base_url, path);
        debug!(path = %diagnostic_path(path), "post_unauthenticated");
        self.metrics.increment_logical_operations();
        self.ensure_before_dispatch(
            HttpTimeoutClass::StandardOperation,
            TimeoutOutcome::MutationIndeterminate,
            &Method::POST,
            path,
            &timing,
        )?;
        timing.record_attempt();
        self.metrics.increment_requests();
        let response = self
            .client
            .post(&full_url)
            .header(ANYTYPE_API_HEADER, ANYTYPE_API_VERSION)
            .json(body)
            .send()
            .await
            .map_err(reqwest::Error::without_url)
            .map_err(|source| self.transport_error(source, &Method::POST, path, &timing))?;
        if !response.status().is_success() {
            self.metrics.increment_errors();
            let status = response.status();
            let code = status.as_u16();
            // The ambiguous status alone fixes the classification, so the
            // error-body read is best-effort diagnostics and must not mask
            // the indeterminate outcome.
            let body = self.read_error_body(response, "post", path).await;
            if mutation_status_is_indeterminate(status) {
                return Err(Self::mutation_indeterminate(
                    &Method::POST,
                    path,
                    timing.attempts(),
                    status,
                ));
            }
            let message = body?;
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
    /// response entity; this variant is for non-file endpoints that return
    /// `204` with no body (currently chat message deletion). REST file
    /// deletion — including permanent deletion — routes through
    /// [`file_request`](Self::file_request) instead, which applies the long
    /// timeout profile that the observed ~154-second permanent delete
    /// requires. Do not route file deletes back through this
    /// standard-profile helper.
    pub(crate) async fn delete_no_content(&self, path: &str) -> Result<()> {
        let timing = OperationTiming::start();
        self.with_deadline(
            HttpTimeoutClass::StandardOperation,
            TimeoutOutcome::MutationIndeterminate,
            &Method::DELETE,
            path,
            &timing,
            self.delete_no_content_inner(path, timing.clone()),
        )
        .await
    }

    async fn delete_no_content_inner(&self, path: &str, timing: OperationTiming) -> Result<()> {
        let api_key = self.get_api_key();
        let Some(token) = api_key.token() else {
            return Err(AnytypeError::Auth {
                message: "HTTP credentials missing token. Client is not authenticated.".to_owned(),
            });
        };
        let full_url = format!("{}{}", self.base_url, path);
        debug!(path = %diagnostic_path(path), "delete_no_content");
        self.metrics.increment_logical_operations();
        self.ensure_before_dispatch(
            HttpTimeoutClass::StandardOperation,
            TimeoutOutcome::MutationIndeterminate,
            &Method::DELETE,
            path,
            &timing,
        )?;
        timing.record_attempt();
        self.metrics.increment_requests();
        let response = self
            .client
            .delete(&full_url)
            .header(ANYTYPE_API_HEADER, ANYTYPE_API_VERSION)
            .bearer_auth(token)
            .send()
            .await
            .map_err(reqwest::Error::without_url)
            .map_err(|source| self.transport_error(source, &Method::DELETE, path, &timing))?;
        if !response.status().is_success() {
            self.metrics.increment_errors();
            let status = response.status();
            let code = status.as_u16();
            // The ambiguous status alone fixes the classification, so the
            // error-body read is best-effort diagnostics and must not mask
            // the indeterminate outcome.
            let body = self.read_error_body(response, "delete", path).await;
            if mutation_status_is_indeterminate(status) {
                return Err(Self::mutation_indeterminate(
                    &Method::DELETE,
                    path,
                    timing.attempts(),
                    status,
                ));
            }
            let message = body?;
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
        self.file_request_with_limits(
            method,
            path,
            query,
            headers,
            self.response_limits.file_bytes,
            self.response_limits.error_bytes,
            crate::files::DEFAULT_FILE_HEADER_EVIDENCE_BYTES,
            1,
        )
        .await
    }

    /// Makes an authenticated file request under caller-specific finite limits.
    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    pub(crate) async fn file_request_with_limits(
        &self,
        method: Method,
        path: &str,
        query: &[(String, String)],
        headers: HeaderMap,
        success_body_limit: u64,
        error_body_limit: u64,
        header_evidence_limit: u64,
        max_attempts: u32,
    ) -> Result<RawHttpResponse> {
        let timing = OperationTiming::start();
        let diagnostic_method = method.clone();
        self.with_deadline(
            HttpTimeoutClass::LongOperation,
            timeout_outcome(&diagnostic_method),
            &diagnostic_method,
            path,
            &timing,
            self.file_request_with_limits_inner(
                method,
                path,
                query,
                headers,
                success_body_limit,
                error_body_limit,
                header_evidence_limit,
                max_attempts,
                timing.clone(),
            ),
        )
        .await
    }

    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    async fn file_request_with_limits_inner(
        &self,
        method: Method,
        path: &str,
        query: &[(String, String)],
        headers: HeaderMap,
        success_body_limit: u64,
        error_body_limit: u64,
        header_evidence_limit: u64,
        max_attempts: u32,
        timing: OperationTiming,
    ) -> Result<RawHttpResponse> {
        if success_body_limit == 0 || success_body_limit > self.response_limits.file_bytes {
            return Err(AnytypeError::Validation {
                message: format!(
                    "file response limit must be between 1 and {} bytes",
                    self.response_limits.file_bytes
                ),
            });
        }
        if error_body_limit == 0 || error_body_limit > self.response_limits.error_bytes {
            return Err(AnytypeError::Validation {
                message: format!(
                    "file error response limit must be between 1 and {} bytes",
                    self.response_limits.error_bytes
                ),
            });
        }
        if header_evidence_limit == 0
            || header_evidence_limit > crate::files::MAX_FILE_HEADER_EVIDENCE_BYTES
        {
            return Err(AnytypeError::Validation {
                message: format!(
                    "file header evidence limit must be between 1 and {} bytes",
                    crate::files::MAX_FILE_HEADER_EVIDENCE_BYTES
                ),
            });
        }
        if max_attempts == 0 || max_attempts > crate::files::MAX_FILE_REQUEST_ATTEMPTS {
            return Err(AnytypeError::Validation {
                message: format!(
                    "file request attempts must be between 1 and {}",
                    crate::files::MAX_FILE_REQUEST_ATTEMPTS
                ),
            });
        }
        let api_key = self.get_api_key();
        let Some(token) = api_key.token() else {
            return Err(AnytypeError::Auth {
                message: "HTTP credentials missing token. Client is not authenticated.".to_owned(),
            });
        };
        let full_url = format!("{}{}", self.base_url, path);
        debug!(method = %method, path = %diagnostic_path(path), "file_request");
        let replay_safe = matches!(method, Method::GET | Method::HEAD);
        self.metrics.increment_logical_operations();
        loop {
            self.ensure_before_dispatch(
                HttpTimeoutClass::LongOperation,
                timeout_outcome(&method),
                &method,
                path,
                &timing,
            )?;
            let attempts = timing.record_attempt();
            self.metrics.increment_requests();
            let sent = self
                .client
                .request(method.clone(), &full_url)
                .query(query)
                .header(ANYTYPE_API_HEADER, ANYTYPE_API_VERSION)
                .bearer_auth(token)
                .headers(headers.clone())
                .send()
                .await
                .map_err(reqwest::Error::without_url);
            let response = match sent {
                Ok(response) => response,
                Err(source) if replay_safe && attempts < max_attempts && source.is_connect() => {
                    self.metrics.increment_retries();
                    log_and_backoff(attempts - 1, "file transport failure").await;
                    continue;
                }
                Err(source) => {
                    self.metrics.increment_errors();
                    return Err(self.transport_error(source, &method, path, &timing));
                }
            };
            let status = response.status();
            // Enforce the allowlisted-header evidence budget before any retry
            // decision or body read. Every physical response is independently
            // bounded, including intermediate 429 and retryable-status frames.
            crate::files::retained_file_header_bytes(
                response.headers(),
                status,
                header_evidence_limit,
            )?;

            if replay_safe && attempts < max_attempts && status == StatusCode::TOO_MANY_REQUESTS {
                let ParsedRetry { header, duration } = parse_retry_after(response.headers())?;
                if duration > Duration::from_secs(RATE_LIMIT_WAIT_MAX_SECS) {
                    self.metrics.increment_errors();
                    return Err(AnytypeError::RateLimitExceeded { header, duration });
                }
                // Bound and discard every retry response before replaying. This
                // keeps per-attempt evidence within the caller's error policy.
                if method != Method::HEAD {
                    self.read_bounded(response, error_body_limit, method.as_str(), path)
                        .await?;
                }
                self.metrics.increment_rate_limit_errors();
                self.metrics.increment_retries();
                self.metrics.add_rate_limit_delay(duration.as_secs());
                tokio::time::sleep(duration).await;
                continue;
            }

            if replay_safe && attempts < max_attempts && retry_for_status(status) {
                if method != Method::HEAD {
                    self.read_bounded(response, error_body_limit, method.as_str(), path)
                        .await?;
                }
                self.metrics.increment_errors();
                self.metrics.increment_retries();
                log_and_backoff(attempts - 1, "retryable file HTTP status").await;
                continue;
            }

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
                    success_body_limit
                } else {
                    error_body_limit
                };
                match self
                    .read_bounded(response, body_limit, method.as_str(), path)
                    .await
                {
                    Ok(body) => body,
                    Err(error) => {
                        self.metrics.increment_errors();
                        // A failed error-body read must not mask an already
                        // known ambiguous mutation status; the body is
                        // best-effort diagnostics.
                        if !replay_safe
                            && !(status.is_success() || allowed_control_status)
                            && mutation_status_is_indeterminate(status)
                        {
                            return Err(Self::mutation_indeterminate(
                                &method, path, attempts, status,
                            ));
                        }
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
                if !replay_safe && mutation_status_is_indeterminate(status) {
                    return Err(Self::mutation_indeterminate(
                        &method, path, attempts, status,
                    ));
                }
                return Err(AnytypeError::ApiError {
                    code: status.as_u16(),
                    method: method.as_str().to_ascii_lowercase(),
                    url: path.to_string(),
                    message: String::from_utf8_lossy(&body).into_owned(),
                });
            }

            return Ok(RawHttpResponse {
                status,
                headers: response_headers,
                body,
            });
        }
    }

    /// Makes one non-replayed multipart POST under caller-specific byte limits.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn post_multipart_with_limits<T: DeserializeOwned>(
        &self,
        path: &str,
        form: reqwest::multipart::Form,
        serialized_body_bytes: Option<u64>,
        request_body_limit: Option<u64>,
        response_body_limit: Option<u64>,
        error_body_limit: Option<u64>,
    ) -> Result<T> {
        let timing = OperationTiming::start();
        self.with_deadline(
            HttpTimeoutClass::LongOperation,
            TimeoutOutcome::MutationIndeterminate,
            &Method::POST,
            path,
            &timing,
            self.post_multipart_with_limits_inner(
                path,
                form,
                serialized_body_bytes,
                request_body_limit,
                response_body_limit,
                error_body_limit,
                timing.clone(),
            ),
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn post_multipart_with_limits_inner<T: DeserializeOwned>(
        &self,
        path: &str,
        form: reqwest::multipart::Form,
        serialized_body_bytes: Option<u64>,
        request_body_limit: Option<u64>,
        response_body_limit: Option<u64>,
        error_body_limit: Option<u64>,
        timing: OperationTiming,
    ) -> Result<T> {
        if let Some(limit) = request_body_limit {
            if limit == 0 {
                return Err(AnytypeError::Validation {
                    message: "multipart request limit must be nonzero".to_owned(),
                });
            }
            let actual = serialized_body_bytes.ok_or_else(|| AnytypeError::Validation {
                message: "multipart request length is unavailable".to_owned(),
            })?;
            if actual > limit {
                return Err(AnytypeError::Validation {
                    message: format!(
                        "multipart request body exceeds the {limit}-byte request limit"
                    ),
                });
            }
        }
        let response_body_limit = response_body_limit.unwrap_or(self.response_limits.json_bytes);
        if response_body_limit == 0 || response_body_limit > self.response_limits.json_bytes {
            return Err(AnytypeError::Validation {
                message: format!(
                    "multipart response limit must be between 1 and {} bytes",
                    self.response_limits.json_bytes
                ),
            });
        }
        let error_body_limit = error_body_limit.unwrap_or(self.response_limits.error_bytes);
        if error_body_limit == 0 || error_body_limit > self.response_limits.error_bytes {
            return Err(AnytypeError::Validation {
                message: format!(
                    "multipart error response limit must be between 1 and {} bytes",
                    self.response_limits.error_bytes
                ),
            });
        }
        let api_key = self.get_api_key();
        let Some(token) = api_key.token() else {
            return Err(AnytypeError::Auth {
                message: "HTTP credentials missing token. Client is not authenticated.".to_owned(),
            });
        };
        let full_url = format!("{}{}", self.base_url, path);
        debug!(path = %diagnostic_path(path), "post_multipart");
        self.metrics.increment_logical_operations();
        self.ensure_before_dispatch(
            HttpTimeoutClass::LongOperation,
            TimeoutOutcome::MutationIndeterminate,
            &Method::POST,
            path,
            &timing,
        )?;
        timing.record_attempt();
        self.metrics.increment_requests();
        self.metrics.increment_multipart_posts();
        if let Some(actual) = serialized_body_bytes {
            self.metrics.add_bytes_sent(actual);
        }
        let response = self
            .client
            .post(&full_url)
            .header(ANYTYPE_API_HEADER, ANYTYPE_API_VERSION)
            .bearer_auth(token)
            .multipart(form)
            .send()
            .await
            .map_err(reqwest::Error::without_url)
            .map_err(|source| self.transport_error(source, &Method::POST, path, &timing))?;
        if !response.status().is_success() {
            self.metrics.increment_errors();
            let status = response.status();
            let code = status.as_u16();
            // The ambiguous status alone fixes the classification, so the
            // error-body read is best-effort diagnostics and must not mask
            // the indeterminate outcome.
            let body = self
                .read_bounded(response, error_body_limit, "post", path)
                .await;
            if mutation_status_is_indeterminate(status) {
                return Err(Self::mutation_indeterminate(
                    &Method::POST,
                    path,
                    timing.attempts(),
                    status,
                ));
            }
            let message = String::from_utf8_lossy(&body?).into_owned();
            return Err(AnytypeError::ApiError {
                code,
                method: "post".to_string(),
                url: path.to_string(),
                message,
            });
        }
        let data = match self
            .read_bounded(response, response_body_limit, "post", path)
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

    async fn send_preserving_status<T: DeserializeOwned>(
        &self,
        req: HttpRequest,
    ) -> Result<PreservedStatusResponse<T>> {
        let timing = OperationTiming::start();
        let method = req.method.clone();
        let path = req.path.clone();
        self.with_deadline(
            HttpTimeoutClass::StandardOperation,
            timeout_outcome(&method),
            &method,
            &path,
            &timing,
            self.send_preserving_status_inner(req, timing.clone()),
        )
        .await
    }

    async fn send_preserving_status_inner<T: DeserializeOwned>(
        &self,
        req: HttpRequest,
        timing: OperationTiming,
    ) -> Result<PreservedStatusResponse<T>> {
        self.limits.validate_query(&req.query)?;
        if let Some(ref body) = req.body {
            self.limits.validate_body(
                body,
                &format!("http {} {}", req.method, diagnostic_path(&req.path)),
            )?;
        }
        let api_key = self.get_api_key();
        let Some(token) = api_key.token() else {
            return Err(AnytypeError::Auth {
                message: "HTTP credentials missing token. Client is not authenticated.".to_owned(),
            });
        };
        self.metrics.increment_logical_operations();
        let full_url = format!("{}{}", self.base_url, req.path);
        let body = req.body.clone().unwrap_or_default();
        let body_size = body.len() as u64;
        log_request(&req);
        self.ensure_before_dispatch(
            HttpTimeoutClass::StandardOperation,
            timeout_outcome(&req.method),
            &req.method,
            &req.path,
            &timing,
        )?;
        let physical_attempt = timing.record_attempt();
        self.metrics.increment_requests();
        self.metrics.add_bytes_sent(body_size);
        debug!(
            target: "anytype::http",
            http_method = %req.method,
            http_path = %diagnostic_path(&req.path),
            physical_attempt,
            "HTTP physical attempt"
        );
        let response = self
            .client
            .request(req.method.clone(), full_url)
            .query(&req.query)
            .header(ANYTYPE_API_HEADER, ANYTYPE_API_VERSION)
            .bearer_auth(token)
            .body(body)
            .send()
            .await
            .map_err(reqwest::Error::without_url)
            .map_err(|source| {
                self.metrics.increment_errors();
                log_http_transport(&req, physical_attempt);
                self.transport_error(source, &req.method, &req.path, &timing)
            })?;
        let status = response.status();
        if !status.is_success() {
            if status == StatusCode::TOO_MANY_REQUESTS {
                self.metrics.increment_rate_limit_errors();
            } else {
                self.metrics.increment_errors();
            }
            log_http_status(&req, status, "preserved_status", physical_attempt);
            let indeterminate = mutation_status_is_indeterminate(status);
            let status = status.as_u16();
            return Ok(if indeterminate {
                PreservedStatusResponse::Indeterminate { status }
            } else {
                PreservedStatusResponse::Rejected { status }
            });
        }
        let body = match self
            .read_bounded(
                response,
                self.response_limits.json_bytes,
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
        log_response(&req.path, &body);
        let value = match deserialize_json(&body) {
            Ok(value) => value,
            Err(error) => {
                self.metrics.increment_errors();
                return Err(error);
            }
        };
        self.metrics.increment_success();
        Ok(PreservedStatusResponse::Success(value))
    }

    #[allow(clippy::too_many_lines)]
    pub(crate) async fn send_with_limit<T: DeserializeOwned>(
        &self,
        req: HttpRequest,
        response_limit: u64,
    ) -> Result<T> {
        self.send_with_limit_and_retries(req, response_limit, true)
            .await
    }

    #[allow(clippy::too_many_lines)]
    async fn send_with_limit_and_retries<T: DeserializeOwned>(
        &self,
        req: HttpRequest,
        response_limit: u64,
        allow_retries: bool,
    ) -> Result<T> {
        let timing = OperationTiming::start();
        let method = req.method.clone();
        let path = req.path.clone();
        let outcome = timeout_outcome(&method);
        self.with_deadline(
            HttpTimeoutClass::StandardOperation,
            outcome,
            &method,
            &path,
            &timing,
            self.send_with_limit_and_retries_inner(
                req,
                response_limit,
                allow_retries,
                timing.clone(),
            ),
        )
        .await
    }

    #[allow(clippy::too_many_lines)]
    async fn send_with_limit_and_retries_inner<T: DeserializeOwned>(
        &self,
        req: HttpRequest,
        response_limit: u64,
        allow_retries: bool,
        timing: OperationTiming,
    ) -> Result<T> {
        // A retry clones and replays the complete request. Restrict every
        // retry path, including rate-limit handling, to methods whose HTTP
        // semantics make that replay safe. In particular, a POST or PATCH
        // response proves only that a response arrived; it does not make the
        // mutation safe to send again.
        let retryable_method = allow_retries && is_idempotent_method(&req.method);
        // Preserve the existing stricter status/transport retry budget while
        // one request-lifetime counter prevents mixed retry classes from
        // exceeding the common physical-attempt ceiling.
        let mut retry_attempt = 0u32;
        let mut rate_limit_retries = 0u32;

        // time to wait on next iteration
        let mut retry_wait: Option<Duration> = None;

        // check for excessive request size or invalid query
        self.limits.validate_query(&req.query)?;
        if let Some(ref body) = req.body {
            self.limits.validate_body(
                body,
                &format!("http {} {}", req.method, diagnostic_path(&req.path)),
            )?;
        }
        let api_key = self.get_api_key();
        let Some(token) = api_key.token() else {
            return Err(AnytypeError::Auth {
                message: "HTTP credentials missing token. Client is not authenticated.".to_owned(),
            });
        };
        self.metrics.increment_logical_operations();
        let full_url = format!("{}{}", self.base_url, req.path);
        let req_builder = self
            .client
            .request(req.method.clone(), &full_url)
            .query(&req.query)
            .header(ANYTYPE_API_HEADER, ANYTYPE_API_VERSION)
            .bearer_auth(token);

        // debug log (if tracing enabled)
        log_request(&req);

        // Track bytes to be sent (body size)
        let body_size = req.body.as_ref().map_or(0, |bytes| bytes.len() as u64);

        loop {
            if let Some(wait_time) = retry_wait {
                info!("RateLimit: pausing for {} sec", wait_time.as_secs());
                tokio::time::sleep(wait_time).await;
                retry_wait = None;
            }
            self.ensure_before_dispatch(
                HttpTimeoutClass::StandardOperation,
                timeout_outcome(&req.method),
                &req.method,
                &req.path,
                &timing,
            )?;
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
            let physical_attempt = timing.record_attempt();
            self.metrics.increment_requests();
            self.metrics.add_bytes_sent(body_size);
            debug!(
                target: "anytype::http",
                http_method = %req.method,
                http_path = %diagnostic_path(&req.path),
                physical_attempt,
                "HTTP physical attempt"
            );

            match request.send().await.map_err(reqwest::Error::without_url) {
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
                            if !retryable_method {
                                // The 429 alone fixes the classification;
                                // the error body is best-effort diagnostics
                                // and must not mask the indeterminate
                                // outcome.
                                let _best_effort_body = self.read_error_body(
                                    response,
                                    req.method.as_str(),
                                    &req.path,
                                )
                                .await;
                                return Err(Self::mutation_indeterminate(
                                    &req.method,
                                    &req.path,
                                    physical_attempt,
                                    code,
                                ));
                            }
                            rate_limit_retries = rate_limit_retries.saturating_add(1);
                            let headers = response.headers();
                            match parse_retry_after(headers) {
                                Err(err) => {
                                    error!(
                                        target: "anytype::http",
                                        error_variant = "invalid_rate_limit_header",
                                        http_status = code.as_u16(),
                                        http_method = %req.method,
                                        http_path = %diagnostic_path(&req.path),
                                        "HTTP request failed"
                                    );
                                    // couldn't parse header.
                                    return Err(err)
                                }
                                Ok(ParsedRetry{ header, duration}) => {
                                    self.read_error_body(
                                        response,
                                        req.method.as_str(),
                                        &req.path,
                                    )
                                    .await?;
                                    if self.rate_limit_max_retries > 0
                                        && rate_limit_retries > self.rate_limit_max_retries
                                    {
                                    error!(
                                            target: "anytype::http",
                                            error_variant = "rate_limit_retry_limit",
                                            http_status = code.as_u16(),
                                            http_method = %req.method,
                                            http_path = %diagnostic_path(&req.path),
                                            physical_attempt,
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
                                            target: "anytype::http",
                                            error_variant = "rate_limit_backoff_limit",
                                            http_status = code.as_u16(),
                                            http_method = %req.method,
                                            http_path = %diagnostic_path(&req.path),
                                            physical_attempt,
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
                                            physical_attempt,
                                            "http 429 Rate-limit backoff={}s",
                                            duration.as_secs()
                                        );
                                    }
                                    if physical_attempt >= MAX_HTTP_REQUEST_ATTEMPTS {
                                        error!(
                                            target: "anytype::http",
                                            error_variant = "physical_attempt_limit",
                                            http_status = code.as_u16(),
                                            http_method = %req.method,
                                            http_path = %diagnostic_path(&req.path),
                                            physical_attempt,
                                            "HTTP physical-attempt ceiling reached"
                                        );
                                        return Err(AnytypeError::RateLimitExceeded {
                                            header,
                                            duration,
                                        });
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
                            log_http_status(&req, code, "validation", physical_attempt);
                            return Err(AnytypeError::ApiError {
                                code: code.as_u16(),
                                method: req.method.to_string(),
                                url: req.path,
                                message,
                            })
                        }
                        StatusCode::NOT_FOUND /* 404 */ |
                        StatusCode::GONE /* 410 */
                         => {
                            self.metrics.increment_errors();
                            self.read_error_body(response, req.method.as_str(), &req.path).await?;
                            log_http_status(&req, code, "not_found", physical_attempt);
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
                            log_http_status(&req, code, "unauthorized", physical_attempt);
                            return Err(AnytypeError::Unauthorized)
                        }
                        StatusCode::FORBIDDEN /* 403 */ => {
                            // client is authenticated, but does not have permission to access the object
                            self.metrics.increment_errors();
                            self.read_error_body(response, req.method.as_str(), &req.path).await?;
                            log_http_status(&req, code, "forbidden", physical_attempt);
                            return Err(AnytypeError::Forbidden)
                        }
                        _ => {
                            self.metrics.increment_errors();
                            // The status alone fixes an indeterminate
                            // mutation classification, so the error-body
                            // read is best-effort diagnostics for that path
                            // and must not mask the outcome.
                            let body = self.read_error_body(response, req.method.as_str(), &req.path).await;
                            log_http_status(&req, code, "api_error", physical_attempt);
                            if !retryable_method
                                && (code.is_server_error() || retry_for_status(code))
                            {
                                return Err(Self::mutation_indeterminate(
                                    &req.method,
                                    &req.path,
                                    physical_attempt,
                                    code,
                                ));
                            }
                            let message = body?;
                            if retry_attempt < MAX_RETRIES
                                && physical_attempt < MAX_HTTP_REQUEST_ATTEMPTS
                                && retry_for_status(code)
                                && retryable_method
                            {
                                self.metrics.increment_retries();
                                log_and_backoff(retry_attempt, "retryable HTTP status").await;
                                retry_attempt += 1;
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
                    log_http_transport(&req, physical_attempt);
                    // Check for connection or timeout errors
                    if err.is_connect() && retryable_method {
                        rate_limit_retries = 0;
                        if retry_attempt < MAX_RETRIES
                            && physical_attempt < MAX_HTTP_REQUEST_ATTEMPTS
                        {
                            log_and_backoff(retry_attempt, "transport failure").await;
                            self.metrics.increment_retries();
                            retry_attempt += 1;
                            continue;
                        }
                        self.metrics.increment_errors();
                        return Err(self.transport_error(err, &req.method, &req.path, &timing));
                    }
                    // Other non-recoverable errors (e.g., DNS error, invalid URL, etc.)
                    self.metrics.increment_errors();
                    return Err(self.transport_error(err, &req.method, &req.path, &timing));
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

    /// Makes a paginated GET whose every page uses the same response ceiling.
    async fn get_request_paged_with_limit<T: DeserializeOwned + Send + 'static>(
        &self,
        path: &str,
        query: QueryWithFilters,
        response_limit: u64,
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
            message: format!("get_request_paged {} {err}", diagnostic_path(path)),
        })?;
        let req = HttpRequest {
            method: Method::GET,
            path: path.into(),
            query: query.params,
            body: None,
        };
        let response: PaginatedResponse<T> = self.send(req.clone()).await?;
        Ok(super::paged::PagedResult::new(
            response,
            self.clone(),
            req,
            None,
        ))
    }

    async fn get_request_paged_with_limit<T: DeserializeOwned + Send + 'static>(
        &self,
        path: &str,
        query: QueryWithFilters,
        response_limit: u64,
    ) -> Result<super::paged::PagedResult<T>> {
        query.validate().map_err(|err| AnytypeError::Validation {
            message: format!(
                "get_request_paged_with_limit {} {err}",
                diagnostic_path(path)
            ),
        })?;
        if response_limit == 0 || response_limit > self.response_limits.document_bytes {
            return Err(AnytypeError::Validation {
                message: format!(
                    "paged response limit must be between 1 and {} bytes",
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
        let response: PaginatedResponse<T> =
            self.send_with_limit(req.clone(), response_limit).await?;
        Ok(super::paged::PagedResult::new(
            response,
            self.clone(),
            req,
            Some(response_limit),
        ))
    }

    /// Makes an authenticated POST request that returns a `PagedResult` for pagination support.
    async fn post_request_paged<T: DeserializeOwned + Send + 'static, B: Serialize + Sync>(
        &self,
        path: &str,
        body: &B,
        query: QueryWithFilters,
    ) -> Result<super::paged::PagedResult<T>> {
        query.validate().map_err(|err| AnytypeError::Validation {
            message: format!("post_request_paged {} {err}", diagnostic_path(path)),
        })?;
        let req = HttpRequest {
            method: Method::POST,
            path: path.into(),
            query: query.params,
            body: Some(Bytes::from(
                serde_json::to_vec(body)
                    .map_err(|source| AnytypeError::Serialization { source })?,
            )),
        };
        let response: PaginatedResponse<T> = self.send(req.clone()).await?;
        Ok(super::paged::PagedResult::new(
            response,
            self.clone(),
            req,
            None,
        ))
    }
}

// Secret-safe request metadata. Payload tracing is intentionally unavailable:
// RUST_LOG may select this target but cannot expose bodies, query values,
// headers, full URLs, or credentials.
fn log_request(request: &HttpRequest) {
    if tracing::enabled!(target: "anytype::http_json", tracing::Level::TRACE) {
        trace!(
            target: "anytype::http_json",
            method = %request.method,
            path = %diagnostic_path(&request.path),
            query_fields = request.query.len(),
            body_bytes = request.body.as_ref().map_or(0, Bytes::len),
            "HTTP request metadata"
        );
    }
}

// Secret-safe response metadata. Anytype JSON may contain private document
// data, so no tracing level or target can include `body` itself.
fn log_response(path: &str, body: &Bytes) {
    if tracing::enabled!(target: "anytype::http_json", tracing::Level::TRACE) {
        trace!(
            target: "anytype::http_json",
            path = %diagnostic_path(path),
            body_bytes = body.len(),
            "HTTP response metadata"
        );
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
            let source = err.inner();
            error!(
                target: "anytype::http",
                error_variant = "deserialization",
                json_category = ?source.classify(),
                line = source.line(),
                column = source.column(),
                "HTTP response deserialization failed"
            );
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
    matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS)
}

#[cfg(test)]
mod tests {
    use std::{
        io::{self, Write},
        sync::{Arc, Barrier, Mutex, Once, atomic::Ordering},
        time::Duration,
    };

    use bytes::Bytes;
    use futures::StreamExt;
    use reqwest::{
        ClientBuilder, Method, StatusCode,
        header::{HeaderMap, HeaderValue},
    };
    use serde::Deserialize;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::{TcpListener, TcpStream},
        sync::oneshot,
        task::JoinHandle,
    };
    use tracing::Dispatch;
    use tracing_subscriber::{fmt as tracing_fmt, layer::SubscriberExt};

    use super::{
        HttpClient, HttpMetrics, HttpRequest, MAX_DIAGNOSTIC_PATH_CHARS, REDACTED_DIAGNOSTIC_PATH,
        deserialize_json, diagnostic_path, log_http_status, log_request, log_response,
        parse_retry_after,
    };
    use crate::filters::QueryWithFilters;
    use crate::prelude::{
        AnytypeClient, AnytypeError, ClientConfig, HttpCredentials, HttpTimeoutPolicy,
        MAX_JSON_RESPONSE_BYTES, ResponseLimits, ValidationLimits,
    };

    const TEST_SPACE_ID: &str =
        "bafyreid5fvqlnsobih2keakcxjrrlpmly6kf37klzjzen4ibfdgalcdp4y.2tq5w93cr6oe7";
    const TEST_OBJECT_ID: &str = "bafyreie6n5l5nkbjal37su54cha4coy7qzuhrnajluzv5qd5jvtsrxkequ";

    static TRACE_TEST_INTEREST: Once = Once::new();

    fn ensure_trace_interest() {
        TRACE_TEST_INTEREST.call_once(|| {
            let subscriber =
                tracing_subscriber::registry().with(tracing_subscriber::filter::LevelFilter::TRACE);
            let _ = tracing::subscriber::set_global_default(subscriber);
        });
    }

    #[derive(Clone, Default)]
    struct Capture(Arc<Mutex<Vec<u8>>>);

    impl Capture {
        fn contents(&self) -> String {
            String::from_utf8(self.0.lock().expect("capture lock").clone())
                .expect("diagnostics are UTF-8")
        }
    }

    impl Write for Capture {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0
                .lock()
                .expect("capture lock")
                .extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl<'writer> tracing_subscriber::fmt::writer::MakeWriter<'writer> for Capture {
        type Writer = Self;

        fn make_writer(&'writer self) -> Self::Writer {
            self.clone()
        }
    }

    fn capture() -> (Dispatch, Capture) {
        ensure_trace_interest();
        let output = Capture::default();
        let layer = tracing_fmt::layer()
            .with_writer(output.clone())
            .with_target(true)
            .with_ansi(false);
        let subscriber = tracing_subscriber::registry()
            .with(tracing_subscriber::filter::LevelFilter::TRACE)
            .with(layer);
        (Dispatch::new(subscriber), output)
    }

    fn test_limits(json_bytes: u64, document_bytes: u64, error_bytes: u64) -> ResponseLimits {
        ResponseLimits {
            json_bytes,
            document_bytes,
            error_bytes,
            file_bytes: 1024,
            chat_sse_event_bytes: 1024,
        }
    }

    fn credential_test_client() -> Arc<HttpClient> {
        Arc::new(
            HttpClient::new(
                ClientBuilder::new().no_proxy(),
                "http://127.0.0.1:1".to_owned(),
                ValidationLimits::default(),
                test_limits(4, 8, 4),
                1,
                HttpTimeoutPolicy::default(),
                HttpCredentials::new("old-token"),
            )
            .expect("credential test client"),
        )
    }

    #[test]
    fn credential_replacement_and_generation_are_one_atomic_state_transition() {
        let client = credential_test_client();
        let set_barrier = Arc::new(Barrier::new(3));
        let writer = {
            let client = client.clone();
            let barrier = set_barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                client.set_api_key(HttpCredentials::new("new-token"));
            })
        };
        let reader = {
            let client = client.clone();
            let barrier = set_barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                let state = client.credential_state.lock();
                (
                    state.credentials.token().map(str::to_owned),
                    state.generation,
                )
            })
        };
        set_barrier.wait();
        writer.join().expect("set writer");
        let set_observation = reader.join().expect("set reader");
        assert!(
            matches!(
                set_observation,
                (Some(ref token), 0) if token == "old-token"
            ) || matches!(
                set_observation,
                (Some(ref token), 1) if token == "new-token"
            )
        );

        let clear_barrier = Arc::new(Barrier::new(3));
        let writer = {
            let client = client.clone();
            let barrier = clear_barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                client.clear_api_key();
            })
        };
        let reader = {
            let client = client.clone();
            let barrier = clear_barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                let state = client.credential_state.lock();
                (
                    state.credentials.token().map(str::to_owned),
                    state.generation,
                )
            })
        };
        clear_barrier.wait();
        writer.join().expect("clear writer");
        let clear_observation = reader.join().expect("clear reader");
        assert!(
            matches!(
                clear_observation,
                (Some(ref token), 1) if token == "new-token"
            ) || matches!(clear_observation, (None, 2))
        );
        assert_eq!(client.credential_generation(), 2);
        assert!(!client.has_key());
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
            HttpTimeoutPolicy::default(),
            HttpCredentials::new("test-token"),
        )
        .expect("test client");
        (Arc::new(client), server)
    }

    async fn sse_chunk_fixture(
        policy: HttpTimeoutPolicy,
        schedule: Vec<(Duration, &'static [u8])>,
        finish: bool,
    ) -> (Arc<HttpClient>, JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind SSE server");
        let address = listener.local_addr().expect("SSE server address");
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("accept SSE request");
            let _request = read_fixture_request(&mut socket).await;
            socket
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n",
                )
                .await
                .expect("write SSE headers");
            socket.flush().await.expect("flush SSE headers");
            for (delay, chunk) in schedule {
                tokio::time::sleep(delay).await;
                let prefix = format!("{:x}\r\n", chunk.len());
                socket
                    .write_all(prefix.as_bytes())
                    .await
                    .expect("write SSE chunk prefix");
                socket.write_all(chunk).await.expect("write SSE chunk");
                socket
                    .write_all(b"\r\n")
                    .await
                    .expect("write SSE chunk suffix");
                socket.flush().await.expect("flush SSE chunk");
            }
            if finish {
                socket
                    .write_all(b"0\r\n\r\n")
                    .await
                    .expect("finish SSE body");
            } else {
                let mut peer_data = [0_u8; 1];
                let read = socket
                    .read(&mut peer_data)
                    .await
                    .expect("observe SSE peer close");
                assert_eq!(read, 0, "SSE peer sent unexpected data");
            }
        });
        let client = HttpClient::new(
            ClientBuilder::new().no_proxy(),
            format!("http://{address}"),
            ValidationLimits::default(),
            test_limits(4096, 4096, 4096),
            1,
            policy,
            HttpCredentials::new("test-token"),
        )
        .expect("SSE client");
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

    fn fixture_response(status: &str, body: &str, extra_headers: &str) -> Vec<u8> {
        format!(
            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n{extra_headers}Connection: close\r\n\r\n{body}",
            body.len()
        )
        .into_bytes()
    }

    async fn deadline_fixture(
        policy: HttpTimeoutPolicy,
        prefix: Option<&'static [u8]>,
        builder: ClientBuilder,
    ) -> (Arc<HttpClient>, oneshot::Receiver<()>, JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind deadline fixture");
        let address = listener.local_addr().expect("deadline fixture address");
        let (ready_tx, ready_rx) = oneshot::channel();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("accept deadline request");
            let _request = read_fixture_request(&mut socket).await;
            if let Some(prefix) = prefix {
                socket
                    .write_all(prefix)
                    .await
                    .expect("write deadline prefix");
                socket.flush().await.expect("flush deadline prefix");
            }
            let _ = ready_tx.send(());
            std::future::pending::<()>().await;
        });
        let client = HttpClient::new(
            builder,
            format!("http://{address}"),
            ValidationLimits::default(),
            test_limits(1024, 2048, 1024),
            5,
            policy,
            HttpCredentials::new("test-token"),
        )
        .expect("deadline client");
        (Arc::new(client), ready_rx, server)
    }

    fn one_second_standard_policy() -> HttpTimeoutPolicy {
        HttpTimeoutPolicy {
            standard_operation: Some(Duration::from_secs(1)),
            ..HttpTimeoutPolicy::default()
        }
    }

    // The deadline tests run in real time until the fixture has accepted the
    // request, then freeze the clock to advance past the deadline exactly.
    // Under `start_paused`, auto-advance can virtually expire the deadline
    // while the real TCP connect is still in flight, after which the fixture
    // never accepts and the test parks forever.
    #[tokio::test]
    async fn standard_deadline_bounds_stalled_headers_and_records_one_timeout() {
        let (client, accepted, server) = deadline_fixture(
            one_second_standard_policy(),
            None,
            ClientBuilder::new().no_proxy(),
        )
        .await;
        let request_client = client.clone();
        let request = tokio::spawn(async move { request_client.send::<()>(get_request()).await });
        accepted.await.expect("request accepted");
        tokio::time::pause();
        tokio::time::advance(Duration::from_secs(1)).await;
        let error = request.await.expect("request task").expect_err("deadline");
        let diagnostic = error.diagnostic().to_string();
        assert!(diagnostic.contains("method=GET"));
        assert!(diagnostic.contains("path=/test"));
        assert!(diagnostic.contains("timeout_class=standard_operation"));
        assert!(diagnostic.contains("outcome=read_aborted"));
        assert!(diagnostic.contains("attempts=1"));
        assert!(matches!(
            error,
            AnytypeError::HttpTimeout {
                class: crate::http_timeout::HttpTimeoutClass::StandardOperation,
                outcome: crate::http_timeout::TimeoutOutcome::ReadAborted,
                attempts: 1,
                ..
            }
        ));
        let metrics = client.metrics_snapshot();
        assert_eq!(
            metrics
                .timeout(crate::http_timeout::HttpTimeoutClass::StandardOperation)
                .count,
            1
        );
        assert_eq!(
            metrics.timeout_outcome_count(crate::http_timeout::TimeoutOutcome::ReadAborted),
            1
        );
        server.abort();
    }

    #[tokio::test]
    async fn standard_deadline_includes_success_and_error_bodies() {
        for prefix in [
            b"HTTP/1.1 200 OK\r\nContent-Length: 8\r\n\r\n{".as_slice(),
            b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 8\r\n\r\n{".as_slice(),
        ] {
            let (client, prefix_written, server) = deadline_fixture(
                one_second_standard_policy(),
                Some(prefix),
                ClientBuilder::new().no_proxy(),
            )
            .await;
            let request = tokio::spawn(async move { client.send::<()>(get_request()).await });
            prefix_written.await.expect("response prefix written");
            tokio::time::pause();
            tokio::time::advance(Duration::from_secs(1)).await;
            assert!(matches!(
                request.await.expect("request task"),
                Err(AnytypeError::HttpTimeout {
                    class: crate::http_timeout::HttpTimeoutClass::StandardOperation,
                    ..
                })
            ));
            server.abort();
            tokio::time::resume();
        }
    }

    #[tokio::test]
    async fn mutation_deadline_is_indeterminate_and_dispatches_once() {
        let (client, accepted, server) = deadline_fixture(
            one_second_standard_policy(),
            None,
            ClientBuilder::new().no_proxy(),
        )
        .await;
        let request_client = client.clone();
        let request = tokio::spawn(async move {
            request_client
                .send::<()>(HttpRequest {
                    method: Method::POST,
                    path: "/mutation?secret=redacted".to_owned(),
                    query: Vec::new(),
                    body: Some(Bytes::from_static(b"secret body")),
                })
                .await
        });
        accepted.await.expect("mutation accepted");
        tokio::time::pause();
        tokio::time::advance(Duration::from_secs(1)).await;
        let error = request.await.expect("request task").expect_err("deadline");
        let diagnostic = error.diagnostic().to_string();
        assert!(diagnostic.contains("method=POST"));
        assert!(diagnostic.contains("path=/mutation"));
        assert!(diagnostic.contains("timeout_class=standard_operation"));
        assert!(diagnostic.contains("outcome=mutation_indeterminate"));
        assert!(diagnostic.contains("attempts=1"));
        assert!(!diagnostic.contains("secret"));
        assert!(!diagnostic.contains("redacted"));
        assert!(matches!(
            error,
            AnytypeError::HttpTimeout {
                outcome: crate::http_timeout::TimeoutOutcome::MutationIndeterminate,
                attempts: 1,
                ..
            }
        ));
        assert_eq!(client.metrics_snapshot().physical_attempts, 1);
        server.abort();
    }

    #[tokio::test]
    async fn shorter_caller_transport_timeout_wins_and_is_measured_once() {
        let policy = HttpTimeoutPolicy {
            standard_operation: Some(Duration::from_secs(10)),
            ..HttpTimeoutPolicy::default()
        };
        let (client, accepted, server) = deadline_fixture(
            policy,
            None,
            ClientBuilder::new()
                .no_proxy()
                .timeout(Duration::from_secs(1)),
        )
        .await;
        let request_client = client.clone();
        let request = tokio::spawn(async move { request_client.send::<()>(get_request()).await });
        accepted.await.expect("request accepted");
        tokio::time::pause();
        tokio::time::advance(Duration::from_secs(1)).await;
        let error = request
            .await
            .expect("request task")
            .expect_err("transport timeout");
        let diagnostic = error.diagnostic().to_string();
        assert!(diagnostic.contains("timeout_class=transport"));
        assert!(diagnostic.contains("outcome=read_aborted"));
        assert!(diagnostic.contains("elapsed_ms="));
        assert!(diagnostic.contains("attempts=1"));
        assert!(matches!(error, AnytypeError::Http { source, .. } if source.is_timeout()));
        let metrics = client.metrics_snapshot();
        assert_eq!(metrics.transport_timeouts().count, 1);
        assert_eq!(
            metrics.timeout_outcome_count(crate::http_timeout::TimeoutOutcome::ReadAborted),
            1
        );
        server.abort();
    }

    #[tokio::test]
    async fn mutation_caller_transport_timeout_reports_indeterminate_outcome() {
        let policy = HttpTimeoutPolicy {
            standard_operation: Some(Duration::from_secs(10)),
            ..HttpTimeoutPolicy::default()
        };
        let (client, accepted, server) = deadline_fixture(
            policy,
            None,
            ClientBuilder::new()
                .no_proxy()
                .timeout(Duration::from_secs(1)),
        )
        .await;
        let request_client = client.clone();
        let request = tokio::spawn(async move {
            request_client
                .send::<()>(HttpRequest {
                    method: Method::POST,
                    path: "/mutation?secret=redacted".to_owned(),
                    query: Vec::new(),
                    body: Some(Bytes::from_static(b"secret body")),
                })
                .await
        });
        accepted.await.expect("mutation accepted");
        tokio::time::pause();
        tokio::time::advance(Duration::from_secs(1)).await;
        let error = request
            .await
            .expect("request task")
            .expect_err("transport timeout");
        let diagnostic = error.diagnostic().to_string();
        assert!(diagnostic.contains("timeout_class=transport"));
        assert!(diagnostic.contains("outcome=mutation_indeterminate"));
        assert!(diagnostic.contains("elapsed_ms="));
        assert!(diagnostic.contains("attempts=1"));
        assert!(!diagnostic.contains("secret"));
        assert!(matches!(&error, AnytypeError::Http { source, .. } if source.is_timeout()));
        let metrics = client.metrics_snapshot();
        assert_eq!(metrics.transport_timeouts().count, 1);
        assert_eq!(
            metrics
                .timeout_outcome_count(crate::http_timeout::TimeoutOutcome::MutationIndeterminate),
            1
        );
        server.abort();
    }

    #[tokio::test]
    async fn connect_refused_mutation_keeps_typed_transport_error() {
        // Reserve a local port and release it so the connection is refused.
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind refused-port probe");
        let address = listener.local_addr().expect("refused-port address");
        drop(listener);
        let client = Arc::new(
            HttpClient::new(
                ClientBuilder::new().no_proxy(),
                format!("http://{address}"),
                ValidationLimits::default(),
                test_limits(1024, 2048, 1024),
                5,
                HttpTimeoutPolicy::default(),
                HttpCredentials::new("test-token"),
            )
            .expect("refused client"),
        );
        let error = client
            .send::<()>(HttpRequest {
                method: Method::POST,
                path: "/mutation".to_owned(),
                query: Vec::new(),
                body: Some(Bytes::from_static(b"body")),
            })
            .await
            .expect_err("connection refused");
        // The mutation was provably never dispatched, so the typed transport
        // error and its reqwest source survive instead of an indeterminate
        // outcome.
        assert!(
            matches!(&error, AnytypeError::Http { source, .. } if source.is_connect()),
            "unexpected connect-refused error: {error:?}"
        );
        let metrics = client.metrics_snapshot();
        assert_eq!(
            metrics
                .timeout_outcome_count(crate::http_timeout::TimeoutOutcome::MutationIndeterminate),
            0
        );
    }

    #[test]
    fn timeout_metrics_use_saturating_arithmetic() {
        let metrics = HttpMetrics::default();
        let class = crate::http_timeout::HttpTimeoutClass::StandardOperation;
        let outcome = crate::http_timeout::TimeoutOutcome::ReadAborted;
        metrics.timeout_counts[class.index()].store(u64::MAX, Ordering::Relaxed);
        metrics.timeout_elapsed_millis[class.index()].store(u64::MAX - 1, Ordering::Relaxed);
        metrics.timeout_outcomes[outcome.index()].store(u64::MAX, Ordering::Relaxed);
        metrics.record_timeout(class, outcome, Duration::from_millis(10));
        let snapshot = metrics.snapshot();
        assert_eq!(snapshot.timeout(class).count, u64::MAX);
        assert_eq!(snapshot.timeout(class).elapsed_millis, u64::MAX);
        assert_eq!(snapshot.timeout_outcome_count(outcome), u64::MAX);
    }

    #[tokio::test]
    async fn rate_limit_wait_cannot_reset_the_standard_deadline() {
        let response = fixture_response(
            "429 Too Many Requests",
            "rate limited",
            "RateLimit-Reset: 2\r\n",
        );
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind rate-limit deadline fixture");
        let address = listener.local_addr().expect("rate-limit fixture address");
        let (sent_tx, sent_rx) = oneshot::channel();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("accept rate-limit request");
            let _request = read_fixture_request(&mut socket).await;
            socket
                .write_all(&response)
                .await
                .expect("write rate-limit response");
            let _ = sent_tx.send(());
        });
        let client = Arc::new(
            HttpClient::new(
                ClientBuilder::new().no_proxy(),
                format!("http://{address}"),
                ValidationLimits::default(),
                test_limits(4096, 4096, 4096),
                5,
                one_second_standard_policy(),
                HttpCredentials::new("test-token"),
            )
            .expect("rate-limit deadline client"),
        );
        let request_client = client.clone();
        let request = tokio::spawn(async move { request_client.send::<()>(get_request()).await });
        sent_rx.await.expect("rate-limit response sent");
        for _ in 0..32 {
            if client.metrics_snapshot().rate_limit_errors == 1 {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert_eq!(client.metrics_snapshot().rate_limit_errors, 1);
        tokio::time::pause();
        tokio::time::advance(Duration::from_secs(1)).await;
        let error = request
            .await
            .expect("rate-limit request task")
            .expect_err("shared deadline must expire during rate-limit wait");
        assert!(matches!(
            error,
            AnytypeError::HttpTimeout {
                class: crate::http_timeout::HttpTimeoutClass::StandardOperation,
                attempts: 1,
                ..
            }
        ));
        assert_eq!(client.metrics_snapshot().physical_attempts, 1);
        tokio::time::resume();
        server.await.expect("rate-limit server");
    }

    #[tokio::test]
    async fn retry_backoff_cannot_reset_the_standard_deadline() {
        let response = fixture_response("504 Gateway Timeout", "retry later", "");
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind retry deadline fixture");
        let address = listener.local_addr().expect("retry fixture address");
        let (sent_tx, sent_rx) = oneshot::channel();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("accept retry request");
            let _request = read_fixture_request(&mut socket).await;
            socket
                .write_all(&response)
                .await
                .expect("write retry response");
            let _ = sent_tx.send(());
        });
        let client = Arc::new(
            HttpClient::new(
                ClientBuilder::new().no_proxy(),
                format!("http://{address}"),
                ValidationLimits::default(),
                test_limits(4096, 4096, 4096),
                5,
                one_second_standard_policy(),
                HttpCredentials::new("test-token"),
            )
            .expect("retry deadline client"),
        );
        let request_client = client.clone();
        let request = tokio::spawn(async move { request_client.send::<()>(get_request()).await });
        sent_rx.await.expect("retry response sent");
        for _ in 0..32 {
            if client.metrics_snapshot().retries == 1 {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert_eq!(client.metrics_snapshot().retries, 1);
        tokio::time::pause();
        tokio::time::advance(Duration::from_secs(1)).await;
        let error = request
            .await
            .expect("retry request task")
            .expect_err("shared deadline must expire during retry backoff");
        assert!(matches!(
            error,
            AnytypeError::HttpTimeout {
                class: crate::http_timeout::HttpTimeoutClass::StandardOperation,
                attempts: 1,
                ..
            }
        ));
        assert_eq!(client.metrics_snapshot().physical_attempts, 1);
        let metrics = client.metrics_snapshot();
        assert_eq!(
            metrics
                .timeout(crate::http_timeout::HttpTimeoutClass::StandardOperation)
                .count,
            1
        );
        assert_eq!(
            metrics.timeout_outcome_count(crate::http_timeout::TimeoutOutcome::ReadAborted),
            1
        );
        tokio::time::resume();
        server.await.expect("retry server");
    }

    #[tokio::test]
    async fn delayed_file_response_uses_long_profile() {
        let policy = HttpTimeoutPolicy {
            standard_operation: Some(Duration::from_secs(120)),
            long_operation: Some(Duration::from_secs(600)),
            ..HttpTimeoutPolicy::default()
        };
        let (client, accepted, server) =
            deadline_fixture(policy, None, ClientBuilder::new().no_proxy()).await;
        let file_client = client.clone();
        let file = tokio::spawn(async move {
            file_client
                .file_request(Method::GET, "/v1/files/id", &[], HeaderMap::new())
                .await
        });
        accepted.await.expect("file request accepted");
        tokio::time::pause();
        tokio::time::advance(Duration::from_secs(600)).await;
        assert!(matches!(
            file.await.expect("file request task"),
            Err(AnytypeError::HttpTimeout {
                class: crate::http_timeout::HttpTimeoutClass::LongOperation,
                ..
            })
        ));
        server.abort();

        let timing = super::OperationTiming::start();
        client
            .with_deadline(
                crate::http_timeout::HttpTimeoutClass::LongOperation,
                crate::http_timeout::TimeoutOutcome::ReadAborted,
                &Method::GET,
                "/v1/files/id",
                &timing,
                async {
                    tokio::time::sleep(Duration::from_secs(154)).await;
                    Ok::<_, AnytypeError>(())
                },
            )
            .await
            .expect("154-second operation fits the long profile");
    }

    #[tokio::test]
    async fn scripted_delayed_file_response_completes_within_long_profile() {
        // A scripted HTTP fixture delivers the complete file response only
        // after the standard deadline has passed, proving the real wire path
        // for file requests runs under the long profile. Paused time cannot
        // complete loopback responses deterministically (auto-advance fires
        // pending deadline timers before IO readiness is surfaced), so this
        // runs on the real clock with a two-second scripted delay; the
        // 154-second policy literal stays covered by the paused
        // `with_deadline` leg of `delayed_file_response_uses_long_profile`.
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind delayed file fixture");
        let address = listener.local_addr().expect("delayed file fixture address");
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener
                .accept()
                .await
                .expect("accept delayed file request");
            let _request = read_fixture_request(&mut socket).await;
            tokio::time::sleep(Duration::from_secs(2)).await;
            socket
                .write_all(&fixture_response("200 OK", "file body", ""))
                .await
                .expect("write delayed file response");
            std::future::pending::<()>().await;
        });
        let policy = HttpTimeoutPolicy {
            standard_operation: Some(Duration::from_secs(1)),
            long_operation: Some(Duration::from_secs(8)),
            ..HttpTimeoutPolicy::default()
        };
        let client = Arc::new(
            HttpClient::new(
                ClientBuilder::new().no_proxy(),
                format!("http://{address}"),
                ValidationLimits::default(),
                test_limits(1024, 2048, 1024),
                5,
                policy,
                HttpCredentials::new("test-token"),
            )
            .expect("delayed file client"),
        );
        let response = client
            .file_request(Method::GET, "/v1/files/id", &[], HeaderMap::new())
            .await
            .expect("delayed scripted response fits the long profile");
        assert_eq!(response.status, StatusCode::OK);
        let metrics = client.metrics_snapshot();
        assert_eq!(metrics.physical_attempts, 1);
        assert_eq!(
            metrics
                .timeout(crate::http_timeout::HttpTimeoutClass::LongOperation)
                .count,
            0
        );
        assert_eq!(
            metrics
                .timeout(crate::http_timeout::HttpTimeoutClass::StandardOperation)
                .count,
            0
        );
        server.abort();
    }

    #[tokio::test]
    async fn each_paginated_page_receives_a_fresh_standard_deadline() {
        use super::GetPaged;

        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind paged fixture");
        let address = listener.local_addr().expect("paged fixture address");
        let (page_two_tx, mut page_two_rx) = oneshot::channel();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("accept page one");
            let _request = read_fixture_request(&mut socket).await;
            let body =
                r#"{"data":[1],"pagination":{"has_more":true,"limit":1,"offset":1,"total":2}}"#;
            socket
                .write_all(&fixture_response("200 OK", body, ""))
                .await
                .expect("write page one");
            let (mut stalled, _) = listener.accept().await.expect("accept page two");
            let _request = read_fixture_request(&mut stalled).await;
            let _ = page_two_tx.send(());
            std::future::pending::<()>().await;
        });
        let client = Arc::new(
            HttpClient::new(
                ClientBuilder::new().no_proxy(),
                format!("http://{address}"),
                ValidationLimits::default(),
                test_limits(1024, 2048, 1024),
                5,
                one_second_standard_policy(),
                HttpCredentials::new("test-token"),
            )
            .expect("paged client"),
        );
        let paged_client = client.clone();
        let (first_item_tx, first_item_rx) = oneshot::channel();
        let (resume_tx, resume_rx) = oneshot::channel();
        let mut task = tokio::spawn(async move {
            let paged = paged_client
                .get_request_paged::<u32>("/v1/paged", QueryWithFilters::default())
                .await?;
            let mut stream = paged.into_stream();
            let first = stream.next().await.expect("first page item")?;
            assert_eq!(first, 1);
            let _ = first_item_tx.send(());
            resume_rx.await.expect("resume signal");
            // The next poll triggers the page-two refill request.
            stream
                .next()
                .await
                .expect("page-two outcome")
                .map(|_| unreachable!("page two never completes"))
        });
        if first_item_rx.await.is_err() {
            let finished = task.await;
            panic!("paged task failed before first item: {finished:?}");
        }
        // Wait out more than the whole one-second standard deadline between
        // pages. A budget shared across pages would already be expired when
        // page two dispatches, so its request would never reach the wire.
        tokio::time::sleep(Duration::from_millis(1_500)).await;
        resume_tx.send(()).expect("send resume");
        tokio::select! {
            dispatched = &mut page_two_rx => {
                dispatched.expect("page two dispatched");
            }
            finished = &mut task => {
                panic!("paged task finished before page-two dispatch: {finished:?}");
            }
        }
        let error = task
            .await
            .expect("paged task")
            .expect_err("page-two deadline");
        // A fresh page-two deadline expires after roughly its own second of
        // waiting, not the 1.5-second inter-page wait already elapsed.
        assert!(
            matches!(
                &error,
                AnytypeError::HttpTimeout {
                    class: crate::http_timeout::HttpTimeoutClass::StandardOperation,
                    elapsed,
                    ..
                } if *elapsed >= Duration::from_secs(1) && *elapsed < Duration::from_millis(1_400)
            ),
            "unexpected page-two error: {error:?}"
        );
        server.abort();
    }

    #[tokio::test]
    async fn multipart_uses_long_profile_instead_of_standard_profile() {
        let policy = HttpTimeoutPolicy {
            standard_operation: Some(Duration::from_secs(1)),
            long_operation: Some(Duration::from_secs(10)),
            ..HttpTimeoutPolicy::default()
        };
        let (client, accepted, server) =
            deadline_fixture(policy, None, ClientBuilder::new().no_proxy()).await;
        let form = reqwest::multipart::Form::new().text("field", "value");
        let multipart = tokio::spawn(async move {
            client
                .post_multipart_with_limits::<serde_json::Value>(
                    "/v1/upload",
                    form,
                    None,
                    None,
                    None,
                    None,
                )
                .await
        });
        accepted.await.expect("multipart accepted");
        tokio::time::pause();
        tokio::time::advance(Duration::from_secs(10)).await;
        assert!(matches!(
            multipart.await.expect("multipart task"),
            Err(AnytypeError::HttpTimeout {
                class: crate::http_timeout::HttpTimeoutClass::LongOperation,
                ..
            })
        ));
        server.abort();
    }

    #[tokio::test]
    async fn sse_open_and_error_body_use_separate_timeout_classes() {
        let policy = HttpTimeoutPolicy {
            sse_open: Some(Duration::from_secs(1)),
            sse_error_body: Some(Duration::from_secs(1)),
            ..HttpTimeoutPolicy::default()
        };
        let (client, accepted, server) =
            deadline_fixture(policy, None, ClientBuilder::new().no_proxy()).await;
        let open_client = client.clone();
        let open = tokio::spawn(async move {
            open_client
                .get_streaming_request("/events", QueryWithFilters::default(), HeaderMap::new())
                .await
        });
        accepted.await.expect("SSE open accepted");
        assert!(matches!(
            open.await.expect("SSE open task"),
            Err(AnytypeError::HttpTimeout {
                class: crate::http_timeout::HttpTimeoutClass::SseOpen,
                ..
            })
        ));
        server.abort();

        let prefix = b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 8\r\n\r\n{";
        let error_policy = HttpTimeoutPolicy {
            sse_open: Some(Duration::from_secs(10)),
            sse_error_body: Some(Duration::from_secs(1)),
            ..HttpTimeoutPolicy::default()
        };
        let (client, prefix_written, server) =
            deadline_fixture(error_policy, Some(prefix), ClientBuilder::new().no_proxy()).await;
        let error_body = tokio::spawn(async move {
            client
                .get_streaming_request("/events", QueryWithFilters::default(), HeaderMap::new())
                .await
        });
        prefix_written.await.expect("SSE error prefix");
        let error = error_body
            .await
            .expect("SSE error task")
            .err()
            .expect("SSE error response must fail");
        assert!(
            matches!(
                error,
                AnytypeError::HttpTimeout {
                    class: crate::http_timeout::HttpTimeoutClass::SseErrorBody,
                    ..
                }
            ),
            "unexpected SSE error: {error:?}"
        );
        server.abort();
    }

    #[tokio::test]
    async fn established_sse_default_outlives_buffered_deadline() {
        let policy = HttpTimeoutPolicy {
            standard_operation: Some(Duration::from_secs(1)),
            sse_open: Some(Duration::from_secs(120)),
            sse_idle: None,
            sse_total_lifetime: None,
            ..HttpTimeoutPolicy::default()
        };
        let (client, server) = sse_chunk_fixture(
            policy,
            vec![(Duration::from_secs(2), b"data: healthy\n\n")],
            true,
        )
        .await;
        let response = client
            .get_streaming_request("/events", QueryWithFilters::default(), HeaderMap::new())
            .await
            .expect("SSE headers open promptly");
        let mut chunks = response.bytes_stream();
        assert_eq!(
            chunks
                .next()
                .await
                .expect("SSE chunk")
                .expect("healthy SSE"),
            Bytes::from_static(b"data: healthy\n\n")
        );
        server.await.expect("SSE server");
    }

    #[tokio::test]
    async fn established_sse_idle_and_lifetime_are_independent() {
        let idle_policy = HttpTimeoutPolicy {
            sse_idle: Some(Duration::from_secs(1)),
            ..HttpTimeoutPolicy::default()
        };
        let (client, server) = sse_chunk_fixture(idle_policy, Vec::new(), false).await;
        let response = client
            .get_streaming_request("/events", QueryWithFilters::default(), HeaderMap::new())
            .await
            .expect("SSE opens");
        let mut chunks = response.bytes_stream();
        assert!(matches!(
            chunks.next().await,
            Some(Err(AnytypeError::HttpTimeout {
                class: crate::http_timeout::HttpTimeoutClass::SseIdle,
                outcome: crate::http_timeout::TimeoutOutcome::StreamTerminated,
                ..
            }))
        ));
        tokio::time::timeout(Duration::from_secs(1), server)
            .await
            .expect("timed-out SSE response closes promptly")
            .expect("SSE server");
        assert!(
            chunks.next().await.is_none(),
            "stream emits one terminal error"
        );

        let lifetime_policy = HttpTimeoutPolicy {
            sse_idle: Some(Duration::from_secs(10)),
            sse_total_lifetime: Some(Duration::from_secs(2)),
            ..HttpTimeoutPolicy::default()
        };
        let (client, server) = sse_chunk_fixture(
            lifetime_policy,
            vec![
                (Duration::from_secs(1), b": heartbeat\n\n"),
                (Duration::from_secs(1), b": heartbeat\n\n"),
                (Duration::from_secs(1), b"data: late\n\n"),
            ],
            false,
        )
        .await;
        let response = client
            .get_streaming_request("/events", QueryWithFilters::default(), HeaderMap::new())
            .await
            .expect("SSE opens");
        let mut chunks = response.bytes_stream();
        assert!(
            chunks.next().await.is_some(),
            "heartbeat counts as progress"
        );
        let terminal = chunks.next().await;
        assert!(matches!(
            terminal,
            Some(Err(AnytypeError::HttpTimeout {
                class: crate::http_timeout::HttpTimeoutClass::SseLifetime,
                ..
            }))
        ));
        server.abort();
    }

    #[tokio::test(start_paused = true)]
    async fn queued_sse_chunks_cannot_cross_exact_deadline_boundaries() {
        let started = tokio::time::Instant::now();
        let idle_state = super::EstablishedSseState {
            chunks: futures::stream::iter([Ok(Bytes::from_static(b"data: queued\n\n"))]).boxed(),
            metrics: Arc::new(HttpMetrics::new()),
            path: "/events".to_owned(),
            started,
            last_progress: started,
            idle: Some(Duration::from_secs(1)),
            lifetime: Some(Duration::from_secs(10)),
            terminated: false,
        };
        tokio::time::advance(Duration::from_secs(1)).await;
        let (result, idle_state) = idle_state.next().await.expect("idle terminal error");
        assert!(matches!(
            result,
            Err(AnytypeError::HttpTimeout {
                class: crate::http_timeout::HttpTimeoutClass::SseIdle,
                ..
            })
        ));
        assert!(idle_state.next().await.is_none());

        let started = tokio::time::Instant::now();
        let lifetime_state = super::EstablishedSseState {
            chunks: futures::stream::iter([Ok(Bytes::from_static(b"data: queued\n\n"))]).boxed(),
            metrics: Arc::new(HttpMetrics::new()),
            path: "/events".to_owned(),
            started,
            last_progress: started,
            idle: Some(Duration::from_secs(1)),
            lifetime: Some(Duration::from_secs(1)),
            terminated: false,
        };
        tokio::time::advance(Duration::from_secs(1)).await;
        let (result, lifetime_state) = lifetime_state
            .next()
            .await
            .expect("lifetime terminal error");
        assert!(matches!(
            result,
            Err(AnytypeError::HttpTimeout {
                class: crate::http_timeout::HttpTimeoutClass::SseLifetime,
                ..
            })
        ));
        assert!(lifetime_state.next().await.is_none());
    }

    async fn read_fixture_request(socket: &mut TcpStream) -> String {
        let mut request = Vec::new();
        let mut expected_len = None;
        let mut buffer = [0_u8; 1024];
        loop {
            let read = socket.read(&mut buffer).await.expect("read public request");
            if read == 0 {
                break;
            }
            request.extend_from_slice(&buffer[..read]);
            if expected_len.is_none()
                && let Some(header_end) =
                    request.windows(4).position(|window| window == b"\r\n\r\n")
            {
                let headers = String::from_utf8_lossy(&request[..header_end]);
                let body_len = headers
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().ok())
                            .flatten()
                    })
                    .unwrap_or_default();
                expected_len = Some(header_end + 4 + body_len);
            }
            if expected_len.is_some_and(|length| request.len() >= length) {
                break;
            }
        }
        String::from_utf8(request).expect("request is UTF-8")
    }

    async fn public_fixture_client(
        responses: Vec<Vec<u8>>,
        rate_limit_max_retries: u32,
    ) -> (AnytypeClient, JoinHandle<Vec<String>>) {
        public_fixture_client_with_builder(
            responses,
            rate_limit_max_retries,
            ClientBuilder::new().no_proxy(),
        )
        .await
    }

    async fn public_fixture_client_with_builder(
        responses: Vec<Vec<u8>>,
        rate_limit_max_retries: u32,
        builder: ClientBuilder,
    ) -> (AnytypeClient, JoinHandle<Vec<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind public-path fixture");
        let address = listener.local_addr().expect("public fixture address");
        let server = tokio::spawn(async move {
            let mut requests = Vec::with_capacity(responses.len());
            for response in responses {
                let (mut socket, _) = listener.accept().await.expect("accept public request");
                let request = read_fixture_request(&mut socket).await;
                socket
                    .write_all(&response)
                    .await
                    .expect("write public response");
                requests.push(request);
            }
            requests
        });

        let client =
            public_client_for(format!("http://{address}"), rate_limit_max_retries, builder);
        (client, server)
    }

    fn public_client_for(
        base_url: String,
        rate_limit_max_retries: u32,
        builder: ClientBuilder,
    ) -> AnytypeClient {
        let mut config = ClientConfig::default().app_name("retry-safety-http-fixture");
        config.base_url = Some(base_url);
        config.keystore = Some("env".to_string());
        config.disable_cache = true;
        config.rate_limit_max_retries = rate_limit_max_retries;
        let client =
            AnytypeClient::with_client(builder, config).expect("create public fixture client");
        client.set_api_key(HttpCredentials::new("fixture-secret-token"));
        client
    }

    async fn assert_public_mutation_sent_once(
        method: Method,
        status: &'static str,
        code: u16,
        rate_limit_max_retries: u32,
    ) {
        let retry_header = if code == 429 {
            "Retry-After: 0\r\nRateLimit-Reset: 0\r\n"
        } else {
            ""
        };
        let response = fixture_response(status, "fixture rejection", retry_header);
        let (client, server) = public_fixture_client(vec![response], rate_limit_max_retries).await;

        let result = public_mutation(&client, &method).await;
        let error = result.expect_err("mutation fixture must reject");
        assert!(
            matches!(
                &error,
                AnytypeError::HttpMutationIndeterminate {
                    status: Some(actual),
                    attempts: 1,
                    ..
                } if *actual == code
            ),
            "unexpected mutation error: {error:?}"
        );

        let requests = server.await.expect("public fixture task");
        assert_eq!(requests.len(), 1);
        assert!(requests[0].starts_with(method.as_str()));
        let metrics = client.http_metrics();
        assert_eq!(metrics.total_requests, 1);
        assert_eq!(metrics.physical_attempts, 1);
        assert_eq!(metrics.retries, 0);
    }

    async fn public_mutation(
        client: &AnytypeClient,
        method: &Method,
    ) -> crate::Result<crate::objects::Object> {
        if *method == Method::POST {
            client
                .new_object(TEST_SPACE_ID, "page")
                .name("retry safety")
                .no_verify()
                .create()
                .await
        } else if *method == Method::DELETE {
            client.object(TEST_SPACE_ID, TEST_OBJECT_ID).delete().await
        } else {
            client
                .update_object(TEST_SPACE_ID, TEST_OBJECT_ID)
                .name("retry safety")
                .no_verify()
                .update()
                .await
        }
    }

    #[tokio::test]
    async fn public_post_and_patch_429_and_500_are_each_sent_exactly_once() {
        // Exercise default, unlimited, and high custom retry settings. None may
        // broaden replay permission for a non-idempotent method.
        assert_public_mutation_sent_once(Method::POST, "429 Too Many Requests", 429, 0).await;
        assert_public_mutation_sent_once(
            Method::PATCH,
            "429 Too Many Requests",
            429,
            crate::config::RATE_LIMIT_MAX_RETRIES_DEFAULT,
        )
        .await;
        assert_public_mutation_sent_once(Method::POST, "500 Internal Server Error", 500, 99).await;
        assert_public_mutation_sent_once(Method::PATCH, "500 Internal Server Error", 500, 0).await;
    }

    #[tokio::test]
    async fn public_post_and_patch_408_and_504_are_each_sent_exactly_once() {
        // These statuses enter the explicit retry_for_status branch and must
        // still stop after the first non-idempotent send.
        assert_public_mutation_sent_once(Method::POST, "408 Request Timeout", 408, 0).await;
        assert_public_mutation_sent_once(
            Method::PATCH,
            "408 Request Timeout",
            408,
            crate::config::RATE_LIMIT_MAX_RETRIES_DEFAULT,
        )
        .await;
        assert_public_mutation_sent_once(Method::POST, "504 Gateway Timeout", 504, 99).await;
        assert_public_mutation_sent_once(Method::PATCH, "504 Gateway Timeout", 504, 0).await;
    }

    #[tokio::test]
    async fn public_delete_is_sent_exactly_once_for_ambiguous_statuses() {
        // DELETE was auto-replayed by the previous release's method-semantics
        // rule; this wire fixture pins the at-most-once regression directly.
        assert_public_mutation_sent_once(Method::DELETE, "429 Too Many Requests", 429, 0).await;
        assert_public_mutation_sent_once(
            Method::DELETE,
            "500 Internal Server Error",
            500,
            crate::config::RATE_LIMIT_MAX_RETRIES_DEFAULT,
        )
        .await;
        assert_public_mutation_sent_once(Method::DELETE, "408 Request Timeout", 408, 99).await;
        assert_public_mutation_sent_once(Method::DELETE, "504 Gateway Timeout", 504, 0).await;
    }

    #[tokio::test]
    async fn put_has_no_replay_permission_and_reports_indeterminate() {
        // No PUT endpoint exists today; pin the exclusion so a future PUT
        // endpoint cannot silently inherit replay permission.
        assert!(!super::is_idempotent_method(&Method::PUT));
        assert!(!super::is_idempotent_method(&Method::POST));
        assert!(!super::is_idempotent_method(&Method::PATCH));
        assert!(!super::is_idempotent_method(&Method::DELETE));
        assert!(super::is_idempotent_method(&Method::GET));
        assert!(super::is_idempotent_method(&Method::HEAD));
        assert!(super::is_idempotent_method(&Method::OPTIONS));

        const PUT_REJECTION: &[u8] = b"HTTP/1.1 500 Internal Server Error\r\nContent-Type: application/json\r\nContent-Length: 13\r\nConnection: close\r\n\r\nput rejection";
        let (client, response_written, server) = deadline_fixture(
            one_second_standard_policy(),
            Some(PUT_REJECTION),
            ClientBuilder::new().no_proxy(),
        )
        .await;
        let request_client = client.clone();
        let request = tokio::spawn(async move {
            request_client
                .send::<()>(HttpRequest {
                    method: Method::PUT,
                    path: "/put".to_owned(),
                    query: Vec::new(),
                    body: Some(Bytes::from_static(b"body")),
                })
                .await
        });
        response_written.await.expect("response written");
        let error = request
            .await
            .expect("request task")
            .expect_err("server rejection");
        assert!(
            matches!(
                &error,
                AnytypeError::HttpMutationIndeterminate {
                    status: Some(500),
                    attempts: 1,
                    ..
                }
            ),
            "unexpected PUT error: {error:?}"
        );
        let metrics = client.metrics_snapshot();
        assert_eq!(metrics.physical_attempts, 1);
        assert_eq!(metrics.retries, 0);
        server.abort();
    }

    async fn assert_public_redirect_is_not_followed(
        method: Method,
        status: &'static str,
        code: u16,
    ) {
        let redirect_target = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind redirect target");
        let target_address = redirect_target
            .local_addr()
            .expect("redirect target address");
        let target = tokio::spawn(async move {
            let accepted =
                tokio::time::timeout(Duration::from_millis(150), redirect_target.accept()).await;
            let Ok(Ok((mut socket, _))) = accepted else {
                return None;
            };
            let request = read_fixture_request(&mut socket).await;
            socket
                .write_all(&fixture_response(
                    "500 Internal Server Error",
                    "redirect replay reached target",
                    "",
                ))
                .await
                .expect("write redirect target response");
            Some(request)
        });

        let origin = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind redirect origin");
        let origin_address = origin.local_addr().expect("redirect origin address");
        let location = format!("Location: http://{target_address}/redirected\r\n");
        let redirect = fixture_response(status, "redirect response", &location);
        let source = tokio::spawn(async move {
            let (mut socket, _) = origin
                .accept()
                .await
                .expect("accept redirect source request");
            let request = read_fixture_request(&mut socket).await;
            socket
                .write_all(&redirect)
                .await
                .expect("write redirect response");
            request
        });

        // A caller-supplied follow policy is intentionally replaced.
        let builder = ClientBuilder::new()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::limited(20));
        let client = public_client_for(format!("http://{origin_address}"), 5, builder);
        let error = public_mutation(&client, &method)
            .await
            .expect_err("redirect must be surfaced without replay");
        assert!(
            matches!(&error, AnytypeError::ApiError { code: actual, .. } if *actual == code),
            "unexpected redirect error: {error:?}"
        );

        let source_request = source.await.expect("redirect source task");
        assert!(source_request.starts_with(method.as_str()));
        assert!(source_request.contains("fixture-secret-token"));
        assert!(source_request.contains("retry safety"));
        let redirected_request = target.await.expect("redirect target task");
        assert!(
            redirected_request.is_none(),
            "redirect target received a replayed request body or credentials"
        );
        let metrics = client.http_metrics();
        assert_eq!(metrics.total_requests, 1);
        assert_eq!(metrics.physical_attempts, 1);
        assert_eq!(metrics.retries, 0);
    }

    #[tokio::test]
    async fn public_post_307_and_patch_308_never_follow_or_replay_cross_origin() {
        assert_public_redirect_is_not_followed(Method::POST, "307 Temporary Redirect", 307).await;
        assert_public_redirect_is_not_followed(Method::PATCH, "308 Permanent Redirect", 308).await;
    }

    #[tokio::test]
    async fn caller_supplied_reqwest_retry_policy_cannot_replay_a_mutation() {
        let response = fixture_response("408 Request Timeout", "retry policy probe", "");
        let policy = reqwest::retry::for_host("127.0.0.1")
            .no_budget()
            .max_retries_per_request(10)
            .classify_fn(|request| {
                if request.status() == Some(StatusCode::REQUEST_TIMEOUT) {
                    request.retryable()
                } else {
                    request.success()
                }
            });
        let builder = ClientBuilder::new().no_proxy().retry(policy);
        let (client, server) = public_fixture_client_with_builder(vec![response], 0, builder).await;

        let error = public_mutation(&client, &Method::POST)
            .await
            .expect_err("caller retry policy must be overridden");
        assert!(matches!(
            error,
            AnytypeError::HttpMutationIndeterminate {
                status: Some(408),
                attempts: 1,
                ..
            }
        ));
        let requests = server.await.expect("custom retry fixture task");
        assert_eq!(requests.len(), 1);
        let metrics = client.http_metrics();
        assert_eq!(metrics.total_requests, 1);
        assert_eq!(metrics.physical_attempts, 1);
        assert_eq!(metrics.retries, 0);
    }

    #[tokio::test]
    async fn public_post_and_patch_disconnects_are_each_sent_exactly_once() {
        for (method, retry_limit) in [
            (Method::POST, 0),
            (Method::PATCH, crate::config::RATE_LIMIT_MAX_RETRIES_DEFAULT),
            (Method::DELETE, 0),
        ] {
            // An empty fixture response closes the connection after the full
            // request has arrived but before an HTTP status is available.
            let (client, server) = public_fixture_client(vec![Vec::new()], retry_limit).await;
            let error = public_mutation(&client, &method)
                .await
                .expect_err("disconnect must fail");
            assert!(
                matches!(&error, AnytypeError::HttpMutationIndeterminate { .. }),
                "unexpected disconnect error: {error:?}"
            );
            let requests = server.await.expect("disconnect fixture task");
            assert_eq!(requests.len(), 1);
            assert!(requests[0].starts_with(method.as_str()));
            let metrics = client.http_metrics();
            assert_eq!(metrics.total_requests, 1);
            assert_eq!(metrics.physical_attempts, 1);
            assert_eq!(metrics.retries, 0);
        }
    }

    #[tokio::test]
    async fn public_get_still_retries_429_after_retry_after_without_wall_delay() {
        let rejected = fixture_response(
            "429 Too Many Requests",
            "rate limited",
            "Retry-After: 0\r\nRateLimit-Reset: 0\r\n",
        );
        let body = r#"{"items":[],"pagination":{"has_more":false,"limit":1,"offset":0,"total":0}}"#;
        let success = fixture_response("200 OK", body, "");
        let (client, server) = public_fixture_client(vec![rejected, success], 1).await;

        let page = client
            .spaces()
            .limit(1)
            .list()
            .await
            .expect("GET retries after rate limiting");
        assert!(page.is_empty());
        let requests = server.await.expect("GET rate-limit fixture");
        assert_eq!(requests.len(), 2);
        assert!(requests.iter().all(|request| request.starts_with("GET ")));
        let metrics = client.http_metrics();
        assert_eq!(metrics.total_requests, 2);
        assert_eq!(metrics.physical_attempts, 2);
        assert_eq!(metrics.retries, 1);
        assert_eq!(metrics.rate_limit_errors, 1);
    }

    #[tokio::test]
    async fn public_get_status_backoff_retry_remains_enabled() {
        let rejected = fixture_response("504 Gateway Timeout", "gateway timeout", "");
        let body = r#"{"items":[],"pagination":{"has_more":false,"limit":1,"offset":0,"total":0}}"#;
        let success = fixture_response("200 OK", body, "");
        let (client, server) = public_fixture_client(vec![rejected, success], 1).await;

        let page = client
            .spaces()
            .limit(1)
            .list()
            .await
            .expect("GET retries after replay-safe status");
        assert!(page.is_empty());
        let requests = server.await.expect("GET status fixture");
        assert_eq!(requests.len(), 2);
        assert!(requests.iter().all(|request| request.starts_with("GET ")));
        let metrics = client.http_metrics();
        assert_eq!(metrics.total_requests, 2);
        assert_eq!(metrics.physical_attempts, 2);
        assert_eq!(metrics.retries, 1);
    }

    #[tokio::test]
    async fn caller_transport_timeout_stops_mixed_retry_sequence() {
        enum Reply {
            Response(Vec<u8>),
            Timeout,
        }

        let rate_limited = fixture_response(
            "429 Too Many Requests",
            "rate limited",
            "RateLimit-Reset: 0\r\n",
        );
        let timed_out = fixture_response("504 Gateway Timeout", "gateway timeout", "");
        let replies = vec![
            Reply::Response(rate_limited.clone()),
            Reply::Response(timed_out.clone()),
            Reply::Timeout,
        ];
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind alternating retry fixture");
        let address = listener.local_addr().expect("alternating fixture address");
        let server = tokio::spawn(async move {
            let mut requests = Vec::with_capacity(replies.len());
            for reply in replies {
                let (mut socket, _) = listener.accept().await.expect("accept alternating request");
                requests.push(read_fixture_request(&mut socket).await);
                match reply {
                    Reply::Response(response) => socket
                        .write_all(&response)
                        .await
                        .expect("write alternating response"),
                    Reply::Timeout => {
                        // Hold this connection open beyond the client timeout,
                        // while continuing to accept the next replay.
                        std::mem::drop(tokio::spawn(async move {
                            tokio::time::sleep(Duration::from_millis(100)).await;
                            drop(socket);
                        }));
                    }
                }
            }
            requests
        });
        let client = public_client_for(
            format!("http://{address}"),
            5,
            ClientBuilder::new()
                .no_proxy()
                .timeout(Duration::from_millis(20)),
        );

        let error = client
            .spaces()
            .limit(1)
            .list()
            .await
            .expect_err("caller transport timeout must stop the logical request");
        assert!(
            matches!(&error, AnytypeError::Http { .. }),
            "unexpected terminal error: {error:?}"
        );
        let metrics = client.http_metrics();
        assert_eq!(metrics.total_requests, 3, "terminal error: {error:?}");
        assert_eq!(metrics.logical_operations, 1);
        assert_eq!(metrics.physical_attempts, 3);
        assert_eq!(metrics.retries, 2);
        assert_eq!(metrics.rate_limit_errors, 1);
        let requests = tokio::time::timeout(Duration::from_secs(1), server)
            .await
            .expect("alternating fixture must receive all three attempts")
            .expect("alternating retry fixture");
        assert_eq!(requests.len(), 3);
        assert!(requests.iter().all(|request| request.starts_with("GET ")));
    }

    #[tokio::test]
    async fn rate_limit_specific_unbounded_setting_still_stops_at_six_attempts() {
        let rate_limited = fixture_response(
            "429 Too Many Requests",
            "rate limited",
            "RateLimit-Reset: 0\r\n",
        );
        let (client, server) = public_fixture_client(vec![rate_limited; 6], 0).await;

        let error = client
            .spaces()
            .limit(1)
            .list()
            .await
            .expect_err("the request-lifetime ceiling overrides an unbounded 429 setting");
        assert!(
            matches!(error, AnytypeError::RateLimitExceeded { .. }),
            "unexpected terminal error: {error:?}"
        );
        assert_eq!(server.await.expect("rate-limit ceiling fixture").len(), 6);
        let metrics = client.http_metrics();
        assert_eq!(metrics.total_requests, 6);
        assert_eq!(metrics.logical_operations, 1);
        assert_eq!(metrics.physical_attempts, 6);
        assert_eq!(metrics.retries, 5);
        assert_eq!(metrics.rate_limit_errors, 6);
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
                HttpTimeoutPolicy::default(),
                HttpCredentials::new("test-token"),
            )
            .expect_err("invalid response limit");
            assert!(matches!(
                error,
                crate::error::AnytypeError::Validation { .. }
            ));
        }

        for chat_sse_event_bytes in [0, crate::client::MAX_CHAT_SSE_EVENT_BYTES + 1] {
            let error = HttpClient::new(
                ClientBuilder::new().no_proxy(),
                "http://127.0.0.1:1".to_string(),
                ValidationLimits::default(),
                ResponseLimits {
                    chat_sse_event_bytes,
                    ..ResponseLimits::default()
                },
                1,
                HttpTimeoutPolicy::default(),
                HttpCredentials::new("test-token"),
            )
            .expect_err("invalid chat SSE event limit");
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
    async fn one_byte_declared_response_succeeds_with_one_byte_cap() {
        let response =
            b"HTTP/1.1 200 OK\r\nContent-Length: 1\r\nConnection: close\r\n\r\n0".to_vec();
        let (client, server) = serve_once_with_limits(response, test_limits(1, 1, 1)).await;

        let value = client
            .send::<u8>(get_request())
            .await
            .expect("one-byte declared response");
        server.await.expect("server task");
        assert_eq!(value, 0);
        assert_eq!(client.metrics_snapshot().bytes_received, 1);
    }

    #[tokio::test]
    async fn one_byte_chunked_response_succeeds_with_one_byte_cap() {
        let response = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n1\r\n0\r\n0\r\n\r\n"
            .to_vec();
        let (client, server) = serve_once_with_limits(response, test_limits(1, 1, 1)).await;

        let value = client
            .send::<u8>(get_request())
            .await
            .expect("one-byte chunked response");
        server.await.expect("server task");
        assert_eq!(value, 0);
        assert_eq!(client.metrics_snapshot().bytes_received, 1);
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
            .post_multipart_with_limits::<()>("/test", form, Some(123), None, None, None)
            .await
            .expect_err("multipart JSON response must be bounded");

        assert!(matches!(
            error,
            crate::error::AnytypeError::ResponseTooLarge { limit: 4, .. }
        ));
        server.await.expect("server task");
        let metrics = client.metrics_snapshot();
        assert_eq!(metrics.logical_operations, 1);
        assert_eq!(metrics.total_requests, 1);
        assert_eq!(metrics.physical_attempts, 1);
        assert_eq!(metrics.multipart_posts, 1);
        assert_eq!(metrics.bytes_sent, 123);
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
            HttpTimeoutPolicy::default(),
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
    fn diagnostic_path_keeps_only_bounded_non_control_path_context() {
        let secret = "URL_PASSWORD_SENTINEL";
        let path = diagnostic_path(&format!(
            "https://user:{secret}@example.invalid/v1/objects?token=QUERY_SECRET#fragment"
        ));
        assert_eq!(path, "/v1/objects");
        assert!(!path.contains(secret));
        assert!(!path.contains("QUERY_SECRET"));
        assert_eq!(
            diagnostic_path("//user:SCHEME_PASSWORD@example.invalid/v1/scheme?token=SECRET"),
            "/v1/scheme"
        );

        assert_eq!(
            diagnostic_path("/v1/spaces\nforged?authorization=HEADER_SECRET"),
            REDACTED_DIAGNOSTIC_PATH
        );

        let bounded = diagnostic_path(&format!("/{}?token=QUERY_SECRET", "x".repeat(700)));
        assert_eq!(bounded.chars().count(), MAX_DIAGNOSTIC_PATH_CHARS + 1);
        assert!(bounded.ends_with('…'));
        assert!(!bounded.contains("QUERY_SECRET"));
    }

    #[tokio::test]
    async fn malformed_targets_fail_closed_across_standard_http_diagnostics() {
        let malformed_absolute =
            "https://user:MALFORMED_PASSWORD@[invalid-host/v1?token=MALFORMED_QUERY";
        let malformed_scheme_relative =
            "//user:SCHEME_RELATIVE_PASSWORD@[invalid-host/v1?token=SCHEME_QUERY";
        let unsupported_target =
            "credential:UNSUPPORTED_PASSWORD@example.invalid/v1?token=UNSUPPORTED_QUERY";
        let control_target = "/v1/CONTROL_PATH\nCONTROL_SECRET?token=CONTROL_QUERY";
        for target in [
            malformed_absolute,
            malformed_scheme_relative,
            unsupported_target,
            control_target,
            "relative/path?token=RELATIVE_QUERY",
            "///user:TRIPLE_SLASH_PASSWORD@example.invalid/v1?token=TRIPLE_QUERY",
            "https:////user:EXCESS_SLASH_PASSWORD@example.invalid/v1?token=EXCESS_QUERY",
            "https:\\user:BACKSLASH_PASSWORD@example.invalid/v1?token=BACKSLASH_QUERY",
            "/v1/%ZZ/PERCENT_PASSWORD?token=PERCENT_QUERY",
            "/v1/SPACE PASSWORD?token=SPACE_QUERY",
        ] {
            assert_eq!(diagnostic_path(target), REDACTED_DIAGNOSTIC_PATH);
        }

        let config = ClientConfig {
            base_url: Some(malformed_absolute.to_owned()),
            app_name: "diagnostic-constructor".to_owned(),
            keystore: Some("env".to_owned()),
            keystore_service: Some("diagnostic-constructor".to_owned()),
            grpc_endpoint: Some(malformed_scheme_relative.to_owned()),
            ..ClientConfig::default()
        };
        let config_debug = format!("{config:?}");
        let (dispatch, output) = capture();
        let client = tracing::dispatcher::with_default(&dispatch, || {
            let client = AnytypeClient::with_config(config).expect("diagnostic fixture client");
            let request = HttpRequest {
                method: Method::POST,
                path: control_target.to_owned(),
                query: vec![("authorization".to_owned(), "TRACE_QUERY_SECRET".to_owned())],
                body: Some(bytes::Bytes::from_static(b"TRACE_DOCUMENT_SECRET")),
            };
            log_request(&request);
            log_response(
                malformed_scheme_relative,
                &bytes::Bytes::from_static(b"TRACE_RESPONSE_SECRET"),
            );
            log_http_status(&request, StatusCode::BAD_GATEWAY, "api_error", 1);
            client
        });
        let api_error = AnytypeError::ApiError {
            code: 502,
            method: "GET".to_owned(),
            url: malformed_absolute.to_owned(),
            message: "MALFORMED_RESPONSE_SECRET".to_owned(),
        };
        let source = reqwest::Client::new()
            .get(malformed_absolute)
            .send()
            .await
            .expect_err("malformed URL must fail before transport")
            .without_url();
        let transport_error = AnytypeError::Http {
            method: "GET".to_owned(),
            url: malformed_absolute.to_owned(),
            source,
            outcome: None,
            elapsed: None,
            attempts: None,
        };

        let mut diagnostics = format!(
            "{} {config_debug} {client:?} {:?} {api_error} {api_error:?} {} {transport_error} {transport_error:?} {}",
            output.contents(),
            client.client,
            api_error.diagnostic(),
            transport_error.diagnostic()
        );
        assert!(std::error::Error::source(&transport_error).is_none());
        let mut source = std::error::Error::source(&transport_error);
        while let Some(current) = source {
            diagnostics.push_str(&format!(" {current} {current:?}"));
            source = current.source();
        }

        assert!(diagnostics.contains(REDACTED_DIAGNOSTIC_PATH));
        for secret in [
            "MALFORMED_PASSWORD",
            "MALFORMED_QUERY",
            "SCHEME_RELATIVE_PASSWORD",
            "SCHEME_QUERY",
            "UNSUPPORTED_PASSWORD",
            "UNSUPPORTED_QUERY",
            "CONTROL_SECRET",
            "CONTROL_QUERY",
            "RELATIVE_QUERY",
            "TRIPLE_SLASH_PASSWORD",
            "TRIPLE_QUERY",
            "EXCESS_SLASH_PASSWORD",
            "EXCESS_QUERY",
            "BACKSLASH_PASSWORD",
            "BACKSLASH_QUERY",
            "PERCENT_PASSWORD",
            "PERCENT_QUERY",
            "SPACE PASSWORD",
            "SPACE_QUERY",
            "TRACE_QUERY_SECRET",
            "TRACE_DOCUMENT_SECRET",
            "TRACE_RESPONSE_SECRET",
            "MALFORMED_RESPONSE_SECRET",
        ] {
            assert!(
                !diagnostics.contains(secret),
                "standard diagnostics exposed {secret}: {diagnostics}"
            );
        }
    }

    #[tokio::test]
    async fn non_whitespace_controls_fail_closed_across_aggregated_http_surfaces() {
        let controls = [
            ('\0', "NUL"),
            ('\u{1}', "SOH"),
            ('\u{7}', "BEL"),
            ('\u{7f}', "DEL"),
        ];
        let mut target_sets = Vec::new();
        let mut secrets = Vec::new();
        for (control, label) in controls {
            let password = format!("{label}_PASSWORD_SECRET");
            let query = format!("{label}_QUERY_SECRET");
            target_sets.push([
                format!(
                    "https://user:{password}@example.invalid/v1/{control}absolute?token={query}"
                ),
                format!("//user:{password}@example.invalid/v1/{control}scheme?token={query}"),
                format!("/v1/{control}origin?token={query}"),
            ]);
            secrets.push(password);
            secrets.push(query);
        }

        for target in target_sets.iter().flatten() {
            assert_eq!(
                diagnostic_path(target),
                REDACTED_DIAGNOSTIC_PATH,
                "control-bearing target must fail closed: {target:?}"
            );
        }

        let config = ClientConfig {
            base_url: Some(target_sets[0][0].clone()),
            app_name: "control-diagnostic-constructor".to_owned(),
            keystore: Some("env".to_owned()),
            keystore_service: Some("control-diagnostic-constructor".to_owned()),
            grpc_endpoint: Some(target_sets[1][1].clone()),
            ..ClientConfig::default()
        };
        let config_debug = format!("{config:?}");
        let (dispatch, output) = capture();
        let client = tracing::dispatcher::with_default(&dispatch, || {
            let client = AnytypeClient::with_config(config).expect("control diagnostic client");
            for target in target_sets.iter().flatten() {
                let request = HttpRequest {
                    method: Method::POST,
                    path: target.clone(),
                    query: vec![(
                        "authorization".to_owned(),
                        "CONTROL_TRACE_QUERY_SECRET".to_owned(),
                    )],
                    body: Some(bytes::Bytes::from_static(b"CONTROL_TRACE_DOCUMENT_SECRET")),
                };
                log_request(&request);
                log_response(
                    target,
                    &bytes::Bytes::from_static(b"CONTROL_TRACE_RESPONSE_SECRET"),
                );
                log_http_status(&request, StatusCode::BAD_GATEWAY, "api_error", 1);
            }
            client
        });

        let mut diagnostics = format!(
            "{} {config_debug} {client:?} {:?}",
            output.contents(),
            client.client
        );
        for target in target_sets.iter().flatten() {
            let api_error = AnytypeError::ApiError {
                code: 502,
                method: "GET".to_owned(),
                url: target.clone(),
                message: "CONTROL_API_RESPONSE_SECRET".to_owned(),
            };
            diagnostics.push_str(&format!(
                " {api_error} {api_error:?} {}",
                api_error.diagnostic()
            ));

            let source = reqwest::Client::new()
                .get("relative-invalid-source")
                .send()
                .await
                .expect_err("relative source URL must fail")
                .without_url();
            let transport_error = AnytypeError::Http {
                method: "GET".to_owned(),
                url: target.clone(),
                source,
                outcome: None,
                elapsed: None,
                attempts: None,
            };
            diagnostics.push_str(&format!(
                " {transport_error} {transport_error:?} {}",
                transport_error.diagnostic()
            ));
            assert!(std::error::Error::source(&transport_error).is_none());
        }

        assert!(diagnostics.contains(REDACTED_DIAGNOSTIC_PATH));
        for (control, label) in controls {
            assert!(
                !diagnostics.contains(control),
                "diagnostics retained {label} control: {diagnostics:?}"
            );
        }
        for secret in secrets.iter().map(String::as_str).chain([
            "CONTROL_TRACE_QUERY_SECRET",
            "CONTROL_TRACE_DOCUMENT_SECRET",
            "CONTROL_TRACE_RESPONSE_SECRET",
            "CONTROL_API_RESPONSE_SECRET",
        ]) {
            assert!(
                !diagnostics.contains(secret),
                "standard diagnostics exposed {secret}: {diagnostics:?}"
            );
        }
    }

    #[test]
    fn all_http_trace_levels_remain_metadata_only() {
        let request = HttpRequest {
            method: Method::PATCH,
            path: "https://user:URL_PASSWORD@example.invalid/v1/objects?token=URL_TOKEN".to_owned(),
            query: vec![("authorization".to_owned(), "QUERY_TOKEN".to_owned())],
            body: Some(bytes::Bytes::from_static(b"DOCUMENT_BODY_SECRET")),
        };
        let (dispatch, output) = capture();
        tracing::dispatcher::with_default(&dispatch, || {
            log_request(&request);
            log_response(
                &request.path,
                &bytes::Bytes::from_static(b"RESPONSE_BODY_SECRET"),
            );
            log_http_status(&request, StatusCode::INTERNAL_SERVER_ERROR, "api_error", 2);
        });

        let diagnostics = output.contents();
        assert!(diagnostics.contains("anytype::http_json"));
        assert!(diagnostics.contains("anytype::http"));
        assert!(diagnostics.contains("/v1/objects"));
        assert!(diagnostics.contains("body_bytes=20"));
        assert!(diagnostics.contains("http_status=500"));
        for secret in [
            "URL_PASSWORD",
            "URL_TOKEN",
            "QUERY_TOKEN",
            "DOCUMENT_BODY_SECRET",
            "RESPONSE_BODY_SECRET",
            "authorization",
        ] {
            assert!(
                !diagnostics.contains(secret),
                "diagnostics exposed {secret}: {diagnostics}"
            );
        }
    }

    #[test]
    fn standard_error_and_config_diagnostics_redact_adversarial_http_values() {
        let error = AnytypeError::ApiError {
            code: 502,
            method: "get\nFORGED_METHOD".to_owned(),
            url: "https://user:URL_PASSWORD@example.invalid/v1/objects?token=URL_TOKEN".to_owned(),
            message: "UPSTREAM_RESPONSE_BODY_SECRET".to_owned(),
        };
        let safe = format!("{error} {error:?} {}", error.diagnostic());
        assert!(safe.contains("status=502"));
        assert!(safe.contains("path=/v1/objects"));
        assert!(safe.contains("method=unknown"));
        for secret in [
            "FORGED_METHOD",
            "URL_PASSWORD",
            "URL_TOKEN",
            "UPSTREAM_RESPONSE_BODY_SECRET",
        ] {
            assert!(
                !safe.contains(secret),
                "error diagnostics exposed {secret}: {safe}"
            );
        }

        let rate_limit = AnytypeError::RateLimitExceeded {
            header: "RATE_LIMIT_HEADER_SECRET".to_owned(),
            duration: Duration::from_secs(3),
        };
        let rate_limit_diagnostics =
            format!("{rate_limit} {rate_limit:?} {}", rate_limit.diagnostic());
        assert!(!rate_limit_diagnostics.contains("RATE_LIMIT_HEADER_SECRET"));

        let config = ClientConfig {
            base_url: Some(
                "https://user:CONFIG_PASSWORD@example.invalid/private?token=CONFIG_TOKEN"
                    .to_owned(),
            ),
            app_name: "APP_NAME_SECRET".to_owned(),
            keystore: Some("file:path=/KEYSTORE_PATH_SECRET".to_owned()),
            keystore_service: Some("KEYSTORE_SERVICE_SECRET".to_owned()),
            grpc_endpoint: Some(
                "https://user:GRPC_PASSWORD@example.invalid/grpc?token=GRPC_TOKEN".to_owned(),
            ),
            ..ClientConfig::default()
        };
        let config_diagnostics = format!("{config:?}");
        assert!(config_diagnostics.contains("base_path: Some(\"/private\")"));
        assert!(config_diagnostics.contains("grpc_path: Some(\"/grpc\")"));
        for secret in [
            "CONFIG_PASSWORD",
            "CONFIG_TOKEN",
            "APP_NAME_SECRET",
            "KEYSTORE_PATH_SECRET",
            "KEYSTORE_SERVICE_SECRET",
            "GRPC_PASSWORD",
            "GRPC_TOKEN",
        ] {
            assert!(
                !config_diagnostics.contains(secret),
                "config Debug exposed {secret}: {config_diagnostics}"
            );
        }
    }

    #[derive(Debug, Deserialize)]
    #[serde(rename_all = "snake_case")]
    enum DiagnosticChoice {
        Allowed,
    }

    #[derive(Debug, Deserialize)]
    #[expect(dead_code, reason = "deserialization diagnostic fixture")]
    struct DiagnosticEnvelope {
        choice: DiagnosticChoice,
    }

    #[test]
    fn deserialization_diagnostic_omits_rejected_payload_value_and_source() {
        let (dispatch, output) = capture();
        let error = tracing::dispatcher::with_default(&dispatch, || {
            deserialize_json::<DiagnosticEnvelope>(br#"{"choice":"DOCUMENT_VALUE_SECRET"}"#)
                .expect_err("unknown enum value must fail")
        });

        let diagnostics = format!(
            "{} {error} {error:?} {}",
            output.contents(),
            error.diagnostic()
        );
        assert!(diagnostics.contains("error_variant=\"deserialization\""));
        assert!(diagnostics.contains("json_category=Data"));
        assert!(!diagnostics.contains("DOCUMENT_VALUE_SECRET"));
        assert!(std::error::Error::source(&error).is_none());
    }

    #[tokio::test]
    async fn transport_error_source_chain_drops_credential_bearing_url() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("reserve closed test address");
        let address = listener.local_addr().expect("closed test address");
        drop(listener);

        let client = HttpClient::new(
            ClientBuilder::new().no_proxy(),
            format!("http://user:TRANSPORT_PASSWORD@{address}"),
            ValidationLimits::default(),
            test_limits(1024, 2048, 1024),
            1,
            HttpTimeoutPolicy::default(),
            HttpCredentials::new("AUTHORIZATION_TOKEN_SECRET"),
        )
        .expect("transport test client");
        let error = client
            .send::<()>(HttpRequest {
                method: Method::POST,
                path: "/v1/objects?token=TRANSPORT_QUERY_SECRET".to_owned(),
                query: Vec::new(),
                body: None,
            })
            .await
            .expect_err("closed test address must reject the connection");

        let mut diagnostics = format!("{error} {error:?} {}", error.diagnostic());
        assert!(std::error::Error::source(&error).is_none());
        let mut source = std::error::Error::source(&error);
        while let Some(current) = source {
            diagnostics.push_str(&format!(" {current} {current:?}"));
            source = current.source();
        }

        assert!(diagnostics.contains("path=/v1/objects"));
        for secret in [
            "TRANSPORT_PASSWORD",
            "TRANSPORT_QUERY_SECRET",
            "AUTHORIZATION_TOKEN_SECRET",
        ] {
            assert!(
                !diagnostics.contains(secret),
                "transport diagnostics exposed {secret}: {diagnostics}"
            );
        }
    }

    #[test]
    fn test_retry_for_status() {
        assert!(super::retry_for_status(StatusCode::TOO_MANY_REQUESTS));
        assert!(super::retry_for_status(StatusCode::REQUEST_TIMEOUT));
        assert!(super::retry_for_status(StatusCode::GATEWAY_TIMEOUT));
        assert!(!super::retry_for_status(StatusCode::INTERNAL_SERVER_ERROR));
        for status in [
            StatusCode::REQUEST_TIMEOUT,
            StatusCode::TOO_MANY_REQUESTS,
            StatusCode::INTERNAL_SERVER_ERROR,
            StatusCode::GATEWAY_TIMEOUT,
        ] {
            assert!(super::mutation_status_is_indeterminate(status));
        }
        for status in [StatusCode::BAD_REQUEST, StatusCode::CONFLICT] {
            assert!(!super::mutation_status_is_indeterminate(status));
        }
    }

    #[tokio::test]
    async fn preserved_mutation_status_distinguishes_ambiguous_failures() {
        for (wire_status, expected_status, indeterminate) in [
            ("400 Bad Request", 400, false),
            ("409 Conflict", 409, false),
            ("408 Request Timeout", 408, true),
            ("429 Too Many Requests", 429, true),
            ("500 Internal Server Error", 500, true),
            ("504 Gateway Timeout", 504, true),
        ] {
            let (client, server) = serve_once(fixture_response(wire_status, "", "")).await;
            let response = client
                .post_request_preserve_status::<String, _>(
                    "/test",
                    &(),
                    QueryWithFilters::default(),
                )
                .await
                .expect("preserved mutation status");
            if indeterminate {
                assert!(matches!(
                    response,
                    super::PreservedStatusResponse::Indeterminate { status }
                        if status == expected_status
                ));
            } else {
                assert!(matches!(
                    response,
                    super::PreservedStatusResponse::Rejected { status }
                        if status == expected_status
                ));
            }
            server.await.expect("preserved-status server");
        }
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
