// SPDX-FileCopyrightText: 2025-2026 Steve Schoettler
// SPDX-License-Identifier: Apache-2.0

//! # Process Watcher (gRPC)
//!
//! Subscribe to process events, wait for a specific process kind to complete,
//! and collect progress details.

use std::{
    collections::HashSet,
    fmt,
    time::{Duration, Instant},
};

use anytype_rpc::{
    anytype::{
        Event, StreamRequest,
        event::message::Value as EventValue,
        model::process::State,
        rpc::process::{subscribe as process_subscribe, unsubscribe as process_unsubscribe},
    },
    client::AnytypeGrpcClient,
    deadline::{GrpcCallOptions, with_grpc_call_options},
};
use futures::FutureExt as _;
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
#[derive(Clone)]
#[allow(clippy::struct_field_names)]
pub struct ProcessWatchRequest {
    pub kind: ProcessKind,
    pub space_id: String,
    pub allow_empty_space_id: bool,
    pub completion_fallback: ProcessCompletionFallback,
    pub cancel_message: String,
    pub log_progress: bool,
}

impl fmt::Debug for ProcessWatchRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProcessWatchRequest")
            .field("kind", &self.kind)
            .field("space_id", &"redacted")
            .field("allow_empty_space_id", &self.allow_empty_space_id)
            .field("completion_fallback", &self.completion_fallback)
            .field("cancel_message", &"redacted")
            .field("log_progress", &self.log_progress)
            .finish()
    }
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
#[derive(Clone, Default)]
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

impl fmt::Debug for ProcessWatchProgress {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProcessWatchProgress")
            .field("processes_started", &self.processes_started)
            .field("processes_done", &self.processes_done)
            .field("process_updates", &self.process_updates)
            .field("import_finish_events", &self.import_finish_events)
            .field("import_finish_objects", &self.import_finish_objects)
            .field(
                "last_process_id",
                &self.last_process_id.as_ref().map(|_| "redacted"),
            )
            .field("last_process_state", &self.last_process_state)
            .field("last_progress_done", &self.last_progress_done)
            .field("last_progress_total", &self.last_progress_total)
            .field(
                "last_progress_message",
                &self.last_progress_message.as_ref().map(|_| "redacted"),
            )
            .field(
                "last_process_error",
                &self.last_process_error.as_ref().map(|_| "redacted"),
            )
            .finish()
    }
}

/// Opaque dispatch barrier for one process-producing request.
///
/// Create this immediately before dispatch with
/// [`ProcessWatcher::begin_generation`], then pass it to
/// [`ProcessWatcher::wait_for_generation`]. Events already queued at the
/// barrier cannot complete the new request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcessWatchGeneration(u64);

/// Correlation for one dispatched process generation.
///
/// Carries the server-issued root collection identifier when the dispatch
/// response supplied one; otherwise completion binds to the generation alone
/// (the process started after the dispatch barrier, or the import-finish
/// fallback that the request enables).
#[derive(Clone, PartialEq, Eq)]
pub struct ProcessWatchCorrelation {
    generation: ProcessWatchGeneration,
    root_collection_id: Option<String>,
}

impl fmt::Debug for ProcessWatchCorrelation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProcessWatchCorrelation")
            .field("generation", &self.generation)
            .field(
                "root_collection_id",
                &self.root_collection_id.as_ref().map(|_| "redacted"),
            )
            .finish()
    }
}

/// Watches process lifecycle events over gRPC session events.
#[derive(Default)]
pub struct ProcessWatcher {
    stream: Option<tonic::Streaming<Event>>,
    process_id: Option<String>,
    progress: ProcessWatchProgress,
    timeouts: ProcessWatcherTimeouts,
    generation: u64,
    used_correlations: HashSet<String>,
}

impl fmt::Debug for ProcessWatcher {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProcessWatcher")
            .field("stream_active", &self.stream.is_some())
            .field("process_id", &self.process_id.as_ref().map(|_| "redacted"))
            .field("progress", &self.progress)
            .field("timeouts", &self.timeouts)
            .field("generation", &self.generation)
            .field("used_correlation_count", &self.used_correlations.len())
            .finish()
    }
}

impl ProcessWatcher {
    /// Subscribes to process events and opens the gRPC session event stream.
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

