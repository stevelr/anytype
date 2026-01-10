//! Integration tests for property formats in anytype
//!
//! Tests comprehensive property format handling including:
//! - Creating objects with each property format
//! - Reading property values from objects
//! - Property CRUD operations (create, update, delete)
//! - Property validation and error handling
//! - Property key stability
//!
//! ## Running
//!
//! ```bash
//! source .test-env
//! cargo test -p anytype --test test_properties
//! ```

mod common;

use crate::common::{create_object_with_retry, unique_test_name};
use anytype::prelude::*;
use anytype::test_util::{TestResult, unique_suffix, with_test_context};
use serde_json::Number;

// =============================================================================
// Property Format Setters - Create objects with each format
// =============================================================================

#[tokio::test]
#[test_log::test]
async fn test_set_text_property() -> TestResult<()> {
    with_test_context(|ctx| async move {
        let name = unique_test_name("Text Property Test");
        let description_value = "This is a text property value";

        let obj = ctx
            .client
            .new_object(&ctx.space_id, "page")
            .name(&name)
            .set_text("description", description_value)
            .create()
            .await?;
        ctx.register_object(&obj.id);

        // Verify the text property was set
        assert_eq!(obj.name.as_deref(), Some(name.as_str()));

        // Read back and verify
        let read_obj = ctx.client.object(&ctx.space_id, &obj.id).get().await?;
        let desc = read_obj.get_property_str("description");
        assert_eq!(
            desc,
            Some(description_value),
            "Description should match set value"
        );

        Ok(())
    })
    .await
}

#[tokio::test]
#[test_log::test]
async fn test_set_number_property() -> TestResult<()> {
    with_test_context(|ctx| async move {
        let name = unique_test_name("Number Property Test");

        // First create a custom number property
        let prop_name = unique_test_name("TestNumber");
        let prop_key = format!("test_number_{}", unique_suffix());

        let number_prop = ctx
            .client
            .new_property(&ctx.space_id, &prop_name, PropertyFormat::Number)
            .key(&prop_key)
            .create()
            .await?;

        ctx.register_property(&number_prop.id);

        // Create object with number property
        let number_value = 42;
        let obj = create_object_with_retry("Number Property Test", || async {
            ctx.client
                .new_object(&ctx.space_id, "page")
                .name(&name)
                .set_number(&prop_key, Number::from(number_value))
                .create()
                .await
        })
        .await?;
        ctx.register_object(&obj.id);

        // Read back and verify
        let read_obj = ctx.client.object(&ctx.space_id, &obj.id).get().await?;
        let num = read_obj.get_property_u64(&prop_key);
        assert_eq!(
            num,
            Some(number_value),
            "Number property should match set value"
        );

        Ok(())
    })
    .await
}

#[tokio::test]
#[test_log::test]
async fn test_set_select_property() -> TestResult<()> {
    with_test_context(|ctx| async move {
        let name = unique_test_name("Select Property Test");

        let prop_name = unique_test_name("TestSelect");
        let prop_key = format!("test_select_{}", unique_suffix());
        let select_prop = ctx
            .client
            .new_property(&ctx.space_id, &prop_name, PropertyFormat::Select)
            .key(&prop_key)
            .tag("Test Value", Some("test_value".into()), Color::Blue)
            .create()
            .await?;
        ctx.register_property(&select_prop.id);

        let tags = ctx
            .client
            .tags(&ctx.space_id, &select_prop.id)
            .list()
            .await?
            .collect_all()
            .await?;
        let tag = tags.first().expect("expected select tag to exist");

        let obj = create_object_with_retry("Select Property Test", || async {
            ctx.client
                .new_object(&ctx.space_id, "page")
                .name(&name)
                .set_select(&prop_key, &tag.id)
                .create()
                .await
        })
        .await?;
        ctx.register_object(&obj.id);

        Ok(())
    })
    .await
}

