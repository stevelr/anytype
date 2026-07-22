// any-mcp - bounded, workflow-oriented MCP server for Anytype
//
// SPDX-FileCopyrightText: 2026 Steve Schoettler
// SPDX-License-Identifier: Apache-2.0

//! Shared, bounded MCP filter inputs and their exact Anytype API conversion.

use std::borrow::Cow;

use anytype::filters::{
    Condition, Filter as AnytypeFilter, FilterExpression as AnytypeFilterExpression,
};
use rmcp::schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Deserializer, Serialize, de};
use serde_json::{Number, Value, json};

use crate::{
    domain::{BoundedText, DomainValueError, EntityId, MAX_REFERENCE_CHARS, ObjectId, TypeKey},
    error::ToolError,
    handler_support::HandlerError,
    validation::{FilterBudget, FilterList, FilterValueList, Omittable},
};

/// Maximum characters accepted in a scalar textual filter value.
pub const MAX_FILTER_TEXT_CHARS: usize = 4_096;
/// Maximum characters accepted in a date filter value.
pub const MAX_FILTER_DATE_CHARS: usize = 64;
/// Maximum absolute numeric filter value.
pub const MAX_FILTER_NUMBER_ABS: f64 = 1_000_000_000_000_000.0;
/// Maximum direct leaves in a flat list filter after accounting for its root.
pub const MAX_LIST_FILTER_CONDITIONS: usize = crate::validation::MAX_FILTERS - 1;

/// Bounded scalar text accepted by textual filter formats.
pub type FilterText = BoundedText<MAX_FILTER_TEXT_CHARS>;
/// Bounded date text accepted by the Anytype HTTP API.
pub type FilterDate = BoundedText<MAX_FILTER_DATE_CHARS>;

/// Checked representations of an optional shared filter for flat-AND list
/// endpoints.
pub(crate) struct PreparedFlatFilters {
    /// Exact filter leaves forwarded to the upstream request builder.
    pub(crate) upstream: Vec<AnytypeFilter>,
    /// Original decoded shape used to enforce the raw query-size ceiling.
    pub(crate) raw_binding: Option<Value>,
    /// Canonical semantic shape used only for cursor identity.
    pub(crate) semantic_binding: Option<Value>,
}

/// Validates and prepares an optional filter for an upstream endpoint that
/// supports one flat conjunction and no nested expression body.
pub(crate) fn prepare_flat_filters(
    filters: &Omittable<McpListFilter>,
) -> Result<PreparedFlatFilters, HandlerError> {
    let upstream = filters
        .as_ref()
        .map(McpListFilter::to_anytype)
        .transpose()?
        .unwrap_or_default();
    let raw_binding = filters
        .as_ref()
        .map(serde_json::to_value)
        .transpose()
        .map_err(|_| HandlerError::new(ToolError::upstream()))?;
    let semantic_binding = filters
        .as_ref()
        .map(McpListFilter::cursor_binding_value)
        .transpose()?;
    Ok(PreparedFlatFilters {
        upstream,
        raw_binding,
        semantic_binding,
    })
}

/// A nonempty select/tag reference that cannot collide under Anytype's
/// comma-delimited select serialization.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct SelectReference(BoundedText<MAX_REFERENCE_CHARS>);

impl SelectReference {
    /// Validates a select reference without trimming or otherwise normalizing
    /// the caller's value.
    pub fn new(value: impl Into<String>) -> Result<Self, SelectReferenceError> {
        let value = value.into();
        if value.is_empty() {
            return Err(SelectReferenceError::Domain(DomainValueError::Empty));
        }
        if value.contains(',') {
            return Err(SelectReferenceError::Comma);
        }
        BoundedText::new(value)
            .map(Self)
            .map_err(SelectReferenceError::Domain)
    }

    /// Borrows the exact validated reference.
    #[must_use]
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl AsRef<str> for SelectReference {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl<'de> Deserialize<'de> for SelectReference {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::new(String::deserialize(deserializer)?).map_err(de::Error::custom)
    }
}

impl JsonSchema for SelectReference {
    fn schema_name() -> Cow<'static, str> {
        "SelectReference".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "minLength": 1,
            "maxLength": MAX_REFERENCE_CHARS,
            "pattern": "^[^,]+$",
        })
    }
}

/// Failure to construct an unambiguous select reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectReferenceError {
    /// The shared reference length or nonempty bound failed.
    Domain(DomainValueError),
    /// A comma would collide with Anytype's list delimiter.
    Comma,
}

impl std::fmt::Display for SelectReferenceError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Domain(error) => error.fmt(formatter),
            Self::Comma => formatter.write_str("select reference must not contain a comma"),
        }
    }
}

impl std::error::Error for SelectReferenceError {}

/// Logical operator for one bounded filter group.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FilterOperator {
    /// Require every condition and nested group.
    And,
    /// Require at least one condition or nested group.
    Or,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ListFilterOperator {
    And,
}

/// Operators supported for textual property formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TextCondition {
    /// Equal.
    Eq,
    /// Not equal.
    Ne,
    /// Contains the supplied text.
    Contains,
    /// Does not contain the supplied text.
    NotContains,
}

/// Operators supported for numeric properties.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum NumberCondition {
    /// Equal.
    Eq,
    /// Not equal.
    Ne,
    /// Less than.
    Lt,
    /// Less than or equal.
    Lte,
    /// Greater than.
    Gt,
    /// Greater than or equal.
    Gte,
}

/// Operators supported for dates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DateCondition {
    /// Equal.
    Eq,
    /// Less than.
    Lt,
    /// Less than or equal.
    Lte,
    /// Greater than.
    Gt,
    /// Greater than or equal.
    Gte,
    /// Match a date value accepted by Anytype's `in` condition.
    In,
}

/// Operators supported for a single-select property.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SelectCondition {
    /// Match one of the supplied values.
    In,
    /// Match none of the supplied values.
    NotIn,
}

/// Operators supported for multi-value properties.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ArrayCondition {
    /// Match at least one supplied value.
    In,
    /// Match none of the supplied values.
    NotIn,
    /// Require all supplied values.
    AllIn,
}

/// Operators supported for checkbox properties.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CheckboxCondition {
    /// Equal.
    Eq,
    /// Not equal.
    Ne,
}

/// A finite numeric filter value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct FilterNumber(Number);

impl FilterNumber {
    /// Validates a finite practical numeric value.
    pub fn new(value: Number) -> Result<Self, FilterNumberError> {
        let Some(as_float) = value.as_f64() else {
            return Err(FilterNumberError);
        };
        if !as_float.is_finite()
            || as_float.abs() > MAX_FILTER_NUMBER_ABS
            || value.to_string().len() > 128
        {
            Err(FilterNumberError)
        } else {
            Ok(Self(value))
        }
    }

    fn json_number(&self) -> Number {
        self.0.clone()
    }
}

impl<'de> Deserialize<'de> for FilterNumber {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::new(Number::deserialize(deserializer)?).map_err(de::Error::custom)
    }
}

impl JsonSchema for FilterNumber {
    fn schema_name() -> Cow<'static, str> {
        "FilterNumber".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "number",
            "minimum": -MAX_FILTER_NUMBER_ABS,
            "maximum": MAX_FILTER_NUMBER_ABS,
        })
    }
}

/// A numeric filter outside the supported finite range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FilterNumberError;

impl std::fmt::Display for FilterNumberError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("numeric filter value is outside its supported finite range")
    }
}

