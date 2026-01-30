//! Integration Tests for Tags API
//!
//! This module provides comprehensive integration tests for the Tags API in the anytype crate.
//! Tests cover tag listing, creation, updates, deletion, and setting tags on objects.
//!
//! ## Environment Requirements
//!
//! Required environment variables (see .test-env):
//! - `ANYTYPE_TEST_URL` - API endpoint (default: http://127.0.0.1:31012)
//! - `ANYTYPE_TEST_KEY_FILE` - Path to file containing API key
//! - `ANYTYPE_TEST_SPACE_ID` - Existing space ID for testing
//!
//! ## Running
//!
//! ```bash
//! source .test-env
//! cargo test -p anytype --test test_tags
//! ```

mod common;

use anytype::{prelude::*, test_util::with_test_context_unit};
use common::{create_object_with_retry, unique_test_name, update_object_with_retry};

// =============================================================================
// Tag Listing Tests
// =============================================================================

/// Test listing tags for a select property
#[tokio::test]
#[test_log::test]
async fn test_list_tags_for_select_property() {
    with_test_context_unit(|ctx| async move {
        // Create a select property and tags
        let prop_name = unique_test_name("Select Prop");
        let property = ctx
            .client
            .new_property(&ctx.space_id, &prop_name, PropertyFormat::Select)
            .create()
            .await
            .expect("Failed to create select property");
        ctx.register_property(&property.id);

        let _tag1 = ctx
            .client
            .new_tag(&ctx.space_id, &property.id)
            .name("Option 1")
            .color(Color::Blue)
            .create()
            .await
            .expect("Failed to create tag 1");
        let _tag2 = ctx
            .client
            .new_tag(&ctx.space_id, &property.id)
            .name("Option 2")
            .color(Color::Red)
            .create()
            .await
            .expect("Failed to create tag 2");

        // List tags for this property
        let tags = ctx
            .client
            .tags(&ctx.space_id, &property.id)
            .list()
            .await
            .expect("Failed to list tags")
            .collect_all()
            .await
            .expect("collect tags");

        assert_eq!(tags.len(), 2, "Should have 2 tags");
    })
    .await
}

/// Test listing tags for a multi-select property
#[tokio::test]
#[test_log::test]
async fn test_list_tags_for_multiselect_property() {
    with_test_context_unit(|ctx| async move {
        // Create a multi-select property and tags
        let prop_name = unique_test_name("MultiSelect Prop");
        let property = ctx
            .client
            .new_property(&ctx.space_id, &prop_name, PropertyFormat::MultiSelect)
            .create()
            .await
            .expect("Failed to create multi-select property");
        ctx.register_property(&property.id);

        let _tag_a = ctx
            .client
            .new_tag(&ctx.space_id, &property.id)
            .name("Tag A")
            .color(Color::Yellow)
            .create()
            .await
            .expect("Failed to create tag A");
        let _tag_b = ctx
            .client
            .new_tag(&ctx.space_id, &property.id)
            .name("Tag B")
            .color(Color::Lime)
            .create()
            .await
            .expect("Failed to create tag B");
        let _tag_c = ctx
            .client
            .new_tag(&ctx.space_id, &property.id)
            .name("Tag C")
            .color(Color::Purple)
            .create()
            .await
            .expect("Failed to create tag C");

        // List tags
        let tags = ctx
            .client
            .tags(&ctx.space_id, &property.id)
            .list()
            .await
            .expect("Failed to list tags")
            .collect_all()
            .await
            .expect("collect_all");

        // Verify tag names
        assert_eq!(tags.len(), 3, "Should have 3 tags");
        assert!(tags.iter().any(|t| t.name == "Tag A"));
        assert!(tags.iter().any(|t| t.name == "Tag B"));
        assert!(tags.iter().any(|t| t.name == "Tag C"));
    })
    .await
}

/// Test listing tags for a property with no tags returns empty list
#[tokio::test]
#[test_log::test]
async fn test_list_tags_empty_property() {
    with_test_context_unit(|ctx| async move {
        // Create a select property without tags
        let prop_name = unique_test_name("Empty Select");
        let property = ctx
            .client
            .new_property(&ctx.space_id, &prop_name, PropertyFormat::Select)
            .create()
            .await
            .expect("Failed to create empty select property");
        ctx.register_property(&property.id);

        // List tags should return empty list
        let tags = ctx
            .client
            .tags(&ctx.space_id, &property.id)
            .list()
            .await
            .expect("Failed to list tags");

        assert_eq!(tags.len(), 0, "Should have no tags");
    })
    .await
}

