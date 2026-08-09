// any-mcp - bounded, workflow-oriented MCP server for Anytype
//
// SPDX-FileCopyrightText: 2026 Steve Schoettler
// SPDX-License-Identifier: Apache-2.0

//! Private, process-generation state for remote artifact staging.
//!
//! Visible handles are bearer credentials. This module retains only keyed
//! digests, uses monotonic expiry, and keeps completed bytes behind retained
//! file handles rather than reopening caller-influenced paths.

use std::{
    collections::{BTreeMap, BTreeSet, HashMap, VecDeque},
    convert::Infallible,
    fmt,
    fs::File,
    future::Future,
    io::{self, Read, Seek, SeekFrom, Write},
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering},
    },
    task::{Context, Poll},
    time::{Duration, Instant},
};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use bytes::Bytes;
use chrono::{DateTime, Utc};
use futures::Stream;
use http_body_util::{BodyExt, Full, StreamBody, combinators::UnsyncBoxBody};
use hyper::{
    Method, Request, Response, StatusCode,
    body::{Frame, Incoming},
    header::{
        AUTHORIZATION, CONTENT_LENGTH, CONTENT_RANGE, CONTENT_TYPE, HOST, ORIGIN, RANGE,
        TRANSFER_ENCODING,
    },
    service::service_fn,
};
use hyper_util::rt::{TokioIo, TokioTimer};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio_util::{io::ReaderStream, sync::CancellationToken};

use crate::{
    artifact_config::{ArtifactLimits, StagingConfig},
    artifact_roots::{
        AnchoredImport, PositionalReader, RootAccessErrorKind, RootRegistry, StagingDirectory,
        StagingFileIdentity, StagingInventory, StagingPayload,
    },
    domain::{EntityId, SpaceId},
};

const HANDLE_VERSION: u8 = 1;
const RECORD_BYTES: usize = 16;
const SECRET_BYTES: usize = 32;
const CHECKSUM_BYTES: usize = 8;
const HANDLE_BYTES: usize = 1 + RECORD_BYTES + SECRET_BYTES + CHECKSUM_BYTES;
const DURABLE_RECORD_VERSION: u8 = 1;

type StagingBody = UnsyncBoxBody<Bytes, io::Error>;

/// Fixed staging guidance returned when the startup policy disabled staging.
pub const STAGING_REQUIRED_GUIDANCE: &str =
    "Remote artifact staging is disabled. Enable it in the selected any-mcp TOML config.";

/// Direction of one private staging record.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StageDirection {
    Import,
    Export,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum DurableDirection {
    Import,
    Export,
}

impl From<StageDirection> for DurableDirection {
    fn from(direction: StageDirection) -> Self {
        match direction {
            StageDirection::Import => Self::Import,
            StageDirection::Export => Self::Export,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum DurableStageState {
    Allocated,
    Receiving,
    Ready,
    Reconciliation,
    Available,
    Consumed,
    CleanupPending,
    PublicationIndeterminate,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct DurableFileIdentity {
    volume: u64,
    file: u64,
}

impl From<StagingFileIdentity> for DurableFileIdentity {
    fn from(identity: StagingFileIdentity) -> Self {
        Self {
            volume: identity.volume,
            file: identity.file,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct DurableStageRecord {
    format_version: u8,
    generation: String,
    record_id: String,
    bearer_digest: String,
    direction: DurableDirection,
    state: DurableStageState,
    created_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
    space_id: String,
    size_bytes: u64,
    media_type: Option<String>,
    expected_sha256: Option<String>,
    observed_sha256: Option<String>,
    committed_offset: u64,
    payload_identity: Option<DurableFileIdentity>,
    operation_fingerprint: Option<String>,
    candidate_id: Option<String>,
    candidate_cleanup: Option<String>,
    cleanup_evidence: Option<String>,
    uncertainty: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct DurableTombstone {
    format_version: u8,
    record_id: String,
    payload_identity: Option<DurableFileIdentity>,
    record_identity: DurableFileIdentity,
}

struct DurableRecordOwner {
    document: DurableStageRecord,
    source: AnchoredImport,
}

struct RecoveredDurableRecord {
    document: DurableStageRecord,
    record_source: AnchoredImport,
    payload_source: AnchoredImport,
}

struct ReconciliationOutcome {
    cleaned: usize,
    retained: Vec<RecoveredDurableRecord>,
}

impl fmt::Debug for DurableRecordOwner {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DurableRecordOwner")
            .field("state", &self.document.state)
            .finish_non_exhaustive()
    }
}

/// Bounded allocation metadata returned to an MCP tool.
#[derive(Clone, Debug)]
pub(crate) struct StageAllocation {
    pub(crate) record: String,
    pub(crate) handle: String,
    pub(crate) url: String,
    pub(crate) expires_at: DateTime<Utc>,
    pub(crate) size_bytes: u64,
}

/// Authenticated, bounded staging metadata.
#[derive(Clone, Debug)]
pub(crate) struct StageStatus {
    pub(crate) direction: StageDirection,
    pub(crate) state: &'static str,
    pub(crate) offset: u64,
    pub(crate) size_bytes: u64,
    pub(crate) sha256: Option<String>,
    pub(crate) media_type: Option<String>,
    pub(crate) expires_at: DateTime<Utc>,
}

/// Retained staged source used by one Anytype import attempt.
pub(crate) struct StageSource {
    pub(crate) file: File,
    pub(crate) length: u64,
    pub(crate) sha256: String,
    pub(crate) media_type: Option<String>,
    record: [u8; RECORD_BYTES],
    operation: [u8; 32],
    restore_ready_on_drop: bool,
    record_owner: Arc<StageRecord>,
    #[cfg(test)]
    fail_reader_clone: bool,
    lease: tokio::sync::OwnedMutexGuard<RecordState>,
}

/// Retained staged export protected from shared-cursor concurrent reads.
struct StageExportSource {
    file: File,
    lease: tokio::sync::OwnedMutexGuard<RecordState>,
}

struct VerifiedExportStream {
    reader: ReaderStream<PositionalReader>,
    lease: tokio::sync::OwnedMutexGuard<RecordState>,
    deadline: Pin<Box<tokio::time::Sleep>>,
    final_verification_complete: bool,
    terminated: bool,
}

impl Stream for VerifiedExportStream {
    type Item = Result<Frame<Bytes>, io::Error>;

    fn poll_next(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let stream = self.get_mut();
        if stream.terminated {
            return Poll::Ready(None);
        }
        if stream.deadline.as_mut().poll(context).is_ready() {
            stream.terminated = true;
            return Poll::Ready(Some(Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "artifact transfer deadline exceeded",
            ))));
        }
        match Pin::new(&mut stream.reader).poll_next(context) {
            Poll::Ready(Some(Ok(bytes))) => Poll::Ready(Some(Ok(Frame::data(bytes)))),
            Poll::Ready(Some(Err(error))) => Poll::Ready(Some(Err(error))),
            Poll::Ready(None) if !stream.final_verification_complete => {
                stream.final_verification_complete = true;
                let verified = match &*stream.lease {
                    RecordState::Available { source, .. } => source.verify_unchanged().is_ok(),
                    _ => false,
                };
                if verified {
                    stream.terminated = true;
                    Poll::Ready(None)
                } else {
                    stream.terminated = true;
                    Poll::Ready(Some(Err(io::Error::other(
                        "retained artifact changed during transfer",
                    ))))
                }
            }
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        }
    }
}

/// Exclusive write lease for one receiving staging record.
pub(crate) struct StageWriteLease {
    destination: Option<StagingPayload>,
    pub(crate) offset: u64,
    pub(crate) size_bytes: u64,
    record: Arc<StageRecord>,
    cleanup_active: bool,
}

impl fmt::Debug for StageWriteLease {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StageWriteLease")
            .field("offset", &self.offset)
            .field("size_bytes", &self.size_bytes)
            .finish_non_exhaustive()
    }
}

impl StageWriteLease {
    pub(crate) fn take_destination(&mut self) -> Result<StagingPayload, StagingError> {
        self.destination.take().ok_or(StagingError::Conflict)
    }
}

impl Drop for StageWriteLease {
    fn drop(&mut self) {
        if self.cleanup_active {
            self.record.cleanup_blocked.store(false, Ordering::Release);
        }
    }
}

impl fmt::Debug for StageSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StageSource")
            .field("length", &self.length)
            .field("media_type_configured", &self.media_type.is_some())
            .finish_non_exhaustive()
    }
}

impl StageSource {
    pub(crate) fn record(&self) -> String {
        record_hex(&self.record)
    }

    pub(crate) fn try_clone_reader(&self) -> Result<File, StagingError> {
        #[cfg(test)]
        if self.fail_reader_clone {
            return Err(StagingError::Upstream);
        }
        let mut reader = self.file.try_clone().map_err(|_| StagingError::Upstream)?;
        reader
            .seek(SeekFrom::Start(0))
            .map_err(|_| StagingError::Upstream)?;
        Ok(reader)
    }
}

/// Reconciliation metadata retained after staged import authority has been
/// acquired.  This is intentionally content-free: it permits same-operation
/// idempotency replay to authenticate the retained identity without handing a
/// second caller a readable source lease.
#[derive(Clone, Debug)]
pub(crate) struct RetainedStageImport {
    pub(crate) length: u64,
    pub(crate) sha256: String,
    pub(crate) media_type: Option<String>,
    pub(crate) record: String,
}

impl Drop for StageSource {
    fn drop(&mut self) {
        // Before POST dispatch, the lease itself is the rollback guard: a
        // cancelled or panicked settlement restores the one-use authority.
        // Once dispatch starts, reconciliation remains bound to the operation
        // until a definitive rejection restores it explicitly or verification
        // consumes it.
        if self.restore_ready_on_drop
            && let RecordState::Reconciliation { import, operation } = &*self.lease
            && operation == &self.operation
        {
            *self.lease = RecordState::Ready {
                import: Arc::clone(import),
            };
        }
    }
}

/// Fixed, credential-free staging failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StagingError {
    Disabled,
    InvalidPolicy,
    Reconciliation,
    NotFound,
    BadRequest,
    Conflict,
    Bounded,
    Timeout,
    Upstream,
    Indeterminate,
}

impl fmt::Display for StagingError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("artifact staging operation failed")
    }
}

impl std::error::Error for StagingError {}

#[derive(Debug)]
enum RecordState {
    Receiving {
        destination: Option<StagingPayload>,
        offset: u64,
    },
    Ready {
        import: Arc<RetainedImport>,
    },
    Reconciliation {
        import: Arc<RetainedImport>,
        operation: [u8; 32],
    },
    Available {
        source: AnchoredImport,
        sha256: String,
    },
    PublicationIndeterminate {
        completion: Arc<PublicationCompletion>,
    },
    CleanupPending {
        destination: Option<StagingPayload>,
        source: Option<AnchoredImport>,
        pathname_cleanup_unsafe: bool,
    },
    Consumed {
        import: Arc<RetainedImport>,
        operation: [u8; 32],
    },
}

#[derive(Debug)]
struct RetainedImport {
    source: AnchoredImport,
    sha256: String,
}

#[derive(Debug)]
struct PublicationCompletion {
    completed: AtomicU8,
    cleanup_blocked: Arc<AtomicBool>,
    notify: tokio::sync::Notify,
}

impl PublicationCompletion {
    const WORKER_DONE: u8 = 1;
    const OWNER_DONE: u8 = 2;
    const ALL_DONE: u8 = Self::WORKER_DONE | Self::OWNER_DONE;

    fn new(cleanup_blocked: Arc<AtomicBool>) -> Arc<Self> {
        Arc::new(Self {
            completed: AtomicU8::new(0),
            cleanup_blocked,
            notify: tokio::sync::Notify::new(),
        })
    }

    fn settled(&self) -> bool {
        self.completed.load(Ordering::Acquire) == Self::ALL_DONE
    }

    fn mark_done(&self, completed: u8) {
        let prior = self.completed.fetch_or(completed, Ordering::AcqRel);
        if prior | completed == Self::ALL_DONE {
            self.cleanup_blocked.store(false, Ordering::Release);
        }
        self.notify.notify_waiters();
    }

    #[cfg(test)]
    async fn wait(&self) {
        loop {
            let notified = self.notify.notified();
            if self.settled() {
                return;
            }
            notified.await;
        }
    }
}

struct PublicationCompletionGuard(Arc<PublicationCompletion>);

impl Drop for PublicationCompletionGuard {
    fn drop(&mut self) {
        self.0.mark_done(PublicationCompletion::WORKER_DONE);
    }
}

struct PublicationOwnerGuard(Arc<PublicationCompletion>);

impl Drop for PublicationOwnerGuard {
    fn drop(&mut self) {
        self.0.mark_done(PublicationCompletion::OWNER_DONE);
    }
}

/// Releases a cleanup claim even when its coordinator is cancelled or panics.
///
/// The record remains retained and charged until a coordinator proves removal.
struct CleanupClaimGuard(Arc<AtomicBool>);

impl Drop for CleanupClaimGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

#[cfg(test)]
#[derive(Clone)]
struct PublicationTestPause {
    record_name: String,
    entered: Arc<std::sync::Barrier>,
    release: Arc<std::sync::Barrier>,
}

#[cfg(test)]
static PUBLICATION_TEST_PAUSE: std::sync::OnceLock<std::sync::Mutex<Option<PublicationTestPause>>> =
    std::sync::OnceLock::new();
#[cfg(test)]
static PUBLICATION_TEST_SERIAL: std::sync::OnceLock<tokio::sync::Mutex<()>> =
    std::sync::OnceLock::new();

#[cfg(test)]
#[derive(Clone)]
struct CleanupTestPause {
    record_name: String,
    entered: Arc<std::sync::Barrier>,
    release: Arc<std::sync::Barrier>,
}

#[cfg(test)]
static CLEANUP_TEST_PAUSE: std::sync::OnceLock<std::sync::Mutex<Option<CleanupTestPause>>> =
    std::sync::OnceLock::new();
#[cfg(test)]
static CLEANUP_TEST_SERIAL: std::sync::OnceLock<tokio::sync::Mutex<()>> =
    std::sync::OnceLock::new();

async fn begin_publication(
    lease: &mut StageWriteLease,
) -> Result<Arc<PublicationCompletion>, StagingError> {
    let mut state = lease.record.state.lock().await;
    let RecordState::Receiving { destination, .. } = &*state else {
        return Err(StagingError::Conflict);
    };
    if destination.is_some() {
        return Err(StagingError::Conflict);
    }
    let completion = PublicationCompletion::new(Arc::clone(&lease.record.cleanup_blocked));
    *state = RecordState::PublicationIndeterminate {
        completion: Arc::clone(&completion),
    };
    lease.cleanup_active = false;
    Ok(completion)
}

#[cfg(test)]
fn pause_publication_for_test(record_name: &str) {
    let pause = PUBLICATION_TEST_PAUSE
        .get_or_init(|| std::sync::Mutex::new(None))
        .lock()
        .ok()
        .and_then(|pause| pause.as_ref().cloned())
        .filter(|pause| pause.record_name == record_name);
    if let Some(pause) = pause {
        pause.entered.wait();
        pause.release.wait();
    }
}

#[cfg(test)]
fn pause_cleanup_for_test(record_name: &str) {
    let pause = CLEANUP_TEST_PAUSE
        .get_or_init(|| std::sync::Mutex::new(None))
        .lock()
        .ok()
        .and_then(|pause| pause.as_ref().cloned())
        .filter(|pause| pause.record_name == record_name);
    if let Some(pause) = pause {
        pause.entered.wait();
        pause.release.wait();
    }
}

#[derive(Debug)]
struct StageRecord {
    record_name: String,
    bearer_digest: [u8; 32],
    direction: StageDirection,
    space_id: SpaceId,
    size_bytes: u64,
    media_type: Option<String>,
    expected_sha256: Option<String>,
    expires: Instant,
    expires_at: DateTime<Utc>,
    cleanup_blocked: Arc<AtomicBool>,
    durable: tokio::sync::Mutex<DurableRecordOwner>,
    tombstone: tokio::sync::Mutex<Option<AnchoredImport>>,
    state: Arc<tokio::sync::Mutex<RecordState>>,
}

#[derive(Debug)]
struct StagingState {
    directory: StagingDirectory,
    generation_key: [u8; 32],
    generation: String,
    public_base_url: String,
    limits: ArtifactLimits,
    records: tokio::sync::RwLock<HashMap<[u8; RECORD_BYTES], Arc<StageRecord>>>,
    allowed_hosts: Vec<String>,
    request_permits: Arc<tokio::sync::Semaphore>,
    connection_permits: Arc<tokio::sync::Semaphore>,
    rate_window: tokio::sync::Mutex<VecDeque<Instant>>,
    active: AtomicBool,
    durability_uncertain: AtomicBool,
    task_active: AtomicUsize,
    task_notify: tokio::sync::Notify,
    shutdown: CancellationToken,
}

struct StagingTaskGuard {
    state: Arc<StagingState>,
    registered: bool,
    completed: bool,
    fatal_if_incomplete: bool,
}

impl StagingTaskGuard {
    fn complete(&mut self) {
        self.completed = true;
    }
}

impl Drop for StagingTaskGuard {
    fn drop(&mut self) {
        if self.registered && !self.completed && self.fatal_if_incomplete {
            self.state
                .durability_uncertain
                .store(true, Ordering::Release);
            self.state.active.store(false, Ordering::Release);
            self.state.shutdown.cancel();
        }
        if self.registered {
            let _ = self.state.task_active.fetch_update(
                Ordering::AcqRel,
                Ordering::Acquire,
                |active| active.checked_sub(1),
            );
        }
        self.state.task_notify.notify_waiters();
    }
}

/// Activated private staging authority for one process generation.
#[derive(Clone, Debug)]
pub(crate) struct ArtifactStaging {
    state: Arc<StagingState>,
}

#[derive(Clone, Copy)]
struct ParsedHandle {
    record: [u8; RECORD_BYTES],
    secret: [u8; SECRET_BYTES],
}

fn digest(parts: &[&[u8]]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update((part.len() as u64).to_be_bytes());
        hasher.update(part);
    }
    hasher.finalize().into()
}

fn staging_policy_digest(
    config: &StagingConfig,
    limits: &ArtifactLimits,
    local_roots: &RootRegistry,
    staging_identity: StagingFileIdentity,
) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(b"any-mcp/artifact-staging-policy/v1\0");
    hasher.update([u8::from(config.enabled)]);
    match config.bind.ip() {
        std::net::IpAddr::V4(address) => {
            hasher.update([4]);
            hasher.update(address.octets());
        }
        std::net::IpAddr::V6(address) => {
            hasher.update([6]);
            hasher.update(address.octets());
        }
    }
    hasher.update(config.bind.port().to_be_bytes());
    hasher.update(staging_identity.volume.to_be_bytes());
    hasher.update(staging_identity.file.to_be_bytes());
    hasher.update(local_roots.staging_policy_digest());
    for value in [
        limits.artifact_bytes,
        limits.transfer_chunk_bytes,
        limits.staging_total_bytes,
        u64::try_from(limits.staging_ttl.as_nanos()).unwrap_or(u64::MAX),
        u64::try_from(limits.staging_header_timeout.as_nanos()).unwrap_or(u64::MAX),
        u64::try_from(limits.staging_no_progress_timeout.as_nanos()).unwrap_or(u64::MAX),
        u64::try_from(limits.operation_timeout.as_nanos()).unwrap_or(u64::MAX),
        limits.markdown_bytes,
        limits.validator_total_input_bytes,
    ] {
        hasher.update(value.to_be_bytes());
    }
    for value in [
        limits.staging_entries,
        limits.staging_connections,
        limits.staging_requests,
        limits.staging_header_bytes,
        limits.receipt_bytes,
        limits.cleanup_batch,
        limits.discovery_rows,
        limits.markdown_chars,
        limits.validator_processes,
    ] {
        hasher.update(u128::try_from(value).unwrap_or(u128::MAX).to_be_bytes());
    }
    hasher.update(limits.staging_requests_per_minute.to_be_bytes());
    hasher.finalize().into()
}

fn constant_time_equal(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0_u8, |difference, (left, right)| {
            difference | (left ^ right)
        })
        == 0
}

fn record_hex(record: &[u8; RECORD_BYTES]) -> String {
    let mut encoded = String::with_capacity(RECORD_BYTES * 2);
    for byte in record {
        let _ = std::fmt::Write::write_fmt(&mut encoded, format_args!("{byte:02x}"));
    }
    encoded
}

fn bytes_hex(bytes: &[u8]) -> String {
    let mut encoded = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        let _ = std::fmt::Write::write_fmt(&mut encoded, format_args!("{byte:02x}"));
    }
    encoded
}

fn decode_hex_array<const N: usize>(value: &str) -> Option<[u8; N]> {
    if value.len() != N.saturating_mul(2) {
        return None;
    }
    let mut decoded = [0_u8; N];
    for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
        let high = hex_nibble(*pair.first()?)?;
        let low = hex_nibble(*pair.get(1)?)?;
        *decoded.get_mut(index)? = high.checked_mul(16)?.checked_add(low)?;
    }
    Some(decoded)
}

fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    }
}

fn durable_json<T: Serialize>(value: &T) -> Result<Vec<u8>, StagingError> {
    let bytes = serde_json::to_vec(value).map_err(|_| StagingError::Reconciliation)?;
    if bytes.len() as u64 > crate::artifact_roots::STAGING_STATE_BYTES {
        return Err(StagingError::Reconciliation);
    }
    Ok(bytes)
}

