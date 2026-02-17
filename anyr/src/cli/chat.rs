use std::{collections::HashMap, io::Read, str::FromStr};

use anyhow::{Result, anyhow, bail};
use anytype::{prelude::*, validation::looks_like_object_id};
use clap::ValueEnum;
use futures::StreamExt;

use crate::{
    cli::{
        AppContext,
        common::{
            MemberCache, load_member_cache, resolve_chat_ids, resolve_chat_name,
            resolve_chat_target, resolve_member_name, resolve_space_id, resolve_type_key,
        },
        pagination_limit, pagination_offset,
    },
    output::{OutputFormat, render_table_dynamic},
};

#[allow(clippy::too_many_lines, clippy::large_stack_frames)]
pub async fn handle(ctx: &AppContext, args: super::ChatArgs) -> Result<()> {
    match *args.command {
        super::ChatCommands::List {
            space,
            text,
            pagination,
        } => {
            let (space_id, result) = if let Some(space) = space.as_deref() {
                let space_id = resolve_space_id(ctx, space).await?;
                if let Some(text) = text {
                    let mut request = ctx
                        .client
                        .chats()
                        .search_chats_in(&space_id)
                        .text(text)
                        .limit(pagination_limit(&pagination))
                        .offset(pagination_offset(&pagination));
                    if pagination.all {
                        request = request.limit(1000).offset(0);
                    }
                    (Some(space_id), request.search().await?)
                } else {
                    let mut request = ctx
                        .client
                        .chats()
                        .list_chats_in(&space_id)
                        .limit(pagination_limit(&pagination))
                        .offset(pagination_offset(&pagination));
                    if pagination.all {
                        request = request.limit(1000).offset(0);
                    }
                    (Some(space_id), request.list().await?)
                }
            } else if let Some(text) = text {
                let mut request = ctx
                    .client
                    .chats()
                    .search_chats()
                    .text(text)
                    .limit(pagination_limit(&pagination))
                    .offset(pagination_offset(&pagination));
                if pagination.all {
                    request = request.limit(1000).offset(0);
                }
                (None, request.search().await?)
            } else {
                let mut request = ctx
                    .client
                    .chats()
                    .list_chats()
                    .limit(pagination_limit(&pagination))
                    .offset(pagination_offset(&pagination));
                if pagination.all {
                    request = request.limit(1000).offset(0);
                }
                (None, request.list().await?)
            };

            match ctx.output.format() {
                OutputFormat::Table => {
                    let space_names = load_space_names(ctx).await?;
                    let rows = result
                        .items
                        .iter()
                        .map(|chat| {
                            let name = chat.name.clone().unwrap_or_default();
                            let space_name = space_names
                                .get(&chat.space_id)
                                .cloned()
                                .or_else(|| space_id.clone())
                                .unwrap_or_else(|| chat.space_id.clone());
                            vec![chat.id.clone(), name, space_name, chat.archived.to_string()]
                        })
                        .collect::<Vec<_>>();
                    let headers = vec![
                        "id".to_string(),
                        "name".to_string(),
                        "space".to_string(),
                        "archived".to_string(),
                    ];
                    let table = render_table_dynamic(&headers, &rows);
                    ctx.output.emit_text(&table)
                }
                _ => ctx.output.emit_json(&result),
            }
        }
        super::ChatCommands::Create { space, name } => {
            let space_id = resolve_space_id(ctx, &space).await?;
            let chat_type_key = match resolve_type_key(ctx, &space_id, "Chat").await {
                Ok(key) => key,
                Err(first_err) => resolve_type_key(ctx, &space_id, "chat")
                    .await
                    .map_err(|_| first_err)?,
            };
            let chat = ctx
                .client
                .new_object(&space_id, chat_type_key)
                .name(name)
                .create()
                .await?;
            ctx.output.emit_json(&chat)
        }
        super::ChatCommands::Get { space, chat } => {
            let space_id = resolve_space_id(ctx, &space).await?;
            let (_space_id, chat_id) = resolve_chat_target(ctx, Some(&space_id), &chat).await?;
            let chat = ctx
                .client
                .chats()
                .get_chat(&space_id, &chat_id)
                .get()
                .await?;
            ctx.output.emit_json(&chat)
        }
        super::ChatCommands::Messages(args) => match args.command {
            super::ChatMessagesCommands::List {
                space,
                chat,
                after,
                before,
                include_boundary,
                limit,
                unread_only,
            } => {
                let space_id = resolve_space_id(ctx, &space).await?;
                let (_space_id, chat_id) = resolve_chat_target(ctx, Some(&space_id), &chat).await?;
                let mut request = ctx.client.chats().list_messages(&chat_id).limit(limit);

                if let Some(after) = after {
                    request = request.after(decode_order_id_arg(&after)?);
                }
                if let Some(before) = before {
                    request = request.before(decode_order_id_arg(&before)?);
                }
                if include_boundary {
                    request = request.include_boundary(true);
                }
                if let Some(read_type) = unread_only {
                    request = request.unread_only(read_type.to_read_type());
                }

                let mut page = request.list_page().await?;
                for message in &mut page.messages {
                    message.order_id = encode_order_id_hex(&message.order_id);
                }
                match ctx.output.format() {
                    OutputFormat::Table => {
                        let member_cache = Some(load_member_cache(ctx, &space_id).await?);
                        let headers = vec![
                            "order_id".to_string(),
                            "timestamp".to_string(),
                            "sender".to_string(),
                            "message".to_string(),
                        ];
                        let rows = page
                            .messages
                            .iter()
                            .map(|message| {
                                let sender = format_sender(
                                    Some(space_id.as_str()),
                                    member_cache.as_ref(),
                                    &message.creator,
                                );
                                vec![
                                    message.order_id.clone(),
                                    message.created_at.format(&ctx.date_format).to_string(),
                                    sender,
                                    message.content.text.clone(),
                                ]
                            })
                            .collect::<Vec<_>>();
                        let table = render_table_dynamic(&headers, &rows);
                        ctx.output.emit_text(&table)
                    }
                    _ => ctx.output.emit_json(&page),
                }
            }
            super::ChatMessagesCommands::Get {
                space,
                chat,
                message_ids,
            } => {
                let space_id = resolve_space_id(ctx, &space).await?;
                let (_space_id, chat_id) = resolve_chat_target(ctx, Some(&space_id), &chat).await?;
                let message_ids = resolve_message_ids(ctx, &chat_id, &message_ids).await?;
                let mut messages = ctx
                    .client
                    .chats()
                    .get_messages(&chat_id, message_ids)
                    .get()
                    .await?;
                for message in &mut messages {
                    message.order_id = encode_order_id_hex(&message.order_id);
                }

                match ctx.output.format() {
                    OutputFormat::Table => {
                        let member_cache = Some(load_member_cache(ctx, &space_id).await?);
                        let headers = vec![
                            "timestamp".to_string(),
                            "sender".to_string(),
                            "message".to_string(),
                            "id".to_string(),
                        ];
                        let rows = messages
                            .iter()
                            .map(|message| {
                                let sender = format_sender(
                                    Some(space_id.as_str()),
                                    member_cache.as_ref(),
                                    &message.creator,
                                );
                                vec![
                                    message.created_at.format(&ctx.date_format).to_string(),
                                    sender,
                                    message.content.text.clone(),
                                    message.id.clone(),
                                ]
                            })
                            .collect::<Vec<_>>();
                        let table = render_table_dynamic(&headers, &rows);
                        ctx.output.emit_text(&table)
                    }
                    _ => ctx.output.emit_json(&messages),
                }
            }
            super::ChatMessagesCommands::Send {
                space,
                chat,
                text,
                style,
                mark,
                attachment,
                content_json,
                content_text,
                text_args,
            } => {
                let space_id = resolve_space_id(ctx, &space).await?;
                let (_space_id, chat_id) = resolve_chat_target(ctx, Some(&space_id), &chat).await?;
                let attachments = parse_message_attachments(&attachment)?;

                let message_id = if let Some(content_json) = content_json {
                    let content = parse_message_content_json(&content_json)?;
                    ctx.client
                        .chats()
                        .add_message(&chat_id)
                        .content(content)
                        .attachments(attachments)
                        .send()
                        .await?
                } else {
                    let text = if let Some(content_text) = content_text {
                        read_content_text(&content_text)?
                    } else if let Some(text) = text {
                        text
                    } else if !text_args.is_empty() {
                        text_args.join(" ")
                    } else {
                        bail!(
                            "message text is required (use --text, positional TEXT, or --content-text)"
                        );
                    };
                    let style = style.unwrap_or_default().to_style();
                    let marks = parse_message_marks(&mark)?;
                    ctx.client
                        .chats()
                        .send_text(&chat_id, text)
                        .style(style)
                        .marks(marks)
                        .attachments(attachments)
                        .send()
                        .await?
                };

                ctx.output.emit_json(&MessageIdOutput { id: message_id })
            }
            super::ChatMessagesCommands::Edit {
                space,
                chat,
                message_id,
                text,
                style,
                mark,
                content_json,
            } => {
                let space_id = resolve_space_id(ctx, &space).await?;
                let (_space_id, chat_id) = resolve_chat_target(ctx, Some(&space_id), &chat).await?;
                let message_id = resolve_message_id_for_order(ctx, &chat_id, &message_id).await?;

                if let Some(content_json) = content_json {
                    let content = parse_message_content_json(&content_json)?;
                    ctx.client
                        .chats()
                        .edit_message(&chat_id, &message_id)
                        .content(content)
                        .send()
                        .await?;
                } else {
                    let text = text.ok_or_else(|| anyhow!("--text is required"))?;
                    let style = style.unwrap_or_default().to_style();
                    let marks = parse_message_marks(&mark)?;
                    ctx.client
                        .chats()
                        .edit_text(&chat_id, &message_id, text)
                        .style(style)
                        .marks(marks)
                        .send()
                        .await?;
                }

                ctx.output.emit_json(&ResultOutput { result: true })
            }
            super::ChatMessagesCommands::Delete {
                space,
                chat,
                message_id,
            } => {
                let space_id = resolve_space_id(ctx, &space).await?;
                let (_space_id, chat_id) = resolve_chat_target(ctx, Some(&space_id), &chat).await?;
                let message_id = resolve_message_id_for_order(ctx, &chat_id, &message_id).await?;
                ctx.client
                    .chats()
                    .delete_message(&chat_id, &message_id)
                    .delete()
                    .await?;
                ctx.output.emit_json(&ResultOutput { result: true })
            }
        },
        super::ChatCommands::Read {
            space,
            chat,
            read_type,
            after,
            before,
            last_state_id,
        } => {
            let space_id = resolve_space_id(ctx, &space).await?;
            let (_space_id, chat_id) = resolve_chat_target(ctx, Some(&space_id), &chat).await?;
            let mut request = ctx.client.chats().read_messages(&chat_id);
            if let Some(read_type) = read_type {
                request = request.read_type(read_type.to_read_type());
            }
            if let Some(after) = after {
                request = request.after(decode_order_id_arg(&after)?);
            }
            if let Some(before) = before {
                request = request.before(decode_order_id_arg(&before)?);
            }
            if let Some(last_state_id) = last_state_id {
                request = request.last_state_id(last_state_id);
            }
            request.mark_read().await?;
            ctx.output.emit_json(&ResultOutput { result: true })
        }
        super::ChatCommands::Unread {
            space,
            chat,
            read_type,
            after,
        } => {
            let space_id = resolve_space_id(ctx, &space).await?;
            let (_space_id, chat_id) = resolve_chat_target(ctx, Some(&space_id), &chat).await?;
            let mut request = ctx.client.chats().unread_messages(&chat_id);
            if let Some(read_type) = read_type {
                request = request.read_type(read_type.to_read_type());
            }
            if let Some(after) = after {
                request = request.after(decode_order_id_arg(&after)?);
            }
            request.mark_unread().await?;
            ctx.output.emit_json(&ResultOutput { result: true })
        }
        super::ChatCommands::Listen {
            chats,
            space,
            include_history,
            after,
            show_events,
        } => {
            let space_id = match space.as_deref() {
                Some(space) => Some(resolve_space_id(ctx, space).await?),
                None => None,
            };
            let chat_ids = resolve_chat_ids(ctx, space_id.as_deref(), &chats).await?;
            if chat_ids.is_empty() {
                bail!("at least one --chat is required");
            }

            let member_cache = match space_id.as_deref() {
                Some(space_id) => Some(load_member_cache(ctx, space_id).await?),
                None => None,
            };

            if let Some(limit) = include_history {
                let show_chat = chat_ids.len() > 1;
                let mut chat_names: HashMap<String, String> = HashMap::new();
                for chat_id in &chat_ids {
                    let chat_label =
                        resolve_chat_label(ctx, space_id.as_deref(), &mut chat_names, chat_id)
                            .await?;
                    let mut request = ctx.client.chats().list_messages(chat_id).limit(limit);
                    if let Some(after) = after.clone() {
                        request = request.after(decode_order_id_arg(&after)?);
                    }
                    let page = request.list_page().await?;
                    emit_message_rows(
                        ctx,
                        Some(&chat_label),
                        &page.messages,
                        show_chat,
                        space_id.as_deref(),
                        member_cache.as_ref(),
                    )?;
                }
            }

            let mut builder = ctx.client.chat_stream();
            for chat_id in &chat_ids {
                builder = builder.subscribe_chat(chat_id);
            }
            let ChatStreamHandle { mut events, .. } = builder.build();

            let mut chat_names: HashMap<String, String> = HashMap::new();
            while let Some(event) = events.next().await {
                match event {
                    ChatEvent::MessageAdded { chat_id, message }
                    | ChatEvent::MessageUpdated { chat_id, message } => {
                        let chat_label =
                            resolve_chat_label(ctx, space_id.as_deref(), &mut chat_names, &chat_id)
                                .await?;
                        emit_message_rows(
                            ctx,
                            Some(&chat_label),
                            &[message],
                            chat_ids.len() > 1,
                            space_id.as_deref(),
                            member_cache.as_ref(),
                        )?;
                    }
                    ChatEvent::MessageDeleted {
                        chat_id,
                        message_id,
                    } => {
                        if show_events {
                            let chat_label = resolve_chat_label(
                                ctx,
                                space_id.as_deref(),
                                &mut chat_names,
                                &chat_id,
                            )
                            .await?;
                            let line = format!("message deleted: {chat_label} {message_id}");
                            ctx.output.emit_text(&line)?;
                        }
                    }
                    ChatEvent::ReactionsUpdated {
                        chat_id,
                        message_id,
                        reactions,
                    } => {
                        if show_events {
                            let chat_label = resolve_chat_label(
                                ctx,
                                space_id.as_deref(),
                                &mut chat_names,
                                &chat_id,
                            )
                            .await?;
                            let summary = reactions
                                .iter()
                                .map(|reaction| reaction.emoji.clone())
                                .collect::<Vec<_>>()
                                .join(" ");
                            let line =
                                format!("reactions updated: {chat_label} {message_id} {summary}");
                            ctx.output.emit_text(&line)?;
                        }
                    }
                    ChatEvent::ChatStateUpdated { .. } => {
                        if show_events {
                            ctx.output.emit_text("chat state updated")?;
                        }
                    }
                    ChatEvent::StreamDisconnected => {
                        if show_events {
                            ctx.output.emit_text("stream disconnected")?;
                        }
                    }
                    ChatEvent::StreamResubscribed => {
                        if show_events {
                            ctx.output.emit_text("stream resubscribed")?;
                        }
                    }
                }
            }
            Ok(())
        }
    }
}

