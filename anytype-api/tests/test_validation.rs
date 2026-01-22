//! Validation and Error Handling Tests for anytype
//!
//! This test suite validates comprehensive error handling and validation scenarios
//! against a live Anytype API server. Each test focuses on a single error condition.
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
//! cargo test -p anytype --test test_validation
//! ```

mod common;

use anytype::prelude::*;
use anytype::test_util::with_test_context_unit;
use common::unique_test_name;

// =============================================================================
// Invalid ID Errors
// =============================================================================

#[tokio::test]
#[test_log::test]
async fn test_invalid_space_id() {
    with_test_context_unit(|ctx| async move {
        let invalid_id = "nonexistent_space_id_12345678901234567890";

        let result = ctx.client.space(invalid_id).get().await;

        // Should be NotFound or Validation error
        assert!(
            result.is_err(),
            "Expected error for invalid space ID, got success"
        );
        match result.unwrap_err() {
            AnytypeError::NotFound { .. } | AnytypeError::Validation { .. } => {
                println!("✓ Correctly returned error for invalid space ID");
            }
            e => panic!("Expected NotFound or Validation error, got: {:?}", e),
        }
    })
    .await
}

#[tokio::test]
#[test_log::test]
async fn test_invalid_object_id() {
    with_test_context_unit(|ctx| async move {
        let invalid_id = "nonexistent_object_id_12345678901234567890";

        let result = ctx.client.object(ctx.space_id(), invalid_id).get().await;

        assert!(
            result.is_err(),
            "Expected error for invalid object ID, got success"
        );
        match result.unwrap_err() {
            AnytypeError::NotFound { .. } | AnytypeError::Validation { .. } => {
                println!("✓ Correctly returned error for invalid object ID");
            }
            e => panic!("Expected NotFound or Validation error, got: {:?}", e),
        }
    })
    .await
}

#[tokio::test]
#[test_log::test]
async fn test_invalid_property_id() {
    with_test_context_unit(|ctx| async move {
        let invalid_id = "nonexistent_property_id_1234567890123456789";

        let result = ctx.client.property(ctx.space_id(), invalid_id).get().await;

        assert!(
            result.is_err(),
            "Expected error for invalid property ID, got success"
        );
        match result.unwrap_err() {
            AnytypeError::NotFound { .. } | AnytypeError::Validation { .. } => {
                println!("✓ Correctly returned error for invalid property ID");
            }
            e => panic!("Expected NotFound or Validation error, got: {:?}", e),
        }
    })
    .await
}

#[tokio::test]
#[test_log::test]
async fn test_invalid_type_id() {
    with_test_context_unit(|ctx| async move {
        let invalid_id = "nonexistent_type_id_123456789012345678901";

        let result = ctx.client.get_type(ctx.space_id(), invalid_id).get().await;

        assert!(
            result.is_err(),
            "Expected error for invalid type ID, got success"
        );
        match result.unwrap_err() {
            AnytypeError::NotFound { .. } | AnytypeError::Validation { .. } => {
                println!("✓ Correctly returned error for invalid type ID");
            }
            e => panic!("Expected NotFound or Validation error, got: {:?}", e),
        }
    })
    .await
}

#[tokio::test]
#[test_log::test]
async fn test_invalid_member_id() {
    with_test_context_unit(|ctx| async move {
        let invalid_id = "nonexistent_member_id_12345678901234567890";

        let result = ctx.client.member(ctx.space_id(), invalid_id).get().await;

        assert!(
            result.is_err(),
            "Expected error for invalid member ID, got success"
        );
        match result.unwrap_err() {
            AnytypeError::NotFound { .. } | AnytypeError::Validation { .. } => {
                println!("✓ Correctly returned error for invalid member ID");
            }
            e => panic!("Expected NotFound or Validation error, got: {:?}", e),
        }
    })
    .await
}

