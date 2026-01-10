//! Shared test utilities for anytype integration tests
//!
//! This module provides common functionality for all integration tests:
//! - Test context creation with client configuration
//! - Environment variable handling
//! - Error types and result handling
//! - Test isolation utilities
#![cfg(test)]
#![allow(dead_code)]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tokio::time::sleep;
use tracing::error;

use anytype::prelude::*;
use anytype::test_util::TestError;

pub use anytype::test_util::{TestContext, TestResult};

// =============================================================================
// Test Helpers
// =============================================================================

/// Generate a unique test name with timestamp
pub fn unique_test_name(prefix: &str) -> String {
    format!("{} {}", prefix, chrono::Utc::now().timestamp_millis())
}

static TYPE_COUNTER: AtomicUsize = AtomicUsize::new(0);

fn unique_type_key(prefix: &str) -> String {
    let counter = TYPE_COUNTER.fetch_add(1, Ordering::SeqCst);
    format!(
        "{}_{}_{}",
        prefix,
        chrono::Utc::now().timestamp_millis(),
        counter
    )
}

fn is_key_already_exists_error(err: &AnytypeError, key_kind: &str, key: &str) -> bool {
    let message = match err {
        AnytypeError::Validation { message } => message,
        AnytypeError::ApiError { message, .. } => message,
        _ => return false,
    };

    message.contains("already exists") && message.contains(key_kind) && message.contains(key)
}

pub async fn create_object_with_retry<F, Fut>(label: &str, mut f: F) -> TestResult<Object>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<Object, AnytypeError>>,
{
    let mut delay_ms = 200u64;
    let max_attempts = 5;

    for attempt in 1..=max_attempts {
        match f().await {
            Ok(obj) => return Ok(obj),
            Err(e) => {
                let retryable = match &e {
                    AnytypeError::ApiError { code, message, .. } => {
                        *code == 500
                            || (*code == 400
                                && (message.contains("invalid multi_select option")
                                    || message.contains("invalid select option")
                                    || message.contains("unknown property key")))
                    }
                    AnytypeError::Validation { message } => {
                        message.contains("invalid multi_select option")
                            || message.contains("invalid select option")
                            || message.contains("unknown property key")
                    }
                    _ => false,
                };
                if retryable && attempt < max_attempts {
                    eprintln!("{label} create failed (attempt {attempt}), retrying: {e}");
                    sleep(Duration::from_millis(delay_ms)).await;
                    delay_ms = (delay_ms * 2).min(1500);
                    continue;
                }
                eprintln!("failed to create {label}: {e}");
                return Err(e.into());
            }
        }
    }

    Err(TestError::Assertion {
        message: format!("failed to create {label} after {max_attempts} attempts"),
    })
}

pub async fn lookup_property_tag_with_retry(
    ctx: &TestContext,
    prop_key: &str,
    tag_name: &str,
) -> TestResult<Tag> {
    let mut delay_ms = 200u64;
    let max_attempts = 5;

    for attempt in 1..=max_attempts {
        match ctx
            .client
            .lookup_property_tag(&ctx.space_id, prop_key, tag_name)
            .await
        {
            Ok(tag) => return Ok(tag),
            Err(e) => {
                let retryable = match &e {
                    AnytypeError::NotFound { .. } => true,
                    AnytypeError::ApiError { code, .. } => *code == 500,
                    AnytypeError::Validation { message } => {
                        message.contains("unknown property key")
                    }
                    _ => false,
                };

                if retryable && attempt < max_attempts {
                    eprintln!(
                        "lookup tag '{tag_name}' for '{prop_key}' failed (attempt {attempt}), retrying: {e}"
                    );
                    sleep(Duration::from_millis(delay_ms)).await;
                    delay_ms = (delay_ms * 2).min(1500);
                    continue;
                }
                eprintln!("failed to lookup tag '{tag_name}' for '{prop_key}': {e}");
                return Err(e.into());
            }
        }
    }

    Err(TestError::Assertion {
        message: format!(
            "failed to lookup tag '{tag_name}' for '{prop_key}' after {max_attempts} attempts"
        ),
    })
}

