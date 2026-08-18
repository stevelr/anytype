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
    require_strict_root(schema_for_output::<T>())
}

fn require_strict_root(schema: Arc<JsonObject>) -> Result<Arc<JsonObject>, SchemaContractError> {
    let dialect_is_current =
        schema.get("$schema").and_then(serde_json::Value::as_str) == Some(JSON_SCHEMA_DIALECT);
    let root = serde_json::Value::Object(schema.as_ref().clone());
    let root_is_object = schema.get("type").and_then(serde_json::Value::as_str) == Some("object")
        || schema
            .get("oneOf")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|branches| is_discriminated_union(branches, &root));
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
const MAX_SCHEMA_NUMBER_ABS: f64 = 9_007_199_254_740_991.0;
const MAX_SCHEMA_ANNOTATION_CHARS: usize = 4_096;

const COMMON_SCHEMA_KEYWORDS: &[&str] = &[
    "$schema",
    "$defs",
    "title",
    "description",
    "$comment",
    "default",
    "examples",
    "deprecated",
    "readOnly",
    "writeOnly",
];

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
        if !has_only_keywords(schema, &["$ref"]) {
            return false;
        }
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

    if schema.get("type").and_then(serde_json::Value::as_str) == Some("object")
        && let Some(branches) = schema.get("oneOf").and_then(serde_json::Value::as_array)
    {
        return has_only_keywords(schema, &["type", "oneOf"])
            && branches.len() >= 2
            && branches
                .iter()
                .all(|branch| strict_wire_schema(branch, root))
            && is_discriminated_union(branches, root);
    }

    match schema.get("type") {
        Some(serde_json::Value::String(kind)) => match kind.as_str() {
            "object" => validate_object_schema(schema, root),
            "array" => validate_array_schema(schema, root),
            "string" => validate_string_schema(schema),
            "integer" | "number" => validate_numeric_schema(schema),
            "boolean" | "null" => validate_scalar_type_schema(schema),
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
    if !has_only_keywords(
        schema,
        &[
            "type",
            "properties",
            "required",
            "additionalProperties",
            "minProperties",
            "maxProperties",
        ],
    ) {
        return false;
    }
    if schema.get("additionalProperties") != Some(&serde_json::Value::Bool(false)) {
        return false;
    }
    let properties_are_strict = match schema.get("properties") {
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
    };
    let property_count = schema
        .get("properties")
        .and_then(serde_json::Value::as_object)
        .map_or(0, serde_json::Map::len) as u64;
    let minimum = match schema.get("minProperties") {
        Some(value) => match value.as_u64() {
            Some(value) => value,
            None => return false,
        },
        None => 0,
    };
    let maximum = match schema.get("maxProperties") {
        Some(value) => match value.as_u64() {
            Some(value) => value,
            None => return false,
        },
        None => property_count,
    };
    properties_are_strict
        && minimum <= maximum
        && maximum <= property_count
        && validate_required_properties(schema)
}

fn validate_array_schema(schema: &JsonObject, root: &serde_json::Value) -> bool {
    if !has_only_keywords(
        schema,
        &["type", "items", "minItems", "maxItems", "uniqueItems"],
    ) {
        return false;
    }
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
    let uniqueness_is_valid = schema
        .get("uniqueItems")
        .is_none_or(serde_json::Value::is_boolean);
    minimum_is_valid && items_are_strict && uniqueness_is_valid
}

fn validate_string_schema(schema: &JsonObject) -> bool {
    if !has_only_keywords(
        schema,
        &[
            "type",
            "minLength",
            "maxLength",
            "pattern",
            "format",
            "const",
            "enum",
        ],
    ) {
        return false;
    }
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
    let minimum_is_valid = schema
        .get("minLength")
        .and_then(serde_json::Value::as_u64)
        .is_none_or(|minimum| minimum <= maximum);
    let pattern_is_valid = schema.get("pattern").is_none_or(|pattern| {
        pattern
            .as_str()
            .is_some_and(|pattern| pattern.len() <= 1_024)
    });
    let format_is_valid = schema
        .get("format")
        .is_none_or(serde_json::Value::is_string);
    minimum_is_valid && pattern_is_valid && format_is_valid
}

fn validate_numeric_schema(schema: &JsonObject) -> bool {
    if !has_only_keywords(
        schema,
        &[
            "type",
            "minimum",
            "maximum",
            "exclusiveMinimum",
            "exclusiveMaximum",
            "multipleOf",
            "const",
            "enum",
        ],
    ) {
        return false;
    }
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
    let bounds_are_valid = matches!((minimum, maximum), (Some(minimum), Some(maximum))
        if minimum.is_finite()
            && maximum.is_finite()
            && minimum <= maximum
            && minimum.abs() <= MAX_SCHEMA_NUMBER_ABS
            && maximum.abs() <= MAX_SCHEMA_NUMBER_ABS);
    let multiple_is_valid = schema.get("multipleOf").is_none_or(|multiple| {
        multiple
            .as_f64()
            .is_some_and(|value| value.is_finite() && value > 0.0)
    });
    bounds_are_valid && multiple_is_valid
}

fn validate_untyped_schema(schema: &JsonObject, root: &serde_json::Value) -> bool {
    if let Some(value) = schema.get("const") {
        return has_only_keywords(schema, &["const"]) && validate_scalar(value);
    }
    if let Some(values) = schema.get("enum").and_then(serde_json::Value::as_array) {
        return has_only_keywords(schema, &["enum"]) && validate_enum_values(values);
    }
    if let Some(branches) = schema.get("allOf").and_then(serde_json::Value::as_array) {
        return has_only_keywords(schema, &["allOf"])
            && !branches.is_empty()
            && branches
                .iter()
                .all(|branch| strict_wire_schema(branch, root));
    }
    for keyword in ["oneOf", "anyOf"] {
        if let Some(branches) = schema.get(keyword).and_then(serde_json::Value::as_array) {
            let strict_branches = has_only_keywords(schema, &[keyword])
                && branches.len() >= 2
                && branches
                    .iter()
                    .all(|branch| strict_wire_schema(branch, root));
            return strict_branches
                && (is_nullable_union(branches)
                    || is_scalar_enum_union(branches, root)
                    || is_discriminated_union(branches, root)
                    || is_nonempty_array_object_union(branches, root));
        }
    }
    false
}

fn validate_scalar_type_schema(schema: &JsonObject) -> bool {
    if !has_only_keywords(schema, &["type", "const", "enum"]) {
        return false;
    }
    schema.get("const").is_none_or(validate_scalar)
        && schema.get("enum").is_none_or(|values| {
            values
                .as_array()
                .is_some_and(|values| validate_enum_values(values))
        })
}

fn has_only_keywords(schema: &JsonObject, structural: &[&str]) -> bool {
    schema.keys().all(|keyword| {
        COMMON_SCHEMA_KEYWORDS.contains(&keyword.as_str()) || structural.contains(&keyword.as_str())
    }) && validate_annotations(schema)
}

fn validate_annotations(schema: &JsonObject) -> bool {
    if !schema
        .get("$schema")
        .is_none_or(|dialect| dialect.as_str() == Some(JSON_SCHEMA_DIALECT))
    {
        return false;
    }
    for keyword in ["title", "description", "$comment"] {
        if !schema.get(keyword).is_none_or(|value| {
            value
                .as_str()
                .is_some_and(|value| value.chars().count() <= MAX_SCHEMA_ANNOTATION_CHARS)
        }) {
            return false;
        }
    }
    for keyword in ["deprecated", "readOnly", "writeOnly"] {
        if !schema
            .get(keyword)
            .is_none_or(serde_json::Value::is_boolean)
        {
            return false;
        }
    }
    if !schema.get("examples").is_none_or(|examples| {
        examples
            .as_array()
            .is_some_and(|examples| examples.len() <= 10)
    }) {
        return false;
    }
    true
}

fn validate_required_properties(schema: &JsonObject) -> bool {
    let Some(required) = schema.get("required") else {
        return true;
    };
    let Some(required) = required.as_array() else {
        return false;
    };
    let properties = schema
        .get("properties")
        .and_then(serde_json::Value::as_object);
    required.iter().all(|name| {
        name.as_str()
            .is_some_and(|name| properties.is_some_and(|properties| properties.contains_key(name)))
    })
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

fn is_nonempty_array_object_union(
    branches: &[serde_json::Value],
    root: &serde_json::Value,
) -> bool {
    if branches.len() != 2 {
        return false;
    }
    let Some(left) = resolve_schema(&branches[0], root).and_then(serde_json::Value::as_object)
    else {
        return false;
    };
    let Some(right) = resolve_schema(&branches[1], root).and_then(serde_json::Value::as_object)
    else {
        return false;
    };
    let Some(left_properties) = left
        .get("properties")
        .and_then(serde_json::Value::as_object)
    else {
        return false;
    };
    let Some(right_properties) = right
        .get("properties")
        .and_then(serde_json::Value::as_object)
    else {
        return false;
    };
    if left_properties.len() != right_properties.len()
        || !left_properties
            .keys()
            .all(|name| right_properties.contains_key(name))
    {
        return false;
    }
    let Some(left_required) = required_property_names(left) else {
        return false;
    };
    let Some(right_required) = required_property_names(right) else {
        return false;
    };
    let left_only = left_required
        .iter()
        .filter(|name| !right_required.contains(name))
        .copied()
        .collect::<Vec<_>>();
    let right_only = right_required
        .iter()
        .filter(|name| !left_required.contains(name))
        .copied()
        .collect::<Vec<_>>();
    if left_only.len() != 1 || right_only.len() != 1 || left_only[0] == right_only[0] {
        return false;
    }
    array_minimum(left_properties.get(left_only[0])) >= 1
        && array_minimum(right_properties.get(left_only[0])) == 0
        && array_minimum(right_properties.get(right_only[0])) >= 1
        && array_minimum(left_properties.get(right_only[0])) == 0
}

fn required_property_names(schema: &JsonObject) -> Option<Vec<&str>> {
    schema
        .get("required")?
        .as_array()?
        .iter()
        .map(serde_json::Value::as_str)
        .collect()
}

fn array_minimum(schema: Option<&serde_json::Value>) -> u64 {
    schema
        .and_then(|schema| schema.get("minItems"))
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0)
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
    #[serde(deny_unknown_fields)]
    #[expect(dead_code, reason = "schema-only patterned map test model")]
    struct PatternedMapInput {
        /// A patterned map keyed by object identifiers.
        values: BTreeMap<ObjectId, DisplayName>,
    }

    #[derive(JsonSchema)]
    #[serde(deny_unknown_fields)]
    #[expect(dead_code, reason = "schema-only tuple test model")]
    struct TupleInput {
        /// A tuple encoded through positional array applicators.
        value: (ObjectId, DisplayName),
    }

    struct UnknownApplicatorInput;

    impl JsonSchema for UnknownApplicatorInput {
        fn schema_name() -> Cow<'static, str> {
            "UnknownApplicatorInput".into()
        }

        fn json_schema(_: &mut SchemaGenerator) -> Schema {
            json_schema!({
                "type": "object",
                "additionalProperties": false,
                "properties": {},
                "if": { "type": "object" },
            })
        }
    }

    fn contains_keyword(value: &serde_json::Value, keyword: &str) -> bool {
        match value {
            serde_json::Value::Object(object) => {
                object.contains_key(keyword)
                    || object
                        .values()
                        .any(|value| contains_keyword(value, keyword))
            }
            serde_json::Value::Array(values) => {
                values.iter().any(|value| contains_keyword(value, keyword))
            }
            _ => false,
        }
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
    fn patterned_maps_tuple_arrays_and_unknown_applicators_are_rejected() {
        let map_schema = serde_json::Value::Object(
            rmcp::handler::server::tool::schema_for_input::<PatternedMapInput>()
                .unwrap()
                .as_ref()
                .clone(),
        );
        let tuple_schema = serde_json::Value::Object(
            rmcp::handler::server::tool::schema_for_input::<TupleInput>()
                .unwrap()
                .as_ref()
                .clone(),
        );
        assert!(
            contains_keyword(&map_schema, "propertyNames")
                || contains_keyword(&map_schema, "patternProperties")
        );
        assert!(contains_keyword(&tuple_schema, "prefixItems"));
        assert_eq!(
            input_schema::<PatternedMapInput>(),
            Err(SchemaContractError)
        );
        assert_eq!(input_schema::<TupleInput>(), Err(SchemaContractError));
        assert_eq!(
            input_schema::<UnknownApplicatorInput>(),
            Err(SchemaContractError)
        );
    }

    #[test]
    fn bounded_tagged_union_is_accepted() {
        let schema = input_schema::<TaggedUnionInput>().unwrap();
        assert!(schema["$defs"]["TaggedSelector"]["oneOf"].is_array());
    }

    #[test]
    fn nonempty_array_object_union_is_narrowly_accepted() {
        let schema = json!({
            "anyOf": [
                {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "operator": {
                            "type": "string",
                            "maxLength": 3,
                            "description": "Boolean operator."
                        },
                        "conditions": {
                            "type": "array",
                            "items": { "type": "string", "maxLength": 8 },
                            "minItems": 1,
                            "maxItems": 50,
                            "description": "Leaf conditions."
                        },
                        "filters": {
                            "type": "array",
                            "items": { "type": "string", "maxLength": 8 },
                            "minItems": 0,
                            "maxItems": 50,
                            "description": "Nested filters."
                        }
                    },
                    "required": ["operator", "conditions"]
                },
                {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "operator": {
                            "type": "string",
                            "maxLength": 3,
                            "description": "Boolean operator."
                        },
                        "conditions": {
                            "type": "array",
                            "items": { "type": "string", "maxLength": 8 },
                            "minItems": 0,
                            "maxItems": 50,
                            "description": "Leaf conditions."
                        },
                        "filters": {
                            "type": "array",
                            "items": { "type": "string", "maxLength": 8 },
                            "minItems": 1,
                            "maxItems": 50,
                            "description": "Nested filters."
                        }
                    },
                    "required": ["operator", "filters"]
                }
            ]
        });

        assert!(strict_wire_schema(&schema, &schema));

        let mut no_nonempty_branch = schema.clone();
        no_nonempty_branch["anyOf"][1]["properties"]["filters"]["minItems"] = json!(0);
        assert!(!strict_wire_schema(
            &no_nonempty_branch,
            &no_nonempty_branch
        ));

        let mut mismatched_properties = schema.clone();
        mismatched_properties["anyOf"][1]["properties"]
            .as_object_mut()
            .unwrap()
            .remove("operator");
        assert!(!strict_wire_schema(
            &mismatched_properties,
            &mismatched_properties
        ));
    }

    #[test]
    fn object_property_count_constraints_fail_closed() {
        let valid = json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "value": {
                    "type": "string",
                    "maxLength": 8,
                    "description": "Bounded value."
                }
            },
            "minProperties": 1,
            "maxProperties": 1,
            "required": ["value"]
        });
        assert!(strict_wire_schema(&valid, &valid));
        for malformed in [json!("1"), json!(1.5), json!(-1), json!(null)] {
            let mut schema = valid.clone();
            schema["minProperties"] = malformed.clone();
            assert!(!strict_wire_schema(&schema, &schema));
            let mut schema = valid.clone();
            schema["maxProperties"] = malformed;
            assert!(!strict_wire_schema(&schema, &schema));
        }
    }
}
