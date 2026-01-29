use std::net::SocketAddr;
use std::str::FromStr;

use anytype::mock::MockChatServer;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let addr = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "127.0.0.1:31010".to_string());
    let addr = SocketAddr::from_str(&addr)?;
    let handle = MockChatServer::start(addr).await?;
    println!(
        "mock chat server listening on {addr} (tokens: token-alice, token-bob, token-carol, token-dash, token-ernie)"
    );
    tokio::signal::ctrl_c().await?;
    handle.shutdown().await;
    Ok(())
}
