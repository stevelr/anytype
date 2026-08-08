// any-mcp - bounded, workflow-oriented MCP server for Anytype
//
// SPDX-FileCopyrightText: 2026 Steve Schoettler
// SPDX-License-Identifier: Apache-2.0

//! Opened filesystem capabilities for local artifact operations.
//!
//! Root paths are used only during activation. The registry retains opened
//! directory handles, and operations walk validated relative components
//! without following links. Absolute physical paths are never retained in
//! errors, receipts, or debug output.

use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsString,
    fmt,
    fs::File,
    future::Future,
    io::{self, Seek, SeekFrom, Write},
    path::Path,
    pin::Pin,
    sync::{Arc, OnceLock},
    task::{Context, Poll},
};

#[cfg(any(test, feature = "acceptance-harness"))]
use std::sync::atomic::{AtomicU64, Ordering};

#[cfg(unix)]
use cap_fs_ext::OpenOptionsExt;
use cap_fs_ext::{FollowSymlinks, OpenOptionsFollowExt, OpenOptionsMaybeDirExt};
use cap_std::fs::{Dir, OpenOptions};
use fs2::FileExt;
use tokio::io::{AsyncRead, ReadBuf};

#[cfg(any(test, feature = "acceptance-harness"))]
use crate::artifact_acceptance_gates::{ArtifactAcceptanceGatePoint, ArtifactAcceptanceGates};
use crate::artifact_config::{
    AbsoluteNativePath, ArtifactConfig, LogicalRootId, RelativeNativePath, RootDefinition,
};

/// Fixed guidance returned when an operation requires an undeclared root.
pub const ROOTS_REQUIRED_GUIDANCE: &str = "No artifact roots are configured. Declare roots in an any-mcp TOML config and select it with ANY_MCP_CONFIG or --config.";

/// Cursor-independent async reader over one retained file handle.
///
/// Reads use explicit offsets, so cancelling a consumer cannot leave a
/// background operation that advances another reader's shared file cursor.
pub(crate) struct PositionalReader {
    file: File,
    offset: u64,
    end: u64,
    state: PositionalReadState,
}

enum PositionalReadState {
    Idle,
    Waiting {
        permit: Pin<
            Box<
                dyn Future<
                        Output = Result<
                            tokio::sync::OwnedSemaphorePermit,
                            tokio::sync::AcquireError,
                        >,
                    > + Send,
            >,
        >,
        available: usize,
        offset: u64,
    },
    Reading(tokio::task::JoinHandle<(Vec<u8>, io::Result<usize>)>),
    Buffered {
        bytes: Vec<u8>,
        length: usize,
        consumed: usize,
    },
}

const POSITIONAL_READ_CHUNK: usize = 1024 * 1024;
#[cfg(windows)]
const POSITIONAL_IO_LIMIT: usize = 1;
#[cfg(not(windows))]
const POSITIONAL_IO_LIMIT: usize = 32;

fn positional_io_semaphore() -> Arc<tokio::sync::Semaphore> {
    static SEMAPHORE: OnceLock<Arc<tokio::sync::Semaphore>> = OnceLock::new();
    Arc::clone(SEMAPHORE.get_or_init(|| Arc::new(tokio::sync::Semaphore::new(POSITIONAL_IO_LIMIT))))
}

impl PositionalReader {
    pub(crate) fn new(file: File, length: u64) -> Self {
        Self {
            file,
            offset: 0,
            end: length,
            state: PositionalReadState::Idle,
        }
    }

    pub(crate) fn range(file: File, offset: u64, length: u64) -> io::Result<Self> {
        let end = offset
            .checked_add(length)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "invalid file range"))?;
        Ok(Self {
            file,
            offset,
            end,
            state: PositionalReadState::Idle,
        })
    }
}

impl AsyncRead for PositionalReader {
    fn poll_read(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let reader = self.get_mut();
        loop {
            match &mut reader.state {
                PositionalReadState::Idle => {
                    if reader.offset == reader.end || buffer.remaining() == 0 {
                        return Poll::Ready(Ok(()));
                    }
                    let remaining = reader.end.saturating_sub(reader.offset);
                    let available = usize::try_from(remaining)
                        .unwrap_or(usize::MAX)
                        .min(buffer.remaining())
                        .min(POSITIONAL_READ_CHUNK);
                    let offset = reader.offset;
                    let semaphore = positional_io_semaphore();
                    reader.state = PositionalReadState::Waiting {
                        permit: Box::pin(async move { semaphore.acquire_owned().await }),
                        available,
                        offset,
                    };
                }
                PositionalReadState::Waiting {
                    permit,
                    available,
                    offset,
                } => {
                    let permit = match permit.as_mut().poll(context) {
                        Poll::Pending => return Poll::Pending,
                        Poll::Ready(Ok(permit)) => permit,
                        Poll::Ready(Err(_)) => {
                            return Poll::Ready(Err(io::Error::other(
                                "positional I/O admission failed",
                            )));
                        }
                    };
                    let file = match reader.file.try_clone() {
                        Ok(file) => file,
                        Err(error) => return Poll::Ready(Err(error)),
                    };
                    let handle = match tokio::runtime::Handle::try_current() {
                        Ok(handle) => handle,
                        Err(_) => {
                            return Poll::Ready(Err(io::Error::other(
                                "positional reader requires a Tokio runtime",
                            )));
                        }
                    };
                    let available = *available;
                    let offset = *offset;
                    reader.state = PositionalReadState::Reading(handle.spawn_blocking(move || {
                        let _permit = permit;
                        let mut bytes = vec![0_u8; available];
                        let result = positional_read(&file, &mut bytes, offset);
                        (bytes, result)
                    }));
                }
                PositionalReadState::Reading(task) => {
                    let outcome = match Pin::new(task).poll(context) {
                        Poll::Pending => return Poll::Pending,
                        Poll::Ready(outcome) => outcome,
                    };
                    reader.state = PositionalReadState::Idle;
                    let (bytes, result) = match outcome {
                        Ok(outcome) => outcome,
                        Err(_) => {
                            return Poll::Ready(Err(io::Error::other(
                                "positional read task failed",
                            )));
                        }
                    };
                    let read = match result {
                        Ok(0) => {
                            return Poll::Ready(Err(io::Error::new(
                                io::ErrorKind::UnexpectedEof,
                                "retained artifact ended early",
                            )));
                        }
                        Ok(read) => read,
                        Err(error) => return Poll::Ready(Err(error)),
                    };
                    if bytes.get(..read).is_none() {
                        return Poll::Ready(Err(io::Error::other(
                            "positional read exceeded its buffer",
                        )));
                    }
                    reader.state = PositionalReadState::Buffered {
                        bytes,
                        length: read,
                        consumed: 0,
                    };
                }
                PositionalReadState::Buffered {
                    bytes,
                    length,
                    consumed,
                } => {
                    if buffer.remaining() == 0 {
                        return Poll::Ready(Ok(()));
                    }
                    let available = length.saturating_sub(*consumed).min(buffer.remaining());
                    let Some(end) = consumed.checked_add(available) else {
                        return Poll::Ready(Err(io::Error::other(
                            "positional read buffer offset overflow",
                        )));
                    };
                    let Some(contents) = bytes.get(*consumed..end) else {
                        return Poll::Ready(Err(io::Error::other(
                            "positional read exceeded its buffer",
                        )));
                    };
                    let Some(offset) = reader.offset.checked_add(available as u64) else {
                        return Poll::Ready(Err(io::Error::other(
                            "retained artifact offset overflow",
                        )));
                    };
                    reader.offset = offset;
                    buffer.put_slice(contents);
                    *consumed = end;
                    if end == *length {
                        reader.state = PositionalReadState::Idle;
                    }
                    return Poll::Ready(Ok(()));
                }
            }
        }
    }
}

#[cfg(unix)]
fn positional_read(file: &File, destination: &mut [u8], offset: u64) -> io::Result<usize> {
    std::os::unix::fs::FileExt::read_at(file, destination, offset)
}

#[cfg(windows)]
fn positional_read(file: &File, destination: &mut [u8], offset: u64) -> io::Result<usize> {
    std::os::windows::fs::FileExt::seek_read(file, destination, offset)
}

/// Filesystem authority held by one configured logical root.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RootCapabilityKind {
    /// Read an existing regular file.
    Import,
    /// Create a new regular file without replacing an existing entry.
    Export,
}

/// Activated static root capabilities.
#[derive(Clone, Default)]
pub struct RootRegistry {
    roots: Arc<BTreeMap<LogicalRootId, RootCapability>>,
    import_count: usize,
    export_count: usize,
    #[cfg(any(test, feature = "acceptance-harness"))]
    access_attempts: Arc<AtomicU64>,
    #[cfg(any(test, feature = "acceptance-harness"))]
    successful_import_opens: Arc<AtomicU64>,
}

impl fmt::Debug for RootRegistry {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RootRegistry")
            .field("import_count", &self.import_count)
            .field("export_count", &self.export_count)
            .finish()
    }
}

impl RootRegistry {
    /// Opens and validates every configured root exactly once.
    ///
    /// Call this only when the `artifacts` toolset is selected in read-write
    /// mode. Parsing a configured but inactive root performs no filesystem I/O.
    ///
    /// # Errors
    ///
    /// Returns a fixed, path-redacted failure if any root cannot provide the
    /// required owned, non-writable-by-others directory capability.
    pub fn activate(config: &ArtifactConfig) -> Result<Self, RootAccessError> {
        let mut roots = BTreeMap::new();
        for definition in config.import_roots() {
            insert_root(&mut roots, definition, RootCapabilityKind::Import)?;
        }
        for definition in config.export_roots() {
            insert_root(&mut roots, definition, RootCapabilityKind::Export)?;
        }
        Ok(Self {
            roots: Arc::new(roots),
            import_count: config.import_root_count(),
            export_count: config.export_root_count(),
            #[cfg(any(test, feature = "acceptance-harness"))]
            access_attempts: Arc::new(AtomicU64::new(0)),
            #[cfg(any(test, feature = "acceptance-harness"))]
            successful_import_opens: Arc::new(AtomicU64::new(0)),
        })
    }

    /// Returns a static-policy view with all configured roots effective.
    #[must_use]
    pub fn static_policy(&self) -> EffectiveRootRegistry {
        EffectiveRootRegistry {
            registry: self.clone(),
            client_allowed: None,
        }
    }

    /// Intersects static roots with one terminal MCP client-root snapshot.
    ///
    /// An empty snapshot denies every local root. Each client root is opened
    /// and compared by stable filesystem identity; it can narrow but never add
    /// a static capability.
    ///
    /// # Errors
    ///
    /// Returns a fixed error when a client root cannot be securely opened. The
    /// caller must freeze that disabled result for the session and must not
    /// fall back to [`Self::static_policy`].
    pub fn intersect_client_roots(
        &self,
        client_roots: &[AbsoluteNativePath],
    ) -> Result<EffectiveRootRegistry, RootAccessError> {
        if client_roots.len() > 64 {
            return Err(RootAccessError::new(RootProblem::ClientRoots));
        }
        let opened = client_roots
            .iter()
            .map(|root| open_root(root).map_err(|_| RootAccessError::new(RootProblem::ClientRoots)))
            .collect::<Result<Vec<_>, _>>()?;
        let mut allowed = BTreeSet::new();
        for (id, capability) in self.roots.iter() {
            if opened
                .iter()
                .any(|client| is_ancestor_identity(client, &capability.directory).unwrap_or(false))
            {
                allowed.insert(id.clone());
            }
        }
        Ok(EffectiveRootRegistry {
            registry: self.clone(),
            client_allowed: Some(allowed),
        })
    }

    /// Returns the count of activated import roots.
    #[must_use]
    pub const fn import_root_count(&self) -> usize {
        self.import_count
    }

    /// Returns the count of activated export roots.
    #[must_use]
    pub const fn export_root_count(&self) -> usize {
        self.export_count
    }

    /// Returns the retained-root operation count for acceptance assertions.
    ///
    /// This test-only counter advances before authorization or filesystem I/O,
    /// allowing adversarial grammar tests to prove malformed paths never
    /// reached the retained-root boundary.
    #[cfg(any(test, feature = "acceptance-harness"))]
    #[doc(hidden)]
    #[must_use]
    pub fn acceptance_access_attempts(&self) -> u64 {
        self.access_attempts.load(Ordering::Acquire)
    }

    /// Returns successful retained import opens for acceptance assertions.
    #[cfg(any(test, feature = "acceptance-harness"))]
    #[doc(hidden)]
    #[must_use]
    pub fn acceptance_successful_import_opens(&self) -> u64 {
        self.successful_import_opens.load(Ordering::Acquire)
    }
}

