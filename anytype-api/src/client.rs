//! Anytype Rust API Client
//!
//! # Creating new api client
//!
//! - [new](AnytypeClient::new) - create new client
//! - [with_config](AnytypeClient::with_config) - create client with custom configuration
//! - [with_client](AnytypeClient::with_client) - create client with configuration and custom reqwest client
//!
//! # Configuration
//!
//! - [get_config](AnytypeClient::get_config) - returns configuration
//! - [api_version](AnytypeClient::api_version) - returns current anytype api version
//!
//!

#[cfg(feature = "grpc")]
use anytype_rpc::client::default_grpc_endpoint;
#[cfg(feature = "grpc")]
use snafu::prelude::*;
use std::sync::Arc;

use crate::{
    ANYTYPE_DESKTOP_URL,
    Result,
    //cache::AnytypeCache,
    config::{
        ANYTYPE_URL_ENV, DEFAULT_SERVICE_NAME, RATE_LIMIT_MAX_RETRIES_DEFAULT,
        RATE_LIMIT_MAX_RETRIES_ENV,
    },
    http_client::HttpClient,
    prelude::*,
    //verify::VerifyConfig,
};
#[cfg(feature = "grpc")]
use anytype_rpc::client::{AnytypeGrpcClient, AnytypeGrpcConfig};
#[cfg(feature = "grpc")]
use tokio::sync::Mutex;

/// Configuration for the Anytype client. Defines endpoint url, validation limits, and other settings.
///
/// ```rust,no_run
/// use anytype::prelude::*;
/// # fn create_client() -> Result<AnytypeClient, AnytypeError> {
/// // create api client with file-based keystore and default configuration
/// let my_app = "my-app";
/// let mut config = ClientConfig::default().app_name(my_app);
/// config.keystore = Some("file".to_string());
/// let client = AnytypeClient::with_config(config)?;
/// # Ok(client)
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct ClientConfig {
    /// Base url for all anytype HTTP/REST api requests.
    /// If not provided in config, url is determined by:
    /// * The environment variable  ANYTYPE_URL, if defined, or
    /// * "http://127.0.0.1:31009" `anytype::ANYTYPE_DESKTOP_URL`
    ///
    /// If you are using the anytype headless client,
    /// you might want to use `anytype::ANYTYPE_HEADLESS_URL` "http://127.0.0.1:31012"
    pub base_url: Option<String>,

    /// Application name used for auth challenge. In application code,
    /// you may want to use `env!("CARGO_BIN_NAME")` to use the executable name, defined at compile time.
    pub app_name: String,

    /// keystore. Defaults to platform keyring service.
    /// To use file (sqlite)-based service instead of keyring,
    /// set to "file" (for default path, usually ~/.local/state/) or "file:path=/path/to/store"
    pub keystore: Option<String>,

    /// optional keystore service name. Defaults to app_name.
    pub keystore_service: Option<String>,

    /// Limits for sanity checking.
    /// To support pages greater than 10MB, increase limits.markdown_max_len.
    pub limits: ValidationLimits,

    /// Maximum consecutive 429 retries before failing (0 disables the cap).
    ///
    /// When the anytype server rate limit is exceeded and responds with http 429 status,
    /// the http client in this library throttles requests (to 1 per second)
    /// until the server stops returning errors, or up to `rate_limit_max_retries` times
    /// before giving up and returning an error to the client. This setting can be increased
    /// to handle arbitrary-sized bursts, with the result that the app may spend more time waiting.
    /// If `rate_limit_max_retries` is 0, the http client will always wait and retry.
    ///
    /// Defaults to RATE_LIMIT_MAX_RETRIES_DEFAULT, or the env override if set:
    /// ANYTYPE_RATE_LIMIT_MAX_RETRIES.
    pub rate_limit_max_retries: u32,

    /// Disable in-memory caches for spaces, properties, and types.
    pub disable_cache: bool,

    /// Optional verification behavior for read-after-write. None disables verification.
    pub verify: Option<VerifyConfig>,

    /// Optional gRPC endpoint (overrides default).
    #[cfg(feature = "grpc")]
    pub grpc_endpoint: Option<String>,
}

