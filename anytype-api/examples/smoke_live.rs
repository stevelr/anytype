//! Live smoke test against a local anytype JSON API.
//!
//! Verifies (1) `SpaceModel` deserialization of the real `object` discriminator
//! and (2) the new REST file transfer methods (upload / download / delete).
//!
//! Usage: API_KEY=... URL=http://127.0.0.1:31009 cargo run -p anytype --example smoke_live

use anytype::keystore::HttpCredentials;
use anytype::prelude::*;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let key = std::env::var("API_KEY").expect("set API_KEY");
    let url = std::env::var("URL").unwrap_or_else(|_| "http://127.0.0.1:31009".into());
    let ks = std::env::var("KS").unwrap_or_else(|_| "file:path=/tmp/anytype-smoke-ks".into());

    let mut config = ClientConfig::default().app_name("smoke");
    config.base_url = Some(url);
    config.keystore = Some(ks);
    let client = AnytypeClient::with_config(config)?;
    client.set_api_key(HttpCredentials::new(key));

    // (1) spaces list — exercises SpaceModel deserialization of "anytype.*" objects.
    let spaces = client.spaces().list().await?;
    println!("spaces: {}", spaces.items.len());
    let space = spaces.items.first().expect("need at least one space");
    println!(
        "  first space id={} object={:?} is_chat={}",
        space.id,
        space.object,
        space.is_chat()
    );
    let space_id = space.id.clone();

    // (2) REST file transfer round-trip.
    let payload = b"hello from smoke_live at 2026-07-17".to_vec();
    let uploaded = client
        .files()
        .http_upload(&space_id)
        .file_name("smoke.txt")
        .mime("text/plain")
        .bytes(payload.clone())
        .upload()
        .await?;
    println!(
        "uploaded: object_id={} name={:?} media={:?} size={:?}",
        uploaded.object_id, uploaded.name, uploaded.media, uploaded.size_in_bytes
    );

    let downloaded = client
        .files()
        .http_download(&space_id, &uploaded.object_id)
        .await?;
    println!("downloaded {} bytes", downloaded.len());
    assert_eq!(
        downloaded.as_ref(),
        payload.as_slice(),
        "round-trip bytes must match"
    );
    println!("round-trip OK");

    client
        .files()
        .http_delete(&space_id, &uploaded.object_id)
        .await?;
    println!("deleted OK");

    println!("SMOKE PASS");
    Ok(())
}