fn parse_record(
    mut file: crate::artifact_roots::StagingInventoryFile,
) -> Result<(DurableStageRecord, AnchoredImport), StagingError> {
    let document: DurableStageRecord =
        serde_json::from_reader(file.source.reader()).map_err(|_| StagingError::Reconciliation)?;
    if file.name != format!("{}.json", document.record_id)
        || document.format_version != DURABLE_RECORD_VERSION
        || document.record_id.len() != 32
        || !document
            .record_id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
        || document.generation.len() != 64
        || !document
            .generation
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
        || document.bearer_digest.len() != 64
        || !document
            .bearer_digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
        || document.committed_offset > document.size_bytes
        || document
            .expected_sha256
            .as_ref()
            .is_some_and(|value| !valid_sha256(value))
        || document
            .observed_sha256
            .as_ref()
            .is_some_and(|value| !valid_sha256(value))
        || document
            .operation_fingerprint
            .as_ref()
            .is_some_and(|value| !valid_sha256(value))
        || document
            .candidate_id
            .as_ref()
            .is_some_and(|value| EntityId::new(value.clone()).is_err())
        || document
            .media_type
            .as_ref()
            .is_some_and(|value| value.len() > 255)
        || SpaceId::new(&document.space_id).is_err()
    {
        return Err(StagingError::Reconciliation);
    }
    Ok((document, file.source))
}

/// Validates process-controlled invariants of one durable record: field
/// coupling per state and a non-negative lifetime. Deliberately excludes wall
/// clocks and configured limits, so a stepped clock or a policy change can
/// never invalidate a record the process itself wrote.
fn durable_shape_valid(document: &DurableStageRecord) -> bool {
    let lifetime = document
        .expires_at
        .signed_duration_since(document.created_at)
        .to_std();
    if lifetime.is_err()
        || (document.direction == DurableDirection::Import && document.size_bytes == 0)
        || (document.direction == DurableDirection::Export && document.expected_sha256.is_some())
    {
        return false;
    }

    let offset_complete = document.committed_offset == document.size_bytes;
    let has_payload = document.payload_identity.is_some();
    let has_observed = document.observed_sha256.is_some();
    let has_operation = document.operation_fingerprint.is_some();
    let has_candidate = document.candidate_id.is_some();
    let candidate_cleanup_valid = document
        .candidate_cleanup
        .as_deref()
        .is_none_or(|value| matches!(value, "delete_dispatched" | "absence_ambiguous"));
    let no_cleanup = document.cleanup_evidence.is_none();
    match document.state {
        DurableStageState::Allocated => {
            !has_payload
                && document.committed_offset == 0
                && !has_observed
                && !has_operation
                && !has_candidate
                && document.candidate_cleanup.is_none()
                && no_cleanup
                && document.uncertainty.is_none()
        }
        DurableStageState::Receiving => {
            has_payload
                && !has_observed
                && !has_operation
                && !has_candidate
                && document.candidate_cleanup.is_none()
                && no_cleanup
                && document.uncertainty.is_none()
        }
        DurableStageState::Ready => {
            document.direction == DurableDirection::Import
                && has_payload
                && offset_complete
                && has_observed
                && !has_operation
                && !has_candidate
                && document.candidate_cleanup.is_none()
                && no_cleanup
                && document.uncertainty.is_none()
        }
        DurableStageState::Available => {
            document.direction == DurableDirection::Export
                && has_payload
                && offset_complete
                && has_observed
                && !has_operation
                && !has_candidate
                && document.candidate_cleanup.is_none()
                && no_cleanup
                && document.uncertainty.is_none()
        }
        DurableStageState::Reconciliation => {
            document.direction == DurableDirection::Import
                && has_payload
                && offset_complete
                && has_observed
                && has_operation
                && candidate_cleanup_valid
                && (document.candidate_cleanup.is_none() || has_candidate)
                && no_cleanup
                && match document.uncertainty.as_deref() {
                    Some("pre_dispatch") => !has_candidate,
                    Some("mutation_dispatched") => true,
                    _ => false,
                }
        }
        DurableStageState::Consumed => {
            document.direction == DurableDirection::Import
                && has_payload
                && offset_complete
                && has_observed
                && has_operation
                && has_candidate
                && document.candidate_cleanup.is_none()
                && no_cleanup
                && document.uncertainty.is_none()
        }
        DurableStageState::CleanupPending => {
            matches!(
                document.cleanup_evidence.as_deref(),
                Some("tombstone_pending" | "pathname_authority_closed")
            ) && document
                .uncertainty
                .as_deref()
                .is_none_or(|value| matches!(value, "pre_dispatch" | "mutation_dispatched"))
                && candidate_cleanup_valid
        }
        // This is an in-memory state only. Persisting it cannot prove which
        // side of an atomic publication became durable.
        DurableStageState::PublicationIndeterminate => false,
    }
}

/// Recency and policy bounds applied only when a restart decides whether a
/// well-formed retained record may be revived. Records outside the current
/// policy (larger than the configured maximum, longer-lived than the current
/// TTL, or stamped by a clock this process cannot reconcile) are reaped
/// through the ordinary tombstone protocol instead of failing activation.
fn durable_policy_current(document: &DurableStageRecord, limits: &ArtifactLimits) -> bool {
    let Ok(ttl) = chrono::Duration::from_std(limits.staging_ttl) else {
        return false;
    };
    let now = Utc::now();
    let Some(latest_creation) = now.checked_add_signed(chrono::Duration::minutes(5)) else {
        return false;
    };
    let Some(latest_expiry) = latest_creation.checked_add_signed(ttl) else {
        return false;
    };
    let lifetime = document
        .expires_at
        .signed_duration_since(document.created_at)
        .to_std();
    document.size_bytes <= limits.artifact_bytes
        && lifetime.is_ok_and(|lifetime| lifetime <= limits.staging_ttl)
        && document.created_at <= latest_creation
        && document.expires_at <= latest_expiry
}

fn parse_tombstone(
    mut file: crate::artifact_roots::StagingInventoryFile,
) -> Result<(DurableTombstone, AnchoredImport), StagingError> {
    let document: DurableTombstone =
        serde_json::from_reader(file.source.reader()).map_err(|_| StagingError::Reconciliation)?;
    if document.format_version != DURABLE_RECORD_VERSION
        || file.name != format!("{}.json", document.record_id)
        || document.record_id.len() != 32
        || !document
            .record_id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        return Err(StagingError::Reconciliation);
    }
    Ok((document, file.source))
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

fn make_handle(
    generation_key: &[u8; 32],
) -> Result<([u8; RECORD_BYTES], String, [u8; 32]), StagingError> {
    let mut record = [0_u8; RECORD_BYTES];
    let mut secret = [0_u8; SECRET_BYTES];
    getrandom::fill(&mut record).map_err(|_| StagingError::Upstream)?;
    getrandom::fill(&mut secret).map_err(|_| StagingError::Upstream)?;
    let checksum = digest(&[b"any-mcp/artifact-handle/v1", &record, &secret]);
    let mut bytes = Vec::with_capacity(HANDLE_BYTES);
    bytes.push(HANDLE_VERSION);
    bytes.extend_from_slice(&record);
    bytes.extend_from_slice(&secret);
    let checksum = checksum
        .get(..CHECKSUM_BYTES)
        .ok_or(StagingError::Upstream)?;
    bytes.extend_from_slice(checksum);
    let handle = URL_SAFE_NO_PAD.encode(bytes);
    let bearer = digest(&[
        b"any-mcp/artifact-bearer/v1",
        generation_key,
        &record,
        &secret,
    ]);
    Ok((record, handle, bearer))
}

fn parse_handle(value: &str) -> Result<ParsedHandle, StagingError> {
    if !(64..=128).contains(&value.len())
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(StagingError::NotFound);
    }
    let decoded = URL_SAFE_NO_PAD
        .decode(value)
        .map_err(|_| StagingError::NotFound)?;
    if decoded.len() != HANDLE_BYTES || URL_SAFE_NO_PAD.encode(&decoded) != value {
        return Err(StagingError::NotFound);
    }
    let version = decoded.first().copied().ok_or(StagingError::NotFound)?;
    let record: [u8; RECORD_BYTES] = decoded
        .get(1..1 + RECORD_BYTES)
        .and_then(|value| value.try_into().ok())
        .ok_or(StagingError::NotFound)?;
    let secret: [u8; SECRET_BYTES] = decoded
        .get(1 + RECORD_BYTES..1 + RECORD_BYTES + SECRET_BYTES)
        .and_then(|value| value.try_into().ok())
        .ok_or(StagingError::NotFound)?;
    let supplied_checksum = decoded
        .get(1 + RECORD_BYTES + SECRET_BYTES..)
        .ok_or(StagingError::NotFound)?;
    let expected = digest(&[b"any-mcp/artifact-handle/v1", &record, &secret]);
    let expected_checksum = expected
        .get(..CHECKSUM_BYTES)
        .ok_or(StagingError::NotFound)?;
    if version != HANDLE_VERSION || !constant_time_equal(supplied_checksum, expected_checksum) {
        return Err(StagingError::NotFound);
    }
    Ok(ParsedHandle { record, secret })
}

fn reconcile_inventory(
    directory: &StagingDirectory,
    inventory: StagingInventory,
    limits: &ArtifactLimits,
) -> Result<ReconciliationOutcome, StagingError> {
    let mut records = BTreeMap::new();
    for file in inventory.records {
        let (document, source) = parse_record(file)?;
        if !durable_shape_valid(&document)
            || records
                .insert(document.record_id.clone(), (document, source))
                .is_some()
        {
            return Err(StagingError::Reconciliation);
        }
    }
    let mut payloads = BTreeMap::new();
    let mut retained_bytes = 0_u64;
    for file in inventory.payloads {
        let record_id = file
            .name
            .strip_suffix(".bin")
            .ok_or(StagingError::Reconciliation)?
            .to_owned();
        retained_bytes = retained_bytes
            .checked_add(file.source.length)
            .ok_or(StagingError::Reconciliation)?;
        if retained_bytes > limits.staging_total_bytes
            || payloads.insert(record_id, file.source).is_some()
        {
            return Err(StagingError::Reconciliation);
        }
    }
    let mut tombstones = BTreeMap::new();
    for file in inventory.tombstones {
        let (document, source) = parse_tombstone(file)?;
        if tombstones
            .insert(document.record_id.clone(), (document, source))
            .is_some()
        {
            return Err(StagingError::Reconciliation);
        }
    }
    if records.len() > limits.staging_entries
        || payloads.len() > limits.staging_entries
        || tombstones.len() > limits.staging_entries
    {
        return Err(StagingError::Reconciliation);
    }
    if records.values().any(|(document, _)| {
        document.cleanup_evidence.as_deref() == Some("pathname_authority_closed")
    }) {
        // A prior process crossed the validate/unlink boundary without
        // proving completion. Retain every indexed object and require operator
        // reconciliation rather than reopening pathname authority.
        return Err(StagingError::Reconciliation);
    }
    let record_ids = records.keys().cloned().collect::<BTreeSet<_>>();
    for (record_id, (document, source)) in &records {
        if document.state == DurableStageState::Allocated {
            // A same-named payload is the expected footprint of a crash
            // between payload creation and the `Receiving` publish. The reap
            // loop deletes the pair through the tombstone protocol; only a
            // payload identity claimed by a record that never reached
            // `Receiving` is semantically impossible.
            if document.payload_identity.is_some() {
                return Err(StagingError::Reconciliation);
            }
        } else {
            let payload = payloads
                .get(record_id)
                .ok_or(StagingError::Reconciliation)?;
            if document.payload_identity
                != Some(DurableFileIdentity::from(payload.staging_identity()))
                || payload.length < document.committed_offset
                || payload.length > document.size_bytes
            {
                return Err(StagingError::Reconciliation);
            }
        }
        if let Some((tombstone, _)) = tombstones.get(record_id)
            && (document.state != DurableStageState::CleanupPending
                || !matches!(
                    document.cleanup_evidence.as_deref(),
                    Some("tombstone_pending" | "pathname_authority_closed")
                )
                || tombstone.record_identity
                    != DurableFileIdentity::from(source.staging_identity()))
        {
            return Err(StagingError::Reconciliation);
        }
    }
    for (record_id, payload) in &payloads {
        if !record_ids.contains(record_id) {
            let Some((tombstone, _)) = tombstones.get(record_id) else {
                return Err(StagingError::Reconciliation);
            };
            if tombstone.payload_identity
                != Some(DurableFileIdentity::from(payload.staging_identity()))
            {
                return Err(StagingError::Reconciliation);
            }
        }
    }
    for record_id in tombstones.keys() {
        if !records.contains_key(record_id) && !payloads.contains_key(record_id) {
            continue;
        }
        let (document, _) = tombstones
            .get(record_id)
            .ok_or(StagingError::Reconciliation)?;
        if let Some(payload) = payloads.get(record_id)
            && document.payload_identity
                != Some(DurableFileIdentity::from(payload.staging_identity()))
        {
            return Err(StagingError::Reconciliation);
        }
    }

    let mut reconciled = 0_usize;
    let mut retained = Vec::new();
    for temporary in inventory.temporary {
        directory
            .remove_exact_temporary(&temporary.name, &temporary.source)
            .map_err(|_| StagingError::Reconciliation)?;
    }
    let mut all_ids = record_ids;
    all_ids.extend(payloads.keys().cloned());
    let ids = all_ids.into_iter().collect::<Vec<_>>();
    for record_id in ids {
        let retain_uncertain = records.get(&record_id).is_some_and(|(document, _)| {
            document.state == DurableStageState::Reconciliation
                && document.uncertainty.as_deref() == Some("mutation_dispatched")
                && document.expires_at > Utc::now()
                && document.observed_sha256.is_some()
                && document.operation_fingerprint.is_some()
                && durable_policy_current(document, limits)
        });
        if retain_uncertain {
            let (document, record_source) = records
                .remove(&record_id)
                .ok_or(StagingError::Reconciliation)?;
            let payload_source = payloads
                .remove(&record_id)
                .ok_or(StagingError::Reconciliation)?;
            retained.push(RecoveredDurableRecord {
                document,
                record_source,
                payload_source,
            });
            continue;
        }
        if let (Some((document, _)), Some(payload)) =
            (records.get(&record_id), payloads.get(&record_id))
            && document.state == DurableStageState::Receiving
            && payload.length > document.committed_offset
        {
            directory
                .truncate_exact_payload(
                    &format!("{record_id}.bin"),
                    payload,
                    document.committed_offset,
                )
                .map_err(|_| StagingError::Reconciliation)?;
        }
        let existing_tombstone = tombstones.remove(&record_id);
        let tombstone_source = if let Some((_, source)) = existing_tombstone {
            source
        } else if let Some((_, record_source)) = records.get(&record_id) {
            let tombstone = DurableTombstone {
                format_version: DURABLE_RECORD_VERSION,
                record_id: record_id.clone(),
                payload_identity: payloads
                    .get(&record_id)
                    .map(|payload| DurableFileIdentity::from(payload.staging_identity())),
                record_identity: DurableFileIdentity::from(record_source.staging_identity()),
            };
            directory
                .publish_tombstone(&record_id, &durable_json(&tombstone)?)
                .map_err(|_| StagingError::Reconciliation)?
        } else {
            return Err(StagingError::Reconciliation);
        };
        if let Some(payload) = payloads.remove(&record_id) {
            directory
                .remove_exact_record(&format!("{record_id}.bin"), &payload)
                .map_err(|_| StagingError::Reconciliation)?;
        }
        if let Some((_, record_source)) = records.remove(&record_id) {
            directory
                .remove_exact_record_state(&record_id, &record_source)
                .map_err(|_| StagingError::Reconciliation)?;
        }
        directory
            .remove_exact_tombstone(&record_id, &tombstone_source)
            .map_err(|_| StagingError::Reconciliation)?;
        reconciled = reconciled.saturating_add(1);
    }
    for (record_id, (_, source)) in tombstones {
        directory
            .remove_exact_tombstone(&record_id, &source)
            .map_err(|_| StagingError::Reconciliation)?;
    }
    Ok(ReconciliationOutcome {
        cleaned: reconciled,
        retained,
    })
}

impl ArtifactStaging {
    fn close_durable_authority(&self) {
        self.state
            .durability_uncertain
            .store(true, Ordering::Release);
        self.state.active.store(false, Ordering::Release);
        self.state.shutdown.cancel();
    }

    async fn publish_document(
        &self,
        document: &DurableStageRecord,
    ) -> Result<AnchoredImport, StagingError> {
        if !durable_shape_valid(document) {
            self.close_durable_authority();
            return Err(StagingError::Indeterminate);
        }
        let bytes = durable_json(document)?;
        let directory = self.state.directory.clone();
        let record_id = document.record_id.clone();
        let task_guard = self.task_guard();
        let publication = tokio::task::spawn_blocking(move || {
            let mut task_guard = task_guard;
            let result = directory.publish_record(&record_id, &bytes);
            task_guard.complete();
            result
        })
        .await;
        match publication {
            Ok(Ok(source)) => Ok(source),
            Ok(Err(_)) | Err(_) => {
                self.close_durable_authority();
                Err(StagingError::Indeterminate)
            }
        }
    }

    async fn persist_transition(
        &self,
        record: &StageRecord,
        update: impl FnOnce(&mut DurableStageRecord),
    ) -> Result<(), StagingError> {
        let mut durable = record.durable.lock().await;
        let mut next = durable.document.clone();
        update(&mut next);
        let source = self.publish_document(&next).await?;
        durable.document = next;
        durable.source = source;
        Ok(())
    }

    /// Returns content-free remaining record and byte capacity.
    pub(crate) async fn available_quota(&self) -> (u64, u32) {
        let records = self.state.records.read().await;
        let reserved_bytes = records.values().fold(0_u64, |total, record| {
            total.saturating_add(record.size_bytes)
        });
        let available_bytes = self
            .state
            .limits
            .staging_total_bytes
            .saturating_sub(reserved_bytes);
        let available_entries = self
            .state
            .limits
            .staging_entries
            .saturating_sub(records.len());
        (
            available_bytes,
            u32::try_from(available_entries).map_or(u32::MAX, |value| value),
        )
    }

    /// Activates private root authority and a fresh handle generation.
    #[cfg(test)]
    pub(crate) async fn activate(
        config: &StagingConfig,
        limits: &ArtifactLimits,
        local_roots: &RootRegistry,
        shutdown: CancellationToken,
    ) -> Result<Self, StagingError> {
        Self::activate_with_policy_digest(config, limits, local_roots, [0; 32], shutdown).await
    }

    /// Activates staging while binding handles and durable records to the
    /// runtime's canonical, credential-free configuration evidence.
    pub(crate) async fn activate_with_policy_digest(
        config: &StagingConfig,
        limits: &ArtifactLimits,
        local_roots: &RootRegistry,
        runtime_policy_digest: [u8; 32],
        shutdown: CancellationToken,
    ) -> Result<Self, StagingError> {
        if !config.enabled {
            return Err(StagingError::Disabled);
        }
        let (directory, inventory) = StagingDirectory::activate(
            config.root(),
            local_roots,
            limits.staging_entries,
            limits.artifact_bytes,
        )
        .map_err(|_| StagingError::InvalidPolicy)?;
        let reconciliation = reconcile_inventory(&directory, inventory, limits)?;
        let mut generation_key = [0_u8; 32];
        getrandom::fill(&mut generation_key).map_err(|_| StagingError::Upstream)?;
        let policy_digest =
            staging_policy_digest(config, limits, local_roots, directory.policy_identity());
        let generation = bytes_hex(&digest(&[
            b"any-mcp/artifact-generation/v2",
            &generation_key,
            &runtime_policy_digest,
            &policy_digest,
        ]));
        let public_base_url = config
            .public_base_url
            .clone()
            .ok_or(StagingError::Upstream)?;
        let listener = tokio::net::TcpListener::bind(config.bind)
            .await
            .map_err(|_| StagingError::Upstream)?;
        let mut recovered_records = HashMap::new();
        for recovered in reconciliation.retained {
            let record_id = decode_hex_array::<RECORD_BYTES>(&recovered.document.record_id)
                .ok_or(StagingError::Reconciliation)?;
            let operation = recovered
                .document
                .operation_fingerprint
                .as_deref()
                .and_then(decode_hex_array::<32>)
                .ok_or(StagingError::Reconciliation)?;
            let sha256 = recovered
                .document
                .observed_sha256
                .clone()
                .ok_or(StagingError::Reconciliation)?;
            let direction = match recovered.document.direction {
                DurableDirection::Import => StageDirection::Import,
                DurableDirection::Export => StageDirection::Export,
            };
            if direction != StageDirection::Import {
                return Err(StagingError::Reconciliation);
            }
            let remaining = recovered
                .document
                .expires_at
                .signed_duration_since(Utc::now())
                .to_std()
                .unwrap_or(Duration::ZERO);
            let expires = Instant::now()
                .checked_add(remaining)
                .ok_or(StagingError::Reconciliation)?;
            let space_id = SpaceId::new(&recovered.document.space_id)
                .map_err(|_| StagingError::Reconciliation)?;
            let record = Arc::new(StageRecord {
                record_name: format!("{}.bin", recovered.document.record_id),
                bearer_digest: [0; 32],
                direction,
                space_id,
                size_bytes: recovered.document.size_bytes,
                media_type: recovered.document.media_type.clone(),
                expected_sha256: recovered.document.expected_sha256.clone(),
                expires,
                expires_at: recovered.document.expires_at,
                cleanup_blocked: Arc::new(AtomicBool::new(false)),
                state: Arc::new(tokio::sync::Mutex::new(RecordState::Reconciliation {
                    import: Arc::new(RetainedImport {
                        source: recovered.payload_source,
                        sha256,
                    }),
                    operation,
                })),
                durable: tokio::sync::Mutex::new(DurableRecordOwner {
                    document: recovered.document,
                    source: recovered.record_source,
                }),
                tombstone: tokio::sync::Mutex::new(None),
            });
            if recovered_records.insert(record_id, record).is_some() {
                return Err(StagingError::Reconciliation);
            }
        }
        let staging = Self {
            state: Arc::new(StagingState {
                directory,
                generation_key,
                generation,
                public_base_url,
                limits: limits.clone(),
                records: tokio::sync::RwLock::new(recovered_records),
                allowed_hosts: allowed_hosts(config)?,
                request_permits: Arc::new(tokio::sync::Semaphore::new(limits.staging_requests)),
                connection_permits: Arc::new(tokio::sync::Semaphore::new(
                    limits.staging_connections,
                )),
                rate_window: tokio::sync::Mutex::new(VecDeque::new()),
                active: AtomicBool::new(true),
                durability_uncertain: AtomicBool::new(false),
                task_active: AtomicUsize::new(0),
                task_notify: tokio::sync::Notify::new(),
                shutdown,
            }),
        };
        staging.spawn_listener(listener);
        staging.spawn_cleanup();
        tracing::info!(
            target: "any_mcp::operation",
            operation = "artifact_staging_reconciliation",
            outcome = "startup_complete",
            cleanup_count = reconciliation.cleaned,
            "Artifact staging reconciliation completed"
        );
        Ok(staging)
    }

