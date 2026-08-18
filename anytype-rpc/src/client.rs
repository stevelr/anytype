use std::time::Duration;
use tonic::transport::{Channel, Endpoint};

use crate::anytype::ClientCommandsClient;
use crate::auth::{
    create_session_token_from_account_key_with_policy,
    create_session_token_from_app_key_with_policy,
};
use crate::deadline::{GrpcDeadlineService, GrpcTimeoutPolicy};
use crate::error::AnytypeGrpcError;

// optional environment variable containing grpc endpoint
const ANYTYPE_GRPC_ENDPOINT_ENV: &str = "ANYTYPE_GRPC_ENDPOINT";
const ANYTYPE_GRPC_ENDPOINT: &str = "http://127.0.0.1:31010"; // headless server

/// checks environment variable "ANYTYPE_GRPC_ENDPOINT", then falls back to headless cli endpoint
pub fn default_grpc_endpoint() -> String {
    std::env::var(ANYTYPE_GRPC_ENDPOINT_ENV).unwrap_or_else(|_| ANYTYPE_GRPC_ENDPOINT.to_string())
}

/// Configuration for connecting to Anytype gRPC.
#[derive(Clone)]
pub struct AnytypeGrpcConfig {
    endpoint: String,
    grpc_timeouts: Option<GrpcTimeoutPolicy>,
}

impl std::fmt::Debug for AnytypeGrpcConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AnytypeGrpcConfig")
            .field("endpoint", &"redacted")
            .field("grpc_timeouts", &self.grpc_timeouts)
            .finish()
    }
}

impl Default for AnytypeGrpcConfig {
    fn default() -> Self {
        Self {
            endpoint: default_grpc_endpoint(),
            grpc_timeouts: None,
        }
    }
}

impl AnytypeGrpcConfig {
    pub fn new(endpoint: impl Into<String>) -> Self {
        Self {
            endpoint: endpoint.into(),
            grpc_timeouts: None,
        }
    }

    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    /// Sets an explicit logical gRPC deadline policy.
    ///
    /// An explicit policy ignores `ANYTYPE_GRPC_TIMEOUT_SECS`. `None` fields
    /// inside the policy disable their individual boundaries.
    #[must_use]
    pub fn grpc_timeouts(mut self, policy: GrpcTimeoutPolicy) -> Self {
        self.grpc_timeouts = Some(policy);
        self
    }

    /// Resolves and validates the effective logical gRPC deadline policy.
    pub fn resolved_grpc_timeouts(
        &self,
    ) -> Result<GrpcTimeoutPolicy, crate::deadline::GrpcTimeoutConfigError> {
        GrpcTimeoutPolicy::resolve(self.grpc_timeouts)
    }
}

/// Deadline-aware tonic service used by generated Anytype clients.
pub type AnytypeGrpcService = GrpcDeadlineService<Channel>;

/// gRPC client wrapper holding the connection and session token.
#[derive(Clone)]
pub struct AnytypeGrpcClient {
    channel: Channel,
    token: String,
    endpoint: String,
    grpc_timeouts: GrpcTimeoutPolicy,
}

impl std::fmt::Debug for AnytypeGrpcClient {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AnytypeGrpcClient")
            .field("channel", &"redacted")
            .field("token_configured", &!self.token.is_empty())
            .field("endpoint", &"redacted")
            .field("grpc_timeouts", &self.grpc_timeouts)
            .finish()
    }
}

impl AnytypeGrpcClient {
    /// returns the endpoint
    pub fn get_endpoint(&self) -> &str {
        &self.endpoint
    }

    /// Connects the raw transport after validating the configured timeout policy.
    ///
    /// This method only establishes the transport. Construct an
    /// [`AnytypeGrpcClient`] to apply logical deadlines to generated RPCs.
    pub async fn connect_channel(config: &AnytypeGrpcConfig) -> Result<Channel, AnytypeGrpcError> {
        // Resolve before network activity so malformed process configuration
        // cannot dispatch a connection attempt.
        let _ = config.resolved_grpc_timeouts()?;
        let endpoint = Endpoint::from_shared(config.endpoint.clone())?
            .connect_timeout(Duration::from_secs(30))
            .tcp_keepalive(Some(Duration::from_secs(60)))
            .http2_keep_alive_interval(Duration::from_secs(30))
            .keep_alive_timeout(Duration::from_secs(10))
            .keep_alive_while_idle(true);
        Ok(endpoint.connect().await?)
    }

