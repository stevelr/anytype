//! Integration tests for pagination helpers and streaming.

mod common;

use std::collections::HashSet;

use anytype::test_util::{TestResult, with_test_context};
use futures::StreamExt;

#[tokio::test]
#[test_log::test]
async fn test_collect_all_matches_total() -> TestResult<()> {
    with_test_context(|ctx| async move {
        let limit = 2;
        let first_page = ctx.client.types(&ctx.space_id).limit(limit).list().await?;
        let total = first_page.pagination.total;

        if total <= limit as usize {
            println!("Skipping collect_all test: total={} limit={}", total, limit);
            return Ok(());
        }

        let all_types = ctx
            .client
            .types(&ctx.space_id)
            .limit(limit)
            .list()
            .await?
            .collect_all()
            .await?;

        if total > 0 {
            assert_eq!(all_types.len(), total, "collect_all should fetch all pages");
        }

        Ok(())
    })
    .await
}

#[tokio::test]
#[test_log::test]
async fn test_stream_matches_collect_all() -> TestResult<()> {
    with_test_context(|ctx| async move {
        let limit = 3;
        let collected = ctx
            .client
            .properties(&ctx.space_id)
            .limit(limit)
            .list()
            .await?
            .collect_all()
            .await?;

        let mut stream = ctx
            .client
            .properties(&ctx.space_id)
            .limit(limit)
            .list()
            .await?
            .into_stream();

        let mut streamed = Vec::new();
        while let Some(item) = stream.next().await {
            streamed.push(item?);
        }

        let collected_ids: HashSet<String> = collected.into_iter().map(|p| p.id).collect();
        let streamed_ids: HashSet<String> = streamed.into_iter().map(|p| p.id).collect();

        assert_eq!(
            collected_ids, streamed_ids,
            "stream should match collect_all"
        );
        Ok(())
    })
    .await
}
