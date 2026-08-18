use anyhow::Result;
use anytype::prelude::AttachedDiscussion;
use serde::Serialize;

use crate::{
    cli::{AppContext, ObjectDiscussionArgs, ObjectDiscussionCommands},
    output::{OutputFormat, TableRow},
};

#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
enum AttachedDiscussionOutput {
    Absent {
        space_id: String,
        parent_id: String,
    },
    Attached {
        space_id: String,
        parent_id: String,
        discussion_id: String,
    },
}

impl From<AttachedDiscussion> for AttachedDiscussionOutput {
    fn from(value: AttachedDiscussion) -> Self {
        match value {
            AttachedDiscussion::Absent {
                space_id,
                parent_id,
            } => Self::Absent {
                space_id,
                parent_id,
            },
            AttachedDiscussion::Attached {
                space_id,
                parent_id,
                discussion_id,
            } => Self::Attached {
                space_id,
                parent_id,
                discussion_id,
            },
        }
    }
}

impl TableRow for AttachedDiscussionOutput {
    fn headers() -> &'static [&'static str] {
        &["state", "space_id", "parent_id", "discussion_id"]
    }

    fn row(&self) -> Vec<String> {
        match self {
            Self::Absent {
                space_id,
                parent_id,
            } => vec![
                "absent".to_owned(),
                space_id.clone(),
                parent_id.clone(),
                String::new(),
            ],
            Self::Attached {
                space_id,
                parent_id,
                discussion_id,
            } => vec![
                "attached".to_owned(),
                space_id.clone(),
                parent_id.clone(),
                discussion_id.clone(),
            ],
        }
    }
}

pub async fn handle(ctx: &AppContext, args: ObjectDiscussionArgs) -> Result<()> {
    let (space, object_id, attach) = match args.command {
        ObjectDiscussionCommands::Get { space, object_id } => (space, object_id, false),
        ObjectDiscussionCommands::Attach { space, object_id } => (space, object_id, true),
    };
    let space_id = ctx.client.resolve_space_id(&space).await?;
    let request = ctx.client.attached_discussion(space_id, object_id);
    let result = if attach {
        request.ensure().await?
    } else {
        request.get().await?
    };
    let output = AttachedDiscussionOutput::from(result);
    if ctx.output.format() == OutputFormat::Table {
        return ctx.output.emit_table(std::slice::from_ref(&output));
    }
    ctx.output.emit_json(&output)
}

#[cfg(test)]
mod tests {
    use anytype::prelude::AttachedDiscussion;
    use clap::Parser;

    use super::*;
    use crate::cli::{Cli, Commands, ObjectCommands};

    #[test]
    fn attached_state_projects_exact_ids() {
        let output = AttachedDiscussionOutput::from(AttachedDiscussion::Attached {
            space_id: "space".to_owned(),
            parent_id: "parent".to_owned(),
            discussion_id: "discussion".to_owned(),
        });
        assert_eq!(
            output,
            AttachedDiscussionOutput::Attached {
                space_id: "space".to_owned(),
                parent_id: "parent".to_owned(),
                discussion_id: "discussion".to_owned(),
            }
        );
    }

    #[test]
    fn object_discussion_commands_parse() {
        for action in ["get", "attach", "ensure"] {
            let cli =
                Cli::try_parse_from(["anyr", "object", "discussion", action, "space", "object"])
                    .expect("object discussion command parses");
            assert!(matches!(
                cli.command,
                Commands::Object(crate::cli::ObjectArgs {
                    command: ObjectCommands::Discussion(_),
                })
            ));
        }
    }
}
