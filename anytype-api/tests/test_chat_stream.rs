//! Ignored live gRPC and REST chat-stream coverage.
//!
//! Each test registers its chat and message immediately in a fresh
//! prefix-authorized disposable space. Run serially with explicit
//! disposable-process admission.

mod common;

use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU8, Ordering},
};

use anytype::{
    prelude::*,
    test_util::{
        DisposableCallbackStage, DisposableRun, TestError, TestResult, disposable_callback_error,
        unique_suffix, with_disposable_space_context,
    },
};
use common::retry_definitive_rate_limit;
use futures::StreamExt;
use tokio::time::{Duration, timeout};

const LIVE_STREAM_TIMEOUT: Duration = Duration::from_secs(10);

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
async fn chat_stream_receives_messages() {
    let callback_ran = Arc::new(AtomicBool::new(false));
    let callback_flag = callback_ran.clone();
    let outcome = Box::pin(with_disposable_space_context(
        "grpc-chat-stream",
        move |ctx| {
            callback_flag.store(true, Ordering::SeqCst);
            Box::pin(async move {
                let chat_name = format!("chat-stream-{}", unique_suffix());
                let chat = retry_definitive_rate_limit("chat stream setup chat", || async {
                    ctx.client
                        .chats()
                        .in_space(&ctx.space_id)
                        .create(
                            &chat_name,
                            Icon::Emoji {
                                emoji: "📡".to_string(),
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
                    .send_text(&chat.id, "hello from the real server")
                    .send()
                    .await?;
                ctx.register_chat_message(&chat.id, &message_id)?;
                // Subscribe after publishing so the real server's initial message
                // snapshot deterministically verifies the gRPC stream conversion.
                let ChatStreamHandle {
                    mut events,
                    control,
                } = ctx.client.chat_stream().subscribe_chat(&chat.id).build();

                let operation = async {
                    let event = timeout(
                        Duration::from_secs(10),
                        wait_for_event(&mut events, |event| {
                            matches!(event, ChatEvent::MessageAdded { .. })
                        }),
                    )
                    .await
                    .map_err(|_| TestError::Assertion {
                        message: "real gRPC chat stream event exceeded its fixed timeout"
                            .to_owned(),
                    })??;

                    match event {
                        ChatEvent::MessageAdded { chat_id, message }
                            if chat_id == chat.id && message.id == message_id =>
                        {
                            Ok(())
                        }
                        _ => Err(TestError::Assertion {
                            message: "gRPC chat stream returned an unexpected message event"
                                .to_owned(),
                        }),
                    }
                }
                .await;
                let shutdown = timeout(Duration::from_secs(10), control.shutdown())
                    .await
                    .map_err(|_| TestError::Assertion {
                        message: "gRPC chat stream shutdown exceeded its fixed timeout".to_owned(),
                    })
                    .and_then(|result| result.map_err(TestError::from));
                match operation {
                    Err(error) => {
                        if shutdown.is_err() {
                            eprintln!("gRPC chat-stream shutdown failed after stream error");
                        }
                        Err(error)
                    }
                    Ok(()) => shutdown,
                }
            })
        },
    ))
    .await
    .expect("cleanup-safe gRPC chat-stream live harness");
    assert_disposable_completed(outcome, &callback_ran, "gRPC chat-stream live suite");
}

#[tokio::test]
#[ignore = "requires configured real server and disposable test admission"]
#[serial_test::serial(disposable_anytype_api)]
async fn rest_chat_stream_receives_initial_message() {
    let callback_ran = Arc::new(AtomicBool::new(false));
    let callback_flag = callback_ran.clone();
    let outcome = Box::pin(with_disposable_space_context(
        "rest-chat-stream",
        move |ctx| {
            callback_flag.store(true, Ordering::SeqCst);
            Box::pin(async move {
                let chats = ctx.client.chats().in_space(&ctx.space_id);
                let chat_name = format!("rest-chat-stream-{}", unique_suffix());
                let chat = retry_definitive_rate_limit("REST SSE setup chat", || async {
                    chats
                        .create(
                            &chat_name,
                            Icon::Emoji {
                                emoji: "📨".to_string(),
                            },
                        )
                        .create()
                        .await
                })
                .await?;
                ctx.register_object(&chat.id);
                let message_id = retry_definitive_rate_limit("REST SSE setup message", || async {
                    chats
                        .add_message(&chat.id, MessageContent::new().text("hello from REST SSE"))
                        .send()
                        .await
                })
                .await?;
                ctx.register_chat_message(&chat.id, &message_id)?;

                let mut events = chats
                    .message_stream(&chat.id)
                    .limit(1)
                    .heartbeat_seconds(1)
                    .open()
                    .await?;
                let event = timeout(Duration::from_secs(10), events.next())
                    .await
                    .map_err(|_| TestError::Assertion {
                        message: "REST chat stream event exceeded its fixed timeout".to_owned(),
                    })?
                    .ok_or_else(|| TestError::Assertion {
                        message: "REST chat stream ended before its initial message".to_owned(),
                    })??;
                assert!(matches!(
                    event,
                    ChatHttpEvent::MessageAdded { message } if message.id == message_id
                ));

                Ok(())
            })
        },
    ))
    .await
    .expect("cleanup-safe REST chat-stream live harness");
    assert_disposable_completed(outcome, &callback_ran, "REST chat-stream live suite");
}

#[tokio::test]
#[ignore = "requires configured real server and disposable test admission"]
#[serial_test::serial(disposable_anytype_api)]
async fn chat_stream_selectively_unsubscribes_one_of_two_chats() {
    let callback_ran = Arc::new(AtomicBool::new(false));
    let callback_flag = callback_ran.clone();
    let stage = Arc::new(AtomicU8::new(0));
    let callback_stage = stage.clone();
    let outcome = Box::pin(with_disposable_space_context(
        "grpc-chat-selective-unsubscribe",
        move |ctx| {
            callback_flag.store(true, Ordering::SeqCst);
            Box::pin(async move {
                let callback = async {
                    let first = create_registered_chat(&ctx, "first").await?;
                    let second = create_registered_chat(&ctx, "second").await?;

                    let ChatStreamHandle {
                        mut events,
                        control,
                    } = ctx.client.chat_stream().build();
                    let operation = async {
                        timeout(LIVE_STREAM_TIMEOUT, control.subscribe_chat(&first.id))
                            .await
                            .map_err(|_| TestError::Assertion {
                                message: "first chat subscription exceeded its fixed timeout"
                                    .to_owned(),
                            })??;
                        timeout(LIVE_STREAM_TIMEOUT, control.subscribe_chat(&second.id))
                            .await
                            .map_err(|_| TestError::Assertion {
                                message: "second chat subscription exceeded its fixed timeout"
                                    .to_owned(),
                            })??;

                        let first_initial =
                            send_registered_text(&ctx, &first.id, "first initial").await?;
                        let second_initial =
                            send_registered_text(&ctx, &second.id, "second initial").await?;

                        callback_stage.store(5, Ordering::SeqCst);
                        wait_for_initial_messages(
                            &mut events,
                            (&first.id, &first_initial),
                            (&second.id, &second_initial),
                        )
                        .await?;

                        timeout(LIVE_STREAM_TIMEOUT, control.unsubscribe_chat(&first.id))
                            .await
                            .map_err(|_| TestError::Assertion {
                                message: "first chat unsubscribe exceeded its fixed timeout"
                                    .to_owned(),
                            })??;

                        let first_sentinel =
                            send_registered_text(&ctx, &first.id, "first unsubscribed sentinel")
                                .await?;
                        let second_sentinel =
                            send_registered_text(&ctx, &second.id, "second retained sentinel")
                                .await?;

                        wait_for_exact_message(
                            &mut events,
                            &second.id,
                            &second_sentinel,
                            Some((&first.id, &first_sentinel)),
                        )
                        .await?;
                        reject_stale_message(&mut events, &first.id, &first_sentinel).await
                    }
                    .await;
                    let shutdown = timeout(LIVE_STREAM_TIMEOUT, control.shutdown())
                        .await
                        .map_err(|_| TestError::Assertion {
                            message: "selective chat-stream shutdown exceeded its fixed timeout"
                                .to_owned(),
                        })
                        .and_then(|result| result.map_err(TestError::from));
                    match operation {
                        Err(error) => {
                            if shutdown.is_err() {
                                eprintln!(
                                    "selective gRPC chat-stream shutdown failed after stream error"
                                );
                            }
                            Err(error)
                        }
                        Ok(()) => shutdown,
                    }
                };
                callback.await.map_err(|error| {
                    disposable_callback_error(DisposableCallbackStage::Fixture, error)
                })
            })
        },
    ))
    .await
    .unwrap_or_else(|error| {
        panic!(
            "cleanup-safe selective gRPC chat-stream live harness failed at stage {}: {error:?}",
            stage.load(Ordering::SeqCst)
        )
    });
    assert_disposable_completed(
        outcome,
        &callback_ran,
        "selective gRPC chat-stream live suite",
    );
}

async fn create_registered_chat(
    ctx: &anytype::test_util::TestContext,
    suffix: &str,
) -> TestResult<Object> {
    let chat_name = format!("chat-stream-selective-{suffix}-{}", unique_suffix());
    let chat = retry_definitive_rate_limit("selective chat stream setup chat", || async {
        ctx.client
            .chats()
            .in_space(&ctx.space_id)
            .create(
                &chat_name,
                Icon::Emoji {
                    emoji: "🔀".to_string(),
                },
            )
            .create()
            .await
    })
    .await?;
    ctx.register_object(&chat.id);
    Ok(chat)
}

async fn send_registered_text(
    ctx: &anytype::test_util::TestContext,
    chat_id: &str,
    text: &str,
) -> TestResult<String> {
    let message_id = timeout(
        LIVE_STREAM_TIMEOUT,
        ctx.client
            .chats()
            .in_space(&ctx.space_id)
            .add_message(chat_id, MessageContent::new().text(text))
            .send(),
    )
    .await
    .map_err(|_| TestError::Assertion {
        message: "registered chat message send exceeded its fixed live-test timeout".to_owned(),
    })??;
    ctx.register_chat_message(chat_id, &message_id)?;
    Ok(message_id)
}

async fn wait_for_exact_message(
    events: &mut ChatEventStream,
    expected_chat_id: &str,
    expected_message_id: &str,
    rejected: Option<(&str, &str)>,
) -> TestResult<()> {
    timeout(LIVE_STREAM_TIMEOUT, async {
        loop {
            let event = events.next().await.ok_or_else(|| TestError::Assertion {
                message: "gRPC chat event stream ended while awaiting a routed message".to_owned(),
            })?;
            if let ChatEvent::MessageAdded { chat_id, message } = event {
                if rejected.is_some_and(|(rejected_chat_id, rejected_message_id)| {
                    chat_id == rejected_chat_id && message.id == rejected_message_id
                }) {
                    return Err(TestError::Assertion {
                        message: "unsubscribed chat emitted its sentinel message".to_owned(),
                    });
                }
                if chat_id == expected_chat_id && message.id == expected_message_id {
                    return Ok(());
                }
                return Err(TestError::Assertion {
                    message:
                        "gRPC chat stream emitted a message for an unexpected chat or message id"
                            .to_owned(),
                });
            }
        }
    })
    .await
    .map_err(|_| TestError::Assertion {
        message: "gRPC chat stream did not route its expected sentinel within the fixed timeout"
            .to_owned(),
    })?
}

async fn wait_for_initial_messages(
    events: &mut ChatEventStream,
    first: (&str, &str),
    second: (&str, &str),
) -> TestResult<()> {
    timeout(LIVE_STREAM_TIMEOUT, async {
        let mut received_first = false;
        let mut received_second = false;
        while !received_first || !received_second {
            let event = events.next().await.ok_or_else(|| TestError::Assertion {
                message: "gRPC chat event stream ended while confirming both subscriptions"
                    .to_owned(),
            })?;
            if let ChatEvent::MessageAdded { chat_id, message } = event {
                if chat_id == first.0 && message.id == first.1 {
                    received_first = true;
                } else if chat_id == second.0 && message.id == second.1 {
                    received_second = true;
                } else {
                    return Err(TestError::Assertion {
                        message: "gRPC chat stream emitted an unexpected message while confirming subscriptions"
                            .to_owned(),
                    });
                }
            }
        }
        Ok(())
    })
    .await
    .map_err(|_| TestError::Assertion {
        message: "gRPC chat stream did not confirm both subscriptions within the fixed timeout"
            .to_owned(),
    })?
}

async fn reject_stale_message(
    events: &mut ChatEventStream,
    unsubscribed_chat_id: &str,
    unsubscribed_message_id: &str,
) -> TestResult<()> {
    match timeout(LIVE_STREAM_TIMEOUT, async {
        loop {
            let event = events.next().await.ok_or_else(|| TestError::Assertion {
                message: "gRPC chat event stream ended during stale-route observation".to_owned(),
            })?;
            if let ChatEvent::MessageAdded { chat_id, message } = event
                && chat_id == unsubscribed_chat_id
                && message.id == unsubscribed_message_id
            {
                return Err(TestError::Assertion {
                    message:
                        "unsubscribed chat emitted a stale sentinel after retained routing continued"
                            .to_owned(),
                });
            }
        }
    })
    .await
    {
        Err(_) => Ok(()),
        Ok(result) => result,
    }
}

async fn wait_for_event<F>(events: &mut ChatEventStream, predicate: F) -> TestResult<ChatEvent>
where
    F: Fn(&ChatEvent) -> bool,
{
    loop {
        if let Some(event) = events.next().await {
            if predicate(&event) {
                return Ok(event);
            }
        } else {
            return Err(TestError::Assertion {
                message: "gRPC chat event stream ended".to_owned(),
            });
        }
    }
}