/// Test that listing tags on a non-select property returns an appropriate response
#[tokio::test]
#[test_log::test]
async fn test_list_tags_invalid_property() {
    with_test_context_unit(|ctx| async move {
        // Create a text property (not select/multi-select)
        let prop_name = unique_test_name("Text Prop");
        let property = ctx
            .client
            .new_property(&ctx.space_id, &prop_name, PropertyFormat::Text)
            .create()
            .await
            .expect("Failed to create text property");
        ctx.register_property(&property.id);

        // Try to list tags on a text property
        // The API may return empty list or error - both are acceptable
        let tags_result = ctx.client.tags(&ctx.space_id, &property.id).list().await;

        // Either succeed with empty list or fail with appropriate error
        match tags_result {
            Ok(tags) => {
                assert_eq!(tags.len(), 0, "Text property should have no tags");
            }
            Err(_) => {
                // Error is acceptable for non-select properties
            }
        }
    })
    .await
}

// =============================================================================
// Tag Structure Tests
// =============================================================================

/// Test that tags have required fields (id, name, color)
#[tokio::test]
#[test_log::test]
async fn test_tag_has_required_fields() {
    with_test_context_unit(|ctx| async move {
        // Create property with a tag
        let prop_name = unique_test_name("Test Prop");
        let property = ctx
            .client
            .new_property(&ctx.space_id, &prop_name, PropertyFormat::Select)
            .tag("Test Tag", Some("test_key".to_string()), Color::Orange)
            .create()
            .await
            .expect("Failed to create property");
        ctx.register_property(&property.id);

        // Get the tags
        let tags = ctx
            .client
            .tags(&ctx.space_id, &property.id)
            .list()
            .await
            .expect("Failed to list tags");

        assert_eq!(tags.len(), 1, "Should have 1 tag");
        let tag = tags.iter().next().expect("Should have at least one tag");

        // Verify required fields exist and are non-empty
        assert!(!tag.id.is_empty(), "Tag ID should not be empty");
        assert_eq!(tag.name, "Test Tag", "Tag name should match");
        assert_eq!(tag.key, "test_key", "Tag key should match");
        assert_eq!(tag.color, Color::Orange, "Tag color should match");
    })
    .await
}

/// Test that tag colors are valid enum values
#[tokio::test]
#[test_log::test]
async fn test_tag_color_values() {
    with_test_context_unit(|ctx| async move {
        // Create property with tags of different colors
        let colors = vec![
            Color::Grey,
            Color::Yellow,
            Color::Orange,
            Color::Red,
            Color::Pink,
            Color::Purple,
            Color::Blue,
            Color::Ice,
            Color::Teal,
            Color::Lime,
        ];

        let prop_name = unique_test_name("Color Test");
        let mut new_prop_req =
            ctx.client
                .new_property(&ctx.space_id, &prop_name, PropertyFormat::MultiSelect);

        for (i, color) in colors.iter().enumerate() {
            new_prop_req = new_prop_req.tag(&format!("Color {}", i), None, color.clone());
        }
        let property = new_prop_req
            .create()
            .await
            .expect("Failed to create property");
        ctx.register_property(&property.id);

        // Get the tags
        let tags = ctx
            .client
            .tags(&ctx.space_id, &property.id)
            .list()
            .await
            .expect("Failed to list tags");

        assert_eq!(tags.len(), colors.len(), "Should have all color tags");

        // Verify all colors are represented
        let tag_colors: Vec<Color> = tags.iter().map(|t| t.color.clone()).collect();
        for color in &colors {
            assert!(
                tag_colors.contains(color),
                "Color {:?} should be present",
                color
            );
        }
    })
    .await
}

// =============================================================================
// Tag Creation Tests
// =============================================================================

/// Test creating a new tag for a select property
#[tokio::test]
#[test_log::test]
async fn test_create_tag() {
    with_test_context_unit(|ctx| async move {
        // Create a select property
        let prop_name = unique_test_name("Create Tag Prop");
        let property = ctx
            .client
            .new_property(&ctx.space_id, &prop_name, PropertyFormat::Select)
            .create()
            .await
            .expect("Failed to create property");
        ctx.register_property(&property.id);

        // Create a new tag
        let tag_name = unique_test_name("New Tag");
        let tag = ctx
            .client
            .new_tag(&ctx.space_id, &property.id)
            .name(&tag_name)
            .create()
            .await
            .expect("Failed to create tag");

        assert_eq!(tag.name, tag_name, "Tag name should match");
        assert!(!tag.id.is_empty(), "Tag ID should not be empty");
        assert_eq!(tag.color, Color::Grey, "Default color should be Grey");

        // Verify tag appears in list
        let tags = ctx
            .client
            .tags(&ctx.space_id, &property.id)
            .list()
            .await
            .expect("Failed to list tags");

        assert!(
            tags.iter().any(|t| t.id == tag.id),
            "Created tag should appear in list"
        );
    })
    .await
}

