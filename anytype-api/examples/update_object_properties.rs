// Updates properties on an existing object using typed setters.

use anytype::prelude::*;

#[tokio::main]
async fn main() -> Result<(), AnytypeError> {
    let client = AnytypeClient::new(env!("CARGO_BIN_NAME"))?.env_key_store()?;
    let space_id = anytype::test_util::example_space_id(&client).await?;

    let obj = client
        .new_object(&space_id, "page")
        .name("API Example: Update Properties")
        .create()
        .await?;

    let updated = client
        .update_object(&space_id, &obj.id)
        .set_text("description", "Updated by the update properties example")
        //.set_text("status", "done")
        //.set_number("priority", 2)
        .update()
        .await?;

    println!(
        "Updated object {} ({})",
        updated.name.unwrap_or_default(),
        updated.id
    );

    // cleanup
    client.object(&space_id, &obj.id).delete().await?;
    Ok(())
}
