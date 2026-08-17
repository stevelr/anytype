use std::{collections::HashMap, io::Read, str::FromStr};

use anyhow::{Result, anyhow, bail};
use anytype::{prelude::*, validation::looks_like_object_id};
use clap::ValueEnum;
use futures::StreamExt;
use tracing::info;

use crate::{
    cli::{
        AppContext,
        common::{MemberCache, load_member_cache, resolve_member_name},
        pagination_limit, pagination_offset,
    },
    filter::parse_filters,
    output::{OutputFormat, render_table_dynamic},
};

#[allow(clippy::too_many_lines, clippy::large_stack_frames)]
pub async fn handle(ctx: &AppContext, args: super::ChatArgs) -> Result<()> {
    // Resolve the transport backend for this operation before touching the
    // network. `resolve_transport` both enforces the rejection guards
    // (`--transport rest` on gRPC-only operations, `--transport grpc` on
    // REST-only ones) and returns the backend each handler dispatches on:
    // REST-capable operations run through `SpaceChatsClient` when the resolved
    // backend is `Rest`/`RestSse`, and gRPC otherwise. The backend is also
    // surfaced in verbose diagnostics.
    let op = classify(&args.command);
    let backend = resolve_transport(args.transport, &op)?;
    info!(
        "chat transport backend for `{}`: {backend} (requested {})",
        op.name, args.transport
    );

    match *args.command {
        super::ChatCommands::List {
            space,
            text,
            filter,
            pagination,
        } => {
            let filters = parse_filters(&filter.filters)?;
            let (space_id, result) = if let Some(space) = space.as_deref() {
                let space_id = ctx.client.resolve_space_id(space).await?;
                if let Some(text) = text {
                    if !filters.is_empty() {
                        bail!(
                            "--filter cannot be combined with --text; chat text search uses the gRPC discovery API"
                        );
                    }
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
                    // Space-scoped plain listing routes through the REST chat
                    // builder so `--filter` can be applied server-side.
                    let mut request = ctx
                        .client
                        .chats()
                        .in_space(&space_id)
                        .list()
                        .limit(pagination_limit(&pagination))
                        .offset(pagination_offset(&pagination));
                    for filter in filters {
                        request = request.filter(filter);
                    }
                    let items = if pagination.all {
                        request.list().await?.collect_all().await?
                    } else {
                        request.list().await?.into_response().items
                    };
                    (Some(space_id), ChatListResult { items })
                }
            } else if let Some(text) = text {
                if !filters.is_empty() {
                    bail!("--filter requires --space (single-space REST listing)");
                }
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
                if !filters.is_empty() {
                    bail!("--filter requires --space (single-space REST listing)");
                }
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
        super::ChatCommands::Create {
            space,
            name,
            icon_emoji,
            icon_file,
        } => {
            let space_id = ctx.client.resolve_space_id(&space).await?;
            let icon = match (icon_emoji, icon_file) {
                (Some(emoji), None) => Some(Icon::Emoji { emoji }),
                (None, Some(file)) => Some(Icon::File { file }),
                (None, None) => None,
                // clap's `chat_icon` group already rejects supplying both.
                (Some(_), Some(_)) => bail!("--icon-emoji and --icon-file are mutually exclusive"),
            };
            // The dedicated REST chat builder always attaches an icon, so it is
            // used when REST is selected and an icon is supplied; otherwise fall
            // back to the generic REST object create (which needs no icon).
            let chat = match (backend, icon) {
                (ChatBackend::Rest, Some(icon)) => {
                    ctx.client
                        .chats()
                        .in_space(&space_id)
                        .create(name, icon)
                        .create()
                        .await?
                }
                (_, icon) => create_chat_object(ctx, &space_id, name, icon).await?,
            };
            ctx.output.emit_json(&chat)
        }
        super::ChatCommands::Get { space, chat } => {
            let space_id = ctx.client.resolve_space_id(&space).await?;
            let chat_id = ctx
                .client
                .resolve_chat_target(Some(&space_id), &chat)
                .await?
                .chat_id;
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
                let space_id = ctx.client.resolve_space_id(&space).await?;
                let chat_id = ctx
                    .client
                    .resolve_chat_target(Some(&space_id), &chat)
                    .await?
                    .chat_id;
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
                let space_id = ctx.client.resolve_space_id(&space).await?;
                let chat_id = ctx
                    .client
                    .resolve_chat_target(Some(&space_id), &chat)
                    .await?
                    .chat_id;
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
                reply_to,
                blocks_json,
                text_args,
            } => {
                let space_id = ctx.client.resolve_space_id(&space).await?;
                let chat_id = ctx
                    .client
                    .resolve_chat_target(Some(&space_id), &chat)
                    .await?
                    .chat_id;
                let attachments = parse_message_attachments(&attachment)?;
                let reply_to_id = match reply_to {
                    Some(reply) => Some(resolve_message_id_for_order(ctx, &chat_id, &reply).await?),
                    None => None,
                };

                let message_id = if backend == ChatBackend::Rest {
                    // REST plain-message send. `--blocks-json` is gRPC-only, so
                    // the transport policy routes any blocks send to gRPC.
                    let content = if let Some(content_json) = content_json {
                        parse_message_content_json(&content_json)?
                    } else {
                        let text = resolve_message_text(text, content_text, &text_args)?
                            .ok_or_else(|| {
                                anyhow!(
                                    "message text is required (use --text, positional TEXT, or --content-text)"
                                )
                            })?;
                        MessageContent {
                            text,
                            style: style.unwrap_or_default().to_style(),
                            marks: parse_message_marks(&mark)?,
                        }
                    };
                    let mut request = ctx
                        .client
                        .chats()
                        .in_space(&space_id)
                        .add_message(&chat_id, content)
                        .attachments(attachments);
                    if let Some(reply_to_id) = reply_to_id {
                        request = request.reply_to(reply_to_id);
                    }
                    request.send().await?
                } else {
                    let blocks = match blocks_json {
                        Some(blocks_json) => parse_message_blocks_json(&blocks_json)?,
                        None => Vec::new(),
                    };
                    let content = if let Some(content_json) = content_json {
                        parse_message_content_json(&content_json)?
                    } else {
                        match resolve_message_text(text, content_text, &text_args)? {
                            Some(text) => MessageContent {
                                text,
                                style: style.unwrap_or_default().to_style(),
                                marks: parse_message_marks(&mark)?,
                            },
                            // Blocks can carry the body on their own.
                            None if !blocks.is_empty() => MessageContent::default(),
                            None => bail!(
                                "message text is required (use --text, positional TEXT, --content-text, or --blocks-json)"
                            ),
                        }
                    };
                    let mut request = ctx
                        .client
                        .chats()
                        .add_message(&chat_id)
                        .content(content)
                        .attachments(attachments)
                        .blocks(blocks);
                    if let Some(reply_to_id) = reply_to_id {
                        request = request.reply_to(reply_to_id);
                    }
                    request.send().await?
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
                attachment,
                content_json,
                blocks_json,
            } => {
                let space_id = ctx.client.resolve_space_id(&space).await?;
                let chat_id = ctx
                    .client
                    .resolve_chat_target(Some(&space_id), &chat)
                    .await?
                    .chat_id;
                let message_id = resolve_message_id_for_order(ctx, &chat_id, &message_id).await?;
                // Supplied `--attachment` values are the complete replacement list.
                let attachments = parse_message_attachments(&attachment)?;

                if backend == ChatBackend::Rest {
                    let content = if let Some(content_json) = content_json {
                        parse_message_content_json(&content_json)?
                    } else {
                        let text = text.ok_or_else(|| anyhow!("--text is required"))?;
                        MessageContent {
                            text,
                            style: style.unwrap_or_default().to_style(),
                            marks: parse_message_marks(&mark)?,
                        }
                    };
                    ctx.client
                        .chats()
                        .in_space(&space_id)
                        .edit_message(&chat_id, &message_id, content)
                        .attachments(attachments)
                        .send()
                        .await?;
                } else {
                    let blocks = match blocks_json {
                        Some(blocks_json) => parse_message_blocks_json(&blocks_json)?,
                        None => Vec::new(),
                    };
                    let content = if let Some(content_json) = content_json {
                        parse_message_content_json(&content_json)?
                    } else {
                        match text {
                            Some(text) => MessageContent {
                                text,
                                style: style.unwrap_or_default().to_style(),
                                marks: parse_message_marks(&mark)?,
                            },
                            None if !blocks.is_empty() => MessageContent::default(),
                            None => bail!(
                                "--text is required (or provide --content-json or --blocks-json)"
                            ),
                        }
                    };
                    ctx.client
                        .chats()
                        .edit_message(&chat_id, &message_id)
                        .content(content)
                        .attachments(attachments)
                        .blocks(blocks)
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
                let space_id = ctx.client.resolve_space_id(&space).await?;
                let chat_id = ctx
                    .client
                    .resolve_chat_target(Some(&space_id), &chat)
                    .await?
                    .chat_id;
                let message_id = resolve_message_id_for_order(ctx, &chat_id, &message_id).await?;
                ctx.client
                    .chats()
                    .delete_message(&chat_id, &message_id)
                    .delete()
                    .await?;
                ctx.output.emit_json(&ResultOutput { result: true })
            }
            super::ChatMessagesCommands::Search {
                space,
                chat,
                query,
                pagination,
            } => {
                let space_id = ctx.client.resolve_space_id(&space).await?;
                let chat_id = ctx
                    .client
                    .resolve_chat_target(Some(&space_id), &chat)
                    .await?
                    .chat_id;
                let mut request = ctx
                    .client
                    .chats()
                    .in_space(&space_id)
                    .search_messages(&chat_id, query)
                    .limit(pagination_limit(&pagination))
                    .offset(pagination_offset(&pagination));
                if pagination.all {
                    request = request.limit(1000).offset(0);
                }
                let mut page = request.search().await?;
                for result in &mut page.items {
                    result.message.order_id = encode_order_id_hex(&result.message.order_id);
                }
                match ctx.output.format() {
                    OutputFormat::Table => {
                        let headers = vec![
                            "order_id".to_string(),
                            "score".to_string(),
                            "highlight".to_string(),
                        ];
                        let rows = page
                            .items
                            .iter()
                            .map(|result| {
                                vec![
                                    result.message.order_id.clone(),
                                    result.score.to_string(),
                                    result.highlight.clone(),
                                ]
                            })
                            .collect::<Vec<_>>();
                        let table = render_table_dynamic(&headers, &rows);
                        ctx.output.emit_text(&table)
                    }
                    _ => ctx.output.emit_json(&page),
                }
            }
            super::ChatMessagesCommands::React {
                space,
                chat,
                message_id,
                emoji,
            } => {
                let space_id = ctx.client.resolve_space_id(&space).await?;
                let chat_id = ctx
                    .client
                    .resolve_chat_target(Some(&space_id), &chat)
                    .await?
                    .chat_id;
                let message_id = resolve_message_id_for_order(ctx, &chat_id, &message_id).await?;
                let added = if backend == ChatBackend::Rest {
                    // REST toggle does not report the resulting on/off state.
                    ctx.client
                        .chats()
                        .in_space(&space_id)
                        .toggle_reaction(&chat_id, &message_id, emoji)
                        .await?;
                    None
                } else {
                    Some(
                        ctx.client
                            .chats()
                            .toggle_reaction(&chat_id, &message_id, emoji)
                            .send()
                            .await?,
                    )
                };
                ctx.output.emit_json(&ReactionOutput { added })
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
            let space_id = ctx.client.resolve_space_id(&space).await?;
            let chat_id = ctx
                .client
                .resolve_chat_target(Some(&space_id), &chat)
                .await?
                .chat_id;
            let read_type = read_type.map(ChatReadTypeArg::to_read_type);
            let after = after.map(|order| decode_order_id_arg(&order)).transpose()?;
            let before = before
                .map(|order| decode_order_id_arg(&order))
                .transpose()?;
            if backend == ChatBackend::Rest {
                // Route to the space-scoped REST read builder.
                let mut request = ctx
                    .client
                    .chats()
                    .in_space(&space_id)
                    .read_messages(&chat_id);
                if let Some(read_type) = read_type {
                    request = request.read_type(read_type);
                }
                if let Some(after) = after {
                    request = request.after(after);
                }
                if let Some(before) = before {
                    request = request.before(before);
                }
                if let Some(last_state_id) = last_state_id {
                    request = request.last_state_id(last_state_id);
                }
                request.mark_read().await?;
            } else {
                let mut request = ctx.client.chats().read_messages(&chat_id);
                if let Some(read_type) = read_type {
                    request = request.read_type(read_type);
                }
                if let Some(after) = after {
                    request = request.after(after);
                }
                if let Some(before) = before {
                    request = request.before(before);
                }
                if let Some(last_state_id) = last_state_id {
                    request = request.last_state_id(last_state_id);
                }
                request.mark_read().await?;
            }
            ctx.output.emit_json(&ResultOutput { result: true })
        }
        super::ChatCommands::ReadReactions {
            space,
            chat,
            order_id,
        } => {
            let space_id = ctx.client.resolve_space_id(&space).await?;
            let chat_id = ctx
                .client
                .resolve_chat_target(Some(&space_id), &chat)
                .await?
                .chat_id;
            let mut request = ctx
                .client
                .chats()
                .in_space(&space_id)
                .read_reactions(&chat_id);
            if let Some(order_id) = order_id {
                request = request.through(decode_order_id_arg(&order_id)?);
            }
            request.mark_read().await?;
            ctx.output.emit_json(&ResultOutput { result: true })
        }
        super::ChatCommands::ReadAll { space, chat } => {
            let space_id = ctx.client.resolve_space_id(&space).await?;
            let chat_id = ctx
                .client
                .resolve_chat_target(Some(&space_id), &chat)
                .await?
                .chat_id;
            ctx.client
                .chats()
                .in_space(&space_id)
                .read_all(&chat_id)
                .await?;
            ctx.output.emit_json(&ResultOutput { result: true })
        }
        super::ChatCommands::Unread {
            space,
            chat,
            read_type,
            after,
        } => {
            let space_id = ctx.client.resolve_space_id(&space).await?;
            let chat_id = ctx
                .client
                .resolve_chat_target(Some(&space_id), &chat)
                .await?
                .chat_id;
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
            initial_limit,
            heartbeat,
            previews,
            buffer,
        } => {
            if backend == ChatBackend::RestSse {
                if include_history.is_some() {
                    bail!(
                        "--include-history is a gRPC listener option; use --initial-limit for REST SSE"
                    );
                }
                if after.is_some() {
                    bail!(
                        "--after is a gRPC listener option; REST SSE replays via --initial-limit"
                    );
                }
                return listen_rest_sse(
                    ctx,
                    space.as_deref(),
                    &chats,
                    initial_limit,
                    heartbeat,
                    show_events,
                )
                .await;
            }

            // gRPC reconnecting listener path.
            if initial_limit.is_some() {
                bail!("--initial-limit only applies to the REST SSE listener (single --chat)");
            }
            if heartbeat.is_some() {
                bail!("--heartbeat only applies to the REST SSE listener (single --chat)");
            }
            if buffer == Some(0) {
                bail!("--buffer must be at least 1");
            }

            let space_id = match space.as_deref() {
                Some(space) => Some(ctx.client.resolve_space_id(space).await?),
                None => None,
            };
            let chat_ids = ctx
                .client
                .resolve_chat_ids(space_id.as_deref(), &chats)
                .await?;
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
            if previews {
                builder = builder.subscribe_previews();
            }
            if let Some(buffer) = buffer {
                builder = builder.buffer(buffer);
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

#[derive(serde::Serialize)]
struct ReactionOutput {
    /// Resulting reaction state when the backend reports it (gRPC toggle).
    /// `None` for REST, whose toggle response carries no state.
    #[serde(skip_serializing_if = "Option::is_none")]
    added: Option<bool>,
}

/// Resolves the message text from the `--text`, positional `TEXT`, or
/// `--content-text` inputs, returning `None` when none were supplied.
fn resolve_message_text(
    text: Option<String>,
    content_text: Option<String>,
    text_args: &[String],
) -> Result<Option<String>> {
    if let Some(content_text) = content_text {
        return Ok(Some(read_content_text(&content_text)?));
    }
    if let Some(text) = text {
        return Ok(Some(text));
    }
    if !text_args.is_empty() {
        return Ok(Some(text_args.join(" ")));
    }
    Ok(None)
}

/// Parses a JSON array of structured [`MessageBlock`] values from a `@file`,
/// `@-`, or `-` source.
fn parse_message_blocks_json(value: &str) -> Result<Vec<MessageBlock>> {
    let contents = read_content_source(value)?;
    let blocks: Vec<MessageBlock> = serde_json::from_str(&contents)?;
    Ok(blocks)
}

/// Creates a chat as a generic space object (no dedicated icon builder).
async fn create_chat_object(
    ctx: &AppContext,
    space_id: &str,
    name: String,
    icon: Option<Icon>,
) -> Result<Object> {
    let chat_type_key = match ctx.client.resolve_type_key(space_id, "Chat").await {
        Ok(key) => key,
        Err(first_err) => ctx
            .client
            .resolve_type_key(space_id, "chat")
            .await
            .map_err(|_| first_err)?,
    };
    let mut request = ctx.client.new_object(space_id, chat_type_key).name(name);
    if let Some(icon) = icon {
        request = request.icon(icon);
    }
    request.create().await.map_err(Into::into)
}

/// Streams one chat over the REST Server-Sent Events endpoint.
async fn listen_rest_sse(
    ctx: &AppContext,
    space: Option<&str>,
    chats: &[String],
    initial_limit: Option<u32>,
    heartbeat: Option<u32>,
    show_events: bool,
) -> Result<()> {
    let space = space.ok_or_else(|| anyhow!("--space is required for a REST SSE listen"))?;
    let [chat] = chats else {
        bail!("REST SSE listen requires exactly one --chat");
    };
    let space_id = ctx.client.resolve_space_id(space).await?;
    let chat_id = ctx
        .client
        .resolve_chat_target(Some(&space_id), chat)
        .await?
        .chat_id;
    let member_cache = Some(load_member_cache(ctx, &space_id).await?);
    let chat_label = ctx
        .client
        .resolve_chat_name(Some(&space_id), &chat_id)
        .await?;

    let mut request = ctx
        .client
        .chats()
        .in_space(&space_id)
        .message_stream(&chat_id);
    if let Some(limit) = initial_limit {
        request = request.limit(limit);
    }
    if let Some(seconds) = heartbeat {
        request = request.heartbeat_seconds(seconds);
    }
    let mut events = request.open().await?;

    while let Some(event) = events.next().await {
        match event? {
            ChatHttpEvent::MessageAdded { message } | ChatHttpEvent::MessageUpdated { message } => {
                emit_message_rows(
                    ctx,
                    Some(&chat_label),
                    &[message],
                    false,
                    Some(&space_id),
                    member_cache.as_ref(),
                )?;
            }
            ChatHttpEvent::MessageDeleted { message_id } => {
                if show_events {
                    ctx.output
                        .emit_text(&format!("message deleted: {chat_label} {message_id}"))?;
                }
            }
            ChatHttpEvent::ReactionsUpdated {
                message_id,
                reactions,
            } => {
                if show_events {
                    let summary = reactions
                        .iter()
                        .map(|reaction| reaction.emoji.clone())
                        .collect::<Vec<_>>()
                        .join(" ");
                    ctx.output.emit_text(&format!(
                        "reactions updated: {chat_label} {message_id} {summary}"
                    ))?;
                }
            }
            ChatHttpEvent::Unknown { event_type, .. } => {
                if show_events {
                    ctx.output.emit_text(&format!("event: {event_type}"))?;
                }
            }
        }
    }
    Ok(())
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

/// Transport requested on the command line via `anyr chat --transport ...`.
///
/// The selector currently drives the transport *policy* (which backend each
/// operation is intended to use, reported in `-v` diagnostics) and the
/// `rest` rejection guard. Per-operation REST routing for REST-capable message
/// operations is staged for follow-up work, so `auto`/`grpc` do not yet change
/// which backend a handler dispatches through.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
pub enum TransportArg {
    /// Resolve each operation to its policy backend from the documented table.
    #[default]
    #[value(name = "auto")]
    Auto,
    /// Reject operations and options that only gRPC can serve.
    #[value(name = "rest")]
    Rest,
    /// Prefer gRPC, which carries the full-fidelity 0.4 message reply shape.
    #[value(name = "grpc")]
    Grpc,
}

impl std::fmt::Display for TransportArg {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Auto => "auto",
            Self::Rest => "rest",
            Self::Grpc => "grpc",
        })
    }
}

/// Backend actually selected for a chat operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChatBackend {
    /// REST request/response.
    Rest,
    /// REST server-sent-events stream (single-chat listen).
    RestSse,
    /// gRPC request/response or stream.
    Grpc,
}

impl std::fmt::Display for ChatBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Rest => "rest",
            Self::RestSse => "rest-sse",
            Self::Grpc => "grpc",
        })
    }
}

/// Transport policy for a single chat invocation.
struct OpTransport {
    /// Command path used in diagnostics and error messages (e.g. `messages get`).
    name: &'static str,
    /// Backend chosen when `--transport auto` is in effect.
    auto: ChatBackend,
    /// `Some(reason)` when REST cannot serve this invocation; `None` when it can.
    grpc_only: Option<&'static str>,
    /// `Some(reason)` when gRPC cannot serve this invocation; `None` when it can.
    rest_only: Option<&'static str>,
}

impl OpTransport {
    /// Policy for an operation both transports serve, defaulting to REST.
    fn rest(name: &'static str) -> Self {
        Self {
            name,
            auto: ChatBackend::Rest,
            grpc_only: None,
            rest_only: None,
        }
    }

    /// Policy for an operation only gRPC can serve.
    fn grpc(name: &'static str, reason: &'static str) -> Self {
        Self {
            name,
            auto: ChatBackend::Grpc,
            grpc_only: Some(reason),
            rest_only: None,
        }
    }

    /// Policy for an operation only REST can serve.
    fn rest_only(name: &'static str, reason: &'static str) -> Self {
        Self {
            name,
            auto: ChatBackend::Rest,
            grpc_only: None,
            rest_only: Some(reason),
        }
    }
}

/// Classifies a parsed chat command into the http/gRPC transport policy
fn classify(command: &super::ChatCommands) -> OpTransport {
    use super::ChatCommands as C;
    use OpTransport as T;
    let (rest, grpc, rest_only) = (T::rest, T::grpc, T::rest_only);

    match command {
        C::List { space, text, .. } => {
            if space.is_none() {
                grpc(
                    "list",
                    "listing chats across all spaces requires gRPC; pass --space to list within one space",
                )
            } else if text.is_some() {
                grpc("list", "chat text search uses the gRPC discovery API")
            } else {
                rest("list")
            }
        }
        C::Create { .. } => rest("create"),
        C::Get { .. } => grpc("get", "rich chat-object lookup requires gRPC"),
        C::Messages(args) => classify_messages(&args.command),
        C::Read { .. } => rest("read"),
        C::ReadReactions { .. } => rest_only(
            "read-reactions",
            "marking chat reactions read is a REST-only operation",
        ),
        C::ReadAll { .. } => rest_only(
            "read-all",
            "marking every chat message read is a REST-only operation",
        ),
        C::Unread { .. } => grpc("unread", "marking messages unread requires gRPC"),
        C::Listen {
            chats,
            space,
            include_history,
            after,
            previews,
            buffer,
            ..
        } => {
            if chats.len() > 1 {
                grpc("listen", "streaming more than one --chat requires gRPC")
            } else if space.is_none() {
                // REST SSE requires a space to scope the stream; a single-chat
                // listen without --space (e.g. `--chat <chat-id>` or a
                // space-name target) is served by the gRPC listener, matching
                // pre-transport behavior.
                grpc(
                    "listen",
                    "the REST SSE listener requires --space; listening without it uses gRPC",
                )
            } else if include_history.is_some() {
                grpc("listen", "--include-history requires the gRPC listener")
            } else if after.is_some() {
                grpc("listen", "--after requires the gRPC listener")
            } else if *previews {
                grpc("listen", "--previews requires the gRPC listener")
            } else if buffer.is_some() {
                grpc("listen", "--buffer requires the gRPC listener")
            } else {
                OpTransport {
                    name: "listen",
                    auto: ChatBackend::RestSse,
                    grpc_only: None,
                    rest_only: None,
                }
            }
        }
    }
}

/// Classifies a parsed `chat messages` subcommand into its transport policy;
/// split from [`classify`] to keep each classifier readable.
fn classify_messages(command: &super::ChatMessagesCommands) -> OpTransport {
    use super::ChatMessagesCommands as M;
    use OpTransport as T;

    match command {
        M::List { .. } => T::rest("messages list"),
        M::Get { .. } => T::rest("messages get"),
        M::Send { blocks_json, .. } => {
            if blocks_json.is_some() {
                T::grpc("messages send", "structured --blocks-json requires gRPC")
            } else {
                T::rest("messages send")
            }
        }
        M::Edit { blocks_json, .. } => {
            if blocks_json.is_some() {
                T::grpc("messages edit", "structured --blocks-json requires gRPC")
            } else {
                T::rest("messages edit")
            }
        }
        M::Delete { .. } => T::rest("messages delete"),
        M::Search { .. } => T::rest_only(
            "messages search",
            "chat message search is a REST-only operation",
        ),
        M::React { .. } => T::rest("messages react"),
    }
}

/// Resolves the requested transport against an operation's policy, returning an
/// actionable error when a transport is asked for an operation only the other
/// transport can serve.
fn resolve_transport(requested: TransportArg, op: &OpTransport) -> Result<ChatBackend> {
    match requested {
        TransportArg::Auto => Ok(op.auto),
        TransportArg::Grpc => match op.rest_only {
            None => Ok(ChatBackend::Grpc),
            Some(reason) => bail!(
                "`anyr chat {}` cannot use --transport grpc: {reason}. \
                 Use --transport rest or --transport auto.",
                op.name
            ),
        },
        TransportArg::Rest => match op.grpc_only {
            None => Ok(op.auto),
            Some(reason) => bail!(
                "`anyr chat {}` cannot use --transport rest: {reason}. \
                 Use --transport grpc or --transport auto.",
                op.name
            ),
        },
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

/// Resolves a CLI message argument into a message id.
///
/// Message ids are passed through unchanged. Anything else is treated as a
/// (possibly hex-encoded) chat `order_id`: the hex form the CLI prints and
/// accepts on the command line is decoded here before the
/// order-id-to-message-id lookup is delegated to the
/// shared [`AnytypeClient::resolve_message_id`] resolver.
async fn resolve_message_id_for_order(
    ctx: &AppContext,
    chat_id: &str,
    message_id_or_order_id: &str,
) -> Result<String> {
    if looks_like_object_id(message_id_or_order_id) {
        return Ok(message_id_or_order_id.to_string());
    }

    let order_id = decode_order_id_arg(message_id_or_order_id)?;
    Ok(ctx.client.resolve_message_id(chat_id, &order_id).await?)
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
    let name = ctx.client.resolve_chat_name(space_id, chat_id).await?;
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
    use clap::Parser;

    use super::*;
    use crate::cli::{Cli, Commands};

    /// Parses an `anyr chat ...` argv into its transport selector and command.
    fn parse_chat(args: &[&str]) -> (TransportArg, super::super::ChatCommands) {
        let cli = Cli::try_parse_from(args).expect("chat command parses");
        match cli.command {
            Commands::Chat(chat) => (chat.transport, *chat.command),
            other => panic!("expected chat command, got {other:?}"),
        }
    }

    /// Classifies and resolves a parsed `anyr chat ...` argv in one step.
    fn backend_of(args: &[&str]) -> Result<ChatBackend> {
        let (transport, command) = parse_chat(args);
        let op = classify(&command);
        resolve_transport(transport, &op)
    }

    #[test]
    fn transport_defaults_to_auto() {
        let (transport, _) = parse_chat(&["anyr", "chat", "list", "--space", "Work"]);
        assert_eq!(transport, TransportArg::Auto);
    }

    #[test]
    fn transport_parses_each_value() {
        for (value, expected) in [
            ("auto", TransportArg::Auto),
            ("rest", TransportArg::Rest),
            ("grpc", TransportArg::Grpc),
        ] {
            let (transport, _) = parse_chat(&[
                "anyr",
                "chat",
                "--transport",
                value,
                "list",
                "--space",
                "Work",
            ]);
            assert_eq!(transport, expected);
        }
    }

    #[test]
    fn transport_rejects_unknown_value() {
        let parsed = Cli::try_parse_from(["anyr", "chat", "--transport", "soap", "list"]);
        assert!(parsed.is_err());
    }

    #[test]
    fn auto_picks_rest_for_single_space_list() {
        assert_eq!(
            backend_of(&["anyr", "chat", "list", "--space", "Work"]).unwrap(),
            ChatBackend::Rest
        );
    }

    #[test]
    fn auto_picks_grpc_for_cross_space_list() {
        assert_eq!(
            backend_of(&["anyr", "chat", "list"]).unwrap(),
            ChatBackend::Grpc
        );
    }

    #[test]
    fn auto_picks_grpc_for_single_space_text_search() {
        assert_eq!(
            backend_of(&["anyr", "chat", "list", "--space", "Work", "--text", "hi"]).unwrap(),
            ChatBackend::Grpc
        );
    }

    #[test]
    fn auto_picks_rest_for_plain_message_crud() {
        for cmd in [
            vec!["anyr", "chat", "messages", "list", "Work", "Ops"],
            vec!["anyr", "chat", "messages", "get", "Work", "Ops", "m1"],
            vec!["anyr", "chat", "messages", "send", "Work", "Ops", "hi"],
            vec![
                "anyr", "chat", "messages", "edit", "Work", "Ops", "m1", "--text", "hi",
            ],
            vec!["anyr", "chat", "messages", "delete", "Work", "Ops", "m1"],
            vec!["anyr", "chat", "create", "Work", "Ops"],
            vec!["anyr", "chat", "read", "Work", "Ops"],
        ] {
            assert_eq!(
                backend_of(&cmd).unwrap(),
                ChatBackend::Rest,
                "expected REST for {cmd:?}"
            );
        }
    }

    #[test]
    fn auto_picks_rest_sse_for_single_chat_listen() {
        assert_eq!(
            backend_of(&["anyr", "chat", "listen", "--chat", "Ops", "--space", "Work"]).unwrap(),
            ChatBackend::RestSse
        );
    }

    #[test]
    fn auto_picks_grpc_for_multi_chat_listen() {
        assert_eq!(
            backend_of(&[
                "anyr", "chat", "listen", "--chat", "Ops", "--chat", "Dev", "--space", "Work",
            ])
            .unwrap(),
            ChatBackend::Grpc
        );
    }

    // These tests cover `resolve_transport`'s policy mapping (the backend it
    // returns for a given `--transport`/operation pair). The handlers
    // dispatch on that resolved backend (REST builders vs gRPC), so the mapping
    // is what selects the executed transport.
    #[test]
    fn grpc_resolves_to_grpc_backend_even_for_rest_capable_ops() {
        assert_eq!(
            backend_of(&[
                "anyr",
                "chat",
                "--transport",
                "grpc",
                "create",
                "Work",
                "Ops"
            ])
            .unwrap(),
            ChatBackend::Grpc
        );
    }

    #[test]
    fn rest_resolves_rest_capable_ops_to_their_policy_backend() {
        assert_eq!(
            backend_of(&[
                "anyr",
                "chat",
                "--transport",
                "rest",
                "list",
                "--space",
                "Work",
            ])
            .unwrap(),
            ChatBackend::Rest
        );
        assert_eq!(
            backend_of(&[
                "anyr",
                "chat",
                "--transport",
                "rest",
                "listen",
                "--chat",
                "Ops",
                "--space",
                "Work",
            ])
            .unwrap(),
            ChatBackend::RestSse
        );
    }

    #[test]
    fn rest_rejects_grpc_only_operations() {
        for cmd in [
            vec!["anyr", "chat", "--transport", "rest", "get", "Work", "Ops"],
            vec!["anyr", "chat", "--transport", "rest", "list"],
            vec![
                "anyr",
                "chat",
                "--transport",
                "rest",
                "list",
                "--space",
                "Work",
                "--text",
                "hi",
            ],
            vec![
                "anyr",
                "chat",
                "--transport",
                "rest",
                "unread",
                "Work",
                "Ops",
            ],
            vec![
                "anyr",
                "chat",
                "--transport",
                "rest",
                "listen",
                "--chat",
                "Ops",
                "--chat",
                "Dev",
                "--space",
                "Work",
            ],
        ] {
            let err = backend_of(&cmd).expect_err(&format!("expected rejection for {cmd:?}"));
            assert!(
                err.to_string().contains("--transport rest"),
                "error should mention --transport rest: {err}"
            );
        }
    }

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

    // any-8bk: list filters and create icon options.
    #[test]
    fn create_icon_options_are_mutually_exclusive() {
        let parsed = Cli::try_parse_from([
            "anyr",
            "chat",
            "create",
            "Work",
            "Ops",
            "--icon-emoji",
            "x",
            "--icon-file",
            "i.png",
        ]);
        assert!(
            parsed.is_err(),
            "--icon-emoji and --icon-file must conflict"
        );
    }

    #[test]
    fn create_accepts_a_single_icon_option() {
        for cmd in [
            vec!["anyr", "chat", "create", "Work", "Ops", "--icon-emoji", "x"],
            vec![
                "anyr",
                "chat",
                "create",
                "Work",
                "Ops",
                "--icon-file",
                "i.png",
            ],
        ] {
            assert!(Cli::try_parse_from(&cmd).is_ok(), "should parse: {cmd:?}");
        }
    }

    #[test]
    fn space_scoped_list_with_filter_is_rest() {
        assert_eq!(
            backend_of(&[
                "anyr",
                "chat",
                "list",
                "--space",
                "Work",
                "--filter",
                "name==Ops"
            ])
            .unwrap(),
            ChatBackend::Rest
        );
    }

    #[test]
    fn create_with_icon_stays_rest_under_auto() {
        assert_eq!(
            backend_of(&["anyr", "chat", "create", "Work", "Ops", "--icon-emoji", "x"]).unwrap(),
            ChatBackend::Rest
        );
    }

    // any-amp: reply-to, blocks-json, and edit attachment replacement.
    #[test]
    fn send_and_edit_blocks_json_forces_grpc() {
        assert_eq!(
            backend_of(&[
                "anyr",
                "chat",
                "messages",
                "send",
                "Work",
                "Ops",
                "--blocks-json",
                "@b.json",
                "--text",
                "hi",
            ])
            .unwrap(),
            ChatBackend::Grpc
        );
        assert_eq!(
            backend_of(&[
                "anyr",
                "chat",
                "messages",
                "edit",
                "Work",
                "Ops",
                "m1",
                "--blocks-json",
                "@b.json",
                "--text",
                "hi",
            ])
            .unwrap(),
            ChatBackend::Grpc
        );
    }

    #[test]
    fn blocks_json_rejected_with_transport_rest() {
        for cmd in [
            vec![
                "anyr",
                "chat",
                "--transport",
                "rest",
                "messages",
                "send",
                "Work",
                "Ops",
                "--blocks-json",
                "@b.json",
                "--text",
                "hi",
            ],
            vec![
                "anyr",
                "chat",
                "--transport",
                "rest",
                "messages",
                "edit",
                "Work",
                "Ops",
                "m1",
                "--blocks-json",
                "@b.json",
                "--text",
                "hi",
            ],
        ] {
            let err = backend_of(&cmd).expect_err(&format!("expected rejection for {cmd:?}"));
            assert!(
                err.to_string().contains("--transport rest"),
                "error should mention --transport rest: {err}"
            );
        }
    }

    #[test]
    fn plain_send_with_reply_to_is_rest() {
        assert_eq!(
            backend_of(&[
                "anyr",
                "chat",
                "messages",
                "send",
                "Work",
                "Ops",
                "--reply-to",
                "m0",
                "--text",
                "hi",
            ])
            .unwrap(),
            ChatBackend::Rest
        );
    }

    #[test]
    fn edit_accepts_replacement_attachments() {
        assert!(
            Cli::try_parse_from([
                "anyr",
                "chat",
                "messages",
                "edit",
                "Work",
                "Ops",
                "m1",
                "--text",
                "hi",
                "--attachment",
                "file:obj1",
                "--attachment",
                "image:obj2",
            ])
            .is_ok()
        );
    }

    // any-nth: search (REST-only) and react (both transports).
    #[test]
    fn search_is_rest_and_rejects_grpc() {
        assert_eq!(
            backend_of(&["anyr", "chat", "messages", "search", "Work", "Ops", "hello"]).unwrap(),
            ChatBackend::Rest
        );
        let err = backend_of(&[
            "anyr",
            "chat",
            "--transport",
            "grpc",
            "messages",
            "search",
            "Work",
            "Ops",
            "hello",
        ])
        .expect_err("grpc search should reject");
        assert!(
            err.to_string().contains("--transport grpc"),
            "error should mention --transport grpc: {err}"
        );
    }

    #[test]
    fn react_defaults_rest_and_allows_grpc() {
        assert_eq!(
            backend_of(&[
                "anyr", "chat", "messages", "react", "Work", "Ops", "m1", "x"
            ])
            .unwrap(),
            ChatBackend::Rest
        );
        assert_eq!(
            backend_of(&[
                "anyr",
                "chat",
                "--transport",
                "grpc",
                "messages",
                "react",
                "Work",
                "Ops",
                "m1",
                "x",
            ])
            .unwrap(),
            ChatBackend::Grpc
        );
    }

    // any-9nx: read-state REST ops; unread stays gRPC-only.
    #[test]
    fn read_state_ops_are_rest() {
        for cmd in [
            vec!["anyr", "chat", "read-reactions", "Work", "Ops"],
            vec![
                "anyr",
                "chat",
                "read-reactions",
                "Work",
                "Ops",
                "--order-id",
                "abc",
            ],
            vec!["anyr", "chat", "read-all", "Work", "Ops"],
        ] {
            assert_eq!(backend_of(&cmd).unwrap(), ChatBackend::Rest, "{cmd:?}");
        }
    }

    #[test]
    fn read_all_allowed_under_rest_unread_is_not() {
        assert_eq!(
            backend_of(&[
                "anyr",
                "chat",
                "--transport",
                "rest",
                "read-all",
                "Work",
                "Ops"
            ])
            .unwrap(),
            ChatBackend::Rest
        );
        let err = backend_of(&[
            "anyr",
            "chat",
            "--transport",
            "rest",
            "unread",
            "Work",
            "Ops",
        ])
        .expect_err("unread over rest should reject");
        assert!(
            err.to_string().contains("--transport rest"),
            "error should mention --transport rest: {err}"
        );
    }

    // any-rcr: listen backends and flags.
    #[test]
    fn listen_previews_or_buffer_forces_grpc() {
        assert_eq!(
            backend_of(&[
                "anyr",
                "chat",
                "listen",
                "--chat",
                "Ops",
                "--space",
                "Work",
                "--previews",
            ])
            .unwrap(),
            ChatBackend::Grpc
        );
        assert_eq!(
            backend_of(&[
                "anyr", "chat", "listen", "--chat", "Ops", "--space", "Work", "--buffer", "32",
            ])
            .unwrap(),
            ChatBackend::Grpc
        );
    }

    #[test]
    fn listen_rest_sse_accepts_initial_limit_and_heartbeat() {
        assert_eq!(
            backend_of(&[
                "anyr",
                "chat",
                "listen",
                "--chat",
                "Ops",
                "--space",
                "Work",
                "--initial-limit",
                "5",
                "--heartbeat",
                "30",
            ])
            .unwrap(),
            ChatBackend::RestSse
        );
    }

    #[test]
    fn listen_previews_rejected_with_transport_rest() {
        let err = backend_of(&[
            "anyr",
            "chat",
            "--transport",
            "rest",
            "listen",
            "--chat",
            "Ops",
            "--space",
            "Work",
            "--previews",
        ])
        .expect_err("previews over rest should reject");
        assert!(
            err.to_string().contains("--transport rest"),
            "error should mention --transport rest: {err}"
        );
    }

    #[test]
    fn resolve_message_text_precedence() {
        assert_eq!(
            resolve_message_text(Some("explicit".into()), None, &[]).unwrap(),
            Some("explicit".into())
        );
        assert_eq!(
            resolve_message_text(None, None, &["a".into(), "b".into()]).unwrap(),
            Some("a b".into())
        );
        assert_eq!(resolve_message_text(None, None, &[]).unwrap(), None);
    }

    #[test]
    fn parse_message_blocks_json_round_trips() {
        let blocks = vec![MessageBlock::Text(MessageBlockText::default())];
        let json = serde_json::to_string(&blocks).expect("serialize blocks");
        let path = std::env::temp_dir().join(format!("anyr_blocks_{}.json", std::process::id()));
        std::fs::write(&path, &json).expect("write temp blocks");
        let parsed =
            parse_message_blocks_json(&format!("@{}", path.display())).expect("parse blocks json");
        assert_eq!(parsed.len(), 1);
        std::fs::remove_file(&path).ok();
    }
}
