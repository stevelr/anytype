// any-mcp - bounded, workflow-oriented MCP server for Anytype
//
// SPDX-FileCopyrightText: 2026 Steve Schoettler
// SPDX-License-Identifier: Apache-2.0

//! Loopback listener and exact request-admission pipeline.
//!
//! Every request passes fixed gates in order: method/path and `Host`
//! validation, `Origin`/CORS, process-global rate admission, authentication,
//! request concurrency, and bounded body collection. A rejected gate performs
//! no JSON decoding, session allocation, handler permit acquisition, Anytype
//! credential access, or upstream I/O. Responses use fixed bodies that never
//! reflect a request value.

use std::{
    convert::Infallible,
    fmt,
    future::Future,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use bytes::Bytes;
use http::{HeaderMap, HeaderValue, Method, Request, Response, StatusCode, header, request::Parts};
use http_body_util::{BodyExt, Full, LengthLimitError, Limited, combinators::BoxBody};
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

use crate::http::{
    admission::RateLimiter,
    auth::{AuthRejection, Authenticator, AuthorizedPrincipal},
    config::{AllowedOrigin, HostAuthority, HttpConfig, find_allowed_origin},
};

/// Fixed request body ceiling shared with the stdio protocol frame bound.
pub(crate) const MAX_BODY_BYTES: usize = 2 * 1024 * 1024;
/// Fixed ceiling on concurrently admitted HTTP requests.
pub(crate) const MAX_CONCURRENT_REQUESTS: usize = 64;
/// Fixed wait deadline for the admitted-request semaphore.
const ADMISSION_WAIT: Duration = Duration::from_secs(10);
/// Fixed CORS preflight cache lifetime.
const PREFLIGHT_MAX_AGE: &str = "600";
const MCP_PATH: &str = "/mcp";
const METADATA_MCP_PATH: &str = "/.well-known/oauth-protected-resource/mcp";
const METADATA_ROOT_PATH: &str = "/.well-known/oauth-protected-resource";
const MCP_ALLOW: &str = "POST, GET, DELETE, OPTIONS";
const METADATA_ALLOW: &str = "GET, OPTIONS";
const CORS_ALLOW_HEADERS: &str =
    "Authorization, Content-Type, Accept, MCP-Protocol-Version, MCP-Session-Id, Last-Event-ID";
const CORS_EXPOSE_HEADERS: &str = "MCP-Session-Id";
const SESSION_ID_HEADER: &str = "mcp-session-id";

/// Response body type shared by fixed responses and the MCP service.
pub(crate) type HttpBody = BoxBody<Bytes, Infallible>;

/// One admitted, bounded request handed to the selected MCP service.
///
/// The body is fully collected under the fixed byte ceiling, and credential
/// and forwarded-identity headers are already removed: the service and the
/// domain handlers behind it never observe a bearer value.
pub(crate) struct AdmittedRequest {
    /// Sanitized request head.
    pub parts: Parts,
    /// Bounded, fully collected request body.
    pub body: Bytes,
    /// The authenticated principal for this request.
    pub principal: AuthorizedPrincipal,
}

type ServiceFuture = Pin<Box<dyn Future<Output = Response<HttpBody>> + Send>>;

/// The transport-neutral MCP service invoked after every admission gate.
pub(crate) type McpService = Arc<dyn Fn(AdmittedRequest) -> ServiceFuture + Send + Sync>;

/// Immutable per-process listener state constructed before the bind.
pub(crate) struct ListenerState {
    allowed_hosts: Vec<HostAuthority>,
    allowed_origins: Vec<AllowedOrigin>,
    /// Fixed public protected-resource metadata document, present only in
    /// OAuth mode. `None` disables both metadata routes.
    metadata: Option<Arc<str>>,
    authenticator: Authenticator,
    rate: RateLimiter,
    permits: Arc<Semaphore>,
    admission_wait: Duration,
    correlation: AtomicU64,
    service: McpService,
}

impl ListenerState {
    pub(crate) fn new(
        config: &HttpConfig,
        authenticator: Authenticator,
        metadata: Option<Arc<str>>,
        service: McpService,
    ) -> Self {
        Self {
            allowed_hosts: config.allowed_hosts.clone(),
            allowed_origins: config.allowed_origins.clone(),
            metadata,
            authenticator,
            rate: RateLimiter::new(config.requests_per_minute),
            permits: Arc::new(Semaphore::new(MAX_CONCURRENT_REQUESTS)),
            admission_wait: ADMISSION_WAIT,
            correlation: AtomicU64::new(0),
            service,
        }
    }

    #[cfg(test)]
    pub(crate) fn with_admission_bounds(
        mut self,
        permits: usize,
        admission_wait: Duration,
    ) -> Self {
        self.permits = Arc::new(Semaphore::new(permits));
        self.admission_wait = admission_wait;
        self
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Route {
    Mcp,
    Metadata,
}

impl Route {
    const fn allow(self) -> &'static str {
        match self {
            Self::Mcp => MCP_ALLOW,
            Self::Metadata => METADATA_ALLOW,
        }
    }

    const fn category(self) -> &'static str {
        match self {
            Self::Mcp => "mcp",
            Self::Metadata => "metadata",
        }
    }
}

fn classify_route(path: &str, metadata_enabled: bool) -> Option<Route> {
    match path {
        MCP_PATH => Some(Route::Mcp),
        METADATA_MCP_PATH | METADATA_ROOT_PATH if metadata_enabled => Some(Route::Metadata),
        _ => None,
    }
}

fn method_category(method: &Method) -> &'static str {
    match *method {
        Method::GET => "GET",
        Method::POST => "POST",
        Method::DELETE => "DELETE",
        Method::OPTIONS => "OPTIONS",
        _ => "other",
    }
}

