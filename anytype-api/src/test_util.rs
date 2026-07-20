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
            block_dataview::view::create as create_dataview_view,
            object::{create_object_type, show as object_show},
            space::delete as space_delete,
            template::create_from_object as template_create_from_object,
        },
    },
    model::{
        block::{ContentValue, content::dataview::View as DataviewView},
        object_type::Layout,
    },
};
use chrono::Utc;
use futures::FutureExt;
use parking_lot::Mutex;
use prost_types::{Struct, Value, value::Kind};
use serde::Deserialize;
use snafu::prelude::*;
use tonic::Request;

#[allow(unused_imports)]
use crate::prelude::{AnytypeClient, AnytypeError, ClientConfig, VerifyConfig};
use crate::{
    filters::{Filter, Query, QueryWithFilters},
    grpc_util::with_token_request,
    http_client::GetPaged,
    objects::{DataModel, Object, ObjectLayout},
    spaces::Space,
    types::{Type, TypeLayout},
    verify::verify_semantic,
    views::ViewLayout,
};

const COLLECTION_DATAVIEW_BLOCK_ID: &str = "dataview";
const COLLECTION_VIEW_FIXTURE_SCAN_LIMIT: u32 = 1_000;
const SPACE_FIXTURE_SCAN_LIMIT: u32 = 1_000;
const SPACE_FIXTURE_VERIFY_TIMEOUT: Duration = Duration::from_secs(20);
const SPACE_FIXTURE_VERIFY_ATTEMPTS: usize = 50;
const TEMPLATE_FIXTURE_LIMIT: u32 = 1_000;
const TEMPLATE_FIXTURE_OBJECT_LIMIT: usize = 10_000;
const TEMPLATE_FIXTURE_ARCHIVED_LIMIT: usize = 10_000;
const TEMPLATE_FIXTURE_GLOBAL_TEMPLATE_LIMIT: usize = 10_000;
const TEMPLATE_FIXTURE_MAX_SOURCES: usize = 16;
const TEMPLATE_FIXTURE_VERIFY_TIMEOUT: Duration = Duration::from_secs(20);
const TEMPLATE_FIXTURE_VERIFY_ATTEMPTS: usize = 50;
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

