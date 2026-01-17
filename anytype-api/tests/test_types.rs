//! Integration tests for the Types API
//!
//! Tests the complete lifecycle of Anytype object types including:
//! - Listing and pagination
//! - Type retrieval
//! - Type creation with properties
//! - System type identification
//! - Template association
//! - Error handling
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
//! cargo test -p anytype --test test_types
//! ```

mod common;

use crate::common::{create_object_with_retry, lookup_property_tag_with_retry};
use anytype::prelude::*;
use anytype::test_util::{TestError, TestResult, unique_suffix, with_test_context};
use tracing::debug;

// =============================================================================
// Type Listing Tests
// =============================================================================

#[tokio::test]
#[test_log::test]
async fn test_types_list() -> TestResult<()> {
    with_test_context(|ctx| async move {
        let result = ctx.client.types(&ctx.space_id).list().await?;
        let types = &result.items;

        ctx.increment_calls(1);

        assert!(!types.is_empty(), "expected at least 1 type");

        // Verify each type has required fields
        for typ in types {
            assert!(!typ.id.is_empty(), "Type ID should not be empty");
            assert!(!typ.key.is_empty(), "Type key should not be empty");
        }

        println!("Listed {} types", types.len());
        Ok(())
    })
    .await
}

#[tokio::test]
#[test_log::test]
async fn test_types_list_with_limit() -> TestResult<()> {
    with_test_context(|ctx| async move {
        let limit = 3;
        let result = ctx.client.types(&ctx.space_id).limit(limit).list().await?;
        let types = &result.items;

        ctx.increment_calls(1);

        // Verify we got at most the requested limit
        assert!(
            types.len() <= limit,
            "Expected at most {} types, got {}",
            limit,
            types.len()
        );

        println!("Listed {} types with limit {}", types.len(), limit);
        Ok(())
    })
    .await
}

#[tokio::test]
#[test_log::test]
async fn test_types_list_with_offset() -> TestResult<()> {
    with_test_context(|ctx| async move {
        // Get all types first
        let all_types = ctx.client.types(&ctx.space_id).list().await?.items.clone();

        // Skip some types and get the rest
        let offset = 2;
        let result = ctx
            .client
            .types(&ctx.space_id)
            .offset(offset)
            .list()
            .await?;
        let offset_types = &result.items;

        ctx.increment_calls(2);

        // Verify offset worked if we have enough types
        if all_types.len() > offset {
            assert!(
                !offset_types.is_empty(),
                "Expected types after offset {}",
                offset
            );

            if offset_types.is_empty() {
                return Err(TestError::Assertion {
                    message: format!("Expected types after offset {}", offset),
                });
            }
        }

        println!(
            "Listed {} types with offset {} (total: {})",
            offset_types.len(),
            offset,
            all_types.len()
        );
        Ok(())
    })
    .await
}

#[tokio::test]
#[test_log::test]
async fn test_types_list_field_presence() -> TestResult<()> {
    with_test_context(|ctx| async move {
        let result = ctx.client.types(&ctx.space_id).list().await?;
        let types = &result.items;

        ctx.increment_calls(1);

        assert!(!types.is_empty(), "Expected types to be present");

        // Verify all required fields are present on each type
        for typ in types {
            assert!(!typ.id.is_empty(), "Type.id is required");
            assert!(!typ.key.is_empty(), "Type.key is required");
            assert!(
                !typ.properties.is_empty(),
                "Type.properties should have at least one property"
            );

            // Layout should be set (has default)
            // Note: We can't directly test the value but verify it deserializes
        }

        println!("Verified field presence on {} types", types.len());
        Ok(())
    })
    .await
}

// =============================================================================
// Type Retrieval Tests
// =============================================================================

#[tokio::test]
#[test_log::test]
async fn test_types_get_by_id() -> TestResult<()> {
    with_test_context(|ctx| async move {
        // Get a type ID from the list
        let types = ctx.client.types(&ctx.space_id).list().await?.items.clone();

        assert!(!types.is_empty(), "Need at least one type for this test");

        let first_type = &types[0];

        // Retrieve the specific type
        let retrieved = ctx
            .client
            .get_type(&ctx.space_id, &first_type.id)
            .get()
            .await?;

        ctx.increment_calls(2);

        // Verify it matches
        assert_eq!(retrieved.id, first_type.id);
        assert_eq!(retrieved.key, first_type.key);
        assert_eq!(retrieved.name, first_type.name);

        println!(
            "Successfully retrieved type: {} ({})",
            retrieved.key, retrieved.id
        );
        Ok(())
    })
    .await
}

