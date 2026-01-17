use anyhow::Context;
use anyhow::Result;
use anytype::prelude::*;
use clap::{Parser, Subcommand};
use regex::Regex;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

mod logging;
use logging::init_logging;

const CLI_KEY_SERVICE_NAME: &str = "any-edit";

#[derive(Debug, Parser)]
#[command(name = "any-edit")]
#[command(about = "Edit Anytype objects as markdown in external editor", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,

    /// path to key file
    #[arg(long, value_name = "PATH", global = true)]
    keyfile_path: Option<PathBuf>,

    /// API endpoint URL. Default: environment $ANYTYPE_URL or http://127.0.0.1:31009 (desktop app)
    #[arg(short, long)]
    url: Option<String>,

    /// increase verbosity
    #[arg(short, long)]
    verbose: bool,

    /// enable debug logging
    #[arg(short, long)]
    debug: bool,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Authentication commands
    Auth {
        #[command(subcommand)]
        command: AuthCommand,
    },

    /// Get an object as markdown file
    Get {
        /// Space ID (required unless using --doc)
        #[arg(required_unless_present = "doc")]
        space_id: Option<String>,

        /// Object ID (required unless using --doc)
        #[arg(required_unless_present = "doc")]
        object_id: Option<String>,

        /// Output file (default: stdout)
        #[arg(short, long)]
        output: Option<PathBuf>,

        /// Parse document URL to get space_id and object_id
        #[arg(short, long)]
        doc: Option<String>,
    },

    /// Update an object from markdown file
    Update {
        /// Input file (default: stdin)
        #[arg(short, long)]
        input: Option<PathBuf>,
    },

    /// (macOS) Send keystroke to Anytype to copy current object link, output the URL
    #[cfg(target_os = "macos")]
    CopyLink {
        /// Delay in milliseconds after activating Anytype (default: 300)
        #[arg(long, default_value = "300")]
        activate_delay: u64,

        /// Delay in milliseconds after sending keystroke (default: 200)
        #[arg(long, default_value = "200")]
        keystroke_delay: u64,
    },

    /// Get, edit with $EDITOR, and update
    Edit {
        /// Space ID (required unless using --doc)
        #[arg(required_unless_present = "doc")]
        space_id: Option<String>,

        /// Object ID (required unless using --doc)
        #[arg(required_unless_present = "doc")]
        object_id: Option<String>,

        /// Parse document URL to get space_id and object_id
        #[arg(short, long)]
        doc: Option<String>,
    },

    /// Get the current visible object from the app, edit with $EDITOR, and update
    #[cfg(target_os = "macos")]
    EditCurrent {},
}

#[derive(Debug, Subcommand)]
enum AuthCommand {
    /// Start the authentication process
    Login,
    /// Remove stored credentials
    Logout,
    /// Show current authentication status
    Status,
}

#[derive(Debug, Deserialize, Serialize)]
struct YamlHeader {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    space_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    object_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    created_date: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tags: Option<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    init_logging(cli.debug, cli.verbose)?;

    let keystore = if let Some(path) = cli.keyfile_path {
        KeyStoreFile::from_path(path)
    } else {
        KeyStoreFile::new(CLI_KEY_SERVICE_NAME)
    }?;
    let base_url = cli.url.unwrap_or_else(|| ANYTYPE_DESKTOP_URL.to_string());

    let client = AnytypeClient::with_config(ClientConfig {
        base_url,
        app_name: CLI_KEY_SERVICE_NAME.into(),
        ..Default::default()
    })?
    .set_key_store(keystore);

    match cli.command {
        Commands::Auth { command } => match command {
            AuthCommand::Login => auth_login(client).await?,
            AuthCommand::Logout => auth_logout(client).await?,
            AuthCommand::Status => check_auth_status(client).await?,
        },
        Commands::Get {
            space_id,
            object_id,
            output,
            doc,
        } => {
            let (final_space_id, final_object_id) = if let Some(url_str) = doc {
                parse_doc_url(&url_str)?
            } else {
                (
                    space_id.ok_or_else(|| anyhow::anyhow!("space_id is required"))?,
                    object_id.ok_or_else(|| anyhow::anyhow!("object_id is required"))?,
                )
            };

            get_command(
                &client,
                &final_space_id,
                &final_object_id,
                output.as_deref(),
            )
            .await?;
        }
        Commands::Update { input } => update_command(&client, input.as_deref()).await?,

        #[cfg(target_os = "macos")]
        Commands::CopyLink {
            activate_delay,
            keystroke_delay,
        } => copy_link_command(activate_delay, keystroke_delay)?,

        #[cfg(target_os = "macos")]
        Commands::EditCurrent {} => edit_command_current(client).await?,

        Commands::Edit {
            space_id,
            object_id,
            doc,
        } => {
            let (space_id, object_id) = if let Some(url_str) = doc {
                parse_doc_url(&url_str)?
            } else {
                (
                    space_id.ok_or_else(|| anyhow::anyhow!("space_id is required"))?,
                    object_id.ok_or_else(|| anyhow::anyhow!("object_id is required"))?,
                )
            };
            edit_command(client, space_id, object_id).await?
        }
    }
    Ok(())
}

