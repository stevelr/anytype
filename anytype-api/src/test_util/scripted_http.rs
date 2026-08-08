//! Finite loopback HTTP scripts for downstream contract tests.
//!
//! The fixture records request method, path, and body bytes in arrival order.
//! Every script and captured field has a fixed ceiling, and diagnostics report
//! only categories and sizes. Callers must explicitly inspect captured payloads.

use std::{fmt, time::Duration};

use reqwest::StatusCode;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    task::JoinHandle,
};

/// Maximum number of responses, and therefore requests, in one script.
pub const MAX_SCRIPTED_HTTP_REQUESTS: usize = 32;
/// Maximum bytes accepted before the end of one request header.
pub const MAX_SCRIPTED_HTTP_HEADER_BYTES: usize = 16 * 1024;
/// Maximum bytes captured for one HTTP method.
pub const MAX_SCRIPTED_HTTP_METHOD_BYTES: usize = 16;
/// Maximum bytes captured for one request target, including its query.
pub const MAX_SCRIPTED_HTTP_PATH_BYTES: usize = 4 * 1024;
/// Maximum bytes captured for one request body.
pub const MAX_SCRIPTED_HTTP_BODY_BYTES: usize = 256 * 1024;
/// Maximum bytes served as one response body.
pub const MAX_SCRIPTED_HTTP_RESPONSE_BYTES: usize = 256 * 1024;

const SCRIPTED_HTTP_IO_TIMEOUT: Duration = Duration::from_secs(5 * 60);

/// Fixed response content types supported by the scripted fixture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScriptedHttpContentType {
    /// `application/json`.
    Json,
    /// `text/plain; charset=utf-8`.
    Text,
}

impl ScriptedHttpContentType {
    fn as_header_value(self) -> &'static str {
        match self {
            Self::Json => "application/json",
            Self::Text => "text/plain; charset=utf-8",
        }
    }
}

/// One bounded response in a finite HTTP script.
#[derive(Clone)]
pub struct ScriptedHttpResponse {
    status: StatusCode,
    content_type: ScriptedHttpContentType,
    body: Vec<u8>,
}

impl ScriptedHttpResponse {
    /// Creates one response. [`ScriptedHttpFixture::start`] rejects bodies over
    /// [`MAX_SCRIPTED_HTTP_RESPONSE_BYTES`] before binding a listener.
    #[must_use]
    pub fn new(
        status: StatusCode,
        content_type: ScriptedHttpContentType,
        body: impl Into<Vec<u8>>,
    ) -> Self {
        Self {
            status,
            content_type,
            body: body.into(),
        }
    }
}

impl fmt::Debug for ScriptedHttpResponse {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ScriptedHttpResponse")
            .field("status", &self.status.as_u16())
            .field("content_type", &self.content_type)
            .field("body_bytes", &self.body.len())
            .finish()
    }
}

/// One request captured by the scripted fixture.
#[derive(Clone, PartialEq, Eq)]
pub struct ScriptedHttpRequest {
    method: String,
    path: String,
    body: Vec<u8>,
}

impl ScriptedHttpRequest {
    /// Returns the exact bounded HTTP method.
    #[must_use]
    pub fn method(&self) -> &str {
        &self.method
    }

    /// Returns the exact bounded request target, including its query.
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Returns the exact bounded request body bytes.
    #[must_use]
    pub fn body(&self) -> &[u8] {
        &self.body
    }
}

impl fmt::Debug for ScriptedHttpRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ScriptedHttpRequest")
            .field("method_bytes", &self.method.len())
            .field("path_bytes", &self.path.len())
            .field("body_bytes", &self.body.len())
            .finish()
    }
}

/// Closed reason that an incoming request could not be captured safely.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScriptedHttpRequestErrorKind {
    /// The header terminator was not received before the peer closed.
    TruncatedHeader,
    /// The request line was not valid bounded HTTP/1.x syntax.
    InvalidRequestLine,
    /// The method was empty, oversized, or contained non-token bytes.
    InvalidMethod,
    /// The request target was empty, oversized, or not valid UTF-8.
    InvalidPath,
    /// A header line was malformed or not valid UTF-8.
    InvalidHeader,
    /// Content length was duplicated, malformed, or inconsistent.
    InvalidContentLength,
    /// Transfer encoding is unsupported because capture requires an exact length.
    TransferEncoding,
    /// The peer closed before the declared body was complete.
    TruncatedBody,
}

