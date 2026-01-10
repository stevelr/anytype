// Updates the markdown body on an object after creation.

use anytype::prelude::*;

#[tokio::main]
async fn main() -> Result<(), AnytypeError> {
    let client = AnytypeClient::new(env!("CARGO_BIN_NAME"))?.env_key_store()?;
    let space_id = anytype::test_util::example_space_id(&client).await?;

    let obj = client
        .new_object(&space_id, "page")
        .name("API Example: Update Markdown")
        .body("Initial content")
        .create()
        .await?;

    let updated = client
        .update_object(&space_id, &obj.id)
        .body("# Updated Content\n\n- Item 1\n- Item 2")
        .update()
        .await?;

    println!("Updated markdown for object {}", updated.id);

    // cleanup
    client.object(&space_id, &obj.id).delete().await?;
    Ok(())
}
