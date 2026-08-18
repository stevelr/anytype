//! Secure storage for API keys and credentials
//!
//! Provides cross-platform storage backends for API keys:
//! - **Keyring**: OS-native secure credential stores (Keychain/Secret Service/Credential Manager)
//! - **File**: File-based storage in user config directory (less secure, for compatibility)
//!
//! All credentials for a service (HTTP token plus the gRPC account id, account
//! key, and session token) are stored together in **one** keystore entry
//! (`user = "credentials"`) as a small JSON document. Keeping them in a single
//! entry means an OS keyring asks the user for access once per application,
//! not once per credential. Stores written by earlier versions, which kept one
//! entry per credential, are read transparently and migrated to the single
//! entry on first access.

use std::path::Path;
use std::{collections::HashMap, fmt, sync::Arc};

use keyring_core::CredentialStore;
use serde::{Deserialize, Serialize};
use tracing::{debug, error, warn};
use zeroize::Zeroize;

use crate::error::KeyStoreError;

/// Keystore entry (`user`) holding the combined credential document.
const KEY_CREDENTIALS: &str = "credentials";
/// Current schema version of the combined credential document.
const CREDENTIALS_FORMAT_VERSION: u32 = 1;

// Legacy per-credential entries (pre-0.5). Read for migration only.
const KEY_HTTP_TOKEN: &str = "http_token";
const KEY_ACCOUNT_ID: &str = "account_id";
const KEY_ACCOUNT_KEY: &str = "account_key";
const KEY_SESSION_TOKEN: &str = "session_token";
const LEGACY_KEYS: [&str; 4] = [
    KEY_HTTP_TOKEN,
    KEY_ACCOUNT_ID,
    KEY_ACCOUNT_KEY,
    KEY_SESSION_TOKEN,
];

/// On-disk/in-keyring representation of every credential for one service.
///
/// Fields that are `None` are omitted from the JSON so the document only
/// carries what is configured.
#[derive(Clone, Default, Serialize, Deserialize)]
struct StoredCredentials {
    #[serde(default, rename = "v")]
    version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    http_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    account_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    account_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    session_token: Option<String>,
}

impl StoredCredentials {
    fn is_empty(&self) -> bool {
        self.http_token.is_none()
            && self.account_id.is_none()
            && self.account_key.is_none()
            && self.session_token.is_none()
    }

    fn http(&self) -> HttpCredentials {
        HttpCredentials {
            token: self.http_token.clone(),
        }
    }

    fn grpc(&self) -> GrpcCredentials {
        GrpcCredentials {
            account_id: self.account_id.clone(),
            account_key: self.account_key.clone(),
            session_token: self.session_token.clone(),
        }
    }

    fn parse(json: &str) -> Result<Self, KeyStoreError> {
        serde_json::from_str(json).map_err(|err| KeyStoreError::Config {
            message: format!("stored credentials entry is not valid: {err}"),
        })
    }

    fn to_json(&self) -> Result<String, KeyStoreError> {
        let mut document = self.clone();
        document.version = CREDENTIALS_FORMAT_VERSION;
        let json = serde_json::to_string(&document).map_err(|err| KeyStoreError::Config {
            message: format!("cannot serialize credentials: {err}"),
        });
        document.zeroize();
        json
    }
}

impl fmt::Debug for StoredCredentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StoredCredentials")
            .field("v", &self.version)
            .field(KEY_HTTP_TOKEN, &fmt_masked(self.http_token.as_ref()))
            .field(KEY_ACCOUNT_ID, &self.account_id)
            .field(KEY_ACCOUNT_KEY, &fmt_masked(self.account_key.as_ref()))
            .field(KEY_SESSION_TOKEN, &fmt_masked(self.session_token.as_ref()))
            .finish()
    }
}

impl Zeroize for StoredCredentials {
    fn zeroize(&mut self) {
        for field in [
            &mut self.http_token,
            &mut self.account_id,
            &mut self.account_key,
            &mut self.session_token,
        ] {
            if let Some(value) = field.as_mut() {
                value.zeroize();
            }
        }
    }
}

/// Type of keystore - builtin or external
#[derive(Clone, PartialEq, Eq)]
pub enum KeyStoreType {
    /// built-in file keystore: stores key in a clear text file
    File,
    /// OS-managed keyring, when supported (uses keyring crate)
    Keyring,
    /// No keystore. If this variant is used, keys are not persisted.
    None,
}

impl fmt::Display for KeyStoreType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::File => "file",
            Self::Keyring => "keyring",
            Self::None => "none",
        })
    }
}

impl fmt::Debug for KeyStoreType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_fmt(format_args!(
            "KeyStoreType({})",
            match self {
                Self::File => "File",
                Self::Keyring => "Keyring",
                Self::None => "None",
            }
        ))
    }
}