#[derive(serde::Serialize)]
struct ResultOutput {
    result: bool,
}

#[derive(serde::Serialize)]
struct MessageIdOutput {
    id: String,
}

#[derive(Clone, Copy, Debug, Default, ValueEnum)]
pub enum MessageStyleArg {
    #[value(name = "paragraph")]
    #[default]
    Paragraph,
    #[value(name = "header1")]
    Header1,
    #[value(name = "header2")]
    Header2,
    #[value(name = "header3")]
    Header3,
    #[value(name = "header4")]
    Header4,
    #[value(name = "quote")]
    Quote,
    #[value(name = "code")]
    Code,
    #[value(name = "title")]
    Title,
    #[value(name = "checkbox")]
    Checkbox,
    #[value(name = "marked")]
    Marked,
    #[value(name = "numbered")]
    Numbered,
    #[value(name = "toggle")]
    Toggle,
    #[value(name = "description")]
    Description,
    #[value(name = "callout")]
    Callout,
}

impl MessageStyleArg {
    fn to_style(self) -> MessageTextStyle {
        match self {
            Self::Paragraph => MessageTextStyle::Paragraph,
            Self::Header1 => MessageTextStyle::Header1,
            Self::Header2 => MessageTextStyle::Header2,
            Self::Header3 => MessageTextStyle::Header3,
            Self::Header4 => MessageTextStyle::Header4,
            Self::Quote => MessageTextStyle::Quote,
            Self::Code => MessageTextStyle::Code,
            Self::Title => MessageTextStyle::Title,
            Self::Checkbox => MessageTextStyle::Checkbox,
            Self::Marked => MessageTextStyle::Marked,
            Self::Numbered => MessageTextStyle::Numbered,
            Self::Toggle => MessageTextStyle::Toggle,
            Self::Description => MessageTextStyle::Description,
            Self::Callout => MessageTextStyle::Callout,
        }
    }
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum ChatReadTypeArg {
    #[value(name = "messages")]
    Messages,
    #[value(name = "mentions")]
    Mentions,
}

impl ChatReadTypeArg {
    fn to_read_type(self) -> ChatReadType {
        match self {
            Self::Messages => ChatReadType::Messages,
            Self::Mentions => ChatReadType::Mentions,
        }
    }
}

fn parse_message_content_json(value: &str) -> Result<MessageContent> {
    let contents = read_content_source(value)?;
    let content: MessageContent = serde_json::from_str(&contents)?;
    Ok(content)
}

fn read_content_text(value: &str) -> Result<String> {
    read_content_source(value)
}

fn read_content_source(value: &str) -> Result<String> {
    if value == "-" || value == "@-" {
        let mut contents = String::new();
        std::io::stdin().read_to_string(&mut contents)?;
        return Ok(contents);
    }
    if let Some(path) = value.strip_prefix('@') {
        if path.is_empty() {
            bail!("content source is empty; use @file, @-, or -");
        }
        let contents =
            std::fs::read_to_string(path).map_err(|err| anyhow!("read {path}: {err}"))?;
        return Ok(contents);
    }
    bail!("content source must be @file, @-, or -");
}

async fn resolve_message_id_for_order(
    ctx: &AppContext,
    chat_id: &str,
    message_id_or_order_id: &str,
) -> Result<String> {
    if looks_like_object_id(message_id_or_order_id) {
        return Ok(message_id_or_order_id.to_string());
    }

    let order_id = decode_order_id_arg(message_id_or_order_id)?;
    let page = ctx
        .client
        .chats()
        .list_messages(chat_id)
        .after(order_id.clone())
        .before(order_id.clone())
        .include_boundary(true)
        .limit(1)
        .list_page()
        .await?;

    let message = page
        .messages
        .into_iter()
        .find(|message| message.order_id == order_id)
        .ok_or_else(|| anyhow!("message not found for order id: {order_id}"))?;

    Ok(message.id)
}

async fn resolve_message_ids(
    ctx: &AppContext,
    chat_id: &str,
    message_ids: &[String],
) -> Result<Vec<String>> {
    let mut resolved = Vec::with_capacity(message_ids.len());
    for message_id in message_ids {
        resolved.push(resolve_message_id_for_order(ctx, chat_id, message_id).await?);
    }
    Ok(resolved)
}

fn encode_order_id_hex(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len() * 2);
    for byte in value.as_bytes() {
        encoded.push(hex_char(byte >> 4));
        encoded.push(hex_char(byte & 0x0f));
    }
    encoded
}

