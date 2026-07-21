// any-mcp - bounded, workflow-oriented MCP server for Anytype
//
// SPDX-FileCopyrightText: 2026 Steve Schoettler
// SPDX-License-Identifier: Apache-2.0

//! Explicitly selected stdio framing for stable and experimental MCP clients.

use std::{collections::HashMap, io, sync::Arc};

use rmcp::{
    RoleServer,
    model::{
        CallToolRequestParams, ErrorData, InitializeRequestParams, PaginatedRequestParams,
        ReadResourceRequestParams,
    },
    service::{RxJsonRpcMessage, TxJsonRpcMessage},
    transport::Transport,
};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use tokio::{
    io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader},
    sync::{Mutex, Semaphore, mpsc},
    task::JoinSet,
};
use tokio_util::sync::CancellationToken;

use crate::{
    config::ProtocolMode,
    runtime::{ServeError, serve_transport},
    server::AnyMcpServer,
};

const MODERN_VERSION: &str = "2026-07-28";
const MAX_FRAME_BYTES: usize = 2 * 1024 * 1024;
const MAX_IN_FLIGHT_REQUESTS: usize = 64;
const CACHE_CATALOG_TTL_MS: u64 = 3_600_000;
const CACHE_PRIVATE_TTL_MS: u64 = 0;

const META_PROTOCOL_VERSION: &str = "io.modelcontextprotocol/protocolVersion";
const META_CLIENT_INFO: &str = "io.modelcontextprotocol/clientInfo";
const META_CLIENT_CAPABILITIES: &str = "io.modelcontextprotocol/clientCapabilities";
const META_SERVER_INFO: &str = "io.modelcontextprotocol/serverInfo";

type CancellationMap = Arc<Mutex<HashMap<String, CancellationToken>>>;

/// Serves one stdio process in its configured protocol mode.
///
/// Input bytes never select the experimental adapter. Stable mode validates
/// the pre-initialize stream and replays the accepted initialize frame
/// byte-for-byte to rmcp. Preview mode is reachable only through explicit
/// process configuration.
pub(crate) async fn serve_stdio(
    server: AnyMcpServer,
    protocol_mode: ProtocolMode,
) -> Result<(), ServeError> {
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();
    let reader = BufReader::new(stdin);
    match protocol_mode {
        ProtocolMode::Stable => serve_stable(server, reader, stdout).await,
        ProtocolMode::Experimental20260728 => serve_preview(server, reader, stdout).await,
    }
}

async fn serve_stable<R, W>(
    server: AnyMcpServer,
    mut reader: R,
    mut writer: W,
) -> Result<(), ServeError>
where
    R: AsyncBufRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    loop {
        let frame = match read_frame(&mut reader).await {
            Ok(Some(frame)) if frame.iter().all(u8::is_ascii_whitespace) => continue,
            Ok(Some(frame)) => frame,
            Ok(None) => {
                server.runtime().begin_shutdown();
                return Ok(());
            }
            Err(FrameReadError::TooLarge) => {
                write_gate_response(&mut writer, &invalid_request(Value::Null)).await?;
                continue;
            }
            Err(FrameReadError::Io) => return Err(ServeError::StdioTransport),
        };

        let frame_without_bom = frame.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(&frame);
        let value = match serde_json::from_slice::<Value>(frame_without_bom) {
            Ok(value) => value,
            Err(_) => {
                write_gate_response(&mut writer, &parse_error()).await?;
                continue;
            }
        };
        if is_stable_initialize(&value) {
            return serve_transport(server, LegacyStdioTransport::new(frame, reader, writer)).await;
        }
        if !is_jsonrpc_notification(&value) {
            write_gate_response(&mut writer, &invalid_request(Value::Null)).await?;
        }
    }
}

async fn write_gate_response<W>(writer: &mut W, response: &Value) -> Result<(), ServeError>
where
    W: AsyncWrite + Unpin,
{
    let encoded = encode_bounded_legacy_frame(response).map_err(|_| ServeError::StdioTransport)?;
    writer
        .write_all(&encoded)
        .await
        .map_err(|_| ServeError::StdioTransport)?;
    writer
        .write_all(b"\n")
        .await
        .map_err(|_| ServeError::StdioTransport)?;
    writer.flush().await.map_err(|_| ServeError::StdioTransport)
}

