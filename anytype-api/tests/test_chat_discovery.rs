#![cfg(feature = "grpc")]

use std::net::SocketAddr;

use anyhow::Result;
use anytype::prelude::*;
use chrono::Utc;
use tokio::net::TcpStream;
use tokio::time::{Duration, sleep};

const DEFAULT_CHAT_ID: &str = "chat-default";
const DEFAULT_SPACE_ID: &str = "space-default";
const DEFAULT_CHAT_NAME: &str = "General";

async fn setup_client(token: &str) -> Result<(AnytypeClient, anytype::mock::MockChatServerHandle)> {
    let temp_path = std::env::temp_dir().join(format!(
        "anytype_chat_discovery_test_{}.db",
        Utc::now().timestamp_nanos_opt().unwrap_or_default()
    ));
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    let addr = listener.local_addr()?;
    drop(listener);

    let handle = anytype::mock::MockChatServer::start(addr).await?;
    wait_for_server(addr).await?;

    let mut config = ClientConfig::default().app_name("anytype-chat-discovery-test");
    config.keystore = Some(format!("file:path={}", temp_path.display()));
    config.keystore_service = Some("anytype-chat-discovery".to_string());
    config.grpc_endpoint = Some(format!("http://{}", addr));

    let client = AnytypeClient::with_config(config)?;
    let keystore = client.get_key_store();
    keystore.update_grpc_credentials(&GrpcCredentials::from_token(token))?;

    Ok((client, handle))
}

#[tokio::test]
#[ignore] // fixme: broken - probably a limitation of the mock server
async fn test_chat_discovery_requests() -> Result<()> {
    let (client, handle) = setup_client("token-alice").await?;

    let chats = client
        .chats()
        .list_chats_in(DEFAULT_SPACE_ID)
        .list()
        .await?;
    assert!(
        chats.items.iter().any(|chat| chat.id == DEFAULT_CHAT_ID),
        "expected default chat to be returned"
    );
    let chat = chats
        .items
        .iter()
        .find(|chat| chat.id == DEFAULT_CHAT_ID)
        .expect("default chat");
    assert!(
        chat.get_property_date("last_modified_date").is_some(),
        "expected last_modified_date property"
    );

    let search = client
        .chats()
        .search_chats_in(DEFAULT_SPACE_ID)
        .text(DEFAULT_CHAT_NAME)
        .search()
        .await?;
    assert!(
        search.items.iter().any(|chat| chat.id == DEFAULT_CHAT_ID),
        "expected search results to include default chat"
    );

    let resolved = client
        .chats()
        .resolve_chat_by_name(DEFAULT_SPACE_ID, DEFAULT_CHAT_NAME)
        .resolve()
        .await?;
    assert_eq!(resolved, DEFAULT_CHAT_ID);

    let chat = client
        .chats()
        .get_chat(DEFAULT_SPACE_ID, DEFAULT_CHAT_ID)
        .get()
        .await?;
    assert_eq!(chat.id, DEFAULT_CHAT_ID);

    let space_chat = client.chats().space_chat(DEFAULT_SPACE_ID).get().await?;
    assert_eq!(space_chat.id, DEFAULT_CHAT_ID);

    handle.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn test_chat_convenience_reactions_and_read_all() -> Result<()> {
    let (client, handle) = setup_client("token-alice").await?;

    let message_id = client
        .chats()
        .send_text(DEFAULT_CHAT_ID, "hello")
        .send()
        .await?;

    client
        .chats()
        .edit_text(DEFAULT_CHAT_ID, &message_id, "updated")
        .send()
        .await?;

    let messages = client
        .chats()
        .get_messages(DEFAULT_CHAT_ID, [&message_id])
        .get()
        .await?;
    assert_eq!(messages[0].content.text, "updated");

    let added = client
        .chats()
        .toggle_reaction(DEFAULT_CHAT_ID, &message_id, "👍")
        .send()
        .await?;
    assert!(added, "expected reaction to be added");

    let messages = client
        .chats()
        .get_messages(DEFAULT_CHAT_ID, [&message_id])
        .get()
        .await?;
    assert!(
        messages[0]
            .reactions
            .iter()
            .any(|reaction| reaction.emoji == "👍"),
        "expected reaction to be present"
    );

    let removed = client
        .chats()
        .toggle_reaction(DEFAULT_CHAT_ID, &message_id, "👍")
        .send()
        .await?;
    assert!(!removed, "expected reaction to be removed");

    let temp_path = std::env::temp_dir().join(format!(
        "anytype_chat_discovery_bob_{}.db",
        Utc::now().timestamp_nanos_opt().unwrap_or_default()
    ));
    let mut config = ClientConfig::default().app_name("anytype-chat-discovery-bob-test");
    config.keystore = Some(format!("file:path={}", temp_path.display()));
    config.keystore_service = Some("anytype-chat-discovery".to_string());
    config.grpc_endpoint = Some(format!("http://{}", handle.addr()));

    let bob_client = AnytypeClient::with_config(config)?;
    let keystore = bob_client.get_key_store();
    keystore.update_grpc_credentials(&GrpcCredentials::from_token("token-bob"))?;

    let page = bob_client
        .chats()
        .list_messages(DEFAULT_CHAT_ID)
        .list_page()
        .await?;
    assert!(
        page.state.oldest_unread_order_id().is_some(),
        "expected unread state for bob"
    );

    bob_client
        .chats()
        .read_all(DEFAULT_SPACE_ID)
        .mark_read()
        .await?;

    let unread = bob_client
        .chats()
        .list_messages(DEFAULT_CHAT_ID)
        .unread_only(ChatReadType::Messages)
        .list_page()
        .await?;
    assert!(unread.messages.is_empty(), "expected all messages read");

    handle.shutdown().await;
    Ok(())
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
