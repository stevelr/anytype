//! Smoke Test for anytype
//!
//! This smoke test validates basic API functionality against a live Anytype server.
//! It covers approximately 50-100 API calls testing spaces, types, properties,
//! objects, search, and property formats.
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
//! cargo test -p anytype --test smoke_test
//! ```

use anytype::{prelude::*, test_util::*};
use std::{collections::HashSet, time::Duration};

// =============================================================================
// Test Configuration
// =============================================================================

const TEST_TIMEOUT_SECS: u64 = 120;
const ESTIMATED_RUNTIME_SECS: u64 = 60;

// =============================================================================
// Main Smoke Test
// =============================================================================

#[tokio::test]
#[test_log::test]
async fn smoke_test() {
    println!("\n========================================");
    println!("  Anytype API Smoke Test");
    println!("========================================");
    println!("Estimated runtime: ~{} seconds", ESTIMATED_RUNTIME_SECS);
    println!();

    // Setup
    let ctx = match TestContext::new().await {
        Ok(ctx) => ctx,
        Err(e) => {
            eprintln!("SMOKE TEST SETUP FAILED: {}", e);
            eprintln!("\nPlease ensure:");
            eprintln!("  1. Anytype server is running");
            eprintln!("  2. Environment variables are set (source .test-env)");
            eprintln!("  3. API key file exists and is valid");
            panic!("Setup failed: {}", e);
        }
    };

    println!("Configuration:");
    println!("  URL: {}", ctx.client.get_config().base_url);
    println!("  Space ID: {}", ctx.space_id);
    println!();

    // Create timeout guard
    let timeout = Duration::from_secs(TEST_TIMEOUT_SECS);
    let result = tokio::time::timeout(timeout, run_smoke_tests(&ctx)).await;

    match result {
        Ok(test_results) => {
            let metrics = ctx.client.http_metrics();
            println!("\n========================================");
            println!("  Smoke Test Results");
            println!("========================================");
            println!("API Calls: ~{}", ctx.call_count());
            println!("Duration: {} seconds", ctx.elapsed_secs());
            println!("{}", test_results.summary());
            println!();
            println!("HTTP Metrics:");
            println!("  {}", metrics);

            if !test_results.is_success() {
                println!("\nFailed tests:");
                for (name, error) in test_results.failures() {
                    println!("  - {}: {}", name, error);
                }
            }

            assert!(
                test_results.is_success(),
                "Smoke test failed: {}",
                test_results.summary()
            );
        }
        Err(_) => {
            panic!(
                "Smoke test timed out after {} seconds. This may indicate a hang or deadlock.",
                TEST_TIMEOUT_SECS
            );
        }
    }
}

async fn run_smoke_tests(ctx: &TestContext) -> TestResults {
    let mut results = TestResults::default();

    // Public API contracts
    println!("Testing: Spaces API");
    test_spaces_api(ctx, &mut results).await;

    println!("\nTesting: Types API");
    test_types_api(ctx, &mut results).await;

    println!("\nTesting: Properties API");
    test_properties_api(ctx, &mut results).await;

    println!("\nTesting: Objects API (list)");
    test_objects_list_api(ctx, &mut results).await;

    println!("\nTesting: Members API");
    test_members_api(ctx, &mut results).await;

    // Property format coverage
    println!("\nTesting: Property Formats");
    test_property_formats(ctx, &mut results).await;

    // Object CRUD
    println!("\nTesting: Object CRUD Operations");
    test_object_crud(ctx, &mut results).await;

    // Search tests
    println!("\nTesting: Search API");
    test_search_api(ctx, &mut results).await;

    // Filter tests
    println!("\nTesting: Filter Operations");
    test_filters(ctx, &mut results).await;

    results
}

// =============================================================================
// Public API Contracts
// =============================================================================

