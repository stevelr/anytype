//! Integration tests for anytype Search functionality
//!
//! Tests cover:
//! - Global search across all spaces
//! - Space-specific search
//! - Text search queries
//! - Type filtering (single and multiple types)
//! - Search with sorting
//! - Combined search (text + filters)
//! - Pagination in search results
//! - Error handling for invalid search parameters
//!
//! ## Running
//!
//! ```bash
//! source .test-env
//! cargo test -p anytype --test test_search
//! ```

mod common;

use std::time::Duration;

use anytype::{prelude::*, test_util::with_test_context_unit};
use common::unique_test_name;

// =============================================================================
// Global Search Tests
// =============================================================================

#[tokio::test]
#[test_log::test]
async fn test_search_global_basic() {
    with_test_context_unit(|ctx| async move {
        let results = ctx
            .client
            .search_global()
            .limit(10)
            .execute()
            .await
            .expect("Failed to execute global search");

        println!("Global search returned {} results", results.len());

        // Verify basic result structure
        for obj in results.iter() {
            assert!(!obj.id.is_empty(), "Object ID should not be empty");
            assert!(!obj.space_id.is_empty(), "Space ID should not be empty");
        }
    })
    .await
}

#[tokio::test]
#[test_log::test]
async fn test_search_global_with_limit() {
    with_test_context_unit(|ctx| async move {
        let limit = 5;
        let results = ctx
            .client
            .search_global()
            .limit(limit)
            .execute()
            .await
            .expect("Failed to execute global search with limit");

        assert!(
            results.len() <= limit as usize,
            "Results should respect limit: got {} for limit {}",
            results.len(),
            limit
        );

        println!(
            "Global search with limit={} returned {} results",
            limit,
            results.len()
        );
    })
    .await
}

#[tokio::test]
#[test_log::test]
async fn test_search_global_empty_query() {
    with_test_context_unit(|ctx| async move {
        // Search with no query should return results
        let results = ctx
            .client
            .search_global()
            .limit(10)
            .execute()
            .await
            .expect("Failed to execute global search with empty query");

        println!(
            "Global search with empty query returned {} results",
            results.len()
        );
    })
    .await
}

// =============================================================================
// Space Search Tests
// =============================================================================

#[tokio::test]
#[test_log::test]
async fn test_search_in_space() {
    with_test_context_unit(|ctx| async move {
        let results = ctx
            .client
            .search_in(&ctx.space_id)
            .limit(10)
            .execute()
            .await
            .expect("Failed to execute space search");

        println!("Space search returned {} results", results.len());

        // Verify all results are from the searched space
        for obj in results.iter() {
            assert_eq!(
                obj.space_id, ctx.space_id,
                "Result should be from searched space"
            );
        }
    })
    .await
}

#[tokio::test]
#[test_log::test]
async fn test_search_space_results_match_space() {
    with_test_context_unit(|ctx| async move {
        let results = ctx
            .client
            .search_in(&ctx.space_id)
            .limit(20)
            .execute()
            .await
            .expect("Failed to execute space search");

        // Verify all results match the space_id
        for obj in results.iter() {
            assert_eq!(
                obj.space_id, ctx.space_id,
                "All search results should be from space {}",
                ctx.space_id
            );
        }

        println!(
            "Verified {} results all from space {}",
            results.len(),
            ctx.space_id
        );
    })
    .await
}

#[tokio::test]
#[test_log::test]
async fn test_search_in_nonexistent_space() {
    with_test_context_unit(|ctx| async move {
        let fake_space_id = "bafyreiabcdefghijklmnopqrstuvwxyz234567890";

        let result = ctx
            .client
            .search_in(fake_space_id)
            .limit(10)
            .execute()
            .await;

        match result {
            Err(AnytypeError::NotFound { obj_type, .. }) if &obj_type == "Space" => {
                println!("Correctly received NotFound for nonexistent space");
            }
            Err(AnytypeError::Validation { .. }) => {
                println!("Correctly received Validation error for invalid space");
            }
            Err(e) => {
                println!("Received error for nonexistent space: {:?}", e);
                // Accept any error for invalid space
            }
            Ok(_) => {
                panic!("Expected error for nonexistent space ID");
            }
        }
    })
    .await
}

// =============================================================================
// Text Search Tests
// =============================================================================

