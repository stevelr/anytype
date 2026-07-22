// any-mcp - bounded, workflow-oriented MCP server for Anytype
//
// SPDX-FileCopyrightText: 2026 Steve Schoettler
// SPDX-License-Identifier: Apache-2.0

//! Environment-backed runtime configuration.

use std::{fmt, time::Duration};

use anytype::prelude::{
    ClientConfig, MAX_DOCUMENT_RESPONSE_BYTES, MAX_JSON_RESPONSE_BYTES, ResponseLimits,
};
use schemars::JsonSchema;
use serde::Serialize;

use crate::optional_toolsets::{
    OPTIONAL_TOOLSETS_ENV, OptionalRetryPolicyError, OptionalSelectorError,
    OptionalToolsetMetadata, OptionalToolsetSelection, admit_optional_retry_policy,
    production_optional_metadata,
};

const DEFAULT_KEYSTORE_SERVICE: &str = "anyr";
const DEFAULT_MAX_CONCURRENCY: usize = 8;
const MAX_MAX_CONCURRENCY: usize = 64;
const DEFAULT_REQUEST_TIMEOUT_SECS: u64 = 30;
const MAX_REQUEST_TIMEOUT_SECS: u64 = 300;
const DEFAULT_STARTUP_TIMEOUT_SECS: u64 = 15;
const MAX_STARTUP_TIMEOUT_SECS: u64 = 120;
const DEFAULT_JSON_RESPONSE_BYTES: u64 = 8 * 1024 * 1024;
const DEFAULT_DOCUMENT_RESPONSE_BYTES: u64 = MAX_DOCUMENT_RESPONSE_BYTES;
const EXPERIMENTAL_PROTOCOL_VALUE: &str = "experimental-2026-07-28";
const ANYTYPE_RATE_LIMIT_MAX_RETRIES: &str = "ANYTYPE_RATE_LIMIT_MAX_RETRIES";
const DEFAULT_RATE_LIMIT_MAX_RETRIES: u32 = 5;

/// Stdio protocol selected for one `any-mcp` process.
///
/// The released initialize-based MCP protocol is the production default. The
/// stateless 2026-07-28 adapter is deliberately available only through its
/// exact experimental environment value.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ProtocolMode {
    /// Latest released initialize/initialized MCP lifecycle.
    #[default]
    Stable,
    /// Experimental stateless MCP 2026-07-28 preview.
    Experimental20260728,
}

/// Startup-selected application catalog profile.
///
/// Profiles select stable sets of complete workflow contracts. They never
/// change the schema of a tool name and are independent of read-only mode.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ApplicationProfile {
    /// Common existing-document search, read, and exact-edit workflows.
    #[default]
    Compact,
    /// Complete fourteen-tool Phase 1 compatibility catalog.
    Standard,
}

impl ApplicationProfile {
    /// Returns the stable configuration/wire name of this profile.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Compact => "compact",
            Self::Standard => "standard",
        }
    }

    /// Returns whether this profile/access selection requires authenticated
    /// gRPC availability before its complete catalog can be advertised.
    ///
    /// Standard read-write includes `object_archive`, whose independent
    /// archived-presence proof uses Anytype's gRPC search surface. All other
    /// Phase 1 catalogs are complete over authenticated HTTP alone.
    #[must_use]
    pub const fn requires_grpc(self, read_only: bool) -> bool {
        matches!(self, Self::Standard) && !read_only
    }
}