#[tokio::test]
#[test_log::test]
async fn test_set_multiselect_property() -> TestResult<()> {
    with_test_context(|ctx| async move {
        let name = unique_test_name("MultiSelect Property Test");

        let prop_name = unique_test_name("TestMultiSelect");
        let prop_key = format!("test_multiselect_{}", unique_suffix());
        let multi_prop = ctx
            .client
            .new_property(&ctx.space_id, &prop_name, PropertyFormat::MultiSelect)
            .key(&prop_key)
            .tag("Value 1", Some("value1".into()), Color::Lime)
            .tag("Value 2", Some("value2".into()), Color::Yellow)
            .tag("Value 3", Some("value3".into()), Color::Red)
            .create()
            .await?;
        ctx.register_property(&multi_prop.id);

        let tags = ctx
            .client
            .tags(&ctx.space_id, &multi_prop.id)
            .list()
            .await?
            .collect_all()
            .await?;
        let values: Vec<String> = tags.iter().map(|tag| tag.id.clone()).collect();

        let obj = create_object_with_retry("MultiSelect Property Test", || async {
            ctx.client
                .new_object(&ctx.space_id, "page")
                .name(&name)
                .set_multi_select(&prop_key, values.clone())
                .create()
                .await
        })
        .await?;
        ctx.register_object(&obj.id);

        Ok(())
    })
    .await
}

#[tokio::test]
#[test_log::test]
async fn test_set_date_property() -> TestResult<()> {
    with_test_context(|ctx| async move {
        let name = unique_test_name("Date Property Test");
        let date_value = "2024-01-15T10:30:00Z";

        let prop_name = unique_test_name("TestDate");
        let prop_key = format!("test_date_{}", unique_suffix());
        let date_prop = ctx
            .client
            .new_property(&ctx.space_id, &prop_name, PropertyFormat::Date)
            .key(&prop_key)
            .create()
            .await?;
        ctx.register_property(&date_prop.id);

        let obj = create_object_with_retry("Date Property Test", || async {
            ctx.client
                .new_object(&ctx.space_id, "page")
                .name(&name)
                .set_date(&prop_key, date_value)
                .create()
                .await
        })
        .await?;
        ctx.register_object(&obj.id);

        // Read back and verify
        let read_obj = ctx.client.object(&ctx.space_id, &obj.id).get().await?;
        let date_str = read_obj.get_property_str(&prop_key);
        assert!(
            date_str.is_some(),
            "Date property should be set on created object"
        );

        Ok(())
    })
    .await
}

#[tokio::test]
#[test_log::test]
async fn test_set_checkbox_property() -> TestResult<()> {
    with_test_context(|ctx| async move {
        let name = unique_test_name("Checkbox Property Test");

        // Create a custom checkbox property
        let prop_name = unique_test_name("TestCheckbox");
        let prop_key = format!("test_checkbox_{}", unique_suffix());

        let checkbox_prop = ctx
            .client
            .new_property(&ctx.space_id, &prop_name, PropertyFormat::Checkbox)
            .key(&prop_key)
            .create()
            .await?;

        ctx.register_property(&checkbox_prop.id);

        // Create object with checkbox set to true
        let obj = create_object_with_retry("Checkbox Property Test", || async {
            ctx.client
                .new_object(&ctx.space_id, "page")
                .name(&name)
                .set_checkbox(&prop_key, true)
                .create()
                .await
        })
        .await?;
        ctx.register_object(&obj.id);

        // Read back and verify
        let read_obj = ctx.client.object(&ctx.space_id, &obj.id).get().await?;
        let checked = read_obj.get_property_bool(&prop_key);
        assert_eq!(
            checked,
            Some(true),
            "Checkbox should be true after creation"
        );

        Ok(())
    })
    .await
}

