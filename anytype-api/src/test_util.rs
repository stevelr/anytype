//! Test utilities
//!
//! Helper functions used to test the `anytype` library.
//! These are not part of the supported api and are subject to change.
//!
#![doc(hidden)]

use std::{
    collections::{BTreeMap, BTreeSet},
    env::VarError,
    path::PathBuf,
    slice::Iter,
    sync::{Arc, atomic::AtomicUsize},
    time::{Duration, Instant},
};

use anytype_rpc::{
    anytype::rpc::{object::create_object_type, space::delete as space_delete},
    model::object_type::Layout,
};
use chrono::Utc;
use futures::FutureExt;
use parking_lot::Mutex;
use prost_types::{Struct, Value, value::Kind};
use snafu::prelude::*;
use tonic::Request;

#[allow(unused_imports)]
use crate::prelude::{AnytypeClient, AnytypeError, ClientConfig, VerifyConfig};
use crate::{
    filters::Filter,
    grpc_util::with_token_request,
    objects::{DataModel, ObjectLayout},
    spaces::Space,
    types::Type,
    verify::verify_semantic,
};

const SPACE_FIXTURE_SCAN_LIMIT: u32 = 1_000;
const SPACE_FIXTURE_VERIFY_TIMEOUT: Duration = Duration::from_secs(20);
const SPACE_FIXTURE_VERIFY_ATTEMPTS: usize = 50;

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
        Self::Api { source }
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
    /// - `ANYTYPE_TEST_URL` - API endpoint (default: <http://127.0.0.1:31012>)
    /// - `ANYTYPE_KEYSTORE` - Keystore specification (for example, `file:path=/path/to/keys.db`)
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
            cleanup: TestCleanup::default(),
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
        self.cleanup.add_object(&self.space_id, obj_id);
    }
    pub fn register_file(&self, file_id: &str) {
        self.cleanup.add_object(&self.space_id, file_id);
    }
    pub fn register_property(&self, prop_id: &str) {
        self.cleanup.add_property(&self.space_id, prop_id);
    }
    pub fn register_type(&self, type_id: &str) {
        self.cleanup.add_type(&self.space_id, type_id);
    }

    /// Creates and immediately registers a cleanup-safe collection-layout type fixture.
    ///
    /// The public REST type API intentionally accepts only its four document
    /// layouts. Tests that require a custom collection therefore use the
    /// narrower heart RPC here, while all production type builders retain the
    /// REST contract. The returned type is verified through the ordinary REST
    /// getter before this helper succeeds.
    pub async fn create_collection_type_fixture(
        &self,
        name: impl Into<String>,
    ) -> TestResult<Type> {
        let name = name.into();
        let plural_name = format!("{name}s");
        let limits = &self.client.get_config().limits;
        limits.validate_id(&self.space_id, "space_id")?;
        limits.validate_name(&name, "collection type name")?;
        limits.validate_name(&plural_name, "collection type plural name")?;

        let grpc = self.client.grpc_client().await?;
        let mut commands = grpc.client_commands();
        let request = create_object_type::Request {
            details: Some(collection_type_details(&name, &plural_name)),
            internal_flags: Vec::new(),
            space_id: self.space_id.clone(),
        };
        let request = with_token_request(Request::new(request), grpc.token())?;
        let response = commands
            .object_create_object_type(request)
            .await
            .map_err(collection_fixture_transport_error)?
            .into_inner();

        let type_id = response.object_id;
        limits.validate_id(&type_id, "created collection type id")?;
        // Register before response classification or any follow-up read can fail.
        self.register_type(&type_id);
        if !response.error.as_ref().is_some_and(|error| {
            error.code == create_object_type::response::error::Code::Null as i32
        }) {
            return Err(AnytypeError::Other {
                message: "gRPC collection type fixture creation was rejected".to_owned(),
            }
            .into());
        }

        let verify_config = VerifyConfig::default();
        let typ = verify_semantic(
            &verify_config,
            "collection type fixture",
            &type_id,
            || self.client.get_type(&self.space_id, &type_id).get_direct(),
            |typ| typ.id == type_id && typ.layout == ObjectLayout::Collection,
        )
        .await?;
        Ok(typ)
    }

    /// Creates a disposable space owned by this test context.
    ///
    /// The normal authenticated REST create path is used without its built-in
    /// follow-up verification. A complete bounded pre-create space snapshot
    /// establishes ownership: the returned ID must be valid, different from
    /// the context space, and absent from that snapshot before it is registered
    /// exactly once for teardown. Registration precedes every follow-up check.
    /// Teardown removes only IDs registered by this helper through Anytype's
    /// irreversible `SpaceDelete` RPC and then proves each ID is absent from a
    /// complete bounded REST space listing.
    ///
    /// This test-only lifecycle must not be used for pre-existing spaces.
    /// If an untrusted create response reuses a pre-existing ID, the helper
    /// refuses cleanup ownership even though that can leave an unknown newly
    /// created server-side resource behind.
    pub async fn create_space_fixture(&self, name: impl Into<String>) -> TestResult<Space> {
        let name = name.into();
        self.client
            .config
            .limits
            .validate_name(name.clone(), "test space")?;
        let preexisting_ids = complete_space_id_snapshot(&self.client).await?;
        let created = self.client.new_space(&name).no_verify().create().await?;
        validate_and_register_owned_space_fixture(
            &self.cleanup,
            &self.client.config.limits,
            &self.space_id,
            &preexisting_ids,
            &created.id,
        )?;

        let config = space_fixture_verify_config(&self.client);
        let expected_id = created.id.clone();
        let expected_name = name.clone();
        verify_semantic(
            &config,
            "Test space",
            &expected_id,
            || space_listing_evidence(&self.client, &expected_id, Some(&expected_name)),
            |evidence| evidence.complete && evidence.present && evidence.name_matches,
        )
        .await
        .map_err(TestError::from)?;
        Ok(created)
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
        self.cleanup.cleanup(&self.client).await
    }
}

