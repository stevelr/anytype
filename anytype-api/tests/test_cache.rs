//! Cache Behavior Integration Tests for anytype
//!
//! This test suite validates the caching behavior of the anytype client,
//! including cache warming, cache clearing, cache isolation, and cache disabled mode.
//!
//! ## Cache Design
//!
//! The client maintains in-memory caches for:
//! - Spaces: Global cache (not space-specific)
//! - Properties: Per-space cache (keyed by space_id)
//! - Types: Per-space cache (keyed by space_id)
//!
//! There are no apis for updating or deleting a single item, or marking items dirty.
//! Updating and Clearing cache based on application usage patterns
//! is the responsibility of the application.
//!
//! ### Cache Warming
//!
//! When calling `list()` for properties/types/spaces, the cache is populated.
//! Subsequent `get()` calls use the cache instead of making API calls.
//!
//! If `get()` is called first and cache is empty, it automatically warms the cache
//! by doing a full list operation, then returns the requested item from cache.
//!
//! ### Cache Clearing
//!
//! - `clear()` - clear everything
//! - `clear_space()` - clear properties and types for a space (but not the space itself)
//! - `clear_spaces()` - clears all spaces
//! - `clear_properties(Some(space_id))` - clears properties for one space
//! - `clear_properties(None)` - clears properties for all spaces
//! - `clear_types(Some(space_id))` - clears types for one space
//! - `clear_types(None)` - clears types for all spaces
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
//! Since there is one "global" cache (per client), cache tests only run serialized.
//! The cache is thread-safe, locked by per-entity mutexes, but tests regularly clear
//! the cache to reset state, and count number of items in the cache,
//! so they must be serialized to make them deterministic.
//!
//! The current design is fairly simplistic, in that there is no fine-grained cache
//! invalidation or updates, so multi-threaded tests and complex invalidation tests are
//! out of scope.
//!
//! ```bash
//! source .test-env
//! cargo test -p anytype --test test_cache
//! ```

mod common;

// =============================================================================
// Cache Warmth Tests
// =============================================================================

#[cfg(test)]
mod cache_warmth {
    use anytype::test_util::*;
    use serial_test::serial;
    use test_log::test;

    /// Test that listing properties warms the cache and get uses it
    #[tokio::test]
    #[test_log::test]
    #[serial]
    async fn test_properties_cache_warmth() {
        with_test_context_unit(|ctx| async move {
            // Clear cache to ensure clean state
            ctx.client.cache().clear_properties(None);

            // Verify cache is empty
            assert_eq!(
                ctx.client.cache().num_properties(),
                0,
                "Cache should be empty initially"
            );

            // List properties - this should warm the cache
            let properties = ctx
                .client
                .properties(&ctx.space_id)
                .list()
                .await
                .expect("Failed to list properties");

            assert!(!properties.is_empty(), "Should have at least one property");

            // Verify cache is now populated
            let cache_count = ctx.client.cache().num_properties();
            assert!(
                cache_count > 0,
                "Cache should be populated after list (got {})",
                cache_count
            );

            // Get metrics after list to establish baseline
            let metrics_after_list = ctx.client.http_metrics();

            // Get a specific property - should use cache (no API call)
            let first_property = properties.iter().next().unwrap();
            let property = ctx
                .client
                .property(&ctx.space_id, &first_property.id)
                .get()
                .await
                .expect("Failed to get property");

            assert_eq!(property.id, first_property.id);
            assert_eq!(property.key, first_property.key);

            // Verify no additional HTTP requests were made (proving it used cache)
            let metrics_after_get = ctx.client.http_metrics();
            assert_eq!(
                metrics_after_get.total_requests, metrics_after_list.total_requests,
                "No additional HTTP requests should be made when using cached data"
            );

            // Cleanup
            ctx.client.cache().clear_properties(None);
        })
        .await
    }