impl std::error::Error for FilterNumberError {}

/// One constrained Anytype property filter shared by MCP workflows.
///
/// The tagged form prevents ambiguous free-form JSON and maps one-to-one to
/// the supported [`anytype::filters::Filter`] variants. Checkbox and numeric
/// filters are forwarded exactly as supplied. Live conformance verifies the
/// configured backend's behavior; this server never rewrites them or filters
/// returned pages locally.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "format", rename_all = "snake_case", deny_unknown_fields)]
pub enum McpFilter {
    /// Plain text property.
    Text {
        /// Property key.
        property_key: TypeKey,
        /// Text operator.
        condition: TextCondition,
        /// Compared text.
        value: FilterText,
    },
    /// Numeric property.
    Number {
        /// Property key.
        property_key: TypeKey,
        /// Numeric operator.
        condition: NumberCondition,
        /// Compared number.
        value: FilterNumber,
    },
    /// Single-select property.
    Select {
        /// Property key.
        property_key: TypeKey,
        /// Select operator.
        condition: SelectCondition,
        /// Compared tag ids or keys.
        values: FilterValueList<SelectReference>,
    },
    /// Multi-select property.
    MultiSelect {
        /// Property key.
        property_key: TypeKey,
        /// Array operator.
        condition: ArrayCondition,
        /// Compared tag ids or keys.
        values: FilterValueList<SelectReference>,
    },
    /// RFC 3339-like date property value accepted by Anytype.
    Date {
        /// Property key.
        property_key: TypeKey,
        /// Date operator.
        condition: DateCondition,
        /// Compared date text.
        value: FilterDate,
    },
    /// Checkbox property.
    Checkbox {
        /// Property key.
        property_key: TypeKey,
        /// Boolean operator.
        condition: CheckboxCondition,
        /// Compared checkbox state.
        value: bool,
    },
    /// File-reference property.
    Files {
        /// Property key.
        property_key: TypeKey,
        /// Array operator.
        condition: ArrayCondition,
        /// Compared file ids.
        values: FilterValueList<EntityId>,
    },
    /// URL property.
    Url {
        /// Property key.
        property_key: TypeKey,
        /// Text operator.
        condition: TextCondition,
        /// Compared URL text.
        value: FilterText,
    },
    /// Email property.
    Email {
        /// Property key.
        property_key: TypeKey,
        /// Text operator.
        condition: TextCondition,
        /// Compared email text.
        value: FilterText,
    },
    /// Phone property.
    Phone {
        /// Property key.
        property_key: TypeKey,
        /// Text operator.
        condition: TextCondition,
        /// Compared phone text.
        value: FilterText,
    },
    /// Object-reference property.
    Objects {
        /// Property key.
        property_key: TypeKey,
        /// Array operator.
        condition: ArrayCondition,
        /// Compared object ids.
        values: FilterValueList<ObjectId>,
    },
    /// Require a property to be empty.
    Empty {
        /// Property key.
        property_key: TypeKey,
    },
    /// Require a property to be present and nonempty.
    NotEmpty {
        /// Property key.
        property_key: TypeKey,
    },
}

impl McpFilter {
    fn value_count(&self) -> usize {
        match self {
            Self::Select { values, .. } | Self::MultiSelect { values, .. } => {
                values.as_slice().len()
            }
            Self::Files { values, .. } => values.as_slice().len(),
            Self::Objects { values, .. } => values.as_slice().len(),
            Self::Empty { .. } | Self::NotEmpty { .. } => 0,
            _ => 1,
        }
    }

    /// Converts this checked wire DTO into the corresponding Anytype API
    /// filter without client-side emulation or semantic rewriting.
    pub(crate) fn to_anytype(&self) -> Result<AnytypeFilter, HandlerError> {
        let filter = match self {
            Self::Text {
                property_key,
                condition,
                value,
            } => AnytypeFilter::Text {
                condition: text_condition(*condition),
                property_key: property_key.as_str().to_owned(),
                text: value.as_str().to_owned(),
            },
            Self::Number {
                property_key,
                condition,
                value,
            } => AnytypeFilter::Number {
                condition: number_condition(*condition),
                property_key: property_key.as_str().to_owned(),
                number: value.json_number(),
            },
            Self::Select {
                property_key,
                condition,
                values,
            } => AnytypeFilter::Select {
                condition: select_condition(*condition),
                property_key: property_key.as_str().to_owned(),
                select: nonempty_values(values)?,
            },
            Self::MultiSelect {
                property_key,
                condition,
                values,
            } => AnytypeFilter::MultiSelect {
                condition: array_condition(*condition),
                property_key: property_key.as_str().to_owned(),
                multi_select: nonempty_values(values)?,
            },
            Self::Date {
                property_key,
                condition,
                value,
            } => AnytypeFilter::Date {
                condition: date_condition(*condition),
                property_key: property_key.as_str().to_owned(),
                date: value.as_str().to_owned(),
            },
            Self::Checkbox {
                property_key,
                condition,
                value,
            } => AnytypeFilter::Checkbox {
                condition: checkbox_condition(*condition),
                property_key: property_key.as_str().to_owned(),
                checkbox: *value,
            },
            Self::Files {
                property_key,
                condition,
                values,
            } => AnytypeFilter::Files {
                condition: array_condition(*condition),
                property_key: property_key.as_str().to_owned(),
                files: nonempty_values(values)?,
            },
            Self::Url {
                property_key,
                condition,
                value,
            } => AnytypeFilter::Url {
                condition: text_condition(*condition),
                property_key: property_key.as_str().to_owned(),
                url: value.as_str().to_owned(),
            },
            Self::Email {
                property_key,
                condition,
                value,
            } => AnytypeFilter::Email {
                condition: text_condition(*condition),
                property_key: property_key.as_str().to_owned(),
                email: value.as_str().to_owned(),
            },
            Self::Phone {
                property_key,
                condition,
                value,
            } => AnytypeFilter::Phone {
                condition: text_condition(*condition),
                property_key: property_key.as_str().to_owned(),
                phone: value.as_str().to_owned(),
            },
            Self::Objects {
                property_key,
                condition,
                values,
            } => AnytypeFilter::Objects {
                condition: array_condition(*condition),
                property_key: property_key.as_str().to_owned(),
                objects: nonempty_values(values)?,
            },
            Self::Empty { property_key } => AnytypeFilter::is_empty(property_key.as_str()),
            Self::NotEmpty { property_key } => AnytypeFilter::not_empty(property_key.as_str()),
        };
        Ok(filter)
    }

    fn cursor_binding_value(&self) -> Result<Value, HandlerError> {
        let mut value =
            serde_json::to_value(self).map_err(|_| HandlerError::new(ToolError::upstream()))?;
        if matches!(
            self,
            Self::Select { .. }
                | Self::MultiSelect { .. }
                | Self::Files { .. }
                | Self::Objects { .. }
        ) {
            let values = value
                .get_mut("values")
                .and_then(Value::as_array_mut)
                .ok_or_else(|| HandlerError::new(ToolError::upstream()))?;
            if values.iter().any(|value| !value.is_string()) {
                return Err(HandlerError::new(ToolError::upstream()));
            }
            values.sort_by(|left, right| left.as_str().cmp(&right.as_str()));
            values.dedup();
        }
        Ok(value)
    }
}

