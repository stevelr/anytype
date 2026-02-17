// SPDX-FileCopyrightText: 2025-2026 Steve Schoettler
// SPDX-License-Identifier: Apache-2.0

//! # Process Watcher (gRPC)
//!
//! Subscribe to process events, wait for a specific process kind to complete,
//! and collect progress details.

use std::time::{Duration, Instant};

use anytype_rpc::{
    anytype::{
        Event, StreamRequest,
        event::message::Value as EventValue,
        model::process::State,
        rpc::process::{subscribe as process_subscribe, unsubscribe as process_unsubscribe},
    },
    client::AnytypeGrpcClient,
};
use tokio::sync::mpsc;
use tonic::Request;
use tracing::debug;

use crate::{
    Result,
    error::AnytypeError,
    grpc_util::{ensure_error_ok, grpc_status, with_token_request},
};

const DEFAULT_EVENT_STREAM_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const DEFAULT_PROCESS_START_TIMEOUT: Duration = Duration::from_secs(20);
const DEFAULT_PROCESS_IDLE_TIMEOUT: Duration = Duration::from_secs(30);
const DEFAULT_PROCESS_DONE_TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// Timeouts used by [`ProcessWatcher`].
#[derive(Debug, Clone, Copy)]
#[allow(clippy::struct_field_names)]
pub struct ProcessWatcherTimeouts {
    pub event_stream_connect_timeout: Duration,
    pub process_start_timeout: Duration,
    pub process_idle_timeout: Duration,
    pub process_done_timeout: Duration,
}

impl Default for ProcessWatcherTimeouts {
    fn default() -> Self {
        Self {
            event_stream_connect_timeout: DEFAULT_EVENT_STREAM_CONNECT_TIMEOUT,
            process_start_timeout: DEFAULT_PROCESS_START_TIMEOUT,
            process_idle_timeout: DEFAULT_PROCESS_IDLE_TIMEOUT,
            process_done_timeout: DEFAULT_PROCESS_DONE_TIMEOUT,
        }
    }
}

/// Process message kind to match while watching events.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessKind {
    DropFiles,
    Import,
    Export,
    SaveFile,
    Migration,
    PreloadFile,
}

/// Optional completion fallback when process events are not emitted reliably.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessCompletionFallback {
    None,
    ImportFinishEvent,
}

/// Process matching policy used by [`ProcessWatcher::wait_for_process`].
#[derive(Debug, Clone)]
#[allow(clippy::struct_field_names)]
pub struct ProcessWatchRequest {
    pub kind: ProcessKind,
    pub space_id: String,
    pub allow_empty_space_id: bool,
    pub completion_fallback: ProcessCompletionFallback,
    pub cancel_message: String,
    pub log_progress: bool,
}

impl ProcessWatchRequest {
    /// Create a watch request for a process kind in a target space.
    #[must_use]
    pub fn new(kind: ProcessKind, space_id: impl Into<String>) -> Self {
        Self {
            kind,
            space_id: space_id.into(),
            allow_empty_space_id: false,
            completion_fallback: ProcessCompletionFallback::None,
            cancel_message: "process canceled by caller".to_string(),
            log_progress: false,
        }
    }

    #[must_use]
    pub fn allow_empty_space_id(mut self, allow: bool) -> Self {
        self.allow_empty_space_id = allow;
        self
    }

    #[must_use]
    pub fn completion_fallback(mut self, fallback: ProcessCompletionFallback) -> Self {
        self.completion_fallback = fallback;
        self
    }

    #[must_use]
    pub fn cancel_message(mut self, message: impl Into<String>) -> Self {
        self.cancel_message = message.into();
        self
    }

    #[must_use]
    pub fn log_progress(mut self, enabled: bool) -> Self {
        self.log_progress = enabled;
        self
    }
}

/// Cancellation token for [`ProcessWatcher::wait_for_process`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessWatchCancelToken {
    Requested,
}

/// Summary of process events observed by [`ProcessWatcher`].
#[derive(Debug, Clone, Default)]
pub struct ProcessWatchProgress {
    pub processes_started: usize,
    pub processes_done: usize,
    pub process_updates: usize,
    pub import_finish_events: usize,
    pub import_finish_objects: i64,
    pub last_process_id: Option<String>,
    pub last_process_state: Option<String>,
    pub last_progress_done: Option<i64>,
    pub last_progress_total: Option<i64>,
    pub last_progress_message: Option<String>,
    pub last_process_error: Option<String>,
}

/// Watches process lifecycle events over gRPC session events.
#[derive(Debug, Default)]
pub struct ProcessWatcher {
    stream: Option<tonic::Streaming<Event>>,
    process_id: Option<String>,
    progress: ProcessWatchProgress,
    timeouts: ProcessWatcherTimeouts,
}

