//! # Anytype Members
//!
//! This module provides a fluent builder API for working with members of a space.
//!
//! ## Member methods on `AnytypeClient`
//!
//! - [members](AnytypeClient::members) - list members in space
//! - [member](AnytypeClient::member) - get member
//!
//! ## Quick Start
//!
//! ```rust,no_run
//! use anytype::prelude::*;
//!
//! # async fn example(client: &AnytypeClient) -> Result<(), AnytypeError> {
//! let space_id = "your_space_id";
//!
//! // List all members
//! let members = client.members(space_id).list().await?;
//!
//! // Get a specific member
//! let member = client.member(space_id, "member_id").get().await?;
//! # Ok(())
//! # }
//! ```

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::{
    Result,
    client::AnytypeClient,
    filters::{Query, QueryWithFilters},
    http_client::{GetPaged, HttpClient},
    prelude::*,
};

/// Maximum byte length accepted for a member profile or network identity.
pub const MAX_MEMBER_REFERENCE_BYTES: usize = 256;

/// Member role within a space.
#[derive(
    Debug, Deserialize, Serialize, Clone, PartialEq, Eq, strum::Display, strum::EnumString,
)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum MemberRole {
    /// Can view but not edit
    Viewer,
    /// Can view and edit
    Editor,
    /// Can manage members and content, but is not the space owner
    Admin,
    /// Full control including admin
    Owner,
    /// No access
    NoPermission,
}

/// Member status within a space.
#[derive(
    Debug, Deserialize, Serialize, Clone, PartialEq, Eq, strum::Display, strum::EnumString,
)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum MemberStatus {
    /// Joining the space
    Joining,
    /// Active member
    Active,
    /// Removed from space
    Removed,
    /// Declined invitation
    Declined,
    /// Being removed
    Removing,
    /// Invitation canceled
    Canceled,
}

/// Represents a member of an Anytype space.
#[derive(Debug, Deserialize, Serialize)]
pub struct Member {
    /// Data model type returned by the REST API.
    #[serde(default = "member_data_model")]
    pub object: DataModel,

    /// Global name in the network (e.g., "john.any")
    pub global_name: Option<String>,

    /// Member's icon
    pub icon: Option<Icon>,

    /// Profile object ID of the member
    pub id: String,

    /// Network identity of the member
    pub identity: Option<String>,

    /// Display name of the member
    pub name: Option<String>,

    /// Member's role (Viewer, Editor, Owner)
    pub role: MemberRole,

    /// Member's status (Active, Joining, etc.)
    pub status: MemberStatus,
}

fn member_data_model() -> DataModel {
    DataModel::Member
}

impl Member {
    /// Returns true if the member is active.
    pub fn is_active(&self) -> bool {
        self.status == MemberStatus::Active
    }

    /// Returns true if the member is an owner.
    pub fn is_owner(&self) -> bool {
        self.role == MemberRole::Owner
    }

    /// Returns true if the member is a space administrator (or owner).
    pub fn is_admin(&self) -> bool {
        matches!(self.role, MemberRole::Admin | MemberRole::Owner)
    }

    /// Returns true if the member can edit.
    pub fn can_edit(&self) -> bool {
        matches!(
            self.role,
            MemberRole::Editor | MemberRole::Admin | MemberRole::Owner
        )
    }

    /// Returns the display name, falling back to `global_name` or "Unknown".
    pub fn display_name(&self) -> &str {
        self.name
            .as_deref()
            .or(self.global_name.as_deref())
            .unwrap_or("Unknown")
    }
}

// ============================================================================
// RESPONSE TYPES (internal)
// ============================================================================

#[derive(Debug, Deserialize)]
struct MemberResponse {
    member: Member,
}

// ============================================================================
// BUILDER STRUCTS (public)
// ============================================================================

/// Request builder for getting a single member.
///
/// Obtained via [`AnytypeClient::member`].
#[derive(Debug)]
pub struct MemberRequest {
    client: Arc<HttpClient>,
    limits: ValidationLimits,
    space_id: String,
    member_id: String,
}

