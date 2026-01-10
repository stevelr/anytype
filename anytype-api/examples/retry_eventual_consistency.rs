// Uses ensure_available to wait for read-after-write consistency.

use std::time::Duration;

use anytype::prelude::*;

#[tokio::main]
async fn main() -> Result<(), AnytypeError> {
    let client = AnytypeClient::new(env!("CARGO_BIN_NAME"))?.env_key_store()?;
    let space_id = anytype::test_util::example_space_id(&client).await?;

    let verify = VerifyConfig {
        timeout: Duration::from_secs(5),
        ..VerifyConfig::default()
    };

    let obj = client
        .new_object(&space_id, "page")
        .name("API Example: Verify Availability")
        .ensure_available_with(verify)
        .create()
        .await?;

    println!("Created and verified object {}", obj.id);

    // cleanup
    client.object(&space_id, &obj.id).delete().await?;
    Ok(())
}
