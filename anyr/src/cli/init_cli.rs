//! Initialize `anyr` credentials through the headless Anytype CLI.

use std::{
    ffi::{OsStr, OsString},
    process::Stdio,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use anytype::prelude::{GrpcCredentials, HttpCredentials, KeyStore};
use tokio::{io::AsyncReadExt, process::Command, time::timeout};

use crate::cli::AppContext;

const DEFAULT_ANYTYPE_CLI: &str = "anytype";
const MAX_CREDENTIAL_OUTPUT_BYTES: u64 = 64 * 1024;
const MAX_CREDENTIAL_LINE_BYTES: usize = 4 * 1024;
const MAX_CREDENTIAL_BYTES: usize = 1024;
const MIN_CREDENTIAL_BYTES: usize = 20;
const MAX_INVITE_LINK_BYTES: usize = 8 * 1024;
const CLI_TIMEOUT: Duration = Duration::from_secs(30);

/// Initializes the selected `anyr` keystore from a running Anytype CLI.
///
/// The child process output is consumed only for credential extraction and is
/// never forwarded to `anyr` output or included in errors.
pub async fn handle(ctx: &AppContext, join: Option<&str>) -> Result<()> {
    if let Some(invite) = join {
        validate_invite_link(invite)?;
    }

    let executable = anytype_cli_executable()?;
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before the Unix epoch")?
        .as_secs();
    let process = CliProcess::new(executable);

    initialize_keystore(ctx.client.get_key_store(), &process, timestamp).await?;

    if let Some(invite) = join {
        process
            .run_status(
                CommandKind::JoinSpace,
                &[OsStr::new("space"), OsStr::new("join"), OsStr::new(invite)],
            )
            .await
            .context("credentials were stored, but Anytype CLI space join failed")?;
    }

    ctx.output.emit_json(&serde_json::json!({
        "initialized": true,
        "joined": join.is_some(),
    }))
}

fn anytype_cli_executable() -> Result<OsString> {
    match std::env::var_os("ANYTYPE_CLI_BIN") {
        Some(executable) if executable.is_empty() => {
            bail!("ANYTYPE_CLI_BIN must not be empty")
        }
        Some(executable) => Ok(executable),
        None => Ok(OsString::from(DEFAULT_ANYTYPE_CLI)),
    }
}

async fn initialize_keystore(
    keystore: &KeyStore,
    process: &CliProcess,
    timestamp: u64,
) -> Result<()> {
    let account_name = format!("bot_{timestamp}");
    let account_output = process
        .run_capture(
            CommandKind::CreateAccount,
            &[
                OsStr::new("auth"),
                OsStr::new("create"),
                OsStr::new(&account_name),
            ],
        )
        .await?;
    let account_key = parse_account_key(&account_output)?;

    let api_name = format!("api_{timestamp}");
    let token_output = process
        .run_capture(
            CommandKind::CreateHttpToken,
            &[
                OsStr::new("auth"),
                OsStr::new("apikey"),
                OsStr::new("create"),
                OsStr::new(&api_name),
            ],
        )
        .await?;
    let http_token = parse_http_token(&token_output)?;

    keystore
        .update_grpc_credentials(&GrpcCredentials::from_account_key(account_key))
        .context("failed to store gRPC credentials in the selected keystore")?;
    keystore
        .update_http_credentials(&HttpCredentials::new(http_token))
        .context("failed to store HTTP credentials in the selected keystore")?;
    Ok(())
}

#[derive(Clone, Copy)]
enum CommandKind {
    CreateAccount,
    CreateHttpToken,
    JoinSpace,
}

impl CommandKind {
    const fn description(self) -> &'static str {
        match self {
            Self::CreateAccount => "account creation",
            Self::CreateHttpToken => "HTTP token creation",
            Self::JoinSpace => "space join",
        }
    }
}

struct CliProcess {
    executable: OsString,
}

impl CliProcess {
    fn new(executable: OsString) -> Self {
        Self { executable }
    }

    fn command(&self, args: &[&OsStr]) -> Command {
        let mut command = Command::new(&self.executable);
        command
            .args(args)
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        command
    }

