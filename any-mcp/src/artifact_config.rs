// any-mcp - bounded, workflow-oriented MCP server for Anytype
//
// SPDX-FileCopyrightText: 2026 Steve Schoettler
// SPDX-License-Identifier: Apache-2.0

//! Strict startup configuration for artifact and space policy.
//!
//! A configuration file is selected by explicit `-c`/`--config`,
//! `ANY_MCP_CONFIG`, or an existing `any-mcp.toml` in the current directory.
//! Physical paths remain native [`OsString`] values after validation and are
//! deliberately omitted from diagnostics.

use std::{
    collections::BTreeSet,
    ffi::{OsStr, OsString},
    fmt,
    io::Read,
    net::SocketAddr,
    path::{Component, Path, PathBuf},
    time::Duration,
};

#[cfg(any(unix, windows))]
use std::fs::File;

#[cfg(windows)]
use cap_fs_ext::{FollowSymlinks, OpenOptionsFollowExt, OpenOptionsMaybeDirExt};
#[cfg(windows)]
use cap_std::fs::{Dir, OpenOptions};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use icu_properties::{CodePointSetData, props::DefaultIgnorableCodePoint};
use serde::Deserialize;
use unicode_general_category::{GeneralCategory, get_general_category};
use unicode_normalization::UnicodeNormalization;
use url::Url;

use crate::domain::SpaceId;

/// Environment variable which explicitly selects the artifact policy file.
pub const CONFIG_ENV: &str = "ANY_MCP_CONFIG";

const CONFIG_BYTES: u64 = 262_144;
const DEFAULT_CONFIG_FILE: &str = "any-mcp.toml";
const NATIVE_ENCODED_BYTES: usize = 5_462;
const NATIVE_PATH_UNITS: usize = 4_096;
#[cfg(windows)]
const WINDOWS_PATH_UNITS: usize = 2_048;
const PATH_COMPONENTS: usize = 64;
const COMPONENT_UNITS: usize = 255;
const ROOT_ID_SCALARS: usize = 64;
const ROOT_ID_BYTES: usize = 128;
const SPACE_ENTRIES: usize = 256;
const VALIDATOR_ENTRIES: usize = 16;

/// Explicit startup configuration selection.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ConfigSelector {
    path: Option<PathBuf>,
}

impl ConfigSelector {
    /// Parses command-line arguments and the optional environment selector.
    ///
    /// The command line accepts only `-c ABSOLUTE_PATH` or
    /// `--config ABSOLUTE_PATH`. When either flag is present, `environment` is
    /// ignored without validation.
    ///
    /// # Errors
    ///
    /// Returns a fixed diagnostic for a duplicate/missing flag value, unknown
    /// argument, non-Unicode selector, or non-absolute path.
    pub fn from_args_and_env<I>(
        arguments: I,
        environment: Option<OsString>,
    ) -> Result<Self, ArtifactConfigError>
    where
        I: IntoIterator<Item = OsString>,
    {
        Self::from_args_and_env_lookup(arguments, || environment)
    }

    /// Parses arguments and evaluates `environment` only when no command-line
    /// selector is present.
    ///
    /// # Errors
    ///
    /// Returns the same fixed selector diagnostics as
    /// [`Self::from_args_and_env`].
    pub fn from_args_and_env_lookup<I, F>(
        arguments: I,
        environment: F,
    ) -> Result<Self, ArtifactConfigError>
    where
        I: IntoIterator<Item = OsString>,
        F: FnOnce() -> Option<OsString>,
    {
        let mut arguments = arguments.into_iter();
        let mut command_line = None;
        while let Some(argument) = arguments.next() {
            if argument != OsStr::new("--config") && argument != OsStr::new("-c") {
                return Err(ArtifactConfigError::new(ConfigProblem::Arguments));
            }
            if command_line.is_some() {
                return Err(ArtifactConfigError::new(ConfigProblem::Arguments));
            }
            let Some(value) = arguments.next() else {
                return Err(ArtifactConfigError::new(ConfigProblem::Arguments));
            };
            command_line = Some(value);
        }

        let selected = if let Some(value) = command_line {
            Some(value)
        } else if let Some(value) = environment() {
            Some(value)
        } else {
            std::env::current_dir()
                .ok()
                .map(|directory| directory.join(DEFAULT_CONFIG_FILE))
                .filter(|path| path.is_file())
                .map(|path| path.into_os_string())
        };
        let Some(selected) = selected else {
            return Ok(Self::default());
        };
        let Some(selected) = selected.to_str() else {
            return Err(ArtifactConfigError::new(ConfigProblem::Selector));
        };
        if selected.is_empty() {
            return Err(ArtifactConfigError::new(ConfigProblem::Selector));
        }
        let path = PathBuf::from(selected);
        if !path.is_absolute() {
            return Err(ArtifactConfigError::new(ConfigProblem::Selector));
        }
        Ok(Self { path: Some(path) })
    }

    /// Returns whether a configuration file was explicitly selected.
    #[must_use]
    pub const fn is_selected(&self) -> bool {
        self.path.is_some()
    }

    /// Loads and validates the selected file, or returns safe defaults.
    ///
    /// # Errors
    ///
    /// Returns a fixed diagnostic when the selected file cannot be securely
    /// opened and read or when its TOML policy is invalid.
    pub fn load(&self) -> Result<ArtifactConfig, ArtifactConfigError> {
        let Some(path) = &self.path else {
            return Ok(ArtifactConfig::default());
        };
        ArtifactConfig::load_file(path)
    }
}

/// Validated, immutable artifact policy for one process.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct ArtifactConfig {
    selected: bool,
    /// Space policy declared by the operator.
    pub spaces: SpaceConfig,
    /// Bounded artifact and staging limits.
    pub limits: ArtifactLimits,
    roots: RootDefinitions,
    /// Optional staging listener policy.
    pub staging: Option<StagingConfig>,
    /// Admitted validator declarations. Executables are not opened here.
    pub validators: Vec<ValidatorConfig>,
    auth: AuthConfig,
}

impl fmt::Debug for ArtifactConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ArtifactConfig")
            .field("selected", &self.selected)
            .field("spaces", &self.spaces)
            .field("limits", &self.limits)
            .field("import_root_count", &self.roots.import.len())
            .field("export_root_count", &self.roots.export.len())
            .field("staging_configured", &self.staging.is_some())
            .field("validator_count", &self.validators.len())
            .field("auth_keystore_configured", &self.auth.keystore.is_some())
            .finish()
    }
}

impl ArtifactConfig {
    /// Securely opens and validates one explicitly selected configuration.
    ///
    /// This internal entry point is shared by normal startup and the
    /// configuration-check command. The caller must supply an absolute path.
    pub(crate) fn load_file(path: &Path) -> Result<Self, ArtifactConfigError> {
        if !path.is_absolute() {
            return Err(ArtifactConfigError::new(ConfigProblem::Selector));
        }
        let contents = read_selected_file(path)?;
        Self::from_toml(&contents)
    }

    /// Parses one already-bounded UTF-8 TOML document.
    ///
    /// This method does not open configured roots, bind a listener, inspect a
    /// validator, or contact Anytype.
    ///
    /// # Errors
    ///
    /// Returns a redacted diagnostic for malformed/unknown fields, unsupported
    /// versions, unsafe access declarations, invalid names or paths, or limits
    /// outside immutable ceilings. TOML syntax and schema errors include a
    /// safe location, schema path when available, and problem category without
    /// echoing configuration values.
    pub fn from_toml(contents: &str) -> Result<Self, ArtifactConfigError> {
        let deserializer = toml::Deserializer::parse(contents)
            .map_err(|error| ArtifactConfigError::toml_source(contents, &error, None))?;
        let raw = serde_path_to_error::deserialize::<_, RawConfig>(deserializer)
            .map_err(|error| ArtifactConfigError::toml(contents, &error))?;
        if raw.schema_version != 1 {
            return Err(ArtifactConfigError::new(ConfigProblem::Version));
        }
        if raw.spaces.read_only {
            return Err(ArtifactConfigError::new(ConfigProblem::SpaceAccess));
        }

        let spaces = SpaceConfig::try_from(raw.spaces)?;
        let limits = ArtifactLimits::try_from(raw.limits.unwrap_or_default())?;
        let roots = RootDefinitions::try_from(raw.roots.unwrap_or_default())?;
        let staging = parse_staging(raw.staging)?;
        let validators = parse_validators(raw.validators)?;
        let auth = AuthConfig::try_from(raw.auth)?;

        validate_cross_fields(&limits, staging.as_ref(), &validators)?;
        Ok(Self {
            selected: true,
            spaces,
            limits,
            roots,
            staging,
            validators,
            auth,
        })
    }

    /// Returns whether the process loaded an explicitly selected file.
    #[must_use]
    pub const fn is_selected(&self) -> bool {
        self.selected
    }

    /// Returns the number of configured import/read roots.
    #[must_use]
    pub fn import_root_count(&self) -> usize {
        self.roots.import.len()
    }

    /// Returns the number of configured export/create roots.
    #[must_use]
    pub fn export_root_count(&self) -> usize {
        self.roots.export.len()
    }

    pub(crate) fn import_roots(&self) -> &[RootDefinition] {
        &self.roots.import
    }

    pub(crate) fn export_roots(&self) -> &[RootDefinition] {
        &self.roots.export
    }

    /// Returns the validated staging declaration, when configured.
    #[must_use]
    pub const fn staging(&self) -> Option<&StagingConfig> {
        self.staging.as_ref()
    }

