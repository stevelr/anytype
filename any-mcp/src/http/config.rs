// any-mcp - bounded, workflow-oriented MCP server for Anytype
//
// SPDX-FileCopyrightText: 2026 Steve Schoettler
// SPDX-License-Identifier: Apache-2.0

//! Exact environment configuration for the Streamable HTTP transport.
//!
//! All transport configuration is read exactly once at startup. Missing,
//! non-Unicode, duplicate, malformed, unknown, or out-of-range values fail
//! before Anytype probes or a listener bind. Diagnostics use fixed variable
//! names and categories and never echo a configured value.

use std::{
    fmt,
    net::SocketAddr,
    path::{Path, PathBuf},
    time::Duration,
};

use url::Url;

const TRANSPORT_ENV: &str = "ANY_MCP_TRANSPORT";
const BIND_ENV: &str = "ANY_MCP_HTTP_BIND";
const ALLOWED_HOSTS_ENV: &str = "ANY_MCP_HTTP_ALLOWED_HOSTS";
const ALLOWED_ORIGINS_ENV: &str = "ANY_MCP_HTTP_ALLOWED_ORIGINS";
const AUTH_ENV: &str = "ANY_MCP_HTTP_AUTH";
const TOKEN_FILE_ENV: &str = "ANY_MCP_HTTP_TOKEN_FILE";
const RESOURCE_URI_ENV: &str = "ANY_MCP_HTTP_RESOURCE_URI";
const ISSUER_ENV: &str = "ANY_MCP_HTTP_ISSUER";
const AUTHORIZATION_SERVER_ENV: &str = "ANY_MCP_HTTP_AUTHORIZATION_SERVER";
const JWKS_URI_ENV: &str = "ANY_MCP_HTTP_JWKS_URI";
const AUDIENCE_ENV: &str = "ANY_MCP_HTTP_AUDIENCE";
const REQUIRED_SCOPE_ENV: &str = "ANY_MCP_HTTP_REQUIRED_SCOPE";
const MAX_SESSIONS_ENV: &str = "ANY_MCP_HTTP_MAX_SESSIONS";
const REQUESTS_PER_MINUTE_ENV: &str = "ANY_MCP_HTTP_REQUESTS_PER_MINUTE";
const SHUTDOWN_SECS_ENV: &str = "ANY_MCP_HTTP_SHUTDOWN_SECS";

/// Every recognized `ANY_MCP_HTTP_*` variable. A present variable with any
/// other name in this namespace is rejected as unknown configuration.
const KNOWN_HTTP_VARS: [&str; 14] = [
    BIND_ENV,
    ALLOWED_HOSTS_ENV,
    ALLOWED_ORIGINS_ENV,
    AUTH_ENV,
    TOKEN_FILE_ENV,
    RESOURCE_URI_ENV,
    ISSUER_ENV,
    AUTHORIZATION_SERVER_ENV,
    JWKS_URI_ENV,
    AUDIENCE_ENV,
    REQUIRED_SCOPE_ENV,
    MAX_SESSIONS_ENV,
    REQUESTS_PER_MINUTE_ENV,
    SHUTDOWN_SECS_ENV,
];

const HTTP_VAR_PREFIX: &str = "ANY_MCP_HTTP_";

const DEFAULT_BIND: &str = "127.0.0.1:8000";
const DEFAULT_MAX_SESSIONS: u32 = 32;
const MAX_MAX_SESSIONS: u32 = 256;
const DEFAULT_REQUESTS_PER_MINUTE: u32 = 120;
const MAX_REQUESTS_PER_MINUTE: u32 = 600;
const DEFAULT_SHUTDOWN_SECS: u32 = 10;
const MAX_SHUTDOWN_SECS: u32 = 30;
const MAX_LIST_ENTRIES: usize = 16;
const MAX_LIST_ENTRY_BYTES: usize = 256;
const DEFAULT_REQUIRED_SCOPE: &str = "anytype.mcp";
const MAX_SCOPE_CHARS: usize = 64;
const MAX_AUDIENCE_CHARS: usize = 256;

/// Transport selected for one `any-mcp` process.
///
/// Absent or exact `stdio` keeps the production stdio default; exact
/// `streamable-http` opts into the authenticated loopback HTTP listener. One
/// process never serves both.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TransportSelection {
    /// Bounded stdio framing over the process's stdin/stdout.
    Stdio,
    /// Authenticated Streamable HTTP on a loopback listener.
    StreamableHttp(Box<HttpConfig>),
}

impl TransportSelection {
    /// Loads and validates the transport selection from process environment
    /// variables.
    ///
    /// In stdio mode every `ANY_MCP_HTTP_*` variable is rejected so a broken
    /// deployment cannot silently ignore its HTTP intent. In HTTP mode the
    /// complete authentication profile is required before any network or
    /// keystore activity.
    ///
    /// # Errors
    ///
    /// Returns [`HttpConfigError`] naming the failed variable and fixed
    /// problem category without echoing any configured value.
    pub fn from_env() -> Result<Self, HttpConfigError> {
        let present = std::env::vars_os()
            .filter_map(|(name, _)| {
                let name = name.to_string_lossy();
                name.starts_with(HTTP_VAR_PREFIX).then(|| name.into_owned())
            })
            .collect::<Vec<_>>();
        Self::from_lookup(
            |name| match std::env::var(name) {
                Ok(value) => Ok(Some(value)),
                Err(std::env::VarError::NotPresent) => Ok(None),
                Err(std::env::VarError::NotUnicode(_)) => {
                    Err(HttpConfigError::invalid(name, "must contain valid Unicode"))
                }
            },
            &present,
        )
    }