/// Test creating a tag with a specific color
#[tokio::test]
#[test_log::test]
async fn test_create_tag_with_color() {
    with_test_context_unit(|ctx| async move {
        // Create a select property
        let prop_name = unique_test_name("Color Tag Prop");
        let property = ctx
            .client
            .new_property(&ctx.space_id, &prop_name, PropertyFormat::Select)
            .create()
            .await
            .expect("Failed to create property");
        ctx.register_property(&property.id);

        // Create a tag with a specific color
        let tag_name = unique_test_name("Blue Tag");
        let tag = ctx
            .client
            .new_tag(&ctx.space_id, &property.id)
            .name(&tag_name)
            .color(Color::Blue)
            .create()
            .await
            .expect("Failed to create tag with color");

        assert_eq!(tag.color, Color::Blue, "Tag color should be Blue");
    })
    .await
}

/// Test handling of duplicate tag names
#[tokio::test]
#[test_log::test]
async fn test_create_duplicate_tag_name() {
    with_test_context_unit(|ctx| async move {
        // Create a property with a tag
        let prop_name = unique_test_name("Dup Tag Prop");
        let tag_name = "Duplicate";
        let property = ctx
            .client
            .new_property(&ctx.space_id, &prop_name, PropertyFormat::Select)
            .tag(tag_name, None, Color::Red)
            .create()
            .await
            .expect("Failed to create property");
        ctx.register_property(&property.id);

        // Try to create another tag with the same name
        ctx.client
            .new_tag(&ctx.space_id, &property.id)
            .name(tag_name)
            .color(Color::Blue)
            .create()
            .await
            .expect("creating two tags with same name should be ok");

        // this should be ok - it'll assign different keys
        // If duplicate names are allowed, verify both exist
        let tags = ctx
            .client
            .tags(&ctx.space_id, &property.id)
            .list()
            .await
            .expect("Failed to list tags")
            .collect_all()
            .await
            .expect("collect_all");
        assert_eq!(tags.len(), 2, "two tags");
        let tag0 = tags.first().unwrap();
        let tag1 = tags.get(1).unwrap();
        assert_eq!(tag0.name, tag1.name, "names equal");
        assert_ne!(tag0.id, tag1.id, "tag ids should be different");
    })
    .await
}

// =============================================================================
// Tag on Objects Tests
// =============================================================================

/// Test setting a tag value on an object's select property
#[tokio::test]
#[test_log::test]
async fn test_set_tag_on_object_select() {
    with_test_context_unit(|ctx| async move {
        // Create a select property with tags
        let prop_name = unique_test_name("Status");
        // convert key to snake_case
        let prop_key = prop_name.to_lowercase().replace(" ", "_");

        let property = ctx
            .client
            .new_property(&ctx.space_id, &prop_name, PropertyFormat::Select)
            .key(&prop_key)
            .create()
            .await
            .expect("Failed to create property");
        ctx.register_property(&property.id);

        let active = ctx
            .client
            .new_tag(&ctx.space_id, &property.id)
            .name("Active")
            .key("active")
            .color(Color::Lime)
            .create()
            .await
            .expect("create active tag");
        let _inactive = ctx
            .client
            .new_tag(&ctx.space_id, &property.id)
            .name("Inactive")
            .key("inactive")
            .color(Color::Grey)
            .create()
            .await
            .expect("create inactive tag");

        // Create an object with the select property set
        let obj_name = unique_test_name("Test Object");
        let object = create_object_with_retry("Test Object", || async {
            ctx.client
                .new_object(&ctx.space_id, "page")
                .name(&obj_name)
                .set_select(&prop_key, &active.id)
                .create()
                .await
        })
        .await
        .expect("Failed to create object");
        ctx.register_object(&object.id);

        // Read the object back and verify the property value
        let fetched = ctx
            .client
            .object(&ctx.space_id, &object.id)
            .get()
            .await
            .expect("Failed to fetch object");

        // Find the status property
        let status_prop = fetched
            .properties
            .iter()
            .find(|p| p.key == prop_key)
            .expect("Status property should exist");

        if let PropertyValue::Select { select } = &status_prop.value {
            assert_eq!(select.key, active.key, "Status should be 'active'");
        } else {
            panic!("Property should be Select type");
        }
    })
    .await
}

