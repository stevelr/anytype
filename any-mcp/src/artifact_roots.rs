// any-mcp - bounded, workflow-oriented MCP server for Anytype
//
// SPDX-FileCopyrightText: 2026 Steve Schoettler
// SPDX-License-Identifier: Apache-2.0

//! Opened filesystem capabilities for local artifact operations.
//!
//! Root paths are used only during activation. The registry retains opened
//! directory handles, and Unix operations walk validated relative components
//! with `openat` and `O_NOFOLLOW`. Absolute physical paths are never retained
//! in errors, receipts, or debug output.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    fs::File,
    io,
    sync::Arc,
};

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

    /// Creates one new export file beneath an authorized retained root.
    ///
    /// This low-level primitive is create-new only. Higher artifact workflows
    /// write a private same-directory temporary and atomically publish it;
    /// callers cannot request replacement.
    ///
    /// # Errors
    ///
    /// Returns fixed guidance/authorization/containment/collision errors.
    pub fn create_new_export(
        &self,
        root: &str,
        path: &RelativeNativePath,
    ) -> Result<File, RootAccessError> {
        let capability = self.authorize(root, RootCapabilityKind::Export)?;
        create_export_at(capability, path)
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
        let snapshot = FileSnapshot::from_metadata(&metadata)
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
            #[cfg(not(unix))]
            RootProblem::Platform => "Artifact root controls are unavailable on this platform.",
        })
    }
}

impl std::error::Error for RootAccessError {}

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
    #[cfg(not(unix))]
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
}

impl fmt::Debug for OpenedDirectory {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("OpenedDirectory(<capability>)")
    }
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
    fn from_metadata(metadata: &std::fs::Metadata) -> io::Result<Self> {
        use std::os::unix::fs::MetadataExt;

        Ok(Self {
            identity: FileIdentity::from_metadata(metadata)?,
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
    fn from_metadata(metadata: &std::fs::Metadata) -> io::Result<Self> {
        use std::os::windows::fs::MetadataExt;

        Ok(Self {
            identity: FileIdentity::from_metadata(metadata)?,
            length: metadata.file_size(),
            modified: i128::from(metadata.last_write_time()),
            changed: i128::from(metadata.creation_time()),
        })
    }
}

#[cfg(not(any(unix, windows)))]
impl FileSnapshot {
    fn from_metadata(_: &std::fs::Metadata) -> io::Result<Self> {
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
impl FileIdentity {
    fn from_metadata(metadata: &std::fs::Metadata) -> io::Result<Self> {
        use std::os::unix::fs::MetadataExt;

        Ok(Self {
            volume: metadata.dev(),
            file: metadata.ino(),
        })
    }
}

#[cfg(windows)]
impl FileIdentity {
    fn from_metadata(metadata: &std::fs::Metadata) -> io::Result<Self> {
        use std::os::windows::fs::MetadataExt;

        let volume = metadata
            .volume_serial_number()
            .ok_or_else(|| io::Error::other("missing volume identity"))?;
        let file = metadata
            .file_index()
            .ok_or_else(|| io::Error::other("missing file identity"))?;
        Ok(Self {
            volume: u64::from(volume),
            file,
        })
    }
}

#[cfg(not(any(unix, windows)))]
impl FileIdentity {
    fn from_metadata(_: &std::fs::Metadata) -> io::Result<Self> {
        Err(io::Error::other("unsupported platform"))
    }
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
    let identity = FileIdentity::from_metadata(&metadata)?;
    Ok(OpenedDirectory {
        file: Arc::new(current),
        identity,
    })
}

#[cfg(windows)]
fn open_root(path: &AbsoluteNativePath) -> io::Result<OpenedDirectory> {
    use std::{
        fs::OpenOptions,
        os::windows::fs::{MetadataExt, OpenOptionsExt},
    };

    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
    let mut options = OpenOptions::new();
    options
        .read(true)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT);
    let file = options.open(path.as_path())?;
    let metadata = file.metadata()?;
    if !metadata.is_dir() || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(io::Error::other("unsafe root"));
    }
    let identity = FileIdentity::from_metadata(&metadata)?;
    Ok(OpenedDirectory {
        file: Arc::new(file),
        identity,
    })
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
        let identity = FileIdentity::from_metadata(&parent.metadata()?)?;
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
    if candidate.identity == descendant.identity {
        Ok(true)
    } else {
        Err(io::Error::other(
            "secure Windows ancestry walk is unavailable",
        ))
    }
}

#[cfg(not(any(unix, windows)))]
fn is_ancestor_identity(_: &OpenedDirectory, _: &OpenedDirectory) -> io::Result<bool> {
    Err(io::Error::other("unsupported platform"))
}

#[cfg(unix)]
fn open_import_at(
    root: &RootCapability,
    path: &RelativeNativePath,
    maximum_bytes: u64,
) -> Result<AnchoredImport, RootAccessError> {
    use std::{
        ffi::CString,
        os::{
            fd::{AsRawFd, FromRawFd},
            unix::{ffi::OsStrExt, fs::MetadataExt},
        },
    };

    let (parent, name) = walk_parent(root, path)?;
    let name = CString::new(name.as_bytes())
        .map_err(|_| RootAccessError::new(RootProblem::Containment))?;
    // SAFETY: parent is a retained directory and name is one validated native
    // component. O_NOFOLLOW rejects a substituted final symlink.
    let descriptor = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if descriptor < 0 {
        return Err(RootAccessError::new(RootProblem::Containment));
    }
    // SAFETY: `openat` returned a new owned descriptor.
    let file = unsafe { File::from_raw_fd(descriptor) };
    let metadata = file
        .metadata()
        .map_err(|_| RootAccessError::new(RootProblem::Containment))?;
    // SAFETY: `geteuid` has no memory or ownership preconditions.
    let effective_user = unsafe { libc::geteuid() };
    if !metadata.is_file()
        || metadata.uid() != effective_user
        || metadata.mode() & 0o022 != 0
        || metadata.nlink() != 1
    {
        return Err(RootAccessError::new(RootProblem::Containment));
    }
    if metadata.len() > maximum_bytes {
        return Err(RootAccessError::new(RootProblem::TooLarge));
    }
    let snapshot = FileSnapshot::from_metadata(&metadata)
        .map_err(|_| RootAccessError::new(RootProblem::Containment))?;
    Ok(AnchoredImport {
        file,
        length: metadata.len(),
        snapshot,
    })
}

#[cfg(unix)]
fn create_export_at(
    root: &RootCapability,
    path: &RelativeNativePath,
) -> Result<File, RootAccessError> {
    use std::{
        ffi::CString,
        os::{
            fd::{AsRawFd, FromRawFd},
            unix::ffi::OsStrExt,
        },
    };

    let (parent, name) = walk_parent(root, path)?;
    let name = CString::new(name.as_bytes())
        .map_err(|_| RootAccessError::new(RootProblem::Containment))?;
    // SAFETY: parent is a retained directory and name is one validated native
    // component. O_EXCL makes collision handling create-new only.
    let descriptor = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            0o600,
        )
    };
    if descriptor < 0 {
        let error = io::Error::last_os_error();
        return if error.kind() == io::ErrorKind::AlreadyExists {
            Err(RootAccessError::new(RootProblem::Collision))
        } else {
            Err(RootAccessError::new(RootProblem::Containment))
        };
    }
    // SAFETY: `openat` returned a new owned descriptor.
    Ok(unsafe { File::from_raw_fd(descriptor) })
}

