// any-mcp - bounded, workflow-oriented MCP server for Anytype
//
// SPDX-FileCopyrightText: 2026 Steve Schoettler
// SPDX-License-Identifier: Apache-2.0

#![cfg(feature = "acceptance-harness")]

use std::{
    process::Command,
    sync::{Arc, Mutex},
    time::Duration,
};

use anytype::chats::MessageContent;
use anytype::test_util::{
    DisposableRun, TestContext, TestError, TestResult, unique_suffix, with_disposable_space_context,
};
use serde_json::{Value, json};

#[allow(dead_code)]
#[path = "support/process.rs"]
mod process_support;

use process_support::ProtocolProcess;

const TOOL: &str = "object_discussion_get";

struct OwnedDiscussionProcess {
    process: Arc<Mutex<Option<ProtocolProcess>>>,
    metrics_path: std::path::PathBuf,
    preview: bool,
    next_id: u64,
}

impl OwnedDiscussionProcess {
    fn spawn(ctx: &TestContext, mode: &'static str) -> TestResult<Self> {
        let environment = ctx
            .disposable_child_environment()
            .ok_or_else(|| TestError::Assertion {
                message: "disposable callback omitted its child environment".to_owned(),
            })?
            .clone();
        let metrics_path =
            std::env::temp_dir().join(format!("any-mcp-discussions-metrics-{}", unique_suffix()));
        let mut command = Command::new(env!("CARGO_BIN_EXE_any-mcp-discussions-acceptance"));
        command.arg(&metrics_path).arg(mode);
        environment.configure(&mut command)?;
        let process = Arc::new(Mutex::new(Some(ProtocolProcess::spawn_with_deadline(
            command,
            Duration::from_secs(15),
        ))));
        let stopped = Arc::clone(&process);
        let cleanup_path = metrics_path.clone();
        ctx.spawn_owned_child(move || {
            ((), move || {
                let result = stopped
                    .lock()
                    .map_err(|_| TestError::Assertion {
                        message: "discussion child lock failed".to_owned(),
                    })?
                    .take()
                    .map_or(Ok(()), |process| {
                        process
                            .try_finish()
                            .map(|_| ())
                            .map_err(|_| TestError::Assertion {
                                message: "discussion child did not stop cleanly".to_owned(),
                            })
                    });
                let _ = std::fs::remove_file(&cleanup_path);
                result
            })
        })?;
        let mut owned = Self {
            process,
            metrics_path,
            preview: mode == "preview",
            next_id: 1,
        };
        owned.initialize()?;
        Ok(owned)
    }

    fn initialize(&mut self) -> TestResult<()> {
        if self.preview {
            return Ok(());
        }
        let response = self.request(
            "initialize",
            json!({
                "protocolVersion":"2025-11-25",
                "capabilities":{},
                "clientInfo":{"name":"discussion-acceptance","version":"1"}
            }),
        )?;
        if response
            .pointer("/result/protocolVersion")
            .and_then(Value::as_str)
            != Some("2025-11-25")
        {
            return Err(TestError::Assertion {
                message: "discussion child negotiated an unexpected protocol".to_owned(),
            });
        }
        self.with_process(|process| {
            process.notification("notifications/initialized", json!({}));
        })?;
        Ok(())
    }

    fn call(&mut self, arguments: Value) -> TestResult<Value> {
        self.call_named(TOOL, arguments)
    }

    fn call_named(&mut self, name: &str, arguments: Value) -> TestResult<Value> {
        self.request("tools/call", json!({"name":name,"arguments":arguments}))
    }

    fn request(&mut self, method: &str, mut params: Value) -> TestResult<Value> {
        if self.preview {
            let Some(object) = params.as_object_mut() else {
                return Err(TestError::Assertion {
                    message: "discussion child params were not an object".to_owned(),
                });
            };
            object.insert(
                "_meta".to_owned(),
                json!({
                    "io.modelcontextprotocol/protocolVersion":"2026-07-28",
                    "io.modelcontextprotocol/clientInfo":{
                        "name":"discussion-acceptance",
                        "version":"1"
                    },
                    "io.modelcontextprotocol/clientCapabilities":{}
                }),
            );
        }
        let id = self.next_id;
        self.next_id = self
            .next_id
            .checked_add(1)
            .ok_or_else(|| TestError::Assertion {
                message: "discussion child request counter overflowed".to_owned(),
            })?;
        self.with_process(|process| process.request(id, method, params))
    }

