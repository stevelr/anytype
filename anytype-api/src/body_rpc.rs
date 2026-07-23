// SPDX-FileCopyrightText: 2025-2026 Steve Schoettler
// SPDX-License-Identifier: Apache-2.0

//! Finite, payload-free lifecycle controls for body-block gRPC operations.
//!
//! The public values in this module deliberately contain no generated gRPC
//! types, object identifiers, body content, credentials, or upstream text.

use std::{
    future::{Future, poll_fn},
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use anytype_rpc::{
    anytype::rpc::object::{close as object_close, show as object_show},
    client::AnytypeGrpcClient,
    model,
};
use tokio::time::Instant;
use tonic::{Code, Request, Status};

use crate::{Result, client::AnytypeClient, error::AnytypeError, grpc_util::with_token_request};

/// Hard ceiling for one decoded `ObjectShow` response.
pub const MAX_BODY_SHOW_RESPONSE_BYTES: usize = 4_194_304;
/// Hard ceiling for every decoded non-Show body response, including closes.
pub const MAX_BODY_NON_SHOW_RESPONSE_BYTES: usize = 65_536;
/// Maximum timeout assigned to one body gRPC call.
pub const MAX_BODY_RPC_TIMEOUT: Duration = Duration::from_secs(3);
/// Default end-to-end budget used by source-compatible body calls.
pub const DEFAULT_BODY_OPERATION_TIMEOUT: Duration = Duration::from_secs(30);

/// Closed, payload-free classification for a finite body gRPC lifecycle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum BodyRpcLifecycleErrorKind {
    /// `ObjectShow` did not complete within its finite RPC timeout.
    ShowDeadline,
    /// The decoder rejected `ObjectShow` above its configured byte limit.
    ShowResponseTooLarge,
    /// The matching shown view could not be confirmed closed.
    CleanupFailed,
    /// The shared absolute operation deadline has no remaining budget.
    AbsoluteDeadlineExhausted,
}

impl std::fmt::Display for BodyRpcLifecycleErrorKind {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::ShowDeadline => "show_deadline",
            Self::ShowResponseTooLarge => "show_response_too_large",
            Self::CleanupFailed => "cleanup_failed",
            Self::AbsoluteDeadlineExhausted => "absolute_deadline_exhausted",
        })
    }
}

/// Exact payload-free counters for one or more finite body operations.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct BodyRpcMetricsSnapshot {
    /// `ObjectShow` futures polled.
    pub show_attempts: usize,
    /// Foreground `ObjectClose` futures polled.
    pub foreground_close_attempts: usize,
    /// Foreground closes confirmed by a complete successful response.
    pub foreground_close_confirmed: usize,
    /// Detached fallback `ObjectClose` futures polled.
    pub fallback_close_attempts: usize,
    /// Detached fallback closes confirmed by a complete successful response.
    pub fallback_close_confirmed: usize,
    /// Body-write futures polled.
    pub write_polls: usize,
    /// `ObjectShow` responses rejected by the decoder limit.
    pub show_limit_rejections: usize,
    /// All non-Show responses rejected by the decoder limit.
    pub non_show_limit_rejections: usize,
    /// Close responses rejected by the decoder limit.
    pub close_limit_rejections: usize,
    /// Mutation responses rejected by the decoder limit.
    pub mutation_limit_rejections: usize,
}

#[derive(Debug, Default)]
struct BodyRpcMetricAtoms {
    show_attempts: AtomicUsize,
    foreground_close_attempts: AtomicUsize,
    foreground_close_confirmed: AtomicUsize,
    fallback_close_attempts: AtomicUsize,
    fallback_close_confirmed: AtomicUsize,
    write_polls: AtomicUsize,
    show_limit_rejections: AtomicUsize,
    non_show_limit_rejections: AtomicUsize,
    close_limit_rejections: AtomicUsize,
    mutation_limit_rejections: AtomicUsize,
}

/// Cloneable observer for exact, payload-free body lifecycle counters.
#[derive(Clone, Debug, Default)]
pub struct BodyRpcMetrics(Arc<BodyRpcMetricAtoms>);

