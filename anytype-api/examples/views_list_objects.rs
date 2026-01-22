// Lists views and objects in a collection/query if available.

use anytype::prelude::*;

fn find_list_object(objects: &[Object]) -> Option<&Object> {
    objects
        .iter()
        .find(|obj| matches!(obj.layout, ObjectLayout::Collection | ObjectLayout::Set))
}

#[tokio::main]
async fn main() -> Result<(), AnytypeError> {
    let client = AnytypeClient::with_config(ClientConfig {
        app_name: "anytype-examples".into(),
        keystore_service: Some("anyr".to_string()), // reuse "anyr"'s auth tokens
        ..Default::default()
    })?;
    let space_id = anytype::test_util::example_space_id(&client).await?;

    let objects = client.objects(&space_id).limit(100).list().await?;
    let list_obj = match find_list_object(&objects.items) {
        Some(obj) => obj,
        None => {
            println!("No collection/set objects found in this space.");
            return Ok(());
        }
    };

    let views = client.list_views(&space_id, &list_obj.id).list().await?;
    println!("List {} has {} views", list_obj.id, views.items.len());

    let objects_default = client
        .view_list_objects(&space_id, &list_obj.id)
        .limit(10)
        .list()
        .await?;
    println!("Default view has {} objects", objects_default.items.len());

    if let Some(view) = views.items.first() {
        let objects_view = client
            .view_list_objects(&space_id, &list_obj.id)
            .view(&view.id)
            .limit(10)
            .list()
            .await?;
        println!("View {} has {} objects", view.id, objects_view.items.len());
    }

    Ok(())
}