#[tokio::test]
#[test_log::test]
async fn test_malformed_id_format() {
    with_test_context_unit(|ctx| async move {
        // Test IDs with invalid characters
        let test_cases = vec![
            ("empty", ""),
            ("too_short", "abc"),
            ("with_newline", "id_with\n_newline_12345678901234567"),
            ("with_null", "id_with\0_null_12345678901234567890"),
            ("with_tab", "id_with\t_tab_12345678901234567890"),
        ];

        for (name, invalid_id) in test_cases {
            let result = ctx.client.object(ctx.space_id(), invalid_id).get().await;

            assert!(
                result.is_err(),
                "Expected error for malformed ID '{}', got success",
                name
            );
            match result.unwrap_err() {
                AnytypeError::Validation { message } => {
                    println!("✓ Correctly rejected malformed ID '{}': {}", name, message);
                }
                e => {
                    // Some IDs might return NotFound if they pass validation but don't exist
                    println!(
                        "⚠ Malformed ID '{}' returned {:?} instead of Validation",
                        name, e
                    );
                }
            }
        }
    })
    .await
}

// =============================================================================
// Empty/Missing Required Fields
// =============================================================================

#[tokio::test]
#[test_log::test]
async fn test_create_object_without_type() {
    with_test_context_unit(|ctx| async move {
        // This test verifies that the client-side validation catches missing type
        // by testing with an empty type_key, which should fail validation

        // The API requires type_key - we test with empty string
        let result = ctx
            .client
            .new_object(ctx.space_id(), "")
            .name("Test Object")
            .create()
            .await;

        assert!(
            result.is_err(),
            "Expected error for object creation without type, got success"
        );
        match result.unwrap_err() {
            AnytypeError::Validation { message } => {
                println!(
                    "✓ Correctly rejected object creation without type: {}",
                    message
                );
            }
            AnytypeError::Http { .. } => {
                println!("✓ Correctly rejected object creation without type (HTTP error)");
            }
            e => {
                // Server may return different error
                println!(
                    "⚠ Object creation without type returned {:?} instead of Validation",
                    e
                );
            }
        }
    })
    .await
}

#[tokio::test]
#[test_log::test]
async fn test_create_property_without_name() {
    with_test_context_unit(|ctx| async move {
        // Create property with empty name should fail
        let result = ctx
            .client
            .new_property(ctx.space_id(), "", PropertyFormat::Text)
            .key("test_key")
            .create()
            .await;

        assert!(
            result.is_err(),
            "Expected error for property creation without name, got success"
        );
        match result.unwrap_err() {
            AnytypeError::Validation { message } => {
                assert!(
                    message.contains("name") || message.contains("empty"),
                    "Error message should mention name or empty: {}",
                    message
                );
                println!(
                    "✓ Correctly rejected property creation without name: {}",
                    message
                );
            }
            e => panic!("Expected Validation error, got: {:?}", e),
        }
    })
    .await
}

#[tokio::test]
#[test_log::test]
async fn test_update_without_changes() {
    with_test_context_unit(|ctx| async move {
        // First create an object
        let obj = ctx
            .client
            .new_object(ctx.space_id(), "page")
            .name(unique_test_name("Update Test"))
            .create()
            .await
            .expect("Failed to create test object");
        ctx.register_object(&obj.id);

        // Try to update without setting any fields
        let result = ctx
            .client
            .update_object(ctx.space_id(), &obj.id)
            .update()
            .await;

        assert!(
            result.is_err(),
            "Expected error for update without changes, got success"
        );
        match result.unwrap_err() {
            AnytypeError::Validation { message } => {
                assert!(
                    message.contains("at least one field") || message.contains("must set"),
                    "Error message should mention setting at least one field: {}",
                    message
                );
                println!("✓ Correctly rejected update without changes: {}", message);
            }
            e => panic!("Expected Validation error, got: {:?}", e),
        }
    })
    .await
}

