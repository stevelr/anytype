//! Filters and sorting
//!

use serde::{Deserialize, Deserializer, Serialize, ser::SerializeStruct};
use serde_json::{Number, Value};
use tracing::warn;

use crate::{
    Result,
    config::{DEFAULT_PAGINATION_LIMIT, MAX_PAGINATION_LIMIT},
    prelude::*,
};

/// Sort direction for search results
#[derive(Debug, Default, Deserialize, Serialize, Clone, strum::Display)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum SortDirection {
    #[default]
    Asc,
    Desc,
}

/// Sort options for search results
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct Sort {
    #[serde(default)]
    pub direction: SortDirection,
    pub property_key: String,
}

impl Sort {
    /// Constructs an ascending sort request.
    pub fn asc(property: impl Into<String>) -> Self {
        Sort {
            direction: SortDirection::Asc,
            property_key: property.into(),
        }
    }

    /// Constructs a descending sort request.
    pub fn desc(property: impl Into<String>) -> Self {
        Sort {
            direction: SortDirection::Desc,
            property_key: property.into(),
        }
    }
}

/// Operator for combining filter conditions
#[derive(Debug, Serialize, Clone, Default, strum::Display)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum FilterOperator {
    #[default]
    And,
    Or,
}

/// Expression with nested AND/OR conditions.
///
/// Prefer `Filter` methods such as `Filter::text_equal`
/// and `Filter::date_greater`, over creating filters
/// and expressions with this, because the Filter
/// functions are more convenient, and limited to the
/// valid combinations of field and operator supported by the server api.
///
/// This struct is public, to allow constructing arbitrary expressions,
/// including invalid combinations that result in ApiError (code 400)
/// responses from the server.
///
/// For the common use-case of a logical AND
/// expression made up of filters, you can use the `into()` function
/// with a `Vec<Filter>`. For example:
///
/// ```rust
/// use anytype::prelude::*;
/// /// common use-case:
/// /// create a logical AND FilterExpression
/// let expr : FilterExpression = vec![
///   Filter::text_contains("title", "draft"),
///   Filter::date_greater("last_modified", "2025-01-01"),
/// ].into();
/// ```
///
/// ```rust
/// use anytype::prelude::*;
/// /// create a more complex filter expression
/// /// high priority and recent tasks
/// let high_priority = FilterExpression::and(
///   vec![
///     Filter::select_in("status", vec!["open"]),
///     Filter::number_less("priority", 3),
///     Filter::date_greater("created_date", "2025-12-01"),
///   ].into(),
///   Vec::new()
/// );
/// /// backlog and older
/// let backlog = FilterExpression::and(
///     vec![
///         Filter::select_in("status", vec!["backlog"]),
///         Filter::date_less("created_date", "2025-01-01"),
///     ].into(),
///     Vec::new()
/// );
/// // type task AND (either high priority or backlog)
/// let tasks_to_review = FilterExpression::and(
///    vec![
///       Filter::type_in(vec!["task_id"])
///    ],
///    vec![ FilterExpression::or(
///       Vec::new(),
///       vec![ high_priority, backlog ]
///    )]
/// );
/// ```
#[derive(Debug, Serialize, Default)]
pub struct FilterExpression {
    /// filter conditions
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub conditions: Vec<Filter>,

    /// nested filter expressions
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub filters: Vec<FilterExpression>,

    /// logical operator for combining filters (and, or). Default: "and"
    pub operator: FilterOperator,
}

impl From<Vec<Filter>> for FilterExpression {
    /// Creates an AND expression from conditions.
    fn from(conditions: Vec<Filter>) -> Self {
        FilterExpression {
            conditions,
            filters: Default::default(),
            operator: FilterOperator::And,
        }
    }
}

impl FilterExpression {
    pub(crate) fn is_empty(&self) -> bool {
        self.conditions.is_empty() && self.filters.is_empty()
    }

    /// Constructs an AND expression for combining filters.
    pub fn and(conditions: Vec<Filter>, filters: Vec<FilterExpression>) -> Self {
        FilterExpression {
            conditions,
            filters,
            operator: FilterOperator::And,
        }
    }

    /// Constructs an OR expression for combining filters.
    pub fn or(conditions: Vec<Filter>, filters: Vec<FilterExpression>) -> Self {
        FilterExpression {
            conditions,
            filters,
            operator: FilterOperator::Or,
        }
    }
}

