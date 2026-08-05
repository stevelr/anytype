// any-mcp - bounded, workflow-oriented MCP server for Anytype
//
// SPDX-FileCopyrightText: 2026 Steve Schoettler
// SPDX-License-Identifier: Apache-2.0

//! OAuth protected-resource profile: RFC 9728 metadata and JWT validation.
//!
//! `any-mcp` implements only the MCP protected-resource role. The external
//! authorization server owns OAuth 2.1, PKCE, consent, client registration,
//! refresh tokens, revocation, and user interaction. Access tokens are JWTs
//! from the one configured issuer, verified against a bounded, cached JWKS
//! fetched over HTTPS at startup. Token validation has no Anytype or
//! model-visible side effects, and raw tokens and claims are never logged.

use std::{
    collections::HashMap,
    fmt,
    sync::Arc,
    time::{Duration, Instant},
};

use bytes::Bytes;
use jsonwebtoken::{
    Algorithm, DecodingKey, Validation, decode, decode_header,
    jwk::{AlgorithmParameters, EllipticCurve, Jwk, JwkSet, KeyAlgorithm},
};
use serde::Deserialize;
use tokio::sync::{Mutex, RwLock};

use crate::http::{auth::AuthorizedPrincipal, config::OAuthResourceConfig};

/// Maximum accepted JWKS document size.
const JWKS_MAX_BYTES: usize = 1024 * 1024;
/// Maximum usable keys admitted from one JWKS document.
const JWKS_MAX_KEYS: usize = 32;
/// Maximum JWT `sub` length in Unicode scalars.
const MAX_SUBJECT_CHARS: usize = 256;
/// A successful JWKS is cached for at most one hour.
const JWKS_CACHE_TTL: Duration = Duration::from_secs(3600);
/// Refresh begins no later than five minutes before cache expiry.
const JWKS_REFRESH_AGE: Duration = Duration::from_secs(3300);

/// One usable verification key resolved from the JWKS.
struct VerificationKey {
    key: DecodingKey,
    algorithm: Algorithm,
}

impl fmt::Debug for VerificationKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("VerificationKey")
            .field("algorithm", &self.algorithm)
            .finish_non_exhaustive()
    }
}

type KeyMap = HashMap<String, Arc<VerificationKey>>;

struct JwksCache {
    keys: Arc<KeyMap>,
    fetched: Instant,
}

/// Fixed JWKS handling failure categories.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JwksError {
    /// The document exceeds the fixed byte bound.
    Oversized,
    /// The document is not a JWK set.
    Malformed,
    /// No usable asymmetric key, too many usable keys, or duplicate IDs.
    Keys,
    /// The document could not be retrieved.
    Fetch,
}

impl fmt::Display for JwksError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::Oversized => "JWKS document exceeds the supported size",
            Self::Malformed => "JWKS document is not a JWK set",
            Self::Keys => "JWKS document has no supported bounded key set",
            Self::Fetch => "JWKS document could not be retrieved",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for JwksError {}

/// Token rejection detail used only to bound the unknown-`kid` refresh.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum VerifyRejection {
    /// The token names a key the current JWKS does not contain.
    UnknownKid,
    /// Every other failure: grammar, algorithm, signature, or claims.
    Invalid,
}

/// OAuth resource-server validator for one configured issuer.
pub struct OAuthValidator {
    config: OAuthResourceConfig,
    challenge: String,
    metadata_document: Arc<str>,
    client: reqwest::Client,
    jwks: RwLock<JwksCache>,
    refresh: Mutex<()>,
}

impl OAuthValidator {
    /// Builds the validator and retrieves the initial JWKS.
    ///
    /// The fetch uses HTTPS (guaranteed by configuration validation), the
    /// supplied startup deadline, a 1 MiB response cap, and no automatic
    /// redirects.
    ///
    /// # Errors
    ///
    /// Returns a fixed [`JwksError`] category without echoing any URI.
    pub(crate) async fn start(
        config: OAuthResourceConfig,
        fetch_timeout: Duration,
    ) -> Result<Self, JwksError> {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(fetch_timeout)
            .build()
            .map_err(|_| JwksError::Fetch)?;
        let document = fetch_bounded(&client, config.jwks_uri.as_str()).await?;
        let keys = parse_jwks(&document)?;
        let metadata_document = Arc::from(metadata_document(&config));
        let challenge = challenge_value(&config);
        Ok(Self {
            config,
            challenge,
            metadata_document,
            client,
            jwks: RwLock::new(JwksCache {
                keys: Arc::new(keys),
                fetched: Instant::now(),
            }),
            refresh: Mutex::new(()),
        })
    }

