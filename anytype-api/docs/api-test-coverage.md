# anytype-api HTTP/gRPC test coverage

Current status document for test coverage of the `anytype` crate's public
network-facing surface. Originated as the fixed-scope audit for any-gc5
(parent any-q1n, review gate any-jzn) dated 2026-07-21; now maintained as a
living inventory. Status as of 2026-08-15, reconciled from jj change
`kmnyrwzy` / Git commit `4d76af1e` through the current `main` history, the
crate and any-mcp CHANGELOG `[Unreleased]` sections, and tracker closures in
the same interval.

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
REST/gRPC split; crate CHANGELOG `[Unreleased]`; `jj log -r kmnyrwzy::main`
and its file diffs; and coverage, CI, fixture, and transport tickets closed
after 2026-08-10 07:12 UTC.

Coverage classes:

- **unit**: offline tests inside `src/` modules, backed by scripted HTTP
  fixture servers or constructed transport-independent values. Run in tier 1
  (`cargo test -p anytype --lib`). The custom semantic gRPC mock has been
  removed; gRPC behavior is covered by constructed reducer values plus
  real-server tests.
- **live**: `tests/` integration tests requiring a running Anytype server
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

- **HTTP core (unit):** `src/http_client.rs` (56) and `src/http_timeout.rs`
  (6) cover deadlines, policy defaults, retry and replay safety, connection
  classification, response ceilings, redirects, redaction, metrics,
  `Retry-After`, and credential generation.
- **Resolvers and chats (unit):** `src/resolve.rs` (33) covers every resolver
  class; `src/chats.rs` (32) covers REST wire shapes, SSE bounds, gRPC block
  conversions, timestamp conversion, history paging, and edit readback.
- **Bodies and discussions (unit):** `src/body.rs` (33),
  `src/body_mutation.rs` (28), and `src/body_rpc.rs` (12) cover typed graph
  validation, mutation state machines, deadlines, decoder limits, and
  payload-free counters. `src/attached_discussions.rs` (19) covers discovery,
  ensure, reconciliation, and closed error kinds.
- **Resource modules (unit):** `src/files.rs` (20), `src/properties.rs` (20),
  `src/types.rs` (19), `src/spaces.rs` (26), `src/views.rs` (20),
  `src/objects.rs` (9), and `src/members.rs` (5) cover serialization,
  validation, bounded scans, transfer limits, archive counts, collection
  membership, and view paths.
- **Cross-cutting modules (unit):** `src/auth.rs` (9), `src/verify.rs` (11),
  `src/paged.rs` (12), `src/cache.rs` (3), `src/keystore.rs` (6),
  `src/error.rs` (3), `src/validation.rs` (4), `src/client.rs` (5),
  `src/chat_stream.rs` (1), `src/process_watcher.rs` (4), `src/search.rs` (4),
  and `src/filters.rs` (5) cover authentication, verification, pagination,
  cache and keystore behavior, error handling, port discovery, streaming,
  process correlation, search, and filter serialization.
- **Harness:** `src/test_util.rs`, `src/test_util/`, and `tests/common/` provide
  disposable spaces, recovery-ledger sweeps, immediate resource registration,
  retry helpers, complex fixtures, and archive-evidence scans.
- **Live:** 27 integration targets under `tests/` cover the operations detailed
  below, including the protected fixture manifest.

## Coverage matrix

### Auth and client lifecycle (REST unless noted)

| Surface                                                | Evidence                                                                                                                                                | Status                   |
| ------------------------------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------ |
| REST: `create_auth_challenge`                          | Unit: exact wire and API errors                                                                                                                         | covered                  |
| REST: `create_api_key`                                 | Unit: exact wire and API errors                                                                                                                         | covered                  |
| REST: `authenticate_interactive`                       | Unit: fast, forced, callback, API, and persistence paths                                                                                                | covered                  |
| Helper: `auth_status` / `logout`                       | Unit: exact memory and keystore transitions                                                                                                             | covered                  |
| Mixed: `ping_http` / `ping_grpc`                       | Unit: typed authentication failures<br>Live: every disposable readiness preflight                                                                       | covered                  |
| gRPC: `grpc_client`                                    | Unit: credential selection, cached/concurrent initialization, typed missing-credential and connection failures<br>Live: indirect through all gRPC tests | covered                  |
| Helper: `find_grpc`                                    | Unit: lsof absence/failure, process/LISTEN/duplicate filtering, failed probes, first responsive candidate<br>Process: supported-Unix owned listener     | covered                  |
| REST: `http_metrics`                                   | Live: indirect assertions in `test_cache` and `smoke_test`                                                                                              | partial; `any-upsa` (P3) |
| REST: HTTP pipeline                                    | Unit: 56 deadline, retry, limit, redirect, diagnostic, and metric tests<br>Live: `test_retry_helpers` (5)                                               | covered                  |
| Helper: cache enable/disable/clear                     | Unit: `cache.rs` (3)<br>Live: `test_cache` (28)                                                                                                         | covered                  |
| Helper: keystore save/load/update and modifier parsing | Unit: `keystore.rs` (6)<br>Live: env-only disposable credential setup                                                                                   | covered                  |