/// Test setting multiple tags on an object's multi-select property
#[tokio::test]
#[test_log::test]
async fn test_set_tags_on_object_multiselect() {
    with_test_context_unit(|ctx| async move {
        // Create a multi-select property with tags
        let prop_name = unique_test_name("Categories");
        // convert key to snake_case
        let prop_key = prop_name.to_lowercase().replace(" ", "_");

        let property = ctx
            .client
            .new_property(&ctx.space_id, &prop_name, PropertyFormat::MultiSelect)
            .key(&prop_key)
            .create()
            .await
            .expect("Failed to create property");
        ctx.register_property(&property.id);
        let work_tag = ctx
            .client
            .new_tag(&ctx.space_id, &property.id)
            .name("Work")
            .key("work")
            .color(Color::Blue)
            .create()
            .await
            .expect("create work tag");
        let _personal_tag = ctx
            .client
            .new_tag(&ctx.space_id, &property.id)
            .name("Personal")
            .key("personal")
            .color(Color::Lime)
            .create()
            .await
            .expect("create personal tag");
        let urgent_tag = ctx
            .client
            .new_tag(&ctx.space_id, &property.id)
            .name("Urgent")
            .key("urgent")
            .color(Color::Red)
            .create()
            .await
            .expect("create urgent tag");

        // Create an object with multiple tags
        let obj_name = unique_test_name("Multi Tag Object");
        let object = create_object_with_retry("Multi Tag Object", || async {
            ctx.client
                .new_object(&ctx.space_id, "page")
                .name(&obj_name)
                .set_multi_select(&prop_key, vec![work_tag.id.clone(), urgent_tag.id.clone()])
                .create()
                .await
        })
        .await
        .expect("Failed to create object");
        ctx.register_object(&object.id);

        // Read the object back and verify the property values
        let fetched = ctx
            .client
            .object(&ctx.space_id, &object.id)
            .get()
            .await
            .expect("Failed to fetch object");

        // Find the categories property
        let categories_prop = fetched
            .properties
            .iter()
            .find(|p| p.key == prop_key)
            .expect("Categories property should exist");

        if let PropertyValue::MultiSelect { multi_select } = &categories_prop.value {
            assert_eq!(multi_select.len(), 2, "Should have 2 selected tags");

            assert!(
                multi_select.iter().any(|t| t.id == work_tag.id),
                "has tag 'work'"
            );
            assert!(
                multi_select.iter().any(|t| t.id == urgent_tag.id),
                "has tag 'urgent'"
            );
        } else {
            panic!("Property should be MultiSelect type");
        }
    })
    .await
}

/// Test reading tag values from an object
#[tokio::test]
#[test_log::test]
async fn test_read_tag_from_object() {
    with_test_context_unit(|ctx| async move {
        // Create a select property
        let prop_name = unique_test_name("Priority");
        let prop_key = prop_name.to_lowercase().replace(" ", "_");

        let property = ctx
            .client
            .new_property(&ctx.space_id, &prop_name, PropertyFormat::Select)
            .key(&prop_key)
            .create()
            .await
            .expect("Failed to create property");
        ctx.register_property(&property.id);
        let high_tag = ctx
            .client
            .new_tag(&ctx.space_id, &property.id)
            .name("High")
            .key("high")
            .color(Color::Red)
            .create()
            .await
            .expect("create high tag");
        let _medium_tag = ctx
            .client
            .new_tag(&ctx.space_id, &property.id)
            .name("Medium")
            .key("medium")
            .color(Color::Yellow)
            .create()
            .await
            .expect("create medium tag");
        let low_tag = ctx
            .client
            .new_tag(&ctx.space_id, &property.id)
            .name("Low")
            .key("low")
            .color(Color::Grey)
            .create()
            .await
            .expect("create low tag");

        // Create object with priority set
        let obj_name = unique_test_name("Priority Object");
        let object = create_object_with_retry("Priority Object", || async {
            ctx.client
                .new_object(&ctx.space_id, "page")
                .name(&obj_name)
                // TODO: should this be the id or the key?
                .set_select(&prop_key, &high_tag.id)
                .create()
                .await
        })
        .await
        .expect("Failed to create object");
        ctx.register_object(&object.id);

        // Get the object and extract tag value
        let fetched = ctx
            .client
            .object(&ctx.space_id, &object.id)
            .get()
            .await
            .expect("Failed to fetch object");

        // Verify we can read the tag value
        // find tag by iterating through properties
        let priority = fetched
            .properties
            .iter()
            .find(|p| p.key == prop_key)
            .and_then(|p| {
                if let PropertyValue::Select { select } = &p.value {
                    Some(select)
                } else {
                    None
                }
            })
            .expect("Should be able to read priority tag");

        assert_eq!(priority.name, "High", "Priority should be 'High'");
        assert_eq!(&priority.id, &high_tag.id, "Priority tag id");

        // alternate: find with get_property_select
        assert_eq!(
            fetched.get_property_select(&prop_key).map(|tag| &tag.id),
            Some(&high_tag.id),
            "get_property_select: high"
        );

        // Update the priority
        let updated = update_object_with_retry("Priority Object", || async {
            ctx.client
                .update_object(&ctx.space_id, &object.id)
                .set_select(&prop_key, &low_tag.id)
                .update()
                .await
        })
        .await
        .expect("Failed to update object");

        // verify change after update
        assert_eq!(
            updated.get_property_select(&prop_key).map(|tag| &tag.id),
            Some(&low_tag.id),
            "get_property_select: low"
        );
    })
    .await
}

