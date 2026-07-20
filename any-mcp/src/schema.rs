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
    let root = serde_json::Value::Object(schema.as_ref().clone());
    let all_values_are_bounded = strict_wire_schema(&root, &root);

    if dialect_is_current && root_is_object && all_values_are_bounded {
        Ok(schema)
    } else {
        Err(SchemaContractError)
    }
}

const MAX_SCHEMA_STRING_CHARS: u64 = 100_000;
const MAX_SCHEMA_ARRAY_ITEMS: u64 = 10_000;
const MAX_SCHEMA_ENUM_VALUES: usize = 128;
const MAX_SCHEMA_NUMBER_ABS: f64 = 1_000_000_000_000_000.0;

fn strict_wire_schema(value: &serde_json::Value, root: &serde_json::Value) -> bool {
    let serde_json::Value::Object(schema) = value else {
        return value == &serde_json::Value::Bool(false);
    };
    if schema.is_empty() {
        return false;
    }
    if !validate_definitions(schema, root) {
        return false;
    }

    if let Some(reference) = schema.get("$ref").and_then(serde_json::Value::as_str) {
        return reference
            .strip_prefix("#/$defs/")
            .map(|name| format!("/$defs/{name}"))
            .and_then(|pointer| root.pointer(&pointer))
            .is_some_and(|target| {
                matches!(
                    target,
                    serde_json::Value::Object(_) | serde_json::Value::Bool(false)
                )
            });
    }

    match schema.get("type") {
        Some(serde_json::Value::String(kind)) => match kind.as_str() {
            "object" => validate_object_schema(schema, root),
            "array" => validate_array_schema(schema, root),
            "string" => validate_string_schema(schema),
            "integer" | "number" => validate_numeric_schema(schema),
            "boolean" | "null" => true,
            _ => false,
        },
        Some(serde_json::Value::Array(types)) => validate_nullable_type_array(schema, types, root),
        Some(_) => false,
        None => validate_untyped_schema(schema, root),
    }
}

fn validate_nullable_type_array(
    schema: &JsonObject,
    types: &[serde_json::Value],
    root: &serde_json::Value,
) -> bool {
    if types.len() != 2 || !types.iter().any(|kind| kind.as_str() == Some("null")) {
        return false;
    }
    let Some(kind) = types.iter().find_map(|kind| {
        let kind = kind.as_str()?;
        (kind != "null").then_some(kind)
    }) else {
        return false;
    };
    let mut normalized = schema.clone();
    normalized.insert(
        "type".to_owned(),
        serde_json::Value::String(kind.to_owned()),
    );
    strict_wire_schema(&serde_json::Value::Object(normalized), root)
}

fn validate_definitions(schema: &JsonObject, root: &serde_json::Value) -> bool {
    match schema.get("$defs") {
        None => true,
        Some(serde_json::Value::Object(definitions)) => {
            !definitions.is_empty()
                && definitions.values().all(|definition| {
                    definition.get("$ref").is_none() && strict_wire_schema(definition, root)
                })
        }
        Some(_) => false,
    }
}

fn validate_object_schema(schema: &JsonObject, root: &serde_json::Value) -> bool {
    if schema.get("additionalProperties") != Some(&serde_json::Value::Bool(false)) {
        return false;
    }
    match schema.get("properties") {
        None => true,
        Some(serde_json::Value::Object(properties)) => properties.values().all(|property| {
            let documented = property
                .get("description")
                .and_then(serde_json::Value::as_str)
                .is_some()
                || scalar_const(property).is_some();
            documented && strict_wire_schema(property, root)
        }),
        Some(_) => false,
    }
}

fn validate_array_schema(schema: &JsonObject, root: &serde_json::Value) -> bool {
    let Some(max_items) = schema
        .get("maxItems")
        .and_then(serde_json::Value::as_u64)
        .filter(|maximum| *maximum <= MAX_SCHEMA_ARRAY_ITEMS)
    else {
        return false;
    };
    let minimum_is_valid = schema
        .get("minItems")
        .and_then(serde_json::Value::as_u64)
        .is_none_or(|minimum| minimum <= max_items);
    let items_are_strict = schema
        .get("items")
        .is_some_and(|items| strict_wire_schema(items, root));
    minimum_is_valid && items_are_strict
}

fn validate_string_schema(schema: &JsonObject) -> bool {
    if let Some(values) = schema.get("enum").and_then(serde_json::Value::as_array) {
        return validate_enum_values(values);
    }
    if let Some(value) = schema.get("const") {
        return validate_scalar(value);
    }
    let Some(maximum) = schema
        .get("maxLength")
        .and_then(serde_json::Value::as_u64)
        .filter(|maximum| *maximum <= MAX_SCHEMA_STRING_CHARS)
    else {
        return false;
    };
    schema
        .get("minLength")
        .and_then(serde_json::Value::as_u64)
        .is_none_or(|minimum| minimum <= maximum)
}