/// Query parameters for list and search methods.
/// Not exported from crate. Callers should
/// invoke the builder struct with `limit` and `offset` for pagination,
/// `filter` (if the query supports filtering),
/// and any other available property setters.
#[derive(Debug, Clone, Default)]
pub(crate) struct Query {
    pub(crate) params: Vec<(String, String)>,
}

impl Query {
    /// Sets the pagination limit (number of items to return).
    /// Default limit is 100 items. If value set is greater than the max allowed by the api (1000),
    /// a warning is printed and the limit is reduced to the max.
    pub(crate) fn limit(self, mut limit: usize) -> Self {
        if limit > MAX_PAGINATION_LIMIT {
            warn!(
                "attempt to set pagination limit to {limit}. reducing to max value: {MAX_PAGINATION_LIMIT}"
            );
            limit = MAX_PAGINATION_LIMIT;
        }
        if limit != DEFAULT_PAGINATION_LIMIT {
            self.add_param("limit", limit.to_string())
        } else {
            self
        }
    }

    /// Sets the pagination offset (starting item number for the next page)
    /// Default offset is 0.
    pub(crate) fn offset(self, offset: usize) -> Self {
        if offset != 0 {
            self.add_param("offset", offset.to_string())
        } else {
            self
        }
    }

    /// Adds query parameter name=value to request url.
    /// This is a general-purpose (and therefore possibly error-prone) function,
    /// and not exported outside the crate.
    fn add_param(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.params.push((name.into(), value.into()));
        self
    }

    /// Adds a filter, converting Query into QueryWithFilters
    pub(crate) fn add_filters(self, filters: &[Filter]) -> QueryWithFilters {
        let mut query_with_filters = QueryWithFilters::from(filters);
        query_with_filters.params.extend(self.params);
        query_with_filters
    }

    /// Sets optional limit (crate-internal)
    pub(crate) fn set_limit_opt(self, limit: &Option<usize>) -> Self {
        if let Some(limit) = limit {
            Self::limit(self, *limit)
        } else {
            self
        }
    }

    /// Sets optional offset (crate-internal)
    pub(crate) fn set_offset_opt(self, offset: &Option<usize>) -> Self {
        if let Some(offset) = offset {
            Self::offset(self, *offset)
        } else {
            self
        }
    }
}

/// converts Query to QueryWithFilters
/// convenience method: Most http requests support optional
/// query parameters `limit` and `offset`. A subset of backend apis
/// also support filter parameters (especially the list_* methods).
/// This conversion lets us start with the most common Query, and for backend apis
/// that support QueryWithFilters, accept `Into<QueryWithFilters>` to handle
/// either Query (with no filters) or QueryWithFilters.
impl From<Query> for QueryWithFilters {
    fn from(query: Query) -> Self {
        QueryWithFilters {
            params: query.params,
            error: None,
        }
    }
}

/// Internal structure to store common query parameters (`limit` and `offset`),
/// additional parameters. Also stores an error message for deferred reporting.
/// (parameters are when Query is converted to QueryWithFilters, but
/// not reported until submitted to HttpClient)
#[derive(Default, Clone, Debug)]
pub(crate) struct QueryWithFilters {
    pub(crate) params: Vec<(String, String)>,
    // deferred validation error, or None if no errors encountered so far.
    error: Option<String>,
}

impl QueryWithFilters {
    pub(crate) fn validate(&self) -> crate::Result<()> {
        if let Some(error) = &self.error {
            return Err(AnytypeError::Validation {
                message: error.to_owned(),
            });
        }
        if self.params.iter().any(|(key, _)| key.trim().is_empty()) {
            return Err(AnytypeError::Validation {
                message: "query filter has empty property name".to_string(),
            });
        }
        Ok(())
    }
}

impl From<&[Filter]> for QueryWithFilters {
    fn from(filters: &[Filter]) -> Self {
        let mut errors = Vec::new();
        let mut params = Vec::new();
        for filter in filters {
            if let Some(err) = filter.validate() {
                errors.push(err)
            } else {
                params.extend(filter.to_query_params());
            }
        }
        QueryWithFilters {
            params,
            error: if errors.is_empty() {
                None
            } else {
                Some(errors.join(","))
            },
        }
    }
}

/// Condition in a Filter
#[derive(
    Copy,
    Clone,
    Debug,
    Deserialize,
    Serialize,
    PartialEq,
    Eq,
    strum::Display,
    strum::EnumString,
    Default,
)]
pub enum Condition {
    #[default]
    None,

    #[serde(rename = "eq")]
    #[strum(serialize = "eq")]
    Equal,

