//! Integration tests for Filter and FilterExpression functionality
//!
//! Tests the Filter API against a live Anytype server, covering:
//! - Basic filters (text, number, date, checkbox, select)
//! - Filter composition (AND/OR logic)
//! - Filter application to object lists
//! - Error handling and validation
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
//! cargo test -p anytype --test test_filters
//! ```

mod common;

use crate::common::{
    create_object_with_retry, ensure_properties_and_type, lookup_property_tag_with_retry,
    unique_test_name,
};
use anytype::prelude::*;
use anytype::{
    test_assert,
    test_util::{TestContext, TestResult, with_test_context},
};
use serial_test::serial;

// =============================================================================
// Test Helper - Create Test Objects
// =============================================================================

/// Creates a set of test objects with various property values for filter testing
async fn create_test_objects(ctx: &TestContext) -> TestResult<(Vec<Object>, String)> {
    // Ensure all required properties exist before creating objects
    let type_key = ensure_properties_and_type(ctx).await?;
    let mut objects = Vec::new();

    // Object 1: High priority, checked, recent date
    let obj1 = create_object_with_retry("Filter Test High Priority", || async {
        ctx.client
            .new_object(&ctx.space_id, &type_key)
            .name(unique_test_name("Filter Test High Priority"))
            .body("Important task")
            .set_text("description", "High priority item")
            .set_number("priority", 1)
            .set_checkbox("done", true)
            .create()
            .await
    })
    .await?;
    ctx.register_object(&obj1.id);
    objects.push(obj1);

    // Object 2: Medium priority, unchecked
    let obj2 = create_object_with_retry("Filter Test Medium Priority", || async {
        ctx.client
            .new_object(&ctx.space_id, &type_key)
            .name(unique_test_name("Filter Test Medium Priority"))
            .body("Normal task")
            .set_text("description", "Medium priority item")
            .set_number("priority", 5)
            .set_checkbox("done", false)
            .create()
            .await
    })
    .await?;
    ctx.register_object(&obj2.id);
    objects.push(obj2);

    // Object 3: Low priority, unchecked, empty description
    let obj3 = create_object_with_retry("Filter Test Low Priority", || async {
        ctx.client
            .new_object(&ctx.space_id, &type_key)
            .name(unique_test_name("Filter Test Low Priority"))
            .body("Low priority task")
            .set_number("priority", 10)
            .set_checkbox("done", false)
            .create()
            .await
    })
    .await?;
    ctx.register_object(&obj3.id);
    objects.push(obj3);

    // Object 4: No priority set, has description
    let obj4 = create_object_with_retry("Filter Test No Priority", || async {
        ctx.client
            .new_object(&ctx.space_id, &type_key)
            .name(unique_test_name("Filter Test No Priority"))
            .body("Task without priority")
            .set_text("description", "Item without priority value")
            .create()
            .await
    })
    .await?;
    ctx.register_object(&obj4.id);
    objects.push(obj4);

    Ok((objects, type_key))
}

// =============================================================================
// Basic Filter Tests
// =============================================================================

#[tokio::test]
#[test_log::test]
#[serial]
async fn test_filter_not_empty() -> TestResult<()> {
    with_test_context(|ctx| async move {
        let (test_objs, _type_key) = create_test_objects(&ctx).await?;

        // Filter for objects with non-empty name
        let results = ctx
            .client
            .objects(&ctx.space_id)
            .filter(Filter::not_empty("name"))
            .limit(100)
            .list()
            .await?;

        // All our test objects have names
        let found_count = results
            .iter()
            .filter(|obj| test_objs.iter().any(|test_obj| test_obj.id == obj.id))
            .count();

        assert_eq!(
            found_count, 4,
            "Expected all 4 test objects with names to be found"
        );

        Ok(())
    })
    .await
}

