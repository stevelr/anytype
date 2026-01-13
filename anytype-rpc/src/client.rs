use std::fmt;

use tonic::transport::{Channel, Endpoint};

use crate::anytype::ClientCommandsClient;
use crate::auth::{
    AuthError, create_session_token_from_account_key, create_session_token_from_app_key,
};

/// Configuration for connecting to Anytype gRPC.
#[derive(Debug, Clone)]
pub struct AnytypeGrpcConfig {
    endpoint: String,
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

/// Errors that can occur when creating a gRPC client.
#[derive(Debug)]
pub enum AnytypeGrpcClientError {
    Transport(tonic::transport::Error),
    Auth(AuthError),
}

impl fmt::Display for AnytypeGrpcClientError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AnytypeGrpcClientError::Transport(err) => {
                write!(f, "grpc transport error: {err}")
            }
            AnytypeGrpcClientError::Auth(err) => write!(f, "grpc auth error: {err}"),
        }
    }
}

impl std::error::Error for AnytypeGrpcClientError {}

impl From<tonic::transport::Error> for AnytypeGrpcClientError {
    fn from(err: tonic::transport::Error) -> Self {
        AnytypeGrpcClientError::Transport(err)
    }
}

impl From<AuthError> for AnytypeGrpcClientError {
    fn from(err: AuthError) -> Self {
        AnytypeGrpcClientError::Auth(err)
    }
}

/// gRPC client wrapper holding the connection and session token.
#[derive(Debug, Clone)]
pub struct AnytypeGrpcClient {
    channel: Channel,
    token: String,
}

impl AnytypeGrpcClient {
    pub async fn connect_channel(
        config: &AnytypeGrpcConfig,
    ) -> Result<Channel, AnytypeGrpcClientError> {
        Ok(Endpoint::from_shared(config.endpoint.clone())?
            .connect()
            .await?)
    }

    /// if you're using the headless client, you can generate a session token
    /// from the account key in ~/.anytype/config.json
    pub async fn from_account_key(
        config: &AnytypeGrpcConfig,
        account_key: impl AsRef<str>,
    ) -> Result<Self, AnytypeGrpcClientError> {
        let channel = Self::connect_channel(config).await?;
        let token = create_session_token_from_account_key(channel.clone(), account_key).await?;
        Ok(Self { channel, token })
    }

    // this may not work: the api may not have sufficient scope to create a grpc token
    pub async fn from_app_key(
        config: &AnytypeGrpcConfig,
        app_key: impl AsRef<str>,
    ) -> Result<Self, AnytypeGrpcClientError> {
        let channel = Self::connect_channel(config).await?;
        let token = create_session_token_from_app_key(channel.clone(), app_key).await?;
        Ok(Self { channel, token })
    }

    pub async fn from_token(
        config: &AnytypeGrpcConfig,
        token: impl Into<String>,
    ) -> Result<Self, AnytypeGrpcClientError> {
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