/// Effective root authority after optional MCP client-root narrowing.
#[derive(Clone)]
pub struct EffectiveRootRegistry {
    registry: RootRegistry,
    client_allowed: Option<BTreeSet<LogicalRootId>>,
}

impl fmt::Debug for EffectiveRootRegistry {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EffectiveRootRegistry")
            .field("static_import_count", &self.registry.import_count)
            .field("static_export_count", &self.registry.export_count)
            .field(
                "client_intersection_present",
                &self.client_allowed.is_some(),
            )
            .finish()
    }
}

impl EffectiveRootRegistry {
    /// Opens one existing import file beneath an authorized retained root.
    ///
    /// # Errors
    ///
    /// Returns fixed guidance when no import roots exist, a fixed authorization
    /// error for absent/wrong/client-excluded roots, or a fixed containment
    /// error for unsafe paths and filesystem entries.
    pub fn open_import(
        &self,
        root: &str,
        path: &RelativeNativePath,
        maximum_bytes: u64,
    ) -> Result<AnchoredImport, RootAccessError> {
        #[cfg(any(test, feature = "acceptance-harness"))]
        self.registry.access_attempts.fetch_add(1, Ordering::AcqRel);
        let capability = self.authorize(root, RootCapabilityKind::Import)?;
        let opened = open_import_at(capability, path, maximum_bytes);
        #[cfg(any(test, feature = "acceptance-harness"))]
        if opened.is_ok() {
            self.registry
                .successful_import_opens
                .fetch_add(1, Ordering::AcqRel);
        }
        opened
    }

    /// Starts one bounded, create-new atomic export.
    ///
    /// Bytes are written to an owner-private temporary file in the exact
    /// destination directory. [`AtomicExport::commit`] publishes the complete
    /// file with an atomic no-replace hard link and removes the private name.
    /// Dropping an uncommitted export removes only its private temporary.
    ///
    /// # Errors
    ///
    /// Returns fixed guidance/authorization/containment/collision errors.
    pub fn begin_atomic_export(
        &self,
        root: &str,
        path: &RelativeNativePath,
        maximum_bytes: u64,
    ) -> Result<AtomicExport, RootAccessError> {
        #[cfg(any(test, feature = "acceptance-harness"))]
        self.registry.access_attempts.fetch_add(1, Ordering::AcqRel);
        if maximum_bytes == 0 {
            return Err(RootAccessError::new(RootProblem::TooLarge));
        }
        let capability = self.authorize(root, RootCapabilityKind::Export)?;
        begin_atomic_export_at(capability, path, maximum_bytes)
    }

    fn authorize(
        &self,
        root: &str,
        kind: RootCapabilityKind,
    ) -> Result<&RootCapability, RootAccessError> {
        let count = match kind {
            RootCapabilityKind::Import => self.registry.import_count,
            RootCapabilityKind::Export => self.registry.export_count,
        };
        if count == 0 {
            return Err(RootAccessError::new(RootProblem::Missing));
        }
        let id = LogicalRootId::parse(root)
            .map_err(|_| RootAccessError::new(RootProblem::Unauthorized))?;
        let Some(capability) = self.registry.roots.get(&id) else {
            return Err(RootAccessError::new(RootProblem::Unauthorized));
        };
        if capability.kind != kind
            || self
                .client_allowed
                .as_ref()
                .is_some_and(|allowed| !allowed.contains(&id))
        {
            return Err(RootAccessError::new(RootProblem::Unauthorized));
        }
        Ok(capability)
    }
}

/// Private capability used by the supervised artifact staging service.
#[derive(Clone)]
pub(crate) struct StagingDirectory {
    records: RootCapability,
    payloads: RootCapability,
    temporary: RootCapability,
    tombstones: RootCapability,
    _instance_lock: Arc<File>,
}

/// One preflighted private file discovered during staging reconciliation.
pub(crate) struct StagingInventoryFile {
    pub(crate) name: String,
    pub(crate) source: AnchoredImport,
}

/// Complete bounded staging inventory collected before reconciliation mutates it.
pub(crate) struct StagingInventory {
    pub(crate) records: Vec<StagingInventoryFile>,
    pub(crate) payloads: Vec<StagingInventoryFile>,
    pub(crate) temporary: Vec<StagingInventoryFile>,
    pub(crate) tombstones: Vec<StagingInventoryFile>,
}

/// Stable native identity persisted beside one private staging payload.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct StagingFileIdentity {
    pub(crate) volume: u64,
    pub(crate) file: u64,
}

/// Sequential writer for one exact private staging payload.
pub(crate) struct StagingPayload {
    file: File,
    identity: FileIdentity,
    maximum_bytes: u64,
    written: u64,
}

impl fmt::Debug for StagingPayload {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StagingPayload")
            .field("maximum_bytes", &self.maximum_bytes)
            .field("written", &self.written)
            .finish_non_exhaustive()
    }
}

impl StagingPayload {
    pub(crate) fn identity(&self) -> StagingFileIdentity {
        StagingFileIdentity {
            volume: self.identity.volume,
            file: self.identity.file,
        }
    }

    pub(crate) fn sync_all(&self) -> Result<(), RootAccessError> {
        self.file
            .sync_all()
            .map_err(|_| RootAccessError::new(RootProblem::Containment))
    }

    pub(crate) fn truncate(&mut self, offset: u64) -> Result<(), RootAccessError> {
        if offset > self.written {
            return Err(RootAccessError::new(RootProblem::Changed));
        }
        self.file
            .set_len(offset)
            .and_then(|()| self.file.seek(SeekFrom::Start(offset)).map(|_| ()))
            .and_then(|()| self.file.sync_all())
            .map_err(|_| RootAccessError::new(RootProblem::Containment))?;
        self.written = offset;
        Ok(())
    }

    pub(crate) fn try_clone_reader(&self) -> io::Result<File> {
        let mut reader = self.file.try_clone()?;
        reader.seek(SeekFrom::Start(0))?;
        Ok(reader)
    }

    pub(crate) fn into_anchored(self) -> Result<AnchoredImport, RootAccessError> {
        self.sync_all()?;
        let metadata = self
            .file
            .metadata()
            .map_err(|_| RootAccessError::new(RootProblem::Changed))?;
        let snapshot = FileSnapshot::from_file(&self.file, &metadata)
            .map_err(|_| RootAccessError::new(RootProblem::Changed))?;
        if snapshot.identity != self.identity || snapshot.length != self.written {
            return Err(RootAccessError::new(RootProblem::Changed));
        }
        Ok(AnchoredImport {
            file: self.file,
            length: self.written,
            snapshot,
            namespace: None,
        })
    }
}

impl Write for StagingPayload {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let proposed = self
            .written
            .checked_add(bytes.len() as u64)
            .ok_or_else(|| io::Error::other("staging payload length overflow"))?;
        if proposed > self.maximum_bytes {
            return Err(io::Error::other("staging payload exceeds its bound"));
        }
        let written = self.file.write(bytes)?;
        self.written = self
            .written
            .checked_add(written as u64)
            .ok_or_else(|| io::Error::other("staging payload length overflow"))?;
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

impl fmt::Debug for StagingDirectory {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("StagingDirectory(<capability>)")
    }
}

impl StagingDirectory {
    /// Activates an owner-private staging root which does not overlap a local
    /// import or export root.
    pub(crate) fn activate(
        path: &AbsoluteNativePath,
        local_roots: &RootRegistry,
        maximum_entries: usize,
        maximum_payload_bytes: u64,
    ) -> Result<(Self, StagingInventory), RootAccessError> {
        let directory =
            open_root(path).map_err(|_| RootAccessError::new(RootProblem::Activation))?;
        for capability in local_roots.roots.values() {
            let overlaps = is_ancestor_identity(&directory, &capability.directory)
                .and_then(|left| {
                    is_ancestor_identity(&capability.directory, &directory)
                        .map(|right| left || right)
                })
                .map_err(|_| RootAccessError::new(RootProblem::Activation))?;
            if overlaps {
                return Err(RootAccessError::new(RootProblem::Activation));
            }
        }
        let root = RootCapability {
            kind: RootCapabilityKind::Export,
            directory,
        };
        validate_staging_root(&root, maximum_entries)?;
        let instance_lock = acquire_staging_lock(&root)?;
        let records = staging_child(&root, STAGING_RECORDS_DIRECTORY)?;
        let payloads = staging_child(&root, STAGING_PAYLOADS_DIRECTORY)?;
        let temporary = staging_child(&root, STAGING_TEMPORARY_DIRECTORY)?;
        let tombstones = staging_child(&root, STAGING_TOMBSTONES_DIRECTORY)?;
        sync_retained_directory(&root)?;
        validate_staging_root(&root, maximum_entries)?;
        let staging = Self {
            records,
            payloads,
            temporary,
            tombstones,
            _instance_lock: Arc::new(instance_lock),
        };
        let inventory = staging.inventory(maximum_entries, maximum_payload_bytes)?;
        Ok((staging, inventory))
    }

    pub(crate) fn inventory(
        &self,
        maximum_entries: usize,
        maximum_payload_bytes: u64,
    ) -> Result<StagingInventory, RootAccessError> {
        Ok(StagingInventory {
            records: inventory_directory(
                &self.records,
                maximum_entries,
                STAGING_STATE_BYTES,
                staging_record_name,
            )?,
            payloads: inventory_directory(
                &self.payloads,
                maximum_entries,
                maximum_payload_bytes,
                staging_payload_name,
            )?,
            temporary: inventory_directory(
                &self.temporary,
                maximum_entries,
                maximum_payload_bytes.max(STAGING_STATE_BYTES),
                staging_temporary_name,
            )?,
            tombstones: inventory_directory(
                &self.tombstones,
                maximum_entries,
                STAGING_STATE_BYTES,
                staging_record_name,
            )?,
        })
    }

    pub(crate) fn create_payload(
        &self,
        record_id: &str,
        maximum_bytes: u64,
    ) -> Result<StagingPayload, RootAccessError> {
        if !record_id_valid(record_id) || maximum_bytes == 0 {
            return Err(RootAccessError::new(RootProblem::Containment));
        }
        let directory = retained_directory(&self.payloads.directory)?;
        let name = format!("{record_id}.bin");
        let file = directory
            .open_with(Path::new(&name), &owner_private_create_options())
            .map(cap_std::fs::File::into_std)
            .map_err(|_| RootAccessError::new(RootProblem::Containment))?;
        let metadata = file
            .metadata()
            .map_err(|_| RootAccessError::new(RootProblem::Containment))?;
        if !safe_created_export_metadata(&file, &metadata) || !safe_windows_security(&file) {
            return Err(RootAccessError::new(RootProblem::Changed));
        }
        let identity = file_identity(&file, &metadata)
            .map_err(|_| RootAccessError::new(RootProblem::Changed))?;
        sync_parent_directory(&directory)
            .map_err(|_| RootAccessError::new(RootProblem::Indeterminate))?;
        Ok(StagingPayload {
            file,
            identity,
            maximum_bytes,
            written: 0,
        })
    }

    pub(crate) fn publish_record(
        &self,
        record_id: &str,
        bytes: &[u8],
    ) -> Result<AnchoredImport, RootAccessError> {
        self.publish_state(&self.records, record_id, bytes)
    }

    pub(crate) fn publish_tombstone(
        &self,
        record_id: &str,
        bytes: &[u8],
    ) -> Result<AnchoredImport, RootAccessError> {
        self.publish_state(&self.tombstones, record_id, bytes)
    }

    fn publish_state(
        &self,
        destination: &RootCapability,
        record_id: &str,
        bytes: &[u8],
    ) -> Result<AnchoredImport, RootAccessError> {
        if !record_id_valid(record_id) || bytes.len() as u64 > STAGING_STATE_BYTES {
            return Err(RootAccessError::new(RootProblem::TooLarge));
        }
        let temporary_directory = retained_directory(&self.temporary.directory)?;
        let destination_directory = retained_directory(&destination.directory)?;
        let random =
            getrandom::u64().map_err(|_| RootAccessError::new(RootProblem::Containment))?;
        let temporary_name = format!("{record_id}.{random:016x}.tmp");
        let mut file = temporary_directory
            .open_with(Path::new(&temporary_name), &owner_private_create_options())
            .map(cap_std::fs::File::into_std)
            .map_err(|_| RootAccessError::new(RootProblem::Containment))?;
        let result = (|| {
            file.write_all(bytes)
                .map_err(|_| RootAccessError::new(RootProblem::Containment))?;
            file.sync_all()
                .map_err(|_| RootAccessError::new(RootProblem::Containment))?;
            let metadata = file
                .metadata()
                .map_err(|_| RootAccessError::new(RootProblem::Containment))?;
            if metadata.len() != bytes.len() as u64
                || !safe_created_export_metadata(&file, &metadata)
                || !safe_windows_security(&file)
            {
                return Err(RootAccessError::new(RootProblem::Changed));
            }
            let identity = file_identity(&file, &metadata)
                .map_err(|_| RootAccessError::new(RootProblem::Changed))?;
            temporary_directory
                .rename(
                    Path::new(&temporary_name),
                    &destination_directory,
                    Path::new(&format!("{record_id}.json")),
                )
                .map_err(|_| RootAccessError::new(RootProblem::Indeterminate))?;
            sync_parent_directory(&destination_directory)
                .and_then(|()| sync_parent_directory(&temporary_directory))
                .map_err(|_| RootAccessError::new(RootProblem::Indeterminate))?;
            let snapshot = FileSnapshot::from_file(&file, &metadata)
                .map_err(|_| RootAccessError::new(RootProblem::Indeterminate))?;
            if snapshot.identity != identity {
                return Err(RootAccessError::new(RootProblem::Indeterminate));
            }
            Ok(AnchoredImport {
                file,
                length: bytes.len() as u64,
                snapshot,
                namespace: None,
            })
        })();
        if result.is_err() {
            let _ = remove_private_file(&temporary_directory, Path::new(&temporary_name), 1);
        }
        result
    }