    /// Test that listing types warms the cache and get uses it
    #[test(tokio::test)]
    #[test_log::test]
    #[serial]
    async fn test_types_cache_warmth() {
        with_test_context_unit(|ctx| async move {
            // Clear cache to ensure clean state
            ctx.client.cache().clear_types(None);

            // Verify cache is empty
            assert_eq!(
                ctx.client.cache().num_types(),
                0,
                "Cache should be empty initially"
            );

            // List types - this should warm the cache
            let types = ctx
                .client
                .types(&ctx.space_id)
                .list()
                .await
                .expect("Failed to list types");

            assert!(!types.is_empty(), "Should have at least one type");

            // Verify cache is now populated
            let cache_count = ctx.client.cache().num_types();
            assert!(
                cache_count > 0,
                "Cache should be populated after list (got {})",
                cache_count
            );

            // Get metrics after list to establish baseline
            let metrics_after_list = ctx.client.http_metrics();

            // Get a specific type - should use cache (no API call)
            let first_type = types.iter().next().unwrap();
            let typ = ctx
                .client
                .get_type(&ctx.space_id, &first_type.id)
                .get()
                .await
                .expect("Failed to get type");

            assert_eq!(typ.id, first_type.id);
            assert_eq!(typ.key, first_type.key);

            // Verify no additional HTTP requests were made (proving it used cache)
            let metrics_after_get = ctx.client.http_metrics();
            assert_eq!(
                metrics_after_get.total_requests, metrics_after_list.total_requests,
                "No additional HTTP requests should be made when using cached data"
            );

            // Cleanup
            ctx.client.cache().clear_types(None);
        })
        .await
    }

    /// Test that listing spaces warms the cache and get uses it
    #[test(tokio::test)]
    #[test_log::test]
    #[serial]
    async fn test_spaces_cache_warmth() {
        with_test_context_unit(|ctx| async move {
            let spaces = ctx.client.spaces().list().await.expect("get spaces");
            eprintln!("TMP200 Warmth found {} spaces", spaces.len());

            // Clear cache to ensure clean state
            ctx.client.cache().clear();

            // Verify cache is empty
            assert_eq!(
                ctx.client.cache().num_spaces(),
                0,
                "Cache should be empty initially"
            );

            // List spaces - this should warm the cache
            let spaces = ctx
                .client
                .spaces()
                .list()
                .await
                .expect("Failed to list spaces");

            eprintln!("TMP216 list spaces found {} spaces", spaces.len());
            assert!(!spaces.is_empty(), "Should have at least one space");

            // Verify cache is now populated
            let cache_count = ctx.client.cache().num_spaces();
            assert!(
                cache_count > 0,
                "Cache should be populated after list (got {})",
                cache_count
            );
            eprintln!("TMP226, num_spaces in cache: {cache_count}");

            // Get metrics after list to establish baseline
            let metrics_after_list = ctx.client.http_metrics();

            // Get a specific space - should use cache (no API call)
            let first_space = spaces.iter().next().unwrap();
            let space = ctx
                .client
                .space(&first_space.id)
                .get()
                .await
                .expect("Failed to get space");

            assert_eq!(space.id, first_space.id);

            // Verify no additional HTTP requests were made (proving it used cache)
            let metrics_after_get = ctx.client.http_metrics();
            assert_eq!(
                metrics_after_get.total_requests, metrics_after_list.total_requests,
                "No additional HTTP requests should be made when using cached data"
            );

            // Cleanup
            ctx.client.cache().clear();
        })
        .await
    }

    /// Test that get() auto-warms cache when empty
    #[test(tokio::test)]
    #[test_log::test]
    #[serial]
    async fn test_get_auto_warms_cache() {
        with_test_context_unit(|ctx| async move {
            // Clear cache to ensure clean state
            ctx.client.cache().clear_properties(None);

            // Verify cache is empty
            assert_eq!(
                ctx.client.cache().num_properties(),
                0,
                "Cache should be empty initially"
            );

            // First, get a property ID by listing (then clear cache)
            let properties = ctx
                .client
                .properties(&ctx.space_id)
                .list()
                .await
                .expect("Failed to list properties");
            let property_id = properties.iter().next().unwrap().id.clone();

            // Clear cache again
            ctx.client.cache().clear_properties(None);
            assert_eq!(ctx.client.cache().num_properties(), 0);

            // Get property directly - should auto-warm cache via list
            let property = ctx
                .client
                .property(&ctx.space_id, &property_id)
                .get()
                .await
                .expect("Failed to get property");

            assert_eq!(property.id, property_id);

            // Verify cache was warmed
            assert!(
                ctx.client.cache().num_properties() > 0,
                "Cache should be warmed after get() with empty cache"
            );

            // Cleanup
            ctx.client.cache().clear_properties(None);
        })
        .await
    }
}

