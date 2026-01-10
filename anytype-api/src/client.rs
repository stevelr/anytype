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

use std::sync::Arc;

use tracing::debug;

use crate::{
    ANYTYPE_DESKTOP_URL, Result,
    cache::AnytypeCache,
    config::{
        ANYTYPE_URL_ENV, DEFAULT_SERVICE_NAME, RATE_LIMIT_MAX_RETRIES_DEFAULT,
        RATE_LIMIT_MAX_RETRIES_ENV,
    },
    http_client::HttpClient,
    prelude::*,
    verify::VerifyConfig,
};

/// Configuration for the Anytype client. Defines endpoint url, validation limits, and other settings.
///
/// ```rust,no_run
/// use anytype::prelude::*;
/// # fn create_client() -> Result<AnytypeClient, AnytypeError> {
/// // create api client with file-based keystore and default configuration
/// let my_app = "my-app";
/// let client = AnytypeClient::new(my_app)?
///     .set_key_store(KeyStoreFile::new(my_app)?);
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
    pub base_url: String,

    /// Application name used for auth challenge. In application code,
    /// you may want to use `env!("CARGO_BIN_NAME")` to use the executable name, defined at compile time.
    pub app_name: String,

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

    /// Optional verification behavior for read-after-write.
    pub verify: Option<VerifyConfig>,
}

impl Default for ClientConfig {
    fn default() -> Self {
        ClientConfig {
            base_url: std::env::var(ANYTYPE_URL_ENV).unwrap_or(ANYTYPE_DESKTOP_URL.to_string()),
            app_name: DEFAULT_SERVICE_NAME.to_string(),
            limits: Default::default(),
            rate_limit_max_retries: std::env::var(RATE_LIMIT_MAX_RETRIES_ENV)
                .ok()
                .and_then(|value| value.parse::<u32>().ok())
                .unwrap_or(RATE_LIMIT_MAX_RETRIES_DEFAULT),
            disable_cache: false,
            verify: None,
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

    pub fn get_limits(&self) -> &ValidationLimits {
        &self.limits
    }

    pub fn get_verify_config(&self) -> Option<&VerifyConfig> {
        self.verify.as_ref()
    }
}

/// An ergonomic Anytype API client in Rust.
//#[derive(Clone)]
pub struct AnytypeClient {
    pub(crate) client: Arc<HttpClient>,
    pub(crate) config: ClientConfig,
    pub(crate) keystore: Arc<Box<dyn KeyStore>>,
    pub(crate) cache: Arc<AnytypeCache>,
}

impl std::fmt::Debug for AnytypeClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AnytypeClient")
            .field("config", &self.config)
            .field("keystore", &self.keystore)
            .field("cache", &self.cache)
            .finish()
    }
}

impl AnytypeClient {
    /// Creates a new client with default configuration.
    /// After creation, call `set_key_store` if you want persistent key storage.
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
    /// After creation, call `set_key_store` if you want persistent key storage.
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
    /// After creation, call `set_key_store` if you want persistent key storage.
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
    pub fn with_client(client: reqwest::ClientBuilder, config: ClientConfig) -> Result<Self> {
        debug!(url=?config.base_url, "new client");
        let client = HttpClient::new(
            client,
            config.base_url.clone(),
            config.limits.clone(),
            config.rate_limit_max_retries,
        )?;
        let cache = Arc::new(AnytypeCache::default());
        if config.disable_cache {
            cache.disable();
        }
        Ok(Self {
            client: Arc::new(client),
            config,
            keystore: Arc::new(Box::new(NoKeyStore::default())),
            cache,
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
    /// println!("base_url: {}", config.base_url);
    /// # Ok(())
    /// # }
    /// ```
    pub fn get_config(&self) -> &ClientConfig {
        &self.config
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