    /// Drain events already queued on the subscription and establish a new
    /// dispatch generation. Call this immediately before the request that
    /// starts the process being watched.
    pub fn begin_generation(&mut self) -> Result<ProcessWatchGeneration> {
        loop {
            let stream = self.stream.as_mut().ok_or_else(|| AnytypeError::Other {
                message: "session event stream is not active".to_string(),
            })?;
            match stream.message().now_or_never() {
                None => break,
                Some(Ok(Some(_))) => {}
                Some(Ok(None)) => {
                    return Err(AnytypeError::Other {
                        message: "session event stream ended while establishing dispatch barrier"
                            .to_string(),
                    });
                }
                Some(Err(error)) => return Err(grpc_status(error)),
            }
        }
        self.generation = self
            .generation
            .checked_add(1)
            .ok_or_else(|| AnytypeError::Other {
                message: "process watch generation limit exceeded".to_string(),
            })?;
        self.process_id = None;
        Ok(ProcessWatchGeneration(self.generation))
    }

    /// Bind a generation to the server correlation returned by the dispatch
    /// RPC.
    ///
    /// A non-empty root collection identifier must be fresh: reuse is rejected
    /// so an event from an earlier batch cannot complete a later batch. The
    /// server omits the identifier for imports that create no root collection
    /// (ordinary `External` object imports); those batches complete on the
    /// generation alone, which the dispatch barrier already isolates from
    /// earlier batches.
    pub fn correlate_generation(
        &mut self,
        generation: ProcessWatchGeneration,
        root_collection_id: &str,
    ) -> Result<ProcessWatchCorrelation> {
        if generation.0 != self.generation {
            return Err(AnytypeError::Other {
                message: "stale process watch generation".to_string(),
            });
        }
        if root_collection_id.is_empty() {
            return Ok(ProcessWatchCorrelation {
                generation,
                root_collection_id: None,
            });
        }
        if !self
            .used_correlations
            .insert(root_collection_id.to_string())
        {
            return Err(AnytypeError::Other {
                message: "dispatch response reused process correlation".to_string(),
            });
        }
        Ok(ProcessWatchCorrelation {
            generation,
            root_collection_id: Some(root_collection_id.to_string()),
        })
    }

    /// Waits for a matching process on the open gRPC event stream.
    pub async fn wait_for_process(
        &mut self,
        grpc: &AnytypeGrpcClient,
        request: &ProcessWatchRequest,
        cancel_rx: Option<&mut mpsc::UnboundedReceiver<ProcessWatchCancelToken>>,
    ) -> Result<()> {
        self.generation = self
            .generation
            .checked_add(1)
            .ok_or_else(|| AnytypeError::Other {
                message: "process watch generation limit exceeded".to_string(),
            })?;
        self.process_id = None;
        self.wait(
            grpc,
            request,
            ProcessWatchGeneration(self.generation),
            None,
            cancel_rx,
        )
        .await
    }

    /// Waits on the gRPC event stream for a process started after `generation`.
    pub async fn wait_for_generation(
        &mut self,
        _grpc: &AnytypeGrpcClient,
        request: &ProcessWatchRequest,
        correlation: ProcessWatchCorrelation,
        cancel_rx: Option<&mut mpsc::UnboundedReceiver<ProcessWatchCancelToken>>,
    ) -> Result<()> {
        self.wait(
            _grpc,
            request,
            correlation.generation,
            correlation.root_collection_id.as_deref(),
            cancel_rx,
        )
        .await
    }

