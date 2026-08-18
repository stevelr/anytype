use std::{
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use anytype::prelude::*;
use serde_json::json;

use crate::{
    cli::{AppContext, pagination_limit, pagination_offset},
    filter::parse_filters,
    output::OutputFormat,
};

pub async fn handle(ctx: &AppContext, args: super::SpaceArgs) -> Result<()> {
    match args.command {
        super::SpaceCommands::List { pagination, filter } => {
            list_spaces(ctx, pagination, filter).await
        }
        super::SpaceCommands::Get { space } => {
            let space_id = ctx.client.resolve_space_id(&space).await?;
            let space = ctx.client.space(space_id).get().await?;
            ctx.output.emit_json(&space)
        }
        super::SpaceCommands::Create {
            name,
            description,
            chat,
        } => {
            if chat {
                if description.is_some() {
                    anyhow::bail!("--description is not supported with --chat");
                }
                let space = ctx.client.create_chat_space(name).await?;
                return ctx.output.emit_json(&space);
            }
            let mut request = ctx.client.new_space(name);
            if let Some(description) = description {
                request = request.description(description);
            }
            let space = request.create().await?;
            ctx.output.emit_json(&space)
        }
        super::SpaceCommands::Update {
            space,
            name,
            description,
        } => {
            let space_id = ctx.client.resolve_space_id(&space).await?;
            let mut request = ctx.client.update_space(space_id);
            if let Some(name) = name {
                request = request.name(name);
            }
            if let Some(description) = description {
                request = request.description(description);
            }
            let space = request.update().await?;
            ctx.output.emit_json(&space)
        }
        super::SpaceCommands::CountArchived { space } => {
            let space_id = ctx.client.resolve_space_id(&space).await?;
            let count = ctx.client.count_archived(&space_id).await?;
            ctx.output.emit_text(&format!("{count} archived object(s)"))
        }
        super::SpaceCommands::DeleteArchived { space, confirm } => {
            let space_id = ctx.client.resolve_space_id(&space).await?;
            if !confirm {
                let count = ctx.client.count_archived(&space_id).await?;
                if count == 0 {
                    return ctx.output.emit_text("no archived objects to delete");
                }
                anyhow::bail!(
                    "{count} archived object(s) in space \"{space}\". \
                     Re-run with --confirm to delete them permanently."
                );
            }
            let result = ctx.client.delete_all_archived(&space_id).await?;
            if result.failed_ids.is_empty() {
                ctx.output
                    .emit_text(&format!("deleted {} archived object(s)", result.deleted))
            } else {
                ctx.output.emit_text(&format!(
                    "deleted {}, failed to delete {}",
                    result.deleted,
                    result.failed_ids.len()
                ))
            }
        }
        super::SpaceCommands::Delete {
            space,
            archive,
            skip_archive,
            confirm,
        } => delete_space(ctx, &space, archive, skip_archive, confirm).await,
        super::SpaceCommands::Invite(args) => invite(ctx, args).await,
        super::SpaceCommands::EnableSharing { space } => {
            let space_id = ctx.client.resolve_space_id(&space).await?;
            ctx.client.enable_space_sharing(&space_id).await?;
            ctx.output.emit_json(&json!({
                "space_id": space_id,
                "sharing": "enabled",
            }))
        }
        super::SpaceCommands::DisableSharing { space } => {
            let space_id = ctx.client.resolve_space_id(&space).await?;
            ctx.client.disable_space_sharing(&space_id).await?;
            ctx.output.emit_json(&json!({
                "space_id": space_id,
                "sharing": "disabled",
            }))
        }
    }
}

async fn list_spaces(
    ctx: &AppContext,
    pagination: super::PaginationArgs,
    filter: super::FilterArgs,
) -> Result<()> {
    let mut request = ctx
        .client
        .spaces()
        .limit(pagination_limit(&pagination))
        .offset(pagination_offset(&pagination));

    for filter in parse_filters(&filter.filters)? {
        request = request.filter(filter);
    }

    if pagination.all {
        let items = ctx
            .collect_all(async { request.list().await?.collect_all().await })
            .await?;
        if ctx.output.format() == OutputFormat::Table {
            return ctx.output.emit_table(&items);
        }
        return ctx.output.emit_json(&items);
    }

    let result = request.list().await?;
    if ctx.output.format() == OutputFormat::Table {
        return ctx.output.emit_table(&result.items);
    }
    ctx.output.emit_json(&result)
}

async fn invite(ctx: &AppContext, args: super::InviteArgs) -> Result<()> {
    match args.command {
        super::InviteCommands::Show { space } => {
            let space_id = ctx.client.resolve_space_id(&space).await?;
            let invites = ctx.client.list_space_invites(&space_id).await?;
            ctx.output.emit_json(&invites)
        }
        super::InviteCommands::Create {
            space,
            reader: _,
            writer,
            owner,
            guest,
            with_approval,
            auto_approve: _,
        } => {
            let space_id = ctx.client.resolve_space_id(&space).await?;
            let permissions = if owner {
                SpaceInvitePermission::Owner
            } else if writer {
                SpaceInvitePermission::Writer
            } else {
                SpaceInvitePermission::Reader
            };
            let invite_type = if guest {
                SpaceInviteType::Guest
            } else if with_approval {
                SpaceInviteType::Member
            } else {
                SpaceInviteType::AutoApprove
            };
            let invite = ctx
                .client
                .create_space_invite(&space_id, invite_type, permissions)
                .await?;
            ctx.output.emit_json(&invite)
        }
        super::InviteCommands::Revoke { space } => {
            let space_id = ctx.client.resolve_space_id(&space).await?;
            ctx.client.revoke_space_invite(&space_id).await?;
            ctx.output.emit_json(&json!({
                "space_id": space_id,
                "invite": "revoked",
            }))
        }
    }
}

async fn delete_space(
    ctx: &AppContext,
    selector: &str,
    archive: Option<PathBuf>,
    skip_archive: bool,
    confirm: bool,
) -> Result<()> {
    let space_id = ctx.client.resolve_space_id(selector).await?;
    let space = ctx.client.space(&space_id).get_direct().await?;

    match select_archive_action(&space.name, archive, skip_archive)? {
        ArchiveAction::Cancel => {
            eprintln!("Space deletion canceled.");
            return Ok(());
        }
        ArchiveAction::Create(destination) => {
            let output_path = archive_space(ctx, &space, destination.as_deref())
                .await
                .context("failed to archive the space; deletion was not attempted")?;
            eprintln!("Space archived to {}.", output_path.display());
        }
        ArchiveAction::Skip => {}
    }

    if !confirm && !prompt_delete_confirmation(&space.name)? {
        eprintln!("Space deletion canceled.");
        return Ok(());
    }

    ctx.client.delete_space(&space.id).await?;
    ctx.output.emit_json(&json!({
        "space_id": space.id,
        "name": space.name,
        "deleted": true,
    }))
}

async fn archive_space(
    ctx: &AppContext,
    space: &Space,
    destination: Option<&Path>,
) -> Result<PathBuf> {
    let parent = destination.map_or_else(|| Path::new("."), archive_parent);
    if let Some(destination) = destination {
        ensure_archive_destination_available(destination)?;
    } else {
        ensure_archive_parent(parent)?;
    }

    let staging = TemporaryArchiveDirectory::new(parent)?;
    let backup = ctx
        .client
        .backup_space(&space.id)
        .backup_dir(staging.path())
        .filename_prefix(backup_prefix(&space.name))
        .format(BackupExportFormat::Protobuf)
        .zip(true)
        .include_nested(true)
        .include_files(true)
        .include_archived(true)
        .include_backlinks(true)
        .include_space(true)
        .backup()
        .await?;

    validate_staged_archive(staging.path(), &backup.output_path)?;
    let destination = if let Some(path) = destination {
        path.to_path_buf()
    } else {
        let file_name = backup.output_path.file_name().ok_or_else(|| {
            anyhow::anyhow!(
                "backup produced an archive path without a file name: {}",
                backup.output_path.display()
            )
        })?;
        parent.join(file_name)
    };
    ensure_archive_destination_available(&destination)?;
    copy_archive_no_clobber(&backup.output_path, &destination)?;
    validate_archive_file(&destination)?;
    validate_archive_contents(&destination)?;
    staging.remove()?;
    Ok(destination)
}

fn archive_parent(destination: &Path) -> &Path {
    match destination.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    }
}

