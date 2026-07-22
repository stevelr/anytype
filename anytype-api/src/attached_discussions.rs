// SPDX-FileCopyrightText: 2025-2026 Steve Schoettler
// SPDX-License-Identifier: Apache-2.0

//! Typed discovery and idempotent creation of discussions attached to objects.
//!
//! Attached discussions are derived objects owned by a Basic- or Note-layout
//! parent. They are deliberately separate from ordinary space chats: callers
//! name the parent object, and this module proves the returned discussion's
//! parent-derived identity before returning it.

use std::{
    future::Future,
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

use anytype_rpc::{
    anytype::rpc::object::{close as object_close, discussion_add, show as object_show},
    model::{self, SmartBlockType, object_type},
};
use prost_types::{Struct, value::Kind};
use serde::Deserialize;
use snafu::prelude::*;
use tokio::time::Instant;
use tonic::{Code, Request};

use crate::{
    Result,
    client::AnytypeClient,
    error::{AnytypeError, ValidationSnafu},
    filters::QueryWithFilters,
    grpc_util::{GrpcError, with_token_request},
};

/// Maximum deadline accepted for each attached-discussion gRPC operation.
pub const MAX_ATTACHED_DISCUSSION_RPC_TIMEOUT: Duration = Duration::from_secs(5);

/// Maximum total deadline accepted for one attached-discussion operation.
pub const MAX_ATTACHED_DISCUSSION_OPERATION_TIMEOUT: Duration = Duration::from_secs(30);

/// Payload-free failure classification for attached-discussion operations.
#[derive(Clone, Copy, Debug, PartialEq, Eq, strum::Display)]
#[strum(serialize_all = "snake_case")]
pub enum AttachedDiscussionErrorKind {
    /// The exact parent is not a Basic- or Note-layout object.
    UnsupportedParentLayout,
    /// Identity or relation evidence was missing, malformed, or inconsistent.
    MalformedEvidence,
    /// One gRPC call exceeded its finite per-call deadline.
    RpcDeadline,
    /// The complete operation exhausted its caller-selected absolute deadline.
    OperationDeadline,
    /// A shown object view could not be confirmed closed.
    CleanupFailed,
    /// A dispatched attachment could not be reconciled to one verified state.
    MutationIndeterminate,
    /// An upstream gRPC status or application result was not usable.
    Upstream,
    /// An owned Tokio task terminated without returning its result.
    OwnedTaskFailed,
}

/// Cumulative work counters for attached-discussion operations.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AttachedDiscussionMetricsSnapshot {
    /// Exact parent REST reads started by this operation family.
    pub parent_get_attempts: u64,
    /// `ObjectShow` RPCs dispatched.
    pub show_attempts: u64,
    /// `ObjectShow` RPCs that returned one usable object view.
    pub accepted_shows: u64,
    /// Owned `ObjectClose` RPCs dispatched.
    pub close_attempts: u64,
    /// Owned `ObjectClose` RPCs confirmed successful.
    pub close_successes: u64,
    /// `ObjectAddDiscussion` RPCs dispatched.
    pub write_dispatches: u64,
    /// Fresh state reads started after a write dispatch.
    pub reconciliation_attempts: u64,
}

#[derive(Debug, Default)]
pub(crate) struct AttachedDiscussionMetrics {
    parent_get_attempts: AtomicU64,
    show_attempts: AtomicU64,
    accepted_shows: AtomicU64,
    close_attempts: AtomicU64,
    close_successes: AtomicU64,
    write_dispatches: AtomicU64,
    reconciliation_attempts: AtomicU64,
}

