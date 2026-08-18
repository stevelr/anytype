use std::{
    collections::{HashMap, HashSet},
    io::Read,
};

use anyhow::{Context, Result, bail};
use anytype::prelude::*;
use serde::{Deserialize, Serialize, de::DeserializeOwned};

use crate::{
    cli::{AppContext, BodyArgs, BodyCommands, InsertPositionArg},
    output::{OutputFormat, TableRow},
};

const MAX_LIST_BLOCKS: usize = 2_048;

impl From<InsertPositionArg> for InsertPosition {
    fn from(value: InsertPositionArg) -> Self {
        match value {
            InsertPositionArg::Before => Self::Before,
            InsertPositionArg::After => Self::After,
            InsertPositionArg::FirstChild => Self::FirstChild,
            InsertPositionArg::LastChild => Self::LastChild,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct NewBlockSpec {
    content: NewBlockContentSpec,
    #[serde(default)]
    marks: Vec<TextMark>,
    #[serde(default)]
    text_color: Option<ColorToken>,
    #[serde(default)]
    horizontal_align: Option<HorizontalAlign>,
    #[serde(default)]
    vertical_align: Option<VerticalAlign>,
    #[serde(default)]
    background_color: Option<ColorToken>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum NewBlockContentSpec {
    Paragraph {
        text: String,
    },
    Heading {
        level: u8,
        text: String,
    },
    Bulleted {
        text: String,
    },
    Numbered {
        text: String,
    },
    Checkbox {
        text: String,
        checked: bool,
    },
    Toggle {
        text: String,
    },
    Callout {
        text: String,
        #[serde(default)]
        icon: Option<CalloutIcon>,
    },
    Quote {
        text: String,
    },
    Code {
        text: String,
    },
    Divider {
        style: DividerStyle,
    },
    Bookmark {
        url: String,
    },
    LinkCard {
        target_object_id: String,
        card_style: LinkCardStyle,
        icon_size: LinkIconSize,
        description: LinkDescriptionMode,
        #[serde(default)]
        relations: Vec<String>,
    },
    Relation {
        key: String,
    },
    Table {
        rows: u32,
        columns: u32,
        #[serde(default)]
        header_row: bool,
    },
    Embed {
        processor: EmbedProcessor,
        text: String,
    },
    TableOfContents,
}

impl NewBlockSpec {
    fn into_block(self) -> Result<NewBlock> {
        let mut block = match self.content {
            NewBlockContentSpec::Paragraph { text } => NewBlock::paragraph(text)?,
            NewBlockContentSpec::Heading { level, text } => NewBlock::heading(level, text)?,
            NewBlockContentSpec::Bulleted { text } => NewBlock::bulleted(text)?,
            NewBlockContentSpec::Numbered { text } => NewBlock::numbered(text)?,
            NewBlockContentSpec::Checkbox { text, checked } => NewBlock::checkbox(text, checked)?,
            NewBlockContentSpec::Toggle { text } => NewBlock::toggle(text)?,
            NewBlockContentSpec::Callout { text, icon } => NewBlock::callout(text, icon)?,
            NewBlockContentSpec::Quote { text } => NewBlock::quote(text)?,
            NewBlockContentSpec::Code { text } => NewBlock::code(text)?,
            NewBlockContentSpec::Divider { style } => NewBlock::divider(style),
            NewBlockContentSpec::Bookmark { url } => NewBlock::bookmark(url)?,
            NewBlockContentSpec::LinkCard {
                target_object_id,
                card_style,
                icon_size,
                description,
                relations,
            } => NewBlock::link_card(target_object_id, card_style, icon_size, description)?
                .link_relations(relations)?,
            NewBlockContentSpec::Relation { key } => NewBlock::relation(key)?,
            NewBlockContentSpec::Table {
                rows,
                columns,
                header_row,
            } => NewBlock::table(rows, columns, header_row)?,
            NewBlockContentSpec::Embed { processor, text } => match processor {
                EmbedProcessor::Latex => NewBlock::embed_latex(text)?,
                EmbedProcessor::Mermaid => NewBlock::embed_mermaid(text)?,
                EmbedProcessor::Youtube => NewBlock::embed_youtube(text)?,
            },
            NewBlockContentSpec::TableOfContents => NewBlock::table_of_contents(),
        };

        if !self.marks.is_empty() {
            block = block.marks(self.marks)?;
        }
        if let Some(color) = self.text_color {
            block = block.text_color(color)?;
        }
        if let Some(align) = self.horizontal_align {
            block = block.align(align);
        }
        if let Some(align) = self.vertical_align {
            block = block.vertical_align(align);
        }
        if let Some(color) = self.background_color {
            block = block.background(color);
        }
        Ok(block)
    }
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum BlockChangeSpec {
    Text {
        text: String,
        #[serde(default)]
        marks: Vec<TextMark>,
    },
    TextStyle {
        style: TextStyle,
    },
    Checked {
        checked: bool,
    },
    TextColor {
        color: Option<ColorToken>,
    },
    CalloutIcon {
        icon: Option<CalloutIcon>,
    },
    Embed {
        processor: EmbedProcessor,
        text: String,
    },
    DividerStyle {
        style: DividerStyle,
    },
    LinkAppearance {
        card_style: LinkCardStyle,
        icon_size: LinkIconSize,
        description: LinkDescriptionMode,
        #[serde(default)]
        relations: Vec<String>,
    },
    HorizontalAlign {
        align: HorizontalAlign,
    },
    VerticalAlign {
        align: VerticalAlign,
    },
    Background {
        color: Option<ColorToken>,
    },
}

impl BlockChangeSpec {
    fn into_change(self) -> Result<BlockChange> {
        Ok(match self {
            Self::Text { text, marks } => BlockChange::Text { text, marks },
            Self::TextStyle { style } => BlockChange::TextStyle(style),
            Self::Checked { checked } => BlockChange::Checked(checked),
            Self::TextColor { color } => BlockChange::TextColor(color),
            Self::CalloutIcon { icon } => BlockChange::CalloutIcon(icon),
            Self::Embed { processor, text } => {
                BlockChange::Embed(EmbedContent::new(processor, text)?)
            }
            Self::DividerStyle { style } => BlockChange::DividerStyle(style),
            Self::LinkAppearance {
                card_style,
                icon_size,
                description,
                relations,
            } => BlockChange::LinkAppearance {
                card_style,
                icon_size,
                description,
                relations,
            },
            Self::HorizontalAlign { align } => BlockChange::HorizontalAlign(align),
            Self::VerticalAlign { align } => BlockChange::VerticalAlign(align),
            Self::Background { color } => BlockChange::Background(color),
        })
    }
}

#[derive(Debug, Serialize)]
struct BodyBlockView {
    order: usize,
    depth: usize,
    parent_id: Option<String>,
    sibling_index: Option<usize>,
    #[serde(flatten)]
    block: BodyBlock,
}

impl TableRow for BodyBlockView {
    fn headers() -> &'static [&'static str] {
        &[
            "order",
            "depth",
            "id",
            "parent_id",
            "sibling",
            "type",
            "children",
        ]
    }

    fn row(&self) -> Vec<String> {
        vec![
            self.order.to_string(),
            self.depth.to_string(),
            self.block.id.to_string(),
            self.parent_id.clone().unwrap_or_default(),
            self.sibling_index
                .map_or_else(String::new, |value| value.to_string()),
            content_kind(&self.block.content).to_owned(),
            self.block.children.len().to_string(),
        ]
    }
}

#[derive(Debug, Serialize)]
struct BodyListOutput {
    space_id: String,
    object_id: String,
    root_id: String,
    total: usize,
    offset: usize,
    limit: usize,
    items: Vec<BodyBlockView>,
}

#[derive(Debug, Serialize)]
struct MutationRow {
    space_id: String,
    object_id: String,
    block_id: String,
    root_id: String,
    block_count: usize,
}

impl TableRow for MutationRow {
    fn headers() -> &'static [&'static str] {
        &["space_id", "object_id", "block_id", "root_id", "blocks"]
    }

    fn row(&self) -> Vec<String> {
        vec![
            self.space_id.clone(),
            self.object_id.clone(),
            self.block_id.clone(),
            self.root_id.clone(),
            self.block_count.to_string(),
        ]
    }
}

#[allow(clippy::too_many_lines)] // Exhaustive dispatch keeps each operation's safety checks local.
pub async fn handle(ctx: &AppContext, args: BodyArgs) -> Result<()> {
    match args.command {
        BodyCommands::List {
            space,
            object_id,
            limit,
            offset,
        } => {
            if !(1..=MAX_LIST_BLOCKS).contains(&limit) {
                bail!("body list limit must be within 1..={MAX_LIST_BLOCKS}");
            }
            let snapshot = fetch_snapshot(ctx, &space, &object_id).await?;
            let all = block_views(&snapshot);
            let total = all.len();
            let items = all.into_iter().skip(offset).take(limit).collect();
            let output = BodyListOutput {
                space_id: snapshot.space_id.clone(),
                object_id: snapshot.object_id.clone(),
                root_id: snapshot.root_id.to_string(),
                total,
                offset,
                limit,
                items,
            };
            if ctx.output.format() == OutputFormat::Table {
                return ctx.output.emit_table(&output.items);
            }
            ctx.output.emit_json(&output)
        }
        BodyCommands::Show {
            space,
            object_id,
            block_id,
        } => {
            let snapshot = fetch_snapshot(ctx, &space, &object_id).await?;
            let block_id = parse_block_id(block_id)?;
            let view = block_views(&snapshot)
                .into_iter()
                .find(|view| view.block.id == block_id)
                .with_context(|| format!("body block \"{block_id}\" was not found"))?;
            if ctx.output.format() == OutputFormat::Table {
                return ctx.output.emit_table(std::slice::from_ref(&view));
            }
            ctx.output.emit_json(&view)
        }
        BodyCommands::Create {
            space,
            object_id,
            target_block_id,
            position,
            block,
        } => {
            let snapshot = fetch_snapshot(ctx, &space, &object_id).await?;
            let target = parse_block_id(target_block_id)?;
            let block = parse_json_source::<NewBlockSpec>(&block)?.into_block()?;
            let receipt = snapshot
                .edit(&ctx.client)
                .create(block, &target, position.into())
                .await?;
            emit_mutation(ctx, &receipt)
        }
        BodyCommands::Update {
            space,
            object_id,
            block_id,
            change,
        } => {
            let snapshot = fetch_snapshot(ctx, &space, &object_id).await?;
            let block_id = parse_block_id(block_id)?;
            let change = parse_json_source::<BlockChangeSpec>(&change)?.into_change()?;
            let receipt = snapshot.edit(&ctx.client).update(&block_id, change).await?;
            emit_mutation(ctx, &receipt)
        }
        BodyCommands::Delete {
            space,
            object_id,
            block_id,
            expected_subtree_blocks,
            confirm,
        } => {
            if !confirm {
                bail!("body delete requires --confirm");
            }
            let snapshot = fetch_snapshot(ctx, &space, &object_id).await?;
            let block_id = parse_block_id(block_id)?;
            let actual = subtree_size(&snapshot, &block_id)?;
            if actual != expected_subtree_blocks {
                bail!(
                    "body delete expected {expected_subtree_blocks} subtree blocks but found {actual}"
                );
            }
            let receipt = snapshot.edit(&ctx.client).delete(&block_id).await?;
            emit_mutation(ctx, &receipt)
        }
        BodyCommands::Move {
            space,
            object_id,
            block_id,
            target_block_id,
            position,
        } => {
            let snapshot = fetch_snapshot(ctx, &space, &object_id).await?;
            let block_id = parse_block_id(block_id)?;
            let target = parse_block_id(target_block_id)?;
            let receipt = snapshot
                .edit(&ctx.client)
                .move_block(&block_id, &target, position.into())
                .await?;
            emit_mutation(ctx, &receipt)
        }
    }
}

async fn fetch_snapshot(ctx: &AppContext, space: &str, object_id: &str) -> Result<BodySnapshot> {
    let space_id = ctx.client.resolve_space_id(space).await?;
    Ok(ctx
        .client
        .blocks()
        .body(space_id, object_id)
        .fetch()
        .await?)
}

fn parse_block_id(value: String) -> Result<BlockId> {
    BlockId::try_from(value).map_err(anyhow::Error::msg)
}

fn parse_json_source<T: DeserializeOwned>(source: &str) -> Result<T> {
    let contents = if source == "-" || source == "@-" {
        let mut contents = String::new();
        std::io::stdin()
            .read_to_string(&mut contents)
            .context("read JSON from stdin")?;
        contents
    } else if let Some(path) = source.strip_prefix('@') {
        if path.is_empty() {
            bail!("JSON source path after @ must not be empty");
        }
        std::fs::read_to_string(path).with_context(|| format!("read JSON source {path}"))?
    } else {
        source.to_owned()
    };
    serde_json::from_str(&contents).context("parse typed body JSON")
}

fn block_views(snapshot: &BodySnapshot) -> Vec<BodyBlockView> {
    let mut parents = HashMap::<BlockId, (BlockId, usize)>::new();
    for parent in snapshot.iter() {
        for (sibling_index, child) in parent.children.iter().enumerate() {
            parents.insert(child.clone(), (parent.id.clone(), sibling_index));
        }
    }

    let mut depths = HashMap::<BlockId, usize>::new();
    snapshot
        .iter()
        .enumerate()
        .map(|(order, block)| {
            let parent = parents.get(&block.id).cloned();
            let depth = parent
                .as_ref()
                .and_then(|(parent_id, _)| depths.get(parent_id))
                .copied()
                .unwrap_or(0)
                .saturating_add(usize::from(parent.is_some()));
            depths.insert(block.id.clone(), depth);
            BodyBlockView {
                order,
                depth,
                parent_id: parent.as_ref().map(|(id, _)| id.to_string()),
                sibling_index: parent.map(|(_, index)| index),
                block: block.clone(),
            }
        })
        .collect()
}

fn subtree_size(snapshot: &BodySnapshot, root: &BlockId) -> Result<usize> {
    if snapshot.get(root).is_none() {
        bail!("body block \"{root}\" was not found");
    }
    let mut pending = vec![root.clone()];
    let mut visited = HashSet::new();
    while let Some(block_id) = pending.pop() {
        if !visited.insert(block_id.clone()) {
            continue;
        }
        let block = snapshot
            .get(&block_id)
            .with_context(|| format!("body block \"{block_id}\" was not found"))?;
        pending.extend(block.children.iter().cloned());
    }
    Ok(visited.len())
}

fn emit_mutation(ctx: &AppContext, receipt: &BlockMutation) -> Result<()> {
    if ctx.output.format() != OutputFormat::Table {
        return ctx.output.emit_json(receipt);
    }
    let rows = receipt
        .affected
        .iter()
        .map(|affected| MutationRow {
            space_id: affected.space_id.clone(),
            object_id: affected.object_id.clone(),
            block_id: affected.block_id.to_string(),
            root_id: receipt.snapshot.root_id.to_string(),
            block_count: receipt.snapshot.len(),
        })
        .collect::<Vec<_>>();
    ctx.output.emit_table(&rows)
}

fn content_kind(content: &BlockContent) -> &'static str {
    match content {
        BlockContent::Text(_) => "text",
        BlockContent::Layout(_) => "layout",
        BlockContent::Divider(_) => "divider",
        BlockContent::Bookmark(_) => "bookmark",
        BlockContent::Link(_) => "link",
        BlockContent::Relation(_) => "relation",
        BlockContent::FeaturedRelations => "featured_relations",
        BlockContent::Embed(_) => "embed",
        BlockContent::TableOfContents => "table_of_contents",
        BlockContent::Table => "table",
        BlockContent::TableRow { .. } => "table_row",
        BlockContent::TableColumn => "table_column",
        BlockContent::File(_) => "file",
        BlockContent::Unsupported(_) => "unsupported",
        _ => "unknown",
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;
    use crate::cli::{Cli, Commands};

    #[test]
    fn representative_rich_block_specs_build() {
        for source in [
            r#"{"content":{"kind":"paragraph","text":"hello"}}"#,
            r#"{"content":{"kind":"heading","level":2,"text":"heading"}}"#,
            r#"{"content":{"kind":"checkbox","text":"done","checked":true}}"#,
            r#"{"content":{"kind":"callout","text":"note","icon":{"type":"emoji","content":"💡"}}}"#,
            r#"{"content":{"kind":"divider","style":"dots"}}"#,
            r#"{"content":{"kind":"bookmark","url":"https://example.com"}}"#,
            r#"{"content":{"kind":"relation","key":"status"}}"#,
            r#"{"content":{"kind":"table","rows":2,"columns":3,"header_row":true}}"#,
            r#"{"content":{"kind":"embed","processor":"mermaid","text":"graph TD; A-->B"}}"#,
            r#"{"content":{"kind":"table_of_contents"}}"#,
        ] {
            parse_json_source::<NewBlockSpec>(source)
                .and_then(NewBlockSpec::into_block)
                .expect("representative typed block builds");
        }
    }

    #[test]
    fn representative_changes_build() {
        for source in [
            r#"{"kind":"text","text":"updated","marks":[]}"#,
            r#"{"kind":"checked","checked":false}"#,
            r#"{"kind":"text_color","color":"blue"}"#,
            r#"{"kind":"text_color","color":null}"#,
            r#"{"kind":"embed","processor":"latex","text":"x^2"}"#,
            r#"{"kind":"background","color":"grey"}"#,
        ] {
            parse_json_source::<BlockChangeSpec>(source)
                .and_then(BlockChangeSpec::into_change)
                .expect("representative typed change builds");
        }
    }

    #[test]
    fn every_body_command_parses() {
        for args in [
            vec!["anyr", "body", "list", "space", "object"],
            vec!["anyr", "body", "show", "space", "object", "block"],
            vec![
                "anyr",
                "body",
                "create",
                "space",
                "object",
                "target",
                "last-child",
                "--block",
                r#"{"content":{"kind":"paragraph","text":"hello"}}"#,
            ],
            vec![
                "anyr",
                "body",
                "update",
                "space",
                "object",
                "block",
                "--change",
                r#"{"kind":"text","text":"updated"}"#,
            ],
            vec![
                "anyr",
                "body",
                "delete",
                "space",
                "object",
                "block",
                "--expected-subtree-blocks",
                "1",
                "--confirm",
            ],
            vec![
                "anyr", "body", "move", "space", "object", "block", "target", "after",
            ],
        ] {
            let cli = Cli::try_parse_from(args).expect("body command parses");
            assert!(matches!(cli.command, Commands::Body(_)));
        }
    }
}
