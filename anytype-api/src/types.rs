//! # Anytype Types
//!
//! This module provides a fluent builder API for working with Anytype object types.
//!
//! ## Type methods on `AnytypeClient`
//!
//! - [`types`](AnytypeClient::types) - list types in the space
//! - [`get_type`](AnytypeClient::get_type) - get type for retrieval or deletion
//! - [`new_type`](AnytypeClient::new_type) - create a new type
//! - [`update_type`](AnytypeClient::update_type) - update type properties
//! - [`lookup_type_by_key`](AnytypeClient::lookup_type_by_key) - find type using key
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
//! // List all types
//! let types = client.types(&space_id).list().await?;
//! let some_type = types.iter().next().unwrap().clone();
//!
//! // Get a type by id
//! let typ = client.get_type(&space_id, &some_type.id).get().await?;
//!
//! // Get a type by key
//! let typ = client.lookup_type_by_key(&space_id, "page").await?;
//!
//! // Create a new type
//! let project = client.new_type(&space_id, "Project")
//!     .key("project")
//!     .create().await?;
//!
//! // Update a type: change its name and replace its recommended properties
//! let project = client.update_type(&space_id, &project.id)
//!     .name("My New Project")
//!     .property("Location", "location", PropertyFormat::Text)
//!     .update().await?;
//!
//! // Delete a type
//! client.get_type(&space_id, &project.id).delete().await?;
//!
//! # Ok(())
//! # }
//! ```
//!
//! ## Types
//!
//! - [`Type`] - Represents an Anytype object type
//! - [`TypePropertyClassification`] - Separates featured properties from the
//!   complete non-featured set replaceable by an update
//! - [`TypeLayout`] - Layout variants for types (Basic, Profile, Action, Note)
//! - [`TypeRequest`] - Builder for get/delete operations
//! - [`NewTypeRequest`] - Builder for creating types
//! - [`ListTypesRequest`] - Builder for listing types

use std::{
    collections::HashMap,
    future::Future,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use anytype_rpc::anytype::rpc::object::{close as object_close, show as object_show};
use anytype_rpc::model;
use prost_types::value::Kind;
use serde::{Deserialize, Deserializer, Serialize};
use snafu::prelude::*;
use tonic::Request;

use crate::{
    Result,
    cache::AnytypeCache,
    client::AnytypeClient,
    error::{CacheDisabledSnafu, NotFoundSnafu, OtherSnafu, ValidationSnafu},
    filters::{Query, QueryWithFilters},
    grpc_util::{ensure_error_ok, grpc_status, with_token_request},
    http_client::{GetPaged, HttpClient},
    prelude::*,
    verify::{VerifyConfig, VerifyPolicy, resolve_verify, verify_available},
};

/// Longest per-RPC deadline accepted by the finite type-property classifier.
pub const MAX_TYPE_PROPERTY_RPC_TIMEOUT: Duration = Duration::from_secs(5);

/// Cumulative work counters for type-property classification RPC ownership.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TypePropertyClassificationMetricsSnapshot {
    /// `ObjectShow` RPCs polled by the classifier.
    pub show_attempts: u64,
    /// `ObjectClose` RPCs polled by explicit cleanup or the detached fallback.
    pub close_attempts: u64,
    /// Detached cleanup fallbacks started after cancellation or failed cleanup.
    pub close_fallbacks: u64,
    /// Explicit or detached close attempts that confirmed cleanup.
    pub cleanup_successes: u64,
    /// Explicit or detached close attempts that did not confirm cleanup.
    pub cleanup_failures: u64,
}

#[derive(Debug, Default)]
pub(crate) struct TypePropertyClassificationMetrics {
    show_attempts: AtomicU64,
    close_attempts: AtomicU64,
    close_fallbacks: AtomicU64,
    cleanup_successes: AtomicU64,
    cleanup_failures: AtomicU64,
}

impl TypePropertyClassificationMetrics {
    pub(crate) fn snapshot(&self) -> TypePropertyClassificationMetricsSnapshot {
        TypePropertyClassificationMetricsSnapshot {
            show_attempts: self.show_attempts.load(Ordering::Relaxed),
            close_attempts: self.close_attempts.load(Ordering::Relaxed),
            close_fallbacks: self.close_fallbacks.load(Ordering::Relaxed),
            cleanup_successes: self.cleanup_successes.load(Ordering::Relaxed),
            cleanup_failures: self.cleanup_failures.load(Ordering::Relaxed),
        }
    }
}

/// Maximum number of featured and ordinary recommended property links
/// accepted by one exact type-property classification read.
pub const MAX_TYPE_PROPERTY_LINKS: usize = 1_000;

const RECOMMENDED_FEATURED_RELATIONS: &str = "recommendedFeaturedRelations";
const RECOMMENDED_RELATIONS: &str = "recommendedRelations";

/// Layout variants for types.
///
/// Determines the default appearance and behavior of objects of this type.
/// Note: This differs from [`ObjectLayout`] which has additional variants
/// (Bookmark, Set, Collection, Participant). Anytype's public REST create and
/// update contract accepts only the four variants below; collection-layout
/// types used by integration tests are created through the cleanup-safe helper
/// in [`crate::test_util::TestContext`].
#[derive(
    Debug, Deserialize, Serialize, Clone, Default, PartialEq, Eq, strum::Display, strum::EnumString,
)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum TypeLayout {
    /// Standard object layout with full editing capabilities
    #[default]
    Basic,
    /// Profile layout for user/contact information
    Profile,
    /// Action/task layout
    Action,
    /// Note layout - simplified, name is optional
    Note,
}

/// Property definition for type creation.
///
/// Defines a property to be associated with a new type.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct CreateTypeProperty {
    /// The format of the property (text, number, date, etc.)
    pub format: PropertyFormat,
    /// Unique key for the property
    pub key: String,
    /// Display name for the property
    pub name: String,
}

/// Represents an Anytype object type.
///
/// Types define the structure and default behavior for objects. Each type
/// has a unique key, a display name, and a default layout. Built-in types
/// include Page, Note, Task, and Bookmark.
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct Type {
    /// Data model type returned by the REST API.
    #[serde(default = "type_data_model")]
    pub object: DataModel,

    /// Whether the type is archived
    pub archived: bool,

    /// Type icon (emoji, file, or colored icon)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<Icon>,

    /// Unique type identifier (unique across all spaces)
    pub id: String,

    /// Key of the type (can be the same across spaces for known types, e.g., "page")
    pub key: String,

    /// Default layout for objects of this type
    #[serde(default)]
    pub layout: ObjectLayout,

    /// Display name of the type
    #[serde(default)]
    pub name: Option<String>,

    /// Plural form of the name
    #[serde(default)]
    pub plural_name: Option<String>,

    /// Properties linked to the type
    #[serde(default, deserialize_with = "deserialize_vec_properties_or_null")]
    pub properties: Vec<Property>,
}

fn type_data_model() -> DataModel {
    DataModel::Type
}

/// Source-backed classification of the properties linked to a type.
///
/// Anytype stores featured and ordinary recommended properties in separate
/// source lists, while the REST `Type.properties` field combines their visible
/// definitions and carries no classification boundary. Obtain this model with
/// [`TypeRequest::classify_properties`].
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct TypePropertyClassification {
    /// Exact ordered IDs from Anytype's featured-property source list.
    ///
    /// Some system-featured properties are intentionally omitted from the REST
    /// type representation, so not every ID necessarily has a corresponding
    /// entry in [`featured`](Self::featured).
    pub featured_ids: Vec<String>,

    /// REST-visible featured property definitions, in source-list order.
    pub featured: Vec<Property>,

    /// Complete non-featured recommended property list, in source-list order.
    ///
    /// This is the exact set replaced by [`UpdateTypeRequest::properties`] or
    /// removed by [`UpdateTypeRequest::clear_properties`].
    pub recommended: Vec<Property>,
}

/// Payload-free failure classification for the finite property-classification
/// lifecycle.
#[derive(Clone, Copy, Debug, PartialEq, Eq, strum::Display)]
#[strum(serialize_all = "snake_case")]
pub enum TypePropertyClassificationErrorKind {
    /// `ObjectShow` exceeded its caller-selected finite RPC deadline.
    RpcDeadline,
    /// The matching `ObjectClose` could not be confirmed within its deadline.
    CleanupFailed,
    /// No Tokio runtime was available to own the close fallback.
    RuntimeUnavailable,
}

type TypePropertyCleanupFuture = Pin<Box<dyn Future<Output = Result<()>> + Send>>;
type TypePropertyCleanupAction = Arc<dyn Fn(Duration) -> TypePropertyCleanupFuture + Send + Sync>;