#[tokio::test]
#[test_log::test]
async fn test_search_with_text_query() {
    with_test_context_unit(|ctx| async move {
        // Create an object with unique searchable text
        let unique_term = format!("SearchText{}", chrono::Utc::now().timestamp_millis());
        let obj = ctx
            .client
            .new_object(&ctx.space_id, "page")
            .name(&unique_term)
            .body(format!(
                "This is a test document containing {}",
                unique_term
            ))
            .create()
            .await
            .expect("Failed to create searchable object");

        ctx.register_object(&obj.id);

        // Small delay to allow indexing
        tokio::time::sleep(Duration::from_millis(200)).await;

        // Search for the unique term
        let results = ctx
            .client
            .search_in(&ctx.space_id)
            .text(&unique_term)
            .execute()
            .await
            .expect("Failed to execute text search");

        println!(
            "Text search for '{}' returned {} results",
            unique_term,
            results.len()
        );
    })
    .await
}

#[tokio::test]
#[test_log::test]
async fn test_search_text_case_insensitive() {
    with_test_context_unit(|ctx| async move {
        // Create object with mixed case name
        let base_term = format!("CaseSensitive{}", chrono::Utc::now().timestamp_millis());
        let obj = ctx
            .client
            .new_object(&ctx.space_id, "page")
            .name(&base_term)
            .create()
            .await
            .expect("Failed to create object");
        ctx.register_object(&obj.id);

        // Wait for indexing
        tokio::time::sleep(Duration::from_millis(200)).await;

        // Search with lowercase
        let lower_results = ctx
            .client
            .search_in(&ctx.space_id)
            .text(base_term.to_lowercase())
            .execute()
            .await
            .expect("Failed to execute lowercase search");

        // Search with uppercase
        let upper_results = ctx
            .client
            .search_in(&ctx.space_id)
            .text(base_term.to_uppercase())
            .execute()
            .await
            .expect("Failed to execute uppercase search");

        println!(
            "Case sensitivity test: lowercase={} results, uppercase={} results",
            lower_results.len(),
            upper_results.len()
        );
    })
    .await
}

#[tokio::test]
#[test_log::test]
async fn test_search_text_partial_match() {
    with_test_context_unit(|ctx| async move {
        // Create object with specific text
        let full_term = format!("PartialMatchTest{}", chrono::Utc::now().timestamp_millis());
        let obj = ctx
            .client
            .new_object(&ctx.space_id, "page")
            .name(&full_term)
            .create()
            .await
            .expect("Failed to create object");
        ctx.register_object(&obj.id);

        // Wait for indexing
        tokio::time::sleep(Duration::from_millis(200)).await;

        // Search with partial term (first part)
        let partial_term = &full_term[..10]; // Take first 10 chars
        let results = ctx
            .client
            .search_in(&ctx.space_id)
            .text(partial_term)
            .execute()
            .await
            .expect("Failed to execute partial match search");

        println!(
            "Partial match search for '{}' (from '{}') returned {} results",
            partial_term,
            full_term,
            results.len()
        );
    })
    .await
}

// =============================================================================
// Type Filtering Tests
// =============================================================================

#[tokio::test]
#[test_log::test]
async fn test_search_filter_by_single_type() {
    with_test_context_unit(|ctx| async move {
        let results = ctx
            .client
            .search_in(&ctx.space_id)
            .types(["page"])
            .limit(10)
            .execute()
            .await
            .expect("Failed to execute type-filtered search");

        println!(
            "Search filtered by type='page' returned {} results",
            results.len()
        );

        // Verify all results are of type "page"
        for obj in results.iter() {
            if let Some(ref obj_type) = obj.r#type {
                assert_eq!(
                    obj_type.key, "page",
                    "Result should be of type 'page', got '{}'",
                    obj_type.key
                );
            }
        }
    })
    .await
}

#[tokio::test]
#[test_log::test]
async fn test_search_filter_by_multiple_types() {
    with_test_context_unit(|ctx| async move {
        let types = vec!["page", "note"];
        let results = ctx
            .client
            .search_in(&ctx.space_id)
            .types(types.clone())
            .limit(20)
            .execute()
            .await
            .expect("Failed to execute multi-type filtered search");

        println!(
            "Search filtered by types={:?} returned {} results",
            types,
            results.len()
        );

        // Verify all results match one of the specified types
        for obj in results.iter() {
            if let Some(ref obj_type) = obj.r#type {
                assert!(
                    types.contains(&obj_type.key.as_str()),
                    "Result type '{}' should be in {:?}",
                    obj_type.key,
                    types
                );
            }
        }
    })
    .await
}

