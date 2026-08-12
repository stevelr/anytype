// any-mcp - bounded, workflow-oriented MCP server for Anytype
//
// SPDX-FileCopyrightText: 2026 Steve Schoettler
// SPDX-License-Identifier: Apache-2.0

//! Startup-pinned, bounded artifact validator processes.

use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::PathBuf,
    process::Stdio,
    sync::Arc,
};

use rmcp::schemars::JsonSchema;
use serde::Serialize;
use sha2::{Digest, Sha256};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::Command,
    sync::Semaphore,
};

use crate::{
    artifact_config::{ArtifactLimits, ValidatorConfig, ValidatorDriver},
    artifact_roots::PositionalReader,
    artifact_toolset::ArtifactToolError,
};

const EXECUTABLE_BYTES: u64 = 128 * 1024 * 1024;

/// Bounded public result from one startup-configured validator.
#[derive(Clone, Debug, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ValidatorFinding {
    /// Stable configured logical validator ID.
    #[schemars(length(min = 1, max = 128))]
    pub(crate) id: String,
    /// Closed completion category.
    pub(crate) status: ValidatorStatus,
    /// Bounded detected MIME essence for a successful `file-mime` driver.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(length(min = 3, max = 255))]
    pub(crate) detected_media_type: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ValidatorStatus {
    Accepted,
    Rejected,
    Unavailable,
    Skipped,
    Failed,
}

#[derive(Clone, Debug)]
struct ActivatedValidator {
    config: ValidatorConfig,
    available: bool,
    executable: Option<Arc<File>>,
}

/// Immutable validator authority activated once for one process generation.
#[derive(Clone, Debug)]
pub(crate) struct ValidatorRunner {
    validators: Arc<[ActivatedValidator]>,
    processes: Arc<Semaphore>,
    total_input_bytes: u64,
}

fn clone_rewound(source: &File) -> Result<File, ArtifactToolError> {
    let mut input = source
        .try_clone()
        .map_err(|_| ArtifactToolError::Upstream)?;
    input
        .seek(SeekFrom::Start(0))
        .map_err(|_| ArtifactToolError::Upstream)?;
    Ok(input)
}

impl ValidatorRunner {
    /// Pins configured executable identities and hashes without launching them.
    pub(crate) async fn activate(
        configs: &[ValidatorConfig],
        limits: &ArtifactLimits,
    ) -> Result<Self, ValidatorActivationError> {
        let configs = configs.to_vec();
        let validators = tokio::task::spawn_blocking(move || {
            configs
                .into_iter()
                .map(|config| {
                    let executable = pin_executable(&config)?.map(Arc::new);
                    let available = executable.is_some();
                    Ok(ActivatedValidator {
                        config,
                        available,
                        executable,
                    })
                })
                .collect::<Result<Vec<_>, ValidatorActivationError>>()
        })
        .await
        .map_err(|_| ValidatorActivationError)??;
        Ok(Self {
            validators: validators.into(),
            processes: Arc::new(Semaphore::new(limits.validator_processes)),
            total_input_bytes: limits.validator_total_input_bytes,
        })
    }

    pub(crate) fn configured_count(&self) -> usize {
        self.validators.len()
    }

    pub(crate) fn available_count(&self) -> usize {
        self.validators
            .iter()
            .filter(|validator| validator.available)
            .count()
    }

