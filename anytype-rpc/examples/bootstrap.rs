//! Throwaway bootstrap: create a local-only account on a running grpc-server
//! and enable the JSON API. Prints the mnemonic, account id, and (if the
//! local-link flow succeeds) a JSON-API app key.
//!
//! Usage: ANYTYPE_GRPC=http://127.0.0.1:31007 ROOT=/path/to/data cargo run -p anytype-rpc --example bootstrap

use anytype_rpc::anytype::rpc::{account, initial, wallet};
use anytype_rpc::anytype::{ClientCommandsClient, Event, StreamRequest, event::message::Value};
use anytype_rpc::auth::with_token;
use anytype_rpc::model::account::auth::LocalApiScope;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let endpoint =
        std::env::var("ANYTYPE_GRPC").unwrap_or_else(|_| "http://127.0.0.1:31007".into());
    let root = std::env::var("ROOT").unwrap_or_else(|_| "/tmp/anytype-data".into());
    let json_api = std::env::var("JSON_API").unwrap_or_else(|_| "127.0.0.1:31009".into());

    let channel = tonic::transport::Endpoint::from_shared(endpoint.clone())?
        .connect()
        .await?;
    let mut client = ClientCommandsClient::new(channel);

    eprintln!("InitialSetParameters");
    client
        .initial_set_parameters(initial::set_parameters::Request {
            platform: "test".into(),
            version: "0.50.15".into(),
            workdir: root.clone(),
            log_level: String::new(),
            do_not_send_logs: true,
            do_not_save_logs: true,
            do_not_send_telemetry: true,
        })
        .await?;

    eprintln!("WalletCreate root={root}");
    let wc = client
        .wallet_create(wallet::create::Request {
            root_path: root.clone(),
            fulltext_primary_language: String::new(),
        })
        .await?
        .into_inner();
    if let Some(err) = &wc.error {
        if err.code != 0 {
            return Err(format!("WalletCreate error {}: {}", err.code, err.description).into());
        }
    }
    println!("MNEMONIC={}", wc.mnemonic);

    eprintln!("AccountCreate (LocalOnly, json_api={json_api})");
    let ac = client
        .account_create(account::create::Request {
            name: "test".into(),
            store_path: root.clone(),
            icon: 0,
            disable_local_network_sync: true,
            network_mode: account::NetworkMode::LocalOnly as i32,
            json_api_listen_addr: json_api.clone(),
            ..Default::default()
        })
        .await?
        .into_inner();
    if let Some(err) = &ac.error {
        if err.code != 0 {
            return Err(format!("AccountCreate error {}: {}", err.code, err.description).into());
        }
    }
    let account_id = ac
        .account
        .as_ref()
        .map(|a| a.id.clone())
        .unwrap_or_default();
    println!("ACCOUNT_ID={account_id}");
    println!("JSON_API=http://{json_api}");

    // --- issue a JSON-API app key via the local-link challenge flow ---
    eprintln!("WalletCreateSession");
    let session = client
        .wallet_create_session(wallet::create_session::Request {
            auth: Some(wallet::create_session::request::Auth::Mnemonic(
                wc.mnemonic.clone(),
            )),
        })
        .await?
        .into_inner();
    let token = session.token;

    // Listen for the LinkChallenge event that carries the one-time code.
    let (tx, rx) = tokio::sync::oneshot::channel::<String>();
    let mut event_client = client.clone();
    let event_token = token.clone();
    tokio::spawn(async move {
        // StreamRequest carries the token in its body.
        let req = tonic::Request::new(StreamRequest { token: event_token });
        if let Ok(stream) = event_client.listen_session_events(req).await {
            let mut stream = stream.into_inner();
            let mut tx = Some(tx);
            while let Ok(Some(event)) = stream.message().await {
                let event: Event = event;
                for msg in event.messages {
                    if let Some(Value::AccountLinkChallenge(ch)) = msg.value {
                        if let Some(tx) = tx.take() {
                            let _ = tx.send(ch.challenge);
                        }
                        return;
                    }
                }
            }
        }
    });

    eprintln!("AccountLocalLinkNewChallenge (JsonApi scope)");
    let nc = client
        .account_local_link_new_challenge(with_token(
            tonic::Request::new(account::local_link::new_challenge::Request {
                app_name: "anyr-bootstrap".into(),
                scope: LocalApiScope::JsonApi as i32,
            }),
            &token,
        )?)
        .await?
        .into_inner();
    let challenge_id = nc.challenge_id;

    let code = tokio::time::timeout(std::time::Duration::from_secs(15), rx)
        .await
        .map_err(|_| "timed out waiting for LinkChallenge event")??;
    eprintln!("got challenge code");

    let solved = client
        .account_local_link_solve_challenge(with_token(
            tonic::Request::new(account::local_link::solve_challenge::Request {
                challenge_id,
                answer: code,
            }),
            &token,
        )?)
        .await?
        .into_inner();
    if let Some(err) = &solved.error {
        if err.code != 0 {
            return Err(format!("SolveChallenge error {}: {}", err.code, err.description).into());
        }
    }
    println!("APP_KEY={}", solved.app_key);
    Ok(())
}
