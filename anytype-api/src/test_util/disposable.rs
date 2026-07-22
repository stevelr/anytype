//! Destructive, prefix-authorized disposable-space test isolation.

use std::{
    any::Any,
    collections::BTreeMap,
    fmt,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
    time::{Duration, Instant},
};

use anytype_rpc::anytype::rpc::space::delete as space_delete;
use fs2::FileExt;
use futures::FutureExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tonic::Request;

use super::{
    AnytypeClient, AnytypeError, ChildOwnershipOutcome, ClientConfig, DisposableCallbackStage,
    DisposableFailureCategory, DisposableReadinessStage, DisposableSetupStage, Space, SpaceModel,
    TestContext, TestError, TestResult, VerifyConfig, space_delete_succeeded, with_token_request,
};

const PREFIX_ENV: &str = "ANYTYPE_TEST_SPACE_PREFIX";
const PREFIX_MAX: usize = 485;
const RANDOM_BYTES: usize = 16;
const RANDOM_BASE32_LEN: usize = 26;
const GENERATED_SUFFIX_LEN: usize = 27;
const PLAN_PAGE_LIMIT: u32 = 100;
const STATE_DIR_NAME: &str = "any-mcp-tests";
const LEDGER_VERSION: u8 = 2;
const PROCESS_GATE_ENV: &str = "ANYTYPE_DISPOSABLE_TEST_PROCESS";
const RECOVER_STOPPED_RUN_ENV: &str = "ANYTYPE_DISPOSABLE_RECOVER_STOPPED_RUN";
const CHILD_ENV_LIMIT: usize = 16_384;
const ARG_MAX_RESERVE: usize = 4_096;
const READINESS_TIMEOUT: Duration = Duration::from_secs(20);
const READINESS_MAX_ATTEMPTS: usize = 50;
const PAGE_TYPE_REFERENCE: &str = "@page";
const PAGE_TYPE_KEY: &str = "page";
const CREDENTIAL_NAMES: [&str; 4] = [
    "ANYTYPE_KEY_HTTP_TOKEN",
    "ANYTYPE_KEY_ACCOUNT_ID",
    "ANYTYPE_KEY_ACCOUNT_KEY",
    "ANYTYPE_KEY_SESSION_TOKEN",
];

/// Typed outcome of a disposable-space invocation.
#[doc(hidden)]
#[derive(Debug, PartialEq, Eq)]
pub enum DisposableRun<T> {
    /// The callback ran and returned this value.
    Completed(T),
    /// Admission intentionally skipped before authentication or filesystem I/O.
    Skipped(DisposableSkip),
}

/// Replaces a disposable callback error with closed, payload-free evidence.
///
/// API failures retain only [`AnytypeError::diagnostic`]'s static variant name.
/// Configuration and assertion text, identifiers, names, endpoints, queries,
/// credentials, and upstream bodies are discarded.
#[doc(hidden)]
pub fn disposable_callback_error(stage: DisposableCallbackStage, error: TestError) -> TestError {
    let category = match error {
        TestError::Api { source } => failure_category_from_anytype(&source),
        TestError::Env { .. } => DisposableFailureCategory::Environment,
        TestError::Config { .. } => DisposableFailureCategory::Config,
        TestError::DisposableReadiness { .. } => DisposableFailureCategory::Readiness,
        TestError::DisposableSetup { .. } => DisposableFailureCategory::Setup,
        TestError::DisposableCallback { .. } => DisposableFailureCategory::Callback,
        TestError::Assertion { .. } => DisposableFailureCategory::Assertion,
        TestError::SpaceCreateIndeterminate => DisposableFailureCategory::SpaceCreateIndeterminate,
    };
    TestError::DisposableCallback { stage, category }
}

fn failure_category_from_anytype(error: &AnytypeError) -> DisposableFailureCategory {
    match error {
        AnytypeError::Http { .. } => DisposableFailureCategory::HttpTransport,
        AnytypeError::ApiError { .. } => DisposableFailureCategory::ApiError,
        AnytypeError::ResponseTooLarge { .. } => DisposableFailureCategory::ResponseTooLarge,
        AnytypeError::FileHeaderEvidenceTooLarge { .. } => {
            DisposableFailureCategory::FileHeaderEvidenceTooLarge
        }
        AnytypeError::InvalidFileResponseHeader { .. } => {
            DisposableFailureCategory::InvalidFileResponseHeader
        }
        AnytypeError::ChatSseEventTooLarge { .. } => {
            DisposableFailureCategory::ChatSseEventTooLarge
        }
        AnytypeError::ChatSseTransport { .. } => DisposableFailureCategory::ChatSseTransport,
        AnytypeError::ChatTimestamp { .. } => DisposableFailureCategory::ChatTimestamp,
        AnytypeError::ChatHistoryEvidence { .. } => DisposableFailureCategory::ChatHistoryEvidence,
        AnytypeError::ChatEditTimestampNotAdvanced => {
            DisposableFailureCategory::ChatEditTimestampNotAdvanced
        }
        AnytypeError::TooManyRetries { .. } => DisposableFailureCategory::TooManyRetries,
        AnytypeError::Auth { .. } => DisposableFailureCategory::Auth,
        AnytypeError::Deserialization { .. } => DisposableFailureCategory::Deserialization,
        AnytypeError::Serialization { .. } => DisposableFailureCategory::Serialization,
        AnytypeError::NotFound { .. } => DisposableFailureCategory::NotFound,
        AnytypeError::Ambiguous { .. } => DisposableFailureCategory::Ambiguous,
        AnytypeError::ResolutionLimitExceeded { .. } => {
            DisposableFailureCategory::ResolutionLimitExceeded
        }
        AnytypeError::Unauthorized => DisposableFailureCategory::Unauthorized,
        AnytypeError::Forbidden => DisposableFailureCategory::Forbidden,
        AnytypeError::RateLimitExceeded { .. } => DisposableFailureCategory::RateLimit,
        AnytypeError::Validation { .. } => DisposableFailureCategory::Validation,
        AnytypeError::NoKeyStore => DisposableFailureCategory::NoKeystore,
        AnytypeError::Grpc { .. } => DisposableFailureCategory::Grpc,
        AnytypeError::GrpcUnavailable { .. } => DisposableFailureCategory::GrpcUnavailable,
        AnytypeError::KeyStore { .. } => DisposableFailureCategory::Keystore,
        AnytypeError::CacheDisabled => DisposableFailureCategory::CacheDisabled,
        AnytypeError::BodyGraph { .. } => DisposableFailureCategory::BodyGraph,
        AnytypeError::BodyMutationIndeterminate { .. } => {
            DisposableFailureCategory::BodyMutationIndeterminate
        }
        AnytypeError::BodyRpcLifecycle { .. } => DisposableFailureCategory::BodyRpcLifecycle,
        AnytypeError::CollectionMembershipEvidence { .. } => {
            DisposableFailureCategory::CollectionMembershipEvidence
        }
        AnytypeError::TypePropertyClassification { .. } => {
            DisposableFailureCategory::TypePropertyClassification
        }
        AnytypeError::AttachedDiscussion { .. } => DisposableFailureCategory::AttachedDiscussion,
        AnytypeError::VerifyTimeout { .. } => DisposableFailureCategory::VerifyTimeout,
        AnytypeError::Other { .. } => DisposableFailureCategory::Other,
    }
}

/// Secret-safe reason a disposable-space test was skipped.
#[doc(hidden)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DisposableSkip {
    /// `ANYTYPE_TEST_SPACE_PREFIX` was absent or not Unicode.
    PrefixNotConfigured,
    /// The configured prefix was empty or outside its strict ASCII grammar.
    PrefixInvalid,
    /// This platform cannot prove the private recovery-directory contract.
    PlatformIsolationUnavailable,
    /// The caller did not establish the dedicated, single-threaded process contract.
    ProcessIsolationUnavailable,
    /// Environment-only credential provisioning was absent or invalid.
    EnvironmentProvisioningUnavailable,
}

/// Exact, bounded environment admitted for a disposable test child.
///
/// Applying this value clears the command environment first. Values are kept
/// private because the map contains credentials and must never be formatted.
#[doc(hidden)]
#[derive(Clone)]
pub struct DisposableChildEnvironment {
    entries: Arc<Vec<(String, String)>>,
}

impl DisposableChildEnvironment {
    /// Clears ambient variables and installs the approved child environment.
    ///
    /// The complete executable, argument, and environment encoding is checked
    /// again immediately before the caller spawns the command.
    pub fn configure(&self, command: &mut Command) -> TestResult<()> {
        let program = command.get_program().to_string_lossy().into_owned();
        let arguments = command
            .get_args()
            .map(|value| value.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        validate_child_block(&self.entries, &program, &arguments, platform_arg_max())?;
        command.env_clear();
        command.envs(self.entries.iter().map(|(name, value)| (name, value)));
        Ok(())
    }
}

/// Fixed-category failure returned by [`with_disposable_space_context`].
///
/// Upstream bodies, credentials, paths, identifiers, names, and panic payloads
/// are retained privately and are never included in `Display` or `Debug`.
#[doc(hidden)]
pub struct DisposableTestError {
    category: DisposableErrorCategory,
    source: Option<Box<TestError>>,
    evidence: Box<CleanupEvidence>,
}

impl DisposableTestError {
    fn setup(source: TestError, evidence: CleanupEvidence) -> Self {
        Self {
            category: DisposableErrorCategory::Setup,
            source: Some(Box::new(source)),
            evidence: Box::new(evidence),
        }
    }

    fn cleanup(
        category: DisposableErrorCategory,
        source: Option<TestError>,
        evidence: CleanupEvidence,
    ) -> Self {
        Self {
            category,
            source: source.map(Box::new),
            evidence: Box::new(evidence),
        }
    }

    /// Returns a stable non-secret category suitable for test assertions.
    #[must_use]
    pub fn category(&self) -> &'static str {
        self.category.as_str()
    }

    /// Returns the secret-safe setup stage and category, when available.
    ///
    /// Readiness, callback, and cleanup failures return `None`. The diagnostic
    /// never contains an Anytype identifier, name, endpoint, credential, or
    /// upstream body.
    #[must_use]
    pub fn setup_failure(&self) -> Option<(&'static str, &'static str)> {
        match self.source.as_deref() {
            Some(TestError::DisposableSetup { stage, category }) => {
                Some((stage.as_str(), category.as_str()))
            }
            _ => None,
        }
    }

    /// Returns the secret-safe callback stage and error category, when available.
    ///
    /// A value proves the callback started and crossed the named boundary.
    /// Pre-callback setup/readiness and cleanup-only failures return `None`.
    #[must_use]
    pub fn callback_failure(&self) -> Option<(&'static str, &'static str)> {
        match self.source.as_deref() {
            Some(TestError::DisposableCallback { stage, category }) => {
                Some((stage.as_str(), category.as_str()))
            }
            _ => None,
        }
    }

    /// Returns the final secret-safe readiness stage, category, and attempt count.
    ///
    /// Other setup and cleanup failures return `None`. The diagnostic never
    /// contains an Anytype identifier, endpoint, credential, or upstream body.
    #[must_use]
    pub fn readiness_failure(&self) -> Option<(&'static str, &'static str, usize)> {
        match self.source.as_deref() {
            Some(TestError::DisposableReadiness {
                stage,
                category,
                attempts,
            }) => Some((stage.as_str(), category.as_str(), *attempts)),
            _ => None,
        }
    }
}

impl fmt::Display for DisposableTestError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.category.as_str())
    }
}

impl fmt::Debug for DisposableTestError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DisposableTestError")
            .field("category", &self.category.as_str())
            .field("primary_error_retained", &self.source.is_some())
            .field("setup_failure", &self.setup_failure())
            .field("readiness_failure", &self.readiness_failure())
            .field("callback_failure", &self.callback_failure())
            .field("evidence", &self.evidence)
            .finish()
    }
}

impl std::error::Error for DisposableTestError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DisposableErrorCategory {
    Setup,
    PrimaryPanic,
    CleanupDefect,
    AbsenceUnproven,
    HarnessStateCleanup,
}

impl DisposableErrorCategory {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Setup => "disposable test setup failed",
            Self::PrimaryPanic => "disposable test primary stage panicked",
            Self::CleanupDefect => "disposable test cleanup defect",
            Self::AbsenceUnproven => "disposable test space absence unproven",
            Self::HarnessStateCleanup => "disposable test harness state cleanup failed",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum StageOutcome {
    #[default]
    NotRun,
    Success,
    Error,
    Panic,
    DeleteAcknowledged,
    DeleteIndeterminate,
    Verified,
    Unproven,
}

#[derive(Default)]
struct CleanupEvidence {
    primary: StageOutcome,
    child: StageOutcome,
    delete: StageOutcome,
    absence: StageOutcome,
    credentials: StageOutcome,
    ledger: StageOutcome,
    panic_payloads: Vec<Box<dyn Any + Send>>,
}

impl fmt::Debug for CleanupEvidence {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CleanupEvidence")
            .field("primary", &self.primary)
            .field("child", &self.child)
            .field("delete", &self.delete)
            .field("absence", &self.absence)
            .field("credentials", &self.credentials)
            .field("ledger", &self.ledger)
            .field("panic_payload_count", &self.panic_payloads.len())
            .finish()
    }
}

struct CompositePanic {
    category: DisposableErrorCategory,
    evidence: CleanupEvidence,
}

impl fmt::Debug for CompositePanic {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DisposableTestCompositePanic")
            .field("category", &self.category.as_str())
            .field("payload_count", &self.evidence.panic_payloads.len())
            .field("evidence", &self.evidence)
            .finish()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct DisposablePrefix(String);

impl DisposablePrefix {
    fn from_environment() -> Result<Self, DisposableSkip> {
        Self::admit_environment_value(std::env::var(PREFIX_ENV))
    }

    fn admit_environment_value(
        value: Result<String, std::env::VarError>,
    ) -> Result<Self, DisposableSkip> {
        let value = value.map_err(|_| DisposableSkip::PrefixNotConfigured)?;
        Self::parse(value).map_err(|_| DisposableSkip::PrefixInvalid)
    }

    fn parse(value: String) -> TestResult<Self> {
        if value.is_empty()
            || value.len() > PREFIX_MAX
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        {
            return Err(config_error("invalid disposable-space prefix"));
        }
        Ok(Self(value))
    }

    fn authorizes(&self, name: &str) -> bool {
        name.get(..self.0.len())
            .is_some_and(|candidate| candidate.eq_ignore_ascii_case(&self.0))
    }

    fn generate_name(&self) -> TestResult<String> {
        let mut random = [0_u8; RANDOM_BYTES];
        getrandom::fill(&mut random).map_err(|_| config_error("operating-system RNG failed"))?;
        Ok(self.name_from_random(random))
    }

    fn name_from_random(&self, random: [u8; RANDOM_BYTES]) -> String {
        let suffix = base32_lower_unpadded(random);
        debug_assert_eq!(suffix.len(), RANDOM_BASE32_LEN);
        let name = format!("{}-{suffix}", self.0);
        debug_assert_eq!(name.len(), self.0.len() + GENERATED_SUFFIX_LEN);
        name
    }
}

fn base32_lower_unpadded(input: [u8; RANDOM_BYTES]) -> String {
    const ALPHABET: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";
    let mut output = String::with_capacity(RANDOM_BASE32_LEN);
    let mut buffer = 0_u32;
    let mut bits = 0_u8;
    for byte in input {
        buffer = (buffer << 8) | u32::from(byte);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            output.push(ALPHABET[((buffer >> bits) & 31) as usize] as char);
        }
    }
    if bits > 0 {
        output.push(ALPHABET[((buffer << (5 - bits)) & 31) as usize] as char);
    }
    output
}

