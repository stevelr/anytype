//! Live REST/gRPC integration coverage for the unified file surface.
//!
//! Every test provisions its own cleanup-owned space through
//! [`with_test_context`] and registers each created file so the harness removes
//! it, so no ambient space is mutated.
//!
//! ```bash
//! source .test-env
//! cargo test -p anytype --test test_files -- --test-threads=1
//! ```

use std::time::Duration;

use anytype::{
    files::{FileStyle, FileType},
    prelude::AnytypeError,
    test_util::{TestError, TestResult, unique_suffix, with_test_context},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    task::JoinHandle,
    time::timeout,
};

const FILE_TRANSFER_BUDGET: Duration = Duration::from_secs(30);
const LOOPBACK_HEADER_LIMIT: usize = 16 * 1024;

struct LoopbackFileSource {
    url: String,
    server: Option<JoinHandle<TestResult<LoopbackFileRequest>>>,
}

struct LoopbackFileRequest {
    method: String,
    target: String,
}

impl LoopbackFileSource {
    async fn start(path: &str, body: Vec<u8>) -> TestResult<Self> {
        let listener =
            TcpListener::bind("127.0.0.1:0")
                .await
                .map_err(|error| TestError::Config {
                    message: format!("failed to bind loopback URL source: {error}"),
                })?;
        let address = listener.local_addr().map_err(|error| TestError::Config {
            message: format!("failed to read loopback URL source address: {error}"),
        })?;
        let target = format!("/{path}");
        let expected_target = target.clone();
        let server = tokio::spawn(async move {
            let (mut stream, _) = timeout(FILE_TRANSFER_BUDGET, listener.accept())
                .await
                .map_err(|_| TestError::Config {
                    message: "loopback URL source did not receive a request before its deadline"
                        .to_string(),
                })?
                .map_err(|error| TestError::Config {
                    message: format!("loopback URL source could not accept a request: {error}"),
                })?;

            let mut request = Vec::new();
            let mut chunk = [0_u8; 1024];
            let header_end = loop {
                if let Some(index) = request.windows(4).position(|window| window == b"\r\n\r\n") {
                    break index + 4;
                }
                if request.len() >= LOOPBACK_HEADER_LIMIT {
                    return Err(TestError::Config {
                        message: format!(
                            "loopback URL source request headers exceeded {LOOPBACK_HEADER_LIMIT} bytes"
                        ),
                    });
                }
                let read = timeout(FILE_TRANSFER_BUDGET, stream.read(&mut chunk))
                    .await
                    .map_err(|_| TestError::Config {
                        message: "loopback URL source request read exceeded its deadline"
                            .to_string(),
                    })?
                    .map_err(|error| TestError::Config {
                        message: format!("loopback URL source request read failed: {error}"),
                    })?;
                if read == 0 {
                    return Err(TestError::Config {
                        message: "loopback URL source peer closed before request headers"
                            .to_string(),
                    });
                }
                request.extend_from_slice(&chunk[..read]);
            };
            let header =
                std::str::from_utf8(&request[..header_end]).map_err(|error| TestError::Config {
                    message: format!("loopback URL source request headers were not UTF-8: {error}"),
                })?;
            let request_line = header.lines().next().ok_or_else(|| TestError::Config {
                message: "loopback URL source request had no request line".to_string(),
            })?;
            let mut parts = request_line.split_ascii_whitespace();
            let method = parts.next().ok_or_else(|| TestError::Config {
                message: "loopback URL source request had no method".to_string(),
            })?;
            let target = parts.next().ok_or_else(|| TestError::Config {
                message: "loopback URL source request had no target".to_string(),
            })?;
            let version = parts.next().ok_or_else(|| TestError::Config {
                message: "loopback URL source request had no HTTP version".to_string(),
            })?;
            if parts.next().is_some() || !version.starts_with("HTTP/") {
                return Err(TestError::Config {
                    message: "loopback URL source request line was malformed".to_string(),
                });
            }
            if method != "GET" || target != expected_target {
                return Err(TestError::Config {
                    message: format!(
                        "loopback URL source expected GET {expected_target}, received {method} {target}"
                    ),
                });
            }

            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            timeout(FILE_TRANSFER_BUDGET, async {
                stream.write_all(response.as_bytes()).await?;
                stream.write_all(&body).await?;
                stream.shutdown().await
            })
            .await
            .map_err(|_| TestError::Config {
                message: "loopback URL source response write exceeded its deadline".to_string(),
            })?
            .map_err(|error| TestError::Config {
                message: format!("loopback URL source response write failed: {error}"),
            })?;

            Ok(LoopbackFileRequest {
                method: method.to_string(),
                target: target.to_string(),
            })
        });
        Ok(Self {
            url: format!("http://{address}{target}"),
            server: Some(server),
        })
    }

