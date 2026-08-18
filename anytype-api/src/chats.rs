//! # Anytype Chats
//!
//! REST backs operations where it provides the same fidelity. Structured
//! message publishing and full-fidelity reads remain available as gRPC
//! extensions because the HTTP message model omits structured blocks and chat
//! state. Plain messages and per-chat event streams use REST when accessed
//! through [`SpaceChatsClient`].
//!
//! Chat objects are identified by `chat_object_id` (a chat room/topic object).
//! Use `ChatClient::list_chats*` or `ChatClient::search_chats*` to discover chat
//! objects.
//!
//! Messages can include attachments by referencing file object ids. Use
//! `AnytypeClient::files()` to upload/download file objects and attach their ids
//! to messages.
//!
//! ## Example
//! ```rust,no_run
//! use anytype::prelude::*;
//! # async fn example(client: &AnytypeClient) -> Result<(), AnytypeError> {
//! let chat_id = "chat_object_id";
//! let page = client
//!     .chats()
//!     .list_messages(chat_id)
//!     .limit(20)
//!     .list_page()
//!     .await?;
//! println!("unread: {}", page.state.messages_unread);
//! println!(
//!     "latest message: {}",
//!     page.messages
//!         .first()
//!         .map(|m| &m.content.text)
//!         .unwrap_or(&"".into())
//! );
//! # Ok(())
//! # }
//! ```
//!
//! ## Open Questions
//! - Does `ListenSessionEvents` include all chat updates or only subscribed ones?
//! - Is `last_state_id` stable enough for resume, or should we use `order_id` only?
//! - Should previews and message subscriptions use separate `sub_id`s or a shared registry?

use std::{
    pin::Pin,
    task::{Context, Poll},
};

use anytype_rpc::anytype::rpc::{
    chat::{
        add_message, delete_message, edit_message_content, get_messages, get_messages_by_ids,
        read_all, read_messages, toggle_message_reaction, unread,
    },
    object::search_with_meta,
    workspace::open as workspace_open,
};
use anytype_rpc::model;
use chrono::{DateTime, Datelike, FixedOffset, SecondsFormat, TimeZone, Utc};
use futures::{Stream, StreamExt, stream::BoxStream};
use prost_types::{Struct, Value};
use reqwest::header::{ACCEPT, HeaderMap, HeaderValue};
use serde::{Deserialize, Serialize};
use tonic::Request;

use crate::{
    Result,
    client::AnytypeClient,
    error::AnytypeError,
    filters::{Query, QueryWithFilters},
    grpc_util::{ensure_error_ok, grpc_status, with_token_request},
    http_client::GetPaged,
    objects::{Color, DataModel, Icon, Object, ObjectLayout, ObjectResponse},
    paged::{PaginatedResponse, PaginationMeta},
    properties::{PropertyValue, PropertyWithValue},
    validation::looks_like_object_id,
};

const MAX_CHAT_REFERENCE_BYTES: usize = 256;
const MAX_CHAT_REACTION_BYTES: usize = 64;

// ============================================================================
// Public types
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ChatState {
    pub messages_unread: i32,
    pub mentions_unread: i32,
    pub last_state_id: String,
    pub order: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub messages_oldest_order_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mentions_oldest_order_id: Option<String>,
}

impl ChatState {
    /// Returns the oldest unread message order id, if available.
    #[must_use]
    pub fn oldest_unread_order_id(&self) -> Option<&str> {
        self.messages_oldest_order_id.as_deref()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatListResult {
    pub items: Vec<Object>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessagesPage {
    pub messages: Vec<ChatMessage>,
    pub state: ChatState,
}

/// Maximum number of messages returned by one older-history REST page.
pub const MAX_CHAT_HISTORY_PAGE_SIZE: u32 = 12;

/// Maximum encoded length of an opaque older-history anchor.
pub const MAX_MESSAGE_BEFORE_ANCHOR_BYTES: usize = 256;

/// Opaque server token that requests the next older REST message page.
///
/// The token is intentionally comparable only for equality. Callers must not
/// infer temporal or numeric order from its bytes.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct MessageBeforeAnchor(String);

impl std::fmt::Debug for MessageBeforeAnchor {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("MessageBeforeAnchor([redacted])")
    }
}

impl TryFrom<String> for MessageBeforeAnchor {
    type Error = AnytypeError;

    fn try_from(value: String) -> Result<Self> {
        if value.is_empty()
            || value.len() > MAX_MESSAGE_BEFORE_ANCHOR_BYTES
            || !value.bytes().all(|byte| byte.is_ascii_graphic())
        {
            return Err(AnytypeError::ChatHistoryEvidence {
                kind: ChatHistoryEvidenceKind::InvalidAnchor,
            });
        }
        Ok(Self(value))
    }
}

impl From<MessageBeforeAnchor> for String {
    fn from(anchor: MessageBeforeAnchor) -> Self {
        anchor.0
    }
}

impl MessageBeforeAnchor {
    fn into_inner(self) -> String {
        self.0
    }
}

/// One bounded server-ordered page of older REST chat messages.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessageHistoryPage {
    /// Messages in the exact oldest-to-newest order returned for this window.
    pub messages: Vec<ChatMessage>,
    /// Opaque anchor for the next older page, when this page was full.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_before: Option<MessageBeforeAnchor>,
}

/// Fixed field classification for a malformed chat timestamp.
#[derive(Clone, Copy, Debug, PartialEq, Eq, strum::Display)]
#[strum(serialize_all = "snake_case")]
pub enum ChatTimestampField {
    /// Message creation timestamp.
    CreatedAt,
    /// Message modification timestamp.
    ModifiedAt,
}

/// Closed classification for incomplete older-history evidence.
#[derive(Clone, Copy, Debug, PartialEq, Eq, strum::Display)]
#[strum(serialize_all = "snake_case")]
pub enum ChatHistoryEvidenceKind {
    /// An input or returned anchor exceeded its fixed safe wire contract.
    InvalidAnchor,
    /// A page contained the same message identity more than once.
    DuplicateMessageId,
    /// The server returned more rows than the requested bounded page size.
    TooManyMessages,
    /// A full page did not provide a usable final order token.
    MissingNextAnchor,
    /// A successor page returned the same anchor that it consumed.
    NonProgress,
}

/// Evidence from one supported REST edit and its exact readback.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessageEditEvidence {
    /// Canonical message state captured before PATCH dispatch.
    pub before: ChatMessage,
    /// Canonical message state independently fetched after PATCH dispatch.
    pub after: ChatMessage,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(clippy::struct_excessive_bools)]
pub struct ChatMessage {
    pub id: String,
    pub order_id: String,
    pub state_id: String,
    pub creator: String,
    /// Display name supplied by the REST API, when available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub creator_name: Option<String>,
    pub created_at: DateTime<FixedOffset>,
    pub modified_at: DateTime<FixedOffset>,
    pub reply_to_message_id: Option<String>,
    pub content: MessageContent,
    pub attachments: Vec<MessageAttachment>,
    pub reactions: Vec<MessageReaction>,
    pub read: bool,
    pub mention_read: bool,
    pub has_mention: bool,
    pub synced: bool,
    /// Whether the message is pinned.
    #[serde(default)]
    pub pinned: bool,
    /// Whether the current user has an unread reaction notification.
    #[serde(default)]
    pub unread_reaction: bool,
    /// Structured blocks carried by gRPC. REST message reads leave this empty.
    #[serde(default)]
    pub blocks: Vec<MessageBlock>,
}

/// Structured chat content available through the gRPC message API.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "content", rename_all = "snake_case")]
pub enum MessageBlock {
    Text(MessageBlockText),
    Link(MessageBlockLink),
    Embed(MessageBlockEmbed),
    EditorQuote(MessageBlockEditorQuote),
    MessageQuote(MessageBlockMessageQuote),
}

/// Text block with styling, marks, checkbox state, and language metadata.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MessageBlockText {
    /// Plain text stored in the block.
    pub text: String,
    /// Block-level text style.
    #[serde(default)]
    pub style: MessageTextStyle,
    /// Inline text marks.
    #[serde(default)]
    pub marks: Vec<MessageTextMark>,
    /// Checkbox state for checkbox-style blocks.
    #[serde(default)]
    pub checked: bool,
    /// Optional language identifier for code/text blocks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
}

/// Link to an Anytype object or file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageBlockLink {
    /// ID of the referenced Anytype object.
    pub target_object_id: String,
    /// Kind of referenced object.
    pub kind: MessageBlockLinkType,
}

/// Type of object referenced by a structured chat link block.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageBlockLinkType {
    Object,
    File,
    Image,
    Bookmark,
    Other(i32),
}

/// Embedded structured content, such as LaTeX.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageBlockEmbed {
    /// Source text or embed identifier.
    pub text: String,
    /// Processor used to render the embed.
    pub processor: MessageBlockProcessor,
}

/// Processor used for an embedded chat block.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageBlockProcessor {
    Latex,
    Mermaid,
    Graphviz,
    Other(i32),
}

/// Quote of a block from an editor object.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageBlockEditorQuote {
    /// ID of the quoted editor block.
    pub block_id: String,
    /// Snapshot of the quoted content, when supplied by the server.
    pub content: Option<MessageBlockText>,
}

/// Quote of another chat message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageBlockMessageQuote {
    /// ID of the quoted chat message.
    pub message_id: String,
    /// Participant that authored the quoted message.
    pub participant_id: String,
    /// Snapshot of the quoted content, when supplied by the server.
    pub content: Option<MessageBlockText>,
}

/// One result returned by REST full-text message search.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessageSearchResult {
    /// Matching chat message.
    pub message: ChatMessage,
    /// Server-provided relevance score.
    pub score: i64,
    /// Highlight snippet around the match.
    pub highlight: String,
    /// Matching ranges within the highlight.
    #[serde(default)]
    pub highlight_ranges: Vec<MessageTextRange>,
}

/// A page of REST message-search results.
#[derive(Debug, Serialize, Deserialize)]
pub struct ChatMessageSearchPage {
    /// Search results in this page.
    pub items: Vec<ChatMessageSearchResult>,
    /// Pagination metadata returned by the server.
    pub pagination: PaginationMeta,
}

/// Typed event received from the REST chat Server-Sent Events endpoint.
///
/// The `Unknown` variant preserves future server event kinds so adding a new
/// upstream event does not terminate an otherwise healthy stream.
#[derive(Debug, Clone)]
pub enum ChatHttpEvent {
    MessageAdded {
        message: ChatMessage,
    },
    MessageUpdated {
        message: ChatMessage,
    },
    MessageDeleted {
        message_id: String,
    },
    ReactionsUpdated {
        message_id: String,
        reactions: Vec<MessageReaction>,
    },
    Unknown {
        event_type: String,
        payload: serde_json::Value,
    },
}

/// Stream returned by [`ChatHttpMessageStreamRequest::open`].
///
/// Each item is one decoded SSE event. Heartbeat comments are consumed by the
/// parser and do not appear in the stream.
pub struct ChatHttpEventStream {
    inner: BoxStream<'static, Result<ChatHttpEvent>>,
}

impl Stream for ChatHttpEventStream {
    type Item = Result<ChatHttpEvent>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.inner.as_mut().poll_next(cx)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MessageContent {
    pub text: String,
    #[serde(default)]
    pub style: MessageTextStyle,
    #[serde(default)]
    pub marks: Vec<MessageTextMark>,
}

impl MessageContent {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Append text without styling
    #[must_use]
    pub fn text(mut self, value: impl AsRef<str>) -> Self {
        self.text.push_str(value.as_ref());
        self
    }

    /// append a newline to the text
    #[must_use]
    pub fn nl(mut self) -> Self {
        self.text.push('\n');
        self
    }

    /// Append boldface text
    #[must_use]
    pub fn bold(self, value: impl AsRef<str>) -> Self {
        self.push_marked_text(value.as_ref(), MessageTextMarkType::Bold, None)
    }

    /// Append italic text
    #[must_use]
    pub fn italic(self, value: impl AsRef<str>) -> Self {
        self.push_marked_text(value.as_ref(), MessageTextMarkType::Italic, None)
    }

    /// Append code-formatted text
    #[must_use]
    pub fn code(self, value: impl AsRef<str>) -> Self {
        self.push_marked_text(value.as_ref(), MessageTextMarkType::Keyboard, None)
    }

    /// Append a link
    #[must_use]
    pub fn link(self, title: impl AsRef<str>, url: impl Into<String>) -> Self {
        self.push_marked_text(title.as_ref(), MessageTextMarkType::Link, Some(url.into()))
    }

    /// Append emoji
    #[must_use]
    pub fn emoji(self, value: impl AsRef<str>) -> Self {
        self.push_marked_text(value.as_ref(), MessageTextMarkType::Emoji, None)
    }

    /// Append text with foreground color
    #[must_use]
    pub fn text_color(self, value: impl AsRef<str>, color: &Color) -> Self {
        self.push_marked_text(
            value.as_ref(),
            MessageTextMarkType::TextColor,
            Some(color.to_string()),
        )
    }

    /// Append text with foreground and background color
    #[must_use]
    pub fn text_color_bg(
        mut self,
        value: impl AsRef<str>,
        fg: impl AsRef<Color>,
        bg: impl AsRef<Color>,
    ) -> Self {
        let value = value.as_ref();
        #[allow(clippy::cast_possible_wrap, clippy::cast_possible_truncation)]
        let start = self.text.len() as i32;
        self.text.push_str(value);
        #[allow(clippy::cast_possible_wrap, clippy::cast_possible_truncation)]
        let end = self.text.len() as i32;

        let range = Some(MessageTextRange {
            from: start,
            to: end,
        });

        self.marks.push(MessageTextMark {
            range: range.clone(),
            kind: MessageTextMarkType::TextColor,
            param: Some(fg.as_ref().to_string()),
        });
        self.marks.push(MessageTextMark {
            range,
            kind: MessageTextMarkType::BackgroundColor,
            param: Some(bg.as_ref().to_string()),
        });
        self
    }

    fn push_marked_text(
        mut self,
        value: &str,
        kind: MessageTextMarkType,
        param: Option<String>,
    ) -> Self {
        #[allow(clippy::cast_possible_wrap, clippy::cast_possible_truncation)]
        let start = self.text.len() as i32;
        self.text.push_str(value);
        #[allow(clippy::cast_possible_wrap, clippy::cast_possible_truncation)]
        let end = self.text.len() as i32;

        self.marks.push(MessageTextMark {
            range: Some(MessageTextRange {
                from: start,
                to: end,
            }),
            kind,
            param,
        });
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, strum::EnumString, strum::Display)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum MessageAttachmentType {
    File,
    Image,
    Link,
    #[serde(untagged)]
    Other(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageAttachment {
    pub target: String,
    pub kind: MessageAttachmentType,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageReaction {
    pub emoji: String,
    pub identities: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageTextMark {
    pub range: Option<MessageTextRange>,
    pub kind: MessageTextMarkType,
    pub param: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageTextRange {
    pub from: i32,
    pub to: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize, strum::EnumString, strum::Display, Default)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum MessageTextStyle {
    #[default]
    Paragraph,
    Header1,
    Header2,
    Header3,
    Header4,
    Quote,
    Code,
    Title,
    Checkbox,
    Marked,
    Numbered,
    Toggle,
    ToggleHeader1,
    ToggleHeader2,
    ToggleHeader3,
    Description,
    Callout,
    #[serde(untagged)]
    Other(String),
}

#[derive(Debug, Clone, Serialize, Deserialize, strum::EnumString, strum::Display)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum MessageTextMarkType {
    Strikethrough,
    Keyboard,
    Italic,
    Bold,
    Underscored,
    Link,
    TextColor,
    BackgroundColor,
    Mention,
    Emoji,
    Object,
    #[serde(untagged)]
    Other(String),
}

#[derive(Debug, Clone, Serialize, Deserialize, strum::EnumString, strum::Display)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum ChatReadType {
    Messages,
    Mentions,
    #[serde(untagged)]
    Other(String),
}

// ============================================================================
// Client entry point
// ============================================================================

#[derive(Debug)]
pub struct ChatClient<'a> {
    client: &'a AnytypeClient,
}

impl AnytypeClient {
    /// Entry point for chat message operations.
    ///
    /// ```rust,no_run
    /// use anytype::prelude::*;
    /// # async fn example(client: &AnytypeClient) -> Result<(), AnytypeError> {
    /// let chat_id = "chat_object_id";
    /// let _page = client.chats().list_messages(chat_id).limit(5).list_page().await?;
    /// # Ok(())
    /// # }
    /// ```
    #[must_use]
    pub fn chats(&self) -> ChatClient<'_> {
        ChatClient { client: self }
    }
}

impl<'a> ChatClient<'a> {
    /// Scope REST chat operations to a space.
    ///
    /// The HTTP routes require both the space and chat IDs. Rich message
    /// publishing remains available directly on [`ChatClient`] through
    /// [`add_message`](Self::add_message) and [`edit_message`](Self::edit_message).
    pub fn in_space(&self, space_id: impl Into<String>) -> SpaceChatsClient<'a> {
        SpaceChatsClient {
            client: self.client,
            space_id: space_id.into(),
        }
    }

    /// List all chat objects (all spaces).
    #[must_use]
    pub fn list_chats(&self) -> ChatListRequest<'a> {
        ChatListRequest {
            client: self.client,
            space_id: None,
            limit: None,
            offset: None,
        }
    }

    /// List chat objects in a space.
    pub fn list_chats_in(&self, space_id: impl Into<String>) -> ChatListRequest<'a> {
        ChatListRequest {
            client: self.client,
            space_id: Some(space_id.into()),
            limit: None,
            offset: None,
        }
    }

