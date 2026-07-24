// any-mcp - bounded, workflow-oriented MCP server for Anytype
//
// SPDX-FileCopyrightText: 2026 Steve Schoettler
// SPDX-License-Identifier: Apache-2.0

//! Startup-only optional toolset selection and registry composition.

use std::{
    borrow::Cow,
    collections::{HashMap, HashSet},
    fmt,
    future::Future,
    pin::Pin,
};

use rmcp::{
    model::{
        CallToolRequestParams, CallToolResult, ErrorData, ProtocolVersion,
        ReadResourceRequestMethod, ReadResourceRequestParams, ReadResourceResult, Resource,
        ResourceTemplate, Tool,
    },
    schemars::{JsonSchema, Schema, SchemaGenerator, json_schema},
};
use serde::Serialize;
use tokio_util::sync::CancellationToken;

use crate::{
    artifact_toolset::ARTIFACT_REGISTRY,
    body_toolset::BODY_BLOCKS_REGISTRY,
    chats_toolset::CHATS_REGISTRY,
    collection_member_toolset::VIEWS_WRITE_REGISTRY,
    cursor::CursorStore,
    file_content::FILE_CONTENT_REGISTRY,
    member_toolset::MEMBERS_REGISTRY,
    protocol::{ToolProfile, WorkflowTool, workflow_tool},
    runtime::RuntimeContext,
    schema::SchemaContractError,
    schema_toolset::SCHEMA_REGISTRY,
};

/// Environment variable selecting optional production registries.
pub const OPTIONAL_TOOLSETS_ENV: &str = "ANY_MCP_TOOLSETS";
/// Maximum selected registries accepted by one process.
pub const MAX_OPTIONAL_TOOLSETS: usize = 16;
/// Maximum Unicode scalar values in the complete selector.
pub const MAX_OPTIONAL_SELECTOR_CHARS: usize = 255;

const MAX_TOOLSET_NAME_CHARS: usize = 32;
const STATUS_TOOL_NAME: &str = "optional_toolset_status";
const STATUS_DESCRIPTION: &str = "Inspect configured and active optional Anytype toolsets for this startup. Returns canonical selector names and no secrets.";

/// Boxed asynchronous result returned by optional registry handlers.
pub type OptionalRegistryFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Immutable startup metadata for one linked optional registry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OptionalToolsetMetadata {
    /// Exact selector and registry name.
    pub name: &'static str,
    /// Whether this registry adds an authenticated gRPC startup requirement.
    pub requires_grpc: bool,
}

impl OptionalToolsetMetadata {
    /// Defines static registry metadata.
    #[must_use]
    pub const fn new(name: &'static str, requires_grpc: bool) -> Self {
        Self {
            name,
            requires_grpc,
        }
    }
}

/// One validated, exact optional registry name.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct ToolsetName(String);

impl ToolsetName {
    fn new(value: &str) -> Result<Self, OptionalSelectorError> {
        if valid_toolset_name(value) {
            Ok(Self(value.to_owned()))
        } else {
            Err(OptionalSelectorError::Invalid)
        }
    }

    /// Borrows the exact validated selector spelling.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl JsonSchema for ToolsetName {
    fn schema_name() -> Cow<'static, str> {
        "ToolsetName".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "minLength": 1,
            "maxLength": MAX_TOOLSET_NAME_CHARS,
            "pattern": "^[a-z][a-z0-9-]{0,31}$"
        })
    }
}

/// Canonical startup selection resolved against linked registry metadata.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OptionalToolsetSelection {
    entries: Vec<OptionalToolsetMetadata>,
}