fn collection_type_details(name: &str, plural_name: &str) -> Struct {
    Struct {
        fields: BTreeMap::from([
            ("name".to_owned(), string_value(name)),
            ("pluralName".to_owned(), string_value(plural_name)),
            (
                "recommendedLayout".to_owned(),
                Value {
                    kind: Some(Kind::NumberValue(Layout::Collection as i32 as f64)),
                },
            ),
        ]),
    }
}

fn string_value(value: &str) -> Value {
    Value {
        kind: Some(Kind::StringValue(value.to_owned())),
    }
}

fn collection_fixture_transport_error(_: tonic::Status) -> AnytypeError {
    AnytypeError::Other {
        message: "collection type fixture gRPC request failed".to_owned(),
    }
}

#[doc(hidden)]
pub async fn with_test_context<F, Fut, T>(test_fn: F) -> TestResult<T>
where
    F: FnOnce(Arc<TestContext>) -> Fut,
    Fut: std::future::Future<Output = TestResult<T>>,
{
    let ctx = Arc::new(TestContext::new().await?);
    let result = std::panic::AssertUnwindSafe(test_fn(Arc::clone(&ctx)))
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
pub async fn with_test_context_unit<F, Fut>(test_fn: F)
where
    F: FnOnce(Arc<TestContext>) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    let ctx = Arc::new(
        TestContext::new()
            .await
            .expect("Failed to create test context"),
    );

    let result = std::panic::AssertUnwindSafe(test_fn(Arc::clone(&ctx)))
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
        println!("  [PASS] {name}");
        self.passed.push(name.to_string());
    }

    pub fn fail(&mut self, name: &str, error: &str) {
        println!("  [FAIL] {name}: {error}");
        self.failed.push((name.to_string(), error.to_string()));
    }

    // iterate through failures
    pub fn failures(&self) -> Iter<'_, (String, String)> {
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

    let keystore_spec = match std::env::var("ANYTYPE_KEYSTORE") {
        Ok(spec) => spec,
        Err(_) => {
            let default_key_db = db_keystore::default_path()
                .map_err(|err| TestError::Config {
                    message: err.to_string(),
                })?
                .parent()
                .context(ConfigSnafu {
                    message: "invalid default path (check $XDG_STATE_HOME or $HOME)",
                })?
                .join("anytype-test-keys.db");
            format!("file:path={}", default_key_db.display())
        }
    };
    let config = ClientConfig {
        base_url: Some(base_url),
        app_name: app_name.to_string(),
        rate_limit_max_retries: 0, // Don't retry on rate limit
        verify: Some(VerifyConfig::default()),
        keystore: Some(keystore_spec),
        keystore_service: Some("anyr".into()), // TODO: temporary fix
        ..Default::default()
    };
    let client = AnytypeClient::with_config(config)?;

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
    space_fixtures: Mutex<BTreeSet<String>>,
    temp_paths: Mutex<Vec<PathBuf>>,
}