    /// Search chat objects across all spaces.
    #[must_use]
    pub fn search_chats(&self) -> ChatSearchRequest<'a> {
        ChatSearchRequest {
            client: self.client,
            space_id: None,
            text: None,
            limit: None,
            offset: None,
        }
    }

    /// Search chat objects within a space.
    pub fn search_chats_in(&self, space_id: impl Into<String>) -> ChatSearchRequest<'a> {
        ChatSearchRequest {
            client: self.client,
            space_id: Some(space_id.into()),
            text: None,
            limit: None,
            offset: None,
        }
    }

    /// Get a chat object by id.
    pub fn get_chat(
        &self,
        space_id: impl Into<String>,
        chat_id: impl Into<String>,
    ) -> ChatGetRequest<'a> {
        ChatGetRequest {
            client: self.client,
            space_id: space_id.into(),
            chat_id: chat_id.into(),
        }
    }

    /// Resolve a chat id by its name (title).
    pub fn resolve_chat_by_name(
        &self,
        space_id: impl Into<String>,
        name: impl Into<String>,
    ) -> ChatResolveRequest<'a> {
        ChatResolveRequest {
            client: self.client,
            space_id: space_id.into(),
            name: name.into(),
        }
    }

    /// Get the default space chat object, given space id or name
    pub fn space_chat(&self, space_id_or_name: impl Into<String>) -> ChatSpaceRequest<'a> {
        ChatSpaceRequest {
            client: self.client,
            space_id_or_name: space_id_or_name.into(),
        }
    }

    /// Send a plain text message.
    pub fn send_text(
        &self,
        chat_object_id: impl Into<String>,
        text: impl Into<String>,
    ) -> ChatSendTextRequest<'a> {
        ChatSendTextRequest {
            client: self.client,
            chat_object_id: chat_object_id.into(),
            text: text.into(),
            style: MessageTextStyle::default(),
            marks: Vec::new(),
            attachments: Vec::new(),
            reply_to_message_id: None,
        }
    }

    /// Edit a message with plain text content.
    pub fn edit_text(
        &self,
        chat_object_id: impl Into<String>,
        message_id: impl Into<String>,
        text: impl Into<String>,
    ) -> ChatEditTextRequest<'a> {
        ChatEditTextRequest {
            client: self.client,
            chat_object_id: chat_object_id.into(),
            message_id: message_id.into(),
            text: text.into(),
            style: MessageTextStyle::default(),
            marks: Vec::new(),
        }
    }

    /// Toggle a reaction on a message.
    pub fn toggle_reaction(
        &self,
        chat_object_id: impl Into<String>,
        message_id: impl Into<String>,
        emoji: impl Into<String>,
    ) -> ChatToggleReactionRequest<'a> {
        ChatToggleReactionRequest {
            client: self.client,
            chat_object_id: chat_object_id.into(),
            message_id: message_id.into(),
            emoji: emoji.into(),
        }
    }

    /// Mark every chat known to the current Heart session as read.
    ///
    /// Heart's `ChatReadAll` request has no space field, so `space_id` does not
    /// scope the mutation. The argument is retained for API compatibility and
    /// validated before dispatch. Only call this operation when the account's
    /// complete chat inventory is safe to mutate.
    pub fn read_all(&self, space_id: impl Into<String>) -> ChatReadAllRequest<'a> {
        ChatReadAllRequest {
            client: self.client,
            space_id: space_id.into(),
        }
    }

    /// Add a message to a chat.
    ///
    /// ```rust,no_run
    /// use anytype::prelude::*;
    /// # async fn example(client: &AnytypeClient) -> Result<(), AnytypeError> {
    /// let message_id = client
    ///     .chats()
    ///     .add_message("chat_object_id")
    ///     .content(MessageContent {
    ///         text: "hello".to_string(),
    ///         style: MessageTextStyle::Paragraph,
    ///         marks: Vec::new(),
    ///     })
    ///     .send()
    ///     .await?;
    /// println!("message id: {message_id}");
    /// # Ok(())
    /// # }
    /// ```
    pub fn add_message(&self, chat_object_id: impl Into<String>) -> ChatAddMessageRequest<'a> {
        ChatAddMessageRequest {
            client: self.client,
            chat_object_id: chat_object_id.into(),
            content: None,
            attachments: Vec::new(),
            blocks: Vec::new(),
            reply_to_message_id: None,
        }
    }

    /// Edit a message in a chat.
    ///
    /// ```rust,no_run
    /// use anytype::prelude::*;
    /// # async fn example(client: &AnytypeClient) -> Result<(), AnytypeError> {
    /// client
    ///     .chats()
    ///     .edit_message("chat_object_id", "message_id")
    ///     .content(MessageContent {
    ///         text: "updated".to_string(),
    ///         style: MessageTextStyle::Paragraph,
    ///         marks: Vec::new(),
    ///     })
    ///     .send()
    ///     .await?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn edit_message(
        &self,
        chat_object_id: impl Into<String>,
        message_id: impl Into<String>,
    ) -> ChatEditMessageRequest<'a> {
        ChatEditMessageRequest {
            client: self.client,
            chat_object_id: chat_object_id.into(),
            message_id: message_id.into(),
            content: None,
            attachments: Vec::new(),
            blocks: Vec::new(),
        }
    }

    /// Delete a message in a chat.
    ///
    /// ```rust,no_run
    /// use anytype::prelude::*;
    /// # async fn example(client: &AnytypeClient) -> Result<(), AnytypeError> {
    /// client
    ///     .chats()
    ///     .delete_message("chat_object_id", "message_id")
    ///     .delete()
    ///     .await?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn delete_message(
        &self,
        chat_object_id: impl Into<String>,
        message_id: impl Into<String>,
    ) -> ChatDeleteMessageRequest<'a> {
        ChatDeleteMessageRequest {
            client: self.client,
            chat_object_id: chat_object_id.into(),
            message_id: message_id.into(),
        }
    }

    /// List messages in a chat.
    ///
    /// ```rust,no_run
    /// use anytype::prelude::*;
    /// # async fn example(client: &AnytypeClient) -> Result<(), AnytypeError> {
    /// let page = client
    ///     .chats()
    ///     .list_messages("chat_object_id")
    ///     .after("0000000000000005")
    ///     .limit(50)
    ///     .list_page()
    ///     .await?;
    /// println!("unread: {}", page.state.messages_unread);
    /// println!("messages: {}", page.messages.len());
    /// # Ok(())
    /// # }
    /// ```
    pub fn list_messages(&self, chat_object_id: impl Into<String>) -> ChatListMessagesRequest<'a> {
        ChatListMessagesRequest {
            client: self.client,
            chat_object_id: chat_object_id.into(),
            after: None,
            before: None,
            include_boundary: None,
            limit: None,
            unread_only: None,
        }
    }

    /// Get messages by id.
    ///
    /// ```rust,no_run
    /// use anytype::prelude::*;
    /// # async fn example(client: &AnytypeClient) -> Result<(), AnytypeError> {
    /// let messages = client
    ///     .chats()
    ///     .get_messages("chat_object_id", ["message_id"])
    ///     .get()
    ///     .await?;
    /// println!("messages: {}", messages.len());
    /// # Ok(())
    /// # }
    /// ```
    pub fn get_messages(
        &self,
        chat_object_id: impl Into<String>,
        ids: impl IntoIterator<Item = impl Into<String>>,
    ) -> ChatGetMessagesRequest<'a> {
        ChatGetMessagesRequest {
            client: self.client,
            chat_object_id: chat_object_id.into(),
            ids: ids.into_iter().map(Into::into).collect(),
        }
    }

    /// Mark messages as read.
    ///
    /// ```rust,no_run
    /// use anytype::prelude::*;
    /// # async fn example(client: &AnytypeClient) -> Result<(), AnytypeError> {
    /// client
    ///     .chats()
    ///     .read_messages("chat_object_id")
    ///     .read_type(ChatReadType::Messages)
    ///     .after("0000000000000005")
    ///     .mark_read()
    ///     .await?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn read_messages(&self, chat_object_id: impl Into<String>) -> ChatReadMessagesRequest<'a> {
        ChatReadMessagesRequest {
            client: self.client,
            chat_object_id: chat_object_id.into(),
            read_type: None,
            after: None,
            before: None,
            last_state_id: None,
        }
    }

    /// Mark messages as unread.
    ///
    /// ```rust,no_run
    /// use anytype::prelude::*;
    /// # async fn example(client: &AnytypeClient) -> Result<(), AnytypeError> {
    /// client
    ///     .chats()
    ///     .unread_messages("chat_object_id")
    ///     .read_type(ChatReadType::Messages)
    ///     .after("0000000000000005")
    ///     .mark_unread()
    ///     .await?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn unread_messages(
        &self,
        chat_object_id: impl Into<String>,
    ) -> ChatUnreadMessagesRequest<'a> {
        ChatUnreadMessagesRequest {
            client: self.client,
            chat_object_id: chat_object_id.into(),
            read_type: None,
            after: None,
        }
    }
}

// ============================================================================
// REST chat operations
// ============================================================================

/// REST chat operations scoped to one space.
#[derive(Debug)]
pub struct SpaceChatsClient<'a> {
    client: &'a AnytypeClient,
    space_id: String,
}

impl<'a> SpaceChatsClient<'a> {
    /// List chat objects using the REST API.
    pub fn list(&self) -> ChatHttpListRequest<'a> {
        ChatHttpListRequest {
            client: self.client,
            space_id: self.space_id.clone(),
            limit: None,
            offset: None,
            filters: Vec::new(),
        }
    }

    /// Create a chat using the REST API.
    pub fn create(&self, name: impl Into<String>, icon: Icon) -> ChatCreateRequest<'a> {
        ChatCreateRequest {
            client: self.client,
            space_id: self.space_id.clone(),
            name: name.into(),
            icon,
        }
    }

    /// List the HTTP representation of messages.
    ///
    /// HTTP messages include text style, marks, attachments, reactions, and
    /// pin state, but not gRPC structured blocks or per-user chat state. Use
    /// [`ChatClient::list_messages`] when full fidelity is required.
    pub fn list_messages(&self, chat_id: impl Into<String>) -> ChatHttpListMessagesRequest<'a> {
        ChatHttpListMessagesRequest {
            client: self.client,
            space_id: self.space_id.clone(),
            chat_id: chat_id.into(),
            before: None,
            after: None,
            limit: None,
        }
    }

    /// Read one bounded page of older REST messages in server order.
    ///
    /// The initial request has no anchor. A successor request accepts only the
    /// opaque [`MessageBeforeAnchor`] returned by the preceding page.
    pub fn older_messages(&self, chat_id: impl Into<String>) -> ChatMessageHistoryRequest<'a> {
        ChatMessageHistoryRequest {
            client: self.client,
            space_id: self.space_id.clone(),
            chat_id: chat_id.into(),
            before: None,
            limit: MAX_CHAT_HISTORY_PAGE_SIZE,
        }
    }

    /// Add a plain message using the REST API.
    ///
    /// Use [`ChatClient::add_message`] for structured gRPC blocks. REST
    /// supports text, style, inline marks, attachments, and replies.
    pub fn add_message(
        &self,
        chat_id: impl Into<String>,
        content: MessageContent,
    ) -> ChatHttpAddMessageRequest<'a> {
        ChatHttpAddMessageRequest {
            client: self.client,
            space_id: self.space_id.clone(),
            chat_id: chat_id.into(),
            content,
            attachments: Vec::new(),
            reply_to_message_id: None,
        }
    }

    /// Edit a plain message using the REST API.
    ///
    /// Use [`ChatClient::edit_message`] when structured gRPC blocks are
    /// required.
    pub fn edit_message(
        &self,
        chat_id: impl Into<String>,
        message_id: impl Into<String>,
        content: MessageContent,
    ) -> ChatHttpEditMessageRequest<'a> {
        ChatHttpEditMessageRequest {
            client: self.client,
            space_id: self.space_id.clone(),
            chat_id: chat_id.into(),
            message_id: message_id.into(),
            content,
            attachments: Vec::new(),
        }
    }

    /// Open the REST Server-Sent Events stream for one chat.
    pub fn message_stream(&self, chat_id: impl Into<String>) -> ChatHttpMessageStreamRequest<'a> {
        ChatHttpMessageStreamRequest {
            client: self.client,
            space_id: self.space_id.clone(),
            chat_id: chat_id.into(),
            limit: None,
            heartbeat_seconds: None,
        }
    }

    /// Get one message using the REST API.
    pub fn get_message(
        &self,
        chat_id: impl Into<String>,
        message_id: impl Into<String>,
    ) -> ChatGetMessageRequest<'a> {
        ChatGetMessageRequest {
            client: self.client,
            space_id: self.space_id.clone(),
            chat_id: chat_id.into(),
            message_id: message_id.into(),
        }
    }

    /// Search messages in one chat using the REST API.
    pub fn search_messages(
        &self,
        chat_id: impl Into<String>,
        query: impl Into<String>,
    ) -> ChatSearchMessagesRequest<'a> {
        ChatSearchMessagesRequest {
            client: self.client,
            space_id: self.space_id.clone(),
            chat_id: chat_id.into(),
            query: query.into(),
            limit: None,
            offset: None,
        }
    }

    /// Delete a message using the REST API.
    pub async fn delete_message(
        &self,
        chat_id: impl AsRef<str>,
        message_id: impl AsRef<str>,
    ) -> Result<()> {
        let path = chat_message_path(&self.space_id, chat_id.as_ref(), Some(message_id.as_ref()));
        self.client.client.delete_no_content(&path).await
    }

    /// Toggle a reaction using the REST API.
    pub async fn toggle_reaction(
        &self,
        chat_id: impl AsRef<str>,
        message_id: impl AsRef<str>,
        emoji: impl Into<String>,
    ) -> Result<()> {
        let path = format!(
            "{}/reactions",
            chat_message_path(&self.space_id, chat_id.as_ref(), Some(message_id.as_ref()))
        );
        self.client
            .client
            .post_request(
                &path,
                &ToggleReactionBody {
                    emoji: emoji.into(),
                },
                QueryWithFilters::default(),
            )
            .await
    }

    /// Mark a range of messages as read using the REST API.
    pub fn read_messages(&self, chat_id: impl Into<String>) -> ChatHttpReadMessagesRequest<'a> {
        ChatHttpReadMessagesRequest {
            client: self.client,
            space_id: self.space_id.clone(),
            chat_id: chat_id.into(),
            before: None,
            after: None,
            last_state_id: None,
            read_type: None,
        }
    }

    /// Mark reactions as read using the REST API.
    pub fn read_reactions(&self, chat_id: impl Into<String>) -> ChatReadReactionsRequest<'a> {
        ChatReadReactionsRequest {
            client: self.client,
            space_id: self.space_id.clone(),
            chat_id: chat_id.into(),
            order_id: None,
        }
    }

    /// Mark every message in a chat as read using the REST API.
    pub async fn read_all(&self, chat_id: impl AsRef<str>) -> Result<()> {
        let path = format!(
            "/v1/spaces/{}/chats/{}/read_all",
            self.space_id,
            chat_id.as_ref()
        );
        self.client
            .client
            .post_request(&path, &(), QueryWithFilters::default())
            .await
    }
}

/// Builder for listing chats over REST.
pub struct ChatHttpListRequest<'a> {
    client: &'a AnytypeClient,
    space_id: String,
    limit: Option<u32>,
    offset: Option<u32>,
    filters: Vec<crate::filters::Filter>,
}

impl ChatHttpListRequest<'_> {
    #[must_use]
    pub fn limit(mut self, limit: u32) -> Self {
        self.limit = Some(limit);
        self
    }

    #[must_use]
    pub fn offset(mut self, offset: u32) -> Self {
        self.offset = Some(offset);
        self
    }

    /// Add a dynamic property filter to the chat listing request.
    #[must_use]
    pub fn filter(mut self, filter: crate::filters::Filter) -> Self {
        self.filters.push(filter);
        self
    }

    pub async fn list(self) -> Result<crate::paged::PagedResult<Object>> {
        let query = Query::default()
            .set_limit_opt(self.limit)
            .set_offset_opt(self.offset)
            .add_filters(&self.filters);
        self.client
            .client
            .get_request_paged(&format!("/v1/spaces/{}/chats", self.space_id), query)
            .await
    }
}

/// Builder for creating a chat over REST.
pub struct ChatCreateRequest<'a> {
    client: &'a AnytypeClient,
    space_id: String,
    name: String,
    icon: Icon,
}

impl ChatCreateRequest<'_> {
    pub async fn create(self) -> Result<Object> {
        let response: ObjectResponse = self
            .client
            .client
            .post_request(
                &format!("/v1/spaces/{}/chats", self.space_id),
                &CreateChatBody {
                    name: self.name,
                    icon: self.icon,
                },
                QueryWithFilters::default(),
            )
            .await?;
        Ok(response.object)
    }
}

/// Builder for adding a plain chat message over REST.
pub struct ChatHttpAddMessageRequest<'a> {
    client: &'a AnytypeClient,
    space_id: String,
    chat_id: String,
    content: MessageContent,
    attachments: Vec<MessageAttachment>,
    reply_to_message_id: Option<String>,
}

impl ChatHttpAddMessageRequest<'_> {
    /// Set message attachments.
    #[must_use]
    pub fn attachments(mut self, attachments: Vec<MessageAttachment>) -> Self {
        self.attachments = attachments;
        self
    }

    /// Make this message a reply to an existing message.
    #[must_use]
    pub fn reply_to(mut self, message_id: impl Into<String>) -> Self {
        self.reply_to_message_id = Some(message_id.into());
        self
    }

    /// Send the message and return its server-assigned ID.
    pub async fn send(self) -> Result<String> {
        let response: AddChatMessageResponse = self
            .client
            .client
            .post_request(
                &chat_message_path(&self.space_id, &self.chat_id, None),
                &HttpMessageWriteBody::new(
                    self.content,
                    self.attachments,
                    self.reply_to_message_id,
                ),
                QueryWithFilters::default(),
            )
            .await?;
        Ok(response.message_id)
    }
}

/// Builder for editing a plain chat message over REST.
pub struct ChatHttpEditMessageRequest<'a> {
    client: &'a AnytypeClient,
    space_id: String,
    chat_id: String,
    message_id: String,
    content: MessageContent,
    attachments: Vec<MessageAttachment>,
}

impl ChatHttpEditMessageRequest<'_> {
    /// Replace the message attachments.
    #[must_use]
    pub fn attachments(mut self, attachments: Vec<MessageAttachment>) -> Self {
        self.attachments = attachments;
        self
    }

    /// Send the edit.
    pub async fn send(self) -> Result<()> {
        self.client
            .client
            .patch_request(
                &chat_message_path(&self.space_id, &self.chat_id, Some(&self.message_id)),
                &HttpMessageWriteBody::new(self.content, self.attachments, None),
            )
            .await
    }

    /// Send the edit and prove that an independent exact read advanced
    /// `modified_at`.
    ///
    /// This performs one exact GET before PATCH and one exact GET afterward.
    /// It returns fixed typed evidence failure when either read returns a
    /// different identity or the supported edit does not strictly advance the
    /// timestamp.
    pub async fn send_verified(self) -> Result<ChatMessageEditEvidence> {
        let ChatHttpEditMessageRequest {
            client,
            space_id,
            chat_id,
            message_id,
            content,
            attachments,
        } = self;
        let scoped = client.chats().in_space(&space_id);
        let before = scoped.get_message(&chat_id, &message_id).get().await?;
        validate_exact_chat_message(&before, &message_id)?;
        scoped
            .edit_message(&chat_id, &message_id, content)
            .attachments(attachments)
            .send()
            .await?;
        let after = scoped.get_message(&chat_id, &message_id).get().await?;
        validate_exact_chat_message(&after, &message_id)?;
        if after.modified_at <= before.modified_at {
            return Err(AnytypeError::ChatEditTimestampNotAdvanced);
        }
        Ok(ChatMessageEditEvidence { before, after })
    }
}

/// Builder for the REST chat Server-Sent Events endpoint.
pub struct ChatHttpMessageStreamRequest<'a> {
    client: &'a AnytypeClient,
    space_id: String,
    chat_id: String,
    limit: Option<u32>,
    heartbeat_seconds: Option<u32>,
}

impl ChatHttpMessageStreamRequest<'_> {
    /// Set the number of recent messages sent when the stream opens (1-1000).
    #[must_use]
    pub fn limit(mut self, limit: u32) -> Self {
        self.limit = Some(limit);
        self
    }

    /// Set the SSE heartbeat interval in seconds (1-60).
    #[must_use]
    pub fn heartbeat_seconds(mut self, seconds: u32) -> Self {
        self.heartbeat_seconds = Some(seconds);
        self
    }

    /// Open the SSE response and return a typed asynchronous event stream.
    ///
    /// Space and chat IDs must be nonempty path-safe identifiers containing
    /// only ASCII letters, digits, `.`, `_`, `~`, or `-`.
    pub async fn open(self) -> Result<ChatHttpEventStream> {
        validate_chat_stream_path_id("space_id", &self.space_id)?;
        validate_chat_stream_path_id("chat_id", &self.chat_id)?;
        if self.limit == Some(0) || self.limit.is_some_and(|limit| limit > 1000) {
            return Err(AnytypeError::Validation {
                message: "chat message stream limit must be between 1 and 1000".to_string(),
            });
        }
        if self
            .heartbeat_seconds
            .is_some_and(|seconds| !(1..=60).contains(&seconds))
        {
            return Err(AnytypeError::Validation {
                message: "chat message stream heartbeat must be between 1 and 60 seconds"
                    .to_string(),
            });
        }

        let query = Query::default().set_limit_opt(self.limit);
        let mut headers = HeaderMap::new();
        headers.insert(ACCEPT, HeaderValue::from_static("text/event-stream"));
        if let Some(seconds) = self.heartbeat_seconds {
            headers.insert(
                "Anytype-Heartbeat-Seconds",
                HeaderValue::from_str(&seconds.to_string()).map_err(|err| {
                    AnytypeError::Validation {
                        message: format!("invalid chat stream heartbeat header: {err}"),
                    }
                })?,
            );
        }
        let path = format!(
            "{}/stream",
            chat_message_path(&self.space_id, &self.chat_id, None)
        );
        let response = self
            .client
            .client
            .get_streaming_request(&path, query.into(), headers)
            .await?;
        let chunks = response.bytes_stream().boxed();
        let state = ChatHttpSseState {
            chunks,
            buffer: Vec::new(),
            pending: None,
            pending_offset: 0,
            finished: false,
            event_limit: self.client.config.response_limits.chat_sse_event_bytes,
        };
        let inner = futures::stream::unfold(state, |mut state| async move {
            loop {
                if state.finished {
                    return None;
                }
                if let Some(byte) = state
                    .pending
                    .as_ref()
                    .and_then(|chunk| chunk.get(state.pending_offset))
                    .copied()
                {
                    state.pending_offset += 1;
                    match append_sse_byte(&mut state.buffer, byte, state.event_limit) {
                        Ok(Some(delimiter_len)) => {
                            let frame_end = state.buffer.len() - delimiter_len;
                            let parsed = parse_chat_http_event(&state.buffer[..frame_end]);
                            state.buffer.clear();
                            match parsed {
                                Ok(Some(event)) => return Some((Ok(event), state)),
                                Ok(None) => continue,
                                Err(err) => return Some((Err(err), state)),
                            }
                        }
                        Ok(None) => {}
                        Err(err) => {
                            state.terminate();
                            return Some((Err(err), state));
                        }
                    }
                    continue;
                }
                state.pending = None;
                state.pending_offset = 0;
                match state.chunks.next().await {
                    Some(Ok(chunk)) => {
                        // Keep reqwest's immutable transport chunk without
                        // copying it into another unbounded allocation. The
                        // event buffer itself grows one checked byte at a time,
                        // so a chunk may safely contain several bounded events.
                        state.pending = Some(chunk);
                    }
                    Some(Err(error)) => {
                        state.terminate();
                        return Some((Err(error), state));
                    }
                    None => {
                        if let Some(final_buffer) = state.finish_at_eof() {
                            let parsed = parse_chat_http_event(&final_buffer);
                            drop(final_buffer);
                            match parsed {
                                Ok(Some(event)) => return Some((Ok(event), state)),
                                Ok(None) => continue,
                                Err(err) => return Some((Err(err), state)),
                            }
                        }
                    }
                }
            }
        })
        .boxed();
        Ok(ChatHttpEventStream { inner })
    }
}