    async fn finish(mut self) -> TestResult<LoopbackFileRequest> {
        let Some(mut server) = self.server.take() else {
            return Err(TestError::Config {
                message: "loopback URL source was already stopped".to_string(),
            });
        };
        let result = timeout(FILE_TRANSFER_BUDGET, &mut server).await;
        if result.is_err() {
            server.abort();
            let _ = server.await;
            return Err(TestError::Config {
                message: "loopback URL source did not stop before its deadline".to_string(),
            });
        }
        result
            .map_err(|_| TestError::Config {
                message: "loopback URL source did not stop before its deadline".to_string(),
            })?
            .map_err(|error| TestError::Config {
                message: format!("loopback URL source task failed: {error}"),
            })?
    }
}

impl Drop for LoopbackFileSource {
    fn drop(&mut self) {
        if let Some(server) = self.server.take() {
            server.abort();
        }
    }
}

/// Write one local upload fixture, mapping I/O failures onto the harness error.
fn write_fixture(path: &std::path::Path, bytes: &[u8]) -> TestResult<()> {
    std::fs::write(path, bytes).map_err(|error| TestError::Config {
        message: format!("failed to write upload fixture {}: {error}", path.display()),
    })
}

/// Open one local upload fixture for a streamed REST upload.
async fn open_fixture(path: &std::path::Path) -> TestResult<tokio::fs::File> {
    tokio::fs::File::open(path)
        .await
        .map_err(|error| TestError::Config {
            message: format!("failed to open upload fixture {}: {error}", path.display()),
        })
}

#[tokio::test]
async fn test_rest_file_upload_download_and_delete() -> TestResult<()> {
    with_test_context(|ctx| async move {
        let file_name = format!("rest-file-{}.txt", unique_suffix());
        let payload = format!("REST file migration coverage: {file_name}").into_bytes();
        let file = ctx
            .client
            .files()
            .upload(&ctx.space_id)
            .bytes(&file_name, payload.clone())
            .mime("text/plain")
            .upload()
            .await?;
        ctx.register_file(&file.id);

        assert_eq!(file.space_id, ctx.space_id);
        assert_eq!(
            file.name.as_deref(),
            Some(file_name.trim_end_matches(".txt"))
        );
        assert!(
            file.mime
                .as_deref()
                .is_some_and(|mime| mime.starts_with("text/plain"))
        );
        assert_eq!(file.size, Some(payload.len() as i64));

        let downloaded = ctx
            .client
            .files()
            .download_bytes(&ctx.space_id, &file.id)
            .await?;
        assert_eq!(downloaded.as_ref(), payload.as_slice());

        let bounded = ctx
            .client
            .files()
            .download_request(&ctx.space_id, &file.id)
            .response_limit_bytes(payload.len() as u64)
            .error_limit_bytes(64 * 1024)
            .header_evidence_limit_bytes(4096)
            .max_attempts(2)
            .download()
            .await?;
        assert_eq!(bounded.bytes.as_ref(), payload.as_slice());
        assert_eq!(bounded.metadata.content_length, Some(payload.len() as u64));
        assert!(bounded.metadata.retained_header_bytes <= 4096);

        let oversized = ctx
            .client
            .files()
            .download_request(&ctx.space_id, &file.id)
            .response_limit_bytes(payload.len() as u64 - 1)
            .download()
            .await
            .expect_err("caller-specific file ceiling must reject one-over response");
        assert!(matches!(
            oversized,
            AnytypeError::ResponseTooLarge {
                limit,
                declared: Some(declared)
            } if limit == payload.len() as u64 - 1 && declared == payload.len() as u64
        ));

        ctx.client.files().delete(&ctx.space_id, &file.id).await?;
        Ok(())
    })
    .await
}