    pub(crate) fn is_active(&self) -> bool {
        self.ensure_authority().is_ok()
    }

    /// Reports whether durable staging authority closed on uncertainty.
    pub(crate) fn durability_uncertain(&self) -> bool {
        self.state.durability_uncertain.load(Ordering::Acquire)
    }

    fn ensure_authority(&self) -> Result<(), StagingError> {
        if !self.state.active.load(Ordering::Acquire) || self.state.shutdown.is_cancelled() {
            self.close_durable_authority();
            return Err(StagingError::Indeterminate);
        }
        match self.state.directory.authority_intact() {
            Ok(()) => Ok(()),
            // Resource exhaustion (attacker-influenceable descriptor floods
            // included) sheds only the probing request; identity was not
            // disproven, so staging authority stays open.
            Err(crate::artifact_roots::AuthorityProbe::Transient) => Err(StagingError::Upstream),
            Err(crate::artifact_roots::AuthorityProbe::Replaced) => {
                self.close_durable_authority();
                Err(StagingError::Indeterminate)
            }
        }
    }

    fn task_guard(&self) -> StagingTaskGuard {
        let registered = self
            .state
            .task_active
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |active| {
                active.checked_add(1)
            })
            .is_ok();
        if !registered {
            self.state.shutdown.cancel();
        }
        StagingTaskGuard {
            state: Arc::clone(&self.state),
            registered,
            completed: false,
            fatal_if_incomplete: true,
        }
    }

    fn cleanup_task_guard(&self) -> StagingTaskGuard {
        let mut guard = self.task_guard();
        guard.fatal_if_incomplete = false;
        guard
    }

    pub(crate) async fn drain(&self, timeout: Duration) -> bool {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let notified = self.state.task_notify.notified();
            if self.state.task_active.load(Ordering::Acquire) == 0 {
                return true;
            }
            if tokio::time::timeout_at(deadline, notified).await.is_err() {
                return false;
            }
        }
    }

    fn spawn_listener(&self, listener: tokio::net::TcpListener) {
        let staging = self.clone();
        let task_guard = self.task_guard();
        tokio::spawn(async move {
            let mut task_guard = task_guard;
            loop {
                let accepted = tokio::select! {
                    biased;
                    () = staging.state.shutdown.cancelled() => break,
                    () = tokio::time::sleep(Duration::from_secs(1)) => {
                        // Transient probe failures shed nothing here; only a
                        // proven replacement (which closes authority) stops
                        // the listener.
                        let _ = staging.ensure_authority();
                        if !staging.state.active.load(Ordering::Acquire) {
                            break;
                        }
                        continue;
                    },
                    accepted = listener.accept() => accepted,
                };
                let (stream, _) = match accepted {
                    Ok(accepted) => accepted,
                    Err(_) => {
                        staging.state.shutdown.cancel();
                        break;
                    }
                };
                let permit = match Arc::clone(&staging.state.connection_permits).try_acquire_owned()
                {
                    Ok(permit) => permit,
                    Err(_) => continue,
                };
                let connection_staging = staging.clone();
                let connection_guard = staging.task_guard();
                tokio::spawn(async move {
                    let mut connection_guard = connection_guard;
                    let _permit = permit;
                    let service_staging = connection_staging.clone();
                    let service = service_fn(move |request| {
                        let request_staging = service_staging.clone();
                        async move { Ok::<_, Infallible>(request_staging.handle_http(request).await) }
                    });
                    let mut builder = hyper::server::conn::http1::Builder::new();
                    builder
                        .timer(TokioTimer::new())
                        .header_read_timeout(connection_staging.state.limits.staging_header_timeout)
                        .max_headers(32)
                        .max_buf_size(
                            connection_staging
                                .state
                                .limits
                                .staging_header_bytes
                                .max(8 * 1024),
                        );
                    let connection = builder.serve_connection(TokioIo::new(stream), service);
                    tokio::select! {
                        biased;
                        () = connection_staging.state.shutdown.cancelled() => {}
                        _ = connection => {}
                    }
                    connection_guard.complete();
                });
            }
            staging.state.active.store(false, Ordering::Release);
            task_guard.complete();
        });
    }

    fn spawn_cleanup(&self) {
        let staging = self.clone();
        let task_guard = self.task_guard();
        tokio::spawn(async move {
            let mut task_guard = task_guard;
            let cadence = staging
                .state
                .limits
                .staging_ttl
                .min(Duration::from_secs(60));
            loop {
                tokio::select! {
                    biased;
                    () = staging.state.shutdown.cancelled() => break,
                    () = tokio::time::sleep(cadence) => {
                        let expired = {
                            let mut records = staging.state.records.write().await;
                            staging.take_expired_locked(&mut records, Instant::now())
                        };
                        if let Some(cleanup) = staging.spawn_expired_cleanup(expired)
                            && cleanup.await.is_err_and(|error| error.is_panic())
                        {
                            staging
                                .state
                                .durability_uncertain
                                .store(true, Ordering::Release);
                            staging.state.active.store(false, Ordering::Release);
                            staging.state.shutdown.cancel();
                            break;
                        }
                    }
                }
            }
            if staging.state.durability_uncertain.load(Ordering::Acquire) {
                task_guard.complete();
                return;
            }
            let records = {
                let records = staging.state.records.write().await;
                records
                    .iter()
                    .filter(|(_, record)| claim_shutdown_record(record))
                    .map(|(id, record)| (*id, Arc::clone(record)))
                    .collect::<Vec<_>>()
            };
            for (id, record) in records {
                let _ = staging.cleanup_coordinator(id, record).await;
            }
            task_guard.complete();
        });
    }

    async fn handle_http(&self, request: Request<Incoming>) -> Response<StagingBody> {
        if self.ensure_authority().is_err() {
            return fixed_response(StatusCode::SERVICE_UNAVAILABLE);
        }
        let request_permit = match Arc::clone(&self.state.request_permits).try_acquire_owned() {
            Ok(permit) => permit,
            Err(_) => return fixed_response(StatusCode::SERVICE_UNAVAILABLE),
        };
        let _request_permit = request_permit;
        if !valid_header_block(&request, &self.state.limits)
            || request.headers().contains_key(TRANSFER_ENCODING)
        {
            return fixed_response(StatusCode::BAD_REQUEST);
        }
        if request.headers().contains_key(ORIGIN) {
            return fixed_response(StatusCode::FORBIDDEN);
        }
        let host = match single_header(request.headers(), HOST, 255) {
            Ok(Some(host))
                if self
                    .state
                    .allowed_hosts
                    .iter()
                    .any(|allowed| allowed == host) =>
            {
                host
            }
            _ => return fixed_response(StatusCode::FORBIDDEN),
        };
        let _ = host;
        let path = request.uri().path();
        if request.uri().query().is_some()
            || path.len() > 256
            || path.contains('%')
            || !path.starts_with("/artifacts/v1/")
        {
            return fixed_response(StatusCode::NOT_FOUND);
        }
        let record = match path.strip_prefix("/artifacts/v1/") {
            Some(record)
                if record.len() == 32
                    && record
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)) =>
            {
                record.to_owned()
            }
            _ => return fixed_response(StatusCode::NOT_FOUND),
        };
        if !self.admit_rate().await {
            return fixed_response(StatusCode::TOO_MANY_REQUESTS);
        }
        let authorization = match single_header(request.headers(), AUTHORIZATION, 160) {
            Ok(Some(value)) => value,
            _ => return fixed_response(StatusCode::UNAUTHORIZED),
        };
        let handle = match authorization.strip_prefix("Bearer ") {
            Some(handle)
                if !handle.is_empty() && !handle.bytes().any(|byte| byte.is_ascii_whitespace()) =>
            {
                handle.to_owned()
            }
            _ => return fixed_response(StatusCode::UNAUTHORIZED),
        };
        match *request.method() {
            Method::HEAD => self.http_head(&handle, &record).await,
            Method::PUT => self.http_put(request, &handle, &record).await,
            Method::GET => self.http_get(request, &handle, &record).await,
            Method::DELETE => self.http_delete(&handle, &record).await,
            _ => fixed_response(StatusCode::METHOD_NOT_ALLOWED),
        }
    }

    async fn admit_rate(&self) -> bool {
        let now = Instant::now();
        let cutoff = now.checked_sub(Duration::from_secs(60)).unwrap_or(now);
        let mut window = self.state.rate_window.lock().await;
        while window.front().is_some_and(|entry| *entry <= cutoff) {
            window.pop_front();
        }
        if window.len() >= self.state.limits.staging_requests_per_minute as usize {
            return false;
        }
        window.push_back(now);
        true
    }

    async fn http_head(&self, handle: &str, record: &str) -> Response<StagingBody> {
        match self.inspect_route(handle, record).await {
            Ok(status) => status_response(StatusCode::OK, &status, true),
            Err(error) => staging_http_error(error),
        }
    }

    async fn http_delete(&self, handle: &str, record: &str) -> Response<StagingBody> {
        if let Err(error) = self.authenticate(handle, Some(record)).await {
            return staging_http_error(error);
        }
        match self.release(handle).await {
            Ok(()) => fixed_response(StatusCode::NO_CONTENT),
            Err(error) => staging_http_error(error),
        }
    }

    async fn http_put(
        &self,
        request: Request<Incoming>,
        handle: &str,
        record: &str,
    ) -> Response<StagingBody> {
        let operation_deadline = tokio::time::Instant::now() + self.state.limits.operation_timeout;
        let status = match self.inspect_route(handle, record).await {
            Ok(status) if status.direction == StageDirection::Import => status,
            Ok(_) => return fixed_response(StatusCode::NOT_FOUND),
            Err(error) => return staging_http_error(error),
        };
        let content_length = match parse_single_u64(request.headers(), CONTENT_LENGTH, 128) {
            Ok(Some(length)) if length > 0 => length,
            _ => return fixed_response(StatusCode::BAD_REQUEST),
        };
        let (offset, expected_request_bytes) =
            match single_header(request.headers(), CONTENT_RANGE, 128) {
                Ok(Some(range)) => match parse_content_range(range, status.size_bytes) {
                    Some((offset, length)) if length == content_length => (offset, length),
                    _ => return fixed_response(StatusCode::BAD_REQUEST),
                },
                Ok(None) if content_length == status.size_bytes => (0, content_length),
                _ => return fixed_response(StatusCode::BAD_REQUEST),
            };
        if expected_request_bytes > self.state.limits.transfer_chunk_bytes {
            return fixed_response(StatusCode::PAYLOAD_TOO_LARGE);
        }
        if offset != status.offset {
            return fixed_response(StatusCode::CONFLICT);
        }
        let supplied_media = match single_header(request.headers(), CONTENT_TYPE, 255) {
            Ok(value) => value,
            Err(_) => return fixed_response(StatusCode::BAD_REQUEST),
        };
        if supplied_media != status.media_type.as_deref() {
            return fixed_response(StatusCode::BAD_REQUEST);
        }
        let mut lease = match self
            .begin_write(handle, Some(record), StageDirection::Import, offset)
            .await
        {
            Ok(lease) => lease,
            Err(error) => return staging_http_error(error),
        };
        let destination = match lease.take_destination() {
            Ok(destination) => destination,
            Err(error) => return staging_http_error(error),
        };
        let (_, body) = request.into_parts();
        let outcome = write_incoming(
            body,
            destination,
            expected_request_bytes,
            self.state.limits.staging_no_progress_timeout,
            operation_deadline,
            &self.state.shutdown,
            self.task_guard(),
        )
        .await;
        let outcome = match outcome {
            Ok(outcome) => outcome,
            Err(error) => return staging_http_error(error),
        };
        let cumulative = match lease.offset.checked_add(outcome.written) {
            Some(cumulative) => cumulative,
            None => {
                return fixed_response(StatusCode::PAYLOAD_TOO_LARGE);
            }
        };
        if let Some(error) = outcome.error {
            let committed = lease.offset;
            let _ = self
                .restore_write(lease, outcome.destination, committed)
                .await;
            return staging_http_error(error);
        }
        if outcome.written != expected_request_bytes {
            let _ = self
                .restore_write(lease, outcome.destination, cumulative)
                .await;
            return fixed_response(StatusCode::BAD_REQUEST);
        }
        if cumulative < status.size_bytes {
            return match self
                .restore_write(lease, outcome.destination, cumulative)
                .await
            {
                Ok(()) => offset_response(StatusCode::NO_CONTENT, cumulative),
                Err(error) => staging_http_error(error),
            };
        }
        if cumulative != status.size_bytes {
            return fixed_response(StatusCode::PAYLOAD_TOO_LARGE);
        }
        match self
            .finish_import(lease, outcome.destination, cumulative)
            .await
        {
            Ok(()) => offset_response(StatusCode::CREATED, cumulative),
            Err(error) => staging_http_error(error),
        }
    }

    async fn http_get(
        &self,
        request: Request<Incoming>,
        handle: &str,
        record: &str,
    ) -> Response<StagingBody> {
        let operation_deadline = tokio::time::Instant::now() + self.state.limits.operation_timeout;
        let (source, status) = match self.export_reader(handle, Some(record)).await {
            Ok(reader) => reader,
            Err(error) => return staging_http_error(error),
        };
        let requested = match single_header(request.headers(), RANGE, 128) {
            Ok(Some(range)) => match parse_download_range(range, status.size_bytes) {
                DownloadRange::Valid(range) => Some(range),
                DownloadRange::Malformed => return fixed_response(StatusCode::BAD_REQUEST),
                DownloadRange::Unsatisfiable => {
                    return fixed_response(StatusCode::RANGE_NOT_SATISFIABLE);
                }
            },
            Ok(None) => None,
            Err(_) => return fixed_response(StatusCode::BAD_REQUEST),
        };
        let (offset, length, response_status) = match requested {
            Some((offset, length)) => (offset, length, StatusCode::PARTIAL_CONTENT),
            None => (0, status.size_bytes, StatusCode::OK),
        };
        let StageExportSource { file, lease } = source;
        let reader = match PositionalReader::range(file, offset, length) {
            Ok(reader) => reader,
            Err(_) => return fixed_response(StatusCode::INTERNAL_SERVER_ERROR),
        };
        let stream = VerifiedExportStream {
            reader: ReaderStream::with_capacity(
                reader,
                self.state.limits.transfer_chunk_bytes.min(1024 * 1024) as usize,
            ),
            lease,
            deadline: Box::pin(tokio::time::sleep_until(operation_deadline)),
            final_verification_complete: false,
            terminated: false,
        };
        let body = StreamBody::new(stream).boxed_unsync();
        let mut response = Response::new(body);
        *response.status_mut() = response_status;
        if set_header(&mut response, CONTENT_LENGTH, &length.to_string()).is_err()
            || status
                .media_type
                .as_deref()
                .is_some_and(|media| set_header(&mut response, CONTENT_TYPE, media).is_err())
            || status.sha256.as_deref().is_some_and(|sha256| {
                set_header(&mut response, hyper::header::ETAG, &format!("\"{sha256}\"")).is_err()
            })
        {
            return fixed_response(StatusCode::INTERNAL_SERVER_ERROR);
        }
        if response_status == StatusCode::PARTIAL_CONTENT {
            let end = match offset.checked_add(length.saturating_sub(1)) {
                Some(end) => end,
                None => return fixed_response(StatusCode::INTERNAL_SERVER_ERROR),
            };
            let value = format!("bytes {offset}-{end}/{}", status.size_bytes);
            if set_header(&mut response, CONTENT_RANGE, &value).is_err() {
                return fixed_response(StatusCode::INTERNAL_SERVER_ERROR);
            }
        }
        response
    }

    /// Allocates one exact-size remote upload record.
    pub(crate) async fn allocate_import(
        &self,
        space_id: SpaceId,
        size_bytes: u64,
        media_type: Option<String>,
        expected_sha256: Option<String>,
    ) -> Result<StageAllocation, StagingError> {
        self.allocate(
            StageDirection::Import,
            space_id,
            size_bytes,
            media_type,
            expected_sha256,
        )
        .await
    }

    /// Allocates one exact-size remote export record.
    pub(crate) async fn allocate_export(
        &self,
        space_id: SpaceId,
        size_bytes: u64,
        media_type: Option<String>,
    ) -> Result<StageAllocation, StagingError> {
        self.allocate(
            StageDirection::Export,
            space_id,
            size_bytes,
            media_type,
            None,
        )
        .await
    }

    async fn allocate(
        &self,
        direction: StageDirection,
        space_id: SpaceId,
        size_bytes: u64,
        media_type: Option<String>,
        expected_sha256: Option<String>,
    ) -> Result<StageAllocation, StagingError> {
        self.ensure_authority()?;
        if (size_bytes == 0 && direction == StageDirection::Import)
            || size_bytes > self.state.limits.artifact_bytes
        {
            return Err(StagingError::Bounded);
        }
        let expired = {
            let mut records = self.state.records.write().await;
            self.take_expired_locked(&mut records, Instant::now())
        };
        if let Some(cleanup) = self.spawn_expired_cleanup(expired) {
            cleanup.await.map_err(|_| StagingError::Upstream)?;
        }
        let mut records = self.state.records.write().await;
        if records.len() >= self.state.limits.staging_entries {
            return Err(StagingError::Bounded);
        }
        let reserved = records
            .values()
            .try_fold(0_u64, |total, record| total.checked_add(record.size_bytes));
        if reserved
            .and_then(|reserved| reserved.checked_add(size_bytes))
            .is_none_or(|total| total > self.state.limits.staging_total_bytes)
        {
            return Err(StagingError::Bounded);
        }

        let (record, handle, bearer_digest) = make_handle(&self.state.generation_key)?;
        if records.contains_key(&record) {
            return Err(StagingError::Upstream);
        }
        let record_id = record_hex(&record);
        let record_name = format!("{record_id}.bin");
        let ttl = self.state.limits.staging_ttl;
        let created_at = Utc::now();
        let expires = Instant::now()
            .checked_add(ttl)
            .ok_or(StagingError::Upstream)?;
        let expires_at = wall_expiry(created_at, ttl)?;
        let allocated = DurableStageRecord {
            format_version: DURABLE_RECORD_VERSION,
            generation: self.state.generation.clone(),
            record_id: record_id.clone(),
            bearer_digest: bytes_hex(&bearer_digest),
            direction: direction.into(),
            state: DurableStageState::Allocated,
            created_at,
            expires_at,
            space_id: space_id.as_str().to_owned(),
            size_bytes,
            media_type: media_type.clone(),
            expected_sha256: expected_sha256.clone(),
            observed_sha256: None,
            committed_offset: 0,
            payload_identity: None,
            operation_fingerprint: None,
            candidate_id: None,
            candidate_cleanup: None,
            cleanup_evidence: None,
            uncertainty: None,
        };
        let allocated_source = self.publish_document(&allocated).await?;
        let directory = self.state.directory.clone();
        let payload_id = record_id.clone();
        let maximum = self.state.limits.artifact_bytes;
        let task_guard = self.task_guard();
        let created = tokio::task::spawn_blocking(move || {
            let mut task_guard = task_guard;
            let result = directory.create_payload(&payload_id, maximum);
            task_guard.complete();
            result
        })
        .await;
        let destination = match created {
            Ok(Ok(destination)) => destination,
            Ok(Err(error)) if error.kind() != RootAccessErrorKind::Indeterminate => {
                // The payload provably does not exist (`create_payload`
                // removes its own failures), so the published `Allocated`
                // record is the only durable evidence. Remove it by retained
                // identity rather than leaving poison for the next restart.
                let directory = self.state.directory.clone();
                let cleanup_id = record_id.clone();
                let task_guard = self.task_guard();
                let removal = tokio::task::spawn_blocking(move || {
                    let mut task_guard = task_guard;
                    let result =
                        directory.remove_exact_record_state(&cleanup_id, &allocated_source);
                    task_guard.complete();
                    result
                })
                .await;
                if !matches!(removal, Ok(Ok(()))) {
                    self.close_durable_authority();
                    return Err(StagingError::Indeterminate);
                }
                return Err(StagingError::Upstream);
            }
            Ok(Err(_)) | Err(_) => {
                // Payload existence is unproven; only closing durable
                // authority reports that uncertainty truthfully.
                self.close_durable_authority();
                return Err(StagingError::Indeterminate);
            }
        };
        let mut receiving = allocated;
        receiving.state = DurableStageState::Receiving;
        receiving.payload_identity = Some(destination.identity().into());
        let receiving_source = match self.publish_document(&receiving).await {
            Ok(source) => source,
            Err(error) => {
                self.state.shutdown.cancel();
                return Err(error);
            }
        };
        drop(allocated_source);
        records.insert(
            record,
            Arc::new(StageRecord {
                record_name,
                bearer_digest,
                direction,
                space_id,
                size_bytes,
                media_type,
                expected_sha256,
                expires,
                expires_at,
                cleanup_blocked: Arc::new(AtomicBool::new(false)),
                durable: tokio::sync::Mutex::new(DurableRecordOwner {
                    document: receiving,
                    source: receiving_source,
                }),
                tombstone: tokio::sync::Mutex::new(None),
                state: Arc::new(tokio::sync::Mutex::new(RecordState::Receiving {
                    destination: Some(destination),
                    offset: 0,
                })),
            }),
        );
        Ok(StageAllocation {
            record: record_id.clone(),
            handle,
            url: format!("{}{record_id}", self.state.public_base_url),
            expires_at,
            size_bytes,
        })
    }

    async fn authenticate(
        &self,
        handle: &str,
        route_record: Option<&str>,
    ) -> Result<([u8; RECORD_BYTES], Arc<StageRecord>), StagingError> {
        self.ensure_authority()?;
        let parsed = parse_handle(handle)?;
        if route_record.is_some_and(|route| route != record_hex(&parsed.record)) {
            return Err(StagingError::NotFound);
        }
        let record = self
            .state
            .records
            .read()
            .await
            .get(&parsed.record)
            .cloned()
            .ok_or(StagingError::NotFound)?;
        let supplied = digest(&[
            b"any-mcp/artifact-bearer/v1",
            &self.state.generation_key,
            &parsed.record,
            &parsed.secret,
        ]);
        if record.expires <= Instant::now()
            || !constant_time_equal(&record.bearer_digest, &supplied)
        {
            return Err(StagingError::NotFound);
        }
        Ok((parsed.record, record))
    }

    /// Returns bounded state for one authenticated handle.
    #[cfg(test)]
    pub(crate) async fn inspect(&self, handle: &str) -> Result<StageStatus, StagingError> {
        let (_, record) = self.authenticate(handle, None).await?;
        let state = record.state.lock().await;
        Ok(status_for(&record, &state))
    }

    async fn inspect_route(
        &self,
        handle: &str,
        route_record: &str,
    ) -> Result<StageStatus, StagingError> {
        let (_, record) = self.authenticate(handle, Some(route_record)).await?;
        let state = record.state.lock().await;
        Ok(status_for(&record, &state))
    }

    /// Leases one receiving record at its exact committed offset.
    pub(crate) async fn begin_write(
        &self,
        handle: &str,
        route_record: Option<&str>,
        direction: StageDirection,
        offset: u64,
    ) -> Result<StageWriteLease, StagingError> {
        let (record_id, record) = self.authenticate(handle, route_record).await?;
        if record.direction != direction {
            return Err(StagingError::NotFound);
        }
        let records = self.state.records.read().await;
        if !records
            .get(&record_id)
            .is_some_and(|current| Arc::ptr_eq(current, &record))
        {
            return Err(StagingError::NotFound);
        }
        let mut state = record.state.lock().await;
        let RecordState::Receiving {
            destination,
            offset: committed,
        } = &mut *state
        else {
            return Err(StagingError::Conflict);
        };
        if *committed != offset {
            return Err(StagingError::Conflict);
        }
        let destination = destination.take().ok_or(StagingError::Conflict)?;
        record.cleanup_blocked.store(true, Ordering::Release);
        drop(records);
        Ok(StageWriteLease {
            destination: Some(destination),
            offset,
            size_bytes: record.size_bytes,
            record: Arc::clone(&record),
            cleanup_active: true,
        })
    }

    /// Restores an incomplete sequential upload at a proven offset.
    pub(crate) async fn restore_write(
        &self,
        lease: StageWriteLease,
        destination: StagingPayload,
        offset: u64,
    ) -> Result<(), StagingError> {
        if offset < lease.offset || offset > lease.size_bytes {
            return Err(StagingError::Conflict);
        }
        let mut destination = destination;
        if destination.truncate(offset).is_err() {
            self.close_durable_authority();
            return Err(StagingError::Indeterminate);
        }
        self.persist_transition(&lease.record, |document| {
            document.state = DurableStageState::Receiving;
            document.committed_offset = offset;
        })
        .await?;
        let mut state = lease.record.state.lock().await;
        match &mut *state {
            RecordState::Receiving {
                destination: slot,
                offset: committed,
            } if slot.is_none() => {
                *slot = Some(destination);
                *committed = offset;
            }
            _ => return Err(StagingError::Conflict),
        }
        Ok(())
    }

    /// Surrenders an active writer before releasing its staging record.
    ///
    /// Callers routinely reach this after `take_destination` handed the
    /// payload writer to a transfer that failed without returning it. An
    /// emptied lease therefore still releases: cleanup reopens the payload
    /// from its durable identity instead of requiring the live handle.
    pub(crate) async fn abort_write(
        &self,
        mut lease: StageWriteLease,
        handle: &str,
    ) -> Result<(), StagingError> {
        match lease.destination.take() {
            Some(destination) => {
                let offset = lease.offset;
                self.restore_write(lease, destination, offset).await?;
            }
            None => drop(lease),
        }
        self.release(handle).await
    }

    /// Publishes a complete import upload as a retained ready source.
    pub(crate) async fn finish_import(
        &self,
        mut lease: StageWriteLease,
        mut destination: StagingPayload,
        observed_size: u64,
    ) -> Result<(), StagingError> {
        if lease.record.direction != StageDirection::Import || observed_size != lease.size_bytes {
            return Err(StagingError::Conflict);
        }
        destination.flush().map_err(|_| StagingError::Upstream)?;
        let mut pending = destination
            .try_clone_reader()
            .map_err(|_| StagingError::Upstream)?;
        let prepublication_sha256 =
            hash_file(&mut pending, observed_size).map_err(|_| StagingError::Upstream)?;
        if lease
            .record
            .expected_sha256
            .as_ref()
            .is_some_and(|expected| expected != &prepublication_sha256)
        {
            self.restore_write(lease, destination, observed_size)
                .await?;
            return Err(StagingError::Conflict);
        }
        let completion = begin_publication(&mut lease).await?;
        let owner_guard = PublicationOwnerGuard(Arc::clone(&completion));
        let completion_guard = PublicationCompletionGuard(completion);
        #[cfg(test)]
        let record_name = lease.record.record_name.clone();
        let task_guard = self.task_guard();
        let publication = tokio::task::spawn_blocking(move || {
            let mut task_guard = task_guard;
            let _completion_guard = completion_guard;
            #[cfg(test)]
            pause_publication_for_test(&record_name);
            let result = (|| {
                let source = destination
                    .into_anchored()
                    .map_err(|_| StagingError::Indeterminate)?;
                let sha256 = hash_source(&source)?;
                Ok::<_, StagingError>((source, sha256))
            })();
            task_guard.complete();
            result
        })
        .await;
        let (source, sha256) = match publication {
            Ok(Ok(publication)) => publication,
            Ok(Err(_)) | Err(_) => {
                self.close_durable_authority();
                return Err(StagingError::Indeterminate);
            }
        };
        if sha256 != prepublication_sha256 {
            let name = lease.record.record_name.clone();
            let directory = self.state.directory.clone();
            let _ =
                tokio::task::spawn_blocking(move || directory.remove_exact_record(&name, &source))
                    .await;
            self.close_durable_authority();
            return Err(StagingError::Indeterminate);
        }
        self.persist_transition(&lease.record, |document| {
            document.state = DurableStageState::Ready;
            document.committed_offset = observed_size;
            document.observed_sha256 = Some(sha256.clone());
        })
        .await?;
        let mut state = lease.record.state.lock().await;
        *state = RecordState::Ready {
            import: Arc::new(RetainedImport { source, sha256 }),
        };
        drop(owner_guard);
        Ok(())
    }

    /// Publishes a complete Anytype export as an immutable staged download.
    pub(crate) async fn finish_export(
        &self,
        mut lease: StageWriteLease,
        destination: StagingPayload,
        observed_size: u64,
        sha256: String,
    ) -> Result<(), StagingError> {
        if lease.record.direction != StageDirection::Export || observed_size != lease.size_bytes {
            return Err(StagingError::Conflict);
        }
        let completion = begin_publication(&mut lease).await?;
        let owner_guard = PublicationOwnerGuard(Arc::clone(&completion));
        let completion_guard = PublicationCompletionGuard(completion);
        #[cfg(test)]
        let record_name = lease.record.record_name.clone();
        let task_guard = self.task_guard();
        let publication = tokio::task::spawn_blocking(move || {
            let mut task_guard = task_guard;
            let _completion_guard = completion_guard;
            #[cfg(test)]
            pause_publication_for_test(&record_name);
            let result = (|| {
                let source = destination
                    .into_anchored()
                    .map_err(|_| StagingError::Indeterminate)?;
                let independently_observed = hash_source(&source)?;
                Ok::<_, StagingError>((source, independently_observed))
            })();
            task_guard.complete();
            result
        })
        .await;
        let (source, independently_observed) = match publication {
            Ok(Ok(publication)) => publication,
            Ok(Err(_)) | Err(_) => {
                self.close_durable_authority();
                return Err(StagingError::Indeterminate);
            }
        };
        if independently_observed != sha256 {
            self.close_durable_authority();
            return Err(StagingError::Indeterminate);
        }
        self.persist_transition(&lease.record, |document| {
            document.state = DurableStageState::Available;
            document.committed_offset = observed_size;
            document.observed_sha256 = Some(independently_observed.clone());
        })
        .await?;
        let mut state = lease.record.state.lock().await;
        *state = RecordState::Available {
            source,
            sha256: independently_observed,
        };
        drop(owner_guard);
        Ok(())
    }

    /// Clones the retained file behind one authenticated available export.
    async fn export_reader(
        &self,
        handle: &str,
        route_record: Option<&str>,
    ) -> Result<(StageExportSource, StageStatus), StagingError> {
        let (_, record) = self.authenticate(handle, route_record).await?;
        if record.direction != StageDirection::Export {
            return Err(StagingError::NotFound);
        }
        let state = Arc::clone(&record.state).lock_owned().await;
        let RecordState::Available { source, .. } = &*state else {
            return Err(StagingError::NotFound);
        };
        source
            .verify_unchanged()
            .map_err(|_| StagingError::Conflict)?;
        let file = source
            .try_clone_reader()
            .map_err(|_| StagingError::Upstream)?;
        let status = status_for(&record, &state);
        Ok((StageExportSource { file, lease: state }, status))
    }

    /// Returns a retained source for one ready import record.
    pub(crate) async fn import_source(
        &self,
        handle: &str,
        space_id: &SpaceId,
    ) -> Result<StageSource, StagingError> {
        let (record_id, record) = self.authenticate(handle, None).await?;
        if record.direction != StageDirection::Import || &record.space_id != space_id {
            return Err(StagingError::NotFound);
        }
        let state = Arc::clone(&record.state).lock_owned().await;
        let RecordState::Ready { import } = &*state else {
            return Err(StagingError::NotFound);
        };
        import
            .source
            .verify_unchanged()
            .map_err(|_| StagingError::Conflict)?;
        let import = Arc::clone(import);
        Ok(StageSource {
            file: import
                .source
                .try_clone_reader()
                .map_err(|_| StagingError::Upstream)?,
            length: import.source.length,
            sha256: import.sha256.clone(),
            media_type: record.media_type.clone(),
            record: record_id,
            operation: [0; 32],
            restore_ready_on_drop: false,
            record_owner: Arc::clone(&record),
            #[cfg(test)]
            fail_reader_clone: false,
            lease: state,
        })
    }

    /// Binds an already-acquired source to its idempotency operation before
    /// dispatch.  The binding is one-way and is never exposed to another
    /// staging caller.
    pub(crate) async fn bind_import_operation(
        &self,
        source: &mut StageSource,
        operation: [u8; 32],
    ) -> Result<(), StagingError> {
        let RecordState::Ready { import } = &*source.lease else {
            return Err(StagingError::NotFound);
        };
        let import = Arc::clone(import);
        self.persist_transition(&source.record_owner, |document| {
            document.state = DurableStageState::Reconciliation;
            document.operation_fingerprint = Some(bytes_hex(&operation));
            document.uncertainty = Some("pre_dispatch".to_owned());
        })
        .await?;
        *source.lease = RecordState::Reconciliation { import, operation };
        source.operation = operation;
        source.restore_ready_on_drop = true;
        Ok(())
    }

    /// Marks the point immediately before the bound source's upload request is
    /// dispatched.  Later drops retain reconciliation authority.
    pub(crate) async fn mark_import_dispatched(
        &self,
        source: &mut StageSource,
    ) -> Result<(), StagingError> {
        let RecordState::Reconciliation { operation, .. } = &*source.lease else {
            return Err(StagingError::NotFound);
        };
        if operation != &source.operation {
            return Err(StagingError::NotFound);
        }
        self.persist_transition(&source.record_owner, |document| {
            document.uncertainty = Some("mutation_dispatched".to_owned());
        })
        .await?;
        source.restore_ready_on_drop = false;
        Ok(())
    }

    /// Restores a bound source after a definitive upload rejection proved that
    /// no candidate needs reconciliation.
    pub(crate) async fn restore_import_operation(
        &self,
        source: &mut StageSource,
    ) -> Result<(), StagingError> {
        if source
            .record_owner
            .durable
            .lock()
            .await
            .document
            .candidate_cleanup
            .as_deref()
            == Some("absence_ambiguous")
        {
            return Err(StagingError::Conflict);
        }
        let prior = std::mem::replace(
            &mut *source.lease,
            RecordState::Receiving {
                destination: None,
                offset: 0,
            },
        );
        let RecordState::Reconciliation { import, operation } = prior else {
            *source.lease = prior;
            return Err(StagingError::NotFound);
        };
        if operation != source.operation {
            *source.lease = RecordState::Reconciliation { import, operation };
            return Err(StagingError::NotFound);
        }
        if let Err(error) = self
            .persist_transition(&source.record_owner, |document| {
                document.state = DurableStageState::Ready;
                document.operation_fingerprint = None;
                document.candidate_id = None;
                document.candidate_cleanup = None;
                document.uncertainty = None;
            })
            .await
        {
            // Every real persist failure closes durable authority, but the
            // in-memory record must still describe the retained import rather
            // than a torn placeholder.
            *source.lease = RecordState::Reconciliation { import, operation };
            return Err(error);
        }
        *source.lease = RecordState::Ready { import };
        source.operation = [0; 32];
        source.restore_ready_on_drop = false;
        Ok(())
    }

    /// Persists the exact Anytype candidate returned after a dispatched
    /// import before verification or cleanup can proceed.
    pub(crate) async fn retain_import_candidate(
        &self,
        source: &StageSource,
        candidate: &EntityId,
    ) -> Result<(), StagingError> {
        let RecordState::Reconciliation { operation, .. } = &*source.lease else {
            return Err(StagingError::NotFound);
        };
        if operation != &source.operation {
            return Err(StagingError::NotFound);
        }
        self.persist_transition(&source.record_owner, |document| {
            document.candidate_id = Some(candidate.as_str().to_owned());
        })
        .await
    }

    /// Persists a closed candidate-cleanup category before or after the one
    /// permitted remote deletion attempt.
    pub(crate) async fn retain_candidate_cleanup(
        &self,
        source: &StageSource,
        category: &'static str,
    ) -> Result<(), StagingError> {
        if !matches!(category, "delete_dispatched" | "absence_ambiguous") {
            return Err(StagingError::BadRequest);
        }
        let RecordState::Reconciliation { operation, .. } = &*source.lease else {
            return Err(StagingError::NotFound);
        };
        if operation != &source.operation {
            return Err(StagingError::NotFound);
        }
        self.persist_transition(&source.record_owner, |document| {
            document.candidate_cleanup = Some(category.to_owned());
        })
        .await
    }

    /// Returns same-operation reconciliation metadata without reopening staged
    /// authority.  Wrong operations deliberately receive the same fixed
    /// NotFound response as an absent/stale handle.
    pub(crate) async fn reconciliation_import(
        &self,
        handle: &str,
        space_id: &SpaceId,
        operation: [u8; 32],
    ) -> Result<RetainedStageImport, StagingError> {
        let (record_id, record) = self.authenticate(handle, None).await?;
        if record.direction != StageDirection::Import || &record.space_id != space_id {
            return Err(StagingError::NotFound);
        }
        let records = self.state.records.read().await;
        if !records
            .get(&record_id)
            .is_some_and(|current| Arc::ptr_eq(current, &record))
        {
            return Err(StagingError::NotFound);
        }
        let state = record.state.lock().await;
        let (import, retained_operation) = match &*state {
            RecordState::Reconciliation { import, operation }
            | RecordState::Consumed { import, operation } => (import, operation),
            _ => return Err(StagingError::NotFound),
        };
        if retained_operation != &operation {
            return Err(StagingError::NotFound);
        }
        Ok(RetainedStageImport {
            length: import.source.length,
            sha256: import.sha256.clone(),
            media_type: record.media_type.clone(),
            record: record_hex(&record_id),
        })
    }

    /// Consumes a same-operation reconciliation record after candidate
    /// verification without reopening a readable staged source.
    pub(crate) async fn consume_reconciliation(
        &self,
        handle: &str,
        space_id: &SpaceId,
        operation: [u8; 32],
    ) -> Result<(), StagingError> {
        let (_, record) = self.authenticate(handle, None).await?;
        if record.direction != StageDirection::Import || &record.space_id != space_id {
            return Err(StagingError::NotFound);
        }
        let mut state = record.state.lock().await;
        let prior = std::mem::replace(
            &mut *state,
            RecordState::Receiving {
                destination: None,
                offset: 0,
            },
        );
        match prior {
            RecordState::Reconciliation {
                import,
                operation: retained,
            } if retained == operation => {
                if let Err(error) = self
                    .persist_transition(&record, |document| {
                        document.state = DurableStageState::Consumed;
                        document.operation_fingerprint = Some(bytes_hex(&operation));
                        document.uncertainty = None;
                    })
                    .await
                {
                    *state = RecordState::Reconciliation {
                        import,
                        operation: retained,
                    };
                    return Err(error);
                }
                *state = RecordState::Consumed { import, operation };
                Ok(())
            }
            RecordState::Consumed {
                import,
                operation: retained,
            } if retained == operation => {
                *state = RecordState::Consumed { import, operation };
                Ok(())
            }
            other => {
                *state = other;
                Err(StagingError::NotFound)
            }
        }
    }

    /// Reads authenticated import metadata without acquiring the one-use
    /// source authority.  This is the only staging read permitted before an
    /// idempotency ledger decision.
    pub(crate) async fn import_metadata(
        &self,
        handle: &str,
        space_id: &SpaceId,
    ) -> Result<RetainedStageImport, StagingError> {
        let (record_id, record) = self.authenticate(handle, None).await?;
        if record.direction != StageDirection::Import || &record.space_id != space_id {
            return Err(StagingError::NotFound);
        }
        let state = record.state.lock().await;
        let import = match &*state {
            RecordState::Ready { import }
            | RecordState::Reconciliation { import, .. }
            | RecordState::Consumed { import, .. } => import,
            _ => return Err(StagingError::NotFound),
        };
        Ok(RetainedStageImport {
            length: import.source.length,
            sha256: import.sha256.clone(),
            media_type: record.media_type.clone(),
            record: record_hex(&record_id),
        })
    }

    /// Marks one verified import source consumed while retaining the exact
    /// metadata required for same-key replay.
    pub(crate) async fn consume(&self, source: &mut StageSource) -> Result<(), StagingError> {
        if !matches!(*source.lease, RecordState::Reconciliation { .. }) {
            return Err(StagingError::NotFound);
        }
        let state = std::mem::replace(
            &mut *source.lease,
            RecordState::Receiving {
                destination: None,
                offset: 0,
            },
        );
        let RecordState::Reconciliation { import, operation } = state else {
            *source.lease = state;
            return Err(StagingError::NotFound);
        };
        if operation != source.operation {
            *source.lease = RecordState::Reconciliation { import, operation };
            return Err(StagingError::NotFound);
        }
        if let Err(error) = self
            .persist_transition(&source.record_owner, |document| {
                document.state = DurableStageState::Consumed;
                document.operation_fingerprint = Some(bytes_hex(&source.operation));
                document.uncertainty = None;
            })
            .await
        {
            *source.lease = RecordState::Reconciliation { import, operation };
            return Err(error);
        }
        *source.lease = RecordState::Consumed { import, operation };
        source.restore_ready_on_drop = false;
        Ok(())
    }

    /// Releases one exact authenticated record and removes its private file.
    pub(crate) async fn release(&self, handle: &str) -> Result<(), StagingError> {
        let (record_id, record) = self.authenticate(handle, None).await?;
        let records = self.state.records.write().await;
        if !records
            .get(&record_id)
            .is_some_and(|current| Arc::ptr_eq(current, &record))
        {
            return Err(StagingError::NotFound);
        }
        if record.cleanup_blocked.load(Ordering::Acquire) {
            return Err(StagingError::Conflict);
        }
        let Ok(mut state) = record.state.try_lock() else {
            return Err(StagingError::Conflict);
        };
        if !transition_to_cleanup_pending(&mut state) {
            return Err(StagingError::Conflict);
        }
        record.cleanup_blocked.store(true, Ordering::Release);
        drop(records);
        drop(state);
        let cleanup = self.spawn_cleanup_coordinator(record_id, record);
        match cleanup.await {
            Ok(true) => Ok(()),
            Ok(false) => Err(StagingError::Conflict),
            Err(error) => {
                if error.is_panic() {
                    self.state
                        .durability_uncertain
                        .store(true, Ordering::Release);
                    self.state.active.store(false, Ordering::Release);
                    self.state.shutdown.cancel();
                }
                Err(StagingError::Upstream)
            }
        }
    }

    fn take_expired_locked(
        &self,
        records: &mut HashMap<[u8; RECORD_BYTES], Arc<StageRecord>>,
        now: Instant,
    ) -> Vec<([u8; RECORD_BYTES], Arc<StageRecord>)> {
        let candidates = records
            .iter()
            .filter_map(|(id, record)| (record.expires <= now).then_some(*id))
            .collect::<Vec<_>>();
        let mut expired = Vec::with_capacity(self.state.limits.cleanup_batch);
        for id in candidates {
            if expired.len() >= self.state.limits.cleanup_batch {
                break;
            }
            let claimed = records
                .get(&id)
                .is_some_and(|record| claim_expired_record(record));
            if claimed && let Some(record) = records.get(&id) {
                expired.push((id, Arc::clone(record)));
            }
        }
        expired
    }

    async fn cleanup_expired(&self, expired: Vec<([u8; RECORD_BYTES], Arc<StageRecord>)>) {
        let mut reaped = 0_usize;
        for (id, record) in expired {
            if self.cleanup_coordinator(id, record).await {
                reaped = reaped.saturating_add(1);
            }
        }
        if reaped > 0 {
            tracing::info!(
                target: "any_mcp::operation",
                operation = "artifact_staging_cleanup",
                outcome = "expired_reaped",
                cleanup_count = reaped,
                "Artifact staging cleanup completed"
            );
        }
    }

    fn spawn_expired_cleanup(
        &self,
        expired: Vec<([u8; RECORD_BYTES], Arc<StageRecord>)>,
    ) -> Option<tokio::task::JoinHandle<()>> {
        if expired.is_empty() {
            return None;
        }
        let staging = self.clone();
        let task_guard = self.task_guard();
        Some(tokio::spawn(async move {
            let mut task_guard = task_guard;
            staging.cleanup_expired(expired).await;
            task_guard.complete();
        }))
    }

    async fn cleanup_pending_record(&self, record: &Arc<StageRecord>) -> bool {
        if self.prepare_cleanup(record).await.is_err() {
            self.close_durable_authority();
            return false;
        }
        let target = {
            let mut state = record.state.lock().await;
            if !transition_to_cleanup_pending(&mut state) {
                return false;
            }
            // Classify the unlink target before durably closing pathname
            // authority: a record that cannot produce a target must stay in
            // the retryable tombstone-pending phase instead of persisting
            // terminal evidence it can never act on.
            enum TargetKind {
                Temporary,
                Published,
                Surrendered(StagingFileIdentity),
            }
            let kind = match &*state {
                RecordState::CleanupPending {
                    destination: Some(_),
                    source: None,
                    pathname_cleanup_unsafe: false,
                } => TargetKind::Temporary,
                RecordState::CleanupPending {
                    destination: None,
                    source: Some(_),
                    pathname_cleanup_unsafe: false,
                } => TargetKind::Published,
                RecordState::CleanupPending {
                    destination: None,
                    source: None,
                    pathname_cleanup_unsafe: false,
                } => {
                    // Both live handles were surrendered: a failed transfer
                    // consumed the write lease, or shutdown dropped an active
                    // upload. The durable record still indexes the payload's
                    // stable identity, so cleanup reopens the fixed name and
                    // revalidates that identity before unlinking.
                    let durable = record.durable.lock().await;
                    match durable.document.payload_identity {
                        Some(identity) => TargetKind::Surrendered(StagingFileIdentity {
                            volume: identity.volume,
                            file: identity.file,
                        }),
                        None => return false,
                    }
                }
                _ => return false,
            };
            // Identity validation and unlink are separate pathname
            // operations. Permanently close pathname authority — durably,
            // then in memory, both before releasing this lock — so a failed,
            // cancelled, or panicked attempt retains its record rather than
            // retrying by name.
            if self
                .persist_transition(record, |document| {
                    document.cleanup_evidence = Some("pathname_authority_closed".to_owned());
                })
                .await
                .is_err()
            {
                self.close_durable_authority();
                return false;
            }
            match (kind, &mut *state) {
                (
                    TargetKind::Temporary,
                    RecordState::CleanupPending {
                        destination,
                        pathname_cleanup_unsafe,
                        ..
                    },
                ) => {
                    let Some(destination) = destination.take() else {
                        return false;
                    };
                    *pathname_cleanup_unsafe = true;
                    PendingCleanupTarget::Temporary {
                        name: record.record_name.clone(),
                        destination,
                    }
                }
                (
                    TargetKind::Published,
                    RecordState::CleanupPending {
                        source,
                        pathname_cleanup_unsafe,
                        ..
                    },
                ) => {
                    let Some(source) = source.take() else {
                        return false;
                    };
                    *pathname_cleanup_unsafe = true;
                    PendingCleanupTarget::Published {
                        name: record.record_name.clone(),
                        source,
                    }
                }
                (
                    TargetKind::Surrendered(identity),
                    RecordState::CleanupPending {
                        pathname_cleanup_unsafe,
                        ..
                    },
                ) => {
                    *pathname_cleanup_unsafe = true;
                    PendingCleanupTarget::Surrendered {
                        name: record.record_name.clone(),
                        identity,
                    }
                }
                _ => return false,
            }
        };
        let directory = self.state.directory.clone();
        let result = tokio::task::spawn_blocking(move || match target {
            PendingCleanupTarget::Temporary { name, destination } => {
                match destination.into_anchored() {
                    Ok(source) if directory.remove_exact_record(&name, &source).is_ok() => {
                        PendingCleanupResult::Removed
                    }
                    Ok(source) => PendingCleanupResult::RetainPublished(source),
                    Err(error) => PendingCleanupResult::RetainTemporary(error.payload),
                }
            }
            PendingCleanupTarget::Published { name, source } => {
                if directory.remove_exact_record(&name, &source).is_ok() {
                    PendingCleanupResult::Removed
                } else {
                    PendingCleanupResult::RetainPublished(source)
                }
            }
            PendingCleanupTarget::Surrendered { name, identity } => {
                if directory.remove_exact_payload(&name, identity).is_ok() {
                    PendingCleanupResult::Removed
                } else {
                    PendingCleanupResult::RetainSurrendered
                }
            }
        })
        .await;
        match result {
            Ok(PendingCleanupResult::Removed) => {
                if self.finish_durable_cleanup(record).await.is_ok() {
                    true
                } else {
                    self.close_durable_authority();
                    false
                }
            }
            Ok(PendingCleanupResult::RetainPublished(source)) => {
                let mut state = record.state.lock().await;
                if let RecordState::CleanupPending {
                    source: slot,
                    pathname_cleanup_unsafe,
                    ..
                } = &mut *state
                {
                    *slot = Some(source);
                    *pathname_cleanup_unsafe = true;
                }
                false
            }
            Ok(PendingCleanupResult::RetainTemporary(destination)) => {
                let mut state = record.state.lock().await;
                if let RecordState::CleanupPending {
                    destination: slot,
                    pathname_cleanup_unsafe,
                    ..
                } = &mut *state
                {
                    *slot = Some(destination);
                    *pathname_cleanup_unsafe = true;
                }
                false
            }
            Ok(PendingCleanupResult::RetainSurrendered) | Err(_) => false,
        }
    }

    async fn prepare_cleanup(&self, record: &StageRecord) -> Result<(), StagingError> {
        if record.tombstone.lock().await.is_some() {
            return Ok(());
        }
        self.persist_transition(record, |document| {
            document.state = DurableStageState::CleanupPending;
            document.cleanup_evidence = Some("tombstone_pending".to_owned());
        })
        .await?;
        let durable = record.durable.lock().await;
        let tombstone = DurableTombstone {
            format_version: DURABLE_RECORD_VERSION,
            record_id: durable.document.record_id.clone(),
            payload_identity: durable.document.payload_identity,
            record_identity: DurableFileIdentity::from(durable.source.staging_identity()),
        };
        let bytes = durable_json(&tombstone)?;
        let record_id = durable.document.record_id.clone();
        drop(durable);
        let directory = self.state.directory.clone();
        let task_guard = self.task_guard();
        let publication = tokio::task::spawn_blocking(move || {
            let mut task_guard = task_guard;
            let result = directory.publish_tombstone(&record_id, &bytes);
            task_guard.complete();
            result
        })
        .await;
        let source = match publication {
            Ok(Ok(source)) => source,
            Ok(Err(_)) | Err(_) => {
                self.close_durable_authority();
                return Err(StagingError::Indeterminate);
            }
        };
        *record.tombstone.lock().await = Some(source);
        Ok(())
    }

    async fn finish_durable_cleanup(&self, record: &StageRecord) -> Result<(), StagingError> {
        let durable = record.durable.lock().await;
        self.state
            .directory
            .remove_exact_record_state(&durable.document.record_id, &durable.source)
            .map_err(|_| StagingError::Indeterminate)?;
        let record_id = durable.document.record_id.clone();
        drop(durable);
        let mut tombstone = record.tombstone.lock().await;
        let source = tombstone.as_ref().ok_or(StagingError::Indeterminate)?;
        self.state
            .directory
            .remove_exact_tombstone(&record_id, source)
            .map_err(|_| StagingError::Indeterminate)?;
        *tombstone = None;
        Ok(())
    }

    fn spawn_cleanup_coordinator(
        &self,
        id: [u8; RECORD_BYTES],
        record: Arc<StageRecord>,
    ) -> tokio::task::JoinHandle<bool> {
        let staging = self.clone();
        let task_guard = self.cleanup_task_guard();
        tokio::spawn(async move {
            let mut task_guard = task_guard;
            let result = staging.cleanup_coordinator(id, record).await;
            task_guard.complete();
            result
        })
    }

    async fn cleanup_coordinator(&self, id: [u8; RECORD_BYTES], record: Arc<StageRecord>) -> bool {
        let _claim = CleanupClaimGuard(Arc::clone(&record.cleanup_blocked));
        #[cfg(test)]
        pause_cleanup_for_test(&record.record_name);
        let removed = self.cleanup_pending_record(&record).await;
        if removed {
            self.remove_owned_record(id, &record).await;
        } else {
            tracing::warn!(
                target: "any_mcp::operation",
                operation = "artifact_staging_cleanup",
                outcome = "retained",
                "Artifact staging cleanup retained private on-disk evidence"
            );
        }
        removed
    }

    async fn remove_owned_record(&self, id: [u8; RECORD_BYTES], record: &Arc<StageRecord>) {
        let mut records = self.state.records.write().await;
        if records
            .get(&id)
            .is_some_and(|current| Arc::ptr_eq(current, record))
        {
            records.remove(&id);
        }
    }
}

