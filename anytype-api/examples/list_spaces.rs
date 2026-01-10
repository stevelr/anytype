// Lists spaces available to the authenticated API key.
//

use anytype::prelude::*;

#[tokio::main]
async fn main() -> Result<(), AnytypeError> {
    let client = AnytypeClient::new(env!("CARGO_BIN_NAME"))?.env_key_store()?;
    let spaces = client.spaces().list().await?;

    for space in spaces.iter() {
        println!("{} {}", space.id, space.name);
    }

    Ok(())
}