/// Handles one request through the fixed gate order with completion logging.
pub(crate) async fn handle_request<B>(
    state: &ListenerState,
    request: Request<B>,
) -> Response<HttpBody>
where
    B: http_body::Body<Data = Bytes> + Send + 'static,
    B::Error: std::error::Error + Send + Sync + 'static,
{
    let correlation = state.correlation.fetch_add(1, Ordering::Relaxed);
    let started = Instant::now();
    let method = method_category(request.method());
    let route = classify_route(request.uri().path(), state.metadata.is_some());
    let session_present = request.headers().contains_key(SESSION_ID_HEADER);

    let response = admit(state, request, route).await;

    tracing::info!(
        target: "any_mcp::http",
        correlation,
        method,
        route = route.map_or("unknown", Route::category),
        status = response.status().as_u16(),
        elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        session_present,
        "http_request_completed"
    );
    response
}

async fn admit<B>(
    state: &ListenerState,
    request: Request<B>,
    route: Option<Route>,
) -> Response<HttpBody>
where
    B: http_body::Body<Data = Bytes> + Send + 'static,
    B::Error: std::error::Error + Send + Sync + 'static,
{
    // Gate 1: fixed path and method.
    let Some(route) = route else {
        return fixed_response(StatusCode::NOT_FOUND, "Not Found");
    };
    let method_allowed = match route {
        Route::Mcp => matches!(
            *request.method(),
            Method::POST | Method::GET | Method::DELETE | Method::OPTIONS
        ),
        Route::Metadata => matches!(*request.method(), Method::GET | Method::OPTIONS),
    };
    if !method_allowed {
        return method_not_allowed(route.allow());
    }

    // Gate 1b: exact Host validation before any other processing.
    if !host_allowed(&request, &state.allowed_hosts) {
        tracing::info!(target: "any_mcp::http", "http_host_rejected");
        return fixed_response(StatusCode::FORBIDDEN, "Forbidden");
    }

    // Gate 2: exact Origin validation.
    let origin = match evaluate_origin(request.headers(), &state.allowed_origins) {
        Ok(origin) => origin,
        Err(()) => {
            tracing::info!(target: "any_mcp::http", "http_origin_rejected");
            return fixed_response(StatusCode::FORBIDDEN, "Forbidden");
        }
    };

    // Gate 3: process-global rate admission, counting preflight and
    // later-rejected requests.
    if !state.rate.try_admit() {
        tracing::info!(target: "any_mcp::http", "http_rate_rejected");
        let response = rate_limited_response();
        return with_cors(response, origin);
    }

    // Gate 4a: validated CORS preflight is the only unauthenticated OPTIONS.
    if *request.method() == Method::OPTIONS {
        return origin.map_or_else(|| method_not_allowed(route.allow()), preflight_response);
    }

    // Gate 4b: the two fixed RFC 9728 metadata routes return only immutable
    // public configuration and never require a bearer.
    if route == Route::Metadata {
        let Some(document) = state.metadata.as_ref() else {
            tracing::error!(target: "any_mcp::http", "http_metadata_state_unavailable");
            return fixed_response(StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error");
        };
        return with_cors(metadata_response(document), origin);
    }

    // Gate 4c: bearer authentication and required authority on every other
    // route.
    let principal = match state.authenticator.authenticate(request.headers()).await {
        Ok(principal) => principal,
        Err(AuthRejection::Unauthorized) => {
            tracing::info!(target: "any_mcp::http", "http_auth_rejected");
            return with_cors(
                unauthorized_response(state.authenticator.challenge()),
                origin,
            );
        }
        Err(AuthRejection::Oversized) => {
            tracing::info!(target: "any_mcp::http", "http_auth_rejected");
            return with_cors(
                fixed_response(
                    StatusCode::REQUEST_HEADER_FIELDS_TOO_LARGE,
                    "Request Header Fields Too Large",
                ),
                origin,
            );
        }
    };

    // Gate 5: bounded request concurrency before body collection.
    let Ok(Ok(permit)) =
        tokio::time::timeout(state.admission_wait, state.permits.clone().acquire_owned()).await
    else {
        tracing::info!(target: "any_mcp::http", "http_capacity_rejected");
        return with_cors(
            fixed_response(StatusCode::SERVICE_UNAVAILABLE, "Service Unavailable"),
            origin,
        );
    };

    // Gate 6: bounded body collection before any JSON decoding.
    let (parts, body) = request.into_parts();
    let collected = match Limited::new(body, MAX_BODY_BYTES).collect().await {
        Ok(collected) => collected.to_bytes(),
        Err(error) => {
            let response = if error.downcast_ref::<LengthLimitError>().is_some() {
                fixed_response(StatusCode::PAYLOAD_TOO_LARGE, "Payload Too Large")
            } else {
                fixed_response(StatusCode::BAD_REQUEST, "Bad Request")
            };
            return with_cors(response, origin);
        }
    };
    let parts = sanitize_parts(parts);

    // Dispatch to the selected MCP service. The admission permit covers the
    // service call; streaming response bodies are bounded separately by
    // session, queue, deadline, and shutdown limits.
    let response = (state.service)(AdmittedRequest {
        parts,
        body: collected,
        principal,
    })
    .await;
    drop(permit);
    with_cors(response, origin)
}

/// Removes credential and forwarded-identity headers from the admitted
/// request head so no downstream code can observe them.
fn sanitize_parts(mut parts: Parts) -> Parts {
    const REMOVED: [&str; 8] = [
        "authorization",
        "proxy-authorization",
        "cookie",
        "forwarded",
        "x-forwarded-for",
        "x-forwarded-host",
        "x-forwarded-proto",
        "x-real-ip",
    ];
    for name in REMOVED {
        while parts.headers.remove(name).is_some() {}
    }
    parts
}

fn host_allowed<B>(request: &Request<B>, allowed: &[HostAuthority]) -> bool {
    let mut hosts = request.headers().get_all(header::HOST).iter();
    let value = match (hosts.next(), hosts.next()) {
        (Some(value), None) => value,
        _ => return false,
    };
    let Ok(value) = value.to_str() else {
        return false;
    };
    let Some((host, port)) = parse_request_authority(value) else {
        return false;
    };
    // An absolute-form request target must agree with the Host header.
    if let Some(authority) = request.uri().authority() {
        let target = parse_request_authority(authority.as_str());
        if target.as_ref() != Some(&(host.clone(), port)) {
            return false;
        }
    }
    allowed
        .iter()
        .any(|authority| authority.matches(&host, port))
}

/// Parses one request authority into a lowercase host and optional port.
///
/// Bracketed IPv6 addresses must parse exactly; every other host is limited
/// to DNS/IPv4 characters. Userinfo, wildcards, whitespace, and control
/// bytes never parse.
fn parse_request_authority(value: &str) -> Option<(String, Option<u16>)> {
    if value.is_empty()
        || value.len() > 256
        || !value.bytes().all(|byte| byte.is_ascii_graphic())
        || value.contains('@')
        || value.contains('*')
    {
        return None;
    }
    if let Some(rest) = value.strip_prefix('[') {
        let (address, remainder) = rest.split_once(']')?;
        address.parse::<std::net::Ipv6Addr>().ok()?;
        let port = match remainder.strip_prefix(':') {
            Some(port) => Some(parse_port(port)?),
            None if remainder.is_empty() => None,
            None => return None,
        };
        return Some((address.to_ascii_lowercase(), port));
    }
    let (host, port) = match value.rsplit_once(':') {
        Some((host, port)) => {
            if host.contains(':') {
                return None;
            }
            (host, Some(parse_port(port)?))
        }
        None => (value, None),
    };
    if host.is_empty()
        || !host
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'.' || byte == b'-')
    {
        return None;
    }
    Some((host.to_ascii_lowercase(), port))
}

