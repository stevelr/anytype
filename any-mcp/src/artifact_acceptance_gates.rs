// any-mcp - bounded, workflow-oriented MCP server for Anytype
//
// SPDX-FileCopyrightText: 2026 Steve Schoettler
// SPDX-License-Identifier: Apache-2.0

//! Private, runtime-owned synchronization points for artifact acceptance tests.

use std::{
    collections::HashMap,
    env, io,
    path::{Path, PathBuf},
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    task::{Context, Poll},
    time::Duration,
};

use cap_std::fs::{Dir, OpenOptions};
use sha2::{Digest, Sha256};
use tokio::sync::{Mutex, watch};
use tokio::{
    io::{AsyncRead, ReadBuf},
    task::JoinHandle,
};

/// An exact artifact operation point exposed only to the acceptance harness.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub enum ArtifactAcceptanceGatePoint {
    /// The import reservation is about to dispatch its first upload request.
    ImportBeforeDispatch,
    /// At least one upload chunk has been consumed by the import request.
    ImportFirstUploadChunk,
    /// The final export namespace check succeeded and publication is next.
    ExportPrepublication,
    /// The export destination is linked but its publication is not settled.
    ExportAtomicPublication,
    /// The import upload was dispatched and a candidate object is recorded.
    ImportPostDispatch,
    /// A document import is about to perform its final source check.
    DocumentFinalRevalidation,
    /// The document mutation was dispatched and its candidate is recorded.
    DocumentPostDispatch,
}

/// Capability-directory environment variable used only by the private child.
pub const ACCEPTANCE_GATE_DIRECTORY_ENV: &str = "ANY_MCP_ACCEPTANCE_GATE_DIR";
/// Bounded structured gate configuration used only by the private child.
pub const ACCEPTANCE_GATE_ENV: &str = "ANY_MCP_ACCEPTANCE_GATE";
const GATE_CONFIG_VERSION: &str = "v1";
const MAX_GATE_CONFIG_BYTES: usize = 1024;
const MAX_IDEMPOTENCY_KEY_CHARS: usize = 256;
const NONCE_HEX_BYTES: usize = 64;
const MARKER_CONTENT_BYTES: usize = NONCE_HEX_BYTES + 1;
const GATE_DEADLINE: Duration = Duration::from_secs(30);
const GATE_POLL_INTERVAL: Duration = Duration::from_millis(25);

/// A redacted error from private acceptance-child setup.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AcceptanceGateSetupError {
    /// The paired environment configuration was missing or malformed.
    Configuration,
    /// The supplied directory was not a safe capability directory.
    Directory,
    /// A marker was stale, invalid, or could not be safely accessed.
    Marker,
    /// The requested point could not be armed in this runtime.
    Arm,
}

impl std::fmt::Display for AcceptanceGateSetupError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Configuration => "acceptance gate configuration rejected",
            Self::Directory => "acceptance gate capability directory rejected",
            Self::Marker => "acceptance gate marker rejected",
            Self::Arm => "acceptance gate could not be armed",
        })
    }
}

impl std::error::Error for AcceptanceGateSetupError {}

#[derive(Clone, Debug)]
struct AcceptanceGateConfig {
    point: ArtifactAcceptanceGatePoint,
    raw_key: String,
    nonce: String,
    directory: PathBuf,
}

