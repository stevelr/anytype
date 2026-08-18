//! # Anytype Chat Streaming (gRPC)
//!
//! Async streaming interface for chat message updates and chat state changes.
//!
//! The stream is backed by `ListenSessionEvents` and chat subscription RPCs.
//! It supports reconnect with per-chat watermarks to reduce missed messages.

use std::{
    collections::{HashMap, HashSet, VecDeque},
    future::Future,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    task::{Context, Poll},
    time::Duration,
};

use anytype_rpc::{
    anytype::rpc::chat::{
        get_messages, subscribe_last_messages, subscribe_to_message_previews, unsubscribe,
        unsubscribe_from_message_previews,
    },
    anytype::{Event, StreamRequest, event::message::Value as EventValue},
    client::{AnytypeGrpcClient, AnytypeGrpcConfig},
    deadline::{
        GrpcCallOptions, GrpcEnclosingDeadline, GrpcStreamDeadline, GrpcStreamError,
        GrpcTimeoutClass, GrpcTimeoutOutcome, GrpcTimeoutPolicy, GrpcTransportProgress,
        with_grpc_call_options,
    },
    error::{AnytypeGrpcError, GrpcControlBoundaryKind},
};
use futures::{Stream, StreamExt};
use tokio::{
    sync::{mpsc, oneshot, watch},
    time::sleep,
};
use tonic::Request;

use crate::{
    Result,
    chats::{
        ChatMessage, ChatState, MessageReaction, chat_message_from_grpc, chat_state_from_grpc,
        message_reactions_from_grpc,
    },
    client::AnytypeClient,
    error::AnytypeError,
    grpc_util::{ensure_error_ok, grpc_status, with_token_request},
};

const DEFAULT_BUFFER_CAPACITY: usize = 256;
const DEFAULT_LAST_MESSAGES_LIMIT: u32 = 1;
const STABLE_EVENT_DELIVERIES: u32 = 2;
const CONTROL_BOUNDARY_METADATA: &str = "x-anytype-control-boundary";

static SUBSCRIPTION_COUNTER: AtomicU64 = AtomicU64::new(1);

/// Builder for chat streaming.
#[derive(Debug, Clone)]
pub struct ChatStreamBuilder {
    client: AnytypeClient,
    chat_ids: Vec<String>,
    previews: bool,
    buffer: usize,
    backoff: BackoffPolicy,
    last_messages_limit: u32,
    enclosing: Option<GrpcEnclosingDeadline>,
}

impl AnytypeClient {
    /// Create a chat stream builder for gRPC chat events.
    ///
    /// ```rust,no_run
    /// use anytype::prelude::*;
    /// use futures::StreamExt;
    /// # async fn example(client: AnytypeClient) -> Result<(), AnytypeError> {
    /// let ChatStreamHandle { mut events, .. } = client
    ///     .chat_stream()
    ///     .subscribe_chat("chat_object_id")
    ///     .build();
    /// while let Some(event) = events.next().await {
    ///     if let ChatEvent::MessageAdded { message, .. } = event {
    ///         println!("{}", message.content.text);
    ///     }
    /// }
    /// # Ok(())
    /// # }
    /// ```
    #[must_use]
    pub fn chat_stream(&self) -> ChatStreamBuilder {
        ChatStreamBuilder::new(self.clone())
    }
}

impl ChatStreamBuilder {
    fn new(client: AnytypeClient) -> Self {
        Self {
            client,
            chat_ids: Vec::new(),
            previews: false,
            buffer: DEFAULT_BUFFER_CAPACITY,
            backoff: BackoffPolicy::default(),
            last_messages_limit: DEFAULT_LAST_MESSAGES_LIMIT,
            enclosing: None,
        }
    }

    /// Subscribe to a chat by object id.
    #[must_use]
    pub fn subscribe_chat(mut self, chat_id: impl Into<String>) -> Self {
        self.chat_ids.push(chat_id.into());
        self
    }

    /// Subscribe to message previews for all chats.
    #[must_use]
    pub fn subscribe_previews(mut self) -> Self {
        self.previews = true;
        self
    }

    /// Set the event buffer capacity.
    #[must_use]
    pub fn buffer(mut self, capacity: usize) -> Self {
        self.buffer = capacity;
        self
    }

    /// Set the reconnect backoff policy.
    #[must_use]
    pub fn backoff(mut self, policy: BackoffPolicy) -> Self {
        self.backoff = policy;
        self
    }

    /// Caps connection, setup, reconnect, and established streaming with one deadline.
    #[must_use]
    pub fn enclosing_deadline(mut self, deadline: GrpcEnclosingDeadline) -> Self {
        self.enclosing = Some(deadline);
        self
    }

    /// Build and start the chat stream worker.
    #[must_use]
    pub fn build(self) -> ChatStreamHandle {
        let (event_tx, event_rx) = mpsc::channel(self.buffer);
        let (control_tx, control_rx) = mpsc::channel(self.buffer);

        let mut worker = ChatStreamWorker::new(self, event_tx, control_rx);

        tokio::spawn(async move {
            worker.run().await;
        });

        ChatStreamHandle {
            events: ChatEventStream { receiver: event_rx },
            control: ChatStreamControl { sender: control_tx },
        }
    }
}

/// Backoff policy for reconnect attempts.
#[derive(Debug, Clone)]
pub struct BackoffPolicy {
    pub initial: Duration,
    pub max: Duration,
    pub factor: f64,
}

impl Default for BackoffPolicy {
    fn default() -> Self {
        Self {
            initial: Duration::from_millis(250),
            max: Duration::from_secs(5),
            factor: 2.0,
        }
    }
}

impl BackoffPolicy {
    #[allow(clippy::cast_precision_loss)]
    fn delay(&self, attempt: u32) -> Duration {
        let initial_ms = self.initial.as_millis() as f64;
        let max_ms = self.max.as_millis() as f64;
        let factor = self.factor.max(1.0);
        let exp = factor.powi(attempt.cast_signed());
        let millis = (initial_ms * exp).min(max_ms).max(initial_ms).round();
        #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
        Duration::from_millis(millis.round() as u64)
    }
}

/// Chat stream handle containing event stream and control interface.
pub struct ChatStreamHandle {
    pub events: ChatEventStream,
    pub control: ChatStreamControl,
}

/// Stream of chat events.
pub struct ChatEventStream {
    receiver: mpsc::Receiver<ChatEvent>,
}

impl Stream for ChatEventStream {
    type Item = ChatEvent;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Pin::new(&mut self.receiver).poll_recv(cx)
    }
}

/// Control interface for managing chat subscriptions.
#[derive(Clone)]
pub struct ChatStreamControl {
    sender: mpsc::Sender<ControlMessage>,
}

impl ChatStreamControl {
    /// Subscribe to a chat while the stream is running.
    pub async fn subscribe_chat(&self, chat_id: impl Into<String>) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        let message = ControlMessage::SubscribeChat {
            chat_id: chat_id.into(),
            respond_to: tx,
        };
        self.sender
            .send(message)
            .await
            .map_err(|_| AnytypeError::Other {
                message: "chat stream control channel closed".to_string(),
            })?;
        rx.await.map_err(|_| AnytypeError::Other {
            message: "chat stream control response dropped".to_string(),
        })?
    }

    /// Unsubscribe from a chat while the stream is running.
    pub async fn unsubscribe_chat(&self, chat_id: impl Into<String>) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        let message = ControlMessage::UnsubscribeChat {
            chat_id: chat_id.into(),
            respond_to: tx,
        };
        self.sender
            .send(message)
            .await
            .map_err(|_| AnytypeError::Other {
                message: "chat stream control channel closed".to_string(),
            })?;
        rx.await.map_err(|_| AnytypeError::Other {
            message: "chat stream control response dropped".to_string(),
        })?
    }

    /// Shut down the chat stream worker.
    pub async fn shutdown(&self) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        let message = ControlMessage::Shutdown { respond_to: tx };
        self.sender
            .send(message)
            .await
            .map_err(|_| AnytypeError::Other {
                message: "chat stream control channel closed".to_string(),
            })?;
        rx.await.map_err(|_| AnytypeError::Other {
            message: "chat stream shutdown response dropped".to_string(),
        })?
    }
}

/// Chat event emitted by the stream.
#[derive(Debug, Clone)]
pub enum ChatEvent {
    MessageAdded {
        chat_id: String,
        message: ChatMessage,
    },
    MessageUpdated {
        chat_id: String,
        message: ChatMessage,
    },
    MessageDeleted {
        chat_id: String,
        message_id: String,
    },
    ReactionsUpdated {
        chat_id: String,
        message_id: String,
        reactions: Vec<MessageReaction>,
    },
    ChatStateUpdated {
        chat_id: String,
        state: ChatState,
    },
    StreamDisconnected,
    StreamResubscribed,
}

enum ControlMessage {
    SubscribeChat {
        chat_id: String,
        respond_to: oneshot::Sender<Result<()>>,
    },
    UnsubscribeChat {
        chat_id: String,
        respond_to: oneshot::Sender<Result<()>>,
    },
    Shutdown {
        respond_to: oneshot::Sender<Result<()>>,
    },
}

enum ConnectedAction {
    Control(Option<ControlMessage>),
    Reader(SessionReaderEvent),
}

enum SessionReaderEvent {
    Event(Box<Event>),
    Boundary(SessionReaderBoundary),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SessionReaderBoundary {
    Closed,
    Transport(tonic::Code),
    QueueSaturated,
}

struct SessionEventReader {
    receiver: mpsc::Receiver<Box<Event>>,
    boundary: watch::Receiver<Option<SessionReaderBoundary>>,
    task: tokio::task::JoinHandle<()>,
}

impl SessionEventReader {
    fn spawn<S>(stream: S) -> Self
    where
        S: Stream<Item = std::result::Result<Event, tonic::Status>> + Send + Unpin + 'static,
    {
        Self::spawn_after(stream, std::future::ready(()))
    }