fn claim_expired_record(record: &StageRecord) -> bool {
    if record
        .cleanup_blocked
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return false;
    }
    // A held state lock means a live settlement or export stream still owns
    // this record. Claiming it anyway would persist cleanup evidence under an
    // active transition, so leave it for a later expiry pass.
    let Ok(mut state) = record.state.try_lock() else {
        record.cleanup_blocked.store(false, Ordering::Release);
        return false;
    };
    if !transition_to_cleanup_pending(&mut state) {
        record.cleanup_blocked.store(false, Ordering::Release);
        return false;
    }
    true
}

fn claim_shutdown_record(record: &StageRecord) -> bool {
    if record
        .cleanup_blocked
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return false;
    }
    let Ok(mut state) = record.state.try_lock() else {
        record.cleanup_blocked.store(false, Ordering::Release);
        return false;
    };
    if matches!(
        *state,
        RecordState::Reconciliation { .. } | RecordState::PublicationIndeterminate { .. }
    ) || !transition_to_cleanup_pending(&mut state)
    {
        record.cleanup_blocked.store(false, Ordering::Release);
        return false;
    }
    true
}

fn transition_to_cleanup_pending(state: &mut RecordState) -> bool {
    let prior = std::mem::replace(
        state,
        RecordState::CleanupPending {
            destination: None,
            source: None,
            pathname_cleanup_unsafe: false,
        },
    );
    let (destination, source, pathname_cleanup_unsafe) = match prior {
        RecordState::Receiving { destination, .. } => (destination, None, false),
        RecordState::PublicationIndeterminate { completion } if completion.settled() => {
            (None, None, true)
        }
        RecordState::Ready { import } => match Arc::try_unwrap(import) {
            Ok(import) => (None, Some(import.source), false),
            Err(import) => {
                *state = RecordState::Ready { import };
                return false;
            }
        },
        RecordState::Reconciliation { import, operation } => match Arc::try_unwrap(import) {
            Ok(import) => (None, Some(import.source), false),
            Err(import) => {
                *state = RecordState::Reconciliation { import, operation };
                return false;
            }
        },
        RecordState::Consumed { import, operation } => match Arc::try_unwrap(import) {
            Ok(import) => (None, Some(import.source), false),
            Err(import) => {
                *state = RecordState::Consumed { import, operation };
                return false;
            }
        },
        RecordState::Available { source, .. } => (None, Some(source), false),
        prior @ RecordState::CleanupPending { .. } => {
            *state = prior;
            return true;
        }
        prior @ RecordState::PublicationIndeterminate { .. } => {
            *state = prior;
            return false;
        }
    };
    *state = RecordState::CleanupPending {
        destination,
        source,
        pathname_cleanup_unsafe,
    };
    true
}