async fn serve_preview<R, W>(
    server: AnyMcpServer,
    mut reader: R,
    writer: W,
) -> Result<(), ServeError>
where
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let first = loop {
        match read_frame(&mut reader).await {
            Ok(Some(frame)) if frame.iter().all(u8::is_ascii_whitespace) => continue,
            Ok(frame) => break frame,
            Err(FrameReadError::TooLarge) => {
                return serve_modern(server, reader, writer, FirstFrame::TooLarge).await;
            }
            Err(FrameReadError::Io) => return Err(ServeError::StdioTransport),
        }
    };
    let Some(first) = first else {
        server.runtime().begin_shutdown();
        return Ok(());
    };

    serve_modern(server, reader, writer, FirstFrame::Bytes(first)).await
}

/// Bounded legacy line transport that preserves rmcp dispatch and lifecycle.
///
/// The decoder and rmcp response path share one writer lock. This ensures a
/// malformed frame's JSON-RPC error cannot interleave with a normal response,
/// while keeping all service dispatch inside rmcp.
struct LegacyStdioTransport<R, W> {
    first: Option<Vec<u8>>,
    reader: R,
    line: Vec<u8>,
    draining_oversize: bool,
    pending_decoder_frame: Option<Vec<u8>>,
    outbound: Option<mpsc::Sender<Vec<u8>>>,
    writer_task: Option<tokio::task::JoinHandle<io::Result<()>>>,
    writer: std::marker::PhantomData<fn() -> W>,
}

impl<R, W> LegacyStdioTransport<R, W>
where
    W: AsyncWrite + Unpin + Send + 'static,
{
    fn new(first: Vec<u8>, reader: R, writer: W) -> Self {
        let (outbound, receiver) = mpsc::channel(MAX_IN_FLIGHT_REQUESTS);
        Self {
            first: Some(first),
            reader,
            line: Vec::new(),
            draining_oversize: false,
            pending_decoder_frame: None,
            outbound: Some(outbound),
            writer_task: Some(tokio::spawn(write_legacy_responses(writer, receiver))),
            writer: std::marker::PhantomData,
        }
    }
}

impl<R, W> LegacyStdioTransport<R, W>
where
    R: AsyncBufRead + Unpin,
{
    async fn queue_decoder_message(&mut self, item: Value) -> io::Result<()> {
        debug_assert!(self.pending_decoder_frame.is_none());
        self.pending_decoder_frame = Some(encode_bounded_legacy_frame(&item)?);
        self.flush_pending_decoder_frame().await
    }

    async fn flush_pending_decoder_frame(&mut self) -> io::Result<()> {
        let outbound = self
            .outbound
            .as_ref()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotConnected, "transport is closed"))?
            .clone();
        let permit = outbound.reserve_owned().await.map_err(|_| {
            io::Error::new(io::ErrorKind::BrokenPipe, "stdout writer is unavailable")
        })?;
        let encoded = self
            .pending_decoder_frame
            .take()
            .expect("pending decoder frame survives cancellation");
        permit.send(encoded);
        Ok(())
    }

    /// Reads one bounded frame without losing partially read bytes when rmcp
    /// cancels a pending receive future to send an outgoing response.
    async fn receive_frame(&mut self) -> Result<Option<Vec<u8>>, FrameReadError> {
        if let Some(first) = self.first.take() {
            return Ok(Some(first));
        }

        loop {
            let available = self
                .reader
                .fill_buf()
                .await
                .map_err(|_| FrameReadError::Io)?;
            if available.is_empty() {
                if self.draining_oversize {
                    self.draining_oversize = false;
                    return Err(FrameReadError::TooLarge);
                }
                self.line.clear();
                return Ok(None);
            }

            let newline = available.iter().position(|byte| *byte == b'\n');
            let take = newline.map_or(available.len(), |position| position + 1);
            if self.draining_oversize {
                self.reader.consume(take);
                if newline.is_some() {
                    self.draining_oversize = false;
                    return Err(FrameReadError::TooLarge);
                }
                continue;
            }

            if self.line.len().saturating_add(take) > MAX_FRAME_BYTES {
                self.line.clear();
                self.reader.consume(take);
                if newline.is_some() {
                    return Err(FrameReadError::TooLarge);
                }
                self.draining_oversize = true;
                continue;
            }

            self.line.extend_from_slice(&available[..take]);
            self.reader.consume(take);
            if newline.is_some() {
                self.line.pop();
                if self.line.last() == Some(&b'\r') {
                    self.line.pop();
                }
                return Ok(Some(std::mem::take(&mut self.line)));
            }
        }
    }
}

