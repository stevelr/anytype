//! Opaque process-lifetime pagination cursors bound to normalized queries.
use crate::{
    pagination::PageOffset,
    validation::{ValidationCode, ValidationError, error},
};
use anytype::chats::MessageBeforeAnchor;
use rmcp::schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Deserializer, Serialize, de};
use sha2::{Digest, Sha256};
use std::{
    borrow::Cow,
    collections::{HashMap, VecDeque},
    fmt,
    sync::Mutex,
};
/// Maximum encoded cursor length.
pub const MAX_CURSOR_CHARS: usize = 64;
/// Minimum encoded cursor length for the one-digit initial version.
pub const MIN_CURSOR_CHARS: usize = 52;
/// Maximum live cursor states retained by one process.
pub const MAX_CURSOR_ENTRIES: usize = 4096;
/// Maximum canonical query size accepted for hashing.
pub const MAX_NORMALIZED_QUERY_BYTES: usize = 65_536;
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
/// Bounded opaque cursor token carried over MCP.
pub struct CursorToken(String);
impl CursorToken {
    /// Constructs a bounded token matching the documented cursor grammar.
    pub fn new(v: impl Into<String>) -> Result<Self, ValidationError> {
        let v = v.into();
        cursor_parts(&v)?;
        Ok(Self(v))
    }
    /// Borrows the encoded token.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl<'de> Deserialize<'de> for CursorToken {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Self::new(String::deserialize(d)?).map_err(de::Error::custom)
    }
}
impl JsonSchema for CursorToken {
    fn schema_name() -> Cow<'static, str> {
        "CursorToken".into()
    }
    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({"type":"string","minLength":MIN_CURSOR_CHARS,"maxLength":MAX_CURSOR_CHARS,"pattern":"^c[0-9]+\\.[0-9a-f]{16}\\.[0-9a-f]{32}$"})
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// SHA-256 binding for canonical normalized query parameters.
pub struct QueryFingerprint([u8; 32]);
impl QueryFingerprint {
    /// Canonicalizes and hashes bounded serializable query parameters.
    pub fn from_normalized<T: Serialize>(query: &T) -> Result<Self, ValidationError> {
        let mut value =
            serde_json::to_value(query).map_err(|_| error(ValidationCode::QueryTooLarge))?;
        sort_json(&mut value);
        let bytes = serde_json::to_vec(&value).map_err(|_| error(ValidationCode::QueryTooLarge))?;
        if bytes.len() > MAX_NORMALIZED_QUERY_BYTES {
            return Err(error(ValidationCode::QueryTooLarge));
        }
        Ok(Self(Sha256::digest(bytes).into()))
    }
}
fn sort_json(v: &mut serde_json::Value) {
    match v {
        serde_json::Value::Object(m) => {
            let old = std::mem::take(m);
            let mut pairs: Vec<_> = old.into_iter().collect();
            pairs.sort_by(|a, b| a.0.cmp(&b.0));
            for (k, mut v) in pairs {
                sort_json(&mut v);
                m.insert(k, v);
            }
        }
        serde_json::Value::Array(a) => a.iter_mut().for_each(sort_json),
        _ => {}
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EvidenceCursorState {
    offset: PageOffset,
    total: u64,
    boundary_id: String,
}

impl EvidenceCursorState {
    pub(crate) fn new(offset: PageOffset, total: u64, boundary_id: String) -> Self {
        Self {
            offset,
            total,
            boundary_id,
        }
    }

    pub(crate) const fn offset(&self) -> PageOffset {
        self.offset
    }

    pub(crate) const fn total(&self) -> u64 {
        self.total
    }

    pub(crate) fn boundary_id(&self) -> &str {
        &self.boundary_id
    }
}

#[derive(Clone)]
enum CursorState {
    Offset(PageOffset),
    Evidence(EvidenceCursorState),
    MessageHistory { anchor: String, page: u8 },
}

#[derive(Clone)]
struct State {
    value: CursorState,
    query: QueryFingerprint,
}

impl fmt::Debug for State {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.value {
            CursorState::Offset(offset) => formatter
                .debug_struct("Offset")
                .field("offset", offset)
                .finish_non_exhaustive(),
            CursorState::Evidence(evidence) => formatter
                .debug_struct("Evidence")
                .field("offset", &evidence.offset())
                .field("total", &evidence.total())
                .field("boundary_id", &"[redacted]")
                .finish(),
            CursorState::MessageHistory { page, .. } => formatter
                .debug_struct("MessageHistory")
                .field("anchor", &"[redacted]")
                .field("page", page)
                .finish_non_exhaustive(),
        }
    }
}

/// Private state resolved from one message-history cursor.
///
/// Its debug representation deliberately redacts the upstream anchor.
#[derive(Clone)]
pub(crate) struct MessageHistoryCursorState {
    anchor: String,
    page: u8,
}

impl fmt::Debug for MessageHistoryCursorState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MessageHistoryCursorState")
            .field("anchor", &"[redacted]")
            .field("page", &self.page)
            .finish()
    }
}

