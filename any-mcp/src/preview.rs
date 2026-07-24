// any-mcp - bounded, workflow-oriented MCP server for Anytype
//
// SPDX-FileCopyrightText: 2026 Steve Schoettler
// SPDX-License-Identifier: Apache-2.0

//! Transport-neutral experimental 2026-07-28 preview request adapter.
//!
//! Exactly one copy of the preview decoder, `_meta` validation, and method
//! routing serves both the stdio framing loop and the stateless HTTP POST
//! path. Transports provide one bounded frame and consume one bounded
//! response value; neither owns duplicate method routing, so a final
//! preview-contract change lands here once.

use std::{collections::HashMap, sync::Arc};

use rmcp::model::{
    CallToolRequestParams, ErrorData, PaginatedRequestParams, ReadResourceRequestParams,
};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use crate::server::AnyMcpServer;

pub(crate) const MODERN_VERSION: &str = "2026-07-28";
pub(crate) const CACHE_CATALOG_TTL_MS: u64 = 3_600_000;
pub(crate) const CACHE_PRIVATE_TTL_MS: u64 = 0;

pub(crate) const META_PROTOCOL_VERSION: &str = "io.modelcontextprotocol/protocolVersion";
pub(crate) const META_CLIENT_INFO: &str = "io.modelcontextprotocol/clientInfo";
pub(crate) const META_CLIENT_CAPABILITIES: &str = "io.modelcontextprotocol/clientCapabilities";
pub(crate) const META_SERVER_INFO: &str = "io.modelcontextprotocol/serverInfo";

pub(crate) type CancellationMap = Arc<Mutex<HashMap<String, CancellationToken>>>;

/// One decoded, meta-validated preview request ready for dispatch.
pub(crate) struct PreviewRequest {
    /// Validated JSON-RPC request ID.
    pub id: Value,
    /// Exact request key used by the cancellation registry.
    pub request_key: String,
    /// Requested method name.
    pub method: String,
    /// Request params with `_meta` still attached; dispatch removes it.
    pub params: Map<String, Value>,
}

/// Transport-neutral classification of one bounded preview frame.
pub(crate) enum PreviewClassification {
    /// A complete JSON-RPC response to emit without dispatching.
    Response(Value),
    /// A well-formed `notifications/cancelled` naming this request key.
    CancelRequested(String),
    /// A notification the preview intentionally ignores.
    Ignored,
    /// A validated request for the transport to dispatch.
    Request(PreviewRequest),
}

/// Classifies one preview frame with the exact stdio-reviewed gate order.
pub(crate) fn classify_preview_frame(frame: &[u8]) -> PreviewClassification {
    let Ok(value) = serde_json::from_slice::<Value>(frame) else {
        return PreviewClassification::Response(parse_error());
    };
    let Some(object) = value.as_object() else {
        return PreviewClassification::Response(invalid_request(Value::Null));
    };
    if object.get("jsonrpc") != Some(&Value::String("2.0".to_owned())) {
        let id = valid_id(object.get("id")).unwrap_or(Value::Null);
        return PreviewClassification::Response(invalid_request(id));
    }
    let Some(method) = object.get("method").and_then(Value::as_str) else {
        let id = valid_id(object.get("id")).unwrap_or(Value::Null);
        return PreviewClassification::Response(invalid_request(id));
    };

    let Some(id) = object.get("id") else {
        if method == "notifications/cancelled"
            && let Some(request_id) = object
                .get("params")
                .and_then(Value::as_object)
                .and_then(|params| params.get("requestId"))
                .and_then(|id| valid_id(Some(id)))
        {
            return PreviewClassification::CancelRequested(request_id.to_string());
        }
        return PreviewClassification::Ignored;
    };
    let Some(id) = valid_id(Some(id)) else {
        return PreviewClassification::Response(invalid_request(Value::Null));
    };
    let request_key = id.to_string();
    let Some(params) = object.get("params").and_then(Value::as_object).cloned() else {
        return PreviewClassification::Response(invalid_params(id));
    };
    if let Err(error) = validate_meta(&params) {
        return PreviewClassification::Response(error.into_response(id));
    }
    PreviewClassification::Request(PreviewRequest {
        id,
        request_key,
        method: method.to_owned(),
        params,
    })
}

