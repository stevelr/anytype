#![cfg(feature = "scripted-http-fixture")]

use std::collections::BTreeSet;

use anytype::{
    prelude::{AnytypeClient, AnytypeError, ClientConfig, HttpCredentials},
    test_util::scripted_http::{
        ScriptedHttpContentType, ScriptedHttpFixture, ScriptedHttpRequest, ScriptedHttpResponse,
    },
};
use reqwest::StatusCode;
use serde_json::{Value, json};

const SPACE_ID: &str = "bafyreie6n5l5nkbjal37su54cha4coy7qzuhrnajluzv5qd5jvtsrxkequ";

fn fixture_client(base_url: String, disable_cache: bool) -> AnytypeClient {
    let mut config = ClientConfig::default().app_name("lookup-helper-fixture");
    config.base_url = Some(base_url);
    config.keystore = Some("env".to_owned());
    config.disable_cache = disable_cache;
    let client = AnytypeClient::with_config(config).expect("create lookup helper fixture client");
    client.set_api_key(HttpCredentials::new("fixture-token"));
    client
}

fn json_response(status: StatusCode, body: Value) -> ScriptedHttpResponse {
    ScriptedHttpResponse::new(
        status,
        ScriptedHttpContentType::Json,
        body.to_string().into_bytes(),
    )
}

fn page(items: Vec<Value>, has_more: bool, offset: u32, total: usize) -> Value {
    json!({
        "data": items,
        "pagination": {
            "has_more": has_more,
            "limit": 100,
            "offset": offset,
            "total": total,
        },
    })
}

fn space(id: &str, name: &str) -> Value {
    json!({ "id": id, "name": name, "object": "space" })
}

fn type_(id: &str, key: &str, name: &str) -> Value {
    json!({
        "id": id,
        "key": key,
        "name": name,
        "archived": false,
    })
}

fn property(id: &str, key: &str, name: &str) -> Value {
    json!({ "id": id, "key": key, "name": name, "format": "text" })
}

fn assert_paths(requests: &[ScriptedHttpRequest], expected: &[&str]) {
    assert_eq!(requests.len(), expected.len());
    for (request, expected_path) in requests.iter().zip(expected) {
        assert_eq!(request.method(), "GET");
        assert_eq!(request.path(), *expected_path);
        assert!(request.body().is_empty());
    }
}

fn assert_not_found(error: AnytypeError, object_type: &str, key: &str) {
    assert!(matches!(
        error,
        AnytypeError::NotFound { obj_type, key: error_key }
            if obj_type == object_type && error_key == key
    ));
}

#[tokio::test]
async fn lookup_space_by_name_pages_for_cache_enabled_and_disabled_clients() {
    for disable_cache in [false, true] {
        let fixture = ScriptedHttpFixture::start(vec![
            json_response(
                StatusCode::OK,
                page(vec![space("space-other", "Other")], true, 0, 101),
            ),
            json_response(
                StatusCode::OK,
                page(vec![space(SPACE_ID, "Target")], false, 100, 101),
            ),
        ])
        .await
        .expect("start space lookup fixture");
        let client = fixture_client(fixture.base_url(), disable_cache);

        let found = client
            .lookup_space_by_name("Target")
            .await
            .expect("target space is on the continuation page");

        assert_eq!(found.id, SPACE_ID);
        assert_eq!(found.name, "Target");
        let requests = fixture.finish().await.expect("finish space lookup fixture");
        assert_paths(
            &requests,
            &["/v1/spaces", "/v1/spaces?limit=100&offset=100"],
        );
    }
}

