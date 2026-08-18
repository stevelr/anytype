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
        DisposableCallbackStage, DisposableRun, TestError, TestResult, disposable_callback_error,
        unique_suffix, with_disposable_space_context, with_test_context,
    },
};
use common::retry_definitive_rate_limit;
use tokio::time::{Duration, Instant, sleep};

const LIVE_OPERATION_TIMEOUT: Duration = Duration::from_secs(20);

fn assert_disposable_completed(outcome: DisposableRun<()>, callback_ran: &AtomicBool, suite: &str) {
    match outcome {
        DisposableRun::Completed(()) => assert!(callback_ran.load(Ordering::SeqCst)),
        DisposableRun::Skipped(reason) => {
            assert!(!callback_ran.load(Ordering::SeqCst));
            eprintln!("{suite} skipped before callback: {reason:?}");
        }
    }
}

async fn bounded_api<T>(
    operation: &'static str,
    future: impl Future<Output = Result<T, AnytypeError>>,
) -> TestResult<T> {
    tokio::time::timeout(LIVE_OPERATION_TIMEOUT, future)
        .await
        .map_err(|_| TestError::Assertion {
            message: format!("{operation} exceeded its fixed live-test timeout"),
        })?
        .map_err(Into::into)
}

#[tokio::test]
#[ignore = "requires configured real server and disposable test admission"]
#[serial_test::serial(disposable_anytype_api)]
async fn direct_grpc_chat_listing_space_chat_and_edit_text_read_back() {
    let callback_ran = Arc::new(AtomicBool::new(false));
    let callback_flag = callback_ran.clone();
    let stage = Arc::new(AtomicU8::new(0));
    let callback_stage = stage.clone();
    let outcome = Box::pin(with_disposable_space_context(
        "grpc-chat-direct-reads",
        move |ctx| {
            callback_flag.store(true, Ordering::SeqCst);
            Box::pin(async move {
                let callback = async {
                    callback_stage.store(1, Ordering::SeqCst);
                    let chat_name = format!("grpc-chat-direct-reads-{}", unique_suffix());
                let chat = retry_definitive_rate_limit("direct gRPC chat setup", || async {
                    ctx.client
                        .chats()
                        .in_space(&ctx.space_id)
                        .create(
                            &chat_name,
                            Icon::Emoji {
                                emoji: "🧩".to_string(),
                            },
                        )
                        .create()
                        .await
                })
                .await?;
                ctx.register_object(&chat.id);

                callback_stage.store(2, Ordering::SeqCst);
                let message_id = bounded_api(
                    "direct gRPC edit_text setup message",
                    ctx.client
                        .chats()
                        .send_text(&chat.id, "before direct edit")
                        .send(),
                )
                .await?;
                ctx.register_chat_message(&chat.id, &message_id)?;

                callback_stage.store(3, Ordering::SeqCst);
                let deadline = Instant::now() + LIVE_OPERATION_TIMEOUT;
                loop {
                    let chats = bounded_api(
                        "direct global gRPC chat listing",
                        ctx.client.chats().list_chats().limit(100).list(),
                    )
                    .await?;
                    if chats.items.iter().any(|item| item.id == chat.id) {
                        break;
                    }
                    if Instant::now() >= deadline {
                        return Err(TestError::Assertion {
                            message: "created chat was absent from direct global gRPC chat listing within the fixed deadline"
                                .to_owned(),
                        });
                    }
                    sleep(Duration::from_millis(250)).await;
                }

                callback_stage.store(4, Ordering::SeqCst);
                let default_chat = tokio::time::timeout(
                    LIVE_OPERATION_TIMEOUT,
                    ctx.client.chats().space_chat(&ctx.space_id).get(),
                )
                .await
                .map_err(|_| TestError::Assertion {
                    message: "direct gRPC default space chat exceeded its fixed live-test timeout"
                        .to_owned(),
                })?;
                match default_chat {
                    Err(AnytypeError::NotFound { .. }) => {}
                    Err(error) => return Err(error.into()),
                    Ok(_) => {
                        return Err(TestError::Assertion {
                            message: "fresh disposable space unexpectedly resolved a default chat"
                                .to_owned(),
                        });
                    }
                }

                callback_stage.store(5, Ordering::SeqCst);
                bounded_api(
                    "direct gRPC edit_text",
                    ctx.client
                        .chats()
                        .edit_text(&chat.id, &message_id, "after direct edit")
                        .style(MessageTextStyle::Quote)
                        .marks(vec![MessageTextMark {
                            range: Some(MessageTextRange { from: 0, to: 5 }),
                            kind: MessageTextMarkType::Bold,
                            param: None,
                        }])
                        .send(),
                )
                .await?;

                callback_stage.store(6, Ordering::SeqCst);
                let edit_deadline = Instant::now() + LIVE_OPERATION_TIMEOUT;
                loop {
                    let message = bounded_api(
                        "independent REST post-edit chat read",
                        ctx.client
                            .chats()
                            .in_space(&ctx.space_id)
                            .get_message(&chat.id, &message_id)
                            .get(),
                    )
                    .await?;
                    if message.content.text == "after direct edit" {
                        assert_eq!(message.id, message_id);
                        assert!(matches!(message.content.style, MessageTextStyle::Quote));
                        assert!(matches!(
                            message.content.marks.as_slice(),
                            [MessageTextMark {
                                kind: MessageTextMarkType::Bold,
                                range: Some(MessageTextRange { from: 0, to: 5 }),
                                param: None,
                            }]
                        ));
                        break;
                    }
                    if Instant::now() >= edit_deadline {
                        return Err(TestError::Assertion {
                            message: "direct gRPC edit_text was not visible through the independent REST read within the fixed deadline"
                                .to_owned(),
                        });
                    }
                    sleep(Duration::from_millis(250)).await;
                }
                Ok(())
                };
                callback
                    .await
                    .map_err(|error| disposable_callback_error(DisposableCallbackStage::Fixture, error))
            })
        },
    ))
    .await
    .unwrap_or_else(|error| {
        panic!(
            "cleanup-safe direct gRPC chat live harness failed at stage {}: {error:?}",
            stage.load(Ordering::SeqCst)
        )
    });
    assert_disposable_completed(outcome, &callback_ran, "direct gRPC chat live suite");
}