### Spaces

| Surface                                           | Evidence                                                                                                                   | Status                                                             |
| ------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------ |
| REST: `spaces().list`                             | Live: `smoke_test`, `test_cache`                                                                                           | covered                                                            |
| REST: `space(id).get`                             | Live: `smoke_test`, `test_cache`, `test_validation`                                                                        | covered                                                            |
| REST: `space(id).get_direct`                      | Unit: cache-independent exact GET<br>Live: disposable readiness reads                                                      | covered                                                            |
| REST: `new_space(..).create`                      | Unit: body serialization and empty-name rejection<br>Live: every disposable context verifies create, readiness, and delete | covered                                                            |
| REST: `update_space(..).update`                   | Unit: body serialization                                                                                                   | partial; `any-e7ei.3` (P2)                                         |
| REST: `lookup_space_by_name`                      | Unit: cache modes, pagination, not-found, and partial failure                                                              | covered                                                            |
| REST: `create_chat_space`                         | Unit: validation<br>Live: ignored disposable lifecycle                                                                     | covered                                                            |
| REST: `delete_space`                              | Unit: validation<br>Live: disposable teardown and ignored lifecycle                                                        | covered                                                            |
| REST: space invite list/create/revoke             | Unit: validation<br>Live: ignored disposable lifecycle                                                                     | covered                                                            |
| REST: enable/disable space sharing                | Unit: validation and definitive retry classification<br>Live: connected clean-server tier                                  | covered                                                            |
| gRPC: `backup` (`backup_space`)                   | Live: required anyr/anyback smoke and fidelity gates                                                                       | covered (cross-crate)                                              |
| gRPC: `list_archived(..).list`                    | Unit: page, filter, type-ID, and fail-safe validation<br>Live: harness archive-evidence scans                              | covered                                                            |
| gRPC: `count_archived` / `count_archived_bounded` | Unit: exact boundary, probe, budget, and request-count state machine                                                       | covered                                                            |
| gRPC: `delete_archived` / `delete_all_archived`   | Live: `delete_archived` through backup/restore cleanup                                                                     | partial; `any-vjj` (P3) owns direct `delete_all_archived` coverage |

### Objects and attached discussions

| Surface                                           | Evidence                                                                                                                                              | Status                     |
| ------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------- | -------------------------- |
| REST: `object(..).get`                            | Live: `integration`, `smoke_test`, and other suites                                                                                                   | covered                    |
| REST: `objects(..).list`                          | Unit: endpoint selection for typed numeric, checkbox, and positive type filters<br>Live: `test_filters`, `integration`, and owned pagination fixtures | covered                    |
| REST: `new_object(..).create`                     | Unit: body serialization<br>Live: `integration`, `smoke_test`, and fixture setup                                                                      | covered                    |
| REST: `update_object(..).update`                  | Unit: body serialization<br>Live: `integration`, `smoke_test`                                                                                         | covered                    |
| REST: `object(..).delete` / `delete_once`         | Live: `integration`, `smoke_test`, and harness cleanup                                                                                                | covered                    |
| Helper: `get_share_link` / `Object::get_link`     | Unit: exact current-client universal URL and typed ID validation<br>Live: cleanup-owned object link and server-health proof                           | covered                    |
| Helper: `set_properties`                          | Unit: typed property-value coercion<br>Live: all-format set/read matrix                                                                               | covered                    |
| Mixed: discussion discovery                       | Unit: `attached_discussions.rs` lifecycle cases<br>Live: ignored disposable suite                                                                     | covered                    |
| Mixed: discussion `ensure`                        | Unit: lifecycle and state-machine cases<br>Live: exact get/ensure/repeat                                                                              | covered                    |
| gRPC: repeated `ObjectAddDiscussion` defect probe | Live: two raw RPCs in a prefix-authorized evidence tier                                                                                               | covered (characterization) |