    async fn run_capture(&self, kind: CommandKind, args: &[&OsStr]) -> Result<Vec<u8>> {
        let mut command = self.command(args);
        command.stdout(Stdio::piped());
        let mut child = command.spawn().with_context(|| {
            format!(
                "failed to start Anytype CLI for {} (set ANYTYPE_CLI_BIN to its executable)",
                kind.description()
            )
        })?;
        let stdout = child
            .stdout
            .take()
            .context("failed to capture Anytype CLI output")?;
        let mut output = Vec::new();
        let read = timeout(
            CLI_TIMEOUT,
            stdout
                .take(MAX_CREDENTIAL_OUTPUT_BYTES + 1)
                .read_to_end(&mut output),
        )
        .await;
        let read = if let Ok(result) = read {
            result.context("failed to read Anytype CLI output")
        } else {
            stop_child(&mut child).await;
            bail!("Anytype CLI {} timed out", kind.description());
        };
        if let Err(err) = read {
            stop_child(&mut child).await;
            return Err(err);
        }
        if output.len() as u64 > MAX_CREDENTIAL_OUTPUT_BYTES {
            stop_child(&mut child).await;
            bail!(
                "Anytype CLI {} output exceeded the safety limit",
                kind.description()
            );
        }
        let status = if let Ok(result) = timeout(CLI_TIMEOUT, child.wait()).await {
            result.context("failed waiting for Anytype CLI")?
        } else {
            stop_child(&mut child).await;
            bail!("Anytype CLI {} timed out", kind.description());
        };
        if !status.success() {
            bail!(
                "Anytype CLI {} failed with status {}; child output was withheld",
                kind.description(),
                status
            );
        }
        Ok(output)
    }

    async fn run_status(&self, kind: CommandKind, args: &[&OsStr]) -> Result<()> {
        let mut command = self.command(args);
        command.stdout(Stdio::null());
        let mut child = command.spawn().with_context(|| {
            format!(
                "failed to start Anytype CLI for {} (set ANYTYPE_CLI_BIN to its executable)",
                kind.description()
            )
        })?;
        let status = if let Ok(result) = timeout(CLI_TIMEOUT, child.wait()).await {
            result.context("failed waiting for Anytype CLI")?
        } else {
            stop_child(&mut child).await;
            bail!("Anytype CLI {} timed out", kind.description());
        };
        if !status.success() {
            bail!(
                "Anytype CLI {} failed with status {}; child output was withheld",
                kind.description(),
                status
            );
        }
        Ok(())
    }
}

async fn stop_child(child: &mut tokio::process::Child) {
    let _ = child.kill().await;
    let _ = child.wait().await;
}

fn parse_account_key(output: &[u8]) -> Result<String> {
    parse_unique_credential(output, "account key", |line| {
        line.strip_prefix('║')?.strip_suffix('║').map(str::trim)
    })
}

fn parse_http_token(output: &[u8]) -> Result<String> {
    parse_unique_credential(output, "HTTP token", |line| {
        line.trim().strip_prefix("Key:").map(str::trim)
    })
}

fn parse_unique_credential(
    output: &[u8],
    label: &str,
    extract: impl Fn(&str) -> Option<&str>,
) -> Result<String> {
    let text = std::str::from_utf8(output)
        .with_context(|| format!("Anytype CLI {label} output was not valid UTF-8"))?;
    let mut found: Option<&str> = None;
    for line in text.lines() {
        if line.len() > MAX_CREDENTIAL_LINE_BYTES {
            bail!("Anytype CLI {label} output contained an oversized line");
        }
        let Some(candidate) = extract(line) else {
            continue;
        };
        if !valid_credential(candidate) {
            continue;
        }
        if found.replace(candidate).is_some() {
            bail!("Anytype CLI {label} output contained multiple credential candidates");
        }
    }
    found
        .map(str::to_owned)
        .with_context(|| format!("Anytype CLI did not return a valid {label}"))
}

fn valid_credential(value: &str) -> bool {
    (MIN_CREDENTIAL_BYTES..=MAX_CREDENTIAL_BYTES).contains(&value.len())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/' | b'='))
}