    /// Runs every configured validator whose fixed MIME scope admits this artifact.
    pub(crate) async fn validate(
        &self,
        source: &File,
        size: u64,
        declared_media_type: Option<&str>,
    ) -> Result<Vec<ValidatorFinding>, ArtifactToolError> {
        let mut admitted_bytes = 0_u64;
        let mut findings = Vec::with_capacity(self.validators.len());
        for validator in self.validators.iter() {
            if !mime_scope_matches(&validator.config.mime, declared_media_type) {
                continue;
            }
            if !validator.available {
                if validator.config.required {
                    return Err(ArtifactToolError::Validation);
                }
                findings.push(finding(
                    &validator.config,
                    ValidatorStatus::Unavailable,
                    None,
                ));
                continue;
            }
            if size > validator.config.input_bytes {
                if validator.config.required {
                    return Err(ArtifactToolError::Bounded);
                }
                findings.push(finding(&validator.config, ValidatorStatus::Skipped, None));
                continue;
            }
            admitted_bytes = admitted_bytes
                .checked_add(size)
                .ok_or(ArtifactToolError::Bounded)?;
            if admitted_bytes > self.total_input_bytes {
                return Err(ArtifactToolError::Bounded);
            }
            let input = clone_rewound(source)?;
            let permit = self
                .processes
                .clone()
                .acquire_owned()
                .await
                .map_err(|_| ArtifactToolError::Upstream)?;
            let result = run_validator(validator, input, size).await;
            drop(permit);
            match result {
                Ok(media_type) => {
                    if declared_media_type.is_some_and(|declared| declared != media_type) {
                        if validator.config.required {
                            return Err(ArtifactToolError::Validation);
                        }
                        findings.push(finding(
                            &validator.config,
                            ValidatorStatus::Rejected,
                            Some(media_type),
                        ));
                    } else {
                        findings.push(finding(
                            &validator.config,
                            ValidatorStatus::Accepted,
                            Some(media_type),
                        ));
                    }
                }
                Err(_) if validator.config.required => {
                    return Err(ArtifactToolError::Validation);
                }
                Err(_) => findings.push(finding(&validator.config, ValidatorStatus::Failed, None)),
            }
        }
        Ok(findings)
    }
}

fn finding(
    config: &ValidatorConfig,
    status: ValidatorStatus,
    detected_media_type: Option<String>,
) -> ValidatorFinding {
    ValidatorFinding {
        id: config.id.as_str().to_owned(),
        status,
        detected_media_type,
    }
}

fn mime_scope_matches(patterns: &[String], media_type: Option<&str>) -> bool {
    patterns.iter().any(|pattern| {
        if pattern == "*/*" {
            return true;
        }
        let Some(media_type) = media_type else {
            return false;
        };
        if pattern == media_type {
            return true;
        }
        pattern.strip_suffix("/*").is_some_and(|prefix| {
            media_type.starts_with(prefix) && media_type[prefix.len()..].starts_with('/')
        })
    })
}

fn pin_executable(config: &ValidatorConfig) -> Result<Option<File>, ValidatorActivationError> {
    #[cfg(not(target_os = "linux"))]
    {
        let _ = config;
        // Retained-handle execution needs a reviewed fexecve or restricted
        // Windows Job implementation before other platforms can be enabled.
        Ok(None)
    }
    #[cfg(target_os = "linux")]
    {
        let path = config.path();
        let link_metadata =
            std::fs::symlink_metadata(path).map_err(|_| ValidatorActivationError)?;
        if link_metadata.file_type().is_symlink() || !link_metadata.is_file() {
            return Err(ValidatorActivationError);
        }
        let mut file = open_executable_no_follow(path)?;
        let metadata = file.metadata().map_err(|_| ValidatorActivationError)?;
        if !safe_executable_metadata(&metadata) {
            return Err(ValidatorActivationError);
        }
        let hash = hash_reader(&mut file, EXECUTABLE_BYTES)?;
        if hash != config.sha256 {
            return Err(ValidatorActivationError);
        }
        file.seek(SeekFrom::Start(0))
            .map_err(|_| ValidatorActivationError)?;
        let mut magic = [0_u8; 4];
        let read = file
            .read(&mut magic)
            .map_err(|_| ValidatorActivationError)?;
        if !native_binary_magic(&magic[..read]) {
            return Err(ValidatorActivationError);
        }
        file.seek(SeekFrom::Start(0))
            .map_err(|_| ValidatorActivationError)?;
        Ok(Some(file))
    }
}