/// Owns the matching close for one type-property `ObjectShow` boundary.
struct TypePropertyCloseGuard {
    action: TypePropertyCleanupAction,
    runtime: tokio::runtime::Handle,
    metrics: Option<Arc<TypePropertyClassificationMetrics>>,
    armed: bool,
}

impl TypePropertyCloseGuard {
    fn new(
        grpc: anytype_rpc::client::AnytypeGrpcClient,
        space_id: String,
        type_id: String,
        metrics: Arc<TypePropertyClassificationMetrics>,
    ) -> Result<Self> {
        let runtime = classification_runtime_handle()?;
        let raw_action: TypePropertyCleanupAction = Arc::new(move |timeout| {
            let grpc = grpc.clone();
            let space_id = space_id.clone();
            let type_id = type_id.clone();
            Box::pin(
                async move { close_type_property_view(grpc, space_id, type_id, timeout).await },
            )
        });
        let action = instrument_cleanup_action(raw_action, Arc::clone(&metrics));
        Ok(Self {
            action,
            runtime,
            metrics: Some(metrics),
            armed: true,
        })
    }

    #[cfg(test)]
    fn from_action(action: TypePropertyCleanupAction) -> Self {
        Self {
            action,
            runtime: tokio::runtime::Handle::current(),
            metrics: None,
            armed: true,
        }
    }

    #[cfg(test)]
    fn from_action_with_metrics(
        action: TypePropertyCleanupAction,
        metrics: Arc<TypePropertyClassificationMetrics>,
    ) -> Self {
        Self {
            action: instrument_cleanup_action(action, Arc::clone(&metrics)),
            runtime: tokio::runtime::Handle::current(),
            metrics: Some(metrics),
            armed: true,
        }
    }

    async fn cleanup(&mut self, timeout: Duration) -> Result<()> {
        match (self.action)(timeout).await {
            Ok(()) => {
                self.armed = false;
                Ok(())
            }
            Err(_) => classification_error(TypePropertyClassificationErrorKind::CleanupFailed),
        }
    }
}

fn instrument_cleanup_action(
    action: TypePropertyCleanupAction,
    metrics: Arc<TypePropertyClassificationMetrics>,
) -> TypePropertyCleanupAction {
    Arc::new(move |timeout| {
        let action = Arc::clone(&action);
        let metrics = Arc::clone(&metrics);
        Box::pin(async move {
            metrics.close_attempts.fetch_add(1, Ordering::Relaxed);
            let result = action(timeout).await;
            if result.is_ok() {
                metrics.cleanup_successes.fetch_add(1, Ordering::Relaxed);
            } else {
                metrics.cleanup_failures.fetch_add(1, Ordering::Relaxed);
            }
            result
        })
    })
}

fn classification_runtime_handle() -> Result<tokio::runtime::Handle> {
    tokio::runtime::Handle::try_current().map_err(|_| {
        classification_error_value(TypePropertyClassificationErrorKind::RuntimeUnavailable)
    })
}

impl Drop for TypePropertyCloseGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let action = Arc::clone(&self.action);
        let metrics = self.metrics.clone();
        self.runtime.spawn(async move {
            if let Some(metrics) = metrics.as_ref() {
                metrics.close_fallbacks.fetch_add(1, Ordering::Relaxed);
            }
            let _ = action(MAX_TYPE_PROPERTY_RPC_TIMEOUT).await;
        });
    }
}

impl TypePropertyClassification {
    /// Returns the complete property set replaceable by a type update.
    #[must_use]
    pub fn replaceable(&self) -> &[Property] {
        &self.recommended
    }
}

fn deserialize_vec_properties_or_null<'de, D>(deserializer: D) -> Result<Vec<Property>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Option::<Vec<Property>>::deserialize(deserializer)?;
    Ok(value.unwrap_or_default())
}

impl Type {
    /// Returns true if this is a built-in system type.
    ///
    /// System types like "page" and "note" cannot be deleted.
    pub fn is_system_type(&self) -> bool {
        matches!(self.key.as_str(), "page" | "note" | "task" | "bookmark")
    }

    /// Returns the name of the type, or the key if name is not set.
    pub fn display_name(&self) -> &str {
        self.name.as_deref().unwrap_or(&self.key)
    }

    pub fn get_property_by_key(&self, property_key: &str) -> Option<&Property> {
        self.properties.iter().find(|prop| prop.key == property_key)
    }
}

// ============================================================================
// RESPONSE TYPES (internal)
// ============================================================================

/// Response wrapper for single type operations
#[derive(Debug, Deserialize)]
struct TypeResponse {
    #[serde(rename = "type")]
    type_: Type,
}

// ============================================================================
// REQUEST BODY TYPES (internal)
// ============================================================================

/// Internal request body for creating a type
#[derive(Debug, Serialize)]
struct CreateTypeRequestBody {
    name: String,

    #[serde(skip_serializing_if = "Option::is_none")]
    key: Option<String>,

    plural_name: String,

    #[serde(skip_serializing_if = "Option::is_none")]
    icon: Option<Icon>,

    layout: TypeLayout,

    #[serde(skip_serializing_if = "Vec::is_empty")]
    properties: Vec<CreateTypeProperty>,
}

/// Internal request body for updating a type
#[derive(Debug, Serialize, Default)]
struct UpdateTypeRequestBody {
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    key: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    plural_name: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    icon: Option<Icon>,

    #[serde(skip_serializing_if = "Option::is_none")]
    layout: Option<TypeLayout>,

    #[serde(skip_serializing_if = "Option::is_none")]
    properties: Option<Vec<CreateTypeProperty>>,
}

// ============================================================================
// BUILDER STRUCTS (public)
// ============================================================================

/// Request builder for getting or deleting a single type.
///
/// Obtained via [`AnytypeClient::get_type`].
#[derive(Debug)]
pub struct TypeRequest {
    api: AnytypeClient,
    client: Arc<HttpClient>,
    limits: ValidationLimits,
    space_id: String,
    type_id: String,
    cache: Arc<AnytypeCache>,
}

impl TypeRequest {
    /// Creates a new `TypeRequest`.
    pub(crate) fn new(
        api: AnytypeClient,
        space_id: impl Into<String>,
        type_id: impl Into<String>,
    ) -> Self {
        Self {
            client: api.client.clone(),
            limits: api.config.limits.clone(),
            space_id: space_id.into(),
            type_id: type_id.into(),
            cache: api.cache.clone(),
            api,
        }
    }

    /// Retrieves the type by ID.
    ///
    /// # Returns
    /// The type with all its metadata and properties.
    ///
    /// # Errors
    /// - [`AnytypeError::NotFound`] if the type doesn't exist
    /// - [`AnytypeError::Validation`] if IDs are invalid
    pub async fn get(self) -> Result<Type> {
        self.limits.validate_id(&self.space_id, "space_id")?;
        self.limits.validate_id(&self.type_id, "type_id")?;

        if self.cache.is_enabled() {
            if let Some(typ) = self.cache.get_type(&self.space_id, &self.type_id) {
                return Ok((*typ).clone());
            }
            // see note on locking design in cache.rs
            if !self.cache.has_types(&self.space_id) {
                prime_cache_types(&self.client, &self.cache, &self.space_id).await?;
                if let Some(type_) = self.cache.get_type(&self.space_id, &self.type_id) {
                    return Ok((*type_).clone());
                }
            }
            return NotFoundSnafu {
                obj_type: "Type".to_string(),
                key: self.type_id.clone(),
            }
            .fail();
        }
        self.fetch_direct().await
    }

    /// Retrieves the type with one cache-independent HTTP request.
    ///
    /// Unlike [`get`](Self::get), this method neither reads nor primes the
    /// in-memory type cache. It validates the scoped space and type IDs before
    /// dispatch, then rejects a successful response whose type ID differs from
    /// the requested ID. This is useful for bounded resolver and protocol
    /// paths that must not turn a single-ID lookup into an all-types scan.
    ///
    /// # Returns
    /// The type returned for the exact scoped type endpoint.
    ///
    /// # Errors
    /// - [`AnytypeError::NotFound`] if the type doesn't exist
    /// - [`AnytypeError::Validation`] if either ID is invalid
    /// - [`AnytypeError::Other`] if the upstream response identity does not
    ///   match the scoped request
    pub async fn get_direct(self) -> Result<Type> {
        self.limits.validate_id(&self.space_id, "space_id")?;
        self.limits.validate_id(&self.type_id, "type_id")?;
        self.fetch_direct().await
    }

