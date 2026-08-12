// any-mcp - bounded, workflow-oriented MCP server for Anytype
//
// SPDX-FileCopyrightText: 2026 Steve Schoettler
// SPDX-License-Identifier: Apache-2.0

//! Session-scoped MCP client-root narrowing for local artifact operations.
//!
//! Stable stdio serves exactly one initialized session per process, so the
//! server may take one bounded `roots/list` snapshot and use it as a
//! session-specific narrowing layer over the static TOML root policy. The
//! snapshot can only remove static authority; it never adds any.
//!
//! A client that advertises no roots capability keeps the configured static
//! policy. An unusable snapshot — transport failure, timeout, oversize,
//! duplicate, or unparsable entry — freezes local root operations as disabled
//! for the whole session and never falls back to broader static roots.
//!
//! Client root URIs and decoded paths never enter diagnostics or receipts.
//!
//! rmcp marks the whole roots wire model deprecated ahead of SEP-2577, but it
//! is the only released mechanism for client-supplied filesystem roots, so
//! this module — and only this module — opts out of that deprecation. The
//! opt-out is item-scoped to the declarations that name the deprecated wire
//! types, so unrelated deprecations in this file still warn.

use std::{
    fmt,
    future::Future,
    pin::Pin,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

#[allow(deprecated)]
use rmcp::model::Root;
use rmcp::{RoleServer, service::Peer};
use tokio::sync::OnceCell;
use url::Url;

use crate::{
    artifact_config::AbsoluteNativePath,
    artifact_roots::{EffectiveRootRegistry, RootAccessError, RootRegistry},
};

/// Maximum client roots accepted in one snapshot.
pub(crate) const MAX_CLIENT_ROOTS: usize = 64;

/// Maximum accepted length, in bytes, of one client root URI.
const MAX_ROOT_URI_BYTES: usize = 4096;

/// Boxed future returned by a client-root snapshot source.
#[allow(deprecated)]
type SnapshotFuture<'a> = Pin<Box<dyn Future<Output = Result<Vec<Root>, ()>> + Send + 'a>>;

/// One session's terminal source of MCP client roots.
///
/// The production implementation wraps the initialized rmcp peer. Tests
/// substitute a scripted source so the whole narrowing decision is verifiable
/// without a transport.
pub(crate) trait ClientRootsSource: Send + Sync {
    /// Returns whether the initialized client advertised the roots capability.
    fn advertises_roots(&self) -> bool;

    /// Requests exactly one `roots/list` snapshot.
    fn list_roots(&self) -> SnapshotFuture<'_>;
}

/// Production snapshot source backed by the initialized rmcp peer.
pub(crate) struct PeerRootsSource(Peer<RoleServer>);

impl PeerRootsSource {
    /// Wraps one initialized peer as a snapshot source.
    pub(crate) const fn new(peer: Peer<RoleServer>) -> Self {
        Self(peer)
    }
}

#[allow(deprecated)]
impl ClientRootsSource for PeerRootsSource {
    fn advertises_roots(&self) -> bool {
        self.0
            .peer_info()
            .is_some_and(|info| info.capabilities.roots.is_some())
    }

    fn list_roots(&self) -> SnapshotFuture<'_> {
        Box::pin(async move {
            self.0
                .list_roots()
                .await
                .map(|result| result.roots)
                .map_err(|_| ())
        })
    }
}

/// Session-scoped client-root narrowing gate shared by artifact handlers.
///
/// The gate is inert unless a transport enables it, so transports that do not
/// carry a single terminal client session keep the static policy unchanged.
#[derive(Default)]
pub(crate) struct ClientRootsGate {
    enabled: AtomicBool,
    source: OnceLock<Arc<dyn ClientRootsSource>>,
    decision: OnceCell<Option<EffectiveRootRegistry>>,
}

impl fmt::Debug for ClientRootsGate {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ClientRootsGate")
            .field("enabled", &self.is_enabled())
            .field("source_installed", &self.source.get().is_some())
            .field("decision_frozen", &self.decision.initialized())
            .finish()
    }
}

impl ClientRootsGate {
    /// Enables client-root narrowing for this session.
    ///
    /// Only a transport that serves exactly one terminal client session may
    /// call this. The call is idempotent.
    pub(crate) fn enable(&self) {
        self.enabled.store(true, Ordering::Release);
    }