/// Payload-free failure from a scripted HTTP fixture.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ScriptedHttpFixtureError {
    /// The script did not contain a response.
    EmptyScript,
    /// The script exceeded its request/response count ceiling.
    ScriptTooLong { count: usize, limit: usize },
    /// A scripted response body exceeded its byte ceiling.
    ResponseTooLarge {
        index: usize,
        bytes: usize,
        limit: usize,
    },
    /// Binding the loopback listener failed.
    Bind,
    /// Reading the listener address failed.
    LocalAddress,
    /// Waiting for the next request exceeded the fixture deadline.
    AcceptTimeout { index: usize },
    /// Accepting a loopback connection failed.
    Accept { index: usize },
    /// Reading a request exceeded the fixture deadline.
    ReadTimeout { index: usize },
    /// Reading a request failed.
    Read { index: usize },
    /// Request headers exceeded their byte ceiling.
    HeaderTooLarge { index: usize, limit: usize },
    /// The declared request body exceeded its byte ceiling.
    BodyTooLarge {
        index: usize,
        bytes: usize,
        limit: usize,
    },
    /// The request could not be parsed within the fixture contract.
    InvalidRequest {
        index: usize,
        kind: ScriptedHttpRequestErrorKind,
    },
    /// Writing a scripted response exceeded the fixture deadline.
    WriteTimeout { index: usize },
    /// Writing a scripted response failed.
    Write { index: usize },
    /// The fixture task ended without a structured result.
    ServerTask,
}

impl fmt::Display for ScriptedHttpFixtureError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyScript => formatter.write_str("scripted HTTP fixture requires a response"),
            Self::ScriptTooLong { count, limit } => write!(
                formatter,
                "scripted HTTP fixture has {count} responses; limit is {limit}"
            ),
            Self::ResponseTooLarge {
                index,
                bytes,
                limit,
            } => write!(
                formatter,
                "scripted HTTP response {index} has {bytes} body bytes; limit is {limit}"
            ),
            Self::Bind => formatter.write_str("scripted HTTP fixture could not bind loopback"),
            Self::LocalAddress => {
                formatter.write_str("scripted HTTP fixture could not read its loopback address")
            }
            Self::AcceptTimeout { index } => {
                write!(
                    formatter,
                    "scripted HTTP request {index} was not accepted in time"
                )
            }
            Self::Accept { index } => {
                write!(
                    formatter,
                    "scripted HTTP request {index} could not be accepted"
                )
            }
            Self::ReadTimeout { index } => {
                write!(
                    formatter,
                    "scripted HTTP request {index} was not read in time"
                )
            }
            Self::Read { index } => {
                write!(formatter, "scripted HTTP request {index} could not be read")
            }
            Self::HeaderTooLarge { index, limit } => write!(
                formatter,
                "scripted HTTP request {index} headers exceed the {limit}-byte limit"
            ),
            Self::BodyTooLarge {
                index,
                bytes,
                limit,
            } => write!(
                formatter,
                "scripted HTTP request {index} declares {bytes} body bytes; limit is {limit}"
            ),
            Self::InvalidRequest { index, kind } => {
                write!(
                    formatter,
                    "scripted HTTP request {index} is invalid: {kind:?}"
                )
            }
            Self::WriteTimeout { index } => {
                write!(
                    formatter,
                    "scripted HTTP response {index} was not written in time"
                )
            }
            Self::Write { index } => {
                write!(
                    formatter,
                    "scripted HTTP response {index} could not be written"
                )
            }
            Self::ServerTask => {
                formatter.write_str("scripted HTTP fixture task ended unexpectedly")
            }
        }
    }
}

impl std::error::Error for ScriptedHttpFixtureError {}

/// Running finite loopback HTTP script.
pub struct ScriptedHttpFixture {
    address: std::net::SocketAddr,
    response_count: usize,
    server: Option<JoinHandle<Result<Vec<ScriptedHttpRequest>, ScriptedHttpFixtureError>>>,
}