// =============================================================================
// Cache Clearing Tests
// =============================================================================

mod cache_clearing {
    use anytype::test_util::*;
    use serial_test::serial;
    use test_log::test;

    /// Test clearing properties cache for a specific space
    #[test(tokio::test)]
    #[test_log::test]
    #[serial]
    async fn test_clear_properties_cache_targeted() {
        with_test_context_unit(|ctx| async move {
            // Clear all caches first
            ctx.client.cache().clear_properties(None);

            // Warm the cache for our test space
            let properties = ctx
                .client
                .properties(&ctx.space_id)
                .list()
                .await
                .expect("Failed to list properties")
                .collect_all()
                .await
                .expect("collect-all properties");

            let initial_count = ctx.client.cache().num_properties();
            assert!(initial_count > 0, "Cache should be populated after list");
            assert_eq!(properties.len(), initial_count);

            // Clear cache for this specific space
            ctx.client.cache().clear_properties(Some(&ctx.space_id));

            // Verify cache is now empty
            assert_eq!(
                ctx.client.cache().num_properties(),
                0,
                "Cache should be empty after targeted clear"
            );

            // Re-list to warm cache again
            let properties_next = ctx
                .client
                .properties(&ctx.space_id)
                .list()
                .await
                .expect("Failed to re-list properties")
                .collect_all()
                .await
                .expect("collect properties_next");

            let temp_dir = ctx.temp_dir("properties").expect("temp dir");
            let file1 = temp_dir.join("properties1.json");
            let file2 = temp_dir.join("properties2.json");

            std::fs::write(&file1, serde_json::to_string_pretty(&properties).unwrap())
                .expect("dump json1");

            std::fs::write(
                &file2,
                serde_json::to_string_pretty(&properties_next).unwrap(),
            )
            .expect("dump json2");

            assert_eq!(ctx.client.cache().num_properties(), properties_next.len());
            assert_eq!(
                properties_next.len(),
                initial_count,
                "Cache should be re-populated with same count"
            );

            // Cleanup
            ctx.client.cache().clear_properties(None);
        })
        .await
    }

    /// Test clearing properties cache globally (all spaces)
    #[test(tokio::test)]
    #[test_log::test]
    #[serial]
    async fn test_clear_properties_cache_global() {
        with_test_context_unit(|ctx| async move {
            // Clear all caches first
            ctx.client.cache().clear_properties(None);

            // Warm the cache
            ctx.client
                .properties(&ctx.space_id)
                .list()
                .await
                .expect("Failed to list properties");

            assert!(
                ctx.client.cache().num_properties() > 0,
                "Cache should be populated"
            );

            // Clear all properties caches
            ctx.client.cache().clear_properties(None);

            // Verify cache is empty
            assert_eq!(
                ctx.client.cache().num_properties(),
                0,
                "Cache should be empty after global clear"
            );

            // Cleanup (redundant but consistent)
            ctx.client.cache().clear_properties(None);
        })
        .await
    }

