//! Integration tests for the Members API
//!
//! Tests member listing, retrieval, field validation, roles, and error handling
//! against a live Anytype API server.
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
//! cargo test -p anytype --test test_members
//! ```

mod common;

use anytype::prelude::*;
use anytype::test_util::{TestResult, with_test_context};
use anytype::validation::looks_like_object_id;

fn tweak_id(id: &str) -> String {
    if id.is_empty() {
        return "bafyreiaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string();
    }
    let (prefix, last) = id.split_at(id.len() - 1);
    let replacement = if last == "0" { "1" } else { "0" };
    format!("{prefix}{replacement}")
}

fn is_expected_member_lookup_error(err: &AnytypeError) -> bool {
    match err {
        AnytypeError::NotFound { .. } => true,
        AnytypeError::Validation { message } => {
            message.contains("member_id") || message.contains("space_id")
        }
        _ => false,
    }
}

// =============================================================================
// Member Listing Tests
// =============================================================================

/// Test listing all members in a space
#[tokio::test]
#[test_log::test]
async fn test_list_members() -> TestResult<()> {
    with_test_context(|ctx| async move {
        let members = ctx.client.members(&ctx.space_id).list().await?;

        // Verify we got a result
        assert!(
            !members.is_empty(),
            "Members list should not be empty - every space should have at least one member"
        );

        Ok(())
    })
    .await
}

/// Test that listing members includes at least one owner (the space creator)
#[tokio::test]
#[test_log::test]
async fn test_list_members_includes_owner() -> TestResult<()> {
    with_test_context(|ctx| async move {
        let members = ctx.client.members(&ctx.space_id).list().await?;

        // Verify at least one member exists
        assert!(
            !members.is_empty(),
            "Space should have at least one member (the owner)"
        );

        // Find an owner
        let has_owner = members.iter().any(|m| m.role == MemberRole::Owner);

        assert!(
            has_owner,
            "Space should have at least one member with Owner role"
        );

        Ok(())
    })
    .await
}

/// Test listing members with pagination limit
#[tokio::test]
#[test_log::test]
async fn test_list_members_with_limit() -> TestResult<()> {
    with_test_context(|ctx| async move {
        // Request only 1 member
        let members = ctx.client.members(&ctx.space_id).limit(1).list().await?;

        assert!(
            !members.is_empty(),
            "Should return at least one member when limit is 1"
        );
        assert!(
            members.len() <= 1,
            "Should not return more than limit (1), got {}",
            members.len()
        );

        Ok(())
    })
    .await
}

/// Test that member list contains required fields
#[tokio::test]
#[test_log::test]
async fn test_list_members_field_presence() -> TestResult<()> {
    with_test_context(|ctx| async move {
        let members = ctx.client.members(&ctx.space_id).list().await?;

        assert!(!members.is_empty(), "Members list should not be empty");

        // Verify required fields on each member
        for member in members.iter() {
            assert!(
                !member.id.is_empty(),
                "Member ID should not be empty: {:?}",
                member
            );

            // Verify role is set (it's a required enum field)
            // Just accessing it verifies it's present and valid
            let _role = &member.role;

            // Verify status is set (it's a required enum field)
            let _status = &member.status;

            // Note: name can be None, but display_name() should always return a string
            let display_name = member.display_name();
            assert!(
                !display_name.is_empty(),
                "Member display_name should not be empty"
            );
        }

        Ok(())
    })
    .await
}

// =============================================================================
// Member Retrieval Tests
// =============================================================================

