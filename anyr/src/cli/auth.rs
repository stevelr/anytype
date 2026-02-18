use std::io::{self, Write};

use anyhow::Result;
use anytype::prelude::*;
use serde_json::json;

use crate::{cli::AppContext, output::OutputFormat};

pub async fn handle(ctx: &AppContext, args: super::AuthArgs) -> Result<()> {
    match args.command {
        super::AuthCommands::Login { force } => login(ctx, force).await,
        super::AuthCommands::Logout => logout(ctx),
        super::AuthCommands::Status => status(ctx).await,
        super::AuthCommands::SetHttp => set_http(ctx),
        super::AuthCommands::SetGrpc {
            config,
            account_key,
            token,
            bip39,
        } => set_grpc(ctx, config, account_key, token, bip39),
        super::AuthCommands::FindGrpc { .. } => unreachable!("handled before client init"),
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

fn logout(ctx: &AppContext) -> Result<()> {
    ctx.client.logout()?;

    if ctx.output.format() == OutputFormat::Quiet {
        return Ok(());
    }

    let response = serde_json::json!({ "authenticated": false });
    ctx.output.emit_json(&response)
}

async fn status(ctx: &AppContext) -> Result<()> {
    let status = ctx.client.auth_status()?;
    let http_ping = if status.http.is_authenticated() {
        match ctx.client.ping_http().await {
            Ok(()) => "Ping check ok".to_string(),
            Err(e) => format!("Ping failed: {e}"),
        }
    } else {
        "(credentials required)".to_string()
    };
    let grpc_ping = if status.grpc.is_authenticated() {
        match ctx.client.ping_grpc().await {
            Ok(()) => "Ping check ok".to_string(),
            Err(e) => format!("Ping failed: {e}"),
        }
    } else {
        "(credentials required)".to_string()
    };
    ctx.output.emit_json(&json!({
        "status": status,
        "ping": {
            "http": http_ping,
            "grpc": grpc_ping,
        }
    }))
}

fn set_http(ctx: &AppContext) -> Result<()> {
    print!("Enter HTTP API token: ");
    io::stdout().flush()?;
    let mut token = String::new();
    io::stdin().read_line(&mut token)?;
    let token = token.trim();
    if token.is_empty() {
        anyhow::bail!("HTTP API token is empty");
    }
    let creds = HttpCredentials::new(token.to_string());
    ctx.client.get_key_store().update_http_credentials(&creds)?;

    if ctx.output.format() == OutputFormat::Quiet {
        return Ok(());
    }
    let response = serde_json::json!({ "http_credentials": "updated" });
    ctx.output.emit_json(&response)
}

#[derive(serde::Deserialize)]
struct HeadlessConfig {
    #[serde(default, rename = "accountId")]
    account_id: Option<String>,
    #[serde(default, rename = "accountKey")]
    account_key: Option<String>,
    #[serde(default, rename = "sessionToken")]
    session_token: Option<String>,
}

fn set_grpc(
    ctx: &AppContext,
    config: Option<std::path::PathBuf>,
    account_key: bool,
    token: bool,
    bip39: bool,
) -> Result<()> {
    let options = [token, config.is_some(), account_key, bip39]
        .into_iter()
        .filter(|enabled| *enabled)
        .count();
    if options > 1 {
        anyhow::bail!("--token, --config, --account-key, and --bip39 are mutually exclusive");
    }
    let creds = if token {
        print!("Enter gRPC session token: ");
        io::stdout().flush()?;
        let mut token = String::new();
        io::stdin().read_line(&mut token)?;
        let token = token.trim();
        if token.is_empty() {
            anyhow::bail!("gRPC session token is empty");
        }
        GrpcCredentials::from_token(token.to_string())
    } else if account_key {
        print!("Enter gRPC account key: ");
        io::stdout().flush()?;
        let mut account_key = String::new();
        io::stdin().read_line(&mut account_key)?;
        let account_key = account_key.trim();
        if account_key.is_empty() {
            anyhow::bail!("gRPC account key is empty");
        }
        GrpcCredentials::from_account_key(account_key.to_string())
    } else if bip39 {
        print!("Enter BIP39 mnemonic: ");
        io::stdout().flush()?;
        let mut mnemonic = String::new();
        io::stdin().read_line(&mut mnemonic)?;
        let mnemonic = mnemonic.trim();
        if mnemonic.is_empty() {
            anyhow::bail!("BIP39 mnemonic is empty");
        }
        let (account_key, account_id) = crate::crypto::derive_keys_from_mnemonic(mnemonic)?;
        GrpcCredentials::from_account_key(account_key).with_account_id(account_id)
    } else {
        let path = config.ok_or_else(|| {
            anyhow::anyhow!("--config PATH, --account-key, --bip39, or --token is required")
        })?;
        let contents = std::fs::read_to_string(&path)?;
        let cfg: HeadlessConfig = serde_json::from_str(&contents)?;
        let mut creds = GrpcCredentials::default();
        if let Some(account_id) = cfg.account_id {
            creds = creds.with_account_id(account_id);
        }
        if let Some(account_key) = cfg.account_key {
            creds = creds.with_account_key(account_key);
        }
        if let Some(session_token) = cfg.session_token {
            creds = creds.with_session_token(session_token);
        }
        creds
    };

    ctx.client.get_key_store().update_grpc_credentials(&creds)?;

    if ctx.output.format() == OutputFormat::Quiet {
        return Ok(());
    }
    let response = serde_json::json!({ "grpc_credentials": "updated" });
    ctx.output.emit_json(&response)
}

pub async fn find_grpc_cmd(output: &crate::output::Output, program: &str) -> Result<()> {
    match anytype::client::find_grpc(Some(program)).await {
        Some(port) => {
            if output.format() == OutputFormat::Quiet {
                return Ok(());
            }
            output.emit_json(&serde_json::json!({ "port": port }))
        }
        None => anyhow::bail!("No gRPC listener found"),
    }
}
