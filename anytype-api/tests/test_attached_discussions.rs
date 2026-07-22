// SPDX-FileCopyrightText: 2025-2026 Steve Schoettler
// SPDX-License-Identifier: Apache-2.0

//! Live attached-discussion tests against a cleanup-owned real Anytype space.
//!
//! Run serially with explicit disposable-process admission. No semantic mock
//! or transport-fault fixture is used.

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use anytype::attached_discussions::AttachedDiscussionMetricsSnapshot;
use anytype::test_util::{DisposableRun, unique_suffix, with_disposable_space_context};

#[tokio::test]
#[ignore = "requires configured real server and disposable test admission"]
#[serial_test::serial(disposable_anytype_api)]
async fn test_attached_discussion_get_ensure_and_repeat_are_exact() {
    let callback_ran = Arc::new(AtomicBool::new(false));
    let callback_flag = Arc::clone(&callback_ran);
    let outcome = Box::pin(with_disposable_space_context(
        "attached-discussion",
        move |ctx| {
            callback_flag.store(true, Ordering::SeqCst);
            Box::pin(async move {
                let parent = ctx
                    .client
                    .new_object(&ctx.space_id, "page")
                    .name(format!("attached-discussion-{}", unique_suffix()))
                    .create()
                    .await?;
                ctx.register_object(&parent.id);
                let http_before = ctx.client.http_metrics();
                let work_before = ctx.client.attached_discussion_metrics();

                let absent = ctx
                    .client
                    .attached_discussion(&ctx.space_id, &parent.id)
                    .get()
                    .await?;
                assert_eq!(absent.space_id(), ctx.space_id);
                assert_eq!(absent.parent_id(), parent.id);
                assert_eq!(absent.discussion_id(), None);

                let attached = ctx
                    .client
                    .attached_discussion(&ctx.space_id, &parent.id)
                    .ensure()
                    .await?;
                let discussion_id = attached
                    .discussion_id()
                    .expect("ensure returns a verified attached state")
                    .to_owned();
                ctx.register_object(&discussion_id);
                assert_eq!(attached.space_id(), ctx.space_id);
                assert_eq!(attached.parent_id(), parent.id);

                let reread = ctx
                    .client
                    .attached_discussion(&ctx.space_id, &parent.id)
                    .get()
                    .await?;
                assert_eq!(reread.discussion_id(), Some(discussion_id.as_str()));

                let repeated = ctx
                    .client
                    .attached_discussion(&ctx.space_id, &parent.id)
                    .ensure()
                    .await?;
                assert_eq!(repeated.discussion_id(), Some(discussion_id.as_str()));

                let http_after = ctx.client.http_metrics();
                let work_after = ctx.client.attached_discussion_metrics();
                assert_eq!(
                    http_after.logical_operations - http_before.logical_operations,
                    4
                );
                assert_eq!(
                    http_after.physical_attempts - http_before.physical_attempts,
                    4
                );
                assert_eq!(
                    metrics_delta(work_after, work_before),
                    AttachedDiscussionMetricsSnapshot {
                        parent_get_attempts: 4,
                        show_attempts: 8,
                        accepted_shows: 8,
                        close_attempts: 8,
                        close_successes: 8,
                        write_dispatches: 1,
                        reconciliation_attempts: 1,
                    }
                );
                Ok(())
            })
        },
    ))
    .await
    .expect("cleanup-safe attached-discussion live harness");

    match outcome {
        DisposableRun::Completed(()) => assert!(callback_ran.load(Ordering::SeqCst)),
        DisposableRun::Skipped(reason) => {
            assert!(!callback_ran.load(Ordering::SeqCst));
            eprintln!("attached-discussion live suite skipped before callback: {reason:?}");
        }
    }
}

fn metrics_delta(
    after: AttachedDiscussionMetricsSnapshot,
    before: AttachedDiscussionMetricsSnapshot,
) -> AttachedDiscussionMetricsSnapshot {
    AttachedDiscussionMetricsSnapshot {
        parent_get_attempts: after.parent_get_attempts - before.parent_get_attempts,
        show_attempts: after.show_attempts - before.show_attempts,
        accepted_shows: after.accepted_shows - before.accepted_shows,
        close_attempts: after.close_attempts - before.close_attempts,
        close_successes: after.close_successes - before.close_successes,
        write_dispatches: after.write_dispatches - before.write_dispatches,
        reconciliation_attempts: after.reconciliation_attempts - before.reconciliation_attempts,
    }
}
