use anytype::{
    prelude::*,
    test_util::{TestResult, unique_suffix, with_test_context},
};

#[tokio::test]
async fn test_chat_message_crud() -> TestResult<()> {
    with_test_context(|ctx| async move {
        let chat = ctx
            .client
            .chats()
            .in_space(&ctx.space_id)
            .create(
                format!("chat-crud-{}", unique_suffix()),
                Icon::Emoji {
                    emoji: "🧪".to_string(),
                },
            )
            .create()
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
        let chat = chats
            .create(
                format!("rest-chat-crud-{}", unique_suffix()),
                Icon::Emoji {
                    emoji: "🌐".to_string(),
                },
            )
            .create()
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
