//! # Anytype Chats (gRPC)
//!
//! gRPC-backed chat message operations.
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

#[cfg(feature = "grpc")]
use anytype_rpc::anytype::rpc::{
    chat::{
        add_message, delete_message, edit_message_content, get_messages, get_messages_by_ids,
        read_all, read_messages, toggle_message_reaction, unread,
    },
    object::search_with_meta,
    workspace::open as workspace_open,
};
use anytype_rpc::model;
use chrono::{DateTime, FixedOffset, TimeZone, Utc};
use prost_types::{Struct, Value};
use serde::{Deserialize, Serialize};
use tonic::Request;

use crate::{
    Result,
    client::AnytypeClient,
    error::AnytypeError,
    grpc_util::{ensure_error_ok, grpc_status, with_token_request},
    objects::{Color, DataModel, Object, ObjectLayout},
    properties::{PropertyValue, PropertyWithValue},
    validation::looks_like_object_id,
};

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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(clippy::struct_excessive_bools)]
pub struct ChatMessage {
    pub id: String,
    pub order_id: String,
    pub state_id: String,
    pub creator: String,
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

    /// Mark all messages as read (if supported server-side).
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
        chat_search(
            self.client,
            self.space_id,
            None,
            Vec::new(),
            self.limit,
            self.offset,
        )
        .await
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

    pub async fn send(self) -> Result<String> {
        ChatAddMessageRequest {
            client: self.client,
            chat_object_id: self.chat_object_id,
            content: Some(MessageContent {
                text: self.text,
                style: self.style,
                marks: self.marks,
            }),
            attachments: self.attachments,
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
        let _ = self.space_id;
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

pub struct ChatAddMessageRequest<'a> {
    client: &'a AnytypeClient,
    chat_object_id: String,
    content: Option<MessageContent>,
    attachments: Vec<MessageAttachment>,
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
            reply_to_message_id: String::new(),
            message: Some(grpc_message_content(content)),
            attachments: grpc_attachments(self.attachments),
            reactions: None,
            read: false,
            mention_read: false,
            has_mention: false,
            synced: false,
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
            attachments: Vec::new(),
            reactions: None,
            read: false,
            mention_read: false,
            has_mention: false,
            synced: false,
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
            .collect();
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
        Ok(response
            .messages
            .into_iter()
            .map(chat_message_from_grpc)
            .collect())
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
        return Some(timestamp_to_datetime(value as i64).to_rfc3339());
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

pub(crate) fn chat_message_from_grpc(message: model::ChatMessage) -> ChatMessage {
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
    ChatMessage {
        id: message.id,
        order_id: message.order_id,
        state_id: message.state_id,
        creator: message.creator,
        created_at: timestamp_to_datetime(message.created_at),
        modified_at: timestamp_to_datetime(message.modified_at),
        reply_to_message_id: empty_to_none(message.reply_to_message_id),
        content,
        attachments,
        reactions,
        read: message.read,
        mention_read: message.mention_read,
        has_mention: message.has_mention,
        synced: message.synced,
    }
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

fn timestamp_to_datetime(value: i64) -> DateTime<FixedOffset> {
    let offset = FixedOffset::east_opt(0).unwrap();
    if value.abs() > 10_000_000_000 {
        offset
            .timestamp_millis_opt(value)
            .single()
            .unwrap_or_else(|| offset.timestamp_opt(0, 0).single().unwrap())
    } else {
        offset
            .timestamp_opt(value, 0)
            .single()
            .unwrap_or_else(|| offset.timestamp_opt(0, 0).single().unwrap())
    }
}

fn empty_to_none(value: String) -> Option<String> {
    if value.is_empty() { None } else { Some(value) }
}
