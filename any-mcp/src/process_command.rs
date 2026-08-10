// any-mcp - bounded, workflow-oriented MCP server for Anytype
//
// SPDX-FileCopyrightText: 2026 Steve Schoettler
// SPDX-License-Identifier: Apache-2.0

//! Process-level commands which run without starting the MCP runtime.

use std::{
    ffi::{OsStr, OsString},
    fmt,
    fs::OpenOptions,
    io::{self, Write},
    path::{Path, PathBuf},
};

use crate::artifact_config::ArtifactConfig;

const DEFAULT_CONFIG_FILE: &str = "any-mcp.toml";

/// Valid configuration emitted by `any-mcp config init`.
pub const CONFIG_TEMPLATE: &str = r#"# any-mcp policy
# Select this file with --config ABSOLUTE_PATH or ANY_MCP_CONFIG.
schema_version = 1

[spaces]
# Writable access must be deliberate. Omit allowed to permit every space,
# use allowed = [] to permit none, or list exact space IDs or names.
read_only = false
# allowed = ["space-id-or-name"]

# [[roots.import]]
# id = "inbox"
# path = "/absolute/read-only/import/path"

# [[roots.export]]
# id = "outbox"
# path = "/absolute/create-only/export/path"

# [limits]
# artifact_bytes = 67108864

# [auth]
# Set exactly one keystore selector. `secret-service` is for Linux hosts.
# keystore.file = "/absolute/path/to/keystore.db"
# keystore.secret-service = true
"#;

/// One process-level action selected before logging or runtime construction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProcessCommand {
    /// Start the stdio MCP server with the remaining configuration arguments.
    Serve(Vec<OsString>),
    /// Print the executable and package version.
    Version,
    /// Create a new configuration template.
    ConfigInit(PathBuf),
    /// Validate an existing configuration without starting the server.
    ConfigCheck(PathBuf),
}

impl ProcessCommand {
    /// Parses arguments after the executable name.
    ///
    /// # Errors
    ///
    /// Returns a fixed diagnostic for malformed maintenance or version
    /// commands. Server arguments remain subject to [`crate::ConfigSelector`]
    /// validation when the runtime configuration is loaded.
    pub fn parse<I>(arguments: I) -> Result<Self, ProcessCommandError>
    where
        I: IntoIterator<Item = OsString>,
    {
        let arguments = arguments.into_iter().collect::<Vec<_>>();
        let Some(first) = arguments.first() else {
            return Ok(Self::Serve(Vec::new()));
        };
        if first == OsStr::new("-V") || first == OsStr::new("--version") {
            return if arguments.len() == 1 {
                Ok(Self::Version)
            } else {
                Err(ProcessCommandError::new(CommandProblem::Arguments))
            };
        }
        if first != OsStr::new("config") {
            return Ok(Self::Serve(arguments));
        }

        let Some(subcommand) = arguments.get(1) else {
            return Err(ProcessCommandError::new(CommandProblem::Arguments));
        };
        let path = parse_config_path(&arguments[2..])?;
        if subcommand == OsStr::new("init") {
            Ok(Self::ConfigInit(path))
        } else if subcommand == OsStr::new("check") {
            Ok(Self::ConfigCheck(path))
        } else {
            Err(ProcessCommandError::new(CommandProblem::Arguments))
        }
    }
}

/// Returns the stable version line printed by `-V` and `--version`.
#[must_use]
pub fn version_line() -> String {
    format!("{} {}", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"))
}

/// Creates a new template without replacing any directory entry.
///
/// # Errors
///
/// Returns a fixed, path-redacted diagnostic when the path cannot be resolved,
/// already exists, cannot be created, or cannot be completely written.
pub fn init_config(path: &Path) -> Result<(), ProcessCommandError> {
    let path = absolute_command_path(path)?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;

        options.share_mode(0);
    }
    let mut file = match options.open(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            return Err(ProcessCommandError::new(CommandProblem::Exists));
        }
        Err(_) => return Err(ProcessCommandError::new(CommandProblem::Create)),
    };
    if file
        .write_all(CONFIG_TEMPLATE.as_bytes())
        .and_then(|()| file.sync_all())
        .is_err()
    {
        drop(file);
        let _ = std::fs::remove_file(path);
        return Err(ProcessCommandError::new(CommandProblem::Write));
    }
    #[cfg(windows)]
    if !crate::artifact_roots::windows_security::owner_and_dacl_are_safe(&file).unwrap_or(false) {
        drop(file);
        let _ = std::fs::remove_file(path);
        return Err(ProcessCommandError::new(CommandProblem::Create));
    }
    Ok(())
}

/// Securely opens and validates one configuration without starting Anytype.
///
/// # Errors
///
/// Returns a fixed, path-redacted diagnostic when the path cannot be resolved
/// or the selected file and policy fail normal startup validation.
pub fn check_config(path: &Path) -> Result<(), ProcessCommandError> {
    let path = absolute_command_path(path)?;
    ArtifactConfig::load_file(&path)
        .map(|_| ())
        .map_err(ProcessCommandError::config)
}

fn parse_config_path(arguments: &[OsString]) -> Result<PathBuf, ProcessCommandError> {
    match arguments {
        [] => Ok(PathBuf::from(DEFAULT_CONFIG_FILE)),
        [flag, value]
            if (flag == OsStr::new("-c") || flag == OsStr::new("--config"))
                && !value.is_empty() =>
        {
            Ok(PathBuf::from(value))
        }
        _ => Err(ProcessCommandError::new(CommandProblem::Arguments)),
    }
}