// Validator activation is Linux-only: execution below uses the retained
// descriptor through `/proc/self/fd` and applies `PR_SET_NO_NEW_PRIVS`.
// macOS has neither equivalent, so it must not activate this authority based
// only on the fact that `O_NOFOLLOW` is available there.
#[cfg(target_os = "linux")]
fn open_executable_no_follow(path: &std::path::Path) -> Result<File, ValidatorActivationError> {
    use std::os::unix::fs::OpenOptionsExt;

    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)
        .map_err(|_| ValidatorActivationError)
}

#[cfg(unix)]
fn safe_executable_metadata(metadata: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;

    // SAFETY: `geteuid` has no memory or ownership preconditions.
    let effective_user = unsafe { libc::geteuid() };
    let owner_is_trusted_root = metadata.uid() == 0 && effective_user != 0;
    let safe_write_mode = if owner_is_trusted_root {
        metadata.mode() & 0o022 == 0
    } else {
        metadata.mode() & 0o222 == 0
    };
    metadata.is_file()
        && (metadata.uid() == 0 || metadata.uid() == effective_user)
        && safe_write_mode
        && metadata.mode() & 0o100 != 0
}

#[cfg(not(unix))]
fn safe_executable_metadata(metadata: &std::fs::Metadata) -> bool {
    metadata.is_file()
}

#[cfg(target_os = "linux")]
fn native_binary_magic(magic: &[u8]) -> bool {
    magic.starts_with(b"\x7fELF")
}

fn hash_reader(reader: &mut File, maximum: u64) -> Result<String, ValidatorActivationError> {
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 64 * 1024];
    let mut observed = 0_u64;
    loop {
        let read = reader
            .read(&mut buffer)
            .map_err(|_| ValidatorActivationError)?;
        if read == 0 {
            break;
        }
        observed = observed
            .checked_add(read as u64)
            .ok_or(ValidatorActivationError)?;
        if observed > maximum {
            return Err(ValidatorActivationError);
        }
        hasher.update(&buffer[..read]);
    }
    let digest = hasher.finalize();
    let mut encoded = String::with_capacity(64);
    for byte in digest {
        let _ = std::fmt::Write::write_fmt(&mut encoded, format_args!("{byte:02x}"));
    }
    Ok(encoded)
}

async fn run_validator(
    validator: &ActivatedValidator,
    input: File,
    size: u64,
) -> Result<String, ValidatorExecutionError> {
    let executable = validator
        .executable
        .as_ref()
        .ok_or(ValidatorExecutionError)?;
    let mut pinned = executable
        .try_clone()
        .map_err(|_| ValidatorExecutionError)?;
    let metadata = pinned.metadata().map_err(|_| ValidatorExecutionError)?;
    if !safe_executable_metadata(&metadata) {
        return Err(ValidatorExecutionError);
    }
    pinned
        .seek(SeekFrom::Start(0))
        .map_err(|_| ValidatorExecutionError)?;
    let hash = hash_reader(&mut pinned, EXECUTABLE_BYTES).map_err(|_| ValidatorExecutionError)?;
    if hash != validator.config.sha256 {
        return Err(ValidatorExecutionError);
    }
    let executable_path = retained_executable_path(executable)?;
    let mut command = Command::new(executable_path);
    match validator.config.driver {
        ValidatorDriver::FileMime => {
            command.args(["--brief", "--mime-type", "--", "-"]);
        }
    }
    command
        .env_clear()
        .env("LANG", "C")
        .env("LC_ALL", "C")
        .env("HOME", platform_null_home())
        .current_dir(platform_safe_working_directory())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    configure_process_group(&mut command);
    configure_process_boundary(&mut command, &validator.config)?;
    let mut child = command.spawn().map_err(|_| ValidatorExecutionError)?;
    let child_id = child.id();
    let stdin = child.stdin.take().ok_or(ValidatorExecutionError)?;
    let stdout = child.stdout.take().ok_or(ValidatorExecutionError)?;
    let stderr = child.stderr.take().ok_or(ValidatorExecutionError)?;
    let stdout_limit = validator.config.stdout_bytes;
    let stderr_limit = validator.config.stderr_bytes;
    let mut process_group = ProcessGroupGuard::new(child_id);
    let (input_result, stdout, stderr, status) = tokio::join!(
        write_input(stdin, input, size),
        read_bounded(stdout, stdout_limit),
        read_bounded(stderr, stderr_limit),
        wait_for_validator(&mut child, child_id, validator.config.timeout),
    );
    // Every `wait_for_validator` branch signals the process group before it
    // reaps the leader, so once the join settles the leader pid may already
    // be recycled and must never be signalled again.
    process_group.disarm();
    input_result?;
    let stdout = stdout?;
    let stderr = stderr?;
    let status = status?;
    if !status.success() || !stderr.is_empty() {
        return Err(ValidatorExecutionError);
    }
    let media_type = parse_file_mime(stdout)?;
    if media_type.len() > validator.config.field_bytes || validator.config.fields == 0 {
        return Err(ValidatorExecutionError);
    }
    Ok(media_type)
}

