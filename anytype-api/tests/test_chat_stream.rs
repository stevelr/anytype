#![cfg(feature = "grpc")]

use std::net::SocketAddr;

use anyhow::Result;
use anytype::prelude::*;
use chrono::Utc;
use futures::StreamExt;
use tokio::{
    net::TcpStream,
    time::{Duration, sleep, timeout},
};

async fn setup_client() -> Result<(AnytypeClient, anytype::mock::MockChatServerHandle)> {
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
async fn chat_stream_receives_messages() -> Result<()> {
    let (client, handle) = setup_client().await?;
    let chat_id = "chat-default";

    let ChatStreamHandle { mut events, .. } = client.chat_stream().subscribe_chat(chat_id).build();
    let message_id = client
        .chats()
        .add_message(chat_id)
        .content(MessageContent {
            text: "hello".to_string(),
            style: MessageTextStyle::Paragraph,
            marks: Vec::new(),
        })
        .send()
        .await?;

    let event = timeout(
        Duration::from_secs(2),
        wait_for_event(&mut events, |event| {
            matches!(event, ChatEvent::MessageAdded { .. })
        }),
    )
    .await??;

    match event {
        ChatEvent::MessageAdded { chat_id, message } => {
            assert_eq!(chat_id, "chat-default");
            assert_eq!(message.id, message_id);
        }
        other => {
            anyhow::bail!("expected MessageAdded event, got {other:?}");
        }
    }

    handle.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn chat_stream_reconnects_after_disconnect() -> Result<()> {
    let (client, handle) = setup_client().await?;
    let chat_id = "chat-default";

    let backoff = BackoffPolicy {
        initial: Duration::from_millis(25),
        max: Duration::from_millis(100),
        factor: 1.5,
    };

    eprintln!("aa");
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

    eprintln!("bb");
    let _ = timeout(
        Duration::from_secs(2),
        wait_for_event(&mut events, |event| {
            matches!(event, ChatEvent::MessageAdded { .. })
        }),
    )
    .await??;
    eprintln!("b2");

    handle.disconnect_streams().await;
    eprintln!("b3");
    let _ = timeout(
        Duration::from_secs(2),
        wait_for_event(&mut events, |event| {
            matches!(event, ChatEvent::StreamDisconnected)
        }),
    )
    .await??;
    eprintln!("b4");

    eprintln!("cc");
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

    eprintln!("dd");
    let event = timeout(
        Duration::from_secs(2),
        wait_for_event(&mut events, |event| {
            matches!(event, ChatEvent::MessageAdded { .. })
        }),
    )
    .await??;

    eprintln!("ee");
    if let ChatEvent::MessageAdded { message, .. } = event {
        assert_eq!(message.id, message_id);
    } else {
        anyhow::bail!("expected MessageAdded after reconnect");
    }

    eprintln!("ff");
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