    /// Test clearing types cache for a specific space
    #[test(tokio::test)]
    #[test_log::test]
    #[serial]
    async fn test_clear_types_cache_targeted() {
        with_test_context_unit(|ctx| async move {
            // Clear all caches first
            ctx.client.cache().clear_types(None);

            // Warm the cache for our test space
            ctx.client
                .types(&ctx.space_id)
                .list()
                .await
                .expect("Failed to list types");

            let initial_count = ctx.client.cache().num_types();
            assert!(initial_count > 0, "Cache should be populated after list");

            // Clear cache for this specific space
            ctx.client.cache().clear_types(Some(&ctx.space_id));

            // Verify cache is now empty
            assert_eq!(
                ctx.client.cache().num_types(),
                0,
                "Cache should be empty after targeted clear"
            );

            // Re-list to warm cache again
            ctx.client
                .types(&ctx.space_id)
                .list()
                .await
                .expect("Failed to re-list types");

            assert_eq!(
                ctx.client.cache().num_types(),
                initial_count,
                "Cache should be re-populated with same count"
            );

            // Cleanup
            ctx.client.cache().clear_types(None);
        })
        .await
    }

    /// Test clearing types cache globally (all spaces)
    #[test(tokio::test)]
    #[test_log::test]
    #[serial]
    async fn test_cache_clear_types_global() {
        with_test_context_unit(|ctx| async move {
            // Clear all caches first
            ctx.client.cache().clear_types(None);

            // Warm the cache
            ctx.client
                .types(&ctx.space_id)
                .list()
                .await
                .expect("Failed to list types");

            assert!(
                ctx.client.cache().num_types() > 0,
                "Cache should be populated"
            );

            // Clear all types caches
            ctx.client.cache().clear_types(None);

            // Verify cache is empty
            assert_eq!(
                ctx.client.cache().num_types(),
                0,
                "Cache should be empty after global clear"
            );

            // Cleanup (redundant but consistent)
            ctx.client.cache().clear_types(None);
        })
        .await
    }

    /// Test that clearing cache multiple times is idempotent
    #[test(tokio::test)]
    #[test_log::test]
    #[serial]
    async fn test_cache_clear_idempotent() {
        with_test_context_unit(|ctx| async move {
            // Clear cache multiple times - should not fail
            ctx.client.cache().clear_properties(None);
            ctx.client.cache().clear_properties(None);

            ctx.client.cache().clear_types(None);
            ctx.client.cache().clear_types(None);

            ctx.client.cache().clear_spaces();
            ctx.client.cache().clear_spaces();

            ctx.client.cache().clear();
            ctx.client.cache().clear();

            // All caches should be empty
            assert_eq!(ctx.client.cache().num_properties(), 0);
            assert_eq!(ctx.client.cache().num_types(), 0);
            assert_eq!(ctx.client.cache().num_spaces(), 0);

            // Clear specific space cache when cache is empty - should not fail
            ctx.client.cache().clear_properties(Some(&ctx.space_id));
            ctx.client.cache().clear_types(Some(&ctx.space_id));

            // Cleanup (redundant but consistent)
            ctx.client.cache().clear();
        })
        .await
    }
}

// =============================================================================
// Cache Disabled Tests
// =============================================================================

mod cache_disabled {
    use anytype::prelude::*;
    use serial_test::serial;
    use test_log::test;