    /// Reads the exact replaceable property set and its featured-property
    /// classification without reading or priming either metadata cache.
    ///
    /// One direct REST type GET supplies public property definitions. One gRPC
    /// `ObjectShow` supplies the separate source ID lists because the REST wire
    /// model flattens them. The shown view is released with a finite,
    /// cancellation-resilient owned `ObjectClose`. The combined source-list size is capped by
    /// [`MAX_TYPE_PROPERTY_LINKS`], and the read fails whole on duplicate,
    /// overlapping, missing, extra, malformed, or inconsistent evidence.
    ///
    /// These two reads are not an atomic server snapshot. A concurrent edit or
    /// eventual-consistency window can therefore produce an error; callers may
    /// retry the complete read. gRPC credentials are required.
    ///
    /// # Errors
    /// - [`AnytypeError::Validation`] if either scoped ID is invalid
    /// - [`AnytypeError::NotFound`] if the exact type is not returned
    /// - [`AnytypeError::GrpcUnavailable`] when gRPC credentials are unavailable
    /// - [`AnytypeError::Other`] for malformed, oversized, or inconsistent
    ///   upstream evidence
    pub async fn classify_properties(self) -> Result<TypePropertyClassification> {
        self.classify_properties_with_deadline(MAX_TYPE_PROPERTY_RPC_TIMEOUT)
            .await
    }

    /// Reads the exact property classification with a caller-selected finite
    /// deadline for `ObjectShow`.
    ///
    /// The deadline must be nonzero and no greater than
    /// [`MAX_TYPE_PROPERTY_RPC_TIMEOUT`]. Every explicit or detached
    /// `ObjectClose` receives its own fresh [`MAX_TYPE_PROPERTY_RPC_TIMEOUT`]
    /// deadline. The close lifecycle is owned before show dispatch, so dropping
    /// this future during show or close starts one detached close fallback on
    /// the current Tokio runtime.
    ///
    /// # Errors
    /// - [`AnytypeError::Validation`] if IDs or the deadline are invalid
    /// - [`AnytypeError::TypePropertyClassification`] for an RPC deadline or
    ///   unconfirmed cleanup
    /// - the errors documented by [`Self::classify_properties`]
    pub async fn classify_properties_with_deadline(
        self,
        rpc_timeout: Duration,
    ) -> Result<TypePropertyClassification> {
        self.limits.validate_id(&self.space_id, "space_id")?;
        self.limits.validate_id(&self.type_id, "type_id")?;
        ensure!(
            !rpc_timeout.is_zero() && rpc_timeout <= MAX_TYPE_PROPERTY_RPC_TIMEOUT,
            ValidationSnafu {
                message: "type property RPC deadline must be between zero and five seconds"
                    .to_owned(),
            }
        );

        let typ = self.fetch_direct().await?;
        let (featured_ids, recommended_ids) = fetch_type_property_source_ids(
            &self.api,
            &self.limits,
            &self.space_id,
            &self.type_id,
            rpc_timeout,
        )
        .await?;
        classify_type_properties(typ.properties, featured_ids, recommended_ids)
    }

    async fn fetch_direct(&self) -> Result<Type> {
        let response: TypeResponse = self
            .client
            .get_request(
                &format!("/v1/spaces/{}/types/{}", self.space_id, self.type_id),
                QueryWithFilters::default(),
            )
            .await?;
        if response.type_.id != self.type_id {
            return OtherSnafu {
                message: "Anytype returned a mismatched type identity".to_string(),
            }
            .fail();
        }
        Ok(response.type_)
    }

    /// Deletes (archives) the type.
    ///
    /// # Returns
    /// The deleted type.
    ///
    /// # Errors
    /// - [`AnytypeError::NotFound`] if the type doesn't exist
    /// - [`AnytypeError::Forbidden`] if you don't have permission
    pub async fn delete(self) -> Result<Type> {
        self.limits.validate_id(&self.space_id, "space_id")?;
        self.limits.validate_id(&self.type_id, "type_id")?;

        let response: TypeResponse = self
            .client
            .delete_request(&format!(
                "/v1/spaces/{}/types/{}",
                self.space_id, self.type_id
            ))
            .await?;

        if self.cache.has_types(&self.space_id) {
            self.cache.delete_type(&self.space_id, &self.type_id);
        }
        Ok(response.type_)
    }
}

async fn fetch_type_property_source_ids(
    client: &AnytypeClient,
    limits: &ValidationLimits,
    space_id: &str,
    type_id: &str,
    rpc_timeout: Duration,
) -> Result<(Vec<String>, Vec<String>)> {
    let grpc = client.grpc_client().await?;
    let mut commands = grpc.client_commands();
    let request = object_show::Request {
        context_id: type_id.to_owned(),
        object_id: type_id.to_owned(),
        space_id: space_id.to_owned(),
        include_relations_as_dependent_objects: false,
        ..Default::default()
    };
    let mut request = with_token_request(Request::new(request), grpc.token())?;
    request.set_timeout(rpc_timeout);
    let metrics = Arc::clone(&client.type_property_metrics);
    let mut cleanup = TypePropertyCloseGuard::new(
        grpc,
        space_id.to_owned(),
        type_id.to_owned(),
        Arc::clone(&metrics),
    )?;
    metrics.show_attempts.fetch_add(1, Ordering::Relaxed);
    let response = match tokio::time::timeout(rpc_timeout, commands.object_show(request)).await {
        Ok(Ok(response)) => response.into_inner(),
        Ok(Err(status)) => {
            let show_error = grpc_status(status);
            cleanup.cleanup(MAX_TYPE_PROPERTY_RPC_TIMEOUT).await?;
            return Err(show_error);
        }
        Err(_) => {
            cleanup.cleanup(MAX_TYPE_PROPERTY_RPC_TIMEOUT).await?;
            return classification_error(TypePropertyClassificationErrorKind::RpcDeadline);
        }
    };
    let response_error = ensure_error_ok(response.error.as_ref(), "type property source read");
    let cleanup_result = cleanup.cleanup(MAX_TYPE_PROPERTY_RPC_TIMEOUT).await;
    finish_type_property_show(response_error, cleanup_result)?;

    let view = response.object_view.ok_or_else(|| AnytypeError::Other {
        message: "type property source read returned no object view".to_owned(),
    })?;
    type_property_source_ids_from_view(&view, limits, type_id)
}

fn finish_type_property_show(
    response_result: Result<()>,
    cleanup_result: Result<()>,
) -> Result<()> {
    cleanup_result?;
    response_result
}

async fn close_type_property_view(
    grpc: anytype_rpc::client::AnytypeGrpcClient,
    space_id: String,
    type_id: String,
    rpc_timeout: Duration,
) -> Result<()> {
    let mut commands = grpc.client_commands();
    let close = object_close::Request {
        context_id: type_id.clone(),
        object_id: type_id,
        space_id,
    };
    let mut close = with_token_request(Request::new(close), grpc.token())?;
    close.set_timeout(rpc_timeout);
    let response = tokio::time::timeout(rpc_timeout, commands.object_close(close))
        .await
        .map_err(|_| {
            classification_error_value(TypePropertyClassificationErrorKind::CleanupFailed)
        })?
        .map_err(grpc_status)?
        .into_inner();
    ensure_error_ok(response.error.as_ref(), "type property source cleanup")
}

fn classification_error<T>(kind: TypePropertyClassificationErrorKind) -> Result<T> {
    Err(classification_error_value(kind))
}

fn classification_error_value(kind: TypePropertyClassificationErrorKind) -> AnytypeError {
    AnytypeError::TypePropertyClassification { kind }
}

fn type_property_source_ids_from_view(
    view: &model::ObjectView,
    limits: &ValidationLimits,
    type_id: &str,
) -> Result<(Vec<String>, Vec<String>)> {
    let mut matching = view.details.iter().filter(|details| details.id == type_id);
    let first = matching.next().ok_or_else(|| AnytypeError::Other {
        message: "type property source read omitted the requested type details".to_owned(),
    })?;
    let source_ids = type_property_source_ids_from_details(first, limits)?;
    for duplicate in matching {
        ensure!(
            type_property_source_ids_from_details(duplicate, limits)? == source_ids,
            OtherSnafu {
                message: "type property source read returned conflicting type details".to_owned(),
            }
        );
    }
    Ok(source_ids)
}

