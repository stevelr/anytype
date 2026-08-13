use std::io::{self, Write};

use anyhow::{Context, Result};
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
    let http_present = status.http.is_authenticated();
    let grpc_present = status.grpc.is_authenticated();
    let http_ping = if http_present {
        match ctx.client.ping_http().await {
            Ok(()) => "Ping check ok".to_string(),
            Err(e) => format!("Ping failed: {e}"),
        }
    } else {
        "(credentials required)".to_string()
    };
    let grpc_ping = if grpc_present {
        match ctx.client.ping_grpc().await {
            Ok(()) => "Ping check ok".to_string(),
            Err(e) => format!("Ping failed: {e}"),
        }
    } else {
        "(credentials required)".to_string()
    };
    ctx.output.emit_json(&json!({
        "status": status,
        "credentials": credentials_summary(http_present, grpc_present),
        "ping": {
            "http": http_ping,
            "grpc": grpc_ping,
        }
    }))
}

/// Summarize which credential set (HTTP versus gRPC) is present or missing.
///
/// The 0.5 command surface mixes REST (HTTP) and gRPC backends per command, so
/// `auth status` reports each credential set independently: a command that
/// routes over REST needs the HTTP token, while a gRPC-only command needs the
/// gRPC credentials. Each side carries a `present` flag plus a `detail` string
/// naming the command that provisions the missing set.
fn credentials_summary(http_present: bool, grpc_present: bool) -> serde_json::Value {
    json!({
        "http": {
            "present": http_present,
            "detail": if http_present {
                "HTTP API token present (used by REST commands)"
            } else {
                "HTTP API token missing (run `anyr auth login` or `anyr auth set-http`)"
            },
        },
        "grpc": {
            "present": grpc_present,
            "detail": if grpc_present {
                "gRPC credentials present (used by gRPC-only commands)"
            } else {
                "gRPC credentials missing (run `anyr auth set-grpc`)"
            },
        },
    })
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
        GrpcCredentials::from_cli_config(Some(&path))?.context("headless config not found")?
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

#[cfg(test)]
mod tests {
    use super::credentials_summary;

    #[test]
    fn credentials_summary_marks_both_present() {
        let value = credentials_summary(true, true);
        assert_eq!(value["http"]["present"], true);
        assert_eq!(value["grpc"]["present"], true);
        assert!(
            value["http"]["detail"]
                .as_str()
                .unwrap()
                .contains("present")
        );
        assert!(
            value["grpc"]["detail"]
                .as_str()
                .unwrap()
                .contains("present")
        );
    }

    #[test]
    fn credentials_summary_distinguishes_missing_http() {
        // gRPC credentials present, HTTP missing: the two sets must be reported
        // independently so a REST command's missing token is visible.
        let value = credentials_summary(false, true);
        assert_eq!(value["http"]["present"], false);
        assert_eq!(value["grpc"]["present"], true);
        let http_detail = value["http"]["detail"].as_str().unwrap();
        assert!(http_detail.contains("missing"));
        assert!(http_detail.contains("set-http"));
    }

    #[test]
    fn credentials_summary_distinguishes_missing_grpc() {
        // HTTP token present, gRPC missing: a gRPC-only command's missing
        // credentials must be identified separately from the HTTP set.
        let value = credentials_summary(true, false);
        assert_eq!(value["http"]["present"], true);
        assert_eq!(value["grpc"]["present"], false);
        let grpc_detail = value["grpc"]["detail"].as_str().unwrap();
        assert!(grpc_detail.contains("missing"));
        assert!(grpc_detail.contains("set-grpc"));
    }

    #[test]
    fn credentials_summary_marks_both_missing() {
        let value = credentials_summary(false, false);
        assert_eq!(value["http"]["present"], false);
        assert_eq!(value["grpc"]["present"], false);
    }
}