/// Validated configuration for one `any-mcp` process.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeConfig {
    /// Stdio protocol mode selected at process startup.
    pub protocol_mode: ProtocolMode,
    /// Startup-selected stable application catalog profile.
    pub profile: ApplicationProfile,
    /// Whether the production catalog omits and rejects mutating workflows.
    pub read_only: bool,
    /// Canonical optional registry selection resolved at startup.
    pub optional_toolsets: OptionalToolsetSelection,
    /// Maximum number of concurrent Anytype operations.
    pub max_concurrency: usize,
    /// End-to-end timeout for one Anytype operation, including permit wait.
    pub request_timeout: Duration,
    /// Timeout applied independently to each startup health check.
    pub startup_timeout: Duration,
    /// Maximum bytes buffered for ordinary Anytype JSON responses.
    pub json_response_bytes: u64,
    /// Maximum bytes buffered for a document/object JSON response.
    pub document_response_bytes: u64,
    anytype_url: Option<String>,
    grpc_endpoint: Option<String>,
    keystore: Option<String>,
    keystore_service: String,
    admitted_optional_max_retries: Option<u32>,
}

impl RuntimeConfig {
    /// Loads and validates configuration from process environment variables.
    ///
    /// Anytype settings use `ANYTYPE_URL`, `ANYTYPE_GRPC_ENDPOINT`,
    /// `ANYTYPE_KEYSTORE`, and `ANYTYPE_KEYSTORE_SERVICE`. Operational limits
    /// use `ANY_MCP_PROTOCOL`, `ANY_MCP_PROFILE`, `ANY_MCP_READ_ONLY`,
    /// `ANY_MCP_MAX_CONCURRENCY`, `ANY_MCP_REQUEST_TIMEOUT_SECS`,
    /// `ANY_MCP_STARTUP_TIMEOUT_SECS`, `ANY_MCP_JSON_RESPONSE_BYTES`, and
    /// `ANY_MCP_DOCUMENT_RESPONSE_BYTES`, and `ANY_MCP_TOOLSETS`. A nonempty
    /// optional selection also admits the effective
    /// `ANYTYPE_RATE_LIMIT_MAX_RETRIES` policy before client construction.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError`] when an environment value is non-Unicode,
    /// violates an exact protocol/profile/read-only switch grammar, is
    /// non-numeric or zero, or exceeds its defensive maximum.
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::from_lookup(|name| match std::env::var(name) {
            Ok(value) => Ok(Some(value)),
            Err(std::env::VarError::NotPresent) => Ok(None),
            Err(std::env::VarError::NotUnicode(_)) => Err(ConfigError::non_unicode(name)),
        })
    }

    /// Builds the `anytype-api` configuration without copying credentials into
    /// the MCP runtime.
    #[must_use]
    pub fn client_config(&self) -> ClientConfig {
        let mut config = ClientConfig {
            base_url: self.anytype_url.clone(),
            grpc_endpoint: self.grpc_endpoint.clone(),
            keystore: self.keystore.clone(),
            keystore_service: Some(self.keystore_service.clone()),
            app_name: env!("CARGO_PKG_NAME").to_string(),
            response_limits: ResponseLimits {
                json_bytes: self.json_response_bytes,
                document_bytes: self.document_response_bytes,
                ..ResponseLimits::default()
            },
            ..ClientConfig::default()
        };
        if let Some(max_retries) = self.admitted_optional_max_retries {
            config.rate_limit_max_retries = max_retries;
        }
        config
    }

    fn from_lookup<F>(lookup: F) -> Result<Self, ConfigError>
    where
        F: Fn(&'static str) -> Result<Option<String>, ConfigError>,
    {
        let metadata = production_optional_metadata();
        Self::from_lookup_with_optional_metadata(lookup, &metadata)
    }

    fn from_lookup_with_optional_metadata<F>(
        lookup: F,
        optional_metadata: &[OptionalToolsetMetadata],
    ) -> Result<Self, ConfigError>
    where
        F: Fn(&'static str) -> Result<Option<String>, ConfigError>,
    {
        let optional_value = lookup(OPTIONAL_TOOLSETS_ENV)
            .map_err(|_| ConfigError::fixed(OptionalSelectorError::Invalid.to_string()))?;
        let optional_toolsets = OptionalToolsetSelection::parse(optional_value, optional_metadata)
            .map_err(|error| ConfigError::fixed(error.to_string()))?;
        let admitted_optional_max_retries = if optional_toolsets.is_empty() {
            None
        } else {
            let raw = lookup(ANYTYPE_RATE_LIMIT_MAX_RETRIES)
                .map_err(|_| ConfigError::fixed(OptionalRetryPolicyError.to_string()))?;
            let effective = raw
                .and_then(|value| value.parse::<u32>().ok())
                .unwrap_or(DEFAULT_RATE_LIMIT_MAX_RETRIES);
            admit_optional_retry_policy(&optional_toolsets, effective)
                .map_err(|error| ConfigError::fixed(error.to_string()))?;
            Some(effective)
        };

        let protocol_mode = parse_protocol_mode(lookup("ANY_MCP_PROTOCOL")?)?;
        let profile = parse_profile(lookup("ANY_MCP_PROFILE")?)?;
        let read_only = parse_read_only(lookup("ANY_MCP_READ_ONLY")?)?;
        let max_concurrency = parse_bounded(
            "ANY_MCP_MAX_CONCURRENCY",
            lookup("ANY_MCP_MAX_CONCURRENCY")?,
            DEFAULT_MAX_CONCURRENCY,
            MAX_MAX_CONCURRENCY,
        )?;
        let request_timeout_secs = parse_bounded(
            "ANY_MCP_REQUEST_TIMEOUT_SECS",
            lookup("ANY_MCP_REQUEST_TIMEOUT_SECS")?,
            DEFAULT_REQUEST_TIMEOUT_SECS,
            MAX_REQUEST_TIMEOUT_SECS,
        )?;
        let startup_timeout_secs = parse_bounded(
            "ANY_MCP_STARTUP_TIMEOUT_SECS",
            lookup("ANY_MCP_STARTUP_TIMEOUT_SECS")?,
            DEFAULT_STARTUP_TIMEOUT_SECS,
            MAX_STARTUP_TIMEOUT_SECS,
        )?;
        let json_response_bytes = parse_bounded(
            "ANY_MCP_JSON_RESPONSE_BYTES",
            lookup("ANY_MCP_JSON_RESPONSE_BYTES")?,
            DEFAULT_JSON_RESPONSE_BYTES,
            MAX_JSON_RESPONSE_BYTES,
        )?;
        let document_response_bytes = parse_bounded(
            "ANY_MCP_DOCUMENT_RESPONSE_BYTES",
            lookup("ANY_MCP_DOCUMENT_RESPONSE_BYTES")?,
            DEFAULT_DOCUMENT_RESPONSE_BYTES,
            MAX_DOCUMENT_RESPONSE_BYTES,
        )?;
        if document_response_bytes < json_response_bytes {
            return Err(ConfigError::invalid(
                "ANY_MCP_DOCUMENT_RESPONSE_BYTES",
                "must be at least ANY_MCP_JSON_RESPONSE_BYTES",
            ));
        }

        Ok(Self {
            protocol_mode,
            profile,
            read_only,
            optional_toolsets,
            max_concurrency,
            request_timeout: Duration::from_secs(request_timeout_secs),
            startup_timeout: Duration::from_secs(startup_timeout_secs),
            json_response_bytes,
            document_response_bytes,
            anytype_url: non_empty(lookup("ANYTYPE_URL")?),
            grpc_endpoint: non_empty(lookup("ANYTYPE_GRPC_ENDPOINT")?),
            keystore: non_empty(lookup("ANYTYPE_KEYSTORE")?),
            keystore_service: non_empty(lookup("ANYTYPE_KEYSTORE_SERVICE")?)
                .unwrap_or_else(|| DEFAULT_KEYSTORE_SERVICE.to_string()),
            admitted_optional_max_retries,
        })
    }
}

fn parse_protocol_mode(value: Option<String>) -> Result<ProtocolMode, ConfigError> {
    match value.as_deref() {
        None | Some("stable") => Ok(ProtocolMode::Stable),
        Some(EXPERIMENTAL_PROTOCOL_VALUE) => Ok(ProtocolMode::Experimental20260728),
        Some(_) => Err(ConfigError::invalid(
            "ANY_MCP_PROTOCOL",
            "must be exactly stable or experimental-2026-07-28",
        )),
    }
}

fn parse_profile(value: Option<String>) -> Result<ApplicationProfile, ConfigError> {
    match value.as_deref() {
        None | Some("compact") => Ok(ApplicationProfile::Compact),
        Some("standard") => Ok(ApplicationProfile::Standard),
        Some(_) => Err(ConfigError::invalid(
            "ANY_MCP_PROFILE",
            "must be exactly compact or standard",
        )),
    }
}

fn parse_read_only(value: Option<String>) -> Result<bool, ConfigError> {
    match value.as_deref() {
        None | Some("0") => Ok(false),
        Some("1") => Ok(true),
        Some(_) => Err(ConfigError::invalid(
            "ANY_MCP_READ_ONLY",
            "must be exactly 0 or 1",
        )),
    }
}

fn non_empty(value: Option<String>) -> Option<String> {
    value.filter(|value| !value.trim().is_empty())
}

fn parse_bounded<T>(
    name: &'static str,
    value: Option<String>,
    default: T,
    maximum: T,
) -> Result<T, ConfigError>
where
    T: Copy + Ord + From<u8> + std::str::FromStr,
{
    let Some(value) = value else {
        return Ok(default);
    };
    let parsed = value
        .parse::<T>()
        .map_err(|_| ConfigError::invalid(name, "must be a positive integer"))?;
    if parsed < T::from(1) || parsed > maximum {
        return Err(ConfigError::invalid(name, "is outside the supported range"));
    }
    Ok(parsed)
}

/// A safe configuration diagnostic which never contains an environment value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfigError {
    variable: &'static str,
    problem: &'static str,
    fixed: Option<String>,
}