fn type_property_source_ids_from_details(
    details: &model::object_view::DetailsSet,
    limits: &ValidationLimits,
) -> Result<(Vec<String>, Vec<String>)> {
    let details = details
        .details
        .as_ref()
        .ok_or_else(|| AnytypeError::Other {
            message: "type property source read returned empty type details".to_owned(),
        })?;

    let featured = property_source_ids(details, RECOMMENDED_FEATURED_RELATIONS, limits)?;
    let recommended = property_source_ids(details, RECOMMENDED_RELATIONS, limits)?;
    let count = featured
        .len()
        .checked_add(recommended.len())
        .ok_or_else(|| AnytypeError::Other {
            message: "type property source link count overflowed".to_owned(),
        })?;
    ensure!(
        count <= MAX_TYPE_PROPERTY_LINKS,
        OtherSnafu {
            message: "type property source exceeded the 1,000-link limit".to_owned(),
        }
    );
    Ok((featured, recommended))
}

fn property_source_ids(
    details: &prost_types::Struct,
    key: &str,
    limits: &ValidationLimits,
) -> Result<Vec<String>> {
    let Some(value) = details.fields.get(key) else {
        return Ok(Vec::new());
    };
    let Some(Kind::ListValue(list)) = value.kind.as_ref() else {
        return OtherSnafu {
            message: "type property source field was not a list".to_owned(),
        }
        .fail();
    };
    ensure!(
        list.values.len() <= MAX_TYPE_PROPERTY_LINKS,
        OtherSnafu {
            message: "type property source exceeded the 1,000-link limit".to_owned(),
        }
    );

    let mut ids = Vec::with_capacity(list.values.len());
    for value in &list.values {
        let Some(Kind::StringValue(id)) = value.kind.as_ref() else {
            return OtherSnafu {
                message: "type property source list contained a non-string ID".to_owned(),
            }
            .fail();
        };
        if limits.validate_id(id, "property_id").is_err() {
            return OtherSnafu {
                message: "type property source list contained an invalid ID".to_owned(),
            }
            .fail();
        }
        ids.push(id.clone());
    }
    Ok(ids)
}

fn classify_type_properties(
    properties: Vec<Property>,
    featured_ids: Vec<String>,
    recommended_ids: Vec<String>,
) -> Result<TypePropertyClassification> {
    let mut classes =
        HashMap::with_capacity(featured_ids.len().saturating_add(recommended_ids.len()));
    for id in &featured_ids {
        ensure!(
            classes.insert(id.as_str(), true).is_none(),
            OtherSnafu {
                message: "type property source lists contained duplicate IDs".to_owned(),
            }
        );
    }
    for id in &recommended_ids {
        ensure!(
            classes.insert(id.as_str(), false).is_none(),
            OtherSnafu {
                message: "type property source lists overlapped or contained duplicate IDs"
                    .to_owned(),
            }
        );
    }

    let mut definitions = HashMap::with_capacity(properties.len());
    for property in properties {
        ensure!(
            classes.contains_key(property.id.as_str()),
            OtherSnafu {
                message: "REST type properties contained an unclassified property".to_owned(),
            }
        );
        let id = property.id.clone();
        ensure!(
            definitions.insert(id, property).is_none(),
            OtherSnafu {
                message: "REST type properties contained a duplicate property".to_owned(),
            }
        );
    }

    let mut featured = Vec::with_capacity(featured_ids.len());
    for id in &featured_ids {
        if let Some(property) = definitions.remove(id) {
            featured.push(property);
        }
    }

    let mut recommended = Vec::with_capacity(recommended_ids.len());
    for id in &recommended_ids {
        let property = definitions.remove(id).ok_or_else(|| AnytypeError::Other {
            message: "REST type properties omitted a replaceable property definition".to_owned(),
        })?;
        recommended.push(property);
    }
    ensure!(
        definitions.is_empty(),
        OtherSnafu {
            message: "REST type properties could not be fully classified".to_owned(),
        }
    );

    Ok(TypePropertyClassification {
        featured_ids,
        featured,
        recommended,
    })
}

/// Request builder for creating a new type.
///
/// Obtained via [`AnytypeClient::new_type`].
///
#[derive(Debug)]
pub struct NewTypeRequest {
    client: Arc<HttpClient>,
    limits: ValidationLimits,
    space_id: String,
    name: String,
    key: Option<String>,
    plural_name: String,
    icon: Option<Icon>,
    layout: TypeLayout,
    properties: Vec<CreateTypeProperty>,
    cache: Arc<AnytypeCache>,
    verify_policy: VerifyPolicy,
    verify_config: Option<VerifyConfig>,
}

impl NewTypeRequest {
    /// Creates a new `NewTypeRequest`. You must specify the name and `plural_name`.
    /// Defaults to Basic Layout
    pub(crate) fn new(
        client: Arc<HttpClient>,
        limits: ValidationLimits,
        space_id: impl Into<String>,
        name: String,
        plural_name: String,
        cache: Arc<AnytypeCache>,
        verify_config: Option<VerifyConfig>,
    ) -> Self {
        Self {
            client,
            limits,
            space_id: space_id.into(),
            name,
            key: None,
            plural_name,
            icon: None,
            layout: TypeLayout::Basic,
            properties: Vec::new(),
            cache,
            verify_policy: VerifyPolicy::Default,
            verify_config,
        }
    }

    /// Sets the plural name.
    ///
    /// Default plural name is the name + 's'.
    ///
    /// # Arguments
    /// * `plural_name` - plural display name for the type
    #[must_use]
    pub fn plural_name(mut self, plural_name: impl Into<String>) -> Self {
        self.plural_name = plural_name.into();
        self
    }

    /// Sets the type key.
    ///
    /// The key is a unique identifier for the type, typically lowercase
    /// with underscores (e.g., `project`, `meeting_note`).
    ///
    /// # Arguments
    /// * `key` - Unique key for the type
    #[must_use]
    pub fn key(mut self, key: impl Into<String>) -> Self {
        self.key = Some(key.into());
        self
    }

    /// Sets the type icon.
    ///
    /// # Arguments
    /// * `icon` - Icon for the type
    #[must_use]
    pub fn icon(mut self, icon: Icon) -> Self {
        self.icon = Some(icon);
        self
    }

    /// Sets the default layout for objects of this type.
    ///
    /// # Arguments
    /// * `layout` - Default layout for new objects
    #[must_use]
    pub fn layout(mut self, layout: TypeLayout) -> Self {
        self.layout = layout;
        self
    }

    /// Enables read-after-write verification for this request.
    #[must_use]
    pub fn ensure_available(mut self) -> Self {
        self.verify_policy = VerifyPolicy::Enabled;
        self
    }

    /// Enables verification using a custom config for this request.
    #[must_use]
    pub fn ensure_available_with(mut self, config: VerifyConfig) -> Self {
        self.verify_policy = VerifyPolicy::Enabled;
        self.verify_config = Some(config);
        self
    }

    /// Disables verification for this request.
    #[must_use]
    pub fn no_verify(mut self) -> Self {
        self.verify_policy = VerifyPolicy::Disabled;
        self
    }

    /// Adds a property definition to the type.
    ///
    /// # Arguments
    /// * `name` - name of property to add
    /// * `key` - property key
    /// * `format` - property format
    #[must_use]
    pub fn property(
        mut self,
        name: impl Into<String>,
        key: impl Into<String>,
        format: PropertyFormat,
    ) -> Self {
        self.properties.push({
            CreateTypeProperty {
                name: name.into(),
                key: key.into(),
                format,
            }
        });
        self
    }

    /// Adds multiple property definitions to the type.
    ///
    /// # Arguments
    /// * `properties` - Iterator of property definitions
    #[must_use]
    pub fn properties(mut self, properties: impl IntoIterator<Item = CreateTypeProperty>) -> Self {
        self.properties.extend(properties);
        self
    }

    /// Creates the type with the configured settings.
    ///
    /// # Returns
    /// The newly created type.
    ///
    /// # Errors
    /// - [`AnytypeError::Validation`] if name is not provided or invalid
    pub async fn create(self) -> Result<Type> {
        self.limits.validate_id(&self.space_id, "space_id")?;
        self.limits.validate_name(&self.name, "type name")?;

        let request_body = CreateTypeRequestBody {
            name: self.name,
            key: self.key,
            plural_name: self.plural_name,
            icon: self.icon,
            layout: self.layout,
            properties: self.properties,
        };

        let response: TypeResponse = self
            .client
            .post_request(
                &format!("/v1/spaces/{}/types", self.space_id),
                &request_body,
                QueryWithFilters::default(),
            )
            .await?;

        if self.cache.has_types(&self.space_id) {
            self.cache.set_type(&self.space_id, response.type_.clone());
        }
        let typ = response.type_;
        if let Some(config) = resolve_verify(self.verify_policy, self.verify_config.as_ref()) {
            return verify_available(&config, "Type", &typ.id, || async {
                let response: TypeResponse = self
                    .client
                    .get_request(
                        &format!("/v1/spaces/{}/types/{}", self.space_id, typ.id),
                        QueryWithFilters::default(),
                    )
                    .await?;
                Ok(response.type_)
            })
            .await;
        }
        Ok(typ)
    }
}