// =============================================================================
// Tag Deletion Tests
// =============================================================================

/// Test deleting/archiving a tag
#[tokio::test]
#[test_log::test]
async fn test_delete_tag() {
    with_test_context_unit(|ctx| async move {
        // Create a select property and tags
        let prop_name = unique_test_name("Delete Tag Prop");
        let property = ctx
            .client
            .new_property(&ctx.space_id, &prop_name, PropertyFormat::Select)
            .create()
            .await
            .expect("Failed to create property");
        ctx.register_property(&property.id);

        let _keep = ctx
            .client
            .new_tag(&ctx.space_id, &property.id)
            .name("Keep")
            .color(Color::Lime)
            .create()
            .await
            .expect("create keep tag");
        let _delete = ctx
            .client
            .new_tag(&ctx.space_id, &property.id)
            .name("Delete")
            .color(Color::Red)
            .create()
            .await
            .expect("create delete tag");

        // Get the tags
        let tags = ctx
            .client
            .tags(&ctx.space_id, &property.id)
            .list()
            .await
            .expect("Failed to list tags")
            .collect_all()
            .await
            .expect("collect tags");

        assert_eq!(tags.len(), 2, "Should have 2 tags initially");

        // Find the tag to delete
        let tag_to_delete = tags
            .iter()
            .find(|t| t.name == "Delete")
            .expect("Should find 'Delete' tag");

        // Delete the tag
        let deleted = ctx
            .client
            .tag(&ctx.space_id, &property.id, &tag_to_delete.id)
            .delete()
            .await
            .expect("Failed to delete tag");

        assert_eq!(deleted.name, "Delete", "Deleted tag name should match");

        // Verify tag list (may lag behind deletion)
        let tags_after = ctx
            .client
            .tags(&ctx.space_id, &property.id)
            .list()
            .await
            .expect("Failed to list tags after deletion")
            .collect_all()
            .await
            .expect("collect tags after deletion");

        let remaining = tags_after
            .iter()
            .find(|tag| tag.name == "Keep")
            .expect("Remaining tag should be 'Keep'");
        if tags_after.iter().any(|tag| tag.id == tag_to_delete.id) {
            eprintln!("warning: deleted tag still visible in list");
        }
        assert_eq!(remaining.name, "Keep", "Remaining tag should be 'Keep'");
    })
    .await
}

// =============================================================================
// Tag Update Tests
// =============================================================================