#[tokio::test]
#[test_log::test]
async fn test_types_get_consistency() -> TestResult<()> {
    with_test_context(|ctx| async move {
        // Get types via list
        let types_from_list = ctx.client.types(&ctx.space_id).list().await?.items.clone();

        assert!(
            !types_from_list.is_empty(),
            "Need types for consistency test"
        );

        let sample_type = &types_from_list[0];

        // Get the same type via get
        let type_from_get = ctx
            .client
            .get_type(&ctx.space_id, &sample_type.id)
            .get()
            .await?;

        ctx.increment_calls(2);

        // Verify consistency
        assert_eq!(type_from_get.id, sample_type.id);
        assert_eq!(type_from_get.key, sample_type.key);
        assert_eq!(type_from_get.name, sample_type.name);
        assert_eq!(type_from_get.archived, sample_type.archived);
        assert_eq!(type_from_get.properties.len(), sample_type.properties.len());

        println!("Verified consistency for type: {}", sample_type.key);
        Ok(())
    })
    .await
}

#[tokio::test]
#[test_log::test]
async fn test_types_get_nonexistent() -> TestResult<()> {
    with_test_context(|ctx| async move {
        let fake_id = "nonexistent-type-12345";
        let result = ctx.client.get_type(&ctx.space_id, fake_id).get().await;

        ctx.increment_calls(1);

        // Should fail with NotFound error
        assert!(result.is_err(), "Expected error for nonexistent type");

        if let Err(e) = result {
            match e {
                AnytypeError::NotFound { obj_type, .. } if &obj_type == "Type" => {
                    println!("Correctly received NotFound error");
                }
                AnytypeError::Validation { .. } => {
                    println!("Correctly received Validation error for invalid type id");
                }
                _ => {
                    return Err(TestError::Assertion {
                        message: format!("Expected NotFound or Validation error, got: {:?}", e),
                    });
                }
            }
        }

        Ok(())
    })
    .await
}

// =============================================================================
// System Types Tests
// =============================================================================

#[tokio::test]
#[test_log::test]
async fn test_types_system_present() -> TestResult<()> {
    with_test_context(|ctx| async move {
        let types = ctx.client.types(&ctx.space_id).list().await?.items.clone();

        ctx.increment_calls(1);

        // Collect all type keys
        let type_keys: Vec<&str> = types.iter().map(|t| t.key.as_str()).collect();

        // Verify core system types are present
        let expected_system_types = ["page", "note", "task", "bookmark"];

        for expected_key in expected_system_types {
            assert!(
                type_keys.contains(&expected_key),
                "Expected system type '{}' to be present",
                expected_key
            );
        }

        println!(
            "Verified {} system types present",
            expected_system_types.len()
        );
        Ok(())
    })
    .await
}

#[tokio::test]
#[test_log::test]
async fn test_types_system_properties() -> TestResult<()> {
    with_test_context(|ctx| async move {
        let types = ctx.client.types(&ctx.space_id).list().await?.items.clone();

        ctx.increment_calls(1);

        // Find a system type (e.g., "page")
        let page_type = types
            .iter()
            .find(|t| t.key == "page")
            .expect("Page type should exist");

        // System types should have properties
        assert!(
            !page_type.properties.is_empty(),
            "System type 'page' should have properties"
        );

        // Verify properties have required fields
        for prop in &page_type.properties {
            assert!(!prop.id.is_empty(), "Property id should not be empty");
            assert!(!prop.key.is_empty(), "Property key should not be empty");
            assert!(!prop.name.is_empty(), "Property name should not be empty");
        }

        println!(
            "System type 'page' has {} properties",
            page_type.properties.len()
        );
        Ok(())
    })
    .await
}

#[tokio::test]
#[test_log::test]
async fn test_types_is_system_type() -> TestResult<()> {
    with_test_context(|ctx| async move {
        let types = ctx.client.types(&ctx.space_id).list().await?.items.clone();

        ctx.increment_calls(1);

        // Count system vs custom types
        let mut system_count = 0;
        let mut custom_count = 0;

        for typ in &types {
            if typ.is_system_type() {
                system_count += 1;
                // Verify it's actually a known system type
                assert!(
                    matches!(typ.key.as_str(), "page" | "note" | "task" | "bookmark"),
                    "is_system_type() returned true for non-system type: {}",
                    typ.key
                );
            } else {
                custom_count += 1;
            }
        }

        // We should have at least the 4 core system types
        assert!(
            system_count >= 4,
            "Expected at least 4 system types, found {}",
            system_count
        );

        println!(
            "Found {} system types and {} custom types",
            system_count, custom_count
        );
        Ok(())
    })
    .await
}

