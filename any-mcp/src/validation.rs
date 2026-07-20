//! Bounded validation primitives shared by MCP workflows.

use crate::error::ToolError;
use rmcp::schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Deserializer, Serialize, de};
use sha2::{Digest, Sha256};
use std::{borrow::Cow, fmt};

/// Maximum IDs in one request.
pub const MAX_IDS: usize = 100;
/// Maximum projected property keys.
pub const MAX_PROJECTIONS: usize = 50;
/// Maximum filters per query.
pub const MAX_FILTERS: usize = 50;
/// Maximum aggregate filter values.
pub const MAX_FILTER_VALUES: usize = 100;
/// Maximum filter-tree depth.
pub const MAX_FILTER_DEPTH: usize = 4;
/// Default body chunk size in characters.
pub const DEFAULT_BODY_CHARS: u32 = 20_000;
/// Maximum body chunk size in characters.
pub const MAX_BODY_CHARS: u32 = 100_000;
/// Practical maximum complete body size for indexed chunking.
pub const MAX_BODY_TOTAL_CHARS: u32 = 100_000_000;

/// Stable validation failure categories.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValidationCode {
    /// Invalid page limit.
    InvalidLimit,
    /// Bounded list overflow.
    TooManyItems,
    /// Filter count overflow.
    FilterCount,
    /// Filter value overflow.
    FilterValues,
    /// Filter nesting overflow.
    FilterDepth,
    /// Invalid body chunk limit.
    BodyLimit,
    /// Body offset outside the text.
    BodyOffset,
    /// Complete body too large.
    BodyTooLarge,
    /// Cursor syntax is malformed.
    MalformedCursor,
    /// Cursor version is unsupported.
    CursorVersion,
    /// Cursor state is missing.
    UnknownCursor,
    /// Cursor belongs to another process.
    ExpiredCursor,
    /// Cursor query binding differs.
    CursorMismatch,
    /// Canonical query is too large.
    QueryTooLarge,
}

/// Secret-free validation error with fixed corrective text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ValidationError {
    code: ValidationCode,
    message: &'static str,
}
impl ValidationError {
    pub(crate) const fn new(code: ValidationCode, message: &'static str) -> Self {
        Self { code, message }
    }
    /// Returns the stable validation code.
    #[must_use]
    pub const fn code(&self) -> ValidationCode {
        self.code
    }
    /// Returns fixed corrective text.
    #[must_use]
    pub const fn message(&self) -> &'static str {
        self.message
    }
    /// Converts to a secret-safe MCP validation error.
    #[must_use]
    pub const fn tool_error(&self) -> ToolError {
        ToolError::validation_message(self.message)
    }
}
impl fmt::Display for ValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.message)
    }
}
impl std::error::Error for ValidationError {}

pub(crate) const fn error(code: ValidationCode) -> ValidationError {
    let message = match code {
        ValidationCode::InvalidLimit => "Limit must be between 1 and 100; omit it to use 20.",
        ValidationCode::TooManyItems => "The supplied list exceeds its documented maximum.",
        ValidationCode::FilterCount => "At most 50 filters are allowed.",
        ValidationCode::FilterValues => "At most 100 filter values are allowed.",
        ValidationCode::FilterDepth => "Filter nesting may not exceed 4 levels.",
        ValidationCode::BodyLimit => {
            "max_chars must be between 1 and 100000; omit it to use 20000."
        }
        ValidationCode::BodyOffset => "Body offset is outside the current document.",
        ValidationCode::BodyTooLarge => "The document exceeds the supported character count.",
        ValidationCode::MalformedCursor => "Cursor is malformed. Restart without a cursor.",
        ValidationCode::CursorVersion => "Cursor version is unsupported. Restart without a cursor.",
        ValidationCode::UnknownCursor => {
            "Cursor is unknown or no longer available. Restart without a cursor."
        }
        ValidationCode::ExpiredCursor => {
            "Cursor expired with the server process. Restart without a cursor."
        }
        ValidationCode::CursorMismatch => {
            "Cursor does not match the current parameters. Restart without a cursor."
        }
        ValidationCode::QueryTooLarge => "Normalized query exceeds 65536 bytes.",
    };
    ValidationError::new(code, message)
}

