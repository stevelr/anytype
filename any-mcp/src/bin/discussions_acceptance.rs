// any-mcp - bounded, workflow-oriented MCP server for Anytype
//
// SPDX-FileCopyrightText: 2026 Steve Schoettler
// SPDX-License-Identifier: Apache-2.0

//! Test-only spawned stdio entrypoint for the production-unlinked discussions slice.

fn main() {
    if let Err(error) = any_mcp::logging::init() {
        eprintln!("any-mcp discussions acceptance: diagnostic setup failed: {error}");
        std::process::exit(1);
    }
    let spawned = std::thread::Builder::new()
        .name("discussions-acceptance-runtime".to_owned())
        .stack_size(16 * 1024 * 1024)
        .spawn(|| {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?;
            runtime
                .block_on(any_mcp::discussion_toolset::serve_acceptance_stdio_from_env())
                .map_err(|_| std::io::Error::other("startup or service failed"))
        });
    let failure = match spawned {
        Ok(thread) => thread
            .join()
            .map_err(|_| "runtime thread failed")
            .and_then(|result| result.map_err(|_| "startup or service failed")),
        Err(_) => Err("runtime thread start failed"),
    };
    if let Err(reason) = failure {
        tracing::error!(reason, "discussions acceptance failure");
        std::process::exit(1);
    }
}