#[tokio::test]
async fn lookup_space_by_name_returns_not_found_and_propagates_continuation_failure() {
    let fixture = ScriptedHttpFixture::start(vec![json_response(
        StatusCode::OK,
        page(vec![space(SPACE_ID, "Other")], false, 0, 1),
    )])
    .await
    .expect("start not-found fixture");
    let client = fixture_client(fixture.base_url(), false);
    let error = client
        .lookup_space_by_name("Missing")
        .await
        .expect_err("missing space is classified without an upstream payload");
    assert_not_found(error, "Space", "Missing");
    assert_paths(
        &fixture.finish().await.expect("finish not-found fixture"),
        &["/v1/spaces"],
    );

    for disable_cache in [false, true] {
        let fixture = ScriptedHttpFixture::start(vec![
            json_response(
                StatusCode::OK,
                page(vec![space("space-other", "Other")], true, 0, 101),
            ),
            json_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                json!({ "error": "private" }),
            ),
        ])
        .await
        .expect("start partial-failure fixture");
        let client = fixture_client(fixture.base_url(), disable_cache);
        let error = client
            .lookup_space_by_name("Target")
            .await
            .expect_err("a continuation failure does not yield a partial lookup result");
        assert!(matches!(error, AnytypeError::ApiError { code: 500, .. }));
        assert!(!error.to_string().contains("private"));
        assert!(!format!("{error:?}").contains("private"));
        assert_eq!(client.cache().num_spaces(), 0);
        assert_paths(
            &fixture
                .finish()
                .await
                .expect("finish partial-failure fixture"),
            &["/v1/spaces", "/v1/spaces?limit=100&offset=100"],
        );
    }
}

#[tokio::test]
async fn lookup_types_pages_deduplicates_and_preserves_ambiguous_matches() {
    let fixture = ScriptedHttpFixture::start(vec![
        json_response(
            StatusCode::OK,
            page(vec![type_("type-alpha", "alpha", "Shared")], true, 0, 102),
        ),
        json_response(
            StatusCode::OK,
            page(
                vec![
                    type_("type-alpha", "alpha", "Shared"),
                    type_("type-beta", "beta", "Shared"),
                ],
                false,
                100,
                102,
            ),
        ),
    ])
    .await
    .expect("start type lookup fixture");
    let client = fixture_client(fixture.base_url(), false);

    let matches = client
        .lookup_types(SPACE_ID, "shared")
        .await
        .expect("ambiguous type names are returned as all unique matches");
    assert_eq!(matches.len(), 2);
    let ids = matches
        .into_iter()
        .map(|type_| type_.id)
        .collect::<BTreeSet<_>>();
    assert_eq!(
        ids,
        BTreeSet::from(["type-alpha".to_owned(), "type-beta".to_owned()])
    );
    let exact = client
        .lookup_types(SPACE_ID, "alpha")
        .await
        .expect("type key is an exact lookup after warming the cache");
    assert_eq!(exact.len(), 1);
    assert_eq!(exact[0].id, "type-alpha");
    assert_paths(
        &fixture.finish().await.expect("finish type lookup fixture"),
        &[
            &format!("/v1/spaces/{SPACE_ID}/types"),
            &format!("/v1/spaces/{SPACE_ID}/types?limit=100&offset=100"),
        ],
    );
}

#[tokio::test]
async fn lookup_types_classifies_not_found_and_disabled_cache() {
    let fixture = ScriptedHttpFixture::start(vec![json_response(
        StatusCode::OK,
        page(vec![type_("type-other", "other", "Other")], false, 0, 1),
    )])
    .await
    .expect("start type not-found fixture");
    let client = fixture_client(fixture.base_url(), false);
    let error = client
        .lookup_types(SPACE_ID, "missing")
        .await
        .expect_err("missing type is classified without an upstream payload");
    assert_not_found(error, "Type", "missing");
    assert_paths(
        &fixture
            .finish()
            .await
            .expect("finish type not-found fixture"),
        &[&format!("/v1/spaces/{SPACE_ID}/types")],
    );

    let client = fixture_client("http://127.0.0.1:1".to_owned(), true);
    let error = client
        .lookup_types(SPACE_ID, "missing")
        .await
        .expect_err("type lookup requires an enabled cache before HTTP");
    assert!(matches!(error, AnytypeError::CacheDisabled));
}