/// Retains the owned coordinator task for a private child gate.
///
/// Dropping a join handle deliberately detaches this bounded task: the lease
/// remains owned until it releases or its deadline elapses.
#[derive(Debug)]
pub(crate) struct AcceptanceGateCoordinator {
    _task: JoinHandle<()>,
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
struct GateKey {
    point: ArtifactAcceptanceGatePoint,
    operation: [u8; 32],
}

#[derive(Debug)]
struct ArmedGate {
    entered: watch::Sender<bool>,
    released: watch::Receiver<bool>,
    failed: Arc<AtomicBool>,
}

/// One runtime's opt-in acceptance synchronization state.
///
/// Production construction leaves this disabled. Each arm is consumed once,
/// is scoped to the supplied operation digest, and has a bounded wait.
#[derive(Clone, Debug, Default)]
pub struct ArtifactAcceptanceGates {
    enabled: bool,
    arms: Arc<Mutex<HashMap<GateKey, ArmedGate>>>,
}

/// A test-side lease for one armed acceptance point.
#[derive(Clone, Debug)]
pub struct ArtifactAcceptanceGateLease {
    entered: watch::Receiver<bool>,
    released: watch::Sender<bool>,
    failed: Arc<AtomicBool>,
}

/// Describes why an acceptance point could not be armed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArtifactAcceptanceGateError {
    /// The runtime was constructed for ordinary production use.
    Disabled,
    /// The exact point and operation are already armed.
    AlreadyArmed,
}

impl ArtifactAcceptanceGates {
    /// Arms an import at the last point before reservation and dispatch.
    pub async fn arm_import_before_dispatch(
        &self,
        idempotency_key: &str,
    ) -> Result<ArtifactAcceptanceGateLease, ArtifactAcceptanceGateError> {
        self.arm(
            ArtifactAcceptanceGatePoint::ImportBeforeDispatch,
            operation_key(b"import", idempotency_key),
        )
        .await
    }
    /// Arms the exact import operation selected by its caller idempotency key.
    pub async fn arm_file_import(
        &self,
        idempotency_key: &str,
    ) -> Result<ArtifactAcceptanceGateLease, ArtifactAcceptanceGateError> {
        self.arm(
            ArtifactAcceptanceGatePoint::ImportFirstUploadChunk,
            operation_key(b"import", idempotency_key),
        )
        .await
    }

    /// Arms the exact file-export operation selected by its caller key.
    pub async fn arm_file_export(
        &self,
        idempotency_key: &str,
    ) -> Result<ArtifactAcceptanceGateLease, ArtifactAcceptanceGateError> {
        self.arm(
            ArtifactAcceptanceGatePoint::ExportPrepublication,
            operation_key(b"export", idempotency_key),
        )
        .await
    }

    /// Arms the atomic publication window for one file-export operation.
    pub async fn arm_file_export_atomic_publication(
        &self,
        idempotency_key: &str,
    ) -> Result<ArtifactAcceptanceGateLease, ArtifactAcceptanceGateError> {
        self.arm(
            ArtifactAcceptanceGatePoint::ExportAtomicPublication,
            operation_key(b"export", idempotency_key),
        )
        .await
    }

    /// Arms a file import after dispatch has returned and before settlement.
    pub async fn arm_file_import_post_dispatch(
        &self,
        idempotency_key: &str,
    ) -> Result<ArtifactAcceptanceGateLease, ArtifactAcceptanceGateError> {
        self.arm(
            ArtifactAcceptanceGatePoint::ImportPostDispatch,
            operation_key(b"import", idempotency_key),
        )
        .await
    }

    /// Arms the exact final document-source revalidation selected by its key.
    pub async fn arm_document_import(
        &self,
        idempotency_key: &str,
    ) -> Result<ArtifactAcceptanceGateLease, ArtifactAcceptanceGateError> {
        self.arm(
            ArtifactAcceptanceGatePoint::DocumentFinalRevalidation,
            operation_key(b"document", idempotency_key),
        )
        .await
    }

