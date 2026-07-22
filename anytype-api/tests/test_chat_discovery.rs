mod common;

use std::future::Future;

use anytype::{
    prelude::*,
    test_util::{TestError, TestResult, unique_suffix, with_test_context},
};
use common::retry_definitive_rate_limit;
use tokio::time::{Duration, sleep, timeout};

const LIVE_OPERATION_TIMEOUT: Duration = Duration::from_secs(20);

async fn bounded_api<T>(
    operation: &'static str,
    future: impl Future<Output = Result<T, AnytypeError>>,
) -> TestResult<T> {
    timeout(LIVE_OPERATION_TIMEOUT, future)
        .await
        .map_err(|_| TestError::Assertion {
            message: format!("{operation} exceeded its fixed live-test timeout"),
        })?
        .map_err(Into::into)
}

#[tokio::test]
async fn test_chat_discovery_requests() -> TestResult<()> {
    with_test_context(|ctx| async move {
        let name = format!("chat-discovery-{}", unique_suffix());
        let chat = retry_definitive_rate_limit("chat discovery setup chat", || async {
            ctx.client
                .chats()
                .in_space(&ctx.space_id)
                .create(
                    &name,
                    Icon::Emoji {
                        emoji: "🔎".to_string(),
                    },
                )
                .create()
                .await
        })
        .await?;
        ctx.register_object(&chat.id);

        let chats = bounded_api(
            "REST chat inventory",
            ctx.client.chats().list_chats_in(&ctx.space_id).list(),
        )
        .await?;
        assert!(
            chats.items.iter().any(|item| item.id == chat.id),
            "REST chat listing should include the created chat"
        );

        let search = bounded_api(
            "gRPC chat search",
            ctx.client
                .chats()
                .search_chats_in(&ctx.space_id)
                .text(&name)
                .search(),
        )
        .await?;
        assert!(
            search.items.iter().any(|item| item.id == chat.id),
            "gRPC chat-object search should include the created chat"
        );

        let resolved = ctx
            .client
            .chats()
            .resolve_chat_by_name(&ctx.space_id, &name)
            .resolve()
            .await?;
        assert_eq!(resolved, chat.id);

        let scoped_target = bounded_api(
            "space-scoped public chat resolution",
            ctx.client.resolve_chat_target(Some(&ctx.space_id), &name),
        )
        .await?;
        assert_eq!(scoped_target.chat_id, chat.id);
        assert_eq!(
            scoped_target.space_id.as_deref(),
            Some(ctx.space_id.as_str())
        );

        let direct_target = bounded_api(
            "bare public chat-id resolution",
            ctx.client.resolve_chat_target(None, &chat.id),
        )
        .await?;
        assert_eq!(direct_target.chat_id, chat.id);
        assert_eq!(direct_target.space_id, None);

        let fetched = ctx
            .client
            .chats()
            .get_chat(&ctx.space_id, &chat.id)
            .get()
            .await?;
        assert_eq!(fetched.id, chat.id);
        Ok(())
    })
    .await
}

#[tokio::test]
async fn test_rest_chat_messages_reactions_search_and_reads() -> TestResult<()> {
    with_test_context(|ctx| async move {
        let name = format!("chat-rest-{}", unique_suffix());
        let chat = retry_definitive_rate_limit("REST chat workflow setup chat", || async {
            ctx.client
                .chats()
                .in_space(&ctx.space_id)
                .create(
                    &name,
                    Icon::Emoji {
                        emoji: "💬".to_string(),
                    },
                )
                .create()
                .await
        })
        .await?;
        ctx.register_object(&chat.id);

        // Publishing remains gRPC so structured blocks are not discarded.
        let message_id = bounded_api(
            "gRPC message creation",
            ctx.client
                .chats()
                .add_message(&chat.id)
                .content(MessageContent::new().bold("migration coverage"))
                .blocks(vec![MessageBlock::Text(MessageBlockText {
                    text: "structured heading".to_string(),
                    style: MessageTextStyle::Header2,
                    ..MessageBlockText::default()
                })])
                .send(),
        )
        .await?;
        ctx.register_chat_message(&chat.id, &message_id)?;

        let rich = ctx
            .client
            .chats()
            .get_messages(&chat.id, [&message_id])
            .get()
            .await?;
        assert_eq!(rich.len(), 1);
        assert_eq!(rich[0].content.text, "migration coverage");
        assert!(!rich[0].content.marks.is_empty());
        assert!(!rich[0].blocks.is_empty());

        let chats = ctx.client.chats().in_space(&ctx.space_id);
        let plain = chats.get_message(&chat.id, &message_id).get().await?;
        assert_eq!(plain.content.text, "migration coverage");
        assert!(plain.blocks.is_empty(), "REST does not expose blocks");

        let listed = chats.list_messages(&chat.id).limit(20).list().await?;
        assert!(listed.iter().any(|message| message.id == message_id));

        let rich_page = bounded_api(
            "gRPC message order inventory",
            ctx.client
                .chats()
                .list_messages(&chat.id)
                .limit(20)
                .list_page(),
        )
        .await?;
        let order_id = rich_page
            .messages
            .iter()
            .find(|message| message.id == message_id)
            .map(|message| message.order_id.clone())
            .ok_or_else(|| TestError::Assertion {
                message: "created message was absent from the bounded gRPC page".to_owned(),
            })?;
        assert_ne!(order_id, message_id);

        let by_order = bounded_api(
            "message order-id resolution",
            ctx.client.resolve_message_id(&chat.id, &order_id),
        )
        .await?;
        assert_eq!(by_order, message_id);
        let direct = bounded_api(
            "direct message-id pass-through",
            ctx.client.resolve_message_id(&chat.id, &message_id),
        )
        .await?;
        assert_eq!(direct, message_id);
        let batch = bounded_api(
            "mixed message-id resolution",
            ctx.client
                .resolve_message_ids(&chat.id, &[order_id, message_id.clone()]),
        )
        .await?;
        assert_eq!(batch, vec![message_id.clone(), message_id.clone()]);

        let unknown_order_id = format!("unknown-order-{}", unique_suffix());
        let unknown_result = timeout(
            LIVE_OPERATION_TIMEOUT,
            ctx.client.resolve_message_id(&chat.id, &unknown_order_id),
        )
        .await
        .map_err(|_| TestError::Assertion {
            message: "unknown message order-id resolution exceeded its fixed live-test timeout"
                .to_owned(),
        })?;
        assert!(matches!(unknown_result, Err(AnytypeError::NotFound { .. })));

        let mut search_found = false;
        for _ in 0..40 {
            let matches = chats
                .search_messages(&chat.id, "migration coverage")
                .limit(20)
                .search()
                .await?;
            if matches
                .items
                .iter()
                .any(|result| result.message.id == message_id)
            {
                search_found = true;
                break;
            }
            sleep(Duration::from_millis(250)).await;
        }
        assert!(search_found, "message did not become searchable");

        chats.toggle_reaction(&chat.id, &message_id, "👍").await?;
        let reacted = chats.get_message(&chat.id, &message_id).get().await?;
        assert!(
            reacted
                .reactions
                .iter()
                .any(|reaction| reaction.emoji == "👍")
        );

        chats.read_messages(&chat.id).mark_read().await?;
        chats.read_reactions(&chat.id).mark_read().await?;
        chats.read_all(&chat.id).await?;
        Ok(())
    })
    .await
}