fn parse_port(port: &str) -> Option<u16> {
    if port.is_empty() || !port.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    port.parse::<u16>().ok()
}

/// Evaluates the `Origin` header against the exact allowlist.
///
/// A missing header is allowed for native clients. A present header must be
/// single, well-formed, and exactly listed; an empty allowlist rejects every
/// Origin-bearing request.
fn evaluate_origin<'a>(
    headers: &HeaderMap,
    allowed: &'a [AllowedOrigin],
) -> Result<Option<&'a AllowedOrigin>, ()> {
    let mut origins = headers.get_all(header::ORIGIN).iter();
    let Some(value) = origins.next() else {
        return Ok(None);
    };
    if origins.next().is_some() {
        return Err(());
    }
    let value = value.to_str().map_err(|_| ())?;
    find_allowed_origin(allowed, value).map(Some).ok_or(())
}

pub(crate) fn fixed_response(status: StatusCode, body: &'static str) -> Response<HttpBody> {
    let mut response = Response::new(Full::new(Bytes::from_static(body.as_bytes())).boxed());
    *response.status_mut() = status;
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    response
}

fn method_not_allowed(allow: &'static str) -> Response<HttpBody> {
    let mut response = fixed_response(StatusCode::METHOD_NOT_ALLOWED, "Method Not Allowed");
    response
        .headers_mut()
        .insert(header::ALLOW, HeaderValue::from_static(allow));
    response
}