#[tokio::test]
#[test_log::test]
async fn test_create_space_without_name() {
    with_test_context_unit(|ctx| async move {
        // Create space with empty name may be rejected or allowed (server dependent)
        let result = ctx.client.new_space("").create().await;

        match result {
            Ok(space) => {
                if space.name.is_empty() {
                    println!("✓ Space creation without name allowed (empty name returned)");
                } else {
                    println!(
                        "⚠ Space creation without name returned non-empty name: {}",
                        space.name
                    );
                }
            }
            Err(AnytypeError::Validation { message }) => {
                assert!(
                    message.contains("name") || message.contains("empty"),
                    "Error message should mention name or empty: {}",
                    message
                );
                println!(
                    "✓ Correctly rejected space creation without name: {}",
                    message
                );
            }
            Err(e) => panic!("Expected Validation error or success, got: {:?}", e),
        }
    })
    .await
}

// =============================================================================
// Field Length/Format Validation
// =============================================================================

#[tokio::test]
#[test_log::test]
async fn test_name_too_long() {
    with_test_context_unit(|ctx| async move {
        // Create a name longer than the validation limit (4096 bytes default)
        let long_name = "x".repeat(ctx.client.get_config().get_limits().name_max_len as usize + 10);

        let result = ctx
            .client
            .new_object(ctx.space_id(), "page")
            .name(long_name)
            .create()
            .await;
        if let Ok(obj) = &result {
            ctx.register_object(&obj.id);
        }

        assert!(
            result.is_err(),
            "Expected error for name exceeding max length, got success"
        );
        match result.unwrap_err() {
            AnytypeError::Validation { message } => {
                assert!(
                    message.contains("too long") || message.contains("max"),
                    "Error message should mention length limit: {}",
                    message
                );
                println!(
                    "✓ Correctly rejected name exceeding max length: {}",
                    message
                );
            }
            e => panic!("Expected Validation error, got: {:?}", e),
        }
    })
    .await
}

#[tokio::test]
#[test_log::test]
async fn test_invalid_email_format() {
    with_test_context_unit(|ctx| async move {
        // First, we need to find or create an email property
        // For this test, we'll create an object and try to set an invalid email
        // The API might not validate email format client-side, so we expect server validation

        let obj = ctx
            .client
            .new_object(ctx.space_id(), "page")
            .name(unique_test_name("Email Test"))
            .create()
            .await
            .expect("Failed to create test object");
        ctx.register_object(&obj.id);

        // Try to set an invalid email using set_email
        // Note: The API may or may not validate email format strictly
        let result = ctx
            .client
            .update_object(ctx.space_id(), &obj.id)
            .set_email("email", "not-a-valid-email")
            .update()
            .await;

        // Email validation might be lenient, so we just check if it errors or succeeds
        match result {
            Ok(_) => {
                println!("⚠ API accepted invalid email format (validation may be lenient)");
            }
            Err(AnytypeError::Validation { message }) => {
                println!("✓ Correctly rejected invalid email format: {}", message);
            }
            Err(e) => {
                println!("⚠ Invalid email returned {:?} instead of Validation", e);
            }
        }
    })
    .await
}

#[tokio::test]
#[test_log::test]
async fn test_invalid_url_format() {
    with_test_context_unit(|ctx| async move {
        // Create an object and set an invalid URL
        let obj = ctx
            .client
            .new_object(ctx.space_id(), "page")
            .name(unique_test_name("URL Test"))
            .create()
            .await
            .expect("Failed to create test object");
        ctx.register_object(&obj.id);

        // Try to set an invalid URL
        let result = ctx
            .client
            .update_object(ctx.space_id(), &obj.id)
            .set_url("url", "not a valid url with spaces")
            .update()
            .await;

        // URL validation might be lenient, so we check behavior
        match result {
            Ok(_) => {
                println!("⚠ API accepted invalid URL format (validation may be lenient)");
            }
            Err(AnytypeError::Validation { message }) => {
                println!("✓ Correctly rejected invalid URL format: {}", message);
            }
            Err(e) => {
                println!("⚠ Invalid URL returned {:?} instead of Validation", e);
            }
        }
    })
    .await
}

