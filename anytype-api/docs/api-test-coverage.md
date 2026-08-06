# anytype-api HTTP/gRPC test coverage

Current status document for test coverage of the `anytype` crate's public
network-facing surface. Originated as the fixed-scope audit for any-gc5
(parent any-q1n, review gate any-jzn) dated 2026-07-21; now maintained as a
living inventory. Status as of 2026-08-06, reconciled against the crate
CHANGELOG `[Unreleased]` section. Planned coverage work is tracked in epic
any-dm9k (see "Planned work" below).

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
  `with_disposable_space_context` — automation of that tier is planned work
  (any-dm9k.7).
- Status: **covered** (meaningful direct assertions), **partial** (only one
  path, only error paths, or only indirect exercise), **uncovered** (no test
  invokes it).

## Test asset inventory

| Asset                                                                                                                                                                                                                                                    | Kind    | What it covers                                                                                                                                                                                                                                                                                                                                                  |
| -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `src/http_client.rs` tests (35)                                                                                                                                                                                                                          | unit    | retry/replay policy, response-size ceilings, redirect policy, diagnostics redaction, Retry-After parsing, credential-generation counter                                                                                                                                                                                                                         |
| `src/resolve.rs` tests (33)                                                                                                                                                                                                                              | unit    | all resolver classes: bounded scans, ambiguity candidates, dedup, direct-ID fast paths, chat/space/view/template resolution over paged fixtures                                                                                                                                                                                                                 |
| `src/chats.rs` tests (31)                                                                                                                                                                                                                                | unit    | REST chat wire shapes, SSE stream robustness/bounds/buffer ceilings, gRPC block round-trips, route paths, timestamp conversion, before-anchor history paging, verified-edit readback                                                                                                                                                                            |
| `src/body.rs` (33), `src/body_mutation.rs` (28), `src/body_rpc.rs` (11)                                                                                                                                                                                  | unit    | typed body-graph validation (identities, order, closed enums, fail-closed malformed/oversized graphs), verified block mutation state machines and constructors, finite show/close RPC seam (deadlines, decoder limits, payload-free counters)                                                                                                                   |
| `src/attached_discussions.rs` tests (19)                                                                                                                                                                                                                 | unit    | discussion discovery/ensure lifecycle and state machines, closed payload-free error kinds, reconciliation outcomes                                                                                                                                                                                                                                              |
| `src/files.rs` (19), `src/properties.rs` (20), `src/types.rs` (19), `src/spaces.rs` (12), `src/views.rs` (20), `src/objects.rs` (7), `src/members.rs` (5)                                                                                                | unit    | request-body serialization, model enums, direct-get scoping, tag-lookup budgets, upload backend selection and multipart byte ceilings, ranged/conditional downloads, space-administration validation, collection-membership seams, view path validation                                                                                                         |
| `src/verify.rs` (9), `src/paged.rs` (12), `src/cache.rs` (3), `src/keystore.rs` (5), `src/error.rs` (3), `src/validation.rs` (4), `src/client.rs` (5), `src/chat_stream.rs` (1), `src/process_watcher.rs` (4), `src/search.rs` (1), `src/filters.rs` (2) | unit    | verification retry semantics, PagedResult iteration/streaming, cache ops, keystore storage and modifier parsing, error redaction/classification, port discovery helpers, stream sub-id routing, import-finish fallback correlation, search limit validation, #2879 query-encoding regressions                                                                   |
| `src/test_util.rs` + `src/test_util/` + `tests/common/` (49 unit tests)                                                                                                                                                                                  | harness | disposable per-test space contexts with recovery ledger and sweeps, immediate `register_*` cleanup, retry helpers, template/collection-layout/second-view/Kanban/saved-view-filter fixtures, archive-evidence scans                                                                                                                                             |
| `tests/` (21 files, ~225 tests)                                                                                                                                                                                                                          | live    | see matrix; largest: filters (32 incl. compatibility matrix), search (25), types (24), validation (24), integration (24), properties (23), tags (18), members (18), cache (14); new since the original audit: body (5), body mutations (2), attached discussions (1), space admin (2), chat prerequisites (1), Kanban fixture (1), Markdown fidelity (1 matrix) |