fn rate_limited_response() -> Response<HttpBody> {
    let mut response = fixed_response(StatusCode::TOO_MANY_REQUESTS, "Too Many Requests");
    response
        .headers_mut()
        .insert(header::RETRY_AFTER, HeaderValue::from_static("60"));
    response
}

fn unauthorized_response(challenge: &str) -> Response<HttpBody> {
    let mut response = fixed_response(StatusCode::UNAUTHORIZED, "Unauthorized");
    let challenge =
        HeaderValue::from_str(challenge).unwrap_or_else(|_| HeaderValue::from_static("Bearer"));
    response
        .headers_mut()
        .insert(header::WWW_AUTHENTICATE, challenge);
    response
}

fn metadata_response(document: &str) -> Response<HttpBody> {
    let mut response =
        Response::new(Full::new(Bytes::copy_from_slice(document.as_bytes())).boxed());
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    response
}

fn preflight_response(origin: &AllowedOrigin) -> Response<HttpBody> {
    let mut response = Response::new(Full::new(Bytes::new()).boxed());
    *response.status_mut() = StatusCode::NO_CONTENT;
    let headers = response.headers_mut();
    insert_cors_headers(headers, origin);
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_METHODS,
        HeaderValue::from_static(MCP_ALLOW),
    );
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_HEADERS,
        HeaderValue::from_static(CORS_ALLOW_HEADERS),
    );
    headers.insert(
        header::ACCESS_CONTROL_MAX_AGE,
        HeaderValue::from_static(PREFLIGHT_MAX_AGE),
    );
    response
}

fn with_cors(
    mut response: Response<HttpBody>,
    origin: Option<&AllowedOrigin>,
) -> Response<HttpBody> {
    if let Some(origin) = origin {
        insert_cors_headers(response.headers_mut(), origin);
    }
    response
}

fn insert_cors_headers(headers: &mut HeaderMap, origin: &AllowedOrigin) {
    if let Ok(value) = HeaderValue::from_str(origin.serialized()) {
        headers.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, value);
    }
    headers.insert(header::VARY, HeaderValue::from_static("Origin"));
    headers.insert(
        header::ACCESS_CONTROL_EXPOSE_HEADERS,
        HeaderValue::from_static(CORS_EXPOSE_HEADERS),
    );
}

/// A safe HTTP service failure without addresses or payloads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HttpServeError {
    /// The listener or a connection task failed fatally.
    Listener,
}

impl fmt::Display for HttpServeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Listener => formatter.write_str("HTTP listener service failed"),
        }
    }
}

impl std::error::Error for HttpServeError {}

/// Builds the fixed single-fallback router over the admission pipeline.
fn router(state: Arc<ListenerState>) -> axum::Router {
    axum::Router::new().fallback(move |request: axum::extract::Request| {
        let state = state.clone();
        async move {
            handle_request(state.as_ref(), request)
                .await
                .map(axum::body::Body::new)
        }
    })
}

