// any-mcp - bounded, workflow-oriented MCP server for Anytype
//
// SPDX-FileCopyrightText: 2026 Steve Schoettler
// SPDX-License-Identifier: Apache-2.0

//! Stateless experimental 2026-07-28 preview over bounded HTTP POSTs.
//!
//! With `ANY_MCP_PROTOCOL=experimental-2026-07-28` and the HTTP transport
//! selected, POST `/mcp` accepts one bounded stateless preview request and
//! returns one `application/json` response. There is no initialize session
//! and no server stream, so GET and DELETE return 405. Decoding, `_meta`
//! validation, and method routing are the shared transport-neutral adapter
//! in [`crate::preview`]; this module owns only HTTP concerns and the
//! principal-partitioned stateless cancellation registry. Preview cursors
//! live on the per-principal server facade, so they are principal-keyed as
//! the design requires.

use std::{collections::HashMap, sync::Arc};

use bytes::Bytes;
use http::{HeaderValue, Method, Response, StatusCode, header};
use http_body_util::{BodyExt, Full};
use serde_json::Value;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use crate::{
    http::{
        listener::{AdmittedRequest, HttpBody, fixed_response},
        session::PrincipalServers,
    },
    preview::{
        PreviewClassification, PreviewRequest, classify_preview_frame, dispatch_modern,
        invalid_request,
    },
    runtime::RuntimeContext,
};

/// Responses inherit the fixed 2 MiB protocol frame ceiling.
const MAX_RESPONSE_BYTES: usize = 2 * 1024 * 1024;
const PREVIEW_ALLOW: &str = "POST, OPTIONS";

type InflightKey = ([u8; 32], String);

/// Preview-mode MCP backend: one bounded POST, one bounded JSON response.
pub(crate) struct PreviewBackend {
    servers: PrincipalServers,
    /// Stateless in-flight cancellation registry keyed by authenticated
    /// principal and exact request key, so one principal can neither observe
    /// nor cancel another principal's requests.
    inflight: Mutex<HashMap<InflightKey, CancellationToken>>,
}

impl PreviewBackend {
    pub(crate) fn new(runtime: RuntimeContext) -> Self {
        Self {
            servers: PrincipalServers::new(runtime),
            inflight: Mutex::new(HashMap::new()),
        }
    }

    /// Handles one admitted preview-mode request.
    pub(crate) async fn call(self: Arc<Self>, admitted: AdmittedRequest) -> Response<HttpBody> {
        let AdmittedRequest {
            parts,
            body,
            principal,
            invocation: _,
        } = admitted;

        // The preview has no initialize session or server stream.
        if parts.method != Method::POST {
            let mut response = fixed_response(StatusCode::METHOD_NOT_ALLOWED, "Method Not Allowed");
            response
                .headers_mut()
                .insert(header::ALLOW, HeaderValue::from_static(PREVIEW_ALLOW));
            return response;
        }
        let json_content_type = parts
            .headers
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.starts_with("application/json"));
        if !json_content_type {
            return fixed_response(StatusCode::UNSUPPORTED_MEDIA_TYPE, "Unsupported Media Type");
        }
        let json_acceptable = parts
            .headers
            .get(header::ACCEPT)
            .and_then(|value| value.to_str().ok())
            .is_none_or(|value| value.contains("application/json") || value.contains("*/*"));
        if !json_acceptable {
            return fixed_response(StatusCode::NOT_ACCEPTABLE, "Not Acceptable");
        }

        let request = match classify_preview_frame(&body) {
            PreviewClassification::Response(response) => return json_response(&response),
            PreviewClassification::CancelRequested(request_key) => {
                if let Some(cancellation) = self
                    .inflight
                    .lock()
                    .await
                    .get(&(*principal.key(), request_key))
                {
                    cancellation.cancel();
                }
                return accepted_response();
            }
            PreviewClassification::Ignored => return accepted_response(),
            PreviewClassification::Request(request) => request,
        };
        let PreviewRequest {
            id,
            request_key,
            method,
            params,
        } = request;