/// Parse document URL to extract space_id and object_id
fn parse_doc_url(url: &str) -> Result<(String, String)> {
    let re = Regex::new(
        r"https://[a-z\.]+/(bafyrei[a-z2-7]+)\?spaceId=(bafyrei[a-z2-7]+\.[a-z0-9]{1,13})",
    )
    .context("Failed to create URL regex")?;

    let captures = re.captures(url).ok_or_else(|| {
        anyhow::anyhow!(
            "Invalid URL format. Expected format: https://[domain]/[object_id]?spaceId=[space_id]"
        )
    })?;

    let object_id = captures
        .get(1)
        .ok_or_else(|| anyhow::anyhow!("Could not extract object_id from URL"))?
        .as_str()
        .to_string();

    let space_id = captures
        .get(2)
        .ok_or_else(|| anyhow::anyhow!("Could not extract space_id from URL"))?
        .as_str()
        .to_string();

    Ok((space_id, object_id))
}

/// Auth login: authenticate with Anytype app
async fn auth_login(client: AnytypeClient) -> Result<(), anyhow::Error> {
    println!("Starting authentication with local Anytype app...");

    client
        .authenticate_interactive(
            |challenge_id| {
                println!("Challenge ID: {}", challenge_id);
                // Prompt user and return their code
                print!("Enter 4-digit code displayed by app: ");
                let mut code = String::new();
                std::io::stdin()
                    .read_line(&mut code)
                    .map_err(|e| AnytypeError::Auth {
                        message: e.to_string(),
                    })?;
                Ok(code.trim().to_string())
            },
            false,
        )
        .await?;

    Ok(())
}

async fn auth_logout(client: AnytypeClient) -> Result<(), AnytypeError> {
    client.logout()?;
    Ok(())
}
async fn check_auth_status(client: AnytypeClient) -> Result<()> {
    client.load_key(false)?;
    let auth = if client.is_authenticated() {
        "yes"
    } else {
        "no"
    };

    println!("Authenticated: {auth}");
    println!("Keystore:      {:?}", client.get_key_store());
    Ok(())
}

/// Get command: retrieve object and output as markdown with YAML header
async fn get_command(
    client: &AnytypeClient,
    space_id: &str,
    object_id: &str,
    output_file: Option<&Path>,
) -> Result<()> {
    client.load_key(false)?;
    if !client.is_authenticated() {
        eprintln!("Not logged in - run 'any-edit auth login' first");
        return Err(AnytypeError::Auth {
            message: "Not logged in".to_string(),
        }
        .into());
    }

    // Fetch object with full body
    let object = client.object(space_id, object_id).get().await?;

    let tags = if let Some(tags) = object.get_property_multi_select("tags")
        && !tags.is_empty()
    {
        Some(
            tags.iter()
                .map(|tag| tag.name.as_str())
                .collect::<Vec<&str>>()
                .join(","),
        )
    } else {
        None
    };
    let name = match &object.name {
        Some(t) if !t.trim().is_empty() => Some(t.trim().to_string()),
        _ => None,
    };

    // Yaml header with
    //   space_id:
    //   object_id:
    //   name:
    //   created_date:
    //   tags:
    let header = YamlHeader {
        space_id: Some(space_id.to_string()),
        object_id: Some(object.id.clone()),
        name,
        created_date: object
            .get_property_date("created_date")
            .map(|d| d.to_rfc3339()),
        tags,
    };
    let output = format!(
        "---\n{}---\n{}",
        &serde_yaml_ng::to_string(&header)?,
        object.markdown.unwrap_or_default()
    );
    // Write output to file or stdout
    if let Some(path) = output_file {
        std::fs::write(path, &output).context(format!("Failed to write to file: {:?}", path))?;
        eprintln!("Object written to: {:?}", path);
    } else {
        print!("{}", output);
    }

    Ok(())
}

