//! Integration tests for Views (collections and queries)
//!
//! Validates listing views and objects within a view.

mod common;

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use anytype::{
    error::AnytypeError,
    prelude::*,
    test_util::{
        DisposableRun, TestError, TestResult, unique_suffix, with_disposable_space_context,
        with_test_context,
    },
};
use common::retry_definitive_rate_limit;
use serial_test::serial;
use tokio::time::{Duration, sleep};

fn find_list_object_by_layout(objects: &[Object], layout: ObjectLayout) -> Vec<&Object> {
    objects.iter().filter(|obj| obj.layout == layout).collect()
}

async fn ensure_list_object(
    ctx: &anytype::test_util::TestContext,
    layout: ObjectLayout,
) -> TestResult<Object> {
    let objects = ctx.client.objects(&ctx.space_id).limit(200).list().await?;
    let candidates = find_list_object_by_layout(&objects.items, layout.clone());
    for obj in candidates {
        let views = match list_views_with_retry(ctx, &obj.id).await {
            Ok(views) => views,
            Err(_) => continue,
        };
        if let Some(view) = views.items.first()
            && view_list_objects_with_retry(ctx, &obj.id, Some(&view.id), 1)
                .await
                .is_ok()
        {
            let fetched = ctx.client.object(&ctx.space_id, &obj.id).get().await?;
            return Ok(fetched);
        }
    }

    let types_result = ctx.client.types(&ctx.space_id).list().await?;
    let types = types_result.items.clone();
    let fallback_key = match layout {
        ObjectLayout::Collection => "collection",
        ObjectLayout::Set => "set",
        _ => "",
    };
    let typ = types
        .iter()
        .find(|t| t.layout == layout)
        .or_else(|| types.iter().find(|t| t.key == fallback_key))
        .ok_or_else(|| TestError::Assertion {
            message: format!(
                "no type found for layout {layout}; expected type with layout or key '{fallback_key}'"
            ),
        })?;

    let object_name = format!("Test {layout} {}", unique_suffix());
    let obj = retry_definitive_rate_limit("view list setup object", || async {
        ctx.client
            .new_object(&ctx.space_id, &typ.key)
            .name(&object_name)
            .create()
            .await
    })
    .await?;
    ctx.register_object(&obj.id);
    let views = list_views_with_retry(ctx, &obj.id).await?;
    let view = views.items.first().ok_or_else(|| TestError::Assertion {
        message: format!("expected views for list {}, got none", obj.id),
    })?;
    view_list_objects_with_retry(ctx, &obj.id, Some(&view.id), 1).await?;
    Ok(obj)
}

async fn list_views_with_retry(
    ctx: &anytype::test_util::TestContext,
    list_id: &str,
) -> TestResult<PagedResult<View>> {
    let mut last_err = None;
    for attempt in 0..3 {
        match ctx.client.list_views(&ctx.space_id, list_id).list().await {
            Ok(result) => return Ok(result),
            Err(err) => {
                if matches!(err, AnytypeError::NotFound { .. }) {
                    last_err = Some(err);
                    sleep(Duration::from_millis(500 * (attempt + 1) as u64)).await;
                    continue;
                }
                return Err(err.into());
            }
        }
    }
    Err(TestError::Assertion {
        message: format!(
            "list_views not found after retries for list {}: {:?}",
            list_id, last_err
        ),
    })
}

async fn view_list_objects_with_retry(
    ctx: &anytype::test_util::TestContext,
    list_id: &str,
    view_id: Option<&str>,
    limit: u32,
) -> TestResult<PagedResult<Object>> {
    let mut last_err = None;
    for attempt in 0..3 {
        let mut request = ctx
            .client
            .view_list_objects(&ctx.space_id, list_id)
            .limit(limit);
        if let Some(view_id) = view_id {
            request = request.view(view_id);
        }
        match request.list().await {
            Ok(result) => return Ok(result),
            Err(err) => {
                if matches!(err, AnytypeError::NotFound { .. }) {
                    last_err = Some(err);
                    sleep(Duration::from_millis(500 * (attempt + 1) as u64)).await;
                    continue;
                }
                return Err(err.into());
            }
        }
    }
    Err(TestError::Assertion {
        message: format!(
            "view_list_objects not found after retries for list {}: {:?}",
            list_id, last_err
        ),
    })
}

#[tokio::test]
#[test_log::test]
#[serial]
async fn test_views_list() -> TestResult<()> {
    with_test_context(|ctx| async move {
        let list_obj = ensure_list_object(ctx.as_ref(), ObjectLayout::Collection).await?;

        let views = list_views_with_retry(ctx.as_ref(), &list_obj.id).await?;

        assert!(
            !views.items.is_empty(),
            "expected views for list {}, got none",
            list_obj.id
        );

        for view in views.iter() {
            assert!(!view.id.is_empty(), "View id should not be empty");
        }

        Ok(())
    })
    .await
}