#[cfg(unix)]
fn walk_parent(
    root: &RootCapability,
    path: &RelativeNativePath,
) -> Result<(File, std::ffi::OsString), RootAccessError> {
    use std::{
        ffi::CString,
        os::{
            fd::{AsRawFd, FromRawFd},
            unix::{ffi::OsStrExt, fs::MetadataExt},
        },
    };

    let mut components = path.as_path().components().peekable();
    let mut current = root
        .directory
        .file
        .try_clone()
        .map_err(|_| RootAccessError::new(RootProblem::Containment))?;
    while let Some(component) = components.next() {
        let std::path::Component::Normal(component) = component else {
            return Err(RootAccessError::new(RootProblem::Containment));
        };
        if components.peek().is_none() {
            return Ok((current, component.to_os_string()));
        }
        let component = CString::new(component.as_bytes())
            .map_err(|_| RootAccessError::new(RootProblem::Containment))?;
        // SAFETY: current is a retained directory and component is one
        // validated NUL-terminated name.
        let descriptor = unsafe {
            libc::openat(
                current.as_raw_fd(),
                component.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if descriptor < 0 {
            return Err(RootAccessError::new(RootProblem::Containment));
        }
        // SAFETY: `openat` returned a new owned descriptor.
        current = unsafe { File::from_raw_fd(descriptor) };
        let metadata = current
            .metadata()
            .map_err(|_| RootAccessError::new(RootProblem::Containment))?;
        // SAFETY: `geteuid` has no memory or ownership preconditions.
        let effective_user = unsafe { libc::geteuid() };
        if !metadata.is_dir()
            || metadata.dev() != root.directory.identity.volume
            || metadata.uid() != effective_user
            || metadata.mode() & 0o022 != 0
        {
            return Err(RootAccessError::new(RootProblem::Containment));
        }
    }
    Err(RootAccessError::new(RootProblem::Containment))
}

#[cfg(windows)]
fn open_import_at(
    _: &RootCapability,
    _: &RelativeNativePath,
    _: u64,
) -> Result<AnchoredImport, RootAccessError> {
    Err(RootAccessError::new(RootProblem::Platform))
}

#[cfg(windows)]
fn create_export_at(_: &RootCapability, _: &RelativeNativePath) -> Result<File, RootAccessError> {
    Err(RootAccessError::new(RootProblem::Platform))
}

#[cfg(not(any(unix, windows)))]
fn open_import_at(
    _: &RootCapability,
    _: &RelativeNativePath,
    _: u64,
) -> Result<AnchoredImport, RootAccessError> {
    Err(RootAccessError::new(RootProblem::Platform))
}

#[cfg(not(any(unix, windows)))]
fn create_export_at(_: &RootCapability, _: &RelativeNativePath) -> Result<File, RootAccessError> {
    Err(RootAccessError::new(RootProblem::Platform))
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        io::{Read, Write},
    };

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

    #[cfg(unix)]
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
        use std::os::unix::fs::symlink;

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

    #[cfg(unix)]
    #[test]
    fn export_is_create_new_and_capabilities_do_not_cross() {
        let (base, import, export) = temporary_tree();
        let effective = RootRegistry::activate(&config(&import, &export))
            .expect("activate")
            .static_policy();
        let path = RelativeNativePath::from_utf8("new.bin").expect("path");
        let mut file = effective
            .create_new_export("outbox", &path)
            .expect("create export");
        file.write_all(b"new").expect("write");
        drop(file);

        assert!(effective.create_new_export("outbox", &path).is_err());
        assert!(effective.create_new_export("inbox", &path).is_err());
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

    #[cfg(unix)]
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
                .create_new_export(
                    "outbox",
                    &RelativeNativePath::from_utf8("denied.bin").expect("path"),
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