impl<R, W> Transport<RoleServer> for LegacyStdioTransport<R, W>
where
    R: AsyncBufRead + Unpin + Send,
    W: AsyncWrite + Unpin + Send + 'static,
{
    type Error = io::Error;

    fn send(
        &mut self,
        item: TxJsonRpcMessage<RoleServer>,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send + 'static {
        let outbound = self.outbound.clone();
        async move {
            let encoded = encode_legacy_message(item)?;
            outbound
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotConnected, "transport is closed"))?
                .send(encoded)
                .await
                .map_err(|_| {
                    io::Error::new(io::ErrorKind::BrokenPipe, "stdout writer is unavailable")
                })
        }
    }

    async fn receive(&mut self) -> Option<RxJsonRpcMessage<RoleServer>> {
        if self.pending_decoder_frame.is_some() {
            self.flush_pending_decoder_frame().await.ok()?;
        }
        loop {
            let frame = match self.receive_frame().await {
                Ok(Some(frame)) => frame,
                Ok(None) | Err(FrameReadError::Io) => return None,
                Err(FrameReadError::TooLarge) => {
                    self.queue_decoder_message(invalid_request(Value::Null))
                        .await
                        .ok()?;
                    continue;
                }
            };
            if frame.iter().all(u8::is_ascii_whitespace) {
                continue;
            }

            let frame = frame.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(&frame);
            let value = match serde_json::from_slice::<Value>(frame) {
                Ok(value) => value,
                Err(_) => {
                    self.queue_decoder_message(parse_error()).await.ok()?;
                    continue;
                }
            };
            let is_notification = is_jsonrpc_notification(&value);
            match serde_json::from_value(value) {
                Ok(message) => return Some(message),
                Err(_) => {
                    if is_notification {
                        continue;
                    }
                    self.queue_decoder_message(invalid_request(Value::Null))
                        .await
                        .ok()?;
                }
            }
        }
    }

    fn close(&mut self) -> impl Future<Output = Result<(), Self::Error>> + Send {
        self.outbound.take();
        let writer_task = self.writer_task.take();
        async move {
            if let Some(writer_task) = writer_task {
                writer_task
                    .await
                    .map_err(|_| io::Error::other("stdout writer task failed"))??;
            }
            Ok(())
        }
    }
}

fn encode_legacy_message(item: TxJsonRpcMessage<RoleServer>) -> io::Result<Vec<u8>> {
    encode_bounded_legacy_frame(&item)
}

fn encode_bounded_legacy_frame(item: &impl serde::Serialize) -> io::Result<Vec<u8>> {
    let encoded = serde_json::to_vec(item)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "JSON-RPC encoding failed"))?;
    if encoded.len().saturating_add(1) > MAX_FRAME_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "JSON-RPC frame exceeds byte cap",
        ));
    }
    Ok(encoded)
}

async fn write_legacy_responses<W>(
    mut writer: W,
    mut outbound: mpsc::Receiver<Vec<u8>>,
) -> io::Result<()>
where
    W: AsyncWrite + Unpin,
{
    while let Some(encoded) = outbound.recv().await {
        writer.write_all(&encoded).await?;
        writer.write_all(b"\n").await?;
        writer.flush().await?;
    }
    writer.shutdown().await
}

fn is_jsonrpc_notification(value: &Value) -> bool {
    value.as_object().is_some_and(|object| {
        !object.contains_key("id")
            && object.get("jsonrpc").and_then(Value::as_str) == Some("2.0")
            && object.get("method").and_then(Value::as_str).is_some()
    })
}

enum FirstFrame {
    Bytes(Vec<u8>),
    TooLarge,
}

fn is_stable_initialize(value: &Value) -> bool {
    value.as_object().is_some_and(|object| {
        object.get("jsonrpc").and_then(Value::as_str) == Some("2.0")
            && object.get("method").and_then(Value::as_str) == Some("initialize")
            && valid_id(object.get("id")).is_some()
            && object.get("params").cloned().is_some_and(|params| {
                serde_json::from_value::<InitializeRequestParams>(params)
                    .is_ok_and(|params| params.protocol_version.as_str() != MODERN_VERSION)
            })
    })
}

