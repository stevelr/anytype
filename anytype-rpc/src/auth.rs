//! Authentication helpers for Anytype gRPC clients.

use tonic::{
    metadata::{Ascii, MetadataValue},
    service::Interceptor,
    {Request, Status, transport::Channel},
};

use crate::client::AnytypeGrpcConfig;
use crate::deadline::{
    GrpcCallOptions, GrpcDeadlineError, GrpcDeadlineService, GrpcTimeoutClass, GrpcTimeoutOutcome,
    GrpcTimeoutPolicy, with_grpc_call_options,
};
use crate::error::AuthError;
use crate::{
    anytype::ClientCommandsClient,
    anytype::rpc::account::local_link::{
        new_challenge::Request as LocalLinkChallengeRequest,
        new_challenge::Response as LocalLinkChallengeResponse,
        solve_challenge::Request as LocalLinkSolveRequest,
        solve_challenge::Response as LocalLinkSolveResponse,
    },
    anytype::rpc::wallet::create_session::{
        Request as CreateSessionRequest, Response as CreateSessionResponse, request::Auth,
    },
    model::account::auth::LocalApiScope,
};

/// Authentication options for `WalletCreateSession`.
#[derive(Clone)]
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

impl std::fmt::Debug for SessionAuth {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::AppKey(_) => "SessionAuth::AppKey(redacted)",
            Self::AccountKey(_) => "SessionAuth::AccountKey(redacted)",
            Self::Mnemonic(_) => "SessionAuth::Mnemonic(redacted)",
            Self::Token(_) => "SessionAuth::Token(redacted)",
        })
    }
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

/// Create a session and return the full response for additional fields (like `app_token`).
pub async fn create_session(
    channel: Channel,
    auth: SessionAuth,
) -> Result<CreateSessionResponse, AuthError> {
    let policy = GrpcTimeoutPolicy::resolve(None)?;
    create_session_with_policy(channel, auth, policy).await
}