/// Shared filter form for list endpoints whose upstream request model accepts
/// only a flat conjunction.
///
/// Every approved [`McpFilter`] leaf remains available. The wire operator must
/// be `and`, `conditions` must be nonempty, and nested `filters` are rejected
/// by the closed schema rather than flattened or emulated after pagination.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpListFilter {
    /// Required flat conjunction operator.
    #[serde(rename = "operator")]
    _operator: ListFilterOperator,
    /// Direct shared filter leaves.
    conditions: FilterList<McpFilter>,
}

impl JsonSchema for McpListFilter {
    fn schema_name() -> Cow<'static, str> {
        "McpListFilter".into()
    }

    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "object",
            "description": "One flat conjunction for an upstream list endpoint. All shared filter leaf formats are available; nested groups and or are unsupported.",
            "additionalProperties": false,
            "properties": {
                "operator": {
                    "type": "string",
                    "const": "and",
                    "description": "List endpoints combine every condition with and.",
                },
                "conditions": {
                    "type": "array",
                    "description": "Direct shared filter leaves forwarded to the upstream list request.",
                    "items": generator.subschema_for::<McpFilter>(),
                    "minItems": 1,
                    "maxItems": MAX_LIST_FILTER_CONDITIONS,
                },
            },
            "required": ["operator", "conditions"],
        })
    }
}

impl McpListFilter {
    fn to_anytype(&self) -> Result<Vec<AnytypeFilter>, HandlerError> {
        let mut budget = FilterBudget::default();
        budget.record(1, 0)?;
        if self.conditions.as_slice().is_empty() {
            return Err(HandlerError::new(ToolError::validation()));
        }
        self.conditions
            .as_slice()
            .iter()
            .map(|filter| {
                budget.record(1, filter.value_count())?;
                filter.to_anytype()
            })
            .collect()
    }

    fn cursor_binding_value(&self) -> Result<Value, HandlerError> {
        let conditions = self
            .conditions
            .as_slice()
            .iter()
            .map(McpFilter::cursor_binding_value)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(json!({
            "operator": "and",
            "conditions": sort_and_deduplicate(conditions)?,
        }))
    }
}

/// One nested, bounded filter expression shared by MCP workflows.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpFilterExpression {
    /// Operator combining direct conditions and child groups.
    operator: FilterOperator,
    /// Direct conditions in this group.
    #[serde(default)]
    conditions: FilterList<McpFilter>,
    /// Nested groups in this group.
    #[serde(default)]
    filters: FilterList<McpFilterExpression>,
}

impl JsonSchema for McpFilterExpression {
    fn schema_name() -> Cow<'static, str> {
        "McpFilterExpression".into()
    }

    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "description": "One nested, bounded filter expression shared by MCP workflows. At least one of conditions or filters must be present and nonempty.",
            "anyOf": [
                expression_schema_branch(generator, true),
                expression_schema_branch(generator, false),
            ],
        })
    }
}

fn expression_schema_branch(generator: &mut SchemaGenerator, require_conditions: bool) -> Schema {
    let required_member = if require_conditions {
        "conditions"
    } else {
        "filters"
    };
    let condition_minimum = usize::from(require_conditions);
    let filter_minimum = usize::from(!require_conditions);
    json_schema!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "operator": described_schema(
                generator.subschema_for::<FilterOperator>(),
                "Operator combining direct conditions and child groups.",
            ),
            "conditions": {
                "type": "array",
                "description": "Direct conditions in this group.",
                "items": generator.subschema_for::<McpFilter>(),
                "minItems": condition_minimum,
                "maxItems": crate::validation::MAX_FILTERS,
                "default": [],
            },
            "filters": {
                "type": "array",
                "description": "Nested groups in this group.",
                "items": generator.subschema_for::<McpFilterExpression>(),
                "minItems": filter_minimum,
                "maxItems": crate::validation::MAX_FILTERS,
                "default": [],
            },
        },
        "required": ["operator", required_member],
    })
}

fn described_schema(mut schema: Schema, description: &'static str) -> Schema {
    schema.insert(
        "description".to_owned(),
        Value::String(description.to_owned()),
    );
    schema
}

impl McpFilterExpression {
    /// Converts the complete expression after enforcing aggregate count,
    /// value, and nesting budgets.
    pub(crate) fn to_anytype(&self) -> Result<AnytypeFilterExpression, HandlerError> {
        let mut budget = FilterBudget::default();
        self.to_anytype_at(1, &mut budget)
    }

    /// Produces the semantic form used only for cursor query binding.
    ///
    /// Logical group members and set-valued operands are commutative. Sorting
    /// and deduplicating them prevents an equivalent continuation request from
    /// being rejected solely because the caller changed their presentation.
    /// The original DTO remains untouched and is forwarded upstream in the
    /// caller's order.
    pub(crate) fn cursor_binding_value(&self) -> Result<Value, HandlerError> {
        let conditions = self
            .conditions
            .as_slice()
            .iter()
            .map(McpFilter::cursor_binding_value)
            .collect::<Result<Vec<_>, _>>()?;
        let filters = self
            .filters
            .as_slice()
            .iter()
            .map(Self::cursor_binding_value)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(json!({
            "operator": self.operator,
            "conditions": sort_and_deduplicate(conditions)?,
            "filters": sort_and_deduplicate(filters)?,
        }))
    }

    fn to_anytype_at(
        &self,
        depth: usize,
        budget: &mut FilterBudget,
    ) -> Result<AnytypeFilterExpression, HandlerError> {
        budget.record(depth, 0)?;
        if self.conditions.as_slice().is_empty() && self.filters.as_slice().is_empty() {
            return Err(HandlerError::new(ToolError::validation()));
        }
        let conditions = self
            .conditions
            .as_slice()
            .iter()
            .map(|filter| {
                budget.record(depth, filter.value_count())?;
                filter.to_anytype()
            })
            .collect::<Result<Vec<_>, HandlerError>>()?;
        let filters = self
            .filters
            .as_slice()
            .iter()
            .map(|filter| filter.to_anytype_at(depth.saturating_add(1), budget))
            .collect::<Result<Vec<_>, HandlerError>>()?;
        Ok(match self.operator {
            FilterOperator::And => AnytypeFilterExpression::and(conditions, filters),
            FilterOperator::Or => AnytypeFilterExpression::or(conditions, filters),
        })
    }
}

fn nonempty_values<T: AsRef<str>>(
    values: &FilterValueList<T>,
) -> Result<Vec<String>, HandlerError> {
    if values.as_slice().is_empty() {
        return Err(HandlerError::new(ToolError::validation()));
    }
    Ok(values
        .as_slice()
        .iter()
        .map(|value| value.as_ref().to_owned())
        .collect())
}

const fn text_condition(condition: TextCondition) -> Condition {
    match condition {
        TextCondition::Eq => Condition::Equal,
        TextCondition::Ne => Condition::NotEqual,
        TextCondition::Contains => Condition::Contains,
        TextCondition::NotContains => Condition::NotContains,
    }
}

const fn number_condition(condition: NumberCondition) -> Condition {
    match condition {
        NumberCondition::Eq => Condition::Equal,
        NumberCondition::Ne => Condition::NotEqual,
        NumberCondition::Lt => Condition::Less,
        NumberCondition::Lte => Condition::LessOrEqual,
        NumberCondition::Gt => Condition::Greater,
        NumberCondition::Gte => Condition::GreaterOrEqual,
    }
}

const fn date_condition(condition: DateCondition) -> Condition {
    match condition {
        DateCondition::Eq => Condition::Equal,
        DateCondition::Lt => Condition::Less,
        DateCondition::Lte => Condition::LessOrEqual,
        DateCondition::Gt => Condition::Greater,
        DateCondition::Gte => Condition::GreaterOrEqual,
        DateCondition::In => Condition::In,
    }
}

