//! Initialize `anyr` credentials through the headless Anytype CLI.

use std::{
    ffi::{OsStr, OsString},
    fs::{self, OpenOptions},
    future::Future,
    io::{self, Write},
    path::Path,
    process::Stdio,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use anytype::prelude::{AnytypeClient, GrpcCredentials, HttpCredentials, KeyStore};
use tokio::{io::AsyncReadExt, process::Command, time::timeout};

use crate::cli::AppContext;

const DEFAULT_ANYTYPE_CLI: &str = "anytype";
const MAX_CREDENTIAL_OUTPUT_BYTES: u64 = 64 * 1024;
const MAX_CREDENTIAL_LINE_BYTES: usize = 4 * 1024;
const MAX_CREDENTIAL_BYTES: usize = 1024;
const MIN_CREDENTIAL_BYTES: usize = 20;
const MAX_INVITE_LINK_BYTES: usize = 8 * 1024;
const MAX_ACCOUNT_NAME_BYTES: usize = 256;
const CLI_TIMEOUT: Duration = Duration::from_secs(30);

/// Initializes the selected `anyr` keystore from a running Anytype CLI.
///
/// The child process output is consumed only for credential extraction and is
/// never forwarded to `anyr` output or included in errors.
pub async fn handle(ctx: &AppContext, join: Option<&str>, save_env: Option<&Path>) -> Result<()> {
    if let Some(invite) = join {
        validate_invite_link(invite)?;
    }
    if let Some(path) = save_env {
        validate_environment_file_path(path)?;
        if let Some(output_path) = ctx.output.path() {
            validate_distinct_output_paths(path, output_path)?;
        }
    }

    let executable = anytype_cli_executable()?;
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before the Unix epoch")?
        .as_secs();
    let cli_credentials = load_reusable_cli_credentials(None)?;
    let account_name = if cli_credentials.is_none() {
        Some(account_name(std::env::var_os("ANY_USER"), timestamp)?)
    } else {
        None
    };
    let grpc_endpoint = ctx
        .client
        .get_grpc_endpoint()
        .context("gRPC endpoint is not configured")?;
    let process = CliProcess::new(
        executable,
        ctx.client.get_http_endpoint().to_owned(),
        grpc_endpoint,
    );
    let environment_file = save_env.map(|path| EnvironmentFile {
        path,
        http_endpoint: &process.http_endpoint,
        grpc_endpoint: &process.grpc_endpoint,
        keystore_service: ctx.client.get_key_store().service(),
    });

    let http_credentials = initialize_keystore(
        ctx.client.get_key_store(),
        &process,
        timestamp,
        account_name.as_deref(),
        cli_credentials,
        environment_file.as_ref(),
    )
    .await?;
    ctx.client.set_api_key(http_credentials);
    let status = verify_stored_credentials(&ctx.client).await?;

    if let Some(invite) = join {
        join_space(&process, invite).await?;
    }

    ctx.output.emit_json(&serde_json::json!({
        "initialized": true,
        "joined": join.is_some(),
        "status": status,
    }))
}

struct EnvironmentFile<'a> {
    path: &'a Path,
    http_endpoint: &'a str,
    grpc_endpoint: &'a str,
    keystore_service: &'a str,
}

fn validate_environment_file_path(path: &Path) -> Result<()> {
    if path == Path::new("-") {
        bail!("--save-env requires a file path; '-' is not supported");
    }
    if path.as_os_str().is_empty() || path.file_name().is_none() {
        bail!("--save-env must name a file");
    }
    match fs::symlink_metadata(path) {
        Ok(_) => bail!("--save-env destination already exists"),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(_) => bail!("--save-env destination could not be inspected"),
    }
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    match fs::metadata(parent) {
        Ok(metadata) if metadata.is_dir() => Ok(()),
        Ok(_) => bail!("--save-env parent is not a directory"),
        Err(_) => bail!("--save-env parent directory is unavailable"),
    }
}

fn validate_distinct_output_paths(environment_path: &Path, output_path: &Path) -> Result<()> {
    let Some((environment_parent, environment_name)) = resolved_parent_and_name(environment_path)
    else {
        bail!("--save-env and --output must name different files");
    };
    let Some((output_parent, output_name)) = resolved_parent_and_name(output_path) else {
        return Ok(());
    };
    if environment_parent == output_parent && file_names_equal(&environment_name, &output_name) {
        bail!("--save-env and --output must name different files");
    }
    Ok(())
}

fn resolved_parent_and_name(path: &Path) -> Option<(std::path::PathBuf, OsString)> {
    let name = path.file_name()?.to_owned();
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let parent = fs::canonicalize(parent).ok()?;
    Some((parent, name))
}

#[cfg(windows)]
fn file_names_equal(left: &OsStr, right: &OsStr) -> bool {
    left.to_string_lossy()
        .eq_ignore_ascii_case(&right.to_string_lossy())
}

#[cfg(not(windows))]
fn file_names_equal(left: &OsStr, right: &OsStr) -> bool {
    left == right
}

