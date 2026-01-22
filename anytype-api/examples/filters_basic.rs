// Lists objects using simple filters (type and text).

use anytype::prelude::*;

#[tokio::main]
async fn main() -> Result<(), AnytypeError> {
    let client = AnytypeClient::with_config(ClientConfig {
        app_name: "anytype-examples".into(),
        keystore_service: Some("anyr".to_string()), // reuse "anyr"'s auth tokens
        ..Default::default()
    })?;
    let space_id = anytype::test_util::example_space_id(&client).await?;

    // we need the id of type page to use it for a filter
    let page_id = client.lookup_type_by_key(&space_id, "page").await?;

    let results = client
        .objects(&space_id)
        .filter(Filter::type_in(vec![page_id.id]))
        .filter(Filter::text_contains("name", "Project"))
        .limit(10)
        .list()
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