fn absolute_command_path(path: &Path) -> Result<PathBuf, ProcessCommandError> {
    if path.as_os_str().is_empty() {
        return Err(ProcessCommandError::new(CommandProblem::Arguments));
    }
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }
    std::env::current_dir()
        .map(|directory| directory.join(path))
        .map_err(|_| ProcessCommandError::new(CommandProblem::CurrentDirectory))
}

/// Fixed, path-redacted process command failure.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcessCommandError {
    problem: CommandProblem,
}

impl ProcessCommandError {
    const fn new(problem: CommandProblem) -> Self {
        Self { problem }
    }

    const fn config(error: crate::ArtifactConfigError) -> Self {
        Self::new(CommandProblem::Config(error))
    }
}

impl fmt::Display for ProcessCommandError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.problem {
            CommandProblem::Arguments => {
                formatter.write_str("invalid any-mcp command-line arguments")
            }
            CommandProblem::CurrentDirectory => {
                formatter.write_str("unable to resolve the current directory")
            }
            CommandProblem::Exists => {
                formatter.write_str("any-mcp configuration file already exists")
            }
            CommandProblem::Create => {
                formatter.write_str("unable to create any-mcp configuration file")
            }
            CommandProblem::Write => {
                formatter.write_str("unable to write any-mcp configuration file")
            }
            CommandProblem::Config(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for ProcessCommandError {}

#[derive(Clone, Debug, PartialEq, Eq)]
enum CommandProblem {
    Arguments,
    CurrentDirectory,
    Exists,
    Create,
    Write,
    Config(crate::ArtifactConfigError),
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    fn temporary_file(name: &str) -> PathBuf {
        std::env::temp_dir()
            .canonicalize()
            .expect("canonical temporary directory")
            .join(format!(
                "any-mcp-command-{}-{}-{name}",
                std::process::id(),
                getrandom::u64().unwrap_or(0)
            ))
    }

    #[test]
    fn version_forms_are_exact_and_do_not_admit_other_arguments() {
        assert_eq!(
            ProcessCommand::parse([OsString::from("-V")]).expect("short version"),
            ProcessCommand::Version
        );
        assert_eq!(
            ProcessCommand::parse([OsString::from("--version")]).expect("long version"),
            ProcessCommand::Version
        );
        assert!(ProcessCommand::parse([OsString::from("-V"), OsString::from("extra")]).is_err());
        assert_eq!(
            version_line(),
            format!("any-mcp {}", env!("CARGO_PKG_VERSION"))
        );
    }

    #[test]
    fn config_commands_accept_default_short_and_long_paths() {
        assert_eq!(
            ProcessCommand::parse([OsString::from("config"), OsString::from("init")])
                .expect("default init"),
            ProcessCommand::ConfigInit(PathBuf::from(DEFAULT_CONFIG_FILE))
        );
        assert_eq!(
            ProcessCommand::parse([
                OsString::from("config"),
                OsString::from("check"),
                OsString::from("-c"),
                OsString::from("local.toml"),
            ])
            .expect("short check"),
            ProcessCommand::ConfigCheck(PathBuf::from("local.toml"))
        );
        assert_eq!(
            ProcessCommand::parse([
                OsString::from("config"),
                OsString::from("init"),
                OsString::from("--config"),
                OsString::from("/tmp/policy.toml"),
            ])
            .expect("long init"),
            ProcessCommand::ConfigInit(PathBuf::from("/tmp/policy.toml"))
        );
    }

    #[test]
    fn malformed_config_commands_fail_without_becoming_server_arguments() {
        for arguments in [
            vec![OsString::from("config")],
            vec![OsString::from("config"), OsString::from("unknown")],
            vec![
                OsString::from("config"),
                OsString::from("init"),
                OsString::from("-c"),
            ],
            vec![
                OsString::from("config"),
                OsString::from("check"),
                OsString::from("--other"),
                OsString::from("file"),
            ],
        ] {
            assert!(ProcessCommand::parse(arguments).is_err());
        }
    }

    #[test]
    fn init_is_create_new_and_check_uses_startup_validation() {
        let path = temporary_file("policy.toml");
        init_config(&path).expect("create template");
        check_config(&path).expect("validate template");
        let original = fs::read(&path).expect("read template");
        assert_eq!(original, CONFIG_TEMPLATE.as_bytes());

        let error = init_config(&path).expect_err("must not overwrite");
        assert_eq!(
            error.to_string(),
            "any-mcp configuration file already exists"
        );
        assert_eq!(fs::read(&path).expect("read unchanged"), original);

        fs::write(&path, "not valid TOML").expect("replace with invalid content");
        let error = check_config(&path).expect_err("invalid config");
        let diagnostic = error.to_string();
        assert!(diagnostic.starts_with("invalid any-mcp TOML configuration at line 1"));
        assert!(diagnostic.contains("syntax or value does not match the configuration schema"));
        fs::remove_file(path).expect("cleanup");
    }

    #[cfg(unix)]
    #[test]
    fn init_uses_owner_only_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let path = temporary_file("mode.toml");
        init_config(&path).expect("create template");
        let mode = fs::metadata(&path).expect("metadata").permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        fs::remove_file(path).expect("cleanup");
    }
}
