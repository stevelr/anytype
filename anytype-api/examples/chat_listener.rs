// Chat streaming listener example (gRPC).

use anytype::prelude::*;
use futures::StreamExt;

#[tokio::main]
async fn main() -> Result<(), AnytypeError> {
    let client = AnytypeClient::with_config(ClientConfig {
        app_name: "anytype-examples".into(),
        keystore_service: Some("anyr".to_string()), // reuse "anyr" gRPC credentials
        ..Default::default()
    })?;

    let chat_ids: Vec<String> = std::env::args().skip(1).collect();
    let mut builder = client
        .chat_stream()
        .buffer(512)
        .backoff(BackoffPolicy::default());

    if chat_ids.is_empty() {
        builder = builder.subscribe_previews();
    } else {
        for chat_id in &chat_ids {
            builder = builder.subscribe_chat(chat_id.clone());
        }
    }

    let ChatStreamHandle { mut events, .. } = builder.build();

    println!(
        "{:<20} | {:<20} | {:<16} | {}",
        "timestamp", "chat", "sender", "message"
    );
    // Chat names are printed as chat ids; map ids to names with the objects API if desired.
    while let Some(event) = events.next().await {
        match event {
            ChatEvent::MessageAdded { chat_id, message }
            | ChatEvent::MessageUpdated { chat_id, message } => {
                println!(
                    "{:<20} | {:<20} | {:<16} | {}",
                    message.created_at.to_rfc3339(),
                    chat_id,
                    message.creator,
                    message.content.text
                );
            }
            ChatEvent::StreamDisconnected => {
                eprintln!("chat stream disconnected");
            }
            ChatEvent::StreamResubscribed => {
                eprintln!("chat stream resubscribed");
            }
            _ => {}
        }
    }

    Ok(())
}
