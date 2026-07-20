// any-mcp - bounded, workflow-oriented MCP server for Anytype
//
// SPDX-FileCopyrightText: 2026 Steve Schoettler
// SPDX-License-Identifier: Apache-2.0

//! Environment-backed runtime configuration.

use std::{fmt, time::Duration};

use anytype::prelude::ClientConfig;

const DEFAULT_KEYSTORE_SERVICE: &str = "anyr";
const DEFAULT_MAX_CONCURRENCY: usize = 8;
const MAX_MAX_CONCURRENCY: usize = 64;
const DEFAULT_REQUEST_TIMEOUT_SECS: u64 = 30;
const MAX_REQUEST_TIMEOUT_SECS: u64 = 300;
const DEFAULT_STARTUP_TIMEOUT_SECS: u64 = 15;
const MAX_STARTUP_TIMEOUT_SECS: u64 = 120;

/// Validated configuration for one `any-mcp` process.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeConfig {
    /// Maximum number of concurrent Anytype operations.
    pub max_concurrency: usize,
    /// End-to-end timeout for one Anytype operation, including permit wait.
    pub request_timeout: Duration,
    /// Timeout applied independently to each startup health check.
    pub startup_timeout: Duration,
    anytype_url: Option<String>,
    grpc_endpoint: Option<String>,
    keystore: Option<String>,
    keystore_service: String,
}

impl RuntimeConfig {
    /// Loads and validates configuration from process environment variables.
    ///
    /// Anytype settings use `ANYTYPE_URL`, `ANYTYPE_GRPC_ENDPOINT`,
    /// `ANYTYPE_KEYSTORE`, and `ANYTYPE_KEYSTORE_SERVICE`. Operational limits
    /// use `ANY_MCP_MAX_CONCURRENCY`, `ANY_MCP_REQUEST_TIMEOUT_SECS`, and
    /// `ANY_MCP_STARTUP_TIMEOUT_SECS`.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError`] when an environment value is non-Unicode,
    /// non-numeric, zero, or above its defensive maximum.
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
        ClientConfig {
            base_url: self.anytype_url.clone(),
            grpc_endpoint: self.grpc_endpoint.clone(),
            keystore: self.keystore.clone(),
            keystore_service: Some(self.keystore_service.clone()),
            app_name: env!("CARGO_PKG_NAME").to_string(),
            ..ClientConfig::default()
        }
    }

    fn from_lookup<F>(lookup: F) -> Result<Self, ConfigError>
    where
        F: Fn(&'static str) -> Result<Option<String>, ConfigError>,
    {
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

        Ok(Self {
            max_concurrency,
            request_timeout: Duration::from_secs(request_timeout_secs),
            startup_timeout: Duration::from_secs(startup_timeout_secs),
            anytype_url: non_empty(lookup("ANYTYPE_URL")?),
            grpc_endpoint: non_empty(lookup("ANYTYPE_GRPC_ENDPOINT")?),
            keystore: non_empty(lookup("ANYTYPE_KEYSTORE")?),
            keystore_service: non_empty(lookup("ANYTYPE_KEYSTORE_SERVICE")?)
                .unwrap_or_else(|| DEFAULT_KEYSTORE_SERVICE.to_string()),
        })
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
}

impl ConfigError {
    fn invalid(variable: &'static str, problem: &'static str) -> Self {
        Self { variable, problem }
    }

    fn non_unicode(variable: &'static str) -> Self {
        Self::invalid(variable, "must contain valid Unicode")
    }
}

impl fmt::Display for ConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "invalid configuration: {} {}",
            self.variable, self.problem
        )
    }
}

impl std::error::Error for ConfigError {}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn config(values: &[(&str, &str)]) -> Result<RuntimeConfig, ConfigError> {
        let values = values
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect::<HashMap<_, _>>();
        RuntimeConfig::from_lookup(|name| Ok(values.get(name).cloned()))
    }

    #[test]
    fn defaults_are_bounded_and_reuse_anyr_keystore_service() {
        let config = config(&[]).expect("default configuration");

        assert_eq!(config.max_concurrency, 8);
        assert_eq!(config.request_timeout, Duration::from_secs(30));
        assert_eq!(config.startup_timeout, Duration::from_secs(15));
        assert_eq!(
            config.client_config().keystore_service.as_deref(),
            Some("anyr")
        );
    }

    #[test]
    fn maps_supported_anytype_environment_settings() {
        let config = config(&[
            ("ANYTYPE_URL", "http://127.0.0.1:31012"),
            ("ANYTYPE_GRPC_ENDPOINT", "127.0.0.1:31013"),
            ("ANYTYPE_KEYSTORE", "env"),
            ("ANYTYPE_KEYSTORE_SERVICE", "custom-service"),
            ("ANY_MCP_MAX_CONCURRENCY", "16"),
            ("ANY_MCP_REQUEST_TIMEOUT_SECS", "45"),
            ("ANY_MCP_STARTUP_TIMEOUT_SECS", "20"),
        ])
        .expect("valid configuration");
        let client = config.client_config();

        assert_eq!(client.base_url.as_deref(), Some("http://127.0.0.1:31012"));
        assert_eq!(client.grpc_endpoint.as_deref(), Some("127.0.0.1:31013"));
        assert_eq!(client.keystore.as_deref(), Some("env"));
        assert_eq!(client.keystore_service.as_deref(), Some("custom-service"));
        assert_eq!(config.max_concurrency, 16);
        assert_eq!(config.request_timeout, Duration::from_secs(45));
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
    }
}
