//! Test utilities
//!
//! Helper functions used to test the `anytype` library.
//! These are not part of the supported api and are subject to change.
//!
#![doc(hidden)]

use std::path::PathBuf;
use std::slice::Iter;
use std::sync::Arc;
use std::{env::VarError, sync::atomic::AtomicUsize, time::Instant};

use crate::filters::Filter;
use crate::objects::DataModel;
#[allow(unused_imports)]
use crate::prelude::{AnytypeClient, AnytypeError, ClientConfig, KeyStoreFile, VerifyConfig};

use chrono::Utc;
use futures::FutureExt;
use parking_lot::Mutex;
use snafu::prelude::*;

// =============================================================================
// TestError
// =============================================================================

#[doc(hidden)]
pub type TestResult<T> = std::result::Result<T, TestError>;

#[doc(hidden)]
#[derive(Debug, Snafu)]
pub enum TestError {
    #[snafu(display("API error: {source}"))]
    Api { source: AnytypeError },

    #[snafu(display("Missing environment variable"))]
    Env { source: VarError, name: String },

    #[snafu(display("Configuration error: {message}"))]
    Config { message: String },

    #[snafu(display("Test assertion failed: {message}"))]
    Assertion { message: String },
}

impl From<AnytypeError> for TestError {
    fn from(source: AnytypeError) -> Self {
        TestError::Api { source }
    }
}

// =============================================================================
// TestContext
// =============================================================================

/// Shared test context providing client and space configuration
#[doc(hidden)]
pub struct TestContext {
    pub client: AnytypeClient,
    pub space_id: String,
    start_time: Instant,
    api_call_count: AtomicUsize,
    cleanup: TestCleanup,
}

impl TestContext {
    /// Creates a new test context from environment variables
    ///
    /// Required environment variables:
    /// - `ANYTYPE_TEST_URL` - API endpoint (default: http://127.0.0.1:31012)
    /// - `ANYTYPE_TEST_KEY_FILE` - Path to file containing API key
    /// - `ANYTYPE_TEST_SPACE_ID` - Existing space ID for testing
    ///
    pub async fn new() -> TestResult<Self> {
        let client = test_client_named("anytype_test")?;
        let space_id = example_space_id(&client).await?;

        Ok(Self {
            client,
            space_id,
            start_time: Instant::now(),
            api_call_count: AtomicUsize::new(0),
            cleanup: Default::default(),
        })
    }

    pub fn increment_calls(&self, count: usize) {
        self.api_call_count
            .fetch_add(count, std::sync::atomic::Ordering::SeqCst);
    }

    pub fn call_count(&self) -> usize {
        self.api_call_count
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    pub fn elapsed_secs(&self) -> u64 {
        self.start_time.elapsed().as_secs()
    }

    pub fn register_object(&self, obj_id: &str) {
        self.cleanup.add_object(&self.space_id, obj_id)
    }
    pub fn register_property(&self, prop_id: &str) {
        self.cleanup.add_property(&self.space_id, prop_id);
    }
    pub fn register_type(&self, type_id: &str) {
        self.cleanup.add_type(&self.space_id, type_id);
    }

    pub fn temp_dir(&self, prefix: &str) -> TestResult<PathBuf> {
        let dir = std::env::temp_dir().join(format!("anytype_test_{prefix}_{}", unique_suffix()));
        std::fs::create_dir_all(&dir).map_err(|err| TestError::Config {
            message: format!("Failed to create temp dir {}: {err}", dir.display()),
        })?;
        self.cleanup.add_temp_path(dir.clone());
        Ok(dir)
    }

    /// Get a reference to the space ID
    pub fn space_id(&self) -> &str {
        &self.space_id
    }

    pub async fn cleanup(&self) -> TestResult<()> {
        self.cleanup.cleanup(&self.client).await;
        Ok(())
    }
}

#[doc(hidden)]
pub async fn with_test_context<F, Fut, T>(f: F) -> TestResult<T>
where
    F: FnOnce(Arc<TestContext>) -> Fut,
    Fut: std::future::Future<Output = TestResult<T>>,
{
    let ctx = Arc::new(TestContext::new().await?);
    let result = std::panic::AssertUnwindSafe(f(Arc::clone(&ctx)))
        .catch_unwind()
        .await;
    let cleanup_res = ctx.cleanup().await;

    match result {
        Ok(Ok(value)) => {
            cleanup_res?;
            Ok(value)
        }
        Ok(Err(err)) => {
            if let Err(cleanup_err) = cleanup_res {
                eprintln!("cleanup failed after test error: {cleanup_err:?}");
            }
            Err(err)
        }
        Err(panic) => {
            if let Err(cleanup_err) = cleanup_res {
                eprintln!("cleanup failed after panic: {cleanup_err:?}");
            }
            std::panic::resume_unwind(panic)
        }
    }
}

#[doc(hidden)]
pub async fn with_test_context_unit<F, Fut>(f: F)
where
    F: FnOnce(Arc<TestContext>) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    let ctx = Arc::new(
        TestContext::new()
            .await
            .expect("Failed to create test context"),
    );

