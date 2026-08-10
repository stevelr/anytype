# anytype-api HTTP/gRPC test coverage

Current status document for test coverage of the `anytype` crate's public
network-facing surface. Originated as the fixed-scope audit for any-gc5
(parent any-q1n, review gate any-jzn) dated 2026-07-21; now maintained as a
living inventory. Status as of 2026-08-10, reconciled against the crate
CHANGELOG `[Unreleased]` section, the completed any-dm9k campaign, and the
direct gap-coverage work recorded below.

## Scope and method

Inventory covers every public network-facing operation exposed by the `anytype`
crate (`anytype-api/`): fluent-builder REST endpoints, the selected gRPC
extensions, and the public cross-cutting helpers that wrap them (resolve,
verify, pagination, process watching, chat streaming, body reads/mutations,
attached discussions, collection membership). Pure model/builder setters,
accessors, and internal plumbing are excluded except where they are the only
test surface for an operation.

Sources: `rg` over `pub async fn`/`impl AnytypeClient` in
`anytype-api/src/*.rs`; per-module `#[cfg(test)]` test lists; test function
inventory of `anytype-api/tests/*.rs`; `docs/http-grpc-overlap.md` for the
REST/gRPC split; crate CHANGELOG `[Unreleased]`.

Coverage classes:

- **unit** — offline tests inside `src/` modules, backed by scripted HTTP
  fixture servers or constructed transport-independent values. Run in tier 1
  (`cargo test -p anytype --lib`). The custom semantic gRPC mock has been
  removed; gRPC behavior is covered by constructed reducer values plus
  real-server tests.
- **live** — `tests/` integration tests requiring a running Anytype server
  (`.test-env`, default `127.0.0.1:31012`). Tier 2, serial only. Shared
  contexts now REQUIRE `ANYTYPE_TEST_SPACE_PREFIX`, create a fresh uniquely
  named space per test, and delete it on the way out; ambient space-ID
  variables are no longer consulted. Fixture-heavy suites (body, chats,
  discussions, space administration, Markdown fidelity, Kanban, compatibility
  matrices) run in the ignored disposable tier via
  `with_disposable_space_context`. The closed manifest and protected serial
  gate delivered by any-dm9k.7 automate that tier.
- Status: **covered** (meaningful direct assertions), **partial** (only one
  path, only error paths, or only indirect exercise), **uncovered** (no test
  invokes it).

## Test asset inventory

| Asset                                                                                                                                                                                                                                                                        | Kind    | What it covers                                                                                                                                                                                                                                                                                                           |
| ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `src/http_client.rs` (56) and `src/http_timeout.rs` (6)                                                                                                                                                                                                                      | unit    | absolute request and SSE phase deadlines, per-page deadline freshness, policy resolution and pinned defaults, retry/replay safety for every mutation method, pre-dispatch connect classification, response ceilings, redirect policy, diagnostics redaction, timeout metrics, Retry-After parsing, credential generation |
| `src/resolve.rs` tests (33)                                                                                                                                                                                                                                                  | unit    | all resolver classes: bounded scans, ambiguity candidates, dedup, direct-ID fast paths, chat/space/view/template resolution over paged fixtures                                                                                                                                                                          |
| `src/chats.rs` tests (32)                                                                                                                                                                                                                                                    | unit    | REST chat wire shapes, SSE stream robustness/bounds/buffer ceilings, gRPC block round-trips, route paths, timestamp conversion, before-anchor history paging, verified-edit readback                                                                                                                                     |
| `src/body.rs` (33), `src/body_mutation.rs` (28), `src/body_rpc.rs` (12)                                                                                                                                                                                                      | unit    | typed body-graph validation (identities, order, closed enums, fail-closed malformed/oversized graphs), verified block mutation state machines and constructors, finite show/close RPC seam (deadlines, decoder limits, payload-free counters)                                                                            |
| `src/attached_discussions.rs` tests (19)                                                                                                                                                                                                                                     | unit    | discussion discovery/ensure lifecycle and state machines, closed payload-free error kinds, reconciliation outcomes                                                                                                                                                                                                       |
| `src/files.rs` (20), `src/properties.rs` (20), `src/types.rs` (19), `src/spaces.rs` (24), `src/views.rs` (20), `src/objects.rs` (7), `src/members.rs` (5)                                                                                                                    | unit    | request-body serialization, model enums, direct-get scoping, tag-lookup budgets, upload backend selection and multipart byte ceilings, ranged/conditional downloads, bounded archive counts and metadata validation, collection-membership seams, view path validation                                                   |
| `src/auth.rs` (9), `src/verify.rs` (11), `src/paged.rs` (12), `src/cache.rs` (3), `src/keystore.rs` (4), `src/error.rs` (3), `src/validation.rs` (4), `src/client.rs` (5), `src/chat_stream.rs` (1), `src/process_watcher.rs` (4), `src/search.rs` (4), `src/filters.rs` (4) | unit    | authentication wire/state/error paths, verification retry semantics, pagination, cache and keystore operations, error redaction/classification, port discovery, stream routing, import-finish correlation, and search/filter serialization                                                                               |
| `src/test_util.rs` + `src/test_util/` + `tests/common/`                                                                                                                                                                                                                      | harness | disposable per-test space contexts with recovery ledger and sweeps, immediate `register_*` cleanup, retry helpers, template/collection-layout/second-view/Kanban/saved-view-filter fixtures, archive-evidence scans                                                                                                      |
| `tests/` (27 integration targets)                                                                                                                                                                                                                                            | live    | see matrix; includes filters, search, types, validation, integration, properties, tags, members, cache, body and body-mutation, attached-discussion, space-administration, chat-prerequisite, Kanban, Markdown-fidelity, and protected-manifest coverage                                                                 |

