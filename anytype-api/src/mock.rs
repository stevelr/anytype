//! Mock gRPC server for chat/message APIs.

use std::{
    collections::{HashMap, HashSet},
    convert::Infallible,
    net::SocketAddr,
    pin::Pin,
    sync::Arc,
    sync::atomic::{AtomicU64, Ordering},
    task::{Context, Poll},
};

use chrono::Utc;
use futures::Stream;
use tokio::sync::{Mutex, mpsc, oneshot};
use tokio::time::{Duration, Instant, Sleep};
use tonic::{
    body::Body,
    codegen::{BoxFuture, Service, http},
    metadata::MetadataMap,
    server::Grpc,
    transport::Server,
    {Request, Response, Status},
};
use tonic_prost::ProstCodec;

use anytype_rpc::anytype::{
    Event, StreamRequest,
    event::{Message as EventMessage, message::Value as EventValue},
    rpc::{
        chat::{
            add_message, delete_message, edit_message_content, get_messages, get_messages_by_ids,
            read_all, read_messages, subscribe_last_messages, subscribe_to_message_previews,
            toggle_message_reaction, unread, unsubscribe, unsubscribe_from_message_previews,
        },
        object::search_with_meta,
        workspace::open as workspace_open,
    },
};
use anytype_rpc::model;
use prost_types::{Struct, Value};

const DEFAULT_CHAT_ID: &str = "chat-default";
const DEFAULT_CHAT_NAME: &str = "General";
const DEFAULT_SPACE_ID: &str = "space-default";

const MOCK_USERS: &[(&str, &str)] = &[
    ("alice", "token-alice"),
    ("bob", "token-bob"),
    ("carol", "token-carol"),
    ("dash", "token-dash"),
    ("ernie", "token-ernie"),
];

/// Handle to a running mock server.
pub struct MockChatServerHandle {
    addr: SocketAddr,
    shutdown: oneshot::Sender<()>,
    task: tokio::task::JoinHandle<()>,
    state: Arc<Mutex<MockState>>,
}

impl MockChatServerHandle {
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    pub async fn shutdown(self) {
        let _ = self.shutdown.send(());
        let _ = self.task.await;
    }

    pub async fn disconnect_streams(&self) {
        let mut state = self.state.lock().await;
        state.disconnect_streams().await;
    }
}

#[derive(Clone)]
pub struct MockChatServer {
    state: Arc<Mutex<MockState>>,
}

impl MockChatServer {
    pub fn new() -> Self {
        let mut tokens = HashMap::new();
        for (user, token) in MOCK_USERS {
            tokens.insert(token.to_string(), user.to_string());
        }
        let mut state = MockState {
            tokens,
            chats: HashMap::new(),
            space_chats: HashMap::new(),
            subscriptions: HashMap::new(),
            preview_subscriptions: HashSet::new(),
            event_listeners: Vec::new(),
            disconnect_epoch: Arc::new(AtomicU64::new(0)),
        };
        state
            .space_chats
            .insert(DEFAULT_SPACE_ID.to_string(), DEFAULT_CHAT_ID.to_string());
        state.ensure_chat(DEFAULT_CHAT_ID);
        Self {
            state: Arc::new(Mutex::new(state)),
        }
    }

    pub async fn serve(
        self,
        addr: SocketAddr,
        shutdown: oneshot::Receiver<()>,
    ) -> Result<(), tonic::transport::Error> {
        let service = ChatService { state: self.state };
        Server::builder()
            .add_service(service)
            .serve_with_shutdown(addr, async {
                let _ = shutdown.await;
            })
            .await
    }

    pub async fn start(addr: SocketAddr) -> Result<MockChatServerHandle, tonic::transport::Error> {
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let server = Self::new();
        let state = server.state.clone();
        let task = tokio::spawn(async move {
            let _ = server.serve(addr, shutdown_rx).await;
        });
        Ok(MockChatServerHandle {
            addr,
            shutdown: shutdown_tx,
            task,
            state,
        })
    }
}

impl Default for MockChatServer {
    fn default() -> Self {
        Self::new()
    }
}

struct MockState {
    tokens: HashMap<String, String>,
    chats: HashMap<String, ChatRoom>,
    space_chats: HashMap<String, String>,
    subscriptions: HashMap<String, HashSet<String>>,
    preview_subscriptions: HashSet<String>,
    event_listeners: Vec<EventListener>,
    disconnect_epoch: Arc<AtomicU64>,
}

impl Default for MockState {
    fn default() -> Self {
        Self {
            tokens: HashMap::new(),
            chats: HashMap::new(),
            space_chats: HashMap::new(),
            subscriptions: HashMap::new(),
            preview_subscriptions: HashSet::new(),
            event_listeners: Vec::new(),
            disconnect_epoch: Arc::new(AtomicU64::new(0)),
        }
    }
}

#[derive(Debug)]
struct EventListener {
    sender: mpsc::Sender<Event>,
}

impl MockState {
    fn ensure_chat(&mut self, chat_id: &str) {
        self.chats.entry(chat_id.to_string()).or_insert_with(|| {
            let members = MOCK_USERS
                .iter()
                .map(|(name, _)| name.to_string())
                .collect();
            let (space_id, name) = if chat_id == DEFAULT_CHAT_ID {
                (DEFAULT_SPACE_ID.to_string(), DEFAULT_CHAT_NAME.to_string())
            } else {
                (DEFAULT_SPACE_ID.to_string(), format!("Chat {chat_id}"))
            };
            ChatRoom::new(space_id, name, members)
        });
    }

    fn chat_mut(&mut self, chat_id: &str) -> &mut ChatRoom {
        self.ensure_chat(chat_id);
        self.chats.get_mut(chat_id).expect("chat exists")
    }

    fn token_user(&self, token: &str) -> Option<&str> {
        self.tokens.get(token).map(String::as_str)
    }

