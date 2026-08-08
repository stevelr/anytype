// any-mcp - bounded, workflow-oriented MCP server for Anytype
//
// SPDX-FileCopyrightText: 2026 Steve Schoettler
// SPDX-License-Identifier: Apache-2.0

//! Explicitly selected stdio framing for stable and experimental MCP clients.

use std::{collections::HashMap, io, sync::Arc};

use rmcp::{
    RoleServer,
    model::InitializeRequestParams,
    service::{RxJsonRpcMessage, TxJsonRpcMessage},
    transport::Transport,
};
use serde_json::Value;
use tokio::{
    io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader},
    sync::{Mutex, Semaphore, mpsc},
    task::JoinSet,
};
use tokio_util::sync::CancellationToken;

use crate::{
    config::ProtocolMode,
    preview::{
        CancellationMap, MODERN_VERSION, PreviewClassification, PreviewRequest, cancel_all,
        classify_preview_frame, dispatch_modern, internal_error, invalid_request, parse_error,
        valid_id,
    },
    runtime::{ServeError, serve_transport},
    server::AnyMcpServer,
};

const MAX_FRAME_BYTES: usize = 2 * 1024 * 1024;
const MAX_IN_FLIGHT_REQUESTS: usize = 64;

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

pub(crate) async fn serve_stable<R, W>(
    server: AnyMcpServer,
    mut reader: R,
    mut writer: W,
) -> Result<(), ServeError>
where
    R: AsyncBufRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    // Stable stdio serves exactly one initialized client session per process,
    // so MCP client roots may narrow the static local artifact root policy for
    // this session. Preview stdio and multi-session transports do not.
    server.runtime().client_roots().enable();
    loop {
        let frame = match read_frame(&mut reader).await {
            Ok(Some(frame)) if frame.iter().all(u8::is_ascii_whitespace) => continue,
            Ok(Some(frame)) => frame,
            Ok(None) => {
                shutdown_runtime(server.runtime()).await;
                return Ok(());
            }
            Err(FrameReadError::TooLarge) => {
                write_gate_response(&mut writer, &invalid_request(Value::Null)).await?;
                continue;
            }
            Err(FrameReadError::Io) => {
                shutdown_runtime(server.runtime()).await;
                return Err(ServeError::StdioTransport);
            }
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

pub(crate) async fn serve_preview<R, W>(
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
            Err(FrameReadError::Io) => {
                shutdown_runtime(server.runtime()).await;
                return Err(ServeError::StdioTransport);
            }
        }
    };
    let Some(first) = first else {
        shutdown_runtime(server.runtime()).await;
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
        if self.pending_decoder_frame.is_some() {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "a decoder frame is already pending",
            ));
        }
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
        let encoded = self.pending_decoder_frame.take().ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotConnected, "no decoder frame is pending")
        })?;
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
    let runtime_shutdown = runtime.shutdown_token();

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
            () = runtime_shutdown.cancelled() => {
                cancel_all(&cancellations).await;
                abort_requests(&mut requests).await;
                drop(responses);
                let _ = writer_task.await;
                runtime
                    .drain_artifact_staging(runtime.artifact_config().limits.operation_timeout)
                    .await;
                return Err(ServeError::ServiceTask);
            }
            writer_result = &mut writer_task => {
                runtime.begin_shutdown();
                cancel_all(&cancellations).await;
                abort_requests(&mut requests).await;
                drain_artifact_settlements(&runtime).await;
                return match writer_result {
                    Ok(Ok(())) => Err(ServeError::StdioTransport),
                    Ok(Err(())) | Err(_) => Err(ServeError::StdioTransport),
                };
            }
            completed = requests.join_next(), if !requests.is_empty() => {
                if completed.is_some_and(|result| result.is_err()) {
                    runtime.begin_shutdown();
                    cancel_all(&cancellations).await;
                    abort_requests(&mut requests).await;
                    drop(responses);
                    let _ = writer_task.await;
                    drain_artifact_settlements(&runtime).await;
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
                        abort_requests(&mut requests).await;
                        drain_artifact_settlements(&runtime).await;
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
    let writer_result = writer_task.await;
    drain_artifact_settlements(&runtime).await;
    match (task_failed, writer_result) {
        (false, Ok(Ok(()))) => Ok(()),
        (true, _) => Err(ServeError::ServiceTask),
        (false, Ok(Err(()))) | (false, Err(_)) => Err(ServeError::StdioTransport),
    }
}

async fn abort_requests(requests: &mut JoinSet<()>) {
    requests.abort_all();
    while requests.join_next().await.is_some() {}
}

/// Waits for shutdown-owned artifact settlement after preview request work has stopped.
async fn drain_artifact_settlements(runtime: &crate::RuntimeContext) {
    runtime
        .drain_artifact_settlements(runtime.artifact_config().limits.operation_timeout)
        .await;
}

/// Stops runtime admission and waits for owned artifact settlement.
async fn shutdown_runtime(runtime: &crate::RuntimeContext) {
    runtime.begin_shutdown();
    drain_artifact_settlements(runtime).await;
    runtime
        .drain_artifact_staging(runtime.artifact_config().limits.operation_timeout)
        .await;
}

async fn handle_frame(
    server: &AnyMcpServer,
    cancellations: &CancellationMap,
    permits: &Arc<Semaphore>,
    responses: &mpsc::Sender<Value>,
    requests: &mut JoinSet<()>,
    frame: &[u8],
) {
    let request = match classify_preview_frame(frame) {
        PreviewClassification::Response(response) => {
            let _ = responses.send(response).await;
            return;
        }
        PreviewClassification::CancelRequested(request_key) => {
            if let Some(cancellation) = cancellations.lock().await.get(&request_key) {
                cancellation.cancel();
            }
            return;
        }
        PreviewClassification::Ignored => return,
        PreviewClassification::Request(request) => request,
    };
    let PreviewRequest {
        id,
        request_key,
        method,
        params,
    } = request;

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
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };

    use serde_json::{Map, json};
    use tokio::io::{AsyncWriteExt, BufReader, duplex};

    use super::*;
    use crate::artifact_toolset::ImportIdempotency;
    use crate::preview::{
        META_CLIENT_CAPABILITIES, META_CLIENT_INFO, META_PROTOCOL_VERSION, MetaError, validate_meta,
    };

    struct DropMarker(Arc<AtomicBool>);

    impl Drop for DropMarker {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Release);
        }
    }

    fn test_runtime() -> crate::runtime::RuntimeContext {
        use anytype::prelude::{AnytypeClient, ClientConfig};

        let client = AnytypeClient::with_config(ClientConfig {
            base_url: Some("http://127.0.0.1:1".to_owned()),
            keystore: Some("env".to_owned()),
            keystore_service: Some("any-mcp-test".to_owned()),
            app_name: "any-mcp-test".to_owned(),
            ..ClientConfig::default()
        })
        .expect("in-memory test client");
        crate::runtime::RuntimeContext::from_parts(
            client,
            1,
            std::time::Duration::from_secs(1),
            crate::runtime::StartupStatus {
                http_available: true,
                grpc_available: true,
            },
        )
    }

    async fn pending_settlement(runtime: &crate::runtime::RuntimeContext) -> Arc<AtomicBool> {
        let key = [31; 32];
        assert!(matches!(
            runtime
                .artifact_operations()
                .reserve_import(key, [32; 32])
                .await,
            Ok(ImportIdempotency::Dispatch)
        ));
        let permit = runtime
            .admit_import_settlement(runtime.request_deadline())
            .await
            .expect("settlement permit");
        let dropped = Arc::new(AtomicBool::new(false));
        let marker = Arc::clone(&dropped);
        let _receiver = runtime.supervise_import_settlement(key, permit, async move {
            let _marker = DropMarker(marker);
            std::future::pending().await
        });
        tokio::task::yield_now().await;
        dropped
    }

    #[tokio::test]
    async fn stable_stdio_enables_client_root_narrowing_and_preview_does_not() {
        let stable = test_runtime();
        let (client, server_side) = duplex(64);
        drop(client);
        let (reader, writer) = tokio::io::split(server_side);
        serve_stable(
            AnyMcpServer::new(stable.clone()).expect("static catalog"),
            BufReader::new(reader),
            writer,
        )
        .await
        .expect("clean stable shutdown");
        assert!(stable.client_roots().is_enabled());

        let preview = test_runtime();
        let (client, server_side) = duplex(64);
        drop(client);
        let (reader, writer) = tokio::io::split(server_side);
        serve_preview(
            AnyMcpServer::new(preview.clone()).expect("static catalog"),
            BufReader::new(reader),
            writer,
        )
        .await
        .expect("clean preview shutdown");
        assert!(!preview.client_roots().is_enabled());
    }

    #[tokio::test]
    async fn preview_eof_waits_for_owned_artifact_settlement() {
        let runtime = test_runtime();
        let dropped = pending_settlement(&runtime).await;
        let (client, server_side) = duplex(64);
        drop(client);
        let (reader, writer) = tokio::io::split(server_side);

        serve_preview(
            AnyMcpServer::new(runtime).expect("static catalog"),
            BufReader::new(reader),
            writer,
        )
        .await
        .expect("clean preview shutdown");

        assert!(dropped.load(Ordering::Acquire));
    }

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
