//! Anytype client authentication
//!
//! Performs interactive authentication, and transfers keys to and from the key store.
//!
//! # Authentication Flow methods
//!
//! - [authenticate_interactive](AnytypeClient::authenticate_interactive) - all-in-one authenticate with desktop app (combines `create_auth_challenge` and `create_api_key`)
//! - [create_auth_challenge](AnytypeClient::create_auth_challenge) - auth flow part 1
//! - [create_api_key](AnytypeClient::create_api_key) - auth flow part 2
//! - [logout](AnytypeClient::logout) - discard api key
//! - [is_authenticated](AnytypeClient::is_authenticated) - test whether client has a valid api key
//!
//! # KeyStore methods
//!
//! - [load_key](AnytypeClient::load_key)
//! - [save_key](AnytypeClient::save_key)
//! - [clear_api_key](AnytypeClient::clear_api_key)
//! - [set_api_key](AnytypeClient::set_api_key)
//! - [set_key_store](AnytypeClient::set_key_store)
//! - [get_key_store](AnytypeClient::get_key_store)
//!

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tracing::{debug, info};

use crate::{Result, prelude::*};
use snafu::prelude::*;

/// Request to create an authentication challenge
#[derive(Debug, Serialize)]
struct CreateChallengeRequest {
    /// The name of the application requesting the challenge
    pub app_name: String,
}

/// Response containing challenge information
#[derive(Debug, Deserialize)]
struct CreateChallengeResponse {
    /// The unique identifier for the challenge
    pub challenge_id: String,
}

/// Request to create an API key using challenge response
#[derive(Debug, Serialize)]
struct CreateApiKeyRequest {
    /// The unique identifier for the challenge, returned from the challenge creation
    pub challenge_id: String,
    /// The 4-digit code provided by the user from the Anytype application in response to the challenge
    pub code: String,
}

/// Response from create_api_key
/// Example: `zhSG/zQRmgADyilWPtgdnfo1qD60oK02/SVgi1GaFt6=`
#[derive(Debug, Deserialize)]
struct CreateApiKeyResponse {
    /// API key that can be used in the Authorization header for subsequent requests
    pub api_key: String,
}

impl AnytypeClient {
    /// Generates a one-time authentication challenge for granting API
    /// access to the user's vault.
    ///
    /// Uses `ClientConfig.app_name` to identify the app, and causes the
    /// Anytype Desktop app to display a 4-digit code.
    /// After you receive the `challenge_id` from this method, and the code,
    /// call `create_api_key`
    ///
    /// Note: this is a low-level method: use `authenticate_interactive` for
    /// an all-in-one authentication.
    pub async fn create_auth_challenge(&self) -> Result<String> {
        let request = CreateChallengeRequest {
            app_name: self.config.app_name.clone(),
        };
        debug!("creating auth challenge ...");
        let response: CreateChallengeResponse = self
            .client
            .post_unauthenticated("/v1/auth/challenges", &request)
            .await?;
        debug!("challenge received: {}", &response.challenge_id);
        Ok(response.challenge_id)
    }

    /// Exchanges the challenge response for an API key.
    ///
    /// Invoke with the challenge_id returned by `create_auth_challenge`,
    /// and the 4-digit code from the user
    /// (displayed by the desktop app). If the challenge solution is correct,
    /// this method generates the api key.
    ///
    /// Your app should set this as the client api key with
    /// `set_api_key` and save it to the keystore with `get_key_store().save_key(key)`
    ///
    /// Note: this is a low-level method: use `authenticate_interactive` for
    /// an all-in-one authentication.
    ///
    /// Parameters:
    ///   `challenge_id`: challenge id, example "67647f5ecda913e9a2e11b26"
    ///   `code`: 4-digit code from the desktop app, example `1234`
    /// Returns:
    ///   `SecretApiKey`
    pub async fn create_api_key(
        &self,
        challenge_id: &str,
        code: impl Into<String>,
    ) -> Result<SecretApiKey> {
        let request = CreateApiKeyRequest {
            challenge_id: challenge_id.to_string(),
            code: code.into(),
        };
        let response: CreateApiKeyResponse = self
            .client
            .post_unauthenticated("/v1/auth/api_keys", &request)
            .await?;
        Ok(SecretApiKey::new(response.api_key))
    }

