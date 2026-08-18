mod common;

use std::time::Duration;

use anytype::prelude::AnytypeError;
use anytype::test_util::{
    DEFINITIVE_RATE_LIMIT_MAX_ATTEMPTS, definitive_rate_limit_retry_delay,
    is_definitive_rate_limit_rejection, retry_definitive_rate_limit,
};

fn api_error(code: u16) -> AnytypeError {
    AnytypeError::ApiError {
        code,
        method: "post".to_string(),
        url: "/fixture".to_string(),
        message: "fixture response".to_string(),
    }
}

#[test]
fn only_typed_http_429_is_a_definitive_retryable_rejection() {
    assert!(is_definitive_rate_limit_rejection(&api_error(429)));

    for error in [
        api_error(400),
        api_error(408),
        api_error(500),
        AnytypeError::RateLimitExceeded {
            header: "unparsable or exhausted retry policy".to_string(),
            duration: Duration::ZERO,
        },
        AnytypeError::Validation {
            message: "invalid setup input".to_string(),
        },
    ] {
        assert!(!is_definitive_rate_limit_rejection(&error));
    }
}

#[test]
fn definitive_rate_limit_backoff_is_finite_and_bounded() {
    let delays: Vec<_> = (1..DEFINITIVE_RATE_LIMIT_MAX_ATTEMPTS)
        .map(|attempt| {
            definitive_rate_limit_retry_delay(attempt)
                .expect("every non-final failed attempt has a delay")
        })
        .collect();

    assert_eq!(
        delays,
        [
            Duration::from_millis(200),
            Duration::from_millis(400),
            Duration::from_millis(800),
            Duration::from_millis(1_500),
        ]
    );
    assert_eq!(
        delays.iter().copied().sum::<Duration>(),
        Duration::from_millis(2_900)
    );
    assert_eq!(definitive_rate_limit_retry_delay(0), None);
    assert_eq!(
        definitive_rate_limit_retry_delay(DEFINITIVE_RATE_LIMIT_MAX_ATTEMPTS),
        None
    );
}

#[tokio::test(start_paused = true)]
async fn definitive_rate_limit_retry_exhausts_at_the_finite_attempt_cap() {
    let mut attempts = 0;
    let error = retry_definitive_rate_limit("exhaustion fixture", || {
        attempts += 1;
        async { Err::<(), _>(api_error(429)) }
    })
    .await
    .expect_err("every attempt is rate limited");

    assert!(matches!(error, AnytypeError::ApiError { code: 429, .. }));
    assert_eq!(attempts, DEFINITIVE_RATE_LIMIT_MAX_ATTEMPTS);
}

#[tokio::test(start_paused = true)]
async fn indeterminate_failure_is_never_retried() {
    let mut attempts = 0;
    let error = retry_definitive_rate_limit("indeterminate fixture", || {
        attempts += 1;
        async { Err::<(), _>(api_error(500)) }
    })
    .await
    .expect_err("500 is returned directly");

    assert!(matches!(error, AnytypeError::ApiError { code: 500, .. }));
    assert_eq!(attempts, 1);
}

#[test]
fn live_mutation_retry_inventory_is_current() {
    // (path, source, generic, object-create, object-update, excluded terminal)
    let inventory = [
        // common has two setup calls plus the object-create helper delegation.
        ("common/mod.rs", include_str!("common/mod.rs"), 3, 0, 0, 0),
        (
            "../src/test_util.rs",
            include_str!("../src/test_util.rs"),
            7,
            0,
            0,
            1,
        ),
        (
            "integration.rs",
            include_str!("integration.rs"),
            2,
            0,
            0,
            11,
        ),
        ("smoke_test.rs", include_str!("smoke_test.rs"), 0, 0, 0, 2),
        ("test_cache.rs", include_str!("test_cache.rs"), 0, 0, 0, 0),
        (
            "test_chat_discovery.rs",
            include_str!("test_chat_discovery.rs"),
            2,
            0,
            0,
            1,
        ),
        (
            "test_chat_stream.rs",
            include_str!("test_chat_stream.rs"),
            4,
            0,
            0,
            2,
        ),
        ("test_chats.rs", include_str!("test_chats.rs"), 5, 0, 0, 11),
        ("test_files.rs", include_str!("test_files.rs"), 0, 0, 0, 8),
        (
            "test_filters.rs",
            include_str!("test_filters.rs"),
            0,
            22,
            0,
            0,
        ),
        (
            "test_members.rs",
            include_str!("test_members.rs"),
            0,
            0,
            0,
            0,
        ),
        (
            "test_pagination.rs",
            include_str!("test_pagination.rs"),
            0,
            0,
            0,
            0,
        ),
        (
            "test_properties.rs",
            include_str!("test_properties.rs"),
            14,
            11,
            0,
            9,
        ),
        ("test_search.rs", include_str!("test_search.rs"), 7, 0, 0, 1),
        ("test_tags.rs", include_str!("test_tags.rs"), 32, 3, 0, 10),
        ("test_types.rs", include_str!("test_types.rs"), 4, 1, 0, 10),
        (
            "test_validation.rs",
            include_str!("test_validation.rs"),
            0,
            4,
            0,
            12,
        ),
        ("test_views.rs", include_str!("test_views.rs"), 9, 0, 0, 0),
    ];

    let count = |source: &str, needle: &str| source.matches(needle).count();
    let mut setup_calls = 0;
    let mut excluded_calls = 0;

    for (path, source, generic_calls, object_create_calls, object_update_calls, excluded) in
        inventory
    {
        assert_eq!(
            count(source, "retry_definitive_rate_limit("),
            generic_calls,
            "generic retry inventory changed in {}",
            path
        );
        assert_eq!(
            count(source, "create_object_with_retry("),
            object_create_calls,
            "object-create retry inventory changed in {}",
            path
        );
        assert_eq!(
            count(source, "update_object_with_retry("),
            object_update_calls,
            "object-update retry inventory changed in {}",
            path
        );

        let terminal_calls = [".create()", ".update()", ".send()", ".upload()"]
            .iter()
            .map(|needle| count(source, needle))
            .sum::<usize>();
        let delegated_calls = usize::from(path == "common/mod.rs");
        let wrapped_calls =
            generic_calls - delegated_calls + object_create_calls + object_update_calls;
        assert_eq!(
            terminal_calls - wrapped_calls,
            excluded,
            "direct terminal mutation inventory changed in {}",
            path
        );

        setup_calls += wrapped_calls;
        excluded_calls += excluded;
    }

    assert_eq!(setup_calls, 129);
    assert_eq!(excluded_calls, 78);
}