    let result = std::panic::AssertUnwindSafe(f(Arc::clone(&ctx)))
        .catch_unwind()
        .await;
    if let Err(cleanup_err) = ctx.cleanup().await {
        eprintln!("cleanup failed after test: {cleanup_err:?}");
    }
    if let Err(panic) = result {
        std::panic::resume_unwind(panic)
    }
}

/// Get space id for tests and example programs
/// Search order:
///   1. environment variable "ANYTYPE_TEST_SPACE_ID"
///   2. environment variable "ANYTYPE_SPACE_ID"
///   3. the first space found with 'test' in the name
///
#[doc(hidden)]
#[allow(dead_code)]
pub async fn example_space_id(client: &AnytypeClient) -> Result<String, AnytypeError> {
    if let Ok(space_id) = std::env::var("ANYTYPE_TEST_SPACE_ID") {
        return Ok(space_id);
    }
    if let Ok(space_id) = std::env::var("ANYTYPE_SPACE_ID") {
        return Ok(space_id);
    }
    let spaces = client
        .spaces()
        .filter(Filter::text_contains("name", "test"))
        .limit(1)
        .list()
        .await?;
    if let Some(space) = spaces.iter().next() {
        return Ok(space.id.clone());
    }
    Err(AnytypeError::Other {
        message: "No spaces available for testing!".to_string(),
    })
}

// =============================================================================
// Test Result Tracking
// =============================================================================

#[doc(hidden)]
#[derive(Default)]
pub struct TestResults {
    passed: Vec<String>,
    failed: Vec<(String, String)>,
}

impl TestResults {
    pub fn pass(&mut self, name: &str) {
        println!("  [PASS] {}", name);
        self.passed.push(name.to_string());
    }

    pub fn fail(&mut self, name: &str, error: &str) {
        println!("  [FAIL] {}: {}", name, error);
        self.failed.push((name.to_string(), error.to_string()));
    }

    // iterate through failures
    pub fn failures<'a>(&'a self) -> Iter<'a, (String, String)> {
        self.failed.iter()
    }

    pub fn summary(&self) -> String {
        format!(
            "Passed: {}, Failed: {}",
            self.passed.len(),
            self.failed.len()
        )
    }

    pub fn is_success(&self) -> bool {
        self.failed.is_empty()
    }
}

// =============================================================================
// Functions
// =============================================================================

static UNIQUE_SUFFIX_COUNTER: AtomicUsize = AtomicUsize::new(0);

/// Returns a unique ASCII suffix for names/keys in tests.
#[doc(hidden)]
pub fn unique_suffix() -> String {
    // use atomic counter + timestamp, so different test runs are still unique,
    // and we don't have to worry about the system clock resolution.
    // Relaxed ordering is fine - the return values only need to be unique, not monotonic
    let counter = UNIQUE_SUFFIX_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    format!("{}_{}", Utc::now().timestamp_millis(), counter)
}

/// Creates a new test context with a custom app name
#[doc(hidden)]
pub fn test_client() -> TestResult<AnytypeClient> {
    test_client_named("anytype_test")
}