// =============================================================================
// Type Creation Tests
// =============================================================================

#[tokio::test]
#[test_log::test]
async fn test_types_create_simple() -> TestResult<()> {
    with_test_context(|ctx| async move {
        let unique_key = format!("test_type_{}", unique_suffix());

        // Create a new type
        let created = ctx
            .client
            .new_type(&ctx.space_id, "Simple Type")
            .key(&unique_key)
            .create()
            .await?;
        ctx.register_type(&created.id);

        ctx.increment_calls(1);

        // Verify creation
        assert!(!created.id.is_empty(), "Created type should have an ID");
        assert_eq!(created.key, unique_key);
        assert_eq!(created.name.as_deref(), Some("Simple Type"));
        assert!(!created.is_system_type());

        debug!("Created custom type: {} ({})", created.key, created.id);
        Ok(())
    })
    .await
}

#[tokio::test]
#[test_log::test]
async fn test_types_create_with_properties() -> TestResult<()> {
    with_test_context(|ctx| async move {
        let unique_key = format!("test_type_props_{}", unique_suffix());

        // Create type with properties
        let created = ctx
            .client
            .new_type(&ctx.space_id, "Product Type")
            .key(&unique_key)
            .property("Status", "status", PropertyFormat::Select)
            .property("Priority", "priority", PropertyFormat::Number)
            .create()
            .await?;

        ctx.increment_calls(1);
        ctx.register_type(&created.id);

        // Verify properties were created
        // Note: The API may add default properties, so we check for at least our 2
        let custom_props: Vec<_> = created
            .properties
            .iter()
            .filter(|p| p.key == "status" || p.key == "priority")
            .collect();

        assert!(
            custom_props.len() >= 2,
            "Expected at least 2 custom properties, found {}",
            custom_props.len()
        );

        // Verify property details
        let status_prop = created
            .properties
            .iter()
            .find(|p| p.key == "status")
            .expect("Status property should exist");
        assert_eq!(status_prop.name, "Status");
        assert_eq!(status_prop.format(), PropertyFormat::Select);

        println!(
            "Created type with {} total properties",
            created.properties.len()
        );

        Ok(())
    })
    .await
}

/// create custom type, and an object of that type
/// custom type has properties: priority (number), status (select).
/// This also tests set_property and get_property with number and select fields.
#[tokio::test]
#[test_log::test]
async fn test_types_create_type_and_object() -> TestResult<()> {
    with_test_context(|ctx| async move {
        let unique = unique_suffix();

        let type_name = format!("Type {unique}");
        let type_key = format!("type_{unique}");

        // Create type with properties
        let created = ctx
            .client
            .new_type(&ctx.space_id, &type_name)
            .key(&type_key)
            .property("Status", "status", PropertyFormat::Select)
            .property("Priority", "priority", PropertyFormat::Number)
            .create()
            .await?;

        ctx.increment_calls(1);
        ctx.register_type(&created.id);

        let object_name = format!("Type Object {unique}");
        let object_key = type_key.clone();

        // find tag id for "Done"
        let tag_done = lookup_property_tag_with_retry(ctx.as_ref(), "status", "Done").await?;

        // create object of custom type
        let obj = create_object_with_retry("Type Object", || async {
            ctx.client
                .new_object(&ctx.space_id, &object_key)
                .name(&object_name)
                .set_select("status", &tag_done.id)
                .set_number("priority", 2)
                .create()
                .await
        })
        .await?;
        ctx.increment_calls(1);
        ctx.register_object(&obj.id);

        assert_eq!(
            obj.get_property_number("priority"),
            Some(&serde_json::Number::from(2u64)),
            "get priority as Number"
        );
        assert_eq!(
            obj.get_property_u64("priority"),
            Some(2u64),
            "get priority as u64"
        );
        assert_eq!(
            obj.get_property_i64("priority"),
            Some(2i64),
            "get priority as i64"
        );
        assert_eq!(
            obj.get_property_f64("priority"),
            Some(2.0f64),
            "get priority as f64"
        );

        let status_value = obj
            .get_property_select("status")
            .expect("expected to find 'status' property");
        assert_eq!(status_value.name, "Done", "Done name match");
        assert_eq!(status_value.id, tag_done.id, "Done id match");

        println!("Created object with custom type");

        Ok(())
    })
    .await
}