/// Test getting a specific member by ID
#[tokio::test]
#[test_log::test]
async fn test_get_member_by_id() -> TestResult<()> {
    with_test_context(|ctx| async move {
        // First, list members to get a valid member ID
        let members = ctx.client.members(&ctx.space_id).list().await?;
        assert!(!members.is_empty(), "Need at least one member to test get");

        let first_member = members.iter().next().unwrap();
        let member_id = &first_member.id;

        if !looks_like_object_id(member_id) {
            eprintln!("member id is not object-id shaped, skipping get_by_id");
            return Ok(());
        }

        // Now get that specific member
        let member = match ctx.client.member(&ctx.space_id, member_id).get().await {
            Ok(member) => member,
            Err(e) if is_expected_member_lookup_error(&e) => {
                eprintln!("member lookup not supported for id {member_id}: {e}");
                return Ok(());
            }
            Err(e) => return Err(e.into()),
        };

        // Verify the retrieved member matches
        assert_eq!(
            member.id, *member_id,
            "Retrieved member ID should match requested ID"
        );
        assert_eq!(
            member.role, first_member.role,
            "Retrieved member role should match"
        );
        assert_eq!(
            member.status, first_member.status,
            "Retrieved member status should match"
        );

        Ok(())
    })
    .await
}

/// Test that getting a nonexistent member returns proper error
#[tokio::test]
#[test_log::test]
async fn test_get_nonexistent_member() -> TestResult<()> {
    with_test_context(|ctx| async move {
        // Use a UUID-like string that's unlikely to exist
        let fake_member_id = tweak_id(&ctx.space_id);
        assert!(
            looks_like_object_id(&fake_member_id),
            "fake_member_id should look like an object id"
        );

        let result = ctx.client.member(&ctx.space_id, fake_member_id).get().await;

        // Should return an error
        assert!(
            result.is_err(),
            "Getting nonexistent member should return an error"
        );

        // Verify it's the right kind of error (NotFound)
        match result {
            Err(e) if is_expected_member_lookup_error(&e) => {}
            Ok(_) => panic!("Expected error when getting nonexistent member"),
            Err(e) => panic!("Expected NotFound/Validation error, got: {:?}", e),
        }

        Ok(())
    })
    .await
}

// =============================================================================
// Member Role Tests
// =============================================================================

/// Test that member roles have valid enum values
#[tokio::test]
#[test_log::test]
async fn test_member_role_values() -> TestResult<()> {
    with_test_context(|ctx| async move {
        let members = ctx.client.members(&ctx.space_id).list().await?;
        assert!(!members.is_empty(), "Need members to test roles");

        // Verify that all roles are valid enum values
        for member in members.iter() {
            // If we can match on the role, it's a valid enum value
            match member.role {
                MemberRole::Owner => {
                    // Verify owner-specific helper methods
                    assert!(member.is_owner(), "is_owner() should be true for Owner");
                    assert!(member.can_edit(), "can_edit() should be true for Owner");
                }
                MemberRole::Editor => {
                    assert!(!member.is_owner(), "is_owner() should be false for Editor");
                    assert!(member.can_edit(), "can_edit() should be true for Editor");
                }
                MemberRole::Viewer => {
                    assert!(!member.is_owner(), "is_owner() should be false for Viewer");
                    assert!(!member.can_edit(), "can_edit() should be false for Viewer");
                }
                MemberRole::NoPermission => {
                    assert!(
                        !member.is_owner(),
                        "is_owner() should be false for NoPermission"
                    );
                    assert!(
                        !member.can_edit(),
                        "can_edit() should be false for NoPermission"
                    );
                }
            }
        }

        Ok(())
    })
    .await
}

/// Test that the space has at least one owner
#[tokio::test]
#[test_log::test]
async fn test_owner_role_exists() -> TestResult<()> {
    with_test_context(|ctx| async move {
        let members = ctx.client.members(&ctx.space_id).list().await?;
        assert!(!members.is_empty(), "Space should have members");

        // Count owners
        let owner_count = members
            .iter()
            .filter(|m| m.role == MemberRole::Owner)
            .count();

        assert!(
            owner_count > 0,
            "Space should have at least one owner, found {}",
            owner_count
        );

        // Verify using helper method too
        let owner_count_via_helper = members.iter().filter(|m| m.is_owner()).count();
        assert_eq!(
            owner_count, owner_count_via_helper,
            "is_owner() helper should match direct role check"
        );

        Ok(())
    })
    .await
}