#[cfg(unix)]
async fn wait_for_validator(
    child: &mut tokio::process::Child,
    child_id: Option<u32>,
    timeout: std::time::Duration,
) -> Result<std::process::ExitStatus, ValidatorExecutionError> {
    // Observe leader exit without reaping so its pid — and therefore the
    // private process-group id — stays reserved while descendants are
    // signalled. Signalling after the reap could hit a recycled pid.
    let exited = wait_for_leader_exit_without_reap(child_id, timeout).await;
    terminate_process_group(child_id);
    if exited {
        return child.wait().await.map_err(|_| ValidatorExecutionError);
    }
    let _ = child.kill().await;
    let _ = child.wait().await;
    Err(ValidatorExecutionError)
}

#[cfg(not(unix))]
async fn wait_for_validator(
    child: &mut tokio::process::Child,
    child_id: Option<u32>,
    timeout: std::time::Duration,
) -> Result<std::process::ExitStatus, ValidatorExecutionError> {
    match tokio::time::timeout(timeout, child.wait()).await {
        Ok(Ok(status)) => Ok(status),
        Ok(Err(_)) | Err(_) => {
            terminate_process_group(child_id);
            let _ = child.kill().await;
            let _ = child.wait().await;
            Err(ValidatorExecutionError)
        }
    }
}

/// Waits until the leader has exited, leaving it unreaped, or until the
/// bounded deadline elapses. Returns whether an exit was observed.
#[cfg(unix)]
async fn wait_for_leader_exit_without_reap(
    child_id: Option<u32>,
    timeout: std::time::Duration,
) -> bool {
    let Some(child_id) = child_id.and_then(|value| libc::id_t::try_from(value).ok()) else {
        return false;
    };
    let waited = tokio::task::spawn_blocking(move || {
        // SAFETY: `info` is written by the kernel before use; `WNOWAIT` keeps
        // the child waitable for the owning tokio `Child` to reap afterwards.
        unsafe {
            let mut info: libc::siginfo_t = std::mem::zeroed();
            libc::waitid(
                libc::P_PID,
                child_id,
                &raw mut info,
                libc::WEXITED | libc::WNOWAIT,
            ) == 0
        }
    });
    // On timeout the blocking wait stays parked until the subsequent group
    // and direct kills make the child exit, then ends with the detached task.
    tokio::time::timeout(timeout, waited)
        .await
        .is_ok_and(|joined| joined.unwrap_or(false))
}

struct ProcessGroupGuard {
    child_id: Option<u32>,
}

impl ProcessGroupGuard {
    fn new(child_id: Option<u32>) -> Self {
        Self { child_id }
    }

    fn disarm(&mut self) {
        self.child_id = None;
    }
}

impl Drop for ProcessGroupGuard {
    fn drop(&mut self) {
        terminate_process_group(self.child_id);
    }
}

