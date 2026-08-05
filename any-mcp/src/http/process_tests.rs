// any-mcp - bounded, workflow-oriented MCP server for Anytype
//
// SPDX-FileCopyrightText: 2026 Steve Schoettler
// SPDX-License-Identifier: Apache-2.0

//! Process-level Streamable HTTP conformance over real loopback sockets.
//!
//! These tests assemble the production listener, static-token
//! authenticator, and stable session backend on an OS-assigned loopback
//! port and drive them with a real HTTP client: authentication, the MCP
//! initialize lifecycle, SSE responses, session termination, CORS
//! preflight, and exact catalog parity with the stdio implementation.

use std::{sync::Arc, time::Duration};

use anytype::prelude::{AnytypeClient, ClientConfig};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::{
    config::ApplicationProfile,
    http::{
        auth::Authenticator,
        listener::{ListenerState, run_listener, tests::test_config},
        secret::StaticToken,
        session::StableBackend,
    },
    runtime::{RuntimeContext, StartupStatus},
    server::AnyMcpServer,
};

const TEST_TOKEN: &str = "process-test-token-0123456789abcdefghijklmnop";

struct TempTokenFile {
    path: std::path::PathBuf,
}

impl TempTokenFile {
    fn create() -> Self {
        let path = std::env::temp_dir().join(format!(
            "any-mcp-http-process-token-{}-{:x}",
            std::process::id(),
            std::ptr::from_ref(&TEST_TOKEN) as usize
        ));
        std::fs::write(&path, TEST_TOKEN).expect("write process test token");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
                .expect("restrict process test token");
        }
        Self { path }
    }
}

impl Drop for TempTokenFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn test_runtime() -> RuntimeContext {
    let config = ClientConfig {
        base_url: Some("http://127.0.0.1:1".to_string()),
        keystore: Some("env".to_string()),
        keystore_service: Some("any-mcp-http-process-test".to_string()),
        app_name: "any-mcp-http-process-test".to_string(),
        ..ClientConfig::default()
    };
    let client = AnytypeClient::with_config(config).expect("test client");
    RuntimeContext::from_parts_with_profile(
        client,
        4,
        Duration::from_secs(5),
        StartupStatus {
            http_available: true,
            grpc_available: false,
        },
        ApplicationProfile::Compact,
        false,
    )
}

struct RunningServer {
    base: String,
    shutdown: CancellationToken,
    task: tokio::task::JoinHandle<Result<(), super::listener::HttpServeError>>,
    runtime: RuntimeContext,
    _token_file: TempTokenFile,
}

impl RunningServer {
    async fn start() -> Self {
        let token_file = TempTokenFile::create();
        let config = test_config(&[("ANY_MCP_HTTP_ALLOWED_ORIGINS", "https://app.example.com")]);
        let runtime = test_runtime();
        let token = StaticToken::load(&token_file.path).expect("load process test token");
        let backend = Arc::new(StableBackend::new(
            runtime.clone(),
            &config,
            CancellationToken::new(),
        ));
        let service: super::listener::McpService =
            Arc::new(move |admitted| Box::pin(backend.clone().call(admitted)));
        let state = Arc::new(ListenerState::new(
            &config,
            Authenticator::StaticToken(token),
            None,
            service,
        ));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind process test listener");
        let address = listener.local_addr().expect("listener address");
        let shutdown = CancellationToken::new();
        let task = tokio::spawn(run_listener(
            listener,
            state,
            shutdown.clone(),
            Duration::from_secs(2),
        ));
        Self {
            base: format!("http://127.0.0.1:{}", address.port()),
            shutdown,
            task,
            runtime,
            _token_file: token_file,
        }
    }

    async fn stop(self) {
        self.shutdown.cancel();
        let result = tokio::time::timeout(Duration::from_secs(5), self.task)
            .await
            .expect("listener shutdown deadline")
            .expect("listener join");
        assert_eq!(result, Ok(()));
    }
}

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .build()
        .expect("process test client")
}

/// Extracts the last `data:` event payload from one SSE body.
fn last_sse_data(body: &str) -> Value {
    let data = body
        .lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .next_back()
        .expect("SSE data event");
    serde_json::from_str(data).expect("SSE data JSON")
}

