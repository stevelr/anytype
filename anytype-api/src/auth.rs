//! Anytype client authentication
//!
//! Performs interactive authentication, and transfers keys to and from the key store.
//!
//! # Authentication Flow methods
//!
//! - [`authenticate_interactive`](AnytypeClient::authenticate_interactive) - all-in-one authenticate with desktop app (combines `create_auth_challenge` and `create_api_key`)
//! - [`create_auth_challenge`](AnytypeClient::create_auth_challenge) - auth flow part 1
//! - [`create_api_key`](AnytypeClient::create_api_key) - auth flow part 2
//! - [`auth_status`](AnytypeClient::auth_status) - check current HTTP/gRPC auth state
//! - [`logout`](AnytypeClient::logout) - discard api key
//!
//! # `KeyStore` methods
//!
//! - [`clear_api_key`](AnytypeClient::clear_api_key)
//! - [`set_api_key`](AnytypeClient::set_api_key)
//! - [`get_key_store`](AnytypeClient::get_key_store)
//!

use std::path::PathBuf;

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

/// Response from `create_api_key`
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
    #[must_use]
    pub fn is_authenticated(&self) -> bool {
        self.has_token
    }
}

/// gRPC auth status
/// Contents subject to change
#[doc(hidden)]
#[derive(Clone, Debug, Serialize)]
pub struct GrpcStatus {
    pub endpoint: Option<String>,
    pub has_account_key: bool,
    pub has_session_token: bool,
}

