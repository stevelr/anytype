//! HttpClient middleware used by AnytypeClient
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
use reqwest::{ClientBuilder, Method, StatusCode, header::HeaderMap};
use serde::{Serialize, de::DeserializeOwned};
use snafu::prelude::*;
use tracing::{debug, error, info, trace, warn};

use crate::{
    Result,
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

fn format_bytes(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{}B", bytes)
    } else if bytes < 1024 * 1024 {
        format!("{:.1}KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1}MB", bytes as f64 / (1024.0 * 1024.0))
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
pub(crate) struct HttpRequest {
    pub method: Method,
    pub path: String,
    pub query: Vec<(String, String)>,
    pub body: Option<Bytes>,
}

impl fmt::Debug for HttpRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HttpRequest")
            .field("method", &self.method)
            .field("path", &self.path)
            .field("query", &self.query)
            .field("body", &self.body.as_ref().map(|b| b.len()).unwrap_or(0))
            .finish()
    }
}

impl HttpRequest {
    /// Create a new request with updated pagination parameters.
    /// This replaces any existing limit/offset query parameters.
    pub(crate) fn with_pagination(&self, offset: usize, limit: usize) -> Self {
        let mut new_query: Vec<(String, String)> = self
            .query
            .iter()
            .filter(|(key, _)| key != "offset" && key != "limit")
            .cloned()
            .collect();

        new_query.push(("limit".to_string(), limit.to_string()));
        new_query.push(("offset".to_string(), offset.to_string()));

        HttpRequest {
            method: self.method.clone(),
            path: self.path.clone(),
            query: new_query,
            body: self.body.clone(),
        }
    }
}

#[derive(Debug)]
pub(crate) struct HttpClient {
    pub client: reqwest::Client,

    /// Base URL for API requests (e.g., "http://localhost:31009")
    pub base_url: String,

    pub api_key: Arc<Mutex<Option<SecretApiKey>>>,

    limits: ValidationLimits,

    // Max consecutive 429 retries before failing; 0 disables cap.
    rate_limit_max_retries: u32,

    /// HTTP request/response metrics
    pub metrics: Arc<HttpMetrics>,
}

impl Clone for HttpClient {
    fn clone(&self) -> Self {
        Self {
            client: self.client.clone(),
            base_url: self.base_url.clone(),
            api_key: self.api_key.clone(),
            limits: self.limits.clone(),
            rate_limit_max_retries: self.rate_limit_max_retries,
            metrics: self.metrics.clone(),
        }
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
            error!("Could not parse 429 response header '{header_name}: {header}'");
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
        rate_limit_max_retries: u32,
    ) -> Result<Self> {
        let client = builder.build().context(HttpSnafu {
            method: "client-init",
            url: "",
        })?;
        Ok(HttpClient {
            client,
            base_url,
            api_key: Arc::new(Mutex::new(None)),
            limits,
            rate_limit_max_retries,
            metrics: Arc::new(HttpMetrics::new()),
        })
    }

    /// Returns a snapshot of current HTTP metrics
    pub fn metrics_snapshot(&self) -> HttpMetricsSnapshot {
        self.metrics.snapshot()
    }

    /// Returns true if api_key has been initialized.
    pub fn has_key(&self) -> bool {
        self.api_key.lock().is_some()
    }

    /// Sets the API key for authenticated requests.
    pub fn set_api_key(&self, api_key: &SecretApiKey) {
        let mut write_key = self.api_key.lock();
        *write_key = Some(api_key.clone());
    }

    /// Clears the api key if set.
    pub fn clear_api_key(&self) {
        let mut write_key = self.api_key.lock();
        *write_key = None;
    }

    pub(crate) fn get_api_key(&self) -> Option<SecretApiKey> {
        self.api_key.lock().clone()
    }