#[tokio::test]
#[test_log::test]
#[serial]
async fn test_filter_is_empty() -> TestResult<()> {
    with_test_context(|ctx| async move {
        let (test_objs, _type_key) = create_test_objects(&ctx).await?;

        // Filter for objects with empty description
        let results = ctx
            .client
            .objects(&ctx.space_id)
            .filter(Filter::is_empty("description"))
            .limit(100)
            .list()
            .await?;

        // Object 3 has no description
        let found = results.iter().any(|obj| obj.id == test_objs[2].id);

        assert!(found, "Expected to find object with empty description");

        Ok(())
    })
    .await
}

#[tokio::test]
#[test_log::test]
#[serial]
async fn test_filter_checkbox_true() -> TestResult<()> {
    with_test_context(|ctx| async move {
        let task = create_object_with_retry("Filter Checkbox True", || async {
            ctx.client
                .new_object(&ctx.space_id, "task")
                .name(unique_test_name("test checkbox task done"))
                .body("Important task")
                .set_checkbox("done", true)
                .create()
                .await
        })
        .await?;
        ctx.register_object(&task.id);

        // Filter for checked items
        let results = ctx
            .client
            .objects(&ctx.space_id)
            .filter(Filter::checkbox_true("done"))
            .limit(100)
            .list()
            .await?
            .collect_all()
            .await?;

        let found = results.iter().any(|obj| obj.id == task.id);
        test_assert!(found, "Expected to find object with done=true");

        test_assert!(
            results
                .iter()
                .all(|obj| obj.get_property_bool("done") == Some(true)),
            "all 'done' should be true based on filter"
        );
        Ok(())
    })
    .await
}

#[tokio::test]
#[test_log::test]
#[serial]
async fn test_filter_checkbox_false() -> TestResult<()> {
    with_test_context(|ctx| async move {
        let task = create_object_with_retry("Filter Checkbox False", || async {
            ctx.client
                .new_object(&ctx.space_id, "task")
                .name(unique_test_name("test checkbox task not done"))
                .body("Important task")
                .set_checkbox("done", false)
                .create()
                .await
        })
        .await?;
        ctx.register_object(&task.id);

        // Filter for checked items
        let results = ctx
            .client
            .objects(&ctx.space_id)
            .filter(Filter::checkbox_false("done"))
            .limit(100)
            .list()
            .await?
            .collect_all()
            .await?;

        let found = results.iter().any(|obj| obj.id == task.id);
        test_assert!(found, "Expected to find object with done=false");

        for t in results.iter() {
            eprintln!(
                "{}: {:?}",
                t.name.as_deref().unwrap_or("(unnamed)"),
                t.get_property("done"),
            );
        }
        test_assert!(
            results
                .iter()
                .all(|obj| obj.get_property_bool("done") == Some(false)),
            "all 'done' should be false based on filter"
        );

        Ok(())
    })
    .await
}

// =============================================================================
// Numeric Filter Tests
// =============================================================================

#[tokio::test]
#[test_log::test]
#[serial]
async fn test_filter_number_equal() -> TestResult<()> {
    with_test_context(|ctx| async move {
        let (test_objs, _type_key) = create_test_objects(&ctx).await?;

        // Filter for priority equal to 5
        let results = ctx
            .client
            .objects(&ctx.space_id)
            .filter(Filter::number_equal("priority", 5))
            .limit(100)
            .list()
            .await?;

        // Object 2 has priority=5
        let found = results.iter().any(|obj| obj.id == test_objs[1].id);

        assert!(found, "Expected to find object with priority=5");

        Ok(())
    })
    .await
}

#[tokio::test]
#[test_log::test]
#[serial]
async fn test_filter_number_greater_than() -> TestResult<()> {
    with_test_context(|ctx| async move {
        let (test_objs, _type_key) = create_test_objects(&ctx).await?;

        // Filter for priority greater than 5
        let results = ctx
            .client
            .objects(&ctx.space_id)
            .filter(Filter::number_greater("priority", 5))
            .limit(100)
            .list()
            .await?;

        // Object 3 has priority=10
        let found = results.iter().any(|obj| obj.id == test_objs[2].id);

        assert!(found, "Expected to find object with priority > 5");

        Ok(())
    })
    .await
}