    #[serde(rename = "ne")]
    #[strum(serialize = "ne")]
    NotEqual,

    /// Property is empty
    #[serde(rename = "empty")]
    #[strum(serialize = "empty")]
    Empty,

    /// Property is defined and not empty
    #[serde(rename = "nempty")]
    #[strum(serialize = "nempty")]
    NotEmpty,

    /// Number or Date is less than the value
    #[serde(rename = "lt")]
    #[strum(serialize = "lt")]
    Less,

    /// Number or Date is less than or equal to the value
    #[serde(rename = "lte")]
    #[strum(serialize = "lte")]
    LessOrEqual,

    /// Number or Date is greater than the value
    #[serde(rename = "gt")]
    #[strum(serialize = "gt")]
    Greater,

    /// Number or Date is greater than or equal to the value
    #[serde(rename = "gte")]
    #[strum(serialize = "gte")]
    GreaterOrEqual,

    /// Text field contains the value
    #[serde(rename = "contains")]
    #[strum(serialize = "contains")]
    Contains,

    /// Text field does not contain the value
    #[serde(rename = "ncontains")]
    #[strum(serialize = "ncontains")]
    NotContains,

    /// property is in the list.
    /// used for tags (select, multi_select), files, and objects
    #[serde(rename = "in")]
    #[strum(serialize = "in")]
    In,

    /// used for tags (select, multi_select), files, and objects
    #[serde(rename = "nin")]
    #[strum(serialize = "nin")]
    NotIn,

    // the following variants were found in model.pb.go, but are undocumented in the yaml openapi spec
    //
    /// Multi-select property includes all the values
    #[serde(rename = "all")]
    #[strum(serialize = "all")]
    All,

    /// Multi-select property are a subset of the values
    #[serde(rename = "all_in")]
    #[strum(serialize = "all_in")]
    AllIn,

    /// Multi-select property are not a subset of the values
    #[serde(rename = "not_all_in")]
    #[strum(serialize = "not_all_in")]
    NotAllIn,

    #[serde(rename = "exact_in")]
    #[strum(serialize = "exact_in")]
    ExactIn,

    #[serde(rename = "not_exact_in")]
    #[strum(serialize = "not_exact_in")]
    NotExactIn,

    /// Tests whether the property is defined
    #[serde(rename = "exists")]
    #[strum(serialize = "exists")]
    Exists,
}

impl Condition {
    /// Returns true when the condition is [`Condition::None`].
    pub fn is_none(&self) -> bool {
        matches!(self, Condition::None)
    }
    /// Returns true when the condition is [`Condition::Equal`].
    pub fn is_equal(&self) -> bool {
        matches!(self, Condition::Equal)
    }
}

impl Filter {
    /// Matches when the property is empty.
    pub fn is_empty(property_key: impl Into<String>) -> Self {
        Filter::Empty {
            property_key: property_key.into(),
            condition: Condition::Empty,
        }
    }

    /// Matches when the property is not empty.
    pub fn not_empty(property_key: impl Into<String>) -> Self {
        Filter::NotEmpty {
            property_key: property_key.into(),
            condition: Condition::NotEmpty,
        }
    }

    /// Matches when the checkbox property is true.
    pub fn checkbox_true(property_key: impl Into<String>) -> Self {
        Filter::Checkbox {
            property_key: property_key.into(),
            condition: Condition::Equal,
            checkbox: true,
        }
    }

    /// Matches when the checkbox property is false.
    pub fn checkbox_false(property_key: impl Into<String>) -> Self {
        Filter::Checkbox {
            property_key: property_key.into(),
            condition: Condition::Equal,
            checkbox: false,
        }
    }

    /// Matches when the text property equals the value.
    pub fn text_equal(property_key: impl Into<String>, value: impl Into<String>) -> Self {
        Filter::Text {
            property_key: property_key.into(),
            condition: Condition::Equal,
            text: value.into(),
        }
    }

    /// Matches when the text property does not equal the value.
    pub fn text_not_equal(property_key: impl Into<String>, value: impl Into<String>) -> Self {
        Filter::Text {
            property_key: property_key.into(),
            condition: Condition::NotEqual,
            text: value.into(),
        }
    }

    /// Matches when the text property contains the substring.
    ///
    /// Example:
    /// ```rust,no_run
    /// use anytype::prelude::Filter;
    /// // filter condition where title contains "draft"
    /// let filter = Filter::text_contains("title", "draft");
    /// ```
    pub fn text_contains(property_key: impl Into<String>, value: impl Into<String>) -> Self {
        Filter::Text {
            property_key: property_key.into(),
            condition: Condition::Contains,
            text: value.into(),
        }
    }