    fn with_process<T>(&self, operation: impl FnOnce(&mut ProtocolProcess) -> T) -> TestResult<T> {
        let mut guard = self.process.lock().map_err(|_| TestError::Assertion {
            message: "discussion child lock failed".to_owned(),
        })?;
        let process = guard.as_mut().ok_or_else(|| TestError::Assertion {
            message: "discussion child was already stopped".to_owned(),
        })?;
        Ok(operation(process))
    }

    fn finish(self) -> TestResult<Value> {
        let process = self
            .process
            .lock()
            .map_err(|_| TestError::Assertion {
                message: "discussion child lock failed".to_owned(),
            })?
            .take()
            .ok_or_else(|| TestError::Assertion {
                message: "discussion child was already stopped".to_owned(),
            })?;
        process.try_finish().map_err(|_| TestError::Assertion {
            message: "discussion child did not stop cleanly".to_owned(),
        })?;
        let bytes = std::fs::read(&self.metrics_path).map_err(|_| TestError::Assertion {
            message: "discussion child metrics were unavailable".to_owned(),
        })?;
        let _ = std::fs::remove_file(&self.metrics_path);
        serde_json::from_slice(&bytes).map_err(|_| TestError::Assertion {
            message: "discussion child metrics were malformed".to_owned(),
        })
    }
}

fn result_code(value: &Value) -> Option<&str> {
    value
        .pointer("/result/structuredContent/code")
        .and_then(Value::as_str)
}

fn result_state(value: &Value) -> Option<&str> {
    value
        .pointer("/result/structuredContent/state")
        .and_then(Value::as_str)
}

fn assert_metrics(metrics: &Value) {
    // Six discussion reads plus the required unchanged-ID chat handoff.
    assert_eq!(metrics["http_logical_operations"], 7);
    assert!(
        metrics["http_physical_attempts"]
            .as_u64()
            .is_some_and(|attempts| (7..=42).contains(&attempts))
    );
    assert_eq!(metrics["parent_get_attempts"], 6);
    assert_eq!(metrics["show_attempts"], 6);
    assert_eq!(metrics["accepted_shows"], 6);
    assert_eq!(metrics["close_attempts"], 6);
    assert_eq!(metrics["close_successes"], 6);
    assert_eq!(metrics["write_dispatches"], 0);
}