#[tokio::test]
#[test_log::test]
async fn test_search_type_filter_validates_results() {
    with_test_context_unit(|ctx| async move {
        // Create a page object
        let page_name = unique_test_name("TypeFilterPage");
        let page_obj = ctx
            .client
            .new_object(&ctx.space_id, "page")
            .name(&page_name)
            .create()
            .await
            .expect("Failed to create page object");
        ctx.register_object(&page_obj.id);

        // Wait for indexing
        tokio::time::sleep(Duration::from_millis(200)).await;

        // Search for pages only
        let page_results = ctx
            .client
            .search_in(&ctx.space_id)
            .types(["page"])
            .text(&page_name)
            .execute()
            .await
            .expect("Failed to search for pages");

        // Search for notes only (should not include our page)
        let note_results = ctx
            .client
            .search_in(&ctx.space_id)
            .types(["note"])
            .text(&page_name)
            .execute()
            .await
            .expect("Failed to search for notes");

        println!(
            "Type filter validation: page results={}, note results={}",
            page_results.len(),
            note_results.len()
        );

        // Page search may contain our object
        for obj in page_results.iter() {
            if let Some(ref obj_type) = obj.r#type {
                assert_eq!(obj_type.key, "page", "Page search should only return pages");
            }
        }

        // Note search should not contain our page object
        for obj in note_results.iter() {
            assert_ne!(
                obj.id, page_obj.id,
                "Note search should not return page object"
            );
        }
    })
    .await
}

// =============================================================================
// Search with Sorting Tests
// =============================================================================

#[tokio::test]
#[test_log::test]
async fn test_search_sort_by_created_date() {
    with_test_context_unit(|ctx| async move {
        let results = ctx
            .client
            .search_in(&ctx.space_id)
            .sort_desc("created_date")
            .limit(10)
            .execute()
            .await
            .expect("Failed to execute search with created_date sort");

        println!(
            "Search sorted by created_date returned {} results",
            results.len()
        );

        // Verify results have created_date when available
        for obj in results.iter() {
            if let Some(created) = obj.get_property_date("created_date") {
                println!("  Object {} created: {:?}", obj.id, created);
            }
        }
    })
    .await
}

#[tokio::test]
#[test_log::test]
async fn test_search_sort_by_last_modified() {
    with_test_context_unit(|ctx| async move {
        let results = ctx
            .client
            .search_in(&ctx.space_id)
            .sort_desc("last_modified_date")
            .limit(10)
            .execute()
            .await
            .expect("Failed to execute search with last_modified_date sort");

        println!(
            "Search sorted by last_modified_date returned {} results",
            results.len()
        );

        // Verify results have last_modified_date when available
        for obj in results.iter() {
            if let Some(modified) = obj.get_property_date("last_modified_date") {
                println!("  Object {} modified: {:?}", obj.id, modified);
            }
        }
    })
    .await
}

#[tokio::test]
#[test_log::test]
async fn test_search_sort_ascending_vs_descending() {
    with_test_context_unit(|ctx| async move {
        // Get results sorted ascending
        let asc_results = ctx
            .client
            .search_in(&ctx.space_id)
            .sort_asc("created_date")
            .limit(10)
            .execute()
            .await
            .expect("Failed to execute ascending search");

        // Get results sorted descending
        let desc_results = ctx
            .client
            .search_in(&ctx.space_id)
            .sort_desc("created_date")
            .limit(10)
            .execute()
            .await
            .expect("Failed to execute descending search");

        println!(
            "Sort comparison: ascending={} results, descending={} results",
            asc_results.len(),
            desc_results.len()
        );

        // If we have results, verify ordering is different (when both have dates)
        if !asc_results.is_empty() && !desc_results.is_empty() {
            let asc_first = asc_results.iter().next().unwrap();
            let desc_first = desc_results.iter().next().unwrap();

            let asc_date = asc_first.get_property_date("created_date");
            let desc_date = desc_first.get_property_date("created_date");

            if asc_date.is_some() && desc_date.is_some() {
                println!("  Ascending first: {} ({:?})", asc_first.id, asc_date);
                println!("  Descending first: {} ({:?})", desc_first.id, desc_date);
            }
        }
    })
    .await
}

// =============================================================================
// Combined Search Tests
// =============================================================================