### Body blocks (gRPC `ObjectShow`/mutation RPCs)

| Surface                                               | Evidence                                                                                                  | Status  |
| ----------------------------------------------------- | --------------------------------------------------------------------------------------------------------- | ------- |
| gRPC: `blocks()` → typed `BodySnapshot` / `BodyBlock` | Unit: `body.rs` (33)<br>Live: five disposable graph, limit, missing-object, dataview, and lifecycle cases | covered |
| gRPC: show/close lifecycle                            | Unit: `body_rpc.rs` seams<br>Live: `DebugOpenedObjects` proves no server-side open state                  | covered |
| gRPC: `BodySnapshot::edit` mutations                  | Unit: `body_mutation.rs` (28)<br>Live: `test_body_mutations` (2)                                          | covered |
| gRPC: finite `BodyRpcConfig` seam                     | Unit: `body_rpc.rs` (12)<br>Live: exercised by all body suites                                            | covered |
| REST: `plain_markdown_representation`                 | Unit: object serialization<br>Live: byte-identical replay cohorts and documented drift                    | covered |

### Types, properties, tags, templates

| Surface                                                    | Evidence                                                                                                | Status  |
| ---------------------------------------------------------- | ------------------------------------------------------------------------------------------------------- | ------- |
| REST: type list/get/create/update/delete                   | Unit: serialization and classification (19)<br>Live: `test_types` (48), `test_cache`                    | covered |
| REST: `type(..).get_direct`                                | Unit: resolver cases<br>Live: primed-cache bypass                                                       | covered |
| Mixed: type-property classification                        | Unit: reconciliation and fail-closed checks<br>Live: disposable server and cross-crate restore fidelity | covered |
| REST: `lookup_type_by_key`                                 | Unit: resolver cases<br>Live: `smoke_test`, `test_filters`                                              | covered |
| REST: bulk `lookup_types`                                  | Unit: cache modes, pagination, deduplication, and ambiguity                                             | covered |
| REST: property list/get/create/update/delete               | Unit: serialization and direct-get (20)<br>Live: `test_properties` (46)                                 | covered |
| REST: `property(..).get_direct` and explicit-ID tag lookup | Unit: budget and identity cases                                                                         | covered |
| REST: `lookup_property_by_key`                             | Unit: resolver cases<br>Live: `tests/common`, `smoke_test`                                              | covered |
| REST: bulk `lookup_properties`                             | Unit: cache modes, pagination, deduplication, and ambiguity                                             | covered |
| REST: `lookup_property_tag`                                | Unit: tag budgets<br>Live: `test_tags`, `tests/common`                                                  | covered |
| REST: tag list/get/create/update/delete                    | Live: `test_tags` (38)                                                                                  | covered |
| REST: template get/list                                    | Unit: resolver cases<br>Live: `test_types` trio and harness fixtures                                    | covered |

### Views, collections, members, search

| Surface                                         | Evidence                                                                                                                    | Status                                                              |
| ----------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------- |
| gRPC: `list_views(..).list`                     | Unit: path validation<br>Live: Set, collection, and raw-RPC second-view fixture                                             | covered; production view-create API is absent                       |
| gRPC: selected-view object list                 | Live: differential second-view `limit(1)` continuation                                                                      | covered                                                             |
| gRPC: `view_add_objects` / `view_remove_object` | Live: collection add/remove                                                                                                 | covered                                                             |
| gRPC: `collection_member_add`                   | Unit: singular non-replay and exact status<br>Live: membership suites                                                       | covered                                                             |
| gRPC: `collection_membership_page`              | Unit: pages, continuation arithmetic, bounded subscriptions<br>Live: disposable server and Kanban fixture                   | covered                                                             |
| gRPC: `observe_collection_membership`           | Unit: present/absent and index controls<br>Live: disposable server                                                          | covered                                                             |
| Harness: Kanban grouping fixture                | Live: grouping relation, movement, and two-item pages                                                                       | covered                                                             |
| REST: member get/list                           | Unit: model helpers (5)<br>Live: `test_members` (32)                                                                        | covered                                                             |
| REST: global and space search                   | Unit: limit, request, sort, and pagination wire shapes (4)<br>Live: `test_search` (50), `integration`                       | covered                                                             |
| REST: filter DSL                                | Unit: encodings, scalar validation, and endpoint selection (5)<br>Live: `test_filters` (43), including `anytype-heart#2879` | covered; extended negative tag-filter pagination is `any-gz2k` (P3) |

