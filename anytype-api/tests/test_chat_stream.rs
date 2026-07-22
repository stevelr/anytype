//! Ignored live gRPC and REST chat-stream coverage.
//!
//! Each test registers its chat and message immediately in a fresh
//! prefix-authorized disposable space. Run serially with explicit
//! disposable-process admission.

mod common;

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use anytype::{
    prelude::*,
    test_util::{
        DisposableRun, TestError, TestResult, unique_suffix, with_disposable_space_context,
    },
};
use common::retry_definitive_rate_limit;
use futures::StreamExt;
use tokio::time::{Duration, timeout};

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