async fn test_spaces_api(ctx: &TestContext, results: &mut TestResults) {
    // Test: List spaces
    match ctx.client.spaces().list().await {
        Ok(spaces) => {
            ctx.increment_calls(1);
            if spaces.is_empty() {
                results.fail("spaces.list", "No spaces returned");
            } else {
                results.pass(&format!("spaces.list ({} spaces)", spaces.len()));

                // Verify space fields
                let first = spaces.iter().next().unwrap();
                if first.id.is_empty() {
                    results.fail("spaces.list.fields", "Missing required id field");
                } else if first.name.is_empty() {
                    println!("spaces.list.fields: empty name returned (allowed)");
                    results.pass("spaces.list.fields");
                } else {
                    results.pass("spaces.list.fields");
                }
            }
        }
        Err(e) => {
            ctx.increment_calls(1);
            results.fail("spaces.list", &e.to_string());
        }
    }

    // Test: Get specific space
    match ctx.client.space(&ctx.space_id).get().await {
        Ok(space) => {
            ctx.increment_calls(1);
            if space.id == ctx.space_id {
                results.pass("space.get");
            } else {
                results.fail("space.get", "Space ID mismatch");
            }
        }
        Err(e) => {
            ctx.increment_calls(1);
            results.fail("space.get", &e.to_string());
        }
    }
}

async fn test_types_api(ctx: &TestContext, results: &mut TestResults) {
    // Test: List types
    let types_result = ctx.client.types(&ctx.space_id).list().await;
    match types_result {
        Ok(types) => {
            ctx.increment_calls(1);
            if types.is_empty() {
                results.fail("types.list", "No types returned");
                return;
            }
            results.pass(&format!("types.list ({} types)", types.len()));

            // Verify type fields
            let first = types.iter().next().unwrap();
            if first.id.is_empty() || first.key.is_empty() {
                results.fail("types.list.fields", "Missing required fields (id, key)");
            } else {
                results.pass("types.list.fields");
            }

            // Test: Get specific type
            match ctx.client.get_type(&ctx.space_id, &first.id).get().await {
                Ok(typ) => {
                    ctx.increment_calls(1);
                    if typ.id == first.id && typ.key == first.key {
                        results.pass("type.get");
                    } else {
                        results.fail("type.get", "Type data mismatch");
                    }
                }
                Err(e) => {
                    ctx.increment_calls(1);
                    results.fail("type.get", &e.to_string());
                }
            }

            // Check for common system types
            let type_keys: HashSet<_> = types.iter().map(|t| t.key.as_str()).collect();
            if type_keys.contains("page") {
                results.pass("types.contains_page");
            } else {
                results.fail("types.contains_page", "Missing 'page' type");
            }
            if type_keys.contains("task") {
                results.pass("types.contains_task");
            } else {
                results.fail("types.contains_task", "Missing 'task' type");
            }
        }
        Err(e) => {
            ctx.increment_calls(1);
            results.fail("types.list", &e.to_string());
        }
    }

    // lookup type by key
    match ctx.client.lookup_type_by_key(&ctx.space_id, "page").await {
        Err(e) => results.fail("types.lookup_page", &e.to_string()),
        Ok(_) => results.pass("types.lookup_page"),
    }
}

async fn test_properties_api(ctx: &TestContext, results: &mut TestResults) {
    // Test: List properties
    let props_result = ctx.client.properties(&ctx.space_id).list().await;
    match props_result {
        Ok(properties) => {
            ctx.increment_calls(1);
            if properties.is_empty() {
                results.fail("properties.list", "No properties returned");
                return;
            }
            results.pass(&format!(
                "properties.list ({} properties)",
                properties.len()
            ));

            // Verify property fields
            let first = properties.iter().next().unwrap();
            if first.id.is_empty() || first.key.is_empty() {
                results.fail(
                    "properties.list.fields",
                    "Missing required fields (id, key)",
                );
            } else {
                results.pass("properties.list.fields");
            }

            // Test: Get specific property
            match ctx.client.property(&ctx.space_id, &first.id).get().await {
                Ok(prop) => {
                    ctx.increment_calls(1);
                    if prop.id == first.id && prop.key == first.key {
                        results.pass("property.get");
                    } else {
                        results.fail("property.get", "Property data mismatch");
                    }
                }
                Err(e) => {
                    ctx.increment_calls(1);
                    results.fail("property.get", &e.to_string());
                }
            }

            // Check for common system properties
            let prop_keys: HashSet<_> = properties.iter().map(|p| p.key.as_str()).collect();
            let has_name = prop_keys.contains("name");
            let has_description = prop_keys.contains("description");
            if has_name || has_description {
                results.pass("properties.system_props");
            } else {
                results.fail(
                    "properties.system_props",
                    "Missing common system properties",
                );
            }
        }
        Err(e) => {
            ctx.increment_calls(1);
            results.fail("properties.list", &e.to_string());
        }
    }

    // lookup property by key
    let property = ctx
        .client
        .lookup_property_by_key(&ctx.space_id, "done")
        .await
        .expect("lookup_property_by_key");

    match property.format() {
        PropertyFormat::Checkbox => {
            results.pass("properties.done_format");
        }
        fmt => {
            results.fail(
                "properties.done_format",
                &format!("done format is {fmt}, expected checkbox"),
            );
        }
    }
}

