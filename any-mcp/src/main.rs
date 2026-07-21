// any-mcp - bounded, workflow-oriented MCP server for Anytype
//
// SPDX-FileCopyrightText: 2026 Steve Schoettler
// SPDX-License-Identifier: Apache-2.0

#[tokio::main]
async fn main() {
    if let Err(error) = any_mcp::logging::init() {
        eprintln!("any-mcp: diagnostic setup failed: {error}");
        std::process::exit(1);
    }
    if let Err(error) = run().await {
        tracing::error!(reason = %error, "any-mcp startup or service failure");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let config = any_mcp::RuntimeConfig::from_env()?;
    let protocol_mode = config.protocol_mode;
    let runtime = any_mcp::RuntimeContext::start(&config).await?;
    tracing::info!(
        http_available = runtime.startup_status().http_available,
        grpc_available = runtime.startup_status().grpc_available,
        "authenticated Anytype runtime ready"
    );
    any_mcp::serve_stdio(runtime, protocol_mode).await?;
    Ok(())
}
