//! Anytype Rust API Client
//!
//! # Creating new api client
//!
//! - [new](AnytypeClient::new) - create new client
//! - [`with_config`](AnytypeClient::with_config) - create client with custom configuration
//! - [`with_client`](AnytypeClient::with_client) - create client with configuration and custom reqwest client
//!
//! # Configuration
//!
//! - [`get_config`](AnytypeClient::get_config) - returns configuration
//! - [`api_version`](AnytypeClient::api_version) - returns current anytype api version
//!
//!

use std::{future::Future, sync::Arc};

use anytype_rpc::client::default_grpc_endpoint;
use anytype_rpc::client::{AnytypeGrpcClient, AnytypeGrpcConfig};
use anytype_rpc::deadline::GrpcTimeoutPolicy;
use tokio::sync::Mutex;
use tracing::debug;

use crate::{
    ANYTYPE_DESKTOP_URL, Result,
    config::{
        ANYTYPE_URL_ENV, DEFAULT_SERVICE_NAME, RATE_LIMIT_MAX_RETRIES_DEFAULT,
        RATE_LIMIT_MAX_RETRIES_ENV,
    },
    http_client::{HttpClient, diagnostic_path},
    http_timeout::HttpTimeoutPolicy,
    prelude::*,
};

/// Byte ceilings for HTTP response bodies buffered by the client.
///
/// JSON, error bodies, and chat SSE events are read incrementally and rejected
/// as soon as they exceed the selected ceiling. File downloads use a separate
/// limit because their binary payloads are commonly much larger than API JSON
/// responses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResponseLimits {
    /// Default ceiling for buffered JSON API responses (8 MiB).
    pub json_bytes: u64,
    /// Default ceiling for a single object/document JSON response (64 MiB).
    pub document_bytes: u64,
    /// Ceiling for a buffered upstream error body (64 KiB).
    pub error_bytes: u64,
    /// Ceiling for a buffered raw file-download response (256 MiB).
    pub file_bytes: u64,
    /// Ceiling for one buffered chat SSE event, including its delimiter (1 MiB).
    pub chat_sse_event_bytes: u64,
}

/// Hard maximum accepted for a configured generic JSON response ceiling.
pub const MAX_JSON_RESPONSE_BYTES: u64 = 64 * 1024 * 1024;
/// Hard maximum accepted for a configured document response ceiling.
pub const MAX_DOCUMENT_RESPONSE_BYTES: u64 = 64 * 1024 * 1024;
/// Hard maximum accepted for a configured error response ceiling.
pub const MAX_ERROR_RESPONSE_BYTES: u64 = 1024 * 1024;
/// Hard maximum accepted for a configured raw file response ceiling.
pub const MAX_FILE_RESPONSE_BYTES: u64 = 1024 * 1024 * 1024;
/// Hard maximum accepted for a configured chat SSE event ceiling.
pub const MAX_CHAT_SSE_EVENT_BYTES: u64 = 64 * 1024 * 1024;

impl Default for ResponseLimits {
    fn default() -> Self {
        Self {
            json_bytes: 8 * 1024 * 1024,
            // A valid 10 MiB outgoing markdown body may expand substantially
            // when represented as an escaped JSON string in the response.
            document_bytes: MAX_DOCUMENT_RESPONSE_BYTES,
            error_bytes: 64 * 1024,
            file_bytes: 256 * 1024 * 1024,
            chat_sse_event_bytes: 1024 * 1024,
        }
    }
}

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
#[derive(Clone)]
pub struct ClientConfig {
    /// Base url for all anytype HTTP/REST api requests.
    /// If not provided in config, url is determined by:
    /// * The environment variable  `ANYTYPE_URL`, if defined, or
    /// * <http://127.0.0.1:31009> `anytype::ANYTYPE_DESKTOP_URL`
    ///
    /// If you are using the anytype headless client,
    /// you might want to use `anytype::ANYTYPE_HEADLESS_URL` <http://127.0.0.1:31012>
    pub base_url: Option<String>,

    /// Application name used for auth challenge. In application code,
    /// you may want to use `env!("CARGO_BIN_NAME")` to use the executable name, defined at compile time.
    pub app_name: String,

    /// keystore. Defaults to platform keyring service.
    /// To use file (sqlite)-based service instead of keyring,
    /// set to "file" (for default path, usually ~/.local/state/) or `file:path=/path/to/store`
    pub keystore: Option<String>,

    /// optional keystore service name. Defaults to `app_name`.
    pub keystore_service: Option<String>,