#[derive(Clone, Default)]
pub struct GrpcCredentials {
    account_id: Option<String>,
    account_key: Option<String>,
    session_token: Option<String>,
}

impl GrpcCredentials {
    pub fn new(
        account_id: Option<String>,
        account_key: Option<String>,
        session_token: Option<String>,
    ) -> Self {
        Self {
            account_id,
            account_key,
            session_token,
        }
    }

    pub fn account_id(&self) -> Option<&str> {
        self.account_id.as_deref()
    }

    pub fn account_key(&self) -> Option<&str> {
        self.account_key.as_deref()
    }

    pub fn session_token(&self) -> Option<&str> {
        self.session_token.as_deref()
    }
}

fn fmt_masked(val: Option<&String>) -> String {
    match val {
        Some(_) => "Some(MASKED)",
        None => "None",
    }
    .to_string()
}

impl fmt::Debug for GrpcCredentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GrpcCredentials")
            .field(KEY_ACCOUNT_ID, &self.account_id)
            .field(KEY_ACCOUNT_KEY, &fmt_masked(self.account_key.as_ref()))
            .field(KEY_SESSION_TOKEN, &fmt_masked(self.session_token.as_ref()))
            .finish()
    }
}

#[derive(Clone, Default)]
pub struct HttpCredentials {
    token: Option<String>,
}

impl fmt::Debug for HttpCredentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HttpCredentials")
            .field("token", &fmt_masked(self.token.as_ref()))
            .finish()
    }
}

impl HttpCredentials {
    pub fn new(token: impl Into<String>) -> Self {
        Self {
            token: Some(token.into()),
        }
    }

    pub fn has_creds(&self) -> bool {
        self.token.as_ref().is_some_and(|token| !token.is_empty())
    }

    pub(crate) fn token(&self) -> Option<&str> {
        self.token.as_deref()
    }
}

impl GrpcCredentials {
    /// Loads gRPC credentials from an Anytype CLI `config.json` file.
    ///
    /// Passing `None` selects the CLI's default `~/.anytype/config.json` path.
    /// A missing file returns `Ok(None)`; malformed or unreadable files return
    /// an error so callers can distinguish an uninitialized CLI from a broken
    /// configuration.
    pub fn from_cli_config(path: Option<&Path>) -> Result<Option<Self>, KeyStoreError> {
        use anytype_rpc::config::load_headless_config;

        let config = load_headless_config(path).map_err(|err| KeyStoreError::External {
            message: format!("failed to load headless config: {err}"),
        })?;
        Ok(config.map(|config| Self {
            account_id: config.account_id,
            account_key: config.account_key,
            session_token: config.session_token,
        }))
    }

    pub fn from_token(token: impl Into<String>) -> Self {
        Self {
            session_token: Some(token.into()),
            ..Default::default()
        }
    }

    pub fn from_account_key(account_key: impl Into<String>) -> Self {
        Self {
            account_key: Some(account_key.into()),
            ..Default::default()
        }
    }

    #[must_use]
    pub fn with_account_id(mut self, account_id: impl Into<String>) -> Self {
        self.account_id = Some(account_id.into());
        self
    }

    #[must_use]
    pub fn with_account_key(mut self, account_key: impl Into<String>) -> Self {
        self.account_key = Some(account_key.into());
        self
    }

    #[must_use]
    pub fn with_session_token(mut self, token: impl Into<String>) -> Self {
        self.session_token = Some(token.into());
        self
    }

    pub fn has_session_token(&self) -> bool {
        self.session_token
            .as_ref()
            .is_some_and(|token| !token.is_empty())
    }

    pub fn has_account_key(&self) -> bool {
        self.account_key.as_ref().is_some_and(|key| !key.is_empty())
    }

    pub fn has_creds(&self) -> bool {
        self.has_session_token() || self.has_account_key()
    }
}

impl Zeroize for HttpCredentials {
    fn zeroize(&mut self) {
        if let Some(token) = self.token.as_mut() {
            token.zeroize();
        }
    }
}

impl Zeroize for GrpcCredentials {
    fn zeroize(&mut self) {
        if let Some(token) = self.session_token.as_mut() {
            token.zeroize();
        }
        if let Some(key) = self.account_key.as_mut() {
            key.zeroize();
        }
        if let Some(id) = self.account_id.as_mut() {
            id.zeroize();
        }
    }
}