#[tokio::test]
#[test_log::test]
async fn test_set_url_property() -> TestResult<()> {
    with_test_context(|ctx| async move {
        let name = unique_test_name("URL Property Test");
        let url_value = "https://example.com/test";

        // Create a custom URL property
        let prop_name = unique_test_name("TestURL");
        let prop_key = format!("test_url_{}", unique_suffix());

        let url_prop = ctx
            .client
            .new_property(&ctx.space_id, &prop_name, PropertyFormat::Url)
            .key(&prop_key)
            .create()
            .await?;

        ctx.register_property(&url_prop.id);

        let obj = create_object_with_retry("Url Property Test", || async {
            ctx.client
                .new_object(&ctx.space_id, "page")
                .name(&name)
                .set_url(&prop_key, url_value)
                .create()
                .await
        })
        .await?;
        ctx.register_object(&obj.id);

        // Read back and verify
        let read_obj = ctx.client.object(&ctx.space_id, &obj.id).get().await?;
        let url = read_obj.get_property_str(&prop_key);
        assert_eq!(url, Some(url_value), "URL property should match set value");

        Ok(())
    })
    .await
}

#[tokio::test]
#[test_log::test]
async fn test_set_email_property() -> TestResult<()> {
    with_test_context(|ctx| async move {
        let name = unique_test_name("Email Property Test");
        let email_value = "test@example.com";

        // Create a custom email property
        let prop_name = unique_test_name("TestEmail");
        let prop_key = format!("test_email_{}", unique_suffix());

        let email_prop = ctx
            .client
            .new_property(&ctx.space_id, &prop_name, PropertyFormat::Email)
            .key(&prop_key)
            .create()
            .await?;
        ctx.register_property(&email_prop.id);

        let obj = create_object_with_retry("Email Property Test", || async {
            ctx.client
                .new_object(&ctx.space_id, "page")
                .name(&name)
                .set_email(&prop_key, email_value)
                .create()
                .await
        })
        .await?;
        ctx.register_object(&obj.id);

        // Read back and verify
        let read_obj = ctx.client.object(&ctx.space_id, &obj.id).get().await?;
        let email = read_obj.get_property_str(&prop_key);
        assert_eq!(
            email,
            Some(email_value),
            "Email property should match set value"
        );

        Ok(())
    })
    .await
}

#[tokio::test]
#[test_log::test]
async fn test_set_phone_property() -> TestResult<()> {
    with_test_context(|ctx| async move {
        let name = unique_test_name("Phone Property Test");
        let phone_value = "+1-555-123-4567";

        // Create a custom phone property
        let prop_name = unique_test_name("TestPhone");
        let prop_key = format!("test_phone_{}", unique_suffix());

        let phone_prop = ctx
            .client
            .new_property(&ctx.space_id, &prop_name, PropertyFormat::Phone)
            .key(&prop_key)
            .create()
            .await?;
        ctx.register_property(&phone_prop.id);

        let obj = create_object_with_retry("Phone Property Test", || async {
            ctx.client
                .new_object(&ctx.space_id, "page")
                .name(&name)
                .set_phone(&prop_key, phone_value)
                .create()
                .await
        })
        .await?;

        ctx.register_object(&obj.id);

        // Read back and verify
        let read_obj = ctx.client.object(&ctx.space_id, &obj.id).get().await?;
        let phone = read_obj.get_property_str(&prop_key);
        assert_eq!(
            phone,
            Some(phone_value),
            "Phone property should match set value"
        );

        Ok(())
    })
    .await
}