    /// Returns whether client-root narrowing is active for this session.
    pub(crate) fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Acquire)
    }

    /// Installs the terminal snapshot source exactly once.
    ///
    /// Later installs are ignored, so a session cannot be re-pointed at a
    /// different peer after its snapshot decision is taken.
    pub(crate) fn install_source(&self, source: Arc<dyn ClientRootsSource>) {
        if self.is_enabled() {
            let _ = self.source.set(source);
        }
    }

    /// Installs the initialized rmcp peer as this session's snapshot source.
    pub(crate) fn install_peer(&self, peer: &Peer<RoleServer>) {
        if self.is_enabled() && self.source.get().is_none() {
            self.install_source(Arc::new(PeerRootsSource::new(peer.clone())));
        }
    }

    /// Resolves the effective root authority for one local artifact operation.
    ///
    /// The snapshot is taken at most once per session and its outcome is
    /// frozen, so repeated operations neither re-query the client nor observe
    /// a widened authority.
    ///
    /// # Errors
    ///
    /// Returns the fixed client-root failure when the snapshot could not be
    /// securely frozen. The caller must not fall back to static policy.
    pub(crate) async fn effective(
        &self,
        registry: &RootRegistry,
        timeout: Duration,
    ) -> Result<EffectiveRootRegistry, RootAccessError> {
        if !self.is_enabled() {
            return Ok(registry.static_policy());
        }
        self.decision
            .get_or_init(|| self.resolve(registry, timeout))
            .await
            .clone()
            .ok_or_else(RootAccessError::client_roots)
    }

    async fn resolve(
        &self,
        registry: &RootRegistry,
        timeout: Duration,
    ) -> Option<EffectiveRootRegistry> {
        let source = self.source.get()?;
        if !source.advertises_roots() {
            return Some(registry.static_policy());
        }
        let roots = tokio::time::timeout(timeout, source.list_roots())
            .await
            .ok()?
            .ok()?;
        let paths = parse_client_root_snapshot(&roots).ok()?;
        // The intersection opens and walks every client root, so it runs on a
        // blocking worker like every other artifact filesystem operation and a
        // stalled mount cannot occupy an async worker thread. A blocking-worker
        // failure is treated as an unusable snapshot, so the session denies
        // local roots rather than falling back to the broader static policy.
        let registry = registry.clone();
        tokio::task::spawn_blocking(move || registry.intersect_client_roots(&paths).ok())
            .await
            .ok()?
    }
}

/// Fixed, path-free client-root snapshot rejection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ClientRootsSnapshotError;

/// Validates one complete `roots/list` snapshot into native directory paths.
///
/// Rejects an oversize snapshot, any unusable URI, and duplicate aliases that
/// would make the intersection ambiguous.
///
/// # Errors
///
/// Returns the fixed snapshot rejection. The caller must disable local root
/// operations for the session rather than fall back to static policy.
#[allow(deprecated)]
pub(crate) fn parse_client_root_snapshot(
    roots: &[Root],
) -> Result<Vec<AbsoluteNativePath>, ClientRootsSnapshotError> {
    if roots.len() > MAX_CLIENT_ROOTS {
        return Err(ClientRootsSnapshotError);
    }
    let mut parsed = Vec::with_capacity(roots.len());
    for root in roots {
        let path = parse_client_root_uri(&root.uri)?;
        if parsed.contains(&path) {
            return Err(ClientRootsSnapshotError);
        }
        parsed.push(path);
    }
    Ok(parsed)
}

/// Decodes one canonical `file:` URI into a validated absolute native path.
///
/// # Errors
///
/// Returns the fixed snapshot rejection for a non-`file` scheme, a non-local
/// host, userinfo, a port, a query, a fragment, an encoded separator or NUL,
/// an overlong value, or a path this platform cannot represent.
pub(crate) fn parse_client_root_uri(
    uri: &str,
) -> Result<AbsoluteNativePath, ClientRootsSnapshotError> {
    if uri.is_empty() || uri.len() > MAX_ROOT_URI_BYTES {
        return Err(ClientRootsSnapshotError);
    }
    if !has_canonical_local_prefix(uri) || contains_encoded_separator(uri) {
        return Err(ClientRootsSnapshotError);
    }
    let url = Url::parse(uri).map_err(|_| ClientRootsSnapshotError)?;
    if url.scheme() != "file"
        || url.host().is_some()
        || url.port().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(ClientRootsSnapshotError);
    }
    // `Path::components` drops the trailing separator that canonical directory
    // URIs carry, and collapses repeated separators, without resolving `..`.
    let path = file_url_path(&url)?
        .components()
        .collect::<std::path::PathBuf>();
    AbsoluteNativePath::from_os_str(path.as_os_str()).map_err(|_| ClientRootsSnapshotError)
}