    fn spawn_after<S, F>(mut stream: S, start: F) -> Self
    where
        S: Stream<Item = std::result::Result<Event, tonic::Status>> + Send + Unpin + 'static,
        F: Future<Output = ()> + Send + 'static,
    {
        let (sender, receiver) = mpsc::channel(1);
        let (boundary_sender, boundary) = watch::channel(None);
        let task = tokio::spawn(async move {
            start.await;
            let boundary = loop {
                match stream.next().await {
                    Some(Ok(event)) => match sender.try_send(Box::new(event)) {
                        Ok(()) => {}
                        Err(mpsc::error::TrySendError::Full(_)) => {
                            break SessionReaderBoundary::QueueSaturated;
                        }
                        Err(mpsc::error::TrySendError::Closed(_)) => return,
                    },
                    None => break SessionReaderBoundary::Closed,
                    Some(Err(status)) => {
                        break SessionReaderBoundary::Transport(status.code());
                    }
                }
            };
            let _ = boundary_sender.send(Some(boundary));
        });
        Self {
            receiver,
            boundary,
            task,
        }
    }

    async fn recv(&mut self) -> SessionReaderEvent {
        if let Ok(event) = self.receiver.try_recv() {
            return SessionReaderEvent::Event(event);
        }
        tokio::select! {
            biased;
            event = self.receiver.recv() => match event {
                Some(event) => SessionReaderEvent::Event(event),
                None => SessionReaderEvent::Boundary(
                    wait_session_boundary(&mut self.boundary).await
                ),
            },
            boundary = wait_session_boundary(&mut self.boundary) => {
                SessionReaderEvent::Boundary(boundary)
            },
        }
    }

    async fn wait_boundary(&mut self) -> SessionReaderBoundary {
        wait_session_boundary(&mut self.boundary).await
    }
}

async fn wait_session_boundary(
    boundary: &mut watch::Receiver<Option<SessionReaderBoundary>>,
) -> SessionReaderBoundary {
    loop {
        if let Some(boundary) = *boundary.borrow_and_update() {
            return boundary;
        }
        if boundary.changed().await.is_err() {
            return SessionReaderBoundary::Closed;
        }
    }
}

impl Drop for SessionEventReader {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl SessionReaderBoundary {
    fn into_stream_error(self) -> GrpcStreamError {
        let (code, message, kind) = match self {
            Self::Closed => (
                tonic::Code::Unavailable,
                "session event stream closed",
                "stream_closed",
            ),
            Self::Transport(code) => (
                code,
                "session event transport failed (details redacted)",
                "transport_lost",
            ),
            Self::QueueSaturated => (
                tonic::Code::ResourceExhausted,
                "session event queue saturated; reconnect required",
                "queue_saturated",
            ),
        };
        let mut metadata = tonic::metadata::MetadataMap::new();
        metadata.insert(
            CONTROL_BOUNDARY_METADATA,
            tonic::metadata::MetadataValue::from_static(kind),
        );
        let status = tonic::Status::with_metadata(code, message, metadata);
        GrpcStreamError::Status(status)
    }
}

#[derive(Debug, Clone)]
#[allow(clippy::struct_field_names)]
struct ChatSubscription {
    chat_id: String,
    sub_id: String,
    last_order_id: Option<String>,
    last_state_id: Option<String>,
}

struct ChatStreamWorker {
    client: AnytypeClient,
    backoff: BackoffPolicy,
    previews: bool,
    last_messages_limit: u32,
    subscriptions: HashMap<String, ChatSubscription>,
    preview_sub_id: Option<String>,
    control_rx: mpsc::Receiver<ControlMessage>,
    event_tx: mpsc::Sender<ChatEvent>,
    pending_events: VecDeque<ChatEvent>,
    enclosing: Option<GrpcEnclosingDeadline>,
    shutdown: bool,
}

impl ChatStreamWorker {
    fn new(
        builder: ChatStreamBuilder,
        event_tx: mpsc::Sender<ChatEvent>,
        control_rx: mpsc::Receiver<ControlMessage>,
    ) -> Self {
        let ChatStreamBuilder {
            client,
            chat_ids,
            previews,
            buffer: _,
            backoff,
            last_messages_limit,
            enclosing,
        } = builder;
        let mut subscriptions = HashMap::new();
        for chat_id in chat_ids {
            let sub_id = next_sub_id("chat");
            subscriptions.insert(
                chat_id.clone(),
                ChatSubscription {
                    chat_id,
                    sub_id,
                    last_order_id: None,
                    last_state_id: None,
                },
            );
        }
        Self {
            client,
            backoff,
            previews,
            last_messages_limit,
            subscriptions,
            preview_sub_id: None,
            control_rx,
            event_tx,
            pending_events: VecDeque::new(),
            enclosing,
            shutdown: false,
        }
    }

    async fn run(&mut self) {
        let client = self.client.clone();
        self.run_with_session_acquisition(async move { GrpcSession::from_client(&client).await })
            .await;
    }

    async fn run_with_session_acquisition<F>(&mut self, acquisition: F)
    where
        F: Future<Output = Result<GrpcSession>>,
    {
        let initial_enclosing = match self.enclosing {
            Some(enclosing) => match GrpcStreamDeadline::new(
                GrpcTimeoutPolicy {
                    stream_idle: None,
                    stream_total_lifetime: None,
                    ..GrpcTimeoutPolicy::default()
                },
                Some(enclosing),
            ) {
                Ok(deadline) => Some(deadline),
                Err(error) => {
                    tracing::warn!("chat stream: invalid enclosing deadline: {error}");
                    return;
                }
            },
            None => None,
        };
        let acquisition = match initial_enclosing.as_ref() {
            Some(deadline) => deadline.phase(acquisition).await,
            None => Ok(acquisition.await),
        };
        let session = match acquisition {
            Err(GrpcStreamError::Deadline(error)) => {
                let _ = self.event_tx.try_send(ChatEvent::StreamDisconnected);
                tracing::warn!("chat stream: {error}");
                return;
            }
            Err(GrpcStreamError::Status(_)) => {
                let _ = self.event_tx.try_send(ChatEvent::StreamDisconnected);
                tracing::warn!("chat stream: session acquisition failed");
                return;
            }
            Ok(Ok(session)) => session,
            Ok(Err(err)) => {
                let _ = self.event_tx.try_send(ChatEvent::StreamDisconnected);
                tracing::error!("chat stream: grpc session unavailable: {err}");
                return;
            }
        };

        let mut attempt = 0;
        let mut was_connected = false;
        let mut stream_deadline: Option<GrpcStreamDeadline> = None;

        loop {
            if self.is_shutdown() {
                break;
            }

            let connect = session.connect();
            let connect = match stream_deadline.as_ref().or(initial_enclosing.as_ref()) {
                Some(deadline) => deadline.phase(connect).await,
                None => Ok(connect.await),
            };
            let grpc = match connect {
                Err(GrpcStreamError::Deadline(error)) => {
                    tracing::warn!("chat stream: {error}");
                    self.shutdown = true;
                    return;
                }
                Err(GrpcStreamError::Status(_)) => {
                    tracing::warn!("chat stream: reconnect phase failed");
                    return;
                }
                Ok(Ok(client)) => client,
                Ok(Err(err)) => {
                    tracing::warn!("chat stream: connect failed: {err}");
                    attempt += 1;
                    let backoff = self.wait_backoff(attempt);
                    match stream_deadline.as_ref().or(initial_enclosing.as_ref()) {
                        Some(deadline) => {
                            if let Err(error) = deadline.phase(backoff).await {
                                tracing::warn!("chat stream: {error}");
                                self.shutdown = true;
                                return;
                            }
                        }
                        None => backoff.await,
                    }
                    continue;
                }
            };

            let workflow_deadline = stream_deadline
                .as_ref()
                .or(initial_enclosing.as_ref())
                .and_then(GrpcStreamDeadline::workflow_deadline);
            let open = open_session_events(&grpc, workflow_deadline);
            let open = match stream_deadline.as_ref().or(initial_enclosing.as_ref()) {
                Some(deadline) => deadline.phase(open).await,
                None => Ok(open.await),
            };
            let (stream, progress) = match open {
                Err(GrpcStreamError::Deadline(error)) => {
                    tracing::warn!("chat stream: {error}");
                    self.shutdown = true;
                    return;
                }
                Err(GrpcStreamError::Status(_)) => {
                    tracing::warn!("chat stream: reopen phase failed");
                    return;
                }
                Ok(Ok(stream)) => stream,
                Ok(Err(err)) => {
                    tracing::warn!("chat stream: listen failed: {err}");
                    attempt += 1;
                    let backoff = self.wait_backoff(attempt);
                    match stream_deadline.as_ref().or(initial_enclosing.as_ref()) {
                        Some(deadline) => {
                            if let Err(error) = deadline.phase(backoff).await {
                                tracing::warn!("chat stream: {error}");
                                self.shutdown = true;
                                return;
                            }
                        }
                        None => backoff.await,
                    }
                    continue;
                }
            };

            match stream_deadline.as_mut() {
                Some(deadline) => {
                    deadline.set_transport_progress(progress);
                    if let Err(error) = deadline.reset_idle() {
                        tracing::warn!("chat stream: invalid idle deadline: {error}");
                        return;
                    }
                }
                None => match GrpcStreamDeadline::new(session.grpc_timeouts, self.enclosing) {
                    Ok(deadline) => {
                        stream_deadline = Some(deadline.with_transport_progress(progress));
                    }
                    Err(error) => {
                        tracing::warn!("chat stream: invalid deadline policy: {error}");
                        return;
                    }
                },
            }
            let mut reader = SessionEventReader::spawn(stream);

            if !self.pending_events.is_empty() {
                let pending =
                    bounded_connected_work(stream_deadline.as_mut(), self.deliver_pending_events())
                        .await;
                if let Err(error) = pending {
                    let disconnected = self.connected_failure(error);
                    if !disconnected || self.is_shutdown() {
                        return;
                    }
                    let _ = self.event_tx.try_send(ChatEvent::StreamDisconnected);
                    was_connected = true;
                    attempt = attempt.saturating_add(1);
                    if let Err(error) = self
                        .wait_reconnect_backoff(attempt, stream_deadline.as_ref())
                        .await
                    {
                        tracing::warn!("chat stream: {error}");
                        self.shutdown = true;
                        return;
                    }
                    continue;
                }
            }

            let workflow_deadline = stream_deadline
                .as_ref()
                .and_then(GrpcStreamDeadline::workflow_deadline);
            let resubscribe = async {
                if was_connected {
                    let _ = self.event_tx.send(ChatEvent::StreamResubscribed).await;
                }
                self.resubscribe(&grpc, was_connected, workflow_deadline)
                    .await
            };
            let resubscribe =
                bounded_reader_work(Some(&mut reader), stream_deadline.as_mut(), resubscribe).await;
            match resubscribe {
                Err(GrpcStreamError::Deadline(error)) => {
                    tracing::warn!("chat stream: {error}");
                    self.shutdown = true;
                    return;
                }
                Err(GrpcStreamError::Status(_)) => {
                    tracing::warn!("chat stream: resubscribe stream boundary reached");
                    let _ = self.event_tx.try_send(ChatEvent::StreamDisconnected);
                    was_connected = true;
                    attempt = attempt.saturating_add(1);
                    if let Err(error) = self
                        .wait_reconnect_backoff(attempt, stream_deadline.as_ref())
                        .await
                    {
                        tracing::warn!("chat stream: {error}");
                        self.shutdown = true;
                        return;
                    }
                    continue;
                }
                Ok(Err(err)) => tracing::warn!("chat stream: resubscribe failed: {err}"),
                Ok(Ok(())) => {}
            }

            let disconnected = self
                .connected_loop(grpc, &mut reader, &mut attempt, stream_deadline.as_mut())
                .await;
            if disconnected && !self.is_shutdown() {
                let _ = self.event_tx.try_send(ChatEvent::StreamDisconnected);
                was_connected = true;
                attempt = attempt.saturating_add(1);
                if let Err(error) = self
                    .wait_reconnect_backoff(attempt, stream_deadline.as_ref())
                    .await
                {
                    tracing::warn!("chat stream: {error}");
                    self.shutdown = true;
                    return;
                }
            }
        }
    }