    /// Returns the fixed public RFC 9728 protected-resource document.
    pub(crate) fn metadata_document(&self) -> Arc<str> {
        self.metadata_document.clone()
    }

    /// Returns the fixed `WWW-Authenticate` challenge value.
    pub(crate) fn challenge(&self) -> &str {
        &self.challenge
    }

    /// Verifies one bearer credential and reduces it to a principal.
    ///
    /// An unknown key ID triggers at most one bounded JWKS refresh before
    /// rejection. A refresh failure may use a still-unexpired cached set but
    /// never an expired one.
    pub(crate) async fn authenticate_credential(
        &self,
        credential: &[u8],
    ) -> Result<AuthorizedPrincipal, ()> {
        let (keys, age) = {
            let cache = self.jwks.read().await;
            (cache.keys.clone(), cache.fetched.elapsed())
        };
        let keys = if age >= JWKS_REFRESH_AGE {
            let refreshed = self.refresh_once().await;
            if !refreshed && age >= JWKS_CACHE_TTL {
                return Err(());
            }
            self.jwks.read().await.keys.clone()
        } else {
            keys
        };
        match verify(credential, &keys, &self.config) {
            Ok(principal) => Ok(principal),
            Err(VerifyRejection::UnknownKid) => {
                if !self.refresh_once().await {
                    return Err(());
                }
                let keys = self.jwks.read().await.keys.clone();
                verify(credential, &keys, &self.config).map_err(|_| ())
            }
            Err(VerifyRejection::Invalid) => Err(()),
        }
    }

    /// Fetches and installs a fresh JWKS exactly once across racing callers.
    async fn refresh_once(&self) -> bool {
        let _guard = self.refresh.lock().await;
        // A racer may have refreshed while this caller waited for the lock.
        if self.jwks.read().await.fetched.elapsed() < Duration::from_secs(1) {
            return true;
        }
        let Ok(document) = fetch_bounded(&self.client, self.config.jwks_uri.as_str()).await else {
            return false;
        };
        let Ok(keys) = parse_jwks(&document) else {
            return false;
        };
        let mut cache = self.jwks.write().await;
        cache.keys = Arc::new(keys);
        cache.fetched = Instant::now();
        true
    }
}

/// Retrieves one bounded document without redirects.
async fn fetch_bounded(client: &reqwest::Client, url: &str) -> Result<Bytes, JwksError> {
    let response = client
        .get(url)
        .header(http::header::ACCEPT, "application/json")
        .send()
        .await
        .map_err(|_| JwksError::Fetch)?;
    if !response.status().is_success() {
        return Err(JwksError::Fetch);
    }
    let mut collected = Vec::new();
    let mut response = response;
    while let Some(chunk) = response.chunk().await.map_err(|_| JwksError::Fetch)? {
        if collected.len().saturating_add(chunk.len()) > JWKS_MAX_BYTES {
            return Err(JwksError::Oversized);
        }
        collected.extend_from_slice(&chunk);
    }
    Ok(Bytes::from(collected))
}

/// Parses a JWKS document into the bounded usable key map.
///
/// Only asymmetric `RS256`, `ES256`, and `EdDSA` keys with a key ID are
/// usable. `none`, HMAC, and unknown algorithms are never admitted. More
/// than 32 usable keys, duplicate key IDs, or zero usable keys reject the
/// document.
fn parse_jwks(document: &[u8]) -> Result<KeyMap, JwksError> {
    if document.len() > JWKS_MAX_BYTES {
        return Err(JwksError::Oversized);
    }
    let set: JwkSet = serde_json::from_slice(document).map_err(|_| JwksError::Malformed)?;
    let mut keys = KeyMap::new();
    for jwk in &set.keys {
        let Some(kid) = jwk.common.key_id.clone() else {
            continue;
        };
        let Some(algorithm) = usable_algorithm(jwk) else {
            continue;
        };
        let Ok(key) = DecodingKey::from_jwk(jwk) else {
            continue;
        };
        if keys
            .insert(kid, Arc::new(VerificationKey { key, algorithm }))
            .is_some()
        {
            return Err(JwksError::Keys);
        }
        if keys.len() > JWKS_MAX_KEYS {
            return Err(JwksError::Keys);
        }
    }
    if keys.is_empty() {
        return Err(JwksError::Keys);
    }
    Ok(keys)
}