impl AttachedDiscussionMetrics {
    pub(crate) fn snapshot(&self) -> AttachedDiscussionMetricsSnapshot {
        AttachedDiscussionMetricsSnapshot {
            parent_get_attempts: self.parent_get_attempts.load(Ordering::Relaxed),
            show_attempts: self.show_attempts.load(Ordering::Relaxed),
            accepted_shows: self.accepted_shows.load(Ordering::Relaxed),
            close_attempts: self.close_attempts.load(Ordering::Relaxed),
            close_successes: self.close_successes.load(Ordering::Relaxed),
            write_dispatches: self.write_dispatches.load(Ordering::Relaxed),
            reconciliation_attempts: self.reconciliation_attempts.load(Ordering::Relaxed),
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct OperationBudget {
    deadline: Instant,
}

impl OperationBudget {
    fn new(timeout: Duration) -> Self {
        Self {
            deadline: Instant::now() + timeout,
        }
    }

    fn remaining(self) -> Result<Duration> {
        self.deadline
            .checked_duration_since(Instant::now())
            .filter(|remaining| !remaining.is_zero())
            .ok_or_else(|| attached_error_value(AttachedDiscussionErrorKind::OperationDeadline))
    }

    fn call_timeout(self, rpc_timeout: Duration) -> Result<Duration> {
        Ok(self.remaining()?.min(rpc_timeout))
    }

    fn slice(self, divisor: u32) -> Result<Self> {
        let timeout = self
            .remaining()?
            .checked_div(divisor)
            .filter(|timeout| !timeout.is_zero())
            .ok_or_else(|| attached_error_value(AttachedDiscussionErrorKind::OperationDeadline))?;
        Ok(Self::new(timeout))
    }
}

#[derive(Debug, Deserialize)]
struct ExactParentResponse {
    object: ExactParent,
}

#[derive(Debug, Deserialize)]
struct ExactParent {
    id: String,
    space_id: String,
    layout: ExactParentLayout,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ExactParentLayout {
    Basic,
    Note,
    #[serde(other)]
    Unsupported,
}

const DISCUSSION_ID: &str = "discussionId";
const SPACE_ID: &str = "spaceId";
const UNIQUE_KEY: &str = "uniqueKey";
const RESOLVED_LAYOUT: &str = "resolvedLayout";
const MAX_DETAILS_SETS: usize = 64;
const MAX_DETAIL_FIELDS: usize = 256;

/// Exact state of the discussion relation on one parent object.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AttachedDiscussion {
    /// The verified parent has no attached discussion relation.
    Absent {
        /// Exact containing space.
        space_id: String,
        /// Exact parent object.
        parent_id: String,
    },
    /// The parent has one verified derived discussion object.
    Attached {
        /// Exact containing space.
        space_id: String,
        /// Exact parent object.
        parent_id: String,
        /// Verified derived discussion object ID.
        discussion_id: String,
    },
}

impl AttachedDiscussion {
    /// Returns the containing space ID.
    #[must_use]
    pub fn space_id(&self) -> &str {
        match self {
            Self::Absent { space_id, .. } | Self::Attached { space_id, .. } => space_id,
        }
    }

    /// Returns the parent object ID.
    #[must_use]
    pub fn parent_id(&self) -> &str {
        match self {
            Self::Absent { parent_id, .. } | Self::Attached { parent_id, .. } => parent_id,
        }
    }

    /// Returns the attached discussion ID when one exists.
    #[must_use]
    pub fn discussion_id(&self) -> Option<&str> {
        match self {
            Self::Absent { .. } => None,
            Self::Attached { discussion_id, .. } => Some(discussion_id),
        }
    }

    /// Returns whether the parent has a verified attached discussion.
    #[must_use]
    pub const fn is_attached(&self) -> bool {
        matches!(self, Self::Attached { .. })
    }
}

/// Request builder for one parent's attached discussion.
#[derive(Clone, Debug)]
pub struct AttachedDiscussionRequest {
    client: AnytypeClient,
    space_id: String,
    parent_id: String,
    rpc_timeout: Duration,
    operation_timeout: Duration,
}

impl AttachedDiscussionRequest {
    fn new(
        client: AnytypeClient,
        space_id: impl Into<String>,
        parent_id: impl Into<String>,
    ) -> Self {
        Self {
            client,
            space_id: space_id.into(),
            parent_id: parent_id.into(),
            rpc_timeout: MAX_ATTACHED_DISCUSSION_RPC_TIMEOUT,
            operation_timeout: MAX_ATTACHED_DISCUSSION_OPERATION_TIMEOUT,
        }
    }

    /// Sets the finite deadline used independently for every gRPC call.
    ///
    /// The deadline must be nonzero and no greater than five seconds.
    #[must_use]
    pub const fn rpc_timeout(mut self, timeout: Duration) -> Self {
        self.rpc_timeout = timeout;
        self
    }

    /// Sets the finite absolute deadline for the complete operation.
    ///
    /// The deadline includes REST preflight, every gRPC show/close pair, a
    /// possible sole write dispatch, and post-dispatch reconciliation. Each
    /// show reserves time for its owned close, and a dispatched write reserves
    /// time for reconciliation. The deadline must be nonzero and no greater
    /// than thirty seconds.
    #[must_use]
    pub const fn operation_timeout(mut self, timeout: Duration) -> Self {
        self.operation_timeout = timeout;
        self
    }

    /// Reads and verifies the parent's current attached-discussion state.
    ///
    /// This performs a cache-independent REST parent read followed by exact
    /// `ObjectShow` reads. Every successful or indeterminate show is paired
    /// with a fresh, bounded `ObjectClose`. An attached result is returned only
    /// after the derived discussion's space, smart-block type, layout, and
    /// unique key have all been verified.
    ///
    /// # Errors
    ///
    /// Returns validation errors for invalid IDs or deadlines, structural
    /// authentication errors, and [`AnytypeError::AttachedDiscussion`] with a
    /// closed payload-free kind for unsupported, malformed, deadline, cleanup,
    /// upstream, or internally inconsistent outcomes.
    pub async fn get(self) -> Result<AttachedDiscussion> {
        self.validate()?;
        let budget = OperationBudget::new(self.operation_timeout);
        verify_parent_rest(&self.client, &self.space_id, &self.parent_id, budget).await?;
        read_verified_state(
            self.client,
            self.space_id,
            self.parent_id,
            self.rpc_timeout,
            budget,
        )
        .await
    }

    /// Returns the existing attached discussion or creates and verifies it once.
    ///
    /// The operation is idempotent by construction: it reads before writing and
    /// never dispatches `ObjectAddDiscussion` when a relation already exists.
    /// After the sole possible dispatch it always re-reads the parent and
    /// independently verifies the derived discussion. Transport errors and
    /// malformed or absent post-write evidence fail safely without replaying
    /// the mutation. Once dispatched, reconciliation runs in an owned Tokio
    /// task so cancellation of the calling future cannot trigger a second write.
    ///
    /// # Errors
    ///
    /// Returns the errors documented by [`Self::get`]. A dispatched mutation
    /// whose final state cannot be verified returns a fixed, payload-free
    /// indeterminate error and is never retried automatically.
    pub async fn ensure(self) -> Result<AttachedDiscussion> {
        self.validate()?;
        let budget = OperationBudget::new(self.operation_timeout);
        verify_parent_rest(&self.client, &self.space_id, &self.parent_id, budget).await?;
        let existing = read_verified_state(
            self.client.clone(),
            self.space_id.clone(),
            self.parent_id.clone(),
            self.rpc_timeout,
            budget,
        )
        .await?;
        if existing.is_attached() {
            return Ok(existing);
        }

        let client = self.client;
        let space_id = self.space_id;
        let parent_id = self.parent_id;
        let timeout = self.rpc_timeout;
        tokio::spawn(async move {
            dispatch_once_and_reconcile(client, space_id, parent_id, timeout, budget).await
        })
        .await
        .map_err(|_| attached_error_value(AttachedDiscussionErrorKind::OwnedTaskFailed))?
    }

    fn validate(&self) -> Result<()> {
        self.client
            .config
            .limits
            .validate_id(&self.space_id, "space_id")?;
        self.client
            .config
            .limits
            .validate_id(&self.parent_id, "parent_id")?;
        ensure!(
            !self.rpc_timeout.is_zero() && self.rpc_timeout <= MAX_ATTACHED_DISCUSSION_RPC_TIMEOUT,
            ValidationSnafu {
                message: "attached discussion RPC deadline must be between zero and five seconds"
                    .to_owned(),
            }
        );
        ensure!(
            !self.operation_timeout.is_zero()
                && self.operation_timeout <= MAX_ATTACHED_DISCUSSION_OPERATION_TIMEOUT,
            ValidationSnafu {
                message:
                    "attached discussion operation deadline must be between zero and thirty seconds"
                        .to_owned(),
            }
        );
        Ok(())
    }
}

impl AnytypeClient {
    /// Scopes typed attached-discussion operations to one exact parent object.
    ///
    /// Attached discussions are not ordinary space chats. Use this builder to
    /// discover or ensure the derived discussion belonging to `parent_id`.
    #[must_use]
    pub fn attached_discussion(
        &self,
        space_id: impl Into<String>,
        parent_id: impl Into<String>,
    ) -> AttachedDiscussionRequest {
        AttachedDiscussionRequest::new(self.clone(), space_id, parent_id)
    }
}

async fn verify_parent_rest(
    client: &AnytypeClient,
    space_id: &str,
    parent_id: &str,
    budget: OperationBudget,
) -> Result<()> {
    client
        .attached_discussion_metrics
        .parent_get_attempts
        .fetch_add(1, Ordering::Relaxed);
    let path = format!("/v1/spaces/{space_id}/objects/{parent_id}");
    let remaining = budget.remaining()?;
    let response = tokio::time::timeout(
        remaining,
        client.client.get_request_with_limit::<ExactParentResponse>(
            &path,
            QueryWithFilters::default(),
            client.client.document_response_limit(),
        ),
    )
    .await
    .map_err(|_| attached_error_value(AttachedDiscussionErrorKind::OperationDeadline))?
    .map_err(parent_rest_error)?;
    verify_parent(&response.object, space_id, parent_id)
}

fn parent_rest_error(error: AnytypeError) -> AnytypeError {
    if matches!(error, AnytypeError::Deserialization { .. }) {
        attached_error_value(AttachedDiscussionErrorKind::MalformedEvidence)
    } else {
        error
    }
}

fn verify_parent(parent: &ExactParent, space_id: &str, parent_id: &str) -> Result<()> {
    if parent.id != parent_id || parent.space_id != space_id {
        return attached_error(AttachedDiscussionErrorKind::MalformedEvidence);
    }
    if !matches!(
        parent.layout,
        ExactParentLayout::Basic | ExactParentLayout::Note
    ) {
        return attached_error(AttachedDiscussionErrorKind::UnsupportedParentLayout);
    }
    Ok(())
}

async fn dispatch_once_and_reconcile(
    client: AnytypeClient,
    space_id: String,
    parent_id: String,
    timeout: Duration,
    budget: OperationBudget,
) -> Result<AttachedDiscussion> {
    let (grpc, request, call_timeout) =
        prepare_discussion_add(&client, &parent_id, timeout, budget).await?;
    let metrics = client.attached_discussion_metrics.clone();
    let dispatch_metrics = metrics.clone();
    dispatch_and_reconcile_with(
        metrics.as_ref(),
        || async move {
            dispatch_prepared(dispatch_metrics.as_ref(), grpc, request, call_timeout).await
        },
        || async move { read_verified_state(client, space_id, parent_id, timeout, budget).await },
    )
    .await
}

async fn dispatch_and_reconcile_with<D, DF, R, RF>(
    metrics: &AttachedDiscussionMetrics,
    dispatch: D,
    read: R,
) -> Result<AttachedDiscussion>
where
    D: FnOnce() -> DF,
    DF: Future<Output = Result<discussion_add::Response>>,
    R: FnOnce() -> RF,
    RF: Future<Output = Result<AttachedDiscussion>>,
{
    let dispatch = dispatch().await;
    metrics
        .reconciliation_attempts
        .fetch_add(1, Ordering::Relaxed);
    let state = read().await;
    reconcile_dispatch(dispatch, state)
}

fn reconcile_dispatch(
    dispatch: Result<discussion_add::Response>,
    state: Result<AttachedDiscussion>,
) -> Result<AttachedDiscussion> {
    let candidate = dispatch
        .as_ref()
        .ok()
        .map(|response| response.discussion_id.as_str())
        .filter(|id| !id.is_empty())
        .map(ToOwned::to_owned);
    match state {
        Ok(attached @ AttachedDiscussion::Attached { .. }) => {
            let actual = attached.discussion_id();
            if candidate.as_deref().is_some_and(|id| Some(id) != actual) {
                return attached_error(AttachedDiscussionErrorKind::MalformedEvidence);
            }
            Ok(attached)
        }
        Ok(AttachedDiscussion::Absent { .. }) => match dispatch {
            Ok(response) => absent_after_dispatch(response.error.as_ref()),
            Err(error) if error.is_authentication() => Err(error),
            Err(error @ AnytypeError::Validation { .. }) => Err(error),
            Err(_) => attached_error(AttachedDiscussionErrorKind::MutationIndeterminate),
        },
        Err(_) => attached_error(AttachedDiscussionErrorKind::MutationIndeterminate),
    }
}

async fn prepare_discussion_add(
    client: &AnytypeClient,
    parent_id: &str,
    timeout: Duration,
    budget: OperationBudget,
) -> Result<(
    anytype_rpc::client::AnytypeGrpcClient,
    Request<discussion_add::Request>,
    Duration,
)> {
    let remaining = budget.remaining()?;
    let grpc = tokio::time::timeout(remaining, client.grpc_client())
        .await
        .map_err(|_| attached_error_value(AttachedDiscussionErrorKind::OperationDeadline))??;
    let request = discussion_add::Request {
        object_id: parent_id.to_owned(),
    };
    let mut request = with_token_request(Request::new(request), grpc.token())?;
    let call_timeout = budget.slice(2)?.call_timeout(timeout)?;
    request.set_timeout(call_timeout);
    Ok((grpc, request, call_timeout))
}

async fn dispatch_prepared(
    metrics: &AttachedDiscussionMetrics,
    grpc: anytype_rpc::client::AnytypeGrpcClient,
    request: Request<discussion_add::Request>,
    call_timeout: Duration,
) -> Result<discussion_add::Response> {
    metrics.write_dispatches.fetch_add(1, Ordering::Relaxed);
    let mut commands = grpc.client_commands();
    tokio::time::timeout(call_timeout, commands.object_add_discussion(request))
        .await
        .map_err(|_| attached_error_value(AttachedDiscussionErrorKind::RpcDeadline))?
        .map_err(attached_grpc_status)
        .map(tonic::Response::into_inner)
}

async fn read_verified_state(
    client: AnytypeClient,
    space_id: String,
    parent_id: String,
    timeout: Duration,
    budget: OperationBudget,
) -> Result<AttachedDiscussion> {
    let parent_view = show_owned(
        client.clone(),
        space_id.clone(),
        parent_id.clone(),
        timeout,
        budget.slice(2)?,
    )
    .await?;
    let discussion_id = discussion_id_from_parent(&parent_view, &parent_id)?;
    let Some(discussion_id) = discussion_id else {
        return Ok(AttachedDiscussion::Absent {
            space_id,
            parent_id,
        });
    };
    client
        .config
        .limits
        .validate_id(&discussion_id, "discussion_id")
        .map_err(|_| attached_error_value(AttachedDiscussionErrorKind::MalformedEvidence))?;
    let discussion_view = show_owned(
        client,
        space_id.clone(),
        discussion_id.clone(),
        timeout,
        budget,
    )
    .await?;
    verify_discussion_view(&discussion_view, &space_id, &parent_id, &discussion_id)?;
    Ok(AttachedDiscussion::Attached {
        space_id,
        parent_id,
        discussion_id,
    })
}

async fn show_owned(
    client: AnytypeClient,
    space_id: String,
    object_id: String,
    timeout: Duration,
    budget: OperationBudget,
) -> Result<model::ObjectView> {
    await_owned(async move { show_and_close(client, space_id, object_id, timeout, budget).await })
        .await
}

async fn await_owned<F, T>(future: F) -> Result<T>
where
    F: Future<Output = Result<T>> + Send + 'static,
    T: Send + 'static,
{
    tokio::spawn(future)
        .await
        .map_err(|_| attached_error_value(AttachedDiscussionErrorKind::OwnedTaskFailed))?
}

async fn show_and_close(
    client: AnytypeClient,
    space_id: String,
    object_id: String,
    timeout: Duration,
    budget: OperationBudget,
) -> Result<model::ObjectView> {
    let remaining = budget.remaining()?;
    let grpc = tokio::time::timeout(remaining, client.grpc_client())
        .await
        .map_err(|_| attached_error_value(AttachedDiscussionErrorKind::OperationDeadline))??;
    let show = object_show::Request {
        context_id: object_id.clone(),
        object_id: object_id.clone(),
        space_id: space_id.clone(),
        include_relations_as_dependent_objects: false,
        ..Default::default()
    };
    let mut show = with_token_request(Request::new(show), grpc.token())?;
    let show_timeout = budget.slice(2)?.call_timeout(timeout)?;
    show.set_timeout(show_timeout);
    let show_grpc = grpc.clone();
    let close_grpc = grpc;
    let close_space_id = space_id;
    let close_object_id = object_id;
    show_lifecycle_with(
        client.attached_discussion_metrics.as_ref(),
        || async move {
            let mut commands = show_grpc.client_commands();
            let response =
                match tokio::time::timeout(show_timeout, commands.object_show(show)).await {
                    Err(_) => {
                        return ShowAttempt::indeterminate(attached_error_value(
                            AttachedDiscussionErrorKind::RpcDeadline,
                        ));
                    }
                    Ok(Err(status)) if is_definitive_show_rejection(&status) => {
                        return ShowAttempt::definitive_failure(attached_grpc_status(status));
                    }
                    Ok(Err(status)) => {
                        return ShowAttempt::indeterminate(attached_grpc_status(status));
                    }
                    Ok(Ok(response)) => response.into_inner(),
                };
            let shown = show_response_ok(response.error.as_ref()).and_then(|()| {
                response.object_view.ok_or_else(|| {
                    attached_error_value(AttachedDiscussionErrorKind::MalformedEvidence)
                })
            });
            ShowAttempt::responded(shown)
        },
        || async move {
            let close = object_close::Request {
                context_id: close_object_id.clone(),
                object_id: close_object_id,
                space_id: close_space_id,
            };
            let mut close = with_token_request(Request::new(close), close_grpc.token())?;
            let close_timeout = budget
                .call_timeout(timeout)
                .map_err(|_| attached_error_value(AttachedDiscussionErrorKind::CleanupFailed))?;
            close.set_timeout(close_timeout);
            let mut commands = close_grpc.client_commands();
            let response = tokio::time::timeout(close_timeout, commands.object_close(close))
                .await
                .map_err(|_| attached_error_value(AttachedDiscussionErrorKind::CleanupFailed))?
                .map_err(|_| attached_error_value(AttachedDiscussionErrorKind::CleanupFailed))?
                .into_inner();
            close_response_ok(response.error.as_ref())
        },
    )
    .await
}

struct ShowAttempt {
    shown: Result<model::ObjectView>,
    close_required: bool,
    accepted: bool,
}

impl ShowAttempt {
    fn responded(shown: Result<model::ObjectView>) -> Self {
        Self {
            accepted: shown.is_ok(),
            shown,
            close_required: true,
        }
    }

    fn indeterminate(error: AnytypeError) -> Self {
        Self {
            shown: Err(error),
            close_required: true,
            accepted: false,
        }
    }

    fn definitive_failure(error: AnytypeError) -> Self {
        Self {
            shown: Err(error),
            close_required: false,
            accepted: false,
        }
    }
}

fn is_definitive_show_rejection(status: &tonic::Status) -> bool {
    matches!(
        status.code(),
        Code::Unauthenticated | Code::PermissionDenied
    )
}

async fn show_lifecycle_with<S, SF, C, CF>(
    metrics: &AttachedDiscussionMetrics,
    show: S,
    close: C,
) -> Result<model::ObjectView>
where
    S: FnOnce() -> SF,
    SF: Future<Output = ShowAttempt>,
    C: FnOnce() -> CF,
    CF: Future<Output = Result<()>>,
{
    metrics.show_attempts.fetch_add(1, Ordering::Relaxed);
    let attempt = show().await;
    if attempt.accepted {
        metrics.accepted_shows.fetch_add(1, Ordering::Relaxed);
    }
    if !attempt.close_required {
        return attempt.shown;
    }
    metrics.close_attempts.fetch_add(1, Ordering::Relaxed);
    let cleanup = close().await;
    if cleanup.is_ok() {
        metrics.close_successes.fetch_add(1, Ordering::Relaxed);
    }
    finish_show_lifecycle(attempt.shown, cleanup)
}

fn finish_show_lifecycle(
    shown: Result<model::ObjectView>,
    cleanup: Result<()>,
) -> Result<model::ObjectView> {
    cleanup?;
    shown
}

fn discussion_id_from_parent(view: &model::ObjectView, parent_id: &str) -> Result<Option<String>> {
    if view.root_id != parent_id {
        return attached_error(AttachedDiscussionErrorKind::MalformedEvidence);
    }
    let details = exact_details(view, parent_id, "parent")?;
    details.fields.get(DISCUSSION_ID).map_or_else(
        || Ok(None),
        |value| match value.kind.as_ref() {
            Some(Kind::StringValue(value)) if value.is_empty() => Ok(None),
            Some(Kind::StringValue(value)) => Ok(Some(value.clone())),
            _ => attached_error(AttachedDiscussionErrorKind::MalformedEvidence),
        },
    )
}

fn verify_discussion_view(
    view: &model::ObjectView,
    space_id: &str,
    parent_id: &str,
    discussion_id: &str,
) -> Result<()> {
    if view.root_id != discussion_id || view.r#type != SmartBlockType::DiscussionObject as i32 {
        return attached_error(AttachedDiscussionErrorKind::MalformedEvidence);
    }
    let details = exact_details(view, discussion_id, "discussion")?;
    ensure_string(details, SPACE_ID, space_id)?;
    ensure_string(details, UNIQUE_KEY, &format!("discussion-{parent_id}"))?;
    ensure_number(
        details,
        RESOLVED_LAYOUT,
        object_type::Layout::Discussion as i32 as f64,
    )?;
    Ok(())
}

fn exact_details<'a>(
    view: &'a model::ObjectView,
    object_id: &str,
    _label: &str,
) -> Result<&'a Struct> {
    if view.details.len() > MAX_DETAILS_SETS {
        return attached_error(AttachedDiscussionErrorKind::MalformedEvidence);
    }
    let mut matches = view.details.iter().filter(|set| set.id == object_id);
    let first = matches
        .next()
        .and_then(|set| set.details.as_ref())
        .ok_or_else(|| attached_error_value(AttachedDiscussionErrorKind::MalformedEvidence))?;
    if first.fields.len() > MAX_DETAIL_FIELDS {
        return attached_error(AttachedDiscussionErrorKind::MalformedEvidence);
    }
    for duplicate in matches {
        let duplicate = duplicate
            .details
            .as_ref()
            .ok_or_else(|| attached_error_value(AttachedDiscussionErrorKind::MalformedEvidence))?;
        if duplicate.fields.len() > MAX_DETAIL_FIELDS || duplicate != first {
            return attached_error(AttachedDiscussionErrorKind::MalformedEvidence);
        }
    }
    Ok(first)
}

