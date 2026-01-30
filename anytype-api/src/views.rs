//! # Anytype Views (for Collections and Queries)
//!
//! This module provides a fluent builder API for working with (collections and queries).
//!
//! - [`list_views`](AnytypeClient::list_views) - list views (for collections and queries)
//! - [`view_list_objects`](AnytypeClient::view_list_objects) - list objects in a collection or query
//! - [`view_remove_object`](AnytypeClient::view_remove_object) - remove an object from a view (collection)
//! - [`view_add_objects`](AnytypeClient::view_add_objects) - add objects to a collection
//!
//! ## Quick Start
//!
//! ```rust,no_run
//! use anytype::prelude::*;
//! # use anytype::Result;
//! # async fn example(client: &AnytypeClient) -> Result<()> {
//! let space_id = "ba000000";
//! let list_id = "ba111111";
//!
//! // List views for a collection or query
//! let views = client.list_views(space_id, list_id).list().await?;
//! for view in views.iter() {
//!   println!("{} {}", view.id, view.name.as_deref().unwrap_or("(unnamed)"));
//! }
//!
//! // Add objects to a collection
//! client.view_add_objects(space_id, list_id, ["obj1", "obj2"]).await?;
//! # Ok(())
//! # }
//! ```

use std::sync::Arc;

use serde::{Deserialize, Deserializer, Serialize};
use snafu::prelude::*;

use crate::{
    Result,
    client::AnytypeClient,
    filters::{Query, QueryWithFilters},
    http_client::{GetPaged, HttpClient},
    prelude::*,
};

/// View layout for list types
///
/// The 2025-11-08 openapi spec defined only grid and table.
/// Current implementation (as of 2026-Jan) removed table and adds calendar, gallery, graph, kanban, and list.
#[derive(
    Debug, Deserialize, Serialize, Clone, PartialEq, Eq, strum::Display, strum::EnumString,
)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum ViewLayout {
    Calendar,
    Gallery,
    Graph,
    Grid,
    Kanban,
    List,
}

/// Represents a view defined for a list.
#[derive(Debug, Deserialize, Serialize)]
pub struct View {
    /// Applied filters for the view
    #[serde(default, deserialize_with = "deserialize_vec_filter_or_null")]
    pub filters: Vec<Filter>,
    /// View identifier
    pub id: String,
    /// Layout of the view
    pub layout: ViewLayout,
    /// View name
    pub name: Option<String>,
    /// Sort options for the view
    #[serde(default, deserialize_with = "deserialize_vec_sort_or_null")]
    pub sorts: Vec<Sort>,
}

fn deserialize_vec_filter_or_null<'de, D>(deserializer: D) -> Result<Vec<Filter>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Option::<Vec<Filter>>::deserialize(deserializer)?;
    Ok(value.unwrap_or_default())
}

fn deserialize_vec_sort_or_null<'de, D>(deserializer: D) -> Result<Vec<Sort>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Option::<Vec<Sort>>::deserialize(deserializer)?;
    Ok(value.unwrap_or_default())
}

// ============================================================================
// REQUEST BODY TYPES (internal)
// ============================================================================

#[derive(Debug, Serialize)]
struct ViewAddObjectsRequest {
    objects: Vec<String>,
}

// ============================================================================
// BUILDER STRUCTS (public)
// ============================================================================

/// Request builder for listing objects in a list.
#[derive(Debug)]
pub struct ViewListObjectsRequest {
    client: Arc<HttpClient>,
    limits: ValidationLimits,
    space_id: String,
    list_id: String,
    view_id: Option<String>,
    limit: Option<u32>,
    offset: Option<u32>,
    filters: Vec<Filter>,
}

impl ViewListObjectsRequest {
    pub(crate) fn new(
        client: Arc<HttpClient>,
        limits: ValidationLimits,
        space_id: impl Into<String>,
        list_id: impl Into<String>,
    ) -> Self {
        Self {
            client,
            limits,
            space_id: space_id.into(),
            list_id: list_id.into(),
            view_id: None,
            limit: None,
            offset: None,
            filters: Vec::new(),
        }
    }