## Coverage matrix

### Auth and client lifecycle (REST unless noted)

| Operation                                                               | Unit                                         | Live                                   | Status                                                     |
| ----------------------------------------------------------------------- | -------------------------------------------- | -------------------------------------- | ---------------------------------------------------------- |
| `create_auth_challenge`                                                 | –                                            | –                                      | uncovered                                                  |
| `create_api_key`                                                        | –                                            | –                                      | uncovered                                                  |
| `authenticate_interactive`                                              | –                                            | –                                      | uncovered (needs a dedicated interactive protocol harness) |
| `auth_status` / `logout`                                                | –                                            | –                                      | uncovered                                                  |
| `ping_http` / `ping_grpc`                                               | –                                            | –                                      | uncovered (exercised only implicitly)                      |
| `grpc_client`                                                           | –                                            | indirect (all gRPC tests)              | partial                                                    |
| `find_grpc`                                                             | helpers only (`extract_port_*`, lsof filter) | –                                      | partial                                                    |
| `http_metrics`                                                          | –                                            | asserted in `test_cache`, `smoke_test` | partial                                                    |
| HTTP pipeline (retry/limits/redirect/diagnostics/credential generation) | 35 tests                                     | `test_retry_helpers` (3 live-relevant) | covered                                                    |
| cache enable/disable/clear                                              | `cache.rs` (3)                               | `test_cache` (14)                      | covered                                                    |
| keystore save/load/update + modifier parsing                            | `keystore.rs` (5)                            | –                                      | covered (unit is the right level)                          |

### Spaces

| Operation                                                                           | Transport | Unit                                                                    | Live                                                                                 | Status                                                                                            |
| ----------------------------------------------------------------------------------- | --------- | ----------------------------------------------------------------------- | ------------------------------------------------------------------------------------ | ------------------------------------------------------------------------------------------------- |
| `spaces().list`                                                                     | REST      | –                                                                       | `smoke_test`, `test_cache`                                                           | covered                                                                                           |
| `space(id).get`                                                                     | REST      | –                                                                       | `smoke_test`, `test_cache`, `test_validation`                                        | covered                                                                                           |
| `space(id).get_direct` (cache-independent exact GET)                                | REST      | spaces unit tests                                                       | disposable readiness/preflight reads                                                 | covered                                                                                           |
| `new_space(..).create`                                                              | REST      | body serialization + empty-name rejection                               | exercised by every disposable-context test; harness verifies create/readiness/delete | covered — former no-delete blocker resolved                                                       |
| `update_space(..).update`                                                           | REST      | body serialization                                                      | –                                                                                    | partial                                                                                           |
| `lookup_space_by_name`                                                              | REST      | – (resolver-space unit tests cover `resolve_space_id`, not this helper) | –                                                                                    | uncovered                                                                                         |
| `create_chat_space`                                                                 | REST      | validation (`test_space_admin`)                                         | ignored disposable lifecycle (`space_administration_lifecycle`)                      | covered                                                                                           |
| `delete_space` (permanent)                                                          | REST      | validation                                                              | disposable-context teardown on every fixture-heavy test; ignored lifecycle test      | covered                                                                                           |
| space invites: `list_space_invites` / `create_space_invite` / `revoke_space_invite` | REST      | validation                                                              | ignored disposable lifecycle                                                         | covered                                                                                           |
| `enable_space_sharing` / `disable_space_sharing`                                    | REST      | validation                                                              | ignored disposable lifecycle                                                         | covered                                                                                           |
| `backup` (`backup_space`)                                                           | gRPC      | –                                                                       | –                                                                                    | uncovered — planned: any-dm9k.2/.4 exercise the surface end-to-end through anyr                   |
| `list_archived(..).list`                                                            | gRPC      | –                                                                       | indirect (harness archive-evidence scans)                                            | partial                                                                                           |
| `count_archived`                                                                    | gRPC      | –                                                                       | –                                                                                    | uncovered                                                                                         |
| `delete_archived` / `delete_all_archived`                                           | gRPC      | –                                                                       | –                                                                                    | uncovered — planned: any-dm9k.5 covers archive selection for space deletion at the anyr/CLI level |