pub(crate) async fn dispatch_modern(
    server: &AnyMcpServer,
    id: Value,
    method: &str,
    mut params: Map<String, Value>,
    cancellation: &CancellationToken,
) -> Value {
    params.remove("_meta");
    let result = match method {
        "server/discover" => {
            if !params.is_empty() {
                return invalid_params(id);
            }
            Ok(json!({
                "resultType": "complete",
                "supportedVersions": [MODERN_VERSION],
                "capabilities": {"tools": {}, "resources": {}},
                "_meta": {
                    META_SERVER_INFO: {
                        "name": env!("CARGO_PKG_NAME"),
                        "version": env!("CARGO_PKG_VERSION")
                    }
                },
                "instructions": "Bounded, workflow-oriented access to Anytype",
                "ttlMs": CACHE_CATALOG_TTL_MS,
                "cacheScope": "public"
            }))
        }
        "tools/list" => decode::<ListParams>(params)
            .and_then(|params| server.list_tools_wire(params.into_rmcp()))
            .and_then(encode_result)
            .map(|mut result| {
                add_complete(&mut result);
                add_cache(&mut result, CACHE_CATALOG_TTL_MS, "public");
                result
            }),
        "tools/call" => match decode::<ToolCallParams>(params) {
            Ok(params) if params.request_state.is_none() && params.input_responses.is_none() => {
                Box::pin(server.dispatch_tool_for_protocol(
                    params.into_rmcp(),
                    &rmcp::model::ProtocolVersion::V_2026_07_28,
                    cancellation,
                ))
                .await
                .and_then(encode_result)
                .map(|mut result| {
                    add_complete(&mut result);
                    result
                })
            }
            Ok(_) | Err(_) => Err(validation_error()),
        },
        "resources/list" => decode::<ListParams>(params)
            .and_then(|params| server.list_resources_wire(params.into_rmcp()))
            .and_then(encode_result)
            .map(|mut result| {
                add_complete(&mut result);
                add_cache(&mut result, CACHE_CATALOG_TTL_MS, "public");
                result
            }),
        "resources/templates/list" => decode::<ListParams>(params)
            .and_then(|params| server.list_resource_templates_wire(params.into_rmcp()))
            .and_then(encode_result)
            .map(|mut result| {
                add_complete(&mut result);
                add_cache(&mut result, CACHE_CATALOG_TTL_MS, "public");
                result
            }),
        "resources/read" => match decode::<ResourceReadParams>(params) {
            Ok(params) => server
                .read_resource_wire(params.into_rmcp(), cancellation)
                .await
                .and_then(encode_result)
                .map(|mut result| {
                    add_complete(&mut result);
                    add_cache(&mut result, CACHE_PRIVATE_TTL_MS, "private");
                    result
                }),
            Err(error) => Err(error),
        },
        _ => return method_not_found(id),
    };

    match result {
        Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
        Err(error) => error_response(id, error),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ListParams {
    cursor: Option<String>,
}

impl ListParams {
    fn into_rmcp(self) -> Option<PaginatedRequestParams> {
        Some(PaginatedRequestParams::default().with_cursor(self.cursor))
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ToolCallParams {
    name: String,
    arguments: Option<Map<String, Value>>,
    input_responses: Option<Map<String, Value>>,
    request_state: Option<String>,
}

impl ToolCallParams {
    fn into_rmcp(self) -> CallToolRequestParams {
        let request = CallToolRequestParams::new(self.name);
        if let Some(arguments) = self.arguments {
            request.with_arguments(arguments)
        } else {
            request
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ResourceReadParams {
    uri: String,
}

impl ResourceReadParams {
    fn into_rmcp(self) -> ReadResourceRequestParams {
        ReadResourceRequestParams::new(self.uri)
    }
}

fn decode<T: for<'de> Deserialize<'de>>(params: Map<String, Value>) -> Result<T, ErrorData> {
    serde_json::from_value(Value::Object(params)).map_err(|_| validation_error())
}

fn encode_result<T: serde::Serialize>(result: T) -> Result<Value, ErrorData> {
    serde_json::to_value(result).map_err(|_| ErrorData::internal_error("Internal error", None))
}

fn add_complete(result: &mut Value) {
    if let Some(result) = result.as_object_mut() {
        result.insert(
            "resultType".to_owned(),
            Value::String("complete".to_owned()),
        );
    }
}

fn add_cache(result: &mut Value, ttl_ms: u64, scope: &str) {
    if let Some(result) = result.as_object_mut() {
        result.insert("ttlMs".to_owned(), Value::from(ttl_ms));
        result.insert("cacheScope".to_owned(), Value::String(scope.to_owned()));
    }
}

pub(crate) enum MetaError {
    Invalid,
    Unsupported(String),
}

impl MetaError {
    pub(crate) fn into_response(self, id: Value) -> Value {
        match self {
            Self::Invalid => invalid_params(id),
            Self::Unsupported(requested) => json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": {
                    "code": -32022,
                    "message": "Unsupported protocol version",
                    "data": {"supported": [MODERN_VERSION], "requested": requested}
                }
            }),
        }
    }
}

pub(crate) fn validate_meta(params: &Map<String, Value>) -> Result<(), MetaError> {
    let meta = params
        .get("_meta")
        .and_then(Value::as_object)
        .ok_or(MetaError::Invalid)?;
    let version = meta
        .get(META_PROTOCOL_VERSION)
        .and_then(Value::as_str)
        .filter(|version| version.len() <= 64)
        .ok_or(MetaError::Invalid)?;
    if version != MODERN_VERSION {
        return Err(MetaError::Unsupported(version.to_owned()));
    }
    if let Some(client_info) = meta.get(META_CLIENT_INFO) {
        validate_client_info(client_info.as_object().ok_or(MetaError::Invalid)?)?;
    }
    let client_capabilities = meta
        .get(META_CLIENT_CAPABILITIES)
        .and_then(Value::as_object)
        .ok_or(MetaError::Invalid)?;
    validate_client_capabilities(client_capabilities)?;
    if let Some(level) = meta.get("io.modelcontextprotocol/logLevel")
        && !matches!(
            level.as_str(),
            Some(
                "debug"
                    | "info"
                    | "notice"
                    | "warning"
                    | "error"
                    | "critical"
                    | "alert"
                    | "emergency"
            )
        )
    {
        return Err(MetaError::Invalid);
    }
    if let Some(progress) = meta.get("progressToken") {
        let valid = match progress {
            Value::String(_) => true,
            Value::Number(number) => number.is_i64() || number.is_u64(),
            _ => false,
        };
        if !valid {
            return Err(MetaError::Invalid);
        }
    }
    Ok(())
}

fn validate_client_info(client_info: &Map<String, Value>) -> Result<(), MetaError> {
    for field in ["name", "version"] {
        client_info
            .get(field)
            .and_then(Value::as_str)
            .filter(|value| value.len() <= 256)
            .ok_or(MetaError::Invalid)?;
    }
    for field in ["title", "description", "websiteUrl"] {
        if let Some(value) = client_info.get(field)
            && !value.as_str().is_some_and(|value| value.len() <= 4_096)
        {
            return Err(MetaError::Invalid);
        }
    }
    if let Some(icons) = client_info.get("icons") {
        let icons = icons.as_array().ok_or(MetaError::Invalid)?;
        if icons.len() > 16 {
            return Err(MetaError::Invalid);
        }
        for icon in icons {
            let icon = icon.as_object().ok_or(MetaError::Invalid)?;
            if !icon
                .get("src")
                .and_then(Value::as_str)
                .is_some_and(|src| !src.is_empty() && src.len() <= 4_096)
            {
                return Err(MetaError::Invalid);
            }
            if let Some(mime_type) = icon.get("mimeType")
                && !mime_type.as_str().is_some_and(|value| value.len() <= 256)
            {
                return Err(MetaError::Invalid);
            }
            if let Some(sizes) = icon.get("sizes") {
                let sizes = sizes.as_array().ok_or(MetaError::Invalid)?;
                if sizes.len() > 32
                    || sizes
                        .iter()
                        .any(|size| !size.as_str().is_some_and(|value| value.len() <= 64))
                {
                    return Err(MetaError::Invalid);
                }
            }
            if let Some(theme) = icon.get("theme")
                && !matches!(theme.as_str(), Some("light" | "dark"))
            {
                return Err(MetaError::Invalid);
            }
        }
    }
    Ok(())
}

fn validate_client_capabilities(capabilities: &Map<String, Value>) -> Result<(), MetaError> {
    for field in [
        "sampling",
        "roots",
        "elicitation",
        "experimental",
        "extensions",
    ] {
        if let Some(value) = capabilities.get(field)
            && !value.is_object()
        {
            return Err(MetaError::Invalid);
        }
    }
    for field in ["experimental", "extensions"] {
        if let Some(entries) = capabilities.get(field).and_then(Value::as_object)
            && entries.values().any(|value| !value.is_object())
        {
            return Err(MetaError::Invalid);
        }
    }
    if let Some(roots) = capabilities.get("roots").and_then(Value::as_object)
        && let Some(list_changed) = roots.get("listChanged")
        && !list_changed.is_boolean()
    {
        return Err(MetaError::Invalid);
    }
    if let Some(elicitation) = capabilities.get("elicitation").and_then(Value::as_object) {
        for field in ["form", "url"] {
            if let Some(value) = elicitation.get(field)
                && !value.is_object()
            {
                return Err(MetaError::Invalid);
            }
        }
    }
    Ok(())
}

pub(crate) async fn cancel_all(cancellations: &CancellationMap) {
    let mut active = cancellations.lock().await;
    for cancellation in active.values() {
        cancellation.cancel();
    }
    active.clear();
}

pub(crate) fn valid_id(id: Option<&Value>) -> Option<Value> {
    match id? {
        Value::String(id) if id.len() <= 256 => Some(Value::String(id.clone())),
        Value::Number(id) if id.is_i64() || id.is_u64() => Some(Value::Number(id.clone())),
        _ => None,
    }
}

fn validation_error() -> ErrorData {
    ErrorData::invalid_params("Invalid params", None)
}

fn error_response(id: Value, error: ErrorData) -> Value {
    let error = serde_json::to_value(error)
        .unwrap_or_else(|_| json!({"code": -32603, "message": "Internal error"}));
    json!({"jsonrpc": "2.0", "id": id, "error": error})
}

pub(crate) fn parse_error() -> Value {
    json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32700, "message": "Parse error"}})
}

pub(crate) fn invalid_request(id: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32600, "message": "Invalid request"}})
}

pub(crate) fn invalid_params(id: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32602, "message": "Invalid params"}})
}

pub(crate) fn method_not_found(id: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": "Method not found"}})
}

pub(crate) fn internal_error(id: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32603, "message": "Internal error"}})
}