impl ScriptedHttpFixture {
    /// Binds a loopback listener and starts serving the supplied finite script.
    ///
    /// # Errors
    ///
    /// Returns a payload-free error when the script violates a ceiling or the
    /// loopback listener cannot be created.
    pub async fn start(
        responses: Vec<ScriptedHttpResponse>,
    ) -> Result<Self, ScriptedHttpFixtureError> {
        validate_responses(&responses)?;
        let response_count = responses.len();
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .map_err(|_| ScriptedHttpFixtureError::Bind)?;
        let address = listener
            .local_addr()
            .map_err(|_| ScriptedHttpFixtureError::LocalAddress)?;
        let server = tokio::spawn(serve_script(listener, responses));
        Ok(Self {
            address,
            response_count,
            server: Some(server),
        })
    }

    /// Returns the loopback socket address used by the fixture.
    #[must_use]
    pub fn address(&self) -> std::net::SocketAddr {
        self.address
    }

    /// Returns the loopback base URL used by the fixture.
    #[must_use]
    pub fn base_url(&self) -> String {
        format!("http://{}", self.address)
    }

    /// Waits for every scripted response and returns requests in arrival order.
    ///
    /// # Errors
    ///
    /// Returns a payload-free transport, parsing, limit, timeout, or task error.
    pub async fn finish(mut self) -> Result<Vec<ScriptedHttpRequest>, ScriptedHttpFixtureError> {
        let Some(server) = self.server.take() else {
            return Err(ScriptedHttpFixtureError::ServerTask);
        };
        server
            .await
            .map_err(|_| ScriptedHttpFixtureError::ServerTask)?
    }
}

impl fmt::Debug for ScriptedHttpFixture {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ScriptedHttpFixture")
            .field("response_count", &self.response_count)
            .field("running", &self.server.is_some())
            .finish()
    }
}

impl Drop for ScriptedHttpFixture {
    fn drop(&mut self) {
        if let Some(server) = self.server.take() {
            server.abort();
        }
    }
}

fn validate_responses(responses: &[ScriptedHttpResponse]) -> Result<(), ScriptedHttpFixtureError> {
    if responses.is_empty() {
        return Err(ScriptedHttpFixtureError::EmptyScript);
    }
    if responses.len() > MAX_SCRIPTED_HTTP_REQUESTS {
        return Err(ScriptedHttpFixtureError::ScriptTooLong {
            count: responses.len(),
            limit: MAX_SCRIPTED_HTTP_REQUESTS,
        });
    }
    for (index, response) in responses.iter().enumerate() {
        if response.body.len() > MAX_SCRIPTED_HTTP_RESPONSE_BYTES {
            return Err(ScriptedHttpFixtureError::ResponseTooLarge {
                index,
                bytes: response.body.len(),
                limit: MAX_SCRIPTED_HTTP_RESPONSE_BYTES,
            });
        }
    }
    Ok(())
}

async fn serve_script(
    listener: TcpListener,
    responses: Vec<ScriptedHttpResponse>,
) -> Result<Vec<ScriptedHttpRequest>, ScriptedHttpFixtureError> {
    let mut requests = Vec::with_capacity(responses.len());
    for (index, response) in responses.into_iter().enumerate() {
        let (mut stream, _) = tokio::time::timeout(SCRIPTED_HTTP_IO_TIMEOUT, listener.accept())
            .await
            .map_err(|_| ScriptedHttpFixtureError::AcceptTimeout { index })?
            .map_err(|_| ScriptedHttpFixtureError::Accept { index })?;
        let request = capture_request(&mut stream, index).await?;
        write_response(&mut stream, index, response).await?;
        requests.push(request);
    }
    Ok(requests)
}