    /// Makes an authenticated DELETE request.
    pub(crate) async fn delete_request<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        let req = HttpRequest {
            method: Method::DELETE,
            path: path.into(),
            query: Default::default(),
            body: Default::default(),
        };
        self.send(req).await
    }

    pub(crate) async fn get_request<T: DeserializeOwned>(
        &self,
        path: &str,
        query: QueryWithFilters,
    ) -> Result<T> {
        query.validate().map_err(|e| AnytypeError::Validation {
            message: format!("get_request {path} {e}"),
        })?;
        let req = HttpRequest {
            method: Method::GET,
            path: path.into(),
            query: query.params,
            body: None,
        };
        self.send(req).await
    }

    /// Makes an authenticated PATCH request with JSON body.
    pub(crate) async fn patch_request<T: DeserializeOwned, B: Serialize>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T, AnytypeError> {
        let req = HttpRequest {
            method: Method::PATCH,
            path: path.into(),
            query: Default::default(),
            body: Some(Bytes::from(
                serde_json::to_vec(body).context(SerializationSnafu)?,
            )),
        };
        self.send(req).await
    }

    pub(crate) async fn post_request<T: DeserializeOwned, B: Serialize>(
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

    /// Makes an unauthenticated POST request (for auth endpoints).
    pub(crate) async fn post_unauthenticated<Resp: DeserializeOwned, Req: Serialize>(
        &self,
        path: &str,
        body: &Req,
    ) -> Result<Resp> {
        let full_url = format!("{}{}", self.base_url, path);
        debug!("post_unauthenticated {full_url}");
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
            return Err(AnytypeError::ApiError {
                code: response.status().as_u16(),
                method: "post".to_string(),
                url: full_url,
                message: response.text().await.unwrap_or_default(),
            });
        }
        let data = response.bytes().await.context(HttpSnafu {
            method: "post",
            url: &full_url,
        })?;
        deserialize_json(&data)
    }

    /// This function handles all anytype rest api requests (http: get,post,patch,delete)
    /// - handles 429 rate limit feedback
    /// - retries up to N(=3) times for connection failures or server timeout
    /// - maps http error codes into AnytypeErrors
    /// - deserializes json response body into return type T
    pub(crate) async fn send<T: DeserializeOwned>(&self, req: HttpRequest) -> Result<T> {
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
                .validate_body(body, &format!("http {} {}", &req.method, &req.path))?;
        }

        let api_key = {
            let key = self.api_key.lock().clone();
            key.ok_or_else(|| AnytypeError::Auth {
                message: "API key not set. Call set_api_key() or load_key() first.".to_string(),
            })?
        };
        let full_url = format!("{}{}", self.base_url, req.path);
        let req_builder = self
            .client
            .request(req.method.clone(), &full_url)
            .query(&req.query)
            .header(ANYTYPE_API_HEADER, ANYTYPE_API_VERSION);
        let req_builder = api_key.set_auth_header(req_builder);

        // debug log (if tracing enabled)
        log_request(&req_builder, &req.body);

        // Track bytes to be sent (body size)
        let body_size = req.body.as_ref().map_or(0, |b| b.len() as u64);

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
                            let body = response.bytes().await
                            .context(HttpSnafu{
                                method: req.method.to_string(),
                                url: req.path.clone(),
                            })?;
                            // Track success and bytes received
                            self.metrics.increment_success();
                            self.metrics.add_bytes_received(body.len() as u64);

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
                                Err(e) => {
                                    error!("{e:?}");
                                    // couldn't parse header.
                                    return Err(e)
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
                                    continue;
                                }
                            }
                        }
                        StatusCode::BAD_REQUEST /* 400 */ => {
                            self.metrics.increment_errors();
                            let message = response.text().await.unwrap_or("BadRequest".into());
                            error!(?code, ?message, ?req, "http");
                            return Err(AnytypeError::Validation { message })
                        }
                        StatusCode::NOT_FOUND /* 404 */ |
                        StatusCode::GONE /* 410 */
                         => {
                            self.metrics.increment_errors();
                            let message = response.text().await.unwrap_or("NotFound".into());
                            error!(?code, ?message, ?req, "http");
                            return Err(AnytypeError::NotFound{
                                // too generic here - we don't know whether the query
                                // needs to be reported at higher level
                                obj_type: "Object".into(),
                                key: "".into()
                            })
                        },
                        StatusCode::UNAUTHORIZED /* 401 */ => {
                            // client is not authenticated
                            self.metrics.increment_errors();
                            let message = response.text().await.unwrap_or("Unauthorized".into());
                            error!(?code, ?message, ?req, "http");
                            return Err(AnytypeError::Unauthorized)
                        }
                        StatusCode::FORBIDDEN /* 403 */ => {
                            // client is authenticated, but does not have permission to access the object
                            self.metrics.increment_errors();
                            let message = response.text().await.unwrap_or("Forbidden".into());
                            error!(?code, ?message, ?req, "http");
                            return Err(AnytypeError::Forbidden)
                        }
                        _ => {
                            let message  = response.text().await.unwrap_or_default();
                            error!(?code, ?req, message, attempt, "http");
                            self.metrics.increment_errors();
                            if attempt < MAX_RETRIES && retry_for_status(code) && is_idempotent_method(&req.method)
                            {
                              log_and_backoff(attempt, code.to_string()).await;
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
                    };
                }
                Err(e) => {
                    error!(source=?e, ?req, "http");
                    // Check for connection or timeout errors
                    if (e.is_connect() || e.is_timeout()) && is_idempotent_method(&req.method) {
                        rate_limit_retries = 0;
                        if attempt < MAX_RETRIES {
                            log_and_backoff(attempt, e.to_string()).await;
                            self.metrics.increment_retries();
                            attempt += 1;
                            continue;
                        }
                        self.metrics.increment_errors();
                        return Err(AnytypeError::Http {
                            method: req.method.to_string(),
                            url: req.path,
                            source: e,
                        });
                    } else {
                        // Other non-recoverable errors (e.g., DNS error, invalid URL, etc.)
                        self.metrics.increment_errors();
                        return Err(AnytypeError::Http {
                            method: req.method.to_string(),
                            url: req.path,
                            source: e,
                        });
                    }
                }
            }
        }
    }
}

