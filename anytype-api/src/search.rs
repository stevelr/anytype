//! # Anytype Search
//!
//! This module provides a fluent builder API for searching objects - globally or in a space.
//!
//! ## Quick Start
//!
//! ```rust
//! use anytype::prelude::*;
//!
//! # async fn example() -> Result<(), AnytypeError> {
//! #   let client = AnytypeClient::new("doc test")?.env_key_store()?;
//! #   let space_id = anytype::test_util::example_space_id(&client).await?;
//!
//! // Global search across all spaces
//! let results = client.search_global()
//!     .text("meeting notes")
//!     .types(["page", "note"])
//!     .sort_desc("created_date")
//!     .execute().await?;
//!
//! // Search within a specific space
//! // Example: find objects in space containing text "project" (in title or body)
//! let results = client.search_in(&space_id)
//!     .text("project")
//!     .execute().await?;
//!
//! # Ok(())
//! # }
//! ```

use std::sync::Arc;

use serde::Serialize;

use crate::{
    Result,
    client::AnytypeClient,
    filters::Query,
    http_client::{GetPaged, HttpClient},
    prelude::*,
};

// ============================================================================
// REQUEST BODY TYPES (internal)
// ============================================================================

#[derive(Debug, Default, Serialize)]
struct SearchRequestBody {
    #[serde(skip_serializing_if = "Option::is_none")]
    query: Option<String>,

    #[serde(skip_serializing_if = "Vec::is_empty")]
    types: Vec<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    sort: Option<Sort>,

    #[serde(skip_serializing_if = "FilterExpression::is_empty")]
    filters: FilterExpression,
}

// ============================================================================
// BUILDER STRUCTS (public)
// ============================================================================

/// Request builder for search (global or in-space).
///
/// Obtained via [`AnytypeClient::search_global`] or [`AnytypeClient::search_in`].
#[derive(Debug)]
pub struct SearchRequest {
    client: Arc<HttpClient>,
    limit: Option<usize>,
    offset: Option<usize>,
    body: SearchRequestBody,
    limits: ValidationLimits,
    space_id: Option<String>,
}

impl SearchRequest {
    pub(crate) fn new(
        client: Arc<HttpClient>,
        limits: ValidationLimits,
        space_id: Option<String>,
    ) -> Self {
        Self {
            client,
            limit: None,
            offset: None,
            body: SearchRequestBody::default(),
            limits,
            space_id,
        }
    }

    /// Sets the search text (searches in name and content).
    pub fn text(mut self, text: impl Into<String>) -> Self {
        self.body.query = Some(text.into());
        self
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

    /// Limits results to specific types.
    pub fn types<S: Into<String>>(mut self, types: impl IntoIterator<Item = S>) -> Self {
        self.body.types = types.into_iter().map(Into::into).collect();
        self
    }

    /// Sorts results ascending by property.
    pub fn sort_asc(mut self, property: impl Into<String>) -> Self {
        self.body.sort = Some(Sort::asc(property));
        self
    }

    /// Sorts results descending by property.
    pub fn sort_desc(mut self, property: impl Into<String>) -> Self {
        self.body.sort = Some(Sort::desc(property));
        self
    }

    /// Adds a filter condition.
    pub fn filter(mut self, filter: Filter) -> Self {
        self.body.filters = FilterExpression::from(vec![filter]);
        self
    }

    /// Sets the filter expression.
    pub fn filters(mut self, filters: FilterExpression) -> Self {
        self.body.filters = filters;
        self
    }

    /// Executes the search.
    ///
    /// Note: the response may include archived objects,
    /// To exclude, filter returned values with `.filter(|obj| !obj.archived)`
    ///
    pub async fn execute(self) -> Result<PagedResult<Object>> {
        let query = Query::default()
            .set_limit_opt(&self.limit)
            .set_offset_opt(&self.offset);

        if let Some(space_id) = self.space_id {
            self.limits.validate_id(&space_id, "space_id")?;
            self.client
                .post_request_paged(
                    &format!("/v1/spaces/{}/search", &space_id),
                    &self.body,
                    query.into(),
                )
                .await
        } else {
            self.client
                .post_request_paged("/v1/search", &self.body, query.into())
                .await
        }
    }
}

// ============================================================================
// ANYTYPECLIENT METHODS
// ============================================================================

impl AnytypeClient {
    /// Creates a request builder for global search (all spaces).
    pub fn search_global(&self) -> SearchRequest {
        SearchRequest::new(self.client.clone(), self.config.limits.clone(), None)
    }

    /// Creates a request builder for search (all spaces).
    pub fn search_in(&self, space_id: impl Into<String>) -> SearchRequest {
        SearchRequest::new(
            self.client.clone(),
            self.config.limits.clone(),
            Some(space_id.into()),
        )
    }
}
