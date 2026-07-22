// SPDX-FileCopyrightText: 2025-2026 Steve Schoettler
// SPDX-License-Identifier: Apache-2.0

//! Live (tier-2) body-block read tests against a running Anytype server.
//!
//! Every fixture is disposable: objects and types are created fresh with
//! unique suffixes, registered with the test context, and removed by its
//! cleanup. Requires `ANYTYPE_TEST_URL`, `ANYTYPE_KEYSTORE`, and
//! `ANYTYPE_TEST_SPACE_ID` (see `anytype::test_util`).

use anytype::prelude::*;
use anytype::test_util::{TestResult, unique_suffix, with_test_context};

#[tokio::test]
async fn test_body_read_preserves_typed_variants_ids_and_order() -> TestResult<()> {
    with_test_context(|ctx| async move {
        let name = format!("body-read-{}", unique_suffix());
        let markdown = concat!(
            "# Heading One\n\n",
            "A paragraph with **bold** text.\n\n",
            "- bullet one\n",
            "- bullet two\n\n",
            "1. numbered\n\n",
            "> a quote\n\n",
            "```\ncode block\n```\n",
        );
        let object = ctx
            .client
            .new_object(&ctx.space_id, "page")
            .name(&name)
            .body(markdown)
            .create()
            .await?;
        ctx.register_object(&object.id);

        let snapshot = ctx
            .client
            .blocks()
            .body(&ctx.space_id, &object.id)
            .fetch()
            .await?;
        assert_eq!(snapshot.space_id, ctx.space_id);
        assert_eq!(snapshot.object_id, object.id);
        assert!(snapshot.len() > 1);
        assert_eq!(snapshot.root().id, snapshot.root_id);

        // Every block ID resolves, traversal is complete, and child order is
        // the exact server order.
        let mut seen = 0_usize;
        for block in snapshot.iter() {
            seen += 1;
            assert!(snapshot.get(&block.id).is_some());
            assert_eq!(snapshot.children(&block.id), block.children.as_slice());
            let reference = snapshot.block_ref(&block.id).expect("block ref");
            assert_eq!(reference.object_id, snapshot.object_id);
        }
        assert_eq!(seen, snapshot.len());

        // The markdown body round-trips into the expected typed styles.
        let text_styles: Vec<TextStyle> = snapshot
            .iter()
            .filter_map(|block| match &block.content {
                BlockContent::Text(text) => Some(text.style),
                _ => None,
            })
            .collect();
        for expected in [
            TextStyle::Title,
            TextStyle::Paragraph,
            TextStyle::Bulleted,
            TextStyle::Numbered,
            TextStyle::Quote,
            TextStyle::Code,
        ] {
            assert!(
                text_styles.contains(&expected),
                "expected a {expected:?} text block in the read body; got {text_styles:?}"
            );
        }

        // The bold mark survives with a range that maps back into the text.
        let bold = snapshot.iter().find_map(|block| match &block.content {
            BlockContent::Text(text) => text
                .marks
                .iter()
                .find(|mark| matches!(mark.kind, MarkKind::Bold))
                .map(|mark| (text.text.clone(), mark.range)),
            _ => None,
        });
        let (bold_text, bold_range) = bold.expect("a bold mark in the read body");
        let byte_range = bold_range
            .to_byte_range(&bold_text)
            .expect("bold mark range maps to byte offsets");
        assert_eq!(&bold_text[byte_range], "bold");

        // A second show after the first read's best-effort ObjectClose proves
        // the public lifecycle remains usable and preserves exact identity and
        // document order.
        let reopened = ctx
            .client
            .blocks()
            .body(&ctx.space_id, &object.id)
            .fetch()
            .await?;
        assert_eq!(reopened.root_id, snapshot.root_id);
        assert_eq!(
            reopened.iter().map(|block| &block.id).collect::<Vec<_>>(),
            snapshot.iter().map(|block| &block.id).collect::<Vec<_>>()
        );
        Ok(())
    })
    .await
}

#[tokio::test]
async fn test_body_read_tightened_limits_reject_real_multi_block_object() -> TestResult<()> {
    with_test_context(|ctx| async move {
        let object = ctx
            .client
            .new_object(&ctx.space_id, "page")
            .name(format!("body-limit-{}", unique_suffix()))
            .body("# Heading\n\nFirst paragraph.\n\nSecond paragraph.\n\nThird paragraph.")
            .create()
            .await?;
        ctx.register_object(&object.id);

        let baseline = ctx
            .client
            .blocks()
            .body(&ctx.space_id, &object.id)
            .fetch()
            .await?;
        assert!(baseline.len() > 1, "fixture must contain multiple blocks");

        let error = ctx
            .client
            .blocks()
            .body(&ctx.space_id, &object.id)
            .limits(BodyLimits {
                max_blocks: 1,
                ..BodyLimits::default()
            })
            .fetch()
            .await
            .expect_err("oversized read must fail, not truncate");
        assert!(matches!(
            error,
            AnytypeError::BodyGraph {
                kind: BodyGraphErrorKind::Oversized,
                ..
            }
        ));

        // Validation happens after the shown view is released; a subsequent
        // unbounded read remains usable after the rejected snapshot.
        let reopened = ctx
            .client
            .blocks()
            .body(&ctx.space_id, &object.id)
            .fetch()
            .await?;
        assert_eq!(reopened.root_id, baseline.root_id);
        assert_eq!(
            reopened.iter().map(|block| &block.id).collect::<Vec<_>>(),
            baseline.iter().map(|block| &block.id).collect::<Vec<_>>()
        );
        Ok(())
    })
    .await
}

#[tokio::test]
async fn test_body_read_missing_object_returns_public_failure_without_fixture() -> TestResult<()> {
    with_test_context(|ctx| async move {
        let missing_id = format!("missing-body-{}", unique_suffix());
        let error = ctx
            .client
            .blocks()
            .body(&ctx.space_id, &missing_id)
            .fetch()
            .await
            .expect_err("a never-created object must fail");

        assert!(matches!(error, AnytypeError::Other { .. }));
        Ok(())
    })
    .await
}

#[tokio::test]
async fn test_body_read_reports_dataview_blocks_as_opaque() -> TestResult<()> {
    with_test_context(|ctx| async move {
        let suffix = unique_suffix();
        let collection_type = ctx
            .create_collection_type_fixture(format!("BodyOpaque{suffix}"))
            .await?;
        let collection = ctx
            .create_collection_fixture(&collection_type, format!("body-opaque-{suffix}"))
            .await?;

        let snapshot = ctx
            .client
            .blocks()
            .body(&ctx.space_id, &collection.id)
            .fetch()
            .await?;

        // The collection's dataview block reads fail-closed as an opaque
        // marker with a content-free summary, while the tree stays complete.
        let dataview = snapshot
            .iter()
            .find(|block| {
                matches!(
                    &block.content,
                    BlockContent::Unsupported(opaque) if opaque.kind == "dataview"
                )
            })
            .expect("a collection body must contain an opaque dataview block");
        assert!(snapshot.get(&dataview.id).is_some());
        let serialized = serde_json::to_string(&snapshot).expect("serialize snapshot");
        // The opaque summary never leaks view or relation configuration.
        assert!(!serialized.contains("relationKey"));
        Ok(())
    })
    .await
}