#[tokio::test]
#[test_log::test]
#[serial]
async fn test_filter_number_less_than() -> TestResult<()> {
    with_test_context(|ctx| async move {
        let (test_objs, _type_key) = create_test_objects(&ctx).await?;

        // Filter for priority less than 5
        let results = ctx
            .client
            .objects(&ctx.space_id)
            .filter(Filter::number_less("priority", 5))
            .limit(100)
            .list()
            .await?;

        // Object 1 has priority=1
        let found = results.iter().any(|obj| obj.id == test_objs[0].id);

        assert!(found, "Expected to find object with priority < 5");

        Ok(())
    })
    .await
}

#[tokio::test]
#[test_log::test]
#[serial]
async fn test_filter_number_range() -> TestResult<()> {
    with_test_context(|ctx| async move {
        let (test_objs, _type_key) = create_test_objects(&ctx).await?;

        // Filter for priority in range [3, 7] using >= 3 AND <= 7
        let results = ctx
            .client
            .objects(&ctx.space_id)
            .filter(Filter::number_greater_or_equal("priority", 3))
            .filter(Filter::number_less_or_equal("priority", 7))
            .limit(100)
            .list()
            .await?;

        // Only object 2 has priority=5 in range
        let found_obj2 = results.iter().any(|obj| obj.id == test_objs[1].id);
        let not_found_obj1 = !results.iter().any(|obj| obj.id == test_objs[0].id);
        let not_found_obj3 = !results.iter().any(|obj| obj.id == test_objs[2].id);

        assert!(
            found_obj2 && not_found_obj1 && not_found_obj3,
            "Expected only object with priority in range [3, 7]"
        );

        Ok(())
    })
    .await
}

// =============================================================================
// Date Filter Tests
// =============================================================================

#[tokio::test]
#[test_log::test]
#[serial]
async fn test_filter_date_equal() -> TestResult<()> {
    with_test_context(|ctx| async move {
        let (_test_objs, type_key) = create_test_objects(&ctx).await?;

        let date = "2025-06-15";
        let obj = create_object_with_retry("Filter Date Equal", || async {
            ctx.client
                .new_object(&ctx.space_id, &type_key)
                .name(unique_test_name("Date Equal Test"))
                .body("Test")
                .set_date("due_date", date)
                .create()
                .await
        })
        .await?;
        ctx.register_object(&obj.id);

        // Filter for exact date match
        let results = ctx
            .client
            .objects(&ctx.space_id)
            .filter(Filter::date_equal("due_date", date))
            .limit(10)
            .list()
            .await?;

        let found = results.iter().any(|o| o.id == obj.id);
        assert!(found, "Expected to find object with matching due_date");

        Ok(())
    })
    .await
}

#[tokio::test]
#[test_log::test]
#[serial]
async fn test_filter_date_after() -> TestResult<()> {
    with_test_context(|ctx| async move {
        let (_test_objs, type_key) = create_test_objects(&ctx).await?;
        let future_date = "2030-12-31";
        let obj = create_object_with_retry("Filter Date After", || async {
            ctx.client
                .new_object(&ctx.space_id, &type_key)
                .name(unique_test_name("Date After Test"))
                .body("Test")
                .set_date("due_date", future_date)
                .create()
                .await
        })
        .await?;
        ctx.register_object(&obj.id);

        // Filter for dates after 2030
        let results = ctx
            .client
            .objects(&ctx.space_id)
            .filter(Filter::date_greater("due_date", "2030-01-01"))
            .limit(100)
            .list()
            .await?;

        let found = results.iter().any(|o| o.id == obj.id);
        assert!(
            found,
            "Expected to find object with due_date after 2030-01-01"
        );

        Ok(())
    })
    .await
}

