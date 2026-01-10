//! Secure storage for API keys and credentials
//!
//! Provides cross-platform storage backends for API keys:
//! - **Keyring**: OS-native secure credential stores (Keychain/Secret Service/Credential Manager)
//! - **File**: File-based storage in user config directory (less secure, for compatibility)

use crate::{Result, config::DEFAULT_KEY_USER, prelude::*};
use snafu::prelude::*;
use std::{
    fmt,
    path::{Path, PathBuf},
};
use tracing::{debug, error, warn};

/// Type of keystore - builtin or external
#[derive(Clone, PartialEq, Eq)]
pub enum KeyStoreType {
    /// built-in file keystore: stores key in a clear text file
    File,
    /// OS-managed keyring, when supported (uses keyring crate)
    Keyring,
    /// No keystore. If this variant is used, keys are not persisted.
    None,
    /// Other keystore - Implementation outside this crate.
    #[cfg(feature = "keystore-ext")]
    Other(String),
}

impl fmt::Display for KeyStoreType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            KeyStoreType::File => "file",
            KeyStoreType::Keyring => "keyring",
            KeyStoreType::None => "none",
            #[cfg(feature = "keystore-ext")]
            KeyStoreType::Other(s) => s.as_str(),
        })
    }
}

impl fmt::Debug for KeyStoreType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_fmt(format_args!(
            "KeyStoreType({})",
            match self {
                KeyStoreType::File => "File",
                KeyStoreType::Keyring => "Keyring",
                KeyStoreType::None => "None",
                #[cfg(feature = "keystore-ext")]
                KeyStoreType::Other(s) => s.as_str(),
            }
        ))
    }
}

/// Safe wrapper for api key, used for passing key between AnytypeClient and KeyStore.
/// Prevents logging secrets and implements zeroize on drop.
#[derive(Clone)]
pub struct SecretApiKey(String);

impl SecretApiKey {
    /// Creates a wrapper for a secret key.
    pub fn new(key: impl Into<String>) -> Self {
        SecretApiKey(key.into())
    }

    /// Retrieve the inner key.
    ///
    /// **SAFETY: To prevent accidentally leaking the secret api key,
    /// applications should not use this method**.
    /// If applications need the key at all (for example, to pass to a KeyStore,
    /// they should use the SecretApiKey wrapper, which prevents accidental logging,
    /// and implements zeroize on drop.
    ///
    /// For completeness of documentation of key security:
    ///
    /// - This is the only api method for directly obtaining the api key. Its purpose
    ///   is to enable anytype users to provide alternate key storage (encrypted storage,
    ///   KMS, etc.).
    /// - Another way to get the key programmatically is to create a KeyStoreFile
    ///   with an absolute path, save the key, and then read the file directly. KeyStoreFile
    ///   stores the key in cleartext. On linux and macos, the file access is set to mode 0600
    ///   (owner-only access) but it is not terribly secure.
    #[cfg(feature = "keystore-ext")]
    pub fn get_key(&self) -> &str {
        &self.0
    }

    // helper function for http client to set bearer token header
    pub(crate) fn set_auth_header(
        &self,
        request_builder: reqwest::RequestBuilder,
    ) -> reqwest::RequestBuilder {
        // don't log api key
        //trace!("setting bearer token: {}..(MASKED)", &self.0[..2]);
        request_builder.bearer_auth(&self.0)
    }

    #[cfg(test)]
    #[doc(hidden)]
    /// this method allows tests to confirm a key without revealing it
    /// This may be overkill, since hopefully tests don't use real keys
    pub fn check_key(&self, value: &str) -> bool {
        self.0 == value
    }
}

impl<S: Into<String>> From<S> for SecretApiKey {
    /// Creates a wrapper for a secret key.
    fn from(value: S) -> Self {
        SecretApiKey::new(value)
    }
}

impl Drop for SecretApiKey {
    /// Implements zeroize on drop.
    fn drop(&mut self) {
        use zeroize::Zeroize;
        self.0.zeroize()
    }
}

impl fmt::Display for SecretApiKey {
    /// Display implementation to prevent accidental logging of secrets
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&format!("SecretApiKey(REDACTED[len={}])", self.0.len()))
    }
}

impl fmt::Debug for SecretApiKey {
    /// Debug implementation to prevent accidental logging of secrets
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&format!("SecretApiKey(REDACTED[len={}])", self.0.len()))
    }
}

pub trait KeyStore: fmt::Debug + Send + Sync {
    /// Load API key from storage
    ///
    /// Returns:
    /// - SecretApiKey if key was retrieved. The Contents of SecretApiKey cannot be read
    ///   by clients, but it can be passed to client.set_api_key(),
    ///   or saved to a different keystore.
    ///
    /// Errors:
    /// - AnytypeError::KeystoreEmpty: keystore contains no such key
    /// - AnytypeError::Keyring: os keyring could not load key
    /// - AnytypeError::KeystoreFile: file store could not be read
    fn load_key(&self) -> std::result::Result<Option<SecretApiKey>, KeyStoreError>;