### Objects and attached discussions

| Operation                                                              | Transport | Unit                                               | Live                                                                                       | Status                              |
| ---------------------------------------------------------------------- | --------- | -------------------------------------------------- | ------------------------------------------------------------------------------------------ | ----------------------------------- |
| `object(..).get`                                                       | REST      | –                                                  | `integration`, `smoke_test`, many others                                                   | covered                             |
| `objects(..).list` (+ filters/sort/pagination)                         | REST      | –                                                  | `test_filters`, `integration`; empty-filter and pagination-offset cases own their fixtures | covered                             |
| `new_object(..).create`                                                | REST      | body serialization                                 | `integration`, `smoke_test`, fixtures everywhere                                           | covered                             |
| `update_object(..).update`                                             | REST      | body serialization                                 | `integration`, `smoke_test`                                                                | covered                             |
| `object(..).delete` / `delete_once`                                    | REST      | –                                                  | `integration`, `smoke_test`, harness cleanup                                               | covered                             |
| `get_share_link`                                                       | gRPC      | –                                                  | –                                                                                          | uncovered                           |
| `set_properties` (typed value coercion)                                | helper    | property-value unit tests                          | `test_properties` set/read matrix (all formats)                                            | covered                             |
| discussion discovery (`discussion` get)                                | gRPC+REST | `attached_discussions.rs` (19, shared with ensure) | ignored disposable `test_attached_discussions`                                             | covered                             |
| discussion `ensure` (idempotent, single dispatch, reconciled outcomes) | gRPC+REST | lifecycle/state-machine unit tests                 | ignored disposable exact get/ensure/repeat test                                            | covered                             |
| `ObjectAddDiscussion` repeat-create upstream defect                    | gRPC      | –                                                  | ignored prefix-authorized probe (2 raw RPCs, evidence-only)                                | covered (defect documentation tier) |

### Body blocks (gRPC `ObjectShow`/mutation RPCs)

| Operation                                                                                                                          | Unit                     | Live                                                                                                                                  | Status  |
| ---------------------------------------------------------------------------------------------------------------------------------- | ------------------------ | ------------------------------------------------------------------------------------------------------------------------------------- | ------- |
| `blocks()` read → `BodySnapshot`/`BodyBlock` (typed variants, exact IDs/order, closed enums, `Unsupported` markers, `BodyLimits`)  | `body.rs` (33)           | 5 ignored disposable tests: typed-variant/order preservation, tightened limits, missing object, opaque dataview, show/close lifecycle | covered |
| show/close lifecycle (no server-side open state; owned foreground close)                                                           | `body_rpc.rs` seam tests | `test_body_show_close_lifecycle_holds_no_server_open_state` (verified via `DebugOpenedObjects`)                                       | covered |
| `BodySnapshot::edit` verified mutations (create/append/update/delete/reorder, bounded batches, table receipts, constructor matrix) | `body_mutation.rs` (28)  | `test_body_mutations` (2, tier-2 disposable)                                                                                          | covered |
| `BodyRpcConfig` finite RPC seam (shared deadline, decoder limits, payload-free counters)                                           | `body_rpc.rs` (11)       | exercised by all body live suites                                                                                                     | covered |
| `plain_markdown_representation` (closed write/read contract)                                                                       | objects unit tests       | ignored serial disposable Markdown fidelity matrix (byte-identical replay cohorts + documented drift)                                 | covered |

### Types, properties, tags, templates