async fn capture_request(
    stream: &mut TcpStream,
    index: usize,
) -> Result<ScriptedHttpRequest, ScriptedHttpFixtureError> {
    let mut request = Vec::new();
    let mut chunk = [0_u8; 1024];
    let header_end = loop {
        if let Some(position) = request.windows(4).position(|window| window == b"\r\n\r\n") {
            break position;
        }
        if request.len() > MAX_SCRIPTED_HTTP_HEADER_BYTES {
            return Err(ScriptedHttpFixtureError::HeaderTooLarge {
                index,
                limit: MAX_SCRIPTED_HTTP_HEADER_BYTES,
            });
        }
        let read = tokio::time::timeout(SCRIPTED_HTTP_IO_TIMEOUT, stream.read(&mut chunk))
            .await
            .map_err(|_| ScriptedHttpFixtureError::ReadTimeout { index })?
            .map_err(|_| ScriptedHttpFixtureError::Read { index })?;
        if read == 0 {
            return Err(ScriptedHttpFixtureError::InvalidRequest {
                index,
                kind: ScriptedHttpRequestErrorKind::TruncatedHeader,
            });
        }
        request.extend_from_slice(&chunk[..read]);
    };

    if header_end > MAX_SCRIPTED_HTTP_HEADER_BYTES {
        return Err(ScriptedHttpFixtureError::HeaderTooLarge {
            index,
            limit: MAX_SCRIPTED_HTTP_HEADER_BYTES,
        });
    }
    let (method, path, content_length) = parse_headers(&request[..header_end], index)?;
    if content_length > MAX_SCRIPTED_HTTP_BODY_BYTES {
        return Err(ScriptedHttpFixtureError::BodyTooLarge {
            index,
            bytes: content_length,
            limit: MAX_SCRIPTED_HTTP_BODY_BYTES,
        });
    }
    let body_start = header_end + 4;
    let expected_len = body_start + content_length;
    while request.len() < expected_len {
        let read = tokio::time::timeout(SCRIPTED_HTTP_IO_TIMEOUT, stream.read(&mut chunk))
            .await
            .map_err(|_| ScriptedHttpFixtureError::ReadTimeout { index })?
            .map_err(|_| ScriptedHttpFixtureError::Read { index })?;
        if read == 0 {
            return Err(ScriptedHttpFixtureError::InvalidRequest {
                index,
                kind: ScriptedHttpRequestErrorKind::TruncatedBody,
            });
        }
        request.extend_from_slice(&chunk[..read]);
    }
    request.truncate(expected_len);
    Ok(ScriptedHttpRequest {
        method,
        path,
        body: request[body_start..].to_vec(),
    })
}

fn parse_headers(
    headers: &[u8],
    index: usize,
) -> Result<(String, String, usize), ScriptedHttpFixtureError> {
    let headers =
        std::str::from_utf8(headers).map_err(|_| ScriptedHttpFixtureError::InvalidRequest {
            index,
            kind: ScriptedHttpRequestErrorKind::InvalidHeader,
        })?;
    let mut lines = headers.split("\r\n");
    let request_line = lines
        .next()
        .ok_or(ScriptedHttpFixtureError::InvalidRequest {
            index,
            kind: ScriptedHttpRequestErrorKind::InvalidRequestLine,
        })?;
    let mut parts = request_line.split(' ');
    let method = parts.next().unwrap_or_default();
    let path = parts.next().unwrap_or_default();
    let version = parts.next().unwrap_or_default();
    if parts.next().is_some() || !matches!(version, "HTTP/1.0" | "HTTP/1.1") {
        return Err(ScriptedHttpFixtureError::InvalidRequest {
            index,
            kind: ScriptedHttpRequestErrorKind::InvalidRequestLine,
        });
    }
    if method.is_empty()
        || method.len() > MAX_SCRIPTED_HTTP_METHOD_BYTES
        || !method.bytes().all(is_http_token_byte)
    {
        return Err(ScriptedHttpFixtureError::InvalidRequest {
            index,
            kind: ScriptedHttpRequestErrorKind::InvalidMethod,
        });
    }
    if path.is_empty() || path.len() > MAX_SCRIPTED_HTTP_PATH_BYTES || !path.starts_with('/') {
        return Err(ScriptedHttpFixtureError::InvalidRequest {
            index,
            kind: ScriptedHttpRequestErrorKind::InvalidPath,
        });
    }

    let mut content_length = None;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            return Err(ScriptedHttpFixtureError::InvalidRequest {
                index,
                kind: ScriptedHttpRequestErrorKind::InvalidHeader,
            });
        };
        if name.eq_ignore_ascii_case("transfer-encoding") {
            return Err(ScriptedHttpFixtureError::InvalidRequest {
                index,
                kind: ScriptedHttpRequestErrorKind::TransferEncoding,
            });
        }
        if name.eq_ignore_ascii_case("content-length") {
            if content_length.is_some() {
                return Err(ScriptedHttpFixtureError::InvalidRequest {
                    index,
                    kind: ScriptedHttpRequestErrorKind::InvalidContentLength,
                });
            }
            content_length = Some(value.trim().parse::<usize>().map_err(|_| {
                ScriptedHttpFixtureError::InvalidRequest {
                    index,
                    kind: ScriptedHttpRequestErrorKind::InvalidContentLength,
                }
            })?);
        }
    }
    Ok((
        method.to_owned(),
        path.to_owned(),
        content_length.unwrap_or(0),
    ))
}

