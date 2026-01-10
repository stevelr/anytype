use crate::cli::AppContext;
use crate::output::OutputFormat;
use anyhow::Result;
use anytype::prelude::*;
use std::io::{self, Write};

pub async fn handle(ctx: &AppContext, args: super::AuthArgs) -> Result<()> {
    match args.command {
        super::AuthCommands::Login { force } => login(ctx, force).await,
        super::AuthCommands::Logout => logout(ctx).await,
        super::AuthCommands::Status => status(ctx).await,
    }
}

async fn login(ctx: &AppContext, force: bool) -> Result<()> {
    ctx.client
        .authenticate_interactive(
            |challenge_id| {
                println!("Challenge ID: {challenge_id}");
                print!("Enter 4-digit code displayed by Anytype: ");
                io::stdout().flush().map_err(|err| AnytypeError::Auth {
                    message: err.to_string(),
                })?;
                let mut code = String::new();
                io::stdin()
                    .read_line(&mut code)
                    .map_err(|err| AnytypeError::Auth {
                        message: err.to_string(),
                    })?;
                Ok(code.trim().to_string())
            },
            force,
        )
        .await?;

    if ctx.output.format() == OutputFormat::Quiet {
        return Ok(());
    }

    let response = serde_json::json!({ "authenticated": true });
    ctx.output.emit_json(&response)
}

async fn logout(ctx: &AppContext) -> Result<()> {
    ctx.client.logout()?;

    if ctx.output.format() == OutputFormat::Quiet {
        return Ok(());
    }

    let response = serde_json::json!({ "authenticated": false });
    ctx.output.emit_json(&response)
}

async fn status(ctx: &AppContext) -> Result<()> {
    let authenticated = ctx.client.load_key(false)?;

    let response = serde_json::json!({
        "authenticated": authenticated,
        "keystore": ctx.keystore.description(),
        "url": ctx.base_url,
    });

    ctx.output.emit_json(&response)
}