    fn is_shutdown(&self) -> bool {
        self.shutdown || self.event_tx.is_closed()
    }

    async fn wait_backoff(&mut self, attempt: u32) {
        let delay = self.backoff.delay(attempt);
        tokio::select! {
            () = sleep(delay) => {},
            message = self.control_rx.recv(), if !self.control_rx.is_closed() => {
                if let Some(message) = message {
                    self.handle_control_message(message, None).await;
                }
            }
        }
    }

    async fn wait_reconnect_backoff(
        &mut self,
        attempt: u32,
        deadline: Option<&GrpcStreamDeadline>,
    ) -> std::result::Result<(), GrpcStreamError> {
        let backoff = self.wait_backoff(attempt);
        match deadline {
            Some(deadline) => deadline.phase(backoff).await,
            None => {
                backoff.await;
                Ok(())
            }
        }
    }

    async fn connected_loop(
        &mut self,
        grpc: AnytypeGrpcClient,
        reader: &mut SessionEventReader,
        reconnect_attempt: &mut u32,
        mut deadline: Option<&mut GrpcStreamDeadline>,
    ) -> bool {
        let mut stable_deliveries = 0_u32;
        loop {
            if self.is_shutdown() {
                return false;
            }
            let action = async {
                tokio::select! {
                    message = self.control_rx.recv(), if !self.control_rx.is_closed() => {
                        ConnectedAction::Control(message)
                    }
                    message = reader.recv() => ConnectedAction::Reader(message),
                }
            };
            let action = bounded_connected_work(deadline.as_deref_mut(), action).await;
            match action {
                Err(error) => return self.connected_failure(error),
                Ok(ConnectedAction::Control(Some(message))) => {
                    let result = self
                        .handle_connected_control_message(
                            message,
                            &grpc,
                            Some(reader),
                            deadline.as_deref_mut(),
                        )
                        .await;
                    if let Err(error) = result {
                        return self.connected_failure(error);
                    }
                    if self.is_shutdown() {
                        return false;
                    }
                }
                Ok(ConnectedAction::Control(None)) => {}
                Ok(ConnectedAction::Reader(SessionReaderEvent::Event(event))) => {
                    if let Some(deadline) = deadline.as_deref_mut()
                        && let Err(error) = deadline.observe_decoded_message()
                    {
                        tracing::warn!("chat stream: invalid idle deadline: {error}");
                        self.shutdown = true;
                        return false;
                    }
                    let work = self.handle_event(*event);
                    let result = bounded_connected_work(deadline.as_deref_mut(), work).await;
                    match result {
                        Err(error) => return self.connected_failure(error),
                        Ok(Err(error)) => {
                            tracing::warn!("chat stream: invalid event: {}", error.diagnostic());
                        }
                        Ok(Ok(delivered)) => {
                            if delivered {
                                stable_deliveries = stable_deliveries.saturating_add(1);
                                if stable_deliveries >= STABLE_EVENT_DELIVERIES {
                                    *reconnect_attempt = 0;
                                }
                            }
                        }
                    }
                }
                Ok(ConnectedAction::Reader(SessionReaderEvent::Boundary(boundary))) => {
                    return self.connected_failure(boundary.into_stream_error());
                }
            }
        }
    }

    async fn handle_connected_control_message(
        &mut self,
        message: ControlMessage,
        grpc: &AnytypeGrpcClient,
        reader: Option<&mut SessionEventReader>,
        deadline: Option<&mut GrpcStreamDeadline>,
    ) -> std::result::Result<(), GrpcStreamError> {
        match message {
            ControlMessage::SubscribeChat {
                chat_id,
                respond_to,
            } => {
                let work = self.subscribe_chat(chat_id, Some(grpc));
                bounded_control_mutation(reader, deadline, respond_to, work).await
            }
            ControlMessage::UnsubscribeChat {
                chat_id,
                respond_to,
            } => {
                let work = self.unsubscribe_chat(&chat_id, Some(grpc));
                bounded_control_mutation(reader, deadline, respond_to, work).await
            }
            ControlMessage::Shutdown { respond_to } => {
                let work = self.shutdown(Some(grpc));
                bounded_control_mutation(reader, deadline, respond_to, work).await
            }
        }
    }

    fn connected_failure(&mut self, error: GrpcStreamError) -> bool {
        match error {
            GrpcStreamError::Deadline(error) => {
                tracing::warn!("chat stream: {error}");
                if error.class == GrpcTimeoutClass::StreamLifetime {
                    self.shutdown = true;
                    false
                } else {
                    true
                }
            }
            GrpcStreamError::Status(_) => {
                tracing::warn!("chat stream: event transport failed");
                true
            }
        }
    }

    async fn handle_event(&mut self, event: Event) -> Result<bool> {
        let active_sub_ids = self.active_sub_ids();
        let chat_id = event.context_id.clone();
        if chat_id.is_empty() {
            return Ok(false);
        }

        let events = chat_events_from_event(&chat_id, event, &active_sub_ids)?;
        self.pending_events.extend(events);
        Ok(self.deliver_pending_events().await)
    }

    async fn deliver_pending_events(&mut self) -> bool {
        let mut delivered = false;
        while let Some(chat_event) = self.pending_events.front().cloned() {
            if self.event_tx.send(chat_event.clone()).await.is_err() {
                break;
            }
            self.update_watermark(&chat_event);
            self.pending_events.pop_front();
            delivered = true;
        }
        delivered
    }

    async fn handle_control_message(
        &mut self,
        message: ControlMessage,
        grpc: Option<&AnytypeGrpcClient>,
    ) {
        match message {
            ControlMessage::SubscribeChat {
                chat_id,
                respond_to,
            } => {
                let result = self.subscribe_chat(chat_id, grpc).await;
                let _ = respond_to.send(result);
            }
            ControlMessage::UnsubscribeChat {
                chat_id,
                respond_to,
            } => {
                let result = self.unsubscribe_chat(&chat_id, grpc).await;
                let _ = respond_to.send(result);
            }
            ControlMessage::Shutdown { respond_to } => {
                let result = self.shutdown(grpc).await;
                let _ = respond_to.send(result);
            }
        }
    }

    async fn resubscribe(
        &mut self,
        grpc: &AnytypeGrpcClient,
        is_reconnect: bool,
        enclosing: Option<GrpcEnclosingDeadline>,
    ) -> Result<()> {
        if self.previews {
            if self.preview_sub_id.is_none() {
                self.preview_sub_id = Some(next_sub_id("preview"));
            }
            let sub_id = self.preview_sub_id.clone().unwrap_or_default();
            let response = subscribe_previews(grpc, &sub_id, enclosing).await?;
            if !is_reconnect {
                for preview in response.previews {
                    if let Err(error) = self.emit_preview(preview).await {
                        tracing::warn!("chat stream: invalid preview: {}", error.diagnostic());
                    }
                }
            }
        }

        // Collect catch-up work to avoid borrowing self mutably twice
        let mut catch_ups: Vec<(String, String)> = Vec::new();

        for subscription in self.subscriptions.values_mut() {
            let response = call_subscribe_last_messages(
                grpc,
                &subscription.chat_id,
                &subscription.sub_id,
                self.last_messages_limit,
                enclosing,
            )
            .await?;

            if let Some(state) = response.chat_state.as_ref() {
                let state = chat_state_from_grpc(state);
                let should_emit = subscription
                    .last_state_id
                    .as_deref()
                    .is_none_or(|current| current != state.last_state_id);
                if should_emit {
                    let state_id = state.last_state_id.clone();
                    if self
                        .event_tx
                        .send(ChatEvent::ChatStateUpdated {
                            chat_id: subscription.chat_id.clone(),
                            state,
                        })
                        .await
                        .is_ok()
                    {
                        subscription.last_state_id = Some(state_id);
                    }
                }
            }

            if subscription.last_order_id.is_none() {
                for message in response.messages {
                    let message = chat_message_from_grpc(message)?;
                    let order_id = message.order_id.clone();
                    if self
                        .event_tx
                        .send(ChatEvent::MessageAdded {
                            chat_id: subscription.chat_id.clone(),
                            message,
                        })
                        .await
                        .is_err()
                    {
                        break;
                    }
                    subscription.last_order_id = Some(order_id);
                }
            } else if let Some(order_id) = subscription.last_order_id.clone() {
                catch_ups.push((subscription.chat_id.clone(), order_id));
            }
        }

        for (chat_id, order_id) in catch_ups {
            let _ = self
                .catch_up_messages(grpc, &chat_id, &order_id, enclosing)
                .await;
        }

        Ok(())
    }