enum PendingCleanupTarget {
    Temporary {
        name: String,
        destination: StagingPayload,
    },
    Published {
        name: String,
        source: AnchoredImport,
    },
    /// Both live handles were surrendered; the durable record's indexed
    /// payload identity authorizes one revalidated unlink by fixed name.
    Surrendered {
        name: String,
        identity: StagingFileIdentity,
    },
}

enum PendingCleanupResult {
    Removed,
    RetainPublished(AnchoredImport),
    RetainTemporary(StagingPayload),
    /// The revalidated unlink failed; no live handle exists to restore.
    RetainSurrendered,
}

struct IncomingWrite {
    destination: StagingPayload,
    written: u64,
    error: Option<StagingError>,
}

async fn write_incoming(
    mut body: Incoming,
    destination: StagingPayload,
    maximum: u64,
    no_progress_timeout: Duration,
    operation_deadline: tokio::time::Instant,
    shutdown: &CancellationToken,
    task_guard: StagingTaskGuard,
) -> Result<IncomingWrite, StagingError> {
    let (sender, mut receiver) = tokio::sync::mpsc::channel::<Bytes>(1);
    let writer = tokio::task::spawn_blocking(move || {
        let mut task_guard = task_guard;
        let mut destination = destination;
        let mut written = 0_u64;
        let mut error = None;
        while let Some(chunk) = receiver.blocking_recv() {
            if destination.write_all(&chunk).is_err() {
                error = Some(StagingError::Upstream);
                break;
            }
            let Some(total) = written.checked_add(chunk.len() as u64) else {
                error = Some(StagingError::Bounded);
                break;
            };
            written = total;
        }
        let outcome = IncomingWrite {
            destination,
            written,
            error,
        };
        task_guard.complete();
        outcome
    });
    let mut admitted = 0_u64;
    let mut receive_error = None;
    loop {
        let frame = tokio::select! {
            biased;
            () = shutdown.cancelled() => {
                receive_error = Some(StagingError::Timeout);
                break;
            }
            () = tokio::time::sleep_until(operation_deadline) => {
                receive_error = Some(StagingError::Timeout);
                break;
            }
            () = tokio::time::sleep(no_progress_timeout) => {
                receive_error = Some(StagingError::Timeout);
                break;
            }
            frame = body.frame() => frame,
        };
        let frame = match frame {
            Some(Ok(frame)) => frame,
            Some(Err(_)) => {
                receive_error = Some(StagingError::BadRequest);
                break;
            }
            None => break,
        };
        let data = match frame.into_data() {
            Ok(data) => data,
            Err(_) => {
                receive_error = Some(StagingError::Conflict);
                break;
            }
        };
        let Some(proposed) = admitted.checked_add(data.len() as u64) else {
            receive_error = Some(StagingError::Bounded);
            break;
        };
        if proposed > maximum {
            receive_error = Some(StagingError::Bounded);
            break;
        }
        let send = sender.send(data);
        tokio::pin!(send);
        tokio::select! {
            biased;
            () = shutdown.cancelled() => {
                receive_error = Some(StagingError::Timeout);
                break;
            }
            () = tokio::time::sleep_until(operation_deadline) => {
                receive_error = Some(StagingError::Timeout);
                break;
            }
            result = &mut send => {
                if result.is_err() {
                    receive_error = Some(StagingError::Upstream);
                    break;
                }
            }
        }
        admitted = proposed;
    }
    drop(sender);
    let mut outcome = writer.await.map_err(|_| StagingError::Upstream)?;
    if outcome.error.is_none() {
        outcome.error = receive_error;
    }
    Ok(outcome)
}

fn allowed_hosts(config: &StagingConfig) -> Result<Vec<String>, StagingError> {
    let mut hosts = vec![config.bind.to_string()];
    let base = config
        .public_base_url
        .as_deref()
        .ok_or(StagingError::Upstream)?;
    let parsed = url::Url::parse(base).map_err(|_| StagingError::Upstream)?;
    let host = parsed.host_str().ok_or(StagingError::Upstream)?;
    let authority = match (host.contains(':'), parsed.port()) {
        (true, Some(port)) => format!("[{host}]:{port}"),
        (true, None) => format!("[{host}]"),
        (false, Some(port)) => format!("{host}:{port}"),
        (false, None) => host.to_owned(),
    };
    if !hosts.iter().any(|existing| existing == &authority) {
        hosts.push(authority);
    }
    Ok(hosts)
}

fn valid_header_block(request: &Request<Incoming>, limits: &ArtifactLimits) -> bool {
    if request.headers().len() > 32 {
        return false;
    }
    let retained = request
        .headers()
        .iter()
        .try_fold(0_usize, |total, (name, value)| {
            total
                .checked_add(name.as_str().len())
                .and_then(|total| total.checked_add(value.as_bytes().len()))
                .and_then(|total| total.checked_add(4))
        });
    retained.is_some_and(|retained| retained <= limits.staging_header_bytes)
}

fn single_header(
    headers: &hyper::HeaderMap,
    name: hyper::header::HeaderName,
    maximum: usize,
) -> Result<Option<&str>, StagingError> {
    let values = headers.get_all(name).iter().collect::<Vec<_>>();
    if values.len() > 1 {
        return Err(StagingError::Conflict);
    }
    values
        .first()
        .map(|value| {
            if value.as_bytes().len() > maximum {
                return Err(StagingError::Bounded);
            }
            value.to_str().map_err(|_| StagingError::Conflict)
        })
        .transpose()
}

fn parse_single_u64(
    headers: &hyper::HeaderMap,
    name: hyper::header::HeaderName,
    maximum: usize,
) -> Result<Option<u64>, StagingError> {
    single_header(headers, name, maximum)?
        .map(|value| {
            if value.len() > 1 && value.starts_with('0') {
                return Err(StagingError::Conflict);
            }
            value.parse::<u64>().map_err(|_| StagingError::Conflict)
        })
        .transpose()
}