/// Serves the bound loopback listener until shutdown.
///
/// Cancelling `shutdown` stops accepting connections, drains in-flight work
/// until the configured deadline, then cancels the remainder. The caller owns
/// signal handling and emits its own ready/stopping diagnostics around this
/// call.
///
/// # Errors
///
/// Returns a redacted [`HttpServeError`] for a fatal listener failure. A
/// drained or deadline-cancelled shutdown is success.
pub(crate) async fn run_listener(
    listener: tokio::net::TcpListener,
    state: Arc<ListenerState>,
    shutdown: CancellationToken,
    drain: Duration,
) -> Result<(), HttpServeError> {
    use std::future::IntoFuture;

    let app = router(state);
    let signal = shutdown.clone();
    let serve = axum::serve(listener, app)
        .with_graceful_shutdown(async move { signal.cancelled().await })
        .into_future();
    let mut serve = std::pin::pin!(serve);
    tokio::select! {
        result = &mut serve => result.map_err(|_| HttpServeError::Listener),
        () = shutdown.cancelled() => {
            match tokio::time::timeout(drain, &mut serve).await {
                Ok(result) => result.map_err(|_| HttpServeError::Listener),
                // The drain deadline cancels remaining connections by drop.
                Err(_) => Ok(()),
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::sync::{Mutex, atomic::AtomicUsize};

    use super::*;
    use crate::http::config::{HttpAuthConfig, TransportSelection};

    struct Recorded {
        calls: Arc<AtomicUsize>,
        last: Arc<Mutex<Option<(HeaderMap, AuthorizedPrincipal, Bytes)>>>,
    }

    fn recording_service() -> (McpService, Recorded) {
        let calls = Arc::new(AtomicUsize::new(0));
        let last = Arc::new(Mutex::new(None));
        let recorded = Recorded {
            calls: calls.clone(),
            last: last.clone(),
        };
        let service: McpService = Arc::new(move |admitted: AdmittedRequest| {
            calls.fetch_add(1, Ordering::SeqCst);
            let last = last.clone();
            Box::pin(async move {
                *last.lock().expect("test lock") =
                    Some((admitted.parts.headers, admitted.principal, admitted.body));
                fixed_response(StatusCode::OK, "ok")
            })
        });
        (service, recorded)
    }

    pub(crate) fn test_config(extra: &[(&str, &str)]) -> HttpConfig {
        let mut values = vec![
            ("ANY_MCP_TRANSPORT".to_owned(), "streamable-http".to_owned()),
            ("ANY_MCP_HTTP_AUTH".to_owned(), "static-token".to_owned()),
            (
                "ANY_MCP_HTTP_TOKEN_FILE".to_owned(),
                "/etc/any-mcp/token".to_owned(),
            ),
        ];
        values.extend(
            extra
                .iter()
                .map(|(name, value)| ((*name).to_owned(), (*value).to_owned())),
        );
        let map = values
            .into_iter()
            .collect::<std::collections::HashMap<_, _>>();
        let present = map
            .keys()
            .filter(|key| key.starts_with("ANY_MCP_HTTP_"))
            .cloned()
            .collect::<Vec<_>>();
        match TransportSelection::from_lookup(|name| Ok(map.get(name).cloned()), &present) {
            Ok(TransportSelection::StreamableHttp(config)) => {
                let config = *config;
                assert!(matches!(config.auth, HttpAuthConfig::StaticToken { .. }));
                config
            }
            other => panic!("expected http config, got {other:?}"),
        }
    }

    fn test_state(extra: &[(&str, &str)]) -> (Arc<ListenerState>, Recorded) {
        let (service, recorded) = recording_service();
        let state = ListenerState::new(
            &test_config(extra),
            Authenticator::SyntheticAllow,
            None,
            service,
        );
        (Arc::new(state), recorded)
    }

    fn request(method: Method, path: &str, headers: &[(&str, &str)]) -> Request<Full<Bytes>> {
        request_with_body(method, path, headers, Bytes::new())
    }

    fn request_with_body(
        method: Method,
        path: &str,
        headers: &[(&str, &str)],
        body: Bytes,
    ) -> Request<Full<Bytes>> {
        let mut builder = Request::builder().method(method).uri(path);
        if !headers.iter().any(|(name, _)| *name == "host") {
            builder = builder.header("host", "localhost:8000");
        }
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        builder.body(Full::new(body)).expect("test request")
    }

    const AUTH: (&str, &str) = ("authorization", "Bearer synthetic-token");

    #[tokio::test]
    async fn enabled_metadata_routes_are_public_fixed_json() {
        let (service, recorded) = recording_service();
        let document = r#"{"resource":"https://mcp.example.com/mcp"}"#;
        let state = Arc::new(ListenerState::new(
            &test_config(&[]),
            Authenticator::SyntheticAllow,
            Some(Arc::from(document)),
            service,
        ));
        for path in [METADATA_MCP_PATH, METADATA_ROOT_PATH] {
            // No Authorization header: metadata is public after the earlier
            // Host, Origin, rate, and method gates.
            let response = handle_request(&state, request(Method::GET, path, &[])).await;
            assert_eq!(response.status(), StatusCode::OK, "{path}");
            assert_eq!(
                response.headers().get(header::CONTENT_TYPE).unwrap(),
                "application/json"
            );
            let body = response.into_body().collect().await.unwrap().to_bytes();
            assert_eq!(&body[..], document.as_bytes());
        }
        // Metadata routes still enforce the fixed method set and Host gate.
        let response = handle_request(&state, request(Method::POST, METADATA_ROOT_PATH, &[])).await;
        assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
        assert_eq!(
            response.headers().get(header::ALLOW).unwrap(),
            METADATA_ALLOW
        );
        let response = handle_request(
            &state,
            request(Method::GET, METADATA_MCP_PATH, &[("host", "evil.test")]),
        )
        .await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert_eq!(recorded.calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn unknown_paths_and_disabled_metadata_return_404() {
        let (state, recorded) = test_state(&[]);
        for path in [
            "/",
            "/mcp/",
            "/mcp/extra",
            "/other",
            METADATA_MCP_PATH,
            METADATA_ROOT_PATH,
        ] {
            let response = handle_request(&state, request(Method::POST, path, &[AUTH])).await;
            assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
        }
        assert_eq!(recorded.calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn unsupported_methods_return_405_with_fixed_allow() {
        let (state, recorded) = test_state(&[]);
        for method in [Method::PUT, Method::PATCH, Method::HEAD] {
            let response = handle_request(&state, request(method.clone(), "/mcp", &[AUTH])).await;
            assert_eq!(
                response.status(),
                StatusCode::METHOD_NOT_ALLOWED,
                "{method}"
            );
            assert_eq!(response.headers().get(header::ALLOW).unwrap(), MCP_ALLOW);
        }
        assert_eq!(recorded.calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn host_gate_rejects_missing_duplicate_malformed_and_foreign_hosts() {
        let (state, recorded) = test_state(&[]);

        let mut missing = request(Method::POST, "/mcp", &[AUTH]);
        missing.headers_mut().remove(header::HOST);
        let response = handle_request(&state, missing).await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);

        let mut duplicate = request(Method::POST, "/mcp", &[AUTH]);
        duplicate
            .headers_mut()
            .append(header::HOST, HeaderValue::from_static("localhost:8000"));
        let response = handle_request(&state, duplicate).await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);

        for host in [
            "evil.test",
            "localhost.evil.test",
            "localhost:8000:1",
            "user@localhost",
            "local host",
            "[::1",
            "127.0.0.1.nip.io",
        ] {
            let response = handle_request(
                &state,
                request(Method::POST, "/mcp", &[("host", host), AUTH]),
            )
            .await;
            assert_eq!(response.status(), StatusCode::FORBIDDEN, "{host}");
            let body = response.into_body().collect().await.unwrap().to_bytes();
            assert_eq!(&body[..], b"Forbidden");
        }
        assert_eq!(recorded.calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn host_gate_admits_default_local_authorities() {
        let (state, recorded) = test_state(&[]);
        for host in [
            "localhost",
            "localhost:8000",
            "127.0.0.1:9000",
            "[::1]:8000",
            "LOCALHOST",
        ] {
            let response = handle_request(
                &state,
                request(Method::POST, "/mcp", &[("host", host), AUTH]),
            )
            .await;
            assert_eq!(response.status(), StatusCode::OK, "{host}");
        }
        assert_eq!(recorded.calls.load(Ordering::SeqCst), 5);
    }

    #[tokio::test]
    async fn origin_gate_is_exact_and_fails_closed_without_configuration() {
        let (state, recorded) = test_state(&[]);
        let response = handle_request(
            &state,
            request(
                Method::POST,
                "/mcp",
                &[("origin", "https://app.example.com"), AUTH],
            ),
        )
        .await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert_eq!(recorded.calls.load(Ordering::SeqCst), 0);

        let (state, recorded) =
            test_state(&[("ANY_MCP_HTTP_ALLOWED_ORIGINS", "https://app.example.com")]);
        for (origin, expected) in [
            ("https://app.example.com", StatusCode::OK),
            ("https://app.example.com:443", StatusCode::OK),
            ("http://app.example.com", StatusCode::FORBIDDEN),
            ("https://evil.test", StatusCode::FORBIDDEN),
            ("null", StatusCode::FORBIDDEN),
            ("garbage origin", StatusCode::FORBIDDEN),
        ] {
            let response = handle_request(
                &state,
                request(Method::POST, "/mcp", &[("origin", origin), AUTH]),
            )
            .await;
            assert_eq!(response.status(), expected, "{origin}");
        }

        let mut duplicate = request(
            Method::POST,
            "/mcp",
            &[("origin", "https://app.example.com"), AUTH],
        );
        duplicate.headers_mut().append(
            header::ORIGIN,
            HeaderValue::from_static("https://app.example.com"),
        );
        let response = handle_request(&state, duplicate).await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert_eq!(recorded.calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn allowed_origin_headers_are_attached_to_success_and_fixed_errors() {
        let (state, _recorded) =
            test_state(&[("ANY_MCP_HTTP_ALLOWED_ORIGINS", "https://app.example.com")]);
        let ok = handle_request(
            &state,
            request(
                Method::POST,
                "/mcp",
                &[("origin", "https://app.example.com"), AUTH],
            ),
        )
        .await;
        assert_eq!(ok.status(), StatusCode::OK);
        assert_eq!(
            ok.headers()
                .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
                .unwrap(),
            "https://app.example.com"
        );
        assert_eq!(ok.headers().get(header::VARY).unwrap(), "Origin");
        assert_eq!(
            ok.headers()
                .get(header::ACCESS_CONTROL_EXPOSE_HEADERS)
                .unwrap(),
            CORS_EXPOSE_HEADERS
        );

        let unauthorized = handle_request(
            &state,
            request(
                Method::POST,
                "/mcp",
                &[("origin", "https://app.example.com")],
            ),
        )
        .await;
        assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(
            unauthorized
                .headers()
                .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
                .unwrap(),
            "https://app.example.com"
        );
    }

    #[tokio::test]
    async fn preflight_is_unauthenticated_with_exact_fixed_policy() {
        let (state, recorded) =
            test_state(&[("ANY_MCP_HTTP_ALLOWED_ORIGINS", "https://app.example.com")]);
        let response = handle_request(
            &state,
            request(
                Method::OPTIONS,
                "/mcp",
                &[
                    ("origin", "https://app.example.com"),
                    ("access-control-request-method", "POST"),
                ],
            ),
        )
        .await;
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        let headers = response.headers();
        assert_eq!(
            headers.get(header::ACCESS_CONTROL_ALLOW_ORIGIN).unwrap(),
            "https://app.example.com"
        );
        assert_eq!(
            headers.get(header::ACCESS_CONTROL_ALLOW_METHODS).unwrap(),
            MCP_ALLOW
        );
        assert_eq!(
            headers.get(header::ACCESS_CONTROL_ALLOW_HEADERS).unwrap(),
            CORS_ALLOW_HEADERS
        );
        assert_eq!(headers.get(header::ACCESS_CONTROL_MAX_AGE).unwrap(), "600");
        assert!(
            headers
                .get(header::ACCESS_CONTROL_ALLOW_CREDENTIALS)
                .is_none()
        );
        assert_eq!(recorded.calls.load(Ordering::SeqCst), 0);

        // OPTIONS without an allowed Origin is not a preflight.
        let response = handle_request(&state, request(Method::OPTIONS, "/mcp", &[])).await;
        assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
    }

    #[tokio::test]
    async fn rate_gate_is_process_global_and_counts_preflight() {
        let (state, recorded) = test_state(&[
            ("ANY_MCP_HTTP_REQUESTS_PER_MINUTE", "2"),
            ("ANY_MCP_HTTP_ALLOWED_ORIGINS", "https://app.example.com"),
        ]);
        let preflight = handle_request(
            &state,
            request(
                Method::OPTIONS,
                "/mcp",
                &[("origin", "https://app.example.com")],
            ),
        )
        .await;
        assert_eq!(preflight.status(), StatusCode::NO_CONTENT);
        let ok = handle_request(&state, request(Method::POST, "/mcp", &[AUTH])).await;
        assert_eq!(ok.status(), StatusCode::OK);
        let limited = handle_request(&state, request(Method::POST, "/mcp", &[AUTH])).await;
        assert_eq!(limited.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(limited.headers().get(header::RETRY_AFTER).unwrap(), "60");
        assert_eq!(recorded.calls.load(Ordering::SeqCst), 1);

        // A Host rejection happens before the rate gate and consumes nothing.
        let (state, _recorded) = test_state(&[("ANY_MCP_HTTP_REQUESTS_PER_MINUTE", "1")]);
        for _ in 0..3 {
            let response = handle_request(
                &state,
                request(Method::POST, "/mcp", &[("host", "evil.test"), AUTH]),
            )
            .await;
            assert_eq!(response.status(), StatusCode::FORBIDDEN);
        }
        let ok = handle_request(&state, request(Method::POST, "/mcp", &[AUTH])).await;
        assert_eq!(ok.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn authentication_failures_use_fixed_statuses_and_challenge() {
        let (state, recorded) = test_state(&[]);
        let missing = handle_request(&state, request(Method::POST, "/mcp", &[])).await;
        assert_eq!(missing.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(
            missing.headers().get(header::WWW_AUTHENTICATE).unwrap(),
            "Bearer"
        );

        let oversized = format!("Bearer {}", "a".repeat(1024));
        let response = handle_request(
            &state,
            request(Method::POST, "/mcp", &[("authorization", &oversized)]),
        )
        .await;
        assert_eq!(
            response.status(),
            StatusCode::REQUEST_HEADER_FIELDS_TOO_LARGE
        );
        assert_eq!(recorded.calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn body_bound_is_exactly_two_mebibytes() {
        let (state, recorded) = test_state(&[]);
        let exact = Bytes::from(vec![b'a'; MAX_BODY_BYTES]);
        let response = handle_request(
            &state,
            request_with_body(Method::POST, "/mcp", &[AUTH], exact.clone()),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        {
            let last = recorded.last.lock().expect("test lock");
            let (_, _, body) = last.as_ref().expect("recorded call");
            assert_eq!(body.len(), MAX_BODY_BYTES);
        }

        let over = Bytes::from(vec![b'a'; MAX_BODY_BYTES + 1]);
        let response = handle_request(
            &state,
            request_with_body(Method::POST, "/mcp", &[AUTH], over),
        )
        .await;
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
        assert_eq!(recorded.calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn admitted_requests_are_sanitized_and_carry_the_principal() {
        let (state, recorded) = test_state(&[]);
        let response = handle_request(
            &state,
            request(
                Method::POST,
                "/mcp",
                &[
                    AUTH,
                    ("cookie", "session=1"),
                    ("x-forwarded-for", "203.0.113.7"),
                    ("forwarded", "for=203.0.113.7"),
                    ("mcp-session-id", "session-1"),
                ],
            ),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let last = recorded.last.lock().expect("test lock");
        let (headers, principal, _) = last.as_ref().expect("recorded call");
        assert!(headers.get(header::AUTHORIZATION).is_none());
        assert!(headers.get(header::COOKIE).is_none());
        assert!(headers.get("x-forwarded-for").is_none());
        assert!(headers.get("forwarded").is_none());
        assert_eq!(headers.get("mcp-session-id").unwrap(), "session-1");
        let expected = AuthorizedPrincipal::from_identity_material("synthetic", b"synthetic-token");
        assert_eq!(principal, &expected);
    }

    #[tokio::test]
    async fn request_concurrency_exhaustion_returns_503_after_the_wait() {
        let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
        let release_rx = Arc::new(Mutex::new(Some(release_rx)));
        let service: McpService = Arc::new(move |_admitted| {
            let release_rx = release_rx.clone();
            Box::pin(async move {
                let receiver = release_rx.lock().expect("test lock").take();
                if let Some(receiver) = receiver {
                    let _ = receiver.await;
                }
                fixed_response(StatusCode::OK, "ok")
            })
        });
        let state = Arc::new(
            ListenerState::new(
                &test_config(&[]),
                Authenticator::SyntheticAllow,
                None,
                service,
            )
            .with_admission_bounds(1, Duration::from_millis(50)),
        );

        let blocked_state = state.clone();
        let blocked = tokio::spawn(async move {
            handle_request(
                blocked_state.as_ref(),
                request(Method::POST, "/mcp", &[AUTH]),
            )
            .await
        });
        tokio::time::sleep(Duration::from_millis(10)).await;

        let rejected = handle_request(&state, request(Method::POST, "/mcp", &[AUTH])).await;
        assert_eq!(rejected.status(), StatusCode::SERVICE_UNAVAILABLE);

        release_tx.send(()).expect("release blocked request");
        let response = blocked.await.expect("blocked request join");
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn listener_serves_and_drains_over_a_real_loopback_socket() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let (state, _recorded) = test_state(&[]);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test listener");
        let address = listener.local_addr().expect("listener address");
        let shutdown = CancellationToken::new();
        let server = tokio::spawn(run_listener(
            listener,
            state,
            shutdown.clone(),
            Duration::from_secs(1),
        ));

        let mut stream = tokio::net::TcpStream::connect(address)
            .await
            .expect("connect");
        let request = format!(
            "POST /mcp HTTP/1.1\r\nhost: 127.0.0.1:{}\r\nauthorization: Bearer synthetic-token\r\ncontent-length: 2\r\n\r\n{{}}",
            address.port()
        );
        stream
            .write_all(request.as_bytes())
            .await
            .expect("write request");
        let mut buffer = vec![0u8; 1024];
        let read = stream.read(&mut buffer).await.expect("read response");
        let response = String::from_utf8_lossy(&buffer[..read]);
        assert!(response.starts_with("HTTP/1.1 200 OK"), "{response}");
        drop(stream);

        shutdown.cancel();
        let result = tokio::time::timeout(Duration::from_secs(5), server)
            .await
            .expect("shutdown deadline")
            .expect("server join");
        assert_eq!(result, Ok(()));
    }
}
