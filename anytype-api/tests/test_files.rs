use anytype::test_util::{TestResult, unique_suffix, with_test_context};

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

        ctx.client.files().delete(&ctx.space_id, &file.id).await?;
        Ok(())
    })
    .await
}