#[tokio::test]
#[test_log::test]
#[serial]
async fn test_filter_date_before() -> TestResult<()> {
    with_test_context(|ctx| async move {
        let (_test_objs, type_key) = create_test_objects(&ctx).await?;
        let past_date = "2025-01-01";
        let obj = create_object_with_retry("Filter Date Before", || async {
            ctx.client
                .new_object(&ctx.space_id, &type_key)
                .name(unique_test_name("Date Before Test"))
                .body("Test")
                .set_date("due_date", past_date)
                .create()
                .await
        })
        .await?;
        ctx.register_object(&obj.id);

        // Filter for dates before mid-year
        let results = ctx
            .client
            .objects(&ctx.space_id)
            .filter(Filter::date_less("due_date", "2025-06-01"))
            .limit(100)
            .list()
            .await?;

        let found = results.iter().any(|o| o.id == obj.id);
        assert!(
            found,
            "Expected to find object with due_date before 2025-06-01"
        );

        Ok(())
    })
    .await
}

// =============================================================================
// Select Filter Tests
// =============================================================================

#[tokio::test]
#[test_log::test]
#[serial]
async fn test_filter_select_in_single_with_id() -> TestResult<()> {
    with_test_context(|ctx| async move {
        let type_key = ensure_properties_and_type(&ctx).await?;

        let in_progress =
            lookup_property_tag_with_retry(ctx.as_ref(), "status", "In Progress").await?;

        let obj = create_object_with_retry("Filter Select In", || async {
            ctx.client
                .new_object(&ctx.space_id, &type_key)
                .name(unique_test_name("Select In Test"))
                .body("Test")
                .set_select("status", &in_progress.id)
                .create()
                .await
        })
        .await?;
        ctx.register_object(&obj.id);

        // Filter for status in_progress
        let results = ctx
            .client
            .objects(&ctx.space_id)
            .filter(Filter::select_in(
                "status",
                //vec!["in_progress"],
                vec![in_progress.id],
            ))
            .limit(100)
            .list()
            .await?;

        let found = results.iter().any(|o| o.id == obj.id);
        assert!(found, "Expected to find object with status='in_progress'");

        Ok(())
    })
    .await
}

#[tokio::test]
#[test_log::test]
#[serial]
async fn test_filter_select_in_multiple() -> TestResult<()> {
    with_test_context(|ctx| async move {
        let type_key = ensure_properties_and_type(&ctx).await?;

        let in_progress =
            lookup_property_tag_with_retry(ctx.as_ref(), "status", "In Progress").await?;
        let done = lookup_property_tag_with_retry(ctx.as_ref(), "status", "Done").await?;

        // this object is in_progress
        let obj1 = create_object_with_retry("Filter Select Multi 1", || async {
            ctx.client
                .new_object(&ctx.space_id, &type_key)
                .name(unique_test_name("Select Multi 1"))
                .body("Test")
                .set_select("status", &in_progress.id)
                .create()
                .await
        })
        .await?;
        ctx.register_object(&obj1.id);

        // this object has status=done
        let obj2 = create_object_with_retry("Filter Select Multi 2", || async {
            ctx.client
                .new_object(&ctx.space_id, &type_key)
                .name(unique_test_name("Select Multi 2"))
                .body("Test")
                .set_select("status", &done.id)
                .create()
                .await
        })
        .await?;
        ctx.register_object(&obj2.id);

        // Filter for status in ["open", "in_progress"]
        let results = ctx
            .client
            .objects(&ctx.space_id)
            .filter(Filter::select_in(
                "status",
                vec![&in_progress.id, &done.id], // in_progress_tag.id, done_tag.id],
            ))
            .limit(100)
            .list()
            .await?;

        let found_obj1 = results.iter().any(|o| o.id == obj1.id);
        let found_obj2 = results.iter().any(|o| o.id == obj2.id);

        assert!(
            found_obj1 && found_obj2,
            "Expected to find both objects with status in [open, in_progress]"
        );

        Ok(())
    })
    .await
}