### Files

| Surface                                     | Evidence                                                                                                                      | Status                     |
| ------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------- | -------------------------- |
| REST: plain path/bytes/reader upload        | Unit: backend selection, bounded multipart, response normalization<br>Live: auto-selection, reader, and rich-option promotion | covered                    |
| gRPC: URL and rich-option upload            | Unit: backend selection<br>Live: bounded loopback URL and protected anyr gate                                                 | covered                    |
| REST: `download_bytes`                      | Live: `test_files` and cross-crate exact restored bytes                                                                       | covered                    |
| REST: ranged/conditional `download_request` | Unit: range, 304, and ceilings<br>Live: `HEAD`, `206`, `412`, `416`, and rejected empty range                                 | covered                    |
| REST: `metadata` / `head`                   | Unit: body-free HEAD and validator parsing<br>Live: metadata HEAD and restored MIME                                           | covered                    |
| REST: delete and permanent delete           | Unit: `skip_bin` query<br>Live: `test_files`                                                                                  | covered                    |
| REST: legacy upload/download/delete aliases | Unit: delegation to covered builders                                                                                          | covered through delegation |
| gRPC: rich metadata list/search/get         | Live: protected anyr file gate                                                                                                | covered (cross-crate)      |
| gRPC: preload/discard                       | Live: protected anyr file gate                                                                                                | covered (cross-crate)      |
| gRPC: legacy path download                  | Live: owned-path exact bytes and cleanup                                                                                      | covered                    |

### Chats

| Surface                                                 | Evidence                                                                                         | Status                                                  |
| ------------------------------------------------------- | ------------------------------------------------------------------------------------------------ | ------------------------------------------------------- |
| REST: chat and message CRUD/search/reactions/read state | Unit: route and wire shapes<br>Live: disposable discovery and CRUD; cross-crate restore fidelity | covered                                                 |
| REST: MCP chat prerequisites                            | Unit: timestamp, history-page, and edit transport cases<br>Live: disposable prerequisite suite   | covered                                                 |
| REST: SSE `message_stream(..).open`                     | Unit: bounds and robustness<br>Live: initial-message receipt                                     | covered                                                 |
| gRPC: message CRUD/read operations                      | Unit: block and rich-state conversion<br>Live: `test_chat_message_crud`                          | covered                                                 |
| gRPC: `send_text` / `toggle_reaction` / `read_all`      | Live: single paths in `test_chats` and `test_chat_discovery`                                     | partial; `any-ih7t` (P3)                                |
| gRPC: `edit_text`                                       | Live: direct edit with independent REST text/style/mark readback                                 | covered                                                 |
| gRPC: search/get/resolve chat                           | Unit: resolver cases<br>Live: disposable discovery                                               | covered                                                 |
| Mixed: global and in-space chat list                    | Unit: resolver and REST wire shapes<br>Live: direct global and scoped assertions                 | covered                                                 |
| gRPC: default `space_chat`                              | Live: fresh-space `NotFound` semantics and backup fixture                                        | covered                                                 |
| gRPC: `chat_stream` / `subscribe_chat`                  | Unit: subscription-ID routing<br>Live: receive and shutdown                                      | partial; reconnect chain `any-k6o5.4`–`any-k6o5.6` (P4) |
| gRPC: `unsubscribe_chat` / `shutdown`                   | Live: selective routing, quiet window, and bounded shutdown                                      | covered                                                 |

### Cross-cutting helpers

| Surface                                             | Evidence                                                                                                   | Status                                                                         |
| --------------------------------------------------- | ---------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------ |
| Helpers: all `resolve_*` operations                 | Unit: 33 bounded scan, ambiguity, deduplication, and fast-path cases<br>Live: type and template resolution | covered                                                                        |
| Helpers: `verify_semantic` / `ensure_available`     | Unit: 11 cap, backoff, classification, and drop-safety cases                                               | covered                                                                        |
| Helpers: `PagedResult::collect_all` / `into_stream` | Unit: 12 cases<br>Live: `test_pagination` (4)                                                              | covered                                                                        |
| gRPC: `ProcessWatcher` subscribe/wait/unsubscribe   | Unit: reducer and space correlation (4)<br>Live: Markdown import and single-subscription fallback          | covered; connection faults tracked by `any-vvue` (P2), blocked by `any-k6o5.6` |
| Harness: disposable-space lifecycle                 | Unit: lease, ledger, sweep, readiness, and credential suites<br>Live: every fixture-heavy target           | covered                                                                        |