    /// Arms a document update after dispatch has returned and before settlement.
    pub async fn arm_document_update_post_dispatch(
        &self,
        idempotency_key: &str,
    ) -> Result<ArtifactAcceptanceGateLease, ArtifactAcceptanceGateError> {
        self.arm(
            ArtifactAcceptanceGatePoint::DocumentPostDispatch,
            operation_key(b"document", idempotency_key),
        )
        .await
    }
    /// Creates a gate-free runtime facility.
    #[must_use]
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            arms: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Creates an acceptance-only facility that permits in-process arming.
    #[must_use]
    pub fn enabled() -> Self {
        Self {
            enabled: true,
            arms: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Arms one exact point for one operation digest.
    pub async fn arm(
        &self,
        point: ArtifactAcceptanceGatePoint,
        operation: [u8; 32],
    ) -> Result<ArtifactAcceptanceGateLease, ArtifactAcceptanceGateError> {
        if !self.enabled {
            return Err(ArtifactAcceptanceGateError::Disabled);
        }
        let key = GateKey { point, operation };
        let (entered, entered_receiver) = watch::channel(false);
        let (released, released_receiver) = watch::channel(false);
        let failed = Arc::new(AtomicBool::new(false));
        let gate = ArmedGate {
            entered,
            released: released_receiver,
            failed: failed.clone(),
        };
        let lease = ArtifactAcceptanceGateLease {
            entered: entered_receiver,
            released,
            failed,
        };
        let mut arms = self.arms.lock().await;
        if let std::collections::hash_map::Entry::Vacant(slot) = arms.entry(key) {
            slot.insert(gate);
            Ok(lease)
        } else {
            Err(ArtifactAcceptanceGateError::AlreadyArmed)
        }
    }

    /// Pauses a matching operation once, with a bounded fail-closed wait.
    pub(crate) async fn reach(
        &self,
        point: ArtifactAcceptanceGatePoint,
        operation: [u8; 32],
    ) -> bool {
        if !self.enabled {
            return true;
        }
        let gate = self.arms.lock().await.remove(&GateKey { point, operation });
        let Some(gate) = gate else {
            return true;
        };
        let _ = gate.entered.send(true);
        let failed = gate.failed.clone();
        let mut released = gate.released;
        if *released.borrow() {
            return !failed.load(Ordering::Acquire);
        }
        tokio::time::timeout(Duration::from_secs(30), released.changed())
            .await
            .is_ok_and(|result| {
                result.is_ok() && *released.borrow() && !failed.load(Ordering::Acquire)
            })
    }
}

impl AcceptanceGateConfig {
    fn from_environment() -> Result<Option<Self>, AcceptanceGateSetupError> {
        let directory = env::var_os(ACCEPTANCE_GATE_DIRECTORY_ENV);
        let encoded = env::var_os(ACCEPTANCE_GATE_ENV);
        let (directory, encoded) = match (directory, encoded) {
            (None, None) => return Ok(None),
            (Some(directory), Some(encoded)) => (directory, encoded),
            _ => return Err(AcceptanceGateSetupError::Configuration),
        };
        let directory = PathBuf::from(directory);
        if !directory.is_absolute() {
            return Err(AcceptanceGateSetupError::Directory);
        }
        let encoded = encoded
            .into_string()
            .map_err(|_| AcceptanceGateSetupError::Configuration)?;
        if encoded.len() > MAX_GATE_CONFIG_BYTES {
            return Err(AcceptanceGateSetupError::Configuration);
        }
        let mut fields = encoded.split('|');
        let (Some(version), Some(point), Some(raw_key), Some(nonce)) =
            (fields.next(), fields.next(), fields.next(), fields.next())
        else {
            return Err(AcceptanceGateSetupError::Configuration);
        };
        if fields.next().is_some() || version != GATE_CONFIG_VERSION {
            return Err(AcceptanceGateSetupError::Configuration);
        }
        let point = match point {
            "import-first-upload-chunk" => ArtifactAcceptanceGatePoint::ImportFirstUploadChunk,
            "export-prepublication" => ArtifactAcceptanceGatePoint::ExportPrepublication,
            "export-atomic-publication" => ArtifactAcceptanceGatePoint::ExportAtomicPublication,
            "import-post-dispatch" => ArtifactAcceptanceGatePoint::ImportPostDispatch,
            "document-final-revalidation" => ArtifactAcceptanceGatePoint::DocumentFinalRevalidation,
            "document-post-dispatch" => ArtifactAcceptanceGatePoint::DocumentPostDispatch,
            "import-before-dispatch" => ArtifactAcceptanceGatePoint::ImportBeforeDispatch,
            _ => return Err(AcceptanceGateSetupError::Configuration),
        };
        if raw_key.is_empty()
            || raw_key.chars().count() > MAX_IDEMPOTENCY_KEY_CHARS
            || raw_key.contains('|')
            || raw_key.bytes().any(|byte| byte.is_ascii_control())
            || nonce.len() != NONCE_HEX_BYTES
            || !nonce.bytes().all(lowercase_hex)
        {
            return Err(AcceptanceGateSetupError::Configuration);
        }
        Ok(Some(Self {
            point,
            raw_key: raw_key.to_owned(),
            nonce: nonce.to_owned(),
            directory,
        }))
    }

    async fn arm(
        &self,
        gates: &ArtifactAcceptanceGates,
    ) -> Result<ArtifactAcceptanceGateLease, AcceptanceGateSetupError> {
        let operation = match self.point {
            ArtifactAcceptanceGatePoint::ImportFirstUploadChunk
            | ArtifactAcceptanceGatePoint::ImportBeforeDispatch
            | ArtifactAcceptanceGatePoint::ImportPostDispatch => {
                operation_key(b"import", &self.raw_key)
            }
            ArtifactAcceptanceGatePoint::ExportPrepublication
            | ArtifactAcceptanceGatePoint::ExportAtomicPublication => {
                operation_key(b"export", &self.raw_key)
            }
            ArtifactAcceptanceGatePoint::DocumentFinalRevalidation
            | ArtifactAcceptanceGatePoint::DocumentPostDispatch => {
                operation_key(b"document", &self.raw_key)
            }
        };
        gates
            .arm(self.point, operation)
            .await
            .map_err(|_| AcceptanceGateSetupError::Arm)
    }
}

/// Reads and arms a private child-process gate. Ordinary entrypoints never
/// call this function, so feature unification cannot activate environment I/O.
pub(crate) async fn configure_acceptance_gate_from_environment(
    gates: &ArtifactAcceptanceGates,
) -> Result<Option<AcceptanceGateCoordinator>, AcceptanceGateSetupError> {
    let Some(config) = AcceptanceGateConfig::from_environment()? else {
        return Ok(None);
    };
    let directory = GateDirectory::open(&config.directory, &config.nonce)?;
    let lease = config.arm(gates).await?;
    let task = tokio::spawn(async move {
        coordinate_gate(directory, lease).await;
    });
    Ok(Some(AcceptanceGateCoordinator { _task: task }))
}

#[derive(Debug)]
struct GateDirectory {
    directory: Dir,
    ready: String,
    release: String,
    done: String,
    nonce: String,
}

impl GateDirectory {
    fn open(path: &Path, nonce: &str) -> Result<Self, AcceptanceGateSetupError> {
        let metadata =
            std::fs::symlink_metadata(path).map_err(|_| AcceptanceGateSetupError::Directory)?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(AcceptanceGateSetupError::Directory);
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            if metadata.permissions().mode() & 0o077 != 0 {
                return Err(AcceptanceGateSetupError::Directory);
            }
        }
        let mut options = std::fs::OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW);
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt as _;
            options.custom_flags(
                windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT,
            );
        }
        let file = options
            .open(path)
            .map_err(|_| AcceptanceGateSetupError::Directory)?;
        let directory = Dir::from_std_file(file);
        let ready = format!("ready-{nonce}");
        let release = format!("release-{nonce}");
        let done = format!("done-{nonce}");
        for marker in [&ready, &release, &done] {
            match directory.symlink_metadata(marker) {
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                _ => return Err(AcceptanceGateSetupError::Marker),
            }
        }
        Ok(Self {
            directory,
            ready,
            release,
            done,
            nonce: nonce.to_owned(),
        })
    }

    fn create_marker(&self, marker: &str) -> Result<(), AcceptanceGateSetupError> {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use cap_std::fs::OpenOptionsExt as _;
            options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
        }
        #[cfg(windows)]
        {
            use cap_std::fs::OpenOptionsExt as _;
            options.custom_flags(
                windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT,
            );
        }
        let mut file = self
            .directory
            .open_with(marker, &options)
            .map_err(|_| AcceptanceGateSetupError::Marker)?;
        use std::io::Write as _;
        file.write_all(self.nonce.as_bytes())
            .map_err(|_| AcceptanceGateSetupError::Marker)?;
        file.write_all(b"\n")
            .map_err(|_| AcceptanceGateSetupError::Marker)?;
        file.sync_all()
            .map_err(|_| AcceptanceGateSetupError::Marker)
    }

    fn release_is_valid(&self) -> Result<bool, AcceptanceGateSetupError> {
        let metadata = match self.directory.symlink_metadata(&self.release) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
            Ok(metadata) => metadata,
            Err(_) => return Err(AcceptanceGateSetupError::Marker),
        };
        if !metadata.is_file()
            || metadata.file_type().is_symlink()
            || metadata.len() != MARKER_CONTENT_BYTES as u64
        {
            return Err(AcceptanceGateSetupError::Marker);
        }
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use cap_std::fs::OpenOptionsExt as _;
            options.custom_flags(libc::O_NOFOLLOW);
        }
        #[cfg(windows)]
        {
            use cap_std::fs::OpenOptionsExt as _;
            options.custom_flags(
                windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT,
            );
        }
        let file = self
            .directory
            .open_with(&self.release, &options)
            .map_err(|_| AcceptanceGateSetupError::Marker)?;
        let mut payload = Vec::with_capacity(MARKER_CONTENT_BYTES);
        use std::io::Read as _;
        file.take((MARKER_CONTENT_BYTES + 1) as u64)
            .read_to_end(&mut payload)
            .map_err(|_| AcceptanceGateSetupError::Marker)?;
        if payload.len() != MARKER_CONTENT_BYTES
            || payload != format!("{}\n", self.nonce).as_bytes()
        {
            return Err(AcceptanceGateSetupError::Marker);
        }
        Ok(true)
    }
}