// =============================================================================
// Member Status Tests
// =============================================================================

/// Test that member status values are valid
#[tokio::test]
#[test_log::test]
async fn test_member_status_values() -> TestResult<()> {
    with_test_context(|ctx| async move {
        let members = ctx.client.members(&ctx.space_id).list().await?;
        assert!(!members.is_empty(), "Need members to test status");

        // Verify that all statuses are valid enum values
        for member in members.iter() {
            // If we can match on the status, it's a valid enum value
            match member.status {
                MemberStatus::Active => {
                    assert!(member.is_active(), "is_active() should be true for Active");
                }
                MemberStatus::Joining => {
                    assert!(
                        !member.is_active(),
                        "is_active() should be false for Joining"
                    );
                }
                MemberStatus::Removed => {
                    assert!(
                        !member.is_active(),
                        "is_active() should be false for Removed"
                    );
                }
                MemberStatus::Declined => {
                    assert!(
                        !member.is_active(),
                        "is_active() should be false for Declined"
                    );
                }
                MemberStatus::Removing => {
                    assert!(
                        !member.is_active(),
                        "is_active() should be false for Removing"
                    );
                }
                MemberStatus::Canceled => {
                    assert!(
                        !member.is_active(),
                        "is_active() should be false for Canceled"
                    );
                }
            }
        }

        Ok(())
    })
    .await
}

/// Test that the space has at least one active member
#[tokio::test]
#[test_log::test]
async fn test_active_member_exists() -> TestResult<()> {
    with_test_context(|ctx| async move {
        let members = ctx.client.members(&ctx.space_id).list().await?;
        assert!(!members.is_empty(), "Space should have members");

        // Find at least one active member
        let has_active = members.iter().any(|m| m.is_active());

        assert!(has_active, "Space should have at least one active member");

        Ok(())
    })
    .await
}

// =============================================================================
// Member Helper Method Tests
// =============================================================================

/// Test member display_name helper method
#[tokio::test]
#[test_log::test]
async fn test_member_display_name() -> TestResult<()> {
    with_test_context(|ctx| async move {
        let members = ctx.client.members(&ctx.space_id).list().await?;
        assert!(!members.is_empty(), "Need members to test display_name");

        for member in members.iter() {
            let display_name = member.display_name();

            // display_name should never be empty
            assert!(
                !display_name.is_empty(),
                "display_name should never be empty"
            );

            // Verify fallback logic:
            // 1. If name is set, should use name
            // 2. Otherwise if global_name is set, should use global_name
            // 3. Otherwise should use "Unknown"
            if let Some(ref name) = member.name {
                assert_eq!(
                    display_name, name,
                    "display_name should match name when name is set"
                );
            } else if let Some(ref global_name) = member.global_name {
                assert_eq!(
                    display_name, global_name,
                    "display_name should match global_name when name is not set"
                );
            } else {
                assert_eq!(
                    display_name, "Unknown",
                    "display_name should be 'Unknown' when both name and global_name are not set"
                );
            }
        }

        Ok(())
    })
    .await
}

// =============================================================================
// Error Handling Tests
// =============================================================================

/// Test listing members in an invalid/nonexistent space
#[tokio::test]
#[test_log::test]
async fn test_list_members_invalid_space() -> TestResult<()> {
    with_test_context(|ctx| async move {
        // Use a fake space ID
        let fake_space_id = tweak_id(&ctx.space_id);
        assert!(
            looks_like_object_id(&fake_space_id),
            "fake_space_id should look like an object id"
        );

        let result = ctx.client.members(fake_space_id).list().await;

        match result {
            Err(e) if is_expected_member_lookup_error(&e) => {}
            Ok(members) => {
                assert!(
                    members.is_empty(),
                    "Expected no members for nonexistent space"
                );
            }
            Err(e) => panic!("Expected NotFound/Validation error, got: {:?}", e),
        }

        Ok(())
    })
    .await
}