    async fn wait(
        &mut self,
        _grpc: &AnytypeGrpcClient,
        request: &ProcessWatchRequest,
        generation: ProcessWatchGeneration,
        root_collection_id: Option<&str>,
        cancel_rx: Option<&mut mpsc::UnboundedReceiver<ProcessWatchCancelToken>>,
    ) -> Result<()> {
        if generation.0 != self.generation {
            return Err(AnytypeError::Other {
                message: "stale process watch generation".to_string(),
            });
        }
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
                return Err(AnytypeError::Other {
                    message:
                        "session event stream disconnected during the active dispatch generation"
                            .to_string(),
                });
            };
            let correlated_completion = root_collection_id
                .is_some_and(|expected| event_completes_correlation(&event, request, expected));
            let (completed, observed) =
                self.process_event_for_generation(&event, request, generation)?;
            if observed {
                last_update = Instant::now();
            }
            if root_collection_id.is_some() && correlated_completion {
                return Ok(());
            }
            if root_collection_id.is_none() && completed {
                return Ok(());
            }
            if root_collection_id.is_none()
                && self.process_id.is_none()
                && request.kind == ProcessKind::Import
                && request.completion_fallback == ProcessCompletionFallback::ImportFinishEvent
                && self.progress.import_finish_events > import_finish_at_start
            {
                return Ok(());
            }
        }
    }

    /// Unsubscribes from gRPC process events.
    pub async fn unsubscribe(&self, grpc: &AnytypeGrpcClient) -> Result<()> {
        let mut commands = grpc.client_commands();
        let mut request =
            with_token_request(Request::new(process_unsubscribe::Request {}), grpc.token())?;
        request.set_timeout(self.timeouts.event_stream_connect_timeout);
        let request = with_grpc_call_options(request, GrpcCallOptions::cleanup());
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

    fn process_event(
        &mut self,
        event: &Event,
        request: &ProcessWatchRequest,
    ) -> Result<(bool, bool)> {
        let mut started_id = self.process_id.as_deref();
        for message in &event.messages {
            let Some(EventValue::ProcessNew(new)) = &message.value else {
                continue;
            };
            let Some(process) = new.process.as_ref() else {
                continue;
            };
            if matches_process_kind(process, request.kind)
                && space_matches(
                    process.space_id.as_str(),
                    request.space_id.as_str(),
                    request.allow_empty_space_id,
                )
            {
                if started_id.is_some_and(|id| id != process.id) {
                    return Err(AnytypeError::Other {
                        message:
                            "multiple matching processes started during one dispatch generation"
                                .to_string(),
                    });
                }
                started_id = Some(process.id.as_str());
            }
        }
        let mut observed = false;
        for message in &event.messages {
            if let Some(EventValue::ImportFinish(finish)) = &message.value {
                if request.kind != ProcessKind::Import
                    || request.completion_fallback != ProcessCompletionFallback::ImportFinishEvent
                    || !space_matches(
                        message.space_id.as_str(),
                        request.space_id.as_str(),
                        request.allow_empty_space_id,
                    )
                {
                    continue;
                }
                self.progress.import_finish_events =
                    self.progress.import_finish_events.saturating_add(1);
                self.progress.import_finish_objects = self
                    .progress
                    .import_finish_objects
                    .saturating_add(finish.objects_count.max(0));
                observed = true;
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
            self.progress.last_process_id = Some("redacted".to_string());
            let process_state = State::try_from(process.state).ok();
            self.progress.last_process_state =
                process_state.map(|state| state.as_str_name().to_string());
            self.progress.last_process_error = if process.error.is_empty() {
                None
            } else {
                Some("reported".to_string())
            };
            if let Some(progress) = &process.progress {
                self.progress.last_progress_done = Some(progress.done);
                self.progress.last_progress_total = Some(progress.total);
                self.progress.last_progress_message = if progress.message.is_empty() {
                    None
                } else {
                    Some("reported".to_string())
                };
                if request.log_progress {
                    debug!(
                        class = "process_progress",
                        done = progress.done,
                        total = progress.total,
                        "process event progress observed"
                    );
                }
            }

            match kind {
                "processUpdate" => {
                    self.progress.process_updates = self.progress.process_updates.saturating_add(1);
                }
                "processDone" => {
                    self.progress.processes_done = self.progress.processes_done.saturating_add(1);
                    if !process.error.is_empty() || process_state != Some(State::Done) {
                        return Err(AnytypeError::Other {
                            message: terminal_failure_category(process_state).to_string(),
                        });
                    }
                    return Ok((true, true));
                }
                _ => {}
            }

            if matches!(
                process_state,
                Some(State::Done | State::Canceled | State::Error)
            ) {
                if !process.error.is_empty()
                    || matches!(process_state, Some(State::Canceled | State::Error))
                {
                    return Err(AnytypeError::Other {
                        message: terminal_failure_category(process_state).to_string(),
                    });
                }
                self.progress.processes_done = self.progress.processes_done.saturating_add(1);
                return Ok((true, true));
            }
        }
        Ok((false, observed))
    }

    fn process_event_for_generation(
        &mut self,
        event: &Event,
        request: &ProcessWatchRequest,
        generation: ProcessWatchGeneration,
    ) -> Result<(bool, bool)> {
        if generation.0 != self.generation {
            return Err(AnytypeError::Other {
                message: "stale process watch generation".to_string(),
            });
        }
        self.process_event(event, request)
    }
}

fn event_completes_correlation(
    event: &Event,
    request: &ProcessWatchRequest,
    expected: &str,
) -> bool {
    event.messages.iter().any(|message| {
        space_matches(
            message.space_id.as_str(),
            request.space_id.as_str(),
            request.allow_empty_space_id,
        ) && matches!(
            &message.value,
            Some(EventValue::ImportFinish(finish)) if finish.root_collection_id == expected
        )
    })
}

fn terminal_failure_category(state: Option<State>) -> &'static str {
    match state {
        Some(State::Canceled) => "process watch observed terminal state=canceled",
        Some(State::Error) => "process watch observed terminal state=error",
        Some(State::Done) => "process watch observed terminal state=done with reported error",
        _ => "process watch observed invalid processDone state",
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
    let request = session_event_request(grpc.token())?;
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

fn session_event_request(token: &str) -> Result<Request<StreamRequest>> {
    let request = StreamRequest {
        token: token.to_owned(),
    };
    let request = with_token_request(Request::new(request), token)?;
    Ok(with_grpc_call_options(
        request,
        GrpcCallOptions::stream_setup(),
    ))
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
            log_stream_read_failure(&err);
            Ok(None)
        }
    }
}