async fn serve_modern<R, W>(
    server: AnyMcpServer,
    mut reader: R,
    writer: W,
    first: FirstFrame,
) -> Result<(), ServeError>
where
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let runtime = server.runtime().clone();
    let cancellations: CancellationMap = Arc::new(Mutex::new(HashMap::new()));
    let permits = Arc::new(Semaphore::new(MAX_IN_FLIGHT_REQUESTS));
    let (responses, response_rx) = mpsc::channel(MAX_IN_FLIGHT_REQUESTS);
    let mut writer_task = tokio::spawn(write_responses(writer, response_rx));
    let mut requests = JoinSet::new();

    match first {
        FirstFrame::Bytes(frame) => {
            handle_frame(
                &server,
                &cancellations,
                &permits,
                &responses,
                &mut requests,
                &frame,
            )
            .await;
        }
        FirstFrame::TooLarge => {
            let _ = responses.send(invalid_request(Value::Null)).await;
        }
    }

    loop {
        tokio::select! {
            biased;
            writer_result = &mut writer_task => {
                runtime.begin_shutdown();
                cancel_all(&cancellations).await;
                requests.abort_all();
                return match writer_result {
                    Ok(Ok(())) => Err(ServeError::StdioTransport),
                    Ok(Err(())) | Err(_) => Err(ServeError::StdioTransport),
                };
            }
            completed = requests.join_next(), if !requests.is_empty() => {
                if completed.is_some_and(|result| result.is_err()) {
                    runtime.begin_shutdown();
                    cancel_all(&cancellations).await;
                    requests.abort_all();
                    drop(responses);
                    let _ = writer_task.await;
                    return Err(ServeError::ServiceTask);
                }
            }
            frame = read_frame(&mut reader) => {
                match frame {
                    Ok(Some(frame)) if frame.iter().all(u8::is_ascii_whitespace) => {}
                    Ok(Some(frame)) => {
                        handle_frame(
                            &server,
                            &cancellations,
                            &permits,
                            &responses,
                            &mut requests,
                            &frame,
                        ).await;
                    }
                    Ok(None) => break,
                    Err(FrameReadError::TooLarge) => {
                        if responses.send(invalid_request(Value::Null)).await.is_err() {
                            break;
                        }
                    }
                    Err(FrameReadError::Io) => {
                        runtime.begin_shutdown();
                        cancel_all(&cancellations).await;
                        requests.abort_all();
                        return Err(ServeError::StdioTransport);
                    }
                }
            }
        }
    }

    runtime.begin_shutdown();
    cancel_all(&cancellations).await;
    let mut task_failed = false;
    while let Some(completed) = requests.join_next().await {
        task_failed |= completed.is_err();
    }
    drop(responses);
    match (task_failed, writer_task.await) {
        (false, Ok(Ok(()))) => Ok(()),
        (true, _) => Err(ServeError::ServiceTask),
        (false, Ok(Err(()))) | (false, Err(_)) => Err(ServeError::StdioTransport),
    }
}

async fn handle_frame(
    server: &AnyMcpServer,
    cancellations: &CancellationMap,
    permits: &Arc<Semaphore>,
    responses: &mpsc::Sender<Value>,
    requests: &mut JoinSet<()>,
    frame: &[u8],
) {
    let value = match serde_json::from_slice::<Value>(frame) {
        Ok(value) => value,
        Err(_) => {
            let _ = responses.send(parse_error()).await;
            return;
        }
    };
    let Some(object) = value.as_object() else {
        let _ = responses.send(invalid_request(Value::Null)).await;
        return;
    };
    if object.get("jsonrpc") != Some(&Value::String("2.0".to_owned())) {
        let id = valid_id(object.get("id")).unwrap_or(Value::Null);
        let _ = responses.send(invalid_request(id)).await;
        return;
    }
    let Some(method) = object.get("method").and_then(Value::as_str) else {
        let id = valid_id(object.get("id")).unwrap_or(Value::Null);
        let _ = responses.send(invalid_request(id)).await;
        return;
    };

    let Some(id) = object.get("id") else {
        if method == "notifications/cancelled" {
            handle_cancellation(object.get("params"), cancellations).await;
        }
        return;
    };
    let Some(id) = valid_id(Some(id)) else {
        let _ = responses.send(invalid_request(Value::Null)).await;
        return;
    };
    let request_key = id.to_string();
    let Some(params) = object.get("params").and_then(Value::as_object).cloned() else {
        let _ = responses.send(invalid_params(id)).await;
        return;
    };
    if let Err(error) = validate_meta(&params) {
        let _ = responses.send(error.into_response(id)).await;
        return;
    }

    let permit = match permits.clone().try_acquire_owned() {
        Ok(permit) => permit,
        Err(_) => {
            let _ = responses.send(internal_error(id)).await;
            return;
        }
    };
    let cancellation = CancellationToken::new();
    {
        let mut active = cancellations.lock().await;
        if active.contains_key(&request_key) {
            drop(active);
            let _ = responses.send(invalid_request(id)).await;
            return;
        }
        active.insert(request_key.clone(), cancellation.clone());
    }

    let server = server.clone();
    let cancellations = cancellations.clone();
    let responses = responses.clone();
    let method = method.to_owned();
    requests.spawn(async move {
        let _permit = permit;
        let response = tokio::select! {
            biased;
            () = cancellation.cancelled() => None,
            response = dispatch_modern(&server, id.clone(), &method, params, &cancellation) => {
                Some(response)
            }
        };
        cancellations.lock().await.remove(&request_key);
        if let Some(response) = response {
            let _ = responses.send(response).await;
        }
    });
}