fn parse_content_range(value: &str, expected_total: u64) -> Option<(u64, u64)> {
    let range = value.strip_prefix("bytes ")?;
    let (bounds, total) = range.split_once('/')?;
    if total.parse::<u64>().ok()? != expected_total {
        return None;
    }
    let (start, end) = bounds.split_once('-')?;
    let start = start.parse::<u64>().ok()?;
    let end = end.parse::<u64>().ok()?;
    let length = end.checked_sub(start)?.checked_add(1)?;
    Some((start, length))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DownloadRange {
    Valid((u64, u64)),
    Malformed,
    Unsatisfiable,
}

fn parse_download_range(value: &str, total: u64) -> DownloadRange {
    let Some(range) = value.strip_prefix("bytes=") else {
        return DownloadRange::Malformed;
    };
    if range.contains(',') {
        return DownloadRange::Malformed;
    }
    let Some((start, end)) = range.split_once('-') else {
        return DownloadRange::Malformed;
    };
    if end.contains('-') || (start.is_empty() && end.is_empty()) {
        return DownloadRange::Malformed;
    }
    let (start, end) = match (start.is_empty(), end.is_empty()) {
        (false, false) => {
            let (Ok(start), Ok(end)) = (start.parse::<u64>(), end.parse::<u64>()) else {
                return DownloadRange::Malformed;
            };
            (start, end)
        }
        (false, true) => {
            let Ok(start) = start.parse::<u64>() else {
                return DownloadRange::Malformed;
            };
            let Some(end) = total.checked_sub(1) else {
                return DownloadRange::Unsatisfiable;
            };
            (start, end)
        }
        (true, false) => {
            let Ok(suffix) = end.parse::<u64>() else {
                return DownloadRange::Malformed;
            };
            if suffix == 0 {
                return DownloadRange::Unsatisfiable;
            }
            let Some(last) = total.checked_sub(1) else {
                return DownloadRange::Unsatisfiable;
            };
            (total.saturating_sub(suffix), last)
        }
        (true, true) => return DownloadRange::Malformed,
    };
    if start > end || end >= total {
        return DownloadRange::Unsatisfiable;
    }
    match end
        .checked_sub(start)
        .and_then(|length| length.checked_add(1))
    {
        Some(length) => DownloadRange::Valid((start, length)),
        None => DownloadRange::Unsatisfiable,
    }
}

fn full_body(bytes: Bytes) -> StagingBody {
    Full::new(bytes)
        .map_err(|never: Infallible| match never {})
        .boxed_unsync()
}

fn fixed_response(status: StatusCode) -> Response<StagingBody> {
    let body = match status {
        StatusCode::NO_CONTENT => Bytes::new(),
        StatusCode::UNAUTHORIZED => Bytes::from_static(b"unauthorized\n"),
        StatusCode::FORBIDDEN => Bytes::from_static(b"forbidden\n"),
        StatusCode::NOT_FOUND => Bytes::from_static(b"not found\n"),
        StatusCode::METHOD_NOT_ALLOWED => Bytes::from_static(b"method not allowed\n"),
        StatusCode::CONFLICT => Bytes::from_static(b"conflict\n"),
        StatusCode::PAYLOAD_TOO_LARGE => Bytes::from_static(b"payload too large\n"),
        StatusCode::TOO_MANY_REQUESTS => Bytes::from_static(b"rate limited\n"),
        StatusCode::SERVICE_UNAVAILABLE => Bytes::from_static(b"unavailable\n"),
        StatusCode::REQUEST_TIMEOUT => Bytes::from_static(b"timeout\n"),
        StatusCode::RANGE_NOT_SATISFIABLE => Bytes::from_static(b"invalid range\n"),
        StatusCode::INSUFFICIENT_STORAGE => Bytes::from_static(b"quota exhausted\n"),
        StatusCode::INTERNAL_SERVER_ERROR => Bytes::from_static(b"internal error\n"),
        _ => Bytes::from_static(b"invalid request\n"),
    };
    let mut response = Response::new(full_body(body));
    *response.status_mut() = status;
    response
}

fn staging_http_error(error: StagingError) -> Response<StagingBody> {
    fixed_response(match error {
        StagingError::Disabled | StagingError::NotFound => StatusCode::NOT_FOUND,
        StagingError::BadRequest => StatusCode::BAD_REQUEST,
        StagingError::Conflict => StatusCode::CONFLICT,
        StagingError::Bounded => StatusCode::INSUFFICIENT_STORAGE,
        StagingError::Timeout => StatusCode::REQUEST_TIMEOUT,
        StagingError::InvalidPolicy
        | StagingError::Reconciliation
        | StagingError::Upstream
        | StagingError::Indeterminate => StatusCode::INTERNAL_SERVER_ERROR,
    })
}

fn set_header(
    response: &mut Response<StagingBody>,
    name: hyper::header::HeaderName,
    value: &str,
) -> Result<(), StagingError> {
    let value = hyper::header::HeaderValue::from_str(value).map_err(|_| StagingError::Upstream)?;
    response.headers_mut().insert(name, value);
    Ok(())
}

fn offset_response(status: StatusCode, offset: u64) -> Response<StagingBody> {
    let mut response = fixed_response(status);
    if set_header(
        &mut response,
        hyper::header::HeaderName::from_static("upload-offset"),
        &offset.to_string(),
    )
    .is_err()
    {
        return fixed_response(StatusCode::INTERNAL_SERVER_ERROR);
    }
    response
}

fn status_response(code: StatusCode, status: &StageStatus, head: bool) -> Response<StagingBody> {
    let mut response = if head {
        fixed_response(code)
    } else {
        fixed_response(StatusCode::NO_CONTENT)
    };
    *response.status_mut() = code;
    let direction = match status.direction {
        StageDirection::Import => "import",
        StageDirection::Export => "export",
    };
    for (name, value) in [
        ("x-artifact-direction", direction.to_owned()),
        ("x-artifact-state", status.state.to_owned()),
        ("upload-offset", status.offset.to_string()),
        ("x-artifact-size", status.size_bytes.to_string()),
        ("x-artifact-expires", status.expires_at.to_rfc3339()),
    ] {
        if set_header(
            &mut response,
            hyper::header::HeaderName::from_static(name),
            &value,
        )
        .is_err()
        {
            return fixed_response(StatusCode::INTERNAL_SERVER_ERROR);
        }
    }
    response
}

fn hash_source(source: &AnchoredImport) -> Result<String, StagingError> {
    let mut reader = source
        .try_clone_reader()
        .map_err(|_| StagingError::Upstream)?;
    hash_file(&mut reader, source.length)
}

fn hash_file(reader: &mut File, expected_length: u64) -> Result<String, StagingError> {
    reader
        .seek(SeekFrom::Start(0))
        .map_err(|_| StagingError::Upstream)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 1024 * 1024];
    let mut observed = 0_u64;
    loop {
        let read = reader
            .read(&mut buffer)
            .map_err(|_| StagingError::Upstream)?;
        if read == 0 {
            break;
        }
        observed = observed
            .checked_add(read as u64)
            .ok_or(StagingError::Bounded)?;
        if observed > expected_length {
            return Err(StagingError::Conflict);
        }
        hasher.update(&buffer[..read]);
    }
    if observed != expected_length {
        return Err(StagingError::Conflict);
    }
    let digest = hasher.finalize();
    let mut encoded = String::with_capacity(64);
    for byte in digest {
        let _ = std::fmt::Write::write_fmt(&mut encoded, format_args!("{byte:02x}"));
    }
    Ok(encoded)
}

fn status_for(record: &StageRecord, state: &RecordState) -> StageStatus {
    let (state_name, offset, sha256) = match state {
        RecordState::Receiving { offset, .. } => ("receiving", *offset, None),
        RecordState::Ready { import } => ("ready", record.size_bytes, Some(import.sha256.clone())),
        RecordState::Reconciliation { import, .. } => (
            "reconciliation",
            record.size_bytes,
            Some(import.sha256.clone()),
        ),
        RecordState::Available { sha256, .. } => {
            ("available", record.size_bytes, Some(sha256.clone()))
        }
        RecordState::PublicationIndeterminate { .. } => ("receiving", record.size_bytes, None),
        RecordState::CleanupPending { .. } => ("receiving", record.size_bytes, None),
        RecordState::Consumed { .. } => ("consumed", record.size_bytes, None),
    };
    StageStatus {
        direction: record.direction,
        state: state_name,
        offset,
        size_bytes: record.size_bytes,
        sha256,
        media_type: record.media_type.clone(),
        expires_at: record.expires_at,
    }
}