async fn join_space(process: &CliProcess, invite: &str) -> Result<()> {
    process
        .run_status(
            CommandKind::JoinSpace,
            &[OsStr::new("space"), OsStr::new("join"), OsStr::new(invite)],
        )
        .await
        .context("credentials were stored, but Anytype CLI space join failed")
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

fn account_name(value: Option<OsString>, timestamp: u64) -> Result<String> {
    let Some(value) = value else {
        return Ok(format!("bot_{timestamp}"));
    };
    let value = value
        .into_string()
        .map_err(|_| anyhow::anyhow!("ANY_USER must be valid UTF-8"))?;
    if value.is_empty() {
        bail!("ANY_USER must not be empty");
    }
    if value.len() > MAX_ACCOUNT_NAME_BYTES {
        bail!("ANY_USER exceeds the maximum supported length");
    }
    if value.chars().any(char::is_control) {
        bail!("ANY_USER must not contain control characters");
    }
    Ok(value)
}

async fn initialize_keystore(
    keystore: &(impl CredentialStore + Sync),
    process: &CliProcess,
    timestamp: u64,
    account_name: Option<&str>,
    cli_credentials: Option<GrpcCredentials>,
    environment_file: Option<&EnvironmentFile<'_>>,
) -> Result<HttpCredentials> {
    let grpc_credentials = if let Some(credentials) = cli_credentials {
        credentials
    } else {
        let account_name = account_name.context("account name missing for account creation")?;
        let account_output = process
            .run_capture(
                CommandKind::CreateAccount,
                &[
                    OsStr::new("auth"),
                    OsStr::new("create"),
                    OsStr::new(account_name),
                ],
            )
            .await?;
        let account_key = parse_account_key(&account_output)?;
        GrpcCredentials::from_account_key(account_key)
    };
    let account_key = grpc_credentials
        .account_key()
        .context("gRPC account key missing after initialization")?
        .to_owned();

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

    let http_credentials = HttpCredentials::new(http_token.clone());
    store_credential_pair(keystore, &grpc_credentials, &http_credentials)?;
    if let Some(environment_file) = environment_file {
        save_environment_file(environment_file, &account_key, &http_token)?;
    }
    Ok(http_credentials)
}

fn load_reusable_cli_credentials(path: Option<&Path>) -> Result<Option<GrpcCredentials>> {
    let credentials = GrpcCredentials::from_cli_config(path)
        .context("failed to inspect the Anytype CLI account configuration")?;
    credentials
        .as_ref()
        .map(validate_reusable_cli_credentials)
        .transpose()
}

fn validate_reusable_cli_credentials(credentials: &GrpcCredentials) -> Result<GrpcCredentials> {
    let account_id = credentials
        .account_id()
        .filter(|value| valid_credential(value))
        .context("Anytype CLI config does not contain a valid accountId")?
        .to_owned();
    let account_key = credentials
        .account_key()
        .filter(|value| valid_credential(value))
        .context("Anytype CLI config does not contain a valid accountKey")?
        .to_owned();
    Ok(GrpcCredentials::from_account_key(account_key).with_account_id(account_id))
}

fn save_environment_file(
    environment_file: &EnvironmentFile<'_>,
    account_key: &str,
    http_token: &str,
) -> Result<()> {
    let contents = render_environment_file(environment_file, account_key, http_token);
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;

        options.mode(0o600);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt as _;

        options.share_mode(0);
    }
    let mut file = match options.open(environment_file.path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            bail!("credentials were stored, but --save-env refuses to overwrite its destination")
        }
        Err(_) => {
            bail!("credentials were stored, but --save-env could not create its destination")
        }
    };
    if file
        .write_all(contents.as_bytes())
        .and_then(|()| file.sync_all())
        .is_err()
    {
        drop(file);
        if fs::remove_file(environment_file.path).is_err() {
            bail!(
                "credentials were stored, but --save-env failed and its incomplete destination could not be removed"
            );
        }
        bail!(
            "credentials were stored, but --save-env failed; its incomplete destination was removed"
        );
    }
    Ok(())
}

fn render_environment_file(
    environment_file: &EnvironmentFile<'_>,
    account_key: &str,
    http_token: &str,
) -> String {
    let mut output = String::new();
    push_shell_export(&mut output, "ANYTYPE_URL", environment_file.http_endpoint);
    push_shell_export(
        &mut output,
        "ANYTYPE_GRPC_ENDPOINT",
        environment_file.grpc_endpoint,
    );
    push_shell_export(&mut output, "ANYTYPE_KEYSTORE", "env");
    push_shell_export(
        &mut output,
        "ANYTYPE_KEYSTORE_SERVICE",
        environment_file.keystore_service,
    );
    push_shell_export(&mut output, "ANYTYPE_TEST_SPACE_PREFIX", "xtest");
    push_shell_export(&mut output, "ANYTYPE_KEY_HTTP_TOKEN", http_token);
    push_shell_export(&mut output, "ANYTYPE_KEY_ACCOUNT_KEY", account_key);
    output
}

fn push_shell_export(output: &mut String, name: &str, value: &str) {
    output.push_str("export ");
    output.push_str(name);
    output.push_str("='");
    for character in value.chars() {
        if character == '\'' {
            output.push_str("'\"'\"'");
        } else {
            output.push(character);
        }
    }
    output.push_str("'\n");
}

async fn verify_stored_credentials(client: &AnytypeClient) -> Result<serde_json::Value> {
    verify_connectivity_checks(
        async { client.ping_http().await.is_ok() },
        || async { client.ping_grpc().await.is_ok() },
        || {
            let auth = client.auth_status().map_err(|_| ())?;
            Ok((auth.http.is_authenticated(), auth.grpc.is_authenticated()))
        },
    )
    .await
}

async fn verify_connectivity_checks<HttpPing, GrpcPing, GrpcFuture, Status>(
    http_ping: HttpPing,
    grpc_ping: GrpcPing,
    status: Status,
) -> Result<serde_json::Value>
where
    HttpPing: Future<Output = bool>,
    GrpcPing: FnOnce() -> GrpcFuture,
    GrpcFuture: Future<Output = bool>,
    Status: FnOnce() -> std::result::Result<(bool, bool), ()>,
{
    if !http_ping.await {
        bail!("credentials stored, but HTTP verification failed");
    }
    if !grpc_ping().await {
        bail!("credentials stored, but gRPC verification failed");
    }
    let (http_present, grpc_present) = status()
        .map_err(|()| anyhow::anyhow!("credentials stored, but status verification failed"))?;
    if !http_present || !grpc_present {
        bail!("credentials stored, but status verification found missing credentials");
    }
    Ok(serde_json::json!({
        "http": {
            "credentials": "stored",
            "ping": "ok",
        },
        "grpc": {
            "credentials": "stored",
            "ping": "ok",
        },
    }))
}