/// Reports whether the raw URI uses the canonical local `file:` authority form.
///
/// Only the empty and `localhost` authorities are local, and only the explicit
/// `file://.../` form is canonical, so scheme-relative and host-bearing
/// spellings are rejected before any normalization can rewrite them.
fn has_canonical_local_prefix(uri: &str) -> bool {
    let lowered = uri.to_ascii_lowercase();
    lowered.starts_with("file:///") || lowered.starts_with("file://localhost/")
}

/// Reports whether the raw URI encodes a separator or NUL.
///
/// An encoded separator would let one URI address a path the same client could
/// not name literally, so it is rejected before any decoding.
fn contains_encoded_separator(uri: &str) -> bool {
    let bytes = uri.as_bytes();
    bytes.windows(3).any(|window| {
        window[0] == b'%'
            && matches!(
                (
                    window[1].to_ascii_lowercase(),
                    window[2].to_ascii_lowercase()
                ),
                (b'2', b'f') | (b'5', b'c') | (b'0', b'0')
            )
    })
}

#[cfg(any(unix, windows))]
fn file_url_path(url: &Url) -> Result<std::path::PathBuf, ClientRootsSnapshotError> {
    url.to_file_path().map_err(|()| ClientRootsSnapshotError)
}

#[cfg(not(any(unix, windows)))]
fn file_url_path(_: &Url) -> Result<std::path::PathBuf, ClientRootsSnapshotError> {
    Err(ClientRootsSnapshotError)
}

#[cfg(test)]
#[allow(deprecated)]
mod tests {
    use std::{fs, path::Path, sync::atomic::AtomicUsize};

    use super::*;
    use crate::{
        artifact_config::{ArtifactConfig, RelativeNativePath},
        artifact_roots::RootAccessErrorKind,
    };

    struct ScriptedRoots {
        advertises: bool,
        calls: AtomicUsize,
        answer: Option<Vec<Root>>,
        stall: bool,
    }

    impl ScriptedRoots {
        fn advertised(roots: Vec<Root>) -> Self {
            Self {
                advertises: true,
                calls: AtomicUsize::new(0),
                answer: Some(roots),
                stall: false,
            }
        }

        fn silent() -> Self {
            Self {
                advertises: false,
                calls: AtomicUsize::new(0),
                answer: None,
                stall: false,
            }
        }

        fn failing() -> Self {
            Self {
                advertises: true,
                calls: AtomicUsize::new(0),
                answer: None,
                stall: false,
            }
        }

        fn stalled() -> Self {
            Self {
                advertises: true,
                calls: AtomicUsize::new(0),
                answer: None,
                stall: true,
            }
        }

        fn calls(&self) -> usize {
            self.calls.load(Ordering::Acquire)
        }
    }

    impl ClientRootsSource for ScriptedRoots {
        fn advertises_roots(&self) -> bool {
            self.advertises
        }