struct ChatHttpSseState {
    chunks: BoxStream<'static, Result<bytes::Bytes>>,
    buffer: Vec<u8>,
    pending: Option<bytes::Bytes>,
    pending_offset: usize,
    finished: bool,
    event_limit: u64,
}

impl ChatHttpSseState {
    fn finish_at_eof(&mut self) -> Option<Vec<u8>> {
        self.finished = true;
        if self.buffer.is_empty() {
            None
        } else {
            Some(std::mem::take(&mut self.buffer))
        }
    }

    fn terminate(&mut self) {
        self.finished = true;
        self.buffer = Vec::new();
        self.pending = None;
        self.pending_offset = 0;
        self.chunks = futures::stream::empty().boxed();
    }
}

/// Builder for REST message listing.
pub struct ChatHttpListMessagesRequest<'a> {
    client: &'a AnytypeClient,
    space_id: String,
    chat_id: String,
    before: Option<String>,
    after: Option<String>,
    limit: Option<u32>,
}

impl ChatHttpListMessagesRequest<'_> {
    #[must_use]
    pub fn before(mut self, order_id: impl Into<String>) -> Self {
        self.before = Some(order_id.into());
        self
    }

    #[must_use]
    pub fn after(mut self, order_id: impl Into<String>) -> Self {
        self.after = Some(order_id.into());
        self
    }

    #[must_use]
    pub fn limit(mut self, limit: u32) -> Self {
        self.limit = Some(limit);
        self
    }

    pub async fn list(self) -> Result<Vec<ChatMessage>> {
        let query = Query::default()
            .set_limit_opt(self.limit)
            .add_param_opt("before_order_id", self.before)
            .add_param_opt("after_order_id", self.after);
        let response: HttpChatMessagesResponse = self
            .client
            .client
            .get_request(
                &chat_message_path(&self.space_id, &self.chat_id, None),
                query.into(),
            )
            .await?;
        response
            .messages
            .into_iter()
            .map(TryInto::try_into)
            .collect()
    }
}

/// Builder for bounded, server-ordered older REST message history.
pub struct ChatMessageHistoryRequest<'a> {
    client: &'a AnytypeClient,
    space_id: String,
    chat_id: String,
    before: Option<MessageBeforeAnchor>,
    limit: u32,
}

impl ChatMessageHistoryRequest<'_> {
    /// Use the opaque continuation returned by the preceding page.
    #[must_use]
    pub fn before(mut self, anchor: MessageBeforeAnchor) -> Self {
        self.before = Some(anchor);
        self
    }

    /// Set the page size from 1 through [`MAX_CHAT_HISTORY_PAGE_SIZE`].
    #[must_use]
    pub fn limit(mut self, limit: u32) -> Self {
        self.limit = limit;
        self
    }

    /// Execute one bounded older-history request.
    ///
    /// The response preserves Heart's oldest-to-newest order within each
    /// window. Successive windows move toward older history. A full page
    /// returns its first row's order token as the next opaque before-anchor. A
    /// short page is terminal.
    pub async fn get(self) -> Result<ChatMessageHistoryPage> {
        if !(1..=MAX_CHAT_HISTORY_PAGE_SIZE).contains(&self.limit) {
            return Err(AnytypeError::Validation {
                message: format!(
                    "chat history limit must be between 1 and {MAX_CHAT_HISTORY_PAGE_SIZE}"
                ),
            });
        }
        let requested_limit =
            usize::try_from(self.limit).map_err(|_| AnytypeError::ChatHistoryEvidence {
                kind: ChatHistoryEvidenceKind::TooManyMessages,
            })?;
        let consumed = self.before.clone();
        let query = Query::default()
            .set_limit_opt(Some(self.limit))
            .add_param_opt(
                "before_order_id",
                self.before.map(MessageBeforeAnchor::into_inner),
            );
        let response: HttpChatHistoryResponse = self
            .client
            .client
            .get_request(
                &chat_message_path(&self.space_id, &self.chat_id, None),
                query.into(),
            )
            .await?;
        let messages = decode_history_messages(response.messages, self.limit)?;
        let next_before = if messages.len() == requested_limit {
            let order_id = messages
                .first()
                .map(|message| message.order_id.clone())
                .ok_or(AnytypeError::ChatHistoryEvidence {
                    kind: ChatHistoryEvidenceKind::MissingNextAnchor,
                })?;
            let next = MessageBeforeAnchor::try_from(order_id).map_err(|_| {
                AnytypeError::ChatHistoryEvidence {
                    kind: ChatHistoryEvidenceKind::MissingNextAnchor,
                }
            })?;
            if consumed.as_ref() == Some(&next) {
                return Err(AnytypeError::ChatHistoryEvidence {
                    kind: ChatHistoryEvidenceKind::NonProgress,
                });
            }
            Some(next)
        } else {
            None
        };
        Ok(ChatMessageHistoryPage {
            messages,
            next_before,
        })
    }
}

/// Builder for fetching one REST chat message.
pub struct ChatGetMessageRequest<'a> {
    client: &'a AnytypeClient,
    space_id: String,
    chat_id: String,
    message_id: String,
}

impl ChatGetMessageRequest<'_> {
    pub async fn get(self) -> Result<ChatMessage> {
        let response: HttpChatMessageResponse = self
            .client
            .client
            .get_request(
                &chat_message_path(&self.space_id, &self.chat_id, Some(&self.message_id)),
                QueryWithFilters::default(),
            )
            .await?;
        response.message.try_into()
    }
}

/// Builder for REST full-text chat-message search.
pub struct ChatSearchMessagesRequest<'a> {
    client: &'a AnytypeClient,
    space_id: String,
    chat_id: String,
    query: String,
    limit: Option<u32>,
    offset: Option<u32>,
}

impl ChatSearchMessagesRequest<'_> {
    #[must_use]
    pub fn limit(mut self, limit: u32) -> Self {
        self.limit = Some(limit);
        self
    }

    #[must_use]
    pub fn offset(mut self, offset: u32) -> Self {
        self.offset = Some(offset);
        self
    }

    pub async fn search(self) -> Result<ChatMessageSearchPage> {
        if self.query.trim().is_empty() {
            return Err(AnytypeError::Validation {
                message: "chat message search query cannot be empty".to_string(),
            });
        }
        let query = Query::default()
            .add_param("query", self.query)
            .set_limit_opt(self.limit)
            .set_offset_opt(self.offset);
        let response: PaginatedResponse<HttpChatMessageSearchResult> = self
            .client
            .client
            .get_request(
                &format!(
                    "{}/search",
                    chat_message_path(&self.space_id, &self.chat_id, None)
                ),
                query.into(),
            )
            .await?;
        Ok(ChatMessageSearchPage {
            items: response
                .items
                .into_iter()
                .map(TryInto::try_into)
                .collect::<Result<Vec<_>>>()?,
            pagination: response.pagination,
        })
    }
}

/// Builder for marking messages read over REST.
pub struct ChatHttpReadMessagesRequest<'a> {
    client: &'a AnytypeClient,
    space_id: String,
    chat_id: String,
    before: Option<String>,
    after: Option<String>,
    last_state_id: Option<String>,
    read_type: Option<ChatReadType>,
}

impl ChatHttpReadMessagesRequest<'_> {
    #[must_use]
    pub fn before(mut self, order_id: impl Into<String>) -> Self {
        self.before = Some(order_id.into());
        self
    }

    #[must_use]
    pub fn after(mut self, order_id: impl Into<String>) -> Self {
        self.after = Some(order_id.into());
        self
    }

    #[must_use]
    pub fn last_state_id(mut self, state_id: impl Into<String>) -> Self {
        self.last_state_id = Some(state_id.into());
        self
    }

    #[must_use]
    pub fn read_type(mut self, read_type: ChatReadType) -> Self {
        self.read_type = Some(read_type);
        self
    }

    pub async fn mark_read(self) -> Result<()> {
        let path = format!(
            "{}/read",
            chat_message_path(&self.space_id, &self.chat_id, None)
        );
        self.client
            .client
            .post_request(
                &path,
                &ReadMessagesBody {
                    before_order_id: self.before,
                    after_order_id: self.after,
                    last_state_id: self.last_state_id,
                    read_type: self.read_type.map(|value| value.to_string()),
                },
                QueryWithFilters::default(),
            )
            .await
    }
}

/// Builder for marking reactions read over REST.
pub struct ChatReadReactionsRequest<'a> {
    client: &'a AnytypeClient,
    space_id: String,
    chat_id: String,
    order_id: Option<String>,
}

impl ChatReadReactionsRequest<'_> {
    #[must_use]
    pub fn through(mut self, order_id: impl Into<String>) -> Self {
        self.order_id = Some(order_id.into());
        self
    }

    pub async fn mark_read(self) -> Result<()> {
        self.client
            .client
            .post_request(
                &format!(
                    "/v1/spaces/{}/chats/{}/reactions/read",
                    self.space_id, self.chat_id
                ),
                &ReadReactionsBody {
                    order_id: self.order_id,
                },
                QueryWithFilters::default(),
            )
            .await
    }
}

fn chat_message_path(space_id: &str, chat_id: &str, message_id: Option<&str>) -> String {
    let mut path = format!("/v1/spaces/{space_id}/chats/{chat_id}/messages");
    if let Some(message_id) = message_id {
        path.push('/');
        path.push_str(message_id);
    }
    path
}

trait QueryOptionalParam {
    fn add_param_opt(self, name: &'static str, value: Option<String>) -> Self;
}

impl QueryOptionalParam for Query {
    fn add_param_opt(self, name: &'static str, value: Option<String>) -> Self {
        match value {
            Some(value) => self.add_param(name, value),
            None => self,
        }
    }
}

#[derive(Serialize)]
struct CreateChatBody {
    name: String,
    icon: Icon,
}

#[derive(Serialize)]
struct HttpMessageWriteBody {
    text: String,
    style: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    marks: Vec<HttpMessageWriteMark>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    attachments: Vec<HttpMessageWriteAttachment>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reply_to_message_id: Option<String>,
}

impl HttpMessageWriteBody {
    fn new(
        content: MessageContent,
        attachments: Vec<MessageAttachment>,
        reply_to_message_id: Option<String>,
    ) -> Self {
        Self {
            text: content.text,
            style: http_message_style(&content.style),
            marks: content
                .marks
                .into_iter()
                .map(|mark| {
                    let range = mark.range.unwrap_or(MessageTextRange { from: 0, to: 0 });
                    HttpMessageWriteMark {
                        from: range.from,
                        to: range.to,
                        kind: mark.kind.to_string(),
                        param: mark.param,
                    }
                })
                .collect(),
            attachments: attachments
                .into_iter()
                .map(|attachment| HttpMessageWriteAttachment {
                    target: attachment.target,
                    kind: attachment.kind.to_string(),
                })
                .collect(),
            reply_to_message_id,
        }
    }
}

#[derive(Serialize)]
struct HttpMessageWriteMark {
    from: i32,
    to: i32,
    #[serde(rename = "type")]
    kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    param: Option<String>,
}

#[derive(Serialize)]
struct HttpMessageWriteAttachment {
    target: String,
    #[serde(rename = "type")]
    kind: String,
}

fn http_message_style(style: &MessageTextStyle) -> String {
    match style {
        MessageTextStyle::Marked => "bulleted".to_string(),
        other => other.to_string(),
    }
}

#[derive(Serialize)]
struct ToggleReactionBody {
    emoji: String,
}

#[derive(Serialize)]
struct ReadMessagesBody {
    #[serde(skip_serializing_if = "Option::is_none")]
    before_order_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    after_order_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_state_id: Option<String>,
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    read_type: Option<String>,
}

#[derive(Serialize)]
struct ReadReactionsBody {
    #[serde(skip_serializing_if = "Option::is_none")]
    order_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AddChatMessageResponse {
    message_id: String,
}

#[derive(Debug, Deserialize)]
struct HttpChatMessagesResponse {
    #[serde(default)]
    messages: Vec<HttpChatMessage>,
}

#[derive(Debug, Deserialize)]
struct HttpChatHistoryResponse {
    messages: Vec<HttpChatMessage>,
}

#[derive(Debug, Deserialize)]
struct HttpChatMessageResponse {
    message: HttpChatMessage,
}

#[derive(Debug, Deserialize)]
struct HttpChatEventEnvelope {
    #[serde(rename = "type")]
    event_type: String,
    payload: serde_json::Value,
}

#[derive(Debug, Deserialize)]
struct HttpChatMessageDeletedPayload {
    id: String,
}

#[derive(Debug, Deserialize)]
struct HttpChatReactionsUpdatedPayload {
    id: String,
    #[serde(default)]
    reactions: std::collections::HashMap<String, Vec<String>>,
}

#[derive(Debug, Deserialize)]
struct HttpChatMessageSearchResult {
    message: HttpChatMessage,
    score: i64,
    highlight: String,
    #[serde(default)]
    highlight_ranges: Vec<MessageTextRange>,
}

#[derive(Debug, Deserialize)]
struct HttpChatMessage {
    id: String,
    order_id: String,
    creator: String,
    #[serde(default)]
    creator_name: String,
    created_at: i64,
    modified_at: i64,
    #[serde(default)]
    reply_to_message_id: String,
    content: HttpMessageContent,
    #[serde(default)]
    attachments: Vec<HttpMessageAttachment>,
    #[serde(default)]
    reactions: std::collections::HashMap<String, Vec<String>>,
    #[serde(default)]
    pinned: bool,
}

#[derive(Debug, Deserialize)]
struct HttpMessageContent {
    text: String,
    #[serde(default)]
    style: String,
    #[serde(default)]
    marks: Vec<HttpMessageMark>,
}

#[derive(Debug, Deserialize)]
struct HttpMessageMark {
    from: i32,
    to: i32,
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    param: String,
}

#[derive(Debug, Deserialize)]
struct HttpMessageAttachment {
    target: String,
    #[serde(rename = "type")]
    kind: String,
}

impl TryFrom<HttpChatMessage> for ChatMessage {
    type Error = AnytypeError;

    fn try_from(message: HttpChatMessage) -> Result<Self> {
        let reactions = http_reactions(message.reactions);
        Ok(Self {
            id: message.id,
            order_id: message.order_id,
            state_id: String::new(),
            creator: message.creator,
            creator_name: empty_to_none(message.creator_name),
            created_at: timestamp_to_datetime(message.created_at, ChatTimestampField::CreatedAt)?,
            modified_at: timestamp_to_datetime(
                message.modified_at,
                ChatTimestampField::ModifiedAt,
            )?,
            reply_to_message_id: empty_to_none(message.reply_to_message_id),
            content: MessageContent {
                text: message.content.text,
                style: parse_message_style(message.content.style),
                marks: message
                    .content
                    .marks
                    .into_iter()
                    .map(|mark| MessageTextMark {
                        range: Some(MessageTextRange {
                            from: mark.from,
                            to: mark.to,
                        }),
                        kind: parse_message_mark_type(mark.kind),
                        param: empty_to_none(mark.param),
                    })
                    .collect(),
            },
            attachments: message
                .attachments
                .into_iter()
                .map(|attachment| MessageAttachment {
                    target: attachment.target,
                    kind: parse_attachment_type(attachment.kind),
                })
                .collect(),
            reactions,
            read: false,
            mention_read: false,
            has_mention: false,
            synced: false,
            pinned: message.pinned,
            unread_reaction: false,
            blocks: Vec::new(),
        })
    }
}

fn decode_history_messages(messages: Vec<HttpChatMessage>, limit: u32) -> Result<Vec<ChatMessage>> {
    let limit = usize::try_from(limit).map_err(|_| AnytypeError::ChatHistoryEvidence {
        kind: ChatHistoryEvidenceKind::TooManyMessages,
    })?;
    if messages.len() > limit {
        return Err(AnytypeError::ChatHistoryEvidence {
            kind: ChatHistoryEvidenceKind::TooManyMessages,
        });
    }
    let mut seen = std::collections::HashSet::with_capacity(messages.len());
    let mut decoded = Vec::with_capacity(messages.len());
    for message in messages {
        let message = ChatMessage::try_from(message)?;
        if !seen.insert(message.id.clone()) {
            return Err(AnytypeError::ChatHistoryEvidence {
                kind: ChatHistoryEvidenceKind::DuplicateMessageId,
            });
        }
        decoded.push(message);
    }
    Ok(decoded)
}

fn validate_exact_chat_message(message: &ChatMessage, expected_id: &str) -> Result<()> {
    if message.id != expected_id {
        return Err(AnytypeError::Validation {
            message: "chat message response identity did not match the request".to_string(),
        });
    }
    Ok(())
}

fn http_reactions(
    reactions: std::collections::HashMap<String, Vec<String>>,
) -> Vec<MessageReaction> {
    let mut reactions: Vec<MessageReaction> = reactions
        .into_iter()
        .map(|(emoji, identities)| MessageReaction { emoji, identities })
        .collect();
    reactions.sort_by(|left, right| left.emoji.cmp(&right.emoji));
    reactions
}

fn append_sse_byte(buffer: &mut Vec<u8>, byte: u8, limit: u64) -> Result<Option<usize>> {
    let current =
        u64::try_from(buffer.len()).map_err(|_| AnytypeError::ChatSseEventTooLarge { limit })?;
    let next = current
        .checked_add(1)
        .ok_or(AnytypeError::ChatSseEventTooLarge { limit })?;
    if next > limit {
        return Err(AnytypeError::ChatSseEventTooLarge { limit });
    }
    buffer.push(byte);
    if buffer.ends_with(b"\r\n\r\n") {
        Ok(Some(4))
    } else if buffer.ends_with(b"\n\n") {
        Ok(Some(2))
    } else {
        Ok(None)
    }
}

fn validate_chat_stream_path_id(name: &'static str, id: &str) -> Result<()> {
    const MAX_PATH_ID_CHARS: usize = 256;
    if id.is_empty()
        || matches!(id, "." | "..")
        || id.chars().count() > MAX_PATH_ID_CHARS
        || !id
            .bytes()
            .all(|character| character.is_ascii_alphanumeric() || b"._~-".contains(&character))
    {
        return Err(AnytypeError::Validation {
            message: format!("{name} must be a nonempty safe path identifier"),
        });
    }
    Ok(())
}

#[cfg(test)]
fn chat_stream_diagnostic_path(url: &reqwest::Url) -> String {
    url.path().to_owned()
}

fn parse_chat_http_event(frame: &[u8]) -> Result<Option<ChatHttpEvent>> {
    let frame = std::str::from_utf8(frame).map_err(|err| AnytypeError::Other {
        message: format!("chat SSE event is not valid UTF-8: {err}"),
    })?;
    let mut data_lines = Vec::new();
    for line in frame.lines() {
        let line = line.trim_end_matches('\r');
        if line.starts_with(':') {
            continue;
        }
        if let Some(data) = line.strip_prefix("data:") {
            data_lines.push(data.strip_prefix(' ').unwrap_or(data));
        }
    }
    if data_lines.is_empty() {
        return Ok(None);
    }
    let envelope: HttpChatEventEnvelope = serde_json::from_str(&data_lines.join("\n"))
        .map_err(|source| AnytypeError::Deserialization { source })?;
    let event = match envelope.event_type.as_str() {
        "message_added" => {
            let payload: HttpChatMessageResponse = serde_json::from_value(envelope.payload)
                .map_err(|source| AnytypeError::Deserialization { source })?;
            ChatHttpEvent::MessageAdded {
                message: payload.message.try_into()?,
            }
        }
        "message_updated" => {
            let payload: HttpChatMessageResponse = serde_json::from_value(envelope.payload)
                .map_err(|source| AnytypeError::Deserialization { source })?;
            ChatHttpEvent::MessageUpdated {
                message: payload.message.try_into()?,
            }
        }
        "message_deleted" => {
            let payload: HttpChatMessageDeletedPayload =
                serde_json::from_value(envelope.payload)
                    .map_err(|source| AnytypeError::Deserialization { source })?;
            ChatHttpEvent::MessageDeleted {
                message_id: payload.id,
            }
        }
        "reactions_updated" => {
            let payload: HttpChatReactionsUpdatedPayload = serde_json::from_value(envelope.payload)
                .map_err(|source| AnytypeError::Deserialization { source })?;
            ChatHttpEvent::ReactionsUpdated {
                message_id: payload.id,
                reactions: http_reactions(payload.reactions),
            }
        }
        _ => ChatHttpEvent::Unknown {
            event_type: envelope.event_type,
            payload: envelope.payload,
        },
    };
    Ok(Some(event))
}

impl TryFrom<HttpChatMessageSearchResult> for ChatMessageSearchResult {
    type Error = AnytypeError;

