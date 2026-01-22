// Lists templates for a type in the current space.

use anytype::prelude::*;

#[tokio::main]
async fn main() -> Result<(), AnytypeError> {
    let client = AnytypeClient::with_config(ClientConfig {
        app_name: "anytype-examples".into(),
        keystore_service: Some("anyr".to_string()), // reuse "anyr"'s auth tokens
        ..Default::default()
    })?;
    let space_id = anytype::test_util::example_space_id(&client).await?;

    let page_type = client.lookup_type_by_key(&space_id, "page").await?;

    let templates = client.templates(&space_id, &page_type.id).list().await?;
    println!(
        "Type '{}' has {} templates",
        page_type.key,
        templates.items.len()
    );

    for template in templates.iter() {
        println!(
            "- {} ({})",
            template.name.as_deref().unwrap_or("(unnamed)"),
            template.id
        );
    }

    Ok(())
}