## Coverage matrix

### Auth and client lifecycle (REST unless noted)

| Operation                                                           | Unit                                               | Live                                   | Status                            |
| ------------------------------------------------------------------- | -------------------------------------------------- | -------------------------------------- | --------------------------------- |
| `create_auth_challenge`                                             | scripted exact wire and API errors                 | –                                      | covered                           |
| `create_api_key`                                                    | scripted exact wire and API errors                 | –                                      | covered                           |
| `authenticate_interactive`                                          | fast, forced, callback, API, and persistence paths | –                                      | covered                           |
| `auth_status` / `logout`                                            | exact memory/keystore transitions                  | –                                      | covered                           |
| `ping_http` / `ping_grpc`                                           | typed authentication failures                      | every disposable readiness preflight   | covered                           |
| `grpc_client`                                                       | –                                                  | indirect (all gRPC tests)              | partial                           |
| `find_grpc`                                                         | helpers only (`extract_port_*`, lsof filter)       | –                                      | partial                           |
| `http_metrics`                                                      | –                                                  | asserted in `test_cache`, `smoke_test` | partial                           |
| HTTP pipeline (deadlines/retry/limits/redirect/diagnostics/metrics) | 55 tests                                           | `test_retry_helpers` (3 live-relevant) | covered                           |
| cache enable/disable/clear                                          | `cache.rs` (3)                                     | `test_cache` (14)                      | covered                           |
| keystore save/load/update + modifier parsing                        | `keystore.rs` (5)                                  | –                                      | covered (unit is the right level) |

### Spaces