fn ensure_archive_parent(parent: &Path) -> Result<()> {
    let metadata = fs::metadata(parent)
        .with_context(|| format!("failed to inspect archive parent {}", parent.display()))?;
    anyhow::ensure!(
        metadata.is_dir(),
        "archive parent is not a directory: {}",
        parent.display()
    );
    Ok(())
}

fn ensure_archive_destination_available(destination: &Path) -> Result<()> {
    match fs::symlink_metadata(destination) {
        Ok(_) => anyhow::bail!(
            "archive destination already exists; refusing to overwrite {}",
            destination.display()
        ),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error).with_context(|| {
                format!(
                    "failed to inspect archive destination {}",
                    destination.display()
                )
            });
        }
    }
    ensure_archive_parent(archive_parent(destination))
}

fn validate_staged_archive(staging: &Path, archive: &Path) -> Result<()> {
    validate_archive_file(archive)?;
    validate_archive_contents(archive)?;
    let canonical_staging = fs::canonicalize(staging)
        .with_context(|| format!("failed to resolve staging directory {}", staging.display()))?;
    let canonical_archive = fs::canonicalize(archive)
        .with_context(|| format!("failed to resolve staged archive {}", archive.display()))?;
    anyhow::ensure!(
        canonical_archive.parent() == Some(canonical_staging.as_path()),
        "backup output escaped its staging directory: {}",
        archive.display()
    );
    Ok(())
}