impl ConfigError {
    fn invalid(variable: &'static str, problem: &'static str) -> Self {
        Self {
            variable,
            problem,
            fixed: None,
        }
    }

    fn non_unicode(variable: &'static str) -> Self {
        Self::invalid(variable, "must contain valid Unicode")
    }

    fn fixed(message: String) -> Self {
        Self {
            variable: "",
            problem: "",
            fixed: Some(message),
        }
    }
}

impl fmt::Display for ConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(fixed) = &self.fixed {
            formatter.write_str(fixed)
        } else {
            write!(
                formatter,
                "invalid configuration: {} {}",
                self.variable, self.problem
            )
        }
    }
}

impl std::error::Error for ConfigError {}

#[cfg(test)]
mod tests {
    use std::{cell::Cell, collections::HashMap};

    use super::*;

    fn config(values: &[(&str, &str)]) -> Result<RuntimeConfig, ConfigError> {
        config_with_optional(values, &[])
    }

    fn config_with_optional(
        values: &[(&str, &str)],
        metadata: &[OptionalToolsetMetadata],
    ) -> Result<RuntimeConfig, ConfigError> {
        let values = values
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect::<HashMap<_, _>>();
        RuntimeConfig::from_lookup_with_optional_metadata(
            |name| Ok(values.get(name).cloned()),
            metadata,
        )
    }

