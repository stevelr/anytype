mod cli;
mod config;
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

fn init_tracing(verbose: u8) -> Result<()> {
    use tracing_subscriber::{EnvFilter, fmt};

    let filter = if let Ok(filter) = std::env::var("RUST_LOG") {
        EnvFilter::new(filter)
    } else {
        let level = match verbose {
            0 => "warn",
            1 => "info",
            2 => "debug",
            _ => "trace",
        };
        EnvFilter::new(level)
    };

    fmt().with_env_filter(filter).init();
    Ok(())
}
