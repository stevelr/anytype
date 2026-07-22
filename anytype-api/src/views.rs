//! # Anytype Views (for Collections and Queries)
//!
//! This module provides a fluent builder API for working with (collections and queries).
//!
//! - [`list_views`](AnytypeClient::list_views) - list views (for collections and queries)
//! - [`view_list_objects`](AnytypeClient::view_list_objects) - list objects in a collection or query
//! - [`view_remove_object`](AnytypeClient::view_remove_object) - remove an object from a view (collection)
//! - [`view_add_objects`](AnytypeClient::view_add_objects) - add objects to a collection
//! - [`observe_collection_membership`](AnytypeClient::observe_collection_membership) - prove exact
//!   direct collection membership independently of saved view filters
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

use std::{fmt::Write as _, future::Future, pin::Pin, sync::Arc, time::Duration};

use anytype_rpc::{
    anytype::rpc::object::{search_subscribe, search_unsubscribe},
    client::AnytypeGrpcClient,
    model::block::content::dataview::{
        Filter as RpcFilter,
        filter::{Condition as RpcCondition, Operator as RpcOperator, QuickOption},
    },
};
use prost_types::{Struct, Value, value::Kind};
use serde::{Deserialize, Deserializer, Serialize};
use tonic::Request;

use crate::{
    Result,
    client::AnytypeClient,
    error::AnytypeError,
    filters::{Query, QueryWithFilters},
    grpc_util::{ensure_error_ok, grpc_status, with_token_request},
    http_client::{GetPaged, HttpClient},
    prelude::*,
};

const MAX_VIEW_ID_CHARS: usize = 256;
const MEMBERSHIP_QUERY_LIMIT: i64 = 2;
const MAX_MEMBERSHIP_SUBSCRIPTION_ID_BYTES: usize = 128;
const MEMBERSHIP_RPC_TIMEOUT: Duration = Duration::from_secs(5);
const MEMBERSHIP_CLEANUP_TIMEOUT: Duration = Duration::from_secs(5);
const MEMBERSHIP_SUBSCRIPTION_PREFIX: &str = "anytype-api-membership-";
const ID_KEY: &str = "id";
const SPACE_ID_KEY: &str = "spaceId";
const ARCHIVED_KEY: &str = "isArchived";
const DELETED_KEY: &str = "isDeleted";
const RESOLVED_LAYOUT_KEY: &str = "resolvedLayout";

/// Exact direct-membership state established by a complete bounded read.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CollectionMembershipState {
    /// The exact object is stored in the collection.
    Present,
    /// The exact object is not stored in the collection.
    Absent,
}

/// Identity-bound result of a direct collection-membership observation.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct CollectionMembershipObservation {
    /// Exact space whose index and objects were checked.
    pub space_id: String,
    /// Exact collection whose canonical membership scope was checked.
    pub collection_id: String,
    /// Exact object checked for membership.
    pub object_id: String,
    /// Complete direct-membership outcome.
    pub state: CollectionMembershipState,
}

/// Closed classification for incomplete collection-membership evidence.
#[derive(Clone, Copy, Debug, PartialEq, Eq, strum::Display)]
#[strum(serialize_all = "snake_case")]
pub enum CollectionMembershipEvidenceKind {
    /// The scoped collection GET returned a different identity.
    CollectionIdentityMismatch,
    /// The list object is a query/Set rather than a direct collection.
    NotACollection,
    /// The target object GET returned a different identity.
    ObjectIdentityMismatch,
    /// Heart returned no bounded subscription identifier.
    MissingSubscriptionId,
    /// Heart did not echo the exact client-owned subscription identifier.
    SubscriptionIdMismatch,
    /// Heart omitted or contradicted the finite result counters.
    InvalidCounters,
    /// Heart returned malformed, duplicate, or mismatched records.
    InvalidRecords,
    /// The independent unscoped query could not prove the target index row.
    IncompleteControl,
    /// A finite membership RPC deadline expired.
    RpcDeadline,
    /// The app-global temporary subscription could not be released cleanly.
    CleanupFailed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MembershipQueryState {
    Present,
    Absent,
}

type MembershipCleanupFuture = Pin<Box<dyn Future<Output = Result<()>> + Send>>;
type MembershipCleanupAction = Arc<dyn Fn() -> MembershipCleanupFuture + Send + Sync>;

/// Owns cleanup of one app-global Heart subscription.
///
/// The guard is armed before the subscribe future is polled. Explicit cleanup
/// disarms it only after a confirmed unsubscribe; cancellation or timeout at
/// either RPC boundary drops the guard and starts one detached, bounded retry.
struct MembershipSubscriptionGuard {
    action: MembershipCleanupAction,
    armed: bool,
}

impl MembershipSubscriptionGuard {
    fn new(grpc: AnytypeGrpcClient, subscription_id: String) -> Self {
        let action: MembershipCleanupAction = Arc::new(move || {
            let grpc = grpc.clone();
            let subscription_id = subscription_id.clone();
            Box::pin(async move { unsubscribe_membership(grpc, subscription_id).await })
        });
        Self {
            action,
            armed: true,
        }
    }