    fn subscribe_chat(&mut self, chat_id: &str, sub_id: &str) {
        self.subscriptions
            .entry(chat_id.to_string())
            .or_default()
            .insert(sub_id.to_string());
    }

    fn unsubscribe_chat(&mut self, chat_id: &str, sub_id: &str) {
        if let Some(sub_ids) = self.subscriptions.get_mut(chat_id) {
            sub_ids.remove(sub_id);
            if sub_ids.is_empty() {
                self.subscriptions.remove(chat_id);
            }
        }
    }

    fn sub_ids_for_chat(&self, chat_id: &str) -> Vec<String> {
        self.subscriptions
            .get(chat_id)
            .map(|set| set.iter().cloned().collect())
            .unwrap_or_default()
    }

    fn add_listener(&mut self, sender: mpsc::Sender<Event>) -> (Arc<AtomicU64>, u64) {
        let epoch = self.disconnect_epoch.load(Ordering::SeqCst);
        self.event_listeners.push(EventListener { sender });
        (self.disconnect_epoch.clone(), epoch)
    }

    async fn disconnect_streams(&mut self) {
        let wake_event = Event::default();
        self.disconnect_epoch.fetch_add(1, Ordering::SeqCst);
        let listeners = std::mem::take(&mut self.event_listeners);
        for listener in listeners {
            let _ = listener.sender.send(wake_event.clone()).await;
        }
    }
}

#[derive(Debug)]
struct ChatRoom {
    messages: Vec<StoredMessage>,
    order_counter: i64,
    state_counter: i64,
    state_id: String,
    space_id: String,
    name: String,
    archived: bool,
    last_modified: i64,
}

impl ChatRoom {
    fn new(space_id: String, name: String, _members: Vec<String>) -> Self {
        Self {
            messages: Vec::new(),
            order_counter: 0,
            state_counter: 0,
            state_id: "state-0".to_string(),
            space_id,
            name,
            archived: false,
            last_modified: Utc::now().timestamp(),
        }
    }

    fn next_order_id(&mut self) -> String {
        self.order_counter += 1;
        format!("{:016}", self.order_counter)
    }

    fn next_state_id(&mut self) -> String {
        self.state_counter += 1;
        self.state_id = format!("state-{:016}", self.state_counter);
        self.state_id.clone()
    }

    fn touch(&mut self) {
        self.last_modified = Utc::now().timestamp();
    }
}

#[derive(Debug)]
struct StoredMessage {
    id: String,
    order_id: String,
    state_id: String,
    creator: String,
    created_at: i64,
    modified_at: i64,
    reply_to_message_id: String,
    content: model::chat_message::MessageContent,
    attachments: Vec<model::chat_message::Attachment>,
    reactions: HashMap<String, HashSet<String>>,
    read_by: HashSet<String>,
    mention_read_by: HashSet<String>,
    has_mention: bool,
    synced: bool,
}

#[derive(Clone)]
struct ChatService {
    state: Arc<Mutex<MockState>>,
}

impl Service<http::Request<Body>> for ChatService {
    type Response = http::Response<Body>;
    type Error = Infallible;
    type Future = BoxFuture<Self::Response, Self::Error>;

    fn poll_ready(
        &mut self,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: http::Request<Body>) -> Self::Future {
        let state = self.state.clone();
        match req.uri().path() {
            "/anytype.ClientCommands/ChatAddMessage" => {
                let fut = async move {
                    let mut grpc = Grpc::new(ProstCodec::default());
                    let svc = ChatAddMessageSvc { state };
                    Ok(grpc.unary(svc, req).await)
                };
                Box::pin(fut)
            }
            "/anytype.ClientCommands/ChatEditMessageContent" => {
                let fut = async move {
                    let mut grpc = Grpc::new(ProstCodec::default());
                    let svc = ChatEditMessageSvc { state };
                    Ok(grpc.unary(svc, req).await)
                };
                Box::pin(fut)
            }
            "/anytype.ClientCommands/ChatDeleteMessage" => {
                let fut = async move {
                    let mut grpc = Grpc::new(ProstCodec::default());
                    let svc = ChatDeleteMessageSvc { state };
                    Ok(grpc.unary(svc, req).await)
                };
                Box::pin(fut)
            }
            "/anytype.ClientCommands/ChatGetMessages" => {
                let fut = async move {
                    let mut grpc = Grpc::new(ProstCodec::default());
                    let svc = ChatGetMessagesSvc { state };
                    Ok(grpc.unary(svc, req).await)
                };
                Box::pin(fut)
            }
            "/anytype.ClientCommands/ChatGetMessagesByIds" => {
                let fut = async move {
                    let mut grpc = Grpc::new(ProstCodec::default());
                    let svc = ChatGetMessagesByIdsSvc { state };
                    Ok(grpc.unary(svc, req).await)
                };
                Box::pin(fut)
            }
            "/anytype.ClientCommands/ChatToggleMessageReaction" => {
                let fut = async move {
                    let mut grpc = Grpc::new(ProstCodec::default());
                    let svc = ChatToggleReactionSvc { state };
                    Ok(grpc.unary(svc, req).await)
                };
                Box::pin(fut)
            }
            "/anytype.ClientCommands/ChatReadAll" => {
                let fut = async move {
                    let mut grpc = Grpc::new(ProstCodec::default());
                    let svc = ChatReadAllSvc { state };
                    Ok(grpc.unary(svc, req).await)
                };
                Box::pin(fut)
            }
            "/anytype.ClientCommands/ChatSubscribeLastMessages" => {
                let fut = async move {
                    let mut grpc = Grpc::new(ProstCodec::default());
                    let svc = ChatSubscribeLastMessagesSvc { state };
                    Ok(grpc.unary(svc, req).await)
                };
                Box::pin(fut)
            }
            "/anytype.ClientCommands/ChatUnsubscribe" => {
                let fut = async move {
                    let mut grpc = Grpc::new(ProstCodec::default());
                    let svc = ChatUnsubscribeSvc { state };
                    Ok(grpc.unary(svc, req).await)
                };
                Box::pin(fut)
            }
            "/anytype.ClientCommands/ChatReadMessages" => {
                let fut = async move {
                    let mut grpc = Grpc::new(ProstCodec::default());
                    let svc = ChatReadMessagesSvc { state };
                    Ok(grpc.unary(svc, req).await)
                };
                Box::pin(fut)
            }
            "/anytype.ClientCommands/ChatUnreadMessages" => {
                let fut = async move {
                    let mut grpc = Grpc::new(ProstCodec::default());
                    let svc = ChatUnreadMessagesSvc { state };
                    Ok(grpc.unary(svc, req).await)
                };
                Box::pin(fut)
            }
            "/anytype.ClientCommands/ChatSubscribeToMessagePreviews" => {
                let fut = async move {
                    let mut grpc = Grpc::new(ProstCodec::default());
                    let svc = ChatSubscribeToMessagePreviewsSvc { state };
                    Ok(grpc.unary(svc, req).await)
                };
                Box::pin(fut)
            }
            "/anytype.ClientCommands/ChatUnsubscribeFromMessagePreviews" => {
                let fut = async move {
                    let mut grpc = Grpc::new(ProstCodec::default());
                    let svc = ChatUnsubscribeFromMessagePreviewsSvc { state };
                    Ok(grpc.unary(svc, req).await)
                };
                Box::pin(fut)
            }
            "/anytype.ClientCommands/ListenSessionEvents" => {
                let fut = async move {
                    let mut grpc = Grpc::new(ProstCodec::default());
                    let svc = ListenSessionEventsSvc { state };
                    Ok(grpc.server_streaming(svc, req).await)
                };
                Box::pin(fut)
            }
            "/anytype.ClientCommands/ObjectSearchWithMeta" => {
                let fut = async move {
                    let mut grpc = Grpc::new(ProstCodec::default());
                    let svc = ObjectSearchWithMetaSvc { state };
                    Ok(grpc.unary(svc, req).await)
                };
                Box::pin(fut)
            }
            "/anytype.ClientCommands/WorkspaceOpen" => {
                let fut = async move {
                    let mut grpc = Grpc::new(ProstCodec::default());
                    let svc = WorkspaceOpenSvc { state };
                    Ok(grpc.unary(svc, req).await)
                };
                Box::pin(fut)
            }
            _ => {
                let fut = async move {
                    let response = Status::unimplemented("method not implemented").into_http();
                    Ok(response)
                };
                Box::pin(fut)
            }
        }
    }
}