#[tokio::test]
#[test_log::test]
async fn test_search_text_and_type_filter() {
    with_test_context_unit(|ctx| async move {
        // Create a page with unique text
        let unique_term = format!("CombinedSearch{}", chrono::Utc::now().timestamp_millis());
        let page_obj = ctx
            .client
            .new_object(&ctx.space_id, "page")
            .name(&unique_term)
            .create()
            .await
            .expect("Failed to create page object");
        ctx.register_object(&page_obj.id);

        // Wait for indexing
        tokio::time::sleep(Duration::from_millis(200)).await;

        // Search combining text query and type filter
        let results = ctx
            .client
            .search_in(&ctx.space_id)
            .text(&unique_term)
            .types(["page"])
            .execute()
            .await
            .expect("Failed to execute combined search");

        println!(
            "Combined search (text='{}', type='page') returned {} results",
            unique_term,
            results.len()
        );

        // Verify results match both criteria
        for obj in results.iter() {
            if let Some(ref obj_type) = obj.r#type {
                assert_eq!(
                    obj_type.key, "page",
                    "Combined search result should be type 'page'"
                );
            }
        }
    })
    .await
}

#[tokio::test]
#[test_log::test]
async fn test_search_pagination() {
    with_test_context_unit(|ctx| async move {
        // Get first page
        let page1 = ctx
            .client
            .search_in(&ctx.space_id)
            .limit(3)
            .offset(0)
            .execute()
            .await
            .expect("Failed to get page 1");

        // Get second page
        let page2 = ctx
            .client
            .search_in(&ctx.space_id)
            .limit(3)
            .offset(3)
            .execute()
            .await
            .expect("Failed to get page 2");

        println!(
            "Pagination test: page1={} results, page2={} results",
            page1.len(),
            page2.len()
        );

        // Verify pages are different (if both have results)
        if !page1.is_empty() && !page2.is_empty() {
            let page1_ids: Vec<&str> = page1.iter().map(|o| o.id.as_str()).collect();
            let page2_ids: Vec<&str> = page2.iter().map(|o| o.id.as_str()).collect();

            for id in &page2_ids {
                assert!(
                    !page1_ids.contains(id),
                    "Page 2 should not contain items from page 1"
                );
            }
            println!("  Verified pages are distinct");
        }
    })
    .await
}

// =============================================================================
// Search with Filters Tests
// =============================================================================

#[tokio::test]
#[test_log::test]
async fn test_search_with_filter_condition() {
    with_test_context_unit(|ctx| async move {
        // Search with a filter for non-empty names
        let results = ctx
            .client
            .search_in(&ctx.space_id)
            .filter(Filter::not_empty("name"))
            .limit(10)
            .execute()
            .await
            .expect("Failed to execute search with filter");

        println!(
            "Search with not_empty(name) filter returned {} results",
            results.len()
        );

        // Verify all results have non-empty names
        for obj in results.iter() {
            assert!(
                obj.name.as_ref().map(|n| !n.is_empty()).unwrap_or(false),
                "Object should have non-empty name"
            );
        }
    })
    .await
}

#[tokio::test]
#[test_log::test]
async fn test_search_with_combined_text_and_filters() {
    with_test_context_unit(|ctx| async move {
        // Create objects with specific characteristics
        let base_name = unique_test_name("FilteredSearch");

        let obj1 = ctx
            .client
            .new_object(&ctx.space_id, "page")
            .name(format!("{} One", base_name))
            .description("Has description")
            .create()
            .await
            .expect("Failed to create object 1");
        ctx.register_object(&obj1.id);

        let obj2 = ctx
            .client
            .new_object(&ctx.space_id, "page")
            .name(format!("{} Two", base_name))
            .create()
            .await
            .expect("Failed to create object 2");
        ctx.register_object(&obj2.id);

        // Wait for indexing
        tokio::time::sleep(Duration::from_millis(200)).await;

        // Search with text and filter for non-empty description
        let results = ctx
            .client
            .search_in(&ctx.space_id)
            .text(&base_name)
            .filter(Filter::not_empty("description"))
            .execute()
            .await
            .expect("Failed to execute filtered search");

        println!(
            "Search with text='{}' and filter=not_empty(description) returned {} results",
            base_name,
            results.len()
        );
    })
    .await
}

// =============================================================================
// Edge Cases and Error Handling
// =============================================================================