/// Request builder for updating an existing type.
///
/// Obtained via [`AnytypeClient::update_type`].
///
#[derive(Debug)]
pub struct UpdateTypeRequest {
    client: Arc<HttpClient>,
    limits: ValidationLimits,
    space_id: String,
    type_id: String,
    name: Option<String>,
    key: Option<String>,
    plural_name: Option<String>,
    icon: Option<Icon>,
    layout: Option<TypeLayout>,
    properties: Option<Vec<CreateTypeProperty>>,
    cache: Arc<AnytypeCache>,
    verify_policy: VerifyPolicy,
    verify_config: Option<VerifyConfig>,
}

impl UpdateTypeRequest {
    /// Creates a new `UpdateTypeRequest`.
    pub(crate) fn new(
        client: Arc<HttpClient>,
        limits: ValidationLimits,
        space_id: impl Into<String>,
        type_id: impl Into<String>,
        cache: Arc<AnytypeCache>,
        verify_config: Option<VerifyConfig>,
    ) -> Self {
        Self {
            client,
            limits,
            space_id: space_id.into(),
            type_id: type_id.into(),
            name: None,
            key: None,
            plural_name: None,
            icon: None,
            layout: None,
            properties: None,
            cache,
            verify_policy: VerifyPolicy::Default,
            verify_config,
        }
    }

    /// Updates the type name.
    ///
    /// # Arguments
    /// * `name` - New display name for the type
    #[must_use]
    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Updates the type key.
    ///
    /// # Arguments
    /// * `key` - New key for the type
    #[must_use]
    pub fn key(mut self, key: impl Into<String>) -> Self {
        self.key = Some(key.into());
        self
    }

    /// Updates the plural name.
    ///
    /// # Arguments
    /// * `plural_name` - New plural form of the type name
    #[must_use]
    pub fn plural_name(mut self, plural_name: impl Into<String>) -> Self {
        self.plural_name = Some(plural_name.into());
        self
    }

    /// Updates the type icon.
    ///
    /// # Arguments
    /// * `icon` - New icon for the type
    #[must_use]
    pub fn icon(mut self, icon: Icon) -> Self {
        self.icon = Some(icon);
        self
    }

    /// Updates the default layout.
    ///
    /// # Arguments
    /// * `layout` - New default layout for objects of this type
    #[must_use]
    pub fn layout(mut self, layout: TypeLayout) -> Self {
        self.layout = Some(layout);
        self
    }

    /// Enables read-after-write verification for this request.
    #[must_use]
    pub fn ensure_available(mut self) -> Self {
        self.verify_policy = VerifyPolicy::Enabled;
        self
    }

    /// Enables verification using a custom config for this request.
    #[must_use]
    pub fn ensure_available_with(mut self, config: VerifyConfig) -> Self {
        self.verify_policy = VerifyPolicy::Enabled;
        self.verify_config = Some(config);
        self
    }

    /// Disables verification for this request.
    #[must_use]
    pub fn no_verify(mut self) -> Self {
        self.verify_policy = VerifyPolicy::Disabled;
        self
    }

    /// Adds a property definition to the replacement property list.
    ///
    /// When this method is used, the REST API replaces all existing
    /// non-featured recommended properties with the properties supplied to
    /// this update. It does not append to the type's current properties.
    ///
    /// # Arguments
    /// * `name` - name of property to add
    /// * `key` - property key
    /// * `format` - property format
    #[must_use]
    pub fn property(
        mut self,
        name: impl Into<String>,
        key: impl Into<String>,
        format: PropertyFormat,
    ) -> Self {
        self.properties.get_or_insert_default().push({
            CreateTypeProperty {
                name: name.into(),
                key: key.into(),
                format,
            }
        });
        self
    }

    /// Replaces all non-featured recommended properties on the type.
    ///
    /// The provided collection is the complete replacement, not a set of
    /// additions. Pass an empty collection or use [`Self::clear_properties`]
    /// to remove all non-featured recommended properties.
    #[must_use]
    pub fn properties(mut self, properties: impl IntoIterator<Item = CreateTypeProperty>) -> Self {
        self.properties = Some(properties.into_iter().collect());
        self
    }

    /// Removes all non-featured recommended properties from the type.
    ///
    /// Featured properties managed by Anytype are not affected.
    #[must_use]
    pub fn clear_properties(mut self) -> Self {
        self.properties = Some(Vec::new());
        self
    }

    /// Applies the update to the type.
    ///
    /// # Returns
    /// The updated type.
    ///
    /// # Errors
    /// - [`AnytypeError::Validation`] if called without setting any fields
    /// - [`AnytypeError::NotFound`] if the type doesn't exist
    pub async fn update(self) -> Result<Type> {
        self.limits.validate_id(&self.space_id, "space_id")?;
        self.limits.validate_id(&self.type_id, "type_id")?;

        // Check that at least one field is being updated
        ensure!(
            self.name.is_some()
                || self.key.is_some()
                || self.plural_name.is_some()
                || self.icon.is_some()
                || self.layout.is_some()
                || self.properties.is_some(),
            ValidationSnafu {
                message: "update_type: must set at least one field to update".to_string(),
            }
        );

        if let Some(ref name) = self.name {
            self.limits.validate_name(name, "type")?;
        }

        let request_body = UpdateTypeRequestBody {
            name: self.name,
            key: self.key,
            plural_name: self.plural_name,
            icon: self.icon,
            layout: self.layout,
            properties: self.properties,
        };

        let response: TypeResponse = self
            .client
            .patch_request(
                &format!("/v1/spaces/{}/types/{}", self.space_id, self.type_id),
                &request_body,
            )
            .await?;

        if self.cache.has_types(&self.space_id) {
            self.cache.set_type(&self.space_id, response.type_.clone());
        }

        let typ = response.type_;
        if let Some(config) = resolve_verify(self.verify_policy, self.verify_config.as_ref()) {
            return verify_available(&config, "Type", &typ.id, || async {
                let response: TypeResponse = self
                    .client
                    .get_request(
                        &format!("/v1/spaces/{}/types/{}", self.space_id, typ.id),
                        QueryWithFilters::default(),
                    )
                    .await?;
                Ok(response.type_)
            })
            .await;
        }
        Ok(typ)
    }
}

/// Request builder for listing types in a space.
///
/// Obtained via [`AnytypeClient::types`].
///
#[derive(Debug)]
pub struct ListTypesRequest {
    client: Arc<HttpClient>,
    limits: ValidationLimits,
    space_id: String,
    limit: Option<u32>,
    offset: Option<u32>,
    filters: Vec<Filter>,
    cache: Arc<AnytypeCache>,
}

impl ListTypesRequest {
    /// Creates a new `ListTypesRequest`.
    pub(crate) fn new(
        client: Arc<HttpClient>,
        limits: ValidationLimits,
        space_id: impl Into<String>,
        cache: Arc<AnytypeCache>,
    ) -> Self {
        Self {
            client,
            limits,
            space_id: space_id.into(),
            limit: None,
            offset: None,
            filters: Vec::new(),
            cache,
        }
    }

    /// Sets the pagination limit (max items per page).
    ///
    /// Default is 100, maximum is 1000.
    ///
    /// # Arguments
    /// * `limit` - Number of items to return per page
    #[must_use]
    pub fn limit(mut self, limit: u32) -> Self {
        self.limit = Some(limit);
        self
    }

    /// Sets the pagination offset (starting position).
    ///
    /// # Arguments
    /// * `offset` - Number of items to skip
    #[must_use]
    pub fn offset(mut self, offset: u32) -> Self {
        self.offset = Some(offset);
        self
    }

    /// Adds a filter condition.
    ///
    /// Multiple filters are combined with AND logic.
    ///
    /// # Arguments
    /// * `filter` - Filter condition to add
    #[must_use]
    pub fn filter(mut self, filter: Filter) -> Self {
        self.filters.push(filter);
        self
    }

    /// Adds multiple filter conditions.
    ///
    /// # Arguments
    /// * `filters` - Iterator of filters to add
    #[must_use]
    pub fn filters(mut self, filters: impl IntoIterator<Item = Filter>) -> Self {
        self.filters.extend(filters);
        self
    }

