// Runs a global search across all spaces.

use anytype::prelude::*;

#[tokio::main]
async fn main() -> Result<(), AnytypeError> {
    let client = AnytypeClient::with_config(ClientConfig {
        app_name: "anytype-examples".into(),
        keystore_service: Some("anyr".to_string()), // reuse "anyr"'s auth tokens
        ..Default::default()
    })?;

    let results = client
        .search_global()
        .text("meeting")
        .types(["page", "note"])
        .limit(10)
        .execute()
        .await?;

    for obj in results.iter() {
        println!(
            "{} ({})",
            obj.name.as_deref().unwrap_or("(unnamed)"),
            obj.id
        );
    }

    Ok(())
}
