// Demonstrates gRPC-backed file operations.

use anyhow::{Context, Result};

mod example_lib;
use example_lib::{create_file_image::create_png, create_file_pdf::create_pdf};

use anytype::prelude::*;

const NUM_FILES: usize = 2;

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

    // create image files and upload
    let mut files = Vec::new();
    for color_num in 0..NUM_FILES {
        let path = create_png(10, color_num, &temp_dir)?;
        let file = client
            .files()
            .upload(&space_id)
            .from_path(&path)
            .file_type(FileType::Image)
            .upload()
            .await?;
        println!(
            "Uploaded file {} id:{}",
            file.name.as_deref().unwrap_or("unnamed"),
            file.id
        );
        files.push(file);
    }

    // create pdf file and upload
    let path = create_pdf(&temp_dir)?;
    let file = client
        .files()
        .upload(&space_id)
        .from_path(&path)
        .file_type(FileType::Pdf)
        .upload()
        .await?;
    println!(
        "Uploaded file {} id:{}",
        file.name.as_deref().unwrap_or("unnamed"),
        file.id
    );
    files.push(file);

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
        let download = client
            .files()
            .download(&file.id)
            .to_path(&download_dir)
            .download()
            .await?;
        eprintln!(
            "download {i} {} to {download:?}",
            &file.name.as_deref().unwrap_or("unnamed")
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