    /// Executes the list request.
    ///
    /// # Returns
    /// A paginated result containing the matching types.
    ///
    /// To take advantage of cached properties for the `list()` method,
    /// the cache must be enabled, and  the query
    /// parameter must not contain filters or pagination limits or offsets.
    ///
    /// The response may include archived types,
    /// To exclude, filter returned values with `.filter(|typ| !typ.archived)`
    ///
    /// # Errors
    /// - [`AnytypeError::Validation`] if `space_id` is invalid
    pub async fn list(self) -> Result<PagedResult<Type>> {
        self.limits.validate_id(&self.space_id, "space_id")?;

        if self.cache.is_enabled()
            && self.limit.is_none()
            && self.offset.unwrap_or_default() == 0
            && self.filters.is_empty()
        {
            // see note on locking design in cache.rs
            if !self.cache.has_types(&self.space_id) {
                prime_cache_types(&self.client, &self.cache, &self.space_id).await?;
            }
            return Ok(PagedResult::from_items(
                self.cache
                    .types_for_space(&self.space_id)
                    .unwrap_or_default(),
            ));
        }

        // cache disabled, or query has limits or filters that need to be evaluated on the server
        let query = Query::default()
            .set_limit_opt(self.limit)
            .set_offset_opt(self.offset)
            .add_filters(&self.filters);

        self.client
            .get_request_paged(&format!("/v1/spaces/{}/types", self.space_id), query)
            .await
    }
}

/// Load all space types into cache.
async fn prime_cache_types(
    client: &Arc<HttpClient>,
    cache: &Arc<AnytypeCache>,
    space_id: &str,
) -> Result<()> {
    let types: Vec<Type> = client
        .get_request_paged(
            &format!("/v1/spaces/{space_id}/types"),
            QueryWithFilters::default(),
        )
        .await?
        .collect_all()
        .await?
        .into_iter()
        .filter(|typ: &Type| !typ.archived)
        .collect();
    cache.set_types(space_id, types);
    Ok(())
}

// ============================================================================
// ANYTYPECLIENT METHODS
// ============================================================================

impl AnytypeClient {
    /// Creates a request builder for getting or deleting a single type by id.
    /// To get by key, use [`lookup_type_by_key`](AnytypeClient::lookup_type_by_key)
    ///
    /// # Arguments
    /// * `space_id` - ID of the space containing the type
    /// * `type_id` - ID of the type
    ///
    /// # Example
    ///
    /// ```rust
    /// # use anytype::prelude::*;
    /// # async fn example() -> Result<(), AnytypeError> {
    /// #   let client = AnytypeClient::new("doc test")?;
    /// #   let space_id = anytype::test_util::example_space_id(&client).await?;
    /// #   let typ = client.lookup_type_by_key(&space_id, "page").await?;
    /// #   let type_id = &typ.id;
    /// let typ = client.get_type(&space_id, type_id).get().await?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn get_type(&self, space_id: impl Into<String>, type_id: impl Into<String>) -> TypeRequest {
        TypeRequest::new(self.clone(), space_id, type_id)
    }

    /// Creates a request builder for creating a new type.
    /// - default plural name is name + 's'. Override with .`plural_name()`
    /// - default layout is Basic. Override with `.layout(`)
    ///
    /// # Arguments
    /// * `space_id` - ID of the space to create the type in
    /// * `name` - type name
    ///
    /// # Example
    ///
    /// ```rust
    /// # use anytype::prelude::*;
    /// # async fn example() -> Result<(), AnytypeError> {
    /// #   let client = AnytypeClient::new("doc test")?;
    /// #   let space_id = anytype::test_util::example_space_id(&client).await?;
    ///
    /// let project = client.new_type(&space_id, "My Project")
    ///     .key("my_project")
    ///     .create().await?;
    ///
    /// # client.get_type(&space_id, &project.id).delete().await?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn new_type(&self, space_id: impl Into<String>, name: impl Into<String>) -> NewTypeRequest {
        let name = name.into();
        let plural_name = format!("{name}s");
        NewTypeRequest::new(
            self.client.clone(),
            self.config.limits.clone(),
            space_id,
            name,
            plural_name,
            self.cache.clone(),
            self.config.verify.clone(),
        )
    }

    /// Creates a request builder for updating an existing type.
    ///
    /// # Arguments
    /// * `space_id` - ID of the space containing the type
    /// * `type_id` - ID of the type to update
    ///
    /// # Example
    ///
    /// ```rust
    /// # use anytype::prelude::*;
    /// # async fn example() -> Result<(), AnytypeError> {
    /// #   let client = AnytypeClient::new("doc test")?;
    /// #   let space_id = anytype::test_util::example_space_id(&client).await?;
    ///
    /// let project = client.new_type(&space_id, "My Project")
    ///     .key("my_project")
    ///     .create().await?;
    ///
    /// // Change the name and replace all non-featured recommended properties
    /// // with a single text field named "Location".
    /// let typ = client.update_type(&space_id, &project.id)
    ///     .name("My New Project")
    ///     .property("Location", "location", PropertyFormat::Text)
    ///     .update().await?;
    ///
    /// # client.get_type(&space_id, &project.id).delete().await?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn update_type(
        &self,
        space_id: impl Into<String>,
        type_id: impl Into<String>,
    ) -> UpdateTypeRequest {
        UpdateTypeRequest::new(
            self.client.clone(),
            self.config.limits.clone(),
            space_id,
            type_id,
            self.cache.clone(),
            self.config.verify.clone(),
        )
    }

    /// Creates a request builder for listing types in a space.
    ///
    /// # Arguments
    /// * `space_id` - ID of the space to list types from
    ///
    /// # Example
    ///
    /// ```rust
    /// use anytype::prelude::*;
    /// # async fn example() -> Result<(), AnytypeError> {
    /// #   let client = AnytypeClient::new("doc test")?;
    /// #   let space_id = anytype::test_util::example_space_id(&client).await?;
    ///
    /// let types = client.types(&space_id)
    ///     .limit(50)
    ///     .list().await?.collect_all().await?;
    /// for typ in types.iter() {
    ///     println!("{:20} {:20} {}", &typ.display_name(), &typ.key, &typ.id);
    /// }
    ///
    /// # Ok(())
    /// # }
    /// ```
    pub fn types(&self, space_id: impl Into<String>) -> ListTypesRequest {
        ListTypesRequest::new(
            self.client.clone(),
            self.config.limits.clone(),
            space_id,
            self.cache.clone(),
        )
    }

    /// Searches for type in space by id, key, or name using case-insensitive match
    /// Excludes archived types.
    ///
    /// # Example
    ///
    /// ```rust
    /// use anytype::prelude::*;
    /// # async fn example() -> Result<(), AnytypeError> {
    /// #   let client = AnytypeClient::new("doc test")?;
    /// #   let space_id = anytype::test_util::example_space_id(&client).await?;
    ///
    /// let types = client.lookup_types(&space_id, "page").await?;
    /// for typ in types.iter() {
    ///     println!("Type {}", &typ.display_name());
    /// }
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// Errors:
    /// - `AnytypeError::NotFound` if no type in the space matched
    /// - `AnytypeError::CacheDisabled` if cache is disabled
    /// - `AnytypeError::*` any other error (likely server connection error)
    pub async fn lookup_types(&self, space_id: &str, text: impl AsRef<str>) -> Result<Vec<Type>> {
        ensure!(self.cache.is_enabled(), CacheDisabledSnafu);
        // see note on locking design in cache.rs
        if !self.cache.has_types(space_id) {
            prime_cache_types(&self.client, &self.cache, space_id).await?;
        }
        match self.cache.lookup_types(space_id, text.as_ref()) {
            Some(types) if !types.is_empty() => {
                Ok(types.into_iter().map(|arc| (*arc).clone()).collect())
            }
            _ => NotFoundSnafu {
                obj_type: "Type".to_string(),
                key: text.as_ref().to_string(),
            }
            .fail(),
        }
    }

    /// Searches for type in space by key.
    /// Excludes archived types.
    ///
    /// # Example
    ///
    /// ```rust
    /// # use anytype::prelude::*;
    /// # async fn example() -> Result<(), AnytypeError> {
    /// #   let client = AnytypeClient::new("doc test")?;
    /// #   let space_id = anytype::test_util::example_space_id(&client).await?;
    ///
    /// let typ = client.lookup_type_by_key(&space_id, "page").await?;
    /// println!("Type {} key:{} id:{}", &typ.display_name(), &typ.key, &typ.id);
    ///
    /// # Ok(())
    /// # }
    /// ```
    /// Errors:
    /// - `AnytypeError::NotFound` if no type in the space matched
    /// - `AnytypeError::CacheDisabled` if cache is disabled
    /// - `AnytypeError::*` any other error (likely server connection error)
    ///
    pub async fn lookup_type_by_key(&self, space_id: &str, text: impl AsRef<str>) -> Result<Type> {
        ensure!(self.cache.is_enabled(), CacheDisabledSnafu);
        // see note on locking design in cache.rs
        if !self.cache.has_types(space_id) {
            prime_cache_types(&self.client, &self.cache, space_id).await?;
        }
        self.cache
            .lookup_type_by_key(space_id, text.as_ref())
            .map_or_else(
                || {
                    NotFoundSnafu {
                        obj_type: "Type".to_string(),
                        key: text.as_ref().to_string(),
                    }
                    .fail()
                },
                |typ| Ok((*typ).clone()),
            )
    }
}