impl ProcessWatcher {
    /// Subscribe to process events and open the session event stream.
    pub async fn subscribe(
        grpc: &AnytypeGrpcClient,
        timeouts: ProcessWatcherTimeouts,
    ) -> Result<Self> {
        let mut commands = grpc.client_commands();
        let mut subscribe_request =
            with_token_request(Request::new(process_subscribe::Request {}), grpc.token())?;
        subscribe_request.set_timeout(timeouts.event_stream_connect_timeout);
        let subscribe_response = commands
            .process_subscribe(subscribe_request)
            .await
            .map_err(grpc_status)?
            .into_inner();
        ensure_error_ok(subscribe_response.error.as_ref(), "process subscribe")?;

        let stream = open_session_events(grpc, timeouts.event_stream_connect_timeout).await?;
        Ok(Self {
            stream: Some(stream),
            timeouts,
            ..Self::default()
        })
    }

    /// Wait for a matching process to complete.
    pub async fn wait_for_process(
        &mut self,
        grpc: &AnytypeGrpcClient,
        request: &ProcessWatchRequest,
        cancel_rx: Option<&mut mpsc::UnboundedReceiver<ProcessWatchCancelToken>>,
    ) -> Result<()> {
        self.process_id = None;
        let import_finish_at_start = self.progress.import_finish_events;
        let started_at = Instant::now();
        let start_deadline = started_at + self.timeouts.process_start_timeout;
        let done_deadline = started_at + self.timeouts.process_done_timeout;
        let mut last_update = started_at;
        let mut cancel_rx = cancel_rx;

        loop {
            let now = Instant::now();
            if now >= done_deadline {
                return Err(AnytypeError::Other {
                    message: format!(
                        "process watch timed out waiting for completion after {}s",
                        self.timeouts.process_done_timeout.as_secs()
                    ),
                });
            }
            let checkpoint = if self.process_id.is_none() {
                start_deadline
            } else {
                std::cmp::min(
                    done_deadline,
                    last_update + self.timeouts.process_idle_timeout,
                )
            };
            if now >= checkpoint {
                if self.process_id.is_none() {
                    return Err(AnytypeError::Other {
                        message: format!(
                            "process watch timed out waiting for process start after {}s",
                            self.timeouts.process_start_timeout.as_secs()
                        ),
                    });
                }
                return Err(AnytypeError::Other {
                    message: format!(
                        "process watch became idle for {}s",
                        self.timeouts.process_idle_timeout.as_secs()
                    ),
                });
            }
            let timeout_for_event = checkpoint.saturating_duration_since(now);
            let stream = self.stream.as_mut().ok_or_else(|| AnytypeError::Other {
                message: "session event stream is not active".to_string(),
            })?;
            let next = wait_for_next_event(
                stream,
                timeout_for_event,
                cancel_rx.as_deref_mut(),
                &request.cancel_message,
            )
            .await?;
            let Some(event) = next else {
                self.reconnect_stream(grpc).await?;
                continue;
            };
            let (completed, observed) = self.process_event(&event, request)?;
            if observed {
                last_update = Instant::now();
            }
            if completed {
                return Ok(());
            }
            if self.process_id.is_none()
                && request.completion_fallback == ProcessCompletionFallback::ImportFinishEvent
                && self.progress.import_finish_events > import_finish_at_start
            {
                return Ok(());
            }
        }
    }

    /// Unsubscribe from process events.
    pub async fn unsubscribe(&self, grpc: &AnytypeGrpcClient) -> Result<()> {
        let mut commands = grpc.client_commands();
        let mut request =
            with_token_request(Request::new(process_unsubscribe::Request {}), grpc.token())?;
        request.set_timeout(self.timeouts.event_stream_connect_timeout);
        let response = commands
            .process_unsubscribe(request)
            .await
            .map_err(grpc_status)?
            .into_inner();
        ensure_error_ok(response.error.as_ref(), "process unsubscribe")
    }

    /// Return a snapshot of observed process progress.
    #[must_use]
    pub fn progress(&self) -> ProcessWatchProgress {
        self.progress.clone()
    }

    /// Consume watcher and return observed process progress.
    #[must_use]
    pub fn into_progress(self) -> ProcessWatchProgress {
        self.progress
    }

    async fn reconnect_stream(&mut self, grpc: &AnytypeGrpcClient) -> Result<()> {
        let stream = open_session_events(grpc, self.timeouts.event_stream_connect_timeout).await?;
        self.stream = Some(stream);
        Ok(())
    }