impl tonic::server::NamedService for ChatService {
    const NAME: &'static str = "anytype.ClientCommands";
}

#[derive(Clone)]
struct ChatAddMessageSvc {
    state: Arc<Mutex<MockState>>,
}

impl tonic::server::UnaryService<add_message::Request> for ChatAddMessageSvc {
    type Response = add_message::Response;
    type Future = BoxFuture<Response<Self::Response>, Status>;

    fn call(&mut self, request: Request<add_message::Request>) -> Self::Future {
        let state_handle = self.state.clone();
        Box::pin(async move {
            let user = authenticate(request.metadata(), &state_handle).await?;
            let input = request.into_inner();
            let message = input.message.unwrap_or_default();

            let mut state = state_handle.lock().await;
            let sub_ids = state.sub_ids_for_chat(&input.chat_object_id);
            let chat = state.chat_mut(&input.chat_object_id);
            let now_ms = Utc::now().timestamp_millis();
            let message_id = format!("msg-{:016}", chat.order_counter + 1);
            let order_id = chat.next_order_id();
            let state_id = chat.next_state_id();
            chat.touch();

            let mut read_by = HashSet::new();
            read_by.insert(user.clone());
            let stored = StoredMessage {
                id: message_id.clone(),
                order_id: order_id.clone(),
                state_id: state_id.clone(),
                creator: user.clone(),
                created_at: if message.created_at == 0 {
                    now_ms
                } else {
                    message.created_at
                },
                modified_at: if message.modified_at == 0 {
                    now_ms
                } else {
                    message.modified_at
                },
                reply_to_message_id: message.reply_to_message_id,
                content: message.message.unwrap_or_default(),
                attachments: message.attachments,
                reactions: HashMap::new(),
                read_by,
                mention_read_by: HashSet::new(),
                has_mention: message.has_mention,
                synced: message.synced,
            };

            let proto_message = stored.to_proto(&user);
            let after_order_id = chat
                .messages
                .last()
                .map(|msg| msg.order_id.clone())
                .unwrap_or_default();
            chat.messages.push(stored);

            if !sub_ids.is_empty() {
                let chat_state = build_chat_state(chat, &user);
                let event = build_event(
                    &input.chat_object_id,
                    vec![
                        chat_add_value(proto_message, after_order_id, sub_ids.clone()),
                        chat_state_update_value(chat_state, sub_ids),
                    ],
                );
                drop(state);
                broadcast_event(&state_handle, event).await;
            }

            Ok(Response::new(add_message::Response {
                error: None,
                message_id,
                event: None,
            }))
        })
    }
}

#[derive(Clone)]
struct ChatEditMessageSvc {
    state: Arc<Mutex<MockState>>,
}

impl tonic::server::UnaryService<edit_message_content::Request> for ChatEditMessageSvc {
    type Response = edit_message_content::Response;
    type Future = BoxFuture<Response<Self::Response>, Status>;

