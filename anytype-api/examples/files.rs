// Demonstrates combined REST and gRPC file operations.
//
// Before running this example, update the two file paths to absolute path to local files,
//
// and set file_type to FileType::(File, Image, Video, Audio, Pdf, or Other)
//

use anyhow::{Context, Result};

mod example_lib;
use anytype::prelude::*;
use std::path::PathBuf;

#[tokio::main]
async fn main() -> Result<()> {
    let client = AnytypeClient::with_config(ClientConfig {
        app_name: "anytype-examples".into(),
        keystore_service: Some("anyr".to_string()), // reuse "anyr"'s auth tokens
        ..Default::default()
    })?;
    let space_id = anytype::test_util::example_space_id(&client).await?;

    let temp_dir = std::env::temp_dir().join("anytype_files_example");
    std::fs::create_dir_all(&temp_dir).context(format!("create temp dir {temp_dir:?}"))?;

    // upload local files
    let mut files = Vec::new();

    // TODO: update path and file_type
    let path = "/home/user/project/anytype/README.md";
    if PathBuf::from(path).is_file() {
        let file = client
            .files()
            .upload(&space_id)
            .from_path(path)
            .upload()
            .await?;
        println!(
            "Uploaded file {} id:{}",
            file.name.as_deref().unwrap_or("unnamed"),
            file.id
        );
        files.push(file);
    } else {
        println!("Missing file: {path}");
    }

    // TODO: update path and file_type
    let path = "./picture.png";
    if PathBuf::from(path).is_file() {
        let file = client
            .files()
            .upload(&space_id)
            .from_path(path)
            .file_type(FileType::Image)
            .upload()
            .await?;
        println!(
            "Uploaded file {} id:{}",
            file.name.as_deref().unwrap_or("unnamed"),
            file.id
        );
        files.push(file);
    } else {
        println!("Missing file: {path}");
    }

    let files = client.files().list(&space_id).limit(20).list().await?;
    println!("First {} file(s) in space:", files.items.len());
    for item in files.iter() {
        println!(
            "- {} ({})",
            item.name.as_deref().unwrap_or("unnamed"),
            item.id
        );
    }

    let download_dir = temp_dir.join("downloads");
    std::fs::create_dir_all(&download_dir).context(format!("create downloads {download_dir:?}"))?;

    for (i, file) in files.iter().enumerate() {
        let bytes = client.files().download_bytes(&space_id, &file.id).await?;
        let download = download_dir.join(&file.id);
        tokio::fs::write(&download, bytes).await?;
        eprintln!(
            "download {i} {} to {download:?}",
            file.name.as_deref().unwrap_or("unnamed")
        );
    }

    // Cleanup: delete file objects and remove temp folder.
    eprintln!("temp folder: {temp_dir:?}");
    //for file in files.iter() {
    //client.object(&space_id, &file.id).delete().await?;
    //}
    // let _ = std::fs::remove_dir_all(&temp_dir);

    Ok(())
}