/// Runtime- and schema-bounded list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct BoundedList<T, const MAX: usize>(Vec<T>);
impl<T, const MAX: usize> BoundedList<T, MAX> {
    /// Validates and constructs the list.
    pub fn new(values: Vec<T>) -> Result<Self, ValidationError> {
        if values.len() > MAX {
            Err(error(ValidationCode::TooManyItems))
        } else {
            Ok(Self(values))
        }
    }
    /// Borrows validated elements.
    #[must_use]
    pub fn as_slice(&self) -> &[T] {
        &self.0
    }
}
impl<'de, T: Deserialize<'de>, const MAX: usize> Deserialize<'de> for BoundedList<T, MAX> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Self::new(Vec::deserialize(d)?).map_err(de::Error::custom)
    }
}
impl<T: JsonSchema, const MAX: usize> JsonSchema for BoundedList<T, MAX> {
    fn schema_name() -> Cow<'static, str> {
        Cow::Owned(format!("BoundedList{MAX}Of{}", T::schema_name()))
    }
    fn json_schema(g: &mut SchemaGenerator) -> Schema {
        json_schema!({"type":"array","items":g.subschema_for::<T>(),"maxItems":MAX})
    }
}
/// Bounded identifier list.
pub type IdList<T> = BoundedList<T, MAX_IDS>;
/// Bounded property projection.
pub type ProjectionList<T> = BoundedList<T, MAX_PROJECTIONS>;
/// Bounded filter list.
pub type FilterList<T> = BoundedList<T, MAX_FILTERS>;
/// Bounded filter-value list.
pub type FilterValueList<T> = BoundedList<T, MAX_FILTER_VALUES>;

/// Aggregate runtime budget for nested filters.
#[derive(Debug, Default)]
pub struct FilterBudget {
    filters: usize,
    values: usize,
}
impl FilterBudget {
    /// Records one filter at `depth` with its value count.
    pub fn record(&mut self, depth: usize, values: usize) -> Result<(), ValidationError> {
        if depth == 0 || depth > MAX_FILTER_DEPTH {
            return Err(error(ValidationCode::FilterDepth));
        }
        if self.filters + 1 > MAX_FILTERS {
            return Err(error(ValidationCode::FilterCount));
        }
        if self.values + values > MAX_FILTER_VALUES {
            return Err(error(ValidationCode::FilterValues));
        }
        self.filters += 1;
        self.values += values;
        Ok(())
    }
}

/// Validated body-chunk character limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct BodyCharLimit(u32);
impl Default for BodyCharLimit {
    fn default() -> Self {
        Self(DEFAULT_BODY_CHARS)
    }
}
impl BodyCharLimit {
    /// Validates a chunk character limit.
    pub fn new(v: u32) -> Result<Self, ValidationError> {
        if (1..=MAX_BODY_CHARS).contains(&v) {
            Ok(Self(v))
        } else {
            Err(error(ValidationCode::BodyLimit))
        }
    }
    /// Returns the validated character limit.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }
}
impl<'de> Deserialize<'de> for BodyCharLimit {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Self::new(u32::deserialize(d)?).map_err(de::Error::custom)
    }
}
impl JsonSchema for BodyCharLimit {
    fn schema_name() -> Cow<'static, str> {
        "BodyCharLimit".into()
    }
    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({"type":"integer","minimum":1,"maximum":MAX_BODY_CHARS})
    }
}

/// Validated character offset within the practical body-size bound.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct BodyOffset(u32);

impl BodyOffset {
    /// Validates a character offset.
    pub fn new(value: u32) -> Result<Self, ValidationError> {
        if value <= MAX_BODY_TOTAL_CHARS {
            Ok(Self(value))
        } else {
            Err(error(ValidationCode::BodyOffset))
        }
    }

    /// Returns the validated character offset.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }
}

impl<'de> Deserialize<'de> for BodyOffset {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::new(u32::deserialize(deserializer)?).map_err(de::Error::custom)
    }
}

impl JsonSchema for BodyOffset {
    fn schema_name() -> Cow<'static, str> {
        "BodyOffset".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({"type":"integer","minimum":0,"maximum":MAX_BODY_TOTAL_CHARS})
    }
}

/// Shared input for character-indexed document body reads.
#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BodyChunkInput {
    /// Starting character offset, defaulting to zero.
    #[serde(default)]
    pub offset: BodyOffset,
    /// Maximum returned characters, defaulting to 20,000.
    #[serde(default)]
    pub max_chars: BodyCharLimit,
}

/// Character-indexed body chunk.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BodyChunk {
    /// UTF-8 text sliced on character boundaries.
    #[schemars(length(max=MAX_BODY_CHARS))]
    pub text: String,
    /// Starting character offset.
    pub offset: BodyOffset,
    /// Next character offset, absent at the end.
    pub next_offset: Option<BodyOffset>,
    /// Complete body character count.
    pub total_chars: BodyOffset,
    /// SHA-256 of the complete current body.
    #[schemars(length(equal = 64))]
    pub sha256: String,
}

