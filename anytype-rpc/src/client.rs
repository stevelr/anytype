use tonic::transport::{Channel, Endpoint};

use crate::anytype::ClientCommandsClient;
use crate::auth::{create_session_token_from_account_key, create_session_token_from_app_key};
use crate::error::AnytypeGrpcError;

// optional environment variable containing grpc endpoint
const ANYTYPE_GRPC_ENDPOINT_ENV: &str = "ANYTYPE_GRPC_ENDPOINT";
const ANYTYPE_GRPC_HEADLESS_URL: &str = "http://127.0.0.1:31010";

/// checks environment variable "ANYTYPE_GRPC_ENDPOINT", then falls back to headless cli endpoint
pub fn default_grpc_endpoint() -> String {
    std::env::var(ANYTYPE_GRPC_ENDPOINT_ENV)
        .unwrap_or_else(|_| ANYTYPE_GRPC_HEADLESS_URL.to_string())
}

/// Configuration for connecting to Anytype gRPC.
#[derive(Debug, Clone)]
pub struct AnytypeGrpcConfig {
    endpoint: String,
}

impl Default for AnytypeGrpcConfig {
    fn default() -> Self {
        Self {
            endpoint: default_grpc_endpoint(),
        }
    }
}

impl AnytypeGrpcConfig {
    pub fn new(endpoint: impl Into<String>) -> Self {
        Self {
            endpoint: endpoint.into(),
        }
    }

    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }
}

/// gRPC client wrapper holding the connection and session token.
#[derive(Debug, Clone)]
pub struct AnytypeGrpcClient {
    channel: Channel,
    token: String,
}

impl AnytypeGrpcClient {
    pub async fn connect_channel(config: &AnytypeGrpcConfig) -> Result<Channel, AnytypeGrpcError> {
        Ok(Endpoint::from_shared(config.endpoint.clone())?
            .connect()
            .await?)
    }

    /// if you're using the headless client, you can generate a session token
    /// from the account key in ~/.anytype/config.json
    pub async fn from_account_key(
        config: &AnytypeGrpcConfig,
        account_key: impl AsRef<str>,
    ) -> Result<Self, AnytypeGrpcError> {
        let channel = Self::connect_channel(config).await?;
        let token = create_session_token_from_account_key(channel.clone(), account_key).await?;
        Ok(Self { channel, token })
    }

    // this may not work: the api may not have sufficient scope to create a grpc token
    pub async fn from_app_key(
        config: &AnytypeGrpcConfig,
        app_key: impl AsRef<str>,
    ) -> Result<Self, AnytypeGrpcError> {
        let channel = Self::connect_channel(config).await?;
        let token = create_session_token_from_app_key(channel.clone(), app_key).await?;
        Ok(Self { channel, token })
    }

    pub async fn from_token(
        config: &AnytypeGrpcConfig,
        token: impl Into<String>,
    ) -> Result<Self, AnytypeGrpcError> {
        let channel = Self::connect_channel(config).await?;
        Ok(Self {
            channel,
            token: token.into(),
        })
    }

    pub fn client_commands(&self) -> ClientCommandsClient<Channel> {
        ClientCommandsClient::new(self.channel.clone())
    }

    pub fn token(&self) -> &str {
        &self.token
    }

    pub fn channel(&self) -> Channel {
        self.channel.clone()
    }
}