fn log_stream_read_failure(error: &tonic::Status) {
    debug!(
        code = %error.code(),
        class = "stream_read",
        "session event stream read failed; reconnecting"
    );
}

#[cfg(test)]
mod tests {
    use std::{
        io::{self, Write},
        sync::{Arc, Mutex, Once},
    };

    use anytype_rpc::anytype::{
        Event, event::Message as EventMessage, event::message::Value as EventValue,
    };
    use tracing::Dispatch;
    use tracing_subscriber::{fmt as tracing_fmt, layer::SubscriberExt};

    use super::*;

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

    fn import_process_event(kind: &str, process_id: &str) -> Event {
        use anytype_rpc::anytype::{event::process, model, model::process as model_process};

        let process = model::Process {
            id: process_id.to_owned(),
            state: if kind == "done" {
                model_process::State::Done as i32
            } else {
                model_process::State::Running as i32
            },
            progress: None,
            space_id: "space-test".to_owned(),
            error: String::new(),
            message: Some(model_process::Message::Import(model_process::Import {})),
        };
        let value = if kind == "done" {
            EventValue::ProcessDone(process::Done {
                process: Some(process),
            })
        } else {
            EventValue::ProcessNew(process::New {
                process: Some(process),
            })
        };
        Event {
            messages: vec![EventMessage {
                space_id: "space-test".to_owned(),
                value: Some(value),
            }],
            context_id: String::new(),
            initiator: None,
            trace_id: String::new(),
        }
    }

    #[test]
    fn stale_generation_cannot_consume_a_later_batch_event() {
        let request = ProcessWatchRequest::new(ProcessKind::Import, "space-test");
        let mut watcher = ProcessWatcher {
            generation: 2,
            ..ProcessWatcher::default()
        };
        let error = watcher
            .process_event_for_generation(
                &import_process_event("new", "second"),
                &request,
                ProcessWatchGeneration(1),
            )
            .expect_err("stale generation must fail");
        assert!(matches!(
            error,
            AnytypeError::Other { message } if message == "stale process watch generation"
        ));
        assert!(watcher.process_id.is_none());
    }

    #[test]
    fn concurrent_matching_processes_are_ambiguous() {
        let request = ProcessWatchRequest::new(ProcessKind::Import, "space-test");
        let mut event = import_process_event("new", "first");
        event
            .messages
            .extend(import_process_event("new", "second").messages);
        let mut watcher = ProcessWatcher::default();
        let error = watcher
            .process_event(&event, &request)
            .expect_err("two starts in one generation must fail");
        assert!(matches!(
            error,
            AnytypeError::Other { message }
                if message.contains("multiple matching processes")
        ));
    }