    async fn subscribe_chat(
        &mut self,
        chat_id: String,
        grpc: Option<&AnytypeGrpcClient>,
    ) -> Result<()> {
        if self.subscriptions.contains_key(&chat_id) {
            return Ok(());
        }

        let sub_id = next_sub_id("chat");
        let mut subscription = ChatSubscription {
            chat_id: chat_id.clone(),
            sub_id,
            last_order_id: None,
            last_state_id: None,
        };

        if let Some(grpc) = grpc {
            let response = call_subscribe_last_messages(
                grpc,
                &subscription.chat_id,
                &subscription.sub_id,
                self.last_messages_limit,
                None,
            )
            .await?;
            if let Some(state) = response.chat_state.as_ref() {
                let state = chat_state_from_grpc(state);
                let state_id = state.last_state_id.clone();
                if self
                    .event_tx
                    .send(ChatEvent::ChatStateUpdated {
                        chat_id: subscription.chat_id.clone(),
                        state,
                    })
                    .await
                    .is_ok()
                {
                    subscription.last_state_id = Some(state_id);
                }
            }
            for message in response.messages {
                let message = chat_message_from_grpc(message)?;
                let order_id = message.order_id.clone();
                if self
                    .event_tx
                    .send(ChatEvent::MessageAdded {
                        chat_id: subscription.chat_id.clone(),
                        message,
                    })
                    .await
                    .is_err()
                {
                    break;
                }
                subscription.last_order_id = Some(order_id);
            }
        }

        self.subscriptions.insert(chat_id, subscription);
        Ok(())
    }

    async fn unsubscribe_chat(
        &mut self,
        chat_id: &str,
        grpc: Option<&AnytypeGrpcClient>,
    ) -> Result<()> {
        let Some(subscription) = self.subscriptions.get(chat_id).cloned() else {
            return Ok(());
        };

        if let Some(grpc) = grpc {
            unsubscribe_chat(grpc, &subscription.chat_id, &subscription.sub_id).await?;
        }
        self.subscriptions.remove(chat_id);
        Ok(())
    }

    async fn shutdown(&mut self, grpc: Option<&AnytypeGrpcClient>) -> Result<()> {
        if let Some(grpc) = grpc {
            if let Some(preview_sub_id) = self.preview_sub_id.clone() {
                unsubscribe_previews(grpc, &preview_sub_id).await?;
                self.preview_sub_id = None;
            }
            for subscription in self.subscriptions.values() {
                unsubscribe_chat(grpc, &subscription.chat_id, &subscription.sub_id).await?;
            }
        }
        self.shutdown = true;
        Ok(())
    }

    async fn catch_up_messages(
        &mut self,
        grpc: &AnytypeGrpcClient,
        chat_id: &str,
        after_order_id: &str,
        enclosing: Option<GrpcEnclosingDeadline>,
    ) -> Result<()> {
        let mut cursor = after_order_id.to_string();
        loop {
            let response = get_messages_after(grpc, chat_id, &cursor, enclosing).await?;
            if response.messages.is_empty() {
                if let Some(state) = response.chat_state.as_ref() {
                    let state = chat_state_from_grpc(state);
                    let state_id = state.last_state_id.clone();
                    if self
                        .event_tx
                        .send(ChatEvent::ChatStateUpdated {
                            chat_id: chat_id.to_string(),
                            state,
                        })
                        .await
                        .is_ok()
                        && let Some(subscription) = self.subscriptions.get_mut(chat_id)
                    {
                        subscription.last_state_id = Some(state_id);
                    }
                }
                break;
            }
            for message in response.messages {
                let message = chat_message_from_grpc(message)?;
                let order_id = message.order_id.clone();
                if self
                    .event_tx
                    .send(ChatEvent::MessageAdded {
                        chat_id: chat_id.to_string(),
                        message,
                    })
                    .await
                    .is_err()
                {
                    return Ok(());
                }
                cursor = order_id.clone();
                if let Some(subscription) = self.subscriptions.get_mut(chat_id) {
                    subscription.last_order_id = Some(order_id);
                }
            }
        }
        Ok(())
    }

    async fn emit_preview(
        &self,
        preview: subscribe_to_message_previews::response::ChatPreview,
    ) -> Result<()> {
        if let Some(message) = preview.message {
            let message = chat_message_from_grpc(message)?;
            let _ = self
                .event_tx
                .send(ChatEvent::MessageAdded {
                    chat_id: preview.chat_object_id.clone(),
                    message,
                })
                .await;
        }
        if let Some(state) = preview.state.as_ref() {
            let state = chat_state_from_grpc(state);
            let _ = self
                .event_tx
                .send(ChatEvent::ChatStateUpdated {
                    chat_id: preview.chat_object_id,
                    state,
                })
                .await;
        }
        Ok(())
    }

    fn update_watermark(&mut self, event: &ChatEvent) {
        let (chat_id, order_id, state_id) = match event {
            ChatEvent::MessageAdded { chat_id, message }
            | ChatEvent::MessageUpdated { chat_id, message } => {
                (chat_id, Some(&message.order_id), Some(&message.state_id))
            }
            ChatEvent::ChatStateUpdated { chat_id, state } => {
                (chat_id, None, Some(&state.last_state_id))
            }
            _ => return,
        };

        if let Some(subscription) = self.subscriptions.get_mut(chat_id) {
            if let Some(order_id) = order_id {
                let should_update = subscription
                    .last_order_id
                    .as_ref()
                    .is_none_or(|current| order_id > current);
                if should_update {
                    subscription.last_order_id = Some(order_id.clone());
                }
            }
            if let Some(state_id) = state_id {
                subscription.last_state_id = Some(state_id.clone());
            }
        }
    }

    fn active_sub_ids(&self) -> HashSet<String> {
        let mut ids = HashSet::new();
        for subscription in self.subscriptions.values() {
            ids.insert(subscription.sub_id.clone());
        }
        if let Some(preview_id) = &self.preview_sub_id {
            ids.insert(preview_id.clone());
        }
        ids
    }
}

#[derive(Clone)]
struct GrpcSession {
    endpoint: String,
    token: String,
    grpc_timeouts: GrpcTimeoutPolicy,
}

impl GrpcSession {
    async fn from_client(client: &AnytypeClient) -> Result<Self> {
        let grpc = client.grpc_client().await?;
        Ok(Self {
            endpoint: grpc.get_endpoint().to_string(),
            token: grpc.token().to_string(),
            grpc_timeouts: grpc.grpc_timeouts(),
        })
    }