impl MemberRequest {
    pub(crate) fn new(
        client: Arc<HttpClient>,
        limits: ValidationLimits,
        space_id: impl Into<String>,
        member_id: impl Into<String>,
    ) -> Self {
        Self {
            client,
            limits,
            space_id: space_id.into(),
            member_id: member_id.into(),
        }
    }

    /// Retrieves the member by ID.
    pub async fn get(self) -> Result<Member> {
        self.limits.validate_id(&self.space_id, "space_id")?;
        validate_member_reference(&self.member_id)?;

        let response: MemberResponse = self
            .client
            .get_request(
                &format!("/v1/spaces/{}/members/{}", self.space_id, self.member_id),
                QueryWithFilters::default(),
            )
            .await?;
        Ok(response.member)
    }
}

/// Member endpoints accept participant IDs and network identities in addition
/// to object-shaped IDs. Keep the path segment bounded and URL-unreserved
/// without imposing the object CID grammar used by other endpoint builders.
fn validate_member_reference(member_id: &str) -> Result<()> {
    let valid = !member_id.is_empty()
        && member_id.len() <= MAX_MEMBER_REFERENCE_BYTES
        && !matches!(member_id, "." | "..")
        && member_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~'));
    if valid {
        Ok(())
    } else {
        Err(AnytypeError::Validation {
            message: "member_id is not a valid bounded member reference".to_owned(),
        })
    }
}

/// Request builder for listing members in a space.
///
/// Obtained via [`AnytypeClient::members`].
#[derive(Debug)]
pub struct ListMembersRequest {
    client: Arc<HttpClient>,
    limits: ValidationLimits,
    space_id: String,
    limit: Option<u32>,
    offset: Option<u32>,
    filters: Vec<Filter>,
}

impl ListMembersRequest {
    pub(crate) fn new(
        client: Arc<HttpClient>,
        limits: ValidationLimits,
        space_id: impl Into<String>,
    ) -> Self {
        Self {
            client,
            limits,
            space_id: space_id.into(),
            limit: None,
            offset: None,
            filters: Vec::new(),
        }
    }

    /// Sets the pagination limit.
    #[must_use]
    pub fn limit(mut self, limit: u32) -> Self {
        self.limit = Some(limit);
        self
    }

    /// Sets the pagination offset.
    #[must_use]
    pub fn offset(mut self, offset: u32) -> Self {
        self.offset = Some(offset);
        self
    }

    /// Adds a filter condition.
    #[must_use]
    pub fn filter(mut self, filter: Filter) -> Self {
        self.filters.push(filter);
        self
    }

    /// Executes the list request.
    pub async fn list(self) -> Result<PagedResult<Member>> {
        self.limits.validate_id(&self.space_id, "space_id")?;

        let query = Query::default()
            .set_limit_opt(self.limit)
            .set_offset_opt(self.offset)
            .add_filters(&self.filters);

        self.client
            .get_request_paged(&format!("/v1/spaces/{}/members", self.space_id), query)
            .await
    }
}

// ============================================================================
// ANYTYPECLIENT METHODS
// ============================================================================

impl AnytypeClient {
    /// Creates a request builder for getting a single member.
    pub fn member(
        &self,
        space_id: impl Into<String>,
        member_id: impl Into<String>,
    ) -> MemberRequest {
        MemberRequest::new(
            self.client.clone(),
            self.config.limits.clone(),
            space_id,
            member_id,
        )
    }

    /// Creates a request builder for listing members in a space.
    pub fn members(&self, space_id: impl Into<String>) -> ListMembersRequest {
        ListMembersRequest::new(self.client.clone(), self.config.limits.clone(), space_id)
    }
}