#[tokio::test]
#[test_log::test]
#[serial]
async fn test_view_list_objects() -> TestResult<()> {
    with_test_context(|ctx| async move {
        let list_obj = ensure_list_object(ctx.as_ref(), ObjectLayout::Collection).await?;

        let views = list_views_with_retry(ctx.as_ref(), &list_obj.id).await?;

        let view = views.items.first().ok_or_else(|| TestError::Assertion {
            message: format!("expected views for list {}", list_obj.id),
        })?;
        let objects_for_view =
            view_list_objects_with_retry(ctx.as_ref(), &list_obj.id, Some(&view.id), 10).await?;
        println!(
            "View {} returned {} objects",
            view.id,
            objects_for_view.items.len()
        );

        Ok(())
    })
    .await
}

#[tokio::test]
#[test_log::test]
#[ignore = "requires a configured Set with an internal set_of source"]
#[serial]
async fn test_views_list_collection_and_set() -> TestResult<()> {
    with_test_context(|ctx| async move {
        let collection = ensure_list_object(ctx.as_ref(), ObjectLayout::Collection).await?;
        let set = ensure_list_object(ctx.as_ref(), ObjectLayout::Set).await?;

        let collection_views = list_views_with_retry(ctx.as_ref(), &collection.id).await?;
        assert!(
            !collection_views.items.is_empty(),
            "expected views for collection {}, got none",
            collection.id
        );
        for view in collection_views.iter() {
            assert!(
                !view.id.is_empty(),
                "Collection view id should not be empty"
            );
        }

        let set_views = list_views_with_retry(ctx.as_ref(), &set.id).await?;
        assert!(
            !set_views.items.is_empty(),
            "expected views for set {}, got none",
            set.id
        );
        for view in set_views.iter() {
            assert!(!view.id.is_empty(), "Set view id should not be empty");
        }

        Ok(())
    })
    .await
}

#[tokio::test]
#[test_log::test]
#[ignore = "requires a configured Set with an internal set_of source"]
#[serial]
async fn test_view_list_objects_collection_and_set() -> TestResult<()> {
    with_test_context(|ctx| async move {
        let collection = ensure_list_object(ctx.as_ref(), ObjectLayout::Collection).await?;
        let set = ensure_list_object(ctx.as_ref(), ObjectLayout::Set).await?;

        let collection_views = list_views_with_retry(ctx.as_ref(), &collection.id).await?;
        let collection_view =
            collection_views
                .items
                .first()
                .ok_or_else(|| TestError::Assertion {
                    message: format!("expected views for collection {}", collection.id),
                })?;
        let collection_listed = view_list_objects_with_retry(
            ctx.as_ref(),
            &collection.id,
            Some(&collection_view.id),
            10,
        )
        .await?;
        println!(
            "Collection {} returned {} objects",
            collection.id,
            collection_listed.items.len()
        );

        let set_views = list_views_with_retry(ctx.as_ref(), &set.id).await?;
        let set_view = set_views
            .items
            .first()
            .ok_or_else(|| TestError::Assertion {
                message: format!("expected views for set {}", set.id),
            })?;
        let set_listed =
            view_list_objects_with_retry(ctx.as_ref(), &set.id, Some(&set_view.id), 10).await?;
        println!("Set {} returned {} objects", set.id, set_listed.items.len());

        Ok(())
    })
    .await
}

#[tokio::test]
#[test_log::test]
#[serial]
async fn test_view_add_remove_objects_collection() -> TestResult<()> {
    with_test_context(|ctx| async move {
        let obj = retry_definitive_rate_limit("collection member setup object", || async {
            ctx.client
                .new_object(&ctx.space_id, "page")
                .name("Test Collection Item")
                .create()
                .await
        })
        .await?;
        ctx.register_object(&obj.id);

        let collection = ensure_list_object(ctx.as_ref(), ObjectLayout::Collection).await?;
        let add_result = ctx
            .client
            .view_add_objects(&ctx.space_id, &collection.id, vec![obj.id.clone()])
            .await
            .map_err(|err| TestError::Assertion {
                message: format!(
                    "view_add_objects failed for collection {}: {err:?}",
                    collection.id
                ),
            })?;
        assert!(
            !add_result.is_empty(),
            "view_add_objects should return a response"
        );

        let collection_views = list_views_with_retry(ctx.as_ref(), &collection.id).await?;
        let collection_view =
            collection_views
                .items
                .first()
                .ok_or_else(|| TestError::Assertion {
                    message: format!("expected views for collection {}", collection.id),
                })?;
        let listed = view_list_objects_with_retry(
            ctx.as_ref(),
            &collection.id,
            Some(&collection_view.id),
            100,
        )
        .await?;
        assert!(
            listed.items.iter().any(|item| item.id == obj.id),
            "collection view should include added object"
        );

        let remove_result = ctx
            .client
            .view_remove_object(&ctx.space_id, &collection.id, &obj.id)
            .await
            .map_err(|err| TestError::Assertion {
                message: format!(
                    "view_remove_object failed for collection {}: {err:?}",
                    collection.id
                ),
            })?;
        assert!(
            !remove_result.is_empty(),
            "view_remove_object should return a response"
        );

        let listed_after = view_list_objects_with_retry(
            ctx.as_ref(),
            &collection.id,
            Some(&collection_view.id),
            100,
        )
        .await?;
        assert!(
            !listed_after.items.iter().any(|item| item.id == obj.id),
            "collection view should not include removed object"
        );

        Ok(())
    })
    .await
}