fn validate_numeric_schema(schema: &JsonObject) -> bool {
    if let Some(values) = schema.get("enum").and_then(serde_json::Value::as_array) {
        return validate_enum_values(values);
    }
    if let Some(value) = schema.get("const") {
        return validate_scalar(value);
    }
    let minimum = schema
        .get("minimum")
        .or_else(|| schema.get("exclusiveMinimum"))
        .and_then(serde_json::Value::as_f64);
    let maximum = schema
        .get("maximum")
        .or_else(|| schema.get("exclusiveMaximum"))
        .and_then(serde_json::Value::as_f64);
    matches!((minimum, maximum), (Some(minimum), Some(maximum))
        if minimum.is_finite()
            && maximum.is_finite()
            && minimum <= maximum
            && minimum.abs() <= MAX_SCHEMA_NUMBER_ABS
            && maximum.abs() <= MAX_SCHEMA_NUMBER_ABS)
}

fn validate_untyped_schema(schema: &JsonObject, root: &serde_json::Value) -> bool {
    if let Some(value) = schema.get("const") {
        return validate_scalar(value);
    }
    if let Some(values) = schema.get("enum").and_then(serde_json::Value::as_array) {
        return validate_enum_values(values);
    }
    if let Some(branches) = schema.get("allOf").and_then(serde_json::Value::as_array) {
        return !branches.is_empty()
            && branches
                .iter()
                .all(|branch| strict_wire_schema(branch, root));
    }
    for keyword in ["oneOf", "anyOf"] {
        if let Some(branches) = schema.get(keyword).and_then(serde_json::Value::as_array) {
            let strict_branches = branches.len() >= 2
                && branches
                    .iter()
                    .all(|branch| strict_wire_schema(branch, root));
            return strict_branches
                && (is_nullable_union(branches)
                    || is_scalar_enum_union(branches, root)
                    || is_discriminated_union(branches, root));
        }
    }
    false
}

fn is_scalar_enum_union(branches: &[serde_json::Value], root: &serde_json::Value) -> bool {
    if branches.len() > MAX_SCHEMA_ENUM_VALUES {
        return false;
    }
    let mut values = Vec::with_capacity(branches.len());
    for branch in branches {
        let Some(value) = resolve_schema(branch, root).and_then(scalar_const) else {
            return false;
        };
        if values.contains(&value) {
            return false;
        }
        values.push(value);
    }
    true
}

fn is_nullable_union(branches: &[serde_json::Value]) -> bool {
    branches.len() == 2
        && branches
            .iter()
            .filter(|branch| branch.get("type").and_then(serde_json::Value::as_str) == Some("null"))
            .count()
            == 1
}

fn is_discriminated_union(branches: &[serde_json::Value], root: &serde_json::Value) -> bool {
    let Some(first) = resolve_schema(&branches[0], root).and_then(serde_json::Value::as_object)
    else {
        return false;
    };
    let Some(first_properties) = first
        .get("properties")
        .and_then(serde_json::Value::as_object)
    else {
        return false;
    };

    first_properties.iter().any(|(name, property)| {
        let Some(first_tag) = scalar_const(property) else {
            return false;
        };
        if !property_is_required(first, name) {
            return false;
        }
        let mut tags = vec![first_tag];
        for branch in &branches[1..] {
            let Some(schema) = resolve_schema(branch, root).and_then(serde_json::Value::as_object)
            else {
                return false;
            };
            let tag = schema
                .get("properties")
                .and_then(serde_json::Value::as_object)
                .and_then(|properties| properties.get(name))
                .and_then(scalar_const);
            let Some(tag) = tag else {
                return false;
            };
            if !property_is_required(schema, name) || tags.contains(&tag) {
                return false;
            }
            tags.push(tag);
        }
        true
    })
}

fn resolve_schema<'a>(
    schema: &'a serde_json::Value,
    root: &'a serde_json::Value,
) -> Option<&'a serde_json::Value> {
    let Some(reference) = schema.get("$ref") else {
        return Some(schema);
    };
    root.pointer(reference.as_str()?.strip_prefix('#')?)
}

fn property_is_required(schema: &JsonObject, name: &str) -> bool {
    schema
        .get("required")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|required| required.iter().any(|item| item.as_str() == Some(name)))
}

fn scalar_const(schema: &serde_json::Value) -> Option<&serde_json::Value> {
    schema
        .get("const")
        .filter(|value| validate_scalar(value))
        .or_else(|| {
            let values = schema.get("enum")?.as_array()?;
            (values.len() == 1 && validate_scalar(&values[0])).then_some(&values[0])
        })
}

fn validate_enum_values(values: &[serde_json::Value]) -> bool {
    !values.is_empty()
        && values.len() <= MAX_SCHEMA_ENUM_VALUES
        && values.iter().all(validate_scalar)
}