// The purpose of this trait is to define methods for Arc<HttpClient>
pub(crate) trait GetPaged {
    async fn get_request_paged<T: DeserializeOwned + Send + 'static>(
        &self,
        path: &str,
        query: QueryWithFilters,
    ) -> Result<super::paged::PagedResult<T>>;

    async fn post_request_paged<T: DeserializeOwned + Send + 'static, B: Serialize>(
        &self,
        path: &str,
        body: &B,
        query: QueryWithFilters,
    ) -> Result<super::paged::PagedResult<T>>;
}

impl GetPaged for Arc<HttpClient> {
    /// Makes an authenticated GET request that returns a PagedResult for pagination support.
    async fn get_request_paged<T: DeserializeOwned + Send + 'static>(
        &self,
        path: &str,
        query: QueryWithFilters,
    ) -> Result<super::paged::PagedResult<T>> {
        query.validate().map_err(|e| AnytypeError::Validation {
            message: format!("get_request_paged {path} {e}"),
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

    /// Makes an authenticated POST request that returns a PagedResult for pagination support.
    async fn post_request_paged<T: DeserializeOwned + Send + 'static, B: Serialize>(
        &self,
        path: &str,
        body: &B,
        query: QueryWithFilters,
    ) -> Result<super::paged::PagedResult<T>> {
        query.validate().map_err(|e| AnytypeError::Validation {
            message: format!("post_request_paged {path} {e}"),
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
fn log_request(builder: &reqwest::RequestBuilder, body: &Option<Bytes>) {
    if tracing::enabled!(target: "anytype::http_json", tracing::Level::TRACE)
        && let Some(req) = builder.try_clone().and_then(|b| b.build().ok())
    {
        let method = req.method().as_str();
        let url = req.url();
        let body = body
            .as_ref()
            .map(|b| String::from_utf8_lossy(b).to_string())
            .unwrap_or_default();
        // Log method, url (including all query parameters), and body
        // don't log headers so we don't leak api token
        trace!(target: "anytype::http_json", "{method} url={url} body={body}");
    }
}

// dump json response, for debugging
fn log_response(path: &str, body: &Bytes) {
    if tracing::enabled!(target: "anytype::http_json", tracing::Level::TRACE) {
        trace!(target: "anytype::http_json", "Response path={path} body={}",
            String::from_utf8_lossy(body)
        );
    }
}

// deserialize, reporting errors with 'serde_path_to_error', which provides
// detailed json path to the error
fn deserialize_json<T: DeserializeOwned>(body: &[u8]) -> Result<T> {
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
async fn log_and_backoff(attempt: u32, err: String) {
    // exponential backoff: 1s, 2s, 4s, with jitter
    let base_delay = 2u64.pow(attempt);
    let jitter = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos() as f64
        / 1_000_000_000.0;
    let jittered_delay = ((base_delay as f64) * (0.5 + jitter)).round() as u64;
    let delay = if jittered_delay == 0 {
        1
    } else {
        jittered_delay
    };
    warn!("Recoverable error {err}. Attempt {attempt}. Waiting {delay}s before retry");
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
    use super::parse_retry_after;
    use reqwest::StatusCode;
    use reqwest::header::{HeaderMap, HeaderValue};

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