impl OptionalToolsetSelection {
    /// Parses one selector exactly and resolves it against linked metadata.
    pub fn parse(
        value: Option<String>,
        available: &[OptionalToolsetMetadata],
    ) -> Result<Self, OptionalSelectorError> {
        let Some(value) = value else {
            return Ok(Self::default());
        };
        if value.is_empty() || value.chars().count() > MAX_OPTIONAL_SELECTOR_CHARS {
            return Err(OptionalSelectorError::Invalid);
        }
        let tokens = value.split(',').collect::<Vec<_>>();
        if tokens.is_empty() || tokens.len() > MAX_OPTIONAL_TOOLSETS {
            return Err(OptionalSelectorError::Invalid);
        }
        if tokens.iter().any(|token| !valid_toolset_name(token)) {
            return Err(OptionalSelectorError::Invalid);
        }
        let mut unique = HashSet::with_capacity(tokens.len());
        if tokens.iter().any(|token| !unique.insert(*token)) {
            return Err(OptionalSelectorError::Duplicate);
        }

        let mut linked = HashMap::with_capacity(available.len());
        for metadata in available {
            if !valid_toolset_name(metadata.name)
                || linked.insert(metadata.name, *metadata).is_some()
            {
                return Err(OptionalSelectorError::Unsupported);
            }
        }
        let mut entries = tokens
            .into_iter()
            .map(|token| {
                linked
                    .get(token)
                    .copied()
                    .ok_or(OptionalSelectorError::Unsupported)
            })
            .collect::<Result<Vec<_>, _>>()?;
        entries.sort_by(|left, right| left.name.cmp(right.name));
        Ok(Self { entries })
    }

    /// Returns whether no optional registry was selected.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Returns the exact canonical selected names.
    #[must_use]
    pub fn names(&self) -> impl ExactSizeIterator<Item = &'static str> + '_ {
        self.entries.iter().map(|entry| entry.name)
    }

    /// Returns whether the selected transport union requires gRPC.
    #[must_use]
    pub fn requires_grpc(&self) -> bool {
        self.entries.iter().any(|entry| entry.requires_grpc)
    }

    /// Returns whether the exact linked name is selected.
    #[must_use]
    pub fn contains(&self, name: &str) -> bool {
        self.entries
            .binary_search_by_key(&name, |entry| entry.name)
            .is_ok()
    }
}

/// Fixed, secret-safe selector failure category.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OptionalSelectorError {
    /// Selector grammar or global bounds were invalid.
    Invalid,
    /// The exact selector contained a duplicate token.
    Duplicate,
    /// A syntactically valid name had no complete linked registry.
    Unsupported,
}

impl fmt::Display for OptionalSelectorError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Invalid => "invalid optional toolset selector",
            Self::Duplicate => "duplicate optional toolset selector",
            Self::Unsupported => "unsupported optional toolset selector",
        })
    }
}

impl std::error::Error for OptionalSelectorError {}

/// Rejects an unsafe effective retry limit for a nonempty optional selection.
pub fn admit_optional_retry_policy(
    selection: &OptionalToolsetSelection,
    effective_max_retries: u32,
) -> Result<(), OptionalRetryPolicyError> {
    if selection.is_empty() || (1..=5).contains(&effective_max_retries) {
        Ok(())
    } else {
        Err(OptionalRetryPolicyError)
    }
}

/// Fixed, secret-safe optional retry admission failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OptionalRetryPolicyError;

impl fmt::Display for OptionalRetryPolicyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("invalid optional retry policy")
    }
}

impl std::error::Error for OptionalRetryPolicyError {}

fn valid_toolset_name(value: &str) -> bool {
    let bytes = value.as_bytes();
    (1..=MAX_TOOLSET_NAME_CHARS).contains(&bytes.len())
        && bytes[0].is_ascii_lowercase()
        && bytes[1..]
            .iter()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'-')
}

/// One typed optional tool plus its access classification.
#[derive(Clone, Debug)]
pub struct OptionalRegistryTool {
    tool: Tool,
    mutation: bool,
}

impl OptionalRegistryTool {
    /// Registers one read-only optional tool.
    #[must_use]
    pub fn read<O>(tool: WorkflowTool<O>) -> Self {
        Self {
            tool: tool.into_tool(),
            mutation: false,
        }
    }

