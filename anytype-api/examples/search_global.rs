// Runs a global search across all spaces.

use anytype::prelude::*;

#[tokio::main]
async fn main() -> Result<(), AnytypeError> {
    let client = AnytypeClient::new(env!("CARGO_BIN_NAME"))?.env_key_store()?;

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