async fn test_objects_list_api(ctx: &TestContext, results: &mut TestResults) {
    // Test: List objects with limit
    match ctx.client.objects(&ctx.space_id).limit(10).list().await {
        Ok(objects) => {
            ctx.increment_calls(1);
            results.pass(&format!("objects.list ({} objects)", objects.len()));

            if !objects.is_empty() {
                let first = objects.iter().next().unwrap();
                // Verify required fields
                if first.id.is_empty() || first.space_id.is_empty() {
                    results.fail("objects.list.fields", "Missing required fields");
                } else {
                    results.pass("objects.list.fields");
                }

                // Test: Get specific object
                match ctx.client.object(&ctx.space_id, &first.id).get().await {
                    Ok(obj) => {
                        ctx.increment_calls(1);
                        if obj.id == first.id {
                            results.pass("object.get");
                        } else {
                            results.fail("object.get", "Object ID mismatch");
                        }
                    }
                    Err(e) => {
                        ctx.increment_calls(1);
                        results.fail("object.get", &e.to_string());
                    }
                }
            }
        }
        Err(e) => {
            ctx.increment_calls(1);
            results.fail("objects.list", &e.to_string());
        }
    }
}

async fn test_members_api(ctx: &TestContext, results: &mut TestResults) {
    // Test: List members
    match ctx.client.members(&ctx.space_id).list().await {
        Ok(members) => {
            ctx.increment_calls(1);
            if members.is_empty() {
                results.fail(
                    "members.list",
                    "No members returned (expected at least owner)",
                );
            } else {
                results.pass(&format!("members.list ({} members)", members.len()));

                // Verify member fields
                let first = members.iter().next().unwrap();
                if first.id.is_empty() {
                    results.fail("members.list.fields", "Missing member ID");
                } else {
                    results.pass("members.list.fields");
                }
            }
        }
        Err(e) => {
            ctx.increment_calls(1);
            results.fail("members.list", &e.to_string());
        }
    }
}

// =============================================================================
// Property Format Coverage
// =============================================================================

async fn test_property_formats(ctx: &TestContext, results: &mut TestResults) {
    // List all properties and check format coverage
    match ctx.client.properties(&ctx.space_id).list().await {
        Ok(properties) => {
            ctx.increment_calls(1);

            // Collect unique formats using Vec (PropertyFormat doesn't impl Hash)
            let mut found_formats: Vec<PropertyFormat> = Vec::new();
            for prop in properties.iter() {
                if !found_formats.iter().any(|f| *f == prop.format()) {
                    found_formats.push(prop.format());
                }
            }

            // Check which formats are present
            let all_formats = [
                PropertyFormat::Text,
                PropertyFormat::Number,
                PropertyFormat::Select,
                PropertyFormat::MultiSelect,
                PropertyFormat::Date,
                PropertyFormat::Checkbox,
                PropertyFormat::Url,
                PropertyFormat::Email,
                PropertyFormat::Phone,
                PropertyFormat::Objects,
                PropertyFormat::Files,
            ];

            let mut found_count = 0;
            for format in &all_formats {
                if found_formats.iter().any(|f| f == format) {
                    found_count += 1;
                }
            }

            // Report coverage
            results.pass(&format!(
                "property_formats ({}/{} formats found)",
                found_count,
                all_formats.len()
            ));

            // Text format should always be present
            if found_formats.contains(&PropertyFormat::Text) {
                results.pass("property_format.text");
            } else {
                results.fail("property_format.text", "Text format not found");
            }

            // Select format commonly present
            if found_formats.contains(&PropertyFormat::Select) {
                results.pass("property_format.select");
            }

            // Date format commonly present
            if found_formats.contains(&PropertyFormat::Date) {
                results.pass("property_format.date");
            }
        }
        Err(e) => {
            ctx.increment_calls(1);
            results.fail("property_formats", &e.to_string());
        }
    }
}