/// parse keystore to get name and modifiers
/// from --keystore NAME:key=value
/// or `ANYTYPE_KEYSTORE`=
fn parse_keystore(input: &str) -> Result<(&str, HashMap<&str, &str>), String> {
    // remove spaces and optional trailing colon
    let input = input.trim().trim_end_matches(':');
    if input.is_empty() {
        error!("missing keystore type");
        return Err("missing keystore type".to_string());
    }

    // Split at the first colon to separate the keystore from key=value pairs
    let (keystore, remainder) = match input.split_once(':') {
        Some((ks, remainder)) => (ks, Some(remainder)),
        None => (input, None),
    };

    if keystore.is_empty() {
        error!("missing keystore type");
        return Err("missing keystore type".to_string());
    }

    let mut map = HashMap::new();

    if let Some(modifiers) = remainder {
        for (key, value) in parse_modifier_pairs(modifiers)? {
            // Preserve the established last-wins behavior for duplicate
            // modifiers. Callers that require unique keys must reject them
            // before constructing a store.
            map.insert(key, value);
        }
    }

    Ok((keystore, map))
}

fn parse_modifier_pairs(input: &str) -> Result<Vec<(&str, &str)>, String> {
    const SYNTAX: &str = "invalid syntax. Expecting keystore name, or with modifiers, for example: 'keystore:key1=val1:key2=val2'";
    let mut pairs = Vec::new();
    let mut remaining = input;
    while !remaining.is_empty() {
        let Some(equals) = remaining.find('=') else {
            return Err(SYNTAX.to_owned());
        };
        let key = &remaining[..equals];
        if !valid_modifier_key(key) {
            return Err(SYNTAX.to_owned());
        }
        let value_and_rest = &remaining[equals + 1..];
        let boundary = value_and_rest
            .char_indices()
            .find_map(|(index, character)| {
                if character != ':' {
                    return None;
                }
                let candidate = &value_and_rest[index + 1..];
                let candidate_equals = candidate.find('=')?;
                valid_modifier_key(&candidate[..candidate_equals]).then_some(index)
            });
        match boundary {
            Some(index) => {
                pairs.push((key, &value_and_rest[..index]));
                remaining = &value_and_rest[index + 1..];
            }
            None => {
                pairs.push((key, value_and_rest));
                remaining = "";
            }
        }
    }
    Ok(pairs)
}

fn valid_modifier_key(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

/// The static default keystore backend for the current platform.
///
/// On Linux the effective default is resolved at runtime by
/// `resolve_default_store`, which prefers a running Secret Service and only
/// falls back to this `"file"` value when none is available; keyutils is no
/// longer used as a default because it is non-persistent and its read path is
/// unsupported by the keyring core (`search`). Other platforms use their native
/// store unconditionally.
pub fn default_platform_keyring() -> &'static str {
    if cfg!(target_os = "macos") {
        "keychain"
    } else if cfg!(target_os = "windows") {
        "windows"
    } else {
        // Linux resolves at runtime (see `resolve_default_store`); this is the
        // fallback for Linux and any other platform.
        "file"
    }
}

/// Resolve the default keystore when the user has not selected one explicitly
/// (neither a `--keystore` argument nor `ANYTYPE_KEYSTORE`).
///
/// On Linux this mirrors the official anytype-cli: prefer the OS Secret Service
/// (gnome-keyring/KWallet) when a session bus is present and the provider
/// answers, otherwise fall back to the file-based store. Constructing the
/// Secret Service store performs the D-Bus `OpenSession` handshake, so a
/// successful build is an authoritative availability probe and the resulting
/// store is reused. Other platforms build their native default directly.
fn resolve_default_store(service: &str) -> Result<(String, Arc<CredentialStore>), KeyStoreError> {
    #[cfg(target_os = "linux")]
    {
        // Fast reject for headless/CI: without a session bus there is no Secret
        // Service to talk to, so skip the connect attempt entirely.
        if std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_some() {
            match init_keystore("secret-service", service) {
                Ok(store) => return Ok(("secret-service".to_string(), store)),
                Err(err) => {
                    debug!("secret-service unavailable, falling back to file store: {err}");
                }
            }
        } else {
            debug!("no DBUS_SESSION_BUS_ADDRESS; using file store");
        }
        let store = init_keystore("file", service)?;
        Ok(("file".to_string(), store))
    }
    #[cfg(not(target_os = "linux"))]
    {
        let spec = default_platform_keyring().to_string();
        let store = init_keystore(&spec, service)?;
        Ok((spec, store))
    }
}

/// create in-memory hashmap store populated from environment variables
/// This may be useful in environments where keys can be set in environment, e.g., AWS, github actions, etc.
fn store_from_env(service: &str) -> std::result::Result<Arc<CredentialStore>, KeyStoreError> {
    use keyring_core::api::CredentialStoreApi;

    let sample = keyring_core::sample::Store::new().map_err(|_| KeyStoreError::Config {
        message: "cannot create default sample store".to_string(),
    })?;

    let mut credentials = StoredCredentials {
        version: CREDENTIALS_FORMAT_VERSION,
        http_token: std::env::var("ANYTYPE_KEY_HTTP_TOKEN").ok(),
        account_id: std::env::var("ANYTYPE_KEY_ACCOUNT_ID").ok(),
        account_key: std::env::var("ANYTYPE_KEY_ACCOUNT_KEY").ok(),
        session_token: std::env::var("ANYTYPE_KEY_SESSION_TOKEN").ok(),
    };
    if !credentials.is_empty() {
        let mut json = credentials.to_json()?;
        let entry = sample.build(service, KEY_CREDENTIALS, None)?;
        let stored = entry.set_password(&json);
        json.zeroize();
        stored?;
    }
    credentials.zeroize();

    Ok(sample)
}

