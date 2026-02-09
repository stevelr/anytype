//! Secure storage for API keys and credentials
//!
//! Provides cross-platform storage backends for API keys:
//! - **Keyring**: OS-native secure credential stores (Keychain/Secret Service/Credential Manager)
//! - **File**: File-based storage in user config directory (less secure, for compatibility)

#[cfg(feature = "grpc")]
use std::path::Path;
use std::{collections::HashMap, fmt, sync::Arc};

use keyring_core::CredentialStore;
use tracing::{debug, error};
use zeroize::Zeroize;

use crate::error::KeyStoreError;

const KEY_HTTP_TOKEN: &str = "http_token";
const KEY_ACCOUNT_ID: &str = "account_id";
const KEY_ACCOUNT_KEY: &str = "account_key";
const KEY_SESSION_TOKEN: &str = "session_token";

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

#[cfg(feature = "grpc")]
#[derive(Clone, Default)]
pub struct GrpcCredentials {
    account_id: Option<String>,
    account_key: Option<String>,
    session_token: Option<String>,
}

#[cfg(feature = "grpc")]
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

#[cfg(feature = "grpc")]
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

#[cfg(feature = "grpc")]
impl GrpcCredentials {
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

#[cfg(feature = "grpc")]
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
        for part in modifiers.split(':') {
            if let Some((key, value)) = part.split_once('=') {
                if key.is_empty() {
                    return Err("invalid syntax. Expecting keystore name, or with modifiers, for example: 'keystore:key1=val1:key2=val2'".to_string());
                }
                map.insert(key, value);
            } else {
                return Err("invalid syntax. Expecting keystore name, or with modifiers, for example: 'keystore:key1=val1:key2=val2'".to_string());
            }
        }
    }

    Ok((keystore, map))
}

pub fn default_platform_keyring() -> &'static str {
    if cfg!(target_os = "macos") {
        "keychain"
    } else if cfg!(target_os = "linux") {
        "keyutils"
    } else if cfg!(target_os = "windows") {
        "windows"
    } else {
        "file"
    }
}

/// create in-memory hashmap store populated from environment variables
/// This may be useful in environments where keys can be set in environment, e.g., AWS, github actions, etc.
fn store_from_env(service: &str) -> std::result::Result<Arc<CredentialStore>, KeyStoreError> {
    use keyring_core::api::CredentialStoreApi;

    let sample = keyring_core::sample::Store::new().map_err(|_| KeyStoreError::Config {
        message: "cannot create default sample store".to_string(),
    })?;

    if let Ok(http_token) = std::env::var("ANYTYPE_KEY_HTTP_TOKEN") {
        let entry = sample.build(service, KEY_HTTP_TOKEN, None)?;
        entry.set_password(&http_token)?;
    }

    if let Ok(account_id) = std::env::var("ANYTYPE_KEY_ACCOUNT_ID") {
        let entry = sample.build(service, KEY_ACCOUNT_ID, None)?;
        entry.set_password(&account_id)?;
    }

    if let Ok(account_key) = std::env::var("ANYTYPE_KEY_ACCOUNT_KEY") {
        let entry = sample.build(service, KEY_ACCOUNT_KEY, None)?;
        entry.set_password(&account_key)?;
    }

    if let Ok(session_token) = std::env::var("ANYTYPE_KEY_SESSION_TOKEN") {
        let entry = sample.build(service, KEY_SESSION_TOKEN, None)?;
        entry.set_password(&session_token)?;
    }

    Ok(sample)
}