    /// Registers one optional mutation removed from read-only catalogs.
    #[must_use]
    pub fn mutation<O>(tool: WorkflowTool<O>) -> Self {
        Self {
            tool: tool.into_tool(),
            mutation: true,
        }
    }
}

/// Static optional registry linked into the binary only when complete.
pub trait OptionalToolsetRegistry: fmt::Debug + Send + Sync {
    /// Returns exact immutable registry metadata.
    fn metadata(&self) -> OptionalToolsetMetadata;

    /// Builds the registry's complete typed tool contracts.
    fn tools(&self) -> Result<Vec<OptionalRegistryTool>, SchemaContractError>;

    /// Returns static resource instances owned by this registry.
    fn resources(&self) -> Vec<Resource> {
        Vec::new()
    }

    /// Returns static resource templates owned by this registry.
    fn resource_templates(&self) -> Vec<ResourceTemplate> {
        Vec::new()
    }

    /// Returns exact scripted scenario identifiers owned by this registry.
    fn scripted_scenario_ids(&self) -> &'static [&'static str];

    /// Returns exact real-headless scenario identifiers owned by this registry.
    fn headless_scenario_ids(&self) -> &'static [&'static str];

    /// Returns the reviewed incremental catalog-token ceiling.
    fn catalog_token_ceiling(&self) -> usize;

    /// Dispatches one tool name owned by this selected registry.
    fn call_tool<'a>(
        &'a self,
        request: CallToolRequestParams,
        runtime: &'a RuntimeContext,
        cursors: &'a CursorStore,
        protocol_version: &'a ProtocolVersion,
        cancellation: &'a CancellationToken,
    ) -> OptionalRegistryFuture<'a, Result<CallToolResult, ErrorData>>;

    /// Returns whether this registry owns the supplied resource URI.
    fn owns_resource_uri(&self, _uri: &str) -> bool {
        false
    }

    /// Returns whether this registry owns the exact advertised URI template.
    fn owns_resource_template(&self, _uri_template: &str) -> bool {
        false
    }

    /// Reads one URI for which [`Self::owns_resource_uri`] returned true.
    fn read_resource<'a>(
        &'a self,
        _request: ReadResourceRequestParams,
        _runtime: &'a RuntimeContext,
        _cancellation: &'a CancellationToken,
    ) -> OptionalRegistryFuture<'a, Result<ReadResourceResult, ErrorData>> {
        Box::pin(async { Err(ErrorData::method_not_found::<ReadResourceRequestMethod>()) })
    }
}

/// Production registries linked into this binary.
///
/// The foundation deliberately starts empty. Domain implementations add a
/// descriptor here only after their independent review and complete handler,
/// schema, resource, scenario, and token ownership land together.
#[must_use]
pub fn production_optional_registries() -> &'static [&'static dyn OptionalToolsetRegistry] {
    &PRODUCTION_OPTIONAL_REGISTRIES
}

static PRODUCTION_OPTIONAL_REGISTRIES: [&dyn OptionalToolsetRegistry; 7] = [
    &ARTIFACT_REGISTRY,
    BODY_BLOCKS_REGISTRY,
    CHATS_REGISTRY,
    MEMBERS_REGISTRY,
    &FILE_CONTENT_REGISTRY,
    SCHEMA_REGISTRY,
    VIEWS_WRITE_REGISTRY,
];

/// Returns immutable production metadata without building schemas or clients.
#[must_use]
pub fn production_optional_metadata() -> Vec<OptionalToolsetMetadata> {
    production_optional_registries()
        .iter()
        .map(|registry| registry.metadata())
        .collect()
}

/// Empty input for `optional_toolset_status`.
#[derive(Clone, Copy, Debug, Default, serde::Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OptionalToolsetStatusInput {}

/// Immutable optional registry status returned without I/O.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OptionalToolsetStatusOutput {
    /// Canonical names requested at startup.
    #[schemars(schema_with = "configured_toolsets_schema")]
    configured_toolsets: Vec<ToolsetName>,
    /// Canonical selected names contributing an admitted contract.
    #[schemars(schema_with = "active_toolsets_schema")]
    active_toolsets: Vec<ToolsetName>,
}

