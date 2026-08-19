// SPDX-FileCopyrightText: 2026 Steve Schoettler
// SPDX-License-Identifier: Apache-2.0

//! Live verification of space-description update semantics (any-e7ei).
//!
//! Proves, against a real server in a cleanup-owned disposable space, that an
//! update which omits the description leaves it untouched, that a string
//! replaces it, and that [`clear_description`] clears it — and that the
//! cleared description reads back as `Some("")`, the same representation a
//! never-described space reports.
//!
//! [`clear_description`]: anytype::spaces::UpdateSpaceRequest::clear_description

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use anytype::test_util::{
    DisposableRun, TestError, retry_definitive_rate_limit, with_disposable_space_context,
};

#[tokio::test]
#[ignore = "requires configured real server and disposable test admission"]
#[serial_test::serial(disposable_anytype_api)]
async fn space_description_omission_replacement_and_clearing_are_distinct() {
    let callback_ran = Arc::new(AtomicBool::new(false));
    let callback_flag = Arc::clone(&callback_ran);
    let outcome = Box::pin(with_disposable_space_context(
        "space-description-clearing",
        move |ctx| {
            callback_flag.store(true, Ordering::SeqCst);
            Box::pin(async move {
                // A freshly created space without a description reports "".
                let created = ctx.client.space(&ctx.space_id).get().await?;
                assert_eq!(created.description.as_deref(), Some(""));
                assert_eq!(created.description_text(), None);
                let name = created.name.clone();

                // Replacement: a string sets the description.
                let replaced = retry_definitive_rate_limit("space description set", || async {
                    ctx.client
                        .update_space(&ctx.space_id)
                        .description("initial description")
                        .no_verify()
                        .update()
                        .await
                })
                .await?;
                assert_eq!(replaced.description.as_deref(), Some("initial description"));
                assert_eq!(replaced.description_text(), Some("initial description"));

                // Omission: an update that does not mention the description
                // leaves it untouched (the name is re-sent unchanged so the
                // prefix-owned space keeps its cleanup identity).
                let renamed = retry_definitive_rate_limit("space name-only update", || async {
                    ctx.client
                        .update_space(&ctx.space_id)
                        .name(&name)
                        .no_verify()
                        .update()
                        .await
                })
                .await?;
                assert_eq!(renamed.description.as_deref(), Some("initial description"));
                let read_back = ctx.client.space(&ctx.space_id).get().await?;
                assert_eq!(
                    read_back.description.as_deref(),
                    Some("initial description")
                );

                // Clearing: the cleared description round-trips as "".
                let cleared = retry_definitive_rate_limit("space description clear", || async {
                    ctx.client
                        .update_space(&ctx.space_id)
                        .clear_description()
                        .no_verify()
                        .update()
                        .await
                })
                .await?;
                assert_eq!(cleared.description.as_deref(), Some(""));
                assert_eq!(cleared.description_text(), None);
                let read_back = ctx.client.space(&ctx.space_id).get().await?;
                assert_eq!(read_back.description.as_deref(), Some(""));
                if read_back.name != name {
                    return Err(TestError::Assertion {
                        message: "space name changed during description updates".to_owned(),
                    });
                }
                Ok(())
            })
        },
    ))
    .await
    .expect("cleanup-safe space description suite");

    match outcome {
        DisposableRun::Completed(()) => assert!(callback_ran.load(Ordering::SeqCst)),
        DisposableRun::Skipped(reason) => {
            assert!(!callback_ran.load(Ordering::SeqCst));
            eprintln!("space description suite skipped before callback: {reason:?}");
        }
    }
}