fn validate_archive_file(archive: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(archive)
        .with_context(|| format!("failed to inspect backup archive {}", archive.display()))?;
    anyhow::ensure!(
        metadata.file_type().is_file(),
        "backup did not produce a regular archive file: {}",
        archive.display()
    );
    anyhow::ensure!(
        metadata.len() > 0,
        "backup produced an empty archive: {}",
        archive.display()
    );
    Ok(())
}

#[cfg(feature = "backup")]
fn validate_archive_contents(archive: &Path) -> Result<()> {
    let reader = anyback_reader::archive::ArchiveReader::from_path(archive)
        .with_context(|| format!("backup archive is unreadable: {}", archive.display()))?;
    let files = reader
        .list_files()
        .with_context(|| format!("failed to list backup archive {}", archive.display()))?;
    anyhow::ensure!(
        !files.is_empty(),
        "backup archive contains no files: {}",
        archive.display()
    );
    Ok(())
}

#[cfg(not(feature = "backup"))]
fn validate_archive_contents(_archive: &Path) -> Result<()> {
    anyhow::bail!(
        "pre-delete archive validation requires anyr's default backup feature; rebuild with the backup feature or use --skip-archive"
    )
}

fn copy_archive_no_clobber(source: &Path, destination: &Path) -> Result<()> {
    let expected_bytes = fs::metadata(source)
        .with_context(|| format!("failed to inspect staged archive {}", source.display()))?
        .len();
    let mut input = fs::File::open(source)
        .with_context(|| format!("failed to open staged archive {}", source.display()))?;
    let mut output = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)
        .with_context(|| {
            format!(
                "failed to create archive destination {} without overwriting",
                destination.display()
            )
        })?;

    let copied = io::copy(&mut input, &mut output).with_context(|| {
        format!(
            "failed to write archive destination {}",
            destination.display()
        )
    });
    let result = copied.and_then(|bytes| {
        anyhow::ensure!(
            bytes == expected_bytes,
            "archive copy was incomplete: wrote {bytes} of {expected_bytes} bytes to {}",
            destination.display()
        );
        output.sync_all().with_context(|| {
            format!(
                "failed to flush archive destination {}",
                destination.display()
            )
        })
    });
    drop(output);

    if let Err(error) = result {
        if let Err(cleanup_error) = fs::remove_file(destination)
            && cleanup_error.kind() != io::ErrorKind::NotFound
        {
            return Err(error).context(format!(
                "also failed to remove incomplete archive destination {}: {cleanup_error}",
                destination.display()
            ));
        }
        return Err(error);
    }
    Ok(())
}