async fn dispatch_modern(
    server: &AnyMcpServer,
    id: Value,
    method: &str,
    mut params: Map<String, Value>,
    cancellation: &CancellationToken,
) -> Value {
    params.remove("_meta");
    let result = match method {
        "server/discover" => {
            if !params.is_empty() {
                return invalid_params(id);
            }
            Ok(json!({
                "resultType": "complete",
                "supportedVersions": [MODERN_VERSION],
                "capabilities": {"tools": {}, "resources": {}},
                "_meta": {
                    META_SERVER_INFO: {
                        "name": env!("CARGO_PKG_NAME"),
                        "version": env!("CARGO_PKG_VERSION")
                    }
                },
                "instructions": "Bounded, workflow-oriented access to Anytype",
                "ttlMs": CACHE_CATALOG_TTL_MS,
                "cacheScope": "public"
            }))
        }
        "tools/list" => decode::<ListParams>(params)
            .and_then(|params| server.list_tools_wire(params.into_rmcp()))
            .and_then(encode_result)
            .map(|mut result| {
                add_complete(&mut result);
                add_cache(&mut result, CACHE_CATALOG_TTL_MS, "public");
                result
            }),
        "tools/call" => match decode::<ToolCallParams>(params) {
            Ok(params) if params.request_state.is_none() && params.input_responses.is_none() => {
                server
                    .dispatch_tool(params.into_rmcp(), cancellation)
                    .await
                    .and_then(encode_result)
                    .map(|mut result| {
                        add_complete(&mut result);
                        result
                    })
            }
            Ok(_) | Err(_) => Err(validation_error()),
        },
        "resources/list" => decode::<ListParams>(params)
            .and_then(|params| server.list_resources_wire(params.into_rmcp()))
            .and_then(encode_result)
            .map(|mut result| {
                add_complete(&mut result);
                add_cache(&mut result, CACHE_CATALOG_TTL_MS, "public");
                result
            }),
        "resources/templates/list" => decode::<ListParams>(params)
            .and_then(|params| server.list_resource_templates_wire(params.into_rmcp()))
            .and_then(encode_result)
            .map(|mut result| {
                add_complete(&mut result);
                add_cache(&mut result, CACHE_CATALOG_TTL_MS, "public");
                result
            }),
        "resources/read" => match decode::<ResourceReadParams>(params) {
            Ok(params) => server
                .read_resource_wire(params.into_rmcp(), cancellation)
                .await
                .and_then(encode_result)
                .map(|mut result| {
                    add_complete(&mut result);
                    add_cache(&mut result, CACHE_PRIVATE_TTL_MS, "private");
                    result
                }),
            Err(error) => Err(error),
        },
        _ => return method_not_found(id),
    };

    match result {
        Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
        Err(error) => error_response(id, error),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ListParams {
    cursor: Option<String>,
}

impl ListParams {
    fn into_rmcp(self) -> Option<PaginatedRequestParams> {
        Some(PaginatedRequestParams::default().with_cursor(self.cursor))
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ToolCallParams {
    name: String,
    arguments: Option<Map<String, Value>>,
    input_responses: Option<Map<String, Value>>,
    request_state: Option<String>,
}

impl ToolCallParams {
    fn into_rmcp(self) -> CallToolRequestParams {
        let request = CallToolRequestParams::new(self.name);
        if let Some(arguments) = self.arguments {
            request.with_arguments(arguments)
        } else {
            request
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ResourceReadParams {
    uri: String,
}

impl ResourceReadParams {
    fn into_rmcp(self) -> ReadResourceRequestParams {
        ReadResourceRequestParams::new(self.uri)
    }
}

fn decode<T: for<'de> Deserialize<'de>>(params: Map<String, Value>) -> Result<T, ErrorData> {
    serde_json::from_value(Value::Object(params)).map_err(|_| validation_error())
}

fn encode_result<T: serde::Serialize>(result: T) -> Result<Value, ErrorData> {
    serde_json::to_value(result).map_err(|_| ErrorData::internal_error("Internal error", None))
}

fn add_complete(result: &mut Value) {
    if let Some(result) = result.as_object_mut() {
        result.insert(
            "resultType".to_owned(),
            Value::String("complete".to_owned()),
        );
    }
}

fn add_cache(result: &mut Value, ttl_ms: u64, scope: &str) {
    if let Some(result) = result.as_object_mut() {
        result.insert("ttlMs".to_owned(), Value::from(ttl_ms));
        result.insert("cacheScope".to_owned(), Value::String(scope.to_owned()));
    }
}

enum MetaError {
    Invalid,
    Unsupported(String),
}

impl MetaError {
    fn into_response(self, id: Value) -> Value {
        match self {
            Self::Invalid => invalid_params(id),
            Self::Unsupported(requested) => json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": {
                    "code": -32022,
                    "message": "Unsupported protocol version",
                    "data": {"supported": [MODERN_VERSION], "requested": requested}
                }
            }),
        }
    }
}

fn validate_meta(params: &Map<String, Value>) -> Result<(), MetaError> {
    let meta = params
        .get("_meta")
        .and_then(Value::as_object)
        .ok_or(MetaError::Invalid)?;
    let version = meta
        .get(META_PROTOCOL_VERSION)
        .and_then(Value::as_str)
        .filter(|version| version.len() <= 64)
        .ok_or(MetaError::Invalid)?;
    if version != MODERN_VERSION {
        return Err(MetaError::Unsupported(version.to_owned()));
    }
    if let Some(client_info) = meta.get(META_CLIENT_INFO) {
        validate_client_info(client_info.as_object().ok_or(MetaError::Invalid)?)?;
    }
    let client_capabilities = meta
        .get(META_CLIENT_CAPABILITIES)
        .and_then(Value::as_object)
        .ok_or(MetaError::Invalid)?;
    validate_client_capabilities(client_capabilities)?;
    if let Some(level) = meta.get("io.modelcontextprotocol/logLevel")
        && !matches!(
            level.as_str(),
            Some(
                "debug"
                    | "info"
                    | "notice"
                    | "warning"
                    | "error"
                    | "critical"
                    | "alert"
                    | "emergency"
            )
        )
    {
        return Err(MetaError::Invalid);
    }
    if let Some(progress) = meta.get("progressToken") {
        let valid = match progress {
            Value::String(_) => true,
            Value::Number(number) => number.is_i64() || number.is_u64(),
            _ => false,
        };
        if !valid {
            return Err(MetaError::Invalid);
        }
    }
    Ok(())
}

fn validate_client_info(client_info: &Map<String, Value>) -> Result<(), MetaError> {
    for field in ["name", "version"] {
        client_info
            .get(field)
            .and_then(Value::as_str)
            .filter(|value| value.len() <= 256)
            .ok_or(MetaError::Invalid)?;
    }
    for field in ["title", "description", "websiteUrl"] {
        if let Some(value) = client_info.get(field)
            && !value.as_str().is_some_and(|value| value.len() <= 4_096)
        {
            return Err(MetaError::Invalid);
        }
    }
    if let Some(icons) = client_info.get("icons") {
        let icons = icons.as_array().ok_or(MetaError::Invalid)?;
        if icons.len() > 16 {
            return Err(MetaError::Invalid);
        }
        for icon in icons {
            let icon = icon.as_object().ok_or(MetaError::Invalid)?;
            if !icon
                .get("src")
                .and_then(Value::as_str)
                .is_some_and(|src| !src.is_empty() && src.len() <= 4_096)
            {
                return Err(MetaError::Invalid);
            }
            if let Some(mime_type) = icon.get("mimeType")
                && !mime_type.as_str().is_some_and(|value| value.len() <= 256)
            {
                return Err(MetaError::Invalid);
            }
            if let Some(sizes) = icon.get("sizes") {
                let sizes = sizes.as_array().ok_or(MetaError::Invalid)?;
                if sizes.len() > 32
                    || sizes
                        .iter()
                        .any(|size| !size.as_str().is_some_and(|value| value.len() <= 64))
                {
                    return Err(MetaError::Invalid);
                }
            }
            if let Some(theme) = icon.get("theme")
                && !matches!(theme.as_str(), Some("light" | "dark"))
            {
                return Err(MetaError::Invalid);
            }
        }
    }
    Ok(())
}

fn validate_client_capabilities(capabilities: &Map<String, Value>) -> Result<(), MetaError> {
    for field in [
        "sampling",
        "roots",
        "elicitation",
        "experimental",
        "extensions",
    ] {
        if let Some(value) = capabilities.get(field)
            && !value.is_object()
        {
            return Err(MetaError::Invalid);
        }
    }
    for field in ["experimental", "extensions"] {
        if let Some(entries) = capabilities.get(field).and_then(Value::as_object)
            && entries.values().any(|value| !value.is_object())
        {
            return Err(MetaError::Invalid);
        }
    }
    if let Some(roots) = capabilities.get("roots").and_then(Value::as_object)
        && let Some(list_changed) = roots.get("listChanged")
        && !list_changed.is_boolean()
    {
        return Err(MetaError::Invalid);
    }
    if let Some(elicitation) = capabilities.get("elicitation").and_then(Value::as_object) {
        for field in ["form", "url"] {
            if let Some(value) = elicitation.get(field)
                && !value.is_object()
            {
                return Err(MetaError::Invalid);
            }
        }
    }
    Ok(())
}

async fn handle_cancellation(params: Option<&Value>, cancellations: &CancellationMap) {
    let Some(request_id) = params
        .and_then(Value::as_object)
        .and_then(|params| params.get("requestId"))
        .and_then(|id| valid_id(Some(id)))
    else {
        return;
    };
    if let Some(cancellation) = cancellations.lock().await.get(&request_id.to_string()) {
        cancellation.cancel();
    }
}

async fn cancel_all(cancellations: &CancellationMap) {
    let mut active = cancellations.lock().await;
    for cancellation in active.values() {
        cancellation.cancel();
    }
    active.clear();
}

fn valid_id(id: Option<&Value>) -> Option<Value> {
    match id? {
        Value::String(id) if id.len() <= 256 => Some(Value::String(id.clone())),
        Value::Number(id) if id.is_i64() || id.is_u64() => Some(Value::Number(id.clone())),
        _ => None,
    }
}

fn validation_error() -> ErrorData {
    ErrorData::invalid_params("Invalid params", None)
}

fn error_response(id: Value, error: ErrorData) -> Value {
    let error = serde_json::to_value(error)
        .unwrap_or_else(|_| json!({"code": -32603, "message": "Internal error"}));
    json!({"jsonrpc": "2.0", "id": id, "error": error})
}

fn parse_error() -> Value {
    json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32700, "message": "Parse error"}})
}

fn invalid_request(id: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32600, "message": "Invalid request"}})
}

