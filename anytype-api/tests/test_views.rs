//! Integration tests for Views (collections and queries)
//!
//! Validates listing views and objects within a view.

mod common;

use std::{
    collections::BTreeSet,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
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

async fn collect_canonical_members(
    client: &AnytypeClient,
    space_id: &str,
    collection_id: &str,
    limit: u32,
) -> anytype::Result<Vec<String>> {
    for attempt in 0..20 {
        match collect_canonical_members_once(client, space_id, collection_id, limit).await {
            Ok(object_ids) => return Ok(object_ids),
            Err(AnytypeError::CollectionMembershipEvidence { kind })
                if attempt < 19
                    && matches!(
                        kind,
                        CollectionMembershipEvidenceKind::InvalidCounters
                            | CollectionMembershipEvidenceKind::InvalidRecords
                            | CollectionMembershipEvidenceKind::ConcurrentShift
                    ) =>
            {
                sleep(Duration::from_millis(50)).await;
            }
            Err(error) => return Err(error),
        }
    }
    Err(AnytypeError::Other {
        message: "canonical membership test exhausted its restart bound".to_owned(),
    })
}

async fn collect_canonical_members_once(
    client: &AnytypeClient,
    space_id: &str,
    collection_id: &str,
    limit: u32,
) -> anytype::Result<Vec<String>> {
    let mut continuation = None;
    let mut object_ids = Vec::new();
    for _ in 0..64 {
        let before = client.http_metrics();
        let page = client
            .collection_membership_page(space_id, collection_id, limit, continuation.take())
            .await?;
        let after = client.http_metrics();
        let logical = after
            .logical_operations
            .saturating_sub(before.logical_operations);
        let physical = after
            .physical_attempts
            .saturating_sub(before.physical_attempts);
        if logical != 1 || !(1..=6).contains(&physical) {
            return Err(AnytypeError::Other {
                message: "canonical membership page exceeded its HTTP work budget".to_owned(),
            });
        }
        object_ids.extend(page.object_ids);
        continuation = page.continuation;
        if continuation.is_none() {
            return Ok(object_ids);
        }
    }
    Err(AnytypeError::Other {
        message: "canonical membership test exceeded its page bound".to_owned(),
    })
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

async fn second_view_continuation_pages(
    ctx: &anytype::test_util::TestContext,
    collection_id: &str,
    view_id: &str,
    expected_object_ids: &BTreeSet<String>,
) -> TestResult<()> {
    for attempt in 0..20 {
        let mut observed_object_ids = BTreeSet::new();
        let mut complete = true;
        for offset in 0_u32..3 {
            let page = match ctx
                .client
                .view_list_objects(&ctx.space_id, collection_id)
                .view(view_id)
                .limit(1)
                .offset(offset)
                .list()
                .await
            {
                Ok(page) => page,
                Err(AnytypeError::NotFound { .. }) => {
                    complete = false;
                    break;
                }
                Err(error) => return Err(error.into()),
            };
            let Some(object) = page.items.first() else {
                complete = false;
                break;
            };
            if page.pagination.offset != offset
                || page.pagination.total != expected_object_ids.len()
                || page.pagination.has_more != (offset < 2)
                || page.items.len() != 1
                || object.space_id != ctx.space_id
                || object.archived
                || !expected_object_ids.contains(&object.id)
                || !observed_object_ids.insert(object.id.clone())
            {
                complete = false;
                break;
            }
        }
        if complete && observed_object_ids == *expected_object_ids {
            return Ok(());
        }
        if attempt < 19 {
            sleep(Duration::from_millis(250)).await;
        }
    }
    Err(TestError::Assertion {
        message: "second-view continuation evidence did not converge".to_owned(),
    })
}

async fn exact_view_members(
    ctx: &anytype::test_util::TestContext,
    collection_id: &str,
    view_id: &str,
    expected_object_ids: &BTreeSet<String>,
) -> TestResult<()> {
    for attempt in 0..20 {
        let page = match ctx
            .client
            .view_list_objects(&ctx.space_id, collection_id)
            .view(view_id)
            .limit(100)
            .offset(0)
            .list()
            .await
        {
            Ok(page) => page,
            Err(AnytypeError::NotFound { .. }) => {
                if attempt < 19 {
                    sleep(Duration::from_millis(250)).await;
                    continue;
                }
                break;
            }
            Err(error) => return Err(error.into()),
        };
        let object_ids = page
            .items
            .iter()
            .map(|object| object.id.clone())
            .collect::<BTreeSet<_>>();
        if page.pagination.offset == 0
            && !page.pagination.has_more
            && page.pagination.total == expected_object_ids.len()
            && page.items.len() == object_ids.len()
            && object_ids == *expected_object_ids
        {
            return Ok(());
        }
        if attempt < 19 {
            sleep(Duration::from_millis(250)).await;
        }
    }
    Err(TestError::Assertion {
        message: "exact view membership evidence did not converge".to_owned(),
    })
}

struct OwnedCollectionAndSet {
    collection: Object,
    set: Object,
    source: Object,
}

async fn create_owned_collection_and_set(
    ctx: &anytype::test_util::TestContext,
) -> TestResult<OwnedCollectionAndSet> {
    let types = ctx
        .client
        .types(&ctx.space_id)
        .limit(1_000)
        .offset(0)
        .list()
        .await?
        .into_response();
    if types.pagination.offset != 0
        || types.pagination.has_more
        || types.pagination.total != types.items.len()
        || types.items.len() > 1_000
    {
        return Err(TestError::Assertion {
            message: "Set source type inventory is incomplete".to_owned(),
        });
    }
    let matching = types
        .items
        .iter()
        .filter(|typ| typ.key == "note" && !typ.archived)
        .collect::<Vec<_>>();
    let [source_type] = matching.as_slice() else {
        return Err(TestError::Assertion {
            message: "Set source type identity is ambiguous".to_owned(),
        });
    };
    let source_type = (*source_type).clone();
    let verify = VerifyConfig::default();
    let source_type = verify_semantic(
        &verify,
        "Set source type",
        &source_type.id,
        || {
            ctx.client
                .get_type(&ctx.space_id, &source_type.id)
                .get_direct()
        },
        |observed| {
            observed.id == source_type.id && observed.key == source_type.key && !observed.archived
        },
    )
    .await?;
    let source = create_owned_set_source(ctx, &source_type).await?;
    let set = ctx
        .create_set_fixture(&source_type, format!("Owned Set {}", unique_suffix()))
        .await?;
    let collection_type = ctx
        .create_collection_type_fixture(format!("Owned Collection Type {}", unique_suffix()))
        .await?;
    let collection = ctx
        .create_collection_fixture(
            &collection_type,
            format!("Owned Collection {}", unique_suffix()),
        )
        .await?;
    Ok(OwnedCollectionAndSet {
        collection,
        set,
        source,
    })
}

async fn create_owned_set_source(
    ctx: &anytype::test_util::TestContext,
    source_type: &Type,
) -> TestResult<Object> {
    const SNAPSHOT_LIMIT: u32 = 1_000;

    let snapshot = ctx
        .client
        .objects(&ctx.space_id)
        .filter(Filter::type_in([&source_type.id]))
        .limit(SNAPSHOT_LIMIT)
        .offset(0)
        .list()
        .await?
        .into_response();
    if snapshot.pagination.offset != 0
        || snapshot.pagination.has_more
        || snapshot.pagination.total != snapshot.items.len()
        || snapshot.items.len() > SNAPSHOT_LIMIT as usize
        || snapshot.items.iter().any(|object| {
            object.space_id != ctx.space_id
                || object.r#type.as_ref().map(|typ| typ.id.as_str())
                    != Some(source_type.id.as_str())
        })
    {
        return Err(TestError::Assertion {
            message: "Set source pre-create inventory is incomplete".to_owned(),
        });
    }
    let preexisting = snapshot
        .items
        .into_iter()
        .map(|object| object.id)
        .collect::<BTreeSet<_>>();
    if preexisting.len() != snapshot.pagination.total {
        return Err(TestError::Assertion {
            message: "Set source pre-create inventory contains duplicate identities".to_owned(),
        });
    }

    let name = format!("Owned Set Source {}", unique_suffix());
    let source = retry_definitive_rate_limit("Set source object", || async {
        ctx.client
            .new_object(&ctx.space_id, &source_type.key)
            .name(&name)
            .no_verify()
            .create()
            .await
    })
    .await?;
    ctx.client
        .get_config()
        .limits
        .validate_id(&source.id, "Set source object")?;
    if preexisting.contains(&source.id) {
        return Err(TestError::Assertion {
            message: "cleanup-owned Set source identity could not be established".to_owned(),
        });
    }
    let source_id = source.id;
    let verify = VerifyConfig::default();
    let verified = verify_semantic(
        &verify,
        "Set source object",
        &source_id,
        || ctx.client.object(&ctx.space_id, &source_id).get(),
        |object| {
            object.id == source_id
                && object.space_id == ctx.space_id
                && !object.archived
                && object.layout == source_type.layout
                && object.r#type.as_ref().map(|typ| typ.id.as_str())
                    == Some(source_type.id.as_str())
        },
    )
    .await?;
    ctx.register_object(&verified.id);
    Ok(verified)
}

async fn wait_for_owned_view_member(
    ctx: &anytype::test_util::TestContext,
    list_id: &str,
    view_id: &str,
    member_id: &str,
) -> TestResult<PagedResult<Object>> {
    for attempt in 0..20 {
        let listed = view_list_objects_with_retry(ctx, list_id, Some(view_id), 100).await?;
        if listed.items.iter().any(|object| object.id == member_id) {
            return Ok(listed);
        }
        if attempt < 19 {
            sleep(Duration::from_millis(250)).await;
        }
    }
    Err(TestError::Assertion {
        message: "cleanup-owned view member did not converge".to_owned(),
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
#[ignore = "requires a configured real server and disposable test admission"]
#[serial(disposable_anytype_api)]
async fn test_views_list_collection_and_set() {
    let callback_ran = Arc::new(AtomicBool::new(false));
    let callback_flag = Arc::clone(&callback_ran);
    let outcome = Box::pin(with_disposable_space_context(
        "views-list-collection-set",
        move |ctx| {
            callback_flag.store(true, Ordering::SeqCst);
            Box::pin(async move {
                let fixtures = create_owned_collection_and_set(&ctx).await?;

                let collection_views = list_views_with_retry(&ctx, &fixtures.collection.id).await?;
                assert!(!collection_views.items.is_empty());
                assert!(collection_views.iter().all(|view| !view.id.is_empty()));

                let set_views = list_views_with_retry(&ctx, &fixtures.set.id).await?;
                assert!(!set_views.items.is_empty());
                assert!(set_views.iter().all(|view| !view.id.is_empty()));

                Ok(())
            })
        },
    ))
    .await
    .expect("cleanup-safe collection and Set view-list harness");
    match outcome {
        DisposableRun::Completed(()) => assert!(callback_ran.load(Ordering::SeqCst)),
        DisposableRun::Skipped(reason) => {
            assert!(!callback_ran.load(Ordering::SeqCst));
            panic!("collection and Set view-list produced no evidence: {reason:?}");
        }
    }
}

#[tokio::test]
#[test_log::test]
#[ignore = "requires a configured real server and disposable test admission"]
#[serial(disposable_anytype_api)]
async fn test_view_list_objects_collection_and_set() {
    let callback_ran = Arc::new(AtomicBool::new(false));
    let callback_flag = Arc::clone(&callback_ran);
    let outcome = Box::pin(with_disposable_space_context(
        "view-objects-collection-set",
        move |ctx| {
            callback_flag.store(true, Ordering::SeqCst);
            Box::pin(async move {
                let fixtures = create_owned_collection_and_set(&ctx).await?;
                let second_view = ctx
                    .create_collection_view_fixture(
                        &fixtures.collection.id,
                        format!("Owned Collection Second View {}", unique_suffix()),
                    )
                    .await?;
                let selected_member_name =
                    format!("Owned Collection Second View Member {}", unique_suffix());
                let mut selected_members = Vec::with_capacity(3);
                for _ in 0..3 {
                    let member = retry_definitive_rate_limit(
                        "second-view continuation member setup",
                        || async {
                            ctx.client
                                .new_object(&ctx.space_id, "page")
                                .name(&selected_member_name)
                                .no_verify()
                                .create()
                                .await
                        },
                    )
                    .await?;
                    ctx.register_object(&member.id);
                    selected_members.push(member);
                }
                let default_only_member = retry_definitive_rate_limit(
                    "second-view default-only member setup",
                    || async {
                        ctx.client
                            .new_object(&ctx.space_id, "page")
                            .name(format!(
                                "Owned Collection Default-only Member {}",
                                unique_suffix()
                            ))
                            .no_verify()
                            .create()
                            .await
                    },
                )
                .await?;
                ctx.register_object(&default_only_member.id);
                let all_member_ids = selected_members
                    .iter()
                    .map(|member| member.id.clone())
                    .chain(std::iter::once(default_only_member.id.clone()))
                    .collect::<BTreeSet<_>>();
                let selected_member_ids = selected_members
                    .iter()
                    .map(|member| member.id.clone())
                    .collect::<BTreeSet<_>>();
                if selected_member_ids.len() != selected_members.len()
                    || all_member_ids.len() != selected_members.len() + 1
                {
                    return Err(TestError::Assertion {
                        message: "cleanup-owned second-view members are not unique".to_owned(),
                    });
                }
                ctx.client
                    .view_add_objects(
                        &ctx.space_id,
                        &fixtures.collection.id,
                        all_member_ids.iter().cloned(),
                    )
                    .await?;

                let collection_views = list_views_with_retry(&ctx, &fixtures.collection.id).await?;
                let second_views = collection_views
                    .items
                    .iter()
                    .filter(|view| {
                        view.id == second_view.id
                            && view.name.as_deref() == Some(second_view.name.as_str())
                    })
                    .collect::<Vec<_>>();
                if second_views.len() != 1 {
                    return Err(TestError::Assertion {
                        message: "cleanup-owned second view identity is not exact".to_owned(),
                    });
                }
                let default_views = collection_views
                    .items
                    .iter()
                    .filter(|view| view.id != second_view.id)
                    .collect::<Vec<_>>();
                let [default_view] = default_views.as_slice() else {
                    return Err(TestError::Assertion {
                        message: "cleanup-owned collection has no exact default view".to_owned(),
                    });
                };
                exact_view_members(
                    &ctx,
                    &fixtures.collection.id,
                    &second_view.id,
                    &all_member_ids,
                )
                .await?;
                exact_view_members(
                    &ctx,
                    &fixtures.collection.id,
                    &default_view.id,
                    &all_member_ids,
                )
                .await?;
                ctx.add_collection_name_filter_fixture(
                    &fixtures.collection.id,
                    &second_view.id,
                    &selected_member_name,
                )
                .await?;
                second_view_continuation_pages(
                    &ctx,
                    &fixtures.collection.id,
                    &second_view.id,
                    &selected_member_ids,
                )
                .await?;
                exact_view_members(
                    &ctx,
                    &fixtures.collection.id,
                    &default_view.id,
                    &all_member_ids,
                )
                .await?;

                let set_views = list_views_with_retry(&ctx, &fixtures.set.id).await?;
                let set_view = set_views
                    .items
                    .first()
                    .ok_or_else(|| TestError::Assertion {
                        message: "cleanup-owned Set has no default view".to_owned(),
                    })?;
                wait_for_owned_view_member(
                    &ctx,
                    &fixtures.set.id,
                    &set_view.id,
                    &fixtures.source.id,
                )
                .await?;

                Ok(())
            })
        },
    ))
    .await
    .expect("cleanup-safe collection and Set object-list harness");
    match outcome {
        DisposableRun::Completed(()) => assert!(callback_ran.load(Ordering::SeqCst)),
        DisposableRun::Skipped(reason) => {
            assert!(!callback_ran.load(Ordering::SeqCst));
            panic!("collection and Set object-list produced no evidence: {reason:?}");
        }
    }
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
                let object = retry_definitive_rate_limit("membership object A setup", || async {
                    ctx.client
                        .new_object(&ctx.space_id, "page")
                        .name(format!("Membership Object {}", unique_suffix()))
                        .create()
                        .await
                })
                .await?;
                ctx.register_object(&object.id);
                let object_b = retry_definitive_rate_limit("membership object B setup", || async {
                    ctx.client
                        .new_object(&ctx.space_id, "page")
                        .name(format!("Membership Object B {}", unique_suffix()))
                        .create()
                        .await
                })
                .await?;
                ctx.register_object(&object_b.id);
                let object_c = retry_definitive_rate_limit("membership object C setup", || async {
                    ctx.client
                        .new_object(&ctx.space_id, "page")
                        .name(format!("Membership Object C {}", unique_suffix()))
                        .create()
                        .await
                })
                .await?;
                ctx.register_object(&object_c.id);

                let verify = VerifyConfig::default();
                let pagination_verify = VerifyConfig {
                    timeout: Duration::from_secs(15),
                    initial_delay: Duration::ZERO,
                    max_delay: Duration::ZERO,
                    max_attempts: 1,
                };
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
                    .view_add_objects(&ctx.space_id, &collection.id, [&object.id, &object_b.id])
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

                let mut expected_members = vec![object.id.clone(), object_b.id.clone()];
                expected_members.sort();
                let listed = verify_semantic(
                    &pagination_verify,
                    "canonical collection membership pagination",
                    &collection.id,
                    || collect_canonical_members(&ctx.client, &ctx.space_id, &collection.id, 1),
                    |ids| {
                        let mut members = ids.clone();
                        members.sort();
                        members == expected_members
                    },
                )
                .await?;
                let restarted =
                    collect_canonical_members(&ctx.client, &ctx.space_id, &collection.id, 1)
                        .await?;
                assert_eq!(restarted, listed);
                for (target, expected_state) in [
                    (&object_b.id, CollectionMembershipState::Present),
                    (&object_c.id, CollectionMembershipState::Absent),
                ] {
                    let observed = ctx
                        .client
                        .observe_collection_membership(&ctx.space_id, &collection.id, target)
                        .await?;
                    assert_eq!(observed.state, expected_state);
                }

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
                let remaining = verify_semantic(
                    &pagination_verify,
                    "canonical collection membership after removal",
                    &collection.id,
                    || collect_canonical_members(&ctx.client, &ctx.space_id, &collection.id, 1),
                    |ids| ids == std::slice::from_ref(&object_b.id),
                )
                .await?;
                assert_eq!(remaining, std::slice::from_ref(&object_b.id));

                let types = ctx.client.types(&ctx.space_id).list().await?;
                let set_type = types
                    .items
                    .iter()
                    .find(|typ| typ.layout == ObjectLayout::Set)
                    .ok_or_else(|| TestError::Assertion {
                        message: "disposable space has no Set-layout type".to_owned(),
                    })?;
                let query = retry_definitive_rate_limit("membership query setup", || async {
                    ctx.client
                        .new_object(&ctx.space_id, &set_type.key)
                        .name(format!("Membership Query {}", unique_suffix()))
                        .create()
                        .await
                })
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
                let page_error = ctx
                    .client
                    .collection_membership_page(&ctx.space_id, &query.id, 1, None)
                    .await
                    .expect_err("Set/query pages must fail before subscription");
                assert!(matches!(
                    page_error,
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