fn sort_and_deduplicate(values: Vec<Value>) -> Result<Vec<Value>, HandlerError> {
    let mut keyed = values
        .into_iter()
        .map(|value| canonical_value_key(&value).map(|key| (key, value)))
        .collect::<Result<Vec<_>, _>>()?;
    keyed.sort_by(|left, right| left.0.cmp(&right.0));
    keyed.dedup_by(|left, right| left.0 == right.0);
    Ok(keyed.into_iter().map(|(_, value)| value).collect())
}

fn canonical_value_key(value: &Value) -> Result<Vec<u8>, HandlerError> {
    let mut value = value.clone();
    sort_object_fields(&mut value);
    serde_json::to_vec(&value).map_err(|_| HandlerError::new(ToolError::upstream()))
}

fn sort_object_fields(value: &mut Value) {
    match value {
        Value::Object(fields) => {
            let old = std::mem::take(fields);
            let mut entries = old.into_iter().collect::<Vec<_>>();
            entries.sort_by(|left, right| left.0.cmp(&right.0));
            for (key, mut value) in entries {
                sort_object_fields(&mut value);
                fields.insert(key, value);
            }
        }
        Value::Array(values) => values.iter_mut().for_each(sort_object_fields),
        _ => {}
    }
}

const fn select_condition(condition: SelectCondition) -> Condition {
    match condition {
        SelectCondition::In => Condition::In,
        SelectCondition::NotIn => Condition::NotIn,
    }
}

const fn array_condition(condition: ArrayCondition) -> Condition {
    match condition {
        ArrayCondition::In => Condition::In,
        ArrayCondition::NotIn => Condition::NotIn,
        ArrayCondition::AllIn => Condition::AllIn,
    }
}