    fn process_event(
        &mut self,
        event: &Event,
        request: &ProcessWatchRequest,
    ) -> Result<(bool, bool)> {
        let mut observed = false;
        for message in &event.messages {
            if let Some(EventValue::ImportFinish(finish)) = &message.value {
                self.progress.import_finish_events =
                    self.progress.import_finish_events.saturating_add(1);
                self.progress.import_finish_objects = self
                    .progress
                    .import_finish_objects
                    .saturating_add(finish.objects_count.max(0));
                continue;
            }
            let (kind, process) = match &message.value {
                Some(EventValue::ProcessNew(new)) => ("processNew", new.process.as_ref()),
                Some(EventValue::ProcessUpdate(update)) => {
                    ("processUpdate", update.process.as_ref())
                }
                Some(EventValue::ProcessDone(done)) => ("processDone", done.process.as_ref()),
                _ => continue,
            };
            let Some(process) = process else {
                continue;
            };
            if !matches_process_kind(process, request.kind) {
                continue;
            }
            if !space_matches(
                process.space_id.as_str(),
                request.space_id.as_str(),
                request.allow_empty_space_id,
            ) {
                continue;
            }
            if self.process_id.is_none() {
                if kind != "processNew" {
                    continue;
                }
                self.process_id = Some(process.id.clone());
                self.progress.processes_started = self.progress.processes_started.saturating_add(1);
            }
            if self.process_id.as_deref() != Some(process.id.as_str()) {
                continue;
            }
            observed = true;
            self.progress.last_process_id = Some(process.id.clone());
            self.progress.last_process_state = State::try_from(process.state)
                .ok()
                .map(|state| state.as_str_name().to_string());
            self.progress.last_process_error = if process.error.is_empty() {
                None
            } else {
                Some(process.error.clone())
            };
            if let Some(progress) = &process.progress {
                self.progress.last_progress_done = Some(progress.done);
                self.progress.last_progress_total = Some(progress.total);
                self.progress.last_progress_message = if progress.message.is_empty() {
                    None
                } else {
                    Some(progress.message.clone())
                };
                if request.log_progress {
                    debug!(
                        "process event progress: process={} done={} total={} message={}",
                        process.id, progress.done, progress.total, progress.message
                    );
                }
            }

            match kind {
                "processUpdate" => {
                    self.progress.process_updates = self.progress.process_updates.saturating_add(1);
                }
                "processDone" => {
                    self.progress.processes_done = self.progress.processes_done.saturating_add(1);
                    if !process.error.is_empty() {
                        return Err(AnytypeError::Other {
                            message: format!("process {} failed: {}", process.id, process.error),
                        });
                    }
                    return Ok((true, true));
                }
                _ => {}
            }

            if matches!(
                State::try_from(process.state),
                Ok(State::Done | State::Canceled | State::Error)
            ) {
                if !process.error.is_empty() {
                    return Err(AnytypeError::Other {
                        message: format!("process {} failed: {}", process.id, process.error),
                    });
                }
                self.progress.processes_done = self.progress.processes_done.saturating_add(1);
                return Ok((true, true));
            }
        }
        Ok((false, observed))
    }
}

fn matches_process_kind(process: &anytype_rpc::anytype::model::Process, kind: ProcessKind) -> bool {
    use anytype_rpc::anytype::model::process::Message;
    matches!(
        (&process.message, kind),
        (Some(Message::DropFiles(_)), ProcessKind::DropFiles)
            | (Some(Message::Import(_)), ProcessKind::Import)
            | (Some(Message::Export(_)), ProcessKind::Export)
            | (Some(Message::SaveFile(_)), ProcessKind::SaveFile)
            | (Some(Message::Migration(_)), ProcessKind::Migration)
            | (Some(Message::PreloadFile(_)), ProcessKind::PreloadFile)
    )
}

fn space_matches(actual: &str, expected: &str, allow_empty_space_id: bool) -> bool {
    actual == expected || (allow_empty_space_id && actual.is_empty())
}

async fn open_session_events(
    grpc: &AnytypeGrpcClient,
    connect_timeout: Duration,
) -> Result<tonic::Streaming<Event>> {
    let request = StreamRequest {
        token: grpc.token().to_string(),
    };
    let request = with_token_request(Request::new(request), grpc.token())?;
    let response = tokio::time::timeout(
        connect_timeout,
        grpc.client_commands().listen_session_events(request),
    )
    .await
    .map_err(|_| AnytypeError::Other {
        message: format!(
            "timed out opening session event stream after {}s",
            connect_timeout.as_secs()
        ),
    })?
    .map_err(grpc_status)?
    .into_inner();
    Ok(response)
}

