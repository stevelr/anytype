// any-mcp - bounded, workflow-oriented MCP server for Anytype
//
// SPDX-FileCopyrightText: 2026 Steve Schoettler
// SPDX-License-Identifier: Apache-2.0

//! Shared finite process-local coordination for create workflows.

use std::{
    borrow::Cow,
    collections::HashMap,
    fmt,
    sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
    },
    time::Instant,
};

use rmcp::{
    model::CallToolResult,
    schemars::{JsonSchema, Schema, SchemaGenerator, json_schema},
};
use serde::{Deserialize, Deserializer, Serialize, de};
use tokio::{
    sync::{Mutex, Notify},
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;

use crate::{
    error::ToolError,
    handler_support::{MutationProgress, MutationStage},
    result::tool_error,
};

/// Maximum Unicode scalar values accepted in an idempotency key.
pub const MAX_IDEMPOTENCY_KEY_CHARS: usize = 256;
/// Maximum retained idempotency entries in one create handler.
pub const DEFAULT_IDEMPOTENCY_CAPACITY: usize = 1_024;

/// A bounded, nonempty caller-generated process-local retry key.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct IdempotencyKey(String);

impl IdempotencyKey {
    /// Validates an exact idempotency key without trimming or normalization.
    pub fn new(value: impl Into<String>) -> Result<Self, CreateInputError> {
        let value = value.into();
        if value.is_empty() {
            return Err(CreateInputError::Empty);
        }
        if value.chars().count() > MAX_IDEMPOTENCY_KEY_CHARS {
            return Err(CreateInputError::TooLong);
        }
        Ok(Self(value))
    }
}

impl<'de> Deserialize<'de> for IdempotencyKey {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::new(String::deserialize(deserializer)?).map_err(de::Error::custom)
    }
}

impl JsonSchema for IdempotencyKey {
    fn schema_name() -> Cow<'static, str> {
        "IdempotencyKey".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "minLength": 1,
            "maxLength": MAX_IDEMPOTENCY_KEY_CHARS,
        })
    }
}

/// Failure to construct one exact idempotency key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CreateInputError {
    /// The key was empty.
    Empty,
    /// The key exceeded its finite Unicode-scalar bound.
    TooLong,
}

impl fmt::Display for CreateInputError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Empty => "value must not be empty",
            Self::TooLong => "value exceeds its maximum length",
        })
    }
}

impl std::error::Error for CreateInputError {}

pub(crate) struct IdempotencyStore {
    entries: Mutex<HashMap<IdempotencyKey, StoredAttempt>>,
    capacity: usize,
}

enum StoredAttempt {
    Running {
        fingerprint: [u8; 32],
        attempt: Arc<Attempt>,
    },
    Complete(CompleteAttempt),
    ResumeRunning {
        prior: CompleteAttempt,
        token: ResumeToken,
        attempt: Arc<Attempt>,
    },
    Indeterminate {
        fingerprint: [u8; 32],
    },
    PendingCandidate {
        fingerprint: [u8; 32],
        candidate: PendingCandidate,
    },
}

pub(crate) struct Attempt {
    result: Mutex<Option<CallToolResult>>,
    notify: Notify,
    progress: MutationProgress,
    deadline: Option<Instant>,
    pending_candidate: Mutex<Option<PendingCandidate>>,
    replay_witness: Mutex<Option<ReplayWitness>>,
    rich_page_type_id: Mutex<Option<String>>,
    leader_cancellation: CancellationToken,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ReplayWitness {
    RootAppendIndex(u64),
    RecoveredCandidate,
    ResumedRelative,
}

/// Private replay metadata retained with one rich-page receipt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RichReceiptMetadata {
    /// Identifies the original page type used for liveness proof.
    pub(crate) page_type_id: String,
    /// Selects the replay proof that the receipt established.
    pub(crate) replay_witness: ReplayWitness,
}

#[derive(Clone)]
struct CompleteAttempt {
    fingerprint: [u8; 32],
    result: CallToolResult,
    replay_metadata: Option<RichReceiptMetadata>,
    resume_consumed: bool,
}

/// Unforgeable identity for one claimed rich recovery attempt.
#[derive(Clone)]
pub(crate) struct ResumeToken(Arc<()>);

impl ResumeToken {
    fn new() -> Self {
        Self(Arc::new(()))
    }