/// Maps one JWK to a permitted asymmetric algorithm, or `None` if unusable.
fn usable_algorithm(jwk: &Jwk) -> Option<Algorithm> {
    let declared = match jwk.common.key_algorithm {
        Some(KeyAlgorithm::RS256) => Some(Algorithm::RS256),
        Some(KeyAlgorithm::ES256) => Some(Algorithm::ES256),
        Some(KeyAlgorithm::EdDSA) => Some(Algorithm::EdDSA),
        Some(_) => return None,
        None => None,
    };
    let inherent = match &jwk.algorithm {
        AlgorithmParameters::RSA(_) => Algorithm::RS256,
        AlgorithmParameters::EllipticCurve(parameters) => {
            if parameters.curve == EllipticCurve::P256 {
                Algorithm::ES256
            } else {
                return None;
            }
        }
        AlgorithmParameters::OctetKeyPair(parameters) => {
            if parameters.curve == EllipticCurve::Ed25519 {
                Algorithm::EdDSA
            } else {
                return None;
            }
        }
        AlgorithmParameters::OctetKey(_) => return None,
        AlgorithmParameters::Other(_) => return None,
        // AlgorithmParameters is declared non-exhaustive in jsonwebtoken v11.0 so we must use a catch-all
        _ => return None,
    };
    match declared {
        Some(declared) if declared != inherent => None,
        _ => Some(inherent),
    }
}

#[derive(Deserialize)]
struct Claims {
    sub: String,
    #[serde(default)]
    scope: Option<String>,
}

/// Verifies one JWT against the resolved key map and exact configuration.
fn verify(
    credential: &[u8],
    keys: &KeyMap,
    config: &OAuthResourceConfig,
) -> Result<AuthorizedPrincipal, VerifyRejection> {
    let token = std::str::from_utf8(credential).map_err(|_| VerifyRejection::Invalid)?;
    let header = decode_header(token).map_err(|_| VerifyRejection::Invalid)?;
    // Embedded key material and certificate pointers are never trusted.
    if header.jku.is_some() || header.jwk.is_some() || header.x5u.is_some() || header.x5c.is_some()
    {
        return Err(VerifyRejection::Invalid);
    }
    if !matches!(
        header.alg,
        Algorithm::RS256 | Algorithm::ES256 | Algorithm::EdDSA
    ) {
        return Err(VerifyRejection::Invalid);
    }
    let kid = header.kid.as_deref().ok_or(VerifyRejection::Invalid)?;
    let key = keys.get(kid).ok_or(VerifyRejection::UnknownKid)?;
    if key.algorithm != header.alg {
        return Err(VerifyRejection::Invalid);
    }

    let mut validation = Validation::new(key.algorithm);
    validation.leeway = 0;
    validation.set_audience(&[config.audience.as_str()]);
    validation.set_issuer(&[config.issuer.as_str()]);
    validation.set_required_spec_claims(&["exp", "aud", "iss", "sub"]);
    validation.validate_nbf = true;
    let data =
        decode::<Claims>(token, &key.key, &validation).map_err(|_| VerifyRejection::Invalid)?;

    let subject_chars = data.claims.sub.chars().count();
    if subject_chars == 0 || subject_chars > MAX_SUBJECT_CHARS {
        return Err(VerifyRejection::Invalid);
    }
    let scope_granted = data
        .claims
        .scope
        .as_deref()
        .is_some_and(|scope| scope.split(' ').any(|token| token == config.required_scope));
    if !scope_granted {
        return Err(VerifyRejection::Invalid);
    }

    let mut material = Vec::with_capacity(config.issuer.len() + 1 + data.claims.sub.len());
    material.extend_from_slice(config.issuer.as_bytes());
    material.push(0);
    material.extend_from_slice(data.claims.sub.as_bytes());
    Ok(AuthorizedPrincipal::from_identity_material(
        "oauth-resource-server",
        &material,
    ))
}

/// Builds the fixed RFC 9728 protected-resource metadata document.
fn metadata_document(config: &OAuthResourceConfig) -> String {
    serde_json::json!({
        "resource": config.resource_uri,
        "authorization_servers": [config.authorization_server],
        "scopes_supported": [config.required_scope],
        "bearer_methods_supported": ["header"],
    })
    .to_string()
}