    /// Returns the configured allowlisted validators.
    #[must_use]
    pub fn validators(&self) -> &[ValidatorConfig] {
        &self.validators
    }

    pub(crate) fn keystore_spec(&self) -> Option<String> {
        self.auth.resolved_keystore_spec()
    }
}

#[derive(Clone, Default, PartialEq, Eq)]
struct AuthConfig {
    keystore: Option<KeystoreConfig>,
}

impl AuthConfig {
    fn resolved_keystore_spec(&self) -> Option<String> {
        match self.keystore.as_ref() {
            Some(KeystoreConfig::File(path)) => Some(format!("file:path={path}")),
            Some(KeystoreConfig::SecretService) => Some("secret-service".to_owned()),
            None => None,
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
enum KeystoreConfig {
    File(String),
    SecretService,
}

/// Startup space access declaration.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SpaceConfig {
    /// MVP writable declaration. Selected files require this to be `false`.
    pub read_only: bool,
    /// Omitted means all spaces; an explicit empty vector means no spaces.
    pub allowed: Option<Vec<SpaceReference>>,
}

/// One exact configured space reference.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum SpaceReference {
    /// Canonical Anytype space identifier.
    Id(String),
    /// Exact Anytype space name, resolved once during startup.
    Name(String),
}

/// Immutable numeric ceilings admitted by artifact workflows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArtifactLimits {
    /// Maximum bytes in one artifact.
    pub artifact_bytes: u64,
    /// Maximum bytes in one transfer buffer.
    pub transfer_chunk_bytes: u64,
    /// Aggregate staged byte quota.
    pub staging_total_bytes: u64,
    /// Aggregate staged record quota.
    pub staging_entries: usize,
    /// Time-to-live for staged records.
    pub staging_ttl: Duration,
    /// Maximum simultaneous staging connections.
    pub staging_connections: usize,
    /// Maximum simultaneous staging requests.
    pub staging_requests: usize,
    /// Maximum staging requests admitted per minute.
    pub staging_requests_per_minute: u32,
    /// Maximum aggregate request header bytes.
    pub staging_header_bytes: usize,
    /// Header read deadline.
    pub staging_header_timeout: Duration,
    /// Streaming no-progress deadline.
    pub staging_no_progress_timeout: Duration,
    /// Maximum serialized receipt bytes.
    pub receipt_bytes: usize,
    /// Artifact operation deadline.
    pub operation_timeout: Duration,
    /// Maximum expired records cleaned in one pass.
    pub cleanup_batch: usize,
    /// Maximum upstream discovery rows scanned per operation.
    pub discovery_rows: usize,
    /// Maximum Markdown input bytes.
    pub markdown_bytes: u64,
    /// Maximum Markdown scalar values.
    pub markdown_chars: usize,
    /// Maximum simultaneous validator processes.
    pub validator_processes: usize,
    /// Aggregate validator input byte quota.
    pub validator_total_input_bytes: u64,
}

impl Default for ArtifactLimits {
    fn default() -> Self {
        Self {
            artifact_bytes: 256 * 1024 * 1024,
            transfer_chunk_bytes: 8 * 1024 * 1024,
            staging_total_bytes: 1024 * 1024 * 1024,
            staging_entries: 256,
            staging_ttl: Duration::from_secs(900),
            staging_connections: 64,
            staging_requests: 64,
            staging_requests_per_minute: 600,
            staging_header_bytes: 16 * 1024,
            staging_header_timeout: Duration::from_secs(5),
            staging_no_progress_timeout: Duration::from_secs(30),
            receipt_bytes: 16 * 1024,
            operation_timeout: Duration::from_secs(300),
            cleanup_batch: 64,
            discovery_rows: 1_000,
            markdown_bytes: 10 * 1024 * 1024,
            markdown_chars: 100_000,
            validator_processes: 4,
            validator_total_input_bytes: 512 * 1024 * 1024,
        }
    }
}

/// Validated staging listener declaration.
#[derive(Clone, PartialEq, Eq)]
pub struct StagingConfig {
    /// Whether staging is enabled when artifact authority is active.
    pub enabled: bool,
    root: AbsoluteNativePath,
    /// Loopback listener address.
    pub bind: SocketAddr,
    /// Externally usable HTTPS base URL, if enabled.
    pub public_base_url: Option<String>,
}

impl fmt::Debug for StagingConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StagingConfig")
            .field("enabled", &self.enabled)
            .field("bind", &self.bind)
            .field(
                "public_base_url_configured",
                &self.public_base_url.is_some(),
            )
            .finish()
    }
}

impl StagingConfig {
    pub(crate) fn root(&self) -> &AbsoluteNativePath {
        &self.root
    }
}

/// Validated declaration for one allowlisted validator executable.
#[derive(Clone, PartialEq, Eq)]
pub struct ValidatorConfig {
    /// Stable logical validator identifier.
    pub id: LogicalRootId,
    /// Closed validator driver name.
    pub driver: ValidatorDriver,
    path: PathBuf,
    /// Expected executable SHA-256 in lowercase hexadecimal.
    pub sha256: String,
    /// Whether rejection prevents artifact use.
    pub required: bool,
    /// MIME patterns admitted to this validator.
    pub mime: Vec<String>,
    /// Per-process timeout.
    pub timeout: Duration,
    /// Per-process virtual-memory ceiling.
    pub memory_bytes: u64,
    /// Per-process input byte ceiling.
    pub input_bytes: u64,
    /// Captured stdout byte ceiling.
    pub stdout_bytes: usize,
    /// Captured stderr byte ceiling.
    pub stderr_bytes: usize,
    /// Maximum parsed result fields.
    pub fields: usize,
    /// Maximum bytes in one parsed field.
    pub field_bytes: usize,
}

impl fmt::Debug for ValidatorConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ValidatorConfig")
            .field("id", &self.id)
            .field("driver", &self.driver)
            .field("required", &self.required)
            .field("mime_count", &self.mime.len())
            .finish_non_exhaustive()
    }
}

impl ValidatorConfig {
    /// Returns the executable path for Linux retained-descriptor activation.
    ///
    /// Other platforms retain the parsed configuration but intentionally do
    /// not activate validators until they have an equivalent execution
    /// boundary.
    #[cfg(target_os = "linux")]
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

/// Closed validator execution contract.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ValidatorDriver {
    /// The allowlisted executable reports a detected MIME essence.
    FileMime,
}

/// Canonical logical root or validator name.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LogicalRootId(String);

impl LogicalRootId {
    /// Validates, trims Pattern_White_Space, and NFC-normalizes a logical ID.
    ///
    /// # Errors
    ///
    /// Returns a fixed diagnostic for invisible or unsupported characters,
    /// unsafe mark placement, or a value outside the length ceilings.
    pub fn parse(value: &str) -> Result<Self, ArtifactConfigError> {
        let trimmed = value.trim_matches(is_pattern_whitespace);
        let normalized = trimmed.nfc().collect::<String>();
        let scalar_count = normalized.chars().count();
        if scalar_count == 0 || scalar_count > ROOT_ID_SCALARS || normalized.len() > ROOT_ID_BYTES {
            return Err(ArtifactConfigError::new(ConfigProblem::LogicalName));
        }

        let default_ignorables = CodePointSetData::new::<DefaultIgnorableCodePoint>();
        let mut has_base = false;
        let mut previous_separator = false;
        for (index, character) in normalized.chars().enumerate() {
            if default_ignorables.contains(character) {
                return Err(ArtifactConfigError::new(ConfigProblem::LogicalName));
            }
            let category = get_general_category(character);
            let is_letter = matches!(
                category,
                GeneralCategory::LowercaseLetter
                    | GeneralCategory::ModifierLetter
                    | GeneralCategory::OtherLetter
                    | GeneralCategory::TitlecaseLetter
                    | GeneralCategory::UppercaseLetter
            );
            let is_digit = category == GeneralCategory::DecimalNumber;
            let is_mark = matches!(
                category,
                GeneralCategory::EnclosingMark
                    | GeneralCategory::NonspacingMark
                    | GeneralCategory::SpacingMark
            );
            let is_separator = matches!(character, '-' | '_');
            if is_mark && (index == 0 || previous_separator) {
                return Err(ArtifactConfigError::new(ConfigProblem::LogicalName));
            }
            if !(is_letter || is_digit || is_mark || is_separator) {
                return Err(ArtifactConfigError::new(ConfigProblem::LogicalName));
            }
            has_base |= is_letter || is_digit;
            previous_separator = is_separator;
        }
        if !has_base {
            return Err(ArtifactConfigError::new(ConfigProblem::LogicalName));
        }
        Ok(Self(normalized))
    }

    /// Returns the canonical UTF-8 spelling.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for LogicalRootId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("LogicalRootId")
            .field(&self.0)
            .finish()
    }
}

impl fmt::Display for LogicalRootId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Validated absolute native filesystem path.
#[derive(Clone, PartialEq, Eq)]
pub struct AbsoluteNativePath(PathBuf);

impl AbsoluteNativePath {
    /// Validates an ordinary UTF-8 absolute configured path.
    ///
    /// # Errors
    ///
    /// Returns a fixed diagnostic for a relative, traversing, overlong, or
    /// otherwise unsupported native path.
    pub fn from_utf8(value: &str) -> Result<Self, ArtifactConfigError> {
        validate_absolute_native(OsStr::new(value)).map(Self)
    }

    /// Decodes and validates a platform-native base64url path.
    ///
    /// # Errors
    ///
    /// Returns a fixed diagnostic for a noncanonical encoding, wrong-platform
    /// encoding, or invalid native path.
    pub fn from_native(encoding: &str, value: &str) -> Result<Self, ArtifactConfigError> {
        let decoded = decode_native(encoding, value)?;
        validate_absolute_native(&decoded).map(Self)
    }

