// any-mcp - bounded, workflow-oriented MCP server for Anytype
//
// SPDX-FileCopyrightText: 2026 Steve Schoettler
// SPDX-License-Identifier: Apache-2.0

//! Private, runtime-owned synchronization points for artifact acceptance tests.

use std::{collections::HashMap, sync::Arc, time::Duration};

use tokio::sync::{Mutex, watch};

/// An exact artifact operation point exposed only to the acceptance harness.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub enum ArtifactAcceptanceGatePoint {
    /// At least one upload chunk has been consumed by the import request.
    ImportFirstUploadChunk,
    /// The final export namespace check succeeded and publication is next.
    ExportPrepublication,
    /// A document import is about to perform its final source check.
    DocumentFinalRevalidation,
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
struct GateKey {
    point: ArtifactAcceptanceGatePoint,
    operation: [u8; 32],
}

#[derive(Debug)]
struct ArmedGate {
    entered: watch::Sender<bool>,
    released: watch::Receiver<bool>,
}

/// One runtime's opt-in acceptance synchronization state.
///
/// Production construction leaves this disabled. Each arm is consumed once,
/// is scoped to the supplied operation digest, and has a bounded wait.
#[derive(Clone, Debug, Default)]
pub struct ArtifactAcceptanceGates {
    enabled: bool,
    arms: Arc<Mutex<HashMap<GateKey, ArmedGate>>>,
}

/// A test-side lease for one armed acceptance point.
#[derive(Clone, Debug)]
pub struct ArtifactAcceptanceGateLease {
    entered: watch::Receiver<bool>,
    released: watch::Sender<bool>,
}

/// Describes why an acceptance point could not be armed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArtifactAcceptanceGateError {
    /// The runtime was constructed for ordinary production use.
    Disabled,
    /// The exact point and operation are already armed.
    AlreadyArmed,
}

impl ArtifactAcceptanceGates {
    /// Creates a gate-free runtime facility.
    #[must_use]
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            arms: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Creates an acceptance-only facility that permits in-process arming.
    #[must_use]
    pub fn enabled() -> Self {
        Self {
            enabled: true,
            arms: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Arms one exact point for one operation digest.
    pub async fn arm(
        &self,
        point: ArtifactAcceptanceGatePoint,
        operation: [u8; 32],
    ) -> Result<ArtifactAcceptanceGateLease, ArtifactAcceptanceGateError> {
        if !self.enabled {
            return Err(ArtifactAcceptanceGateError::Disabled);
        }
        let key = GateKey { point, operation };
        let (entered, entered_receiver) = watch::channel(false);
        let (released, released_receiver) = watch::channel(false);
        let gate = ArmedGate {
            entered,
            released: released_receiver,
        };
        let lease = ArtifactAcceptanceGateLease {
            entered: entered_receiver,
            released,
        };
        let mut arms = self.arms.lock().await;
        if let std::collections::hash_map::Entry::Vacant(slot) = arms.entry(key) {
            slot.insert(gate);
            Ok(lease)
        } else {
            Err(ArtifactAcceptanceGateError::AlreadyArmed)
        }
    }

    /// Pauses a matching operation once, with a bounded fail-closed wait.
    pub(crate) async fn reach(
        &self,
        point: ArtifactAcceptanceGatePoint,
        operation: [u8; 32],
    ) -> bool {
        if !self.enabled {
            return true;
        }
        let gate = self.arms.lock().await.remove(&GateKey { point, operation });
        let Some(gate) = gate else {
            return true;
        };
        let _ = gate.entered.send(true);
        let mut released = gate.released;
        if *released.borrow() {
            return true;
        }
        tokio::time::timeout(Duration::from_secs(30), released.changed())
            .await
            .is_ok_and(|result| result.is_ok() && *released.borrow())
    }
}

impl ArtifactAcceptanceGateLease {
    /// Waits until the runtime reaches the armed point.
    pub async fn wait_until_reached(&self, timeout: Duration) -> bool {
        let mut entered = self.entered.clone();
        if *entered.borrow() {
            return true;
        }
        tokio::time::timeout(timeout, entered.changed())
            .await
            .is_ok_and(|result| result.is_ok() && *entered.borrow())
    }

    /// Lets the one paused operation continue.
    pub fn release(&self) {
        let _ = self.released.send(true);
    }
}

impl Drop for ArtifactAcceptanceGateLease {
    fn drop(&mut self) {
        let _ = self.released.send(true);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn arm_is_one_shot_and_scoped_to_one_operation() {
        let gates = ArtifactAcceptanceGates::enabled();
        let operation = [7_u8; 32];
        let lease = gates
            .arm(
                ArtifactAcceptanceGatePoint::ImportFirstUploadChunk,
                operation,
            )
            .await
            .expect("arm gate");
        let reached = gates.clone();
        let task = tokio::spawn(async move {
            reached
                .reach(
                    ArtifactAcceptanceGatePoint::ImportFirstUploadChunk,
                    operation,
                )
                .await;
        });
        assert!(lease.wait_until_reached(Duration::from_secs(1)).await);
        lease.release();
        task.await.expect("gate task");
        gates
            .reach(
                ArtifactAcceptanceGatePoint::ImportFirstUploadChunk,
                operation,
            )
            .await;
    }
}
