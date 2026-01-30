//! Integration Tests for anytype
//!
//! Covers:
//! - Public API contracts for spaces, types, properties, objects, search, tags, members
//! - Getters and setters for all PropertyFormats
//! - Mutation tests for object create/update/delete with cleanup
//! - Search and filters
//! - Error handling, validation, and integration surface correctness
//! - Property formats and custom property types
//!
//! ## Running
//!
//! ```bash
//! source .test-env
//! cargo test -p anytype --test integration
//! ```

mod common;

// =============================================================================
// Property Format Getters and Setters
// =============================================================================

mod property_formats {
    use anytype::{prelude::*, test_util::*};

    use super::common::{unique_test_name /*with_test_context_unit*/};

    /// Test creating objects with various property formats
    #[tokio::test]
    #[test_log::test]
    async fn test_property_format_setters() {
        with_test_context_unit(|ctx| async move {
            let name = unique_test_name("Property Format Test");

            // Create object with various property values
            let obj = ctx
                .client
                .new_object(&ctx.space_id, "page")
                .name(&name)
                .description("Testing property format setters")
                .set_text("description", "Test description via set_text")
                .create()
                .await
                .expect("Failed to create object with properties");
            ctx.register_object(&obj.id);

            // Verify the object was created
            assert_eq!(obj.name.as_deref(), Some(name.as_str()));
        })
        .await
    }

    /// Test reading property values from objects
    #[tokio::test]
    #[test_log::test]
    async fn test_property_format_getters() {
        with_test_context_unit(|ctx| async move {
            // Get an existing object with properties
            let objects = ctx
                .client
                .objects(&ctx.space_id)
                .limit(1)
                .list()
                .await
                .expect("Failed to list objects");

            if let Some(obj) = objects.iter().next() {
                // Verify we can read properties
                for prop in &obj.properties {
                    // Property should have valid structure
                    assert!(!prop.id.is_empty(), "Property ID should not be empty");
                    assert!(!prop.key.is_empty(), "Property key should not be empty");

                    // Test value accessors based on format
                    match prop.format() {
                        PropertyFormat::Text => {
                            let _ = prop.value.as_str();
                        }
                        PropertyFormat::Number => {
                            let _ = prop.value.as_number();
                        }
                        PropertyFormat::Checkbox => {
                            let _ = prop.value.as_bool();
                        }
                        PropertyFormat::Date => {
                            let _ = prop.value.as_date();
                        }
                        PropertyFormat::Select => {
                            let _ = prop.value.as_str();
                        }
                        PropertyFormat::MultiSelect
                        | PropertyFormat::Objects
                        | PropertyFormat::Files => {
                            let _ = prop.value.as_array();
                        }
                        _ => {}
                    }
                }
            }
        })
        .await
    }

    /// Test all property format types are recognized
    #[tokio::test]
    #[test_log::test]
    async fn test_property_format_coverage() {
        with_test_context_unit(|ctx| async move {
            let properties = ctx
                .client
                .properties(&ctx.space_id)
                .list()
                .await
                .expect("Failed to list properties");

            // Collect unique formats
            let mut found_formats: Vec<PropertyFormat> = Vec::new();
            for prop in properties.iter() {
                if !found_formats.iter().any(|f| *f == prop.format()) {
                    found_formats.push(prop.format());
                }
            }

            eprintln!("Found {} unique property formats:", found_formats.len());
            for format in &found_formats {
                eprintln!("  - {:?}", format);
            }

            // Should have at least Text format
            assert!(
                found_formats.contains(&PropertyFormat::Text),
                "Should have Text format"
            );
        })
        .await
    }
}

// =============================================================================
// Object CRUD Mutations
// =============================================================================

mod object_crud {
    use anytype::test_util::*;

    use super::common::unique_test_name;