#[test]
#[ignore = "requires configured disposable real Anytype server"]
#[serial_test::serial(disposable_anytype_api)]
fn cleanup_owned_stable_and_preview_processes_cover_real_discussions() {
    std::thread::Builder::new()
        .name("discussion-process-acceptance".to_owned())
        .stack_size(16 * 1024 * 1024)
        .spawn(|| {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("discussion process runtime")
                .block_on(async {
                    let outcome =
                        with_disposable_space_context("any-mcp-discussions-process", |ctx| {
                            Box::pin(async move {
                                let suffix = unique_suffix();
                                let page = ctx
                                    .client
                                    .new_object(&ctx.space_id, "page")
                                    .name(format!("discussion-process-page-{suffix}"))
                                    .create()
                                    .await?;
                                ctx.register_object(&page.id);
                                let note = ctx
                                    .client
                                    .new_object(&ctx.space_id, "note")
                                    .name(format!("discussion-process-note-{suffix}"))
                                    .create()
                                    .await?;
                                ctx.register_object(&note.id);
                                let action = ctx
                                    .client
                                    .new_object(&ctx.space_id, "task")
                                    .name(format!("discussion-process-action-{suffix}"))
                                    .create()
                                    .await?;
                                ctx.register_object(&action.id);
                                let other_space = ctx
                                    .create_space_fixture(format!(
                                        "any-mcp-discussions-process-other-{suffix}"
                                    ))
                                    .await?;

                                let mut stable = OwnedDiscussionProcess::spawn(&ctx, "stable")?;
                                let mut preview = OwnedDiscussionProcess::spawn(&ctx, "preview")?;
                                let absent_page = json!({"space":ctx.space_id,"object_id":page.id});
                                let absent_note = json!({"space":ctx.space_id,"object_id":note.id});
                                let action_call =
                                    json!({"space":ctx.space_id,"object_id":action.id});
                                let wrong_scope =
                                    json!({"space":other_space.id,"object_id":page.id});

                                for arguments in [&absent_page, &absent_note] {
                                    let stable_result = stable.call(arguments.clone())?;
                                    let preview_result = preview.call(arguments.clone())?;
                                    assert_eq!(result_state(&stable_result), Some("absent"));
                                    assert!(
                                        stable_result.pointer("/result/structuredContent")
                                            == preview_result.pointer("/result/structuredContent")
                                    );
                                }
                                let stable_action = stable.call(action_call)?;
                                let preview_action = preview.call(json!({
                                    "space":ctx.space_id,
                                    "object_id":action.id
                                }))?;
                                assert_eq!(result_code(&stable_action), Some("validation"));
                                assert_eq!(result_code(&preview_action), Some("validation"));

                                let stable_wrong = stable.call(wrong_scope)?;
                                let preview_wrong = preview.call(json!({
                                    "space":other_space.id,
                                    "object_id":page.id
                                }))?;
                                assert!(result_code(&stable_wrong).is_some());
                                assert_eq!(result_code(&stable_wrong), result_code(&preview_wrong));

                                let attached = ctx
                                    .client
                                    .attached_discussion(&ctx.space_id, &page.id)
                                    .ensure()
                                    .await?;
                                let discussion_id = attached
                                    .discussion_id()
                                    .ok_or_else(|| TestError::Assertion {
                                        message: "discussion ensure omitted its derived id"
                                            .to_owned(),
                                    })?
                                    .to_owned();
                                ctx.register_object(&discussion_id);
                                for index in 0..2 {
                                    let message_id = ctx
                                        .client
                                        .chats()
                                        .in_space(&ctx.space_id)
                                        .add_message(
                                            &discussion_id,
                                            MessageContent::new().text(format!(
                                                "discussion-process-{suffix}-{index}"
                                            )),
                                        )
                                        .send()
                                        .await?;
                                    ctx.register_chat_message(&discussion_id, &message_id)?;
                                }
                                let arguments = json!({"space":ctx.space_id,"object_id":page.id});
                                let stable_attached = stable.call(arguments.clone())?;
                                let preview_attached = preview.call(arguments)?;
                                assert_eq!(result_state(&stable_attached), Some("attached"));
                                assert!(
                                    stable_attached.pointer("/result/structuredContent")
                                        == preview_attached.pointer("/result/structuredContent")
                                );
                                assert!(
                                    stable_attached
                                        .pointer("/result/structuredContent/discussion_id")
                                        .and_then(Value::as_str)
                                        == Some(discussion_id.as_str())
                                );

                                for process in [&mut stable, &mut preview] {
                                    let handoff = process.call_named(
                                        "chat_message_list",
                                        json!({
                                            "space":ctx.space_id,
                                            "chat_id":discussion_id,
                                            "limit":8
                                        }),
                                    )?;
                                    assert_eq!(
                                        handoff
                                            .pointer("/result/structuredContent/items")
                                            .and_then(Value::as_array)
                                            .map(Vec::len),
                                        Some(2)
                                    );
                                }

                                let stable_repeat = stable.call(json!({
                                    "space":ctx.space_id,
                                    "object_id":page.id
                                }))?;
                                let preview_repeat = preview.call(json!({
                                    "space":ctx.space_id,
                                    "object_id":page.id
                                }))?;
                                assert!(
                                    stable_repeat.pointer("/result/structuredContent")
                                        == stable_attached.pointer("/result/structuredContent")
                                );
                                assert!(
                                    preview_repeat.pointer("/result/structuredContent")
                                        == preview_attached.pointer("/result/structuredContent")
                                );

                                let stable_metrics = stable.finish()?;
                                let preview_metrics = preview.finish()?;
                                assert_metrics(&stable_metrics);
                                assert_metrics(&preview_metrics);
                                Ok(())
                            })
                        })
                        .await
                        .map_err(|_| "disposable discussion process lifecycle failed")?;
                    assert!(matches!(outcome, DisposableRun::Completed(())));
                    Ok::<(), &'static str>(())
                })
                .expect("discussion process acceptance failed");
        })
        .expect("spawn discussion process thread")
        .join()
        .expect("discussion process thread");
}