    fn call(&mut self, request: Request<edit_message_content::Request>) -> Self::Future {
        let state_handle = self.state.clone();
        Box::pin(async move {
            let user = authenticate(request.metadata(), &state_handle).await?;
            let input = request.into_inner();
            let mut state = state_handle.lock().await;
            let sub_ids = state.sub_ids_for_chat(&input.chat_object_id);
            let chat = state.chat_mut(&input.chat_object_id);

            let Some(message_index) = chat
                .messages
                .iter()
                .position(|msg| msg.id == input.message_id)
            else {
                return Ok(Response::new(edit_message_content::Response {
                    error: Some(edit_message_error(
                        edit_message_content::response::error::Code::UnknownError as i32,
                        "message not found",
                    )),
                }));
            };

            if chat.messages[message_index].creator != user {
                return Ok(Response::new(edit_message_content::Response {
                    error: Some(edit_message_error(
                        edit_message_content::response::error::Code::BadInput as i32,
                        "only the creator can edit messages",
                    )),
                }));
            }

            if let Some(edited) = input.edited_message {
                let message = &mut chat.messages[message_index];
                if let Some(content) = edited.message {
                    message.content = content;
                }
                message.attachments = edited.attachments;
                message.modified_at = Utc::now().timestamp_millis();
                chat.touch();
            }
            chat.next_state_id();

            if !sub_ids.is_empty() {
                let chat_state = build_chat_state(chat, &user);
                let proto_message = chat.messages[message_index].to_proto(&user);
                let event = build_event(
                    &input.chat_object_id,
                    vec![
                        chat_update_value(proto_message, sub_ids.clone()),
                        chat_state_update_value(chat_state, sub_ids),
                    ],
                );
                drop(state);
                broadcast_event(&state_handle, event).await;
            }

            Ok(Response::new(edit_message_content::Response {
                error: None,
            }))
        })
    }
}

#[derive(Clone)]
struct ChatDeleteMessageSvc {
    state: Arc<Mutex<MockState>>,
}

#[derive(Clone)]
struct ChatToggleReactionSvc {
    state: Arc<Mutex<MockState>>,
}

impl tonic::server::UnaryService<toggle_message_reaction::Request> for ChatToggleReactionSvc {
    type Response = toggle_message_reaction::Response;
    type Future = BoxFuture<Response<Self::Response>, Status>;

    fn call(&mut self, request: Request<toggle_message_reaction::Request>) -> Self::Future {
        let state_handle = self.state.clone();
        Box::pin(async move {
            let user = authenticate(request.metadata(), &state_handle).await?;
            let input = request.into_inner();
            let mut state = state_handle.lock().await;
            let chat = state.chat_mut(&input.chat_object_id);

            let Some(message) = chat
                .messages
                .iter_mut()
                .find(|msg| msg.id == input.message_id)
            else {
                return Ok(Response::new(toggle_message_reaction::Response {
                    error: Some(toggle_message_reaction::response::Error {
                        code: toggle_message_reaction::response::error::Code::UnknownError as i32,
                        description: "message not found".to_string(),
                    }),
                    added: false,
                }));
            };

            let entry = message
                .reactions
                .entry(input.emoji)
                .or_insert_with(HashSet::new);
            let added = if entry.contains(&user) {
                entry.remove(&user);
                false
            } else {
                entry.insert(user);
                true
            };

            chat.touch();
            chat.next_state_id();

            Ok(Response::new(toggle_message_reaction::Response {
                error: None,
                added,
            }))
        })
    }
}

#[derive(Clone)]
struct ChatReadAllSvc {
    state: Arc<Mutex<MockState>>,
}

impl tonic::server::UnaryService<read_all::Request> for ChatReadAllSvc {
    type Response = read_all::Response;
    type Future = BoxFuture<Response<Self::Response>, Status>;

    fn call(&mut self, request: Request<read_all::Request>) -> Self::Future {
        let state_handle = self.state.clone();
        Box::pin(async move {
            let user = authenticate(request.metadata(), &state_handle).await?;
            let mut state = state_handle.lock().await;
            for chat in state.chats.values_mut() {
                for message in chat.messages.iter_mut() {
                    message.read_by.insert(user.clone());
                    message.mention_read_by.insert(user.clone());
                }
                chat.next_state_id();
                chat.touch();
            }

            Ok(Response::new(read_all::Response { error: None }))
        })
    }
}

impl tonic::server::UnaryService<delete_message::Request> for ChatDeleteMessageSvc {
    type Response = delete_message::Response;
    type Future = BoxFuture<Response<Self::Response>, Status>;

    fn call(&mut self, request: Request<delete_message::Request>) -> Self::Future {
        let state_handle = self.state.clone();
        Box::pin(async move {
            let user = authenticate(request.metadata(), &state_handle).await?;
            let input = request.into_inner();
            let mut state = state_handle.lock().await;
            let sub_ids = state.sub_ids_for_chat(&input.chat_object_id);
            let chat = state.chat_mut(&input.chat_object_id);

            let Some(index) = chat
                .messages
                .iter()
                .position(|msg| msg.id == input.message_id)
            else {
                return Ok(Response::new(delete_message::Response {
                    error: Some(delete_message_error(
                        delete_message::response::error::Code::UnknownError as i32,
                        "message not found",
                    )),
                }));
            };

            if chat.messages[index].creator != user {
                return Ok(Response::new(delete_message::Response {
                    error: Some(delete_message_error(
                        delete_message::response::error::Code::BadInput as i32,
                        "only the creator can delete messages",
                    )),
                }));
            }

            let message_id = chat.messages[index].id.clone();
            chat.messages.remove(index);
            chat.next_state_id();
            chat.touch();

            if !sub_ids.is_empty() {
                let chat_state = build_chat_state(chat, &user);
                let event = build_event(
                    &input.chat_object_id,
                    vec![
                        chat_delete_value(message_id, sub_ids.clone()),
                        chat_state_update_value(chat_state, sub_ids),
                    ],
                );
                drop(state);
                broadcast_event(&state_handle, event).await;
            }

            Ok(Response::new(delete_message::Response { error: None }))
        })
    }
}

#[derive(Clone)]
struct ChatGetMessagesSvc {
    state: Arc<Mutex<MockState>>,
}

impl tonic::server::UnaryService<get_messages::Request> for ChatGetMessagesSvc {
    type Response = get_messages::Response;
    type Future = BoxFuture<Response<Self::Response>, Status>;