// ============================================================================
// TESTS
// ============================================================================

#[cfg(test)]
mod tests {
    use std::{
        future::pending,
        sync::{
            Arc, Mutex,
            atomic::{AtomicUsize, Ordering},
        },
        time::Duration,
    };

    use tokio::sync::Notify;

    use super::*;

    fn valid_id(suffix: char) -> String {
        format!("bafyrei{}{}", "a".repeat(51), suffix)
    }

    fn property(id: &str, key: &str) -> Property {
        serde_json::from_value(serde_json::json!({
            "object": "property",
            "name": key,
            "key": key,
            "id": id,
            "format": "text"
        }))
        .expect("property fixture")
    }

    #[test]
    fn type_schema_preserves_discriminator() {
        let response: TypeResponse = serde_json::from_value(serde_json::json!({
            "type": {
                "object": "type",
                "archived": false,
                "id": "type-id",
                "key": "page",
                "layout": "basic",
                "name": "Page",
                "plural_name": "Pages",
                "properties": []
            }
        }))
        .expect("type response schema");

        assert_eq!(response.type_.object, DataModel::Type);
        let serialized = serde_json::to_value(response.type_).expect("serialize type");
        assert_eq!(serialized["object"], "type");
    }

    #[test]
    fn type_discriminator_defaults_when_omitted_and_preserves_present_value() {
        let type_without_discriminator: Type = serde_json::from_value(serde_json::json!({
            "archived": false,
            "id": "type-id",
            "key": "page"
        }))
        .expect("type without discriminator");
        assert_eq!(type_without_discriminator.object, DataModel::Type);

        let type_with_observed_member: Type = serde_json::from_value(serde_json::json!({
            "object": "member",
            "archived": false,
            "id": "type-id",
            "key": "page"
        }))
        .expect("type with observed discriminator");
        assert_eq!(type_with_observed_member.object, DataModel::Member);
    }

    fn string_list(ids: &[String]) -> prost_types::Value {
        prost_types::Value {
            kind: Some(Kind::ListValue(prost_types::ListValue {
                values: ids
                    .iter()
                    .map(|id| prost_types::Value {
                        kind: Some(Kind::StringValue(id.clone())),
                    })
                    .collect(),
            })),
        }
    }

    fn update_property(name: &str, key: &str) -> CreateTypeProperty {
        CreateTypeProperty {
            name: name.to_string(),
            key: key.to_string(),
            format: PropertyFormat::Text,
        }
    }

    #[test]
    fn type_property_cleanup_requires_an_owning_runtime() {
        let error = classification_runtime_handle().expect_err("missing Tokio runtime");
        assert!(matches!(
            error,
            AnytypeError::TypePropertyClassification {
                kind: TypePropertyClassificationErrorKind::RuntimeUnavailable
            }
        ));
    }

    #[tokio::test]
    async fn type_property_deadline_is_validated_before_transport() {
        let client = AnytypeClient::with_config(crate::client::ClientConfig {
            base_url: Some("http://127.0.0.1:1".to_owned()),
            keystore: Some(crate::test_util::test_keystore_spec()),
            disable_cache: true,
            ..crate::client::ClientConfig::default()
        })
        .expect("deadline test client");
        for deadline in [
            Duration::ZERO,
            MAX_TYPE_PROPERTY_RPC_TIMEOUT + Duration::from_nanos(1),
        ] {
            let error = client
                .get_type(valid_id('b'), valid_id('c'))
                .classify_properties_with_deadline(deadline)
                .await
                .expect_err("invalid deadline");
            assert!(matches!(error, AnytypeError::Validation { .. }));
        }
        assert_eq!(client.http_metrics().logical_operations, 0);
    }

    fn successful_cleanup_action(
        calls: Arc<AtomicUsize>,
        cleaned: Arc<Notify>,
    ) -> TypePropertyCleanupAction {
        Arc::new(move |_| {
            let calls = Arc::clone(&calls);
            let cleaned = Arc::clone(&cleaned);
            Box::pin(async move {
                calls.fetch_add(1, Ordering::SeqCst);
                cleaned.notify_one();
                Ok(())
            })
        })
    }

