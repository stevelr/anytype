// Lists types and properties in a space for a quick schema overview.

use anytype::prelude::*;

#[tokio::main]
async fn main() -> Result<(), AnytypeError> {
    let client = AnytypeClient::with_config(ClientConfig {
        app_name: "anytype-examples".into(),
        keystore_service: Some("anyr".to_string()), // reuse "anyr"'s auth tokens
        ..Default::default()
    })?;
    let spaces = client.spaces().list().await?;
    let space = spaces.iter().next().unwrap();

    // list some types in the space
    let types = client.types(&space.id).list().await?;
    println!("Types: Showing 5 of {}", types.items.len());
    for typ in types.iter().take(5) {
        println!("  {:20} {:20}", typ.display_name(), typ.key);
    }
    println!();

    // list some properties in the space
    let properties = client.properties(&space.id).list().await?;
    println!("Properties: Showing 5 of {}", properties.items.len());
    for prop in properties.iter().take(5) {
        println!("  {:20} {:20}", prop.name, prop.key);
    }

    Ok(())
}