    #[test]
    fn defaults_are_bounded_and_reuse_anyr_keystore_service() {
        let config = config(&[]).expect("default configuration");

        assert_eq!(config.max_concurrency, 8);
        assert_eq!(config.protocol_mode, ProtocolMode::Stable);
        assert_eq!(config.profile, ApplicationProfile::Compact);
        assert!(!config.read_only);
        assert!(config.optional_toolsets.is_empty());
        assert_eq!(config.request_timeout, Duration::from_secs(30));
        assert_eq!(config.startup_timeout, Duration::from_secs(15));
        assert_eq!(config.json_response_bytes, 8 * 1024 * 1024);
        assert_eq!(config.document_response_bytes, MAX_DOCUMENT_RESPONSE_BYTES);
        assert_eq!(
            config.client_config().keystore_service.as_deref(),
            Some("anyr")
        );
    }

    #[test]
    fn default_document_budget_is_routed_to_anytype_client() {
        let config = config(&[]).expect("default configuration");
        let client = config.client_config();

        assert_eq!(
            client.response_limits.document_bytes,
            MAX_DOCUMENT_RESPONSE_BYTES
        );
        assert_eq!(client.response_limits.json_bytes, 8 * 1024 * 1024);
    }

    #[test]
    fn maps_supported_anytype_environment_settings() {
        let config = config(&[
            ("ANYTYPE_URL", "http://127.0.0.1:31012"),
            ("ANYTYPE_GRPC_ENDPOINT", "127.0.0.1:31013"),
            ("ANYTYPE_KEYSTORE", "env"),
            ("ANYTYPE_KEYSTORE_SERVICE", "custom-service"),
            ("ANY_MCP_PROFILE", "compact"),
            ("ANY_MCP_READ_ONLY", "1"),
            ("ANY_MCP_PROTOCOL", "experimental-2026-07-28"),
            ("ANY_MCP_MAX_CONCURRENCY", "16"),
            ("ANY_MCP_REQUEST_TIMEOUT_SECS", "45"),
            ("ANY_MCP_STARTUP_TIMEOUT_SECS", "20"),
            ("ANY_MCP_JSON_RESPONSE_BYTES", "1048576"),
            ("ANY_MCP_DOCUMENT_RESPONSE_BYTES", "2097152"),
        ])
        .expect("valid configuration");
        let client = config.client_config();

        assert_eq!(client.base_url.as_deref(), Some("http://127.0.0.1:31012"));
        assert_eq!(client.grpc_endpoint.as_deref(), Some("127.0.0.1:31013"));
        assert_eq!(client.keystore.as_deref(), Some("env"));
        assert_eq!(client.keystore_service.as_deref(), Some("custom-service"));
        assert_eq!(config.max_concurrency, 16);
        assert_eq!(config.profile, ApplicationProfile::Compact);
        assert!(config.read_only);
        assert_eq!(config.protocol_mode, ProtocolMode::Experimental20260728);
        assert_eq!(config.request_timeout, Duration::from_secs(45));
        assert_eq!(client.response_limits.json_bytes, 1_048_576);
        assert_eq!(client.response_limits.document_bytes, 2_097_152);
    }