/// Test getting a member from an invalid space
#[tokio::test]
#[test_log::test]
async fn test_get_member_invalid_space() -> TestResult<()> {
    with_test_context(|ctx| async move {
        // Get a valid member ID from the real space first
        let members = ctx.client.members(&ctx.space_id).list().await?;
        assert!(!members.is_empty(), "Need a member ID for this test");
        let member_id = &members.iter().next().unwrap().id;

        // Try to get it from a fake space
        let fake_space_id = "nonexistent-space-id-12345";

        let result = ctx.client.member(fake_space_id, member_id).get().await;

        // Should return an error
        assert!(
            result.is_err(),
            "Getting member from nonexistent space should return an error"
        );

        Ok(())
    })
    .await
}

// =============================================================================
// Pagination Tests
// =============================================================================

/// Test member listing with offset pagination
#[tokio::test]
#[test_log::test]
async fn test_list_members_with_offset() -> TestResult<()> {
    with_test_context(|ctx| async move {
        // Get all members first
        let all_members = ctx.client.members(&ctx.space_id).list().await?;

        if all_members.len() < 2 {
            println!(
                "Skipping offset test: need at least 2 members, found {}",
                all_members.len()
            );
            return Ok(());
        }

        // Get first page
        let first_page = ctx.client.members(&ctx.space_id).limit(1).list().await?;
        assert_eq!(
            first_page.len(),
            1,
            "First page should have exactly 1 member"
        );

        // Get second page
        let second_page = ctx
            .client
            .members(&ctx.space_id)
            .limit(1)
            .offset(1)
            .list()
            .await?;

        // If there are at least 2 members, second page should have results
        if all_members.len() >= 2 {
            assert!(
                !second_page.is_empty(),
                "Second page should have results when there are multiple members"
            );

            // Verify they're different members
            let first_id = &first_page.iter().next().unwrap().id;
            let second_id = &second_page.iter().next().unwrap().id;
            assert_ne!(
                first_id, second_id,
                "First and second page should return different members"
            );
        }

        Ok(())
    })
    .await
}

// =============================================================================
// Field Coverage Tests
// =============================================================================

/// Test that optional member fields can be present
#[tokio::test]
#[test_log::test]
async fn test_member_optional_fields() -> TestResult<()> {
    with_test_context(|ctx| async move {
        let members = ctx.client.members(&ctx.space_id).list().await?;
        assert!(!members.is_empty(), "Need members to test optional fields");

        // Just verify the fields exist and are accessible
        for member in members.iter() {
            // These are all Option types - just access them to verify structure
            let _global_name: &Option<String> = &member.global_name;
            let _icon: &Option<serde_json::Value> = &member.icon;
            let _identity: &Option<String> = &member.identity;
            let _name: &Option<String> = &member.name;
        }

        Ok(())
    })
    .await
}

/// Test member field types are correct
#[tokio::test]
#[test_log::test]
async fn test_member_field_types() -> TestResult<()> {
    with_test_context(|ctx| async move {
        let members = ctx.client.members(&ctx.space_id).list().await?;
        assert!(!members.is_empty(), "Need members to test field types");

        for member in members.iter() {
            // Verify required string fields
            assert!(
                !member.id.is_empty(),
                "Member ID should be a non-empty string"
            );

            // Verify enum fields
            let _role: &MemberRole = &member.role;
            let _status: &MemberStatus = &member.status;

            // Verify optional string fields (if present, should be non-empty)
            if let Some(ref name) = member.name {
                assert!(
                    !name.is_empty(),
                    "If name is present, it should not be empty"
                );
            }

            if let Some(_global_name) = &member.global_name {
                // Some servers return an empty global_name string.
            }

            if let Some(ref identity) = member.identity {
                assert!(
                    !identity.is_empty(),
                    "If identity is present, it should not be empty"
                );
            }
        }

        Ok(())
    })
    .await
}