trait CredentialStore {
    fn snapshot_http(&self) -> Result<HttpCredentials>;
    fn snapshot_grpc(&self) -> Result<GrpcCredentials>;
    fn replace_grpc(&self, credentials: &GrpcCredentials) -> Result<()>;
    fn write_http(&self, credentials: &HttpCredentials) -> Result<()>;
    fn restore_http(&self, credentials: &HttpCredentials) -> Result<()>;
    fn restore_grpc(&self, credentials: &GrpcCredentials) -> Result<()>;
}

impl CredentialStore for KeyStore {
    fn snapshot_http(&self) -> Result<HttpCredentials> {
        Ok(self.get_http_credentials()?)
    }

    fn snapshot_grpc(&self) -> Result<GrpcCredentials> {
        let credentials = self.get_grpc_credentials()?;
        Ok(GrpcCredentials::new(
            credentials.account_id().map(str::to_owned),
            credentials.account_key().map(str::to_owned),
            credentials.session_token().map(str::to_owned),
        ))
    }

    fn replace_grpc(&self, credentials: &GrpcCredentials) -> Result<()> {
        self.clear_grpc_credentials()?;
        self.update_grpc_credentials(credentials)?;
        Ok(())
    }

    fn write_http(&self, credentials: &HttpCredentials) -> Result<()> {
        self.update_http_credentials(credentials)?;
        Ok(())
    }

    fn restore_http(&self, credentials: &HttpCredentials) -> Result<()> {
        self.clear_http_credentials()?;
        self.update_http_credentials(credentials)?;
        Ok(())
    }

    fn restore_grpc(&self, credentials: &GrpcCredentials) -> Result<()> {
        self.clear_grpc_credentials()?;
        self.update_grpc_credentials(credentials)?;
        Ok(())
    }
}

fn store_credential_pair(
    keystore: &impl CredentialStore,
    grpc: &GrpcCredentials,
    http: &HttpCredentials,
) -> Result<()> {
    let prior_http = keystore
        .snapshot_http()
        .map_err(|_| anyhow::anyhow!("failed to snapshot prior HTTP credentials"))?;
    let prior_grpc = keystore
        .snapshot_grpc()
        .map_err(|_| anyhow::anyhow!("failed to snapshot prior gRPC credentials"))?;
    if keystore.replace_grpc(grpc).is_err() {
        return credential_write_failure(
            keystore,
            &prior_http,
            &prior_grpc,
            "failed to store gRPC credentials",
        );
    }
    if keystore.write_http(http).is_err() {
        return credential_write_failure(
            keystore,
            &prior_http,
            &prior_grpc,
            "failed to store HTTP credentials",
        );
    }
    Ok(())
}

fn credential_write_failure(
    keystore: &impl CredentialStore,
    prior_http: &HttpCredentials,
    prior_grpc: &GrpcCredentials,
    failure: &str,
) -> Result<()> {
    let http_failed = keystore.restore_http(prior_http).is_err();
    let grpc_failed = keystore.restore_grpc(prior_grpc).is_err();
    match (http_failed, grpc_failed) {
        (false, false) => {
            bail!("{failure}; prior HTTP and gRPC credentials were restored");
        }
        (true, false) => {
            bail!(
                "{failure}; HTTP rollback failed, gRPC rollback succeeded, and the selected keystore may require manual repair"
            );
        }
        (false, true) => {
            bail!(
                "{failure}; HTTP rollback succeeded, gRPC rollback failed, and the selected keystore may require manual repair"
            );
        }
        (true, true) => {
            bail!(
                "{failure}; HTTP and gRPC rollback failed and the selected keystore may require manual repair"
            );
        }
    }
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
    http_endpoint: String,
    grpc_endpoint: String,
    timeout: Duration,
}

impl CliProcess {
    fn new(executable: OsString, http_endpoint: String, grpc_endpoint: String) -> Self {
        Self {
            executable,
            http_endpoint,
            grpc_endpoint,
            timeout: CLI_TIMEOUT,
        }
    }

