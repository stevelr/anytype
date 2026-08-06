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
    io::{self, Write},
    path::Path,
    sync::Arc,
};

#[cfg(unix)]
use cap_fs_ext::OpenOptionsExt;
use cap_fs_ext::{FollowSymlinks, OpenOptionsFollowExt, OpenOptionsMaybeDirExt};
use cap_std::fs::{Dir, OpenOptions};
use fs2::FileExt;

use crate::artifact_config::{
    AbsoluteNativePath, ArtifactConfig, LogicalRootId, RelativeNativePath, RootDefinition,
};

/// Fixed guidance returned when an operation requires an undeclared root.
pub const ROOTS_REQUIRED_GUIDANCE: &str = "No artifact roots are configured. Declare roots in an any-mcp TOML config and select it with ANY_MCP_CONFIG or --config.";

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
        let capability = self.authorize(root, RootCapabilityKind::Import)?;
        open_import_at(capability, path, maximum_bytes)
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
    root: RootCapability,
    _instance_lock: Arc<File>,
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
    ) -> Result<Self, RootAccessError> {
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
        let instance_lock = acquire_staging_lock(&root)?;
        cleanup_stale_staging_files(&root)?;
        Ok(Self {
            root,
            _instance_lock: Arc::new(instance_lock),
        })
    }

    /// Reserves one private create-new record destination.
    pub(crate) fn begin_record(
        &self,
        record_name: &str,
        maximum_bytes: u64,
    ) -> Result<AtomicExport, RootAccessError> {
        let path = RelativeNativePath::from_utf8(record_name)
            .map_err(|_| RootAccessError::new(RootProblem::Containment))?;
        begin_atomic_export_at(&self.root, &path, maximum_bytes)
    }

    /// Removes one exact completed record.
    pub(crate) fn remove_record(&self, record_name: &str) -> Result<(), RootAccessError> {
        let path = RelativeNativePath::from_utf8(record_name)
            .map_err(|_| RootAccessError::new(RootProblem::Containment))?;
        let (parent, name) = walk_parent(&self.root, &path)?;
        parent
            .remove_file(Path::new(&name))
            .map_err(|_| RootAccessError::new(RootProblem::Containment))?;
        sync_parent_directory(&parent).map_err(|_| RootAccessError::new(RootProblem::Containment))
    }
}

const STAGING_LOCK_NAME: &str = ".any-mcp-staging.lock";

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

fn cleanup_stale_staging_files(root: &RootCapability) -> Result<(), RootAccessError> {
    let directory = retained_directory(&root.directory)?;
    let entries = directory
        .entries()
        .map_err(|_| RootAccessError::new(RootProblem::Activation))?;
    for entry in entries {
        let entry = entry.map_err(|_| RootAccessError::new(RootProblem::Activation))?;
        let name = entry.file_name();
        if name == STAGING_LOCK_NAME {
            continue;
        }
        let Some(name_utf8) = name.to_str() else {
            return Err(RootAccessError::new(RootProblem::Activation));
        };
        if !stale_staging_name(name_utf8) {
            return Err(RootAccessError::new(RootProblem::Activation));
        }
        let metadata = directory
            .symlink_metadata(Path::new(&name))
            .map_err(|_| RootAccessError::new(RootProblem::Activation))?;
        if !metadata.is_file() {
            return Err(RootAccessError::new(RootProblem::Activation));
        }
        directory
            .remove_file(Path::new(&name))
            .map_err(|_| RootAccessError::new(RootProblem::Activation))?;
    }
    sync_parent_directory(&directory).map_err(|_| RootAccessError::new(RootProblem::Activation))
}

fn retained_directory(directory: &OpenedDirectory) -> Result<Dir, RootAccessError> {
    directory
        .file
        .try_clone()
        .map(Dir::from_std_file)
        .map_err(|_| RootAccessError::new(RootProblem::Activation))
}

fn stale_staging_name(name: &str) -> bool {
    let record = name
        .strip_suffix(".bin")
        .is_some_and(|stem| stem.len() == 32 && stem.bytes().all(lowercase_hex));
    let temporary = name
        .strip_prefix(".any-mcp-")
        .and_then(|name| name.strip_suffix(".tmp"))
        .is_some_and(|stem| stem.len() == 16 && stem.bytes().all(lowercase_hex));
    record || temporary
}