/// Slices a body on Unicode character boundaries and hashes the full text.
pub fn chunk_body(
    text: &str,
    offset: BodyOffset,
    limit: BodyCharLimit,
) -> Result<BodyChunk, ValidationError> {
    let total =
        u32::try_from(text.chars().count()).map_err(|_| error(ValidationCode::BodyTooLarge))?;
    if total > MAX_BODY_TOTAL_CHARS {
        return Err(error(ValidationCode::BodyTooLarge));
    }
    if offset.get() > total {
        return Err(error(ValidationCode::BodyOffset));
    }
    let end = offset.get().saturating_add(limit.get()).min(total);
    let byte_at = |n: u32| {
        if n == total {
            text.len()
        } else {
            text.char_indices()
                .nth(n as usize)
                .map_or(text.len(), |(i, _)| i)
        }
    };
    let chunk = text[byte_at(offset.get())..byte_at(end)].to_owned();
    let sha256: String = Sha256::digest(text.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    Ok(BodyChunk {
        text: chunk,
        offset,
        next_offset: (end < total).then(|| BodyOffset::new(end).expect("end is bounded by total")),
        total_chars: BodyOffset::new(total).expect("body size was validated"),
        sha256,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{input_schema, output_schema};

    #[test]
    fn every_list_and_filter_cap_is_enforced() {
        assert!(BoundedList::<u8, 2>::new(vec![1, 2]).is_ok());
        assert!(BoundedList::<u8, 2>::new(vec![1, 2, 3]).is_err());
        assert!(IdList::new(vec![0_u8; MAX_IDS]).is_ok());
        assert!(IdList::new(vec![0_u8; MAX_IDS + 1]).is_err());
        assert!(ProjectionList::new(vec![0_u8; MAX_PROJECTIONS]).is_ok());
        assert!(ProjectionList::new(vec![0_u8; MAX_PROJECTIONS + 1]).is_err());
        assert!(FilterList::new(vec![0_u8; MAX_FILTERS]).is_ok());
        assert!(FilterList::new(vec![0_u8; MAX_FILTERS + 1]).is_err());
        assert!(FilterValueList::new(vec![0_u8; MAX_FILTER_VALUES]).is_ok());
        assert!(FilterValueList::new(vec![0_u8; MAX_FILTER_VALUES + 1]).is_err());
        let mut b = FilterBudget::default();
        for _ in 0..MAX_FILTERS {
            b.record(1, 2).unwrap();
        }
        assert_eq!(
            b.record(1, 0).unwrap_err().code(),
            ValidationCode::FilterCount
        );
        assert_eq!(
            FilterBudget::default()
                .record(MAX_FILTER_DEPTH + 1, 0)
                .unwrap_err()
                .code(),
            ValidationCode::FilterDepth
        );
        let mut b = FilterBudget::default();
        assert_eq!(
            b.record(1, MAX_FILTER_VALUES + 1).unwrap_err().code(),
            ValidationCode::FilterValues
        );
    }
    #[test]
    fn body_limits_and_utf8_boundaries() {
        assert_eq!(BodyCharLimit::default().get(), DEFAULT_BODY_CHARS);
        assert!(BodyCharLimit::new(0).is_err());
        assert!(BodyCharLimit::new(MAX_BODY_CHARS).is_ok());
        assert!(BodyCharLimit::new(MAX_BODY_CHARS + 1).is_err());
        let c = chunk_body(
            "aé🦀z",
            BodyOffset::new(1).unwrap(),
            BodyCharLimit::new(2).unwrap(),
        )
        .unwrap();
        assert_eq!(c.text, "é🦀");
        assert_eq!(c.next_offset, Some(BodyOffset::new(3).unwrap()));
        assert_eq!(c.sha256.len(), 64);
        assert_eq!(
            chunk_body("abc", BodyOffset::new(4).unwrap(), BodyCharLimit::default())
                .unwrap_err()
                .code(),
            ValidationCode::BodyOffset
        );
    }
    #[test]
    fn errors_are_fixed_and_secret_safe() {
        let e = error(ValidationCode::ExpiredCursor);
        assert_eq!(
            e.message(),
            "Cursor expired with the server process. Restart without a cursor."
        );
        assert!(!e.tool_error().message().contains("token"));
    }

    #[test]
    fn body_chunk_wire_contract_is_strict() {
        assert!(input_schema::<BodyChunkInput>().is_ok());
        assert!(output_schema::<BodyChunk>().is_ok());
    }
}
