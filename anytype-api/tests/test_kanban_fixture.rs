// SPDX-FileCopyrightText: 2026 Steve Schoettler
// SPDX-License-Identifier: Apache-2.0

//! Live coverage for cleanup-owned representative Kanban test fixtures.

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use anytype::{
    properties::PropertyFormat,
    test_util::{
        DisposableRun, TestError, retry_definitive_rate_limit, unique_suffix,
        with_disposable_space_context,
    },
};

#[tokio::test]
#[ignore = "requires configured real server and disposable test admission"]
#[serial_test::serial(disposable_anytype_api)]
async fn representative_kanban_fixture_is_verified_and_cleanup_owned() {
    let callback_ran = Arc::new(AtomicBool::new(false));
    let callback_flag = Arc::clone(&callback_ran);
    let outcome = Box::pin(with_disposable_space_context(
        "representative-kanban",
        move |ctx| {
            callback_flag.store(true, Ordering::SeqCst);
            Box::pin(async move {
                let fixture_result = ctx
                    .create_kanban_fixture(format!("Kanban {}", unique_suffix()))
                    .await;
                if let Err(error) = &fixture_result {
                    eprintln!("Kanban fixture setup failed: {error}");
                }
                let mut fixture = fixture_result?;
                ctx.verify_kanban_fixture(&fixture).await?;

                let moving_item = fixture
                    .items
                    .first()
                    .map(|item| item.object.id.clone())
                    .ok_or_else(|| TestError::Assertion {
                        message: "Kanban fixture omitted its first item".to_owned(),
                    })?;
                let destination = fixture
                    .columns
                    .get(1)
                    .map(|column| column.id.clone())
                    .ok_or_else(|| TestError::Assertion {
                        message: "Kanban fixture omitted its destination column".to_owned(),
                    })?;
                ctx.move_kanban_item_fixture(&mut fixture, &moving_item, &destination)
                    .await?;
                assert_eq!(
                    fixture
                        .items
                        .first()
                        .and_then(|item| item.column_id.as_deref()),
                    Some(destination.as_str())
                );

                let mut missing_relation = fixture.clone();
                missing_relation.status_property.key = format!("missing_{}", unique_suffix());
                assert!(ctx.verify_kanban_fixture(&missing_relation).await.is_err());

                let wrong_format = retry_definitive_rate_limit(
                    "kanban wrong-format evidence property",
                    || async {
                        ctx.client
                            .new_property(
                                &ctx.space_id,
                                format!("Wrong format {}", unique_suffix()),
                                PropertyFormat::Number,
                            )
                            .no_verify()
                            .create()
                            .await
                    },
                )
                .await?;
                ctx.register_property(&wrong_format.id);
                let mut wrong_relation = fixture.clone();
                wrong_relation.status_property = wrong_format;
                assert!(ctx.verify_kanban_fixture(&wrong_relation).await.is_err());

                let deleted_tag = fixture
                    .columns
                    .first()
                    .map(|column| column.id.clone())
                    .ok_or_else(|| TestError::Assertion {
                        message: "Kanban fixture omitted its deletable column".to_owned(),
                    })?;
                ctx.client
                    .tag(&ctx.space_id, &fixture.status_property.id, &deleted_tag)
                    .delete()
                    .await?;
                assert!(ctx.verify_kanban_fixture(&fixture).await.is_err());
                Ok(())
            })
        },
    ))
    .await
    .expect("cleanup-safe Kanban fixture suite");

    match outcome {
        DisposableRun::Completed(()) => assert!(callback_ran.load(Ordering::SeqCst)),
        DisposableRun::Skipped(reason) => {
            assert!(!callback_ran.load(Ordering::SeqCst));
            eprintln!("representative Kanban fixture skipped before callback: {reason:?}");
        }
    }
}