    pub(crate) fn from_lookup<F>(
        lookup: F,
        present_http_vars: &[String],
    ) -> Result<Self, HttpConfigError>
    where
        F: Fn(&'static str) -> Result<Option<String>, HttpConfigError>,
    {
        if present_http_vars
            .iter()
            .any(|name| !KNOWN_HTTP_VARS.contains(&name.as_str()))
        {
            return Err(HttpConfigError::fixed(
                "unknown ANY_MCP_HTTP_ configuration variable",
            ));
        }
        match lookup(TRANSPORT_ENV)?.as_deref() {
            None | Some("stdio") => {
                if present_http_vars.is_empty() {
                    Ok(Self::Stdio)
                } else {
                    Err(HttpConfigError::fixed(
                        "ANY_MCP_HTTP_ variables require ANY_MCP_TRANSPORT=streamable-http",
                    ))
                }
            }
            Some("streamable-http") => Ok(Self::StreamableHttp(Box::new(HttpConfig::from_lookup(
                lookup,
            )?))),
            Some(_) => Err(HttpConfigError::invalid(
                TRANSPORT_ENV,
                "must be exactly stdio or streamable-http",
            )),
        }
    }
}

/// Validated Streamable HTTP listener configuration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HttpConfig {
    /// Loopback socket address the listener binds.
    pub bind: SocketAddr,
    /// Exact `Host` authorities admitted by the DNS-rebinding gate.
    pub allowed_hosts: Vec<HostAuthority>,
    /// Exact origins admitted by the browser gate. Empty rejects every
    /// request that carries an `Origin` header.
    pub allowed_origins: Vec<AllowedOrigin>,
    /// Required MCP authentication profile.
    pub auth: HttpAuthConfig,
    /// Maximum concurrently initialized or initializing stable sessions.
    pub max_sessions: u32,
    /// Process-global request admissions per fixed one-minute window.
    pub requests_per_minute: u32,
    /// Graceful shutdown drain deadline.
    pub shutdown: Duration,
}

/// MCP authentication profile selected by `ANY_MCP_HTTP_AUTH`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HttpAuthConfig {
    /// One fixed principal proved by a strong local static bearer token.
    StaticToken {
        /// Absolute regular-file path holding the single token.
        token_file: PathBuf,
    },
    /// MCP protected-resource role against one external authorization server.
    OAuthResourceServer(Box<OAuthResourceConfig>),
}

/// OAuth protected-resource configuration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OAuthResourceConfig {
    /// Canonical externally visible HTTPS resource URI ending in `/mcp`.
    pub resource_uri: String,
    /// Exact HTTPS issuer required in every JWT `iss` claim.
    pub issuer: String,
    /// Exact HTTPS authorization server advertised by resource metadata.
    /// v1 requires it to equal [`Self::issuer`].
    pub authorization_server: String,
    /// Exact HTTPS JWKS document URI.
    pub jwks_uri: Url,
    /// Exact JWT audience required in every access token.
    pub audience: String,
    /// Exact scope token required in every access token.
    pub required_scope: String,
}

/// One exact `(host, optional port)` authority admitted by the `Host` gate.
///
/// Hosts compare case-insensitively through lowercase normalization. An
/// authority configured without a port admits the host at any port, matching
/// the documented loopback deployment shape; an authority with a port admits
/// exactly that port.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostAuthority {
    host: String,
    port: Option<u16>,
}

impl HostAuthority {
    /// Returns whether an already-normalized request authority matches.
    ///
    /// `host` must be lowercase with IPv6 brackets removed; `port` is the
    /// port serialized in the request authority, when present.
    #[must_use]
    pub fn matches(&self, host: &str, port: Option<u16>) -> bool {
        self.host == host && self.port.is_none_or(|allowed| port == Some(allowed))
    }
}

/// One exact allowed origin as a `(scheme, host, effective port)` tuple.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AllowedOrigin {
    serialized: String,
    scheme: OriginScheme,
    host: String,
    effective_port: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OriginScheme {
    Http,
    Https,
}

impl OriginScheme {
    const fn default_port(self) -> u16 {
        match self {
            Self::Http => 80,
            Self::Https => 443,
        }
    }
}

impl AllowedOrigin {
    /// Returns the exact configured serialization for CORS response headers.
    #[must_use]
    pub fn serialized(&self) -> &str {
        &self.serialized
    }

    /// Returns whether a request `Origin` value matches this exact tuple.
    ///
    /// The candidate is parsed with the same grammar used for configuration;
    /// malformed candidates never match.
    #[must_use]
    pub fn matches_origin_value(&self, candidate: &str) -> bool {
        parse_origin_tuple(candidate).is_ok_and(|(_, scheme, host, effective_port)| {
            scheme == self.scheme && host == self.host && effective_port == self.effective_port
        })
    }
}