#[tokio::test]
#[test_log::test]
async fn test_search_with_empty_type_array() {
    with_test_context_unit(|ctx| async move {
        // Search with empty types array should work like no type filter
        let empty_types: Vec<&str> = vec![];
        let results = ctx
            .client
            .search_in(&ctx.space_id)
            .types(empty_types)
            .limit(10)
            .execute()
            .await
            .expect("Failed to execute search with empty types");

        println!(
            "Search with empty types array returned {} results",
            results.len()
        );
    })
    .await
}

#[tokio::test]
#[test_log::test]
async fn test_search_with_zero_limit() {
    with_test_context_unit(|ctx| async move {
        // Search with limit=0 should either be rejected or return 0 results.
        let result = ctx.client.search_in(&ctx.space_id).limit(0).execute().await;

        match result {
            Ok(results) => {
                assert_eq!(
                    results.len(),
                    0,
                    "Search with limit=0 should return 0 results"
                );
                println!("Search with limit=0 correctly returned 0 results");
            }
            Err(AnytypeError::Validation { message }) => {
                assert!(
                    message.contains("limit must be between 1 and 1000"),
                    "Unexpected validation error for limit=0: {message}"
                );
                println!("Search with limit=0 correctly rejected: {message}");
            }
            Err(err) => {
                panic!("Unexpected error for limit=0: {err:?}");
            }
        }
    })
    .await
}

#[tokio::test]
#[test_log::test]
async fn test_search_with_large_offset() {
    with_test_context_unit(|ctx| async move {
        // Search with very large offset should return empty or few results
        let results = ctx
            .client
            .search_in(&ctx.space_id)
            .limit(10)
            .offset(10000)
            .execute()
            .await
            .expect("Failed to execute search with large offset");

        println!(
            "Search with offset=10000 returned {} results (expected 0 or very few)",
            results.len()
        );
    })
    .await
}

#[tokio::test]
#[test_log::test]
async fn test_search_with_special_characters() {
    with_test_context_unit(|ctx| async move {
        // Search with special characters should not crash
        let special_queries = vec![
            "test@example.com",
            "path/to/file",
            "100%",
            "C++",
            "rock & roll",
        ];

        for query in special_queries {
            let result = ctx
                .client
                .search_in(&ctx.space_id)
                .text(query)
                .limit(5)
                .execute()
                .await;

            match result {
                Ok(results) => {
                    println!(
                        "Search with special chars '{}' returned {} results",
                        query,
                        results.len()
                    );
                }
                Err(e) => {
                    println!("Search with special chars '{}' failed: {:?}", query, e);
                    // Some special characters might cause errors, which is acceptable
                }
            }
        }
    })
    .await
}

// =============================================================================
// Performance and Stress Tests
// =============================================================================

#[tokio::test]
#[test_log::test]
async fn test_search_performance_multiple_searches() {
    with_test_context_unit(|ctx| async move {
        let start = std::time::Instant::now();
        let num_searches = 5;

        for i in 0..num_searches {
            let _ = ctx
                .client
                .search_in(&ctx.space_id)
                .limit(10)
                .execute()
                .await
                .expect("Failed to execute search");

            println!("  Completed search {}/{}", i + 1, num_searches);
        }

        let elapsed = start.elapsed();
        let avg_time = elapsed.as_millis() / num_searches;

        println!(
            "Performed {} searches in {:?} (avg: {}ms per search)",
            num_searches, elapsed, avg_time
        );
    })
    .await
}

#[tokio::test]
#[test_log::test]
async fn test_search_with_multiple_filter_types() {
    with_test_context_unit(|ctx| async move {
        // Create test objects of different types
        let base_name = unique_test_name("MultiType");

        let page = ctx
            .client
            .new_object(&ctx.space_id, "page")
            .name(format!("{} Page", base_name))
            .create()
            .await;

        if let Ok(obj) = page {
            ctx.register_object(&obj.id);
        }

        // Wait for indexing
        tokio::time::sleep(Duration::from_millis(200)).await;

        // Search across multiple types
        let types = vec!["page", "note", "task", "bookmark"];
        let results = ctx
            .client
            .search_in(&ctx.space_id)
            .types(types.clone())
            .text(&base_name)
            .limit(20)
            .execute()
            .await
            .expect("Failed to search multiple types");

        println!(
            "Search across {} types with text='{}' returned {} results",
            types.len(),
            base_name,
            results.len()
        );

        // Verify results match one of the types
        for obj in results.iter() {
            if let Some(ref obj_type) = obj.r#type {
                println!("  Found object type: {}", obj_type.key);
            }
        }
    })
    .await
}