/// `FileDeleteRequest::permanently` must bypass the bin, so the deleted file is
/// unresolvable rather than merely archived.
///
/// On `anytype-cli` 0.3.6, permanent deletion was measured taking about 154
/// seconds before returning `204 No Content` (tracked as any-18f5). Because the
/// client's 600-second long-operation deadline preserves that behavior. This
/// test retains the tighter 180-second regression budget used for live CLI
/// commands, so a recurrence fails before the production boundary.
#[tokio::test]
async fn test_rest_file_permanent_delete_bypasses_bin() -> TestResult<()> {
    /// Wall-clock ceiling for the permanent delete.
    const PERMANENT_DELETE_BUDGET: Duration = Duration::from_secs(180);

    with_test_context(|ctx| async move {
        let file_name = format!("permanent-{}.txt", unique_suffix());
        let payload = format!("permanent delete coverage: {file_name}").into_bytes();
        let file = ctx
            .client
            .files()
            .upload(&ctx.space_id)
            .bytes(&file_name, payload)
            .mime("text/plain")
            .upload()
            .await?;

        // Deliberately not registered: cleanup deletes every registered file,
        // and a second delete of a permanently removed file fails. The
        // cleanup-owned space is dropped either way, so nothing leaks even if
        // this test fails before the delete below.

        timeout(
            PERMANENT_DELETE_BUDGET,
            ctx.client
                .files()
                .delete_request(&ctx.space_id, &file.id)
                .permanently()
                .delete(),
        )
        .await
        .map_err(|_| TestError::Config {
            message: format!(
                "permanent file delete did not respond within {PERMANENT_DELETE_BUDGET:?}"
            ),
        })??;

        // A permanently deleted file leaves no listed representation behind.
        let remaining = ctx
            .client
            .files()
            .list(&ctx.space_id)
            .limit(100)
            .list()
            .await?;
        assert!(
            remaining.items.iter().all(|item| item.id != file.id),
            "permanently deleted file is still listed"
        );
        Ok(())
    })
    .await
}