/// Finds the exact allowed origin matching one request `Origin` value.
///
/// The candidate is parsed with the configuration grammar; malformed
/// candidates match nothing. An empty allowlist admits no origin.
#[must_use]
pub(crate) fn find_allowed_origin<'a>(
    allowed: &'a [AllowedOrigin],
    candidate: &str,
) -> Option<&'a AllowedOrigin> {
    let (_, scheme, host, effective_port) = parse_origin_tuple(candidate).ok()?;
    allowed.iter().find(|origin| {
        origin.scheme == scheme && origin.host == host && origin.effective_port == effective_port
    })
}

impl HttpConfig {
    fn from_lookup<F>(lookup: F) -> Result<Self, HttpConfigError>
    where
        F: Fn(&'static str) -> Result<Option<String>, HttpConfigError>,
    {
        let bind = parse_bind(lookup(BIND_ENV)?)?;
        let allowed_hosts = parse_allowed_hosts(lookup(ALLOWED_HOSTS_ENV)?)?;
        let allowed_origins = parse_allowed_origins(lookup(ALLOWED_ORIGINS_ENV)?)?;
        let auth = parse_auth(&lookup)?;
        let max_sessions = parse_bounded_u32(
            MAX_SESSIONS_ENV,
            lookup(MAX_SESSIONS_ENV)?,
            DEFAULT_MAX_SESSIONS,
            MAX_MAX_SESSIONS,
        )?;
        let requests_per_minute = parse_bounded_u32(
            REQUESTS_PER_MINUTE_ENV,
            lookup(REQUESTS_PER_MINUTE_ENV)?,
            DEFAULT_REQUESTS_PER_MINUTE,
            MAX_REQUESTS_PER_MINUTE,
        )?;
        let shutdown_secs = parse_bounded_u32(
            SHUTDOWN_SECS_ENV,
            lookup(SHUTDOWN_SECS_ENV)?,
            DEFAULT_SHUTDOWN_SECS,
            MAX_SHUTDOWN_SECS,
        )?;
        Ok(Self {
            bind,
            allowed_hosts,
            allowed_origins,
            auth,
            max_sessions,
            requests_per_minute,
            shutdown: Duration::from_secs(u64::from(shutdown_secs)),
        })
    }
}

fn parse_bind(value: Option<String>) -> Result<SocketAddr, HttpConfigError> {
    let value = value.unwrap_or_else(|| DEFAULT_BIND.to_owned());
    let address = value
        .parse::<SocketAddr>()
        .map_err(|_| HttpConfigError::invalid(BIND_ENV, "must be a socket address"))?;
    if !address.ip().is_loopback() {
        return Err(HttpConfigError::invalid(
            BIND_ENV,
            "must be an IPv4 or IPv6 loopback address",
        ));
    }
    Ok(address)
}

fn parse_list(variable: &'static str, value: &str) -> Result<Vec<String>, HttpConfigError> {
    let entries = value.split(',').map(str::to_owned).collect::<Vec<_>>();
    if entries.is_empty() || entries.len() > MAX_LIST_ENTRIES {
        return Err(HttpConfigError::invalid(
            variable,
            "must contain 1..16 comma-separated entries",
        ));
    }
    for (index, entry) in entries.iter().enumerate() {
        if entry.is_empty() || entry.len() > MAX_LIST_ENTRY_BYTES {
            return Err(HttpConfigError::invalid(
                variable,
                "entries must be 1..256 bytes with no empty items",
            ));
        }
        if !entry.bytes().all(|byte| byte.is_ascii_graphic()) {
            return Err(HttpConfigError::invalid(
                variable,
                "entries must be printable ASCII without whitespace",
            ));
        }
        if entry.contains('*') {
            return Err(HttpConfigError::invalid(
                variable,
                "wildcard entries are not supported",
            ));
        }
        if entries[..index].contains(entry) {
            return Err(HttpConfigError::invalid(
                variable,
                "entries must not repeat",
            ));
        }
    }
    Ok(entries)
}

fn parse_allowed_hosts(value: Option<String>) -> Result<Vec<HostAuthority>, HttpConfigError> {
    let Some(value) = value else {
        return Ok(vec![
            HostAuthority {
                host: "localhost".to_owned(),
                port: None,
            },
            HostAuthority {
                host: "127.0.0.1".to_owned(),
                port: None,
            },
            HostAuthority {
                host: "::1".to_owned(),
                port: None,
            },
        ]);
    };
    parse_list(ALLOWED_HOSTS_ENV, &value)?
        .into_iter()
        .map(|entry| parse_host_authority(&entry))
        .collect()
}

fn parse_host_authority(entry: &str) -> Result<HostAuthority, HttpConfigError> {
    let malformed =
        || HttpConfigError::invalid(ALLOWED_HOSTS_ENV, "entries must be exact authorities");
    let (host, port) = if let Some(rest) = entry.strip_prefix('[') {
        let (address, remainder) = rest.split_once(']').ok_or_else(malformed)?;
        if address.parse::<std::net::Ipv6Addr>().is_err() {
            return Err(malformed());
        }
        let port = match remainder.strip_prefix(':') {
            Some(port) => Some(parse_port(port).ok_or_else(malformed)?),
            None if remainder.is_empty() => None,
            None => return Err(malformed()),
        };
        (address.to_ascii_lowercase(), port)
    } else if let Some((host, port)) = entry.rsplit_once(':') {
        if host.contains(':') {
            return Err(malformed());
        }
        (
            validate_host_name(host).ok_or_else(malformed)?,
            Some(parse_port(port).ok_or_else(malformed)?),
        )
    } else {
        (validate_host_name(entry).ok_or_else(malformed)?, None)
    };
    Ok(HostAuthority { host, port })
}

fn validate_host_name(host: &str) -> Option<String> {
    if host.is_empty()
        || !host
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'.' || byte == b'-')
    {
        return None;
    }
    Some(host.to_ascii_lowercase())
}

