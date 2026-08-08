// any-mcp - bounded, workflow-oriented MCP server for Anytype
//
// SPDX-FileCopyrightText: 2026 Steve Schoettler
// SPDX-License-Identifier: Apache-2.0

//! Bounded, workflow-oriented MCP server foundations for Anytype.
//!
//! The embedded MCP process owns one authenticated
//! [`anytype`](https://docs.rs/anytype) client and serves strict tools and
//! document resources over stdio. It is a curated workflow surface, not a
//! one-for-one Anytype API mirror, and never depends directly on generated
//! `anytype-rpc` support.
//!
//! # Production defaults
//!
//! - [`ProtocolMode::Stable`] uses the released MCP `2025-11-25`
//!   initialize/initialized lifecycle. The compiled stateless `2026-07-28`
//!   adapter requires an explicit experimental environment selector.
//! - [`ApplicationProfile::Compact`] advertises `server_status`,
//!   `object_search`, `object_get`, and `object_edit`; read-only compact omits
//!   `object_edit`.
//! - [`ApplicationProfile::Standard`] advertises the complete fourteen-tool
//!   Phase 1 catalog; read-only standard retains its ten read tools.
//! - [`OptionalToolsetSelection`] is empty unless `ANY_MCP_TOOLSETS` names a
//!   complete registry linked into the binary. The default-off `members`,
//!   `files`, nine-tool `schema`, three-tool `views-write`, and six-tool
//!   `chats` registries are linked; acceptance-blocked `discussions` remains
//!   unavailable and Phase 1 remains the default catalog.
//! - Resources advertise only the canonical
//!   `anytype://spaces/{space_id}/objects/{object_id}` template. Instance
//!   listing is empty and document discovery remains paginated through
//!   `object_search`.
//!
//! ```
//! use any_mcp::{ApplicationProfile, ProtocolMode};
//!
//! assert_eq!(ApplicationProfile::default(), ApplicationProfile::Compact);
//! assert_eq!(ProtocolMode::default(), ProtocolMode::Stable);
//! ```
//!
//! # Startup and safety
//!
//! [`RuntimeConfig::from_env`] validates exact protocol, profile, read-only,
//! timeout, concurrency, response-budget, optional-registry, and selected TOML
//! policy settings without echoing invalid values. [`RuntimeContext::start`]
//! loads existing Anytype credentials, performs bounded authenticated health
//! checks, and freezes canonical configured space authority. HTTP is always required;
//! standard read-write also requires gRPC for verified archive readback, while
//! compact and read-only selections may run HTTP-only unless `schema` or
//! `views-write` is selected, because their bounded type classification or
//! canonical membership evidence requires gRPC through `anytype-api`.
//! [`serve_stdio`] reserves
//! stdout for protocol frames; redacted diagnostics go to stderr.
//!
//! Every handler runs under shared concurrency, timeout, cancellation, response
//! byte, schema, and result bounds. Read-only mode removes mutation tools and
//! rejects stale direct calls before decoding or I/O. Once a mutation may have
//! been dispatched, uncertain outcomes require rereading before retry rather
//! than claiming failure or replaying a write.
//!
//! The crate README contains current host registration, complete tool semantics,
//! protocol compatibility, token baselines, and operational guidance.

mod artifact_client_roots;
mod artifact_acceptance_gates;
pub mod artifact_config;
pub mod artifact_roots;
mod artifact_staging;
pub mod artifact_toolset;
mod artifact_validators;
pub mod body_toolset;
pub mod chat_add_toolset;
pub mod chat_delete_toolset;
pub mod chat_read_toolset;
pub mod chats_toolset;
pub mod collection_member_toolset;
pub mod config;
mod create_idempotency;
pub mod cursor;
pub mod discovery;
pub mod discussion_toolset;
pub mod domain;
pub mod error;
mod file_content;
pub mod filters;
pub mod handler_support;
pub mod http;
pub mod logging;
pub mod member_toolset;
pub mod mutation_value;
pub mod object_archive;
pub mod object_create;
pub mod object_edit;
pub mod object_output;
pub mod object_read;
pub mod object_update;
pub mod optional_toolsets;
pub mod pagination;
mod preview;
pub mod process_command;
pub mod protocol;
pub mod resources;
pub mod result;
pub mod runtime;
pub mod schema;
pub mod schema_property_toolset;
pub mod schema_space_toolset;
pub mod schema_tag_toolset;
pub mod schema_toolset;
pub mod schema_type_toolset;
pub mod server;
pub mod space_policy;
mod stdio;
pub mod validation;
pub mod view_handlers;