    fn matches(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

/// A side-effect-free retained receipt admission result.
pub(crate) enum ResumeEligibilitySnapshot {
    Eligible {
        fingerprint: [u8; 32],
        metadata: RichReceiptMetadata,
    },
    Refused,
}

/// A claimed recovery attempt with the immutable receipt it supersedes.
pub(crate) struct ResumeClaim {
    pub(crate) token: ResumeToken,
    pub(crate) attempt: Arc<Attempt>,
    pub(crate) result: CallToolResult,
    pub(crate) metadata: RichReceiptMetadata,
}

/// An atomically observed cached rich receipt and its proof metadata.
pub(crate) struct CachedRichReceipt {
    pub(crate) result: CallToolResult,
    pub(crate) metadata: Option<RichReceiptMetadata>,
}

/// A token-matched terminal reduction for one recovery attempt.
pub(crate) enum ResumeFinish {
    /// Closes eligibility and restores the prior receipt because no write polled.
    BeforeWritePoll(CallToolResult),
    /// Records that a suffix write may have applied.
    Indeterminate(CallToolResult),
    /// Replaces the prior receipt with a receipt proved under relative replay.
    Superseded {
        result: CallToolResult,
        metadata: RichReceiptMetadata,
    },
}

impl Attempt {
    pub(crate) fn progress(&self) -> MutationProgress {
        self.progress.clone()
    }

    pub(crate) fn deadline(&self) -> Option<Instant> {
        self.deadline
    }

    pub(crate) async fn record_pending_candidate(
        &self,
        space_id: String,
        object_id: String,
    ) -> PendingCandidate {
        let rich_page_type_id = self.rich_page_type_id.lock().await.clone();
        let candidate = PendingCandidate::new(space_id, object_id, rich_page_type_id);
        *self.pending_candidate.lock().await = Some(candidate.clone());
        candidate
    }

    pub(crate) async fn record_replay_witness(&self, witness: ReplayWitness) {
        *self.replay_witness.lock().await = Some(witness);
    }

    /// Records the page type retained with a rich-page receipt.
    pub(crate) async fn record_rich_page_type_id(&self, page_type_id: String) {
        *self.rich_page_type_id.lock().await = Some(page_type_id);
    }

    pub(crate) fn leader_cancellation(&self) -> CancellationToken {
        self.leader_cancellation.clone()
    }
}

#[derive(Clone)]
pub(crate) struct PendingCandidate(Arc<PendingCandidateInner>);

pub(crate) enum PendingCandidateLookup {
    Available(PendingCandidate),
    Exhausted,
    Absent,
}

struct PendingCandidateInner {
    space_id: String,
    object_id: String,
    rich_page_type_id: Option<String>,
    attempts: AtomicU8,
}

impl PendingCandidate {
    fn new(space_id: String, object_id: String, rich_page_type_id: Option<String>) -> Self {
        Self(Arc::new(PendingCandidateInner {
            space_id,
            object_id,
            rich_page_type_id,
            attempts: AtomicU8::new(0),
        }))
    }

    pub(crate) fn space_id(&self) -> &str {
        &self.0.space_id
    }

    pub(crate) fn object_id(&self) -> &str {
        &self.0.object_id
    }

    pub(crate) fn rich_page_type_id(&self) -> Option<&str> {
        self.0.rich_page_type_id.as_deref()
    }

    pub(crate) fn claim_get_attempt(&self) -> bool {
        self.0
            .attempts
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |attempts| {
                (attempts < 3).then_some(attempts + 1)
            })
            .is_ok()
    }

    fn exhausted(&self) -> bool {
        self.0.attempts.load(Ordering::Acquire) >= 3
    }

    fn same(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

pub(crate) enum BeginAttempt {
    Lead(Arc<Attempt>),
    Wait(Arc<Attempt>),
    Cached(CallToolResult),
    Indeterminate,
    Conflict,
    Full,
    Expired,
}

/// Rich-create admission that snapshots a cached receipt and its metadata
/// while holding the cohort lock.
pub(crate) enum RichBeginAttempt {
    Lead(Arc<Attempt>),
    Wait(Arc<Attempt>),
    Cached(CachedRichReceipt),
    Indeterminate,
    Conflict,
    Full,
    Expired,
}

impl IdempotencyStore {
    pub(crate) fn new(capacity: usize) -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
            capacity,
        }
    }

    pub(crate) async fn begin(&self, key: IdempotencyKey, fingerprint: [u8; 32]) -> BeginAttempt {
        self.begin_inner(None, key, fingerprint).await
    }

    pub(crate) async fn begin_until(
        &self,
        deadline: Instant,
        key: IdempotencyKey,
        fingerprint: [u8; 32],
    ) -> BeginAttempt {
        self.begin_inner(Some(deadline), key, fingerprint).await
    }

    /// Admits a rich create and snapshots any cached result with matching
    /// replay metadata in the same critical section.
    pub(crate) async fn begin_rich_until(
        &self,
        deadline: Instant,
        key: IdempotencyKey,
        fingerprint: [u8; 32],
    ) -> RichBeginAttempt {
        if Instant::now() >= deadline {
            return RichBeginAttempt::Expired;
        }
        let mut entries = self.entries.lock().await;
        if Instant::now() >= deadline {
            return RichBeginAttempt::Expired;
        }
        if let Some(entry) = entries.get(&key) {
            return match entry {
                StoredAttempt::Running {
                    fingerprint: saved,
                    attempt,
                } if saved == &fingerprint => RichBeginAttempt::Wait(attempt.clone()),
                StoredAttempt::Complete(complete) if complete.fingerprint == fingerprint => {
                    RichBeginAttempt::Cached(CachedRichReceipt {
                        result: complete.result.clone(),
                        metadata: complete.replay_metadata.clone(),
                    })
                }
                StoredAttempt::ResumeRunning { prior, .. } if prior.fingerprint == fingerprint => {
                    RichBeginAttempt::Cached(CachedRichReceipt {
                        result: prior.result.clone(),
                        metadata: prior.replay_metadata.clone(),
                    })
                }
                StoredAttempt::Indeterminate { fingerprint: saved } if saved == &fingerprint => {
                    RichBeginAttempt::Indeterminate
                }
                StoredAttempt::PendingCandidate {
                    fingerprint: saved, ..
                } if saved == &fingerprint => RichBeginAttempt::Indeterminate,
                _ => RichBeginAttempt::Conflict,
            };
        }
        if self.capacity == 0 || entries.len() >= self.capacity {
            return RichBeginAttempt::Full;
        }
        let attempt = Arc::new(new_attempt(Some(deadline)));
        entries.insert(
            key.clone(),
            StoredAttempt::Running {
                fingerprint,
                attempt: attempt.clone(),
            },
        );
        if Instant::now() >= deadline {
            entries.remove(&key);
            RichBeginAttempt::Expired
        } else {
            RichBeginAttempt::Lead(attempt)
        }
    }

    async fn begin_inner(
        &self,
        deadline: Option<Instant>,
        key: IdempotencyKey,
        fingerprint: [u8; 32],
    ) -> BeginAttempt {
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            return BeginAttempt::Expired;
        }
        let mut entries = self.entries.lock().await;
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            return BeginAttempt::Expired;
        }
        if let Some(entry) = entries.get(&key) {
            return match entry {
                StoredAttempt::Running {
                    fingerprint: saved,
                    attempt,
                } if saved == &fingerprint => BeginAttempt::Wait(attempt.clone()),
                StoredAttempt::Complete(complete) if complete.fingerprint == fingerprint => {
                    BeginAttempt::Cached(complete.result.clone())
                }
                StoredAttempt::ResumeRunning { prior, .. } if prior.fingerprint == fingerprint => {
                    BeginAttempt::Cached(prior.result.clone())
                }
                StoredAttempt::Indeterminate { fingerprint: saved } if saved == &fingerprint => {
                    BeginAttempt::Indeterminate
                }
                StoredAttempt::PendingCandidate {
                    fingerprint: saved,
                    candidate,
                } if saved == &fingerprint => {
                    let _ = candidate;
                    BeginAttempt::Indeterminate
                }
                _ => BeginAttempt::Conflict,
            };
        }
        if self.capacity == 0 || entries.len() >= self.capacity {
            return BeginAttempt::Full;
        }
        let attempt = Arc::new(new_attempt(deadline));
        entries.insert(
            key.clone(),
            StoredAttempt::Running {
                fingerprint,
                attempt: attempt.clone(),
            },
        );
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            entries.remove(&key);
            BeginAttempt::Expired
        } else {
            BeginAttempt::Lead(attempt)
        }
    }

    pub(crate) async fn finish(
        &self,
        key: &IdempotencyKey,
        attempt: &Arc<Attempt>,
        execution: CreateExecution,
    ) {
        let pending_candidate = attempt.pending_candidate.lock().await.clone();
        let replay_witness = *attempt.replay_witness.lock().await;
        let rich_page_type_id = attempt.rich_page_type_id.lock().await.clone();
        let mut entries = self.entries.lock().await;
        if let Some(StoredAttempt::Running {
            fingerprint,
            attempt: stored,
        }) = entries.get(key)
            && Arc::ptr_eq(stored, attempt)
        {
            let fingerprint = *fingerprint;
            match execution.disposition {
                CreateDisposition::Verified | CreateDisposition::Terminal => {
                    entries.insert(
                        key.clone(),
                        StoredAttempt::Complete(CompleteAttempt {
                            fingerprint,
                            result: execution.result.clone(),
                            replay_metadata: rich_metadata(replay_witness, rich_page_type_id),
                            resume_consumed: false,
                        }),
                    );
                }
                CreateDisposition::Indeterminate => {
                    if let Some(candidate) = pending_candidate {
                        entries.insert(
                            key.clone(),
                            StoredAttempt::PendingCandidate {
                                fingerprint,
                                candidate,
                            },
                        );
                    } else {
                        entries.insert(key.clone(), StoredAttempt::Indeterminate { fingerprint });
                    }
                }
                CreateDisposition::PreDispatchFailure => {
                    entries.remove(key);
                }
            }
        }
        drop(entries);
        *attempt.result.lock().await = Some(execution.result);
        attempt.notify.notify_waiters();
    }

    #[cfg(test)]
    pub(crate) async fn complete_pending_candidate(
        &self,
        key: &IdempotencyKey,
        fingerprint: [u8; 32],
        candidate: &PendingCandidate,
        result: CallToolResult,
    ) -> bool {
        let mut entries = self.entries.lock().await;
        let matches = entries.get(key).is_some_and(|entry| {
            matches!(
                entry,
                StoredAttempt::PendingCandidate {
                    fingerprint: saved,
                    candidate: saved_candidate,
                } if saved == &fingerprint
                    && saved_candidate.same(candidate)
                    && saved_candidate.rich_page_type_id().is_none()
            )
        });
        if matches {
            entries.insert(
                key.clone(),
                StoredAttempt::Complete(CompleteAttempt {
                    fingerprint,
                    result,
                    replay_metadata: None,
                    resume_consumed: false,
                }),
            );
        }
        matches
    }

    /// Replaces a proven pending page candidate using its originally retained
    /// type identity rather than caller-supplied replay metadata.
    pub(crate) async fn complete_pending_rich_candidate(
        &self,
        key: &IdempotencyKey,
        fingerprint: [u8; 32],
        candidate: &PendingCandidate,
        result: CallToolResult,
        observed_page_type_id: &str,
    ) -> bool {
        if observed_page_type_id.is_empty() {
            return false;
        }
        let mut entries = self.entries.lock().await;
        let retained_type_id = entries.get(key).and_then(|entry| match entry {
            StoredAttempt::PendingCandidate {
                fingerprint: saved,
                candidate: saved_candidate,
            } if saved == &fingerprint && saved_candidate.same(candidate) => saved_candidate
                .rich_page_type_id()
                .filter(|retained| *retained == observed_page_type_id)
                .map(str::to_owned),
            _ => None,
        });
        if let Some(page_type_id) = retained_type_id {
            entries.insert(
                key.clone(),
                StoredAttempt::Complete(CompleteAttempt {
                    fingerprint,
                    result,
                    replay_metadata: Some(RichReceiptMetadata {
                        page_type_id,
                        replay_witness: ReplayWitness::RecoveredCandidate,
                    }),
                    resume_consumed: false,
                }),
            );
            true
        } else {
            false
        }
    }

    pub(crate) async fn pending_candidate(
        &self,
        key: &IdempotencyKey,
        fingerprint: [u8; 32],
    ) -> PendingCandidateLookup {
        let entries = self.entries.lock().await;
        match entries.get(key) {
            Some(StoredAttempt::PendingCandidate {
                fingerprint: saved,
                candidate,
            }) if saved == &fingerprint && !candidate.exhausted() => {
                PendingCandidateLookup::Available(candidate.clone())
            }
            Some(StoredAttempt::PendingCandidate {
                fingerprint: saved,
                candidate,
            }) if saved == &fingerprint && candidate.exhausted() => {
                PendingCandidateLookup::Exhausted
            }
            _ => PendingCandidateLookup::Absent,
        }
    }

    #[cfg(test)]
    pub(crate) async fn replay_witness(
        &self,
        key: &IdempotencyKey,
        fingerprint: [u8; 32],
    ) -> Option<ReplayWitness> {
        let entries = self.entries.lock().await;
        match entries.get(key) {
            Some(StoredAttempt::Complete(complete)) if complete.fingerprint == fingerprint => {
                complete
                    .replay_metadata
                    .as_ref()
                    .map(|metadata| metadata.replay_witness)
            }
            Some(StoredAttempt::ResumeRunning { prior, .. })
                if prior.fingerprint == fingerprint =>
            {
                prior
                    .replay_metadata
                    .as_ref()
                    .map(|metadata| metadata.replay_witness)
            }
            _ => None,
        }
    }

    /// Reports whether a retained rich receipt can be claimed without I/O.
    pub(crate) async fn resume_eligibility<F>(
        &self,
        key: &IdempotencyKey,
        receipt_shape: F,
    ) -> ResumeEligibilitySnapshot
    where
        F: FnOnce(&CallToolResult) -> bool,
    {
        let entries = self.entries.lock().await;
        match entries.get(key) {
            Some(StoredAttempt::Complete(complete))
                if !complete.resume_consumed
                    && receipt_shape(&complete.result)
                    && complete
                        .replay_metadata
                        .as_ref()
                        .is_some_and(|metadata| !metadata.page_type_id.is_empty()) =>
            {
                let Some(metadata) = complete.replay_metadata.clone() else {
                    return ResumeEligibilitySnapshot::Refused;
                };
                ResumeEligibilitySnapshot::Eligible {
                    fingerprint: complete.fingerprint,
                    metadata,
                }
            }
            _ => ResumeEligibilitySnapshot::Refused,
        }
    }

    /// Atomically validates and claims one retained rich receipt for recovery.
    pub(crate) async fn claim_resume<F>(
        &self,
        key: &IdempotencyKey,
        fingerprint: [u8; 32],
        expected_metadata: &RichReceiptMetadata,
        receipt_shape: F,
    ) -> Result<ResumeClaim, ()>
    where
        F: FnOnce(&CallToolResult) -> bool,
    {
        let mut entries = self.entries.lock().await;
        let Some(StoredAttempt::Complete(complete)) = entries.get(key) else {
            return Err(());
        };
        if complete.fingerprint != fingerprint
            || complete.resume_consumed
            || complete.replay_metadata.as_ref() != Some(expected_metadata)
            || !receipt_shape(&complete.result)
        {
            return Err(());
        }
        let Some(metadata) = complete.replay_metadata.clone() else {
            return Err(());
        };
        if metadata.page_type_id.is_empty() {
            return Err(());
        }
        let mut prior = complete.clone();
        prior.resume_consumed = true;
        let token = ResumeToken::new();
        let attempt = Arc::new(new_attempt(None));
        let claim = ResumeClaim {
            token: token.clone(),
            attempt: attempt.clone(),
            result: prior.result.clone(),
            metadata: metadata.clone(),
        };
        entries.insert(
            key.clone(),
            StoredAttempt::ResumeRunning {
                prior,
                token,
                attempt,
            },
        );
        Ok(claim)
    }

    /// Finishes a rich recovery only when its token still owns the cohort.
    pub(crate) async fn finish_resume(
        &self,
        key: &IdempotencyKey,
        token: &ResumeToken,
        finish: ResumeFinish,
    ) -> bool {
        let mut entries = self.entries.lock().await;
        let Some(StoredAttempt::ResumeRunning {
            prior,
            token: stored_token,
            attempt,
        }) = entries.get(key)
        else {
            return false;
        };
        if !stored_token.matches(token) {
            return false;
        }
        let mut restored = prior.clone();
        let attempt = attempt.clone();
        let result = match &finish {
            ResumeFinish::BeforeWritePoll(result) => result.clone(),
            ResumeFinish::Indeterminate(result) => {
                restored.result = result.clone();
                if let Some(metadata) = restored.replay_metadata.as_mut() {
                    metadata.replay_witness = ReplayWitness::ResumedRelative;
                }
                result.clone()
            }
            ResumeFinish::Superseded { result, metadata } => {
                restored.result = result.clone();
                restored.replay_metadata = Some(metadata.clone());
                result.clone()
            }
        };
        match finish {
            ResumeFinish::BeforeWritePoll(_)
            | ResumeFinish::Indeterminate(_)
            | ResumeFinish::Superseded { .. } => {
                entries.insert(key.clone(), StoredAttempt::Complete(restored));
            }
        }
        drop(entries);
        *attempt.result.lock().await = Some(result);
        attempt.notify.notify_waiters();
        true
    }
}

