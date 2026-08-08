// Shared test support for production-process and live Anytype scenarios.

#[cfg(windows)]
pub use any_mcp::artifact_roots::acceptance_owner_private_file;

pub mod live_scenario;
pub mod process;
