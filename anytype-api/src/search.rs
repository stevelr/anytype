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
//! #   let client = AnytypeClient::new("doc test")?;
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
    config::MAX_PAGINATION_LIMIT,
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
    limit: Option<u32>,
    offset: Option<u32>,
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
    #[must_use]
    pub fn text(mut self, text: impl Into<String>) -> Self {
        self.body.query = Some(text.into());
        self
    }

    /// Sets the pagination limit.
    ///
    /// [`Self::execute`] rejects values outside `1..=1000` before HTTP.
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

    /// Limits results to specific types.
    #[must_use]
    pub fn types<S: Into<String>>(mut self, types: impl IntoIterator<Item = S>) -> Self {
        self.body.types = types.into_iter().map(Into::into).collect();
        self
    }

    /// Sorts results ascending by property.
    #[must_use]
    pub fn sort_asc(mut self, property: impl Into<String>) -> Self {
        self.body.sort = Some(Sort::asc(property));
        self
    }

    /// Sorts results descending by property.
    #[must_use]
    pub fn sort_desc(mut self, property: impl Into<String>) -> Self {
        self.body.sort = Some(Sort::desc(property));
        self
    }

    /// Adds a filter condition.
    #[must_use]
    pub fn filter(mut self, filter: Filter) -> Self {
        self.body.filters = FilterExpression::from(vec![filter]);
        self
    }

    /// Sets the filter expression.
    #[must_use]
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
        if self
            .limit
            .is_some_and(|limit| limit == 0 || limit > MAX_PAGINATION_LIMIT)
        {
            return Err(AnytypeError::Validation {
                message: format!("search limit must be between 1 and {MAX_PAGINATION_LIMIT}"),
            });
        }
        let query = Query::default()
            .set_limit_opt(self.limit)
            .set_offset_opt(self.offset);

        if let Some(space_id) = self.space_id {
            self.limits.validate_id(&space_id, "space_id")?;
            self.client
                .post_request_paged(
                    &format!("/v1/spaces/{space_id}/search"),
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

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tokio::net::TcpListener;

    use super::*;
    use crate::test_util::scripted_http::{
        ScriptedHttpContentType, ScriptedHttpFixture, ScriptedHttpResponse,
    };

    const EMPTY_PAGE: &str =
        r#"{"data":[],"pagination":{"has_more":false,"limit":25,"offset":3,"total":0}}"#;

    fn scripted_client(address: std::net::SocketAddr) -> AnytypeClient {
        let mut config = ClientConfig::default().app_name("search-wire-shape");
        config.base_url = Some(format!("http://{address}"));
        config.keystore = Some("env".to_owned());
        let client = AnytypeClient::with_config(config).expect("create scripted search client");
        client.set_api_key(HttpCredentials::new("fixture-token"));
        client
    }

    async fn scripted_search_client() -> (AnytypeClient, ScriptedHttpFixture) {
        let fixture = ScriptedHttpFixture::start(vec![ScriptedHttpResponse::new(
            reqwest::StatusCode::OK,
            ScriptedHttpContentType::Json,
            EMPTY_PAGE,
        )])
        .await
        .expect("start scripted search fixture");
        let client = scripted_client(fixture.address());
        (client, fixture)
    }

    fn sentinel_client(address: std::net::SocketAddr) -> AnytypeClient {
        let mut config = ClientConfig::default().app_name("search-limit-validation");
        config.base_url = Some(format!("http://{address}"));
        config.keystore = Some("env".to_owned());
        let client = AnytypeClient::with_config(config).expect("search limit sentinel client");
        client.set_api_key(HttpCredentials::new("fixture-token"));
        client
    }

    #[tokio::test]
    async fn invalid_search_limits_fail_before_http_for_both_scopes() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind search limit sentinel");
        let address = listener
            .local_addr()
            .expect("search limit sentinel address");
        let client = sentinel_client(address);
        let expected = format!("search limit must be between 1 and {MAX_PAGINATION_LIMIT}");

        for limit in [0, MAX_PAGINATION_LIMIT + 1] {
            for request in [client.search_global(), client.search_in("space-id")] {
                let error = request
                    .limit(limit)
                    .execute()
                    .await
                    .expect_err("invalid search limit must fail");
                assert!(
                    matches!(error, AnytypeError::Validation { ref message } if message == &expected)
                );
            }
        }
        assert_eq!(client.http_metrics().logical_operations, 0);
        assert_eq!(client.http_metrics().physical_attempts, 0);
        assert!(
            tokio::time::timeout(Duration::from_millis(50), listener.accept())
                .await
                .is_err(),
            "invalid search limits must not open a connection"
        );
    }

    #[tokio::test]
    async fn default_search_omits_body_fields_and_pagination_parameters() {
        let (client, fixture) = scripted_search_client().await;

        client
            .search_global()
            .execute()
            .await
            .expect("default scripted search response");

        let requests = fixture.finish().await.expect("default search request");
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].method(), "POST");
        assert_eq!(requests[0].path(), "/v1/search");
        assert_eq!(requests[0].body(), b"{}");
    }

    #[tokio::test]
    async fn search_forwards_nested_filters_sort_and_pagination_without_body_defaults() {
        let (client, fixture) = scripted_search_client().await;
        let filters = FilterExpression::and(
            vec![Filter::text_contains("name", "draft")],
            vec![FilterExpression::or(
                vec![
                    Filter::select_in("status", ["open", "blocked"]),
                    Filter::number_greater("priority", 2),
                ],
                Vec::new(),
            )],
        );

        client
            .search_global()
            .text("quarterly plan")
            .types(["page", "task"])
            .sort_desc("last_modified_date")
            .filters(filters)
            .limit(25)
            .offset(3)
            .execute()
            .await
            .expect("scripted search response");

        let requests = fixture.finish().await.expect("populated search request");
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].method(), "POST");
        assert_eq!(requests[0].path(), "/v1/search?limit=25&offset=3");
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(requests[0].body())
                .expect("search request body is JSON"),
            serde_json::json!({
                "query": "quarterly plan",
                "types": ["page", "task"],
                "sort": {"direction": "desc", "property_key": "last_modified_date"},
                "filters": {
                    "conditions": [
                        {"condition": "contains", "property_key": "name", "text": "draft"}
                    ],
                    "filters": [{
                        "conditions": [
                            {"condition": "in", "property_key": "status", "select": "open,blocked"},
                            {"condition": "gt", "property_key": "priority", "number": 2}
                        ],
                        "operator": "or"
                    }],
                    "operator": "and"
                }
            })
        );
    }

    #[tokio::test]
    async fn malformed_space_id_fails_before_http() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind malformed space sentinel");
        let address = listener
            .local_addr()
            .expect("malformed space sentinel address");
        let client = sentinel_client(address);

        let error = client
            .search_in("")
            .execute()
            .await
            .expect_err("empty space ID must fail validation");
        assert!(matches!(error, AnytypeError::Validation { .. }));
        assert_eq!(client.http_metrics().logical_operations, 0);
        assert_eq!(client.http_metrics().physical_attempts, 0);
        assert!(
            tokio::time::timeout(Duration::from_millis(50), listener.accept())
                .await
                .is_err(),
            "malformed space ID must not open a connection"
        );
    }
}
