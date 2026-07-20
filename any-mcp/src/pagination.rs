//! Bounded pagination inputs and result pages.
use crate::{
    cursor::CursorToken,
    validation::{ValidationCode, ValidationError, error},
};
use rmcp::schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Deserializer, Serialize, de};
use std::borrow::Cow;
/// Default number of items requested per page.
pub const DEFAULT_PAGE_LIMIT: u16 = 20;
/// Maximum number of items returned per page.
pub const MAX_PAGE_LIMIT: u16 = 100;
/// Practical maximum upstream pagination offset.
pub const MAX_PAGE_OFFSET: u32 = 1_000_000_000;
/// Validated page limit between one and [`MAX_PAGE_LIMIT`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct PageLimit(u16);
impl Default for PageLimit {
    fn default() -> Self {
        Self(DEFAULT_PAGE_LIMIT)
    }
}
impl PageLimit {
    /// Validates a caller-supplied page limit.
    pub fn new(v: u16) -> Result<Self, ValidationError> {
        if (1..=MAX_PAGE_LIMIT).contains(&v) {
            Ok(Self(v))
        } else {
            Err(error(ValidationCode::InvalidLimit))
        }
    }
    /// Returns the validated limit.
    #[must_use]
    pub const fn get(self) -> u16 {
        self.0
    }
}
impl<'de> Deserialize<'de> for PageLimit {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Self::new(u16::deserialize(d)?).map_err(de::Error::custom)
    }
}
impl JsonSchema for PageLimit {
    fn schema_name() -> Cow<'static, str> {
        "PageLimit".into()
    }
    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({"type":"integer","minimum":1,"maximum":MAX_PAGE_LIMIT})
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(transparent)]
/// Validated upstream pagination offset.
pub struct PageOffset(u32);
impl PageOffset {
    /// Validates an upstream offset.
    pub fn new(v: u32) -> Result<Self, ValidationError> {
        if v <= MAX_PAGE_OFFSET {
            Ok(Self(v))
        } else {
            Err(error(ValidationCode::TooManyItems))
        }
    }
    /// Returns the validated offset.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }
}
impl JsonSchema for PageOffset {
    fn schema_name() -> Cow<'static, str> {
        "PageOffset".into()
    }
    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({"type":"integer","minimum":0,"maximum":MAX_PAGE_OFFSET})
    }
}
#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
/// Shared pagination input embedded by list and search tools.
pub struct PaginationInput {
    /// Requested item limit, defaulting to 20.
    #[serde(default)]
    pub limit: PageLimit,
    /// Opaque continuation cursor, when continuing a page.
    #[serde(default)]
    pub cursor: Option<CursorToken>,
}
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
/// One bounded result page.
pub struct Page<T: JsonSchema> {
    /// Items in this page, never more than 100.
    #[schemars(length(max=MAX_PAGE_LIMIT))]
    items: Vec<T>,
    /// Opaque continuation cursor, absent at the end.
    next_cursor: Option<CursorToken>,
}
impl<T: JsonSchema> Page<T> {
    /// Validates and constructs a bounded page.
    pub fn new(items: Vec<T>, next_cursor: Option<CursorToken>) -> Result<Self, ValidationError> {
        if items.len() > MAX_PAGE_LIMIT as usize {
            Err(error(ValidationCode::TooManyItems))
        } else {
            Ok(Self { items, next_cursor })
        }
    }
    /// Borrows page items.
    #[must_use]
    pub fn items(&self) -> &[T] {
        &self.items
    }
    /// Borrows the continuation cursor.
    #[must_use]
    pub fn next_cursor(&self) -> Option<&CursorToken> {
        self.next_cursor.as_ref()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{input_schema, output_schema};

    #[test]
    fn defaults_zero_max_and_overmax() {
        assert_eq!(PageLimit::default().get(), 20);
        assert!(PageLimit::new(0).is_err());
        assert_eq!(PageLimit::new(100).unwrap().get(), 100);
        assert!(PageLimit::new(101).is_err());
        assert!(Page::<bool>::new(vec![true; 101], None).is_err());
    }

    #[test]
    fn pagination_wire_contracts_are_strict() {
        assert!(input_schema::<PaginationInput>().is_ok());
        assert!(output_schema::<Page<bool>>().is_ok());
    }
}