#[derive(Debug)]
struct TemporaryArchiveDirectory {
    path: PathBuf,
}

impl TemporaryArchiveDirectory {
    fn new(parent: &Path) -> Result<Self> {
        ensure_archive_parent(parent)?;
        let seed = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos());
        for attempt in 0..128_u32 {
            let path = parent.join(format!(
                ".anyr-space-delete-{}-{seed}-{attempt}",
                std::process::id()
            ));
            match fs::create_dir(&path) {
                Ok(()) => return Ok(Self { path }),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => {
                    return Err(error).with_context(|| {
                        format!(
                            "failed to create archive staging directory {}",
                            path.display()
                        )
                    });
                }
            }
        }
        anyhow::bail!(
            "failed to allocate an archive staging directory under {}",
            parent.display()
        )
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn remove(&self) -> Result<()> {
        fs::remove_dir_all(&self.path).with_context(|| {
            format!(
                "failed to remove archive staging directory {}",
                self.path.display()
            )
        })
    }
}

impl Drop for TemporaryArchiveDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

#[derive(Debug, PartialEq, Eq)]
enum ArchiveAction {
    Create(Option<PathBuf>),
    Skip,
    Cancel,
}

fn select_archive_action(
    name: &str,
    archive: Option<PathBuf>,
    skip_archive: bool,
) -> Result<ArchiveAction> {
    if archive.is_some() && skip_archive {
        anyhow::bail!("--archive and --skip-archive cannot be used together");
    }
    if let Some(destination) = archive {
        return Ok(ArchiveAction::Create(Some(destination)));
    }
    if skip_archive {
        return Ok(ArchiveAction::Skip);
    }
    Ok(match prompt_archive_choice(name)? {
        ArchiveChoice::Archive => ArchiveAction::Create(None),
        ArchiveChoice::Skip => ArchiveAction::Skip,
        ArchiveChoice::Cancel => ArchiveAction::Cancel,
    })
}

#[derive(Debug, PartialEq, Eq)]
enum ArchiveChoice {
    Archive,
    Skip,
    Cancel,
}

fn prompt_archive_choice(name: &str) -> Result<ArchiveChoice> {
    Ok(parse_archive_choice(&prompt_line(&format!(
        "This operation is irreversible. Do you want to archive space {name} first? [Y/n] "
    ))?))
}

fn prompt_delete_confirmation(name: &str) -> Result<bool> {
    let expected = format!("delete:{name}");
    let answer = prompt_line(&format!(
        "Really delete space {name}? To delete it, type \"{expected}\" spelling the name exactly: "
    ))?;
    Ok(delete_confirmation_matches(name, &answer))
}

fn delete_confirmation_matches(name: &str, answer: &str) -> bool {
    answer == format!("delete:{name}")
}

fn parse_archive_choice(answer: &str) -> ArchiveChoice {
    match answer.trim().to_ascii_lowercase().as_str() {
        "" | "y" | "yes" => ArchiveChoice::Archive,
        "n" | "no" => ArchiveChoice::Skip,
        _ => ArchiveChoice::Cancel,
    }
}