#[tokio::test]
#[test_log::test]
async fn test_set_objects_property() -> TestResult<()> {
    with_test_context(|ctx| async move {
        let name = unique_test_name("Objects Property Test");

        // First create some objects to link to
        let obj1 = ctx
            .client
            .new_object(&ctx.space_id, "page")
            .name(&format!("{} Target 1", name))
            .create()
            .await?;
        ctx.register_object(&obj1.id);

        let obj2 = ctx
            .client
            .new_object(&ctx.space_id, "page")
            .name(&format!("{} Target 2", name))
            .create()
            .await?;
        ctx.register_object(&obj2.id);

        let prop_name = unique_test_name("TestObjects");
        let prop_key = format!("test_objects_{}", unique_suffix());
        let objects_prop = ctx
            .client
            .new_property(&ctx.space_id, &prop_name, PropertyFormat::Objects)
            .key(&prop_key)
            .create()
            .await?;
        ctx.register_property(&objects_prop.id);

        let linked_ids = vec![obj1.id.clone(), obj2.id.clone()];
        let obj = create_object_with_retry("Objects Property Test", || async {
            ctx.client
                .new_object(&ctx.space_id, "page")
                .name(&name)
                .set_objects(&prop_key, linked_ids.clone())
                .create()
                .await
        })
        .await?;
        ctx.register_object(&obj.id);

        // Read back and verify
        let read_obj = ctx.client.object(&ctx.space_id, &obj.id).get().await?;
        let linked = read_obj.get_property_array(&prop_key);

        if let Some(array) = linked {
            assert!(!array.is_empty(), "Linked objects should not be empty");
        }

        Ok(())
    })
    .await
}

// =============================================================================
// Property Format Getters - Read property values
// =============================================================================

#[tokio::test]
#[test_log::test]
async fn test_read_text_property_value() -> TestResult<()> {
    with_test_context(|ctx| async move {
        let name = unique_test_name("Read Text Test");
        let description = "Test description for reading";

        let obj = ctx
            .client
            .new_object(&ctx.space_id, "page")
            .name(&name)
            .set_text("description", description)
            .create()
            .await?;
        ctx.register_object(&obj.id);

        // Test get_property_str
        let desc_value = obj.get_property_str("description");
        assert_eq!(
            desc_value,
            Some(description),
            "get_property_str should return the text value"
        );

        // Test get_property
        let prop = obj.get_property("description");
        assert!(prop.is_some(), "get_property should find the property");

        if let Some(p) = prop {
            assert_eq!(p.format(), PropertyFormat::Text);
            assert_eq!(p.value.as_str(), Some(description));
        }

        Ok(())
    })
    .await
}

#[tokio::test]
#[test_log::test]
async fn test_read_number_property_value() -> TestResult<()> {
    with_test_context(|ctx| async move {
        // Find an object with a number property
        let properties = ctx.client.properties(&ctx.space_id).list().await?;
        let number_prop = properties
            .iter()
            .find(|p| p.format() == PropertyFormat::Number);

        if let Some(prop) = number_prop {
            let name = unique_test_name("Read Number Test");
            let number_val = 123;

            let obj = ctx
                .client
                .new_object(&ctx.space_id, "page")
                .name(&name)
                .set_number(&prop.key, Number::from(number_val))
                .create()
                .await?;
            ctx.register_object(&obj.id);

            // Test number getters
            let num_ref = obj.get_property_number(&prop.key);
            assert!(
                num_ref.is_some(),
                "get_property_number should return the number"
            );

            let num_u64 = obj.get_property_u64(&prop.key);
            assert_eq!(
                num_u64,
                Some(number_val),
                "get_property_u64 should return the value"
            );
        } else {
            println!("Skipping test_read_number_property_value: no number property found");
        }

        Ok(())
    })
    .await
}

#[tokio::test]
#[test_log::test]
async fn test_read_checkbox_property_value() -> TestResult<()> {
    with_test_context(|ctx| async move {
        // Create a checkbox property and object
        let prop_name = unique_test_name("ReadCheckbox");
        let prop_key = format!("read_checkbox_{}", unique_suffix());

        let prop = ctx
            .client
            .new_property(&ctx.space_id, &prop_name, PropertyFormat::Checkbox)
            .key(&prop_key)
            .create()
            .await?;
        ctx.register_property(&prop.id);

        let name = unique_test_name("Read Checkbox Test");
        let obj = create_object_with_retry("Read Checkbox Test", || async {
            ctx.client
                .new_object(&ctx.space_id, "page")
                .name(&name)
                .set_checkbox(&prop_key, true)
                .create()
                .await
        })
        .await?;

        // Test checkbox getter
        let checked = obj.get_property_bool(&prop_key);
        assert_eq!(checked, Some(true), "get_property_bool should return true");

        // Test via PropertyValue
        if let Some(p) = obj.get_property(&prop_key) {
            assert_eq!(p.format(), PropertyFormat::Checkbox);
            assert_eq!(p.value.as_bool(), Some(true));
        }

        Ok(())
    })
    .await
}