        let Ok(server) = self.servers.server_for(&principal) else {
            return fixed_response(StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error");
        };

        let inflight_key = (*principal.key(), request_key);
        let cancellation = CancellationToken::new();
        {
            let mut inflight = self.inflight.lock().await;
            if inflight.contains_key(&inflight_key) {
                drop(inflight);
                return json_response(&invalid_request(id));
            }
            inflight.insert(inflight_key.clone(), cancellation.clone());
        }

        let response = tokio::select! {
            biased;
            () = cancellation.cancelled() => None,
            response = dispatch_modern(&server, id, &method, params, &cancellation) => {
                Some(response)
            }
        };
        self.inflight.lock().await.remove(&inflight_key);

        response.map_or_else(accepted_response, |response| json_response(&response))
    }
}

fn json_response(value: &Value) -> Response<HttpBody> {
    let Ok(encoded) = serde_json::to_vec(value) else {
        return fixed_response(StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error");
    };
    if encoded.len() > MAX_RESPONSE_BYTES {
        return fixed_response(StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error");
    }
    let mut response = Response::new(Full::new(Bytes::from(encoded)).boxed());
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    response
}

/// A consumed notification or an explicitly cancelled request has no
/// JSON-RPC response; HTTP acknowledges it with an empty 202.
fn accepted_response() -> Response<HttpBody> {
    let mut response = Response::new(Full::new(Bytes::new()).boxed());
    *response.status_mut() = StatusCode::ACCEPTED;
    response
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use anytype::prelude::{AnytypeClient, ClientConfig};
    use http::Request;
    use serde_json::json;

    use super::*;
    use crate::{
        config::ApplicationProfile,
        http::{
            auth::{Authenticator, AuthorizedPrincipal},
            listener::{ListenerState, McpService, handle_request},
        },
        runtime::{RuntimeContext, StartupStatus},
    };

    fn test_runtime() -> RuntimeContext {
        test_runtime_with_timeout(Duration::from_secs(5))
    }

    fn test_runtime_with_timeout(timeout: Duration) -> RuntimeContext {
        let config = ClientConfig {
            base_url: Some("http://127.0.0.1:1".to_string()),
            keystore: Some("env".to_string()),
            keystore_service: Some("any-mcp-preview-test".to_string()),
            app_name: "any-mcp-preview-test".to_string(),
            ..ClientConfig::default()
        };
        let client = AnytypeClient::with_config(config).expect("test client");
        RuntimeContext::from_parts_with_profile(
            client,
            4,
            timeout,
            StartupStatus {
                http_available: true,
                grpc_available: false,
            },
            ApplicationProfile::Compact,
            false,
        )
    }

    fn backend() -> Arc<PreviewBackend> {
        Arc::new(PreviewBackend::new(test_runtime()))
    }

    fn principal(name: &str) -> AuthorizedPrincipal {
        AuthorizedPrincipal::from_identity_material("test", name.as_bytes())
    }

    fn admitted(
        method: Method,
        headers: &[(&str, &str)],
        body: &str,
        principal: &AuthorizedPrincipal,
    ) -> AdmittedRequest {
        let mut builder = Request::builder()
            .method(method)
            .uri("/mcp")
            .header("host", "localhost:8000")
            .header("accept", "application/json")
            .header("content-type", "application/json");
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        let (parts, ()) = builder.body(()).expect("test request").into_parts();
        AdmittedRequest {
            parts,
            body: Bytes::copy_from_slice(body.as_bytes()),
            principal: principal.clone(),
            invocation: crate::runtime::InvocationAnchor::capture_durations(
                Duration::from_secs(5),
                Duration::from_secs(300),
            ),
        }
    }

    fn preview_request(id: u64, method: &str, params: Value) -> String {
        let mut params = params;
        params.as_object_mut().expect("params object").insert(
            "_meta".to_owned(),
            json!({
                "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                "io.modelcontextprotocol/clientInfo": {
                    "name": "test-client",
                    "version": "1.0.0"
                },
                "io.modelcontextprotocol/clientCapabilities": {}
            }),
        );
        json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        })
        .to_string()
    }

    fn preview_listener_with_claim_barrier(
        timeout: Duration,
        claimed: Arc<std::sync::Barrier>,
        release: Arc<std::sync::Barrier>,
    ) -> (
        Arc<ListenerState>,
        tokio::sync::mpsc::UnboundedReceiver<crate::runtime::InvocationAnchor>,
    ) {
        let runtime = test_runtime_with_timeout(timeout);
        let backend = Arc::new(PreviewBackend::new(runtime.clone()));
        let config = crate::http::listener::tests::test_config(&[]);
        let (anchor_sender, anchor_receiver) = tokio::sync::mpsc::unbounded_channel();
        let ingress_runtime = runtime.clone();
        let service: McpService = Arc::new(move |admitted| {
            let backend = Arc::clone(&backend);
            let runtime = ingress_runtime.clone();
            admitted
                .invocation
                .arm_dispatch_claim_barrier(Arc::clone(&claimed), Arc::clone(&release));
            let _ = anchor_sender.send(admitted.invocation.clone());
            Box::pin(async move {
                let invocation = admitted.invocation.clone();
                runtime
                    .scope_ingress(invocation, backend.call(admitted))
                    .await
            })
        });
        let state = ListenerState::new_with_runtime(
            &config,
            Authenticator::SyntheticAllow,
            None,
            service,
            &runtime,
        );
        (Arc::new(state), anchor_receiver)
    }

    fn preview_listener_request(body: String) -> Request<Full<Bytes>> {
        Request::builder()
            .method(Method::POST)
            .uri("/mcp")
            .header("host", "localhost:8000")
            .header("authorization", "Bearer synthetic-token")
            .header("accept", "application/json")
            .header("content-type", "application/json")
            .body(Full::new(Bytes::from(body)))
            .expect("preview listener request")
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn preview_http_terminal_wins_a_claiming_deadline_without_dispatch() {
        let claimed = Arc::new(std::sync::Barrier::new(2));
        let release = Arc::new(std::sync::Barrier::new(2));
        let (state, mut anchors) = preview_listener_with_claim_barrier(
            Duration::from_millis(250),
            Arc::clone(&claimed),
            Arc::clone(&release),
        );
        let running = tokio::spawn(async move {
            handle_request(
                &state,
                preview_listener_request(preview_request(
                    50,
                    "tools/call",
                    json!({"name": "__test_deadline_mutation", "arguments": {}}),
                )),
            )
            .await
        });
        let anchor = anchors.recv().await.expect("claiming anchor");
        tokio::task::spawn_blocking(move || claimed.wait())
            .await
            .expect("claim barrier");
        tokio::time::sleep_until(anchor.deadline()).await;
        let response = tokio::time::timeout(Duration::from_secs(1), running).await;
        tokio::task::spawn_blocking(move || release.wait())
            .await
            .expect("release barrier");
        let response = response
            .expect("listener terminal response")
            .expect("request join");
        assert_eq!(response.status(), StatusCode::REQUEST_TIMEOUT);
        assert!(!anchor.dispatched());
        tokio::task::yield_now().await;
        assert!(!anchor.dispatched());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn preview_http_dispatch_wins_before_deadline_and_returns_structured_outcome() {
        let claimed = Arc::new(std::sync::Barrier::new(2));
        let release = Arc::new(std::sync::Barrier::new(2));
        let (state, mut anchors) = preview_listener_with_claim_barrier(
            Duration::from_millis(250),
            Arc::clone(&claimed),
            Arc::clone(&release),
        );
        let running = tokio::spawn(async move {
            handle_request(
                &state,
                preview_listener_request(preview_request(
                    51,
                    "tools/call",
                    json!({"name": "__test_deadline_mutation", "arguments": {}}),
                )),
            )
            .await
        });
        let anchor = anchors.recv().await.expect("claiming anchor");
        tokio::task::spawn_blocking(move || claimed.wait())
            .await
            .expect("claim barrier");
        tokio::task::spawn_blocking(move || release.wait())
            .await
            .expect("release barrier");
        for _ in 0..32 {
            if anchor.dispatched() {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert!(anchor.dispatched());
        let response = tokio::time::timeout(Duration::from_secs(1), running)
            .await
            .expect("structured terminal response")
            .expect("request join");
        assert_eq!(response.status(), StatusCode::OK);
        let body = json_body(response).await;
        assert_eq!(body["id"], 51);
        assert_eq!(body["result"]["isError"], true);
        assert!(
            body.to_string().contains("mutation may have applied"),
            "{body}"
        );
    }

    async fn json_body(response: Response<HttpBody>) -> Value {
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("collect body")
            .to_bytes();
        serde_json::from_slice(&bytes).expect("json body")
    }

    #[tokio::test]
    async fn discover_and_catalog_flow_matches_the_stdio_contract() {
        let backend = backend();
        let alice = principal("alice");

        let response = backend
            .clone()
            .call(admitted(
                Method::POST,
                &[],
                &preview_request(1, "server/discover", json!({})),
                &alice,
            ))
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(header::CONTENT_TYPE).unwrap(),
            "application/json"
        );
        let body = json_body(response).await;
        assert_eq!(body["result"]["resultType"], "complete");
        assert_eq!(body["result"]["supportedVersions"], json!(["2026-07-28"]));

        let response = backend
            .clone()
            .call(admitted(
                Method::POST,
                &[],
                &preview_request(2, "tools/list", json!({})),
                &alice,
            ))
            .await;
        let body = json_body(response).await;
        let tools = body["result"]["tools"].as_array().expect("tools array");
        assert!(
            tools.iter().any(|tool| tool["name"] == "object_search"),
            "{body}"
        );
    }

    #[tokio::test]
    async fn version_fallback_and_grammar_errors_keep_wire_contracts() {
        let backend = backend();
        let alice = principal("alice");

        let mut request: Value = serde_json::from_str(&preview_request(3, "tools/list", json!({})))
            .expect("request json");
        request["params"]["_meta"]["io.modelcontextprotocol/protocolVersion"] = json!("2025-11-25");
        let response = backend
            .clone()
            .call(admitted(Method::POST, &[], &request.to_string(), &alice))
            .await;
        let body = json_body(response).await;
        assert_eq!(body["error"]["code"], -32022);
        assert_eq!(body["error"]["data"]["supported"], json!(["2026-07-28"]));

        let response = backend
            .clone()
            .call(admitted(Method::POST, &[], "not json", &alice))
            .await;
        let body = json_body(response).await;
        assert_eq!(body["error"]["code"], -32700);

        let response = backend
            .clone()
            .call(admitted(
                Method::POST,
                &[],
                &preview_request(4, "nonexistent/method", json!({})),
                &alice,
            ))
            .await;
        let body = json_body(response).await;
        assert_eq!(body["error"]["code"], -32601);
    }

    #[tokio::test]
    async fn get_and_delete_are_stateless_405s() {
        let backend = backend();
        let alice = principal("alice");
        for method in [Method::GET, Method::DELETE] {
            let response = backend
                .clone()
                .call(admitted(method.clone(), &[], "", &alice))
                .await;
            assert_eq!(
                response.status(),
                StatusCode::METHOD_NOT_ALLOWED,
                "{method}"
            );
            assert_eq!(
                response.headers().get(header::ALLOW).unwrap(),
                PREVIEW_ALLOW
            );
        }
    }

    #[tokio::test]
    async fn content_negotiation_is_enforced() {
        let backend = backend();
        let alice = principal("alice");

        let mut wrong_type = admitted(
            Method::POST,
            &[],
            &preview_request(5, "server/discover", json!({})),
            &alice,
        );
        wrong_type
            .parts
            .headers
            .insert(header::CONTENT_TYPE, HeaderValue::from_static("text/plain"));
        let response = backend.clone().call(wrong_type).await;
        assert_eq!(response.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);

        let mut wrong_accept = admitted(
            Method::POST,
            &[],
            &preview_request(6, "server/discover", json!({})),
            &alice,
        );
        wrong_accept
            .parts
            .headers
            .insert(header::ACCEPT, HeaderValue::from_static("text/html"));
        let response = backend.clone().call(wrong_accept).await;
        assert_eq!(response.status(), StatusCode::NOT_ACCEPTABLE);
    }

    #[tokio::test]
    async fn cancellation_notifications_are_principal_scoped_202s() {
        let backend = backend();
        let alice = principal("alice");
        let cancel = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "notifications/cancelled",
            "params": {"requestId": 42},
        })
        .to_string();
        let response = backend
            .clone()
            .call(admitted(Method::POST, &[], &cancel, &alice))
            .await;
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        let body = response
            .into_body()
            .collect()
            .await
            .expect("empty body")
            .to_bytes();
        assert!(body.is_empty());
    }
}
