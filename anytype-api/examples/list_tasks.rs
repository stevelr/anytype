// Lists tasks

use anytype::prelude::*;

#[tokio::main]
async fn main() -> Result<(), AnytypeError> {
    let client = AnytypeClient::with_config(ClientConfig {
        app_name: "anytype-examples".into(),
        keystore_service: Some("anyr".to_string()), // reuse "anyr"'s auth tokens
        ..Default::default()
    })?;
    let space_id = anytype::test_util::example_space_id(&client).await?;

    let tasks = client
        .search_in(&space_id)
        .types(vec!["task"])
        // bug[2887]: due_date not supported in sort criteria
        //.sort_asc("due_date")
        .sort_asc("created_date")
        .execute()
        .await?
        .collect_all()
        .await?;

    let space = client.space(&space_id).get().await?;
    println!("Listing {} tasks in {}\n", tasks.len(), &space.name);

    println!("{:20} {:10} {:10} Id", "Task", "Created", "Due");
    for task in tasks.iter() {
        let due_date = task
            .get_property_date("due_date")
            .map(|date| date.format("%Y-%m-%d").to_string())
            .unwrap_or_default();
        let created_date = task
            .get_property_date("created_date")
            .map(|date| date.format("%Y-%m-%d").to_string())
            .unwrap_or_default();

        println!(
            "{:20} {created_date:10} {due_date:10} {}",
            task.name.as_deref().unwrap_or("(Unnamed)"),
            &task.id,
        )
    }

    Ok(())
}
