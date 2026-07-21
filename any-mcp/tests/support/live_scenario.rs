// any-mcp - bounded, workflow-oriented MCP server for Anytype
//
// SPDX-FileCopyrightText: 2026 Steve Schoettler
// SPDX-License-Identifier: Apache-2.0

//! Transport-neutral scenarios and live-coverage ownership declarations.

use std::{collections::HashSet, future::Future, pin::Pin, time::Duration};

use anytype::{
    prelude::{Color, ObjectLayout, PropertyFormat},
    test_util::{TestContext, unique_suffix},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

/// Test-only transport seam used by both direct-router and stdio drivers.
pub trait McpDriver {
    fn call_tool<'a>(
        &'a mut self,
        name: &'static str,
        arguments: Value,
    ) -> Pin<Box<dyn Future<Output = Result<Value, String>> + 'a>>;

    fn call_tool_error<'a>(
        &'a mut self,
        name: &'static str,
        arguments: Value,
    ) -> Pin<Box<dyn Future<Output = Result<String, String>> + 'a>>;

    fn list_tools<'a>(
        &'a mut self,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<String>, String>> + 'a>>;

    fn list_resources<'a>(
        &'a mut self,
    ) -> Pin<Box<dyn Future<Output = Result<Value, String>> + 'a>>;

    fn list_resource_templates<'a>(
        &'a mut self,
    ) -> Pin<Box<dyn Future<Output = Result<Value, String>> + 'a>>;

    fn read_resource<'a>(
        &'a mut self,
        uri: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Value, String>> + 'a>>;
}

/// Stable identifiers for every executable live scenario.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ScenarioId {
    Discovery,
    Documents,
    Views,
    Mutations,
    Archive,
    #[cfg(test)]
    SyntheticNonExecutable,
}

impl ScenarioId {
    pub const EXECUTABLE: [Self; 5] = [
        Self::Discovery,
        Self::Documents,
        Self::Views,
        Self::Mutations,
        Self::Archive,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Discovery => "standard_discovery",
            Self::Documents => "standard_documents",
            Self::Views => "standard_views",
            Self::Mutations => "standard_mutations",
            Self::Archive => "standard_archive",
            #[cfg(test)]
            Self::SyntheticNonExecutable => "synthetic_non_executable",
        }
    }

    pub const fn is_executable(self) -> bool {
        match self {
            Self::Discovery | Self::Documents | Self::Views | Self::Mutations | Self::Archive => {
                true
            }
            #[cfg(test)]
            Self::SyntheticNonExecutable => false,
        }
    }
}

/// Bounded non-content evidence accumulated while a scenario builds fixtures.
#[derive(Debug)]
pub struct ScenarioEvidence {
    pub scenario: ScenarioId,
    pub fixture_ids: Vec<String>,
    redactions: Vec<String>,
}

impl ScenarioEvidence {
    pub fn new(scenario: ScenarioId) -> Self {
        Self {
            scenario,
            fixture_ids: Vec::new(),
            redactions: Vec::new(),
        }
    }

    pub fn fixture(&mut self, id: &str) {
        self.fixture_ids.push(id.to_owned());
    }

    pub fn sensitive(&mut self, value: &str) {
        if !value.is_empty() {
            self.redactions.push(value.to_owned());
        }
    }

    pub fn sanitize(&self, value: &str) -> String {
        let mut sanitized = value.to_owned();
        for secret in &self.redactions {
            sanitized = sanitized.replace(secret, "<redacted-content>");
        }
        const MAX_EVIDENCE_CHARS: usize = 16_384;
        sanitized.chars().take(MAX_EVIDENCE_CHARS).collect()
    }
}

/// Inputs owned by the fixture rather than by a transport driver.
pub struct DocumentFixture<'a> {
    pub space_id: &'a str,
    pub object_id: &'a str,
    pub name: &'a str,
    pub initial_body: &'a str,
    pub old_text: &'a str,
    pub new_text: &'a str,
}

/// Observable result used for an independent backend readback assertion.
pub struct DocumentScenarioEvidence {
    pub expected_body: String,
    pub edited_sha256: String,
}