/// Identity returned for a cleanup-owned collection view fixture.
#[doc(hidden)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollectionViewFixture {
    /// Server-assigned view identifier.
    pub id: String,
    /// Exact requested view name.
    pub name: String,
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
        let created = self
            .client
            .new_object(&self.space_id, &exact_type.key)
            .name(&name)
            .no_verify()
            .create()
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

    /// Creates a custom type and cleanup-owned templates from new source objects.
    ///
    /// The custom type and every source object use the authenticated REST API
    /// without built-in verification. Complete bounded pre-create snapshots of
    /// every type, space-wide object and archived-object ID, and every template
    /// on every active type prove each returned ID was not pre-existing before
    /// cleanup registration or any fallible follow-up. Each source is converted
    /// with exactly one authenticated `TemplateCreateFromObject` RPC. The returned template ID
    /// must be new, distinct from the space/type/source IDs, and is
    /// deduplicated into the private cleanup registry before the RPC response
    /// code or any REST evidence is inspected.
    ///
    /// Creation succeeds only after a finite, complete type-scoped list and an
    /// exact template GET agree on every returned ID.
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

        let type_snapshot =
            complete_template_ownership_snapshot(&self.client, &self.space_id).await?;
        let type_key = format!("template_fixture_{}", unique_suffix());
        let created_type = self
            .client
            .new_type(&self.space_id, &type_name)
            .key(&type_key)
            .plural_name(format!("{type_name}s"))
            .layout(TypeLayout::Basic)
            .no_verify()
            .create()
            .await?;
        limits.validate_id(&created_type.id, "template fixture type")?;
        authorize_template_resource(
            &self.cleanup,
            &type_snapshot,
            &[self.space_id.as_str()],
            TemplateFixtureResource::Type {
                space_id: self.space_id.clone(),
                type_id: created_type.id.clone(),
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
                complete_template_ownership_snapshot(&self.client, &self.space_id).await?;
            let source = self
                .client
                .new_object(&self.space_id, &verified_type.key)
                .name(source_name)
                .no_verify()
                .create()
                .await?;
            limits.validate_id(&source.id, "template fixture source")?;
            authorize_template_resource(
                &self.cleanup,
                &source_snapshot,
                &[self.space_id.as_str(), verified_type.id.as_str()],
                TemplateFixtureResource::Source {
                    space_id: self.space_id.clone(),
                    source_id: source.id.clone(),
                },
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
                complete_template_ownership_snapshot(&self.client, &self.space_id).await?;
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
            authorize_template_resource(
                &self.cleanup,
                &template_snapshot,
                &[
                    self.space_id.as_str(),
                    verified_type.id.as_str(),
                    source.id.as_str(),
                ],
                TemplateFixtureResource::Template {
                    space_id: self.space_id.clone(),
                    type_id: verified_type.id.clone(),
                    template_id: response.id.clone(),
                },
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
            .await?;
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
        || !valid_collection_view_id(&response.view_id)
        || response.view_id == request_id
        || existing.iter().any(|view| view.id == response.view_id)
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

fn template_fixture_error() -> TestError {
    TestError::Assertion {
        message: "cleanup-owned template fixture operation failed".to_owned(),
    }
}

fn template_fixture_api_error() -> AnytypeError {
    AnytypeError::Other {
        message: "template fixture evidence was incomplete".to_owned(),
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
        return Err(template_fixture_api_error());
    }
    let mut templates = BTreeMap::new();
    for template in response.items {
        if !template_has_canonical_identity(client, &template, space_id)
            || templates.insert(template.id.clone(), template).is_some()
        {
            return Err(template_fixture_api_error());
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
        return Err(template_fixture_api_error());
    }
    let mut all_ids = BTreeSet::new();
    let mut active_ids = Vec::new();
    for typ in response.items {
        client
            .get_config()
            .limits
            .validate_id(&typ.id, "template fixture type evidence")?;
        if !all_ids.insert(typ.id.clone()) {
            return Err(template_fixture_api_error());
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

async fn complete_space_object_ids(
    client: &AnytypeClient,
    space_id: &str,
) -> Result<BTreeSet<String>, AnytypeError> {
    let mut all_ids = BTreeSet::new();
    let mut offset = 0u32;
    let mut expected_total = None;
    loop {
        let response = client
            .objects(space_id)
            .limit(TEMPLATE_FIXTURE_LIMIT)
            .offset(offset)
            .list()
            .await?
            .into_response();
        let total = response.pagination.total;
        if response.pagination.offset != offset
            || response.pagination.limit != TEMPLATE_FIXTURE_LIMIT
            || response.items.len() > TEMPLATE_FIXTURE_LIMIT as usize
            || total > TEMPLATE_FIXTURE_OBJECT_LIMIT
            || expected_total.is_some_and(|expected| expected != total)
            || (response.pagination.has_more && response.items.is_empty())
        {
            return Err(template_fixture_api_error());
        }
        expected_total = Some(total);
        for object in response.items {
            client
                .get_config()
                .limits
                .validate_id(&object.id, "template fixture object evidence")?;
            if object.space_id != space_id || !all_ids.insert(object.id) {
                return Err(template_fixture_api_error());
            }
        }
        if !response.pagination.has_more {
            if all_ids.len() != total {
                return Err(template_fixture_api_error());
            }
            break;
        }
        offset = offset
            .checked_add(TEMPLATE_FIXTURE_LIMIT)
            .ok_or_else(template_fixture_api_error)?;
    }

    let mut archived_ids = BTreeSet::new();
    let mut offset = 0u32;
    loop {
        let archived = client
            .list_archived(space_id)
            .limit(TEMPLATE_FIXTURE_LIMIT)
            .offset(offset)
            .list()
            .await?
            .into_response();
        let total = archived.pagination.total;
        if archived.pagination.offset != offset
            || archived.pagination.limit != TEMPLATE_FIXTURE_LIMIT
            || archived.items.len() > TEMPLATE_FIXTURE_LIMIT as usize
            || total > TEMPLATE_FIXTURE_ARCHIVED_LIMIT
            || total != offset as usize + archived.items.len()
            || (archived.pagination.has_more && archived.items.is_empty())
        {
            return Err(template_fixture_api_error());
        }
        for object in archived.items {
            client
                .get_config()
                .limits
                .validate_id(&object.id, "template fixture archived evidence")?;
            if object.space_id != space_id
                || !object.archived
                || !archived_ids.insert(object.id.clone())
            {
                return Err(template_fixture_api_error());
            }
            all_ids.insert(object.id);
        }
        if !archived.pagination.has_more {
            if archived_ids.len() != total {
                return Err(template_fixture_api_error());
            }
            break;
        }
        offset = offset
            .checked_add(TEMPLATE_FIXTURE_LIMIT)
            .ok_or_else(template_fixture_api_error)?;
    }
    Ok(all_ids)
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
            return Err(template_fixture_api_error());
        }
        for id in templates.into_keys() {
            if owners.insert(id, type_id.clone()).is_some() {
                return Err(template_fixture_api_error());
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

async fn complete_template_ownership_snapshot(
    client: &AnytypeClient,
    space_id: &str,
) -> Result<TemplateOwnershipSnapshot, AnytypeError> {
    let types = complete_type_inventory(client, space_id).await?;
    let object_ids = complete_space_object_ids(client, space_id).await?;
    let template_ids = complete_global_template_owners(client, space_id, &types.active_ids)
        .await?
        .into_keys()
        .collect();
    Ok(TemplateOwnershipSnapshot {
        type_ids: types.all_ids,
        object_ids,
        template_ids,
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
                .ok_or_else(template_fixture_api_error)?;
            let templates = complete_template_objects(client, space_id, type_id).await?;
            let listed = templates
                .get(template_id)
                .cloned()
                .ok_or_else(template_fixture_api_error)?;
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
    collection_fixtures: Mutex<BTreeSet<(String, String, String)>>,
    space_fixtures: Mutex<BTreeSet<String>>,
    template_resources: Mutex<Vec<TemplateFixtureResource>>,
    registered_ids: Mutex<BTreeSet<String>>,
    temp_paths: Mutex<Vec<PathBuf>>,
}

#[derive(Clone, Debug)]
enum TemplateFixtureResource {
    Type {
        space_id: String,
        type_id: String,
    },
    Source {
        space_id: String,
        source_id: String,
    },
    Template {
        space_id: String,
        type_id: String,
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
}

impl TestCleanup {
    pub fn is_empty(&self) -> bool {
        // Inspect each registry under its own guard. The collection claim is
        // the only operation that holds multiple registry locks at once.
        let objects_empty = self.objects.lock().is_empty();
        let collections_empty = self.collection_fixtures.lock().is_empty();
        let spaces_empty = self.space_fixtures.lock().is_empty();
        let templates_empty = self.template_resources.lock().is_empty();
        let registered_empty = self.registered_ids.lock().is_empty();
        let paths_empty = self.temp_paths.lock().is_empty();
        objects_empty
            && collections_empty
            && spaces_empty
            && templates_empty
            && registered_empty
            && paths_empty
    }

    /// Remembers this object for deletion after the test
    pub fn add_object(&self, space_id: &str, id: &str) {
        self.add_generic_resource(space_id, id, DataModel::Object);
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
        // This is the sole multi-registry critical section. Keep its lock order
        // authoritative IDs -> generic cleanup entries -> private provenance.
        // Every other registry method releases one guard before taking another.
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

    /// Remembers an exact space ID created by `TestContext::create_space_fixture`.
    fn add_space_fixture(&self, id: &str) -> bool {
        let claimed = self.registered_ids.lock().insert(id.to_owned());
        if !claimed {
            return false;
        }
        self.space_fixtures.lock().insert(id.into())
    }

    fn add_template_resource(&self, resource: TemplateFixtureResource) -> TestResult<()> {
        let claimed = self.registered_ids.lock().insert(resource.id().to_owned());
        if !claimed {
            return Err(template_fixture_error());
        }
        self.template_resources.lock().push(resource);
        Ok(())
    }

    /// Deletes this file or folder after the test
    pub fn add_temp_path(&self, path: PathBuf) {
        self.temp_paths.lock().push(path);
    }

    /// Cleans up all remembered items.
    /// Child resources are deleted in reverse creation order and grouped as
    /// template-owned resources, objects, properties, then types. The
    /// deduplicated disposable-space set is processed only after all child
    /// resources.
    pub async fn cleanup(&self, client: &AnytypeClient) -> TestResult<()> {
        let mut template_resources = {
            let mut guard = self.template_resources.lock();
            std::mem::take(&mut *guard)
        };
        template_resources.reverse();
        let mut template_cleanup_failed = false;
        for resource in template_resources {
            if cleanup_template_resource(client, &resource).await.is_err() {
                template_cleanup_failed = true;
            }
        }

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
        self.collection_fixtures.lock().clear();

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
        self.registered_ids.lock().clear();
        if template_cleanup_failed {
            return Err(template_fixture_error());
        }
        if space_cleanup_failed {
            return Err(space_cleanup_error());
        }
        Ok(())
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
            template_id,
        } => {
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
            source_id,
        } => {
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
        TemplateFixtureResource::Type { space_id, type_id } => {
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
mod tests {
    use super::*;

    const COLLECTION_ID: &str = "bafyreig4ztsqf3f55gxm7wzh2z7njm4hzqvxwu7ha3frxfg5oimwnbzanu";
    const SPACE_ID: &str = "bafyreiafl45wf5eaxiby44pxrkhia3y5jsyix3ov2jzqiftsxjotujqlh4";
    const COLLECTION_TYPE_ID: &str = "bafyreic73elywzqx2m7ihtqnmrx7aqmobey4upchx7y4e2d4sc5cchzjiu";
    const DEFAULT_VIEW_ID: &str = "77dbd55c-5f52-4a5b-9d73-e1a46845dd45";
    const CREATED_VIEW_ID: &str = "9c4d60de-66bb-41b9-984e-ce750e4301e1";
    const BLOCK_ID: &str = "dataview";

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
            })
            .unwrap();
        assert!(
            cleanup
                .add_template_resource(TemplateFixtureResource::Source {
                    space_id: "space".to_owned(),
                    source_id: "owned-id".to_owned(),
                })
                .is_err()
        );
        assert_eq!(cleanup.template_resources.lock().len(), 1);
        assert_eq!(cleanup.registered_ids.lock().len(), 1);
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