    fn try_from(result: HttpChatMessageSearchResult) -> Result<Self> {
        Ok(Self {
            message: result.message.try_into()?,
            score: result.score,
            highlight: result.highlight,
            highlight_ranges: result.highlight_ranges,
        })
    }
}

fn parse_message_style(value: String) -> MessageTextStyle {
    if value == "bulleted" {
        MessageTextStyle::Marked
    } else {
        value.parse().unwrap_or(MessageTextStyle::Other(value))
    }
}

fn parse_message_mark_type(value: String) -> MessageTextMarkType {
    value.parse().unwrap_or(MessageTextMarkType::Other(value))
}

fn parse_attachment_type(value: String) -> MessageAttachmentType {
    value.parse().unwrap_or(MessageAttachmentType::Other(value))
}

// ============================================================================
// Request builders
// ============================================================================

pub struct ChatListRequest<'a> {
    client: &'a AnytypeClient,
    space_id: Option<String>,
    limit: Option<u32>,
    offset: Option<u32>,
}

impl ChatListRequest<'_> {
    #[must_use]
    pub fn limit(mut self, limit: u32) -> Self {
        self.limit = Some(limit);
        self
    }

    #[must_use]
    pub fn offset(mut self, offset: u32) -> Self {
        self.offset = Some(offset);
        self
    }

    pub async fn list(self) -> Result<ChatListResult> {
        if let Some(space_id) = self.space_id {
            let result = SpaceChatsClient {
                client: self.client,
                space_id,
            }
            .list()
            .limit(self.limit.unwrap_or(100))
            .offset(self.offset.unwrap_or_default())
            .list()
            .await?;
            return Ok(ChatListResult {
                items: result.into_response().items,
            });
        }
        chat_search(self.client, None, None, Vec::new(), self.limit, self.offset).await
    }
}

pub struct ChatSearchRequest<'a> {
    client: &'a AnytypeClient,
    space_id: Option<String>,
    text: Option<String>,
    limit: Option<u32>,
    offset: Option<u32>,
}

impl ChatSearchRequest<'_> {
    #[must_use]
    pub fn text(mut self, text: impl Into<String>) -> Self {
        self.text = Some(text.into());
        self
    }

    #[must_use]
    pub fn limit(mut self, limit: u32) -> Self {
        self.limit = Some(limit);
        self
    }

    #[must_use]
    pub fn offset(mut self, offset: u32) -> Self {
        self.offset = Some(offset);
        self
    }

    pub async fn search(self) -> Result<ChatListResult> {
        chat_search(
            self.client,
            self.space_id,
            self.text,
            Vec::new(),
            self.limit,
            self.offset,
        )
        .await
    }
}

pub struct ChatGetRequest<'a> {
    client: &'a AnytypeClient,
    space_id: String,
    chat_id: String,
}

impl ChatGetRequest<'_> {
    pub async fn get(self) -> Result<Object> {
        let result = chat_search(
            self.client,
            Some(self.space_id.clone()),
            None,
            vec![filter_id_equal(&self.chat_id)],
            Some(1),
            None,
        )
        .await?;
        result
            .items
            .into_iter()
            .next()
            .ok_or_else(|| AnytypeError::NotFound {
                obj_type: "chat".to_string(),
                key: self.chat_id,
            })
    }
}

pub struct ChatResolveRequest<'a> {
    client: &'a AnytypeClient,
    space_id: String,
    name: String,
}

impl ChatResolveRequest<'_> {
    pub async fn resolve(self) -> Result<String> {
        let result = chat_search(
            self.client,
            Some(self.space_id.clone()),
            None,
            vec![filter_name_equal(&self.name)],
            Some(1),
            None,
        )
        .await?;
        result
            .items
            .into_iter()
            .next()
            .map(|obj| obj.id)
            .ok_or_else(|| AnytypeError::NotFound {
                obj_type: "chat".to_string(),
                key: self.name,
            })
    }
}

pub struct ChatSpaceRequest<'a> {
    client: &'a AnytypeClient,
    space_id_or_name: String,
}

impl ChatSpaceRequest<'_> {
    pub async fn get(self) -> Result<Object> {
        let space_id = if looks_like_object_id(&self.space_id_or_name) {
            self.space_id_or_name
        } else {
            self.client
                .lookup_space_by_name(&self.space_id_or_name)
                .await?
                .id
        };

        let grpc = self.client.grpc_client().await?;
        let mut commands = grpc.client_commands();
        let request = workspace_open::Request {
            space_id: space_id.clone(),
            with_chat: false,
        };
        let request = with_token_request(Request::new(request), grpc.token())?;
        let response = commands
            .workspace_open(request)
            .await
            .map_err(grpc_status)?
            .into_inner();
        ensure_error_ok(response.error.as_ref(), "workspace open")?;
        let info = response.info.ok_or_else(|| AnytypeError::Other {
            message: "workspace open missing info".to_string(),
        })?;
        if info.space_chat_id.is_empty() {
            return Err(AnytypeError::NotFound {
                obj_type: "chat".to_string(),
                key: "space_chat_id".to_string(),
            });
        }
        ChatGetRequest {
            client: self.client,
            space_id,
            chat_id: info.space_chat_id,
        }
        .get()
        .await
    }
}

pub struct ChatSendTextRequest<'a> {
    client: &'a AnytypeClient,
    chat_object_id: String,
    text: String,
    style: MessageTextStyle,
    marks: Vec<MessageTextMark>,
    attachments: Vec<MessageAttachment>,
    reply_to_message_id: Option<String>,
}

impl ChatSendTextRequest<'_> {
    #[must_use]
    pub fn style(mut self, style: MessageTextStyle) -> Self {
        self.style = style;
        self
    }

    #[must_use]
    pub fn marks(mut self, marks: Vec<MessageTextMark>) -> Self {
        self.marks = marks;
        self
    }

    #[must_use]
    pub fn attachments(mut self, attachments: Vec<MessageAttachment>) -> Self {
        self.attachments = attachments;
        self
    }

    /// Reply to an existing message.
    #[must_use]
    pub fn reply_to(mut self, message_id: impl Into<String>) -> Self {
        self.reply_to_message_id = Some(message_id.into());
        self
    }

    pub async fn send(self) -> Result<String> {
        validate_chat_reference("chat object id", &self.chat_object_id)?;
        validate_chat_text(&self.text, self.client.get_config().limits.markdown_max_len)?;
        ChatAddMessageRequest {
            client: self.client,
            chat_object_id: self.chat_object_id,
            content: Some(MessageContent {
                text: self.text,
                style: self.style,
                marks: self.marks,
            }),
            attachments: self.attachments,
            blocks: Vec::new(),
            reply_to_message_id: self.reply_to_message_id,
        }
        .send()
        .await
    }
}

pub struct ChatEditTextRequest<'a> {
    client: &'a AnytypeClient,
    chat_object_id: String,
    message_id: String,
    text: String,
    style: MessageTextStyle,
    marks: Vec<MessageTextMark>,
}

impl ChatEditTextRequest<'_> {
    #[must_use]
    pub fn style(mut self, style: MessageTextStyle) -> Self {
        self.style = style;
        self
    }

    #[must_use]
    pub fn marks(mut self, marks: Vec<MessageTextMark>) -> Self {
        self.marks = marks;
        self
    }

    pub async fn send(self) -> Result<()> {
        ChatEditMessageRequest {
            client: self.client,
            chat_object_id: self.chat_object_id,
            message_id: self.message_id,
            content: Some(MessageContent {
                text: self.text,
                style: self.style,
                marks: self.marks,
            }),
            attachments: Vec::new(),
            blocks: Vec::new(),
        }
        .send()
        .await
    }
}

pub struct ChatToggleReactionRequest<'a> {
    client: &'a AnytypeClient,
    chat_object_id: String,
    message_id: String,
    emoji: String,
}

impl ChatToggleReactionRequest<'_> {
    pub async fn send(self) -> Result<bool> {
        validate_chat_reference("chat object id", &self.chat_object_id)?;
        validate_chat_reference("chat message id", &self.message_id)?;
        validate_chat_reaction(&self.emoji)?;
        let grpc = self.client.grpc_client().await?;
        let mut commands = grpc.client_commands();
        let request = toggle_message_reaction::Request {
            chat_object_id: self.chat_object_id,
            message_id: self.message_id,
            emoji: self.emoji,
        };
        let request = with_token_request(Request::new(request), grpc.token())?;
        let response = commands
            .chat_toggle_message_reaction(request)
            .await
            .map_err(grpc_status)?
            .into_inner();
        ensure_error_ok(response.error.as_ref(), "chat toggle reaction")?;
        Ok(response.added)
    }
}

pub struct ChatReadAllRequest<'a> {
    client: &'a AnytypeClient,
    space_id: String,
}

impl ChatReadAllRequest<'_> {
    pub async fn mark_read(self) -> Result<()> {
        validate_chat_reference("space id", &self.space_id)?;
        let grpc = self.client.grpc_client().await?;
        let mut commands = grpc.client_commands();
        let request = read_all::Request {};
        let request = with_token_request(Request::new(request), grpc.token())?;
        let response = commands
            .chat_read_all(request)
            .await
            .map_err(grpc_status)?
            .into_inner();
        ensure_error_ok(response.error.as_ref(), "chat read all")?;
        Ok(())
    }
}

fn validate_chat_reference(description: &str, value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > MAX_CHAT_REFERENCE_BYTES
        || value.chars().any(char::is_control)
    {
        return Err(AnytypeError::Validation {
            message: format!(
                "{description} must be nonempty, at most {MAX_CHAT_REFERENCE_BYTES} bytes, and control-free"
            ),
        });
    }
    Ok(())
}

fn validate_chat_text(text: &str, maximum_bytes: u64) -> Result<()> {
    let text_bytes = u64::try_from(text.len()).map_err(|_| AnytypeError::Validation {
        message: "chat text exceeds the configured size limit".to_owned(),
    })?;
    if text.is_empty() || text_bytes > maximum_bytes {
        return Err(AnytypeError::Validation {
            message: format!("chat text must be nonempty and at most {maximum_bytes} bytes"),
        });
    }
    Ok(())
}

fn validate_chat_reaction(emoji: &str) -> Result<()> {
    if emoji.is_empty()
        || emoji.len() > MAX_CHAT_REACTION_BYTES
        || emoji.chars().any(char::is_control)
    {
        return Err(AnytypeError::Validation {
            message: format!(
                "chat reaction must be nonempty, at most {MAX_CHAT_REACTION_BYTES} bytes, and control-free"
            ),
        });
    }
    Ok(())
}

pub struct ChatAddMessageRequest<'a> {
    client: &'a AnytypeClient,
    chat_object_id: String,
    content: Option<MessageContent>,
    attachments: Vec<MessageAttachment>,
    blocks: Vec<MessageBlock>,
    reply_to_message_id: Option<String>,
}

impl ChatAddMessageRequest<'_> {
    /// Set message content.
    ///
    /// ```rust,no_run
    /// use anytype::prelude::*;
    /// # async fn example(client: &AnytypeClient) -> Result<(), AnytypeError> {
    /// let _id = client
    ///     .chats()
    ///     .add_message("chat_object_id")
    ///     .content(MessageContent {
    ///         text: "hello".to_string(),
    ///         style: MessageTextStyle::Paragraph,
    ///         marks: Vec::new(),
    ///     })
    ///     .send()
    ///     .await?;
    /// # Ok(())
    /// # }
    /// ```
    #[must_use]
    pub fn content(mut self, content: MessageContent) -> Self {
        self.content = Some(content);
        self
    }

    /// Attach objects to a message.
    ///
    /// ```rust,no_run
    /// use anytype::prelude::*;
    /// # async fn example(client: &AnytypeClient) -> Result<(), AnytypeError> {
    /// let _id = client
    ///     .chats()
    ///     .add_message("chat_object_id")
    ///     .content(MessageContent {
    ///         text: "see file".to_string(),
    ///         style: MessageTextStyle::Paragraph,
    ///         marks: Vec::new(),
    ///     })
    ///     .attachments(vec![MessageAttachment {
    ///         target: "file_object_id".to_string(),
    ///         kind: MessageAttachmentType::File,
    ///     }])
    ///     .send()
    ///     .await?;
    /// # Ok(())
    /// # }
    /// ```
    #[must_use]
    pub fn attachments(mut self, attachments: Vec<MessageAttachment>) -> Self {
        self.attachments = attachments;
        self
    }

    /// Set structured blocks. This capability is gRPC-only and is why rich
    /// message publishing is not routed through the REST API.
    #[must_use]
    pub fn blocks(mut self, blocks: Vec<MessageBlock>) -> Self {
        self.blocks = blocks;
        self
    }

    /// Reply to an existing message.
    #[must_use]
    pub fn reply_to(mut self, message_id: impl Into<String>) -> Self {
        self.reply_to_message_id = Some(message_id.into());
        self
    }

    /// Send the message and return the new message id.
    ///
    /// ```rust,no_run
    /// use anytype::prelude::*;
    /// # async fn example(client: &AnytypeClient) -> Result<(), AnytypeError> {
    /// let message_id = client
    ///     .chats()
    ///     .add_message("chat_object_id")
    ///     .content(MessageContent {
    ///         text: "hello".to_string(),
    ///         style: MessageTextStyle::Paragraph,
    ///         marks: Vec::new(),
    ///     })
    ///     .send()
    ///     .await?;
    /// println!("{message_id}");
    /// # Ok(())
    /// # }
    /// ```
    pub async fn send(self) -> Result<String> {
        let content = self.content.ok_or_else(|| AnytypeError::Validation {
            message: "chat message content is required".to_string(),
        })?;

        let grpc = self.client.grpc_client().await?;
        let mut commands = grpc.client_commands();
        let now_ms = Utc::now().timestamp_millis();
        let message = model::ChatMessage {
            id: String::new(),
            order_id: String::new(),
            creator: String::new(),
            created_at: now_ms,
            modified_at: now_ms,
            state_id: String::new(),
            reply_to_message_id: self.reply_to_message_id.unwrap_or_default(),
            message: Some(grpc_message_content(content)),
            attachments: grpc_attachments(self.attachments),
            reactions: None,
            read: false,
            mention_read: false,
            has_mention: false,
            synced: false,
            pinned: false,
            unread_reaction: false,
            blocks: self.blocks.into_iter().map(grpc_message_block).collect(),
        };
        let request = add_message::Request {
            chat_object_id: self.chat_object_id,
            message: Some(message),
        };

        let request = with_token_request(Request::new(request), grpc.token())?;
        let response = commands
            .chat_add_message(request)
            .await
            .map_err(grpc_status)?
            .into_inner();

        ensure_error_ok(response.error.as_ref(), "chat add message")?;
        Ok(response.message_id)
    }
}

pub struct ChatEditMessageRequest<'a> {
    client: &'a AnytypeClient,
    chat_object_id: String,
    message_id: String,
    content: Option<MessageContent>,
    attachments: Vec<MessageAttachment>,
    blocks: Vec<MessageBlock>,
}

impl ChatEditMessageRequest<'_> {
    /// Set the updated content for the message.
    ///
    /// ```rust,no_run
    /// use anytype::prelude::*;
    /// # async fn example(client: &AnytypeClient) -> Result<(), AnytypeError> {
    /// client
    ///     .chats()
    ///     .edit_message("chat_object_id", "message_id")
    ///     .content(MessageContent {
    ///         text: "updated".to_string(),
    ///         style: MessageTextStyle::Paragraph,
    ///         marks: Vec::new(),
    ///     })
    ///     .send()
    ///     .await?;
    /// # Ok(())
    /// # }
    /// ```
    #[must_use]
    pub fn content(mut self, content: MessageContent) -> Self {
        self.content = Some(content);
        self
    }

    /// Replace message attachments.
    #[must_use]
    pub fn attachments(mut self, attachments: Vec<MessageAttachment>) -> Self {
        self.attachments = attachments;
        self
    }

    /// Replace structured gRPC message blocks.
    #[must_use]
    pub fn blocks(mut self, blocks: Vec<MessageBlock>) -> Self {
        self.blocks = blocks;
        self
    }

    /// Send the edit request.
    ///
    /// ```rust,no_run
    /// use anytype::prelude::*;
    /// # async fn example(client: &AnytypeClient) -> Result<(), AnytypeError> {
    /// client
    ///     .chats()
    ///     .edit_message("chat_object_id", "message_id")
    ///     .content(MessageContent {
    ///         text: "updated".to_string(),
    ///         style: MessageTextStyle::Paragraph,
    ///         marks: Vec::new(),
    ///     })
    ///     .send()
    ///     .await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn send(self) -> Result<()> {
        let content = self.content.ok_or_else(|| AnytypeError::Validation {
            message: "chat message content is required".to_string(),
        })?;

        let grpc = self.client.grpc_client().await?;
        let mut commands = grpc.client_commands();
        let now_ms = Utc::now().timestamp_millis();
        let message = model::ChatMessage {
            id: self.message_id.clone(),
            order_id: String::new(),
            creator: String::new(),
            created_at: 0,
            modified_at: now_ms,
            state_id: String::new(),
            reply_to_message_id: String::new(),
            message: Some(grpc_message_content(content)),
            attachments: grpc_attachments(self.attachments),
            reactions: None,
            read: false,
            mention_read: false,
            has_mention: false,
            synced: false,
            pinned: false,
            unread_reaction: false,
            blocks: self.blocks.into_iter().map(grpc_message_block).collect(),
        };
        let request = edit_message_content::Request {
            chat_object_id: self.chat_object_id,
            message_id: self.message_id,
            edited_message: Some(message),
        };

        let request = with_token_request(Request::new(request), grpc.token())?;
        let response = commands
            .chat_edit_message_content(request)
            .await
            .map_err(grpc_status)?
            .into_inner();

        ensure_error_ok(response.error.as_ref(), "chat edit message")?;
        Ok(())
    }
}

pub struct ChatDeleteMessageRequest<'a> {
    client: &'a AnytypeClient,
    chat_object_id: String,
    message_id: String,
}

impl ChatDeleteMessageRequest<'_> {
    /// Delete the message.
    ///
    /// ```rust,no_run
    /// use anytype::prelude::*;
    /// # async fn example(client: &AnytypeClient) -> Result<(), AnytypeError> {
    /// client
    ///     .chats()
    ///     .delete_message("chat_object_id", "message_id")
    ///     .delete()
    ///     .await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn delete(self) -> Result<()> {
        let grpc = self.client.grpc_client().await?;
        let mut commands = grpc.client_commands();
        let request = delete_message::Request {
            chat_object_id: self.chat_object_id,
            message_id: self.message_id,
        };

        let request = with_token_request(Request::new(request), grpc.token())?;
        let response = commands
            .chat_delete_message(request)
            .await
            .map_err(grpc_status)?
            .into_inner();

        ensure_error_ok(response.error.as_ref(), "chat delete message")?;
        Ok(())
    }
}

pub struct ChatListMessagesRequest<'a> {
    client: &'a AnytypeClient,
    chat_object_id: String,
    after: Option<String>,
    before: Option<String>,
    include_boundary: Option<bool>,
    limit: Option<usize>,
    unread_only: Option<ChatReadType>,
}