impl MessageHistoryCursorState {
    /// Consumes the state without exposing the anchor through formatting.
    #[must_use]
    pub(crate) fn into_parts(self) -> (String, u8) {
        (self.anchor, self.page)
    }
}
#[derive(Default)]
struct Registry {
    entries: HashMap<[u8; 16], State>,
    order: VecDeque<[u8; 16]>,
}
/// Failure to obtain operating-system entropy for cursor identifiers.
#[derive(Debug, Clone, Copy)]
pub struct CursorStoreError;
impl fmt::Display for CursorStoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("cursor entropy unavailable")
    }
}
impl std::error::Error for CursorStoreError {}
/// Capped in-memory cursor registry. Dropping it expires all issued cursors.
pub struct CursorStore {
    instance: [u8; 8],
    registry: Mutex<Registry>,
}
impl CursorStore {
    /// Creates a new process-scoped cursor registry.
    pub fn new() -> Result<Self, CursorStoreError> {
        let mut instance = [0; 8];
        getrandom::fill(&mut instance).map_err(|_| CursorStoreError)?;
        Ok(Self {
            instance,
            registry: Mutex::new(Registry::default()),
        })
    }
    /// Issues a versioned cursor for an offset and normalized query.
    pub fn issue(
        &self,
        offset: PageOffset,
        query: QueryFingerprint,
    ) -> Result<CursorToken, CursorStoreError> {
        self.issue_state(CursorState::Offset(offset), query)
    }

    pub(crate) fn issue_evidence(
        &self,
        state: EvidenceCursorState,
        query: QueryFingerprint,
    ) -> Result<CursorToken, CursorStoreError> {
        self.issue_state(CursorState::Evidence(state), query)
    }

    fn issue_state(
        &self,
        value: CursorState,
        query: QueryFingerprint,
    ) -> Result<CursorToken, CursorStoreError> {
        let mut id = [0; 16];
        getrandom::fill(&mut id).map_err(|_| CursorStoreError)?;
        let mut r = self
            .registry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        while r.entries.contains_key(&id) {
            getrandom::fill(&mut id).map_err(|_| CursorStoreError)?;
        }
        if r.entries.len() >= MAX_CURSOR_ENTRIES
            && let Some(old) = r.order.pop_front()
        {
            r.entries.remove(&old);
        }
        r.entries.insert(id, State { value, query });
        r.order.push_back(id);
        CursorToken::new(format!("c1.{}.{}", hex(&self.instance), hex(&id)))
            .map_err(|_| CursorStoreError)
    }
    /// Resolves a cursor after checking process and query binding.
    pub fn resolve(
        &self,
        cursor: &CursorToken,
        query: QueryFingerprint,
    ) -> Result<PageOffset, ValidationError> {
        let (instance, id) = parse(cursor.as_str())?;
        if instance != self.instance {
            return Err(error(ValidationCode::ExpiredCursor));
        }
        let r = self
            .registry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let state = r
            .entries
            .get(&id)
            .ok_or_else(|| error(ValidationCode::UnknownCursor))?;
        if state.query != query {
            return Err(error(ValidationCode::CursorMismatch));
        }
        match &state.value {
            CursorState::Offset(offset) => Ok(*offset),
            CursorState::Evidence(_) | CursorState::MessageHistory { .. } => {
                Err(error(ValidationCode::CursorMismatch))
            }
        }
    }

