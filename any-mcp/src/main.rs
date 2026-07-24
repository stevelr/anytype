// any-mcp - bounded, workflow-oriented MCP server for Anytype
//
// SPDX-FileCopyrightText: 2026 Steve Schoettler
// SPDX-License-Identifier: Apache-2.0

const WORKER_STACK_BYTES: usize = 8 * 1024 * 1024;

fn main() {
    let command = match any_mcp::ProcessCommand::parse(std::env::args_os().skip(1)) {
        Ok(command) => command,
        Err(error) => {
            eprintln!("any-mcp: {error}");
            std::process::exit(1);
        }
    };
    match command {
        any_mcp::ProcessCommand::Version => {
            println!("{}", any_mcp::version_line());
        }
        any_mcp::ProcessCommand::ConfigInit(path) => {
            if let Err(error) = any_mcp::init_config(&path) {
                eprintln!("any-mcp: {error}");
                std::process::exit(1);
            }
            println!("Created any-mcp configuration.");
        }
        any_mcp::ProcessCommand::ConfigCheck(path) => {
            if let Err(error) = any_mcp::check_config(&path) {
                eprintln!("any-mcp: {error}");
                std::process::exit(1);
            }
            println!("any-mcp configuration is valid.");
        }
        any_mcp::ProcessCommand::Serve(arguments) => start_server(arguments),
    }
}

fn start_server(arguments: Vec<std::ffi::OsString>) {
    if let Err(error) = any_mcp::logging::init() {
        eprintln!("any-mcp: diagnostic setup failed: {error}");
        std::process::exit(1);
    }
    let runtime = match production_runtime() {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("any-mcp: runtime setup failed: {error}");
            std::process::exit(1);
        }
    };
    if let Err(error) = runtime.block_on(run(arguments)) {
        tracing::error!(reason = %error, "any-mcp startup or service failure");
        std::process::exit(1);
    }
}

fn production_runtime() -> std::io::Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_stack_size(WORKER_STACK_BYTES)
        .build()
}

async fn run(arguments: Vec<std::ffi::OsString>) -> Result<(), Box<dyn std::error::Error>> {
    // Transport and HTTP configuration are validated before any Anytype
    // credential access, probe, or listener bind.
    let transport = any_mcp::http::TransportSelection::from_env()?;
    let config = any_mcp::RuntimeConfig::from_process_args(arguments)?;
    let protocol_mode = config.protocol_mode;
    match transport {
        any_mcp::http::TransportSelection::Stdio => {
            let runtime = any_mcp::RuntimeContext::start(&config).await?;
            tracing::info!(
                http_available = runtime.startup_status().http_available,
                grpc_available = runtime.startup_status().grpc_available,
                "authenticated Anytype runtime ready"
            );
            any_mcp::serve_stdio(runtime, protocol_mode).await?;
        }
        any_mcp::http::TransportSelection::StreamableHttp(http_config) => {
            let auth =
                any_mcp::http::prepare_http_auth(&http_config, config.startup_timeout).await?;
            let runtime = any_mcp::RuntimeContext::start(&config).await?;
            tracing::info!(
                http_available = runtime.startup_status().http_available,
                grpc_available = runtime.startup_status().grpc_available,
                "authenticated Anytype runtime ready"
            );
            any_mcp::http::serve_http(runtime, protocol_mode, *http_config, auth).await?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
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
                "tests::production_runtime_worker_stack_probe",
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