    /// Saves api key to secure storage.
    fn save_key(&self, api_key: &SecretApiKey) -> std::result::Result<(), KeyStoreError>;

    /// Deletes key from key storage
    fn remove_key(&self) -> std::result::Result<(), KeyStoreError>;

    /// Returns true if the keystore is configured (in other words, is not type None)
    fn is_configured(&self) -> bool {
        self.store_type() != KeyStoreType::None
    }

    fn store_type(&self) -> KeyStoreType;
}

/// File-based key storage - stores key in plain text file.
/// On Linux and MacOS, the file is protected with mode 600, but the file is not encrypted,
/// and generally this method of key storage is far less secure than the OS-based keyring store.
#[derive(Clone, Debug)]
pub struct KeyStoreFile {
    path: PathBuf,
}

/// OS-native secure credential store (Keychain/Secret Service/Credential Manager)
/// Parameter is service name
/// ```rust
/// use anytype::prelude::*;
///
/// # fn create_keystore() -> Result<KeyStoreFile, AnytypeError> {
/// // create keyring-based keystore
/// let my_app = "my-app";
/// let keystore = KeyStoreKeyring::new(my_app, None);
///
/// // create file-based keystore with absolute path
/// use std::path::PathBuf;
/// let path = PathBuf::from("/var/lib/anytype/secret.key");
/// let keystore = KeyStoreFile::from_path(path)?;
/// # Ok(keystore)
/// # }
/// ```
#[derive(Clone, Debug)]
pub struct KeyStoreKeyring {
    service_name: String,
    user_name: String,
}

impl KeyStoreFile {
    /// Creates file-based key storage with path to configuration file.
    ///
    /// ```rust
    /// use anytype::prelude::*;
    /// # fn new_keystore() -> Result<KeyStoreFile, AnytypeError> {
    /// let keystore = KeyStoreFile::from_path("/var/lib/anytype/secret.key")?;
    /// # Ok(keystore)
    /// # }
    /// ```
    pub fn from_path(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_owned();
        // if the file doesn't exist, make sure there's a directory for storing it later
        if !path.is_file()
            && let Some(parent) = path.parent()
        {
            std::fs::create_dir_all(parent).context(FileSnafu { path: parent })?;
        }
        Ok(Self {
            path: path.to_owned(),
        })
    }

    /// Creates file-based keystore.
    ///
    /// If the environment variable 'ANYTYPE_KEY_FILE' is set, that path
    /// is used for the key file, otherwise the path is determined from the
    /// platform's config directory:
    ///   `(config-dir)/(service-name)/anytype_key`
    /// where config-dir is the default configuration dir for your platform
    ///       (see [dirs::config_dir](https://docs.rs/dirs/latest/dirs/fn.config_dir.html))
    /// and service_name is the parameter, or the default "anytype"
    ///
    ///
    /// ```rust
    /// use anytype::prelude::*;
    /// # fn new_keystore() -> Result<KeyStoreFile, AnytypeError> {
    /// let keystore = KeyStoreFile::new("my-app")?;
    /// # Ok(keystore)
    /// # }
    /// ```
    pub fn new(service_name: impl AsRef<str>) -> Result<Self> {
        if let Ok(path) = std::env::var(crate::config::ANYTYPE_KEY_FILE_ENV) {
            // if path is in environment var, overrides config dir and ignore parameter
            return Self::from_path(path);
        }

        let config_dir = default_config_dir()?.join(service_name.as_ref());
        std::fs::create_dir_all(&config_dir).context(FileSnafu {
            path: config_dir.clone(),
        })?;
        let path = config_dir.join(DEFAULT_KEY_USER);
        Ok(KeyStoreFile { path })
    }

    pub fn store_type(&self) -> KeyStoreType {
        KeyStoreType::File
    }
}

impl KeyStoreKeyring {
    /// Creates key storage using OS keyring backend.
    /// `service_name` defaults to "anytype" or the app_name configured in the anytype client.
    /// `user_name` defaults to `anytype_key`.
    pub fn new(service_name: impl AsRef<str>, user_name: Option<String>) -> Self {
        let service_name = service_name.as_ref().to_string();
        let user_name = user_name.unwrap_or_else(|| DEFAULT_KEY_USER.to_string());
        KeyStoreKeyring {
            service_name,
            user_name,
        }
    }
}

