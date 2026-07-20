mod common;

use std::net::SocketAddr;

use anyhow::Result;
use anytype::{
    prelude::*,
    test_util::{unique_suffix, with_test_context},
};
use chrono::Utc;
use common::retry_definitive_rate_limit;
use futures::StreamExt;
use tokio::{
    net::TcpStream,
    time::{Duration, sleep, timeout},
};

async fn setup_mock_client() -> Result<(AnytypeClient, anytype::mock::MockChatServerHandle)> {
    let temp_path = std::env::temp_dir().join(format!(
        "anytype_chat_stream_test_{}.db",
        Utc::now().timestamp_nanos_opt().unwrap_or_default()
    ));
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    let addr = listener.local_addr()?;
    drop(listener);

    let handle = anytype::mock::MockChatServer::start(addr)?;
    wait_for_server(addr).await?;

    let mut config = ClientConfig::default().app_name("anytype-chat-stream-test");
    config.keystore = Some(format!("file:path={}", temp_path.display()));
    config.keystore_service = Some("anyr".to_string());
    config.grpc_endpoint = Some(format!("http://{}", addr));

    let client = AnytypeClient::with_config(config)?;
    let keystore = client.get_key_store();
    keystore.update_grpc_credentials(&GrpcCredentials::from_token("token-alice"))?;

    Ok((client, handle))
}

#[tokio::test]
#[serial_test::serial(chat_stream)]
async fn chat_stream_receives_messages() -> Result<()> {
    with_test_context(|ctx| async move {
        let chat_name = format!("chat-stream-{}", unique_suffix());
        let chat = retry_definitive_rate_limit("chat stream setup chat", || async {
            ctx.client
                .chats()
                .in_space(&ctx.space_id)
                .create(
                    &chat_name,
                    Icon::Emoji {
                        emoji: "📡".to_string(),
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
            .send_text(&chat.id, "hello from the real server")
            .send()
            .await?;
        // Subscribe after publishing so the real server's initial message
        // snapshot deterministically verifies the gRPC stream conversion.
        let ChatStreamHandle { mut events, .. } =
            ctx.client.chat_stream().subscribe_chat(&chat.id).build();

        let event = timeout(
            Duration::from_secs(10),
            wait_for_event(&mut events, |event| {
                matches!(event, ChatEvent::MessageAdded { .. })
            }),
        )
        .await
        .expect("real chat stream event timed out")
        .expect("real chat stream ended");

        match event {
            ChatEvent::MessageAdded { chat_id, message } => {
                assert_eq!(chat_id, chat.id);
                assert_eq!(message.id, message_id);
            }
            other => panic!("expected MessageAdded event, got {other:?}"),
        }

        ctx.client
            .chats()
            .in_space(&ctx.space_id)
            .delete_message(&chat.id, &message_id)
            .await?;
        Ok(())
    })
    .await?;
    Ok(())
}

#[tokio::test]
#[serial_test::serial(chat_stream)]
async fn rest_chat_stream_receives_initial_message() -> Result<()> {
    with_test_context(|ctx| async move {
        let chats = ctx.client.chats().in_space(&ctx.space_id);
        let chat_name = format!("rest-chat-stream-{}", unique_suffix());
        let chat = retry_definitive_rate_limit("REST SSE setup chat", || async {
            chats
                .create(
                    &chat_name,
                    Icon::Emoji {
                        emoji: "📨".to_string(),
                    },
                )
                .create()
                .await
        })
        .await?;
        ctx.register_object(&chat.id);
        let message_id = retry_definitive_rate_limit("REST SSE setup message", || async {
            chats
                .add_message(&chat.id, MessageContent::new().text("hello from REST SSE"))
                .send()
                .await
        })
        .await?;

        let mut events = chats
            .message_stream(&chat.id)
            .limit(1)
            .heartbeat_seconds(1)
            .open()
            .await?;
        let event = timeout(Duration::from_secs(10), events.next())
            .await
            .expect("REST chat stream event timed out")
            .expect("REST chat stream ended")?;
        assert!(matches!(
            event,
            ChatHttpEvent::MessageAdded { message } if message.id == message_id
        ));

        chats.delete_message(&chat.id, &message_id).await?;
        Ok(())
    })
    .await?;
    Ok(())
}

#[tokio::test]
#[serial_test::serial(chat_stream)]
async fn chat_stream_reconnects_after_disconnect() -> Result<()> {
    let (client, handle) = setup_mock_client().await?;
    let chat_id = "chat-default";

    let backoff = BackoffPolicy {
        initial: Duration::from_millis(25),
        max: Duration::from_millis(100),
        factor: 1.5,
    };

    let ChatStreamHandle { mut events, .. } = client
        .chat_stream()
        .subscribe_chat(chat_id)
        .backoff(backoff)
        .build();

    let _ = client
        .chats()
        .add_message(chat_id)
        .content(MessageContent {
            text: "initial".to_string(),
            style: MessageTextStyle::Paragraph,
            marks: Vec::new(),
        })
        .send()
        .await?;

    let _ = timeout(
        Duration::from_secs(2),
        wait_for_event(&mut events, |event| {
            matches!(event, ChatEvent::MessageAdded { .. })
        }),
    )
    .await??;
    handle.disconnect_streams().await;
    let _ = timeout(
        Duration::from_secs(2),
        wait_for_event(&mut events, |event| {
            matches!(event, ChatEvent::StreamDisconnected)
        }),
    )
    .await??;
    let message_id = client
        .chats()
        .add_message(chat_id)
        .content(MessageContent {
            text: "after disconnect".to_string(),
            style: MessageTextStyle::Paragraph,
            marks: Vec::new(),
        })
        .send()
        .await?;

    let _ = timeout(
        Duration::from_secs(2),
        wait_for_event(&mut events, |event| {
            matches!(event, ChatEvent::StreamResubscribed)
        }),
    )
    .await??;

    let event = timeout(
        Duration::from_secs(2),
        wait_for_event(&mut events, |event| {
            matches!(event, ChatEvent::MessageAdded { .. })
        }),
    )
    .await??;

    if let ChatEvent::MessageAdded { message, .. } = event {
        assert_eq!(message.id, message_id);
    } else {
        anyhow::bail!("expected MessageAdded after reconnect");
    }

    handle.shutdown().await;
    Ok(())
}

async fn wait_for_event<F>(events: &mut ChatEventStream, predicate: F) -> Result<ChatEvent>
where
    F: Fn(&ChatEvent) -> bool,
{
    loop {
        if let Some(event) = events.next().await {
            if predicate(&event) {
                return Ok(event);
            }
        } else {
            anyhow::bail!("event stream ended");
        }
    }
}

async fn wait_for_server(addr: SocketAddr) -> Result<()> {
    for _ in 0..20 {
        if TcpStream::connect(addr).await.is_ok() {
            return Ok(());
        }
        sleep(Duration::from_millis(50)).await;
    }
    anyhow::bail!("mock server failed to start on {addr}");
}
