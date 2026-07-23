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
    Complete {
        fingerprint: [u8; 32],
        result: CallToolResult,
        replay_witness: Option<ReplayWitness>,
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
    leader_cancellation: CancellationToken,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ReplayWitness {
    RichRootAppendIndex(u64),
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
        let candidate = PendingCandidate::new(space_id, object_id);
        *self.pending_candidate.lock().await = Some(candidate.clone());
        candidate
    }

    pub(crate) async fn record_replay_witness(&self, witness: ReplayWitness) {
        *self.replay_witness.lock().await = Some(witness);
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
    attempts: AtomicU8,
}

impl PendingCandidate {
    fn new(space_id: String, object_id: String) -> Self {
        Self(Arc::new(PendingCandidateInner {
            space_id,
            object_id,
            attempts: AtomicU8::new(0),
        }))
    }

    pub(crate) fn space_id(&self) -> &str {
        &self.0.space_id
    }

    pub(crate) fn object_id(&self) -> &str {
        &self.0.object_id
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
                StoredAttempt::Complete {
                    fingerprint: saved,
                    result,
                    ..
                } if saved == &fingerprint => BeginAttempt::Cached(result.clone()),
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
        let attempt = Arc::new(Attempt {
            result: Mutex::new(None),
            notify: Notify::new(),
            progress: MutationProgress::new(),
            deadline,
            pending_candidate: Mutex::new(None),
            replay_witness: Mutex::new(None),
            leader_cancellation: CancellationToken::new(),
        });
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
        let mut entries = self.entries.lock().await;
        if let Some(StoredAttempt::Running {
            fingerprint,
            attempt: stored,
        }) = entries.get(key)
            && Arc::ptr_eq(stored, attempt)
        {
            let fingerprint = *fingerprint;
            match execution.disposition {
                CreateDisposition::Verified => {
                    entries.insert(
                        key.clone(),
                        StoredAttempt::Complete {
                            fingerprint,
                            result: execution.result.clone(),
                            replay_witness,
                        },
                    );
                }
                CreateDisposition::Terminal => {
                    entries.insert(
                        key.clone(),
                        StoredAttempt::Complete {
                            fingerprint,
                            result: execution.result.clone(),
                            replay_witness,
                        },
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
                } if saved == &fingerprint && saved_candidate.same(candidate)
            )
        });
        if matches {
            entries.insert(
                key.clone(),
                StoredAttempt::Complete {
                    fingerprint,
                    result,
                    replay_witness: None,
                },
            );
        }
        matches
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

    pub(crate) async fn replay_witness(
        &self,
        key: &IdempotencyKey,
        fingerprint: [u8; 32],
    ) -> Option<ReplayWitness> {
        let entries = self.entries.lock().await;
        match entries.get(key) {
            Some(StoredAttempt::Complete {
                fingerprint: saved,
                replay_witness,
                ..
            }) if saved == &fingerprint => *replay_witness,
            _ => None,
        }
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
            .record_replay_witness(ReplayWitness::RichRootAppendIndex(7))
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
            Some(ReplayWitness::RichRootAppendIndex(7))
        );
        assert_eq!(store.replay_witness(&key, [6; 32]).await, None);
    }
}