    /// Limits for sanity checking.
    /// To support pages greater than 10MB, increase `limits.markdown_max_len`.
    pub limits: ValidationLimits,

    /// Finite byte ceilings for buffered HTTP response bodies.
    ///
    /// Use [`ResponseLimits::default`] unless the server is known to return
    /// larger documents, files, or individual chat events. Chat streams are
    /// incremental, but each pending SSE event is bounded by
    /// [`ResponseLimits::chat_sse_event_bytes`].
    pub response_limits: ResponseLimits,

    /// Logical HTTP deadline policy.
    ///
    /// `None` inherits `ANYTYPE_HTTP_TIMEOUT_SECS` and then the library
    /// defaults. A supplied policy ignores that environment variable; `None`
    /// fields inside it disable their individual boundaries.
    pub http_timeouts: Option<HttpTimeoutPolicy>,

    /// Logical gRPC deadline policy.
    ///
    /// `None` inherits `ANYTYPE_GRPC_TIMEOUT_SECS` and then the library
    /// defaults. A supplied policy ignores that environment variable; `None`
    /// fields inside it disable their individual boundaries.
    pub grpc_timeouts: Option<GrpcTimeoutPolicy>,

    /// Maximum consecutive 429 retries before failing for replay-safe HTTP
    /// methods (0 disables this rate-limit-specific cap).
    ///
    /// When the anytype server rate limit is exceeded and responds with http 429 status,
    /// the HTTP client throttles and retries replay-safe methods
    /// until the server stops returning errors, or up to `rate_limit_max_retries` times
    /// before giving up and returning an error to the client. This setting can be increased
    /// to handle arbitrary-sized bursts, with the result that the app may spend more time waiting.
    /// Every replay-safe logical operation has a separate hard ceiling of six
    /// physical attempts across all automatic retry classes. Mutation methods
    /// such as `POST` and `PATCH` are never replayed automatically; their 429
    /// response is returned to the caller so application-level recovery can
    /// determine whether to retry.
    ///
    /// Defaults to `RATE_LIMIT_MAX_RETRIES_DEFAULT`, or the env override if set:
    /// `ANYTYPE_RATE_LIMIT_MAX_RETRIES`.
    pub rate_limit_max_retries: u32,

    /// Disable in-memory caches for spaces, properties, and types.
    pub disable_cache: bool,

    /// Optional verification behavior for read-after-write. None disables verification.
    pub verify: Option<VerifyConfig>,

    /// Optional gRPC endpoint (overrides default).
    pub grpc_endpoint: Option<String>,
}

impl std::fmt::Debug for ClientConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ClientConfig")
            .field("base_path", &self.base_url.as_deref().map(diagnostic_path))
            .field("keystore_configured", &self.keystore.is_some())
            .field(
                "keystore_service_configured",
                &self.keystore_service.is_some(),
            )
            .field("limits", &self.limits)
            .field("response_limits", &self.response_limits)
            .field("http_timeouts", &self.http_timeouts)
            .field("grpc_timeouts", &self.grpc_timeouts)
            .field("rate_limit_max_retries", &self.rate_limit_max_retries)
            .field("disable_cache", &self.disable_cache)
            .field("verify_configured", &self.verify.is_some())
            .field(
                "grpc_path",
                &self.grpc_endpoint.as_deref().map(diagnostic_path),
            )
            .finish()
    }
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            base_url: None,
            app_name: DEFAULT_SERVICE_NAME.to_string(),
            limits: ValidationLimits::default(),
            response_limits: ResponseLimits::default(),
            http_timeouts: None,
            grpc_timeouts: None,
            rate_limit_max_retries: std::env::var(RATE_LIMIT_MAX_RETRIES_ENV)
                .ok()
                .and_then(|value| value.parse::<u32>().ok())
                .unwrap_or(RATE_LIMIT_MAX_RETRIES_DEFAULT),
            disable_cache: false,
            verify: None,
            keystore: None,
            keystore_service: None,
            grpc_endpoint: None,
        }
    }
}

impl ClientConfig {
    /// Sets the `app_name`.
    #[must_use]
    pub fn app_name(self, app_name: &str) -> Self {
        Self {
            app_name: app_name.to_string(),
            ..self
        }
    }

    #[must_use]
    pub fn limits(self, limits: ValidationLimits) -> Self {
        Self { limits, ..self }
    }

    /// Sets an explicit logical HTTP deadline policy.
    ///
    /// Explicit policy ignores `ANYTYPE_HTTP_TIMEOUT_SECS`. Use `None` on an
    /// individual policy field to disable that boundary.
    #[must_use]
    pub fn http_timeouts(self, policy: HttpTimeoutPolicy) -> Self {
        Self {
            http_timeouts: Some(policy),
            ..self
        }
    }