/// Update command: read markdown file with YAML header and update object
async fn update_command(client: &AnytypeClient, input_file: Option<&Path>) -> Result<()> {
    client.load_key(false)?;
    if !client.is_authenticated() {
        eprintln!("Not logged in - run 'any-edit auth login' first");
        return Err(AnytypeError::Auth {
            message: "Not logged in".to_string(),
        }
        .into());
    }

    // Read input
    let content = if let Some(path) = input_file {
        std::fs::read_to_string(path).with_context(|| format!("Failed to read file: {:?}", path))?
    } else {
        let mut buffer = String::new();
        io::stdin()
            .read_to_string(&mut buffer)
            .context("Failed to read from stdin")?;
        buffer
    };

    // Parse YAML header and body
    let (header, body) = parse_markdown_with_yaml(&content)?;

    // Extract required fields
    let space_id = header
        .space_id
        .ok_or_else(|| anyhow::anyhow!("space_id is required in YAML header"))?;
    let object_id = header
        .object_id
        .ok_or_else(|| anyhow::anyhow!("object_id is required in YAML header"))?;
    let name = header.name.unwrap_or_default();
    let name = name.trim();

    // fetch original so we can detect if there are changes
    let prev_object = client
        .object(&space_id, &object_id)
        .get()
        .await
        .context("Could not load space_id {space_id} object {object_id}")?;

    let prev_name = prev_object.name.unwrap_or_default();
    let prev_body = prev_object.markdown.as_deref().unwrap_or("").trim();
    let body_changed = prev_body != body.trim_end();
    let name_changed = prev_name.trim() != name && !name.is_empty();

    if name_changed || body_changed {
        println!("document changed .. sending update");
        let mut object = client.update_object(&space_id, &object_id);
        if name_changed {
            object = object.name(name);
        }
        if body_changed {
            object = object.body(body);
        }
        object.update().await?;
    } else {
        println!("no change");
    }

    Ok(())
}

/// Parse markdown content with YAML frontmatter
fn parse_markdown_with_yaml(content: &str) -> Result<(YamlHeader, String)> {
    let lines: Vec<&str> = content.lines().collect();

    // Check if content starts with YAML frontmatter
    if !lines.first().map(|l| *l == "---").unwrap_or(false) {
        return Err(anyhow::anyhow!(
            "Invalid format: content must start with YAML frontmatter (---)"
        ));
    }

    // Find the end of YAML frontmatter
    let yaml_end = lines
        .iter()
        .skip(1)
        .position(|l| *l == "---")
        .ok_or_else(|| anyhow::anyhow!("Invalid format: YAML frontmatter not closed with ---"))?;

    // Extract YAML content (skip first ---, take until second ---)
    let yaml_lines = &lines[1..=yaml_end];
    let yaml_content = yaml_lines.join("\n");

    // Parse YAML header
    let header: YamlHeader =
        serde_yaml_ng::from_str(&yaml_content).context("Failed to parse YAML header")?;

    // Extract body (everything after second --- and any blank lines)
    let body_start = yaml_end + 2; // +1 for 0-index, +1 to skip the closing ---
    let body = if body_start < lines.len() {
        lines[body_start..].join("\n").trim_start().to_string()
    } else {
        String::new()
    };

    Ok((header, body))
}

/// macOS: Send keystroke to Anytype to copy current object link
#[cfg(target_os = "macos")]
fn copy_link_command(activate_delay: u64, keystroke_delay: u64) -> Result<()> {
    let url = copy_link_url(activate_delay, keystroke_delay)?;
    // Output the URL
    println!("{}", url);
    Ok(())
}