impl BodyRpcMetrics {
    /// Returns one consistent-enough monotonic snapshot of all counters.
    ///
    /// Counters can advance while detached cleanup is running. Each field is
    /// exact at the instant it is loaded and never decreases.
    #[must_use]
    pub fn snapshot(&self) -> BodyRpcMetricsSnapshot {
        let load = |value: &AtomicUsize| value.load(Ordering::Acquire);
        BodyRpcMetricsSnapshot {
            show_attempts: load(&self.0.show_attempts),
            foreground_close_attempts: load(&self.0.foreground_close_attempts),
            foreground_close_confirmed: load(&self.0.foreground_close_confirmed),
            fallback_close_attempts: load(&self.0.fallback_close_attempts),
            fallback_close_confirmed: load(&self.0.fallback_close_confirmed),
            write_polls: load(&self.0.write_polls),
            show_limit_rejections: load(&self.0.show_limit_rejections),
            non_show_limit_rejections: load(&self.0.non_show_limit_rejections),
            close_limit_rejections: load(&self.0.close_limit_rejections),
            mutation_limit_rejections: load(&self.0.mutation_limit_rejections),
        }
    }

    fn increment(value: &AtomicUsize) {
        let _ = value.fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
            Some(current.saturating_add(1))
        });
    }

    fn record_show_poll(&self) {
        Self::increment(&self.0.show_attempts);
    }

    fn record_foreground_close_poll(&self) {
        Self::increment(&self.0.foreground_close_attempts);
    }

    fn record_foreground_close_confirmed(&self) {
        Self::increment(&self.0.foreground_close_confirmed);
    }

    fn record_fallback_close_poll(&self) {
        Self::increment(&self.0.fallback_close_attempts);
    }

    fn record_fallback_close_confirmed(&self) {
        Self::increment(&self.0.fallback_close_confirmed);
    }

    pub(crate) fn record_write_poll(&self) {
        Self::increment(&self.0.write_polls);
    }

    fn record_show_limit_rejection(&self) {
        Self::increment(&self.0.show_limit_rejections);
    }

    fn record_close_limit_rejection(&self) {
        Self::increment(&self.0.non_show_limit_rejections);
        Self::increment(&self.0.close_limit_rejections);
    }

    pub(crate) fn record_mutation_limit_rejection(&self) {
        Self::increment(&self.0.non_show_limit_rejections);
        Self::increment(&self.0.mutation_limit_rejections);
    }
}

/// Finite configuration shared by acquisition, reads, writes, verification,
/// and cleanup for one body operation.
#[derive(Clone, Debug)]
pub struct BodyRpcConfig {
    deadline: Instant,
    rpc_timeout: Duration,
    show_response_limit: usize,
    non_show_response_limit: usize,
    metrics: BodyRpcMetrics,
}

impl BodyRpcConfig {
    /// Creates a configuration with one caller-selected absolute deadline.
    #[must_use]
    pub fn new(deadline: Instant) -> Self {
        Self {
            deadline,
            rpc_timeout: MAX_BODY_RPC_TIMEOUT,
            show_response_limit: MAX_BODY_SHOW_RESPONSE_BYTES,
            non_show_response_limit: MAX_BODY_NON_SHOW_RESPONSE_BYTES,
            metrics: BodyRpcMetrics::default(),
        }
    }

    /// Creates a configuration whose absolute deadline is `timeout` from now.
    #[must_use]
    pub fn for_timeout(timeout: Duration) -> Self {
        let now = Instant::now();
        let fallback = now
            .checked_add(DEFAULT_BODY_OPERATION_TIMEOUT)
            .map_or(now, std::convert::identity);
        let deadline = now
            .checked_add(timeout)
            .map_or(fallback, std::convert::identity);
        Self::new(deadline)
    }

    /// Tightens the timeout for each individual RPC.
    #[must_use]
    pub fn rpc_timeout(mut self, timeout: Duration) -> Self {
        self.rpc_timeout = timeout
            .max(Duration::from_nanos(1))
            .min(MAX_BODY_RPC_TIMEOUT);
        self
    }

    /// Tightens decoded response limits; values above hard ceilings clamp.
    #[must_use]
    pub fn response_limits(mut self, show_bytes: usize, non_show_bytes: usize) -> Self {
        self.show_response_limit = show_bytes.min(MAX_BODY_SHOW_RESPONSE_BYTES);
        self.non_show_response_limit = non_show_bytes.min(MAX_BODY_NON_SHOW_RESPONSE_BYTES);
        self
    }