fn invalid_params(id: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32602, "message": "Invalid params"}})
}

fn method_not_found(id: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": "Method not found"}})
}

fn internal_error(id: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32603, "message": "Internal error"}})
}

async fn write_responses<W>(mut writer: W, mut responses: mpsc::Receiver<Value>) -> Result<(), ()>
where
    W: AsyncWrite + Unpin,
{
    while let Some(response) = responses.recv().await {
        let encoded = serde_json::to_vec(&response).map_err(|_| ())?;
        if encoded.len().saturating_add(1) > MAX_FRAME_BYTES {
            return Err(());
        }
        writer.write_all(&encoded).await.map_err(|_| ())?;
        writer.write_all(b"\n").await.map_err(|_| ())?;
        writer.flush().await.map_err(|_| ())?;
    }
    Ok(())
}

#[derive(Debug)]
enum FrameReadError {
    Io,
    TooLarge,
}

async fn read_frame<R>(reader: &mut R) -> Result<Option<Vec<u8>>, FrameReadError>
where
    R: AsyncBufRead + Unpin,
{
    let mut frame = Vec::new();
    loop {
        let available = reader.fill_buf().await.map_err(|_| FrameReadError::Io)?;
        if available.is_empty() {
            return Ok(None);
        }
        let newline = available.iter().position(|byte| *byte == b'\n');
        let take = newline.map_or(available.len(), |position| position + 1);
        if frame.len().saturating_add(take) > MAX_FRAME_BYTES {
            reader.consume(take);
            if newline.is_none() {
                drain_frame(reader).await?;
            }
            return Err(FrameReadError::TooLarge);
        }
        frame.extend_from_slice(&available[..take]);
        reader.consume(take);
        if newline.is_some() {
            frame.pop();
            if frame.last() == Some(&b'\r') {
                frame.pop();
            }
            return Ok(Some(frame));
        }
    }
}

