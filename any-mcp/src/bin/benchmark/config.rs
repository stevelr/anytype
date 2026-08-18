// any-mcp - bounded, workflow-oriented MCP server for Anytype
//
// SPDX-FileCopyrightText: 2026 Steve Schoettler
// SPDX-License-Identifier: Apache-2.0

use std::{collections::BTreeMap, path::Path};

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const SCHEMA_VERSION: u32 = 1;
pub const COMPLETE_PAIRS: u16 = 120;

#[derive(Clone, Copy, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum BenchmarkMode {
    ProductionVerifiedOfficial,
    ControlledFixedSpecWarm,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum BenchmarkTrack {
    Protocol,
    Agent,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub schema_version: u32,
    pub mode: BenchmarkMode,
    pub track: BenchmarkTrack,
    pub run_id: String,
    pub result_file: String,
    pub seed: u64,
    pub pairs_per_scenario: u16,
    pub local: Server,
    pub upstream: Server,
    #[serde(default)]
    pub controlled_spec: Option<AttestedFile>,
    pub oracle: Oracle,
    pub manifest: ManifestInput,
    pub ancestry: AncestryAdmission,
    pub scenarios: Vec<Scenario>,
    #[serde(default)]
    pub agent: Option<AgentInput>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Server {
    pub executable: String,
    pub arguments: Vec<String>,
    pub executable_sha256: String,
    #[serde(default)]
    pub environment: BTreeMap<String, String>,
    #[serde(default)]
    pub credentials: Vec<Credential>,
    pub artifact: ServerArtifact,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum ServerArtifact {
    Local {
        revision: String,
        features: Vec<String>,
    },
    OfficialNpm {
        package: String,
        version: String,
        integrity: String,
        tarball_path: String,
        tarball_sha256: String,
        tarball_bundle_entry: String,
        bundle_path: String,
        bundle_sha256: String,
    },
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Credential {
    pub source_fd: i32,
    pub child_environment: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Oracle {
    pub base_url: String,
    pub credential_fd: i32,
    pub credential_header: String,
    pub openapi_path: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ManifestInput {
    pub local_revision: String,
    pub local_features: Vec<String>,
    pub rust_toolchain: String,
    pub node_runtime: String,
    pub anytype_cli_version: String,
    pub anytype_cli_path: String,
    pub anytype_cli_sha256: String,
    pub heart_version: String,
    pub heart_path: String,
    pub heart_sha256: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttestedFile {
    pub path: String,
    pub sha256: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AncestryAdmission {
    pub repository_path: String,
    pub integrated_revision: String,
    pub required_ancestors: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scenario {
    pub name: String,
    pub class: ScenarioClass,
    #[serde(default)]
    pub setup: Vec<HttpStep>,
    pub calls: Vec<ToolCall>,
    pub oracle: HttpStep,
    #[serde(default)]
    pub cleanup: Vec<HttpStep>,
    pub answer: AnswerContract,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AnswerContract {
    pub local_pointer: String,
    pub upstream_pointer: String,
    pub oracle_pointer: String,
    pub expected: Value,
    pub agent_answer: Value,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum ScenarioClass {
    DirectCrud,
    SearchRead,
    SearchReadEdit,
    AmbiguousNameMutation,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HttpStep {
    pub method: String,
    pub path: String,
    #[serde(default)]
    pub body: Option<Value>,
    pub expect_status: u16,
    #[serde(default)]
    pub capture: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolCall {
    pub name: String,
    pub arguments: Value,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentInput {
    pub provider: String,
    pub model: String,
    pub endpoint: String,
    pub settings_sha256: String,
    pub input_cost_per_million: String,
    pub output_cost_per_million: String,
    pub credential_fd: i32,
}

impl Config {
    pub fn read(path: &Path) -> Result<Self, String> {
        let metadata = std::fs::symlink_metadata(path)
            .map_err(|error| format!("cannot inspect benchmark config: {error}"))?;
        if !metadata.file_type().is_file() {
            return Err("benchmark config must be a regular file".to_owned());
        }
        if metadata.len() > 1024 * 1024 {
            return Err("benchmark config exceeds the 1 MiB limit".to_owned());
        }
        let bytes = std::fs::read(path)
            .map_err(|error| format!("cannot read benchmark config: {error}"))?;
        let config: Self = serde_json::from_slice(&bytes)
            .map_err(|error| format!("invalid benchmark config: {error}"))?;
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != SCHEMA_VERSION {
            return Err("unsupported benchmark config schema".to_owned());
        }
        validate_identifier(&self.run_id, "run_id")?;
        validate_relative_file(&self.result_file, "result_file")?;
        if self.pairs_per_scenario != COMPLETE_PAIRS {
            return Err(format!(
                "pairs_per_scenario must be exactly {COMPLETE_PAIRS}"
            ));
        }
        if self.seed == 0 {
            return Err("benchmark seed must be nonzero".to_owned());
        }
        if self.scenarios.len() != 4 {
            return Err("the benchmark requires exactly four scenarios".to_owned());
        }
        let expected_classes = [
            ScenarioClass::DirectCrud,
            ScenarioClass::SearchRead,
            ScenarioClass::SearchReadEdit,
            ScenarioClass::AmbiguousNameMutation,
        ];
        for class in expected_classes {
            if self
                .scenarios
                .iter()
                .filter(|item| item.class == class)
                .count()
                != 1
            {
                return Err("each required scenario class must appear exactly once".to_owned());
            }
        }
        for scenario in &self.scenarios {
            validate_identifier(&scenario.name, "scenario name")?;
            if scenario.calls.is_empty() || scenario.calls.len() > 8 {
                return Err("each scenario requires between one and eight tool calls".to_owned());
            }
            validate_http_step(&scenario.oracle)?;
            for step in scenario.setup.iter().chain(&scenario.cleanup) {
                validate_http_step(step)?;
            }
            for call in &scenario.calls {
                validate_identifier(&call.name, "tool name")?;
                validate_json_depth(&call.arguments, 0)?;
            }
            for pointer in [
                &scenario.answer.local_pointer,
                &scenario.answer.upstream_pointer,
                &scenario.answer.oracle_pointer,
            ] {
                if !pointer.starts_with('/') || pointer.len() > 512 {
                    return Err("answer contract requires bounded JSON pointers".to_owned());
                }
            }
            validate_json_depth(&scenario.answer.expected, 0)?;
            validate_json_depth(&scenario.answer.agent_answer, 0)?;
        }
        validate_server(&self.local, false, self.mode)?;
        validate_server(&self.upstream, true, self.mode)?;
        match (self.mode, &self.controlled_spec) {
            (BenchmarkMode::ProductionVerifiedOfficial, None) => {}
            (BenchmarkMode::ProductionVerifiedOfficial, Some(_)) => {
                return Err("production mode must not contain a controlled spec".to_owned());
            }
            (BenchmarkMode::ControlledFixedSpecWarm, Some(spec)) => {
                if !Path::new(&spec.path).is_absolute() {
                    return Err("controlled spec path must be absolute".to_owned());
                }
                validate_sha256(&spec.sha256)?;
                if self.upstream.arguments.get(2) != Some(&spec.path) {
                    return Err(
                        "controlled upstream argument differs from its attested spec".to_owned(),
                    );
                }
            }
            (BenchmarkMode::ControlledFixedSpecWarm, None) => {
                return Err("controlled mode requires an attested fixed spec".to_owned());
            }
        }
        if self.oracle.credential_fd < 3 {
            return Err("oracle credential_fd must be an inherited descriptor".to_owned());
        }
        if self.oracle.credential_header != "Authorization" {
            return Err("oracle credential_header must be Authorization".to_owned());
        }
        let base = reqwest::Url::parse(&self.oracle.base_url)
            .map_err(|_| "oracle base_url is invalid".to_owned())?;
        if base.scheme() != "http" && base.scheme() != "https" {
            return Err("oracle base_url must use http or https".to_owned());
        }
        validate_http_path(&self.oracle.openapi_path)?;
        if self.manifest.local_revision != local_revision(&self.local)? {
            return Err("manifest local revision differs from the local artifact".to_owned());
        }
        if self.manifest.local_features != local_features(&self.local)? {
            return Err("manifest local features differ from the local artifact".to_owned());
        }
        validate_ancestry(&self.ancestry, &self.manifest.local_revision)?;
        match self.track {
            BenchmarkTrack::Protocol if self.agent.is_some() => {
                return Err("protocol track must not contain agent settings".to_owned());
            }
            BenchmarkTrack::Agent if self.agent.is_none() => {
                return Err("agent track requires immutable model and cost settings".to_owned());
            }
            _ => {}
        }
        if let Some(agent) = &self.agent {
            for value in [
                &agent.provider,
                &agent.model,
                &agent.endpoint,
                &agent.settings_sha256,
                &agent.input_cost_per_million,
                &agent.output_cost_per_million,
            ] {
                if value.trim().is_empty() {
                    return Err("agent settings must be immutable and nonempty".to_owned());
                }
            }
            if agent.credential_fd < 3 {
                return Err("agent credential_fd must be an inherited descriptor".to_owned());
            }
        }
        Ok(())
    }
}

fn validate_server(server: &Server, upstream: bool, mode: BenchmarkMode) -> Result<(), String> {
    let path = Path::new(&server.executable);
    if !path.is_absolute() {
        return Err("server executable must be absolute".to_owned());
    }
    validate_sha256(&server.executable_sha256)?;
    if server.arguments.len() > 16 || server.environment.len() > 8 || server.credentials.len() > 4 {
        return Err("server launch configuration exceeds its bound".to_owned());
    }
    const PUBLIC_ENV_ALLOWLIST: &[&str] = &[
        "ANYTYPE_API_BASE_URL",
        "ANYTYPE_KEYSTORE",
        "ANYTYPE_KEYSTORE_SERVICE",
        "ANY_MCP_PROFILE",
        "ANY_MCP_TOOLSETS",
        "ANY_MCP_READ_ONLY",
        "RUST_LOG",
    ];
    const CREDENTIAL_ENV_ALLOWLIST: &[&str] = &[
        "ANYTYPE_API_KEY",
        "ANYTYPE_KEY_HTTP_TOKEN",
        "ANYTYPE_KEY_ACCOUNT_ID",
        "ANYTYPE_KEY_ACCOUNT_KEY",
        "ANYTYPE_KEY_SESSION_TOKEN",
    ];
    for name in server.environment.keys() {
        if !PUBLIC_ENV_ALLOWLIST.contains(&name.as_str()) {
            return Err("server environment contains a non-allowlisted name".to_owned());
        }
    }
    let mut credential_names = std::collections::BTreeSet::new();
    let mut credential_fds = std::collections::BTreeSet::new();
    for credential in &server.credentials {
        if credential.source_fd < 3
            || !CREDENTIAL_ENV_ALLOWLIST.contains(&credential.child_environment.as_str())
        {
            return Err("server credential seam is not allowlisted".to_owned());
        }
        if server
            .environment
            .contains_key(&credential.child_environment)
        {
            return Err("credential environment must not also contain a public value".to_owned());
        }
        if !credential_names.insert(&credential.child_environment)
            || !credential_fds.insert(credential.source_fd)
        {
            return Err("server credential seams must be unique".to_owned());
        }
    }
    if upstream {
        let official_bundle_path = match &server.artifact {
            ServerArtifact::OfficialNpm {
                package,
                version,
                integrity,
                tarball_path,
                tarball_sha256,
                tarball_bundle_entry,
                bundle_path,
                bundle_sha256,
            } => {
                if package != "@anyproto/anytype-mcp" || version != "1.2.10" {
                    return Err(
                        "production comparator must be official anytype-mcp 1.2.10".to_owned()
                    );
                }
                if !integrity.starts_with("sha512-") || integrity.len() < 80 {
                    return Err("official npm integrity must be a resolved sha512 SRI".to_owned());
                }
                validate_sha256(tarball_sha256)?;
                validate_sha256(bundle_sha256)?;
                if !Path::new(bundle_path).is_absolute() || !Path::new(tarball_path).is_absolute() {
                    return Err("official npm artifact paths must be absolute".to_owned());
                }
                validate_tar_entry(tarball_bundle_entry)?;
                bundle_path
            }
            ServerArtifact::Local { .. } => {
                return Err("upstream server must be an official npm artifact".to_owned());
            }
        };
        match mode {
            BenchmarkMode::ProductionVerifiedOfficial => {
                if server.arguments.len() != 2
                    || server.arguments.first() != Some(official_bundle_path)
                    || server.arguments.get(1).map(String::as_str) != Some("run")
                    || server
                        .arguments
                        .iter()
                        .any(|argument| argument.contains("spec"))
                {
                    return Err(
                        "production upstream must run its official bundle without a spec argument"
                            .to_owned(),
                    );
                }
                let env_names = server
                    .environment
                    .keys()
                    .map(String::as_str)
                    .collect::<Vec<_>>();
                if env_names.as_slice() != ["ANYTYPE_API_BASE_URL"] {
                    return Err(
                        "production upstream public environment is limited to ANYTYPE_API_BASE_URL"
                            .to_owned(),
                    );
                }
                if server.credentials.len() != 1
                    || server
                        .credentials
                        .first()
                        .map(|credential| credential.child_environment.as_str())
                        != Some("ANYTYPE_API_KEY")
                {
                    return Err(
                        "production upstream accepts exactly one API-key credential seam"
                            .to_owned(),
                    );
                }
            }
            BenchmarkMode::ControlledFixedSpecWarm => {
                if server.arguments.len() != 3
                    || server.arguments.first() != Some(official_bundle_path)
                    || server.arguments.get(1).map(String::as_str) != Some("run")
                    || server
                        .arguments
                        .get(2)
                        .is_none_or(|path| !Path::new(path).is_absolute())
                {
                    return Err("controlled upstream requires one absolute fixed spec".to_owned());
                }
            }
        }
    } else {
        if !matches!(server.artifact, ServerArtifact::Local { .. }) {
            return Err("local server requires a local artifact attestation".to_owned());
        }
        if server.arguments.as_slice() != ["mcp"] {
            return Err(
                "local comparator must invoke the production anyr mcp entrypoint".to_owned(),
            );
        }
    }
    Ok(())
}

fn validate_tar_entry(value: &str) -> Result<(), String> {
    let path = Path::new(value);
    if value.is_empty()
        || value.len() > 512
        || path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                std::path::Component::ParentDir | std::path::Component::CurDir
            )
        })
    {
        return Err("npm tarball bundle entry must be a safe relative path".to_owned());
    }
    Ok(())
}

fn validate_ancestry(ancestry: &AncestryAdmission, local_revision: &str) -> Result<(), String> {
    if !Path::new(&ancestry.repository_path).is_absolute() {
        return Err("ancestry repository path must be absolute".to_owned());
    }
    validate_revision(&ancestry.integrated_revision)?;
    if ancestry.integrated_revision != local_revision {
        return Err("integrated revision differs from the local manifest revision".to_owned());
    }
    const REQUIRED: &[&str] = &["any-3d4m", "any-ljmz", "any-mhe6"];
    if ancestry.required_ancestors.len() != REQUIRED.len()
        || REQUIRED
            .iter()
            .any(|name| !ancestry.required_ancestors.contains_key(*name))
    {
        return Err("ancestry admission must name all integrated benchmark carriers".to_owned());
    }
    for revision in ancestry.required_ancestors.values() {
        validate_revision(revision)?;
    }
    Ok(())
}

fn validate_revision(value: &str) -> Result<(), String> {
    if !matches!(value.len(), 40 | 64) || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("revision must be a full hexadecimal commit id".to_owned());
    }
    Ok(())
}

fn local_revision(server: &Server) -> Result<String, String> {
    match &server.artifact {
        ServerArtifact::Local { revision, .. } => Ok(revision.clone()),
        ServerArtifact::OfficialNpm { .. } => Err("local artifact is missing".to_owned()),
    }
}

fn local_features(server: &Server) -> Result<Vec<String>, String> {
    match &server.artifact {
        ServerArtifact::Local { features, .. } => Ok(features.clone()),
        ServerArtifact::OfficialNpm { .. } => Err("local artifact is missing".to_owned()),
    }
}

fn validate_identifier(value: &str, field: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > 80
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err(format!("{field} is not a safe identifier"));
    }
    Ok(())
}

fn validate_relative_file(value: &str, field: &str) -> Result<(), String> {
    let path = Path::new(value);
    if value.is_empty()
        || value.len() > 120
        || path.is_absolute()
        || path.components().count() != 1
        || value == "."
        || value == ".."
    {
        return Err(format!("{field} must be one relative file name"));
    }
    Ok(())
}

fn validate_sha256(value: &str) -> Result<(), String> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("sha256 values must contain 64 hexadecimal digits".to_owned());
    }
    Ok(())
}

fn validate_http_step(step: &HttpStep) -> Result<(), String> {
    if !matches!(step.method.as_str(), "GET" | "POST" | "PATCH" | "DELETE") {
        return Err("HTTP oracle method is not allowlisted".to_owned());
    }
    validate_http_path(&step.path)?;
    if !(200..=299).contains(&step.expect_status) {
        return Err("HTTP oracle expect_status must be successful".to_owned());
    }
    if let Some(body) = &step.body {
        validate_json_depth(body, 0)?;
    }
    for (name, pointer) in &step.capture {
        validate_identifier(name, "capture name")?;
        if !pointer.starts_with('/') || pointer.len() > 512 {
            return Err("capture must use a bounded JSON pointer".to_owned());
        }
    }
    Ok(())
}

fn validate_http_path(path: &str) -> Result<(), String> {
    if !path.starts_with('/')
        || path.starts_with("//")
        || path.len() > 2048
        || path.contains('#')
        || path.contains("..")
    {
        return Err("HTTP oracle path must be an absolute-path reference".to_owned());
    }
    Ok(())
}

fn validate_json_depth(value: &Value, depth: usize) -> Result<(), String> {
    if depth > 64 {
        return Err("JSON value exceeds the depth limit".to_owned());
    }
    match value {
        Value::Array(items) => {
            for item in items {
                validate_json_depth(item, depth + 1)?;
            }
        }
        Value::Object(items) => {
            if items.len() > 4096 {
                return Err("JSON object exceeds the member limit".to_owned());
            }
            for item in items.values() {
                validate_json_depth(item, depth + 1)?;
            }
        }
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fixture(mode: &str) -> Value {
        let classes = [
            "direct-crud",
            "search-read",
            "search-read-edit",
            "ambiguous-name-mutation",
        ];
        let scenarios = classes
            .iter()
            .map(|class| {
                json!({
                    "name": class,
                    "class": class,
                    "setup": [],
                    "calls": [{"name": "fixture", "arguments": {}}],
                    "oracle": {"method": "GET", "path": "/v1/fixture", "expect_status": 200},
                    "cleanup": [],
                    "answer": {
                        "local_pointer": "/result",
                        "upstream_pointer": "/result",
                        "oracle_pointer": "/result",
                        "expected": {"ok": true},
                        "agent_answer": {"ok": true}
                    }
                })
            })
            .collect::<Vec<_>>();
        let mut value = json!({
            "schema_version": 1,
            "mode": mode,
            "track": "protocol",
            "run_id": "fixture",
            "result_file": "result.jsonl",
            "seed": 1,
            "pairs_per_scenario": 120,
            "local": {
                "executable": "/bin/anyr",
                "arguments": ["mcp"],
                "executable_sha256": "0".repeat(64),
                "environment": {},
                "credentials": [],
                "artifact": {"kind": "local", "revision": "a".repeat(40), "features": ["benchmark-harness"]}
            },
            "upstream": {
                "executable": "/bin/node",
                "arguments": ["/opt/npm/package/bin/cli.mjs", "run"],
                "executable_sha256": "1".repeat(64),
                "environment": {"ANYTYPE_API_BASE_URL": "http://127.0.0.1:31009"},
                "credentials": [{"source_fd": 4, "child_environment": "ANYTYPE_API_KEY"}],
                "artifact": {
                    "kind": "official-npm",
                    "package": "@anyproto/anytype-mcp",
                    "version": "1.2.10",
                    "integrity": format!("sha512-{}", "A".repeat(88)),
                    "tarball_path": "/opt/npm/package.tgz",
                    "tarball_sha256": "2".repeat(64),
                    "tarball_bundle_entry": "package/bin/cli.mjs",
                    "bundle_path": "/opt/npm/package/bin/cli.mjs",
                    "bundle_sha256": "3".repeat(64)
                }
            },
            "oracle": {
                "base_url": "http://127.0.0.1:31009/",
                "credential_fd": 3,
                "credential_header": "Authorization",
                "openapi_path": "/docs/openapi.json"
            },
            "manifest": {
                "local_revision": "a".repeat(40),
                "local_features": ["benchmark-harness"],
                "rust_toolchain": "rustc fixture",
                "node_runtime": "node fixture",
                "anytype_cli_version": "fixture",
                "anytype_cli_path": "/bin/anytype",
                "anytype_cli_sha256": "4".repeat(64),
                "heart_version": "fixture",
                "heart_path": "/bin/heart",
                "heart_sha256": "5".repeat(64)
            },
            "ancestry": {
                "repository_path": "/repo",
                "integrated_revision": "a".repeat(40),
                "required_ancestors": {
                    "any-3d4m": "b".repeat(40),
                    "any-ljmz": "c".repeat(40),
                    "any-mhe6": "d".repeat(40)
                }
            },
            "scenarios": scenarios
        });
        if mode == "controlled-fixed-spec-warm" {
            value["controlled_spec"] = json!({
                "path": "/opt/npm/openapi.json",
                "sha256": "6".repeat(64)
            });
            value["upstream"]["arguments"] = json!([
                "/opt/npm/package/bin/cli.mjs",
                "run",
                "/opt/npm/openapi.json"
            ]);
        }
        value
    }

    fn parse(value: Value) -> Result<Config, String> {
        let config: Config = serde_json::from_value(value)
            .map_err(|error| format!("fixture parse failed: {error}"))?;
        config.validate()?;
        Ok(config)
    }

    #[test]
    fn separates_production_and_controlled_spec_modes() {
        parse(fixture("production-verified-official")).expect("valid production fixture");
        parse(fixture("controlled-fixed-spec-warm")).expect("valid controlled fixture");

        let mut smuggled = fixture("production-verified-official");
        smuggled["upstream"]["arguments"] = json!([
            "/opt/npm/package/bin/cli.mjs",
            "run",
            "/opt/npm/openapi.json"
        ]);
        assert!(parse(smuggled).is_err());

        let mut relative = fixture("controlled-fixed-spec-warm");
        relative["controlled_spec"]["path"] = json!("openapi.json");
        assert!(parse(relative).is_err());
    }

    #[test]
    fn credential_seams_cannot_select_behavior() {
        let mut value = fixture("production-verified-official");
        value["upstream"]["credentials"][0]["child_environment"] = json!("ANY_MCP_PROFILE");
        assert!(parse(value).is_err());
    }

    #[test]
    fn unknown_environment_diagnostic_does_not_echo_the_name() {
        let mut value = fixture("production-verified-official");
        value["local"]["environment"]["OPERATOR_SECRET_NAME"] = json!("value");
        let error = parse(value).expect_err("unknown environment rejected");
        assert!(!error.contains("OPERATOR_SECRET_NAME"));
    }
}