    /// Removes one completed record after proving its retained identity.
    ///
    /// The record name is an index, not authority.  Cleanup reopens the fixed
    /// name without following links and compares its stable identity with the
    /// original retained handle before unlinking it.  Deliberately, this does
    /// not compare ctime: creating and later removing an external hard link
    /// legitimately changes ctime without changing the staged object.
    pub(crate) fn remove_exact_record(
        &self,
        record_name: &str,
        expected: &AnchoredImport,
    ) -> Result<(), RootAccessError> {
        let path = RelativeNativePath::from_utf8(record_name)
            .map_err(|_| RootAccessError::new(RootProblem::Containment))?;
        let (parent, name) = walk_parent(&self.payloads, &path)?;
        let mut options = OpenOptions::new();
        options.read(true).follow(FollowSymlinks::No);
        let file = parent
            .open_with(Path::new(&name), &options)
            .map(cap_std::fs::File::into_std)
            .map_err(|_| RootAccessError::new(RootProblem::Containment))?;
        let metadata = file
            .metadata()
            .map_err(|_| RootAccessError::new(RootProblem::Containment))?;
        let identity = file_identity(&file, &metadata)
            .map_err(|_| RootAccessError::new(RootProblem::Containment))?;
        if !private_file_with_links(&file, &metadata, 1)
            || !safe_windows_security(&file)
            || identity != expected.snapshot.identity
        {
            return Err(RootAccessError::new(RootProblem::Changed));
        }
        parent
            .remove_file(Path::new(&name))
            .map_err(|_| RootAccessError::new(RootProblem::Containment))?;
        sync_parent_directory(&parent).map_err(|_| RootAccessError::new(RootProblem::Containment))
    }

    pub(crate) fn truncate_exact_payload(
        &self,
        record_name: &str,
        expected: &AnchoredImport,
        offset: u64,
    ) -> Result<(), RootAccessError> {
        let path = RelativeNativePath::from_utf8(record_name)
            .map_err(|_| RootAccessError::new(RootProblem::Containment))?;
        let (parent, name) = walk_parent(&self.payloads, &path)?;
        let mut options = OpenOptions::new();
        options.read(true).write(true).follow(FollowSymlinks::No);
        let file = parent
            .open_with(Path::new(&name), &options)
            .map(cap_std::fs::File::into_std)
            .map_err(|_| RootAccessError::new(RootProblem::Containment))?;
        let metadata = file
            .metadata()
            .map_err(|_| RootAccessError::new(RootProblem::Containment))?;
        let identity = file_identity(&file, &metadata)
            .map_err(|_| RootAccessError::new(RootProblem::Containment))?;
        if !private_file_with_links(&file, &metadata, 1)
            || !safe_windows_security(&file)
            || identity != expected.snapshot.identity
            || offset > metadata.len()
        {
            return Err(RootAccessError::new(RootProblem::Changed));
        }
        file.set_len(offset)
            .and_then(|()| file.sync_all())
            .map_err(|_| RootAccessError::new(RootProblem::Containment))
    }

    pub(crate) fn remove_exact_record_state(
        &self,
        record_id: &str,
        expected: &AnchoredImport,
    ) -> Result<(), RootAccessError> {
        self.remove_exact_state(&self.records, record_id, expected)
    }

    pub(crate) fn remove_exact_tombstone(
        &self,
        record_id: &str,
        expected: &AnchoredImport,
    ) -> Result<(), RootAccessError> {
        self.remove_exact_state(&self.tombstones, record_id, expected)
    }

    pub(crate) fn remove_exact_temporary(
        &self,
        name: &str,
        expected: &AnchoredImport,
    ) -> Result<(), RootAccessError> {
        if !staging_temporary_name(name) {
            return Err(RootAccessError::new(RootProblem::Containment));
        }
        remove_exact_at(&self.temporary, name, expected.snapshot.identity)
    }

    fn remove_exact_state(
        &self,
        capability: &RootCapability,
        record_id: &str,
        expected: &AnchoredImport,
    ) -> Result<(), RootAccessError> {
        if !record_id_valid(record_id) {
            return Err(RootAccessError::new(RootProblem::Containment));
        }
        remove_exact_at(
            capability,
            &format!("{record_id}.json"),
            expected.snapshot.identity,
        )
    }
}

fn remove_exact_at(
    capability: &RootCapability,
    name: &str,
    expected: FileIdentity,
) -> Result<(), RootAccessError> {
    let directory = retained_directory(&capability.directory)?;
    let mut options = OpenOptions::new();
    options.read(true).follow(FollowSymlinks::No);
    let file = directory
        .open_with(Path::new(name), &options)
        .map(cap_std::fs::File::into_std)
        .map_err(|_| RootAccessError::new(RootProblem::Containment))?;
    let metadata = file
        .metadata()
        .map_err(|_| RootAccessError::new(RootProblem::Containment))?;
    let identity = file_identity(&file, &metadata)
        .map_err(|_| RootAccessError::new(RootProblem::Containment))?;
    if !private_file_with_links(&file, &metadata, 1)
        || !safe_windows_security(&file)
        || identity != expected
    {
        return Err(RootAccessError::new(RootProblem::Changed));
    }
    directory
        .remove_file(Path::new(name))
        .map_err(|_| RootAccessError::new(RootProblem::Containment))?;
    sync_parent_directory(&directory).map_err(|_| RootAccessError::new(RootProblem::Containment))
}

const STAGING_LOCK_NAME: &str = "instance.lock";
const STAGING_RECORDS_DIRECTORY: &str = "records";
const STAGING_PAYLOADS_DIRECTORY: &str = "payloads";
const STAGING_TEMPORARY_DIRECTORY: &str = "tmp";
const STAGING_TOMBSTONES_DIRECTORY: &str = "tombstones";
pub(crate) const STAGING_STATE_BYTES: u64 = 16 * 1024;

fn acquire_staging_lock(root: &RootCapability) -> Result<File, RootAccessError> {
    let directory = retained_directory(&root.directory)?;
    let mut options = OpenOptions::new();
    options
        .read(true)
        .write(true)
        .create(true)
        .follow(FollowSymlinks::No);
    #[cfg(unix)]
    options.mode(0o600);
    let file = directory
        .open_with(Path::new(STAGING_LOCK_NAME), &options)
        .map(cap_std::fs::File::into_std)
        .map_err(|_| RootAccessError::new(RootProblem::Activation))?;
    let metadata = file
        .metadata()
        .map_err(|_| RootAccessError::new(RootProblem::Activation))?;
    if !safe_created_export_metadata(&file, &metadata) || !safe_windows_security(&file) {
        return Err(RootAccessError::new(RootProblem::Activation));
    }
    file.try_lock_exclusive()
        .map_err(|_| RootAccessError::new(RootProblem::Activation))?;
    Ok(file)
}

fn validate_staging_root(
    root: &RootCapability,
    maximum_entries: usize,
) -> Result<(), RootAccessError> {
    let directory = retained_directory(&root.directory)?;
    let entries = directory
        .entries()
        .map_err(|_| RootAccessError::new(RootProblem::Activation))?;
    let maximum = maximum_entries
        .checked_add(5)
        .ok_or_else(|| RootAccessError::new(RootProblem::Activation))?;
    let mut observed = 0_usize;
    let mut names = BTreeSet::new();
    for entry in entries {
        let entry = entry.map_err(|_| RootAccessError::new(RootProblem::Activation))?;
        observed = observed
            .checked_add(1)
            .ok_or_else(|| RootAccessError::new(RootProblem::Activation))?;
        if observed > maximum {
            return Err(RootAccessError::new(RootProblem::Activation));
        }
        let name = entry.file_name();
        let Some(name_utf8) = name.to_str() else {
            return Err(RootAccessError::new(RootProblem::Activation));
        };
        if !matches!(
            name_utf8,
            STAGING_LOCK_NAME
                | STAGING_RECORDS_DIRECTORY
                | STAGING_PAYLOADS_DIRECTORY
                | STAGING_TEMPORARY_DIRECTORY
                | STAGING_TOMBSTONES_DIRECTORY
        ) || !names.insert(name_utf8.to_owned())
        {
            return Err(RootAccessError::new(RootProblem::Activation));
        }
        let metadata = entry
            .metadata()
            .map_err(|_| RootAccessError::new(RootProblem::Activation))?;
        if (name_utf8 == STAGING_LOCK_NAME && !metadata.is_file())
            || (name_utf8 != STAGING_LOCK_NAME && !metadata.is_dir())
        {
            return Err(RootAccessError::new(RootProblem::Activation));
        }
    }
    Ok(())
}

fn staging_child(
    root: &RootCapability,
    name: &'static str,
) -> Result<RootCapability, RootAccessError> {
    let directory = retained_directory(&root.directory)?;
    match directory.create_dir(Path::new(name)) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(_) => return Err(RootAccessError::new(RootProblem::Activation)),
    }
    let mut options = OpenOptions::new();
    options
        .read(true)
        .follow(FollowSymlinks::No)
        .maybe_dir(true);
    let file = directory
        .open_with(Path::new(name), &options)
        .map(cap_std::fs::File::into_std)
        .map_err(|_| RootAccessError::new(RootProblem::Activation))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        file.set_permissions(std::fs::Permissions::from_mode(0o700))
            .map_err(|_| RootAccessError::new(RootProblem::Activation))?;
    }
    let metadata = file
        .metadata()
        .map_err(|_| RootAccessError::new(RootProblem::Activation))?;
    if !safe_directory_metadata(&file, &metadata, root.directory.identity.volume)
        || !safe_windows_security(&file)
    {
        return Err(RootAccessError::new(RootProblem::Activation));
    }
    let identity = file_identity(&file, &metadata)
        .map_err(|_| RootAccessError::new(RootProblem::Activation))?;
    Ok(RootCapability {
        kind: RootCapabilityKind::Export,
        directory: OpenedDirectory {
            file: Arc::new(file),
            identity,
            #[cfg(windows)]
            ancestry: root.directory.ancestry.clone(),
        },
    })
}

fn inventory_directory(
    capability: &RootCapability,
    maximum_entries: usize,
    maximum_bytes: u64,
    valid_name: fn(&str) -> bool,
) -> Result<Vec<StagingInventoryFile>, RootAccessError> {
    let directory = retained_directory(&capability.directory)?;
    let mut inventory = Vec::new();
    let entries = directory
        .entries()
        .map_err(|_| RootAccessError::new(RootProblem::Activation))?;
    for entry in entries {
        let entry = entry.map_err(|_| RootAccessError::new(RootProblem::Activation))?;
        if inventory.len() >= maximum_entries {
            return Err(RootAccessError::new(RootProblem::Activation));
        }
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| RootAccessError::new(RootProblem::Activation))?;
        if !valid_name(&name)
            || inventory
                .iter()
                .any(|known: &StagingInventoryFile| known.name == name)
        {
            return Err(RootAccessError::new(RootProblem::Activation));
        }
        let path = RelativeNativePath::from_utf8(&name)
            .map_err(|_| RootAccessError::new(RootProblem::Activation))?;
        let source = open_import_at(capability, &path, maximum_bytes)
            .map_err(|_| RootAccessError::new(RootProblem::Activation))?;
        inventory.push(StagingInventoryFile { name, source });
    }
    Ok(inventory)
}

fn staging_record_name(name: &str) -> bool {
    name.strip_suffix(".json")
        .is_some_and(|stem| stem.len() == 32 && stem.bytes().all(lowercase_hex))
}