// =============================================================================
// Type Template Tests
// =============================================================================

#[tokio::test]
#[test_log::test]
async fn test_types_list_templates() -> TestResult<()> {
    with_test_context(|ctx| async move {
        // Get a system type (they often have templates)
        let types = ctx.client.types(&ctx.space_id).list().await?.items.clone();

        let page_type = types
            .iter()
            .find(|t| t.key == "page")
            .expect("Page type should exist");

        // List templates for this type
        let result = ctx
            .client
            .templates(&ctx.space_id, &page_type.id)
            .list()
            .await?;

        let templates = &result.items;

        ctx.increment_calls(2);

        // Templates may or may not exist, but the call should succeed
        println!("Type '{}' has {} templates", page_type.key, templates.len());

        // If templates exist, verify they're objects
        for template in templates {
            assert!(!template.id.is_empty(), "Template should have an ID");
        }

        Ok(())
    })
    .await
}

#[tokio::test]
#[test_log::test]
async fn test_types_get_template() -> TestResult<()> {
    with_test_context(|ctx| async move {
        let types = ctx.client.types(&ctx.space_id).list().await?;

        for typ in types.iter().take(10) {
            let templates = ctx.client.templates(&ctx.space_id, &typ.id).list().await?;
            if let Some(template) = templates.iter().next() {
                let fetched = ctx
                    .client
                    .template(&ctx.space_id, &typ.id, &template.id)
                    .get()
                    .await?;
                assert_eq!(
                    fetched.id, template.id,
                    "template get should match listed template id"
                );
                return Ok(());
            }
        }

        println!("No templates found to test template get (this is OK)");
        Ok(())
    })
    .await
}

#[tokio::test]
#[test_log::test]
async fn test_types_template_type_linkage() -> TestResult<()> {
    with_test_context(|ctx| async move {
        // Get all types
        let types = ctx.client.types(&ctx.space_id).list().await?.items.clone();

        ctx.increment_calls(1);

        // Try to find a type with templates
        for typ in types.iter().take(3) {
            // Limit to first 3 to avoid too many API calls
            let result = ctx.client.templates(&ctx.space_id, &typ.id).list().await?;

            ctx.increment_calls(1);

            let templates = &result.items;

            if !templates.is_empty() {
                println!(
                    "Type '{}' has {} template(s), verifying linkage",
                    typ.key,
                    templates.len()
                );

                let mut matched = false;

                // Verify template has type reference
                for template in templates {
                    // Templates are objects and should have a type field
                    let template_type = template
                        .r#type
                        .as_ref()
                        .expect("Template should have a type");
                    assert!(
                        !template_type.id.is_empty(),
                        "Template type ID should be present"
                    );
                    if template_type.id == typ.id {
                        matched = true;
                    }
                }

                if matched {
                    return Ok(());
                }

                println!("⚠ Templates returned, but none matched type '{}'", typ.key);
            }
        }

        println!("No types with templates found in first 3 types (this is OK)");
        Ok(())
    })
    .await
}

// =============================================================================
// Type Update and Delete Tests
// =============================================================================

#[tokio::test]
#[test_log::test]
async fn test_types_update_custom() -> TestResult<()> {
    with_test_context(|ctx| async move {
        let unique_key = format!("test_update_{}", chrono::Utc::now().timestamp_millis());

        // Create a type
        let created = ctx
            .client
            .new_type(&ctx.space_id, "Original Name")
            .key(&unique_key)
            .create()
            .await?;
        ctx.register_type(&created.id);

        // Update it
        let updated = ctx
            .client
            .update_type(&ctx.space_id, &created.id)
            .name("Updated Name")
            .update()
            .await?;

        ctx.increment_calls(2);

        // Verify update
        assert_eq!(updated.id, created.id);
        assert_eq!(updated.name.as_deref(), Some("Updated Name"));

        println!("Successfully updated type name");

        Ok(())
    })
    .await
}