fn init_keystore(input: &str, service: &str) -> Result<Arc<CredentialStore>, KeyStoreError> {
    let (keystore_name, modifiers) =
        parse_keystore(input).map_err(|message| KeyStoreError::Config { message })?;

    match keystore_name {
        "env" => {
            let env_store = store_from_env(service)?;
            Ok(env_store)
        }
        "file" | "sqlite" => {
            let store: Arc<db_keystore::DbKeyStore> =
                db_keystore::DbKeyStore::new_with_modifiers(&modifiers)?;
            Ok(store)
        }
        #[cfg(target_os = "macos")]
        "keychain" => {
            use apple_native_keyring_store::keychain::Store;
            Ok(Store::new_with_configuration(&modifiers)?)
        }
        #[cfg(target_os = "linux")]
        "keyutils" => {
            use linux_keyutils_keyring_store::Store;
            Ok(Store::new_with_configuration(&modifiers)?)
        }
        #[cfg(target_os = "linux")]
        "secret-service" | "secret-service-sync" => {
            use dbus_secret_service_keyring_store::Store;
            Ok(Store::new_with_configuration(&modifiers)?)
        }
        #[cfg(target_os = "windows")]
        "windows" => {
            use windows_native_keyring_store::Store;
            Ok(Store::new_with_configuration(&modifiers)?)
        }
        _ => Err(KeyStoreError::Config {
            message: format!("Unsupported keystore {keystore_name} for this platform"),
        }),
    }
}

#[derive(Clone)]
pub struct KeyStore {
    service: String,
    store: Arc<CredentialStore>,
    spec: String,
}

impl fmt::Debug for KeyStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_fmt(format_args!(
            "KeyStore(id:{} service:{} spec:{})",
            self.id(),
            self.service,
            self.spec
        ))
    }
}

impl KeyStore {
    /// new keystore with default platform store
    pub fn new_default_store(service: impl Into<String>) -> Result<Self, KeyStoreError> {
        Self::new(service, "")
    }

    pub fn new(service: impl Into<String>, keystore_spec: &str) -> Result<Self, KeyStoreError> {
        let service = service.into();
        let keystore_spec = keystore_spec.trim().trim_end_matches(':');
        // Explicit selection wins: the `--keystore` argument, then the
        // `ANYTYPE_KEYSTORE` env var. When neither is set, resolve the platform
        // default (which on Linux probes for a running Secret Service).
        let explicit = if keystore_spec.is_empty() {
            std::env::var("ANYTYPE_KEYSTORE")
                .ok()
                .filter(|spec| !spec.is_empty())
        } else {
            Some(keystore_spec.to_string())
        };
        let (spec, store) = match explicit {
            Some(spec) => {
                let store = init_keystore(&spec, &service)?;
                (spec, store)
            }
            None => resolve_default_store(&service)?,
        };
        Ok(Self {
            service,
            store,
            spec,
        })
    }

    /// returns service name
    pub fn service(&self) -> &str {
        &self.service
    }

    /// returns keystore id
    pub fn id(&self) -> String {
        self.store.id()
    }

    pub(crate) fn store(&self) -> Arc<CredentialStore> {
        self.store.clone()
    }

    fn get_key(&self, name: impl AsRef<str>) -> Result<Option<String>, KeyStoreError> {
        let name = name.as_ref();
        debug!(service = &self.service, user = name, "get_key");
        // Read the credential directly by (service, user) via `build`, mirroring
        // how `put_key`/`remove_key` address entries. This avoids `search()`,
        // which some backends (e.g. keyutils) do not implement (they return
        // `NotSupportedByStore` and break all reads even though writes succeed).
        let entry = self.store.build(&self.service, name, None)?;
        match entry.get_password() {
            Ok(key) => Ok(Some(key)),
            Err(keyring_core::Error::NoEntry) => {
                debug!(service = &self.service, user = name, "key lookup: no entry");
                Ok(None)
            }
            Err(err) => {
                error!(service = &self.service, user = name, "key lookup: {err}");
                Err(err.into())
            }
        }
    }