| Operation                                                                           | Transport | Unit                                                                   | Live                                                                                 | Status                                                |
| ----------------------------------------------------------------------------------- | --------- | ---------------------------------------------------------------------- | ------------------------------------------------------------------------------------ | ----------------------------------------------------- |
| `spaces().list`                                                                     | REST      | –                                                                      | `smoke_test`, `test_cache`                                                           | covered                                               |
| `space(id).get`                                                                     | REST      | –                                                                      | `smoke_test`, `test_cache`, `test_validation`                                        | covered                                               |
| `space(id).get_direct` (cache-independent exact GET)                                | REST      | spaces unit tests                                                      | disposable readiness/preflight reads                                                 | covered                                               |
| `new_space(..).create`                                                              | REST      | body serialization + empty-name rejection                              | exercised by every disposable-context test; harness verifies create/readiness/delete | covered — former no-delete blocker resolved           |
| `update_space(..).update`                                                           | REST      | body serialization                                                     | –                                                                                    | partial                                               |
| `lookup_space_by_name`                                                              | REST      | scripted cache modes, pagination, not-found, partial failure           | –                                                                                    | covered                                               |
| `create_chat_space`                                                                 | REST      | validation (`test_space_admin`)                                        | ignored disposable lifecycle (`space_administration_lifecycle`)                      | covered                                               |
| `delete_space` (permanent)                                                          | REST      | validation                                                             | disposable-context teardown on every fixture-heavy test; ignored lifecycle test      | covered                                               |
| space invites: `list_space_invites` / `create_space_invite` / `revoke_space_invite` | REST      | validation                                                             | ignored disposable lifecycle                                                         | covered                                               |
| `enable_space_sharing` / `disable_space_sharing`                                    | REST      | validation                                                             | ignored disposable lifecycle                                                         | covered                                               |
| `backup` (`backup_space`)                                                           | gRPC      | –                                                                      | required anyr/anyback smoke and fidelity gates                                       | covered (cross-crate integration)                     |
| `list_archived(..).list`                                                            | gRPC      | page validation, filter wire, type-ID validation and fail-safe absence | indirect (harness archive-evidence scans)                                            | covered                                               |
| `count_archived` / `count_archived_bounded`                                         | gRPC      | exact boundary, probe, budget, and request-count state machine         | –                                                                                    | covered                                               |
| `delete_archived` / `delete_all_archived`                                           | gRPC      | –                                                                      | `delete_archived` exercised by backup/restore cleanup                                | partial (`delete_all_archived` lacks direct coverage) |

### Objects and attached discussions

| Operation                                                              | Transport | Unit                                               | Live                                                                                       | Status                                      |
| ---------------------------------------------------------------------- | --------- | -------------------------------------------------- | ------------------------------------------------------------------------------------------ | ------------------------------------------- |
| `object(..).get`                                                       | REST      | –                                                  | `integration`, `smoke_test`, many others                                                   | covered                                     |
| `objects(..).list` (+ filters/sort/pagination)                         | REST      | –                                                  | `test_filters`, `integration`; empty-filter and pagination-offset cases own their fixtures | covered                                     |
| `new_object(..).create`                                                | REST      | body serialization                                 | `integration`, `smoke_test`, fixtures everywhere                                           | covered                                     |
| `update_object(..).update`                                             | REST      | body serialization                                 | `integration`, `smoke_test`                                                                | covered                                     |
| `object(..).delete` / `delete_once`                                    | REST      | –                                                  | `integration`, `smoke_test`, harness cleanup                                               | covered                                     |
| `get_share_link`                                                       | gRPC      | missing-credentials classification                 | live call blocked by removed Heart backend                                                 | uncovered — `any-6s3` blocked on `any-x7f0` |
| `set_properties` (typed value coercion)                                | helper    | property-value unit tests                          | `test_properties` set/read matrix (all formats)                                            | covered                                     |
| discussion discovery (`discussion` get)                                | gRPC+REST | `attached_discussions.rs` (19, shared with ensure) | ignored disposable `test_attached_discussions`                                             | covered                                     |
| discussion `ensure` (idempotent, single dispatch, reconciled outcomes) | gRPC+REST | lifecycle/state-machine unit tests                 | ignored disposable exact get/ensure/repeat test                                            | covered                                     |
| `ObjectAddDiscussion` repeat-create upstream defect                    | gRPC      | –                                                  | ignored prefix-authorized probe (2 raw RPCs, evidence-only)                                | covered (defect documentation tier)         |

### Body blocks (gRPC `ObjectShow`/mutation RPCs)