/// The documented backend matrix: a plain path upload stays on REST, while any
/// rich option (here `file_type`) promotes the same source to gRPC.
///
/// The two backends are distinguished by their normalized results: the REST
/// response carries no upstream detail struct, whereas the gRPC response is
/// built from one.
#[tokio::test]
async fn test_file_upload_backend_auto_selection() -> TestResult<()> {
    with_test_context(|ctx| async move {
        let suffix = unique_suffix();
        // Harness-owned scratch directory; cleanup removes it with the context.
        let directory = ctx.temp_dir("file_upload_backend")?;
        // Anytype file objects are content addressed, so each source needs
        // distinct bytes or the backends would collapse onto one object id.
        let rest_path = directory.join(format!("rest-{suffix}.txt"));
        let payload = format!("path upload auto-selection (rest): {suffix}");
        write_fixture(&rest_path, payload.as_bytes())?;

        let rest = ctx
            .client
            .files()
            .upload(&ctx.space_id)
            .from_path(&rest_path)
            .upload()
            .await?;
        ctx.register_file(&rest.id);

        assert_eq!(rest.space_id, ctx.space_id);
        assert_eq!(rest.size, Some(payload.len() as i64));
        assert!(
            rest.details.is_null(),
            "REST upload must not synthesize an upstream detail struct"
        );
        assert!(
            rest.mime
                .as_deref()
                .is_some_and(|mime| mime.starts_with("text/plain")),
            "REST upload must report the detected media type"
        );

        let grpc_path = directory.join(format!("grpc-{suffix}.txt"));
        let promoted_payload = format!("path upload auto-selection (grpc): {suffix}");
        write_fixture(&grpc_path, promoted_payload.as_bytes())?;
        let promoted = ctx
            .client
            .files()
            .upload(&ctx.space_id)
            .from_path(&grpc_path)
            .file_type(FileType::File)
            .upload()
            .await?;
        ctx.register_file(&promoted.id);

        assert_eq!(promoted.space_id, ctx.space_id);
        assert_ne!(promoted.id, rest.id);
        assert!(
            promoted.details.is_object(),
            "a rich option must promote the upload to the gRPC backend"
        );
        for key in ["fileId", "sizeInBytes", "fileMimeType"] {
            assert!(
                promoted.details.get(key).is_some(),
                "gRPC upload details are missing {key}"
            );
        }
        assert_eq!(promoted.size, Some(promoted_payload.len() as i64));
        assert!(matches!(promoted.style, FileStyle::Auto));

        // An open file handle is the third REST source and must not be promoted.
        let reader_path = directory.join(format!("reader-{suffix}.txt"));
        let streamed_payload = format!("path upload auto-selection (reader): {suffix}");
        write_fixture(&reader_path, streamed_payload.as_bytes())?;
        let handle = open_fixture(&reader_path).await?;
        let streamed = ctx
            .client
            .files()
            .upload(&ctx.space_id)
            .reader(
                format!("reader-{suffix}.txt"),
                handle,
                streamed_payload.len() as u64,
            )
            .mime("text/plain")
            .upload()
            .await?;
        ctx.register_file(&streamed.id);
        assert!(
            streamed.details.is_null(),
            "a reader source must stay on the REST backend"
        );
        assert_eq!(streamed.size, Some(streamed_payload.len() as i64));
        Ok(())
    })
    .await
}

