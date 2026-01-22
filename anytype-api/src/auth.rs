//! Anytype client authentication
//!
//! Performs interactive authentication, and transfers keys to and from the key store.
//!
//! # Authentication Flow methods
//!
//! - [authenticate_interactive](AnytypeClient::authenticate_interactive) - all-in-one authenticate with desktop app (combines `create_auth_challenge` and `create_api_key`)
//! - [create_auth_challenge](AnytypeClient::create_auth_challenge) - auth flow part 1
//! - [create_api_key](AnytypeClient::create_api_key) - auth flow part 2
//! - [auth_status](AnytypeClient::auth_status) - check current HTTP/gRPC auth state
//! - [logout](AnytypeClient::logout) - discard api key
//!
//! # KeyStore methods
//!
//! - [clear_api_key](AnytypeClient::clear_api_key)
//! - [set_api_key](AnytypeClient::set_api_key)
//! - [get_key_store](AnytypeClient::get_key_store)
//!

use serde::{Deserialize, Serialize};
use tracing::debug;

use crate::{Result, prelude::*};

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

/// Status response from auth_status()
/// Contents subject to change
#[doc(hidden)]
#[derive(Clone, Debug, Serialize)]
pub struct AuthStatus {
    pub keystore: KeyStoreStatus,
    pub http: HttpStatus,
    #[cfg(feature = "grpc")]
    pub grpc: GrpcStatus,
}

/// Http auth status
/// Contents subject to change
#[doc(hidden)]
#[derive(Clone, Debug, Serialize)]
pub struct HttpStatus {
    pub url: String,
    pub has_token: bool,
}

impl HttpStatus {
    /// Returns true if the http client has an auth token
    /// To check whether the credentials are valid, use `client.ping_http()`
    pub fn is_authenticated(&self) -> bool {
        self.has_token
    }
}

/// gRPC auth status
/// Contents subject to change
#[cfg(feature = "grpc")]
#[doc(hidden)]
#[derive(Clone, Debug, Serialize)]
pub struct GrpcStatus {
    pub endpoint: String,
    pub has_account_key: bool,
    pub has_session_token: bool,
}

#[cfg(feature = "grpc")]
impl GrpcStatus {
    /// Returns true if the grpc client has either an account key or session token
    /// To check whether the credentials are valid, use `client.ping_grpc()`
    pub fn is_authenticated(&self) -> bool {
        self.has_account_key || self.has_session_token
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct KeyStoreStatus {
    pub id: String,
    pub service: String,
    /// path to file, if db-keystore (sqlite backend) is used
    pub path: Option<std::path::PathBuf>,
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
    /// `set_api_key` and save it to the keystore with
    /// `get_key_store().update_http_credentials(key)`
    ///
    /// Note: this is a low-level method: use `authenticate_interactive` for
    /// an all-in-one authentication.
    ///
    /// Parameters:
    ///   `challenge_id`: challenge id, example "67647f5ecda913e9a2e11b26"
    ///   `code`: 4-digit code from the desktop app, example `1234`
    /// Returns:
    ///   `Secret<HttpCredentials>`
    pub async fn create_api_key(
        &self,
        challenge_id: &str,
        code: impl Into<String>,
    ) -> Result<HttpCredentials> {
        let request = CreateApiKeyRequest {
            challenge_id: challenge_id.to_string(),
            code: code.into(),
        };
        let response: CreateApiKeyResponse = self
            .client
            .post_unauthenticated("/v1/auth/api_keys", &request)
            .await?;
        Ok(HttpCredentials::new(response.api_key))
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
    /// let mut config = ClientConfig::default().app_name("my-app");
    /// config.keystore = Some("file".to_string());
    /// let client = AnytypeClient::with_config(config)?;
    ///
    /// client
    ///     .authenticate_interactive(
    ///         |challenge_id| {
    ///             use std::io::{self, Write};
    ///             println!("Challenge ID: {}", challenge_id);
    ///             print!("Enter 4-digit code displayed by app: ");
    ///             io::stdout().flush().map_err(|e| AnytypeError::Auth {
    ///                 message: e.to_string(),
    ///             })?;
    ///             let mut code = String::new();
    ///             io::stdin().read_line(&mut code).map_err(|e| AnytypeError::Auth {
    ///                 message: e.to_string(),
    ///             })?;
    ///             Ok(code.trim().to_string())
    ///         },
    ///         false,
    ///     )
    ///     .await?;
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
            let creds = self.keystore.get_http_credentials()?;
            if creds.has_creds() {
                self.client.set_api_key(creds);
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

        // save to keystore
        self.keystore.update_http_credentials(&api_key)?;

        // save to client
        self.set_api_key(api_key.clone());

        Ok(())
    }

    /// Returns the configured keystore.
    ///
    /// # Example
    /// ```rust,no_run
    /// use anytype::prelude::*;
    /// # fn example() -> Result<(), AnytypeError> {
    /// let mut config = ClientConfig::default().app_name("my-app");
    /// config.keystore = Some("file".to_string());
    /// let client = AnytypeClient::with_config(config)?;
    /// let keystore = client.get_key_store();
    /// println!("keystore id: {}", keystore.id());
    /// # Ok(())
    /// # }
    /// ```
    pub fn get_key_store(&self) -> &KeyStore {
        &self.keystore
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
    /// let api_key = HttpCredentials::new("api_key_value");
    /// client.set_api_key(api_key);
    /// # Ok(())
    /// # }
    /// ```
    pub fn set_api_key(&self, key: HttpCredentials) {
        self.client.set_api_key(key);
    }

    /// Clears client api key and removes key from configured key storage.
    /// Equivalent to calling `clear_api_key()` followed by `get_key_store().clear_http_credentials()`
    ///
    /// # Example
    /// ```rust,no_run
    /// use anytype::prelude::*;
    /// # fn example() -> Result<(), AnytypeError> {
    /// let mut config = ClientConfig::default().app_name("my-app");
    /// config.keystore = Some("file".to_string());
    /// let client = AnytypeClient::with_config(config)?;
    /// client.logout()?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn logout(&self) -> Result<()> {
        self.clear_api_key();
        self.keystore.clear_http_credentials()?;
        Ok(())
    }

    /// Returns information about connection configuration and keystore status
    pub fn auth_status(&self) -> Result<AuthStatus, AnytypeError> {
        let keystore = self.get_key_store();
        let http_creds = keystore.get_http_credentials()?;
        #[cfg(feature = "grpc")]
        let grpc_creds = keystore.get_grpc_credentials()?;
        let path = keystore
            .store()
            .as_any()
            .downcast_ref::<db_keystore::DbKeyStore>()
            .map(|store| store.path().to_owned());

        Ok(AuthStatus {
            keystore: KeyStoreStatus {
                id: keystore.id(),
                service: keystore.service().to_string(),
                path,
            },
            http: HttpStatus {
                url: self.get_http_endpoint().to_string(),
                has_token: http_creds.has_creds(),
            },
            #[cfg(feature = "grpc")]
            grpc: GrpcStatus {
                endpoint: self.get_grpc_endpoint().to_string(),
                has_account_key: grpc_creds.has_account_key(),
                has_session_token: grpc_creds.has_session_token(),
            },
        })
    }
}