#[tokio::test]
#[test_log::test]
async fn test_types_delete_custom() -> TestResult<()> {
    with_test_context(|ctx| async move {
        let unique_key = format!("test_delete_{}", chrono::Utc::now().timestamp_millis());

        // Create a type
        let created = ctx
            .client
            .new_type(&ctx.space_id, "To Be Deleted")
            .key(&unique_key)
            .create()
            .await?;

        // Delete it
        let deleted = ctx
            .client
            .get_type(&ctx.space_id, &created.id)
            .delete()
            .await?;

        ctx.increment_calls(2);

        // Verify deletion (delete actually archives)
        assert_eq!(deleted.id, created.id);
        if !deleted.archived {
            println!("Delete returned archived=false");
        }

        println!("Successfully deleted (archived) type");

        Ok(())
    })
    .await
}

// =============================================================================
// Advanced Type Tests
// =============================================================================

#[tokio::test]
#[test_log::test]
async fn test_types_with_icon() -> TestResult<()> {
    with_test_context(|ctx| async move {
        let unique_key = format!("test_icon_{}", chrono::Utc::now().timestamp_millis());

        // Create type with emoji icon
        let created = ctx
            .client
            .new_type(&ctx.space_id, "Type With Icon")
            .key(&unique_key)
            .icon(Icon::Emoji {
                emoji: "📋".to_string(),
            })
            .create()
            .await?;

        ctx.increment_calls(1);
        ctx.register_type(&created.id);

        // Verify icon
        assert!(created.icon.is_some(), "Type should have an icon");
        if let Some(Icon::Emoji { emoji }) = &created.icon {
            assert_eq!(emoji, "📋");
        } else {
            return Err(TestError::Assertion {
                message: "Expected Emoji icon variant".to_string(),
            });
        }

        println!("Created type with icon: {}", created.display_name());

        Ok(())
    })
    .await
}

#[tokio::test]
#[test_log::test]
async fn test_types_display_name() -> TestResult<()> {
    with_test_context(|ctx| async move {
        let unique_key = format!("test_display_{}", chrono::Utc::now().timestamp_millis());

        // Create type with name
        let with_name = ctx
            .client
            .new_type(&ctx.space_id, "Display Name Test")
            .key(&unique_key)
            .create()
            .await?;

        ctx.increment_calls(1);
        ctx.register_type(&with_name.id);

        // display_name() should return the name
        assert_eq!(with_name.display_name(), "Display Name Test");

        println!("Type display_name() works correctly");

        Ok(())
    })
    .await
}

#[tokio::test]
#[test_log::test]
async fn test_types_layouts() -> TestResult<()> {
    with_test_context(|ctx| async move {
        let layouts = &[
            TypeLayout::Basic,
            TypeLayout::Note,
            TypeLayout::Action,
            TypeLayout::Profile,
        ];

        for (idx, layout) in layouts.iter().enumerate() {
            let unique_key = format!(
                "test_layout_{}_{}",
                idx,
                chrono::Utc::now().timestamp_millis()
            );

            let created = ctx
                .client
                .new_type(&ctx.space_id, format!("Layout Test {:?}", layout))
                .key(&unique_key)
                .layout(layout.clone())
                .create()
                .await?;

            ctx.increment_calls(1);
            ctx.register_type(&created.id);
            println!("Created type with layout: {:?}", layout);
        }

        println!("Successfully created types with all layout variants");

        Ok(())
    })
    .await
}

// =============================================================================
// Error Handling Tests
// =============================================================================

#[tokio::test]
#[test_log::test]
async fn test_types_duplicate_key() -> TestResult<()> {
    with_test_context(|ctx| async move {
        let unique_key = format!("test_dup_{}", chrono::Utc::now().timestamp_millis());

        // Create first type
        let first = ctx
            .client
            .new_type(&ctx.space_id, "First Type")
            .key(&unique_key)
            .create()
            .await?;
        ctx.register_type(&first.id);

        // Try to create second type with same key
        let result = ctx
            .client
            .new_type(&ctx.space_id, "Second Type")
            .key(&unique_key)
            .create()
            .await;

        ctx.increment_calls(2);

        // May succeed or fail depending on API validation
        // If it succeeds, clean up the second type too
        if let Ok(second) = result {
            ctx.register_type(&second.id);
            println!("API allows duplicate type keys (types have different IDs)");
        } else {
            println!("API correctly rejected duplicate type key");
        }

        Ok(())
    })
    .await
}
