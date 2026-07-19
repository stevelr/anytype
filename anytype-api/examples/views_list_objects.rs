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

    for view in views.into_iter() {
        let view_name = view.name.as_deref().unwrap_or("");
        println!(
            "- View:{} name: {view_name} layout: {}",
            view.id, view.layout,
        );
        let objects_view = client
            .view_list_objects(&space_id, &list_obj.id)
            .view(&view.id)
            .limit(10)
            .list()
            .await?;
        for obj in objects_view.items.iter() {
            let ty = obj.get_type().and_then(|t| t.name);
            let name = obj.name.as_deref().unwrap_or("");
            println!("    - name: {name}  type: {}", ty.as_deref().unwrap_or(""));
        }
    }

    Ok(())
}