async fn wait_for_next_event(
    stream: &mut tonic::Streaming<Event>,
    timeout: Duration,
    cancel_rx: Option<&mut mpsc::UnboundedReceiver<ProcessWatchCancelToken>>,
    cancel_message: &str,
) -> Result<Option<Event>> {
    let next = if let Some(cancel_rx) = cancel_rx {
        tokio::select! {
            _ = tokio::time::sleep(timeout) => {
                return Err(AnytypeError::Other {
                    message: "timed out waiting for process event".to_string(),
                });
            }
            token = cancel_rx.recv() => {
                match token {
                    Some(ProcessWatchCancelToken::Requested) => {
                        return Err(AnytypeError::Other {
                            message: cancel_message.to_string(),
                        });
                    }
                    None => {
                        return Err(AnytypeError::Other {
                            message: "process cancel channel closed unexpectedly".to_string(),
                        });
                    }
                }
            }
            next = stream.message() => next,
        }
    } else {
        tokio::select! {
            _ = tokio::time::sleep(timeout) => {
                return Err(AnytypeError::Other {
                    message: "timed out waiting for process event".to_string(),
                });
            }
            next = stream.message() => next,
        }
    };
    match next {
        Ok(Some(event)) => Ok(Some(event)),
        Ok(None) => Ok(None),
        Err(err) => {
            debug!("session event stream read failed; reconnecting: {err:#}");
            Ok(None)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        net::{SocketAddr, TcpListener},
        time::Duration,
    };

    use anytype_rpc::{
        anytype::{Event, event::Message as EventMessage, event::message::Value as EventValue},
        client::{AnytypeGrpcClient, AnytypeGrpcConfig},
    };

    use super::*;
    use crate::mock::MockChatServer;

    fn next_test_addr() -> SocketAddr {
        let listener =
            TcpListener::bind("127.0.0.1:0").expect("bind ephemeral test listener must succeed");
        let addr = listener
            .local_addr()
            .expect("ephemeral listener must have local address");
        drop(listener);
        addr
    }

    #[tokio::test]
    async fn watcher_completes_on_import_finish_fallback() {
        let addr = next_test_addr();
        let server = MockChatServer::start(addr).expect("mock server must start");

        let endpoint = format!("http://{}", server.addr());
        let grpc = {
            let mut last_err = None;
            let mut connected = None;
            for _ in 0..20 {
                match AnytypeGrpcClient::from_token(
                    &AnytypeGrpcConfig::new(endpoint.clone()),
                    "token-alice".to_string(),
                )
                .await
                {
                    Ok(client) => {
                        connected = Some(client);
                        break;
                    }
                    Err(err) => {
                        last_err = Some(err);
                        tokio::time::sleep(Duration::from_millis(25)).await;
                    }
                }
            }
            connected.unwrap_or_else(|| {
                panic!(
                    "grpc mock client must connect: {}",
                    last_err.map_or_else(|| "unknown error".to_string(), |err| err.to_string())
                )
            })
        };

        let timeouts = ProcessWatcherTimeouts {
            event_stream_connect_timeout: Duration::from_secs(2),
            process_start_timeout: Duration::from_secs(2),
            process_idle_timeout: Duration::from_secs(2),
            process_done_timeout: Duration::from_secs(5),
        };
        let mut watcher = ProcessWatcher::subscribe(&grpc, timeouts)
            .await
            .expect("watcher subscribe must succeed");

        let event = Event {
            messages: vec![EventMessage {
                space_id: String::new(),
                value: Some(EventValue::ImportFinish(
                    anytype_rpc::anytype::event::import::Finish {
                        objects_count: 3,
                        root_collection_id: String::new(),
                        import_type: 0,
                    },
                )),
            }],
            context_id: String::new(),
            initiator: None,
            trace_id: String::new(),
        };
        server.emit_event(event).await;

        let request = ProcessWatchRequest::new(ProcessKind::Import, "space-test")
            .allow_empty_space_id(true)
            .completion_fallback(ProcessCompletionFallback::ImportFinishEvent);
        tokio::time::timeout(
            Duration::from_secs(5),
            watcher.wait_for_process(&grpc, &request, None),
        )
        .await
        .expect("watcher wait should not hang")
        .expect("watcher should complete from fallback event");

        let progress = watcher.progress();
        assert_eq!(progress.import_finish_events, 1);
        assert_eq!(progress.import_finish_objects, 3);

        tokio::time::timeout(Duration::from_secs(3), watcher.unsubscribe(&grpc))
            .await
            .expect("watcher unsubscribe should not hang")
            .expect("watcher unsubscribe must succeed");
        tokio::time::timeout(Duration::from_secs(3), server.shutdown())
            .await
            .expect("mock server shutdown should not hang");
    }
}
