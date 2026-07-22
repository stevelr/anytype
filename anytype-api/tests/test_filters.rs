//! Integration tests for Filter and FilterExpression functionality
//!
//! Tests the Filter API against a live Anytype server, covering:
//! - Basic filters (text, number, date, checkbox, select)
//! - Filter composition (AND/OR logic)
//! - Filter application to object lists
//! - Error handling and validation
//!
//! ## Environment Requirements
//!
//! Required environment variables (see .test-env):
//! - `ANYTYPE_TEST_URL` - API endpoint (default: http://127.0.0.1:31012)
//! - `ANYTYPE_KEYSTORE` - Keystore specification (for example, `file:path=/path/to/keys.db`)
//! - `ANYTYPE_TEST_SPACE_ID` - Existing space ID for testing
//!
//! ## Running
//!
//! ```bash
//! source .test-env
//! ANYTYPE_DISPOSABLE_TEST_PROCESS=1 cargo test -p anytype --test test_filters \
//!   test_filter_number_checkbox_condition_matrix -- --ignored
//! ```
//!
//! The ignored matrix always executes all eleven fixed cases independently on
//! both endpoints before cleanup. It then reports only the static endpoint and
//! case labels, a closed failure category, and a validated HTTP status/class
//! when one was available; request values, URLs, response bodies, and fixture
//! identities are never retained.
//!
//! On the `anytype-cli` 0.3.6 acceptance server, all eleven object-list cells
//! return HTTP 400. Scoped search passes number `eq`, `gt`, `gte`, negative
//! decimal `eq`, checkbox `eq true`, and checkbox `ne false`; its other five
//! cells fail bounded exact-identity convergence. Keep the test ignored as a
//! deliberate compatibility probe until those upstream behaviors change.

mod common;

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use anytype::{
    prelude::*,
    test_assert,
    test_util::{
        DisposableCallbackStage, DisposableFailureCategory, DisposableRun, TestContext, TestError,
        TestResult, disposable_callback_error, with_disposable_space_context, with_test_context,
    },
};
use serial_test::serial;
use tokio::time::{Duration, sleep};

use crate::common::{
    create_object_with_retry, ensure_properties_and_type, ensure_properties_and_type_quiet,
    lookup_property_tag_with_retry, unique_test_name,
};

// =============================================================================
// Test Helper - Create Test Objects
// =============================================================================

/// Creates a set of test objects with various property values for filter testing
async fn create_test_objects(ctx: &TestContext) -> TestResult<(Vec<Object>, String)> {
    create_test_objects_with_logging(ctx, true).await
}

async fn create_filter_matrix_objects(ctx: &TestContext) -> TestResult<(Vec<Object>, String)> {
    create_test_objects_with_logging(ctx, false).await
}