impl TestCleanup {
    pub fn is_empty(&self) -> bool {
        self.objects.lock().is_empty()
            && self.space_fixtures.lock().is_empty()
            && self.temp_paths.lock().is_empty()
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

    /// Remembers an exact space ID created by `TestContext::create_space_fixture`.
    fn add_space_fixture(&self, id: &str) -> bool {
        self.space_fixtures.lock().insert(id.into())
    }

    /// Deletes this file or folder after the test
    pub fn add_temp_path(&self, path: PathBuf) {
        self.temp_paths.lock().push(path);
    }

    /// Cleans up all remembered items.
    /// Child resources are deleted in reverse creation order and grouped as
    /// objects, properties, then types. The deduplicated disposable-space set
    /// is processed only after all child resources.
    pub async fn cleanup(&self, client: &AnytypeClient) -> TestResult<()> {
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

        // Delete disposable spaces only after their possible child resources.
        // SpaceDelete is irreversible, so this registry is private and can be
        // populated only by the create-and-register helper above.
        let space_fixtures = {
            let mut guard = self.space_fixtures.lock();
            std::mem::take(&mut *guard)
        };
        let mut space_cleanup_failed = false;
        for space_id in space_fixtures.into_iter().rev() {
            if delete_space_fixture(client, &space_id).await.is_err() {
                space_cleanup_failed = true;
            }
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

        if space_cleanup_failed {
            return Err(space_cleanup_error());
        }
        Ok(())
    }
}

async fn complete_space_id_snapshot(client: &AnytypeClient) -> TestResult<BTreeSet<String>> {
    let response = client
        .spaces()
        .limit(SPACE_FIXTURE_SCAN_LIMIT)
        .offset(0)
        .list()
        .await?
        .into_response();
    if !space_page_is_complete(&response) {
        return Err(space_fixture_ownership_error());
    }
    Ok(response.items.into_iter().map(|space| space.id).collect())
}

fn validate_and_register_owned_space_fixture(
    cleanup: &TestCleanup,
    limits: &crate::validation::ValidationLimits,
    current_space_id: &str,
    preexisting_ids: &BTreeSet<String>,
    returned_id: &str,
) -> TestResult<()> {
    limits.validate_id(returned_id, "test space")?;
    if returned_id == current_space_id || preexisting_ids.contains(returned_id) {
        // An untrusted duplicate response may leak a newly created server-side
        // space, but must never authorize deletion of pre-existing state.
        return Err(space_fixture_ownership_error());
    }
    if !cleanup.add_space_fixture(returned_id) {
        return Err(space_fixture_ownership_error());
    }
    Ok(())
}

async fn delete_space_fixture(client: &AnytypeClient, space_id: &str) -> TestResult<()> {
    client
        .config
        .limits
        .validate_id(space_id, "test space")
        .map_err(|_| space_cleanup_error())?;
    let grpc = client
        .grpc_client()
        .await
        .map_err(|_| space_cleanup_error())?;
    let mut commands = grpc.client_commands();
    let request = with_token_request(
        Request::new(space_delete::Request {
            space_id: space_id.to_owned(),
        }),
        grpc.token(),
    )
    .map_err(|_| space_cleanup_error())?;
    let response = commands
        .space_delete(request)
        .await
        .map_err(space_cleanup_transport_error)?
        .into_inner();
    if !space_delete_succeeded(response.error.as_ref().map(|error| error.code)) {
        return Err(space_cleanup_error());
    }

    let config = space_fixture_verify_config(client);
    verify_semantic(
        &config,
        "Deleted test space",
        space_id,
        || space_listing_evidence(client, space_id, None),
        |evidence| evidence.complete && !evidence.present,
    )
    .await
    .map(|_| ())
    .map_err(|_| space_cleanup_error())
}

#[derive(Debug)]
struct SpaceListingEvidence {
    complete: bool,
    present: bool,
    name_matches: bool,
}

async fn space_listing_evidence(
    client: &AnytypeClient,
    space_id: &str,
    expected_name: Option<&str>,
) -> Result<SpaceListingEvidence, AnytypeError> {
    let response = client
        .spaces()
        .limit(SPACE_FIXTURE_SCAN_LIMIT)
        .offset(0)
        .list()
        .await?
        .into_response();
    let complete = space_page_is_complete(&response);
    let matching_space = response.items.iter().find(|space| space.id == space_id);
    let present = matching_space.is_some();
    let name_matches = expected_name
        .is_none_or(|expected| matching_space.is_some_and(|space| space.name == expected));
    Ok(SpaceListingEvidence {
        complete,
        present,
        name_matches,
    })
}

fn space_page_is_complete(response: &crate::paged::PaginatedResponse<Space>) -> bool {
    response.pagination.offset == 0
        && !response.pagination.has_more
        && response.items.len() <= SPACE_FIXTURE_SCAN_LIMIT as usize
        && response.pagination.total == response.items.len()
}

fn space_delete_succeeded(error_code: Option<i32>) -> bool {
    error_code == Some(space_delete::response::error::Code::Null as i32)
}

fn space_fixture_verify_config(client: &AnytypeClient) -> VerifyConfig {
    let mut config = client.config.verify.clone().unwrap_or_default();
    config.timeout = config.timeout.max(SPACE_FIXTURE_VERIFY_TIMEOUT);
    config.max_attempts = config.max_attempts.max(SPACE_FIXTURE_VERIFY_ATTEMPTS);
    config
}

fn space_cleanup_error() -> TestError {
    TestError::Assertion {
        message: "registered test space cleanup failed".to_owned(),
    }
}

fn space_fixture_ownership_error() -> TestError {
    TestError::Assertion {
        message: "created test space ownership could not be established".to_owned(),
    }
}

fn space_cleanup_transport_error(_: tonic::Status) -> TestError {
    space_cleanup_error()
}

#[cfg(test)]
mod space_tests {
    use super::*;

    const CURRENT_SPACE_ID: &str = "bafyreiafl45wf5eaxiby44pxrkhia3y5jsyix3ov2jzqiftsxjotujqlh4";
    const STALE_SPACE_ID: &str = "bafyreifmrdlvfk5uolhph6xmh6geta47auzqjilcsxarpyxlkrbqxks64a";
    const OWNED_SPACE_ID: &str = "bafyreie6n5l5nkbjal37su54cha4coy7qzuhrnajluzv5qd5jvtsrxkequ";

    fn registered_spaces(cleanup: &TestCleanup) -> BTreeSet<String> {
        cleanup.space_fixtures.lock().clone()
    }

    #[test]
    fn malformed_create_response_never_enters_deletion_registry() {
        let cleanup = TestCleanup::default();
        let result = validate_and_register_owned_space_fixture(
            &cleanup,
            &crate::validation::ValidationLimits::default(),
            CURRENT_SPACE_ID,
            &BTreeSet::new(),
            "malformed-space-id",
        );
        assert!(result.is_err());
        assert!(registered_spaces(&cleanup).is_empty());
    }

    #[test]
    fn current_space_create_response_never_enters_deletion_registry() {
        let cleanup = TestCleanup::default();
        let result = validate_and_register_owned_space_fixture(
            &cleanup,
            &crate::validation::ValidationLimits::default(),
            CURRENT_SPACE_ID,
            &BTreeSet::new(),
            CURRENT_SPACE_ID,
        );
        assert!(result.is_err());
        assert!(registered_spaces(&cleanup).is_empty());
    }

    #[test]
    fn stale_create_response_never_enters_deletion_registry() {
        let cleanup = TestCleanup::default();
        let preexisting = BTreeSet::from([STALE_SPACE_ID.to_owned()]);
        let result = validate_and_register_owned_space_fixture(
            &cleanup,
            &crate::validation::ValidationLimits::default(),
            CURRENT_SPACE_ID,
            &preexisting,
            STALE_SPACE_ID,
        );
        assert!(result.is_err());
        assert!(registered_spaces(&cleanup).is_empty());
    }

    #[test]
    fn duplicate_create_response_is_registered_for_at_most_one_delete() {
        let cleanup = TestCleanup::default();
        let limits = crate::validation::ValidationLimits::default();
        assert!(
            validate_and_register_owned_space_fixture(
                &cleanup,
                &limits,
                CURRENT_SPACE_ID,
                &BTreeSet::new(),
                OWNED_SPACE_ID,
            )
            .is_ok()
        );
        assert!(
            validate_and_register_owned_space_fixture(
                &cleanup,
                &limits,
                CURRENT_SPACE_ID,
                &BTreeSet::new(),
                OWNED_SPACE_ID,
            )
            .is_err()
        );
        assert_eq!(
            registered_spaces(&cleanup),
            BTreeSet::from([OWNED_SPACE_ID.to_owned()])
        );
    }

    #[test]
    fn space_delete_requires_an_explicit_null_error_code() {
        assert!(space_delete_succeeded(Some(
            space_delete::response::error::Code::Null as i32
        )));
        assert!(!space_delete_succeeded(None));
        assert!(!space_delete_succeeded(Some(
            space_delete::response::error::Code::NoSuchSpace as i32
        )));
    }

    #[test]
    fn space_cleanup_transport_error_redacts_tonic_status() {
        const SECRET: &str = "space-cleanup-secret-sentinel";
        let error = space_cleanup_transport_error(tonic::Status::internal(SECRET));
        let rendered = error.to_string();
        assert_eq!(
            rendered,
            "Test assertion failed: registered test space cleanup failed"
        );
        assert!(!rendered.contains(SECRET));
    }
}

#[cfg(test)]
mod collection_tests {
    use super::*;

    #[test]
    fn collection_type_fixture_details_use_the_canonical_heart_layout() {
        let details = collection_type_details("MCP Collection", "MCP Collections");
        assert_eq!(
            details.fields["name"].kind,
            Some(Kind::StringValue("MCP Collection".to_owned()))
        );
        assert_eq!(
            details.fields["pluralName"].kind,
            Some(Kind::StringValue("MCP Collections".to_owned()))
        );
        assert_eq!(
            details.fields["recommendedLayout"].kind,
            Some(Kind::NumberValue(Layout::Collection as i32 as f64))
        );
        assert_eq!(details.fields.len(), 3);
    }

    #[test]
    fn collection_fixture_transport_error_redacts_tonic_status() {
        const SECRET: &str = "collection-fixture-secret-sentinel";
        let error = collection_fixture_transport_error(tonic::Status::internal(SECRET));
        let rendered = error.to_string();
        assert_eq!(rendered, "collection type fixture gRPC request failed");
        assert!(!rendered.contains(SECRET));
    }
}