    pub(crate) fn resolve_evidence(
        &self,
        cursor: &CursorToken,
        query: QueryFingerprint,
    ) -> Result<EvidenceCursorState, ValidationError> {
        let (instance, id) = parse(cursor.as_str())?;
        if instance != self.instance {
            return Err(error(ValidationCode::ExpiredCursor));
        }
        let r = self
            .registry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let state = r
            .entries
            .get(&id)
            .ok_or_else(|| error(ValidationCode::UnknownCursor))?;
        if state.query != query {
            return Err(error(ValidationCode::CursorMismatch));
        }
        match &state.value {
            CursorState::Evidence(state) => Ok(state.clone()),
            CursorState::Offset(_) | CursorState::MessageHistory { .. } => {
                Err(error(ValidationCode::CursorMismatch))
            }
        }
    }

    /// Issues an opaque cursor that privately owns one chat history anchor.
    pub(crate) fn issue_message_history(
        &self,
        anchor: MessageBeforeAnchor,
        page: u8,
        query: QueryFingerprint,
    ) -> Result<CursorToken, CursorStoreError> {
        self.issue_state(
            CursorState::MessageHistory {
                anchor: String::from(anchor),
                page,
            },
            query,
        )
    }

    /// Resolves a message-history cursor after process and query binding.
    pub(crate) fn resolve_message_history(
        &self,
        cursor: &CursorToken,
        query: QueryFingerprint,
    ) -> Result<MessageHistoryCursorState, ValidationError> {
        let (instance, id) = parse(cursor.as_str())?;
        if instance != self.instance {
            return Err(error(ValidationCode::ExpiredCursor));
        }
        let r = self
            .registry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match r.entries.get(&id) {
            Some(State {
                value: CursorState::MessageHistory { anchor, page },
                query: stored,
            }) if *stored == query => Ok(MessageHistoryCursorState {
                anchor: anchor.clone(),
                page: *page,
            }),
            Some(_) => Err(error(ValidationCode::CursorMismatch)),
            None => Err(error(ValidationCode::UnknownCursor)),
        }
    }