fn rich_metadata(
    replay_witness: Option<ReplayWitness>,
    page_type_id: Option<String>,
) -> Option<RichReceiptMetadata> {
    match (replay_witness, page_type_id) {
        (Some(replay_witness), Some(page_type_id)) if !page_type_id.is_empty() => {
            Some(RichReceiptMetadata {
                page_type_id,
                replay_witness,
            })
        }
        _ => None,
    }
}

fn new_attempt(deadline: Option<Instant>) -> Attempt {
    Attempt {
        result: Mutex::new(None),
        notify: Notify::new(),
        progress: MutationProgress::new(),
        deadline,
        pending_candidate: Mutex::new(None),
        replay_witness: Mutex::new(None),
        rich_page_type_id: Mutex::new(None),
        leader_cancellation: CancellationToken::new(),
    }
}

pub(crate) async fn wait_for_attempt(
    attempt: Arc<Attempt>,
    cancellation: &CancellationToken,
) -> CallToolResult {
    loop {
        let notified = attempt.notify.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        if let Some(result) = attempt.result.lock().await.clone() {
            return result;
        }
        tokio::select! {
            biased;
            () = cancellation.cancelled() => {
                let error = match attempt.progress.stage() {
                    MutationStage::PreDispatch => ToolError::upstream(),
                    MutationStage::Dispatched => ToolError::mutation_indeterminate(),
                };
                return tool_error(&error);
            },
            () = &mut notified => {}
        }
    }
}