    /// Test full object lifecycle: create, read, update, delete
    #[tokio::test]
    #[test_log::test]
    async fn test_object_crud_lifecycle() {
        with_test_context_unit(|ctx| async move {
            let original_name = unique_test_name("CRUD Test");
            let updated_name = format!("{} (Updated)", original_name);

            // CREATE
            let created = ctx
                .client
                .new_object(&ctx.space_id, "page")
                .name(&original_name)
                .body("# Test Content\n\nThis is test content.")
                .description("Created for CRUD test")
                .create()
                .await
                .expect("Failed to create object");
            ctx.register_object(&created.id);

            assert_eq!(created.name.as_deref(), Some(original_name.as_str()));
            let object_id = created.id.clone();

            // READ
            let read = ctx
                .client
                .object(&ctx.space_id, &object_id)
                .get()
                .await
                .expect("Failed to read object");
            assert_eq!(read.id, object_id);
            assert_eq!(read.name.as_deref(), Some(original_name.as_str()));

            // UPDATE
            let updated = ctx
                .client
                .update_object(&ctx.space_id, &object_id)
                .name(&updated_name)
                .body("# Updated Content\n\nThis content was updated.")
                .update()
                .await
                .expect("Failed to update object");
            assert_eq!(updated.name.as_deref(), Some(updated_name.as_str()));

            // Verify update persisted
            let verified = ctx
                .client
                .object(&ctx.space_id, &object_id)
                .get()
                .await
                .expect("Failed to verify update");
            assert_eq!(verified.name.as_deref(), Some(updated_name.as_str()));

            // DELETE (archive)
            let deleted = ctx
                .client
                .object(&ctx.space_id, &object_id)
                .delete()
                .await
                .expect("Failed to delete object");
            // Note: archived status may not be immediately reflected
            eprintln!("Delete returned archived={}", deleted.archived);
        })
        .await
    }

    /// Test creating multiple objects and batch operations
    #[tokio::test]
    #[test_log::test]
    async fn test_create_multiple_objects() {
        with_test_context_unit(|ctx| async move {
            let base_name = unique_test_name("Multi");
            let mut created_ids = Vec::new();

            // Create 3 objects
            for i in 1..=3 {
                let name = format!("{} Object {}", base_name, i);
                let obj = ctx
                    .client
                    .new_object(&ctx.space_id, "page")
                    .name(&name)
                    .create()
                    .await
                    .expect("Failed to create object");
                ctx.register_object(&obj.id);
                created_ids.push(obj.id);
            }

            assert_eq!(created_ids.len(), 3, "Should have created 3 objects");

            // Verify all can be read
            for id in &created_ids {
                let obj = ctx
                    .client
                    .object(&ctx.space_id, id)
                    .get()
                    .await
                    .expect("Failed to read created object");
                assert_eq!(&obj.id, id);
            }
        })
        .await
    }

    /// Test object body content handling
    #[tokio::test]
    #[test_log::test]
    async fn test_object_body_content() {
        with_test_context_unit(|ctx| async move {
            let name = unique_test_name("Body Content Test");
            let body_content = r#"# Heading 1

        This is a paragraph with **bold** and *italic* text.

        ## Heading 2

        - List item 1
        - List item 2
        - List item 3

        ```rust
        fn main() {
            println!("Hello, world!");
        }
        ```
        "#;

            // Create with body
            let obj = ctx
                .client
                .new_object(&ctx.space_id, "page")
                .name(&name)
                .body(body_content)
                .create()
                .await
                .expect("Failed to create object with body");
            ctx.register_object(&obj.id);

            // Read back and verify content
            let read = ctx
                .client
                .object(&ctx.space_id, &obj.id)
                .get()
                .await
                .expect("Failed to read object");

            // Content should be present as markdown or snippet
            assert!(
                read.markdown.is_some() || read.snippet.is_some(),
                "Should have content (markdown or snippet)"
            );
        })
        .await
    }
}

// =============================================================================
// Search and Filters
// =============================================================================

mod search_and_filters {
    use std::time::Duration;

    use anytype::{prelude::*, test_util::*};

    /// Test global search functionality
    #[tokio::test]
    #[test_log::test]
    async fn test_global_search() {
        with_test_context_unit(|ctx| async move {
            let results = ctx
                .client
                .search_global()
                .limit(10)
                .execute()
                .await
                .expect("Failed to execute global search");

            // Global search should return results
            eprintln!("Global search returned {} results", results.len());
        })
        .await
    }

    /// Test search within a specific space
    #[tokio::test]
    #[test_log::test]
    async fn test_space_search() {
        with_test_context_unit(|ctx| async move {
            let results = ctx
                .client
                .search_in(&ctx.space_id)
                .limit(10)
                .execute()
                .await
                .expect("Failed to execute space search");

            eprintln!("Space search returned {} results", results.len());

            // All results should be from the specified space
            for obj in &results {
                assert_eq!(
                    obj.space_id, ctx.space_id,
                    "Result should be from search space"
                );
            }
        })
        .await
    }

