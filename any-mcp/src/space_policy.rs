// any-mcp - bounded, workflow-oriented MCP server for Anytype
//
// SPDX-FileCopyrightText: 2026 Steve Schoettler
// SPDX-License-Identifier: Apache-2.0

//! Frozen Anytype space authority and policy-aware client resolution.

use std::{collections::BTreeSet, fmt, ops::Deref, sync::Arc};

use anytype::prelude::{AnytypeClient, AnytypeError};
use sha2::{Digest, Sha256};

use crate::{
    artifact_config::{SpaceConfig, SpaceReference},
    domain::SpaceId,
};

const GENERATION_DOMAIN: &[u8] = b"any-mcp/configuration-generation/v1\0";

/// Opaque process-local configuration generation.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct ConfigurationGeneration([u8; 32]);

impl fmt::Debug for ConfigurationGeneration {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ConfigurationGeneration(<opaque>)")
    }
}

impl ConfigurationGeneration {
    fn create(policy: &SpacePolicy) -> Result<Self, SpacePolicyError> {
        let mut process_instance = [0_u8; 16];
        getrandom::fill(&mut process_instance).map_err(|_| SpacePolicyError)?;
        let mut hasher = Sha256::new();
        hasher.update(GENERATION_DOMAIN);
        hasher.update(process_instance);
        match policy {
            SpacePolicy::AllReadWrite => hasher.update([0]),
            SpacePolicy::None => hasher.update([1]),
            SpacePolicy::OnlyReadWrite(identifiers) => {
                hasher.update([2]);
                for identifier in identifiers {
                    let bytes = identifier.as_str().as_bytes();
                    hasher.update(u64::try_from(bytes.len()).unwrap_or(u64::MAX).to_be_bytes());
                    hasher.update(bytes);
                }
            }
        }
        Ok(Self(hasher.finalize().into()))
    }

    #[cfg(test)]
    const fn test_value() -> Self {
        Self([0_u8; 32])
    }
}

/// Frozen canonical space policy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SpacePolicy {
    /// Every otherwise-authorized Anytype space is writable.
    AllReadWrite,
    /// No Anytype space is authorized.
    None,
    /// Only the canonical identifiers in this set are writable.
    OnlyReadWrite(BTreeSet<SpaceId>),
}

/// Immutable central authorization gate.
#[derive(Clone)]
pub struct SpaceAuthority {
    policy: SpacePolicy,
    generation: ConfigurationGeneration,
}

impl fmt::Debug for SpaceAuthority {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (kind, count) = match &self.policy {
            SpacePolicy::AllReadWrite => ("all_read_write", None),
            SpacePolicy::None => ("none", Some(0)),
            SpacePolicy::OnlyReadWrite(identifiers) => ("only_read_write", Some(identifiers.len())),
        };
        formatter
            .debug_struct("SpaceAuthority")
            .field("policy", &kind)
            .field("allowed_count", &count)
            .field("generation", &self.generation)
            .finish()
    }
}

impl SpaceAuthority {
    /// Resolves configured names once and freezes their canonical identifiers.
    ///
    /// Omitted `allowed` permits all otherwise-authorized spaces. An explicit
    /// empty list permits none. Resolution ambiguity, missing spaces, malformed
    /// returned identifiers, and post-resolution duplicates fail startup.
    ///
    /// # Errors
    ///
    /// Returns a fixed policy error without exposing configured names or IDs.
    pub async fn initialize(
        client: &AnytypeClient,
        config: &SpaceConfig,
    ) -> Result<Self, SpacePolicyError> {
        let policy = match &config.allowed {
            None => SpacePolicy::AllReadWrite,
            Some(entries) if entries.is_empty() => SpacePolicy::None,
            Some(entries) => {
                let mut identifiers = BTreeSet::new();
                for entry in entries {
                    let raw = match entry {
                        SpaceReference::Id(identifier) => identifier.clone(),
                        SpaceReference::Name(name) => client
                            .resolve_space_id(name)
                            .await
                            .map_err(|_| SpacePolicyError)?,
                    };
                    let identifier = SpaceId::new(raw).map_err(|_| SpacePolicyError)?;
                    if !identifiers.insert(identifier) {
                        return Err(SpacePolicyError);
                    }
                }
                SpacePolicy::OnlyReadWrite(identifiers)
            }
        };
        let generation = ConfigurationGeneration::create(&policy)?;
        Ok(Self { policy, generation })
    }