impl Default for ClientConfig {
    fn default() -> Self {
        ClientConfig {
            base_url: None,
            app_name: DEFAULT_SERVICE_NAME.to_string(),
            limits: Default::default(),
            rate_limit_max_retries: std::env::var(RATE_LIMIT_MAX_RETRIES_ENV)
                .ok()
                .and_then(|value| value.parse::<u32>().ok())
                .unwrap_or(RATE_LIMIT_MAX_RETRIES_DEFAULT),
            disable_cache: false,
            verify: None,
            keystore: None,
            keystore_service: None,
            #[cfg(feature = "grpc")]
            grpc_endpoint: None,
        }
    }
}

impl ClientConfig {
    /// Sets the app_name.
    pub fn app_name(self, app_name: &str) -> Self {
        ClientConfig {
            app_name: app_name.to_string(),
            ..self
        }
    }

    pub fn limits(self, limits: ValidationLimits) -> Self {
        ClientConfig { limits, ..self }
    }

    pub fn disable_cache(self, disable_cache: bool) -> Self {
        ClientConfig {
            disable_cache,
            ..self
        }
    }

    /// Enables read-after-write verification using the provided config.
    pub fn ensure_available(self, verify: VerifyConfig) -> Self {
        ClientConfig {
            verify: Some(verify),
            ..self
        }
    }

    /// Sets the verify config explicitly (None disables verification).
    pub fn verify_config(self, verify: Option<VerifyConfig>) -> Self {
        ClientConfig { verify, ..self }
    }

    /// Sets the gRPC endpoint (override default)
    #[cfg(feature = "grpc")]
    pub fn grpc_endpoint(mut self, endpoint: String) -> Self {
        self.grpc_endpoint = Some(endpoint);
        self
    }

    pub fn get_limits(&self) -> &ValidationLimits {
        &self.limits
    }

    pub fn get_verify_config(&self) -> Option<&VerifyConfig> {
        self.verify.as_ref()
    }
}

/// An ergonomic Anytype API client in Rust.
pub struct AnytypeClient {
    pub(crate) client: Arc<HttpClient>,
    pub(crate) config: ClientConfig,
    pub(crate) keystore: KeyStore,
    pub(crate) cache: Arc<AnytypeCache>,
    #[cfg(feature = "grpc")]
    pub(crate) grpc: Mutex<Option<AnytypeGrpcClient>>,
}

impl std::fmt::Debug for AnytypeClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AnytypeClient")
            .field("config", &self.config)
            .field("keystore:service", &self.keystore.service().to_string())
            .field("cache", &self.cache)
            .finish()
    }
}

impl AnytypeClient {
    /// Creates a new client with default configuration.
    /// Configure `ClientConfig.keystore` if you want file-based credential storage.
    ///
    /// # Example
    /// ```rust,no_run
    /// use anytype::prelude::*;
    /// # fn create_client() -> Result<AnytypeClient, AnytypeError> {
    /// let client = AnytypeClient::new("my-app")?;
    /// # Ok(client)
    /// # }
    /// ```
    pub fn new(app_name: &str) -> Result<Self> {
        Self::with_config(ClientConfig::default().app_name(app_name))
    }

    /// Creates a new client with the provided configuration.
    /// Configure `ClientConfig.keystore` if you want file-based credential storage.
    ///
    /// # Example
    /// ```rust,no_run
    /// use anytype::prelude::*;
    /// # fn create_client() -> Result<AnytypeClient, AnytypeError> {
    /// let config = ClientConfig::default().app_name("my-app");
    /// let client = AnytypeClient::with_config(config)?;
    /// # Ok(client)
    /// # }
    /// ```
    pub fn with_config(config: ClientConfig) -> Result<Self> {
        let client = reqwest::Client::builder().no_proxy();
        Self::with_client(client, config)
    }

