// Demonstrates OR filter expressions for search.

use anytype::prelude::*;

#[tokio::main]
async fn main() -> Result<(), AnytypeError> {
    let client = AnytypeClient::new(env!("CARGO_BIN_NAME"))?.env_key_store()?;
    let space_id = anytype::test_util::example_space_id(&client).await?;

    let contains_meeting = Filter::text_contains("name", "Meeting");
    let contains_notes = Filter::text_contains("name", "Notes");

    let expr = FilterExpression::or(vec![contains_meeting, contains_notes], Vec::new());

    let results = client
        .search_in(&space_id)
        .filters(expr)
        .limit(10)
        .execute()
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