    async fn connect(&self) -> Result<AnytypeGrpcClient> {
        let config =
            AnytypeGrpcConfig::new(self.endpoint.clone()).grpc_timeouts(self.grpc_timeouts);
        AnytypeGrpcClient::from_token(&config, self.token.clone())
            .await
            .map_err(AnytypeError::from)
    }
}

fn next_sub_id(prefix: &str) -> String {
    let id = SUBSCRIPTION_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{prefix}-{id}")
}

fn chat_events_from_event(
    chat_id: &str,
    event: Event,
    active_sub_ids: &HashSet<String>,
) -> Result<Vec<ChatEvent>> {
    let mut events = Vec::new();
    for message in event.messages {
        let Some(value) = message.value else {
            continue;
        };
        match value {
            EventValue::ChatAdd(add) => {
                if should_emit(&add.sub_ids, active_sub_ids)
                    && let Some(message) = add.message
                {
                    events.push(ChatEvent::MessageAdded {
                        chat_id: chat_id.to_string(),
                        message: chat_message_from_grpc(message)?,
                    });
                }
            }
            EventValue::ChatUpdate(update) => {
                if should_emit(&update.sub_ids, active_sub_ids)
                    && let Some(message) = update.message
                {
                    events.push(ChatEvent::MessageUpdated {
                        chat_id: chat_id.to_string(),
                        message: chat_message_from_grpc(message)?,
                    });
                }
            }
            EventValue::ChatDelete(delete) => {
                if should_emit(&delete.sub_ids, active_sub_ids) {
                    events.push(ChatEvent::MessageDeleted {
                        chat_id: chat_id.to_string(),
                        message_id: delete.id,
                    });
                }
            }
            EventValue::ChatUpdateReactions(update) => {
                if should_emit(&update.sub_ids, active_sub_ids) {
                    let reactions = update
                        .reactions
                        .as_ref()
                        .map(message_reactions_from_grpc)
                        .unwrap_or_default();
                    events.push(ChatEvent::ReactionsUpdated {
                        chat_id: chat_id.to_string(),
                        message_id: update.id,
                        reactions,
                    });
                }
            }
            EventValue::ChatStateUpdate(update) => {
                if should_emit(&update.sub_ids, active_sub_ids)
                    && let Some(state) = update.state.as_ref()
                {
                    events.push(ChatEvent::ChatStateUpdated {
                        chat_id: chat_id.to_string(),
                        state: chat_state_from_grpc(state),
                    });
                }
            }
            _ => {}
        }
    }
    Ok(events)
}

fn should_emit(sub_ids: &[String], active_sub_ids: &HashSet<String>) -> bool {
    if sub_ids.is_empty() {
        return true;
    }
    sub_ids.iter().any(|id| active_sub_ids.contains(id))
}

async fn open_session_events(
    grpc: &AnytypeGrpcClient,
    enclosing: Option<GrpcEnclosingDeadline>,
) -> Result<(tonic::Streaming<Event>, Option<GrpcTransportProgress>)> {
    let request = StreamRequest {
        token: grpc.token().to_string(),
    };
    let request = with_token_request(Request::new(request), grpc.token())?;
    let options = stream_options(GrpcCallOptions::stream_setup(), enclosing);
    let request = with_grpc_call_options(request, options);
    let mut response = grpc
        .client_commands()
        .listen_session_events(request)
        .await
        .map_err(grpc_status)?;
    let progress = response.extensions_mut().remove::<GrpcTransportProgress>();
    Ok((response.into_inner(), progress))
}

async fn call_subscribe_last_messages(
    grpc: &AnytypeGrpcClient,
    chat_id: &str,
    sub_id: &str,
    limit: u32,
    enclosing: Option<GrpcEnclosingDeadline>,
) -> Result<subscribe_last_messages::Response> {
    let request = subscribe_last_messages::Request {
        chat_object_id: chat_id.to_string(),
        limit: limit.cast_signed(),
        sub_id: sub_id.to_string(),
    };
    let request = with_token_request(Request::new(request), grpc.token())?;
    let options = stream_options(GrpcCallOptions::ordinary_mutation(), enclosing);
    let request = with_grpc_call_options(request, options);
    let response = grpc
        .client_commands()
        .chat_subscribe_last_messages(request)
        .await
        .map_err(grpc_status)?
        .into_inner();
    ensure_error_ok(response.error.as_ref(), "chat subscribe last messages")?;
    Ok(response)
}

async fn unsubscribe_chat(grpc: &AnytypeGrpcClient, chat_id: &str, sub_id: &str) -> Result<()> {
    let request = unsubscribe::Request {
        chat_object_id: chat_id.to_string(),
        sub_id: sub_id.to_string(),
    };
    let request = with_token_request(Request::new(request), grpc.token())?;
    let request = with_grpc_call_options(request, GrpcCallOptions::cleanup());
    let response = grpc
        .client_commands()
        .chat_unsubscribe(request)
        .await
        .map_err(grpc_status)?
        .into_inner();
    ensure_error_ok(response.error.as_ref(), "chat unsubscribe")?;
    Ok(())
}

async fn subscribe_previews(
    grpc: &AnytypeGrpcClient,
    sub_id: &str,
    enclosing: Option<GrpcEnclosingDeadline>,
) -> Result<subscribe_to_message_previews::Response> {
    let request = subscribe_to_message_previews::Request {
        sub_id: sub_id.to_string(),
    };
    let request = with_token_request(Request::new(request), grpc.token())?;
    let options = stream_options(GrpcCallOptions::ordinary_mutation(), enclosing);
    let request = with_grpc_call_options(request, options);
    let response = grpc
        .client_commands()
        .chat_subscribe_to_message_previews(request)
        .await
        .map_err(grpc_status)?
        .into_inner();
    ensure_error_ok(response.error.as_ref(), "chat subscribe previews")?;
    Ok(response)
}

async fn unsubscribe_previews(grpc: &AnytypeGrpcClient, sub_id: &str) -> Result<()> {
    let request = unsubscribe_from_message_previews::Request {
        sub_id: sub_id.to_string(),
    };
    let request = with_token_request(Request::new(request), grpc.token())?;
    let request = with_grpc_call_options(request, GrpcCallOptions::cleanup());
    let response = grpc
        .client_commands()
        .chat_unsubscribe_from_message_previews(request)
        .await
        .map_err(grpc_status)?
        .into_inner();
    ensure_error_ok(response.error.as_ref(), "chat unsubscribe previews")?;
    Ok(())
}

async fn get_messages_after(
    grpc: &AnytypeGrpcClient,
    chat_id: &str,
    after_order_id: &str,
    enclosing: Option<GrpcEnclosingDeadline>,
) -> Result<get_messages::Response> {
    let request = get_messages::Request {
        chat_object_id: chat_id.to_string(),
        after_order_id: after_order_id.to_string(),
        before_order_id: String::new(),
        limit: 100,
        include_boundary: false,
    };
    let request = with_token_request(Request::new(request), grpc.token())?;
    let options = stream_options(GrpcCallOptions::ordinary_read(), enclosing);
    let request = with_grpc_call_options(request, options);
    let response = grpc
        .client_commands()
        .chat_get_messages(request)
        .await
        .map_err(grpc_status)?
        .into_inner();
    ensure_error_ok(response.error.as_ref(), "chat get messages")?;
    Ok(response)
}

fn stream_options(
    options: GrpcCallOptions,
    enclosing: Option<GrpcEnclosingDeadline>,
) -> GrpcCallOptions {
    enclosing.map_or(options, |deadline| options.enclosing(deadline))
}

async fn bounded_connected_work<T, F>(
    deadline: Option<&mut GrpcStreamDeadline>,
    work: F,
) -> std::result::Result<T, GrpcStreamError>
where
    F: Future<Output = T>,
{
    match deadline {
        Some(deadline) => deadline.established_phase(work).await,
        None => Ok(work.await),
    }
}

async fn bounded_reader_work<T, F>(
    reader: Option<&mut SessionEventReader>,
    deadline: Option<&mut GrpcStreamDeadline>,
    work: F,
) -> std::result::Result<T, GrpcStreamError>
where
    F: Future<Output = T>,
{
    match reader {
        Some(reader) => {
            let bounded = bounded_connected_work(deadline, work);
            tokio::select! {
                biased;
                boundary = reader.wait_boundary() => Err(boundary.into_stream_error()),
                result = bounded => result,
            }
        }
        None => bounded_connected_work(deadline, work).await,
    }
}

struct PossibleDispatch<F> {
    inner: Pin<Box<F>>,
    started: Arc<AtomicBool>,
}

impl<F> PossibleDispatch<F> {
    fn new(inner: F, started: Arc<AtomicBool>) -> Self {
        Self {
            inner: Box::pin(inner),
            started,
        }
    }
}

impl<F> Future for PossibleDispatch<F>
where
    F: Future,
{
    type Output = F::Output;

    fn poll(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        self.started.store(true, Ordering::Release);
        self.inner.as_mut().poll(context)
    }
}

async fn bounded_control_mutation<F>(
    reader: Option<&mut SessionEventReader>,
    deadline: Option<&mut GrpcStreamDeadline>,
    respond_to: oneshot::Sender<Result<()>>,
    work: F,
) -> std::result::Result<(), GrpcStreamError>
where
    F: Future<Output = Result<()>>,
{
    let started = Arc::new(AtomicBool::new(false));
    let tracked = PossibleDispatch::new(work, started.clone());
    match bounded_reader_work(reader, deadline, tracked).await {
        Ok(result) => {
            let _ = respond_to.send(result);
            Ok(())
        }
        Err(error) => {
            let possible_dispatch = started.load(Ordering::Acquire);
            let _ = respond_to.send(Err(control_mutation_failure(&error, possible_dispatch)));
            Err(error)
        }
    }
}

fn control_mutation_failure(error: &GrpcStreamError, possible_dispatch: bool) -> AnytypeError {
    match error {
        GrpcStreamError::Deadline(source) => {
            let mut source = *source;
            source.outcome = if possible_dispatch {
                GrpcTimeoutOutcome::MutationIndeterminate
            } else {
                GrpcTimeoutOutcome::ReadAborted
            };
            AnytypeError::Grpc {
                source: AnytypeGrpcError::Deadline { source },
            }
        }
        GrpcStreamError::Status(status) => {
            let kind = match status
                .metadata()
                .get(CONTROL_BOUNDARY_METADATA)
                .and_then(|value| value.to_str().ok())
            {
                Some("queue_saturated") => GrpcControlBoundaryKind::QueueSaturated,
                Some("stream_closed") => GrpcControlBoundaryKind::StreamClosed,
                _ => GrpcControlBoundaryKind::TransportLost,
            };
            let outcome = if possible_dispatch {
                GrpcTimeoutOutcome::MutationIndeterminate
            } else {
                GrpcTimeoutOutcome::ReadAborted
            };
            AnytypeError::Grpc {
                source: AnytypeGrpcError::ControlBoundary { kind, outcome },
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, atomic::AtomicBool};

    use anytype_rpc::{anytype::event::Message as EventMessage, model};
    use prost::Message as _;
    use tonic::codec::Codec as _;

    use super::*;
    use crate::client::ClientConfig;

    fn test_client(label: &str) -> (AnytypeClient, std::path::PathBuf) {
        let id = SUBSCRIPTION_COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("anytype-chat-stream-{label}-{id}.db"));
        let app_name = format!("chat-stream-{label}-{id}");
        let mut config = ClientConfig::default().app_name(&app_name);
        config.keystore = Some(format!("file:path={}", path.display()));
        config.keystore_service = Some(format!("chat-stream-{label}-{id}"));
        let client = AnytypeClient::with_config(config).expect("construct test client");
        (client, path)
    }

    fn remove_test_keystore(path: &std::path::Path) {
        for suffix in ["", "-shm", "-wal"] {
            let _ = std::fs::remove_file(format!("{}{}", path.display(), suffix));
        }
    }

    async fn stalled_control_rpc() -> Result<()> {
        std::future::pending().await
    }

    async fn inert_grpc_client() -> (AnytypeGrpcClient, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind inert gRPC listener");
        let address = listener.local_addr().expect("inert listener address");
        let server = tokio::spawn(async move {
            if let Ok((socket, _)) = listener.accept().await {
                let _socket = socket;
                std::future::pending::<()>().await;
            }
        });
        let config = AnytypeGrpcConfig::new(format!("http://{address}"));
        let client = AnytypeGrpcClient::from_token(&config, "test-token")
            .await
            .expect("connect inert gRPC client");
        (client, server)
    }

    fn raw_event_stream(events: impl IntoIterator<Item = Event>) -> tonic::Streaming<Event> {
        let mut raw = Vec::new();
        for event in events {
            let payload = event.encode_to_vec();
            raw.push(0);
            raw.extend_from_slice(&(payload.len() as u32).to_be_bytes());
            raw.extend_from_slice(&payload);
        }
        let body = http_body_util::Full::new(tonic::codegen::Bytes::from(raw));
        let mut codec = tonic_prost::ProstCodec::<Event, Event>::default();
        tonic::Streaming::new_response(
            codec.decoder(),
            body,
            tonic::codegen::http::StatusCode::OK,
            None,
            None,
        )
    }

    fn raw_saturated_stream() -> tonic::Streaming<Event> {
        let event = Event {
            messages: Vec::new(),
            context_id: "saturation-test".to_owned(),
            initiator: None,
            trace_id: String::new(),
        };
        raw_event_stream([event.clone(), event])
    }

    fn raw_saturated_event_stream(event: Event) -> tonic::Streaming<Event> {
        raw_event_stream([event.clone(), event])
    }

    fn raw_saturated_session_reader() -> SessionEventReader {
        SessionEventReader::spawn(raw_saturated_stream())
    }

    fn paused_raw_saturated_session_reader() -> (SessionEventReader, oneshot::Sender<()>) {
        let (start, started) = oneshot::channel();
        let reader = SessionEventReader::spawn_after(raw_saturated_stream(), async move {
            let _ = started.await;
        });
        (reader, start)
    }

    fn assert_queue_saturated(error: GrpcStreamError) {
        assert!(matches!(
            error,
            GrpcStreamError::Status(status) if status.code() == tonic::Code::ResourceExhausted
        ));
    }

    fn message_event(chat_id: &str, sub_id: &str, order_id: &str) -> Event {
        Event {
            messages: vec![EventMessage {
                space_id: String::new(),
                value: Some(EventValue::ChatAdd(
                    anytype_rpc::anytype::event::chat::Add {
                        id: "message-1".to_owned(),
                        order_id: order_id.to_owned(),
                        after_order_id: String::new(),
                        message: Some(model::ChatMessage {
                            id: "message-1".to_owned(),
                            order_id: order_id.to_owned(),
                            state_id: "message-state-1".to_owned(),
                            ..Default::default()
                        }),
                        sub_ids: vec![sub_id.to_owned()],
                        dependencies: Vec::new(),
                    },
                )),
            }],
            context_id: chat_id.to_owned(),
            initiator: None,
            trace_id: String::new(),
        }
    }

    fn state_event(chat_id: &str, sub_id: &str, state_id: &str) -> Event {
        Event {
            messages: vec![EventMessage {
                space_id: String::new(),
                value: Some(EventValue::ChatStateUpdate(
                    anytype_rpc::anytype::event::chat::UpdateState {
                        state: Some(model::ChatState {
                            last_state_id: state_id.to_owned(),
                            ..Default::default()
                        }),
                        sub_ids: vec![sub_id.to_owned()],
                    },
                )),
            }],
            context_id: chat_id.to_owned(),
            initiator: None,
            trace_id: String::new(),
        }
    }

    #[tokio::test]
    async fn queued_chat_event_is_delivered_before_ready_terminal_boundary() {
        let (client, path) = test_client("queued-event-before-boundary");
        let builder = ChatStreamBuilder::new(client).subscribe_chat("chat-1");
        let (event_tx, mut event_rx) = mpsc::channel(8);
        let (_control_tx, control_rx) = mpsc::channel(1);
        let mut worker = ChatStreamWorker::new(builder, event_tx, control_rx);
        let sub_id = worker
            .subscriptions
            .get("chat-1")
            .expect("initial subscription")
            .sub_id
            .clone();
        let mut event = message_event("chat-1", &sub_id, "0001");
        event.messages.extend([
            EventMessage {
                space_id: String::new(),
                value: Some(EventValue::ChatDelete(
                    anytype_rpc::anytype::event::chat::Delete {
                        id: "deleted-message".to_owned(),
                        sub_ids: vec![sub_id.clone()],
                    },
                )),
            },
            EventMessage {
                space_id: String::new(),
                value: Some(EventValue::ChatUpdateReactions(
                    anytype_rpc::anytype::event::chat::UpdateReactions {
                        id: "reacted-message".to_owned(),
                        reactions: None,
                        sub_ids: vec![sub_id.clone()],
                    },
                )),
            },
            EventMessage {
                space_id: String::new(),
                value: Some(EventValue::ChatStateUpdate(
                    anytype_rpc::anytype::event::chat::UpdateState {
                        state: Some(model::ChatState {
                            last_state_id: "state-1".to_owned(),
                            ..Default::default()
                        }),
                        sub_ids: vec![sub_id],
                    },
                )),
            },
        ]);
        let mut reader = SessionEventReader::spawn(raw_event_stream([event]));
        while !reader.task.is_finished() {
            tokio::task::yield_now().await;
        }

        let SessionReaderEvent::Event(queued) = reader.recv().await else {
            panic!("queued event must precede the terminal boundary");
        };
        let delivered = bounded_connected_work(None, worker.handle_event(*queued))
            .await
            .expect("unbounded event delivery")
            .expect("queued chat event is valid");
        assert!(delivered);
        assert!(matches!(
            event_rx.recv().await,
            Some(ChatEvent::MessageAdded { .. })
        ));
        assert!(matches!(
            event_rx.recv().await,
            Some(ChatEvent::MessageDeleted { .. })
        ));
        assert!(matches!(
            event_rx.recv().await,
            Some(ChatEvent::ReactionsUpdated { .. })
        ));
        assert!(matches!(
            event_rx.recv().await,
            Some(ChatEvent::ChatStateUpdated { .. })
        ));
        assert!(matches!(
            reader.recv().await,
            SessionReaderEvent::Boundary(
                SessionReaderBoundary::Closed | SessionReaderBoundary::Transport(_)
            )
        ));
        remove_test_keystore(&path);
    }

    #[tokio::test]
    async fn full_output_does_not_lose_queued_delete_or_reaction_before_boundary() {
        let (grpc, server) = inert_grpc_client().await;
        let (client, path) = test_client("full-output-queued-event");
        let builder = ChatStreamBuilder::new(client).subscribe_chat("chat-1");
        let (event_tx, mut event_rx) = mpsc::channel(1);
        event_tx
            .try_send(ChatEvent::StreamDisconnected)
            .expect("fill public event output");
        let (_control_tx, control_rx) = mpsc::channel(1);
        let mut worker = ChatStreamWorker::new(builder, event_tx, control_rx);
        let sub_id = worker
            .subscriptions
            .get("chat-1")
            .expect("initial subscription")
            .sub_id
            .clone();
        let event = Event {
            messages: vec![
                EventMessage {
                    space_id: String::new(),
                    value: Some(EventValue::ChatDelete(
                        anytype_rpc::anytype::event::chat::Delete {
                            id: "deleted-message".to_owned(),
                            sub_ids: vec![sub_id.clone()],
                        },
                    )),
                },
                EventMessage {
                    space_id: String::new(),
                    value: Some(EventValue::ChatUpdateReactions(
                        anytype_rpc::anytype::event::chat::UpdateReactions {
                            id: "reacted-message".to_owned(),
                            reactions: None,
                            sub_ids: vec![sub_id],
                        },
                    )),
                },
            ],
            context_id: "chat-1".to_owned(),
            initiator: None,
            trace_id: String::new(),
        };
        let mut reader = SessionEventReader::spawn(raw_event_stream([event]));
        while !reader.task.is_finished() {
            tokio::task::yield_now().await;
        }
        let delivery = tokio::spawn(async move {
            let mut reconnect_attempt = 0;
            let disconnected = worker
                .connected_loop(grpc, &mut reader, &mut reconnect_attempt, None)
                .await;
            (disconnected, worker)
        });

        tokio::task::yield_now().await;
        assert!(
            !delivery.is_finished(),
            "full public output applies backpressure"
        );
        assert!(matches!(
            event_rx.recv().await,
            Some(ChatEvent::StreamDisconnected)
        ));
        assert!(matches!(
            event_rx.recv().await,
            Some(ChatEvent::MessageDeleted { .. })
        ));
        assert!(matches!(
            event_rx.recv().await,
            Some(ChatEvent::ReactionsUpdated { .. })
        ));
        let (disconnected, worker) = delivery.await.expect("delivery task");
        assert!(disconnected);
        assert!(worker.pending_events.is_empty());
        server.abort();
        remove_test_keystore(&path);
    }

    #[tokio::test]
    async fn queued_event_is_delivered_before_ready_saturation_boundary() {
        let mut reader = raw_saturated_session_reader();
        while !reader.task.is_finished() {
            tokio::task::yield_now().await;
        }

        assert!(matches!(reader.recv().await, SessionReaderEvent::Event(_)));
        assert!(matches!(
            reader.recv().await,
            SessionReaderEvent::Boundary(SessionReaderBoundary::QueueSaturated)
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn chat_reconnect_phases_share_one_stream_lifetime() {
        let policy = GrpcTimeoutPolicy {
            stream_total_lifetime: Some(Duration::from_secs(6)),
            ..GrpcTimeoutPolicy::default()
        };
        let mut deadline = GrpcStreamDeadline::new(policy, None).expect("stream deadline");
        let retained = deadline.workflow_deadline().expect("lifetime boundary");

        deadline
            .phase(tokio::time::sleep(Duration::from_secs(2)))
            .await
            .expect("reconnect connection phase");
        deadline.reset_idle().expect("stream reopen");
        assert_eq!(deadline.workflow_deadline(), Some(retained));
        deadline
            .phase(tokio::time::sleep(Duration::from_secs(2)))
            .await
            .expect("reconnect backoff phase");

        let options = stream_options(GrpcCallOptions::stream_setup(), Some(retained));
        assert_eq!(options.enclosing, Some(retained));
        let error = deadline
            .phase(tokio::time::sleep(Duration::from_secs(3)))
            .await
            .expect_err("resubscribe phase must retain lifetime");
        assert!(matches!(
            error,
            GrpcStreamError::Deadline(anytype_rpc::deadline::GrpcDeadlineError {
                class: GrpcTimeoutClass::StreamLifetime,
                ..
            })
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn sustained_saturation_reconnects_observe_configured_backoff() {
        let (grpc, server) = inert_grpc_client().await;
        let (client, path) = test_client("saturation-backoff");
        let mut builder = ChatStreamBuilder::new(client).subscribe_chat("chat-1");
        builder.backoff = BackoffPolicy {
            initial: Duration::from_secs(1),
            max: Duration::from_secs(4),
            factor: 2.0,
        };
        let (event_tx, _event_rx) = mpsc::channel(8);
        let (_control_tx, control_rx) = mpsc::channel(1);
        let mut worker = ChatStreamWorker::new(builder, event_tx, control_rx);
        let sub_id = worker
            .subscriptions
            .get("chat-1")
            .expect("initial subscription")
            .sub_id
            .clone();
        let (completed, mut completions) = mpsc::channel(4);
        let task = tokio::spawn(async move {
            let mut attempt = 0_u32;
            for order in 1..=4 {
                let event = message_event("chat-1", &sub_id, &format!("{order:04}"));
                let mut reader = SessionEventReader::spawn(raw_saturated_event_stream(event));
                assert!(
                    worker
                        .connected_loop(grpc.clone(), &mut reader, &mut attempt, None)
                        .await
                );
                attempt = attempt.saturating_add(1);
                worker
                    .wait_reconnect_backoff(attempt, None)
                    .await
                    .expect("unbounded backoff wait");
                completed
                    .send(attempt)
                    .await
                    .expect("completion observer remains open");
            }
        });

        tokio::task::yield_now().await;
        for (expected_attempt, delay) in [(1, 2), (2, 4), (3, 4), (4, 4)] {
            tokio::time::advance(Duration::from_secs(delay) - Duration::from_millis(1)).await;
            assert!(!task.is_finished(), "reconnect backoff must not spin");
            tokio::time::advance(Duration::from_millis(1)).await;
            tokio::task::yield_now().await;
            assert_eq!(completions.recv().await, Some(expected_attempt));
            tokio::task::yield_now().await;
        }
        task.await.expect("bounded reconnect loop");
        server.abort();
        remove_test_keystore(&path);
    }

    #[tokio::test(start_paused = true)]
    async fn chat_enclosing_deadline_caps_initial_connection_phase() {
        let enclosing =
            GrpcEnclosingDeadline::from_now(Duration::from_secs(3)).expect("enclosing deadline");
        let deadline = GrpcStreamDeadline::new(
            GrpcTimeoutPolicy {
                stream_total_lifetime: None,
                ..GrpcTimeoutPolicy::default()
            },
            Some(enclosing),
        )
        .expect("stream deadline");
        assert_eq!(deadline.workflow_deadline(), Some(enclosing));
        let options = stream_options(GrpcCallOptions::stream_setup(), Some(enclosing));
        assert_eq!(options.enclosing, Some(enclosing));

        let error = deadline
            .phase(tokio::time::sleep(Duration::from_secs(4)))
            .await
            .expect_err("initial connection must retain enclosing deadline");
        assert!(matches!(error, GrpcStreamError::Deadline(_)));
    }

    #[tokio::test(start_paused = true)]
    async fn chat_enclosing_deadline_caps_stalled_session_acquisition() {
        let (client, path) = test_client("acquisition");
        let enclosing =
            GrpcEnclosingDeadline::from_now(Duration::from_secs(3)).expect("enclosing deadline");
        let builder = ChatStreamBuilder::new(client).enclosing_deadline(enclosing);
        let (event_tx, mut event_rx) = mpsc::channel(1);
        let (_control_tx, control_rx) = mpsc::channel(1);
        let mut worker = ChatStreamWorker::new(builder, event_tx, control_rx);
        let task = tokio::spawn(async move {
            worker
                .run_with_session_acquisition(std::future::pending::<Result<GrpcSession>>())
                .await;
        });

        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(3)).await;
        task.await.expect("acquisition task terminates");
        assert!(matches!(
            event_rx.recv().await,
            Some(ChatEvent::StreamDisconnected)
        ));
        remove_test_keystore(&path);
    }

    #[tokio::test(start_paused = true)]
    async fn full_event_output_buffer_cannot_outlive_stream_lifetime() {
        let (client, path) = test_client("full-output");
        let builder = ChatStreamBuilder::new(client).subscribe_chat("chat-1");
        let (event_tx, _event_rx) = mpsc::channel(1);
        event_tx
            .try_send(ChatEvent::StreamDisconnected)
            .expect("fill event output buffer");
        let (_control_tx, control_rx) = mpsc::channel(1);
        let mut worker = ChatStreamWorker::new(builder, event_tx, control_rx);
        let event = Event {
            messages: vec![EventMessage {
                space_id: String::new(),
                value: Some(EventValue::ChatDelete(
                    anytype_rpc::anytype::event::chat::Delete {
                        id: "message-1".to_owned(),
                        sub_ids: Vec::new(),
                    },
                )),
            }],
            context_id: "chat-1".to_owned(),
            initiator: None,
            trace_id: String::new(),
        };
        let mut deadline = GrpcStreamDeadline::new(
            GrpcTimeoutPolicy {
                stream_total_lifetime: Some(Duration::from_secs(2)),
                ..GrpcTimeoutPolicy::default()
            },
            None,
        )
        .expect("stream lifetime");
        let task = tokio::spawn(async move {
            bounded_connected_work(Some(&mut deadline), worker.handle_event(event)).await
        });

        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(2)).await;
        let error = task
            .await
            .expect("bounded event task")
            .expect_err("full output buffer must expire");
        assert!(matches!(
            error,
            GrpcStreamError::Deadline(anytype_rpc::deadline::GrpcDeadlineError {
                class: GrpcTimeoutClass::StreamLifetime,
                ..
            })
        ));
        remove_test_keystore(&path);
    }

    #[tokio::test]
    async fn saturated_undelivered_message_replays_without_advancing_watermark() {
        let (client, path) = test_client("saturated-message-watermark");
        let builder = ChatStreamBuilder::new(client).subscribe_chat("chat-1");
        let (event_tx, mut event_rx) = mpsc::channel(1);
        event_tx
            .try_send(ChatEvent::StreamDisconnected)
            .expect("fill event output buffer");
        let (_control_tx, control_rx) = mpsc::channel(1);
        let mut worker = ChatStreamWorker::new(builder, event_tx, control_rx);
        let sub_id = worker
            .subscriptions
            .get("chat-1")
            .expect("initial chat subscription")
            .sub_id
            .clone();
        let event = message_event("chat-1", &sub_id, "0001");
        let (mut reader, start_reader) = paused_raw_saturated_session_reader();
        let (entered, send_entered) = oneshot::channel();
        let task = tokio::spawn(async move {
            let work = async {
                let _ = entered.send(());
                worker.handle_event(event).await
            };
            let result = bounded_reader_work(Some(&mut reader), None, work).await;
            (result, worker)
        });

        send_entered.await.expect("output send reached");
        let _ = start_reader.send(());
        let (result, mut worker) = task.await.expect("saturated output task");
        assert_queue_saturated(result.expect_err("saturation cancels output"));
        assert_eq!(
            worker
                .subscriptions
                .get("chat-1")
                .and_then(|subscription| subscription.last_order_id.as_deref()),
            None
        );

        assert!(matches!(
            event_rx.recv().await,
            Some(ChatEvent::StreamDisconnected)
        ));
        assert!(worker.deliver_pending_events().await);
        assert!(matches!(
            event_rx.recv().await,
            Some(ChatEvent::MessageAdded { .. })
        ));
        assert_eq!(
            worker
                .subscriptions
                .get("chat-1")
                .and_then(|subscription| subscription.last_order_id.as_deref()),
            Some("0001")
        );
        remove_test_keystore(&path);
    }

    #[tokio::test(start_paused = true)]
    async fn lifetime_canceled_state_replays_without_advancing_watermark() {
        let (client, path) = test_client("lifetime-state-watermark");
        let builder = ChatStreamBuilder::new(client).subscribe_chat("chat-1");
        let (event_tx, mut event_rx) = mpsc::channel(1);
        event_tx
            .try_send(ChatEvent::StreamDisconnected)
            .expect("fill event output buffer");
        let (_control_tx, control_rx) = mpsc::channel(1);
        let mut worker = ChatStreamWorker::new(builder, event_tx, control_rx);
        let sub_id = worker
            .subscriptions
            .get("chat-1")
            .expect("initial chat subscription")
            .sub_id
            .clone();
        let event = state_event("chat-1", &sub_id, "state-1");
        let mut deadline = GrpcStreamDeadline::new(
            GrpcTimeoutPolicy {
                stream_total_lifetime: Some(Duration::from_secs(2)),
                ..GrpcTimeoutPolicy::default()
            },
            None,
        )
        .expect("stream lifetime");
        let (entered, send_entered) = oneshot::channel();
        let task = tokio::spawn(async move {
            let work = async {
                let _ = entered.send(());
                worker.handle_event(event).await
            };
            let result = bounded_reader_work(None, Some(&mut deadline), work).await;
            (result, worker)
        });

        send_entered.await.expect("output send reached");
        tokio::time::advance(Duration::from_secs(2)).await;
        let (result, mut worker) = task.await.expect("lifetime output task");
        assert!(matches!(result, Err(GrpcStreamError::Deadline(_))));
        assert_eq!(
            worker
                .subscriptions
                .get("chat-1")
                .and_then(|subscription| subscription.last_state_id.as_deref()),
            None
        );

        assert!(matches!(
            event_rx.recv().await,
            Some(ChatEvent::StreamDisconnected)
        ));
        assert!(worker.deliver_pending_events().await);
        assert!(matches!(
            event_rx.recv().await,
            Some(ChatEvent::ChatStateUpdated { .. })
        ));
        assert_eq!(
            worker
                .subscriptions
                .get("chat-1")
                .and_then(|subscription| subscription.last_state_id.as_deref()),
            Some("state-1")
        );
        remove_test_keystore(&path);
    }

    #[tokio::test]
    async fn saturated_raw_reader_terminates_stalled_output_work() {
        let mut reader = raw_saturated_session_reader();
        let error = bounded_reader_work(Some(&mut reader), None, std::future::pending::<()>())
            .await
            .expect_err("saturated decoded queue terminates output work");
        assert_queue_saturated(error);
    }

    #[tokio::test]
    async fn saturated_raw_reader_terminates_stalled_resubscribe_work() {
        let mut reader = raw_saturated_session_reader();
        let error = bounded_reader_work(Some(&mut reader), None, std::future::pending::<()>())
            .await
            .expect_err("saturated decoded queue terminates resubscription");
        assert_queue_saturated(error);
    }

    #[tokio::test(start_paused = true)]
    async fn stalled_connected_control_rpc_cannot_outlive_enclosing_deadline() {
        let enclosing =
            GrpcEnclosingDeadline::from_now(Duration::from_secs(2)).expect("enclosing deadline");
        let mut deadline = GrpcStreamDeadline::new(
            GrpcTimeoutPolicy {
                stream_idle: None,
                stream_total_lifetime: None,
                ..GrpcTimeoutPolicy::default()
            },
            Some(enclosing),
        )
        .expect("stream enclosing deadline");
        let task = tokio::spawn(async move {
            bounded_connected_work(Some(&mut deadline), stalled_control_rpc()).await
        });

        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(2)).await;
        let error = task
            .await
            .expect("bounded control task")
            .expect_err("stalled control work must expire");
        assert!(matches!(error, GrpcStreamError::Deadline(_)));
    }

    #[tokio::test]
    async fn post_dispatch_saturation_reports_mutation_indeterminate() {
        let (mut reader, start_reader) = paused_raw_saturated_session_reader();
        let (respond_to, response) = oneshot::channel();
        let (dispatched, possible_dispatch) = oneshot::channel();
        let task = tokio::spawn(async move {
            let work = async move {
                let _ = dispatched.send(());
                std::future::pending::<Result<()>>().await
            };
            bounded_control_mutation(Some(&mut reader), None, respond_to, work).await
        });

        possible_dispatch.await.expect("control work first polled");
        let _ = start_reader.send(());
        let error = task
            .await
            .expect("control task")
            .expect_err("saturated decoded queue terminates control work");
        assert_queue_saturated(error);
        assert!(matches!(
            response.await.expect("control response delivered"),
            Err(AnytypeError::Grpc {
                source: AnytypeGrpcError::ControlBoundary {
                    kind: GrpcControlBoundaryKind::QueueSaturated,
                    outcome: GrpcTimeoutOutcome::MutationIndeterminate,
                },
            })
        ));
    }

    #[tokio::test]
    async fn pre_dispatch_saturation_reports_read_aborted() {
        let mut reader = raw_saturated_session_reader();
        while !reader.task.is_finished() {
            tokio::task::yield_now().await;
        }
        let polled = Arc::new(AtomicBool::new(false));
        let work_polled = polled.clone();
        let (respond_to, response) = oneshot::channel();
        let work = std::future::poll_fn(move |_context| {
            work_polled.store(true, Ordering::Relaxed);
            Poll::<Result<()>>::Pending
        });

        let error = bounded_control_mutation(Some(&mut reader), None, respond_to, work)
            .await
            .expect_err("preexisting saturation terminates control work");
        assert_queue_saturated(error);
        assert!(matches!(
            response.await.expect("control response delivered"),
            Err(AnytypeError::Grpc {
                source: AnytypeGrpcError::ControlBoundary {
                    kind: GrpcControlBoundaryKind::QueueSaturated,
                    outcome: GrpcTimeoutOutcome::ReadAborted,
                },
            })
        ));
        assert!(!polled.load(Ordering::Relaxed));
    }

    #[tokio::test]
    async fn definitive_control_rejection_is_preserved() {
        let (respond_to, response) = oneshot::channel();
        let result = bounded_control_mutation(None, None, respond_to, async {
            Err(AnytypeError::Validation {
                message: "definitive rejection".to_owned(),
            })
        })
        .await;

        assert!(result.is_ok());
        assert!(matches!(
            response.await.expect("control response delivered"),
            Err(AnytypeError::Validation { .. })
        ));
    }

    #[test]
    fn post_dispatch_transport_loss_reports_mutation_indeterminate() {
        let error =
            SessionReaderBoundary::Transport(tonic::Code::ResourceExhausted).into_stream_error();
        assert!(matches!(
            control_mutation_failure(&error, true),
            AnytypeError::Grpc {
                source: AnytypeGrpcError::ControlBoundary {
                    kind: GrpcControlBoundaryKind::TransportLost,
                    outcome: GrpcTimeoutOutcome::MutationIndeterminate,
                },
            }
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn pre_dispatch_control_expiry_reports_read_aborted() {
        let polled = Arc::new(AtomicBool::new(false));
        let work_polled = polled.clone();
        let enclosing = GrpcEnclosingDeadline::from_instant(tokio::time::Instant::now());
        let mut deadline = GrpcStreamDeadline::new(
            GrpcTimeoutPolicy {
                stream_idle: None,
                stream_total_lifetime: None,
                ..GrpcTimeoutPolicy::default()
            },
            Some(enclosing),
        )
        .expect("expired enclosing deadline is representable");
        let (respond_to, response) = oneshot::channel();
        let work = std::future::poll_fn(move |_context| {
            work_polled.store(true, Ordering::Relaxed);
            Poll::<Result<()>>::Pending
        });

        let result = bounded_control_mutation(None, Some(&mut deadline), respond_to, work).await;
        assert!(result.is_err());
        let response = response
            .await
            .expect("pre-dispatch response delivered")
            .expect_err("expired work is read-aborted");
        assert!(matches!(
            response,
            AnytypeError::Grpc {
                source: AnytypeGrpcError::Deadline {
                    source: anytype_rpc::deadline::GrpcDeadlineError {
                        outcome: GrpcTimeoutOutcome::ReadAborted,
                        ..
                    }
                }
            }
        ));
        assert!(!polled.load(Ordering::Relaxed));
    }

    #[tokio::test(start_paused = true)]
    async fn post_dispatch_control_expiry_reports_mutation_indeterminate_without_commit() {
        let committed = Arc::new(AtomicBool::new(false));
        let task_committed = committed.clone();
        let (dispatched_tx, dispatched_rx) = oneshot::channel();
        let (respond_to, response) = oneshot::channel();
        let mut deadline = GrpcStreamDeadline::new(
            GrpcTimeoutPolicy {
                stream_total_lifetime: Some(Duration::from_secs(2)),
                ..GrpcTimeoutPolicy::default()
            },
            None,
        )
        .expect("stream lifetime");
        let task = tokio::spawn(async move {
            let work = async move {
                let _ = dispatched_tx.send(());
                std::future::pending::<()>().await;
                task_committed.store(true, Ordering::Relaxed);
                Ok(())
            };
            bounded_control_mutation(None, Some(&mut deadline), respond_to, work).await
        });

        dispatched_rx.await.expect("control RPC dispatched");
        tokio::time::advance(Duration::from_secs(2)).await;
        let response = response
            .await
            .expect("deadline response delivered")
            .expect_err("possibly dispatched mutation is indeterminate");
        assert!(matches!(
            response,
            AnytypeError::Grpc {
                source: AnytypeGrpcError::Deadline {
                    source: anytype_rpc::deadline::GrpcDeadlineError {
                        outcome: GrpcTimeoutOutcome::MutationIndeterminate,
                        ..
                    }
                }
            }
        ));
        assert!(!committed.load(Ordering::Relaxed));
        assert!(task.await.expect("control task").is_err());
    }

    #[test]
    fn chat_events_respect_sub_ids() {
        let chat_id = "chat-1".to_string();
        let sub_id = "sub-1".to_string();
        let message = model::ChatMessage {
            id: "msg-1".to_string(),
            order_id: "0001".to_string(),
            state_id: "state-1".to_string(),
            creator: "alice".to_string(),
            ..Default::default()
        };
        let add = anytype_rpc::anytype::event::chat::Add {
            id: "msg-1".to_string(),
            order_id: "0001".to_string(),
            after_order_id: String::new(),
            message: Some(message),
            sub_ids: vec![sub_id.clone()],
            dependencies: Vec::new(),
        };
        let event = Event {
            messages: vec![EventMessage {
                space_id: String::new(),
                value: Some(EventValue::ChatAdd(add)),
            }],
            context_id: chat_id.clone(),
            initiator: None,
            trace_id: String::new(),
        };

        let mut active = HashSet::new();
        active.insert(sub_id);
        let events =
            chat_events_from_event(&chat_id, event.clone(), &active).expect("valid chat event");
        assert!(matches!(
            events.as_slice(),
            [ChatEvent::MessageAdded { chat_id: id, .. }] if id == &chat_id
        ));

        let mut inactive = HashSet::new();
        inactive.insert("other".to_string());
        let events = chat_events_from_event(&chat_id, event, &inactive).expect("valid chat event");
        assert!(events.is_empty());
    }
}