    /// if you're using the headless client, you can generate a session token
    /// from the account key in ~/.anytype/config.json
    pub async fn from_account_key(
        config: &AnytypeGrpcConfig,
        account_key: impl AsRef<str>,
    ) -> Result<Self, AnytypeGrpcError> {
        let grpc_timeouts = config.resolved_grpc_timeouts()?;
        let channel = Self::connect_channel(config).await?;
        let token = create_session_token_from_account_key_with_policy(
            channel.clone(),
            account_key,
            grpc_timeouts,
        )
        .await?;
        Ok(Self {
            channel,
            token,
            endpoint: config.endpoint.clone(),
            grpc_timeouts,
        })
    }

    // this may not work: the api may not have sufficient scope to create a grpc token
    pub async fn from_app_key(
        config: &AnytypeGrpcConfig,
        app_key: impl AsRef<str>,
    ) -> Result<Self, AnytypeGrpcError> {
        let grpc_timeouts = config.resolved_grpc_timeouts()?;
        let channel = Self::connect_channel(config).await?;
        let token =
            create_session_token_from_app_key_with_policy(channel.clone(), app_key, grpc_timeouts)
                .await?;
        Ok(Self {
            channel,
            token,
            endpoint: config.endpoint.clone(),
            grpc_timeouts,
        })
    }

    pub async fn from_token(
        config: &AnytypeGrpcConfig,
        token: impl Into<String>,
    ) -> Result<Self, AnytypeGrpcError> {
        let grpc_timeouts = config.resolved_grpc_timeouts()?;
        let channel = Self::connect_channel(config).await?;
        Ok(Self {
            channel,
            token: token.into(),
            endpoint: config.endpoint.clone(),
            grpc_timeouts,
        })
    }

    /// Returns generated commands over the configured deadline-aware service.
    ///
    /// Requests without explicit [`GrpcCallOptions`](crate::deadline::GrpcCallOptions)
    /// use the ordinary unary profile. Existing shorter `grpc-timeout`
    /// metadata remains authoritative.
    pub fn client_commands(&self) -> ClientCommandsClient<AnytypeGrpcService> {
        ClientCommandsClient::new(self.deadline_channel())
    }

    pub fn token(&self) -> &str {
        &self.token
    }

    /// Returns the raw transport channel for compatibility with channel-generic helpers.
    ///
    /// Calls made directly through this channel do not receive the logical
    /// deadline policy. Prefer [`Self::client_commands`] for generated RPCs.
    #[must_use]
    pub fn channel(&self) -> Channel {
        self.channel.clone()
    }

    /// Returns a cloned deadline-aware tonic service.
    #[must_use]
    pub fn deadline_channel(&self) -> AnytypeGrpcService {
        GrpcDeadlineService::new_resolved(self.channel.clone(), self.grpc_timeouts)
    }

    /// Returns the resolved logical gRPC deadline policy.
    #[must_use]
    pub const fn grpc_timeouts(&self) -> GrpcTimeoutPolicy {
        self.grpc_timeouts
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn debug_output_redacts_endpoint_token_and_transport() {
        let endpoint = "http://ENDPOINT_SECRET.invalid";
        let config = AnytypeGrpcConfig::new(endpoint);
        let config_debug = format!("{config:?}");
        assert!(!config_debug.contains("ENDPOINT_SECRET"));

        let channel = Endpoint::from_static("http://CHANNEL_SECRET.invalid").connect_lazy();
        let client = AnytypeGrpcClient {
            channel,
            token: "TOKEN_SECRET".to_owned(),
            endpoint: endpoint.to_owned(),
            grpc_timeouts: GrpcTimeoutPolicy::default(),
        };
        let client_debug = format!("{client:?}");
        for secret in ["ENDPOINT_SECRET", "CHANNEL_SECRET", "TOKEN_SECRET"] {
            assert!(!client_debug.contains(secret));
        }
    }
}