    /// Test that when cache is disabled, list operations don't populate cache
    #[test(tokio::test)]
    #[test_log::test]
    #[serial]
    async fn test_cache_disabled_via_config() {
        // Create a client with cache disabled
        let base_url = std::env::var("ANYTYPE_TEST_URL")
            .unwrap_or_else(|_| "http://127.0.0.1:31012".to_string());
        let space_id =
            std::env::var("ANYTYPE_TEST_SPACE_ID").expect("ANYTYPE_TEST_SPACE_ID required");
        let api_key_path =
            std::env::var("ANYTYPE_TEST_KEY_FILE").expect("ANYTYPE_TEST_KEY_FILE required");

        let config = ClientConfig {
            base_url,
            app_name: "anytype-cache-test".to_string(),
            rate_limit_max_retries: 0,
            disable_cache: true, // Disable cache
            ..Default::default()
        };

        let client = AnytypeClient::with_config(config)
            .expect("Failed to create client")
            .set_key_store(
                KeyStoreFile::from_path(&api_key_path).expect("Failed to create keystore"),
            );

        client.load_key(false).expect("Failed to load key");

        // Verify cache is initially empty
        assert_eq!(client.cache().num_properties(), 0);
        assert_eq!(client.cache().num_types(), 0);
        assert_eq!(client.cache().num_spaces(), 0);

        // List properties - should NOT populate cache
        let properties = client
            .properties(&space_id)
            .list()
            .await
            .expect("Failed to list properties");

        assert!(!properties.is_empty(), "Should have properties");

        // Cache should still be empty
        assert_eq!(
            client.cache().num_properties(),
            0,
            "Cache should remain empty when disabled"
        );

        // List types - should NOT populate cache
        let types = client
            .types(&space_id)
            .list()
            .await
            .expect("Failed to list types");

        assert!(!types.is_empty(), "Should have types");

        // Cache should still be empty
        assert_eq!(
            client.cache().num_types(),
            0,
            "Cache should remain empty when disabled"
        );

        // List spaces - should NOT populate cache
        let spaces = client.spaces().list().await.expect("Failed to list spaces");

        assert!(!spaces.is_empty(), "Should have spaces");

        // Cache should still be empty
        assert_eq!(
            client.cache().num_spaces(),
            0,
            "Cache should remain empty when disabled"
        );

        // Get operations should also not use or populate cache
        let first_property = properties.iter().next().unwrap();
        let _property = client
            .property(&space_id, &first_property.id)
            .get()
            .await
            .expect("Failed to get property");

        assert_eq!(
            client.cache().num_properties(),
            0,
            "Cache should remain empty after get with cache disabled"
        );
    }
}

// =============================================================================
// Cache Isolation Tests
// =============================================================================

mod cache_isolation {
    use anytype::test_util::*;
    use serial_test::serial;
    use test_log::test;

    /// Test that cache for space A doesn't affect space B
    #[test(tokio::test)]
    #[test_log::test]
    #[serial]
    async fn test_cache_space_isolation() {
        with_test_context_unit(|ctx| async move {
            // Clear all caches
            ctx.client.cache().clear();

            // Get all spaces to find a second space (if available)
            let spaces = ctx
                .client
                .spaces()
                .list()
                .await
                .expect("Failed to list spaces");

            if spaces.len() < 2 {
                println!(
                    "Skipping cache isolation test - need at least 2 spaces, found {}",
                    spaces.len()
                );
                return;
            }

            let space_a = &ctx.space_id;
            let space_b = spaces
                .iter()
                .find(|s| s.id != *space_a)
                .map(|s| &s.id)
                .expect("Failed to find second space");

            // Warm cache for space A
            ctx.client
                .properties(space_a)
                .list()
                .await
                .expect("Failed to list properties for space A");

            let cache_count_after_a = ctx.client.cache().num_properties();
            assert!(cache_count_after_a > 0, "Space A cache should be populated");

            // Warm cache for space B
            ctx.client
                .properties(space_b)
                .list()
                .await
                .expect("Failed to list properties for space B");

            let cache_count_after_b = ctx.client.cache().num_properties();
            assert!(
                cache_count_after_b >= cache_count_after_a,
                "Cache should include both spaces"
            );

            // Clear cache for space A only
            ctx.client.cache().clear_properties(Some(space_a));

            // Space B cache should still exist
            assert!(
                ctx.client.cache().has_properties(space_b),
                "space b property cache should not be empty"
            );

            // Space A cache should be gone
            //
            assert!(
                !ctx.client.cache().has_properties(space_a),
                "space a property cache should be empty"
            );

            // Cleanup
            ctx.client.cache().clear();
        })
        .await
    }