fn wall_expiry(created_at: DateTime<Utc>, ttl: Duration) -> Result<DateTime<Utc>, StagingError> {
    let ttl = chrono::Duration::from_std(ttl).map_err(|_| StagingError::Upstream)?;
    created_at
        .checked_add_signed(ttl)
        .ok_or(StagingError::Upstream)
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use reqwest::header::{
        AUTHORIZATION, CONTENT_RANGE, CONTENT_TYPE, HOST, HeaderValue, ORIGIN, RANGE,
    };

    use super::*;
    use crate::{artifact_config::ArtifactConfig, artifact_roots::RootRegistry};

    struct TestStaging {
        staging: ArtifactStaging,
        shutdown: CancellationToken,
        root: PathBuf,
    }

    impl Drop for TestStaging {
        fn drop(&mut self) {
            self.shutdown.cancel();
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    async fn test_staging() -> TestStaging {
        let suffix = getrandom::u64().expect("test randomness");
        let root = std::env::temp_dir().join(format!("any-mcp-stage-{suffix:016x}"));
        std::fs::create_dir(&root).expect("create staging root");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))
                .expect("make staging root owner-private");
        }
        let probe = std::net::TcpListener::bind("127.0.0.1:0").expect("bind port probe");
        let port = probe.local_addr().expect("probe address").port();
        drop(probe);
        let base = format!("http://127.0.0.1:{port}/artifacts/v1/");
        let root_toml = root.to_string_lossy().replace('\\', "\\\\");
        let config = ArtifactConfig::from_toml(&format!(
            "schema_version = 1\n\
             [spaces]\n\
             read_only = false\n\
             [staging]\n\
             enabled = true\n\
             root = \"{root_toml}\"\n\
             bind = \"127.0.0.1:{port}\"\n\
             public_base_url = \"{base}\"\n"
        ))
        .expect("staging config");
        let roots = RootRegistry::activate(&config).expect("activate empty local roots");
        let shutdown = CancellationToken::new();
        let staging = ArtifactStaging::activate(
            config.staging().expect("staging declaration"),
            &config.limits,
            &roots,
            shutdown.clone(),
        )
        .await
        .expect("activate staging");
        TestStaging {
            staging,
            shutdown,
            root,
        }
    }

    fn space_id() -> SpaceId {
        SpaceId::new("bafyreid5fvqlnsobih2keakcxjrrlpmly6kf37klzjzen4ibfdgalcdp4y.2tq5w93cr6oe7")
            .expect("space id")
    }

    fn payload_path(root: &Path, record: &str) -> PathBuf {
        root.join("payloads").join(format!("{record}.bin"))
    }

    fn assert_empty_closed_layout(root: &Path) {
        assert!(root.join("instance.lock").is_file());
        for directory in ["records", "payloads", "tmp", "tombstones"] {
            assert!(
                std::fs::read_dir(root.join(directory))
                    .expect("inspect staging layout directory")
                    .next()
                    .is_none(),
                "staging layout directory was not empty: {directory}"
            );
        }
    }

    fn install_publication_pause(
        record_name: &str,
    ) -> (Arc<std::sync::Barrier>, Arc<std::sync::Barrier>) {
        let entered = Arc::new(std::sync::Barrier::new(2));
        let release = Arc::new(std::sync::Barrier::new(2));
        let pause = PublicationTestPause {
            record_name: record_name.to_owned(),
            entered: Arc::clone(&entered),
            release: Arc::clone(&release),
        };
        *PUBLICATION_TEST_PAUSE
            .get_or_init(|| std::sync::Mutex::new(None))
            .lock()
            .expect("publication pause lock") = Some(pause);
        (entered, release)
    }

    fn clear_publication_pause() {
        *PUBLICATION_TEST_PAUSE
            .get_or_init(|| std::sync::Mutex::new(None))
            .lock()
            .expect("publication pause lock") = None;
    }

    fn install_cleanup_pause(
        record_name: &str,
    ) -> (Arc<std::sync::Barrier>, Arc<std::sync::Barrier>) {
        let entered = Arc::new(std::sync::Barrier::new(2));
        let release = Arc::new(std::sync::Barrier::new(2));
        let pause = CleanupTestPause {
            record_name: record_name.to_owned(),
            entered: Arc::clone(&entered),
            release: Arc::clone(&release),
        };
        *CLEANUP_TEST_PAUSE
            .get_or_init(|| std::sync::Mutex::new(None))
            .lock()
            .expect("cleanup pause lock") = Some(pause);
        (entered, release)
    }

    fn clear_cleanup_pause() {
        *CLEANUP_TEST_PAUSE
            .get_or_init(|| std::sync::Mutex::new(None))
            .lock()
            .expect("cleanup pause lock") = None;
    }

    async fn raw_staging_status(url: &str, request: Vec<u8>) -> StatusCode {
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

        let parsed = url::Url::parse(url).expect("parse private staging URL");
        let host = parsed.host_str().expect("staging URL host");
        let port = parsed.port_or_known_default().expect("staging URL port");
        let mut stream = tokio::net::TcpStream::connect((host, port))
            .await
            .expect("connect raw staging request");
        stream
            .write_all(&request)
            .await
            .expect("write raw staging request");
        stream.shutdown().await.expect("finish raw staging request");
        let mut response = Vec::new();
        tokio::time::timeout(Duration::from_secs(2), stream.read_to_end(&mut response))
            .await
            .expect("raw staging response deadline")
            .expect("read raw staging response");
        let first = response
            .split(|byte| *byte == b'\n')
            .next()
            .expect("raw staging status line");
        match first {
            line if line.starts_with(b"HTTP/1.1 400 ") => StatusCode::BAD_REQUEST,
            line if line.starts_with(b"HTTP/1.1 401 ") => StatusCode::UNAUTHORIZED,
            line if line.starts_with(b"HTTP/1.1 404 ") => StatusCode::NOT_FOUND,
            line if line.starts_with(b"HTTP/1.1 405 ") => StatusCode::METHOD_NOT_ALLOWED,
            line if line.starts_with(b"HTTP/1.1 429 ") => StatusCode::TOO_MANY_REQUESTS,
            line if line.starts_with(b"HTTP/1.1 503 ") => StatusCode::SERVICE_UNAVAILABLE,
            _ => panic!(
                "unexpected raw staging status category: {}",
                String::from_utf8_lossy(first)
            ),
        }
    }

    #[tokio::test]
    async fn available_quota_tracks_and_releases_private_reservations() {
        let test = test_staging().await;
        let before = test.staging.available_quota().await;
        let allocation = test
            .staging
            .allocate_import(space_id(), 5, Some("text/plain".to_owned()), None)
            .await
            .expect("allocate quota fixture");
        let during = test.staging.available_quota().await;
        assert_eq!(during.0, before.0 - 5);
        assert_eq!(during.1, before.1 - 1);
        test.staging
            .release(&allocation.handle)
            .await
            .expect("release quota fixture");
        assert_eq!(test.staging.available_quota().await, before);
    }

    #[tokio::test]
    async fn staged_import_accepts_sequential_ranges_and_consumes_once() {
        let test = test_staging().await;
        let quota_before = test.staging.available_quota().await;
        let expected = "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824";
        let allocation = test
            .staging
            .allocate_import(
                space_id(),
                5,
                Some("text/plain".to_owned()),
                Some(expected.to_owned()),
            )
            .await
            .expect("allocate import");
        let client = reqwest::Client::new();
        let first = client
            .put(&allocation.url)
            .header(AUTHORIZATION, format!("Bearer {}", allocation.handle))
            .header(CONTENT_TYPE, "text/plain")
            .header(CONTENT_RANGE, "bytes 0-1/5")
            .body("he")
            .send()
            .await
            .expect("first ranged upload");
        assert_eq!(first.status(), StatusCode::NO_CONTENT);
        assert_eq!(first.headers()["upload-offset"], "2");

        let second = client
            .put(&allocation.url)
            .header(AUTHORIZATION, format!("Bearer {}", allocation.handle))
            .header(CONTENT_TYPE, "text/plain")
            .header(CONTENT_RANGE, "bytes 2-4/5")
            .body("llo")
            .send()
            .await
            .expect("second ranged upload");
        assert_eq!(second.status(), StatusCode::CREATED);

        let status = test
            .staging
            .inspect(&allocation.handle)
            .await
            .expect("ready status");
        assert_eq!(status.state, "ready");
        assert_eq!(status.sha256.as_deref(), Some(expected));
        let mut source = test
            .staging
            .import_source(&allocation.handle, &space_id())
            .await
            .expect("retained import source");
        assert_eq!(source.length, 5);
        let mut reader = source.try_clone_reader().expect("rewound import reader");
        let mut bytes = Vec::new();
        reader
            .read_to_end(&mut bytes)
            .expect("read retained import");
        assert_eq!(bytes, b"hello");
        test.staging
            .bind_import_operation(&mut source, [1; 32])
            .await
            .expect("bind import operation");
        assert!(
            tokio::time::timeout(
                Duration::from_millis(20),
                test.staging.import_source(&allocation.handle, &space_id()),
            )
            .await
            .is_err(),
            "a second caller cannot acquire reconciliation authority"
        );
        source.fail_reader_clone = true;
        assert!(matches!(
            source.try_clone_reader(),
            Err(StagingError::Upstream)
        ));
        drop(source);
        assert_eq!(
            test.staging
                .inspect(&allocation.handle)
                .await
                .expect("pre-dispatch rollback status")
                .state,
            "ready",
            "dropping before upload dispatch restores staged authority"
        );
        let mut source = test
            .staging
            .import_source(&allocation.handle, &space_id())
            .await
            .expect("reacquire after pre-dispatch rollback");
        test.staging
            .bind_import_operation(&mut source, [1; 32])
            .await
            .expect("rebind import operation");
        test.staging
            .mark_import_dispatched(&mut source)
            .await
            .expect("mark upload dispatched");
        test.staging
            .restore_import_operation(&mut source)
            .await
            .expect("definitive rejection restores staged authority");
        drop(source);
        let mut source = test
            .staging
            .import_source(&allocation.handle, &space_id())
            .await
            .expect("reacquire after definitive rejection");
        test.staging
            .bind_import_operation(&mut source, [1; 32])
            .await
            .expect("bind final import operation");
        test.staging
            .mark_import_dispatched(&mut source)
            .await
            .expect("dispatch final import operation");
        test.staging
            .retain_import_candidate(
                &source,
                &EntityId::new("candidate-file").expect("candidate ID"),
            )
            .await
            .expect("retain candidate evidence");
        assert_eq!(
            source
                .record_owner
                .durable
                .lock()
                .await
                .document
                .candidate_id
                .as_deref(),
            Some("candidate-file")
        );
        test.staging
            .retain_candidate_cleanup(&source, "delete_dispatched")
            .await
            .expect("retain candidate cleanup dispatch");
        assert_eq!(
            source
                .record_owner
                .durable
                .lock()
                .await
                .document
                .candidate_cleanup
                .as_deref(),
            Some("delete_dispatched")
        );
        test.staging
            .restore_import_operation(&mut source)
            .await
            .expect("proven candidate absence restores source");
        drop(source);
        let mut source = test
            .staging
            .import_source(&allocation.handle, &space_id())
            .await
            .expect("reacquire after candidate cleanup");
        test.staging
            .bind_import_operation(&mut source, [1; 32])
            .await
            .expect("bind consumed operation");
        test.staging
            .mark_import_dispatched(&mut source)
            .await
            .expect("dispatch consumed operation");
        test.staging
            .retain_import_candidate(
                &source,
                &EntityId::new("candidate-file").expect("candidate ID"),
            )
            .await
            .expect("retain consumed candidate");
        test.staging
            .consume(&mut source)
            .await
            .expect("consume source");
        drop(source);
        let same = test
            .staging
            .reconciliation_import(&allocation.handle, &space_id(), [1; 32])
            .await
            .expect("same operation can inspect consumed metadata");
        assert_eq!(same.sha256, expected);
        assert!(
            test.staging
                .reconciliation_import(&allocation.handle, &space_id(), [2; 32])
                .await
                .is_err(),
            "a wrong key cannot inspect consumed authority"
        );
        test.staging
            .consume_reconciliation(&allocation.handle, &space_id(), [1; 32])
            .await
            .expect("same operation can replay consumed settlement");
        test.staging
            .consume_reconciliation(&allocation.handle, &space_id(), [1; 32])
            .await
            .expect("consumed settlement replay remains idempotent");
        assert!(matches!(
            test.staging
                .consume_reconciliation(&allocation.handle, &space_id(), [2; 32])
                .await,
            Err(StagingError::NotFound)
        ));
        test.staging
            .release(&allocation.handle)
            .await
            .expect("release consumed reconciliation source");
        assert_eq!(test.staging.available_quota().await, quota_before);
    }

    #[tokio::test]
    async fn graceful_shutdown_retains_dispatched_import_reconciliation_evidence() {
        let test = test_staging().await;
        let allocation = test
            .staging
            .allocate_import(space_id(), 5, Some("text/plain".to_owned()), None)
            .await
            .expect("allocate import");
        let response = reqwest::Client::new()
            .put(&allocation.url)
            .header(AUTHORIZATION, format!("Bearer {}", allocation.handle))
            .header(CONTENT_TYPE, "text/plain")
            .header(CONTENT_LENGTH, "5")
            .body("hello")
            .send()
            .await
            .expect("upload import");
        assert_eq!(response.status(), StatusCode::CREATED);
        let mut source = test
            .staging
            .import_source(&allocation.handle, &space_id())
            .await
            .expect("acquire ready source");
        test.staging
            .bind_import_operation(&mut source, [7; 32])
            .await
            .expect("bind operation");
        test.staging
            .mark_import_dispatched(&mut source)
            .await
            .expect("persist dispatch uncertainty");
        test.staging
            .retain_import_candidate(
                &source,
                &EntityId::new("shutdown-candidate").expect("candidate ID"),
            )
            .await
            .expect("persist candidate");
        test.staging
            .retain_candidate_cleanup(&source, "delete_dispatched")
            .await
            .expect("persist cleanup dispatch");
        test.staging
            .retain_candidate_cleanup(&source, "absence_ambiguous")
            .await
            .expect("persist cleanup ambiguity");
        assert!(matches!(
            test.staging.restore_import_operation(&mut source).await,
            Err(StagingError::Conflict)
        ));
        drop(source);

        test.shutdown.cancel();
        assert!(test.staging.drain(Duration::from_secs(2)).await);
        assert!(payload_path(&test.root, &allocation.record).is_file());
        let record_path = test
            .root
            .join("records")
            .join(format!("{}.json", allocation.record));
        let document: DurableStageRecord =
            serde_json::from_slice(&std::fs::read(record_path).expect("read retained record"))
                .expect("parse retained record");
        assert_eq!(document.state, DurableStageState::Reconciliation);
        assert_eq!(document.uncertainty.as_deref(), Some("mutation_dispatched"));
        assert!(document.operation_fingerprint.is_some());
        assert_eq!(document.candidate_id.as_deref(), Some("shutdown-candidate"));
        assert_eq!(
            document.candidate_cleanup.as_deref(),
            Some("absence_ambiguous")
        );
    }

    #[tokio::test]
    async fn staged_export_streams_exact_full_and_single_range_bytes() {
        let test = test_staging().await;
        let allocation = test
            .staging
            .allocate_export(space_id(), 5, Some("application/octet-stream".to_owned()))
            .await
            .expect("allocate export");
        let mut lease = test
            .staging
            .begin_write(
                &allocation.handle,
                Some(&allocation.record),
                StageDirection::Export,
                0,
            )
            .await
            .expect("lease export");
        let mut destination = lease.take_destination().expect("export destination");
        destination.write_all(b"hello").expect("write export");
        test.staging
            .finish_export(
                lease,
                destination,
                5,
                "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824".to_owned(),
            )
            .await
            .expect("publish export");

        let (mut first, _) = test
            .staging
            .export_reader(&allocation.handle, Some(&allocation.record))
            .await
            .expect("first export lease");
        assert!(
            tokio::time::timeout(
                Duration::from_millis(20),
                test.staging
                    .export_reader(&allocation.handle, Some(&allocation.record)),
            )
            .await
            .is_err(),
            "a second reader must wait for the first reader's cursor lease"
        );
        let mut retained = Vec::new();
        first
            .file
            .read_to_end(&mut retained)
            .expect("read first leased export");
        assert_eq!(retained, b"hello");
        drop(first);
        let (mut second, _) = test
            .staging
            .export_reader(&allocation.handle, Some(&allocation.record))
            .await
            .expect("second export lease after release");
        let mut repeated = Vec::new();
        second
            .file
            .read_to_end(&mut repeated)
            .expect("read second leased export");
        assert_eq!(repeated, b"hello");
        drop(second);

        let client = reqwest::Client::new();
        let full = client
            .get(&allocation.url)
            .header(AUTHORIZATION, format!("Bearer {}", allocation.handle))
            .send()
            .await
            .expect("full staged download");
        assert_eq!(full.status(), StatusCode::OK);
        assert_eq!(full.bytes().await.expect("full bytes"), b"hello".as_slice());

        let range = client
            .get(&allocation.url)
            .header(AUTHORIZATION, format!("Bearer {}", allocation.handle))
            .header(RANGE, "bytes=1-3")
            .send()
            .await
            .expect("ranged staged download");
        assert_eq!(range.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(range.bytes().await.expect("range bytes"), b"ell".as_slice());
        let unsatisfiable = client
            .get(&allocation.url)
            .header(AUTHORIZATION, format!("Bearer {}", allocation.handle))
            .header(RANGE, "bytes=5-5")
            .send()
            .await
            .expect("unsatisfiable range");
        assert_eq!(unsatisfiable.status(), StatusCode::RANGE_NOT_SATISFIABLE);
        let malformed = client
            .get(&allocation.url)
            .header(AUTHORIZATION, format!("Bearer {}", allocation.handle))
            .header(RANGE, "bytes=invalid")
            .send()
            .await
            .expect("malformed range");
        assert_eq!(malformed.status(), StatusCode::BAD_REQUEST);
        test.staging
            .release(&allocation.handle)
            .await
            .expect("release export");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn linked_staged_record_permanently_disables_pathname_cleanup_after_conflict() {
        let test = test_staging().await;
        let before_allocation = test.staging.available_quota().await;
        let allocation = test
            .staging
            .allocate_export(space_id(), 5, Some("application/octet-stream".to_owned()))
            .await
            .expect("allocate export");
        let before_release = test.staging.available_quota().await;
        let mut lease = test
            .staging
            .begin_write(
                &allocation.handle,
                Some(&allocation.record),
                StageDirection::Export,
                0,
            )
            .await
            .expect("lease export");
        let mut destination = lease.take_destination().expect("export destination");
        destination.write_all(b"hello").expect("write export");
        test.staging
            .finish_export(
                lease,
                destination,
                5,
                "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824".to_owned(),
            )
            .await
            .expect("publish export");

        let staged_path = payload_path(&test.root, &allocation.record);
        let retained_link = test.root.join("retained-link.bin");
        std::fs::hard_link(&staged_path, &retained_link).expect("link staged record");

        assert!(matches!(
            test.staging.release(&allocation.handle).await,
            Err(StagingError::Conflict)
        ));
        assert_eq!(test.staging.available_quota().await, before_release);
        assert!(staged_path.exists());
        assert_eq!(
            std::fs::read(&retained_link).expect("read retained link"),
            b"hello"
        );
        assert_eq!(test.staging.state.records.read().await.len(), 1);

        std::fs::remove_file(&retained_link).expect("remove retained link");
        for _ in 0..2 {
            assert!(matches!(
                test.staging.release(&allocation.handle).await,
                Err(StagingError::Conflict)
            ));
            assert_eq!(
                std::fs::read(&staged_path).expect("read staged bytes"),
                b"hello"
            );
            assert_eq!(test.staging.available_quota().await, before_release);
            assert_eq!(test.staging.state.records.read().await.len(), 1);
        }
        for _ in 0..2 {
            let expired = {
                let mut records = test.staging.state.records.write().await;
                test.staging
                    .take_expired_locked(&mut records, Instant::now() + Duration::from_secs(3_600))
            };
            assert_eq!(expired.len(), 1);
            test.staging.cleanup_expired(expired).await;
            assert_eq!(
                std::fs::read(&staged_path).expect("read staged bytes"),
                b"hello"
            );
            assert_eq!(test.staging.available_quota().await, before_release);
            assert_eq!(test.staging.state.records.read().await.len(), 1);
        }
        assert_ne!(test.staging.available_quota().await, before_allocation);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn linked_receiving_payload_retains_terminal_cleanup_evidence_across_retry_and_restart() {
        let test = test_staging().await;
        let allocation = test
            .staging
            .allocate_import(space_id(), 5, Some("text/plain".to_owned()), None)
            .await
            .expect("allocate receiving record");
        let payload = payload_path(&test.root, &allocation.record);
        let retained_link = test.root.join("receiving-retained-link.bin");
        std::fs::hard_link(&payload, &retained_link).expect("link receiving payload");

        assert!(matches!(
            test.staging.release(&allocation.handle).await,
            Err(StagingError::Conflict)
        ));
        assert!(payload.is_file());
        assert_eq!(test.staging.state.records.read().await.len(), 1);

        std::fs::remove_file(&retained_link).expect("remove external link");
        for _ in 0..2 {
            assert!(matches!(
                test.staging.release(&allocation.handle).await,
                Err(StagingError::Conflict)
            ));
            assert!(payload.is_file());
            assert_eq!(test.staging.state.records.read().await.len(), 1);
        }

        let inventory = test
            .staging
            .state
            .directory
            .inventory(
                test.staging.state.limits.staging_entries,
                test.staging.state.limits.artifact_bytes,
            )
            .expect("inventory retained cleanup evidence");
        assert!(matches!(
            reconcile_inventory(
                &test.staging.state.directory,
                inventory,
                &test.staging.state.limits,
            ),
            Err(StagingError::Reconciliation)
        ));
        assert!(payload.is_file());
        assert!(
            test.root
                .join("records")
                .join(format!("{}.json", allocation.record))
                .is_file()
        );
        assert!(
            test.root
                .join("tombstones")
                .join(format!("{}.json", allocation.record))
                .is_file()
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn replacement_cannot_redirect_pending_cleanup() {
        let test = test_staging().await;
        let before_allocation = test.staging.available_quota().await;
        let allocation = test
            .staging
            .allocate_export(space_id(), 5, Some("application/octet-stream".to_owned()))
            .await
            .expect("allocate export");
        let reserved_quota = test.staging.available_quota().await;
        let mut lease = test
            .staging
            .begin_write(
                &allocation.handle,
                Some(&allocation.record),
                StageDirection::Export,
                0,
            )
            .await
            .expect("lease export");
        let mut destination = lease.take_destination().expect("export destination");
        destination.write_all(b"hello").expect("write export");
        test.staging
            .finish_export(
                lease,
                destination,
                5,
                "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824".to_owned(),
            )
            .await
            .expect("publish export");

        let staged_path = payload_path(&test.root, &allocation.record);
        let outside_original = test.root.join("outside-original.bin");
        std::fs::hard_link(&staged_path, &outside_original).expect("link staged record");
        assert!(matches!(
            test.staging.release(&allocation.handle).await,
            Err(StagingError::Conflict)
        ));
        let moved_original = test.root.join("moved-original.bin");
        std::fs::rename(&staged_path, &moved_original).expect("move indexed name");
        std::fs::write(&staged_path, b"other").expect("replace indexed name");

        assert!(matches!(
            test.staging.release(&allocation.handle).await,
            Err(StagingError::Conflict)
        ));
        assert_eq!(
            std::fs::read(&outside_original).expect("read outside original"),
            b"hello"
        );
        assert_eq!(
            std::fs::read(&moved_original).expect("read moved original"),
            b"hello"
        );
        assert_eq!(
            std::fs::read(&staged_path).expect("read replacement"),
            b"other"
        );
        assert_eq!(test.staging.available_quota().await, reserved_quota);
        assert_eq!(test.staging.state.records.read().await.len(), 1);
        assert_ne!(test.staging.available_quota().await, before_allocation);
    }

    #[tokio::test]
    async fn stalled_expired_reader_defers_cleanup_and_never_blocks_requests() {
        let test = test_staging().await;
        let allocation = test
            .staging
            .allocate_import(space_id(), 5, Some("text/plain".to_owned()), None)
            .await
            .expect("allocate import");
        let uploaded = reqwest::Client::new()
            .put(&allocation.url)
            .header(AUTHORIZATION, format!("Bearer {}", allocation.handle))
            .header(CONTENT_TYPE, "text/plain")
            .header(CONTENT_RANGE, "bytes 0-4/5")
            .body("hello")
            .send()
            .await
            .expect("upload staged source");
        assert_eq!(uploaded.status(), StatusCode::CREATED);
        let source = test
            .staging
            .import_source(&allocation.handle, &space_id())
            .await
            .expect("hold staged source lease");
        // A record whose state lock is held by a live reader must be skipped
        // by the expiry pass: claiming it would persist cleanup evidence
        // underneath the active lease.
        let expired = {
            let mut records = test.staging.state.records.write().await;
            test.staging
                .take_expired_locked(&mut records, Instant::now() + Duration::from_secs(3_600))
        };
        assert!(expired.is_empty(), "lock-held record must not be claimed");
        let record_path = test
            .root
            .join("records")
            .join(format!("{}.json", allocation.record));
        let document: DurableStageRecord = serde_json::from_slice(
            &std::fs::read(&record_path).expect("read live durable record"),
        )
        .expect("parse live durable record");
        assert_eq!(document.cleanup_evidence, None);

        tokio::time::timeout(
            Duration::from_millis(200),
            test.staging.allocate_export(
                space_id(),
                1,
                Some("application/octet-stream".to_owned()),
            ),
        )
        .await
        .expect("unrelated allocation must not wait for expired reader")
        .expect("allocate unrelated export");

        drop(source);
        // A later pass claims the now-unlocked record and reaps it.
        let expired = {
            let mut records = test.staging.state.records.write().await;
            test.staging
                .take_expired_locked(&mut records, Instant::now() + Duration::from_secs(3_600))
        };
        assert!(
            expired
                .iter()
                .any(|(id, _)| record_hex(id) == allocation.record),
            "released record must be claimable on the later pass"
        );
        let cleanup = test
            .staging
            .spawn_expired_cleanup(expired)
            .expect("schedule expired cleanup");
        cleanup.await.expect("expired cleanup completes");
        let staged_path = payload_path(&test.root, &allocation.record);
        assert!(!staged_path.exists());
        assert!(!record_path.exists());
        assert!(test.staging.is_active());
    }

    #[tokio::test]
    async fn staging_http_rejects_ambient_browser_and_bearer_confusion() {
        let test = test_staging().await;
        let allocation = test
            .staging
            .allocate_import(space_id(), 5, Some("text/plain".to_owned()), None)
            .await
            .expect("allocate import");
        let client = reqwest::Client::new();

        let unauthenticated = client
            .head(&allocation.url)
            .send()
            .await
            .expect("unauthenticated request");
        assert_eq!(unauthenticated.status(), StatusCode::UNAUTHORIZED);

        let browser = client
            .head(&allocation.url)
            .header(AUTHORIZATION, format!("Bearer {}", allocation.handle))
            .header(ORIGIN, "https://attacker.invalid")
            .send()
            .await
            .expect("browser-origin request");
        assert_eq!(browser.status(), StatusCode::FORBIDDEN);

        let wrong_host = client
            .head(&allocation.url)
            .header(AUTHORIZATION, format!("Bearer {}", allocation.handle))
            .header(HOST, "attacker.invalid")
            .send()
            .await
            .expect("wrong-host request");
        assert_eq!(wrong_host.status(), StatusCode::FORBIDDEN);

        let query = client
            .head(format!("{}?handle={}", allocation.url, allocation.handle))
            .header(AUTHORIZATION, format!("Bearer {}", allocation.handle))
            .send()
            .await
            .expect("query-bearing request");
        assert_eq!(query.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn handle_abuse_matrix_is_uniform_and_state_preserving() {
        let test = test_staging().await;
        let allocation = test
            .staging
            .allocate_import(space_id(), 5, Some("text/plain".to_owned()), None)
            .await
            .expect("allocate HAND fixture");
        let client = reqwest::Client::new();

        let (_, guessed, _) = make_handle(&[0x55; 32]).expect("mint guessed handle");
        assert!(matches!(
            test.staging.release(&guessed).await,
            Err(StagingError::NotFound)
        ));
        assert!(matches!(
            test.staging.release("not-a-handle").await,
            Err(StagingError::NotFound)
        ));

        let wrong_space = SpaceId::new(
            "bafyreih62bq2tfyvb4chv53hxsfm74qf27medzfkfap6bxsno7yhk3qxwu.2tq5w93cr6oe7",
        )
        .expect("second valid space id");
        assert!(matches!(
            test.staging
                .import_source(&allocation.handle, &wrong_space)
                .await,
            Err(StagingError::NotFound)
        ));

        let direction = client
            .get(&allocation.url)
            .header(AUTHORIZATION, format!("Bearer {}", allocation.handle))
            .send()
            .await
            .expect("HAND-06 direction request");
        assert_eq!(direction.status(), StatusCode::NOT_FOUND);
        assert_eq!(
            direction.bytes().await.expect("HAND-06 response body"),
            Bytes::from_static(b"not found\n")
        );

        let wrong_record = "00000000000000000000000000000000";
        assert_ne!(allocation.record, wrong_record);
        let wrong_route = allocation.url.replace(&allocation.record, wrong_record);
        let route = client
            .head(wrong_route)
            .header(AUTHORIZATION, format!("Bearer {}", allocation.handle))
            .send()
            .await
            .expect("HAND-08 route request");
        assert_eq!(route.status(), StatusCode::NOT_FOUND);
        assert_eq!(
            test.staging
                .inspect(&allocation.handle)
                .await
                .expect("record remains live")
                .offset,
            0
        );
        test.staging
            .release(&allocation.handle)
            .await
            .expect("release HAND fixture");
    }

    #[tokio::test]
    async fn staging_request_shedding_and_closed_grammar_preserve_records() {
        let test = test_staging().await;
        let allocation = test
            .staging
            .allocate_import(space_id(), 5, Some("text/plain".to_owned()), None)
            .await
            .expect("allocate request-grammar fixture");
        let client = reqwest::Client::new();
        let bearer = format!("Bearer {}", allocation.handle);

        for request in [
            client
                .head(&allocation.url)
                .header(AUTHORIZATION, format!("Basic {}", allocation.handle)),
            client
                .head(&allocation.url)
                .header(AUTHORIZATION, format!("{bearer} extra")),
        ] {
            let response = request.send().await.expect("send HAND-09 request");
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        }

        let method = client
            .post(&allocation.url)
            .header(AUTHORIZATION, &bearer)
            .send()
            .await
            .expect("send HAND-13 request");
        assert_eq!(method.status(), StatusCode::METHOD_NOT_ALLOWED);

        let parsed = url::Url::parse(&allocation.url).expect("parse HAND-15 URL");
        let host = parsed.host_str().expect("HAND-15 host");
        let port = parsed.port_or_known_default().expect("HAND-15 port");
        let request = format!(
            "PUT {} HTTP/1.1\r\nHost: {host}:{port}\r\nAuthorization: {bearer}\r\nTransfer-Encoding: gzip\r\nConnection: close\r\n\r\n",
            parsed.path()
        )
        .into_bytes();
        assert_eq!(
            raw_staging_status(&allocation.url, request).await,
            StatusCode::BAD_REQUEST
        );

        {
            let mut window = test.staging.state.rate_window.lock().await;
            window.extend(std::iter::repeat_n(
                Instant::now(),
                test.staging.state.limits.staging_requests_per_minute as usize,
            ));
        }
        let rate_limited = client
            .head(&allocation.url)
            .header(AUTHORIZATION, &bearer)
            .send()
            .await
            .expect("send HAND-14 request");
        assert_eq!(rate_limited.status(), StatusCode::TOO_MANY_REQUESTS);
        test.staging.state.rate_window.lock().await.clear();
        let resumed = client
            .head(&allocation.url)
            .header(AUTHORIZATION, &bearer)
            .send()
            .await
            .expect("resume after HAND-14 window");
        assert_eq!(resumed.status(), StatusCode::OK);

        let permits = Arc::clone(&test.staging.state.request_permits)
            .acquire_many_owned(
                u32::try_from(test.staging.state.limits.staging_requests)
                    .expect("staging request permits fit u32"),
            )
            .await
            .expect("hold all staging request permits");
        let overloaded = client
            .head(&allocation.url)
            .header(AUTHORIZATION, &bearer)
            .send()
            .await
            .expect("send FLOOD-06 request");
        assert_eq!(overloaded.status(), StatusCode::SERVICE_UNAVAILABLE);
        drop(permits);

        assert_eq!(
            test.staging
                .inspect(&allocation.handle)
                .await
                .expect("request grammar preserved record")
                .offset,
            0
        );
        test.staging
            .release(&allocation.handle)
            .await
            .expect("release request-grammar fixture");
    }

    #[test]
    fn partial_write_and_critical_header_grammar_is_closed() {
        assert_eq!(parse_content_range("bytes 5-9/10", 10), Some((5, 5)));
        assert_eq!(parse_content_range("bytes 5-9/11", 10), None);
        assert_eq!(parse_content_range("bytes 9-5/10", 10), None);
        assert_eq!(
            parse_download_range("bytes=10-10", 10),
            DownloadRange::Unsatisfiable
        );
        assert_eq!(
            parse_download_range("bytes=invalid", 10),
            DownloadRange::Malformed
        );

        let mut headers = hyper::HeaderMap::new();
        headers.append(AUTHORIZATION, HeaderValue::from_static("Bearer first"));
        headers.append(AUTHORIZATION, HeaderValue::from_static("Bearer second"));
        assert!(matches!(
            single_header(&headers, AUTHORIZATION, 160),
            Err(StagingError::Conflict)
        ));
        headers.clear();
        headers.append(CONTENT_LENGTH, HeaderValue::from_static("05"));
        assert!(matches!(
            parse_single_u64(&headers, CONTENT_LENGTH, 128),
            Err(StagingError::Conflict)
        ));
    }

    #[tokio::test]
    async fn short_put_body_returns_bad_request_without_advancing_offset() {
        let test = test_staging().await;
        let allocation = test
            .staging
            .allocate_import(space_id(), 5, Some("text/plain".to_owned()), None)
            .await
            .expect("allocate short-body fixture");
        let parsed = url::Url::parse(&allocation.url).expect("parse staging URL");
        let host = parsed.host_str().expect("staging host");
        let authority = format!("{host}:{}", parsed.port().expect("staging port"));
        let request = format!(
            "PUT {} HTTP/1.1\r\nHost: {authority}\r\nAuthorization: Bearer {}\r\nContent-Type: text/plain\r\nContent-Length: 5\r\n\r\nabc",
            parsed.path(),
            allocation.handle,
        )
        .into_bytes();
        assert_eq!(
            raw_staging_status(&allocation.url, request).await,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            test.staging
                .inspect(&allocation.handle)
                .await
                .expect("inspect short-body fixture")
                .offset,
            0
        );
        test.staging
            .release(&allocation.handle)
            .await
            .expect("release short-body fixture");
    }

    #[tokio::test]
    async fn restart_reconciliation_invalidates_and_reaps_a_complete_old_generation() {
        let test = test_staging().await;
        let allocation = test
            .staging
            .allocate_import(space_id(), 5, Some("text/plain".to_owned()), None)
            .await
            .expect("allocate restart fixture");
        let response = reqwest::Client::new()
            .put(&allocation.url)
            .header(AUTHORIZATION, format!("Bearer {}", allocation.handle))
            .header(CONTENT_TYPE, "text/plain")
            .header(CONTENT_LENGTH, "5")
            .body("hello")
            .send()
            .await
            .expect("upload restart fixture");
        assert_eq!(response.status(), StatusCode::CREATED);
        let inventory = test
            .staging
            .state
            .directory
            .inventory(
                test.staging.state.limits.staging_entries,
                test.staging.state.limits.artifact_bytes,
            )
            .expect("inventory durable restart state");
        assert_eq!(
            reconcile_inventory(
                &test.staging.state.directory,
                inventory,
                &test.staging.state.limits,
            )
            .expect("reconcile old generation")
            .cleaned,
            1
        );
        assert_empty_closed_layout(&test.root);
    }

    #[tokio::test]
    async fn corrupt_durable_record_fails_closed_without_deleting_payload() {
        let test = test_staging().await;
        let allocation = test
            .staging
            .allocate_import(space_id(), 5, Some("text/plain".to_owned()), None)
            .await
            .expect("allocate corrupt-state fixture");
        let record_path = test
            .root
            .join("records")
            .join(format!("{}.json", allocation.record));
        std::fs::write(&record_path, b"{\"format_version\":1,\"unknown\":true}")
            .expect("corrupt durable record");
        let payload = payload_path(&test.root, &allocation.record);
        let inventory = test
            .staging
            .state
            .directory
            .inventory(
                test.staging.state.limits.staging_entries,
                test.staging.state.limits.artifact_bytes,
            )
            .expect("inventory corrupt durable state");
        assert!(matches!(
            reconcile_inventory(
                &test.staging.state.directory,
                inventory,
                &test.staging.state.limits,
            ),
            Err(StagingError::Reconciliation)
        ));
        assert!(payload.is_file());
        assert_eq!(
            std::fs::read(&record_path).expect("corrupt record retained"),
            b"{\"format_version\":1,\"unknown\":true}"
        );
    }

    #[tokio::test]
    async fn semantically_impossible_durable_record_fails_before_reconciliation_mutation() {
        let test = test_staging().await;
        let allocation = test
            .staging
            .allocate_import(space_id(), 5, Some("text/plain".to_owned()), None)
            .await
            .expect("allocate semantic-corruption fixture");
        let record_path = test
            .root
            .join("records")
            .join(format!("{}.json", allocation.record));
        let record_bytes = std::fs::read(&record_path).expect("read durable record");
        let mut far_future: DurableStageRecord =
            serde_json::from_slice(&record_bytes).expect("parse durable record");
        far_future.created_at += chrono::Duration::days(365);
        far_future.expires_at += chrono::Duration::days(365);
        // A stepped clock keeps the record well-formed (reapable), but makes
        // it ineligible for restart revival.
        assert!(durable_shape_valid(&far_future));
        assert!(!durable_policy_current(
            &far_future,
            &test.staging.state.limits
        ));
        let mut document: serde_json::Value =
            serde_json::from_slice(&record_bytes).expect("parse durable record value");
        document["state"] = serde_json::Value::String("ready".to_owned());
        let corrupt = serde_json::to_vec(&document).expect("serialize semantic corruption");
        std::fs::write(&record_path, &corrupt).expect("replace durable record");
        let payload = payload_path(&test.root, &allocation.record);
        let inventory = test
            .staging
            .state
            .directory
            .inventory(
                test.staging.state.limits.staging_entries,
                test.staging.state.limits.artifact_bytes,
            )
            .expect("inventory semantic corruption");

        assert!(matches!(
            reconcile_inventory(
                &test.staging.state.directory,
                inventory,
                &test.staging.state.limits,
            ),
            Err(StagingError::Reconciliation)
        ));
        assert!(payload.is_file());
        assert_eq!(
            std::fs::read(&record_path).expect("semantic corruption retained"),
            corrupt
        );
        assert!(
            std::fs::read_dir(test.root.join("tombstones"))
                .expect("inspect tombstones")
                .next()
                .is_none()
        );
    }

    #[tokio::test]
    async fn expected_hash_mismatch_never_publishes_or_orphans_bytes() {
        let test = test_staging().await;
        let allocation = test
            .staging
            .allocate_import(
                space_id(),
                5,
                Some("text/plain".to_owned()),
                Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_owned()),
            )
            .await
            .expect("allocate import");
        let response = reqwest::Client::new()
            .put(&allocation.url)
            .header(AUTHORIZATION, format!("Bearer {}", allocation.handle))
            .header(CONTENT_TYPE, "text/plain")
            .header(CONTENT_LENGTH, "5")
            .body("hello")
            .send()
            .await
            .expect("upload mismatched bytes");
        assert_eq!(response.status(), StatusCode::CONFLICT);
        test.staging
            .release(&allocation.handle)
            .await
            .expect("release rejected upload");
        assert_empty_closed_layout(&test.root);
    }

    #[tokio::test]
    async fn empty_document_export_is_an_immutable_zero_byte_stage() {
        let test = test_staging().await;
        let allocation = test
            .staging
            .allocate_export(space_id(), 0, Some("text/markdown".to_owned()))
            .await
            .expect("allocate empty export");
        let mut lease = test
            .staging
            .begin_write(
                &allocation.handle,
                Some(&allocation.record),
                StageDirection::Export,
                0,
            )
            .await
            .expect("empty export lease");
        let destination = lease.take_destination().expect("empty destination");
        test.staging
            .finish_export(
                lease,
                destination,
                0,
                "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855".to_owned(),
            )
            .await
            .expect("publish empty export");
        let response = reqwest::Client::new()
            .get(&allocation.url)
            .header(AUTHORIZATION, format!("Bearer {}", allocation.handle))
            .send()
            .await
            .expect("empty download");
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.content_length(), Some(0));
        assert!(response.bytes().await.expect("empty bytes").is_empty());
    }

    #[tokio::test]
    async fn export_completion_rehashes_from_byte_zero_and_rejects_caller_mismatch() {
        let test = test_staging().await;
        let allocation = test
            .staging
            .allocate_export(space_id(), 5, Some("text/plain".to_owned()))
            .await
            .expect("allocate export");
        let mut lease = test
            .staging
            .begin_write(
                &allocation.handle,
                Some(&allocation.record),
                StageDirection::Export,
                0,
            )
            .await
            .expect("lease export");
        let mut destination = lease.take_destination().expect("export payload");
        destination.write_all(b"hello").expect("write export");

        assert!(matches!(
            test.staging
                .finish_export(lease, destination, 5, "00".repeat(32))
                .await,
            Err(StagingError::Indeterminate)
        ));
        assert!(test.shutdown.is_cancelled());
        assert!(payload_path(&test.root, &allocation.record).is_file());
    }

    #[tokio::test]
    async fn expiry_retains_indeterminate_publication_and_its_quota_record() {
        let test = test_staging().await;
        let quota_before = test.staging.available_quota().await;
        let allocation = test
            .staging
            .allocate_export(space_id(), 5, Some("application/octet-stream".to_owned()))
            .await
            .expect("allocate export");
        let mut lease = test
            .staging
            .begin_write(
                &allocation.handle,
                Some(&allocation.record),
                StageDirection::Export,
                0,
            )
            .await
            .expect("lease export");
        let record = Arc::clone(&lease.record);
        let mut destination = lease.take_destination().expect("export destination");
        destination.write_all(b"hello").expect("write export");
        let source = destination.into_anchored().expect("publish export");
        drop(source);
        let completion = PublicationCompletion::new(Arc::clone(&record.cleanup_blocked));
        drop(PublicationCompletionGuard(Arc::clone(&completion)));
        drop(PublicationOwnerGuard(Arc::clone(&completion)));
        *record.state.lock().await = RecordState::PublicationIndeterminate { completion };
        let published = test.root.join("payloads").join(&record.record_name);
        let reserved_quota = test.staging.available_quota().await;
        assert!(published.is_file());
        assert!(matches!(
            test.staging.release(&allocation.handle).await,
            Err(StagingError::Conflict)
        ));
        assert_eq!(
            std::fs::read(&published).expect("read publication"),
            b"hello"
        );
        assert_eq!(test.staging.available_quota().await, reserved_quota);
        assert_eq!(test.staging.state.records.read().await.len(), 1);

        for _ in 0..2 {
            let expired = {
                let mut records = test.staging.state.records.write().await;
                test.staging
                    .take_expired_locked(&mut records, Instant::now() + Duration::from_secs(3_600))
            };
            assert_eq!(expired.len(), 1);
            test.staging.cleanup_expired(expired).await;
            assert_eq!(
                std::fs::read(&published).expect("read publication"),
                b"hello"
            );
            assert_eq!(test.staging.available_quota().await, reserved_quota);
            assert_eq!(test.staging.state.records.read().await.len(), 1);
        }
        assert_ne!(test.staging.available_quota().await, quota_before);
    }

    #[tokio::test]
    async fn release_refuses_an_active_write_lease_until_it_is_restored() {
        let test = test_staging().await;
        let allocation = test
            .staging
            .allocate_import(
                space_id(),
                5,
                Some("application/octet-stream".to_owned()),
                None,
            )
            .await
            .expect("allocate import");
        let mut lease = test
            .staging
            .begin_write(
                &allocation.handle,
                Some(&allocation.record),
                StageDirection::Import,
                0,
            )
            .await
            .expect("lease import");
        let destination = lease.take_destination().expect("import destination");

        assert!(matches!(
            test.staging.release(&allocation.handle).await,
            Err(StagingError::Conflict)
        ));
        assert_eq!(test.staging.state.records.read().await.len(), 1);

        test.staging
            .restore_write(lease, destination, 0)
            .await
            .expect("restore import lease");
        test.staging
            .release(&allocation.handle)
            .await
            .expect("release restored import");
        assert!(test.staging.state.records.read().await.is_empty());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancelled_release_keeps_published_cleanup_coordinator_alive() {
        let _serial = CLEANUP_TEST_SERIAL
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        let test = test_staging().await;
        let allocation = test
            .staging
            .allocate_export(space_id(), 5, Some("application/octet-stream".to_owned()))
            .await
            .expect("allocate export");
        let mut lease = test
            .staging
            .begin_write(
                &allocation.handle,
                Some(&allocation.record),
                StageDirection::Export,
                0,
            )
            .await
            .expect("lease export");
        let mut destination = lease.take_destination().expect("export destination");
        destination.write_all(b"hello").expect("write export");
        test.staging
            .finish_export(
                lease,
                destination,
                5,
                "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824".to_owned(),
            )
            .await
            .expect("publish export");
        let (entered, release) = install_cleanup_pause(&format!("{}.bin", allocation.record));
        let staging = test.staging.clone();
        let handle = allocation.handle.clone();
        let release_task = tokio::spawn(async move { staging.release(&handle).await });
        tokio::task::spawn_blocking(move || entered.wait())
            .await
            .expect("cleanup coordinator entered");
        release_task.abort();
        assert!(
            release_task
                .await
                .expect_err("release task cancelled")
                .is_cancelled()
        );
        tokio::task::spawn_blocking(move || release.wait())
            .await
            .expect("release cleanup coordinator");
        clear_cleanup_pause();

        let staged_path = payload_path(&test.root, &allocation.record);
        tokio::time::timeout(Duration::from_secs(1), async {
            while !test.staging.state.records.read().await.is_empty() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("coordinator settled map ownership");
        assert!(!staged_path.exists());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancelled_release_keeps_temporary_cleanup_coordinator_alive() {
        let _serial = CLEANUP_TEST_SERIAL
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        let test = test_staging().await;
        let allocation = test
            .staging
            .allocate_import(
                space_id(),
                5,
                Some("application/octet-stream".to_owned()),
                None,
            )
            .await
            .expect("allocate import");
        let (entered, release) = install_cleanup_pause(&format!("{}.bin", allocation.record));
        let staging = test.staging.clone();
        let handle = allocation.handle.clone();
        let release_task = tokio::spawn(async move { staging.release(&handle).await });
        tokio::task::spawn_blocking(move || entered.wait())
            .await
            .expect("cleanup coordinator entered");
        release_task.abort();
        assert!(
            release_task
                .await
                .expect_err("release task cancelled")
                .is_cancelled()
        );
        tokio::task::spawn_blocking(move || release.wait())
            .await
            .expect("release cleanup coordinator");
        clear_cleanup_pause();

        tokio::time::timeout(Duration::from_secs(1), async {
            while !test.staging.state.records.read().await.is_empty() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("coordinator settled map ownership");
        assert_empty_closed_layout(&test.root);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn aborted_cleanup_coordinator_releases_its_claim_for_retry() {
        let _serial = CLEANUP_TEST_SERIAL
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        let test = test_staging().await;
        let allocation = test
            .staging
            .allocate_import(
                space_id(),
                5,
                Some("application/octet-stream".to_owned()),
                None,
            )
            .await
            .expect("allocate import");
        let (id, record) = test
            .staging
            .authenticate(&allocation.handle, None)
            .await
            .expect("authenticate record");
        record.cleanup_blocked.store(true, Ordering::Release);
        let mut state = record.state.lock().await;
        assert!(transition_to_cleanup_pending(&mut state));
        drop(state);
        let (entered, release) = install_cleanup_pause(&record.record_name);
        let task = test
            .staging
            .spawn_cleanup_coordinator(id, Arc::clone(&record));
        tokio::task::spawn_blocking(move || entered.wait())
            .await
            .expect("coordinator entered");
        task.abort();
        tokio::task::spawn_blocking(move || release.wait())
            .await
            .expect("release coordinator pause");
        clear_cleanup_pause();
        assert!(task.await.expect_err("coordinator aborted").is_cancelled());
        tokio::time::timeout(Duration::from_secs(1), async {
            while record.cleanup_blocked.load(Ordering::Acquire) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("cleanup claim released");
        test.staging
            .release(&allocation.handle)
            .await
            .expect("retry after cancelled coordinator");
        assert!(test.staging.state.records.read().await.is_empty());
    }

    #[tokio::test]
    async fn abort_write_surrenders_lease_before_releasing_quota() {
        let test = test_staging().await;
        let allocation = test
            .staging
            .allocate_export(space_id(), 5, Some("application/octet-stream".to_owned()))
            .await
            .expect("allocate export");
        let lease = test
            .staging
            .begin_write(
                &allocation.handle,
                Some(&allocation.record),
                StageDirection::Export,
                0,
            )
            .await
            .expect("lease export");

        test.staging
            .abort_write(lease, &allocation.handle)
            .await
            .expect("abort active export");

        assert!(test.staging.state.records.read().await.is_empty());
        assert_empty_closed_layout(&test.root);
    }

    #[tokio::test]
    async fn expiry_requeues_an_active_write_until_cleanup_can_claim_it() {
        let test = test_staging().await;
        let allocation = test
            .staging
            .allocate_export(space_id(), 5, Some("application/octet-stream".to_owned()))
            .await
            .expect("allocate export");
        let mut lease = test
            .staging
            .begin_write(
                &allocation.handle,
                Some(&allocation.record),
                StageDirection::Export,
                0,
            )
            .await
            .expect("lease export");
        let destination = lease.take_destination().expect("export destination");
        let future = Instant::now() + Duration::from_secs(3_600);

        let busy = {
            let mut records = test.staging.state.records.write().await;
            test.staging.take_expired_locked(&mut records, future)
        };
        assert!(busy.is_empty());
        assert_eq!(test.staging.state.records.read().await.len(), 1);

        test.staging
            .restore_write(lease, destination, 0)
            .await
            .expect("restore export lease");
        let claimed = {
            let mut records = test.staging.state.records.write().await;
            test.staging.take_expired_locked(&mut records, future)
        };
        assert_eq!(claimed.len(), 1);
        test.staging.cleanup_expired(claimed).await;
        assert!(test.staging.state.records.read().await.is_empty());
    }

    #[tokio::test]
    async fn cancelled_publication_retains_late_file_cleanup_ownership() {
        let _serial = PUBLICATION_TEST_SERIAL
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        let test = test_staging().await;
        let allocation = test
            .staging
            .allocate_export(space_id(), 5, Some("application/octet-stream".to_owned()))
            .await
            .expect("allocate export");
        let mut lease = test
            .staging
            .begin_write(
                &allocation.handle,
                Some(&allocation.record),
                StageDirection::Export,
                0,
            )
            .await
            .expect("lease export");
        let record = Arc::clone(&lease.record);
        let mut destination = lease.take_destination().expect("export destination");
        destination.write_all(b"hello").expect("write export");
        let (entered, release) = install_publication_pause(&record.record_name);
        let entered_wait = tokio::task::spawn_blocking({
            let entered = Arc::clone(&entered);
            move || entered.wait()
        });
        let staging = test.staging.clone();
        let publication = tokio::spawn(async move {
            staging
                .finish_export(
                    lease,
                    destination,
                    5,
                    "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824".to_owned(),
                )
                .await
        });
        tokio::time::timeout(Duration::from_secs(1), entered_wait)
            .await
            .expect("publication reached cancellation boundary")
            .expect("publication boundary waiter");
        publication.abort();
        let cancellation = publication.await.expect_err("publication task cancelled");
        assert!(cancellation.is_cancelled());
        tokio::task::spawn_blocking(move || release.wait())
            .await
            .expect("release detached publication");
        clear_publication_pause();
        let completion = match &*record.state.lock().await {
            RecordState::PublicationIndeterminate { completion } => Arc::clone(completion),
            _ => panic!("cancelled publication lost cleanup ownership"),
        };
        completion.wait().await;

        let published = test.root.join("payloads").join(&record.record_name);
        assert!(published.is_file(), "detached publication completed");
        let expired = {
            let mut records = test.staging.state.records.write().await;
            test.staging
                .take_expired_locked(&mut records, Instant::now() + Duration::from_secs(3_600))
        };
        test.staging.cleanup_expired(expired).await;

        assert_eq!(
            std::fs::read(&published).expect("read publication"),
            b"hello"
        );
        assert_eq!(test.staging.state.records.read().await.len(), 1);
    }

    #[tokio::test]
    async fn expiry_waits_for_successful_publication_state_transition() {
        let _serial = PUBLICATION_TEST_SERIAL
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        let test = test_staging().await;
        let allocation = test
            .staging
            .allocate_export(space_id(), 5, Some("application/octet-stream".to_owned()))
            .await
            .expect("allocate export");
        let mut lease = test
            .staging
            .begin_write(
                &allocation.handle,
                Some(&allocation.record),
                StageDirection::Export,
                0,
            )
            .await
            .expect("lease export");
        let record = Arc::clone(&lease.record);
        let mut destination = lease.take_destination().expect("export destination");
        destination.write_all(b"hello").expect("write export");
        let (entered, release) = install_publication_pause(&record.record_name);
        let entered_wait = tokio::task::spawn_blocking({
            let entered = Arc::clone(&entered);
            move || entered.wait()
        });
        let staging = test.staging.clone();
        let publication = tokio::spawn(async move {
            staging
                .finish_export(
                    lease,
                    destination,
                    5,
                    "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824".to_owned(),
                )
                .await
        });
        tokio::time::timeout(Duration::from_secs(1), entered_wait)
            .await
            .expect("publication reached expiry boundary")
            .expect("publication boundary waiter");
        let future = Instant::now() + Duration::from_secs(3_600);
        let busy = {
            let mut records = test.staging.state.records.write().await;
            test.staging.take_expired_locked(&mut records, future)
        };
        assert!(busy.is_empty());
        assert_eq!(test.staging.state.records.read().await.len(), 1);

        tokio::task::spawn_blocking(move || release.wait())
            .await
            .expect("release successful publication");
        publication
            .await
            .expect("publication task")
            .expect("publication succeeds");
        clear_publication_pause();
        let published = test.root.join("payloads").join(&record.record_name);
        assert!(published.is_file());

        let expired = {
            let mut records = test.staging.state.records.write().await;
            test.staging.take_expired_locked(&mut records, future)
        };
        assert_eq!(expired.len(), 1);
        test.staging.cleanup_expired(expired).await;
        assert!(!published.exists());
        assert!(test.staging.state.records.read().await.is_empty());
    }

    #[test]
    fn concurrent_publication_completion_releases_cleanup_ownership() {
        let cleanup_blocked = Arc::new(AtomicBool::new(true));
        let completion = PublicationCompletion::new(Arc::clone(&cleanup_blocked));
        let worker = PublicationCompletionGuard(Arc::clone(&completion));
        let owner = PublicationOwnerGuard(Arc::clone(&completion));
        let boundary = Arc::new(std::sync::Barrier::new(3));
        let worker_boundary = Arc::clone(&boundary);
        let worker = std::thread::spawn(move || {
            worker_boundary.wait();
            drop(worker);
        });
        let owner_boundary = Arc::clone(&boundary);
        let owner = std::thread::spawn(move || {
            owner_boundary.wait();
            drop(owner);
        });

        boundary.wait();
        worker.join().expect("worker completion thread");
        owner.join().expect("owner completion thread");

        assert!(completion.settled());
        assert!(!cleanup_blocked.load(Ordering::Acquire));
    }

    #[test]
    fn handles_are_canonical_and_tampering_is_uniformly_rejected() {
        let key = [7_u8; 32];
        let (_, handle, _) = make_handle(&key).expect("handle");
        assert!((64..=128).contains(&handle.len()));
        assert!(parse_handle(&handle).is_ok());
        let mut tampered = handle.into_bytes();
        if let Some(first) = tampered.first_mut() {
            *first = if *first == b'A' { b'B' } else { b'A' };
        }
        let tampered = String::from_utf8(tampered).expect("ASCII handle");
        assert!(matches!(
            parse_handle(&tampered),
            Err(StagingError::NotFound)
        ));
    }
}