fn is_http_token_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte)
}

async fn write_response(
    stream: &mut TcpStream,
    index: usize,
    response: ScriptedHttpResponse,
) -> Result<(), ScriptedHttpFixtureError> {
    let reason = response.status.canonical_reason().unwrap_or("");
    let head = format!(
        "HTTP/1.1 {} {reason}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        response.status.as_u16(),
        response.content_type.as_header_value(),
        response.body.len()
    );
    tokio::time::timeout(SCRIPTED_HTTP_IO_TIMEOUT, async {
        stream.write_all(head.as_bytes()).await?;
        stream.write_all(&response.body).await?;
        stream.shutdown().await
    })
    .await
    .map_err(|_| ScriptedHttpFixtureError::WriteTimeout { index })?
    .map_err(|_| ScriptedHttpFixtureError::Write { index })
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn send_raw(address: std::net::SocketAddr, request: Vec<u8>) -> Vec<u8> {
        let mut stream = TcpStream::connect(address)
            .await
            .expect("connect scripted fixture");
        stream
            .write_all(&request)
            .await
            .expect("write scripted request");
        let mut response = Vec::new();
        stream
            .read_to_end(&mut response)
            .await
            .expect("read scripted response");
        response
    }

    #[tokio::test]
    async fn preserves_response_and_capture_order() {
        let fixture = ScriptedHttpFixture::start(vec![
            ScriptedHttpResponse::new(
                StatusCode::OK,
                ScriptedHttpContentType::Json,
                br#"{"step":1}"#.to_vec(),
            ),
            ScriptedHttpResponse::new(
                StatusCode::CREATED,
                ScriptedHttpContentType::Text,
                b"second".to_vec(),
            ),
        ])
        .await
        .expect("start scripted fixture");
        let address = fixture.address();
        let first = send_raw(
            address,
            b"POST /first?q=1 HTTP/1.1\r\nHost: local\r\nContent-Length: 3\r\n\r\none".to_vec(),
        )
        .await;
        let second = send_raw(
            address,
            b"GET /second HTTP/1.1\r\nHost: local\r\n\r\n".to_vec(),
        )
        .await;
        assert!(first.starts_with(b"HTTP/1.1 200 OK"));
        assert!(second.starts_with(b"HTTP/1.1 201 Created"));
        let requests = fixture.finish().await.expect("finish scripted fixture");
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].method(), "POST");
        assert_eq!(requests[0].path(), "/first?q=1");
        assert_eq!(requests[0].body(), b"one");
        assert_eq!(requests[1].method(), "GET");
        assert_eq!(requests[1].path(), "/second");
        assert!(requests[1].body().is_empty());
    }

    #[tokio::test]
    async fn rejects_script_and_response_ceilings_before_bind() {
        let too_many = vec![
            ScriptedHttpResponse::new(
                StatusCode::OK,
                ScriptedHttpContentType::Text,
                Vec::new(),
            );
            MAX_SCRIPTED_HTTP_REQUESTS + 1
        ];
        assert!(matches!(
            ScriptedHttpFixture::start(too_many).await,
            Err(ScriptedHttpFixtureError::ScriptTooLong { .. })
        ));
        let too_large = ScriptedHttpResponse::new(
            StatusCode::OK,
            ScriptedHttpContentType::Text,
            vec![0; MAX_SCRIPTED_HTTP_RESPONSE_BYTES + 1],
        );
        assert!(matches!(
            ScriptedHttpFixture::start(vec![too_large]).await,
            Err(ScriptedHttpFixtureError::ResponseTooLarge { .. })
        ));
    }

    #[tokio::test]
    async fn rejects_request_capture_ceilings() {
        let fixture = ScriptedHttpFixture::start(vec![ScriptedHttpResponse::new(
            StatusCode::OK,
            ScriptedHttpContentType::Text,
            Vec::new(),
        )])
        .await
        .expect("start body-ceiling fixture");
        let request = format!(
            "POST /body HTTP/1.1\r\nHost: local\r\nContent-Length: {}\r\n\r\n",
            MAX_SCRIPTED_HTTP_BODY_BYTES + 1
        );
        let _ = send_raw(fixture.address(), request.into_bytes()).await;
        assert!(matches!(
            fixture.finish().await,
            Err(ScriptedHttpFixtureError::BodyTooLarge { .. })
        ));

        let fixture = ScriptedHttpFixture::start(vec![ScriptedHttpResponse::new(
            StatusCode::OK,
            ScriptedHttpContentType::Text,
            Vec::new(),
        )])
        .await
        .expect("start method-ceiling fixture");
        let request = format!(
            "{} /method HTTP/1.1\r\nHost: local\r\n\r\n",
            "M".repeat(MAX_SCRIPTED_HTTP_METHOD_BYTES + 1)
        );
        let _ = send_raw(fixture.address(), request.into_bytes()).await;
        assert!(matches!(
            fixture.finish().await,
            Err(ScriptedHttpFixtureError::InvalidRequest {
                kind: ScriptedHttpRequestErrorKind::InvalidMethod,
                ..
            })
        ));

        let fixture = ScriptedHttpFixture::start(vec![ScriptedHttpResponse::new(
            StatusCode::OK,
            ScriptedHttpContentType::Text,
            Vec::new(),
        )])
        .await
        .expect("start path-ceiling fixture");
        let request = format!(
            "GET /{} HTTP/1.1\r\nHost: local\r\n\r\n",
            "x".repeat(MAX_SCRIPTED_HTTP_PATH_BYTES)
        );
        let _ = send_raw(fixture.address(), request.into_bytes()).await;
        assert!(matches!(
            fixture.finish().await,
            Err(ScriptedHttpFixtureError::InvalidRequest {
                kind: ScriptedHttpRequestErrorKind::InvalidPath,
                ..
            })
        ));

        let fixture = ScriptedHttpFixture::start(vec![ScriptedHttpResponse::new(
            StatusCode::OK,
            ScriptedHttpContentType::Text,
            Vec::new(),
        )])
        .await
        .expect("start header-ceiling fixture");
        let request = format!(
            "GET /header HTTP/1.1\r\nX-Fill: {}\r\n\r\n",
            "x".repeat(MAX_SCRIPTED_HTTP_HEADER_BYTES)
        );
        let _ = send_raw(fixture.address(), request.into_bytes()).await;
        assert!(matches!(
            fixture.finish().await,
            Err(ScriptedHttpFixtureError::HeaderTooLarge { .. })
        ));
    }

    #[tokio::test]
    async fn debug_and_error_output_exclude_payloads() {
        let secret = b"fixture-secret-value";
        let response = ScriptedHttpResponse::new(
            StatusCode::OK,
            ScriptedHttpContentType::Text,
            secret.to_vec(),
        );
        assert!(!format!("{response:?}").contains("fixture-secret-value"));
        let request = ScriptedHttpRequest {
            method: "POST".to_owned(),
            path: "/secret?token=fixture-secret-value".to_owned(),
            body: secret.to_vec(),
        };
        assert!(!format!("{request:?}").contains("fixture-secret-value"));
        let error = ScriptedHttpFixtureError::InvalidRequest {
            index: 0,
            kind: ScriptedHttpRequestErrorKind::InvalidHeader,
        };
        assert!(!format!("{error:?} {error}").contains("fixture-secret-value"));
    }
}