#[cfg(test)]
mod skill_examples;

pub use artifact_config::{
    AbsoluteNativePath, ArtifactConfig, ArtifactConfigError, ArtifactLimits, ConfigSelector,
    LogicalRootId, RelativeNativePath, SpaceConfig, SpaceReference, StagingConfig, ValidatorConfig,
    ValidatorDriver,
};
#[cfg(any(test, feature = "acceptance-harness"))]
pub use artifact_acceptance_gates::{
    ArtifactAcceptanceGateError, ArtifactAcceptanceGateLease, ArtifactAcceptanceGatePoint,
    ArtifactAcceptanceGates,
};
pub use artifact_roots::{
    AnchoredImport, AtomicExport, EffectiveRootRegistry, ROOTS_REQUIRED_GUIDANCE, RootAccessError,
    RootAccessErrorKind, RootCapabilityKind, RootRegistry,
};
pub use config::{ApplicationProfile, ProtocolMode, RuntimeConfig};
pub use optional_toolsets::{OptionalToolsetSelection, ToolsetName};
pub use process_command::{
    ProcessCommand, ProcessCommandError, check_config, init_config, version_line,
};
pub use runtime::{
    OperationContext, RuntimeContext, RuntimeError, ServeError, StartupError, StartupStatus,
    serve_stdio,
};
pub use server::{AnyMcpServer, ServerBuildError};
pub use space_policy::{
    ConfigurationGeneration, PolicyClient, SpaceAuthority, SpacePolicy, SpacePolicyError,
};

const WORKER_STACK_BYTES: usize = 8 * 1024 * 1024;

/// Run the any-mcp process command and return a process exit status.
///
/// The `Serve` branch creates the production runtime with the historical
/// 8 MiB worker stacks. The caller owns process termination so this entrypoint
/// can be embedded by `anyr` without parsing or duplicating MCP flags.
pub fn run_process<I>(arguments: I) -> std::process::ExitCode
where
    I: IntoIterator<Item = std::ffi::OsString>,
{
    run_process_with_keystore_override(arguments, None)
}

/// Run the any-mcp process while overriding the selected Anytype keystore.
///
/// This is used by `anyr` to forward its global `--keystore` option. The
/// explicit selector takes precedence over the selected MCP TOML configuration
/// and environment.
pub fn run_process_with_keystore_override<I>(
    arguments: I,
    keystore: Option<String>,
) -> std::process::ExitCode
where
    I: IntoIterator<Item = std::ffi::OsString>,
{
    let command = match ProcessCommand::parse(arguments) {
        Ok(command) => command,
        Err(error) => {
            eprintln!("any-mcp: {error}");
            return std::process::ExitCode::FAILURE;
        }
    };

    match command {
        ProcessCommand::Version => {
            println!("{}", version_line());
            std::process::ExitCode::SUCCESS
        }
        ProcessCommand::ConfigInit(path) => match init_config(&path) {
            Ok(()) => {
                println!("Created any-mcp configuration.");
                std::process::ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!("any-mcp: {error}");
                std::process::ExitCode::FAILURE
            }
        },
        ProcessCommand::ConfigCheck(path) => match check_config(&path) {
            Ok(()) => {
                println!("any-mcp configuration is valid.");
                std::process::ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!("any-mcp: {error}");
                std::process::ExitCode::FAILURE
            }
        },
        ProcessCommand::Serve(arguments) => start_server(arguments, keystore),
    }
}