fn configured_toolsets_schema(generator: &mut SchemaGenerator) -> Schema {
    let items = generator.subschema_for::<ToolsetName>();
    json_schema!({
        "type": "array",
        "items": items,
        "minItems": 1,
        "maxItems": MAX_OPTIONAL_TOOLSETS,
        "uniqueItems": true
    })
}

fn active_toolsets_schema(generator: &mut SchemaGenerator) -> Schema {
    let items = generator.subschema_for::<ToolsetName>();
    json_schema!({
        "type": "array",
        "items": items,
        "minItems": 0,
        "maxItems": MAX_OPTIONAL_TOOLSETS,
        "uniqueItems": true
    })
}

/// Builds the common optional status tool contract.
pub fn optional_toolset_status_tool()
-> Result<WorkflowTool<OptionalToolsetStatusOutput>, SchemaContractError> {
    workflow_tool::<OptionalToolsetStatusInput, OptionalToolsetStatusOutput>(
        STATUS_TOOL_NAME,
        STATUS_DESCRIPTION,
        ToolProfile::Read,
    )
}

/// Validated optional catalog contribution for one server instance.
pub(crate) struct OptionalCatalog {
    pub(crate) tools: Vec<Tool>,
    pub(crate) resources: Vec<Resource>,
    pub(crate) resource_templates: Vec<ResourceTemplate>,
    selected_registries: Vec<&'static dyn OptionalToolsetRegistry>,
    dispatch: HashMap<String, &'static dyn OptionalToolsetRegistry>,
    read_only_mutations: HashSet<String>,
    status: OptionalToolsetStatusOutput,
}

impl OptionalCatalog {
    pub(crate) fn is_selected(&self) -> bool {
        !self.status.configured_toolsets.is_empty()
    }

    pub(crate) fn status(&self) -> &OptionalToolsetStatusOutput {
        &self.status
    }

    pub(crate) fn registry_for_tool(
        &self,
        name: &str,
    ) -> Option<&'static dyn OptionalToolsetRegistry> {
        self.dispatch.get(name).copied()
    }

    pub(crate) fn is_read_only_mutation(&self, name: &str) -> bool {
        self.read_only_mutations.contains(name)
    }

    pub(crate) fn registry_for_resource(
        &self,
        uri: &str,
    ) -> Option<&'static dyn OptionalToolsetRegistry> {
        self.selected_registries
            .iter()
            .copied()
            .find(|registry| registry.owns_resource_uri(uri))
    }
}