    fn put_key(&self, name: &str, value: impl AsRef<str>) -> Result<(), KeyStoreError> {
        debug!(
            service = &self.service,
            user = name,
            value = value.as_ref().len(),
            "put_key"
        );
        let entry = self.store.build(&self.service, name, None)?;
        entry.set_password(value.as_ref())?;
        Ok(())
    }

    fn remove_key(&self, name: impl AsRef<str>) -> Result<(), KeyStoreError> {
        debug!(service = &self.service, user = name.as_ref(), "remove_key");
        let entry = self.store.build(&self.service, name.as_ref(), None)?;
        match entry.delete_credential() {
            Ok(()) | Err(keyring_core::Error::NoEntry) => Ok(()),
            Err(err) => Err(KeyStoreError::Keyring { source: err }),
        }
    }

    /// Loads the combined credential document.
    ///
    /// When the combined entry is absent, the legacy per-credential entries are
    /// read instead and, if any exist, migrated into the combined entry. A
    /// failed migration is logged and does not fail the read: the credentials
    /// are still returned, and migration is retried on the next access.
    fn load_credentials(&self) -> Result<StoredCredentials, KeyStoreError> {
        if let Some(mut json) = self.get_key(KEY_CREDENTIALS)? {
            let parsed = StoredCredentials::parse(&json);
            json.zeroize();
            return parsed;
        }
        let legacy = StoredCredentials {
            version: 0,
            http_token: self.get_key(KEY_HTTP_TOKEN)?,
            account_id: self.get_key(KEY_ACCOUNT_ID)?,
            account_key: self.get_key(KEY_ACCOUNT_KEY)?,
            session_token: self.get_key(KEY_SESSION_TOKEN)?,
        };
        if !legacy.is_empty() {
            debug!(
                service = &self.service,
                "migrating legacy per-credential entries to the combined credentials entry"
            );
            if let Err(err) = self.save_credentials(&legacy) {
                warn!(
                    service = &self.service,
                    "credential migration deferred: {err}"
                );
            }
        }
        Ok(legacy)
    }

    /// Writes the combined credential document (or removes it when empty) and
    /// removes any legacy per-credential entries.
    ///
    /// Legacy removal is best-effort: an entry that survives is ignored on later
    /// reads because the combined entry takes precedence.
    fn save_credentials(&self, credentials: &StoredCredentials) -> Result<(), KeyStoreError> {
        if credentials.is_empty() {
            self.remove_key(KEY_CREDENTIALS)?;
        } else {
            let mut json = credentials.to_json()?;
            let stored = self.put_key(KEY_CREDENTIALS, &json);
            json.zeroize();
            stored?;
        }
        self.remove_legacy_keys();
        Ok(())
    }

    fn remove_legacy_keys(&self) {
        for name in LEGACY_KEYS {
            if let Err(err) = self.remove_key(name) {
                debug!(
                    service = &self.service,
                    user = name,
                    "legacy credential entry not removed: {err}"
                );
            }
        }
    }

    /// Looks up http auth token.
    /// If connection with keystore succeeded, returns Ok, even if no token exists
    /// for the current service.
    /// Check `has_creds()` or `has_token()` on `HttpCredentials` to determine whether a token is present.
    /// Returns Err if keystore was not correctly configured or there was an error
    /// connecting with the keystore (such as user biometric auth failure for os keyring,
    /// or file permission error for file-based keystore)
    pub fn get_http_credentials(&self) -> Result<HttpCredentials, KeyStoreError> {
        let mut stored = self.load_credentials()?;
        let http = stored.http();
        stored.zeroize();
        if !http.has_creds() {
            debug!(
                service = &self.service,
                id = &self.id(),
                "get_http_creds: no token",
            );
        }
        Ok(http)
    }

    /// Looks up grpc auth credentials.
    /// If connection with keystore succeeded, returns Ok, even if no credentials exist
    /// for the current service and credential type.
    /// Check `has_creds()` on `GrpcCredentials` to determine whether a token is present.
    /// Returns Err if keystore was not correctly configured or there was an error
    /// connecting with the keystore (such as user biometric auth failure for os keyring,
    /// or file permission error for file-based keystore)
    pub fn get_grpc_credentials(&self) -> Result<GrpcCredentials, KeyStoreError> {
        let mut stored = self.load_credentials()?;
        let grpc = stored.grpc();
        stored.zeroize();
        Ok(grpc)
    }

