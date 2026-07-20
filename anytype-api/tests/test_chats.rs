mod common;

use anytype::{
    prelude::*,
    test_util::{TestResult, unique_suffix, with_test_context},
};
use common::retry_definitive_rate_limit;

#[tokio::test]
async fn test_chat_message_crud() -> TestResult<()> {
    with_test_context(|ctx| async move {
        let chat_name = format!("chat-crud-{}", unique_suffix());
        let chat = retry_definitive_rate_limit("chat CRUD setup chat", || async {
            ctx.client
                .chats()
                .in_space(&ctx.space_id)
                .create(
                    &chat_name,
                    Icon::Emoji {
                        emoji: "🧪".to_string(),
                    },
                )
                .create()
                .await
        })
        .await?;
        ctx.register_object(&chat.id);

        let message_id = ctx
            .client
            .chats()
            .add_message(&chat.id)
            .content(MessageContent {
                text: "hello".to_string(),
                style: MessageTextStyle::Paragraph,
                marks: Vec::new(),
            })
            .send()
            .await?;

        let page = ctx
            .client
            .chats()
            .list_messages(&chat.id)
            .list_page()
            .await?;
        assert!(page.messages.iter().any(|msg| msg.id == message_id));

        ctx.client
            .chats()
            .edit_message(&chat.id, &message_id)
            .content(MessageContent::new().italic("updated"))
            .send()
            .await?;

        let messages = ctx
            .client
            .chats()
            .get_messages(&chat.id, [&message_id])
            .get()
            .await?;
        assert_eq!(messages[0].content.text, "updated");
        assert!(matches!(
            messages[0].content.marks[0].kind,
            MessageTextMarkType::Italic
        ));

        ctx.client
            .chats()
            .read_messages(&chat.id)
            .read_type(ChatReadType::Messages)
            .mark_read()
            .await?;

        ctx.client
            .chats()
            .unread_messages(&chat.id)
            .read_type(ChatReadType::Messages)
            .after("0000000000000000")
            .mark_unread()
            .await?;

        ctx.client
            .chats()
            .in_space(&ctx.space_id)
            .delete_message(&chat.id, &message_id)
            .await?;
        Ok(())
    })
    .await
}

#[tokio::test]
async fn test_rest_chat_message_crud() -> TestResult<()> {
    with_test_context(|ctx| async move {
        let chats = ctx.client.chats().in_space(&ctx.space_id);
        let chat_name = format!("rest-chat-crud-{}", unique_suffix());
        let chat = retry_definitive_rate_limit("REST chat CRUD setup chat", || async {
            chats
                .create(
                    &chat_name,
                    Icon::Emoji {
                        emoji: "🌐".to_string(),
                    },
                )
                .create()
                .await
        })
        .await?;
        ctx.register_object(&chat.id);

        let message_id = chats
            .add_message(&chat.id, MessageContent::new().bold("hello over REST"))
            .send()
            .await?;
        let created = chats.get_message(&chat.id, &message_id).get().await?;
        assert_eq!(created.content.text, "hello over REST");
        assert!(matches!(
            created.content.marks[0].kind,
            MessageTextMarkType::Bold
        ));

        chats
            .edit_message(
                &chat.id,
                &message_id,
                MessageContent::new().italic("updated over REST"),
            )
            .send()
            .await?;
        let updated = chats.get_message(&chat.id, &message_id).get().await?;
        assert_eq!(updated.content.text, "updated over REST");
        assert!(matches!(
            updated.content.marks[0].kind,
            MessageTextMarkType::Italic
        ));

        chats.delete_message(&chat.id, &message_id).await?;
        Ok(())
    })
    .await
}