| Operation                                                                                                      | Unit                                       | Live                                                           | Status              |
| -------------------------------------------------------------------------------------------------------------- | ------------------------------------------ | -------------------------------------------------------------- | ------------------- |
| types list/get/create/update/delete                                                                            | update serialization + classification (19) | `test_types` (24), `test_cache`                                | covered             |
| `type(..).get_direct`                                                                                          | resolver unit tests                        | `test_resolve_type_by_id_bypasses_primed_cache`                | covered             |
| type-property classification read (featured vs recommended reconciliation, fail-closed cross-transport checks) | classification unit tests                  | disposable real-server coverage                                | covered             |
| `lookup_type_by_key`                                                                                           | resolver unit tests                        | `smoke_test`, `test_filters`                                   | covered             |
| `lookup_types` (bulk)                                                                                          | –                                          | –                                                              | uncovered           |
| properties list/get/create/update/delete (incl. `no_cache_refresh` bounded readback)                           | serialization + direct-get (20)            | `test_properties` (23)                                         | covered             |
| `property(..).get_direct` + explicit-ID tag lookup                                                             | budget/identity unit tests                 | –                                                              | covered (unit)      |
| `lookup_property_by_key`                                                                                       | resolver unit tests                        | `tests/common`, `smoke_test`                                   | covered             |
| `lookup_properties` (bulk)                                                                                     | –                                          | –                                                              | uncovered           |
| `lookup_property_tag`                                                                                          | tag-budget unit tests                      | `test_tags`, `tests/common`                                    | covered             |
| tags list/get/create/update/delete                                                                             | –                                          | `test_tags` (18)                                               | covered (live only) |
| `template(..).get` / `templates(..).list`                                                                      | resolver template unit tests               | `test_types` template trio; `create_template_fixtures` harness | covered             |

### Views, collections, members, search

| Operation                                                                                                        | Unit                                 | Live                                                                                                         | Status                                                    |
| ---------------------------------------------------------------------------------------------------------------- | ------------------------------------ | ------------------------------------------------------------------------------------------------------------ | --------------------------------------------------------- |
| `list_views(..).list`                                                                                            | path validation                      | `test_views` (set + collection); second-view test fixture (raw Heart RPC) now exists for multi-view evidence | covered — production view-create API still absent         |
| view objects list (selected view)                                                                                | –                                    | `test_views`                                                                                                 | covered                                                   |
| `view_add_objects` / `view_remove_object`                                                                        | –                                    | `test_view_add_remove_objects_collection`                                                                    | covered                                                   |
| `collection_member_add` (singular, non-replayed, exact status preservation)                                      | views unit tests                     | membership observation/page suites                                                                           | covered                                                   |
| `collection_membership_page` (canonical manual-collection pages, continuation arithmetic, bounded subscriptions) | views unit tests                     | disposable real-server evidence incl. Kanban fixture                                                         | covered                                                   |
| `observe_collection_membership` (present/absent with index controls)                                             | views unit tests                     | disposable real-server coverage                                                                              | covered                                                   |
| Kanban grouping fixture (grouping relation, column movement, two-item pages)                                     | –                                    | ignored disposable `test_kanban_fixture`                                                                     | covered (harness tier)                                    |
| `member(..).get` / `members(..).list`                                                                            | model helpers (5)                    | `test_members` (18)                                                                                          | covered                                                   |
| `search_global().execute` / space search                                                                         | limit validation (1)                 | `test_search` (25), `integration`                                                                            | covered (live; offline wire-shape tests still limited)    |
| filter DSL (`filters.rs`)                                                                                        | #2879 query-encoding regressions (2) | `test_filters` (32 incl. ignored numeric/checkbox compatibility matrix vs `anytype-heart#2879`)              | covered (live-heavy; serialization unit tests still thin) |

### Files