impl KeyStore for KeyStoreFile {
    /// Load API key from storage
    ///
    /// Returns:
    /// - Some(SecretApiKey) if key was retrieved. The Contents of SecretApiKey cannot be read
    ///   by clients, but it can be passed to client.set_api_key(),
    ///   or saved to a different keystore.
    /// - None if key is not defined in keystore
    ///
    /// Errors:
    /// - KeyStoreError::ring: os keyring could not load key
    /// - AnytypeError::KeystoreFile: file store could not be read
    ///
    fn load_key(&self) -> std::result::Result<Option<SecretApiKey>, KeyStoreError> {
        if !self.path.exists() {
            return Ok(None);
        }
        let api_key = std::fs::read_to_string(&self.path)
            .context(FileSnafu {
                path: self.path.clone(),
            })?
            .trim()
            .to_string();
        if api_key.is_empty() {
            warn!("keystore file {:?} is empty", self.path.display());
            return Ok(None);
        }
        debug!(path=?self.path, "load_key: key found in file");
        Ok(Some(SecretApiKey::new(api_key)))
    }

    fn save_key(&self, api_key: &SecretApiKey) -> std::result::Result<(), KeyStoreError> {
        // Write the file
        std::fs::write(&self.path, &api_key.0).context(FileSnafu { path: &self.path })?;

        // Set file permissions to 0600 (owner read/write only)
        #[cfg(target_family = "unix")]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&self.path)
                .context(FileSnafu { path: &self.path })?
                .permissions();
            perms.set_mode(0o600);
            std::fs::set_permissions(&self.path, perms).context(FileSnafu { path: &self.path })?;
        }
        debug!(path=?self.path, "key saved");
        Ok(())
    }

    fn remove_key(&self) -> std::result::Result<(), KeyStoreError> {
        if self.path.exists() {
            std::fs::remove_file(&self.path).context(FileSnafu { path: &self.path })?;
        }
        debug!(path=?self.path, "key removed");
        Ok(())
    }

    fn store_type(&self) -> KeyStoreType {
        KeyStoreType::File
    }
}

impl KeyStore for KeyStoreKeyring {
    /// Load API key from storage
    ///
    /// Returns:
    /// - SecretApiKey if key was retrieved. The Contents of SecretApiKey cannot be read
    ///   by clients, but it can be passed to client.set_api_key(),
    ///   or saved to a different keystore.
    ///
    /// Errors:
    /// - AnytypeError::KeystoreEmpty: keystore contains no such key
    /// - AnytypeError::Keyring: os keyring could not load key
    /// - AnytypeError::KeystoreFile: file store could not be read
    ///
    fn load_key(&self) -> std::result::Result<Option<SecretApiKey>, KeyStoreError> {
        let entry =
            keyring::Entry::new(&self.service_name, &self.user_name).context(KeyringSnafu {
                service: &self.service_name,
                user: &self.user_name,
            })?;
        match entry.get_password() {
            Ok(password) => {
                debug!("key loaded from keyring");
                Ok(Some(SecretApiKey::new(password)))
            }
            Err(keyring::Error::NoEntry) => {
                debug!("key undefined in keyring");
                Ok(None)
            }
            Err(e) => {
                error!("keyring {e:?}");
                Err(KeyStoreError::Keyring {
                    source: e,
                    service: self.service_name.clone(),
                    user: self.user_name.clone(),
                })
            }
        }
    }

    fn save_key(&self, api_key: &SecretApiKey) -> std::result::Result<(), KeyStoreError> {
        let entry =
            keyring::Entry::new(&self.service_name, &self.user_name).context(KeyringSnafu {
                service: self.service_name.clone(),
                user: self.user_name.clone(),
            })?;
        entry.set_password(&api_key.0).context(KeyringSnafu {
            service: self.service_name.clone(),
            user: self.user_name.clone(),
        })?;
        debug!("key saved in keyring");
        Ok(())
    }

    fn remove_key(&self) -> std::result::Result<(), KeyStoreError> {
        let entry =
            keyring::Entry::new(&self.service_name, &self.user_name).context(KeyringSnafu {
                service: self.service_name.clone(),
                user: self.user_name.clone(),
            })?;
        match entry.delete_credential() {
            // NoEntry means it's already been deleted
            Ok(()) | Err(keyring::Error::NoEntry) => {
                debug!(service_name=?self.service_name, user_name=?self.user_name, "key removed from keyring");
                Ok(())
            }
            Err(e) => {
                error!(service_name=?self.service_name, user_name=?self.user_name, ?e, "key remove");
                Err(KeyStoreError::Keyring {
                    source: e,
                    service: self.service_name.clone(),
                    user: self.user_name.clone(),
                })
            }
        }
    }

    fn store_type(&self) -> KeyStoreType {
        KeyStoreType::Keyring
    }
}

