mod common;

use std::{
    future::Future,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use anytype::{
    prelude::{
        ProcessCompletionFallback, ProcessKind, ProcessWatchRequest, ProcessWatcher,
        ProcessWatcherTimeouts,
    },
    test_util::{DisposableRun, TestError, TestResult, with_disposable_space_context},
};
use anytype_rpc::{
    anytype::rpc::object::import::{Request as ImportRequest, request as import_request},
    auth::with_token,
    model::r#import::Type as ImportType,
};
use tokio::time::timeout;
use tonic::Request;

const RPC_TIMEOUT: Duration = Duration::from_secs(20);
const EVENT_TIMEOUT: Duration = Duration::from_secs(30);

fn live_failure(category: &'static str, stage: &'static str) -> TestError {
    eprintln!("process-watcher live {category} failure at {stage}");
    TestError::Assertion {
        message: format!("process-watcher live {category} failure at {stage}"),
    }
}

async fn bounded<T>(
    duration: Duration,
    category: &'static str,
    stage: &'static str,
    future: impl Future<Output = T>,
) -> TestResult<T> {
    timeout(duration, future)
        .await
        .map_err(|_| live_failure(category, stage))
}

fn markdown_import_request(space_id: &str, path: &Path) -> ImportRequest {
    ImportRequest {
        space_id: space_id.to_owned(),
        snapshots: Vec::new(),
        update_existing_objects: false,
        r#type: ImportType::Markdown as i32,
        mode: import_request::Mode::AllOrNothing as i32,
        no_progress: false,
        is_migration: false,
        is_new_space: false,
        params: Some(import_request::Params::MarkdownParams(
            import_request::MarkdownParams {
                path: vec![path.to_string_lossy().into_owned()],
                create_directory_pages: false,
                include_properties_as_block: false,
                no_collection: false,
            },
        )),
    }
}

#[tokio::test]
#[ignore = "requires configured real server and disposable test admission"]
async fn watcher_completes_on_real_import_finish_fallback() {
    let callback_ran = Arc::new(AtomicBool::new(false));
    let callback_flag = callback_ran.clone();
    let outcome = Box::pin(with_disposable_space_context(
        "anytype-process-watcher-import",
        move |ctx| {
            callback_flag.store(true, Ordering::SeqCst);
            Box::pin(async move {
                let source_dir = ctx.temp_dir("process_watcher_import")?;
                let source_path = source_dir.join("fallback.md");
                std::fs::write(
                    &source_path,
                    "# Process watcher fallback\n\nCleanup-owned live import fixture.\n",
                )
                .map_err(|_| live_failure("server", "fixture-write"))?;

                let grpc = bounded(
                    RPC_TIMEOUT,
                    "credential",
                    "grpc-client",
                    ctx.client.grpc_client(),
                )
                .await?
                .map_err(|_| live_failure("credential", "grpc-client"))?;
                let timeouts = ProcessWatcherTimeouts {
                    event_stream_connect_timeout: Duration::from_secs(10),
                    process_start_timeout: EVENT_TIMEOUT,
                    process_idle_timeout: EVENT_TIMEOUT,
                    process_done_timeout: EVENT_TIMEOUT,
                };
                let mut watcher = bounded(
                    RPC_TIMEOUT,
                    "server",
                    "subscribe",
                    ProcessWatcher::subscribe(&grpc, timeouts),
                )
                .await?
                .map_err(|_| live_failure("server", "subscribe"))?;

                let operation = async {
                    let import_request = with_token(
                        Request::new(markdown_import_request(&ctx.space_id, &source_path)),
                        grpc.token(),
                    )
                    .map_err(|_| live_failure("credential", "import-auth"))?;
                    let response = bounded(
                        EVENT_TIMEOUT,
                        "server",
                        "import-rpc",
                        grpc.client_commands().object_import(import_request),
                    )
                    .await?
                    .map_err(|_| live_failure("server", "import-rpc"))?
                    .into_inner();
                    if response.error.as_ref().is_some_and(|error| error.code != 0) {
                        return Err(live_failure("server", "import-response"));
                    }

                    let request = ProcessWatchRequest::new(ProcessKind::Import, &ctx.space_id)
                        .allow_empty_space_id(true)
                        .completion_fallback(ProcessCompletionFallback::ImportFinishEvent);
                    bounded(
                        EVENT_TIMEOUT,
                        "event-correlation",
                        "process-wait",
                        watcher.wait_for_process(&grpc, &request, None),
                    )
                    .await?
                    .map_err(|_| live_failure("event-correlation", "process-wait"))?;

                    let process_progress = watcher.progress();
                    if process_progress.import_finish_events == 0 {
                        if process_progress.processes_started != 1
                            || process_progress.processes_done != 1
                        {
                            eprintln!(
                                "process-watcher live initial progress mismatch: import_finish_events={}, processes_started={}, processes_done={}",
                                process_progress.import_finish_events,
                                process_progress.processes_started,
                                process_progress.processes_done,
                            );
                            return Err(live_failure(
                                "event-correlation",
                                "initial-progress",
                            ));
                        }
                        bounded(
                            EVENT_TIMEOUT,
                            "event-correlation",
                            "fallback-wait",
                            watcher.wait_for_process(&grpc, &request, None),
                        )
                        .await?
                        .map_err(|_| live_failure("event-correlation", "fallback-wait"))?;
                    }

                    let progress = watcher.progress();
                    if progress.import_finish_events != 1
                        || progress.import_finish_objects < 1
                        || progress.processes_started != process_progress.processes_started
                        || progress.processes_done != process_progress.processes_done
                    {
                        eprintln!(
                            "process-watcher live fallback progress mismatch: import_finish_events={}, import_finish_objects={}, processes_started={}, processes_done={}",
                            progress.import_finish_events,
                            progress.import_finish_objects,
                            progress.processes_started,
                            progress.processes_done,
                        );
                        return Err(live_failure("event-correlation", "fallback-progress"));
                    }
                    Ok(())
                }
                .await;

                let unsubscribe = bounded(
                    RPC_TIMEOUT,
                    "server",
                    "unsubscribe",
                    watcher.unsubscribe(&grpc),
                )
                .await
                .and_then(|result| result.map_err(|_| live_failure("server", "unsubscribe")));
                match operation {
                    Err(error) => {
                        if unsubscribe.is_err() {
                            eprintln!(
                                "process-watcher live server failure at unsubscribe-after-error"
                            );
                        }
                        Err(error)
                    }
                    Ok(()) => unsubscribe,
                }
            })
        },
    ))
    .await
    .expect("cleanup-safe live ProcessWatcher import harness");
    match outcome {
        DisposableRun::Completed(()) => assert!(callback_ran.load(Ordering::SeqCst)),
        DisposableRun::Skipped(reason) => {
            assert!(!callback_ran.load(Ordering::SeqCst));
            eprintln!("disposable ProcessWatcher suite skipped before callback: {reason:?}");
        }
    }
}