async fn initialize(client: &reqwest::Client, base: &str) -> String {
    let response = client
        .post(format!("{base}/mcp"))
        .bearer_auth(TEST_TOKEN)
        .header("accept", "application/json, text/event-stream")
        .header("content-type", "application/json")
        .json(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": {"name": "process-test", "version": "1.0.0"},
            },
        }))
        .send()
        .await
        .expect("initialize request");
    assert_eq!(response.status(), 200);
    let session = response
        .headers()
        .get("mcp-session-id")
        .expect("session id header")
        .to_str()
        .expect("ascii session id")
        .to_owned();
    let body = response.text().await.expect("initialize body");
    let message = last_sse_data(&body);
    assert_eq!(
        message["result"]["protocolVersion"], "2025-11-25",
        "{message}"
    );

    let response = client
        .post(format!("{base}/mcp"))
        .bearer_auth(TEST_TOKEN)
        .header("accept", "application/json, text/event-stream")
        .header("content-type", "application/json")
        .header("mcp-session-id", &session)
        .header("mcp-protocol-version", "2025-11-25")
        .json(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))
        .send()
        .await
        .expect("initialized notification");
    assert_eq!(response.status(), 202);
    session
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn streamable_http_process_conformance() {
    let server = RunningServer::start().await;
    let client = client();
    let base = server.base.clone();

    // Authentication is required on every MCP route and challenged with the
    // fixed header; the static profile publishes no metadata routes.
    let response = client
        .post(format!("{base}/mcp"))
        .header("accept", "application/json, text/event-stream")
        .header("content-type", "application/json")
        .body("{}")
        .send()
        .await
        .expect("unauthenticated request");
    assert_eq!(response.status(), 401);
    assert_eq!(
        response
            .headers()
            .get("www-authenticate")
            .expect("challenge")
            .to_str()
            .unwrap(),
        "Bearer"
    );
    let response = client
        .get(format!("{base}/.well-known/oauth-protected-resource"))
        .send()
        .await
        .expect("metadata request");
    assert_eq!(response.status(), 404);

    // CORS preflight is unauthenticated and exact.
    let response = client
        .request(reqwest::Method::OPTIONS, format!("{base}/mcp"))
        .header("origin", "https://app.example.com")
        .header("access-control-request-method", "POST")
        .send()
        .await
        .expect("preflight");
    assert_eq!(response.status(), 204);
    assert_eq!(
        response
            .headers()
            .get("access-control-allow-origin")
            .expect("allow origin")
            .to_str()
            .unwrap(),
        "https://app.example.com"
    );
    let response = client
        .request(reqwest::Method::OPTIONS, format!("{base}/mcp"))
        .header("origin", "https://evil.example.com")
        .header("access-control-request-method", "POST")
        .send()
        .await
        .expect("rejected preflight");
    assert_eq!(response.status(), 403);

    // Initialize, then verify the advertised catalog is byte-identical to
    // the stdio catalog for the same profile.
    let session = initialize(&client, &base).await;
    let response = client
        .post(format!("{base}/mcp"))
        .bearer_auth(TEST_TOKEN)
        .header("accept", "application/json, text/event-stream")
        .header("content-type", "application/json")
        .header("mcp-session-id", &session)
        .header("mcp-protocol-version", "2025-11-25")
        .json(&json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}))
        .send()
        .await
        .expect("tools/list request");
    assert_eq!(response.status(), 200);
    let body = response.text().await.expect("tools/list body");
    let message = last_sse_data(&body);
    let http_tools = message["result"]["tools"].clone();
    let stdio_catalog = AnyMcpServer::new(server.runtime.clone()).expect("stdio catalog");
    let stdio_tools = serde_json::to_value(stdio_catalog.tools()).expect("stdio tools");
    assert_eq!(http_tools, stdio_tools);

    // A standalone GET opens a live SSE stream for the session.
    let response = client
        .get(format!("{base}/mcp"))
        .bearer_auth(TEST_TOKEN)
        .header("accept", "text/event-stream")
        .header("mcp-session-id", &session)
        .header("mcp-protocol-version", "2025-11-25")
        .send()
        .await
        .expect("standalone SSE stream");
    assert_eq!(response.status(), 200);
    assert!(
        response
            .headers()
            .get("content-type")
            .expect("SSE content type")
            .to_str()
            .unwrap()
            .starts_with("text/event-stream")
    );
    drop(response);

    // DELETE terminates the session; it is unknown afterwards.
    let response = client
        .delete(format!("{base}/mcp"))
        .bearer_auth(TEST_TOKEN)
        .header("accept", "application/json, text/event-stream")
        .header("mcp-session-id", &session)
        .header("mcp-protocol-version", "2025-11-25")
        .send()
        .await
        .expect("session delete");
    assert!(response.status().is_success(), "{}", response.status());
    let response = client
        .post(format!("{base}/mcp"))
        .bearer_auth(TEST_TOKEN)
        .header("accept", "application/json, text/event-stream")
        .header("content-type", "application/json")
        .header("mcp-session-id", &session)
        .header("mcp-protocol-version", "2025-11-25")
        .json(&json!({"jsonrpc": "2.0", "id": 3, "method": "tools/list"}))
        .send()
        .await
        .expect("post-delete request");
    assert_eq!(response.status(), 404);

    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wrong_bearer_and_untested_versions_fail_over_the_socket() {
    let server = RunningServer::start().await;
    let client = client();
    let base = server.base.clone();

    let response = client
        .post(format!("{base}/mcp"))
        .bearer_auth("wrong-token-wrong-token-wrong-token-wrong-1")
        .header("accept", "application/json, text/event-stream")
        .header("content-type", "application/json")
        .body("{}")
        .send()
        .await
        .expect("wrong bearer");
    assert_eq!(response.status(), 401);

    // The launch revision is a valid proposal: real Streamable HTTP clients
    // (e.g. zeroclaw) still open with 2024-11-05, and rmcp echoes any known
    // proposed version, so the handshake succeeds and a session is issued.
    let response = client
        .post(format!("{base}/mcp"))
        .bearer_auth(TEST_TOKEN)
        .header("accept", "application/json, text/event-stream")
        .header("content-type", "application/json")
        .json(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {"name": "process-test", "version": "1.0.0"},
            },
        }))
        .send()
        .await
        .expect("launch revision over HTTP");
    assert!(response.status().is_success(), "{}", response.status());
    assert!(
        response.headers().get("mcp-session-id").is_some(),
        "initialize with the launch revision must issue a session"
    );

    // An untested revision proposed in an initialize body still fails closed.
    let response = client
        .post(format!("{base}/mcp"))
        .bearer_auth(TEST_TOKEN)
        .header("accept", "application/json, text/event-stream")
        .header("content-type", "application/json")
        .json(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "1999-01-01",
                "capabilities": {},
                "clientInfo": {"name": "process-test", "version": "1.0.0"},
            },
        }))
        .send()
        .await
        .expect("unknown revision over HTTP");
    assert_eq!(response.status(), 400);

    server.stop().await;
}
