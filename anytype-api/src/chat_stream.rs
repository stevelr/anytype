//! # Anytype Chat Streaming (gRPC)
//!
//! Async streaming interface for chat message updates and chat state changes.
//!
//! The stream is backed by `ListenSessionEvents` and chat subscription RPCs.
//! It supports reconnect with per-chat watermarks to reduce missed messages.

use std::{
    collections::{HashMap, HashSet},
    pin::Pin,
    sync::atomic::{AtomicU64, Ordering},
    task::{Context, Poll},
    time::Duration,
};

#[cfg(feature = "grpc")]
use anytype_rpc::{
    anytype::rpc::chat::{
        get_messages, subscribe_last_messages, subscribe_to_message_previews, unsubscribe,
        unsubscribe_from_message_previews,
    },
    anytype::{Event, StreamRequest, event::message::Value as EventValue},
    client::{AnytypeGrpcClient, AnytypeGrpcConfig},
};
use futures::Stream;
use tokio::{
    sync::{mpsc, oneshot},
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

    /// Build and start the chat stream worker.
    #[must_use]
    pub fn build(self) -> ChatStreamHandle {
        let (event_tx, event_rx) = mpsc::channel(self.buffer);
        let (control_tx, control_rx) = mpsc::channel(self.buffer);

        let mut worker = ChatStreamWorker::new(
            self.client,
            self.chat_ids,
            self.previews,
            self.backoff,
            self.last_messages_limit,
            event_tx,
            control_rx,
        );

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
    shutdown: bool,
}

impl ChatStreamWorker {
    fn new(
        client: AnytypeClient,
        chat_ids: Vec<String>,
        previews: bool,
        backoff: BackoffPolicy,
        last_messages_limit: u32,
        event_tx: mpsc::Sender<ChatEvent>,
        control_rx: mpsc::Receiver<ControlMessage>,
    ) -> Self {
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
            shutdown: false,
        }
    }

    async fn run(&mut self) {
        let session = match GrpcSession::from_client(&self.client).await {
            Ok(session) => session,
            Err(err) => {
                let _ = self.event_tx.send(ChatEvent::StreamDisconnected).await;
                tracing::error!("chat stream: grpc session unavailable: {err}");
                return;
            }
        };

        let mut attempt = 0;
        let mut was_connected = false;

        loop {
            if self.is_shutdown() {
                break;
            }

            let grpc = match session.connect().await {
                Ok(client) => client,
                Err(err) => {
                    tracing::warn!("chat stream: connect failed: {err}");
                    attempt += 1;
                    self.wait_backoff(attempt).await;
                    continue;
                }
            };

            let stream = match open_session_events(&grpc).await {
                Ok(stream) => stream,
                Err(err) => {
                    tracing::warn!("chat stream: listen failed: {err}");
                    attempt += 1;
                    self.wait_backoff(attempt).await;
                    continue;
                }
            };

            attempt = 0;
            if was_connected {
                let _ = self.event_tx.send(ChatEvent::StreamResubscribed).await;
            }

            if let Err(err) = self.resubscribe(&grpc, was_connected).await {
                tracing::warn!("chat stream: resubscribe failed: {err}");
            }

            let disconnected = self.connected_loop(grpc, stream).await;
            if disconnected && !self.is_shutdown() {
                let _ = self.event_tx.send(ChatEvent::StreamDisconnected).await;
                was_connected = true;
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
            message = self.control_rx.recv() => {
                if let Some(message) = message {
                    self.handle_control_message(message, None).await;
                }
            }
        }
    }

    async fn connected_loop(
        &mut self,
        grpc: AnytypeGrpcClient,
        mut stream: tonic::Streaming<Event>,
    ) -> bool {
        loop {
            if self.is_shutdown() {
                return false;
            }
            tokio::select! {
                message = self.control_rx.recv() => {
                    if let Some(message) = message {
                        self.handle_control_message(message, Some(&grpc)).await;
                        if self.is_shutdown() {
                            return false;
                        }
                    } else {
                        return false;
                    }
                }
                message = stream.message() => {
                    match message {
                        Ok(Some(event)) => {
                            self.handle_event(event).await;
                        }
                        Ok(None) => {
                            return true;
                        }
                        Err(err) => {
                            tracing::warn!("chat stream: event error: {err}");
                            return true;
                        }
                    }
                }
            }
        }
    }

    async fn handle_event(&mut self, event: Event) {
        let active_sub_ids = self.active_sub_ids();
        let chat_id = event.context_id.clone();
        if chat_id.is_empty() {
            return;
        }

        let events = chat_events_from_event(&chat_id, event, &active_sub_ids);
        for chat_event in events {
            self.update_watermark(&chat_event);
            if self.event_tx.send(chat_event).await.is_err() {
                break;
            }
        }
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

    async fn resubscribe(&mut self, grpc: &AnytypeGrpcClient, is_reconnect: bool) -> Result<()> {
        if self.previews {
            if self.preview_sub_id.is_none() {
                self.preview_sub_id = Some(next_sub_id("preview"));
            }
            let sub_id = self.preview_sub_id.clone().unwrap_or_default();
            let response = subscribe_previews(grpc, &sub_id).await?;
            if !is_reconnect {
                for preview in response.previews {
                    self.emit_preview(preview).await;
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
            )
            .await?;

            if let Some(state) = response.chat_state.as_ref() {
                let state = chat_state_from_grpc(state);
                let should_emit = subscription
                    .last_state_id
                    .as_deref()
                    .is_none_or(|current| current != state.last_state_id);
                subscription.last_state_id = Some(state.last_state_id.clone());
                if should_emit {
                    let _ = self
                        .event_tx
                        .send(ChatEvent::ChatStateUpdated {
                            chat_id: subscription.chat_id.clone(),
                            state,
                        })
                        .await;
                }
            }

            if subscription.last_order_id.is_none() {
                for message in response.messages {
                    let message = chat_message_from_grpc(message);
                    subscription.last_order_id = Some(message.order_id.clone());
                    let _ = self
                        .event_tx
                        .send(ChatEvent::MessageAdded {
                            chat_id: subscription.chat_id.clone(),
                            message,
                        })
                        .await;
                }
            } else if let Some(order_id) = subscription.last_order_id.clone() {
                catch_ups.push((subscription.chat_id.clone(), order_id));
            }
        }

        for (chat_id, order_id) in catch_ups {
            let _ = self.catch_up_messages(grpc, &chat_id, &order_id).await;
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
            )
            .await?;
            if let Some(state) = response.chat_state.as_ref() {
                let state = chat_state_from_grpc(state);
                subscription.last_state_id = Some(state.last_state_id.clone());
                let _ = self
                    .event_tx
                    .send(ChatEvent::ChatStateUpdated {
                        chat_id: subscription.chat_id.clone(),
                        state,
                    })
                    .await;
            }
            for message in response.messages {
                let message = chat_message_from_grpc(message);
                subscription.last_order_id = Some(message.order_id.clone());
                let _ = self
                    .event_tx
                    .send(ChatEvent::MessageAdded {
                        chat_id: subscription.chat_id.clone(),
                        message,
                    })
                    .await;
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
        let Some(subscription) = self.subscriptions.remove(chat_id) else {
            return Ok(());
        };

        if let Some(grpc) = grpc {
            unsubscribe_chat(grpc, &subscription.chat_id, &subscription.sub_id).await?;
        }
        Ok(())
    }

    async fn shutdown(&mut self, grpc: Option<&AnytypeGrpcClient>) -> Result<()> {
        if let Some(grpc) = grpc {
            if let Some(preview_sub_id) = self.preview_sub_id.take() {
                let _ = unsubscribe_previews(grpc, &preview_sub_id).await;
            }
            for subscription in self.subscriptions.values() {
                let _ = unsubscribe_chat(grpc, &subscription.chat_id, &subscription.sub_id).await;
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
    ) -> Result<()> {
        let mut cursor = after_order_id.to_string();
        loop {
            let response = get_messages_after(grpc, chat_id, &cursor).await?;
            if response.messages.is_empty() {
                if let Some(state) = response.chat_state.as_ref() {
                    let state = chat_state_from_grpc(state);
                    if let Some(subscription) = self.subscriptions.get_mut(chat_id) {
                        subscription.last_state_id = Some(state.last_state_id.clone());
                    }
                    let _ = self
                        .event_tx
                        .send(ChatEvent::ChatStateUpdated {
                            chat_id: chat_id.to_string(),
                            state,
                        })
                        .await;
                }
                break;
            }
            for message in response.messages {
                let message = chat_message_from_grpc(message);
                cursor = message.order_id.clone();
                if let Some(subscription) = self.subscriptions.get_mut(chat_id) {
                    subscription.last_order_id = Some(message.order_id.clone());
                }
                let _ = self
                    .event_tx
                    .send(ChatEvent::MessageAdded {
                        chat_id: chat_id.to_string(),
                        message,
                    })
                    .await;
            }
        }
        Ok(())
    }

    async fn emit_preview(&self, preview: subscribe_to_message_previews::response::ChatPreview) {
        if let Some(message) = preview.message {
            let message = chat_message_from_grpc(message);
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
}

impl GrpcSession {
    async fn from_client(client: &AnytypeClient) -> Result<Self> {
        let grpc = client.grpc_client().await?;
        Ok(Self {
            endpoint: grpc.get_endpoint().to_string(),
            token: grpc.token().to_string(),
        })
    }

    async fn connect(&self) -> Result<AnytypeGrpcClient> {
        let config = AnytypeGrpcConfig::new(self.endpoint.clone());
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
) -> Vec<ChatEvent> {
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
                        message: chat_message_from_grpc(message),
                    });
                }
            }
            EventValue::ChatUpdate(update) => {
                if should_emit(&update.sub_ids, active_sub_ids)
                    && let Some(message) = update.message
                {
                    events.push(ChatEvent::MessageUpdated {
                        chat_id: chat_id.to_string(),
                        message: chat_message_from_grpc(message),
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
    events
}

fn should_emit(sub_ids: &[String], active_sub_ids: &HashSet<String>) -> bool {
    if sub_ids.is_empty() {
        return true;
    }
    sub_ids.iter().any(|id| active_sub_ids.contains(id))
}

async fn open_session_events(grpc: &AnytypeGrpcClient) -> Result<tonic::Streaming<Event>> {
    let request = StreamRequest {
        token: grpc.token().to_string(),
    };
    let request = with_token_request(Request::new(request), grpc.token())?;
    let response = grpc
        .client_commands()
        .listen_session_events(request)
        .await
        .map_err(grpc_status)?
        .into_inner();
    Ok(response)
}

async fn call_subscribe_last_messages(
    grpc: &AnytypeGrpcClient,
    chat_id: &str,
    sub_id: &str,
    limit: u32,
) -> Result<subscribe_last_messages::Response> {
    let request = subscribe_last_messages::Request {
        chat_object_id: chat_id.to_string(),
        limit: limit.cast_signed(),
        sub_id: sub_id.to_string(),
    };
    let request = with_token_request(Request::new(request), grpc.token())?;
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
) -> Result<subscribe_to_message_previews::Response> {
    let request = subscribe_to_message_previews::Request {
        sub_id: sub_id.to_string(),
    };
    let request = with_token_request(Request::new(request), grpc.token())?;
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
) -> Result<get_messages::Response> {
    let request = get_messages::Request {
        chat_object_id: chat_id.to_string(),
        after_order_id: after_order_id.to_string(),
        before_order_id: String::new(),
        limit: 100,
        include_boundary: false,
    };
    let request = with_token_request(Request::new(request), grpc.token())?;
    let response = grpc
        .client_commands()
        .chat_get_messages(request)
        .await
        .map_err(grpc_status)?
        .into_inner();
    ensure_error_ok(response.error.as_ref(), "chat get messages")?;
    Ok(response)
}

#[cfg(test)]
mod tests {
    use anytype_rpc::{anytype::event::Message as EventMessage, model};

    use super::*;

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
        let events = chat_events_from_event(&chat_id, event.clone(), &active);
        assert!(matches!(
            events.as_slice(),
            [ChatEvent::MessageAdded { chat_id: id, .. }] if id == &chat_id
        ));

        let mut inactive = HashSet::new();
        inactive.insert("other".to_string());
        let events = chat_events_from_event(&chat_id, event, &inactive);
        assert!(events.is_empty());
    }
}