    /// Performs interactive authentication with Anytype app.
    ///
    /// This is a convenience method that:
    /// 1. Creates a challenge
    /// 2. Calls the provided closure to prompt the user for a code
    /// 3. Exchanges the code for an API key
    /// 4. Saves the api_key for this client
    /// 5. If KeyStore is configured, saves the key in the keystore
    ///
    /// # Arguments
    /// * `get_code` - Callback to obtain the 4-digit code from the user
    /// * `force_reauth` - ignore any existing keys, in client or keystore, and execute the interactive flow
    ///   to generate a new key.
    ///
    /// # Example
    /// ```no_run
    ///
    /// # use anytype::prelude::*;
    /// # async fn example() -> anytype::Result<()> {
    /// let mut client = AnytypeClient::new("my-app")?
    ///     .set_key_store(KeyStoreKeyring::new("my-app", None));
    ///
    /// client.authenticate_interactive(|challenge_id| {
    ///     println!("Challenge ID: {}", challenge_id);
    ///     // Prompt user to enter code
    ///     print!("Enter 4-digit code displayed by app: ");
    ///     let mut code = String::new();
    ///     std::io::stdin().read_line(&mut code).map_err(|e| AnytypeError::Auth { message: e.to_string() })?;
    ///     Ok(code.trim().to_string())
    /// }, false).await?;
    ///
    /// // Client is now authenticated
    /// # Ok(())
    /// # }
    /// ```
    pub async fn authenticate_interactive<F>(&self, get_code: F, force_reauth: bool) -> Result<()>
    where
        F: FnOnce(&str) -> Result<String>,
    {
        // the common code path is force_reauth==false: use key if we have one
        if !force_reauth {
            // if client has key already, no need to re-authenticate
            if self.client.has_key() {
                debug!("client already has key - no need to re-authenticate");
                return Ok(());
            }

            // if key is in keystore, no need to re-authenticate with Anytype Desktop/server
            // (user may still be prompted to authenticate with keystore)
            if self.get_key_store().is_configured()
                && let Ok(true) = self.load_key(force_reauth)
            {
                return Ok(());
            }
        }
        debug!("beginning interactive authentication");

        // Create challenge
        // App displays 4-digit code
        let challenge_id: String = self.create_auth_challenge().await?;

        // Prompt user for code
        let code = get_code(&challenge_id)?;

        // Create API key
        let api_key = self.create_api_key(&challenge_id, code).await?;

        // save key to client
        self.set_api_key(&api_key);

        if self.keystore.is_configured() {
            self.keystore.save_key(&api_key)?;
        } else {
            debug!(
                "authentication completed, but key not persisted because no keystore is configured."
            );
        }
        Ok(())
    }