/// Composes selected descriptors and rejects incomplete or colliding ownership.
pub(crate) fn compose_optional_catalog(
    selection: &OptionalToolsetSelection,
    linked: &'static [&'static dyn OptionalToolsetRegistry],
    read_only: bool,
    reserved_tool_names: &[&str],
    reserved_resource_uris: &[&str],
    reserved_resource_templates: &[&str],
) -> Result<OptionalCatalog, OptionalCatalogError> {
    let mut linked_by_name = HashMap::with_capacity(linked.len());
    for registry in linked {
        let metadata = registry.metadata();
        if !valid_toolset_name(metadata.name)
            || linked_by_name.insert(metadata.name, *registry).is_some()
        {
            return Err(OptionalCatalogError);
        }
    }

    let mut selected_registries = Vec::with_capacity(selection.entries.len());
    for entry in &selection.entries {
        let registry = linked_by_name
            .get(entry.name)
            .copied()
            .ok_or(OptionalCatalogError)?;
        if registry.metadata() != *entry || registry.catalog_token_ceiling() == 0 {
            return Err(OptionalCatalogError);
        }
        selected_registries.push(registry);
    }

    let mut occupied_tools = reserved_tool_names
        .iter()
        .map(|name| (*name).to_owned())
        .collect::<HashSet<_>>();
    if !occupied_tools.insert(STATUS_TOOL_NAME.to_owned()) {
        return Err(OptionalCatalogError);
    }
    let mut occupied_resources = reserved_resource_uris
        .iter()
        .map(|uri| (*uri).to_owned())
        .collect::<HashSet<_>>();
    let mut occupied_templates = reserved_resource_templates
        .iter()
        .map(|uri| (*uri).to_owned())
        .collect::<HashSet<_>>();
    let mut occupied_scenarios = [
        "optional_toolset_status_direct_contract",
        "optional_toolset_status_stdio_contract",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<HashSet<_>>();

    let mut tools = Vec::new();
    let mut resources = Vec::new();
    let mut resource_templates = Vec::new();
    let mut dispatch = HashMap::new();
    let mut read_only_mutations = HashSet::new();
    let mut active_names = Vec::new();

    for registry in &selected_registries {
        let mut active = false;
        let scripted_scenarios = registry.scripted_scenario_ids();
        let headless_scenarios = registry.headless_scenario_ids();
        if scripted_scenarios.is_empty() || headless_scenarios.is_empty() {
            return Err(OptionalCatalogError);
        }
        for scenario in scripted_scenarios.iter().chain(headless_scenarios) {
            if scenario.is_empty() || !occupied_scenarios.insert((*scenario).to_owned()) {
                return Err(OptionalCatalogError);
            }
        }
        let contributions = registry.tools().map_err(|_| OptionalCatalogError)?;
        let registry_resources = registry.resources();
        let registry_templates = registry.resource_templates();
        if contributions.is_empty()
            && registry_resources.is_empty()
            && registry_templates.is_empty()
        {
            return Err(OptionalCatalogError);
        }
        for contribution in contributions {
            let name = contribution.tool.name.to_string();
            if name.is_empty()
                || !occupied_tools.insert(name.clone())
                || dispatch.insert(name.clone(), *registry).is_some()
            {
                return Err(OptionalCatalogError);
            }
            if contribution.mutation && read_only {
                read_only_mutations.insert(name);
            } else {
                tools.push(contribution.tool);
                active = true;
            }
        }
        for resource in registry_resources {
            let uri = resource.uri.to_string();
            if uri.is_empty()
                || !registry.owns_resource_uri(&uri)
                || !occupied_resources.insert(uri)
            {
                return Err(OptionalCatalogError);
            }
            resources.push(resource);
            active = true;
        }
        for template in registry_templates {
            let uri = template.uri_template.to_string();
            if uri.is_empty()
                || !registry.owns_resource_template(&uri)
                || !occupied_templates.insert(uri)
            {
                return Err(OptionalCatalogError);
            }
            resource_templates.push(template);
            active = true;
        }
        if active {
            active_names.push(ToolsetName::new(registry.metadata().name)?);
        }
    }

    tools.sort_by(|left, right| left.name.cmp(&right.name));
    resources.sort_by(|left, right| left.uri.cmp(&right.uri));
    resource_templates.sort_by(|left, right| left.uri_template.cmp(&right.uri_template));
    active_names.sort();
    let configured_toolsets = selection
        .names()
        .map(ToolsetName::new)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(OptionalCatalog {
        tools,
        resources,
        resource_templates,
        selected_registries,
        dispatch,
        read_only_mutations,
        status: OptionalToolsetStatusOutput {
            configured_toolsets,
            active_toolsets: active_names,
        },
    })
}

/// Fixed startup failure for invalid optional descriptor composition.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct OptionalCatalogError;

impl From<OptionalSelectorError> for OptionalCatalogError {
    fn from(_: OptionalSelectorError) -> Self {
        Self
    }
}

impl fmt::Display for OptionalCatalogError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("unable to compose optional MCP registries")
    }
}