pub(crate) async fn wait_for_attempt_until(
    attempt: Arc<Attempt>,
    cancellation: &CancellationToken,
    invocation_deadline: Instant,
) -> CallToolResult {
    let deadline = attempt.deadline.map_or(invocation_deadline, |leader| {
        leader.min(invocation_deadline)
    });
    loop {
        let notified = attempt.notify.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        if let Some(result) = attempt.result.lock().await.clone() {
            return result;
        }
        tokio::select! {
            biased;
            () = cancellation.cancelled() => {
                let error = match attempt.progress.stage() {
                    MutationStage::PreDispatch => ToolError::upstream(),
                    MutationStage::Dispatched => ToolError::mutation_indeterminate(),
                };
                return tool_error(&error);
            },
            () = tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)) => {
                let error = match attempt.progress.stage() {
                    MutationStage::PreDispatch => ToolError::upstream(),
                    MutationStage::Dispatched => ToolError::mutation_indeterminate(),
                };
                return tool_error(&error);
            },
            () = &mut notified => {}
        }
    }
}

pub(crate) async fn wait_for_leader_attempt_until(
    attempt: Arc<Attempt>,
    cancellation: &CancellationToken,
    invocation_deadline: Instant,
) -> CallToolResult {
    let deadline = attempt.deadline.map_or(invocation_deadline, |leader| {
        leader.min(invocation_deadline)
    });
    let leader_cancellation = attempt.leader_cancellation();
    let mut cancellation_forwarded = false;
    loop {
        let notified = attempt.notify.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        if let Some(result) = attempt.result.lock().await.clone() {
            return result;
        }
        tokio::select! {
            biased;
            () = cancellation.cancelled(), if !cancellation_forwarded => {
                cancellation_forwarded = true;
                leader_cancellation.cancel();
            },
            () = tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)) => {
                leader_cancellation.cancel();
                let error = match attempt.progress.stage() {
                    MutationStage::PreDispatch => ToolError::upstream(),
                    MutationStage::Dispatched => ToolError::conflict(),
                };
                return tool_error(&error);
            },
            () = &mut notified => {}
        }
    }
}