#[tokio::test]
#[test_log::test]
#[serial]
async fn test_filter_select_not_in() -> TestResult<()> {
    with_test_context(|ctx| async move {
        let type_key = ensure_properties_and_type(&ctx).await?;

        let in_progress =
            lookup_property_tag_with_retry(ctx.as_ref(), "status", "In Progress").await?;
        let to_do = lookup_property_tag_with_retry(ctx.as_ref(), "status", "To Do").await?;
        let done = lookup_property_tag_with_retry(ctx.as_ref(), "status", "Done").await?;

        let obj = create_object_with_retry("Filter Select Not In", || async {
            ctx.client
                .new_object(&ctx.space_id, &type_key)
                .name(unique_test_name("Select Not In Test"))
                .body("Test")
                .set_select("status", &done.id)
                .create()
                .await
        })
        .await?;
        ctx.register_object(&obj.id);

        // Filter for status not in ["todo", "in_progress"]
        let results = ctx
            .client
            .objects(&ctx.space_id)
            .filter(Filter::select_not_in(
                "status",
                vec![&in_progress.id, &to_do.id],
            ))
            .limit(100)
            .list()
            .await?;

        let found = results.iter().any(|o| o.id == obj.id);
        assert!(
            found,
            "Expected to find object with status not in [open, in_progress]"
        );

        Ok(())
    })
    .await
}

// =============================================================================
// FilterExpression Composition Tests
// =============================================================================

#[tokio::test]
#[test_log::test]
#[serial]
async fn test_filter_expression_and() -> TestResult<()> {
    with_test_context(|ctx| async move {
        let type_key = ensure_properties_and_type(&ctx).await?;

        // create the object
        let new_obj = create_object_with_retry("Filter Expression And", || async {
            ctx.client
                .new_object(&ctx.space_id, &type_key)
                .set_number("priority", 3)
                .set_checkbox("done", true)
                .create()
                .await
        })
        .await?;
        ctx.register_object(&new_obj.id);

        // Combine filters: priority < 5 AND done = true
        let results = ctx
            .client
            .objects(&ctx.space_id)
            .filter(Filter::number_less("priority", 5))
            .filter(Filter::checkbox_true("done"))
            .limit(100)
            .list()
            .await?;

        assert!(!results.is_empty(), "filter should find a match");
        assert!(results.iter().any(|obj| obj.id == new_obj.id));

        Ok(())
    })
    .await
}

#[tokio::test]
#[test_log::test]
#[serial]
async fn test_filter_expression_empty() -> TestResult<()> {
    with_test_context(|ctx| async move {
        // Empty filters should return all objects (up to limit)
        let results = ctx.client.objects(&ctx.space_id).limit(10).list().await?;

        assert!(
            !results.is_empty(),
            "Expected to get results with no filters applied"
        );

        Ok(())
    })
    .await
}

// =============================================================================
// Filter on Objects List Tests
// =============================================================================

#[tokio::test]
#[test_log::test]
#[serial]
async fn test_objects_list_with_filter() -> TestResult<()> {
    with_test_context(|ctx| async move {
        let obj = create_object_with_retry("Filter List With Filter", || async {
            ctx.client
                .new_object(&ctx.space_id, "page")
                .name(unique_test_name("List With Filter"))
                .body("Test")
                .create()
                .await
        })
        .await?;
        ctx.register_object(&obj.id);

        // Apply filter to list
        let results = ctx
            .client
            .objects(&ctx.space_id)
            .filter(Filter::not_empty("name"))
            .limit(100)
            .list()
            .await?;

        let found = results.iter().any(|o| o.id == obj.id);
        assert!(found, "Expected to find object in filtered list");

        Ok(())
    })
    .await
}

