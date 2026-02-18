/*
 * anyr - list, search, and manipulate anytype objects
 * github.com/stevelr/anytype
 *
 * SPDX-FileCopyrightText: 2025-2026 Steve Schoettler
 * SPDX-License-Identifier: Apache-2.0
 */
#![warn(clippy::pedantic)] // experimental
#![warn(clippy::nursery)] // experimental
#![allow(clippy::missing_errors_doc)] // pedantic
#![allow(clippy::missing_const_for_fn)] //  nursery function
#![allow(clippy::must_use_candidate)] // pedantic
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
mod crypto;
mod error;
mod filter;
mod output;

use anyhow::Result;
use clap::Parser;

#[tokio::main]
async fn main() {
    if let Err(err) = run().await {
        let code = error::exit_code(&err);
        eprintln!("{err}");
        std::process::exit(code);
    }
}

async fn run() -> Result<()> {
    let cli = cli::Cli::parse();
    init_tracing(cli.verbose)?;
    cli::run(cli).await
}

#[allow(clippy::unnecessary_wraps)] // may need lerrors later
fn init_tracing(verbose: u8) -> Result<()> {
    use tracing_subscriber::{EnvFilter, fmt};

    let filter = std::env::var("RUST_LOG").map_or_else(
        |_| {
            let level = match verbose {
                0 => "warn",
                1 => "info",
                2 => "debug",
                _ => "trace",
            };
            EnvFilter::new(level)
        },
        EnvFilter::new,
    );

    fmt().with_env_filter(filter).init();
    Ok(())
}