| Operation                                                                                                                          | Unit                     | Live                                                                                                                                  | Status  |
| ---------------------------------------------------------------------------------------------------------------------------------- | ------------------------ | ------------------------------------------------------------------------------------------------------------------------------------- | ------- |
| `blocks()` read → `BodySnapshot`/`BodyBlock` (typed variants, exact IDs/order, closed enums, `Unsupported` markers, `BodyLimits`)  | `body.rs` (33)           | 5 ignored disposable tests: typed-variant/order preservation, tightened limits, missing object, opaque dataview, show/close lifecycle | covered |
| show/close lifecycle (no server-side open state; owned foreground close)                                                           | `body_rpc.rs` seam tests | `test_body_show_close_lifecycle_holds_no_server_open_state` (verified via `DebugOpenedObjects`)                                       | covered |
| `BodySnapshot::edit` verified mutations (create/append/update/delete/reorder, bounded batches, table receipts, constructor matrix) | `body_mutation.rs` (28)  | `test_body_mutations` (2, tier-2 disposable)                                                                                          | covered |
| `BodyRpcConfig` finite RPC seam (shared deadline, decoder limits, payload-free counters)                                           | `body_rpc.rs` (11)       | exercised by all body live suites                                                                                                     | covered |
| `plain_markdown_representation` (closed write/read contract)                                                                       | objects unit tests       | ignored serial disposable Markdown fidelity matrix (byte-identical replay cohorts + documented drift)                                 | covered |

### Types, properties, tags, templates

| Operation                                                                                                      | Unit                                               | Live                                                                                                                            | Status              |
| -------------------------------------------------------------------------------------------------------------- | -------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------- | ------------------- |
| types list/get/create/update/delete                                                                            | update serialization + classification (19)         | `test_types` (24), `test_cache`                                                                                                 | covered             |
| `type(..).get_direct`                                                                                          | resolver unit tests                                | `test_resolve_type_by_id_bypasses_primed_cache`                                                                                 | covered             |
| type-property classification read (featured vs recommended reconciliation, fail-closed cross-transport checks) | classification unit tests                          | disposable real-server coverage; cross-crate ignored `e2e_restore_preserves_custom_schema_keys_formats_and_featured_membership` | covered             |
| `lookup_type_by_key`                                                                                           | resolver unit tests                                | `smoke_test`, `test_filters`                                                                                                    | covered             |
| `lookup_types` (bulk)                                                                                          | scripted cache modes, pagination, dedup, ambiguity | –                                                                                                                               | covered             |
| properties list/get/create/update/delete (incl. `no_cache_refresh` bounded readback)                           | serialization + direct-get (20)                    | `test_properties` (23)                                                                                                          | covered             |
| `property(..).get_direct` + explicit-ID tag lookup                                                             | budget/identity unit tests                         | –                                                                                                                               | covered (unit)      |
| `lookup_property_by_key`                                                                                       | resolver unit tests                                | `tests/common`, `smoke_test`                                                                                                    | covered             |
| `lookup_properties` (bulk)                                                                                     | scripted cache modes, pagination, dedup, ambiguity | –                                                                                                                               | covered             |
| `lookup_property_tag`                                                                                          | tag-budget unit tests                              | `test_tags`, `tests/common`                                                                                                     | covered             |
| tags list/get/create/update/delete                                                                             | –                                                  | `test_tags` (19)                                                                                                                | covered (live only) |
| `template(..).get` / `templates(..).list`                                                                      | resolver template unit tests                       | `test_types` template trio; `create_template_fixtures` harness                                                                  | covered             |

### Views, collections, members, search

| Operation                                                                                                        | Unit                                               | Live                                                                                                         | Status                                            |
| ---------------------------------------------------------------------------------------------------------------- | -------------------------------------------------- | ------------------------------------------------------------------------------------------------------------ | ------------------------------------------------- |
| `list_views(..).list`                                                                                            | path validation                                    | `test_views` (set + collection); second-view test fixture (raw Heart RPC) now exists for multi-view evidence | covered — production view-create API still absent |
| view objects list (selected view)                                                                                | –                                                  | `test_views`, including differential second-view `limit(1)` continuation                                     | covered                                           |
| `view_add_objects` / `view_remove_object`                                                                        | –                                                  | `test_view_add_remove_objects_collection`                                                                    | covered                                           |
| `collection_member_add` (singular, non-replayed, exact status preservation)                                      | views unit tests                                   | membership observation/page suites                                                                           | covered                                           |
| `collection_membership_page` (canonical manual-collection pages, continuation arithmetic, bounded subscriptions) | views unit tests                                   | disposable real-server evidence incl. Kanban fixture                                                         | covered                                           |
| `observe_collection_membership` (present/absent with index controls)                                             | views unit tests                                   | disposable real-server coverage                                                                              | covered                                           |
| Kanban grouping fixture (grouping relation, column movement, two-item pages)                                     | –                                                  | ignored disposable `test_kanban_fixture`                                                                     | covered (harness tier)                            |
| `member(..).get` / `members(..).list`                                                                            | model helpers (5)                                  | `test_members` (16)                                                                                          | covered                                           |
| `search_global().execute` / space search                                                                         | limit plus request/sort/pagination wire shapes (4) | `test_search` (25), `integration`                                                                            | covered                                           |
| filter DSL (`filters.rs`)                                                                                        | nested/default/value encodings and validation (4)  | `test_filters` (24 incl. numeric/checkbox compatibility matrix vs `anytype-heart#2879`)                      | covered                                           |