    /// Test search with text query
    #[tokio::test]
    #[test_log::test]
    async fn test_search_with_text() {
        with_test_context_unit(|ctx| async move {
            // Create a uniquely named object to search for
            let unique_term = format!("SearchTest{}", chrono::Utc::now().timestamp_millis());
            let obj = ctx
                .client
                .new_object(&ctx.space_id, "page")
                .name(&unique_term)
                .create()
                .await
                .expect("Failed to create searchable object");
            ctx.register_object(&obj.id);

            // Small delay to allow indexing
            tokio::time::sleep(Duration::from_millis(100)).await;

            // Search for the unique term
            let results = ctx
                .client
                .search_in(&ctx.space_id)
                .text(&unique_term)
                .execute()
                .await
                .expect("Failed to execute text search");

            eprintln!(
                "Text search for '{}' returned {} results",
                unique_term,
                results.len()
            );
        })
        .await
    }

    /// Test search with type filter
    #[tokio::test]
    #[test_log::test]
    async fn test_search_with_type_filter() {
        with_test_context_unit(|ctx| async move {
            let results = ctx
                .client
                .search_in(&ctx.space_id)
                .types(["page"])
                .limit(10)
                .execute()
                .await
                .expect("Failed to execute type-filtered search");

            // All results should be of type "page"
            for obj in &results {
                if let Some(ref typ) = obj.r#type {
                    assert_eq!(typ.key, "page", "Result should be of type 'page'");
                }
            }
        })
        .await
    }

    /// Test object list with filters
    #[tokio::test]
    #[test_log::test]
    async fn test_object_list_filters() {
        with_test_context_unit(|ctx| async move {
            // Test not_empty filter
            let results = ctx
                .client
                .objects(&ctx.space_id)
                .filter(Filter::not_empty("name"))
                .limit(5)
                .list()
                .await
                .expect("Failed to list with not_empty filter");

            eprintln!("not_empty(name) returned {} results", results.len());

            // All results should have non-empty names
            for obj in results.iter() {
                assert!(
                    obj.name.as_ref().map(|n| !n.is_empty()).unwrap_or(false),
                    "Object should have non-empty name"
                );
            }
        })
        .await
    }

    /// Test object list with is_empty filter
    #[tokio::test]
    #[test_log::test]
    async fn test_object_list_empty_filter() {
        with_test_context_unit(|ctx| async move {
            // Test is_empty filter on description
            let results = ctx
                .client
                .objects(&ctx.space_id)
                .filter(Filter::is_empty("description"))
                .limit(5)
                .list()
                .await
                .expect("Failed to list with is_empty filter");

            eprintln!("is_empty(description) returned {} results", results.len());
        })
        .await
    }
}

// =============================================================================
// Error Handling and Validation
// =============================================================================

mod error_handling {
    use anytype::{prelude::*, test_util::*};

    /// Test that invalid space ID returns appropriate error
    #[tokio::test]
    #[test_log::test]
    async fn test_invalid_space_id() {
        with_test_context_unit(|ctx| async move {
            let result = ctx.client.space("invalid-space-id-12345").get().await;

            match result {
                Err(AnytypeError::NotFound { obj_type, .. }) if &obj_type == "Space" => {
                    eprintln!("Correctly received NotFound for invalid space");
                }
                Err(AnytypeError::Validation { .. }) => {
                    eprintln!("Correctly received Validation error for invalid space");
                }
                Err(e) => {
                    eprintln!("Received error: {:?}", e);
                    // Accept any error for invalid space
                }
                Ok(_) => {
                    panic!("Expected error for invalid space ID");
                }
            }
        })
        .await
    }

    /// Test that invalid object ID returns appropriate error
    #[tokio::test]
    #[test_log::test]
    async fn test_invalid_object_id() {
        with_test_context_unit(|ctx| async move {
            let result = ctx
                .client
                .object(&ctx.space_id, "invalid-object-id-12345")
                .get()
                .await;

            match result {
                Err(AnytypeError::NotFound { obj_type, .. }) if &obj_type == "Object" => {
                    eprintln!("Correctly received NotFound for invalid object");
                }
                Err(AnytypeError::Validation { .. }) => {
                    eprintln!("Correctly received Validation error for invalid object");
                }
                Err(e) => {
                    eprintln!("Received error: {:?}", e);
                    // Accept any error for invalid object
                }
                Ok(_) => {
                    panic!("Expected error for invalid object ID");
                }
            }
        })
        .await
    }

