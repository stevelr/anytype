// any-mcp - bounded, workflow-oriented MCP server for Anytype
//
// SPDX-FileCopyrightText: 2026 Steve Schoettler
// SPDX-License-Identifier: Apache-2.0

//! Bearer extraction, authentication profiles, and principal reduction.
//!
//! Every non-preflight, non-metadata request is authenticated on every
//! request; session possession alone is never authorization. A valid
//! credential is reduced to an immutable [`AuthorizedPrincipal`] carrying
//! only an internal principal key and the single `anytype.mcp` authority.
//! Raw tokens, claims, and headers never travel past this module.

use http::{HeaderMap, header};
use sha2::{Digest, Sha256};

use crate::http::{oauth::OAuthValidator, secret::StaticToken};

/// Maximum accepted `Authorization` header length in bytes.
pub(crate) const MAX_AUTHORIZATION_BYTES: usize = 1024;

/// One authenticated request principal reduced to an internal key.
///
/// The key is a keyed digest of profile-specific identity material; raw
/// issuer, subject, and token bytes are unrecoverable from it. Equality of
/// keys defines principal identity for sessions, cursors, and idempotency.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct AuthorizedPrincipal {
    key: [u8; 32],
}

impl AuthorizedPrincipal {
    pub(crate) fn from_identity_material(domain: &str, material: &[u8]) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(b"any-mcp.principal.v1\x00");
        hasher.update(domain.as_bytes());
        hasher.update([0u8]);
        hasher.update(material);
        Self {
            key: hasher.finalize().into(),
        }
    }

    /// Returns the opaque internal principal key.
    #[must_use]
    pub(crate) const fn key(&self) -> &[u8; 32] {
        &self.key
    }
}

/// Fixed authentication rejection categories.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AuthRejection {
    /// Missing, malformed, duplicate, or non-Bearer credential, or a valid
    /// credential that failed verification. Returned as a fixed 401.
    Unauthorized,
    /// The `Authorization` header exceeds the fixed bound. Returned as 431.
    Oversized,
}

/// Selected authentication profile for one process.
pub(crate) enum Authenticator {
    /// One fixed principal proved by the loaded static token.
    StaticToken(StaticToken),
    /// MCP protected-resource role against one configured issuer.
    OAuthResourceServer(Box<OAuthValidator>),
    /// Test seam admitting one synthetic principal for shell tests.
    #[cfg(test)]
    SyntheticAllow,
}

impl Authenticator {
    /// Authenticates one request from its headers.
    ///
    /// The bearer credential is extracted with the common grammar and
    /// verified by the selected profile. No Anytype credential access or
    /// model-visible side effect occurs here; only the OAuth profile may
    /// perform its bounded JWKS refresh.
    pub(crate) async fn authenticate(
        &self,
        headers: &HeaderMap,
    ) -> Result<AuthorizedPrincipal, AuthRejection> {
        let credential = extract_bearer(headers)?;
        match self {
            Self::StaticToken(token) => {
                if token.matches(credential) {
                    Ok(AuthorizedPrincipal::from_identity_material(
                        "static-token",
                        b"local-operator",
                    ))
                } else {
                    Err(AuthRejection::Unauthorized)
                }
            }
            Self::OAuthResourceServer(validator) => validator
                .authenticate_credential(credential)
                .await
                .map_err(|()| AuthRejection::Unauthorized),
            #[cfg(test)]
            Self::SyntheticAllow => Ok(AuthorizedPrincipal::from_identity_material(
                "synthetic",
                credential,
            )),
        }
    }

    /// Returns the fixed `WWW-Authenticate` challenge for a 401 response.
    pub(crate) fn challenge(&self) -> &str {
        match self {
            Self::StaticToken(_) => "Bearer",
            Self::OAuthResourceServer(validator) => validator.challenge(),
            #[cfg(test)]
            Self::SyntheticAllow => "Bearer",
        }
    }
}

/// Extracts exactly one well-formed bearer credential.
///
/// Multiple `Authorization` headers, non-Bearer schemes, empty credentials,
/// whitespace or control characters, and oversized headers are rejected
/// before any comparison.
fn extract_bearer(headers: &HeaderMap) -> Result<&[u8], AuthRejection> {
    let mut values = headers.get_all(header::AUTHORIZATION).iter();
    let value = values.next().ok_or(AuthRejection::Unauthorized)?;
    if values.next().is_some() {
        return Err(AuthRejection::Unauthorized);
    }
    let value = value.as_bytes();
    if value.len() > MAX_AUTHORIZATION_BYTES {
        return Err(AuthRejection::Oversized);
    }
    let scheme = value.get(..7).ok_or(AuthRejection::Unauthorized)?;
    if !scheme.eq_ignore_ascii_case(b"Bearer ") {
        return Err(AuthRejection::Unauthorized);
    }
    let credential = &value[7..];
    if credential.is_empty() || !credential.iter().all(|byte| byte.is_ascii_graphic()) {
        return Err(AuthRejection::Unauthorized);
    }
    Ok(credential)
}

#[cfg(test)]
mod tests {
    use http::HeaderValue;

    use super::*;

    fn headers(value: &str) -> HeaderMap {
        let mut map = HeaderMap::new();
        map.insert(
            header::AUTHORIZATION,
            HeaderValue::from_str(value).expect("test header"),
        );
        map
    }

    #[tokio::test]
    async fn bearer_grammar_is_exact() {
        let auth = Authenticator::SyntheticAllow;
        assert!(auth.authenticate(&headers("Bearer abc123")).await.is_ok());
        assert!(auth.authenticate(&headers("bearer abc123")).await.is_ok());
        for invalid in [
            "Basic abc123",
            "Bearer",
            "Bearer ",
            "Bearer a b",
            "Bearer\tabc",
            "abc123",
            "",
        ] {
            assert_eq!(
                auth.authenticate(&headers(invalid)).await.unwrap_err(),
                AuthRejection::Unauthorized,
                "value {invalid:?}"
            );
        }
        assert_eq!(
            auth.authenticate(&HeaderMap::new()).await.unwrap_err(),
            AuthRejection::Unauthorized
        );
    }

    #[tokio::test]
    async fn duplicate_authorization_headers_are_rejected() {
        let auth = Authenticator::SyntheticAllow;
        let mut map = headers("Bearer abc123");
        map.append(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer abc124"),
        );
        assert_eq!(
            auth.authenticate(&map).await.unwrap_err(),
            AuthRejection::Unauthorized
        );
    }

    #[tokio::test]
    async fn oversized_authorization_returns_dedicated_category() {
        let auth = Authenticator::SyntheticAllow;
        let long = format!("Bearer {}", "a".repeat(MAX_AUTHORIZATION_BYTES));
        assert_eq!(
            auth.authenticate(&headers(&long)).await.unwrap_err(),
            AuthRejection::Oversized
        );
        let exactly = format!("Bearer {}", "a".repeat(MAX_AUTHORIZATION_BYTES - 7));
        assert!(auth.authenticate(&headers(&exactly)).await.is_ok());
    }

    #[test]
    fn principal_keys_are_stable_domain_separated_and_opaque() {
        let first = AuthorizedPrincipal::from_identity_material("static-token", b"subject");
        let second = AuthorizedPrincipal::from_identity_material("static-token", b"subject");
        let other_domain = AuthorizedPrincipal::from_identity_material("oauth", b"subject");
        let other_subject = AuthorizedPrincipal::from_identity_material("static-token", b"other");
        assert_eq!(first, second);
        assert_ne!(first, other_domain);
        assert_ne!(first, other_subject);
        assert!(!format!("{first:?}").contains("subject"));
    }
}