async fn coordinate_gate(directory: GateDirectory, lease: ArtifactAcceptanceGateLease) {
    let deadline = tokio::time::Instant::now() + GATE_DEADLINE;
    if !wait_for_reach(&lease, deadline).await {
        return;
    }
    if directory.create_marker(&directory.ready).is_err() {
        lease.fail_closed();
        return;
    }
    loop {
        match directory.release_is_valid() {
            Ok(true) => {
                lease.release();
                let _ = directory.create_marker(&directory.done);
                return;
            }
            Err(_) => {
                lease.fail_closed();
                return;
            }
            Ok(false) => {}
        }
        if tokio::time::Instant::now() >= deadline {
            lease.fail_closed();
            return;
        }
        tokio::time::sleep(GATE_POLL_INTERVAL).await;
    }
}

async fn wait_for_reach(
    lease: &ArtifactAcceptanceGateLease,
    deadline: tokio::time::Instant,
) -> bool {
    let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
    !remaining.is_zero() && lease.wait_until_reached(remaining).await
}

fn lowercase_hex(byte: u8) -> bool {
    byte.is_ascii_digit() || matches!(byte, b'a'..=b'f')
}

/// An upload reader that pauses only after it has yielded its first nonempty
/// chunk. The pause is intentionally between chunks, so the upstream multipart
/// body has consumed concrete source bytes before an adversarial test can act.
pub(crate) struct FirstChunkGateReader<R> {
    inner: R,
    gates: ArtifactAcceptanceGates,
    operation: [u8; 32],
    pause_before_next_read: bool,
    pause: Option<Pin<Box<dyn Future<Output = bool> + Send>>>,
}

