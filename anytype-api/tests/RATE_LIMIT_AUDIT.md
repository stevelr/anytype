# Live-test mutation rate-limit audit

Live HTTP `POST` and `PATCH` setup operations use the test utility
`anytype::test_util::retry_definitive_rate_limit` (re-exported by the
integration-test `common` module). The seam retries only the typed
`AnytypeError::ApiError { code: 429, .. }` rejection, at most four times after
the initial attempt, with bounded delays of 200, 400, 800, and 1,500 ms. Every
other failure is returned immediately because transport errors, timeouts, 5xx
responses, and malformed responses do not prove whether a mutation applied.

The inventory is locked by
`test_retry_helpers::live_mutation_retry_inventory_is_current`. It counts all
live-test terminal `.create()`, `.update()`, `.send()`, and `.upload()` calls,
then reconciles every call to the two retry entry points with the deliberately
single-attempt remainder. A new or removed mutation therefore requires an
explicit audit update.

## Retried setup inventory

- The shared object-create helper and common filter property/type fixture.
- Shared `TestContext` collection, space, template-type, and template-source
  REST fixtures used by MCP live tests. Their gRPC fixture writes remain
  single-attempt asserted behavior.
- Search-index objects used only by subsequent search assertions.
- Custom property, object-link target, and getter fixtures used by property
  format tests.
- Properties and tag options used only by tag list/get/update/delete,
  pagination, and object-value assertions.
- Chat container objects used by discovery, message, and stream tests, plus
  the initial REST SSE message.
- Collection/list objects and collection members used by view assertions.
- Types created only to set up type update/delete/duplicate-key assertions.
- The validation setup objects migrated by `any-ae5`.

| Test source | Generic | Object create | Object update | Excluded direct |
| --- | ---: | ---: | ---: | ---: |
| `common/mod.rs` | 2 | 0 | 0 | 0 |
| `../src/test_util.rs` | 4 | 0 | 0 | 0 |
| `integration.rs` | 1 | 0 | 0 | 8 |
| `smoke_test.rs` | 0 | 0 | 0 | 2 |
| `test_chat_discovery.rs` | 2 | 0 | 0 | 1 |
| `test_chat_stream.rs` | 3 | 0 | 0 | 3 |
| `test_chats.rs` | 2 | 0 | 0 | 4 |
| `test_files.rs` | 0 | 0 | 0 | 1 |
| `test_filters.rs` | 0 | 24 | 0 | 0 |
| `test_properties.rs` | 14 | 10 | 0 | 10 |
| `test_search.rs` | 7 | 0 | 0 | 1 |
| `test_tags.rs` | 32 | 3 | 0 | 6 |
| `test_types.rs` | 4 | 1 | 0 | 10 |
| `test_validation.rs` | 0 | 4 | 0 | 12 |
| `test_views.rs` | 2 | 0 | 0 | 0 |
| Sources without terminal mutations | 0 | 0 | 0 | 0 |
| **Total** | **73** | **42** | **0** | **58** |

The 115 wrapped calls above are the complete successful fixture-setup
inventory. The 58 direct terminal calls are deliberately excluded. The
machine check also covers `test_cache.rs`, `test_members.rs`, and
`test_pagination.rs`, which currently contain no terminal mutations.

Names, keys, bodies, property values, tag values, and IDs are computed before
the retry closure or rebuilt from the same immutable inputs. Cleanup resources
are registered immediately after the first successful response, or at the
earliest safe point after validating an untrusted returned identity in the
ownership-proving shared fixtures, before any fallible follow-up.

## Deliberate exclusions

- Invalid-input, authentication, permission, duplicate/optional-success, and
  other failure-classification mutations remain single-attempt so retrying
  cannot change the behavior being asserted.
- Object, property, type, tag, chat-message, file-upload, and smoke CRUD calls
  that are themselves the operation under test remain single-attempt.
- View add/remove, chat reaction/read/delete, and gRPC-only mutation behavior
  remains unchanged because those calls are the asserted contract or are not
  affected by the HTTP 429 path.
- HTTP `DELETE` setup/cleanup is not wrapped: the production client already
  treats DELETE as replay-safe and applies its bounded rate-limit policy.

After these exclusions, there are no residual direct successful HTTP
`POST`/`PATCH` calls whose sole purpose is fixture setup. Direct mutations that
remain in live tests are the asserted operation, an explicitly optional
operation, a validation/failure case, cleanup, or a gRPC-only path.