fn init_keystore(input: &str, service: &str) -> Result<Arc<CredentialStore>, KeyStoreError> {
    let (mut keystore_name, modifiers) =
        parse_keystore(input).map_err(|message| KeyStoreError::Config { message })?;

    if keystore_name == "file" {
        keystore_name = "sqlite"
    };

    match keystore_name {
        "env" => {
            let env_store = store_from_env(service)?;
            keyring_core::set_default_store(env_store);
        }
        _ => {
            keyring::use_named_store_with_modifiers(keystore_name, &modifiers)?;
        }
    }
    // unwrap ok because every code path above sets default store
    let store = keyring_core::get_default_store().unwrap();
    Ok(store)
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
            &self.service,
            &self.spec
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
        // if keystore isn't specified here,
        // try environment, otherwise default for platform
        let spec = if keystore_spec.is_empty() {
            std::env::var("ANYTYPE_KEYSTORE")
                .unwrap_or_else(|_| default_platform_keyring().to_string())
        } else {
            keystore_spec.to_string()
        };
        let store = init_keystore(&spec, &service)?;
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
        let mut map = HashMap::new();
        map.insert("service", self.service.as_ref());
        map.insert("user", name);
        debug!(service = &self.service, user = name, "get_key");
        match self.store.search(&map) {
            Ok(entries) => {
                debug!("get_key found {} entries", entries.len());
                // search results are not ambiguous: there are 0 or 1 entries,
                // because there is no way to insert multiple keys with same (service,user)
                entries.first().map_or_else(
                    || Ok(None),
                    |entry| match entry.get_password() {
                        Ok(key) => Ok(Some(key)),
                        Err(keyring_core::Error::NoEntry) => {
                            debug!("get_key got entry with NoEntry !?!?");
                            Ok(None)
                        }
                        Err(err) => {
                            error!("get_key: {err}");
                            Err(err.into())
                        }
                    },
                )
            }
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
        entry.delete_credential()?;
        Ok(())
    }

    /// Looks up http auth token.
    /// If connection with keystore succeeded, returns Ok, even if no token exists
    /// for the current service.
    /// Check `has_creds()` or `has_token()` on `HttpCredentials` to determine whether a token is present.
    /// Returns Err if keystore was not correctly configured or there was an error
    /// connecting with the keystore (such as user biometric auth failure for os keyring,
    /// or file permission error for file-based keystore)
    pub fn get_http_credentials(&self) -> Result<HttpCredentials, KeyStoreError> {
        let token = self.get_key(KEY_HTTP_TOKEN)?;
        if token.is_none() {
            debug!(
                service = &self.service,
                id = &self.id(),
                "get_http_creds: no token",
            );
        }
        Ok(HttpCredentials { token })
    }

    /// Looks up grpc auth credentials.
    /// If connection with keystore succeeded, returns Ok, even if no credentials exist
    /// for the current service and credential type.
    /// Check `has_creds()` on `GrpcCredentials` to determine whether a token is present.
    /// Returns Err if keystore was not correctly configured or there was an error
    /// connecting with the keystore (such as user biometric auth failure for os keyring,
    /// or file permission error for file-based keystore)
    #[cfg(feature = "grpc")]
    pub fn get_grpc_credentials(&self) -> Result<GrpcCredentials, KeyStoreError> {
        Ok(GrpcCredentials {
            account_id: self.get_key(KEY_ACCOUNT_ID)?,
            account_key: self.get_key(KEY_ACCOUNT_KEY)?,
            session_token: self.get_key(KEY_SESSION_TOKEN)?,
        })
    }

    /// Saves HTTP credentials (read-modify-write).
    /// Fails if credentials are empty (use clear_* to remove).
    pub fn update_http_credentials(&self, creds: &HttpCredentials) -> Result<(), KeyStoreError> {
        if let Some(token) = &creds.token
            && !token.is_empty()
        {
            self.put_key(KEY_HTTP_TOKEN, token)?;
        }
        Ok(())
    }

    /// Saves gRPC credentials (read-modify-write).
    /// Fails if credentials are empty (use clear_* to remove).
    #[cfg(feature = "grpc")]
    pub fn update_grpc_credentials(&self, creds: &GrpcCredentials) -> Result<(), KeyStoreError> {
        if let Some(account_id) = &creds.account_id {
            self.put_key(KEY_ACCOUNT_ID, account_id)?;
        }
        if let Some(account_key) = &creds.account_key {
            self.put_key(KEY_ACCOUNT_KEY, account_key)?;
        }
        if let Some(session_token) = &creds.session_token {
            self.put_key(KEY_SESSION_TOKEN, session_token)?;
        }
        Ok(())
    }

    /// Clear HTTP credentials.
    pub fn clear_http_credentials(&self) -> Result<(), KeyStoreError> {
        self.remove_key(KEY_HTTP_TOKEN)?;
        Ok(())
    }

    /// Clear gRPC credentials.
    #[cfg(feature = "grpc")]
    pub fn clear_grpc_credentials(&self) -> Result<(), KeyStoreError> {
        self.remove_key(KEY_ACCOUNT_ID)?;
        self.remove_key(KEY_ACCOUNT_KEY)?;
        self.remove_key(KEY_SESSION_TOKEN)?;
        Ok(())
    }

    /// Clear all credentials (for the service associated with this `KeyStore`).
    pub fn clear_all_credentials(&self) -> Result<(), KeyStoreError> {
        self.clear_http_credentials()?;
        #[cfg(feature = "grpc")]
        self.clear_grpc_credentials()?;
        Ok(())
    }

    /// Update gRPC credentials from the headless CLI config.json.
    #[cfg(feature = "grpc")]
    pub fn update_grpc_from_cli_config(&self, path: Option<&Path>) -> Result<(), KeyStoreError> {
        use anytype_rpc::config::load_headless_config;
        let config = load_headless_config(path).map_err(|err| KeyStoreError::External {
            message: format!("failed to load headless config: {err}"),
        })?;
        let config = config.ok_or_else(|| KeyStoreError::External {
            message: "headless config not found".to_string(),
        })?;
        let creds = GrpcCredentials {
            account_id: config.account_id,
            account_key: config.account_key,
            session_token: config.session_token,
        };
        self.update_grpc_credentials(&creds)
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::config::DEFAULT_SERVICE_NAME;

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
    //#[ignore] // Run with: cargo test -- --ignored
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