fn parse_port(port: &str) -> Option<u16> {
    if port.is_empty() || !port.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    port.parse::<u16>().ok()
}

fn parse_allowed_origins(value: Option<String>) -> Result<Vec<AllowedOrigin>, HttpConfigError> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    parse_list(ALLOWED_ORIGINS_ENV, &value)?
        .into_iter()
        .map(|entry| {
            parse_origin_tuple(&entry)
                .map(|(serialized, scheme, host, effective_port)| AllowedOrigin {
                    serialized,
                    scheme,
                    host,
                    effective_port,
                })
                .map_err(|problem| HttpConfigError::invalid(ALLOWED_ORIGINS_ENV, problem))
        })
        .collect()
}

/// Parses one serialized origin into an exact tuple.
///
/// Returns the exact input serialization plus the normalized
/// `(scheme, host, effective port)` tuple, or a fixed problem category.
fn parse_origin_tuple(value: &str) -> Result<(String, OriginScheme, String, u16), &'static str> {
    const MALFORMED: &str = "entries must be serialized HTTP or HTTPS origins";
    if value == "null" {
        return Err("null origins are not supported");
    }
    let (scheme, remainder) = if let Some(remainder) = value.strip_prefix("https://") {
        (OriginScheme::Https, remainder)
    } else if let Some(remainder) = value.strip_prefix("http://") {
        (OriginScheme::Http, remainder)
    } else {
        return Err(MALFORMED);
    };
    if remainder.is_empty()
        || remainder.contains('/')
        || remainder.contains('?')
        || remainder.contains('#')
        || remainder.contains('@')
    {
        return Err("entries must not contain userinfo, path, query, or fragment");
    }
    let url = Url::parse(value).map_err(|_| MALFORMED)?;
    if !url.username().is_empty() || url.password().is_some() {
        return Err("entries must not contain userinfo, path, query, or fragment");
    }
    let host = url.host_str().ok_or(MALFORMED)?.to_ascii_lowercase();
    let host = host
        .strip_prefix('[')
        .and_then(|host| host.strip_suffix(']'))
        .map_or(host.clone(), str::to_owned);
    let effective_port = url.port().unwrap_or_else(|| scheme.default_port());
    Ok((value.to_owned(), scheme, host, effective_port))
}