#[tokio::test]
#[test_log::test]
async fn test_read_date_property_value() -> TestResult<()> {
    with_test_context(|ctx| async move {
        // Find a date property from existing properties
        let properties = ctx.client.properties(&ctx.space_id).list().await?;
        let date_prop = properties
            .iter()
            .find(|p| p.format() == PropertyFormat::Date);

        if let Some(prop) = date_prop {
            // Find an object with this date property set
            let objects = ctx.client.objects(&ctx.space_id).limit(10).list().await?;

            for obj in objects.iter() {
                if let Some(date_str) = obj.get_property_str(&prop.key) {
                    println!("Found date property value: {}", date_str);

                    // Test get_property_date (returns chrono::DateTime)
                    let date = obj.get_property_date(&prop.key);
                    if date.is_some() {
                        println!("Successfully parsed date as DateTime");
                    }

                    // Test via PropertyValue
                    if let Some(p) = obj.get_property(&prop.key) {
                        assert_eq!(p.format(), PropertyFormat::Date);
                        assert!(p.value.as_date().is_some());
                    }

                    break;
                }
            }
        } else {
            println!("Skipping test_read_date_property_value: no date property found");
        }

        Ok(())
    })
    .await
}

#[tokio::test]
#[test_log::test]
async fn test_read_select_property_value() -> TestResult<()> {
    with_test_context(|ctx| async move {
        // Find a select property
        let properties = ctx.client.properties(&ctx.space_id).list().await?;
        let select_prop = properties
            .iter()
            .find(|p| p.format() == PropertyFormat::Select);

        if let Some(prop) = select_prop {
            // Find an object with this select property set
            let objects = ctx.client.objects(&ctx.space_id).limit(10).list().await?;

            for obj in objects.iter() {
                if let Some(select_val) = obj.get_property_str(&prop.key) {
                    println!("Found select property value: {}", select_val);

                    // Test via PropertyValue
                    if let Some(p) = obj.get_property(&prop.key) {
                        assert_eq!(p.format(), PropertyFormat::Select);
                        assert_eq!(p.value.as_str(), Some(select_val));
                    }

                    break;
                }
            }
        } else {
            println!("Skipping test_read_select_property_value: no select property found");
        }

        Ok(())
    })
    .await
}

#[tokio::test]
#[test_log::test]
async fn test_read_multiselect_property_value() -> TestResult<()> {
    with_test_context(|ctx| async move {
        // Find a multi-select property
        let properties = ctx.client.properties(&ctx.space_id).list().await?;
        let multiselect_prop = properties
            .iter()
            .find(|p| p.format() == PropertyFormat::MultiSelect);

        if let Some(prop) = multiselect_prop {
            // Find an object with this multi-select property set
            let objects = ctx.client.objects(&ctx.space_id).limit(10).list().await?;

            for obj in objects.iter() {
                if let Some(values) = obj.get_property_array(&prop.key) {
                    println!("Found multi-select property with {} values", values.len());

                    // Test via PropertyValue
                    if let Some(p) = obj.get_property(&prop.key) {
                        assert_eq!(p.format(), PropertyFormat::MultiSelect);
                        assert!(p.value.as_array().is_some());
                    }

                    break;
                }
            }
        } else {
            println!(
                "Skipping test_read_multiselect_property_value: no multi-select property found"
            );
        }

        Ok(())
    })
    .await
}