#[derive(Debug, Serialize, Deserialize)]
struct RunLedger {
    version: u8,
    backend_key: String,
    created_unix_ms: u128,
    phase: LedgerPhase,
    #[serde(default)]
    plan: Option<String>,
    #[serde(default)]
    plan_state: PlanState,
    credential_mode: String,
    credential_state: CredentialState,
    #[serde(default)]
    child_state: ChildState,
    #[serde(default)]
    recovery_action: RecoveryAction,
    #[serde(default)]
    create_name: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum PlanState {
    #[default]
    None,
    Allocated,
    Complete,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum CredentialState {
    Ready,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ChildState {
    #[default]
    NotStarted,
    Running,
    Stopped,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum RecoveryAction {
    #[default]
    None,
    ProveChildStoppedOrGone,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum LedgerPhase {
    Admitted,
    Sweeping,
    Running,
    Cleaning,
}

struct HarnessState {
    root: PathBuf,
    ledger_path: PathBuf,
    ledger: RunLedger,
}

#[derive(Clone)]
struct ChildLedgerMarker {
    root: PathBuf,
    ledger_path: PathBuf,
    backend_key: String,
}

impl ChildLedgerMarker {
    fn mark_running(&self) -> TestResult<()> {
        let ledger = read_run_ledger(&self.ledger_path)?;
        if ledger.backend_key != self.backend_key
            || ledger.version != LEDGER_VERSION
            || ledger.credential_mode != "env"
            || ledger.credential_state != CredentialState::Ready
            || ledger.child_state == ChildState::Stopped
            || ledger.recovery_action != RecoveryAction::None
        {
            return Err(config_error("invalid child-running ledger transition"));
        }
        let mut state = HarnessState {
            root: self.root.clone(),
            ledger_path: self.ledger_path.clone(),
            ledger,
        };
        if state.ledger.child_state == ChildState::NotStarted {
            state.mark_child_running()?;
        }
        Ok(())
    }
}

impl HarnessState {
    fn create(root: PathBuf, backend_key: String) -> TestResult<Self> {
        let handle = random_handle("run")?;
        let ledger_path = root.join(format!("{handle}.json"));
        let ledger = RunLedger {
            version: LEDGER_VERSION,
            backend_key,
            created_unix_ms: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|_| config_error("system clock precedes Unix epoch"))?
                .as_millis(),
            phase: LedgerPhase::Admitted,
            plan: None,
            plan_state: PlanState::None,
            credential_mode: "env".to_owned(),
            credential_state: CredentialState::Ready,
            child_state: ChildState::NotStarted,
            recovery_action: RecoveryAction::None,
            create_name: None,
        };
        let state = Self {
            root,
            ledger_path,
            ledger,
        };
        state.persist()?;
        Ok(state)
    }

    fn set_phase(&mut self, phase: LedgerPhase) -> TestResult<()> {
        self.ledger.phase = phase;
        self.persist()
    }

    fn child_marker(&self) -> ChildLedgerMarker {
        ChildLedgerMarker {
            root: self.root.clone(),
            ledger_path: self.ledger_path.clone(),
            backend_key: self.ledger.backend_key.clone(),
        }
    }

    fn reload(&mut self) -> TestResult<()> {
        let ledger = read_run_ledger(&self.ledger_path)?;
        if ledger.backend_key != self.ledger.backend_key || ledger.version != LEDGER_VERSION {
            return Err(config_error("run ledger identity changed"));
        }
        self.ledger = ledger;
        Ok(())
    }

    fn record_create_intent(&mut self, name: String) -> TestResult<()> {
        self.ledger.create_name = Some(name);
        self.ledger.phase = LedgerPhase::Running;
        self.persist()
    }

    fn allocate_plan(&mut self) -> TestResult<PathBuf> {
        let handle = format!("{}.plan", random_handle("sweep")?);
        self.ledger.plan = Some(handle.clone());
        self.ledger.plan_state = PlanState::Allocated;
        self.ledger.phase = LedgerPhase::Sweeping;
        self.persist()?;
        let path = self.root.join(handle);
        drop(create_private_file(&path)?);
        initialize_plan_database(&path)?;
        fsync_directory(&self.root)?;
        Ok(path)
    }

    fn mark_plan_complete(&mut self) -> TestResult<()> {
        if self.ledger.plan.is_none() {
            return Err(config_error("missing sweep plan"));
        }
        self.ledger.plan_state = PlanState::Complete;
        self.persist()
    }

    fn mark_child_running(&mut self) -> TestResult<()> {
        if self.ledger.credential_mode != "env"
            || self.ledger.credential_state != CredentialState::Ready
        {
            return Err(config_error(
                "child requires prepared environment credentials",
            ));
        }
        self.ledger.child_state = ChildState::Running;
        self.ledger.recovery_action = RecoveryAction::None;
        self.persist()
    }

    fn mark_child_stopped(&mut self) -> TestResult<()> {
        self.ledger.child_state = ChildState::Stopped;
        self.ledger.recovery_action = RecoveryAction::None;
        self.persist()
    }

    fn require_recovery_child_proof(&mut self) -> TestResult<()> {
        if self.ledger.child_state != ChildState::Running {
            return Err(config_error("recovery child proof requires running state"));
        }
        self.ledger.recovery_action = RecoveryAction::ProveChildStoppedOrGone;
        self.persist()
    }

    fn confirm_recovery_child_stopped(&mut self) -> TestResult<()> {
        if self.ledger.child_state != ChildState::Running
            || self.ledger.recovery_action != RecoveryAction::ProveChildStoppedOrGone
        {
            return Err(config_error("invalid recovered-child stopped transition"));
        }
        self.ledger.child_state = ChildState::Stopped;
        self.ledger.recovery_action = RecoveryAction::None;
        self.persist()
    }

    fn remove_plan(&mut self, path: &Path) -> TestResult<()> {
        if path.parent() != Some(self.root.as_path())
            || self.ledger.plan.as_deref() != path.file_name().and_then(|name| name.to_str())
        {
            return Err(config_error("invalid sweep plan handle"));
        }
        remove_private_plan_if_present(path)?;
        fsync_directory(&self.root)?;
        self.ledger.plan = None;
        self.ledger.plan_state = PlanState::None;
        self.persist()
    }

    fn finish(mut self) -> TestResult<()> {
        if self.ledger.child_state == ChildState::Running {
            return Err(config_error("owned child may still be running"));
        }
        if let Some(handle) = self.ledger.plan.clone() {
            let path = self.root.join(handle);
            self.remove_plan(&path)?;
        }
        remove_private_file_if_present(&self.ledger_path)?;
        fsync_directory(&self.root)
    }

    fn persist(&self) -> TestResult<()> {
        let file_name = self
            .ledger_path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| config_error("invalid ledger handle"))?;
        let temporary = self.root.join(format!(
            ".backend-{}.{}.new",
            self.ledger.backend_key, file_name
        ));
        let bytes =
            serde_json::to_vec(&self.ledger).map_err(|_| config_error("serialize run ledger"))?;
        remove_private_file_if_present(&temporary)?;
        let mut file = create_private_file(&temporary)?;
        file.write_all(&bytes)
            .and_then(|()| file.sync_all())
            .map_err(|_| config_error("persist run ledger"))?;
        fs::rename(&temporary, &self.ledger_path)
            .map_err(|_| config_error("replace run ledger"))?;
        fsync_directory(&self.root)
    }
}

struct BackendLease {
    _file: File,
}

impl BackendLease {
    fn acquire(root: &Path, key: &str) -> TestResult<Self> {
        let path = root.join(format!("backend-{key}.lock"));
        let file = open_or_create_private_file(&path)?;
        file.try_lock_exclusive()
            .map_err(|_| config_error("disposable backend lease is busy"))?;
        Ok(Self { _file: file })
    }
}

fn private_state_root() -> TestResult<PathBuf> {
    #[cfg(unix)]
    let base = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .ok_or_else(|| config_error("owner runtime directory unavailable"))?;
    #[cfg(windows)]
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .ok_or_else(|| config_error("local application-data directory unavailable"))?;

    let root = base.join(STATE_DIR_NAME);
    if !root.exists() {
        create_private_directory(&root)?;
    }
    drop(open_private_directory(&root)?);
    Ok(root)
}

#[cfg(unix)]
fn verify_private_metadata(metadata: &fs::Metadata, directory: bool) -> TestResult<()> {
    use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
    // SAFETY: geteuid has no preconditions and does not dereference pointers.
    if metadata.uid() != unsafe { libc::geteuid() } {
        return Err(config_error("private recovery owner mismatch"));
    }
    if metadata.permissions().mode() & 0o077 != 0 {
        return Err(config_error("private recovery permissions are too broad"));
    }
    let kind_matches = if directory {
        metadata.is_dir()
    } else {
        metadata.is_file()
            && !metadata.file_type().is_socket()
            && !metadata.file_type().is_fifo()
            && !metadata.file_type().is_block_device()
            && !metadata.file_type().is_char_device()
    };
    if !kind_matches {
        return Err(config_error("private recovery object kind mismatch"));
    }
    Ok(())
}

#[cfg(windows)]
fn verify_private_metadata(_metadata: &fs::Metadata, _directory: bool) -> TestResult<()> {
    // Fail closed until this helper can prove current-user ownership, a
    // private DACL, and reparse-point refusal with Windows-native APIs.
    Err(config_error(
        "private Windows recovery-directory verification unavailable",
    ))
}

fn open_private_directory(path: &Path) -> TestResult<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    let file = options
        .open(path)
        .map_err(|_| config_error("open private recovery directory"))?;
    verify_private_metadata(
        &file
            .metadata()
            .map_err(|_| config_error("inspect private recovery directory"))?,
        true,
    )?;
    Ok(file)
}

#[cfg(unix)]
fn private_component_name(path: &Path) -> TestResult<(&Path, &std::ffi::OsStr)> {
    let parent = path
        .parent()
        .ok_or_else(|| config_error("private recovery entry has no parent"))?;
    let name = path
        .file_name()
        .ok_or_else(|| config_error("private recovery entry has no filename"))?;
    Ok((parent, name))
}

#[cfg(unix)]
fn component_c_string(name: &std::ffi::OsStr) -> TestResult<std::ffi::CString> {
    use std::os::unix::ffi::OsStrExt;
    if Path::new(name).components().count() != 1 {
        return Err(config_error("invalid private recovery component"));
    }
    std::ffi::CString::new(name.as_bytes())
        .map_err(|_| config_error("invalid private recovery component"))
}

#[cfg(unix)]
fn open_private_at(
    directory: &File,
    name: &std::ffi::OsStr,
    is_directory: bool,
) -> TestResult<File> {
    use std::os::fd::{AsRawFd, FromRawFd};

    let name = component_c_string(name)?;
    let (access_flag, kind_flag) = if is_directory {
        (libc::O_RDONLY, libc::O_DIRECTORY)
    } else {
        (libc::O_RDWR, 0)
    };
    // SAFETY: both file descriptors and the NUL-terminated component pointer
    // are valid for the duration of this call. O_NOFOLLOW rejects a substituted
    // final-component symlink.
    let descriptor = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            name.as_ptr(),
            access_flag | libc::O_CLOEXEC | libc::O_NOFOLLOW | kind_flag,
        )
    };
    if descriptor < 0 {
        return Err(config_error(
            "open private recovery entry relative to directory",
        ));
    }
    // SAFETY: openat returned a new owned descriptor.
    let file = unsafe { File::from_raw_fd(descriptor) };
    verify_private_metadata(
        &file
            .metadata()
            .map_err(|_| config_error("inspect private recovery entry"))?,
        is_directory,
    )?;
    Ok(file)
}

#[cfg(unix)]
fn unlink_private_at(
    directory: &File,
    name: &std::ffi::OsStr,
    is_directory: bool,
) -> TestResult<()> {
    use std::os::fd::AsRawFd;

    let name = component_c_string(name)?;
    let flags = if is_directory { libc::AT_REMOVEDIR } else { 0 };
    // SAFETY: the directory descriptor and NUL-terminated component pointer
    // remain valid for the call. unlinkat never follows the final component.
    let result = unsafe { libc::unlinkat(directory.as_raw_fd(), name.as_ptr(), flags) };
    if result == 0 {
        Ok(())
    } else {
        Err(config_error(
            "remove private recovery entry relative to directory",
        ))
    }
}

fn create_private_directory(path: &Path) -> TestResult<()> {
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder
        .create(path)
        .map_err(|_| config_error("create private recovery directory"))?;
    drop(open_private_directory(path)?);
    Ok(())
}

fn create_private_file(path: &Path) -> TestResult<File> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    let file = options
        .open(path)
        .map_err(|_| config_error("create private recovery file"))?;
    verify_private_metadata(
        &file
            .metadata()
            .map_err(|_| config_error("inspect private recovery file"))?,
        false,
    )?;
    Ok(file)
}

fn open_private_file(path: &Path) -> TestResult<File> {
    let mut options = OpenOptions::new();
    options.read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    let file = options
        .open(path)
        .map_err(|_| config_error("open private recovery file"))?;
    verify_private_metadata(
        &file
            .metadata()
            .map_err(|_| config_error("inspect private recovery file"))?,
        false,
    )?;
    Ok(file)
}

fn open_or_create_private_file(path: &Path) -> TestResult<File> {
    match create_private_file(path) {
        Ok(file) => Ok(file),
        Err(_) if path.exists() => open_private_file(path),
        Err(error) => Err(error),
    }
}

fn remove_private_file_if_present(path: &Path) -> TestResult<()> {
    #[cfg(unix)]
    {
        let (parent, name) = private_component_name(path)?;
        let directory = open_private_directory(parent)?;
        match open_private_at(&directory, name, false) {
            Ok(file) => {
                drop(file);
                unlink_private_at(&directory, name, false)
            }
            Err(_) if !path_entry_exists(path)? => Ok(()),
            Err(error) => Err(error),
        }
    }
    #[cfg(not(unix))]
    match open_private_file(path) {
        Ok(file) => {
            drop(file);
            fs::remove_file(path).map_err(|_| config_error("remove private recovery file"))
        }
        Err(_) if !path_entry_exists(path)? => Ok(()),
        Err(error) => Err(error),
    }
}

fn remove_private_plan_if_present(path: &Path) -> TestResult<()> {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| config_error("invalid sweep plan filename"))?;
    if !name.ends_with(".plan") {
        return Err(config_error("invalid sweep plan filename"));
    }
    for suffix in ["-journal", "-wal", "-shm"] {
        remove_private_file_if_present(&path.with_file_name(format!("{name}{suffix}")))?;
    }
    remove_private_file_if_present(path)
}

fn path_entry_exists(path: &Path) -> TestResult<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(_) => Err(config_error("inspect private recovery entry")),
    }
}

fn fsync_directory(path: &Path) -> TestResult<()> {
    open_private_directory(path)?
        .sync_all()
        .map_err(|_| config_error("fsync recovery directory"))
}

fn random_handle(prefix: &str) -> TestResult<String> {
    let mut random = [0_u8; RANDOM_BYTES];
    getrandom::fill(&mut random).map_err(|_| config_error("operating-system RNG failed"))?;
    Ok(format!("{prefix}-{}", base32_lower_unpadded(random)))
}

fn valid_random_handle(value: &str, prefix: &str) -> bool {
    value
        .strip_prefix(prefix)
        .and_then(|value| value.strip_prefix('-'))
        .is_some_and(|random| {
            random.len() == RANDOM_BASE32_LEN
                && random
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || (b'2'..=b'7').contains(&byte))
        })
}

fn read_run_ledger(path: &Path) -> TestResult<RunLedger> {
    let mut file = open_private_file(path)?;
    let mut bytes = Vec::new();
    std::io::Read::by_ref(&mut file)
        .take(4_097)
        .read_to_end(&mut bytes)
        .map_err(|_| config_error("read run ledger"))?;
    if bytes.len() > 4_096 {
        return Err(config_error("oversized run ledger"));
    }
    serde_json::from_slice(&bytes).map_err(|_| config_error("malformed run ledger"))
}

fn run_handle_from_ledger_name(name: &str) -> Option<&str> {
    name.strip_suffix(".json")
        .filter(|handle| valid_random_handle(handle, "run"))
}