    /// Matches when the text property does not contain the substring.
    ///
    /// Example:
    /// ```rust,no_run
    /// use anytype::prelude::Filter;
    /// // filter condition where title does not contain "draft"
    /// let filter = Filter::text_not_contains("title", "draft");
    /// ```
    pub fn text_not_contains(property_key: impl Into<String>, value: impl Into<String>) -> Self {
        Filter::Text {
            property_key: property_key.into(),
            condition: Condition::NotContains,
            text: value.into(),
        }
    }

    /// Matches when the multi-select property is in the array of values.
    ///
    /// Example:
    /// ```rust,no_run
    /// use anytype::prelude::Filter;
    /// let filter = Filter::multi_select_in("tags", vec!["urgent", "critical"]);
    /// ```
    pub fn multi_select_in(
        property_key: impl Into<String>,
        values: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        Filter::MultiSelect {
            property_key: property_key.into(),
            condition: Condition::In,
            multi_select: values.into_iter().map(std::convert::Into::into).collect(),
        }
    }

    /// Matches when the select property is in the array of values.
    ///
    /// Example:
    /// ```rust,no_run
    /// use anytype::prelude::Filter;
    /// let filter = Filter::select_in("status", vec!["open", "backlog"]);
    /// ```
    pub fn select_in(
        property_key: impl Into<String>,
        values: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        Filter::Select {
            property_key: property_key.into(),
            condition: Condition::In,
            select: values.into_iter().map(std::convert::Into::into).collect(),
        }
    }

    /// Matches when the select property equals the value (shortcut for select_in vec["value"]).
    ///
    /// Example:
    /// ```rust,no_run
    /// use anytype::prelude::Filter;
    /// let filter = Filter::select_equal("status", "open");
    /// ```
    pub fn select_equal(property_key: impl Into<String>, value: impl Into<String>) -> Self {
        Filter::Select {
            property_key: property_key.into(),
            condition: Condition::In,
            select: vec![value.into()],
        }
    }

    /// Matches when the 'type' property is one of the options (shortcut for select_in("type", values)).
    ///
    /// Example:
    /// ```rust,no_run
    /// use anytype::prelude::Filter;
    /// // select object types page or note
    /// let filter = Filter::type_in(vec!["page", "note"]);
    /// ```
    pub fn type_in(values: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Filter::Select {
            property_key: "type".into(),
            condition: Condition::In,
            select: values.into_iter().map(std::convert::Into::into).collect(),
        }
    }

    /// Matches when the multi-select property is not in the array of values.
    ///
    /// Example:
    /// ```rust,no_run
    /// use anytype::prelude::Filter;
    /// let filter = Filter::multi_select_not_in("tag", vec!["demo", "test"]);
    /// ```
    pub fn multi_select_not_in(
        property_key: impl Into<String>,
        values: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        Filter::MultiSelect {
            property_key: property_key.into(),
            condition: Condition::NotIn,
            multi_select: values.into_iter().map(std::convert::Into::into).collect(),
        }
    }

    /// Matches when the select property is not in the array of values.
    ///
    /// Example:
    /// ```rust,no_run
    /// use anytype::prelude::Filter;
    /// let filter = Filter::select_not_in("status", vec!["trash", "archived"]);
    /// ```
    pub fn select_not_in(
        property_key: impl Into<String>,
        values: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        Filter::Select {
            property_key: property_key.into(),
            condition: Condition::NotIn,
            select: values.into_iter().map(std::convert::Into::into).collect(),
        }
    }

    /// Matches when the multi-select property has all elements of the array.
    ///
    /// Example:
    /// ```rust,no_run
    /// use anytype::prelude::Filter;
    /// let filter = Filter::multi_select_all("tags", vec!["urgent", "critical"]);
    /// ```
    pub fn multi_select_all(
        property_key: impl Into<String>,
        values: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        Filter::MultiSelect {
            property_key: property_key.into(),
            condition: Condition::All,
            multi_select: values.into_iter().map(std::convert::Into::into).collect(),
        }
    }

    /// Matches multi-select all-in condition.
    ///
    /// Example:
    /// ```rust,no_run
    /// use anytype::prelude::Filter;
    /// let filter = Filter::multi_select_all_in("tags", vec!["urgent", "critical"]);
    /// ```
    pub fn multi_select_all_in(
        property_key: impl Into<String>,
        values: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        Filter::MultiSelect {
            property_key: property_key.into(),
            condition: Condition::AllIn,
            multi_select: values.into_iter().map(std::convert::Into::into).collect(),
        }
    }

