// any-mcp - bounded, workflow-oriented MCP server for Anytype
//
// SPDX-FileCopyrightText: 2026 Steve Schoettler
// SPDX-License-Identifier: Apache-2.0

//! Strict JSON Schema generation for MCP tool contracts.

use std::{fmt, sync::Arc};

use rmcp::{
    handler::server::tool::{schema_for_input, schema_for_output},
    model::JsonObject,
    schemars::JsonSchema,
};

/// JSON Schema dialect required for every `any-mcp` tool contract.
pub const JSON_SCHEMA_DIALECT: &str = "https://json-schema.org/draft/2020-12/schema";

/// Error returned when a Rust wire model is not a valid MCP object schema.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SchemaContractError;

impl fmt::Display for SchemaContractError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("tool schema must be a strict JSON Schema 2020-12 object")
    }
}

impl std::error::Error for SchemaContractError {}

/// Generates an MCP input schema for a strict object model.
///
/// Wire structs must use `serde(deny_unknown_fields)` so the generated root
/// includes `additionalProperties: false` and runtime deserialization rejects
/// the same unknown fields.
pub fn input_schema<T>() -> Result<Arc<JsonObject>, SchemaContractError>
where
    T: JsonSchema + 'static,
{
    schema_for_input::<T>()
        .map_err(|_| SchemaContractError)
        .and_then(require_strict_root)
}

/// Generates an MCP output schema for a strict object model.
///
/// The same schema can be attached to an `rmcp::model::Tool` and used to
/// validate the value returned in `structuredContent`.
pub fn output_schema<T>() -> Result<Arc<JsonObject>, SchemaContractError>
where
    T: JsonSchema + 'static,
{
    schema_for_output::<T>()
        .map_err(|_| SchemaContractError)
        .and_then(require_strict_root)
}

fn require_strict_root(schema: Arc<JsonObject>) -> Result<Arc<JsonObject>, SchemaContractError> {
    let dialect_is_current =
        schema.get("$schema").and_then(serde_json::Value::as_str) == Some(JSON_SCHEMA_DIALECT);
    let root_is_object = schema.get("type").and_then(serde_json::Value::as_str) == Some("object");
    let all_values_are_bounded =
        strict_wire_schema(&serde_json::Value::Object(schema.as_ref().clone()));

    if dialect_is_current && root_is_object && all_values_are_bounded {
        Ok(schema)
    } else {
        Err(SchemaContractError)
    }
}

fn strict_wire_schema(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Array(values) => values.iter().all(strict_wire_schema),
        serde_json::Value::Object(object) => {
            let bounded_here = match object.get("type").and_then(serde_json::Value::as_str) {
                Some("object") => {
                    object.get("additionalProperties") == Some(&serde_json::Value::Bool(false))
                        && object
                            .get("properties")
                            .and_then(serde_json::Value::as_object)
                            .is_none_or(|properties| {
                                properties.values().all(|property| {
                                    property
                                        .get("description")
                                        .and_then(serde_json::Value::as_str)
                                        .is_some()
                                })
                            })
                }
                Some("array") => object.contains_key("maxItems"),
                Some("string") => {
                    object.contains_key("maxLength")
                        || object.contains_key("enum")
                        || object.contains_key("const")
                }
                _ => true,
            };
            bounded_here && object.values().all(strict_wire_schema)
        }
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use rmcp::schemars::JsonSchema;
    use serde::{Deserialize, Serialize};
    use serde_json::json;

    use super::*;
    use crate::domain::{DisplayName, ObjectId};

    #[derive(Debug, Serialize, Deserialize, JsonSchema)]
    #[serde(deny_unknown_fields)]
    struct RepresentativeInput {
        /// Stable object identifier to retrieve.
        object_id: ObjectId,
        /// Optional bounded display label supplied by the caller.
        label: Option<DisplayName>,
    }

    #[derive(Debug, Serialize, JsonSchema)]
    #[serde(deny_unknown_fields)]
    struct RepresentativeOutput {
        /// Whether the workflow completed.
        complete: bool,
    }

    #[derive(JsonSchema)]
    #[expect(dead_code, reason = "schema-only permissive test model")]
    struct PermissiveInput {
        value: String,
    }

    #[derive(JsonSchema)]
    #[serde(deny_unknown_fields)]
    #[expect(dead_code, reason = "schema-only unbounded test model")]
    struct UnboundedInput {
        /// An intentionally unbounded string.
        value: String,
    }

    #[test]
    fn representative_input_schema_is_exact_and_bounded() {
        let schema = input_schema::<RepresentativeInput>().unwrap();

        assert_eq!(schema.get("$schema"), Some(&json!(JSON_SCHEMA_DIALECT)));
        assert_eq!(schema.get("additionalProperties"), Some(&json!(false)));
        assert_eq!(schema["required"], json!(["object_id"]));
        assert_eq!(
            schema["$defs"]["ObjectId"]["pattern"],
            json!("^[A-Za-z0-9._~-]+$")
        );
        assert_eq!(schema["$defs"]["BoundedText512"]["maxLength"], json!(512));
        assert_eq!(
            schema["properties"]["object_id"]["description"],
            json!("Stable object identifier to retrieve.")
        );
    }

    #[test]
    fn output_schema_uses_the_same_strict_contract() {
        let schema = output_schema::<RepresentativeOutput>().unwrap();

        assert_eq!(schema.get("$schema"), Some(&json!(JSON_SCHEMA_DIALECT)));
        assert_eq!(schema.get("additionalProperties"), Some(&json!(false)));
        assert_eq!(schema["required"], json!(["complete"]));
    }

    #[test]
    fn permissive_root_schema_is_rejected() {
        assert_eq!(input_schema::<PermissiveInput>(), Err(SchemaContractError));
    }

    #[test]
    fn unbounded_string_schema_is_rejected() {
        assert_eq!(input_schema::<UnboundedInput>(), Err(SchemaContractError));
    }
}