pub async fn update_object_with_retry<F, Fut>(label: &str, mut f: F) -> TestResult<Object>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<Object, AnytypeError>>,
{
    let mut delay_ms = 200u64;
    let max_attempts = 5;

    for attempt in 1..=max_attempts {
        match f().await {
            Ok(obj) => return Ok(obj),
            Err(e) => {
                let retryable = match &e {
                    AnytypeError::ApiError { code, message, .. } => {
                        *code == 500
                            || (*code == 400
                                && (message.contains("invalid multi_select option")
                                    || message.contains("invalid select option")
                                    || message.contains("unknown property key")))
                    }
                    AnytypeError::Validation { message } => {
                        message.contains("invalid multi_select option")
                            || message.contains("invalid select option")
                            || message.contains("unknown property key")
                    }
                    _ => false,
                };

                if retryable && attempt < max_attempts {
                    eprintln!("{label} update failed (attempt {attempt}), retrying: {e}");
                    sleep(Duration::from_millis(delay_ms)).await;
                    delay_ms = (delay_ms * 2).min(1500);
                    continue;
                }
                eprintln!("failed to update {label}: {e}");
                return Err(e.into());
            }
        }
    }

    Err(TestError::Assertion {
        message: format!("failed to update {label} after {max_attempts} attempts"),
    })
}

// =============================================================================
// Property Setup Utilities
// =============================================================================

/// Creates required properties and a unique type for the test.
///
/// # Arguments
/// * `ctx` - Test context with client and space_id
///
/// # Returns
/// The created type key.
pub async fn ensure_properties_and_type(ctx: &TestContext) -> TestResult<String> {
    // Due Date/due_date Date
    match ctx
        .client
        .lookup_property_by_key(&ctx.space_id, "due_date")
        .await
    {
        Err(AnytypeError::NotFound { .. }) => {
            eprintln!("due_date not found in space {}, creating", &ctx.space_id);
            match ctx
                .client
                .new_property(&ctx.space_id, "Due Date", PropertyFormat::Date)
                .key("due_date")
                .create()
                .await
            {
                Ok(prop) => {
                    ctx.register_property(&prop.id);
                }
                Err(e) => {
                    if is_key_already_exists_error(&e, "property key", "due_date") {
                        let _prop = ctx
                            .client
                            .lookup_property_by_key(&ctx.space_id, "due_date")
                            .await?;
                    } else {
                        eprintln!("creating due_date: {e}");
                        return Err(e.into());
                    }
                }
            }
        }
        Err(e) => return Err(e.into()),
        Ok(_prop) => {
            // found
        }
    }

    // create a type with these properties
    let type_key = unique_type_key("my_page");
    eprintln!("creating type {type_key} in space {}", &ctx.space_id);
    match ctx
        .client
        .new_type(&ctx.space_id, "MyPage")
        .key(&type_key)
        .property("Priority", "priority", PropertyFormat::Number)
        .property("Done", "done", PropertyFormat::Checkbox)
        .property("Description", "description", PropertyFormat::Text)
        .property("Due Date", "due_date", PropertyFormat::Date)
        .property("Status", "status", PropertyFormat::Select)
        .create()
        .await
    {
        Ok(typ) => {
            ctx.register_type(&typ.id);
        }
        Err(e) => {
            error!("creating type {type_key}: {e:?}");
            return Err(e.into());
        }
    }

    Ok(type_key)
}

// =============================================================================
// Test Result Tracking (for multi-assertion tests)
// =============================================================================

/// Tracks multiple test results within a single test function
#[derive(Default)]
pub struct TestResultTracker {
    pub passed: Vec<String>,
    pub failed: Vec<(String, String)>,
}

impl TestResultTracker {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn pass(&mut self, name: &str) {
        println!("  [PASS] {}", name);
        self.passed.push(name.to_string());
    }

    pub fn fail(&mut self, name: &str, error: &str) {
        println!("  [FAIL] {}: {}", name, error);
        self.failed.push((name.to_string(), error.to_string()));
    }

    pub fn is_success(&self) -> bool {
        self.failed.is_empty()
    }

    pub fn summary(&self) -> String {
        format!(
            "Passed: {}, Failed: {}",
            self.passed.len(),
            self.failed.len()
        )
    }

    /// Assert that all tests passed, panicking with details if not
    pub fn assert_all_passed(&self) {
        if !self.is_success() {
            let failures: Vec<String> = self
                .failed
                .iter()
                .map(|(name, err)| format!("  - {}: {}", name, err))
                .collect();
            panic!(
                "Test failures:\n{}\n\n{}",
                failures.join("\n"),
                self.summary()
            );
        }
    }
}
