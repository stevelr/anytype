// SPDX-FileCopyrightText: 2025-2026 Steve Schoettler
// SPDX-License-Identifier: Apache-2.0

//! Live (tier-2) body-block read tests against a running Anytype server.
//!
//! Every fixture is disposable: objects and types are created fresh with
//! unique suffixes, registered with the test context, and removed by its
//! cleanup. The ignored live tier requires explicit disposable-process
//! admission and environment-only credentials (see `anytype::test_util`).

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use anytype::prelude::*;
use anytype::test_util::{DisposableRun, unique_suffix, with_disposable_space_context};

fn assert_disposable_completed(outcome: DisposableRun<()>, callback_ran: &AtomicBool, suite: &str) {
    match outcome {
        DisposableRun::Completed(()) => assert!(callback_ran.load(Ordering::SeqCst)),
        DisposableRun::Skipped(reason) => {
            assert!(!callback_ran.load(Ordering::SeqCst));
            eprintln!("{suite} skipped before callback: {reason:?}");
        }
    }
}

#[tokio::test]
#[ignore = "requires configured real server and disposable test admission"]
#[serial_test::serial(disposable_anytype_api)]
async fn test_body_read_preserves_typed_variants_ids_and_order() {
    let callback_ran = Arc::new(AtomicBool::new(false));
    let callback_flag = callback_ran.clone();
    let outcome = Box::pin(with_disposable_space_context(
        "body-read-typed-order",
        move |ctx| {
            callback_flag.store(true, Ordering::SeqCst);
            Box::pin(async move {
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

                // A second show after the first read's confirmed ObjectClose proves
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
        },
    ))
    .await
    .expect("cleanup-safe typed body live harness");
    assert_disposable_completed(outcome, &callback_ran, "typed body live suite");
}

#[tokio::test]
#[ignore = "requires configured real server and disposable test admission"]
#[serial_test::serial(disposable_anytype_api)]
async fn test_body_read_tightened_limits_reject_real_multi_block_object() {
    let callback_ran = Arc::new(AtomicBool::new(false));
    let callback_flag = callback_ran.clone();
    let outcome = Box::pin(with_disposable_space_context(
        "body-read-limits",
        move |ctx| {
            callback_flag.store(true, Ordering::SeqCst);
            Box::pin(async move {
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
        },
    ))
    .await
    .expect("cleanup-safe body-limits live harness");
    assert_disposable_completed(outcome, &callback_ran, "body-limits live suite");
}

#[tokio::test]
#[ignore = "requires configured real server and disposable test admission"]
#[serial_test::serial(disposable_anytype_api)]
async fn test_body_read_missing_object_returns_public_failure_without_fixture() {
    let callback_ran = Arc::new(AtomicBool::new(false));
    let callback_flag = callback_ran.clone();
    let outcome = Box::pin(with_disposable_space_context(
        "body-read-missing-object",
        move |ctx| {
            callback_flag.store(true, Ordering::SeqCst);
            Box::pin(async move {
                let missing_id = format!("missing-body-{}", unique_suffix());
                let error = ctx
                    .client
                    .blocks()
                    .body(&ctx.space_id, &missing_id)
                    .fetch()
                    .await
                    .expect_err("a never-created object must fail");

                // Once ObjectShow is polled, R4 requires an unconfirmed matching
                // ObjectClose to take precedence over the Show application error.
                // Heart rejects Close for a never-created object, so the public
                // result is the fixed, payload-free cleanup classification.
                assert!(matches!(
                    error,
                    AnytypeError::BodyRpcLifecycle {
                        kind: BodyRpcLifecycleErrorKind::CleanupFailed
                    }
                ));
                Ok(())
            })
        },
    ))
    .await
    .expect("cleanup-safe missing-body live harness");
    assert_disposable_completed(outcome, &callback_ran, "missing-body live suite");
}

#[tokio::test]
#[ignore = "requires configured real server and disposable test admission"]
#[serial_test::serial(disposable_anytype_api)]
async fn test_body_read_reports_dataview_blocks_as_opaque() {
    let callback_ran = Arc::new(AtomicBool::new(false));
    let callback_flag = callback_ran.clone();
    let outcome = Box::pin(with_disposable_space_context(
        "body-read-dataview",
        move |ctx| {
            callback_flag.store(true, Ordering::SeqCst);
            Box::pin(async move {
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
        },
    ))
    .await
    .expect("cleanup-safe dataview-body live harness");
    assert_disposable_completed(outcome, &callback_ran, "dataview-body live suite");
}

/// Returns the object IDs the heart currently reports as open.
///
/// Uses the payload-free `DebugOpenedObjects` RPC so the lifecycle test can
/// observe server-side open state without parsing object content.
async fn opened_object_ids(grpc: &anytype_rpc::client::AnytypeGrpcClient) -> Vec<String> {
    use anytype_rpc::{anytype::rpc::debug::opened_objects, auth::with_token};
    let request = with_token(
        tonic::Request::new(opened_objects::Request {}),
        grpc.token(),
    )
    .expect("attach session token to DebugOpenedObjects");
    let response = grpc
        .client_commands()
        .debug_opened_objects(request)
        .await
        .expect("DebugOpenedObjects transport")
        .into_inner();
    assert!(
        !response.error.is_some_and(|error| error.code != 0),
        "DebugOpenedObjects application failed"
    );
    response.object_i_ds
}

/// Measured `ObjectShow`/`ObjectOpen`/`ObjectClose` lifecycle (`any-cy8q`).
///
/// The heart's `DebugOpenedObjects` RPC is the observation instrument. On the
/// measured heart, an accepted `ObjectShow` builds the view without
/// registering the object in the server-side opened set, so an unclosed show
/// leaks no observable open state; `ObjectOpen` does register the object (the
/// instrument-validation leg) and one `ObjectClose` releases it. The public
/// reader keeps its owned foreground close per the R4 design decision, and
/// the server accepts that close even for a show that held nothing open.
#[tokio::test]
#[ignore = "requires configured real server and disposable test admission"]
#[serial_test::serial(disposable_anytype_api)]
async fn test_body_show_close_lifecycle_holds_no_server_open_state() {
    use anytype_rpc::{
        anytype::rpc::object::{close as object_close, open as object_open, show as object_show},
        auth::with_token,
    };

    let callback_ran = Arc::new(AtomicBool::new(false));
    let callback_flag = callback_ran.clone();
    let outcome = Box::pin(with_disposable_space_context(
        "body-show-close-lifecycle",
        move |ctx| {
            callback_flag.store(true, Ordering::SeqCst);
            Box::pin(async move {
                let name = format!("body-lifecycle-{}", unique_suffix());
                let object = ctx
                    .client
                    .new_object(&ctx.space_id, "page")
                    .name(&name)
                    .body("A paragraph so the shown view is non-trivial.\n")
                    .create()
                    .await?;
                ctx.register_object(&object.id);

                let grpc = ctx.client.grpc_client().await?;
                let baseline = opened_object_ids(&grpc).await;
                assert!(
                    !baseline.contains(&object.id),
                    "object must not be open before any show"
                );

                // Measurement A: a raw ObjectShow with NO ObjectClose does not
                // register the object in the server-side opened set.
                let show_request = with_token(
                    tonic::Request::new(object_show::Request {
                        context_id: object.id.clone(),
                        object_id: object.id.clone(),
                        space_id: ctx.space_id.clone(),
                        ..Default::default()
                    }),
                    grpc.token(),
                )
                .expect("attach session token to ObjectShow");
                let shown = grpc
                    .client_commands()
                    .object_show(show_request)
                    .await
                    .expect("raw ObjectShow transport")
                    .into_inner();
                assert!(
                    !shown.error.is_some_and(|error| error.code != 0),
                    "raw ObjectShow application failed"
                );
                assert!(
                    shown
                        .object_view
                        .is_some_and(|view| !view.blocks.is_empty()),
                    "raw ObjectShow must return a non-empty view"
                );
                let after_show = opened_object_ids(&grpc).await;
                assert!(
                    !after_show.contains(&object.id),
                    "an unclosed ObjectShow must not appear in DebugOpenedObjects \
                     on this heart; got {after_show:?}"
                );

                // Instrument validation: ObjectOpen DOES register the object, so
                // the empty result above is a real absence, not a blind probe.
                let open_request = with_token(
                    tonic::Request::new(object_open::Request {
                        context_id: object.id.clone(),
                        object_id: object.id.clone(),
                        space_id: ctx.space_id.clone(),
                        ..Default::default()
                    }),
                    grpc.token(),
                )
                .expect("attach session token to ObjectOpen");
                let opened = grpc
                    .client_commands()
                    .object_open(open_request)
                    .await
                    .expect("raw ObjectOpen transport")
                    .into_inner();
                assert!(
                    !opened.error.is_some_and(|error| error.code != 0),
                    "raw ObjectOpen application failed"
                );
                let after_open = opened_object_ids(&grpc).await;
                assert!(
                    after_open.contains(&object.id),
                    "an accepted ObjectOpen must register the object as open; \
                     got {after_open:?}"
                );

                // Measurement B: one ObjectClose releases the opened object.
                let close_request = with_token(
                    tonic::Request::new(object_close::Request {
                        context_id: object.id.clone(),
                        object_id: object.id.clone(),
                        space_id: ctx.space_id.clone(),
                    }),
                    grpc.token(),
                )
                .expect("attach session token to ObjectClose");
                let closed = grpc
                    .client_commands()
                    .object_close(close_request)
                    .await
                    .expect("raw ObjectClose transport")
                    .into_inner();
                assert!(
                    !closed.error.is_some_and(|error| error.code != 0),
                    "raw ObjectClose application failed"
                );
                let after_close = opened_object_ids(&grpc).await;
                assert!(
                    !after_close.contains(&object.id),
                    "a confirmed ObjectClose must release the open object"
                );

                // Measurement C: the public reader's owned foreground close is
                // accepted by the server (confirmed counter) and leaves no open
                // object behind.
                let metrics = BodyRpcMetrics::default();
                let config = BodyRpcConfig::default().with_metrics(metrics.clone());
                let snapshot = ctx
                    .client
                    .blocks()
                    .body(&ctx.space_id, &object.id)
                    .rpc_config(config)
                    .fetch()
                    .await?;
                assert_eq!(snapshot.object_id, object.id);
                let after_fetch = opened_object_ids(&grpc).await;
                assert!(
                    !after_fetch.contains(&object.id),
                    "BodyRequest::fetch must not leave the object open"
                );
                let counters = metrics.snapshot();
                assert_eq!(counters.show_attempts, 1);
                assert_eq!(counters.foreground_close_attempts, 1);
                assert_eq!(counters.foreground_close_confirmed, 1);
                assert_eq!(counters.fallback_close_attempts, 0);
                Ok(())
            })
        },
    ))
    .await
    .expect("cleanup-safe show/close lifecycle live harness");
    assert_disposable_completed(outcome, &callback_ran, "show/close lifecycle live suite");
}