        fn list_roots(&self) -> SnapshotFuture<'_> {
            self.calls.fetch_add(1, Ordering::AcqRel);
            Box::pin(async move {
                if self.stall {
                    std::future::pending::<()>().await;
                }
                self.answer.clone().ok_or(())
            })
        }
    }

    fn config(import: &Path, export: &Path) -> ArtifactConfig {
        ArtifactConfig::from_toml(&format!(
            "schema_version = 1\n[spaces]\nread_only = false\n\
             [[roots.import]]\nid = \"inbox\"\npath = {import:?}\n\
             [[roots.export]]\nid = \"outbox\"\npath = {export:?}\n"
        ))
        .expect("root config")
    }

    fn temporary_tree() -> (std::path::PathBuf, std::path::PathBuf, std::path::PathBuf) {
        let base = std::env::temp_dir()
            .canonicalize()
            .expect("canonical temporary directory")
            .join(format!(
                "any-mcp-client-roots-{}-{}",
                std::process::id(),
                getrandom::u64().unwrap_or(0)
            ));
        let import = base.join("import");
        let export = base.join("export");
        fs::create_dir_all(&import).expect("import directory");
        fs::create_dir_all(&export).expect("export directory");
        crate::artifact_roots::prepare_test_private_directory(&base)
            .expect("private base directory");
        crate::artifact_roots::prepare_test_private_directory(&import)
            .expect("private import directory");
        crate::artifact_roots::prepare_test_private_directory(&export)
            .expect("private export directory");
        (base, import, export)
    }

    fn relative(value: &str) -> RelativeNativePath {
        RelativeNativePath::from_utf8(value).expect("relative path")
    }

    fn directory_uri(path: &Path) -> String {
        Url::from_directory_path(path)
            .expect("directory URI")
            .to_string()
    }

    #[test]
    fn client_root_uris_admit_only_local_absolute_directories() {
        let inbox = directory_uri(&std::env::temp_dir().join("inbox"));
        let local_inbox = inbox.replacen("file:///", "file://localhost/", 1);
        let with_space = directory_uri(&std::env::temp_dir().join("with space"));
        assert!(parse_client_root_uri(&inbox).is_ok());
        assert!(parse_client_root_uri(&local_inbox).is_ok());
        assert!(parse_client_root_uri(&with_space).is_ok());

        for rejected in [
            "",
            "http://127.0.0.1/tmp",
            "file://example.com/tmp",
            "file://user@localhost/tmp",
            "file:///tmp?query=1",
            "file:///tmp#fragment",
            "file:///tmp/a%2Fb",
            "file:///tmp/a%00b",
            "file:relative/path",
            "file:/tmp/inbox",
            "FILE://EXAMPLE.COM/tmp",
            "not a uri",
        ] {
            assert!(
                parse_client_root_uri(rejected).is_err(),
                "expected rejection: {rejected}"
            );
        }
    }

    #[test]
    fn a_canonical_directory_uri_ignores_its_trailing_separator() {
        assert_eq!(
            parse_client_root_uri("file:///tmp/inbox/").expect("directory URI"),
            parse_client_root_uri("file:///tmp/inbox").expect("plain URI")
        );
    }

    #[test]
    fn overlong_client_root_uris_are_rejected_before_parsing() {
        let long = format!("file:///{}", "a".repeat(MAX_ROOT_URI_BYTES));
        assert_eq!(parse_client_root_uri(&long), Err(ClientRootsSnapshotError));
    }

    #[test]
    fn snapshots_reject_oversize_and_duplicate_entries() {
        let oversize = (0..=MAX_CLIENT_ROOTS)
            .map(|index| Root::new(format!("file:///tmp/root-{index}")))
            .collect::<Vec<_>>();
        assert!(parse_client_root_snapshot(&oversize).is_err());

        let duplicates = vec![
            Root::new("file:///tmp/inbox"),
            Root::new("file:///tmp/./inbox"),
        ];
        assert!(parse_client_root_snapshot(&duplicates).is_err());

        let accepted = vec![
            Root::new("file:///tmp/inbox"),
            Root::new("file:///tmp/outbox"),
        ];
        assert_eq!(
            parse_client_root_snapshot(&accepted)
                .expect("accepted snapshot")
                .len(),
            2
        );
    }

    #[tokio::test]
    async fn a_disabled_gate_keeps_static_policy_without_a_snapshot() {
        let (base, import, export) = temporary_tree();
        let registry = RootRegistry::activate(&config(&import, &export)).expect("activate");
        let gate = ClientRootsGate::default();
        let source = Arc::new(ScriptedRoots::advertised(Vec::new()));
        gate.install_source(source.clone());
        fs::write(import.join("source.bin"), b"source").expect("source");

        let effective = gate
            .effective(&registry, Duration::from_secs(1))
            .await
            .expect("static policy");

        assert!(
            effective
                .open_import("inbox", &relative("source.bin"), 64)
                .is_ok()
        );
        assert_eq!(source.calls(), 0);
        fs::remove_dir_all(base).expect("cleanup");
    }

    #[tokio::test]
    async fn a_client_without_roots_capability_keeps_static_policy() {
        let (base, import, export) = temporary_tree();
        let registry = RootRegistry::activate(&config(&import, &export)).expect("activate");
        let gate = ClientRootsGate::default();
        gate.enable();
        let source = Arc::new(ScriptedRoots::silent());
        gate.install_source(source.clone());
        fs::write(import.join("source.bin"), b"source").expect("source");

        let effective = gate
            .effective(&registry, Duration::from_secs(1))
            .await
            .expect("static fallback");

        assert!(
            effective
                .open_import("inbox", &relative("source.bin"), 64)
                .is_ok()
        );
        assert_eq!(source.calls(), 0);
        fs::remove_dir_all(base).expect("cleanup");
    }

    #[cfg(any(unix, windows))]
    #[tokio::test]
    async fn one_snapshot_narrows_every_later_operation() {
        let (base, import, export) = temporary_tree();
        let registry = RootRegistry::activate(&config(&import, &export)).expect("activate");
        let gate = ClientRootsGate::default();
        gate.enable();
        let source = Arc::new(ScriptedRoots::advertised(vec![Root::new(directory_uri(
            &import,
        ))]));
        gate.install_source(source.clone());
        fs::write(import.join("source.bin"), b"source").expect("source");

        for _ in 0..3 {
            let effective = gate
                .effective(&registry, Duration::from_secs(1))
                .await
                .expect("narrowed policy");
            assert!(
                effective
                    .open_import("inbox", &relative("source.bin"), 64)
                    .is_ok()
            );
            assert!(
                effective
                    .begin_atomic_export("outbox", &relative("denied.bin"), 64)
                    .is_err()
            );
        }

        assert_eq!(source.calls(), 1);
        fs::remove_dir_all(base).expect("cleanup");
    }

    #[tokio::test]
    async fn an_empty_snapshot_denies_every_local_root() {
        let (base, import, export) = temporary_tree();
        let registry = RootRegistry::activate(&config(&import, &export)).expect("activate");
        let gate = ClientRootsGate::default();
        gate.enable();
        gate.install_source(Arc::new(ScriptedRoots::advertised(Vec::new())));
        fs::write(import.join("source.bin"), b"source").expect("source");

        let effective = gate
            .effective(&registry, Duration::from_secs(1))
            .await
            .expect("empty intersection");

        assert!(
            effective
                .open_import("inbox", &relative("source.bin"), 64)
                .is_err()
        );
        fs::remove_dir_all(base).expect("cleanup");
    }

    #[tokio::test]
    async fn a_failed_snapshot_freezes_disabled_local_roots() {
        let (base, import, export) = temporary_tree();
        let registry = RootRegistry::activate(&config(&import, &export)).expect("activate");
        let gate = ClientRootsGate::default();
        gate.enable();
        let source = Arc::new(ScriptedRoots::failing());
        gate.install_source(source.clone());

        for _ in 0..2 {
            let error = gate
                .effective(&registry, Duration::from_secs(1))
                .await
                .expect_err("frozen disabled");
            assert_eq!(error.kind(), RootAccessErrorKind::ClientRoots);
        }

        assert_eq!(source.calls(), 1);
        fs::remove_dir_all(base).expect("cleanup");
    }

    #[tokio::test]
    async fn an_unusable_snapshot_uri_freezes_disabled_local_roots() {
        let (base, import, export) = temporary_tree();
        let registry = RootRegistry::activate(&config(&import, &export)).expect("activate");
        let gate = ClientRootsGate::default();
        gate.enable();
        gate.install_source(Arc::new(ScriptedRoots::advertised(vec![Root::new(
            "http://example.com/tmp",
        )])));

        assert!(
            gate.effective(&registry, Duration::from_secs(1))
                .await
                .is_err()
        );
        fs::remove_dir_all(base).expect("cleanup");
    }

    #[tokio::test(start_paused = true)]
    async fn a_stalled_snapshot_times_out_into_disabled_local_roots() {
        let (base, import, export) = temporary_tree();
        let registry = RootRegistry::activate(&config(&import, &export)).expect("activate");
        let gate = ClientRootsGate::default();
        gate.enable();
        gate.install_source(Arc::new(ScriptedRoots::stalled()));

        assert!(
            gate.effective(&registry, Duration::from_secs(5))
                .await
                .is_err()
        );
        fs::remove_dir_all(base).expect("cleanup");
    }

    #[tokio::test]
    async fn an_enabled_gate_without_a_source_denies_local_roots() {
        let (base, import, export) = temporary_tree();
        let registry = RootRegistry::activate(&config(&import, &export)).expect("activate");
        let gate = ClientRootsGate::default();
        gate.enable();

        assert!(
            gate.effective(&registry, Duration::from_secs(1))
                .await
                .is_err()
        );
        fs::remove_dir_all(base).expect("cleanup");
    }

    #[test]
    fn gate_debug_output_carries_no_client_paths() {
        let gate = ClientRootsGate::default();
        gate.enable();
        gate.install_source(Arc::new(ScriptedRoots::advertised(vec![Root::new(
            "file:///tmp/secret-inbox",
        )])));

        let rendered = format!("{gate:?}");
        assert!(!rendered.contains("secret-inbox"));
        assert!(rendered.contains("enabled: true"));
    }
}