    fn call(&mut self, request: Request<get_messages::Request>) -> Self::Future {
        let state = self.state.clone();
        Box::pin(async move {
            let user = authenticate(request.metadata(), &state).await?;
            let input = request.into_inner();
            let state_guard = state.lock().await;
            let chat = state_guard
                .chats
                .get(&input.chat_object_id)
                .unwrap_or_else(|| {
                    state_guard
                        .chats
                        .get(DEFAULT_CHAT_ID)
                        .expect("default chat")
                });

            let mut messages = filter_messages(chat, &input, &user);
            if input.limit > 0 {
                messages.truncate(input.limit as usize);
            }
            let chat_state = build_chat_state(chat, &user);

            Ok(Response::new(get_messages::Response {
                error: None,
                messages,
                chat_state: Some(chat_state),
            }))
        })
    }
}

#[derive(Clone)]
struct ChatGetMessagesByIdsSvc {
    state: Arc<Mutex<MockState>>,
}

impl tonic::server::UnaryService<get_messages_by_ids::Request> for ChatGetMessagesByIdsSvc {
    type Response = get_messages_by_ids::Response;
    type Future = BoxFuture<Response<Self::Response>, Status>;

    fn call(&mut self, request: Request<get_messages_by_ids::Request>) -> Self::Future {
        let state = self.state.clone();
        Box::pin(async move {
            let user = authenticate(request.metadata(), &state).await?;
            let input = request.into_inner();
            let state_guard = state.lock().await;
            let chat = state_guard
                .chats
                .get(&input.chat_object_id)
                .unwrap_or_else(|| {
                    state_guard
                        .chats
                        .get(DEFAULT_CHAT_ID)
                        .expect("default chat")
                });

            let messages = chat
                .messages
                .iter()
                .filter(|msg| input.message_ids.contains(&msg.id))
                .map(|msg| msg.to_proto(&user))
                .collect();

            Ok(Response::new(get_messages_by_ids::Response {
                error: None,
                messages,
            }))
        })
    }
}

#[derive(Clone)]
struct ChatSubscribeLastMessagesSvc {
    state: Arc<Mutex<MockState>>,
}

impl tonic::server::UnaryService<subscribe_last_messages::Request>
    for ChatSubscribeLastMessagesSvc
{
    type Response = subscribe_last_messages::Response;
    type Future = BoxFuture<Response<Self::Response>, Status>;

    fn call(&mut self, request: Request<subscribe_last_messages::Request>) -> Self::Future {
        let state = self.state.clone();
        Box::pin(async move {
            let user = authenticate(request.metadata(), &state).await?;
            let input = request.into_inner();
            let mut state_guard = state.lock().await;
            state_guard.ensure_chat(&input.chat_object_id);
            state_guard.subscribe_chat(&input.chat_object_id, &input.sub_id);
            let chat = state_guard
                .chats
                .get(&input.chat_object_id)
                .expect("chat exists");

            let limit = if input.limit <= 0 {
                1
            } else {
                input.limit as usize
            };
            let total = chat.messages.len();
            let start = total.saturating_sub(limit);
            let messages = chat.messages[start..]
                .iter()
                .map(|msg| msg.to_proto(&user))
                .collect::<Vec<_>>();
            let num_messages_before = start as i32;
            let chat_state = build_chat_state(chat, &user);

            Ok(Response::new(subscribe_last_messages::Response {
                error: None,
                messages,
                num_messages_before,
                chat_state: Some(chat_state),
            }))
        })
    }
}

#[derive(Clone)]
struct ChatUnsubscribeSvc {
    state: Arc<Mutex<MockState>>,
}

impl tonic::server::UnaryService<unsubscribe::Request> for ChatUnsubscribeSvc {
    type Response = unsubscribe::Response;
    type Future = BoxFuture<Response<Self::Response>, Status>;

    fn call(&mut self, request: Request<unsubscribe::Request>) -> Self::Future {
        let state = self.state.clone();
        Box::pin(async move {
            let _user = authenticate(request.metadata(), &state).await?;
            let input = request.into_inner();
            let mut state_guard = state.lock().await;
            state_guard.unsubscribe_chat(&input.chat_object_id, &input.sub_id);
            Ok(Response::new(unsubscribe::Response { error: None }))
        })
    }
}

#[derive(Clone)]
struct ChatSubscribeToMessagePreviewsSvc {
    state: Arc<Mutex<MockState>>,
}

impl tonic::server::UnaryService<subscribe_to_message_previews::Request>
    for ChatSubscribeToMessagePreviewsSvc
{
    type Response = subscribe_to_message_previews::Response;
    type Future = BoxFuture<Response<Self::Response>, Status>;

    fn call(&mut self, request: Request<subscribe_to_message_previews::Request>) -> Self::Future {
        let state = self.state.clone();
        Box::pin(async move {
            let user = authenticate(request.metadata(), &state).await?;
            let input = request.into_inner();
            let mut state_guard = state.lock().await;
            state_guard
                .preview_subscriptions
                .insert(input.sub_id.clone());

            let chat_ids: Vec<String> = state_guard.chats.keys().cloned().collect();
            for chat_id in &chat_ids {
                state_guard.subscribe_chat(chat_id, &input.sub_id);
            }

            let mut previews = Vec::new();
            for chat_id in chat_ids {
                let chat = state_guard.chats.get(&chat_id).expect("chat exists");
                let message = chat.messages.last().map(|msg| msg.to_proto(&user));
                let chat_state = build_chat_state(chat, &user);
                previews.push(subscribe_to_message_previews::response::ChatPreview {
                    space_id: String::new(),
                    chat_object_id: chat_id,
                    message,
                    state: Some(chat_state),
                    dependencies: Vec::new(),
                });
            }

            Ok(Response::new(subscribe_to_message_previews::Response {
                error: None,
                previews,
            }))
        })
    }
}

#[derive(Clone)]
struct ChatUnsubscribeFromMessagePreviewsSvc {
    state: Arc<Mutex<MockState>>,
}

