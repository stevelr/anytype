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
    anytype::{
        event::message::Value as EventValue,
        rpc::{
            block_dataview::{
                filter::add as add_dataview_filter,
                relation::add as add_dataview_relation,
                view::{create as create_dataview_view, update as update_dataview_view},
            },
            object::{create_object_type, show as object_show},
            space::delete as space_delete,
            template::create_from_object as template_create_from_object,
        },
    },
    error::{AnytypeGrpcError, AuthError, ViewError},
    model::{
        block::{ContentValue, content::dataview::View as DataviewView},
        object_type::Layout,
    },
};
use chrono::{SecondsFormat, Utc};
use futures::FutureExt;
use parking_lot::Mutex;
use prost_types::{Struct, Value, value::Kind};
use serde::Deserialize;
use snafu::prelude::*;
use tonic::Request;

mod disposable;
pub use disposable::{
    DisposableChildEnvironment, DisposableRun, DisposableSkip, DisposableTestError,
    with_disposable_space_context,
};

#[allow(unused_imports)]
use crate::prelude::{AnytypeClient, AnytypeError, ClientConfig, VerifyConfig};
use crate::{
    filters::{Filter, Query, QueryWithFilters},
    grpc_util::with_token_request,
    http_client::GetPaged,
    objects::{Color, DataModel, Object, ObjectLayout},
    properties::{Property, PropertyFormat, SetProperty},
    spaces::{Space, SpaceModel},
    tags::Tag,
    types::{Type, TypeLayout},
    verify::verify_semantic,
    views::ViewLayout,
};

const COLLECTION_DATAVIEW_BLOCK_ID: &str = "dataview";
const COLLECTION_VIEW_FIXTURE_SCAN_LIMIT: u32 = 1_000;
const COLLECTION_VIEW_FILTER_RPC_TIMEOUT: Duration = Duration::from_secs(5);
const KANBAN_FIXTURE_PAGE_LIMIT: u32 = 2;
const KANBAN_FIXTURE_MAX_ITEMS: usize = 32;
const SPACE_FIXTURE_SCAN_LIMIT: u32 = 1_000;
const SPACE_FIXTURE_VERIFY_TIMEOUT: Duration = Duration::from_secs(20);
const SPACE_FIXTURE_VERIFY_ATTEMPTS: usize = 50;
const TEMPLATE_FIXTURE_LIMIT: u32 = 1_000;
const TEMPLATE_FIXTURE_GLOBAL_TEMPLATE_LIMIT: usize = 10_000;
const TEMPLATE_FIXTURE_MAX_SOURCES: usize = 16;
const TEMPLATE_FIXTURE_SCOPED_OBJECT_PAGE_LIMIT: u32 = TEMPLATE_FIXTURE_MAX_SOURCES as u32 + 1;
const TEMPLATE_FIXTURE_VERIFY_TIMEOUT: Duration = Duration::from_secs(20);
const TEMPLATE_FIXTURE_VERIFY_ATTEMPTS: usize = 50;
/// Maximum attempts for test-only setup mutations rejected with HTTP 429.
#[doc(hidden)]
pub const DEFINITIVE_RATE_LIMIT_MAX_ATTEMPTS: usize = 5;
const DEFINITIVE_RATE_LIMIT_DELAYS_MS: [u64; DEFINITIVE_RATE_LIMIT_MAX_ATTEMPTS - 1] =
    [200, 400, 800, 1_500];

/// Returns whether an error proves a setup mutation was rejected with HTTP 429.
#[doc(hidden)]
pub fn is_definitive_rate_limit_rejection(error: &AnytypeError) -> bool {
    matches!(error, AnytypeError::ApiError { code: 429, .. })
}

/// Returns the bounded delay after a failed setup-mutation attempt.
#[doc(hidden)]
pub fn definitive_rate_limit_retry_delay(failed_attempt: usize) -> Option<Duration> {
    failed_attempt
        .checked_sub(1)
        .and_then(|index| DEFINITIVE_RATE_LIMIT_DELAYS_MS.get(index))
        .copied()
        .map(Duration::from_millis)
}

/// Retries a test-only setup mutation only after a typed HTTP 429 rejection.
///
/// A 429 response proves that Anytype rejected the request before applying it.
/// Transport failures, timeouts, validation errors, and all other HTTP statuses
/// are returned immediately because replay could duplicate a mutation.
#[doc(hidden)]
pub async fn retry_definitive_rate_limit<T, F, Fut>(
    label: &str,
    mut operation: F,
) -> Result<T, AnytypeError>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, AnytypeError>>,
{
    for attempt in 1..=DEFINITIVE_RATE_LIMIT_MAX_ATTEMPTS {
        match operation().await {
            Ok(value) => return Ok(value),
            Err(error) => {
                let retry_delay = is_definitive_rate_limit_rejection(&error)
                    .then(|| definitive_rate_limit_retry_delay(attempt))
                    .flatten();
                if let Some(delay) = retry_delay {
                    eprintln!(
                        "{label} received definitive HTTP 429 on attempt {attempt}; retrying after {}ms",
                        delay.as_millis()
                    );
                    tokio::time::sleep(delay).await;
                    continue;
                }
                return Err(error);
            }
        }
    }

    unreachable!("the final attempt always returns its result")
}
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

    #[snafu(display(
        "test space creation may have committed; reconcile only from the exact create-intent name and UTC timestamp"
    ))]
    SpaceCreateIndeterminate,
}

/// Builds a typed pre-dispatch view authentication failure for downstream tests.
#[doc(hidden)]
pub fn view_authentication_error_fixture() -> AnytypeError {
    let source = "SECRET_VIEW_TOKEN\n"
        .parse::<tonic::metadata::MetadataValue<tonic::metadata::Ascii>>()
        .expect_err("a newline is invalid in an ASCII metadata value");

    AnytypeError::Grpc {
        source: AnytypeGrpcError::View {
            source: ViewError::Auth {
                source: AuthError::InvalidMetadata { source },
            },
        },
    }
}

impl From<AnytypeError> for TestError {
    fn from(source: AnytypeError) -> Self {
        Self::Api { source }
    }
}

// =============================================================================
// TestContext
// =============================================================================

type OwnedChildStopper = Box<dyn FnMut() -> TestResult<()> + Send>;
type OwnedChildStart = Arc<dyn Fn() -> TestResult<()> + Send + Sync>;

enum OwnedChildRegistryState {
    Open {
        spawn_attempts: usize,
        stoppers: Vec<OwnedChildStopper>,
    },
    Sealed,
}

struct OwnedChildRegistry {
    state: Mutex<OwnedChildRegistryState>,
    mark_running: Option<OwnedChildStart>,
}

/// Shared test context providing client and space configuration
#[doc(hidden)]
pub struct TestContext {
    pub client: AnytypeClient,
    pub space_id: String,
    start_time: Instant,
    api_call_count: AtomicUsize,
    cleanup: TestCleanup,
    disposable_child_environment: Option<DisposableChildEnvironment>,
    owned_children: OwnedChildRegistry,
}

/// Identity returned for a cleanup-owned collection view fixture.
#[doc(hidden)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollectionViewFixture {
    /// Server-assigned view identifier.
    pub id: String,
    /// Exact requested view name.
    pub name: String,
}

/// One cleanup-owned card in a representative Kanban test fixture.
#[doc(hidden)]
#[derive(Debug, Clone)]
pub struct KanbanItemFixture {
    /// Exact object created for the card.
    pub object: Object,
    /// Expected select-tag ID, or `None` for the ungrouped column.
    pub column_id: Option<String>,
}

/// Cleanup-owned representative Kanban layout for disposable live tests.
#[doc(hidden)]
#[derive(Debug, Clone)]
pub struct KanbanFixture {
    /// Custom basic type used by every card.
    pub item_type: Type,
    /// Cleanup-owned collection-layout type used by the board.
    pub collection_type: Type,
    /// Collection containing exactly [`items`](Self::items).
    pub collection: Object,
    /// Existing server view converted to Kanban layout.
    pub view: CollectionViewFixture,
    /// Select/status property used as the grouping relation.
    pub status_property: Property,
    /// Heart-internal relation key used by the Kanban dataview.
    pub status_relation_key: String,
    /// Exact cleanup-owned status options used by this fixture.
    pub columns: Vec<Tag>,
    /// Exact cleanup-owned cards and their expected columns.
    pub items: Vec<KanbanItemFixture>,
}

/// Cleanup-owned custom type, source objects, and templates created for tests.
#[doc(hidden)]
#[derive(Debug)]
pub struct TemplateFixtureSet {
    /// Custom type targeted by every template in this fixture set.
    pub type_: Type,
    /// Exact source objects converted into templates.
    pub sources: Vec<Object>,
    /// Exact templates returned by heart's template-from-object RPC.
    pub templates: Vec<Object>,
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