    /// Sets an explicit logical gRPC deadline policy.
    ///
    /// Explicit policy ignores `ANYTYPE_GRPC_TIMEOUT_SECS`. Use `None` on an
    /// individual policy field to disable that boundary.
    #[must_use]
    pub fn grpc_timeouts(self, policy: GrpcTimeoutPolicy) -> Self {
        Self {
            grpc_timeouts: Some(policy),
            ..self
        }
    }

    #[must_use]
    pub fn disable_cache(self, disable_cache: bool) -> Self {
        Self {
            disable_cache,
            ..self
        }
    }

    /// Enables read-after-write verification using the provided config.
    #[must_use]
    pub fn ensure_available(self, verify: VerifyConfig) -> Self {
        Self {
            verify: Some(verify),
            ..self
        }
    }

    /// Sets the verify config explicitly (None disables verification).
    #[must_use]
    pub fn verify_config(self, verify: Option<VerifyConfig>) -> Self {
        Self { verify, ..self }
    }

    /// Sets the gRPC endpoint (override default)
    #[must_use]
    pub fn grpc_endpoint(mut self, endpoint: String) -> Self {
        self.grpc_endpoint = Some(endpoint);
        self
    }

    #[must_use]
    pub fn get_limits(&self) -> &ValidationLimits {
        &self.limits
    }

    #[must_use]
    pub fn get_verify_config(&self) -> Option<&VerifyConfig> {
        self.verify.as_ref()
    }
}

/// An ergonomic Anytype API client in Rust.
#[derive(Clone)]
pub struct AnytypeClient {
    pub(crate) client: Arc<HttpClient>,
    pub(crate) config: ClientConfig,
    pub(crate) keystore: KeyStore,
    pub(crate) cache: Arc<AnytypeCache>,
    pub(crate) grpc: Arc<Mutex<Option<AnytypeGrpcClient>>>,
    pub(crate) attached_discussion_metrics:
        Arc<crate::attached_discussions::AttachedDiscussionMetrics>,
    pub(crate) type_property_metrics: Arc<crate::types::TypePropertyClassificationMetrics>,
    pub(crate) collection_membership_metrics: Arc<crate::views::CollectionMembershipMetrics>,
}