// ============================================================================
// TESTS
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn make_member(role: MemberRole, status: MemberStatus) -> Member {
        Member {
            object: DataModel::Member,
            global_name: None,
            icon: None,
            id: "test".to_string(),
            identity: None,
            name: None,
            role,
            status,
        }
    }

    #[test]
    fn test_member_is_active() {
        assert!(make_member(MemberRole::Editor, MemberStatus::Active).is_active());
        assert!(!make_member(MemberRole::Editor, MemberStatus::Joining).is_active());
    }

    #[test]
    fn test_member_is_owner() {
        assert!(make_member(MemberRole::Owner, MemberStatus::Active).is_owner());
        assert!(!make_member(MemberRole::Editor, MemberStatus::Active).is_owner());
    }

    #[test]
    fn test_member_can_edit() {
        assert!(make_member(MemberRole::Owner, MemberStatus::Active).can_edit());
        assert!(make_member(MemberRole::Editor, MemberStatus::Active).can_edit());
        assert!(!make_member(MemberRole::Viewer, MemberStatus::Active).can_edit());
    }

    #[test]
    fn test_member_display_name() {
        let mut member = make_member(MemberRole::Editor, MemberStatus::Active);
        assert_eq!(member.display_name(), "Unknown");

        member.global_name = Some("john.any".to_string());
        assert_eq!(member.display_name(), "john.any");

        member.name = Some("John Doe".to_string());
        assert_eq!(member.display_name(), "John Doe");
    }

    #[test]
    fn member_reference_accepts_documented_forms_and_rejects_path_injection() {
        for value in [
            "_participant_bafyreid5fvqlnsobih2keakcxjrrlpmly6kf37klzjzen4ibfdgalcdp4a",
            "12D3KooWExampleNetworkIdentity",
            "bafyreid5fvqlnsobih2keakcxjrrlpmly6kf37klzjzen4ibfdgalcdp4a",
        ] {
            assert!(validate_member_reference(value).is_ok(), "{value}");
        }
        for value in ["", ".", "..", "member/id", "member?id", "member id"] {
            assert!(validate_member_reference(value).is_err(), "{value}");
        }
        assert!(validate_member_reference(&"x".repeat(MAX_MEMBER_REFERENCE_BYTES)).is_ok());
        assert!(validate_member_reference(&"x".repeat(MAX_MEMBER_REFERENCE_BYTES + 1)).is_err());
    }

    #[test]
    fn member_schema_preserves_discriminator_and_typed_icon() {
        let response: MemberResponse = serde_json::from_value(serde_json::json!({
            "member": {
                "object": "member",
                "global_name": "john.any",
                "icon": {
                    "format": "icon",
                    "name": "person",
                    "color": "blue"
                },
                "id": "member-id",
                "identity": "identity-id",
                "name": "John",
                "role": "owner",
                "status": "active"
            }
        }))
        .expect("member response schema");

        assert_eq!(response.member.object, DataModel::Member);
        assert_eq!(
            response.member.icon,
            Some(Icon::Icon {
                name: "person".to_owned(),
                color: Color::Blue,
            })
        );
        let serialized = serde_json::to_value(response.member).expect("serialize member");
        assert_eq!(serialized["object"], "member");
        assert_eq!(serialized["icon"]["format"], "icon");
    }

    #[test]
    fn member_discriminator_defaults_when_omitted_and_preserves_present_value() {
        let member_without_discriminator: Member = serde_json::from_value(serde_json::json!({
            "global_name": null,
            "icon": null,
            "id": "member-id",
            "identity": null,
            "name": null,
            "role": "viewer",
            "status": "active"
        }))
        .expect("member without discriminator");
        assert_eq!(member_without_discriminator.object, DataModel::Member);

        let member_with_observed_type: Member = serde_json::from_value(serde_json::json!({
            "object": "type",
            "global_name": null,
            "icon": null,
            "id": "member-id",
            "identity": null,
            "name": null,
            "role": "viewer",
            "status": "active"
        }))
        .expect("member with observed discriminator");
        assert_eq!(member_with_observed_type.object, DataModel::Type);
    }
}