// =============================================================================
// Object CRUD
// =============================================================================

async fn test_object_crud(ctx: &TestContext, results: &mut TestResults) {
    let test_name = format!("Smoke Test Object {}", chrono::Utc::now().timestamp());

    // CREATE
    let create_result = ctx
        .client
        .new_object(&ctx.space_id, "page")
        .name(&test_name)
        .body("# Smoke Test\n\nThis is a test object created by the smoke test.")
        .description("Created by anytype smoke test")
        .create()
        .await;

    let created_obj = match create_result {
        Ok(obj) => {
            ctx.increment_calls(1);
            if obj.name.as_deref() == Some(&test_name) {
                results.pass("object.create");
                obj
            } else {
                results.fail("object.create", "Name mismatch after creation");
                return;
            }
        }
        Err(e) => {
            ctx.increment_calls(1);
            results.fail("object.create", &e.to_string());
            return;
        }
    };

    let object_id = &created_obj.id;
    ctx.register_object(object_id);

    // READ (verify creation)
    match ctx.client.object(&ctx.space_id, object_id).get().await {
        Ok(obj) => {
            ctx.increment_calls(1);
            if &obj.id == object_id && obj.name.as_deref() == Some(&test_name) {
                results.pass("object.read_after_create");
            } else {
                results.fail("object.read_after_create", "Data mismatch");
            }
        }
        Err(e) => {
            ctx.increment_calls(1);
            results.fail("object.read_after_create", &e.to_string());
        }
    }

    // UPDATE
    let updated_name = format!("{} (Updated)", test_name);
    match ctx
        .client
        .update_object(&ctx.space_id, object_id)
        .name(&updated_name)
        .body("# Updated Smoke Test\n\nThis object has been updated.")
        .update()
        .await
    {
        Ok(obj) => {
            ctx.increment_calls(1);
            if obj.name.as_deref() == Some(&updated_name) {
                results.pass("object.update");
            } else {
                results.fail("object.update", "Name not updated");
            }
        }
        Err(e) => {
            ctx.increment_calls(1);
            results.fail("object.update", &e.to_string());
        }
    }

    // READ (verify update)
    match ctx.client.object(&ctx.space_id, object_id).get().await {
        Ok(obj) => {
            ctx.increment_calls(1);
            if obj.name.as_deref() == Some(&updated_name) {
                results.pass("object.read_after_update");
            } else {
                results.fail("object.read_after_update", "Update not persisted");
            }
        }
        Err(e) => {
            ctx.increment_calls(1);
            results.fail("object.read_after_update", &e.to_string());
        }
    }

    // DELETE (archive)
    match ctx.client.object(&ctx.space_id, object_id).delete().await {
        Ok(obj) => {
            ctx.increment_calls(1);
            // The API may or may not immediately reflect archived status
            // Success means the delete call worked
            results.pass(&format!("object.delete (archived={})", obj.archived));
        }
        Err(e) => {
            ctx.increment_calls(1);
            results.fail("object.delete", &e.to_string());
        }
    }

    // Verify deletion - object should be archived or not found
    match ctx.client.object(&ctx.space_id, object_id).get().await {
        Ok(obj) => {
            ctx.increment_calls(1);
            // Object exists - check if it's archived or accept it either way
            // since the primary test is that delete() succeeded
            results.pass(&format!(
                "object.read_after_delete (archived={})",
                obj.archived
            ));
        }
        Err(e) => {
            ctx.increment_calls(1);
            // NotFound is acceptable - object may be fully deleted
            if matches!(e, AnytypeError::NotFound { .. }) {
                results.pass("object.read_after_delete (not_found)");
            } else {
                results.fail("object.read_after_delete", &e.to_string());
            }
        }
    }
}

