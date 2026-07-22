//! Ignored real-server evidence for bounded REST chat prerequisites.
//!
//! Run this test in its own process after sourcing `.test-env` and setting
//! `ANYTYPE_DISPOSABLE_TEST_PROCESS=1`. The disposable harness owns every
//! created chat and message and proves space cleanup after the callback.

mod common;

use std::{
    future::Future,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU8, Ordering},
    },
};

use anytype::{
    prelude::*,
    test_util::{
        DisposableRun, TestError, TestResult, unique_suffix, with_disposable_space_context,
    },
};
use common::retry_definitive_rate_limit;
use tokio::time::{Duration, sleep, timeout};

const LIVE_OPERATION_TIMEOUT: Duration = Duration::from_secs(20);
const TIMESTAMP_TICK: Duration = Duration::from_millis(5);
const EDIT_TIMESTAMP_TICK: Duration = Duration::from_millis(1_100);

async fn bounded_api<T>(
    operation: &'static str,
    future: impl Future<Output = Result<T, AnytypeError>>,
) -> TestResult<T> {
    timeout(LIVE_OPERATION_TIMEOUT, future)
        .await
        .map_err(|_| TestError::Assertion {
            message: format!("{operation} exceeded its fixed live-test timeout"),
        })?
        .map_err(Into::into)
}

fn assert_completed(outcome: DisposableRun<()>, callback_ran: &AtomicBool) {
    match outcome {
        DisposableRun::Completed(()) => assert!(callback_ran.load(Ordering::SeqCst)),
        DisposableRun::Skipped(reason) => {
            assert!(!callback_ran.load(Ordering::SeqCst));
            eprintln!("chat REST prerequisites skipped before callback: {reason:?}");
        }
    }
}

async fn add_registered_message(
    ctx: &anytype::test_util::TestContext,
    chat_id: &str,
    text: &str,
) -> TestResult<String> {
    let message_id = bounded_api(
        "REST chat prerequisite message creation",
        ctx.client
            .chats()
            .in_space(&ctx.space_id)
            .add_message(chat_id, MessageContent::new().text(text))
            .send(),
    )
    .await?;
    ctx.register_chat_message(chat_id, &message_id)?;
    sleep(TIMESTAMP_TICK).await;
    Ok(message_id)
}