use std::future::Future;

impl<R> FirstChunkGateReader<R> {
    /// Wraps one upload reader with the exact import gate.
    pub(crate) fn new(inner: R, gates: ArtifactAcceptanceGates, operation: [u8; 32]) -> Self {
        Self {
            inner,
            gates,
            operation,
            pause_before_next_read: false,
            pause: None,
        }
    }
}

impl<R: AsyncRead + Unpin> AsyncRead for FirstChunkGateReader<R> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        if self.pause_before_next_read {
            if self.pause.is_none() {
                let gates = self.gates.clone();
                let operation = self.operation;
                self.pause = Some(Box::pin(async move {
                    gates
                        .reach(
                            ArtifactAcceptanceGatePoint::ImportFirstUploadChunk,
                            operation,
                        )
                        .await
                }));
            }
            let Some(pause) = self.pause.as_mut() else {
                return Poll::Ready(Err(io::Error::other("artifact gate state missing")));
            };
            match pause.as_mut().poll(context) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(true) => {
                    self.pause_before_next_read = false;
                    self.pause = None;
                }
                Poll::Ready(false) => {
                    return Poll::Ready(Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "artifact acceptance gate was not released",
                    )));
                }
            }
        }
        let before = buffer.filled().len();
        match Pin::new(&mut self.inner).poll_read(context, buffer) {
            Poll::Ready(Ok(())) if buffer.filled().len() > before => {
                self.pause_before_next_read = true;
                Poll::Ready(Ok(()))
            }
            outcome => outcome,
        }
    }
}