    #[test]
    fn errors_name_the_variable_without_echoing_its_value() {
        let secret_like_value = "token-do-not-echo";
        let error = config(&[("ANY_MCP_MAX_CONCURRENCY", secret_like_value)])
            .expect_err("invalid numeric setting");
        let message = error.to_string();

        assert!(message.contains("ANY_MCP_MAX_CONCURRENCY"));
        assert!(!message.contains(secret_like_value));
    }

    #[test]
    fn rejects_zero_and_values_above_defensive_maxima() {
        assert!(config(&[("ANY_MCP_MAX_CONCURRENCY", "0")]).is_err());
        assert!(config(&[("ANY_MCP_MAX_CONCURRENCY", "65")]).is_err());
        assert!(config(&[("ANY_MCP_REQUEST_TIMEOUT_SECS", "301")]).is_err());
        assert!(config(&[("ANY_MCP_STARTUP_TIMEOUT_SECS", "121")]).is_err());
        assert!(config(&[("ANY_MCP_JSON_RESPONSE_BYTES", "0")]).is_err());
        assert!(config(&[("ANY_MCP_JSON_RESPONSE_BYTES", "67108865")]).is_err());
        assert!(
            config(&[
                ("ANY_MCP_JSON_RESPONSE_BYTES", "2097152"),
                ("ANY_MCP_DOCUMENT_RESPONSE_BYTES", "1048576"),
            ])
            .is_err()
        );
    }