fn parse_auth<F>(lookup: &F) -> Result<HttpAuthConfig, HttpConfigError>
where
    F: Fn(&'static str) -> Result<Option<String>, HttpConfigError>,
{
    let token_file = lookup(TOKEN_FILE_ENV)?;
    let resource_uri = lookup(RESOURCE_URI_ENV)?;
    let issuer = lookup(ISSUER_ENV)?;
    let authorization_server = lookup(AUTHORIZATION_SERVER_ENV)?;
    let jwks_uri = lookup(JWKS_URI_ENV)?;
    let audience = lookup(AUDIENCE_ENV)?;
    let required_scope = lookup(REQUIRED_SCOPE_ENV)?;

    match lookup(AUTH_ENV)?.as_deref() {
        Some("static-token") => {
            if resource_uri.is_some()
                || issuer.is_some()
                || authorization_server.is_some()
                || jwks_uri.is_some()
                || audience.is_some()
                || required_scope.is_some()
            {
                return Err(HttpConfigError::fixed(
                    "OAuth variables require ANY_MCP_HTTP_AUTH=oauth-resource-server",
                ));
            }
            let token_file = token_file.ok_or_else(|| {
                HttpConfigError::invalid(TOKEN_FILE_ENV, "is required for static-token")
            })?;
            let token_file = PathBuf::from(token_file);
            if !token_file.is_absolute() {
                return Err(HttpConfigError::invalid(
                    TOKEN_FILE_ENV,
                    "must be an absolute path",
                ));
            }
            Ok(HttpAuthConfig::StaticToken { token_file })
        }
        Some("oauth-resource-server") => {
            if token_file.is_some() {
                return Err(HttpConfigError::invalid(
                    TOKEN_FILE_ENV,
                    "is forbidden for oauth-resource-server",
                ));
            }
            Ok(HttpAuthConfig::OAuthResourceServer(Box::new(
                parse_oauth_config(
                    resource_uri,
                    issuer,
                    authorization_server,
                    jwks_uri,
                    audience,
                    required_scope,
                )?,
            )))
        }
        Some(_) => Err(HttpConfigError::invalid(
            AUTH_ENV,
            "must be exactly static-token or oauth-resource-server",
        )),
        None => Err(HttpConfigError::invalid(
            AUTH_ENV,
            "is required in streamable-http mode",
        )),
    }
}

fn parse_oauth_config(
    resource_uri: Option<String>,
    issuer: Option<String>,
    authorization_server: Option<String>,
    jwks_uri: Option<String>,
    audience: Option<String>,
    required_scope: Option<String>,
) -> Result<OAuthResourceConfig, HttpConfigError> {
    let resource_uri = required(RESOURCE_URI_ENV, resource_uri)?;
    let resource_url = parse_https_url(RESOURCE_URI_ENV, &resource_uri, false)?;
    if !resource_url.path().ends_with("/mcp") {
        return Err(HttpConfigError::invalid(
            RESOURCE_URI_ENV,
            "must end in /mcp",
        ));
    }
    if resource_url.query().is_some() {
        return Err(HttpConfigError::invalid(
            RESOURCE_URI_ENV,
            "must not contain a query",
        ));
    }

    let issuer = required(ISSUER_ENV, issuer)?;
    parse_https_url(ISSUER_ENV, &issuer, false)?;
    let authorization_server = required(AUTHORIZATION_SERVER_ENV, authorization_server)?;
    parse_https_url(AUTHORIZATION_SERVER_ENV, &authorization_server, false)?;
    if authorization_server != issuer {
        return Err(HttpConfigError::invalid(
            AUTHORIZATION_SERVER_ENV,
            "must equal ANY_MCP_HTTP_ISSUER in this version",
        ));
    }
    let jwks_uri_value = required(JWKS_URI_ENV, jwks_uri)?;
    let jwks_uri = parse_https_url(JWKS_URI_ENV, &jwks_uri_value, true)?;

    let audience = required(AUDIENCE_ENV, audience)?;
    let audience_chars = audience.chars().count();
    if audience_chars == 0 || audience_chars > MAX_AUDIENCE_CHARS {
        return Err(HttpConfigError::invalid(
            AUDIENCE_ENV,
            "must be 1..256 Unicode scalars",
        ));
    }

    let required_scope = required_scope.unwrap_or_else(|| DEFAULT_REQUIRED_SCOPE.to_owned());
    let scope_valid = !required_scope.is_empty()
        && required_scope.len() <= MAX_SCOPE_CHARS
        && required_scope
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._~-".contains(&byte));
    if !scope_valid {
        return Err(HttpConfigError::invalid(
            REQUIRED_SCOPE_ENV,
            "must match [A-Za-z0-9._~-]{1,64}",
        ));
    }

    Ok(OAuthResourceConfig {
        resource_uri,
        issuer,
        authorization_server,
        jwks_uri,
        audience,
        required_scope,
    })
}

fn required(variable: &'static str, value: Option<String>) -> Result<String, HttpConfigError> {
    value.ok_or_else(|| HttpConfigError::invalid(variable, "is required for oauth-resource-server"))
}

fn parse_https_url(
    variable: &'static str,
    value: &str,
    allow_query: bool,
) -> Result<Url, HttpConfigError> {
    let url = Url::parse(value)
        .map_err(|_| HttpConfigError::invalid(variable, "must be an exact HTTPS URI"))?;
    if url.scheme() != "https" || url.host_str().is_none() {
        return Err(HttpConfigError::invalid(
            variable,
            "must be an exact HTTPS URI",
        ));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(HttpConfigError::invalid(
            variable,
            "must not contain userinfo",
        ));
    }
    if url.fragment().is_some() {
        return Err(HttpConfigError::invalid(
            variable,
            "must not contain a fragment",
        ));
    }
    if !allow_query && url.query().is_some() {
        return Err(HttpConfigError::invalid(
            variable,
            "must not contain a query",
        ));
    }
    Ok(url)
}

fn parse_bounded_u32(
    variable: &'static str,
    value: Option<String>,
    default: u32,
    maximum: u32,
) -> Result<u32, HttpConfigError> {
    let Some(value) = value else {
        return Ok(default);
    };
    let parsed = value
        .parse::<u32>()
        .map_err(|_| HttpConfigError::invalid(variable, "must be a positive integer"))?;
    if parsed == 0 || parsed > maximum {
        return Err(HttpConfigError::invalid(
            variable,
            "is outside the supported range",
        ));
    }
    Ok(parsed)
}

/// A safe HTTP configuration diagnostic which never contains a value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HttpConfigError {
    variable: &'static str,
    problem: &'static str,
    fixed: Option<&'static str>,
}

impl HttpConfigError {
    fn invalid(variable: &'static str, problem: &'static str) -> Self {
        Self {
            variable,
            problem,
            fixed: None,
        }
    }

    fn fixed(message: &'static str) -> Self {
        Self {
            variable: "",
            problem: "",
            fixed: Some(message),
        }
    }
}

