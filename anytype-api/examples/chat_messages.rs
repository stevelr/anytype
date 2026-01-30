// Chat message list example (gRPC).

use anytype::prelude::*;
use clap::{Parser, Subcommand};

mod example_lib;
use example_lib::table::render_table;

const DATE_FORMAT: &str = "%Y-%m-%d %H:%M:%S";
const MESSAGE_PAGE_LIMIT: usize = 200;

#[derive(Parser, Debug)]
#[command(name = "chat_messages", about = "Chat list/message example")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// List all chat objects
    Chats,
    /// List messages for a chat
    Messages {
        /// Chat object id
        chat_id: String,
        /// Show messages after order id
        #[arg(long)]
        after: Option<String>,
        /// Show only unread messages
        #[arg(long)]
        unread: bool,
    },
}

#[tokio::main]
async fn main() -> Result<(), AnytypeError> {
    let cli = Cli::parse();
    let client = AnytypeClient::with_config(ClientConfig {
        app_name: "anytype-examples".into(),
        keystore_service: Some("anyr".to_string()), // reuse "anyr" gRPC credentials
        ..Default::default()
    })?;

    match cli.command {
        Commands::Chats => list_chats(&client).await,
        Commands::Messages {
            chat_id,
            after,
            unread,
        } => list_messages(&client, &chat_id, after.as_deref(), unread).await,
    }
}

async fn list_chats(client: &AnytypeClient) -> Result<(), AnytypeError> {
    let chats = client.chats().list_chats().limit(200).list().await?;
    let mut rows = Vec::new();

    for chat in chats.items {
        let name = chat.name.as_deref().unwrap_or("(unnamed)").to_string();
        let date = chat
            .get_property_date("last_modified_date")
            .map(|value| value.format(DATE_FORMAT).to_string())
            .unwrap_or_default();
        rows.push(vec![chat.id, date, name]);
    }

    let headers = ["id", "date-last-modified", "name"];
    println!("{}", render_table(&headers, &rows));
    Ok(())
}

async fn list_messages(
    client: &AnytypeClient,
    chat_id: &str,
    after: Option<&str>,
    unread: bool,
) -> Result<(), AnytypeError> {
    let resolved_after = resolve_order_id_arg(after)?;
    let mut request = client
        .chats()
        .list_messages(chat_id)
        .limit(MESSAGE_PAGE_LIMIT);

    if let Some(after) = resolved_after.as_deref() {
        request = request.after(after);
    }
    if unread {
        request = request.unread_only(ChatReadType::Messages);
    }

    let page = request.list_page().await?;
    let headers = ["order_id", "timestamp", "unread", "sender", "message"];
    let rows = page
        .messages
        .iter()
        .map(|message| {
            let timestamp = message.created_at.format(DATE_FORMAT).to_string();
            let unread_marker = if message.read { " " } else { "*" }.to_string();
            let sender = last_five_chars(&message.creator);
            vec![
                format_order_id(&message.order_id),
                timestamp,
                unread_marker,
                sender,
                message.content.text.clone(),
            ]
        })
        .collect::<Vec<_>>();

    println!("{}", render_table(&headers, &rows));
    Ok(())
}

fn last_five_chars(value: &str) -> String {
    let len = value.len();
    if len <= 5 {
        value.to_string()
    } else {
        value[len - 5..].to_string()
    }
}

fn resolve_order_id_arg(value: Option<&str>) -> Result<Option<String>, AnytypeError> {
    let Some(value) = value else {
        return Ok(None);
    };
    if !is_hex(value) {
        return Err(AnytypeError::Validation {
            message: "after must be lowercase hex with even length".to_string(),
        });
    }
    let bytes = hex_to_bytes(value).map_err(|message| AnytypeError::Validation { message })?;
    let decoded = String::from_utf8(bytes).map_err(|err| AnytypeError::Validation {
        message: format!("invalid hex order id: {err}"),
    })?;
    Ok(Some(decoded))
}

fn is_hex(value: &str) -> bool {
    !value.is_empty()
        && value.len().is_multiple_of(2)
        && value
            .chars()
            .all(|ch| ch.is_ascii_digit() || ('a'..='f').contains(&ch))
}

fn hex_to_bytes(value: &str) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::with_capacity(value.len() / 2);
    let chars: Vec<char> = value.chars().collect();
    if !chars.len().is_multiple_of(2) {
        return Err("hex order id must have even length".to_string());
    }
    for chunk in chars.chunks(2) {
        let high = hex_value(chunk[0])?;
        let low = hex_value(chunk[1])?;
        bytes.push((high << 4) | low);
    }
    Ok(bytes)
}

fn hex_value(ch: char) -> Result<u8, String> {
    match ch {
        '0'..='9' => Ok((ch as u8) - b'0'),
        'a'..='f' => Ok((ch as u8) - b'a' + 10),
        _ => Err(format!("invalid hex character: {ch}")),
    }
}

fn format_order_id(order_id: &str) -> String {
    let mut out = String::with_capacity(order_id.len() * 2);
    for byte in order_id.as_bytes() {
        out.push_str(&format!("{:02x}", byte));
    }
    out
}