    #[test]
    fn read_only_parser_is_exact_and_fails_closed() {
        assert!(!config(&[]).unwrap().read_only);
        assert!(!config(&[("ANY_MCP_READ_ONLY", "0")]).unwrap().read_only);
        assert!(config(&[("ANY_MCP_READ_ONLY", "1")]).unwrap().read_only);
        for invalid in ["", "true", "TRUE", "01", " 1", "1 ", "2", "-1"] {
            let error = config(&[("ANY_MCP_READ_ONLY", invalid)]).unwrap_err();
            let message = error.to_string();
            assert!(message.contains("ANY_MCP_READ_ONLY"));
        }
    }
    #[test]
    fn protocol_mode_is_stable_by_default_and_preview_is_exact() {
        assert_eq!(config(&[]).unwrap().protocol_mode, ProtocolMode::Stable);
        assert_eq!(
            config(&[("ANY_MCP_PROTOCOL", "stable")])
                .unwrap()
                .protocol_mode,
            ProtocolMode::Stable
        );
        assert_eq!(
            config(&[("ANY_MCP_PROTOCOL", "experimental-2026-07-28")])
                .unwrap()
                .protocol_mode,
            ProtocolMode::Experimental20260728
        );
        for invalid in [
            "",
            "preview",
            "2026-07-28",
            "Experimental-2026-07-28",
            " experimental-2026-07-28",
            "experimental-2026-07-28 ",
        ] {
            let error = config(&[("ANY_MCP_PROTOCOL", invalid)]).unwrap_err();
            let message = error.to_string();
            assert!(message.contains("ANY_MCP_PROTOCOL"));
        }
    }

    #[test]
    fn application_profile_parser_is_exact_and_secret_safe() {
        assert_eq!(config(&[]).unwrap().profile, ApplicationProfile::Compact);
        assert_eq!(
            config(&[("ANY_MCP_PROFILE", "compact")]).unwrap().profile,
            ApplicationProfile::Compact
        );
        assert_eq!(
            config(&[("ANY_MCP_PROFILE", "standard")]).unwrap().profile,
            ApplicationProfile::Standard
        );
        for invalid in ["", "default", "Compact", " compact", "standard "] {
            let error = config(&[("ANY_MCP_PROFILE", invalid)]).unwrap_err();
            let message = error.to_string();
            assert!(message.contains("ANY_MCP_PROFILE"));
        }
        let secret_like = "token-do-not-echo-as-profile";
        let message = config(&[("ANY_MCP_PROFILE", secret_like)])
            .unwrap_err()
            .to_string();
        assert!(!message.contains(secret_like));
    }

    #[test]
    fn only_standard_read_write_requires_grpc() {
        assert!(!ApplicationProfile::Compact.requires_grpc(false));
        assert!(!ApplicationProfile::Compact.requires_grpc(true));
        assert!(ApplicationProfile::Standard.requires_grpc(false));
        assert!(!ApplicationProfile::Standard.requires_grpc(true));
    }

    #[test]
    fn non_unicode_profile_failure_names_only_the_variable() {
        let error = RuntimeConfig::from_lookup(|name| {
            if name == "ANY_MCP_PROFILE" {
                Err(ConfigError::non_unicode(name))
            } else {
                Ok(None)
            }
        })
        .unwrap_err();
        assert_eq!(
            error.to_string(),
            "invalid configuration: ANY_MCP_PROFILE must contain valid Unicode"
        );
    }