impl ChatListMessagesRequest<'_> {
    /// Set `after` order id filter.
    ///
    /// ```rust,no_run
    /// use anytype::prelude::*;
    /// # async fn example(client: &AnytypeClient) -> Result<(), AnytypeError> {
    /// let _page = client
    ///     .chats()
    ///     .list_messages("chat_object_id")
    ///     .after("0000000000000005")
    ///     .list_page()
    ///     .await?;
    /// # Ok(())
    /// # }
    /// ```
    #[must_use]
    pub fn after(mut self, order_id: impl Into<String>) -> Self {
        self.after = Some(order_id.into());
        self
    }

    /// Set `before` order id filter.
    ///
    /// ```rust,no_run
    /// use anytype::prelude::*;
    /// # async fn example(client: &AnytypeClient) -> Result<(), AnytypeError> {
    /// let _page = client
    ///     .chats()
    ///     .list_messages("chat_object_id")
    ///     .before("0000000000000010")
    ///     .list_page()
    ///     .await?;
    /// # Ok(())
    /// # }
    /// ```
    #[must_use]
    pub fn before(mut self, order_id: impl Into<String>) -> Self {
        self.before = Some(order_id.into());
        self
    }

    /// Include the boundary order id when filtering.
    ///
    /// ```rust,no_run
    /// use anytype::prelude::*;
    /// # async fn example(client: &AnytypeClient) -> Result<(), AnytypeError> {
    /// let _page = client
    ///     .chats()
    ///     .list_messages("chat_object_id")
    ///     .include_boundary(true)
    ///     .list_page()
    ///     .await?;
    /// # Ok(())
    /// # }
    /// ```
    #[must_use]
    pub fn include_boundary(mut self, include: bool) -> Self {
        self.include_boundary = Some(include);
        self
    }

    /// Limit the number of messages returned.
    ///
    /// ```rust,no_run
    /// use anytype::prelude::*;
    /// # async fn example(client: &AnytypeClient) -> Result<(), AnytypeError> {
    /// let _page = client
    ///     .chats()
    ///     .list_messages("chat_object_id")
    ///     .limit(10)
    ///     .list_page()
    ///     .await?;
    /// # Ok(())
    /// # }
    /// ```
    #[must_use]
    pub fn limit(mut self, limit: usize) -> Self {
        self.limit = Some(limit);
        self
    }

    /// Filter results to unread messages only.
    ///
    /// ```rust,no_run
    /// use anytype::prelude::*;
    /// # async fn example(client: &AnytypeClient) -> Result<(), AnytypeError> {
    /// let page = client
    ///     .chats()
    ///     .list_messages("chat_object_id")
    ///     .unread_only(ChatReadType::Messages)
    ///     .list_page()
    ///     .await?;
    /// println!("unread: {}", page.messages.len());
    /// # Ok(())
    /// # }
    /// ```
    #[must_use]
    pub fn unread_only(mut self, read_type: ChatReadType) -> Self {
        self.unread_only = Some(read_type);
        self
    }

    /// Execute the list request and return a page wrapper.
    ///
    /// ```rust,no_run
    /// use anytype::prelude::*;
    /// # async fn example(client: &AnytypeClient) -> Result<(), AnytypeError> {
    /// let page = client
    ///     .chats()
    ///     .list_messages("chat_object_id")
    ///     .limit(25)
    ///     .list_page()
    ///     .await?;
    /// println!("messages: {}, unread: {}", page.messages.len(), page.state.messages_unread);
    /// # Ok(())
    /// # }
    /// ```
    pub async fn list_page(self) -> Result<ChatMessagesPage> {
        let grpc = self.client.grpc_client().await?;
        let mut commands = grpc.client_commands();

        let request = get_messages::Request {
            chat_object_id: self.chat_object_id,
            after_order_id: self.after.unwrap_or_default(),
            before_order_id: self.before.unwrap_or_default(),
            #[allow(clippy::cast_possible_wrap, clippy::cast_possible_truncation)]
            limit: self.limit.unwrap_or(0) as i32,
            include_boundary: self.include_boundary.unwrap_or(false),
        };

        let request = with_token_request(Request::new(request), grpc.token())?;
        let response = commands
            .chat_get_messages(request)
            .await
            .map_err(grpc_status)?
            .into_inner();

        ensure_error_ok(response.error.as_ref(), "chat list messages")?;
        let mut messages: Vec<ChatMessage> = response
            .messages
            .into_iter()
            .map(chat_message_from_grpc)
            .collect::<Result<Vec<_>>>()?;
        if let Some(read_type) = self.unread_only {
            messages = filter_unread_messages(messages, &read_type);
        }
        let state = response
            .chat_state
            .as_ref()
            .map_or_else(ChatState::default, chat_state_from_grpc);
        Ok(ChatMessagesPage { messages, state })
    }
}

pub struct ChatGetMessagesRequest<'a> {
    client: &'a AnytypeClient,
    chat_object_id: String,
    ids: Vec<String>,
}

impl ChatGetMessagesRequest<'_> {
    /// Fetch messages by id.
    ///
    /// ```rust,no_run
    /// use anytype::prelude::*;
    /// # async fn example(client: &AnytypeClient) -> Result<(), AnytypeError> {
    /// let messages = client
    ///     .chats()
    ///     .get_messages("chat_object_id", ["message_id"])
    ///     .get()
    ///     .await?;
    /// println!("messages: {}", messages.len());
    /// # Ok(())
    /// # }
    /// ```
    pub async fn get(self) -> Result<Vec<ChatMessage>> {
        let grpc = self.client.grpc_client().await?;
        let mut commands = grpc.client_commands();
        let request = get_messages_by_ids::Request {
            chat_object_id: self.chat_object_id,
            message_ids: self.ids,
        };

        let request = with_token_request(Request::new(request), grpc.token())?;
        let response = commands
            .chat_get_messages_by_ids(request)
            .await
            .map_err(grpc_status)?
            .into_inner();

        ensure_error_ok(response.error.as_ref(), "chat get messages")?;
        response
            .messages
            .into_iter()
            .map(chat_message_from_grpc)
            .collect()
    }
}

pub struct ChatReadMessagesRequest<'a> {
    client: &'a AnytypeClient,
    chat_object_id: String,
    read_type: Option<ChatReadType>,
    after: Option<String>,
    before: Option<String>,
    last_state_id: Option<String>,
}

impl ChatReadMessagesRequest<'_> {
    /// Select whether to mark messages or mentions as read.
    ///
    /// ```rust,no_run
    /// use anytype::prelude::*;
    /// # async fn example(client: &AnytypeClient) -> Result<(), AnytypeError> {
    /// client
    ///     .chats()
    ///     .read_messages("chat_object_id")
    ///     .read_type(ChatReadType::Mentions)
    ///     .mark_read()
    ///     .await?;
    /// # Ok(())
    /// # }
    /// ```
    #[must_use]
    pub fn read_type(mut self, read_type: ChatReadType) -> Self {
        self.read_type = Some(read_type);
        self
    }

    /// Set `after` order id filter.
    ///
    /// ```rust,no_run
    /// use anytype::prelude::*;
    /// # async fn example(client: &AnytypeClient) -> Result<(), AnytypeError> {
    /// client
    ///     .chats()
    ///     .read_messages("chat_object_id")
    ///     .after("0000000000000005")
    ///     .mark_read()
    ///     .await?;
    /// # Ok(())
    /// # }
    /// ```
    #[must_use]
    pub fn after(mut self, order_id: impl Into<String>) -> Self {
        self.after = Some(order_id.into());
        self
    }

    /// Set `before` order id filter.
    ///
    /// ```rust,no_run
    /// use anytype::prelude::*;
    /// # async fn example(client: &AnytypeClient) -> Result<(), AnytypeError> {
    /// client
    ///     .chats()
    ///     .read_messages("chat_object_id")
    ///     .before("0000000000000010")
    ///     .mark_read()
    ///     .await?;
    /// # Ok(())
    /// # }
    /// ```
    #[must_use]
    pub fn before(mut self, order_id: impl Into<String>) -> Self {
        self.before = Some(order_id.into());
        self
    }

    /// Set the last known chat state id (to avoid race conditions).
    ///
    /// ```rust,no_run
    /// use anytype::prelude::*;
    /// # async fn example(client: &AnytypeClient) -> Result<(), AnytypeError> {
    /// let page = client
    ///     .chats()
    ///     .list_messages("chat_object_id")
    ///     .limit(1)
    ///     .list_page()
    ///     .await?;
    /// client
    ///     .chats()
    ///     .read_messages("chat_object_id")
    ///     .last_state_id(page.state.last_state_id)
    ///     .mark_read()
    ///     .await?;
    /// # Ok(())
    /// # }
    /// ```
    #[must_use]
    pub fn last_state_id(mut self, state_id: impl Into<String>) -> Self {
        self.last_state_id = Some(state_id.into());
        self
    }

    /// Execute the mark-read request.
    ///
    /// ```rust,no_run
    /// use anytype::prelude::*;
    /// # async fn example(client: &AnytypeClient) -> Result<(), AnytypeError> {
    /// client
    ///     .chats()
    ///     .read_messages("chat_object_id")
    ///     .mark_read()
    ///     .await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn mark_read(self) -> Result<()> {
        let grpc = self.client.grpc_client().await?;
        let mut commands = grpc.client_commands();
        let read_type = self.read_type.unwrap_or(ChatReadType::Messages);
        let request = read_messages::Request {
            r#type: grpc_read_type(&read_type),
            chat_object_id: self.chat_object_id,
            after_order_id: self.after.unwrap_or_default(),
            before_order_id: self.before.unwrap_or_default(),
            last_state_id: self.last_state_id.unwrap_or_default(),
        };

        let request = with_token_request(Request::new(request), grpc.token())?;
        let response = commands
            .chat_read_messages(request)
            .await
            .map_err(grpc_status)?
            .into_inner();

        ensure_error_ok(response.error.as_ref(), "chat mark read")?;
        Ok(())
    }
}

pub struct ChatUnreadMessagesRequest<'a> {
    client: &'a AnytypeClient,
    chat_object_id: String,
    read_type: Option<ChatReadType>,
    after: Option<String>,
}

impl ChatUnreadMessagesRequest<'_> {
    /// Select whether to mark messages or mentions as unread.
    ///
    /// ```rust,no_run
    /// use anytype::prelude::*;
    /// # async fn example(client: &AnytypeClient) -> Result<(), AnytypeError> {
    /// client
    ///     .chats()
    ///     .unread_messages("chat_object_id")
    ///     .read_type(ChatReadType::Messages)
    ///     .mark_unread()
    ///     .await?;
    /// # Ok(())
    /// # }
    /// ```
    #[must_use]
    pub fn read_type(mut self, read_type: ChatReadType) -> Self {
        self.read_type = Some(read_type);
        self
    }

    /// Set `after` order id filter for unread marking.
    ///
    /// ```rust,no_run
    /// use anytype::prelude::*;
    /// # async fn example(client: &AnytypeClient) -> Result<(), AnytypeError> {
    /// client
    ///     .chats()
    ///     .unread_messages("chat_object_id")
    ///     .after("0000000000000005")
    ///     .mark_unread()
    ///     .await?;
    /// # Ok(())
    /// # }
    /// ```
    #[must_use]
    pub fn after(mut self, order_id: impl Into<String>) -> Self {
        self.after = Some(order_id.into());
        self
    }

    /// Execute the mark-unread request.
    ///
    /// ```rust,no_run
    /// use anytype::prelude::*;
    /// # async fn example(client: &AnytypeClient) -> Result<(), AnytypeError> {
    /// client
    ///     .chats()
    ///     .unread_messages("chat_object_id")
    ///     .mark_unread()
    ///     .await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn mark_unread(self) -> Result<()> {
        let grpc = self.client.grpc_client().await?;
        let mut commands = grpc.client_commands();
        let read_type = self.read_type.unwrap_or(ChatReadType::Messages);
        let request = unread::Request {
            r#type: grpc_unread_type(&read_type),
            chat_object_id: self.chat_object_id,
            after_order_id: self.after.unwrap_or_default(),
        };

        let request = with_token_request(Request::new(request), grpc.token())?;
        let response = commands
            .chat_unread_messages(request)
            .await
            .map_err(grpc_status)?
            .into_inner();

        ensure_error_ok(response.error.as_ref(), "chat mark unread")?;
        Ok(())
    }
}

// ============================================================================
// Chat discovery helpers
// ============================================================================

async fn chat_search(
    client: &AnytypeClient,
    space_id: Option<String>,
    text: Option<String>,
    filters: Vec<model::block::content::dataview::Filter>,
    limit: Option<u32>,
    offset: Option<u32>,
) -> Result<ChatListResult> {
    if let Some(space_id) = space_id {
        return chat_search_space(client, &space_id, text, filters, limit, offset).await;
    }

    let spaces = client.spaces().list().await?.collect_all().await?;
    let mut items = Vec::new();
    for space in spaces {
        let result = chat_search_space(
            client,
            &space.id,
            text.clone(),
            filters.clone(),
            limit,
            offset,
        )
        .await?;
        items.extend(result.items);
    }

    let offset_value = offset.unwrap_or(0);
    let mut items = if offset_value > 0 {
        items.into_iter().skip(offset_value as usize).collect()
    } else {
        items
    };

    if let Some(limit) = limit {
        items.truncate(limit as usize);
    }

    Ok(ChatListResult { items })
}

async fn chat_search_space(
    client: &AnytypeClient,
    space_id: &str,
    text: Option<String>,
    filters: Vec<model::block::content::dataview::Filter>,
    limit: Option<u32>,
    offset: Option<u32>,
) -> Result<ChatListResult> {
    let grpc = client.grpc_client().await?;
    let mut commands = grpc.client_commands();

    let mut grpc_filters = Vec::with_capacity(filters.len() + 1);
    grpc_filters.push(chat_layout_filter());
    grpc_filters.extend(filters);

    let request = search_with_meta::Request {
        space_id: space_id.to_string(),
        filters: grpc_filters,
        sorts: Vec::new(),
        full_text: text.unwrap_or_default(),
        #[allow(clippy::cast_possible_wrap, clippy::cast_possible_truncation)]
        offset: offset.unwrap_or_default() as i32,
        #[allow(clippy::cast_possible_wrap, clippy::cast_possible_truncation)]
        limit: limit.unwrap_or(100) as i32,
        object_type_filter: Vec::new(),
        keys: chat_details_keys(),
        return_meta: false,
        return_meta_relation_details: false,
        return_html_highlights_instead_of_ranges: false,
    };

    let request = with_token_request(Request::new(request), grpc.token())?;
    let response = commands
        .object_search_with_meta(request)
        .await
        .map_err(grpc_status)?
        .into_inner();
    ensure_error_ok(response.error.as_ref(), "chat search")?;

    let mut items = Vec::with_capacity(response.results.len());
    for result in response.results {
        let details = result.details.ok_or_else(|| AnytypeError::Other {
            message: "chat search result missing details".to_string(),
        })?;
        items.push(object_from_details(
            Some(space_id),
            result.object_id,
            &details,
        ));
    }

    Ok(ChatListResult { items })
}

fn chat_details_keys() -> Vec<String> {
    vec![
        "id".to_string(),
        "name".to_string(),
        "lastModifiedDate".to_string(),
        "resolvedLayout".to_string(),
        "type".to_string(),
        "isArchived".to_string(),
        "spaceId".to_string(),
    ]
}

fn chat_layout_filter() -> model::block::content::dataview::Filter {
    model::block::content::dataview::Filter {
        id: String::new(),
        operator: model::block::content::dataview::filter::Operator::No as i32,
        relation_key: "resolvedLayout".to_string(),
        relation_property: String::new(),
        condition: model::block::content::dataview::filter::Condition::Equal as i32,
        value: Some(value_number(f64::from(
            model::object_type::Layout::ChatDerived as i32,
        ))),
        quick_option: model::block::content::dataview::filter::QuickOption::ExactDate as i32,
        format: 0,
        include_time: false,
        nested_filters: Vec::new(),
    }
}

fn filter_id_equal(id: &str) -> model::block::content::dataview::Filter {
    model::block::content::dataview::Filter {
        id: String::new(),
        operator: model::block::content::dataview::filter::Operator::No as i32,
        relation_key: "id".to_string(),
        relation_property: String::new(),
        condition: model::block::content::dataview::filter::Condition::Equal as i32,
        value: Some(value_string(id.to_string())),
        quick_option: model::block::content::dataview::filter::QuickOption::ExactDate as i32,
        format: 0,
        include_time: false,
        nested_filters: Vec::new(),
    }
}

fn filter_name_equal(name: &str) -> model::block::content::dataview::Filter {
    model::block::content::dataview::Filter {
        id: String::new(),
        operator: model::block::content::dataview::filter::Operator::No as i32,
        relation_key: "name".to_string(),
        relation_property: String::new(),
        condition: model::block::content::dataview::filter::Condition::Equal as i32,
        value: Some(value_string(name.to_string())),
        quick_option: model::block::content::dataview::filter::QuickOption::ExactDate as i32,
        format: 0,
        include_time: false,
        nested_filters: Vec::new(),
    }
}

fn object_from_details(
    default_space_id: Option<&str>,
    object_id: String,
    details: &Struct,
) -> Object {
    let name = string_field(details, "name");
    let archived = bool_field(details, "isArchived").unwrap_or(false);
    let space_id = string_field(details, "spaceId")
        .or_else(|| default_space_id.map(ToString::to_string))
        .unwrap_or_default();
    #[allow(clippy::cast_possible_wrap, clippy::cast_possible_truncation)]
    let layout = resolved_layout_to_object_layout(
        number_field(details, "resolvedLayout").map(|fval| fval as i32),
    );

    let mut properties = Vec::new();
    if let Some(date) = last_modified_date(details) {
        properties.push(property_date("last_modified_date", date));
    }

    Object {
        archived,
        icon: None,
        id: object_id,
        layout,
        markdown: None,
        name,
        object: DataModel::Object,
        properties,
        snippet: None,
        space_id,
        r#type: None,
    }
}

fn resolved_layout_to_object_layout(value: Option<i32>) -> ObjectLayout {
    let Some(value) = value else {
        return ObjectLayout::Basic;
    };
    match value {
        value if value == model::object_type::Layout::Basic as i32 => ObjectLayout::Basic,
        value if value == model::object_type::Layout::Profile as i32 => ObjectLayout::Profile,
        value if value == model::object_type::Layout::Todo as i32 => ObjectLayout::Action,
        value if value == model::object_type::Layout::Set as i32 => ObjectLayout::Set,
        value if value == model::object_type::Layout::Note as i32 => ObjectLayout::Note,
        value if value == model::object_type::Layout::Bookmark as i32 => ObjectLayout::Bookmark,
        value if value == model::object_type::Layout::Collection as i32 => ObjectLayout::Collection,
        value if value == model::object_type::Layout::Participant as i32 => {
            ObjectLayout::Participant
        }
        _ => ObjectLayout::Basic,
    }
}

fn last_modified_date(details: &Struct) -> Option<String> {
    if let Some(value) = string_field(details, "lastModifiedDate") {
        return Some(value);
    }
    if let Some(value) = number_field(details, "lastModifiedDate") {
        // f64 has 53 bit mantissa and we only need 31 bits for timestamp in seconds,
        // so this isn't lossy
        #[allow(clippy::cast_possible_wrap, clippy::cast_possible_truncation)]
        return Utc
            .timestamp_opt(value as i64, 0)
            .single()
            .map(|timestamp| timestamp.to_rfc3339());
    }
    None
}

fn property_date(key: &str, date: String) -> PropertyWithValue {
    PropertyWithValue {
        name: key.to_string(),
        key: key.to_string(),
        id: key.to_string(),
        value: PropertyValue::Date { date },
    }
}

fn string_field(details: &Struct, key: &str) -> Option<String> {
    details.fields.get(key).and_then(|value| match &value.kind {
        Some(prost_types::value::Kind::StringValue(value)) => Some(value.clone()),
        _ => None,
    })
}

fn number_field(details: &Struct, key: &str) -> Option<f64> {
    details.fields.get(key).and_then(|value| match &value.kind {
        Some(prost_types::value::Kind::NumberValue(value)) => Some(*value),
        _ => None,
    })
}

