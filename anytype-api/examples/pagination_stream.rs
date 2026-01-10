// Shows collect_all and streaming pagination helpers.

use anytype::prelude::*;
use futures::StreamExt;

#[tokio::main]
async fn main() -> Result<(), AnytypeError> {
    let client = AnytypeClient::new(env!("CARGO_BIN_NAME"))?.env_key_store()?;
    let space_id = anytype::test_util::example_space_id(&client).await?;

    let all_properties = client
        .properties(&space_id)
        .limit(50)
        .list()
        .await?
        .collect_all()
        .await?;
    println!("Collected {} properties", all_properties.len());

    let mut stream = client
        .properties(&space_id)
        .limit(50)
        .list()
        .await?
        .into_stream();

    let mut count = 0usize;
    while let Some(property) = stream.next().await {
        let property = property?;
        count += 1;
        println!("- {} ({})", property.name, property.key);
        if count >= 5 {
            break;
        }
    }

    Ok(())
}