### Files

| Operation                                                              | Transport | Unit                                                                      | Live                                                                       | Status                               |
| ---------------------------------------------------------------------- | --------- | ------------------------------------------------------------------------- | -------------------------------------------------------------------------- | ------------------------------------ |
| `upload` (path/bytes/reader, plain)                                    | REST      | backend selection, bounded multipart construction, response normalization | `test_files` backend auto-selection incl. reader and rich-option promotion | covered                              |
| `upload` (URL / rich options)                                          | gRPC      | backend-selection only                                                    | bounded loopback URL transfer; protected anyr rich-option upload           | covered                              |
| `download_bytes`                                                       | REST      | –                                                                         | `test_files`; cross-crate ignored exact restored-byte comparison           | covered                              |
| `download_request` (range/width/conditional, header-evidence ceilings) | REST      | ranged + conditional + 304 + ceiling unit tests                           | live `HEAD`/`206`/`412`/`416` + rejected zero-length range                 | covered                              |
| `metadata` / `head`                                                    | REST      | head-without-body + validator parsing                                     | live metadata `HEAD`; cross-crate restored MIME assertion                  | covered                              |
| `delete` / `delete_request(..).permanently`                            | REST      | skip_bin query unit test                                                  | `test_files` incl. permanent delete                                        | covered                              |
| `http_upload` / `http_download` / `http_delete` (legacy REST)          | REST      | delegates to covered modern builders                                      | –                                                                          | covered through delegated operations |
| `list` / `search` / `get` (rich metadata)                              | gRPC      | –                                                                         | protected anyr file-operation gate                                         | covered (cross-crate integration)    |
| `preload` / `discard_preload`                                          | gRPC      | –                                                                         | protected anyr file-operation gate                                         | covered (cross-crate integration)    |
| `download` (legacy, Heart writes to path)                              | gRPC      | –                                                                         | owned-path exact-byte transfer and cleanup                                 | covered                              |

### Chats