    /// Checks a test-owned byte buffer for every configured credential
    /// without returning any credential bytes to the caller.
    ///
    /// This helper exists only for downstream acceptance fixtures. It returns
    /// `false` when either HTTP or gRPC credentials are absent, or when any
    /// configured credential occurs verbatim in `bytes`.
    #[cfg(feature = "test-fixtures")]
    #[doc(hidden)]
    pub fn configured_credentials_absent_from(&self, bytes: &[u8]) -> Result<bool, KeyStoreError> {
        let mut http = self.get_http_credentials()?;
        let mut grpc = self.get_grpc_credentials()?;
        let configured = http.has_creds() && grpc.has_creds();
        let exposed = http
            .token
            .iter()
            .chain(grpc.account_id.iter())
            .chain(grpc.account_key.iter())
            .chain(grpc.session_token.iter())
            .filter(|credential| !credential.is_empty())
            .any(|credential| {
                bytes
                    .windows(credential.len())
                    .any(|window| window == credential.as_bytes())
            });
        http.zeroize();
        grpc.zeroize();
        Ok(configured && !exposed)
    }

    /// Saves HTTP credentials (read-modify-write).
    /// An empty token leaves the stored token unchanged (use `clear_*` to remove).
    pub fn update_http_credentials(&self, creds: &HttpCredentials) -> Result<(), KeyStoreError> {
        if let Some(token) = &creds.token
            && !token.is_empty()
        {
            let mut stored = self.load_credentials()?;
            stored.http_token = Some(token.clone());
            let saved = self.save_credentials(&stored);
            stored.zeroize();
            saved?;
        }
        Ok(())
    }

    /// Saves gRPC credentials (read-modify-write).
    /// Fields that are `None` leave the stored value unchanged (use `clear_*` to remove).
    pub fn update_grpc_credentials(&self, creds: &GrpcCredentials) -> Result<(), KeyStoreError> {
        let mut stored = self.load_credentials()?;
        if let Some(account_id) = &creds.account_id {
            stored.account_id = Some(account_id.clone());
        }
        if let Some(account_key) = &creds.account_key {
            stored.account_key = Some(account_key.clone());
        }
        if let Some(session_token) = &creds.session_token {
            stored.session_token = Some(session_token.clone());
        }
        let saved = self.save_credentials(&stored);
        stored.zeroize();
        saved
    }

    /// Clear HTTP credentials.
    pub fn clear_http_credentials(&self) -> Result<(), KeyStoreError> {
        let mut stored = self.load_credentials()?;
        stored.http_token = None;
        let saved = self.save_credentials(&stored);
        stored.zeroize();
        saved
    }

    /// Clear gRPC credentials.
    pub fn clear_grpc_credentials(&self) -> Result<(), KeyStoreError> {
        let mut stored = self.load_credentials()?;
        stored.account_id = None;
        stored.account_key = None;
        stored.session_token = None;
        let saved = self.save_credentials(&stored);
        stored.zeroize();
        saved
    }

    /// Clear all credentials (for the service associated with this `KeyStore`).
    pub fn clear_all_credentials(&self) -> Result<(), KeyStoreError> {
        self.remove_key(KEY_CREDENTIALS)?;
        for name in LEGACY_KEYS {
            self.remove_key(name)?;
        }
        Ok(())
    }