    #[test]
    fn optional_selector_is_exact_canonical_and_landed_only() {
        let metadata = [
            OptionalToolsetMetadata::new("zeta", false),
            OptionalToolsetMetadata::new("alpha", false),
        ];
        let selected =
            config_with_optional(&[(OPTIONAL_TOOLSETS_ENV, "zeta,alpha")], &metadata).unwrap();
        assert_eq!(
            selected.optional_toolsets.names().collect::<Vec<_>>(),
            ["alpha", "zeta"]
        );
        assert_eq!(selected.client_config().rate_limit_max_retries, 5);

        let unsupported = config(&[(OPTIONAL_TOOLSETS_ENV, "schema")]).unwrap_err();
        assert_eq!(
            unsupported.to_string(),
            "unsupported optional toolset selector"
        );
    }

    #[test]
    fn optional_selector_diagnostics_are_fixed_and_secret_safe() {
        let metadata = [OptionalToolsetMetadata::new("alpha", false)];
        for (value, expected) in [
            ("secret_like", "invalid optional toolset selector"),
            ("alpha,alpha", "duplicate optional toolset selector"),
            ("secret-like", "unsupported optional toolset selector"),
        ] {
            let error =
                config_with_optional(&[(OPTIONAL_TOOLSETS_ENV, value)], &metadata).unwrap_err();
            assert_eq!(error.to_string(), expected);
            assert!(!error.to_string().contains(value));
        }
        let non_unicode = RuntimeConfig::from_lookup_with_optional_metadata(
            |name| {
                if name == OPTIONAL_TOOLSETS_ENV {
                    Err(ConfigError::non_unicode(name))
                } else {
                    Ok(None)
                }
            },
            &metadata,
        )
        .unwrap_err();
        assert_eq!(non_unicode.to_string(), "invalid optional toolset selector");
    }

    #[test]
    fn optional_retry_policy_is_admitted_before_client_construction() {
        let metadata = [OptionalToolsetMetadata::new("alpha", false)];
        for admitted in 1..=5 {
            let value = admitted.to_string();
            let config = config_with_optional(
                &[
                    (OPTIONAL_TOOLSETS_ENV, "alpha"),
                    (ANYTYPE_RATE_LIMIT_MAX_RETRIES, &value),
                ],
                &metadata,
            )
            .unwrap();
            assert_eq!(config.client_config().rate_limit_max_retries, admitted);
        }
        for rejected in ["0", "6", "4294967295"] {
            let error = config_with_optional(
                &[
                    (OPTIONAL_TOOLSETS_ENV, "alpha"),
                    (ANYTYPE_RATE_LIMIT_MAX_RETRIES, rejected),
                ],
                &metadata,
            )
            .unwrap_err();
            assert_eq!(error.to_string(), "invalid optional retry policy");
            assert!(!error.to_string().contains(rejected));
        }

        assert!(
            config(&[(ANYTYPE_RATE_LIMIT_MAX_RETRIES, "0")])
                .unwrap()
                .optional_toolsets
                .is_empty()
        );
    }

    #[test]
    fn optional_selector_is_read_once_and_retry_rejection_precedes_other_config() {
        let selector_reads = Cell::new(0usize);
        let profile_reads = Cell::new(0usize);
        let metadata = [OptionalToolsetMetadata::new("alpha", false)];
        let error = RuntimeConfig::from_lookup_with_optional_metadata(
            |name| match name {
                OPTIONAL_TOOLSETS_ENV => {
                    selector_reads.set(selector_reads.get() + 1);
                    Ok(Some("alpha".to_owned()))
                }
                ANYTYPE_RATE_LIMIT_MAX_RETRIES => Ok(Some("0".to_owned())),
                "ANY_MCP_PROFILE" => {
                    profile_reads.set(profile_reads.get() + 1);
                    Ok(Some("invalid".to_owned()))
                }
                _ => Ok(None),
            },
            &metadata,
        )
        .unwrap_err();

        assert_eq!(error.to_string(), "invalid optional retry policy");
        assert_eq!(selector_reads.get(), 1);
        assert_eq!(profile_reads.get(), 0);
    }
}