fn hex_char(value: u8) -> char {
    match value {
        0..=9 => (b'0' + value) as char,
        10..=15 => (b'a' + (value - 10)) as char,
        _ => '0',
    }
}

fn decode_order_id_arg(value: &str) -> Result<String> {
    if !is_hex_string(value) {
        return Ok(value.to_string());
    }

    let mut bytes = Vec::with_capacity(value.len() / 2);
    for chunk in value.as_bytes().chunks(2) {
        let hi = hex_value(chunk[0])?;
        let lo = hex_value(chunk[1])?;
        bytes.push((hi << 4) | lo);
    }
    String::from_utf8(bytes).map_err(|_| anyhow!("invalid order id hex: {value}"))
}

fn is_hex_string(value: &str) -> bool {
    if value.is_empty() || !value.len().is_multiple_of(2) {
        return false;
    }
    value.chars().all(|ch| ch.is_ascii_hexdigit())
}

fn hex_value(value: u8) -> Result<u8> {
    match value {
        b'0'..=b'9' => Ok(value - b'0'),
        b'a'..=b'f' => Ok(value - b'a' + 10),
        b'A'..=b'F' => Ok(value - b'A' + 10),
        _ => Err(anyhow!("invalid hex value")),
    }
}

fn parse_message_marks(values: &[String]) -> Result<Vec<MessageTextMark>> {
    values
        .iter()
        .map(|value| parse_message_mark(value))
        .collect()
}

