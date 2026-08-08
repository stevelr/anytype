// any-mcp - bounded, workflow-oriented MCP server for Anytype
// SPDX-License-Identifier: Apache-2.0

//! Multi-transport real-server acceptance harness for the artifact data plane.
//!
//! The harness owns three concerns that every artifact acceptance scenario
//! shares, so scenario families stay small and comparable:
//!
//! 1. a closed transport matrix (control plane x data plane) whose complete
//!    coverage is proven by an offline inventory test rather than by hand,
//! 2. fixture discipline: an operator-shaped strict policy fixture on private
//!    temporary directories, immediate resource registration, exact teardown,
//!    and rejection of skipped disposable admission,
//! 3. content-free evidence: exact catalog/schema snapshots plus byte hashes
//!    that are compared for parity across every executed transport.
//!
//! Nothing here retains artifact bytes, credentials, staging bearers, or raw
//! server log lines; every reported value is a hash, a count, or a fixed
//! category name.
//!
//! Three scenario families share that harness: the smoke matrix (one happy
//! path per transport), the policy family (what a configuration must refuse),
//! and the content family ([`ArtifactContentScenario`]) covering
//! representative MIME artifacts, Markdown and plain-text canonicalization
//! including explicit no-op and lossy evidence, and configured validators.
//!
//! The validator scenarios declare a real host `file(1)` executable pinned by
//! hash ([`PinnedValidatorExecutable`]); no executable is shipped or
//! synthesized, and a host without a hash-pinnable one fails those scenarios
//! loudly rather than degrading into silent non-coverage.
//! `ANY_MCP_ACCEPTANCE_VALIDATOR` selects an exact executable when the host
//! keeps it outside `PATH`.
#![allow(dead_code)] // Shared support: each consuming target executes a subset.

use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsString,
    fmt,
    fs::{self, File, OpenOptions},
    future::Future,
    io::{Read, Write},
    path::{Path, PathBuf},
    pin::Pin,
    sync::Arc,
    time::{Duration, Instant, SystemTime},
};

use anytype::{
    objects::ANYTYPE_PLAIN_MARKDOWN_SUFFIX,
    test_util::{DisposableRun, TestContext, unique_suffix},
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
#[cfg(any(unix, windows))]
use cap_fs_ext::OpenOptionsFollowExt;
use futures_util::StreamExt;
use reqwest::{
    Method,
    header::{AUTHORIZATION, CONTENT_RANGE, CONTENT_TYPE, HeaderName, HeaderValue, RANGE},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use zeroize::{Zeroize, Zeroizing};

#[cfg(windows)]
use super::acceptance_owner_private_file;
use super::{McpDriver, ToolErrorEvidence};

/// Exact sorted production artifact tool inventory.
pub const ARTIFACT_TOOL_NAMES: [&str; 8] = [
    "artifact_release",
    "artifact_stage_upload",
    "artifact_status",
    "document_export",
    "document_import_create",
    "document_import_update",
    "file_export",
    "file_import",
];

/// Exact bytes imported and exported by every smoke scenario.
pub const ARTIFACT_FILE_PAYLOAD: &[u8] = b"artifact-file-payload";
/// Exact Markdown source used to create the smoke document.
pub const ARTIFACT_CREATE_MARKDOWN: &str = "# Artifact create\n";
/// Exact Markdown source used to update the smoke document.
pub const ARTIFACT_UPDATE_MARKDOWN: &str = "# Artifact update\n";

/// Stable families in the closed artifact adversarial inventory.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AdversarialFamily {
    /// Portable and native relative-path traversal attempts.
    PathTraversal,
    /// Symlink, junction, and reparse-point containment attempts.
    SymlinkReparse,
    /// Rename and time-of-check/time-of-use races.
    RenameRace,
    /// Case, normalization, reserved-name, and alias behavior.
    PathAliases,
    /// Hard-link identity and containment behavior.
    HardLinks,
    /// Malformed filenames, MIME declarations, and document bytes.
    MaliciousMetadata,
    /// Staging-handle guessing, replay, and cross-space use.
    HandleReplay,
    /// Partial and malformed staging writes.
    PartialWrites,
    /// Child-process crash and restart behavior.
    ProcessCrash,
    /// Bounded response and staging-concurrency behavior.
    OutputFlood,
    /// Cleanup after deliberately failed operations.
    Cleanup,
}

impl AdversarialFamily {
    /// Every stable inventory family, in matrix order.
    pub const ALL: &[Self] = &[
        Self::PathTraversal,
        Self::SymlinkReparse,
        Self::RenameRace,
        Self::PathAliases,
        Self::HardLinks,
        Self::MaliciousMetadata,
        Self::HandleReplay,
        Self::PartialWrites,
        Self::ProcessCrash,
        Self::OutputFlood,
        Self::Cleanup,
    ];

    /// Returns the stable lowercase inventory name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PathTraversal => "path_traversal",
            Self::SymlinkReparse => "symlink_reparse",
            Self::RenameRace => "rename_race",
            Self::PathAliases => "path_aliases",
            Self::HardLinks => "hard_links",
            Self::MaliciousMetadata => "malicious_metadata",
            Self::HandleReplay => "handle_replay",
            Self::PartialWrites => "partial_writes",
            Self::ProcessCrash => "process_crash",
            Self::OutputFlood => "output_flood",
            Self::Cleanup => "cleanup",
        }
    }

    /// Parses a stable lowercase inventory name.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .copied()
            .find(|family| family.as_str() == value)
    }
}

macro_rules! adversarial_case_ids {
    ($( $variant:ident => ($id:literal, $family:ident) ),+ $(,)?) => {
        /// One stable case identifier from the closed adversarial inventory.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub enum AdversarialCaseId {
            $( $variant, )+
        }

        impl AdversarialCaseId {
            /// Every stable case identifier, in matrix order.
            pub const ALL: &[Self] = &[
                $( Self::$variant, )+
            ];

            /// Returns the exact `FAMILY-NN` matrix identifier.
            #[must_use]
            pub const fn as_str(self) -> &'static str {
                match self {
                    $( Self::$variant => $id, )+
                }
            }

            /// Returns the family owning this case.
            #[must_use]
            pub const fn family(self) -> AdversarialFamily {
                match self {
                    $( Self::$variant => AdversarialFamily::$family, )+
                }
            }

            /// Parses an exact stable matrix identifier.
            #[must_use]
            pub fn parse(value: &str) -> Option<Self> {
                Self::ALL
                    .iter()
                    .copied()
                    .find(|case| case.as_str() == value)
            }

            /// Returns the current implementation partition for this case.
            #[must_use]
            pub const fn status(self) -> AdversarialCaseStatus {
                match self {
                    Self::Trav01 | Self::Trav02 | Self::Trav03 | Self::Trav04 | Self::Trav05
                    | Self::Trav06 | Self::Trav07 | Self::Trav08 | Self::Trav09 | Self::Trav10
                    | Self::Trav11 | Self::Trav12 | Self::Trav13 | Self::Trav14 | Self::Trav15
                    | Self::Trav16 | Self::Trav17 | Self::Trav18 | Self::Trav19 | Self::Trav20
                    | Self::Alias01 | Self::Alias02 | Self::Alias06 | Self::Alias07
                    | Self::Alias08 | Self::Alias09 | Self::Mal01 | Self::Mal02 | Self::Mal03
                    | Self::Mal04 | Self::Mal05 | Self::Mal06 | Self::Mal07 | Self::Mal08
                    | Self::Mal09 | Self::Mal10 | Self::Mal11 | Self::Mal12 | Self::Mal14 => {
                        AdversarialCaseStatus::Executed
                    }
                    Self::Sym01 | Self::Sym02 | Self::Sym03 | Self::Sym04 | Self::Sym05
                    | Self::Sym06 | Self::Sym07 | Self::Sym08 | Self::Sym09 | Self::Sym10
                    | Self::Sym11 | Self::Sym12 | Self::Sym13 | Self::Race01 | Self::Race02
                    | Self::Race03 | Self::Race04 | Self::Race05 | Self::Race06 | Self::Race07
                    | Self::Race08 | Self::Race09 | Self::Race10 | Self::Hlink01
                    | Self::Hlink02 | Self::Hlink03 | Self::Hlink04 | Self::Hlink05
                    | Self::Hlink06 => dynamic_filesystem_status(self),
                    Self::Alias03 | Self::Alias04 | Self::Alias05 => alias_windows_status(),
                    Self::Mal13 => validator_platform_status(),
                    _ => AdversarialCaseStatus::Pending,
                }
            }
        }
    };
}

adversarial_case_ids! {
    Trav01 => ("TRAV-01", PathTraversal), Trav02 => ("TRAV-02", PathTraversal),
    Trav03 => ("TRAV-03", PathTraversal), Trav04 => ("TRAV-04", PathTraversal),
    Trav05 => ("TRAV-05", PathTraversal), Trav06 => ("TRAV-06", PathTraversal),
    Trav07 => ("TRAV-07", PathTraversal), Trav08 => ("TRAV-08", PathTraversal),
    Trav09 => ("TRAV-09", PathTraversal), Trav10 => ("TRAV-10", PathTraversal),
    Trav11 => ("TRAV-11", PathTraversal), Trav12 => ("TRAV-12", PathTraversal),
    Trav13 => ("TRAV-13", PathTraversal), Trav14 => ("TRAV-14", PathTraversal),
    Trav15 => ("TRAV-15", PathTraversal), Trav16 => ("TRAV-16", PathTraversal),
    Trav17 => ("TRAV-17", PathTraversal), Trav18 => ("TRAV-18", PathTraversal),
    Trav19 => ("TRAV-19", PathTraversal), Trav20 => ("TRAV-20", PathTraversal),
    Sym01 => ("SYM-01", SymlinkReparse), Sym02 => ("SYM-02", SymlinkReparse),
    Sym03 => ("SYM-03", SymlinkReparse), Sym04 => ("SYM-04", SymlinkReparse),
    Sym05 => ("SYM-05", SymlinkReparse), Sym06 => ("SYM-06", SymlinkReparse),
    Sym07 => ("SYM-07", SymlinkReparse), Sym08 => ("SYM-08", SymlinkReparse),
    Sym09 => ("SYM-09", SymlinkReparse), Sym10 => ("SYM-10", SymlinkReparse),
    Sym11 => ("SYM-11", SymlinkReparse), Sym12 => ("SYM-12", SymlinkReparse),
    Sym13 => ("SYM-13", SymlinkReparse),
    Race01 => ("RACE-01", RenameRace), Race02 => ("RACE-02", RenameRace),
    Race03 => ("RACE-03", RenameRace), Race04 => ("RACE-04", RenameRace),
    Race05 => ("RACE-05", RenameRace), Race06 => ("RACE-06", RenameRace),
    Race07 => ("RACE-07", RenameRace), Race08 => ("RACE-08", RenameRace),
    Race09 => ("RACE-09", RenameRace), Race10 => ("RACE-10", RenameRace),
    Alias01 => ("ALIAS-01", PathAliases), Alias02 => ("ALIAS-02", PathAliases),
    Alias03 => ("ALIAS-03", PathAliases), Alias04 => ("ALIAS-04", PathAliases),
    Alias05 => ("ALIAS-05", PathAliases), Alias06 => ("ALIAS-06", PathAliases),
    Alias07 => ("ALIAS-07", PathAliases), Alias08 => ("ALIAS-08", PathAliases),
    Alias09 => ("ALIAS-09", PathAliases),
    Hlink01 => ("HLINK-01", HardLinks), Hlink02 => ("HLINK-02", HardLinks),
    Hlink03 => ("HLINK-03", HardLinks), Hlink04 => ("HLINK-04", HardLinks),
    Hlink05 => ("HLINK-05", HardLinks), Hlink06 => ("HLINK-06", HardLinks),
    Mal01 => ("MAL-01", MaliciousMetadata), Mal02 => ("MAL-02", MaliciousMetadata),
    Mal03 => ("MAL-03", MaliciousMetadata), Mal04 => ("MAL-04", MaliciousMetadata),
    Mal05 => ("MAL-05", MaliciousMetadata), Mal06 => ("MAL-06", MaliciousMetadata),
    Mal07 => ("MAL-07", MaliciousMetadata), Mal08 => ("MAL-08", MaliciousMetadata),
    Mal09 => ("MAL-09", MaliciousMetadata), Mal10 => ("MAL-10", MaliciousMetadata),
    Mal11 => ("MAL-11", MaliciousMetadata), Mal12 => ("MAL-12", MaliciousMetadata),
    Mal13 => ("MAL-13", MaliciousMetadata), Mal14 => ("MAL-14", MaliciousMetadata),
    Hand01 => ("HAND-01", HandleReplay), Hand02 => ("HAND-02", HandleReplay),
    Hand03 => ("HAND-03", HandleReplay), Hand04 => ("HAND-04", HandleReplay),
    Hand05 => ("HAND-05", HandleReplay), Hand06 => ("HAND-06", HandleReplay),
    Hand07 => ("HAND-07", HandleReplay), Hand08 => ("HAND-08", HandleReplay),
    Hand09 => ("HAND-09", HandleReplay), Hand10 => ("HAND-10", HandleReplay),
    Hand11 => ("HAND-11", HandleReplay), Hand12 => ("HAND-12", HandleReplay),
    Hand13 => ("HAND-13", HandleReplay), Hand14 => ("HAND-14", HandleReplay),
    Hand15 => ("HAND-15", HandleReplay), Hand16 => ("HAND-16", HandleReplay),
    Part01 => ("PART-01", PartialWrites), Part02 => ("PART-02", PartialWrites),
    Part03 => ("PART-03", PartialWrites), Part04 => ("PART-04", PartialWrites),
    Part05 => ("PART-05", PartialWrites), Part06 => ("PART-06", PartialWrites),
    Part07 => ("PART-07", PartialWrites), Part08 => ("PART-08", PartialWrites),
    Part09 => ("PART-09", PartialWrites), Part10 => ("PART-10", PartialWrites),
    Part11 => ("PART-11", PartialWrites), Part12 => ("PART-12", PartialWrites),
    Crash01 => ("CRASH-01", ProcessCrash), Crash02 => ("CRASH-02", ProcessCrash),
    Crash03 => ("CRASH-03", ProcessCrash), Crash04 => ("CRASH-04", ProcessCrash),
    Crash05 => ("CRASH-05", ProcessCrash), Crash06 => ("CRASH-06", ProcessCrash),
    Crash07 => ("CRASH-07", ProcessCrash),
    Flood01 => ("FLOOD-01", OutputFlood), Flood02 => ("FLOOD-02", OutputFlood),
    Flood03 => ("FLOOD-03", OutputFlood), Flood04 => ("FLOOD-04", OutputFlood),
    Flood05 => ("FLOOD-05", OutputFlood), Flood06 => ("FLOOD-06", OutputFlood),
    Flood07 => ("FLOOD-07", OutputFlood),
    Clean01 => ("CLEAN-01", Cleanup), Clean02 => ("CLEAN-02", Cleanup),
    Clean03 => ("CLEAN-03", Cleanup), Clean04 => ("CLEAN-04", Cleanup),
    Clean05 => ("CLEAN-05", Cleanup), Clean06 => ("CLEAN-06", Cleanup),
    Clean07 => ("CLEAN-07", Cleanup), Clean08 => ("CLEAN-08", Cleanup),
}

/// Exact case ownership of the default-policy runner.
pub const ADVERSARIAL_DEFAULT_CASE_IDS: &[AdversarialCaseId] = &[
    AdversarialCaseId::Trav01,
    AdversarialCaseId::Trav02,
    AdversarialCaseId::Trav03,
    AdversarialCaseId::Trav04,
    AdversarialCaseId::Trav05,
    AdversarialCaseId::Trav06,
    AdversarialCaseId::Trav07,
    AdversarialCaseId::Trav08,
    AdversarialCaseId::Trav09,
    AdversarialCaseId::Trav10,
    AdversarialCaseId::Trav11,
    AdversarialCaseId::Trav12,
    AdversarialCaseId::Trav13,
    AdversarialCaseId::Trav14,
    AdversarialCaseId::Trav15,
    AdversarialCaseId::Trav16,
    AdversarialCaseId::Trav17,
    AdversarialCaseId::Trav18,
    AdversarialCaseId::Alias01,
    AdversarialCaseId::Alias02,
    AdversarialCaseId::Alias03,
    AdversarialCaseId::Alias04,
    AdversarialCaseId::Alias05,
    AdversarialCaseId::Alias06,
    AdversarialCaseId::Alias08,
    AdversarialCaseId::Alias09,
    AdversarialCaseId::Mal01,
    AdversarialCaseId::Mal02,
    AdversarialCaseId::Mal03,
    AdversarialCaseId::Mal04,
    AdversarialCaseId::Mal05,
    AdversarialCaseId::Mal06,
    AdversarialCaseId::Mal07,
    AdversarialCaseId::Mal08,
    AdversarialCaseId::Mal09,
    AdversarialCaseId::Mal11,
    AdversarialCaseId::Mal14,
];

/// Exact cases requiring distinct runtime policy or startup ownership.
pub const ADVERSARIAL_SPECIAL_CASE_IDS: &[AdversarialCaseId] = &[
    AdversarialCaseId::Trav19,
    AdversarialCaseId::Trav20,
    AdversarialCaseId::Alias07,
    AdversarialCaseId::Mal10,
    AdversarialCaseId::Mal12,
    AdversarialCaseId::Mal13,
];

/// Canonical default cases repeated on stable and preview stdio.
pub const ADVERSARIAL_STDIO_SENTINEL_IDS: &[AdversarialCaseId] = &[
    AdversarialCaseId::Trav01,
    AdversarialCaseId::Alias06,
    AdversarialCaseId::Mal01,
    AdversarialCaseId::Mal02,
];

/// Exact dynamic-filesystem cases owned by `any-d9ia.8.2.3`.
pub const ADVERSARIAL_DYNAMIC_FILESYSTEM_CASE_IDS: &[AdversarialCaseId] = &[
    AdversarialCaseId::Sym01,
    AdversarialCaseId::Sym02,
    AdversarialCaseId::Sym03,
    AdversarialCaseId::Sym04,
    AdversarialCaseId::Sym05,
    AdversarialCaseId::Sym06,
    AdversarialCaseId::Sym07,
    AdversarialCaseId::Sym08,
    AdversarialCaseId::Sym09,
    AdversarialCaseId::Sym10,
    AdversarialCaseId::Sym11,
    AdversarialCaseId::Sym12,
    AdversarialCaseId::Sym13,
    AdversarialCaseId::Race01,
    AdversarialCaseId::Race02,
    AdversarialCaseId::Race03,
    AdversarialCaseId::Race04,
    AdversarialCaseId::Race05,
    AdversarialCaseId::Race06,
    AdversarialCaseId::Race07,
    AdversarialCaseId::Race08,
    AdversarialCaseId::Race09,
    AdversarialCaseId::Race10,
    AdversarialCaseId::Hlink01,
    AdversarialCaseId::Hlink02,
    AdversarialCaseId::Hlink03,
    AdversarialCaseId::Hlink04,
    AdversarialCaseId::Hlink05,
    AdversarialCaseId::Hlink06,
];

/// Runtime rows in the direct owner; startup-only SYM-11/12 are merged later.
pub const ADVERSARIAL_DYNAMIC_RUNTIME_CASE_IDS: &[AdversarialCaseId] = &[
    AdversarialCaseId::Sym01,
    AdversarialCaseId::Sym02,
    AdversarialCaseId::Sym03,
    AdversarialCaseId::Sym04,
    AdversarialCaseId::Sym05,
    AdversarialCaseId::Sym06,
    AdversarialCaseId::Sym07,
    AdversarialCaseId::Sym08,
    AdversarialCaseId::Sym09,
    AdversarialCaseId::Sym10,
    AdversarialCaseId::Sym13,
    AdversarialCaseId::Race01,
    AdversarialCaseId::Race02,
    AdversarialCaseId::Race03,
    AdversarialCaseId::Race04,
    AdversarialCaseId::Race05,
    AdversarialCaseId::Race06,
    AdversarialCaseId::Race07,
    AdversarialCaseId::Race08,
    AdversarialCaseId::Race09,
    AdversarialCaseId::Race10,
    AdversarialCaseId::Hlink01,
    AdversarialCaseId::Hlink02,
    AdversarialCaseId::Hlink03,
    AdversarialCaseId::Hlink04,
    AdversarialCaseId::Hlink05,
    AdversarialCaseId::Hlink06,
];

/// Implemented dynamic-filesystem sentinels repeated through stable stdio.
pub const ADVERSARIAL_DYNAMIC_STDIO_IMPLEMENTED_IDS: &[AdversarialCaseId] =
    &[AdversarialCaseId::Sym01, AdversarialCaseId::Hlink01];

/// Fixture root replaced by a directory symlink before a startup probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtifactSymlinkStartupTarget {
    /// The configured client-visible import root (SYM-11).
    ImportRoot,
    /// The private staging root (SYM-12).
    StagingRoot,
}

/// Content-free result of one bounded startup probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtifactStartupCaseOutcome {
    /// Startup failed with the supplied fixed diagnostic category.
    Rejected(&'static str),
    /// The target cannot create directory symlinks on this platform.
    Unsupported,
}

/// Whether a case is executed now, explicitly unsupported, or still pending.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdversarialCaseStatus {
    /// The case has a concrete regression test in this ticket.
    Executed,
    /// The platform lacks a required primitive and records that fact.
    PlatformUnsupported,
    /// The case remains in the closed inventory for a later ticket.
    Pending,
}

const fn alias_windows_status() -> AdversarialCaseStatus {
    if cfg!(windows) {
        AdversarialCaseStatus::Executed
    } else {
        AdversarialCaseStatus::PlatformUnsupported
    }
}

const fn validator_platform_status() -> AdversarialCaseStatus {
    if VALIDATOR_PLATFORM_ACTIVATES {
        AdversarialCaseStatus::Executed
    } else {
        AdversarialCaseStatus::PlatformUnsupported
    }
}

const fn dynamic_filesystem_status(id: AdversarialCaseId) -> AdversarialCaseStatus {
    match id {
        AdversarialCaseId::Sym01
        | AdversarialCaseId::Sym02
        | AdversarialCaseId::Sym03
        | AdversarialCaseId::Sym04
        | AdversarialCaseId::Sym05
        | AdversarialCaseId::Sym06
        | AdversarialCaseId::Sym09
        | AdversarialCaseId::Sym11
        | AdversarialCaseId::Sym12
        | AdversarialCaseId::Hlink01
        | AdversarialCaseId::Hlink02
        | AdversarialCaseId::Hlink04
        | AdversarialCaseId::Race01
        | AdversarialCaseId::Race02
        | AdversarialCaseId::Race03
        | AdversarialCaseId::Race06
        | AdversarialCaseId::Race07
        | AdversarialCaseId::Race08
        | AdversarialCaseId::Hlink06 => {
            if cfg!(any(unix, windows)) {
                AdversarialCaseStatus::Executed
            } else {
                AdversarialCaseStatus::PlatformUnsupported
            }
        }
        AdversarialCaseId::Sym13 | AdversarialCaseId::Hlink05 => {
            if cfg!(unix) {
                AdversarialCaseStatus::Executed
            } else {
                AdversarialCaseStatus::PlatformUnsupported
            }
        }
        _ => AdversarialCaseStatus::Pending,
    }
}

/// One row of the current closed case partition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdversarialCasePartition {
    /// Stable case identifier.
    pub id: AdversarialCaseId,
    /// Current implementation status.
    pub status: AdversarialCaseStatus,
}

/// Returns every case and exactly one current implementation status.
pub fn adversarial_case_partition() -> impl Iterator<Item = AdversarialCasePartition> {
    AdversarialCaseId::ALL
        .iter()
        .copied()
        .map(|id| AdversarialCasePartition {
            id,
            status: id.status(),
        })
}

/// Closed domain error codes admitted by the adversarial matrix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExpectedToolErrorCode {
    /// Invalid grammar or a missing policy declaration.
    Validation,
    /// A hidden or unauthorized resource.
    NotFound,
    /// A conflicting mutation or indeterminate mutation result.
    Conflict,
    /// A result that correctly hit an explicit bound.
    BoundedResult,
    /// A classified upstream failure.
    Upstream,
}

impl ExpectedToolErrorCode {
    /// Returns the exact domain error code sent by the MCP server.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Validation => "validation",
            Self::NotFound => "not_found",
            Self::Conflict => "conflict",
            Self::BoundedResult => "bounded_result",
            Self::Upstream => "upstream",
        }
    }
}

/// Named tool-error outcomes from the adversarial matrix vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExpectedToolErrorKind {
    /// Generic portable or native grammar rejection.
    Validation,
    /// Uniform hidden-resource rejection.
    NotFound,
    /// A conflicting or indeterminate mutation.
    Conflict,
    /// A correctly bounded result.
    BoundedResult,
    /// A classified upstream failure.
    Upstream,
    /// Missing local-root configuration guidance.
    MissingRoots,
    /// Missing staging configuration guidance.
    MissingStaging,
}

impl ExpectedToolErrorKind {
    /// Returns the exact matrix vocabulary spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Validation => "validation",
            Self::NotFound => "not_found",
            Self::Conflict => "conflict",
            Self::BoundedResult => "bounded_result",
            Self::Upstream => "upstream",
            Self::MissingRoots => "missing_roots",
            Self::MissingStaging => "missing_staging",
        }
    }

    /// Returns the exact server domain code for this outcome.
    #[must_use]
    pub const fn code(self) -> ExpectedToolErrorCode {
        match self {
            Self::Validation | Self::MissingRoots | Self::MissingStaging => {
                ExpectedToolErrorCode::Validation
            }
            Self::NotFound => ExpectedToolErrorCode::NotFound,
            Self::Conflict => ExpectedToolErrorCode::Conflict,
            Self::BoundedResult => ExpectedToolErrorCode::BoundedResult,
            Self::Upstream => ExpectedToolErrorCode::Upstream,
        }
    }
}

/// One exact expected outcome from the adversarial matrix vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExpectedOutcome {
    /// A fully validated domain tool error with its fixed message.
    ToolError {
        /// Exact matrix error category and its corresponding domain code.
        kind: ExpectedToolErrorKind,
        /// Exact stable server message.
        message: &'static str,
    },
    /// JSON-RPC method-not-found before argument decoding.
    MethodNotFound,
    /// Exact staging HTTP status and fixed small response body.
    Http {
        /// Exact HTTP status.
        status: u16,
        /// Exact small response body.
        body: &'static [u8],
    },
    /// Startup fails with the exact fixed stderr category.
    StartupRejected {
        /// Fixed rejected-config or startup category.
        category: &'static str,
    },
    /// The operation succeeds and its separate invariant must hold.
    Accepted,
}

/// A minimal observation suitable for comparing one expected outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObservedOutcome<'a> {
    /// A validated domain tool error.
    ToolError {
        /// Domain error code.
        code: &'a str,
        /// Domain error message.
        message: &'a str,
    },
    /// JSON-RPC method-not-found.
    MethodNotFound,
    /// Staging response status and body.
    Http {
        /// HTTP status.
        status: u16,
        /// Response body.
        body: &'a [u8],
    },
    /// Child startup failure category.
    StartupRejected {
        /// Captured fixed category.
        category: &'a str,
    },
    /// Successful operation.
    Accepted,
}

impl ExpectedOutcome {
    /// Compares a complete observed outcome against this exact expectation.
    ///
    /// # Errors
    ///
    /// Returns a fixed category when the observed outcome differs. Callers
    /// retain redacted, case-specific evidence separately.
    pub fn assert_matches(self, observed: ObservedOutcome<'_>) -> Result<(), String> {
        let matches = match (self, observed) {
            (
                Self::ToolError { kind, message },
                ObservedOutcome::ToolError {
                    code: observed_code,
                    message: observed_message,
                },
            ) => kind.code().as_str() == observed_code && message == observed_message,
            (Self::MethodNotFound, ObservedOutcome::MethodNotFound)
            | (Self::Accepted, ObservedOutcome::Accepted) => true,
            (
                Self::Http { status, body },
                ObservedOutcome::Http {
                    status: observed_status,
                    body: observed_body,
                },
            ) => status == observed_status && body == observed_body,
            (
                Self::StartupRejected { category },
                ObservedOutcome::StartupRejected {
                    category: observed_category,
                },
            ) => category == observed_category,
            _ => false,
        };
        if matches {
            Ok(())
        } else {
            Err("adversarial expected outcome mismatch".to_owned())
        }
    }

    /// Compares a fully validated MCP error result against this expectation.
    ///
    /// # Errors
    ///
    /// Returns a fixed category when this is not a matching exact tool error.
    pub fn assert_tool_error(self, evidence: &ToolErrorEvidence) -> Result<(), String> {
        let message = evidence
            .normalized_result()
            .pointer("/structuredContent/message")
            .and_then(Value::as_str)
            .ok_or_else(|| "adversarial tool error evidence omitted message".to_owned())?;
        self.assert_matches(ObservedOutcome::ToolError {
            code: evidence.code(),
            message,
        })
    }
}

const ADVERSARIAL_VALIDATION_MESSAGE: &str =
    "Input validation failed. Correct the supplied fields and retry.";
const ADVERSARIAL_NOT_FOUND_MESSAGE: &str =
    "The requested Anytype entity was not found. Verify its identifier and space.";
const ADVERSARIAL_CONFLICT_MESSAGE: &str =
    "The object changed or a request precondition failed. Read it again before retrying.";
const ADVERSARIAL_BOUNDED_MESSAGE: &str =
    "The result exceeds this workflow's limit. Retry with a paginated or chunked read.";
const ADVERSARIAL_ROOTS_REQUIRED_MESSAGE: &str = "No artifact roots are configured. Declare roots in an any-mcp TOML config and select it with ANY_MCP_CONFIG or --config.";

fn adversarial_tool_error(kind: ExpectedToolErrorKind) -> ExpectedOutcome {
    let message = match kind {
        ExpectedToolErrorKind::Validation => ADVERSARIAL_VALIDATION_MESSAGE,
        ExpectedToolErrorKind::NotFound => ADVERSARIAL_NOT_FOUND_MESSAGE,
        ExpectedToolErrorKind::Conflict => ADVERSARIAL_CONFLICT_MESSAGE,
        ExpectedToolErrorKind::BoundedResult => ADVERSARIAL_BOUNDED_MESSAGE,
        ExpectedToolErrorKind::MissingRoots => ADVERSARIAL_ROOTS_REQUIRED_MESSAGE,
        ExpectedToolErrorKind::Upstream | ExpectedToolErrorKind::MissingStaging => {
            return ExpectedOutcome::ToolError {
                kind,
                message: "Anytype could not complete the request. Retry later or inspect redacted server diagnostics.",
            };
        }
    };
    ExpectedOutcome::ToolError { kind, message }
}

/// Content-free execution partition for one adversarial acceptance run.
#[derive(Clone, Default)]
pub struct AdversarialExecution {
    executed: BTreeSet<AdversarialCaseId>,
    unsupported: BTreeSet<AdversarialCaseId>,
    unsupported_reasons: BTreeMap<AdversarialCaseId, &'static str>,
    quota_restored: BTreeSet<AdversarialCaseId>,
    forbidden_log_needles: Vec<Zeroizing<Vec<u8>>>,
    uniform_not_found_digest: Option<String>,
}

impl fmt::Debug for AdversarialExecution {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AdversarialExecution")
            .field("executed_count", &self.executed.len())
            .field("unsupported_count", &self.unsupported.len())
            .field("forbidden_needle_count", &self.forbidden_log_needles.len())
            .finish()
    }
}

impl AdversarialExecution {
    /// Records one case only after its complete assertions pass.
    pub fn record_executed(&mut self, id: AdversarialCaseId) -> Result<(), String> {
        if self.unsupported.contains(&id) || !self.executed.insert(id) {
            return Err("adversarial case executed more than once".to_owned());
        }
        Ok(())
    }

    /// Records one matrix-approved platform-unsupported case.
    pub fn record_unsupported(&mut self, id: AdversarialCaseId) -> Result<(), String> {
        let reason = unsupported_reason(id)
            .ok_or_else(|| "adversarial case has no approved unsupported reason".to_owned())?;
        self.record_unsupported_with_reason(id, reason)
    }

    /// Records one capability-probed unsupported case with a fixed safe reason.
    pub fn record_unsupported_with_reason(
        &mut self,
        id: AdversarialCaseId,
        reason: &'static str,
    ) -> Result<(), String> {
        if !approved_unsupported_reason(id, reason) {
            return Err("adversarial unsupported reason was not approved".to_owned());
        }
        if self.executed.contains(&id) || !self.unsupported.insert(id) {
            return Err("adversarial case partitioned more than once".to_owned());
        }
        self.unsupported_reasons.insert(id, reason);
        Ok(())
    }

    /// Merges a disjoint content-free execution partition.
    ///
    /// # Errors
    ///
    /// Returns a fixed category if either partition reports the same case.
    pub fn merge(&mut self, other: Self) -> Result<(), String> {
        for id in other.executed {
            self.record_executed(id)?;
        }
        for id in other.unsupported.iter().copied() {
            let reason = other
                .unsupported_reasons
                .get(&id)
                .copied()
                .ok_or_else(|| "adversarial unsupported reason was missing".to_owned())?;
            self.record_unsupported_with_reason(id, reason)?;
        }
        self.quota_restored.extend(other.quota_restored);
        self.forbidden_log_needles
            .extend(other.forbidden_log_needles);
        if let Some(digest) = other.uniform_not_found_digest {
            if let Some(existing) = &self.uniform_not_found_digest {
                if existing != &digest {
                    return Err("TRAV uniform not-found payloads diverged".to_owned());
                }
            } else {
                self.uniform_not_found_digest = Some(digest);
            }
        }
        Ok(())
    }

    /// Retains one sensitive value only for the later redacted log audit.
    pub fn record_forbidden_log_needle(&mut self, value: &[u8]) -> Result<(), String> {
        if value.is_empty() || value.len() > SERVER_LOG_NEEDLE_BYTES {
            return Err("adversarial forbidden log needle was outside the audit limit".to_owned());
        }
        if !self
            .forbidden_log_needles
            .iter()
            .any(|needle| needle.as_slice() == value)
        {
            self.forbidden_log_needles
                .push(Zeroizing::new(value.to_vec()));
        }
        Ok(())
    }

    /// Appends transient forbidden log values without exposing them in evidence.
    pub fn append_forbidden_log_needles<'a>(&'a self, destination: &mut Vec<&'a [u8]>) {
        destination.extend(
            self.forbidden_log_needles
                .iter()
                .map(|needle| needle.as_slice()),
        );
    }

    /// Returns transient forbidden log values for immediate redacted auditing.
    #[must_use]
    pub fn forbidden_log_needles(&self) -> Vec<&[u8]> {
        self.forbidden_log_needles
            .iter()
            .map(|needle| needle.as_slice())
            .collect()
    }

    /// Emits one bounded, content-free owner summary after teardown succeeds.
    pub fn emit_owner_evidence(
        &self,
        control: ArtifactControlPlane,
        audit: &ArtifactServerLogAudit,
    ) -> Result<(), String> {
        let observed = self
            .executed
            .union(&self.unsupported)
            .copied()
            .collect::<BTreeSet<_>>();
        if !observed.is_subset(&self.quota_restored) || !audit.is_clean() {
            return Err("adversarial owner evidence was incomplete".to_owned());
        }
        let case_ids = observed.iter().map(|id| id.as_str()).collect::<Vec<_>>();
        let families = observed
            .iter()
            .map(|id| id.family().as_str())
            .collect::<BTreeSet<_>>();
        let unsupported = self
            .unsupported_reasons
            .iter()
            .map(|(id, reason)| json!({"case_id": id.as_str(), "reason": reason}))
            .collect::<Vec<_>>();
        let evidence = json!({
            "case_ids": case_ids,
            "families": families,
            "transport": control.as_str(),
            "outcomes": {
                "executed": self.executed.len(),
                "unsupported": unsupported,
            },
            "log_audit": {
                "panic_or_fatal": audit.panic_or_fatal_lines,
                "unclassified": audit.unclassified_error_lines,
                "known_noise": audit.known_classes.values().copied().sum::<u64>(),
            },
            "teardown": {
                "space_deleted": true,
                "prefix_inventory": 0,
                "quota_restored": true,
            },
        });
        let encoded = serde_json::to_string(&evidence)
            .map_err(|_| "encode adversarial owner evidence".to_owned())?;
        eprintln!("{encoded}");
        Ok(())
    }

    /// Records the canonical payload digest shared by uniform not-found cases.
    pub fn record_uniform_not_found_payload(&mut self, result: &Value) -> Result<(), String> {
        let encoded = serde_json::to_vec(result)
            .map_err(|_| "encode uniform adversarial not-found payload".to_owned())?;
        let digest = hex_digest(&Sha256::digest(encoded));
        if let Some(existing) = &self.uniform_not_found_digest {
            if existing != &digest {
                return Err("TRAV uniform not-found payloads diverged".to_owned());
            }
        } else {
            self.uniform_not_found_digest = Some(digest);
        }
        Ok(())
    }

    fn record_quota_restored(&mut self) {
        self.quota_restored.extend(self.executed.iter().copied());
        self.quota_restored.extend(self.unsupported.iter().copied());
    }

    /// Records that a startup-rejection case cannot activate staging quota.
    pub fn record_quota_not_applicable(&mut self) {
        self.record_quota_restored();
    }

    /// Number of cases actually executed on this platform.
    #[must_use]
    pub fn executed_count(&self) -> usize {
        self.executed.len()
    }

    /// Number of cases explicitly unsupported on this platform.
    #[must_use]
    pub fn unsupported_count(&self) -> usize {
        self.unsupported.len()
    }

    /// Proves this run partitions all 43 cases owned by the ticket exactly.
    ///
    /// # Errors
    ///
    /// Returns a fixed category if any assigned case is absent or a pending
    /// family appears in the execution record.
    pub fn assert_ticket_complete(&self) -> Result<(), String> {
        let assigned = AdversarialCaseId::ALL
            .iter()
            .copied()
            .filter(|id| {
                matches!(
                    id.family(),
                    AdversarialFamily::PathTraversal
                        | AdversarialFamily::PathAliases
                        | AdversarialFamily::MaliciousMetadata
                )
            })
            .collect::<BTreeSet<_>>();
        let observed = self
            .executed
            .union(&self.unsupported)
            .copied()
            .collect::<BTreeSet<_>>();
        if observed != assigned
            || self
                .unsupported
                .iter()
                .any(|id| !self.unsupported_reasons.contains_key(id))
            || !observed.is_subset(&self.quota_restored)
        {
            return Err("adversarial ticket execution partition was incomplete".to_owned());
        }
        Ok(())
    }

    /// Proves this execution contains exactly the supplied case IDs.
    pub fn assert_exact(&self, expected: &[AdversarialCaseId]) -> Result<(), String> {
        let expected_count = expected.len();
        let expected = expected.iter().copied().collect::<BTreeSet<_>>();
        let observed = self
            .executed
            .union(&self.unsupported)
            .copied()
            .collect::<BTreeSet<_>>();
        if expected.len() != expected_count
            || expected.len() != observed.len()
            || expected != observed
            || !observed.is_subset(&self.quota_restored)
        {
            return Err("adversarial execution did not match its exact owner inventory".to_owned());
        }
        Ok(())
    }
}

fn unsupported_reason(id: AdversarialCaseId) -> Option<&'static str> {
    match id {
        AdversarialCaseId::Alias03 | AdversarialCaseId::Alias05 if !cfg!(windows) => {
            Some("windows_only")
        }
        AdversarialCaseId::Alias04 if !cfg!(windows) => Some("windows_only"),
        AdversarialCaseId::Alias04 if cfg!(windows) => Some("windows_8dot3_unavailable"),
        AdversarialCaseId::Mal13 if !VALIDATOR_PLATFORM_ACTIVATES => Some("validator_not_active"),
        AdversarialCaseId::Sym07 | AdversarialCaseId::Sym08 | AdversarialCaseId::Sym10
            if !cfg!(windows) =>
        {
            Some("windows_only")
        }
        AdversarialCaseId::Sym13
        | AdversarialCaseId::Race05
        | AdversarialCaseId::Race06
        | AdversarialCaseId::Hlink03
        | AdversarialCaseId::Hlink05
            if !cfg!(unix) =>
        {
            Some("unix_only")
        }
        AdversarialCaseId::Sym01
        | AdversarialCaseId::Sym02
        | AdversarialCaseId::Sym03
        | AdversarialCaseId::Sym04
        | AdversarialCaseId::Sym05
        | AdversarialCaseId::Sym06
        | AdversarialCaseId::Sym09
        | AdversarialCaseId::Sym11
        | AdversarialCaseId::Sym12
        | AdversarialCaseId::Race01
        | AdversarialCaseId::Race02
        | AdversarialCaseId::Race03
        | AdversarialCaseId::Race04
        | AdversarialCaseId::Race07
        | AdversarialCaseId::Race08
        | AdversarialCaseId::Race09
        | AdversarialCaseId::Race10
        | AdversarialCaseId::Hlink01
        | AdversarialCaseId::Hlink02
        | AdversarialCaseId::Hlink04
        | AdversarialCaseId::Hlink06
            if !cfg!(any(unix, windows)) =>
        {
            Some("filesystem_primitive_unavailable")
        }
        _ => None,
    }
}

fn approved_unsupported_reason(id: AdversarialCaseId, reason: &'static str) -> bool {
    unsupported_reason(id) == Some(reason)
        || matches!(
            (id, reason),
            (
                AdversarialCaseId::Sym11 | AdversarialCaseId::Sym12,
                "symlink_creation_unavailable"
            ) | (
                AdversarialCaseId::Hlink01
                    | AdversarialCaseId::Hlink02
                    | AdversarialCaseId::Hlink03
                    | AdversarialCaseId::Hlink04
                    | AdversarialCaseId::Hlink05
                    | AdversarialCaseId::Hlink06,
                "link_count_unavailable"
            )
        )
}

/// Fixture inputs for the path, alias, and hostile-metadata scenarios.
pub struct ArtifactAdversarialRun<'a> {
    /// Control plane under test, retained only as a fixed evidence category.
    pub control: ArtifactControlPlane,
    /// Strict private-root policy backing this server.
    pub policy: &'a ArtifactPolicyFixture,
    /// Disposable space that owns every created Anytype resource.
    pub ctx: &'a TestContext,
    /// Retained-root counter used only by direct grammar-rejection assertions.
    pub root_access_attempts: Option<&'a dyn Fn() -> u64>,
    /// Successful import-open counter used to prove alias targets stay unread.
    pub successful_import_opens: Option<&'a dyn Fn() -> u64>,
    /// Optional direct-runtime synchronization hooks for deterministic races.
    pub gate_hooks: Option<&'a dyn ArtifactGateHooks>,
}

/// Test-harness-owned gate adapter, deliberately independent of the library's
/// concrete gate type so this support module compiles both in-crate and as an
/// external integration-test module.
pub trait ArtifactGateHooks: Send + Sync {
    /// Arms the exact import operation selected by its raw idempotency key.
    fn arm_import<'a>(
        &'a self,
        key: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Box<dyn ArtifactGateLease>, String>> + Send + 'a>>;
    /// Arms the exact export operation selected by its raw idempotency key.
    fn arm_export<'a>(
        &'a self,
        key: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Box<dyn ArtifactGateLease>, String>> + Send + 'a>>;
    /// Arms the exact document operation selected by its raw idempotency key.
    fn arm_document<'a>(
        &'a self,
        key: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Box<dyn ArtifactGateLease>, String>> + Send + 'a>>;
}

/// One armed, re-armable test synchronization point.
pub trait ArtifactGateLease: Send {
    /// Waits for production to enter the armed point.
    fn wait<'a>(&'a mut self, timeout: Duration)
    -> Pin<Box<dyn Future<Output = bool> + Send + 'a>>;
    /// Releases the blocked production operation.
    fn release(&self);
}

fn local_source(root: &str, path: &str) -> Value {
    json!({"local": {"root": root, "path": path}})
}

fn local_destination(root: &str, path: &str) -> Value {
    json!({"local": {"root": root, "path": path}})
}

fn native_relative(value: &str) -> Value {
    #[cfg(unix)]
    let (encoding, bytes) = ("unix-bytes-base64url", value.as_bytes().to_vec());
    #[cfg(windows)]
    let (encoding, bytes) = (
        "windows-wtf16le-base64url",
        value
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>(),
    );
    json!({"encoding": encoding, "value": URL_SAFE_NO_PAD.encode(bytes)})
}

fn native_submission_value(value: &Value) -> Result<&str, String> {
    value
        .get("value")
        .and_then(Value::as_str)
        .ok_or_else(|| "adversarial native input omitted its encoded value".to_owned())
}

#[cfg(windows)]
fn native_wtf16(units: &[u16]) -> Value {
    let bytes = units
        .iter()
        .flat_map(|unit| unit.to_le_bytes())
        .collect::<Vec<_>>();
    json!({"encoding": "windows-wtf16le-base64url", "value": URL_SAFE_NO_PAD.encode(bytes)})
}

#[cfg(windows)]
fn windows_short_alias(path: &Path) -> Result<Option<String>, String> {
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    use windows_sys::Win32::Storage::FileSystem::GetShortPathNameW;

    let source = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    // SAFETY: `source` is NUL-terminated and remains live for both calls. The
    // first call supplies no output buffer and obtains the exact bound.
    let required = unsafe { GetShortPathNameW(source.as_ptr(), std::ptr::null_mut(), 0) };
    if required == 0 {
        return Err("inspect Windows short-name capability".to_owned());
    }
    let capacity = usize::try_from(required)
        .map_err(|_| "inspect Windows short-name capability".to_owned())?;
    let mut output = vec![0_u16; capacity];
    // SAFETY: the buffer has the exact capacity returned above and both
    // pointers remain live for the duration of the call.
    let length = unsafe { GetShortPathNameW(source.as_ptr(), output.as_mut_ptr(), required) };
    let length =
        usize::try_from(length).map_err(|_| "inspect Windows short-name capability".to_owned())?;
    if length == 0 || length >= output.len() {
        return Err("inspect Windows short-name capability".to_owned());
    }
    let short_path = PathBuf::from(OsString::from_wide(&output[..length]));
    let Some(short_name) = short_path.file_name().and_then(|name| name.to_str()) else {
        return Err("inspect Windows short-name capability".to_owned());
    };
    let Some(long_name) = path.file_name().and_then(|name| name.to_str()) else {
        return Err("inspect Windows short-name capability".to_owned());
    };
    if short_name.eq_ignore_ascii_case(long_name) || !short_name.contains('~') {
        return Ok(None);
    }
    Ok(Some(short_name.to_owned()))
}

fn file_import_arguments(
    space: &str,
    source: Value,
    name: &str,
    media_type: Option<&str>,
) -> Value {
    let mut arguments = json!({
        "space": space,
        "source": source,
        "name": name,
        "idempotency_key": format!("adversarial-import-{}", unique_suffix()),
    });
    if let Some(media_type) = media_type {
        arguments["media_type"] = Value::String(media_type.to_owned());
    }
    arguments
}

async fn adversarial_quota_snapshot(driver: &mut impl McpDriver) -> Result<(u64, u64), String> {
    let status = driver.call_tool("artifact_status", json!({})).await?;
    let bytes = status
        .get("staging_available_bytes")
        .and_then(Value::as_u64)
        .ok_or_else(|| "adversarial status omitted available staging bytes".to_owned())?;
    let entries = status
        .get("staging_available_entries")
        .and_then(Value::as_u64)
        .ok_or_else(|| "adversarial status omitted available staging entries".to_owned())?;
    Ok((bytes, entries))
}

async fn finish_adversarial_quota(
    driver: &mut impl McpDriver,
    before: (u64, u64),
    execution: &mut AdversarialExecution,
) -> Result<(), String> {
    if adversarial_quota_snapshot(driver).await? != before {
        return Err("adversarial staging quota was not restored".to_owned());
    }
    execution.record_quota_restored();
    Ok(())
}

fn seed_control_import_source(
    policy: &ArtifactPolicyFixture,
    source_name: &str,
) -> Result<(), String> {
    #[cfg(unix)]
    {
        let path = policy.import_root().join(source_name);
        fs::write(&path, b"malicious filename source")
            .map_err(|_| "write MAL-01 control source".to_owned())?;
        secure_files(std::slice::from_ref(&path))?;
    }
    #[cfg(not(unix))]
    let _ = (policy, source_name);
    Ok(())
}

async fn adversarial_refusal(
    driver: &mut impl McpDriver,
    tool: &'static str,
    arguments: Value,
    kind: ExpectedToolErrorKind,
    submitted: &[&str],
) -> Result<ToolErrorEvidence, String> {
    let evidence = driver.call_tool_error(tool, arguments).await?;
    adversarial_tool_error(kind).assert_tool_error(&evidence)?;
    if refusal_discloses_submitted(evidence.normalized_result(), submitted)? {
        return Err("adversarial refusal disclosed submitted input".to_owned());
    }
    Ok(evidence)
}

fn refusal_discloses_submitted(result: &Value, submitted: &[&str]) -> Result<bool, String> {
    let encoded =
        serde_json::to_string(result).map_err(|_| "encode adversarial tool refusal".to_owned())?;
    for value in submitted.iter().filter(|value| !value.is_empty()) {
        let json_encoded = serde_json::to_string(value)
            .map_err(|_| "encode adversarial submitted input".to_owned())?;
        if encoded.contains(*value)
            || encoded.contains(&json_encoded)
            || value_includes(result, value)
        {
            return Ok(true);
        }
    }
    Ok(false)
}

fn value_includes(value: &Value, needle: &str) -> bool {
    match value {
        Value::String(text) => text.contains(needle),
        Value::Array(values) => values.iter().any(|value| value_includes(value, needle)),
        Value::Object(values) => values.values().any(|value| value_includes(value, needle)),
        _ => false,
    }
}

fn assert_adversarial_response_frame(result: &Value) -> Result<(), String> {
    let envelope = json!({"jsonrpc": "2.0", "id": 0, "result": result});
    let mut frame = serde_json::to_vec(&envelope)
        .map_err(|_| "encode adversarial response frame".to_owned())?;
    frame.push(b'\n');
    let tokenizer =
        tiktoken_rs::cl100k_base().map_err(|_| "initialize artifact frame tokenizer".to_owned())?;
    let bytes = u64::try_from(frame.len())
        .map_err(|_| "adversarial response frame exceeds the addressable range".to_owned())?;
    let tokens = u64::try_from(
        tokenizer
            .encode_with_special_tokens(
                std::str::from_utf8(&frame)
                    .map_err(|_| "adversarial response frame was not UTF-8".to_owned())?,
            )
            .len(),
    )
    .map_err(|_| "adversarial response token count exceeds the addressable range".to_owned())?;
    if bytes > ARTIFACT_FRAME_CEILING_BYTES || tokens > ARTIFACT_FRAME_CEILING_TOKENS {
        return Err("artifact response exceeded its fixed MCP frame ceiling".to_owned());
    }
    Ok(())
}

async fn artifact_object_ids(ctx: &TestContext) -> Result<BTreeSet<String>, String> {
    let page = ctx
        .client
        .objects(&ctx.space_id)
        .limit(200)
        .list()
        .await
        .map_err(|_| "capture adversarial object inventory".to_owned())?;
    let objects = page
        .collect_all()
        .await
        .map_err(|_| "capture adversarial object inventory".to_owned())?;
    Ok(objects.into_iter().map(|object| object.id).collect())
}

async fn adversarial_seed_file(
    driver: &mut impl McpDriver,
    run: &ArtifactAdversarialRun<'_>,
    name: &str,
) -> Result<String, String> {
    let imported = driver
        .call_tool(
            "file_import",
            file_import_arguments(
                &run.ctx.space_id,
                local_source(
                    ArtifactPolicyFixture::IMPORT_ROOT,
                    ArtifactPolicyFixture::FILE_SOURCE,
                ),
                name,
                Some(ARTIFACT_FILE_MEDIA_TYPE),
            ),
        )
        .await?;
    let file_id = required_str(&imported, "/file_id")?;
    run.ctx.register_file(&file_id);
    Ok(file_id)
}

async fn adversarial_seed_document(
    driver: &mut impl McpDriver,
    run: &ArtifactAdversarialRun<'_>,
) -> Result<String, String> {
    let created = driver
        .call_tool(
            "document_import_create",
            json!({
                "space": run.ctx.space_id,
                "source": local_source(
                    ArtifactPolicyFixture::IMPORT_ROOT,
                    ArtifactPolicyFixture::CREATE_SOURCE,
                ),
                "source_format": "markdown",
                "object_type": "page",
                "name": format!("Adversarial document {}", unique_suffix()),
                "idempotency_key": format!("adversarial-document-{}", unique_suffix()),
            }),
        )
        .await?;
    let object_id = required_str(&created, "/object_id")?;
    run.ctx.register_object(&object_id);
    Ok(object_id)
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct FileObservation {
    len: u64,
    modified: Option<SystemTime>,
    accessed: Option<SystemTime>,
}

fn observe_file(path: &Path) -> Result<FileObservation, String> {
    let metadata = fs::metadata(path).map_err(|_| "observe adversarial file".to_owned())?;
    Ok(FileObservation {
        len: metadata.len(),
        modified: metadata.modified().ok(),
        accessed: metadata.accessed().ok(),
    })
}

fn create_file_symlink(target: &Path, link: &Path) -> Result<(), String> {
    #[cfg(unix)]
    std::os::unix::fs::symlink(target, link)
        .map_err(|_| "create adversarial file symlink".to_owned())?;
    #[cfg(windows)]
    std::os::windows::fs::symlink_file(target, link)
        .map_err(|_| "create adversarial file symlink".to_owned())?;
    #[cfg(not(any(unix, windows)))]
    return Err("adversarial file symlinks are unavailable".to_owned());
    Ok(())
}

fn create_directory_symlink(target: &Path, link: &Path) -> Result<(), String> {
    #[cfg(unix)]
    std::os::unix::fs::symlink(target, link)
        .map_err(|_| "create adversarial directory symlink".to_owned())?;
    #[cfg(windows)]
    std::os::windows::fs::symlink_dir(target, link)
        .map_err(|_| "create adversarial directory symlink".to_owned())?;
    #[cfg(not(any(unix, windows)))]
    return Err("adversarial directory symlinks are unavailable".to_owned());
    Ok(())
}

#[cfg(unix)]
struct RetainedRootNamespaceSwap {
    configured: PathBuf,
    retained: PathBuf,
}

#[cfg(unix)]
impl RetainedRootNamespaceSwap {
    fn replace_with_decoy(configured: &Path, decoy: &Path) -> Result<Self, String> {
        let retained =
            configured.with_file_name(format!("retained-import-root-{}", unique_suffix()));
        fs::rename(configured, &retained)
            .map_err(|_| "rename retained import root for SYM-13".to_owned())?;
        if std::os::unix::fs::symlink(decoy, configured).is_err() {
            let _ = fs::rename(&retained, configured);
            return Err("substitute SYM-13 decoy root".to_owned());
        }
        Ok(Self {
            configured: configured.to_owned(),
            retained,
        })
    }

    fn restore(mut self) -> Result<(), String> {
        fs::remove_file(&self.configured)
            .map_err(|_| "remove SYM-13 decoy root substitution".to_owned())?;
        fs::rename(&self.retained, &self.configured)
            .map_err(|_| "restore SYM-13 import root namespace".to_owned())?;
        self.retained.clear();
        Ok(())
    }
}

#[cfg(unix)]
impl Drop for RetainedRootNamespaceSwap {
    fn drop(&mut self) {
        if self.retained.as_os_str().is_empty() {
            return;
        }
        let _ = fs::remove_file(&self.configured);
        let _ = fs::rename(&self.retained, &self.configured);
    }
}

/// Proves that an already-opened import root keeps its authority when the
/// configured pathname is replaced after server startup.
#[cfg(unix)]
async fn run_sym13(
    driver: &mut impl McpDriver,
    run: &ArtifactAdversarialRun<'_>,
) -> Result<(), String> {
    let original = fs::read(
        run.policy
            .import_root()
            .join(ArtifactPolicyFixture::FILE_SOURCE),
    )
    .map_err(|_| "read SYM-13 original fixture payload".to_owned())?;
    let decoy = run
        .policy
        .base
        .join(format!("sym13-decoy-{}", unique_suffix()));
    fs::create_dir(&decoy).map_err(|_| "create SYM-13 decoy root".to_owned())?;
    let decoy_source = decoy.join(ArtifactPolicyFixture::FILE_SOURCE);
    let decoy_bytes = b"SYM-13 decoy payload";
    fs::write(&decoy_source, decoy_bytes).map_err(|_| "write SYM-13 decoy payload".to_owned())?;
    secure_directories(&[&decoy])?;
    secure_files(std::slice::from_ref(&decoy_source))?;
    let swap = RetainedRootNamespaceSwap::replace_with_decoy(run.policy.import_root(), &decoy)?;
    let result = async {
        let imported = driver
            .call_tool(
                "file_import",
                file_import_arguments(
                    &run.ctx.space_id,
                    local_source(
                        ArtifactPolicyFixture::IMPORT_ROOT,
                        ArtifactPolicyFixture::FILE_SOURCE,
                    ),
                    "sym13-retained-root.bin",
                    Some(ARTIFACT_FILE_MEDIA_TYPE),
                ),
            )
            .await?;
        let file_id = required_str(&imported, "/file_id")?;
        run.ctx.register_file(&file_id);
        let export_name = format!("sym13-export-{}", unique_suffix());
        driver
            .call_tool(
                "file_export",
                json!({
                    "space": run.ctx.space_id,
                    "file_id": file_id,
                    "destination": local_destination(ArtifactPolicyFixture::EXPORT_ROOT, &export_name),
                    "idempotency_key": format!("sym13-export-{}", unique_suffix()),
                }),
            )
            .await?;
        let exported = run.policy.read_export(&export_name)?;
        if artifact_sha256(&exported) != artifact_sha256(&original) {
            return Err("SYM-13 imported bytes from the substituted root".to_owned());
        }
        Ok(())
    }
    .await;
    let restored = swap.restore();
    if fs::read(&decoy_source).ok().as_deref() != Some(decoy_bytes) {
        return Err("SYM-13 changed the decoy fixture payload".to_owned());
    }
    result.and(restored)
}

/// Submits an ADS colon through the native Windows encoding and proves that
/// grammar validation rejects it before the retained-root boundary.
#[cfg(windows)]
async fn run_sym10(
    driver: &mut impl McpDriver,
    run: &ArtifactAdversarialRun<'_>,
) -> Result<(), String> {
    let root_access = run
        .root_access_attempts
        .ok_or_else(|| "SYM-10 requires retained-root access evidence".to_owned())?;
    let before = root_access();
    let native = native_wtf16(
        "file.bin:evil"
            .encode_utf16()
            .collect::<Vec<_>>()
            .as_slice(),
    );
    let encoded = native_submission_value(&native)?.to_owned();
    adversarial_refusal(
        driver,
        "file_import",
        file_import_arguments(
            &run.ctx.space_id,
            json!({"local": {
                "root": ArtifactPolicyFixture::IMPORT_ROOT,
                "path_native": native,
            }}),
            "sym10.bin",
            None,
        ),
        ExpectedToolErrorKind::Validation,
        &[&encoded],
    )
    .await?;
    if root_access() != before {
        return Err("SYM-10 reached the retained-root boundary".to_owned());
    }
    Ok(())
}

/// Proves that this fixture volume reports a stable hard-link identity and a
/// reliable link count before any hard-link scenario relies on either fact.
///
/// The probe is deliberately local to the fixture root. A successful
/// `hard_link` alone is insufficient evidence: the two entries must report
/// the same platform identity and an independently observed count transition
/// from two back to one after the alias is removed.
fn prove_hard_link_capability(root: &Path) -> Result<bool, String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;

        let origin = root.join(format!(".hlink-origin-{}", unique_suffix()));
        let alias = root.join(format!(".hlink-alias-{}", unique_suffix()));
        fs::write(&origin, b"any-mcp-hard-link-capability")
            .map_err(|_| "seed hard-link capability probe".to_owned())?;
        let result = (|| {
            fs::hard_link(&origin, &alias)
                .map_err(|_| "create hard-link capability probe".to_owned())?;
            let original = fs::metadata(&origin)
                .map_err(|_| "inspect hard-link capability origin".to_owned())?;
            let linked = fs::metadata(&alias)
                .map_err(|_| "inspect hard-link capability alias".to_owned())?;
            if original.dev() != linked.dev()
                || original.ino() != linked.ino()
                || original.nlink() != 2
                || linked.nlink() != 2
            {
                return Ok(false);
            }
            fs::remove_file(&alias).map_err(|_| "unlink hard-link capability alias".to_owned())?;
            let restored = fs::metadata(&origin)
                .map_err(|_| "inspect restored hard-link capability".to_owned())?;
            Ok(restored.nlink() == 1)
        })();
        let _ = fs::remove_file(&alias);
        let _ = fs::remove_file(&origin);
        result
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;

        let origin = root.join(format!(".hlink-origin-{}", unique_suffix()));
        let alias = root.join(format!(".hlink-alias-{}", unique_suffix()));
        fs::write(&origin, b"any-mcp-hard-link-capability")
            .map_err(|_| "seed hard-link capability probe".to_owned())?;
        let result = (|| {
            fs::hard_link(&origin, &alias)
                .map_err(|_| "create hard-link capability probe".to_owned())?;
            let original = fs::metadata(&origin)
                .map_err(|_| "inspect hard-link capability origin".to_owned())?;
            let linked = fs::metadata(&alias)
                .map_err(|_| "inspect hard-link capability alias".to_owned())?;
            if original.volume_serial_number().is_none()
                || original.file_index().is_none()
                || original.volume_serial_number() != linked.volume_serial_number()
                || original.file_index() != linked.file_index()
                || original.number_of_links() != Some(2)
                || linked.number_of_links() != Some(2)
            {
                return Ok(false);
            }
            fs::remove_file(&alias).map_err(|_| "unlink hard-link capability alias".to_owned())?;
            let restored = fs::metadata(&origin)
                .map_err(|_| "inspect restored hard-link capability".to_owned())?;
            Ok(restored.number_of_links() == Some(1))
        })();
        let _ = fs::remove_file(&alias);
        let _ = fs::remove_file(&origin);
        result
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = root;
        Ok(false)
    }
}

async fn run_sym01(
    driver: &mut impl McpDriver,
    run: &ArtifactAdversarialRun<'_>,
) -> Result<(), String> {
    let outside = run
        .policy
        .base
        .join(format!("sym01-outside-{}", unique_suffix()));
    let link_name = format!("sym01-link-{}", unique_suffix());
    let link = run.policy.import.join(&link_name);
    fs::write(&outside, b"SYM-01 outside sentinel")
        .map_err(|_| "seed SYM-01 outside file".to_owned())?;
    secure_files(std::slice::from_ref(&outside))?;
    create_file_symlink(&outside, &link)?;
    let before = observe_file(&outside)?;
    adversarial_refusal(
        driver,
        "file_import",
        file_import_arguments(
            &run.ctx.space_id,
            local_source(ArtifactPolicyFixture::IMPORT_ROOT, &link_name),
            "sym01.bin",
            None,
        ),
        ExpectedToolErrorKind::NotFound,
        &[&link_name],
    )
    .await?;
    if observe_file(&outside)? != before {
        return Err("SYM-01 read or changed the outside file".to_owned());
    }
    Ok(())
}

async fn run_hlink01(
    driver: &mut impl McpDriver,
    run: &ArtifactAdversarialRun<'_>,
) -> Result<(), String> {
    let outside = run
        .policy
        .base
        .join(format!("hlink01-outside-{}", unique_suffix()));
    let link_name = format!("hlink01-link-{}", unique_suffix());
    let link = run.policy.import.join(&link_name);
    fs::write(&outside, b"HLINK-01 outside sentinel")
        .map_err(|_| "seed HLINK-01 outside file".to_owned())?;
    secure_files(std::slice::from_ref(&outside))?;
    fs::hard_link(&outside, &link).map_err(|_| "create HLINK-01 hard link".to_owned())?;
    let before = observe_file(&outside)?;
    adversarial_refusal(
        driver,
        "file_import",
        file_import_arguments(
            &run.ctx.space_id,
            local_source(ArtifactPolicyFixture::IMPORT_ROOT, &link_name),
            "hlink01.bin",
            None,
        ),
        ExpectedToolErrorKind::NotFound,
        &[&link_name],
    )
    .await?;
    if observe_file(&outside)? != before {
        return Err("HLINK-01 read or changed the outside file".to_owned());
    }
    Ok(())
}

/// Mutates a source pathname only after the import has consumed a real first
/// chunk. The retained source descriptor must make every variant fail as a
/// conflict, with no second upload or object left behind.
async fn run_import_gate_race(
    driver: &mut impl McpDriver,
    run: &ArtifactAdversarialRun<'_>,
    id: AdversarialCaseId,
    mutation: ImportRaceMutation,
) -> Result<(), String> {
    let gates = run
        .gate_hooks
        .ok_or_else(|| "dynamic import race requires direct acceptance gates".to_owned())?;
    let source_name = format!(
        "{}-source-{}",
        id.as_str().to_ascii_lowercase(),
        unique_suffix()
    );
    let source = run.policy.import_root().join(&source_name);
    let bytes = vec![0x41; ACCEPTANCE_TRANSFER_CHUNK_BYTES.saturating_mul(2)];
    fs::write(&source, &bytes).map_err(|_| "seed gated import race source".to_owned())?;
    secure_files(std::slice::from_ref(&source))?;
    let key = format!(
        "{}-gate-{}",
        id.as_str().to_ascii_lowercase(),
        unique_suffix()
    );
    let mut lease = gates.arm_import(&key).await?;
    let request = driver.call_tool(
        "file_import",
        json!({
            "space": run.ctx.space_id,
            "source": local_source(ArtifactPolicyFixture::IMPORT_ROOT, &source_name),
            "name": format!("{source_name}.bin"),
            "media_type": ARTIFACT_FILE_MEDIA_TYPE,
            "idempotency_key": key,
        }),
    );
    tokio::pin!(request);
    tokio::select! {
        reached = lease.wait(Duration::from_secs(10)) => {
            if !reached {
                return Err("dynamic import race did not reach its exact gate".to_owned());
            }
        }
        _ = &mut request => return Err("dynamic import race completed before its gate".to_owned()),
    }
    let decoy = run
        .policy
        .base
        .join(format!("{}-decoy-{}", id.as_str(), unique_suffix()));
    fs::write(&decoy, b"adversarial replacement")
        .map_err(|_| "seed gated import race replacement".to_owned())?;
    secure_files(std::slice::from_ref(&decoy))?;
    match mutation {
        ImportRaceMutation::Replace => fs::write(&source, b"replaced source"),
        ImportRaceMutation::Rename => {
            let moved = source.with_extension("moved");
            fs::rename(&source, &moved).and_then(|_| fs::write(&source, b"replacement"))
        }
        ImportRaceMutation::Symlink => {
            let moved = source.with_extension("moved");
            fs::rename(&source, &moved)
                .and_then(|_| create_file_symlink(&decoy, &source).map_err(std::io::Error::other))
        }
        ImportRaceMutation::HardLink => {
            let moved = source.with_extension("moved");
            fs::rename(&source, &moved).and_then(|_| fs::hard_link(&decoy, &source))
        }
    }
    .map_err(|_| "apply gated import race mutation".to_owned())?;
    lease.release();
    if request.await.is_ok() {
        return Err("dynamic import race accepted a changed source".to_owned());
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum ImportRaceMutation {
    Replace,
    Rename,
    Symlink,
    HardLink,
}

fn raw_content_range(start: u64, end: u64, total: u64) -> Result<HeaderValue, String> {
    format!("bytes {start}-{end}/{total}")
        .parse()
        .map_err(|_| "encode raw staging content range".to_owned())
}

async fn raw_stage_head_offset(
    client: &RawStagingClient,
    allocation: &ArtifactStageAllocation,
) -> Result<u64, String> {
    let outcome = client
        .send(
            Method::HEAD,
            allocation.url(),
            allocation.handle(),
            &[],
            Vec::new(),
        )
        .await?;
    if outcome.status != 200 {
        return Err("raw staging HEAD did not return success".to_owned());
    }
    outcome
        .upload_offset
        .ok_or_else(|| "raw staging HEAD omitted committed offset".to_owned())
}

/// Runs RACE-07 through RACE-08, whose protocol interleavings need no
/// filesystem synchronization seam.
async fn run_raw_staging_races(
    driver: &mut impl McpDriver,
    run: &ArtifactAdversarialRun<'_>,
    execution: &mut AdversarialExecution,
) -> Result<(), String> {
    let payload = vec![0x5a; ACCEPTANCE_TRANSFER_CHUNK_BYTES.saturating_add(32)];
    let total = u64::try_from(payload.len())
        .map_err(|_| "RACE-07 payload exceeds addressable range".to_owned())?;
    let allocation = allocate_stage_upload(
        driver,
        &run.ctx.space_id,
        total,
        ARTIFACT_FILE_MEDIA_TYPE,
        Some(&artifact_sha256(&payload)),
    )
    .await?;
    execution.record_forbidden_log_needle(allocation.handle().as_bytes())?;
    let client = RawStagingClient::new()?;
    let before = raw_stage_head_offset(&client, &allocation).await?;
    let ahead_start = u64::try_from(ACCEPTANCE_TRANSFER_CHUNK_BYTES)
        .map_err(|_| "RACE-07 offset exceeds addressable range".to_owned())?;
    let ahead_end = ahead_start
        .checked_add(15)
        .and_then(|value| value.checked_sub(1))
        .ok_or_else(|| "RACE-07 range overflow".to_owned())?;
    let ahead = client
        .send(
            Method::PUT,
            allocation.url(),
            allocation.handle(),
            &[
                (
                    CONTENT_TYPE,
                    HeaderValue::from_static(ARTIFACT_FILE_MEDIA_TYPE),
                ),
                (
                    CONTENT_RANGE,
                    raw_content_range(ahead_start, ahead_end, total)?,
                ),
            ],
            vec![0x5a; 16],
        )
        .await?;
    ExpectedOutcome::Http {
        status: 409,
        body: b"conflict\n",
    }
    .assert_matches(ObservedOutcome::Http {
        status: ahead.status,
        body: &ahead.body,
    })?;
    if raw_stage_head_offset(&client, &allocation).await? != before {
        return Err("RACE-07 advanced the committed offset".to_owned());
    }
    release_stage_upload(driver, &allocation).await?;
    execution.record_executed(AdversarialCaseId::Race07)?;

    let overlap = b"RACE-08 overlapping staging bytes".to_vec();
    let overlap_total = u64::try_from(overlap.len())
        .map_err(|_| "RACE-08 payload exceeds addressable range".to_owned())?;
    let allocation = allocate_stage_upload(
        driver,
        &run.ctx.space_id,
        overlap_total,
        ARTIFACT_FILE_MEDIA_TYPE,
        Some(&artifact_sha256(&overlap)),
    )
    .await?;
    execution.record_forbidden_log_needle(allocation.handle().as_bytes())?;
    let end = overlap_total
        .checked_sub(1)
        .ok_or_else(|| "RACE-08 payload was empty".to_owned())?;
    let headers = vec![
        (
            CONTENT_TYPE,
            HeaderValue::from_static(ARTIFACT_FILE_MEDIA_TYPE),
        ),
        (CONTENT_RANGE, raw_content_range(0, end, overlap_total)?),
    ];
    let arrived = Arc::new(tokio::sync::Barrier::new(3));
    let release = Arc::new(tokio::sync::Barrier::new(3));
    let first_body = overlap.clone();
    let first = tokio::spawn({
        let client = RawStagingClient::new()?;
        let url = allocation.url().to_owned();
        let handle = Zeroizing::new(allocation.handle().to_owned());
        let headers = headers.clone();
        let arrived = Arc::clone(&arrived);
        let release = Arc::clone(&release);
        async move {
            client
                .send_with_body_barrier(
                    Method::PUT,
                    url,
                    handle,
                    headers,
                    first_body,
                    arrived,
                    release,
                )
                .await
        }
    });
    let second = tokio::spawn({
        let client = RawStagingClient::new()?;
        let url = allocation.url().to_owned();
        let handle = Zeroizing::new(allocation.handle().to_owned());
        let arrived = Arc::clone(&arrived);
        let release = Arc::clone(&release);
        async move {
            client
                .send_with_body_barrier(
                    Method::PUT,
                    url,
                    handle,
                    headers,
                    overlap,
                    arrived,
                    release,
                )
                .await
        }
    });
    tokio::time::timeout(Duration::from_secs(5), arrived.wait())
        .await
        .map_err(|_| "RACE-08 requests did not reach their shared body barrier".to_owned())?;
    tokio::time::timeout(Duration::from_secs(5), release.wait())
        .await
        .map_err(|_| "RACE-08 requests did not leave their shared body barrier".to_owned())?;
    let first = first
        .await
        .map_err(|_| "RACE-08 first request task ended unexpectedly".to_owned())??;
    let second = second
        .await
        .map_err(|_| "RACE-08 second request task ended unexpectedly".to_owned())??;
    let outcomes = [&first, &second];
    let accepted = outcomes
        .iter()
        .filter(|outcome| outcome.status == 201)
        .count();
    let conflicted = outcomes
        .iter()
        .filter(|outcome| outcome.status == 409)
        .count();
    if accepted != 1 || conflicted != 1 {
        return Err("RACE-08 did not produce one accepted and one conflict response".to_owned());
    }
    for outcome in outcomes.iter().filter(|outcome| outcome.status == 409) {
        ExpectedOutcome::Http {
            status: 409,
            body: b"conflict\n",
        }
        .assert_matches(ObservedOutcome::Http {
            status: outcome.status,
            body: &outcome.body,
        })?;
    }
    if raw_stage_head_offset(&client, &allocation).await? != overlap_total {
        return Err("RACE-08 final committed offset diverged from its declaration".to_owned());
    }
    release_stage_upload(driver, &allocation).await?;
    execution.record_executed(AdversarialCaseId::Race08)?;
    Ok(())
}

/// Exercises the hostile-link release lifecycle after the shared capability
/// probe has established that link counts are meaningful on this volume.
#[cfg(unix)]
async fn run_hlink05(
    driver: &mut impl McpDriver,
    run: &ArtifactAdversarialRun<'_>,
    execution: &mut AdversarialExecution,
) -> Result<(), String> {
    let payload = b"HLINK-05 staged payload";
    let quota_before = adversarial_quota_snapshot(driver).await?;
    let allocation = allocate_stage_upload(
        driver,
        &run.ctx.space_id,
        u64::try_from(payload.len())
            .map_err(|_| "HLINK-05 payload exceeds addressable range".to_owned())?,
        ARTIFACT_FILE_MEDIA_TYPE,
        Some(&artifact_sha256(payload)),
    )
    .await?;
    execution.record_forbidden_log_needle(allocation.handle().as_bytes())?;
    upload_stage_bytes(&allocation, payload, ARTIFACT_FILE_MEDIA_TYPE).await?;
    let quota_reserved = adversarial_quota_snapshot(driver).await?;
    if quota_reserved == quota_before {
        return Err("HLINK-05 allocation did not reserve staging quota".to_owned());
    }
    let record = run
        .policy
        .staging
        .join(format!("{}.bin", allocation.record()));
    let outside = run
        .policy
        .base
        .join(format!("hlink05-outside-{}", unique_suffix()));
    fs::hard_link(&record, &outside).map_err(|_| "create HLINK-05 outside link".to_owned())?;
    if fs::read(&outside).ok().as_deref() != Some(payload)
        || fs::read(&record).ok().as_deref() != Some(payload)
    {
        return Err("HLINK-05 link setup did not preserve staged bytes".to_owned());
    }
    adversarial_refusal(
        driver,
        "artifact_release",
        json!({"handle": allocation.handle()}),
        ExpectedToolErrorKind::Conflict,
        &[],
    )
    .await?;
    if fs::read(&outside).ok().as_deref() != Some(payload)
        || fs::read(&record).ok().as_deref() != Some(payload)
        || adversarial_quota_snapshot(driver).await? != quota_reserved
    {
        return Err("HLINK-05 failed cleanup did not retain its ownership state".to_owned());
    }
    fs::remove_file(&outside).map_err(|_| "remove HLINK-05 outside link".to_owned())?;
    release_stage_upload(driver, &allocation).await?;
    if run.policy.staging_snapshot()?.is_reaped() == false
        || adversarial_quota_snapshot(driver).await? != quota_before
    {
        return Err("HLINK-05 retry did not reap record and restore quota".to_owned());
    }
    execution.record_executed(AdversarialCaseId::Hlink05)?;
    Ok(())
}

/// Executes the currently closed dynamic-filesystem rows against one router.
///
/// Race rows remain absent until their production synchronization seams are
/// available; callers' exact-owner assertion therefore fails closed rather
/// than reporting unobserved cases.
///
/// # Errors
///
/// Returns a fixed category on the first production outcome, invariant, or
/// quota mismatch.
pub async fn run_artifact_dynamic_filesystem_cases(
    driver: &mut impl McpDriver,
    run: &ArtifactAdversarialRun<'_>,
) -> Result<AdversarialExecution, String> {
    let quota_before = adversarial_quota_snapshot(driver).await?;
    let objects_before = artifact_object_ids(run.ctx).await?;
    let mut execution = AdversarialExecution::default();

    run_sym01(driver, run).await?;
    execution.record_executed(AdversarialCaseId::Sym01)?;

    let sym02_name = format!("sym02-link-{}", unique_suffix());
    create_file_symlink(
        &run.policy.import.join(ArtifactPolicyFixture::FILE_SOURCE),
        &run.policy.import.join(&sym02_name),
    )?;
    adversarial_refusal(
        driver,
        "file_import",
        file_import_arguments(
            &run.ctx.space_id,
            local_source(ArtifactPolicyFixture::IMPORT_ROOT, &sym02_name),
            "sym02.bin",
            None,
        ),
        ExpectedToolErrorKind::NotFound,
        &[&sym02_name],
    )
    .await?;
    execution.record_executed(AdversarialCaseId::Sym02)?;

    let sym03_target = run
        .policy
        .import
        .join(format!("sym03-target-{}", unique_suffix()));
    let sym03_link_name = format!("sym03-link-{}", unique_suffix());
    fs::create_dir(&sym03_target).map_err(|_| "create SYM-03 target directory".to_owned())?;
    secure_directories(&[&sym03_target])?;
    fs::write(sym03_target.join("source.bin"), b"SYM-03 source")
        .map_err(|_| "seed SYM-03 source".to_owned())?;
    secure_files(&[sym03_target.join("source.bin")])?;
    create_directory_symlink(&sym03_target, &run.policy.import.join(&sym03_link_name))?;
    let sym03_path = format!("{sym03_link_name}/source.bin");
    adversarial_refusal(
        driver,
        "file_import",
        file_import_arguments(
            &run.ctx.space_id,
            local_source(ArtifactPolicyFixture::IMPORT_ROOT, &sym03_path),
            "sym03.bin",
            None,
        ),
        ExpectedToolErrorKind::NotFound,
        &[&sym03_path],
    )
    .await?;
    execution.record_executed(AdversarialCaseId::Sym03)?;

    let sym04_name = format!("sym04-link-{}", unique_suffix());
    create_file_symlink(
        &run.policy
            .base
            .join(format!("sym04-missing-{}", unique_suffix())),
        &run.policy.import.join(&sym04_name),
    )?;
    adversarial_refusal(
        driver,
        "file_import",
        file_import_arguments(
            &run.ctx.space_id,
            local_source(ArtifactPolicyFixture::IMPORT_ROOT, &sym04_name),
            "sym04.bin",
            None,
        ),
        ExpectedToolErrorKind::NotFound,
        &[&sym04_name],
    )
    .await?;
    execution.record_executed(AdversarialCaseId::Sym04)?;

    let file_id = adversarial_seed_file(driver, run, "dynamic-export-source.bin").await?;
    let sym05_escape = run
        .policy
        .base
        .join(format!("sym05-escape-{}", unique_suffix()));
    let sym05_link_name = format!("sym05-link-{}", unique_suffix());
    fs::create_dir(&sym05_escape).map_err(|_| "create SYM-05 escape directory".to_owned())?;
    secure_directories(&[&sym05_escape])?;
    create_directory_symlink(&sym05_escape, &run.policy.export.join(&sym05_link_name))?;
    let sym05_path = format!("{sym05_link_name}/out.bin");
    adversarial_refusal(
        driver,
        "file_export",
        json!({
            "space": run.ctx.space_id,
            "file_id": file_id,
            "destination": local_destination(ArtifactPolicyFixture::EXPORT_ROOT, &sym05_path),
            "idempotency_key": format!("sym05-export-{}", unique_suffix()),
        }),
        ExpectedToolErrorKind::NotFound,
        &[&sym05_path],
    )
    .await?;
    if fs::read_dir(&sym05_escape)
        .map_err(|_| "inspect SYM-05 escape directory".to_owned())?
        .next()
        .is_some()
    {
        return Err("SYM-05 changed the escape directory".to_owned());
    }
    execution.record_executed(AdversarialCaseId::Sym05)?;

    let sym06_target = run
        .policy
        .base
        .join(format!("sym06-target-{}", unique_suffix()));
    let sym06_name = format!("sym06-link-{}", unique_suffix());
    let sym06_bytes = b"SYM-06 escape sentinel";
    fs::write(&sym06_target, sym06_bytes).map_err(|_| "seed SYM-06 target".to_owned())?;
    secure_files(std::slice::from_ref(&sym06_target))?;
    create_file_symlink(&sym06_target, &run.policy.export.join(&sym06_name))?;
    adversarial_refusal(
        driver,
        "file_export",
        json!({
            "space": run.ctx.space_id,
            "file_id": file_id,
            "destination": local_destination(ArtifactPolicyFixture::EXPORT_ROOT, &sym06_name),
            "idempotency_key": format!("sym06-export-{}", unique_suffix()),
        }),
        ExpectedToolErrorKind::Conflict,
        &[&sym06_name],
    )
    .await?;
    if fs::read(&sym06_target).ok().as_deref() != Some(sym06_bytes) {
        return Err("SYM-06 changed the escape target".to_owned());
    }
    execution.record_executed(AdversarialCaseId::Sym06)?;

    let sym09_path = "file.bin:evil";
    let root_access = run
        .root_access_attempts
        .ok_or_else(|| "SYM-09 requires retained-root access evidence".to_owned())?;
    let root_access_before = root_access();
    adversarial_refusal(
        driver,
        "file_import",
        file_import_arguments(
            &run.ctx.space_id,
            local_source(ArtifactPolicyFixture::IMPORT_ROOT, sym09_path),
            "sym09.bin",
            None,
        ),
        ExpectedToolErrorKind::Validation,
        &[sym09_path],
    )
    .await?;
    if root_access() != root_access_before {
        return Err("SYM-09 reached the retained-root boundary".to_owned());
    }
    execution.record_executed(AdversarialCaseId::Sym09)?;

    #[cfg(windows)]
    {
        run_sym10(driver, run).await?;
        execution.record_executed(AdversarialCaseId::Sym10)?;
    }

    #[cfg(unix)]
    {
        run_sym13(driver, run).await?;
        execution.record_executed(AdversarialCaseId::Sym13)?;
    }

    run_import_gate_race(
        driver,
        run,
        AdversarialCaseId::Race01,
        ImportRaceMutation::Replace,
    )
    .await?;
    execution.record_executed(AdversarialCaseId::Race01)?;
    run_import_gate_race(
        driver,
        run,
        AdversarialCaseId::Race02,
        ImportRaceMutation::Rename,
    )
    .await?;
    execution.record_executed(AdversarialCaseId::Race02)?;
    run_import_gate_race(
        driver,
        run,
        AdversarialCaseId::Race03,
        ImportRaceMutation::Symlink,
    )
    .await?;
    execution.record_executed(AdversarialCaseId::Race03)?;
    run_import_gate_race(
        driver,
        run,
        AdversarialCaseId::Race06,
        ImportRaceMutation::HardLink,
    )
    .await?;
    execution.record_executed(AdversarialCaseId::Race06)?;

    run_raw_staging_races(driver, run, &mut execution).await?;

    // Every hard-link row shares this capability observation. A filesystem
    // that cannot prove identity and the 2-to-1 count transition contributes
    // an explicit unsupported partition instead of turning a missing
    // primitive into an apparent product regression.
    if !prove_hard_link_capability(run.policy.import_root()).unwrap_or(false) {
        for id in [
            AdversarialCaseId::Hlink01,
            AdversarialCaseId::Hlink02,
            AdversarialCaseId::Hlink03,
            AdversarialCaseId::Hlink04,
            AdversarialCaseId::Hlink05,
            AdversarialCaseId::Hlink06,
        ] {
            execution.record_unsupported_with_reason(id, "link_count_unavailable")?;
        }
    } else {
        execution.record_executed(AdversarialCaseId::Hlink06)?;
        run_hlink01(driver, run).await?;
        execution.record_executed(AdversarialCaseId::Hlink01)?;

        let hlink02_name = format!("hlink02-link-{}", unique_suffix());
        fs::hard_link(
            run.policy.import.join(ArtifactPolicyFixture::FILE_SOURCE),
            run.policy.import.join(&hlink02_name),
        )
        .map_err(|_| "create HLINK-02 hard link".to_owned())?;
        adversarial_refusal(
            driver,
            "file_import",
            file_import_arguments(
                &run.ctx.space_id,
                local_source(ArtifactPolicyFixture::IMPORT_ROOT, &hlink02_name),
                "hlink02.bin",
                None,
            ),
            ExpectedToolErrorKind::NotFound,
            &[&hlink02_name],
        )
        .await?;
        execution.record_executed(AdversarialCaseId::Hlink02)?;

        let hlink04_target = run
            .policy
            .base
            .join(format!("hlink04-target-{}", unique_suffix()));
        let hlink04_name = format!("hlink04-link-{}", unique_suffix());
        let hlink04_bytes = b"HLINK-04 outside sentinel";
        fs::write(&hlink04_target, hlink04_bytes).map_err(|_| "seed HLINK-04 target".to_owned())?;
        secure_files(std::slice::from_ref(&hlink04_target))?;
        fs::hard_link(&hlink04_target, run.policy.export.join(&hlink04_name))
            .map_err(|_| "create HLINK-04 hard link".to_owned())?;
        adversarial_refusal(
            driver,
            "file_export",
            json!({
                "space": run.ctx.space_id,
                "file_id": file_id,
                "destination": local_destination(ArtifactPolicyFixture::EXPORT_ROOT, &hlink04_name),
                "idempotency_key": format!("hlink04-export-{}", unique_suffix()),
            }),
            ExpectedToolErrorKind::Conflict,
            &[&hlink04_name],
        )
        .await?;
        if fs::read(&hlink04_target).ok().as_deref() != Some(hlink04_bytes) {
            return Err("HLINK-04 changed the outside file".to_owned());
        }
        execution.record_executed(AdversarialCaseId::Hlink04)?;

        #[cfg(unix)]
        run_hlink05(driver, run, &mut execution).await?;
    }

    let objects_after = artifact_object_ids(run.ctx).await?;
    if !objects_before.is_subset(&objects_after)
        || objects_after.len() < objects_before.len().saturating_add(1)
    {
        return Err("dynamic filesystem refusals changed the object inventory".to_owned());
    }
    finish_adversarial_quota(driver, quota_before, &mut execution).await?;
    Ok(execution)
}

/// Repeats the implemented stable-stdio dynamic filesystem sentinels.
///
/// # Errors
///
/// Returns a fixed category if either production refusal, filesystem
/// invariant, or staging quota diverges.
pub async fn run_artifact_dynamic_filesystem_stdio_sentinels(
    driver: &mut impl McpDriver,
    run: &ArtifactAdversarialRun<'_>,
) -> Result<AdversarialExecution, String> {
    let quota_before = adversarial_quota_snapshot(driver).await?;
    let objects_before = artifact_object_ids(run.ctx).await?;
    let mut execution = AdversarialExecution::default();
    run_sym01(driver, run).await?;
    execution.record_executed(AdversarialCaseId::Sym01)?;
    run_hlink01(driver, run).await?;
    execution.record_executed(AdversarialCaseId::Hlink01)?;
    if artifact_object_ids(run.ctx).await? != objects_before {
        return Err("dynamic stdio refusals changed the object inventory".to_owned());
    }
    finish_adversarial_quota(driver, quota_before, &mut execution).await?;
    Ok(execution)
}

/// Executes TRAV-01 through TRAV-18 against one production router.
///
/// TRAV-19 and TRAV-20 require distinct runtime root policies and are exposed
/// as separate helpers below. Every rejected argument is compared against the
/// complete canonical tool result and checked for submitted-text disclosure.
///
/// # Errors
///
/// Returns a fixed category on the first outcome, inventory, or cleanup
/// mismatch.
pub async fn run_artifact_traversal_default(
    driver: &mut impl McpDriver,
    run: &ArtifactAdversarialRun<'_>,
) -> Result<AdversarialExecution, String> {
    let quota_before = adversarial_quota_snapshot(driver).await?;
    let mut execution = AdversarialExecution::default();
    let space = run.ctx.space_id.as_str();
    let escape_sentinel = run.policy.base.join("escape.bin");
    fs::write(&escape_sentinel, b"traversal escape sentinel")
        .map_err(|_| "seed traversal escape sibling".to_owned())?;
    secure_files(std::slice::from_ref(&escape_sentinel))?;
    let fixture_before = RootInventory::capture(&run.policy.base)?;
    let import_before = RootInventory::capture(run.policy.import_root())?;
    let export_before = RootInventory::capture(run.policy.export_root())?;
    let object_ids_before = artifact_object_ids(run.ctx).await?;
    let root_access_before = run.root_access_attempts.map(|counter| counter());
    let portable = [
        (AdversarialCaseId::Trav01, "../escape.bin"),
        (AdversarialCaseId::Trav02, "/etc/passwd"),
        (AdversarialCaseId::Trav03, "safe/../../escape.bin"),
        (AdversarialCaseId::Trav04, "%2e%2e%2fescape.bin"),
        (AdversarialCaseId::Trav05, "..\\escape.bin"),
        (AdversarialCaseId::Trav06, "C:/escape.bin"),
        (AdversarialCaseId::Trav11, "a//file.bin"),
        (AdversarialCaseId::Trav12, "dir/"),
        (AdversarialCaseId::Trav14, "sub/../file.bin"),
    ];
    for (id, path) in portable {
        adversarial_refusal(
            driver,
            "file_import",
            file_import_arguments(
                space,
                local_source(ArtifactPolicyFixture::IMPORT_ROOT, path),
                "adversarial.bin",
                None,
            ),
            ExpectedToolErrorKind::Validation,
            &[path],
        )
        .await?;
        execution.record_executed(id)?;
    }

    let native_escape = native_relative("../escape.bin");
    let native_escape_value = native_submission_value(&native_escape)?.to_owned();
    adversarial_refusal(
        driver,
        "file_import",
        file_import_arguments(
            space,
            json!({"local": {
                "root": ArtifactPolicyFixture::IMPORT_ROOT,
                "path_native": native_escape,
            }}),
            "adversarial.bin",
            None,
        ),
        ExpectedToolErrorKind::Validation,
        &[&native_escape_value, "../escape.bin"],
    )
    .await?;
    execution.record_executed(AdversarialCaseId::Trav07)?;

    let native_same = native_relative(ArtifactPolicyFixture::FILE_SOURCE);
    let native_same_value = native_submission_value(&native_same)?.to_owned();
    adversarial_refusal(
        driver,
        "file_import",
        file_import_arguments(
            space,
            json!({"local": {
                "root": ArtifactPolicyFixture::IMPORT_ROOT,
                "path": ArtifactPolicyFixture::FILE_SOURCE,
                "path_native": native_same,
            }}),
            "adversarial.bin",
            None,
        ),
        ExpectedToolErrorKind::Validation,
        &[ArtifactPolicyFixture::FILE_SOURCE, &native_same_value],
    )
    .await?;
    adversarial_refusal(
        driver,
        "file_import",
        file_import_arguments(
            space,
            json!({"local": {"root": ArtifactPolicyFixture::IMPORT_ROOT}}),
            "adversarial.bin",
            None,
        ),
        ExpectedToolErrorKind::Validation,
        &[],
    )
    .await?;
    execution.record_executed(AdversarialCaseId::Trav08)?;

    let native_control = native_relative("safe\nfile.bin");
    let native_control_value = native_submission_value(&native_control)?.to_owned();
    adversarial_refusal(
        driver,
        "file_import",
        file_import_arguments(
            space,
            json!({"local": {
                "root": ArtifactPolicyFixture::IMPORT_ROOT,
                "path_native": native_control,
            }}),
            "adversarial.bin",
            None,
        ),
        ExpectedToolErrorKind::Validation,
        &[&native_control_value, "safe\nfile.bin"],
    )
    .await?;
    execution.record_executed(AdversarialCaseId::Trav09)?;

    for path in ["a".repeat(4_097), "b".repeat(256)] {
        adversarial_refusal(
            driver,
            "file_import",
            file_import_arguments(
                space,
                local_source(ArtifactPolicyFixture::IMPORT_ROOT, &path),
                "adversarial.bin",
                None,
            ),
            ExpectedToolErrorKind::Validation,
            &[],
        )
        .await?;
    }
    execution.record_executed(AdversarialCaseId::Trav10)?;

    import_before.assert_unchanged()?;
    export_before.assert_unchanged()?;
    fixture_before.assert_unchanged()?;
    if artifact_object_ids(run.ctx).await? != object_ids_before {
        return Err("TRAV malformed paths changed the Anytype object inventory".to_owned());
    }

    if let Some(before) = root_access_before {
        let after = run
            .root_access_attempts
            .map(|counter| counter())
            .ok_or_else(|| "adversarial root counter disappeared".to_owned())?;
        if after != before {
            return Err("TRAV malformed paths reached the retained-root boundary".to_owned());
        }
    }

    let file_id = adversarial_seed_file(driver, run, "adversarial-seed.bin").await?;
    let fixture_before = RootInventory::capture(&run.policy.base)?;
    let objects_before = artifact_object_ids(run.ctx).await?;
    let export_before = run.policy.export_snapshot()?;
    let traversal_export = json!({
        "space": space,
        "file_id": file_id,
        "destination": local_destination(
            ArtifactPolicyFixture::EXPORT_ROOT,
            "../escape.out",
        ),
        "idempotency_key": format!("adversarial-export-{}", unique_suffix()),
    });
    for _ in 0..2 {
        adversarial_refusal(
            driver,
            "file_export",
            traversal_export.clone(),
            ExpectedToolErrorKind::Validation,
            &["../escape.out"],
        )
        .await?;
    }
    if run.policy.export_snapshot()? != export_before
        || artifact_object_ids(run.ctx).await? != objects_before
    {
        return Err("TRAV-13 changed the export root".to_owned());
    }
    fixture_before.assert_unchanged()?;
    execution.record_executed(AdversarialCaseId::Trav13)?;

    let fixture_before = RootInventory::capture(&run.policy.base)?;
    let import_before = RootInventory::capture(run.policy.import_root())?;
    let export_before = RootInventory::capture(run.policy.export_root())?;
    let objects_before = artifact_object_ids(run.ctx).await?;
    let capability_refusal = adversarial_refusal(
        driver,
        "file_export",
        json!({
            "space": space,
            "file_id": file_id,
            "destination": local_destination(
                ArtifactPolicyFixture::IMPORT_ROOT,
                "denied.out",
            ),
            "idempotency_key": format!("adversarial-export-{}", unique_suffix()),
        }),
        ExpectedToolErrorKind::NotFound,
        &[],
    )
    .await?;
    execution.record_executed(AdversarialCaseId::Trav15)?;

    let unknown_refusal = adversarial_refusal(
        driver,
        "file_import",
        file_import_arguments(
            space,
            local_source("nope", ArtifactPolicyFixture::FILE_SOURCE),
            "adversarial.bin",
            None,
        ),
        ExpectedToolErrorKind::NotFound,
        &[],
    )
    .await?;
    if capability_refusal.normalized_result() != unknown_refusal.normalized_result() {
        return Err("TRAV uniform not-found payloads diverged".to_owned());
    }
    execution.record_uniform_not_found_payload(capability_refusal.normalized_result())?;
    execution.record_uniform_not_found_payload(unknown_refusal.normalized_result())?;
    execution.record_executed(AdversarialCaseId::Trav16)?;
    fixture_before.assert_unchanged()?;
    import_before.assert_unchanged()?;
    export_before.assert_unchanged()?;
    if artifact_object_ids(run.ctx).await? != objects_before {
        return Err("TRAV capability denials changed the Anytype object inventory".to_owned());
    }

    let before_create = artifact_object_ids(run.ctx).await?;
    let fixture_before = RootInventory::capture(&run.policy.base)?;
    adversarial_refusal(
        driver,
        "document_import_create",
        json!({
            "space": space,
            "source": local_source(ArtifactPolicyFixture::IMPORT_ROOT, "../escape.md"),
            "source_format": "markdown",
            "object_type": "page",
            "name": "Adversarial traversal",
            "idempotency_key": format!("adversarial-document-{}", unique_suffix()),
        }),
        ExpectedToolErrorKind::Validation,
        &["../escape.md"],
    )
    .await?;
    if artifact_object_ids(run.ctx).await? != before_create {
        return Err("TRAV-17 created an Anytype object".to_owned());
    }
    fixture_before.assert_unchanged()?;
    execution.record_executed(AdversarialCaseId::Trav17)?;

    let object_id = adversarial_seed_document(driver, run).await?;
    let fixture_before = RootInventory::capture(&run.policy.base)?;
    let import_before = RootInventory::capture(run.policy.import_root())?;
    let objects_before = artifact_object_ids(run.ctx).await?;
    let export_before = run.policy.export_snapshot()?;
    let traversal_document_export = json!({
        "space": space,
        "object_id": object_id,
        "destination": local_destination(
            ArtifactPolicyFixture::EXPORT_ROOT,
            "../escape.md",
        ),
        "idempotency_key": format!("adversarial-document-export-{}", unique_suffix()),
    });
    for _ in 0..2 {
        adversarial_refusal(
            driver,
            "document_export",
            traversal_document_export.clone(),
            ExpectedToolErrorKind::Validation,
            &["../escape.md"],
        )
        .await?;
    }
    if run.policy.export_snapshot()? != export_before
        || artifact_object_ids(run.ctx).await? != objects_before
    {
        return Err("TRAV-18 changed the export root".to_owned());
    }
    fixture_before.assert_unchanged()?;
    import_before.assert_unchanged()?;
    execution.record_executed(AdversarialCaseId::Trav18)?;
    finish_adversarial_quota(driver, quota_before, &mut execution).await?;
    Ok(execution)
}

/// Executes TRAV-19 against a runtime whose frozen client-root set is empty.
pub async fn run_artifact_empty_client_roots_case(
    driver: &mut impl McpDriver,
    run: &ArtifactAdversarialRun<'_>,
) -> Result<AdversarialExecution, String> {
    let quota_before = adversarial_quota_snapshot(driver).await?;
    let fixture_before = RootInventory::capture(&run.policy.base)?;
    let import_before = RootInventory::capture(run.policy.import_root())?;
    let export_before = RootInventory::capture(run.policy.export_root())?;
    let objects_before = artifact_object_ids(run.ctx).await?;
    let refusal = adversarial_refusal(
        driver,
        "file_import",
        file_import_arguments(
            &run.ctx.space_id,
            local_source(
                ArtifactPolicyFixture::IMPORT_ROOT,
                ArtifactPolicyFixture::FILE_SOURCE,
            ),
            "adversarial.bin",
            None,
        ),
        ExpectedToolErrorKind::NotFound,
        &[],
    )
    .await?;
    fixture_before.assert_unchanged()?;
    import_before.assert_unchanged()?;
    export_before.assert_unchanged()?;
    if artifact_object_ids(run.ctx).await? != objects_before {
        return Err("TRAV-19 changed the Anytype object inventory".to_owned());
    }
    let mut execution = AdversarialExecution::default();
    execution.record_uniform_not_found_payload(refusal.normalized_result())?;
    execution.record_executed(AdversarialCaseId::Trav19)?;
    finish_adversarial_quota(driver, quota_before, &mut execution).await?;
    Ok(execution)
}

/// Executes TRAV-20 against a runtime with the artifacts toolset selected but
/// no configured roots.
pub async fn run_artifact_missing_roots_case(
    driver: &mut impl McpDriver,
    space_id: &str,
) -> Result<AdversarialExecution, String> {
    let quota_before = adversarial_quota_snapshot(driver).await?;
    adversarial_refusal(
        driver,
        "file_import",
        file_import_arguments(
            space_id,
            local_source("inbox", "valid.bin"),
            "adversarial.bin",
            None,
        ),
        ExpectedToolErrorKind::MissingRoots,
        &["valid.bin"],
    )
    .await?;
    let mut execution = AdversarialExecution::default();
    execution.record_executed(AdversarialCaseId::Trav20)?;
    finish_adversarial_quota(driver, quota_before, &mut execution).await?;
    Ok(execution)
}

/// Executes the default-policy alias cases. ALIAS-07 owns a deliberately
/// rejected child startup and is therefore completed by the process owner.
///
/// # Errors
///
/// Returns a fixed category if probed volume semantics, exact tool outcomes,
/// original bytes, or staged-record state diverge.
pub async fn run_artifact_alias_cases(
    driver: &mut impl McpDriver,
    run: &ArtifactAdversarialRun<'_>,
) -> Result<AdversarialExecution, String> {
    let quota_before = adversarial_quota_snapshot(driver).await?;
    let mut execution = AdversarialExecution::default();
    let space = run.ctx.space_id.as_str();
    let file_id = adversarial_seed_file(driver, run, "alias-seed.bin").await?;

    let case_suffix = unique_suffix();
    let lower = format!("alias-{case_suffix}-report.bin");
    let upper = lower.to_ascii_uppercase();
    let original = b"alias-original";
    fs::write(run.policy.export.join(&lower), original)
        .map_err(|_| "seed alias export fixture".to_owned())?;
    secure_files(std::slice::from_ref(&run.policy.export.join(&lower)))?;
    match probe_volume_case_folding(run.policy.export_root())? {
        VolumeCaseFolding::Insensitive => {
            adversarial_refusal(
                driver,
                "file_export",
                json!({
                    "space": space,
                    "file_id": file_id,
                    "destination": local_destination(ArtifactPolicyFixture::EXPORT_ROOT, &upper),
                    "idempotency_key": format!("alias-case-export-{}", unique_suffix()),
                }),
                ExpectedToolErrorKind::Conflict,
                &[],
            )
            .await?;
        }
        VolumeCaseFolding::Sensitive => {
            driver
                .call_tool(
                    "file_export",
                    json!({
                        "space": space,
                        "file_id": file_id,
                        "destination": local_destination(
                            ArtifactPolicyFixture::EXPORT_ROOT,
                            &upper,
                        ),
                        "idempotency_key": format!("alias-case-export-{}", unique_suffix()),
                    }),
                )
                .await?;
            if run.policy.read_export(&upper)? != ARTIFACT_FILE_PAYLOAD {
                return Err("ALIAS-01 distinct export bytes diverged".to_owned());
            }
        }
    }
    if run.policy.read_export(&lower)? != original {
        return Err("ALIAS-01 replaced the original file".to_owned());
    }
    execution.record_executed(AdversarialCaseId::Alias01)?;

    let nfc = format!("alias-{case_suffix}-caf\u{e9}.bin");
    let nfd = format!("alias-{case_suffix}-cafe\u{301}.bin");
    fs::write(run.policy.export.join(&nfc), original)
        .map_err(|_| "seed normalization fixture".to_owned())?;
    secure_files(std::slice::from_ref(&run.policy.export.join(&nfc)))?;
    match probe_volume_normalization(run.policy.export_root())? {
        VolumeNormalization::Equivalent => {
            adversarial_refusal(
                driver,
                "file_export",
                json!({
                    "space": space,
                    "file_id": file_id,
                    "destination": local_destination(ArtifactPolicyFixture::EXPORT_ROOT, &nfd),
                    "idempotency_key": format!("alias-normalization-{}", unique_suffix()),
                }),
                ExpectedToolErrorKind::Conflict,
                &[],
            )
            .await?;
        }
        VolumeNormalization::Distinct => {
            driver
                .call_tool(
                    "file_export",
                    json!({
                        "space": space,
                        "file_id": file_id,
                        "destination": local_destination(
                            ArtifactPolicyFixture::EXPORT_ROOT,
                            &nfd,
                        ),
                        "idempotency_key": format!("alias-normalization-{}", unique_suffix()),
                    }),
                )
                .await?;
            if run.policy.read_export(&nfd)? != ARTIFACT_FILE_PAYLOAD {
                return Err("ALIAS-02 distinct export bytes diverged".to_owned());
            }
        }
    }
    if run.policy.read_export(&nfc)? != original {
        return Err("ALIAS-02 replaced the original file".to_owned());
    }
    execution.record_executed(AdversarialCaseId::Alias02)?;

    #[cfg(windows)]
    {
        fs::write(run.policy.export.join("report.bin"), original)
            .map_err(|_| "seed Windows alias fixture".to_owned())?;
        secure_files(std::slice::from_ref(&run.policy.export.join("report.bin")))?;
        for destination in ["report.bin.", "report.bin "] {
            adversarial_refusal(
                driver,
                "file_export",
                json!({
                    "space": space,
                    "file_id": file_id,
                    "destination": local_destination(
                        ArtifactPolicyFixture::EXPORT_ROOT,
                        destination,
                    ),
                    "idempotency_key": format!("alias-windows-{}", unique_suffix()),
                }),
                ExpectedToolErrorKind::Validation,
                &[],
            )
            .await?;
        }
        if run.policy.read_export("report.bin")? != original {
            return Err("ALIAS-03 replaced the original file".to_owned());
        }
        execution.record_executed(AdversarialCaseId::Alias03)?;

        let long_name = format!("long-artifact-name-{case_suffix}.bin");
        let long_path = run.policy.export.join(&long_name);
        fs::write(&long_path, original)
            .map_err(|_| "seed Windows short-name fixture".to_owned())?;
        secure_files(std::slice::from_ref(&long_path))?;
        if let Some(short_name) = windows_short_alias(&long_path)? {
            adversarial_refusal(
                driver,
                "file_export",
                json!({
                    "space": space,
                    "file_id": file_id,
                    "destination": local_destination(
                        ArtifactPolicyFixture::EXPORT_ROOT,
                        &short_name,
                    ),
                    "idempotency_key": format!("alias-short-name-{}", unique_suffix()),
                }),
                ExpectedToolErrorKind::Conflict,
                &[],
            )
            .await?;
            execution.record_executed(AdversarialCaseId::Alias04)?;
        } else {
            execution.record_unsupported(AdversarialCaseId::Alias04)?;
        }
        if run.policy.read_export(&long_name)? != original {
            return Err("ALIAS-04 replaced the long-name file".to_owned());
        }

        for name in [
            "CON",
            "NUL",
            "COM1",
            "LPT1",
            "NUL.txt",
            "COM¹",
            "COM².bin",
            "COM³",
            "LPT¹",
            "LPT².txt",
            "LPT³",
        ] {
            adversarial_refusal(
                driver,
                "file_import",
                file_import_arguments(
                    space,
                    local_source(ArtifactPolicyFixture::IMPORT_ROOT, name),
                    "adversarial.bin",
                    None,
                ),
                ExpectedToolErrorKind::Validation,
                &[],
            )
            .await?;
        }
        execution.record_executed(AdversarialCaseId::Alias05)?;
    }
    #[cfg(not(windows))]
    for id in [
        AdversarialCaseId::Alias03,
        AdversarialCaseId::Alias04,
        AdversarialCaseId::Alias05,
    ] {
        execution.record_unsupported(id)?;
    }

    adversarial_refusal(
        driver,
        "file_import",
        file_import_arguments(
            space,
            local_source("Inbox", ArtifactPolicyFixture::FILE_SOURCE),
            "adversarial.bin",
            None,
        ),
        ExpectedToolErrorKind::NotFound,
        &[],
    )
    .await?;
    execution.record_executed(AdversarialCaseId::Alias06)?;

    let real_name = format!("alias-latin-a-{case_suffix}.bin");
    let homoglyph = real_name.replacen('a', "\u{430}", 1);
    run.policy.seed_import(&real_name, b"homoglyph-source")?;
    let before = RootInventory::capture(run.policy.import_root())?;
    let successful_opens_before = run.successful_import_opens.map(|counter| counter());
    adversarial_refusal(
        driver,
        "file_import",
        file_import_arguments(
            space,
            local_source(ArtifactPolicyFixture::IMPORT_ROOT, &homoglyph),
            "adversarial.bin",
            None,
        ),
        ExpectedToolErrorKind::NotFound,
        &[],
    )
    .await?;
    before.assert_unchanged()?;
    if let Some(before) = successful_opens_before {
        let after = run
            .successful_import_opens
            .map(|counter| counter())
            .ok_or_else(|| "adversarial successful-open counter disappeared".to_owned())?;
        if before != after {
            return Err("ALIAS-08 opened the real source file".to_owned());
        }
    }
    execution.record_executed(AdversarialCaseId::Alias08)?;

    let allocation = allocate_stage_upload(
        driver,
        space,
        ARTIFACT_FILE_PAYLOAD.len() as u64,
        ARTIFACT_FILE_MEDIA_TYPE,
        Some(&artifact_sha256(ARTIFACT_FILE_PAYLOAD)),
    )
    .await?;
    execution.record_forbidden_log_needle(allocation.handle().as_bytes())?;
    upload_stage_bytes(&allocation, ARTIFACT_FILE_PAYLOAD, ARTIFACT_FILE_MEDIA_TYPE).await?;
    let before_status = stage_head_status(&allocation).await?;
    let uppercase_url = allocation.url().replace(
        allocation.record(),
        &allocation.record().to_ascii_uppercase(),
    );
    let outcome = RawStagingClient::new()?
        .send(
            Method::GET,
            &uppercase_url,
            allocation.handle(),
            &[],
            Vec::new(),
        )
        .await?;
    ExpectedOutcome::Http {
        status: 404,
        body: b"not found\n",
    }
    .assert_matches(ObservedOutcome::Http {
        status: outcome.status,
        body: &outcome.body,
    })?;
    if stage_head_status(&allocation).await? != before_status {
        return Err("ALIAS-09 changed the staged record".to_owned());
    }
    release_stage_upload(driver, &allocation).await?;
    execution.record_executed(AdversarialCaseId::Alias09)?;
    finish_adversarial_quota(driver, quota_before, &mut execution).await?;
    Ok(execution)
}

/// Executes the default-policy hostile filename, MIME, and metadata cases.
/// MAL-10/MAL-12 use the reduced payload-ceiling policy and MAL-13 uses an
/// active validator policy, so those cases have dedicated helpers below.
///
/// # Errors
///
/// Returns a fixed category if any exact result, object inventory, staging
/// state, readback identity, or caller-selected export destination diverges.
pub async fn run_artifact_malicious_metadata_default(
    driver: &mut impl McpDriver,
    run: &ArtifactAdversarialRun<'_>,
) -> Result<AdversarialExecution, String> {
    let quota_before = adversarial_quota_snapshot(driver).await?;
    let mut execution = AdversarialExecution::default();
    let space = run.ctx.space_id.as_str();
    let before_names = artifact_object_ids(run.ctx).await?;
    let hostile_sources = ['\n', '\r', '\t', '\u{1b}']
        .into_iter()
        .map(|control| format!("mal01-{control}-{}.bin", unique_suffix()))
        .collect::<Vec<_>>();
    for source_name in &hostile_sources {
        seed_control_import_source(run.policy, source_name)?;
        execution.record_forbidden_log_needle(source_name.as_bytes())?;
        adversarial_refusal(
            driver,
            "file_import",
            file_import_arguments(
                space,
                local_source(ArtifactPolicyFixture::IMPORT_ROOT, source_name),
                "adversarial.bin",
                None,
            ),
            ExpectedToolErrorKind::Validation,
            &[source_name],
        )
        .await?;
    }
    if artifact_object_ids(run.ctx).await? != before_names {
        return Err("MAL-01 created an Anytype object".to_owned());
    }
    execution.record_executed(AdversarialCaseId::Mal01)?;

    let bidi_name = format!("adversarial-\u{202e}-join\u{200d}-{}", unique_suffix());
    execution.record_forbidden_log_needle(bidi_name.as_bytes())?;
    let imported = driver
        .call_tool(
            "file_import",
            file_import_arguments(
                space,
                local_source(
                    ArtifactPolicyFixture::IMPORT_ROOT,
                    ArtifactPolicyFixture::FILE_SOURCE,
                ),
                &bidi_name,
                Some(ARTIFACT_FILE_MEDIA_TYPE),
            ),
        )
        .await?;
    let bidi_file_id = required_str(&imported, "/file_id")?;
    run.ctx.register_file(&bidi_file_id);
    let fetched = run
        .ctx
        .client
        .files()
        .get(space, &bidi_file_id)
        .get()
        .await
        .map_err(|_| "read back adversarial file name".to_owned())?;
    if fetched.name.as_deref() != Some(bidi_name.as_str()) {
        return Err("MAL-02 name did not round-trip exactly".to_owned());
    }
    execution.record_executed(AdversarialCaseId::Mal02)?;

    let accepted_name = "n".repeat(255);
    let accepted = driver
        .call_tool(
            "file_import",
            file_import_arguments(
                space,
                local_source(
                    ArtifactPolicyFixture::IMPORT_ROOT,
                    ArtifactPolicyFixture::FILE_SOURCE,
                ),
                &accepted_name,
                None,
            ),
        )
        .await?;
    run.ctx.register_file(&required_str(&accepted, "/file_id")?);
    let before_invalid_names = artifact_object_ids(run.ctx).await?;
    for name in [String::new(), "n".repeat(256), "invalid\nname".to_owned()] {
        adversarial_refusal(
            driver,
            "file_import",
            file_import_arguments(
                space,
                local_source(
                    ArtifactPolicyFixture::IMPORT_ROOT,
                    ArtifactPolicyFixture::FILE_SOURCE,
                ),
                &name,
                None,
            ),
            ExpectedToolErrorKind::Validation,
            &[&name],
        )
        .await?;
    }
    if artifact_object_ids(run.ctx).await? != before_invalid_names {
        return Err("MAL-03 created an object for an invalid name".to_owned());
    }
    execution.record_executed(AdversarialCaseId::Mal03)?;

    let markdown = b"# staged MIME\n";
    let allocation = allocate_stage_upload(
        driver,
        space,
        markdown.len() as u64,
        ARTIFACT_MARKDOWN_MEDIA_TYPE,
        Some(&artifact_sha256(markdown)),
    )
    .await?;
    execution.record_forbidden_log_needle(allocation.handle().as_bytes())?;
    upload_stage_bytes(&allocation, markdown, ARTIFACT_MARKDOWN_MEDIA_TYPE).await?;
    let before_stage = stage_head_status(&allocation).await?;
    let before_objects = artifact_object_ids(run.ctx).await?;
    adversarial_refusal(
        driver,
        "file_import",
        file_import_arguments(
            space,
            json!({"staged_handle": allocation.handle()}),
            "staged-mime.bin",
            Some(ARTIFACT_FILE_MEDIA_TYPE),
        ),
        ExpectedToolErrorKind::Conflict,
        &[],
    )
    .await?;
    if stage_head_status(&allocation).await? != before_stage
        || artifact_object_ids(run.ctx).await? != before_objects
    {
        return Err("MAL-04 consumed its stage or created an object".to_owned());
    }
    release_stage_upload(driver, &allocation).await?;
    execution.record_executed(AdversarialCaseId::Mal04)?;

    let staging_before = run.policy.staging_snapshot()?;
    adversarial_refusal(
        driver,
        "artifact_stage_upload",
        json!({
            "space": space,
            "size_bytes": 4,
            "media_type": "text/plain; charset=utf-8",
        }),
        ExpectedToolErrorKind::Validation,
        &[],
    )
    .await?;
    if run.policy.staging_snapshot()? != staging_before {
        return Err("MAL-05 allocated a staging record".to_owned());
    }
    execution.record_executed(AdversarialCaseId::Mal05)?;

    for media_type in [
        "text /plain".to_owned(),
        "text/\nplain".to_owned(),
        "x".repeat(256),
    ] {
        adversarial_refusal(
            driver,
            "artifact_stage_upload",
            json!({
                "space": space,
                "size_bytes": 4,
                "media_type": media_type,
            }),
            ExpectedToolErrorKind::Validation,
            &[],
        )
        .await?;
    }
    if run.policy.staging_snapshot()? != staging_before {
        return Err("MAL-06 allocated a staging record".to_owned());
    }
    execution.record_executed(AdversarialCaseId::Mal06)?;

    let executable_name = format!("hostile-executable-{}.bin", unique_suffix());
    let executable_bytes = b"\x7fELF\x02\x01\x01adversarial-script-text";
    run.policy.seed_import(&executable_name, executable_bytes)?;
    let executable = driver
        .call_tool(
            "file_import",
            file_import_arguments(
                space,
                local_source(ArtifactPolicyFixture::IMPORT_ROOT, &executable_name),
                "declared-markdown.bin",
                Some(ARTIFACT_MARKDOWN_MEDIA_TYPE),
            ),
        )
        .await?;
    let executable_id = required_str(&executable, "/file_id")?;
    run.ctx.register_file(&executable_id);
    if executable.pointer("/receipt/declared_media_type")
        != Some(&Value::String(ARTIFACT_MARKDOWN_MEDIA_TYPE.to_owned()))
        || executable.pointer("/receipt/stored_media_type").is_some()
    {
        return Err("MAL-07 conflated declared and stored MIME evidence".to_owned());
    }
    execution.record_executed(AdversarialCaseId::Mal07)?;

    let invalid_utf8_name = format!("invalid-utf8-{}.md", unique_suffix());
    run.policy
        .seed_import(&invalid_utf8_name, b"\xf0\x28\x8c\x28")?;
    let before_document = artifact_object_ids(run.ctx).await?;
    adversarial_refusal(
        driver,
        "document_import_create",
        json!({
            "space": space,
            "source": local_source(ArtifactPolicyFixture::IMPORT_ROOT, &invalid_utf8_name),
            "source_format": "markdown",
            "object_type": "page",
            "name": "Invalid UTF-8 document",
            "idempotency_key": format!("invalid-utf8-{}", unique_suffix()),
        }),
        ExpectedToolErrorKind::Validation,
        &[],
    )
    .await?;
    if artifact_object_ids(run.ctx).await? != before_document {
        return Err("MAL-08 created an Anytype object".to_owned());
    }
    #[cfg(windows)]
    {
        let surrogate_content_name = format!("invalid-utf16-content-{}.md", unique_suffix());
        run.policy
            .seed_import(&surrogate_content_name, b"\xff\xfe\x00\xd8")?;
        adversarial_refusal(
            driver,
            "document_import_create",
            json!({
                "space": space,
                "source": local_source(ArtifactPolicyFixture::IMPORT_ROOT, &surrogate_content_name),
                "source_format": "markdown",
                "object_type": "page",
                "name": "Invalid UTF-16 document content",
                "idempotency_key": format!("invalid-utf16-content-{}", unique_suffix()),
            }),
            ExpectedToolErrorKind::Validation,
            &[],
        )
        .await?;
        let native_surrogate = native_wtf16(&[0xd800]);
        let native_surrogate_value = native_submission_value(&native_surrogate)?.to_owned();
        let native_surrogate_lossy = String::from_utf16_lossy(&[0xd800]);
        adversarial_refusal(
            driver,
            "document_import_create",
            json!({
                "space": space,
                "source": {"local": {
                    "root": ArtifactPolicyFixture::IMPORT_ROOT,
                    "path_native": native_surrogate,
                }},
                "source_format": "markdown",
                "object_type": "page",
                "name": "Invalid native document path",
                "idempotency_key": format!("invalid-native-utf8-{}", unique_suffix()),
            }),
            ExpectedToolErrorKind::Validation,
            &[&native_surrogate_value, &native_surrogate_lossy],
        )
        .await?;
        if artifact_object_ids(run.ctx).await? != before_document {
            return Err("MAL-08 native path created an Anytype object".to_owned());
        }
    }
    execution.record_executed(AdversarialCaseId::Mal08)?;

    let bom_name = format!("bom-{}.md", unique_suffix());
    run.policy.seed_import(&bom_name, b"\xef\xbb\xbf# BOM\n")?;
    adversarial_refusal(
        driver,
        "document_import_create",
        json!({
            "space": space,
            "source": local_source(ArtifactPolicyFixture::IMPORT_ROOT, &bom_name),
            "source_format": "markdown",
            "object_type": "page",
            "name": "BOM document",
            "idempotency_key": format!("bom-document-{}", unique_suffix()),
        }),
        ExpectedToolErrorKind::Validation,
        &[],
    )
    .await?;
    if artifact_object_ids(run.ctx).await? != before_document {
        return Err("MAL-09 created an Anytype object".to_owned());
    }
    execution.record_executed(AdversarialCaseId::Mal09)?;

    let boundary_name = "b".repeat(255);
    run.policy.seed_import(&boundary_name, b"boundary")?;
    let boundary = driver
        .call_tool(
            "file_import",
            file_import_arguments(
                space,
                local_source(ArtifactPolicyFixture::IMPORT_ROOT, &boundary_name),
                "boundary.bin",
                None,
            ),
        )
        .await?;
    run.ctx.register_file(&required_str(&boundary, "/file_id")?);
    let over_boundary = "b".repeat(256);
    adversarial_refusal(
        driver,
        "file_import",
        file_import_arguments(
            space,
            local_source(ArtifactPolicyFixture::IMPORT_ROOT, &over_boundary),
            "boundary.bin",
            None,
        ),
        ExpectedToolErrorKind::Validation,
        &[],
    )
    .await?;
    execution.record_executed(AdversarialCaseId::Mal11)?;

    let hostile_upstream_name = "../evil";
    let uploaded = run
        .ctx
        .client
        .files()
        .upload(space)
        .bytes(hostile_upstream_name, ARTIFACT_FILE_PAYLOAD.to_vec())
        .mime(ARTIFACT_FILE_MEDIA_TYPE)
        .upload()
        .await
        .map_err(|_| "seed hostile upstream file name".to_owned())?;
    run.ctx.register_file(&uploaded.id);
    if uploaded.name.as_deref() != Some(hostile_upstream_name) {
        return Err("MAL-14 upstream did not retain the hostile name".to_owned());
    }
    let escape = run.policy.base.join("evil");
    fs::write(&escape, b"escape-sentinel")
        .map_err(|_| "seed reverse traversal sentinel".to_owned())?;
    secure_files(std::slice::from_ref(&escape))?;
    let destination = format!("safe-export-{}.bin", unique_suffix());
    driver
        .call_tool(
            "file_export",
            json!({
                "space": space,
                "file_id": uploaded.id,
                "destination": local_destination(
                    ArtifactPolicyFixture::EXPORT_ROOT,
                    &destination,
                ),
                "idempotency_key": format!("reverse-traversal-{}", unique_suffix()),
            }),
        )
        .await?;
    if run.policy.read_export(&destination)? != ARTIFACT_FILE_PAYLOAD
        || fs::read(&escape).ok().as_deref() != Some(b"escape-sentinel")
    {
        return Err("MAL-14 wrote outside the caller destination".to_owned());
    }
    execution.record_executed(AdversarialCaseId::Mal14)?;
    finish_adversarial_quota(driver, quota_before, &mut execution).await?;
    Ok(execution)
}

/// Executes the adversarial cases supported by the default strict policy.
/// Distinct runtime-policy cases are merged by the direct and spawned owners.
pub async fn run_artifact_adversarial_default(
    driver: &mut impl McpDriver,
    run: &ArtifactAdversarialRun<'_>,
) -> Result<AdversarialExecution, String> {
    let mut execution = run_artifact_traversal_default(driver, run).await?;
    execution.merge(run_artifact_alias_cases(driver, run).await?)?;
    execution.merge(run_artifact_malicious_metadata_default(driver, run).await?)?;
    Ok(execution)
}

/// Executes the four canonical adversarial cases through one stdio child.
///
/// # Errors
///
/// Returns a fixed category when a sentinel outcome, no-mutation assertion, or
/// exact four-case ownership partition diverges.
pub async fn run_artifact_adversarial_stdio_sentinels(
    driver: &mut impl McpDriver,
    run: &ArtifactAdversarialRun<'_>,
) -> Result<AdversarialExecution, String> {
    let quota_before = adversarial_quota_snapshot(driver).await?;
    let mut execution = AdversarialExecution::default();
    let space = run.ctx.space_id.as_str();
    adversarial_refusal(
        driver,
        "file_import",
        file_import_arguments(
            space,
            local_source(ArtifactPolicyFixture::IMPORT_ROOT, "../escape.bin"),
            "adversarial.bin",
            None,
        ),
        ExpectedToolErrorKind::Validation,
        &["../escape.bin"],
    )
    .await?;
    execution.record_executed(AdversarialCaseId::Trav01)?;
    adversarial_refusal(
        driver,
        "file_import",
        file_import_arguments(
            space,
            local_source("nope", "../escape.bin"),
            "adversarial.bin",
            None,
        ),
        ExpectedToolErrorKind::Validation,
        &["../escape.bin"],
    )
    .await?;

    adversarial_refusal(
        driver,
        "file_import",
        file_import_arguments(
            space,
            local_source("Inbox", ArtifactPolicyFixture::FILE_SOURCE),
            "adversarial.bin",
            None,
        ),
        ExpectedToolErrorKind::NotFound,
        &[],
    )
    .await?;
    execution.record_executed(AdversarialCaseId::Alias06)?;

    let source_name = format!("mal01-\n-{}.bin", unique_suffix());
    seed_control_import_source(run.policy, &source_name)?;
    execution.record_forbidden_log_needle(source_name.as_bytes())?;
    let objects_before = artifact_object_ids(run.ctx).await?;
    adversarial_refusal(
        driver,
        "file_import",
        file_import_arguments(
            space,
            local_source(ArtifactPolicyFixture::IMPORT_ROOT, &source_name),
            "adversarial.bin",
            None,
        ),
        ExpectedToolErrorKind::Validation,
        &[&source_name],
    )
    .await?;
    if artifact_object_ids(run.ctx).await? != objects_before {
        return Err("MAL-01 created an Anytype object".to_owned());
    }
    execution.record_executed(AdversarialCaseId::Mal01)?;

    let bidi_name = format!("adversarial-\u{202e}-join\u{200d}-{}", unique_suffix());
    execution.record_forbidden_log_needle(bidi_name.as_bytes())?;
    let imported = driver
        .call_tool(
            "file_import",
            file_import_arguments(
                space,
                local_source(
                    ArtifactPolicyFixture::IMPORT_ROOT,
                    ArtifactPolicyFixture::FILE_SOURCE,
                ),
                &bidi_name,
                Some(ARTIFACT_FILE_MEDIA_TYPE),
            ),
        )
        .await?;
    let bidi_file_id = required_str(&imported, "/file_id")?;
    run.ctx.register_file(&bidi_file_id);
    let fetched = run
        .ctx
        .client
        .files()
        .get(space, &bidi_file_id)
        .get()
        .await
        .map_err(|_| "read back stdio adversarial file name".to_owned())?;
    if fetched.name.as_deref() != Some(bidi_name.as_str()) {
        return Err("MAL-02 stdio name did not round-trip exactly".to_owned());
    }
    execution.record_executed(AdversarialCaseId::Mal02)?;
    finish_adversarial_quota(driver, quota_before, &mut execution).await?;
    execution.assert_exact(ADVERSARIAL_STDIO_SENTINEL_IDS)?;
    Ok(execution)
}

/// Executes MAL-10 and MAL-12 under the reduced one-MiB payload profile.
///
/// # Errors
///
/// Returns a fixed category when the exact byte boundary, Markdown bound, or
/// disposable resource ownership diverges.
pub async fn run_artifact_payload_boundary_cases(
    driver: &mut impl McpDriver,
    run: &ArtifactAdversarialRun<'_>,
) -> Result<AdversarialExecution, String> {
    let quota_before = adversarial_quota_snapshot(driver).await?;
    if run.policy.options().limits != ArtifactLimitProfile::PayloadCeiling {
        return Err("payload adversarial cases require the reviewed limit profile".to_owned());
    }
    let mut execution = AdversarialExecution::default();
    let space = run.ctx.space_id.as_str();
    let limit = usize::try_from(run.policy.options().limits.artifact_bytes())
        .map_err(|_| "payload limit exceeds the addressable range".to_owned())?;

    let over_markdown_name = format!("over-markdown-{}.md", unique_suffix());
    run.policy
        .seed_import(&over_markdown_name, &vec![b'x'; limit.saturating_add(1)])?;
    let over_markdown = tokio::time::timeout(
        Duration::from_secs(15),
        adversarial_refusal(
            driver,
            "document_import_create",
            json!({
                "space": space,
                "source": local_source(
                    ArtifactPolicyFixture::IMPORT_ROOT,
                    &over_markdown_name,
                ),
                "source_format": "markdown",
                "object_type": "page",
                "name": "Bounded Markdown document",
                "idempotency_key": format!("bounded-markdown-{}", unique_suffix()),
            }),
            ExpectedToolErrorKind::BoundedResult,
            &[],
        ),
    )
    .await
    .map_err(|_| "MAL-10 exceeded the fixed completion deadline")??;
    assert_adversarial_response_frame(over_markdown.normalized_result())?;
    let deep_name = format!("deep-markdown-{}.md", unique_suffix());
    let deep_markdown = format!("{}leaf\n", "> ".repeat(128));
    run.policy
        .seed_import(&deep_name, deep_markdown.as_bytes())?;
    let deep_result = tokio::time::timeout(
        Duration::from_secs(15),
        driver.call_tool_full_result(
            "document_import_create",
            json!({
                "space": space,
                "source": local_source(ArtifactPolicyFixture::IMPORT_ROOT, &deep_name),
                "source_format": "markdown",
                "object_type": "page",
                "name": "Deep bounded Markdown document",
                "idempotency_key": format!("deep-markdown-{}", unique_suffix()),
            }),
        ),
    )
    .await
    .map_err(|_| "MAL-10 exceeded the fixed completion deadline")??;
    assert_adversarial_response_frame(&deep_result)?;
    let deep = deep_result
        .get("structuredContent")
        .ok_or_else(|| "MAL-10 complete result omitted structured content".to_owned())?;
    run.ctx.register_object(&required_str(deep, "/object_id")?);
    execution.record_executed(AdversarialCaseId::Mal10)?;

    let exact_name = format!("exact-payload-{}.bin", unique_suffix());
    run.policy.seed_import(&exact_name, &vec![0x5a; limit])?;
    let exact = driver
        .call_tool(
            "file_import",
            file_import_arguments(
                space,
                local_source(ArtifactPolicyFixture::IMPORT_ROOT, &exact_name),
                "exact-payload.bin",
                Some(ARTIFACT_FILE_MEDIA_TYPE),
            ),
        )
        .await?;
    run.ctx.register_file(&required_str(&exact, "/file_id")?);
    if exact.pointer("/receipt/size_bytes").and_then(Value::as_u64) != Some(limit as u64) {
        return Err("MAL-12 exact payload boundary changed".to_owned());
    }
    let over_name = format!("over-payload-{}.bin", unique_suffix());
    run.policy
        .seed_import(&over_name, &vec![0x5a; limit.saturating_add(1)])?;
    adversarial_refusal(
        driver,
        "file_import",
        file_import_arguments(
            space,
            local_source(ArtifactPolicyFixture::IMPORT_ROOT, &over_name),
            "over-payload.bin",
            Some(ARTIFACT_FILE_MEDIA_TYPE),
        ),
        ExpectedToolErrorKind::BoundedResult,
        &[],
    )
    .await?;
    execution.record_executed(AdversarialCaseId::Mal12)?;
    finish_adversarial_quota(driver, quota_before, &mut execution).await?;
    Ok(execution)
}

/// Executes MAL-13 with the reviewed optional, hash-pinned validator policy.
/// Non-activating platforms record the matrix-approved unsupported status.
pub async fn run_artifact_hostile_validator_case(
    driver: &mut impl McpDriver,
    run: &ArtifactAdversarialRun<'_>,
) -> Result<AdversarialExecution, String> {
    let quota_before = adversarial_quota_snapshot(driver).await?;
    let mut execution = AdversarialExecution::default();
    if !VALIDATOR_PLATFORM_ACTIVATES {
        execution.record_unsupported(AdversarialCaseId::Mal13)?;
        finish_adversarial_quota(driver, quota_before, &mut execution).await?;
        return Ok(execution);
    }
    if run.policy.options().validators != FixtureValidatorPolicy::Optional {
        return Err("hostile metadata case requires the reviewed validator policy".to_owned());
    }
    let name = format!("hostile-image-{}.png", unique_suffix());
    let bytes = hostile_png_metadata_fixture();
    run.policy.seed_import(&name, &bytes)?;
    let imported = driver
        .call_tool(
            "file_import",
            file_import_arguments(
                &run.ctx.space_id,
                local_source(ArtifactPolicyFixture::IMPORT_ROOT, &name),
                "hostile-image.png",
                Some("image/png"),
            ),
        )
        .await?;
    run.ctx.register_file(&required_str(&imported, "/file_id")?);
    let validators = imported
        .get("validators")
        .and_then(Value::as_array)
        .ok_or_else(|| "MAL-13 omitted validator evidence".to_owned())?;
    if validators.as_slice()
        != [json!({
            "id": FIXTURE_VALIDATOR_ID,
            "status": "accepted",
            "detected_media_type": "image/png",
        })]
    {
        return Err("MAL-13 validator evidence was not exact".to_owned());
    }
    let encoded = serde_json::to_vec(&imported)
        .map_err(|_| "encode hostile validator evidence".to_owned())?;
    if encoded.len() > ARTIFACT_FRAME_CEILING_BYTES as usize
        || encoded
            .windows(b"<script>".len())
            .any(|window| window == b"<script>")
    {
        return Err("MAL-13 exposed unbounded hostile metadata".to_owned());
    }
    execution.record_executed(AdversarialCaseId::Mal13)?;
    finish_adversarial_quota(driver, quota_before, &mut execution).await?;
    Ok(execution)
}

fn hostile_png_metadata_fixture() -> Vec<u8> {
    let mut image = b"\x89PNG\r\n\x1a\n".to_vec();
    png_chunk(
        &mut image,
        *b"IHDR",
        &[0, 0, 0, 1, 0, 0, 0, 1, 8, 6, 0, 0, 0],
    );
    let mut comment = b"ASCII\0\0\0".to_vec();
    comment.extend(std::iter::repeat_n(b'X', 60 * 1024));
    comment.extend_from_slice(b"<script>adversarial</script>");
    let mut exif = b"MM\0*\0\0\0\x08\0\x01\x92\x86\0\x07".to_vec();
    exif.extend_from_slice(
        &u32::try_from(comment.len())
            .unwrap_or(u32::MAX)
            .to_be_bytes(),
    );
    exif.extend_from_slice(&26_u32.to_be_bytes());
    exif.extend_from_slice(&0_u32.to_be_bytes());
    exif.extend_from_slice(&comment);
    png_chunk(&mut image, *b"eXIf", &exif);
    png_chunk(
        &mut image,
        *b"IDAT",
        &[
            0x78, 0x01, 0x01, 0x05, 0x00, 0xfa, 0xff, 0, 1, 2, 3, 0xff, 1, 0x10, 1, 5,
        ],
    );
    png_chunk(&mut image, *b"IEND", &[]);
    image
}

fn png_chunk(output: &mut Vec<u8>, kind: [u8; 4], bytes: &[u8]) {
    let length = u32::try_from(bytes.len()).unwrap_or(u32::MAX);
    output.extend_from_slice(&length.to_be_bytes());
    output.extend_from_slice(&kind);
    output.extend_from_slice(bytes);
    let mut crc_input = Vec::with_capacity(kind.len().saturating_add(bytes.len()));
    crc_input.extend_from_slice(&kind);
    crc_input.extend_from_slice(bytes);
    output.extend_from_slice(&png_crc32(&crc_input).to_be_bytes());
}

fn png_crc32(bytes: &[u8]) -> u32 {
    let mut crc = u32::MAX;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = if crc & 1 == 0 {
                crc >> 1
            } else {
                (crc >> 1) ^ 0xedb8_8320
            };
        }
    }
    !crc
}

/// Content-free exact inventory of a fixture root.
///
/// Inventory entries retain only relative names, file kinds, byte counts, and
/// SHA-256 digests. The custom debug representation never exposes those names.
pub struct RootInventory {
    #[cfg(any(unix, windows))]
    root: cap_std::fs::Dir,
    #[cfg(any(unix, windows))]
    device: u64,
    #[cfg(any(unix, windows))]
    inode: u64,
    entries: BTreeMap<PathBuf, RootInventoryEntry>,
}

#[derive(Clone, PartialEq, Eq)]
enum RootInventoryEntry {
    Directory,
    File { size_bytes: u64, sha256: String },
}

impl fmt::Debug for RootInventory {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RootInventory")
            .field("entry_count", &self.entries.len())
            .finish()
    }
}

impl RootInventory {
    /// Captures an exact, no-follow inventory of one private fixture root.
    ///
    /// # Errors
    ///
    /// Returns a fixed category when the root cannot be read, contains a
    /// symlink or special file, or changes while being captured.
    #[cfg(unix)]
    pub fn capture(root: &Path) -> Result<Self, String> {
        use std::os::unix::{fs::MetadataExt, fs::OpenOptionsExt};

        let mut options = OpenOptions::new();
        options
            .read(true)
            .custom_flags(libc::O_CLOEXEC | libc::O_DIRECTORY | libc::O_NOFOLLOW);
        let root_file = options
            .open(root)
            .map_err(|_| "capture artifact root inventory".to_owned())?;
        let metadata = root_file
            .metadata()
            .map_err(|_| "capture artifact root inventory".to_owned())?;
        if !metadata.file_type().is_dir() {
            return Err("capture artifact root inventory".to_owned());
        }
        let root = cap_std::fs::Dir::from_std_file(root_file);
        let mut entries = BTreeMap::new();
        root_inventory_visit(&root, &mut entries)?;
        Ok(Self {
            root,
            device: metadata.dev(),
            inode: metadata.ino(),
            entries,
        })
    }

    /// Captures a no-follow directory handle on Windows before walking it.
    #[cfg(windows)]
    pub fn capture(root: &Path) -> Result<Self, String> {
        use std::os::windows::fs::{MetadataExt, OpenOptionsExt};

        const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
        let mut options = OpenOptions::new();
        options
            .read(true)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT);
        let root_file = options
            .open(root)
            .map_err(|_| "capture artifact root inventory".to_owned())?;
        let metadata = root_file
            .metadata()
            .map_err(|_| "capture artifact root inventory".to_owned())?;
        if !metadata.is_dir() || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err("capture artifact root inventory".to_owned());
        }
        let root = cap_std::fs::Dir::from_std_file(root_file);
        let mut entries = BTreeMap::new();
        root_inventory_visit(&root, &mut entries)?;
        Ok(Self {
            root,
            device: metadata.volume_serial_number().unwrap_or_default().into(),
            inode: metadata.file_index().unwrap_or_default(),
            entries,
        })
    }

    /// Refuses root inventory capture where no no-follow descriptor primitive
    /// is available in this harness target.
    #[cfg(not(any(unix, windows)))]
    pub fn capture(_root: &Path) -> Result<Self, String> {
        Err("capture artifact root inventory is unsupported on this platform".to_owned())
    }

    /// Fails unless the root exactly matches this prior content-free capture.
    ///
    /// # Errors
    ///
    /// Returns a fixed category when the root changed or cannot be captured.
    #[cfg(any(unix, windows))]
    pub fn assert_unchanged(&self) -> Result<(), String> {
        use cap_fs_ext::MetadataExt;

        let metadata = self
            .root
            .metadata(".")
            .map_err(|_| "capture artifact root inventory".to_owned())?;
        if !metadata.file_type().is_dir()
            || metadata.dev() != self.device
            || metadata.ino() != self.inode
        {
            return Err("artifact root inventory changed".to_owned());
        }
        let mut entries = BTreeMap::new();
        root_inventory_visit(&self.root, &mut entries)?;
        if self.entries == entries {
            Ok(())
        } else {
            Err("artifact root inventory changed".to_owned())
        }
    }

    #[cfg(not(any(unix, windows)))]
    pub fn assert_unchanged(&self) -> Result<(), String> {
        let _ = self;
        Err("capture artifact root inventory is unsupported on this platform".to_owned())
    }

    /// Returns the number of files and directories captured.
    #[must_use]
    pub fn entry_count(&self) -> usize {
        self.entries.len()
    }
}

#[cfg(any(unix, windows))]
fn root_inventory_visit(
    root: &cap_std::fs::Dir,
    entries: &mut BTreeMap<PathBuf, RootInventoryEntry>,
) -> Result<(), String> {
    use cap_fs_ext::{FollowSymlinks, OpenOptionsFollowExt, OpenOptionsMaybeDirExt};

    let mut directories = vec![(
        PathBuf::new(),
        root.try_clone()
            .map_err(|_| "capture artifact root inventory".to_owned())?,
    )];
    while let Some((relative, directory)) = directories.pop() {
        let iterator = directory
            .read_dir(".")
            .map_err(|_| "capture artifact root inventory".to_owned())?;
        for entry in iterator {
            let entry = entry.map_err(|_| "capture artifact root inventory".to_owned())?;
            let name = entry.file_name();
            let child_relative = relative.join(name);
            let mut options = cap_std::fs::OpenOptions::new();
            options
                .read(true)
                .follow(FollowSymlinks::No)
                .maybe_dir(true);
            let file = entry
                .open_with(&options)
                .map_err(|_| "capture artifact root inventory".to_owned())?;
            let metadata = file
                .metadata()
                .map_err(|_| "capture artifact root inventory".to_owned())?;
            let file_type = metadata.file_type();
            if file_type.is_dir() {
                if entries
                    .insert(child_relative.clone(), RootInventoryEntry::Directory)
                    .is_some()
                {
                    return Err("capture artifact root inventory".to_owned());
                }
                directories.push((
                    child_relative,
                    cap_std::fs::Dir::from_std_file(file.into_std()),
                ));
            } else if file_type.is_file() {
                let digest = root_inventory_file_digest(file.into_std())?;
                let entry = RootInventoryEntry::File {
                    size_bytes: metadata.len(),
                    sha256: digest,
                };
                if entries.insert(child_relative, entry).is_some() {
                    return Err("capture artifact root inventory".to_owned());
                }
            } else {
                return Err("capture artifact root inventory".to_owned());
            }
        }
    }
    Ok(())
}

#[cfg(any(unix, windows))]
fn root_inventory_file_digest(mut file: File) -> Result<String, String> {
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 32 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|_| "capture artifact root inventory".to_owned())?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(hex_digest(&digest.finalize()))
}

/// Observed case behavior for the volume containing an artifact root.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VolumeCaseFolding {
    /// The differently cased private name referred to the created file.
    Insensitive,
    /// The differently cased private name was distinct.
    Sensitive,
}

/// Observed Unicode-normalization behavior for an artifact root volume.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VolumeNormalization {
    /// NFC and NFD spellings referred to the created file.
    Equivalent,
    /// NFC and NFD spellings were distinct paths.
    Distinct,
}

/// Probes case folding by creating and removing exactly one private file.
///
/// # Errors
///
/// Returns a fixed category when the root cannot safely host the probe or the
/// temporary private file cannot be removed.
pub fn probe_volume_case_folding(root: &Path) -> Result<VolumeCaseFolding, String> {
    with_private_volume_probe(root, "case", |probe| {
        let alternate = probe.with_file_name(
            probe
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or_else(|| "probe artifact root capability".to_owned())?
                .to_ascii_uppercase(),
        );
        volume_alias_result(
            &alternate,
            VolumeCaseFolding::Insensitive,
            VolumeCaseFolding::Sensitive,
        )
    })
}

/// Probes Unicode normalization by creating and removing exactly one private file.
///
/// # Errors
///
/// Returns a fixed category when the root cannot safely host the probe or the
/// temporary private file cannot be removed.
pub fn probe_volume_normalization(root: &Path) -> Result<VolumeNormalization, String> {
    with_private_volume_probe(root, "normalization-\u{e9}", |probe| {
        let name = probe
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| "probe artifact root capability".to_owned())?;
        let alternate = probe.with_file_name(name.replace('\u{e9}', "e\u{301}"));
        volume_alias_result(
            &alternate,
            VolumeNormalization::Equivalent,
            VolumeNormalization::Distinct,
        )
    })
}

fn with_private_volume_probe<T>(
    root: &Path,
    kind: &str,
    probe: impl FnOnce(&Path) -> Result<T, String>,
) -> Result<T, String> {
    let metadata =
        fs::symlink_metadata(root).map_err(|_| "probe artifact root capability".to_owned())?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err("probe artifact root capability".to_owned());
    }
    let path = root.join(format!(".any-mcp-{kind}-probe-{}", unique_suffix()));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .map_err(|_| "probe artifact root capability".to_owned())?;
    let write = file
        .write_all(b"artifact-volume-probe")
        .and_then(|()| file.sync_all())
        .map_err(|_| "probe artifact root capability".to_owned());
    drop(file);
    let result = write.and_then(|()| probe(&path));
    let cleanup = fs::remove_file(&path).map_err(|_| "probe artifact root capability".to_owned());
    match (result, cleanup) {
        (Ok(value), Ok(())) => Ok(value),
        _ => Err("probe artifact root capability".to_owned()),
    }
}

fn volume_alias_result<T: Copy>(alternate: &Path, equivalent: T, distinct: T) -> Result<T, String> {
    match fs::symlink_metadata(alternate) {
        Ok(_) => Ok(equivalent),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(distinct),
        Err(_) => Err("probe artifact root capability".to_owned()),
    }
}

/// Minimal data-plane client for requests MCP arguments cannot represent.
pub struct RawStagingClient {
    client: reqwest::Client,
}

/// Bounded status/body observation from a raw staging request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawStagingOutcome {
    /// Exact HTTP status.
    pub status: u16,
    /// Complete fixed response body, capped before allocation can grow.
    pub body: Vec<u8>,
    /// Committed offset reported by a successful or status response.
    pub upload_offset: Option<u64>,
}

impl fmt::Debug for RawStagingClient {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("RawStagingClient")
    }
}

impl RawStagingClient {
    /// Creates the bounded raw staging client without making a network request.
    ///
    /// # Errors
    ///
    /// Returns a fixed category when the HTTP client cannot be constructed.
    pub fn new() -> Result<Self, String> {
        staging_client().map(|client| Self { client })
    }

    /// Sends a staging request with verbatim additional headers.
    ///
    /// Duplicate supplied headers are retained, allowing tests to exercise
    /// protocol requests the MCP tool surface cannot build.
    ///
    /// # Errors
    ///
    /// Returns a fixed category when the request cannot be sent or read.
    pub async fn send(
        &self,
        method: Method,
        url: &str,
        bearer: &str,
        headers: &[(HeaderName, HeaderValue)],
        body: Vec<u8>,
    ) -> Result<RawStagingOutcome, String> {
        let mut additional = reqwest::header::HeaderMap::new();
        for (name, value) in headers {
            additional.append(name.clone(), value.clone());
        }
        let response = self
            .client
            .request(method, url)
            .bearer_auth(bearer)
            .headers(additional)
            .body(body)
            .send()
            .await
            .map_err(|_| "send raw staging request".to_owned())?;
        let status = response.status().as_u16();
        let upload_offset = response
            .headers()
            .get("upload-offset")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<u64>().ok());
        let mut body = Vec::with_capacity(4_096);
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|_| "read raw staging response".to_owned())?;
            if chunk.len() > 4_096usize.saturating_sub(body.len()) {
                return Err("raw staging response exceeded its fixed bound".to_owned());
            }
            body.extend_from_slice(&chunk);
        }
        Ok(RawStagingOutcome {
            status,
            body,
            upload_offset,
        })
    }

    /// Starts a raw request whose sole body chunk waits at two test-owned
    /// barriers. Callers wait for both body streams to reach `arrived` before
    /// releasing `release`, which establishes a real overlapping-body race
    /// rather than merely scheduling two independent client futures.
    ///
    /// # Errors
    ///
    /// Returns a fixed category when the request cannot be sent or read.
    pub async fn send_with_body_barrier(
        &self,
        method: Method,
        url: String,
        bearer: Zeroizing<String>,
        headers: Vec<(HeaderName, HeaderValue)>,
        body: Vec<u8>,
        arrived: Arc<tokio::sync::Barrier>,
        release: Arc<tokio::sync::Barrier>,
    ) -> Result<RawStagingOutcome, String> {
        let mut additional = reqwest::header::HeaderMap::new();
        for (name, value) in headers {
            additional.append(name, value);
        }
        let body = reqwest::Body::wrap_stream(futures_util::stream::once(async move {
            arrived.wait().await;
            release.wait().await;
            Ok::<Vec<u8>, std::io::Error>(body)
        }));
        let response = self
            .client
            .request(method, url)
            .bearer_auth(bearer.as_str())
            .headers(additional)
            .body(body)
            .send()
            .await
            .map_err(|_| "send raw staged barrier request".to_owned())?;
        let status = response.status().as_u16();
        let upload_offset = response
            .headers()
            .get("upload-offset")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<u64>().ok());
        let mut response_body = Vec::with_capacity(4_096);
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|_| "read raw staging response".to_owned())?;
            if chunk.len() > 4_096usize.saturating_sub(response_body.len()) {
                return Err("raw staging response exceeded its fixed bound".to_owned());
            }
            response_body.extend_from_slice(&chunk);
        }
        Ok(RawStagingOutcome {
            status,
            body: response_body,
            upload_offset,
        })
    }
}
/// Fixed canonical MIME essence asserted for the smoke file.
pub const ARTIFACT_FILE_MEDIA_TYPE: &str = "application/octet-stream";
/// Fixed canonical MIME essence asserted for staged Markdown uploads.
pub const ARTIFACT_MARKDOWN_MEDIA_TYPE: &str = "text/markdown";

/// Control plane through which acceptance scenarios reach the artifact tools.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ArtifactControlPlane {
    /// Exact JSON-RPC frames exchanged with a spawned production child.
    ScriptedProtocol,
    /// In-process production router dispatch without any transport.
    DirectRouter,
    /// Spawned production stdio child on the stable protocol revision.
    SpawnedStableStdio,
    /// Spawned production stdio child on the preview protocol revision.
    SpawnedPreviewStdio,
}

impl ArtifactControlPlane {
    /// Complete closed control-plane inventory.
    pub const ALL: [Self; 4] = [
        Self::ScriptedProtocol,
        Self::DirectRouter,
        Self::SpawnedStableStdio,
        Self::SpawnedPreviewStdio,
    ];

    /// Stable identifier used in evidence and failure reports.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ScriptedProtocol => "scripted_protocol",
            Self::DirectRouter => "direct_router",
            Self::SpawnedStableStdio => "spawned_stable_stdio",
            Self::SpawnedPreviewStdio => "spawned_preview_stdio",
        }
    }

    /// Advertised MCP protocol revision for this control plane.
    #[must_use]
    pub const fn protocol_version(self) -> &'static str {
        match self {
            Self::SpawnedPreviewStdio => "2026-07-28",
            Self::ScriptedProtocol | Self::DirectRouter | Self::SpawnedStableStdio => "2025-11-25",
        }
    }

    /// Whether the control plane runs in a separate production process.
    #[must_use]
    pub const fn is_spawned(self) -> bool {
        matches!(
            self,
            Self::ScriptedProtocol | Self::SpawnedStableStdio | Self::SpawnedPreviewStdio
        )
    }

    /// Parses an exact stable identifier.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|candidate| candidate.as_str() == value)
    }
}

/// Byte path taken by imported and exported artifact payloads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ArtifactDataPlane {
    /// Bytes move through authorized local import/export roots.
    LocalRoots,
    /// Bytes move through the remote HTTP staging service.
    RemoteStaging,
}

impl ArtifactDataPlane {
    /// Complete closed data-plane inventory.
    pub const ALL: [Self; 2] = [Self::LocalRoots, Self::RemoteStaging];

    /// Stable identifier used in evidence and failure reports.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::LocalRoots => "local_roots",
            Self::RemoteStaging => "remote_staging",
        }
    }

    /// Parses an exact stable identifier.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|candidate| candidate.as_str() == value)
    }
}

/// One acceptance transport: a control plane paired with a data plane.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ArtifactTransport {
    control: ArtifactControlPlane,
    data: ArtifactDataPlane,
}

impl ArtifactTransport {
    /// Complete closed acceptance matrix.
    pub const ALL: [Self; 8] = [
        Self::new(
            ArtifactControlPlane::ScriptedProtocol,
            ArtifactDataPlane::LocalRoots,
        ),
        Self::new(
            ArtifactControlPlane::ScriptedProtocol,
            ArtifactDataPlane::RemoteStaging,
        ),
        Self::new(
            ArtifactControlPlane::DirectRouter,
            ArtifactDataPlane::LocalRoots,
        ),
        Self::new(
            ArtifactControlPlane::DirectRouter,
            ArtifactDataPlane::RemoteStaging,
        ),
        Self::new(
            ArtifactControlPlane::SpawnedStableStdio,
            ArtifactDataPlane::LocalRoots,
        ),
        Self::new(
            ArtifactControlPlane::SpawnedStableStdio,
            ArtifactDataPlane::RemoteStaging,
        ),
        Self::new(
            ArtifactControlPlane::SpawnedPreviewStdio,
            ArtifactDataPlane::LocalRoots,
        ),
        Self::new(
            ArtifactControlPlane::SpawnedPreviewStdio,
            ArtifactDataPlane::RemoteStaging,
        ),
    ];

    /// Transports executed by the in-crate direct-router acceptance target.
    pub const DIRECT_MATRIX: [Self; 2] = [
        Self::new(
            ArtifactControlPlane::DirectRouter,
            ArtifactDataPlane::LocalRoots,
        ),
        Self::new(
            ArtifactControlPlane::DirectRouter,
            ArtifactDataPlane::RemoteStaging,
        ),
    ];

    /// Transports executed by the spawned production-process acceptance target.
    pub const SPAWNED_MATRIX: [Self; 6] = [
        Self::new(
            ArtifactControlPlane::ScriptedProtocol,
            ArtifactDataPlane::LocalRoots,
        ),
        Self::new(
            ArtifactControlPlane::ScriptedProtocol,
            ArtifactDataPlane::RemoteStaging,
        ),
        Self::new(
            ArtifactControlPlane::SpawnedStableStdio,
            ArtifactDataPlane::LocalRoots,
        ),
        Self::new(
            ArtifactControlPlane::SpawnedStableStdio,
            ArtifactDataPlane::RemoteStaging,
        ),
        Self::new(
            ArtifactControlPlane::SpawnedPreviewStdio,
            ArtifactDataPlane::LocalRoots,
        ),
        Self::new(
            ArtifactControlPlane::SpawnedPreviewStdio,
            ArtifactDataPlane::RemoteStaging,
        ),
    ];

    /// Pairs one control plane with one data plane.
    #[must_use]
    pub const fn new(control: ArtifactControlPlane, data: ArtifactDataPlane) -> Self {
        Self { control, data }
    }

    /// Selected control plane.
    #[must_use]
    pub const fn control(self) -> ArtifactControlPlane {
        self.control
    }

    /// Selected data plane.
    #[must_use]
    pub const fn data(self) -> ArtifactDataPlane {
        self.data
    }

    /// Stable `<control>+<data>` identifier used in evidence.
    #[must_use]
    pub const fn id(self) -> &'static str {
        match (self.control, self.data) {
            (ArtifactControlPlane::ScriptedProtocol, ArtifactDataPlane::LocalRoots) => {
                "scripted_protocol+local_roots"
            }
            (ArtifactControlPlane::ScriptedProtocol, ArtifactDataPlane::RemoteStaging) => {
                "scripted_protocol+remote_staging"
            }
            (ArtifactControlPlane::DirectRouter, ArtifactDataPlane::LocalRoots) => {
                "direct_router+local_roots"
            }
            (ArtifactControlPlane::DirectRouter, ArtifactDataPlane::RemoteStaging) => {
                "direct_router+remote_staging"
            }
            (ArtifactControlPlane::SpawnedStableStdio, ArtifactDataPlane::LocalRoots) => {
                "spawned_stable_stdio+local_roots"
            }
            (ArtifactControlPlane::SpawnedStableStdio, ArtifactDataPlane::RemoteStaging) => {
                "spawned_stable_stdio+remote_staging"
            }
            (ArtifactControlPlane::SpawnedPreviewStdio, ArtifactDataPlane::LocalRoots) => {
                "spawned_preview_stdio+local_roots"
            }
            (ArtifactControlPlane::SpawnedPreviewStdio, ArtifactDataPlane::RemoteStaging) => {
                "spawned_preview_stdio+remote_staging"
            }
        }
    }

    /// Parses an exact stable `<control>+<data>` identifier.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|candidate| candidate.id() == value)
    }
}

/// Reviewed exact snapshot of the complete artifact tool catalog.
///
/// The fixture is regenerated only by the ignored updater documented beside it
/// in `tests/snapshots/README.md`; ordinary runs compare against it.
pub const REVIEWED_ARTIFACT_CATALOG: &str = include_str!("../snapshots/artifact-catalog.snap");

/// Exact catalog and schema snapshot of the advertised artifact tools.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactCatalogSnapshot {
    tools: BTreeMap<String, String>,
}

impl ArtifactCatalogSnapshot {
    /// Builds the snapshot from complete `tools/list` descriptors.
    ///
    /// Only the closed artifact inventory is retained. Each entry hashes the
    /// canonical (recursively key-sorted) descriptor, so an added field, a
    /// changed description, or a relaxed schema bound all diverge.
    ///
    /// # Errors
    ///
    /// Returns a fixed message when a descriptor is malformed or when the
    /// advertised artifact inventory is not exactly [`ARTIFACT_TOOL_NAMES`].
    pub fn from_descriptors(descriptors: &[Value]) -> Result<Self, String> {
        let mut tools = BTreeMap::new();
        for descriptor in descriptors {
            let name = descriptor
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| "tools/list descriptor omitted its name".to_owned())?;
            if !ARTIFACT_TOOL_NAMES.contains(&name) {
                continue;
            }
            if tools
                .insert(name.to_owned(), canonical_digest(descriptor))
                .is_some()
            {
                return Err(format!("duplicate artifact tool descriptor: {name}"));
            }
        }
        let advertised = tools.keys().map(String::as_str).collect::<Vec<_>>();
        if advertised != ARTIFACT_TOOL_NAMES {
            return Err("advertised artifact catalog is not the exact inventory".to_owned());
        }
        Ok(Self { tools })
    }

    /// Builds the snapshot from the reviewed committed fixture.
    ///
    /// # Errors
    ///
    /// Returns a fixed message when the fixture is malformed or no longer
    /// contains the exact artifact inventory.
    pub fn reviewed() -> Result<Self, String> {
        let value: Value = serde_json::from_str(REVIEWED_ARTIFACT_CATALOG)
            .map_err(|_| "reviewed artifact catalog fixture is malformed".to_owned())?;
        let tools = value
            .get("tools")
            .and_then(Value::as_array)
            .ok_or_else(|| "reviewed artifact catalog fixture omitted its tools".to_owned())?;
        Self::from_descriptors(tools)
    }

    /// Exact digest over the complete artifact catalog.
    #[must_use]
    pub fn digest(&self) -> String {
        let mut hasher = Sha256::new();
        for (name, digest) in &self.tools {
            hasher.update(name.as_bytes());
            hasher.update(b"\0");
            hasher.update(digest.as_bytes());
            hasher.update(b"\n");
        }
        hex_digest(&hasher.finalize())
    }

    /// Per-tool canonical descriptor digests, sorted by tool name.
    #[must_use]
    pub fn tool_digests(&self) -> &BTreeMap<String, String> {
        &self.tools
    }

    /// Compares two snapshots and names the first divergent tool.
    ///
    /// # Errors
    ///
    /// Returns a fixed message naming only the divergent tool; no schema
    /// fragment is retained in the report.
    pub fn compare(&self, other: &Self) -> Result<(), String> {
        for (name, digest) in &self.tools {
            match other.tools.get(name) {
                None => return Err(format!("artifact catalog omitted tool: {name}")),
                Some(candidate) if candidate != digest => {
                    return Err(format!("artifact tool contract diverged: {name}"));
                }
                Some(_) => {}
            }
        }
        if other.tools.len() == self.tools.len() {
            Ok(())
        } else {
            Err("artifact catalog advertised an unexpected tool".to_owned())
        }
    }
}

/// Recursively sorts object keys and preserves array order.
fn canonical_value(value: &Value) -> Value {
    match value {
        Value::Object(entries) => Value::Object(
            entries
                .iter()
                .map(|(key, nested)| (key.clone(), canonical_value(nested)))
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.iter().map(canonical_value).collect()),
        other => other.clone(),
    }
}

fn canonical_digest(value: &Value) -> String {
    let canonical = canonical_value(value);
    let encoded =
        serde_json::to_string(&canonical).unwrap_or_else(|_| String::from("<unencodable>"));
    hex_digest(&Sha256::digest(encoded.as_bytes()))
}

fn hex_digest(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut encoded, byte| {
        encoded.push_str(&format!("{byte:02x}"));
        encoded
    })
}

/// Lowercase SHA-256 of exact bytes.
#[must_use]
pub fn artifact_sha256(bytes: &[u8]) -> String {
    hex_digest(&Sha256::digest(bytes))
}

/// Operator space policy declared by an acceptance fixture.
///
/// The three configured shapes are distinguished by the `allowed` key alone:
/// omitting it admits every otherwise-authorized space, an explicit empty list
/// admits none, and an explicit list admits exactly its entries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FixtureSpacePolicy {
    /// `allowed` is omitted, so the space under test is admitted.
    Omitted,
    /// `allowed = []`, so no space is admitted.
    Empty,
    /// `allowed` names exactly the disposable space under test.
    AllowedUnderTest,
    /// `allowed` names exactly one other space, denying the space under test.
    RestrictedElsewhere,
}

impl FixtureSpacePolicy {
    /// Complete closed inventory of configured space policies.
    pub const ALL: [Self; 4] = [
        Self::Omitted,
        Self::Empty,
        Self::AllowedUnderTest,
        Self::RestrictedElsewhere,
    ];

    /// Stable identifier used in evidence and failure reports.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Omitted => "omitted",
            Self::Empty => "empty",
            Self::AllowedUnderTest => "allowed_under_test",
            Self::RestrictedElsewhere => "restricted_elsewhere",
        }
    }

    /// Whether the disposable space under test is admitted by this policy.
    #[must_use]
    pub const fn admits_space_under_test(self) -> bool {
        matches!(self, Self::Omitted | Self::AllowedUnderTest)
    }
}

/// Syntactically valid identifier of a space that exists in no test account.
///
/// The restricted fixture authorizes only this identifier, so the disposable
/// space under test is denied by policy rather than by upstream visibility.
pub const UNAUTHORIZED_SPACE_ID: &str = "any-mcp-acceptance-unauthorized-space";

/// Configured artifact validator declaration shape.
///
/// A declared validator is always the same real `file(1)`-compatible
/// executable pinned by hash; only its `required` flag changes, because that
/// flag is the single production switch between a reported finding and a
/// refused artifact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FixtureValidatorPolicy {
    /// No `[[validators]]` table is declared.
    Absent,
    /// One optional validator: a rejection is reported, never fatal.
    Optional,
    /// One required validator: a rejection refuses the artifact.
    Required,
}

impl FixtureValidatorPolicy {
    /// Complete closed inventory of configured validator shapes.
    pub const ALL: [Self; 3] = [Self::Absent, Self::Optional, Self::Required];

    /// Stable identifier used in evidence and failure reports.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Absent => "absent",
            Self::Optional => "optional",
            Self::Required => "required",
        }
    }

    /// Whether the rendered policy declares a validator at all.
    #[must_use]
    pub const fn is_declared(self) -> bool {
        !matches!(self, Self::Absent)
    }

    /// Whether a declared validator refuses the artifact on rejection.
    #[must_use]
    pub const fn is_required(self) -> bool {
        matches!(self, Self::Required)
    }
}

/// Logical identifier of the single validator declared by the fixture policy.
pub const FIXTURE_VALIDATOR_ID: &str = "mime";

/// Exact MIME scope admitted to the fixture validator.
///
/// The scope is deliberately narrow so an out-of-scope declaration proves that
/// a configured validator does not run on every artifact.
pub const FIXTURE_VALIDATOR_MIME: [&str; 2] = ["text/plain", "image/png"];

/// Whether this platform activates configured validator executables.
///
/// Retained-descriptor execution is reviewed for Linux only; every other
/// platform admits the configuration but reports the validator unavailable.
pub const VALIDATOR_PLATFORM_ACTIVATES: bool = cfg!(target_os = "linux");

/// One real, hash-pinned executable declared as the fixture validator.
///
/// The fixture never ships or synthesizes an executable: it pins whatever
/// `file(1)`-compatible binary the host already provides, so validator
/// evidence comes from a real MIME detector rather than a semantic mock.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinnedValidatorExecutable {
    path: PathBuf,
    sha256: String,
}

impl PinnedValidatorExecutable {
    /// Environment variable naming an exact `file(1)`-compatible executable.
    pub const OVERRIDE_ENV: &'static str = "ANY_MCP_ACCEPTANCE_VALIDATOR";

    /// Pins the first host executable that production would also admit.
    ///
    /// # Errors
    ///
    /// Returns a fixed message when no candidate satisfies the production
    /// executable boundary, so a validator scenario fails loudly instead of
    /// silently degrading into no coverage.
    pub fn discover() -> Result<Self, String> {
        for candidate in validator_candidates() {
            if let Some(pinned) = Self::pin(&candidate) {
                return Ok(pinned);
            }
        }
        Err("artifact validator fixture requires a hash-pinnable file(1) executable".to_owned())
    }

    /// Absolute path declared in the rendered policy.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Lowercase SHA-256 declared in the rendered policy.
    #[must_use]
    pub fn sha256(&self) -> &str {
        &self.sha256
    }

    /// Pins one candidate, mirroring the production executable boundary.
    fn pin(candidate: &Path) -> Option<Self> {
        // Production refuses a symlinked declaration outright, so the fixture
        // declares the resolved target instead of the launcher symlink.
        let path = fs::canonicalize(candidate).ok()?;
        if !path.is_absolute() {
            return None;
        }
        let metadata = fs::symlink_metadata(&path).ok()?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return None;
        }
        if !pinnable_executable(&metadata) {
            return None;
        }
        let bytes = fs::read(&path).ok()?;
        if !bytes.starts_with(b"\x7fELF") && VALIDATOR_PLATFORM_ACTIVATES {
            return None;
        }
        Some(Self {
            path,
            sha256: artifact_sha256(&bytes),
        })
    }
}

/// Candidate validator executables in priority order.
fn validator_candidates() -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(selected) = std::env::var_os(PinnedValidatorExecutable::OVERRIDE_ENV) {
        candidates.push(PathBuf::from(selected));
    }
    if let Some(search) = std::env::var_os("PATH") {
        candidates.extend(std::env::split_paths(&search).map(|entry| entry.join("file")));
    }
    candidates.push(PathBuf::from("/usr/bin/file"));
    candidates.push(PathBuf::from("/bin/file"));
    // Platforms that never activate a validator still need one real, readable
    // declaration, because the policy is parsed everywhere it is rendered.
    if !VALIDATOR_PLATFORM_ACTIVATES && let Ok(current) = std::env::current_exe() {
        candidates.push(current);
    }
    candidates
}

/// Whether production would admit this executable's ownership and mode.
///
/// The rule mirrors production's `safe_executable_metadata` exactly, so every
/// host that production accepts is also pinnable: a trusted root-owned
/// executable only has to deny group and other writers, while a caller-owned
/// executable must deny every writer. A stricter fixture rule would make the
/// validator scenarios unrunnable on mainstream distributions, whose stock
/// `file(1)` is root-owned mode `0755`.
#[cfg(unix)]
fn pinnable_executable(metadata: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;

    if !VALIDATOR_PLATFORM_ACTIVATES {
        // A non-activating platform never opens or executes the declaration;
        // it only has to parse a real path, so ordinary build-output modes
        // must remain pinnable there.
        return metadata.is_file() && metadata.mode() & 0o100 != 0;
    }
    // SAFETY: `geteuid` reads process identity and has no preconditions.
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
fn pinnable_executable(metadata: &fs::Metadata) -> bool {
    metadata.is_file()
}

/// Whether a fixture-relative path is exactly one ordinary file name.
///
/// `Path::join` silently replaces the base when the joined path is absolute or
/// drive-qualified, so fixture confinement depends on this check. It is
/// deliberately platform-independent: a single normal component plus a literal
/// rejection of `..`, both separators, and a drive separator gives the same
/// admitted set on Unix and Windows.
fn simple_relative_name(relative: &str) -> bool {
    if relative.is_empty()
        || relative.contains("..")
        || relative.contains(['/', '\\', ':'])
        || relative.contains(|character: char| character.is_control())
    {
        return false;
    }
    let mut components = Path::new(relative).components();
    matches!(
        (components.next(), components.next()),
        (Some(std::path::Component::Normal(_)), None)
    )
}

/// Closed numeric-limit profiles used by lifecycle acceptance fixtures.
///
/// Every non-default profile stays inside production configuration bounds. The
/// profiles deliberately lower limits instead of adding test-only runtime
/// bypasses, so quota, expiry, and payload evidence exercises the same parser
/// and staging implementation as an operator configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum ArtifactLimitProfile {
    /// Production defaults; the rendered policy omits `[limits]`.
    #[default]
    Default,
    /// Two-entry, one-MiB aggregate quota with sub-MiB artifacts.
    Quota,
    /// Minimum admitted sixty-second TTL and bounded cleanup batch.
    TtlCleanup,
    /// One-MiB artifact ceiling with sixty-four-KiB transfer chunks.
    PayloadCeiling,
}

impl ArtifactLimitProfile {
    /// Complete closed lifecycle limit inventory.
    pub const ALL: [Self; 4] = [
        Self::Default,
        Self::Quota,
        Self::TtlCleanup,
        Self::PayloadCeiling,
    ];

    /// Stable identifier used in evidence and failure reports.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Quota => "quota",
            Self::TtlCleanup => "ttl_cleanup",
            Self::PayloadCeiling => "payload_ceiling",
        }
    }

    /// Parses an exact stable identifier.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|candidate| candidate.as_str() == value)
    }

    /// Maximum accepted artifact bytes for this profile.
    #[must_use]
    pub const fn artifact_bytes(self) -> u64 {
        match self {
            Self::Default => 256 * 1024 * 1024,
            Self::Quota => 768 * 1024,
            Self::TtlCleanup => 64 * 1024,
            Self::PayloadCeiling => 1024 * 1024,
        }
    }

    /// Staged-record TTL in seconds for this profile.
    #[must_use]
    pub const fn staging_ttl_secs(self) -> u64 {
        match self {
            Self::Default | Self::Quota => 900,
            Self::TtlCleanup | Self::PayloadCeiling => 60,
        }
    }

    fn render(self) -> String {
        match self {
            Self::Default => String::new(),
            Self::Quota => format!(
                "[limits]\nartifact_bytes = {}\ntransfer_chunk_bytes = 65536\nstaging_total_bytes = 1048576\nstaging_entries = 2\ncleanup_batch = 2\n",
                self.artifact_bytes()
            ),
            Self::TtlCleanup => format!(
                "[limits]\nartifact_bytes = {}\ntransfer_chunk_bytes = 65536\nstaging_total_bytes = 1048576\nstaging_entries = 8\nstaging_ttl_secs = {}\ncleanup_batch = 8\n",
                self.artifact_bytes(),
                self.staging_ttl_secs()
            ),
            Self::PayloadCeiling => format!(
                "[limits]\nartifact_bytes = {}\ntransfer_chunk_bytes = 65536\nstaging_total_bytes = 4194304\nstaging_entries = 8\nstaging_ttl_secs = {}\ncleanup_batch = 8\n",
                self.artifact_bytes(),
                self.staging_ttl_secs()
            ),
        }
    }
}

/// Strict artifact policy options shared by every acceptance fixture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArtifactPolicyOptions {
    /// Whether the remote HTTP staging service is enabled.
    pub staging: bool,
    /// Whether the server under test runs in read-only mode.
    ///
    /// Read-only is a server mode, never a policy field: `ArtifactConfig`
    /// rejects a configuration declaring `spaces.read_only = true`, so the
    /// rendered policy always declares `read_only = false` and the caller
    /// selects read-only through the production server's read-only switch
    /// (`ANY_MCP_READ_ONLY` for a spawned child).
    pub read_only: bool,
    /// Configured space policy shape.
    pub spaces: FixtureSpacePolicy,
    /// Configured validator declaration shape.
    pub validators: FixtureValidatorPolicy,
    /// Numeric-limit profile rendered into the strict policy.
    pub limits: ArtifactLimitProfile,
}

impl Default for ArtifactPolicyOptions {
    fn default() -> Self {
        Self {
            staging: true,
            read_only: false,
            spaces: FixtureSpacePolicy::AllowedUnderTest,
            validators: FixtureValidatorPolicy::Absent,
            limits: ArtifactLimitProfile::Default,
        }
    }
}

/// Content-free exact snapshot of one acceptance fixture directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ArtifactDirectorySnapshot {
    /// Owner-private staging lock files.
    pub lock_files: u64,
    /// Published private staging records.
    pub record_files: u64,
    /// In-progress private staging files.
    pub temporary_files: u64,
    /// Ordinary export files.
    pub ordinary_files: u64,
    /// Entries outside the closed fixture inventory.
    pub unexpected_entries: u64,
    /// Aggregate bytes across ordinary, record, and temporary files.
    pub total_file_bytes: u64,
}

impl ArtifactDirectorySnapshot {
    /// Whether a staging directory contains only its single process lock.
    #[must_use]
    pub const fn is_reaped(self) -> bool {
        self.lock_files == 1
            && self.record_files == 0
            && self.temporary_files == 0
            && self.ordinary_files == 0
            && self.unexpected_entries == 0
            && self.total_file_bytes == 0
    }
}

/// Private temporary operator policy, roots, and seeded import sources.
///
/// Dropping the fixture removes the complete tree, so no acceptance byte
/// survives a scenario.
#[derive(Debug)]
pub struct ArtifactPolicyFixture {
    base: PathBuf,
    config: PathBuf,
    import: PathBuf,
    export: PathBuf,
    staging: PathBuf,
    staging_base_url: Option<String>,
    validator: Option<PinnedValidatorExecutable>,
    options: ArtifactPolicyOptions,
}

/// Replaces the selected configured directory with a symlink to its renamed
/// original, leaving the policy text unchanged for a startup rejection probe.
///
/// # Errors
///
/// Returns a fixed category when a supported target cannot rename the private
/// directory or create the required directory symlink.
pub fn prepare_artifact_symlink_startup_case(
    policy: &ArtifactPolicyFixture,
    target: ArtifactSymlinkStartupTarget,
) -> Result<bool, String> {
    let path = match target {
        ArtifactSymlinkStartupTarget::ImportRoot => &policy.import,
        ArtifactSymlinkStartupTarget::StagingRoot => &policy.staging,
    };
    let retained = policy.base.join(match target {
        ArtifactSymlinkStartupTarget::ImportRoot => "startup-import-retained",
        ArtifactSymlinkStartupTarget::StagingRoot => "startup-staging-retained",
    });
    fs::rename(path, &retained)
        .map_err(|_| "prepare artifact symlink startup fixture".to_owned())?;

    #[cfg(unix)]
    std::os::unix::fs::symlink(&retained, path)
        .map_err(|_| "prepare artifact symlink startup fixture".to_owned())?;
    #[cfg(windows)]
    std::os::windows::fs::symlink_dir(&retained, path)
        .map_err(|_| "prepare artifact symlink startup fixture".to_owned())?;
    #[cfg(not(any(unix, windows)))]
    {
        fs::rename(&retained, path)
            .map_err(|_| "restore unsupported artifact startup fixture".to_owned())?;
        return Ok(false);
    }
    Ok(true)
}

/// Records the exact SYM-11/SYM-12 startup observations.
///
/// # Errors
///
/// Returns a fixed category if a child reports the wrong diagnostic or the
/// startup partition cannot be recorded exactly once.
pub fn record_artifact_dynamic_filesystem_startup_cases(
    sym11: ArtifactStartupCaseOutcome,
    sym12: ArtifactStartupCaseOutcome,
) -> Result<AdversarialExecution, String> {
    let mut execution = AdversarialExecution::default();
    for (id, observed, expected) in [
        (
            AdversarialCaseId::Sym11,
            sym11,
            "invalid any-mcp artifact root",
        ),
        (
            AdversarialCaseId::Sym12,
            sym12,
            "invalid any-mcp staging policy",
        ),
    ] {
        match observed {
            ArtifactStartupCaseOutcome::Rejected(category) if category == expected => {
                execution.record_executed(id)?;
            }
            ArtifactStartupCaseOutcome::Unsupported if !cfg!(any(unix, windows)) => {
                execution.record_unsupported_with_reason(id, "symlink_creation_unavailable")?;
            }
            ArtifactStartupCaseOutcome::Unsupported => {
                return Err(
                    "artifact startup symlink setup was unexpectedly unavailable".to_owned(),
                );
            }
            ArtifactStartupCaseOutcome::Rejected(_) => {
                return Err("artifact startup rejection category diverged".to_owned());
            }
        }
    }
    execution.record_quota_not_applicable();
    Ok(execution)
}

impl ArtifactPolicyFixture {
    /// Logical import root identifier declared by the fixture policy.
    pub const IMPORT_ROOT: &'static str = "inbox";
    /// Logical export root identifier declared by the fixture policy.
    pub const EXPORT_ROOT: &'static str = "outbox";
    /// Relative path of the seeded binary import source.
    pub const FILE_SOURCE: &'static str = "file.bin";
    /// Relative path of the seeded document-create source.
    pub const CREATE_SOURCE: &'static str = "create.md";
    /// Relative path of the seeded document-update source.
    pub const UPDATE_SOURCE: &'static str = "update.md";

    /// Creates the default read-write fixture with staging enabled.
    ///
    /// # Errors
    ///
    /// Returns a fixed message when a directory, source, or policy file cannot
    /// be created with private permissions.
    pub fn create(space_id: &str) -> Result<Self, String> {
        Self::create_with(space_id, ArtifactPolicyOptions::default())
    }

    /// Creates a fixture with explicit staging and read-only policy.
    ///
    /// # Errors
    ///
    /// Returns a fixed message when a directory, source, or policy file cannot
    /// be created with private permissions.
    pub fn create_with(space_id: &str, options: ArtifactPolicyOptions) -> Result<Self, String> {
        if space_id.is_empty() || space_id.len() > 512 {
            return Err("artifact fixture requires an exact space identity".to_owned());
        }
        let base =
            std::env::temp_dir().join(format!("any-mcp-artifact-harness-{}", unique_suffix()));
        let import = base.join("import");
        let export = base.join("export");
        let staging = base.join("staging");
        fs::create_dir(&base)
            .and_then(|()| fs::create_dir(&import))
            .and_then(|()| fs::create_dir(&export))
            .and_then(|()| fs::create_dir(&staging))
            .map_err(|_| "create artifact acceptance directories".to_owned())?;
        secure_directories(&[&base, &import, &export, &staging])?;

        fs::write(import.join(Self::FILE_SOURCE), ARTIFACT_FILE_PAYLOAD)
            .and_then(|()| fs::write(import.join(Self::CREATE_SOURCE), ARTIFACT_CREATE_MARKDOWN))
            .and_then(|()| fs::write(import.join(Self::UPDATE_SOURCE), ARTIFACT_UPDATE_MARKDOWN))
            .map_err(|_| "write artifact acceptance sources".to_owned())?;
        secure_files(&[
            import.join(Self::FILE_SOURCE),
            import.join(Self::CREATE_SOURCE),
            import.join(Self::UPDATE_SOURCE),
        ])?;

        let staging_base_url = if options.staging {
            let port = reserve_loopback_port()?;
            Some(format!("http://127.0.0.1:{port}/artifacts/v1/"))
        } else {
            None
        };
        let validator = if options.validators.is_declared() {
            Some(PinnedValidatorExecutable::discover()?)
        } else {
            None
        };
        let config = base.join("policy.toml");
        fs::write(
            &config,
            render_policy(
                space_id,
                &import,
                &export,
                &staging,
                staging_base_url.as_deref(),
                validator.as_ref(),
                options,
            ),
        )
        .map_err(|_| "write artifact acceptance policy".to_owned())?;
        secure_files(std::slice::from_ref(&config))?;

        Ok(Self {
            base,
            config,
            import,
            export,
            staging,
            staging_base_url,
            validator,
            options,
        })
    }

    /// Path of the strict operator policy passed through `ANY_MCP_CONFIG`.
    #[must_use]
    pub fn config_path(&self) -> &Path {
        &self.config
    }

    /// Returns the private fixture-base bytes for a redacted server-log audit.
    ///
    /// Callers pass the value directly to [`audit_server_log`] and must not
    /// include it in failure text or content-free evidence.
    #[must_use]
    pub fn log_forbidden_needle(&self) -> Zeroizing<Vec<u8>> {
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;

            Zeroizing::new(self.base.as_os_str().as_bytes().to_vec())
        }
        #[cfg(not(unix))]
        Zeroizing::new(self.base.to_string_lossy().as_bytes().to_vec())
    }

    /// Returns the private fixture-base bytes for immediate log auditing.
    #[must_use]
    pub fn forbidden_log_needle(&self) -> Zeroizing<Vec<u8>> {
        self.log_forbidden_needle()
    }

    /// Physical directory backing the logical import root.
    #[must_use]
    pub fn import_root(&self) -> &Path {
        &self.import
    }

    /// Physical directory backing the logical export root.
    #[must_use]
    pub fn export_root(&self) -> &Path {
        &self.export
    }

    /// Configured staging base URL, when staging is enabled.
    #[must_use]
    pub fn staging_base_url(&self) -> Option<&str> {
        self.staging_base_url.as_deref()
    }

    /// Selected policy options.
    #[must_use]
    pub const fn options(&self) -> ArtifactPolicyOptions {
        self.options
    }

    /// Executable pinned into the rendered validator declaration, when any.
    #[must_use]
    pub fn validator(&self) -> Option<&PinnedValidatorExecutable> {
        self.validator.as_ref()
    }

    /// Writes one exact import source under the authorized import root.
    ///
    /// Content scenarios seed their own sources, because every scenario needs
    /// different bytes and every byte must disappear with the fixture tree.
    ///
    /// # Errors
    ///
    /// Returns a fixed message when the relative path is unsafe or the source
    /// cannot be written with private permissions.
    pub fn seed_import(&self, relative: &str, bytes: &[u8]) -> Result<(), String> {
        if !simple_relative_name(relative) {
            return Err("import fixture path must be a simple file name".to_owned());
        }
        let path = self.import.join(relative);
        fs::write(&path, bytes).map_err(|_| "write artifact import source".to_owned())?;
        secure_files(std::slice::from_ref(&path))
    }

    /// Reads the complete strict policy contents.
    ///
    /// # Errors
    ///
    /// Returns a fixed message when the policy cannot be read.
    pub fn policy_contents(&self) -> Result<String, String> {
        fs::read_to_string(&self.config).map_err(|_| "read artifact acceptance policy".to_owned())
    }

    /// Reads exact bytes published under the export root.
    ///
    /// # Errors
    ///
    /// Returns a fixed message when the relative path is unsafe or unreadable.
    pub fn read_export(&self, relative: &str) -> Result<Vec<u8>, String> {
        if !simple_relative_name(relative) {
            return Err("export fixture path must be a simple file name".to_owned());
        }
        fs::read(self.export.join(relative)).map_err(|_| "read artifact export".to_owned())
    }

    /// Whether an export artifact exists under the export root.
    #[must_use]
    pub fn export_exists(&self, relative: &str) -> bool {
        simple_relative_name(relative) && self.export.join(relative).is_file()
    }

    /// Returns a counts-only exact snapshot of the private staging directory.
    ///
    /// # Errors
    ///
    /// Returns a fixed message when an entry cannot be classified or inspected.
    pub fn staging_snapshot(&self) -> Result<ArtifactDirectorySnapshot, String> {
        directory_snapshot(&self.staging, true)
    }

    /// Returns a counts-only exact snapshot of the authorized export directory.
    ///
    /// # Errors
    ///
    /// Returns a fixed message when an entry cannot be classified or inspected.
    pub fn export_snapshot(&self) -> Result<ArtifactDirectorySnapshot, String> {
        directory_snapshot(&self.export, false)
    }

    /// Private marker written when the test-only pre-dispatch pause is reached.
    #[must_use]
    pub fn acceptance_pause_ready_path(&self) -> PathBuf {
        self.base.join("acceptance-pause-ready")
    }

    /// Private marker written after the paused import future releases its resources.
    #[must_use]
    pub fn acceptance_pause_released_path(&self) -> PathBuf {
        self.base.join("acceptance-pause-released")
    }

    /// Returns the private fixture directory used for child-only test capabilities.
    #[must_use]
    pub fn acceptance_gate_base(&self) -> &Path {
        &self.base
    }

    /// Whether the complete private fixture tree still exists.
    #[must_use]
    pub fn tree_exists(&self) -> bool {
        self.base.is_dir()
    }
}

fn directory_snapshot(path: &Path, staging: bool) -> Result<ArtifactDirectorySnapshot, String> {
    let mut snapshot = ArtifactDirectorySnapshot::default();
    let entries =
        fs::read_dir(path).map_err(|_| "inspect artifact fixture directory".to_owned())?;
    for entry in entries {
        let entry = entry.map_err(|_| "inspect artifact fixture directory".to_owned())?;
        let metadata = entry
            .metadata()
            .map_err(|_| "inspect artifact fixture directory".to_owned())?;
        if !metadata.is_file() {
            snapshot.unexpected_entries = snapshot.unexpected_entries.saturating_add(1);
            continue;
        }
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if staging && name == ".any-mcp-staging.lock" {
            snapshot.lock_files = snapshot.lock_files.saturating_add(1);
            continue;
        }
        snapshot.total_file_bytes = snapshot.total_file_bytes.saturating_add(metadata.len());
        if staging
            && name
                .strip_suffix(".bin")
                .is_some_and(|stem| stem.len() == 32 && stem.bytes().all(lowercase_hex_byte))
        {
            snapshot.record_files = snapshot.record_files.saturating_add(1);
        } else if staging
            && name
                .strip_prefix(".any-mcp-")
                .and_then(|value| value.strip_suffix(".tmp"))
                .is_some_and(|stem| stem.len() == 16 && stem.bytes().all(lowercase_hex_byte))
        {
            snapshot.temporary_files = snapshot.temporary_files.saturating_add(1);
        } else if staging {
            snapshot.unexpected_entries = snapshot.unexpected_entries.saturating_add(1);
        } else {
            snapshot.ordinary_files = snapshot.ordinary_files.saturating_add(1);
        }
    }
    Ok(snapshot)
}

fn lowercase_hex_byte(byte: u8) -> bool {
    byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)
}

impl Drop for ArtifactPolicyFixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.base);
    }
}

fn render_policy(
    space_id: &str,
    import: &Path,
    export: &Path,
    staging: &Path,
    staging_base_url: Option<&str>,
    validator: Option<&PinnedValidatorExecutable>,
    options: ArtifactPolicyOptions,
) -> String {
    // `ArtifactConfig` rejects `spaces.read_only = true` outright, so the
    // policy always declares a writable configuration and read-only coverage
    // uses the production server's read-only mode instead.
    let allowed = match options.spaces {
        FixtureSpacePolicy::Omitted => String::new(),
        FixtureSpacePolicy::Empty => "allowed = []\n".to_owned(),
        FixtureSpacePolicy::AllowedUnderTest => {
            format!(
                "allowed = [{{ id = \"{}\" }}]\n",
                toml_basic_string(space_id)
            )
        }
        FixtureSpacePolicy::RestrictedElsewhere => format!(
            "allowed = [{{ id = \"{}\" }}]\n",
            toml_basic_string(UNAUTHORIZED_SPACE_ID)
        ),
    };
    let mut contents = format!(
        "schema_version = 1\n\
         [spaces]\n\
         read_only = false\n\
         {}\
         {}\
         [[roots.import]]\n\
         id = \"{}\"\n\
         path = \"{}\"\n\
         [[roots.export]]\n\
         id = \"{}\"\n\
         path = \"{}\"\n",
        allowed,
        options.limits.render(),
        toml_basic_string(ArtifactPolicyFixture::IMPORT_ROOT),
        toml_basic_string(&import.display().to_string()),
        toml_basic_string(ArtifactPolicyFixture::EXPORT_ROOT),
        toml_basic_string(&export.display().to_string()),
    );
    if let Some(base_url) = staging_base_url {
        let bind = base_url
            .strip_prefix("http://")
            .and_then(|rest| rest.split('/').next())
            .unwrap_or("127.0.0.1:0");
        contents.push_str(&format!(
            "[staging]\n\
             enabled = true\n\
             root = \"{}\"\n\
             bind = \"{}\"\n\
             public_base_url = \"{}\"\n",
            toml_basic_string(&staging.display().to_string()),
            toml_basic_string(bind),
            toml_basic_string(base_url),
        ));
    }
    if let Some(validator) = validator.filter(|_| options.validators.is_declared()) {
        let mime = FIXTURE_VALIDATOR_MIME
            .iter()
            .map(|pattern| format!("\"{}\"", toml_basic_string(pattern)))
            .collect::<Vec<_>>()
            .join(", ");
        contents.push_str(&format!(
            "[[validators]]\n\
             id = \"{}\"\n\
             driver = \"file-mime\"\n\
             path = \"{}\"\n\
             sha256 = \"{}\"\n\
             required = {}\n\
             mime = [{mime}]\n\
             timeout_secs = 20\n\
             memory_bytes = 268435456\n\
             input_bytes = 1048576\n\
             stdout_bytes = 4096\n\
             stderr_bytes = 4096\n\
             fields = 4\n\
             field_bytes = 255\n\
             platform = \"linux-retained-fd-v1\"\n",
            toml_basic_string(FIXTURE_VALIDATOR_ID),
            toml_basic_string(&validator.path().display().to_string()),
            toml_basic_string(validator.sha256()),
            options.validators.is_required(),
        ));
    }
    contents
}

/// Escapes one value for a TOML basic string.
///
/// Windows fixture paths contain backslashes, which are escape introducers in a
/// TOML basic string; emitting them verbatim produces an unparsable policy file.
fn toml_basic_string(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '\\' => escaped.push_str("\\\\"),
            '"' => escaped.push_str("\\\""),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            control if control.is_control() => {
                escaped.push_str(&format!("\\u{:04X}", u32::from(control)));
            }
            other => escaped.push(other),
        }
    }
    escaped
}

fn reserve_loopback_port() -> Result<u16, String> {
    std::net::TcpListener::bind("127.0.0.1:0")
        .and_then(|listener| listener.local_addr())
        .map(|address| address.port())
        .map_err(|_| "reserve artifact acceptance port".to_owned())
}

#[cfg(unix)]
fn secure_directories(directories: &[&PathBuf]) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    for directory in directories {
        fs::set_permissions(directory, fs::Permissions::from_mode(0o700))
            .map_err(|_| "secure artifact acceptance directory".to_owned())?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn secure_directories(_directories: &[&PathBuf]) -> Result<(), String> {
    Ok(())
}

#[cfg(unix)]
fn secure_files(files: &[PathBuf]) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    for file in files {
        fs::set_permissions(file, fs::Permissions::from_mode(0o600))
            .map_err(|_| "secure artifact acceptance file".to_owned())?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn secure_files(_files: &[PathBuf]) -> Result<(), String> {
    Ok(())
}

/// Content-free result of one transport's artifact smoke scenario.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactSmokeEvidence {
    /// Stable transport identifier that produced this evidence.
    pub transport: &'static str,
    /// Exact advertised artifact catalog and schema snapshot.
    pub catalog: ArtifactCatalogSnapshot,
    /// Authorized import roots reported by `artifact_status`.
    pub import_root_count: u64,
    /// Authorized export roots reported by `artifact_status`.
    pub export_root_count: u64,
    /// Whether the staging service reported itself active.
    pub staging_active: bool,
    /// Verified imported and exported file byte length.
    pub file_bytes: u64,
    /// Verified SHA-256 of the round-tripped file bytes.
    pub file_sha256: String,
    /// Canonical Markdown hash proven after document creation.
    pub created_document_sha256: String,
    /// Canonical Markdown hash proven after the exported readback.
    pub exported_document_sha256: String,
    /// Canonical Markdown hash proven after the document update.
    pub updated_document_sha256: String,
    /// Whether an explicitly allocated staging record was released.
    pub stage_released: bool,
}

/// Transport-independent projection compared across the acceptance matrix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactParityKey {
    catalog_digest: String,
    import_root_count: u64,
    export_root_count: u64,
    staging_active: bool,
    file_bytes: u64,
    file_sha256: String,
    created_document_sha256: String,
    exported_document_sha256: String,
    updated_document_sha256: String,
    stage_released: bool,
}

impl ArtifactSmokeEvidence {
    /// Projection that every transport must reproduce exactly.
    #[must_use]
    pub fn parity_key(&self) -> ArtifactParityKey {
        ArtifactParityKey {
            catalog_digest: self.catalog.digest(),
            import_root_count: self.import_root_count,
            export_root_count: self.export_root_count,
            staging_active: self.staging_active,
            file_bytes: self.file_bytes,
            file_sha256: self.file_sha256.clone(),
            created_document_sha256: self.created_document_sha256.clone(),
            exported_document_sha256: self.exported_document_sha256.clone(),
            updated_document_sha256: self.updated_document_sha256.clone(),
            stage_released: self.stage_released,
        }
    }
}

/// Proves that every executed transport observed the same artifact behavior.
///
/// # Errors
///
/// Returns a fixed message naming the first divergent transport, or reporting
/// an incomplete or duplicated executed matrix.
pub fn assert_artifact_parity(
    executed: &[ArtifactSmokeEvidence],
    expected: &[ArtifactTransport],
) -> Result<(), String> {
    if executed.len() != expected.len() {
        return Err("executed artifact transport matrix is incomplete".to_owned());
    }
    let mut observed = Vec::with_capacity(executed.len());
    for (evidence, transport) in executed.iter().zip(expected) {
        if evidence.transport != transport.id() {
            return Err(format!(
                "artifact transport evidence out of order: {}",
                evidence.transport
            ));
        }
        if observed.contains(&evidence.transport) {
            return Err(format!(
                "duplicate artifact transport evidence: {}",
                evidence.transport
            ));
        }
        observed.push(evidence.transport);
    }
    let Some(baseline) = executed.first() else {
        return Err("artifact parity requires at least one executed transport".to_owned());
    };
    let baseline_key = baseline.parity_key();
    for evidence in executed.iter().skip(1) {
        baseline.catalog.compare(&evidence.catalog)?;
        if evidence.parity_key() != baseline_key {
            return Err(format!(
                "artifact transport diverged from {}: {}",
                baseline.transport, evidence.transport
            ));
        }
    }
    Ok(())
}

/// Fixture inputs owned by the caller rather than by a transport driver.
pub struct ArtifactSmokeFixture<'a> {
    /// Transport under test.
    pub transport: ArtifactTransport,
    /// Strict operator policy backing the server under test.
    pub policy: &'a ArtifactPolicyFixture,
    /// Disposable space context that owns every created resource.
    pub ctx: &'a TestContext,
}

/// Runs the complete artifact smoke scenario through one transport.
///
/// The scenario imports and exports a file, creates, exports, and updates a
/// document, and allocates then releases an explicit staging record. Every
/// created Anytype resource is registered with the disposable context before
/// the next step, so a mid-scenario failure still tears down exactly.
///
/// # Errors
///
/// Returns a fixed message describing the first failed stage; no artifact
/// bytes, staging bearer, or upstream body is retained.
pub async fn run_artifact_smoke_scenario(
    driver: &mut impl McpDriver,
    fixture: &ArtifactSmokeFixture<'_>,
) -> Result<ArtifactSmokeEvidence, String> {
    let transport = fixture.transport;
    let space_id = fixture.ctx.space_id.as_str();
    let suffix = unique_suffix();

    let descriptors = driver.list_tool_descriptors().await?;
    let catalog = ArtifactCatalogSnapshot::from_descriptors(&descriptors)?;
    ArtifactCatalogSnapshot::reviewed()?.compare(&catalog)?;

    let status = driver.call_tool("artifact_status", json!({})).await?;
    let import_root_count = required_u64(&status, "import_root_count")?;
    let export_root_count = required_u64(&status, "export_root_count")?;
    let staging_configured = status["staging_configured"] == Value::Bool(true);
    let staging_active = status["staging_active"] == Value::Bool(true);
    if import_root_count != 1 || export_root_count != 1 {
        return Err("artifact status did not report the fixture roots".to_owned());
    }
    if staging_configured != fixture.policy.options().staging
        || staging_active != fixture.policy.options().staging
    {
        return Err("artifact status did not report the configured staging service".to_owned());
    }
    if transport.data() == ArtifactDataPlane::RemoteStaging && !staging_active {
        return Err("remote staging transport requires an active staging service".to_owned());
    }

    let file_source = artifact_source(
        driver,
        &transport,
        space_id,
        ARTIFACT_FILE_PAYLOAD,
        ARTIFACT_FILE_MEDIA_TYPE,
        ArtifactPolicyFixture::FILE_SOURCE,
    )
    .await?;
    let imported = driver
        .call_tool(
            "file_import",
            json!({
                "space": space_id,
                "source": file_source,
                "name": format!("artifact-{suffix}.bin"),
                "media_type": ARTIFACT_FILE_MEDIA_TYPE,
                "idempotency_key": format!("artifact-file-import-{suffix}")
            }),
        )
        .await?;
    let file_id = required_str(&imported, "/file_id")?;
    fixture.ctx.register_file(&file_id);
    let file_sha256 = required_str(&imported, "/receipt/sha256")?;
    let file_bytes = imported
        .pointer("/receipt/size_bytes")
        .and_then(Value::as_u64)
        .ok_or_else(|| "file import omitted its verified size".to_owned())?;
    if file_sha256 != artifact_sha256(ARTIFACT_FILE_PAYLOAD)
        || file_bytes != ARTIFACT_FILE_PAYLOAD.len() as u64
    {
        return Err("file import did not verify the exact fixture bytes".to_owned());
    }

    let export_name = format!("file-export-{suffix}.bin");
    let exported = driver
        .call_tool(
            "file_export",
            json!({
                "space": space_id,
                "file_id": file_id,
                "destination": artifact_destination(&transport, &export_name),
                "idempotency_key": format!("artifact-file-export-{suffix}")
            }),
        )
        .await?;
    let exported_bytes =
        read_exported_bytes(&transport, fixture.policy, &exported, &export_name).await?;
    if exported_bytes != ARTIFACT_FILE_PAYLOAD
        || required_str(&exported, "/receipt/sha256")? != file_sha256
    {
        return Err("file export did not republish the exact imported bytes".to_owned());
    }
    release_remote_receipt(driver, &transport, &exported).await?;

    let create_source = artifact_source(
        driver,
        &transport,
        space_id,
        ARTIFACT_CREATE_MARKDOWN.as_bytes(),
        ARTIFACT_MARKDOWN_MEDIA_TYPE,
        ArtifactPolicyFixture::CREATE_SOURCE,
    )
    .await?;
    let created = driver
        .call_tool(
            "document_import_create",
            json!({
                "space": space_id,
                "source": create_source,
                "source_format": "markdown",
                "object_type": "page",
                "name": format!("Artifact document {suffix}"),
                "idempotency_key": format!("artifact-document-create-{suffix}")
            }),
        )
        .await?;
    let object_id = required_str(&created, "/object_id")?;
    fixture.ctx.register_object(&object_id);
    let created_document_sha256 = required_str(&created, "/canonical_sha256")?;
    if required_str(&created, "/source_sha256")?
        != artifact_sha256(ARTIFACT_CREATE_MARKDOWN.as_bytes())
    {
        return Err("document create did not verify the exact source bytes".to_owned());
    }

    let document_name = format!("document-export-{suffix}.md");
    let document_export = driver
        .call_tool(
            "document_export",
            json!({
                "space": space_id,
                "object_id": object_id,
                "destination": artifact_destination(&transport, &document_name),
                "expected_body_sha256": created_document_sha256,
                "idempotency_key": format!("artifact-document-export-{suffix}")
            }),
        )
        .await?;
    let exported_document_sha256 = required_str(&document_export, "/sha256")?;
    let exported_document =
        read_exported_bytes(&transport, fixture.policy, &document_export, &document_name).await?;
    if artifact_sha256(&exported_document) != exported_document_sha256 {
        return Err("document export bytes did not match the reported hash".to_owned());
    }
    release_remote_receipt(driver, &transport, &document_export).await?;

    let update_source = artifact_source(
        driver,
        &transport,
        space_id,
        ARTIFACT_UPDATE_MARKDOWN.as_bytes(),
        ARTIFACT_MARKDOWN_MEDIA_TYPE,
        ArtifactPolicyFixture::UPDATE_SOURCE,
    )
    .await?;
    let updated = driver
        .call_tool(
            "document_import_update",
            json!({
                "space": space_id,
                "object_id": object_id,
                "source": update_source,
                "source_format": "markdown",
                "expected_body_sha256": created_document_sha256,
                "idempotency_key": format!("artifact-document-update-{suffix}")
            }),
        )
        .await?;
    let updated_document_sha256 = required_str(&updated, "/canonical_sha256")?;
    if updated_document_sha256 == created_document_sha256 || updated["no_op"] != Value::Bool(false)
    {
        return Err("document update did not verify a changed body".to_owned());
    }

    let stage_released = allocate_and_release_stage(driver, space_id).await?;

    Ok(ArtifactSmokeEvidence {
        transport: transport.id(),
        catalog,
        import_root_count,
        export_root_count,
        staging_active,
        file_bytes,
        file_sha256,
        created_document_sha256,
        exported_document_sha256,
        updated_document_sha256,
        stage_released,
    })
}

async fn artifact_source(
    driver: &mut impl McpDriver,
    transport: &ArtifactTransport,
    space_id: &str,
    payload: &[u8],
    media_type: &str,
    local_path: &str,
) -> Result<Value, String> {
    match transport.data() {
        ArtifactDataPlane::LocalRoots => Ok(json!({
            "local": {"root": ArtifactPolicyFixture::IMPORT_ROOT, "path": local_path}
        })),
        ArtifactDataPlane::RemoteStaging => {
            let handle = stage_upload(driver, space_id, payload, media_type).await?;
            Ok(json!({"staged_handle": handle}))
        }
    }
}

fn artifact_destination(transport: &ArtifactTransport, relative: &str) -> Value {
    match transport.data() {
        ArtifactDataPlane::LocalRoots => json!({
            "local": {"root": ArtifactPolicyFixture::EXPORT_ROOT, "path": relative}
        }),
        ArtifactDataPlane::RemoteStaging => json!({"remote": true}),
    }
}

/// Allocates a staging record, uploads exact bytes, and returns its bearer.
async fn stage_upload(
    driver: &mut impl McpDriver,
    space_id: &str,
    payload: &[u8],
    media_type: &str,
) -> Result<String, String> {
    let size = u64::try_from(payload.len())
        .map_err(|_| "staged payload exceeds the addressable range".to_owned())?;
    let expected_sha256 = artifact_sha256(payload);
    let allocation =
        allocate_stage_upload(driver, space_id, size, media_type, Some(&expected_sha256)).await?;
    upload_stage_bytes(&allocation, payload, media_type).await?;
    Ok(allocation.handle.to_string())
}

/// Reads exact published bytes from the selected data plane.
async fn read_exported_bytes(
    transport: &ArtifactTransport,
    policy: &ArtifactPolicyFixture,
    receipt_owner: &Value,
    relative: &str,
) -> Result<Vec<u8>, String> {
    match transport.data() {
        ArtifactDataPlane::LocalRoots => {
            if !policy.export_exists(relative) {
                return Err("export did not create the authorized artifact".to_owned());
            }
            policy.read_export(relative)
        }
        ArtifactDataPlane::RemoteStaging => {
            let handle = required_str(receipt_owner, "/receipt/staging_handle")?;
            let url = required_str(receipt_owner, "/receipt/staging_url")?;
            if url.contains(&handle) {
                return Err("staging URL must never carry the bearer credential".to_owned());
            }
            let response = staging_client()?
                .get(&url)
                .header(AUTHORIZATION, format!("Bearer {handle}"))
                .header(RANGE, "bytes=0-")
                .send()
                .await
                .map_err(|_| "staged download transport failed".to_owned())?;
            if !response.status().is_success() {
                return Err(format!(
                    "staged download rejected with status {}",
                    response.status().as_u16()
                ));
            }
            response
                .bytes()
                .await
                .map(|bytes| bytes.to_vec())
                .map_err(|_| "staged download body failed".to_owned())
        }
    }
}

/// Releases a remote publication so no staged byte outlives the scenario.
async fn release_remote_receipt(
    driver: &mut impl McpDriver,
    transport: &ArtifactTransport,
    receipt_owner: &Value,
) -> Result<(), String> {
    if transport.data() != ArtifactDataPlane::RemoteStaging {
        return Ok(());
    }
    let handle = required_str(receipt_owner, "/receipt/staging_handle")?;
    let released = driver
        .call_tool("artifact_release", json!({"handle": handle}))
        .await?;
    if released["released"] == Value::Bool(true) {
        Ok(())
    } else {
        Err("remote artifact publication was not released".to_owned())
    }
}

async fn allocate_and_release_stage(
    driver: &mut impl McpDriver,
    space_id: &str,
) -> Result<bool, String> {
    let allocation = driver
        .call_tool(
            "artifact_stage_upload",
            json!({
                "space": space_id,
                "size_bytes": ARTIFACT_FILE_PAYLOAD.len(),
                "media_type": ARTIFACT_FILE_MEDIA_TYPE,
                "expected_sha256": artifact_sha256(ARTIFACT_FILE_PAYLOAD)
            }),
        )
        .await?;
    let handle = required_str(&allocation, "/handle")?;
    let record = required_str(&allocation, "/record")?;
    let url = required_str(&allocation, "/upload_url")?;
    if !url.contains(&record) || url.contains(&handle) {
        return Err("staging URL must expose only the non-secret record".to_owned());
    }
    let released = driver
        .call_tool("artifact_release", json!({"handle": handle}))
        .await?;
    Ok(released["released"] == Value::Bool(true))
}

fn staging_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|_| "build staging data-plane client".to_owned())
}

fn required_str(value: &Value, pointer: &str) -> Result<String, String> {
    value
        .pointer(pointer)
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .ok_or_else(|| format!("artifact result omitted {pointer}"))
}

fn take_required_string(value: &mut Value, pointer: &str) -> Result<String, String> {
    match value.pointer_mut(pointer) {
        Some(Value::String(text)) => Ok(std::mem::take(text)),
        _ => Err(format!("artifact result omitted {pointer}")),
    }
}

fn zeroize_json_strings(value: &mut Value) {
    match value {
        Value::String(text) => text.zeroize(),
        Value::Array(values) => values.iter_mut().for_each(zeroize_json_strings),
        Value::Object(fields) => fields.values_mut().for_each(zeroize_json_strings),
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

fn required_u64(value: &Value, field: &str) -> Result<u64, String> {
    value
        .get(field)
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("artifact status omitted {field}"))
}

/// Validates one complete JSON-RPC `tools/call` frame and returns its content.
///
/// The scripted-protocol control plane asserts the exact wire envelope rather
/// than a decoded convenience value.
///
/// # Errors
///
/// Returns a fixed message describing the first envelope violation.
pub fn validate_tool_frame(name: &str, id: u64, frame: &Value) -> Result<Value, String> {
    if frame["jsonrpc"] != Value::String("2.0".to_owned()) {
        return Err(format!("{name} frame omitted the JSON-RPC version"));
    }
    if frame["id"].as_u64() != Some(id) {
        return Err(format!("{name} frame carried a mismatched identifier"));
    }
    if frame.get("error").is_some() {
        return Err(format!("{name} frame returned a protocol error"));
    }
    let result = frame
        .get("result")
        .ok_or_else(|| format!("{name} frame omitted its result"))?;
    if result["isError"] != Value::Bool(false) {
        let code = result
            .pointer("/structuredContent/code")
            .and_then(Value::as_str)
            .filter(|code| {
                matches!(
                    *code,
                    "authentication"
                        | "validation"
                        | "ambiguous"
                        | "not_found"
                        | "conflict"
                        | "bounded_result"
                        | "upstream"
                )
            })
            .unwrap_or("invalid");
        return Err(format!("{name} frame reported tool error {code}"));
    }
    let content_len = result["content"]
        .as_array()
        .ok_or_else(|| format!("{name} frame omitted its content array"))?
        .len();
    if content_len == 0 {
        return Err(format!("{name} frame returned empty content"));
    }
    result
        .get("structuredContent")
        .cloned()
        .ok_or_else(|| format!("{name} frame omitted structured content"))
}

/// Fixed corrective text returned when a mutation reaches a read-only server.
///
/// Mirrors `ToolError::read_only` in `any-mcp/src/error.rs`. The harness is
/// compiled into external test targets that cannot name crate-private items,
/// so the exact expected text is restated here and asserted verbatim.
pub const READ_ONLY_GUIDANCE: &str =
    "This Anytype server is read-only. Mutating workflows are disabled.";

/// Fixed corrective text returned when remote staging is not configured.
///
/// Mirrors the crate-private `STAGING_REQUIRED_GUIDANCE` in
/// `any-mcp/src/artifact_staging.rs`.
pub const STAGING_REQUIRED_GUIDANCE: &str =
    "Remote artifact staging is disabled. Enable it in the selected any-mcp TOML config.";

/// Stable domain error code reported for a policy-denied space.
pub const AUTHENTICATION_CODE: &str = "authentication";
/// Stable domain error code reported for an unauthorized root or absent entity.
pub const NOT_FOUND_CODE: &str = "not_found";
/// Stable domain error code reported for read-only and configuration refusals.
pub const VALIDATION_CODE: &str = "validation";

/// Closed inventory of operator policy and configuration scenarios.
///
/// Every scenario is one complete server configuration. The smoke matrix
/// already proves the permissive local and remote happy paths, so these
/// scenarios prove what a configuration must *refuse*, plus the two
/// permissive space shapes that are configured differently from it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ArtifactPolicyScenario {
    /// `spaces.allowed` omitted: the space under test stays authorized.
    SpacesOmitted,
    /// `spaces.allowed = []`: every space-scoped artifact call is denied.
    SpacesEmpty,
    /// `spaces.allowed` names another space: the space under test is denied.
    SpacesRestrictedElsewhere,
    /// Read-only server: every artifact mutation is unadvertised and refused.
    ReadOnly,
    /// Staging omitted: remote opaque-handle allocation is refused.
    StagingDisabled,
}

impl ArtifactPolicyScenario {
    /// Complete closed policy scenario inventory.
    pub const ALL: [Self; 5] = [
        Self::SpacesOmitted,
        Self::SpacesEmpty,
        Self::SpacesRestrictedElsewhere,
        Self::ReadOnly,
        Self::StagingDisabled,
    ];

    /// Stable identifier used in evidence and failure reports.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SpacesOmitted => "spaces_omitted",
            Self::SpacesEmpty => "spaces_empty",
            Self::SpacesRestrictedElsewhere => "spaces_restricted_elsewhere",
            Self::ReadOnly => "read_only",
            Self::StagingDisabled => "staging_disabled",
        }
    }

    /// Parses an exact stable identifier.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|candidate| candidate.as_str() == value)
    }

    /// Exact fixture policy options that realize this scenario.
    #[must_use]
    pub const fn policy_options(self) -> ArtifactPolicyOptions {
        let (staging, read_only, spaces) = match self {
            Self::SpacesOmitted => (true, false, FixtureSpacePolicy::Omitted),
            Self::SpacesEmpty => (true, false, FixtureSpacePolicy::Empty),
            Self::SpacesRestrictedElsewhere => {
                (true, false, FixtureSpacePolicy::RestrictedElsewhere)
            }
            Self::ReadOnly => (true, true, FixtureSpacePolicy::AllowedUnderTest),
            Self::StagingDisabled => (false, false, FixtureSpacePolicy::AllowedUnderTest),
        };
        ArtifactPolicyOptions {
            staging,
            read_only,
            spaces,
            // Validator behavior is a separate scenario family: policy
            // scenarios must observe an unchanged, validator-free catalog.
            validators: FixtureValidatorPolicy::Absent,
            limits: ArtifactLimitProfile::Default,
        }
    }

    /// Whether the server under test runs in read-only mode.
    #[must_use]
    pub const fn is_read_only(self) -> bool {
        self.policy_options().read_only
    }

    /// Whether the disposable space under test is admitted by policy.
    #[must_use]
    pub const fn admits_space_under_test(self) -> bool {
        self.policy_options().spaces.admits_space_under_test()
    }

    /// Exact sorted artifact tools this configuration must advertise.
    ///
    /// A read-only server removes every artifact mutation from its catalog and
    /// keeps only the read-only status tool.
    #[must_use]
    pub fn advertised_tools(self) -> Vec<&'static str> {
        if self.is_read_only() {
            vec!["artifact_status"]
        } else {
            ARTIFACT_TOOL_NAMES.to_vec()
        }
    }

    /// Exact `artifact_status` report this configuration must produce.
    #[must_use]
    pub const fn expected_status(self) -> ArtifactStatusEvidence {
        let options = self.policy_options();
        // A read-only server never activates local roots, and staging is only
        // activated on top of activated roots, so both report inactive while
        // the configured staging declaration stays visible.
        let roots_active = !options.read_only;
        ArtifactStatusEvidence {
            local_roots_active: roots_active,
            import_root_count: if roots_active { 1 } else { 0 },
            export_root_count: if roots_active { 1 } else { 0 },
            staging_configured: options.staging,
            staging_active: options.staging && roots_active,
        }
    }
}

/// Closed inventory of policy probes executed by every policy scenario.
///
/// Each probe is classified before any Anytype write can occur, so a denied
/// configuration creates nothing and an authorized configuration fails on the
/// next gate instead of mutating the disposable space.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ArtifactPolicyProbe {
    /// Import of a source that does not exist under the authorized import root.
    LocalImportMissingSource,
    /// Export whose destination names the import-only root.
    LocalExportUnauthorizedRoot,
    /// Remote staging allocation for a bounded payload.
    StageUpload,
}

impl ArtifactPolicyProbe {
    /// Complete closed probe inventory.
    pub const ALL: [Self; 3] = [
        Self::LocalImportMissingSource,
        Self::LocalExportUnauthorizedRoot,
        Self::StageUpload,
    ];

    /// Stable identifier used in evidence and failure reports.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::LocalImportMissingSource => "local_import_missing_source",
            Self::LocalExportUnauthorizedRoot => "local_export_unauthorized_root",
            Self::StageUpload => "stage_upload",
        }
    }

    /// Production tool exercised by this probe.
    #[must_use]
    pub const fn tool_name(self) -> &'static str {
        match self {
            Self::LocalImportMissingSource => "file_import",
            Self::LocalExportUnauthorizedRoot => "file_export",
            Self::StageUpload => "artifact_stage_upload",
        }
    }
}

/// Required result of one policy probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtifactProbeExpectation {
    /// The call must succeed; any allocated staging record is released again.
    Accepted,
    /// The call must be refused with this exact code and corrective message.
    Refused {
        /// Stable machine-readable domain error code.
        code: &'static str,
        /// Exact fixed corrective message, when the code alone is ambiguous.
        message: Option<&'static str>,
    },
}

/// Observed result of one policy probe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArtifactProbeOutcome {
    /// The call succeeded.
    Accepted,
    /// The call was refused with this stable code and fixed corrective text.
    Refused {
        /// Stable machine-readable domain error code.
        code: String,
        /// Fixed corrective message; never an upstream body.
        message: String,
    },
}

/// Required outcome of one probe under one policy scenario.
#[must_use]
pub const fn probe_expectation(
    scenario: ArtifactPolicyScenario,
    probe: ArtifactPolicyProbe,
) -> ArtifactProbeExpectation {
    // Read-only is checked first by every artifact mutation, before the space
    // policy and before any root or staging gate.
    if scenario.is_read_only() {
        return ArtifactProbeExpectation::Refused {
            code: VALIDATION_CODE,
            message: Some(READ_ONLY_GUIDANCE),
        };
    }
    if !scenario.admits_space_under_test() {
        return ArtifactProbeExpectation::Refused {
            code: AUTHENTICATION_CODE,
            message: None,
        };
    }
    match probe {
        ArtifactPolicyProbe::StageUpload => {
            if scenario.policy_options().staging {
                ArtifactProbeExpectation::Accepted
            } else {
                ArtifactProbeExpectation::Refused {
                    code: VALIDATION_CODE,
                    message: Some(STAGING_REQUIRED_GUIDANCE),
                }
            }
        }
        // An absent source and an import-only export destination are both
        // reported through the single fixed not-found message, so neither
        // refusal discloses which authorized root exists.
        ArtifactPolicyProbe::LocalImportMissingSource
        | ArtifactPolicyProbe::LocalExportUnauthorizedRoot => ArtifactProbeExpectation::Refused {
            code: NOT_FOUND_CODE,
            message: None,
        },
    }
}

/// Content-free `artifact_status` projection compared across transports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArtifactStatusEvidence {
    /// Whether local roots were activated.
    pub local_roots_active: bool,
    /// Authorized import roots.
    pub import_root_count: u64,
    /// Authorized export roots.
    pub export_root_count: u64,
    /// Whether the policy declares an enabled staging service.
    pub staging_configured: bool,
    /// Whether the staging service is running.
    pub staging_active: bool,
}

impl ArtifactStatusEvidence {
    /// Reads the projection from an `artifact_status` result.
    ///
    /// # Errors
    ///
    /// Returns a fixed message when a required status field is absent.
    pub fn from_status(status: &Value) -> Result<Self, String> {
        Ok(Self {
            local_roots_active: status["local_roots_active"] == Value::Bool(true),
            import_root_count: required_u64(status, "import_root_count")?,
            export_root_count: required_u64(status, "export_root_count")?,
            staging_configured: status["staging_configured"] == Value::Bool(true),
            staging_active: status["staging_active"] == Value::Bool(true),
        })
    }
}

/// Content-free result of one policy scenario on one control plane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactPolicyEvidence {
    /// Stable policy scenario identifier.
    pub scenario: &'static str,
    /// Stable control-plane identifier that produced this evidence.
    pub control: &'static str,
    /// Exact sorted artifact tools advertised by this configuration.
    pub advertised_tools: Vec<String>,
    /// Canonical descriptor digests of the advertised artifact tools.
    pub catalog_digests: BTreeMap<String, String>,
    /// Exact reported artifact status.
    pub status: ArtifactStatusEvidence,
    /// Observed outcome of every executed probe, keyed by probe identifier.
    pub probes: BTreeMap<&'static str, ArtifactProbeOutcome>,
}

impl ArtifactPolicyEvidence {
    /// Whether two control planes observed identical policy behavior.
    ///
    /// Only the control-plane identifier may differ.
    #[must_use]
    fn matches(&self, other: &Self) -> bool {
        self.scenario == other.scenario
            && self.advertised_tools == other.advertised_tools
            && self.catalog_digests == other.catalog_digests
            && self.status == other.status
            && self.probes == other.probes
    }
}

/// Fixture inputs for one policy scenario run.
pub struct ArtifactPolicyRun<'a> {
    /// Policy scenario under test.
    pub scenario: ArtifactPolicyScenario,
    /// Control plane driving the production server.
    pub control: ArtifactControlPlane,
    /// Strict operator policy backing the server under test.
    pub policy: &'a ArtifactPolicyFixture,
    /// Disposable space context that owns every created resource.
    pub ctx: &'a TestContext,
}

/// Runs one policy scenario through one control plane.
///
/// The scenario proves three things about a complete server configuration: the
/// exact advertised artifact catalog, the exact reported artifact status, and
/// the exact refusal (or acceptance) of every probe. Denied configurations
/// create nothing upstream, and an accepted staging allocation is released
/// before the scenario returns, so no staged byte outlives the run.
///
/// # Errors
///
/// Returns a fixed message describing the first violated expectation; no
/// artifact byte, staging bearer, or upstream body is retained.
pub async fn run_artifact_policy_scenario(
    driver: &mut impl McpDriver,
    run: &ArtifactPolicyRun<'_>,
) -> Result<ArtifactPolicyEvidence, String> {
    let scenario = run.scenario;
    if run.policy.options() != scenario.policy_options() {
        return Err(format!(
            "policy fixture does not realize scenario: {}",
            scenario.as_str()
        ));
    }
    let space_id = run.ctx.space_id.as_str();
    let suffix = unique_suffix();

    let descriptors = driver.list_tool_descriptors().await?;
    let catalog_digests = artifact_descriptor_digests(&descriptors)?;
    let advertised_tools = catalog_digests.keys().cloned().collect::<Vec<_>>();
    if advertised_tools != scenario.advertised_tools() {
        return Err(format!(
            "advertised artifact catalog is not exact: {}",
            scenario.as_str()
        ));
    }
    let reviewed = ArtifactCatalogSnapshot::reviewed()?;
    for (name, digest) in &catalog_digests {
        if reviewed.tool_digests().get(name) != Some(digest) {
            return Err(format!("artifact tool contract diverged: {name}"));
        }
    }

    let status = ArtifactStatusEvidence::from_status(
        &driver.call_tool("artifact_status", json!({})).await?,
    )?;
    if status != scenario.expected_status() {
        return Err(format!(
            "artifact status did not match the configuration: {}",
            scenario.as_str()
        ));
    }

    let mut probes = BTreeMap::new();
    for probe in ArtifactPolicyProbe::ALL {
        let outcome = Box::pin(execute_policy_probe(
            driver, scenario, probe, space_id, &suffix,
        ))
        .await?;
        probes.insert(probe.as_str(), outcome);
    }

    Ok(ArtifactPolicyEvidence {
        scenario: scenario.as_str(),
        control: run.control.as_str(),
        advertised_tools,
        catalog_digests,
        status,
        probes,
    })
}

/// Canonical descriptor digests of every advertised artifact tool.
///
/// Unlike [`ArtifactCatalogSnapshot::from_descriptors`] this accepts a reduced
/// inventory, because a read-only configuration advertises only the status
/// tool.
fn artifact_descriptor_digests(descriptors: &[Value]) -> Result<BTreeMap<String, String>, String> {
    let mut digests = BTreeMap::new();
    for descriptor in descriptors {
        let name = descriptor
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| "tools/list descriptor omitted its name".to_owned())?;
        if !ARTIFACT_TOOL_NAMES.contains(&name) {
            continue;
        }
        if digests
            .insert(name.to_owned(), canonical_digest(descriptor))
            .is_some()
        {
            return Err(format!("duplicate artifact tool descriptor: {name}"));
        }
    }
    Ok(digests)
}

async fn execute_policy_probe(
    driver: &mut impl McpDriver,
    scenario: ArtifactPolicyScenario,
    probe: ArtifactPolicyProbe,
    space_id: &str,
    suffix: &str,
) -> Result<ArtifactProbeOutcome, String> {
    let name = probe.tool_name();
    let arguments = policy_probe_arguments(probe, space_id, suffix);
    match probe_expectation(scenario, probe) {
        ArtifactProbeExpectation::Accepted => {
            let accepted = driver.call_tool(name, arguments).await?;
            release_probe_allocation(driver, probe, &accepted).await?;
            Ok(ArtifactProbeOutcome::Accepted)
        }
        ArtifactProbeExpectation::Refused { code, message } => {
            let refusal = driver.call_tool_error(name, arguments).await?;
            if refusal.code() != code {
                return Err(format!(
                    "probe {} was not refused with {code}",
                    probe.as_str()
                ));
            }
            let observed = refusal
                .normalized_result()
                .pointer("/structuredContent/message")
                .and_then(Value::as_str)
                .ok_or_else(|| format!("probe {} omitted its message", probe.as_str()))?
                .to_owned();
            if message.is_some_and(|expected| expected != observed) {
                return Err(format!(
                    "probe {} was refused with unexpected guidance",
                    probe.as_str()
                ));
            }
            Ok(ArtifactProbeOutcome::Refused {
                code: refusal.code().to_owned(),
                message: observed,
            })
        }
    }
}

fn policy_probe_arguments(probe: ArtifactPolicyProbe, space_id: &str, suffix: &str) -> Value {
    match probe {
        ArtifactPolicyProbe::LocalImportMissingSource => json!({
            "space": space_id,
            "source": {"local": {
                "root": ArtifactPolicyFixture::IMPORT_ROOT,
                "path": "policy-absent-source.bin"
            }},
            "name": format!("policy-{suffix}.bin"),
            "media_type": ARTIFACT_FILE_MEDIA_TYPE,
            "idempotency_key": format!("artifact-policy-import-{suffix}")
        }),
        // The import root is declared import-only, so naming it as an export
        // destination must be refused before any upstream file is read.
        ArtifactPolicyProbe::LocalExportUnauthorizedRoot => json!({
            "space": space_id,
            "file_id": format!("policy-absent-file-{suffix}"),
            "destination": {"local": {
                "root": ArtifactPolicyFixture::IMPORT_ROOT,
                "path": format!("policy-export-{suffix}.bin")
            }},
            "idempotency_key": format!("artifact-policy-export-{suffix}")
        }),
        ArtifactPolicyProbe::StageUpload => json!({
            "space": space_id,
            "size_bytes": ARTIFACT_FILE_PAYLOAD.len(),
            "media_type": ARTIFACT_FILE_MEDIA_TYPE,
            "expected_sha256": artifact_sha256(ARTIFACT_FILE_PAYLOAD)
        }),
    }
}

/// Releases an accepted staging allocation so no record outlives the probe.
async fn release_probe_allocation(
    driver: &mut impl McpDriver,
    probe: ArtifactPolicyProbe,
    accepted: &Value,
) -> Result<(), String> {
    if probe != ArtifactPolicyProbe::StageUpload {
        return Ok(());
    }
    let handle = required_str(accepted, "/handle")?;
    let released = driver
        .call_tool("artifact_release", json!({"handle": handle}))
        .await?;
    if released["released"] == Value::Bool(true) {
        Ok(())
    } else {
        Err("accepted staging allocation was not released".to_owned())
    }
}

/// Proves that every control plane observed the same policy behavior.
///
/// # Errors
///
/// Returns a fixed message naming the first divergent control plane, or
/// reporting an incomplete, misordered, or duplicated executed matrix.
pub fn assert_artifact_policy_parity(
    executed: &[ArtifactPolicyEvidence],
    expected: &[ArtifactControlPlane],
) -> Result<(), String> {
    if executed.len() != expected.len() {
        return Err("executed artifact policy matrix is incomplete".to_owned());
    }
    let mut observed = Vec::with_capacity(executed.len());
    for (evidence, control) in executed.iter().zip(expected) {
        if evidence.control != control.as_str() {
            return Err(format!(
                "artifact policy evidence out of order: {}",
                evidence.control
            ));
        }
        if observed.contains(&evidence.control) {
            return Err(format!(
                "duplicate artifact policy evidence: {}",
                evidence.control
            ));
        }
        observed.push(evidence.control);
    }
    let Some(baseline) = executed.first() else {
        return Err("artifact policy parity requires at least one control plane".to_owned());
    };
    for evidence in executed.iter().skip(1) {
        if !baseline.matches(evidence) {
            return Err(format!(
                "artifact policy control plane diverged from {}: {}",
                baseline.control, evidence.control
            ));
        }
    }
    Ok(())
}

/// Representative artifact whose exact bytes and declared MIME are fixed.
///
/// Every fixture carries distinct bytes: Anytype files are content addressed,
/// so identical payloads would collapse into one object and hide a per-MIME
/// difference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ArtifactMimeFixture {
    /// Opaque binary payload with non-textual bytes.
    Binary,
    /// UTF-8 text payload.
    Text,
    /// Complete minimal PNG image.
    Image,
    /// Complete minimal RIFF/WAVE audio clip.
    Audio,
    /// Payload whose declared MIME is outside every registered tree.
    Unknown,
}

/// Exact bytes of a complete minimal 1x1 PNG image.
const MIME_IMAGE_PAYLOAD: [u8; 70] = [
    137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1, 8, 6, 0,
    0, 0, 31, 21, 196, 137, 0, 0, 0, 13, 73, 68, 65, 84, 120, 218, 99, 252, 207, 192, 80, 15, 0, 4,
    133, 1, 128, 132, 169, 140, 33, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
];

/// Exact bytes of a complete minimal 16-bit mono RIFF/WAVE clip.
const MIME_AUDIO_PAYLOAD: [u8; 60] = [
    82, 73, 70, 70, 52, 0, 0, 0, 87, 65, 86, 69, 102, 109, 116, 32, 16, 0, 0, 0, 1, 0, 1, 0, 64,
    31, 0, 0, 128, 62, 0, 0, 2, 0, 16, 0, 100, 97, 116, 97, 16, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0,
];

impl ArtifactMimeFixture {
    /// Complete closed representative-artifact inventory.
    pub const ALL: [Self; 5] = [
        Self::Binary,
        Self::Text,
        Self::Image,
        Self::Audio,
        Self::Unknown,
    ];

    /// Stable identifier used in evidence and failure reports.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Binary => "binary",
            Self::Text => "text",
            Self::Image => "image",
            Self::Audio => "audio",
            Self::Unknown => "unknown",
        }
    }

    /// Parses an exact stable identifier.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|candidate| candidate.as_str() == value)
    }

    /// Canonical MIME essence declared at import and at staging.
    #[must_use]
    pub const fn media_type(self) -> &'static str {
        match self {
            Self::Binary => "application/octet-stream",
            Self::Text => "text/plain",
            Self::Image => "image/png",
            Self::Audio => "audio/wav",
            Self::Unknown => "application/x-any-mcp-unknown",
        }
    }

    /// Exact imported and exported bytes.
    #[must_use]
    pub const fn payload(self) -> &'static [u8] {
        match self {
            Self::Binary => b"\x00\x01\x02\x7f\x80\xfe\xffany-mcp-binary-artifact",
            Self::Text => b"any-mcp acceptance text artifact\n",
            Self::Image => &MIME_IMAGE_PAYLOAD,
            Self::Audio => &MIME_AUDIO_PAYLOAD,
            Self::Unknown => b"ANYMCPUNKNOWN\x01\x02\x03unknown-artifact-bytes",
        }
    }

    /// File-name extension used for the seeded source and the export.
    #[must_use]
    pub const fn extension(self) -> &'static str {
        match self {
            Self::Binary => "bin",
            Self::Text => "txt",
            Self::Image => "png",
            Self::Audio => "wav",
            Self::Unknown => "unknown",
        }
    }
}

/// Exact Markdown source of the acceptance document create.
pub const CONTENT_CREATE_MARKDOWN: &str = "# Artifact content\n\nFirst body line.\n";

/// Exact plain-text source whose canonical form is statically known.
///
/// The value is Unicode alphanumeric text with internal ASCII spaces, which is
/// the closed subset `anytype` models exactly: Anytype stores it with one
/// appended hard-break suffix and nothing else.
pub const CONTENT_PLAIN_TEXT: &str = "any mcp plain text canonicalization";

/// Exact plain-text source containing a character the importer must escape.
pub const CONTENT_PLAIN_ESCAPED: &str = "any_mcp plain text escape";

/// Exact non-canonical Markdown source used for lossy-rewrite evidence.
pub const CONTENT_NON_CANONICAL_MARKDOWN: &str =
    "Artifact heading\n================\n\n\nSecond paragraph.\n";

/// Closed inventory of observed canonicalization differences.
///
/// The categories describe what Anytype added, dropped, or rewrote between an
/// operator's source bytes and the stored canonical body, without retaining
/// either text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CanonicalizationEffect {
    /// The stored body is byte-identical to the source.
    Identical,
    /// The stored body gained the Anytype plain hard-break suffix.
    HardBreakSuffixAppended,
    /// Every source underscore is stored backslash-escaped.
    UnderscoreEscaped,
    /// Carriage returns present in the source are absent from the body.
    CarriageReturnDropped,
    /// A trailing newline was added.
    TrailingNewlineAdded,
    /// A trailing newline was dropped.
    TrailingNewlineDropped,
    /// A run of consecutive blank lines became shorter.
    BlankLinesCollapsed,
    /// The stored body has a different number of lines.
    LineCountChanged,
    /// The bodies differ in a way none of the closed categories explains.
    TextRewritten,
}

impl CanonicalizationEffect {
    /// Complete closed effect inventory.
    pub const ALL: [Self; 9] = [
        Self::Identical,
        Self::HardBreakSuffixAppended,
        Self::UnderscoreEscaped,
        Self::CarriageReturnDropped,
        Self::TrailingNewlineAdded,
        Self::TrailingNewlineDropped,
        Self::BlankLinesCollapsed,
        Self::LineCountChanged,
        Self::TextRewritten,
    ];

    /// Stable identifier used in evidence and failure reports.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Identical => "identical",
            Self::HardBreakSuffixAppended => "hard_break_suffix_appended",
            Self::UnderscoreEscaped => "underscore_escaped",
            Self::CarriageReturnDropped => "carriage_return_dropped",
            Self::TrailingNewlineAdded => "trailing_newline_added",
            Self::TrailingNewlineDropped => "trailing_newline_dropped",
            Self::BlankLinesCollapsed => "blank_lines_collapsed",
            Self::LineCountChanged => "line_count_changed",
            Self::TextRewritten => "text_rewritten",
        }
    }

    /// Parses an exact stable identifier.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|candidate| candidate.as_str() == value)
    }
}

/// Longest run of consecutive newlines, used to detect collapsed blank lines.
fn longest_newline_run(value: &str) -> usize {
    let mut longest = 0;
    let mut current = 0;
    for character in value.chars() {
        if character == '\n' {
            current += 1;
            longest = longest.max(current);
        } else {
            current = 0;
        }
    }
    longest
}

/// Classifies exactly how a stored canonical body differs from its source.
///
/// The result is a sorted, deduplicated, closed category list. It is a pure
/// function of the two texts, so every transport must reproduce it exactly.
#[must_use]
pub fn classify_canonicalization(source: &str, canonical: &str) -> Vec<&'static str> {
    if source == canonical {
        return vec![CanonicalizationEffect::Identical.as_str()];
    }
    let mut effects = Vec::new();
    if canonical.ends_with(ANYTYPE_PLAIN_MARKDOWN_SUFFIX)
        && !source.ends_with(ANYTYPE_PLAIN_MARKDOWN_SUFFIX)
    {
        effects.push(CanonicalizationEffect::HardBreakSuffixAppended);
    }
    let source_underscores = source.matches('_').count();
    if source_underscores > 0
        && !source.contains("\\_")
        && canonical.matches("\\_").count() == source_underscores
    {
        effects.push(CanonicalizationEffect::UnderscoreEscaped);
    }
    if source.contains('\r') && !canonical.contains('\r') {
        effects.push(CanonicalizationEffect::CarriageReturnDropped);
    }
    match (source.ends_with('\n'), canonical.ends_with('\n')) {
        (false, true) => effects.push(CanonicalizationEffect::TrailingNewlineAdded),
        (true, false) => effects.push(CanonicalizationEffect::TrailingNewlineDropped),
        _ => {}
    }
    // A blank line needs two consecutive newlines, so a shorter run only means
    // collapsed blank lines when the source actually had one.
    let source_run = longest_newline_run(source);
    if source_run >= 2 && longest_newline_run(canonical) < source_run {
        effects.push(CanonicalizationEffect::BlankLinesCollapsed);
    }
    if source.lines().count() != canonical.lines().count() {
        effects.push(CanonicalizationEffect::LineCountChanged);
    }
    if effects.is_empty() {
        effects.push(CanonicalizationEffect::TextRewritten);
    }
    effects.sort_unstable();
    effects.dedup();
    effects
        .into_iter()
        .map(CanonicalizationEffect::as_str)
        .collect()
}

/// Closed inventory of validator probes executed by a validator scenario.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ArtifactValidatorProbe {
    /// Declared MIME agrees with the detected MIME.
    MatchedDeclaration,
    /// Declared MIME is inside the validator scope but disagrees with the bytes.
    MismatchedDeclaration,
    /// Declared MIME is outside the validator scope, so no validator runs.
    OutOfScope,
}

impl ArtifactValidatorProbe {
    /// Complete closed validator probe inventory.
    pub const ALL: [Self; 3] = [
        Self::MatchedDeclaration,
        Self::MismatchedDeclaration,
        Self::OutOfScope,
    ];

    /// Stable identifier used in evidence and failure reports.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::MatchedDeclaration => "matched_declaration",
            Self::MismatchedDeclaration => "mismatched_declaration",
            Self::OutOfScope => "out_of_scope",
        }
    }

    /// Parses an exact stable identifier.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|candidate| candidate.as_str() == value)
    }

    /// Canonical MIME essence declared by this probe.
    #[must_use]
    pub const fn declared_media_type(self) -> &'static str {
        match self {
            // Both in-scope probes carry text bytes, so the mismatch is the
            // declaration rather than the payload.
            Self::MatchedDeclaration => "text/plain",
            Self::MismatchedDeclaration => "image/png",
            Self::OutOfScope => "application/octet-stream",
        }
    }

    /// Exact bytes imported by this probe.
    #[must_use]
    pub const fn payload(self) -> &'static [u8] {
        match self {
            Self::MatchedDeclaration => b"validator matched text payload\n",
            Self::MismatchedDeclaration => b"validator mismatched text payload\n",
            Self::OutOfScope => b"\x00\x01\x02validator-out-of-scope-bytes",
        }
    }

    /// MIME essence a real `file(1)` driver must detect for this payload.
    #[must_use]
    pub const fn detected_media_type(self) -> &'static str {
        "text/plain"
    }
}

/// Observed outcome of one validator probe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArtifactValidatorOutcome {
    /// The import succeeded and reported exactly one validator finding.
    Finding {
        /// Closed completion category reported by the validator.
        status: String,
        /// Detected MIME essence, when the validator produced one.
        detected_media_type: Option<String>,
    },
    /// The import succeeded and reported no validator finding at all.
    NoFindings,
    /// The import was refused with this stable domain error code.
    Refused {
        /// Stable machine-readable domain error code.
        code: String,
    },
}

/// Required outcome of one validator probe under one validator policy.
///
/// Platforms that do not activate validator execution admit the configuration
/// and report the validator unavailable, which a required validator still
/// treats as a refusal.
#[must_use]
pub fn validator_expectation(
    policy: FixtureValidatorPolicy,
    probe: ArtifactValidatorProbe,
) -> ArtifactValidatorOutcome {
    if !policy.is_declared() || probe == ArtifactValidatorProbe::OutOfScope {
        return ArtifactValidatorOutcome::NoFindings;
    }
    if !VALIDATOR_PLATFORM_ACTIVATES {
        return if policy.is_required() {
            ArtifactValidatorOutcome::Refused {
                code: VALIDATION_CODE.to_owned(),
            }
        } else {
            ArtifactValidatorOutcome::Finding {
                status: "unavailable".to_owned(),
                detected_media_type: None,
            }
        };
    }
    match probe {
        ArtifactValidatorProbe::MatchedDeclaration => ArtifactValidatorOutcome::Finding {
            status: "accepted".to_owned(),
            detected_media_type: Some(probe.detected_media_type().to_owned()),
        },
        ArtifactValidatorProbe::MismatchedDeclaration => {
            if policy.is_required() {
                ArtifactValidatorOutcome::Refused {
                    code: VALIDATION_CODE.to_owned(),
                }
            } else {
                ArtifactValidatorOutcome::Finding {
                    status: "rejected".to_owned(),
                    detected_media_type: Some(probe.detected_media_type().to_owned()),
                }
            }
        }
        ArtifactValidatorProbe::OutOfScope => ArtifactValidatorOutcome::NoFindings,
    }
}

/// Closed inventory of content functional scenarios.
///
/// The smoke matrix proves one happy path per transport; these scenarios prove
/// what the artifact *content* contract must do with representative MIME
/// types, with Markdown and plain text, and with configured validators.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ArtifactContentScenario {
    /// Representative MIME artifacts imported and exported through both planes.
    MimeMatrix,
    /// Markdown and plain-text create/update/export canonicalization evidence.
    DocumentCanonicalization,
    /// One optional validator: a rejection is reported and the import proceeds.
    ValidatorOptional,
    /// One required validator: a rejection refuses the import.
    ValidatorRequired,
}

impl ArtifactContentScenario {
    /// Complete closed content scenario inventory.
    pub const ALL: [Self; 4] = [
        Self::MimeMatrix,
        Self::DocumentCanonicalization,
        Self::ValidatorOptional,
        Self::ValidatorRequired,
    ];

    /// Stable identifier used in evidence and failure reports.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::MimeMatrix => "mime_matrix",
            Self::DocumentCanonicalization => "document_canonicalization",
            Self::ValidatorOptional => "validator_optional",
            Self::ValidatorRequired => "validator_required",
        }
    }

    /// Parses an exact stable identifier.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|candidate| candidate.as_str() == value)
    }

    /// Configured validator shape realized by this scenario.
    #[must_use]
    pub const fn validator_policy(self) -> FixtureValidatorPolicy {
        match self {
            Self::MimeMatrix | Self::DocumentCanonicalization => FixtureValidatorPolicy::Absent,
            Self::ValidatorOptional => FixtureValidatorPolicy::Optional,
            Self::ValidatorRequired => FixtureValidatorPolicy::Required,
        }
    }

    /// Exact fixture policy options that realize this scenario.
    #[must_use]
    pub const fn policy_options(self) -> ArtifactPolicyOptions {
        ArtifactPolicyOptions {
            staging: true,
            read_only: false,
            spaces: FixtureSpacePolicy::AllowedUnderTest,
            validators: self.validator_policy(),
            limits: ArtifactLimitProfile::Default,
        }
    }

    /// Exact configured and available validator counts reported by status.
    #[must_use]
    pub const fn expected_validator_counts(self) -> (u64, u64) {
        if !self.validator_policy().is_declared() {
            return (0, 0);
        }
        (1, if VALIDATOR_PLATFORM_ACTIVATES { 1 } else { 0 })
    }
}

/// Content-free record of one representative MIME artifact round trip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactFileRecord {
    /// `<fixture>+<data plane>` identifier of this round trip.
    pub case: String,
    /// Canonical MIME essence asserted at import.
    pub declared_media_type: String,
    /// Canonical MIME essence reported by the stored representation.
    pub stored_media_type: Option<String>,
    /// Verified imported byte length.
    pub size_bytes: u64,
    /// Verified SHA-256 of the imported bytes.
    pub sha256: String,
    /// Verified exported byte length.
    pub exported_size_bytes: u64,
    /// Verified SHA-256 of the exported bytes.
    pub exported_sha256: String,
}

/// Content-free record of one document mutation and its canonical readback.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactDocumentRecord {
    /// `<case>+<data plane>` identifier of this mutation.
    pub case: String,
    /// SHA-256 of the exact authorized source bytes.
    pub source_sha256: String,
    /// SHA-256 of the complete Markdown dispatched to Anytype.
    pub dispatched_sha256: String,
    /// SHA-256 of the complete canonical body read back from Anytype.
    pub canonical_sha256: String,
    /// Whether the mutation proved that no write was necessary.
    pub no_op: bool,
    /// Exact source byte count.
    pub source_bytes: u64,
    /// Exact canonical body byte count.
    pub canonical_bytes: u64,
    /// Closed categories describing what canonicalization changed.
    pub effects: Vec<&'static str>,
}

/// Content-free record of one validator probe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactValidatorRecord {
    /// `<probe>+<data plane>` identifier of this probe.
    pub case: String,
    /// Observed probe outcome.
    pub outcome: ArtifactValidatorOutcome,
}

/// Content-free result of one content scenario on one control plane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactContentEvidence {
    /// Stable content scenario identifier.
    pub scenario: &'static str,
    /// Stable control-plane identifier that produced this evidence.
    pub control: &'static str,
    /// Configured validator count reported by `artifact_status`.
    pub validator_count: u64,
    /// Available validator count reported by `artifact_status`.
    pub validator_available_count: u64,
    /// Ordered representative MIME round trips.
    pub files: Vec<ArtifactFileRecord>,
    /// Ordered document mutations and canonical readbacks.
    pub documents: Vec<ArtifactDocumentRecord>,
    /// Ordered validator probes.
    pub validators: Vec<ArtifactValidatorRecord>,
}

impl ArtifactContentEvidence {
    /// Whether two control planes observed identical content behavior.
    ///
    /// Only the control-plane identifier may differ.
    #[must_use]
    fn matches(&self, other: &Self) -> bool {
        self.scenario == other.scenario
            && self.validator_count == other.validator_count
            && self.validator_available_count == other.validator_available_count
            && self.files == other.files
            && self.documents == other.documents
            && self.validators == other.validators
    }
}

/// Fixture inputs for one content scenario run.
pub struct ArtifactContentRun<'a> {
    /// Content scenario under test.
    pub scenario: ArtifactContentScenario,
    /// Control plane driving the production server.
    pub control: ArtifactControlPlane,
    /// Strict operator policy backing the server under test.
    pub policy: &'a ArtifactPolicyFixture,
    /// Disposable space context that owns every created resource.
    pub ctx: &'a TestContext,
}

/// Runs one content scenario through one control plane and both data planes.
///
/// Every created Anytype resource is registered with the disposable context
/// before the next step, every staged record is consumed or released, and no
/// artifact byte, staging bearer, or upstream body is retained in the returned
/// evidence.
///
/// # Errors
///
/// Returns a fixed message describing the first violated expectation.
pub async fn run_artifact_content_scenario(
    driver: &mut impl McpDriver,
    run: &ArtifactContentRun<'_>,
) -> Result<ArtifactContentEvidence, String> {
    let scenario = run.scenario;
    if run.policy.options() != scenario.policy_options() {
        return Err(format!(
            "content fixture does not realize scenario: {}",
            scenario.as_str()
        ));
    }
    let suffix = unique_suffix();

    let descriptors = driver.list_tool_descriptors().await?;
    let catalog = ArtifactCatalogSnapshot::from_descriptors(&descriptors)?;
    ArtifactCatalogSnapshot::reviewed()?.compare(&catalog)?;

    let status = driver.call_tool("artifact_status", json!({})).await?;
    if required_u64(&status, "import_root_count")? != 1
        || required_u64(&status, "export_root_count")? != 1
        || status["staging_active"] != Value::Bool(true)
    {
        return Err("content scenario requires both planes to be active".to_owned());
    }
    let validator_count = required_u64(&status, "validator_count")?;
    let validator_available_count = required_u64(&status, "validator_available_count")?;
    if (validator_count, validator_available_count) != scenario.expected_validator_counts() {
        return Err(format!(
            "artifact status did not report the configured validators: {}",
            scenario.as_str()
        ));
    }

    let mut files = Vec::new();
    let mut documents = Vec::new();
    let mut validators = Vec::new();
    match scenario {
        ArtifactContentScenario::MimeMatrix => {
            files = Box::pin(run_mime_family(driver, run, &suffix)).await?;
        }
        ArtifactContentScenario::DocumentCanonicalization => {
            documents = Box::pin(run_document_family(driver, run, &suffix)).await?;
        }
        ArtifactContentScenario::ValidatorOptional | ArtifactContentScenario::ValidatorRequired => {
            validators = Box::pin(run_validator_family(driver, run, &suffix)).await?;
        }
    }

    Ok(ArtifactContentEvidence {
        scenario: scenario.as_str(),
        control: run.control.as_str(),
        validator_count,
        validator_available_count,
        files,
        documents,
        validators,
    })
}

/// Prepares one authorized source and returns any staged bearer it allocated.
async fn content_source(
    driver: &mut impl McpDriver,
    transport: ArtifactTransport,
    policy: &ArtifactPolicyFixture,
    space_id: &str,
    payload: &[u8],
    media_type: &str,
    relative: &str,
) -> Result<(Value, Option<String>), String> {
    match transport.data() {
        ArtifactDataPlane::LocalRoots => {
            policy.seed_import(relative, payload)?;
            Ok((
                json!({"local": {"root": ArtifactPolicyFixture::IMPORT_ROOT, "path": relative}}),
                None,
            ))
        }
        ArtifactDataPlane::RemoteStaging => {
            let handle = stage_upload(driver, space_id, payload, media_type).await?;
            Ok((json!({"staged_handle": handle.clone()}), Some(handle)))
        }
    }
}

/// Imports and exports every representative MIME artifact on both planes.
async fn run_mime_family(
    driver: &mut impl McpDriver,
    run: &ArtifactContentRun<'_>,
    suffix: &str,
) -> Result<Vec<ArtifactFileRecord>, String> {
    let space_id = run.ctx.space_id.as_str();
    let mut records =
        Vec::with_capacity(ArtifactMimeFixture::ALL.len() * ArtifactDataPlane::ALL.len());
    for data in ArtifactDataPlane::ALL {
        let transport = ArtifactTransport::new(run.control, data);
        for fixture in ArtifactMimeFixture::ALL {
            let case = format!("{}+{}", fixture.as_str(), data.as_str());
            let relative = format!(
                "mime-{}-{}-{suffix}.{}",
                fixture.as_str(),
                data.as_str(),
                fixture.extension()
            );
            let (source, _) = content_source(
                driver,
                transport,
                run.policy,
                space_id,
                fixture.payload(),
                fixture.media_type(),
                &relative,
            )
            .await?;
            let imported = driver
                .call_tool(
                    "file_import",
                    json!({
                        "space": space_id,
                        "source": source,
                        "name": relative,
                        "media_type": fixture.media_type(),
                        "idempotency_key": format!("content-import-{case}-{suffix}")
                    }),
                )
                .await?;
            let file_id = required_str(&imported, "/file_id")?;
            run.ctx.register_file(&file_id);
            let sha256 = required_str(&imported, "/receipt/sha256")?;
            let size_bytes = imported
                .pointer("/receipt/size_bytes")
                .and_then(Value::as_u64)
                .ok_or_else(|| format!("import {case} omitted its verified size"))?;
            let declared_media_type = required_str(&imported, "/receipt/declared_media_type")?;
            if sha256 != artifact_sha256(fixture.payload())
                || size_bytes != fixture.payload().len() as u64
                || declared_media_type != fixture.media_type()
            {
                return Err(format!("import {case} did not verify the exact fixture"));
            }

            let export_relative = format!(
                "mime-export-{}-{}-{suffix}.{}",
                fixture.as_str(),
                data.as_str(),
                fixture.extension()
            );
            let exported = driver
                .call_tool(
                    "file_export",
                    json!({
                        "space": space_id,
                        "file_id": file_id,
                        "destination": artifact_destination(&transport, &export_relative),
                        "idempotency_key": format!("content-export-{case}-{suffix}")
                    }),
                )
                .await?;
            let exported_bytes =
                read_exported_bytes(&transport, run.policy, &exported, &export_relative).await?;
            let exported_sha256 = required_str(&exported, "/receipt/sha256")?;
            let exported_size_bytes = exported
                .pointer("/receipt/size_bytes")
                .and_then(Value::as_u64)
                .ok_or_else(|| format!("export {case} omitted its verified size"))?;
            if exported_bytes != fixture.payload()
                || exported_sha256 != sha256
                || exported_size_bytes != size_bytes
            {
                return Err(format!("export {case} did not republish the exact bytes"));
            }
            release_remote_receipt(driver, &transport, &exported).await?;

            records.push(ArtifactFileRecord {
                case,
                declared_media_type,
                stored_media_type: imported
                    .pointer("/receipt/stored_media_type")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned),
                size_bytes,
                sha256,
                exported_size_bytes,
                exported_sha256,
            });
        }
    }
    Ok(records)
}

/// One document under test on one data plane.
///
/// Every mutation and readback of that document shares this identity, so each
/// step only names what actually varies: the source, the format, and a label.
struct DocumentPlane<'a> {
    run: &'a ArtifactContentRun<'a>,
    transport: ArtifactTransport,
    object_id: String,
    plane: &'static str,
    suffix: &'a str,
}

impl DocumentPlane<'_> {
    /// Exports the current body and returns its exact canonical text.
    async fn export_canonical(
        &self,
        driver: &mut impl McpDriver,
        expected_sha256: &str,
        label: &str,
    ) -> Result<String, String> {
        let relative = format!("document-{label}-{}-{}.md", self.plane, self.suffix);
        let exported = driver
            .call_tool(
                "document_export",
                json!({
                    "space": self.run.ctx.space_id.as_str(),
                    "object_id": self.object_id,
                    "destination": artifact_destination(&self.transport, &relative),
                    "expected_body_sha256": expected_sha256,
                    "idempotency_key": format!(
                        "content-document-export-{label}-{}-{}",
                        self.plane, self.suffix
                    )
                }),
            )
            .await?;
        if required_str(&exported, "/sha256")? != expected_sha256 {
            return Err("document export did not publish the expected canonical body".to_owned());
        }
        let bytes =
            read_exported_bytes(&self.transport, self.run.policy, &exported, &relative).await?;
        if artifact_sha256(&bytes) != expected_sha256 {
            return Err("document export bytes did not match the reported hash".to_owned());
        }
        release_remote_receipt(driver, &self.transport, &exported).await?;
        String::from_utf8(bytes).map_err(|_| "canonical document body is not UTF-8".to_owned())
    }

    /// Replaces the document body from an authorized source on this plane.
    async fn update(
        &self,
        driver: &mut impl McpDriver,
        expected_body_sha256: &str,
        source_text: &str,
        source_format: &'static str,
        label: &str,
    ) -> Result<Value, String> {
        let space_id = self.run.ctx.space_id.as_str();
        let (source, _) = content_source(
            driver,
            self.transport,
            self.run.policy,
            space_id,
            source_text.as_bytes(),
            ARTIFACT_MARKDOWN_MEDIA_TYPE,
            &format!("document-{label}-{}-{}.md", self.plane, self.suffix),
        )
        .await?;
        driver
            .call_tool(
                "document_import_update",
                json!({
                    "space": space_id,
                    "object_id": self.object_id,
                    "source": source,
                    "source_format": source_format,
                    "expected_body_sha256": expected_body_sha256,
                    "idempotency_key": format!(
                        "content-document-update-{label}-{}-{}",
                        self.plane, self.suffix
                    )
                }),
            )
            .await
    }
}

/// Builds one content-free document record from a mutation result.
fn document_record(
    case: String,
    source: &str,
    canonical: &str,
    result: &Value,
) -> Result<ArtifactDocumentRecord, String> {
    let source_sha256 = required_str(result, "/source_sha256")?;
    if source_sha256 != artifact_sha256(source.as_bytes()) {
        return Err(format!("document {case} did not verify its source bytes"));
    }
    let canonical_sha256 = required_str(result, "/canonical_sha256")?;
    if canonical_sha256 != artifact_sha256(canonical.as_bytes()) {
        return Err(format!("document {case} canonical readback disagreed"));
    }
    Ok(ArtifactDocumentRecord {
        case,
        source_sha256,
        dispatched_sha256: required_str(result, "/dispatched_sha256")?,
        canonical_sha256,
        no_op: result["no_op"] == Value::Bool(true),
        source_bytes: result
            .pointer("/source_bytes")
            .and_then(Value::as_u64)
            .ok_or_else(|| "document mutation omitted its source size".to_owned())?,
        canonical_bytes: canonical.len() as u64,
        effects: classify_canonicalization(source, canonical),
    })
}

/// Proves Markdown and plain-text create/update/export canonicalization.
///
/// The family covers three distinct obligations on each data plane: a create
/// whose canonical readback is exported exactly; a round trip of that exact
/// canonical body, which must be a proven no-op; and three non-canonical
/// sources whose stored bodies are classified into closed effect categories.
/// The plain-text case is additionally pinned to the statically known
/// canonical form, so the evidence names exactly what Anytype appended.
async fn run_document_family(
    driver: &mut impl McpDriver,
    run: &ArtifactContentRun<'_>,
    suffix: &str,
) -> Result<Vec<ArtifactDocumentRecord>, String> {
    let space_id = run.ctx.space_id.as_str();
    let mut records = Vec::new();
    for data in ArtifactDataPlane::ALL {
        let transport = ArtifactTransport::new(run.control, data);
        let plane = data.as_str();
        let (source, _) = content_source(
            driver,
            transport,
            run.policy,
            space_id,
            CONTENT_CREATE_MARKDOWN.as_bytes(),
            ARTIFACT_MARKDOWN_MEDIA_TYPE,
            &format!("document-create-{plane}-{suffix}.md"),
        )
        .await?;
        let created = driver
            .call_tool(
                "document_import_create",
                json!({
                    "space": space_id,
                    "source": source,
                    "source_format": "markdown",
                    "object_type": "page",
                    "name": format!("Artifact content {plane} {suffix}"),
                    "idempotency_key": format!("content-document-create-{plane}-{suffix}")
                }),
            )
            .await?;
        let object_id = required_str(&created, "/object_id")?;
        run.ctx.register_object(&object_id);
        let document = DocumentPlane {
            run,
            transport,
            object_id,
            plane,
            suffix,
        };
        let mut current = required_str(&created, "/canonical_sha256")?;
        let mut canonical =
            Box::pin(document.export_canonical(driver, &current, "created")).await?;
        records.push(document_record(
            format!("markdown_create+{plane}"),
            CONTENT_CREATE_MARKDOWN,
            &canonical,
            &created,
        )?);

        // A round trip of the exact canonical body must change nothing: this
        // is the explicit no-op obligation of the acceptance design.
        let no_op =
            Box::pin(document.update(driver, &current, &canonical, "markdown", "noop")).await?;
        if no_op["no_op"] != Value::Bool(true) {
            return Err("canonical round trip was not reported as a no-op".to_owned());
        }
        if required_str(&no_op, "/canonical_sha256")? != current {
            return Err("no-op round trip changed the canonical body".to_owned());
        }
        records.push(document_record(
            format!("canonical_no_op+{plane}"),
            &canonical,
            &canonical,
            &no_op,
        )?);

        // Plain text is the closed subset whose canonical form is known before
        // the call, so the lossy rewrite is asserted rather than only observed.
        let updated =
            Box::pin(document.update(driver, &current, CONTENT_PLAIN_TEXT, "plain_text", "plain"))
                .await?;
        let expected = format!("{CONTENT_PLAIN_TEXT}{ANYTYPE_PLAIN_MARKDOWN_SUFFIX}");
        current = required_str(&updated, "/canonical_sha256")?;
        if current != artifact_sha256(expected.as_bytes()) || updated["no_op"] != Value::Bool(false)
        {
            return Err("plain-text canonicalization did not match the modeled form".to_owned());
        }
        canonical = Box::pin(document.export_canonical(driver, &current, "plain")).await?;
        if canonical != expected {
            return Err("exported plain-text body was not the modeled canonical form".to_owned());
        }
        let record = document_record(
            format!("plain_text_hard_break+{plane}"),
            CONTENT_PLAIN_TEXT,
            &canonical,
            &updated,
        )?;
        if !record
            .effects
            .contains(&CanonicalizationEffect::HardBreakSuffixAppended.as_str())
        {
            return Err("plain-text canonicalization evidence lost its hard break".to_owned());
        }
        records.push(record);

        // An escaped plain-text source proves the dispatched form differs from
        // the operator's bytes before Anytype canonicalizes anything.
        let escaped = Box::pin(document.update(
            driver,
            &current,
            CONTENT_PLAIN_ESCAPED,
            "plain_text",
            "escaped",
        ))
        .await?;
        if required_str(&escaped, "/dispatched_sha256")?
            == artifact_sha256(CONTENT_PLAIN_ESCAPED.as_bytes())
        {
            return Err("plain-text escaping did not rewrite the dispatched body".to_owned());
        }
        current = required_str(&escaped, "/canonical_sha256")?;
        canonical = Box::pin(document.export_canonical(driver, &current, "escaped")).await?;
        records.push(document_record(
            format!("plain_text_escaped+{plane}"),
            CONTENT_PLAIN_ESCAPED,
            &canonical,
            &escaped,
        )?);

        let rewritten = Box::pin(document.update(
            driver,
            &current,
            CONTENT_NON_CANONICAL_MARKDOWN,
            "markdown",
            "rewritten",
        ))
        .await?;
        current = required_str(&rewritten, "/canonical_sha256")?;
        canonical = Box::pin(document.export_canonical(driver, &current, "rewritten")).await?;
        records.push(document_record(
            format!("markdown_non_canonical+{plane}"),
            CONTENT_NON_CANONICAL_MARKDOWN,
            &canonical,
            &rewritten,
        )?);
    }
    Ok(records)
}

/// Runs every validator probe on both data planes.
async fn run_validator_family(
    driver: &mut impl McpDriver,
    run: &ArtifactContentRun<'_>,
    suffix: &str,
) -> Result<Vec<ArtifactValidatorRecord>, String> {
    let space_id = run.ctx.space_id.as_str();
    let policy = run.scenario.validator_policy();
    let mut records =
        Vec::with_capacity(ArtifactValidatorProbe::ALL.len() * ArtifactDataPlane::ALL.len());
    for data in ArtifactDataPlane::ALL {
        let transport = ArtifactTransport::new(run.control, data);
        for probe in ArtifactValidatorProbe::ALL {
            let case = format!("{}+{}", probe.as_str(), data.as_str());
            eprintln!("artifact validator probe={case}");
            let relative = format!(
                "validator-{}-{}-{suffix}.bin",
                probe.as_str(),
                data.as_str()
            );
            let (source, staged) = content_source(
                driver,
                transport,
                run.policy,
                space_id,
                probe.payload(),
                probe.declared_media_type(),
                &relative,
            )
            .await?;
            let arguments = json!({
                "space": space_id,
                "source": source,
                "name": relative,
                "media_type": probe.declared_media_type(),
                "idempotency_key": format!("content-validator-{case}-{suffix}")
            });
            let expected = validator_expectation(policy, probe);
            let outcome = match expected {
                ArtifactValidatorOutcome::Refused { .. } => {
                    let refusal = driver.call_tool_error("file_import", arguments).await?;
                    // A refused import never consumes its staged source, so the
                    // record is released instead of outliving the probe.
                    if let Some(handle) = staged {
                        let released = driver
                            .call_tool("artifact_release", json!({"handle": handle}))
                            .await?;
                        if released["released"] != Value::Bool(true) {
                            return Err(format!("probe {case} left a staged record allocated"));
                        }
                    }
                    ArtifactValidatorOutcome::Refused {
                        code: refusal.code().to_owned(),
                    }
                }
                ArtifactValidatorOutcome::Finding { .. } | ArtifactValidatorOutcome::NoFindings => {
                    let imported = driver.call_tool("file_import", arguments).await?;
                    run.ctx.register_file(&required_str(&imported, "/file_id")?);
                    validator_outcome(&imported, &case)?
                }
            };
            if outcome != expected {
                return Err(format!(
                    "validator probe {case} produced an unexpected outcome"
                ));
            }
            records.push(ArtifactValidatorRecord { case, outcome });
        }
    }
    Ok(records)
}

/// Reads the single expected validator finding from an import receipt.
fn validator_outcome(imported: &Value, case: &str) -> Result<ArtifactValidatorOutcome, String> {
    let Some(findings) = imported.pointer("/receipt/validators") else {
        return Ok(ArtifactValidatorOutcome::NoFindings);
    };
    let findings = findings
        .as_array()
        .ok_or_else(|| format!("probe {case} reported a malformed validator array"))?;
    match findings.as_slice() {
        [] => Ok(ArtifactValidatorOutcome::NoFindings),
        [finding] => {
            if finding["id"] != Value::String(FIXTURE_VALIDATOR_ID.to_owned()) {
                return Err(format!("probe {case} reported an unconfigured validator"));
            }
            Ok(ArtifactValidatorOutcome::Finding {
                status: finding["status"]
                    .as_str()
                    .ok_or_else(|| format!("probe {case} omitted its validator status"))?
                    .to_owned(),
                detected_media_type: finding["detected_media_type"]
                    .as_str()
                    .map(ToOwned::to_owned),
            })
        }
        _ => Err(format!("probe {case} reported more than one finding")),
    }
}

/// Proves that every control plane observed the same content behavior.
///
/// # Errors
///
/// Returns a fixed message naming the first divergent control plane, or
/// reporting an incomplete, misordered, or duplicated executed matrix.
pub fn assert_artifact_content_parity(
    executed: &[ArtifactContentEvidence],
    expected: &[ArtifactControlPlane],
) -> Result<(), String> {
    if executed.len() != expected.len() {
        return Err("executed artifact content matrix is incomplete".to_owned());
    }
    let mut observed = Vec::with_capacity(executed.len());
    for (evidence, control) in executed.iter().zip(expected) {
        if evidence.control != control.as_str() {
            return Err(format!(
                "artifact content evidence out of order: {}",
                evidence.control
            ));
        }
        if observed.contains(&evidence.control) {
            return Err(format!(
                "duplicate artifact content evidence: {}",
                evidence.control
            ));
        }
        observed.push(evidence.control);
    }
    let Some(baseline) = executed.first() else {
        return Err("artifact content parity requires at least one control plane".to_owned());
    };
    for evidence in executed.iter().skip(1) {
        if !baseline.matches(evidence) {
            return Err(format!(
                "artifact content control plane diverged from {}: {}",
                baseline.control, evidence.control
            ));
        }
    }
    Ok(())
}

/// Closed inventory of lifecycle and payload acceptance scenarios.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ArtifactLifecycleScenario {
    /// Aggregate byte and record quotas refuse further reservations.
    Quota,
    /// Expired records are reaped and their handles become uniformly stale.
    TtlCleanup,
    /// Concurrent create-new publication produces one winner and one conflict.
    Collision,
    /// A cancelled pre-dispatch import leaves its staged source reusable.
    Cancellation,
    /// A restarted child invalidates old handles and reconciles private state.
    RestartStaleGeneration,
    /// Small and ceiling-sized payloads produce bounded MCP frames.
    PayloadCeiling,
}

impl ArtifactLifecycleScenario {
    /// Complete closed lifecycle scenario inventory.
    pub const ALL: [Self; 6] = [
        Self::Quota,
        Self::TtlCleanup,
        Self::Collision,
        Self::Cancellation,
        Self::RestartStaleGeneration,
        Self::PayloadCeiling,
    ];

    /// Stable identifier used in evidence and failure reports.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Quota => "quota",
            Self::TtlCleanup => "ttl_cleanup",
            Self::Collision => "collision",
            Self::Cancellation => "cancellation",
            Self::RestartStaleGeneration => "restart_stale_generation",
            Self::PayloadCeiling => "payload_ceiling",
        }
    }

    /// Parses an exact stable identifier.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|candidate| candidate.as_str() == value)
    }

    /// Strict policy options realizing this scenario.
    #[must_use]
    pub const fn policy_options(self) -> ArtifactPolicyOptions {
        let limits = match self {
            Self::Quota => ArtifactLimitProfile::Quota,
            Self::TtlCleanup => ArtifactLimitProfile::TtlCleanup,
            Self::Cancellation | Self::PayloadCeiling => ArtifactLimitProfile::PayloadCeiling,
            Self::Collision | Self::RestartStaleGeneration => ArtifactLimitProfile::Default,
        };
        ArtifactPolicyOptions {
            staging: true,
            read_only: false,
            spaces: FixtureSpacePolicy::AllowedUnderTest,
            validators: FixtureValidatorPolicy::Absent,
            limits,
        }
    }
}

/// Sensitive staging capability retained only inside one acceptance scenario.
///
/// Callers must never log or return this value as content-free evidence.
pub struct ArtifactStageAllocation {
    handle: Zeroizing<String>,
    record: String,
    url: String,
    size_bytes: u64,
}

impl fmt::Debug for ArtifactStageAllocation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ArtifactStageAllocation")
            .field("size_bytes", &self.size_bytes)
            .finish_non_exhaustive()
    }
}

impl ArtifactStageAllocation {
    /// Bearer used only in the staging authorization header.
    #[must_use]
    pub fn handle(&self) -> &str {
        self.handle.as_str()
    }

    /// Non-secret record identifier present in the staging URL.
    #[must_use]
    pub fn record(&self) -> &str {
        &self.record
    }

    /// Opaque staging URL, which must not contain the bearer.
    #[must_use]
    pub fn url(&self) -> &str {
        &self.url
    }

    /// Exact reserved byte length.
    #[must_use]
    pub const fn size_bytes(&self) -> u64 {
        self.size_bytes
    }
}

/// Content-free measurement of one complete MCP response frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactFrameMeasurement {
    /// Complete structured tool result after envelope validation.
    pub structured_content: Value,
    /// Exact serialized JSON frame bytes.
    pub frame_bytes: u64,
    /// Exact cl100k token count of the serialized frame.
    pub frame_tokens: u64,
}

/// Fixed maximum serialized response bytes for measured artifact calls.
pub const ARTIFACT_FRAME_CEILING_BYTES: u64 = 16 * 1024;
/// Fixed maximum cl100k tokens for measured artifact calls.
pub const ARTIFACT_FRAME_CEILING_TOKENS: u64 = 4_096;
/// Maximum frame-byte variation allowed between small and ceiling payloads.
pub const ARTIFACT_PAYLOAD_FRAME_DELTA_BYTES: u64 = 128;
/// Maximum token variation allowed between small and ceiling payloads.
pub const ARTIFACT_PAYLOAD_FRAME_DELTA_TOKENS: u64 = 32;

/// Measures and validates one raw artifact `tools/call` response frame.
///
/// # Errors
///
/// Returns a fixed message when the frame is malformed, exceeds either fixed
/// ceiling, or cannot be tokenized.
pub fn measure_artifact_frame(
    name: &str,
    id: u64,
    frame: &[u8],
) -> Result<ArtifactFrameMeasurement, String> {
    if frame.last() != Some(&b'\n')
        || frame.first() == Some(&b'\n')
        || frame[..frame.len().saturating_sub(1)].contains(&b'\n')
    {
        return Err("measured artifact response was not one LF-delimited frame".to_owned());
    }
    let frame_text = std::str::from_utf8(frame)
        .map_err(|_| "measured artifact response was not UTF-8".to_owned())?;
    let parsed: Value = serde_json::from_slice(&frame[..frame.len().saturating_sub(1)])
        .map_err(|_| "measured artifact response was not JSON".to_owned())?;
    let tokenizer =
        tiktoken_rs::cl100k_base().map_err(|_| "initialize artifact frame tokenizer".to_owned())?;
    let tokens = tokenizer.encode_with_special_tokens(frame_text);
    let frame_bytes = u64::try_from(frame.len())
        .map_err(|_| "measured artifact frame exceeds the addressable range".to_owned())?;
    let frame_tokens = u64::try_from(tokens.len())
        .map_err(|_| "measured artifact token count exceeds the addressable range".to_owned())?;
    if frame_bytes > ARTIFACT_FRAME_CEILING_BYTES || frame_tokens > ARTIFACT_FRAME_CEILING_TOKENS {
        return Err("artifact response exceeded its fixed MCP frame ceiling".to_owned());
    }
    Ok(ArtifactFrameMeasurement {
        structured_content: validate_tool_frame(name, id, &parsed)?,
        frame_bytes,
        frame_tokens,
    })
}

/// Proves that payload-size changes do not materially expand MCP responses.
///
/// # Errors
///
/// Returns a fixed message if either measured difference exceeds its reviewed
/// fixed allowance.
pub fn assert_payload_frame_independence(
    small: &ArtifactFrameMeasurement,
    large: &ArtifactFrameMeasurement,
) -> Result<(), String> {
    if small.frame_bytes.abs_diff(large.frame_bytes) > ARTIFACT_PAYLOAD_FRAME_DELTA_BYTES
        || small.frame_tokens.abs_diff(large.frame_tokens) > ARTIFACT_PAYLOAD_FRAME_DELTA_TOKENS
    {
        return Err("artifact MCP frame size varied with payload bytes".to_owned());
    }
    Ok(())
}

/// Reads and validates the exact reviewed artifact catalog from one driver.
///
/// # Errors
///
/// Returns a fixed message when the advertised catalog is incomplete or has
/// drifted from the reviewed fixture.
pub async fn artifact_catalog_snapshot(
    driver: &mut impl McpDriver,
) -> Result<ArtifactCatalogSnapshot, String> {
    let descriptors = driver.list_tool_descriptors().await?;
    let snapshot = ArtifactCatalogSnapshot::from_descriptors(&descriptors)?;
    ArtifactCatalogSnapshot::reviewed()?.compare(&snapshot)?;
    Ok(snapshot)
}

/// Allocates one exact remote upload reservation without sending payload bytes.
///
/// # Errors
///
/// Returns a fixed message when the production tool omits or confuses one
/// capability field.
pub async fn allocate_stage_upload(
    driver: &mut impl McpDriver,
    space_id: &str,
    size_bytes: u64,
    media_type: &str,
    expected_sha256: Option<&str>,
) -> Result<ArtifactStageAllocation, String> {
    let mut arguments = json!({
        "space": space_id,
        "size_bytes": size_bytes,
        "media_type": media_type,
    });
    if let Some(expected) = expected_sha256 {
        arguments
            .as_object_mut()
            .ok_or_else(|| "stage allocation arguments are not an object".to_owned())?
            .insert(
                "expected_sha256".to_owned(),
                Value::String(expected.to_owned()),
            );
    }
    let mut allocation = driver.call_tool("artifact_stage_upload", arguments).await?;
    let handle = Zeroizing::new(take_required_string(&mut allocation, "/handle")?);
    let record = required_str(&allocation, "/record")?;
    let url = required_str(&allocation, "/upload_url")?;
    if !url.contains(&record) || url.contains(handle.as_str()) {
        return Err("staging URL must expose only the non-secret record".to_owned());
    }
    let observed_size = allocation["size_bytes"]
        .as_u64()
        .ok_or_else(|| "stage allocation omitted its exact size".to_owned())?;
    if observed_size != size_bytes {
        return Err("stage allocation changed its reserved size".to_owned());
    }
    zeroize_json_strings(&mut allocation);
    Ok(ArtifactStageAllocation {
        handle,
        record,
        url,
        size_bytes,
    })
}

/// Exact transfer range used by acceptance uploads.
///
/// Every non-default lifecycle profile configures this production minimum, so
/// large fixtures must cross the real sequential-range boundary.
pub const ACCEPTANCE_TRANSFER_CHUNK_BYTES: usize = 65_536;

/// Uploads exact bytes into one previously allocated staging record.
///
/// # Errors
///
/// Returns a fixed message when lengths disagree, an offset is not acknowledged,
/// or the production HTTP route rejects a bounded sequential range.
pub async fn upload_stage_bytes(
    allocation: &ArtifactStageAllocation,
    payload: &[u8],
    media_type: &str,
) -> Result<(), String> {
    let size = u64::try_from(payload.len())
        .map_err(|_| "staged payload exceeds the addressable range".to_owned())?;
    if size == 0 || size != allocation.size_bytes {
        return Err("staged upload bytes disagree with the reservation".to_owned());
    }
    let client = staging_client()?;
    let mut offset = 0_u64;
    for chunk in payload.chunks(ACCEPTANCE_TRANSFER_CHUNK_BYTES) {
        let chunk_length = u64::try_from(chunk.len())
            .map_err(|_| "staged upload range exceeds the addressable range".to_owned())?;
        let next_offset = offset
            .checked_add(chunk_length)
            .ok_or_else(|| "staged upload offset overflow".to_owned())?;
        let last = next_offset
            .checked_sub(1)
            .ok_or_else(|| "staged payload must not be empty".to_owned())?;
        let response = client
            .put(allocation.url())
            .bearer_auth(allocation.handle())
            .header(CONTENT_TYPE, media_type)
            .header(CONTENT_RANGE, format!("bytes {offset}-{last}/{size}"))
            .body(chunk.to_vec())
            .send()
            .await
            .map_err(|_| "staged upload transport failed".to_owned())?;
        let expected_status = if next_offset == size {
            reqwest::StatusCode::CREATED
        } else {
            reqwest::StatusCode::NO_CONTENT
        };
        if response.status() != expected_status
            || response
                .headers()
                .get("upload-offset")
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.parse::<u64>().ok())
                != Some(next_offset)
        {
            return Err("staged upload range was not acknowledged exactly".to_owned());
        }
        offset = next_offset;
    }
    if offset == size {
        Ok(())
    } else {
        Err("staged upload did not send its complete reservation".to_owned())
    }
}

/// Proves the production staging route rejects one range above the transfer ceiling.
///
/// # Errors
///
/// Returns a fixed message when the reservation is too small for the probe or
/// the oversized range is not rejected before any offset is committed.
pub async fn reject_oversized_stage_chunk(
    allocation: &ArtifactStageAllocation,
    payload: &[u8],
    media_type: &str,
) -> Result<(), String> {
    let probe_length = ACCEPTANCE_TRANSFER_CHUNK_BYTES.saturating_add(1);
    if payload.len() < probe_length || allocation.size_bytes != payload.len() as u64 {
        return Err("oversized staging-range probe requires a larger exact reservation".to_owned());
    }
    let last = probe_length.saturating_sub(1);
    let response = staging_client()?
        .put(allocation.url())
        .bearer_auth(allocation.handle())
        .header(CONTENT_TYPE, media_type)
        .header(
            CONTENT_RANGE,
            format!("bytes 0-{last}/{}", allocation.size_bytes),
        )
        .body(payload[..probe_length].to_vec())
        .send()
        .await
        .map_err(|_| "oversized staged upload probe failed".to_owned())?;
    if response.status() == reqwest::StatusCode::PAYLOAD_TOO_LARGE {
        Ok(())
    } else {
        Err("staging route did not enforce the transfer chunk ceiling".to_owned())
    }
}

/// Releases one exact staging allocation through the production MCP tool.
///
/// # Errors
///
/// Returns a fixed message when the release is not definitive.
pub async fn release_stage_upload(
    driver: &mut impl McpDriver,
    allocation: &ArtifactStageAllocation,
) -> Result<(), String> {
    let released = driver
        .call_tool("artifact_release", json!({"handle": allocation.handle()}))
        .await?;
    if released["released"] == Value::Bool(true) {
        Ok(())
    } else {
        Err("staging allocation was not released".to_owned())
    }
}

/// Returns the staging HTTP status for one authenticated record without a body.
///
/// # Errors
///
/// Returns a fixed message when the bounded HTTP request cannot complete.
pub async fn stage_head_status(
    allocation: &ArtifactStageAllocation,
) -> Result<reqwest::StatusCode, String> {
    staging_client()?
        .head(allocation.url())
        .bearer_auth(allocation.handle())
        .send()
        .await
        .map(|response| response.status())
        .map_err(|_| "staged status transport failed".to_owned())
}

/// Waits until an expired record is both inaccessible and physically reaped.
///
/// # Errors
///
/// Returns a fixed timeout or fixture-inspection message. The returned snapshot
/// contains counts only and cannot expose a record identity or bearer.
pub async fn wait_for_stage_reaped(
    policy: &ArtifactPolicyFixture,
    allocation: &ArtifactStageAllocation,
    timeout: Duration,
) -> Result<ArtifactDirectorySnapshot, String> {
    let deadline = Instant::now()
        .checked_add(timeout)
        .ok_or_else(|| "artifact cleanup deadline overflow".to_owned())?;
    loop {
        let status = stage_head_status(allocation).await?;
        let snapshot = policy.staging_snapshot()?;
        if status == reqwest::StatusCode::NOT_FOUND && snapshot.is_reaped() {
            return Ok(snapshot);
        }
        if Instant::now() >= deadline {
            return Err("artifact TTL cleanup did not finish before its deadline".to_owned());
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

/// Classifies two concurrent tool frames as exactly one success and one conflict.
///
/// # Errors
///
/// Returns a fixed message when either envelope is malformed or the race does
/// not produce the single-winner create-new contract.
pub fn classify_collision_frames(
    name: &str,
    ids: [u64; 2],
    frames: &[Value; 2],
    preview: bool,
) -> Result<Value, String> {
    let mut success = None;
    let mut conflicts = 0_u8;
    for (id, frame) in ids.into_iter().zip(frames) {
        if frame["jsonrpc"] != Value::String("2.0".to_owned())
            || frame["id"].as_u64() != Some(id)
            || frame.get("error").is_some()
        {
            return Err("collision response carried an invalid JSON-RPC envelope".to_owned());
        }
        let result = frame
            .get("result")
            .ok_or_else(|| "collision response omitted its result".to_owned())?;
        if result["isError"] == Value::Bool(false) {
            if success
                .replace(validate_tool_frame(name, id, frame)?)
                .is_some()
            {
                return Err("artifact collision produced more than one winner".to_owned());
            }
        } else {
            let refusal = ToolErrorEvidence::from_result(result, preview)?;
            if refusal.code() != "conflict" {
                return Err("artifact collision loser was not a conflict".to_owned());
            }
            conflicts = conflicts.saturating_add(1);
        }
    }
    match (success, conflicts) {
        (Some(winner), 1) => Ok(winner),
        _ => Err("artifact collision did not produce one winner and one conflict".to_owned()),
    }
}

/// Fixed upstream server-log error classes already isolated and tracked.
pub const KNOWN_SERVER_LOG_CLASSES: [(&str, &str); 5] = [
    (
        "deleted_space_sync_status",
        "failed to update details failed to load space",
    ),
    (
        "filesync_pending_upload",
        "process next pending upload item",
    ),
    ("headsync_peer", "can't sync with peer"),
    ("object_cache_closed", "object cache is closed"),
    ("space_storage_sqlite", "SQLITE_ERROR"),
];

/// Content-free audit of a captured Anytype server log window.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ArtifactServerLogAudit {
    /// Lines inspected in the bounded window.
    pub inspected_lines: u64,
    /// Lines reporting a panic or a fatal condition.
    pub panic_or_fatal_lines: u64,
    /// Counts of already isolated upstream error classes.
    pub known_classes: BTreeMap<&'static str, u64>,
    /// Error lines matching no known class.
    pub unclassified_error_lines: u64,
    /// Submitted secrets or fixture paths observed in the audited bytes.
    pub forbidden_needle_matches: u64,
    /// Lines that exceeded the fixed audit ceiling.
    pub oversized_lines: u64,
}

impl ArtifactServerLogAudit {
    /// Whether the window contains no panic, fatal, or new error class.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.panic_or_fatal_lines == 0
            && self.unclassified_error_lines == 0
            && self.forbidden_needle_matches == 0
            && self.oversized_lines == 0
    }
}

const SERVER_LOG_WINDOW_BYTES: u64 = 8 * 1024 * 1024;
const SERVER_LOG_LINE_BYTES: usize = 64 * 1024;
const SERVER_LOG_NEEDLE_BYTES: usize = 4 * 1024;
const SERVER_LOG_ANCHOR_BYTES: usize = 256;

/// Retained opened capability and immutable baseline for one server-log audit.
pub struct ArtifactServerLogBaseline {
    file: File,
    #[cfg(any(unix, windows))]
    parent: cap_std::fs::Dir,
    #[cfg(any(unix, windows))]
    name: OsString,
    #[cfg(any(unix, windows))]
    device: u64,
    #[cfg(any(unix, windows))]
    inode: u64,
    size_bytes: u64,
    anchor_digest: String,
}

impl fmt::Debug for ArtifactServerLogBaseline {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ArtifactServerLogBaseline")
            .field("size_bytes", &self.size_bytes)
            .finish_non_exhaustive()
    }
}

/// Opens and validates a captured server log before an acceptance operation.
///
/// # Errors
///
/// Returns a fixed category when the path cannot be opened as an owner-private
/// regular file. The retained descriptor prevents a later path substitution.
#[cfg(unix)]
pub fn server_log_baseline(path: &Path) -> Result<ArtifactServerLogBaseline, String> {
    use std::{
        io::{Seek, SeekFrom},
        os::unix::fs::{MetadataExt, OpenOptionsExt},
    };

    let parent_path = path
        .parent()
        .ok_or_else(|| "open captured server log capability".to_owned())?;
    let name = path
        .file_name()
        .ok_or_else(|| "open captured server log capability".to_owned())?
        .to_owned();
    let mut parent_options = OpenOptions::new();
    parent_options
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_DIRECTORY | libc::O_NOFOLLOW);
    let parent_file = parent_options
        .open(parent_path)
        .map_err(|_| "open captured server log capability".to_owned())?;
    let parent = cap_std::fs::Dir::from_std_file(parent_file);
    let mut options = cap_std::fs::OpenOptions::new();
    options.read(true).follow(cap_fs_ext::FollowSymlinks::No);
    let mut file = parent
        .open_with(Path::new(&name), &options)
        .map_err(|_| "open captured server log capability".to_owned())?
        .into_std();
    let metadata = file
        .metadata()
        .map_err(|_| "inspect captured server log capability".to_owned())?;
    if !metadata.file_type().is_file()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o777 != 0o600
    {
        return Err("captured server log is not an owner-private regular file".to_owned());
    }
    let size_bytes = metadata.len();
    let anchor_start = size_bytes.saturating_sub(SERVER_LOG_ANCHOR_BYTES as u64);
    file.seek(SeekFrom::Start(anchor_start))
        .map_err(|_| "inspect captured server log capability".to_owned())?;
    let mut anchor = Vec::new();
    (&mut file)
        .take(SERVER_LOG_ANCHOR_BYTES as u64)
        .read_to_end(&mut anchor)
        .map_err(|_| "inspect captured server log capability".to_owned())?;
    Ok(ArtifactServerLogBaseline {
        file,
        parent,
        name,
        device: metadata.dev(),
        inode: metadata.ino(),
        size_bytes,
        anchor_digest: hex_digest(&Sha256::digest(anchor)),
    })
}

/// Opens the Windows log through retained no-reparse parent and file handles.
#[cfg(windows)]
pub fn server_log_baseline(path: &Path) -> Result<ArtifactServerLogBaseline, String> {
    use std::{
        io::{Seek, SeekFrom},
        os::windows::fs::{MetadataExt, OpenOptionsExt},
    };

    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
    let parent_path = path
        .parent()
        .ok_or_else(|| "open captured server log capability".to_owned())?;
    let name = path
        .file_name()
        .ok_or_else(|| "open captured server log capability".to_owned())?
        .to_owned();
    let mut parent_options = OpenOptions::new();
    parent_options
        .read(true)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT);
    let parent_file = parent_options
        .open(parent_path)
        .map_err(|_| "open captured server log capability".to_owned())?;
    let parent_metadata = parent_file
        .metadata()
        .map_err(|_| "inspect captured server log capability".to_owned())?;
    if !parent_metadata.is_dir()
        || parent_metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    {
        return Err("captured server log parent is not a retained directory".to_owned());
    }
    let parent = cap_std::fs::Dir::from_std_file(parent_file);
    let mut options = cap_std::fs::OpenOptions::new();
    options.read(true).follow(cap_fs_ext::FollowSymlinks::No);
    let mut file = parent
        .open_with(Path::new(&name), &options)
        .map_err(|_| "open captured server log capability".to_owned())?
        .into_std();
    let metadata = file
        .metadata()
        .map_err(|_| "inspect captured server log capability".to_owned())?;
    let device = metadata
        .volume_serial_number()
        .map(u64::from)
        .ok_or_else(|| "inspect captured server log capability".to_owned())?;
    let inode = metadata
        .file_index()
        .ok_or_else(|| "inspect captured server log capability".to_owned())?;
    if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
        || !acceptance_owner_private_file(&file)
    {
        return Err("captured server log is not an owner-private regular file".to_owned());
    }
    let size_bytes = metadata.len();
    let anchor_start = size_bytes.saturating_sub(SERVER_LOG_ANCHOR_BYTES as u64);
    file.seek(SeekFrom::Start(anchor_start))
        .map_err(|_| "inspect captured server log capability".to_owned())?;
    let mut anchor = Vec::new();
    (&mut file)
        .take(SERVER_LOG_ANCHOR_BYTES as u64)
        .read_to_end(&mut anchor)
        .map_err(|_| "inspect captured server log capability".to_owned())?;
    Ok(ArtifactServerLogBaseline {
        file,
        parent,
        name,
        device,
        inode,
        size_bytes,
        anchor_digest: hex_digest(&Sha256::digest(anchor)),
    })
}

#[cfg(unix)]
fn assert_server_log_namespace_current(baseline: &ArtifactServerLogBaseline) -> Result<(), String> {
    use std::os::unix::fs::MetadataExt;

    let mut options = cap_std::fs::OpenOptions::new();
    options.read(true).follow(cap_fs_ext::FollowSymlinks::No);
    let current = baseline
        .parent
        .open_with(Path::new(&baseline.name), &options)
        .map_err(|_| "captured server log namespace changed".to_owned())?
        .into_std();
    let metadata = current
        .metadata()
        .map_err(|_| "captured server log namespace changed".to_owned())?;
    if !metadata.file_type().is_file()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o777 != 0o600
        || metadata.dev() != baseline.device
        || metadata.ino() != baseline.inode
    {
        return Err("captured server log namespace changed".to_owned());
    }
    Ok(())
}

#[cfg(windows)]
fn assert_server_log_namespace_current(baseline: &ArtifactServerLogBaseline) -> Result<(), String> {
    use std::os::windows::fs::MetadataExt;

    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
    let mut options = cap_std::fs::OpenOptions::new();
    options.read(true).follow(cap_fs_ext::FollowSymlinks::No);
    let current = baseline
        .parent
        .open_with(Path::new(&baseline.name), &options)
        .map_err(|_| "captured server log namespace changed".to_owned())?
        .into_std();
    let metadata = current
        .metadata()
        .map_err(|_| "captured server log namespace changed".to_owned())?;
    let device = metadata
        .volume_serial_number()
        .map(u64::from)
        .ok_or_else(|| "captured server log namespace changed".to_owned())?;
    let inode = metadata
        .file_index()
        .ok_or_else(|| "captured server log namespace changed".to_owned())?;
    if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
        || !acceptance_owner_private_file(&current)
        || device != baseline.device
        || inode != baseline.inode
    {
        return Err("captured server log namespace changed".to_owned());
    }
    Ok(())
}

#[cfg(unix)]
fn assert_server_log_descriptor_current(
    baseline: &ArtifactServerLogBaseline,
    file: &File,
) -> Result<u64, String> {
    use std::os::unix::fs::MetadataExt;

    let metadata = file
        .metadata()
        .map_err(|_| "inspect captured server log capability".to_owned())?;
    if !metadata.file_type().is_file()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o777 != 0o600
        || metadata.dev() != baseline.device
        || metadata.ino() != baseline.inode
        || metadata.len() < baseline.size_bytes
    {
        return Err("captured server log capability changed".to_owned());
    }
    Ok(metadata.len())
}

#[cfg(windows)]
fn assert_server_log_descriptor_current(
    baseline: &ArtifactServerLogBaseline,
    file: &File,
) -> Result<u64, String> {
    use std::os::windows::fs::MetadataExt;

    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
    let metadata = file
        .metadata()
        .map_err(|_| "inspect captured server log capability".to_owned())?;
    let device = metadata
        .volume_serial_number()
        .map(u64::from)
        .ok_or_else(|| "inspect captured server log capability".to_owned())?;
    let inode = metadata
        .file_index()
        .ok_or_else(|| "inspect captured server log capability".to_owned())?;
    if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
        || !acceptance_owner_private_file(file)
        || device != baseline.device
        || inode != baseline.inode
        || metadata.len() < baseline.size_bytes
    {
        return Err("captured server log capability changed".to_owned());
    }
    Ok(metadata.len())
}

/// Refuses server-log audits on platforms without the required no-follow file
/// descriptor semantics.
#[cfg(not(any(unix, windows)))]
pub fn server_log_baseline(_path: &Path) -> Result<ArtifactServerLogBaseline, String> {
    Err("captured server log capability is unsupported on this platform".to_owned())
}

/// Audits bytes appended to one retained server-log capability.
///
/// The caller supplies the fixture paths, transient staging capabilities, and
/// child credentials that must never appear in the log. The audit retains only
/// counters and fixed category names.
///
/// # Errors
///
/// Returns a fixed category when the retained file changed identity, shrank,
/// rotated, exceeded the window limit, or cannot be read.
#[cfg(any(unix, windows))]
pub fn audit_server_log(
    baseline: &ArtifactServerLogBaseline,
    forbidden_needles: &[&[u8]],
) -> Result<ArtifactServerLogAudit, String> {
    audit_server_log_with_snapshot_hook(baseline, forbidden_needles, || {})
}

#[cfg(any(unix, windows))]
fn audit_server_log_with_snapshot_hook(
    baseline: &ArtifactServerLogBaseline,
    forbidden_needles: &[&[u8]],
    after_snapshot: impl FnOnce(),
) -> Result<ArtifactServerLogAudit, String> {
    use std::io::{Read, Seek, SeekFrom};

    if forbidden_needles
        .iter()
        .any(|needle| needle.is_empty() || needle.len() > SERVER_LOG_NEEDLE_BYTES)
    {
        return Err("captured server log needle was outside the audit limit".to_owned());
    }
    assert_server_log_namespace_current(baseline)?;
    let mut file = baseline
        .file
        .try_clone()
        .map_err(|_| "read captured server log capability".to_owned())?;
    let snapshot_size = assert_server_log_descriptor_current(baseline, &file)?;
    let anchor_start = baseline
        .size_bytes
        .saturating_sub(SERVER_LOG_ANCHOR_BYTES as u64);
    file.seek(SeekFrom::Start(anchor_start))
        .map_err(|_| "read captured server log capability".to_owned())?;
    let mut anchor = Vec::new();
    (&mut file)
        .take(baseline.size_bytes - anchor_start)
        .read_to_end(&mut anchor)
        .map_err(|_| "read captured server log capability".to_owned())?;
    if hex_digest(&Sha256::digest(anchor)) != baseline.anchor_digest {
        return Err("captured server log baseline changed".to_owned());
    }
    let window_bytes = snapshot_size - baseline.size_bytes;
    if window_bytes > SERVER_LOG_WINDOW_BYTES {
        return Err("captured server log window exceeded the audit limit".to_owned());
    }
    after_snapshot();
    file.seek(SeekFrom::Start(baseline.size_bytes))
        .map_err(|_| "read captured server log capability".to_owned())?;
    let mut audit = ArtifactServerLogAudit::default();
    let mut line = Vec::new();
    let mut line_overflowed = false;
    let mut scan_tail = Vec::new();
    let mut chunk = [0_u8; 8192];
    let mut remaining = window_bytes;
    while remaining != 0 {
        let maximum = usize::try_from(remaining.min(chunk.len() as u64))
            .map_err(|_| "read captured server log capability".to_owned())?;
        let read = file
            .read(&mut chunk[..maximum])
            .map_err(|_| "read captured server log capability".to_owned())?;
        if read == 0 {
            return Err("captured server log shrank during audit".to_owned());
        }
        remaining = remaining.saturating_sub(read as u64);
        let bytes = &chunk[..read];
        if contains_forbidden(&scan_tail, bytes, forbidden_needles) {
            audit.forbidden_needle_matches = audit.forbidden_needle_matches.saturating_add(1);
        }
        let retained = SERVER_LOG_NEEDLE_BYTES.saturating_sub(1);
        scan_tail.extend_from_slice(bytes);
        if scan_tail.len() > retained {
            let remove = scan_tail.len() - retained;
            scan_tail.drain(..remove);
        }
        for byte in bytes {
            if *byte == b'\n' {
                classify_server_log_bytes(&mut audit, &line);
                line.clear();
                line_overflowed = false;
            } else if line.len() < SERVER_LOG_LINE_BYTES {
                line.push(*byte);
            } else if !line_overflowed {
                audit.oversized_lines = audit.oversized_lines.saturating_add(1);
                line_overflowed = true;
            }
        }
    }
    if !line.is_empty() {
        classify_server_log_bytes(&mut audit, &line);
    }
    assert_server_log_namespace_current(baseline)?;
    Ok(audit)
}

#[cfg(not(any(unix, windows)))]
pub fn audit_server_log(
    _baseline: &ArtifactServerLogBaseline,
    _forbidden_needles: &[&[u8]],
) -> Result<ArtifactServerLogAudit, String> {
    Err("captured server log capability is unsupported on this platform".to_owned())
}

fn contains_forbidden(prefix: &[u8], bytes: &[u8], needles: &[&[u8]]) -> bool {
    let mut combined = Vec::with_capacity(prefix.len().saturating_add(bytes.len()));
    combined.extend_from_slice(prefix);
    combined.extend_from_slice(bytes);
    needles.iter().any(|needle| {
        combined
            .windows(needle.len())
            .any(|candidate| candidate == *needle)
    })
}

/// Classifies a bounded captured-log window without retaining any line.
#[must_use]
pub fn classify_server_log(window: &str) -> ArtifactServerLogAudit {
    let mut audit = ArtifactServerLogAudit::default();
    for line in window.lines() {
        classify_server_log_line(&mut audit, line);
    }
    audit
}

/// Accumulates one captured-log line into an audit without retaining it.
fn classify_server_log_line(audit: &mut ArtifactServerLogAudit, line: &str) {
    audit.inspected_lines = audit.inspected_lines.saturating_add(1);
    if is_panic_or_fatal(line) {
        audit.panic_or_fatal_lines = audit.panic_or_fatal_lines.saturating_add(1);
        return;
    }
    if !is_error_line(line) {
        return;
    }
    match KNOWN_SERVER_LOG_CLASSES
        .into_iter()
        .find(|(_, marker)| line.contains(marker))
    {
        Some((class, _)) => {
            let counter = audit.known_classes.entry(class).or_insert(0);
            *counter = counter.saturating_add(1);
        }
        None => {
            audit.unclassified_error_lines = audit.unclassified_error_lines.saturating_add(1);
        }
    }
}

fn classify_server_log_bytes(audit: &mut ArtifactServerLogAudit, line: &[u8]) {
    let line = String::from_utf8_lossy(line);
    classify_server_log_line(audit, &line);
}

fn is_panic_or_fatal(line: &str) -> bool {
    line.contains("\"level\":\"fatal\"")
        || line.contains("panic: ")
        || line.contains("runtime error:")
        || line.starts_with("goroutine ")
}

fn is_error_line(line: &str) -> bool {
    line.contains("\"level\":\"error\"") || line.contains("\tERROR\t")
}

/// Requires a completed disposable run, rejecting skipped admission.
///
/// # Errors
///
/// Returns a fixed message when admission was skipped, so an unauthorized or
/// unconfigured environment can never be reported as passing coverage.
pub fn require_completed<T>(run: DisposableRun<T>, scenario: &str) -> Result<T, String> {
    match run {
        DisposableRun::Completed(value) => Ok(value),
        DisposableRun::Skipped(_) => Err(format!(
            "{scenario} requires prefix-authorized disposable admission"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixture_relative_names_admit_only_one_ordinary_component() {
        for accepted in ["source.txt", "a", "name with spaces.png", "a.b.c"] {
            assert!(
                simple_relative_name(accepted),
                "expected an admitted fixture name: {accepted}"
            );
        }
        // Rejections are identical on every platform, including the spellings
        // that only `Path::join` on Windows would resolve outside the fixture.
        for rejected in [
            "",
            ".",
            "..",
            "../escape",
            "sub/source.txt",
            "sub\\source.txt",
            "\\\\server\\share",
            "C:\\evil",
            "C:evil",
            "/absolute",
            "name\u{0}.txt",
        ] {
            assert!(
                !simple_relative_name(rejected),
                "expected a refused fixture name: {rejected:?}"
            );
        }
    }

    #[test]
    fn transport_matrix_is_closed_complete_and_uniquely_identified() {
        let mut ids = std::collections::BTreeSet::new();
        for transport in ArtifactTransport::ALL {
            assert!(ids.insert(transport.id()), "duplicate transport identifier");
            assert_eq!(ArtifactTransport::parse(transport.id()), Some(transport));
        }
        assert_eq!(
            ids.len(),
            ArtifactControlPlane::ALL.len() * ArtifactDataPlane::ALL.len()
        );
        for control in ArtifactControlPlane::ALL {
            assert_eq!(ArtifactControlPlane::parse(control.as_str()), Some(control));
            for data in ArtifactDataPlane::ALL {
                assert!(ArtifactTransport::ALL.contains(&ArtifactTransport::new(control, data)));
            }
        }
    }

    #[test]
    fn executed_matrices_partition_the_complete_transport_inventory() {
        let mut union = ArtifactTransport::DIRECT_MATRIX.to_vec();
        union.extend(ArtifactTransport::SPAWNED_MATRIX);
        assert_eq!(union.len(), ArtifactTransport::ALL.len());
        for transport in ArtifactTransport::ALL {
            assert_eq!(
                union
                    .iter()
                    .filter(|candidate| **candidate == transport)
                    .count(),
                1,
                "transport {} is not executed exactly once",
                transport.id()
            );
        }
        assert!(
            ArtifactTransport::DIRECT_MATRIX
                .iter()
                .all(|transport| !transport.control().is_spawned())
        );
        assert!(
            ArtifactTransport::SPAWNED_MATRIX
                .iter()
                .all(|transport| transport.control().is_spawned())
        );
    }

    #[test]
    fn preview_control_plane_is_the_only_preview_revision() {
        for control in ArtifactControlPlane::ALL {
            let expected = if control == ArtifactControlPlane::SpawnedPreviewStdio {
                "2026-07-28"
            } else {
                "2025-11-25"
            };
            assert_eq!(control.protocol_version(), expected);
        }
    }

    fn descriptor(name: &str, description: &str) -> Value {
        json!({
            "name": name,
            "description": description,
            "inputSchema": {"type": "object", "properties": {"space": {"type": "string"}}},
            "outputSchema": {"type": "object"}
        })
    }

    fn complete_descriptors() -> Vec<Value> {
        ARTIFACT_TOOL_NAMES
            .into_iter()
            .map(|name| descriptor(name, "bounded"))
            .collect()
    }

    #[test]
    fn catalog_snapshot_requires_the_exact_artifact_inventory() {
        let mut descriptors = complete_descriptors();
        assert!(ArtifactCatalogSnapshot::from_descriptors(&descriptors).is_ok());
        descriptors.push(descriptor("object_search", "unrelated"));
        let snapshot = ArtifactCatalogSnapshot::from_descriptors(&descriptors)
            .expect("unrelated tools are ignored");
        assert_eq!(snapshot.tool_digests().len(), ARTIFACT_TOOL_NAMES.len());

        descriptors.retain(|entry| entry["name"] != json!("file_import"));
        assert!(ArtifactCatalogSnapshot::from_descriptors(&descriptors).is_err());
    }

    #[test]
    fn catalog_snapshot_ignores_key_order_and_detects_contract_drift() {
        let baseline = ArtifactCatalogSnapshot::from_descriptors(&complete_descriptors())
            .expect("baseline catalog");
        let reordered = ARTIFACT_TOOL_NAMES
            .into_iter()
            .map(|name| {
                json!({
                    "outputSchema": {"type": "object"},
                    "inputSchema": {"properties": {"space": {"type": "string"}}, "type": "object"},
                    "description": "bounded",
                    "name": name
                })
            })
            .collect::<Vec<_>>();
        let reordered =
            ArtifactCatalogSnapshot::from_descriptors(&reordered).expect("reordered catalog");
        assert_eq!(baseline.digest(), reordered.digest());
        assert!(baseline.compare(&reordered).is_ok());

        let mut drifted = complete_descriptors();
        drifted[0] = descriptor(ARTIFACT_TOOL_NAMES[0], "relaxed");
        let drifted = ArtifactCatalogSnapshot::from_descriptors(&drifted).expect("drifted catalog");
        assert_ne!(baseline.digest(), drifted.digest());
        assert_eq!(
            baseline.compare(&drifted),
            Err(format!(
                "artifact tool contract diverged: {}",
                ARTIFACT_TOOL_NAMES[0]
            ))
        );
    }

    fn evidence(transport: ArtifactTransport, file_sha256: &str) -> ArtifactSmokeEvidence {
        ArtifactSmokeEvidence {
            transport: transport.id(),
            catalog: ArtifactCatalogSnapshot::from_descriptors(&complete_descriptors())
                .expect("catalog"),
            import_root_count: 1,
            export_root_count: 1,
            staging_active: true,
            file_bytes: 21,
            file_sha256: file_sha256.to_owned(),
            created_document_sha256: "a".repeat(64),
            exported_document_sha256: "a".repeat(64),
            updated_document_sha256: "b".repeat(64),
            stage_released: true,
        }
    }

    #[test]
    fn parity_requires_the_complete_ordered_matrix_and_identical_behavior() {
        let expected = ArtifactTransport::DIRECT_MATRIX;
        let hash = artifact_sha256(ARTIFACT_FILE_PAYLOAD);
        let executed = vec![evidence(expected[0], &hash), evidence(expected[1], &hash)];
        assert_eq!(assert_artifact_parity(&executed, &expected), Ok(()));

        assert!(assert_artifact_parity(&executed[..1], &expected).is_err());
        let reversed = vec![evidence(expected[1], &hash), evidence(expected[0], &hash)];
        assert!(assert_artifact_parity(&reversed, &expected).is_err());

        let divergent = vec![
            evidence(expected[0], &hash),
            evidence(expected[1], &"c".repeat(64)),
        ];
        assert_eq!(
            assert_artifact_parity(&divergent, &expected),
            Err(format!(
                "artifact transport diverged from {}: {}",
                expected[0].id(),
                expected[1].id()
            ))
        );
    }

    #[test]
    fn tool_frames_must_carry_a_complete_successful_envelope() {
        let frame = json!({
            "jsonrpc": "2.0",
            "id": 7,
            "result": {
                "isError": false,
                "content": [{"type": "text", "text": "{}"}],
                "structuredContent": {"released": true}
            }
        });
        assert_eq!(
            validate_tool_frame("artifact_release", 7, &frame),
            Ok(json!({"released": true}))
        );
        assert!(validate_tool_frame("artifact_release", 8, &frame).is_err());

        for mutation in [
            json!({"id": 7, "result": {"isError": false, "content": [1], "structuredContent": {}}}),
            json!({"jsonrpc": "2.0", "id": 7, "error": {"code": -32602}}),
            json!({"jsonrpc": "2.0", "id": 7, "result": {"isError": true, "content": [1], "structuredContent": {}}}),
            json!({"jsonrpc": "2.0", "id": 7, "result": {"isError": false, "content": [], "structuredContent": {}}}),
            json!({"jsonrpc": "2.0", "id": 7, "result": {"isError": false, "content": [1]}}),
        ] {
            assert!(validate_tool_frame("artifact_release", 7, &mutation).is_err());
        }
    }

    #[test]
    fn server_log_audit_counts_classes_without_retaining_lines() {
        let window = concat!(
            "{\"level\":\"error\",\"msg\":\"failed to update details failed to load space, mode is 3\"}\n",
            "{\"level\":\"error\",\"msg\":\"failed to update details failed to load space, mode is 3\"}\n",
            "{\"level\":\"error\",\"msg\":\"process next pending upload item\"}\n",
            "{\"level\":\"info\",\"msg\":\"ordinary\"}\n",
            "{\"level\":\"error\",\"msg\":\"brand new artifact staging failure\"}\n",
            "{\"level\":\"fatal\",\"msg\":\"store closed\"}\n"
        );
        let audit = classify_server_log(window);
        assert_eq!(audit.inspected_lines, 6);
        assert_eq!(audit.panic_or_fatal_lines, 1);
        assert_eq!(audit.unclassified_error_lines, 1);
        assert_eq!(
            audit
                .known_classes
                .get("deleted_space_sync_status")
                .copied(),
            Some(2)
        );
        assert_eq!(
            audit.known_classes.get("filesync_pending_upload").copied(),
            Some(1)
        );
        assert!(!audit.is_clean());
        assert!(classify_server_log("{\"level\":\"info\",\"msg\":\"ok\"}\n").is_clean());
    }

    #[test]
    fn server_log_audit_reads_only_the_window_after_the_baseline() {
        let directory = std::env::temp_dir().join(format!("any-mcp-log-{}", unique_suffix()));
        fs::create_dir_all(&directory).expect("create audit fixture directory");
        let path = directory.join("server.log");
        fs::write(
            &path,
            b"{\"level\":\"error\",\"msg\":\"pre-baseline unclassified \xff\xfe\"}\n",
        )
        .expect("write baseline log");
        secure_files(std::slice::from_ref(&path)).expect("secure baseline log");
        let baseline = server_log_baseline(&path).expect("open baseline capability");
        let mut appended =
            b"{\"level\":\"error\",\"msg\":\"process next pending upload item\"}\r\n".to_vec();
        appended.extend_from_slice(b"{\"level\":\"info\",\"msg\":\"ordinary\"}\n");
        OpenOptions::new()
            .append(true)
            .open(&path)
            .expect("open audit log")
            .write_all(&appended)
            .expect("append audit log");

        let audit = audit_server_log(&baseline, &[]).expect("audit appended window");
        assert_eq!(audit.inspected_lines, 2);
        assert_eq!(audit.panic_or_fatal_lines, 0);
        assert_eq!(audit.unclassified_error_lines, 0);
        assert_eq!(
            audit.known_classes.get("filesync_pending_upload").copied(),
            Some(1)
        );

        fs::write(&path, b"short\n").expect("truncate audit log");
        assert!(
            audit_server_log(&baseline, &[]).is_err(),
            "a shrunken log must fail closed"
        );
        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    fn server_log_audit_stops_at_its_snapshotted_end() {
        let directory = std::env::temp_dir().join(format!("any-mcp-log-{}", unique_suffix()));
        fs::create_dir_all(&directory).expect("create audit fixture directory");
        let path = directory.join("server.log");
        fs::write(&path, b"").expect("write baseline log");
        secure_files(std::slice::from_ref(&path)).expect("secure baseline log");
        let baseline = server_log_baseline(&path).expect("open baseline capability");

        let audit = audit_server_log_with_snapshot_hook(&baseline, &[], || {
            OpenOptions::new()
                .append(true)
                .open(&path)
                .expect("open growing audit log")
                .write_all(b"{\"level\":\"fatal\",\"msg\":\"late append\"}\n")
                .expect("append after snapshot");
        })
        .expect("audit snapshotted window");
        assert_eq!(audit.inspected_lines, 0);
        assert!(audit.is_clean());
        assert!(
            !audit_server_log(&baseline, &[])
                .expect("audit late append")
                .is_clean()
        );
        let _ = fs::remove_dir_all(&directory);
    }

    #[cfg(unix)]
    #[test]
    fn server_log_audit_rejects_rotation_and_reopen() {
        let directory = std::env::temp_dir().join(format!("any-mcp-log-{}", unique_suffix()));
        fs::create_dir_all(&directory).expect("create audit fixture directory");
        let path = directory.join("server.log");
        let rotated = directory.join("server.log.1");
        fs::write(&path, b"").expect("write baseline log");
        secure_files(std::slice::from_ref(&path)).expect("secure baseline log");
        let baseline = server_log_baseline(&path).expect("open baseline capability");

        fs::rename(&path, &rotated).expect("rotate server log");
        fs::write(&path, b"{\"level\":\"fatal\",\"msg\":\"new inode\"}\n")
            .expect("write replacement server log");
        secure_files(std::slice::from_ref(&path)).expect("secure replacement log");
        assert!(
            audit_server_log(&baseline, &[]).is_err(),
            "rotation and reopen must fail closed"
        );
        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    fn policy_values_are_escaped_for_toml_basic_strings() {
        assert_eq!(
            toml_basic_string("C:\\Users\\any \"mcp\"\ttmp"),
            "C:\\\\Users\\\\any \\\"mcp\\\"\\ttmp"
        );
        assert_eq!(toml_basic_string("/tmp/any-mcp"), "/tmp/any-mcp");
        assert_eq!(toml_basic_string("\u{1}"), "\\u0001");
    }

    #[test]
    fn policy_fixture_declares_exact_roots_and_private_permissions() {
        let fixture = ArtifactPolicyFixture::create("bafyrei-acceptance-space")
            .expect("artifact policy fixture");
        let contents = fixture.policy_contents().expect("policy contents");
        assert!(contents.contains("schema_version = 1"));
        assert!(contents.contains("read_only = false"));
        assert!(contents.contains("id = \"bafyrei-acceptance-space\""));
        assert!(contents.contains("id = \"inbox\""));
        assert!(contents.contains("id = \"outbox\""));
        assert!(contents.contains("[staging]"));
        assert!(
            fixture
                .staging_base_url()
                .is_some_and(|url| url.starts_with("http://127.0.0.1:"))
        );
        assert_eq!(
            fs::read(
                fixture
                    .import_root()
                    .join(ArtifactPolicyFixture::FILE_SOURCE)
            )
            .expect("seeded file source"),
            ARTIFACT_FILE_PAYLOAD
        );
        assert!(fixture.read_export("../escape").is_err());
        assert!(!fixture.export_exists("missing.bin"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(fixture.config_path())
                .expect("policy metadata")
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        let base = fixture.base.clone();
        drop(fixture);
        assert!(!base.exists(), "fixture teardown removed every artifact");
    }

    #[test]
    fn policy_fixture_omits_staging_when_it_is_disabled() {
        let fixture = ArtifactPolicyFixture::create_with(
            "bafyrei-acceptance-space",
            ArtifactPolicyScenario::StagingDisabled.policy_options(),
        )
        .expect("artifact policy fixture");
        let contents = fixture.policy_contents().expect("policy contents");
        assert!(!contents.contains("[staging]"));
        assert!(fixture.staging_base_url().is_none());
    }

    #[test]
    fn policy_scenarios_are_closed_uniquely_named_and_configured() {
        let mut identifiers = std::collections::BTreeSet::new();
        let mut options = Vec::new();
        for scenario in ArtifactPolicyScenario::ALL {
            assert!(
                identifiers.insert(scenario.as_str()),
                "duplicate policy scenario identifier"
            );
            assert_eq!(
                ArtifactPolicyScenario::parse(scenario.as_str()),
                Some(scenario)
            );
            assert!(
                !options.contains(&scenario.policy_options()),
                "policy scenario {} duplicates another configuration",
                scenario.as_str()
            );
            options.push(scenario.policy_options());
        }
        assert!(ArtifactPolicyScenario::parse("spaces_partial").is_none());
        for probe in ArtifactPolicyProbe::ALL {
            assert!(ARTIFACT_TOOL_NAMES.contains(&probe.tool_name()));
        }
    }

    #[test]
    fn rendered_policy_declares_each_configured_space_shape() {
        let render = |scenario: ArtifactPolicyScenario| {
            ArtifactPolicyFixture::create_with("bafyrei-under-test", scenario.policy_options())
                .expect("artifact policy fixture")
                .policy_contents()
                .expect("policy contents")
        };

        let omitted = render(ArtifactPolicyScenario::SpacesOmitted);
        assert!(!omitted.contains("allowed"));
        let empty = render(ArtifactPolicyScenario::SpacesEmpty);
        assert!(empty.contains("allowed = []"));
        let restricted = render(ArtifactPolicyScenario::SpacesRestrictedElsewhere);
        assert!(restricted.contains(UNAUTHORIZED_SPACE_ID));
        assert!(!restricted.contains("bafyrei-under-test"));
        let allowed = render(ArtifactPolicyScenario::ReadOnly);
        assert!(allowed.contains("allowed = [{ id = \"bafyrei-under-test\" }]"));
    }

    #[test]
    fn rendered_policy_is_always_writable_because_the_parser_rejects_read_only() {
        // `ArtifactConfig::from_toml` fails closed on `spaces.read_only = true`,
        // so read-only coverage must come from the server's read-only mode.
        for scenario in ArtifactPolicyScenario::ALL {
            let contents =
                ArtifactPolicyFixture::create_with("bafyrei-under-test", scenario.policy_options())
                    .expect("artifact policy fixture")
                    .policy_contents()
                    .expect("policy contents");
            assert!(contents.contains("read_only = false"));
            assert!(!contents.contains("read_only = true"));
        }
    }

    #[test]
    fn policy_expectations_cover_every_scenario_and_probe_exactly() {
        let refused = |code, message| ArtifactProbeExpectation::Refused { code, message };
        for probe in ArtifactPolicyProbe::ALL {
            assert_eq!(
                probe_expectation(ArtifactPolicyScenario::ReadOnly, probe),
                refused(VALIDATION_CODE, Some(READ_ONLY_GUIDANCE))
            );
            for denied in [
                ArtifactPolicyScenario::SpacesEmpty,
                ArtifactPolicyScenario::SpacesRestrictedElsewhere,
            ] {
                assert!(!denied.admits_space_under_test());
                assert_eq!(
                    probe_expectation(denied, probe),
                    refused(AUTHENTICATION_CODE, None)
                );
            }
            let expected = if probe == ArtifactPolicyProbe::StageUpload {
                ArtifactProbeExpectation::Accepted
            } else {
                refused(NOT_FOUND_CODE, None)
            };
            assert_eq!(
                probe_expectation(ArtifactPolicyScenario::SpacesOmitted, probe),
                expected
            );
            let staging_disabled = if probe == ArtifactPolicyProbe::StageUpload {
                refused(VALIDATION_CODE, Some(STAGING_REQUIRED_GUIDANCE))
            } else {
                refused(NOT_FOUND_CODE, None)
            };
            assert_eq!(
                probe_expectation(ArtifactPolicyScenario::StagingDisabled, probe),
                staging_disabled
            );
        }
    }

    #[test]
    fn read_only_configuration_advertises_only_the_status_tool() {
        assert_eq!(
            ArtifactPolicyScenario::ReadOnly.advertised_tools(),
            vec!["artifact_status"]
        );
        assert_eq!(
            ArtifactPolicyScenario::ReadOnly.expected_status(),
            ArtifactStatusEvidence {
                local_roots_active: false,
                import_root_count: 0,
                export_root_count: 0,
                staging_configured: true,
                staging_active: false,
            }
        );
        for scenario in ArtifactPolicyScenario::ALL {
            if scenario.is_read_only() {
                continue;
            }
            assert_eq!(scenario.advertised_tools(), ARTIFACT_TOOL_NAMES.to_vec());
            let status = scenario.expected_status();
            assert!(status.local_roots_active);
            assert_eq!(status.import_root_count, 1);
            assert_eq!(status.export_root_count, 1);
            assert_eq!(
                status.staging_active,
                scenario.policy_options().staging,
                "staging activation must follow the configuration"
            );
            assert_eq!(status.staging_configured, status.staging_active);
        }
    }

    fn policy_evidence(
        scenario: ArtifactPolicyScenario,
        control: ArtifactControlPlane,
    ) -> ArtifactPolicyEvidence {
        let advertised_tools = scenario
            .advertised_tools()
            .into_iter()
            .map(ToOwned::to_owned)
            .collect::<Vec<_>>();
        let catalog_digests = advertised_tools
            .iter()
            .map(|name| (name.clone(), "d".repeat(64)))
            .collect();
        let probes = ArtifactPolicyProbe::ALL
            .into_iter()
            .map(|probe| {
                let outcome = match probe_expectation(scenario, probe) {
                    ArtifactProbeExpectation::Accepted => ArtifactProbeOutcome::Accepted,
                    ArtifactProbeExpectation::Refused { code, message } => {
                        ArtifactProbeOutcome::Refused {
                            code: code.to_owned(),
                            message: message.unwrap_or("Not found.").to_owned(),
                        }
                    }
                };
                (probe.as_str(), outcome)
            })
            .collect();
        ArtifactPolicyEvidence {
            scenario: scenario.as_str(),
            control: control.as_str(),
            advertised_tools,
            catalog_digests,
            status: scenario.expected_status(),
            probes,
        }
    }

    #[test]
    fn policy_parity_requires_the_complete_ordered_control_matrix() {
        let scenario = ArtifactPolicyScenario::SpacesRestrictedElsewhere;
        let expected = ArtifactControlPlane::ALL;
        let executed = expected
            .into_iter()
            .map(|control| policy_evidence(scenario, control))
            .collect::<Vec<_>>();
        assert_eq!(assert_artifact_policy_parity(&executed, &expected), Ok(()));

        assert!(assert_artifact_policy_parity(&executed[..2], &expected).is_err());
        let mut reordered = executed.clone();
        reordered.swap(0, 1);
        assert!(assert_artifact_policy_parity(&reordered, &expected).is_err());

        let mut divergent = executed.clone();
        divergent[3].probes.insert(
            ArtifactPolicyProbe::StageUpload.as_str(),
            ArtifactProbeOutcome::Accepted,
        );
        assert_eq!(
            assert_artifact_policy_parity(&divergent, &expected),
            Err(format!(
                "artifact policy control plane diverged from {}: {}",
                expected[0].as_str(),
                expected[3].as_str()
            ))
        );

        let mut relaxed = executed;
        relaxed[2].advertised_tools = vec!["artifact_status".to_owned()];
        assert!(assert_artifact_policy_parity(&relaxed, &expected).is_err());
    }

    #[test]
    fn advertised_digests_accept_a_reduced_read_only_catalog() {
        let mut descriptors = complete_descriptors();
        assert_eq!(
            artifact_descriptor_digests(&descriptors)
                .expect("complete catalog")
                .len(),
            ARTIFACT_TOOL_NAMES.len()
        );
        descriptors.retain(|entry| entry["name"] == json!("artifact_status"));
        descriptors.push(descriptor("object_search", "unrelated"));
        let digests = artifact_descriptor_digests(&descriptors).expect("reduced catalog");
        assert_eq!(
            digests.keys().map(String::as_str).collect::<Vec<_>>(),
            vec!["artifact_status"]
        );

        descriptors.push(descriptor("artifact_status", "duplicate"));
        assert!(artifact_descriptor_digests(&descriptors).is_err());
    }

    #[test]
    fn content_scenarios_are_closed_uniquely_named_and_permissive() {
        let mut ids = std::collections::BTreeSet::new();
        for scenario in ArtifactContentScenario::ALL {
            assert!(ids.insert(scenario.as_str()), "duplicate content scenario");
            assert_eq!(
                ArtifactContentScenario::parse(scenario.as_str()),
                Some(scenario)
            );
            let options = scenario.policy_options();
            // Content scenarios must never re-test refusal: both planes stay
            // active and the space under test stays authorized.
            assert!(options.staging);
            assert!(!options.read_only);
            assert!(options.spaces.admits_space_under_test());
            assert_eq!(options.validators, scenario.validator_policy());
            let (configured, available) = scenario.expected_validator_counts();
            assert_eq!(
                configured,
                u64::from(scenario.validator_policy().is_declared())
            );
            assert!(available <= configured);
        }
        assert_eq!(
            ArtifactContentScenario::ALL
                .into_iter()
                .filter(|scenario| scenario.validator_policy().is_declared())
                .count(),
            2,
            "exactly one optional and one required validator scenario"
        );
    }

    #[test]
    fn mime_fixtures_are_distinct_in_identity_bytes_and_media_type() {
        let mut ids = std::collections::BTreeSet::new();
        let mut payloads = std::collections::BTreeSet::new();
        let mut media_types = std::collections::BTreeSet::new();
        for fixture in ArtifactMimeFixture::ALL {
            assert!(ids.insert(fixture.as_str()), "duplicate fixture identifier");
            assert_eq!(ArtifactMimeFixture::parse(fixture.as_str()), Some(fixture));
            // Anytype files are content addressed, so shared bytes would
            // collapse two fixtures into one upstream object.
            assert!(
                payloads.insert(fixture.payload()),
                "duplicate fixture payload"
            );
            assert!(
                media_types.insert(fixture.media_type()),
                "duplicate fixture media type"
            );
            assert!(!fixture.payload().is_empty());
            assert!(!fixture.extension().is_empty());
        }
        assert!(
            ArtifactMimeFixture::Image
                .payload()
                .starts_with(b"\x89PNG\r\n\x1a\n")
        );
        assert!(ArtifactMimeFixture::Audio.payload().starts_with(b"RIFF"));
    }

    #[test]
    fn canonicalization_effects_are_closed_and_classified_exactly() {
        for effect in CanonicalizationEffect::ALL {
            assert_eq!(
                CanonicalizationEffect::parse(effect.as_str()),
                Some(effect),
                "effect identifier must round trip"
            );
        }
        let hard_break = format!("{CONTENT_PLAIN_TEXT}{ANYTYPE_PLAIN_MARKDOWN_SUFFIX}");
        let cases: [(&str, &str, Vec<&'static str>); 7] = [
            ("same", "same", vec!["identical"]),
            (
                CONTENT_PLAIN_TEXT,
                hard_break.as_str(),
                vec!["hard_break_suffix_appended", "trailing_newline_added"],
            ),
            (
                "any_mcp",
                "any\\_mcp   \n",
                vec![
                    "hard_break_suffix_appended",
                    "underscore_escaped",
                    "trailing_newline_added",
                ],
            ),
            (
                "first\r\nsecond\n",
                "first\nsecond\n",
                vec!["carriage_return_dropped"],
            ),
            ("body\n", "body", vec!["trailing_newline_dropped"]),
            (
                "one\n\n\ntwo\n",
                "one\n\ntwo\n",
                vec!["blank_lines_collapsed", "line_count_changed"],
            ),
            ("alpha\n", "beta\n", vec!["text_rewritten"]),
        ];
        for (source, canonical, expected) in &cases {
            let observed = classify_canonicalization(source, canonical);
            assert_eq!(
                &observed, expected,
                "source {source:?} canonical {canonical:?}"
            );
            // Every reported category must belong to the closed inventory.
            for effect in observed {
                assert!(CanonicalizationEffect::parse(effect).is_some());
            }
        }
    }

    #[test]
    fn validator_expectations_cover_every_policy_and_probe_exactly() {
        for probe in ArtifactValidatorProbe::ALL {
            assert_eq!(ArtifactValidatorProbe::parse(probe.as_str()), Some(probe));
            assert_eq!(
                validator_expectation(FixtureValidatorPolicy::Absent, probe),
                ArtifactValidatorOutcome::NoFindings,
                "an undeclared validator never reports a finding"
            );
        }
        // An out-of-scope declaration proves a configured validator does not
        // run on every artifact, whatever its required flag.
        for policy in FixtureValidatorPolicy::ALL {
            assert_eq!(
                validator_expectation(policy, ArtifactValidatorProbe::OutOfScope),
                ArtifactValidatorOutcome::NoFindings
            );
        }
        let matched = validator_expectation(
            FixtureValidatorPolicy::Optional,
            ArtifactValidatorProbe::MatchedDeclaration,
        );
        let mismatched_optional = validator_expectation(
            FixtureValidatorPolicy::Optional,
            ArtifactValidatorProbe::MismatchedDeclaration,
        );
        let mismatched_required = validator_expectation(
            FixtureValidatorPolicy::Required,
            ArtifactValidatorProbe::MismatchedDeclaration,
        );
        if VALIDATOR_PLATFORM_ACTIVATES {
            assert_eq!(
                matched,
                ArtifactValidatorOutcome::Finding {
                    status: "accepted".to_owned(),
                    detected_media_type: Some("text/plain".to_owned())
                }
            );
            assert_eq!(
                mismatched_optional,
                ArtifactValidatorOutcome::Finding {
                    status: "rejected".to_owned(),
                    detected_media_type: Some("text/plain".to_owned())
                }
            );
            assert_eq!(
                mismatched_required,
                ArtifactValidatorOutcome::Refused {
                    code: VALIDATION_CODE.to_owned()
                }
            );
        } else {
            assert_eq!(
                matched,
                ArtifactValidatorOutcome::Finding {
                    status: "unavailable".to_owned(),
                    detected_media_type: None
                }
            );
            assert_eq!(mismatched_optional, matched);
            assert_eq!(
                mismatched_required,
                ArtifactValidatorOutcome::Refused {
                    code: VALIDATION_CODE.to_owned()
                }
            );
        }
        // The mismatch probe must stay inside the configured MIME scope, or it
        // would prove scope gating instead of declaration checking.
        assert!(
            FIXTURE_VALIDATOR_MIME
                .contains(&ArtifactValidatorProbe::MatchedDeclaration.declared_media_type())
        );
        assert!(
            FIXTURE_VALIDATOR_MIME
                .contains(&ArtifactValidatorProbe::MismatchedDeclaration.declared_media_type())
        );
        assert!(
            !FIXTURE_VALIDATOR_MIME
                .contains(&ArtifactValidatorProbe::OutOfScope.declared_media_type())
        );
    }

    #[test]
    fn declared_validator_policy_renders_a_parsable_pinned_declaration() {
        let validator = PinnedValidatorExecutable {
            path: PathBuf::from(if cfg!(windows) {
                r"C:\tools\file.exe"
            } else {
                "/usr/bin/file"
            }),
            sha256: "a".repeat(64),
        };
        for policy in FixtureValidatorPolicy::ALL {
            let options = ArtifactPolicyOptions {
                validators: policy,
                ..ArtifactPolicyOptions::default()
            };
            let rendered = render_policy(
                "bafyrei-under-test",
                Path::new("/tmp/import"),
                Path::new("/tmp/export"),
                Path::new("/tmp/staging"),
                Some("http://127.0.0.1:18765/artifacts/v1/"),
                Some(&validator),
                options,
            );
            let parsed = rendered
                .parse::<toml::Table>()
                .expect("rendered fixture policy must be valid TOML");
            let declared = parsed.get("validators").and_then(toml::Value::as_array);
            if !policy.is_declared() {
                assert!(declared.is_none(), "{}", policy.as_str());
                continue;
            }
            let entries = declared.expect("declared validator table");
            assert_eq!(entries.len(), 1);
            let entry = &entries[0];
            // `RawValidator` denies unknown fields and requires every one of
            // these, so the rendered key set must match it exactly.
            let mut keys = entry
                .as_table()
                .expect("validator entry")
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>();
            keys.sort_unstable();
            assert_eq!(
                keys,
                vec![
                    "driver",
                    "field_bytes",
                    "fields",
                    "id",
                    "input_bytes",
                    "memory_bytes",
                    "mime",
                    "path",
                    "platform",
                    "required",
                    "sha256",
                    "stderr_bytes",
                    "stdout_bytes",
                    "timeout_secs",
                ]
            );
            // Bounds enforced by the production validator policy parser.
            let integer = |key: &str| entry[key].as_integer().expect("integer field");
            assert!(integer("timeout_secs") > 0);
            assert!((16 * 1024 * 1024..=2 * 1024 * 1024 * 1024).contains(&integer("memory_bytes")));
            assert!(integer("input_bytes") > 0);
            assert!((1..=1024 * 1024).contains(&integer("stdout_bytes")));
            assert!((1..=1024 * 1024).contains(&integer("stderr_bytes")));
            assert!((1..=256).contains(&integer("fields")));
            assert!((1..=64 * 1024).contains(&integer("field_bytes")));
            assert_eq!(entry["id"].as_str(), Some(FIXTURE_VALIDATOR_ID));
            assert_eq!(entry["driver"].as_str(), Some("file-mime"));
            assert_eq!(entry["platform"].as_str(), Some("linux-retained-fd-v1"));
            assert_eq!(entry["required"].as_bool(), Some(policy.is_required()));
            assert_eq!(entry["sha256"].as_str(), Some(validator.sha256()));
            assert_eq!(
                entry["mime"]
                    .as_array()
                    .expect("mime scope")
                    .iter()
                    .filter_map(toml::Value::as_str)
                    .collect::<Vec<_>>(),
                FIXTURE_VALIDATOR_MIME.to_vec()
            );
        }
    }

    #[test]
    fn discovered_validator_is_a_real_hash_pinned_executable() {
        match PinnedValidatorExecutable::discover() {
            Ok(pinned) => {
                assert!(pinned.path().is_absolute());
                assert!(pinned.path().is_file());
                assert_eq!(pinned.sha256().len(), 64);
                assert!(
                    pinned
                        .sha256()
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
                );
                // The pinned bytes must still hash to the declared digest, or
                // production would refuse the executable at activation.
                let bytes = fs::read(pinned.path()).expect("read pinned validator");
                assert_eq!(artifact_sha256(&bytes), pinned.sha256());
            }
            Err(error) => assert_eq!(
                error,
                "artifact validator fixture requires a hash-pinnable file(1) executable"
            ),
        }
    }

    fn content_evidence(
        scenario: ArtifactContentScenario,
        control: ArtifactControlPlane,
    ) -> ArtifactContentEvidence {
        let (validator_count, validator_available_count) = scenario.expected_validator_counts();
        ArtifactContentEvidence {
            scenario: scenario.as_str(),
            control: control.as_str(),
            validator_count,
            validator_available_count,
            files: vec![ArtifactFileRecord {
                case: "text+local_roots".to_owned(),
                declared_media_type: "text/plain".to_owned(),
                stored_media_type: None,
                size_bytes: 3,
                sha256: "a".repeat(64),
                exported_size_bytes: 3,
                exported_sha256: "a".repeat(64),
            }],
            documents: Vec::new(),
            validators: Vec::new(),
        }
    }

    #[test]
    fn content_parity_requires_the_complete_ordered_control_matrix() {
        let scenario = ArtifactContentScenario::MimeMatrix;
        let expected = ArtifactControlPlane::ALL;
        let executed = expected
            .into_iter()
            .map(|control| content_evidence(scenario, control))
            .collect::<Vec<_>>();
        assert_eq!(assert_artifact_content_parity(&executed, &expected), Ok(()));

        assert!(assert_artifact_content_parity(&executed[..2], &expected).is_err());
        let mut reordered = executed.clone();
        reordered.swap(0, 1);
        assert!(assert_artifact_content_parity(&reordered, &expected).is_err());

        let mut divergent = executed.clone();
        divergent[3].files[0].stored_media_type = Some("text/plain".to_owned());
        assert_eq!(
            assert_artifact_content_parity(&divergent, &expected),
            Err(format!(
                "artifact content control plane diverged from {}: {}",
                expected[0].as_str(),
                expected[3].as_str()
            ))
        );

        let mut relabeled = executed;
        relabeled[1].validator_count += 1;
        assert!(assert_artifact_content_parity(&relabeled, &expected).is_err());
    }

    #[test]
    fn lifecycle_scenarios_are_closed_and_render_production_limit_profiles() {
        let mut ids = std::collections::BTreeSet::new();
        for scenario in ArtifactLifecycleScenario::ALL {
            assert!(
                ids.insert(scenario.as_str()),
                "duplicate lifecycle scenario"
            );
            assert_eq!(
                ArtifactLifecycleScenario::parse(scenario.as_str()),
                Some(scenario)
            );
            let profile = scenario.policy_options().limits;
            assert!(ArtifactLimitProfile::parse(profile.as_str()).is_some());
            let fixture = ArtifactPolicyFixture::create_with(
                "bafyrei-lifecycle-space",
                scenario.policy_options(),
            )
            .expect("lifecycle policy fixture");
            let contents = fixture.policy_contents().expect("lifecycle policy");
            let parsed = contents
                .parse::<toml::Table>()
                .expect("lifecycle fixture uses production TOML schema");
            assert_eq!(
                parsed.contains_key("limits"),
                profile != ArtifactLimitProfile::Default
            );
            assert_eq!(
                fixture.staging_snapshot().expect("empty staging snapshot"),
                ArtifactDirectorySnapshot::default()
            );
        }
        assert_eq!(ids.len(), ArtifactLifecycleScenario::ALL.len());
        for profile in ArtifactLimitProfile::ALL {
            assert_eq!(ArtifactLimitProfile::parse(profile.as_str()), Some(profile));
            assert!(profile.artifact_bytes() >= 64 * 1024);
            assert!(profile.staging_ttl_secs() >= 60);
        }
    }

    #[test]
    fn measured_frames_are_bounded_and_payload_independence_is_quantified() {
        let frame = |id: u64, size: u64| {
            let value = json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "isError": false,
                    "content": [{"type": "text", "text": "bounded artifact result"}],
                    "structuredContent": {
                        "file_id": "bafyrei-measured-file",
                        "receipt": {"size_bytes": size, "sha256": "a".repeat(64)}
                    }
                }
            });
            let mut encoded = serde_json::to_vec(&value).expect("encode measured frame fixture");
            encoded.push(b'\n');
            encoded
        };
        let small =
            measure_artifact_frame("file_import", 7, &frame(7, 13)).expect("small measured frame");
        let large = measure_artifact_frame("file_import", 8, &frame(8, 1024 * 1024))
            .expect("large measured frame");
        assert!(small.frame_bytes <= ARTIFACT_FRAME_CEILING_BYTES);
        assert!(large.frame_tokens <= ARTIFACT_FRAME_CEILING_TOKENS);
        assert_eq!(assert_payload_frame_independence(&small, &large), Ok(()));

        let mut divergent = large;
        divergent.frame_bytes = divergent
            .frame_bytes
            .saturating_add(ARTIFACT_PAYLOAD_FRAME_DELTA_BYTES + 1);
        assert!(assert_payload_frame_independence(&small, &divergent).is_err());
    }

    #[test]
    fn staging_allocation_debug_output_does_not_expose_capabilities() {
        let allocation = ArtifactStageAllocation {
            handle: Zeroizing::new("secret-bearer".to_owned()),
            record: "0123456789abcdef0123456789abcdef".to_owned(),
            url: "http://127.0.0.1/artifacts/v1/0123456789abcdef0123456789abcdef".to_owned(),
            size_bytes: 9,
        };
        let debug = format!("{allocation:?}");
        assert!(!debug.contains("secret-bearer"));
    }

    #[test]
    fn skipped_disposable_admission_is_never_reported_as_coverage() {
        let completed: DisposableRun<u8> = DisposableRun::Completed(3);
        assert_eq!(require_completed(completed, "artifact smoke"), Ok(3));
        let skipped: DisposableRun<u8> =
            DisposableRun::Skipped(anytype::test_util::DisposableSkip::PrefixNotConfigured);
        assert!(require_completed(skipped, "artifact smoke").is_err());
    }

    #[test]
    fn adversarial_families_are_closed_and_round_trip() {
        let mut names = std::collections::BTreeSet::new();
        for family in AdversarialFamily::ALL {
            assert!(
                names.insert(family.as_str()),
                "duplicate adversarial family"
            );
            assert_eq!(AdversarialFamily::parse(family.as_str()), Some(*family));
        }
        assert_eq!(names.len(), 11);
        assert_eq!(AdversarialFamily::parse("aliases"), None);
    }

    #[test]
    fn adversarial_case_inventory_is_exact_closed_and_partitioned() {
        let expected_family_counts = [
            (AdversarialFamily::PathTraversal, 20_usize),
            (AdversarialFamily::SymlinkReparse, 13),
            (AdversarialFamily::RenameRace, 10),
            (AdversarialFamily::PathAliases, 9),
            (AdversarialFamily::HardLinks, 6),
            (AdversarialFamily::MaliciousMetadata, 14),
            (AdversarialFamily::HandleReplay, 16),
            (AdversarialFamily::PartialWrites, 12),
            (AdversarialFamily::ProcessCrash, 7),
            (AdversarialFamily::OutputFlood, 7),
            (AdversarialFamily::Cleanup, 8),
        ];
        let mut ids = std::collections::BTreeSet::new();
        for case in AdversarialCaseId::ALL {
            assert!(ids.insert(case.as_str()), "duplicate adversarial case");
            assert_eq!(AdversarialCaseId::parse(case.as_str()), Some(*case));
        }
        assert_eq!(ids.len(), 122);
        assert_eq!(AdversarialCaseId::parse("TRAV-21"), None);
        for (family, expected) in expected_family_counts {
            assert_eq!(
                AdversarialCaseId::ALL
                    .iter()
                    .filter(|case| case.family() == family)
                    .count(),
                expected,
                "unexpected {} inventory count",
                family.as_str()
            );
        }

        let partition = adversarial_case_partition().collect::<Vec<_>>();
        assert_eq!(partition.len(), AdversarialCaseId::ALL.len());
        let implemented = 64;
        assert_eq!(
            partition
                .iter()
                .filter(|entry| entry.status != AdversarialCaseStatus::Pending)
                .count(),
            implemented
        );
        assert_eq!(
            partition
                .iter()
                .filter(|entry| entry.status == AdversarialCaseStatus::Pending)
                .count(),
            122 - implemented
        );
        assert_eq!(
            AdversarialCaseId::Hlink06.status(),
            AdversarialCaseStatus::Executed
        );
        assert_eq!(
            ADVERSARIAL_DYNAMIC_STDIO_IMPLEMENTED_IDS,
            [AdversarialCaseId::Sym01, AdversarialCaseId::Hlink01]
        );
        for case in AdversarialCaseId::ALL {
            assert_eq!(
                partition.iter().filter(|entry| entry.id == *case).count(),
                1,
                "{} is not partitioned exactly once",
                case.as_str()
            );
        }
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn dynamic_startup_fixture_replaces_only_the_selected_root() {
        let policy = ArtifactPolicyFixture::create("startup-symlink-fixture")
            .expect("create startup fixture");
        let staging_before = fs::canonicalize(&policy.staging).expect("canonical staging root");
        assert_eq!(
            prepare_artifact_symlink_startup_case(
                &policy,
                ArtifactSymlinkStartupTarget::ImportRoot,
            ),
            Ok(true)
        );
        assert!(
            fs::symlink_metadata(&policy.import)
                .expect("inspect configured import root")
                .file_type()
                .is_symlink()
        );
        assert!(policy.base.join("startup-import-retained").is_dir());
        assert_eq!(
            fs::canonicalize(&policy.staging).expect("canonical staging root after setup"),
            staging_before
        );
    }

    #[test]
    fn dynamic_startup_evidence_rejects_forged_categories_and_capabilities() {
        let exact = record_artifact_dynamic_filesystem_startup_cases(
            ArtifactStartupCaseOutcome::Rejected("invalid any-mcp artifact root"),
            ArtifactStartupCaseOutcome::Rejected("invalid any-mcp staging policy"),
        )
        .expect("record exact startup evidence");
        assert_eq!(
            exact.assert_exact(&[AdversarialCaseId::Sym11, AdversarialCaseId::Sym12]),
            Ok(())
        );
        assert!(
            record_artifact_dynamic_filesystem_startup_cases(
                ArtifactStartupCaseOutcome::Rejected("invalid any-mcp staging policy"),
                ArtifactStartupCaseOutcome::Rejected("invalid any-mcp staging policy"),
            )
            .is_err()
        );
        if cfg!(any(unix, windows)) {
            assert!(
                record_artifact_dynamic_filesystem_startup_cases(
                    ArtifactStartupCaseOutcome::Unsupported,
                    ArtifactStartupCaseOutcome::Rejected("invalid any-mcp staging policy"),
                )
                .is_err()
            );
        }
    }

    #[test]
    fn adversarial_expected_outcomes_compare_exactly() {
        let validation = ExpectedOutcome::ToolError {
            kind: ExpectedToolErrorKind::Validation,
            message: "Invalid artifact argument.",
        };
        assert_eq!(
            validation.assert_matches(ObservedOutcome::ToolError {
                code: "validation",
                message: "Invalid artifact argument.",
            }),
            Ok(())
        );
        assert!(
            validation
                .assert_matches(ObservedOutcome::ToolError {
                    code: "validation",
                    message: "different",
                })
                .is_err()
        );
        assert_eq!(
            ExpectedToolErrorKind::MissingRoots.code(),
            ExpectedToolErrorCode::Validation
        );
        assert_eq!(
            ExpectedToolErrorKind::MissingStaging.as_str(),
            "missing_staging"
        );
        assert_eq!(
            ExpectedOutcome::Http {
                status: 416,
                body: b"invalid range",
            }
            .assert_matches(ObservedOutcome::Http {
                status: 416,
                body: b"invalid range",
            }),
            Ok(())
        );
        assert!(
            ExpectedOutcome::StartupRejected {
                category: "invalid artifact root",
            }
            .assert_matches(ObservedOutcome::Accepted)
            .is_err()
        );
        assert_eq!(
            ExpectedOutcome::MethodNotFound.assert_matches(ObservedOutcome::MethodNotFound),
            Ok(())
        );
        assert_eq!(
            ExpectedOutcome::Accepted.assert_matches(ObservedOutcome::Accepted),
            Ok(())
        );
    }

    #[test]
    fn root_inventory_detects_byte_changes_without_debugging_names() {
        let directory =
            std::env::temp_dir().join(format!("any-mcp-adversarial-inventory-{}", unique_suffix()));
        fs::create_dir_all(directory.join("nested")).expect("create inventory root");
        fs::write(directory.join("nested/source.bin"), b"before").expect("seed inventory root");
        let inventory = RootInventory::capture(&directory).expect("capture inventory");
        assert_eq!(inventory.entry_count(), 2);
        assert_eq!(inventory.assert_unchanged(), Ok(()));
        assert!(!format!("{inventory:?}").contains("source.bin"));
        fs::write(directory.join("nested/source.bin"), b"after!").expect("mutate inventory root");
        assert_eq!(
            inventory.assert_unchanged(),
            Err("artifact root inventory changed".to_owned())
        );
        fs::remove_dir_all(&directory).expect("remove inventory root");
    }

    #[test]
    fn volume_capability_probes_leave_the_private_root_unchanged() {
        let directory =
            std::env::temp_dir().join(format!("any-mcp-adversarial-volume-{}", unique_suffix()));
        fs::create_dir_all(&directory).expect("create volume root");
        let before = RootInventory::capture(&directory).expect("capture empty root");
        let case_folding = probe_volume_case_folding(&directory).expect("case probe");
        let normalization = probe_volume_normalization(&directory).expect("normalization probe");
        assert!(matches!(
            case_folding,
            VolumeCaseFolding::Insensitive | VolumeCaseFolding::Sensitive
        ));
        assert!(matches!(
            normalization,
            VolumeNormalization::Equivalent | VolumeNormalization::Distinct
        ));
        assert_eq!(before.assert_unchanged(), Ok(()));
        fs::remove_dir_all(&directory).expect("remove volume root");
    }

    #[test]
    fn raw_staging_client_is_constructible_and_redacted() {
        let client = RawStagingClient::new().expect("construct raw staging client");
        assert_eq!(format!("{client:?}"), "RawStagingClient");
    }

    #[test]
    fn hard_link_capability_probe_requires_identity_and_count_round_trip() {
        let policy = ArtifactPolicyFixture::create("hard-link-capability-fixture")
            .expect("create hard-link capability fixture");
        let observed = prove_hard_link_capability(policy.import_root())
            .expect("run hard-link capability probe");
        #[cfg(any(unix, windows))]
        assert!(observed);
        #[cfg(not(any(unix, windows)))]
        assert!(!observed);
    }
}