/// Creates a session using an already resolved logical deadline policy.
pub async fn create_session_with_policy(
    channel: Channel,
    auth: SessionAuth,
    policy: GrpcTimeoutPolicy,
) -> Result<CreateSessionResponse, AuthError> {
    let policy = policy.validate()?;
    let mut client = ClientCommandsClient::new(GrpcDeadlineService::new_resolved(channel, policy));
    let request = with_grpc_call_options(
        Request::new(auth.into_request()),
        GrpcCallOptions::new(
            GrpcTimeoutClass::CredentialSetup,
            GrpcTimeoutOutcome::MutationIndeterminate,
        ),
    );
    let started = std::time::Instant::now();
    let response: tonic::Response<CreateSessionResponse> = client
        .wallet_create_session(request)
        .await
        .map_err(|status| {
            deadline_or_auth_status(
                status,
                GrpcTimeoutClass::CredentialSetup,
                GrpcTimeoutOutcome::MutationIndeterminate,
                started.elapsed(),
            )
        })?;
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

/// Creates an app-key session token with an already resolved deadline policy.
pub async fn create_session_token_from_app_key_with_policy(
    channel: Channel,
    app_key: impl AsRef<str>,
    policy: GrpcTimeoutPolicy,
) -> Result<String, AuthError> {
    let response = create_session_with_policy(
        channel,
        SessionAuth::AppKey(app_key.as_ref().to_string()),
        policy,
    )
    .await?;
    if response.token.is_empty() {
        return Err(AuthError::EmptyToken);
    }
    Ok(response.token)
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

/// Creates an account-key session token with an already resolved deadline policy.
pub async fn create_session_token_from_account_key_with_policy(
    channel: Channel,
    account_key: impl AsRef<str>,
    policy: GrpcTimeoutPolicy,
) -> Result<String, AuthError> {
    let response = create_session_with_policy(
        channel,
        SessionAuth::AccountKey(account_key.as_ref().to_string()),
        policy,
    )
    .await?;
    if response.token.is_empty() {
        return Err(AuthError::EmptyToken);
    }
    Ok(response.token)
}

/// Response from LocalLink SolveChallenge.
#[derive(Clone)]
pub struct LocalLinkCredentials {
    pub app_key: String,
    pub session_token: Option<String>,
}

impl std::fmt::Debug for LocalLinkCredentials {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LocalLinkCredentials")
            .field("app_key", &"redacted")
            .field("session_token_configured", &self.session_token.is_some())
            .finish()
    }
}

/// Create a LocalLink challenge for the given app name and scope.
pub async fn create_local_link_challenge(
    channel: Channel,
    app_name: impl Into<String>,
    scope: LocalApiScope,
) -> Result<String, AuthError> {
    let policy = GrpcTimeoutPolicy::resolve(None)?;
    create_local_link_challenge_with_policy(channel, app_name, scope, policy).await
}

/// Creates a LocalLink challenge using an explicit client configuration.
pub async fn create_local_link_challenge_with_config(
    channel: Channel,
    app_name: impl Into<String>,
    scope: LocalApiScope,
    config: &AnytypeGrpcConfig,
) -> Result<String, AuthError> {
    let policy = config.resolved_grpc_timeouts()?;
    create_local_link_challenge_with_policy(channel, app_name, scope, policy).await
}

/// Creates a LocalLink challenge using an already resolved deadline policy.
pub async fn create_local_link_challenge_with_policy(
    channel: Channel,
    app_name: impl Into<String>,
    scope: LocalApiScope,
    policy: GrpcTimeoutPolicy,
) -> Result<String, AuthError> {
    let policy = policy.validate()?;
    let mut client = ClientCommandsClient::new(GrpcDeadlineService::new_resolved(channel, policy));
    let request = LocalLinkChallengeRequest {
        app_name: app_name.into(),
        scope: scope as i32,
    };
    let request = with_grpc_call_options(
        Request::new(request),
        GrpcCallOptions::new(
            GrpcTimeoutClass::CredentialSetup,
            GrpcTimeoutOutcome::MutationIndeterminate,
        ),
    );
    let started = std::time::Instant::now();
    let response: tonic::Response<LocalLinkChallengeResponse> = client
        .account_local_link_new_challenge(request)
        .await
        .map_err(|status| {
            deadline_or_auth_status(
                status,
                GrpcTimeoutClass::CredentialSetup,
                GrpcTimeoutOutcome::MutationIndeterminate,
                started.elapsed(),
            )
        })?;
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
    let policy = GrpcTimeoutPolicy::resolve(None)?;
    solve_local_link_challenge_with_policy(channel, challenge_id, answer, policy).await
}

/// Solves a LocalLink challenge using an explicit client configuration.
pub async fn solve_local_link_challenge_with_config(
    channel: Channel,
    challenge_id: impl Into<String>,
    answer: impl Into<String>,
    config: &AnytypeGrpcConfig,
) -> Result<LocalLinkCredentials, AuthError> {
    let policy = config.resolved_grpc_timeouts()?;
    solve_local_link_challenge_with_policy(channel, challenge_id, answer, policy).await
}

/// Solves a LocalLink challenge using an already resolved deadline policy.
pub async fn solve_local_link_challenge_with_policy(
    channel: Channel,
    challenge_id: impl Into<String>,
    answer: impl Into<String>,
    policy: GrpcTimeoutPolicy,
) -> Result<LocalLinkCredentials, AuthError> {
    let policy = policy.validate()?;
    let mut client = ClientCommandsClient::new(GrpcDeadlineService::new_resolved(channel, policy));
    let request = LocalLinkSolveRequest {
        challenge_id: challenge_id.into(),
        answer: answer.into(),
    };
    let request = with_grpc_call_options(
        Request::new(request),
        GrpcCallOptions::new(
            GrpcTimeoutClass::CredentialSetup,
            GrpcTimeoutOutcome::MutationIndeterminate,
        ),
    );
    let started = std::time::Instant::now();
    let response: tonic::Response<LocalLinkSolveResponse> = client
        .account_local_link_solve_challenge(request)
        .await
        .map_err(|status| {
            deadline_or_auth_status(
                status,
                GrpcTimeoutClass::CredentialSetup,
                GrpcTimeoutOutcome::MutationIndeterminate,
                started.elapsed(),
            )
        })?;
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

fn deadline_or_auth_status(
    status: Status,
    class: GrpcTimeoutClass,
    outcome: GrpcTimeoutOutcome,
    elapsed: std::time::Duration,
) -> AuthError {
    GrpcDeadlineError::from_status(&status, class, outcome, elapsed).map_or_else(
        || AuthError::Status { source: status },
        |source| AuthError::Deadline { source },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credential_debug_output_is_redacted() {
        for auth in [
            SessionAuth::AppKey("APP_KEY_SECRET".to_owned()),
            SessionAuth::AccountKey("ACCOUNT_KEY_SECRET".to_owned()),
            SessionAuth::Mnemonic("MNEMONIC_SECRET".to_owned()),
            SessionAuth::Token("TOKEN_SECRET".to_owned()),
        ] {
            assert!(!format!("{auth:?}").contains("SECRET"));
        }
        let credentials = LocalLinkCredentials {
            app_key: "APP_KEY_SECRET".to_owned(),
            session_token: Some("TOKEN_SECRET".to_owned()),
        };
        assert!(!format!("{credentials:?}").contains("SECRET"));
    }
}