#[tokio::test]
#[ignore = "requires configured real server and disposable test admission"]
#[serial_test::serial(disposable_anytype_api)]
async fn direct_grpc_send_text_and_toggle_reaction_have_independent_readback() {
    let callback_ran = Arc::new(AtomicBool::new(false));
    let callback_flag = callback_ran.clone();
    let outcome = Box::pin(with_disposable_space_context(
        "grpc-chat-direct-mutations",
        move |ctx| {
            callback_flag.store(true, Ordering::SeqCst);
            Box::pin(async move {
                let chat_name = format!("grpc-chat-direct-mutations-{}", unique_suffix());
                let chat = retry_definitive_rate_limit("direct gRPC chat setup", || async {
                    ctx.client
                        .chats()
                        .in_space(&ctx.space_id)
                        .create(
                            &chat_name,
                            Icon::Emoji {
                                emoji: "🧪".to_owned(),
                            },
                        )
                        .create()
                        .await
                })
                .await?;
                ctx.register_object(&chat.id);

                let expected_text = format!("direct gRPC text {}", unique_suffix());
                let message_id = bounded_api(
                    "direct gRPC send_text",
                    ctx.client
                        .chats()
                        .send_text(&chat.id, &expected_text)
                        .send(),
                )
                .await?;
                ctx.register_chat_message(&chat.id, &message_id)?;

                let chats = ctx.client.chats().in_space(&ctx.space_id);
                let text_deadline = Instant::now() + LIVE_OPERATION_TIMEOUT;
                loop {
                    let message = bounded_api(
                        "independent REST send_text read",
                        chats.get_message(&chat.id, &message_id).get(),
                    )
                    .await?;
                    if message.content.text == expected_text {
                        break;
                    }
                    if Instant::now() >= text_deadline {
                        return Err(TestError::Assertion {
                            message: "direct gRPC send_text did not converge through the independent REST read within the fixed deadline"
                                .to_owned(),
                        });
                    }
                    sleep(Duration::from_millis(250)).await;
                }

                let primary = "👍";
                let unrelated = "🎯";
                assert!(
                    bounded_api(
                        "direct gRPC primary reaction add",
                        ctx.client
                            .chats()
                            .toggle_reaction(&chat.id, &message_id, primary)
                            .send(),
                    )
                    .await?
                );
                assert!(
                    bounded_api(
                        "direct gRPC unrelated reaction add",
                        ctx.client
                            .chats()
                            .toggle_reaction(&chat.id, &message_id, unrelated)
                            .send(),
                    )
                    .await?
                );

                let add_deadline = Instant::now() + LIVE_OPERATION_TIMEOUT;
                loop {
                    let message = bounded_api(
                        "independent REST reaction-add read",
                        chats.get_message(&chat.id, &message_id).get(),
                    )
                    .await?;
                    let primary_present = message
                        .reactions
                        .iter()
                        .any(|reaction| reaction.emoji == primary);
                    let unrelated_present = message
                        .reactions
                        .iter()
                        .any(|reaction| reaction.emoji == unrelated);
                    if primary_present && unrelated_present {
                        break;
                    }
                    if Instant::now() >= add_deadline {
                        return Err(TestError::Assertion {
                            message: "direct gRPC reaction additions did not converge through the independent REST read within the fixed deadline"
                                .to_owned(),
                        });
                    }
                    sleep(Duration::from_millis(250)).await;
                }

                assert!(
                    !bounded_api(
                        "direct gRPC primary reaction remove",
                        ctx.client
                            .chats()
                            .toggle_reaction(&chat.id, &message_id, primary)
                            .send(),
                    )
                    .await?
                );
                let remove_deadline = Instant::now() + LIVE_OPERATION_TIMEOUT;
                loop {
                    let message = bounded_api(
                        "independent REST reaction-remove read",
                        chats.get_message(&chat.id, &message_id).get(),
                    )
                    .await?;
                    let primary_present = message
                        .reactions
                        .iter()
                        .any(|reaction| reaction.emoji == primary);
                    let unrelated_present = message
                        .reactions
                        .iter()
                        .any(|reaction| reaction.emoji == unrelated);
                    if !primary_present && unrelated_present {
                        break;
                    }
                    if Instant::now() >= remove_deadline {
                        return Err(TestError::Assertion {
                            message: "direct gRPC reaction removal did not preserve the unrelated reaction through the independent REST read within the fixed deadline"
                                .to_owned(),
                        });
                    }
                    sleep(Duration::from_millis(250)).await;
                }
                Ok(())
            })
        },
    ))
    .await
    .expect("cleanup-safe direct gRPC chat mutation live harness");
    assert_disposable_completed(
        outcome,
        &callback_ran,
        "direct gRPC chat mutation live suite",
    );
}