fn record_id_valid(record_id: &str) -> bool {
    record_id.len() == 32 && record_id.bytes().all(lowercase_hex)
}

fn staging_payload_name(name: &str) -> bool {
    name.strip_suffix(".bin")
        .is_some_and(|stem| stem.len() == 32 && stem.bytes().all(lowercase_hex))
}

fn staging_temporary_name(name: &str) -> bool {
    let Some((record, random)) = name
        .strip_suffix(".tmp")
        .and_then(|name| name.split_once('.'))
    else {
        return false;
    };
    record.len() == 32
        && record.bytes().all(lowercase_hex)
        && random.len() == 16
        && random.bytes().all(lowercase_hex)
}

fn retained_directory(directory: &OpenedDirectory) -> Result<Dir, RootAccessError> {
    directory
        .file
        .try_clone()
        .map(Dir::from_std_file)
        .map_err(|_| RootAccessError::new(RootProblem::Activation))
}

fn lowercase_hex(byte: u8) -> bool {
    byte.is_ascii_digit() || matches!(byte, b'a'..=b'f')
}

fn sync_retained_directory(capability: &RootCapability) -> Result<(), RootAccessError> {
    let directory = retained_directory(&capability.directory)?;
    sync_parent_directory(&directory).map_err(|_| RootAccessError::new(RootProblem::Activation))
}

/// One bounded same-directory export which is invisible until commit.
pub struct AtomicExport {
    parent: Dir,
    root: RootCapability,
    path: RelativeNativePath,
    parent_identity: FileIdentity,
    file: Option<File>,
    temporary_name: OsString,
    destination_name: OsString,
    maximum_bytes: u64,
    written: u64,
    published: bool,
    #[cfg(any(test, feature = "acceptance-harness"))]
    acceptance_gate: Option<(ArtifactAcceptanceGates, [u8; 32])>,
}

impl fmt::Debug for AtomicExport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AtomicExport")
            .field("maximum_bytes", &self.maximum_bytes)
            .field("written", &self.written)
            .field("published", &self.published)
            .finish()
    }
}

impl AtomicExport {
    /// Installs the acceptance-only prepublication point for this one export.
    #[cfg(any(test, feature = "acceptance-harness"))]
    pub(crate) fn with_acceptance_gate(
        mut self,
        gates: ArtifactAcceptanceGates,
        operation: [u8; 32],
    ) -> Self {
        self.acceptance_gate = Some((gates, operation));
        self
    }
    /// Flushes, verifies, and atomically publishes the destination.
    ///
    /// The destination must still be absent. The returned byte count is the
    /// exact length durably written before publication.
    ///
    /// # Errors
    ///
    /// Returns collision if another entry won the destination race, a size
    /// error if the retained file disagrees with bounded writes, or an
    /// indeterminate publication error if publication may have completed but
    /// durability or private-name cleanup could not be proven.
    pub fn commit(self) -> Result<u64, RootAccessError> {
        self.commit_retained().map(|published| published.length)
    }

    /// Publishes the destination and retains its verified read handle.
    ///
    /// This is used by private staging so completed bytes never need to be
    /// reopened through a mutable namespace before upload or download.
    pub(crate) fn commit_retained(mut self) -> Result<AnchoredImport, RootAccessError> {
        let Some(file) = self.file.take() else {
            return Err(RootAccessError::new(RootProblem::Containment));
        };
        file.sync_all()
            .map_err(|_| RootAccessError::new(RootProblem::Containment))?;
        let metadata = file
            .metadata()
            .map_err(|_| RootAccessError::new(RootProblem::Containment))?;
        if !metadata.is_file()
            || metadata.len() != self.written
            || metadata.len() > self.maximum_bytes
            || !safe_created_export_metadata(&file, &metadata)
            || !safe_windows_security(&file)
        {
            return Err(RootAccessError::new(RootProblem::Changed));
        }
        let source_identity = file_identity(&file, &metadata)
            .map_err(|_| RootAccessError::new(RootProblem::Changed))?;
        self.verify_export_namespace()?;
        #[cfg(any(test, feature = "acceptance-harness"))]
        if let Some((gates, operation)) = self.acceptance_gate.as_ref() {
            let handle = tokio::runtime::Handle::try_current()
                .map_err(|_| RootAccessError::new(RootProblem::Indeterminate))?;
            if !handle.block_on(gates.reach(
                ArtifactAcceptanceGatePoint::ExportPrepublication,
                *operation,
            )) {
                return Err(RootAccessError::new(RootProblem::Indeterminate));
            }
        }
        match self.parent.hard_link(
            Path::new(&self.temporary_name),
            &self.parent,
            Path::new(&self.destination_name),
        ) {
            Ok(()) => self.published = true,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                return Err(RootAccessError::new(RootProblem::Collision));
            }
            Err(_) => return Err(RootAccessError::new(RootProblem::Containment)),
        }
        #[cfg(any(test, feature = "acceptance-harness"))]
        if let Some((gates, operation)) = self.acceptance_gate.take() {
            let handle = tokio::runtime::Handle::try_current()
                .map_err(|_| RootAccessError::new(RootProblem::Indeterminate))?;
            if !handle.block_on(gates.reach(
                ArtifactAcceptanceGatePoint::ExportAtomicPublication,
                operation,
            )) {
                return Err(RootAccessError::new(RootProblem::Indeterminate));
            }
        }
        if self.verify_export_namespace().is_err() {
            drop(file);
            return Err(self.settle_namespace_mismatch());
        }
        let mut options = OpenOptions::new();
        options.read(true).follow(FollowSymlinks::No);
        let published = self
            .parent
            .open_with(Path::new(&self.destination_name), &options)
            .map(cap_std::fs::File::into_std)
            .map_err(|_| RootAccessError::new(RootProblem::Indeterminate))?;
        let published_metadata = published
            .metadata()
            .map_err(|_| RootAccessError::new(RootProblem::Indeterminate))?;
        let published_identity = file_identity(&published, &published_metadata)
            .map_err(|_| RootAccessError::new(RootProblem::Indeterminate))?;
        if !published_metadata.is_file()
            || published_metadata.len() != self.written
            || published_identity != source_identity
            || !private_file_with_links(&published, &published_metadata, 2)
            || !safe_windows_security(&published)
        {
            return Err(RootAccessError::new(RootProblem::Indeterminate));
        }
        drop(file);
        if remove_private_file(&self.parent, Path::new(&self.temporary_name), 2).is_err() {
            return Err(RootAccessError::new(RootProblem::Indeterminate));
        }
        sync_parent_directory(&self.parent)
            .map_err(|_| RootAccessError::new(RootProblem::Indeterminate))?;
        let settled_metadata = published
            .metadata()
            .map_err(|_| RootAccessError::new(RootProblem::Indeterminate))?;
        let snapshot = FileSnapshot::from_file(&published, &settled_metadata)
            .map_err(|_| RootAccessError::new(RootProblem::Indeterminate))?;
        Ok(AnchoredImport {
            file: published,
            length: self.written,
            snapshot,
            namespace: None,
        })
    }

    fn verify_export_namespace(&self) -> Result<(), RootAccessError> {
        let (parent, _) = walk_parent(&self.root, &self.path)
            .map_err(|_| RootAccessError::new(RootProblem::Changed))?;
        let identity =
            directory_identity(&parent).map_err(|_| RootAccessError::new(RootProblem::Changed))?;
        if identity != self.parent_identity {
            return Err(RootAccessError::new(RootProblem::Changed));
        }
        Ok(())
    }

    fn remove_mismatched_publication(&mut self) -> bool {
        let destination_absent =
            remove_private_file(&self.parent, Path::new(&self.destination_name), 2).is_ok()
                || private_name_absent(&self.parent, Path::new(&self.destination_name));
        if !destination_absent {
            return false;
        }
        self.published = false;
        let temporary_absent =
            remove_private_file(&self.parent, Path::new(&self.temporary_name), 1).is_ok()
                || private_name_absent(&self.parent, Path::new(&self.temporary_name));
        temporary_absent
            && sync_parent_directory(&self.parent).is_ok()
            && private_name_absent(&self.parent, Path::new(&self.destination_name))
            && private_name_absent(&self.parent, Path::new(&self.temporary_name))
    }

    fn settle_namespace_mismatch(&mut self) -> RootAccessError {
        if self.remove_mismatched_publication() {
            RootAccessError::new(RootProblem::Changed)
        } else {
            RootAccessError::new(RootProblem::Indeterminate)
        }
    }
}

impl Write for AtomicExport {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let requested = u64::try_from(bytes.len())
            .map_err(|_| io::Error::other("artifact write length overflow"))?;
        let proposed = self
            .written
            .checked_add(requested)
            .ok_or_else(|| io::Error::other("artifact write length overflow"))?;
        if proposed > self.maximum_bytes {
            return Err(io::Error::other(
                "artifact exceeds the configured size limit",
            ));
        }
        let file = self
            .file
            .as_mut()
            .ok_or_else(|| io::Error::other("artifact export is not writable"))?;
        let written = file.write(bytes)?;
        self.written = self
            .written
            .checked_add(written as u64)
            .ok_or_else(|| io::Error::other("artifact write length overflow"))?;
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file
            .as_mut()
            .ok_or_else(|| io::Error::other("artifact export is not writable"))?
            .flush()
    }
}

impl Drop for AtomicExport {
    fn drop(&mut self) {
        self.file.take();
        let _ = remove_private_file(&self.parent, Path::new(&self.temporary_name), 1);
    }
}

#[derive(Clone)]
struct NamespaceBinding {
    root: RootCapability,
    path: RelativeNativePath,
}

/// An opened, preflighted import source.
pub struct AnchoredImport {
    file: File,
    /// Source length observed after the anchored no-follow open.
    pub length: u64,
    snapshot: FileSnapshot,
    namespace: Option<NamespaceBinding>,
}

impl fmt::Debug for AnchoredImport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AnchoredImport")
            .field("length", &self.length)
            .finish_non_exhaustive()
    }
}

impl AnchoredImport {
    pub(crate) fn staging_identity(&self) -> StagingFileIdentity {
        StagingFileIdentity {
            volume: self.snapshot.identity.volume,
            file: self.snapshot.identity.file,
        }
    }
    /// Returns a mutable reader for the retained source.
    pub fn reader(&mut self) -> &mut File {
        &mut self.file
    }

    /// Clones the retained source handle without reopening its path.
    ///
    /// # Errors
    ///
    /// Returns a fixed containment error when the operating system cannot
    /// duplicate the live handle.
    pub fn try_clone_reader(&self) -> Result<File, RootAccessError> {
        let mut reader = self
            .file
            .try_clone()
            .map_err(|_| RootAccessError::new(RootProblem::Containment))?;
        reader
            .seek(SeekFrom::Start(0))
            .map_err(|_| RootAccessError::new(RootProblem::Containment))?;
        Ok(reader)
    }

    /// Rechecks stable identity and size after streaming.
    ///
    /// # Errors
    ///
    /// Returns a fixed conflict error when the opened object changed.
    pub fn verify_unchanged(&self) -> Result<(), RootAccessError> {
        let metadata = self
            .file
            .metadata()
            .map_err(|_| RootAccessError::new(RootProblem::Changed))?;
        let snapshot = FileSnapshot::from_file(&self.file, &metadata)
            .map_err(|_| RootAccessError::new(RootProblem::Changed))?;
        if snapshot != self.snapshot {
            return Err(RootAccessError::new(RootProblem::Changed));
        }
        if let Some(binding) = &self.namespace {
            let reopened = open_import_at(&binding.root, &binding.path, u64::MAX)
                .map_err(|_| RootAccessError::new(RootProblem::Changed))?;
            if reopened.snapshot != self.snapshot {
                return Err(RootAccessError::new(RootProblem::Changed));
            }
        }
        Ok(())
    }
}

/// Fixed root-capability diagnostic.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RootAccessError {
    problem: RootProblem,
}

impl RootAccessError {
    const fn new(problem: RootProblem) -> Self {
        Self { problem }
    }

    /// Builds the fixed failure used when a client-root snapshot cannot be
    /// securely frozen for the session.
    pub(crate) const fn client_roots() -> Self {
        Self::new(RootProblem::ClientRoots)
    }

    /// Returns the stable, path-free failure classification.
    #[must_use]
    pub const fn kind(&self) -> RootAccessErrorKind {
        match self.problem {
            RootProblem::Missing => RootAccessErrorKind::Missing,
            RootProblem::Unauthorized => RootAccessErrorKind::Unauthorized,
            RootProblem::Activation => RootAccessErrorKind::Activation,
            RootProblem::ClientRoots => RootAccessErrorKind::ClientRoots,
            RootProblem::Containment => RootAccessErrorKind::Containment,
            RootProblem::TooLarge => RootAccessErrorKind::TooLarge,
            RootProblem::Collision => RootAccessErrorKind::Collision,
            RootProblem::Changed => RootAccessErrorKind::Changed,
            RootProblem::Indeterminate => RootAccessErrorKind::Indeterminate,
            #[cfg(not(any(unix, windows)))]
            RootProblem::Platform => RootAccessErrorKind::Platform,
        }
    }
}