fn prepare_prior_child_recovery(
    state: &HarnessState,
    confirmed_stopped_run: Option<&str>,
) -> TestResult<()> {
    if confirmed_stopped_run.is_some_and(|name| run_handle_from_ledger_name(name).is_none()) {
        return Err(config_error("invalid recovered-child confirmation handle"));
    }
    let mut confirmation_used = false;
    let entries = fs::read_dir(&state.root).map_err(|_| config_error("enumerate run ledgers"))?;
    for entry in entries {
        let entry = entry.map_err(|_| config_error("enumerate run ledger"))?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| config_error("invalid run ledger filename"))?;
        if run_handle_from_ledger_name(&name).is_none() || entry.path() == state.ledger_path {
            continue;
        }
        let ledger = read_run_ledger(&entry.path())?;
        if ledger.version != LEDGER_VERSION {
            return Err(config_error("unsupported run ledger version"));
        }
        if ledger.backend_key != state.ledger.backend_key {
            continue;
        }
        if ledger.child_state != ChildState::Running {
            if ledger.recovery_action != RecoveryAction::None {
                return Err(config_error("invalid recovered-child operator action"));
            }
            continue;
        }

        let mut recovered = HarnessState {
            root: state.root.clone(),
            ledger_path: entry.path(),
            ledger,
        };
        match recovered.ledger.recovery_action {
            RecoveryAction::None => {
                recovered.require_recovery_child_proof()?;
                return Err(config_error(
                    "recovered child must be proven stopped or gone before cleanup",
                ));
            }
            RecoveryAction::ProveChildStoppedOrGone
                if confirmed_stopped_run == Some(name.as_str()) =>
            {
                recovered.confirm_recovery_child_stopped()?;
                confirmation_used = true;
            }
            RecoveryAction::ProveChildStoppedOrGone => {
                return Err(config_error(
                    "recovered child still requires stopped-or-gone confirmation",
                ));
            }
        }
    }
    if confirmed_stopped_run.is_some() && !confirmation_used {
        return Err(config_error(
            "recovered-child confirmation was not applicable",
        ));
    }
    Ok(())
}

async fn recover_prior_ledgers(
    client: &AnytypeClient,
    prefix: &DisposablePrefix,
    state: &HarnessState,
    confirmed_stopped_run: Option<&str>,
    deadline: Instant,
) -> TestResult<()> {
    prepare_prior_child_recovery(state, confirmed_stopped_run)?;
    check_deadline(deadline)?;
    let entries = fs::read_dir(&state.root).map_err(|_| config_error("enumerate run ledgers"))?;
    for entry in entries {
        check_deadline(deadline)?;
        let entry = entry.map_err(|_| config_error("enumerate run ledger"))?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| config_error("invalid run ledger filename"))?;
        let temporary_prefix = format!(".backend-{}.", state.ledger.backend_key);
        if let Some(handle) = name
            .strip_prefix(&temporary_prefix)
            .and_then(|name| name.strip_suffix(".json.new"))
        {
            if !valid_random_handle(handle, "run") {
                return Err(config_error("invalid temporary run ledger filename"));
            }
            remove_private_file_if_present(&entry.path())?;
            fsync_directory(&state.root)?;
            continue;
        }
        let Some(_handle) = run_handle_from_ledger_name(&name) else {
            continue;
        };
        let path = entry.path();
        if path == state.ledger_path {
            continue;
        }
        let ledger = read_run_ledger(&path)?;
        if ledger.version != LEDGER_VERSION {
            return Err(config_error("unsupported run ledger version"));
        }
        if ledger.backend_key != state.ledger.backend_key {
            continue;
        }
        if ledger
            .create_name
            .as_deref()
            .is_some_and(|name| !prefix.authorizes(name) || name.chars().count() > 512)
        {
            return Err(config_error("invalid recorded create intent"));
        }
        if let Some(plan_handle) = ledger.plan {
            let Some(plan_random) = plan_handle.strip_suffix(".plan") else {
                return Err(config_error("invalid recorded sweep plan handle"));
            };
            if !valid_random_handle(plan_random, "sweep") {
                return Err(config_error("invalid recorded sweep plan handle"));
            }
            let plan = state.root.join(plan_handle);
            if path_entry_exists(&plan)? {
                drop(open_private_file(&plan)?);
                if ledger.plan_state == PlanState::Complete {
                    apply_plan(client, prefix, &plan, deadline).await?;
                }
            }
            remove_private_plan_if_present(&plan)?;
        }
        if ledger.child_state == ChildState::Running {
            return Err(config_error("recovered ledger may have a surviving child"));
        }
        remove_private_file_if_present(&path)?;
        fsync_directory(&state.root)?;
    }
    Ok(())
}