| Operation                                                              | Transport | Unit                                                                      | Live                                                                       | Status                                   |
| ---------------------------------------------------------------------- | --------- | ------------------------------------------------------------------------- | -------------------------------------------------------------------------- | ---------------------------------------- |
| `upload` (path/bytes/reader, plain)                                    | REST      | backend selection, bounded multipart construction, response normalization | `test_files` backend auto-selection incl. reader and rich-option promotion | covered                                  |
| `upload` (URL / rich options)                                          | gRPC      | backend-selection only                                                    | –                                                                          | partial — selection tested, transfer not |
| `download_bytes`                                                       | REST      | –                                                                         | `test_files`                                                               | covered                                  |
| `download_request` (range/width/conditional, header-evidence ceilings) | REST      | ranged + conditional + 304 + ceiling unit tests                           | live `HEAD`/`206`/`412`/`416` + rejected zero-length range                 | covered                                  |
| `metadata` / `head`                                                    | REST      | head-without-body + validator parsing                                     | live metadata `HEAD` coverage                                              | covered                                  |
| `delete` / `delete_request(..).permanently`                            | REST      | skip_bin query unit test                                                  | `test_files` incl. permanent delete                                        | covered                                  |
| `http_upload` / `http_download` / `http_delete` (legacy REST)          | REST      | upload schema deserialization only                                        | –                                                                          | partial/uncovered                        |
| `list` / `search` / `get` (rich metadata)                              | gRPC      | –                                                                         | –                                                                          | uncovered                                |
| `preload` / `discard_preload`                                          | gRPC      | –                                                                         | –                                                                          | uncovered                                |
| `download` (legacy, Heart writes to path)                              | gRPC      | –                                                                         | –                                                                          | uncovered                                |

### Chats

| Operation                                                                                                                                     | Transport | Unit                                          | Live                                                                                                                 | Status                                                                                                                                                           |
| --------------------------------------------------------------------------------------------------------------------------------------------- | --------- | --------------------------------------------- | -------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `chats().in_space(..)`: chat list/create; message add/edit/get/list/search/delete; reactions; read state                                      | REST      | wire-shape tests (add/edit/list/search paths) | disposable resolver-supporting reads in `test_chat_discovery`; CRUD/search/reaction/read-state cases in `test_chats` | covered                                                                                                                                                          |
| MCP prerequisites: fallible UTC-ms timestamp conversion, before-anchor history pages, verified text/format edits with advancing `modified_at` | REST      | scripted transport tests                      | ignored disposable `test_chat_prerequisites`                                                                         | covered                                                                                                                                                          |
| REST SSE `message_stream(..).open` (incl. per-event buffer ceilings)                                                                          | REST      | SSE bound/robustness suite                    | disposable `test_chat_stream::rest_chat_stream_receives_initial_message`                                             | covered                                                                                                                                                          |
| `add_message`/`edit_message`/`delete_message`/`get_messages`/`list_messages`/`read_messages`/`unread_messages`                                | gRPC      | block round-trip + rich-state conversion      | `test_chats::test_chat_message_crud`                                                                                 | covered                                                                                                                                                          |
| `send_text` / `toggle_reaction` / `read_all`                                                                                                  | gRPC      | –                                             | `test_chats` / `test_chat_discovery` (single paths)                                                                  | partial                                                                                                                                                          |
| `edit_text`                                                                                                                                   | gRPC      | –                                             | –                                                                                                                    | uncovered                                                                                                                                                        |
| `search_chats_in` / `get_chat` / `resolve_chat_by_name`                                                                                       | gRPC      | resolver chat-discovery unit tests            | disposable `test_chat_discovery`                                                                                     | covered                                                                                                                                                          |
| `list_chats` / `list_chats_in` (global; `list_chats_in` now REST)                                                                             | mixed     | –                                             | –                                                                                                                    | uncovered                                                                                                                                                        |
| `space_chat` (default space chat)                                                                                                             | gRPC      | –                                             | –                                                                                                                    | uncovered                                                                                                                                                        |
| `chat_stream` + `subscribe_chat`                                                                                                              | gRPC      | sub-id routing unit test                      | disposable real-server receive and shutdown                                                                          | partial — ordinary semantics covered; disconnect/reconnect fault coverage removed with the semantic mock, deferred to the reviewed external fault-injection plan |
| `unsubscribe_chat` / `shutdown` runtime control                                                                                               | gRPC      | –                                             | shutdown only                                                                                                        | partial                                                                                                                                                          |