impl std::error::Error for OptionalCatalogError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn metadata() -> [OptionalToolsetMetadata; 3] {
        [
            OptionalToolsetMetadata::new("alpha", false),
            OptionalToolsetMetadata::new("beta-2", true),
            OptionalToolsetMetadata::new("zeta", false),
        ]
    }

    #[test]
    fn absent_selector_is_empty_and_present_names_are_canonical() {
        assert!(
            OptionalToolsetSelection::parse(None, &metadata())
                .unwrap()
                .is_empty()
        );
        let selection =
            OptionalToolsetSelection::parse(Some("zeta,alpha,beta-2".to_owned()), &metadata())
                .unwrap();
        assert_eq!(
            selection.names().collect::<Vec<_>>(),
            ["alpha", "beta-2", "zeta"]
        );
        assert!(selection.requires_grpc());
    }

    #[test]
    fn selector_grammar_and_global_bounds_fail_closed() {
        for invalid in [
            "",
            ",",
            "alpha,",
            ",alpha",
            "alpha,,zeta",
            "Alpha",
            "alpha_beta",
            "alpha beta",
            " alpha",
            "alpha ",
            "é",
            "a01234567890123456789012345678901x",
        ] {
            assert_eq!(
                OptionalToolsetSelection::parse(Some(invalid.to_owned()), &metadata()),
                Err(OptionalSelectorError::Invalid),
                "{invalid:?}"
            );
        }
        let too_many = (0..17)
            .map(|index| format!("a{index}"))
            .collect::<Vec<_>>()
            .join(",");
        assert_eq!(
            OptionalToolsetSelection::parse(Some(too_many), &[]),
            Err(OptionalSelectorError::Invalid)
        );
        assert_eq!(
            OptionalToolsetSelection::parse(Some("a".repeat(256)), &[]),
            Err(OptionalSelectorError::Invalid)
        );
    }

    #[test]
    fn duplicate_and_unsupported_categories_never_echo_names() {
        assert_eq!(
            OptionalToolsetSelection::parse(Some("alpha,alpha".to_owned()), &metadata()),
            Err(OptionalSelectorError::Duplicate)
        );
        assert_eq!(
            OptionalToolsetSelection::parse(Some("secret-like-name".to_owned()), &metadata()),
            Err(OptionalSelectorError::Unsupported)
        );
        for error in [
            OptionalSelectorError::Invalid,
            OptionalSelectorError::Duplicate,
            OptionalSelectorError::Unsupported,
        ] {
            assert!(!error.to_string().contains("secret-like-name"));
        }
    }

    #[test]
    fn retry_admission_is_orthogonal_to_empty_selection() {
        let empty = OptionalToolsetSelection::default();
        assert!(admit_optional_retry_policy(&empty, 0).is_ok());
        assert!(admit_optional_retry_policy(&empty, u32::MAX).is_ok());

        let selected =
            OptionalToolsetSelection::parse(Some("alpha".to_owned()), &metadata()).unwrap();
        for admitted in 1..=5 {
            assert!(admit_optional_retry_policy(&selected, admitted).is_ok());
        }
        for rejected in [0, 6, u32::MAX] {
            assert_eq!(
                admit_optional_retry_policy(&selected, rejected),
                Err(OptionalRetryPolicyError)
            );
        }
    }

    #[test]
    fn status_contract_is_exact_and_bounded() {
        let tool = optional_toolset_status_tool().unwrap().into_tool();
        assert_eq!(tool.name, STATUS_TOOL_NAME);
        assert_eq!(tool.description.as_deref(), Some(STATUS_DESCRIPTION));
        assert_eq!(tool.input_schema["additionalProperties"], false);
        let output = tool.output_schema.unwrap();
        assert_eq!(output["additionalProperties"], false);
        assert_eq!(output["properties"]["configured_toolsets"]["minItems"], 1);
        assert_eq!(output["properties"]["configured_toolsets"]["maxItems"], 16);
        assert_eq!(
            output["properties"]["configured_toolsets"]["uniqueItems"],
            true
        );
        assert_eq!(output["properties"]["active_toolsets"]["minItems"], 0);
        assert_eq!(output["properties"]["active_toolsets"]["maxItems"], 16);
    }
}