    #[test]
    fn omitted_server_correlation_binds_to_the_generation_alone() {
        let mut watcher = ProcessWatcher {
            generation: 1,
            ..ProcessWatcher::default()
        };
        let correlation = watcher
            .correlate_generation(ProcessWatchGeneration(1), "")
            .expect("omitted identifier falls back to the generation");
        assert_eq!(correlation.generation, ProcessWatchGeneration(1));
        assert!(correlation.root_collection_id.is_none());
        assert!(watcher.used_correlations.is_empty());
        assert!(
            watcher
                .correlate_generation(ProcessWatchGeneration(2), "")
                .is_err(),
            "a stale generation is still rejected"
        );
    }

    #[test]
    fn server_correlations_must_be_unique_per_batch() {
        let mut watcher = ProcessWatcher {
            generation: 1,
            ..ProcessWatcher::default()
        };
        watcher
            .correlate_generation(ProcessWatchGeneration(1), "collection-one")
            .expect("first server correlation");
        watcher.generation = 2;
        assert!(
            watcher
                .correlate_generation(ProcessWatchGeneration(2), "collection-one")
                .is_err(),
            "a prior batch correlation must never be reused"
        );
    }

    #[test]
    fn public_watcher_debug_redacts_process_and_correlation_ids() {
        let hostile = "HOSTILE_ID\nC:\\secret\\token";
        let correlation = ProcessWatchCorrelation {
            generation: ProcessWatchGeneration(7),
            root_collection_id: Some(hostile.to_string()),
        };
        let mut watcher = ProcessWatcher {
            process_id: Some(hostile.to_string()),
            generation: 7,
            ..ProcessWatcher::default()
        };
        watcher.used_correlations.insert(hostile.to_string());
        let request = ProcessWatchRequest::new(ProcessKind::Import, hostile)
            .cancel_message(hostile.to_string());
        let progress = ProcessWatchProgress {
            last_process_id: Some(hostile.to_string()),
            last_progress_message: Some(hostile.to_string()),
            last_process_error: Some(hostile.to_string()),
            ..ProcessWatchProgress::default()
        };

        let diagnostics = format!("{correlation:?} {watcher:?} {request:?} {progress:?}");
        assert!(diagnostics.contains("redacted"));
        assert!(!diagnostics.contains("HOSTILE_ID"));
        assert!(!diagnostics.contains("secret"));
        assert!(!diagnostics.contains("token"));
    }

    #[test]
    fn empty_error_terminal_states_fail_with_fixed_redacted_categories() {
        use anytype_rpc::anytype::{event::process, model::process::State};

        let request = ProcessWatchRequest::new(ProcessKind::Import, "space-test");
        for state in [State::Canceled, State::Error] {
            let mut watcher = ProcessWatcher::default();
            watcher
                .process_event(
                    &import_process_event("new", "HOSTILE_ID\n/tmp/token"),
                    &request,
                )
                .expect("process start");
            let mut event = import_process_event("done", "HOSTILE_ID\n/tmp/token");
            let Some(EventValue::ProcessDone(process::Done {
                process: Some(process),
            })) = event.messages[0].value.as_mut()
            else {
                unreachable!("constructed processDone")
            };
            process.state = state as i32;
            process.error = String::new();
            let error = watcher
                .process_event(&event, &request)
                .expect_err("canceled/error state must fail");
            let diagnostics = format!("{error} {error:?} {}", error.diagnostic());
            assert!(diagnostics.contains("other"));
            assert!(!diagnostics.contains("HOSTILE_ID"));
            assert!(!diagnostics.contains("/tmp/token"));
            assert!(std::error::Error::source(&error).is_none());
        }
    }

    #[test]
    fn process_done_error_payload_is_never_echoed() {
        use anytype_rpc::anytype::event::process;

        let request = ProcessWatchRequest::new(ProcessKind::Import, "space-test");
        let mut watcher = ProcessWatcher::default();
        watcher
            .process_event(&import_process_event("new", "HOSTILE_ID"), &request)
            .expect("process start");
        let mut event = import_process_event("done", "HOSTILE_ID");
        let Some(EventValue::ProcessDone(process::Done {
            process: Some(process),
        })) = event.messages[0].value.as_mut()
        else {
            unreachable!("constructed processDone")
        };
        process.error = "HOSTILE_ERROR\nC:\\secret\\token".to_string();
        let error = watcher
            .process_event(&event, &request)
            .expect_err("reported process error must fail");
        let diagnostics = format!("{error} {error:?} {}", error.diagnostic());
        assert!(!diagnostics.contains("HOSTILE_ID"));
        assert!(!diagnostics.contains("HOSTILE_ERROR"));
        assert!(!diagnostics.contains("secret"));
    }