impl fmt::Display for HttpConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(fixed) = self.fixed {
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

impl std::error::Error for HttpConfigError {}

/// Returns the absolute static token file path for later validation.
#[must_use]
pub fn static_token_file(auth: &HttpAuthConfig) -> Option<&Path> {
    match auth {
        HttpAuthConfig::StaticToken { token_file } => Some(token_file),
        HttpAuthConfig::OAuthResourceServer(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn select(values: &[(&str, &str)]) -> Result<TransportSelection, HttpConfigError> {
        let values = values
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect::<HashMap<_, _>>();
        let present = values
            .keys()
            .filter(|key| key.starts_with(HTTP_VAR_PREFIX))
            .cloned()
            .collect::<Vec<_>>();
        TransportSelection::from_lookup(|name| Ok(values.get(name).cloned()), &present)
    }

    fn http_base<'a>() -> Vec<(&'a str, &'a str)> {
        vec![
            ("ANY_MCP_TRANSPORT", "streamable-http"),
            ("ANY_MCP_HTTP_AUTH", "static-token"),
            ("ANY_MCP_HTTP_TOKEN_FILE", "/etc/any-mcp/token"),
        ]
    }

    fn http_config(mut extra: Vec<(&str, &str)>) -> Result<HttpConfig, HttpConfigError> {
        let mut values = http_base();
        values.append(&mut extra);
        match select(&values)? {
            TransportSelection::StreamableHttp(config) => Ok(*config),
            TransportSelection::Stdio => panic!("expected streamable-http selection"),
        }
    }

    fn oauth_base<'a>() -> Vec<(&'a str, &'a str)> {
        vec![
            ("ANY_MCP_TRANSPORT", "streamable-http"),
            ("ANY_MCP_HTTP_AUTH", "oauth-resource-server"),
            ("ANY_MCP_HTTP_RESOURCE_URI", "https://mcp.example.com/mcp"),
            ("ANY_MCP_HTTP_ISSUER", "https://auth.example.com"),
            (
                "ANY_MCP_HTTP_AUTHORIZATION_SERVER",
                "https://auth.example.com",
            ),
            (
                "ANY_MCP_HTTP_JWKS_URI",
                "https://auth.example.com/.well-known/jwks.json",
            ),
            ("ANY_MCP_HTTP_AUDIENCE", "https://mcp.example.com/mcp"),
        ]
    }

    #[test]
    fn transport_defaults_to_stdio_and_is_exact() {
        assert_eq!(select(&[]).unwrap(), TransportSelection::Stdio);
        assert_eq!(
            select(&[("ANY_MCP_TRANSPORT", "stdio")]).unwrap(),
            TransportSelection::Stdio
        );
        for invalid in ["", "http", "STREAMABLE-HTTP", " streamable-http", "sse"] {
            let error = select(&[("ANY_MCP_TRANSPORT", invalid)]).unwrap_err();
            assert!(error.to_string().contains("ANY_MCP_TRANSPORT"));
            assert!(!error.to_string().contains("sse"));
        }
    }

    #[test]
    fn stdio_mode_rejects_every_http_variable() {
        for (name, value) in [
            ("ANY_MCP_HTTP_AUTH", "static-token"),
            ("ANY_MCP_HTTP_BIND", "127.0.0.1:9000"),
            ("ANY_MCP_HTTP_MAX_SESSIONS", "4"),
        ] {
            let error = select(&[(name, value)]).unwrap_err();
            assert_eq!(
                error.to_string(),
                "ANY_MCP_HTTP_ variables require ANY_MCP_TRANSPORT=streamable-http"
            );
        }
    }

    #[test]
    fn unknown_http_variables_are_rejected_in_every_mode() {
        for transport in [None, Some("stdio"), Some("streamable-http")] {
            let mut values = vec![("ANY_MCP_HTTP_TLS", "1")];
            if let Some(transport) = transport {
                values.push(("ANY_MCP_TRANSPORT", transport));
            }
            let error = select(&values).unwrap_err();
            assert_eq!(
                error.to_string(),
                "unknown ANY_MCP_HTTP_ configuration variable"
            );
        }
    }

    #[test]
    fn http_mode_requires_complete_authentication() {
        let error = select(&[("ANY_MCP_TRANSPORT", "streamable-http")]).unwrap_err();
        assert!(error.to_string().contains("ANY_MCP_HTTP_AUTH"));

        let error = select(&[
            ("ANY_MCP_TRANSPORT", "streamable-http"),
            ("ANY_MCP_HTTP_AUTH", "static-token"),
        ])
        .unwrap_err();
        assert!(error.to_string().contains("ANY_MCP_HTTP_TOKEN_FILE"));

        for invalid in ["", "token", "oauth", "Static-Token"] {
            let error = select(&[
                ("ANY_MCP_TRANSPORT", "streamable-http"),
                ("ANY_MCP_HTTP_AUTH", invalid),
            ])
            .unwrap_err();
            assert!(error.to_string().contains("ANY_MCP_HTTP_AUTH"));
        }
    }

    #[test]
    fn bind_defaults_to_loopback_and_rejects_public_addresses() {
        let config = http_config(vec![]).unwrap();
        assert_eq!(config.bind, "127.0.0.1:8000".parse().unwrap());

        for valid in ["127.0.0.1:9000", "127.5.4.3:8123", "[::1]:8000"] {
            let config = http_config(vec![("ANY_MCP_HTTP_BIND", valid)]).unwrap();
            assert!(config.bind.ip().is_loopback());
        }
        for invalid in [
            "0.0.0.0:8000",
            "192.168.1.4:8000",
            "[::]:8000",
            "127.0.0.1",
            "localhost:8000",
            "http://127.0.0.1:8000",
            "",
        ] {
            let error = http_config(vec![("ANY_MCP_HTTP_BIND", invalid)]).unwrap_err();
            assert!(error.to_string().contains("ANY_MCP_HTTP_BIND"));
            assert!(!error.to_string().contains("192.168.1.4"));
        }
    }

    #[test]
    fn allowed_hosts_default_to_local_authorities() {
        let config = http_config(vec![]).unwrap();
        assert_eq!(config.allowed_hosts.len(), 3);
        assert!(
            config
                .allowed_hosts
                .iter()
                .any(|host| host.matches("localhost", None))
        );
        assert!(
            config
                .allowed_hosts
                .iter()
                .any(|host| host.matches("localhost", Some(8000)))
        );
        assert!(
            config
                .allowed_hosts
                .iter()
                .any(|host| host.matches("127.0.0.1", Some(9999)))
        );
        assert!(
            config
                .allowed_hosts
                .iter()
                .any(|host| host.matches("::1", None))
        );
        assert!(
            !config
                .allowed_hosts
                .iter()
                .any(|host| host.matches("evil.test", None))
        );
    }

    #[test]
    fn allowed_hosts_accept_exact_authorities_and_ports() {
        let config = http_config(vec![(
            "ANY_MCP_HTTP_ALLOWED_HOSTS",
            "mcp.example.com:443,localhost,[::1]:8000",
        )])
        .unwrap();
        assert!(
            config
                .allowed_hosts
                .iter()
                .any(|host| host.matches("mcp.example.com", Some(443)))
        );
        assert!(
            !config
                .allowed_hosts
                .iter()
                .any(|host| host.matches("mcp.example.com", Some(444)))
        );
        assert!(
            !config
                .allowed_hosts
                .iter()
                .any(|host| host.matches("mcp.example.com", None))
        );
        assert!(
            config
                .allowed_hosts
                .iter()
                .any(|host| host.matches("::1", Some(8000)))
        );
    }

    #[test]
    fn allowed_hosts_reject_malformed_lists() {
        for invalid in [
            "",
            ",localhost",
            "localhost,",
            "localhost,localhost",
            "local host",
            "*.example.com",
            "user@example.com",
            "example.com:0x50",
            "[::1",
            "a:b:c",
            "héllo.example",
        ] {
            let error = http_config(vec![("ANY_MCP_HTTP_ALLOWED_HOSTS", invalid)]).unwrap_err();
            assert!(error.to_string().contains("ANY_MCP_HTTP_ALLOWED_HOSTS"));
        }
        let seventeen = (0..17)
            .map(|i| format!("h{i}.test"))
            .collect::<Vec<_>>()
            .join(",");
        let error = http_config(vec![("ANY_MCP_HTTP_ALLOWED_HOSTS", &seventeen)]).unwrap_err();
        assert!(error.to_string().contains("1..16"));
    }

    #[test]
    fn origins_are_absent_by_default_and_exact_when_configured() {
        assert!(http_config(vec![]).unwrap().allowed_origins.is_empty());

        let config = http_config(vec![(
            "ANY_MCP_HTTP_ALLOWED_ORIGINS",
            "https://app.example.com,http://localhost:5173",
        )])
        .unwrap();
        assert_eq!(config.allowed_origins.len(), 2);
        let first = &config.allowed_origins[0];
        assert_eq!(first.serialized(), "https://app.example.com");
        assert!(first.matches_origin_value("https://app.example.com"));
        assert!(first.matches_origin_value("https://app.example.com:443"));
        assert!(!first.matches_origin_value("http://app.example.com"));
        assert!(!first.matches_origin_value("https://app.example.com.evil.test"));
        assert!(!first.matches_origin_value("https://evil.test"));
        assert!(!first.matches_origin_value("null"));
        let second = &config.allowed_origins[1];
        assert!(second.matches_origin_value("http://localhost:5173"));
        assert!(!second.matches_origin_value("http://localhost"));
    }

    #[test]
    fn origins_reject_null_userinfo_paths_queries_fragments_and_wildcards() {
        for invalid in [
            "null",
            "https://user@example.com",
            "https://example.com/",
            "https://example.com/app",
            "https://example.com?q=1",
            "https://example.com#frag",
            "ftp://example.com",
            "https://*.example.com",
            "example.com",
            "",
        ] {
            let error = http_config(vec![("ANY_MCP_HTTP_ALLOWED_ORIGINS", invalid)]).unwrap_err();
            assert!(error.to_string().contains("ANY_MCP_HTTP_ALLOWED_ORIGINS"));
            assert!(!error.to_string().contains("example.com"));
        }
    }

    #[test]
    fn static_token_requires_absolute_path_and_forbids_oauth_variables() {
        let error = select(&[
            ("ANY_MCP_TRANSPORT", "streamable-http"),
            ("ANY_MCP_HTTP_AUTH", "static-token"),
            ("ANY_MCP_HTTP_TOKEN_FILE", "relative/token"),
        ])
        .unwrap_err();
        assert!(error.to_string().contains("ANY_MCP_HTTP_TOKEN_FILE"));

        let error =
            http_config(vec![("ANY_MCP_HTTP_ISSUER", "https://auth.example.com")]).unwrap_err();
        assert_eq!(
            error.to_string(),
            "OAuth variables require ANY_MCP_HTTP_AUTH=oauth-resource-server"
        );
    }

    #[test]
    fn oauth_profile_requires_every_variable_and_exact_grammars() {
        let config = match select(&oauth_base()).unwrap() {
            TransportSelection::StreamableHttp(config) => *config,
            TransportSelection::Stdio => panic!("expected streamable-http"),
        };
        let HttpAuthConfig::OAuthResourceServer(oauth) = config.auth else {
            panic!("expected oauth profile");
        };
        assert_eq!(oauth.resource_uri, "https://mcp.example.com/mcp");
        assert_eq!(oauth.issuer, "https://auth.example.com");
        assert_eq!(oauth.required_scope, "anytype.mcp");
        assert_eq!(
            oauth.jwks_uri.as_str(),
            "https://auth.example.com/.well-known/jwks.json"
        );

        for missing in [
            "ANY_MCP_HTTP_RESOURCE_URI",
            "ANY_MCP_HTTP_ISSUER",
            "ANY_MCP_HTTP_AUTHORIZATION_SERVER",
            "ANY_MCP_HTTP_JWKS_URI",
            "ANY_MCP_HTTP_AUDIENCE",
        ] {
            let values = oauth_base()
                .into_iter()
                .filter(|(name, _)| *name != missing)
                .collect::<Vec<_>>();
            let error = select(&values).unwrap_err();
            assert!(error.to_string().contains(missing), "{missing}");
        }
    }

    #[test]
    fn oauth_grammars_reject_mismatched_and_malformed_values() {
        let with = |name: &'static str, value: &'static str| {
            let mut values = oauth_base()
                .into_iter()
                .filter(|(existing, _)| *existing != name)
                .collect::<Vec<_>>();
            values.push((name, value));
            select(&values).unwrap_err().to_string()
        };

        assert!(
            with("ANY_MCP_HTTP_RESOURCE_URI", "https://mcp.example.com/api")
                .contains("ANY_MCP_HTTP_RESOURCE_URI")
        );
        assert!(
            with("ANY_MCP_HTTP_RESOURCE_URI", "http://mcp.example.com/mcp")
                .contains("ANY_MCP_HTTP_RESOURCE_URI")
        );
        assert!(
            with(
                "ANY_MCP_HTTP_RESOURCE_URI",
                "https://user@mcp.example.com/mcp"
            )
            .contains("ANY_MCP_HTTP_RESOURCE_URI")
        );
        assert!(
            with("ANY_MCP_HTTP_ISSUER", "http://auth.example.com").contains("ANY_MCP_HTTP_ISSUER")
        );
        assert!(
            with(
                "ANY_MCP_HTTP_AUTHORIZATION_SERVER",
                "https://other.example.com"
            )
            .contains("ANY_MCP_HTTP_AUTHORIZATION_SERVER")
        );
        assert!(
            with(
                "ANY_MCP_HTTP_JWKS_URI",
                "https://auth.example.com/jwks#frag"
            )
            .contains("ANY_MCP_HTTP_JWKS_URI")
        );
        assert!(with("ANY_MCP_HTTP_AUDIENCE", "").contains("ANY_MCP_HTTP_AUDIENCE"));
        assert!(
            with("ANY_MCP_HTTP_REQUIRED_SCOPE", "bad scope")
                .contains("ANY_MCP_HTTP_REQUIRED_SCOPE")
        );
        assert!(with("ANY_MCP_HTTP_REQUIRED_SCOPE", "").contains("ANY_MCP_HTTP_REQUIRED_SCOPE"));

        let mut values = oauth_base();
        values.push(("ANY_MCP_HTTP_TOKEN_FILE", "/etc/any-mcp/token"));
        let error = select(&values).unwrap_err();
        assert!(error.to_string().contains("ANY_MCP_HTTP_TOKEN_FILE"));
    }

    #[test]
    fn bounds_use_reviewed_defaults_and_ranges() {
        let config = http_config(vec![]).unwrap();
        assert_eq!(config.max_sessions, 32);
        assert_eq!(config.requests_per_minute, 120);
        assert_eq!(config.shutdown, Duration::from_secs(10));

        let config = http_config(vec![
            ("ANY_MCP_HTTP_MAX_SESSIONS", "256"),
            ("ANY_MCP_HTTP_REQUESTS_PER_MINUTE", "600"),
            ("ANY_MCP_HTTP_SHUTDOWN_SECS", "30"),
        ])
        .unwrap();
        assert_eq!(config.max_sessions, 256);
        assert_eq!(config.requests_per_minute, 600);
        assert_eq!(config.shutdown, Duration::from_secs(30));

        for (name, value) in [
            ("ANY_MCP_HTTP_MAX_SESSIONS", "0"),
            ("ANY_MCP_HTTP_MAX_SESSIONS", "257"),
            ("ANY_MCP_HTTP_REQUESTS_PER_MINUTE", "601"),
            ("ANY_MCP_HTTP_SHUTDOWN_SECS", "0"),
            ("ANY_MCP_HTTP_SHUTDOWN_SECS", "31"),
            ("ANY_MCP_HTTP_SHUTDOWN_SECS", "ten"),
        ] {
            let error = http_config(vec![(name, value)]).unwrap_err();
            assert!(error.to_string().contains(name));
        }
    }

    #[test]
    fn errors_never_echo_configured_values() {
        let secret_like = "token@do:not/echo";
        for (name, value) in [
            ("ANY_MCP_HTTP_BIND", secret_like),
            ("ANY_MCP_HTTP_ALLOWED_HOSTS", secret_like),
            ("ANY_MCP_HTTP_ALLOWED_ORIGINS", secret_like),
            ("ANY_MCP_HTTP_MAX_SESSIONS", secret_like),
        ] {
            let error = http_config(vec![(name, value)]).unwrap_err();
            assert!(!error.to_string().contains(secret_like));
        }
    }
}
