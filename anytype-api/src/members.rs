//! # Anytype Members
//!
//! This module provides a fluent builder API for working with members of a space.
//!
//! ## Member methods on AnytypeClient
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
    filters::Query,
    http_client::{GetPaged, HttpClient},
    prelude::*,
};

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
    /// Global name in the network (e.g., "john.any")
    pub global_name: Option<String>,

    /// Member's icon
    pub icon: Option<serde_json::Value>,

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

impl Member {
    /// Returns true if the member is active.
    pub fn is_active(&self) -> bool {
        self.status == MemberStatus::Active
    }

    /// Returns true if the member is an owner.
    pub fn is_owner(&self) -> bool {
        self.role == MemberRole::Owner
    }

    /// Returns true if the member can edit.
    pub fn can_edit(&self) -> bool {
        matches!(self.role, MemberRole::Editor | MemberRole::Owner)
    }

    /// Returns the display name, falling back to global_name or "Unknown".
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
        self.limits.validate_id(&self.member_id, "member_id")?;

        let response: MemberResponse = self
            .client
            .get_request(
                &format!("/v1/spaces/{}/members/{}", self.space_id, self.member_id),
                Default::default(),
            )
            .await?;
        Ok(response.member)
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
    limit: Option<usize>,
    offset: Option<usize>,
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
    pub fn limit(mut self, limit: usize) -> Self {
        self.limit = Some(limit);
        self
    }

    /// Sets the pagination offset.
    pub fn offset(mut self, offset: usize) -> Self {
        self.offset = Some(offset);
        self
    }

    /// Adds a filter condition.
    pub fn filter(mut self, filter: Filter) -> Self {
        self.filters.push(filter);
        self
    }

    /// Executes the list request.
    pub async fn list(self) -> Result<PagedResult<Member>> {
        self.limits.validate_id(&self.space_id, "space_id")?;

        let query = Query::default()
            .set_limit_opt(&self.limit)
            .set_offset_opt(&self.offset)
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
}