## Supplemental any-mcp transport coverage

This section records cross-crate transport evidence because every MCP workflow
ultimately exercises the `anytype` client, but it does not change the direct
API status rows above. The stdio suite is the deeper end-to-end protocol suite.
The HTTP interface is nevertheless substantially automated:

- The current library inventory has 77 HTTP-specific tests across exact
  environment parsing, owner-private static tokens, OAuth metadata and JWKS,
  Host/Origin/CORS gates, authentication ordering, session ownership and
  deletion, protocol negotiation, preview behavior, listener admission, and
  real-loopback process and load/fault cases. The portable workflow runs this
  library suite on every supported CI platform; literal per-platform
  `--all-targets` and clippy evidence reconciliation is `any-ucd.4` (P2).
- Real-socket stable tests cover authentication, CORS preflight, initialize and
  initialized lifecycle, POST SSE responses, exact `tools/list` catalog parity,
  opening the standalone GET SSE stream, and session DELETE. These tests run
  the production listener and backend in-process. The `anyr` crate's
  `mcp_streamable_http` suite additionally spawns the shipped `anyr mcp`
  command in HTTP mode against a scripted upstream and drives the complete
  authenticated stable lifecycle plus the preview JSON sentinel across the
  command boundary, with bounded waits, empty stdout, fixed diagnostics, and
  token/session/body non-disclosure (`any-2c9n`, closed).
- Stream contract tests consume live frames from the standalone GET stream
  (priming, repeated keep-alives, disconnect, resume, termination on DELETE)
  and prove the exact `rmcp` 2.2.0 `Last-Event-ID` behavior: an in-flight
  POST response stream resumes and delivers its response once without
  redispatching upstream, while completed, unknown, and malformed IDs yield an
  empty successful stream (`any-ddpp`, closed).
- Load/fault tests cover session, rate, concurrency, and body ceilings; the
  exact 2 MiB boundary and streamed chunked overflow; idle SSE disconnect;
  per-connection slow-reader backpressure against an application-generated
  incremental event stream whose generation provably stalls and resumes;
  drain-then-cancel shutdown; and an abrupt disconnect during mutation
  followed by a safe keyed retry.
- Spawned-process coverage now sends `SIGINT` to stable stdio and HTTP servers
  and `SIGTERM` to preview stdio and HTTP servers. Stdio is initialized before
  the signal. HTTP must bind and return the expected unauthenticated response
  before the signal. All four cases require a bounded successful exit, fixed
  stopping diagnostics, empty or protocol-pure stdout, and redacted secrets.

Recorded browser and reverse-proxy smoke runs remain the only evidence for
unbuffered TLS proxying and the production 15-second keep-alive interval; the
recurring stream tests observe keep-alives through a shorter test-only interval
seam on the same production listener and backend. The preview HTTP mode is
intentionally stateless JSON and rejects GET and DELETE, so the SSE contract
does not apply to preview mode.

## Delivered coverage work

Epic any-dm9k and all 18 descendants closed on 2026-08-08. The campaign
delivered required backup/restore smoke coverage, cleanup-owned fidelity
matrices, repaired CLI output contracts, backup-before-delete archive
selection, exact ignored-test dispositions, and protected serial live gates
for anytype-api, any-mcp, and anyr.

The anytype-api manifest now owns 20 required cases, three scheduled
characterization cases, and two explicitly excluded probes. Seven superseded
filter ignores were removed. The former ambient Set/view probes now create
cleanup-owned source-backed Sets and collections. These cross-crate gates are
meaningful integration evidence, but they do not erase a direct crate-level
gap unless they exercise the same public operation and assertions.

### Changes since `kmnyrwzy` / `4d76af1e`