pub(crate) async fn finish_supervised_execution(
    task: JoinHandle<CreateExecution>,
    progress: &MutationProgress,
) -> CreateExecution {
    task.await
        .unwrap_or_else(|_| CreateExecution::supervisor_failed(progress.stage()))
}

pub(crate) struct CreateExecution {
    pub(crate) result: CallToolResult,
    pub(crate) disposition: CreateDisposition,
}

impl CreateExecution {
    pub(crate) fn new(result: CallToolResult, disposition: CreateDisposition) -> Self {
        Self {
            result,
            disposition,
        }
    }

    pub(crate) fn supervisor_failed(stage: MutationStage) -> Self {
        let (error, disposition) = match stage {
            MutationStage::PreDispatch => {
                (ToolError::upstream(), CreateDisposition::PreDispatchFailure)
            }
            MutationStage::Dispatched => (
                ToolError::mutation_indeterminate(),
                CreateDisposition::Indeterminate,
            ),
        };
        Self {
            result: tool_error(&error),
            disposition,
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) enum CreateDisposition {
    Verified,
    /// A definitive result that must be retained without another write.
    Terminal,
    Indeterminate,
    PreDispatchFailure,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> IdempotencyKey {
        IdempotencyKey::new("pending-key").expect("test key")
    }

    #[tokio::test]
    async fn pending_candidate_get_attempts_are_lifetime_bounded_and_closed() {
        let store = IdempotencyStore::new(1);
        let key = key();
        let fingerprint = [7; 32];
        let BeginAttempt::Lead(attempt) = store.begin(key.clone(), fingerprint).await else {
            panic!("first call leads");
        };
        let candidate = attempt
            .record_pending_candidate("space".to_owned(), "object".to_owned())
            .await;
        assert!(candidate.claim_get_attempt());
        store
            .finish(
                &key,
                &attempt,
                CreateExecution::new(
                    tool_error(&ToolError::conflict()),
                    CreateDisposition::Indeterminate,
                ),
            )
            .await;

        assert!(matches!(
            store.pending_candidate(&key, fingerprint).await,
            PendingCandidateLookup::Available(_)
        ));
        assert!(candidate.claim_get_attempt());
        assert!(candidate.claim_get_attempt());
        assert!(!candidate.claim_get_attempt());
        assert!(matches!(
            store.pending_candidate(&key, fingerprint).await,
            PendingCandidateLookup::Exhausted
        ));
        assert!(matches!(
            store.pending_candidate(&key, [8; 32]).await,
            PendingCandidateLookup::Absent
        ));
    }

    #[tokio::test]
    async fn proven_pending_candidate_becomes_a_terminal_cached_receipt() {
        let store = IdempotencyStore::new(1);
        let key = key();
        let fingerprint = [3; 32];
        let BeginAttempt::Lead(attempt) = store.begin(key.clone(), fingerprint).await else {
            panic!("first call leads");
        };
        let candidate = attempt
            .record_pending_candidate("space".to_owned(), "object".to_owned())
            .await;
        store
            .finish(
                &key,
                &attempt,
                CreateExecution::new(
                    tool_error(&ToolError::conflict()),
                    CreateDisposition::Indeterminate,
                ),
            )
            .await;
        let receipt = CallToolResult::structured(serde_json::json!({"status":"partial"}));
        assert!(
            store
                .complete_pending_candidate(&key, fingerprint, &candidate, receipt.clone(),)
                .await
        );
        let BeginAttempt::Cached(cached) = store.begin(key, fingerprint).await else {
            panic!("proven candidate is cached");
        };
        assert_eq!(cached.structured_content, receipt.structured_content);
    }

    #[tokio::test]
    async fn terminal_attempt_preserves_internal_replay_witness() {
        let store = IdempotencyStore::new(1);
        let key = key();
        let fingerprint = [5; 32];
        let BeginAttempt::Lead(attempt) = store.begin(key.clone(), fingerprint).await else {
            panic!("first call leads");
        };
        attempt
            .record_replay_witness(ReplayWitness::RootAppendIndex(7))
            .await;
        attempt
            .record_rich_page_type_id("page-type".to_owned())
            .await;
        store
            .finish(
                &key,
                &attempt,
                CreateExecution::new(
                    CallToolResult::structured(serde_json::json!({"status":"partial"})),
                    CreateDisposition::Terminal,
                ),
            )
            .await;

        assert!(matches!(
            store.begin(key.clone(), fingerprint).await,
            BeginAttempt::Cached(_)
        ));
        assert_eq!(
            store.replay_witness(&key, fingerprint).await,
            Some(ReplayWitness::RootAppendIndex(7))
        );
        assert_eq!(store.replay_witness(&key, [6; 32]).await, None);
    }

    async fn complete_rich(store: &IdempotencyStore, key: &IdempotencyKey, fingerprint: [u8; 32]) {
        let BeginAttempt::Lead(attempt) = store.begin(key.clone(), fingerprint).await else {
            panic!("first call leads");
        };
        attempt
            .record_replay_witness(ReplayWitness::RootAppendIndex(0))
            .await;
        attempt
            .record_rich_page_type_id("page-type".to_owned())
            .await;
        store
            .finish(
                key,
                &attempt,
                CreateExecution::new(
                    CallToolResult::structured(serde_json::json!({"status":"partial"})),
                    CreateDisposition::Terminal,
                ),
            )
            .await;
    }

    fn test_metadata() -> RichReceiptMetadata {
        RichReceiptMetadata {
            page_type_id: "page-type".to_owned(),
            replay_witness: ReplayWitness::RootAppendIndex(0),
        }
    }

    #[tokio::test]
    async fn resume_claim_is_single_slot_and_create_observes_prior_receipt() {
        let store = IdempotencyStore::new(1);
        let key = key();
        let fingerprint = [9; 32];
        complete_rich(&store, &key, fingerprint).await;

        let claim = store
            .claim_resume(&key, fingerprint, &test_metadata(), |_| true)
            .await
            .expect("first claim");
        assert!(
            store
                .claim_resume(&key, fingerprint, &test_metadata(), |_| true)
                .await
                .is_err()
        );
        assert!(matches!(
            store.begin(key.clone(), fingerprint).await,
            BeginAttempt::Cached(_)
        ));
        assert!(matches!(
            store.resume_eligibility(&key, |_| true).await,
            ResumeEligibilitySnapshot::Refused
        ));
        assert!(
            store
                .finish_resume(
                    &key,
                    &claim.token,
                    ResumeFinish::BeforeWritePoll(tool_error(&ToolError::conflict())),
                )
                .await
        );
        assert!(matches!(
            store.begin(key, fingerprint).await,
            BeginAttempt::Cached(_)
        ));
    }

    #[tokio::test]
    async fn stale_resume_token_is_a_noop_and_post_poll_retains_receipt() {
        let store = IdempotencyStore::new(1);
        let key = key();
        let fingerprint = [10; 32];
        complete_rich(&store, &key, fingerprint).await;
        let claim = store
            .claim_resume(&key, fingerprint, &test_metadata(), |_| true)
            .await
            .expect("claim");
        let stale = ResumeToken::new();
        assert!(
            !store
                .finish_resume(
                    &key,
                    &stale,
                    ResumeFinish::BeforeWritePoll(tool_error(&ToolError::conflict())),
                )
                .await
        );
        assert!(
            store
                .finish_resume(
                    &key,
                    &claim.token,
                    ResumeFinish::Indeterminate(tool_error(&ToolError::conflict())),
                )
                .await
        );
        assert!(matches!(
            store.begin(key, fingerprint).await,
            BeginAttempt::Cached(_)
        ));
    }

    #[tokio::test]
    async fn resume_claim_rechecks_fingerprint_and_works_at_capacity() {
        let store = IdempotencyStore::new(1);
        let key = key();
        let fingerprint = [11; 32];
        complete_rich(&store, &key, fingerprint).await;
        assert!(
            store
                .claim_resume(&key, [12; 32], &test_metadata(), |_| true)
                .await
                .is_err()
        );
        assert!(
            store
                .claim_resume(&key, fingerprint, &test_metadata(), |_| false)
                .await
                .is_err()
        );
        assert!(
            store
                .claim_resume(&key, fingerprint, &test_metadata(), |_| true)
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn rich_cached_admission_keeps_prior_across_superseding_finish() {
        let store = IdempotencyStore::new(1);
        let key = key();
        let fingerprint = [13; 32];
        complete_rich(&store, &key, fingerprint).await;
        let claim = store
            .claim_resume(&key, fingerprint, &test_metadata(), |_| true)
            .await
            .expect("resume claim");
        let RichBeginAttempt::Cached(observed) = store
            .begin_rich_until(
                Instant::now() + std::time::Duration::from_secs(1),
                key.clone(),
                fingerprint,
            )
            .await
        else {
            panic!("cached rich admission");
        };
        let superseding = CallToolResult::structured(
            serde_json::json!({"status":"complete","marker":"superseding"}),
        );
        assert!(
            store
                .finish_resume(
                    &key,
                    &claim.token,
                    ResumeFinish::Superseded {
                        result: superseding,
                        metadata: RichReceiptMetadata {
                            page_type_id: "page-type".to_owned(),
                            replay_witness: ReplayWitness::ResumedRelative,
                        },
                    },
                )
                .await
        );
        assert_eq!(
            observed
                .result
                .structured_content
                .as_ref()
                .and_then(|value| value.get("status"))
                .and_then(serde_json::Value::as_str),
            Some("partial")
        );
        let RichBeginAttempt::Cached(current) = store
            .begin_rich_until(
                Instant::now() + std::time::Duration::from_secs(1),
                key,
                fingerprint,
            )
            .await
        else {
            panic!("superseding rich admission");
        };
        assert_eq!(
            current
                .result
                .structured_content
                .as_ref()
                .and_then(|value| value.get("marker"))
                .and_then(serde_json::Value::as_str),
            Some("superseding")
        );
    }

    #[tokio::test]
    async fn rich_cached_admission_retains_indeterminate_finish() {
        let store = IdempotencyStore::new(1);
        let key = key();
        let fingerprint = [14; 32];
        complete_rich(&store, &key, fingerprint).await;
        let claim = store
            .claim_resume(&key, fingerprint, &test_metadata(), |_| true)
            .await
            .expect("resume claim");
        let RichBeginAttempt::Cached(observed) = store
            .begin_rich_until(
                Instant::now() + std::time::Duration::from_secs(1),
                key.clone(),
                fingerprint,
            )
            .await
        else {
            panic!("cached rich admission");
        };
        let indeterminate = CallToolResult::structured(
            serde_json::json!({"status":"indeterminate","marker":"resumed"}),
        );
        assert!(
            store
                .finish_resume(
                    &key,
                    &claim.token,
                    ResumeFinish::Indeterminate(indeterminate),
                )
                .await
        );
        assert_eq!(
            observed
                .result
                .structured_content
                .as_ref()
                .and_then(|value| value.get("status"))
                .and_then(serde_json::Value::as_str),
            Some("partial")
        );
        let RichBeginAttempt::Cached(current) = store
            .begin_rich_until(
                Instant::now() + std::time::Duration::from_secs(1),
                key,
                fingerprint,
            )
            .await
        else {
            panic!("indeterminate rich admission");
        };
        assert_eq!(
            current
                .result
                .structured_content
                .as_ref()
                .and_then(|value| value.get("status"))
                .and_then(serde_json::Value::as_str),
            Some("indeterminate")
        );
        assert_eq!(
            current.metadata.map(|metadata| metadata.replay_witness),
            Some(ReplayWitness::ResumedRelative)
        );
    }

    #[tokio::test]
    async fn pending_rich_candidate_cannot_rebind_page_type() {
        let store = IdempotencyStore::new(1);
        let key = key();
        let fingerprint = [15; 32];
        let BeginAttempt::Lead(attempt) = store.begin(key.clone(), fingerprint).await else {
            panic!("first call leads");
        };
        attempt
            .record_rich_page_type_id("page-type-one".to_owned())
            .await;
        let candidate = attempt
            .record_pending_candidate("space".to_owned(), "object".to_owned())
            .await;
        store
            .finish(
                &key,
                &attempt,
                CreateExecution::new(
                    tool_error(&ToolError::conflict()),
                    CreateDisposition::Indeterminate,
                ),
            )
            .await;
        let receipt = CallToolResult::structured(serde_json::json!({"status":"partial"}));
        assert!(
            !store
                .complete_pending_rich_candidate(
                    &key,
                    fingerprint,
                    &candidate,
                    receipt.clone(),
                    "page-type-two",
                )
                .await
        );
        assert!(
            !store
                .complete_pending_candidate(&key, fingerprint, &candidate, receipt.clone())
                .await
        );
        assert!(
            store
                .complete_pending_rich_candidate(
                    &key,
                    fingerprint,
                    &candidate,
                    receipt,
                    "page-type-one",
                )
                .await
        );
        let RichBeginAttempt::Cached(cached) = store
            .begin_rich_until(
                Instant::now() + std::time::Duration::from_secs(1),
                key,
                fingerprint,
            )
            .await
        else {
            panic!("recovered candidate cached");
        };
        assert_eq!(
            cached.metadata.map(|metadata| metadata.page_type_id),
            Some("page-type-one".to_owned())
        );
    }

    #[tokio::test]
    async fn raw_resume_eligibility_applies_receipt_shape_before_admission() {
        let store = IdempotencyStore::new(1);
        let key = key();
        let fingerprint = [16; 32];
        complete_rich(&store, &key, fingerprint).await;
        assert!(matches!(
            store.resume_eligibility(&key, |_| false).await,
            ResumeEligibilitySnapshot::Refused
        ));
        assert!(matches!(
            store.resume_eligibility(&key, |_| true).await,
            ResumeEligibilitySnapshot::Eligible { .. }
        ));
    }
}
