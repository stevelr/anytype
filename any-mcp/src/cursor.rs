//! Opaque process-lifetime pagination cursors bound to normalized queries.
use crate::{
    pagination::PageOffset,
    validation::{ValidationCode, ValidationError, error},
};
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
/// Maximum live cursor states retained by one process.
pub const MAX_CURSOR_ENTRIES: usize = 4096;
/// Maximum canonical query size accepted for hashing.
pub const MAX_NORMALIZED_QUERY_BYTES: usize = 65_536;
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
/// Bounded opaque cursor token carried over MCP.
pub struct CursorToken(String);
impl CursorToken {
    /// Constructs a bounded token; store resolution validates its syntax.
    pub fn new(v: impl Into<String>) -> Result<Self, ValidationError> {
        let v = v.into();
        if v.is_empty() || v.len() > MAX_CURSOR_CHARS {
            Err(error(ValidationCode::MalformedCursor))
        } else {
            Ok(Self(v))
        }
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
        json_schema!({"type":"string","minLength":1,"maxLength":MAX_CURSOR_CHARS,"pattern":"^c[0-9]+\\.[0-9a-f]{16}\\.[0-9a-f]{32}$"})
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
#[derive(Debug, Clone, Copy)]
struct State {
    offset: PageOffset,
    query: QueryFingerprint,
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
        let mut id = [0; 16];
        getrandom::fill(&mut id).map_err(|_| CursorStoreError)?;
        let mut r = self.registry.lock().expect("cursor registry poisoned");
        while r.entries.contains_key(&id) {
            getrandom::fill(&mut id).map_err(|_| CursorStoreError)?;
        }
        if r.entries.len() >= MAX_CURSOR_ENTRIES
            && let Some(old) = r.order.pop_front()
        {
            r.entries.remove(&old);
        }
        r.entries.insert(id, State { offset, query });
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
        let r = self.registry.lock().expect("cursor registry poisoned");
        let state = r
            .entries
            .get(&id)
            .ok_or_else(|| error(ValidationCode::UnknownCursor))?;
        if state.query != query {
            return Err(error(ValidationCode::CursorMismatch));
        }
        Ok(state.offset)
    }
}
fn parse(v: &str) -> Result<([u8; 8], [u8; 16]), ValidationError> {
    let p: Vec<_> = v.split('.').collect();
    if p.len() != 3 {
        return Err(error(ValidationCode::MalformedCursor));
    }
    if p[0] != "c1" {
        return Err(error(if p[0].starts_with('c') {
            ValidationCode::CursorVersion
        } else {
            ValidationCode::MalformedCursor
        }));
    }
    Ok((unhex::<8>(p[1])?, unhex::<16>(p[2])?))
}
fn hex(v: &[u8]) -> String {
    v.iter().map(|b| format!("{b:02x}")).collect()
}
fn unhex<const N: usize>(v: &str) -> Result<[u8; N], ValidationError> {
    if v.len() != N * 2 {
        return Err(error(ValidationCode::MalformedCursor));
    }
    let mut out = [0; N];
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(&v[i * 2..i * 2 + 2], 16)
            .map_err(|_| error(ValidationCode::MalformedCursor))?;
    }
    Ok(out)
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
            s.resolve(&CursorToken::new("bad").unwrap(), q1)
                .unwrap_err()
                .code(),
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
}