fn validate_invite_link(invite: &str) -> Result<()> {
    let trimmed = invite.trim();
    if trimmed != invite || invite.is_empty() {
        bail!("--join must be a non-empty invitation link without surrounding whitespace");
    }
    if invite.len() > MAX_INVITE_LINK_BYTES {
        bail!("--join invitation link exceeds the maximum supported length");
    }
    if invite.chars().any(char::is_control) {
        bail!("--join invitation link must not contain control characters");
    }
    if !["anytype://", "http://", "https://"]
        .iter()
        .any(|scheme| invite.starts_with(scheme) && invite.len() > scheme.len())
    {
        bail!("--join must use an anytype, http, or https invitation link");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::{Path, PathBuf},
        sync::atomic::{AtomicU64, Ordering},
    };

    use anytype::{client::ClientConfig, prelude::AnytypeClient};
    use clap::Parser;

    use super::*;
    use crate::cli::{Cli, Commands};

    const ACCOUNT_KEY: &str =
        "QUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQQ==";
    const HTTP_TOKEN: &str = "QkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkI=";
    static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn parses_init_cli_arguments() {
        let cli = Cli::try_parse_from([
            "anyr",
            "init-cli",
            "--join",
            "anytype://invite/?cid=test&key=value",
        ])
        .expect("parse init-cli");
        match cli.command {
            Commands::InitCli { join } => {
                assert_eq!(
                    join.as_deref(),
                    Some("anytype://invite/?cid=test&key=value")
                );
            }
            command => panic!("unexpected command: {command:?}"),
        }
    }

    #[test]
    fn parses_only_unique_bounded_credentials() {
        let account = format!("header\n║ {ACCOUNT_KEY} ║\nfooter\n");
        assert_eq!(
            parse_account_key(account.as_bytes()).expect("account key"),
            ACCOUNT_KEY
        );
        let token = format!("created\nKey: {HTTP_TOKEN}\n");
        assert_eq!(
            parse_http_token(token.as_bytes()).expect("HTTP token"),
            HTTP_TOKEN
        );

        let duplicate = format!("Key: {HTTP_TOKEN}\nKey: {ACCOUNT_KEY}\n");
        assert!(parse_http_token(duplicate.as_bytes()).is_err());
        assert!(parse_http_token(b"Key: short\n").is_err());
        assert!(parse_account_key(&[0xff]).is_err());
    }

    #[test]
    fn validates_join_links_without_echoing_values() {
        assert!(validate_invite_link("anytype://invite/?cid=c&key=k").is_ok());
        assert!(validate_invite_link("https://example.test/cid#key").is_ok());
        assert!(validate_invite_link("not-an-invite").is_err());
        let sensitive = "https://example.test/private#credential";
        let error = validate_invite_link(&format!(" {sensitive}"))
            .expect_err("surrounding whitespace must fail")
            .to_string();
        assert!(!error.contains(sensitive));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn fake_cli_initializes_selected_keystore_and_joins() {
        use std::os::unix::fs::PermissionsExt;

        let temp = TestDir::new();
        let executable = temp.path().join("fake-anytype");
        let script = format!(
            r#"#!/bin/sh
case "$1:$2:$3" in
  auth:create:bot_4242)
    printf 'account\n║ {ACCOUNT_KEY} ║\n'
    ;;
  auth:apikey:create)
    test "$4" = "api_4242" || exit 21
    printf 'Key: {HTTP_TOKEN}\n'
    ;;
  space:join:https://example.test/cid#key)
    ;;
  *)
    exit 22
    ;;
esac
"#
        );
        fs::write(&executable, script).expect("write fake executable");
        let mut permissions = fs::metadata(&executable)
            .expect("fake executable metadata")
            .permissions();
        permissions.set_mode(0o700);
        fs::set_permissions(&executable, permissions).expect("make fake executable");

        let key_path = temp.path().join("credentials.db");
        let mut config = ClientConfig::default().app_name("anyr-init-cli-test");
        config.keystore = Some(format!("file:path={}", key_path.display()));
        config.keystore_service = Some("anyr-init-cli-test".to_owned());
        let client = AnytypeClient::with_config(config).expect("build test client");
        let process = CliProcess::new(executable.into_os_string());

        initialize_keystore(client.get_key_store(), &process, 4242)
            .await
            .expect("initialize keystore");
        process
            .run_status(
                CommandKind::JoinSpace,
                &[
                    OsStr::new("space"),
                    OsStr::new("join"),
                    OsStr::new("https://example.test/cid#key"),
                ],
            )
            .await
            .expect("join through fake CLI");

        assert!(
            client
                .get_key_store()
                .get_http_credentials()
                .expect("read HTTP credentials")
                .has_creds()
        );
        assert_eq!(
            client
                .get_key_store()
                .get_grpc_credentials()
                .expect("read gRPC credentials")
                .account_key(),
            Some(ACCOUNT_KEY)
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn child_failure_withholds_credential_output() {
        use std::os::unix::fs::PermissionsExt;

        let temp = TestDir::new();
        let executable = temp.path().join("failing-anytype");
        let script = format!("#!/bin/sh\nprintf 'Key: {HTTP_TOKEN}\\n'\nexit 7\n");
        fs::write(&executable, script).expect("write fake executable");
        let mut permissions = fs::metadata(&executable)
            .expect("fake executable metadata")
            .permissions();
        permissions.set_mode(0o700);
        fs::set_permissions(&executable, permissions).expect("make fake executable");

        let process = CliProcess::new(executable.into_os_string());
        let error = process
            .run_capture(
                CommandKind::CreateHttpToken,
                &[
                    OsStr::new("auth"),
                    OsStr::new("apikey"),
                    OsStr::new("create"),
                    OsStr::new("api_4242"),
                ],
            )
            .await
            .expect_err("failed child must fail")
            .to_string();
        assert!(!error.contains(HTTP_TOKEN));
        assert!(error.contains("status"));
    }

    struct TestDir {
        path: PathBuf,
    }

    impl TestDir {
        fn new() -> Self {
            let unique = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "anyr-init-cli-test-{}-{unique}",
                std::process::id()
            ));
            fs::create_dir(&path).expect("create test directory");
            Self { path }
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}