    /// Test that invalid property ID returns appropriate error
    #[tokio::test]
    #[test_log::test]
    async fn test_invalid_property_id() {
        with_test_context_unit(|ctx| async move {
            let result = ctx
                .client
                .property(&ctx.space_id, "invalid-property-id-12345")
                .get()
                .await;

            match result {
                Err(AnytypeError::NotFound { obj_type, .. }) if &obj_type == "Property" => {
                    eprintln!("Correctly received NotFound for invalid property");
                }
                Err(AnytypeError::Validation { .. }) => {
                    eprintln!("Correctly received Validation error for invalid property");
                }
                Err(e) => {
                    eprintln!("Received error: {:?}", e);
                }
                Ok(_) => {
                    panic!("Expected error for invalid property ID");
                }
            }
        })
        .await
    }

    /// Test that invalid type ID returns appropriate error
    #[tokio::test]
    #[test_log::test]
    async fn test_invalid_type_id() {
        with_test_context_unit(|ctx| async move {
            let result = ctx
                .client
                .get_type(&ctx.space_id, "invalid-type-id-12345")
                .get()
                .await;

            match result {
                Err(AnytypeError::NotFound { obj_type, .. }) if &obj_type == "Type" => {
                    eprintln!("Correctly received NotFound for invalid type");
                }
                Err(AnytypeError::Validation { .. }) => {
                    eprintln!("Correctly received Validation error for invalid type");
                }
                Err(e) => {
                    eprintln!("Received error: {:?}", e);
                }
                Ok(_) => {
                    panic!("Expected error for invalid type ID");
                }
            }
        })
        .await
    }

    /// Test update without changes returns validation error
    #[tokio::test]
    #[test_log::test]
    async fn test_update_without_changes() {
        with_test_context_unit(|ctx| async move {
            // Get an existing object
            let objects = ctx
                .client
                .objects(&ctx.space_id)
                .limit(1)
                .list()
                .await
                .expect("Failed to list objects");

            if let Some(obj) = objects.iter().next() {
                // Try to update without setting any fields
                let result = ctx
                    .client
                    .update_object(&ctx.space_id, &obj.id)
                    .update()
                    .await;

                match result {
                    Err(AnytypeError::Validation { message }) => {
                        eprintln!("Correctly received validation error: {}", message);
                        assert!(
                            message.contains("must set at least one field"),
                            "Error should mention missing fields"
                        );
                    }
                    Err(e) => {
                        eprintln!("Received error: {:?}", e);
                    }
                    Ok(_) => {
                        panic!("Expected validation error for update without changes");
                    }
                }
            }
        })
        .await
    }

    /// Test that empty name is handled correctly
    #[tokio::test]
    #[test_log::test]
    async fn test_create_with_empty_name() {
        with_test_context_unit(|ctx| async move {
            // Creating with empty name may succeed (for Note types) or fail
            let result = ctx
                .client
                .new_object(&ctx.space_id, "page")
                .name("")
                .create()
                .await;

            match result {
                Ok(obj) => {
                    ctx.register_object(&obj.id);
                    eprintln!("Object created with empty name (ID: {})", obj.id);
                }
                Err(e) => {
                    eprintln!("Create with empty name failed (expected): {:?}", e);
                }
            }
        })
        .await
    }
}

// =============================================================================
// Property Formats and Custom Property Types
// =============================================================================

mod custom_properties {
    use std::collections::HashMap;

    use anytype::{prelude::*, test_util::*};
    use common::unique_test_name;

    use super::*;

    /// Test property key stability across list and get
    #[tokio::test]
    #[test_log::test]
    async fn test_property_key_stability() {
        with_test_context_unit(|ctx| async move {
            let properties = ctx
                .client
                .properties(&ctx.space_id)
                .list()
                .await
                .expect("Failed to list properties");

            // For each property, verify get returns same key
            for prop in properties.iter().take(5) {
                let fetched = ctx
                    .client
                    .property(&ctx.space_id, &prop.id)
                    .get()
                    .await
                    .expect("Failed to get property");

                assert_eq!(prop.key, fetched.key, "Property key should be stable");
                assert_eq!(
                    prop.format(),
                    fetched.format(),
                    "Property format should be stable"
                );
            }
        })
        .await
    }