fn ensure_string(details: &Struct, key: &str, expected: &str) -> Result<()> {
    if !matches!(
        details.fields.get(key).and_then(|value| value.kind.as_ref()),
        Some(Kind::StringValue(value)) if value == expected
    ) {
        return attached_error(AttachedDiscussionErrorKind::MalformedEvidence);
    }
    Ok(())
}

fn ensure_number(details: &Struct, key: &str, expected: f64) -> Result<()> {
    if !matches!(
        details.fields.get(key).and_then(|value| value.kind.as_ref()),
        Some(Kind::NumberValue(value)) if *value == expected
    ) {
        return attached_error(AttachedDiscussionErrorKind::MalformedEvidence);
    }
    Ok(())
}

fn attached_error<T>(kind: AttachedDiscussionErrorKind) -> Result<T> {
    Err(attached_error_value(kind))
}

fn attached_error_value(kind: AttachedDiscussionErrorKind) -> AnytypeError {
    AnytypeError::AttachedDiscussion { kind }
}

fn attached_grpc_status(status: tonic::Status) -> AnytypeError {
    match status.code() {
        Code::Unauthenticated | Code::PermissionDenied => AnytypeError::Auth {
            message: "attached discussion gRPC authentication failed".to_owned(),
        },
        Code::DeadlineExceeded => attached_error_value(AttachedDiscussionErrorKind::RpcDeadline),
        _ => attached_error_value(AttachedDiscussionErrorKind::Upstream),
    }
}