    /// Numeric property is equal
    pub fn number_equal(property_key: impl Into<String>, value: impl Into<Number>) -> Self {
        Filter::Number {
            property_key: property_key.into(),
            condition: Condition::Equal,
            number: value.into(),
        }
    }

    /// Numeric property is not equal
    pub fn number_not_equal(property_key: impl Into<String>, value: impl Into<Number>) -> Self {
        Filter::Number {
            property_key: property_key.into(),
            condition: Condition::NotEqual,
            number: value.into(),
        }
    }

    /// Numeric property less than
    pub fn number_less(property_key: impl Into<String>, value: impl Into<Number>) -> Self {
        Filter::Number {
            property_key: property_key.into(),
            condition: Condition::Less,
            number: value.into(),
        }
    }

    /// Numeric property less-than-or-equal
    pub fn number_less_or_equal(property_key: impl Into<String>, value: impl Into<Number>) -> Self {
        Filter::Number {
            property_key: property_key.into(),
            condition: Condition::LessOrEqual,
            number: value.into(),
        }
    }

    /// Numeric property greater-than
    pub fn number_greater(property_key: impl Into<String>, value: impl Into<Number>) -> Self {
        Filter::Number {
            property_key: property_key.into(),
            condition: Condition::Greater,
            number: value.into(),
        }
    }

    /// Numeric property greater-than-or-equal
    pub fn number_greater_or_equal(
        property_key: impl Into<String>,
        value: impl Into<Number>,
    ) -> Self {
        Filter::Number {
            property_key: property_key.into(),
            condition: Condition::GreaterOrEqual,
            number: value.into(),
        }
    }

    /// Date is equal
    pub fn date_equal(property_key: impl Into<String>, value: impl Into<String>) -> Self {
        Filter::Date {
            property_key: property_key.into(),
            condition: Condition::Equal,
            date: value.into(),
        }
    }

    /// Date is not equal
    pub fn date_not_equal(property_key: impl Into<String>, value: impl Into<String>) -> Self {
        Filter::Date {
            property_key: property_key.into(),
            condition: Condition::NotEqual,
            date: value.into(),
        }
    }

    /// Date is less than (use rfc3339 format strings)
    /// Example:
    /// ```rust,no_run
    /// use anytype::prelude::Filter;
    /// // select items due before January 1
    /// let filter = Filter::date_less("due_date", "2026-01-01");
    /// ```
    pub fn date_less(property_key: impl Into<String>, value: impl Into<String>) -> Self {
        Filter::Date {
            property_key: property_key.into(),
            condition: Condition::Less,
            date: value.into(),
        }
    }

    /// Date is less than or equal (use rfc3339 format strings)
    /// Example:
    /// ```rust,no_run
    /// use anytype::prelude::Filter;
    /// // select items due before midnight
    /// let filter = Filter::date_less_or_equal("due_date", "2025-12-31T23:59:59Z");
    /// ```
    pub fn date_less_or_equal(property_key: impl Into<String>, value: impl Into<String>) -> Self {
        Filter::Date {
            property_key: property_key.into(),
            condition: Condition::LessOrEqual,
            date: value.into(),
        }
    }

    /// Date is greater than (use rfc3339 format strings)
    pub fn date_greater(property_key: impl Into<String>, value: impl Into<String>) -> Self {
        Filter::Date {
            property_key: property_key.into(),
            condition: Condition::Greater,
            date: value.into(),
        }
    }

    /// Date is greater than or equal (use rfc3339 format strings)
    pub fn date_greater_or_equal(
        property_key: impl Into<String>,
        value: impl Into<String>,
    ) -> Self {
        Filter::Date {
            property_key: property_key.into(),
            condition: Condition::GreaterOrEqual,
            date: value.into(),
        }
    }

    /// Checkbox field is equal to the (bool) value
    pub fn checkbox_equal(property_key: impl Into<String>, checkbox: bool) -> Self {
        Filter::Checkbox {
            property_key: property_key.into(),
            condition: Condition::Equal,
            checkbox,
        }
    }

    /// Checkbox field is not equal to the (bool) value
    pub fn checkbox_not_equal(property_key: impl Into<String>, checkbox: bool) -> Self {
        Filter::Checkbox {
            property_key: property_key.into(),
            condition: Condition::NotEqual,
            checkbox,
        }
    }
}

