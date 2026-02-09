/*
 * anyback - backup and restore anytype object archives
 * github.com/stevelr/anytype
 *
 * SPDX-FileCopyrightText: 2025-2026 Steve Schoettler
 * SPDX-License-Identifier: Apache-2.0
 */
#![warn(clippy::pedantic)]
#![warn(clippy::nursery)]
#![allow(clippy::missing_errors_doc)]
#![allow(clippy::missing_const_for_fn)]
#![allow(clippy::must_use_candidate)]
#![warn(clippy::default_trait_access)]
#![warn(clippy::doc_markdown)]
#![warn(clippy::explicit_iter_loop)]
#![warn(clippy::future_not_send)]
#![warn(clippy::implicit_clone)]
#![warn(clippy::literal_string_with_formatting_args)]
#![warn(clippy::match_same_arms)]
#![warn(clippy::option_if_let_else)]
#![warn(clippy::redundant_clone)]
#![warn(clippy::ref_option)]
#![warn(clippy::redundant_closure)]
#![warn(clippy::uninlined_format_args)]
#![warn(clippy::unnecessary_wraps)]
#![warn(clippy::unused_async)]

mod cli;

use anyhow::Result;
use std::io::IsTerminal;

#[tokio::main]
async fn main() {
    if let Err(err) = run().await {
        eprintln!("{err:#}");
        std::process::exit(1);
    }
}

async fn run() -> Result<()> {
    let cli = cli::parse_cli_from_env()?;
    init_tracing(cli.verbose, cli.color)?;
    cli::run(cli).await
}

#[allow(clippy::unnecessary_wraps)]
fn init_tracing(verbose: u8, color: cli::ColorArg) -> Result<()> {
    use tracing_subscriber::{EnvFilter, fmt};

    let filter = std::env::var("RUST_LOG").map_or_else(
        |_| {
            let level = if verbose > 0 { "debug" } else { "warn" };
            EnvFilter::new(level)
        },
        EnvFilter::new,
    );

    let ansi = match color {
        cli::ColorArg::Always => true,
        cli::ColorArg::Never => false,
        cli::ColorArg::Auto => std::io::stderr().is_terminal(),
    };

    fmt().with_env_filter(filter).with_ansi(ansi).init();
    Ok(())
}