    /// Validates an already decoded platform-native absolute path.
    ///
    /// Used by decoders that produce native units directly, such as the MCP
    /// client-root `file:` URI decoder, which must not require UTF-8 on Unix.
    ///
    /// # Errors
    ///
    /// Returns a fixed diagnostic for a relative, traversing, overlong, or
    /// otherwise unsupported native path.
    pub(crate) fn from_os_str(value: &OsStr) -> Result<Self, ArtifactConfigError> {
        validate_absolute_native(value).map(Self)
    }

    pub(crate) fn as_path(&self) -> &Path {
        &self.0
    }
}

impl fmt::Debug for AbsoluteNativePath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("AbsoluteNativePath(<redacted>)")
    }
}

/// Validated relative native path used by a local artifact operation.
#[derive(Clone, PartialEq, Eq)]
pub struct RelativeNativePath(PathBuf);

impl RelativeNativePath {
    /// Parses the portable UTF-8 `/`-separated MCP representation.
    ///
    /// # Errors
    ///
    /// Returns a fixed diagnostic for traversal, absolute/prefixed syntax,
    /// empty/overlong components, URI/percent syntax, or controls.
    pub fn from_utf8(value: &str) -> Result<Self, ArtifactConfigError> {
        if value.is_empty()
            || value.len() > NATIVE_PATH_UNITS
            || value.contains('\\')
            || value.contains('%')
            || value.contains(':')
            || value.chars().any(char::is_control)
            || value.starts_with('/')
            || value.ends_with('/')
        {
            return Err(ArtifactConfigError::new(ConfigProblem::RelativePath));
        }
        let components = value.split('/').collect::<Vec<_>>();
        if components.is_empty() || components.len() > PATH_COMPONENTS {
            return Err(ArtifactConfigError::new(ConfigProblem::RelativePath));
        }
        if components.iter().any(|component| {
            component.is_empty()
                || component.len() > COMPONENT_UNITS
                || matches!(*component, "." | "..")
        }) {
            return Err(ArtifactConfigError::new(ConfigProblem::RelativePath));
        }
        validate_relative_native(OsStr::new(value)).map(Self)
    }

    /// Decodes and validates a platform-native base64url relative path.
    ///
    /// # Errors
    ///
    /// Returns a fixed diagnostic for a noncanonical encoding, wrong-platform
    /// encoding, or invalid relative native path.
    pub fn from_native(encoding: &str, value: &str) -> Result<Self, ArtifactConfigError> {
        let decoded = decode_native(encoding, value)?;
        validate_relative_native(&decoded).map(Self)
    }

    /// Returns the validated native path.
    #[must_use]
    pub fn as_path(&self) -> &Path {
        &self.0
    }
}

impl fmt::Debug for RelativeNativePath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("RelativeNativePath(<redacted>)")
    }
}

/// Path-safe artifact configuration error.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArtifactConfigError {
    problem: ConfigProblem,
    toml: Option<TomlDiagnostic>,
}

impl ArtifactConfigError {
    const fn new(problem: ConfigProblem) -> Self {
        Self {
            problem,
            toml: None,
        }
    }

    fn toml(contents: &str, error: &serde_path_to_error::Error<toml::de::Error>) -> Self {
        let source = error.inner();
        let path = safe_toml_schema_path(&error.path().to_string());
        Self::toml_source(contents, source, path)
    }

    fn toml_source(contents: &str, source: &toml::de::Error, path: Option<String>) -> Self {
        let location = source
            .span()
            .map(|span| toml_line_column(contents, span.start));
        Self {
            problem: ConfigProblem::Toml,
            toml: Some(TomlDiagnostic {
                location,
                path,
                problem: classify_toml_problem(source.message()),
            }),
        }
    }
}

impl fmt::Display for ArtifactConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self.problem {
            ConfigProblem::Arguments => "invalid any-mcp command-line arguments",
            ConfigProblem::Selector => "invalid any-mcp config selector",
            ConfigProblem::File => "selected any-mcp config file is not secure and readable",
            ConfigProblem::Toml => "invalid any-mcp TOML configuration",
            ConfigProblem::Version => "unsupported any-mcp configuration version",
            ConfigProblem::SpaceAccess => {
                "selected any-mcp configuration must declare spaces.read_only = false"
            }
            ConfigProblem::SpacePolicy => "invalid any-mcp space policy",
            ConfigProblem::Limit => "invalid any-mcp artifact limit",
            ConfigProblem::LogicalName => "invalid any-mcp logical name",
            ConfigProblem::Root => "invalid any-mcp artifact root",
            ConfigProblem::NativePath => "invalid any-mcp native path",
            ConfigProblem::RelativePath => "invalid any-mcp relative artifact path",
            ConfigProblem::Staging => "invalid any-mcp staging policy",
            ConfigProblem::Validator => "invalid any-mcp validator policy",
            ConfigProblem::Auth => "invalid any-mcp authentication policy",
        };
        formatter.write_str(message)?;
        if let Some(diagnostic) = &self.toml {
            if let Some((line, column)) = diagnostic.location {
                write!(formatter, " at line {line}, column {column}")?;
            }
            if let Some(path) = &diagnostic.path {
                write!(formatter, " in `{path}`")?;
            }
            write!(formatter, ": {}", diagnostic.problem.message())?;
        }
        Ok(())
    }
}