impl fmt::Display for RootAccessError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self.problem {
            RootProblem::Missing => ROOTS_REQUIRED_GUIDANCE,
            RootProblem::Unauthorized => "Artifact root is not authorized.",
            RootProblem::Activation => "Artifact root activation failed.",
            RootProblem::ClientRoots => "Client root policy is invalid.",
            RootProblem::Containment => "Artifact path is not safely contained.",
            RootProblem::TooLarge => "Artifact exceeds the configured size limit.",
            RootProblem::Collision => "Artifact export destination already exists.",
            RootProblem::Changed => "Artifact source changed during the operation.",
            RootProblem::Indeterminate => "Artifact export publication is indeterminate.",
            #[cfg(not(any(unix, windows)))]
            RootProblem::Platform => "Artifact root controls are unavailable on this platform.",
        })
    }
}

impl std::error::Error for RootAccessError {}

/// Stable classification for a root-capability failure.
///
/// Callers use this classification instead of parsing human-readable
/// diagnostics, which remain fixed and path-free.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RootAccessErrorKind {
    /// No root of the required capability was configured.
    Missing,
    /// The requested logical root or capability is not authorized.
    Unauthorized,
    /// A configured root could not be securely activated.
    Activation,
    /// The client-root intersection could not be securely frozen.
    ClientRoots,
    /// The relative path could not be safely contained.
    Containment,
    /// The artifact exceeded its configured byte ceiling.
    TooLarge,
    /// The create-new destination already exists.
    Collision,
    /// An anchored source or destination changed during the operation.
    Changed,
    /// Publication may have completed but could not be proven.
    Indeterminate,
    /// Secure root controls are unavailable on this target.
    #[cfg(not(any(unix, windows)))]
    Platform,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RootProblem {
    Missing,
    Unauthorized,
    Activation,
    ClientRoots,
    Containment,
    TooLarge,
    Collision,
    Changed,
    Indeterminate,
    #[cfg(not(any(unix, windows)))]
    Platform,
}

#[derive(Clone)]
struct RootCapability {
    kind: RootCapabilityKind,
    directory: OpenedDirectory,
}

#[derive(Clone)]
struct OpenedDirectory {
    file: Arc<File>,
    identity: FileIdentity,
    #[cfg(windows)]
    ancestry: Arc<Vec<RetainedWindowsAncestor>>,
}

impl fmt::Debug for OpenedDirectory {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("OpenedDirectory(<capability>)")
    }
}

#[cfg(windows)]
#[derive(Clone)]
struct RetainedWindowsAncestor {
    _file: Arc<File>,
    identity: FileIdentity,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct FileIdentity {
    volume: u64,
    file: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FileSnapshot {
    identity: FileIdentity,
    length: u64,
    modified: i128,
    changed: i128,
}

#[cfg(unix)]
impl FileSnapshot {
    fn from_file(_: &File, metadata: &std::fs::Metadata) -> io::Result<Self> {
        use std::os::unix::fs::MetadataExt;

        Ok(Self {
            identity: file_identity_unix(metadata),
            length: metadata.len(),
            modified: i128::from(metadata.mtime()) * 1_000_000_000
                + i128::from(metadata.mtime_nsec()),
            changed: i128::from(metadata.ctime()) * 1_000_000_000
                + i128::from(metadata.ctime_nsec()),
        })
    }
}

#[cfg(windows)]
impl FileSnapshot {
    fn from_file(file: &File, metadata: &std::fs::Metadata) -> io::Result<Self> {
        use std::os::windows::fs::MetadataExt;

        let handle = windows_security::handle_metadata(file)?;
        Ok(Self {
            identity: FileIdentity {
                volume: handle.volume,
                file: handle.file,
            },
            length: metadata.file_size(),
            modified: i128::from(handle.last_write),
            changed: i128::from(handle.change),
        })
    }
}

#[cfg(not(any(unix, windows)))]
impl FileSnapshot {
    fn from_file(_: &File, _: &std::fs::Metadata) -> io::Result<Self> {
        Err(io::Error::other("unsupported platform"))
    }
}

fn insert_root(
    roots: &mut BTreeMap<LogicalRootId, RootCapability>,
    definition: &RootDefinition,
    kind: RootCapabilityKind,
) -> Result<(), RootAccessError> {
    let directory =
        open_root(&definition.path).map_err(|_| RootAccessError::new(RootProblem::Activation))?;
    if roots
        .insert(definition.id.clone(), RootCapability { kind, directory })
        .is_some()
    {
        return Err(RootAccessError::new(RootProblem::Activation));
    }
    Ok(())
}

#[cfg(unix)]
fn file_identity(file: &File, metadata: &std::fs::Metadata) -> io::Result<FileIdentity> {
    let _ = file;
    Ok(file_identity_unix(metadata))
}

#[cfg(unix)]
fn file_identity_unix(metadata: &std::fs::Metadata) -> FileIdentity {
    use std::os::unix::fs::MetadataExt;

    FileIdentity {
        volume: metadata.dev(),
        file: metadata.ino(),
    }
}

#[cfg(windows)]
fn file_identity(file: &File, _: &std::fs::Metadata) -> io::Result<FileIdentity> {
    let metadata = windows_security::handle_metadata(file)?;
    Ok(FileIdentity {
        volume: metadata.volume,
        file: metadata.file,
    })
}

#[cfg(not(any(unix, windows)))]
fn file_identity(_: &File, _: &std::fs::Metadata) -> io::Result<FileIdentity> {
    Err(io::Error::other("unsupported platform"))
}

#[cfg(unix)]
fn open_root(path: &AbsoluteNativePath) -> io::Result<OpenedDirectory> {
    use std::{
        ffi::CString,
        os::{
            fd::{AsRawFd, FromRawFd},
            unix::{ffi::OsStrExt, fs::MetadataExt},
        },
    };

    let mut current = File::open("/")?;
    for component in path.as_path().components() {
        match component {
            std::path::Component::RootDir => continue,
            std::path::Component::Normal(component) => {
                let component = CString::new(component.as_bytes())
                    .map_err(|_| io::Error::other("invalid component"))?;
                // SAFETY: `current` is a live directory descriptor and the
                // component is a validated, NUL-terminated single component.
                let descriptor = unsafe {
                    libc::openat(
                        current.as_raw_fd(),
                        component.as_ptr(),
                        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                    )
                };
                if descriptor < 0 {
                    return Err(io::Error::last_os_error());
                }
                // SAFETY: `openat` returned a new owned descriptor.
                current = unsafe { File::from_raw_fd(descriptor) };
            }
            _ => return Err(io::Error::other("invalid root path")),
        }
    }
    let metadata = current.metadata()?;
    // SAFETY: `geteuid` has no memory or ownership preconditions.
    let effective_user = unsafe { libc::geteuid() };
    if !metadata.is_dir() || metadata.uid() != effective_user || metadata.mode() & 0o022 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "unsafe root permissions",
        ));
    }
    let identity = file_identity(&current, &metadata)?;
    Ok(OpenedDirectory {
        file: Arc::new(current),
        identity,
    })
}

#[cfg(windows)]
fn open_root(path: &AbsoluteNativePath) -> io::Result<OpenedDirectory> {
    let (file, ancestry) = open_windows_root_chain(path.as_path())?;
    let metadata = file.metadata()?;
    let identity = file_identity(&file, &metadata)?;
    if !safe_directory_metadata(&file, &metadata, identity.volume) || !safe_windows_security(&file)
    {
        return Err(io::Error::other("unsafe root"));
    }
    Ok(OpenedDirectory {
        file: Arc::new(file),
        identity,
        ancestry: Arc::new(ancestry),
    })
}

#[cfg(windows)]
fn open_windows_root_chain(path: &Path) -> io::Result<(File, Vec<RetainedWindowsAncestor>)> {
    use std::os::windows::fs::MetadataExt;

    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
    let anchor = path
        .ancestors()
        .filter(|ancestor| !ancestor.as_os_str().is_empty())
        .last()
        .ok_or_else(|| io::Error::other("root path has no volume anchor"))?;
    let relative = path
        .strip_prefix(anchor)
        .map_err(|_| io::Error::other("root path has no stable anchor"))?;
    let mut current = Dir::open_ambient_dir(anchor, cap_std::ambient_authority())?.into_std_file();
    let mut retained = Vec::new();
    let anchor_metadata = current.metadata()?;
    if !anchor_metadata.is_dir()
        || anchor_metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    {
        return Err(io::Error::other("unsafe root anchor"));
    }
    retained.push(RetainedWindowsAncestor {
        _file: Arc::new(current.try_clone()?),
        identity: file_identity(&current, &anchor_metadata)?,
    });
    for component in relative.components() {
        let std::path::Component::Normal(component) = component else {
            return Err(io::Error::other("invalid root component"));
        };
        let mut options = OpenOptions::new();
        options
            .read(true)
            .follow(FollowSymlinks::No)
            .maybe_dir(true);
        let parent = Dir::from_std_file(current);
        current = parent.open_with(Path::new(component), &options)?.into_std();
        let metadata = current.metadata()?;
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(io::Error::other("root namespace contains a reparse point"));
        }
        if !metadata.is_dir() {
            return Err(io::Error::other("root component is not a directory"));
        }
        retained.push(RetainedWindowsAncestor {
            _file: Arc::new(current.try_clone()?),
            identity: file_identity(&current, &metadata)?,
        });
    }
    Ok((current, retained))
}

#[cfg(not(any(unix, windows)))]
fn open_root(_: &AbsoluteNativePath) -> io::Result<OpenedDirectory> {
    Err(io::Error::other("unsupported platform"))
}