    /// Filters by a specific view.
    #[must_use]
    pub fn view(mut self, view_id: impl Into<String>) -> Self {
        self.view_id = Some(view_id.into());
        self
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

    /// Executes the request.
    pub async fn list(self) -> Result<PagedResult<Object>> {
        self.limits.validate_id(&self.space_id, "space_id")?;
        self.limits.validate_id(&self.list_id, "list_id")?;

        let query = Query::default()
            .set_limit_opt(self.limit)
            .set_offset_opt(self.offset)
            .add_filters(&self.filters);

        let path = if let Some(ref view_id) = self.view_id {
            ensure!(
                !view_id.is_empty(),
                ValidationSnafu {
                    message: "view_id is empty".to_string(),
                }
            );
            format!(
                "/v1/spaces/{}/lists/{}/views/{}/objects",
                self.space_id, self.list_id, view_id
            )
        } else {
            format!(
                "/v1/spaces/{}/lists/{}/objects",
                self.space_id, self.list_id
            )
        };

        self.client.get_request_paged(&path, query).await
    }
}

/// Request builder for listing views of a list.
#[derive(Debug)]
pub struct ListViewsRequest {
    client: Arc<HttpClient>,
    limits: ValidationLimits,
    space_id: String,
    list_id: String,
    limit: Option<u32>,
    offset: Option<u32>,
}

impl ListViewsRequest {
    pub(crate) fn new(
        client: Arc<HttpClient>,
        limits: ValidationLimits,
        space_id: impl Into<String>,
        list_id: impl Into<String>,
    ) -> Self {
        Self {
            client,
            limits,
            space_id: space_id.into(),
            list_id: list_id.into(),
            limit: None,
            offset: None,
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

    /// Executes the request.
    pub async fn list(self) -> Result<PagedResult<View>> {
        self.limits.validate_id(&self.space_id, "space_id")?;
        self.limits.validate_id(&self.list_id, "list_id")?;

        let query = Query::default()
            .set_limit_opt(self.limit)
            .set_offset_opt(self.offset);

        self.client
            .get_request_paged(
                &format!("/v1/spaces/{}/lists/{}/views", self.space_id, self.list_id),
                QueryWithFilters::from(query),
            )
            .await
    }
}

// ============================================================================
// ANYTYPECLIENT METHODS
// ============================================================================

impl AnytypeClient {
    /// Creates a request builder for listing views (of a list)
    pub fn list_views(
        &self,
        space_id: impl Into<String>,
        list_id: impl Into<String>,
    ) -> ListViewsRequest {
        ListViewsRequest::new(
            self.client.clone(),
            self.config.limits.clone(),
            space_id,
            list_id,
        )
    }

    /// Creates a request builder for listing objects in a view.
    pub fn view_list_objects(
        &self,
        space_id: impl Into<String>,
        list_id: impl Into<String>,
    ) -> ViewListObjectsRequest {
        ViewListObjectsRequest::new(
            self.client.clone(),
            self.config.limits.clone(),
            space_id,
            list_id,
        )
    }

    /// Adds objects to a collection.
    pub async fn view_add_objects<S: Into<String>>(
        &self,
        space_id: impl Into<String>,
        list_id: impl Into<String>,
        object_ids: impl IntoIterator<Item = S>,
    ) -> Result<String> {
        let space_id = space_id.into();
        let list_id = list_id.into();
        let objects: Vec<String> = object_ids.into_iter().map(Into::into).collect();

        self.config.limits.validate_id(&space_id, "space_id")?;
        self.config.limits.validate_id(&list_id, "list_id")?;
        for obj_id in &objects {
            self.config.limits.validate_id(obj_id, "object_id")?;
        }

        let request = ViewAddObjectsRequest { objects };

        self.client
            .post_request(
                &format!("/v1/spaces/{space_id}/lists/{list_id}/objects"),
                &request,
                QueryWithFilters::default(),
            )
            .await
    }

    /// Removes an object from a collection.
    pub async fn view_remove_object(
        &self,
        space_id: impl Into<String>,
        list_id: impl Into<String>,
        object_id: impl Into<String>,
    ) -> Result<String> {
        let space_id = space_id.into();
        let list_id = list_id.into();
        let object_id = object_id.into();

        self.config.limits.validate_id(&space_id, "space_id")?;
        self.config.limits.validate_id(&list_id, "list_id")?;
        self.config.limits.validate_id(&object_id, "object_id")?;
        self.client
            .delete_request(&format!(
                "/v1/spaces/{space_id}/lists/{list_id}/objects/{object_id}",
            ))
            .await
    }
}

// ============================================================================
// TESTS
// ============================================================================

#[cfg(test)]
mod tests {}
