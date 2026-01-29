//! Agenda - example app:
//!
//! - list top 10 tasks sorted by priority
//!    (requires that your Task object has a priority field)
//! - list 10 most recent documents containing the text "meeting notes"
//! - send the lists in a rich-text chat message with colors and hyperlinks
//!
use anytype::prelude::*;

const PROJECT_SPACE: &str = "Projects";
const CHAT_SPACE: &str = "Chat";

#[tokio::main]
async fn main() -> Result<(), AnytypeError> {
    let config = ClientConfig {
        app_name: "agenda".to_string(),
        keystore_service: Some("anyr".to_string()),
        ..Default::default()
    };
    let client = AnytypeClient::with_config(config)?;
    let space = client.lookup_space_by_name(PROJECT_SPACE).await?;

    // List 10 tasks sorted by priority
    let mut tasks = client
        .search_in(&space.id)
        .types(vec!["task"])
        // anytype-heart bug(?): can't sort by priority,
        // so get recently modified and sort after fetch
        //.sort_asc("priority")
        .sort_desc("last_modified_date")
        .limit(40)
        .execute()
        .await?
        .into_response()
        .take_items();
    tasks.sort_by_key(|t| t.get_property_u64("priority").unwrap_or_default());

    // Get 10 most recent pages or notes containing the text "meeting notes"
    // sort most recent on top
    let recent_note_docs = client
        .search_in(&space.id)
        .text("meeting notes")
        .types(["page", "note"])
        .sort_desc("last_modified_date")
        .limit(10)
        .execute()
        .await?;

    // Build the message with colored status indicators
    let mut message = MessageContent::new()
        .text("Good morning Jim,\n")
        .bold("Here are your tasks\n");
    for task in tasks.iter().take(10) {
        let priority = task.get_property_u64("priority").unwrap_or_default();
        let name = task.name.as_deref().unwrap_or("(unnamed)");
        message = message.text(&format!("{priority} "));
        message = status_color(message, task);
        message = message.text(&format!(" {name}\n"));
    }

    message = message.bold("\nand recent notes:\n");
    for doc in &recent_note_docs {
        let date = doc
            .get_property_date("last_modified_date")
            .unwrap_or_default()
            .format("%Y-%m-%d %H:%M");
        let name = doc.name.as_deref().unwrap_or("(unnamed)");
        message = message
            .text(&format!("{date} "))
            .link(name, doc.get_link())
            .nl();
    }

    // Post chat message
    let chat = client.chats().space_chat(CHAT_SPACE).get().await?;
    client
        .chats()
        .add_message(chat.id)
        .content(message)
        .send()
        .await?;

    Ok(())
}

/// Helper to append status name in its corresponding color.
/// If status is undefined, shows "New" in yellow.
fn status_color(message: MessageContent, obj: &Object) -> MessageContent {
    match obj.get_property_select("status") {
        Some(tag) => message.text_color(&tag.name, tag.color.clone()),
        None => message.text_color("New", Color::Yellow),
    }
}