    /// Update gRPC credentials from the headless CLI config.json.
    pub fn update_grpc_from_cli_config(&self, path: Option<&Path>) -> Result<(), KeyStoreError> {
        let credentials =
            GrpcCredentials::from_cli_config(path)?.ok_or_else(|| KeyStoreError::External {
                message: "headless config not found".to_string(),
            })?;
        self.update_grpc_credentials(&credentials)
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::config::DEFAULT_SERVICE_NAME;

    #[test]
    fn parser_preserves_windows_and_colon_bearing_paths() {
        for (specification, expected) in [
            (
                r"file:path=C:\Users\example\keys.db:cipher=aes256",
                r"C:\Users\example\keys.db",
            ),
            (
                "file:path=C:/Users/example/keys.db:cipher=aes256",
                "C:/Users/example/keys.db",
            ),
            (
                "file:path=/var/lib/anytype/keys:primary.db:cipher=aes256",
                "/var/lib/anytype/keys:primary.db",
            ),
        ] {
            let (kind, modifiers) = parse_keystore(specification).expect("portable keystore spec");
            assert_eq!(kind, "file");
            assert_eq!(modifiers.get("path"), Some(&expected));
            assert_eq!(modifiers.get("cipher"), Some(&"aes256"));
        }
    }

    #[test]
    fn parser_retains_documented_last_wins_modifier_behavior() {
        let (_, modifiers) =
            parse_keystore("file:path=first.db:path=second.db").expect("duplicate grammar");
        assert_eq!(modifiers.get("path"), Some(&"second.db"));
    }

    #[test]
    fn grpc_credentials_load_from_cli_config_without_storing() {
        let path = std::env::temp_dir().join(format!(
            "anytype_cli_credentials_{}.json",
            std::process::id()
        ));
        fs::write(
            &path,
            r#"{"accountId":"account-id","accountKey":"account-key","sessionToken":"session-token"}"#,
        )
        .expect("write CLI config");

        let credentials = GrpcCredentials::from_cli_config(Some(&path))
            .expect("load CLI config")
            .expect("CLI config exists");
        assert_eq!(credentials.account_id(), Some("account-id"));
        assert_eq!(credentials.account_key(), Some("account-key"));
        assert_eq!(credentials.session_token(), Some("session-token"));

        fs::remove_file(path).expect("remove CLI config");
    }

    #[cfg(feature = "test-fixtures")]
    #[test]
    fn credential_absence_check_never_returns_secret_bytes() -> Result<(), KeyStoreError> {
        let temp_dir =
            std::env::temp_dir().join(format!("anytype_credential_absence_{}", std::process::id()));
        let _ = fs::remove_dir_all(&temp_dir);
        let file_path = temp_dir.join("credentials.db");
        let store = KeyStore::new(
            "credential-absence-test",
            &format!("file:path={}", file_path.display()),
        )?;
        store.update_http_credentials(&HttpCredentials::new("http-secret"))?;
        store.update_grpc_credentials(&GrpcCredentials::from_account_key("grpc-secret"))?;
        assert!(store.configured_credentials_absent_from(b"reviewed event")?);
        assert!(!store.configured_credentials_absent_from(b"prefix http-secret suffix")?);
        assert!(!store.configured_credentials_absent_from(b"prefix grpc-secret suffix")?);
        store.clear_all_credentials()?;
        let _ = fs::remove_dir_all(temp_dir);
        Ok(())
    }

    fn temp_file_store(name: &str) -> (KeyStore, std::path::PathBuf) {
        let temp_dir = std::env::temp_dir().join(format!("anytype_{name}_{}", std::process::id()));
        let _ = fs::remove_dir_all(&temp_dir);
        let path = temp_dir.join("keys.db");
        let store =
            KeyStore::new(name, &format!("file:path={}", path.display())).expect("file keystore");
        (store, temp_dir)
    }

    #[test]
    fn credentials_are_stored_in_a_single_entry() -> Result<(), KeyStoreError> {
        let (store, temp_dir) = temp_file_store("single_entry");

        store.update_http_credentials(&HttpCredentials::new("http-token"))?;
        store.update_grpc_credentials(
            &GrpcCredentials::from_account_key("account-key").with_account_id("account-id"),
        )?;

        // exactly one entry, holding every credential
        let mut json = store.get_key(KEY_CREDENTIALS)?.expect("combined entry");
        let document: serde_json::Value = serde_json::from_str(&json).expect("json");
        assert_eq!(document["v"], CREDENTIALS_FORMAT_VERSION);
        assert_eq!(document["http_token"], "http-token");
        assert_eq!(document["account_id"], "account-id");
        assert_eq!(document["account_key"], "account-key");
        assert!(document.get("session_token").is_none());
        json.zeroize();
        for name in LEGACY_KEYS {
            assert!(
                store.get_key(name)?.is_none(),
                "legacy entry {name} written"
            );
        }

        // partial updates merge; None leaves fields alone
        store.update_grpc_credentials(&GrpcCredentials::from_token("session"))?;
        let grpc = store.get_grpc_credentials()?;
        assert_eq!(grpc.account_key(), Some("account-key"));
        assert_eq!(grpc.session_token(), Some("session"));
        assert!(store.get_http_credentials()?.has_creds());

        // clearing one family keeps the other
        store.clear_grpc_credentials()?;
        assert!(!store.get_grpc_credentials()?.has_creds());
        assert!(store.get_http_credentials()?.has_creds());

        // clearing everything removes the entry
        store.clear_http_credentials()?;
        assert!(store.get_key(KEY_CREDENTIALS)?.is_none());

        let _ = fs::remove_dir_all(temp_dir);
        Ok(())
    }

    #[test]
    fn legacy_per_credential_entries_are_read_and_migrated() -> Result<(), KeyStoreError> {
        let (store, temp_dir) = temp_file_store("legacy_migration");
        store.put_key(KEY_HTTP_TOKEN, "legacy-http")?;
        store.put_key(KEY_ACCOUNT_KEY, "legacy-account-key")?;
        store.put_key(KEY_SESSION_TOKEN, "legacy-session")?;

        let http = store.get_http_credentials()?;
        assert_eq!(http.token(), Some("legacy-http"));
        let grpc = store.get_grpc_credentials()?;
        assert_eq!(grpc.account_key(), Some("legacy-account-key"));
        assert_eq!(grpc.session_token(), Some("legacy-session"));
        assert_eq!(grpc.account_id(), None);

        // the first read migrated the store
        assert!(store.get_key(KEY_CREDENTIALS)?.is_some());
        for name in LEGACY_KEYS {
            assert!(
                store.get_key(name)?.is_none(),
                "legacy entry {name} remains"
            );
        }

        // and reads still agree afterwards
        assert_eq!(store.get_http_credentials()?.token(), Some("legacy-http"));

        store.clear_all_credentials()?;
        assert!(store.get_key(KEY_CREDENTIALS)?.is_none());
        let _ = fs::remove_dir_all(temp_dir);
        Ok(())
    }

    #[test]
    fn env_store_populates_the_single_entry() -> Result<(), KeyStoreError> {
        // SAFETY: test process; the variables are namespaced and this test owns them.
        unsafe {
            std::env::set_var("ANYTYPE_KEY_HTTP_TOKEN", "env-http");
            std::env::set_var("ANYTYPE_KEY_ACCOUNT_KEY", "env-account-key");
        }
        let store = KeyStore::new("env_single_entry", "env")?;
        assert_eq!(store.get_http_credentials()?.token(), Some("env-http"));
        assert_eq!(
            store.get_grpc_credentials()?.account_key(),
            Some("env-account-key")
        );
        assert!(store.get_key(KEY_HTTP_TOKEN)?.is_none());
        unsafe {
            std::env::remove_var("ANYTYPE_KEY_HTTP_TOKEN");
            std::env::remove_var("ANYTYPE_KEY_ACCOUNT_KEY");
        }
        Ok(())
    }

    // TODO: this test case checks too many things - should be split up
    #[test]
    fn test_file_storage_save_and_load() -> Result<(), KeyStoreError> {
        // Use a unique temp dir based on process id to avoid cleanup issues
        let temp_dir = std::env::temp_dir().join(format!(
            "anytype_rust_api_test_storage_{}",
            std::process::id()
        ));
        // Ensure clean start
        let _ = fs::remove_dir_all(&temp_dir);
        let file_path = temp_dir.join(format!("{DEFAULT_SERVICE_NAME}.test.key"));
        let keystore_spec = format!("file:path={}", file_path.display());
        let key_store = KeyStore::new("test_file_storage", &keystore_spec)?;

        // Initially no key
        let no_exist = key_store.get_http_credentials()?;
        assert!(!no_exist.has_creds());

        // Save a key
        let test_key = "test-key-123";
        key_store.update_http_credentials(&HttpCredentials::new(test_key))?;

        // Read the key from file directly to test save
        let load_key = key_store.get_http_credentials()?;
        assert!(load_key.has_creds());
        assert_eq!(
            load_key.token,
            Some(test_key.to_string()),
            "save+load returns same key"
        );

        // Remove the key
        key_store.clear_http_credentials()?;

        // Key should be gone
        let check_file = key_store.get_http_credentials()?;
        assert!(!check_file.has_creds(), "expected file removed");

        // Clean up
        key_store.clear_all_credentials()?;
        fs::remove_dir_all(&temp_dir).ok();
        Ok(())
    }

    #[test]
    #[ignore] // Run with: cargo test -- --ignored
    fn test_keyring_storage_end_to_end() -> Result<(), KeyStoreError> {
        // This test uses the actual OS keyring and may prompt for authentication
        // Run explicitly with: cargo test -- --ignored test_keyring_storage_end_to_end
        //
        // NOTE: This test may not work in all environments:
        // - macOS: May require Keychain unlock or fail in headless/CI environments
        // - Linux: Requires Secret Service (gnome-keyring/KWallet) and may require GUI session
        // - Windows: Should work but may prompt for credential manager access
        //
        // This is primarily for manual testing to verify the keyring integration works
        // on your specific system. Failure here doesn't necessarily indicate a bug - it
        // may just mean the keyring service isn't available in your test environment.

        let service_name = format!("{DEFAULT_SERVICE_NAME}.e2etest");

        let key_store = KeyStore::new_default_store(service_name)?;

        // Clean up any existing test data first
        let () = key_store.clear_http_credentials()?;

        // Save a test key
        let test_key = "test-keyring-api-key-12345";
        key_store.update_http_credentials(&HttpCredentials {
            token: Some(test_key.to_string()),
        })?;

        // Load the key
        let loaded_key = key_store.get_http_credentials()?;
        assert!(loaded_key.has_creds(), "loaded key");
        assert_eq!(
            loaded_key.token,
            Some(test_key.to_string()),
            "load key from keyring"
        );

        // if this fails, try:
        //   'auth login', 'auth status', 'auth logout'
        //   on macos, program may require code signing or explicit entitlements
        //   on linux, need gnome-keyring or KWallet daemon running
        //   on Windows, may have UAC/permission issues

        // Remove the key
        key_store
            .clear_http_credentials()
            .expect("Should remove from keyring");
        println!("✓ Removed test key from keyring");

        // Verify it's gone
        let after_delete = key_store.get_http_credentials()?;
        assert!(!after_delete.has_creds(), "after removal from keyring");
        Ok(())
    }
}