pub(crate) fn operation_key(direction: &[u8], key: &str) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"any-mcp/artifact/idempotency/v1");
    for field in [direction, key.as_bytes()] {
        digest.update((field.len() as u64).to_be_bytes());
        digest.update(field);
    }
    digest.finalize().into()
}

impl ArtifactAcceptanceGateLease {
    /// Waits until the runtime reaches the armed point.
    pub async fn wait_until_reached(&self, timeout: Duration) -> bool {
        let mut entered = self.entered.clone();
        if *entered.borrow() {
            return true;
        }
        tokio::time::timeout(timeout, entered.changed())
            .await
            .is_ok_and(|result| result.is_ok() && *entered.borrow())
    }

    /// Lets the one paused operation continue.
    pub fn release(&self) {
        let _ = self.released.send(true);
    }

    fn fail_closed(&self) {
        self.failed.store(true, Ordering::Release);
    }
}

impl Drop for ArtifactAcceptanceGateLease {
    fn drop(&mut self) {
        let _ = self.released.send(true);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs::{self, OpenOptions as StdOpenOptions},
        io::Write as _,
        sync::atomic::{AtomicU64, Ordering},
    };

    static NEXT_TEST_DIRECTORY: AtomicU64 = AtomicU64::new(0);

    fn nonce() -> String {
        "a".repeat(NONCE_HEX_BYTES)
    }