/// Builds the fixed 401 challenge naming the metadata URI and scope.
fn challenge_value(config: &OAuthResourceConfig) -> String {
    format!(
        "Bearer resource_metadata=\"{}\", scope=\"{}\"",
        protected_resource_metadata_uri(&config.resource_uri),
        config.required_scope
    )
}

/// Derives the path-specific RFC 9728 well-known URI for the resource.
fn protected_resource_metadata_uri(resource_uri: &str) -> String {
    // The configured resource URI is already validated as an exact HTTPS URI
    // ending in `/mcp` with no userinfo, query, or fragment.
    resource_uri.rfind("/mcp").map_or_else(
        || resource_uri.to_owned(),
        |index| {
            let (origin, path) = resource_uri.split_at(index);
            format!("{origin}/.well-known/oauth-protected-resource{path}")
        },
    )
}

#[cfg(test)]
mod tests {
    use jsonwebtoken::{EncodingKey, Header, encode};
    use serde_json::json;

    use super::*;

    /// Throwaway Ed25519 keypair generated for these tests only.
    const TEST_PRIVATE_PEM: &str = "-----BEGIN PRIVATE KEY-----\nMC4CAQAwBQYDK2VwBCIEICDNEqdf60txt+W2scD1zdF0MS78TiSJcOPUyIm0MaZe\n-----END PRIVATE KEY-----\n";
    const TEST_PUBLIC_X: &str = "j5aYWRSjdjA44EExQYpcNoz9nK2_BqGHY9vIfyQ9uXE";
    const TEST_KID: &str = "test-key";

    fn test_config() -> OAuthResourceConfig {
        OAuthResourceConfig {
            resource_uri: "https://mcp.example.com/mcp".to_owned(),
            issuer: "https://auth.example.com".to_owned(),
            authorization_server: "https://auth.example.com".to_owned(),
            jwks_uri: url::Url::parse("https://auth.example.com/jwks.json").expect("test url"),
            audience: "https://mcp.example.com/mcp".to_owned(),
            required_scope: "anytype.mcp".to_owned(),
        }
    }

    fn test_jwk(kid: &str) -> serde_json::Value {
        json!({
            "kty": "OKP",
            "crv": "Ed25519",
            "x": TEST_PUBLIC_X,
            "kid": kid,
            "alg": "EdDSA",
        })
    }

    fn test_keys() -> KeyMap {
        parse_jwks(
            json!({ "keys": [test_jwk(TEST_KID)] })
                .to_string()
                .as_bytes(),
        )
        .expect("test jwks")
    }