#[cfg(target_os = "linux")]
fn retained_executable_path(executable: &File) -> Result<PathBuf, ValidatorExecutionError> {
    use std::os::fd::AsRawFd;

    let descriptor = executable.as_raw_fd();
    if descriptor < 0 {
        return Err(ValidatorExecutionError);
    }
    Ok(PathBuf::from(format!("/proc/self/fd/{descriptor}")))
}

#[cfg(not(target_os = "linux"))]
fn retained_executable_path(_: &File) -> Result<PathBuf, ValidatorExecutionError> {
    Err(ValidatorExecutionError)
}

async fn write_input(
    mut stdin: tokio::process::ChildStdin,
    input: File,
    size: u64,
) -> Result<(), ValidatorExecutionError> {
    let mut input = PositionalReader::new(input, size);
    let copied = tokio::io::copy(&mut input, &mut stdin)
        .await
        .map_err(|_| ValidatorExecutionError)?;
    if copied != size {
        return Err(ValidatorExecutionError);
    }
    stdin.shutdown().await.map_err(|_| ValidatorExecutionError)
}

async fn read_bounded<R>(reader: R, maximum: usize) -> Result<Vec<u8>, ValidatorExecutionError>
where
    R: tokio::io::AsyncRead + Unpin,
{
    let maximum = u64::try_from(maximum).map_err(|_| ValidatorExecutionError)?;
    let mut reader = reader.take(maximum.saturating_add(1));
    let mut bytes = Vec::new();
    reader
        .read_to_end(&mut bytes)
        .await
        .map_err(|_| ValidatorExecutionError)?;
    if bytes.len() as u64 > maximum {
        return Err(ValidatorExecutionError);
    }
    Ok(bytes)
}

fn parse_file_mime(bytes: Vec<u8>) -> Result<String, ValidatorExecutionError> {
    let output = std::str::from_utf8(&bytes).map_err(|_| ValidatorExecutionError)?;
    let value = output.trim_end_matches(['\r', '\n']);
    if value.len() < 3
        || value.len() > 255
        || value.contains(';')
        || value.chars().any(char::is_control)
    {
        return Err(ValidatorExecutionError);
    }
    let parsed = value
        .parse::<mime::Mime>()
        .map_err(|_| ValidatorExecutionError)?;
    let normalized = format!("{}/{}", parsed.type_(), parsed.subtype());
    if normalized != value.to_ascii_lowercase() {
        return Err(ValidatorExecutionError);
    }
    Ok(normalized)
}

fn platform_null_home() -> PathBuf {
    #[cfg(windows)]
    {
        PathBuf::from(r"C:\Windows\Temp")
    }
    #[cfg(not(windows))]
    {
        PathBuf::from("/var/empty")
    }
}

fn platform_safe_working_directory() -> PathBuf {
    #[cfg(windows)]
    {
        PathBuf::from(r"C:\Windows")
    }
    #[cfg(not(windows))]
    {
        PathBuf::from("/")
    }
}

#[cfg(unix)]
fn configure_process_group(command: &mut Command) {
    use std::os::unix::process::CommandExt;

    command.as_std_mut().process_group(0);
}

#[cfg(not(unix))]
fn configure_process_group(_: &mut Command) {}