fn validate_scalar(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Null | serde_json::Value::Bool(_) => true,
        serde_json::Value::String(value) => {
            value.chars().count() <= MAX_SCHEMA_STRING_CHARS as usize
        }
        serde_json::Value::Number(value) => value
            .as_f64()
            .is_some_and(|number| number.is_finite() && number.abs() <= MAX_SCHEMA_NUMBER_ABS),
        serde_json::Value::Array(_) | serde_json::Value::Object(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use std::{borrow::Cow, collections::BTreeMap};

    use rmcp::schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
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

    struct BooleanTrueSchema;

    impl JsonSchema for BooleanTrueSchema {
        fn schema_name() -> Cow<'static, str> {
            "BooleanTrueSchema".into()
        }

        fn json_schema(_: &mut SchemaGenerator) -> Schema {
            json_schema!(true)
        }
    }

    struct EmptySchema;

    impl JsonSchema for EmptySchema {
        fn schema_name() -> Cow<'static, str> {
            "EmptySchema".into()
        }

        fn json_schema(_: &mut SchemaGenerator) -> Schema {
            json_schema!({})
        }
    }

    #[derive(JsonSchema)]
    #[serde(deny_unknown_fields)]
    #[expect(dead_code, reason = "schema-only boolean-true test model")]
    struct BooleanTrueInput {
        /// An intentionally unconstrained nested schema.
        value: BooleanTrueSchema,
    }

    #[derive(JsonSchema)]
    #[serde(deny_unknown_fields)]
    #[expect(dead_code, reason = "schema-only empty test model")]
    struct EmptyNestedInput {
        /// An intentionally empty nested schema.
        value: EmptySchema,
    }

    #[derive(JsonSchema)]
    #[serde(deny_unknown_fields)]
    #[expect(dead_code, reason = "schema-only free-form value test model")]
    struct FreeFormValueInput {
        /// An intentionally free-form JSON value.
        value: serde_json::Value,
    }

    #[derive(JsonSchema)]
    #[serde(deny_unknown_fields)]
    #[expect(dead_code, reason = "schema-only free-form map test model")]
    struct FreeFormMapInput {
        /// An intentionally open-ended property map.
        values: BTreeMap<String, DisplayName>,
    }

    #[derive(JsonSchema)]
    #[serde(untagged, deny_unknown_fields)]
    #[expect(dead_code, reason = "schema-only untagged union test model")]
    enum UntaggedSelector {
        ById {
            /// Object identifier branch.
            object_id: ObjectId,
        },
        ByName {
            /// Object name branch.
            name: DisplayName,
        },
    }

    #[derive(JsonSchema)]
    #[serde(deny_unknown_fields)]
    #[expect(dead_code, reason = "schema-only untagged wrapper test model")]
    struct UntaggedUnionInput {
        /// An intentionally undiscriminated selector.
        selector: UntaggedSelector,
    }

    #[derive(JsonSchema)]
    #[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
    #[expect(dead_code, reason = "schema-only tagged union test model")]
    enum TaggedSelector {
        ById {
            /// Stable object identifier.
            object_id: ObjectId,
        },
        ByName {
            /// Bounded object name.
            name: DisplayName,
        },
    }

    #[derive(JsonSchema)]
    #[serde(deny_unknown_fields)]
    #[expect(dead_code, reason = "schema-only tagged wrapper test model")]
    struct TaggedUnionInput {
        /// Explicitly discriminated selector.
        selector: TaggedSelector,
    }

    #[derive(JsonSchema)]
    #[serde(deny_unknown_fields)]
    #[expect(dead_code, reason = "schema-only numeric bounds test model")]
    struct UnboundedNumericInput {
        /// An integer with only primitive machine bounds.
        value: i64,
    }

    #[test]
    fn representative_input_schema_is_exact_and_bounded() {
        let schema = input_schema::<RepresentativeInput>().unwrap();

        assert_eq!(schema.get("$schema"), Some(&json!(JSON_SCHEMA_DIALECT)));
        assert_eq!(schema.get("additionalProperties"), Some(&json!(false)));
        assert_eq!(schema["required"], json!(["object_id"]));
        assert_eq!(
            schema["$defs"]["ObjectId"]["pattern"],
            json!("^(?!\\.{1,2}$)[A-Za-z0-9._~-]+$")
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

    #[test]
    fn unconstrained_nested_schema_forms_are_rejected() {
        assert_eq!(input_schema::<BooleanTrueInput>(), Err(SchemaContractError));
        assert_eq!(input_schema::<EmptyNestedInput>(), Err(SchemaContractError));
        assert_eq!(
            input_schema::<FreeFormValueInput>(),
            Err(SchemaContractError)
        );
        assert_eq!(input_schema::<FreeFormMapInput>(), Err(SchemaContractError));
    }

    #[test]
    fn untagged_union_and_impractical_numeric_bounds_are_rejected() {
        assert_eq!(
            input_schema::<UntaggedUnionInput>(),
            Err(SchemaContractError)
        );
        assert_eq!(
            input_schema::<UnboundedNumericInput>(),
            Err(SchemaContractError)
        );
    }

    #[test]
    fn bounded_tagged_union_is_accepted() {
        let schema = input_schema::<TaggedUnionInput>().unwrap();
        assert!(schema["$defs"]["TaggedSelector"]["oneOf"].is_array());
    }
}
