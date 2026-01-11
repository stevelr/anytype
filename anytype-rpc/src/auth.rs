//! Authentication helpers for Anytype gRPC clients.

use std::fmt;

use tonic::metadata::{Ascii, MetadataValue};
use tonic::service::Interceptor;
use tonic::{Request, Status, transport::Channel};

use crate::anytype::ClientCommandsClient;
use crate::anytype::rpc::wallet::create_session::{
    Request as CreateSessionRequest, Response as CreateSessionResponse, request::Auth,
};

/// Authentication options for `WalletCreateSession`.
#[derive(Debug, Clone)]
pub enum SessionAuth {
    /// Local app key created via LocalLink (limited scope).
    AppKey(String),
    /// Account key from the headless CLI (full scope).
    AccountKey(String),
    /// Mnemonic phrase (full scope).
    Mnemonic(String),
    /// Existing session token to refresh.
    Token(String),
}

impl SessionAuth {
    fn into_request(self) -> CreateSessionRequest {
        let auth = match self {
            SessionAuth::AppKey(value) => Auth::AppKey(value),
            SessionAuth::AccountKey(value) => Auth::AccountKey(value),
            SessionAuth::Mnemonic(value) => Auth::Mnemonic(value),
            SessionAuth::Token(value) => Auth::Token(value),
        };
        CreateSessionRequest { auth: Some(auth) }
    }
}

/// Errors surfaced by auth helpers.
#[derive(Debug)]
pub enum AuthError {
    Transport(Status),
    Api { code: i32, description: String },
    EmptyToken,
    InvalidMetadata(tonic::metadata::errors::InvalidMetadataValue),
}

impl fmt::Display for AuthError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AuthError::Transport(status) => write!(f, "transport error: {status}"),
            AuthError::Api { code, description } => {
                write!(f, "api error {code}: {description}")
            }
            AuthError::EmptyToken => write!(f, "create session returned empty token"),
            AuthError::InvalidMetadata(err) => write!(f, "invalid metadata value: {err}"),
        }
    }
}

impl std::error::Error for AuthError {}

impl From<Status> for AuthError {
    fn from(status: Status) -> Self {
        AuthError::Transport(status)
    }
}

impl From<tonic::metadata::errors::InvalidMetadataValue> for AuthError {
    fn from(err: tonic::metadata::errors::InvalidMetadataValue) -> Self {
        AuthError::InvalidMetadata(err)
    }
}

/// Create a session and return the full response for additional fields (like `app_token`).
pub async fn create_session(
    channel: Channel,
    auth: SessionAuth,
) -> Result<CreateSessionResponse, AuthError> {
    let mut client = ClientCommandsClient::new(channel);
    let request = auth.into_request();
    let response = client.wallet_create_session(request).await?;
    let response = response.into_inner();

    if let Some(error) = response.error.as_ref()
        && error.code != 0
    {
        return Err(AuthError::Api {
            code: error.code,
            description: error.description.clone(),
        });
    }

    Ok(response)
}

/// Create a session and return just the session token.
pub async fn create_session_token(
    channel: Channel,
    auth: SessionAuth,
) -> Result<String, AuthError> {
    let response = create_session(channel, auth).await?;
    if response.token.is_empty() {
        return Err(AuthError::EmptyToken);
    }
    Ok(response.token)
}

/// Convenience helper to add the `token` metadata to a request.
pub fn with_token<T>(mut request: Request<T>, token: &str) -> Result<Request<T>, AuthError> {
    let token_value: MetadataValue<Ascii> = token.parse()?;
    request.metadata_mut().insert("token", token_value);
    Ok(request)
}

/// gRPC interceptor that injects a static session token.
pub struct TokenInterceptor {
    token: MetadataValue<Ascii>,
}

impl TokenInterceptor {
    pub fn new(token: impl AsRef<str>) -> Result<Self, AuthError> {
        let token_value: MetadataValue<Ascii> = token.as_ref().parse()?;
        Ok(Self { token: token_value })
    }
}

impl Interceptor for TokenInterceptor {
    fn call(&mut self, mut request: Request<()>) -> Result<Request<()>, Status> {
        request.metadata_mut().insert("token", self.token.clone());
        Ok(request)
    }
}