#[cfg(target_os = "linux")]
fn configure_process_boundary(
    command: &mut Command,
    config: &ValidatorConfig,
) -> Result<(), ValidatorExecutionError> {
    use std::os::unix::process::CommandExt;

    let memory_bytes = config.memory_bytes;
    let cpu_seconds = config.timeout.as_secs().saturating_add(1);
    // SAFETY: the callback performs only async-signal-safe libc calls between
    // fork and exec, captures plain integers, and returns the OS error directly.
    unsafe {
        command.as_std_mut().pre_exec(move || {
            set_limit(libc::RLIMIT_AS, memory_bytes)?;
            set_limit(libc::RLIMIT_CORE, 0)?;
            set_limit(libc::RLIMIT_CPU, cpu_seconds)?;
            set_limit(libc::RLIMIT_FSIZE, 1024 * 1024)?;
            set_limit(libc::RLIMIT_NOFILE, 16)?;
            set_limit(libc::RLIMIT_NPROC, 1)?;
            if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    Ok(())
}

// glibc types setrlimit's resource as `__rlimit_resource_t`; musl (the
// static release target) uses a plain `c_int`.
#[cfg(all(target_os = "linux", target_env = "gnu"))]
type RlimitResource = libc::__rlimit_resource_t;
#[cfg(all(target_os = "linux", not(target_env = "gnu")))]
type RlimitResource = libc::c_int;

#[cfg(target_os = "linux")]
fn set_limit(resource: RlimitResource, value: u64) -> std::io::Result<()> {
    let limit = libc::rlimit {
        rlim_cur: value,
        rlim_max: value,
    };
    // SAFETY: `limit` is a fully initialized structure for the named resource.
    if unsafe { libc::setrlimit(resource, &limit) } == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(not(target_os = "linux"))]
fn configure_process_boundary(
    _: &mut Command,
    _: &ValidatorConfig,
) -> Result<(), ValidatorExecutionError> {
    Err(ValidatorExecutionError)
}

#[cfg(unix)]
fn terminate_process_group(child_id: Option<u32>) {
    let Some(child_id) = child_id.and_then(|value| i32::try_from(value).ok()) else {
        return;
    };
    // SAFETY: a negative PID addresses only the private process group created
    // for this child. Failure is handled by the subsequent direct child kill.
    let _ = unsafe { libc::kill(-child_id, libc::SIGKILL) };
}

#[cfg(not(unix))]
fn terminate_process_group(_: Option<u32>) {}

/// Fixed startup activation failure without paths or executable details.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ValidatorActivationError;

/// Fixed per-invocation validator failure without subprocess output.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ValidatorExecutionError;

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(target_os = "linux")]
    use crate::artifact_config::ArtifactConfig;

    #[test]
    fn mime_scope_is_closed() {
        assert!(mime_scope_matches(
            &["image/*".to_owned()],
            Some("image/png")
        ));
        assert!(!mime_scope_matches(
            &["image/*".to_owned()],
            Some("application/json")
        ));
        assert!(mime_scope_matches(&["*/*".to_owned()], None));
    }

    #[test]
    fn file_mime_parser_is_bounded_and_strict() {
        assert_eq!(
            parse_file_mime(b"image/png\n".to_vec()).expect("MIME"),
            "image/png"
        );
        assert!(parse_file_mime(b"text/plain; charset=utf-8\n".to_vec()).is_err());
        assert!(parse_file_mime(vec![b'x'; 256]).is_err());
        assert!(parse_file_mime(b"not mime\n".to_vec()).is_err());
    }

    #[test]
    fn every_validator_clone_starts_at_the_source_beginning() {
        let suffix = getrandom::u64().expect("random suffix");
        let source_path =
            std::env::temp_dir().join(format!("any-mcp-validator-rewind-{suffix:016x}"));
        std::fs::write(&source_path, b"validator source").expect("write source");
        let source = File::open(&source_path).expect("open source");

        let mut first = clone_rewound(&source).expect("first clone");
        let mut first_bytes = Vec::new();
        first
            .read_to_end(&mut first_bytes)
            .expect("read first clone");
        let mut second = clone_rewound(&source).expect("second clone");
        let mut second_bytes = Vec::new();
        second
            .read_to_end(&mut second_bytes)
            .expect("read second clone");

        assert_eq!(first_bytes, b"validator source");
        assert_eq!(second_bytes, b"validator source");
        std::fs::remove_file(source_path).expect("remove source");
    }

    /// Locates a native fixture executable via `PATH`, falling back to FHS
    /// and NixOS locations. `PATH` must come first: sandboxed builds (Nix)
    /// provide these tools only through `PATH`, never at absolute paths.
    #[cfg(target_os = "linux")]
    fn find_fixture_executable(name: &str) -> Option<PathBuf> {
        let mut candidates: Vec<PathBuf> = std::env::var_os("PATH")
            .map(|paths| {
                std::env::split_paths(&paths)
                    .map(|dir| dir.join(name))
                    .collect()
            })
            .unwrap_or_default();
        candidates.extend([
            PathBuf::from("/bin").join(name),
            PathBuf::from("/usr/bin").join(name),
            PathBuf::from("/run/current-system/sw/bin").join(name),
        ]);
        candidates
            .into_iter()
            .filter_map(|path| path.canonicalize().ok())
            .find(|target| fixture_executable_is_activatable(target))
    }

    #[cfg(target_os = "linux")]
    fn fixture_executable_is_activatable(path: &std::path::Path) -> bool {
        let Ok(mut file) = open_executable_no_follow(path) else {
            return false;
        };
        let Ok(metadata) = file.metadata() else {
            return false;
        };
        if metadata.len() > EXECUTABLE_BYTES || !safe_executable_metadata(&metadata) {
            return false;
        }
        let mut magic = [0_u8; 4];
        file.read(&mut magic)
            .ok()
            .is_some_and(|read| native_binary_magic(&magic[..read]))
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn fixture_discovery_rejects_a_user_writable_binary() {
        use std::os::unix::fs::PermissionsExt;

        let suffix = getrandom::u64().expect("random suffix");
        let path =
            std::env::temp_dir().join(format!("any-mcp-writable-validator-fixture-{suffix:016x}"));
        let native = find_fixture_executable("true").expect("platform true executable");
        std::fs::copy(native, &path).expect("copy native executable");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))
            .expect("make fixture writable");

        assert!(!fixture_executable_is_activatable(&path));

        std::fs::remove_file(path).expect("remove writable fixture");
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn pinned_executable_hash_is_rechecked_before_launch() {
        use std::{fs::OpenOptions, io::Write, os::unix::fs::PermissionsExt};

        let suffix = getrandom::u64().expect("random suffix");
        let root = std::env::temp_dir().join(format!("any-mcp-validator-{suffix:016x}"));
        std::fs::create_dir(&root).expect("temporary validator directory");
        let executable = root.join("validator");
        let fixture_executable = find_fixture_executable("true").expect("platform true executable");
        std::fs::copy(fixture_executable, &executable).expect("copy native executable");
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o500))
            .expect("freeze executable");
        let mut pinned = File::open(&executable).expect("open pinned executable");
        let sha256 = hash_reader(&mut pinned, EXECUTABLE_BYTES).expect("hash executable");
        let path = executable.to_string_lossy().replace('\\', "\\\\");
        let config = ArtifactConfig::from_toml(&format!(
            "schema_version = 1\n[spaces]\nread_only = false\n\
             [[validators]]\nid = \"mime\"\ndriver = \"file-mime\"\n\
             path = \"{path}\"\nsha256 = \"{sha256}\"\nrequired = false\n\
             mime = [\"*/*\"]\ntimeout_secs = 1\nmemory_bytes = 67108864\n\
             input_bytes = 1024\nstdout_bytes = 1024\nstderr_bytes = 1024\n\
             fields = 1\nfield_bytes = 256\nplatform = \"linux-retained-fd-v1\"\n"
        ))
        .expect("validator config");
        let runner = ValidatorRunner::activate(config.validators(), &config.limits)
            .await
            .expect("activate pinned executable");
        assert_eq!(runner.available_count(), 1);

        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700))
            .expect("permit test tamper");
        OpenOptions::new()
            .append(true)
            .open(&executable)
            .expect("open for tamper")
            .write_all(b"tamper")
            .expect("tamper executable");
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o500))
            .expect("refreeze executable");
        let source_path = root.join("source");
        std::fs::write(&source_path, b"hello").expect("write source");
        let source = File::open(source_path).expect("open source");
        let findings = runner
            .validate(&source, 5, Some("text/plain"))
            .await
            .expect("optional validator failure is bounded");
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].status, ValidatorStatus::Failed);
        drop(runner);
        std::fs::remove_dir_all(root).expect("remove validator fixture");
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn pinned_file_driver_runs_with_the_bounded_process_contract() {
        let executable = find_fixture_executable("file").expect("native file executable");
        let mut pinned = File::open(&executable).expect("open file executable");
        let sha256 = hash_reader(&mut pinned, EXECUTABLE_BYTES).expect("hash file executable");
        let path = executable.to_string_lossy().replace('\\', "\\\\");
        let config = ArtifactConfig::from_toml(&format!(
            "schema_version = 1\n[spaces]\nread_only = false\n\
             [[validators]]\nid = \"mime\"\ndriver = \"file-mime\"\n\
             path = \"{path}\"\nsha256 = \"{sha256}\"\nrequired = true\n\
             mime = [\"*/*\"]\ntimeout_secs = 5\nmemory_bytes = 268435456\n\
             input_bytes = 1024\nstdout_bytes = 1024\nstderr_bytes = 1024\n\
             fields = 1\nfield_bytes = 256\nplatform = \"linux-retained-fd-v1\"\n"
        ))
        .expect("validator config");
        let runner = ValidatorRunner::activate(config.validators(), &config.limits)
            .await
            .expect("activate file executable");
        let suffix = getrandom::u64().expect("random suffix");
        let source_path =
            std::env::temp_dir().join(format!("any-mcp-validator-source-{suffix:016x}"));
        std::fs::write(&source_path, b"hello\n").expect("write source");
        let source = File::open(&source_path).expect("open source");
        let findings = runner
            .validate(&source, 6, None)
            .await
            .expect("run required validator");
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].status, ValidatorStatus::Accepted);
        assert_eq!(
            findings[0].detected_media_type.as_deref(),
            Some("text/plain")
        );
        std::fs::remove_file(source_path).expect("remove source");
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn process_group_guard_reaps_a_descendant_after_parent_exit() {
        let shell = find_fixture_executable("sh").expect("platform shell executable");
        let mut command = Command::new(shell);
        command
            .args(["-c", "sleep 30 >/dev/null 2>&1 & echo $!"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        configure_process_group(&mut command);
        let mut child = command.spawn().expect("spawn process-group parent");
        let child_id = child.id();
        let mut stdout = child.stdout.take().expect("parent stdout");
        let mut pid_bytes = Vec::new();
        stdout
            .read_to_end(&mut pid_bytes)
            .await
            .expect("read descendant pid");
        child.wait().await.expect("wait for process-group parent");
        let descendant = std::str::from_utf8(&pid_bytes)
            .expect("descendant pid UTF-8")
            .trim()
            .parse::<i32>()
            .expect("descendant pid integer");
        let guard = ProcessGroupGuard::new(child_id);
        drop(guard);
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(1);
        loop {
            // SAFETY: signal zero performs an existence check without changing
            // the process, and `descendant` came from the owned fixture group.
            if unsafe { libc::kill(descendant, 0) } != 0 || descendant_is_zombie(descendant) {
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "process-group descendant survived guard cleanup"
            );
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }

    /// Whether the reparented descendant is dead but not yet reaped.
    ///
    /// `kill(pid, 0)` succeeds on a zombie, and under a minimal container
    /// init that never reaps orphans the SIGKILLed descendant stays a zombie
    /// indefinitely, so liveness must also consult the process state. An
    /// unreadable stat file means the process is fully gone.
    #[cfg(target_os = "linux")]
    fn descendant_is_zombie(pid: i32) -> bool {
        std::fs::read_to_string(format!("/proc/{pid}/stat"))
            .ok()
            .and_then(|stat| {
                stat.rsplit_once(") ")
                    .map(|(_, state)| state.starts_with('Z'))
            })
            .unwrap_or(true)
    }
}