        Ok(Self::for_space(client, space_id))
    }

    pub(super) fn for_space(client: AnytypeClient, space_id: String) -> Self {
        Self::for_disposable_space(client, space_id, None, None)
    }

    pub(super) fn for_disposable_space(
        client: AnytypeClient,
        space_id: String,
        disposable_child_environment: Option<DisposableChildEnvironment>,
        mark_child_running: Option<OwnedChildStart>,
    ) -> Self {
        Self {
            client,
            space_id,
            start_time: Instant::now(),
            api_call_count: AtomicUsize::new(0),
            cleanup: TestCleanup::default(),
            disposable_child_environment,
            owned_children: OwnedChildRegistry {
                state: Mutex::new(OwnedChildRegistryState::Open {
                    spawn_attempts: 0,
                    stoppers: Vec::new(),
                }),
                mark_running: mark_child_running,
            },
        }
    }

    /// Returns the sanitized environment for a spawned test child.
    ///
    /// This is present only inside [`with_disposable_space_context`]. A child
    /// The environment clears ambient process state and carries only the
    /// approved endpoints, limits, selectors, and environment credentials.
    #[doc(hidden)]
    #[must_use]
    pub fn disposable_child_environment(&self) -> Option<&DisposableChildEnvironment> {
        self.disposable_child_environment.as_ref()
    }

    /// Atomically spawns and registers a child owned by this disposable test.
    ///
    /// `spawn` must return the owned value and its idempotent stop-and-wait
    /// operation together. The durable ledger is marked child-running before
    /// `spawn` is invoked, and the registry lock remains held until the stopper
    /// is installed. Once callback cleanup seals the registry, later calls are
    /// rejected before invoking `spawn`.
    #[doc(hidden)]
    pub fn spawn_owned_child<T, S, F>(&self, spawn: F) -> TestResult<T>
    where
        S: FnMut() -> TestResult<()> + Send + 'static,
        F: FnOnce() -> (T, S),
    {
        let mut registry = self.owned_children.state.lock();
        let OwnedChildRegistryState::Open {
            spawn_attempts,
            stoppers,
        } = &mut *registry
        else {
            return Err(child_registry_error());
        };
        let mark_running = self
            .owned_children
            .mark_running
            .as_ref()
            .ok_or_else(child_registry_error)?;
        mark_running()?;
        *spawn_attempts = spawn_attempts
            .checked_add(1)
            .ok_or_else(child_registry_error)?;
        let (owned, stopper) = spawn();
        stoppers.push(Box::new(stopper));
        Ok(owned)
    }

    pub(super) fn seal_and_stop_owned_children(&self) -> ChildStopReport {
        let (spawn_attempts, stoppers) = {
            let mut registry = self.owned_children.state.lock();
            match std::mem::replace(&mut *registry, OwnedChildRegistryState::Sealed) {
                OwnedChildRegistryState::Open {
                    spawn_attempts,
                    stoppers,
                } => (spawn_attempts, stoppers),
                OwnedChildRegistryState::Sealed => {
                    return ChildStopReport {
                        outcome: ChildOwnershipOutcome::Unproven,
                        errors: vec![child_registry_error()],
                        panics: Vec::new(),
                    };
                }
            }
        };
        run_owned_child_stoppers(spawn_attempts, stoppers)
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
    /// Registers a message belonging to a cleanup-owned chat.
    ///
    /// The chat must already be registered as an object in this test context.
    /// Registered messages are deleted and proved absent before their chat is
    /// archived during teardown.
    pub fn register_chat_message(&self, chat_id: &str, message_id: &str) -> TestResult<()> {
        if self
            .cleanup
            .add_chat_message(&self.space_id, chat_id, message_id)
        {
            Ok(())
        } else {
            Err(child_registry_error())
        }
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

    /// Creates and privately registers a cleanup-owned collection object.
    ///
    /// Only a collection-layout type already owned by this context is accepted.
    /// A complete bounded pre-create snapshot for that exact type prevents a
    /// stale create response from granting cleanup or mutation authority over a
    /// pre-existing object. The returned object is bound to the context space,
    /// type, active state, and collection layout before entering the private
    /// collection-fixture registry used by the second-view helper.
    pub async fn create_collection_fixture(
        &self,
        collection_type: &Type,
        name: impl Into<String>,
    ) -> TestResult<crate::objects::Object> {
        let name = name.into();
        self.client
            .config
            .limits
            .validate_name(&name, "collection fixture")?;
        if !self.cleanup.has_type(&self.space_id, &collection_type.id) {
            return Err(collection_fixture_ownership_error());
        }
        let exact_type = self
            .client
            .get_type(&self.space_id, &collection_type.id)
            .get_direct()
            .await
            .map_err(|_| collection_fixture_ownership_error())?;
        if exact_type.id != collection_type.id
            || exact_type.key != collection_type.key
            || exact_type.archived
            || exact_type.layout != ObjectLayout::Collection
        {
            return Err(collection_fixture_ownership_error());
        }
        let preexisting =
            complete_type_object_id_snapshot(&self.client, &self.space_id, &exact_type.id)
                .await
                .map_err(|_| collection_fixture_ownership_error())?;
        let created = retry_definitive_rate_limit("collection fixture", || async {
            self.client
                .new_object(&self.space_id, &exact_type.key)
                .name(&name)
                .no_verify()
                .create()
                .await
        })
        .await
        .map_err(|_| collection_fixture_ownership_error())?;
        self.client
            .config
            .limits
            .validate_id(&created.id, "created collection fixture")
            .map_err(|_| collection_fixture_ownership_error())?;
        if created.space_id != self.space_id
            || created.archived
            || created.layout != ObjectLayout::Collection
            || created.r#type.as_ref().map(|typ| typ.id.as_str()) != Some(exact_type.id.as_str())
            || preexisting.contains(&created.id)
            || !self
                .cleanup
                .claim_collection_fixture(&self.space_id, &created.id, &exact_type.id)
        {
            return Err(collection_fixture_ownership_error());
        }
        Ok(created)
    }

    /// Adds a second view to a cleanup-registered collection fixture.
    ///
    /// The public REST API does not expose view creation. This test-only helper
    /// therefore snapshots the collection's single default view through REST,
    /// resolves that exact view and its dataview block through `ObjectShow`,
    /// copies the complete view proto, and submits one authenticated
    /// `BlockDataviewViewCreate` RPC. Anytype assigns the final view ID. The
    /// response event and a finite REST read-after-write verification must both
    /// identify the same new ID and requested name before the helper succeeds.
    ///
    /// The collection must carry this context's private, type-bound creation
    /// provenance and must still have exactly its one server-created default
    /// view. Generic object cleanup registration does not grant this authority.
    /// Deleting that collection owns cleanup of the added view; no independent
    /// view deletion is attempted.
    pub async fn create_collection_view_fixture(
        &self,
        collection_id: &str,
        name: impl Into<String>,
    ) -> TestResult<CollectionViewFixture> {
        let name = name.into();
        let limits = &self.client.get_config().limits;
        limits.validate_id(collection_id, "collection fixture")?;
        limits.validate_name(&name, "collection view fixture")?;
        let expected_type_id = self
            .cleanup
            .collection_fixture_type_id(&self.space_id, collection_id)
            .ok_or_else(|| collection_view_fixture_code_error("ownership"))?;

        let collection = self
            .client
            .object(&self.space_id, collection_id)
            .get()
            .await
            .map_err(|_| collection_view_fixture_code_error("collection-get"))?;
        if !collection_matches_fixture_provenance(
            &collection,
            &self.space_id,
            collection_id,
            &expected_type_id,
        ) {
            return Err(collection_view_fixture_code_error("collection-identity"));
        }

        let existing =
            complete_collection_view_snapshot(&self.client, &self.space_id, collection_id)
                .await
                .map_err(|_| collection_view_fixture_code_error("rest-view-list"))?;
        if existing.len() != 1 || !collection_view_ids_are_unique(&existing) {
            return Err(collection_view_fixture_code_error("rest-view-snapshot"));
        }
        let default_id = existing[0].id.clone();

        let grpc = self
            .client
            .grpc_client()
            .await
            .map_err(|_| collection_view_fixture_code_error("grpc-client"))?;
        let mut commands = grpc.client_commands();
        let show_request = object_show::Request {
            object_id: collection_id.to_owned(),
            space_id: self.space_id.clone(),
            ..Default::default()
        };
        let show_request = with_token_request(Request::new(show_request), grpc.token())
            .map_err(|_| collection_view_fixture_error())?;
        let show_response = commands
            .object_show(show_request)
            .await
            .map_err(collection_view_fixture_transport_error)?
            .into_inner();
        if !object_show_succeeded(show_response.error.as_ref().map(|error| error.code)) {
            return Err(collection_view_fixture_code_error("object-show-response"));
        }
        let object_view = show_response
            .object_view
            .ok_or_else(|| collection_view_fixture_code_error("object-show-view"))?;
        let resolved = resolve_collection_dataview(
            &object_view.root_id,
            collection_id,
            &object_view.blocks,
            &existing,
            &default_id,
        )?;

        let request_id = format!("test-view-request-{}", unique_suffix());
        if !valid_collection_view_id(&request_id)
            || existing.iter().any(|view| view.id == request_id)
        {
            return Err(collection_view_fixture_code_error("request-id"));
        }
        let requested_view = clone_collection_view(&resolved.default_view, &request_id, &name);
        let create_request = create_dataview_view::Request {
            context_id: collection_id.to_owned(),
            block_id: resolved.block_id.clone(),
            view: Some(requested_view.clone()),
            source: resolved.source,
        };
        let create_request = with_token_request(Request::new(create_request), grpc.token())
            .map_err(|_| collection_view_fixture_error())?;
        let create_response = commands
            .block_dataview_view_create(create_request)
            .await
            .map_err(collection_view_fixture_transport_error)?
            .into_inner();
        let returned_view_id = validate_created_collection_view_identity(
            &create_response.view_id,
            &request_id,
            &existing,
        )?;
        if !self.cleanup.claim_collection_view_fixture(
            &self.space_id,
            collection_id,
            &returned_view_id,
        ) {
            return Err(collection_view_fixture_code_error("view-claim"));
        }
        let created_id = validate_created_collection_view(
            &create_response,
            &self.space_id,
            collection_id,
            &resolved.block_id,
            &request_id,
            &requested_view,
            &existing,
        )?;

        let mut expected = existing
            .iter()
            .cloned()
            .map(|view| (view.id.clone(), view))
            .collect::<BTreeMap<_, _>>();
        let mut created_rest_view = dataview_view_as_rest(&requested_view)?;
        created_rest_view.id.clone_from(&created_id);
        if expected
            .insert(created_id.clone(), created_rest_view)
            .is_some()
        {
            return Err(collection_view_fixture_error());
        }
        let verify_config = self.client.config.verify.clone().unwrap_or_default();
        verify_semantic(
            &verify_config,
            "collection view fixture",
            collection_id,
            || complete_collection_view_snapshot(&self.client, &self.space_id, collection_id),
            |views| collection_view_snapshot_matches(views, &expected),
        )
        .await
        .map_err(|_| collection_view_fixture_error())?;
        Ok(CollectionViewFixture {
            id: created_id,
            name,
        })
    }

    /// Adds one exact-name saved-view filter to an owned collection view.
    ///
    /// This test-only helper exists to prove that direct collection membership
    /// is independent from saved-view presentation. It accepts only a view and
    /// collection created and cleanup-owned by this context, requires both the
    /// REST and `ObjectShow` snapshots to be initially unfiltered, dispatches
    /// one authenticated `BlockDataviewFilterAdd`, and verifies the assigned
    /// filter identity and complete filter value through both evidence paths.
    /// Collection teardown owns removal of the saved filter.
    pub async fn add_collection_name_filter_fixture(
        &self,
        collection_id: &str,
        view_id: &str,
        exact_name: impl Into<String>,
    ) -> TestResult<String> {
        let exact_name = exact_name.into();
        self.client
            .config
            .limits
            .validate_name(&exact_name, "collection view filter value")?;
        if !self
            .cleanup
            .owns_collection_view_fixture(&self.space_id, collection_id, view_id)
        {
            return Err(collection_view_fixture_code_error("filter-ownership"));
        }

        let before =
            read_kanban_view_evidence(&self.client, &self.space_id, collection_id, view_id).await?;
        if !before.rest_filters_empty || !before.view.filters.is_empty() {
            return Err(collection_view_fixture_code_error("filter-preexisting"));
        }
        let requested = anytype_rpc::model::block::content::dataview::Filter {
            relation_key: "name".to_owned(),
            condition: anytype_rpc::model::block::content::dataview::filter::Condition::Equal
                as i32,
            value: Some(string_value(&exact_name)),
            format: anytype_rpc::model::RelationFormat::Shorttext as i32,
            ..Default::default()
        };
        let grpc = self
            .client
            .grpc_client()
            .await
            .map_err(|_| collection_view_fixture_code_error("filter-grpc"))?;
        let mut commands = grpc.client_commands();
        let mut request = with_token_request(
            Request::new(add_dataview_filter::Request {
                context_id: collection_id.to_owned(),
                block_id: before.block_id.clone(),
                view_id: view_id.to_owned(),
                filter: Some(requested.clone()),
            }),
            grpc.token(),
        )
        .map_err(|_| collection_view_fixture_code_error("filter-auth"))?;
        request.set_timeout(COLLECTION_VIEW_FILTER_RPC_TIMEOUT);
        let response = tokio::time::timeout(
            COLLECTION_VIEW_FILTER_RPC_TIMEOUT,
            commands.block_dataview_filter_add(request),
        )
        .await
        .map_err(|_| collection_view_fixture_code_error("filter-deadline"))?
        .map_err(|_| collection_view_fixture_code_error("filter-transport"))?
        .into_inner();
        if response.error.as_ref().map(|error| error.code) != Some(0)
            || !valid_collection_view_id(&response.filter_id)
        {
            return Err(collection_view_fixture_code_error("filter-response"));
        }
        let event = response
            .event
            .as_ref()
            .ok_or_else(|| collection_view_fixture_code_error("filter-event"))?;
        if event.context_id != collection_id
            || event.messages.is_empty()
            || event
                .messages
                .iter()
                .any(|message| message.space_id != self.space_id)
        {
            return Err(collection_view_fixture_code_error("filter-event-identity"));
        }

        let filter_id = response.filter_id;
        let verify_config = self.client.config.verify.clone().unwrap_or_default();
        verify_semantic(
            &verify_config,
            "collection view name filter fixture",
            collection_id,
            || async {
                let proto =
                    read_kanban_view_evidence(&self.client, &self.space_id, collection_id, view_id)
                        .await
                        .map_err(|_| collection_view_fixture_api_error())?;
                let rest =
                    complete_collection_view_snapshot(&self.client, &self.space_id, collection_id)
                        .await?;
                Ok((proto, rest))
            },
            |(proto, rest)| {
                collection_name_filter_matches(
                    proto,
                    rest,
                    view_id,
                    &filter_id,
                    &requested,
                    &exact_name,
                )
            },
        )
        .await
        .map_err(|_| collection_view_fixture_code_error("filter-reread"))?;
        Ok(filter_id)
    }

    /// Creates a representative cleanup-owned Kanban board on a real server.
    ///
    /// The board uses a custom basic card type, one Select property, two
    /// cleanup-owned options, a collection and a server-created view converted
    /// to Kanban layout. Three cards force the verifier across two REST pages;
    /// one starts in each named column and one starts ungrouped. Every returned
    /// resource is registered before any follow-up read. This is test
    /// infrastructure, not a product-level Kanban API.
    pub async fn create_kanban_fixture(
        &self,
        name: impl Into<String>,
    ) -> TestResult<KanbanFixture> {
        let name = name.into();
        self.client
            .config
            .limits
            .validate_name(&name, "kanban fixture")?;
        let suffix = unique_suffix();
        let status_key = format!("kanban_status_{suffix}");
        let type_key = format!("kanban_card_{suffix}");
        let preexisting_types = complete_type_inventory(&self.client, &self.space_id)
            .await
            .map_err(|_| kanban_fixture_code_error("type-precreate-snapshot"))?;
        let preexisting_properties =
            complete_kanban_property_snapshot(&self.client, &self.space_id).await?;
        let item_type = retry_definitive_rate_limit("kanban item type", || async {
            self.client
                .new_type(&self.space_id, format!("{name} Card"))
                .plural_name(format!("{name} Cards"))
                .key(&type_key)
                .property("Board status", &status_key, PropertyFormat::Select)
                .no_verify()
                .create()
                .await
        })
        .await?;
        self.client
            .config
            .limits
            .validate_id(&item_type.id, "kanban item type")
            .map_err(|_| kanban_fixture_code_error("item-type-id"))?;
        if preexisting_types.all_ids.contains(&item_type.id) {
            return Err(kanban_fixture_code_error("item-type-preexisting"));
        }
        self.register_type(&item_type.id);
        if item_type.archived
            || item_type.key != type_key
            || item_type.layout != ObjectLayout::Basic
        {
            return Err(kanban_fixture_code_error("item-type"));
        }
        let matching_properties = item_type
            .properties
            .iter()
            .filter(|property| property.key == status_key)
            .cloned()
            .collect::<Vec<_>>();
        let [status_property] = matching_properties.as_slice() else {
            return Err(kanban_fixture_code_error("status-property-count"));
        };
        self.client
            .config
            .limits
            .validate_id(&status_property.id, "kanban status property")
            .map_err(|_| kanban_fixture_code_error("status-property-id"))?;
        if preexisting_properties.contains(&status_property.id) {
            return Err(kanban_fixture_code_error("status-property-preexisting"));
        }
        self.register_property(&status_property.id);
        if status_property.format() != PropertyFormat::Select {
            return Err(kanban_fixture_code_error("status-property-format"));
        }
        let status_property = status_property.clone();
        let status_relation_key =
            read_kanban_relation_key(&self.client, &self.space_id, &status_property).await?;

        let mut columns = Vec::with_capacity(2);
        for (label, color) in [("Backlog", Color::Ice), ("Done", Color::Lime)] {
            let tag_name = format!("{label} {suffix}");
            let preexisting_tags =
                complete_kanban_tag_snapshot(&self.client, &self.space_id, &status_property.id)
                    .await?;
            let tag = retry_definitive_rate_limit("kanban status option", || async {
                self.client
                    .new_tag(&self.space_id, &status_property.id)
                    .name(&tag_name)
                    .color(color.clone())
                    .no_verify()
                    .create()
                    .await
            })
            .await?;
            self.client
                .config
                .limits
                .validate_id(&tag.id, "kanban status option")
                .map_err(|_| kanban_fixture_code_error("tag-id"))?;
            if preexisting_tags
                .iter()
                .any(|existing| existing.id == tag.id)
            {
                return Err(kanban_fixture_code_error("tag-preexisting"));
            }
            if !self
                .cleanup
                .claim_kanban_tag_fixture(&self.space_id, &status_property.id, &tag.id)
            {
                return Err(kanban_fixture_code_error("tag-claim"));
            }
            if tag.name != tag_name {
                return Err(kanban_fixture_code_error("tag-identity"));
            }
            columns.push(tag);
        }

        let collection_type = self
            .create_collection_type_fixture(format!("{name} Board"))
            .await?;
        let collection = self
            .create_collection_fixture(&collection_type, format!("{name} Board"))
            .await?;
        let view = self
            .create_collection_view_fixture(&collection.id, format!("{name} Kanban"))
            .await?;
        self.configure_kanban_view(
            &collection.id,
            &view.id,
            &status_property,
            &status_relation_key,
        )
        .await?;

        let preexisting_items =
            complete_type_object_id_snapshot(&self.client, &self.space_id, &item_type.id)
                .await
                .map_err(|_| kanban_fixture_code_error("item-precreate-snapshot"))?;

        let backlog_id = columns
            .first()
            .map(|column| column.id.clone())
            .ok_or_else(|| kanban_fixture_code_error("backlog-column"))?;
        let done_id = columns
            .get(1)
            .map(|column| column.id.clone())
            .ok_or_else(|| kanban_fixture_code_error("done-column"))?;
        let expected_columns = [Some(backlog_id), Some(done_id), None];
        let mut items = Vec::with_capacity(expected_columns.len());
        for (index, column_id) in expected_columns.into_iter().enumerate() {
            let item_name = format!("{name} Card {}", index + 1);
            let object = retry_definitive_rate_limit("kanban card", || async {
                let mut request = self
                    .client
                    .new_object(&self.space_id, &item_type.key)
                    .name(&item_name)
                    .no_verify();
                if let Some(column_id) = column_id.as_deref() {
                    request = request.set_select(&status_property.key, column_id);
                }
                request.create().await
            })
            .await?;
            self.client
                .config
                .limits
                .validate_id(&object.id, "kanban card")
                .map_err(|_| kanban_fixture_code_error("item-id"))?;
            if preexisting_items.contains(&object.id) || self.cleanup.is_registered_id(&object.id) {
                return Err(kanban_fixture_code_error("item-preexisting"));
            }
            self.register_object(&object.id);
            if object.space_id != self.space_id
                || object.archived
                || object.r#type.as_ref().map(|typ| typ.id.as_str()) != Some(item_type.id.as_str())
            {
                return Err(kanban_fixture_code_error("item-identity"));
            }
            items.push(KanbanItemFixture { object, column_id });
        }
        self.client
            .view_add_objects(
                &self.space_id,
                &collection.id,
                items.iter().map(|item| item.object.id.clone()),
            )
            .await
            .map_err(|_| kanban_fixture_code_error("collection-membership"))?;

        let fixture = KanbanFixture {
            item_type,
            collection_type,
            collection,
            view,
            status_property,
            status_relation_key,
            columns,
            items,
        };
        self.verify_kanban_fixture(&fixture).await?;
        Ok(fixture)
    }

    /// Verifies the complete representative Kanban fixture from independent reads.
    pub async fn verify_kanban_fixture(&self, fixture: &KanbanFixture) -> TestResult<()> {
        validate_kanban_fixture_registration(&self.cleanup, &self.space_id, fixture)?;

        let item_type = self
            .client
            .get_type(&self.space_id, &fixture.item_type.id)
            .get_direct()
            .await
            .map_err(|_| kanban_fixture_code_error("item-type-read"))?;
        if item_type.archived
            || item_type.id != fixture.item_type.id
            || item_type.key != fixture.item_type.key
            || item_type.layout != ObjectLayout::Basic
        {
            return Err(kanban_fixture_code_error("item-type-reread"));
        }
        let relation_matches = item_type
            .properties
            .iter()
            .filter(|property| property.id == fixture.status_property.id)
            .collect::<Vec<_>>();
        let [relation] = relation_matches.as_slice() else {
            return Err(kanban_fixture_code_error("missing-or-wrong-relation"));
        };
        if relation.key != fixture.status_property.key
            || relation.format() != PropertyFormat::Select
        {
            return Err(kanban_fixture_code_error("missing-or-wrong-relation"));
        }
        let property = self
            .client
            .property(&self.space_id, &fixture.status_property.id)
            .get_direct()
            .await
            .map_err(|_| kanban_fixture_code_error("status-property-read"))?;
        if property.id != fixture.status_property.id
            || property.key != fixture.status_property.key
            || property.format() != PropertyFormat::Select
        {
            return Err(kanban_fixture_code_error("status-property-reread"));
        }
        let tags =
            complete_kanban_tag_snapshot(&self.client, &self.space_id, &fixture.status_property.id)
                .await?;
        for expected in &fixture.columns {
            if !tags.iter().any(|actual| actual == expected) {
                return Err(kanban_fixture_code_error("deleted-or-changed-tag"));
            }
        }

        let evidence = read_kanban_view_evidence(
            &self.client,
            &self.space_id,
            &fixture.collection.id,
            &fixture.view.id,
        )
        .await?;
        let relation_key =
            read_kanban_relation_key(&self.client, &self.space_id, &fixture.status_property)
                .await?;
        if relation_key != fixture.status_relation_key {
            return Err(kanban_fixture_code_error("status-relation-key-changed"));
        }
        validate_kanban_view_evidence(
            &evidence,
            &fixture.status_property,
            &fixture.status_relation_key,
        )?;

        let listed = complete_kanban_item_snapshot(
            &self.client,
            &self.space_id,
            &fixture.collection.id,
            &fixture.view.id,
        )
        .await?;
        let expected_ids = fixture
            .items
            .iter()
            .map(|item| item.object.id.as_str())
            .collect::<BTreeSet<_>>();
        let actual_ids = listed
            .iter()
            .map(|item| item.id.as_str())
            .collect::<BTreeSet<_>>();
        if expected_ids != actual_ids || expected_ids.len() != fixture.items.len() {
            return Err(kanban_fixture_code_error("item-membership"));
        }
        for expected in &fixture.items {
            let actual = self
                .client
                .object(&self.space_id, &expected.object.id)
                .get()
                .await
                .map_err(|_| kanban_fixture_code_error("item-read"))?;
            let actual_column = actual
                .get_property_select(&fixture.status_property.key)
                .map(|tag| tag.id.as_str());
            if actual.archived
                || actual.space_id != self.space_id
                || actual.r#type.as_ref().map(|typ| typ.id.as_str())
                    != Some(fixture.item_type.id.as_str())
                || actual_column != expected.column_id.as_deref()
            {
                return Err(kanban_fixture_code_error("item-column"));
            }
        }
        Ok(())
    }

    /// Moves one cleanup-owned card by an ordinary Select property update.
    pub async fn move_kanban_item_fixture(
        &self,
        fixture: &mut KanbanFixture,
        item_id: &str,
        column_id: &str,
    ) -> TestResult<()> {
        self.verify_kanban_fixture(fixture).await?;
        if !fixture.columns.iter().any(|column| {
            column.id == column_id
                && self.cleanup.owns_kanban_tag_fixture(
                    &self.space_id,
                    &fixture.status_property.id,
                    column_id,
                )
        }) {
            return Err(kanban_fixture_code_error("move-column"));
        }
        let item_index = fixture
            .items
            .iter()
            .position(|item| item.object.id == item_id)
            .ok_or_else(|| kanban_fixture_code_error("move-item"))?;
        self.client
            .update_object(&self.space_id, item_id)
            .set_select(&fixture.status_property.key, column_id)
            .no_verify()
            .update()
            .await
            .map_err(|_| kanban_fixture_code_error("move-update"))?;
        let actual = self
            .client
            .object(&self.space_id, item_id)
            .get()
            .await
            .map_err(|_| kanban_fixture_code_error("move-reread"))?;
        if actual
            .get_property_select(&fixture.status_property.key)
            .map(|tag| tag.id.as_str())
            != Some(column_id)
        {
            return Err(kanban_fixture_code_error("move-not-observed"));
        }
        let expected = fixture
            .items
            .get_mut(item_index)
            .ok_or_else(|| kanban_fixture_code_error("move-index"))?;
        expected.object = actual;
        expected.column_id = Some(column_id.to_owned());
        self.verify_kanban_fixture(fixture).await
    }

    async fn configure_kanban_view(
        &self,
        collection_id: &str,
        view_id: &str,
        status_property: &Property,
        status_relation_key: &str,
    ) -> TestResult<()> {
        if status_property.format() != PropertyFormat::Select
            || !self
                .cleanup
                .owns_collection_view_fixture(&self.space_id, collection_id, view_id)
        {
            return Err(kanban_fixture_code_error("configure-ownership"));
        }
        let mut evidence =
            read_kanban_view_evidence(&self.client, &self.space_id, collection_id, view_id).await?;
        if !evidence.rest_filters_empty || !evidence.view.filters.is_empty() {
            return Err(kanban_fixture_code_error("configure-filtered-view"));
        }
        let grpc = self
            .client
            .grpc_client()
            .await
            .map_err(|_| kanban_fixture_code_error("configure-grpc"))?;
        let mut commands = grpc.client_commands();
        if !evidence.relation_links.contains(&(
            status_relation_key.to_owned(),
            anytype_rpc::model::RelationFormat::Status as i32,
        )) {
            let request = add_dataview_relation::Request {
                context_id: collection_id.to_owned(),
                block_id: evidence.block_id.clone(),
                relation_keys: vec![status_relation_key.to_owned()],
            };
            let response = commands
                .block_dataview_relation_add(
                    with_token_request(Request::new(request), grpc.token())
                        .map_err(|_| kanban_fixture_code_error("relation-auth"))?,
                )
                .await
                .map_err(|_| kanban_fixture_code_error("relation-transport"))?
                .into_inner();
            if response.error.as_ref().map(|error| error.code) != Some(0) {
                return Err(kanban_fixture_code_error("relation-response"));
            }
            evidence =
                read_kanban_view_evidence(&self.client, &self.space_id, collection_id, view_id)
                    .await?;
        }
        if !evidence.relation_links.contains(&(
            status_relation_key.to_owned(),
            anytype_rpc::model::RelationFormat::Status as i32,
        )) {
            return Err(kanban_fixture_code_error("relation-reread"));
        }
        let mut requested_view = evidence.view;
        requested_view.r#type =
            anytype_rpc::model::block::content::dataview::view::Type::Kanban as i32;
        requested_view.group_relation_key = status_relation_key.to_owned();
        let request = update_dataview_view::Request {
            context_id: collection_id.to_owned(),
            block_id: evidence.block_id.clone(),
            view_id: view_id.to_owned(),
            view: Some(requested_view.clone()),
        };
        let response = commands
            .block_dataview_view_update(
                with_token_request(Request::new(request), grpc.token())
                    .map_err(|_| kanban_fixture_code_error("view-auth"))?,
            )
            .await
            .map_err(|_| kanban_fixture_code_error("view-transport"))?
            .into_inner();
        validate_updated_kanban_view(
            &response,
            &self.space_id,
            collection_id,
            &evidence.block_id,
            view_id,
            &requested_view,
        )?;
        let verified =
            read_kanban_view_evidence(&self.client, &self.space_id, collection_id, view_id).await?;
        validate_kanban_view_evidence(&verified, status_property, status_relation_key)
    }

    /// Creates a disposable space owned by this test context.
    ///
    /// The normal authenticated REST create path is used without its built-in
    /// follow-up verification. A complete bounded pre-create space inventory
    /// validates pagination, every ID/name, and uniqueness. The returned space
    /// must have the exact requested name, regular-space model, and a valid ID
    /// that differs from the context space and was absent from that inventory.
    /// Only then is its exact ID/name pair registered once for teardown, before
    /// follow-up verification.
    ///
    /// Teardown revalidates that exact ID/name/model in another strict complete
    /// inventory before Anytype's irreversible `SpaceDelete` RPC and proves the
    /// ID absent from the same evidence after every acknowledged or uncertain
    /// delete response.
    ///
    /// This test-only lifecycle must not be used for pre-existing spaces.
    /// The supplied name is recorded immediately before POST and therefore must
    /// be a generated non-secret fixture name.
    /// If an untrusted create response reuses a pre-existing ID, the helper
    /// refuses cleanup ownership even though that can leave an unknown newly
    /// created server-side resource behind.
    pub async fn create_space_fixture(&self, name: impl Into<String>) -> TestResult<Space> {
        let name = name.into();
        validate_space_fixture_name(&self.client.config.limits, &name, "test space")?;
        let preexisting = complete_space_inventory(&self.client).await?;
        let created =
            execute_space_create_after_intent(&name, record_space_create_intent, || async {
                retry_definitive_rate_limit("space fixture", || async {
                    self.client.new_space(&name).no_verify().create().await
                })
                .await
            })
            .await
            .map_err(classify_space_create_error)?;
        validate_and_register_owned_space_fixture(
            &self.cleanup,
            &self.client.config.limits,
            &self.space_id,
            &preexisting,
            &name,
            &created,
        )
        .map_err(|_| TestError::SpaceCreateIndeterminate)?;

        let config = space_fixture_verify_config(&self.client);
        let expected_id = created.id.clone();
        let expected_name = name.clone();
        verify_semantic(
            &config,
            "Test space",
            &expected_id,
            || space_listing_evidence(&self.client, &expected_id, Some(&expected_name)),
            |evidence| evidence.present && evidence.name_matches && evidence.object_matches,
        )
        .await
        .map_err(TestError::from)?;
        Ok(created)
    }

    /// Prepares cleanup ownership evidence for a space created through another
    /// reviewed client surface.
    ///
    /// Call [`Self::claim_prepared_space_fixture`] synchronously with the exact
    /// create response immediately after that surface returns. Preparation
    /// records the generated non-secret name before dispatch and captures a
    /// complete bounded inventory, so a forged or pre-existing identity can
    /// never become deletion-authorized.
    #[doc(hidden)]
    pub async fn prepare_space_fixture_claim(
        &self,
        name: impl Into<String>,
    ) -> TestResult<PreparedSpaceFixtureClaim> {
        let expected_name = name.into();
        validate_space_fixture_name(&self.client.config.limits, &expected_name, "test space")?;
        let preexisting = complete_space_inventory(&self.client).await?;
        record_space_create_intent(&expected_name);
        Ok(PreparedSpaceFixtureClaim {
            expected_name,
            preexisting,
        })
    }

    /// Claims a just-returned external space response for guarded teardown.
    ///
    /// The claim is rejected unless the identity is a new regular space with
    /// the exact prepared name. Successful registration occurs before this
    /// method returns.
    #[doc(hidden)]
    pub fn claim_prepared_space_fixture(
        &self,
        claim: &PreparedSpaceFixtureClaim,
        returned: &Space,
    ) -> TestResult<()> {
        validate_and_register_owned_space_fixture(
            &self.cleanup,
            &self.client.config.limits,
            &self.space_id,
            &claim.preexisting,
            &claim.expected_name,
            returned,
        )
    }

    /// Creates a custom type and cleanup-owned templates from new source objects.
    ///
    /// The custom type and every source object use the authenticated REST API
    /// without built-in verification. Complete bounded pre-create snapshots of
    /// every type and every active and archived object belonging to the newly
    /// cleanup-owned type prove each returned type/source ID was not
    /// pre-existing before cleanup registration. Scoping source evidence to
    /// the freshly created type keeps unrelated long-lived space archives out
    /// of the ownership decision without weakening it. Each source is converted
    /// with exactly one authenticated
    /// `TemplateCreateFromObject` RPC. The returned template ID must be new and
    /// absent from the complete pre-RPC template inventory for the exact owned
    /// type, distinct from the space/type/source IDs, and returned from the
    /// exact owned source request. It is registered before response
    /// classification or fallible follow-up evidence so an applied mutation
    /// cannot leak from teardown.
    ///
    /// Creation succeeds only after a finite complete global-owner scan, a
    /// complete type-scoped list, and an exact template GET agree on every
    /// returned template ID.
    pub async fn create_template_fixtures<I, S>(
        &self,
        type_name: impl Into<String>,
        source_names: I,
    ) -> TestResult<TemplateFixtureSet>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let type_name = type_name.into();
        let source_names = source_names
            .into_iter()
            .map(Into::into)
            .collect::<Vec<String>>();
        if source_names.is_empty() || source_names.len() > TEMPLATE_FIXTURE_MAX_SOURCES {
            return Err(template_fixture_error());
        }
        let limits = &self.client.get_config().limits;
        limits.validate_id(&self.space_id, "space_id")?;
        limits.validate_name(&type_name, "template fixture type")?;
        for source_name in &source_names {
            limits.validate_name(source_name, "template fixture source")?;
        }

        let type_snapshot = complete_template_type_snapshot(&self.client, &self.space_id)
            .await
            .map_err(|error| classify_template_fixture_evidence(error, "type-snapshot"))?;
        let type_key = format!("template_fixture_{}", unique_suffix());
        let created_type = retry_definitive_rate_limit("template fixture type", || async {
            self.client
                .new_type(&self.space_id, &type_name)
                .key(&type_key)
                .plural_name(format!("{type_name}s"))
                .layout(TypeLayout::Basic)
                .no_verify()
                .create()
                .await
        })
        .await?;
        limits.validate_id(&created_type.id, "template fixture type")?;
        authorize_template_resource(
            &self.cleanup,
            &type_snapshot,
            &[self.space_id.as_str()],
            TemplateFixtureResource::Type {
                space_id: self.space_id.clone(),
                type_id: created_type.id.clone(),
                type_key: type_key.clone(),
            },
        )?;

        let verify_config = template_fixture_verify_config();
        let expected_type_id = created_type.id.clone();
        let expected_type_key = type_key.clone();
        let verified_type = verify_semantic(
            &verify_config,
            "template fixture type",
            &expected_type_id,
            || {
                self.client
                    .get_type(&self.space_id, &expected_type_id)
                    .get_direct()
            },
            |typ| {
                typ.id == expected_type_id
                    && typ.key == expected_type_key
                    && !typ.archived
                    && typ.layout == ObjectLayout::Basic
            },
        )
        .await?;

        let mut sources = Vec::with_capacity(source_names.len());
        let mut templates = Vec::with_capacity(source_names.len());

        for source_name in source_names {
            let source_snapshot =
                complete_template_source_snapshot(&self.client, &self.space_id, &verified_type.id)
                    .await
                    .map_err(|error| {
                        classify_template_fixture_evidence(error, "owned-type-source-snapshot")
                    })?;
            let source = retry_definitive_rate_limit("template fixture source", || async {
                self.client
                    .new_object(&self.space_id, &verified_type.key)
                    .name(&source_name)
                    .no_verify()
                    .create()
                    .await
            })
            .await?;
            limits.validate_id(&source.id, "template fixture source")?;
            authorize_template_source(
                &self.cleanup,
                &source_snapshot,
                &self.space_id,
                &verified_type.id,
                &source,
            )?;

            let expected_source_id = source.id.clone();
            let expected_source_type = verified_type.id.clone();
            let source = verify_semantic(
                &verify_config,
                "template fixture source",
                &expected_source_id,
                || {
                    self.client
                        .object(&self.space_id, &expected_source_id)
                        .get()
                },
                |object| {
                    object.id == expected_source_id
                        && object.space_id == self.space_id
                        && !object.archived
                        && object
                            .r#type
                            .as_ref()
                            .is_some_and(|typ| typ.id == expected_source_type)
                },
            )
            .await?;

            let template_snapshot =
                complete_template_id_snapshot(&self.client, &self.space_id, &verified_type.id)
                    .await
                    .map_err(|error| {
                        classify_template_fixture_evidence(error, "owned-type-template-snapshot")
                    })?;
            let grpc = self.client.grpc_client().await?;
            let mut commands = grpc.client_commands();
            let request = with_token_request(
                Request::new(template_create_from_object::Request {
                    context_id: source.id.clone(),
                }),
                grpc.token(),
            )?;
            let response = commands
                .template_create_from_object(request)
                .await
                .map_err(template_fixture_transport_error)?
                .into_inner();

            limits.validate_id(&response.id, "template fixture")?;
            authorize_owned_source_template(
                &self.cleanup,
                &template_snapshot,
                &self.space_id,
                &verified_type.id,
                &source.id,
                &response.id,
            )?;
            if !template_fixture_response_succeeded(response.error.as_ref()) {
                return Err(template_fixture_response_error(response.error.as_ref()));
            }

            let template = verify_template_fixture(
                &self.client,
                &verify_config,
                &self.space_id,
                &verified_type.id,
                &response.id,
            )
            .await
            .map_err(|error| classify_template_fixture_evidence(error, "template-verification"))?;
            sources.push(source);
            templates.push(template);
        }

        Ok(TemplateFixtureSet {
            type_: verified_type,
            sources,
            templates,
        })
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

struct ResolvedCollectionDataview {
    block_id: String,
    default_view: DataviewView,
    source: Vec<String>,
}

fn collection_matches_fixture_provenance(
    collection: &Object,
    space_id: &str,
    collection_id: &str,
    type_id: &str,
) -> bool {
    collection.id == collection_id
        && collection.space_id == space_id
        && !collection.archived
        && collection.layout == ObjectLayout::Collection
        && collection.r#type.as_ref().map(|typ| typ.id.as_str()) == Some(type_id)
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct RestCollectionView {
    id: String,
    name: String,
    layout: ViewLayout,
    #[serde(default, deserialize_with = "deserialize_vec_or_null")]
    filters: Vec<RestCollectionViewFilter>,
    #[serde(default, deserialize_with = "deserialize_vec_or_null")]
    sorts: Vec<RestCollectionViewSort>,
}

fn deserialize_vec_or_null<'de, D, T>(deserializer: D) -> Result<Vec<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Ok(Option::<Vec<T>>::deserialize(deserializer)?.unwrap_or_default())
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct RestCollectionViewFilter {
    id: String,
    property_key: String,
    format: crate::properties::PropertyFormat,
    condition: crate::filters::Condition,
    value: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct RestCollectionViewSort {
    id: String,
    property_key: String,
    format: crate::properties::PropertyFormat,
    sort_type: RestCollectionViewSortType,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum RestCollectionViewSortType {
    Asc,
    Desc,
    Custom,
}

#[derive(Debug)]
struct KanbanViewEvidence {
    block_id: String,
    view: DataviewView,
    relation_links: Vec<(String, i32)>,
    rest_layout: ViewLayout,
    rest_filters_empty: bool,
}

async fn read_kanban_relation_key(
    client: &AnytypeClient,
    space_id: &str,
    property: &Property,
) -> TestResult<String> {
    if property.format() != PropertyFormat::Select {
        return Err(kanban_fixture_code_error("relation-key-format"));
    }
    let grpc = client
        .grpc_client()
        .await
        .map_err(|_| kanban_fixture_code_error("relation-key-grpc-client"))?;
    let mut commands = grpc.client_commands();
    let response = commands
        .object_show(
            with_token_request(
                Request::new(object_show::Request {
                    object_id: property.id.clone(),
                    space_id: space_id.to_owned(),
                    ..Default::default()
                }),
                grpc.token(),
            )
            .map_err(|_| kanban_fixture_code_error("relation-key-auth"))?,
        )
        .await
        .map_err(|_| kanban_fixture_code_error("relation-key-transport"))?
        .into_inner();
    if !object_show_succeeded(response.error.as_ref().map(|error| error.code)) {
        return Err(kanban_fixture_code_error("relation-key-response"));
    }
    let object_view = response
        .object_view
        .ok_or_else(|| kanban_fixture_code_error("relation-key-view"))?;
    if object_view.root_id != property.id {
        return Err(kanban_fixture_code_error("relation-key-root"));
    }
    let mut resolved = None;
    for details_set in object_view
        .details
        .iter()
        .filter(|details| details.id == property.id)
    {
        let details = details_set
            .details
            .as_ref()
            .ok_or_else(|| kanban_fixture_code_error("relation-key-details"))?;
        let relation_key = match details
            .fields
            .get("relationKey")
            .and_then(|value| value.kind.as_ref())
        {
            Some(Kind::StringValue(value)) if !value.is_empty() => value,
            _ => return Err(kanban_fixture_code_error("relation-key-value")),
        };
        let unique_key = details
            .fields
            .get("uniqueKey")
            .and_then(|value| value.kind.as_ref());
        let relation_format = details
            .fields
            .get("relationFormat")
            .and_then(|value| value.kind.as_ref());
        if unique_key != Some(&Kind::StringValue(format!("rel-{relation_key}")))
            || relation_format
                != Some(&Kind::NumberValue(
                    anytype_rpc::model::RelationFormat::Status as i32 as f64,
                ))
            || resolved
                .as_ref()
                .is_some_and(|existing| existing != relation_key)
        {
            return Err(kanban_fixture_code_error("relation-key-identity"));
        }
        resolved = Some(relation_key.clone());
    }
    resolved.ok_or_else(|| kanban_fixture_code_error("relation-key-details-count"))
}

async fn complete_collection_view_snapshot(
    client: &AnytypeClient,
    space_id: &str,
    collection_id: &str,
) -> Result<Vec<RestCollectionView>, AnytypeError> {
    let query = Query::default()
        .limit(COLLECTION_VIEW_FIXTURE_SCAN_LIMIT)
        .offset(0);
    let response = client
        .client
        .get_request_paged(
            &format!("/v1/spaces/{space_id}/lists/{collection_id}/views"),
            QueryWithFilters::from(query),
        )
        .await?
        .into_response();
    if response.pagination.offset != 0
        || response.pagination.has_more
        || response.pagination.total != response.items.len()
        || response.items.len() > COLLECTION_VIEW_FIXTURE_SCAN_LIMIT as usize
    {
        return Err(collection_view_fixture_api_error());
    }
    Ok(response.items)
}

async fn read_kanban_view_evidence(
    client: &AnytypeClient,
    space_id: &str,
    collection_id: &str,
    view_id: &str,
) -> TestResult<KanbanViewEvidence> {
    let rest_views = complete_collection_view_snapshot(client, space_id, collection_id)
        .await
        .map_err(|_| kanban_fixture_code_error("view-rest-read"))?;
    let rest_matches = rest_views
        .iter()
        .filter(|view| view.id == view_id)
        .collect::<Vec<_>>();
    let [rest_view] = rest_matches.as_slice() else {
        return Err(kanban_fixture_code_error("view-rest-identity"));
    };

    let grpc = client
        .grpc_client()
        .await
        .map_err(|_| kanban_fixture_code_error("view-grpc-client"))?;
    let mut commands = grpc.client_commands();
    let request = object_show::Request {
        object_id: collection_id.to_owned(),
        space_id: space_id.to_owned(),
        ..Default::default()
    };
    let response = commands
        .object_show(
            with_token_request(Request::new(request), grpc.token())
                .map_err(|_| kanban_fixture_code_error("view-show-auth"))?,
        )
        .await
        .map_err(|_| kanban_fixture_code_error("view-show-transport"))?
        .into_inner();
    if !object_show_succeeded(response.error.as_ref().map(|error| error.code)) {
        return Err(kanban_fixture_code_error("view-show-response"));
    }
    let object_view = response
        .object_view
        .ok_or_else(|| kanban_fixture_code_error("view-show-missing"))?;
    if object_view.root_id != collection_id {
        return Err(kanban_fixture_code_error("view-show-root"));
    }
    let dataviews = object_view
        .blocks
        .iter()
        .filter_map(|block| match block.content_value.as_ref() {
            Some(ContentValue::Dataview(dataview)) => Some((block.id.as_str(), dataview)),
            _ => None,
        })
        .collect::<Vec<_>>();
    let [(block_id, dataview)] = dataviews.as_slice() else {
        return Err(kanban_fixture_code_error("view-dataview-count"));
    };
    if *block_id != COLLECTION_DATAVIEW_BLOCK_ID || !dataview.is_collection {
        return Err(kanban_fixture_code_error("view-dataview-identity"));
    }
    let views = dataview
        .views
        .iter()
        .filter(|view| view.id == view_id)
        .cloned()
        .collect::<Vec<_>>();
    let [view] = views.as_slice() else {
        return Err(kanban_fixture_code_error("view-proto-identity"));
    };
    let relation_links = dataview
        .relation_links
        .iter()
        .map(|relation| (relation.key.clone(), relation.format))
        .collect::<Vec<_>>();
    let relation_keys = relation_links
        .iter()
        .map(|(key, _)| key.as_str())
        .collect::<BTreeSet<_>>();
    if relation_keys.len() != dataview.relation_links.len() {
        return Err(kanban_fixture_code_error("view-relation-duplicates"));
    }
    Ok(KanbanViewEvidence {
        block_id: (*block_id).to_owned(),
        view: view.clone(),
        relation_links,
        rest_layout: rest_view.layout.clone(),
        rest_filters_empty: rest_view.filters.is_empty(),
    })
}

fn collection_name_filter_matches(
    proto: &KanbanViewEvidence,
    rest: &[RestCollectionView],
    view_id: &str,
    filter_id: &str,
    requested: &anytype_rpc::model::block::content::dataview::Filter,
    exact_name: &str,
) -> bool {
    let [proto_filter] = proto.view.filters.as_slice() else {
        return false;
    };
    if proto_filter.id != filter_id
        || proto_filter.relation_key != requested.relation_key
        || proto_filter.condition != requested.condition
        || proto_filter.value != requested.value
        || proto_filter.format != requested.format
    {
        return false;
    }
    let matching_views = rest
        .iter()
        .filter(|view| view.id == view_id)
        .collect::<Vec<_>>();
    let [rest_view] = matching_views.as_slice() else {
        return false;
    };
    let [rest_filter] = rest_view.filters.as_slice() else {
        return false;
    };
    rest_filter.id == filter_id
        && rest_filter.property_key == "name"
        && rest_filter.format == PropertyFormat::Text
        && rest_filter.condition == crate::filters::Condition::Equal
        && rest_filter.value == exact_name
}

fn validate_kanban_view_evidence(
    evidence: &KanbanViewEvidence,
    status_property: &Property,
    status_relation_key: &str,
) -> TestResult<()> {
    if status_property.format() != PropertyFormat::Select {
        return Err(kanban_fixture_code_error("view-group-format"));
    }
    if evidence.rest_layout != ViewLayout::Kanban
        || anytype_rpc::model::block::content::dataview::view::Type::try_from(evidence.view.r#type)
            .ok()
            != Some(anytype_rpc::model::block::content::dataview::view::Type::Kanban)
        || evidence.view.group_relation_key != status_relation_key
    {
        return Err(kanban_fixture_code_error("view-layout-or-group"));
    }
    if !evidence.rest_filters_empty || !evidence.view.filters.is_empty() {
        return Err(kanban_fixture_code_error("view-filters"));
    }
    if !evidence.relation_links.contains(&(
        status_relation_key.to_owned(),
        anytype_rpc::model::RelationFormat::Status as i32,
    )) {
        return Err(kanban_fixture_code_error("view-missing-relation"));
    }
    Ok(())
}

fn validate_updated_kanban_view(
    response: &update_dataview_view::Response,
    space_id: &str,
    collection_id: &str,
    block_id: &str,
    view_id: &str,
    requested_view: &DataviewView,
) -> TestResult<()> {
    if response.error.as_ref().map(|error| error.code) != Some(0) {
        return Err(kanban_fixture_code_error("view-update-response"));
    }
    let event = response
        .event
        .as_ref()
        .ok_or_else(|| kanban_fixture_code_error("view-update-event"))?;
    if event.context_id != collection_id {
        return Err(kanban_fixture_code_error("view-update-context"));
    }
    let sets = event
        .messages
        .iter()
        .filter_map(|message| match message.value.as_ref() {
            Some(EventValue::BlockDataviewViewSet(set)) => Some((message, set)),
            _ => None,
        })
        .collect::<Vec<_>>();
    let updates = event
        .messages
        .iter()
        .filter_map(|message| match message.value.as_ref() {
            Some(EventValue::BlockDataviewViewUpdate(update)) => Some((message, update)),
            _ => None,
        })
        .collect::<Vec<_>>();
    if sets.len() + updates.len() != 1 {
        return Err(kanban_fixture_code_error("view-update-event-count"));
    }
    if let [(message, set)] = sets.as_slice() {
        if message.space_id == space_id
            && set.id == block_id
            && set.view_id == view_id
            && set.view.as_ref() == Some(requested_view)
        {
            return Ok(());
        }
        return Err(kanban_fixture_code_error("view-update-event-identity"));
    }
    let [(message, update)] = updates.as_slice() else {
        return Err(kanban_fixture_code_error("view-update-event-kind"));
    };
    let fields = update
        .fields
        .as_ref()
        .ok_or_else(|| kanban_fixture_code_error("view-update-event-fields"))?;
    if message.space_id != space_id
        || update.id != block_id
        || update.view_id != view_id
        || !update.filter.is_empty()
        || !update.relation.is_empty()
        || !update.sort.is_empty()
        || fields.r#type != requested_view.r#type
        || fields.name != requested_view.name
        || fields.cover_relation_key != requested_view.cover_relation_key
        || fields.hide_icon != requested_view.hide_icon
        || fields.card_size != requested_view.card_size
        || fields.cover_fit != requested_view.cover_fit
        || fields.group_relation_key != requested_view.group_relation_key
        || fields.end_relation_key != requested_view.end_relation_key
        || fields.group_background_colors != requested_view.group_background_colors
        || fields.page_limit != requested_view.page_limit
        || fields.default_template_id != requested_view.default_template_id
        || fields.default_object_type_id != requested_view.default_object_type_id
        || fields.wrap_content != requested_view.wrap_content
        || fields.list_size != requested_view.list_size
        || fields.alternate_rows != requested_view.alternate_rows
    {
        return Err(kanban_fixture_code_error("view-update-event-identity"));
    }
    Ok(())
}

async fn complete_kanban_tag_snapshot(
    client: &AnytypeClient,
    space_id: &str,
    property_id: &str,
) -> TestResult<Vec<Tag>> {
    let mut offset = 0_u32;
    let mut expected_total = None;
    let mut tags = Vec::new();
    loop {
        let response = client
            .tags(space_id, property_id)
            .limit(KANBAN_FIXTURE_PAGE_LIMIT)
            .offset(offset)
            .list()
            .await
            .map_err(|_| kanban_fixture_code_error("tag-page-read"))?
            .into_response();
        if response.pagination.offset != offset
            || response.pagination.limit != KANBAN_FIXTURE_PAGE_LIMIT
            || expected_total.is_some_and(|total| total != response.pagination.total)
            || response.pagination.total > KANBAN_FIXTURE_MAX_ITEMS
            || response.items.len() > KANBAN_FIXTURE_PAGE_LIMIT as usize
            || (response.pagination.has_more && response.items.is_empty())
        {
            return Err(kanban_fixture_code_error("tag-pagination"));
        }
        expected_total = Some(response.pagination.total);
        let page_len = response.items.len();
        tags.extend(response.items);
        if !response.pagination.has_more {
            break;
        }
        offset = offset
            .checked_add(
                u32::try_from(page_len).map_err(|_| kanban_fixture_code_error("tag-page-size"))?,
            )
            .ok_or_else(|| kanban_fixture_code_error("tag-page-overflow"))?;
    }
    let ids = tags
        .iter()
        .map(|tag| tag.id.as_str())
        .collect::<BTreeSet<_>>();
    if expected_total != Some(tags.len()) || ids.len() != tags.len() {
        return Err(kanban_fixture_code_error("tag-completeness"));
    }
    Ok(tags)
}

async fn complete_kanban_property_snapshot(
    client: &AnytypeClient,
    space_id: &str,
) -> TestResult<BTreeSet<String>> {
    let response = client
        .properties(space_id)
        .limit(COLLECTION_VIEW_FIXTURE_SCAN_LIMIT)
        .offset(0)
        .list()
        .await
        .map_err(|_| kanban_fixture_code_error("property-page-read"))?
        .into_response();
    if response.pagination.offset != 0
        || response.pagination.limit != COLLECTION_VIEW_FIXTURE_SCAN_LIMIT
        || response.pagination.has_more
        || response.pagination.total != response.items.len()
        || response.items.len() > COLLECTION_VIEW_FIXTURE_SCAN_LIMIT as usize
    {
        return Err(kanban_fixture_code_error("property-pagination"));
    }
    let mut ids = BTreeSet::new();
    for property in response.items {
        client
            .config
            .limits
            .validate_id(&property.id, "kanban property evidence")
            .map_err(|_| kanban_fixture_code_error("property-id"))?;
        if !ids.insert(property.id) {
            return Err(kanban_fixture_code_error("property-duplicates"));
        }
    }
    Ok(ids)
}

async fn complete_kanban_item_snapshot(
    client: &AnytypeClient,
    space_id: &str,
    collection_id: &str,
    view_id: &str,
) -> TestResult<Vec<Object>> {
    let mut offset = 0_u32;
    let mut expected_total = None;
    let mut items = Vec::new();
    loop {
        let response = client
            .view_list_objects(space_id, collection_id)
            .view(view_id)
            .limit(KANBAN_FIXTURE_PAGE_LIMIT)
            .offset(offset)
            .list()
            .await
            .map_err(|_| kanban_fixture_code_error("item-page-read"))?
            .into_response();
        if response.pagination.offset != offset
            || response.pagination.limit != KANBAN_FIXTURE_PAGE_LIMIT
            || expected_total.is_some_and(|total| total != response.pagination.total)
            || response.pagination.total > KANBAN_FIXTURE_MAX_ITEMS
            || response.items.len() > KANBAN_FIXTURE_PAGE_LIMIT as usize
            || (response.pagination.has_more && response.items.is_empty())
        {
            return Err(kanban_fixture_code_error("item-pagination"));
        }
        expected_total = Some(response.pagination.total);
        let page_len = response.items.len();
        items.extend(response.items);
        if !response.pagination.has_more {
            break;
        }
        offset = offset
            .checked_add(
                u32::try_from(page_len).map_err(|_| kanban_fixture_code_error("item-page-size"))?,
            )
            .ok_or_else(|| kanban_fixture_code_error("item-page-overflow"))?;
    }
    let ids = items
        .iter()
        .map(|item| item.id.as_str())
        .collect::<BTreeSet<_>>();
    if expected_total != Some(items.len()) || ids.len() != items.len() {
        return Err(kanban_fixture_code_error("item-completeness"));
    }
    Ok(items)
}

fn validate_kanban_fixture_registration(
    cleanup: &TestCleanup,
    space_id: &str,
    fixture: &KanbanFixture,
) -> TestResult<()> {
    if fixture.columns.len() != 2
        || fixture.items.len() != 3
        || fixture.status_property.format() != PropertyFormat::Select
        || cleanup.collection_fixture_type_id(space_id, &fixture.collection.id)
            != Some(fixture.collection_type.id.clone())
        || !cleanup.owns_collection_view_fixture(space_id, &fixture.collection.id, &fixture.view.id)
        || !cleanup.is_registered_id(&fixture.item_type.id)
        || !cleanup.is_registered_id(&fixture.collection_type.id)
        || !cleanup.is_registered_id(&fixture.collection.id)
        || !cleanup.is_registered_id(&fixture.status_property.id)
    {
        return Err(kanban_fixture_code_error("registration"));
    }
    if fixture
        .columns
        .iter()
        .any(|tag| !cleanup.owns_kanban_tag_fixture(space_id, &fixture.status_property.id, &tag.id))
        || fixture
            .items
            .iter()
            .any(|item| !cleanup.is_registered_id(&item.object.id))
    {
        return Err(kanban_fixture_code_error("child-registration"));
    }
    let all_ids = fixture
        .columns
        .iter()
        .map(|tag| tag.id.as_str())
        .chain(fixture.items.iter().map(|item| item.object.id.as_str()))
        .collect::<BTreeSet<_>>();
    if all_ids.len() != fixture.columns.len() + fixture.items.len() {
        return Err(kanban_fixture_code_error("registration-collision"));
    }
    Ok(())
}

fn kanban_fixture_code_error(code: &'static str) -> TestError {
    TestError::Assertion {
        message: format!("cleanup-safe Kanban fixture failed: {code}"),
    }
}

fn collection_view_ids_are_unique(views: &[RestCollectionView]) -> bool {
    let ids = views
        .iter()
        .map(|view| view.id.as_str())
        .collect::<BTreeSet<_>>();
    ids.len() == views.len() && ids.iter().all(|id| valid_collection_view_id(id))
}

fn resolve_collection_dataview(
    root_id: &str,
    collection_id: &str,
    blocks: &[anytype_rpc::model::Block],
    rest_views: &[RestCollectionView],
    default_id: &str,
) -> TestResult<ResolvedCollectionDataview> {
    if root_id != collection_id {
        return Err(collection_view_fixture_code_error("object-show-root"));
    }
    let dataview_blocks = blocks
        .iter()
        .filter(|block| matches!(block.content_value, Some(ContentValue::Dataview(_))))
        .collect::<Vec<_>>();
    if dataview_blocks.len() != 1 || dataview_blocks[0].id != COLLECTION_DATAVIEW_BLOCK_ID {
        return Err(collection_view_fixture_code_error("dataview-block"));
    }
    let mut matches = dataview_blocks.into_iter().filter_map(|block| {
        let Some(ContentValue::Dataview(dataview)) = block.content_value.as_ref() else {
            return None;
        };
        let default_views = dataview
            .views
            .iter()
            .filter(|view| view.id == default_id)
            .collect::<Vec<_>>();
        (default_views.len() == 1).then(|| (block, dataview, default_views[0]))
    });
    let Some((block, dataview, default_view)) = matches.next() else {
        return Err(collection_view_fixture_code_error("default-view"));
    };
    if matches.next().is_some()
        || !dataview.is_collection
        || !dataview_view_snapshot_matches(&dataview.views, rest_views)
    {
        return Err(collection_view_fixture_code_error("rest-proto-view"));
    }
    Ok(ResolvedCollectionDataview {
        block_id: block.id.clone(),
        default_view: default_view.clone(),
        source: dataview.source.clone(),
    })
}

fn dataview_view_snapshot_matches(
    proto_views: &[DataviewView],
    rest_views: &[RestCollectionView],
) -> bool {
    if proto_views.len() != rest_views.len() {
        return false;
    }
    let Ok(proto) = proto_views
        .iter()
        .map(dataview_view_as_rest)
        .collect::<TestResult<Vec<_>>>()
    else {
        return false;
    };
    let proto = proto
        .into_iter()
        .map(|view| (view.id.clone(), view))
        .collect::<BTreeMap<_, _>>();
    let rest = rest_views
        .iter()
        .cloned()
        .map(|view| (view.id.clone(), view))
        .collect::<BTreeMap<_, _>>();
    proto.len() == proto_views.len() && rest.len() == rest_views.len() && proto == rest
}

fn dataview_view_as_rest(view: &DataviewView) -> TestResult<RestCollectionView> {
    use anytype_rpc::model::block::content::dataview::{filter, sort, view};

    let layout = match view::Type::try_from(view.r#type).ok() {
        Some(view::Type::Table) => ViewLayout::Grid,
        Some(view::Type::List) => ViewLayout::List,
        Some(view::Type::Gallery) => ViewLayout::Gallery,
        Some(view::Type::Kanban) => ViewLayout::Kanban,
        Some(view::Type::Calendar) => ViewLayout::Calendar,
        Some(view::Type::Graph) => ViewLayout::Graph,
        None => return Err(collection_view_fixture_error()),
    };
    let mut filters = Vec::new();
    for item in &view.filters {
        let condition = match filter::Condition::try_from(item.condition).ok() {
            Some(filter::Condition::None) => continue,
            Some(filter::Condition::Equal) => crate::filters::Condition::Equal,
            Some(filter::Condition::NotEqual) => crate::filters::Condition::NotEqual,
            Some(filter::Condition::Greater) => crate::filters::Condition::Greater,
            Some(filter::Condition::Less) => crate::filters::Condition::Less,
            Some(filter::Condition::GreaterOrEqual) => crate::filters::Condition::GreaterOrEqual,
            Some(filter::Condition::LessOrEqual) => crate::filters::Condition::LessOrEqual,
            Some(filter::Condition::Like) => crate::filters::Condition::Contains,
            Some(filter::Condition::NotLike) => crate::filters::Condition::NotContains,
            Some(filter::Condition::In) => crate::filters::Condition::In,
            Some(filter::Condition::NotIn) => crate::filters::Condition::NotIn,
            Some(filter::Condition::Empty) => crate::filters::Condition::Empty,
            Some(filter::Condition::NotEmpty) => crate::filters::Condition::NotEmpty,
            Some(filter::Condition::AllIn) => crate::filters::Condition::All,
            _ => return Err(collection_view_fixture_error()),
        };
        let value = match item.value.as_ref().and_then(|value| value.kind.as_ref()) {
            Some(Kind::StringValue(value)) => value.clone(),
            _ => return Err(collection_view_fixture_error()),
        };
        filters.push(RestCollectionViewFilter {
            id: item.id.clone(),
            property_key: item.relation_key.clone(),
            format: rest_property_format(item.format)?,
            condition,
            value,
        });
    }
    let sorts = view
        .sorts
        .iter()
        .map(|item| {
            let sort_type = match sort::Type::try_from(item.r#type).ok() {
                Some(sort::Type::Asc) => RestCollectionViewSortType::Asc,
                Some(sort::Type::Desc) => RestCollectionViewSortType::Desc,
                Some(sort::Type::Custom) => RestCollectionViewSortType::Custom,
                None => return Err(collection_view_fixture_error()),
            };
            Ok(RestCollectionViewSort {
                id: item.id.clone(),
                property_key: item.relation_key.clone(),
                format: rest_property_format(item.format)?,
                sort_type,
            })
        })
        .collect::<TestResult<Vec<_>>>()?;
    Ok(RestCollectionView {
        id: view.id.clone(),
        name: view.name.clone(),
        layout,
        filters,
        sorts,
    })
}

fn rest_property_format(format: i32) -> TestResult<crate::properties::PropertyFormat> {
    use anytype_rpc::model::RelationFormat;

    match RelationFormat::try_from(format).ok() {
        Some(RelationFormat::Longtext | RelationFormat::Shorttext) => {
            Ok(crate::properties::PropertyFormat::Text)
        }
        Some(RelationFormat::Number) => Ok(crate::properties::PropertyFormat::Number),
        Some(RelationFormat::Status) => Ok(crate::properties::PropertyFormat::Select),
        Some(RelationFormat::Tag) => Ok(crate::properties::PropertyFormat::MultiSelect),
        Some(RelationFormat::Date) => Ok(crate::properties::PropertyFormat::Date),
        Some(RelationFormat::File) => Ok(crate::properties::PropertyFormat::Files),
        Some(RelationFormat::Checkbox) => Ok(crate::properties::PropertyFormat::Checkbox),
        Some(RelationFormat::Url) => Ok(crate::properties::PropertyFormat::Url),
        Some(RelationFormat::Email) => Ok(crate::properties::PropertyFormat::Email),
        Some(RelationFormat::Phone) => Ok(crate::properties::PropertyFormat::Phone),
        Some(RelationFormat::Object) => Ok(crate::properties::PropertyFormat::Objects),
        _ => Err(collection_view_fixture_error()),
    }
}

fn clone_collection_view(default_view: &DataviewView, id: &str, name: &str) -> DataviewView {
    let mut view = default_view.clone();
    view.id = id.to_owned();
    view.name = name.to_owned();
    view
}

fn validate_created_collection_view(
    response: &create_dataview_view::Response,
    space_id: &str,
    collection_id: &str,
    block_id: &str,
    request_id: &str,
    requested_view: &DataviewView,
    existing: &[RestCollectionView],
) -> TestResult<String> {
    if !create_collection_view_succeeded(response.error.as_ref().map(|error| error.code))
        || validate_created_collection_view_identity(&response.view_id, request_id, existing)
            .is_err()
    {
        return Err(collection_view_fixture_error());
    }
    let Some(event) = response.event.as_ref() else {
        return Err(collection_view_fixture_error());
    };
    if event.context_id != collection_id {
        return Err(collection_view_fixture_error());
    }
    let view_sets = event
        .messages
        .iter()
        .filter_map(|message| match message.value.as_ref() {
            Some(EventValue::BlockDataviewViewSet(view_set)) => Some((message, view_set)),
            _ => None,
        })
        .collect::<Vec<_>>();
    if view_sets.len() != 1 {
        return Err(collection_view_fixture_error());
    }
    let (message, view_set) = view_sets[0];
    let mut expected_view = requested_view.clone();
    expected_view.id.clone_from(&response.view_id);
    if message.space_id != space_id
        || view_set.id != block_id
        || view_set.view_id != response.view_id
        || view_set.view.as_ref() != Some(&expected_view)
    {
        return Err(collection_view_fixture_error());
    }
    Ok(response.view_id.clone())
}

fn validate_created_collection_view_identity(
    returned_view_id: &str,
    request_id: &str,
    existing: &[RestCollectionView],
) -> TestResult<String> {
    if !valid_collection_view_id(returned_view_id)
        || returned_view_id == request_id
        || existing.iter().any(|view| view.id == returned_view_id)
    {
        return Err(collection_view_fixture_error());
    }
    Ok(returned_view_id.to_owned())
}

fn valid_collection_view_id(id: &str) -> bool {
    !id.is_empty()
        && !matches!(id, "." | "..")
        && id.len() <= 256
        && id
            .bytes()
            .all(|character| character.is_ascii_alphanumeric() || b"._~-".contains(&character))
}

fn create_collection_view_succeeded(error_code: Option<i32>) -> bool {
    error_code == Some(create_dataview_view::response::error::Code::Null as i32)
}

fn object_show_succeeded(error_code: Option<i32>) -> bool {
    error_code == Some(object_show::response::error::Code::Null as i32)
}

fn collection_view_snapshot_matches(
    views: &[RestCollectionView],
    expected: &BTreeMap<String, RestCollectionView>,
) -> bool {
    if views.len() != expected.len() || !collection_view_ids_are_unique(views) {
        return false;
    }
    let actual = views
        .iter()
        .cloned()
        .map(|view| (view.id.clone(), view))
        .collect::<BTreeMap<_, _>>();
    &actual == expected
}

fn collection_view_fixture_error() -> TestError {
    TestError::Assertion {
        message: "cleanup-safe collection view fixture creation failed".to_owned(),
    }
}

fn collection_view_fixture_code_error(code: &'static str) -> TestError {
    TestError::Assertion {
        message: format!("cleanup-safe collection view fixture failed: {code}"),
    }
}

fn collection_view_fixture_api_error() -> AnytypeError {
    AnytypeError::Other {
        message: "collection view fixture listing was malformed".to_owned(),
    }
}

fn collection_view_fixture_transport_error(_: tonic::Status) -> TestError {
    collection_view_fixture_error()
}

fn template_fixture_transport_error(_: tonic::Status) -> TestError {
    template_fixture_error()
}

fn template_fixture_response_succeeded(
    error: Option<&template_create_from_object::response::Error>,
) -> bool {
    error.is_some_and(|error| {
        error.code == template_create_from_object::response::error::Code::Null as i32
    })
}

fn template_fixture_response_error(
    _: Option<&template_create_from_object::response::Error>,
) -> TestError {
    template_fixture_error()
}

fn authorize_template_resource(
    cleanup: &TestCleanup,
    snapshot: &TemplateOwnershipSnapshot,
    forbidden_ids: &[&str],
    resource: TemplateFixtureResource,
) -> TestResult<()> {
    let candidate_id = resource.id();
    if forbidden_ids.contains(&candidate_id)
        || snapshot.contains(candidate_id)
        || cleanup.is_registered_id(candidate_id)
    {
        return Err(template_fixture_error());
    }
    cleanup.add_template_resource(resource)
}

fn authorize_template_source(
    cleanup: &TestCleanup,
    snapshot: &TemplateOwnershipSnapshot,
    space_id: &str,
    type_id: &str,
    source: &Object,
) -> TestResult<()> {
    let source_type_id = source.r#type.as_ref().map(|typ| typ.id.as_str());
    if !cleanup.owns_template_type(space_id, type_id)
        || source.space_id != space_id
        || source.archived
        || source.object != DataModel::Object
        || source_type_id != Some(type_id)
    {
        return Err(template_fixture_error());
    }
    authorize_template_resource(
        cleanup,
        snapshot,
        &[space_id, type_id],
        TemplateFixtureResource::Source {
            space_id: space_id.to_owned(),
            type_id: type_id.to_owned(),
            source_id: source.id.clone(),
        },
    )
}

fn authorize_owned_source_template(
    cleanup: &TestCleanup,
    snapshot: &TemplateOwnershipSnapshot,
    space_id: &str,
    type_id: &str,
    source_id: &str,
    template_id: &str,
) -> TestResult<()> {
    if !cleanup.owns_template_type(space_id, type_id)
        || !cleanup.owns_template_source(space_id, type_id, source_id)
    {
        return Err(template_fixture_error());
    }
    authorize_template_resource(
        cleanup,
        snapshot,
        &[space_id, type_id, source_id],
        TemplateFixtureResource::Template {
            space_id: space_id.to_owned(),
            type_id: type_id.to_owned(),
            source_id: source_id.to_owned(),
            template_id: template_id.to_owned(),
        },
    )
}

fn template_fixture_error() -> TestError {
    TestError::Assertion {
        message: "cleanup-owned template fixture operation failed".to_owned(),
    }
}

fn template_fixture_evidence_error(reason: &'static str) -> TestError {
    TestError::Assertion {
        message: format!("cleanup-owned template fixture evidence failed: {reason}"),
    }
}

fn classify_template_fixture_evidence(error: AnytypeError, reason: &'static str) -> TestError {
    eprintln!(
        "cleanup-owned template fixture evidence failed at {reason}: {}",
        error.diagnostic()
    );
    template_fixture_evidence_error(reason)
}

fn template_cleanup_provenance_error() -> TestError {
    TestError::Assertion {
        message: "cleanup-owned template fixture provenance re-verification failed".to_owned(),
    }
}

fn template_fixture_api_error(reason: &'static str) -> AnytypeError {
    eprintln!("cleanup-owned template fixture evidence rejected: {reason}");
    AnytypeError::Other {
        message: format!("template fixture evidence was incomplete: {reason}"),
    }
}

fn template_fixture_verify_config() -> VerifyConfig {
    VerifyConfig {
        timeout: TEMPLATE_FIXTURE_VERIFY_TIMEOUT,
        max_attempts: TEMPLATE_FIXTURE_VERIFY_ATTEMPTS,
        ..VerifyConfig::default()
    }
}

async fn complete_template_ids(
    client: &AnytypeClient,
    space_id: &str,
    type_id: &str,
) -> Result<BTreeSet<String>, AnytypeError> {
    Ok(complete_template_objects(client, space_id, type_id)
        .await?
        .into_keys()
        .collect())
}

async fn complete_template_objects(
    client: &AnytypeClient,
    space_id: &str,
    type_id: &str,
) -> Result<BTreeMap<String, Object>, AnytypeError> {
    let response = client
        .templates(space_id, type_id)
        .limit(TEMPLATE_FIXTURE_LIMIT)
        .offset(0)
        .list()
        .await?
        .into_response();
    if response.pagination.offset != 0
        || response.pagination.limit != TEMPLATE_FIXTURE_LIMIT
        || response.pagination.has_more
        || response.pagination.total != response.items.len()
    {
        return Err(template_fixture_api_error("template-page"));
    }
    let mut templates = BTreeMap::new();
    for template in response.items {
        if !template_has_canonical_identity(client, &template, space_id)
            || templates.insert(template.id.clone(), template).is_some()
        {
            return Err(template_fixture_api_error("template-identity"));
        }
    }
    Ok(templates)
}

fn template_has_canonical_identity(
    client: &AnytypeClient,
    template: &Object,
    space_id: &str,
) -> bool {
    client
        .get_config()
        .limits
        .validate_id(&template.id, "template fixture evidence")
        .is_ok()
        && template.space_id == space_id
        && !template.archived
        && template.r#type.as_ref().is_some_and(|typ| {
            !typ.archived
                && typ.key == "template"
                && client
                    .get_config()
                    .limits
                    .validate_id(&typ.id, "template fixture generic type")
                    .is_ok()
        })
}

struct CompleteTypeInventory {
    all_ids: BTreeSet<String>,
    active_ids: Vec<String>,
}

async fn complete_type_inventory(
    client: &AnytypeClient,
    space_id: &str,
) -> Result<CompleteTypeInventory, AnytypeError> {
    let response = client
        .types(space_id)
        .limit(TEMPLATE_FIXTURE_LIMIT)
        .offset(0)
        .list()
        .await?
        .into_response();
    if response.pagination.offset != 0
        || response.pagination.limit != TEMPLATE_FIXTURE_LIMIT
        || response.pagination.has_more
        || response.pagination.total != response.items.len()
    {
        return Err(template_fixture_api_error("type-page"));
    }
    let mut all_ids = BTreeSet::new();
    let mut active_ids = Vec::new();
    for typ in response.items {
        client
            .get_config()
            .limits
            .validate_id(&typ.id, "template fixture type evidence")?;
        if !all_ids.insert(typ.id.clone()) {
            return Err(template_fixture_api_error("duplicate-type"));
        }
        if !typ.archived {
            active_ids.push(typ.id);
        }
    }
    Ok(CompleteTypeInventory {
        all_ids,
        active_ids,
    })
}

async fn complete_owned_type_object_ids(
    client: &AnytypeClient,
    space_id: &str,
    type_id: &str,
) -> Result<BTreeSet<String>, AnytypeError> {
    complete_owned_type_object_ids_with_archived(client, space_id, type_id, || async {
        Ok(client
            .list_archived(space_id)
            .types([type_id])
            .limit(TEMPLATE_FIXTURE_SCOPED_OBJECT_PAGE_LIMIT)
            .offset(0)
            .list()
            .await?
            .into_response())
    })
    .await
}

async fn complete_owned_type_object_ids_with_archived<F, Fut>(
    client: &AnytypeClient,
    space_id: &str,
    type_id: &str,
    archived_fetch: F,
) -> Result<BTreeSet<String>, AnytypeError>
where
    F: FnOnce() -> Fut,
    Fut:
        std::future::Future<Output = Result<crate::paged::PaginatedResponse<Object>, AnytypeError>>,
{
    let active = client
        .objects(space_id)
        .filter(Filter::type_in([type_id]))
        .limit(TEMPLATE_FIXTURE_SCOPED_OBJECT_PAGE_LIMIT)
        .offset(0)
        .list()
        .await?
        .into_response();
    let archived = archived_fetch().await?;
    owned_type_object_ids_from_pages(client, space_id, type_id, active, archived)
}

fn owned_type_object_ids_from_pages(
    client: &AnytypeClient,
    space_id: &str,
    type_id: &str,
    active: crate::paged::PaginatedResponse<Object>,
    archived: crate::paged::PaginatedResponse<Object>,
) -> Result<BTreeSet<String>, AnytypeError> {
    if active.pagination.offset != 0
        || active.pagination.limit != TEMPLATE_FIXTURE_SCOPED_OBJECT_PAGE_LIMIT
        || active.pagination.has_more
        || active.pagination.total != active.items.len()
        || active.items.len() > TEMPLATE_FIXTURE_MAX_SOURCES
    {
        return Err(template_fixture_api_error("owned-type-active-page"));
    }
    let mut all_ids = BTreeSet::new();
    for object in active.items {
        // The REST objects route can include archived rows even for this exact
        // type. They belong to the separately proven archived inventory, not
        // the active set used for ownership overlap checks.
        if object.archived {
            continue;
        }
        validate_owned_type_object(client, &object, space_id, type_id, Some(false))?;
        if !all_ids.insert(object.id) {
            return Err(template_fixture_api_error("duplicate-owned-type-object"));
        }
    }

    if archived.pagination.offset != 0
        || archived.pagination.limit != TEMPLATE_FIXTURE_SCOPED_OBJECT_PAGE_LIMIT
        || archived.pagination.has_more
        || archived.pagination.total != archived.items.len()
        || archived.items.len() > TEMPLATE_FIXTURE_MAX_SOURCES
    {
        return Err(template_fixture_api_error("owned-type-archived-page"));
    }
    let mut archived_ids = BTreeSet::new();
    for object in archived.items {
        validate_owned_type_object(client, &object, space_id, type_id, Some(true))?;
        if !archived_ids.insert(object.id.clone()) {
            return Err(template_fixture_api_error(
                "duplicate-owned-type-archived-object",
            ));
        }
        if !all_ids.insert(object.id) {
            return Err(template_fixture_api_error(
                "active-archived-owned-type-overlap",
            ));
        }
    }
    if all_ids.len() > TEMPLATE_FIXTURE_MAX_SOURCES {
        return Err(template_fixture_api_error("owned-type-object-capacity"));
    }
    Ok(all_ids)
}

fn validate_owned_type_object(
    client: &AnytypeClient,
    object: &Object,
    space_id: &str,
    type_id: &str,
    archived: Option<bool>,
) -> Result<(), AnytypeError> {
    client
        .get_config()
        .limits
        .validate_id(&object.id, "template fixture owned-type evidence")?;
    let returned_type_id = object.r#type.as_ref().map(|typ| typ.id.as_str());
    let type_matches = if archived == Some(true) {
        // The authenticated archived search is filtered to this exact type,
        // but its normalized result intentionally omits type metadata. Reject
        // a contradictory returned type while accepting that documented
        // omission.
        returned_type_id.is_none_or(|returned| returned == type_id)
    } else {
        returned_type_id == Some(type_id)
    };
    if object.space_id != space_id
        || archived.is_some_and(|expected| object.archived != expected)
        || !type_matches
    {
        return Err(template_fixture_api_error("owned-type-object-identity"));
    }
    Ok(())
}

async fn complete_global_template_owners(
    client: &AnytypeClient,
    space_id: &str,
    active_type_ids: &[String],
) -> Result<BTreeMap<String, String>, AnytypeError> {
    let mut owners = BTreeMap::new();
    for type_id in active_type_ids {
        let templates = complete_template_objects(client, space_id, type_id).await?;
        if owners.len().saturating_add(templates.len()) > TEMPLATE_FIXTURE_GLOBAL_TEMPLATE_LIMIT {
            return Err(template_fixture_api_error("global-template-capacity"));
        }
        for id in templates.into_keys() {
            if owners.insert(id, type_id.clone()).is_some() {
                return Err(template_fixture_api_error("duplicate-template-owner"));
            }
        }
    }
    Ok(owners)
}

#[derive(Default)]
struct TemplateOwnershipSnapshot {
    type_ids: BTreeSet<String>,
    object_ids: BTreeSet<String>,
    template_ids: BTreeSet<String>,
}

impl TemplateOwnershipSnapshot {
    fn contains(&self, id: &str) -> bool {
        self.type_ids.contains(id) || self.object_ids.contains(id) || self.template_ids.contains(id)
    }
}

async fn complete_template_type_snapshot(
    client: &AnytypeClient,
    space_id: &str,
) -> Result<TemplateOwnershipSnapshot, AnytypeError> {
    let types = complete_type_inventory(client, space_id).await?;
    Ok(TemplateOwnershipSnapshot {
        type_ids: types.all_ids,
        ..TemplateOwnershipSnapshot::default()
    })
}

async fn complete_template_source_snapshot(
    client: &AnytypeClient,
    space_id: &str,
    type_id: &str,
) -> Result<TemplateOwnershipSnapshot, AnytypeError> {
    complete_template_source_snapshot_with_archived(client, space_id, type_id, || async {
        Ok(client
            .list_archived(space_id)
            .types([type_id])
            .limit(TEMPLATE_FIXTURE_SCOPED_OBJECT_PAGE_LIMIT)
            .offset(0)
            .list()
            .await?
            .into_response())
    })
    .await
}

async fn complete_template_source_snapshot_with_archived<F, Fut>(
    client: &AnytypeClient,
    space_id: &str,
    type_id: &str,
    archived_fetch: F,
) -> Result<TemplateOwnershipSnapshot, AnytypeError>
where
    F: FnOnce() -> Fut,
    Fut:
        std::future::Future<Output = Result<crate::paged::PaginatedResponse<Object>, AnytypeError>>,
{
    Ok(TemplateOwnershipSnapshot {
        object_ids: complete_owned_type_object_ids_with_archived(
            client,
            space_id,
            type_id,
            archived_fetch,
        )
        .await?,
        ..TemplateOwnershipSnapshot::default()
    })
}

async fn complete_template_id_snapshot(
    client: &AnytypeClient,
    space_id: &str,
    type_id: &str,
) -> Result<TemplateOwnershipSnapshot, AnytypeError> {
    Ok(TemplateOwnershipSnapshot {
        template_ids: complete_template_ids(client, space_id, type_id).await?,
        ..TemplateOwnershipSnapshot::default()
    })
}

struct TemplateFixtureEvidence {
    owning_type_id: String,
    listed: Object,
    template: Object,
}

async fn verify_template_fixture(
    client: &AnytypeClient,
    config: &VerifyConfig,
    space_id: &str,
    type_id: &str,
    template_id: &str,
) -> Result<Object, AnytypeError> {
    let expected_id = template_id.to_owned();
    let evidence = verify_semantic(
        config,
        "template fixture",
        template_id,
        || async {
            let types = complete_type_inventory(client, space_id).await?;
            let owners =
                complete_global_template_owners(client, space_id, &types.active_ids).await?;
            let owning_type_id = owners
                .get(template_id)
                .cloned()
                .ok_or_else(|| template_fixture_api_error("template-owner-missing"))?;
            let templates = complete_template_objects(client, space_id, type_id).await?;
            let listed = templates
                .get(template_id)
                .cloned()
                .ok_or_else(|| template_fixture_api_error("template-listing-missing"))?;
            let template = client
                .template(space_id, type_id, template_id)
                .get()
                .await?;
            Ok(TemplateFixtureEvidence {
                owning_type_id,
                listed,
                template,
            })
        },
        |evidence| {
            let listed_type = evidence.listed.r#type.as_ref();
            let fetched_type = evidence.template.r#type.as_ref();
            evidence.owning_type_id == type_id
                && evidence.listed.id == expected_id
                && template_has_canonical_identity(client, &evidence.template, space_id)
                && evidence.template.id == expected_id
                && listed_type
                    .zip(fetched_type)
                    .is_some_and(|(listed, fetched)| {
                        listed.id == fetched.id && listed.key == fetched.key
                    })
        },
    )
    .await?;
    Ok(evidence.template)
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
    chat_messages: Mutex<BTreeSet<(String, String, String)>>,
    collection_fixtures: Mutex<BTreeSet<(String, String, String)>>,
    collection_view_fixtures: Mutex<BTreeSet<(String, String, String)>>,
    kanban_tag_fixtures: Mutex<BTreeSet<(String, String, String)>>,
    space_fixtures: Mutex<BTreeMap<String, String>>,
    template_resources: Mutex<Vec<TemplateFixtureResource>>,
    registered_ids: Mutex<BTreeSet<String>>,
    temp_paths: Mutex<Vec<PathBuf>>,
}

pub(super) struct ChildStopReport {
    pub(super) outcome: ChildOwnershipOutcome,
    pub(super) errors: Vec<TestError>,
    pub(super) panics: Vec<Box<dyn std::any::Any + Send>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ChildOwnershipOutcome {
    NoChildren,
    Stopped,
    Unproven,
}

fn run_owned_child_stoppers(
    spawn_attempts: usize,
    mut stoppers: Vec<OwnedChildStopper>,
) -> ChildStopReport {
    if spawn_attempts == 0 {
        return ChildStopReport {
            outcome: ChildOwnershipOutcome::NoChildren,
            errors: Vec::new(),
            panics: Vec::new(),
        };
    }
    let mut errors = Vec::new();
    if spawn_attempts != stoppers.len() {
        errors.push(child_registry_error());
    }
    stoppers.reverse();
    let mut panics = Vec::new();
    for mut stopper in stoppers {
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(&mut stopper)) {
            Ok(Ok(())) => {}
            Ok(Err(error)) => errors.push(error),
            Err(payload) => panics.push(payload),
        }
    }
    let stopped = errors.is_empty() && panics.is_empty();
    ChildStopReport {
        outcome: if stopped {
            ChildOwnershipOutcome::Stopped
        } else {
            ChildOwnershipOutcome::Unproven
        },
        errors,
        panics,
    }
}

#[derive(Clone, Debug)]
enum TemplateFixtureResource {
    Type {
        space_id: String,
        type_id: String,
        type_key: String,
    },
    Source {
        space_id: String,
        type_id: String,
        source_id: String,
    },
    Template {
        space_id: String,
        type_id: String,
        source_id: String,
        template_id: String,
    },
}

impl TemplateFixtureResource {
    fn id(&self) -> &str {
        match self {
            Self::Type { type_id, .. } => type_id,
            Self::Source { source_id, .. } => source_id,
            Self::Template { template_id, .. } => template_id,
        }
    }

    fn owner(&self) -> (&str, &str) {
        match self {
            Self::Type {
                space_id, type_id, ..
            }
            | Self::Source {
                space_id, type_id, ..
            }
            | Self::Template {
                space_id, type_id, ..
            } => (space_id, type_id),
        }
    }

    fn kind(&self) -> &'static str {
        match self {
            Self::Type { .. } => "type",
            Self::Source { .. } => "source",
            Self::Template { .. } => "template",
        }
    }
}

impl TestCleanup {
    pub fn is_empty(&self) -> bool {
        // Inspect each registry under its own guard. Creation claims that hold
        // multiple locks always acquire authoritative IDs before their private
        // provenance registry.
        let objects_empty = self.objects.lock().is_empty();
        let chat_messages_empty = self.chat_messages.lock().is_empty();
        let collections_empty = self.collection_fixtures.lock().is_empty();
        let collection_views_empty = self.collection_view_fixtures.lock().is_empty();
        let kanban_tags_empty = self.kanban_tag_fixtures.lock().is_empty();
        let spaces_empty = self.space_fixtures.lock().is_empty();
        let templates_empty = self.template_resources.lock().is_empty();
        let registered_empty = self.registered_ids.lock().is_empty();
        let paths_empty = self.temp_paths.lock().is_empty();
        objects_empty
            && chat_messages_empty
            && collections_empty
            && collection_views_empty
            && kanban_tags_empty
            && spaces_empty
            && templates_empty
            && registered_empty
            && paths_empty
    }

    /// Remembers this object for deletion after the test
    pub fn add_object(&self, space_id: &str, id: &str) {
        self.add_generic_resource(space_id, id, DataModel::Object);
    }

    fn add_chat_message(&self, space_id: &str, chat_id: &str, message_id: &str) -> bool {
        let chat_is_owned =
            self.objects
                .lock()
                .iter()
                .any(|(registered_space, registered_id, model)| {
                    registered_space == space_id
                        && registered_id == chat_id
                        && *model == DataModel::Object
                });
        chat_is_owned
            && self.chat_messages.lock().insert((
                space_id.to_owned(),
                chat_id.to_owned(),
                message_id.to_owned(),
            ))
    }

    fn has_type(&self, space_id: &str, id: &str) -> bool {
        self.objects
            .lock()
            .iter()
            .any(|(registered_space, registered_id, model)| {
                registered_space == space_id && registered_id == id && *model == DataModel::Type
            })
    }

    fn claim_collection_fixture(&self, space_id: &str, id: &str, type_id: &str) -> bool {
        // Keep the shared claim lock order authoritative IDs -> generic cleanup
        // entries -> private provenance. Space claims use authoritative IDs ->
        // private space provenance.
        let mut registered_ids = self.registered_ids.lock();
        let mut objects = self.objects.lock();
        let mut fixtures = self.collection_fixtures.lock();
        if registered_ids.contains(id)
            || objects
                .iter()
                .any(|(_, registered_id, _)| registered_id == id)
            || fixtures
                .iter()
                .any(|(_, registered_id, _)| registered_id == id)
        {
            return false;
        }

        let registered = registered_ids.insert(id.to_owned());
        objects.push((space_id.to_owned(), id.to_owned(), DataModel::Object));
        let proven = fixtures.insert((space_id.to_owned(), id.to_owned(), type_id.to_owned()));
        debug_assert!(registered && proven);
        true
    }

    fn collection_fixture_type_id(&self, space_id: &str, id: &str) -> Option<String> {
        let fixtures = self.collection_fixtures.lock();
        let mut matches = fixtures
            .iter()
            .filter(|(registered_space, registered_id, _)| {
                registered_space == space_id && registered_id == id
            });
        let (_, _, type_id) = matches.next()?;
        if matches.next().is_some() {
            return None;
        }
        Some(type_id.clone())
    }

    fn claim_collection_view_fixture(
        &self,
        space_id: &str,
        collection_id: &str,
        view_id: &str,
    ) -> bool {
        if self
            .collection_fixture_type_id(space_id, collection_id)
            .is_none()
        {
            return false;
        }
        let mut registered_ids = self.registered_ids.lock();
        let mut views = self.collection_view_fixtures.lock();
        if registered_ids.contains(view_id)
            || views
                .iter()
                .any(|(_, _, registered_view)| registered_view == view_id)
        {
            return false;
        }
        registered_ids.insert(view_id.to_owned());
        views.insert((
            space_id.to_owned(),
            collection_id.to_owned(),
            view_id.to_owned(),
        ))
    }

    fn owns_collection_view_fixture(
        &self,
        space_id: &str,
        collection_id: &str,
        view_id: &str,
    ) -> bool {
        self.collection_view_fixtures.lock().contains(&(
            space_id.to_owned(),
            collection_id.to_owned(),
            view_id.to_owned(),
        ))
    }

    fn claim_kanban_tag_fixture(&self, space_id: &str, property_id: &str, tag_id: &str) -> bool {
        let property_is_owned =
            self.objects
                .lock()
                .iter()
                .any(|(registered_space, registered_id, model)| {
                    registered_space == space_id
                        && registered_id == property_id
                        && *model == DataModel::Property
                });
        if !property_is_owned {
            return false;
        }
        let mut registered_ids = self.registered_ids.lock();
        let mut tags = self.kanban_tag_fixtures.lock();
        if registered_ids.contains(tag_id)
            || tags
                .iter()
                .any(|(_, _, registered_tag)| registered_tag == tag_id)
        {
            return false;
        }
        registered_ids.insert(tag_id.to_owned());
        tags.insert((
            space_id.to_owned(),
            property_id.to_owned(),
            tag_id.to_owned(),
        ))
    }

    fn owns_kanban_tag_fixture(&self, space_id: &str, property_id: &str, tag_id: &str) -> bool {
        self.kanban_tag_fixtures.lock().contains(&(
            space_id.to_owned(),
            property_id.to_owned(),
            tag_id.to_owned(),
        ))
    }

    /// Remembers this property for deletion after the test
    pub fn add_property(&self, space_id: &str, id: &str) {
        self.add_generic_resource(space_id, id, DataModel::Property);
    }

    /// Remembers this Type for deletion after the test
    pub fn add_type(&self, space_id: &str, id: &str) {
        self.add_generic_resource(space_id, id, DataModel::Type);
    }

    fn add_generic_resource(&self, space_id: &str, id: &str, model: DataModel) {
        let claimed = self.registered_ids.lock().insert(id.to_owned());
        if claimed {
            self.objects
                .lock()
                .push((space_id.to_owned(), id.to_owned(), model));
        }
    }

    fn is_registered_id(&self, id: &str) -> bool {
        self.registered_ids.lock().contains(id)
    }

    /// Remembers exact ID/name provenance created by `TestContext::create_space_fixture`.
    fn add_space_fixture(&self, id: &str, name: &str) -> bool {
        let mut registered_ids = self.registered_ids.lock();
        let mut spaces = self.space_fixtures.lock();
        if registered_ids.contains(id) || spaces.contains_key(id) {
            return false;
        }
        registered_ids.insert(id.to_owned());
        spaces.insert(id.to_owned(), name.to_owned());
        true
    }

    fn add_template_resource(&self, resource: TemplateFixtureResource) -> TestResult<()> {
        let claimed = self.registered_ids.lock().insert(resource.id().to_owned());
        if !claimed {
            return Err(template_fixture_error());
        }
        self.template_resources.lock().push(resource);
        Ok(())
    }

    fn owns_template_type(&self, space_id: &str, type_id: &str) -> bool {
        self.template_resources.lock().iter().any(|resource| {
            matches!(
                resource,
                TemplateFixtureResource::Type {
                    space_id: registered_space,
                    type_id: registered_type,
                    ..
                } if registered_space == space_id && registered_type == type_id
            )
        })
    }

    fn owns_template_source(&self, space_id: &str, type_id: &str, source_id: &str) -> bool {
        self.template_resources.lock().iter().any(|resource| {
            matches!(
                resource,
                TemplateFixtureResource::Source {
                    space_id: registered_space,
                    type_id: registered_type,
                    source_id: registered_source,
                } if registered_space == space_id
                    && registered_type == type_id
                    && registered_source == source_id
            )
        })
    }

    /// Deletes this file or folder after the test
    pub fn add_temp_path(&self, path: PathBuf) {
        self.temp_paths.lock().push(path);
    }

    /// Cleans up all remembered items.
    /// Child resources are deleted in reverse creation order and grouped as
    /// template-owned resources, objects, properties, then types. The
    /// deduplicated disposable-space registry is processed only after all child
    /// resources. Template-owned resources re-prove their exact type/source
    /// provenance before each destructive request. A failed child proof skips
    /// every remaining cleanup request for that owned type and returns a
    /// static redacted failure rather than risking deletion from stale state.
    pub async fn cleanup(&self, client: &AnytypeClient) -> TestResult<()> {
        let mut template_resources = {
            let mut guard = self.template_resources.lock();
            std::mem::take(&mut *guard)
        };
        template_resources.reverse();
        let mut template_cleanup_failed = false;
        let mut unproven_template_types = BTreeSet::new();
        for resource in template_resources {
            let owner = resource.owner();
            let owner = (owner.0.to_owned(), owner.1.to_owned());
            if unproven_template_types.contains(&owner) {
                template_cleanup_failed = true;
                continue;
            }
            if let Err(error) = cleanup_template_resource(client, &resource).await {
                eprintln!(
                    "cleanup-owned template fixture {} cleanup skipped: {error}",
                    resource.kind()
                );
                template_cleanup_failed = true;
                unproven_template_types.insert(owner);
            }
        }

        let chat_messages = {
            let mut guard = self.chat_messages.lock();
            std::mem::take(&mut *guard)
        };
        let mut chat_cleanup_failed = false;
        for (space_id, chat_id, message_id) in chat_messages.into_iter().rev() {
            if cleanup_chat_message(client, &space_id, &chat_id, &message_id)
                .await
                .is_err()
            {
                chat_cleanup_failed = true;
            }
        }

        let mut objects = {
            let mut guard = self.objects.lock();
            std::mem::take(&mut *guard)
        };
        objects.reverse();

        let mut ordinary_cleanup_failed = false;

        // First delete objects
        for (space_id, id, _) in objects
            .iter()
            .filter(|(_, _, model)| *model == DataModel::Object)
        {
            if client.object(space_id, id).delete().await.is_err() {
                ordinary_cleanup_failed = true;
            }
        }

        // then properties and tags
        for (space_id, prop_id, _) in objects
            .iter()
            .filter(|(_, _, model)| *model == DataModel::Property)
        {
            match client.tags(space_id, prop_id).list().await {
                Ok(tags) => match tags.collect_all().await {
                    Ok(tags) => {
                        for tag in tags {
                            if client
                                .tag(space_id, prop_id, tag.id)
                                .delete()
                                .await
                                .is_err()
                            {
                                ordinary_cleanup_failed = true;
                            }
                        }
                    }
                    Err(_) => ordinary_cleanup_failed = true,
                },
                Err(_) => ordinary_cleanup_failed = true,
            }
            if client.property(space_id, prop_id).delete().await.is_err() {
                ordinary_cleanup_failed = true;
            }
        }

        // then types
        for (space_id, type_id, _) in objects
            .iter()
            .filter(|(_, _, model)| *model == DataModel::Type)
        {
            if client.get_type(space_id, type_id).delete().await.is_err() {
                ordinary_cleanup_failed = true;
            }
        }
        self.collection_fixtures.lock().clear();
        self.collection_view_fixtures.lock().clear();
        self.kanban_tag_fixtures.lock().clear();

        // Delete disposable spaces only after their possible child resources.
        // SpaceDelete is irreversible, so this registry is private and can be
        // populated only by the create-and-register helper above.
        let space_fixtures = {
            let mut guard = self.space_fixtures.lock();
            std::mem::take(&mut *guard)
        };
        let mut space_cleanup_failed = false;
        for (space_id, expected_name) in space_fixtures.into_iter().rev() {
            if delete_space_fixture(client, &space_id, &expected_name)
                .await
                .is_err()
            {
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
        self.registered_ids.lock().clear();
        if template_cleanup_failed {
            return Err(template_cleanup_provenance_error());
        }
        if chat_cleanup_failed || ordinary_cleanup_failed {
            return Err(child_cleanup_error());
        }
        if space_cleanup_failed {
            return Err(space_cleanup_error());
        }
        Ok(())
    }
}

async fn cleanup_chat_message(
    client: &AnytypeClient,
    space_id: &str,
    chat_id: &str,
    message_id: &str,
) -> TestResult<()> {
    let chats = client.chats().in_space(space_id);
    match chats.delete_message(chat_id, message_id).await {
        Ok(())
        | Err(AnytypeError::NotFound { .. })
        | Err(AnytypeError::ApiError { code: 404, .. }) => {}
        Err(_) => return Err(child_cleanup_error()),
    }

    let verify = VerifyConfig {
        timeout: Duration::from_secs(10),
        initial_delay: Duration::from_millis(50),
        max_delay: Duration::from_millis(500),
        max_attempts: 20,
    };
    verify_semantic(
        &verify,
        "cleanup-owned chat message absence",
        message_id,
        || async {
            match chats.get_message(chat_id, message_id).get().await {
                Err(AnytypeError::NotFound { .. })
                | Err(AnytypeError::ApiError { code: 404, .. }) => Ok(()),
                Ok(_) => Err(AnytypeError::NotFound {
                    obj_type: "deleted chat message still visible".to_owned(),
                    key: String::new(),
                }),
                Err(error) => Err(error),
            }
        },
        |()| true,
    )
    .await
    .map_err(|_| child_cleanup_error())
}

fn child_cleanup_error() -> TestError {
    TestError::Assertion {
        message: "registered child-resource cleanup failed".to_owned(),
    }
}

fn child_registry_error() -> TestError {
    TestError::Assertion {
        message: "owned child lifecycle is unproven".to_owned(),
    }
}

async fn complete_type_object_id_snapshot(
    client: &AnytypeClient,
    space_id: &str,
    type_id: &str,
) -> TestResult<BTreeSet<String>> {
    let response = client
        .objects(space_id)
        .filter(Filter::type_in([type_id]))
        .limit(COLLECTION_VIEW_FIXTURE_SCAN_LIMIT)
        .offset(0)
        .list()
        .await
        .map_err(|_| collection_fixture_ownership_error())?
        .into_response();
    if response.pagination.offset != 0
        || response.pagination.has_more
        || response.pagination.total != response.items.len()
        || response.items.len() > COLLECTION_VIEW_FIXTURE_SCAN_LIMIT as usize
        || response.items.iter().any(|object| {
            object.space_id != space_id
                || object.r#type.as_ref().map(|typ| typ.id.as_str()) != Some(type_id)
        })
    {
        return Err(collection_fixture_ownership_error());
    }
    let ids = response
        .items
        .into_iter()
        .map(|object| object.id)
        .collect::<BTreeSet<_>>();
    if ids.len() != response.pagination.total {
        return Err(collection_fixture_ownership_error());
    }
    Ok(ids)
}

fn collection_fixture_ownership_error() -> TestError {
    TestError::Assertion {
        message: "cleanup-safe collection fixture ownership could not be established".to_owned(),
    }
}

#[derive(Deserialize)]
struct TypeFixtureDeleteResponse {
    #[serde(rename = "type")]
    type_: Type,
}

async fn cleanup_template_resource(
    client: &AnytypeClient,
    resource: &TemplateFixtureResource,
) -> TestResult<()> {
    let config = template_fixture_verify_config();
    match resource {
        TemplateFixtureResource::Template {
            space_id,
            type_id,
            source_id,
            template_id,
        } => {
            verify_template_source_cleanup_provenance(client, space_id, type_id, source_id)
                .await
                .map_err(|error| classify_template_cleanup(error, "template-source"))?;
            let template = verify_template_fixture(client, &config, space_id, type_id, template_id)
                .await
                .map_err(|error| classify_template_cleanup(error, "template-owner"))?;
            if template.id != *template_id || template.space_id != *space_id {
                return Err(template_cleanup_provenance_error());
            }
            let deleted = client
                .object(space_id, template_id)
                .delete_once()
                .await
                .map_err(|_| template_fixture_error())?;
            if deleted.id != *template_id || deleted.space_id != *space_id {
                return Err(template_fixture_error());
            }
            verify_semantic(
                &config,
                "deleted template fixture",
                template_id,
                || complete_template_ids(client, space_id, type_id),
                |ids| !ids.contains(template_id),
            )
            .await
            .map(|_| ())
            .map_err(|_| template_fixture_error())
        }
        TemplateFixtureResource::Source {
            space_id,
            type_id,
            source_id,
        } => {
            verify_template_source_cleanup_provenance(client, space_id, type_id, source_id)
                .await
                .map_err(|error| classify_template_cleanup(error, "source"))?;
            let deleted = client
                .object(space_id, source_id)
                .delete_once()
                .await
                .map_err(|_| template_fixture_error())?;
            if deleted.id != *source_id || deleted.space_id != *space_id {
                return Err(template_fixture_error());
            }
            verify_semantic(
                &config,
                "deleted template source fixture",
                source_id,
                || client.object(space_id, source_id).get(),
                |object| object.id == *source_id && object.space_id == *space_id && object.archived,
            )
            .await
            .map(|_| ())
            .map_err(|_| template_fixture_error())
        }
        TemplateFixtureResource::Type {
            space_id,
            type_id,
            type_key,
        } => {
            client
                .get_config()
                .limits
                .validate_id(space_id, "template fixture space")
                .map_err(|_| template_fixture_error())?;
            client
                .get_config()
                .limits
                .validate_id(type_id, "template fixture type")
                .map_err(|_| template_fixture_error())?;
            let typ = client
                .get_type(space_id, type_id)
                .get_direct()
                .await
                .map_err(|error| classify_template_cleanup(error, "type"))?;
            if typ.id != *type_id
                || typ.key != *type_key
                || typ.archived
                || typ.layout != ObjectLayout::Basic
            {
                return Err(template_cleanup_provenance_error());
            }
            let response: TypeFixtureDeleteResponse = client
                .client
                .delete_request_once(&format!("/v1/spaces/{space_id}/types/{type_id}"))
                .await
                .map_err(|_| template_fixture_error())?;
            if response.type_.id != *type_id {
                return Err(template_fixture_error());
            }
            verify_semantic(
                &config,
                "deleted template type fixture",
                type_id,
                || client.get_type(space_id, type_id).get_direct(),
                |typ| typ.id == *type_id && typ.archived,
            )
            .await
            .map(|_| ())
            .map_err(|_| template_fixture_error())
        }
    }
}

async fn verify_template_source_cleanup_provenance(
    client: &AnytypeClient,
    space_id: &str,
    type_id: &str,
    source_id: &str,
) -> Result<(), AnytypeError> {
    let ids = complete_owned_type_object_ids(client, space_id, type_id)
        .await
        .map_err(|error| {
            eprintln!(
                "cleanup-owned template fixture source inventory failed: {}",
                error.diagnostic()
            );
            template_fixture_api_error("cleanup-source-inventory")
        })?;
    if !ids.contains(source_id) {
        eprintln!("cleanup-owned template fixture source scope proof failed");
        return Err(template_fixture_api_error("cleanup-source-scope"));
    }
    let source = client.object(space_id, source_id).get().await?;
    if source.id != source_id
        || source.space_id != space_id
        || source.archived
        || source.object != DataModel::Object
        || source.r#type.as_ref().map(|typ| typ.id.as_str()) != Some(type_id)
    {
        eprintln!("cleanup-owned template fixture source identity proof failed");
        return Err(template_fixture_api_error("cleanup-source-identity"));
    }
    Ok(())
}

fn classify_template_cleanup(error: AnytypeError, stage: &'static str) -> TestError {
    eprintln!(
        "cleanup-owned template fixture {stage} provenance failed: {}",
        error.diagnostic()
    );
    template_cleanup_provenance_error()
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct SpaceInventoryIdentity {
    name: String,
    object: SpaceModel,
}

#[derive(Debug)]
struct CompleteSpaceInventory {
    by_id: BTreeMap<String, SpaceInventoryIdentity>,
}

/// Opaque pre-dispatch proof used to register an externally created test space.
#[doc(hidden)]
#[derive(Debug)]
pub struct PreparedSpaceFixtureClaim {
    expected_name: String,
    preexisting: CompleteSpaceInventory,
}

async fn complete_space_inventory(client: &AnytypeClient) -> TestResult<CompleteSpaceInventory> {
    let response = client
        .spaces()
        .limit(SPACE_FIXTURE_SCAN_LIMIT)
        .offset(0)
        .list()
        .await?
        .into_response();
    strict_space_inventory(&client.config.limits, response)
        .map_err(|_| space_fixture_ownership_error())
}

fn validate_and_register_owned_space_fixture(
    cleanup: &TestCleanup,
    limits: &crate::validation::ValidationLimits,
    current_space_id: &str,
    preexisting: &CompleteSpaceInventory,
    expected_name: &str,
    returned: &Space,
) -> TestResult<()> {
    limits
        .validate_id(&returned.id, "test space")
        .map_err(|_| space_fixture_ownership_error())?;
    validate_space_fixture_name(limits, &returned.name, "test space")
        .map_err(|_| space_fixture_ownership_error())?;
    if returned.id == current_space_id
        || preexisting.by_id.contains_key(&returned.id)
        || returned.name != expected_name
        || returned.object != SpaceModel::Space
    {
        // An untrusted duplicate response may leak a newly created server-side
        // space, but must never authorize deletion of pre-existing state.
        return Err(space_fixture_ownership_error());
    }
    if !cleanup.add_space_fixture(&returned.id, expected_name) {
        return Err(space_fixture_ownership_error());
    }
    Ok(())
}

async fn delete_space_fixture(
    client: &AnytypeClient,
    space_id: &str,
    expected_name: &str,
) -> TestResult<()> {
    client
        .config
        .limits
        .validate_id(space_id, "test space")
        .map_err(|_| space_cleanup_error())?;
    validate_space_fixture_name(&client.config.limits, expected_name, "test space")
        .map_err(|_| space_cleanup_error())?;

    let predelete = space_listing_evidence(client, space_id, Some(expected_name))
        .await
        .map_err(|_| space_cleanup_error())?;
    match plan_space_delete(&predelete)? {
        SpaceDeletePlan::AlreadyAbsent => return Ok(()),
        SpaceDeletePlan::DispatchOnce => {}
    }

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
    let acknowledged = commands
        .space_delete(request)
        .await
        .map(|response| response.into_inner())
        .map(|response| space_delete_succeeded(response.error.as_ref().map(|error| error.code)))
        .unwrap_or(false);
    prove_space_absent(client, space_id, acknowledged).await
}

async fn prove_space_absent(
    client: &AnytypeClient,
    space_id: &str,
    delete_acknowledged: bool,
) -> TestResult<()> {
    if !delete_acknowledged {
        eprintln!(
            "disposable test space delete response indeterminate: reconciling_by_complete_absence id={space_id}"
        );
    }
    let config = space_fixture_verify_config(client);
    verify_semantic(
        &config,
        "Deleted test space",
        space_id,
        || space_listing_evidence(client, space_id, None),
        |evidence| space_fixture_absence_result(evidence).is_ok(),
    )
    .await
    .map(|_| ())
    .map_err(|_| space_cleanup_error())
}

#[derive(Debug)]
struct SpaceListingEvidence {
    present: bool,
    name_matches: bool,
    object_matches: bool,
}

#[derive(Debug, PartialEq, Eq)]
enum SpaceDeletePlan {
    AlreadyAbsent,
    DispatchOnce,
}

fn plan_space_delete(evidence: &SpaceListingEvidence) -> TestResult<SpaceDeletePlan> {
    if !evidence.present {
        // A complete strict inventory already proves absence; no destructive
        // request is necessary.
        return Ok(SpaceDeletePlan::AlreadyAbsent);
    }
    if space_fixture_is_safe_to_delete(evidence) {
        Ok(SpaceDeletePlan::DispatchOnce)
    } else {
        Err(space_cleanup_error())
    }
}

fn space_fixture_is_safe_to_delete(evidence: &SpaceListingEvidence) -> bool {
    evidence.present && evidence.name_matches && evidence.object_matches
}

fn space_fixture_absence_result(evidence: &SpaceListingEvidence) -> TestResult<()> {
    if evidence.present {
        Err(space_cleanup_error())
    } else {
        Ok(())
    }
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
    let inventory = strict_space_inventory(&client.config.limits, response)?;
    let matching_space = inventory.by_id.get(space_id);
    let present = matching_space.is_some();
    let name_matches = expected_name
        .is_none_or(|expected| matching_space.is_some_and(|space| space.name == expected));
    Ok(SpaceListingEvidence {
        present,
        name_matches,
        object_matches: matching_space.is_none_or(|space| space.object == SpaceModel::Space),
    })
}

fn strict_space_inventory(
    limits: &crate::validation::ValidationLimits,
    response: crate::paged::PaginatedResponse<Space>,
) -> Result<CompleteSpaceInventory, AnytypeError> {
    if response.pagination.offset != 0
        || response.pagination.limit != SPACE_FIXTURE_SCAN_LIMIT
        || response.pagination.has_more
        || response.items.len() > SPACE_FIXTURE_SCAN_LIMIT as usize
        || response.pagination.total != response.items.len()
    {
        return Err(AnytypeError::Other {
            message: "space inventory pagination is incomplete".to_owned(),
        });
    }

    let expected_len = response.items.len();
    let mut by_id = BTreeMap::new();
    for space in response.items {
        limits.validate_id(&space.id, "space inventory id")?;
        validate_space_fixture_name(limits, &space.name, "space inventory name")?;
        if by_id
            .insert(
                space.id,
                SpaceInventoryIdentity {
                    name: space.name,
                    object: space.object,
                },
            )
            .is_some()
        {
            return Err(AnytypeError::Other {
                message: "space inventory contains duplicate ids".to_owned(),
            });
        }
    }
    if by_id.len() != expected_len {
        return Err(AnytypeError::Other {
            message: "space inventory identity count is inconsistent".to_owned(),
        });
    }
    Ok(CompleteSpaceInventory { by_id })
}

fn validate_space_fixture_name(
    limits: &crate::validation::ValidationLimits,
    name: &str,
    description: &str,
) -> Result<(), AnytypeError> {
    limits.validate_name(name, description)?;
    if name.chars().any(char::is_control) {
        return Err(AnytypeError::Validation {
            message: "test space identity names cannot contain control characters".to_owned(),
        });
    }
    Ok(())
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

async fn execute_space_create_after_intent<T, Record, Create, CreateFuture>(
    name: &str,
    record_intent: Record,
    create: Create,
) -> Result<T, AnytypeError>
where
    Record: FnOnce(&str),
    Create: FnOnce() -> CreateFuture,
    CreateFuture: std::future::Future<Output = Result<T, AnytypeError>>,
{
    record_intent(name);
    create().await
}

fn record_space_create_intent(name: &str) {
    let timestamp = Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true);
    eprintln!("disposable test space create intent: at={timestamp} name={name}");
}

fn space_cleanup_error() -> TestError {
    TestError::Assertion {
        message: "registered test space cleanup failed".to_owned(),
    }
}

fn classify_space_create_error(error: AnytypeError) -> TestError {
    let definitively_rejected = matches!(
        &error,
        AnytypeError::Validation { .. }
            | AnytypeError::Serialization { .. }
            | AnytypeError::Auth { .. }
            | AnytypeError::Unauthorized
            | AnytypeError::Forbidden
            | AnytypeError::NoKeyStore
            | AnytypeError::KeyStore { .. }
            | AnytypeError::NotFound { .. }
    ) || matches!(
        &error,
        AnytypeError::ApiError { code, .. } if (400..=499).contains(code)
    );
    if definitively_rejected {
        TestError::from(error)
    } else {
        TestError::SpaceCreateIndeterminate
    }
}

fn space_fixture_ownership_error() -> TestError {
    TestError::Assertion {
        message: "created test space ownership could not be established".to_owned(),
    }
}

#[cfg(test)]
fn space_cleanup_transport_error(_: tonic::Status) -> TestError {
    space_cleanup_error()
}

#[cfg(test)]
mod space_tests {
    use super::*;
    use crate::paged::{PaginatedResponse, PaginationMeta};

    const CURRENT_SPACE_ID: &str = "bafyreiafl45wf5eaxiby44pxrkhia3y5jsyix3ov2jzqiftsxjotujqlh4";
    const STALE_SPACE_ID: &str = "bafyreifmrdlvfk5uolhph6xmh6geta47auzqjilcsxarpyxlkrbqxks64a";
    const OWNED_SPACE_ID: &str = "bafyreie6n5l5nkbjal37su54cha4coy7qzuhrnajluzv5qd5jvtsrxkequ";

    const OWNED_SPACE_NAME: &str = "Automated test 123_0";

    fn registered_spaces(cleanup: &TestCleanup) -> BTreeMap<String, String> {
        cleanup.space_fixtures.lock().clone()
    }

    fn space(id: &str, name: &str) -> Space {
        Space {
            id: id.to_owned(),
            name: name.to_owned(),
            object: SpaceModel::Space,
            description: None,
            icon: None,
            gateway_url: None,
            network_id: None,
        }
    }

    fn empty_inventory() -> CompleteSpaceInventory {
        CompleteSpaceInventory {
            by_id: BTreeMap::new(),
        }
    }

    fn inventory_page(items: Vec<Space>) -> PaginatedResponse<Space> {
        let total = items.len();
        PaginatedResponse {
            items,
            pagination: PaginationMeta {
                has_more: false,
                limit: SPACE_FIXTURE_SCAN_LIMIT,
                offset: 0,
                total,
            },
        }
    }

    #[test]
    fn malformed_create_response_never_enters_deletion_registry() {
        let cleanup = TestCleanup::default();
        let returned = space("malformed-space-id", OWNED_SPACE_NAME);
        let result = validate_and_register_owned_space_fixture(
            &cleanup,
            &crate::validation::ValidationLimits::default(),
            CURRENT_SPACE_ID,
            &empty_inventory(),
            OWNED_SPACE_NAME,
            &returned,
        );
        assert!(result.is_err());
        assert!(registered_spaces(&cleanup).is_empty());
    }

    #[test]
    fn current_space_create_response_never_enters_deletion_registry() {
        let cleanup = TestCleanup::default();
        let returned = space(CURRENT_SPACE_ID, OWNED_SPACE_NAME);
        let result = validate_and_register_owned_space_fixture(
            &cleanup,
            &crate::validation::ValidationLimits::default(),
            CURRENT_SPACE_ID,
            &empty_inventory(),
            OWNED_SPACE_NAME,
            &returned,
        );
        assert!(result.is_err());
        assert!(registered_spaces(&cleanup).is_empty());
    }

    #[test]
    fn stale_create_response_never_enters_deletion_registry() {
        let cleanup = TestCleanup::default();
        let preexisting = strict_space_inventory(
            &crate::validation::ValidationLimits::default(),
            inventory_page(vec![space(STALE_SPACE_ID, "Ambient")]),
        )
        .unwrap();
        let returned = space(STALE_SPACE_ID, OWNED_SPACE_NAME);
        let result = validate_and_register_owned_space_fixture(
            &cleanup,
            &crate::validation::ValidationLimits::default(),
            CURRENT_SPACE_ID,
            &preexisting,
            OWNED_SPACE_NAME,
            &returned,
        );
        assert!(result.is_err());
        assert!(registered_spaces(&cleanup).is_empty());
    }

    #[test]
    fn duplicate_create_response_is_registered_for_at_most_one_delete() {
        let cleanup = TestCleanup::default();
        let limits = crate::validation::ValidationLimits::default();
        let returned = space(OWNED_SPACE_ID, OWNED_SPACE_NAME);
        assert!(
            validate_and_register_owned_space_fixture(
                &cleanup,
                &limits,
                CURRENT_SPACE_ID,
                &empty_inventory(),
                OWNED_SPACE_NAME,
                &returned,
            )
            .is_ok()
        );
        assert!(
            validate_and_register_owned_space_fixture(
                &cleanup,
                &limits,
                CURRENT_SPACE_ID,
                &empty_inventory(),
                OWNED_SPACE_NAME,
                &returned,
            )
            .is_err()
        );
        assert_eq!(
            registered_spaces(&cleanup),
            BTreeMap::from([(OWNED_SPACE_ID.to_owned(), OWNED_SPACE_NAME.to_owned())])
        );
    }

    #[test]
    fn mismatched_returned_name_or_model_never_grants_deletion_authority() {
        let limits = crate::validation::ValidationLimits::default();
        for returned in [
            space(OWNED_SPACE_ID, "Unexpected name"),
            Space {
                object: SpaceModel::Chat,
                ..space(OWNED_SPACE_ID, OWNED_SPACE_NAME)
            },
        ] {
            let cleanup = TestCleanup::default();
            assert!(
                validate_and_register_owned_space_fixture(
                    &cleanup,
                    &limits,
                    CURRENT_SPACE_ID,
                    &empty_inventory(),
                    OWNED_SPACE_NAME,
                    &returned,
                )
                .is_err()
            );
            assert!(registered_spaces(&cleanup).is_empty());
        }
    }

    #[test]
    fn strict_inventory_rejects_duplicate_malformed_and_incomplete_pages() {
        let limits = crate::validation::ValidationLimits::default();
        let valid = strict_space_inventory(
            &limits,
            inventory_page(vec![space(CURRENT_SPACE_ID, "Current")]),
        )
        .unwrap();
        assert_eq!(valid.by_id.len(), 1);

        let duplicate = inventory_page(vec![
            space(CURRENT_SPACE_ID, "Current"),
            space(CURRENT_SPACE_ID, "Duplicate"),
        ]);
        assert!(strict_space_inventory(&limits, duplicate).is_err());

        let malformed = inventory_page(vec![space("malformed-space-id", "Malformed")]);
        assert!(strict_space_inventory(&limits, malformed).is_err());

        let malformed_name = inventory_page(vec![space(CURRENT_SPACE_ID, "bad\0name")]);
        assert!(strict_space_inventory(&limits, malformed_name).is_err());

        let mut incomplete = inventory_page(vec![space(CURRENT_SPACE_ID, "Current")]);
        incomplete.pagination.total += 1;
        assert!(strict_space_inventory(&limits, incomplete).is_err());

        let mut continued = inventory_page(vec![space(CURRENT_SPACE_ID, "Current")]);
        continued.pagination.has_more = true;
        assert!(strict_space_inventory(&limits, continued).is_err());

        let mut wrong_limit = inventory_page(vec![space(CURRENT_SPACE_ID, "Current")]);
        wrong_limit.pagination.limit -= 1;
        assert!(strict_space_inventory(&limits, wrong_limit).is_err());

        let mut wrong_offset = inventory_page(vec![space(CURRENT_SPACE_ID, "Current")]);
        wrong_offset.pagination.offset = 1;
        assert!(strict_space_inventory(&limits, wrong_offset).is_err());

        let oversized = inventory_page(vec![
            space(CURRENT_SPACE_ID, "Current");
            SPACE_FIXTURE_SCAN_LIMIT as usize + 1
        ]);
        assert!(strict_space_inventory(&limits, oversized).is_err());
    }

    #[test]
    fn delete_protocol_requires_exact_identity_and_dispatches_at_most_once() {
        let absent = SpaceListingEvidence {
            present: false,
            name_matches: false,
            object_matches: true,
        };
        assert_eq!(
            plan_space_delete(&absent).unwrap(),
            SpaceDeletePlan::AlreadyAbsent
        );

        let wrong_name = SpaceListingEvidence {
            present: true,
            name_matches: false,
            object_matches: true,
        };
        assert!(plan_space_delete(&wrong_name).is_err());

        let wrong_model = SpaceListingEvidence {
            present: true,
            name_matches: true,
            object_matches: false,
        };
        assert!(plan_space_delete(&wrong_model).is_err());

        let exact = SpaceListingEvidence {
            present: true,
            name_matches: true,
            object_matches: true,
        };
        let mut delete_requests = 0;
        if plan_space_delete(&exact).unwrap() == SpaceDeletePlan::DispatchOnce {
            delete_requests += 1;
        }
        assert_eq!(delete_requests, 1);
    }

    #[test]
    fn acknowledged_and_indeterminate_delete_responses_require_the_same_absence_proof() {
        let absent = SpaceListingEvidence {
            present: false,
            name_matches: false,
            object_matches: true,
        };
        let persistent = SpaceListingEvidence {
            present: true,
            name_matches: true,
            object_matches: true,
        };
        for _delete_acknowledged in [true, false] {
            assert!(space_fixture_absence_result(&absent).is_ok());
            assert!(space_fixture_absence_result(&persistent).is_err());
        }
    }

    #[tokio::test]
    async fn create_intent_is_recorded_before_post_future_is_polled() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let intent_events = Arc::clone(&events);
        let post_events = Arc::clone(&events);
        execute_space_create_after_intent(
            OWNED_SPACE_NAME,
            move |_| intent_events.lock().push("intent"),
            move || async move {
                post_events.lock().push("post");
                Ok::<_, AnytypeError>(())
            },
        )
        .await
        .unwrap();
        assert_eq!(*events.lock(), vec!["intent", "post"]);
    }

    #[test]
    fn create_failures_are_classified_and_rendered_without_upstream_secrets() {
        const SECRET: &str = "create-response-secret-sentinel";
        let error = classify_space_create_error(AnytypeError::ResponseTooLarge {
            limit: 1,
            declared: Some(2),
        });
        assert!(matches!(error, TestError::SpaceCreateIndeterminate));

        let definitive = classify_space_create_error(AnytypeError::Validation {
            message: "invalid generated name".to_owned(),
        });
        assert!(matches!(definitive, TestError::Api { .. }));

        let definitive_4xx = classify_space_create_error(AnytypeError::ApiError {
            code: 418,
            method: "POST".to_owned(),
            url: "http://localhost/v1/spaces".to_owned(),
            message: SECRET.to_owned(),
        });
        assert!(matches!(definitive_4xx, TestError::Api { .. }));
        assert!(!definitive_4xx.to_string().contains(SECRET));

        let indeterminate_5xx = classify_space_create_error(AnytypeError::ApiError {
            code: 500,
            method: "POST".to_owned(),
            url: "http://localhost/v1/spaces".to_owned(),
            message: SECRET.to_owned(),
        });
        assert!(matches!(
            indeterminate_5xx,
            TestError::SpaceCreateIndeterminate
        ));
        assert!(!indeterminate_5xx.to_string().contains(SECRET));
    }

    #[test]
    fn ambient_prefix_test12_and_intent_only_names_never_authorize_delete() {
        let cleanup = TestCleanup::default();
        let limits = crate::validation::ValidationLimits::default();
        for returned in [
            space(CURRENT_SPACE_ID, OWNED_SPACE_NAME),
            space(OWNED_SPACE_ID, "Automated test"),
            space(OWNED_SPACE_ID, "test12"),
        ] {
            assert!(
                validate_and_register_owned_space_fixture(
                    &cleanup,
                    &limits,
                    CURRENT_SPACE_ID,
                    &empty_inventory(),
                    OWNED_SPACE_NAME,
                    &returned,
                )
                .is_err()
            );
        }
        assert!(registered_spaces(&cleanup).is_empty());
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
mod tests {
    use super::*;

    const COLLECTION_ID: &str = "bafyreig4ztsqf3f55gxm7wzh2z7njm4hzqvxwu7ha3frxfg5oimwnbzanu";
    const SPACE_ID: &str = "bafyreiafl45wf5eaxiby44pxrkhia3y5jsyix3ov2jzqiftsxjotujqlh4";
    const COLLECTION_TYPE_ID: &str = "bafyreic73elywzqx2m7ihtqnmrx7aqmobey4upchx7y4e2d4sc5cchzjiu";
    const DEFAULT_VIEW_ID: &str = "77dbd55c-5f52-4a5b-9d73-e1a46845dd45";
    const CREATED_VIEW_ID: &str = "9c4d60de-66bb-41b9-984e-ce750e4301e1";
    const BLOCK_ID: &str = "dataview";
    const KANBAN_RELATION_KEY: &str = "fixture_relation";
    const ARCHIVED_SOURCE_ID: &str = "bafyreie6n5l5nkbjal37su54cha4coy7qzuhrnajluzv5qd5jvtsrxkequ";

    fn kanban_status_property(format: PropertyFormat) -> Property {
        serde_json::from_value(serde_json::json!({
            "name": "Status",
            "key": "fixture_status",
            "id": "fixture-property",
            "format": format,
            "tags": null
        }))
        .expect("deserialize Kanban property fixture")
    }

    fn kanban_view_evidence() -> KanbanViewEvidence {
        KanbanViewEvidence {
            block_id: BLOCK_ID.to_owned(),
            view: DataviewView {
                r#type: anytype_rpc::model::block::content::dataview::view::Type::Kanban as i32,
                group_relation_key: KANBAN_RELATION_KEY.to_owned(),
                ..Default::default()
            },
            relation_links: vec![(
                KANBAN_RELATION_KEY.to_owned(),
                anytype_rpc::model::RelationFormat::Status as i32,
            )],
            rest_layout: ViewLayout::Kanban,
            rest_filters_empty: true,
        }
    }

    #[test]
    fn kanban_view_validation_fails_closed_for_relation_format_and_filters() {
        let property = kanban_status_property(PropertyFormat::Select);
        assert!(
            validate_kanban_view_evidence(&kanban_view_evidence(), &property, KANBAN_RELATION_KEY,)
                .is_ok()
        );

        let missing = KanbanViewEvidence {
            relation_links: Vec::new(),
            ..kanban_view_evidence()
        };
        assert!(validate_kanban_view_evidence(&missing, &property, KANBAN_RELATION_KEY).is_err());

        let wrong = kanban_status_property(PropertyFormat::Number);
        assert!(
            validate_kanban_view_evidence(&kanban_view_evidence(), &wrong, KANBAN_RELATION_KEY,)
                .is_err()
        );
        assert!(
            validate_kanban_view_evidence(&kanban_view_evidence(), &property, "wrong-relation")
                .is_err()
        );

        let filtered = KanbanViewEvidence {
            rest_filters_empty: false,
            ..kanban_view_evidence()
        };
        assert!(validate_kanban_view_evidence(&filtered, &property, KANBAN_RELATION_KEY).is_err());

        let mut proto_filtered = kanban_view_evidence();
        proto_filtered
            .view
            .filters
            .push(anytype_rpc::model::block::content::dataview::Filter::default());
        assert!(
            validate_kanban_view_evidence(&proto_filtered, &property, KANBAN_RELATION_KEY).is_err()
        );
    }

    #[test]
    fn kanban_child_claims_require_owned_parents_and_unique_ids() {
        let cleanup = TestCleanup::default();
        assert!(!cleanup.claim_kanban_tag_fixture(SPACE_ID, "property", "tag"));
        assert!(!cleanup.claim_collection_view_fixture(SPACE_ID, COLLECTION_ID, "view"));

        cleanup.add_property(SPACE_ID, "property");
        assert!(cleanup.claim_kanban_tag_fixture(SPACE_ID, "property", "tag"));
        assert!(!cleanup.claim_kanban_tag_fixture(SPACE_ID, "property", "tag"));

        assert!(cleanup.claim_collection_fixture(SPACE_ID, COLLECTION_ID, COLLECTION_TYPE_ID));
        assert!(cleanup.claim_collection_view_fixture(SPACE_ID, COLLECTION_ID, "view"));
        assert!(!cleanup.claim_collection_view_fixture(SPACE_ID, COLLECTION_ID, "view"));
    }

    #[test]
    fn chat_message_cleanup_registration_requires_one_owned_chat() {
        let cleanup = TestCleanup::default();
        assert!(!cleanup.add_chat_message(SPACE_ID, COLLECTION_ID, "message-1"));
        assert!(cleanup.chat_messages.lock().is_empty());

        cleanup.add_object(SPACE_ID, COLLECTION_ID);
        assert!(cleanup.add_chat_message(SPACE_ID, COLLECTION_ID, "message-1"));
        assert!(!cleanup.add_chat_message(SPACE_ID, COLLECTION_ID, "message-1"));
        assert!(!cleanup.add_chat_message(SPACE_ID, "other-chat", "message-2"));
        assert_eq!(
            cleanup
                .chat_messages
                .lock()
                .iter()
                .cloned()
                .collect::<Vec<_>>(),
            vec![(
                SPACE_ID.to_owned(),
                COLLECTION_ID.to_owned(),
                "message-1".to_owned()
            )]
        );
    }

    async fn template_paged_fixture_server(
        bodies: Vec<String>,
    ) -> (String, tokio::task::JoinHandle<Vec<String>>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind template fixture server");
        let address = listener.local_addr().expect("template fixture address");
        let task = tokio::spawn(async move {
            let mut requests = Vec::with_capacity(bodies.len());
            for body in bodies {
                let (mut stream, _) = listener.accept().await.expect("accept fixture request");
                let mut request = Vec::new();
                let mut buffer = [0_u8; 2048];
                loop {
                    let read = stream
                        .read(&mut buffer)
                        .await
                        .expect("read fixture request");
                    if read == 0 {
                        break;
                    }
                    request.extend_from_slice(&buffer[..read]);
                    if request.windows(4).any(|window| window == b"\r\n\r\n") {
                        break;
                    }
                }
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream
                    .write_all(response.as_bytes())
                    .await
                    .expect("write fixture response");
                requests.push(String::from_utf8(request).expect("fixture request is UTF-8"));
            }
            requests
        });
        (format!("http://{address}"), task)
    }

    fn template_fixture_http_client(base_url: String) -> AnytypeClient {
        let mut config = ClientConfig::default().app_name("template-owned-type-fixture");
        config.base_url = Some(base_url);
        config.keystore = Some("env".to_owned());
        let client = AnytypeClient::with_config(config).expect("template fixture client");
        client.set_api_key(crate::keystore::HttpCredentials::new("fixture-token"));
        client
    }

    fn owned_type_page(
        items: Vec<Object>,
        unrelated_archived_total: usize,
    ) -> crate::paged::PaginatedResponse<Object> {
        serde_json::from_str(&owned_type_page_json(items, unrelated_archived_total))
            .expect("owned-type page")
    }

    fn owned_type_page_json(items: Vec<Object>, unrelated_archived_total: usize) -> String {
        let total = items.len();
        serde_json::json!({
            "items": items,
            "pagination": {
                "has_more": false,
                "limit": TEMPLATE_FIXTURE_SCOPED_OBJECT_PAGE_LIMIT,
                "offset": 0,
                "total": total
            },
            "unrelated_archived_total": unrelated_archived_total
        })
        .to_string()
    }

    fn fixture_object(id: &str, archived: bool) -> Object {
        let mut object = collection_object(COLLECTION_TYPE_ID);
        object.id = id.to_owned();
        object.archived = archived;
        object
    }

    fn fixture_object_id(index: usize) -> String {
        let mut id = COLLECTION_ID.to_owned();
        let replacement = b"abcdefghijklmnopqrstuvwxyz234567"[index] as char;
        id.pop();
        id.push(replacement);
        id
    }

    fn rest_view(id: &str, name: &str) -> RestCollectionView {
        RestCollectionView {
            filters: Vec::new(),
            id: id.to_owned(),
            layout: crate::views::ViewLayout::Grid,
            name: name.to_owned(),
            sorts: Vec::new(),
        }
    }

    fn requested_view(id: &str, name: &str) -> DataviewView {
        DataviewView {
            id: id.to_owned(),
            name: name.to_owned(),
            ..Default::default()
        }
    }

    fn collection_object(type_id: &str) -> Object {
        Object {
            archived: false,
            icon: None,
            id: COLLECTION_ID.to_owned(),
            layout: ObjectLayout::Collection,
            markdown: None,
            name: Some("Fixture".to_owned()),
            object: DataModel::Object,
            properties: Vec::new(),
            snippet: None,
            space_id: SPACE_ID.to_owned(),
            r#type: Some(Type {
                archived: false,
                icon: None,
                id: type_id.to_owned(),
                key: "fixture-collection".to_owned(),
                layout: ObjectLayout::Collection,
                name: Some("Fixture collection".to_owned()),
                plural_name: Some("Fixture collections".to_owned()),
                properties: Vec::new(),
            }),
        }
    }

    fn create_response(id: &str, requested: &DataviewView) -> create_dataview_view::Response {
        let mut returned = requested.clone();
        returned.id = id.to_owned();
        create_dataview_view::Response {
            error: Some(create_dataview_view::response::Error {
                code: create_dataview_view::response::error::Code::Null as i32,
                description: String::new(),
            }),
            event: Some(anytype_rpc::anytype::ResponseEvent {
                messages: vec![anytype_rpc::anytype::event::Message {
                    space_id: SPACE_ID.to_owned(),
                    value: Some(EventValue::BlockDataviewViewSet(
                        anytype_rpc::anytype::event::block::dataview::ViewSet {
                            id: BLOCK_ID.to_owned(),
                            view_id: id.to_owned(),
                            view: Some(returned),
                        },
                    )),
                }],
                context_id: COLLECTION_ID.to_owned(),
                trace_id: String::new(),
            }),
            view_id: id.to_owned(),
        }
    }

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
        assert_eq!(rendered, "Anytype error (details redacted)");
        assert!(!rendered.contains(SECRET));
    }

    #[test]
    fn generic_pre_registered_id_cannot_claim_collection_provenance() {
        let cleanup = TestCleanup::default();
        cleanup.add_object(SPACE_ID, COLLECTION_ID);
        assert!(!cleanup.claim_collection_fixture(SPACE_ID, COLLECTION_ID, COLLECTION_TYPE_ID));
        assert_eq!(
            cleanup.collection_fixture_type_id(SPACE_ID, COLLECTION_ID),
            None
        );
        assert_eq!(cleanup.registered_ids.lock().len(), 1);
        assert_eq!(cleanup.objects.lock().len(), 1);
        assert!(cleanup.collection_fixtures.lock().is_empty());
    }

    #[test]
    fn duplicate_private_collection_claim_has_one_cleanup_dispatch() {
        let cleanup = TestCleanup::default();
        assert!(cleanup.claim_collection_fixture(SPACE_ID, COLLECTION_ID, COLLECTION_TYPE_ID));
        assert!(!cleanup.claim_collection_fixture(SPACE_ID, COLLECTION_ID, "different-type"));
        assert_eq!(
            cleanup.collection_fixture_type_id(SPACE_ID, COLLECTION_ID),
            Some(COLLECTION_TYPE_ID.to_owned())
        );
        assert_eq!(cleanup.registered_ids.lock().len(), 1);
        assert_eq!(cleanup.objects.lock().len(), 1);
        assert_eq!(cleanup.collection_fixtures.lock().len(), 1);
    }

    #[test]
    fn wrong_collection_type_is_rejected_before_object_show_rpc() {
        let collection = collection_object("wrong-type");
        assert!(!collection_matches_fixture_provenance(
            &collection,
            SPACE_ID,
            COLLECTION_ID,
            COLLECTION_TYPE_ID
        ));
        assert!(collection_matches_fixture_provenance(
            &collection_object(COLLECTION_TYPE_ID),
            SPACE_ID,
            COLLECTION_ID,
            COLLECTION_TYPE_ID
        ));
    }

    #[test]
    fn collection_view_fixture_clone_changes_only_id_and_name() {
        let default = DataviewView {
            id: DEFAULT_VIEW_ID.to_owned(),
            r#type: 3,
            name: "All".to_owned(),
            cover_relation_key: "cover".to_owned(),
            hide_icon: true,
            card_size: 2,
            cover_fit: true,
            group_relation_key: "group".to_owned(),
            group_background_colors: true,
            page_limit: 42,
            default_template_id: "template".to_owned(),
            default_object_type_id: "type".to_owned(),
            end_relation_key: "end".to_owned(),
            wrap_content: true,
            list_size: 2,
            alternate_rows: true,
            ..Default::default()
        };
        let cloned = clone_collection_view(&default, "request-id", "Second");
        let mut restored = cloned.clone();
        restored.id = default.id.clone();
        restored.name = default.name.clone();
        assert_eq!(restored, default);
        assert_eq!(cloned.id, "request-id");
        assert_eq!(cloned.name, "Second");
    }

    #[test]
    fn collection_view_fixture_accepts_exact_new_event_identity() {
        let existing = vec![rest_view(DEFAULT_VIEW_ID, "All")];
        let requested = requested_view("request-id", "Second");
        let response = create_response(CREATED_VIEW_ID, &requested);
        assert_eq!(
            validate_created_collection_view(
                &response,
                SPACE_ID,
                COLLECTION_ID,
                BLOCK_ID,
                "request-id",
                &requested,
                &existing,
            )
            .expect("exact response"),
            CREATED_VIEW_ID
        );
    }

    #[test]
    fn collection_view_fixture_rejects_preexisting_or_unproven_identity() {
        let existing = vec![rest_view(DEFAULT_VIEW_ID, "All")];
        let requested = requested_view("request-id", "Second");
        let duplicate = create_response(DEFAULT_VIEW_ID, &requested);
        assert!(
            validate_created_collection_view(
                &duplicate,
                SPACE_ID,
                COLLECTION_ID,
                BLOCK_ID,
                "request-id",
                &requested,
                &existing,
            )
            .is_err()
        );

        let mut missing_error = create_response(CREATED_VIEW_ID, &requested);
        missing_error.error = None;
        assert!(
            validate_created_collection_view(
                &missing_error,
                SPACE_ID,
                COLLECTION_ID,
                BLOCK_ID,
                "request-id",
                &requested,
                &existing,
            )
            .is_err()
        );

        let mut wrong_event = create_response(CREATED_VIEW_ID, &requested);
        wrong_event.event.as_mut().unwrap().messages[0].space_id = "wrong-space".to_owned();
        assert!(
            validate_created_collection_view(
                &wrong_event,
                SPACE_ID,
                COLLECTION_ID,
                BLOCK_ID,
                "request-id",
                &requested,
                &existing,
            )
            .is_err()
        );

        let sentinel = create_response("request-id", &requested);
        assert!(
            validate_created_collection_view(
                &sentinel,
                SPACE_ID,
                COLLECTION_ID,
                BLOCK_ID,
                "request-id",
                &requested,
                &existing,
            )
            .is_err()
        );

        let mut duplicate_event = create_response(CREATED_VIEW_ID, &requested);
        let duplicate_message = duplicate_event.event.as_ref().unwrap().messages[0].clone();
        duplicate_event
            .event
            .as_mut()
            .unwrap()
            .messages
            .push(duplicate_message);
        assert!(
            validate_created_collection_view(
                &duplicate_event,
                SPACE_ID,
                COLLECTION_ID,
                BLOCK_ID,
                "request-id",
                &requested,
                &existing,
            )
            .is_err()
        );

        let mut mutated_view = create_response(CREATED_VIEW_ID, &requested);
        let Some(EventValue::BlockDataviewViewSet(view_set)) =
            mutated_view.event.as_mut().unwrap().messages[0]
                .value
                .as_mut()
        else {
            panic!("view-set fixture")
        };
        view_set.view.as_mut().unwrap().hide_icon = true;
        assert!(
            validate_created_collection_view(
                &mutated_view,
                SPACE_ID,
                COLLECTION_ID,
                BLOCK_ID,
                "request-id",
                &requested,
                &existing,
            )
            .is_err()
        );
    }

    #[test]
    fn collection_view_fixture_cross_checks_every_rest_visible_field() {
        use anytype_rpc::model::{RelationFormat, block::content::dataview};

        let proto = DataviewView {
            id: DEFAULT_VIEW_ID.to_owned(),
            name: "All".to_owned(),
            filters: vec![dataview::Filter {
                id: "filter-id".to_owned(),
                relation_key: "name".to_owned(),
                condition: dataview::filter::Condition::Equal as i32,
                value: Some(string_value("alpha")),
                format: RelationFormat::Longtext as i32,
                ..Default::default()
            }],
            sorts: vec![dataview::Sort {
                id: "sort-id".to_owned(),
                relation_key: "name".to_owned(),
                r#type: dataview::sort::Type::Desc as i32,
                format: RelationFormat::Longtext as i32,
                ..Default::default()
            }],
            ..Default::default()
        };
        let exact = dataview_view_as_rest(&proto).expect("supported exact conversion");
        assert!(dataview_view_snapshot_matches(
            std::slice::from_ref(&proto),
            std::slice::from_ref(&exact)
        ));
        let mut changed = exact;
        changed.filters[0].value = "different".to_owned();
        assert!(!dataview_view_snapshot_matches(
            std::slice::from_ref(&proto),
            &[changed]
        ));
        let mut unsupported = proto;
        unsupported.sorts[0].format = RelationFormat::Emoji as i32;
        assert!(dataview_view_as_rest(&unsupported).is_err());
    }

    #[test]
    fn collection_view_fixture_binds_object_show_root_and_exact_block() {
        let default = requested_view(DEFAULT_VIEW_ID, "All");
        let rest = vec![dataview_view_as_rest(&default).unwrap()];
        let block = anytype_rpc::model::Block {
            id: COLLECTION_DATAVIEW_BLOCK_ID.to_owned(),
            content_value: Some(ContentValue::Dataview(
                anytype_rpc::model::block::content::Dataview {
                    is_collection: true,
                    views: vec![default],
                    ..Default::default()
                },
            )),
            ..Default::default()
        };
        assert!(
            resolve_collection_dataview(
                COLLECTION_ID,
                COLLECTION_ID,
                std::slice::from_ref(&block),
                &rest,
                DEFAULT_VIEW_ID,
            )
            .is_ok()
        );
        assert!(
            resolve_collection_dataview(
                "wrong-root",
                COLLECTION_ID,
                std::slice::from_ref(&block),
                &rest,
                DEFAULT_VIEW_ID,
            )
            .is_err()
        );
        let mut wrong_block = block;
        wrong_block.id = "other-dataview".to_owned();
        assert!(
            resolve_collection_dataview(
                COLLECTION_ID,
                COLLECTION_ID,
                &[wrong_block],
                &rest,
                DEFAULT_VIEW_ID,
            )
            .is_err()
        );
    }

    #[test]
    fn collection_view_response_description_is_redacted() {
        const SECRET: &str = "collection-view-response-secret";
        let existing = vec![rest_view(DEFAULT_VIEW_ID, "All")];
        let requested = requested_view("request-id", "Second");
        let mut response = create_response(CREATED_VIEW_ID, &requested);
        response.error = Some(create_dataview_view::response::Error {
            code: create_dataview_view::response::error::Code::UnknownError as i32,
            description: SECRET.to_owned(),
        });
        let rendered = validate_created_collection_view(
            &response,
            SPACE_ID,
            COLLECTION_ID,
            BLOCK_ID,
            "request-id",
            &requested,
            &existing,
        )
        .unwrap_err()
        .to_string();
        assert_eq!(
            rendered,
            "Test assertion failed: cleanup-safe collection view fixture creation failed"
        );
        assert!(!rendered.contains(SECRET));
    }

    #[test]
    fn collection_view_fixture_rejects_missing_default_without_indexing_it() {
        let block = anytype_rpc::model::Block {
            id: BLOCK_ID.to_owned(),
            content_value: Some(ContentValue::Dataview(
                anytype_rpc::model::block::content::Dataview {
                    is_collection: true,
                    views: Vec::new(),
                    ..Default::default()
                },
            )),
            ..Default::default()
        };
        let existing = vec![rest_view(DEFAULT_VIEW_ID, "All")];
        assert!(
            resolve_collection_dataview(
                COLLECTION_ID,
                COLLECTION_ID,
                &[block],
                &existing,
                DEFAULT_VIEW_ID,
            )
            .is_err()
        );
    }

    #[test]
    fn collection_view_fixture_requires_explicit_null_response_codes() {
        assert!(create_collection_view_succeeded(Some(
            create_dataview_view::response::error::Code::Null as i32
        )));
        assert!(!create_collection_view_succeeded(None));
        assert!(!create_collection_view_succeeded(Some(
            create_dataview_view::response::error::Code::UnknownError as i32
        )));
        assert!(object_show_succeeded(Some(
            object_show::response::error::Code::Null as i32
        )));
        assert!(!object_show_succeeded(None));
    }

    #[test]
    fn collection_view_fixture_transport_error_redacts_tonic_status() {
        const SECRET: &str = "collection-view-secret-sentinel";
        let error = collection_view_fixture_transport_error(tonic::Status::internal(SECRET));
        let rendered = error.to_string();
        assert_eq!(
            rendered,
            "Test assertion failed: cleanup-safe collection view fixture creation failed"
        );
        assert!(!rendered.contains(SECRET));
    }

    #[test]
    fn template_fixture_requires_explicit_null_and_redacts_response_description() {
        const SECRET: &str = "template-response-secret-sentinel";
        let success = template_create_from_object::response::Error {
            code: template_create_from_object::response::error::Code::Null as i32,
            description: String::new(),
        };
        assert!(template_fixture_response_succeeded(Some(&success)));
        assert!(!template_fixture_response_succeeded(None));

        let rejected = template_create_from_object::response::Error {
            code: template_create_from_object::response::error::Code::UnknownError as i32,
            description: SECRET.to_owned(),
        };
        assert!(!template_fixture_response_succeeded(Some(&rejected)));
        let rendered = template_fixture_response_error(Some(&rejected)).to_string();
        assert_eq!(
            rendered,
            "Test assertion failed: cleanup-owned template fixture operation failed"
        );
        assert!(!rendered.contains(SECRET));
    }

    #[test]
    fn template_fixture_transport_error_redacts_tonic_status() {
        const SECRET: &str = "template-transport-secret-sentinel";
        let rendered =
            template_fixture_transport_error(tonic::Status::internal(SECRET)).to_string();
        assert_eq!(
            rendered,
            "Test assertion failed: cleanup-owned template fixture operation failed"
        );
        assert!(!rendered.contains(SECRET));
    }

    #[test]
    fn template_fixture_registry_deduplicates_every_owned_id() {
        let cleanup = TestCleanup::default();
        cleanup
            .add_template_resource(TemplateFixtureResource::Type {
                space_id: "space".to_owned(),
                type_id: "owned-id".to_owned(),
                type_key: "owned-key".to_owned(),
            })
            .unwrap();
        assert!(
            cleanup
                .add_template_resource(TemplateFixtureResource::Source {
                    space_id: "space".to_owned(),
                    type_id: "type".to_owned(),
                    source_id: "owned-id".to_owned(),
                })
                .is_err()
        );
        assert_eq!(cleanup.template_resources.lock().len(), 1);
        assert_eq!(cleanup.registered_ids.lock().len(), 1);
    }

    #[tokio::test]
    async fn owned_type_snapshot_ignores_10196_unrelated_archives_and_bounds_both_queries() {
        const UNRELATED_ARCHIVED_TOTAL: usize = 10_196;
        let active = fixture_object(COLLECTION_ID, false);
        let archived = fixture_object(ARCHIVED_SOURCE_ID, true);
        let bodies = vec![
            owned_type_page_json(vec![active], UNRELATED_ARCHIVED_TOTAL),
            owned_type_page_json(vec![archived], UNRELATED_ARCHIVED_TOTAL),
        ];
        let (base_url, requests) = template_paged_fixture_server(bodies).await;
        let client = template_fixture_http_client(base_url);

        let snapshot = complete_template_source_snapshot_with_archived(
            &client,
            SPACE_ID,
            COLLECTION_TYPE_ID,
            || async {
                Ok(client
                    .objects(SPACE_ID)
                    .filter(Filter::type_in([COLLECTION_TYPE_ID]))
                    .filter(Filter::checkbox_true("archived"))
                    .limit(TEMPLATE_FIXTURE_SCOPED_OBJECT_PAGE_LIMIT)
                    .offset(0)
                    .list()
                    .await?
                    .into_response())
            },
        )
        .await
        .expect("unrelated archive population is outside owned-type evidence");
        assert_eq!(
            snapshot.object_ids,
            BTreeSet::from([COLLECTION_ID.to_owned(), ARCHIVED_SOURCE_ID.to_owned()])
        );

        let requests = requests.await.expect("owned-type requests");
        assert_eq!(requests.len(), 2);
        for request in &requests {
            let request_line = request.lines().next().expect("request line");
            assert!(request_line.contains("limit=17"));
            assert!(request_line.contains(COLLECTION_TYPE_ID));
        }
        assert!(!requests[0].lines().next().unwrap().contains("archived"));
        assert!(requests[1].lines().next().unwrap().contains("archived"));
        assert!(requests[1].lines().next().unwrap().contains("true"));
    }

    #[test]
    fn owned_type_pages_reject_archive_flags_overlap_and_seventeenth_row() {
        let client = template_fixture_http_client("http://127.0.0.1:1".to_owned());
        let empty = || owned_type_page(Vec::new(), 10_196);

        assert!(
            validate_owned_type_object(
                &client,
                &fixture_object(COLLECTION_ID, true),
                SPACE_ID,
                COLLECTION_TYPE_ID,
                Some(false),
            )
            .is_err()
        );

        let archived_active = owned_type_page(vec![fixture_object(COLLECTION_ID, false)], 10_196);
        assert!(
            owned_type_object_ids_from_pages(
                &client,
                SPACE_ID,
                COLLECTION_TYPE_ID,
                empty(),
                archived_active,
            )
            .is_err()
        );

        let mut archived_without_type = fixture_object(ARCHIVED_SOURCE_ID, true);
        archived_without_type.r#type = None;
        assert_eq!(
            owned_type_object_ids_from_pages(
                &client,
                SPACE_ID,
                COLLECTION_TYPE_ID,
                empty(),
                owned_type_page(vec![archived_without_type], 10_196),
            )
            .expect("exact-type archived query may omit returned type metadata"),
            BTreeSet::from([ARCHIVED_SOURCE_ID.to_owned()])
        );

        let mut archived_wrong_type = fixture_object(ARCHIVED_SOURCE_ID, true);
        archived_wrong_type.r#type.as_mut().unwrap().id = "wrong-type".to_owned();
        assert!(
            owned_type_object_ids_from_pages(
                &client,
                SPACE_ID,
                COLLECTION_TYPE_ID,
                empty(),
                owned_type_page(vec![archived_wrong_type], 10_196),
            )
            .is_err()
        );

        let active = owned_type_page(vec![fixture_object(COLLECTION_ID, false)], 10_196);
        let archived = owned_type_page(vec![fixture_object(COLLECTION_ID, true)], 10_196);
        assert!(
            owned_type_object_ids_from_pages(
                &client,
                SPACE_ID,
                COLLECTION_TYPE_ID,
                active,
                archived,
            )
            .is_err()
        );

        let overflow = (0..TEMPLATE_FIXTURE_SCOPED_OBJECT_PAGE_LIMIT as usize)
            .map(|index| fixture_object(&fixture_object_id(index), false))
            .collect();
        assert!(
            owned_type_object_ids_from_pages(
                &client,
                SPACE_ID,
                COLLECTION_TYPE_ID,
                owned_type_page(overflow, 10_196),
                empty(),
            )
            .is_err()
        );
    }

    #[tokio::test]
    async fn cleanup_skips_type_delete_when_exact_provenance_is_not_reverified() {
        let mut returned_type = collection_object(COLLECTION_TYPE_ID)
            .r#type
            .expect("fixture type");
        returned_type.key = "different-key".to_owned();
        let body = serde_json::json!({"type": returned_type}).to_string();
        let (base_url, requests) = template_paged_fixture_server(vec![body]).await;
        let client = template_fixture_http_client(base_url);
        let resource = TemplateFixtureResource::Type {
            space_id: SPACE_ID.to_owned(),
            type_id: COLLECTION_TYPE_ID.to_owned(),
            type_key: "expected-key".to_owned(),
        };

        let rendered = cleanup_template_resource(&client, &resource)
            .await
            .expect_err("mismatched type provenance must skip deletion")
            .to_string();
        assert_eq!(
            rendered,
            "Test assertion failed: cleanup-owned template fixture provenance re-verification failed"
        );
        let requests = requests.await.expect("provenance request");
        assert_eq!(requests.len(), 1);
        assert!(requests[0].starts_with(&format!(
            "GET /v1/spaces/{SPACE_ID}/types/{COLLECTION_TYPE_ID} HTTP/1.1"
        )));
    }

    #[tokio::test]
    async fn cleanup_reports_an_ordinary_child_delete_failure() {
        let (base_url, requests) = template_paged_fixture_server(vec!["{}".to_owned()]).await;
        let client = template_fixture_http_client(base_url);
        let cleanup = TestCleanup::default();
        cleanup.add_object(SPACE_ID, COLLECTION_ID);

        let rendered = cleanup
            .cleanup(&client)
            .await
            .expect_err("malformed delete response remains a cleanup defect")
            .to_string();
        assert_eq!(
            rendered,
            "Test assertion failed: registered child-resource cleanup failed"
        );
        let requests = requests.await.expect("object delete request");
        assert_eq!(requests.len(), 1);
        assert!(requests[0].starts_with(&format!(
            "DELETE /v1/spaces/{SPACE_ID}/objects/{COLLECTION_ID} HTTP/1.1"
        )));
    }

    #[test]
    fn owned_child_stoppers_run_in_reverse_and_retain_all_defects() {
        let order = Arc::new(Mutex::new(Vec::new()));
        let first_order = Arc::clone(&order);
        let second_order = Arc::clone(&order);
        let third_order = Arc::clone(&order);
        let stoppers: Vec<OwnedChildStopper> = vec![
            Box::new(move || {
                first_order.lock().push(1);
                Ok(())
            }),
            Box::new(move || {
                second_order.lock().push(2);
                Err(child_cleanup_error())
            }),
            Box::new(move || {
                third_order.lock().push(3);
                panic!("owned-child-stopper")
            }),
        ];
        let report = run_owned_child_stoppers(3, stoppers);
        assert_eq!(*order.lock(), vec![3, 2, 1]);
        assert_eq!(report.outcome, ChildOwnershipOutcome::Unproven);
        assert_eq!(report.errors.len(), 1);
        assert_eq!(report.panics.len(), 1);
    }

    #[test]
    fn owned_child_registry_is_atomic_sealed_and_never_equates_empty_with_stopped() {
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

        let marks = Arc::new(AtomicUsize::new(0));
        let marks_for_context = Arc::clone(&marks);
        let context = TestContext::for_disposable_space(
            template_fixture_http_client("http://127.0.0.1:1".to_owned()),
            SPACE_ID.to_owned(),
            None,
            Some(Arc::new(move || {
                marks_for_context.fetch_add(1, Ordering::SeqCst);
                Ok(())
            })),
        );
        let owned = context
            .spawn_owned_child(|| (7_u8, || Ok(())))
            .expect("spawn and registration are one operation");
        assert_eq!(owned, 7);
        assert_eq!(marks.load(Ordering::SeqCst), 1);
        let report = context.seal_and_stop_owned_children();
        assert_eq!(report.outcome, ChildOwnershipOutcome::Stopped);

        let late_spawn_ran = Arc::new(AtomicBool::new(false));
        let late_spawn_ran_in_closure = Arc::clone(&late_spawn_ran);
        assert!(
            context
                .spawn_owned_child(move || {
                    late_spawn_ran_in_closure.store(true, Ordering::SeqCst);
                    ((), || Ok(()))
                })
                .is_err()
        );
        assert!(!late_spawn_ran.load(Ordering::SeqCst));

        let no_children = TestContext::for_disposable_space(
            template_fixture_http_client("http://127.0.0.1:1".to_owned()),
            SPACE_ID.to_owned(),
            None,
            Some(Arc::new(|| Ok(()))),
        )
        .seal_and_stop_owned_children();
        assert_eq!(no_children.outcome, ChildOwnershipOutcome::NoChildren);
    }

    #[test]
    fn panicking_owned_spawn_is_recorded_as_unproven() {
        let context = TestContext::for_disposable_space(
            template_fixture_http_client("http://127.0.0.1:1".to_owned()),
            SPACE_ID.to_owned(),
            None,
            Some(Arc::new(|| Ok(()))),
        );
        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _: TestResult<()> = context
                .spawn_owned_child(|| -> ((), fn() -> TestResult<()>) { panic!("spawn-stage") });
        }));
        assert!(panic.is_err());
        let report = context.seal_and_stop_owned_children();
        assert_eq!(report.outcome, ChildOwnershipOutcome::Unproven);
        assert_eq!(report.errors.len(), 1);
    }

    #[test]
    fn template_source_requires_owned_type_exact_identity_and_fresh_scoped_id() {
        let cleanup = TestCleanup::default();
        let source = collection_object(COLLECTION_TYPE_ID);
        assert!(
            authorize_template_source(
                &cleanup,
                &TemplateOwnershipSnapshot::default(),
                SPACE_ID,
                COLLECTION_TYPE_ID,
                &source,
            )
            .is_err()
        );

        cleanup
            .add_template_resource(TemplateFixtureResource::Type {
                space_id: SPACE_ID.to_owned(),
                type_id: COLLECTION_TYPE_ID.to_owned(),
                type_key: "fixture-type".to_owned(),
            })
            .unwrap();
        let preexisting = TemplateOwnershipSnapshot {
            object_ids: BTreeSet::from([COLLECTION_ID.to_owned()]),
            ..TemplateOwnershipSnapshot::default()
        };
        assert!(
            authorize_template_source(
                &cleanup,
                &preexisting,
                SPACE_ID,
                COLLECTION_TYPE_ID,
                &source,
            )
            .is_err()
        );

        let wrong_type = collection_object("wrong-type");
        assert!(
            authorize_template_source(
                &cleanup,
                &TemplateOwnershipSnapshot::default(),
                SPACE_ID,
                COLLECTION_TYPE_ID,
                &wrong_type,
            )
            .is_err()
        );
        assert_eq!(cleanup.template_resources.lock().len(), 1);
        assert_eq!(cleanup.registered_ids.lock().len(), 1);
    }

    #[test]
    fn returned_template_is_registered_before_response_classification() {
        let cleanup = TestCleanup::default();
        cleanup
            .add_template_resource(TemplateFixtureResource::Type {
                space_id: SPACE_ID.to_owned(),
                type_id: COLLECTION_TYPE_ID.to_owned(),
                type_key: "fixture-type".to_owned(),
            })
            .unwrap();
        cleanup
            .add_template_resource(TemplateFixtureResource::Source {
                space_id: SPACE_ID.to_owned(),
                type_id: COLLECTION_TYPE_ID.to_owned(),
                source_id: COLLECTION_ID.to_owned(),
            })
            .unwrap();
        assert!(cleanup.owns_template_source(SPACE_ID, COLLECTION_TYPE_ID, COLLECTION_ID));
        assert!(!cleanup.owns_template_source(SPACE_ID, "wrong-type", COLLECTION_ID));

        authorize_owned_source_template(
            &cleanup,
            &TemplateOwnershipSnapshot::default(),
            SPACE_ID,
            COLLECTION_TYPE_ID,
            COLLECTION_ID,
            CREATED_VIEW_ID,
        )
        .expect("owned source authorizes returned template before classification");

        assert!(cleanup.registered_ids.lock().contains(CREATED_VIEW_ID));
        assert_eq!(cleanup.template_resources.lock().len(), 3);
    }

    #[test]
    fn template_evidence_diagnostics_are_static_and_redacted() {
        let rendered = template_fixture_evidence_error("owned-type-archived-page").to_string();
        assert_eq!(
            rendered,
            "Test assertion failed: cleanup-owned template fixture evidence failed: owned-type-archived-page"
        );
        assert!(!rendered.contains("upstream"));
        assert!(!rendered.contains("token"));

        let internal = template_fixture_api_error("owned-type-archived-page");
        assert_eq!(internal.to_string(), "Anytype error (details redacted)");
    }

    #[test]
    fn template_fixture_rejects_current_source_type_and_preexisting_ids() {
        for candidate in ["space", "type", "source"] {
            let cleanup = TestCleanup::default();
            let result = authorize_template_resource(
                &cleanup,
                &TemplateOwnershipSnapshot::default(),
                &["space", "type", "source"],
                TemplateFixtureResource::Template {
                    space_id: "space".to_owned(),
                    type_id: "type".to_owned(),
                    source_id: "source".to_owned(),
                    template_id: candidate.to_owned(),
                },
            );
            assert!(result.is_err());
            assert!(cleanup.template_resources.lock().is_empty());
            assert!(cleanup.registered_ids.lock().is_empty());
        }
    }

    #[test]
    fn template_fixture_rejects_cross_type_object_and_existing_template_or_type() {
        for snapshot in [
            TemplateOwnershipSnapshot {
                object_ids: BTreeSet::from(["candidate".to_owned()]),
                ..Default::default()
            },
            TemplateOwnershipSnapshot {
                template_ids: BTreeSet::from(["candidate".to_owned()]),
                ..Default::default()
            },
            TemplateOwnershipSnapshot {
                type_ids: BTreeSet::from(["candidate".to_owned()]),
                ..Default::default()
            },
        ] {
            let cleanup = TestCleanup::default();
            let result = authorize_template_resource(
                &cleanup,
                &snapshot,
                &["space", "type", "source"],
                TemplateFixtureResource::Template {
                    space_id: "space".to_owned(),
                    type_id: "type".to_owned(),
                    source_id: "source".to_owned(),
                    template_id: "candidate".to_owned(),
                },
            );
            assert!(result.is_err());
            assert!(cleanup.template_resources.lock().is_empty());
            assert!(cleanup.registered_ids.lock().is_empty());
        }
    }

    #[test]
    fn template_fixture_rejects_archived_object_id_without_cleanup_authorization() {
        let cleanup = TestCleanup::default();
        let snapshot = TemplateOwnershipSnapshot {
            // Whole-space snapshots merge the explicit archived surface into
            // this set before any create or RPC mutation is dispatched.
            object_ids: BTreeSet::from(["archived-candidate".to_owned()]),
            ..Default::default()
        };
        let result = authorize_template_resource(
            &cleanup,
            &snapshot,
            &[],
            TemplateFixtureResource::Source {
                space_id: "space".to_owned(),
                type_id: "type".to_owned(),
                source_id: "archived-candidate".to_owned(),
            },
        );
        assert!(result.is_err());
        assert!(cleanup.template_resources.lock().is_empty());
        assert!(cleanup.registered_ids.lock().is_empty());
    }

    #[test]
    fn template_fixture_cross_registry_collision_has_one_cleanup_dispatch() {
        let cleanup = TestCleanup::default();
        cleanup.add_object("space", "collision");
        let result = authorize_template_resource(
            &cleanup,
            &TemplateOwnershipSnapshot::default(),
            &[],
            TemplateFixtureResource::Template {
                space_id: "space".to_owned(),
                type_id: "type".to_owned(),
                source_id: "source".to_owned(),
                template_id: "collision".to_owned(),
            },
        );
        assert!(result.is_err());
        assert_eq!(cleanup.objects.lock().len(), 1);
        assert!(cleanup.template_resources.lock().is_empty());
        assert_eq!(cleanup.registered_ids.lock().len(), 1);

        cleanup.add_type("space", "collision");
        assert_eq!(cleanup.objects.lock().len(), 1);
        assert_eq!(cleanup.registered_ids.lock().len(), 1);

        let cleanup = TestCleanup::default();
        cleanup
            .add_template_resource(TemplateFixtureResource::Source {
                space_id: "space".to_owned(),
                type_id: "type".to_owned(),
                source_id: "collision".to_owned(),
            })
            .unwrap();
        cleanup.add_object("space", "collision");
        assert!(cleanup.objects.lock().is_empty());
        assert_eq!(cleanup.template_resources.lock().len(), 1);
        assert_eq!(cleanup.registered_ids.lock().len(), 1);
    }

    #[test]
    fn template_fixture_verification_bounds_are_fixed_and_finite() {
        let config = template_fixture_verify_config();
        assert_eq!(config.timeout, TEMPLATE_FIXTURE_VERIFY_TIMEOUT);
        assert_eq!(config.max_attempts, TEMPLATE_FIXTURE_VERIFY_ATTEMPTS);
        assert!(config.effective_max_attempts() > 0);
    }
}