async fn drain_frame<R>(reader: &mut R) -> Result<(), FrameReadError>
where
    R: AsyncBufRead + Unpin,
{
    loop {
        let available = reader.fill_buf().await.map_err(|_| FrameReadError::Io)?;
        if available.is_empty() {
            return Ok(());
        }
        let newline = available.iter().position(|byte| *byte == b'\n');
        let take = newline.map_or(available.len(), |position| position + 1);
        reader.consume(take);
        if newline.is_some() {
            return Ok(());
        }
    }
}

#[cfg(test)]
mod tests {
    use tokio::io::{AsyncWriteExt, BufReader, duplex};

    use super::*;

    fn meta(version: &str) -> Map<String, Value> {
        json!({
            "_meta": {
                META_PROTOCOL_VERSION: version,
                META_CLIENT_INFO: {"name": "test-client", "version": "1.0.0"},
                META_CLIENT_CAPABILITIES: {}
            }
        })
        .as_object()
        .unwrap()
        .clone()
    }

    #[test]
    fn stable_gate_accepts_only_valid_initialize_requests() {
        assert!(is_stable_initialize(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": {"name": "test-client", "version": "1.0.0"}
            }
        })));
        assert!(!is_stable_initialize(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "server/discover", "params": {}
        })));
        assert!(!is_stable_initialize(&json!({
            "jsonrpc": "1.0", "id": 1, "method": "initialize", "params": {}
        })));
        assert!(!is_stable_initialize(&json!({
            "jsonrpc": "2.0", "method": "initialize", "params": {}
        })));
        assert!(!is_stable_initialize(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}
        })));
        assert!(!is_stable_initialize(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2026-07-28",
                "capabilities": {},
                "clientInfo": {"name": "preview-client", "version": "1.0.0"}
            }
        })));
    }

    #[test]
    fn modern_metadata_requires_capabilities_and_accepts_optional_identity() {
        assert!(validate_meta(&meta(MODERN_VERSION)).is_ok());

        let unsupported = validate_meta(&meta("1900-01-01"));
        assert!(
            matches!(unsupported, Err(MetaError::Unsupported(version)) if version == "1900-01-01")
        );
        assert!(matches!(
            validate_meta(&meta("")),
            Err(MetaError::Unsupported(version)) if version.is_empty()
        ));

        let mut without_identity = meta(MODERN_VERSION);
        without_identity
            .get_mut("_meta")
            .unwrap()
            .as_object_mut()
            .unwrap()
            .remove(META_CLIENT_INFO);
        assert!(validate_meta(&without_identity).is_ok());

        let mut empty_identity = meta(MODERN_VERSION);
        empty_identity
            .get_mut("_meta")
            .unwrap()
            .as_object_mut()
            .unwrap()
            .insert(
                META_CLIENT_INFO.to_owned(),
                json!({"name": "", "version": ""}),
            );
        assert!(validate_meta(&empty_identity).is_ok());

        let mut without_capabilities = meta(MODERN_VERSION);
        without_capabilities
            .get_mut("_meta")
            .unwrap()
            .as_object_mut()
            .unwrap()
            .remove(META_CLIENT_CAPABILITIES);
        assert!(matches!(
            validate_meta(&without_capabilities),
            Err(MetaError::Invalid)
        ));

        let mut malformed = meta(MODERN_VERSION);
        malformed
            .get_mut("_meta")
            .unwrap()
            .as_object_mut()
            .unwrap()
            .insert(META_CLIENT_CAPABILITIES.to_owned(), json!({"roots": 1}));
        assert!(matches!(validate_meta(&malformed), Err(MetaError::Invalid)));
    }

    #[tokio::test]
    async fn bounded_reader_recovers_at_the_next_line() {
        let (mut client, server) = duplex(MAX_FRAME_BYTES + 64);
        let writer = tokio::spawn(async move {
            client
                .write_all(&vec![b'x'; MAX_FRAME_BYTES + 1])
                .await
                .unwrap();
            client.write_all(b"\n{}\n").await.unwrap();
        });
        let mut reader = BufReader::new(server);
        assert!(matches!(
            read_frame(&mut reader).await,
            Err(FrameReadError::TooLarge)
        ));
        assert_eq!(read_frame(&mut reader).await.unwrap(), Some(b"{}".to_vec()));
        writer.await.unwrap();
    }

    #[test]
    fn request_ids_are_bounded_integers_or_strings_including_empty() {
        assert_eq!(valid_id(Some(&json!(1))), Some(json!(1)));
        assert_eq!(valid_id(Some(&json!("request"))), Some(json!("request")));
        assert_eq!(
            valid_id(Some(&json!("x".repeat(256)))),
            Some(json!("x".repeat(256)))
        );
        assert_eq!(valid_id(Some(&Value::Null)), None);
        assert_eq!(valid_id(Some(&json!(1.5))), None);
        assert_eq!(valid_id(Some(&json!(""))), Some(json!("")));
        assert_eq!(valid_id(Some(&json!("x".repeat(257)))), None);
    }
}