fn start_server(
    arguments: Vec<std::ffi::OsString>,
    keystore: Option<String>,
) -> std::process::ExitCode {
    if let Err(error) = logging::init() {
        eprintln!("any-mcp: diagnostic setup failed: {error}");
        return std::process::ExitCode::FAILURE;
    }
    let runtime = match production_runtime() {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("any-mcp: runtime setup failed: {error}");
            return std::process::ExitCode::FAILURE;
        }
    };
    if let Err(error) = runtime.block_on(run_server(arguments, keystore)) {
        tracing::error!(reason = %error, "any-mcp startup or service failure");
        return std::process::ExitCode::FAILURE;
    }
    std::process::ExitCode::SUCCESS
}

fn production_runtime() -> std::io::Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_stack_size(WORKER_STACK_BYTES)
        .build()
}

async fn run_server(
    arguments: Vec<std::ffi::OsString>,
    keystore: Option<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    // Transport and HTTP configuration are validated before any Anytype
    // credential access, probe, or listener bind.
    let transport = http::TransportSelection::from_env()?;
    let config = RuntimeConfig::from_process_args_with_keystore_override(arguments, keystore)?;
    let protocol_mode = config.protocol_mode;
    match transport {
        http::TransportSelection::Stdio => {
            let runtime = RuntimeContext::start(&config).await?;
            tracing::info!(
                http_available = runtime.startup_status().http_available,
                grpc_available = runtime.startup_status().grpc_available,
                "authenticated Anytype runtime ready"
            );
            serve_stdio(runtime, protocol_mode).await?;
        }
        http::TransportSelection::StreamableHttp(http_config) => {
            let auth = http::prepare_http_auth(&http_config, config.startup_timeout).await?;
            let runtime = RuntimeContext::start(&config).await?;
            tracing::info!(
                http_available = runtime.startup_status().http_available,
                grpc_available = runtime.startup_status().grpc_available,
                "authenticated Anytype runtime ready"
            );
            http::serve_http(runtime, protocol_mode, *http_config, auth).await?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod process_tests {
    use std::{hint::black_box, process::Command, thread};

    const STACK_PROBE_FRAME_BYTES: usize = 16 * 1024;
    const STACK_PROBE_DEPTH: usize = 192;

    #[inline(never)]
    fn use_worker_stack(depth: usize) -> usize {
        let frame = [depth as u8; STACK_PROBE_FRAME_BYTES];
        black_box(&frame);
        let nested = if depth == 0 {
            0
        } else {
            use_worker_stack(depth - 1)
        };
        black_box(frame[depth % frame.len()] as usize).wrapping_add(nested)
    }

    #[test]
    fn production_runtime_worker_contract_survives_isolated_stack_probe() {
        let executable = std::env::current_exe().expect("locate any-mcp test executable");
        let status = Command::new(executable)
            .args([
                "--ignored",
                "--exact",
                "process_tests::production_runtime_worker_stack_probe",
            ])
            .env_remove("RUST_MIN_STACK")
            .status()
            .expect("spawn isolated production runtime stack probe");
        assert!(status.success(), "production runtime stack probe failed");
    }

    #[test]
    #[ignore = "runs in an isolated subprocess from the worker-contract test"]
    fn production_runtime_worker_stack_probe() {
        assert_eq!(super::WORKER_STACK_BYTES, 8 * 1024 * 1024);
        let caller = thread::current().id();
        let runtime = super::production_runtime().expect("build production runtime");
        assert_eq!(
            runtime.handle().runtime_flavor(),
            tokio::runtime::RuntimeFlavor::MultiThread
        );
        let (worker, used) = runtime
            .block_on(async {
                tokio::spawn(async move {
                    (thread::current().id(), use_worker_stack(STACK_PROBE_DEPTH))
                })
                .await
            })
            .expect("production worker stack probe task");
        assert_ne!(
            worker, caller,
            "stack probe must execute on a runtime worker"
        );
        assert_ne!(used, 0, "stack probe frames remain live through recursion");
    }
}