fn lowercase_hex(byte: u8) -> bool {
    byte.is_ascii_digit() || matches!(byte, b'a'..=b'f')
}

/// One bounded same-directory export which is invisible until commit.
pub struct AtomicExport {
    parent: Dir,
    file: Option<File>,
    temporary_name: OsString,
    destination_name: OsString,
    maximum_bytes: u64,
    written: u64,
    published: bool,
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
    /// Clones a read handle for pre-publication verification.
    pub(crate) fn try_clone_reader(&self) -> io::Result<File> {
        self.file
            .as_ref()
            .ok_or_else(|| io::Error::other("artifact export is not readable"))?
            .try_clone()
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
            || !safe_windows_security(&published)
        {
            return Err(RootAccessError::new(RootProblem::Indeterminate));
        }
        drop(file);
        if self
            .parent
            .remove_file(Path::new(&self.temporary_name))
            .is_err()
        {
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
        })
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
        let _ = self.parent.remove_file(Path::new(&self.temporary_name));
    }
}

/// An opened, preflighted import source.
pub struct AnchoredImport {
    file: File,
    /// Source length observed after the anchored no-follow open.
    pub length: u64,
    snapshot: FileSnapshot,
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
        self.file
            .try_clone()
            .map_err(|_| RootAccessError::new(RootProblem::Containment))
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
                    file: Some(file),
                    temporary_name,
                    destination_name,
                    maximum_bytes,
                    written: 0,
                    published: false,
                });
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(_) => return Err(RootAccessError::new(RootProblem::Containment)),
        }
    }
    Err(RootAccessError::new(RootProblem::Containment))
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
    use std::os::unix::fs::MetadataExt;

    // SAFETY: `geteuid` has no memory or ownership preconditions.
    let effective_user = unsafe { libc::geteuid() };
    metadata.is_file()
        && metadata.uid() == effective_user
        && metadata.mode() & 0o077 == 0
        && metadata.nlink() == 1
}

#[cfg(windows)]
fn safe_created_export_metadata(file: &File, metadata: &std::fs::Metadata) -> bool {
    safe_import_metadata(file, metadata)
}

#[cfg(not(any(unix, windows)))]
fn safe_created_export_metadata(_: &File, _: &std::fs::Metadata) -> bool {
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

    #[test]
    fn activates_retained_roots_and_keeps_errors_path_redacted() {
        let (base, import, export) = temporary_tree();
        let config = config(&import, &export);
        let registry = RootRegistry::activate(&config).expect("activate");

        assert_eq!(registry.import_root_count(), 1);
        assert_eq!(registry.export_root_count(), 1);
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

    #[test]
    fn staging_activation_locks_one_generation_and_reconciles_private_files() {
        let (base, import, export) = temporary_tree();
        let staging = base.join("staging");
        fs::create_dir(&staging).expect("staging directory");
        fs::write(
            staging.join("0123456789abcdef0123456789abcdef.bin"),
            b"stale",
        )
        .expect("stale record");
        fs::write(staging.join(".any-mcp-0123456789abcdef.tmp"), b"partial")
            .expect("stale temporary");
        let registry = RootRegistry::activate(&config(&import, &export)).expect("activate roots");
        let path = AbsoluteNativePath::from_utf8(
            staging.to_str().expect("temporary staging path is UTF-8"),
        )
        .expect("staging path");
        let first = StagingDirectory::activate(&path, &registry).expect("first generation");
        assert!(
            !staging
                .join("0123456789abcdef0123456789abcdef.bin")
                .exists()
        );
        assert!(!staging.join(".any-mcp-0123456789abcdef.tmp").exists());
        assert!(staging.join(STAGING_LOCK_NAME).is_file());
        assert!(StagingDirectory::activate(&path, &registry).is_err());
        drop(first);
        let second = StagingDirectory::activate(&path, &registry).expect("next generation");
        drop(second);
        drop(registry);
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