    #[cfg(test)]
    fn from_action(action: MembershipCleanupAction) -> Self {
        Self {
            action,
            armed: true,
        }
    }

    async fn cleanup(&mut self) -> Result<()> {
        let result = (self.action)().await;
        if result.is_ok() {
            self.armed = false;
        }
        result
    }
}

impl Drop for MembershipSubscriptionGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let action = Arc::clone(&self.action);
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                let _ = action().await;
            });
        }
    }
}

/// View layout for list types
///
/// As of anytype-heart 0.50.15 the `2025-11-08` spec enumerates all six
/// layouts (`grid`, `list`, `gallery`, `kanban`, `calendar`, `graph`); earlier
/// spec revisions only documented `grid` and `table`.
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
    ///
    /// The identifier is validated when [`list`](Self::list) executes, before
    /// it can be interpolated into an HTTP path.
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

        let view_id = self.view_id.ok_or_else(|| AnytypeError::Validation {
            message: "You must set the view with `.view(view_id)` before .list()".to_string(),
        })?;
        validate_view_id(&view_id)?;

        let query = Query::default()
            .set_limit_opt(self.limit)
            .set_offset_opt(self.offset)
            .add_filters(&self.filters);

        let path = format!(
            "/v1/spaces/{}/lists/{}/views/{view_id}/objects",
            self.space_id, self.list_id
        );

        self.client.get_request_paged(&path, query).await
    }
}

