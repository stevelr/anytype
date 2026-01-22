// Searches within a space and sorts results by a property.

use anytype::prelude::*;

#[tokio::main]
async fn main() -> Result<(), AnytypeError> {
    let client = AnytypeClient::with_config(ClientConfig {
        app_name: "anytype-examples".into(),
        keystore_service: Some("anyr".to_string()), // reuse "anyr"'s auth tokens
        ..Default::default()
    })?;
    let space_id = anytype::test_util::example_space_id(&client).await?;

    let results = client
        .search_in(&space_id)
        .text("abc")
        .types(["task"])
        .sort_desc("last_modified_date")
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