    fn test_directory() -> PathBuf {
        let sequence = NEXT_TEST_DIRECTORY.fetch_add(1, Ordering::Relaxed);
        let path = env::temp_dir().join(format!("any-mcp-gate-{}-{sequence}", std::process::id()));
        fs::create_dir(&path).expect("create test gate directory");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700))
                .expect("make test directory private");
        }
        path
    }

    fn write_file(path: &Path, contents: &[u8]) {
        let mut file = StdOpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .expect("create test marker");
        file.write_all(contents).expect("write test marker");
    }

    #[tokio::test]
    async fn arm_is_one_shot_and_scoped_to_one_operation() {
        let gates = ArtifactAcceptanceGates::enabled();
        let operation = [7_u8; 32];
        let lease = gates
            .arm(
                ArtifactAcceptanceGatePoint::ImportFirstUploadChunk,
                operation,
            )
            .await
            .expect("arm gate");
        let reached = gates.clone();
        let task = tokio::spawn(async move {
            reached
                .reach(
                    ArtifactAcceptanceGatePoint::ImportFirstUploadChunk,
                    operation,
                )
                .await;
        });
        assert!(lease.wait_until_reached(Duration::from_secs(1)).await);
        lease.release();
        task.await.expect("gate task");
        gates
            .reach(
                ArtifactAcceptanceGatePoint::ImportFirstUploadChunk,
                operation,
            )
            .await;
    }

    #[tokio::test]
    async fn dropping_a_lease_releases_the_waiting_operation() {
        let gates = ArtifactAcceptanceGates::enabled();
        let operation = [9_u8; 32];
        let lease = gates
            .arm(ArtifactAcceptanceGatePoint::ImportBeforeDispatch, operation)
            .await
            .expect("arm gate");
        let reached = gates.clone();
        let task = tokio::spawn(async move {
            reached
                .reach(ArtifactAcceptanceGatePoint::ImportBeforeDispatch, operation)
                .await
        });
        assert!(lease.wait_until_reached(Duration::from_secs(1)).await);
        drop(lease);
        assert!(task.await.expect("gate task"));
    }

    #[test]
    #[serial_test::serial]
    fn malformed_or_unbounded_environment_is_rejected() {
        let directory = test_directory();
        // SAFETY: this serial test restores the process environment before returning.
        unsafe {
            env::set_var(ACCEPTANCE_GATE_DIRECTORY_ENV, &directory);
            env::set_var(
                ACCEPTANCE_GATE_ENV,
                "v1|unknown|key|aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            );
        }
        assert!(matches!(
            AcceptanceGateConfig::from_environment(),
            Err(AcceptanceGateSetupError::Configuration)
        ));
        // SAFETY: this serial test restores the process environment before returning.
        unsafe {
            env::set_var(
                ACCEPTANCE_GATE_ENV,
                format!("v1|import-before-dispatch|{}|{}", "k".repeat(257), nonce()),
            );
        }
        assert!(matches!(
            AcceptanceGateConfig::from_environment(),
            Err(AcceptanceGateSetupError::Configuration)
        ));
        // SAFETY: this serial test restores the process environment before returning.
        unsafe {
            env::remove_var(ACCEPTANCE_GATE_DIRECTORY_ENV);
            env::remove_var(ACCEPTANCE_GATE_ENV);
        }
        fs::remove_dir_all(directory).expect("remove test directory");
    }

    #[test]
    fn stale_and_invalid_markers_fail_closed() {
        let directory = test_directory();
        let nonce = nonce();
        write_file(&directory.join(format!("ready-{nonce}")), b"stale\n");
        assert!(matches!(
            GateDirectory::open(&directory, &nonce),
            Err(AcceptanceGateSetupError::Marker)
        ));
        fs::remove_file(directory.join(format!("ready-{nonce}"))).expect("remove stale marker");
        let capability = GateDirectory::open(&directory, &nonce).expect("open capability");
        write_file(
            &directory.join(format!("release-{nonce}")),
            format!("{}\n", "b".repeat(NONCE_HEX_BYTES)).as_bytes(),
        );
        assert_eq!(
            capability.release_is_valid(),
            Err(AcceptanceGateSetupError::Marker)
        );
        fs::remove_dir_all(directory).expect("remove test directory");
    }

    #[cfg(unix)]
    #[test]
    fn symlink_release_marker_is_rejected() {
        use std::os::unix::fs::symlink;

        let directory = test_directory();
        let nonce = nonce();
        let capability = GateDirectory::open(&directory, &nonce).expect("open capability");
        let target = directory.join("target");
        write_file(&target, b"target");
        symlink(&target, directory.join(format!("release-{nonce}"))).expect("create test symlink");
        assert_eq!(
            capability.release_is_valid(),
            Err(AcceptanceGateSetupError::Marker)
        );
        fs::remove_dir_all(directory).expect("remove test directory");
    }

    #[test]
    #[serial_test::serial]
    fn normal_process_path_does_not_read_acceptance_environment() {
        // SAFETY: this serial test restores the process environment before returning.
        unsafe {
            env::set_var(
                ACCEPTANCE_GATE_DIRECTORY_ENV,
                "/definitely/not/a/capability",
            );
            env::set_var(ACCEPTANCE_GATE_ENV, "not-valid");
        }
        assert_eq!(
            crate::run_process([std::ffi::OsString::from("--version")]),
            std::process::ExitCode::SUCCESS
        );
        // SAFETY: this serial test restores the process environment before returning.
        unsafe {
            env::remove_var(ACCEPTANCE_GATE_DIRECTORY_ENV);
            env::remove_var(ACCEPTANCE_GATE_ENV);
        }
    }
}