    #[test]
    fn successive_batches_require_their_own_generation_and_process() {
        let request = ProcessWatchRequest::new(ProcessKind::Import, "space-test");
        let mut watcher = ProcessWatcher {
            generation: 1,
            ..ProcessWatcher::default()
        };
        watcher
            .process_event_for_generation(
                &import_process_event("new", "first"),
                &request,
                ProcessWatchGeneration(1),
            )
            .expect("first start");
        let (complete, _) = watcher
            .process_event_for_generation(
                &import_process_event("done", "first"),
                &request,
                ProcessWatchGeneration(1),
            )
            .expect("first done");
        assert!(complete);

        watcher.generation = 2;
        watcher.process_id = None;
        assert!(
            watcher
                .process_event_for_generation(
                    &import_process_event("done", "first"),
                    &request,
                    ProcessWatchGeneration(2),
                )
                .is_ok_and(|(complete, observed)| !complete && !observed)
        );
        watcher
            .process_event_for_generation(
                &import_process_event("new", "second"),
                &request,
                ProcessWatchGeneration(2),
            )
            .expect("second start");
        assert!(
            watcher
                .process_event_for_generation(
                    &import_process_event("done", "second"),
                    &request,
                    ProcessWatchGeneration(2),
                )
                .is_ok_and(|(complete, _)| complete)
        );
    }

    #[test]
    fn stream_read_failure_log_redacts_hostile_status_details() {
        let (dispatch, output) = capture();
        tracing::dispatcher::with_default(&dispatch, || {
            log_stream_read_failure(&tonic::Status::internal("HOSTILE_WATCHER_SECRET"));
        });
        let output = output.contents();
        assert!(output.contains("Internal"));
        assert!(output.contains("stream_read"));
        assert!(!output.contains("HOSTILE_WATCHER_SECRET"));
    }

    #[test]
    fn progress_log_redacts_hostile_process_id_and_message() {
        use anytype_rpc::anytype::{event::process, model, model::process as model_process};

        let hostile_id = "HOSTILE_PROCESS_ID\n\u{1b}[31m";
        let hostile_message = "HOSTILE_PROGRESS_MESSAGE\r\n\t\u{7}";
        let event = Event {
            messages: vec![EventMessage {
                space_id: "space-test".to_owned(),
                value: Some(EventValue::ProcessNew(process::New {
                    process: Some(model::Process {
                        id: hostile_id.to_owned(),
                        state: model_process::State::Running as i32,
                        progress: Some(model_process::Progress {
                            total: 10,
                            done: 4,
                            message: hostile_message.to_owned(),
                        }),
                        space_id: "space-test".to_owned(),
                        error: String::new(),
                        message: Some(model_process::Message::Import(model_process::Import {})),
                    }),
                })),
            }],
            context_id: String::new(),
            initiator: None,
            trace_id: String::new(),
        };
        let request =
            ProcessWatchRequest::new(ProcessKind::Import, "space-test").log_progress(true);
        let mut watcher = ProcessWatcher::default();
        let (dispatch, output) = capture();

        tracing::dispatcher::with_default(&dispatch, || {
            watcher
                .process_event(&event, &request)
                .expect("hostile progress event reduces");
        });

        let output = output.contents();
        assert!(output.contains("process_progress"));
        assert!(output.contains("done=4"));
        assert!(output.contains("total=10"));
        assert!(!output.contains("HOSTILE_PROCESS_ID"));
        assert!(!output.contains("HOSTILE_PROGRESS_MESSAGE"));
        assert_eq!(
            watcher.progress().last_process_id.as_deref(),
            Some("redacted")
        );
        assert_eq!(
            watcher.progress().last_progress_message.as_deref(),
            Some("reported")
        );
    }

    fn import_finish_event(space_id: &str, objects_count: i64) -> Event {
        import_finish_event_with_root(space_id, objects_count, "")
    }