### Cross-cutting helpers

| Operation                                                                                                                                                | Unit                                                            | Live                                                                               | Status                                                                    |
| -------------------------------------------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------- | ---------------------------------------------------------------------------------- | ------------------------------------------------------------------------- |
| `resolve_space_id` / `resolve_type*` / `resolve_property_id` / `resolve_view_id` / `resolve_template` / `resolve_chat_*`                                 | 33 resolver tests (bounded scans, ambiguity, dedup, fast paths) | `resolve_type`, `resolve_template` in `test_types`                                 | covered                                                                   |
| `verify_semantic` / `ensure_available`                                                                                                                   | 9 verify tests (caps, backoff, classification, drop safety)     | –                                                                                  | covered (unit)                                                            |
| `PagedResult::collect_all` / `into_stream`                                                                                                               | 12 paged tests                                                  | `test_pagination` (2)                                                              | covered                                                                   |
| `ProcessWatcher` subscribe/wait/unsubscribe                                                                                                              | import-finish reducer + space-correlation cases (4)             | real-server Markdown import lifecycle + import-finish fallback on one subscription | covered — connection faults deferred to the external fault-injection plan |
| disposable-space harness (`with_disposable_space_context`: lease, recovery ledger, sweeps, readiness convergence, child-command credential configurator) | 49 `test_util` unit tests                                       | exercised by every fixture-heavy suite                                             | covered                                                                   |

## Planned work (epic any-dm9k — "Restore executable 0.5 backup and ignored-live-test coverage")

### Backup and restore (exercises `backup_space` and the anyr/anyback surface)

- **any-dm9k.1** (p1 bug) — repair the anyback live harnesses
  (`e2e_backup_restore`, `p1_cross_space`, `restore_matrix`,
  `integrity_nightly`) to invoke the consolidated `anyr` backup CLI; today all
  behavioral cases are ignored and would fail before reaching backup/restore.
- **any-dm9k.2** (p1 task) — one required, non-skipping `anyr backup
  create`/restore smoke gate over cleanup-owned source and destination spaces.
- **any-dm9k.3** (p1 bug) — make embedded backup commands honor anyr's global
  output contracts (quiet/pretty/table/output-file) instead of raw stdout.
- **any-dm9k.4** (p1 task) — extend the repaired live matrix to 0.5
  content-fidelity classes: file bytes/name/MIME/attachment references, chat
  block/message order and replies, via current API paths.

### Spaces and archives

- **any-dm9k.5** (p1 task) — prove backup-before-delete and archive-selection
  behavior for space deletion with a unique cleanup-owned space (replaces the
  fixed ambient `xtest-123-xyz` fixture that can silently skip).

### Ignored-test automation and harness (anytype-api)

- **any-dm9k.7** (p2, tracking container) — automate the anytype-api disposable
  ignored-test suite; work delegated to:
  - **any-dm9k.7.1** — exhaustive inventory + admit/exclude disposition of
    every cleanup-owned disposable ignored test (bodies, chats, Markdown
    fidelity, views, space administration, attached discussions, filters,
    process watching), manifest format and gate design.
  - **any-dm9k.7.2** — implement the manifest plus a protected/scheduled serial
    gate with evidence capture and docs, executing the .7.1 dispositions.
- **any-dm9k.9** (p2 task) — remove the seven legacy filter ignores superseded
  by the cleanup-owned condition matrix and the two ambient-Set view ignores,
  so the ignored tier lists only executable coverage.

### Cross-crate ignored live tests (adjacent crates, tracked here for visibility)