impl std::fmt::Debug for AnytypeClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AnytypeClient")
            .field("config", &self.config)
            .field("keystore_configured", &true)
            .field("cache", &self.cache)
            .finish_non_exhaustive()
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
        let client = reqwest::Client::builder()
            .no_proxy()
            .connect_timeout(std::time::Duration::from_secs(30));
        Self::with_client(client, config)
    }

    /// Creates a client from a `reqwest::ClientBuilder` and configuration.
    /// `ClientBuilder` can be customized with timeouts, proxies, DNS servers,
    /// `user_agent`, and similar transport settings. Redirect and retry
    /// policies are always replaced: redirects are not followed and reqwest's
    /// lower-level retries are disabled so [`AnytypeClient`] can enforce its
    /// method-aware bounded retry policy without replaying mutation bodies.
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
        let resolved_http_timeouts = HttpTimeoutPolicy::resolve(config.http_timeouts)?;
        let resolved_grpc_timeouts =
            GrpcTimeoutPolicy::resolve(config.grpc_timeouts).map_err(|source| {
                AnytypeError::Validation {
                    message: source.to_string(),
                }
            })?;
        let base_url = config.base_url.clone().unwrap_or_else(|| {
            std::env::var(ANYTYPE_URL_ENV).unwrap_or_else(|_| ANYTYPE_DESKTOP_URL.to_string())
        });
        let keystore_service = config
            .keystore_service
            .unwrap_or_else(|| config.app_name.clone());
        let keystore = KeyStore::new(&keystore_service, config.keystore.as_deref().unwrap_or(""))?;
        let grpc_endpoint = config.grpc_endpoint.unwrap_or_else(default_grpc_endpoint);

        // ask keystore for http creds: this may trigger user auth for os keyring keystore
        let http_creds = keystore.get_http_credentials()?;

        let http_client = HttpClient::new(
            builder,
            base_url.clone(),
            config.limits.clone(),
            config.response_limits,
            config.rate_limit_max_retries,
            resolved_http_timeouts,
            http_creds,
        )?;
        let cache = if config.disable_cache {
            AnytypeCache::new_disabled()
        } else {
            AnytypeCache::default()
        };

        debug!(
            http_path = %diagnostic_path(&base_url),
            keystore_configured = true,
            grpc_configured = true,
            "new http client"
        );

        Ok(Self {
            client: Arc::new(http_client),
            // update config with _actual_ values so get_config() will give correct values
            config: ClientConfig {
                // base_url, keystore_service, and grpc_endpoint are always Some(...)
                // ... None values were replaced with defaults from environment or constants
                base_url: Some(base_url),
                keystore_service: Some(keystore_service),
                grpc_endpoint: Some(grpc_endpoint),
                http_timeouts: Some(resolved_http_timeouts),
                grpc_timeouts: Some(resolved_grpc_timeouts),
                // other values unchanged
                ..config
            },
            keystore,
            cache: Arc::new(cache),
            grpc: Arc::new(Mutex::new(None)),
            attached_discussion_metrics: Arc::new(
                crate::attached_discussions::AttachedDiscussionMetrics::default(),
            ),
            type_property_metrics: Arc::new(
                crate::types::TypePropertyClassificationMetrics::default(),
            ),
            collection_membership_metrics: Arc::new(
                crate::views::CollectionMembershipMetrics::default(),
            ),
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
    #[must_use]
    pub fn get_config(&self) -> &ClientConfig {
        &self.config
    }

    /// Returns the configured http endpoint
    #[must_use]
    pub fn get_http_endpoint(&self) -> &str {
        &self.client.base_url
    }

    /// Returns the configured grpc endpoint
    #[must_use]
    pub fn get_grpc_endpoint(&self) -> Option<String> {
        self.config.grpc_endpoint.clone()
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
    #[must_use]
    pub fn api_version(&self) -> String {
        crate::ANYTYPE_API_VERSION.to_string()
    }

    /// Returns a gRPC client authorized using credentials stored in the keystore.
    ///
    /// Requires gRPC credentials saved to the keystore.
    pub async fn grpc_client(&self) -> Result<AnytypeGrpcClient> {
        let grpc_config = self
            .config
            .grpc_endpoint
            .as_ref()
            .map_or_else(AnytypeGrpcConfig::default, |endpoint| {
                AnytypeGrpcConfig::new(endpoint.to_owned())
            })
            .grpc_timeouts(self.config.grpc_timeouts.unwrap_or_default());

        get_or_try_init(&self.grpc, || self.create_grpc_client(&grpc_config)).await
    }

    /// Minimal authenticated HTTP ping (list spaces with limit 1).
    pub async fn ping_http(&self) -> Result<()> {
        let _ = self.spaces().limit(1).list().await?;
        Ok(())
    }

    /// Create and cache a gRPC client using credentials stored in the keystore.
    async fn create_grpc_client(&self, config: &AnytypeGrpcConfig) -> Result<AnytypeGrpcClient> {
        let creds = self.keystore.get_grpc_credentials()?;
        match select_grpc_credential(&creds)? {
            GrpcCredential::SessionToken(token) => {
                AnytypeGrpcClient::from_token(config, token.to_owned())
                    .await
                    .map_err(|source| AnytypeError::Grpc { source })
            }
            GrpcCredential::AccountKey(account_key) => {
                AnytypeGrpcClient::from_account_key(config, account_key.to_owned())
                    .await
                    .map_err(|source| AnytypeError::Grpc { source })
            }
        }
    }

    /// Sends a minimal authenticated gRPC ping (list apps).
    ///
    /// This method requires an Anytype CLI server and gRPC credentials in the
    /// configured keystore.
    pub async fn ping_grpc(&self) -> Result<()> {
        use anytype_rpc::{
            anytype::rpc::account::local_link::list_apps::Request as ListAppsRequest,
            auth::with_token,
            deadline::{GrpcCallOptions, with_grpc_call_options},
        };
        use tonic::Request;

        let grpc = self.grpc_client().await?;
        let mut commands = grpc.client_commands();
        let request = Request::new(ListAppsRequest {});
        let request = with_token(request, grpc.token()).map_err(|err| AnytypeError::Auth {
            message: err.to_string(),
        })?;
        let request = with_grpc_call_options(request, GrpcCallOptions::ordinary_read());
        let started = std::time::Instant::now();
        let response = commands
            .account_local_link_list_apps(request)
            .await
            .map_err(|status| {
                crate::grpc_util::grpc_status_for(
                    status,
                    anytype_rpc::deadline::GrpcTimeoutClass::OrdinaryUnary,
                    anytype_rpc::deadline::GrpcTimeoutOutcome::ReadAborted,
                    started.elapsed(),
                )
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
    /// - `logical_operations`: Number of calls entering the HTTP request pipeline
    /// - `total_requests`: Number of HTTP requests sent
    /// - `physical_attempts`: Number of physical sends, including automatic replays
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
    #[must_use]
    pub fn http_metrics(&self) -> HttpMetricsSnapshot {
        self.client.metrics_snapshot()
    }

    /// Returns cumulative attached-discussion HTTP/gRPC ownership counters.
    #[must_use]
    pub fn attached_discussion_metrics(
        &self,
    ) -> crate::attached_discussions::AttachedDiscussionMetricsSnapshot {
        self.attached_discussion_metrics.snapshot()
    }

    /// Returns cumulative type-property classification RPC ownership metrics.
    #[must_use]
    pub fn type_property_classification_metrics(
        &self,
    ) -> crate::types::TypePropertyClassificationMetricsSnapshot {
        self.type_property_metrics.snapshot()
    }

    /// Returns cumulative canonical collection-membership work counters.
    #[must_use]
    pub fn collection_membership_metrics(
        &self,
    ) -> crate::views::CollectionMembershipMetricsSnapshot {
        self.collection_membership_metrics.snapshot()
    }

    /// Returns a non-secret generation that changes whenever the in-memory
    /// HTTP credentials are set or cleared.
    ///
    /// This can namespace process-local caches without retaining or hashing
    /// credential material.
    #[must_use]
    pub fn http_credential_generation(&self) -> u64 {
        self.client.credential_generation()
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

async fn get_or_try_init<T, E, F, Fut>(slot: &Mutex<Option<T>>, initialize: F) -> Result<T, E>
where
    T: Clone,
    F: FnOnce() -> Fut,
    Fut: Future<Output = std::result::Result<T, E>>,
{
    let mut guard = slot.lock().await;
    if let Some(value) = guard.as_ref() {
        return Ok(value.clone());
    }
    let value = initialize().await?;
    *guard = Some(value.clone());
    Ok(value)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum GrpcCredential<'a> {
    SessionToken(&'a str),
    AccountKey(&'a str),
}

fn select_grpc_credential(credentials: &GrpcCredentials) -> Result<GrpcCredential<'_>> {
    if let Some(token) = credentials
        .session_token()
        .filter(|token| !token.is_empty())
    {
        return Ok(GrpcCredential::SessionToken(token));
    }
    if let Some(account_key) = credentials
        .account_key()
        .filter(|account_key| !account_key.is_empty())
    {
        return Ok(GrpcCredential::AccountKey(account_key));
    }
    GrpcUnavailableSnafu {
        message: "no grpc token or account key in keystore".to_string(),
    }
    .fail()
}

impl AnytypeClient {
    // accessor to support cache tests
    #[doc(hidden)]
    #[must_use]
    pub fn cache(&self) -> Arc<AnytypeCache> {
        self.cache.clone()
    }
}

/// Discover an Anytype gRPC listening port on the local machine.
///
/// Runs `lsof -Pni` to find TCP ports in LISTEN state owned by a process whose
/// name starts with `program` (default `"anytype"`), then probes each candidate
/// with an unauthenticated `AppGetVersion` gRPC call.
///
/// Returns the first port that responds, or `None`.
///
/// Only supported on macOS and Linux.
pub async fn find_grpc(program: Option<impl Into<String>>) -> Option<u16> {
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = program;
        None
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        let prefix = program.map_or_else(|| "anytype".to_string(), Into::into);

        let ports = match lsof_listen_ports(&prefix).await {
            Ok(ports) => ports,
            Err(err) => {
                debug!("lsof failed: {err}");
                return None;
            }
        };

        first_responsive_port(&ports, probe_grpc_port).await
    }
}

/// Run `lsof -Pni` and extract unique listening ports for the given program prefix.
#[cfg(any(target_os = "linux", target_os = "macos", test))]
async fn lsof_listen_ports(prefix: &str) -> std::result::Result<Vec<u16>, String> {
    lsof_listen_ports_with("lsof", prefix).await
}

#[cfg(any(target_os = "linux", target_os = "macos", test))]
async fn lsof_listen_ports_with(
    command: &str,
    prefix: &str,
) -> std::result::Result<Vec<u16>, String> {
    let output = tokio::process::Command::new(command)
        .args(["-Pni"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .output()
        .await
        .map_err(|err| format!("failed to run lsof: {err}"))?;

    parse_lsof_output(prefix, output.status.success(), &output.stdout)
}

#[cfg(any(target_os = "linux", target_os = "macos", test))]
fn parse_lsof_output(
    prefix: &str,
    success: bool,
    stdout: &[u8],
) -> std::result::Result<Vec<u16>, String> {
    if !success {
        return Err("lsof exited unsuccessfully".to_owned());
    }

    let stdout = String::from_utf8_lossy(stdout);
    let mut ports = Vec::new();

    for line in stdout.lines() {
        // COMMAND is the first whitespace-delimited field
        let Some(command) = line.split_whitespace().next() else {
            continue;
        };
        if !command.starts_with(prefix) {
            continue;
        }
        if !line.contains("LISTEN") {
            continue;
        }
        // Extract port: find the last ':' before "(LISTEN)" or end-of-line,
        // then parse the number that follows it.
        if let Some(port) = extract_port(line)
            && !ports.contains(&port)
        {
            ports.push(port);
        }
    }

    Ok(ports)
}

#[cfg(any(target_os = "linux", target_os = "macos", test))]
async fn first_responsive_port<F, Fut>(ports: &[u16], mut probe: F) -> Option<u16>
where
    F: FnMut(u16) -> Fut,
    Fut: Future<Output = bool>,
{
    for port in ports {
        if probe(*port).await {
            return Some(*port);
        }
    }
    None
}

/// Extract a port number from an lsof NAME column like `*:31010 (LISTEN)`
/// or `127.0.0.1:31010 (LISTEN)` or `[::1]:31010 (LISTEN)`.
#[cfg(any(target_os = "linux", target_os = "macos", test))]
fn extract_port(line: &str) -> Option<u16> {
    // Find the portion before "(LISTEN)" and work backwards to the last ':'
    let before_listen = line.split("(LISTEN)").next()?;
    let colon_pos = before_listen.rfind(':')?;
    let after_colon = before_listen[colon_pos + 1..].trim();
    after_colon.parse().ok()
}

/// Total local budget for connecting and completing the unauthenticated probe.
#[cfg(any(target_os = "linux", target_os = "macos", test))]
const GRPC_PORT_PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

#[cfg(any(target_os = "linux", target_os = "macos", test))]
async fn bounded_grpc_port_probe<F>(probe: F) -> bool
where
    F: Future<Output = bool>,
{
    tokio::time::timeout(GRPC_PORT_PROBE_TIMEOUT, probe)
        .await
        .unwrap_or(false)
}

/// Try an unauthenticated `AppGetVersion` call on the given port.
#[cfg(any(target_os = "linux", target_os = "macos"))]
async fn probe_grpc_port(port: u16) -> bool {
    use anytype_rpc::anytype::{
        ClientCommandsClient, rpc::app::get_version::Request as AppGetVersionRequest,
    };
    use tonic::transport::Endpoint;

    bounded_grpc_port_probe(async move {
        let endpoint = match Endpoint::from_shared(format!("http://127.0.0.1:{port}")) {
            Ok(endpoint) => endpoint,
            Err(_) => return false,
        };
        let channel = match endpoint.connect().await {
            Ok(channel) => channel,
            Err(_) => return false,
        };
        let mut client = ClientCommandsClient::new(channel);
        client
            .app_get_version(tonic::Request::new(AppGetVersionRequest {}))
            .await
            .is_ok()
    })
    .await
}

#[cfg(test)]
mod find_grpc_tests {
    use std::ffi::OsString;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use tokio::sync::Mutex;

    use super::{
        GRPC_PORT_PROBE_TIMEOUT, GrpcCredential, bounded_grpc_port_probe, extract_port,
        first_responsive_port, get_or_try_init, lsof_listen_ports, lsof_listen_ports_with,
        parse_lsof_output, select_grpc_credential,
    };
    use crate::{
        client::{AnytypeClient, ClientConfig},
        error::AnytypeError,
        keystore::GrpcCredentials,
    };
    use anytype_rpc::deadline::GrpcTimeoutPolicy;

    struct EnvironmentRestore {
        key: &'static str,
        previous: Option<OsString>,
    }

    impl Drop for EnvironmentRestore {
        fn drop(&mut self) {
            match self.previous.take() {
                Some(value) => {
                    // SAFETY: environment-mutating tests in this module are serialized.
                    unsafe { std::env::set_var(self.key, value) };
                }
                None => {
                    // SAFETY: environment-mutating tests in this module are serialized.
                    unsafe { std::env::remove_var(self.key) };
                }
            }
        }
    }

    #[test]
    fn extract_port_ipv4() {
        let line = "anytype   12345 user   25u  IPv4 0x1234  0t0  TCP 127.0.0.1:31010 (LISTEN)";
        assert_eq!(extract_port(line), Some(31010));
    }

    #[test]
    fn extract_port_wildcard() {
        let line = "anytype   12345 user   25u  IPv4 0x1234  0t0  TCP *:31010 (LISTEN)";
        assert_eq!(extract_port(line), Some(31010));
    }

    #[test]
    fn extract_port_ipv6() {
        let line = "anytypeH  12345 user   26u  IPv6 0x5678  0t0  TCP [::1]:31010 (LISTEN)";
        assert_eq!(extract_port(line), Some(31010));
    }

    #[test]
    fn extract_port_no_listen() {
        let line =
            "anytype   12345 user   25u  IPv4 0x1234  0t0  TCP 127.0.0.1:31010 (ESTABLISHED)";
        // extract_port relies on "(LISTEN)" to delimit the port number,
        // so non-LISTEN lines return None. The caller pre-filters for LISTEN.
        assert_eq!(extract_port(line), None);
    }

    #[test]
    fn lsof_output_filters_process_state_and_duplicate_ports() {
        let output = b"COMMAND PID USER FD TYPE DEVICE SIZE/OFF NODE NAME\n\
            other 1 user 3u IPv4 0 0t0 TCP 127.0.0.1:31001 (LISTEN)\n\
            anytype 2 user 4u IPv4 0 0t0 TCP 127.0.0.1:31002 (ESTABLISHED)\n\
            anytype 2 user 5u IPv4 0 0t0 TCP 127.0.0.1:31003 (LISTEN)\n\
            anytypeH 3 user 6u IPv6 0 0t0 TCP [::1]:31003 (LISTEN)\n";
        assert_eq!(
            parse_lsof_output("anytype", true, output).expect("parse successful lsof output"),
            vec![31003]
        );
        assert!(parse_lsof_output("anytype", false, output).is_err());
    }

    #[tokio::test]
    async fn lsof_absence_is_classified_without_panicking() {
        let error = lsof_listen_ports_with("anytype-definitely-missing-lsof", "anytype")
            .await
            .expect_err("missing lsof command must be classified");
        assert!(error.starts_with("failed to run lsof:"));
    }

    #[tokio::test]
    async fn candidate_probe_selects_first_responsive_port() {
        let observed = Arc::new(Mutex::new(Vec::new()));
        let ports = [31001, 31002, 31003];
        let selected = first_responsive_port(&ports, {
            let observed = observed.clone();
            move |port| {
                let observed = observed.clone();
                async move {
                    observed.lock().await.push(port);
                    port == 31002
                }
            }
        })
        .await;
        assert_eq!(selected, Some(31002));
        assert_eq!(*observed.lock().await, vec![31001, 31002]);
    }

    #[tokio::test(start_paused = true)]
    async fn grpc_port_probe_budget_includes_stalled_logical_probe_after_connection() {
        let (connected, connection_observed) = tokio::sync::oneshot::channel();
        let probe = tokio::spawn(async move {
            bounded_grpc_port_probe(async move {
                let _ = connected.send(());
                std::future::pending::<bool>().await
            })
            .await
        });

        connection_observed
            .await
            .expect("synthetic connection completed");
        tokio::time::advance(GRPC_PORT_PROBE_TIMEOUT - std::time::Duration::from_millis(1)).await;
        assert!(!probe.is_finished());
        tokio::time::advance(std::time::Duration::from_millis(1)).await;
        assert!(!probe.await.expect("probe task"));
    }

    #[tokio::test]
    async fn cache_initialization_is_reused_and_serialized() {
        let slot = Mutex::new(None);
        let initializations = AtomicUsize::new(0);
        let first = get_or_try_init(&slot, || async {
            initializations.fetch_add(1, Ordering::Relaxed);
            tokio::task::yield_now().await;
            Ok::<usize, ()>(42)
        });
        let second = get_or_try_init(&slot, || async {
            initializations.fetch_add(1, Ordering::Relaxed);
            Ok::<usize, ()>(43)
        });
        let (first, second) = tokio::join!(first, second);
        assert_eq!(first, Ok(42));
        assert_eq!(second, Ok(42));
        assert_eq!(initializations.load(Ordering::Relaxed), 1);

        let cached = get_or_try_init(&slot, || async {
            initializations.fetch_add(1, Ordering::Relaxed);
            Ok::<usize, ()>(44)
        })
        .await;
        assert_eq!(cached, Ok(42));
        assert_eq!(initializations.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn credential_selection_prefers_session_token_and_rejects_empty_values() {
        let both = GrpcCredentials::new(
            None,
            Some("account-key".to_owned()),
            Some("session-token".to_owned()),
        );
        assert!(matches!(
            select_grpc_credential(&both),
            Ok(GrpcCredential::SessionToken("session-token"))
        ));
        let account = GrpcCredentials::from_account_key("account-key");
        assert!(matches!(
            select_grpc_credential(&account),
            Ok(GrpcCredential::AccountKey("account-key"))
        ));
        let empty = GrpcCredentials::new(None, Some(String::new()), Some(String::new()));
        assert!(matches!(
            select_grpc_credential(&empty),
            Err(AnytypeError::GrpcUnavailable { .. })
        ));
    }

    #[test]
    fn invalid_grpc_policy_fails_before_client_side_effects() {
        let config = ClientConfig::default().grpc_timeouts(GrpcTimeoutPolicy {
            cleanup: Some(std::time::Duration::from_secs(31)),
            ..GrpcTimeoutPolicy::default()
        });
        assert!(matches!(
            AnytypeClient::with_config(config),
            Err(AnytypeError::Validation { .. })
        ));
    }

    #[test]
    #[serial_test::serial]
    fn absent_api_policy_resolves_grpc_environment_override() {
        let key = anytype_rpc::deadline::ANYTYPE_GRPC_TIMEOUT_SECS;
        let _restore = EnvironmentRestore {
            key,
            previous: std::env::var_os(key),
        };
        // SAFETY: this test is serialized with every test that mutates process environment.
        unsafe { std::env::set_var(key, "17") };

        let id = std::process::id();
        let path = std::env::temp_dir().join(format!("anytype-grpc-env-policy-{id}.db"));
        let mut config = ClientConfig::default().app_name("grpc-environment-policy");
        config.keystore = Some(format!("file:path={}", path.display()));
        config.keystore_service = Some(format!("grpc-environment-policy-{id}"));
        let client = AnytypeClient::with_config(config).expect("construct client with env policy");
        let policy = client
            .get_config()
            .grpc_timeouts
            .expect("client retains resolved policy");
        assert_eq!(
            policy.ordinary_unary,
            Some(std::time::Duration::from_secs(17))
        );
        assert_eq!(policy.long_unary, Some(std::time::Duration::from_secs(17)));

        for suffix in ["", "-shm", "-wal"] {
            let _ = std::fs::remove_file(format!("{}{}", path.display(), suffix));
        }
    }

    #[tokio::test]
    async fn grpc_client_classifies_connection_failure() {
        let id = std::process::id();
        let path = std::env::temp_dir().join(format!("anytype-grpc-client-{id}.db"));
        let mut config = ClientConfig::default().app_name("grpc-connection-failure");
        config.grpc_endpoint = Some("http://127.0.0.1:1".to_owned());
        config.keystore = Some(format!("file:path={}", path.display()));
        config.keystore_service = Some(format!("grpc-connection-failure-{id}"));
        let client = AnytypeClient::with_config(config).expect("construct gRPC failure client");
        client
            .keystore
            .update_grpc_credentials(&GrpcCredentials::from_token("test-session-token"))
            .expect("store test gRPC credentials");

        let error = tokio::time::timeout(std::time::Duration::from_secs(5), client.grpc_client())
            .await
            .expect("connection failure is bounded")
            .expect_err("closed loopback port must reject gRPC connection");
        assert!(matches!(error, AnytypeError::Grpc { .. }));
        client
            .keystore
            .clear_all_credentials()
            .expect("clear test credentials");
        for suffix in ["", "-shm", "-wal"] {
            let _ = std::fs::remove_file(format!("{}{}", path.display(), suffix));
        }
    }

    #[tokio::test]
    async fn lsof_listen_ports_filters_prefix() {
        // With an unlikely prefix, we should get an empty list. A host
        // without lsof is tolerated exactly as `find_grpc` tolerates it:
        // discovery is unavailable rather than an error.
        match lsof_listen_ports("zzz_nonexistent_program_zzz").await {
            Ok(ports) => assert!(ports.is_empty()),
            Err(error) => assert!(error.starts_with("failed to run lsof:")),
        }
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[tokio::test]
    async fn lsof_observes_a_listener_owned_by_this_process() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind process-owned listener");
        let port = listener.local_addr().expect("listener address").port();
        let ports = lsof_listen_ports("")
            .await
            .expect("supported Unix host provides lsof");
        assert!(ports.contains(&port));
    }
}