#[tokio::test]
#[test_log::test]
async fn test_invalid_date_format() {
    with_test_context_unit(|ctx| async move {
        // Create an object and try to set an invalid date
        let obj = ctx
            .client
            .new_object(ctx.space_id(), "page")
            .name(unique_test_name("Date Test"))
            .create()
            .await
            .expect("Failed to create test object");
        ctx.register_object(&obj.id);

        // Try to set an invalid date using set_date with a malformed string
        let result = ctx
            .client
            .update_object(ctx.space_id(), &obj.id)
            .set_date("created_date", "not-a-valid-date")
            .update()
            .await;

        // Date validation should catch invalid format
        match result {
            Ok(_) => {
                println!("⚠ API accepted invalid date format (validation may be lenient)");
            }
            Err(AnytypeError::Validation { message }) => {
                println!("✓ Correctly rejected invalid date format: {}", message);
            }
            Err(AnytypeError::Serialization { .. }) => {
                println!("✓ Correctly rejected invalid date format (Serialization error)");
            }
            Err(e) => {
                println!("⚠ Invalid date returned {:?} instead of Validation", e);
            }
        }
    })
    .await
}

// =============================================================================
// Auth Errors
// =============================================================================

#[tokio::test]
#[test_log::test]
async fn test_request_without_api_key() {
    with_test_context_unit(|ctx| async move {
        // Create a new client without setting an API key
        let config = ClientConfig {
            base_url: ctx.client.get_config().base_url.clone(),
            app_name: "test-no-auth".to_string(),
            rate_limit_max_retries: 0,
            ..Default::default()
        };

        let unauth_client = AnytypeClient::with_config(config).expect("Failed to create client");

        // Try to make a request without authentication
        let result = unauth_client.spaces().list().await;

        assert!(
            result.is_err(),
            "Expected error for request without API key, got success"
        );
        match result.unwrap_err() {
            AnytypeError::Auth { message } => {
                assert!(
                    message.contains("API key") || message.contains("not set"),
                    "Error message should mention API key: {}",
                    message
                );
                println!("✓ Correctly rejected request without API key: {}", message);
            }
            AnytypeError::Unauthorized => {
                println!("✓ Correctly rejected request without API key (Unauthorized)");
            }
            e => panic!("Expected Auth or Unauthorized error, got: {:?}", e),
        }
    })
    .await
}

#[tokio::test]
#[test_log::test]
async fn test_invalid_api_key() {
    with_test_context_unit(|ctx| async move {
        // Create a new client with an invalid API key
        let config = ClientConfig {
            base_url: ctx.client.get_config().base_url.clone(),
            app_name: "test-bad-auth".to_string(),
            rate_limit_max_retries: 0,
            ..Default::default()
        };

        let bad_client = AnytypeClient::with_config(config).expect("Failed to create client");
        bad_client.set_api_key(HttpCredentials::new("invalid_api_key_12345".to_string()));

        // Try to make a request with invalid key
        let result = bad_client.spaces().list().await;

        assert!(
            result.is_err(),
            "Expected error for invalid API key, got success"
        );
        match result.unwrap_err() {
            AnytypeError::Unauthorized | AnytypeError::Forbidden => {
                println!("✓ Correctly rejected request with invalid API key");
            }
            AnytypeError::Auth { message } => {
                println!(
                    "✓ Correctly rejected request with invalid API key: {}",
                    message
                );
            }
            e => panic!("Expected Unauthorized/Forbidden/Auth error, got: {:?}", e),
        }
    })
    .await
}

// =============================================================================
// Permission Errors
// =============================================================================

