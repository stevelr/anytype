// Runs the interactive auth flow and stores the API key in a keystore.
// Requires the desktop client - interactive auth with headless client is done from the cli

use std::io::{self, Write};

use anytype::prelude::*;

#[tokio::main]
async fn main() -> Result<(), AnytypeError> {
    let app_name = env!("CARGO_BIN_NAME");
    let client = AnytypeClient::new(app_name)?.set_key_store(KeyStoreFile::new(app_name)?);

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