    /// Creates compatibility authority for internal fixture constructors.
    #[cfg(any(test, feature = "acceptance-harness"))]
    pub(crate) const fn allow_all_for_fixtures() -> Self {
        Self {
            policy: SpacePolicy::AllReadWrite,
            generation: ConfigurationGeneration([0_u8; 32]),
        }
    }

    #[cfg(test)]
    pub(crate) const fn from_policy_for_tests(policy: SpacePolicy) -> Self {
        Self {
            policy,
            generation: ConfigurationGeneration([0_u8; 32]),
        }
    }

    /// Returns the frozen policy.
    #[must_use]
    pub const fn policy(&self) -> &SpacePolicy {
        &self.policy
    }

    /// Returns the opaque process-local policy generation.
    #[must_use]
    pub const fn generation(&self) -> ConfigurationGeneration {
        self.generation
    }

    /// Validates and authorizes one already-resolved canonical identifier.
    ///
    /// # Errors
    ///
    /// Returns a fixed error for a malformed or disallowed identifier.
    pub fn authorize_resolved(&self, resolved: String) -> Result<SpaceId, SpacePolicyError> {
        let identifier = SpaceId::new(resolved).map_err(|_| SpacePolicyError)?;
        self.authorize_id(&identifier)?;
        Ok(identifier)
    }

    /// Authorizes one validated canonical identifier without name resolution.
    ///
    /// # Errors
    ///
    /// Returns a fixed error when the current frozen policy excludes it.
    pub fn authorize_id(&self, identifier: &SpaceId) -> Result<(), SpacePolicyError> {
        if self.is_allowed(identifier) {
            Ok(())
        } else {
            Err(SpacePolicyError)
        }
    }

    /// Returns whether a canonical identifier is admitted.
    #[must_use]
    pub fn is_allowed(&self, identifier: &SpaceId) -> bool {
        match &self.policy {
            SpacePolicy::AllReadWrite => true,
            SpacePolicy::None => false,
            SpacePolicy::OnlyReadWrite(identifiers) => identifiers.contains(identifier),
        }
    }
}

/// Anytype client whose reference resolver enforces frozen space policy.
///
/// Other `AnytypeClient` methods remain available through [`Deref`]. Handlers
/// use this type so every ordinary `resolve_space_id` call performs the common
/// authorization check before a domain builder can be constructed.
#[derive(Clone)]
pub struct PolicyClient {
    inner: Arc<AnytypeClient>,
    authority: Arc<SpaceAuthority>,
}

impl fmt::Debug for PolicyClient {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PolicyClient")
            .field("authority", &self.authority)
            .finish_non_exhaustive()
    }
}

impl PolicyClient {
    pub(crate) fn new(client: AnytypeClient, authority: SpaceAuthority) -> Self {
        Self {
            inner: Arc::new(client),
            authority: Arc::new(authority),
        }
    }

    /// Resolves one name or ID and checks the returned canonical identifier.
    ///
    /// The upstream resolver runs exactly once. Disallowed and malformed
    /// returned identifiers map to the ordinary payload-free forbidden class.
    ///
    /// # Errors
    ///
    /// Returns the resolver error or [`AnytypeError::Forbidden`] when policy
    /// rejects its canonical result.
    pub async fn resolve_space_id(&self, reference: &str) -> Result<String, AnytypeError> {
        let resolved = self.inner.resolve_space_id(reference).await?;
        self.authority
            .authorize_resolved(resolved)
            .map(|identifier| identifier.as_str().to_owned())
            .map_err(|_| AnytypeError::Forbidden)
    }

    /// Returns the frozen central authorization gate.
    #[must_use]
    pub fn space_authority(&self) -> &SpaceAuthority {
        self.authority.as_ref()
    }

    pub(crate) fn raw_clone(&self) -> AnytypeClient {
        self.inner.as_ref().clone()
    }
}

impl Deref for PolicyClient {
    type Target = AnytypeClient;

