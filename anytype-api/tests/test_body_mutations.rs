// SPDX-FileCopyrightText: 2025-2026 Steve Schoettler
// SPDX-License-Identifier: Apache-2.0

//! Live tier-2 verification for body-block mutations.
//!
//! The suite uses the shared test context and immediately registers every
//! created object for cleanup.

use anytype::prelude::*;
use anytype::test_util::{TestResult, unique_suffix, with_test_context};

#[tokio::test]
async fn test_verified_body_block_mutation_round_trip() -> TestResult<()> {
    with_test_context(|ctx| async move {
        let object = ctx
            .client
            .new_object(&ctx.space_id, "page")
            .name(format!("body-mutations-{}", unique_suffix()))
            .create()
            .await?;
        ctx.register_object(&object.id);

        let initial = ctx
            .client
            .blocks()
            .body(&ctx.space_id, &object.id)
            .fetch()
            .await?;
        let appended = initial
            .edit(&ctx.client)
            .append(NewBlock::checkbox("first", false)?)
            .await?;
        let first_id = receipt_id(&appended)?;
        assert!(matches!(
            &appended.snapshot.get(&first_id).map(|block| &block.content),
            Some(BlockContent::Text(text))
                if text.style == TextStyle::Checkbox && text.text == "first" && !text.checked
        ));

        let updated = appended
            .snapshot
            .edit(&ctx.client)
            .update(&first_id, BlockChange::Checked(true))
            .await?;
        assert!(matches!(
            &updated.snapshot.get(&first_id).map(|block| &block.content),
            Some(BlockContent::Text(text)) if text.checked
        ));

        let second = updated
            .snapshot
            .edit(&ctx.client)
            .create(
                NewBlock::paragraph("second")?,
                &first_id,
                InsertPosition::After,
            )
            .await?;
        let second_id = receipt_id(&second)?;
        let moved = second
            .snapshot
            .edit(&ctx.client)
            .move_block(&second_id, &first_id, InsertPosition::Before)
            .await?;
        let deleted = moved.snapshot.edit(&ctx.client).delete(&first_id).await?;
        assert!(deleted.snapshot.get(&first_id).is_none());
        assert!(deleted.snapshot.get(&second_id).is_some());

        let stale = BlockId::try_from("definitely-stale-block-id".to_owned())
            .map_err(|message| AnytypeError::Validation { message })?;
        let outcome = deleted
            .snapshot
            .edit(&ctx.client)
            .apply_all(vec![
                BodyOp::Append {
                    block: NewBlock::paragraph("batch-applied")?,
                },
                BodyOp::Delete { block_id: stale },
                BodyOp::Delete {
                    block_id: second_id,
                },
            ])
            .await?;
        assert_eq!(outcome.applied.len(), 1);
        assert!(outcome.failed.is_some());
        assert_eq!(outcome.not_attempted.len(), 1);
        Ok(())
    })
    .await
}