impl std::error::Error for ArtifactConfigError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ConfigProblem {
    Arguments,
    Selector,
    File,
    Toml,
    Version,
    SpaceAccess,
    SpacePolicy,
    Limit,
    LogicalName,
    Root,
    NativePath,
    RelativePath,
    Staging,
    Validator,
    Auth,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct TomlDiagnostic {
    location: Option<(usize, usize)>,
    path: Option<String>,
    problem: TomlProblem,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TomlProblem {
    SyntaxOrSchema,
    UnknownField,
    MissingField,
    DuplicateField,
    WrongType,
    UnsupportedValue,
    WrongLength,
    NumericRange,
}

impl TomlProblem {
    const fn message(self) -> &'static str {
        match self {
            Self::SyntaxOrSchema => "syntax or value does not match the configuration schema",
            Self::UnknownField => "field is not recognized",
            Self::MissingField => "required field is missing",
            Self::DuplicateField => "field is declared more than once",
            Self::WrongType => "value has the wrong type",
            Self::UnsupportedValue => "value is not supported",
            Self::WrongLength => "value has the wrong number of items",
            Self::NumericRange => "number is outside the supported range",
        }
    }
}

fn classify_toml_problem(message: &str) -> TomlProblem {
    if message.contains("unknown field") {
        TomlProblem::UnknownField
    } else if message.contains("missing field") {
        TomlProblem::MissingField
    } else if message.contains("duplicate field") {
        TomlProblem::DuplicateField
    } else if message.contains("invalid type") {
        TomlProblem::WrongType
    } else if message.contains("unknown variant") || message.contains("invalid value") {
        TomlProblem::UnsupportedValue
    } else if message.contains("invalid length") {
        TomlProblem::WrongLength
    } else if message.contains("too large") || message.contains("out of range") {
        TomlProblem::NumericRange
    } else {
        TomlProblem::SyntaxOrSchema
    }
}

fn toml_line_column(contents: &str, byte_offset: usize) -> (usize, usize) {
    let mut line = 1;
    let mut column = 1;
    for (offset, character) in contents.char_indices() {
        if offset >= byte_offset {
            break;
        }
        if character == '\n' {
            line += 1;
            column = 1;
        } else {
            column += 1;
        }
    }
    (line, column)
}

fn safe_toml_schema_path(path: &str) -> Option<String> {
    const FIELDS: &[&str] = &[
        "schema_version",
        "spaces",
        "read_only",
        "allowed",
        "id",
        "name",
        "limits",
        "artifact_bytes",
        "transfer_chunk_bytes",
        "staging_total_bytes",
        "staging_entries",
        "staging_ttl_secs",
        "staging_connections",
        "staging_requests",
        "staging_requests_per_minute",
        "staging_header_bytes",
        "staging_header_secs",
        "staging_no_progress_secs",
        "receipt_bytes",
        "operation_secs",
        "cleanup_batch",
        "discovery_rows",
        "markdown_bytes",
        "markdown_chars",
        "validator_processes",
        "validator_total_input_bytes",
        "roots",
        "import",
        "export",
        "path",
        "path_native",
        "encoding",
        "value",
        "staging",
        "enabled",
        "root",
        "bind",
        "public_base_url",
        "validators",
        "driver",
        "sha256",
        "required",
        "mime",
        "timeout_secs",
        "memory_bytes",
        "input_bytes",
        "stdout_bytes",
        "stderr_bytes",
        "fields",
        "field_bytes",
        "platform",
    ];

    if path.is_empty() || path == "." {
        return None;
    }
    for segment in path.split('.') {
        let name_end = segment.find('[').unwrap_or(segment.len());
        let name = &segment[..name_end];
        if !FIELDS.contains(&name) || !safe_toml_indices(&segment[name_end..]) {
            return None;
        }
    }
    Some(path.to_owned())
}

fn safe_toml_indices(mut suffix: &str) -> bool {
    while !suffix.is_empty() {
        let Some(index) = suffix.strip_prefix('[') else {
            return false;
        };
        let Some(end) = index.find(']') else {
            return false;
        };
        if end == 0 || !index[..end].bytes().all(|byte| byte.is_ascii_digit()) {
            return false;
        }
        suffix = &index[end + 1..];
    }
    true
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    schema_version: u32,
    spaces: RawSpaces,
    limits: Option<RawLimits>,
    roots: Option<RawRoots>,
    staging: Option<RawStaging>,
    auth: Option<RawAuth>,
    #[serde(default)]
    validators: Vec<RawValidator>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawAuth {
    keystore: Option<RawKeystore>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawKeystore {
    file: Option<String>,
    #[serde(rename = "secret-service")]
    secret_service: Option<bool>,
}

impl TryFrom<Option<RawAuth>> for AuthConfig {
    type Error = ArtifactConfigError;

    fn try_from(raw: Option<RawAuth>) -> Result<Self, Self::Error> {
        let Some(RawAuth {
            keystore: Some(keystore),
        }) = raw
        else {
            return Ok(Self::default());
        };
        let selected = match (keystore.file, keystore.secret_service) {
            (Some(path), None) if !path.is_empty() => KeystoreConfig::File(path),
            (None, Some(true)) => KeystoreConfig::SecretService,
            _ => return Err(ArtifactConfigError::new(ConfigProblem::Auth)),
        };
        Ok(Self {
            keystore: Some(selected),
        })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSpaces {
    read_only: bool,
    allowed: Option<Vec<RawSpaceReference>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSpaceReference {
    id: Option<String>,
    name: Option<String>,
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawLimits {
    artifact_bytes: Option<u64>,
    transfer_chunk_bytes: Option<u64>,
    staging_total_bytes: Option<u64>,
    staging_entries: Option<usize>,
    staging_ttl_secs: Option<u64>,
    staging_connections: Option<usize>,
    staging_requests: Option<usize>,
    staging_requests_per_minute: Option<u32>,
    staging_header_bytes: Option<usize>,
    staging_header_secs: Option<u64>,
    staging_no_progress_secs: Option<u64>,
    receipt_bytes: Option<usize>,
    operation_secs: Option<u64>,
    cleanup_batch: Option<usize>,
    discovery_rows: Option<usize>,
    markdown_bytes: Option<u64>,
    markdown_chars: Option<usize>,
    validator_processes: Option<usize>,
    validator_total_input_bytes: Option<u64>,
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRoots {
    #[serde(default)]
    import: Vec<RawRoot>,
    #[serde(default)]
    export: Vec<RawRoot>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRoot {
    id: String,
    path: Option<String>,
    path_native: Option<RawNativePath>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawNativePath {
    encoding: String,
    value: String,
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawStaging {
    #[serde(default)]
    enabled: bool,
    root: Option<String>,
    bind: Option<String>,
    public_base_url: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawValidator {
    id: String,
    driver: String,
    path: String,
    sha256: String,
    required: bool,
    mime: Vec<String>,
    timeout_secs: u64,
    memory_bytes: u64,
    input_bytes: u64,
    stdout_bytes: usize,
    stderr_bytes: usize,
    fields: usize,
    field_bytes: usize,
    platform: String,
}

#[derive(Clone, Default, PartialEq, Eq)]
struct RootDefinitions {
    import: Vec<RootDefinition>,
    export: Vec<RootDefinition>,
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct RootDefinition {
    pub(crate) id: LogicalRootId,
    pub(crate) path: AbsoluteNativePath,
}

impl TryFrom<RawSpaces> for SpaceConfig {
    type Error = ArtifactConfigError;

    fn try_from(raw: RawSpaces) -> Result<Self, Self::Error> {
        let allowed = raw
            .allowed
            .map(|entries| {
                if entries.len() > SPACE_ENTRIES {
                    return Err(ArtifactConfigError::new(ConfigProblem::SpacePolicy));
                }
                let mut seen = BTreeSet::new();
                entries
                    .into_iter()
                    .map(|entry| {
                        let reference = match (entry.id, entry.name) {
                            (Some(id), None) if valid_space_id(&id) => SpaceReference::Id(id),
                            (None, Some(name))
                                if !name.trim().is_empty() && name.chars().count() <= 512 =>
                            {
                                SpaceReference::Name(name)
                            }
                            _ => {
                                return Err(ArtifactConfigError::new(ConfigProblem::SpacePolicy));
                            }
                        };
                        if !seen.insert(reference.clone()) {
                            return Err(ArtifactConfigError::new(ConfigProblem::SpacePolicy));
                        }
                        Ok(reference)
                    })
                    .collect::<Result<Vec<_>, _>>()
            })
            .transpose()?;
        Ok(Self {
            read_only: raw.read_only,
            allowed,
        })
    }
}

impl TryFrom<RawLimits> for ArtifactLimits {
    type Error = ArtifactConfigError;

    fn try_from(raw: RawLimits) -> Result<Self, Self::Error> {
        let defaults = Self::default();
        let artifact_bytes = bounded(
            raw.artifact_bytes,
            defaults.artifact_bytes,
            64 * 1024,
            1 << 30,
        )?;

        // Limits that operate on a single artifact inherit its configured ceiling
        // when omitted. This lets an operator lower `artifact_bytes` without
        // having to repeat every dependent default, while explicitly configured
        // incompatible limits are still rejected by cross-field validation.
        let transfer_chunk_default = defaults.transfer_chunk_bytes.min(artifact_bytes);
        let markdown_default = defaults.markdown_bytes.min(artifact_bytes);
        let validator_total_input_default =
            defaults.validator_total_input_bytes.max(artifact_bytes);
        Ok(Self {
            artifact_bytes,
            transfer_chunk_bytes: bounded(
                raw.transfer_chunk_bytes,
                transfer_chunk_default,
                64 * 1024,
                64 * 1024 * 1024,
            )?,
            staging_total_bytes: bounded(
                raw.staging_total_bytes,
                defaults.staging_total_bytes,
                1024 * 1024,
                16_u64 * 1024 * 1024 * 1024,
            )?,
            staging_entries: bounded(raw.staging_entries, defaults.staging_entries, 1, 4_096)?,
            staging_ttl: Duration::from_secs(bounded(
                raw.staging_ttl_secs,
                defaults.staging_ttl.as_secs(),
                60,
                86_400,
            )?),
            staging_connections: bounded(
                raw.staging_connections,
                defaults.staging_connections,
                1,
                256,
            )?,
            staging_requests: bounded(raw.staging_requests, defaults.staging_requests, 1, 256)?,
            staging_requests_per_minute: bounded(
                raw.staging_requests_per_minute,
                defaults.staging_requests_per_minute,
                1,
                10_000,
            )?,
            staging_header_bytes: bounded(
                raw.staging_header_bytes,
                defaults.staging_header_bytes,
                4 * 1024,
                64 * 1024,
            )?,
            staging_header_timeout: Duration::from_secs(bounded(
                raw.staging_header_secs,
                defaults.staging_header_timeout.as_secs(),
                1,
                30,
            )?),
            staging_no_progress_timeout: Duration::from_secs(bounded(
                raw.staging_no_progress_secs,
                defaults.staging_no_progress_timeout.as_secs(),
                1,
                120,
            )?),
            receipt_bytes: bounded(
                raw.receipt_bytes,
                defaults.receipt_bytes,
                2 * 1024,
                64 * 1024,
            )?,
            operation_timeout: Duration::from_secs(bounded(
                raw.operation_secs,
                defaults.operation_timeout.as_secs(),
                1,
                900,
            )?),
            cleanup_batch: bounded(raw.cleanup_batch, defaults.cleanup_batch, 1, 1_024)?,
            discovery_rows: bounded(raw.discovery_rows, defaults.discovery_rows, 1, 10_000)?,
            markdown_bytes: bounded(raw.markdown_bytes, markdown_default, 1, 64 * 1024 * 1024)?,
            markdown_chars: bounded(raw.markdown_chars, defaults.markdown_chars, 1, 1_000_000)?,
            validator_processes: bounded(
                raw.validator_processes,
                defaults.validator_processes,
                1,
                16,
            )?,
            validator_total_input_bytes: bounded(
                raw.validator_total_input_bytes,
                validator_total_input_default,
                1,
                2_u64 * 1024 * 1024 * 1024,
            )?,
        })
    }
}

impl TryFrom<RawRoots> for RootDefinitions {
    type Error = ArtifactConfigError;

    fn try_from(raw: RawRoots) -> Result<Self, Self::Error> {
        if raw.import.len() > PATH_COMPONENTS || raw.export.len() > PATH_COMPONENTS {
            return Err(ArtifactConfigError::new(ConfigProblem::Root));
        }
        let mut identifiers = BTreeSet::new();
        let import = parse_roots(raw.import, &mut identifiers)?;
        let export = parse_roots(raw.export, &mut identifiers)?;
        Ok(Self { import, export })
    }
}

fn parse_staging(raw: Option<RawStaging>) -> Result<Option<StagingConfig>, ArtifactConfigError> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    if !raw.enabled {
        if raw.root.is_some() || raw.bind.is_some() || raw.public_base_url.is_some() {
            return Err(ArtifactConfigError::new(ConfigProblem::Staging));
        }
        return Ok(None);
    }
    let root = raw
        .root
        .as_deref()
        .ok_or_else(|| ArtifactConfigError::new(ConfigProblem::Staging))
        .and_then(AbsoluteNativePath::from_utf8)
        .map_err(|_| ArtifactConfigError::new(ConfigProblem::Staging))?;
    let bind = raw
        .bind
        .as_deref()
        .unwrap_or("127.0.0.1:8765")
        .parse::<SocketAddr>()
        .map_err(|_| ArtifactConfigError::new(ConfigProblem::Staging))?;
    if !bind.ip().is_loopback() || bind.port() < 1024 {
        return Err(ArtifactConfigError::new(ConfigProblem::Staging));
    }
    let public_base_url = raw
        .public_base_url
        .ok_or_else(|| ArtifactConfigError::new(ConfigProblem::Staging))?;
    if public_base_url.len() > 2_048 || !public_base_url.is_ascii() {
        return Err(ArtifactConfigError::new(ConfigProblem::Staging));
    }
    let parsed = Url::parse(&public_base_url)
        .map_err(|_| ArtifactConfigError::new(ConfigProblem::Staging))?;
    let common_invalid = parsed.cannot_be_a_base()
        || parsed.username() != ""
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
        || parsed.path() != "/artifacts/v1/";
    let scheme_valid = match parsed.scheme() {
        "https" => parsed.host_str().is_some(),
        "http" => {
            let host = parsed
                .host_str()
                .and_then(|host| host.parse::<std::net::IpAddr>().ok());
            host == Some(bind.ip()) && parsed.port_or_known_default() == Some(bind.port())
        }
        _ => false,
    };
    if common_invalid || !scheme_valid {
        return Err(ArtifactConfigError::new(ConfigProblem::Staging));
    }
    Ok(Some(StagingConfig {
        enabled: true,
        root,
        bind,
        public_base_url: Some(public_base_url),
    }))
}

fn parse_validators(raw: Vec<RawValidator>) -> Result<Vec<ValidatorConfig>, ArtifactConfigError> {
    if raw.len() > VALIDATOR_ENTRIES {
        return Err(ArtifactConfigError::new(ConfigProblem::Validator));
    }
    let mut identifiers = BTreeSet::new();
    raw.into_iter()
        .map(|validator| {
            let id = LogicalRootId::parse(&validator.id)
                .map_err(|_| ArtifactConfigError::new(ConfigProblem::Validator))?;
            if !identifiers.insert(id.clone())
                || validator.driver != "file-mime"
                || validator.sha256.len() != 64
                || !validator
                    .sha256
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
                || validator.mime.is_empty()
                || validator.mime.len() > 64
                || validator
                    .mime
                    .iter()
                    .any(|mime| !valid_validator_mime(mime))
                || validator.timeout_secs == 0
                || !(16 * 1024 * 1024..=2_u64 * 1024 * 1024 * 1024)
                    .contains(&validator.memory_bytes)
                || validator.input_bytes == 0
                || validator.stdout_bytes == 0
                || validator.stderr_bytes == 0
                || validator.fields == 0
                || validator.stdout_bytes > 1024 * 1024
                || validator.stderr_bytes > 1024 * 1024
                || validator.fields > 256
                || validator.field_bytes == 0
                || validator.field_bytes > 64 * 1024
                || validator.platform != "linux-retained-fd-v1"
            {
                return Err(ArtifactConfigError::new(ConfigProblem::Validator));
            }
            let path = AbsoluteNativePath::from_utf8(&validator.path)
                .map_err(|_| ArtifactConfigError::new(ConfigProblem::Validator))?;
            Ok(ValidatorConfig {
                id,
                driver: ValidatorDriver::FileMime,
                path: path.0,
                sha256: validator.sha256,
                required: validator.required,
                mime: validator.mime,
                timeout: Duration::from_secs(validator.timeout_secs),
                memory_bytes: validator.memory_bytes,
                input_bytes: validator.input_bytes,
                stdout_bytes: validator.stdout_bytes,
                stderr_bytes: validator.stderr_bytes,
                fields: validator.fields,
                field_bytes: validator.field_bytes,
            })
        })
        .collect()
}

fn valid_validator_mime(value: &str) -> bool {
    if value == "*/*" {
        return true;
    }
    if let Some(prefix) = value.strip_suffix("/*") {
        return !prefix.is_empty()
            && prefix.len() <= 127
            && prefix.bytes().all(|byte| {
                byte.is_ascii_alphanumeric()
                    || matches!(
                        byte,
                        b'!' | b'#' | b'$' | b'&' | b'^' | b'_' | b'.' | b'+' | b'-'
                    )
            });
    }
    if value.len() > 255 || value.contains(';') || !value.is_ascii() {
        return false;
    }
    value.parse::<mime::Mime>().ok().is_some_and(|parsed| {
        format!("{}/{}", parsed.type_(), parsed.subtype()) == value.to_ascii_lowercase()
    })
}

fn parse_roots(
    roots: Vec<RawRoot>,
    identifiers: &mut BTreeSet<String>,
) -> Result<Vec<RootDefinition>, ArtifactConfigError> {
    roots
        .into_iter()
        .map(|root| {
            let id = LogicalRootId::parse(&root.id)
                .map_err(|_| ArtifactConfigError::new(ConfigProblem::Root))?;
            if !identifiers.insert(id.as_str().to_ascii_lowercase()) {
                return Err(ArtifactConfigError::new(ConfigProblem::Root));
            }
            let path = match (root.path, root.path_native) {
                (Some(path), None) => AbsoluteNativePath::from_utf8(&path),
                (None, Some(path)) => AbsoluteNativePath::from_native(&path.encoding, &path.value),
                _ => Err(ArtifactConfigError::new(ConfigProblem::Root)),
            }
            .map_err(|_| ArtifactConfigError::new(ConfigProblem::Root))?;
            Ok(RootDefinition { id, path })
        })
        .collect()
}

fn validate_cross_fields(
    limits: &ArtifactLimits,
    staging: Option<&StagingConfig>,
    validators: &[ValidatorConfig],
) -> Result<(), ArtifactConfigError> {
    let invalid_limits = limits.transfer_chunk_bytes > limits.artifact_bytes
        || limits.markdown_bytes > limits.artifact_bytes
        || limits.artifact_bytes > limits.validator_total_input_bytes
        || limits.cleanup_batch > limits.staging_entries
        || limits.staging_header_timeout > limits.operation_timeout
        || limits.staging_no_progress_timeout > limits.operation_timeout
        || staging.is_some_and(|value| {
            value.enabled && limits.artifact_bytes > limits.staging_total_bytes
        })
        || validators.iter().any(|validator| {
            validator.input_bytes > limits.artifact_bytes
                || validator.timeout > limits.operation_timeout
        });
    if invalid_limits {
        return Err(ArtifactConfigError::new(ConfigProblem::Limit));
    }
    Ok(())
}

fn bounded<T>(
    configured: Option<T>,
    default: T,
    minimum: T,
    maximum: T,
) -> Result<T, ArtifactConfigError>
where
    T: Copy + Ord,
{
    let value = configured.unwrap_or(default);
    if value < minimum || value > maximum {
        return Err(ArtifactConfigError::new(ConfigProblem::Limit));
    }
    Ok(value)
}

fn valid_space_id(value: &str) -> bool {
    SpaceId::new(value.to_owned()).is_ok()
}

fn is_pattern_whitespace(character: char) -> bool {
    matches!(
        character,
        '\u{0009}'
            ..='\u{000D}'
                | '\u{0020}'
                | '\u{0085}'
                | '\u{200E}'
                | '\u{200F}'
                | '\u{2028}'
                | '\u{2029}'
    )
}

fn decode_native(encoding: &str, value: &str) -> Result<OsString, ArtifactConfigError> {
    if value.len() > NATIVE_ENCODED_BYTES || !value.is_ascii() || value.contains('=') {
        return Err(ArtifactConfigError::new(ConfigProblem::NativePath));
    }
    let decoded = URL_SAFE_NO_PAD
        .decode(value)
        .map_err(|_| ArtifactConfigError::new(ConfigProblem::NativePath))?;
    if URL_SAFE_NO_PAD.encode(&decoded) != value {
        return Err(ArtifactConfigError::new(ConfigProblem::NativePath));
    }

    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;

        if encoding != "unix-bytes-base64url" {
            return Err(ArtifactConfigError::new(ConfigProblem::NativePath));
        }
        Ok(OsString::from_vec(decoded))
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStringExt;

        if encoding != "windows-wtf16le-base64url" || decoded.len() % 2 != 0 {
            return Err(ArtifactConfigError::new(ConfigProblem::NativePath));
        }
        let units = decoded
            .chunks_exact(2)
            .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
            .collect::<Vec<_>>();
        Ok(OsString::from_wide(&units))
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (encoding, decoded);
        Err(ArtifactConfigError::new(ConfigProblem::NativePath))
    }
}

#[cfg(unix)]
fn validate_absolute_native(value: &OsStr) -> Result<PathBuf, ArtifactConfigError> {
    use std::os::unix::ffi::OsStrExt;

    let bytes = value.as_bytes();
    if bytes.is_empty()
        || bytes.len() > NATIVE_PATH_UNITS
        || bytes[0] != b'/'
        || bytes.iter().any(u8::is_ascii_control)
    {
        return Err(ArtifactConfigError::new(ConfigProblem::NativePath));
    }
    validate_unix_components(&bytes[1..], true)?;
    let path = PathBuf::from(value);
    if !path.is_absolute() {
        return Err(ArtifactConfigError::new(ConfigProblem::NativePath));
    }
    Ok(path)
}

#[cfg(unix)]
fn validate_relative_native(value: &OsStr) -> Result<PathBuf, ArtifactConfigError> {
    use std::os::unix::ffi::OsStrExt;

    let bytes = value.as_bytes();
    if bytes.is_empty()
        || bytes.len() > NATIVE_PATH_UNITS
        || bytes[0] == b'/'
        || bytes.iter().any(u8::is_ascii_control)
    {
        return Err(ArtifactConfigError::new(ConfigProblem::RelativePath));
    }
    validate_unix_components(bytes, false)
        .map_err(|_| ArtifactConfigError::new(ConfigProblem::RelativePath))?;
    let path = PathBuf::from(value);
    if path.is_absolute() {
        return Err(ArtifactConfigError::new(ConfigProblem::RelativePath));
    }
    Ok(path)
}

#[cfg(unix)]
fn validate_unix_components(bytes: &[u8], allow_root: bool) -> Result<(), ArtifactConfigError> {
    if bytes.is_empty() {
        return if allow_root {
            Ok(())
        } else {
            Err(ArtifactConfigError::new(ConfigProblem::NativePath))
        };
    }
    let mut count = 0usize;
    for component in bytes.split(|byte| *byte == b'/') {
        count += 1;
        if component.is_empty()
            || component.len() > COMPONENT_UNITS
            || matches!(component, b"." | b"..")
            || count > PATH_COMPONENTS
        {
            return Err(ArtifactConfigError::new(ConfigProblem::NativePath));
        }
    }
    Ok(())
}

#[cfg(windows)]
fn validate_absolute_native(value: &OsStr) -> Result<PathBuf, ArtifactConfigError> {
    use std::os::windows::ffi::OsStrExt;

    let units = value.encode_wide().collect::<Vec<_>>();
    if units.is_empty()
        || units.len() > WINDOWS_PATH_UNITS
        || units.iter().any(|unit| native_windows_control(*unit))
    {
        return Err(ArtifactConfigError::new(ConfigProblem::NativePath));
    }
    let path = PathBuf::from(value);
    validate_windows_components(&path, true)?;
    Ok(path)
}

#[cfg(windows)]
fn validate_relative_native(value: &OsStr) -> Result<PathBuf, ArtifactConfigError> {
    use std::os::windows::ffi::OsStrExt;

    let units = value.encode_wide().collect::<Vec<_>>();
    if units.is_empty()
        || units.len() > WINDOWS_PATH_UNITS
        || units.iter().any(|unit| native_windows_control(*unit))
    {
        return Err(ArtifactConfigError::new(ConfigProblem::RelativePath));
    }
    let path = PathBuf::from(value);
    validate_windows_components(&path, false)
        .map_err(|_| ArtifactConfigError::new(ConfigProblem::RelativePath))?;
    Ok(path)
}

#[cfg(windows)]
fn validate_windows_components(path: &Path, absolute: bool) -> Result<(), ArtifactConfigError> {
    use std::os::windows::ffi::OsStrExt;

    if path.is_absolute() != absolute {
        return Err(ArtifactConfigError::new(ConfigProblem::NativePath));
    }
    let mut count = 0usize;
    for component in path.components() {
        match component {
            Component::Prefix(_) if absolute => continue,
            Component::RootDir if absolute => continue,
            Component::Normal(value) => {
                let units = value.encode_wide().collect::<Vec<_>>();
                count += 1;
                if units.is_empty()
                    || units.len() > COMPONENT_UNITS
                    || count > PATH_COMPONENTS
                    || units.contains(&(b':' as u16))
                    || windows_device_name(&units)
                {
                    return Err(ArtifactConfigError::new(ConfigProblem::NativePath));
                }
            }
            _ => return Err(ArtifactConfigError::new(ConfigProblem::NativePath)),
        }
    }
    if count == 0 {
        return Err(ArtifactConfigError::new(ConfigProblem::NativePath));
    }
    Ok(())
}

#[cfg(any(test, windows))]
fn windows_device_name(units: &[u16]) -> bool {
    let mut stem = Vec::new();
    for unit in units
        .split(|unit| *unit == b'.' as u16)
        .next()
        .unwrap_or(units)
    {
        let byte = match *unit {
            0x00b9 => b'1',
            0x00b2 => b'2',
            0x00b3 => b'3',
            unit => {
                let Ok(byte) = u8::try_from(unit) else {
                    return false;
                };
                byte
            }
        };
        stem.push(byte.to_ascii_uppercase());
    }
    matches!(
        stem.as_slice(),
        b"CON"
            | b"PRN"
            | b"AUX"
            | b"NUL"
            | b"COM1"
            | b"COM2"
            | b"COM3"
            | b"COM4"
            | b"COM5"
            | b"COM6"
            | b"COM7"
            | b"COM8"
            | b"COM9"
            | b"LPT1"
            | b"LPT2"
            | b"LPT3"
            | b"LPT4"
            | b"LPT5"
            | b"LPT6"
            | b"LPT7"
            | b"LPT8"
            | b"LPT9"
    )
}

#[cfg(any(test, windows))]
fn native_windows_control(unit: u16) -> bool {
    unit <= 0x1f || unit == 0x7f
}

#[cfg(not(any(unix, windows)))]
fn validate_absolute_native(_: &OsStr) -> Result<PathBuf, ArtifactConfigError> {
    Err(ArtifactConfigError::new(ConfigProblem::NativePath))
}

#[cfg(not(any(unix, windows)))]
fn validate_relative_native(_: &OsStr) -> Result<PathBuf, ArtifactConfigError> {
    Err(ArtifactConfigError::new(ConfigProblem::RelativePath))
}

#[cfg(unix)]
fn read_selected_file(path: &Path) -> Result<String, ArtifactConfigError> {
    use std::{
        ffi::CString,
        os::{
            fd::{AsRawFd, FromRawFd},
            unix::{ffi::OsStrExt, fs::MetadataExt},
        },
    };

    let mut current = File::open("/").map_err(|_| ArtifactConfigError::new(ConfigProblem::File))?;
    let mut components = path.components().peekable();
    if !matches!(components.next(), Some(Component::RootDir)) {
        return Err(ArtifactConfigError::new(ConfigProblem::File));
    }
    while let Some(component) = components.next() {
        let Component::Normal(component) = component else {
            return Err(ArtifactConfigError::new(ConfigProblem::File));
        };
        let component = CString::new(component.as_bytes())
            .map_err(|_| ArtifactConfigError::new(ConfigProblem::File))?;
        let final_component = components.peek().is_none();
        let flags = if final_component {
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC
        } else {
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC
        };
        // SAFETY: `current` owns a live directory descriptor, `component` is
        // NUL-terminated, and a successful descriptor is immediately owned by
        // `File`. O_NOFOLLOW is applied at every untrusted component.
        let descriptor = unsafe { libc::openat(current.as_raw_fd(), component.as_ptr(), flags) };
        if descriptor < 0 {
            return Err(ArtifactConfigError::new(ConfigProblem::File));
        }
        // SAFETY: `openat` returned a new owned descriptor.
        current = unsafe { File::from_raw_fd(descriptor) };
    }

    let before = current
        .metadata()
        .map_err(|_| ArtifactConfigError::new(ConfigProblem::File))?;
    // SAFETY: libc exposes the current process effective user ID without
    // dereferencing memory or taking ownership.
    let effective_user = unsafe { libc::geteuid() };
    if !before.is_file()
        || before.uid() != effective_user
        || before.mode() & 0o022 != 0
        || before.nlink() != 1
        || before.len() > CONFIG_BYTES
    {
        return Err(ArtifactConfigError::new(ConfigProblem::File));
    }

    let mut bytes =
        Vec::with_capacity(usize::try_from(before.len()).unwrap_or(CONFIG_BYTES as usize));
    current
        .take(CONFIG_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| ArtifactConfigError::new(ConfigProblem::File))?;
    if bytes.len() as u64 > CONFIG_BYTES {
        return Err(ArtifactConfigError::new(ConfigProblem::File));
    }
    String::from_utf8(bytes).map_err(|_| ArtifactConfigError::new(ConfigProblem::File))
}

#[cfg(windows)]
fn read_selected_file(path: &Path) -> Result<String, ArtifactConfigError> {
    use std::os::windows::fs::MetadataExt;

    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
    let file = open_windows_selected_file(path)?;
    let metadata = file
        .metadata()
        .map_err(|_| ArtifactConfigError::new(ConfigProblem::File))?;
    let handle_metadata = crate::artifact_roots::windows_security::handle_metadata(&file)
        .map_err(|_| ArtifactConfigError::new(ConfigProblem::File))?;
    if !metadata.is_file()
        || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
        || handle_metadata.number_of_links != 1
        || metadata.file_size() > CONFIG_BYTES
        || !crate::artifact_roots::windows_security::owner_and_dacl_are_safe(&file).unwrap_or(false)
    {
        return Err(ArtifactConfigError::new(ConfigProblem::File));
    }
    let mut bytes = Vec::new();
    (&file)
        .take(CONFIG_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| ArtifactConfigError::new(ConfigProblem::File))?;
    if bytes.len() as u64 > CONFIG_BYTES {
        return Err(ArtifactConfigError::new(ConfigProblem::File));
    }
    String::from_utf8(bytes).map_err(|_| ArtifactConfigError::new(ConfigProblem::File))
}

#[cfg(windows)]
fn open_windows_selected_file(path: &Path) -> Result<File, ArtifactConfigError> {
    use std::os::windows::fs::MetadataExt;

    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
    let error = || ArtifactConfigError::new(ConfigProblem::File);
    let anchor = path
        .ancestors()
        .filter(|ancestor| !ancestor.as_os_str().is_empty())
        .last()
        .ok_or_else(error)?;
    let relative = path.strip_prefix(anchor).map_err(|_| error())?;
    let mut components = relative.components().peekable();
    let mut current = Dir::open_ambient_dir(anchor, cap_std::ambient_authority())
        .map(Dir::into_std_file)
        .map_err(|_| error())?;
    while let Some(component) = components.next() {
        let Component::Normal(component) = component else {
            return Err(error());
        };
        let final_component = components.peek().is_none();
        let mut options = OpenOptions::new();
        options
            .read(true)
            .follow(FollowSymlinks::No)
            .maybe_dir(!final_component);
        current = Dir::from_std_file(current)
            .open_with(Path::new(component), &options)
            .map(cap_std::fs::File::into_std)
            .map_err(|_| error())?;
        if !final_component {
            let metadata = current.metadata().map_err(|_| error())?;
            if !metadata.is_dir() || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
            {
                return Err(error());
            }
        }
    }
    if relative.as_os_str().is_empty() {
        return Err(error());
    }
    Ok(current)
}

#[cfg(not(any(unix, windows)))]
fn read_selected_file(_: &Path) -> Result<String, ArtifactConfigError> {
    Err(ArtifactConfigError::new(ConfigProblem::File))
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINIMAL: &str = "schema_version = 1\n[spaces]\nread_only = false\n";

    fn test_absolute_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(name)
    }

    #[test]
    fn no_selector_uses_cwd_file_before_defaults() {
        let selector =
            ConfigSelector::from_args_and_env(Vec::<OsString>::new(), None).expect("selector");
        let config = selector.load().expect("defaults");

        assert_eq!(
            selector.is_selected(),
            Path::new(DEFAULT_CONFIG_FILE).is_file()
        );
        assert_eq!(config.is_selected(), selector.is_selected());
        assert_eq!(config.import_root_count(), 0);
        assert_eq!(config.export_root_count(), 0);
        assert_eq!(config.spaces.allowed, None);
        assert_eq!(config.limits, ArtifactLimits::default());
    }

    #[test]
    fn command_line_wins_without_validating_environment() {
        let selected = test_absolute_path("any-mcp-config.toml");
        let selector = ConfigSelector::from_args_and_env(
            [
                OsString::from("--config"),
                selected.clone().into_os_string(),
            ],
            Some(OsString::from("relative-and-ignored")),
        )
        .expect("command line wins");

        assert!(selector.is_selected());

        let short = ConfigSelector::from_args_and_env(
            [OsString::from("-c"), selected.into_os_string()],
            Some(OsString::from("relative-and-ignored")),
        )
        .expect("short command-line selector wins");
        assert!(short.is_selected());
    }

    #[test]
    fn selector_rejects_unknown_duplicate_missing_and_relative_arguments() {
        for arguments in [
            vec![OsString::from("--unknown")],
            vec![OsString::from("--config")],
            vec![OsString::from("--config"), OsString::from("relative")],
            vec![
                OsString::from("--config"),
                OsString::from("/one"),
                OsString::from("--config"),
                OsString::from("/two"),
            ],
        ] {
            let error =
                ConfigSelector::from_args_and_env(arguments, None).expect_err("invalid selector");
            assert!(!error.to_string().contains("/one"));
        }
    }

    #[test]
    fn selected_schema_requires_version_and_deliberate_write_access() {
        assert!(ArtifactConfig::from_toml(MINIMAL).is_ok());
        assert!(ArtifactConfig::from_toml("[spaces]\nread_only = false\n").is_err());
        assert!(
            ArtifactConfig::from_toml("schema_version = 2\n[spaces]\nread_only = false\n").is_err()
        );
        assert!(
            ArtifactConfig::from_toml("schema_version = 1\n[spaces]\nread_only = true\n").is_err()
        );
        assert!(
            ArtifactConfig::from_toml(
                "schema_version = 1\nunknown = 1\n[spaces]\nread_only = false\n"
            )
            .is_err()
        );
    }

    #[test]
    fn auth_keystore_selectors_are_closed_and_path_safe_in_diagnostics() {
        let path = "/tmp/operator-keystore.db";
        let file =
            ArtifactConfig::from_toml(&format!("{MINIMAL}\n[auth]\nkeystore.file = \"{path}\"\n"))
                .expect("file keystore configuration");
        assert_eq!(
            file.keystore_spec().as_deref(),
            Some("file:path=/tmp/operator-keystore.db")
        );
        assert!(!format!("{file:?}").contains(path));

        let secret_service = ArtifactConfig::from_toml(
            "schema_version = 1\n[spaces]\nread_only = false\n\
             [auth]\nkeystore.secret-service = true\n",
        )
        .expect("secret-service configuration");
        assert_eq!(
            secret_service.keystore_spec().as_deref(),
            Some("secret-service")
        );

        for auth in [
            "keystore.file = \"\"",
            "keystore.secret-service = false",
            "keystore.file = \"/tmp/keys.db\"\nkeystore.secret-service = true",
        ] {
            let error = ArtifactConfig::from_toml(&format!("{MINIMAL}\n[auth]\n{auth}\n"))
                .expect_err("invalid auth configuration");
            assert_eq!(error.to_string(), "invalid any-mcp authentication policy");
            assert!(!error.to_string().contains("/tmp/keys.db"));
        }
    }

    /// Pins the two startup diagnostics the artifact acceptance harness
    /// asserts verbatim for a selected file whose writable-access declaration
    /// is absent or `true`.
    ///
    /// The acceptance harness cannot name this module (it is compiled both as
    /// an external test target and as a crate-internal module), so the exact
    /// texts are restated in
    /// `any-mcp/tests/support/artifact_acceptance.rs` as
    /// `READ_ONLY_MISSING_DIAGNOSTIC` and `READ_ONLY_TRUE_DIAGNOSTIC`. This
    /// test fails first when a production edit drifts either one.
    #[test]
    fn selected_access_declaration_diagnostics_are_stable_for_acceptance() {
        // Exactly the fixture layout: `[spaces]` on the second line.
        let missing = ArtifactConfig::from_toml(
            "schema_version = 1\n[spaces]\nallowed = [{ id = \"bafyrei-under-test\" }]\n",
        )
        .expect_err("absent access declaration");
        assert_eq!(
            missing.to_string(),
            "invalid any-mcp TOML configuration at line 2, column 1 in `spaces`: \
             required field is missing"
        );
        assert!(!missing.to_string().contains("bafyrei-under-test"));

        let read_only =
            ArtifactConfig::from_toml("schema_version = 1\n[spaces]\nread_only = true\n")
                .expect_err("read-only access declaration");
        assert_eq!(
            read_only.to_string(),
            "selected any-mcp configuration must declare spaces.read_only = false"
        );
    }

    #[test]
    fn toml_diagnostics_locate_schema_errors_without_echoing_values() {
        let secret = "operator-secret-value";
        let wrong_type = format!("schema_version = 1\n[spaces]\nread_only = \"{secret}\"\n");
        let error = ArtifactConfig::from_toml(&wrong_type).expect_err("wrong type");
        let diagnostic = error.to_string();
        assert!(diagnostic.contains("line 3, column 13"));
        assert!(diagnostic.contains("`spaces.read_only`"));
        assert!(diagnostic.contains("value has the wrong type"));
        assert!(!diagnostic.contains(secret));

        let unknown = ArtifactConfig::from_toml(
            "schema_version = 1\nunknown_policy = true\n[spaces]\nread_only = false\n",
        )
        .expect_err("unknown field")
        .to_string();
        assert!(unknown.contains("line 2, column 1"));
        assert!(unknown.contains("field is not recognized"));
        assert!(!unknown.contains("unknown_policy"));

        let missing = ArtifactConfig::from_toml("schema_version = 1\n[spaces]\n")
            .expect_err("missing field")
            .to_string();
        assert!(missing.contains("required field is missing"));

        let syntax = ArtifactConfig::from_toml("schema_version = 1\n[spaces\nread_only = false\n")
            .expect_err("malformed syntax")
            .to_string();
        assert!(syntax.contains("line 2"));
        assert!(syntax.contains("syntax or value does not match the configuration schema"));
    }

    #[test]
    fn logical_names_are_unicode_nfc_and_reject_invisible_aliases() {
        let composed = LogicalRootId::parse(" café ").expect("composed");
        let decomposed = LogicalRootId::parse("cafe\u{301}").expect("combining mark");
        assert_eq!(composed, decomposed);
        assert_eq!(composed.as_str(), "café");
        assert!(LogicalRootId::parse("\u{301}bad").is_err());
        assert!(LogicalRootId::parse("bad-\u{301}").is_err());
        assert!(LogicalRootId::parse("bad\u{200d}name").is_err());
        assert!(LogicalRootId::parse("bad name").is_err());
        assert!(LogicalRootId::parse("---").is_err());
        assert!(LogicalRootId::parse("日本語_١").is_ok());
    }

    #[test]
    fn root_identifiers_are_unique_after_canonicalization_and_across_capabilities() {
        let import = test_absolute_path("any-mcp-import");
        let export = test_absolute_path("any-mcp-export");
        let duplicate = format!(
            "{MINIMAL}\n[[roots.import]]\nid = \"café\"\npath = {import:?}\n\
             [[roots.export]]\nid = \"café\"\npath = {export:?}\n"
        );
        assert!(ArtifactConfig::from_toml(&duplicate).is_err());

        let case_collision = format!(
            "{MINIMAL}\n[[roots.import]]\nid = \"inbox\"\npath = {import:?}\n\
             [[roots.export]]\nid = \"INBOX\"\npath = {export:?}\n"
        );
        let error = ArtifactConfig::from_toml(&case_collision)
            .expect_err("ASCII case-colliding root identifiers");
        assert_eq!(error.to_string(), "invalid any-mcp artifact root");

        let distinct = format!(
            "{MINIMAL}\n[[roots.import]]\nid = \"InboxA\"\npath = {import:?}\n\
             [[roots.export]]\nid = \"inboxB\"\npath = {export:?}\n"
        );
        ArtifactConfig::from_toml(&distinct).expect("non-colliding mixed-case roots");
    }

    #[cfg(unix)]
    #[test]
    fn root_path_representations_are_exact_and_native_round_trip() {
        let bytes = URL_SAFE_NO_PAD.encode(b"/tmp/native-\xff");
        let config = ArtifactConfig::from_toml(&format!(
            "{MINIMAL}\n[[roots.import]]\nid = \"native\"\n\
             path_native = {{ encoding = \"unix-bytes-base64url\", value = \"{bytes}\" }}\n"
        ))
        .expect("native Unix path");
        assert_eq!(config.import_root_count(), 1);

        let ambiguous = format!(
            "{MINIMAL}\n[[roots.import]]\nid = \"bad\"\npath = \"/tmp/a\"\n\
             path_native = {{ encoding = \"unix-bytes-base64url\", value = \"{bytes}\" }}\n"
        );
        assert!(ArtifactConfig::from_toml(&ambiguous).is_err());
        assert!(AbsoluteNativePath::from_native("unix-bytes-base64url", "L3RtcA==").is_err());
    }

    #[test]
    fn relative_paths_reject_traversal_uri_and_separator_aliases() {
        assert!(RelativeNativePath::from_utf8("reports/q1.pdf").is_ok());
        for invalid in [
            "",
            "/absolute",
            "../escape",
            "a/../escape",
            "a//b",
            "a/",
            r"a\b",
            "file:report",
            "encoded%2fseparator",
        ] {
            assert!(
                RelativeNativePath::from_utf8(invalid).is_err(),
                "{invalid:?}"
            );
        }
    }

    #[test]
    fn native_relative_paths_reject_ascii_controls() {
        #[cfg(unix)]
        for path in [
            b"safe/nul\0byte".as_slice(),
            b"safe/line\nfeed".as_slice(),
            b"safe/delete\x7f".as_slice(),
        ] {
            let encoded = URL_SAFE_NO_PAD.encode(path);
            let error = RelativeNativePath::from_native("unix-bytes-base64url", &encoded)
                .expect_err("native Unix control byte");
            assert_eq!(error.to_string(), "invalid any-mcp relative artifact path");
        }

        #[cfg(windows)]
        for path in ["safe/line\nfeed", "safe/delete\u{7f}"] {
            let bytes = path
                .encode_utf16()
                .flat_map(u16::to_le_bytes)
                .collect::<Vec<_>>();
            let encoded = URL_SAFE_NO_PAD.encode(bytes);
            let error = RelativeNativePath::from_native("windows-wtf16le-base64url", &encoded)
                .expect_err("native Windows control unit");
            assert_eq!(error.to_string(), "invalid any-mcp relative artifact path");
        }

        for unit in [0, 0x1b, 0x1f, 0x7f] {
            assert!(native_windows_control(unit));
        }
        for unit in [0x20, b'A' as u16, 0x80] {
            assert!(!native_windows_control(unit));
        }
    }

    #[test]
    fn windows_reserved_device_components_are_path_syntax() {
        for name in [
            "CON",
            "nul.txt",
            "Com1",
            "LPT9.log",
            "COM¹",
            "COM².bin",
            "COM³",
            "LPT¹",
            "LPT².txt",
            "LPT³",
        ] {
            assert!(windows_device_name(
                &name.encode_utf16().collect::<Vec<_>>()
            ));
        }
        for name in ["console", "COM10", "LPT0", "nulled.txt", "CéON"] {
            assert!(!windows_device_name(
                &name.encode_utf16().collect::<Vec<_>>()
            ));
        }
    }

    #[test]
    fn explicit_empty_space_allowlist_differs_from_omitted() {
        let omitted = ArtifactConfig::from_toml(MINIMAL).expect("omitted");
        let empty = ArtifactConfig::from_toml(
            "schema_version = 1\n[spaces]\nread_only = false\nallowed = []\n",
        )
        .expect("empty");

        assert_eq!(omitted.spaces.allowed, None);
        assert_eq!(empty.spaces.allowed, Some(Vec::new()));
    }

    #[test]
    fn limits_enforce_individual_and_cross_field_bounds() {
        let one_mebibyte =
            ArtifactConfig::from_toml(&format!("{MINIMAL}\n[limits]\nartifact_bytes = 1048576\n"))
                .expect("one MiB artifact limit with inherited dependent limits");
        assert_eq!(one_mebibyte.limits.artifact_bytes, 1_048_576);
        assert_eq!(one_mebibyte.limits.transfer_chunk_bytes, 1_048_576);
        assert_eq!(one_mebibyte.limits.markdown_bytes, 1_048_576);

        let valid = ArtifactConfig::from_toml(&format!(
            "{MINIMAL}\n[limits]\nartifact_bytes = 1048576\n\
             transfer_chunk_bytes = 65536\nmarkdown_bytes = 1024\n\
             validator_total_input_bytes = 1048576\n"
        ))
        .expect("valid overridden limits");
        assert_eq!(valid.limits.artifact_bytes, 1_048_576);

        for limits in [
            "artifact_bytes = 0",
            "artifact_bytes = 1024\ntransfer_chunk_bytes = 65536",
            "artifact_bytes = 1024\nmarkdown_bytes = 2048",
            "staging_entries = 2\ncleanup_batch = 3",
        ] {
            let error = ArtifactConfig::from_toml(&format!("{MINIMAL}\n[limits]\n{limits}\n"))
                .expect_err("invalid limits");
            assert_eq!(error.to_string(), "invalid any-mcp artifact limit");
        }
    }

    #[test]
    fn staging_requires_loopback_and_https_base_when_enabled() {
        let staging = test_absolute_path("any-mcp-staging");
        let valid = format!(
            "{MINIMAL}\n[staging]\nenabled = true\nroot = {staging:?}\n\
             bind = \"127.0.0.1:8765\"\n\
             public_base_url = \"https://example.invalid/artifacts/v1/\"\n"
        );
        assert!(ArtifactConfig::from_toml(&valid).is_ok());
        assert!(ArtifactConfig::from_toml(&valid.replace("127.0.0.1", "0.0.0.0")).is_err());
        assert!(ArtifactConfig::from_toml(&valid.replace("https://", "http://")).is_err());
    }

    #[test]
    fn validator_policy_uses_closed_driver_and_mime_grammars() {
        let executable = test_absolute_path("any-mcp-file-validator");
        let valid = format!(
            "{MINIMAL}\n[[validators]]\nid = \"mime\"\ndriver = \"file-mime\"\n\
             path = {executable:?}\nsha256 = \"{}\"\nrequired = false\n\
             mime = [\"*/*\", \"image/*\", \"application/json\"]\ntimeout_secs = 5\n\
             memory_bytes = 67108864\ninput_bytes = 1048576\nstdout_bytes = 4096\n\
             stderr_bytes = 4096\nfields = 8\nfield_bytes = 1024\n\
             platform = \"linux-retained-fd-v1\"\n",
            "0".repeat(64)
        );
        let config = ArtifactConfig::from_toml(&valid).expect("validator policy");
        assert_eq!(config.validators().len(), 1);
        assert!(ArtifactConfig::from_toml(&valid.replace("file-mime", "shell")).is_err());
        assert!(ArtifactConfig::from_toml(&valid.replace("image/*", "image / *")).is_err());
        assert!(
            ArtifactConfig::from_toml(
                &valid.replace("application/json", "text/plain; charset=utf-8")
            )
            .is_err()
        );
    }

    #[test]
    fn debug_and_errors_do_not_expose_physical_paths() {
        let secret = test_absolute_path("operator-secret-root");
        let config = ArtifactConfig::from_toml(&format!(
            "{MINIMAL}\n[[roots.import]]\nid = \"inbox\"\npath = {secret:?}\n"
        ))
        .expect("config");
        assert!(!format!("{config:?}").contains(&secret.to_string_lossy().to_string()));

        let error = ArtifactConfig::from_toml(&format!(
            "{MINIMAL}\n[[roots.import]]\nid = \"inbox\"\npath = \"relative-secret\"\n"
        ))
        .expect_err("relative root");
        assert!(!error.to_string().contains("relative-secret"));
    }

    #[cfg(unix)]
    #[test]
    fn selected_file_rejects_symlinks_and_unsafe_permissions() {
        use std::{
            fs,
            os::unix::fs::{PermissionsExt, symlink},
        };

        let temporary = std::env::temp_dir()
            .canonicalize()
            .expect("canonical temporary directory")
            .join(format!(
                "any-mcp-config-{}-{}",
                std::process::id(),
                getrandom::u64().unwrap_or(0)
            ));
        fs::create_dir(&temporary).expect("temporary directory");
        let file = temporary.join("config.toml");
        fs::write(&file, MINIMAL).expect("write config");
        fs::set_permissions(&file, fs::Permissions::from_mode(0o600)).expect("permissions");
        let selector =
            ConfigSelector::from_args_and_env(Vec::<OsString>::new(), Some(file.clone().into()))
                .expect("selector");
        assert!(selector.load().is_ok());

        fs::set_permissions(&file, fs::Permissions::from_mode(0o622)).expect("unsafe permissions");
        assert!(selector.load().is_err());
        fs::set_permissions(&file, fs::Permissions::from_mode(0o600)).expect("safe permissions");
        let link = temporary.join("linked.toml");
        symlink(&file, &link).expect("symlink");
        let linked = ConfigSelector::from_args_and_env(Vec::<OsString>::new(), Some(link.into()))
            .expect("link selector");
        assert!(linked.load().is_err());

        fs::remove_dir_all(&temporary).expect("cleanup");
    }
}