#[tokio::test]
async fn test_chat_message_crud() -> TestResult<()> {
    with_test_context(|ctx| async move {
        let chat_name = format!("chat-crud-{}", unique_suffix());
        let chat = retry_definitive_rate_limit("chat CRUD setup chat", || async {
            ctx.client
                .chats()
                .in_space(&ctx.space_id)
                .create(
                    &chat_name,
                    Icon::Emoji {
                        emoji: "🧪".to_string(),
                    },
                )
                .create()
                .await
        })
        .await?;
        ctx.register_object(&chat.id);

        let message_id = ctx
            .client
            .chats()
            .add_message(&chat.id)
            .content(MessageContent {
                text: "hello".to_string(),
                style: MessageTextStyle::Paragraph,
                marks: Vec::new(),
            })
            .send()
            .await?;
        ctx.register_chat_message(&chat.id, &message_id)?;

        let page = ctx
            .client
            .chats()
            .list_messages(&chat.id)
            .list_page()
            .await?;
        assert!(page.messages.iter().any(|msg| msg.id == message_id));

        ctx.client
            .chats()
            .edit_message(&chat.id, &message_id)
            .content(MessageContent::new().italic("updated"))
            .send()
            .await?;

        let messages = ctx
            .client
            .chats()
            .get_messages(&chat.id, [&message_id])
            .get()
            .await?;
        assert_eq!(messages[0].content.text, "updated");
        assert!(matches!(
            messages[0].content.marks[0].kind,
            MessageTextMarkType::Italic
        ));

        ctx.client
            .chats()
            .read_messages(&chat.id)
            .read_type(ChatReadType::Messages)
            .mark_read()
            .await?;

        ctx.client
            .chats()
            .unread_messages(&chat.id)
            .read_type(ChatReadType::Messages)
            .after("0000000000000000")
            .mark_unread()
            .await?;

        ctx.client
            .chats()
            .in_space(&ctx.space_id)
            .delete_message(&chat.id, &message_id)
            .await?;
        Ok(())
    })
    .await
}