/// Creates a new test context with a custom app name
#[doc(hidden)]
pub fn test_client_named(app_name: &str) -> TestResult<AnytypeClient> {
    let base_url = std::env::var(crate::config::ANYTYPE_TEST_URL_ENV)
        .unwrap_or_else(|_| crate::config::ANYTYPE_TEST_URL.to_string());

    let api_key_path = std::env::var("ANYTYPE_TEST_KEY_FILE").context(EnvSnafu {
        name: "ANYTYPE_TEST_KEY_FILE",
    })?;
    ensure!(
        std::path::PathBuf::from(&api_key_path).is_file(),
        ConfigSnafu {
            message: format!(
                "Missing key file: {api_key_path}. Authenticate first to set the test api key"
            )
        }
    );

    let config = ClientConfig {
        base_url,
        app_name: app_name.to_string(),
        rate_limit_max_retries: 0, // Don't retry on rate limit
        verify: Some(VerifyConfig::default()),
        ..Default::default()
    };

    let client =
        AnytypeClient::with_config(config)?.set_key_store(KeyStoreFile::from_path(&api_key_path)?);
    client.load_key(false)?;

    Ok(client)
}

// =============================================================================
// TestCleanup
// =============================================================================

/// Keeps track of objects and files created during test run so tests can clean-up after themselves.
#[doc(hidden)]
#[derive(Default)]
pub struct TestCleanup {
    objects: Mutex<Vec<(String, String, DataModel)>>,
    temp_paths: Mutex<Vec<PathBuf>>,
}

impl TestCleanup {
    pub fn is_empty(&self) -> bool {
        self.objects.lock().is_empty()
    }

    /// Remembers this object for deletion after the test
    pub fn add_object(&self, space_id: &str, id: &str) {
        self.objects
            .lock()
            .push((space_id.into(), id.into(), DataModel::Object));
    }

    /// Remembers this property for deletion after the test
    pub fn add_property(&self, space_id: &str, id: &str) {
        self.objects
            .lock()
            .push((space_id.into(), id.into(), DataModel::Property));
    }

    /// Remembers this Type for deletion after the test
    pub fn add_type(&self, space_id: &str, id: &str) {
        self.objects
            .lock()
            .push((space_id.into(), id.into(), DataModel::Type));
    }

    /// Deletes this file or folder after the test
    pub fn add_temp_path(&self, path: PathBuf) {
        self.temp_paths.lock().push(path);
    }

    /// Cleans up all remembered item
    /// Delete in reverse order from creation order, so dependencies should be handled correctly.
    /// Also, deletes objects before types before properties
    pub async fn cleanup(&self, client: &AnytypeClient) {
        let mut objects = {
            let mut guard = self.objects.lock();
            std::mem::take(&mut *guard)
        };
        objects.reverse();

        // First delete objects
        for (space_id, id, _) in objects
            .iter()
            .filter(|(_, _, model)| *model == DataModel::Object)
        {
            let _ = client.object(space_id, id).delete().await;
        }

        // then properties and tags
        for (space_id, prop_id, _) in objects
            .iter()
            .filter(|(_, _, model)| *model == DataModel::Property)
        {
            let tags = client.tags(space_id, prop_id).list().await;
            if let Ok(tags) = tags {
                for tag in tags.collect_all().await.unwrap_or_default() {
                    //eprintln!("cleanup tag {}", &tag.id);
                    let _ = client.tag(space_id, prop_id, tag.id).delete().await;
                }
            }
            let _ = client.property(space_id, prop_id).delete().await;
        }

        // then types
        for (space_id, type_id, _) in objects
            .iter()
            .filter(|(_, _, model)| *model == DataModel::Type)
        {
            let _ = client.get_type(space_id, type_id).delete().await;
        }

        let mut temp_paths = {
            let mut guard = self.temp_paths.lock();
            std::mem::take(&mut *guard)
        };
        temp_paths.reverse();
        for path in temp_paths {
            if path.is_dir() {
                let _ = std::fs::remove_dir_all(&path);
            } else {
                let _ = std::fs::remove_file(&path);
            }
        }
    }
}