impl tonic::server::UnaryService<unsubscribe_from_message_previews::Request>
    for ChatUnsubscribeFromMessagePreviewsSvc
{
    type Response = unsubscribe_from_message_previews::Response;
    type Future = BoxFuture<Response<Self::Response>, Status>;

    fn call(
        &mut self,
        request: Request<unsubscribe_from_message_previews::Request>,
    ) -> Self::Future {
        let state = self.state.clone();
        Box::pin(async move {
            let _user = authenticate(request.metadata(), &state).await?;
            let input = request.into_inner();
            let mut state_guard = state.lock().await;
            state_guard.preview_subscriptions.remove(&input.sub_id);
            let chat_ids: Vec<String> = state_guard.chats.keys().cloned().collect();
            for chat_id in chat_ids {
                state_guard.unsubscribe_chat(&chat_id, &input.sub_id);
            }
            Ok(Response::new(unsubscribe_from_message_previews::Response {
                error: None,
            }))
        })
    }
}

#[derive(Clone)]
struct ListenSessionEventsSvc {
    state: Arc<Mutex<MockState>>,
}

struct EventStream {
    receiver: mpsc::Receiver<Event>,
    disconnect_epoch: Arc<AtomicU64>,
    epoch: u64,
    disconnected: bool,
    tick: Pin<Box<Sleep>>,
}

impl Stream for EventStream {
    type Item = Result<Event, Status>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if self.disconnect_epoch.load(Ordering::SeqCst) != self.epoch {
            if self.disconnected {
                return Poll::Ready(None);
            }
            self.disconnected = true;
            return Poll::Ready(Some(Err(Status::unavailable("mock stream disconnected"))));
        }
        match Pin::new(&mut self.receiver).poll_recv(cx) {
            Poll::Ready(Some(event)) => Poll::Ready(Some(Ok(event))),
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => {
                if self.tick.as_mut().poll(cx).is_ready() {
                    self.tick
                        .as_mut()
                        .reset(Instant::now() + Duration::from_millis(20));
                }
                Poll::Pending
            }
        }
    }
}

impl tonic::server::ServerStreamingService<StreamRequest> for ListenSessionEventsSvc {
    type Response = Event;
    type ResponseStream = EventStream;
    type Future = BoxFuture<Response<Self::ResponseStream>, Status>;

    fn call(&mut self, request: Request<StreamRequest>) -> Self::Future {
        let state = self.state.clone();
        Box::pin(async move {
            let _user = authenticate(request.metadata(), &state).await?;
            let (tx, rx) = mpsc::channel(64);
            let mut state_guard = state.lock().await;
            let (disconnect_epoch, epoch) = state_guard.add_listener(tx);
            Ok(Response::new(EventStream {
                receiver: rx,
                disconnect_epoch,
                epoch,
                disconnected: false,
                tick: Box::pin(tokio::time::sleep(Duration::from_millis(20))),
            }))
        })
    }
}

#[derive(Clone)]
struct ChatReadMessagesSvc {
    state: Arc<Mutex<MockState>>,
}

impl tonic::server::UnaryService<read_messages::Request> for ChatReadMessagesSvc {
    type Response = read_messages::Response;
    type Future = BoxFuture<Response<Self::Response>, Status>;

    fn call(&mut self, request: Request<read_messages::Request>) -> Self::Future {
        let state_handle = self.state.clone();
        Box::pin(async move {
            let user = authenticate(request.metadata(), &state_handle).await?;
            let input = request.into_inner();
            let mut state = state_handle.lock().await;
            let sub_ids = state.sub_ids_for_chat(&input.chat_object_id);
            let chat = state.chat_mut(&input.chat_object_id);

            let mut marked = false;
            for message in chat.messages.iter_mut() {
                if !order_in_read_range(&message.order_id, &input) {
                    continue;
                }
                if input.r#type == read_messages::ReadType::Mentions as i32 {
                    message.mention_read_by.insert(user.clone());
                } else {
                    message.read_by.insert(user.clone());
                }
                marked = true;
            }

            if marked {
                chat.next_state_id();
                chat.touch();
                if !sub_ids.is_empty() {
                    let chat_state = build_chat_state(chat, &user);
                    let event = build_event(
                        &input.chat_object_id,
                        vec![chat_state_update_value(chat_state, sub_ids)],
                    );
                    drop(state);
                    broadcast_event(&state_handle, event).await;
                }
            }

            Ok(Response::new(read_messages::Response {
                error: None,
                event: None,
            }))
        })
    }
}

#[derive(Clone)]
struct ChatUnreadMessagesSvc {
    state: Arc<Mutex<MockState>>,
}

impl tonic::server::UnaryService<unread::Request> for ChatUnreadMessagesSvc {
    type Response = unread::Response;
    type Future = BoxFuture<Response<Self::Response>, Status>;

    fn call(&mut self, request: Request<unread::Request>) -> Self::Future {
        let state_handle = self.state.clone();
        Box::pin(async move {
            let user = authenticate(request.metadata(), &state_handle).await?;
            let input = request.into_inner();
            let mut state = state_handle.lock().await;
            let sub_ids = state.sub_ids_for_chat(&input.chat_object_id);
            let chat = state.chat_mut(&input.chat_object_id);

            let mut marked = false;
            for message in chat.messages.iter_mut() {
                if !order_in_unread_range(&message.order_id, &input) {
                    continue;
                }
                if input.r#type == unread::ReadType::Mentions as i32 {
                    message.mention_read_by.remove(&user);
                } else {
                    message.read_by.remove(&user);
                }
                marked = true;
            }

            if marked {
                chat.next_state_id();
                chat.touch();
                if !sub_ids.is_empty() {
                    let chat_state = build_chat_state(chat, &user);
                    let event = build_event(
                        &input.chat_object_id,
                        vec![chat_state_update_value(chat_state, sub_ids)],
                    );
                    drop(state);
                    broadcast_event(&state_handle, event).await;
                }
            }

            Ok(Response::new(unread::Response {
                error: None,
                event: None,
            }))
        })
    }
}