fn validate_view_id(view_id: &str) -> Result<()> {
    if view_id.is_empty()
        || matches!(view_id, "." | "..")
        || view_id.chars().count() > MAX_VIEW_ID_CHARS
        || !view_id
            .bytes()
            .all(|character| character.is_ascii_alphanumeric() || b"._~-".contains(&character))
    {
        return Err(AnytypeError::Validation {
            message: "view_id must be a nonempty safe path identifier".to_owned(),
        });
    }
    Ok(())
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

    /// Observes whether one exact object is a direct member of one exact
    /// collection, independently of every saved view and caller filter.
    ///
    /// The read first binds the collection and object to `space_id` through
    /// exact REST GETs and rejects Set/query objects. It then runs a finite
    /// unscoped exact-ID control followed by the same exact-ID query scoped to
    /// the collection's canonical membership slice. An absent scoped result
    /// requires a second successful unscoped control, preventing a transient
    /// missing index row from manufacturing absence. Every app-global Heart
    /// subscription has a unique client-owned ID, a finite deadline, and
    /// cancellation-resilient bounded cleanup.
    ///
    /// # Errors
    ///
    /// Returns [`AnytypeError::CollectionMembershipEvidence`] rather than an
    /// absent result when identity, counters, records, or the independent
    /// control query are incomplete. Authentication and transport failures
    /// retain their ordinary classifications. A caller verifying a preceding
    /// mutation must treat every error as an indeterminate mutation outcome.
    pub async fn observe_collection_membership(
        &self,
        space_id: impl Into<String>,
        collection_id: impl Into<String>,
        object_id: impl Into<String>,
    ) -> Result<CollectionMembershipObservation> {
        let space_id = space_id.into();
        let collection_id = collection_id.into();
        let object_id = object_id.into();
        self.config.limits.validate_id(&space_id, "space_id")?;
        self.config
            .limits
            .validate_id(&collection_id, "collection_id")?;
        self.config.limits.validate_id(&object_id, "object_id")?;

        let collection = self.object(&space_id, &collection_id).get().await?;
        validate_collection_identity(&collection, &space_id, &collection_id)?;
        let object = self.object(&space_id, &object_id).get().await?;
        validate_object_identity(&object, &space_id, &object_id)?;

        let control = exact_membership_query(self, &space_id, None, &object_id).await?;
        require_complete_control(control)?;
        let scoped =
            exact_membership_query(self, &space_id, Some(&collection_id), &object_id).await?;
        let post_control = if scoped == MembershipQueryState::Absent {
            Some(exact_membership_query(self, &space_id, None, &object_id).await?)
        } else {
            None
        };
        let state = complete_membership_state(scoped, post_control)?;

        Ok(CollectionMembershipObservation {
            space_id,
            collection_id,
            object_id,
            state,
        })
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

fn validate_collection_identity(
    collection: &Object,
    space_id: &str,
    collection_id: &str,
) -> Result<()> {
    if collection.id != collection_id || collection.space_id != space_id {
        return membership_evidence(CollectionMembershipEvidenceKind::CollectionIdentityMismatch);
    }
    if collection.layout != ObjectLayout::Collection {
        return membership_evidence(CollectionMembershipEvidenceKind::NotACollection);
    }
    Ok(())
}

fn validate_object_identity(object: &Object, space_id: &str, object_id: &str) -> Result<()> {
    if object.id != object_id || object.space_id != space_id {
        return membership_evidence(CollectionMembershipEvidenceKind::ObjectIdentityMismatch);
    }
    Ok(())
}

fn require_complete_control(state: MembershipQueryState) -> Result<()> {
    if state == MembershipQueryState::Present {
        Ok(())
    } else {
        membership_evidence(CollectionMembershipEvidenceKind::IncompleteControl)
    }
}

fn complete_membership_state(
    scoped: MembershipQueryState,
    post_control: Option<MembershipQueryState>,
) -> Result<CollectionMembershipState> {
    match scoped {
        MembershipQueryState::Present => Ok(CollectionMembershipState::Present),
        MembershipQueryState::Absent => {
            let post_control = post_control.ok_or_else(|| {
                membership_evidence_error(CollectionMembershipEvidenceKind::IncompleteControl)
            })?;
            require_complete_control(post_control)?;
            Ok(CollectionMembershipState::Absent)
        }
    }
}

async fn exact_membership_query(
    client: &AnytypeClient,
    space_id: &str,
    collection_id: Option<&str>,
    object_id: &str,
) -> Result<MembershipQueryState> {
    let grpc = client.grpc_client().await?;
    let subscription_id = new_membership_subscription_id()?;
    let mut commands = grpc.client_commands();
    let request = membership_query_request(space_id, collection_id, object_id, &subscription_id);
    let mut request = with_token_request(Request::new(request), grpc.token())?;
    request.set_timeout(MEMBERSHIP_RPC_TIMEOUT);
    let mut cleanup = MembershipSubscriptionGuard::new(grpc, subscription_id.clone());
    let response = match tokio::time::timeout(
        MEMBERSHIP_RPC_TIMEOUT,
        commands.object_search_subscribe(request),
    )
    .await
    {
        Ok(Ok(response)) => response.into_inner(),
        Ok(Err(status)) => {
            let query_error = grpc_status(status);
            cleanup.cleanup().await?;
            return Err(query_error);
        }
        Err(_) => {
            cleanup.cleanup().await?;
            return membership_evidence(CollectionMembershipEvidenceKind::RpcDeadline);
        }
    };
    finish_membership_response(
        &response,
        &subscription_id,
        space_id,
        object_id,
        &mut cleanup,
    )
    .await
}

async fn finish_membership_response(
    response: &search_subscribe::Response,
    subscription_id: &str,
    space_id: &str,
    object_id: &str,
    cleanup: &mut MembershipSubscriptionGuard,
) -> Result<MembershipQueryState> {
    let response_error = ensure_error_ok(response.error.as_ref(), "collection membership query");
    let cleanup_result = cleanup.cleanup().await;
    response_error?;
    cleanup_result?;
    validate_echoed_subscription_id(response, subscription_id)?;
    decode_membership_query(response, subscription_id, space_id, object_id)
}

fn validate_echoed_subscription_id(
    response: &search_subscribe::Response,
    subscription_id: &str,
) -> Result<()> {
    if response.sub_id.is_empty() {
        return membership_evidence(CollectionMembershipEvidenceKind::MissingSubscriptionId);
    }
    if response.sub_id != subscription_id {
        return membership_evidence(CollectionMembershipEvidenceKind::SubscriptionIdMismatch);
    }
    Ok(())
}

async fn unsubscribe_membership(grpc: AnytypeGrpcClient, subscription_id: String) -> Result<()> {
    let mut commands = grpc.client_commands();
    let request = search_unsubscribe::Request {
        sub_ids: vec![subscription_id],
    };
    let mut request = with_token_request(Request::new(request), grpc.token())?;
    request.set_timeout(MEMBERSHIP_CLEANUP_TIMEOUT);
    let response = tokio::time::timeout(
        MEMBERSHIP_CLEANUP_TIMEOUT,
        commands.object_search_unsubscribe(request),
    )
    .await
    .map_err(|_| membership_evidence_error(CollectionMembershipEvidenceKind::CleanupFailed))?
    .map_err(grpc_status)?
    .into_inner();
    ensure_error_ok(
        response.error.as_ref(),
        "collection membership query cleanup",
    )
}

fn new_membership_subscription_id() -> Result<String> {
    let mut random = [0_u8; 16];
    getrandom::fill(&mut random).map_err(|_| AnytypeError::Other {
        message: "operating-system RNG failed for membership subscription".to_owned(),
    })?;
    let mut subscription_id = String::with_capacity(MEMBERSHIP_SUBSCRIPTION_PREFIX.len() + 32);
    subscription_id.push_str(MEMBERSHIP_SUBSCRIPTION_PREFIX);
    for byte in random {
        write!(&mut subscription_id, "{byte:02x}").map_err(|_| AnytypeError::Other {
            message: "membership subscription identifier formatting failed".to_owned(),
        })?;
    }
    if !valid_membership_subscription_id(&subscription_id) {
        return membership_evidence(CollectionMembershipEvidenceKind::MissingSubscriptionId);
    }
    Ok(subscription_id)
}

fn valid_membership_subscription_id(subscription_id: &str) -> bool {
    !subscription_id.is_empty()
        && subscription_id.len() <= MAX_MEMBERSHIP_SUBSCRIPTION_ID_BYTES
        && subscription_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
}

fn membership_query_request(
    space_id: &str,
    collection_id: Option<&str>,
    object_id: &str,
    subscription_id: &str,
) -> search_subscribe::Request {
    search_subscribe::Request {
        space_id: space_id.to_owned(),
        sub_id: subscription_id.to_owned(),
        filters: vec![
            exact_id_filter(object_id),
            default_filter_opt_out(ARCHIVED_KEY),
            default_filter_opt_out(DELETED_KEY),
            default_filter_opt_out(RESOLVED_LAYOUT_KEY),
        ],
        sorts: Vec::new(),
        limit: MEMBERSHIP_QUERY_LIMIT,
        offset: 0,
        keys: vec![ID_KEY.to_owned(), SPACE_ID_KEY.to_owned()],
        after_id: String::new(),
        before_id: String::new(),
        source: Vec::new(),
        no_dep_subscription: true,
        collection_id: collection_id.unwrap_or_default().to_owned(),
    }
}

fn exact_id_filter(object_id: &str) -> RpcFilter {
    RpcFilter {
        relation_key: ID_KEY.to_owned(),
        condition: RpcCondition::Equal as i32,
        value: Some(Value {
            kind: Some(Kind::StringValue(object_id.to_owned())),
        }),
        ..empty_rpc_filter()
    }
}

fn default_filter_opt_out(relation_key: &str) -> RpcFilter {
    RpcFilter {
        relation_key: relation_key.to_owned(),
        condition: RpcCondition::None as i32,
        ..empty_rpc_filter()
    }
}

fn empty_rpc_filter() -> RpcFilter {
    RpcFilter {
        id: String::new(),
        operator: RpcOperator::No as i32,
        relation_key: String::new(),
        relation_property: String::new(),
        condition: RpcCondition::None as i32,
        value: None,
        quick_option: QuickOption::ExactDate as i32,
        format: 0,
        include_time: false,
        nested_filters: Vec::new(),
    }
}

fn decode_membership_query(
    response: &search_subscribe::Response,
    subscription_id: &str,
    space_id: &str,
    object_id: &str,
) -> Result<MembershipQueryState> {
    let counters = response.counters.as_ref().ok_or_else(|| {
        membership_evidence_error(CollectionMembershipEvidenceKind::InvalidCounters)
    })?;
    if response.sub_id != subscription_id
        || counters.sub_id != subscription_id
        || counters.total < 0
        || counters.total > 1
        || counters.next_count != 0
        || counters.prev_count != 0
        || usize::try_from(counters.total).ok() != Some(response.records.len())
    {
        return membership_evidence(CollectionMembershipEvidenceKind::InvalidCounters);
    }
    if !response.dependencies.is_empty() {
        return membership_evidence(CollectionMembershipEvidenceKind::InvalidRecords);
    }
    match response.records.as_slice() {
        [] => Ok(MembershipQueryState::Absent),
        [record]
            if struct_string(record, ID_KEY) == Some(object_id)
                && struct_string(record, SPACE_ID_KEY) == Some(space_id) =>
        {
            Ok(MembershipQueryState::Present)
        }
        _ => membership_evidence(CollectionMembershipEvidenceKind::InvalidRecords),
    }
}

fn struct_string<'a>(details: &'a Struct, key: &str) -> Option<&'a str> {
    match details.fields.get(key)?.kind.as_ref()? {
        Kind::StringValue(value) => Some(value),
        _ => None,
    }
}