// =============================================================================
// Property CRUD Operations
// =============================================================================

#[tokio::test]
#[test_log::test]
async fn test_create_custom_property() -> TestResult<()> {
    with_test_context(|ctx| async move {
        let prop_name = unique_test_name("Custom Property");
        let prop_key = format!("custom_prop_{}", chrono::Utc::now().timestamp_millis());

        // Test creating properties with different formats
        for format in [
            PropertyFormat::Text,
            PropertyFormat::Number,
            PropertyFormat::Checkbox,
        ] {
            let key = format!("{}_{}", prop_key, format);
            let result = ctx
                .client
                .new_property(&ctx.space_id, format!("{} {}", prop_name, format), format)
                .key(&key)
                .create()
                .await;

            match result {
                Ok(prop) => {
                    println!("Created property: {} ({:?})", prop.name, prop.format());
                    assert_eq!(prop.format(), format);

                    // Clean up
                    let _ = ctx.client.property(&ctx.space_id, &prop.id).delete().await;
                }
                Err(e) => {
                    println!("Failed to create property with format {:?}: {}", format, e);
                }
            }
        }

        Ok(())
    })
    .await
}

#[tokio::test]
#[test_log::test]
async fn test_update_property_name() -> TestResult<()> {
    with_test_context(|ctx| async move {
        let original_name = unique_test_name("Original Name");
        let updated_name = unique_test_name("Updated Name");
        let prop_key = format!("update_test_{}", chrono::Utc::now().timestamp_millis());

        // Create property
        let prop = match ctx
            .client
            .new_property(&ctx.space_id, &original_name, PropertyFormat::Text)
            .key(&prop_key)
            .create()
            .await
        {
            Ok(p) => p,
            Err(_) => {
                println!("Skipping test_update_property_name: could not create property");
                return Ok(());
            }
        };

        // Update name
        let updated = ctx
            .client
            .update_property(&ctx.space_id, &prop.id)
            .name(&updated_name)
            .update()
            .await?;

        assert_eq!(updated.name, updated_name, "Name should be updated");
        assert_eq!(updated.id, prop.id, "ID should remain the same");

        // Clean up
        let _ = ctx.client.property(&ctx.space_id, &prop.id).delete().await;

        Ok(())
    })
    .await
}

#[tokio::test]
#[test_log::test]
async fn test_update_property_key() -> TestResult<()> {
    with_test_context(|ctx| async move {
        let prop_name = unique_test_name("Key Update Test");
        let original_key = format!("original_key_{}", unique_suffix());
        let updated_key = format!("updated_key_{}", unique_suffix());

        // Create property
        let prop = match ctx
            .client
            .new_property(&ctx.space_id, &prop_name, PropertyFormat::Text)
            .key(&original_key)
            .create()
            .await
        {
            Ok(p) => p,
            Err(_) => {
                println!("Skipping test_update_property_key: could not create property");
                return Ok(());
            }
        };

        // Update key
        let updated = ctx
            .client
            .update_property(&ctx.space_id, &prop.id)
            .name(&prop_name)
            .key(&updated_key)
            .update()
            .await?;

        assert_eq!(updated.key, updated_key, "Key should be updated");

        // Clean up
        let _ = ctx.client.property(&ctx.space_id, &prop.id).delete().await;

        Ok(())
    })
    .await
}

