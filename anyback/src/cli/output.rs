/*
 * anyback_reader - backup command output contract
 * github.com/stevelr/anytype
 *
 * SPDX-FileCopyrightText: 2025-2026 Steve Schoettler
 * SPDX-License-Identifier: Apache-2.0
 */
//! Presentation contract shared between `anyback` commands and the parent CLI
//! that embeds them (`anyr backup ...`).
//!
//! A non-interactive backup command produces one result document.
//! [`CommandOutput`] decides how that document is rendered (compact JSON,
//! indented JSON, human text, or nothing at all) and where it is written
//! (stdout or a file). Only the result document travels through this type;
//! diagnostics, progress, and errors stay on stderr so that stdout remains
//! machine-parseable. Interactive commands render their own terminal UI.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Serialize;

use super::deadline::PublicationCommit;

pub(super) enum PreparedOutput {
    Quiet,
    Stdout(String),
    File { path: PathBuf, stage: PathBuf },
}

impl Drop for PreparedOutput {
    fn drop(&mut self) {
        if let Self::File { stage, .. } = self {
            let _ = fs::remove_file(stage);
        }
    }
}

/// How a backup command result should be presented.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum OutputMode {
    /// Compact single-line JSON. This is the default machine-readable form.
    #[default]
    Json,
    /// Indented, human-readable JSON.
    Pretty,
    /// Plain text summaries and tables intended for a terminal.
    Human,
    /// Suppress all normal output; only the exit status reports the outcome.
    Quiet,
}

impl OutputMode {
    /// Returns the flag-style name of the mode, for diagnostics and errors.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Json => "json",
            Self::Pretty => "pretty",
            Self::Human => "table",
            Self::Quiet => "quiet",
        }
    }

    /// Returns true when the mode renders JSON that callers may parse.
    #[must_use]
    pub const fn is_json(self) -> bool {
        matches!(self, Self::Json | Self::Pretty)
    }
}

/// Destination and presentation contract for a backup command result.
///
/// A successful command emits at most once. File output is replaced only when
/// the result is ready, so validation and command failures preserve any
/// pre-existing destination.
#[derive(Clone, Debug, Default)]
pub struct CommandOutput {
    mode: OutputMode,
    path: Option<PathBuf>,
}

impl CommandOutput {
    /// Creates an output contract for `mode`, writing to `path` when supplied
    /// and to stdout otherwise.
    #[must_use]
    pub fn new(mode: OutputMode, path: Option<PathBuf>) -> Self {
        Self { mode, path }
    }

    /// Creates a compact-JSON contract writing to stdout.
    #[must_use]
    pub fn json() -> Self {
        Self::new(OutputMode::Json, None)
    }

    /// Creates a human-text contract writing to stdout.
    #[must_use]
    pub fn human() -> Self {
        Self::new(OutputMode::Human, None)
    }

    /// The requested presentation mode.
    #[must_use]
    pub const fn mode(&self) -> OutputMode {
        self.mode
    }

    /// The result destination file, or `None` for stdout.
    #[must_use]
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// True when no normal output may be produced.
    #[must_use]
    pub const fn is_quiet(&self) -> bool {
        matches!(self.mode, OutputMode::Quiet)
    }

    /// True when the result is rendered as JSON.
    #[must_use]
    pub const fn is_json(&self) -> bool {
        self.mode.is_json()
    }

    /// True when interactive progress reporting is appropriate: only for
    /// human output going to a terminal that is not competing with a
    /// machine-readable result document.
    #[must_use]
    pub const fn allows_progress(&self) -> bool {
        matches!(self.mode, OutputMode::Human)
    }