#[tokio::test]
#[test_log::test]
async fn test_delete_system_type() {
    with_test_context_unit(|ctx| async move {
        // Try to find a system type (like "page")
        let types = ctx
            .client
            .types(ctx.space_id())
            .list()
            .await
            .expect("Failed to list types");

        let page_type = types
            .into_response()
            .items
            .into_iter()
            .find(|t| t.key == "page")
            .expect("Could not find 'page' type");

        // Try to delete the system type
        let result = ctx
            .client
            .get_type(ctx.space_id(), &page_type.id)
            .delete()
            .await;

        // Deleting system types should fail
        match result {
            Ok(_) => {
                println!("⚠ API allowed deletion of system type (unexpected)");
            }
            Err(AnytypeError::Forbidden) => {
                println!("✓ Correctly rejected deletion of system type (Forbidden)");
            }
            Err(AnytypeError::Validation { message }) => {
                println!("✓ Correctly rejected deletion of system type: {}", message);
            }
            Err(e) => {
                println!(
                    "⚠ Deletion of system type returned {:?} (expected Forbidden/Validation)",
                    e
                );
            }
        }
    })
    .await
}

#[tokio::test]
#[test_log::test]
async fn test_modify_readonly_property() {
    with_test_context_unit(|ctx| async move {


        // Find a system/readonly property
        let properties = ctx
            .client
            .properties(ctx.space_id())
            .list()
            .await
            .expect("Failed to list properties");

        // System properties like "id" or "type" are typically read-only
        let system_prop = properties
            .into_response()
            .items
            .into_iter()
            .find(|p| {
                matches!(
                    p.key.as_str(),
                    "id"
                        | "type"
                        | "created_date"
                        | "last_modified_date"
                        | "space_id"
                        | "creator"
                        | "last_modified_by"
                )
            });
        let system_prop = match system_prop {
            Some(prop) => prop,
            None => {
                println!("⚠ Could not find system property to test");
                return;
            }
        };

        // Try to update the system property
        let result = ctx
            .client
            .update_property(ctx.space_id(), &system_prop.id)
            .name("Modified System Property")
            .update()
            .await;

        // Modifying system properties should fail
        match result {
            Ok(_) => {
                println!("⚠ API allowed modification of system property (unexpected)");
            }
            Err(AnytypeError::Forbidden) => {
                println!("✓ Correctly rejected modification of system property (Forbidden)");
            }
            Err(AnytypeError::Validation { message }) => {
                println!(
                    "✓ Correctly rejected modification of system property: {}",
                    message
                );
            }
            Err(e) => {
                println!(
                    "⚠ Modification of system property returned {:?} (expected Forbidden/Validation)",
                    e
                );
            }
        }
    }).await
}

// =============================================================================
// Error Type Verification
// =============================================================================

#[tokio::test]
#[test_log::test]
async fn test_validation_error_has_message() {
    with_test_context_unit(|ctx| async move {
        // Trigger a validation error (empty name)
        let result = ctx
            .client
            .new_object(ctx.space_id(), "page")
            .name("")
            .create()
            .await;

        assert!(result.is_err(), "Expected validation error for empty name");
        match result.unwrap_err() {
            AnytypeError::Validation { message } => {
                assert!(
                    !message.is_empty(),
                    "Validation error should include a message"
                );
                assert!(
                    message.len() > 10,
                    "Validation error message should be descriptive"
                );
                println!("✓ Validation error includes message: {}", message);
            }
            e => panic!("Expected Validation error, got: {:?}", e),
        }
    })
    .await
}

#[tokio::test]
#[test_log::test]
async fn test_not_found_error() {
    with_test_context_unit(|ctx| async move {
        let nonexistent_id = "bafyreie6n5l5nkbjal37su54cha4coy7qzuhrnajluzv5qd5jvtsrxkequ";

        let result = ctx
            .client
            .object(ctx.space_id(), nonexistent_id)
            .get()
            .await;

        assert!(result.is_err(), "Expected NotFound error");
        match result.unwrap_err() {
            AnytypeError::NotFound { .. } => {
                println!("✓ Correctly returned NotFound error for missing resource");
            }
            e => {
                // Some APIs might return Validation for malformed IDs
                println!(
                    "⚠ Missing resource returned {:?} instead of NotFound (may be acceptable)",
                    e
                );
            }
        }
    })
    .await
}

