#![cfg(feature = "grpc")]

use std::net::SocketAddr;

use anytype::prelude::*;
use chrono::Utc;
use tokio::net::TcpStream;
use tokio::time::{Duration, sleep};

#[tokio::test]
async fn test_chat_message_crud() -> anyhow::Result<()> {
    let temp_path = std::env::temp_dir().join(format!(
        "anytype_chat_test_{}.db",
        Utc::now().timestamp_nanos_opt().unwrap_or_default()
    ));
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    let addr = listener.local_addr()?;
    drop(listener);

    let handle = anytype::mock::MockChatServer::start(addr).await?;
    wait_for_server(addr).await?;

    let mut config = ClientConfig::default().app_name("anytype-chat-test");
    config.keystore = Some(format!("file:path={}", temp_path.display()));
    config.keystore_service = Some("anytype-chat-test".to_string());
    config.grpc_endpoint = Some(format!("http://{}", addr));

    let client = AnytypeClient::with_config(config)?;
    let keystore = client.get_key_store();
    keystore.update_grpc_credentials(&GrpcCredentials::from_token("token-alice"))?;

    let chat_id = "chat-default";
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

    let page = client.chats().list_messages(chat_id).list_page().await?;
    assert!(page.messages.iter().any(|msg| msg.id == message_id));

    client
        .chats()
        .edit_message(chat_id, &message_id)
        .content(MessageContent {
            text: "updated".to_string(),
            style: MessageTextStyle::Paragraph,
            marks: Vec::new(),
        })
        .send()
        .await?;

    let messages = client
        .chats()
        .get_messages(chat_id, [&message_id])
        .get()
        .await?;
    assert_eq!(messages[0].content.text, "updated");

    client
        .chats()
        .read_messages(chat_id)
        .read_type(ChatReadType::Messages)
        .mark_read()
        .await?;

    let page = client
        .chats()
        .list_messages(chat_id)
        .unread_only(ChatReadType::Messages)
        .list_page()
        .await?;
    assert!(
        page.messages.is_empty(),
        "expected no unread messages after mark_read"
    );

    client
        .chats()
        .unread_messages(chat_id)
        .read_type(ChatReadType::Messages)
        .after("0000000000000000")
        .mark_unread()
        .await?;

    let page = client
        .chats()
        .list_messages(chat_id)
        .unread_only(ChatReadType::Messages)
        .list_page()
        .await?;
    assert!(
        page.messages.iter().any(|msg| msg.id == message_id),
        "expected message to be marked unread"
    );

    client
        .chats()
        .delete_message(chat_id, &message_id)
        .delete()
        .await?;

    handle.shutdown().await;
    Ok(())
}

async fn wait_for_server(addr: SocketAddr) -> anyhow::Result<()> {
    for _ in 0..20 {
        if TcpStream::connect(addr).await.is_ok() {
            return Ok(());
        }
        sleep(Duration::from_millis(50)).await;
    }
    anyhow::bail!("mock server failed to start on {addr}");
}