#[tokio::test]
#[test_log::test]
#[serial]
async fn test_objects_list_multiple_filters() -> TestResult<()> {
    with_test_context(|ctx| async move {
        let obj = create_object_with_retry("Filter Multiple Filters", || async {
            ctx.client
                .new_object(&ctx.space_id, "page")
                .name(unique_test_name("Multiple Filters"))
                .body("Test body content")
                .set_text("description", "Has description")
                .create()
                .await
        })
        .await?;
        ctx.register_object(&obj.id);

        // Apply multiple filters
        let results = ctx
            .client
            .objects(&ctx.space_id)
            .filter(Filter::not_empty("name"))
            .filter(Filter::not_empty("description"))
            .limit(100)
            .list()
            .await?;

        let found = results.iter().any(|o| o.id == obj.id);
        assert!(found, "Expected to find object matching multiple filters");

        Ok(())
    })
    .await
}

#[tokio::test]
#[test_log::test]
#[serial]
async fn test_objects_list_filter_with_sort() -> TestResult<()> {
    with_test_context(|ctx| async move {
        let type_key = ensure_properties_and_type(&ctx).await?;
        // Create two objects with different priorities
        let obj1 = create_object_with_retry("Filter Sort 1", || async {
            ctx.client
                .new_object(&ctx.space_id, &type_key)
                .name(unique_test_name("Sort Test 1"))
                .body("Test")
                .set_number("priority", 10)
                .create()
                .await
        })
        .await?;
        ctx.register_object(&obj1.id);

        let obj2 = create_object_with_retry("Filter Sort 2", || async {
            ctx.client
                .new_object(&ctx.space_id, &type_key)
                .name(unique_test_name("Sort Test 2"))
                .body("Test")
                .set_number("priority", 5)
                .create()
                .await
        })
        .await?;
        ctx.register_object(&obj2.id);

        // Filter
        let results = ctx
            .client
            .objects(&ctx.space_id)
            .filter(Filter::not_empty("priority"))
            .limit(100)
            .list()
            .await?;

        // Verify both objects are in results
        let found_obj1 = results.iter().any(|o| o.id == obj1.id);
        let found_obj2 = results.iter().any(|o| o.id == obj2.id);

        assert!(
            found_obj1 && found_obj2,
            "Expected both objects in filtered and sorted results"
        );

        Ok(())
    })
    .await
}

#[tokio::test]
#[test_log::test]
#[serial]
async fn test_objects_list_filter_with_pagination() -> TestResult<()> {
    with_test_context(|ctx| async move {
        // Create multiple objects
        for i in 1..=5 {
            let label = format!("Filter Pagination {i}");
            let obj = create_object_with_retry(&label, || async {
                ctx.client
                    .new_object(&ctx.space_id, "page")
                    .name(unique_test_name(&format!("Pagination Test {}", i)))
                    .body("Test")
                    .create()
                    .await
            })
            .await?;
            ctx.register_object(&obj.id);
        }

        // Get first page
        let page1 = ctx
            .client
            .objects(&ctx.space_id)
            .filter(Filter::not_empty("name"))
            .limit(3)
            .offset(0)
            .list()
            .await?;

        // Get second page
        let page2 = ctx
            .client
            .objects(&ctx.space_id)
            .filter(Filter::not_empty("name"))
            .limit(3)
            .offset(3)
            .list()
            .await?;

        assert!(
            page1.len() <= 3 && page2.len() <= 3,
            "Expected pagination to respect limit"
        );

        Ok(())
    })
    .await
}

// =============================================================================
// Text Filter Variants
// =============================================================================

#[tokio::test]
#[test_log::test]
#[serial]
async fn test_filter_text_equal() -> TestResult<()> {
    with_test_context(|ctx| async move {
        let unique_name = unique_test_name("Unique Text Equal");
        let obj = create_object_with_retry("Filter Text Equal", || async {
            ctx.client
                .new_object(&ctx.space_id, "page")
                .name(&unique_name)
                .body("Test body")
                .create()
                .await
        })
        .await?;
        ctx.register_object(&obj.id);

        // Filter for exact name match
        let results = ctx
            .client
            .objects(&ctx.space_id)
            .filter(Filter::text_equal("name", &unique_name))
            .limit(10)
            .list()
            .await?;

        let found = results.iter().any(|o| o.id == obj.id);
        assert!(found, "Expected to find object with exact name match");

        Ok(())
    })
    .await
}