fn bool_field(details: &Struct, key: &str) -> Option<bool> {
    details.fields.get(key).and_then(|value| match &value.kind {
        Some(prost_types::value::Kind::BoolValue(value)) => Some(*value),
        _ => None,
    })
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

// ============================================================================
// gRPC conversion helpers
// ============================================================================

pub(crate) fn chat_message_from_grpc(message: model::ChatMessage) -> Result<ChatMessage> {
    let content = message
        .message
        .map(message_content_from_grpc)
        .unwrap_or_default();
    let attachments = message
        .attachments
        .into_iter()
        .map(message_attachment_from_grpc)
        .collect();
    let reactions = message
        .reactions
        .as_ref()
        .map(message_reactions_from_grpc)
        .unwrap_or_default();
    let blocks = message
        .blocks
        .into_iter()
        .filter_map(message_block_from_grpc)
        .collect();
    Ok(ChatMessage {
        id: message.id,
        order_id: message.order_id,
        state_id: message.state_id,
        creator: message.creator,
        creator_name: None,
        created_at: timestamp_to_datetime(message.created_at, ChatTimestampField::CreatedAt)?,
        modified_at: timestamp_to_datetime(message.modified_at, ChatTimestampField::ModifiedAt)?,
        reply_to_message_id: empty_to_none(message.reply_to_message_id),
        content,
        attachments,
        reactions,
        read: message.read,
        mention_read: message.mention_read,
        has_mention: message.has_mention,
        synced: message.synced,
        pinned: message.pinned,
        unread_reaction: message.unread_reaction,
        blocks,
    })
}

fn message_content_from_grpc(content: model::chat_message::MessageContent) -> MessageContent {
    MessageContent {
        text: content.text,
        style: message_text_style_from_grpc(content.style),
        marks: content
            .marks
            .into_iter()
            .map(message_mark_from_grpc)
            .collect(),
    }
}

fn message_attachment_from_grpc(attachment: model::chat_message::Attachment) -> MessageAttachment {
    MessageAttachment {
        target: attachment.target,
        kind: message_attachment_type_from_grpc(attachment.r#type),
    }
}

pub(crate) fn message_reactions_from_grpc(
    reactions: &model::chat_message::Reactions,
) -> Vec<MessageReaction> {
    let mut items = Vec::new();
    for (emoji, ids) in &reactions.reactions {
        items.push(MessageReaction {
            emoji: emoji.clone(),
            identities: ids.ids.clone(),
        });
    }
    items
}

fn message_mark_from_grpc(mark: model::block::content::text::Mark) -> MessageTextMark {
    let range = mark.range.map(|range| MessageTextRange {
        from: range.from,
        to: range.to,
    });
    MessageTextMark {
        range,
        kind: message_mark_type_from_grpc(mark.r#type),
        param: empty_to_none(mark.param),
    }
}

pub(crate) fn chat_state_from_grpc(state: &model::ChatState) -> ChatState {
    ChatState {
        messages_unread: state
            .messages
            .as_ref()
            .map(|unread| unread.counter)
            .unwrap_or_default(),
        mentions_unread: state
            .mentions
            .as_ref()
            .map(|unread| unread.counter)
            .unwrap_or_default(),
        last_state_id: state.last_state_id.clone(),
        order: state.order,
        messages_oldest_order_id: state
            .messages
            .as_ref()
            .and_then(|unread| empty_to_none(unread.oldest_order_id.clone())),
        mentions_oldest_order_id: state
            .mentions
            .as_ref()
            .and_then(|unread| empty_to_none(unread.oldest_order_id.clone())),
    }
}

fn grpc_message_content(content: MessageContent) -> model::chat_message::MessageContent {
    model::chat_message::MessageContent {
        text: content.text,
        style: grpc_message_text_style(&content.style),
        marks: content.marks.into_iter().map(grpc_message_mark).collect(),
    }
}

fn grpc_message_mark(mark: MessageTextMark) -> model::block::content::text::Mark {
    model::block::content::text::Mark {
        range: mark.range.map(|range| model::Range {
            from: range.from,
            to: range.to,
        }),
        r#type: grpc_message_mark_type(&mark.kind),
        param: mark.param.unwrap_or_default(),
    }
}

fn grpc_attachments(attachments: Vec<MessageAttachment>) -> Vec<model::chat_message::Attachment> {
    attachments
        .into_iter()
        .map(|attachment| model::chat_message::Attachment {
            target: attachment.target,
            r#type: grpc_message_attachment_type(&attachment.kind),
        })
        .collect()
}

fn grpc_message_block(block: MessageBlock) -> model::chat_message::MessageBlock {
    use model::chat_message::message_block::ContentValue;
    let content_value = match block {
        MessageBlock::Text(text) => ContentValue::Text(grpc_message_block_text(text)),
        MessageBlock::Link(link) => ContentValue::Link(model::chat_message::MessageBlockLink {
            target_object_id: link.target_object_id,
            r#type: grpc_message_block_link_type(&link.kind),
        }),
        MessageBlock::Embed(embed) => ContentValue::Embed(model::chat_message::MessageBlockEmbed {
            text: embed.text,
            processor: grpc_message_block_processor(&embed.processor),
        }),
        MessageBlock::EditorQuote(quote) => {
            ContentValue::EditorQuote(model::chat_message::MessageBlockEditorQuote {
                block_id: quote.block_id,
                content: quote.content.map(grpc_message_block_text),
            })
        }
        MessageBlock::MessageQuote(quote) => {
            ContentValue::MessageQuote(model::chat_message::MessageBlockMessageQuote {
                message_id: quote.message_id,
                participant_id: quote.participant_id,
                content: quote.content.map(grpc_message_block_text),
            })
        }
    };
    model::chat_message::MessageBlock {
        content_value: Some(content_value),
    }
}

fn grpc_message_block_text(text: MessageBlockText) -> model::chat_message::MessageBlockText {
    model::chat_message::MessageBlockText {
        text: text.text,
        style: grpc_message_text_style(&text.style),
        marks: text.marks.into_iter().map(grpc_message_mark).collect(),
        checked: text.checked,
        lang: text.language.unwrap_or_default(),
    }
}

fn message_block_from_grpc(block: model::chat_message::MessageBlock) -> Option<MessageBlock> {
    use model::chat_message::message_block::ContentValue;
    match block.content_value? {
        ContentValue::Text(text) => Some(MessageBlock::Text(message_block_text_from_grpc(text))),
        ContentValue::Link(link) => Some(MessageBlock::Link(MessageBlockLink {
            target_object_id: link.target_object_id,
            kind: message_block_link_type_from_grpc(link.r#type),
        })),
        ContentValue::Embed(embed) => Some(MessageBlock::Embed(MessageBlockEmbed {
            text: embed.text,
            processor: message_block_processor_from_grpc(embed.processor),
        })),
        ContentValue::EditorQuote(quote) => {
            Some(MessageBlock::EditorQuote(MessageBlockEditorQuote {
                block_id: quote.block_id,
                content: quote.content.map(message_block_text_from_grpc),
            }))
        }
        ContentValue::MessageQuote(quote) => {
            Some(MessageBlock::MessageQuote(MessageBlockMessageQuote {
                message_id: quote.message_id,
                participant_id: quote.participant_id,
                content: quote.content.map(message_block_text_from_grpc),
            }))
        }
    }
}

fn message_block_text_from_grpc(text: model::chat_message::MessageBlockText) -> MessageBlockText {
    MessageBlockText {
        text: text.text,
        style: message_text_style_from_grpc(text.style),
        marks: text.marks.into_iter().map(message_mark_from_grpc).collect(),
        checked: text.checked,
        language: empty_to_none(text.lang),
    }
}

fn grpc_message_block_link_type(kind: &MessageBlockLinkType) -> i32 {
    use model::chat_message::message_block_link::LinkType;
    match kind {
        MessageBlockLinkType::Object => LinkType::Object as i32,
        MessageBlockLinkType::File => LinkType::File as i32,
        MessageBlockLinkType::Image => LinkType::Image as i32,
        MessageBlockLinkType::Bookmark => LinkType::Bookmark as i32,
        MessageBlockLinkType::Other(value) => *value,
    }
}

fn message_block_link_type_from_grpc(value: i32) -> MessageBlockLinkType {
    use model::chat_message::message_block_link::LinkType;
    match LinkType::try_from(value).ok() {
        Some(LinkType::Object) => MessageBlockLinkType::Object,
        Some(LinkType::File) => MessageBlockLinkType::File,
        Some(LinkType::Image) => MessageBlockLinkType::Image,
        Some(LinkType::Bookmark) => MessageBlockLinkType::Bookmark,
        None => MessageBlockLinkType::Other(value),
    }
}

fn grpc_message_block_processor(processor: &MessageBlockProcessor) -> i32 {
    use model::block::content::latex::Processor;
    match processor {
        MessageBlockProcessor::Latex => Processor::Latex as i32,
        MessageBlockProcessor::Mermaid => Processor::Mermaid as i32,
        MessageBlockProcessor::Graphviz => Processor::Graphviz as i32,
        MessageBlockProcessor::Other(value) => *value,
    }
}

fn message_block_processor_from_grpc(value: i32) -> MessageBlockProcessor {
    use model::block::content::latex::Processor;
    match Processor::try_from(value).ok() {
        Some(Processor::Latex) => MessageBlockProcessor::Latex,
        Some(Processor::Mermaid) => MessageBlockProcessor::Mermaid,
        Some(Processor::Graphviz) => MessageBlockProcessor::Graphviz,
        Some(_) | None => MessageBlockProcessor::Other(value),
    }
}

fn message_text_style_from_grpc(value: i32) -> MessageTextStyle {
    use model::block::content::text::Style;
    match Style::try_from(value).ok() {
        Some(Style::Paragraph) => MessageTextStyle::Paragraph,
        Some(Style::Header1) => MessageTextStyle::Header1,
        Some(Style::Header2) => MessageTextStyle::Header2,
        Some(Style::Header3) => MessageTextStyle::Header3,
        Some(Style::Header4) => MessageTextStyle::Header4,
        Some(Style::Quote) => MessageTextStyle::Quote,
        Some(Style::Code) => MessageTextStyle::Code,
        Some(Style::Title) => MessageTextStyle::Title,
        Some(Style::Checkbox) => MessageTextStyle::Checkbox,
        Some(Style::Marked) => MessageTextStyle::Marked,
        Some(Style::Numbered) => MessageTextStyle::Numbered,
        Some(Style::Toggle) => MessageTextStyle::Toggle,
        Some(Style::ToggleHeader1) => MessageTextStyle::ToggleHeader1,
        Some(Style::ToggleHeader2) => MessageTextStyle::ToggleHeader2,
        Some(Style::ToggleHeader3) => MessageTextStyle::ToggleHeader3,
        Some(Style::Description) => MessageTextStyle::Description,
        Some(Style::Callout) => MessageTextStyle::Callout,
        None => MessageTextStyle::Other(value.to_string()),
    }
}

fn grpc_message_text_style(style: &MessageTextStyle) -> i32 {
    use model::block::content::text::Style;
    match style {
        MessageTextStyle::Paragraph | MessageTextStyle::Other(_) => Style::Paragraph as i32,
        MessageTextStyle::Header1 => Style::Header1 as i32,
        MessageTextStyle::Header2 => Style::Header2 as i32,
        MessageTextStyle::Header3 => Style::Header3 as i32,
        MessageTextStyle::Header4 => Style::Header4 as i32,
        MessageTextStyle::Quote => Style::Quote as i32,
        MessageTextStyle::Code => Style::Code as i32,
        MessageTextStyle::Title => Style::Title as i32,
        MessageTextStyle::Checkbox => Style::Checkbox as i32,
        MessageTextStyle::Marked => Style::Marked as i32,
        MessageTextStyle::Numbered => Style::Numbered as i32,
        MessageTextStyle::Toggle => Style::Toggle as i32,
        MessageTextStyle::ToggleHeader1 => Style::ToggleHeader1 as i32,
        MessageTextStyle::ToggleHeader2 => Style::ToggleHeader2 as i32,
        MessageTextStyle::ToggleHeader3 => Style::ToggleHeader3 as i32,
        MessageTextStyle::Description => Style::Description as i32,
        MessageTextStyle::Callout => Style::Callout as i32,
    }
}

fn message_mark_type_from_grpc(value: i32) -> MessageTextMarkType {
    use model::block::content::text::mark::Type;
    match Type::try_from(value).ok() {
        Some(Type::Strikethrough) => MessageTextMarkType::Strikethrough,
        Some(Type::Keyboard) => MessageTextMarkType::Keyboard,
        Some(Type::Italic) => MessageTextMarkType::Italic,
        Some(Type::Bold) => MessageTextMarkType::Bold,
        Some(Type::Underscored) => MessageTextMarkType::Underscored,
        Some(Type::Link) => MessageTextMarkType::Link,
        Some(Type::TextColor) => MessageTextMarkType::TextColor,
        Some(Type::BackgroundColor) => MessageTextMarkType::BackgroundColor,
        Some(Type::Mention) => MessageTextMarkType::Mention,
        Some(Type::Emoji) => MessageTextMarkType::Emoji,
        Some(Type::Object) => MessageTextMarkType::Object,
        None => MessageTextMarkType::Other(value.to_string()),
    }
}

fn grpc_message_mark_type(kind: &MessageTextMarkType) -> i32 {
    use model::block::content::text::mark::Type;
    match *kind {
        MessageTextMarkType::Strikethrough => Type::Strikethrough as i32,
        MessageTextMarkType::Keyboard => Type::Keyboard as i32,
        MessageTextMarkType::Italic => Type::Italic as i32,
        MessageTextMarkType::Bold | MessageTextMarkType::Other(_) => Type::Bold as i32,
        MessageTextMarkType::Underscored => Type::Underscored as i32,
        MessageTextMarkType::Link => Type::Link as i32,
        MessageTextMarkType::TextColor => Type::TextColor as i32,
        MessageTextMarkType::BackgroundColor => Type::BackgroundColor as i32,
        MessageTextMarkType::Mention => Type::Mention as i32,
        MessageTextMarkType::Emoji => Type::Emoji as i32,
        MessageTextMarkType::Object => Type::Object as i32,
    }
}

fn message_attachment_type_from_grpc(value: i32) -> MessageAttachmentType {
    use model::chat_message::attachment::AttachmentType;
    match AttachmentType::try_from(value).ok() {
        Some(AttachmentType::File) => MessageAttachmentType::File,
        Some(AttachmentType::Image) => MessageAttachmentType::Image,
        Some(AttachmentType::Link) => MessageAttachmentType::Link,
        None => MessageAttachmentType::Other(value.to_string()),
    }
}

fn grpc_message_attachment_type(kind: &MessageAttachmentType) -> i32 {
    use model::chat_message::attachment::AttachmentType;
    match *kind {
        MessageAttachmentType::File | MessageAttachmentType::Other(_) => {
            AttachmentType::File as i32
        }
        MessageAttachmentType::Image => AttachmentType::Image as i32,
        MessageAttachmentType::Link => AttachmentType::Link as i32,
    }
}

fn grpc_read_type(read_type: &ChatReadType) -> i32 {
    match read_type {
        ChatReadType::Messages | ChatReadType::Other(_) => read_messages::ReadType::Messages as i32,
        ChatReadType::Mentions => read_messages::ReadType::Mentions as i32,
    }
}

fn grpc_unread_type(read_type: &ChatReadType) -> i32 {
    match read_type {
        ChatReadType::Messages | ChatReadType::Other(_) => unread::ReadType::Messages as i32,
        ChatReadType::Mentions => unread::ReadType::Mentions as i32,
    }
}

fn filter_unread_messages(
    messages: Vec<ChatMessage>,
    read_type: &ChatReadType,
) -> Vec<ChatMessage> {
    match read_type {
        ChatReadType::Messages | ChatReadType::Other(_) => {
            messages.into_iter().filter(|msg| !msg.read).collect()
        }
        ChatReadType::Mentions => messages
            .into_iter()
            .filter(|msg| msg.has_mention && !msg.mention_read)
            .collect(),
    }
}

fn timestamp_to_datetime(value: i64, field: ChatTimestampField) -> Result<DateTime<FixedOffset>> {
    let timestamp = if value.unsigned_abs() > 10_000_000_000 {
        Utc.timestamp_millis_opt(value).single()
    } else {
        Utc.timestamp_opt(value, 0).single()
    }
    .filter(|timestamp| (1..=9999).contains(&timestamp.year()))
    .ok_or(AnytypeError::ChatTimestamp { field })?;
    Ok(timestamp.fixed_offset())
}

/// Format a validated chat timestamp as canonical UTC milliseconds.
///
/// # Errors
///
/// Returns [`AnytypeError::ChatTimestamp`] when the supplied value cannot be
/// represented in the required year 0001 through 9999 range.
pub fn canonical_chat_timestamp(
    timestamp: DateTime<FixedOffset>,
    field: ChatTimestampField,
) -> Result<String> {
    let timestamp = timestamp.with_timezone(&Utc);
    if !(1..=9999).contains(&timestamp.year()) {
        return Err(AnytypeError::ChatTimestamp { field });
    }
    Ok(timestamp.to_rfc3339_opts(SecondsFormat::Millis, true))
}

fn empty_to_none(value: String) -> Option<String> {
    if value.is_empty() { None } else { Some(value) }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::{
        ChatHistoryEvidenceKind, ChatHttpEvent, ChatHttpSseState, ChatMessage, ChatTimestampField,
        HttpChatMessage, MAX_CHAT_HISTORY_PAGE_SIZE, MAX_MESSAGE_BEFORE_ANCHOR_BYTES,
        MessageAttachment, MessageAttachmentType, MessageBeforeAnchor, MessageBlock,
        MessageBlockLink, MessageBlockLinkType, MessageBlockText, MessageContent,
        MessageTextMarkType, MessageTextStyle, ReadMessagesBody, append_sse_byte,
        canonical_chat_timestamp, chat_message_from_grpc, chat_message_path,
        chat_stream_diagnostic_path, decode_history_messages, grpc_message_block,
        message_block_from_grpc, timestamp_to_datetime,
    };
    use anytype_rpc::{error::AnytypeGrpcError, model};
    use futures::StreamExt;
    use reqwest::StatusCode;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
        task::JoinHandle,
    };

    use crate::{
        Result,
        client::{AnytypeClient, ClientConfig},
        error::AnytypeError,
        filters::{Condition, Filter},
        keystore::{GrpcCredentials, HttpCredentials},
        test_util::scripted_http::{
            ScriptedHttpContentType, ScriptedHttpFixture, ScriptedHttpRequest, ScriptedHttpResponse,
        },
    };

    static NEXT_SCRIPT_ID: AtomicU64 = AtomicU64::new(1);

    fn scripted_client(
        address: std::net::SocketAddr,
        app_name: &str,
        configure: impl FnOnce(&mut ClientConfig),
    ) -> AnytypeClient {
        let id = NEXT_SCRIPT_ID.fetch_add(1, Ordering::Relaxed);
        let key_path =
            std::env::temp_dir().join(format!("anytype-{app_name}-{}-{id}.db", std::process::id()));
        let mut config = ClientConfig::default().app_name(app_name);
        config.base_url = Some(format!("http://{address}"));
        config.keystore = Some(format!("file:path={}", key_path.display()));
        config.keystore_service = Some(format!("{app_name}-{id}"));
        configure(&mut config);
        let client = AnytypeClient::with_config(config).expect("create scripted client");
        client.set_api_key(HttpCredentials::new("test-token"));
        client
    }

    async fn scripted_http_client(
        status: &str,
        content_type: &str,
        body: &str,
    ) -> (AnytypeClient, JoinHandle<String>) {
        scripted_http_client_with_config(status, content_type, body, |_| {}).await
    }

    async fn scripted_http_client_with_config(
        status: &str,
        content_type: &str,
        body: &str,
        configure: impl FnOnce(&mut ClientConfig),
    ) -> (AnytypeClient, JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind scripted chat HTTP fixture");
        let address = listener
            .local_addr()
            .expect("scripted chat HTTP fixture address");
        let response = format!(
            "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept scripted request");
            let mut request = Vec::new();
            let mut chunk = [0_u8; 1024];
            let mut expected_len = None;
            loop {
                let read = stream
                    .read(&mut chunk)
                    .await
                    .expect("read scripted request");
                if read == 0 {
                    break;
                }
                request.extend_from_slice(&chunk[..read]);
                if expected_len.is_none()
                    && let Some(header_end) = request.windows(4).position(|w| w == b"\r\n\r\n")
                {
                    let headers = String::from_utf8_lossy(&request[..header_end]);
                    let body_len = headers
                        .lines()
                        .find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            name.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse::<usize>().ok())
                                .flatten()
                        })
                        .unwrap_or_default();
                    expected_len = Some(header_end + 4 + body_len);
                }
                if expected_len.is_some_and(|len| request.len() >= len) {
                    break;
                }
            }
            stream
                .write_all(response.as_bytes())
                .await
                .expect("write scripted response");
            String::from_utf8(request).expect("HTTP request is UTF-8")
        });

        let client = scripted_client(address, "chat-http-unit", configure);
        (client, server)
    }

    async fn scripted_http_sequence(
        responses: Vec<ScriptedHttpResponse>,
    ) -> (AnytypeClient, ScriptedHttpFixture) {
        let fixture = ScriptedHttpFixture::start(responses)
            .await
            .expect("start bounded scripted chat HTTP sequence");
        let client = scripted_client(fixture.address(), "chat-http-sequence", |_| {});
        (client, fixture)
    }

    fn scripted_message(
        id: &str,
        order_id: &str,
        text: &str,
        created_at: i64,
        modified_at: i64,
    ) -> serde_json::Value {
        serde_json::json!({
            "id": id,
            "order_id": order_id,
            "creator": "participant-id",
            "created_at": created_at,
            "modified_at": modified_at,
            "content": {"text": text, "style": "paragraph"}
        })
    }

    fn scripted_message_response(message: serde_json::Value) -> String {
        serde_json::json!({"message": message}).to_string()
    }

    fn scripted_messages_response(messages: Vec<serde_json::Value>) -> String {
        serde_json::json!({"messages": messages}).to_string()
    }

    fn scripted_json_response(status: StatusCode, body: String) -> ScriptedHttpResponse {
        ScriptedHttpResponse::new(status, ScriptedHttpContentType::Json, body.into_bytes())
    }

    fn scripted_text_response(status: StatusCode, body: &str) -> ScriptedHttpResponse {
        ScriptedHttpResponse::new(
            status,
            ScriptedHttpContentType::Text,
            body.as_bytes().to_vec(),
        )
    }

    fn request_json(request: &str) -> serde_json::Value {
        let (_, body) = request
            .split_once("\r\n\r\n")
            .expect("request contains headers");
        serde_json::from_str(body).expect("request body is JSON")
    }

    fn captured_request_json(request: &ScriptedHttpRequest) -> serde_json::Value {
        serde_json::from_slice(request.body()).expect("captured request body is JSON")
    }

    #[test]
    fn chat_timestamps_are_fallible_and_canonical_utc_milliseconds() {
        let epoch = timestamp_to_datetime(0, ChatTimestampField::CreatedAt)
            .expect("Unix epoch is representable");
        assert_eq!(
            canonical_chat_timestamp(epoch, ChatTimestampField::CreatedAt)
                .expect("format canonical timestamp"),
            "1970-01-01T00:00:00.000Z"
        );
        let one_millisecond = timestamp_to_datetime(10_000_000_001, ChatTimestampField::ModifiedAt)
            .expect("millisecond timestamp is representable");
        assert_eq!(one_millisecond.timestamp_millis(), 10_000_000_001);

        for value in [i64::MIN, i64::MAX] {
            let error = timestamp_to_datetime(value, ChatTimestampField::ModifiedAt)
                .expect_err("out-of-range timestamp must fail");
            assert!(matches!(
                error,
                AnytypeError::ChatTimestamp {
                    field: ChatTimestampField::ModifiedAt
                }
            ));
        }
    }

    #[test]
    fn message_before_anchor_is_bounded_opaque_and_redacted() {
        let anchor = MessageBeforeAnchor::try_from("opaque-anchor".to_string())
            .expect("valid opaque anchor");
        assert_eq!(format!("{anchor:?}"), "MessageBeforeAnchor([redacted])");
        let encoded = serde_json::to_string(&anchor).expect("serialize bounded anchor");
        assert_eq!(encoded, "\"opaque-anchor\"");
        let decoded: MessageBeforeAnchor =
            serde_json::from_str(&encoded).expect("deserialize bounded anchor");
        assert_eq!(decoded, anchor);
        assert!(serde_json::from_str::<MessageBeforeAnchor>("\"unsafe anchor\"").is_err());
        assert!(matches!(
            MessageBeforeAnchor::try_from(String::new()),
            Err(AnytypeError::ChatHistoryEvidence {
                kind: ChatHistoryEvidenceKind::InvalidAnchor
            })
        ));
        assert!(MessageBeforeAnchor::try_from("x".repeat(MAX_MESSAGE_BEFORE_ANCHOR_BYTES)).is_ok());
        assert!(matches!(
            MessageBeforeAnchor::try_from("x".repeat(MAX_MESSAGE_BEFORE_ANCHOR_BYTES + 1)),
            Err(AnytypeError::ChatHistoryEvidence {
                kind: ChatHistoryEvidenceKind::InvalidAnchor
            })
        ));
        assert!(matches!(
            MessageBeforeAnchor::try_from("unsafe anchor".to_string()),
            Err(AnytypeError::ChatHistoryEvidence {
                kind: ChatHistoryEvidenceKind::InvalidAnchor
            })
        ));
    }

    #[tokio::test]
    async fn malformed_rest_timestamp_returns_typed_error_without_epoch_fallback() {
        let body = scripted_message_response(scripted_message(
            "m1",
            "o1",
            "text",
            i64::MAX,
            1_717_405_200,
        ));
        let (client, server) = scripted_http_client("200 OK", "application/json", &body).await;
        let error = client
            .chats()
            .in_space("space-id")
            .get_message("chat-id", "m1")
            .get()
            .await
            .expect_err("invalid created_at must fail the complete read");
        assert!(matches!(
            error,
            AnytypeError::ChatTimestamp {
                field: ChatTimestampField::CreatedAt
            }
        ));
        let request = server.await.expect("scripted malformed timestamp request");
        assert!(request.starts_with("GET /v1/spaces/space-id/chats/chat-id/messages/m1 HTTP/1.1"));
    }

    #[tokio::test]
    async fn older_history_preserves_order_and_uses_only_opaque_before_successors() {
        let timestamp = 1_717_405_200_i64;
        let responses = vec![
            scripted_json_response(
                StatusCode::OK,
                scripted_messages_response(vec![
                    scripted_message("m2", "o2", "middle", timestamp, timestamp),
                    scripted_message("m3", "o3", "newest", timestamp, timestamp),
                ]),
            ),
            scripted_json_response(
                StatusCode::CREATED,
                r#"{"message_id":"newer-message"}"#.to_string(),
            ),
            scripted_json_response(
                StatusCode::OK,
                scripted_messages_response(vec![scripted_message(
                    "m1", "o1", "oldest", timestamp, timestamp,
                )]),
            ),
        ];
        let (client, server) = scripted_http_sequence(responses).await;
        let chats = client.chats().in_space("space-id");
        let first = chats
            .older_messages("chat-id")
            .limit(2)
            .get()
            .await
            .expect("initial older-history page");
        assert_eq!(
            first
                .messages
                .iter()
                .map(|message| message.id.as_str())
                .collect::<Vec<_>>(),
            ["m2", "m3"]
        );
        let anchor = first.next_before.expect("full page continuation");
        let newer_id = chats
            .add_message("chat-id", MessageContent::new().text("newer"))
            .send()
            .await
            .expect("scripted newer insertion");
        assert_eq!(newer_id, "newer-message");
        let second = chats
            .older_messages("chat-id")
            .before(anchor)
            .limit(2)
            .get()
            .await
            .expect("disjoint older successor");
        assert_eq!(
            second
                .messages
                .iter()
                .map(|message| message.id.as_str())
                .collect::<Vec<_>>(),
            ["m1"]
        );
        assert!(second.next_before.is_none());

        let requests = server.finish().await.expect("scripted history requests");
        assert_eq!(requests.len(), 3);
        assert_eq!(requests[0].method(), "GET");
        assert_eq!(
            requests[0].path(),
            "/v1/spaces/space-id/chats/chat-id/messages?limit=2"
        );
        assert_eq!(requests[1].method(), "POST");
        assert_eq!(
            requests[1].path(),
            "/v1/spaces/space-id/chats/chat-id/messages"
        );
        assert_eq!(requests[2].method(), "GET");
        assert!(
            requests[2]
                .path()
                .starts_with("/v1/spaces/space-id/chats/chat-id/messages?")
        );
        assert!(requests[2].path().contains("limit=2"));
        assert!(requests[2].path().contains("before_order_id=o2"));
        assert!(!requests[2].path().contains("after_order_id"));
    }

    #[tokio::test]
    async fn older_history_rejects_malformed_success_and_response_overrun() {
        let (client, server) = scripted_http_client("200 OK", "application/json", "{}").await;
        let error = client
            .chats()
            .in_space("space-id")
            .older_messages("chat-id")
            .limit(2)
            .get()
            .await
            .expect_err("missing messages evidence must fail");
        assert!(matches!(error, AnytypeError::Deserialization { .. }));
        assert!(!format!("{error:?}").contains("{}"));
        server.await.expect("scripted malformed-success request");

        let timestamp = 1_717_405_200_i64;
        let body = scripted_messages_response(vec![scripted_message(
            "m1",
            "o1",
            "private text",
            timestamp,
            timestamp,
        )]);
        let (client, server) =
            scripted_http_client_with_config("200 OK", "application/json", &body, |config| {
                config.response_limits.json_bytes = 32
            })
            .await;
        let error = client
            .chats()
            .in_space("space-id")
            .older_messages("chat-id")
            .limit(1)
            .get()
            .await
            .expect_err("history response above the JSON ceiling must fail");
        assert!(matches!(
            error,
            AnytypeError::ResponseTooLarge {
                limit: 32,
                declared: Some(_)
            }
        ));
        assert!(!format!("{error:?}").contains("private text"));
        server.await.expect("scripted response-overrun request");
    }

    #[tokio::test]
    async fn older_history_rejects_invalid_limits_before_io() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind invalid-limit sentinel");
        let address = listener.local_addr().expect("invalid-limit address");
        let client = scripted_client(address, "chat-history-invalid-limit", |_| {});

        for limit in [0, MAX_CHAT_HISTORY_PAGE_SIZE + 1] {
            let error = client
                .chats()
                .in_space("space-id")
                .older_messages("chat-id")
                .limit(limit)
                .get()
                .await
                .expect_err("invalid limit must fail");
            assert!(matches!(error, AnytypeError::Validation { .. }));
        }
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), listener.accept())
                .await
                .is_err(),
            "invalid limits must not open a connection"
        );
    }

    #[tokio::test]
    async fn cancelling_older_history_releases_the_transport() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind history cancellation sentinel");
        let address = listener.local_addr().expect("history cancellation address");
        let (accepted_tx, accepted_rx) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener
                .accept()
                .await
                .expect("accept cancellable history request");
            let mut request = Vec::new();
            let mut chunk = [0_u8; 1024];
            while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                let read = socket
                    .read(&mut chunk)
                    .await
                    .expect("read cancellable history request");
                assert_ne!(read, 0, "history request ended before headers");
                request.extend_from_slice(&chunk[..read]);
            }
            accepted_tx
                .send(())
                .expect("signal accepted history request");
            tokio::time::timeout(std::time::Duration::from_secs(2), async {
                matches!(socket.read(&mut chunk).await, Ok(0) | Err(_))
            })
            .await
            .expect("cancelled history request closes promptly")
        });
        let client = scripted_client(address, "chat-history-cancel", |_| {});
        let request = tokio::spawn(async move {
            client
                .chats()
                .in_space("space-id")
                .older_messages("chat-id")
                .limit(2)
                .get()
                .await
        });
        accepted_rx.await.expect("history request was accepted");
        request.abort();
        assert!(
            request
                .await
                .expect_err("aborted history request must cancel")
                .is_cancelled()
        );
        assert!(server.await.expect("history cancellation sentinel"));
    }

    #[tokio::test]
    async fn older_history_retry_failure_has_four_physical_attempts() {
        let responses = (0..4)
            .map(|_| scripted_text_response(StatusCode::GATEWAY_TIMEOUT, "retryable failure"))
            .collect();
        let (client, server) = scripted_http_sequence(responses).await;
        let error = client
            .chats()
            .in_space("space-id")
            .older_messages("chat-id")
            .limit(2)
            .get()
            .await
            .expect_err("history GET must stop after its finite status retry budget");
        let requests = server.finish().await;
        assert!(
            matches!(error, AnytypeError::ApiError { code: 504, .. }),
            "unexpected retry terminal error: {error:?}; fixture: {requests:?}"
        );
        let requests = requests.expect("scripted retry-failure requests");
        assert_eq!(requests.len(), 4);
        assert!(requests.iter().all(|request| {
            request.method() == "GET"
                && request.path() == "/v1/spaces/space-id/chats/chat-id/messages?limit=2"
        }));
        let metrics = client.http_metrics();
        assert_eq!(metrics.logical_operations, 1);
        assert_eq!(metrics.physical_attempts, 4);
        assert_eq!(metrics.retries, 3);
    }

    #[tokio::test]
    async fn older_history_rejects_duplicate_overflow_and_nonprogress_evidence() {
        let timestamp = 1_717_405_200_i64;
        let duplicate = vec![
            serde_json::from_value(scripted_message("same", "o2", "a", timestamp, timestamp))
                .expect("decode scripted message"),
            serde_json::from_value(scripted_message("same", "o1", "b", timestamp, timestamp))
                .expect("decode scripted message"),
        ];
        assert!(matches!(
            decode_history_messages(duplicate, 2),
            Err(AnytypeError::ChatHistoryEvidence {
                kind: ChatHistoryEvidenceKind::DuplicateMessageId
            })
        ));

        let over_limit = (0..=MAX_CHAT_HISTORY_PAGE_SIZE)
            .map(|index| {
                serde_json::from_value(scripted_message(
                    &format!("m{index}"),
                    &format!("o{index}"),
                    "text",
                    timestamp,
                    timestamp,
                ))
                .expect("decode scripted message")
            })
            .collect();
        assert!(matches!(
            decode_history_messages(over_limit, MAX_CHAT_HISTORY_PAGE_SIZE),
            Err(AnytypeError::ChatHistoryEvidence {
                kind: ChatHistoryEvidenceKind::TooManyMessages
            })
        ));

        let response = scripted_messages_response(vec![scripted_message(
            "m1",
            "same-anchor",
            "text",
            timestamp,
            timestamp,
        )]);
        let (client, server) = scripted_http_client("200 OK", "application/json", &response).await;
        let error = client
            .chats()
            .in_space("space-id")
            .older_messages("chat-id")
            .before(
                MessageBeforeAnchor::try_from("same-anchor".to_string())
                    .expect("valid consumed anchor"),
            )
            .limit(1)
            .get()
            .await
            .expect_err("same successor anchor must fail");
        assert!(matches!(
            error,
            AnytypeError::ChatHistoryEvidence {
                kind: ChatHistoryEvidenceKind::NonProgress
            }
        ));
        server.await.expect("scripted nonprogress request");
    }

    #[tokio::test]
    async fn verified_rest_edit_requires_strict_timestamp_advance() {
        let before = scripted_message("m1", "o1", "before", 1, 1);
        let after = scripted_message("m1", "o1", "after", 1, 2);
        let responses = vec![
            scripted_json_response(StatusCode::OK, scripted_message_response(before)),
            scripted_json_response(StatusCode::OK, String::new()),
            scripted_json_response(StatusCode::OK, scripted_message_response(after)),
        ];
        let (client, server) = scripted_http_sequence(responses).await;
        let evidence = client
            .chats()
            .in_space("space-id")
            .edit_message("chat-id", "m1", MessageContent::new().italic("after"))
            .send_verified()
            .await
            .expect("verified REST edit");
        assert!(evidence.after.modified_at > evidence.before.modified_at);
        assert_eq!(evidence.after.content.text, "after");

        let requests = server.finish().await.expect("scripted edit requests");
        assert_eq!(requests.len(), 3);
        assert_eq!(requests[0].method(), "GET");
        assert_eq!(
            requests[0].path(),
            "/v1/spaces/space-id/chats/chat-id/messages/m1"
        );
        assert_eq!(requests[1].method(), "PATCH");
        assert_eq!(
            requests[1].path(),
            "/v1/spaces/space-id/chats/chat-id/messages/m1"
        );
        assert_eq!(requests[2].method(), "GET");
        assert_eq!(
            requests[2].path(),
            "/v1/spaces/space-id/chats/chat-id/messages/m1"
        );
        assert_eq!(
            captured_request_json(&requests[1]),
            serde_json::json!({
                "text": "after",
                "style": "paragraph",
                "marks": [{"from": 0, "to": 5, "type": "italic"}]
            })
        );
    }

    #[tokio::test]
    async fn verified_rest_edit_rejects_equal_timestamp_readback() {
        let before = scripted_message("m1", "o1", "before", 1, 2);
        let after = scripted_message("m1", "o1", "after", 1, 2);
        let responses = vec![
            scripted_json_response(StatusCode::OK, scripted_message_response(before)),
            scripted_json_response(StatusCode::OK, String::new()),
            scripted_json_response(StatusCode::OK, scripted_message_response(after)),
        ];
        let (client, server) = scripted_http_sequence(responses).await;
        let error = client
            .chats()
            .in_space("space-id")
            .edit_message("chat-id", "m1", MessageContent::new().text("after"))
            .send_verified()
            .await
            .expect_err("equal modified_at must fail");
        assert!(matches!(error, AnytypeError::ChatEditTimestampNotAdvanced));
        assert_eq!(
            server
                .finish()
                .await
                .expect("scripted equal-timestamp edit")
                .len(),
            3
        );
    }

    #[tokio::test]
    async fn rest_add_message_sends_current_wire_shape() {
        let (client, server) = scripted_http_client(
            "201 Created",
            "application/json",
            r#"{"message_id":"message-id"}"#,
        )
        .await;

        let message_id = client
            .chats()
            .in_space("space-id")
            .add_message("chat-id", MessageContent::new().bold("hello"))
            .attachments(vec![MessageAttachment {
                target: "file-id".to_string(),
                kind: MessageAttachmentType::Image,
            }])
            .reply_to("parent-id")
            .send()
            .await
            .expect("add REST message");
        assert_eq!(message_id, "message-id");

        let request = server.await.expect("mock server task");
        assert!(request.starts_with("POST /v1/spaces/space-id/chats/chat-id/messages HTTP/1.1"));
        assert_eq!(
            request_json(&request),
            serde_json::json!({
                "text": "hello",
                "style": "paragraph",
                "marks": [{"from": 0, "to": 5, "type": "bold"}],
                "attachments": [{"target": "file-id", "type": "image"}],
                "reply_to_message_id": "parent-id"
            })
        );
    }

    #[tokio::test]
    async fn rest_edit_message_uses_patch_and_http_style_names() {
        let (client, server) = scripted_http_client("200 OK", "application/json", "").await;
        client
            .chats()
            .in_space("space-id")
            .edit_message(
                "chat-id",
                "message-id",
                MessageContent {
                    text: "edited".to_string(),
                    style: MessageTextStyle::Marked,
                    marks: Vec::new(),
                },
            )
            .send()
            .await
            .expect("edit REST message");

        let request = server.await.expect("mock server task");
        assert!(
            request.starts_with(
                "PATCH /v1/spaces/space-id/chats/chat-id/messages/message-id HTTP/1.1"
            )
        );
        assert_eq!(
            request_json(&request),
            serde_json::json!({"text": "edited", "style": "bulleted"})
        );
    }

    #[tokio::test]
    async fn rest_list_chats_forwards_dynamic_filter_values() {
        let body = r#"{"data":[],"pagination":{"has_more":false,"limit":25,"offset":3,"total":0}}"#;
        let (client, server) = scripted_http_client("200 OK", "application/json", body).await;
        let result = client
            .chats()
            .in_space("space-id")
            .list()
            .filter(Filter::Value {
                condition: Condition::Equal,
                property_key: "featured".to_string(),
                value: Some(serde_json::json!(true)),
            })
            .limit(25)
            .offset(3)
            .list()
            .await
            .expect("list filtered chats");
        assert!(result.is_empty());

        let request = server.await.expect("mock server task");
        let request_line = request.lines().next().expect("request line");
        assert!(request_line.starts_with("GET /v1/spaces/space-id/chats?"));
        assert!(request_line.contains("featured=true"));
        assert!(request_line.contains("limit=25"));
        assert!(request_line.contains("offset=3"));
    }

    #[tokio::test]
    async fn rest_chat_stream_sends_configuration_and_decodes_typed_events() {
        let body = concat!(
            ": heartbeat\n\n",
            "event: message_added\n",
            "data: {\"type\":\"message_added\",\"payload\":{\"message\":{\"id\":\"m1\",\"order_id\":\"o1\",\"creator\":\"p1\",\"created_at\":0,\"modified_at\":0,\"content\":{\"text\":\"hello\"}}}}\n\n",
            "event: message_updated\n",
            "data: {\"type\":\"message_updated\",\"payload\":{\"message\":{\"id\":\"m1\",\"order_id\":\"o1\",\"creator\":\"p1\",\"created_at\":0,\"modified_at\":1,\"content\":{\"text\":\"edited\"}}}}\n\n",
            "event: message_deleted\n",
            "data: {\"type\":\"message_deleted\",\"payload\":{\"id\":\"m1\"}}\n\n",
            "event: reactions_updated\n",
            "data: {\"type\":\"reactions_updated\",\"payload\":{\"id\":\"m2\",\"reactions\":{\"👍\":[\"p1\"]}}}\n\n",
        );
        let (client, server) = scripted_http_client("200 OK", "text/event-stream", body).await;
        let mut events = client
            .chats()
            .in_space("space-id")
            .message_stream("chat-id")
            .limit(2)
            .heartbeat_seconds(7)
            .open()
            .await
            .expect("open REST chat stream");

        assert!(matches!(
            events.next().await.expect("added event").expect("decode"),
            ChatHttpEvent::MessageAdded { message } if message.id == "m1" && message.content.text == "hello"
        ));
        assert!(matches!(
            events.next().await.expect("updated event").expect("decode"),
            ChatHttpEvent::MessageUpdated { message } if message.content.text == "edited"
        ));
        assert!(matches!(
            events.next().await.expect("deleted event").expect("decode"),
            ChatHttpEvent::MessageDeleted { message_id } if message_id == "m1"
        ));
        assert!(matches!(
            events.next().await.expect("reaction event").expect("decode"),
            ChatHttpEvent::ReactionsUpdated { message_id, reactions }
                if message_id == "m2" && reactions[0].emoji == "👍"
        ));
        assert!(events.next().await.is_none());

        let request = server.await.expect("mock server task").to_ascii_lowercase();
        assert!(
            request.starts_with(
                "get /v1/spaces/space-id/chats/chat-id/messages/stream?limit=2 http/1.1"
            )
        );
        assert!(request.contains("\r\naccept: text/event-stream\r\n"));
        assert!(request.contains("\r\nanytype-heartbeat-seconds: 7\r\n"));
    }

    #[tokio::test]
    async fn rest_chat_stream_rejects_invalid_configuration_before_connecting() {
        let id = NEXT_SCRIPT_ID.fetch_add(1, Ordering::Relaxed);
        let key_path = std::env::temp_dir().join(format!(
            "anytype-chat-validation-unit-{}-{id}.db",
            std::process::id()
        ));
        let mut config = ClientConfig::default().app_name("chat-stream-validation");
        config.base_url = Some("http://127.0.0.1:1".to_string());
        config.keystore = Some(format!("file:path={}", key_path.display()));
        config.keystore_service = Some(format!("chat-stream-validation-{id}"));
        let client = AnytypeClient::with_config(config).expect("create validation client");
        let result = client
            .chats()
            .in_space("space-id")
            .message_stream("chat-id")
            .heartbeat_seconds(61)
            .open()
            .await;
        let err = match result {
            Err(err) => err,
            Ok(_) => panic!("invalid heartbeat must fail"),
        };
        assert!(matches!(err, crate::error::AnytypeError::Validation { .. }));
    }

    fn collect_sse_frames(chunks: &[&[u8]], limit: u64) -> Result<Vec<Vec<u8>>> {
        let mut buffer = Vec::new();
        let mut frames = Vec::new();
        for chunk in chunks {
            for byte in *chunk {
                if let Some(delimiter_len) = append_sse_byte(&mut buffer, *byte, limit)? {
                    let frame_end = buffer.len() - delimiter_len;
                    frames.push(buffer[..frame_end].to_vec());
                    buffer.clear();
                }
            }
        }
        if !buffer.is_empty() {
            frames.push(buffer);
        }
        Ok(frames)
    }

    #[test]
    fn sse_buffer_accepts_exact_limit_and_rejects_one_over_without_growth() {
        let exact = b"data:x\n\n";
        assert_eq!(
            collect_sse_frames(&[exact], exact.len() as u64).unwrap(),
            vec![b"data:x".to_vec()]
        );

        let secret = b"token-and-body-must-not-leak";
        let mut buffer = Vec::new();
        for byte in &secret[..8] {
            let _ = append_sse_byte(&mut buffer, *byte, 8).unwrap();
        }
        let error = append_sse_byte(&mut buffer, secret[8], 8).unwrap_err();
        assert_eq!(buffer.len(), 8, "one-over byte is rejected before growth");
        assert!(matches!(
            error,
            AnytypeError::ChatSseEventTooLarge { limit: 8 }
        ));
        let rendered = error.to_string();
        assert!(!rendered.contains("token"));
        assert!(!rendered.contains("body"));
    }

    #[test]
    fn sse_buffer_bounds_delimiter_free_chunks_and_accepts_split_delimiters() {
        let split = collect_sse_frames(&[b"data:a\r", b"\n\r", b"\ndata:b\n", b"\n"], 10).unwrap();
        assert_eq!(split, vec![b"data:a".to_vec(), b"data:b".to_vec()]);

        let error = collect_sse_frames(&[b"1234", b"5678", b"9"], 8).unwrap_err();
        assert!(matches!(
            error,
            AnytypeError::ChatSseEventTooLarge { limit: 8 }
        ));
    }

    #[test]
    fn delimiter_free_megabyte_uses_incremental_boundary_detection() {
        let body = vec![b'x'; 1024 * 1024];
        let frames = collect_sse_frames(&[&body], body.len() as u64).unwrap();
        assert_eq!(frames, vec![body]);
    }

    #[test]
    fn terminating_stream_releases_event_buffer_allocation() {
        let mut state = ChatHttpSseState {
            chunks: futures::stream::empty().boxed(),
            buffer: Vec::with_capacity(1024 * 1024),
            pending: None,
            pending_offset: 0,
            finished: false,
            event_limit: 1024 * 1024,
        };
        state.buffer.extend_from_slice(b"delimiter-free prefix");
        assert!(state.buffer.capacity() >= 1024 * 1024);

        state.terminate();

        assert_eq!(state.buffer.capacity(), 0);
        assert!(state.finished);
    }

    #[test]
    fn eof_finalization_moves_event_buffer_out_of_terminal_state() {
        let mut state = ChatHttpSseState {
            chunks: futures::stream::empty().boxed(),
            buffer: Vec::with_capacity(1024 * 1024),
            pending: None,
            pending_offset: 0,
            finished: false,
            event_limit: 1024 * 1024,
        };
        state
            .buffer
            .extend_from_slice(b"delimiter-free final event");
        assert!(state.buffer.capacity() >= 1024 * 1024);

        let final_buffer = state.finish_at_eof().expect("nonempty final event");

        assert_eq!(final_buffer, b"delimiter-free final event");
        assert_eq!(state.buffer.capacity(), 0);
        assert!(state.finished);
    }

    #[tokio::test]
    async fn stream_path_ids_are_validated_before_url_construction() {
        let id = NEXT_SCRIPT_ID.fetch_add(1, Ordering::Relaxed);
        let key_path = std::env::temp_dir().join(format!(
            "anytype-chat-path-validation-unit-{}-{id}.db",
            std::process::id()
        ));
        let mut config = ClientConfig::default().app_name("chat-path-validation-unit");
        config.base_url = Some("http://alice:authority-secret@127.0.0.1:1".to_string());
        config.keystore = Some(format!("file:path={}", key_path.display()));
        config.keystore_service = Some(format!("chat-path-validation-unit-{id}"));
        let client = AnytypeClient::with_config(config).expect("create path validation client");
        client.set_api_key(HttpCredentials::new("bearer-secret"));

        for (space_id, chat_id) in [
            ("space?space-query#space-fragment", "chat-id"),
            ("space-id", "chat?chat-query#chat-fragment"),
        ] {
            let error = match client
                .chats()
                .in_space(space_id)
                .message_stream(chat_id)
                .open()
                .await
            {
                Err(error) => error,
                Ok(_) => panic!("unsafe path identifier must fail before transport"),
            };
            assert!(matches!(error, AnytypeError::Validation { .. }));
            assert!(std::error::Error::source(&error).is_none());
            let rendered = format!("{error} {error:?}");
            for secret in [
                "space-query",
                "space-fragment",
                "chat-query",
                "chat-fragment",
                "authority-secret",
                "bearer-secret",
            ] {
                assert!(!rendered.contains(secret));
            }
        }
    }

    #[test]
    fn chat_stream_diagnostic_omits_url_credentials_query_and_fragment() {
        let url = reqwest::Url::parse(
            "https://alice:secret@example.invalid/v1/spaces/s/chats/c/messages/stream?token=hidden#body",
        )
        .unwrap();
        let diagnostic = chat_stream_diagnostic_path(&url);
        assert_eq!(diagnostic, "/v1/spaces/s/chats/c/messages/stream");
        for secret in [
            "alice",
            "secret",
            "token",
            "hidden",
            "body",
            "example.invalid",
        ] {
            assert!(!diagnostic.contains(secret));
        }
        let error = AnytypeError::ChatSseTransport { path: diagnostic };
        let rendered = format!("{error}");
        let debug = format!("{error:?}");
        assert!(std::error::Error::source(&error).is_none());
        for secret in [
            "alice",
            "secret",
            "token",
            "hidden",
            "body",
            "example.invalid",
        ] {
            assert!(!rendered.contains(secret));
            assert!(!debug.contains(secret));
        }
    }

    #[tokio::test]
    async fn opening_transport_failure_discards_raw_url_and_source() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind transport failure endpoint");
        let address = listener
            .local_addr()
            .expect("transport failure endpoint address");
        let server = tokio::spawn(async move {
            let (socket, _) = listener
                .accept()
                .await
                .expect("accept transport failure request");
            drop(socket);
        });

        let id = NEXT_SCRIPT_ID.fetch_add(1, Ordering::Relaxed);
        let key_path = std::env::temp_dir().join(format!(
            "anytype-chat-open-error-unit-{}-{id}.db",
            std::process::id()
        ));
        let mut config = ClientConfig::default().app_name("chat-open-error-unit");
        config.base_url = Some(format!("http://alice:secret@{address}"));
        config.keystore = Some(format!("file:path={}", key_path.display()));
        config.keystore_service = Some(format!("chat-open-error-unit-{id}"));
        let client = AnytypeClient::with_config(config).expect("create transport error client");
        client.set_api_key(HttpCredentials::new("test-token"));

        let error = match client
            .chats()
            .in_space("space-id")
            .message_stream("chat-id")
            .open()
            .await
        {
            Err(error) => error,
            Ok(_) => panic!("closed endpoint must fail"),
        };
        assert!(matches!(
            error,
            AnytypeError::ChatSseTransport { ref path }
                if path == "/v1/spaces/space-id/chats/chat-id/messages/stream"
        ));
        assert!(std::error::Error::source(&error).is_none());
        let rendered = format!("{error} {error:?}");
        let address_secret = address.to_string();
        for secret in ["alice", "secret", "test-token", &address_secret] {
            assert!(!rendered.contains(secret));
        }
        server.await.expect("transport failure server");
    }

    #[tokio::test]
    async fn overflowing_stream_terminates_and_releases_transport_state() {
        let (mut client, server) =
            scripted_http_client("200 OK", "text/event-stream", "123456789").await;
        client.config.response_limits.chat_sse_event_bytes = 8;
        let mut events = client
            .chats()
            .in_space("space-id")
            .message_stream("chat-id")
            .open()
            .await
            .expect("open bounded stream");
        assert!(matches!(
            events.next().await,
            Some(Err(AnytypeError::ChatSseEventTooLarge { limit: 8 }))
        ));
        assert!(
            events.next().await.is_none(),
            "overflow terminates the stream"
        );
        drop(events);
        server
            .await
            .expect("mock transport is released after overflow");
    }

    #[tokio::test]
    async fn one_transport_chunk_can_carry_multiple_exact_limit_events() {
        let event = "data: {\"type\":\"bounded\",\"payload\":null}\n\n";
        let body = format!("{event}{event}");
        let (mut client, server) = scripted_http_client("200 OK", "text/event-stream", &body).await;
        client.config.response_limits.chat_sse_event_bytes = event.len() as u64;
        let mut events = client
            .chats()
            .in_space("space-id")
            .message_stream("chat-id")
            .open()
            .await
            .expect("open bounded stream");

        for _ in 0..2 {
            assert!(matches!(
                events.next().await,
                Some(Ok(ChatHttpEvent::Unknown { event_type, payload }))
                    if event_type == "bounded" && payload.is_null()
            ));
        }
        assert!(events.next().await.is_none());
        server.await.expect("multiple-event transport server");
    }

    #[tokio::test]
    async fn dropping_stream_cancels_incomplete_transport() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind cancellation server");
        let address = listener.local_addr().expect("cancellation server address");
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("accept stream request");
            let mut request = Vec::new();
            let mut chunk = [0_u8; 1024];
            while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                let read = socket.read(&mut chunk).await.expect("read stream request");
                assert_ne!(read, 0, "request ended before headers");
                request.extend_from_slice(&chunk[..read]);
            }
            socket
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n",
                )
                .await
                .expect("write streaming headers");

            let closed = tokio::time::timeout(std::time::Duration::from_secs(2), async {
                match socket.read(&mut chunk).await {
                    Ok(0) | Err(_) => true,
                    Ok(_) => false,
                }
            })
            .await
            .expect("dropped response body closes transport");
            assert!(closed, "stream cancellation must not leave transport live");
        });

        let id = NEXT_SCRIPT_ID.fetch_add(1, Ordering::Relaxed);
        let key_path = std::env::temp_dir().join(format!(
            "anytype-chat-cancel-unit-{}-{id}.db",
            std::process::id()
        ));
        let mut config = ClientConfig::default().app_name("chat-stream-cancel-unit");
        config.base_url = Some(format!("http://{address}"));
        config.keystore = Some(format!("file:path={}", key_path.display()));
        config.keystore_service = Some(format!("chat-stream-cancel-unit-{id}"));
        let client = AnytypeClient::with_config(config).expect("create cancellation client");
        client.set_api_key(HttpCredentials::new("test-token"));

        let events = client
            .chats()
            .in_space("space-id")
            .message_stream("chat-id")
            .open()
            .await
            .expect("open cancellable stream");
        drop(events);
        server.await.expect("cancellation server task");
    }

    #[test]
    fn current_http_message_schema_preserves_available_fields() {
        let wire: HttpChatMessage = serde_json::from_value(serde_json::json!({
            "id": "message-id",
            "order_id": "order-id",
            "creator": "participant-id",
            "creator_name": "Alice",
            "created_at": 1_717_405_200,
            "modified_at": 1_717_405_201_000_i64,
            "reply_to_message_id": "parent-id",
            "content": {
                "text": "Hello",
                "style": "header1",
                "marks": [{"from": 0, "to": 5, "type": "bold", "param": ""}]
            },
            "attachments": [{"target": "file-id", "type": "image"}],
            "reactions": {"👍": ["participant-id"]},
            "pinned": true
        }))
        .expect("deserialize current anytype-heart chat message");

        let message = ChatMessage::try_from(wire).expect("valid REST timestamp evidence");
        assert_eq!(message.creator_name.as_deref(), Some("Alice"));
        assert!(matches!(message.content.style, MessageTextStyle::Header1));
        assert!(matches!(
            message.content.marks[0].kind,
            MessageTextMarkType::Bold
        ));
        assert!(matches!(
            message.attachments[0].kind,
            MessageAttachmentType::Image
        ));
        assert_eq!(message.reactions[0].emoji, "👍");
        assert!(message.pinned);
        assert!(message.blocks.is_empty());
        assert_eq!(message.created_at.timestamp(), 1_717_405_200);
        assert_eq!(message.modified_at.timestamp_millis(), 1_717_405_201_000);
    }

    #[test]
    fn unknown_http_style_and_mark_are_retained() {
        let wire: HttpChatMessage = serde_json::from_value(serde_json::json!({
            "id": "message-id",
            "order_id": "order-id",
            "creator": "participant-id",
            "created_at": 0,
            "modified_at": 0,
            "content": {
                "text": "Hello",
                "style": "future_style",
                "marks": [{"from": 0, "to": 5, "type": "future_mark"}]
            }
        }))
        .expect("deserialize future-compatible chat message");

        let message = ChatMessage::try_from(wire).expect("valid REST timestamp evidence");
        assert!(matches!(
            message.content.style,
            MessageTextStyle::Other(ref value) if value == "future_style"
        ));
        assert!(matches!(
            message.content.marks[0].kind,
            MessageTextMarkType::Other(ref value) if value == "future_mark"
        ));
    }

    #[test]
    fn structured_grpc_blocks_round_trip() {
        let blocks = vec![
            MessageBlock::Text(MessageBlockText {
                text: "heading".to_string(),
                style: MessageTextStyle::Header2,
                marks: Vec::new(),
                checked: false,
                language: Some("en".to_string()),
            }),
            MessageBlock::Link(MessageBlockLink {
                target_object_id: "file-id".to_string(),
                kind: MessageBlockLinkType::File,
            }),
        ];

        let round_trip: Vec<MessageBlock> = blocks
            .into_iter()
            .map(grpc_message_block)
            .filter_map(message_block_from_grpc)
            .collect();

        assert!(matches!(
            &round_trip[0],
            MessageBlock::Text(text)
                if text.text == "heading"
                    && matches!(text.style, MessageTextStyle::Header2)
                    && text.language.as_deref() == Some("en")
        ));
        assert!(matches!(
            &round_trip[1],
            MessageBlock::Link(link)
                if link.target_object_id == "file-id"
                    && matches!(link.kind, MessageBlockLinkType::File)
        ));
    }

    #[test]
    fn grpc_message_conversion_retains_rich_state() {
        let message = model::ChatMessage {
            id: "message-id".to_string(),
            order_id: "order-id".to_string(),
            creator: "participant-id".to_string(),
            created_at: 1_717_405_200_000,
            modified_at: 1_717_405_201_000,
            state_id: "state-id".to_string(),
            reply_to_message_id: String::new(),
            message: None,
            attachments: Vec::new(),
            reactions: None,
            read: true,
            mention_read: true,
            has_mention: false,
            synced: true,
            pinned: true,
            unread_reaction: true,
            blocks: vec![grpc_message_block(MessageBlock::Text(MessageBlockText {
                text: "rich".to_string(),
                ..MessageBlockText::default()
            }))],
        };

        let converted = chat_message_from_grpc(message).expect("valid gRPC timestamp evidence");
        assert_eq!(converted.state_id, "state-id");
        assert!(converted.read);
        assert!(converted.synced);
        assert!(converted.pinned);
        assert!(converted.unread_reaction);
        assert_eq!(converted.blocks.len(), 1);
    }

    #[test]
    fn rest_paths_and_read_body_match_heart_routes() {
        assert_eq!(
            chat_message_path("space-id", "chat-id", None),
            "/v1/spaces/space-id/chats/chat-id/messages"
        );
        assert_eq!(
            chat_message_path("space-id", "chat-id", Some("message-id")),
            "/v1/spaces/space-id/chats/chat-id/messages/message-id"
        );

        let body = serde_json::to_value(ReadMessagesBody {
            before_order_id: Some("before".to_string()),
            after_order_id: None,
            last_state_id: Some("state".to_string()),
            read_type: Some("mentions".to_string()),
        })
        .expect("serialize read request");
        assert_eq!(
            body,
            serde_json::json!({
                "before_order_id": "before",
                "last_state_id": "state",
                "type": "mentions"
            })
        );
    }

    #[tokio::test]
    async fn direct_grpc_chat_builders_validate_before_transport() {
        let id = NEXT_SCRIPT_ID.fetch_add(1, Ordering::Relaxed);
        let key_path = std::env::temp_dir().join(format!(
            "anytype-chat-validation-{}-{id}.db",
            std::process::id()
        ));
        let mut config = ClientConfig::default().app_name("chat-validation-unit");
        config.keystore = Some(format!("file:path={}", key_path.display()));
        config.keystore_service = Some(format!("chat-validation-unit-{id}"));
        let client = AnytypeClient::with_config(config).expect("construct validation-only client");

        assert!(matches!(
            client.chats().send_text("", "text").send().await,
            Err(AnytypeError::Validation { .. })
        ));
        assert!(matches!(
            client.chats().send_text("chat-id", "").send().await,
            Err(AnytypeError::Validation { .. })
        ));
        assert!(matches!(
            client
                .chats()
                .toggle_reaction("chat-id", "message-id", "")
                .send()
                .await,
            Err(AnytypeError::Validation { .. })
        ));
        assert!(matches!(
            client.chats().read_all("").mark_read().await,
            Err(AnytypeError::Validation { .. })
        ));

        for suffix in ["", "-shm", "-wal"] {
            let _ = std::fs::remove_file(format!("{}{}", key_path.display(), suffix));
        }
    }

    #[tokio::test]
    async fn direct_grpc_chat_builder_preserves_typed_transport_failure() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind closed gRPC endpoint");
        let address = listener.local_addr().expect("closed gRPC endpoint address");
        drop(listener);

        let id = NEXT_SCRIPT_ID.fetch_add(1, Ordering::Relaxed);
        let key_path = std::env::temp_dir().join(format!(
            "anytype-chat-grpc-failure-{}-{id}.db",
            std::process::id()
        ));
        let mut config = ClientConfig::default().app_name("chat-grpc-failure-unit");
        config.grpc_endpoint = Some(format!("http://{address}"));
        config.keystore = Some(format!("file:path={}", key_path.display()));
        config.keystore_service = Some(format!("chat-grpc-failure-unit-{id}"));
        let client = AnytypeClient::with_config(config).expect("create gRPC failure client");
        client
            .keystore
            .update_grpc_credentials(&GrpcCredentials::from_token("test-session-token"))
            .expect("store isolated gRPC credentials");

        let result = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            client.chats().send_text("chat-id", "text").send(),
        )
        .await
        .expect("closed loopback failure is bounded");

        client
            .keystore
            .clear_all_credentials()
            .expect("clear isolated gRPC credentials");
        for suffix in ["", "-shm", "-wal"] {
            let _ = std::fs::remove_file(format!("{}{}", key_path.display(), suffix));
        }

        assert!(matches!(
            result,
            Err(AnytypeError::Grpc {
                source: AnytypeGrpcError::Transport { .. },
            })
        ));
    }
}