    /// Creates a client from a `reqwest::ClientBuilder` and configuration.
    /// ClientBuilder can be customized with timeouts, proxies, dns servers, user_agent, etc.
    /// Configure `ClientConfig.keystore` if you want file-based credential storage.
    ///
    /// # Example
    /// ```rust,no_run
    /// use anytype::prelude::*;
    /// # fn create_client() -> Result<AnytypeClient, AnytypeError> {
    /// let config = ClientConfig::default().app_name("my-app");
    /// let builder = reqwest::Client::builder().timeout(std::time::Duration::from_secs(10));
    /// let client = AnytypeClient::with_client(builder, config)?;
    /// # Ok(client)
    /// # }
    /// ```
    pub fn with_client(builder: reqwest::ClientBuilder, config: ClientConfig) -> Result<Self> {
        let base_url = config
            .base_url
            .clone()
            .unwrap_or(std::env::var(ANYTYPE_URL_ENV).unwrap_or(ANYTYPE_DESKTOP_URL.to_string()));
        let keystore_service = config.keystore_service.unwrap_or(config.app_name.clone());
        let keystore = KeyStore::new(&keystore_service, config.keystore.as_deref().unwrap_or(""))?;
        #[cfg(feature = "grpc")]
        let grpc_endpoint = config.grpc_endpoint.unwrap_or(default_grpc_endpoint());

        // ask keystore for http creds: this may trigger user auth for os keyring keystore
        let http_creds = keystore.get_http_credentials()?;

        let http_client = HttpClient::new(
            builder,
            base_url.clone(),
            config.limits.clone(),
            config.rate_limit_max_retries,
            http_creds,
        )?;
        let cache = if config.disable_cache {
            AnytypeCache::new_disabled()
        } else {
            AnytypeCache::default()
        };

        Ok(Self {
            client: Arc::new(http_client),
            // update config with _actual_ values so get_config() will give correct values
            config: ClientConfig {
                // base_url, keystore_service, and grpc_endpoint are always Some(...)
                // ... None values were replaced with defaults from environment or constants
                base_url: Some(base_url),
                keystore_service: Some(keystore_service),
                #[cfg(feature = "grpc")]
                grpc_endpoint: Some(grpc_endpoint),
                // other values unchanged
                ..config
            },
            keystore,
            cache: Arc::new(cache),
            #[cfg(feature = "grpc")]
            grpc: Mutex::new(None),
        })
    }

    /// Returns the configuration.
    ///
    /// # Example
    /// ```rust,no_run
    /// use anytype::prelude::*;
    /// # fn example() -> Result<(), AnytypeError> {
    /// let client = AnytypeClient::new("my-app")?;
    /// let config = client.get_config();
    /// println!("base_url: {:?}", config.base_url);
    /// # Ok(())
    /// # }
    /// ```
    pub fn get_config(&self) -> &ClientConfig {
        &self.config
    }

    /// Returns the configured http endpoint
    pub fn get_http_endpoint(&self) -> &str {
        &self.client.base_url
    }

    /// Returns the configured grpc endpoint
    #[cfg(feature = "grpc")]
    pub fn get_grpc_endpoint(&self) -> &str {
        // SAFETY: unwrap ok because grpc_endpoint is always Some(...)
        self.config.grpc_endpoint.as_deref().unwrap()
    }

    /// Returns the anytype api version, for example: "2025-11-08".
    ///
    /// # Example
    /// ```rust,no_run
    /// use anytype::prelude::*;
    /// # fn example() -> Result<(), AnytypeError> {
    /// let client = AnytypeClient::new("my-app")?;
    /// println!("api version: {}", client.api_version());
    /// # Ok(())
    /// # }
    /// ```
    pub fn api_version(&self) -> String {
        crate::ANYTYPE_API_VERSION.to_string()
    }

    /// Returns a gRPC client authorized using credentials stored in the keystore.
    ///
    /// Requires the "grpc" feature and gRPC credentials saved to the keystore.
    #[cfg(feature = "grpc")]
    pub async fn grpc_client(&self) -> Result<AnytypeGrpcClient> {
        let guard = self.grpc.lock().await;
        if let Some(client) = guard.as_ref() {
            return Ok(client.clone());
        }
        drop(guard);

        let grpc_config = if let Some(endpoint) = &self.config.grpc_endpoint {
            AnytypeGrpcConfig::new(endpoint)
        } else {
            AnytypeGrpcConfig::default()
        };

        self.create_grpc_client(&grpc_config).await?;
        let guard = self.grpc.lock().await;
        guard.as_ref().cloned().context(GrpcUnavailableSnafu {
            message: "gRPC client was not created".to_string(),
        })
    }