#[cfg(unix)]
fn is_ancestor_identity(
    candidate: &OpenedDirectory,
    descendant: &OpenedDirectory,
) -> io::Result<bool> {
    use std::os::fd::{AsRawFd, FromRawFd};

    let mut current = descendant.clone();
    for _ in 0..1_024 {
        if current.identity == candidate.identity {
            return Ok(true);
        }
        // SAFETY: `current` is a live directory descriptor and the static
        // component is NUL-terminated. The returned descriptor is owned below.
        let descriptor = unsafe {
            libc::openat(
                current.file.as_raw_fd(),
                c"..".as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if descriptor < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `openat` returned a new owned descriptor.
        let parent = unsafe { File::from_raw_fd(descriptor) };
        let metadata = parent.metadata()?;
        let identity = file_identity(&parent, &metadata)?;
        if identity == current.identity {
            return Ok(false);
        }
        current = OpenedDirectory {
            file: Arc::new(parent),
            identity,
        };
    }
    Err(io::Error::other("filesystem ancestry is too deep"))
}

#[cfg(windows)]
fn is_ancestor_identity(
    candidate: &OpenedDirectory,
    descendant: &OpenedDirectory,
) -> io::Result<bool> {
    Ok(descendant
        .ancestry
        .iter()
        .any(|ancestor| ancestor.identity == candidate.identity))
}

#[cfg(not(any(unix, windows)))]
fn is_ancestor_identity(_: &OpenedDirectory, _: &OpenedDirectory) -> io::Result<bool> {
    Err(io::Error::other("unsupported platform"))
}

fn open_import_at(
    root: &RootCapability,
    path: &RelativeNativePath,
    maximum_bytes: u64,
) -> Result<AnchoredImport, RootAccessError> {
    let (parent, name) = walk_parent(root, path)?;
    let mut options = OpenOptions::new();
    options.read(true).follow(FollowSymlinks::No);
    let file = parent
        .open_with(Path::new(&name), &options)
        .map(cap_std::fs::File::into_std)
        .map_err(|_| RootAccessError::new(RootProblem::Containment))?;
    let metadata = file
        .metadata()
        .map_err(|_| RootAccessError::new(RootProblem::Containment))?;
    if !safe_import_metadata(&file, &metadata) || !safe_windows_security(&file) {
        return Err(RootAccessError::new(RootProblem::Containment));
    }
    if metadata.len() > maximum_bytes {
        return Err(RootAccessError::new(RootProblem::TooLarge));
    }
    let snapshot = FileSnapshot::from_file(&file, &metadata)
        .map_err(|_| RootAccessError::new(RootProblem::Containment))?;
    Ok(AnchoredImport {
        file,
        length: metadata.len(),
        snapshot,
        namespace: Some(NamespaceBinding {
            root: root.clone(),
            path: path.clone(),
        }),
    })
}

fn walk_parent(
    root: &RootCapability,
    path: &RelativeNativePath,
) -> Result<(Dir, OsString), RootAccessError> {
    let mut components = path.as_path().components().peekable();
    let root_file = root
        .directory
        .file
        .try_clone()
        .map_err(|_| RootAccessError::new(RootProblem::Containment))?;
    let mut current = Dir::from_std_file(root_file);
    while let Some(component) = components.next() {
        let std::path::Component::Normal(component) = component else {
            return Err(RootAccessError::new(RootProblem::Containment));
        };
        if components.peek().is_none() {
            return Ok((current, component.to_os_string()));
        }
        let mut options = OpenOptions::new();
        options
            .read(true)
            .follow(FollowSymlinks::No)
            .maybe_dir(true);
        let directory = current
            .open_with(Path::new(component), &options)
            .map(cap_std::fs::File::into_std)
            .map_err(|_| RootAccessError::new(RootProblem::Containment))?;
        let metadata = directory
            .metadata()
            .map_err(|_| RootAccessError::new(RootProblem::Containment))?;
        if !safe_directory_metadata(&directory, &metadata, root.directory.identity.volume)
            || !safe_windows_security(&directory)
        {
            return Err(RootAccessError::new(RootProblem::Containment));
        }
        current = Dir::from_std_file(directory);
    }
    Err(RootAccessError::new(RootProblem::Containment))
}

fn begin_atomic_export_at(
    root: &RootCapability,
    path: &RelativeNativePath,
    maximum_bytes: u64,
) -> Result<AtomicExport, RootAccessError> {
    let (parent, destination_name) = walk_parent(root, path)?;
    let parent_identity = directory_identity(&parent)?;
    match parent.symlink_metadata(Path::new(&destination_name)) {
        Ok(_) => return Err(RootAccessError::new(RootProblem::Collision)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(_) => return Err(RootAccessError::new(RootProblem::Containment)),
    }

    for _ in 0..16 {
        let random =
            getrandom::u64().map_err(|_| RootAccessError::new(RootProblem::Containment))?;
        let temporary_name = OsString::from(format!(".any-mcp-{random:016x}.tmp"));
        let options = owner_private_create_options();
        match parent.open_with(Path::new(&temporary_name), &options) {
            Ok(file) => {
                let file = file.into_std();
                let metadata = file
                    .metadata()
                    .map_err(|_| RootAccessError::new(RootProblem::Containment))?;
                if !safe_created_export_metadata(&file, &metadata) || !safe_windows_security(&file)
                {
                    drop(file);
                    let _ = parent.remove_file(Path::new(&temporary_name));
                    return Err(RootAccessError::new(RootProblem::Containment));
                }
                return Ok(AtomicExport {
                    parent,
                    root: root.clone(),
                    path: path.clone(),
                    parent_identity,
                    file: Some(file),
                    temporary_name,
                    destination_name,
                    maximum_bytes,
                    written: 0,
                    published: false,
                    #[cfg(any(test, feature = "acceptance-harness"))]
                    acceptance_gate: None,
                });
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(_) => return Err(RootAccessError::new(RootProblem::Containment)),
        }
    }
    Err(RootAccessError::new(RootProblem::Containment))
}

fn directory_identity(directory: &Dir) -> Result<FileIdentity, RootAccessError> {
    let file = directory
        .try_clone()
        .map(Dir::into_std_file)
        .map_err(|_| RootAccessError::new(RootProblem::Containment))?;
    let metadata = file
        .metadata()
        .map_err(|_| RootAccessError::new(RootProblem::Containment))?;
    file_identity(&file, &metadata).map_err(|_| RootAccessError::new(RootProblem::Containment))
}

fn remove_private_file(
    parent: &Dir,
    name: &Path,
    expected_links: u32,
) -> Result<(), RootAccessError> {
    let mut options = OpenOptions::new();
    options.read(true).follow(FollowSymlinks::No);
    let file = parent
        .open_with(name, &options)
        .map(cap_std::fs::File::into_std)
        .map_err(|_| RootAccessError::new(RootProblem::Containment))?;
    let metadata = file
        .metadata()
        .map_err(|_| RootAccessError::new(RootProblem::Containment))?;
    if !private_file_with_links(&file, &metadata, expected_links) || !safe_windows_security(&file) {
        return Err(RootAccessError::new(RootProblem::Changed));
    }
    parent
        .remove_file(name)
        .map_err(|_| RootAccessError::new(RootProblem::Containment))
}

fn private_name_absent(parent: &Dir, name: &Path) -> bool {
    matches!(
        parent.symlink_metadata(name),
        Err(error) if error.kind() == io::ErrorKind::NotFound
    )
}

fn owner_private_create_options() -> OpenOptions {
    let mut options = OpenOptions::new();
    options
        .read(true)
        .write(true)
        .create_new(true)
        .follow(FollowSymlinks::No);
    #[cfg(unix)]
    options.mode(0o600);
    options
}

#[cfg(unix)]
fn sync_parent_directory(parent: &Dir) -> io::Result<()> {
    parent.try_clone()?.into_std_file().sync_all()
}

#[cfg(not(unix))]
fn sync_parent_directory(_: &Dir) -> io::Result<()> {
    Ok(())
}

#[cfg(unix)]
fn safe_directory_metadata(_: &File, metadata: &std::fs::Metadata, root_volume: u64) -> bool {
    use std::os::unix::fs::MetadataExt;

    // SAFETY: `geteuid` has no memory or ownership preconditions.
    let effective_user = unsafe { libc::geteuid() };
    metadata.is_dir()
        && metadata.dev() == root_volume
        && metadata.uid() == effective_user
        && metadata.mode() & 0o022 == 0
}

#[cfg(windows)]
fn safe_directory_metadata(file: &File, metadata: &std::fs::Metadata, root_volume: u64) -> bool {
    use std::os::windows::fs::MetadataExt;

    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
    metadata.is_dir()
        && metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT == 0
        && file_identity(file, metadata)
            .map(|identity| identity.volume == root_volume)
            .unwrap_or(false)
}

#[cfg(not(any(unix, windows)))]
fn safe_directory_metadata(_: &File, _: &std::fs::Metadata, _: u64) -> bool {
    false
}

#[cfg(unix)]
fn safe_import_metadata(_: &File, metadata: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;

    // SAFETY: `geteuid` has no memory or ownership preconditions.
    let effective_user = unsafe { libc::geteuid() };
    metadata.is_file()
        && metadata.uid() == effective_user
        && metadata.mode() & 0o022 == 0
        && metadata.nlink() == 1
}

#[cfg(windows)]
fn safe_import_metadata(file: &File, metadata: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;

    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
    metadata.is_file()
        && metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT == 0
        && windows_security::handle_metadata(file)
            .map(|handle| handle.number_of_links == 1)
            .unwrap_or(false)
}

#[cfg(not(any(unix, windows)))]
fn safe_import_metadata(_: &File, _: &std::fs::Metadata) -> bool {
    false
}

#[cfg(unix)]
fn safe_created_export_metadata(_: &File, metadata: &std::fs::Metadata) -> bool {
    private_file_with_links_unix(metadata, 1)
}

#[cfg(unix)]
fn private_file_with_links(_: &File, metadata: &std::fs::Metadata, expected_links: u32) -> bool {
    private_file_with_links_unix(metadata, expected_links)
}

#[cfg(unix)]
fn private_file_with_links_unix(metadata: &std::fs::Metadata, expected_links: u32) -> bool {
    use std::os::unix::fs::MetadataExt;

    // SAFETY: `geteuid` has no memory or ownership preconditions.
    let effective_user = unsafe { libc::geteuid() };
    metadata.is_file()
        && metadata.uid() == effective_user
        && metadata.mode() & 0o077 == 0
        && metadata.nlink() == u64::from(expected_links)
}

#[cfg(windows)]
fn safe_created_export_metadata(file: &File, metadata: &std::fs::Metadata) -> bool {
    private_file_with_links(file, metadata, 1)
}

#[cfg(windows)]
fn private_file_with_links(file: &File, metadata: &std::fs::Metadata, expected_links: u32) -> bool {
    use std::os::windows::fs::MetadataExt;

    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
    metadata.is_file()
        && metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT == 0
        && windows_security::handle_metadata(file)
            .map(|handle| handle.number_of_links == expected_links)
            .unwrap_or(false)
}

#[cfg(not(any(unix, windows)))]
fn safe_created_export_metadata(_: &File, _: &std::fs::Metadata) -> bool {
    false
}

#[cfg(not(any(unix, windows)))]
fn private_file_with_links(_: &File, _: &std::fs::Metadata, _: u32) -> bool {
    false
}

#[cfg(not(windows))]
const fn safe_windows_security(_: &File) -> bool {
    true
}

#[cfg(windows)]
fn safe_windows_security(file: &File) -> bool {
    windows_security::owner_and_dacl_are_safe(file).unwrap_or(false)
}

/// Validates an owner-private, non-reparse regular file for Windows acceptance evidence.
#[cfg(windows)]
#[doc(hidden)]
#[must_use]
pub fn acceptance_owner_private_file(file: &File) -> bool {
    file.metadata().ok().is_some_and(|metadata| {
        safe_import_metadata(file, &metadata) && safe_windows_security(file)
    })
}

#[cfg(windows)]
pub(crate) mod windows_security {
    use std::{
        ffi::c_void,
        io,
        mem::{offset_of, size_of},
        os::windows::io::AsRawHandle,
        ptr,
    };

    use windows_sys::Win32::{
        Foundation::{CloseHandle, GENERIC_ALL, GENERIC_WRITE, HANDLE, LocalFree},
        Security::{
            ACCESS_ALLOWED_ACE, ACE_HEADER, ACL, ACL_SIZE_INFORMATION, AclSizeInformation,
            Authorization::{GetSecurityInfo, SE_FILE_OBJECT},
            DACL_SECURITY_INFORMATION, EqualSid, GetAce, GetAclInformation, GetTokenInformation,
            IsWellKnownSid, OWNER_SECURITY_INFORMATION, PSID, TOKEN_QUERY, TOKEN_USER, TokenUser,
            WinBuiltinAdministratorsSid, WinLocalSystemSid,
        },
        Storage::FileSystem::{
            BY_HANDLE_FILE_INFORMATION, DELETE, FILE_APPEND_DATA, FILE_BASIC_INFO,
            FILE_DELETE_CHILD, FILE_WRITE_DATA, FileBasicInfo, GetFileInformationByHandle,
            GetFileInformationByHandleEx, WRITE_DAC, WRITE_OWNER,
        },
        System::{
            SystemServices::ACCESS_ALLOWED_ACE_TYPE,
            Threading::{GetCurrentProcess, OpenProcessToken},
        },
    };

    const ACCESS_DENIED_ACE_TYPE: u8 = 1;
    const ACCESS_DENIED_OBJECT_ACE_TYPE: u8 = 6;
    const ACCESS_DENIED_CALLBACK_ACE_TYPE: u8 = 10;
    const ACCESS_DENIED_CALLBACK_OBJECT_ACE_TYPE: u8 = 12;
    const FILE_WRITE_EA: u32 = 0x0010;
    const FILE_WRITE_ATTRIBUTES: u32 = 0x0100;
    const DANGEROUS_ACCESS: u32 = FILE_WRITE_DATA
        | FILE_APPEND_DATA
        | FILE_WRITE_EA
        | FILE_DELETE_CHILD
        | FILE_WRITE_ATTRIBUTES
        | DELETE
        | WRITE_DAC
        | WRITE_OWNER
        | GENERIC_WRITE
        | GENERIC_ALL;

    pub(crate) struct HandleMetadata {
        pub(crate) volume: u64,
        pub(crate) file: u64,
        pub(crate) number_of_links: u32,
        pub(crate) last_write: i64,
        pub(crate) change: i64,
    }

    pub(crate) fn handle_metadata(file: &std::fs::File) -> io::Result<HandleMetadata> {
        let handle = file.as_raw_handle() as HANDLE;
        let mut information = BY_HANDLE_FILE_INFORMATION::default();
        // SAFETY: the file handle is live and the output structure has the
        // exact type required by the Win32 API.
        if unsafe { GetFileInformationByHandle(handle, &mut information) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut basic = FILE_BASIC_INFO::default();
        // SAFETY: the file handle is live and `basic` is a correctly sized,
        // writable buffer for `FileBasicInfo`.
        if unsafe {
            GetFileInformationByHandleEx(
                handle,
                FileBasicInfo,
                ptr::addr_of_mut!(basic).cast(),
                u32::try_from(size_of::<FILE_BASIC_INFO>()).unwrap_or(u32::MAX),
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(HandleMetadata {
            volume: u64::from(information.dwVolumeSerialNumber),
            file: (u64::from(information.nFileIndexHigh) << 32)
                | u64::from(information.nFileIndexLow),
            number_of_links: information.nNumberOfLinks,
            last_write: basic.LastWriteTime,
            change: basic.ChangeTime,
        })
    }

    struct OwnedHandle(HANDLE);

    impl Drop for OwnedHandle {
        fn drop(&mut self) {
            // SAFETY: the handle was returned by `OpenProcessToken` and is
            // closed exactly once by this owner.
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }

    struct LocalSecurityDescriptor(*mut c_void);

    impl Drop for LocalSecurityDescriptor {
        fn drop(&mut self) {
            // SAFETY: `GetSecurityInfo` allocated this descriptor with the
            // local allocator and ownership is released exactly once.
            unsafe {
                let _ = LocalFree(self.0);
            }
        }
    }

    struct TokenUserBuffer {
        words: Vec<usize>,
    }

    impl TokenUserBuffer {
        fn sid(&self) -> PSID {
            // SAFETY: the buffer is aligned for `TOKEN_USER`, was initialized
            // by `GetTokenInformation`, and remains live with this borrow.
            unsafe { (*self.words.as_ptr().cast::<TOKEN_USER>()).User.Sid }
        }
    }

    pub(crate) fn owner_and_dacl_are_safe(file: &std::fs::File) -> io::Result<bool> {
        let token = current_process_token()?;
        let token_user_buffer = token_user_buffer(token.0)?;
        let token_user = token_user_buffer.sid();

        let mut owner = ptr::null_mut();
        let mut dacl: *mut ACL = ptr::null_mut();
        let mut descriptor = ptr::null_mut();
        // SAFETY: the file handle is live for this call; output pointers are
        // valid and the returned descriptor is immediately assigned an owner.
        let status = unsafe {
            GetSecurityInfo(
                file.as_raw_handle() as HANDLE,
                SE_FILE_OBJECT,
                OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
                &mut owner,
                ptr::null_mut(),
                &mut dacl,
                ptr::null_mut(),
                &mut descriptor,
            )
        };
        if status != 0 || descriptor.is_null() {
            return Err(io::Error::other("Windows security query failed"));
        }
        let _descriptor = LocalSecurityDescriptor(descriptor);
        if owner.is_null() || dacl.is_null() {
            return Ok(false);
        }
        // SAFETY: both SIDs are owned by live token/descriptor buffers.
        if unsafe { EqualSid(owner, token_user) } == 0 {
            return Ok(false);
        }
        dacl_has_no_untrusted_writers(dacl, token_user)
    }

    fn current_process_token() -> io::Result<OwnedHandle> {
        let mut token = ptr::null_mut();
        // SAFETY: the pseudo-process handle is always live and `token` is a
        // valid output pointer.
        let opened = unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) };
        if opened == 0 || token.is_null() {
            return Err(io::Error::last_os_error());
        }
        Ok(OwnedHandle(token))
    }

    fn token_user_buffer(token: HANDLE) -> io::Result<TokenUserBuffer> {
        let mut required = 0_u32;
        // SAFETY: the first call intentionally supplies no buffer to obtain
        // the exact required byte count.
        unsafe {
            GetTokenInformation(token, TokenUser, ptr::null_mut(), 0, &mut required);
        }
        if required < u32::try_from(size_of::<TOKEN_USER>()).unwrap_or(u32::MAX) {
            return Err(io::Error::other("Windows token user query failed"));
        }
        let words = usize::try_from(required)
            .ok()
            .and_then(|bytes| bytes.checked_add(size_of::<usize>() - 1))
            .map(|bytes| bytes / size_of::<usize>())
            .ok_or_else(|| io::Error::other("Windows token buffer overflow"))?;
        let mut buffer = vec![0_usize; words];
        // SAFETY: the aligned buffer contains at least `required` writable
        // bytes and remains live while its SID is used by the caller.
        let success = unsafe {
            GetTokenInformation(
                token,
                TokenUser,
                buffer.as_mut_ptr().cast(),
                required,
                &mut required,
            )
        };
        if success == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(TokenUserBuffer { words: buffer })
    }

    fn dacl_has_no_untrusted_writers(dacl: *mut ACL, token_user: PSID) -> io::Result<bool> {
        let mut information = ACL_SIZE_INFORMATION::default();
        // SAFETY: `dacl` is retained by the live security descriptor and the
        // output structure has the exact declared size.
        let success = unsafe {
            GetAclInformation(
                dacl,
                ptr::addr_of_mut!(information).cast(),
                u32::try_from(size_of::<ACL_SIZE_INFORMATION>()).unwrap_or(u32::MAX),
                AclSizeInformation,
            )
        };
        if success == 0 {
            return Err(io::Error::last_os_error());
        }
        for index in 0..information.AceCount {
            let mut ace: *mut c_void = ptr::null_mut();
            // SAFETY: the index is below the count returned for this live ACL.
            if unsafe { GetAce(dacl, index, &mut ace) } == 0 || ace.is_null() {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: every ACL entry begins with an `ACE_HEADER`; unaligned
            // reads avoid assuming allocator alignment beyond the API contract.
            let header = unsafe { ptr::read_unaligned(ace.cast::<ACE_HEADER>()) };
            if header.AceType != ACCESS_ALLOWED_ACE_TYPE as u8 {
                if matches!(
                    header.AceType,
                    ACCESS_DENIED_ACE_TYPE
                        | ACCESS_DENIED_OBJECT_ACE_TYPE
                        | ACCESS_DENIED_CALLBACK_ACE_TYPE
                        | ACCESS_DENIED_CALLBACK_OBJECT_ACE_TYPE
                ) {
                    continue;
                }
                return Ok(false);
            }
            if usize::from(header.AceSize) < size_of::<ACCESS_ALLOWED_ACE>() {
                return Ok(false);
            }
            // SAFETY: the size check covers the fixed fields and the SID begins
            // at the address of `SidStart` by the Windows ACL ABI.
            let allowed = unsafe { ptr::read_unaligned(ace.cast::<ACCESS_ALLOWED_ACE>()) };
            let sid = unsafe {
                ace.cast::<u8>()
                    .add(offset_of!(ACCESS_ALLOWED_ACE, SidStart))
                    .cast::<c_void>()
            };
            // SAFETY: the SID lies within the validated ACE; trusted identities
            // and the live token SID are valid for the duration of this call.
            let trusted = unsafe {
                EqualSid(sid, token_user) != 0
                    || IsWellKnownSid(sid, WinLocalSystemSid) != 0
                    || IsWellKnownSid(sid, WinBuiltinAdministratorsSid) != 0
            };
            if !trusted && allowed.Mask & DANGEROUS_ACCESS != 0 {
                return Ok(false);
            }
        }
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, io::Write};

    use super::*;

    fn config(import: &std::path::Path, export: &std::path::Path) -> ArtifactConfig {
        ArtifactConfig::from_toml(&format!(
            "schema_version = 1\n[spaces]\nread_only = false\n\
             [[roots.import]]\nid = \"inbox\"\npath = {import:?}\n\
             [[roots.export]]\nid = \"outbox\"\npath = {export:?}\n"
        ))
        .expect("root config")
    }

    fn temporary_tree() -> (std::path::PathBuf, std::path::PathBuf, std::path::PathBuf) {
        let base = std::env::temp_dir().join(format!(
            "any-mcp-roots-{}-{}",
            std::process::id(),
            getrandom::u64().unwrap_or(0)
        ));
        let import = base.join("import");
        let export = base.join("export");
        fs::create_dir_all(&import).expect("import directory");
        fs::create_dir_all(&export).expect("export directory");
        (base, import, export)
    }

    #[tokio::test]
    async fn positional_readers_do_not_share_cursor_state() {
        use tokio::io::AsyncReadExt as _;

        let (base, import, _) = temporary_tree();
        let path = import.join("positional.bin");
        fs::write(&path, b"abcdef").expect("write positional fixture");
        let file = File::open(&path).expect("open positional fixture");
        let clone = file.try_clone().expect("clone positional fixture");
        let mut complete = PositionalReader::new(file, 6);
        let mut middle = PositionalReader::range(clone, 2, 3).expect("bounded range");
        let mut complete_bytes = Vec::new();
        let mut middle_bytes = Vec::new();
        complete
            .read_to_end(&mut complete_bytes)
            .await
            .expect("read complete fixture");
        middle
            .read_to_end(&mut middle_bytes)
            .await
            .expect("read fixture range");
        assert_eq!(complete_bytes, b"abcdef");
        assert_eq!(middle_bytes, b"cde");
        fs::remove_dir_all(base).expect("cleanup positional fixture");
    }

    #[tokio::test]
    async fn positional_reader_accepts_a_smaller_buffer_after_pending() {
        use tokio::io::AsyncReadExt as _;

        let (base, import, _) = temporary_tree();
        let path = import.join("positional-repoll.bin");
        fs::write(&path, b"abcdef").expect("write repoll fixture");
        let file = File::open(&path).expect("open repoll fixture");
        let task_file = file.try_clone().expect("clone repoll fixture");
        let (release_tx, release_rx) = std::sync::mpsc::sync_channel(0);
        let task = tokio::task::spawn_blocking(move || {
            let _ = release_rx.recv();
            let mut bytes = vec![0_u8; 6];
            let result = positional_read(&task_file, &mut bytes, 0);
            (bytes, result)
        });
        let mut reader = PositionalReader::new(file, 6);
        reader.state = PositionalReadState::Reading(task);
        let waker = futures::task::noop_waker_ref();
        let mut context = Context::from_waker(waker);
        let mut large = [0_u8; 6];
        let mut large_buffer = ReadBuf::new(&mut large);
        assert!(matches!(
            Pin::new(&mut reader).poll_read(&mut context, &mut large_buffer),
            Poll::Pending
        ));
        release_tx.send(()).expect("release positional read");

        let mut first = [0_u8; 1];
        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            reader.read_exact(&mut first),
        )
        .await
        .expect("smaller-buffer repoll completed")
        .expect("smaller-buffer repoll succeeded");
        assert_eq!(&first, b"a");
        let mut remainder = Vec::new();
        reader
            .read_to_end(&mut remainder)
            .await
            .expect("read buffered remainder");
        assert_eq!(remainder, b"bcdef");
        fs::remove_dir_all(base).expect("cleanup repoll fixture");
    }

    #[tokio::test]
    async fn positional_reader_waits_for_bounded_io_admission() {
        use tokio::io::AsyncReadExt as _;

        let permits = positional_io_semaphore()
            .acquire_many_owned(u32::try_from(POSITIONAL_IO_LIMIT).expect("permit limit"))
            .await
            .expect("reserve positional permits");
        let (base, import, _) = temporary_tree();
        let path = import.join("positional-admission.bin");
        fs::write(&path, b"bounded").expect("write admission fixture");
        let file = File::open(&path).expect("open admission fixture");
        let mut reader = PositionalReader::new(file, 7);
        let waker = futures::task::noop_waker_ref();
        let mut context = Context::from_waker(waker);
        let mut first = [0_u8; 7];
        let mut first_buffer = ReadBuf::new(&mut first);
        assert!(matches!(
            Pin::new(&mut reader).poll_read(&mut context, &mut first_buffer),
            Poll::Pending
        ));
        assert!(matches!(reader.state, PositionalReadState::Waiting { .. }));
        drop(permits);
        let mut observed = Vec::new();
        reader
            .read_to_end(&mut observed)
            .await
            .expect("read after positional admission");
        assert_eq!(observed, b"bounded");
        fs::remove_dir_all(base).expect("cleanup admission fixture");
    }

    #[test]
    fn activates_retained_roots_and_keeps_errors_path_redacted() {
        let (base, import, export) = temporary_tree();
        let config = config(&import, &export);
        let registry = RootRegistry::activate(&config).expect("activate");

        assert_eq!(registry.import_root_count(), 1);
        assert_eq!(registry.export_root_count(), 1);
        assert_eq!(registry.acceptance_access_attempts(), 0);
        assert_eq!(registry.acceptance_successful_import_opens(), 0);
        let missing = RelativeNativePath::from_utf8("missing.bin").expect("relative path");
        assert!(
            registry
                .static_policy()
                .open_import("inbox", &missing, 64)
                .is_err()
        );
        assert_eq!(registry.acceptance_access_attempts(), 1);
        assert_eq!(registry.acceptance_successful_import_opens(), 0);
        assert!(!format!("{registry:?}").contains(base.to_string_lossy().as_ref()));
        fs::remove_dir_all(base).expect("cleanup");
    }

    #[cfg(unix)]
    #[test]
    fn import_walk_is_anchored_bounded_and_rejects_symlinks() {
        use std::{io::Read, os::unix::fs::symlink};

        let (base, import, export) = temporary_tree();
        fs::create_dir(import.join("safe")).expect("safe directory");
        fs::write(import.join("safe/report.bin"), b"artifact").expect("artifact");
        symlink("/etc/passwd", import.join("safe/link")).expect("symlink");
        let effective = RootRegistry::activate(&config(&import, &export))
            .expect("activate")
            .static_policy();

        let mut opened = effective
            .open_import(
                "inbox",
                &RelativeNativePath::from_utf8("safe/report.bin").expect("path"),
                64,
            )
            .expect("open import");
        let mut bytes = Vec::new();
        opened.reader().read_to_end(&mut bytes).expect("read");
        assert_eq!(bytes, b"artifact");
        let mut cloned = opened.try_clone_reader().expect("rewound clone");
        bytes.clear();
        cloned.read_to_end(&mut bytes).expect("read clone");
        assert_eq!(bytes, b"artifact");
        opened.verify_unchanged().expect("unchanged");
        assert!(
            effective
                .open_import(
                    "inbox",
                    &RelativeNativePath::from_utf8("safe/link").expect("path"),
                    64,
                )
                .is_err()
        );
        assert!(
            effective
                .open_import(
                    "inbox",
                    &RelativeNativePath::from_utf8("safe/report.bin").expect("path"),
                    1,
                )
                .is_err()
        );
        fs::remove_dir_all(base).expect("cleanup");
    }

    #[test]
    fn export_is_atomic_create_new_and_capabilities_do_not_cross() {
        let (base, import, export) = temporary_tree();
        let effective = RootRegistry::activate(&config(&import, &export))
            .expect("activate")
            .static_policy();
        let path = RelativeNativePath::from_utf8("new.bin").expect("path");
        let mut pending = effective
            .begin_atomic_export("outbox", &path, 64)
            .expect("begin export");
        pending.write_all(b"new").expect("write");
        assert!(!export.join("new.bin").exists());
        assert_eq!(pending.commit().expect("commit"), 3);

        assert!(effective.begin_atomic_export("outbox", &path, 64).is_err());
        assert!(effective.begin_atomic_export("inbox", &path, 64).is_err());
        assert!(
            effective
                .open_import("outbox", &path, 64)
                .expect_err("wrong capability")
                .to_string()
                .contains("not authorized")
        );
        assert_eq!(fs::read(export.join("new.bin")).expect("read"), b"new");
        fs::remove_dir_all(base).expect("cleanup");
    }

    #[test]
    fn atomic_export_cleans_partial_and_lost_collision_race() {
        let (base, import, export) = temporary_tree();
        let effective = RootRegistry::activate(&config(&import, &export))
            .expect("activate")
            .static_policy();
        let path = RelativeNativePath::from_utf8("race.bin").expect("path");

        {
            let mut partial = effective
                .begin_atomic_export("outbox", &path, 4)
                .expect("begin partial");
            partial.write_all(b"part").expect("bounded partial");
        }
        assert!(!export.join("race.bin").exists());
        assert_eq!(fs::read_dir(&export).expect("list export").count(), 0);

        {
            let mut oversized = effective
                .begin_atomic_export("outbox", &path, 4)
                .expect("begin bounded export");
            assert!(oversized.write_all(b"large").is_err());
        }
        assert_eq!(
            fs::read_dir(&export).expect("list bounded export").count(),
            0
        );

        let mut pending = effective
            .begin_atomic_export("outbox", &path, 4)
            .expect("begin race");
        pending.write_all(b"ours").expect("write ours");
        fs::write(export.join("race.bin"), b"other").expect("racing destination");
        let error = pending.commit().expect_err("collision must win");
        assert_eq!(
            error.to_string(),
            "Artifact export destination already exists."
        );
        assert_eq!(
            fs::read(export.join("race.bin")).expect("read winner"),
            b"other"
        );
        assert_eq!(fs::read_dir(&export).expect("list export").count(), 1);
        fs::remove_dir_all(base).expect("cleanup");
    }

    #[cfg(unix)]
    #[test]
    fn import_revalidation_rejects_namespace_rename_over() {
        let (base, import, export) = temporary_tree();
        let source_path = import.join("source.bin");
        fs::write(&source_path, b"original").expect("write original source");
        let effective = RootRegistry::activate(&config(&import, &export))
            .expect("activate")
            .static_policy();
        let source = effective
            .open_import(
                "inbox",
                &RelativeNativePath::from_utf8("source.bin").expect("source path"),
                64,
            )
            .expect("open source");

        fs::rename(&source_path, import.join("replaced-source.bin")).expect("rename source");
        fs::write(&source_path, b"replacement").expect("write replacement source");

        assert!(source.verify_unchanged().is_err());
        fs::remove_dir_all(base).expect("cleanup");
    }

    #[cfg(unix)]
    #[test]
    fn atomic_export_rejects_parent_moved_outside_retained_root() {
        let (base, import, export) = temporary_tree();
        let original_parent = export.join("safe");
        fs::create_dir(&original_parent).expect("create export parent");
        let effective = RootRegistry::activate(&config(&import, &export))
            .expect("activate")
            .static_policy();
        let path = RelativeNativePath::from_utf8("safe/result.bin").expect("export path");
        let mut pending = effective
            .begin_atomic_export("outbox", &path, 64)
            .expect("begin export");
        pending.write_all(b"private bytes").expect("write export");

        let moved_parent = base.join("moved-parent");
        fs::rename(&original_parent, &moved_parent).expect("move export parent");
        fs::create_dir(&original_parent).expect("replace export parent");

        assert!(pending.commit().is_err());
        assert!(!moved_parent.join("result.bin").exists());
        assert!(!original_parent.join("result.bin").exists());
        drop(effective);
        fs::remove_dir_all(base).expect("cleanup");
    }

    #[cfg(unix)]
    #[test]
    fn atomic_export_mismatch_is_indeterminate_when_cleanup_is_not_proven() {
        let (base, import, export) = temporary_tree();
        let effective = RootRegistry::activate(&config(&import, &export))
            .expect("activate")
            .static_policy();
        let path = RelativeNativePath::from_utf8("result.bin").expect("export path");
        let mut pending = effective
            .begin_atomic_export("outbox", &path, 64)
            .expect("begin export");
        pending.write_all(b"private bytes").expect("write export");
        let retained_link = base.join("retained-link.bin");
        fs::hard_link(export.join(&pending.temporary_name), &retained_link)
            .expect("retain unexpected private link");

        assert_eq!(
            pending.settle_namespace_mismatch().kind(),
            RootAccessErrorKind::Indeterminate
        );
        assert!(export.join(&pending.temporary_name).is_file());
        assert!(retained_link.is_file());
        drop(pending);
        drop(effective);
        fs::remove_dir_all(base).expect("cleanup");
    }

    #[cfg(unix)]
    #[test]
    fn atomic_export_preserves_retained_whole_root_identity() {
        let (base, import, export) = temporary_tree();
        let effective = RootRegistry::activate(&config(&import, &export))
            .expect("activate")
            .static_policy();
        let path = RelativeNativePath::from_utf8("result.bin").expect("export path");
        let mut pending = effective
            .begin_atomic_export("outbox", &path, 64)
            .expect("begin export");
        pending
            .write_all(b"retained root bytes")
            .expect("write export");

        let retained_export = base.join("retained-export");
        fs::rename(&export, &retained_export).expect("retain opened export root");
        fs::create_dir(&export).expect("create replacement export root");

        assert_eq!(pending.commit(), Ok(19));
        assert_eq!(
            fs::read(retained_export.join("result.bin")).expect("read retained publication"),
            b"retained root bytes"
        );
        assert!(
            fs::read_dir(&export)
                .expect("inspect replacement root")
                .next()
                .is_none()
        );
        drop(effective);
        fs::remove_dir_all(base).expect("cleanup");
    }

    #[test]
    fn staging_activation_creates_closed_layout_and_locks_one_generation() {
        let (base, import, export) = temporary_tree();
        let staging = base.join("staging");
        fs::create_dir(&staging).expect("staging directory");
        let registry = RootRegistry::activate(&config(&import, &export)).expect("activate roots");
        let path = AbsoluteNativePath::from_utf8(
            staging.to_str().expect("temporary staging path is UTF-8"),
        )
        .expect("staging path");
        let (first, inventory) =
            StagingDirectory::activate(&path, &registry, 4, 1024).expect("first generation");
        assert!(inventory.records.is_empty());
        assert!(inventory.payloads.is_empty());
        assert!(inventory.temporary.is_empty());
        assert!(inventory.tombstones.is_empty());
        assert!(staging.join(STAGING_LOCK_NAME).is_file());
        for name in [
            STAGING_RECORDS_DIRECTORY,
            STAGING_PAYLOADS_DIRECTORY,
            STAGING_TEMPORARY_DIRECTORY,
            STAGING_TOMBSTONES_DIRECTORY,
        ] {
            assert!(staging.join(name).is_dir());
        }
        assert!(StagingDirectory::activate(&path, &registry, 4, 1024).is_err());
        drop(first);
        let (second, _) =
            StagingDirectory::activate(&path, &registry, 4, 1024).expect("next generation");
        drop(second);
        drop(registry);
        fs::remove_dir_all(base).expect("cleanup");
    }

    #[cfg(unix)]
    #[test]
    fn staging_activation_refuses_unknown_entry_without_deleting_it() {
        let (base, import, export) = temporary_tree();
        let staging = base.join("staging");
        fs::create_dir(&staging).expect("staging directory");
        let unknown = staging.join("operator-file");
        fs::write(&unknown, b"ambiguous").expect("unknown staging entry");
        let registry = RootRegistry::activate(&config(&import, &export)).expect("activate roots");
        let path = AbsoluteNativePath::from_utf8(
            staging.to_str().expect("temporary staging path is UTF-8"),
        )
        .expect("staging path");

        assert!(StagingDirectory::activate(&path, &registry, 4, 1024).is_err());
        assert_eq!(
            fs::read(&unknown).expect("unknown entry retained"),
            b"ambiguous"
        );
        fs::remove_dir_all(base).expect("cleanup");
    }

    #[cfg(unix)]
    #[test]
    fn retained_root_identity_survives_namespace_replacement() {
        use std::io::Read;

        let (base, import, export) = temporary_tree();
        fs::write(import.join("source.bin"), b"retained").expect("retained source");
        let effective = RootRegistry::activate(&config(&import, &export))
            .expect("activate")
            .static_policy();

        let moved = base.join("moved-import");
        fs::rename(&import, &moved).expect("rename authorized root");
        fs::create_dir(&import).expect("replacement root");
        fs::write(import.join("source.bin"), b"replacement").expect("replacement source");

        let mut opened = effective
            .open_import(
                "inbox",
                &RelativeNativePath::from_utf8("source.bin").expect("path"),
                64,
            )
            .expect("open through retained root");
        let mut bytes = Vec::new();
        opened
            .reader()
            .read_to_end(&mut bytes)
            .expect("read retained");
        assert_eq!(bytes, b"retained");
        opened.verify_unchanged().expect("retained identity");
        fs::remove_dir_all(base).expect("cleanup");
    }

    #[test]
    fn client_roots_only_narrow_static_authority() {
        let (base, import, export) = temporary_tree();
        let registry = RootRegistry::activate(&config(&import, &export)).expect("activate");
        let client = AbsoluteNativePath::from_utf8(import.to_str().expect("test path is UTF-8"))
            .expect("client root");
        let narrowed = registry
            .intersect_client_roots(&[client])
            .expect("intersection");
        fs::write(import.join("source.bin"), b"source").expect("source");
        assert!(
            narrowed
                .open_import(
                    "inbox",
                    &RelativeNativePath::from_utf8("source.bin").expect("path"),
                    64,
                )
                .is_ok()
        );
        assert!(
            narrowed
                .begin_atomic_export(
                    "outbox",
                    &RelativeNativePath::from_utf8("denied.bin").expect("path"),
                    64,
                )
                .is_err()
        );
        let denied = registry
            .intersect_client_roots(&[])
            .expect("empty intersection");
        assert!(
            denied
                .open_import(
                    "inbox",
                    &RelativeNativePath::from_utf8("source.bin").expect("path"),
                    64,
                )
                .is_err()
        );
        fs::remove_dir_all(base).expect("cleanup");
    }

    #[test]
    fn missing_roots_explain_how_to_select_a_config() {
        let error = RootRegistry::default()
            .static_policy()
            .open_import(
                "inbox",
                &RelativeNativePath::from_utf8("file.bin").expect("path"),
                64,
            )
            .expect_err("no roots");
        assert_eq!(error.to_string(), ROOTS_REQUIRED_GUIDANCE);
    }
}
