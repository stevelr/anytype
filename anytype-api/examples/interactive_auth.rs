// Runs the interactive auth flow and stores the API key in a keystore.
// Requires the desktop client - interactive auth with headless client is done from the cli

use std::io::{self, Write};

use anytype::prelude::*;

#[tokio::main]
async fn main() -> Result<(), AnytypeError> {
    let client = AnytypeClient::with_config(ClientConfig {
        app_name: "anytype-examples".into(),
        keystore_service: Some("anyr".to_string()), // reuse "anyr"'s auth tokens
        ..Default::default()
    })?;

    client
        .authenticate_interactive(
            |challenge_id| {
                println!("Challenge ID: {challenge_id}");
                print!("Enter the 4-digit code from Anytype: ");
                io::stdout().flush().map_err(|e| AnytypeError::Auth {
                    message: e.to_string(),
                })?;
                let mut code = String::new();
                io::stdin()
                    .read_line(&mut code)
                    .map_err(|e| AnytypeError::Auth {
                        message: e.to_string(),
                    })?;
                Ok(code.trim().to_string())
            },
            false,
        )
        .await?;

    println!("Authenticated and stored API key.");
    Ok(())
}