// =============================================================================
// Search API Tests
// =============================================================================

async fn test_search_api(ctx: &TestContext, results: &mut TestResults) {
    // Test: Global search
    match ctx.client.search_global().limit(5).execute().await {
        Ok(search_results) => {
            ctx.increment_calls(1);
            results.pass(&format!("search.global ({} results)", search_results.len()));
        }
        Err(e) => {
            ctx.increment_calls(1);
            results.fail("search.global", &e.to_string());
        }
    }

    // Test: Search in space
    match ctx.client.search_in(&ctx.space_id).limit(5).execute().await {
        Ok(search_results) => {
            ctx.increment_calls(1);
            results.pass(&format!(
                "search.in_space ({} results)",
                search_results.len()
            ));
        }
        Err(e) => {
            ctx.increment_calls(1);
            results.fail("search.in_space", &e.to_string());
        }
    }

    // Test: Search with text query
    match ctx
        .client
        .search_in(&ctx.space_id)
        .text("test")
        .limit(5)
        .execute()
        .await
    {
        Ok(search_results) => {
            ctx.increment_calls(1);
            results.pass(&format!(
                "search.with_text ({} results)",
                search_results.len()
            ));
        }
        Err(e) => {
            ctx.increment_calls(1);
            results.fail("search.with_text", &e.to_string());
        }
    }

    // Test: Search with type filter
    match ctx
        .client
        .search_in(&ctx.space_id)
        .types(["page"])
        .limit(5)
        .execute()
        .await
    {
        Ok(search_results) => {
            ctx.increment_calls(1);
            results.pass(&format!(
                "search.with_types ({} results)",
                search_results.len()
            ));
        }
        Err(e) => {
            ctx.increment_calls(1);
            results.fail("search.with_types", &e.to_string());
        }
    }
}

// =============================================================================
// Filter Tests
// =============================================================================

async fn test_filters(ctx: &TestContext, results: &mut TestResults) {
    // Test: Not empty filter (name exists)
    match ctx
        .client
        .objects(&ctx.space_id)
        .filter(Filter::not_empty("name"))
        .limit(5)
        .list()
        .await
    {
        Ok(objects) => {
            ctx.increment_calls(1);
            results.pass(&format!("filter.not_empty ({} results)", objects.len()));
        }
        Err(e) => {
            ctx.increment_calls(1);
            results.fail("filter.not_empty", &e.to_string());
        }
    }

    // Test: Empty filter (find objects with empty description)
    match ctx
        .client
        .objects(&ctx.space_id)
        .filter(Filter::is_empty("description"))
        .limit(5)
        .list()
        .await
    {
        Ok(objects) => {
            ctx.increment_calls(1);
            results.pass(&format!("filter.is_empty ({} results)", objects.len()));
        }
        Err(e) => {
            ctx.increment_calls(1);
            results.fail("filter.is_empty", &e.to_string());
        }
    }

    // Test: Pagination with offset
    match ctx
        .client
        .objects(&ctx.space_id)
        .limit(3)
        .offset(0)
        .list()
        .await
    {
        Ok(first_page) => {
            ctx.increment_calls(1);
            if first_page.len() <= 3 {
                results.pass("pagination.limit");

                // Get second page
                match ctx
                    .client
                    .objects(&ctx.space_id)
                    .limit(3)
                    .offset(3)
                    .list()
                    .await
                {
                    Ok(second_page) => {
                        ctx.increment_calls(1);
                        // Just verify we can paginate
                        results.pass(&format!(
                            "pagination.offset (page2: {} items)",
                            second_page.len()
                        ));
                    }
                    Err(e) => {
                        ctx.increment_calls(1);
                        results.fail("pagination.offset", &e.to_string());
                    }
                }
            } else {
                results.fail(
                    "pagination.limit",
                    &format!("Expected <= 3 items, got {}", first_page.len()),
                );
            }
        }
        Err(e) => {
            ctx.increment_calls(1);
            results.fail("pagination.limit", &e.to_string());
        }
    }
}
