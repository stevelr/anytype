//! Authentication helpers for Anytype gRPC clients.

use std::fmt;

use tonic::metadata::{Ascii, MetadataValue};
use tonic::service::Interceptor;
use tonic::{Request, Status, transport::Channel};

use crate::anytype::ClientCommandsClient;
use crate::anytype::rpc::account::local_link::new_challenge::Request as LocalLinkChallengeRequest;
use crate::anytype::rpc::account::local_link::solve_challenge::Request as LocalLinkSolveRequest;
use crate::anytype::rpc::wallet::create_session::{
    Request as CreateSessionRequest, Response as CreateSessionResponse, request::Auth,
};
use crate::model::account::auth::LocalApiScope;

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

/// Create a session token from a LocalLink app key.
pub async fn create_session_token_from_app_key(
    channel: Channel,
    app_key: impl AsRef<str>,
) -> Result<String, AuthError> {
    create_session_token(channel, SessionAuth::AppKey(app_key.as_ref().to_string())).await
}

/// Create a session token from a headless account key.
pub async fn create_session_token_from_account_key(
    channel: Channel,
    account_key: impl AsRef<str>,
) -> Result<String, AuthError> {
    create_session_token(
        channel,
        SessionAuth::AccountKey(account_key.as_ref().to_string()),
    )
    .await
}

/// Response from LocalLink SolveChallenge.
#[derive(Debug, Clone)]
pub struct LocalLinkCredentials {
    pub app_key: String,
    pub session_token: Option<String>,
}

/// Create a LocalLink challenge for the given app name and scope.
pub async fn create_local_link_challenge(
    channel: Channel,
    app_name: impl Into<String>,
    scope: LocalApiScope,
) -> Result<String, AuthError> {
    let mut client = ClientCommandsClient::new(channel);
    let request = LocalLinkChallengeRequest {
        app_name: app_name.into(),
        scope: scope as i32,
    };
    let response = client.account_local_link_new_challenge(request).await?;
    let response = response.into_inner();
    if let Some(error) = response.error.as_ref()
        && error.code != 0
    {
        return Err(AuthError::Api {
            code: error.code,
            description: error.description.clone(),
        });
    }
    Ok(response.challenge_id)
}

/// Solve a LocalLink challenge and return the app key.
pub async fn solve_local_link_challenge(
    channel: Channel,
    challenge_id: impl Into<String>,
    answer: impl Into<String>,
) -> Result<LocalLinkCredentials, AuthError> {
    let mut client = ClientCommandsClient::new(channel);
    let request = LocalLinkSolveRequest {
        challenge_id: challenge_id.into(),
        answer: answer.into(),
    };
    let response = client.account_local_link_solve_challenge(request).await?;
    let response = response.into_inner();
    if let Some(error) = response.error.as_ref()
        && error.code != 0
    {
        return Err(AuthError::Api {
            code: error.code,
            description: error.description.clone(),
        });
    }
    Ok(LocalLinkCredentials {
        app_key: response.app_key,
        session_token: if response.session_token.is_empty() {
            None
        } else {
            Some(response.session_token)
        },
    })
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