const fn checkbox_condition(condition: CheckboxCondition) -> Condition {
    match condition {
        CheckboxCondition::Eq => Condition::Equal,
        CheckboxCondition::Ne => Condition::NotEqual,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};

    use rmcp::schemars::schema_for;
    use serde_json::{Number, Value, json};

    use super::*;
    use crate::{
        cursor::{CursorStore, QueryFingerprint},
        pagination::PageOffset,
        schema::input_schema,
        validation::{MAX_FILTER_DEPTH, MAX_FILTER_VALUES, ValidationCode},
    };

    #[derive(Clone)]
    struct AcceptedCase {
        format: &'static str,
        condition: Option<&'static str>,
        input: Value,
        expected: Value,
    }

    fn scalar_cases(
        format: &'static str,
        property_key: &'static str,
        api_field: &'static str,
        value: Value,
        conditions: &[(&'static str, &'static str)],
    ) -> Vec<AcceptedCase> {
        conditions
            .iter()
            .map(|&(condition, api_condition)| {
                let mut input = json!({
                    "format": format,
                    "property_key": property_key,
                    "condition": condition,
                    "value": value,
                });
                let mut expected = json!({
                    "condition": api_condition,
                    "property_key": property_key,
                });
                expected
                    .as_object_mut()
                    .expect("expected filter object")
                    .insert(api_field.to_owned(), value.clone());
                input["value"] = value.clone();
                AcceptedCase {
                    format,
                    condition: Some(condition),
                    input,
                    expected,
                }
            })
            .collect()
    }

    fn set_cases(
        format: &'static str,
        property_key: &'static str,
        api_field: &'static str,
        values: Value,
        conditions: &[(&'static str, &'static str)],
    ) -> Vec<AcceptedCase> {
        conditions
            .iter()
            .map(|&(condition, api_condition)| {
                let input = json!({
                    "format": format,
                    "property_key": property_key,
                    "condition": condition,
                    "values": values,
                });
                let api_value = if format == "select" {
                    Value::String("alpha,beta".to_owned())
                } else {
                    values.clone()
                };
                let mut expected = json!({
                    "condition": api_condition,
                    "property_key": property_key,
                });
                expected
                    .as_object_mut()
                    .expect("expected filter object")
                    .insert(api_field.to_owned(), api_value);
                AcceptedCase {
                    format,
                    condition: Some(condition),
                    input,
                    expected,
                }
            })
            .collect()
    }

    fn accepted_cases() -> Vec<AcceptedCase> {
        let text_conditions = [
            ("eq", "eq"),
            ("ne", "ne"),
            ("contains", "contains"),
            ("not_contains", "ncontains"),
        ];
        let mut cases = scalar_cases("text", "name", "text", json!("road"), &text_conditions);
        cases.extend(scalar_cases(
            "number",
            "priority",
            "number",
            json!(2.5),
            &[
                ("eq", "eq"),
                ("ne", "ne"),
                ("lt", "lt"),
                ("lte", "lte"),
                ("gt", "gt"),
                ("gte", "gte"),
            ],
        ));
        cases.extend(set_cases(
            "select",
            "tag",
            "select",
            json!(["alpha", "beta"]),
            &[("in", "in"), ("not_in", "nin")],
        ));
        for (format, property_key, api_field, values) in [
            (
                "multi_select",
                "tags",
                "multi_select",
                json!(["alpha", "beta"]),
            ),
            ("files", "attachments", "files", json!(["file-1", "file-2"])),
            (
                "objects",
                "links",
                "objects",
                json!(["object-1", "object-2"]),
            ),
        ] {
            cases.extend(set_cases(
                format,
                property_key,
                api_field,
                values,
                &[("in", "in"), ("not_in", "nin"), ("all_in", "all_in")],
            ));
        }
        cases.extend(scalar_cases(
            "date",
            "due",
            "date",
            json!("2026-07-21T00:00:00Z"),
            &[
                ("eq", "eq"),
                ("lt", "lt"),
                ("lte", "lte"),
                ("gt", "gt"),
                ("gte", "gte"),
                ("in", "in"),
            ],
        ));
        cases.extend(scalar_cases(
            "checkbox",
            "done",
            "checkbox",
            json!(false),
            &[("eq", "eq"), ("ne", "ne")],
        ));
        for (format, api_field, value) in [
            ("url", "url", "https://example.invalid"),
            ("email", "email", "agent@example.invalid"),
            ("phone", "phone", "+1-555-0100"),
        ] {
            cases.extend(scalar_cases(
                format,
                "contact",
                api_field,
                json!(value),
                &text_conditions,
            ));
        }
        cases.extend([
            AcceptedCase {
                format: "empty",
                condition: None,
                input: json!({"format":"empty","property_key":"status"}),
                expected: json!({"condition":"empty","property_key":"status"}),
            },
            AcceptedCase {
                format: "not_empty",
                condition: None,
                input: json!({"format":"not_empty","property_key":"status"}),
                expected: json!({"condition":"nempty","property_key":"status"}),
            },
        ]);
        cases
    }

    fn decode_filter(input: Value) -> Result<McpFilter, serde_json::Error> {
        serde_json::from_value(input)
    }

    fn assert_conversion(input: Value, expected: Value) {
        let filter = decode_filter(input).expect("valid MCP filter");
        let actual = serde_json::to_value(filter.to_anytype().expect("Anytype conversion"))
            .expect("serialize Anytype filter");
        assert_eq!(actual, expected);
    }

    #[test]
    fn every_supported_format_and_condition_converts_one_to_one() {
        let cases = accepted_cases();
        assert_eq!(cases.len(), 43, "accepted conversion inventory changed");
        let inventory = cases
            .iter()
            .map(|case| (case.format, case.condition))
            .collect::<HashSet<_>>();
        assert_eq!(
            inventory.len(),
            43,
            "accepted inventory contains duplicates"
        );
        for case in cases {
            assert_conversion(case.input, case.expected);
        }
    }

    #[test]
    fn flat_list_conversion_preserves_leaves_and_rejects_unrepresentable_groups() {
        let flat = serde_json::from_value::<McpListFilter>(json!({
            "operator": "and",
            "conditions": [
                {"format":"text","property_key":"name","condition":"contains","value":"road"},
                {"format":"checkbox","property_key":"done","condition":"eq","value":true}
            ]
        }))
        .unwrap();
        assert_eq!(
            serde_json::to_value(flat.to_anytype().unwrap()).unwrap(),
            json!([
                {"condition":"contains","property_key":"name","text":"road"},
                {"condition":"eq","property_key":"done","checkbox":true}
            ])
        );

        for rejected in [
            json!({
                "operator": "or",
                "conditions": [{"format":"not_empty","property_key":"name"}]
            }),
            json!({
                "operator": "and",
                "filters": [{
                    "operator": "and",
                    "conditions": [{"format":"not_empty","property_key":"name"}]
                }]
            }),
        ] {
            assert!(serde_json::from_value::<McpListFilter>(rejected).is_err());
        }
        assert!(
            serde_json::from_value::<McpListFilter>(json!({"operator":"and","conditions":[]}))
                .unwrap()
                .to_anytype()
                .is_err()
        );

        let reordered = serde_json::from_value::<McpListFilter>(json!({
            "operator": "and",
            "conditions": [
                {"format":"checkbox","property_key":"done","condition":"eq","value":true},
                {"format":"text","property_key":"name","condition":"contains","value":"road"},
                {"format":"text","property_key":"name","condition":"contains","value":"road"}
            ]
        }))
        .unwrap();
        assert_eq!(
            flat.cursor_binding_value().unwrap(),
            reordered.cursor_binding_value().unwrap(),
            "flat conjunction permutations and duplicates share cursor identity"
        );
        assert_eq!(
            reordered.to_anytype().unwrap().len(),
            3,
            "cursor-only canonicalization must not rewrite the upstream request"
        );
    }

    fn canonical_inputs_by_format() -> HashMap<&'static str, Value> {
        let mut inputs = HashMap::new();
        for case in accepted_cases() {
            inputs.entry(case.format).or_insert(case.input);
        }
        inputs
    }

    fn payload_field(format: &str) -> Option<&'static str> {
        match format {
            "empty" | "not_empty" => None,
            "select" | "multi_select" | "files" | "objects" => Some("values"),
            _ => Some("value"),
        }
    }

    #[test]
    fn every_excluded_format_condition_pair_is_rejected() {
        const ALL_CONDITIONS: [&str; 17] = [
            "eq",
            "ne",
            "lt",
            "lte",
            "gt",
            "gte",
            "contains",
            "not_contains",
            "in",
            "not_in",
            "all_in",
            "none",
            "exists",
            "all",
            "not_all_in",
            "exact_in",
            "not_exact_in",
        ];
        let accepted = accepted_cases()
            .into_iter()
            .filter_map(|case| case.condition.map(|condition| (case.format, condition)))
            .collect::<HashSet<_>>();
        let inputs = canonical_inputs_by_format();
        let mut rejected = 0;

        for (format, base) in inputs {
            for condition in ALL_CONDITIONS {
                let mut candidate = base.clone();
                candidate
                    .as_object_mut()
                    .expect("filter object")
                    .insert("condition".to_owned(), json!(condition));
                if accepted.contains(&(format, condition)) {
                    assert!(
                        decode_filter(candidate).is_ok(),
                        "accepted {format}/{condition} was rejected"
                    );
                } else {
                    rejected += 1;
                    assert!(
                        decode_filter(candidate).is_err(),
                        "excluded {format}/{condition} was accepted"
                    );
                }
            }
        }
        assert_eq!(rejected, 180, "excluded conversion inventory changed");
    }

    #[test]
    fn unknown_tags_fields_and_malformed_payloads_are_rejected_for_every_format() {
        let inputs = canonical_inputs_by_format();
        assert_eq!(inputs.len(), 13, "format inventory changed");

        for (format, base) in inputs {
            let mut unknown_field = base.clone();
            unknown_field
                .as_object_mut()
                .expect("filter object")
                .insert("unexpected".to_owned(), json!(true));
            assert!(
                decode_filter(unknown_field).is_err(),
                "{format} accepted an unknown field"
            );

            let mut missing_property = base.clone();
            missing_property
                .as_object_mut()
                .expect("filter object")
                .remove("property_key");
            assert!(
                decode_filter(missing_property).is_err(),
                "{format} accepted a missing property_key"
            );
            let mut null_property = base.clone();
            null_property["property_key"] = Value::Null;
            assert!(
                decode_filter(null_property).is_err(),
                "{format} accepted a null property_key"
            );
            let mut wrong_property = base.clone();
            wrong_property["property_key"] = json!(7);
            assert!(
                decode_filter(wrong_property).is_err(),
                "{format} accepted a non-string property_key"
            );

            if let Some(field) = payload_field(format) {
                let mut missing_condition = base.clone();
                missing_condition
                    .as_object_mut()
                    .expect("filter object")
                    .remove("condition");
                assert!(
                    decode_filter(missing_condition).is_err(),
                    "{format} accepted a missing condition"
                );
                let mut null_condition = base.clone();
                null_condition["condition"] = Value::Null;
                assert!(
                    decode_filter(null_condition).is_err(),
                    "{format} accepted a null condition"
                );
                let mut unknown_condition = base.clone();
                unknown_condition["condition"] = json!("future_condition");
                assert!(
                    decode_filter(unknown_condition).is_err(),
                    "{format} accepted an unknown condition tag"
                );

                let mut missing_payload = base.clone();
                missing_payload
                    .as_object_mut()
                    .expect("filter object")
                    .remove(field);
                assert!(
                    decode_filter(missing_payload).is_err(),
                    "{format} accepted a missing {field}"
                );
                let mut null_payload = base.clone();
                null_payload[field] = Value::Null;
                assert!(
                    decode_filter(null_payload).is_err(),
                    "{format} accepted a null {field}"
                );
                let mut wrong_payload_field = base.clone();
                wrong_payload_field
                    .as_object_mut()
                    .expect("filter object")
                    .remove(field);
                let wrong_field = if field == "value" { "values" } else { "value" };
                wrong_payload_field
                    .as_object_mut()
                    .expect("filter object")
                    .insert(wrong_field.to_owned(), json!("wrong"));
                assert!(
                    decode_filter(wrong_payload_field).is_err(),
                    "{format} accepted the wrong payload field"
                );
                let mut wrong_payload_type = base.clone();
                wrong_payload_type[field] = if field == "values" {
                    json!("not-an-array")
                } else {
                    json!(["not-a-scalar"])
                };
                assert!(
                    decode_filter(wrong_payload_type).is_err(),
                    "{format} accepted the wrong payload type"
                );
            } else {
                for (field, value) in [
                    ("condition", json!("eq")),
                    ("value", json!("unexpected")),
                    ("values", json!(["unexpected"])),
                ] {
                    let mut candidate = base.clone();
                    candidate
                        .as_object_mut()
                        .expect("filter object")
                        .insert(field.to_owned(), value);
                    assert!(
                        decode_filter(candidate).is_err(),
                        "{format} accepted forbidden {field}"
                    );
                }
            }
        }

        assert!(
            decode_filter(json!({
                "format":"future_format",
                "property_key":"name",
                "condition":"eq",
                "value":"road"
            }))
            .is_err(),
            "unknown format tag was accepted"
        );
        assert!(
            decode_filter(json!({
                "property_key":"name",
                "condition":"eq",
                "value":"road"
            }))
            .is_err(),
            "missing format tag was accepted"
        );
        assert!(
            decode_filter(json!({
                "format":null,
                "property_key":"name",
                "condition":"eq",
                "value":"road"
            }))
            .is_err(),
            "null format tag was accepted"
        );
    }

    #[test]
    fn select_references_are_comma_free_bounded_and_preserved_exactly() {
        assert!(SelectReference::new("é".repeat(MAX_REFERENCE_CHARS)).is_ok());
        assert!(SelectReference::new("é".repeat(MAX_REFERENCE_CHARS + 1)).is_err());
        assert!(SelectReference::new("").is_err());
        assert!(SelectReference::new("alpha,beta").is_err());

        for format in ["select", "multi_select"] {
            assert!(
                serde_json::from_value::<McpFilter>(json!({
                    "format":format,
                    "property_key":"tag",
                    "condition":"in",
                    "values":["alpha,beta"]
                }))
                .is_err(),
                "{format} accepted an ambiguous comma-bearing reference"
            );
            assert!(
                serde_json::from_value::<McpFilter>(json!({
                    "format":format,
                    "property_key":"tag",
                    "condition":"in",
                    "values":["alpha","beta"]
                }))
                .is_ok()
            );
        }

        assert_conversion(
            json!({
                "format":"select",
                "property_key":"tag",
                "condition":"in",
                "values":[" alpha ","beta"]
            }),
            json!({
                "condition":"in",
                "property_key":"tag",
                "select":" alpha ,beta"
            }),
        );

        let schema = serde_json::to_value(schema_for!(SelectReference)).unwrap();
        assert_eq!(schema["minLength"], 1);
        assert_eq!(schema["maxLength"], MAX_REFERENCE_CHARS);
        assert_eq!(schema["pattern"], "^[^,]+$");
    }

    fn leaf_filter() -> McpFilterExpression {
        McpFilterExpression {
            operator: FilterOperator::And,
            conditions: FilterList::new(vec![McpFilter::Text {
                property_key: TypeKey::new("name").unwrap(),
                condition: TextCondition::Contains,
                value: FilterText::new("road").unwrap(),
            }])
            .unwrap(),
            filters: FilterList::new(Vec::new()).unwrap(),
        }
    }

    fn select_filter(prefix: &str, count: usize) -> McpFilter {
        McpFilter::Select {
            property_key: TypeKey::new("tag").unwrap(),
            condition: SelectCondition::In,
            values: FilterValueList::new(
                (0..count)
                    .map(|index| SelectReference::new(format!("{prefix}{index}")).unwrap())
                    .collect(),
            )
            .unwrap(),
        }
    }

    #[test]
    fn aggregate_depth_value_and_nonempty_array_bounds_are_preserved() {
        let mut nested = leaf_filter();
        for _ in 1..MAX_FILTER_DEPTH {
            nested = McpFilterExpression {
                operator: FilterOperator::And,
                conditions: FilterList::new(Vec::new()).unwrap(),
                filters: FilterList::new(vec![nested]).unwrap(),
            };
        }
        assert!(nested.to_anytype().is_ok());
        let too_deep = McpFilterExpression {
            operator: FilterOperator::And,
            conditions: FilterList::new(Vec::new()).unwrap(),
            filters: FilterList::new(vec![nested]).unwrap(),
        };
        assert!(too_deep.to_anytype().is_err());

        let leaf = McpFilter::Text {
            property_key: TypeKey::new("name").unwrap(),
            condition: TextCondition::Contains,
            value: FilterText::new("road").unwrap(),
        };
        let maximum_filter_count = McpFilterExpression {
            operator: FilterOperator::And,
            conditions: FilterList::new(vec![leaf.clone(); 49]).unwrap(),
            filters: FilterList::new(Vec::new()).unwrap(),
        };
        assert!(maximum_filter_count.to_anytype().is_ok());
        let excessive_filter_count = McpFilterExpression {
            operator: FilterOperator::And,
            conditions: FilterList::new(vec![leaf; 50]).unwrap(),
            filters: FilterList::new(Vec::new()).unwrap(),
        };
        assert!(excessive_filter_count.to_anytype().is_err());

        let max_values = select_filter("v", MAX_FILTER_VALUES);
        assert!(max_values.to_anytype().is_ok());
        let empty_values = McpFilter::Select {
            property_key: TypeKey::new("tag").unwrap(),
            condition: SelectCondition::In,
            values: FilterValueList::new(Vec::new()).unwrap(),
        };
        assert!(empty_values.to_anytype().is_err());
        let maximum_aggregate_values = McpFilterExpression {
            operator: FilterOperator::And,
            conditions: FilterList::new(vec![select_filter("a", 50), select_filter("b", 50)])
                .unwrap(),
            filters: FilterList::new(Vec::new()).unwrap(),
        };
        assert!(maximum_aggregate_values.to_anytype().is_ok());
        let excessive_aggregate_values = McpFilterExpression {
            operator: FilterOperator::And,
            conditions: FilterList::new(vec![select_filter("a", 51), select_filter("b", 50)])
                .unwrap(),
            filters: FilterList::new(Vec::new()).unwrap(),
        };
        assert!(excessive_aggregate_values.to_anytype().is_err());
        assert!(FilterNumber::new(Number::from_f64(MAX_FILTER_NUMBER_ABS).unwrap()).is_ok());
        assert!(FilterNumber::new(Number::from_f64(MAX_FILTER_NUMBER_ABS + 1.0).unwrap()).is_err());
    }

    #[test]
    fn filter_text_date_and_property_key_boundaries_are_exact() {
        for format in ["text", "url", "email", "phone"] {
            for (length, accepted) in [
                (MAX_FILTER_TEXT_CHARS, true),
                (MAX_FILTER_TEXT_CHARS + 1, false),
            ] {
                let input = json!({
                    "format":format,
                    "property_key":"contact",
                    "condition":"eq",
                    "value":"é".repeat(length),
                });
                let converted =
                    decode_filter(input).is_ok_and(|filter| filter.to_anytype().is_ok());
                assert_eq!(
                    converted, accepted,
                    "{format} text boundary {length} had the wrong result"
                );
            }
        }
        for (length, accepted) in [
            (MAX_FILTER_DATE_CHARS, true),
            (MAX_FILTER_DATE_CHARS + 1, false),
        ] {
            let input = json!({
                "format":"date",
                "property_key":"due",
                "condition":"eq",
                "value":"é".repeat(length),
            });
            assert_eq!(
                decode_filter(input).is_ok_and(|filter| filter.to_anytype().is_ok()),
                accepted,
                "date boundary {length} had the wrong result"
            );
        }

        for (format, mut input) in canonical_inputs_by_format() {
            input["property_key"] = json!("é".repeat(crate::domain::MAX_TYPE_KEY_CHARS));
            let converted = decode_filter(input.clone())
                .expect("maximum property key should decode")
                .to_anytype()
                .expect("maximum property key should convert");
            assert_eq!(
                serde_json::to_value(converted).unwrap()["property_key"]
                    .as_str()
                    .expect("serialized property key")
                    .chars()
                    .count(),
                crate::domain::MAX_TYPE_KEY_CHARS,
                "{format} did not preserve the maximum property key"
            );
            input["property_key"] = json!("é".repeat(crate::domain::MAX_TYPE_KEY_CHARS + 1));
            assert!(
                decode_filter(input).is_err(),
                "{format} accepted an oversized property key"
            );
        }
    }

    #[test]
    fn expression_and_per_group_array_boundaries_fail_closed() {
        let leaf = json!({
            "format":"text",
            "property_key":"name",
            "condition":"eq",
            "value":"road"
        });
        let fifty_conditions = vec![leaf.clone(); crate::validation::MAX_FILTERS];
        assert!(
            serde_json::from_value::<McpFilterExpression>(json!({
                "operator":"and",
                "conditions":fifty_conditions,
                "filters":[]
            }))
            .is_ok(),
            "per-group condition maximum should decode"
        );
        assert!(
            serde_json::from_value::<McpFilterExpression>(json!({
                "operator":"and",
                "conditions":vec![leaf.clone(); crate::validation::MAX_FILTERS + 1],
                "filters":[]
            }))
            .is_err(),
            "per-group condition overflow decoded"
        );

        let child = json!({
            "operator":"and",
            "conditions":[leaf],
            "filters":[]
        });
        assert!(
            serde_json::from_value::<McpFilterExpression>(json!({
                "operator":"and",
                "conditions":[],
                "filters":vec![child.clone(); crate::validation::MAX_FILTERS]
            }))
            .is_ok(),
            "per-group child maximum should decode"
        );
        assert!(
            serde_json::from_value::<McpFilterExpression>(json!({
                "operator":"and",
                "conditions":[],
                "filters":vec![child; crate::validation::MAX_FILTERS + 1]
            }))
            .is_err(),
            "per-group child overflow decoded"
        );

        for malformed in [
            json!({"operator":"and"}),
            json!({"operator":"or","conditions":[],"filters":[]}),
            json!({
                "operator":"and",
                "conditions":[],
                "filters":[{"operator":"or","conditions":[],"filters":[]}]
            }),
        ] {
            let expression: McpFilterExpression =
                serde_json::from_value(malformed).expect("closed expression shape decodes");
            assert!(
                expression.to_anytype().is_err(),
                "empty expression reached conversion"
            );
        }
    }

    #[test]
    fn file_and_object_operands_remain_wire_safe_ids() {
        let invalid = [
            "path/segment".to_owned(),
            ".".to_owned(),
            "..".to_owned(),
            "idé".to_owned(),
            "x".repeat(crate::domain::MAX_ENTITY_ID_CHARS + 1),
        ];
        for format in ["files", "objects"] {
            for value in &invalid {
                let filter = json!({
                    "format":format,
                    "property_key":"attachments",
                    "condition":"in",
                    "values":[value]
                });
                assert!(
                    serde_json::from_value::<McpFilter>(filter).is_err(),
                    "{format} accepted unsafe id {value:?}"
                );
            }
        }
    }

    #[test]
    fn canonical_cursor_fingerprint_uses_the_unchanged_wire_shape() {
        let wire = json!({
            "operator":"and",
            "conditions":[{
                "format":"checkbox",
                "property_key":"done",
                "condition":"eq",
                "value":true
            }],
            "filters":[]
        });
        let expression: McpFilterExpression = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(serde_json::to_value(&expression).unwrap(), wire);
        assert_eq!(
            QueryFingerprint::from_normalized(&expression).unwrap(),
            QueryFingerprint::from_normalized(&json!({
                "filters":[],
                "conditions":[{
                    "value":true,
                    "condition":"eq",
                    "property_key":"done",
                    "format":"checkbox"
                }],
                "operator":"and"
            }))
            .unwrap()
        );
        assert_ne!(
            QueryFingerprint::from_normalized(&expression).unwrap(),
            QueryFingerprint::from_normalized(&json!({
                "operator":"and",
                "conditions":[{
                    "format":"checkbox",
                    "property_key":"done",
                    "condition":"ne",
                    "value":true
                }],
                "filters":[]
            }))
            .unwrap()
        );
    }

    fn expression_fingerprint(filter: Value, operator: &str) -> QueryFingerprint {
        let expression: McpFilterExpression = serde_json::from_value(json!({
            "operator":operator,
            "conditions":[filter],
            "filters":[]
        }))
        .expect("valid cursor filter expression");
        QueryFingerprint::from_normalized(&expression.cursor_binding_value().unwrap()).unwrap()
    }

    #[test]
    fn cursor_resolution_separates_every_supported_semantic_leaf() {
        let cases = accepted_cases();
        assert_eq!(cases.len(), 43, "cursor inventory changed");
        let fingerprints = cases
            .iter()
            .map(|case| expression_fingerprint(case.input.clone(), "and"))
            .collect::<Vec<_>>();
        let store = CursorStore::new().unwrap();

        for (index, (case, fingerprint)) in cases.iter().zip(&fingerprints).enumerate() {
            let token = store
                .issue(PageOffset::new(20).unwrap(), *fingerprint)
                .unwrap();
            assert_eq!(store.resolve(&token, *fingerprint).unwrap().get(), 20);

            for (other_index, other) in fingerprints.iter().enumerate() {
                if index == other_index {
                    continue;
                }
                assert_eq!(
                    store.resolve(&token, *other).unwrap_err().code(),
                    ValidationCode::CursorMismatch,
                    "cursor did not separate inventory entries {index} and {other_index}"
                );
            }

            let mut changed_key = case.input.clone();
            changed_key["property_key"] = json!(format!("changed_{}", case.format));
            assert_eq!(
                store
                    .resolve(&token, expression_fingerprint(changed_key, "and"))
                    .unwrap_err()
                    .code(),
                ValidationCode::CursorMismatch,
                "{} cursor did not bind the property key",
                case.format
            );
            assert_eq!(
                store
                    .resolve(&token, expression_fingerprint(case.input.clone(), "or"))
                    .unwrap_err()
                    .code(),
                ValidationCode::CursorMismatch,
                "{} cursor did not bind the expression operator",
                case.format
            );
        }
    }

    #[test]
    fn cursor_binding_normalizes_commutative_groups_and_set_values_only() {
        let presented = json!({
            "operator":"and",
            "conditions":[
                {
                    "format":"select",
                    "property_key":"tag",
                    "condition":"in",
                    "values":["beta","alpha","alpha"]
                },
                {
                    "format":"text",
                    "property_key":"name",
                    "condition":"contains",
                    "value":"road"
                },
                {
                    "format":"text",
                    "property_key":"name",
                    "condition":"contains",
                    "value":"road"
                }
            ],
            "filters":[
                {
                    "operator":"and",
                    "conditions":[{
                        "format":"date",
                        "property_key":"due",
                        "condition":"in",
                        "value":"2026-07-21"
                    }],
                    "filters":[]
                },
                {
                    "operator":"or",
                    "conditions":[
                        {
                            "format":"number",
                            "property_key":"priority",
                            "condition":"gt",
                            "value":2
                        },
                        {
                            "format":"checkbox",
                            "property_key":"done",
                            "condition":"eq",
                            "value":true
                        }
                    ],
                    "filters":[]
                },
                {
                    "operator":"or",
                    "conditions":[
                        {
                            "format":"number",
                            "property_key":"priority",
                            "condition":"gt",
                            "value":2
                        },
                        {
                            "format":"checkbox",
                            "property_key":"done",
                            "condition":"eq",
                            "value":true
                        }
                    ],
                    "filters":[]
                }
            ]
        });
        let equivalent = json!({
            "operator":"and",
            "conditions":[
                {
                    "format":"text",
                    "property_key":"name",
                    "condition":"contains",
                    "value":"road"
                },
                {
                    "format":"select",
                    "property_key":"tag",
                    "condition":"in",
                    "values":["alpha","beta"]
                }
            ],
            "filters":[
                {
                    "operator":"or",
                    "conditions":[
                        {
                            "format":"checkbox",
                            "property_key":"done",
                            "condition":"eq",
                            "value":true
                        },
                        {
                            "format":"number",
                            "property_key":"priority",
                            "condition":"gt",
                            "value":2
                        }
                    ],
                    "filters":[]
                },
                {
                    "operator":"and",
                    "conditions":[{
                        "format":"date",
                        "property_key":"due",
                        "condition":"in",
                        "value":"2026-07-21"
                    }],
                    "filters":[]
                }
            ]
        });
        let mut different = equivalent.clone();
        different["conditions"][0]["value"] = json!("different");
        let presented: McpFilterExpression = serde_json::from_value(presented.clone()).unwrap();
        let equivalent: McpFilterExpression = serde_json::from_value(equivalent).unwrap();
        let presented_binding = presented.cursor_binding_value().unwrap();
        let equivalent_binding = equivalent.cursor_binding_value().unwrap();
        assert_eq!(presented_binding, equivalent_binding);
        assert_eq!(
            QueryFingerprint::from_normalized(&presented_binding).unwrap(),
            QueryFingerprint::from_normalized(&equivalent_binding).unwrap()
        );

        let forwarded = serde_json::to_value(presented.to_anytype().unwrap()).unwrap();
        assert_eq!(forwarded["conditions"].as_array().unwrap().len(), 3);
        assert_eq!(forwarded["conditions"][0]["select"], "beta,alpha,alpha");
        assert_eq!(forwarded["filters"].as_array().unwrap().len(), 3);

        let different: McpFilterExpression = serde_json::from_value(different).unwrap();
        assert_ne!(
            QueryFingerprint::from_normalized(&equivalent_binding).unwrap(),
            QueryFingerprint::from_normalized(&different.cursor_binding_value().unwrap()).unwrap()
        );
    }

    #[test]
    fn every_set_valued_operand_normalizes_order_and_duplicates() {
        for (format, property_key, presented, expected) in [
            ("select", "tag", json!(["b", "a", "a"]), json!(["a", "b"])),
            (
                "multi_select",
                "tags",
                json!(["b", "a", "a"]),
                json!(["a", "b"]),
            ),
            (
                "files",
                "attachments",
                json!(["file-2", "file-1", "file-1"]),
                json!(["file-1", "file-2"]),
            ),
            (
                "objects",
                "links",
                json!(["object-2", "object-1", "object-1"]),
                json!(["object-1", "object-2"]),
            ),
        ] {
            let filter: McpFilter = serde_json::from_value(json!({
                "format":format,
                "property_key":property_key,
                "condition":"in",
                "values":presented
            }))
            .unwrap();
            assert_eq!(filter.cursor_binding_value().unwrap()["values"], expected);
        }
    }

    #[test]
    fn nested_expression_conversion_preserves_operator_and_structure() {
        let expression: McpFilterExpression = serde_json::from_value(json!({
            "operator":"or",
            "conditions":[{
                "format":"text",
                "property_key":"name",
                "condition":"contains",
                "value":"road"
            }],
            "filters":[{
                "operator":"and",
                "conditions":[{
                    "format":"checkbox",
                    "property_key":"done",
                    "condition":"eq",
                    "value":false
                }],
                "filters":[]
            }]
        }))
        .unwrap();
        assert_eq!(
            serde_json::to_value(expression.to_anytype().unwrap()).unwrap(),
            json!({
                "conditions":[{
                    "condition":"contains",
                    "property_key":"name",
                    "text":"road"
                }],
                "filters":[{
                    "conditions":[{
                        "condition":"eq",
                        "property_key":"done",
                        "checkbox":false
                    }],
                    "operator":"and"
                }],
                "operator":"or"
            })
        );
    }

    #[test]
    fn shared_schema_is_closed_and_carries_all_bounds() {
        #[derive(JsonSchema)]
        #[schemars(deny_unknown_fields)]
        #[expect(dead_code, reason = "schema-only test fixture")]
        struct FilterSchemaInput {
            /// Shared bounded filters.
            filters: McpFilterExpression,
        }

        let schema = input_schema::<FilterSchemaInput>().unwrap();
        let encoded = serde_json::to_string(schema.as_ref()).unwrap();
        assert!(!encoded.contains("additionalProperties\":true"));
        assert!(encoded.contains("\"maxItems\":50"));
        assert!(encoded.contains("\"maxItems\":100"));
        assert!(encoded.contains("\"maxLength\":4096"));
        assert!(encoded.contains("\"maximum\":1000000000000000.0"));

        let definitions = schema["$defs"].as_object().unwrap();
        let expression = &definitions["McpFilterExpression"];
        let branches = expression["anyOf"].as_array().unwrap();
        assert_eq!(branches.len(), 2);
        assert_eq!(branches[0]["required"], json!(["operator", "conditions"]));
        assert_eq!(branches[1]["required"], json!(["operator", "filters"]));
        assert_eq!(branches[0]["properties"]["conditions"]["minItems"], 1);
        assert_eq!(branches[0]["properties"]["filters"]["minItems"], 0);
        assert_eq!(branches[1]["properties"]["conditions"]["minItems"], 0);
        assert_eq!(branches[1]["properties"]["filters"]["minItems"], 1);
        for branch in branches {
            assert_eq!(branch["properties"]["conditions"]["default"], json!([]));
            assert_eq!(branch["properties"]["filters"]["default"], json!([]));
            assert_eq!(branch["properties"]["conditions"]["maxItems"], 50);
            assert_eq!(branch["properties"]["filters"]["maxItems"], 50);
        }

        let value_lists = definitions
            .iter()
            .filter(|(name, _)| name.starts_with("NonEmptyFilterValueList100Of"))
            .map(|(_, schema)| schema)
            .collect::<Vec<_>>();
        assert_eq!(value_lists.len(), 3);
        for value_list in value_lists {
            assert_eq!(value_list["minItems"], 1);
            assert_eq!(value_list["maxItems"], 100);
        }

        let empty: McpFilterExpression = serde_json::from_value(json!({"operator":"and"})).unwrap();
        assert!(empty.to_anytype().is_err());
        let explicit_empty: McpFilterExpression = serde_json::from_value(json!({
            "operator":"and",
            "conditions":[],
            "filters":[]
        }))
        .unwrap();
        assert!(explicit_empty.to_anytype().is_err());
        for field in ["conditions", "filters"] {
            let mut value = json!({"operator":"and"});
            value[field] = Value::Null;
            assert!(serde_json::from_value::<McpFilterExpression>(value).is_err());
        }

        let date_schema = serde_json::to_value(schema_for!(DateCondition)).unwrap();
        let conditions = date_schema["oneOf"]
            .as_array()
            .unwrap()
            .iter()
            .map(|condition| condition["const"].as_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(conditions, ["eq", "lt", "lte", "gt", "gte", "in"]);
    }
}