#[tokio::test]
#[test_log::test]
#[ignore = "requires configured real server and disposable test admission"]
#[serial(disposable_anytype_api)]
async fn test_direct_collection_membership_present_absent_and_query_rejection() {
    let callback_ran = Arc::new(AtomicBool::new(false));
    let callback_flag = Arc::clone(&callback_ran);
    let outcome = Box::pin(with_disposable_space_context(
        "direct-collection-membership",
        move |ctx| {
            callback_flag.store(true, Ordering::SeqCst);
            Box::pin(async move {
                let collection_type = ctx
                    .create_collection_type_fixture(format!(
                        "Membership Collection {}",
                        unique_suffix()
                    ))
                    .await?;
                let collection = ctx
                    .create_collection_fixture(
                        &collection_type,
                        format!("Membership List {}", unique_suffix()),
                    )
                    .await?;
                let object = ctx
                    .client
                    .new_object(&ctx.space_id, "page")
                    .name(format!("Membership Object {}", unique_suffix()))
                    .create()
                    .await?;
                ctx.register_object(&object.id);

                let verify = VerifyConfig::default();
                let absent = verify_semantic(
                    &verify,
                    "direct collection membership absence",
                    &object.id,
                    || {
                        ctx.client.observe_collection_membership(
                            &ctx.space_id,
                            &collection.id,
                            &object.id,
                        )
                    },
                    |observation| observation.state == CollectionMembershipState::Absent,
                )
                .await?;
                assert_eq!(absent.space_id, ctx.space_id);
                assert_eq!(absent.collection_id, collection.id);
                assert_eq!(absent.object_id, object.id);

                ctx.client
                    .view_add_objects(&ctx.space_id, &collection.id, [&object.id])
                    .await?;
                let present = verify_semantic(
                    &verify,
                    "direct collection membership presence",
                    &object.id,
                    || {
                        ctx.client.observe_collection_membership(
                            &ctx.space_id,
                            &collection.id,
                            &object.id,
                        )
                    },
                    |observation| observation.state == CollectionMembershipState::Present,
                )
                .await?;
                assert_eq!(present.state, CollectionMembershipState::Present);

                ctx.client
                    .view_remove_object(&ctx.space_id, &collection.id, &object.id)
                    .await?;
                let removed = verify_semantic(
                    &verify,
                    "direct collection membership removal",
                    &object.id,
                    || {
                        ctx.client.observe_collection_membership(
                            &ctx.space_id,
                            &collection.id,
                            &object.id,
                        )
                    },
                    |observation| observation.state == CollectionMembershipState::Absent,
                )
                .await?;
                assert_eq!(removed.state, CollectionMembershipState::Absent);

                let types = ctx.client.types(&ctx.space_id).list().await?;
                let set_type = types
                    .items
                    .iter()
                    .find(|typ| typ.layout == ObjectLayout::Set)
                    .ok_or_else(|| TestError::Assertion {
                        message: "disposable space has no Set-layout type".to_owned(),
                    })?;
                let query = ctx
                    .client
                    .new_object(&ctx.space_id, &set_type.key)
                    .name(format!("Membership Query {}", unique_suffix()))
                    .create()
                    .await?;
                ctx.register_object(&query.id);
                let error = ctx
                    .client
                    .observe_collection_membership(&ctx.space_id, &query.id, &object.id)
                    .await
                    .expect_err("Set/query objects must fail closed");
                assert!(matches!(
                    error,
                    AnytypeError::CollectionMembershipEvidence {
                        kind: CollectionMembershipEvidenceKind::NotACollection
                    }
                ));
                Ok(())
            })
        },
    ))
    .await
    .expect("disposable membership harness");

    match outcome {
        DisposableRun::Completed(()) => assert!(callback_ran.load(Ordering::SeqCst)),
        DisposableRun::Skipped(reason) => {
            assert!(!callback_ran.load(Ordering::SeqCst));
            eprintln!("direct collection membership skipped before callback: {reason:?}");
        }
    }
}