    /// Returns the shared absolute deadline.
    #[must_use]
    pub fn deadline(&self) -> Instant {
        self.deadline
    }

    /// Returns the effective `ObjectShow` decoder limit.
    #[must_use]
    pub fn show_response_limit(&self) -> usize {
        self.show_response_limit
    }

    /// Returns the effective non-Show body decoder limit.
    #[must_use]
    pub fn non_show_response_limit(&self) -> usize {
        self.non_show_response_limit
    }

    /// Returns a cloneable observer for this configuration's counters.
    #[must_use]
    pub fn metrics(&self) -> BodyRpcMetrics {
        self.metrics.clone()
    }

    /// Reuses one metrics observer across multiple independently-deadlined
    /// body operations.
    ///
    /// This is useful for a higher-level workflow that must account for every
    /// show, close, and write poll while still giving each request its own
    /// absolute deadline.
    #[must_use]
    pub fn with_metrics(mut self, metrics: BodyRpcMetrics) -> Self {
        self.metrics = metrics;
        self
    }

    pub(crate) fn remaining(&self) -> Option<Duration> {
        self.deadline.checked_duration_since(Instant::now())
    }

    pub(crate) fn timeout_for_rpc(&self) -> Option<Duration> {
        self.rpc_window().map(|window| window.timeout)
    }

    pub(crate) fn timeout_for(&self, local: Duration) -> Option<Duration> {
        if local.is_zero() {
            return None;
        }
        self.timeout_for_rpc()
            .map(|value| value.min(local))
            .filter(|value| !value.is_zero())
    }

    fn rpc_window(&self) -> Option<RpcWindow> {
        let remaining = self.remaining()?;
        if remaining.is_zero() {
            return None;
        }
        Some(RpcWindow {
            timeout: remaining.min(self.rpc_timeout),
            absolute_deadline_limited: remaining <= self.rpc_timeout,
        })
    }

