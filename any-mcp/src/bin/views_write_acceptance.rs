// any-mcp - bounded, workflow-oriented MCP server for Anytype
//
// SPDX-FileCopyrightText: 2026 Steve Schoettler
// SPDX-License-Identifier: Apache-2.0

//! Test-only spawned stdio entrypoint for the production-unlinked views-write slice.

#[tokio::main]
async fn main() {
    if let Err(error) = any_mcp::logging::init() {
        eprintln!("any-mcp views-write acceptance: diagnostic setup failed: {error}");
        std::process::exit(1);
    }
    if let Err(error) = any_mcp::collection_member_toolset::serve_acceptance_stdio_from_env().await
    {
        tracing::error!(reason = %error, "views-write acceptance startup or service failure");
        std::process::exit(1);
    }
}