| Operation                                                                                                                                     | Transport | Unit                                          | Live                                                                                                                                                                              | Status                                                                                                                                                           |
| --------------------------------------------------------------------------------------------------------------------------------------------- | --------- | --------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `chats().in_space(..)`: chat list/create; message add/edit/get/list/search/delete; reactions; read state                                      | REST      | wire-shape tests (add/edit/list/search paths) | disposable resolver-supporting reads in `test_chat_discovery`; CRUD/search/reaction/read-state cases in `test_chats`; cross-crate ignored restore order/reply/attachment fidelity | covered                                                                                                                                                          |
| MCP prerequisites: fallible UTC-ms timestamp conversion, before-anchor history pages, verified text/format edits with advancing `modified_at` | REST      | scripted transport tests                      | ignored disposable `test_chat_prerequisites`                                                                                                                                      | covered                                                                                                                                                          |
| REST SSE `message_stream(..).open` (incl. per-event buffer ceilings)                                                                          | REST      | SSE bound/robustness suite                    | disposable `test_chat_stream::rest_chat_stream_receives_initial_message`                                                                                                          | covered                                                                                                                                                          |
| `add_message`/`edit_message`/`delete_message`/`get_messages`/`list_messages`/`read_messages`/`unread_messages`                                | gRPC      | block round-trip + rich-state conversion      | `test_chats::test_chat_message_crud`                                                                                                                                              | covered                                                                                                                                                          |
| `send_text` / `toggle_reaction` / `read_all`                                                                                                  | gRPC      | –                                             | `test_chats` / `test_chat_discovery` (single paths)                                                                                                                               | partial                                                                                                                                                          |
| `edit_text`                                                                                                                                   | gRPC      | –                                             | disposable direct edit with independent REST text/style/mark readback                                                                                                             | covered                                                                                                                                                          |
| `search_chats_in` / `get_chat` / `resolve_chat_by_name`                                                                                       | gRPC      | resolver chat-discovery unit tests            | disposable `test_chat_discovery`                                                                                                                                                  | covered                                                                                                                                                          |
| `list_chats` / `list_chats_in` (global; `list_chats_in` now REST)                                                                             | mixed     | resolver and REST wire-shape tests            | disposable direct global and in-space assertions                                                                                                                                  | covered                                                                                                                                                          |
| `space_chat` (default space chat)                                                                                                             | gRPC      | –                                             | disposable direct fresh-space `NotFound` semantics; backup-integrity fixture setup                                                                                                | covered                                                                                                                                                          |
| `chat_stream` + `subscribe_chat`                                                                                                              | gRPC      | sub-id routing unit test                      | disposable real-server receive and shutdown                                                                                                                                       | partial — ordinary semantics covered; disconnect/reconnect fault coverage removed with the semantic mock, deferred to the reviewed external fault-injection plan |
| `unsubscribe_chat` / `shutdown` runtime control                                                                                               | gRPC      | –                                             | disposable two-chat selective routing, full quiet window, bounded shutdown                                                                                                        | covered                                                                                                                                                          |

### Cross-cutting helpers

| Operation                                                                                                                                                | Unit                                                            | Live                                                                               | Status                                                                    |
| -------------------------------------------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------- | ---------------------------------------------------------------------------------- | ------------------------------------------------------------------------- |
| `resolve_space_id` / `resolve_type*` / `resolve_property_id` / `resolve_view_id` / `resolve_template` / `resolve_chat_*`                                 | 33 resolver tests (bounded scans, ambiguity, dedup, fast paths) | `resolve_type`, `resolve_template` in `test_types`                                 | covered                                                                   |
| `verify_semantic` / `ensure_available`                                                                                                                   | 9 verify tests (caps, backoff, classification, drop safety)     | –                                                                                  | covered (unit)                                                            |
| `PagedResult::collect_all` / `into_stream`                                                                                                               | 12 paged tests                                                  | `test_pagination` (2)                                                              | covered                                                                   |
| `ProcessWatcher` subscribe/wait/unsubscribe                                                                                                              | import-finish reducer + space-correlation cases (4)             | real-server Markdown import lifecycle + import-finish fallback on one subscription | covered — connection faults deferred to the external fault-injection plan |
| disposable-space harness (`with_disposable_space_context`: lease, recovery ledger, sweeps, readiness convergence, child-command credential configurator) | `test_util` unit suites                                         | exercised by every fixture-heavy suite                                             | covered                                                                   |

## Delivered coverage work

Epic any-dm9k and all 18 descendants closed on 2026-08-08. The campaign
delivered required backup/restore smoke coverage, cleanup-owned fidelity
matrices, repaired CLI output contracts, backup-before-delete archive
selection, exact ignored-test dispositions, and protected serial live gates
for anytype-api, any-mcp, and anyr.

The anytype-api manifest now owns 19 required cases, three scheduled
characterization cases, and two explicitly excluded probes. Seven superseded
filter ignores were removed. The former ambient Set/view probes now create
cleanup-owned source-backed Sets and collections. These cross-crate gates are
meaningful integration evidence, but they do not erase a direct crate-level
gap unless they exercise the same public operation and assertions.

## Gap-ticket disposition

The audit reused existing tracker owners rather than filing duplicates. Direct
coverage is now implemented for auth (`any-fzsd`), paged lookups (`any-kmji`),
bounded archive counts (`any-h94e`), legacy file download (`any-gb01`), URL
upload (`any-uvck`), direct chat reads/edit (`any-5vbi`), selective unsubscribe
(`any-n1lm`), search/filter wire shapes (`any-vnj5`), and second-view
continuation (`any-v06h`). Two gaps remain outside that completed set:

- `delete_all_archived` direct coverage remains owned by `any-vjj`, which
  depends on the bounded archive fixture in `any-h94e`.
- `get_share_link` remains owned by `any-6s3` and is blocked on `any-x7f0`:
  the current Heart `ObjectShareByLink` backend panics because that operation
  has been removed.

The audited scope and disposition were:

1. Auth lifecycle: scripted challenge/key wire and error coverage;
   `authenticate_interactive` fast, forced, callback-error, and persistence
   paths; `auth_status`/`logout` state transitions; and ping failure
   classification. Disposable setup already proves live HTTP and gRPC ping
   success.
2. Paged lookup helpers: `lookup_space_by_name`, `lookup_types`, and
   `lookup_properties`, including cache modes, ambiguity, not-found, bounded
   pagination, and a partial-failure path for space lookup.
3. Archive aggregates: `any-h94e` now has direct bounded-count boundary and
   metadata tests. `any-vjj` retains the remaining `delete_all_archived` live
   coverage and depends on `any-h94e`; `delete_archived` is already exercised
   through the backup/restore integration tier.
4. `get_share_link`: `any-6s3` remains blocked on implementation ticket
   `any-x7f0` after a cleanup-owned live call reached the removed Heart method
   and crashed the server.
5. Legacy gRPC download-to-path: download into an owned scratch directory and
   compare exact bytes without reopening an untrusted path.
6. URL upload: an owned loopback HTTP source with bounded response bytes,
   exact uploaded-byte evidence, and explicit server shutdown. Rich-option
   upload already has protected cross-crate coverage.
7. Remaining gRPC chat reads and mutation: direct `list_chats`, `space_chat`,
   and `edit_text` coverage. `list_chats_in` already has direct unit and live
   coverage.
8. Selective chat unsubscribe: subscribe two cleanup-owned chats, unsubscribe
   one, and assert routing continues only for the retained subscription.
9. Offline search/filter wire shapes: `SearchRequest` and filter-DSL
   serialization beyond the #2879 regressions. Live negative-filter pagination
   remains owned by any-gz2k.
10. Second-view continuation pagination: extend existing any-v06h rather than
    filing a duplicate, and limit its remaining scope to continuation over the
    cleanup-owned multi-view fixture delivered by any-dm9k.9.

Do not file separate tickets for `update_space` (owned by any-e7ei.3),
`backup_space` (covered by the required backup/restore gates), file
list/search/get or preload/discard (covered by the protected anyr file suite),
legacy REST aliases that delegate to covered modern builders, or rich-option
upload. Existing any-q94w and the original ambient-view portion of any-v06h
are superseded by closed any-dm9k work and require tracker cleanup, not new
tickets.

## Scripted test-helper tooling

The feature-gated scripted-HTTP fixture was delivered by `any-dx9w`. It
captures bounded methods, paths, bodies, and finite response sequences; auth,
lookup, and search/filter coverage now consume that boundary.

Do not file the proposed Markdown-driven surface inventory check: it cannot
soundly enumerate builder-returning public operations and would create a
brittle second source of truth. Do not file a separate fixture-registry p2;
the remaining archived-object builder belongs to `any-vjj`.

## Known fixture constraints

- Space deletion: RESOLVED — production `delete_space` plus the test-only
  disposable-space lifecycle now exist; disposable contexts create and delete a
  fresh space per test. The former "no space delete API" blocker no longer
  applies.
- Views: no production view-create API; API-created collections expose a single
  `default` view. Test fixtures can now create a privately proven second view
  via raw Heart RPC, so multi-view evidence is available in the fixture tier
  only.
- Templates: no REST create; harness template fixtures exist via
  `create_template_fixtures` (heart `TemplateCreateFromObject`) and are the
  pattern to reuse.
- Live tests are tier-2 serial only and must never run from parallel
  workspaces. Fixture-heavy suites additionally require the ignored disposable
  tier (`ANYTYPE_TEST_SPACE_PREFIX`, environment keystore, full HTTP+gRPC
  credentials), automated by the protected any-dm9k.7 manifest gate.
