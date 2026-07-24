// any-mcp - bounded, workflow-oriented MCP server for Anytype
//
// SPDX-FileCopyrightText: 2026 Steve Schoettler
// SPDX-License-Identifier: Apache-2.0

//! Process-global HTTP request admission bounds.
//!
//! The fixed-window request-rate counter and the admitted-request semaphore
//! run before authentication, body collection, JSON decoding, and every
//! handler permit. They provide coarse process self-protection, not tenant
//! fairness; a remote reverse proxy must add per-client limits.

use std::{
    sync::Mutex,
    time::{Duration, Instant},
};

const WINDOW: Duration = Duration::from_secs(60);

/// Fixed-window process-global request-rate counter.
///
/// The counter has no identity map: every request that reaches the rate gate
/// consumes one admission, including CORS preflight and requests later
/// rejected by authentication or capacity gates.
#[derive(Debug)]
pub(crate) struct RateLimiter {
    limit: u32,
    state: Mutex<WindowState>,
}

#[derive(Debug)]
struct WindowState {
    window_start: Instant,
    admitted: u32,
}

impl RateLimiter {
    pub(crate) fn new(limit: u32) -> Self {
        Self {
            limit,
            state: Mutex::new(WindowState {
                window_start: Instant::now(),
                admitted: 0,
            }),
        }
    }

    /// Admits one request or reports rate exhaustion.
    pub(crate) fn try_admit(&self) -> bool {
        self.try_admit_at(Instant::now())
    }

    fn try_admit_at(&self, now: Instant) -> bool {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if now.saturating_duration_since(state.window_start) >= WINDOW {
            state.window_start = now;
            state.admitted = 0;
        }
        if state.admitted >= self.limit {
            return false;
        }
        state.admitted = state.admitted.saturating_add(1);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admits_up_to_the_limit_within_one_window() {
        let limiter = RateLimiter::new(3);
        let start = Instant::now();
        assert!(limiter.try_admit_at(start));
        assert!(limiter.try_admit_at(start));
        assert!(limiter.try_admit_at(start + Duration::from_secs(59)));
        assert!(!limiter.try_admit_at(start + Duration::from_secs(59)));
        assert!(!limiter.try_admit_at(start + Duration::from_millis(59_999)));
    }

    #[test]
    fn window_rollover_resets_the_counter() {
        let limiter = RateLimiter::new(1);
        let start = Instant::now();
        assert!(limiter.try_admit_at(start));
        assert!(!limiter.try_admit_at(start));
        assert!(limiter.try_admit_at(start + WINDOW));
        assert!(!limiter.try_admit_at(start + WINDOW));
        assert!(limiter.try_admit_at(start + WINDOW + WINDOW));
    }

    #[test]
    fn one_request_minimum_limit_is_exact() {
        let limiter = RateLimiter::new(1);
        let start = Instant::now();
        assert!(limiter.try_admit_at(start));
        for offset in 1..60 {
            assert!(!limiter.try_admit_at(start + Duration::from_secs(offset)));
        }
        assert!(limiter.try_admit_at(start + Duration::from_secs(60)));
    }
}
