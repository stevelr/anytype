// Creates a page with markdown and a few properties.

use anytype::prelude::*;

#[tokio::main]
async fn main() -> Result<(), AnytypeError> {
    let client = AnytypeClient::new(env!("CARGO_BIN_NAME"))?.env_key_store()?;
    let space_id = anytype::test_util::example_space_id(&client).await?;

    let obj = client
        .new_object(&space_id, "page")
        .name("API Example: New Page")
        .body("# Hello from anytype\n\nCreated by an example program.")
        .set_text("description", "Created by the create object example")
        .create()
        .await?;

    println!(
        "Created object '{}' id:{}",
        obj.name.unwrap_or_default(),
        obj.id
    );

    // cleanup
    client.object(&space_id, &obj.id).delete().await?;
    Ok(())
}
