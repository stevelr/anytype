// any-mcp - bounded, workflow-oriented MCP server for Anytype
//
// SPDX-License-Identifier: Apache-2.0

//! Private process wrapper used by any-mcp's spawned integration tests.

fn main() {
    #[cfg(feature = "acceptance-harness")]
    let status = any_mcp::run_acceptance_process(std::env::args_os().skip(1));
    #[cfg(not(feature = "acceptance-harness"))]
    let status = any_mcp::run_process(std::env::args_os().skip(1));
    if status != std::process::ExitCode::SUCCESS {
        std::process::exit(1);
    }
}