The reviewed jj range adds four material coverage capabilities to this
inventory. Typed numeric, checkbox, and positive type filters on
`objects(..).list` now route through scoped search with exact unit wire checks
and a cleanup-owned 13-case compatibility matrix. The disposable harness now
has a durable recovery ledger, Windows owner/ACL verification, bounded child
credential propagation, and env-only credentials derived from the validated
Anytype CLI account. Clean-server space administration uses evidence-based
readiness and retries only Heart's definitive pre-admission `NO_SUCH_SPACE`
result. The required and scheduled live tiers now emit closed manifest evidence
and have passed on disposable Anytype servers after these changes.

Tracker reconciliation for tickets closed after the baseline:

- The direct API gap owners closed for authentication (`any-fzsd`), paged
  lookups (`any-kmji`), bounded archive counts (`any-h94e`), legacy gRPC file
  download (`any-gb01`), URL upload (`any-uvck`), direct chat reads and edit
  (`any-5vbi`), selective unsubscribe (`any-n1lm`), search/filter wire shapes
  (`any-vnj5`), and second-view continuation (`any-v06h`).
- Filter and clean-server qualification closed `any-bbyk`, `any-inki`,
  `any-cdjm`, and `any-q7hg`. Env-only live admission closed `any-09uo` after
  38 direct, 30 stdio, and one discussion live cases passed. The protected anyr
  file-operation gate closed `any-g040` and remains the cross-crate evidence
  for rich file list/search/get/preload/discard operations.
- MCP process and platform qualification closed `any-b6km`, `any-6vnk`,
  `any-5dsp`, `any-wnur`, and `any-zeyc`: graceful stdio signals, deterministic
  cancellation/cleanup, bounded modern stdio, all five portable platform rows,
  and both Windows architectures are covered. The new HTTP signal tests extend
  that process contract but are not the basis of those prior closures.

The current Arch-only recurrence in
`chat_delete_toolset::tests::handler_verification_cancellation_and_deadline_are_indeterminate`
is not treated as closed evidence here. `any-5g7w` remains in progress until
the local deterministic pending-future repair is rerun remotely on Arch.

## Gap-ticket disposition

Every current partial or uncovered matrix row and every supplemental transport
gap has an unresolved tracker owner. Existing owners were reused where their
accepted scope already covered the gap; six missing owners were added in this
review.

| Priority | Coverage owner            | Gap                                                                                                                                 |
| -------- | ------------------------- | ----------------------------------------------------------------------------------------------------------------------------------- |
| P2       | `any-e7ei.3`              | Verified update-space omission, replacement, and clearing behavior.                                                                 |
| P2       | `any-vvue`                | Real-server `ProcessWatcher` reconnect and fault coverage, blocked by the P4 `any-k6o5.4`–`any-k6o5.6` design/review/harness chain. |
| P2       | `any-ucd.4`               | Literal all-target and clippy evidence on all five supported platform rows.                                                         |
| P3       | `any-upsa`                | Direct assertions on the public HTTP metrics snapshot.                                                                              |
| P3       | `any-ih7t`                | Direct `send_text`, `toggle_reaction`, and `read_all` coverage.                                                                     |
| P3       | `any-vjj`                 | Direct `delete_all_archived` live coverage.                                                                                         |
| P3       | `any-gz2k`                | Real-server negative tag-filter pagination.                                                                                         |
| P4       | `any-k6o5.4`–`any-k6o5.6` | Reviewed external fault injection and chat-stream reconnect coverage.                                                               |

Completed owners retained for historical reconciliation include auth
(`any-fzsd`), paged lookups (`any-kmji`), bounded archive counts (`any-h94e`),
legacy file download (`any-gb01`), URL upload (`any-uvck`), direct chat
reads/edit (`any-5vbi`), selective unsubscribe (`any-n1lm`), search/filter wire
shapes (`any-vnj5`), and second-view continuation (`any-v06h`).

No separate ticket is needed for `backup_space`, protected anyr file
list/search/get/preload/discard evidence, delegating legacy REST aliases, or
rich-option upload. Production view creation remains an API capability
constraint rather than a test gap.

## Scripted test-helper tooling

The feature-gated scripted-HTTP fixture was delivered by `any-dx9w`. It
captures bounded methods, paths, bodies, and finite response sequences; auth,
lookup, and search/filter coverage now consume that boundary.

Do not file the proposed Markdown-driven surface inventory check: it cannot
soundly enumerate builder-returning public operations and would create a
brittle second source of truth. Do not file a separate fixture-registry p2;
the remaining archived-object builder belongs to `any-vjj`.

## Known fixture constraints

- Space deletion: RESOLVED production `delete_space` plus the test-only
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
