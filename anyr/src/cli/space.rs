use std::io::{self, Write};

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
        super::SpaceCommands::Delete { space } => delete_space(ctx, &space).await,
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
        let items = request.list().await?.collect_all().await?;
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

async fn delete_space(ctx: &AppContext, selector: &str) -> Result<()> {
    let space_id = ctx.client.resolve_space_id(selector).await?;
    let space = ctx.client.space(&space_id).get_direct().await?;

    match prompt_archive_choice(&space.name)? {
        ArchiveChoice::Cancel => {
            eprintln!("Space deletion canceled.");
            return Ok(());
        }
        ArchiveChoice::Archive => {
            let backup = ctx
                .client
                .backup_space(&space.id)
                .backup_dir(".")
                .filename_prefix(backup_prefix(&space.name))
                .format(BackupExportFormat::Protobuf)
                .zip(true)
                .include_nested(true)
                .include_files(true)
                .include_archived(true)
                .include_backlinks(true)
                .include_space(true)
                .backup()
                .await
                .context("failed to archive the space; deletion was not attempted")?;
            eprintln!("Space archived to {}.", backup.output_path.display());
        }
        ArchiveChoice::Skip => {}
    }

    if !prompt_delete_confirmation(&space.name)? {
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
}