    /// Test creating custom property
    #[tokio::test]
    #[test_log::test]
    async fn test_create_custom_property() {
        with_test_context_unit(|ctx| async move {
            let prop_name = unique_test_name("CustomProp");
            let prop_key = format!("custom_prop_{}", chrono::Utc::now().timestamp_millis());

            let result = ctx
                .client
                .new_property(&ctx.space_id, &prop_name, PropertyFormat::Text)
                .key(&prop_key)
                .create()
                .await;

            match result {
                Ok(prop) => {
                    ctx.register_property(&prop.id);
                    eprintln!("Created custom property: {} ({})", prop.name, prop.id);
                    assert_eq!(prop.name, prop_name);
                    assert_eq!(prop.format(), PropertyFormat::Text);
                }
                Err(e) => {
                    eprintln!("Failed to create custom property: {:?}", e);
                    // May fail if property already exists
                }
            }
        })
        .await
    }

    /// Test property format enum completeness
    #[tokio::test]
    #[test_log::test]
    async fn test_property_format_enum() {
        with_test_context_unit(|ctx| async move {
            let properties = ctx
                .client
                .properties(&ctx.space_id)
                .list()
                .await
                .expect("Failed to list properties");

            assert!(!properties.is_empty());

            // Track which formats we've seen
            let mut format_counts: HashMap<String, usize> = HashMap::new();

            for prop in properties.iter() {
                let format_name = format!("{:?}", prop.format());
                *format_counts.entry(format_name).or_insert(0) += 1;
            }

            eprintln!("Property format distribution:");
            for (format, count) in &format_counts {
                eprintln!("  {}: {}", format, count);
            }

            // Verify at least some formats are present
            assert!(!format_counts.is_empty(), "Should have at least one format");
        })
        .await
    }
}

// =============================================================================
// Pagination Tests
// =============================================================================

mod pagination {

    use anytype::test_util::*;
    use serial_test::serial;

    /// Test pagination limit is respected
    #[tokio::test]
    #[test_log::test]
    #[serial]
    async fn test_pagination_limit() {
        with_test_context_unit(|ctx| async move {
            let limit = 3;
            let results = ctx
                .client
                .objects(&ctx.space_id)
                .limit(limit)
                .list()
                .await
                .expect("Failed to list with limit");

            assert!(
                results.len() <= limit as usize,
                "Results should respect limit: got {} for limit {}",
                results.len(),
                limit
            );
        })
        .await
    }

    /// Test pagination offset
    #[tokio::test]
    #[test_log::test]
    #[serial]
    #[ignore]
    async fn test_pagination_offset() {
        with_test_context_unit(|ctx| async move {
            // Get first page
            let page1 = ctx
                .client
                .objects(&ctx.space_id)
                .limit(2)
                .offset(0)
                .list()
                .await
                .expect("Failed to get page 1");

            // Get second page
            let page2 = ctx
                .client
                .objects(&ctx.space_id)
                .limit(2)
                .offset(2)
                .list()
                .await
                .expect("Failed to get page 2");

            // Pages should be different (if enough data exists)
            if !page1.is_empty() && !page2.is_empty() {
                let page1_ids: Vec<&str> = page1.iter().map(|o| o.id.as_str()).collect();
                let page2_ids: Vec<&str> = page2.iter().map(|o| o.id.as_str()).collect();

                for id in &page2_ids {
                    assert!(
                        !page1_ids.contains(id),
                        "Page 2 should not contain items from page 1"
                    );
                }
            }
        })
        .await
    }

    /// Test collect_all for pagination
    #[tokio::test]
    #[test_log::test]
    #[serial]
    async fn test_collect_all() {
        with_test_context_unit(|ctx| async move {
            // Use small limit to force pagination
            let all_objects = ctx
                .client
                .objects(&ctx.space_id)
                .limit(5)
                .list()
                .await
                .expect("Failed to list objects")
                .collect_all()
                .await
                .expect("Failed to collect all");

            eprintln!("collect_all returned {} objects", all_objects.len());
        })
        .await
    }
}