    // Only the unix scripted-CLI tests exercise a non-default timeout.
    #[cfg(all(test, unix))]
    fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    fn command(&self, args: &[&OsStr]) -> Command {
        let mut command = Command::new(&self.executable);
        command
            .args(args)
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .env("ANYTYPE_URL", &self.http_endpoint)
            .env("ANYTYPE_GRPC_ENDPOINT", &self.grpc_endpoint)
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
            self.timeout,
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
            return Err(err).with_context(|| {
                format!(
                    "Anytype CLI {} failed while collecting output",
                    kind.description()
                )
            });
        }
        if output.len() as u64 > MAX_CREDENTIAL_OUTPUT_BYTES {
            stop_child(&mut child).await;
            bail!(
                "Anytype CLI {} output exceeded the safety limit",
                kind.description()
            );
        }
        let status = wait_for_child(&mut child, kind, self.timeout).await?;
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
        let status = wait_for_child(&mut child, kind, self.timeout).await?;
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

async fn wait_for_child(
    child: &mut tokio::process::Child,
    kind: CommandKind,
    wait_timeout: Duration,
) -> Result<std::process::ExitStatus> {
    match timeout(wait_timeout, child.wait()).await {
        Ok(Ok(status)) => Ok(status),
        Ok(Err(_)) => {
            stop_child(child).await;
            bail!("failed waiting for Anytype CLI {}", kind.description());
        }
        Err(_) => {
            stop_child(child).await;
            bail!("Anytype CLI {} timed out", kind.description());
        }
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
        cell::{Cell, RefCell},
        fs,
        path::{Path, PathBuf},
        sync::atomic::{AtomicU64, Ordering},
    };

    #[cfg(unix)]
    use anytype::{client::ClientConfig, prelude::AnytypeClient};
    use clap::Parser;

    use super::*;
    use crate::cli::{
        Cli, Commands, HEADLESS_GRPC_ENDPOINT, HEADLESS_HTTP_URL, apply_init_cli_endpoint_defaults,
    };

    const ACCOUNT_KEY: &str =
        "QUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQQ==";
    const ACCOUNT_ID: &str = "QUNDQ09VTlQtSUQtRklYVFVSRS0wMDAx";
    const HTTP_TOKEN: &str = "QkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkI=";
    static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn parses_init_cli_arguments() {
        let cli = Cli::try_parse_from([
            "anyr",
            "init-cli",
            "--join",
            "anytype://invite/?cid=test&key=value",
            "--save-env",
            "/tmp/anyr.env",
        ])
        .expect("parse init-cli");
        match cli.command {
            Commands::InitCli { join, save_env } => {
                assert_eq!(
                    join.as_deref(),
                    Some("anytype://invite/?cid=test&key=value")
                );
                assert_eq!(save_env.as_deref(), Some(Path::new("/tmp/anyr.env")));
            }
            command => panic!("unexpected command: {command:?}"),
        }
    }

    #[test]
    fn clap_endpoint_precedence_runs_in_isolated_processes() {
        assert_endpoint_helper(
            "defaults",
            &format!("{HEADLESS_HTTP_URL}|{HEADLESS_GRPC_ENDPOINT}"),
        );
        assert_endpoint_helper("environment", "http://env.test:41012|http://env.test:41010");
        assert_endpoint_helper(
            "explicit",
            "http://explicit.test:51012|http://explicit.test:51010",
        );
    }

    #[test]
    fn clap_endpoint_helper() {
        let Ok(mode) = std::env::var("ANYR_INIT_ENDPOINT_HELPER_MODE") else {
            return;
        };
        let args = if mode == "explicit" {
            vec![
                "anyr",
                "--url",
                "http://explicit.test:51012",
                "--grpc",
                "http://explicit.test:51010",
                "init-cli",
            ]
        } else {
            vec!["anyr", "init-cli"]
        };
        let mut cli = Cli::try_parse_from(args).expect("parse isolated init-cli");
        apply_init_cli_endpoint_defaults(&mut cli);
        println!(
            "ANYR_INIT_ENDPOINTS={}|{}",
            cli.url.as_deref().expect("resolved HTTP endpoint"),
            cli.grpc.as_deref().expect("resolved gRPC endpoint")
        );
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

    #[test]
    fn honors_valid_any_user_exactly_and_rejects_unsafe_values() {
        let exact = " automation user Ω ";
        assert_eq!(
            account_name(Some(OsString::from(exact)), 42).expect("valid ANY_USER"),
            exact
        );
        assert_eq!(account_name(None, 4242).expect("fallback"), "bot_4242");
        assert!(account_name(Some(OsString::new()), 42).is_err());
        let sensitive = "private-user\ncredential";
        let error = account_name(Some(OsString::from(sensitive)), 42)
            .expect_err("control character must fail")
            .to_string();
        assert!(!error.contains(sensitive));
        assert!(
            account_name(
                Some(OsString::from("x".repeat(MAX_ACCOUNT_NAME_BYTES + 1))),
                42
            )
            .is_err()
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn fake_cli_initializes_env_keystore_saves_environment_and_joins() {
        let temp = TestDir::new();
        let executable = temp.path().join("fake-anytype");
        let script = format!(
            r#"#!/bin/sh
test "$ANYTYPE_URL" = "http://headless.test:31012" || exit 19
test "$ANYTYPE_GRPC_ENDPOINT" = "http://headless.test:31010" || exit 20
case "$1:$2:$3" in
  auth:create:configured-user)
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
        write_executable(&executable, &script);

        let mut config = ClientConfig::default().app_name("anyr-init-cli-test");
        config.keystore = Some("env".to_owned());
        config.keystore_service = Some("anyr-init-cli-test".to_owned());
        let client = AnytypeClient::with_config(config).expect("build test client");
        let process = CliProcess::new(
            executable.into_os_string(),
            "http://headless.test:31012".to_owned(),
            "http://headless.test:31010".to_owned(),
        );
        let environment_path = temp.path().join("anyr.env");
        let environment_file = EnvironmentFile {
            path: &environment_path,
            http_endpoint: &process.http_endpoint,
            grpc_endpoint: &process.grpc_endpoint,
            keystore_service: client.get_key_store().service(),
        };

        let http_credentials = initialize_keystore(
            client.get_key_store(),
            &process,
            4242,
            Some("configured-user"),
            None,
            Some(&environment_file),
        )
        .await
        .expect("initialize keystore");
        assert!(http_credentials.has_creds());
        assert_saved_environment(&environment_path);
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
    async fn fake_cli_reuses_existing_account_without_creating_another() {
        let temp = TestDir::new();
        let executable = temp.path().join("fake-anytype");
        let script = format!(
            r#"#!/bin/sh
case "$1:$2:$3" in
  auth:apikey:create)
    test "$4" = "api_4242" || exit 21
    printf 'Key: {HTTP_TOKEN}\n'
    ;;
  *)
    exit 22
    ;;
esac
"#
        );
        write_executable(&executable, &script);

        let mut config = ClientConfig::default().app_name("anyr-init-cli-reuse-test");
        config.keystore = Some("env".to_owned());
        config.keystore_service = Some("anyr-init-cli-reuse-test".to_owned());
        let client = AnytypeClient::with_config(config).expect("build test client");
        let configured = GrpcCredentials::from_account_key(ACCOUNT_KEY)
            .with_account_id(ACCOUNT_ID)
            .with_session_token("ignored-existing-session-token");
        let reusable = validate_reusable_cli_credentials(&configured)
            .expect("validate existing CLI credentials");
        initialize_keystore(
            client.get_key_store(),
            &test_process(&executable),
            4242,
            None,
            Some(reusable),
            None,
        )
        .await
        .expect("reuse existing CLI account");

        let stored = client
            .get_key_store()
            .get_grpc_credentials()
            .expect("read stored credentials");
        assert_eq!(stored.account_id(), Some(ACCOUNT_ID));
        assert_eq!(stored.account_key(), Some(ACCOUNT_KEY));
        assert_eq!(stored.session_token(), None);
        assert!(
            client
                .get_key_store()
                .get_http_credentials()
                .expect("read HTTP credentials")
                .has_creds()
        );
    }

    #[test]
    fn reusable_cli_config_requires_valid_account_id_and_key() {
        let valid = GrpcCredentials::from_account_key(ACCOUNT_KEY)
            .with_account_id(ACCOUNT_ID)
            .with_session_token("ignored-existing-session-token");
        let reusable =
            validate_reusable_cli_credentials(&valid).expect("valid reusable credentials");
        assert_eq!(reusable.account_id(), Some(ACCOUNT_ID));
        assert_eq!(reusable.account_key(), Some(ACCOUNT_KEY));
        assert_eq!(reusable.session_token(), None);

        assert!(
            validate_reusable_cli_credentials(&GrpcCredentials::from_account_key(ACCOUNT_KEY))
                .is_err()
        );
        assert!(
            validate_reusable_cli_credentials(
                &GrpcCredentials::default().with_account_id(ACCOUNT_ID)
            )
            .is_err()
        );
    }

    #[test]
    fn reusable_cli_config_loader_distinguishes_missing_and_invalid_files() {
        let temp = TestDir::new();
        let config = temp.path().join("config.json");
        assert!(
            load_reusable_cli_credentials(Some(&config))
                .expect("missing config is allowed")
                .is_none()
        );

        fs::write(
            &config,
            format!(
                r#"{{"accountId":"{ACCOUNT_ID}","accountKey":"{ACCOUNT_KEY}","sessionToken":"ignored-existing-session-token"}}"#
            ),
        )
        .expect("write valid CLI config");
        let credentials = load_reusable_cli_credentials(Some(&config))
            .expect("load valid CLI config")
            .expect("config exists");
        assert_eq!(credentials.account_id(), Some(ACCOUNT_ID));
        assert_eq!(credentials.account_key(), Some(ACCOUNT_KEY));
        assert_eq!(credentials.session_token(), None);

        fs::write(&config, "{not-json").expect("replace with malformed config");
        assert!(load_reusable_cli_credentials(Some(&config)).is_err());
    }

    #[test]
    fn environment_file_is_sourceable_and_shell_quotes_values() {
        let temp = TestDir::new();
        let path = temp.path().join("quoted.env");
        let environment_file = EnvironmentFile {
            path: &path,
            http_endpoint: "http://example.test/it's",
            grpc_endpoint: "http://example.test:31010",
            keystore_service: "agent's-service",
        };
        save_environment_file(&environment_file, "account'key", "http'token")
            .expect("save quoted environment");

        let contents = fs::read_to_string(&path).expect("read quoted environment");
        assert!(contents.contains("export ANYTYPE_URL='http://example.test/it'\"'\"'s'"));
        assert!(contents.contains("export ANYTYPE_KEY_HTTP_TOKEN='http'\"'\"'token'"));
        assert!(contents.contains("export ANYTYPE_KEY_ACCOUNT_KEY='account'\"'\"'key'"));
    }

    #[cfg(unix)]
    #[test]
    fn sourcing_environment_file_exports_values_to_child_process() {
        let temp = TestDir::new();
        let path = temp.path().join("sourceable.env");
        let environment_file = EnvironmentFile {
            path: &path,
            http_endpoint: "http://127.0.0.1:31012",
            grpc_endpoint: "http://127.0.0.1:31010",
            keystore_service: "anyr-test",
        };
        save_environment_file(&environment_file, ACCOUNT_KEY, HTTP_TOKEN)
            .expect("save sourceable environment");

        let status = std::process::Command::new("sh")
            .args([
                "-c",
                ". \"$1\" && sh -c 'test \"$ANYTYPE_KEYSTORE\" = env && test \"$ANYTYPE_TEST_SPACE_PREFIX\" = xtest && test -n \"$ANYTYPE_KEY_HTTP_TOKEN\" && test -n \"$ANYTYPE_KEY_ACCOUNT_KEY\"'",
                "sh",
            ])
            .arg(&path)
            .status()
            .expect("source environment file");
        assert!(status.success());
    }

    #[test]
    fn environment_file_refuses_overwrite_without_disclosing_credentials() {
        let temp = TestDir::new();
        let path = temp.path().join("existing.env");
        fs::write(&path, "operator-owned\n").expect("create existing environment file");
        let environment_file = EnvironmentFile {
            path: &path,
            http_endpoint: "http://127.0.0.1:31012",
            grpc_endpoint: "http://127.0.0.1:31010",
            keystore_service: "anyr",
        };

        let error = save_environment_file(&environment_file, ACCOUNT_KEY, HTTP_TOKEN)
            .expect_err("existing environment file must not be replaced")
            .to_string();
        assert!(error.contains("refuses to overwrite"));
        assert!(!error.contains(ACCOUNT_KEY));
        assert!(!error.contains(HTTP_TOKEN));
        assert_eq!(
            fs::read_to_string(path).expect("read preserved destination"),
            "operator-owned\n"
        );
    }

    #[test]
    fn environment_file_path_is_validated_before_credential_generation() {
        let temp = TestDir::new();
        let available = temp.path().join("available.env");
        validate_environment_file_path(&available).expect("available destination");
        assert!(validate_environment_file_path(Path::new("-")).is_err());

        let existing = temp.path().join("private-existing.env");
        fs::write(&existing, "existing\n").expect("create existing destination");
        let existing_error = validate_environment_file_path(&existing)
            .expect_err("existing destination must fail")
            .to_string();
        assert!(existing_error.contains("already exists"));
        assert!(!existing_error.contains("private-existing"));

        let unavailable = temp.path().join("missing-parent").join("private.env");
        let unavailable_error = validate_environment_file_path(&unavailable)
            .expect_err("missing parent must fail")
            .to_string();
        assert!(unavailable_error.contains("parent directory is unavailable"));
        assert!(!unavailable_error.contains("private.env"));

        let aliased_output = temp.path().join(".").join("available.env");
        let alias_error = validate_distinct_output_paths(&available, &aliased_output)
            .expect_err("output must not replace the environment file")
            .to_string();
        assert!(alias_error.contains("must name different files"));
        assert!(!alias_error.contains("available.env"));
        validate_distinct_output_paths(&available, &temp.path().join("result.json"))
            .expect("distinct output path");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn child_failure_withholds_credential_output() {
        let temp = TestDir::new();
        let executable = temp.path().join("failing-anytype");
        let marker = temp.path().join("child-ran");
        let script = format!(
            "#!/bin/sh\nprintf ran > '{}'\nprintf 'Key: {HTTP_TOKEN}\\n'\nexit 7\n",
            marker.display()
        );
        write_executable(&executable, &script);
        let process = test_process(&executable);
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
        assert_eq!(fs::read_to_string(marker).expect("child marker"), "ran");
        assert!(!error.contains(HTTP_TOKEN));
        assert!(error.contains("HTTP token creation"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn oversized_child_output_is_rejected_and_process_is_reaped() {
        let temp = TestDir::new();
        let executable = temp.path().join("overflow-anytype");
        let pid_file = temp.path().join("overflow.pid");
        let script = format!(
            "#!/bin/sh\nprintf '%s' \"$$\" > '{}'\nwhile :; do printf '0123456789abcdef'; done\n",
            pid_file.display()
        );
        write_executable(&executable, &script);
        let process = test_process(&executable).with_timeout(Duration::from_secs(2));

        let error = process
            .run_capture(
                CommandKind::CreateHttpToken,
                &[OsStr::new("auth"), OsStr::new("apikey")],
            )
            .await
            .expect_err("oversized output must fail")
            .to_string();
        assert!(error.contains("safety limit"));
        assert_process_reaped(&pid_file);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn hung_child_is_terminated_and_reaped() {
        let temp = TestDir::new();
        let executable = temp.path().join("hung-anytype");
        let pid_file = temp.path().join("hung.pid");
        let script = format!(
            "#!/bin/sh\nprintf '%s' \"$$\" > '{}'\nexec sleep 60\n",
            pid_file.display()
        );
        write_executable(&executable, &script);
        let process = test_process(&executable).with_timeout(Duration::from_millis(50));

        let error = process
            .run_capture(CommandKind::CreateAccount, &[OsStr::new("auth")])
            .await
            .expect_err("hung child must time out")
            .to_string();
        assert!(error.contains("timed out"));
        assert_process_reaped(&pid_file);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn child_that_closes_stdout_then_hangs_is_terminated_and_reaped() {
        let temp = TestDir::new();
        let executable = temp.path().join("closed-stdout-anytype");
        let pid_file = temp.path().join("closed-stdout.pid");
        let script = format!(
            "#!/bin/sh\nexec 1>&-\nprintf '%s' \"$$\" > '{}'\nexec sleep 60\n",
            pid_file.display()
        );
        write_executable(&executable, &script);
        let process = test_process(&executable).with_timeout(Duration::from_millis(50));

        let error = process
            .run_capture(CommandKind::CreateAccount, &[OsStr::new("auth")])
            .await
            .expect_err("child wait must time out")
            .to_string();
        assert!(error.contains("timed out"));
        assert_process_reaped(&pid_file);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn join_failure_is_redacted_and_preserves_stored_credentials() {
        let temp = TestDir::new();
        let executable = temp.path().join("join-failure-anytype");
        let invite = "https://example.test/private#join-credential";
        let script = "#!/bin/sh\nprintf '%s\\n' \"$3\"\nprintf '%s\\n' \"$3\" >&2\nexit 9\n";
        write_executable(&executable, script);
        let process = test_process(&executable);
        let store = FakeCredentialStore::new(GrpcCredentials::from_token("prior-session-token"));
        store_credential_pair(
            &store,
            &GrpcCredentials::from_account_key(ACCOUNT_KEY),
            &HttpCredentials::new(HTTP_TOKEN),
        )
        .expect("store credential pair");

        let error = join_space(&process, invite)
            .await
            .expect_err("join must fail")
            .to_string();
        assert!(error.contains("credentials were stored"));
        assert!(!error.contains(invite));
        assert!(!error.contains("join-credential"));
        assert_eq!(store.grpc.borrow().account_key(), Some(ACCOUNT_KEY));
        assert_eq!(store.http_marker.borrow().as_str(), "new-http");
        assert_eq!(store.restore_calls.get(), 0);
    }

    #[test]
    fn mutate_then_error_http_write_restores_both_prior_credentials() {
        let prior = GrpcCredentials::new(
            Some("prior-account-id".to_owned()),
            Some("prior-account-key".to_owned()),
            Some("prior-session-token".to_owned()),
        );
        let store = FakeCredentialStore::new(prior);
        store.fail_http.set(true);
        let error = store_credential_pair(
            &store,
            &GrpcCredentials::from_account_key(ACCOUNT_KEY),
            &HttpCredentials::new(HTTP_TOKEN),
        )
        .expect_err("HTTP write must fail")
        .to_string();

        let restored = store.grpc.borrow();
        assert_eq!(restored.account_id(), Some("prior-account-id"));
        assert_eq!(restored.account_key(), Some("prior-account-key"));
        assert_eq!(restored.session_token(), Some("prior-session-token"));
        assert_eq!(store.restore_calls.get(), 1);
        assert_eq!(store.http_restore_calls.get(), 1);
        assert_eq!(store.http_marker.borrow().as_str(), "prior-http");
        assert!(error.contains("prior HTTP and gRPC credentials were restored"));
        assert!(!error.contains(ACCOUNT_KEY));
        assert!(!error.contains(HTTP_TOKEN));
    }

    #[test]
    fn rollback_reports_each_failed_credential_family_without_secrets() {
        for (http_rollback_fails, grpc_rollback_fails, expected) in [
            (true, false, "HTTP rollback failed, gRPC rollback succeeded"),
            (false, true, "HTTP rollback succeeded, gRPC rollback failed"),
            (true, true, "HTTP and gRPC rollback failed"),
        ] {
            let store =
                FakeCredentialStore::new(GrpcCredentials::from_token("prior-session-token"));
            store.fail_http.set(true);
            store.fail_restore_http.set(http_rollback_fails);
            store.fail_restore_grpc.set(grpc_rollback_fails);
            let error = store_credential_pair(
                &store,
                &GrpcCredentials::from_account_key(ACCOUNT_KEY),
                &HttpCredentials::new(HTTP_TOKEN),
            )
            .expect_err("write and configured rollback must fail")
            .to_string();

            assert!(error.contains(expected));
            assert!(error.contains("manual repair"));
            assert!(!error.contains(ACCOUNT_KEY));
            assert!(!error.contains(HTTP_TOKEN));
        }
    }

    #[tokio::test]
    async fn later_verification_failure_preserves_stored_pair() {
        let store = FakeCredentialStore::new(GrpcCredentials::from_token("prior-session-token"));
        store_credential_pair(
            &store,
            &GrpcCredentials::from_account_key(ACCOUNT_KEY),
            &HttpCredentials::new(HTTP_TOKEN),
        )
        .expect("store new pair");

        let error = verify_connectivity_checks(
            std::future::ready(false),
            || std::future::ready(true),
            || Ok((true, true)),
        )
        .await
        .expect_err("HTTP ping must fail")
        .to_string();
        assert!(error.contains("credentials stored"));
        assert_eq!(store.grpc.borrow().account_key(), Some(ACCOUNT_KEY));
        assert_eq!(store.http_marker.borrow().as_str(), "new-http");
        assert_eq!(store.restore_calls.get(), 0);
    }

    #[tokio::test]
    async fn verification_reports_only_safe_status_fields() {
        let status = verify_connectivity_checks(
            std::future::ready(true),
            || std::future::ready(true),
            || Ok((true, true)),
        )
        .await
        .expect("verification succeeds");
        assert_eq!(status["http"]["credentials"], "stored");
        assert_eq!(status["http"]["ping"], "ok");
        assert_eq!(status["grpc"]["credentials"], "stored");
        assert_eq!(status["grpc"]["ping"], "ok");
        let encoded = serde_json::to_string(&status).expect("serialize safe status");
        assert!(!encoded.contains(ACCOUNT_KEY));
        assert!(!encoded.contains(HTTP_TOKEN));
    }

    #[tokio::test]
    async fn verification_requires_grpc_ping_and_present_status() {
        let grpc_error = verify_connectivity_checks(
            std::future::ready(true),
            || std::future::ready(false),
            || Ok((true, true)),
        )
        .await
        .expect_err("gRPC ping must be required")
        .to_string();
        assert!(grpc_error.contains("credentials stored"));
        assert!(grpc_error.contains("gRPC verification"));

        let status_error = verify_connectivity_checks(
            std::future::ready(true),
            || std::future::ready(true),
            || Ok((true, false)),
        )
        .await
        .expect_err("credential status must be required")
        .to_string();
        assert!(status_error.contains("missing credentials"));
    }

    struct FakeCredentialStore {
        http_marker: RefCell<String>,
        snapshotted_http_marker: RefCell<Option<String>>,
        grpc: RefCell<GrpcCredentials>,
        fail_grpc: Cell<bool>,
        fail_http: Cell<bool>,
        fail_restore_http: Cell<bool>,
        fail_restore_grpc: Cell<bool>,
        http_restore_calls: Cell<u32>,
        restore_calls: Cell<u32>,
    }

    impl FakeCredentialStore {
        fn new(grpc: GrpcCredentials) -> Self {
            Self {
                http_marker: RefCell::new("prior-http".to_owned()),
                snapshotted_http_marker: RefCell::new(None),
                grpc: RefCell::new(grpc),
                fail_grpc: Cell::new(false),
                fail_http: Cell::new(false),
                fail_restore_http: Cell::new(false),
                fail_restore_grpc: Cell::new(false),
                http_restore_calls: Cell::new(0),
                restore_calls: Cell::new(0),
            }
        }
    }

    impl CredentialStore for FakeCredentialStore {
        fn snapshot_http(&self) -> Result<HttpCredentials> {
            *self.snapshotted_http_marker.borrow_mut() = Some(self.http_marker.borrow().to_owned());
            Ok(HttpCredentials::new("opaque-prior-http-credential"))
        }

        fn snapshot_grpc(&self) -> Result<GrpcCredentials> {
            let credentials = self.grpc.borrow();
            Ok(GrpcCredentials::new(
                credentials.account_id().map(str::to_owned),
                credentials.account_key().map(str::to_owned),
                credentials.session_token().map(str::to_owned),
            ))
        }

        fn replace_grpc(&self, credentials: &GrpcCredentials) -> Result<()> {
            if self.fail_grpc.get() {
                bail!("fixture gRPC write failure");
            }
            *self.grpc.borrow_mut() = GrpcCredentials::new(
                credentials.account_id().map(str::to_owned),
                credentials.account_key().map(str::to_owned),
                credentials.session_token().map(str::to_owned),
            );
            Ok(())
        }

        fn write_http(&self, credentials: &HttpCredentials) -> Result<()> {
            if credentials.has_creds() {
                *self.http_marker.borrow_mut() = "new-http".to_owned();
            }
            if self.fail_http.get() {
                bail!("fixture HTTP write failure");
            }
            Ok(())
        }

        fn restore_http(&self, _credentials: &HttpCredentials) -> Result<()> {
            self.http_restore_calls
                .set(self.http_restore_calls.get() + 1);
            if self.fail_restore_http.get() {
                bail!("fixture HTTP rollback failure");
            }
            let marker = self
                .snapshotted_http_marker
                .borrow()
                .clone()
                .context("fixture HTTP snapshot missing")?;
            *self.http_marker.borrow_mut() = marker;
            Ok(())
        }

        fn restore_grpc(&self, credentials: &GrpcCredentials) -> Result<()> {
            self.restore_calls.set(self.restore_calls.get() + 1);
            if self.fail_restore_grpc.get() {
                bail!("fixture gRPC rollback failure");
            }
            *self.grpc.borrow_mut() = GrpcCredentials::new(
                credentials.account_id().map(str::to_owned),
                credentials.account_key().map(str::to_owned),
                credentials.session_token().map(str::to_owned),
            );
            Ok(())
        }
    }

    #[cfg(unix)]
    fn assert_saved_environment(path: &Path) {
        let environment = fs::read_to_string(path).expect("read environment file");
        assert_eq!(
            environment,
            format!(
                "export ANYTYPE_URL='http://headless.test:31012'\n\
                 export ANYTYPE_GRPC_ENDPOINT='http://headless.test:31010'\n\
                 export ANYTYPE_KEYSTORE='env'\n\
                 export ANYTYPE_KEYSTORE_SERVICE='anyr-init-cli-test'\n\
                 export ANYTYPE_TEST_SPACE_PREFIX='xtest'\n\
                 export ANYTYPE_KEY_HTTP_TOKEN='{HTTP_TOKEN}'\n\
                 export ANYTYPE_KEY_ACCOUNT_KEY='{ACCOUNT_KEY}'\n"
            )
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;

            assert_eq!(
                fs::metadata(path)
                    .expect("environment file metadata")
                    .permissions()
                    .mode()
                    & 0o077,
                0
            );
        }
    }

    fn assert_endpoint_helper(mode: &str, expected: &str) {
        let mut command = std::process::Command::new(
            std::env::current_exe().expect("resolve current test executable"),
        );
        command
            .args([
                "--exact",
                "cli::init_cli::tests::clap_endpoint_helper",
                "--nocapture",
            ])
            .env("ANYR_INIT_ENDPOINT_HELPER_MODE", mode)
            .env_remove("ANYTYPE_URL")
            .env_remove("ANYTYPE_GRPC_ENDPOINT");
        if matches!(mode, "environment" | "explicit") {
            command
                .env("ANYTYPE_URL", "http://env.test:41012")
                .env("ANYTYPE_GRPC_ENDPOINT", "http://env.test:41010");
        }
        let output = command.output().expect("run isolated clap helper");
        assert!(
            output.status.success(),
            "isolated clap helper failed with {}",
            output.status
        );
        let stdout = String::from_utf8(output.stdout).expect("helper output is UTF-8");
        assert!(
            stdout.contains(&format!("ANYR_INIT_ENDPOINTS={expected}")),
            "isolated helper did not report expected endpoints"
        );
    }

    #[cfg(unix)]
    fn test_process(executable: &Path) -> CliProcess {
        CliProcess::new(
            executable.as_os_str().to_owned(),
            "http://127.0.0.1:31012".to_owned(),
            "http://127.0.0.1:31010".to_owned(),
        )
    }

    #[cfg(unix)]
    fn write_executable(path: &Path, contents: &str) {
        use std::os::unix::fs::PermissionsExt;

        fs::write(path, contents).expect("write fake executable");
        let mut permissions = fs::metadata(path)
            .expect("fake executable metadata")
            .permissions();
        permissions.set_mode(0o700);
        fs::set_permissions(path, permissions).expect("make fake executable");
    }

    #[cfg(all(unix, target_os = "linux"))]
    fn assert_process_reaped(pid_file: &Path) {
        let pid = fs::read_to_string(pid_file).expect("read child pid");
        assert!(
            !Path::new("/proc").join(pid).exists(),
            "child process was not reaped"
        );
    }

    #[cfg(all(unix, not(target_os = "linux")))]
    fn assert_process_reaped(_pid_file: &Path) {
        // `Child::kill` followed by `Child::wait` is the portable Unix reaping
        // contract; Linux additionally proves PID disappearance through procfs.
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