    #[cfg(test)]
    pub(crate) fn entry_count(&self) -> usize {
        self.registry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entries
            .len()
    }
}
fn parse(v: &str) -> Result<([u8; 8], [u8; 16]), ValidationError> {
    let (version, instance, id) = cursor_parts(v)?;
    if version != "1" {
        return Err(error(ValidationCode::CursorVersion));
    }
    Ok((unhex::<8>(instance)?, unhex::<16>(id)?))
}
fn cursor_parts(v: &str) -> Result<(&str, &str, &str), ValidationError> {
    if !(MIN_CURSOR_CHARS..=MAX_CURSOR_CHARS).contains(&v.len()) || !v.is_ascii() {
        return Err(error(ValidationCode::MalformedCursor));
    }
    let Some((version, remainder)) = v.strip_prefix('c').and_then(|v| v.split_once('.')) else {
        return Err(error(ValidationCode::MalformedCursor));
    };
    let Some((instance, id)) = remainder.split_once('.') else {
        return Err(error(ValidationCode::MalformedCursor));
    };
    let valid_version = !version.is_empty() && version.bytes().all(|byte| byte.is_ascii_digit());
    if !valid_version
        || instance.len() != 16
        || id.len() != 32
        || !instance.bytes().all(is_lower_hex)
        || !id.bytes().all(is_lower_hex)
    {
        return Err(error(ValidationCode::MalformedCursor));
    }
    Ok((version, instance, id))
}
fn hex(v: &[u8]) -> String {
    v.iter().map(|b| format!("{b:02x}")).collect()
}
fn unhex<const N: usize>(v: &str) -> Result<[u8; N], ValidationError> {
    if N.checked_mul(2) != Some(v.len()) {
        return Err(error(ValidationCode::MalformedCursor));
    }
    let mut out = [0; N];
    for (target, pair) in out.iter_mut().zip(v.as_bytes().as_chunks::<2>().0) {
        let [high, low] = *pair;
        *target = (hex_nibble(high)? << 4) | hex_nibble(low)?;
    }
    Ok(out)
}
fn is_lower_hex(byte: u8) -> bool {
    byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)
}
fn hex_nibble(byte: u8) -> Result<u8, ValidationError> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        _ => Err(error(ValidationCode::MalformedCursor)),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn cursor_binding_tampering_and_process_expiry() {
        let a = CursorStore::new().unwrap();
        let q = QueryFingerprint::from_normalized(&json!({"text":"x","limit":20})).unwrap();
        let token = a.issue(PageOffset::new(20).unwrap(), q).unwrap();
        assert_eq!(a.resolve(&token, q).unwrap().get(), 20);
        let q2 = QueryFingerprint::from_normalized(&json!({"text":"y","limit":20})).unwrap();
        assert_eq!(
            a.resolve(&token, q2).unwrap_err().code(),
            ValidationCode::CursorMismatch
        );
        let b = CursorStore::new().unwrap();
        assert_eq!(
            b.resolve(&token, q).unwrap_err().code(),
            ValidationCode::ExpiredCursor
        );
        let mut t = token.as_str().to_owned();
        t.replace_range(t.len() - 1.., if t.ends_with('0') { "1" } else { "0" });
        assert_eq!(
            a.resolve(&CursorToken::new(t).unwrap(), q)
                .unwrap_err()
                .code(),
            ValidationCode::UnknownCursor
        );
    }
    #[test]
    fn malformed_unknown_version_and_canonical_query() {
        let s = CursorStore::new().unwrap();
        let q1 = QueryFingerprint::from_normalized(&json!({"a":1,"b":2})).unwrap();
        let q2 = QueryFingerprint::from_normalized(&json!({"b":2,"a":1})).unwrap();
        assert_eq!(q1, q2);
        assert_eq!(
            CursorToken::new("bad").unwrap_err().code(),
            ValidationCode::MalformedCursor
        );
        let valid = s.issue(PageOffset::new(1).unwrap(), q1).unwrap();
        let v = CursorToken::new(valid.as_str().replacen("c1", "c2", 1)).unwrap();
        assert_eq!(
            s.resolve(&v, q1).unwrap_err().code(),
            ValidationCode::CursorVersion
        );
        assert_eq!(
            QueryFingerprint::from_normalized(&"x".repeat(MAX_NORMALIZED_QUERY_BYTES + 1))
                .unwrap_err()
                .code(),
            ValidationCode::QueryTooLarge
        );
    }

    #[test]
    fn cursor_registry_evicts_oldest_state_at_its_cap() {
        let s = CursorStore::new().unwrap();
        let q = QueryFingerprint::from_normalized(&serde_json::Value::Null).unwrap();
        let first = s.issue(PageOffset::new(0).unwrap(), q).unwrap();
        for offset in 1..=MAX_CURSOR_ENTRIES {
            s.issue(PageOffset::new(offset as u32).unwrap(), q).unwrap();
        }
        assert_eq!(
            s.resolve(&first, q).unwrap_err().code(),
            ValidationCode::UnknownCursor
        );
    }

    #[test]
    fn unicode_uppercase_and_invalid_boundaries_are_malformed_without_panics() {
        let malformed = [
            "c1.0000000é0000000.00000000000000000000000000000000",
            "c1.0000000000000000.000000000000000é000000000000000",
            "c1.0000000000000000.0000000000000000000000000000000é",
            "c1.000000000000000A.00000000000000000000000000000000",
            "c1.0000000000000000.0000000000000000000000000000000F",
            "c١.0000000000000000.00000000000000000000000000000000",
        ];
        for value in malformed {
            assert_eq!(
                CursorToken::new(value).unwrap_err().code(),
                ValidationCode::MalformedCursor
            );
            assert_eq!(
                parse(value).unwrap_err().code(),
                ValidationCode::MalformedCursor
            );
        }
        assert_eq!(
            unhex::<8>("0000000é0000000").unwrap_err().code(),
            ValidationCode::MalformedCursor
        );
    }
}