    fn deref(&self) -> &Self::Target {
        self.inner.as_ref()
    }
}

/// Fixed startup/runtime space policy error.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SpacePolicyError;

impl fmt::Display for SpacePolicyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Anytype space is not authorized by server policy")
    }
}

impl std::error::Error for SpacePolicyError {}

#[cfg(test)]
mod tests {
    use anytype::prelude::ClientConfig;

    use super::*;

    fn authority(policy: SpacePolicy) -> SpaceAuthority {
        SpaceAuthority {
            policy,
            generation: ConfigurationGeneration::test_value(),
        }
    }

    #[test]
    fn omitted_empty_and_selected_policies_are_distinct() {
        let id = SpaceId::new("space-1").expect("space ID");
        let all = authority(SpacePolicy::AllReadWrite);
        let none = authority(SpacePolicy::None);
        let only = authority(SpacePolicy::OnlyReadWrite(BTreeSet::from([id.clone()])));

        assert!(all.authorize_id(&id).is_ok());
        assert!(none.authorize_id(&id).is_err());
        assert!(only.authorize_id(&id).is_ok());
        assert!(
            only.authorize_id(&SpaceId::new("space-2").expect("space ID"))
                .is_err()
        );
    }

    #[test]
    fn malformed_resolved_ids_fail_before_policy_lookup() {
        let all = authority(SpacePolicy::AllReadWrite);
        assert!(all.authorize_resolved("../escape".to_owned()).is_err());
    }

    #[test]
    fn debug_output_contains_no_identifiers_or_generation() {
        let secret = SpaceId::new("secret-space-id").expect("space ID");
        let authority = authority(SpacePolicy::OnlyReadWrite(BTreeSet::from([secret.clone()])));
        let debug = format!("{authority:?}");

        assert!(!debug.contains(secret.as_str()));
        assert!(!debug.contains(&"00".repeat(32)));
        assert!(debug.contains("allowed_count"));
    }

    #[tokio::test]
    async fn configured_ids_freeze_without_name_resolution_io() {
        let client = AnytypeClient::with_config(ClientConfig {
            base_url: Some("http://127.0.0.1:1".to_owned()),
            keystore: Some("env".to_owned()),
            app_name: "space-policy-test".to_owned(),
            ..ClientConfig::default()
        })
        .expect("offline client");
        let config = SpaceConfig {
            read_only: false,
            allowed: Some(vec![SpaceReference::Id("bafy-space-allowed".to_owned())]),
        };
        let authority = SpaceAuthority::initialize(&client, &config)
            .await
            .expect("ID-only policy");

        assert!(
            authority
                .authorize_id(&SpaceId::new("bafy-space-allowed").expect("ID"))
                .is_ok()
        );
        assert!(
            authority
                .authorize_id(&SpaceId::new("bafy-space-denied").expect("ID"))
                .is_err()
        );
    }

    #[tokio::test]
    async fn policy_client_checks_canonical_resolver_result() {
        const ALLOWED: &str =
            "bafyreid5fvqlnsobih2keakcxjrrlpmly6kf37klzjzen4ibfdgalcdp4y.2tq5w93cr6oe7";
        const DENIED: &str =
            "bafyreie6n5l5nkbjal37su54cha4coy7qzuhrnajluzv5qd5jvtsrxkequ.2tq5w93cr6oe7";
        let client = AnytypeClient::with_config(ClientConfig {
            base_url: Some("http://127.0.0.1:1".to_owned()),
            keystore: Some("env".to_owned()),
            app_name: "space-policy-test".to_owned(),
            ..ClientConfig::default()
        })
        .expect("offline client");
        let allowed = SpaceId::new(ALLOWED).expect("ID");
        let client = PolicyClient::new(
            client,
            authority(SpacePolicy::OnlyReadWrite(BTreeSet::from(
                [allowed.clone()],
            ))),
        );

        assert_eq!(
            client
                .resolve_space_id(allowed.as_str())
                .await
                .expect("allowed"),
            allowed.as_str()
        );
        assert!(matches!(
            client.resolve_space_id(DENIED).await,
            Err(AnytypeError::Forbidden)
        ));
    }
}
