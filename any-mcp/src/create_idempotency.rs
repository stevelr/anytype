// any-mcp - bounded, workflow-oriented MCP server for Anytype
//
// SPDX-FileCopyrightText: 2026 Steve Schoettler
// SPDX-License-Identifier: Apache-2.0

//! Shared finite process-local coordination for create workflows.

use std::{borrow::Cow, collections::HashMap, fmt, sync::Arc};

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
    },
    Indeterminate {
        fingerprint: [u8; 32],
    },
}

pub(crate) struct Attempt {
    result: Mutex<Option<CallToolResult>>,
    notify: Notify,
    progress: MutationProgress,
}

impl Attempt {
    pub(crate) fn progress(&self) -> MutationProgress {
        self.progress.clone()
    }
}

pub(crate) enum BeginAttempt {
    Lead(Arc<Attempt>),
    Wait(Arc<Attempt>),
    Cached(CallToolResult),
    Indeterminate,
    Conflict,
    Full,
}

impl IdempotencyStore {
    pub(crate) fn new(capacity: usize) -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
            capacity,
        }
    }

    pub(crate) async fn begin(&self, key: IdempotencyKey, fingerprint: [u8; 32]) -> BeginAttempt {
        let mut entries = self.entries.lock().await;
        if let Some(entry) = entries.get(&key) {
            return match entry {
                StoredAttempt::Running {
                    fingerprint: saved,
                    attempt,
                } if saved == &fingerprint => BeginAttempt::Wait(attempt.clone()),
                StoredAttempt::Complete {
                    fingerprint: saved,
                    result,
                } if saved == &fingerprint => BeginAttempt::Cached(result.clone()),
                StoredAttempt::Indeterminate { fingerprint: saved } if saved == &fingerprint => {
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
        });
        entries.insert(
            key,
            StoredAttempt::Running {
                fingerprint,
                attempt: attempt.clone(),
            },
        );
        BeginAttempt::Lead(attempt)
    }

    pub(crate) async fn finish(
        &self,
        key: &IdempotencyKey,
        attempt: &Arc<Attempt>,
        execution: CreateExecution,
    ) {
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
                        },
                    );
                }
                CreateDisposition::Indeterminate => {
                    entries.insert(key.clone(), StoredAttempt::Indeterminate { fingerprint });
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

    fn supervisor_failed(stage: MutationStage) -> Self {
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
    Indeterminate,
    PreDispatchFailure,
}