    /// Minimal authenticated HTTP ping (list spaces with limit 1).
    pub async fn ping_http(&self) -> Result<()> {
        let _ = self.spaces().limit(1).list().await?;
        Ok(())
    }

    /// Create and cache a gRPC client using credentials stored in the keystore.
    #[cfg(feature = "grpc")]
    async fn create_grpc_client(&self, config: &AnytypeGrpcConfig) -> Result<()> {
        let creds = self.keystore.get_grpc_credentials()?;
        let client = if let Some(token) = creds.session_token() {
            AnytypeGrpcClient::from_token(config, token.to_string())
                .await
                .context(GrpcSnafu)?
        } else if let Some(account_key) = creds.account_key() {
            AnytypeGrpcClient::from_account_key(config, account_key.to_string())
                .await
                .context(GrpcSnafu)?
        } else {
            return GrpcUnavailableSnafu {
                message: "no grpc token or account key in keystore".to_string(),
            }
            .fail();
        };

        let mut guard = self.grpc.lock().await;
        *guard = Some(client);
        Ok(())
    }

    /// Minimal authenticated gRPC ping (list apps).
    #[cfg(feature = "grpc")]
    pub async fn ping_grpc(&self) -> Result<()> {
        use anytype_rpc::anytype::rpc::account::local_link::list_apps::Request as ListAppsRequest;
        use anytype_rpc::auth::with_token;
        use tonic::Request;

        let grpc = self.grpc_client().await?;
        let mut commands = grpc.client_commands();
        let request = Request::new(ListAppsRequest {});
        let request = with_token(request, grpc.token()).map_err(|err| AnytypeError::Auth {
            message: err.to_string(),
        })?;
        let response = commands
            .account_local_link_list_apps(request)
            .await
            .map_err(|status| AnytypeError::Other {
                message: format!("gRPC request failed: {status}"),
            })?
            .into_inner();

        if let Some(error) = response.error
            && error.code != 0
        {
            return Err(AnytypeError::Other {
                message: format!(
                    "grpc list apps failed: {} (code {})",
                    error.description, error.code
                ),
            });
        }

        Ok(())
    }

    /// Returns a snapshot of current HTTP metrics.
    ///
    /// These metrics track HTTP requests made to the API server:
    /// - `total_requests`: Number of HTTP requests sent
    /// - `successful_responses`: Number of successful (2xx) responses
    /// - `errors`: Number of error responses (excluding rate limit errors)
    /// - `retries`: Number of retry attempts
    /// - `bytes_sent`: Total bytes sent in request bodies
    /// - `bytes_received`: Total bytes received in response bodies
    /// - `rate_limit_errors`: Number of rate limit (429) responses received
    /// - `rate_limit_delay_secs`: Total seconds spent waiting for rate limit backoff
    ///
    /// Note: Cached responses do not increment request counters.
    ///
    /// # Example
    /// ```rust,no_run
    /// use anytype::prelude::*;
    /// # async fn example() -> Result<(), AnytypeError> {
    /// let client = AnytypeClient::new("my-app")?;
    /// // ... make some API calls ...
    /// let metrics = client.http_metrics();
    /// println!("Total requests: {}", metrics.total_requests);
    /// println!("Successful: {}", metrics.successful_responses);
    /// # Ok(())
    /// # }
    /// ```
    pub fn http_metrics(&self) -> HttpMetricsSnapshot {
        self.client.metrics_snapshot()
    }

    /// Enables cache.
    /// Cache is always cleared if disabled and re-enabled, to ensure it's not stale
    pub fn enable_cache(&self) {
        self.cache.enable();
    }

    /// Disables cache
    pub fn disable_cache(&self) {
        self.cache.disable();
    }

    /// Returns true if the cache is enabled
    pub fn cache_is_enabled(&self) {
        self.cache.is_enabled();
    }
}

impl AnytypeClient {
    // accessor to support cache tests
    #[doc(hidden)]
    pub fn cache(&self) -> Arc<AnytypeCache> {
        self.cache.clone()
    }
}