    /// Rejects a result destination that aliases a command input or artifact.
    ///
    /// Existing files are compared by filesystem identity, including hard
    /// links. Non-existing paths are compared after resolving their nearest
    /// existing ancestor, so relative paths and symlinked parent directories
    /// cannot bypass the check.
    pub fn ensure_distinct_from(&self, other: &Path, description: &str) -> Result<()> {
        let Some(output_path) = self.path.as_deref() else {
            return Ok(());
        };
        if self.is_quiet() {
            return Ok(());
        }
        if paths_alias(output_path, other)? {
            bail!(
                "result output path {} aliases {description} {}",
                output_path.display(),
                other.display()
            );
        }
        Ok(())
    }

    /// Renders `value` as JSON, honoring compact/indented/quiet modes.
    ///
    /// Use [`Self::emit`] when the command also has a human rendering.
    pub fn emit_json<T: Serialize + ?Sized>(&self, value: &T) -> Result<()> {
        if self.is_quiet() {
            return Ok(());
        }
        let text = match self.mode {
            OutputMode::Pretty | OutputMode::Human => serde_json::to_string_pretty(value)?,
            _ => serde_json::to_string(value)?,
        };
        self.write(&text)
    }

    /// Writes already-rendered text, honoring quiet mode and file routing.
    pub fn emit_text(&self, text: &str) -> Result<()> {
        if self.is_quiet() {
            return Ok(());
        }
        self.write(text)
    }

    /// Emits the result document: `value` in JSON modes, otherwise the text
    /// produced by `render_human`.
    ///
    /// `render_human` is only evaluated when a human rendering is needed, so
    /// expensive formatting is skipped for JSON and quiet output.
    pub fn emit<T, F>(&self, value: &T, render_human: F) -> Result<()>
    where
        T: Serialize + ?Sized,
        F: FnOnce() -> String,
    {
        match self.mode {
            OutputMode::Quiet => Ok(()),
            OutputMode::Json | OutputMode::Pretty => self.emit_json(value),
            OutputMode::Human => self.write(&render_human()),
        }
    }

    pub(super) fn render<T, F>(&self, value: &T, render_human: F) -> Result<Option<String>>
    where
        T: Serialize + ?Sized,
        F: FnOnce() -> String,
    {
        let text = match self.mode {
            OutputMode::Quiet => return Ok(None),
            OutputMode::Json => serde_json::to_string(value)?,
            OutputMode::Pretty => serde_json::to_string_pretty(value)?,
            OutputMode::Human => render_human(),
        };
        Ok(Some(text))
    }

    pub(super) fn prepare_rendered(&self, data: String) -> Result<PreparedOutput> {
        let mut text = data;
        if !text.ends_with('\n') {
            text.push('\n');
        }
        let Some(path) = self.path.as_deref() else {
            return Ok(PreparedOutput::Stdout(text));
        };

        let (mut stage, stage_path) = create_staging_file(path)?;
        let staged = stage
            .write_all(text.as_bytes())
            .and_then(|()| stage.sync_all());
        drop(stage);
        if let Err(error) = staged {
            let _ = fs::remove_file(&stage_path);
            return Err(error)
                .with_context(|| format!("failed to stage output file {}", path.display()));
        }
        Ok(PreparedOutput::File {
            path: path.to_path_buf(),
            stage: stage_path,
        })
    }

    pub(super) fn commit_prepared(
        mut prepared: PreparedOutput,
        authority: PublicationCommit,
    ) -> Result<()> {
        authority.commit(|| match &mut prepared {
            PreparedOutput::Quiet => Ok(()),
            PreparedOutput::Stdout(text) => {
                let mut stdout = io::stdout().lock();
                stdout
                    .write_all(text.as_bytes())
                    .context("failed to write backup result to stdout")?;
                stdout
                    .flush()
                    .context("failed to flush backup result to stdout")
            }
            PreparedOutput::File { path, stage } => replace_file(stage, path)
                .with_context(|| format!("failed to publish output file {}", path.display())),
        })
    }