    pub(crate) fn mutation_commands(
        &self,
        grpc: &AnytypeGrpcClient,
    ) -> anytype_rpc::anytype::ClientCommandsClient<tonic::transport::Channel> {
        grpc.client_commands()
            .max_decoding_message_size(self.non_show_response_limit)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct RpcWindow {
    timeout: Duration,
    absolute_deadline_limited: bool,
}

impl Default for BodyRpcConfig {
    fn default() -> Self {
        Self::for_timeout(DEFAULT_BODY_OPERATION_TIMEOUT)
    }
}

pub(crate) async fn acquire_grpc(
    client: &AnytypeClient,
    config: &BodyRpcConfig,
) -> Result<AnytypeGrpcClient> {
    let remaining = acquisition_timeout(config).ok_or_else(deadline_exhausted)?;
    tokio::time::timeout(remaining, client.grpc_client())
        .await
        .map_err(|_| deadline_exhausted())?
}

pub(crate) fn bounded_body_request<T>(
    request: Request<T>,
    token: &str,
    config: &BodyRpcConfig,
    local_timeout: Duration,
) -> Result<Request<T>> {
    if local_timeout.is_zero() {
        return Err(AnytypeError::Validation {
            message: "body RPC local timeout must be nonzero".to_owned(),
        });
    }
    let timeout = config
        .timeout_for(local_timeout)
        .ok_or_else(deadline_exhausted)?;
    let mut request = with_token_request(request, token)?;
    request.set_timeout(timeout);
    Ok(request)
}

pub(crate) async fn fetch_object_view(
    client: &AnytypeClient,
    space_id: &str,
    object_id: &str,
    config: &BodyRpcConfig,
) -> Result<model::ObjectView> {
    let grpc = acquire_grpc(client, config).await?;
    let mut commands = grpc
        .client_commands()
        .max_decoding_message_size(config.show_response_limit);
    let request = object_show::Request {
        context_id: object_id.to_owned(),
        object_id: object_id.to_owned(),
        space_id: space_id.to_owned(),
        ..Default::default()
    };
    let window = config.rpc_window().ok_or_else(deadline_exhausted)?;
    let mut request = with_token_request(Request::new(request), grpc.token())?;
    request.set_timeout(window.timeout);
    let mut close_owner = ObjectCloseOwner::new(
        grpc,
        space_id.to_owned(),
        object_id.to_owned(),
        config.clone(),
    );
    let show = tokio::time::timeout(
        window.timeout,
        observe_first_poll(commands.object_show(request), || {
            close_owner.mark_show_polled();
            config.metrics.record_show_poll();
        }),
    )
    .await;

    let cleanup = close_owner.foreground_close().await;
    if cleanup.is_ok() {
        close_owner.finish();
    }

    let application = match show {
        Err(_) => Err(show_timeout_error(window)),
        Ok(Err(status))
            if record_response_limit_rejection(
                config,
                &status,
                config.show_response_limit,
                ResponseLimitKind::Show,
            ) =>
        {
            Err(lifecycle(BodyRpcLifecycleErrorKind::ShowResponseTooLarge))
        }
        Ok(Err(_)) => Err(AnytypeError::Other {
            message: "body ObjectShow transport failed".to_owned(),
        }),
        Ok(Ok(response)) => Ok(response.into_inner()),
    };
    let response = cleanup_precedes(cleanup, application)?;
    ensure_show_response_ok(response.error.as_ref())?;
    response.object_view.ok_or_else(|| AnytypeError::Other {
        message: "object show returned no object view".to_owned(),
    })
}

fn ensure_show_response_ok(error: Option<&object_show::response::Error>) -> Result<()> {
    if error.is_some_and(|error| error.code != 0) {
        return Err(AnytypeError::Other {
            message: "body ObjectShow application failed".to_owned(),
        });
    }
    Ok(())
}

#[derive(Debug, Default)]
struct ClosePolicy {
    show_polled: bool,
    foreground_started: bool,
    confirmed: bool,
    finished: bool,
    fallback_started: bool,
}

impl ClosePolicy {
    fn mark_show_polled(&mut self) {
        self.show_polled = true;
    }

    fn begin_foreground(&mut self) -> bool {
        if !self.show_polled || self.foreground_started || self.finished {
            return false;
        }
        self.foreground_started = true;
        true
    }

    fn confirm(&mut self) {
        self.confirmed = true;
    }

    fn finish(&mut self) {
        self.finished = true;
    }

    fn begin_fallback(&mut self) -> bool {
        if !self.show_polled || self.confirmed || self.finished || self.fallback_started {
            return false;
        }
        self.fallback_started = true;
        true
    }
}

#[derive(Debug)]
struct ObjectCloseOwner {
    grpc: AnytypeGrpcClient,
    space_id: String,
    object_id: String,
    config: BodyRpcConfig,
    policy: ClosePolicy,
}

impl ObjectCloseOwner {
    fn new(
        grpc: AnytypeGrpcClient,
        space_id: String,
        object_id: String,
        config: BodyRpcConfig,
    ) -> Self {
        Self {
            grpc,
            space_id,
            object_id,
            config,
            policy: ClosePolicy::default(),
        }
    }

    fn mark_show_polled(&mut self) {
        self.policy.mark_show_polled();
    }

    async fn foreground_close(&mut self) -> Result<()> {
        if !self.policy.begin_foreground() {
            return Err(lifecycle(BodyRpcLifecycleErrorKind::CleanupFailed));
        }
        let result = close_once(
            &self.grpc,
            &self.space_id,
            &self.object_id,
            &self.config,
            ClosePath::Foreground,
        )
        .await;
        if result.is_ok() {
            self.policy.confirm();
        }
        result
    }

    fn finish(&mut self) {
        self.policy.finish();
    }
}

impl Drop for ObjectCloseOwner {
    fn drop(&mut self) {
        let grpc = self.grpc.clone();
        let space_id = self.space_id.clone();
        let object_id = self.object_id.clone();
        let config = self.config.clone();
        spawn_fallback_if_needed(&mut self.policy, async move {
            let _ = close_once(&grpc, &space_id, &object_id, &config, ClosePath::Fallback).await;
        });
    }
}

fn spawn_fallback_if_needed<F>(policy: &mut ClosePolicy, future: F)
where
    F: Future<Output = ()> + Send + 'static,
{
    if !policy.begin_fallback() {
        return;
    }
    let Ok(runtime) = tokio::runtime::Handle::try_current() else {
        return;
    };
    runtime.spawn(future);
}

#[derive(Clone, Copy)]
enum ClosePath {
    Foreground,
    Fallback,
}

async fn close_once(
    grpc: &AnytypeGrpcClient,
    space_id: &str,
    object_id: &str,
    config: &BodyRpcConfig,
    path: ClosePath,
) -> Result<()> {
    let window = config
        .rpc_window()
        .ok_or_else(|| lifecycle(BodyRpcLifecycleErrorKind::CleanupFailed))?;
    let request = object_close::Request {
        context_id: object_id.to_owned(),
        object_id: object_id.to_owned(),
        space_id: space_id.to_owned(),
    };
    let mut request = with_token_request(Request::new(request), grpc.token())?;
    request.set_timeout(window.timeout);
    let mut commands = grpc
        .client_commands()
        .max_decoding_message_size(config.non_show_response_limit);
    let response = tokio::time::timeout(
        window.timeout,
        observe_first_poll(commands.object_close(request), || match path {
            ClosePath::Foreground => config.metrics.record_foreground_close_poll(),
            ClosePath::Fallback => config.metrics.record_fallback_close_poll(),
        }),
    )
    .await
    .map_err(|_| lifecycle(BodyRpcLifecycleErrorKind::CleanupFailed))?
    .map_err(|status| close_transport_error(config, &status))?
    .into_inner();
    if response.error.as_ref().is_some_and(|error| error.code != 0) {
        return Err(lifecycle(BodyRpcLifecycleErrorKind::CleanupFailed));
    }
    match path {
        ClosePath::Foreground => config.metrics.record_foreground_close_confirmed(),
        ClosePath::Fallback => config.metrics.record_fallback_close_confirmed(),
    }
    Ok(())
}

pub(crate) async fn observe_first_poll<F, C>(future: F, callback: C) -> F::Output
where
    F: Future,
    C: FnOnce(),
{
    let mut future = Box::pin(future);
    let mut callback = Some(callback);
    poll_fn(move |context| {
        if let Some(callback) = callback.take() {
            callback();
        }
        Pin::as_mut(&mut future).poll(context)
    })
    .await
}

fn cleanup_precedes<T>(cleanup: Result<()>, application: Result<T>) -> Result<T> {
    cleanup?;
    application
}

fn close_transport_error(config: &BodyRpcConfig, status: &Status) -> AnytypeError {
    let _ = record_response_limit_rejection(
        config,
        status,
        config.non_show_response_limit,
        ResponseLimitKind::Close,
    );
    lifecycle(BodyRpcLifecycleErrorKind::CleanupFailed)
}

fn acquisition_timeout(config: &BodyRpcConfig) -> Option<Duration> {
    config.remaining().filter(|value| !value.is_zero())
}

fn show_timeout_error(window: RpcWindow) -> AnytypeError {
    if window.absolute_deadline_limited {
        deadline_exhausted()
    } else {
        lifecycle(BodyRpcLifecycleErrorKind::ShowDeadline)
    }
}

fn is_decode_limit_status(status: &Status, limit: usize) -> bool {
    if status.code() != Code::OutOfRange {
        return false;
    }
    let prefix = "Error, decoded message length too large: found ";
    let suffix = format!(" bytes, the limit is: {limit} bytes");
    status
        .message()
        .strip_prefix(prefix)
        .and_then(|message| message.strip_suffix(&suffix))
        .is_some_and(|found| found.parse::<usize>().is_ok_and(|value| value > limit))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ResponseLimitKind {
    Show,
    Close,
    Mutation,
}

pub(crate) fn record_response_limit_rejection(
    config: &BodyRpcConfig,
    status: &Status,
    limit: usize,
    kind: ResponseLimitKind,
) -> bool {
    if !is_decode_limit_status(status, limit) {
        return false;
    }
    match kind {
        ResponseLimitKind::Show => config.metrics.record_show_limit_rejection(),
        ResponseLimitKind::Close => config.metrics.record_close_limit_rejection(),
        ResponseLimitKind::Mutation => config.metrics.record_mutation_limit_rejection(),
    }
    true
}

pub(crate) fn deadline_exhausted() -> AnytypeError {
    lifecycle(BodyRpcLifecycleErrorKind::AbsoluteDeadlineExhausted)
}

fn lifecycle(kind: BodyRpcLifecycleErrorKind) -> AnytypeError {
    AnytypeError::BodyRpcLifecycle { kind }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use http_body_util::Full;
    use prost::Message;
    use tonic::codec::{Codec, Streaming};
    use tonic::codegen::http::StatusCode;
    use tonic_prost::ProstCodec;

    #[derive(Clone, PartialEq, Message)]
    struct DecoderFixture {
        #[prost(bytes = "vec", tag = "1")]
        payload: Vec<u8>,
    }

    fn encoded_fixture(target_bytes: usize) -> Vec<u8> {
        for overhead in 1..=10 {
            let Some(payload_bytes) = target_bytes.checked_sub(overhead) else {
                continue;
            };
            let fixture = DecoderFixture {
                payload: vec![0_u8; payload_bytes],
            };
            if fixture.encoded_len() == target_bytes {
                return fixture.encode_to_vec();
            }
        }
        Vec::new()
    }

    async fn decode_fixture(
        encoded_bytes: usize,
        limit: usize,
    ) -> std::result::Result<DecoderFixture, Status> {
        let encoded = encoded_fixture(encoded_bytes);
        assert_eq!(encoded.len(), encoded_bytes);
        let mut frame = Vec::with_capacity(encoded.len() + 5);
        frame.push(0);
        frame.extend_from_slice(&(encoded.len() as u32).to_be_bytes());
        frame.extend_from_slice(&encoded);
        let mut codec = ProstCodec::<DecoderFixture, DecoderFixture>::default();
        let decoder = codec.decoder();
        let mut stream = Streaming::new_response(
            decoder,
            Full::new(Bytes::from(frame)),
            StatusCode::OK,
            None,
            Some(limit),
        );
        stream
            .message()
            .await?
            .ok_or_else(|| Status::internal("missing fixture"))
    }

    #[test]
    fn close_policy_owns_at_most_one_fallback() {
        let mut policy = ClosePolicy::default();
        assert!(!policy.begin_fallback());
        policy.mark_show_polled();
        assert!(policy.begin_fallback());
        assert!(!policy.begin_fallback());
    }

    #[test]
    fn close_policy_distinguishes_confirmation_and_normal_failure() {
        let mut confirmed = ClosePolicy::default();
        confirmed.mark_show_polled();
        assert!(confirmed.begin_foreground());
        confirmed.confirm();
        assert!(!confirmed.begin_fallback());

        let mut failed = ClosePolicy::default();
        failed.mark_show_polled();
        assert!(failed.begin_foreground());
        assert!(failed.begin_fallback());
        assert!(!failed.begin_fallback());

        let mut completed_failure = ClosePolicy::default();
        completed_failure.mark_show_polled();
        assert!(completed_failure.begin_foreground());
        completed_failure.finish();
        assert!(!completed_failure.begin_fallback());
    }

    #[tokio::test]
    async fn cancellation_drop_runs_one_async_fallback_and_counts_it() {
        struct DropProbe {
            policy: ClosePolicy,
            metrics: BodyRpcMetrics,
            completed: Arc<tokio::sync::Notify>,
        }

        impl Drop for DropProbe {
            fn drop(&mut self) {
                let metrics = self.metrics.clone();
                let completed = Arc::clone(&self.completed);
                spawn_fallback_if_needed(&mut self.policy, async move {
                    metrics.record_fallback_close_poll();
                    tokio::task::yield_now().await;
                    metrics.record_fallback_close_confirmed();
                    completed.notify_one();
                });
            }
        }

        let metrics = BodyRpcMetrics::default();
        let completed = Arc::new(tokio::sync::Notify::new());
        let notified = completed.notified();
        {
            let mut probe = DropProbe {
                policy: ClosePolicy::default(),
                metrics: metrics.clone(),
                completed: Arc::clone(&completed),
            };
            probe.policy.mark_show_polled();
            assert!(probe.policy.begin_foreground());
        }
        tokio::time::timeout(Duration::from_secs(1), notified)
            .await
            .expect("fallback completion");
        let snapshot = metrics.snapshot();
        assert_eq!(snapshot.fallback_close_attempts, 1);
        assert_eq!(snapshot.fallback_close_confirmed, 1);
    }

    #[test]
    fn cleanup_failure_precedes_show_failure() {
        let cleanup = Err::<(), _>(lifecycle(BodyRpcLifecycleErrorKind::CleanupFailed));
        let application = Err::<(), _>(lifecycle(BodyRpcLifecycleErrorKind::ShowResponseTooLarge));
        let error = cleanup_precedes(cleanup, application).expect_err("cleanup must win");
        assert!(matches!(
            error,
            AnytypeError::BodyRpcLifecycle {
                kind: BodyRpcLifecycleErrorKind::CleanupFailed
            }
        ));
    }

    #[test]
    fn show_application_error_redacts_upstream_code_and_description() {
        let upstream = object_show::response::Error {
            code: 927,
            description: "adversarial-secret-description".to_owned(),
        };
        let error = ensure_show_response_ok(Some(&upstream))
            .expect_err("nonzero ObjectShow application error must fail");
        let display = error.to_string();
        let debug = format!("{error:?}");
        assert!(matches!(
            error,
            AnytypeError::Other { ref message }
                if message == "body ObjectShow application failed"
        ));
        assert_eq!(display, "Anytype error (details redacted)");
        assert!(!display.contains("927"));
        assert!(!display.contains("adversarial-secret-description"));
        assert!(!debug.contains("927"));
        assert!(!debug.contains("adversarial-secret-description"));
    }

    #[test]
    fn unconfirmed_close_precedes_show_application_error() {
        let upstream = object_show::response::Error {
            code: 404,
            description: "missing-object-payload".to_owned(),
        };
        let application = ensure_show_response_ok(Some(&upstream));
        let cleanup = Err::<(), _>(lifecycle(BodyRpcLifecycleErrorKind::CleanupFailed));
        let error = cleanup_precedes(cleanup, application)
            .expect_err("unconfirmed close must take precedence over Show application failure");
        let debug = format!("{error:?}");
        assert!(matches!(
            error,
            AnytypeError::BodyRpcLifecycle {
                kind: BodyRpcLifecycleErrorKind::CleanupFailed
            }
        ));
        assert!(!debug.contains("404"));
        assert!(!debug.contains("missing-object-payload"));
    }

    #[test]
    fn exact_limit_status_is_classified_without_retaining_payload() {
        let limit = MAX_BODY_NON_SHOW_RESPONSE_BYTES;
        let exact = Status::out_of_range(format!(
            "Error, decoded message length too large: found {limit} bytes, the limit is: {limit} bytes"
        ));
        let over = Status::out_of_range(format!(
            "Error, decoded message length too large: found {} bytes, the limit is: {limit} bytes",
            limit + 1
        ));
        assert!(!is_decode_limit_status(&exact, limit));
        assert!(is_decode_limit_status(&over, limit));
        assert!(!is_decode_limit_status(
            &Status::resource_exhausted("unrelated"),
            limit
        ));
    }

    #[tokio::test]
    async fn tonic_decoder_caps_every_body_response_path_at_exact_bytes() {
        let config = BodyRpcConfig::for_timeout(Duration::from_secs(5));
        let cases = [
            (
                MAX_BODY_SHOW_RESPONSE_BYTES,
                ResponseLimitKind::Show,
                "show",
            ),
            (
                MAX_BODY_NON_SHOW_RESPONSE_BYTES,
                ResponseLimitKind::Mutation,
                "mutation",
            ),
            (
                MAX_BODY_NON_SHOW_RESPONSE_BYTES,
                ResponseLimitKind::Close,
                "foreground_close",
            ),
            (
                MAX_BODY_NON_SHOW_RESPONSE_BYTES,
                ResponseLimitKind::Close,
                "fallback_close",
            ),
        ];
        for (limit, kind, path) in cases {
            let exact = decode_fixture(limit, limit).await;
            assert!(exact.is_ok(), "{path} exact limit must decode");
            let over = decode_fixture(limit + 1, limit)
                .await
                .expect_err("one-over fixture must be decoder-rejected");
            if kind == ResponseLimitKind::Close {
                let cleanup_error = close_transport_error(&config, &over);
                let error = cleanup_precedes(
                    Err(cleanup_error),
                    Err::<(), _>(lifecycle(BodyRpcLifecycleErrorKind::ShowResponseTooLarge)),
                )
                .expect_err("close overrun must take precedence");
                assert!(matches!(
                    error,
                    AnytypeError::BodyRpcLifecycle {
                        kind: BodyRpcLifecycleErrorKind::CleanupFailed
                    }
                ));
            } else {
                assert!(
                    record_response_limit_rejection(&config, &over, limit, kind),
                    "{path} one-over status must be classified"
                );
            }
        }
        let metrics = config.metrics().snapshot();
        assert_eq!(metrics.show_limit_rejections, 1);
        assert_eq!(metrics.mutation_limit_rejections, 1);
        assert_eq!(metrics.close_limit_rejections, 2);
        assert_eq!(metrics.non_show_limit_rejections, 3);
    }

    #[tokio::test(start_paused = true)]
    async fn timeout_windows_distinguish_local_and_absolute_deadlines() {
        let local = BodyRpcConfig::new(Instant::now() + Duration::from_secs(30));
        assert_eq!(acquisition_timeout(&local), Some(Duration::from_secs(30)));
        let local_window = local.rpc_window().expect("local window");
        assert_eq!(local_window.timeout, MAX_BODY_RPC_TIMEOUT);
        assert!(!local_window.absolute_deadline_limited);
        assert!(matches!(
            show_timeout_error(local_window),
            AnytypeError::BodyRpcLifecycle {
                kind: BodyRpcLifecycleErrorKind::ShowDeadline
            }
        ));

        let absolute = BodyRpcConfig::new(Instant::now() + Duration::from_secs(2));
        let absolute_window = absolute.rpc_window().expect("absolute window");
        assert_eq!(absolute_window.timeout, Duration::from_secs(2));
        assert!(absolute_window.absolute_deadline_limited);
        assert!(matches!(
            show_timeout_error(absolute_window),
            AnytypeError::BodyRpcLifecycle {
                kind: BodyRpcLifecycleErrorKind::AbsoluteDeadlineExhausted
            }
        ));
    }

    #[test]
    fn metrics_are_exact_and_payload_free() {
        let metrics = BodyRpcMetrics::default();
        metrics.record_show_poll();
        metrics.record_foreground_close_poll();
        metrics.record_foreground_close_confirmed();
        metrics.record_fallback_close_poll();
        metrics.record_fallback_close_confirmed();
        metrics.record_write_poll();
        metrics.record_show_limit_rejection();
        metrics.record_close_limit_rejection();
        metrics.record_mutation_limit_rejection();
        assert_eq!(
            metrics.snapshot(),
            BodyRpcMetricsSnapshot {
                show_attempts: 1,
                foreground_close_attempts: 1,
                foreground_close_confirmed: 1,
                fallback_close_attempts: 1,
                fallback_close_confirmed: 1,
                write_polls: 1,
                show_limit_rejections: 1,
                non_show_limit_rejections: 2,
                close_limit_rejections: 1,
                mutation_limit_rejections: 1,
            }
        );
    }

    #[test]
    fn independently_deadlined_configs_share_one_metrics_observer() {
        let metrics = BodyRpcMetrics::default();
        let first =
            BodyRpcConfig::for_timeout(Duration::from_secs(1)).with_metrics(metrics.clone());
        let second =
            BodyRpcConfig::for_timeout(Duration::from_secs(2)).with_metrics(metrics.clone());

        first.metrics().record_show_poll();
        second.metrics().record_write_poll();

        let observed = metrics.snapshot();
        assert_eq!(observed.show_attempts, 1);
        assert_eq!(observed.write_polls, 1);
        assert_eq!(first.metrics().snapshot(), observed);
        assert_eq!(second.metrics().snapshot(), observed);
    }

    #[test]
    fn response_limits_clamp_and_accept_exact_boundaries() {
        let config = BodyRpcConfig::for_timeout(Duration::from_secs(1)).response_limits(
            MAX_BODY_SHOW_RESPONSE_BYTES + 1,
            MAX_BODY_NON_SHOW_RESPONSE_BYTES + 1,
        );
        assert_eq!(config.show_response_limit(), MAX_BODY_SHOW_RESPONSE_BYTES);
        assert_eq!(
            config.non_show_response_limit(),
            MAX_BODY_NON_SHOW_RESPONSE_BYTES
        );

        let overflow_safe = BodyRpcConfig::for_timeout(Duration::MAX);
        assert!(overflow_safe.deadline() > Instant::now());
        let nonzero =
            BodyRpcConfig::for_timeout(Duration::from_secs(1)).rpc_timeout(Duration::ZERO);
        assert!(
            nonzero
                .timeout_for_rpc()
                .is_some_and(|value| !value.is_zero())
        );
    }
}