#[tokio::test]
#[ignore = "requires configured real server and disposable test admission"]
#[serial_test::serial(disposable_anytype_api)]
async fn test_rest_chat_timestamp_history_and_edit_prerequisites() {
    let callback_ran = Arc::new(AtomicBool::new(false));
    let callback_flag = callback_ran.clone();
    let stage = Arc::new(AtomicU8::new(0));
    let callback_stage = stage.clone();
    let outcome = Box::pin(with_disposable_space_context(
        "chat-rest-prerequisites",
        move |ctx| {
            callback_flag.store(true, Ordering::SeqCst);
            Box::pin(async move {
                callback_stage.store(1, Ordering::SeqCst);
                let chat_name = format!("chat-rest-prerequisites-{}", unique_suffix());
                let chat = retry_definitive_rate_limit("chat REST prerequisite setup", || async {
                    ctx.client
                        .chats()
                        .in_space(&ctx.space_id)
                        .create(
                            &chat_name,
                            Icon::Emoji {
                                emoji: "🧭".to_string(),
                            },
                        )
                        .create()
                        .await
                })
                .await?;
                ctx.register_object(&chat.id);

                callback_stage.store(2, Ordering::SeqCst);
                let first_id = add_registered_message(&ctx, &chat.id, "history-one").await?;
                let second_id = add_registered_message(&ctx, &chat.id, "history-two").await?;
                let third_id = add_registered_message(&ctx, &chat.id, "history-three").await?;
                let fourth_id = add_registered_message(&ctx, &chat.id, "history-four").await?;

                let chats = ctx.client.chats().in_space(&ctx.space_id);
                callback_stage.store(3, Ordering::SeqCst);
                let first_page = bounded_api(
                    "initial bounded older-history page",
                    chats.older_messages(&chat.id).limit(2).get(),
                )
                .await?;
                assert_eq!(
                    first_page
                        .messages
                        .iter()
                        .map(|message| message.id.as_str())
                        .collect::<Vec<_>>(),
                    [third_id.as_str(), fourth_id.as_str()],
                    "each initial REST history window must be oldest to newest"
                );
                let first_anchor = first_page.next_before.ok_or_else(|| TestError::Assertion {
                    message: "full initial history page omitted its opaque successor".to_string(),
                })?;

                callback_stage.store(4, Ordering::SeqCst);
                let newer_id = add_registered_message(&ctx, &chat.id, "history-newer").await?;
                callback_stage.store(5, Ordering::SeqCst);
                let second_page = bounded_api(
                    "stable older-history successor after newer message",
                    chats
                        .older_messages(&chat.id)
                        .before(first_anchor)
                        .limit(2)
                        .get(),
                )
                .await?;
                assert_eq!(
                    second_page
                        .messages
                        .iter()
                        .map(|message| message.id.as_str())
                        .collect::<Vec<_>>(),
                    [first_id.as_str(), second_id.as_str()],
                    "newer insertion must not move the opaque older successor"
                );
                assert!(
                    first_page.messages.iter().all(|first| second_page
                        .messages
                        .iter()
                        .all(|second| first.id != second.id)),
                    "successor history pages must be disjoint"
                );
                let terminal_anchor =
                    second_page
                        .next_before
                        .ok_or_else(|| TestError::Assertion {
                            message: "full successor page omitted its terminal probe anchor"
                                .to_string(),
                        })?;
                callback_stage.store(6, Ordering::SeqCst);
                let terminal = bounded_api(
                    "terminal older-history page",
                    chats
                        .older_messages(&chat.id)
                        .before(terminal_anchor)
                        .limit(2)
                        .get(),
                )
                .await?;
                assert!(terminal.messages.is_empty());
                assert!(terminal.next_before.is_none());

                callback_stage.store(7, Ordering::SeqCst);
                let captured = bounded_api(
                    "exact timestamp prerequisite read",
                    chats.get_message(&chat.id, &newer_id).get(),
                )
                .await?;
                let canonical_created =
                    canonical_chat_timestamp(captured.created_at, ChatTimestampField::CreatedAt)?;
                let canonical_modified =
                    canonical_chat_timestamp(captured.modified_at, ChatTimestampField::ModifiedAt)?;
                assert_eq!(canonical_created.len(), 24);
                assert_eq!(canonical_modified.len(), 24);
                assert!(canonical_created.ends_with('Z'));
                assert!(canonical_modified.ends_with('Z'));

                callback_stage.store(8, Ordering::SeqCst);
                sleep(EDIT_TIMESTAMP_TICK).await;
                let text_edit = bounded_api(
                    "verified REST text edit",
                    chats
                        .edit_message(
                            &chat.id,
                            &newer_id,
                            MessageContent::new().text("edited-plain"),
                        )
                        .send_verified(),
                )
                .await?;
                assert_eq!(text_edit.after.id, newer_id);
                assert_eq!(text_edit.after.content.text, "edited-plain");
                assert!(text_edit.after.modified_at > text_edit.before.modified_at);

                callback_stage.store(9, Ordering::SeqCst);
                sleep(EDIT_TIMESTAMP_TICK).await;
                let rich_edit = bounded_api(
                    "verified REST formatted edit",
                    chats
                        .edit_message(
                            &chat.id,
                            &newer_id,
                            MessageContent::new().bold("edited-plain"),
                        )
                        .send_verified(),
                )
                .await?;
                assert_eq!(rich_edit.after.id, newer_id);
                assert_eq!(rich_edit.before.content.text, "edited-plain");
                assert_eq!(rich_edit.after.content.text, "edited-plain");
                assert!(rich_edit.before.content.marks.is_empty());
                assert!(matches!(
                    rich_edit.after.content.marks.as_slice(),
                    [MessageTextMark {
                        kind: MessageTextMarkType::Bold,
                        ..
                    }]
                ));
                assert!(rich_edit.after.modified_at > rich_edit.before.modified_at);

                let independently_read = bounded_api(
                    "independent post-edit exact read",
                    chats.get_message(&chat.id, &newer_id).get(),
                )
                .await?;
                assert_eq!(independently_read.id, newer_id);
                assert_eq!(independently_read.content.text, "edited-plain");
                assert!(matches!(
                    independently_read.content.marks.as_slice(),
                    [MessageTextMark {
                        kind: MessageTextMarkType::Bold,
                        ..
                    }]
                ));
                assert_eq!(independently_read.modified_at, rich_edit.after.modified_at);
                Ok(())
            })
        },
    ))
    .await
    .unwrap_or_else(|error| {
        panic!(
            "cleanup-safe chat prerequisite live harness failed at stage {}: {error:?}",
            stage.load(Ordering::SeqCst)
        )
    });
    assert_completed(outcome, &callback_ran);
}