    /// Test cache isolation between different resource types
    #[test(tokio::test)]
    #[test_log::test]
    #[serial]
    async fn test_cache_isolation_between_types() {
        with_test_context_unit(|ctx| async move {
            // Clear all caches
            ctx.client.cache().clear();

            // Warm properties cache
            ctx.client
                .properties(&ctx.space_id)
                .list()
                .await
                .expect("Failed to list properties");

            assert!(ctx.client.cache().num_properties() > 0);
            assert_eq!(ctx.client.cache().num_types(), 0);
            assert_eq!(ctx.client.cache().num_spaces(), 0);

            // Warm types cache
            ctx.client
                .types(&ctx.space_id)
                .list()
                .await
                .expect("Failed to list types");

            assert!(ctx.client.cache().num_properties() > 0);
            assert!(ctx.client.cache().num_types() > 0);
            assert_eq!(ctx.client.cache().num_spaces(), 0);

            // Warm spaces cache
            let spaces = ctx
                .client
                .spaces()
                .list()
                .await
                .expect("Failed to list spaces");
            eprintln!("(2) Found {} spaces", spaces.len());

            assert!(ctx.client.cache().num_properties() > 0);
            assert!(ctx.client.cache().num_types() > 0);
            assert!(ctx.client.cache().num_spaces() > 0);

            // Clear only properties
            ctx.client.cache().clear_properties(None);

            assert_eq!(ctx.client.cache().num_properties(), 0);
            assert!(
                ctx.client.cache().num_types() > 0,
                "Types cache should be unaffected"
            );
            assert!(
                ctx.client.cache().num_spaces() > 0,
                "Spaces cache should be unaffected"
            );

            // Clear only types
            ctx.client.cache().clear_types(None);

            assert_eq!(ctx.client.cache().num_properties(), 0);
            assert_eq!(ctx.client.cache().num_types(), 0);
            assert!(
                ctx.client.cache().num_spaces() > 0,
                "Spaces cache should be unaffected"
            );

            // Cleanup
            ctx.client.cache().clear();
        })
        .await
    }
}

// =============================================================================
// Cache Query Behavior Tests
// =============================================================================

mod cache_query_behavior {
    use anytype::test_util::*;
    use serial_test::serial;
    use test_log::test;

    /// Test that cache introspection methods work correctly
    #[test(tokio::test)]
    #[test_log::test]
    #[serial]
    async fn test_cache_introspection() {
        with_test_context_unit(|ctx| async move {
            // Clear all caches
            ctx.client.cache().clear();

            // Verify all counts are zero
            assert_eq!(ctx.client.cache().num_properties(), 0);
            assert_eq!(ctx.client.cache().num_types(), 0);
            assert_eq!(ctx.client.cache().num_spaces(), 0);

            // Warm properties cache
            ctx.client
                .properties(&ctx.space_id)
                .list()
                .await
                .expect("Failed to list properties");

            let prop_count = ctx.client.cache().num_properties();
            assert!(prop_count > 0);

            // Get properties for specific space

            assert!(
                ctx.client.cache().has_properties(&ctx.space_id),
                "Should have properties for test space"
            );

            // Get properties for non-existent space
            assert!(
                !ctx.client.cache().has_properties("non-existent space"),
                "Should not have properties for non-existent space"
            );

            // Warm types cache
            ctx.client
                .types(&ctx.space_id)
                .list()
                .await
                .expect("Failed to list types");

            let type_count = ctx.client.cache().num_types();
            assert!(type_count > 0);

            // Cleanup
            ctx.client.cache().clear();
        })
        .await
    }

    /// Test that cache returns complete data structures
    #[test(tokio::test)]
    #[test_log::test]
    #[serial]
    async fn test_cache_returns_complete_data() {
        with_test_context_unit(|ctx| async move {
            // Clear cache
            ctx.client.cache().clear_properties(None);

            // Warm cache
            let properties_from_list = ctx
                .client
                .properties(&ctx.space_id)
                .list()
                .await
                .expect("Failed to list properties");

            let first_prop = properties_from_list.iter().next().unwrap();

            // Get from cache
            let property_from_cache = ctx
                .client
                .property(&ctx.space_id, &first_prop.id)
                .get()
                .await
                .expect("Failed to get property from cache");

            // Verify all fields match
            assert_eq!(property_from_cache.id, first_prop.id);
            assert_eq!(property_from_cache.key, first_prop.key);
            assert_eq!(property_from_cache.name, first_prop.name);
            assert_eq!(property_from_cache.format(), first_prop.format());

            // Cleanup
            ctx.client.cache().clear_properties(None);
        })
        .await
    }
}
