// Lists spaces available to the authenticated API key.
//

use anytype::prelude::*;

#[tokio::main]
async fn main() -> Result<(), AnytypeError> {
    let client = AnytypeClient::with_config(ClientConfig {
        app_name: "anytype-examples".into(),
        keystore_service: Some("anyr".to_string()), // reuse "anyr"'s auth tokens
        ..Default::default()
    })?;
    let spaces = client.spaces().list().await?;

    for space in spaces.iter() {
        println!("{} {}", space.id, space.name);
    }

    Ok(())
}