/// Expression filters for list and search functions
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum Filter {
    Text {
        condition: Condition,
        property_key: String,
        text: String,
    },
    Number {
        condition: Condition,
        property_key: String,
        number: Number,
    },
    Select {
        condition: Condition,
        property_key: String,
        select: Vec<String>,
    },
    MultiSelect {
        condition: Condition,
        property_key: String,
        #[serde(default, deserialize_with = "deserialize_vec_string_or_null")]
        multi_select: Vec<String>,
    },
    Date {
        condition: Condition,
        property_key: String,
        date: String,
    },
    Checkbox {
        condition: Condition,
        property_key: String,
        checkbox: bool,
    },
    Files {
        condition: Condition,
        property_key: String,
        #[serde(default, deserialize_with = "deserialize_vec_string_or_null")]
        files: Vec<String>,
    },
    Url {
        condition: Condition,
        property_key: String,
        url: String,
    },
    Email {
        condition: Condition,
        property_key: String,
        email: String,
    },
    Phone {
        condition: Condition,
        property_key: String,
        phone: String,
    },
    Objects {
        condition: Condition,
        property_key: String,
        #[serde(default, deserialize_with = "deserialize_vec_string_or_null")]
        objects: Vec<String>,
    },
    Empty {
        condition: Condition,
        property_key: String,
    },
    NotEmpty {
        condition: Condition,
        property_key: String,
    },
    /// View filter
    // not sure if this one is real but it's the definition of "Filter" (line 419 in yaml spec), which is only used in the return "View" object.
    Value {
        condition: Condition,
        property_key: String,
        #[serde(default)]
        value: Option<serde_json::Value>,
    },
}

impl Serialize for Filter {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut state = serializer.serialize_struct("Filter", 3)?;
        match self {
            Filter::Text {
                condition,
                property_key,
                text,
            } => {
                state.serialize_field("condition", condition)?;
                state.serialize_field("property_key", property_key)?;
                state.serialize_field("text", text)?;
            }
            Filter::Number {
                condition,
                property_key,
                number,
            } => {
                state.serialize_field("condition", condition)?;
                state.serialize_field("property_key", property_key)?;
                state.serialize_field("number", number)?;
            }
            Filter::Select {
                condition,
                property_key,
                select,
            } => {
                state.serialize_field("condition", condition)?;
                state.serialize_field("property_key", property_key)?;
                state.serialize_field("select", &join_values(select))?;
            }
            Filter::MultiSelect {
                condition,
                property_key,
                multi_select,
            } => {
                state.serialize_field("condition", condition)?;
                state.serialize_field("property_key", property_key)?;
                state.serialize_field("multi_select", multi_select)?;
            }
            Filter::Date {
                condition,
                property_key,
                date,
            } => {
                state.serialize_field("condition", condition)?;
                state.serialize_field("property_key", property_key)?;
                state.serialize_field("date", date)?;
            }
            Filter::Checkbox {
                condition,
                property_key,
                checkbox,
            } => {
                state.serialize_field("condition", condition)?;
                state.serialize_field("property_key", property_key)?;
                state.serialize_field("checkbox", checkbox)?;
            }
            Filter::Files {
                condition,
                property_key,
                files,
            } => {
                state.serialize_field("condition", condition)?;
                state.serialize_field("property_key", property_key)?;
                state.serialize_field("files", files)?;
            }
            Filter::Url {
                condition,
                property_key,
                url,
            } => {
                state.serialize_field("condition", condition)?;
                state.serialize_field("property_key", property_key)?;
                state.serialize_field("url", url)?;
            }
            Filter::Email {
                condition,
                property_key,
                email,
            } => {
                state.serialize_field("condition", condition)?;
                state.serialize_field("property_key", property_key)?;
                state.serialize_field("email", email)?;
            }
            Filter::Phone {
                condition,
                property_key,
                phone,
            } => {
                state.serialize_field("condition", condition)?;
                state.serialize_field("property_key", property_key)?;
                state.serialize_field("phone", phone)?;
            }
            Filter::Objects {
                condition,
                property_key,
                objects,
            } => {
                state.serialize_field("condition", condition)?;
                state.serialize_field("property_key", property_key)?;
                state.serialize_field("objects", objects)?;
            }
            Filter::Empty {
                condition,
                property_key,
            } => {
                state.serialize_field("condition", condition)?;
                state.serialize_field("property_key", property_key)?;
            }
            Filter::NotEmpty {
                condition,
                property_key,
            } => {
                state.serialize_field("condition", condition)?;
                state.serialize_field("property_key", property_key)?;
            }
            Filter::Value {
                condition,
                property_key,
                value,
            } => {
                state.serialize_field("condition", condition)?;
                state.serialize_field("property_key", property_key)?;
                state.serialize_field("value", value)?;
            }
        }
        state.end()
    }
}