fn show_response_ok<T: GrpcError>(error: Option<&T>) -> Result<()> {
    if error.is_some_and(|error| error.code() != 0) {
        return attached_error(AttachedDiscussionErrorKind::Upstream);
    }
    Ok(())
}

fn close_response_ok<T: GrpcError>(error: Option<&T>) -> Result<()> {
    if error.is_some_and(|error| error.code() != 0) {
        return attached_error(AttachedDiscussionErrorKind::CleanupFailed);
    }
    Ok(())
}

fn absent_after_dispatch(
    error: Option<&discussion_add::response::Error>,
) -> Result<AttachedDiscussion> {
    match error.map_or(0, GrpcError::code) {
        2 => Err(AnytypeError::Validation {
            message: "attached discussion mutation rejected invalid input".to_owned(),
        }),
        _ => attached_error(AttachedDiscussionErrorKind::MutationIndeterminate),
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeMap,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering as AtomicOrdering},
        },
    };

    use prost_types::Value;
    use tokio::sync::Notify;

    use super::*;

    const SPACE: &str = "bafyreiaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.aaaa";
    const PARENT: &str = "bafyreiaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const DISCUSSION: &str = "bafyreidddddddddddddddddddddddddddddddddddddddddddddddddddd";

    fn value(kind: Kind) -> Value {
        Value { kind: Some(kind) }
    }

    fn fields(entries: impl IntoIterator<Item = (&'static str, Value)>) -> Struct {
        Struct {
            fields: entries
                .into_iter()
                .map(|(key, value)| (key.to_owned(), value))
                .collect::<BTreeMap<_, _>>(),
        }
    }

    fn detail(id: &str, details: Struct) -> model::object_view::DetailsSet {
        model::object_view::DetailsSet {
            id: id.to_owned(),
            details: Some(details),
            sub_ids: Vec::new(),
        }
    }

    fn parent(relation: Option<Value>) -> model::ObjectView {
        let mut entries = Vec::new();
        if let Some(relation) = relation {
            entries.push((DISCUSSION_ID, relation));
        }
        model::ObjectView {
            root_id: PARENT.to_owned(),
            details: vec![detail(PARENT, fields(entries))],
            ..Default::default()
        }
    }

    fn discussion() -> model::ObjectView {
        model::ObjectView {
            root_id: DISCUSSION.to_owned(),
            r#type: SmartBlockType::DiscussionObject as i32,
            details: vec![detail(
                DISCUSSION,
                fields([
                    (SPACE_ID, value(Kind::StringValue(SPACE.to_owned()))),
                    (
                        UNIQUE_KEY,
                        value(Kind::StringValue(format!("discussion-{PARENT}"))),
                    ),
                    (
                        RESOLVED_LAYOUT,
                        value(Kind::NumberValue(
                            object_type::Layout::Discussion as i32 as f64,
                        )),
                    ),
                ]),
            )],
            ..Default::default()
        }
    }

    fn absent_state() -> AttachedDiscussion {
        AttachedDiscussion::Absent {
            space_id: SPACE.to_owned(),
            parent_id: PARENT.to_owned(),
        }
    }

    fn attached_state() -> AttachedDiscussion {
        AttachedDiscussion::Attached {
            space_id: SPACE.to_owned(),
            parent_id: PARENT.to_owned(),
            discussion_id: DISCUSSION.to_owned(),
        }
    }

    fn dispatch_response(id: &str, code: i32) -> discussion_add::Response {
        discussion_add::Response {
            error: (code != 0).then(|| discussion_add::response::Error {
                code,
                description: "untrusted upstream payload".to_owned(),
            }),
            discussion_id: id.to_owned(),
        }
    }

    fn assert_attached_kind(error: AnytypeError, expected: AttachedDiscussionErrorKind) {
        assert!(matches!(
            error,
            AnytypeError::AttachedDiscussion { kind } if kind == expected
        ));
    }

    #[test]
    fn parent_missing_or_empty_relation_is_absent() {
        assert_eq!(
            discussion_id_from_parent(&parent(None), PARENT).unwrap(),
            None
        );
        assert_eq!(
            discussion_id_from_parent(
                &parent(Some(value(Kind::StringValue(String::new())))),
                PARENT
            )
            .unwrap(),
            None
        );
    }

    #[test]
    fn parent_relation_returns_exact_id() {
        assert_eq!(
            discussion_id_from_parent(
                &parent(Some(value(Kind::StringValue(DISCUSSION.to_owned())))),
                PARENT
            )
            .unwrap()
            .as_deref(),
            Some(DISCUSSION)
        );
    }

    #[test]
    fn malformed_parent_relation_is_rejected() {
        assert!(
            discussion_id_from_parent(&parent(Some(value(Kind::NumberValue(1.0)))), PARENT)
                .is_err()
        );
    }

    #[test]
    fn wrong_parent_root_is_rejected() {
        let mut view = parent(None);
        view.root_id = DISCUSSION.to_owned();
        assert!(discussion_id_from_parent(&view, PARENT).is_err());
    }

    #[test]
    fn verified_discussion_requires_all_derived_identity() {
        assert!(verify_discussion_view(&discussion(), SPACE, PARENT, DISCUSSION).is_ok());

        let mut wrong_type = discussion();
        wrong_type.r#type = SmartBlockType::Page as i32;
        assert!(verify_discussion_view(&wrong_type, SPACE, PARENT, DISCUSSION).is_err());

        let mut wrong_space = discussion();
        wrong_space.details[0]
            .details
            .as_mut()
            .unwrap()
            .fields
            .insert(
                SPACE_ID.to_owned(),
                value(Kind::StringValue(PARENT.to_owned())),
            );
        assert!(verify_discussion_view(&wrong_space, SPACE, PARENT, DISCUSSION).is_err());

        let mut wrong_key = discussion();
        wrong_key.details[0]
            .details
            .as_mut()
            .unwrap()
            .fields
            .insert(
                UNIQUE_KEY.to_owned(),
                value(Kind::StringValue("chat-x".to_owned())),
            );
        assert!(verify_discussion_view(&wrong_key, SPACE, PARENT, DISCUSSION).is_err());

        let mut wrong_layout = discussion();
        wrong_layout.details[0]
            .details
            .as_mut()
            .unwrap()
            .fields
            .insert(RESOLVED_LAYOUT.to_owned(), value(Kind::NumberValue(22.0)));
        assert!(verify_discussion_view(&wrong_layout, SPACE, PARENT, DISCUSSION).is_err());
    }

    #[test]
    fn conflicting_duplicate_details_are_rejected() {
        let mut view = discussion();
        view.details.push(detail(DISCUSSION, Struct::default()));
        assert!(verify_discussion_view(&view, SPACE, PARENT, DISCUSSION).is_err());
    }

    #[test]
    fn missing_and_oversized_details_are_rejected() {
        let mut missing = discussion();
        missing.details.clear();
        assert!(verify_discussion_view(&missing, SPACE, PARENT, DISCUSSION).is_err());

        let mut too_many_sets = discussion();
        too_many_sets.details.extend(
            (0..MAX_DETAILS_SETS)
                .map(|index| detail(&format!("dependent-{index}"), Struct::default())),
        );
        assert!(verify_discussion_view(&too_many_sets, SPACE, PARENT, DISCUSSION).is_err());

        let mut too_many_fields = discussion();
        let root = too_many_fields.details[0]
            .details
            .as_mut()
            .expect("discussion details");
        for index in 0..=MAX_DETAIL_FIELDS {
            root.fields.insert(
                format!("extra-{index}"),
                value(Kind::StringValue(String::new())),
            );
        }
        assert!(verify_discussion_view(&too_many_fields, SPACE, PARENT, DISCUSSION).is_err());
    }

    #[test]
    fn exact_parent_accepts_basic_and_note_and_rejects_every_other_layout() {
        for layout in [ExactParentLayout::Basic, ExactParentLayout::Note] {
            assert!(
                verify_parent(
                    &ExactParent {
                        id: PARENT.to_owned(),
                        space_id: SPACE.to_owned(),
                        layout,
                    },
                    SPACE,
                    PARENT,
                )
                .is_ok()
            );
        }
        for layout in [
            "profile",
            "action",
            "bookmark",
            "set",
            "collection",
            "participant",
            "chat",
            "object_type",
            "relation",
            "file",
            "dashboard",
            "image",
            "audio",
            "video",
        ] {
            let response: ExactParentResponse = serde_json::from_value(serde_json::json!({
                "object": { "id": PARENT, "space_id": SPACE, "layout": layout }
            }))
            .expect("closed unsupported layout wire");
            let error = verify_parent(&response.object, SPACE, PARENT)
                .expect_err("unsupported parent layout");
            assert_attached_kind(error, AttachedDiscussionErrorKind::UnsupportedParentLayout);
        }
    }

    #[test]
    fn exact_parent_wire_requires_layout_and_exact_scope() {
        let missing = serde_json::from_value::<ExactParentResponse>(serde_json::json!({
            "object": { "id": PARENT, "space_id": SPACE }
        }));
        assert!(missing.is_err());
        let mapped = parent_rest_error(AnytypeError::Deserialization {
            source: missing.expect_err("required wire layout"),
        });
        assert_attached_kind(mapped, AttachedDiscussionErrorKind::MalformedEvidence);

        for (id, space_id) in [(DISCUSSION, SPACE), (PARENT, PARENT)] {
            let error = verify_parent(
                &ExactParent {
                    id: id.to_owned(),
                    space_id: space_id.to_owned(),
                    layout: ExactParentLayout::Basic,
                },
                SPACE,
                PARENT,
            )
            .expect_err("foreign REST identity");
            assert_attached_kind(error, AttachedDiscussionErrorKind::MalformedEvidence);
        }
    }

    #[test]
    fn grpc_authentication_statuses_are_structural_and_payload_safe() {
        for code in [Code::Unauthenticated, Code::PermissionDenied] {
            let error = attached_grpc_status(tonic::Status::new(code, "secret upstream payload"));
            assert!(error.is_authentication());
            assert!(!format!("{error:?}").contains("secret upstream payload"));
            match error {
                AnytypeError::Auth { message } => {
                    assert!(!message.contains("secret upstream payload"));
                }
                other => panic!("expected structural authentication error, got {other:?}"),
            }
        }
        let upstream = attached_grpc_status(tonic::Status::unavailable("secret upstream payload"));
        assert_attached_kind(upstream, AttachedDiscussionErrorKind::Upstream);
    }

    #[test]
    fn reconciliation_accepts_only_one_exact_verified_final_state() {
        let attached =
            reconcile_dispatch(Ok(dispatch_response(DISCUSSION, 0)), Ok(attached_state()))
                .expect("matching final state");
        assert_eq!(attached.discussion_id(), Some(DISCUSSION));

        let mismatch = reconcile_dispatch(Ok(dispatch_response(PARENT, 0)), Ok(attached_state()))
            .expect_err("candidate mismatch");
        assert_attached_kind(mismatch, AttachedDiscussionErrorKind::MalformedEvidence);

        let recovered = reconcile_dispatch(
            attached_error(AttachedDiscussionErrorKind::RpcDeadline),
            Ok(attached_state()),
        )
        .expect("verified state resolves uncertain transport");
        assert_eq!(recovered.discussion_id(), Some(DISCUSSION));

        for dispatch in [
            Ok(dispatch_response(DISCUSSION, 0)),
            Ok(dispatch_response("", 1)),
            attached_error(AttachedDiscussionErrorKind::RpcDeadline),
            attached_error(AttachedDiscussionErrorKind::Upstream),
        ] {
            let error = reconcile_dispatch(dispatch, Ok(absent_state()))
                .expect_err("absent post-dispatch state is indeterminate");
            assert_attached_kind(error, AttachedDiscussionErrorKind::MutationIndeterminate);
        }

        let validation = reconcile_dispatch(Ok(dispatch_response("", 2)), Ok(absent_state()))
            .expect_err("definitive bad input");
        assert!(matches!(validation, AnytypeError::Validation { .. }));

        let authentication = reconcile_dispatch(
            Err(AnytypeError::Auth {
                message: "fixed".to_owned(),
            }),
            Ok(absent_state()),
        )
        .expect_err("definitive permission denial");
        assert!(authentication.is_authentication());
    }

    #[tokio::test]
    async fn injected_dispatch_is_single_and_reconciliation_is_exactly_once() {
        let metrics = Arc::new(AttachedDiscussionMetrics::default());
        let dispatches = Arc::new(AtomicUsize::new(0));
        let reads = Arc::new(AtomicUsize::new(0));
        let result = dispatch_and_reconcile_with(
            metrics.as_ref(),
            {
                let dispatches = Arc::clone(&dispatches);
                let metrics = Arc::clone(&metrics);
                || async move {
                    dispatches.fetch_add(1, AtomicOrdering::SeqCst);
                    metrics.write_dispatches.fetch_add(1, Ordering::Relaxed);
                    Ok(dispatch_response(DISCUSSION, 0))
                }
            },
            {
                let reads = Arc::clone(&reads);
                || async move {
                    reads.fetch_add(1, AtomicOrdering::SeqCst);
                    Ok(attached_state())
                }
            },
        )
        .await
        .expect("single dispatch reconciliation");
        assert_eq!(result.discussion_id(), Some(DISCUSSION));
        assert_eq!(dispatches.load(AtomicOrdering::SeqCst), 1);
        assert_eq!(reads.load(AtomicOrdering::SeqCst), 1);
        assert_eq!(
            metrics.snapshot(),
            AttachedDiscussionMetricsSnapshot {
                write_dispatches: 1,
                reconciliation_attempts: 1,
                ..AttachedDiscussionMetricsSnapshot::default()
            }
        );
    }

    #[tokio::test]
    async fn injected_show_lifecycle_counts_work_and_cleanup_precedes_evidence() {
        let metrics = AttachedDiscussionMetrics::default();
        let view = show_lifecycle_with(
            &metrics,
            || async { ShowAttempt::responded(Ok(discussion())) },
            || async { Ok(()) },
        )
        .await
        .expect("successful lifecycle");
        assert_eq!(view.root_id, DISCUSSION);
        assert_eq!(
            metrics.snapshot(),
            AttachedDiscussionMetricsSnapshot {
                show_attempts: 1,
                accepted_shows: 1,
                close_attempts: 1,
                close_successes: 1,
                ..AttachedDiscussionMetricsSnapshot::default()
            }
        );

        let failed_metrics = AttachedDiscussionMetrics::default();
        let error = show_lifecycle_with(
            &failed_metrics,
            || async {
                ShowAttempt::indeterminate(attached_error_value(
                    AttachedDiscussionErrorKind::Upstream,
                ))
            },
            || async { attached_error(AttachedDiscussionErrorKind::CleanupFailed) },
        )
        .await
        .expect_err("cleanup failure precedence");
        assert_attached_kind(error, AttachedDiscussionErrorKind::CleanupFailed);
        assert_eq!(
            failed_metrics.snapshot(),
            AttachedDiscussionMetricsSnapshot {
                show_attempts: 1,
                close_attempts: 1,
                ..AttachedDiscussionMetricsSnapshot::default()
            }
        );
    }

    #[tokio::test]
    async fn definitive_forbidden_show_preserves_authentication_without_close() {
        let metrics = AttachedDiscussionMetrics::default();
        let close_calls = AtomicUsize::new(0);
        let status = tonic::Status::permission_denied("unretained upstream payload");
        assert!(is_definitive_show_rejection(&status));
        let error = show_lifecycle_with(
            &metrics,
            || async { ShowAttempt::definitive_failure(attached_grpc_status(status)) },
            || async {
                close_calls.fetch_add(1, AtomicOrdering::SeqCst);
                attached_error(AttachedDiscussionErrorKind::CleanupFailed)
            },
        )
        .await
        .expect_err("forbidden show remains an authentication failure");
        assert!(error.is_authentication());
        assert_eq!(close_calls.load(AtomicOrdering::SeqCst), 0);
        assert_eq!(
            metrics.snapshot(),
            AttachedDiscussionMetricsSnapshot {
                show_attempts: 1,
                ..AttachedDiscussionMetricsSnapshot::default()
            }
        );
    }

    #[tokio::test]
    async fn owned_show_lifecycle_finishes_close_after_caller_cancellation() {
        let metrics = Arc::new(AttachedDiscussionMetrics::default());
        let show_started = Arc::new(Notify::new());
        let release_show = Arc::new(Notify::new());
        let close_finished = Arc::new(Notify::new());
        let owned = {
            let metrics = Arc::clone(&metrics);
            let show_started = Arc::clone(&show_started);
            let release_show = Arc::clone(&release_show);
            let close_finished = Arc::clone(&close_finished);
            async move {
                await_owned(async move {
                    show_lifecycle_with(
                        metrics.as_ref(),
                        || async move {
                            show_started.notify_one();
                            release_show.notified().await;
                            ShowAttempt::responded(Ok(discussion()))
                        },
                        || async move {
                            close_finished.notify_one();
                            Ok(())
                        },
                    )
                    .await
                })
                .await
            }
        };
        let caller = tokio::spawn(owned);
        show_started.notified().await;
        caller.abort();
        let _ = caller.await;
        release_show.notify_one();
        tokio::time::timeout(Duration::from_secs(1), close_finished.notified())
            .await
            .expect("detached owned close completed");
        assert_eq!(
            metrics.snapshot(),
            AttachedDiscussionMetricsSnapshot {
                show_attempts: 1,
                accepted_shows: 1,
                close_attempts: 1,
                close_successes: 1,
                ..AttachedDiscussionMetricsSnapshot::default()
            }
        );
    }

    #[tokio::test]
    async fn owned_dispatch_reconciles_once_after_caller_cancellation() {
        let metrics = Arc::new(AttachedDiscussionMetrics::default());
        let dispatch_started = Arc::new(Notify::new());
        let release_dispatch = Arc::new(Notify::new());
        let reconciliation_finished = Arc::new(Notify::new());
        let dispatches = Arc::new(AtomicUsize::new(0));
        let reads = Arc::new(AtomicUsize::new(0));
        let owned = {
            let metrics = Arc::clone(&metrics);
            let dispatch_started = Arc::clone(&dispatch_started);
            let release_dispatch = Arc::clone(&release_dispatch);
            let reconciliation_finished = Arc::clone(&reconciliation_finished);
            let dispatches = Arc::clone(&dispatches);
            let reads = Arc::clone(&reads);
            let dispatch_metrics = Arc::clone(&metrics);
            async move {
                await_owned(async move {
                    dispatch_and_reconcile_with(
                        metrics.as_ref(),
                        || async move {
                            dispatch_metrics
                                .write_dispatches
                                .fetch_add(1, Ordering::Relaxed);
                            dispatches.fetch_add(1, AtomicOrdering::SeqCst);
                            dispatch_started.notify_one();
                            release_dispatch.notified().await;
                            Ok(dispatch_response(DISCUSSION, 0))
                        },
                        || async move {
                            reads.fetch_add(1, AtomicOrdering::SeqCst);
                            reconciliation_finished.notify_one();
                            Ok(attached_state())
                        },
                    )
                    .await
                })
                .await
            }
        };
        let caller = tokio::spawn(owned);
        dispatch_started.notified().await;
        caller.abort();
        let _ = caller.await;
        release_dispatch.notify_one();
        tokio::time::timeout(Duration::from_secs(1), reconciliation_finished.notified())
            .await
            .expect("detached post-dispatch reconciliation completed");
        assert_eq!(dispatches.load(AtomicOrdering::SeqCst), 1);
        assert_eq!(reads.load(AtomicOrdering::SeqCst), 1);
        assert_eq!(
            metrics.snapshot(),
            AttachedDiscussionMetricsSnapshot {
                write_dispatches: 1,
                reconciliation_attempts: 1,
                ..AttachedDiscussionMetricsSnapshot::default()
            }
        );
    }

    #[tokio::test]
    async fn invalid_input_and_deadlines_fail_before_io() {
        let client = AnytypeClient::with_config(crate::client::ClientConfig {
            base_url: Some("http://127.0.0.1:1".to_owned()),
            disable_cache: true,
            ..crate::client::ClientConfig::default()
        })
        .expect("offline validation client");
        let requests = [
            client.attached_discussion("bad space", PARENT),
            client.attached_discussion(SPACE, "bad parent"),
            client
                .attached_discussion(SPACE, PARENT)
                .rpc_timeout(Duration::ZERO),
            client
                .attached_discussion(SPACE, PARENT)
                .rpc_timeout(MAX_ATTACHED_DISCUSSION_RPC_TIMEOUT + Duration::from_nanos(1)),
            client
                .attached_discussion(SPACE, PARENT)
                .operation_timeout(Duration::ZERO),
            client.attached_discussion(SPACE, PARENT).operation_timeout(
                MAX_ATTACHED_DISCUSSION_OPERATION_TIMEOUT + Duration::from_nanos(1),
            ),
        ];
        for request in requests {
            let error = request.get().await.expect_err("invalid request");
            assert!(matches!(error, AnytypeError::Validation { .. }));
        }
        let ensure_error = client
            .attached_discussion(SPACE, "bad parent")
            .ensure()
            .await
            .expect_err("invalid ensure request");
        assert!(matches!(ensure_error, AnytypeError::Validation { .. }));
        assert_eq!(client.http_metrics().logical_operations, 0);
        assert_eq!(
            client.attached_discussion_metrics(),
            AttachedDiscussionMetricsSnapshot::default()
        );

        let expired = OperationBudget {
            deadline: Instant::now(),
        }
        .remaining()
        .expect_err("expired absolute budget");
        assert_attached_kind(expired, AttachedDiscussionErrorKind::OperationDeadline);
    }

    #[test]
    fn attached_state_accessors_are_closed_and_exact() {
        let absent = AttachedDiscussion::Absent {
            space_id: SPACE.to_owned(),
            parent_id: PARENT.to_owned(),
        };
        assert_eq!(absent.space_id(), SPACE);
        assert_eq!(absent.parent_id(), PARENT);
        assert_eq!(absent.discussion_id(), None);
        assert!(!absent.is_attached());

        let attached = AttachedDiscussion::Attached {
            space_id: SPACE.to_owned(),
            parent_id: PARENT.to_owned(),
            discussion_id: DISCUSSION.to_owned(),
        };
        assert_eq!(attached.discussion_id(), Some(DISCUSSION));
        assert!(attached.is_attached());
    }
}