/// Runs the compact document workflow through an arbitrary MCP transport.
pub async fn run_document_scenario(
    driver: &mut impl McpDriver,
    fixture: DocumentFixture<'_>,
) -> Result<DocumentScenarioEvidence, String> {
    let status = driver.call_tool("server_status", json!({})).await?;
    require(
        status["http_available"] == true,
        "server_status HTTP availability",
    )?;

    let mut found = false;
    for _ in 0..10 {
        let search = driver
            .call_tool(
                "object_search",
                json!({
                    "space": fixture.space_id,
                    "text": fixture.name,
                    "limit": 100
                }),
            )
            .await?;
        found = search["items"].as_array().is_some_and(|items| {
            items.iter().any(|item| {
                item.pointer("/summary/id").and_then(Value::as_str) == Some(fixture.object_id)
            })
        });
        if found {
            break;
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    require(found, "object_search observes the fixture object")?;

    let object = driver
        .call_tool(
            "object_get",
            json!({
                "space": fixture.space_id,
                "object_id": fixture.object_id,
                "body": {"max_chars": 100_000}
            }),
        )
        .await?;
    require(
        object.pointer("/object/summary/id").and_then(Value::as_str) == Some(fixture.object_id),
        "object_get identity",
    )?;
    require(
        object.pointer("/body/text").and_then(Value::as_str) == Some(fixture.initial_body),
        "object_get complete body",
    )?;
    let uri = required_string(&object, "/object/summary/resource_uri")?;

    let resource = driver.read_resource(&uri).await?;
    require(
        resource.pointer("/contents/0/uri").and_then(Value::as_str) == Some(uri.as_str()),
        "resources/read canonical URI",
    )?;
    require(
        resource.pointer("/contents/0/text").and_then(Value::as_str) == Some(fixture.initial_body),
        "resources/read complete body",
    )?;

    // Refresh the optimistic-concurrency token immediately before the edit.
    // A newly created Anytype document can still be converging while the
    // independent resource observation above completes.
    let current = driver
        .call_tool(
            "object_get",
            json!({
                "space": fixture.space_id,
                "object_id": fixture.object_id,
                "body": {"max_chars": 100_000}
            }),
        )
        .await?;
    require(
        current.pointer("/body/text").and_then(Value::as_str) == Some(fixture.initial_body),
        "object_get body remains stable before edit",
    )?;
    let body_sha256 = required_string(&current, "/body/sha256")?;
    let independently_hashed = Sha256::digest(fixture.initial_body.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    require(
        body_sha256 == independently_hashed,
        "object_get hash matches the complete observed body",
    )?;
    require(
        fixture.initial_body.match_indices(fixture.old_text).count() == 1,
        "fixture body contains exactly one edit match",
    )?;

    let edited = driver
        .call_tool(
            "object_edit",
            json!({
                "space": fixture.space_id,
                "object_id": fixture.object_id,
                "edits": [{
                    "old_text": fixture.old_text,
                    "new_text": fixture.new_text,
                    "expected_matches": 1
                }],
                "expected_body_sha256": body_sha256
            }),
        )
        .await?;
    let edited_sha256 = required_string(&edited, "/body_sha256")?;
    let expected_body = fixture
        .initial_body
        .replacen(fixture.old_text, fixture.new_text, 1);
    require(
        expected_body != fixture.initial_body,
        "fixture edit changes exactly one fragment",
    )?;
    Ok(DocumentScenarioEvidence {
        expected_body,
        edited_sha256,
    })
}

/// Heap-owned dispatch future for a complete live scenario.
pub type ScenarioFuture<'a> = Pin<Box<dyn Future<Output = Result<(), String>> + 'a>>;

/// Executes one complete standard-baseline scenario through the selected driver.
///
/// The erased, heap-owned return type is intentional: these fixture-heavy
/// debug futures exceed Tokio's worker-stack budget when their state is kept
/// inline by an ordinary `async fn` dispatcher.
pub fn run_scenario<'a>(
    scenario: ScenarioId,
    driver: &'a mut impl McpDriver,
    ctx: &'a TestContext,
    evidence: &'a mut ScenarioEvidence,
) -> ScenarioFuture<'a> {
    Box::pin(async move {
        match scenario {
            ScenarioId::Discovery => Box::pin(run_discovery(driver, ctx, evidence)).await,
            ScenarioId::Documents => Box::pin(run_documents(driver, ctx, evidence)).await,
            ScenarioId::Views => Box::pin(run_views(driver, ctx, evidence)).await,
            ScenarioId::Mutations => Box::pin(run_mutations(driver, ctx, evidence)).await,
            ScenarioId::Archive => Box::pin(run_archive(driver, ctx, evidence)).await,
            #[cfg(test)]
            ScenarioId::SyntheticNonExecutable => {
                Err("scenario is intentionally non-executable".to_owned())
            }
        }
    })
}

async fn run_discovery(
    driver: &mut impl McpDriver,
    ctx: &TestContext,
    evidence: &mut ScenarioEvidence,
) -> Result<(), String> {
    let status = driver.call_tool("server_status", json!({})).await?;
    require(
        status["http_available"] == true,
        "server_status HTTP availability",
    )?;

    let first_space = ctx
        .create_space_fixture(format!("MCP shared space {}", unique_suffix()))
        .await
        .map_err(|_| "create first disposable space fixture".to_owned())?;
    evidence.fixture(&first_space.id);
    let second_space = ctx
        .create_space_fixture(format!("MCP shared space {}", unique_suffix()))
        .await
        .map_err(|_| "create second disposable space fixture".to_owned())?;
    evidence.fixture(&second_space.id);

    let duplicate_name = format!("MCP shared ambiguous {}", unique_suffix());
    evidence.sensitive(&duplicate_name);
    let first_type = ctx
        .client
        .new_type(&ctx.space_id, &duplicate_name)
        .key(format!("mcp_shared_a_{}", unique_suffix()))
        .ensure_available()
        .create()
        .await
        .map_err(|_| "create first type fixture".to_owned())?;
    ctx.register_type(&first_type.id);
    evidence.fixture(&first_type.id);
    let second_type = ctx
        .client
        .new_type(&ctx.space_id, &duplicate_name)
        .key(format!("mcp_shared_b_{}", unique_suffix()))
        .ensure_available()
        .create()
        .await
        .map_err(|_| "create second type fixture".to_owned())?;
    ctx.register_type(&second_type.id);
    evidence.fixture(&second_type.id);

    let property = ctx
        .client
        .new_property(
            &ctx.space_id,
            format!("MCP shared select {}", unique_suffix()),
            PropertyFormat::Select,
        )
        .create()
        .await
        .map_err(|_| "create select property fixture".to_owned())?;
    ctx.register_property(&property.id);
    evidence.fixture(&property.id);
    let mut tag_ids = Vec::new();
    for (name, color) in [("First", Color::Blue), ("Second", Color::Red)] {
        let tag = ctx
            .client
            .new_tag(&ctx.space_id, &property.id)
            .name(format!("{name} {}", unique_suffix()))
            .color(color)
            .create()
            .await
            .map_err(|_| "create tag fixture".to_owned())?;
        evidence.fixture(&tag.id);
        tag_ids.push(tag.id);
    }

    let templates = ctx
        .create_template_fixtures(
            format!("MCP shared template type {}", unique_suffix()),
            [
                format!("MCP shared template A {}", unique_suffix()),
                format!("MCP shared template B {}", unique_suffix()),
            ],
        )
        .await
        .map_err(|_| "create template fixtures".to_owned())?;
    evidence.fixture(&templates.type_.id);
    let template_ids = templates
        .templates
        .iter()
        .map(|template| {
            evidence.fixture(&template.id);
            template.id.clone()
        })
        .collect::<Vec<_>>();

    let search_term = format!("McpSharedSearch{}", unique_suffix());
    evidence.sensitive(&search_term);
    let mut object_ids = Vec::new();
    for ordinal in ["first", "second"] {
        let object = create_object(ctx, &format!("{search_term} {ordinal}"), "").await?;
        evidence.fixture(&object.id);
        object_ids.push(object.id);
    }
    tokio::time::sleep(Duration::from_millis(300)).await;

    walk_pages(
        driver,
        "space_list",
        json!({}),
        &[first_space.id, second_space.id],
        1_000,
    )
    .await?;
    walk_pages(
        driver,
        "type_list",
        json!({"space": ctx.space_id}),
        &[first_type.id.clone(), second_type.id.clone()],
        1_000,
    )
    .await?;
    walk_pages(
        driver,
        "property_list",
        json!({"space": ctx.space_id}),
        std::slice::from_ref(&property.id),
        1_000,
    )
    .await?;
    walk_pages(
        driver,
        "tag_list",
        json!({"space": ctx.space_id, "property": property.id}),
        &tag_ids,
        32,
    )
    .await?;
    walk_pages(
        driver,
        "template_list",
        json!({"space": ctx.space_id, "type": templates.type_.id}),
        &template_ids,
        32,
    )
    .await?;
    walk_pages(
        driver,
        "object_search",
        json!({"space": ctx.space_id, "text": search_term}),
        &object_ids,
        32,
    )
    .await?;
    let ambiguity = driver
        .call_tool_error(
            "property_list",
            json!({"space": ctx.space_id, "type": duplicate_name, "limit": 1}),
        )
        .await?;
    require(ambiguity == "ambiguous", "ambiguous type resolution")
}

async fn run_documents(
    driver: &mut impl McpDriver,
    ctx: &TestContext,
    evidence: &mut ScenarioEvidence,
) -> Result<(), String> {
    let name = format!("MCP shared document {}", unique_suffix());
    let body = "gamma concurrent body";
    evidence.sensitive(&name);
    evidence.sensitive(body);
    evidence.sensitive("gamma");
    evidence.sensitive("delta");
    let object = create_object(ctx, &name, "").await?;
    evidence.fixture(&object.id);
    ctx.client
        .update_object(&ctx.space_id, &object.id)
        .body(body)
        .ensure_available()
        .update()
        .await
        .map_err(|_| "set document scenario body".to_owned())?;
    let initial = read_body(ctx, &object.id).await?;

    let resources = driver.list_resources().await?;
    require(
        resources["resources"] == json!([]),
        "resources/list is empty",
    )?;
    let templates = driver.list_resource_templates().await?;
    require(
        templates["resourceTemplates"][0]["uriTemplate"]
            == "anytype://spaces/{space_id}/objects/{object_id}",
        "resource template identity",
    )?;
    let result = run_document_scenario(
        driver,
        DocumentFixture {
            space_id: &ctx.space_id,
            object_id: &object.id,
            name: &name,
            initial_body: &initial,
            old_text: "gamma",
            new_text: "delta",
        },
    )
    .await?;
    let stored_body = read_body(ctx, &object.id).await?;
    require(
        stored_body == result.expected_body,
        "independent document edit readback",
    )?;
    let stored_sha256 = Sha256::digest(stored_body.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    require(
        stored_sha256 == result.edited_sha256,
        "edit result hash matches independent backend readback",
    )
}

async fn run_views(
    driver: &mut impl McpDriver,
    ctx: &TestContext,
    evidence: &mut ScenarioEvidence,
) -> Result<(), String> {
    let collection_type = ctx
        .create_collection_type_fixture(format!("MCP shared collection type {}", unique_suffix()))
        .await
        .map_err(|_| "create collection type fixture".to_owned())?;
    require(
        collection_type.layout == ObjectLayout::Collection,
        "collection fixture layout",
    )?;
    evidence.fixture(&collection_type.id);
    let collection = ctx
        .create_collection_fixture(
            &collection_type,
            format!("MCP shared collection {}", unique_suffix()),
        )
        .await
        .map_err(|_| "create collection fixture".to_owned())?;
    evidence.fixture(&collection.id);
    let second_view = ctx
        .create_collection_view_fixture(
            &collection.id,
            &format!("MCP shared second view {}", unique_suffix()),
        )
        .await
        .map_err(|_| "create second view fixture".to_owned())?;
    evidence.fixture(&second_view.id);
    let first = create_object(ctx, &format!("MCP view A {}", unique_suffix()), "").await?;
    let second = create_object(ctx, &format!("MCP view B {}", unique_suffix()), "").await?;
    evidence.fixture(&first.id);
    evidence.fixture(&second.id);
    ctx.client
        .view_add_objects(
            &ctx.space_id,
            &collection.id,
            vec![first.id.clone(), second.id.clone()],
        )
        .await
        .map_err(|_| "add collection members".to_owned())?;
    let views = ctx
        .client
        .list_views(&ctx.space_id, &collection.id)
        .limit(100)
        .offset(0)
        .list()
        .await
        .map_err(|_| "read collection views".to_owned())?
        .into_response();
    let view_ids = views
        .items
        .into_iter()
        .map(|view| view.id)
        .collect::<Vec<_>>();
    walk_pages(
        driver,
        "view_list",
        json!({"space": ctx.space_id, "list_id": collection.id}),
        &view_ids,
        16,
    )
    .await?;
    for _ in 0..10 {
        match walk_pages(
            driver,
            "view_object_list",
            json!({
                "space": ctx.space_id,
                "list_id": collection.id,
                "view": second_view.id
            }),
            &[first.id.clone(), second.id.clone()],
            16,
        )
        .await
        {
            Ok(()) => return Ok(()),
            Err(_) => tokio::time::sleep(Duration::from_millis(300)).await,
        }
    }
    Err("view_object_list did not converge".to_owned())
}

async fn run_mutations(
    driver: &mut impl McpDriver,
    ctx: &TestContext,
    evidence: &mut ScenarioEvidence,
) -> Result<(), String> {
    let suffix = unique_suffix();
    let name = format!("MCP shared mutation {suffix}");
    evidence.sensitive(&name);
    let create_input = json!({
        "space": ctx.space_id,
        "type": "page",
        "name": name,
        "idempotency_key": format!("mcp-shared-{suffix}")
    });
    let created = driver
        .call_tool("object_create", create_input.clone())
        .await?;
    let object_id = required_string(&created, "/object/id")?;
    ctx.register_object(&object_id);
    evidence.fixture(&object_id);
    let replay = driver.call_tool("object_create", create_input).await?;
    require(
        replay["object"]["id"] == object_id,
        "idempotent create replay identity",
    )?;
    let visible = ctx
        .client
        .object(&ctx.space_id, &object_id)
        .get()
        .await
        .map_err(|_| "read created object".to_owned())?;
    require(
        visible.name.as_deref() == Some(name.as_str()),
        "create readback",
    )?;

    let current = driver
        .call_tool(
            "object_get",
            json!({"space": ctx.space_id, "object_id": object_id, "body": {"max_chars": 100}}),
        )
        .await?;
    let updated_name = format!("MCP shared updated {suffix}");
    evidence.sensitive(&updated_name);
    driver
        .call_tool(
            "object_update",
            json!({
                "space": ctx.space_id,
                "object_id": object_id,
                "name": updated_name,
                "expected_body_sha256": current["body"]["sha256"]
            }),
        )
        .await?;
    let visible = ctx
        .client
        .object(&ctx.space_id, &object_id)
        .get()
        .await
        .map_err(|_| "read updated object".to_owned())?;
    require(
        visible.name.as_deref() == Some(updated_name.as_str()),
        "update readback",
    )?;

    ctx.client
        .update_object(&ctx.space_id, &object_id)
        .body("gamma concurrent body")
        .ensure_available()
        .update()
        .await
        .map_err(|_| "create concurrent body state".to_owned())?;
    let stale = driver
        .call_tool_error(
            "object_edit",
            json!({
                "space": ctx.space_id,
                "object_id": object_id,
                "edits": [{"old_text": "gamma", "new_text": "stale"}],
                "expected_body_sha256": current["body"]["sha256"]
            }),
        )
        .await?;
    require(stale == "conflict", "stale edit conflict")?;
    let fresh = driver
        .call_tool(
            "object_get",
            json!({"space": ctx.space_id, "object_id": object_id, "body": {"max_chars": 100}}),
        )
        .await?;
    let count = driver
        .call_tool_error(
            "object_edit",
            json!({
                "space": ctx.space_id,
                "object_id": object_id,
                "edits": [{"old_text": "absent", "new_text": "never", "expected_matches": 1}],
                "expected_body_sha256": fresh["body"]["sha256"]
            }),
        )
        .await?;
    require(count == "conflict", "match-count conflict")?;
    driver
        .call_tool(
            "object_edit",
            json!({
                "space": ctx.space_id,
                "object_id": object_id,
                "edits": [{"old_text": "gamma", "new_text": "delta"}],
                "expected_body_sha256": fresh["body"]["sha256"]
            }),
        )
        .await?;
    require(
        read_body(ctx, &object_id)
            .await?
            .contains("delta concurrent body"),
        "edit readback",
    )
}

async fn run_archive(
    driver: &mut impl McpDriver,
    ctx: &TestContext,
    evidence: &mut ScenarioEvidence,
) -> Result<(), String> {
    let type_key = format!("mcp_shared_archive_{}", unique_suffix());
    let archive_type = ctx
        .client
        .new_type(
            &ctx.space_id,
            format!("MCP shared archive type {}", unique_suffix()),
        )
        .key(&type_key)
        .ensure_available()
        .create()
        .await
        .map_err(|_| "create archive type fixture".to_owned())?;
    ctx.register_type(&archive_type.id);
    evidence.fixture(&archive_type.id);
    let object = ctx
        .client
        .new_object(&ctx.space_id, &type_key)
        .name(format!("MCP shared archive {}", unique_suffix()))
        .ensure_available()
        .create()
        .await
        .map_err(|_| "create archive object fixture".to_owned())?;
    ctx.register_object(&object.id);
    evidence.fixture(&object.id);
    let type_id = object
        .r#type
        .as_ref()
        .map(|value| value.id.clone())
        .ok_or_else(|| "archive fixture type".to_owned())?;
    let result = driver
        .call_tool(
            "object_archive",
            json!({"space": ctx.space_id, "object_id": object.id}),
        )
        .await?;
    require(result["archived"] == true, "archive result")?;
    for _ in 0..10 {
        let active = active_contains(ctx, &object.id, &type_id).await?;
        let archived = archived_contains(ctx, &object.id, &type_id).await?;
        if !active && archived {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    Err("archive evidence did not converge".to_owned())
}

async fn active_contains(
    ctx: &TestContext,
    object_id: &str,
    type_id: &str,
) -> Result<bool, String> {
    let page = ctx
        .client
        .objects(&ctx.space_id)
        .filter(anytype::prelude::Filter::type_in([type_id.to_owned()]))
        .limit(100)
        .offset(0)
        .list()
        .await
        .map_err(|_| "read active archive evidence".to_owned())?;
    require(
        !page.pagination.has_more,
        "unique archive type unexpectedly exceeds active evidence page",
    )?;
    Ok(page
        .items
        .iter()
        .any(|object| object.id == object_id && !object.archived))
}

async fn archived_contains(
    ctx: &TestContext,
    object_id: &str,
    type_id: &str,
) -> Result<bool, String> {
    let page = ctx
        .client
        .list_archived(&ctx.space_id)
        .types([type_id])
        .limit(100)
        .offset(0)
        .list()
        .await
        .map_err(|_| "read archived evidence".to_owned())?;
    require(
        !page.pagination.has_more,
        "unique archive type unexpectedly exceeds archived evidence page",
    )?;
    Ok(page.items.iter().any(|object| object.id == object_id))
}

async fn create_object(
    ctx: &TestContext,
    name: &str,
    body: &str,
) -> Result<anytype::prelude::Object, String> {
    let object = ctx
        .client
        .new_object(&ctx.space_id, "page")
        .name(name)
        .body(body)
        .ensure_available()
        .create()
        .await
        .map_err(|_| "create live object fixture".to_owned())?;
    ctx.register_object(&object.id);
    Ok(object)
}

async fn read_body(ctx: &TestContext, object_id: &str) -> Result<String, String> {
    ctx.client
        .object(&ctx.space_id, object_id)
        .get()
        .await
        .map_err(|_| "read live object fixture".to_owned())
        .map(|object| object.markdown.unwrap_or_default())
}

async fn walk_pages(
    driver: &mut impl McpDriver,
    tool: &'static str,
    base: Value,
    expected_ids: &[String],
    max_pages: usize,
) -> Result<(), String> {
    let mut cursor = None;
    let mut seen_cursors = HashSet::new();
    let mut seen_ids = HashSet::new();
    let mut binding_checked = false;
    for _ in 0..max_pages {
        let mut input = base
            .as_object()
            .cloned()
            .ok_or_else(|| "page input must be an object".to_owned())?;
        input.insert("limit".to_owned(), json!(1));
        if let Some(cursor) = &cursor {
            input.insert("cursor".to_owned(), json!(cursor));
        }
        let page = driver.call_tool(tool, Value::Object(input.clone())).await?;
        for item in page["items"]
            .as_array()
            .ok_or_else(|| format!("{tool} items array"))?
        {
            if let Some(id) = item_id(item) {
                require(
                    seen_ids.insert(id.to_owned()),
                    &format!("{tool} item progress"),
                )?;
            }
        }
        let Some(next) = page.get("next_cursor").and_then(Value::as_str) else {
            for id in expected_ids {
                require(
                    seen_ids.contains(id),
                    &format!("{tool} observes fixture {id}"),
                )?;
            }
            return Ok(());
        };
        require(
            seen_cursors.insert(next.to_owned()),
            &format!("{tool} cursor progress"),
        )?;
        if !binding_checked {
            let mut mismatch = input;
            mismatch.insert("limit".to_owned(), json!(2));
            mismatch.insert("cursor".to_owned(), json!(next));
            let code = driver
                .call_tool_error(tool, Value::Object(mismatch))
                .await?;
            require(code == "validation", &format!("{tool} cursor binding"))?;
            binding_checked = true;
        }
        cursor = Some(next.to_owned());
    }
    Err(format!("{tool} did not terminate within {max_pages} pages"))
}

fn item_id(item: &Value) -> Option<&str> {
    item.get("id")
        .or_else(|| item.pointer("/summary/id"))
        .or_else(|| item.pointer("/object/id"))
        .and_then(Value::as_str)
}

/// Closed inventory of standard tool and resource operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum LiveOperation {
    ObjectArchive,
    ObjectCreate,
    ObjectEdit,
    ObjectGet,
    ObjectSearch,
    ObjectUpdate,
    PropertyList,
    ServerStatus,
    SpaceList,
    TagList,
    TemplateList,
    TypeList,
    ViewList,
    ViewObjectList,
    ResourcesList,
    ResourcesRead,
    ResourcesTemplatesList,
}

/// Typed binding from one advertised operation to one executable scenario.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ownership {
    pub operation: LiveOperation,
    pub scenario: ScenarioId,
}

pub const LIVE_OWNERSHIP: &[Ownership] = &[
    own(LiveOperation::ObjectArchive, ScenarioId::Archive),
    own(LiveOperation::ObjectCreate, ScenarioId::Mutations),
    own(LiveOperation::ObjectEdit, ScenarioId::Documents),
    own(LiveOperation::ObjectGet, ScenarioId::Documents),
    own(LiveOperation::ObjectSearch, ScenarioId::Documents),
    own(LiveOperation::ObjectUpdate, ScenarioId::Mutations),
    own(LiveOperation::PropertyList, ScenarioId::Discovery),
    own(LiveOperation::ServerStatus, ScenarioId::Discovery),
    own(LiveOperation::SpaceList, ScenarioId::Discovery),
    own(LiveOperation::TagList, ScenarioId::Discovery),
    own(LiveOperation::TemplateList, ScenarioId::Discovery),
    own(LiveOperation::TypeList, ScenarioId::Discovery),
    own(LiveOperation::ViewList, ScenarioId::Views),
    own(LiveOperation::ViewObjectList, ScenarioId::Views),
    own(LiveOperation::ResourcesList, ScenarioId::Documents),
    own(LiveOperation::ResourcesRead, ScenarioId::Documents),
    own(LiveOperation::ResourcesTemplatesList, ScenarioId::Documents),
];

const fn own(operation: LiveOperation, scenario: ScenarioId) -> Ownership {
    Ownership {
        operation,
        scenario,
    }
}

fn parse_tool(name: &str) -> Option<LiveOperation> {
    Some(match name {
        "object_archive" => LiveOperation::ObjectArchive,
        "object_create" => LiveOperation::ObjectCreate,
        "object_edit" => LiveOperation::ObjectEdit,
        "object_get" => LiveOperation::ObjectGet,
        "object_search" => LiveOperation::ObjectSearch,
        "object_update" => LiveOperation::ObjectUpdate,
        "property_list" => LiveOperation::PropertyList,
        "server_status" => LiveOperation::ServerStatus,
        "space_list" => LiveOperation::SpaceList,
        "tag_list" => LiveOperation::TagList,
        "template_list" => LiveOperation::TemplateList,
        "type_list" => LiveOperation::TypeList,
        "view_list" => LiveOperation::ViewList,
        "view_object_list" => LiveOperation::ViewObjectList,
        _ => return None,
    })
}

fn parse_resource(name: &str) -> Option<LiveOperation> {
    Some(match name {
        "resources/list" => LiveOperation::ResourcesList,
        "resources/read" => LiveOperation::ResourcesRead,
        "resources/templates/list" => LiveOperation::ResourcesTemplatesList,
        _ => return None,
    })
}

/// Validates exact, unique, executable live ownership for the production catalog.
pub fn validate_live_ownership(
    expected_tools: &[&str],
    expected_resources: &[&str],
) -> Result<(), String> {
    validate_ownership(expected_tools, expected_resources, LIVE_OWNERSHIP)
}

fn validate_ownership(
    expected_tools: &[&str],
    expected_resources: &[&str],
    owners: &[Ownership],
) -> Result<(), String> {
    let mut expected = HashSet::new();
    for name in expected_tools {
        let operation =
            parse_tool(name).ok_or_else(|| format!("unknown advertised tool operation: {name}"))?;
        expected.insert(operation);
    }
    for name in expected_resources {
        let operation = parse_resource(name)
            .ok_or_else(|| format!("unknown advertised resource operation: {name}"))?;
        expected.insert(operation);
    }
    let mut seen = HashSet::new();
    for owner in owners {
        if !expected.contains(&owner.operation) {
            return Err(format!(
                "unknown live operation owner: {:?}",
                owner.operation
            ));
        }
        if !seen.insert(owner.operation) {
            return Err(format!(
                "duplicate live operation owner: {:?}",
                owner.operation
            ));
        }
        if !owner.scenario.is_executable() || !ScenarioId::EXECUTABLE.contains(&owner.scenario) {
            return Err(format!(
                "non-executable live scenario owner: {}",
                owner.scenario.as_str()
            ));
        }
    }
    let mut missing = expected.difference(&seen).copied().collect::<Vec<_>>();
    missing.sort_unstable();
    if let Some(operation) = missing.first() {
        return Err(format!("missing live operation owner: {operation:?}"));
    }
    Ok(())
}

fn required_string(value: &Value, pointer: &str) -> Result<String, String> {
    value
        .pointer(pointer)
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .ok_or_else(|| format!("missing string at {pointer}"))
}

fn require(condition: bool, message: &str) -> Result<(), String> {
    condition.then_some(()).ok_or_else(|| message.to_owned())
}

#[cfg(test)]
mod ownership_tests {
    use super::*;

    // This compile-time assignment ensures callers cannot accidentally regain
    // the large inline dispatcher future that overflowed the live-test worker.
    #[allow(dead_code)]
    fn assert_heap_owned_dispatch<'a, D: McpDriver>(
        driver: &'a mut D,
        ctx: &'a TestContext,
        evidence: &'a mut ScenarioEvidence,
    ) {
        let future: ScenarioFuture<'a> = run_scenario(ScenarioId::Discovery, driver, ctx, evidence);
        std::mem::drop(future);
    }

    const TOOLS: &[&str] = &["server_status", "object_get"];
    const RESOURCES: &[&str] = &["resources/read"];
    const COMPLETE: &[Ownership] = &[
        own(LiveOperation::ServerStatus, ScenarioId::Discovery),
        own(LiveOperation::ObjectGet, ScenarioId::Documents),
        own(LiveOperation::ResourcesRead, ScenarioId::Documents),
    ];

    #[test]
    fn synthetic_missing_operation_fails_deterministically() {
        let error = validate_ownership(TOOLS, RESOURCES, &COMPLETE[..2]).unwrap_err();
        assert_eq!(error, "missing live operation owner: ResourcesRead");
    }

    #[test]
    fn scenario_dispatch_storage_is_only_a_fat_pointer() {
        assert_eq!(
            std::mem::size_of::<ScenarioFuture<'static>>(),
            2 * std::mem::size_of::<usize>()
        );
    }

    #[test]
    fn duplicate_unknown_and_non_executable_owners_fail() {
        let duplicate = [COMPLETE[0], COMPLETE[0], COMPLETE[1], COMPLETE[2]];
        assert!(
            validate_ownership(TOOLS, RESOURCES, &duplicate)
                .unwrap_err()
                .starts_with("duplicate live operation owner")
        );
        let unknown = [
            COMPLETE[0],
            COMPLETE[1],
            COMPLETE[2],
            own(LiveOperation::ObjectCreate, ScenarioId::Discovery),
        ];
        assert!(
            validate_ownership(TOOLS, RESOURCES, &unknown)
                .unwrap_err()
                .starts_with("unknown live operation owner")
        );
        let non_executable = [
            Ownership {
                operation: LiveOperation::ServerStatus,
                scenario: ScenarioId::SyntheticNonExecutable,
            },
            COMPLETE[1],
            COMPLETE[2],
        ];
        assert!(
            validate_ownership(TOOLS, RESOURCES, &non_executable)
                .unwrap_err()
                .starts_with("non-executable live scenario owner")
        );
    }
}