fn parse_message_mark(value: &str) -> Result<MessageTextMark> {
    let mut parts = value.splitn(4, ':');
    let kind = parts.next().unwrap_or_default();
    if kind.is_empty() {
        bail!("invalid mark: {value}");
    }
    let kind =
        MessageTextMarkType::from_str(kind).map_err(|_| anyhow!("invalid mark type: {kind}"))?;

    let from = parts.next();
    let to = parts.next();
    let param = parts.next();

    let range = match (from, to) {
        (None, None) => None,
        (Some(from), Some(to)) => {
            let from: i32 = from
                .parse()
                .map_err(|_| anyhow!("invalid mark range: {value}"))?;
            let to: i32 = to
                .parse()
                .map_err(|_| anyhow!("invalid mark range: {value}"))?;
            Some(MessageTextRange { from, to })
        }
        (Some(_), None) => bail!("mark range missing end: {value}"),
        (None, Some(_)) => bail!("mark range missing from: {value}"),
    };

    Ok(MessageTextMark {
        range,
        kind,
        param: param.map(ToString::to_string),
    })
}

fn parse_message_attachments(values: &[String]) -> Result<Vec<MessageAttachment>> {
    values
        .iter()
        .map(|value| parse_message_attachment(value))
        .collect()
}

fn parse_message_attachment(value: &str) -> Result<MessageAttachment> {
    let (kind, target) = value
        .split_once(':')
        .ok_or_else(|| anyhow!("invalid attachment: {value}"))?;
    if target.is_empty() {
        bail!("invalid attachment: {value}");
    }

    let kind = match kind {
        "file" => MessageAttachmentType::File,
        "image" => MessageAttachmentType::Image,
        "link" => MessageAttachmentType::Link,
        _ => bail!("invalid attachment type: {kind}"),
    };

    Ok(MessageAttachment {
        target: target.to_string(),
        kind,
    })
}