#[derive(Clone)]
struct ObjectSearchWithMetaSvc {
    state: Arc<Mutex<MockState>>,
}

impl tonic::server::UnaryService<search_with_meta::Request> for ObjectSearchWithMetaSvc {
    type Response = search_with_meta::Response;
    type Future = BoxFuture<Response<Self::Response>, Status>;

    fn call(&mut self, request: Request<search_with_meta::Request>) -> Self::Future {
        let state_handle = self.state.clone();
        Box::pin(async move {
            let _user = authenticate(request.metadata(), &state_handle).await?;
            let input = request.into_inner();
            let state = state_handle.lock().await;

            let mut results = Vec::new();
            let text = input.full_text.to_lowercase();

            for (chat_id, chat) in &state.chats {
                if !input.space_id.is_empty() && input.space_id != chat.space_id {
                    continue;
                }
                if !text.is_empty() && !chat.name.to_lowercase().contains(&text) {
                    continue;
                }
                if !filters_match(&input.filters, chat_id, chat) {
                    continue;
                }
                results.push(model::search::Result {
                    object_id: chat_id.clone(),
                    details: Some(chat_details(chat_id, chat)),
                    meta: Vec::new(),
                });
            }

            Ok(Response::new(search_with_meta::Response {
                error: None,
                results,
            }))
        })
    }
}

#[derive(Clone)]
struct WorkspaceOpenSvc {
    state: Arc<Mutex<MockState>>,
}

impl tonic::server::UnaryService<workspace_open::Request> for WorkspaceOpenSvc {
    type Response = workspace_open::Response;
    type Future = BoxFuture<Response<Self::Response>, Status>;

    fn call(&mut self, request: Request<workspace_open::Request>) -> Self::Future {
        let state_handle = self.state.clone();
        Box::pin(async move {
            let _user = authenticate(request.metadata(), &state_handle).await?;
            let input = request.into_inner();
            let state = state_handle.lock().await;
            let chat_id = state
                .space_chats
                .get(&input.space_id)
                .cloned()
                .unwrap_or_default();
            Ok(Response::new(workspace_open::Response {
                error: None,
                info: Some(model::account::Info {
                    space_chat_id: chat_id,
                    ..Default::default()
                }),
            }))
        })
    }
}

impl StoredMessage {
    fn to_proto(&self, viewer: &str) -> model::ChatMessage {
        model::ChatMessage {
            id: self.id.clone(),
            order_id: self.order_id.clone(),
            creator: self.creator.clone(),
            created_at: self.created_at,
            modified_at: self.modified_at,
            state_id: self.state_id.clone(),
            reply_to_message_id: self.reply_to_message_id.clone(),
            message: Some(self.content.clone()),
            attachments: self.attachments.clone(),
            reactions: Some(reactions_to_proto(&self.reactions)),
            read: self.read_by.contains(viewer),
            mention_read: self.mention_read_by.contains(viewer),
            has_mention: self.has_mention,
            synced: self.synced,
        }
    }
}

fn reactions_to_proto(
    reactions: &HashMap<String, HashSet<String>>,
) -> model::chat_message::Reactions {
    let mut map = HashMap::new();
    for (emoji, ids) in reactions {
        map.insert(
            emoji.clone(),
            model::chat_message::reactions::IdentityList {
                ids: ids.iter().cloned().collect(),
            },
        );
    }
    model::chat_message::Reactions { reactions: map }
}

fn filters_match(
    filters: &[model::block::content::dataview::Filter],
    chat_id: &str,
    chat: &ChatRoom,
) -> bool {
    for filter in filters {
        if !filter_match(filter, chat_id, chat) {
            return false;
        }
    }
    true
}

fn filter_match(
    filter: &model::block::content::dataview::Filter,
    chat_id: &str,
    chat: &ChatRoom,
) -> bool {
    let condition = filter.condition;
    let value = &filter.value;
    match filter.relation_key.as_str() {
        "resolvedLayout" => {
            let expected = value.as_ref().and_then(number_value).unwrap_or_default() as i32;
            let actual = model::object_type::Layout::ChatDerived as i32;
            match_condition_i32(condition, actual, expected)
        }
        "id" => {
            let expected = value.as_ref().and_then(string_value).unwrap_or_default();
            match_condition_string(condition, chat_id, &expected)
        }
        "name" => {
            let expected = value.as_ref().and_then(string_value).unwrap_or_default();
            match_condition_string(condition, &chat.name, &expected)
        }
        _ => true,
    }
}

fn match_condition_string(condition: i32, actual: &str, expected: &str) -> bool {
    use model::block::content::dataview::filter::Condition;
    match condition {
        value if value == Condition::Equal as i32 => actual == expected,
        value if value == Condition::NotEqual as i32 => actual != expected,
        _ => true,
    }
}

fn match_condition_i32(condition: i32, actual: i32, expected: i32) -> bool {
    use model::block::content::dataview::filter::Condition;
    match condition {
        value if value == Condition::Equal as i32 => actual == expected,
        value if value == Condition::NotEqual as i32 => actual != expected,
        _ => true,
    }
}

fn string_value(value: &Value) -> Option<String> {
    match &value.kind {
        Some(prost_types::value::Kind::StringValue(text)) => Some(text.clone()),
        _ => None,
    }
}

fn number_value(value: &Value) -> Option<f64> {
    match &value.kind {
        Some(prost_types::value::Kind::NumberValue(number)) => Some(*number),
        _ => None,
    }
}