/// Conditional, ranged, and `HEAD` behavior of `download_request` against the
/// live REST file endpoint.
///
/// `anytype-cli` 0.3.6 advertises `Accept-Ranges: bytes` but supplies neither
/// `ETag` nor `Last-Modified`, so 206, 412, and 416 are asserted
/// unconditionally while the 304 leg stays guarded on a server-supplied
/// validator (tracked as any-5pkh).
#[tokio::test]
async fn test_file_download_conditional_and_range() -> TestResult<()> {
    with_test_context(|ctx| async move {
        let file_name = format!("conditional-{}.txt", unique_suffix());
        let payload: Vec<u8> = (0..512u32).map(|index| (index % 251) as u8).collect();
        let file = ctx
            .client
            .files()
            .upload(&ctx.space_id)
            .bytes(&file_name, payload.clone())
            .mime("application/octet-stream")
            .upload()
            .await?;
        ctx.register_file(&file.id);
        let total = payload.len() as u64;

        // HEAD returns metadata without a body.
        let head = ctx.client.files().metadata(&ctx.space_id, &file.id).await?;
        assert!(head.status.is_success(), "HEAD status {}", head.status);
        assert!(head.bytes.is_empty(), "HEAD must not return a body");
        assert_eq!(head.metadata.content_length, Some(total));
        assert!(head.metadata.retained_header_bytes > 0);
        assert_eq!(
            head.metadata.accept_ranges.as_deref(),
            Some("bytes"),
            "the file endpoint must advertise byte ranges"
        );

        // A checked inclusive range yields 206 with only the requested bytes.
        let ranged = ctx
            .client
            .files()
            .download_request(&ctx.space_id, &file.id)
            .byte_range(0, 100)
            .download()
            .await?;
        assert!(
            ranged.is_partial(),
            "a byte range returned status {}",
            ranged.status
        );
        assert_eq!(ranged.bytes.as_ref(), &payload[..100]);
        assert_eq!(ranged.metadata.content_length, Some(100));
        let content_range = ranged
            .metadata
            .content_range
            .as_deref()
            .expect("206 response must carry Content-Range");
        assert_eq!(content_range, format!("bytes 0-99/{total}"));

        // An unsatisfiable range is reported as 416 with an empty-range
        // Content-Range rather than as a transport error.
        let unsatisfiable = ctx
            .client
            .files()
            .download_request(&ctx.space_id, &file.id)
            .byte_range(total + 1024, 8)
            .download()
            .await?;
        assert_eq!(
            unsatisfiable.status.as_u16(),
            416,
            "unsatisfiable range returned status {}",
            unsatisfiable.status
        );
        assert_eq!(
            unsatisfiable.metadata.content_range.as_deref(),
            Some(format!("bytes */{total}").as_str())
        );

        // A zero-length range is rejected before any network I/O.
        let invalid = ctx
            .client
            .files()
            .download_request(&ctx.space_id, &file.id)
            .byte_range(0, 0)
            .download()
            .await
            .expect_err("a zero-length range must be rejected locally");
        assert!(
            matches!(invalid, AnytypeError::Validation { .. }),
            "unexpected error for a zero-length range: {invalid:?}"
        );

        // A precondition that cannot be satisfied fails closed with 412 and no body.
        let precondition = ctx
            .client
            .files()
            .download_request(&ctx.space_id, &file.id)
            .if_match("\"anytype-never-matches\"")
            .download()
            .await?;
        assert_eq!(
            precondition.status.as_u16(),
            412,
            "unmatched If-Match returned status {}",
            precondition.status
        );
        assert!(precondition.bytes.is_empty(), "412 must have no body");

        // A cache validator that cannot match still yields the complete file.
        let fresh = ctx
            .client
            .files()
            .download_request(&ctx.space_id, &file.id)
            .if_none_match("\"anytype-never-matches\"")
            .download()
            .await?;
        assert!(
            fresh.status.is_success() && !fresh.is_not_modified(),
            "non-matching If-None-Match returned status {}",
            fresh.status
        );
        assert_eq!(fresh.bytes.as_ref(), payload.as_slice());

        // Revalidation against a real validator, once the server offers one.
        if let Some(etag) = head.metadata.etag.clone() {
            let revalidated = ctx
                .client
                .files()
                .download_request(&ctx.space_id, &file.id)
                .if_none_match(&etag)
                .download()
                .await?;
            assert!(
                revalidated.is_not_modified(),
                "matching If-None-Match returned status {}",
                revalidated.status
            );
            assert!(revalidated.bytes.is_empty(), "304 must have no body");
        }

        // A pre-rendered width is ignored for non-image files rather than failing.
        let widened = ctx
            .client
            .files()
            .download_request(&ctx.space_id, &file.id)
            .width(64)
            .head()
            .await?;
        assert!(
            widened.status.is_success(),
            "width on a non-image file returned status {}",
            widened.status
        );

        ctx.client.files().delete(&ctx.space_id, &file.id).await?;
        Ok(())
    })
    .await
}