fn join_values(values: &[String]) -> String {
    if values.len() == 1 {
        values[0].clone()
    } else {
        values.join(",")
    }
}

// serde helper to handle nulls when deserializing Vec<String>
fn deserialize_vec_string_or_null<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Option::<Vec<String>>::deserialize(deserializer)?;
    Ok(value.unwrap_or_default())
}

impl Filter {
    fn name(&self) -> &'static str {
        match self {
            Filter::Text { .. } => "text",
            Filter::Number { .. } => "number",
            Filter::Select { .. } => "select",
            Filter::MultiSelect { .. } => "multi_select",
            Filter::Date { .. } => "date",
            Filter::Checkbox { .. } => "checkbox",
            Filter::Files { .. } => "files",
            Filter::Url { .. } => "url",
            Filter::Email { .. } => "email",
            Filter::Phone { .. } => "phone",
            Filter::Objects { .. } => "objects",
            Filter::Empty { .. } => "empty",
            Filter::NotEmpty { .. } => "not_empty",
            Filter::Value { .. } => "value",
        }
    }

    // validate the filter, returning optional error
    pub(crate) fn validate(&self) -> Option<String> {
        match self {
            Filter::Date {
                        condition:
                            Condition::Equal
                            // | Condition::NotEqual // why not? (see types.go line 71)
                            | Condition::Greater
                            | Condition::GreaterOrEqual
                            | Condition::Less
                            | Condition::LessOrEqual
                            | Condition::In
                            | Condition::Empty
                            | Condition::NotEmpty,
                        ..
                    } => None,
            f if f.is_text_type() && matches!(f.condition(),
                            Condition::Equal
                            | Condition::NotEqual
                            | Condition::Contains
                            | Condition::NotContains
                            | Condition::Empty
                            | Condition::NotEmpty,
                            )
                    => None,
            Filter::Select{ condition:
                            Condition::In
                            | Condition::NotIn
                            | Condition::Empty
                            | Condition::NotEmpty,
                            ..
                        } => None,
            f if f.is_array_type() && matches!(f.condition(),
                            Condition::In
                            | Condition::AllIn
                            | Condition::NotIn
                            | Condition::Empty
                            | Condition::NotEmpty,
                        ) => None,
            Filter::Number {
                        condition:
                            Condition::Equal
                            | Condition::NotEqual
                            | Condition::Greater
                            | Condition::GreaterOrEqual
                            | Condition::Less
                            | Condition::LessOrEqual
                            | Condition::Empty
                            | Condition::NotEmpty,
                        ..
                    } => None,
            Filter::Checkbox {
                        condition:
                            Condition::Equal
                            | Condition::NotEqual,
                        ..
                    } => None,
            Filter::Empty { condition: Condition::Empty, .. } => None,
            Filter::NotEmpty { condition: Condition::NotEmpty, .. } => None,

            // skip validation on Value because it's only created by Deserialization
            Filter::Value { .. } => None ,

            // anything else is invalid
            // could have used '_' here but using the more explicit variants
            // to confirm explicitly that we covered them all
            Filter::Select { .. }
                    | Filter::Date { .. }
                    | Filter::Text { .. }
                    | Filter::Url { .. }
                    | Filter::Email { .. }
                    | Filter::Phone { .. }
                    | Filter::MultiSelect { .. }
                    | Filter::Files{ .. }
                    | Filter::Objects { .. }
                    | Filter::Number { .. }
                    | Filter::Checkbox { .. }
                    | Filter::Empty { .. }
                    | Filter::NotEmpty { .. } => {

                   Some(format!("invalid condition '{}' for {} filter", self.condition(), self.name()))

              }
        }
    }

    // helper function used in validate() match patterns
    fn is_array_type(&self) -> bool {
        match self {
            // even though Select is a string array, don't include here
            // because it's handled in the match patterns in validate()
            // (Select differs from the others because it doesn't support AllIn)
            //Filter::Select { .. } => true,
            Filter::MultiSelect { .. } => true,
            Filter::Files { .. } => true,
            Filter::Objects { .. } => true,
            Filter::Value {
                value: Some(Value::Array(_)),
                ..
            } => true,
            _ => false,
        }
    }

    // helper function used in validate() match patterns
    fn is_text_type(&self) -> bool {
        match self {
            Filter::Text { .. } => true,
            // even though Date is a string type, don't include here
            // because it's handled in the match patterns in validate()
            // (Date differs from the other text types because it supports magnitude comparisons (greater, etc.)
            // and doesn't support NotEqual (I have no idea why. See types.go line 71))
            //Filter::Date { .. } => true,
            Filter::Url { .. } => true,
            Filter::Email { .. } => true,
            Filter::Phone { .. } => true,
            Filter::Value {
                value: Some(Value::String(_)),
                ..
            } => true,
            _ => false,
        }
    }

    pub fn condition(&self) -> Condition {
        *match self {
            Filter::Text { condition, .. } => condition,
            Filter::Number { condition, .. } => condition,
            Filter::Select { condition, .. } => condition,
            Filter::MultiSelect { condition, .. } => condition,
            Filter::Date { condition, .. } => condition,
            Filter::Checkbox { condition, .. } => condition,
            Filter::Files { condition, .. } => condition,
            Filter::Url { condition, .. } => condition,
            Filter::Email { condition, .. } => condition,
            Filter::Phone { condition, .. } => condition,
            Filter::Objects { condition, .. } => condition,
            Filter::Empty { condition, .. } => condition,
            Filter::NotEmpty { condition, .. } => condition,
            Filter::Value { condition, .. } => condition,
        }
    }

    pub fn property_key(&self) -> &str {
        match self {
            Filter::Text { property_key, .. } => property_key,
            Filter::Number { property_key, .. } => property_key,
            Filter::Select { property_key, .. } => property_key,
            Filter::MultiSelect { property_key, .. } => property_key,
            Filter::Date { property_key, .. } => property_key,
            Filter::Checkbox { property_key, .. } => property_key,
            Filter::Files { property_key, .. } => property_key,
            Filter::Url { property_key, .. } => property_key,
            Filter::Email { property_key, .. } => property_key,
            Filter::Phone { property_key, .. } => property_key,
            Filter::Objects { property_key, .. } => property_key,
            Filter::Empty { property_key, .. } => property_key,
            Filter::NotEmpty { property_key, .. } => property_key,
            Filter::Value { property_key, .. } => property_key,
        }
    }

    fn condition_expr(&self) -> String {
        format!("{}[{}]", self.property_key(), self.condition())
    }

    fn query_key(&self) -> String {
        if self.condition().is_equal() || self.condition().is_none() {
            self.property_key().to_owned()
        } else {
            self.condition_expr()
        }
    }

    pub(crate) fn to_query(&self) -> (String, String) {
        match self {
            Filter::Text { text, .. } => (self.query_key(), text.to_owned()),
            Filter::Number { number, .. } => (self.query_key(), number.to_string()),
            Filter::Select { select, .. } => (self.query_key(), select.join(",")),
            Filter::MultiSelect { multi_select, .. } => (self.query_key(), multi_select.join(",")),
            Filter::Date { date, .. } => (self.query_key(), date.to_owned()),
            Filter::Checkbox { checkbox, .. } => (self.query_key(), checkbox.to_string()),
            Filter::Files { files, .. } => (self.query_key(), files.join(",")),
            Filter::Url { url, .. } => (self.query_key(), url.to_owned()),
            Filter::Email { email, .. } => (self.query_key(), email.to_owned()),
            Filter::Phone { phone, .. } => (self.query_key(), phone.to_owned()),
            Filter::Objects { objects, .. } => (self.query_key(), objects.join(",")),
            Filter::Empty { .. } => (self.query_key(), "".to_string()),
            Filter::NotEmpty { .. } => (self.query_key(), "".to_string()),
            Filter::Value { value, .. } => {
                let val_str = match value {
                    Some(Value::Array(vec)) => vec
                        .iter()
                        .map(|v| v.to_string())
                        .collect::<Vec<String>>()
                        .join(","),
                    None | Some(Value::Null) => String::new(),
                    Some(v) => v.to_string(),
                };
                (self.query_key(), val_str)
            }
        }
    }

    pub(crate) fn to_query_params(&self) -> Vec<(String, String)> {
        match self {
            Filter::Objects { objects, .. } => {
                let key = self.query_key();
                objects
                    .iter()
                    .map(|value| (key.clone(), value.clone()))
                    .collect()
            }
            _ => vec![self.to_query()],
        }
    }
}