    fn now() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_secs()
    }

    fn claims() -> serde_json::Value {
        json!({
            "iss": "https://auth.example.com",
            "aud": "https://mcp.example.com/mcp",
            "sub": "user-1",
            "exp": now() + 300,
            "scope": "profile anytype.mcp",
        })
    }

    fn mint(claims: &serde_json::Value, kid: Option<&str>) -> String {
        let mut header = Header::new(Algorithm::EdDSA);
        header.kid = kid.map(str::to_owned);
        encode(
            &header,
            claims,
            &EncodingKey::from_ed_pem(TEST_PRIVATE_PEM.as_bytes()).expect("test key"),
        )
        .expect("mint token")
    }

    #[test]
    fn metadata_and_challenge_are_fixed_and_exact() {
        let config = test_config();
        let document: serde_json::Value =
            serde_json::from_str(&metadata_document(&config)).expect("metadata json");
        assert_eq!(
            document,
            json!({
                "resource": "https://mcp.example.com/mcp",
                "authorization_servers": ["https://auth.example.com"],
                "scopes_supported": ["anytype.mcp"],
                "bearer_methods_supported": ["header"],
            })
        );
        assert_eq!(
            challenge_value(&config),
            "Bearer resource_metadata=\"https://mcp.example.com/.well-known/oauth-protected-resource/mcp\", scope=\"anytype.mcp\""
        );
    }

    #[test]
    fn valid_tokens_reduce_to_a_stable_issuer_subject_principal() {
        let keys = test_keys();
        let config = test_config();
        let token = mint(&claims(), Some(TEST_KID));
        let first = verify(token.as_bytes(), &keys, &config).expect("valid token");
        let second = verify(mint(&claims(), Some(TEST_KID)).as_bytes(), &keys, &config)
            .expect("valid token");
        assert_eq!(first, second);

        let mut other = claims();
        other["sub"] = json!("user-2");
        let other =
            verify(mint(&other, Some(TEST_KID)).as_bytes(), &keys, &config).expect("valid token");
        assert_ne!(first, other);
    }

    #[test]
    fn claim_validation_is_exact() {
        let keys = test_keys();
        let config = test_config();
        for (name, value) in [
            ("iss", json!("https://other.example.com")),
            ("aud", json!("https://other.example.com/mcp")),
            ("exp", json!(now() - 10)),
            ("nbf", json!(now() + 300)),
            ("sub", json!("")),
            ("sub", json!("s".repeat(257))),
            ("scope", json!("profile")),
            ("scope", json!("anytype.mcp.extra")),
            ("scope", serde_json::Value::Null),
        ] {
            let mut claims = claims();
            claims[name] = value.clone();
            let token = mint(&claims, Some(TEST_KID));
            assert_eq!(
                verify(token.as_bytes(), &keys, &config).unwrap_err(),
                VerifyRejection::Invalid,
                "{name}={value}"
            );
        }
        for missing in ["iss", "aud", "exp", "sub", "scope"] {
            let mut claims = claims();
            claims
                .as_object_mut()
                .expect("claims object")
                .remove(missing);
            let token = mint(&claims, Some(TEST_KID));
            assert_eq!(
                verify(token.as_bytes(), &keys, &config).unwrap_err(),
                VerifyRejection::Invalid,
                "missing {missing}"
            );
        }
        // A future nbf rejects now but a past nbf is accepted when present.
        let mut claims = claims();
        claims["nbf"] = json!(now() - 10);
        assert!(verify(mint(&claims, Some(TEST_KID)).as_bytes(), &keys, &config).is_ok());
    }

    #[test]
    fn token_grammar_and_key_binding_fail_closed() {
        let keys = test_keys();
        let config = test_config();

        assert_eq!(
            verify(b"not-a-jwt", &keys, &config).unwrap_err(),
            VerifyRejection::Invalid
        );
        assert_eq!(
            verify(mint(&claims(), None).as_bytes(), &keys, &config).unwrap_err(),
            VerifyRejection::Invalid
        );
        assert_eq!(
            verify(
                mint(&claims(), Some("other-key")).as_bytes(),
                &keys,
                &config
            )
            .unwrap_err(),
            VerifyRejection::UnknownKid
        );

        // Embedded key material is never trusted.
        let mut header = Header::new(Algorithm::EdDSA);
        header.kid = Some(TEST_KID.to_owned());
        header.jku = Some("https://evil.test/jwks.json".to_owned());
        let token = encode(
            &header,
            &claims(),
            &EncodingKey::from_ed_pem(TEST_PRIVATE_PEM.as_bytes()).expect("test key"),
        )
        .expect("mint token");
        assert_eq!(
            verify(token.as_bytes(), &keys, &config).unwrap_err(),
            VerifyRejection::Invalid
        );

        // A symmetric token cannot reach signature verification.
        let hmac = encode(
            &Header::new(Algorithm::HS256),
            &claims(),
            &EncodingKey::from_secret(b"shared"),
        )
        .expect("mint hmac token");
        assert_eq!(
            verify(hmac.as_bytes(), &keys, &config).unwrap_err(),
            VerifyRejection::Invalid
        );
    }

    #[test]
    fn jwks_parsing_is_bounded_and_exact() {
        assert!(parse_jwks(json!({ "keys": [test_jwk("a")] }).to_string().as_bytes()).is_ok());

        assert_eq!(parse_jwks(b"not json").unwrap_err(), JwksError::Malformed);
        assert_eq!(
            parse_jwks(json!({ "keys": [] }).to_string().as_bytes()).unwrap_err(),
            JwksError::Keys
        );
        // Keys without an ID are unusable; a document with only such keys
        // has no usable key.
        let mut anonymous = test_jwk("a");
        anonymous.as_object_mut().expect("jwk").remove("kid");
        assert_eq!(
            parse_jwks(json!({ "keys": [anonymous] }).to_string().as_bytes()).unwrap_err(),
            JwksError::Keys
        );
        // Symmetric keys are never usable.
        let symmetric = json!({ "kty": "oct", "k": "c2VjcmV0", "kid": "sym", "alg": "HS256" });
        assert_eq!(
            parse_jwks(json!({ "keys": [symmetric] }).to_string().as_bytes()).unwrap_err(),
            JwksError::Keys
        );
        // A declared algorithm that contradicts the key type is unusable.
        let mut mismatched = test_jwk("a");
        mismatched["alg"] = json!("RS256");
        assert_eq!(
            parse_jwks(json!({ "keys": [mismatched] }).to_string().as_bytes()).unwrap_err(),
            JwksError::Keys
        );
        assert_eq!(
            parse_jwks(
                json!({ "keys": [test_jwk("dup"), test_jwk("dup")] })
                    .to_string()
                    .as_bytes()
            )
            .unwrap_err(),
            JwksError::Keys
        );
        let many = (0..33)
            .map(|i| test_jwk(&format!("k{i}")))
            .collect::<Vec<_>>();
        assert_eq!(
            parse_jwks(json!({ "keys": many }).to_string().as_bytes()).unwrap_err(),
            JwksError::Keys
        );
        let oversized = vec![b' '; JWKS_MAX_BYTES + 1];
        assert_eq!(parse_jwks(&oversized).unwrap_err(), JwksError::Oversized);
    }

    /// Minimal loopback JWKS server for refresh-behavior tests.
    async fn serve_jwks(
        responses: Vec<String>,
    ) -> (std::net::SocketAddr, Arc<std::sync::atomic::AtomicUsize>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind jwks server");
        let address = listener.local_addr().expect("jwks address");
        let hits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = hits.clone();
        tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    return;
                };
                let index = counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let body = responses
                    .get(index.min(responses.len() - 1))
                    .cloned()
                    .unwrap_or_default();
                let mut buffer = vec![0u8; 4096];
                let _ = stream.read(&mut buffer).await;
                let response = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(response.as_bytes()).await;
            }
        });
        (address, hits)
    }

    #[tokio::test]
    async fn unknown_kid_triggers_at_most_one_refresh() {
        let first = json!({ "keys": [test_jwk(TEST_KID)] }).to_string();
        let second = json!({ "keys": [test_jwk(TEST_KID), test_jwk("rotated")] }).to_string();
        let (address, hits) = serve_jwks(vec![first, second]).await;

        let mut config = test_config();
        config.jwks_uri =
            url::Url::parse(&format!("http://{address}/jwks.json")).expect("test url");
        let validator = OAuthValidator::start(config, Duration::from_secs(5))
            .await
            .expect("start validator");
        assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 1);

        // Await one second so the refresh race-window guard does not treat
        // the startup fetch as a completed refresh.
        tokio::time::sleep(Duration::from_millis(1100)).await;
        let rotated = mint(&claims(), Some("rotated"));
        let principal = validator
            .authenticate_credential(rotated.as_bytes())
            .await
            .expect("rotated key after refresh");
        let stable = validator
            .authenticate_credential(mint(&claims(), Some(TEST_KID)).as_bytes())
            .await
            .expect("stable key");
        assert_eq!(principal, stable);
        assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 2);

        // A still-unknown kid refreshes once more and then rejects without
        // looping.
        tokio::time::sleep(Duration::from_millis(1100)).await;
        let unknown = mint(&claims(), Some("never"));
        assert!(
            validator
                .authenticate_credential(unknown.as_bytes())
                .await
                .is_err()
        );
        assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 3);

        // An invalid token never touches the network.
        assert!(validator.authenticate_credential(b"junk").await.is_err());
        assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn startup_fails_closed_on_bad_documents() {
        let (address, _hits) = serve_jwks(vec!["not json".to_owned()]).await;
        let mut config = test_config();
        config.jwks_uri =
            url::Url::parse(&format!("http://{address}/jwks.json")).expect("test url");
        assert_eq!(
            OAuthValidator::start(config, Duration::from_secs(5))
                .await
                .map(|_| ())
                .unwrap_err(),
            JwksError::Malformed
        );

        let mut config = test_config();
        config.jwks_uri = url::Url::parse("http://127.0.0.1:1/jwks.json").expect("test url");
        assert_eq!(
            OAuthValidator::start(config, Duration::from_secs(1))
                .await
                .map(|_| ())
                .unwrap_err(),
            JwksError::Fetch
        );
    }
}