- **any-dm9k.8** (p2, tracking container) — reconcile orphan ignored live tests
  in any-mcp and anyr; delegated to:
  - **any-dm9k.8.1** — inventory/disposition audit across any-mcp, anyr Rust,
    and the Python CLI suite skips.
  - **any-dm9k.8.2** — any-mcp: admit or retire the ignored real-server tests
    outside the protected `headless_` matrix.
  - **any-dm9k.8.3** — anyr: wire the lone ignored live type-property
    preservation test into a gate; harden Python CLI skip paths.

## Remaining gap candidates (p3 unless noted, none filed)

One line each: method — gap — suggested scope. Items from the original audit
that have since been covered (files `metadata`, conditional/ranged download
liveness, the space-delete blocker) are removed.

1. auth challenge/key flow — no tests — scripted HTTP unit tests for `create_auth_challenge`/`create_api_key` wire shapes and error mapping.
2. `auth_status`/`logout` — no tests — unit tests over keystore-backed credential state transitions.
3. `ping_http`/`ping_grpc` — no direct tests — scripted HTTP coverage plus constructed classification cases and disposable real-server success coverage.
4. `lookup_space_by_name` — no tests — paged-fixture unit tests (dedup, ambiguity, not-found) mirroring existing resolver suites.
5. `update_space` — serialization only — live rename-and-revert test against a cleanup-owned disposable space (now unblocked by the disposable harness).
6. `backup_space` — no direct crate-level tests — live gRPC smoke exporting a disposable space to a temp dir, asserting artifact presence per format option (the anyr-level surface is any-dm9k.2/.4).
7. `count_archived`/`delete_archived`/`delete_all_archived` — no tests — live tests over self-created archived fixtures with type-scoped evidence (reuse harness archive-scan pattern).
8. `get_share_link` — no tests — live gRPC smoke on a registered fixture object.
9. files gRPC `list`/`search`/`get` — no tests — live tests reusing the uploaded-file fixture from `test_files`.
10. files `preload`/`discard_preload` — no tests — live preload lifecycle test with cleanup registration.
11. files legacy gRPC `download` (to path) — no tests — live test downloading to scratch dir and comparing bytes.
12. files legacy `http_upload`/`http_download`/`http_delete` — schema-only — scripted HTTP unit tests for route/body parity with the modern builders.
13. gRPC rich/URL upload transfer — selection-only — live URL-upload and rich-option upload test (needs reachable URL fixture or local server).
14. chats gRPC `list_chats`/`list_chats_in`/`space_chat`/`edit_text` — no tests — extend `test_chat_discovery`/`test_chat_message_crud` with these calls.
15. chat stream `unsubscribe_chat` runtime control — no tests — use cleanup-owned real chats to subscribe two chats, unsubscribe one, and assert routing; keep disconnect behavior in the reviewed external fault tier.
16. search/filter wire shapes — live-heavy — offline serialization unit tests for `SearchRequest` and the filter DSL beyond the two #2879 encoding regressions.
17. `lookup_types`/`lookup_properties` bulk helpers — no tests — paged-fixture unit tests for batch resolution and partial-failure behavior.
18. views continuation pagination — previously blocked — now feasible via the second-view Heart-RPC test fixture; add a continuation case over a fixture-created multi-view collection.

## Candidate test-helper tooling tickets (p2, none filed)

1. Shared scripted-HTTP fixture harness — `http_client.rs`, `resolve.rs`, `chats.rs`, `files.rs`, and `properties.rs` still carry private in-process fixture servers; promote one reusable harness into `test_util` (feature-gated) so gap candidates 1–4, 12, 16–17 are cheap and duplication stops growing. Also directly useful to the shared MCP test harness (any-cwi).
2. Surface-inventory drift check — a unit test that enumerates the public async surface and compares it against this document's matrix (same pattern as `test_retry_helpers::live_mutation_retry_inventory_is_current`), so this status document cannot silently rot.
3. Fixture-registry extensions — archived-object fixture builder (registers then archives N objects of a fresh custom type) for the archive-surface gaps; the template, collection-layout, second-view, Kanban, and saved-view-filter fixtures from the original list have landed.

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
  credentials); automating that tier is any-dm9k.7.