    /// Configures the key storage. Must be called before using `authenticate_from_key_store`
    ///
    /// # Example
    /// ```rust,no_run
    /// use anytype::prelude::*;
    /// # fn create_client() -> Result<AnytypeClient, AnytypeError> {
    /// let key_store = KeyStoreKeyring::new("my-app", None);
    /// let client = AnytypeClient::new("my-app")?.set_key_store(key_store);
    /// # Ok(client)
    /// # }
    /// ```
    pub fn set_key_store<K: KeyStore + 'static>(mut self, keystore: K) -> Self {
        self.keystore = Arc::new(Box::new(keystore));
        self
    }

    /// Sets file-based keystore using ANYTYPE_KEY_FILE as path containing key file
    /// and attempts to load the key.
    /// Returns error if the environment variable is not set or if the path is not reachable.
    /// Used in rustdoc examples
    pub fn env_key_store(self) -> Result<Self> {
        let var = crate::config::ANYTYPE_KEY_FILE_ENV;
        let path = std::env::var(var).context(FileEnvSnafu { var })?;
        let client = self.set_key_store(KeyStoreFile::from_path(path)?);
        client.load_key(false)?;
        Ok(client)
    }

    /// Returns the configured keystore.
    ///
    /// # Example
    /// ```rust,no_run
    /// use anytype::prelude::*;
    /// # fn example() -> Result<(), AnytypeError> {
    /// let client = AnytypeClient::new("my-app")?
    ///     .set_key_store(KeyStoreFile::new("my-app")?);
    /// let keystore = client.get_key_store();
    /// println!("keystore configured: {}", keystore.is_configured());
    /// # Ok(())
    /// # }
    /// ```
    pub fn get_key_store(&self) -> Arc<Box<dyn KeyStore>> {
        self.keystore.clone()
    }

    /// Clears the client's API key.
    /// If the current key has become invalid and you need to re-authenticate,
    /// use `authenticate_interactive`, setting force=true
    /// To clear the client's key and remove key from keystore, use `logout`.
    ///
    /// # Example
    /// ```rust,no_run
    /// use anytype::prelude::*;
    /// # fn example() -> Result<(), AnytypeError> {
    /// let client = AnytypeClient::new("my-app")?;
    /// client.clear_api_key();
    /// # Ok(())
    /// # }
    /// ```
    pub fn clear_api_key(&self) {
        self.client.clear_api_key();
    }

    /// Sets the client's API key in memory for authenticated requests.
    ///
    /// # Example
    /// ```rust,no_run
    /// use anytype::prelude::*;
    /// # fn example() -> Result<(), AnytypeError> {
    /// let client = AnytypeClient::new("my-app")?;
    /// let api_key = SecretApiKey::new("api_key_value".to_string());
    /// client.set_api_key(&api_key);
    /// # Ok(())
    /// # }
    /// ```
    pub fn set_api_key(&self, key: &SecretApiKey) {
        self.client.set_api_key(key);
    }

    /// Clears client api key and removes key from configured key storage.
    /// Equivalent to calling `clear_api_key()` followed by `get_key_store().remove_key()`
    ///
    /// # Example
    /// ```rust,no_run
    /// use anytype::prelude::*;
    /// # fn example() -> Result<(), AnytypeError> {
    /// let client = AnytypeClient::new("my-app")?
    ///     .set_key_store(KeyStoreFile::new("my-app")?);
    /// client.logout()?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn logout(&self) -> Result<()> {
        self.clear_api_key();
        if self.keystore.is_configured() {
            self.keystore.remove_key()?;
        }
        Ok(())
    }

    /// Returns true if the client has a key, either because it authenticated this session,
    /// or because it loaded the key from the keystore.
    ///
    /// # Example
    /// ```rust,no_run
    /// use anytype::prelude::*;
    /// # fn example() -> Result<(), AnytypeError> {
    /// let client = AnytypeClient::new("my-app")?;
    /// if !client.is_authenticated() {
    ///     println!("Not authenticated yet.");
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub fn is_authenticated(&self) -> bool {
        self.client.has_key()
    }

    /// Attempts to load client's api key from keystore: file or keyring.
    /// For keyring keystores, the user may be prompted to give the app permission.
    ///
    /// # Parameters
    /// * `force_reload` - If true, always ask keystore for key. if false, and client
    ///   has the key already (from a previous `load_key` or `authenticate_interactive`),
    ///   returns true without checking keystore.
    ///
    /// # Returns
    /// * `Ok(true)`: key is available and client is authenticated
    /// * `Ok(false)`: keystore does not contain key
    ///
    /// # Errors
    /// * `NoKeyStore` - no Keystore is configured: client should initialize a KeyStore implementation
    /// * `KeyStore` - error loading key
    ///
    /// For keyring keystores, the most likely error causes are user failed biometric auth or
    /// entered wrong password.
    /// For file keystore, file may have been deleted.
    ///
    /// # Example
    /// ```no_run
    /// # use anytype::{prelude::*, Result};
    /// # async fn example() -> Result<()> {
    /// let client = AnytypeClient::new("my-app")?
    ///     .set_key_store(KeyStoreFile::new("my-app")?);
    /// if !client.load_key(false)? {
    ///     println!("Not authenticated. Please log in.");
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub fn load_key(&self, force_reload: bool) -> Result<bool> {
        // fast return if we already have the key
        if !force_reload && self.is_authenticated() {
            return Ok(true);
        }
        if !self.keystore.is_configured() {
            return Err(AnytypeError::NoKeyStore);
        }
        let key = self.keystore.load_key()?;
        if let Some(ref api_key) = key {
            self.set_api_key(api_key);
        } else {
            info!("key store: key not found");
        }
        Ok(key.is_some())
    }

    /// Saves current API key to configured key store
    ///
    /// # Example
    /// ```rust,no_run
    /// use anytype::prelude::*;
    /// # fn example() -> Result<(), AnytypeError> {
    /// let client = AnytypeClient::new("my-app")?
    ///     .set_key_store(KeyStoreFile::new("my-app")?);
    /// let api_key = SecretApiKey::new("api_key_value".to_string());
    /// client.set_api_key(&api_key);
    /// client.save_key()?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn save_key(&self) -> Result<()> {
        if !self.keystore.is_configured() {
            return Err(AnytypeError::NoKeyStore);
        }
        match self.client.get_api_key() {
            Some(key) => self.keystore.save_key(&key).map_err(AnytypeError::from),
            None => Err(AnytypeError::Auth {
                message: "No API key set on client".to_string(),
            }),
        }
    }
}