#[cfg(target_os = "macos")]
fn copy_link_url(activate_delay: u64, keystroke_delay: u64) -> Result<String> {
    use arboard::Clipboard;
    use enigo::{Direction, Enigo, Key, Keyboard, Settings};
    use std::thread;
    use std::time::Duration;

    let mut clipboard = Clipboard::new().context("Failed to access clipboard")?;

    // Save current clipboard contents
    let saved_clipboard = clipboard.get_text().ok();

    // Future: AFAIK, this is the only part that is mac-os specific: bringing anytype forward to get focus
    // The other parts of this function: submitting the keystroke, and reading/writing clipboard,
    // are done with crates that support linux and windows for these operations.
    //
    // Activate Anytype app
    let status = Command::new("open")
        .args(["-a", "Anytype"])
        .status()
        .context("Failed to activate Anytype app")?;

    if !status.success() {
        anyhow::bail!(
            "Failed to activate Anytype app: exit code {:?}",
            status.code()
        );
    }

    // Wait for app to come to foreground
    thread::sleep(Duration::from_millis(activate_delay));

    // Send Cmd+Option+C keystroke
    let mut enigo = Enigo::new(&Settings {
        open_prompt_to_get_permissions: true,
        ..Default::default()
    })
    .map_err(|e| anyhow::anyhow!("Failed to create Enigo instance: {}", e))?;

    // Press modifiers
    enigo
        .key(Key::Meta, Direction::Press)
        .map_err(|e| anyhow::anyhow!("Failed to press Meta key: {}", e))?;
    enigo
        .key(Key::Alt, Direction::Press)
        .map_err(|e| anyhow::anyhow!("Failed to press Alt key: {}", e))?;

    // Press and release 'c'
    enigo
        .key(Key::Unicode('c'), Direction::Click)
        .map_err(|e| anyhow::anyhow!("Failed to send 'c' key: {}", e))?;

    // Release modifiers
    enigo
        .key(Key::Alt, Direction::Release)
        .map_err(|e| anyhow::anyhow!("Failed to release Alt key: {}", e))?;
    enigo
        .key(Key::Meta, Direction::Release)
        .map_err(|e| anyhow::anyhow!("Failed to release Meta key: {}", e))?;

    // Wait for clipboard to be updated
    thread::sleep(Duration::from_millis(keystroke_delay));

    // Read clipboard
    let url = clipboard
        .get_text()
        .context("Failed to read text from clipboard")?;

    // Restore original clipboard contents
    if let Some(saved) = saved_clipboard {
        let _ = clipboard.set_text(&saved);
    }

    // Validate it looks like an Anytype URL
    if !url.contains("spaceId=") {
        anyhow::bail!(
            "Clipboard does not contain a valid Anytype URL. Got: {}",
            if url.len() > 100 {
                format!("{}...", &url[..100])
            } else {
                url
            }
        );
    }

    Ok(url)
}

#[cfg(target_os = "macos")]
async fn edit_command_current(client: AnytypeClient) -> Result<()> {
    let (space_id, object_id) = {
        let url = copy_link_url(300, 200)?;
        parse_doc_url(&url)?
    };

    edit_command(client, space_id, object_id).await
}

async fn edit_command(client: AnytypeClient, space_id: String, object_id: String) -> Result<()> {
    let tmp_path = temp_markdown_path()?;
    get_command(&client, &space_id, &object_id, Some(&tmp_path)).await?;

    let original_body_hash = sha256_body_hash(&tmp_path)?;
    run_editor(&tmp_path)?;
    let edited_body_hash = sha256_body_hash(&tmp_path)?;

    if original_body_hash == edited_body_hash {
        println!("no changes detected; skipping update");
        let _ = std::fs::remove_file(&tmp_path);
        return Ok(());
    }

    if let Err(err) = update_command(&client, Some(&tmp_path)).await {
        eprintln!("update failed; file preserved at: {:?}", tmp_path);
        return Err(err);
    }

    let _ = std::fs::remove_file(&tmp_path);
    Ok(())
}

fn sha256_body_hash(path: &Path) -> Result<String> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read file: {:?}", path))?;
    let (_header, body) = parse_markdown_with_yaml(&content)?;
    let mut hasher = Sha256::new();
    hasher.update(body.as_bytes());
    let digest = hasher.finalize();
    Ok(format!("{:x}", digest))
}

fn temp_markdown_path() -> Result<PathBuf> {
    let mut path = std::env::temp_dir();
    let pid = std::process::id();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("System time before UNIX_EPOCH")?
        .as_nanos();
    path.push(format!("any-edit-{}-{}.md", pid, nanos));
    Ok(path)
}

fn run_editor(path: &Path) -> Result<()> {
    let status = if let Ok(raw) = std::env::var("EDITOR_COMMAND") {
        let args = parse_editor_command(&raw)?;
        if args.is_empty() {
            anyhow::bail!("EDITOR_COMMAND is empty");
        }
        let mut cmd = Command::new(&args[0]);
        cmd.args(&args[1..]).arg(path);
        cmd.status().context("Failed to launch editor")?
    } else if let Ok(editor) = std::env::var("EDITOR") {
        Command::new(editor)
            .arg(path)
            .status()
            .context("Failed to launch editor")?
    } else {
        anyhow::bail!("EDITOR_COMMAND or EDITOR is required");
    };

    if !status.success() {
        anyhow::bail!("Editor exited with status: {:?}", status.code());
    }

    Ok(())
}

fn parse_editor_command(raw: &str) -> Result<Vec<String>> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut chars = raw.chars().peekable();

    while let Some(ch) = chars.next() {
        match ch {
            '\\' => {
                if let Some(next) = chars.next() {
                    current.push(next);
                } else {
                    current.push('\\');
                }
            }
            ' ' | '\t' | '\n' => {
                if !current.is_empty() {
                    args.push(current);
                    current = String::new();
                }
            }
            _ => current.push(ch),
        }
    }

    if !current.is_empty() {
        args.push(current);
    }

    Ok(args)
}