    fn import_finish_event_with_root(
        space_id: &str,
        objects_count: i64,
        root_collection_id: &str,
    ) -> Event {
        Event {
            messages: vec![EventMessage {
                space_id: space_id.to_owned(),
                value: Some(EventValue::ImportFinish(
                    anytype_rpc::anytype::event::import::Finish {
                        objects_count,
                        root_collection_id: root_collection_id.to_string(),
                        import_type: 0,
                    },
                )),
            }],
            context_id: String::new(),
            initiator: None,
            trace_id: String::new(),
        }
    }

    #[test]
    fn only_current_server_correlation_can_complete_a_batch() {
        let request = ProcessWatchRequest::new(ProcessKind::Import, "space-test")
            .completion_fallback(ProcessCompletionFallback::ImportFinishEvent);
        let stale = import_finish_event_with_root("space-test", 1, "collection-prior");
        let current = import_finish_event_with_root("space-test", 1, "collection-current");
        assert!(!event_completes_correlation(
            &stale,
            &request,
            "collection-current"
        ));
        assert!(event_completes_correlation(
            &current,
            &request,
            "collection-current"
        ));
    }

    #[test]
    fn import_finish_reducer_records_matching_space() {
        let mut watcher = ProcessWatcher::default();
        let request = ProcessWatchRequest::new(ProcessKind::Import, "space-test")
            .completion_fallback(ProcessCompletionFallback::ImportFinishEvent);
        let (completed, observed) = watcher
            .process_event(&import_finish_event("space-test", 3), &request)
            .expect("constructed matching import-finish event should reduce");

        let progress = watcher.progress();
        assert!(!completed);
        assert!(observed);
        assert_eq!(progress.import_finish_events, 1);
        assert_eq!(progress.import_finish_objects, 3);
    }

    #[test]
    fn import_finish_reducer_ignores_unrelated_space_and_requires_empty_opt_in() {
        let mut watcher = ProcessWatcher::default();
        let strict_request = ProcessWatchRequest::new(ProcessKind::Import, "space-test")
            .completion_fallback(ProcessCompletionFallback::ImportFinishEvent);
        let (_, unrelated_observed) = watcher
            .process_event(&import_finish_event("space-other", 5), &strict_request)
            .expect("constructed unrelated import-finish event should reduce");
        let (_, empty_observed) = watcher
            .process_event(&import_finish_event("", 7), &strict_request)
            .expect("constructed empty-space import-finish event should reduce");
        assert!(!unrelated_observed);
        assert!(!empty_observed);
        assert_eq!(watcher.progress().import_finish_events, 0);

        let fallback_request = strict_request.allow_empty_space_id(true);
        let (_, fallback_observed) = watcher
            .process_event(&import_finish_event("", 7), &fallback_request)
            .expect("constructed opted-in empty-space import-finish event should reduce");
        assert!(fallback_observed);
        assert_eq!(watcher.progress().import_finish_events, 1);
        assert_eq!(watcher.progress().import_finish_objects, 7);
    }

    #[test]
    fn import_finish_reducer_ignores_non_import_request() {
        let mut watcher = ProcessWatcher::default();
        let request = ProcessWatchRequest::new(ProcessKind::Export, "space-test")
            .completion_fallback(ProcessCompletionFallback::ImportFinishEvent);
        let (completed, observed) = watcher
            .process_event(&import_finish_event("space-test", 3), &request)
            .expect("constructed import-finish event should reduce for export request");

        assert!(!completed);
        assert!(!observed);
        assert_eq!(watcher.progress().import_finish_events, 0);
        assert_eq!(watcher.progress().import_finish_objects, 0);
    }

    #[test]
    fn import_finish_reducer_ignores_request_without_fallback() {
        let mut watcher = ProcessWatcher::default();
        let request = ProcessWatchRequest::new(ProcessKind::Import, "space-test");
        let (completed, observed) = watcher
            .process_event(&import_finish_event("space-test", 3), &request)
            .expect("constructed import-finish event should reduce without fallback");

        assert!(!completed);
        assert!(!observed);
        assert_eq!(watcher.progress().import_finish_events, 0);
        assert_eq!(watcher.progress().import_finish_objects, 0);
    }

    #[test]
    fn session_event_setup_uses_only_the_local_setup_budget() {
        let request = session_event_request("test-token").expect("valid session event request");

        assert!(request.metadata().get("grpc-timeout").is_none());
        assert_eq!(
            request.extensions().get::<GrpcCallOptions>(),
            Some(&GrpcCallOptions::stream_setup())
        );
    }
}