/// The legacy Heart download writes the selected object to the requested file
/// path without changing its bytes or response identity.
#[tokio::test]
async fn test_grpc_file_download_to_owned_path() -> TestResult<()> {
    with_test_context(|ctx| async move {
        let suffix = unique_suffix();
        let file_name = format!("legacy-download-{suffix}.bin");
        let payload = format!("legacy gRPC download fixture: {suffix}").into_bytes();
        let file = timeout(
            FILE_TRANSFER_BUDGET,
            ctx.client
                .files()
                .upload(&ctx.space_id)
                .bytes(&file_name, payload.clone())
                .mime("application/octet-stream")
                .upload(),
        )
        .await
        .map_err(|_| TestError::Config {
            message: "legacy download fixture upload exceeded its deadline".to_string(),
        })??;
        ctx.register_file(&file.id);

        let directory = ctx.temp_dir("legacy_grpc_download")?;
        let destination = directory.join(&file_name);
        let unsafe_destination = ctx
            .client
            .files()
            .download(&file.id)
            .to_file(&directory)
            .download()
            .await
            .expect_err("a directory cannot be used as a legacy download file path");
        assert!(
            matches!(unsafe_destination, AnytypeError::Validation { .. }),
            "unexpected unsafe download destination error: {unsafe_destination:?}"
        );

        let returned_path = timeout(
            FILE_TRANSFER_BUDGET,
            ctx.client
                .files()
                .download(&file.id)
                .to_file(&destination)
                .download(),
        )
        .await
        .map_err(|_| TestError::Config {
            message: "legacy gRPC download exceeded its deadline".to_string(),
        })??;
        assert_eq!(returned_path, destination);

        let downloaded =
            tokio::fs::read(&destination)
                .await
                .map_err(|error| TestError::Config {
                    message: format!(
                        "failed to read legacy gRPC download {}: {error}",
                        destination.display()
                    ),
                })?;
        assert_eq!(downloaded, payload);

        std::fs::remove_file(&destination).map_err(|error| TestError::Config {
            message: format!(
                "failed to remove legacy gRPC download {}: {error}",
                destination.display()
            ),
        })?;
        assert!(
            !destination.exists(),
            "legacy gRPC download path remained after local cleanup"
        );
        Ok(())
    })
    .await
}

/// A URL source remains on gRPC and fetches only the bytes served by its owned
/// loopback endpoint.
#[tokio::test]
async fn test_grpc_file_upload_from_owned_loopback_url() -> TestResult<()> {
    with_test_context(|ctx| async move {
        let suffix = unique_suffix();
        let source_path = format!("grpc-url-source-{suffix}.bin");
        let payload = format!("gRPC URL upload fixture: {suffix}").into_bytes();
        let source = LoopbackFileSource::start(&source_path, payload.clone()).await?;
        let source_url = source.url.clone();

        let uploaded = timeout(
            FILE_TRANSFER_BUDGET,
            ctx.client
                .files()
                .upload(&ctx.space_id)
                .from_url(source_url)
                .upload(),
        )
        .await
        .map_err(|_| TestError::Config {
            message: "gRPC URL upload exceeded its deadline".to_string(),
        })??;
        ctx.register_file(&uploaded.id);

        let request = source.finish().await?;
        assert_eq!(request.method, "GET");
        assert_eq!(request.target, format!("/{source_path}"));
        assert_eq!(uploaded.space_id, ctx.space_id);
        assert_eq!(uploaded.size, Some(payload.len() as i64));
        assert!(
            uploaded.details.is_object(),
            "URL upload must return the gRPC detail structure"
        );

        let metadata = timeout(
            FILE_TRANSFER_BUDGET,
            ctx.client.files().get(&ctx.space_id, &uploaded.id).get(),
        )
        .await
        .map_err(|_| TestError::Config {
            message: "gRPC URL upload metadata lookup exceeded its deadline".to_string(),
        })??;
        assert_eq!(metadata.id, uploaded.id);
        assert_eq!(metadata.space_id, ctx.space_id);
        assert_eq!(metadata.size, Some(payload.len() as i64));

        let downloaded = timeout(
            FILE_TRANSFER_BUDGET,
            ctx.client
                .files()
                .download_bytes(&ctx.space_id, &uploaded.id),
        )
        .await
        .map_err(|_| TestError::Config {
            message: "gRPC URL upload readback exceeded its deadline".to_string(),
        })??;
        assert_eq!(downloaded.as_ref(), payload.as_slice());
        Ok(())
    })
    .await
}