/// Test updating a tag's name and color
#[tokio::test]
#[test_log::test]
async fn test_update_tag() {
    with_test_context_unit(|ctx| async move {
        // Create a select property with a tag
        let prop_name = unique_test_name("Update Tag Prop");
        let property = ctx
            .client
            .new_property(&ctx.space_id, &prop_name, PropertyFormat::Select)
            .tag("Original Name", None, Color::Grey)
            .create()
            .await
            .expect("Failed to create property");
        ctx.register_property(&property.id);

        // Get the tag
        let tags = ctx
            .client
            .tags(&ctx.space_id, &property.id)
            .list()
            .await
            .expect("Failed to list tags");

        assert_eq!(tags.len(), 1, "Should have 1 tag");
        let tag_id = tags.iter().next().expect("Should have one tag").id.clone();

        // Update the tag
        let updated = ctx
            .client
            .update_tag(&ctx.space_id, &property.id, &tag_id)
            .name("Updated Name")
            .color(Color::Purple)
            .update()
            .await
            .expect("Failed to update tag");

        assert_eq!(updated.name, "Updated Name", "Tag name should be updated");
        assert_eq!(updated.color, Color::Purple, "Tag color should be updated");

        // Verify via get
        let fetched = ctx
            .client
            .tag(&ctx.space_id, &property.id, &tag_id)
            .get()
            .await
            .expect("Failed to get tag");

        assert_eq!(
            fetched.name, "Updated Name",
            "Fetched tag name should match"
        );
        assert_eq!(
            fetched.color,
            Color::Purple,
            "Fetched tag color should match"
        );

        // Cleanup
        let _ = ctx
            .client
            .property(&ctx.space_id, &property.id)
            .delete()
            .await;
    })
    .await
}

// =============================================================================
// Edge Cases and Error Handling
// =============================================================================

/// Test getting a specific tag by ID
#[tokio::test]
#[test_log::test]
async fn test_get_tag_by_id() {
    with_test_context_unit(|ctx| async move {
        // Create property with a tag
        let prop_name = unique_test_name("Get Tag Prop");
        let property = ctx
            .client
            .new_property(&ctx.space_id, &prop_name, PropertyFormat::Select)
            .tag("Specific Tag", Some("specific".to_string()), Color::Teal)
            .create()
            .await
            .expect("Failed to create property");
        ctx.register_property(&property.id);

        // List tags to get the ID
        let tags = ctx
            .client
            .tags(&ctx.space_id, &property.id)
            .list()
            .await
            .expect("Failed to list tags");

        assert_eq!(tags.len(), 1, "Should have 1 tag");
        let tag_id = tags.iter().next().expect("Should have one tag").id.clone();

        // Get the specific tag by ID
        let tag = ctx
            .client
            .tag(&ctx.space_id, &property.id, &tag_id)
            .get()
            .await
            .expect("Failed to get tag by ID");

        assert_eq!(tag.name, "Specific Tag", "Tag name should match");
        assert_eq!(tag.key, "specific", "Tag key should match");
        assert_eq!(tag.color, Color::Teal, "Tag color should match");
    })
    .await
}

/// Test pagination with limit and offset for tags
#[tokio::test]
#[test_log::test]
async fn test_tag_pagination() {
    with_test_context_unit(|ctx| async move {
        // Create property with multiple tags
        let prop_name = unique_test_name("Pagination Prop");
        let mut prop_req =
            ctx.client
                .new_property(&ctx.space_id, &prop_name, PropertyFormat::Select);

        for i in 0..5 {
            prop_req = prop_req.tag(&format!("Tag {}", i), None, Color::Blue);
        }

        let property = prop_req.create().await.expect("Failed to create property");
        ctx.register_property(&property.id);

        // Test limit
        let limited = ctx
            .client
            .tags(&ctx.space_id, &property.id)
            .limit(3)
            .list()
            .await
            .expect("Failed to list with limit");

        assert_eq!(limited.len(), 3, "Should respect limit parameter");

        // Test offset
        let offset_tags = ctx
            .client
            .tags(&ctx.space_id, &property.id)
            .offset(2)
            .list()
            .await
            .expect("Failed to list with offset");

        assert_eq!(offset_tags.len(), 3, "Should have 3 tags after offset 2");
    })
    .await
}

/// Test creating a tag without a name (validation error)
#[tokio::test]
#[test_log::test]
async fn test_create_tag_without_name() {
    with_test_context_unit(|ctx| async move {
        // Create a select property
        let prop_name = unique_test_name("Validation Prop");
        let property = ctx
            .client
            .new_property(&ctx.space_id, &prop_name, PropertyFormat::Select)
            .create()
            .await
            .expect("Failed to create property");
        ctx.register_property(&property.id);

        // Try to create a tag without setting a name
        let result = ctx
            .client
            .new_tag(&ctx.space_id, &property.id)
            .color(Color::Red)
            .create()
            .await;

        // Should fail with validation error
        assert!(result.is_err(), "Creating tag without name should fail");

        if let Err(e) = result {
            match e {
                AnytypeError::Validation { message } => {
                    assert!(
                        message.contains("name"),
                        "Error should mention missing name"
                    );
                }
                _ => panic!("Expected Validation error, got: {:?}", e),
            }
        }
    })
    .await
}