/// Gets the os-native configuration directory.
/// (linux: "~/.config". macos: "~/Library/Application Support", etc.)
fn default_config_dir() -> std::result::Result<PathBuf, KeyStoreError> {
    use std::io;
    match dirs::config_dir() {
        Some(d) => Ok(d),
        None => match dirs::home_dir() {
            Some(d) => Ok(d.join(".config")),
            None => Err(KeyStoreError::File {
                source: io::Error::other("cannot determine config directory"),
                path: PathBuf::new(),
            }),
        },
    }
}

#[derive(Clone, Debug, Default)]
pub struct NoKeyStore {}

impl KeyStore for NoKeyStore {
    fn load_key(&self) -> std::result::Result<Option<SecretApiKey>, KeyStoreError> {
        Ok(None)
    }

    fn save_key(&self, _api_key: &SecretApiKey) -> std::result::Result<(), KeyStoreError> {
        Ok(())
    }

    fn remove_key(&self) -> std::result::Result<(), KeyStoreError> {
        Ok(())
    }

    fn store_type(&self) -> KeyStoreType {
        KeyStoreType::None
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::config::DEFAULT_SERVICE_NAME;

    // TODO: this test case checks too many things - should be split up
    #[test]
    fn test_file_storage_save_and_load() -> Result<()> {
        // Use a unique temp dir based on process id to avoid cleanup issues
        let temp_dir = std::env::temp_dir().join(format!(
            "anytype_rust_api_test_storage_{}",
            std::process::id()
        ));
        // Ensure clean start
        let _ = fs::remove_dir_all(&temp_dir);
        let file_path = temp_dir.join(&format!("{DEFAULT_SERVICE_NAME}.test.key"));
        let storage = KeyStoreFile::from_path(&file_path)?;

        // Initially no key
        let no_exist = storage.load_key()?;
        assert!(no_exist.is_none());

        // Save a key
        let test_key = "test-key-123";
        storage.save_key(&SecretApiKey::new(test_key))?;

        // Read the key from file directly to test save
        let load_key = storage.load_key();

        // check that return is Ok(Some(...))
        assert!(matches!(load_key, Ok(Some(_))));
        let load_key = load_key.unwrap().unwrap();

        assert!(load_key.check_key(test_key), "save+load returns same key");

        // Remove the key
        storage.remove_key()?;

        assert!(!file_path.exists(), "file deleted");

        // Key should be gone
        let check_file = storage.load_key();
        assert!(matches!(check_file, Ok(None)), "expected file removed");

        // Clean up
        fs::remove_dir_all(&temp_dir).ok();
        Ok(())
    }

    #[test]
    #[cfg(unix)]
    fn test_file_permissions() -> Result<()> {
        use std::os::unix::fs::PermissionsExt;

        // Use a unique temp dir based on process id to avoid cleanup issues
        let temp_dir = std::env::temp_dir().join(format!(
            "anytype_rust_api_test_perms_{}",
            std::process::id()
        ));
        // Ensure clean start
        let _ = fs::remove_dir_all(&temp_dir);
        let file_path = temp_dir.join(&format!("{DEFAULT_SERVICE_NAME}.test.key"));
        let storage = KeyStoreFile::from_path(&file_path)?;

        // Save a key
        let test_key = "test-key-123";
        storage.save_key(&SecretApiKey::new(test_key))?;

        // Check file permissions
        let metadata = fs::metadata(&file_path).unwrap();
        let permissions = metadata.permissions();
        assert_eq!(permissions.mode() & 0o777, 0o600);

        // Clean up
        fs::remove_dir_all(&temp_dir).ok();
        Ok(())
    }

    #[test]
    #[ignore] // Run with: cargo test -- --ignored
    fn test_keyring_storage_end_to_end() -> Result<()> {
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
        let user_name = "test_api_key";
        let storage = KeyStoreKeyring::new(service_name, Some(user_name.to_string()));

        // Clean up any existing test data first
        let _ = storage.remove_key();

        // Save a test key
        let test_key = "test-keyring-api-key-12345";
        storage.save_key(&SecretApiKey::new(test_key))?;

        // Add a small delay to ensure the keyring backend has time to persist
        // (some backends may not immediately flush to disk/keychain)
        std::thread::sleep(std::time::Duration::from_millis(100));

        // Load the key
        let loaded_key = storage.load_key()?.expect("loaded key");
        assert!(loaded_key.check_key(test_key), "load key from keyring");

        // if this fails, try:
        //   'auth login', 'auth status', 'auth logout'
        //   on macos, program may require code signing or explicit entitlements
        //   on linux, need gnome-keyring or KWallet daemon running
        //   on Windows, may have UAC/permission issues

        // Remove the key
        storage.remove_key().expect("Should remove from keyring");
        println!("✓ Removed test key from keyring");

        // Verify it's gone
        let after_delete = storage.load_key();
        assert!(
            matches!(after_delete, Ok(None)),
            "after removal from keyring"
        );
        Ok(())
    }
}
