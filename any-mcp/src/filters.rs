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
    domain::{AnytypeReference, BoundedText, EntityId, ObjectId, TypeKey},
    error::ToolError,
    handler_support::HandlerError,
    validation::{BoundedList, FilterBudget, FilterList, FilterValueList},
};

/// Maximum characters accepted in a scalar textual filter value.
pub const MAX_FILTER_TEXT_CHARS: usize = 4_096;
/// Maximum characters accepted in a date filter value.
pub const MAX_FILTER_DATE_CHARS: usize = 64;
/// Maximum absolute numeric filter value.
pub const MAX_FILTER_NUMBER_ABS: f64 = 1_000_000_000_000_000.0;

/// Bounded scalar text accepted by textual filter formats.
pub type FilterText = BoundedText<MAX_FILTER_TEXT_CHARS>;
/// Bounded date text accepted by the Anytype HTTP API.
pub type FilterDate = BoundedText<MAX_FILTER_DATE_CHARS>;

/// Logical operator for one bounded filter group.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FilterOperator {
    /// Require every condition and nested group.
    And,
    /// Require at least one condition or nested group.
    Or,
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
/// filters are forwarded exactly as supplied. They are currently affected by
/// upstream `anytype-heart#2879`; this server does not rewrite them into a
/// query with different semantics.
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
        values: FilterValueList<AnytypeReference>,
    },
    /// Multi-select property.
    MultiSelect {
        /// Property key.
        property_key: TypeKey,
        /// Array operator.
        condition: ArrayCondition,
        /// Compared tag ids or keys.
        values: FilterValueList<AnytypeReference>,
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

/// One nested, bounded filter expression shared by MCP workflows.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
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

fn nonempty_values<T: AsRef<str>, const MAX: usize>(
    values: &BoundedList<T, MAX>,
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
    use rmcp::schemars::schema_for;
    use serde_json::{Number, Value, json};

    use super::*;
    use crate::{
        cursor::QueryFingerprint,
        schema::input_schema,
        validation::{MAX_FILTER_DEPTH, MAX_FILTER_VALUES},
    };

    fn assert_conversion(input: Value, expected: Value) {
        let filter: McpFilter = serde_json::from_value(input).expect("valid MCP filter");
        let actual = serde_json::to_value(filter.to_anytype().expect("Anytype conversion"))
            .expect("serialize Anytype filter");
        assert_eq!(actual, expected);
    }

    #[test]
    fn every_supported_format_and_condition_converts_one_to_one() {
        for (condition, expected) in [
            ("eq", "eq"),
            ("ne", "ne"),
            ("contains", "contains"),
            ("not_contains", "ncontains"),
        ] {
            assert_conversion(
                json!({"format":"text","property_key":"name","condition":condition,"value":"road"}),
                json!({"condition":expected,"property_key":"name","text":"road"}),
            );
        }
        for (condition, expected) in [
            ("eq", "eq"),
            ("ne", "ne"),
            ("lt", "lt"),
            ("lte", "lte"),
            ("gt", "gt"),
            ("gte", "gte"),
        ] {
            assert_conversion(
                json!({"format":"number","property_key":"priority","condition":condition,"value":2.5}),
                json!({"condition":expected,"property_key":"priority","number":2.5}),
            );
        }
        assert_conversion(
            json!({"format":"number","property_key":"priority","condition":"eq","value":2}),
            json!({"condition":"eq","property_key":"priority","number":2}),
        );
        for (condition, expected) in [
            ("eq", "eq"),
            ("lt", "lt"),
            ("lte", "lte"),
            ("gt", "gt"),
            ("gte", "gte"),
            ("in", "in"),
        ] {
            assert_conversion(
                json!({"format":"date","property_key":"due","condition":condition,"value":"2026-07-21T00:00:00Z"}),
                json!({"condition":expected,"property_key":"due","date":"2026-07-21T00:00:00Z"}),
            );
        }
        for (condition, expected) in [("in", "in"), ("not_in", "nin")] {
            assert_conversion(
                json!({"format":"select","property_key":"tag","condition":condition,"values":["alpha","beta"]}),
                json!({"condition":expected,"property_key":"tag","select":"alpha,beta"}),
            );
        }
        for (condition, expected) in [("in", "in"), ("not_in", "nin"), ("all_in", "all_in")] {
            assert_conversion(
                json!({"format":"multi_select","property_key":"tags","condition":condition,"values":["alpha","beta"]}),
                json!({"condition":expected,"property_key":"tags","multi_select":["alpha","beta"]}),
            );
            assert_conversion(
                json!({"format":"files","property_key":"attachments","condition":condition,"values":["file-1","file-2"]}),
                json!({"condition":expected,"property_key":"attachments","files":["file-1","file-2"]}),
            );
            assert_conversion(
                json!({"format":"objects","property_key":"links","condition":condition,"values":["object-1","object-2"]}),
                json!({"condition":expected,"property_key":"links","objects":["object-1","object-2"]}),
            );
        }
        for (condition, expected) in [("eq", "eq"), ("ne", "ne")] {
            assert_conversion(
                json!({"format":"checkbox","property_key":"done","condition":condition,"value":false}),
                json!({"condition":expected,"property_key":"done","checkbox":false}),
            );
        }
        for (format, field, value) in [
            ("url", "url", "https://example.invalid"),
            ("email", "email", "agent@example.invalid"),
            ("phone", "phone", "+1-555-0100"),
        ] {
            let mut expected = json!({"condition":"contains","property_key":"contact"});
            expected
                .as_object_mut()
                .unwrap()
                .insert(field.to_owned(), json!(value));
            assert_conversion(
                json!({"format":format,"property_key":"contact","condition":"contains","value":value}),
                expected,
            );
        }
        assert_conversion(
            json!({"format":"empty","property_key":"status"}),
            json!({"condition":"empty","property_key":"status"}),
        );
        assert_conversion(
            json!({"format":"not_empty","property_key":"status"}),
            json!({"condition":"nempty","property_key":"status"}),
        );
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

        let values = (0..MAX_FILTER_VALUES)
            .map(|index| AnytypeReference::new(format!("v{index}")).unwrap())
            .collect();
        let max_values = McpFilter::Select {
            property_key: TypeKey::new("tag").unwrap(),
            condition: SelectCondition::In,
            values: FilterValueList::new(values).unwrap(),
        };
        assert!(max_values.to_anytype().is_ok());
        let empty_values = McpFilter::Select {
            property_key: TypeKey::new("tag").unwrap(),
            condition: SelectCondition::In,
            values: FilterValueList::new(Vec::new()).unwrap(),
        };
        assert!(empty_values.to_anytype().is_err());
        let aggregate_values = McpFilterExpression {
            operator: FilterOperator::And,
            conditions: FilterList::new(vec![
                McpFilter::Select {
                    property_key: TypeKey::new("tag").unwrap(),
                    condition: SelectCondition::In,
                    values: FilterValueList::new(
                        (0..51)
                            .map(|index| AnytypeReference::new(format!("a{index}")).unwrap())
                            .collect(),
                    )
                    .unwrap(),
                },
                McpFilter::Select {
                    property_key: TypeKey::new("tag").unwrap(),
                    condition: SelectCondition::In,
                    values: FilterValueList::new(
                        (0..50)
                            .map(|index| AnytypeReference::new(format!("b{index}")).unwrap())
                            .collect(),
                    )
                    .unwrap(),
                },
            ])
            .unwrap(),
            filters: FilterList::new(Vec::new()).unwrap(),
        };
        assert!(aggregate_values.to_anytype().is_err());
        assert!(FilterNumber::new(Number::from_f64(MAX_FILTER_NUMBER_ABS).unwrap()).is_ok());
        assert!(FilterNumber::new(Number::from_f64(MAX_FILTER_NUMBER_ABS * 2.0).unwrap()).is_err());
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