    #[tokio::test]
    async fn type_property_guard_closes_when_show_boundary_is_cancelled() {
        let armed = Arc::new(Notify::new());
        let cleaned = Arc::new(Notify::new());
        let calls = Arc::new(AtomicUsize::new(0));
        let action = successful_cleanup_action(Arc::clone(&calls), Arc::clone(&cleaned));
        let task = tokio::spawn({
            let armed = Arc::clone(&armed);
            async move {
                let _guard = TypePropertyCloseGuard::from_action(action);
                armed.notify_one();
                pending::<()>().await;
            }
        });
        armed.notified().await;
        task.abort();
        let _ = task.await;
        tokio::time::timeout(Duration::from_secs(1), cleaned.notified())
            .await
            .expect("cancelled show guard cleanup");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn type_property_guard_uses_one_fallback_when_close_is_cancelled() {
        let started = Arc::new(Notify::new());
        let recovered = Arc::new(Notify::new());
        let calls = Arc::new(AtomicUsize::new(0));
        let action: TypePropertyCleanupAction = Arc::new({
            let calls = Arc::clone(&calls);
            let started = Arc::clone(&started);
            let recovered = Arc::clone(&recovered);
            move |_| {
                let attempt = calls.fetch_add(1, Ordering::SeqCst);
                let started = Arc::clone(&started);
                let recovered = Arc::clone(&recovered);
                Box::pin(async move {
                    if attempt == 0 {
                        started.notify_one();
                        pending::<()>().await;
                    }
                    recovered.notify_one();
                    Ok(())
                })
            }
        });
        let task = tokio::spawn(async move {
            let mut guard = TypePropertyCloseGuard::from_action(action);
            let _ = guard.cleanup(Duration::from_secs(1)).await;
        });
        started.notified().await;
        task.abort();
        let _ = task.await;
        tokio::time::timeout(Duration::from_secs(1), recovered.notified())
            .await
            .expect("cancelled close guard fallback");
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn confirmed_type_property_close_disarms_guard() {
        let cleaned = Arc::new(Notify::new());
        let calls = Arc::new(AtomicUsize::new(0));
        let action = successful_cleanup_action(Arc::clone(&calls), Arc::clone(&cleaned));
        {
            let mut guard = TypePropertyCloseGuard::from_action(action);
            guard
                .cleanup(Duration::from_secs(1))
                .await
                .expect("explicit close");
        }
        cleaned.notified().await;
        tokio::task::yield_now().await;
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn type_property_close_failure_is_typed_and_payload_free() {
        let calls = Arc::new(AtomicUsize::new(0));
        let durations = Arc::new(Mutex::new(Vec::new()));
        let action: TypePropertyCleanupAction = Arc::new({
            let calls = Arc::clone(&calls);
            let durations = Arc::clone(&durations);
            move |duration| {
                let calls = Arc::clone(&calls);
                durations.lock().expect("duration lock").push(duration);
                Box::pin(async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Err(AnytypeError::Other {
                        message: "secret upstream payload".to_owned(),
                    })
                })
            }
        });
        let metrics = Arc::new(TypePropertyClassificationMetrics::default());
        let mut guard =
            TypePropertyCloseGuard::from_action_with_metrics(action, Arc::clone(&metrics));
        let error = guard
            .cleanup(Duration::from_millis(1))
            .await
            .expect_err("cleanup failure");
        assert!(matches!(
            error,
            AnytypeError::TypePropertyClassification {
                kind: TypePropertyClassificationErrorKind::CleanupFailed
            }
        ));
        assert!(!format!("{error:?}").contains("secret upstream payload"));
        drop(guard);
        tokio::time::timeout(Duration::from_secs(1), async {
            while metrics.snapshot().close_attempts < 2 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("fallback exhaustion metrics");
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert_eq!(
            metrics.snapshot(),
            TypePropertyClassificationMetricsSnapshot {
                show_attempts: 0,
                close_attempts: 2,
                close_fallbacks: 1,
                cleanup_successes: 0,
                cleanup_failures: 2,
            }
        );
        assert_eq!(
            *durations.lock().expect("duration lock"),
            vec![Duration::from_millis(1), MAX_TYPE_PROPERTY_RPC_TIMEOUT]
        );
    }

    #[test]
    fn type_property_cleanup_failure_precedes_show_application_error() {
        let response = Err(AnytypeError::Other {
            message: "show application payload".to_owned(),
        });
        let cleanup = Err(classification_error_value(
            TypePropertyClassificationErrorKind::CleanupFailed,
        ));
        let error = finish_type_property_show(response, cleanup).expect_err("cleanup precedence");
        assert!(matches!(
            error,
            AnytypeError::TypePropertyClassification {
                kind: TypePropertyClassificationErrorKind::CleanupFailed
            }
        ));
        assert!(!format!("{error:?}").contains("show application payload"));
    }

    #[test]
    fn update_type_omits_unchanged_properties() {
        let body = UpdateTypeRequestBody::default();
        assert_eq!(serde_json::to_value(body).unwrap(), serde_json::json!({}));
    }

    #[test]
    fn update_type_serializes_property_replacement() {
        let body = UpdateTypeRequestBody {
            properties: Some(vec![update_property("Location", "location")]),
            ..UpdateTypeRequestBody::default()
        };

        assert_eq!(
            serde_json::to_value(body).unwrap(),
            serde_json::json!({
                "properties": [{
                    "format": "text",
                    "key": "location",
                    "name": "Location"
                }]
            })
        );
    }

    #[test]
    fn update_type_serializes_explicit_property_clear() {
        let body = UpdateTypeRequestBody {
            properties: Some(Vec::new()),
            ..UpdateTypeRequestBody::default()
        };
        assert_eq!(
            serde_json::to_value(body).unwrap(),
            serde_json::json!({ "properties": [] })
        );
    }

    #[test]
    fn type_property_classification_preserves_source_order_and_hidden_featured_ids() {
        let hidden_featured_id = valid_id('b');
        let visible_featured_id = valid_id('c');
        let first_id = valid_id('d');
        let second_id = valid_id('e');

        let classified = classify_type_properties(
            vec![
                property(&visible_featured_id, "tag"),
                property(&first_id, "first"),
                property(&second_id, "second"),
            ],
            vec![hidden_featured_id.clone(), visible_featured_id.clone()],
            vec![first_id.clone(), second_id.clone()],
        )
        .expect("classification");

        assert_eq!(
            classified.featured_ids,
            vec![hidden_featured_id, visible_featured_id]
        );
        assert_eq!(
            classified
                .featured
                .iter()
                .map(|property| property.id.as_str())
                .collect::<Vec<_>>(),
            vec![classified.featured_ids[1].as_str()]
        );
        assert_eq!(
            classified
                .replaceable()
                .iter()
                .map(|property| property.id.as_str())
                .collect::<Vec<_>>(),
            vec![first_id.as_str(), second_id.as_str()]
        );
    }

    #[test]
    fn type_property_classification_rejects_incomplete_or_ambiguous_evidence() {
        let featured_id = valid_id('b');
        let recommended_id = valid_id('c');

        assert!(
            classify_type_properties(
                vec![property(&featured_id, "tag")],
                vec![featured_id.clone()],
                vec![recommended_id],
            )
            .is_err()
        );
        assert!(
            classify_type_properties(
                vec![property(&featured_id, "tag")],
                vec![featured_id.clone()],
                vec![featured_id],
            )
            .is_err()
        );
    }

    #[test]
    fn type_property_source_view_reads_separate_lists() {
        let type_id = valid_id('b');
        let featured_id = valid_id('c');
        let recommended_id = valid_id('d');
        let details = prost_types::Struct {
            fields: [
                (
                    RECOMMENDED_FEATURED_RELATIONS.to_owned(),
                    string_list(std::slice::from_ref(&featured_id)),
                ),
                (
                    RECOMMENDED_RELATIONS.to_owned(),
                    string_list(std::slice::from_ref(&recommended_id)),
                ),
            ]
            .into_iter()
            .collect(),
        };
        let view = model::ObjectView {
            details: vec![model::object_view::DetailsSet {
                id: type_id.clone(),
                details: Some(details),
                sub_ids: Vec::new(),
            }],
            ..Default::default()
        };

        let ids = type_property_source_ids_from_view(&view, &ValidationLimits::default(), &type_id)
            .expect("source IDs");
        assert_eq!(ids, (vec![featured_id], vec![recommended_id]));
    }

    #[test]
    fn type_property_source_view_rejects_malformed_and_oversized_lists() {
        let type_id = valid_id('b');
        let mut fields = std::collections::BTreeMap::new();
        fields.insert(
            RECOMMENDED_RELATIONS.to_owned(),
            prost_types::Value {
                kind: Some(Kind::StringValue("not-a-list".to_owned())),
            },
        );
        let malformed = model::ObjectView {
            details: vec![model::object_view::DetailsSet {
                id: type_id.clone(),
                details: Some(prost_types::Struct { fields }),
                sub_ids: Vec::new(),
            }],
            ..Default::default()
        };
        assert!(
            type_property_source_ids_from_view(&malformed, &ValidationLimits::default(), &type_id,)
                .is_err()
        );

        let ids = (0..=MAX_TYPE_PROPERTY_LINKS)
            .map(|index| valid_id(char::from(b'b' + u8::try_from(index % 25).unwrap())))
            .collect::<Vec<_>>();
        let oversized = model::ObjectView {
            details: vec![model::object_view::DetailsSet {
                id: type_id.clone(),
                details: Some(prost_types::Struct {
                    fields: [(RECOMMENDED_RELATIONS.to_owned(), string_list(&ids))]
                        .into_iter()
                        .collect(),
                }),
                sub_ids: Vec::new(),
            }],
            ..Default::default()
        };
        assert!(
            type_property_source_ids_from_view(&oversized, &ValidationLimits::default(), &type_id,)
                .is_err()
        );
    }

    #[test]
    fn test_type_layout_default() {
        let layout = TypeLayout::default();
        assert_eq!(layout, TypeLayout::Basic);
    }

    #[test]
    fn test_type_layout_display() {
        assert_eq!(TypeLayout::Basic.to_string(), "basic");
        assert_eq!(TypeLayout::Note.to_string(), "note");
        assert_eq!(TypeLayout::Action.to_string(), "action");
    }

    #[test]
    fn test_type_layout_from_string() {
        use std::str::FromStr;
        assert_eq!(TypeLayout::from_str("basic").unwrap(), TypeLayout::Basic);
        assert_eq!(TypeLayout::from_str("note").unwrap(), TypeLayout::Note);
    }

    #[test]
    fn test_type_is_system_type() {
        let page_type = Type {
            object: DataModel::Type,
            archived: false,
            id: "id".to_string(),
            key: "page".to_string(),
            name: Some("Page".to_string()),
            plural_name: None,
            icon: None,
            layout: ObjectLayout::Basic,
            properties: vec![],
        };
        assert!(page_type.is_system_type());

        let custom_type = Type {
            object: DataModel::Type,
            archived: false,
            id: "id".to_string(),
            key: "project".to_string(),
            name: Some("Project".to_string()),
            plural_name: None,
            icon: None,
            layout: ObjectLayout::Basic,
            properties: vec![],
        };
        assert!(!custom_type.is_system_type());
    }

    #[test]
    fn test_type_display_name() {
        let with_name = Type {
            object: DataModel::Type,
            archived: false,
            id: "id".to_string(),
            key: "page".to_string(),
            name: Some("Page".to_string()),
            plural_name: None,
            icon: None,
            layout: ObjectLayout::Basic,
            properties: vec![],
        };
        assert_eq!(with_name.display_name(), "Page");

        let without_name = Type {
            object: DataModel::Type,
            archived: false,
            id: "id".to_string(),
            key: "custom_type".to_string(),
            name: None,
            plural_name: None,
            icon: None,
            layout: ObjectLayout::Basic,
            properties: vec![],
        };
        assert_eq!(without_name.display_name(), "custom_type");
    }
}