impl GrpcStatus {
    /// Returns true if the grpc client has either an account key or session token
    /// To check whether the credentials are valid, use `client.ping_grpc()`
    #[must_use]
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
    ///
    /// # Errors
    ///
    /// `AnytypeError::Http` for communication error
    /// `AnytypeError::ApiError` for malformed api request
    ///
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
    /// Invoke with the `challenge_id` returned by `create_auth_challenge`,
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
    /// # Parameters:
    ///   `challenge_id`: challenge id, example "67647f5ecda913e9a2e11b26"
    ///   `code`: 4-digit code from the desktop app, example `1234`
    ///
    /// # Returns:
    ///   `HttpCredentials`
    ///
    /// # Errors
    ///  `AnytypeError::Http` for communication error
    ///  `AnytypeError::ApiError` for malformed api request
    ///
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
    /// 4. Saves the `api_key` for this client
    /// 5. If `KeyStore` is configured, saves the key in the keystore
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
        self.set_api_key(api_key);

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
    #[must_use]
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
        self.keystore.clear_all_credentials()?;
        Ok(())
    }

    /// Returns information about connection configuration and keystore status
    pub fn auth_status(&self) -> Result<AuthStatus, AnytypeError> {
        let keystore = self.get_key_store();
        let http_creds = keystore.get_http_credentials()?;
        let grpc_creds = keystore.get_grpc_credentials()?;
        let path = keystore
            .store()
            .as_any()
            .downcast_ref::<db_keystore::DbKeyStore>()
            .map(|store| PathBuf::from(&store.path()));

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
            grpc: GrpcStatus {
                endpoint: self.get_grpc_endpoint(),
                has_account_key: grpc_creds.has_account_key(),
                has_session_token: grpc_creds.has_session_token(),
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use std::{
        io::ErrorKind,
        ops::Deref,
        path::{Path, PathBuf},
        sync::atomic::{AtomicU64, Ordering},
    };

    use reqwest::StatusCode;

    use super::*;
    use crate::{
        client::ClientConfig,
        error::AnytypeError,
        keystore::{GrpcCredentials, HttpCredentials},
        test_util::scripted_http::{
            ScriptedHttpContentType, ScriptedHttpFixture, ScriptedHttpRequest, ScriptedHttpResponse,
        },
    };

    static NEXT_SCRIPT_ID: AtomicU64 = AtomicU64::new(1);

    struct ScriptedClient {
        client: Option<AnytypeClient>,
        key_path: PathBuf,
    }

    impl Deref for ScriptedClient {
        type Target = AnytypeClient;

        fn deref(&self) -> &Self::Target {
            self.client
                .as_ref()
                .expect("scripted auth client remains available until cleanup")
        }
    }

    impl ScriptedClient {
        fn cleanup(mut self) -> Result<(), String> {
            let clear_result = self
                .client
                .as_ref()
                .ok_or_else(|| "scripted auth client was already cleaned".to_owned())?
                .get_key_store()
                .clear_all_credentials()
                .map_err(|error| format!("clear scripted auth credentials: {error}"));
            drop(self.client.take());
            let file_result = remove_scripted_keystore(&self.key_path);
            clear_result.and(file_result)
        }

        fn cleanup_after_keystore_failure(mut self) -> Result<(), String> {
            drop(self.client.take());
            remove_scripted_keystore(&self.key_path)
        }
    }

    impl Drop for ScriptedClient {
        fn drop(&mut self) {
            if let Some(client) = self.client.as_ref() {
                let _ = client.get_key_store().clear_all_credentials();
            }
            drop(self.client.take());
            let _ = remove_scripted_keystore(&self.key_path);
        }
    }

    fn remove_scripted_keystore(path: &Path) -> Result<(), String> {
        [
            path.to_path_buf(),
            PathBuf::from(format!("{}-shm", path.display())),
            PathBuf::from(format!("{}-wal", path.display())),
        ]
        .into_iter()
        .try_for_each(|path| match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
            Err(error) if error.kind() == ErrorKind::IsADirectory => std::fs::remove_dir(&path)
                .map_err(|error| {
                    format!(
                        "remove scripted auth keystore directory {}: {error}",
                        path.display()
                    )
                }),
            Err(error) => Err(format!(
                "remove scripted auth keystore {}: {error}",
                path.display()
            )),
        })
    }

    fn scripted_client(fixture: &ScriptedHttpFixture) -> ScriptedClient {
        let id = NEXT_SCRIPT_ID.fetch_add(1, Ordering::Relaxed);
        let key_path = std::env::temp_dir().join(format!(
            "anytype-auth-scripted-{}-{id}.db",
            std::process::id()
        ));
        let mut config = ClientConfig::default().app_name("auth-scripted");
        config.base_url = Some(format!("http://{}", fixture.address()));
        config.keystore = Some(format!("file:path={}", key_path.display()));
        config.keystore_service = Some(format!("auth-scripted-{id}"));
        ScriptedClient {
            client: Some(AnytypeClient::with_config(config).expect("create scripted auth client")),
            key_path,
        }
    }

    fn response(status: StatusCode, body: &str) -> ScriptedHttpResponse {
        ScriptedHttpResponse::new(
            status,
            ScriptedHttpContentType::Json,
            body.as_bytes().to_vec(),
        )
    }

    fn request_json(request: &ScriptedHttpRequest) -> serde_json::Value {
        serde_json::from_slice(request.body()).expect("scripted auth request body is JSON")
    }

    fn assert_challenge_request(request: &ScriptedHttpRequest) {
        let body = request_json(request);
        assert_eq!(request.method(), "POST", "auth challenge uses POST");
        assert_eq!(
            request.path(),
            "/v1/auth/challenges",
            "auth challenge uses its documented path"
        );
        assert_eq!(
            body.as_object().map(serde_json::Map::len),
            Some(1),
            "auth challenge body has one field"
        );
        assert!(
            body.get("app_name").and_then(serde_json::Value::as_str) == Some("auth-scripted"),
            "auth challenge body identifies the configured application"
        );
    }

    fn assert_api_key_request(request: &ScriptedHttpRequest, challenge_id: &str, code: &str) {
        let body = request_json(request);
        assert_eq!(request.method(), "POST", "API key exchange uses POST");
        assert_eq!(
            request.path(),
            "/v1/auth/api_keys",
            "API key exchange uses its documented path"
        );
        assert_eq!(
            body.as_object().map(serde_json::Map::len),
            Some(2),
            "API key exchange body has two fields"
        );
        assert!(
            body.get("challenge_id").and_then(serde_json::Value::as_str) == Some(challenge_id),
            "API key exchange preserves the generated challenge ID"
        );
        assert!(
            body.get("code").and_then(serde_json::Value::as_str) == Some(code),
            "API key exchange forwards the callback code"
        );
    }

    fn has_http_token(client: &AnytypeClient, expected: &str) -> bool {
        client
            .get_key_store()
            .get_http_credentials()
            .map(|credentials| credentials.token().is_some_and(|token| token == expected))
            .unwrap_or(false)
    }

    fn has_in_memory_http_token(client: &AnytypeClient, expected: &str) -> bool {
        client
            .client
            .get_api_key()
            .token()
            .is_some_and(|token| token == expected)
    }

    #[tokio::test]
    async fn auth_endpoints_send_exact_unauthed_wire_requests() {
        let fixture = ScriptedHttpFixture::start(vec![
            response(StatusCode::OK, r#"{"challenge_id":"challenge-1"}"#),
            response(StatusCode::OK, r#"{"api_key":"scripted-api-key"}"#),
        ])
        .await
        .expect("start auth script");
        let client = scripted_client(&fixture);

        let challenge = client
            .create_auth_challenge()
            .await
            .expect("create scripted challenge");
        let api_key = client
            .create_api_key(&challenge, "4938")
            .await
            .expect("create scripted API key");

        assert!(api_key.has_creds());
        let requests = fixture.finish().await.expect("finish auth script");
        assert_eq!(requests.len(), 2);
        assert_challenge_request(&requests[0]);
        assert_api_key_request(&requests[1], "challenge-1", "4938");
        client.cleanup().expect("remove scripted auth keystore");
    }

    #[tokio::test]
    async fn auth_endpoint_failures_preserve_api_error_mapping() {
        let fixture =
            ScriptedHttpFixture::start(vec![response(StatusCode::BAD_REQUEST, "invalid")])
                .await
                .expect("start auth failure script");
        let client = scripted_client(&fixture);

        let error = client
            .create_auth_challenge()
            .await
            .expect_err("bad challenge response fails");

        assert!(matches!(error, AnytypeError::ApiError { code: 400, .. }));
        let requests = fixture.finish().await.expect("finish auth failure script");
        assert_eq!(requests.len(), 1);
        assert!(requests[0].path() == "/v1/auth/challenges");
        client.cleanup().expect("remove scripted auth keystore");
    }

    #[tokio::test]
    async fn interactive_auth_uses_existing_client_or_keystore_credentials_without_callback() {
        let fixture = ScriptedHttpFixture::start(vec![response(StatusCode::OK, "{}")])
            .await
            .expect("start unused auth script");
        let client = scripted_client(&fixture);
        client.set_api_key(HttpCredentials::new("existing-client-key"));

        client
            .authenticate_interactive(
                |_| {
                    Err(AnytypeError::Auth {
                        message: "callback must not run".to_owned(),
                    })
                },
                false,
            )
            .await
            .expect("existing client credentials bypass authentication");
        assert!(has_in_memory_http_token(&client, "existing-client-key"));

        client.clear_api_key();
        client
            .get_key_store()
            .update_http_credentials(&HttpCredentials::new("stored-key"))
            .expect("store fixture credentials");
        client
            .authenticate_interactive(
                |_| {
                    Err(AnytypeError::Auth {
                        message: "callback must not run".to_owned(),
                    })
                },
                false,
            )
            .await
            .expect("stored credentials bypass authentication");
        assert!(has_in_memory_http_token(&client, "stored-key"));
        client.cleanup().expect("remove scripted auth keystore");
        drop(fixture);
    }

    #[tokio::test]
    async fn forced_interactive_auth_replaces_and_persists_credentials() {
        let fixture = ScriptedHttpFixture::start(vec![
            response(StatusCode::OK, r#"{"challenge_id":"challenge-2"}"#),
            response(StatusCode::OK, r#"{"api_key":"fresh-key"}"#),
        ])
        .await
        .expect("start forced auth script");
        let client = scripted_client(&fixture);
        client.set_api_key(HttpCredentials::new("old-key"));
        client
            .get_key_store()
            .update_http_credentials(&HttpCredentials::new("old-key"))
            .expect("store old credentials");

        client
            .authenticate_interactive(|_| Ok("8402".to_owned()), true)
            .await
            .expect("forced authentication succeeds");

        assert!(has_in_memory_http_token(&client, "fresh-key"));
        assert!(has_http_token(&client, "fresh-key"));
        let requests = fixture.finish().await.expect("finish forced auth script");
        assert_eq!(requests.len(), 2);
        client.cleanup().expect("remove scripted auth keystore");
    }

    #[tokio::test]
    async fn interactive_auth_callback_failure_keeps_existing_credentials() {
        let fixture = ScriptedHttpFixture::start(vec![response(
            StatusCode::OK,
            r#"{"challenge_id":"challenge-3"}"#,
        )])
        .await
        .expect("start callback failure script");
        let client = scripted_client(&fixture);
        client.set_api_key(HttpCredentials::new("old-key"));
        client
            .get_key_store()
            .update_http_credentials(&HttpCredentials::new("old-key"))
            .expect("store old credentials");

        let error = client
            .authenticate_interactive(
                |_| {
                    Err(AnytypeError::Auth {
                        message: "callback failed".to_owned(),
                    })
                },
                true,
            )
            .await
            .expect_err("callback failure stops authentication");

        assert!(matches!(error, AnytypeError::Auth { .. }));
        assert!(has_in_memory_http_token(&client, "old-key"));
        assert!(has_http_token(&client, "old-key"));
        let requests = fixture
            .finish()
            .await
            .expect("finish callback failure script");
        assert_eq!(requests.len(), 1);
        client.cleanup().expect("remove scripted auth keystore");
    }

    #[tokio::test]
    async fn interactive_auth_api_key_failure_keeps_existing_credentials() {
        let fixture = ScriptedHttpFixture::start(vec![
            response(StatusCode::OK, r#"{"challenge_id":"challenge-4"}"#),
            response(StatusCode::BAD_REQUEST, "invalid code"),
        ])
        .await
        .expect("start API key failure script");
        let client = scripted_client(&fixture);
        client.set_api_key(HttpCredentials::new("old-key"));
        client
            .get_key_store()
            .update_http_credentials(&HttpCredentials::new("old-key"))
            .expect("store old credentials");

        let error = client
            .authenticate_interactive(|_| Ok("0000".to_owned()), true)
            .await
            .expect_err("API key failure stops authentication");

        assert!(matches!(error, AnytypeError::ApiError { code: 400, .. }));
        assert!(has_in_memory_http_token(&client, "old-key"));
        assert!(has_http_token(&client, "old-key"));
        let requests = fixture
            .finish()
            .await
            .expect("finish API key failure script");
        assert_eq!(requests.len(), 2);
        client.cleanup().expect("remove scripted auth keystore");
    }

    #[tokio::test]
    async fn interactive_auth_persistence_failure_keeps_existing_memory_credentials() {
        let fixture = ScriptedHttpFixture::start(vec![
            response(StatusCode::OK, r#"{"challenge_id":"challenge-5"}"#),
            response(StatusCode::OK, r#"{"api_key":"unpersisted-key"}"#),
        ])
        .await
        .expect("start persistence failure script");
        let client = scripted_client(&fixture);
        client.set_api_key(HttpCredentials::new("old-key"));
        std::fs::remove_file(&client.key_path).expect("replace scripted keystore with a directory");
        std::fs::create_dir(&client.key_path).expect("block scripted keystore writes");

        let error = client
            .authenticate_interactive(|_| Ok("1138".to_owned()), true)
            .await
            .expect_err("keystore write failure stops authentication");

        assert!(matches!(error, AnytypeError::KeyStore { .. }));
        assert!(has_in_memory_http_token(&client, "old-key"));
        let requests = fixture
            .finish()
            .await
            .expect("finish persistence failure script");
        assert_eq!(requests.len(), 2);
        client
            .cleanup_after_keystore_failure()
            .expect("remove failed scripted auth keystore");
    }

    #[tokio::test]
    async fn auth_status_and_logout_report_keystore_transitions() {
        let fixture = ScriptedHttpFixture::start(vec![response(StatusCode::OK, "{}")])
            .await
            .expect("start unused status script");
        let client = scripted_client(&fixture);

        let initial = client.auth_status().expect("read initial auth status");
        assert!(!initial.http.is_authenticated());
        assert!(!initial.grpc.is_authenticated());
        assert!(initial.keystore.path.is_some());

        client.set_api_key(HttpCredentials::new("status-key"));
        client
            .get_key_store()
            .update_http_credentials(&HttpCredentials::new("status-key"))
            .expect("store status credentials");
        client
            .get_key_store()
            .update_grpc_credentials(&GrpcCredentials::from_token("status-grpc-token"))
            .expect("store gRPC status credentials");
        assert!(
            client
                .auth_status()
                .expect("read stored status")
                .http
                .is_authenticated()
        );
        assert!(
            client
                .auth_status()
                .expect("read stored gRPC status")
                .grpc
                .is_authenticated()
        );

        client.logout().expect("logout clears credentials");
        let logged_out = client.auth_status().expect("read logged out status");
        assert!(!client.deref().client.get_api_key().has_creds());
        assert!(!logged_out.http.is_authenticated());
        assert!(!logged_out.grpc.is_authenticated());
        client.cleanup().expect("remove scripted auth keystore");
        drop(fixture);
    }

    #[tokio::test]
    async fn ping_failures_are_classified_as_authentication_failures() {
        let fixture =
            ScriptedHttpFixture::start(vec![response(StatusCode::UNAUTHORIZED, "denied")])
                .await
                .expect("start ping failure script");
        let client = scripted_client(&fixture);
        client.set_api_key(HttpCredentials::new("ping-key"));

        let http_error = client.ping_http().await.expect_err("HTTP ping is denied");
        assert!(matches!(http_error, AnytypeError::Unauthorized));
        assert!(http_error.is_authentication());
        let requests = fixture.finish().await.expect("finish ping failure script");
        assert_eq!(requests.len(), 1);
        assert!(requests[0].method() == "GET");
        assert!(requests[0].path() == "/v1/spaces?limit=1");
        client.cleanup().expect("remove scripted auth keystore");

        let grpc_fixture = ScriptedHttpFixture::start(vec![response(StatusCode::OK, "{}")])
            .await
            .expect("start unused gRPC script");
        let grpc_client = scripted_client(&grpc_fixture);
        let grpc_error = grpc_client
            .ping_grpc()
            .await
            .expect_err("missing gRPC credentials reject ping");
        assert!(matches!(grpc_error, AnytypeError::GrpcUnavailable { .. }));
        assert!(grpc_error.is_authentication());
        grpc_client
            .cleanup()
            .expect("remove scripted gRPC auth keystore");
        drop(grpc_fixture);
    }
}