#[tokio::test]
async fn lookup_properties_pages_deduplicates_and_preserves_ambiguous_matches() {
    let fixture = ScriptedHttpFixture::start(vec![
        json_response(
            StatusCode::OK,
            page(
                vec![property("property-alpha", "alpha", "Shared")],
                true,
                0,
                102,
            ),
        ),
        json_response(
            StatusCode::OK,
            page(
                vec![
                    property("property-alpha", "alpha", "Shared"),
                    property("property-beta", "beta", "Shared"),
                ],
                false,
                100,
                102,
            ),
        ),
    ])
    .await
    .expect("start property lookup fixture");
    let client = fixture_client(fixture.base_url(), false);

    let matches = client
        .lookup_properties(SPACE_ID, "shared")
        .await
        .expect("ambiguous property names are returned as all unique matches");
    assert_eq!(matches.len(), 2);
    let ids = matches
        .into_iter()
        .map(|property| property.id)
        .collect::<BTreeSet<_>>();
    assert_eq!(
        ids,
        BTreeSet::from(["property-alpha".to_owned(), "property-beta".to_owned()])
    );
    let exact = client
        .lookup_properties(SPACE_ID, "alpha")
        .await
        .expect("property key is an exact lookup after warming the cache");
    assert_eq!(exact.len(), 1);
    assert_eq!(exact[0].id, "property-alpha");
    assert_paths(
        &fixture
            .finish()
            .await
            .expect("finish property lookup fixture"),
        &[
            &format!("/v1/spaces/{SPACE_ID}/properties"),
            &format!("/v1/spaces/{SPACE_ID}/properties?limit=100&offset=100"),
        ],
    );
}

#[tokio::test]
async fn lookup_properties_classifies_not_found_and_disabled_cache() {
    let fixture = ScriptedHttpFixture::start(vec![json_response(
        StatusCode::OK,
        page(
            vec![property("property-other", "other", "Other")],
            false,
            0,
            1,
        ),
    )])
    .await
    .expect("start property not-found fixture");
    let client = fixture_client(fixture.base_url(), false);
    let error = client
        .lookup_properties(SPACE_ID, "missing")
        .await
        .expect_err("missing property is classified without an upstream payload");
    assert_not_found(error, "Property", "missing");
    assert_paths(
        &fixture
            .finish()
            .await
            .expect("finish property not-found fixture"),
        &[&format!("/v1/spaces/{SPACE_ID}/properties")],
    );

    let client = fixture_client("http://127.0.0.1:1".to_owned(), true);
    let error = client
        .lookup_properties(SPACE_ID, "missing")
        .await
        .expect_err("property lookup requires an enabled cache before HTTP");
    assert!(matches!(error, AnytypeError::CacheDisabled));
}

#[tokio::test]
async fn lookup_space_by_name_reports_a_malformed_continuation_without_leaking_its_body() {
    let fixture = ScriptedHttpFixture::start(vec![
        json_response(
            StatusCode::OK,
            page(vec![space("space-other", "Other")], true, 0, 101),
        ),
        ScriptedHttpResponse::new(
            StatusCode::OK,
            ScriptedHttpContentType::Json,
            b"{malformed-continuation".to_vec(),
        ),
    ])
    .await
    .expect("start malformed continuation fixture");
    let client = fixture_client(fixture.base_url(), true);
    let error = client
        .lookup_space_by_name("Target")
        .await
        .expect_err("malformed continuation is not treated as not-found");
    assert!(matches!(error, AnytypeError::Deserialization { .. }));
    assert!(!error.to_string().contains("malformed-continuation"));
    assert_paths(
        &fixture
            .finish()
            .await
            .expect("finish malformed continuation fixture"),
        &["/v1/spaces", "/v1/spaces?limit=100&offset=100"],
    );
}