fn prompt_line(prompt: &str) -> Result<String> {
    eprint!("{prompt}");
    io::stderr().flush()?;
    let mut answer = String::new();
    if io::stdin().read_line(&mut answer)? == 0 {
        return Ok(String::new());
    }
    Ok(answer.trim_end_matches(['\r', '\n']).to_owned())
}

fn backup_prefix(name: &str) -> String {
    let mut sanitized = String::new();
    for character in name.chars() {
        if character.is_ascii_alphanumeric() || matches!(character, '-' | '_') {
            sanitized.push(character);
        } else if !sanitized.ends_with('-') {
            sanitized.push('-');
        }
        if sanitized.len() >= 64 {
            break;
        }
    }
    let sanitized = sanitized.trim_matches('-');
    if sanitized.is_empty() {
        "anyr-space-delete".to_owned()
    } else {
        format!("anyr-space-delete-{sanitized}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn archive_prompt_defaults_to_archive() {
        assert_eq!(parse_archive_choice(""), ArchiveChoice::Archive);
    }

    #[test]
    fn archive_prompt_rejects_unknown_answers() {
        assert_eq!(parse_archive_choice("maybe"), ArchiveChoice::Cancel);
    }

    #[test]
    fn backup_prefix_cannot_escape_the_current_directory() {
        assert_eq!(backup_prefix("a/../../b"), "anyr-space-delete-a-b");
    }

    #[test]
    fn deletion_confirmation_requires_exact_name() {
        assert!(delete_confirmation_matches("My Space", "delete:My Space"));
        assert!(!delete_confirmation_matches("My Space", "delete:my space"));
        assert!(!delete_confirmation_matches("My Space", "delete:My Space "));
    }

    #[test]
    fn explicit_archive_destination_bypasses_archive_prompt() {
        let destination = PathBuf::from("chosen.zip");
        assert_eq!(
            select_archive_action("My Space", Some(destination.clone()), false)
                .expect("explicit archive action should be valid"),
            ArchiveAction::Create(Some(destination))
        );
    }

    #[test]
    fn archive_copy_uses_exact_destination_without_overwrite() {
        let temp = TemporaryArchiveDirectory::new(&std::env::temp_dir())
            .expect("temporary directory should be created");
        let source = temp.path().join("generated.zip");
        let destination = temp.path().join("chosen.zip");
        fs::write(&source, b"archive bytes").expect("source archive should be written");

        copy_archive_no_clobber(&source, &destination)
            .expect("archive should copy to the selected destination");
        assert_eq!(
            fs::read(&destination).expect("destination archive should be readable"),
            b"archive bytes"
        );

        fs::write(&source, b"replacement").expect("source archive should be replaced");
        let error = copy_archive_no_clobber(&source, &destination)
            .expect_err("existing archive destination must not be overwritten");
        assert!(
            error.to_string().contains("without overwriting"),
            "{error:#}"
        );
        assert_eq!(
            fs::read(&destination).expect("existing archive should remain readable"),
            b"archive bytes"
        );
    }

    #[test]
    fn empty_archive_is_rejected_before_deletion() {
        let temp = TemporaryArchiveDirectory::new(&std::env::temp_dir())
            .expect("temporary directory should be created");
        let archive = temp.path().join("empty.zip");
        fs::write(&archive, []).expect("empty archive should be written");
        let error = validate_archive_file(&archive).expect_err("empty archive must fail");
        assert!(error.to_string().contains("empty archive"), "{error:#}");
    }

    #[cfg(feature = "backup")]
    #[test]
    fn non_zip_archive_is_rejected_before_deletion() {
        let temp = TemporaryArchiveDirectory::new(&std::env::temp_dir())
            .expect("temporary directory should be created");
        let archive = temp.path().join("invalid.zip");
        fs::write(&archive, b"not a zip archive").expect("invalid archive should be written");
        let error = validate_archive_contents(&archive).expect_err("invalid zip archive must fail");
        assert!(error.to_string().contains("unreadable"), "{error:#}");
    }
}