#[tokio::test]
#[test_log::test]
#[serial]
async fn test_filter_text_not_equal() -> TestResult<()> {
    with_test_context(|ctx| async move {
        let obj_name = unique_test_name("Text Not Equal");
        let obj = create_object_with_retry("Filter Text Not Equal", || async {
            ctx.client
                .new_object(&ctx.space_id, "page")
                .name(&obj_name)
                .body("Test")
                .create()
                .await
        })
        .await?;
        ctx.register_object(&obj.id);

        // Filter for objects where name is not equal to a different value
        let results = ctx
            .client
            .objects(&ctx.space_id)
            .filter(Filter::text_not_equal("name", "NonExistentName"))
            .limit(10)
            .list()
            .await?;

        let found = results.iter().any(|o| o.id == obj.id);
        assert!(
            found,
            "Expected to find object with name not equal to 'NonExistentName'"
        );

        Ok(())
    })
    .await
}

#[tokio::test]
#[test_log::test]
#[serial]
async fn test_filter_text_contains() -> TestResult<()> {
    with_test_context(|ctx| async move {
        let obj = create_object_with_retry("Filter Text Contains", || async {
            ctx.client
                .new_object(&ctx.space_id, "page")
                .name(unique_test_name("Normal Name Contains"))
                .body("Test")
                .create()
                .await
        })
        .await?;
        ctx.register_object(&obj.id);

        // Filter for name not containing substring
        let results = ctx
            .client
            .objects(&ctx.space_id)
            .filter(Filter::text_contains("name", "name contains"))
            .limit(10)
            .list()
            .await?;

        let found = results.iter().any(|o| o.id == obj.id);
        assert!(found, "Expected to find object with name like 'normal'");

        Ok(())
    })
    .await
}

#[tokio::test]
#[test_log::test]
#[serial]
async fn test_filter_text_not_contains() -> TestResult<()> {
    with_test_context(|ctx| async move {
        let obj = create_object_with_retry("Filter Text Not Contains", || async {
            ctx.client
                .new_object(&ctx.space_id, "page")
                .name(unique_test_name("Normal Name Not Contains"))
                .body("Test")
                .create()
                .await
        })
        .await?;
        ctx.register_object(&obj.id);

        // Filter for name not containing substring
        let results = ctx
            .client
            .objects(&ctx.space_id)
            .filter(Filter::text_not_contains("name", "abnormal"))
            .limit(10)
            .list()
            .await?;

        let found = results.iter().any(|o| o.id == obj.id);
        assert!(
            found,
            "Expected to find object with name not like 'abnormal'"
        );

        Ok(())
    })
    .await
}

// =============================================================================
// Type Filter Tests
// =============================================================================

#[tokio::test]
#[test_log::test]
#[serial]
async fn test_filter_type_in() -> TestResult<()> {
    with_test_context(|ctx| async move {
        let page_obj = create_object_with_retry("Filter Type In", || async {
            ctx.client
                .new_object(&ctx.space_id, "page")
                .name(unique_test_name("Type Test Page"))
                .body("Test")
                .create()
                .await
        })
        .await?;
        ctx.register_object(&page_obj.id);

        let page_type = ctx.client.lookup_type_by_key(&ctx.space_id, "page").await?;

        // Filter for type = page
        let results = ctx
            .client
            .objects(&ctx.space_id)
            .filter(Filter::type_in(vec![&page_type.id]))
            .limit(1)
            .list()
            .await?;

        let found = results.iter().any(|o| o.id == page_obj.id);
        assert!(found, "Expected to find page object when filtering by type");

        Ok(())
    })
    .await
}