#[tokio::test]
async fn test_body_block_constructor_update_and_position_matrix() -> TestResult<()> {
    with_test_context(|ctx| async move {
        let linked = ctx
            .client
            .new_object(&ctx.space_id, "page")
            .name(format!("body-link-target-{}", unique_suffix()))
            .create()
            .await?;
        ctx.register_object(&linked.id);
        let object = ctx
            .client
            .new_object(&ctx.space_id, "page")
            .name(format!("body-matrix-{}", unique_suffix()))
            .create()
            .await?;
        ctx.register_object(&object.id);

        let mut snapshot = ctx
            .client
            .blocks()
            .body(&ctx.space_id, &object.id)
            .fetch()
            .await?;
        let constructors = vec![
            NewBlock::paragraph("paragraph")?,
            NewBlock::heading(1, "heading 1")?,
            NewBlock::heading(2, "heading 2")?,
            NewBlock::heading(3, "heading 3")?,
            NewBlock::bulleted("bulleted")?,
            NewBlock::numbered("numbered")?,
            NewBlock::checkbox("checkbox", false)?,
            NewBlock::toggle("toggle")?,
            NewBlock::callout("callout", Some(CalloutIcon::Emoji("ℹ️".to_owned())))?,
            NewBlock::quote("quote")?,
            NewBlock::code("code")?,
            NewBlock::divider(DividerStyle::Dots),
            NewBlock::bookmark("https://example.com/body-mutation")?,
            NewBlock::link_card(
                linked.id.clone(),
                LinkCardStyle::Card,
                LinkIconSize::Medium,
                LinkDescriptionMode::Content,
            )?
            .link_relations(vec!["name".to_owned()])?,
            NewBlock::relation("name")?,
            NewBlock::table(2, 3, true)?,
            NewBlock::embed_latex("x^2")?,
            NewBlock::embed_mermaid("graph TD; A-->B")?,
            NewBlock::embed_youtube("https://youtu.be/dQw4w9WgXcQ")?,
            NewBlock::table_of_contents(),
        ];
        let mut ids = Vec::with_capacity(constructors.len());
        for constructor in constructors {
            let receipt = snapshot.edit(&ctx.client).append(constructor).await?;
            ids.push(receipt_id(&receipt)?);
            snapshot = receipt.snapshot;
        }

        let paragraph = ids[0].clone();
        let checkbox = ids[6].clone();
        let toggle = ids[7].clone();
        let callout = ids[8].clone();
        let divider = ids[11].clone();
        let link = ids[13].clone();
        let latex = ids[16].clone();
        let changes = [
            (checkbox, BlockChange::Checked(true)),
            (
                callout,
                BlockChange::CalloutIcon(Some(CalloutIcon::Emoji("✅".to_owned()))),
            ),
            (divider, BlockChange::DividerStyle(DividerStyle::Line)),
            (
                link,
                BlockChange::LinkAppearance {
                    card_style: LinkCardStyle::Text,
                    icon_size: LinkIconSize::Small,
                    description: LinkDescriptionMode::None,
                    relations: Vec::new(),
                },
            ),
            (
                latex,
                BlockChange::Embed(EmbedContent::new(EmbedProcessor::Latex, "x^3")?),
            ),
        ];
        for (id, change) in changes {
            snapshot = snapshot
                .edit(&ctx.client)
                .update(&id, change)
                .await?
                .snapshot;
        }
        let red = ColorToken::new("red")?;
        let paragraph_changes = [
            BlockChange::Text {
                text: "changed".to_owned(),
                marks: vec![TextMark::new(
                    TextRange { start: 0, end: 7 },
                    MarkKind::Bold,
                )],
            },
            BlockChange::TextStyle(TextStyle::Quote),
            BlockChange::TextColor(Some(red.clone())),
            BlockChange::HorizontalAlign(HorizontalAlign::Center),
            BlockChange::VerticalAlign(VerticalAlign::Middle),
            BlockChange::Background(Some(red)),
        ];
        for change in paragraph_changes {
            snapshot = snapshot
                .edit(&ctx.client)
                .update(&paragraph, change)
                .await?
                .snapshot;
        }

        let before = snapshot
            .edit(&ctx.client)
            .create(
                NewBlock::paragraph("before")?,
                &paragraph,
                InsertPosition::Before,
            )
            .await?;
        let before_id = receipt_id(&before)?;
        let after = before
            .snapshot
            .edit(&ctx.client)
            .create(
                NewBlock::paragraph("after")?,
                &paragraph,
                InsertPosition::After,
            )
            .await?;
        let first_child = after
            .snapshot
            .edit(&ctx.client)
            .create(
                NewBlock::paragraph("first child")?,
                &toggle,
                InsertPosition::FirstChild,
            )
            .await?;
        let last_child = first_child
            .snapshot
            .edit(&ctx.client)
            .create(
                NewBlock::paragraph("last child")?,
                &toggle,
                InsertPosition::LastChild,
            )
            .await?;
        snapshot = last_child
            .snapshot
            .edit(&ctx.client)
            .move_block(&before_id, &paragraph, InsertPosition::After)
            .await?
            .snapshot;

        let foreign = ctx
            .client
            .blocks()
            .body(&ctx.space_id, &linked.id)
            .fetch()
            .await?;
        let foreign_id = foreign.root_id.clone();
        assert!(matches!(
            snapshot
                .edit(&ctx.client)
                .create(
                    NewBlock::paragraph("wrong context")?,
                    &foreign_id,
                    InsertPosition::LastChild
                )
                .await,
            Err(AnytypeError::NotFound { .. })
        ));

        let parent_one = snapshot
            .edit(&ctx.client)
            .append(NewBlock::toggle("parent one")?)
            .await?;
        let parent_one_id = receipt_id(&parent_one)?;
        let parent_two = parent_one
            .snapshot
            .edit(&ctx.client)
            .append(NewBlock::toggle("parent two")?)
            .await?;
        let parent_two_id = receipt_id(&parent_two)?;
        let anchored = parent_two
            .snapshot
            .edit(&ctx.client)
            .create(
                NewBlock::paragraph("concurrent anchor")?,
                &parent_one_id,
                InsertPosition::LastChild,
            )
            .await?;
        let anchor_id = receipt_id(&anchored)?;
        let stale = anchored.snapshot.clone();
        let _moved = stale
            .edit(&ctx.client)
            .move_block(&anchor_id, &parent_two_id, InsertPosition::FirstChild)
            .await?;
        let error = stale
            .edit(&ctx.client)
            .verify_with(VerifyConfig {
                timeout: std::time::Duration::from_millis(300),
                initial_delay: std::time::Duration::ZERO,
                max_delay: std::time::Duration::from_millis(20),
                max_attempts: 3,
            })
            .create(
                NewBlock::paragraph("must not verify under moved anchor")?,
                &anchor_id,
                InsertPosition::Before,
            )
            .await
            .expect_err("a concurrently reparented anchor must not verify against stale context");
        assert!(matches!(
            error,
            AnytypeError::BodyMutationIndeterminate {
                observed: Some(_),
                ..
            }
        ));
        Ok(())
    })
    .await
}

fn receipt_id(receipt: &BlockMutation) -> TestResult<BlockId> {
    receipt
        .affected
        .first()
        .map(|reference| reference.block_id.clone())
        .ok_or_else(|| {
            AnytypeError::Other {
                message: "verified mutation returned no affected block".to_owned(),
            }
            .into()
        })
}