fn emit_message_rows(
    ctx: &AppContext,
    chat_label: Option<&str>,
    messages: &[ChatMessage],
    show_chat: bool,
    space_id: Option<&str>,
    member_cache: Option<&MemberCache>,
) -> Result<()> {
    for message in messages {
        let sender = format_sender(space_id, member_cache, &message.creator);
        let timestamp = message.created_at.format(&ctx.date_format).to_string();
        let line = if show_chat {
            let chat_label = chat_label.unwrap_or_default();
            format!(
                "{timestamp}\t{chat_label}\t{sender}\t{}",
                message.content.text
            )
        } else {
            format!("{timestamp}\t{sender}\t{}", message.content.text)
        };
        ctx.output.emit_text(&line)?;
    }
    Ok(())
}

fn format_sender(
    space_id: Option<&str>,
    member_cache: Option<&MemberCache>,
    value: &str,
) -> String {
    if let (Some(space_id), Some(member_cache)) = (space_id, member_cache) {
        resolve_member_name(space_id, member_cache, value)
    } else {
        value.chars().take(8).collect()
    }
}

async fn resolve_chat_label(
    ctx: &AppContext,
    space_id: Option<&str>,
    cache: &mut HashMap<String, String>,
    chat_id: &str,
) -> Result<String> {
    if let Some(label) = cache.get(chat_id) {
        return Ok(label.clone());
    }
    let name = resolve_chat_name(ctx, space_id, chat_id).await?;
    cache.insert(chat_id.to_string(), name.clone());
    Ok(name)
}

async fn load_space_names(ctx: &AppContext) -> Result<HashMap<String, String>> {
    let spaces = ctx.client.spaces().list().await?.collect_all().await?;
    Ok(spaces
        .into_iter()
        .map(|space| (space.id, space.name))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_order_id_hex_basic() {
        assert_eq!(encode_order_id_hex("!!@,"), "2121402c");
        assert_eq!(encode_order_id_hex("AbC"), "416243");
    }

    #[test]
    fn decode_order_id_hex_roundtrip() {
        let decoded = decode_order_id_arg("2121402c").expect("decode hex");
        assert_eq!(decoded, "!!@,");
    }

    #[test]
    fn decode_order_id_non_hex_passthrough() {
        let decoded = decode_order_id_arg("abc").expect("passthrough");
        assert_eq!(decoded, "abc");
    }

    #[test]
    fn decode_order_id_invalid_utf8() {
        assert!(decode_order_id_arg("ff").is_err());
    }
}