    fn write(&self, data: &str) -> Result<()> {
        let mut text = data.to_string();
        if !text.ends_with('\n') {
            text.push('\n');
        }

        let Some(path) = self.path.as_deref() else {
            let mut stdout = io::stdout().lock();
            stdout
                .write_all(text.as_bytes())
                .context("failed to write backup result to stdout")?;
            return stdout
                .flush()
                .context("failed to flush backup result to stdout");
        };

        fs::write(path, text.as_bytes())
            .with_context(|| format!("failed to write output file {}", path.display()))
    }
}

#[cfg(not(windows))]
fn replace_file(source: &Path, destination: &Path) -> io::Result<()> {
    fs::rename(source, destination)
}

#[cfg(windows)]
fn replace_file(source: &Path, destination: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt as _;
    use windows_sys::Win32::Storage::FileSystem::{
        MOVE_FILE_FLAGS, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    };

    let source = source
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let destination = destination
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let flags: MOVE_FILE_FLAGS = MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH;
    if unsafe { MoveFileExW(source.as_ptr(), destination.as_ptr(), flags) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn create_staging_file(destination: &Path) -> Result<(fs::File, PathBuf)> {
    let parent = destination
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let name = destination
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("output destination must name a file"))?;
    for nonce in 0..100_u32 {
        let path = parent.join(format!(
            ".{}.anyback-stage-{}-{nonce}",
            name.to_string_lossy(),
            std::process::id()
        ));
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => return Ok((file, path)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("failed to stage output file {}", path.display()));
            }
        }
    }
    bail!("failed to allocate output staging file")
}

fn paths_alias(left: &Path, right: &Path) -> Result<bool> {
    if left.exists() && right.exists() {
        return same_file::is_same_file(left, right).with_context(|| {
            format!(
                "failed to compare output path {} with {}",
                left.display(),
                right.display()
            )
        });
    }

    Ok(path_identity(left)? == path_identity(right)?)
}

fn path_identity(path: &Path) -> Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .context("failed to resolve current directory for output validation")?
            .join(path)
    };
    let normalized = normalize_lexically(&absolute);

    let mut ancestor = normalized.as_path();
    while !ancestor.exists() {
        ancestor = ancestor
            .parent()
            .ok_or_else(|| anyhow::anyhow!("path has no existing ancestor: {}", path.display()))?;
    }
    let canonical_ancestor = fs::canonicalize(ancestor)
        .with_context(|| format!("failed to resolve path ancestor {}", ancestor.display()))?;
    let suffix = normalized.strip_prefix(ancestor).with_context(|| {
        format!(
            "failed to normalize output comparison path {}",
            path.display()
        )
    })?;
    Ok(canonical_ancestor.join(suffix))
}

fn normalize_lexically(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::Prefix(_) | Component::RootDir | Component::Normal(_) => {
                normalized.push(component.as_os_str());
            }
        }
    }
    normalized
}

/// Accumulates human-readable lines for a single result document.
///
/// Handlers build their whole human rendering before emitting so that output
/// file routing writes one complete document instead of interleaved fragments.
#[derive(Debug, Default)]
pub struct TextBuilder {
    text: String,
}

impl TextBuilder {
    /// Creates an empty builder.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends one line.
    pub fn line(&mut self, line: impl AsRef<str>) {
        self.text.push_str(line.as_ref());
        self.text.push('\n');
    }

    /// Appends an empty separator line.
    pub fn blank(&mut self) {
        self.text.push('\n');
    }