fn chat_details(chat_id: &str, chat: &ChatRoom) -> Struct {
    let mut fields = std::collections::BTreeMap::new();
    fields.insert("id".to_string(), value_string(chat_id.to_string()));
    fields.insert("name".to_string(), value_string(chat.name.clone()));
    fields.insert(
        "lastModifiedDate".to_string(),
        value_number(chat.last_modified as f64),
    );
    fields.insert(
        "resolvedLayout".to_string(),
        value_number(model::object_type::Layout::ChatDerived as i32 as f64),
    );
    fields.insert("type".to_string(), value_string("chat".to_string()));
    fields.insert("isArchived".to_string(), value_bool(chat.archived));
    fields.insert("spaceId".to_string(), value_string(chat.space_id.clone()));
    Struct { fields }
}

fn value_string(value: String) -> Value {
    Value {
        kind: Some(prost_types::value::Kind::StringValue(value)),
    }
}

fn value_number(value: f64) -> Value {
    Value {
        kind: Some(prost_types::value::Kind::NumberValue(value)),
    }
}

fn value_bool(value: bool) -> Value {
    Value {
        kind: Some(prost_types::value::Kind::BoolValue(value)),
    }
}

fn filter_messages(
    chat: &ChatRoom,
    input: &get_messages::Request,
    viewer: &str,
) -> Vec<model::ChatMessage> {
    chat.messages
        .iter()
        .filter(|message| order_in_range(&message.order_id, input))
        .map(|message| message.to_proto(viewer))
        .collect()
}

fn order_in_range(order_id: &str, input: &get_messages::Request) -> bool {
    if !input.after_order_id.is_empty() {
        if input.include_boundary {
            if order_id < input.after_order_id.as_str() {
                return false;
            }
        } else if order_id <= input.after_order_id.as_str() {
            return false;
        }
    }
    if !input.before_order_id.is_empty() {
        if input.include_boundary {
            if order_id > input.before_order_id.as_str() {
                return false;
            }
        } else if order_id >= input.before_order_id.as_str() {
            return false;
        }
    }
    true
}

fn order_in_read_range(order_id: &str, input: &read_messages::Request) -> bool {
    if !input.after_order_id.is_empty() && order_id <= input.after_order_id.as_str() {
        return false;
    }
    if !input.before_order_id.is_empty() && order_id >= input.before_order_id.as_str() {
        return false;
    }
    true
}

fn order_in_unread_range(order_id: &str, input: &unread::Request) -> bool {
    if !input.after_order_id.is_empty() && order_id <= input.after_order_id.as_str() {
        return false;
    }
    true
}

fn build_chat_state(chat: &ChatRoom, viewer: &str) -> model::ChatState {
    let unread: Vec<&StoredMessage> = chat
        .messages
        .iter()
        .filter(|message| !message.read_by.contains(viewer))
        .collect();
    let oldest = unread
        .first()
        .map(|message| message.order_id.clone())
        .unwrap_or_default();

    model::ChatState {
        messages: Some(model::chat_state::UnreadState {
            oldest_order_id: oldest,
            counter: unread.len() as i32,
        }),
        mentions: Some(model::chat_state::UnreadState {
            oldest_order_id: String::new(),
            counter: 0,
        }),
        last_state_id: chat.state_id.clone(),
        order: chat.state_counter,
    }
}

fn build_event(chat_id: &str, values: Vec<EventValue>) -> Event {
    Event {
        messages: values
            .into_iter()
            .map(|value| EventMessage {
                space_id: String::new(),
                value: Some(value),
            })
            .collect(),
        context_id: chat_id.to_string(),
        initiator: None,
        trace_id: String::new(),
    }
}

fn chat_add_value(
    message: model::ChatMessage,
    after_order_id: String,
    sub_ids: Vec<String>,
) -> EventValue {
    EventValue::ChatAdd(anytype_rpc::anytype::event::chat::Add {
        id: message.id.clone(),
        order_id: message.order_id.clone(),
        after_order_id,
        message: Some(message),
        sub_ids,
        dependencies: Vec::new(),
    })
}

fn chat_update_value(message: model::ChatMessage, sub_ids: Vec<String>) -> EventValue {
    EventValue::ChatUpdate(anytype_rpc::anytype::event::chat::Update {
        id: message.id.clone(),
        message: Some(message),
        sub_ids,
    })
}

fn chat_delete_value(message_id: String, sub_ids: Vec<String>) -> EventValue {
    EventValue::ChatDelete(anytype_rpc::anytype::event::chat::Delete {
        id: message_id,
        sub_ids,
    })
}

fn chat_state_update_value(state: model::ChatState, sub_ids: Vec<String>) -> EventValue {
    EventValue::ChatStateUpdate(anytype_rpc::anytype::event::chat::UpdateState {
        state: Some(state),
        sub_ids,
    })
}

async fn broadcast_event(state: &Arc<Mutex<MockState>>, event: Event) {
    let epoch = {
        let state_guard = state.lock().await;
        state_guard.disconnect_epoch.load(Ordering::SeqCst)
    };
    let listeners = {
        let mut state_guard = state.lock().await;
        std::mem::take(&mut state_guard.event_listeners)
    };

    if listeners.is_empty() {
        return;
    }

    let mut alive = Vec::new();
    for listener in listeners {
        if listener.sender.send(event.clone()).await.is_ok() {
            alive.push(listener);
        }
    }

    let mut state_guard = state.lock().await;
    if state_guard.disconnect_epoch.load(Ordering::SeqCst) == epoch {
        state_guard.event_listeners = alive;
    }
}

async fn authenticate(
    metadata: &MetadataMap,
    state: &Arc<Mutex<MockState>>,
) -> Result<String, Status> {
    let token = metadata
        .get("token")
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| Status::unauthenticated("missing token"))?;
    let state = state.lock().await;
    let user = state
        .token_user(token)
        .ok_or_else(|| Status::unauthenticated("invalid token"))?;
    Ok(user.to_string())
}

fn edit_message_error(code: i32, description: &str) -> edit_message_content::response::Error {
    edit_message_content::response::Error {
        code,
        description: description.to_string(),
    }
}

fn delete_message_error(code: i32, description: &str) -> delete_message::response::Error {
    delete_message::response::Error {
        code,
        description: description.to_string(),
    }
}