async fn create_test_objects_with_logging(
    ctx: &TestContext,
    emit_values: bool,
) -> TestResult<(Vec<Object>, String)> {
    // Ensure all required properties exist before creating objects
    let type_key = if emit_values {
        ensure_properties_and_type(ctx).await?
    } else {
        ensure_properties_and_type_quiet(ctx).await?
    };
    let decimal_priority =
        serde_json::Number::from_f64(-2.5).ok_or_else(|| TestError::Assertion {
            message: "finite decimal fixture must be representable".to_owned(),
        })?;
    let mut objects = Vec::new();

    // Object 1: High priority, checked, recent date
    let obj1_name = unique_test_name("Filter Test High Priority");
    let obj1 = create_object_with_retry("Filter Test High Priority", || async {
        ctx.client
            .new_object(&ctx.space_id, &type_key)
            .name(&obj1_name)
            .body("Important task")
            .set_text("description", "High priority item")
            .set_number("priority", decimal_priority.clone())
            .set_checkbox("done", true)
            .create()
            .await
    })
    .await?;
    ctx.register_object(&obj1.id);
    objects.push(obj1);

    // Object 2: Medium priority, unchecked
    let obj2_name = unique_test_name("Filter Test Medium Priority");
    let obj2 = create_object_with_retry("Filter Test Medium Priority", || async {
        ctx.client
            .new_object(&ctx.space_id, &type_key)
            .name(&obj2_name)
            .body("Normal task")
            .set_text("description", "Medium priority item")
            .set_number("priority", 5)
            .set_checkbox("done", false)
            .create()
            .await
    })
    .await?;
    ctx.register_object(&obj2.id);
    objects.push(obj2);

    // Object 3: Low priority, unchecked, empty description
    let obj3_name = unique_test_name("Filter Test Low Priority");
    let obj3 = create_object_with_retry("Filter Test Low Priority", || async {
        ctx.client
            .new_object(&ctx.space_id, &type_key)
            .name(&obj3_name)
            .body("Low priority task")
            .set_number("priority", 10)
            .set_checkbox("done", false)
            .create()
            .await
    })
    .await?;
    ctx.register_object(&obj3.id);
    objects.push(obj3);

    // Object 4: No priority set, has description
    let obj4_name = unique_test_name("Filter Test No Priority");
    let obj4 = create_object_with_retry("Filter Test No Priority", || async {
        ctx.client
            .new_object(&ctx.space_id, &type_key)
            .name(&obj4_name)
            .body("Task without priority")
            .set_text("description", "Item without priority value")
            .create()
            .await
    })
    .await?;
    ctx.register_object(&obj4.id);
    objects.push(obj4);

    Ok((objects, type_key))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NumericCheckboxCase {
    NumberEqualInteger,
    NumberNotEqual,
    NumberLess,
    NumberLessOrEqual,
    NumberGreater,
    NumberGreaterOrEqual,
    NumberEqualDecimal,
    CheckboxEqualTrue,
    CheckboxEqualFalse,
    CheckboxNotEqualTrue,
    CheckboxNotEqualFalse,
}

impl NumericCheckboxCase {
    const ALL: [Self; 11] = [
        Self::NumberEqualInteger,
        Self::NumberNotEqual,
        Self::NumberLess,
        Self::NumberLessOrEqual,
        Self::NumberGreater,
        Self::NumberGreaterOrEqual,
        Self::NumberEqualDecimal,
        Self::CheckboxEqualTrue,
        Self::CheckboxEqualFalse,
        Self::CheckboxNotEqualTrue,
        Self::CheckboxNotEqualFalse,
    ];

    fn filter(self) -> TestResult<Filter> {
        let filter = match self {
            Self::NumberEqualInteger => Filter::number_equal("priority", 5),
            Self::NumberNotEqual => Filter::number_not_equal("priority", 5),
            Self::NumberLess => Filter::number_less("priority", 5),
            Self::NumberLessOrEqual => Filter::number_less_or_equal("priority", 5),
            Self::NumberGreater => Filter::number_greater("priority", 5),
            Self::NumberGreaterOrEqual => Filter::number_greater_or_equal("priority", 5),
            Self::NumberEqualDecimal => Filter::number_equal(
                "priority",
                serde_json::Number::from_f64(-2.5).ok_or_else(|| TestError::Assertion {
                    message: "finite decimal filter must be representable".to_owned(),
                })?,
            ),
            Self::CheckboxEqualTrue => Filter::checkbox_equal("done", true),
            Self::CheckboxEqualFalse => Filter::checkbox_equal("done", false),
            Self::CheckboxNotEqualTrue => Filter::checkbox_not_equal("done", true),
            Self::CheckboxNotEqualFalse => Filter::checkbox_not_equal("done", false),
        };
        Ok(filter)
    }

    const fn expected_indexes(self) -> &'static [usize] {
        match self {
            Self::NumberEqualInteger => &[1],
            Self::NumberNotEqual => &[0, 2],
            Self::NumberLess => &[0],
            Self::NumberLessOrEqual => &[0, 1],
            Self::NumberGreater => &[2],
            Self::NumberGreaterOrEqual => &[1, 2],
            Self::NumberEqualDecimal => &[0],
            Self::CheckboxEqualTrue | Self::CheckboxNotEqualFalse => &[0],
            Self::CheckboxEqualFalse | Self::CheckboxNotEqualTrue => &[1, 2],
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::NumberEqualInteger => "number eq integer",
            Self::NumberNotEqual => "number ne",
            Self::NumberLess => "number lt",
            Self::NumberLessOrEqual => "number lte",
            Self::NumberGreater => "number gt",
            Self::NumberGreaterOrEqual => "number gte",
            Self::NumberEqualDecimal => "number eq negative decimal",
            Self::CheckboxEqualTrue => "checkbox eq true",
            Self::CheckboxEqualFalse => "checkbox eq false",
            Self::CheckboxNotEqualTrue => "checkbox ne true",
            Self::CheckboxNotEqualFalse => "checkbox ne false",
        }
    }

    const fn callback_stage(self) -> DisposableCallbackStage {
        match self {
            Self::NumberEqualInteger => DisposableCallbackStage::NumberEqualInteger,
            Self::NumberNotEqual => DisposableCallbackStage::NumberNotEqual,
            Self::NumberLess => DisposableCallbackStage::NumberLess,
            Self::NumberLessOrEqual => DisposableCallbackStage::NumberLessOrEqual,
            Self::NumberGreater => DisposableCallbackStage::NumberGreater,
            Self::NumberGreaterOrEqual => DisposableCallbackStage::NumberGreaterOrEqual,
            Self::NumberEqualDecimal => DisposableCallbackStage::NumberEqualDecimal,
            Self::CheckboxEqualTrue => DisposableCallbackStage::CheckboxEqualTrue,
            Self::CheckboxEqualFalse => DisposableCallbackStage::CheckboxEqualFalse,
            Self::CheckboxNotEqualTrue => DisposableCallbackStage::CheckboxNotEqualTrue,
            Self::CheckboxNotEqualFalse => DisposableCallbackStage::CheckboxNotEqualFalse,
        }
    }

    const fn index(self) -> usize {
        match self {
            Self::NumberEqualInteger => 0,
            Self::NumberNotEqual => 1,
            Self::NumberLess => 2,
            Self::NumberLessOrEqual => 3,
            Self::NumberGreater => 4,
            Self::NumberGreaterOrEqual => 5,
            Self::NumberEqualDecimal => 6,
            Self::CheckboxEqualTrue => 7,
            Self::CheckboxEqualFalse => 8,
            Self::CheckboxNotEqualTrue => 9,
            Self::CheckboxNotEqualFalse => 10,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FilterEndpoint {
    ObjectList,
    ScopedSearch,
}

impl FilterEndpoint {
    const ALL: [Self; 2] = [Self::ObjectList, Self::ScopedSearch];

    const fn index(self) -> usize {
        match self {
            Self::ObjectList => 0,
            Self::ScopedSearch => 1,
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::ObjectList => "object_list",
            Self::ScopedSearch => "scoped_search",
        }
    }
}

const FILTER_INVENTORY_LEN: usize = FilterEndpoint::ALL.len() * NumericCheckboxCase::ALL.len();

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SafeHttpStatusClass {
    Informational,
    Success,
    Redirection,
    ClientError,
    ServerError,
}

impl SafeHttpStatusClass {
    const fn from_code(code: u16) -> Option<Self> {
        match code {
            100..=199 => Some(Self::Informational),
            200..=299 => Some(Self::Success),
            300..=399 => Some(Self::Redirection),
            400..=499 => Some(Self::ClientError),
            500..=599 => Some(Self::ServerError),
            _ => None,
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Informational => "informational",
            Self::Success => "success",
            Self::Redirection => "redirection",
            Self::ClientError => "client_error",
            Self::ServerError => "server_error",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SafeHttpStatus {
    code: u16,
    class: SafeHttpStatusClass,
}

impl SafeHttpStatus {
    const fn from_code(code: u16) -> Option<Self> {
        match SafeHttpStatusClass::from_code(code) {
            Some(class) => Some(Self { code, class }),
            None => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FilterCaseOutcome {
    Passed,
    Failed {
        category: DisposableFailureCategory,
        status: Option<SafeHttpStatus>,
    },
}

#[derive(Debug, PartialEq, Eq)]
struct FilterCaseInventory {
    outcomes: [Option<FilterCaseOutcome>; FILTER_INVENTORY_LEN],
}

impl FilterCaseInventory {
    const fn new() -> Self {
        Self {
            outcomes: [None; FILTER_INVENTORY_LEN],
        }
    }

    const fn index(endpoint: FilterEndpoint, case: NumericCheckboxCase) -> usize {
        endpoint.index() * NumericCheckboxCase::ALL.len() + case.index()
    }

    fn record(
        &mut self,
        endpoint: FilterEndpoint,
        case: NumericCheckboxCase,
        outcome: FilterCaseOutcome,
    ) -> TestResult<()> {
        let slot = &mut self.outcomes[Self::index(endpoint, case)];
        if slot.is_some() {
            return Err(TestError::Assertion {
                message: format!(
                    "duplicate filter inventory case: {} {}",
                    endpoint.label(),
                    case.label()
                ),
            });
        }
        *slot = Some(outcome);
        Ok(())
    }

    fn ensure_complete(&self) -> TestResult<()> {
        for endpoint in FilterEndpoint::ALL {
            for case in NumericCheckboxCase::ALL {
                if self.outcomes[Self::index(endpoint, case)].is_none() {
                    return Err(TestError::Assertion {
                        message: format!(
                            "missing filter inventory case: {} {}",
                            endpoint.label(),
                            case.label()
                        ),
                    });
                }
            }
        }
        Ok(())
    }

    fn failure_count(&self) -> usize {
        self.outcomes
            .iter()
            .filter(|outcome| matches!(outcome, Some(FilterCaseOutcome::Failed { .. })))
            .count()
    }

    fn render(&self) -> TestResult<String> {
        self.ensure_complete()?;
        let mut lines = Vec::with_capacity(FILTER_INVENTORY_LEN);
        for endpoint in FilterEndpoint::ALL {
            for case in NumericCheckboxCase::ALL {
                let line = match self.outcomes[Self::index(endpoint, case)] {
                    Some(FilterCaseOutcome::Passed) => format!(
                        "endpoint={} case={} result=pass",
                        endpoint.label(),
                        case.label()
                    ),
                    Some(FilterCaseOutcome::Failed {
                        category,
                        status: Some(status),
                    }) => format!(
                        "endpoint={} case={} result=fail category={} http_status={} http_class={}",
                        endpoint.label(),
                        case.label(),
                        category,
                        status.code,
                        status.class.label(),
                    ),
                    Some(FilterCaseOutcome::Failed {
                        category,
                        status: None,
                    }) => format!(
                        "endpoint={} case={} result=fail category={} http_status=unavailable",
                        endpoint.label(),
                        case.label(),
                        category,
                    ),
                    None => {
                        return Err(TestError::Assertion {
                            message: format!(
                                "missing filter inventory case: {} {}",
                                endpoint.label(),
                                case.label()
                            ),
                        });
                    }
                };
                lines.push(line);
            }
        }
        Ok(lines.join("\n"))
    }
}

fn safe_http_status(error: &TestError) -> Option<SafeHttpStatus> {
    let code = match error {
        TestError::Api {
            source: AnytypeError::ApiError { code, .. },
        } => Some(*code),
        TestError::Api {
            source: AnytypeError::Http { source, .. },
        } => source.status().map(|status| status.as_u16()),
        TestError::Api {
            source: AnytypeError::FileHeaderEvidenceTooLarge { status, .. },
        }
        | TestError::Api {
            source: AnytypeError::InvalidFileResponseHeader { status, .. },
        } => Some(*status),
        _ => None,
    };
    code.and_then(SafeHttpStatus::from_code)
}

fn closed_filter_case_outcome(
    case: NumericCheckboxCase,
    result: TestResult<()>,
) -> FilterCaseOutcome {
    match result {
        Ok(()) => FilterCaseOutcome::Passed,
        Err(error) => {
            let status = safe_http_status(&error);
            let category = match disposable_callback_error(case.callback_stage(), error) {
                TestError::DisposableCallback { category, .. } => category,
                _ => DisposableFailureCategory::Callback,
            };
            FilterCaseOutcome::Failed { category, status }
        }
    }
}

#[test]
fn filter_case_inventory_aggregates_in_canonical_order() -> TestResult<()> {
    let incomplete = FilterCaseInventory::new()
        .render()
        .expect_err("a partial 22-slot inventory must fail closed");
    assert!(matches!(incomplete, TestError::Assertion { .. }));

    let mut inventory = FilterCaseInventory::new();
    for endpoint in FilterEndpoint::ALL.into_iter().rev() {
        for case in NumericCheckboxCase::ALL.into_iter().rev() {
            inventory.record(endpoint, case, FilterCaseOutcome::Passed)?;
        }
    }

    let report = inventory.render()?;
    let lines = report.lines().collect::<Vec<_>>();
    assert_eq!(lines.len(), FILTER_INVENTORY_LEN);
    assert_eq!(
        lines.first().copied(),
        Some("endpoint=object_list case=number eq integer result=pass")
    );
    assert_eq!(
        lines.get(NumericCheckboxCase::ALL.len()).copied(),
        Some("endpoint=scoped_search case=number eq integer result=pass")
    );
    assert_eq!(
        lines.last().copied(),
        Some("endpoint=scoped_search case=checkbox ne false result=pass")
    );
    assert_eq!(inventory.failure_count(), 0);

    let duplicate = inventory
        .record(
            FilterEndpoint::ObjectList,
            NumericCheckboxCase::NumberEqualInteger,
            FilterCaseOutcome::Passed,
        )
        .expect_err("duplicate inventory entries must fail closed");
    assert!(matches!(duplicate, TestError::Assertion { .. }));
    Ok(())
}

#[test]
fn filter_case_inventory_redacts_failures_and_retains_safe_status() -> TestResult<()> {
    const SECRET_BODY: &str = "secret-upstream-filter-body";
    const SECRET_URL: &str = "http://secret.invalid/v1/spaces/secret/filter?token=secret";
    let failed = closed_filter_case_outcome(
        NumericCheckboxCase::NumberEqualInteger,
        Err(TestError::Api {
            source: AnytypeError::ApiError {
                code: 500,
                method: "GET".to_owned(),
                url: SECRET_URL.to_owned(),
                message: SECRET_BODY.to_owned(),
            },
        }),
    );
    assert_eq!(
        failed,
        FilterCaseOutcome::Failed {
            category: DisposableFailureCategory::ApiError,
            status: Some(SafeHttpStatus {
                code: 500,
                class: SafeHttpStatusClass::ServerError,
            }),
        }
    );

    let mut inventory = FilterCaseInventory::new();
    for endpoint in FilterEndpoint::ALL {
        for case in NumericCheckboxCase::ALL {
            inventory.record(
                endpoint,
                case,
                if endpoint == FilterEndpoint::ObjectList
                    && case == NumericCheckboxCase::NumberEqualInteger
                {
                    failed
                } else {
                    FilterCaseOutcome::Passed
                },
            )?;
        }
    }
    let report = inventory.render()?;
    assert_eq!(inventory.failure_count(), 1);
    assert!(report.starts_with(
        "endpoint=object_list case=number eq integer result=fail category=api_error http_status=500 http_class=server_error\n"
    ));
    assert!(!report.contains(SECRET_BODY));
    assert!(!report.contains("secret.invalid"));
    assert!(!report.contains("token="));
    Ok(())
}

#[test]
fn filter_case_inventory_discards_non_http_status_codes() {
    let failed = closed_filter_case_outcome(
        NumericCheckboxCase::NumberEqualInteger,
        Err(TestError::Api {
            source: AnytypeError::ApiError {
                code: 700,
                method: "GET".to_owned(),
                url: "secret-url".to_owned(),
                message: "secret-body".to_owned(),
            },
        }),
    );
    assert_eq!(
        failed,
        FilterCaseOutcome::Failed {
            category: DisposableFailureCategory::ApiError,
            status: None,
        }
    );
}

fn sorted_ids(objects: &[Object]) -> Vec<String> {
    let mut ids = objects
        .iter()
        .map(|object| object.id.clone())
        .collect::<Vec<_>>();
    ids.sort_unstable();
    ids
}

fn expected_ids(objects: &[Object], indexes: &[usize]) -> TestResult<Vec<String>> {
    let mut ids = Vec::with_capacity(indexes.len());
    for index in indexes {
        let object = objects.get(*index).ok_or_else(|| TestError::Assertion {
            message: "filter fixture index is out of bounds".to_owned(),
        })?;
        ids.push(object.id.clone());
    }
    ids.sort_unstable();
    Ok(ids)
}

async fn assert_filter_endpoint_case(
    ctx: &TestContext,
    objects: &[Object],
    type_key: &str,
    endpoint: FilterEndpoint,
    case: NumericCheckboxCase,
) -> TestResult<()> {
    const MAX_INDEX_ATTEMPTS: usize = 20;

    let expected = expected_ids(objects, case.expected_indexes())?;
    let mut last_count = 0;
    for attempt in 1..=MAX_INDEX_ATTEMPTS {
        let actual = match endpoint {
            FilterEndpoint::ObjectList => {
                ctx.client
                    .objects(&ctx.space_id)
                    .filter(Filter::type_in([type_key]))
                    .filter(case.filter()?)
                    .limit(100)
                    .list()
                    .await?
                    .collect_all()
                    .await?
            }
            FilterEndpoint::ScopedSearch => {
                ctx.client
                    .search_in(&ctx.space_id)
                    .types([type_key])
                    .filters(FilterExpression::from(vec![case.filter()?]))
                    .limit(100)
                    .execute()
                    .await?
                    .collect_all()
                    .await?
            }
        };

        let actual_ids = sorted_ids(&actual);
        if actual_ids == expected {
            return Ok(());
        }
        last_count = actual_ids.len();
        if attempt < MAX_INDEX_ATTEMPTS {
            sleep(Duration::from_millis(250)).await;
        }
    }

    Err(TestError::Assertion {
        message: format!(
            "{} {} did not converge to {} exact cleanup-owned identities: returned {}",
            endpoint.label(),
            case.label(),
            expected.len(),
            last_count,
        ),
    })
}

// =============================================================================
// Basic Filter Tests
// =============================================================================

#[tokio::test]
#[test_log::test]
#[serial]
async fn test_filter_not_empty() -> TestResult<()> {
    with_test_context(|ctx| async move {
        let (test_objs, _type_key) = create_test_objects(&ctx).await?;

        // Filter for objects with non-empty name
        let results = ctx
            .client
            .objects(&ctx.space_id)
            .filter(Filter::not_empty("name"))
            .limit(100)
            .list()
            .await?;

        // All our test objects have names
        let found_count = results
            .iter()
            .filter(|obj| test_objs.iter().any(|test_obj| test_obj.id == obj.id))
            .count();

        assert_eq!(
            found_count, 4,
            "Expected all 4 test objects with names to be found"
        );

        Ok(())
    })
    .await
}

#[tokio::test]
#[test_log::test]
#[serial]
async fn test_filter_is_empty() -> TestResult<()> {
    with_test_context(|ctx| async move {
        let (test_objs, _type_key) = create_test_objects(&ctx).await?;

        // Filter for objects with empty description
        let results = ctx
            .client
            .objects(&ctx.space_id)
            .filter(Filter::is_empty("description"))
            .limit(100)
            .list()
            .await?;

        // Object 3 has no description
        let found = results.iter().any(|obj| obj.id == test_objs[2].id);

        assert!(found, "Expected to find object with empty description");

        Ok(())
    })
    .await
}

#[tokio::test]
#[test_log::test]
#[ignore = "legacy shared-space case; covered by the cleanup-owned condition matrix"]
#[serial]
async fn test_filter_checkbox_true() -> TestResult<()> {
    with_test_context(|ctx| async move {
        let task_name = unique_test_name("test checkbox task done");
        let task = create_object_with_retry("Filter Checkbox True", || async {
            ctx.client
                .new_object(&ctx.space_id, "task")
                .name(&task_name)
                .body("Important task")
                .set_checkbox("done", true)
                .create()
                .await
        })
        .await?;
        ctx.register_object(&task.id);

        // Filter for checked items
        let results = ctx
            .client
            .objects(&ctx.space_id)
            .filter(Filter::checkbox_true("done"))
            .limit(100)
            .list()
            .await?
            .collect_all()
            .await?;

        let found = results.iter().any(|obj| obj.id == task.id);
        test_assert!(found, "Expected to find object with done=true");

        test_assert!(
            results
                .iter()
                .all(|obj| obj.get_property_bool("done") == Some(true)),
            "all 'done' should be true based on filter"
        );
        Ok(())
    })
    .await
}

#[tokio::test]
#[test_log::test]
#[ignore = "legacy shared-space case; covered by the cleanup-owned condition matrix"]
#[serial]
async fn test_filter_checkbox_false() -> TestResult<()> {
    with_test_context(|ctx| async move {
        let task_name = unique_test_name("test checkbox task not done");
        let task = create_object_with_retry("Filter Checkbox False", || async {
            ctx.client
                .new_object(&ctx.space_id, "task")
                .name(&task_name)
                .body("Important task")
                .set_checkbox("done", false)
                .create()
                .await
        })
        .await?;
        ctx.register_object(&task.id);

        // Filter for checked items
        let results = ctx
            .client
            .objects(&ctx.space_id)
            .filter(Filter::checkbox_false("done"))
            .limit(100)
            .list()
            .await?
            .collect_all()
            .await?;

        let found = results.iter().any(|obj| obj.id == task.id);
        test_assert!(found, "Expected to find object with done=false");

        for t in results.iter() {
            eprintln!(
                "{}: {:?}",
                t.name.as_deref().unwrap_or("(unnamed)"),
                t.get_property("done"),
            );
        }
        test_assert!(
            results
                .iter()
                .all(|obj| obj.get_property_bool("done") == Some(false)),
            "all 'done' should be false based on filter"
        );

        Ok(())
    })
    .await
}

// =============================================================================
// Numeric Filter Tests
// =============================================================================

#[tokio::test]
#[test_log::test]
#[ignore = "legacy shared-space case; covered by the cleanup-owned condition matrix"]
#[serial]
async fn test_filter_number_equal() -> TestResult<()> {
    with_test_context(|ctx| async move {
        let (test_objs, _type_key) = create_test_objects(&ctx).await?;

        // Filter for priority equal to 5
        let results = ctx
            .client
            .objects(&ctx.space_id)
            .filter(Filter::number_equal("priority", 5))
            .limit(100)
            .list()
            .await?;

        // Object 2 has priority=5
        let found = results.iter().any(|obj| obj.id == test_objs[1].id);

        assert!(found, "Expected to find object with priority=5");

        Ok(())
    })
    .await
}

#[tokio::test]
#[test_log::test]
#[ignore = "legacy shared-space case; covered by the cleanup-owned condition matrix"]
#[serial]
async fn test_filter_number_greater_than() -> TestResult<()> {
    with_test_context(|ctx| async move {
        let (test_objs, _type_key) = create_test_objects(&ctx).await?;

        // Filter for priority greater than 5
        let results = ctx
            .client
            .objects(&ctx.space_id)
            .filter(Filter::number_greater("priority", 5))
            .limit(100)
            .list()
            .await?;

        // Object 3 has priority=10
        let found = results.iter().any(|obj| obj.id == test_objs[2].id);

        assert!(found, "Expected to find object with priority > 5");

        Ok(())
    })
    .await
}

#[tokio::test]
#[test_log::test]
#[ignore = "legacy shared-space case; covered by the cleanup-owned condition matrix"]
#[serial]
async fn test_filter_number_less_than() -> TestResult<()> {
    with_test_context(|ctx| async move {
        let (test_objs, _type_key) = create_test_objects(&ctx).await?;

        // Filter for priority less than 5
        let results = ctx
            .client
            .objects(&ctx.space_id)
            .filter(Filter::number_less("priority", 5))
            .limit(100)
            .list()
            .await?;

        // Object 1 has priority=1
        let found = results.iter().any(|obj| obj.id == test_objs[0].id);

        assert!(found, "Expected to find object with priority < 5");

        Ok(())
    })
    .await
}

#[tokio::test]
#[test_log::test]
#[ignore = "legacy shared-space case; covered by the cleanup-owned condition matrix"]
#[serial]
async fn test_filter_number_range() -> TestResult<()> {
    with_test_context(|ctx| async move {
        let (test_objs, _type_key) = create_test_objects(&ctx).await?;

        // Filter for priority in range [3, 7] using >= 3 AND <= 7
        let results = ctx
            .client
            .objects(&ctx.space_id)
            .filter(Filter::number_greater_or_equal("priority", 3))
            .filter(Filter::number_less_or_equal("priority", 7))
            .limit(100)
            .list()
            .await?;

        // Only object 2 has priority=5 in range
        let found_obj2 = results.iter().any(|obj| obj.id == test_objs[1].id);
        let not_found_obj1 = !results.iter().any(|obj| obj.id == test_objs[0].id);
        let not_found_obj3 = !results.iter().any(|obj| obj.id == test_objs[2].id);

        assert!(
            found_obj2 && not_found_obj1 && not_found_obj3,
            "Expected only object with priority in range [3, 7]"
        );

        Ok(())
    })
    .await
}

#[tokio::test]
#[ignore = "upstream compatibility probe; requires disposable real-server admission"]
#[serial(disposable_anytype_api)]
async fn test_filter_number_checkbox_condition_matrix() {
    let callback_ran = Arc::new(AtomicBool::new(false));
    let callback_flag = callback_ran.clone();
    let outcome = Box::pin(with_disposable_space_context(
        "filter-number-checkbox-matrix",
        move |ctx| {
            callback_flag.store(true, Ordering::SeqCst);
            Box::pin(async move {
                let (objects, type_key) =
                    create_filter_matrix_objects(&ctx).await.map_err(|error| {
                        disposable_callback_error(DisposableCallbackStage::Fixture, error)
                    })?;
                let mut inventory = FilterCaseInventory::new();
                for endpoint in FilterEndpoint::ALL {
                    for case in NumericCheckboxCase::ALL {
                        let outcome = closed_filter_case_outcome(
                            case,
                            assert_filter_endpoint_case(&ctx, &objects, &type_key, endpoint, case)
                                .await,
                        );
                        inventory.record(endpoint, case, outcome).map_err(|error| {
                            disposable_callback_error(case.callback_stage(), error)
                        })?;
                    }
                }
                inventory.ensure_complete().map_err(|error| {
                    disposable_callback_error(DisposableCallbackStage::Fixture, error)
                })?;
                Ok(inventory)
            })
        },
    ))
    .await
    .expect("cleanup-safe numeric/checkbox filter harness");
    match outcome {
        DisposableRun::Completed(inventory) => {
            assert!(callback_ran.load(Ordering::SeqCst));
            let report = inventory
                .render()
                .expect("complete static filter inventory");
            eprintln!("{report}");
            assert_eq!(
                inventory.failure_count(),
                0,
                "real-server filter matrix contains unsupported endpoint cases:\n{report}",
            );
        }
        DisposableRun::Skipped(reason) => {
            assert!(!callback_ran.load(Ordering::SeqCst));
            eprintln!("numeric/checkbox filter matrix skipped before callback: {reason:?}");
        }
    }
}

// =============================================================================
// Date Filter Tests
// =============================================================================

#[tokio::test]
#[test_log::test]
#[serial]
async fn test_filter_date_equal() -> TestResult<()> {
    with_test_context(|ctx| async move {
        let (_test_objs, type_key) = create_test_objects(&ctx).await?;

        let date = "2025-06-15";
        let obj_name = unique_test_name("Date Equal Test");
        let obj = create_object_with_retry("Filter Date Equal", || async {
            ctx.client
                .new_object(&ctx.space_id, &type_key)
                .name(&obj_name)
                .body("Test")
                .set_date("due_date", date)
                .create()
                .await
        })
        .await?;
        ctx.register_object(&obj.id);

        // Filter for exact date match
        let results = ctx
            .client
            .objects(&ctx.space_id)
            .filter(Filter::date_equal("due_date", date))
            .limit(10)
            .list()
            .await?;

        let found = results.iter().any(|o| o.id == obj.id);
        assert!(found, "Expected to find object with matching due_date");

        Ok(())
    })
    .await
}

#[tokio::test]
#[test_log::test]
#[serial]
async fn test_filter_date_after() -> TestResult<()> {
    with_test_context(|ctx| async move {
        let (_test_objs, type_key) = create_test_objects(&ctx).await?;
        let future_date = "2030-12-31";
        let obj_name = unique_test_name("Date After Test");
        let obj = create_object_with_retry("Filter Date After", || async {
            ctx.client
                .new_object(&ctx.space_id, &type_key)
                .name(&obj_name)
                .body("Test")
                .set_date("due_date", future_date)
                .create()
                .await
        })
        .await?;
        ctx.register_object(&obj.id);

        // Filter for dates after 2030
        let results = ctx
            .client
            .objects(&ctx.space_id)
            .filter(Filter::date_greater("due_date", "2030-01-01"))
            .limit(100)
            .list()
            .await?;

        let found = results.iter().any(|o| o.id == obj.id);
        assert!(
            found,
            "Expected to find object with due_date after 2030-01-01"
        );

        Ok(())
    })
    .await
}

#[tokio::test]
#[test_log::test]
#[serial]
async fn test_filter_date_before() -> TestResult<()> {
    with_test_context(|ctx| async move {
        let (_test_objs, type_key) = create_test_objects(&ctx).await?;
        let past_date = "2025-01-01";
        let obj_name = unique_test_name("Date Before Test");
        let obj = create_object_with_retry("Filter Date Before", || async {
            ctx.client
                .new_object(&ctx.space_id, &type_key)
                .name(&obj_name)
                .body("Test")
                .set_date("due_date", past_date)
                .create()
                .await
        })
        .await?;
        ctx.register_object(&obj.id);

        // Filter for dates before mid-year
        let results = ctx
            .client
            .objects(&ctx.space_id)
            .filter(Filter::date_less("due_date", "2025-06-01"))
            .limit(100)
            .list()
            .await?;

        let found = results.iter().any(|o| o.id == obj.id);
        assert!(
            found,
            "Expected to find object with due_date before 2025-06-01"
        );

        Ok(())
    })
    .await
}

// =============================================================================
// Select Filter Tests
// =============================================================================

#[tokio::test]
#[test_log::test]
#[serial]
async fn test_filter_select_in_single_with_id() -> TestResult<()> {
    with_test_context(|ctx| async move {
        let type_key = ensure_properties_and_type(&ctx).await?;

        let in_progress =
            lookup_property_tag_with_retry(ctx.as_ref(), "status", "In Progress").await?;

        let obj_name = unique_test_name("Select In Test");
        let obj = create_object_with_retry("Filter Select In", || async {
            ctx.client
                .new_object(&ctx.space_id, &type_key)
                .name(&obj_name)
                .body("Test")
                .set_select("status", &in_progress.id)
                .create()
                .await
        })
        .await?;
        ctx.register_object(&obj.id);

        // Filter for status in_progress
        let results = ctx
            .client
            .objects(&ctx.space_id)
            .filter(Filter::select_in(
                "status",
                //vec!["in_progress"],
                vec![in_progress.id],
            ))
            .limit(100)
            .list()
            .await?;

        let found = results.iter().any(|o| o.id == obj.id);
        assert!(found, "Expected to find object with status='in_progress'");

        Ok(())
    })
    .await
}

#[tokio::test]
#[test_log::test]
#[serial]
async fn test_filter_select_in_multiple() -> TestResult<()> {
    with_test_context(|ctx| async move {
        let type_key = ensure_properties_and_type(&ctx).await?;

        let in_progress =
            lookup_property_tag_with_retry(ctx.as_ref(), "status", "In Progress").await?;
        let done = lookup_property_tag_with_retry(ctx.as_ref(), "status", "Done").await?;

        // this object is in_progress
        let obj1_name = unique_test_name("Select Multi 1");
        let obj1 = create_object_with_retry("Filter Select Multi 1", || async {
            ctx.client
                .new_object(&ctx.space_id, &type_key)
                .name(&obj1_name)
                .body("Test")
                .set_select("status", &in_progress.id)
                .create()
                .await
        })
        .await?;
        ctx.register_object(&obj1.id);

        // this object has status=done
        let obj2_name = unique_test_name("Select Multi 2");
        let obj2 = create_object_with_retry("Filter Select Multi 2", || async {
            ctx.client
                .new_object(&ctx.space_id, &type_key)
                .name(&obj2_name)
                .body("Test")
                .set_select("status", &done.id)
                .create()
                .await
        })
        .await?;
        ctx.register_object(&obj2.id);

        // Filter for status in ["open", "in_progress"]
        let results = ctx
            .client
            .objects(&ctx.space_id)
            .filter(Filter::select_in(
                "status",
                vec![&in_progress.id, &done.id], // in_progress_tag.id, done_tag.id],
            ))
            .limit(100)
            .list()
            .await?;

        let found_obj1 = results.iter().any(|o| o.id == obj1.id);
        let found_obj2 = results.iter().any(|o| o.id == obj2.id);

        assert!(
            found_obj1 && found_obj2,
            "Expected to find both objects with status in [open, in_progress]"
        );

        Ok(())
    })
    .await
}

#[tokio::test]
#[test_log::test]
#[serial]
async fn test_filter_select_not_in() -> TestResult<()> {
    with_test_context(|ctx| async move {
        let type_key = ensure_properties_and_type(&ctx).await?;

        let in_progress =
            lookup_property_tag_with_retry(ctx.as_ref(), "status", "In Progress").await?;
        let to_do = lookup_property_tag_with_retry(ctx.as_ref(), "status", "To Do").await?;
        let done = lookup_property_tag_with_retry(ctx.as_ref(), "status", "Done").await?;

        let obj_name = unique_test_name("Select Not In Test");
        let obj = create_object_with_retry("Filter Select Not In", || async {
            ctx.client
                .new_object(&ctx.space_id, &type_key)
                .name(&obj_name)
                .body("Test")
                .set_select("status", &done.id)
                .create()
                .await
        })
        .await?;
        ctx.register_object(&obj.id);

        // Filter for status not in ["todo", "in_progress"]
        let results = ctx
            .client
            .objects(&ctx.space_id)
            .filter(Filter::select_not_in(
                "status",
                vec![&in_progress.id, &to_do.id],
            ))
            .limit(100)
            .list()
            .await?;

        let found = results.iter().any(|o| o.id == obj.id);
        assert!(
            found,
            "Expected to find object with status not in [open, in_progress]"
        );

        Ok(())
    })
    .await
}

// =============================================================================
// FilterExpression Composition Tests
// =============================================================================

#[tokio::test]
#[test_log::test]
#[ignore = "legacy shared-space case; covered by the cleanup-owned condition matrix"]
#[serial]
async fn test_filter_expression_and() -> TestResult<()> {
    with_test_context(|ctx| async move {
        let type_key = ensure_properties_and_type(&ctx).await?;

        // create the object
        let new_obj = create_object_with_retry("Filter Expression And", || async {
            ctx.client
                .new_object(&ctx.space_id, &type_key)
                .set_number("priority", 3)
                .set_checkbox("done", true)
                .create()
                .await
        })
        .await?;
        ctx.register_object(&new_obj.id);

        // Combine filters: priority < 5 AND done = true
        let results = ctx
            .client
            .objects(&ctx.space_id)
            .filter(Filter::number_less("priority", 5))
            .filter(Filter::checkbox_true("done"))
            .limit(100)
            .list()
            .await?;

        assert!(!results.is_empty(), "filter should find a match");
        assert!(results.iter().any(|obj| obj.id == new_obj.id));

        Ok(())
    })
    .await
}

#[tokio::test]
#[test_log::test]
#[serial]
async fn test_filter_expression_empty() -> TestResult<()> {
    with_test_context(|ctx| async move {
        let object_name = unique_test_name("Empty Filter Fixture");
        let object = create_object_with_retry("empty filter fixture", || async {
            ctx.client
                .new_object(&ctx.space_id, "page")
                .name(&object_name)
                .create()
                .await
        })
        .await?;
        ctx.register_object(&object.id);

        // Empty filters should retain the exact fixture object.
        let results = ctx.client.objects(&ctx.space_id).limit(10).list().await?;

        assert!(
            results.iter().any(|result| result.id == object.id),
            "unfiltered list should contain the cleanup-owned fixture"
        );

        Ok(())
    })
    .await
}

// =============================================================================
// Filter on Objects List Tests
// =============================================================================

#[tokio::test]
#[test_log::test]
#[serial]
async fn test_objects_list_with_filter() -> TestResult<()> {
    with_test_context(|ctx| async move {
        let obj_name = unique_test_name("List With Filter");
        let obj = create_object_with_retry("Filter List With Filter", || async {
            ctx.client
                .new_object(&ctx.space_id, "page")
                .name(&obj_name)
                .body("Test")
                .create()
                .await
        })
        .await?;
        ctx.register_object(&obj.id);

        // Apply filter to list
        let results = ctx
            .client
            .objects(&ctx.space_id)
            .filter(Filter::not_empty("name"))
            .limit(100)
            .list()
            .await?;

        let found = results.iter().any(|o| o.id == obj.id);
        assert!(found, "Expected to find object in filtered list");

        Ok(())
    })
    .await
}

#[tokio::test]
#[test_log::test]
#[serial]
async fn test_objects_list_multiple_filters() -> TestResult<()> {
    with_test_context(|ctx| async move {
        let obj_name = unique_test_name("Multiple Filters");
        let obj = create_object_with_retry("Filter Multiple Filters", || async {
            ctx.client
                .new_object(&ctx.space_id, "page")
                .name(&obj_name)
                .body("Test body content")
                .set_text("description", "Has description")
                .create()
                .await
        })
        .await?;
        ctx.register_object(&obj.id);

        // Apply multiple filters
        let results = ctx
            .client
            .objects(&ctx.space_id)
            .filter(Filter::not_empty("name"))
            .filter(Filter::not_empty("description"))
            .limit(100)
            .list()
            .await?;

        let found = results.iter().any(|o| o.id == obj.id);
        assert!(found, "Expected to find object matching multiple filters");

        Ok(())
    })
    .await
}

#[tokio::test]
#[test_log::test]
#[serial]
async fn test_objects_list_filter_with_sort() -> TestResult<()> {
    with_test_context(|ctx| async move {
        let type_key = ensure_properties_and_type(&ctx).await?;
        // Create two objects with different priorities
        let obj1_name = unique_test_name("Sort Test 1");
        let obj1 = create_object_with_retry("Filter Sort 1", || async {
            ctx.client
                .new_object(&ctx.space_id, &type_key)
                .name(&obj1_name)
                .body("Test")
                .set_number("priority", 10)
                .create()
                .await
        })
        .await?;
        ctx.register_object(&obj1.id);

        let obj2_name = unique_test_name("Sort Test 2");
        let obj2 = create_object_with_retry("Filter Sort 2", || async {
            ctx.client
                .new_object(&ctx.space_id, &type_key)
                .name(&obj2_name)
                .body("Test")
                .set_number("priority", 5)
                .create()
                .await
        })
        .await?;
        ctx.register_object(&obj2.id);

        // Filter
        let results = ctx
            .client
            .objects(&ctx.space_id)
            .filter(Filter::not_empty("priority"))
            .limit(100)
            .list()
            .await?;

        // Verify both objects are in results
        let found_obj1 = results.iter().any(|o| o.id == obj1.id);
        let found_obj2 = results.iter().any(|o| o.id == obj2.id);

        assert!(
            found_obj1 && found_obj2,
            "Expected both objects in filtered and sorted results"
        );

        Ok(())
    })
    .await
}

#[tokio::test]
#[test_log::test]
#[serial]
async fn test_objects_list_filter_with_pagination() -> TestResult<()> {
    with_test_context(|ctx| async move {
        // Create multiple objects
        for i in 1..=5 {
            let label = format!("Filter Pagination {i}");
            let obj_name = unique_test_name(&format!("Pagination Test {i}"));
            let obj = create_object_with_retry(&label, || async {
                ctx.client
                    .new_object(&ctx.space_id, "page")
                    .name(&obj_name)
                    .body("Test")
                    .create()
                    .await
            })
            .await?;
            ctx.register_object(&obj.id);
        }

        // Get first page
        let page1 = ctx
            .client
            .objects(&ctx.space_id)
            .filter(Filter::not_empty("name"))
            .limit(3)
            .offset(0)
            .list()
            .await?;

        // Get second page
        let page2 = ctx
            .client
            .objects(&ctx.space_id)
            .filter(Filter::not_empty("name"))
            .limit(3)
            .offset(3)
            .list()
            .await?;

        assert!(
            page1.len() <= 3 && page2.len() <= 3,
            "Expected pagination to respect limit"
        );

        Ok(())
    })
    .await
}

// =============================================================================
// Text Filter Variants
// =============================================================================

#[tokio::test]
#[test_log::test]
#[serial]
async fn test_filter_text_equal() -> TestResult<()> {
    with_test_context(|ctx| async move {
        let unique_name = unique_test_name("Unique Text Equal");
        let obj = create_object_with_retry("Filter Text Equal", || async {
            ctx.client
                .new_object(&ctx.space_id, "page")
                .name(&unique_name)
                .body("Test body")
                .create()
                .await
        })
        .await?;
        ctx.register_object(&obj.id);

        // Filter for exact name match
        let results = ctx
            .client
            .objects(&ctx.space_id)
            .filter(Filter::text_equal("name", &unique_name))
            .limit(10)
            .list()
            .await?;

        let found = results.iter().any(|o| o.id == obj.id);
        assert!(found, "Expected to find object with exact name match");

        Ok(())
    })
    .await
}

#[tokio::test]
#[test_log::test]
#[serial]
async fn test_filter_text_not_equal() -> TestResult<()> {
    with_test_context(|ctx| async move {
        let obj_name = unique_test_name("Text Not Equal");
        let obj = create_object_with_retry("Filter Text Not Equal", || async {
            ctx.client
                .new_object(&ctx.space_id, "page")
                .name(&obj_name)
                .body("Test")
                .create()
                .await
        })
        .await?;
        ctx.register_object(&obj.id);

        // Filter for objects where name is not equal to a different value
        let results = ctx
            .client
            .objects(&ctx.space_id)
            .filter(Filter::text_not_equal("name", "NonExistentName"))
            .limit(10)
            .list()
            .await?;

        let found = results.iter().any(|o| o.id == obj.id);
        assert!(
            found,
            "Expected to find object with name not equal to 'NonExistentName'"
        );

        Ok(())
    })
    .await
}

#[tokio::test]
#[test_log::test]
#[serial]
async fn test_filter_text_contains() -> TestResult<()> {
    with_test_context(|ctx| async move {
        let obj_name = unique_test_name("Normal Name Contains");
        let obj = create_object_with_retry("Filter Text Contains", || async {
            ctx.client
                .new_object(&ctx.space_id, "page")
                .name(&obj_name)
                .body("Test")
                .create()
                .await
        })
        .await?;
        ctx.register_object(&obj.id);

        // Filter for name not containing substring
        let results = ctx
            .client
            .objects(&ctx.space_id)
            .filter(Filter::text_contains("name", "name contains"))
            .limit(10)
            .list()
            .await?;

        let found = results.iter().any(|o| o.id == obj.id);
        assert!(found, "Expected to find object with name like 'normal'");

        Ok(())
    })
    .await
}

#[tokio::test]
#[test_log::test]
#[serial]
async fn test_filter_text_not_contains() -> TestResult<()> {
    with_test_context(|ctx| async move {
        let obj_name = unique_test_name("Normal Name Not Contains");
        let obj = create_object_with_retry("Filter Text Not Contains", || async {
            ctx.client
                .new_object(&ctx.space_id, "page")
                .name(&obj_name)
                .body("Test")
                .create()
                .await
        })
        .await?;
        ctx.register_object(&obj.id);

        // Filter for name not containing substring
        let results = ctx
            .client
            .objects(&ctx.space_id)
            .filter(Filter::text_not_contains("name", "abnormal"))
            .limit(10)
            .list()
            .await?;

        let found = results.iter().any(|o| o.id == obj.id);
        assert!(
            found,
            "Expected to find object with name not like 'abnormal'"
        );

        Ok(())
    })
    .await
}

// =============================================================================
// Type Filter Tests
// =============================================================================

#[tokio::test]
#[test_log::test]
#[serial]
async fn test_filter_type_in() -> TestResult<()> {
    with_test_context(|ctx| async move {
        let obj_name = unique_test_name("Type Test Page");
        let page_obj = create_object_with_retry("Filter Type In", || async {
            ctx.client
                .new_object(&ctx.space_id, "page")
                .name(&obj_name)
                .body("Test")
                .create()
                .await
        })
        .await?;
        ctx.register_object(&page_obj.id);

        let page_type = ctx.client.lookup_type_by_key(&ctx.space_id, "page").await?;

        // Filter for type = page
        let results = ctx
            .client
            .objects(&ctx.space_id)
            .filter(Filter::type_in(vec![&page_type.id]))
            .limit(1)
            .list()
            .await?;

        let found = results.iter().any(|o| o.id == page_obj.id);
        assert!(found, "Expected to find page object when filtering by type");

        Ok(())
    })
    .await
}