fn membership_evidence<T>(kind: CollectionMembershipEvidenceKind) -> Result<T> {
    Err(membership_evidence_error(kind))
}

fn membership_evidence_error(kind: CollectionMembershipEvidenceKind) -> AnytypeError {
    AnytypeError::CollectionMembershipEvidence { kind }
}

// ============================================================================
// TESTS
// ============================================================================

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeMap,
        future::pending,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
    };

    use anytype_rpc::anytype::event::object::subscription::Counters;
    use tokio::sync::Notify;

    use super::*;

    const SPACE_ID: &str = "bafyreiaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const LIST_ID: &str = "bafyreicccccccccccccccccccccccccccccccccccccccccccccccccccc";
    const OBJECT_ID: &str = "bafyreiooooooooooooooooooooooooooooooooooooooooooooooooooo";
    const SUBSCRIPTION_ID: &str = "anytype-api-membership-0123456789abcdef0123456789abcdef";

    fn fixture_client() -> AnytypeClient {
        let mut config = crate::client::ClientConfig::default().app_name("view-id-validation-test");
        config.base_url = Some("http://127.0.0.1:1".to_owned());
        config.keystore = Some("env".to_owned());
        let client = AnytypeClient::with_config(config).expect("fixture client");
        client.set_api_key(crate::keystore::HttpCredentials::new("fixture-token"));
        client
    }

    #[test]
    fn view_id_path_validation_accepts_only_bounded_safe_segments() {
        for valid in ["view-1", "view_2", "view.3", "view~4"] {
            assert!(validate_view_id(valid).is_ok());
        }
        for invalid in ["", ".", "..", "../secret", "view/name", "view?token=x"] {
            assert!(matches!(
                validate_view_id(invalid),
                Err(AnytypeError::Validation { .. })
            ));
        }
        assert!(matches!(
            validate_view_id(&"x".repeat(MAX_VIEW_ID_CHARS + 1)),
            Err(AnytypeError::Validation { .. })
        ));
    }

    #[tokio::test]
    async fn unsafe_selected_view_is_rejected_before_an_http_request() {
        let error = fixture_client()
            .view_list_objects(SPACE_ID, LIST_ID)
            .view("../private?token=secret")
            .list()
            .await
            .expect_err("unsafe view ID must fail before connecting");

        let AnytypeError::Validation { message } = error else {
            panic!("unsafe view ID should be classified as validation");
        };
        assert_eq!(message, "view_id must be a nonempty safe path identifier");
        assert!(!message.contains("secret"));
    }

    #[test]
    fn membership_query_is_exact_bounded_and_view_independent() {
        let scoped = membership_query_request(SPACE_ID, Some(LIST_ID), OBJECT_ID, SUBSCRIPTION_ID);
        assert_eq!(scoped.space_id, SPACE_ID);
        assert_eq!(scoped.sub_id, SUBSCRIPTION_ID);
        assert_eq!(scoped.collection_id, LIST_ID);
        assert_eq!(scoped.limit, MEMBERSHIP_QUERY_LIMIT);
        assert_eq!(scoped.offset, 0);
        assert_eq!(scoped.keys, [ID_KEY, SPACE_ID_KEY]);
        assert!(scoped.sorts.is_empty());
        assert!(scoped.source.is_empty());
        assert!(scoped.no_dep_subscription);
        assert_eq!(scoped.filters.len(), 4);

        let id_filter = &scoped.filters[0];
        assert_eq!(id_filter.relation_key, ID_KEY);
        assert_eq!(id_filter.condition, RpcCondition::Equal as i32);
        assert_eq!(
            id_filter
                .value
                .as_ref()
                .and_then(|value| value.kind.as_ref()),
            Some(&Kind::StringValue(OBJECT_ID.to_owned()))
        );
        for (filter, key) in
            scoped.filters[1..]
                .iter()
                .zip([ARCHIVED_KEY, DELETED_KEY, RESOLVED_LAYOUT_KEY])
        {
            assert_eq!(filter.relation_key, key);
            assert_eq!(filter.condition, RpcCondition::None as i32);
            assert!(filter.value.is_none());
        }

        let control = membership_query_request(SPACE_ID, None, OBJECT_ID, SUBSCRIPTION_ID);
        assert!(control.collection_id.is_empty());
        assert_eq!(control.filters, scoped.filters);

        assert!(valid_membership_subscription_id("0123456789abcdef01234567"));
        assert!(!valid_membership_subscription_id(""));
        assert!(!valid_membership_subscription_id("unsafe/subscription"));
        assert!(!valid_membership_subscription_id(
            &"x".repeat(MAX_MEMBERSHIP_SUBSCRIPTION_ID_BYTES + 1)
        ));

        let generated = new_membership_subscription_id().expect("client-owned subscription ID");
        assert!(generated.starts_with(MEMBERSHIP_SUBSCRIPTION_PREFIX));
        assert!(valid_membership_subscription_id(&generated));
        assert_ne!(
            generated,
            new_membership_subscription_id().expect("second client-owned subscription ID")
        );
        assert!(!MEMBERSHIP_RPC_TIMEOUT.is_zero());
        assert!(!MEMBERSHIP_CLEANUP_TIMEOUT.is_zero());
    }

    #[test]
    fn membership_identity_rejects_wrong_scope_and_queries() {
        let collection = object_fixture(LIST_ID, SPACE_ID, ObjectLayout::Collection);
        assert!(validate_collection_identity(&collection, SPACE_ID, LIST_ID).is_ok());

        let query = object_fixture(LIST_ID, SPACE_ID, ObjectLayout::Set);
        assert_evidence_kind(
            validate_collection_identity(&query, SPACE_ID, LIST_ID),
            CollectionMembershipEvidenceKind::NotACollection,
        );
        let wrong_space = object_fixture(LIST_ID, OBJECT_ID, ObjectLayout::Collection);
        assert_evidence_kind(
            validate_collection_identity(&wrong_space, SPACE_ID, LIST_ID),
            CollectionMembershipEvidenceKind::CollectionIdentityMismatch,
        );
        let wrong_object = object_fixture(LIST_ID, SPACE_ID, ObjectLayout::Basic);
        assert_evidence_kind(
            validate_object_identity(&wrong_object, SPACE_ID, OBJECT_ID),
            CollectionMembershipEvidenceKind::ObjectIdentityMismatch,
        );
    }

    #[test]
    fn membership_decoder_accepts_only_complete_exact_evidence() {
        let absent = membership_response(Vec::new(), 0);
        assert_eq!(
            decode_membership_query(&absent, SUBSCRIPTION_ID, SPACE_ID, OBJECT_ID)
                .expect("complete absence"),
            MembershipQueryState::Absent
        );
        assert_evidence_kind(
            require_complete_control(MembershipQueryState::Absent),
            CollectionMembershipEvidenceKind::IncompleteControl,
        );

        let present = membership_response(vec![record(OBJECT_ID, SPACE_ID)], 1);
        assert_eq!(
            decode_membership_query(&present, SUBSCRIPTION_ID, SPACE_ID, OBJECT_ID)
                .expect("complete presence"),
            MembershipQueryState::Present
        );
        assert!(require_complete_control(MembershipQueryState::Present).is_ok());
        assert_eq!(
            complete_membership_state(MembershipQueryState::Present, None)
                .expect("presence needs no post-control"),
            CollectionMembershipState::Present
        );
        assert_eq!(
            complete_membership_state(
                MembershipQueryState::Absent,
                Some(MembershipQueryState::Present)
            )
            .expect("stable controlled absence"),
            CollectionMembershipState::Absent
        );
        for post_control in [None, Some(MembershipQueryState::Absent)] {
            assert_evidence_kind(
                complete_membership_state(MembershipQueryState::Absent, post_control),
                CollectionMembershipEvidenceKind::IncompleteControl,
            );
        }

        let malformed = [
            membership_response(Vec::new(), 1),
            membership_response(vec![record(OBJECT_ID, SPACE_ID)], 0),
            membership_response(vec![record(LIST_ID, SPACE_ID)], 1),
            membership_response(vec![record(OBJECT_ID, LIST_ID)], 1),
            membership_response(
                vec![record(OBJECT_ID, SPACE_ID), record(OBJECT_ID, SPACE_ID)],
                1,
            ),
        ];
        for response in malformed {
            assert!(matches!(
                decode_membership_query(&response, SUBSCRIPTION_ID, SPACE_ID, OBJECT_ID),
                Err(AnytypeError::CollectionMembershipEvidence { .. })
            ));
        }

        let mut missing_counters = membership_response(Vec::new(), 0);
        missing_counters.counters = None;
        assert_evidence_kind(
            decode_membership_query(&missing_counters, SUBSCRIPTION_ID, SPACE_ID, OBJECT_ID),
            CollectionMembershipEvidenceKind::InvalidCounters,
        );
        let mut wrong_counter_subscription = membership_response(Vec::new(), 0);
        wrong_counter_subscription
            .counters
            .as_mut()
            .expect("counter fixture")
            .sub_id = "different".to_owned();
        assert_evidence_kind(
            decode_membership_query(
                &wrong_counter_subscription,
                SUBSCRIPTION_ID,
                SPACE_ID,
                OBJECT_ID,
            ),
            CollectionMembershipEvidenceKind::InvalidCounters,
        );
        let mut unexpected_dependency = membership_response(Vec::new(), 0);
        unexpected_dependency
            .dependencies
            .push(record(OBJECT_ID, SPACE_ID));
        assert_evidence_kind(
            decode_membership_query(&unexpected_dependency, SUBSCRIPTION_ID, SPACE_ID, OBJECT_ID),
            CollectionMembershipEvidenceKind::InvalidRecords,
        );
    }

    #[test]
    fn membership_decoder_rejects_subscription_counters_and_record_shape_errors() {
        let mut mismatched_echo = membership_response(Vec::new(), 0);
        mismatched_echo.sub_id = "different-subscription".to_owned();
        assert_evidence_kind(
            validate_echoed_subscription_id(&mismatched_echo, SUBSCRIPTION_ID),
            CollectionMembershipEvidenceKind::SubscriptionIdMismatch,
        );
        let mut missing_echo = membership_response(Vec::new(), 0);
        missing_echo.sub_id.clear();
        assert_evidence_kind(
            validate_echoed_subscription_id(&missing_echo, SUBSCRIPTION_ID),
            CollectionMembershipEvidenceKind::MissingSubscriptionId,
        );

        for (total, next_count, prev_count) in [(-1, 0, 0), (2, 0, 0), (0, 1, 0), (0, 0, 1)] {
            let mut response = membership_response(Vec::new(), total);
            let counters = response.counters.as_mut().expect("counter fixture");
            counters.next_count = next_count;
            counters.prev_count = prev_count;
            assert_evidence_kind(
                decode_membership_query(&response, SUBSCRIPTION_ID, SPACE_ID, OBJECT_ID),
                CollectionMembershipEvidenceKind::InvalidCounters,
            );
        }

        let malformed_records = [
            Struct::default(),
            record_with_fields([(ID_KEY, string_value(OBJECT_ID))]),
            record_with_fields([(SPACE_ID_KEY, string_value(SPACE_ID))]),
            record_with_fields([
                (
                    ID_KEY,
                    Value {
                        kind: Some(Kind::NumberValue(1.0)),
                    },
                ),
                (SPACE_ID_KEY, string_value(SPACE_ID)),
            ]),
            record_with_fields([
                (ID_KEY, string_value(OBJECT_ID)),
                (
                    SPACE_ID_KEY,
                    Value {
                        kind: Some(Kind::BoolValue(true)),
                    },
                ),
            ]),
        ];
        for record in malformed_records {
            let response = membership_response(vec![record], 1);
            assert_evidence_kind(
                decode_membership_query(&response, SUBSCRIPTION_ID, SPACE_ID, OBJECT_ID),
                CollectionMembershipEvidenceKind::InvalidRecords,
            );
        }
    }

    #[tokio::test]
    async fn membership_guard_cleans_up_when_subscribe_boundary_is_cancelled() {
        let armed = Arc::new(Notify::new());
        let cleaned = Arc::new(Notify::new());
        let calls = Arc::new(AtomicUsize::new(0));
        let action = successful_cleanup_action(Arc::clone(&calls), Arc::clone(&cleaned));
        let task = tokio::spawn({
            let armed = Arc::clone(&armed);
            async move {
                let _guard = MembershipSubscriptionGuard::from_action(action);
                armed.notify_one();
                pending::<()>().await;
            }
        });
        armed.notified().await;
        task.abort();
        let _ = task.await;
        tokio::time::timeout(Duration::from_secs(1), cleaned.notified())
            .await
            .expect("cancelled subscribe guard cleanup");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn membership_guard_retries_when_unsubscribe_boundary_is_cancelled() {
        let started = Arc::new(Notify::new());
        let recovered = Arc::new(Notify::new());
        let calls = Arc::new(AtomicUsize::new(0));
        let action: MembershipCleanupAction = Arc::new({
            let calls = Arc::clone(&calls);
            let started = Arc::clone(&started);
            let recovered = Arc::clone(&recovered);
            move || {
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
            let mut guard = MembershipSubscriptionGuard::from_action(action);
            let _ = guard.cleanup().await;
        });
        started.notified().await;
        task.abort();
        let _ = task.await;
        tokio::time::timeout(Duration::from_secs(1), recovered.notified())
            .await
            .expect("cancelled unsubscribe guard retry");
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn confirmed_membership_cleanup_disarms_lifecycle_guard() {
        let cleaned = Arc::new(Notify::new());
        let calls = Arc::new(AtomicUsize::new(0));
        let action = successful_cleanup_action(Arc::clone(&calls), Arc::clone(&cleaned));
        {
            let mut guard = MembershipSubscriptionGuard::from_action(action);
            guard.cleanup().await.expect("explicit cleanup");
        }
        cleaned.notified().await;
        tokio::task::yield_now().await;
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn returned_subscription_mismatch_is_rejected_after_owned_cleanup() {
        let cleaned = Arc::new(Notify::new());
        let calls = Arc::new(AtomicUsize::new(0));
        let action = successful_cleanup_action(Arc::clone(&calls), Arc::clone(&cleaned));
        let mut guard = MembershipSubscriptionGuard::from_action(action);
        let mut response = membership_response(Vec::new(), 0);
        response.sub_id = "different-subscription".to_owned();
        assert_evidence_kind(
            finish_membership_response(&response, SUBSCRIPTION_ID, SPACE_ID, OBJECT_ID, &mut guard)
                .await,
            CollectionMembershipEvidenceKind::SubscriptionIdMismatch,
        );
        cleaned.notified().await;
        drop(guard);
        tokio::task::yield_now().await;
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    fn object_fixture(id: &str, space_id: &str, layout: ObjectLayout) -> Object {
        Object {
            archived: false,
            icon: None,
            id: id.to_owned(),
            layout,
            markdown: None,
            name: None,
            object: DataModel::Object,
            properties: Vec::new(),
            snippet: None,
            space_id: space_id.to_owned(),
            r#type: None,
        }
    }

    fn record(id: &str, space_id: &str) -> Struct {
        record_with_fields([
            (ID_KEY, string_value(id)),
            (SPACE_ID_KEY, string_value(space_id)),
        ])
    }

    fn record_with_fields<const N: usize>(fields: [(&str, Value); N]) -> Struct {
        Struct {
            fields: fields
                .into_iter()
                .map(|(key, value)| (key.to_owned(), value))
                .collect::<BTreeMap<_, _>>(),
        }
    }

    fn string_value(value: &str) -> Value {
        Value {
            kind: Some(Kind::StringValue(value.to_owned())),
        }
    }

    fn membership_response(records: Vec<Struct>, total: i64) -> search_subscribe::Response {
        let sub_id = SUBSCRIPTION_ID.to_owned();
        search_subscribe::Response {
            error: None,
            records,
            dependencies: Vec::new(),
            sub_id: sub_id.clone(),
            counters: Some(Counters {
                total,
                next_count: 0,
                prev_count: 0,
                sub_id,
            }),
        }
    }

    fn successful_cleanup_action(
        calls: Arc<AtomicUsize>,
        cleaned: Arc<Notify>,
    ) -> MembershipCleanupAction {
        Arc::new(move || {
            calls.fetch_add(1, Ordering::SeqCst);
            let cleaned = Arc::clone(&cleaned);
            Box::pin(async move {
                cleaned.notify_one();
                Ok(())
            })
        })
    }

    fn assert_evidence_kind<T: std::fmt::Debug>(
        result: Result<T>,
        expected: CollectionMembershipEvidenceKind,
    ) {
        let error = result.expect_err("expected incomplete evidence");
        let AnytypeError::CollectionMembershipEvidence { kind } = error else {
            panic!("expected collection-membership evidence error");
        };
        assert_eq!(kind, expected);
    }
}