#[tokio::test]
async fn test_rest_chat_search_reactions_and_reads() -> TestResult<()> {
    with_test_context(|ctx| async move {
        let chats = ctx.client.chats().in_space(&ctx.space_id);
        let chat_name = format!("rest-chat-search-{}", unique_suffix());
        let chat = retry_definitive_rate_limit("REST chat search setup chat", || async {
            chats
                .create(
                    &chat_name,
                    Icon::Emoji {
                        emoji: "🔍".to_string(),
                    },
                )
                .create()
                .await
        })
        .await?;
        ctx.register_object(&chat.id);

        let message_id = chats
            .add_message(
                &chat.id,
                MessageContent::new().text("ambient REST search coverage"),
            )
            .send()
            .await?;
        ctx.register_chat_message(&chat.id, &message_id)?;

        let search_deadline = Instant::now() + LIVE_OPERATION_TIMEOUT;
        loop {
            let matches = chats
                .search_messages(&chat.id, "ambient REST search coverage")
                .limit(20)
                .search()
                .await?;
            if matches
                .items
                .iter()
                .any(|result| result.message.id == message_id)
            {
                break;
            }
            if Instant::now() >= search_deadline {
                return Err(TestError::Assertion {
                    message: "ambient REST message did not become searchable within the fixed live-test deadline"
                        .to_owned(),
                });
            }
            sleep(Duration::from_millis(250)).await;
        }

        chats.toggle_reaction(&chat.id, &message_id, "👍").await?;
        let reacted = chats.get_message(&chat.id, &message_id).get().await?;
        assert!(
            reacted
                .reactions
                .iter()
                .any(|reaction| reaction.emoji == "👍")
        );

        chats.read_messages(&chat.id).mark_read().await?;
        chats.read_reactions(&chat.id).mark_read().await?;
        chats.read_all(&chat.id).await?;
        Ok(())
    })
    .await
}

#[tokio::test]
async fn test_rest_chat_message_crud() -> TestResult<()> {
    with_test_context(|ctx| async move {
        let chats = ctx.client.chats().in_space(&ctx.space_id);
        let chat_name = format!("rest-chat-crud-{}", unique_suffix());
        let chat = retry_definitive_rate_limit("REST chat CRUD setup chat", || async {
            chats
                .create(
                    &chat_name,
                    Icon::Emoji {
                        emoji: "🌐".to_string(),
                    },
                )
                .create()
                .await
        })
        .await?;
        ctx.register_object(&chat.id);

        let message_id = chats
            .add_message(&chat.id, MessageContent::new().bold("hello over REST"))
            .send()
            .await?;
        ctx.register_chat_message(&chat.id, &message_id)?;
        let created = chats.get_message(&chat.id, &message_id).get().await?;
        assert_eq!(created.content.text, "hello over REST");
        assert!(matches!(
            created.content.marks[0].kind,
            MessageTextMarkType::Bold
        ));

        chats
            .edit_message(
                &chat.id,
                &message_id,
                MessageContent::new().italic("updated over REST"),
            )
            .send()
            .await?;
        let updated = chats.get_message(&chat.id, &message_id).get().await?;
        assert_eq!(updated.content.text, "updated over REST");
        assert!(matches!(
            updated.content.marks[0].kind,
            MessageTextMarkType::Italic
        ));

        chats.delete_message(&chat.id, &message_id).await?;
        Ok(())
    })
    .await
}