fn backend_key(config: &ClientConfig) -> TestResult<String> {
    let http = config
        .base_url
        .as_deref()
        .ok_or_else(|| config_error("missing HTTP endpoint"))?;
    let grpc = config
        .grpc_endpoint
        .as_deref()
        .ok_or_else(|| config_error("missing gRPC endpoint"))?;
    let http = canonical_loopback_endpoint(http)?;
    let grpc = canonical_loopback_endpoint(grpc)?;
    let mut digest = Sha256::new();
    digest.update(http.as_bytes());
    digest.update([0]);
    digest.update(grpc.as_bytes());
    Ok(digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

fn canonical_loopback_endpoint(endpoint: &str) -> TestResult<String> {
    let endpoint = reqwest::Url::parse(endpoint)
        .map_err(|_| config_error("invalid disposable backend endpoint"))?;
    if !matches!(endpoint.scheme(), "http" | "https") {
        return Err(config_error("unsupported disposable backend scheme"));
    }
    let host = endpoint
        .host_str()
        .ok_or_else(|| config_error("missing disposable backend host"))?;
    let address = host
        .parse::<std::net::IpAddr>()
        .map_err(|_| config_error("disposable backend host must be a loopback address"))?;
    if !address.is_loopback() {
        return Err(config_error(
            "remote disposable backend requires scheduler lease",
        ));
    }
    let authority = match address {
        std::net::IpAddr::V4(address) => address.to_string(),
        std::net::IpAddr::V6(address) => format!("[{address}]"),
    };
    let port = endpoint
        .port_or_known_default()
        .ok_or_else(|| config_error("missing disposable backend port"))?;
    Ok(format!("{}://{authority}:{port}", endpoint.scheme()))
}

struct EnvironmentProvisioning {
    config: ClientConfig,
    child: DisposableChildEnvironment,
    captured: BTreeMap<String, String>,
    recover_stopped_run: Option<String>,
}

fn process_isolation_admission() -> Result<(), DisposableSkip> {
    if std::env::var(PROCESS_GATE_ENV).as_deref() != Ok("1") {
        return Err(DisposableSkip::ProcessIsolationUnavailable);
    }
    #[cfg(unix)]
    unsafe {
        // SAFETY: getrlimit/setrlimit operate on the supplied initialized value.
        let mut limit = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        if libc::getrlimit(libc::RLIMIT_CORE, &mut limit) != 0 {
            return Err(DisposableSkip::ProcessIsolationUnavailable);
        }
        limit.rlim_cur = 0;
        if libc::setrlimit(libc::RLIMIT_CORE, &limit) != 0 {
            return Err(DisposableSkip::ProcessIsolationUnavailable);
        }
        #[cfg(target_os = "linux")]
        if libc::prctl(libc::PR_SET_DUMPABLE, 0) != 0 {
            return Err(DisposableSkip::ProcessIsolationUnavailable);
        }
    }
    Ok(())
}

fn capture_environment() -> Result<EnvironmentProvisioning, DisposableSkip> {
    let mut captured = BTreeMap::new();
    for (raw_name, raw_value) in std::env::vars_os() {
        let Some(name) = raw_name.to_str() else {
            return Err(DisposableSkip::EnvironmentProvisioningUnavailable);
        };
        let relevant = matches!(
            name,
            "ANYTYPE_KEYSTORE"
                | "ANYTYPE_KEYSTORE_SERVICE"
                | "ANYTYPE_RATE_LIMIT_MAX_RETRIES"
                | RECOVER_STOPPED_RUN_ENV
                | "ANYTYPE_URL"
                | "ANYTYPE_GRPC_ENDPOINT"
                | "SystemRoot"
        ) || name.starts_with("ANYTYPE_KEY_");
        if !relevant {
            continue;
        }
        let Some(value) = raw_value.to_str() else {
            return Err(DisposableSkip::EnvironmentProvisioningUnavailable);
        };
        if captured.insert(name.to_owned(), value.to_owned()).is_some() {
            return Err(DisposableSkip::EnvironmentProvisioningUnavailable);
        }
    }
    if captured.get("ANYTYPE_KEYSTORE").map(String::as_str) != Some("env") {
        return Err(DisposableSkip::EnvironmentProvisioningUnavailable);
    }
    let service = captured
        .get("ANYTYPE_KEYSTORE_SERVICE")
        .filter(|value| valid_service(value))
        .cloned()
        .ok_or(DisposableSkip::EnvironmentProvisioningUnavailable)?;
    if captured
        .keys()
        .any(|name| name.starts_with("ANYTYPE_KEY_") && !CREDENTIAL_NAMES.contains(&name.as_str()))
    {
        return Err(DisposableSkip::EnvironmentProvisioningUnavailable);
    }
    let credential = |name: &str| captured.get(name).filter(|value| !value.is_empty());
    if credential("ANYTYPE_KEY_HTTP_TOKEN").is_none()
        || (credential("ANYTYPE_KEY_SESSION_TOKEN").is_none()
            && credential("ANYTYPE_KEY_ACCOUNT_KEY").is_none())
        || captured
            .get("ANYTYPE_KEY_ACCOUNT_ID")
            .is_some_and(String::is_empty)
    {
        return Err(DisposableSkip::EnvironmentProvisioningUnavailable);
    }
    if captured
        .get("ANYTYPE_RATE_LIMIT_MAX_RETRIES")
        .is_some_and(|value| value != "5")
    {
        return Err(DisposableSkip::EnvironmentProvisioningUnavailable);
    }
    let http = captured
        .get("ANYTYPE_URL")
        .cloned()
        .ok_or(DisposableSkip::EnvironmentProvisioningUnavailable)?;
    let grpc = captured
        .get("ANYTYPE_GRPC_ENDPOINT")
        .cloned()
        .ok_or(DisposableSkip::EnvironmentProvisioningUnavailable)?;
    let recover_stopped_run = captured.get(RECOVER_STOPPED_RUN_ENV).cloned();
    if recover_stopped_run
        .as_deref()
        .is_some_and(|name| run_handle_from_ledger_name(name).is_none())
    {
        return Err(DisposableSkip::EnvironmentProvisioningUnavailable);
    }
    canonical_loopback_endpoint(&http)
        .and_then(|_| canonical_loopback_endpoint(&grpc))
        .map_err(|_| DisposableSkip::EnvironmentProvisioningUnavailable)?;

    let mut child = vec![
        ("ANYTYPE_URL".to_owned(), http.clone()),
        ("ANYTYPE_GRPC_ENDPOINT".to_owned(), grpc.clone()),
        ("ANYTYPE_RATE_LIMIT_MAX_RETRIES".to_owned(), "5".to_owned()),
        ("ANYTYPE_KEYSTORE".to_owned(), "env".to_owned()),
        ("ANYTYPE_KEYSTORE_SERVICE".to_owned(), service.clone()),
        ("ANY_MCP_PROTOCOL".to_owned(), "stable".to_owned()),
        ("ANY_MCP_PROFILE".to_owned(), "standard".to_owned()),
        ("ANY_MCP_READ_ONLY".to_owned(), "0".to_owned()),
        ("ANY_MCP_MAX_CONCURRENCY".to_owned(), "8".to_owned()),
        ("ANY_MCP_REQUEST_TIMEOUT_SECS".to_owned(), "30".to_owned()),
        ("ANY_MCP_STARTUP_TIMEOUT_SECS".to_owned(), "15".to_owned()),
        (
            "ANY_MCP_JSON_RESPONSE_BYTES".to_owned(),
            "8388608".to_owned(),
        ),
        (
            "ANY_MCP_DOCUMENT_RESPONSE_BYTES".to_owned(),
            "67108864".to_owned(),
        ),
        ("RUST_LOG".to_owned(), "any_mcp=info".to_owned()),
    ];
    for name in CREDENTIAL_NAMES {
        if let Some(value) = captured.get(name) {
            child.push((name.to_owned(), value.clone()));
        }
    }
    #[cfg(windows)]
    child.push((
        "SystemRoot".to_owned(),
        captured
            .get("SystemRoot")
            .cloned()
            .ok_or(DisposableSkip::EnvironmentProvisioningUnavailable)?,
    ));
    child.sort_by(|left, right| left.0.cmp(&right.0));
    validate_child_block(&child, "any-mcp", &[], platform_arg_max())
        .map_err(|_| DisposableSkip::EnvironmentProvisioningUnavailable)?;
    let config = ClientConfig {
        base_url: Some(http),
        grpc_endpoint: Some(grpc),
        app_name: "anytype_disposable_test".to_owned(),
        keystore: Some("env".to_owned()),
        keystore_service: Some(service),
        rate_limit_max_retries: 5,
        disable_cache: true,
        verify: Some(VerifyConfig::default()),
        ..ClientConfig::default()
    };
    Ok(EnvironmentProvisioning {
        config,
        child: DisposableChildEnvironment {
            entries: Arc::new(child),
        },
        captured,
        recover_stopped_run,
    })
}

fn valid_service(value: &str) -> bool {
    (1..=128).contains(&value.len())
        && value.bytes().enumerate().all(|(index, byte)| {
            byte.is_ascii_alphanumeric() || (index > 0 && matches!(byte, b'.' | b'_' | b'-'))
        })
}

fn relevant_environment_matches(expected: &BTreeMap<String, String>) -> bool {
    let Ok(current) = capture_relevant_environment() else {
        return false;
    };
    &current == expected
}

fn capture_relevant_environment() -> Result<BTreeMap<String, String>, ()> {
    let mut values = BTreeMap::new();
    for (name, value) in std::env::vars_os() {
        let name = name.to_str().ok_or(())?;
        if (matches!(
            name,
            "ANYTYPE_KEYSTORE"
                | "ANYTYPE_KEYSTORE_SERVICE"
                | "ANYTYPE_RATE_LIMIT_MAX_RETRIES"
                | RECOVER_STOPPED_RUN_ENV
                | "ANYTYPE_URL"
                | "ANYTYPE_GRPC_ENDPOINT"
                | "SystemRoot"
        ) || name.starts_with("ANYTYPE_KEY_"))
            && values
                .insert(name.to_owned(), value.to_str().ok_or(())?.to_owned())
                .is_some()
        {
            return Err(());
        }
    }
    Ok(values)
}

fn validate_child_block(
    entries: &[(String, String)],
    program: &str,
    arguments: &[String],
    arg_max: Option<usize>,
) -> TestResult<()> {
    let mut units = std::mem::size_of::<usize>();
    #[cfg(unix)]
    {
        for (name, value) in entries {
            units = units
                .checked_add(name.len())
                .and_then(|total| total.checked_add(1 + value.len() + 1))
                .and_then(|total| total.checked_add(std::mem::size_of::<usize>()))
                .ok_or_else(|| config_error("child environment budget overflow"))?;
        }
        let argv_count = arguments
            .len()
            .checked_add(2)
            .ok_or_else(|| config_error("child argument budget overflow"))?;
        let argv_bytes = arguments
            .iter()
            .try_fold(program.len() + 1, |total, value| {
                total.checked_add(value.len() + 1)
            })
            .ok_or_else(|| config_error("child argument budget overflow"))?;
        let argv_cost = argv_bytes
            .checked_add(argv_count * std::mem::size_of::<usize>())
            .and_then(|value| value.checked_add(ARG_MAX_RESERVE))
            .ok_or_else(|| config_error("child argument budget overflow"))?;
        let effective = arg_max
            .and_then(|limit| limit.checked_sub(argv_cost))
            .map_or(0, |limit| limit.min(CHILD_ENV_LIMIT));
        if units > effective {
            return Err(config_error("child environment exceeds total budget"));
        }
    }
    #[cfg(windows)]
    {
        units = 1;
        for (name, value) in entries {
            units = units
                .checked_add(name.encode_utf16().count() + 1 + value.encode_utf16().count() + 1)
                .ok_or_else(|| config_error("child environment budget overflow"))?;
        }
        if units > CHILD_ENV_LIMIT {
            return Err(config_error("child environment exceeds total budget"));
        }
        let _ = (program, arguments, arg_max);
    }
    Ok(())
}

#[cfg(unix)]
fn platform_arg_max() -> Option<usize> {
    // SAFETY: sysconf has no pointer arguments and changes no process state.
    let value = unsafe { libc::sysconf(libc::_SC_ARG_MAX) };
    usize::try_from(value).ok().filter(|value| *value > 0)
}

#[cfg(windows)]
fn platform_arg_max() -> Option<usize> {
    None
}

enum ExactSpace {
    Absent,
    Present(Space),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ReadinessFailure {
    stage: DisposableReadinessStage,
    category: DisposableFailureCategory,
}

impl ReadinessFailure {
    const fn new(stage: DisposableReadinessStage, category: DisposableFailureCategory) -> Self {
        Self { stage, category }
    }

    fn from_api(stage: DisposableReadinessStage, error: &AnytypeError) -> Self {
        Self::new(stage, failure_category_from_anytype(error))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ReadinessAttempt {
    Ready,
    Retry(ReadinessFailure),
    Terminal(ReadinessFailure),
}

#[derive(Debug, Default)]
struct ReadinessConvergence {
    attempts: usize,
    last_failure: Option<ReadinessFailure>,
}

impl ReadinessConvergence {
    fn observe(&mut self, observation: ReadinessAttempt) -> ReadinessAttempt {
        self.attempts = self.attempts.saturating_add(1);
        match observation {
            ReadinessAttempt::Ready => ReadinessAttempt::Ready,
            ReadinessAttempt::Retry(failure) => {
                self.last_failure = Some(failure);
                ReadinessAttempt::Retry(failure)
            }
            ReadinessAttempt::Terminal(failure) => {
                self.last_failure = Some(failure);
                ReadinessAttempt::Terminal(failure)
            }
        }
    }

    fn error(&self) -> TestError {
        let failure = self.last_failure.unwrap_or(ReadinessFailure::new(
            DisposableReadinessStage::Readiness,
            DisposableFailureCategory::NotObserved,
        ));
        TestError::DisposableReadiness {
            stage: failure.stage,
            category: failure.category,
            attempts: self.attempts,
        }
    }

    fn mark_timeout(&mut self) {
        self.last_failure = Some(ReadinessFailure::new(
            DisposableReadinessStage::Readiness,
            DisposableFailureCategory::Timeout,
        ));
    }
}

fn readiness_error_is_terminal(error: &AnytypeError) -> bool {
    match error {
        AnytypeError::ApiError { code, .. } => {
            (400..=499).contains(code) && !matches!(code, 404 | 408 | 409 | 425 | 429)
        }
        AnytypeError::Auth { .. }
        | AnytypeError::Unauthorized
        | AnytypeError::Forbidden
        | AnytypeError::Validation { .. }
        | AnytypeError::NoKeyStore
        | AnytypeError::KeyStore { .. }
        | AnytypeError::CacheDisabled
        | AnytypeError::Ambiguous { .. }
        | AnytypeError::ResolutionLimitExceeded { .. }
        | AnytypeError::Serialization { .. }
        | AnytypeError::Deserialization { .. }
        | AnytypeError::ResponseTooLarge { .. }
        | AnytypeError::Other { .. } => true,
        _ => false,
    }
}

fn classify_readiness_api_failure(
    stage: DisposableReadinessStage,
    error: &AnytypeError,
) -> ReadinessAttempt {
    let failure = ReadinessFailure::from_api(stage, error);
    if readiness_error_is_terminal(error) {
        ReadinessAttempt::Terminal(failure)
    } else {
        ReadinessAttempt::Retry(failure)
    }
}

async fn readiness_attempt(
    client: &AnytypeClient,
    prefix: &DisposablePrefix,
    space_id: &str,
) -> ReadinessAttempt {
    match exact_space(client, space_id).await {
        Ok(ExactSpace::Absent) => {
            return ReadinessAttempt::Retry(ReadinessFailure::new(
                DisposableReadinessStage::Space,
                DisposableFailureCategory::NotFound,
            ));
        }
        Ok(ExactSpace::Present(space)) if !prefix.authorizes(&space.name) => {
            return ReadinessAttempt::Terminal(ReadinessFailure::new(
                DisposableReadinessStage::Space,
                DisposableFailureCategory::IdentityMismatch,
            ));
        }
        Ok(ExactSpace::Present(_)) => {}
        Err(TestError::Api { source }) => {
            return classify_readiness_api_failure(DisposableReadinessStage::Space, &source);
        }
        Err(_) => {
            return ReadinessAttempt::Terminal(ReadinessFailure::new(
                DisposableReadinessStage::Space,
                DisposableFailureCategory::InvalidEvidence,
            ));
        }
    }

    let resolved = match client.resolve_type(space_id, PAGE_TYPE_REFERENCE).await {
        Ok(typ) => typ,
        Err(error) => {
            return classify_readiness_api_failure(DisposableReadinessStage::TypeResolve, &error);
        }
    };
    if resolved.key != PAGE_TYPE_KEY || resolved.archived {
        return ReadinessAttempt::Terminal(ReadinessFailure::new(
            DisposableReadinessStage::TypeResolve,
            DisposableFailureCategory::IdentityMismatch,
        ));
    }

    let direct = match client.get_type(space_id, &resolved.id).get_direct().await {
        Ok(typ) => typ,
        Err(error) => {
            return classify_readiness_api_failure(DisposableReadinessStage::TypeDirect, &error);
        }
    };
    if direct.id != resolved.id || direct.key != PAGE_TYPE_KEY || direct.archived {
        return ReadinessAttempt::Terminal(ReadinessFailure::new(
            DisposableReadinessStage::TypeDirect,
            DisposableFailureCategory::IdentityMismatch,
        ));
    }

    ReadinessAttempt::Ready
}

async fn exact_space(client: &AnytypeClient, space_id: &str) -> TestResult<ExactSpace> {
    match client.space(space_id).get().await {
        Ok(space) => {
            if space.id != space_id || space.object != SpaceModel::Space {
                return Err(config_error("exact space identity mismatch"));
            }
            client
                .get_config()
                .limits
                .validate_name(&space.name, "exact space name")?;
            if space.name.chars().any(char::is_control) {
                return Err(config_error("invalid exact space name"));
            }
            Ok(ExactSpace::Present(space))
        }
        Err(AnytypeError::NotFound { .. }) | Err(AnytypeError::ApiError { code: 404, .. }) => {
            Ok(ExactSpace::Absent)
        }
        Err(error) => Err(TestError::from(error)),
    }
}

fn validate_created_space(
    limits: &crate::validation::ValidationLimits,
    prefix: &DisposablePrefix,
    expected_name: &str,
    created: &Space,
    ambient_space_ids: &[String],
) -> TestResult<()> {
    if limits.validate_id(&created.id, "disposable space").is_err() {
        return Err(setup_error(
            DisposableSetupStage::CreateResponse,
            DisposableFailureCategory::InvalidId,
        ));
    }
    if created.object != SpaceModel::Space {
        return Err(setup_error(
            DisposableSetupStage::CreateResponse,
            DisposableFailureCategory::ModelMismatch,
        ));
    }
    if created.name != expected_name || !prefix.authorizes(&created.name) {
        return Err(setup_error(
            DisposableSetupStage::CreateResponse,
            DisposableFailureCategory::NameMismatch,
        ));
    }
    if ambient_space_ids.iter().any(|id| id == &created.id) {
        return Err(setup_error(
            DisposableSetupStage::CreateResponse,
            DisposableFailureCategory::AmbientCollision,
        ));
    }
    Ok(())
}

fn classify_disposable_space_create_error(error: AnytypeError) -> TestError {
    match super::classify_space_create_error(error) {
        TestError::SpaceCreateIndeterminate => setup_error(
            DisposableSetupStage::SpaceCreate,
            DisposableFailureCategory::Indeterminate,
        ),
        _ => setup_error(
            DisposableSetupStage::SpaceCreate,
            DisposableFailureCategory::ApiRejected,
        ),
    }
}

async fn wait_ready(
    client: &AnytypeClient,
    prefix: &DisposablePrefix,
    space_id: &str,
) -> TestResult<()> {
    let mut config = client.get_config().verify.clone().unwrap_or_default();
    config.timeout = READINESS_TIMEOUT;
    config.max_attempts = READINESS_MAX_ATTEMPTS;
    let deadline = Instant::now() + config.timeout;
    let mut delay = config.initial_delay;
    let mut convergence = ReadinessConvergence::default();
    for attempt in 0..config.max_attempts {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            convergence.mark_timeout();
            break;
        }
        let observation =
            tokio::time::timeout(remaining, readiness_attempt(client, prefix, space_id))
                .await
                .unwrap_or_else(|_| {
                    ReadinessAttempt::Terminal(ReadinessFailure::new(
                        DisposableReadinessStage::Readiness,
                        DisposableFailureCategory::Timeout,
                    ))
                });
        match convergence.observe(observation) {
            ReadinessAttempt::Ready => return Ok(()),
            ReadinessAttempt::Terminal(_) => break,
            ReadinessAttempt::Retry(_) => {}
        }
        if attempt + 1 == config.max_attempts {
            break;
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            convergence.mark_timeout();
            break;
        }
        tokio::time::sleep(delay.min(remaining)).await;
        delay = delay.saturating_mul(2).min(config.max_delay);
    }
    Err(convergence.error())
}

async fn delete_known_space(
    client: &AnytypeClient,
    prefix: &DisposablePrefix,
    space_id: &str,
    deadline: Instant,
) -> TestResult<StageOutcome> {
    let first = with_deadline(deadline, exact_space(client, space_id)).await?;
    if !authorized_delete_presence(prefix, &first)? {
        return Ok(StageOutcome::Success);
    }
    let second = with_deadline(deadline, exact_space(client, space_id)).await?;
    if !authorized_delete_presence(prefix, &second)? {
        return Ok(StageOutcome::Success);
    }
    let grpc = with_deadline(deadline, async {
        client.grpc_client().await.map_err(TestError::from)
    })
    .await?;
    let request = with_token_request(
        Request::new(space_delete::Request {
            space_id: space_id.to_owned(),
        }),
        grpc.token(),
    )
    .map_err(TestError::from)?;
    let outcome = with_deadline(deadline, async {
        Ok(grpc
            .client_commands()
            .space_delete(request)
            .await
            .map(|response| response.into_inner())
            .map(|response| {
                space_delete_succeeded(response.error.as_ref().map(|error| error.code))
            }))
    })
    .await?;
    Ok(if outcome.unwrap_or(false) {
        StageOutcome::DeleteAcknowledged
    } else {
        StageOutcome::DeleteIndeterminate
    })
}

fn authorized_delete_presence(prefix: &DisposablePrefix, space: &ExactSpace) -> TestResult<bool> {
    match space {
        ExactSpace::Absent => Ok(false),
        ExactSpace::Present(space) if prefix.authorizes(&space.name) => Ok(true),
        ExactSpace::Present(_) => Err(config_error(
            "disposable space renamed outside authorized prefix",
        )),
    }
}

async fn prove_absent(client: &AnytypeClient, space_id: &str, deadline: Instant) -> TestResult<()> {
    let config = client.get_config().verify.clone().unwrap_or_default();
    let mut delay = config.initial_delay;
    for attempt in 0..config.max_attempts.max(1) {
        if matches!(
            with_deadline(deadline, exact_space(client, space_id)).await?,
            ExactSpace::Absent
        ) {
            return Ok(());
        }
        if attempt + 1 == config.max_attempts.max(1) || Instant::now() >= deadline {
            break;
        }
        sleep_before_deadline(delay, deadline).await?;
        delay = delay.saturating_mul(2).min(config.max_delay);
    }
    Err(config_error("disposable space absence unproven"))
}

async fn sweep(
    client: &AnytypeClient,
    prefix: &DisposablePrefix,
    state: &mut HarnessState,
    deadline: Instant,
) -> TestResult<()> {
    loop {
        check_deadline(deadline)?;
        let plan = state.allocate_plan()?;
        let enumeration = enumerate_plan(client, prefix, &plan, deadline).await?;
        if enumeration == EnumerationOutcome::Unstable {
            state.remove_plan(&plan)?;
            continue;
        }
        state.mark_plan_complete()?;
        let planned = plan_selected_count(&plan)?;
        if planned == 0 {
            state.remove_plan(&plan)?;
            return Ok(());
        }
        apply_plan(client, prefix, &plan, deadline).await?;
        state.remove_plan(&plan)?;
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EnumerationOutcome {
    Complete,
    Unstable,
}

fn initialize_plan_database(path: &Path) -> TestResult<()> {
    let connection = open_plan_database(path)?;
    connection
        .execute_batch(
            "PRAGMA journal_mode=DELETE; PRAGMA synchronous=FULL;\
             CREATE TABLE IF NOT EXISTS seen (id TEXT PRIMARY KEY, selected INTEGER NOT NULL CHECK(selected IN (0,1)));",
        )
        .map_err(|_| config_error("initialize sweep plan database"))
}

fn open_plan_database(path: &Path) -> TestResult<rusqlite::Connection> {
    drop(open_private_file(path)?);
    rusqlite::Connection::open_with_flags(
        path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|_| config_error("open sweep plan database"))
}

fn plan_selected_count(path: &Path) -> TestResult<usize> {
    let connection = open_plan_database(path)?;
    let count = connection
        .query_row("SELECT count(*) FROM seen WHERE selected=1", (), |row| {
            row.get::<_, i64>(0)
        })
        .map_err(|_| config_error("count sweep plan records"))?;
    usize::try_from(count).map_err(|_| config_error("invalid sweep plan record count"))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PageIngest {
    Next(u32),
    Complete,
    Unstable,
}

fn ingest_space_page(
    connection: &mut rusqlite::Connection,
    limits: &crate::validation::ValidationLimits,
    prefix: &DisposablePrefix,
    response: crate::paged::PaginatedResponse<Space>,
    expected_offset: u32,
    stable_total: &mut Option<usize>,
) -> TestResult<PageIngest> {
    if stable_total.is_some_and(|total| total != response.pagination.total) {
        return Ok(PageIngest::Unstable);
    }
    let offset = usize::try_from(expected_offset)
        .map_err(|_| config_error("space inventory offset overflow"))?;
    let expected_len = response
        .pagination
        .total
        .checked_sub(offset)
        .map(|remaining| remaining.min(PLAN_PAGE_LIMIT as usize))
        .ok_or_else(|| config_error("space inventory offset exceeds total"))?;
    let expected_more = offset
        .checked_add(PLAN_PAGE_LIMIT as usize)
        .is_some_and(|next| next < response.pagination.total);
    if response.pagination.offset != expected_offset
        || response.pagination.limit != PLAN_PAGE_LIMIT
        || response.items.len() != expected_len
        || response.pagination.has_more != expected_more
    {
        return Err(config_error("corrupt disposable space pagination"));
    }
    stable_total.get_or_insert(response.pagination.total);
    let transaction = connection
        .transaction()
        .map_err(|_| config_error("begin sweep plan page"))?;
    for space in response.items {
        limits.validate_id(&space.id, "space inventory id")?;
        limits.validate_name(&space.name, "space inventory name")?;
        if space.object != SpaceModel::Space || space.name.chars().any(char::is_control) {
            return Err(config_error("invalid disposable space inventory identity"));
        }
        let inserted = transaction.execute(
            "INSERT INTO seen(id, selected) VALUES (?1, ?2)",
            (
                &space.id,
                if prefix.authorizes(&space.name) {
                    1_i64
                } else {
                    0_i64
                },
            ),
        );
        if inserted.is_err() {
            return Err(config_error("duplicate disposable space inventory id"));
        }
    }
    transaction
        .commit()
        .map_err(|_| config_error("commit sweep plan page"))?;
    if expected_more {
        Ok(PageIngest::Next(
            expected_offset
                .checked_add(PLAN_PAGE_LIMIT)
                .ok_or_else(|| config_error("space inventory offset overflow"))?,
        ))
    } else {
        Ok(PageIngest::Complete)
    }
}

async fn enumerate_plan(
    client: &AnytypeClient,
    prefix: &DisposablePrefix,
    plan: &Path,
    deadline: Instant,
) -> TestResult<EnumerationOutcome> {
    let mut connection = open_plan_database(plan)?;
    let mut offset = 0_u32;
    let mut stable_total = None;
    loop {
        let response = with_deadline(deadline, async {
            client
                .spaces()
                .limit(PLAN_PAGE_LIMIT)
                .offset(offset)
                .list()
                .await
                .map(|page| page.into_response())
                .map_err(TestError::from)
        })
        .await?;
        match ingest_space_page(
            &mut connection,
            &client.get_config().limits,
            prefix,
            response,
            offset,
            &mut stable_total,
        )? {
            PageIngest::Next(next) => offset = next,
            PageIngest::Complete => {
                connection
                    .execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
                    .map_err(|_| config_error("fsync complete sweep plan"))?;
                open_private_file(plan)?
                    .sync_all()
                    .map_err(|_| config_error("fsync complete sweep plan"))?;
                return Ok(EnumerationOutcome::Complete);
            }
            PageIngest::Unstable => return Ok(EnumerationOutcome::Unstable),
        }
    }
}

async fn apply_plan(
    client: &AnytypeClient,
    prefix: &DisposablePrefix,
    plan: &Path,
    deadline: Instant,
) -> TestResult<()> {
    let mut after = None;
    while let Some(space_id) = next_selected_plan_id(plan, after.as_deref())? {
        check_deadline(deadline)?;
        client
            .get_config()
            .limits
            .validate_id(&space_id, "sweep plan id")?;
        delete_known_space(client, prefix, &space_id, deadline).await?;
        prove_absent(client, &space_id, deadline).await?;
        after = Some(space_id);
    }
    Ok(())
}

fn next_selected_plan_id(path: &Path, after: Option<&str>) -> TestResult<Option<String>> {
    let connection = open_plan_database(path)?;
    let mut statement = connection
        .prepare(
            "SELECT id FROM seen WHERE selected=1 AND (?1 IS NULL OR id > ?1) ORDER BY id LIMIT 1",
        )
        .map_err(|_| config_error("prepare sweep plan record"))?;
    let mut rows = statement
        .query([after])
        .map_err(|_| config_error("read sweep plan record"))?;
    let row = rows
        .next()
        .map_err(|_| config_error("read sweep plan record"))?;
    row.map(|row| {
        row.get::<_, String>(0)
            .map_err(|_| config_error("read sweep plan record"))
    })
    .transpose()
}

fn check_deadline(deadline: Instant) -> TestResult<()> {
    if Instant::now() >= deadline {
        Err(config_error("disposable sweep deadline exceeded"))
    } else {
        Ok(())
    }
}

async fn with_deadline<T>(
    deadline: Instant,
    future: impl std::future::Future<Output = TestResult<T>>,
) -> TestResult<T> {
    check_deadline(deadline)?;
    let remaining = deadline.saturating_duration_since(Instant::now());
    tokio::time::timeout(remaining, future)
        .await
        .map_err(|_| config_error("disposable sweep deadline exceeded"))?
}

async fn sleep_before_deadline(delay: Duration, deadline: Instant) -> TestResult<()> {
    check_deadline(deadline)?;
    let remaining = deadline.saturating_duration_since(Instant::now());
    tokio::time::sleep(delay.min(remaining)).await;
    check_deadline(deadline)
}

enum Guarded<T> {
    Value(T),
    Error(TestError),
    Panic(Box<dyn Any + Send>),
}

impl<T> Guarded<T> {
    fn or_error(
        self,
        evidence: &mut CleanupEvidence,
        category: DisposableErrorCategory,
    ) -> Result<T, DisposableTestError> {
        match self {
            Self::Value(value) => Ok(value),
            failure => Err(failure.into_error(evidence, category)),
        }
    }

    fn into_primary_failure<U>(self) -> std::thread::Result<TestResult<U>> {
        match self {
            Self::Error(error) => Ok(Err(error)),
            Self::Panic(payload) => Err(payload),
            Self::Value(_) => unreachable!("success cannot be converted into a failed stage"),
        }
    }

    fn into_error(
        self,
        evidence: &mut CleanupEvidence,
        category: DisposableErrorCategory,
    ) -> DisposableTestError {
        match self {
            Self::Error(error) => DisposableTestError::setup(error, std::mem::take(evidence)),
            Self::Panic(payload) => std::panic::resume_unwind(payload),
            Self::Value(_) => {
                DisposableTestError::cleanup(category, None, std::mem::take(evidence))
            }
        }
    }

    fn retain_panic(self, evidence: &mut CleanupEvidence) {
        if let Self::Panic(payload) = self {
            evidence.panic_payloads.push(payload);
        }
    }
}

fn guarded_sync<T>(operation: impl FnOnce() -> TestResult<T>) -> Guarded<T> {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(operation)) {
        Ok(Ok(value)) => Guarded::Value(value),
        Ok(Err(error)) => Guarded::Error(error),
        Err(payload) => Guarded::Panic(payload),
    }
}

async fn guarded_async<T>(future: impl std::future::Future<Output = TestResult<T>>) -> Guarded<T> {
    match std::panic::AssertUnwindSafe(future).catch_unwind().await {
        Ok(Ok(value)) => Guarded::Value(value),
        Ok(Err(error)) => Guarded::Error(error),
        Err(payload) => Guarded::Panic(payload),
    }
}

fn guarded_finish_state(state: HarnessState, evidence: &mut CleanupEvidence) {
    match guarded_sync(|| state.finish()) {
        Guarded::Value(()) if evidence.ledger == StageOutcome::NotRun => {
            evidence.ledger = StageOutcome::Success;
        }
        Guarded::Value(()) => {}
        Guarded::Error(_) => evidence.ledger = StageOutcome::Error,
        Guarded::Panic(payload) => {
            evidence.ledger = StageOutcome::Panic;
            evidence.panic_payloads.push(payload);
        }
    }
}

/// Runs one callback in a fresh prefix-authorized Anytype space.
///
/// `ANYTYPE_TEST_SPACE_PREFIX` is mandatory and is checked before client
/// construction. Its case-insensitive prefix grants deletion authority over
/// every matching current space name. The helper acquires a backend-wide
/// process lock, persists a recovery ledger, sweeps interrupted matching runs,
/// creates a cryptographically unique space, and gives its scoped REST state at
/// most 20 seconds and 50 attempts to converge. Readiness requires an exact
/// `@page` key resolution followed by a cache-independent direct GET whose ID,
/// key, archive state, and requested space path all agree. Terminal failures
/// expose only a closed stage/category and attempt count. The helper then runs
/// child cleanup, deletes by exact ID after two fresh name checks, proves
/// absence independently, and performs a final sweep.
///
/// The operator must reserve the prefix exclusively for tests and must not
/// create, rename, or delete spaces through another client while this helper
/// holds its lease.
#[doc(hidden)]
#[allow(clippy::future_not_send)]
pub async fn with_disposable_space_context<F, Fut, T>(
    _suite: &str,
    test_fn: F,
) -> Result<DisposableRun<T>, DisposableTestError>
where
    F: FnOnce(Arc<TestContext>) -> Fut,
    Fut: std::future::Future<Output = TestResult<T>>,
{
    let mut evidence = CleanupEvidence::default();
    let prefix = match DisposablePrefix::from_environment() {
        Ok(prefix) => prefix,
        Err(skip) => return Ok(DisposableRun::Skipped(skip)),
    };
    if let Err(skip) = platform_isolation_admission() {
        return Ok(DisposableRun::Skipped(skip));
    }
    if let Err(skip) = process_isolation_admission() {
        return Ok(DisposableRun::Skipped(skip));
    }
    let provisioning = match capture_environment() {
        Ok(provisioning) => provisioning,
        Err(skip) => return Ok(DisposableRun::Skipped(skip)),
    };
    let config = provisioning.config.clone();
    let ambient_space_ids = ["ANYTYPE_TEST_SPACE_ID", "ANYTYPE_SPACE_ID"]
        .into_iter()
        .filter_map(|name| std::env::var(name).ok())
        .collect::<Vec<_>>();
    let key = guarded_sync(|| backend_key(&config))
        .or_error(&mut evidence, DisposableErrorCategory::Setup)?;
    let root =
        guarded_sync(private_state_root).or_error(&mut evidence, DisposableErrorCategory::Setup)?;
    let _lease = guarded_sync(|| BackendLease::acquire(&root, &key))
        .or_error(&mut evidence, DisposableErrorCategory::Setup)?;
    let mut state = guarded_sync(|| HarnessState::create(root, key))
        .or_error(&mut evidence, DisposableErrorCategory::Setup)?;

    if !relevant_environment_matches(&provisioning.captured) {
        guarded_finish_state(state, &mut evidence);
        return Err(DisposableTestError::cleanup(
            DisposableErrorCategory::HarnessStateCleanup,
            None,
            evidence,
        ));
    }
    let client = match guarded_sync(|| AnytypeClient::with_config(config).map_err(TestError::from))
    {
        Guarded::Value(client) => client,
        failure => {
            let primary = failure.into_primary_failure::<T>();
            guarded_finish_state(state, &mut evidence);
            return finish_outcomes(primary, evidence).map(DisposableRun::Completed);
        }
    };
    if !relevant_environment_matches(&provisioning.captured) {
        guarded_finish_state(state, &mut evidence);
        return Err(DisposableTestError::cleanup(
            DisposableErrorCategory::HarnessStateCleanup,
            None,
            evidence,
        ));
    }

    let verify = client.get_config().verify.clone().unwrap_or_default();
    let sweep_deadline = || Instant::now() + verify.timeout;
    let mut mutation_possible = false;
    let mut recovery_failed = false;
    let mut owned_id = None;
    let mut context = None;
    let mut primary: Option<std::thread::Result<TestResult<T>>> = None;

    evidence.credentials = StageOutcome::Success;
    if primary.is_none() {
        match guarded_async(async {
            client.ping_http().await.map_err(TestError::from)?;
            client.ping_grpc().await.map_err(TestError::from)?;
            Ok(())
        })
        .await
        {
            Guarded::Value(()) => {}
            failure => primary = Some(failure.into_primary_failure()),
        }
    }

    if primary.is_none() {
        mutation_possible = true;
        match guarded_async(recover_prior_ledgers(
            &client,
            &prefix,
            &state,
            provisioning.recover_stopped_run.as_deref(),
            sweep_deadline(),
        ))
        .await
        {
            Guarded::Value(()) => {}
            failure => {
                recovery_failed = true;
                primary = Some(failure.into_primary_failure());
            }
        }
    }

    if primary.is_none() {
        match guarded_async(sweep(&client, &prefix, &mut state, sweep_deadline())).await {
            Guarded::Value(()) => {}
            failure => primary = Some(failure.into_primary_failure()),
        }
    }

    let mut generated_name = None;
    if primary.is_none() {
        match guarded_sync(|| prefix.generate_name()) {
            Guarded::Value(name) => generated_name = Some(name),
            failure => primary = Some(failure.into_primary_failure()),
        }
    }
    if primary.is_none() {
        let name = generated_name.as_ref().expect("generated name").clone();
        match guarded_sync(|| state.record_create_intent(name)) {
            Guarded::Value(()) => {}
            failure => primary = Some(failure.into_primary_failure()),
        }
    }
    if primary.is_none() {
        mutation_possible = true;
        let name = generated_name.as_ref().expect("generated name");
        match guarded_async(async {
            client
                .new_space(name)
                .no_verify()
                .create()
                .await
                .map_err(classify_disposable_space_create_error)
        })
        .await
        {
            Guarded::Value(created) => {
                match validate_created_space(
                    &client.get_config().limits,
                    &prefix,
                    name,
                    &created,
                    &ambient_space_ids,
                ) {
                    Ok(()) => owned_id = Some(created.id),
                    Err(error) => primary = Some(Ok(Err(error))),
                }
            }
            failure => primary = Some(failure.into_primary_failure()),
        }
    }
    if primary.is_none() {
        let space_id = owned_id.as_ref().expect("validated owned ID");
        match guarded_async(wait_ready(&client, &prefix, space_id)).await {
            Guarded::Value(()) => {}
            failure => primary = Some(failure.into_primary_failure()),
        }
    }
    if primary.is_none() {
        let marker = state.child_marker();
        let callback_context = Arc::new(TestContext::for_disposable_space(
            client.clone(),
            owned_id.as_ref().expect("validated owned ID").clone(),
            Some(provisioning.child.clone()),
            Some(Arc::new(move || marker.mark_running())),
        ));
        context = Some(Arc::clone(&callback_context));
        primary = Some(
            match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                test_fn(callback_context)
            })) {
                Ok(future) => std::panic::AssertUnwindSafe(future).catch_unwind().await,
                Err(payload) => Err(payload),
            },
        );
    }

    let mut mark_child_stopped = false;
    if let Some(context) = &context {
        let stop = guarded_sync(|| Ok(context.seal_and_stop_owned_children()));
        match stop {
            Guarded::Value(report) => {
                evidence.panic_payloads.extend(report.panics);
                if !report.errors.is_empty() {
                    evidence.child = StageOutcome::Error;
                }
                match report.outcome {
                    ChildOwnershipOutcome::NoChildren => {}
                    ChildOwnershipOutcome::Stopped => mark_child_stopped = true,
                    ChildOwnershipOutcome::Unproven => evidence.child = StageOutcome::Error,
                }
            }
            Guarded::Error(_) => evidence.child = StageOutcome::Error,
            Guarded::Panic(payload) => {
                evidence.child = StageOutcome::Panic;
                evidence.panic_payloads.push(payload);
            }
        }
        let resources = guarded_async(context.cleanup()).await;
        match resources {
            Guarded::Value(()) if evidence.child == StageOutcome::NotRun => {
                evidence.child = StageOutcome::Success;
            }
            Guarded::Value(()) => {}
            Guarded::Error(_) => evidence.child = StageOutcome::Error,
            Guarded::Panic(payload) => {
                evidence.child = StageOutcome::Panic;
                evidence.panic_payloads.push(payload);
            }
        }
    }
    match guarded_sync(|| state.reload()) {
        Guarded::Value(()) => {}
        Guarded::Error(_) => evidence.ledger = StageOutcome::Error,
        Guarded::Panic(payload) => {
            evidence.ledger = StageOutcome::Panic;
            evidence.panic_payloads.push(payload);
        }
    }
    if mark_child_stopped {
        match guarded_sync(|| state.mark_child_stopped()) {
            Guarded::Value(()) => {}
            Guarded::Error(_) => evidence.ledger = StageOutcome::Error,
            Guarded::Panic(payload) => {
                evidence.ledger = StageOutcome::Panic;
                evidence.panic_payloads.push(payload);
            }
        }
    }

    if let Some(space_id) = &owned_id {
        let cleanup_deadline = sweep_deadline();
        let deletion = guarded_async(delete_known_space(
            &client,
            &prefix,
            space_id,
            cleanup_deadline,
        ))
        .await;
        evidence.delete = match &deletion {
            Guarded::Value(outcome) => *outcome,
            Guarded::Error(_) => StageOutcome::Error,
            Guarded::Panic(_) => StageOutcome::Panic,
        };
        deletion.retain_panic(&mut evidence);
        let absence = guarded_async(prove_absent(&client, space_id, cleanup_deadline)).await;
        evidence.absence = match &absence {
            Guarded::Value(()) => StageOutcome::Verified,
            Guarded::Error(_) => StageOutcome::Unproven,
            Guarded::Panic(_) => StageOutcome::Panic,
        };
        absence.retain_panic(&mut evidence);
    }

    let mut final_sweep_ok = !mutation_possible || recovery_failed;
    if final_sweep_is_allowed(mutation_possible, recovery_failed) {
        let final_sweep =
            guarded_async(sweep(&client, &prefix, &mut state, sweep_deadline())).await;
        final_sweep_ok = matches!(final_sweep, Guarded::Value(()));
        if owned_id.is_none() {
            evidence.absence = if final_sweep_ok {
                StageOutcome::Verified
            } else {
                StageOutcome::Unproven
            };
        }
        if let Guarded::Panic(payload) = final_sweep {
            evidence.ledger = StageOutcome::Panic;
            evidence.panic_payloads.push(payload);
        } else if !final_sweep_ok {
            evidence.ledger = StageOutcome::Error;
        }
    }

    if final_sweep_ok {
        match guarded_sync(|| state.set_phase(LedgerPhase::Cleaning)) {
            Guarded::Value(()) => guarded_finish_state(state, &mut evidence),
            Guarded::Error(_) => evidence.ledger = StageOutcome::Error,
            Guarded::Panic(payload) => {
                evidence.ledger = StageOutcome::Panic;
                evidence.panic_payloads.push(payload);
            }
        }
    }

    let primary = primary.unwrap_or_else(|| Ok(Err(config_error("callback was not run"))));
    evidence.primary = match &primary {
        Ok(Ok(_)) => StageOutcome::Success,
        Ok(Err(_)) => StageOutcome::Error,
        Err(_) => StageOutcome::Panic,
    };

    finish_outcomes(primary, evidence).map(DisposableRun::Completed)
}

fn final_sweep_is_allowed(mutation_possible: bool, recovery_failed: bool) -> bool {
    mutation_possible && !recovery_failed
}

fn finish_outcomes<T>(
    primary: std::thread::Result<TestResult<T>>,
    evidence: CleanupEvidence,
) -> Result<T, DisposableTestError> {
    let absence_failed = matches!(
        evidence.absence,
        StageOutcome::Error | StageOutcome::Panic | StageOutcome::Unproven
    );
    let harness_failed = matches!(
        evidence.credentials,
        StageOutcome::Error | StageOutcome::Panic
    ) || matches!(evidence.ledger, StageOutcome::Error | StageOutcome::Panic);
    let cleanup_failed = matches!(evidence.child, StageOutcome::Error | StageOutcome::Panic)
        || matches!(evidence.delete, StageOutcome::Error | StageOutcome::Panic);

    let dominant = if absence_failed {
        Some(DisposableErrorCategory::AbsenceUnproven)
    } else if harness_failed {
        Some(DisposableErrorCategory::HarnessStateCleanup)
    } else if cleanup_failed {
        Some(DisposableErrorCategory::CleanupDefect)
    } else {
        None
    };

    match (primary, dominant) {
        (Ok(Ok(value)), None) => Ok(value),
        (Ok(Err(source)), None) => Err(DisposableTestError::setup(source, evidence)),
        (Ok(result), Some(category)) => Err(DisposableTestError::cleanup(
            category,
            result.err(),
            evidence,
        )),
        (Err(payload), category) => {
            let mut evidence = evidence;
            evidence.panic_payloads.insert(0, payload);
            std::panic::resume_unwind(Box::new(CompositePanic {
                category: category.unwrap_or(DisposableErrorCategory::PrimaryPanic),
                evidence,
            }))
        }
    }
}

fn config_error(message: &str) -> TestError {
    TestError::Config {
        message: message.to_owned(),
    }
}

fn setup_error(stage: DisposableSetupStage, category: DisposableFailureCategory) -> TestError {
    TestError::DisposableSetup { stage, category }
}

#[cfg(unix)]
#[allow(clippy::unnecessary_wraps)]
fn platform_isolation_admission() -> Result<(), DisposableSkip> {
    Ok(())
}

#[cfg(windows)]
fn platform_isolation_admission() -> Result<(), DisposableSkip> {
    Err(DisposableSkip::PlatformIsolationUnavailable)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clean_evidence() -> CleanupEvidence {
        CleanupEvidence {
            primary: StageOutcome::Success,
            child: StageOutcome::Success,
            delete: StageOutcome::DeleteAcknowledged,
            absence: StageOutcome::Verified,
            credentials: StageOutcome::Success,
            ledger: StageOutcome::Success,
            panic_payloads: Vec::new(),
        }
    }

    fn setup_diagnostic(error: TestError) -> DisposableTestError {
        finish_outcomes::<()>(Ok(Err(error)), clean_evidence()).unwrap_err()
    }

    #[test]
    fn readiness_budget_is_exact_and_finite() {
        assert_eq!(READINESS_TIMEOUT, Duration::from_secs(20));
        assert_eq!(READINESS_MAX_ATTEMPTS, 50);
    }

    #[test]
    fn readiness_convergence_accepts_a_delayed_exact_result() {
        let pending = ReadinessFailure::new(
            DisposableReadinessStage::TypeResolve,
            DisposableFailureCategory::NotFound,
        );
        let mut convergence = ReadinessConvergence::default();

        assert_eq!(
            convergence.observe(ReadinessAttempt::Retry(pending)),
            ReadinessAttempt::Retry(pending)
        );
        assert_eq!(
            convergence.observe(ReadinessAttempt::Retry(pending)),
            ReadinessAttempt::Retry(pending)
        );
        assert_eq!(
            convergence.observe(ReadinessAttempt::Ready),
            ReadinessAttempt::Ready
        );
        assert_eq!(convergence.attempts, 3);
    }

    #[test]
    fn readiness_identity_mismatch_is_terminal_and_sanitized() {
        let mismatch = ReadinessFailure::new(
            DisposableReadinessStage::TypeDirect,
            DisposableFailureCategory::IdentityMismatch,
        );
        let mut convergence = ReadinessConvergence::default();

        assert_eq!(
            convergence.observe(ReadinessAttempt::Terminal(mismatch)),
            ReadinessAttempt::Terminal(mismatch)
        );
        assert!(matches!(
            convergence.error(),
            TestError::DisposableReadiness {
                stage: DisposableReadinessStage::TypeDirect,
                category: DisposableFailureCategory::IdentityMismatch,
                attempts: 1,
            }
        ));
    }

    #[test]
    fn readiness_timeout_replaces_transient_failure_without_unbounded_attempts() {
        let mut convergence = ReadinessConvergence::default();
        let _ = convergence.observe(ReadinessAttempt::Retry(ReadinessFailure::new(
            DisposableReadinessStage::Space,
            DisposableFailureCategory::NotFound,
        )));
        convergence.mark_timeout();

        assert!(matches!(
            convergence.error(),
            TestError::DisposableReadiness {
                stage: DisposableReadinessStage::Readiness,
                category: DisposableFailureCategory::Timeout,
                attempts: 1,
            }
        ));
    }

    #[test]
    fn readiness_api_status_table_distinguishes_transient_and_terminal_codes() {
        for code in [404, 408, 409, 425, 429, 500, 502, 503] {
            let error = AnytypeError::ApiError {
                code,
                method: "GET".to_owned(),
                url: "http://secret.invalid/v1/spaces/secret-id".to_owned(),
                message: "secret body".to_owned(),
            };
            assert!(matches!(
                classify_readiness_api_failure(DisposableReadinessStage::Space, &error),
                ReadinessAttempt::Retry(ReadinessFailure {
                    stage: DisposableReadinessStage::Space,
                    category: DisposableFailureCategory::ApiError,
                })
            ));
        }

        for code in [400, 401, 403, 405, 410, 422] {
            let error = AnytypeError::ApiError {
                code,
                method: "GET".to_owned(),
                url: "http://secret.invalid/v1/spaces/secret-id".to_owned(),
                message: "secret body".to_owned(),
            };
            assert!(matches!(
                classify_readiness_api_failure(DisposableReadinessStage::Space, &error),
                ReadinessAttempt::Terminal(ReadinessFailure {
                    stage: DisposableReadinessStage::Space,
                    category: DisposableFailureCategory::ApiError,
                })
            ));
        }
    }

    #[test]
    fn readiness_variant_table_distinguishes_transient_and_terminal_errors() {
        let transient = [
            AnytypeError::NotFound {
                obj_type: "secret type".to_owned(),
                key: "secret key".to_owned(),
            },
            AnytypeError::RateLimitExceeded {
                header: "secret header".to_owned(),
                duration: Duration::from_secs(1),
            },
            AnytypeError::TooManyRetries { n: 3 },
        ];
        for error in transient {
            assert!(matches!(
                classify_readiness_api_failure(DisposableReadinessStage::TypeResolve, &error),
                ReadinessAttempt::Retry(_)
            ));
        }

        let terminal = [
            AnytypeError::Validation {
                message: "secret validation".to_owned(),
            },
            AnytypeError::CacheDisabled,
            AnytypeError::Unauthorized,
            AnytypeError::Forbidden,
            AnytypeError::ResponseTooLarge {
                limit: 1,
                declared: Some(2),
            },
            AnytypeError::Other {
                message: "secret other".to_owned(),
            },
        ];
        for error in terminal {
            assert!(matches!(
                classify_readiness_api_failure(DisposableReadinessStage::TypeDirect, &error),
                ReadinessAttempt::Terminal(_)
            ));
        }
    }

    #[test]
    fn readiness_failure_is_reported_after_successful_cleanup() {
        let primary = TestError::DisposableReadiness {
            stage: DisposableReadinessStage::TypeResolve,
            category: DisposableFailureCategory::NotFound,
            attempts: READINESS_MAX_ATTEMPTS,
        };
        let error = finish_outcomes::<()>(Ok(Err(primary)), clean_evidence()).unwrap_err();

        assert_eq!(
            error.readiness_failure(),
            Some(("type_resolve", "not_found", READINESS_MAX_ATTEMPTS))
        );
        assert_eq!(error.evidence.child, StageOutcome::Success);
        assert_eq!(error.evidence.delete, StageOutcome::DeleteAcknowledged);
        assert_eq!(error.evidence.absence, StageOutcome::Verified);
        assert_eq!(error.evidence.ledger, StageOutcome::Success);
    }

    #[test]
    fn prefix_admission_and_case_fold_are_exact() {
        for invalid in ["", "contains space", "é", &"x".repeat(PREFIX_MAX + 1)] {
            assert!(DisposablePrefix::parse(invalid.to_owned()).is_err());
        }
        let prefix = DisposablePrefix::parse("xtest".to_owned()).unwrap();
        assert!(prefix.authorizes("xtest"));
        assert!(prefix.authorizes("XTest-old"));
        assert!(prefix.authorizes("Xtest_日本語!?"));
        assert!(!prefix.authorizes("pre-xtest"));
        assert!(!prefix.authorizes("xtes"));
    }

    #[test]
    fn generated_name_has_exact_entropy_encoding_and_boundary() {
        let prefix = DisposablePrefix::parse("x".repeat(PREFIX_MAX)).unwrap();
        let name = prefix.name_from_random([0xff; RANDOM_BYTES]);
        assert_eq!(name.chars().count(), PREFIX_MAX + GENERATED_SUFFIX_LEN);
        assert!(name.ends_with("-77777777777777777777777774"));
        assert_eq!(name.rsplit_once('-').unwrap().1.len(), RANDOM_BASE32_LEN);
    }

    #[test]
    fn backend_key_rejects_non_loopback_and_has_no_credentials() {
        let mut config = ClientConfig {
            base_url: Some("http://127.0.0.1:31012".to_owned()),
            grpc_endpoint: Some("http://127.0.0.1:31013".to_owned()),
            ..ClientConfig::default()
        };
        let key = backend_key(&config).unwrap();
        assert_eq!(key.len(), 64);
        config.base_url = Some("http://user:secret@127.0.0.1:31012/path?q=secret".to_owned());
        assert_eq!(backend_key(&config).unwrap(), key);
        config.base_url = Some("https://example.com".to_owned());
        assert!(backend_key(&config).is_err());
    }

    fn private_test_root(label: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "anytype-disposable-{label}-{}",
            random_handle("test").unwrap()
        ));
        fs::create_dir(&root).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        }
        root
    }

    fn test_space_id(mut index: usize) -> String {
        const ALPHABET: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";
        let mut suffix = [b'a'; 52];
        for byte in suffix.iter_mut().rev() {
            *byte = ALPHABET[index & 31];
            index >>= 5;
        }
        format!("bafyrei{}", String::from_utf8(suffix.to_vec()).unwrap())
    }

    fn test_space(index: usize, name: String) -> Space {
        Space {
            id: test_space_id(index),
            name,
            object: SpaceModel::Space,
            description: None,
            icon: None,
            gateway_url: None,
            network_id: None,
        }
    }

    fn test_page(
        total: usize,
        offset: u32,
        items: Vec<Space>,
    ) -> crate::paged::PaginatedResponse<Space> {
        let has_more = usize::try_from(offset)
            .unwrap()
            .saturating_add(PLAN_PAGE_LIMIT as usize)
            < total;
        crate::paged::PaginatedResponse {
            items,
            pagination: crate::paged::PaginationMeta {
                has_more,
                limit: PLAN_PAGE_LIMIT,
                offset,
                total,
            },
        }
    }

    fn test_plan(root: &Path) -> PathBuf {
        let path = root.join("test.plan");
        drop(create_private_file(&path).unwrap());
        initialize_plan_database(&path).unwrap();
        path
    }

    #[test]
    fn typed_prefix_skip_is_decided_from_admission_value_alone() {
        assert_eq!(
            DisposablePrefix::admit_environment_value(Err(std::env::VarError::NotPresent)),
            Err(DisposableSkip::PrefixNotConfigured)
        );
        assert_eq!(
            DisposablePrefix::admit_environment_value(Ok("bad prefix".to_owned())),
            Err(DisposableSkip::PrefixInvalid)
        );
        assert_eq!(
            DisposablePrefix::admit_environment_value(Ok("XTest".to_owned()))
                .unwrap()
                .0,
            "XTest"
        );
    }

    #[test]
    fn create_response_setup_diagnostics_cover_every_closed_branch() {
        let prefix = DisposablePrefix::parse("xtest".to_owned()).unwrap();
        let expected_name = "xtest-created";
        let created = test_space(7, expected_name.to_owned());
        let limits = crate::validation::ValidationLimits::default();
        assert!(validate_created_space(&limits, &prefix, expected_name, &created, &[]).is_ok());

        let mut invalid_id = created.clone();
        invalid_id.id = "secret-invalid-id".to_owned();
        let error = setup_diagnostic(
            validate_created_space(&limits, &prefix, expected_name, &invalid_id, &[]).unwrap_err(),
        );
        assert_eq!(
            error.setup_failure(),
            Some(("create_response", "invalid_id"))
        );
        assert!(!format!("{error:?}").contains("secret-invalid-id"));

        let mut wrong_model = created.clone();
        wrong_model.object = SpaceModel::Chat;
        let error = setup_diagnostic(
            validate_created_space(&limits, &prefix, expected_name, &wrong_model, &[]).unwrap_err(),
        );
        assert_eq!(
            error.setup_failure(),
            Some(("create_response", "model_mismatch"))
        );

        let mut wrong_name = created.clone();
        wrong_name.name = "secret-response-name".to_owned();
        let error = setup_diagnostic(
            validate_created_space(&limits, &prefix, expected_name, &wrong_name, &[]).unwrap_err(),
        );
        assert_eq!(
            error.setup_failure(),
            Some(("create_response", "name_mismatch"))
        );
        let rendered = format!("{error:?}");
        assert!(rendered.contains("setup_failure: Some((\"create_response\", \"name_mismatch\"))"));
        assert!(!rendered.contains("secret-response-name"));
        assert!(!rendered.contains(expected_name));

        let error = setup_diagnostic(
            validate_created_space(
                &limits,
                &prefix,
                expected_name,
                &created,
                std::slice::from_ref(&created.id),
            )
            .unwrap_err(),
        );
        assert_eq!(
            error.setup_failure(),
            Some(("create_response", "ambient_collision"))
        );
    }

    #[test]
    fn space_create_setup_diagnostics_discard_upstream_values() {
        const SECRET: &str = "secret-create-response-body";
        let rejected = setup_diagnostic(classify_disposable_space_create_error(
            AnytypeError::ApiError {
                code: 418,
                method: "POST".to_owned(),
                url: "http://secret.invalid/v1/spaces".to_owned(),
                message: SECRET.to_owned(),
            },
        ));
        assert_eq!(
            rejected.setup_failure(),
            Some(("space_create", "api_rejected"))
        );

        let indeterminate = setup_diagnostic(classify_disposable_space_create_error(
            AnytypeError::ResponseTooLarge {
                limit: 1,
                declared: Some(2),
            },
        ));
        assert_eq!(
            indeterminate.setup_failure(),
            Some(("space_create", "indeterminate"))
        );
        for rendered in [
            rejected.to_string(),
            format!("{rejected:?}"),
            indeterminate.to_string(),
            format!("{indeterminate:?}"),
        ] {
            assert!(!rendered.contains(SECRET));
            assert!(!rendered.contains("secret.invalid"));
        }
    }

    #[test]
    fn callback_diagnostics_cover_api_config_assertion_and_started_boundary() {
        const SECRET: &str = "secret-callback-value";
        let api = setup_diagnostic(disposable_callback_error(
            DisposableCallbackStage::Fixture,
            TestError::Api {
                source: AnytypeError::ApiError {
                    code: 503,
                    method: "GET".to_owned(),
                    url: "http://secret.invalid/v1/spaces/secret-id/properties".to_owned(),
                    message: SECRET.to_owned(),
                },
            },
        ));
        assert_eq!(api.callback_failure(), Some(("fixture", "api_error")));
        assert_eq!(api.setup_failure(), None);
        assert_eq!(api.readiness_failure(), None);

        let config = setup_diagnostic(disposable_callback_error(
            DisposableCallbackStage::NumberEqualInteger,
            config_error(SECRET),
        ));
        assert_eq!(
            config.callback_failure(),
            Some(("number_equal_integer", "config"))
        );

        let assertion = setup_diagnostic(disposable_callback_error(
            DisposableCallbackStage::CheckboxNotEqualFalse,
            TestError::Assertion {
                message: SECRET.to_owned(),
            },
        ));
        assert_eq!(
            assertion.callback_failure(),
            Some(("checkbox_not_equal_false", "assertion"))
        );

        let pre_callback = setup_diagnostic(setup_error(
            DisposableSetupStage::SpaceCreate,
            DisposableFailureCategory::Indeterminate,
        ));
        assert_eq!(pre_callback.callback_failure(), None);
        assert_eq!(
            pre_callback.setup_failure(),
            Some(("space_create", "indeterminate"))
        );

        for rendered in [
            api.to_string(),
            format!("{api:?}"),
            config.to_string(),
            format!("{config:?}"),
            assertion.to_string(),
            format!("{assertion:?}"),
        ] {
            assert!(!rendered.contains(SECRET));
            assert!(!rendered.contains("secret.invalid"));
            assert!(!rendered.contains("secret-id"));
        }
    }

    #[test]
    fn callback_stage_taxonomy_is_closed_and_exact() {
        let stages = [
            DisposableCallbackStage::Fixture,
            DisposableCallbackStage::NumberEqualInteger,
            DisposableCallbackStage::NumberNotEqual,
            DisposableCallbackStage::NumberLess,
            DisposableCallbackStage::NumberLessOrEqual,
            DisposableCallbackStage::NumberGreater,
            DisposableCallbackStage::NumberGreaterOrEqual,
            DisposableCallbackStage::NumberEqualDecimal,
            DisposableCallbackStage::CheckboxEqualTrue,
            DisposableCallbackStage::CheckboxEqualFalse,
            DisposableCallbackStage::CheckboxNotEqualTrue,
            DisposableCallbackStage::CheckboxNotEqualFalse,
        ];
        assert_eq!(
            stages.map(DisposableCallbackStage::as_str),
            [
                "fixture",
                "number_equal_integer",
                "number_not_equal",
                "number_less",
                "number_less_or_equal",
                "number_greater",
                "number_greater_or_equal",
                "number_equal_decimal",
                "checkbox_equal_true",
                "checkbox_equal_false",
                "checkbox_not_equal_true",
                "checkbox_not_equal_false",
            ]
        );
    }

    #[test]
    fn every_delete_read_must_still_authorize_the_current_name() {
        let prefix = DisposablePrefix::parse("xtest".to_owned()).unwrap();
        assert!(!authorized_delete_presence(&prefix, &ExactSpace::Absent).unwrap());
        assert!(
            authorized_delete_presence(
                &prefix,
                &ExactSpace::Present(test_space(1, "XTest-owned".to_owned())),
            )
            .unwrap()
        );
        assert!(
            authorized_delete_presence(
                &prefix,
                &ExactSpace::Present(test_space(1, "renamed-outside".to_owned())),
            )
            .is_err()
        );
    }

    #[test]
    fn backend_lease_is_nonblocking_and_released_by_drop() {
        let root = private_test_root("lease");
        let first = BackendLease::acquire(&root, "key").unwrap();
        assert!(BackendLease::acquire(&root, "key").is_err());
        drop(first);
        assert!(BackendLease::acquire(&root, "key").is_ok());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn ledger_registers_plan_before_creation_and_cleans_exact_handles() {
        let root = private_test_root("ledger");
        let mut state = HarnessState::create(root.clone(), "backend".to_owned()).unwrap();
        state
            .record_create_intent("xtest-aaaaaaaaaaaaaaaaaaaaaaaaaa".to_owned())
            .unwrap();
        let plan = state.allocate_plan().unwrap();
        assert!(plan.exists());
        let ledger: RunLedger =
            serde_json::from_slice(&fs::read(&state.ledger_path).unwrap()).unwrap();
        assert_eq!(ledger.version, LEDGER_VERSION);
        assert_eq!(
            ledger.plan.as_deref(),
            plan.file_name().and_then(|name| name.to_str())
        );
        assert_eq!(ledger.plan_state, PlanState::Allocated);
        state.mark_plan_complete().unwrap();
        let ledger: RunLedger =
            serde_json::from_slice(&fs::read(&state.ledger_path).unwrap()).unwrap();
        assert_eq!(ledger.plan_state, PlanState::Complete);
        state.remove_plan(&plan).unwrap();
        let ledger_path = state.ledger_path.clone();
        state.finish().unwrap();
        assert!(!ledger_path.exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn ledger_target_windows_are_crash_idempotent() {
        let root = private_test_root("ledger-windows");
        let mut state = HarnessState::create(root.clone(), "backend".to_owned()).unwrap();

        let allocated_plan = state.allocate_plan().unwrap();
        fs::remove_file(&allocated_plan).unwrap();
        state.remove_plan(&allocated_plan).unwrap();
        assert_eq!(state.ledger.plan_state, PlanState::None);

        let completed_plan = state.allocate_plan().unwrap();
        state.mark_plan_complete().unwrap();
        fs::remove_file(&completed_plan).unwrap();
        state.remove_plan(&completed_plan).unwrap();
        assert_eq!(state.ledger.plan_state, PlanState::None);

        state.finish().unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn running_child_retains_ledger() {
        let root = private_test_root("running-child");
        let mut state = HarnessState::create(root.clone(), "backend".to_owned()).unwrap();
        state.mark_child_running().unwrap();
        let ledger_path = state.ledger_path.clone();
        assert!(state.finish().is_err());
        assert!(ledger_path.exists());
        fs::remove_file(&ledger_path).unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn pre_recovery_action_ledgers_deserialize_with_no_operator_action() {
        let root = private_test_root("legacy-recovery-action");
        let state = HarnessState::create(root.clone(), "backend".to_owned()).unwrap();
        let mut encoded = serde_json::to_value(&state.ledger).unwrap();
        encoded.as_object_mut().unwrap().remove("recovery_action");
        let decoded: RunLedger = serde_json::from_value(encoded).unwrap();
        assert_eq!(decoded.recovery_action, RecoveryAction::None);
        state.finish().unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    fn offline_test_client() -> AnytypeClient {
        let mut config = ClientConfig::default().app_name("disposable-recovery-test");
        config.base_url = Some("http://127.0.0.1:9".to_owned());
        config.grpc_endpoint = Some("http://127.0.0.1:9".to_owned());
        config.keystore = Some("env".to_owned());
        AnytypeClient::with_config(config).unwrap()
    }

    async fn absent_space_server(
        response_count: usize,
    ) -> (String, tokio::task::JoinHandle<Vec<String>>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind absent-space recovery fixture");
        let address = listener
            .local_addr()
            .expect("absent-space recovery fixture address");
        let server = tokio::spawn(async move {
            let mut requests = Vec::with_capacity(response_count);
            for _ in 0..response_count {
                let (mut stream, _) = listener
                    .accept()
                    .await
                    .expect("accept absent-space request");
                let mut request = Vec::new();
                let mut buffer = [0_u8; 2048];
                loop {
                    let read = stream
                        .read(&mut buffer)
                        .await
                        .expect("read absent-space request");
                    if read == 0 {
                        break;
                    }
                    request.extend_from_slice(&buffer[..read]);
                    if request.windows(4).any(|window| window == b"\r\n\r\n") {
                        break;
                    }
                }
                let body = r#"{"message":"not found"}"#;
                let response = format!(
                    "HTTP/1.1 404 Not Found\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream
                    .write_all(response.as_bytes())
                    .await
                    .expect("write absent-space response");
                requests.push(String::from_utf8(request).expect("recovery request is UTF-8"));
            }
            requests
        });
        (format!("http://{address}"), server)
    }

    fn recovery_test_client(base_url: String) -> AnytypeClient {
        let mut config = ClientConfig::default().app_name("disposable-recovery-test");
        config.base_url = Some(base_url);
        config.grpc_endpoint = Some("http://127.0.0.1:9".to_owned());
        config.keystore = Some("env".to_owned());
        config.disable_cache = true;
        let client = AnytypeClient::with_config(config).unwrap();
        client.set_api_key(crate::keystore::HttpCredentials::new("fixture-token"));
        client
    }

    #[tokio::test]
    async fn running_child_blocks_complete_plan_until_exact_confirmation_once() {
        let root = private_test_root("running-child-recovery");
        let backend = "backend".to_owned();
        let current = HarnessState::create(root.clone(), backend.clone()).unwrap();
        let mut interrupted = HarnessState::create(root.clone(), backend).unwrap();
        interrupted
            .record_create_intent("xtest-recovery-owned".to_owned())
            .unwrap();
        let plan = interrupted.allocate_plan().unwrap();
        let space_id = test_space_id(41);
        open_plan_database(&plan)
            .unwrap()
            .execute("INSERT INTO seen(id, selected) VALUES (?1, 1)", [&space_id])
            .unwrap();
        interrupted.mark_plan_complete().unwrap();
        interrupted.mark_child_running().unwrap();
        let ledger_path = interrupted.ledger_path.clone();
        let ledger_name = ledger_path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap()
            .to_owned();
        drop(interrupted);

        let blocked_client = offline_test_client();
        let before = blocked_client.http_metrics().total_requests;
        assert!(
            recover_prior_ledgers(
                &blocked_client,
                &DisposablePrefix::parse("xtest".to_owned()).unwrap(),
                &current,
                None,
                Instant::now() + Duration::from_secs(1),
            )
            .await
            .is_err()
        );
        assert_eq!(blocked_client.http_metrics().total_requests, before);
        assert_eq!(plan_selected_count(&plan).unwrap(), 1);
        let blocked = read_run_ledger(&ledger_path).unwrap();
        assert_eq!(blocked.child_state, ChildState::Running);
        assert_eq!(
            blocked.recovery_action,
            RecoveryAction::ProveChildStoppedOrGone
        );

        assert!(
            recover_prior_ledgers(
                &blocked_client,
                &DisposablePrefix::parse("xtest".to_owned()).unwrap(),
                &current,
                None,
                Instant::now() + Duration::from_secs(1),
            )
            .await
            .is_err()
        );
        assert_eq!(blocked_client.http_metrics().total_requests, before);
        assert!(plan.exists());
        assert!(ledger_path.exists());

        let (base_url, server) = absent_space_server(2).await;
        let recovery_client = recovery_test_client(base_url);
        recover_prior_ledgers(
            &recovery_client,
            &DisposablePrefix::parse("xtest".to_owned()).unwrap(),
            &current,
            Some(&ledger_name),
            Instant::now() + Duration::from_secs(2),
        )
        .await
        .unwrap();
        let requests = server.await.unwrap();
        assert_eq!(requests.len(), 2);
        assert!(requests.iter().all(|request| {
            let request_line = request.lines().next().unwrap_or_default();
            request_line.starts_with("GET ")
                && request_line.contains(&space_id)
                && request_line.ends_with(" HTTP/1.1")
        }));
        assert!(!plan.exists());
        assert!(!ledger_path.exists());
        assert!(
            recover_prior_ledgers(
                &recovery_client,
                &DisposablePrefix::parse("xtest".to_owned()).unwrap(),
                &current,
                Some(&ledger_name),
                Instant::now() + Duration::from_secs(1),
            )
            .await
            .is_err()
        );

        current.finish().unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn failed_recovery_forbids_the_caller_final_sweep() {
        assert!(!final_sweep_is_allowed(true, true));
        assert!(final_sweep_is_allowed(true, false));
        assert!(!final_sweep_is_allowed(false, false));
    }

    #[tokio::test]
    async fn recovery_reconciles_precreate_and_postremove_crash_windows_offline() {
        let root = private_test_root("recovery-windows");
        let backend = "backend".to_owned();
        let current = HarnessState::create(root.clone(), backend.clone()).unwrap();

        let mut precreate = HarnessState::create(root.clone(), backend.clone()).unwrap();
        let precreate_plan = precreate.allocate_plan().unwrap();
        let precreate_ledger = precreate.ledger_path.clone();
        drop(precreate);

        let mut postremove = HarnessState::create(root.clone(), backend).unwrap();
        let postremove_plan = postremove.allocate_plan().unwrap();
        postremove.mark_plan_complete().unwrap();
        fs::remove_file(&postremove_plan).unwrap();
        postremove.mark_child_stopped().unwrap();
        let postremove_ledger = postremove.ledger_path.clone();
        drop(postremove);
        let orphaned_replacement =
            root.join(".backend-backend.run-aaaaaaaaaaaaaaaaaaaaaaaaaa.json.new");
        drop(create_private_file(&orphaned_replacement).unwrap());
        let other_backend_replacement =
            root.join(".backend-other.run-bbbbbbbbbbbbbbbbbbbbbbbbbb.json.new");
        drop(create_private_file(&other_backend_replacement).unwrap());

        recover_prior_ledgers(
            &offline_test_client(),
            &DisposablePrefix::parse("xtest".to_owned()).unwrap(),
            &current,
            None,
            Instant::now() + Duration::from_secs(1),
        )
        .await
        .unwrap();
        assert!(!precreate_plan.exists());
        assert!(!precreate_ledger.exists());
        assert!(!postremove_ledger.exists());
        assert!(!orphaned_replacement.exists());
        assert!(other_backend_replacement.exists());

        remove_private_file_if_present(&other_backend_replacement).unwrap();
        current.finish().unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn malformed_recovery_ledger_fails_closed_and_is_retained() {
        let root = private_test_root("malformed-ledger");
        let current = HarnessState::create(root.clone(), "backend".to_owned()).unwrap();
        let malformed = root.join("run-aaaaaaaaaaaaaaaaaaaaaaaaaa.json");
        let mut file = create_private_file(&malformed).unwrap();
        file.write_all(b"not-json").unwrap();
        file.sync_all().unwrap();
        drop(file);

        assert!(
            recover_prior_ledgers(
                &offline_test_client(),
                &DisposablePrefix::parse("xtest".to_owned()).unwrap(),
                &current,
                None,
                Instant::now() + Duration::from_secs(1),
            )
            .await
            .is_err()
        );
        assert!(malformed.exists());

        remove_private_file_if_present(&malformed).unwrap();
        current.finish().unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn corrupt_complete_plan_fails_closed_and_is_retained() {
        let root = private_test_root("corrupt-recovery-plan");
        let current = HarnessState::create(root.clone(), "backend".to_owned()).unwrap();
        let mut interrupted = HarnessState::create(root.clone(), "backend".to_owned()).unwrap();
        let plan = interrupted.allocate_plan().unwrap();
        interrupted.mark_plan_complete().unwrap();
        let ledger = interrupted.ledger_path.clone();
        drop(interrupted);
        let mut plan_file = open_private_file(&plan).unwrap();
        plan_file.set_len(0).unwrap();
        plan_file.write_all(b"not-sqlite").unwrap();
        plan_file.sync_all().unwrap();
        drop(plan_file);

        assert!(
            recover_prior_ledgers(
                &offline_test_client(),
                &DisposablePrefix::parse("xtest".to_owned()).unwrap(),
                &current,
                None,
                Instant::now() + Duration::from_secs(1),
            )
            .await
            .is_err()
        );
        assert!(plan.exists());
        assert!(ledger.exists());

        remove_private_plan_if_present(&plan).unwrap();
        remove_private_file_if_present(&ledger).unwrap();
        current.finish().unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn hard_killed_plan_allocation_is_recovered_by_the_next_leased_run() {
        const CHILD_ROOT: &str = "ANYTYPE_DISPOSABLE_HARD_KILL_ROOT";
        if let Some(root) = std::env::var_os(CHILD_ROOT) {
            let root = PathBuf::from(root);
            let mut interrupted = HarnessState::create(root.clone(), "backend".to_owned()).unwrap();
            let plan = interrupted.allocate_plan().unwrap();
            let connection = open_plan_database(&plan).unwrap();
            connection.execute_batch("BEGIN IMMEDIATE;").unwrap();
            connection
                .execute(
                    "INSERT INTO seen(id, selected) VALUES (?1, 1)",
                    [test_space_id(1)],
                )
                .unwrap();
            fs::write(root.join("child-ready"), b"ready").unwrap();
            std::thread::sleep(Duration::from_secs(30));
            return;
        }

        let root = private_test_root("hard-kill");
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .arg("--exact")
            .arg(
                "test_util::disposable::tests::hard_killed_plan_allocation_is_recovered_by_the_next_leased_run",
            )
            .arg("--nocapture")
            .env(CHILD_ROOT, &root)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let marker = root.join("child-ready");
        let wait_deadline = Instant::now() + Duration::from_secs(5);
        while !marker.exists() && Instant::now() < wait_deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(marker.exists(), "child reached crash injection boundary");
        child.kill().unwrap();
        child.wait().unwrap();
        fs::remove_file(marker).unwrap();

        let current = HarnessState::create(root.clone(), "backend".to_owned()).unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime
            .block_on(recover_prior_ledgers(
                &offline_test_client(),
                &DisposablePrefix::parse("xtest".to_owned()).unwrap(),
                &current,
                None,
                Instant::now() + Duration::from_secs(1),
            ))
            .unwrap();
        let remaining_plans = fs::read_dir(&root)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| {
                entry.file_name().to_str().is_some_and(|name| {
                    name.ends_with(".plan")
                        || name.ends_with(".plan-journal")
                        || name.ends_with(".plan-wal")
                        || name.ends_with(".plan-shm")
                })
            })
            .count();
        assert_eq!(remaining_plans, 0);
        current.finish().unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn recovery_removal_rejects_symlinks_and_broad_permissions() {
        use std::os::unix::fs::{PermissionsExt, symlink};

        let root = private_test_root("recovery-substitution");
        let outside = root
            .parent()
            .unwrap()
            .join(format!("outside-{}", random_handle("target").unwrap()));
        fs::write(&outside, b"preserve").unwrap();
        let link = root.join("substituted.plan");
        symlink(&outside, &link).unwrap();
        assert!(remove_private_file_if_present(&link).is_err());
        assert_eq!(fs::read(&outside).unwrap(), b"preserve");
        fs::remove_file(&link).unwrap();

        let file = root.join("broad.plan");
        drop(create_private_file(&file).unwrap());
        fs::set_permissions(&file, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(remove_private_file_if_present(&file).is_err());
        fs::set_permissions(&file, fs::Permissions::from_mode(0o600)).unwrap();
        remove_private_file_if_present(&file).unwrap();

        fs::remove_file(outside).unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn disk_plan_handles_more_than_one_thousand_spaces_in_fixed_windows() {
        let root = private_test_root("large-plan");
        let plan = test_plan(&root);
        let mut connection = open_plan_database(&plan).unwrap();
        let prefix = DisposablePrefix::parse("xtest".to_owned()).unwrap();
        let limits = crate::validation::ValidationLimits::default();
        let total = 1_101_usize;
        let mut stable_total = None;
        let mut offset = 0_u32;
        loop {
            let start = usize::try_from(offset).unwrap();
            let end = start.saturating_add(PLAN_PAGE_LIMIT as usize).min(total);
            let items = (start..end)
                .map(|index| test_space(index, format!("xtest-{index}")))
                .collect();
            match ingest_space_page(
                &mut connection,
                &limits,
                &prefix,
                test_page(total, offset, items),
                offset,
                &mut stable_total,
            )
            .unwrap()
            {
                PageIngest::Next(next) => {
                    assert_eq!(next, offset + PLAN_PAGE_LIMIT);
                    offset = next;
                }
                PageIngest::Complete => break,
                PageIngest::Unstable => panic!("stable inventory changed total"),
            }
        }
        drop(connection);
        assert_eq!(plan_selected_count(&plan).unwrap(), total);
        remove_private_file_if_present(&plan).unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn disk_plan_rejects_duplicates_corruption_and_unstable_totals() {
        let prefix = DisposablePrefix::parse("xtest".to_owned()).unwrap();
        let limits = crate::validation::ValidationLimits::default();

        let root = private_test_root("duplicate-plan");
        let plan = test_plan(&root);
        let mut connection = open_plan_database(&plan).unwrap();
        let duplicate = test_space(0, "xtest-duplicate".to_owned());
        let response = test_page(2, 0, vec![duplicate.clone(), duplicate]);
        assert!(
            ingest_space_page(&mut connection, &limits, &prefix, response, 0, &mut None,).is_err()
        );
        drop(connection);
        remove_private_file_if_present(&plan).unwrap();
        fs::remove_dir_all(root).unwrap();

        for mutation in 0..4 {
            let root = private_test_root("corrupt-plan");
            let plan = test_plan(&root);
            let mut connection = open_plan_database(&plan).unwrap();
            let items = (0..100)
                .map(|index| test_space(index, format!("other-{index}")))
                .collect();
            let mut response = test_page(101, 0, items);
            match mutation {
                0 => response.pagination.offset = 1,
                1 => response.pagination.limit = 99,
                2 => response.pagination.has_more = false,
                3 => {
                    response.items.pop();
                }
                _ => unreachable!(),
            }
            assert!(
                ingest_space_page(&mut connection, &limits, &prefix, response, 0, &mut None,)
                    .is_err()
            );
            drop(connection);
            remove_private_file_if_present(&plan).unwrap();
            fs::remove_dir_all(root).unwrap();
        }

        let root = private_test_root("unstable-plan");
        let plan = test_plan(&root);
        let mut connection = open_plan_database(&plan).unwrap();
        let items = (100..200)
            .map(|index| test_space(index, format!("other-{index}")))
            .collect();
        let mut stable_total = Some(200);
        assert_eq!(
            ingest_space_page(
                &mut connection,
                &limits,
                &prefix,
                test_page(201, 100, items),
                100,
                &mut stable_total,
            )
            .unwrap(),
            PageIngest::Unstable
        );
        drop(connection);
        remove_private_file_if_present(&plan).unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn deadline_and_guard_seams_are_total() {
        assert!(check_deadline(Instant::now()).is_err());
        assert!(
            with_deadline::<()>(Instant::now(), async { Ok(()) })
                .await
                .is_err()
        );
        assert!(matches!(
            guarded_sync::<()>(|| panic!("sync-stage")),
            Guarded::Panic(_)
        ));
        assert!(matches!(
            guarded_async::<()>(async { panic!("async-stage") }).await,
            Guarded::Panic(_)
        ));
    }

    #[test]
    fn service_and_total_environment_budget_are_bounded() {
        assert!(valid_service("anyr.test-1"));
        for invalid in ["", ".leading", "has space", "line\nbreak", &"x".repeat(129)] {
            assert!(!valid_service(invalid));
        }
        let small = vec![("ANYTYPE_KEYSTORE".to_owned(), "env".to_owned())];
        #[cfg(unix)]
        assert!(validate_child_block(&small, "any-mcp", &[], Some(32_768)).is_ok());
        let huge = vec![(
            "ANYTYPE_KEY_HTTP_TOKEN".to_owned(),
            "x".repeat(CHILD_ENV_LIMIT),
        )];
        assert!(validate_child_block(&huge, "any-mcp", &[], Some(1 << 20)).is_err());
    }

    #[test]
    fn total_precedence_never_hides_cleanup_defects() {
        let clean = clean_evidence();
        assert_eq!(finish_outcomes(Ok(Ok(7_u8)), clean).unwrap(), 7);

        let setup =
            finish_outcomes::<()>(Ok(Err(config_error("primary"))), CleanupEvidence::default())
                .unwrap_err();
        assert_eq!(setup.category(), "disposable test setup failed");

        let absence = CleanupEvidence {
            primary: StageOutcome::Error,
            child: StageOutcome::Error,
            delete: StageOutcome::Error,
            absence: StageOutcome::Unproven,
            credentials: StageOutcome::Error,
            ledger: StageOutcome::Error,
            panic_payloads: Vec::new(),
        };
        let error = finish_outcomes::<()>(Ok(Err(config_error("primary"))), absence).unwrap_err();
        assert_eq!(error.category(), "disposable test space absence unproven");

        let harness = CleanupEvidence {
            primary: StageOutcome::Success,
            child: StageOutcome::Error,
            delete: StageOutcome::Error,
            absence: StageOutcome::Verified,
            credentials: StageOutcome::Error,
            ledger: StageOutcome::Success,
            panic_payloads: Vec::new(),
        };
        let error = finish_outcomes(Ok(Ok(())), harness).unwrap_err();
        assert_eq!(
            error.category(),
            "disposable test harness state cleanup failed"
        );

        let mut child = clean_evidence();
        child.child = StageOutcome::Error;
        let error = finish_outcomes(Ok(Ok(())), child).unwrap_err();
        assert_eq!(error.category(), "disposable test cleanup defect");

        let mut delete = clean_evidence();
        delete.delete = StageOutcome::Panic;
        let error = finish_outcomes(Ok(Ok(())), delete).unwrap_err();
        assert_eq!(error.category(), "disposable test cleanup defect");

        let mut ledger = clean_evidence();
        ledger.ledger = StageOutcome::Panic;
        let error = finish_outcomes(Ok(Ok(())), ledger).unwrap_err();
        assert_eq!(
            error.category(),
            "disposable test harness state cleanup failed"
        );
    }

    #[test]
    fn dominant_cleanup_categories_retain_typed_source_and_all_evidence() {
        let absence = CleanupEvidence {
            primary: StageOutcome::Error,
            child: StageOutcome::Panic,
            delete: StageOutcome::DeleteIndeterminate,
            absence: StageOutcome::Unproven,
            credentials: StageOutcome::Error,
            ledger: StageOutcome::Panic,
            panic_payloads: vec![Box::new("cleanup-panic")],
        };
        let error = finish_outcomes::<()>(
            Ok(Err(TestError::Assertion {
                message: "typed-primary".to_owned(),
            })),
            absence,
        )
        .unwrap_err();
        assert_eq!(error.category, DisposableErrorCategory::AbsenceUnproven);
        assert!(matches!(
            error.source.as_deref(),
            Some(TestError::Assertion { message }) if message == "typed-primary"
        ));
        assert!(std::error::Error::source(&error).is_none());
        assert_eq!(error.evidence.primary, StageOutcome::Error);
        assert_eq!(error.evidence.child, StageOutcome::Panic);
        assert_eq!(error.evidence.delete, StageOutcome::DeleteIndeterminate);
        assert_eq!(error.evidence.absence, StageOutcome::Unproven);
        assert_eq!(error.evidence.credentials, StageOutcome::Error);
        assert_eq!(error.evidence.ledger, StageOutcome::Panic);
        assert_eq!(error.evidence.panic_payloads.len(), 1);

        let harness = CleanupEvidence {
            primary: StageOutcome::Error,
            child: StageOutcome::Error,
            delete: StageOutcome::Panic,
            absence: StageOutcome::Verified,
            credentials: StageOutcome::Success,
            ledger: StageOutcome::Error,
            panic_payloads: Vec::new(),
        };
        let error = finish_outcomes::<()>(
            Ok(Err(TestError::Config {
                message: "earlier-primary".to_owned(),
            })),
            harness,
        )
        .unwrap_err();
        assert_eq!(error.category, DisposableErrorCategory::HarnessStateCleanup);
        assert!(matches!(
            error.source.as_deref(),
            Some(TestError::Config { message }) if message == "earlier-primary"
        ));
        assert_eq!(error.evidence.child, StageOutcome::Error);
        assert_eq!(error.evidence.delete, StageOutcome::Panic);
        assert_eq!(error.evidence.absence, StageOutcome::Verified);
        assert_eq!(error.evidence.ledger, StageOutcome::Error);
    }

    #[test]
    fn cleanup_precedence_retains_closed_setup_diagnostic() {
        let mut absence = clean_evidence();
        absence.absence = StageOutcome::Unproven;
        let error = finish_outcomes::<()>(
            Ok(Err(setup_error(
                DisposableSetupStage::CreateResponse,
                DisposableFailureCategory::NameMismatch,
            ))),
            absence,
        )
        .unwrap_err();

        assert_eq!(error.category, DisposableErrorCategory::AbsenceUnproven);
        assert_eq!(
            error.setup_failure(),
            Some(("create_response", "name_mismatch"))
        );
        assert_eq!(error.readiness_failure(), None);
        assert_eq!(error.evidence.absence, StageOutcome::Unproven);
    }

    #[test]
    fn cleanup_precedence_retains_closed_callback_diagnostic() {
        let mut harness = clean_evidence();
        harness.ledger = StageOutcome::Error;
        let error = finish_outcomes::<()>(
            Ok(Err(disposable_callback_error(
                DisposableCallbackStage::NumberLess,
                TestError::Assertion {
                    message: "secret assertion detail".to_owned(),
                },
            ))),
            harness,
        )
        .unwrap_err();

        assert_eq!(error.category, DisposableErrorCategory::HarnessStateCleanup);
        assert_eq!(error.callback_failure(), Some(("number_less", "assertion")));
        assert_eq!(error.evidence.ledger, StageOutcome::Error);
        assert!(!format!("{error:?}").contains("secret assertion detail"));
    }

    #[test]
    fn panic_is_resumed_or_composed_after_cleanup() {
        let clean = CleanupEvidence {
            primary: StageOutcome::Panic,
            child: StageOutcome::Success,
            delete: StageOutcome::DeleteAcknowledged,
            absence: StageOutcome::Verified,
            credentials: StageOutcome::Success,
            ledger: StageOutcome::Success,
            panic_payloads: Vec::new(),
        };
        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = finish_outcomes::<()>(Err(Box::new("original")), clean);
        }))
        .unwrap_err();
        let composite = panic.downcast_ref::<CompositePanic>().unwrap();
        assert_eq!(composite.category, DisposableErrorCategory::PrimaryPanic);
        assert_eq!(composite.evidence.panic_payloads.len(), 1);

        let unproven = CleanupEvidence {
            primary: StageOutcome::Panic,
            child: StageOutcome::Panic,
            delete: StageOutcome::Panic,
            absence: StageOutcome::Unproven,
            credentials: StageOutcome::Success,
            ledger: StageOutcome::Success,
            panic_payloads: Vec::new(),
        };
        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = finish_outcomes::<()>(Err(Box::new("original")), unproven);
        }))
        .unwrap_err();
        let composite = panic.downcast_ref::<CompositePanic>().unwrap();
        assert_eq!(composite.category, DisposableErrorCategory::AbsenceUnproven);
        assert_eq!(composite.evidence.panic_payloads.len(), 1);
    }
}