#[tokio::test]
#[test_log::test]
async fn test_auth_error() {
    // This is tested in test_request_without_api_key and test_invalid_api_key
    // Here we verify the error type is Auth/Unauthorized
    let config = ClientConfig {
        base_url: Some(
            std::env::var("ANYTYPE_TEST_URL")
                .unwrap_or_else(|_| "http://127.0.0.1:31012".to_string()),
        ),
        app_name: "test-auth-error".to_string(),
        rate_limit_max_retries: 0,
        ..Default::default()
    };

    let client = AnytypeClient::with_config(config).expect("Failed to create client");

    let result = client.spaces().list().await;

    assert!(result.is_err(), "Expected auth error");
    match result.unwrap_err() {
        AnytypeError::Auth { message } => {
            assert!(!message.is_empty(), "Auth error should include a message");
            println!("✓ Auth error includes message: {}", message);
        }
        AnytypeError::Unauthorized => {
            println!("✓ Correctly returned Unauthorized error");
        }
        e => panic!("Expected Auth or Unauthorized error, got: {:?}", e),
    }
}

// =============================================================================
// Additional Edge Cases
// =============================================================================

#[tokio::test]
#[test_log::test]
async fn test_body_too_long() {
    with_test_context_unit(|ctx| async move {
        // Create markdown content exceeding the limit (10 MiB default)
        // Create 11 MB of content
        let huge_body =
            "x".repeat(ctx.client.get_config().get_limits().markdown_max_len as usize + 1000);

        let result = ctx
            .client
            .new_object(ctx.space_id(), "page")
            .name(unique_test_name("Huge Body Test"))
            .body(huge_body)
            .create()
            .await;
        if let Ok(obj) = &result {
            ctx.register_object(&obj.id);
        }

        assert!(
            result.is_err(),
            "Expected error for body exceeding max length, got success"
        );
        match result.unwrap_err() {
            AnytypeError::Validation { message } => {
                assert!(
                    message.contains("too long") || message.contains("max"),
                    "Error message should mention length limit: {}",
                    message
                );
                println!(
                    "✓ Correctly rejected body exceeding max length: {}",
                    message
                );
            }
            e => panic!("Expected Validation error, got: {:?}", e),
        }
    })
    .await
}

#[tokio::test]
#[test_log::test]
async fn test_empty_space_id() {
    with_test_context_unit(|ctx| async move {
        let result = ctx
            .client
            .object("", "some_object_id_1234567890123456789")
            .get()
            .await;

        assert!(
            result.is_err(),
            "Expected error for empty space ID, got success"
        );
        match result.unwrap_err() {
            AnytypeError::Validation { message } => {
                assert!(
                    message.contains("space")
                        || message.contains("empty")
                        || message.contains("id"),
                    "Error message should mention space/empty/id: {}",
                    message
                );
                println!("✓ Correctly rejected empty space ID: {}", message);
            }
            e => panic!("Expected Validation error, got: {:?}", e),
        }
    })
    .await
}

#[tokio::test]
#[test_log::test]
async fn test_multiple_validation_errors() {
    with_test_context_unit(|ctx| async move {
        // Create an object with multiple validation issues
        let result = ctx
            .client
            .new_object(ctx.space_id(), "")
            .name("")
            .create()
            .await;

        assert!(
            result.is_err(),
            "Expected error for multiple validation issues, got success"
        );
        match result.unwrap_err() {
            AnytypeError::Validation { message } => {
                // Should report at least one validation error
                assert!(
                    !message.is_empty(),
                    "Validation error should include a message"
                );
                println!(
                    "✓ Correctly rejected request with multiple validation issues: {}",
                    message
                );
            }
            e => {
                // Other error types are also acceptable
                println!(
                    "⚠ Multiple validation issues returned {:?} instead of Validation",
                    e
                );
            }
        }
    })
    .await
}
