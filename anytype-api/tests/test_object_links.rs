// SPDX-FileCopyrightText: 2026 Steve Schoettler
// SPDX-License-Identifier: Apache-2.0

//! Universal object-link coverage against an isolated real Anytype space.

use anytype::test_util::{DisposableRun, TestResult, unique_suffix, with_disposable_space_context};

#[tokio::test]
#[ignore = "requires configured real server and disposable test admission"]
#[serial_test::serial(disposable_anytype_api)]
async fn object_link_is_local_and_preserves_live_server_health_and_cleanup() {
    let outcome = Box::pin(with_disposable_space_context("object-link", |ctx| {
        Box::pin(async move {
            let object = ctx
                .client
                .new_object(&ctx.space_id, "page")
                .name(format!("object-link-{}", unique_suffix()))
                .create()
                .await?;
            ctx.register_object(&object.id);

            let link = ctx.client.get_share_link(&ctx.space_id, &object.id)?;
            assert_eq!(link, object.get_link());
            assert!(link.contains(&object.id));
            assert!(link.contains(&ctx.space_id));
            ctx.client.ping_http().await?;
            ctx.client.ping_grpc().await?;
            Ok(()) as TestResult<()>
        })
    }))
    .await
    .expect("cleanup-safe universal object-link harness");

    assert!(
        matches!(outcome, DisposableRun::Completed(())),
        "disposable object-link test was not admitted: {outcome:?}"
    );
}
