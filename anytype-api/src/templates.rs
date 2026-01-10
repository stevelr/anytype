//! # Anytype Templates
//!
//! This module provides a fluent builder API for working with templates.
//! Templates provide pre-configured structures for creating new objects.
//!
//! ## Template methods on AnytypeClient
//!
//! - [templates](AnytypeClient::templates) - list templtes in a space
//! - [template](AnytypeClient::template) - get a template
//!
//! To update template or delete templates, use the Object methods
//!
//! - [object](AnytypeClient::object) - get or delete object or template
//! - [update_object](AnytypeClient::object) - update object or template
//!

use std::sync::Arc;

use serde::Deserialize;

use crate::{
    Result,
    client::AnytypeClient,
    filters::Query,
    http_client::{GetPaged, HttpClient},
    prelude::*,
};

// ============================================================================
// RESPONSE TYPES (internal)
// ============================================================================

#[derive(Debug, Deserialize)]
struct TemplateResponse {
    template: Object,
}

// ============================================================================
// BUILDER STRUCTS (public)
// ============================================================================

/// Request builder for getting a single template.
#[derive(Debug)]
pub struct TemplateRequest {
    client: Arc<HttpClient>,
    limits: ValidationLimits,
    space_id: String,
    type_id: String,
    template_id: String,
}

impl TemplateRequest {
    pub(crate) fn new(
        client: Arc<HttpClient>,
        limits: ValidationLimits,
        space_id: impl Into<String>,
        type_id: impl Into<String>,
        template_id: impl Into<String>,
    ) -> Self {
        Self {
            client,
            limits,
            space_id: space_id.into(),
            type_id: type_id.into(),
            template_id: template_id.into(),
        }
    }

    /// Retrieves the template by ID.
    pub async fn get(self) -> Result<Object> {
        self.limits.validate_id(&self.space_id, "space_id")?;
        self.limits.validate_id(&self.type_id, "type_id")?;
        self.limits.validate_id(&self.template_id, "template_id")?;

        let response: TemplateResponse = self
            .client
            .get_request(
                &format!(
                    "/v1/spaces/{}/types/{}/templates/{}",
                    self.space_id, self.type_id, self.template_id
                ),
                Default::default(),
            )
            .await?;
        Ok(response.template)
    }
}

/// Request builder for listing templates.
#[derive(Debug)]
pub struct ListTemplatesRequest {
    client: Arc<HttpClient>,
    limits: ValidationLimits,
    space_id: String,
    type_id: String,
    limit: Option<usize>,
    offset: Option<usize>,
    filters: Vec<Filter>,
}

impl ListTemplatesRequest {
    pub(crate) fn new(
        client: Arc<HttpClient>,
        limits: ValidationLimits,
        space_id: impl Into<String>,
        type_id: impl Into<String>,
    ) -> Self {
        Self {
            client,
            limits,
            space_id: space_id.into(),
            type_id: type_id.into(),
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
    pub async fn list(self) -> Result<PagedResult<Object>> {
        self.limits.validate_id(&self.space_id, "space_id")?;
        self.limits.validate_id(&self.type_id, "type_id")?;

        let query = Query::default()
            .set_limit_opt(&self.limit)
            .set_offset_opt(&self.offset)
            .add_filters(&self.filters);

        self.client
            .get_request_paged(
                &format!(
                    "/v1/spaces/{}/types/{}/templates",
                    self.space_id, self.type_id
                ),
                query,
            )
            .await
    }
}

// ============================================================================
// ANYTYPECLIENT METHODS
// ============================================================================

impl AnytypeClient {
    /// Creates a request builder for getting a single template.
    pub fn template(
        &self,
        space_id: impl Into<String>,
        type_id: impl Into<String>,
        template_id: impl Into<String>,
    ) -> TemplateRequest {
        TemplateRequest::new(
            self.client.clone(),
            self.config.limits.clone(),
            space_id,
            type_id,
            template_id,
        )
    }

    /// Creates a request builder for listing templates for a type.
    pub fn templates(
        &self,
        space_id: impl Into<String>,
        type_id: impl Into<String>,
    ) -> ListTemplatesRequest {
        ListTemplatesRequest::new(
            self.client.clone(),
            self.config.limits.clone(),
            space_id,
            type_id,
        )
    }
}