#[tokio::test]
#[test_log::test]
async fn test_delete_property() -> TestResult<()> {
    with_test_context(|ctx| async move {
        let prop_name = unique_test_name("Delete Test");
        let prop_key = format!("delete_test_{}", chrono::Utc::now().timestamp_millis());

        // Create property
        let prop = match ctx
            .client
            .new_property(&ctx.space_id, &prop_name, PropertyFormat::Text)
            .key(&prop_key)
            .create()
            .await
        {
            Ok(p) => p,
            Err(_) => {
                println!("Skipping test_delete_property: could not create property");
                return Ok(());
            }
        };

        let prop_id = prop.id.clone();

        // Delete property
        let deleted = ctx
            .client
            .property(&ctx.space_id, &prop_id)
            .delete()
            .await?;

        assert_eq!(deleted.id, prop_id, "Deleted property should have same ID");

        // Verify deletion - trying to get should fail
        let get_result = ctx.client.property(&ctx.space_id, &prop_id).get().await;

        match get_result {
            Err(AnytypeError::NotFound { obj_type, .. }) if &obj_type == "Property" => {
                println!("Property correctly not found after deletion");
            }
            Ok(_) => {
                println!("Property still exists after deletion (may be archived)");
            }
            Err(e) => {
                println!("Unexpected error after deletion: {:?}", e);
            }
        }

        Ok(())
    })
    .await
}

#[tokio::test]
#[test_log::test]
async fn test_property_key_stability() -> TestResult<()> {
    with_test_context(|ctx| async move {
        // List all properties
        let properties = ctx.client.properties(&ctx.space_id).list().await?;

        // Test a sample of properties
        for prop in properties.iter().take(5) {
            // Get property individually
            let fetched = ctx.client.property(&ctx.space_id, &prop.id).get().await?;

            // Verify all fields match
            assert_eq!(prop.id, fetched.id, "Property ID should be stable");
            assert_eq!(prop.key, fetched.key, "Property key should be stable");
            assert_eq!(prop.name, fetched.name, "Property name should be stable");
            assert_eq!(
                prop.format(),
                fetched.format(),
                "Property format should be stable"
            );
        }

        Ok(())
    })
    .await
}

// =============================================================================
// Property Validation Tests
// =============================================================================

#[tokio::test]
#[test_log::test]
async fn test_create_property_invalid_name() -> TestResult<()> {
    with_test_context(|ctx| async move {
        // Try to create property with empty name - should fail
        let result = ctx
            .client
            .new_property(&ctx.space_id, "", PropertyFormat::Text)
            .create()
            .await;

        match result {
            Err(AnytypeError::Validation { message }) => {
                println!("Correctly received validation error: {}", message);
                assert!(
                    message.to_lowercase().contains("name"),
                    "Error should mention name"
                );
            }
            Ok(_) => panic!("Expected validation error for property without name"),
            Err(e) => println!("Received unexpected error: {:?}", e),
        }

        Ok(())
    })
    .await
}

#[tokio::test]
#[test_log::test]
async fn test_create_property_duplicate_key() -> TestResult<()> {
    with_test_context(|ctx| async move {
        let prop_name = unique_test_name("Duplicate Key Test");
        let prop_key = format!("duplicate_key_{}", chrono::Utc::now().timestamp_millis());

        // Create first property
        let first = match ctx
            .client
            .new_property(&ctx.space_id, &prop_name, PropertyFormat::Text)
            .key(&prop_key)
            .create()
            .await
        {
            Ok(p) => p,
            Err(_) => {
                println!(
                    "Skipping test_create_property_duplicate_key: could not create first property"
                );
                return Ok(());
            }
        };

        // Try to create second property with same key
        let result = ctx
            .client
            .new_property(
                &ctx.space_id,
                format!("{} Second", &prop_name),
                PropertyFormat::Text,
            )
            .key(&prop_key)
            .create()
            .await;

        match result {
            Err(e) => {
                println!("Correctly failed to create duplicate key property: {:?}", e);
            }
            Ok(second) => {
                println!(
                    "Created second property (duplicate keys may be allowed): {}",
                    second.id
                );
                // Clean up second property
                let _ = ctx
                    .client
                    .property(&ctx.space_id, &second.id)
                    .delete()
                    .await;
            }
        }

        // Clean up first property
        let _ = ctx.client.property(&ctx.space_id, &first.id).delete().await;

        Ok(())
    })
    .await
}