    /// Consumes the builder and returns the rendered text.
    #[must_use]
    pub fn finish(self) -> String {
        self.text
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_mode_is_compact_and_pretty_mode_is_indented() {
        let value = serde_json::json!({"archive": "a.zip", "exported": 2});
        let dir = tempfile::tempdir().expect("tempdir");

        let compact_path = dir.path().join("compact.json");
        CommandOutput::new(OutputMode::Json, Some(compact_path.clone()))
            .emit_json(&value)
            .expect("emit compact");
        let compact = fs::read_to_string(&compact_path).expect("read compact");
        assert_eq!(compact.trim(), r#"{"archive":"a.zip","exported":2}"#);

        let pretty_path = dir.path().join("pretty.json");
        CommandOutput::new(OutputMode::Pretty, Some(pretty_path.clone()))
            .emit_json(&value)
            .expect("emit pretty");
        let pretty = fs::read_to_string(&pretty_path).expect("read pretty");
        assert!(
            pretty.contains("\n  \"archive\""),
            "pretty output: {pretty}"
        );
    }

    #[test]
    fn quiet_mode_writes_nothing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("quiet.json");
        let output = CommandOutput::new(OutputMode::Quiet, Some(path.clone()));
        output
            .emit_json(&serde_json::json!({"a": 1}))
            .expect("json");
        output.emit_text("human text").expect("text");
        output
            .emit(&serde_json::json!({"a": 1}), || "human".to_string())
            .expect("emit");
        assert!(!path.exists(), "quiet mode must not create the output file");
    }

    #[test]
    fn human_mode_uses_the_text_rendering() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("human.txt");
        CommandOutput::new(OutputMode::Human, Some(path.clone()))
            .emit(&serde_json::json!({"a": 1}), || {
                "archive: a.zip".to_string()
            })
            .expect("emit");
        let text = fs::read_to_string(&path).expect("read human");
        assert_eq!(text, "archive: a.zip\n");
    }

    #[test]
    fn each_emit_replaces_the_previous_result() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("result.txt");
        let output = CommandOutput::new(OutputMode::Human, Some(path.clone()));
        output.emit_text("first").expect("first");
        output.emit_text("second").expect("second");
        let text = fs::read_to_string(&path).expect("read result");
        assert_eq!(text, "second\n");
    }

    #[test]
    fn output_file_is_preserved_until_a_result_is_emitted() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("result.txt");
        fs::write(&path, "stale\n").expect("seed");
        let output = CommandOutput::new(OutputMode::Human, Some(path.clone()));
        assert_eq!(fs::read_to_string(&path).expect("read"), "stale\n");
        output.emit_text("fresh").expect("emit");
        assert_eq!(fs::read_to_string(&path).expect("read"), "fresh\n");
    }

    #[test]
    fn alias_validation_detects_relative_and_hard_link_aliases() {
        let dir = tempfile::tempdir().expect("tempdir");
        let input = dir.path().join("archive.zip");
        fs::write(&input, "archive").expect("seed archive");

        let relative_alias = dir.path().join("nested").join("..").join("archive.zip");
        CommandOutput::new(OutputMode::Json, Some(relative_alias))
            .ensure_distinct_from(&input, "input archive")
            .expect_err("relative alias must fail");

        let hard_link = dir.path().join("archive-link.zip");
        fs::hard_link(&input, &hard_link).expect("hard link");
        CommandOutput::new(OutputMode::Json, Some(hard_link))
            .ensure_distinct_from(&input, "input archive")
            .expect_err("hard-link alias must fail");
    }

    #[test]
    fn progress_is_only_allowed_for_human_output() {
        assert!(CommandOutput::human().allows_progress());
        assert!(!CommandOutput::json().allows_progress());
        assert!(!CommandOutput::new(OutputMode::Pretty, None).allows_progress());
        assert!(!CommandOutput::new(OutputMode::Quiet, None).allows_progress());
    }

    #[test]
    fn missing_output_directory_reports_the_path() {
        let output = CommandOutput::new(
            OutputMode::Json,
            Some(PathBuf::from("/nonexistent-anyback-dir/out.json")),
        );
        let err = output
            .emit_json(&serde_json::json!({"a": 1}))
            .expect_err("write must fail");
        assert!(
            err.to_string()
                .contains("/nonexistent-anyback-dir/out.json"),
            "error should name the path: {err}"
        );
    }
}
