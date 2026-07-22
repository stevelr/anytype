# Changelog

All notable changes to this project will be documented in this file.

The format is based on Keep a Changelog, and this project adheres to Semantic Versioning.

## [Unreleased]

### Added

- Add canonical manual-collection membership pages independent of saved views,
  filters, and Kanban presentation. Exact collection REST identity, preserved
  native collection order with no ineffective `id` sort, 1..61 public pages,
  a 62-row internal overlap, client-owned finite
  Heart subscriptions, strict echoed IDs/counters/dependencies, and bounded
  cleanup make empty, terminal, continued, malformed, and concurrent-shift
  evidence explicit. Results expose only exact scoped object IDs and verified
  continuation state; exact total/offset/row arithmetic determines continuation
  because real Heart offset windows leave both relative counters at zero.
  The page contract accepts exact 256-byte safe entity-ID boundaries, records
  one logical GET with the shared six-attempt ceiling, uses one non-replayed
  subscribe plus one foreground unsubscribe, permits only one cleanup fallback,
  and preserves typed secret-safe gRPC authentication failures. Set/query
  objects fail before subscription.
- Add evidence-backed REST chat prerequisites for bounded MCP workflows:
  message timestamp conversion is fallible and canonical UTC milliseconds,
  older-history pages preserve server order behind a 256-byte opaque
  before-anchor and 12-item ceiling, and verified text/format edits require an
  independent exact readback whose `modified_at` strictly advances. Scripted
  transport and prefix-authorized disposable-server coverage lock routes,
  terminal pagination, insertion stability, typed failures, and cleanup.
- Add a bounded direct collection-membership observation that exact-checks
  space, collection, and object identity, rejects Set/query lists, and combines
  a canonical collection-scoped Heart query with independent unscoped index
  controls before and after absence. Unique client-owned subscription IDs,
  finite RPC deadlines, and cancellation-resilient cleanup protect Heart's
  app-global subscription registry. Only complete exact evidence returns
  `present` or `absent`; view filters, pagination, malformed counters, and
  incomplete index evidence cannot manufacture an absence result.

### Fixed

- Refresh the live mutation retry inventory for the seven bounded fixture
  setup retries now owned by `test_util`, restoring the full integration gate.
- Member reads now accept the REST API's bounded `_participant` IDs and
  network identities as safe URL path segments instead of incorrectly
  requiring every member reference to have object-CID shape. The fixed
  endpoint contract accepts 1 through 256 URL-unreserved bytes.
- Body reader integration coverage now uses fresh prefix-authorized disposable
  spaces and cleanup-owned real-server objects for typed conversion, exact
  order, close-safe repeat reads, tightened limits, and missing-object failures
  instead of the custom gRPC mock. The adjacent dataview case shares the
  ignored disposable tier so an empty ambient inventory remains valid. The
  exact accepted-show close policy remains covered by a transport-independent
  unit test.
- Process watcher import-finish fallback events are now correlated to the
  requested space, including an explicit opt-in for legacy events with an empty
  space ID. Real-server Markdown import coverage replaces the custom gRPC mock;
  the same subscription observes the ordinary import lifecycle and the
  subsequent import-finish fallback before cleanup.
- Chat resolver integration coverage now uses fresh prefix-authorized
  disposable spaces with cleanup-owned real-server chats and immediately
  registered messages across HTTP and gRPC instead of the deprecated custom
  gRPC mock. Supporting REST reads and the REST SSE case share the ignored
  serial tier; broader pre-existing REST workflows remain explicitly ambient.
  The gRPC stream worker shuts down before cleanup.
- file/SQLite keystore modifier parsing now recognizes only `:key=`
  boundaries, preserving Windows drive paths and ordinary colon-bearing path
  values while retaining the established last-wins behavior for duplicate
  modifiers
- HTTP diagnostics now emit only structured variant/status/method/path metadata:
  request and response payloads, query values, headers, credentials, and full
  URLs remain unavailable at every trace level. Malformed and unsupported
  request targets fail closed to a fixed marker. Standard error/config debug
  output and transport error source chains apply the same redaction policy.
- REST chat SSE streams now enforce a configurable finite per-event buffer
  ceiling incrementally with checked arithmetic before growth and constant-work
  delimiter detection per byte. Transport chunks are not copied into that
  buffer and may carry multiple bounded events. Exact limits and split
  delimiters remain valid; delimiter-free or one-over streams terminate with a
  typed secret-safe error and release the buffer allocation. Stream path IDs
  are validated before URL construction or logging. Transport failures retain
  only the response path and discard raw reqwest errors so Display, Debug, and
  source chains cannot expose URL userinfo, queries, fragments, tokens, or
  upstream bodies.
- live integration-test setup mutations now retry only typed, definitive HTTP
  429 rejections through one finite test-only seam; transport, timeout, 5xx,
  validation, and other indeterminate failures remain single-attempt, and the
  audited validation/optional-success/mutation-contract cases are unchanged
- direct property-ID reads now offer a metadata-only, cache-independent,
  exact-identity scoped GET that never expands tags; explicit-ID tag lookup
  follows it with a separately paginated 1,000-row scan, validates a stable
  coherent total and complete page windows before accepting a match or
  not-found result, and fails explicitly when completeness exceeds that bound,
  so a cold cache no longer primes every property or collects unbounded tag
  options
- explicit type-ID metadata resolution now performs one cache-independent
  scoped GET and rejects mismatched returned type identities, avoiding an
  unbounded all-types cache prime in bounded protocol consumers
- automatic HTTP retries now apply only to methods already classified as
  replay-safe; `POST` and `PATCH` mutations return 429, timeout-status, server,
  and transport failures after exactly one send instead of replaying a write;
  reqwest redirects and lower-level retry policies are overridden so they
  cannot bypass method safety, leak credentials across redirects, or skew
  request-attempt metrics; replay-safe operations now share one cumulative
  six-physical-attempt ceiling across 429, retryable-status, connection, and
  timeout failures, with independent logical-operation and physical-attempt
  metrics
- examples and integration-test helpers now satisfy workspace rustdoc and lint
  checks without needless borrows
- property updates now reject key-only requests before sending them because the
  REST API requires `name`; type updates now expose full property replacement
  and explicit clearing while preserving omission when properties are unchanged
- view_list_objects() requires setting `.view(view_id)` before invoking `.list()`
  and rejects unsafe selected view path identifiers before building the URL.

- integration-test clients now honor `ANYTYPE_KEYSTORE` instead of always using
  a separate default test database without the configured HTTP credentials
- chat event streams continue polling when callers keep the event stream but
  drop the optional control handle, avoiding reconnect spins and missed events
- integration tests accept the current missing-token authentication error, and
  known-broken boolean/numeric filter cases reference
  [anytype-heart#2879](https://github.com/anyproto/anytype-heart/issues/2879)
- Set view integration cases that require a preconfigured internal `set_of`
  source are ignored in environments where REST cannot create that fixture

### Removed

- Removed the custom semantic gRPC mock module and executable after moving its
  ordinary body, chat, resolver, and process-watcher consumers to cleanup-owned
  real-server coverage. The mock-only chat disconnect test is also removed;
  replacement transport-fault coverage remains tracked behind the reviewed
  external fault-injection plan.

### Added

- Add an ignored serial disposable-space Markdown fidelity matrix covering
  headings, lists, checkboxes, one-line and consecutive-line quotes, fenced
  code, links, tables, Unicode, underscores, escapes, and multiline bodies.
  Stable REST exports and fresh `ObjectShow` identity/type/order evidence lock
  byte-identical replay for seven representative cohorts, unsupported drift
  for consecutive quotes, fenced code, tables, underscores, and escapes, and
  the absence of any block-ID preservation promise.
- Add a cache-independent type-property classification read that reconciles
  the flattened REST type representation with Heart's separate featured and
  ordinary recommended source lists. It exposes the exact non-featured set
  replaced or cleared by type updates, preserves exact featured IDs even when
  REST omits a system definition, caps source evidence at 1,000 links, and
  fails closed on malformed or cross-transport-inconsistent evidence.
- Add verified typed body-block mutations through `BodySnapshot::edit`:
  single-block create/append, exact-field update, delete, reorder, and bounded
  sequential batches all require fresh `ObjectShow` evidence before reporting
  success. Constructors cover paragraphs, headings, bullets, numbered lists,
  checkboxes, toggles, callouts, quotes, code, dividers, tables, link/relation
  cards, bookmarks, LaTeX, Mermaid, YouTube, colors, alignment, and
  backgrounds, including complete divider-style and link-card appearance
  updates with bounded relation keys. Wrong-context, restricted, system, file,
  table-structural, and otherwise unsafe targets fail locally. Every write RPC
  is dispatched exactly once; timeout, cancellation, connection loss,
  shutdown, malformed/oversized replies, unknown response codes, and unproved
  writes return secret-safe receipt-bearing indeterminate errors with the last
  complete observed snapshot when available. Table receipts require the exact
  ordered columns/rows layout topology, direct row/column membership, and
  first-row header state; system text, featured-relation, file, and structural
  blocks are rejected consistently as targets, target parents, and computed
  first-child anchors.
  Bookmark previews are never fetched, providing an explicit SSRF-safe
  networking policy, while YouTube URLs use a strict HTTPS host/ID allowlist.
- REST file request builders now support caller-specific success/error body
  limits, bounded allowlisted-header evidence, checked byte ranges, and one
  cumulative physical-attempt ceiling. File metadata and validators are
  validated without changing global defaults; oversized, truncated,
  duplicate, malformed, contradictory, or over-retried responses fail closed
  with typed secret-safe errors. Each physical response, including an
  intermediate retry, is checked against the allowlisted-header evidence
  ceiling before retry or body processing.
- Add `with_disposable_space_context` for fixture-heavy live suites. A required
  `ANYTYPE_TEST_SPACE_PREFIX` explicitly authorizes case-insensitive cleanup of
  every matching current space name. The helper validates the prefix before
  credential or filesystem access and returns a typed skip, acquires a
  backend-wide process lease, writes a durable owner-private recovery ledger,
  sweeps interrupted matching runs with deadline-bounded, SQLite-backed,
  enumerate-before-delete fixed-window pagination, creates a 128-bit random
  name,
  uses cache-disabled exact-ID readiness/deletion/absence reads, and preserves
  callback, child-cleanup, deletion, absence, and harness-state outcomes under
  returned errors and panics. Disposable runs require the explicitly selected
  environment keystore, an operator-selected service, and complete HTTP plus
  gRPC credentials. The parent captures an exact bounded credential set and
  exposes a child-command configurator that starts from `env_clear`; file,
  keyring, implicit, unknown, or over-budget stores skip before authentication
  or mutation and no credential snapshot file is created. Registered
  idempotent child stoppers run before resource cleanup, while durable
  running/stopped state prevents final ledger cleanup if a child may survive.
  Prior Running-child ledgers now block plan application and all final prefix
  sweeping, durably request stopped-or-gone operator proof, and accept one
  exact-ledger `ANYTYPE_DISPOSABLE_RECOVER_STOPPED_RUN` transition before
  recovery. Dominant cleanup categories retain the original typed error and
  every earlier outcome.
  Unix recovery uses no-follow directory-relative opens and unlinks; unproved
  process or Windows ACL isolation fails before credential capture or mutation.
- Add `objects::plain_markdown_representation`, a documented closed contract
  that separates the safe Anytype write form from the exact canonical read form
  for empty bodies and single plain lines containing alphanumeric characters,
  spaces, and underscores. Canonical replay is idempotent; ambiguous Markdown
  and backslash forms are rejected instead of broadly unescaped.
- Typed, bounded, fail-closed body-block reads: `AnytypeClient::blocks()` and
  the new public `body` module (re-exported through the prelude) expose an
  object's rich body as a validated `BodySnapshot`/`BodyBlock` tree with exact
  identities and child order, read via gRPC `ObjectShow` with a best-effort
  `ObjectClose` after every successful show. Content kinds, text styles, and
  marks are closed v1 enums; anything else reads as an explicit
  `BlockContent::Unsupported` marker with a content-free summary. Per-request
  `BodyLimits` clamp to hard ceilings and can only tighten. Malformed,
  duplicate, cyclic, dangling, or oversized graphs fail the whole read with
  the new `AnytypeError::BodyGraph` variant, whose display and `detail` carry
  only block IDs and structural counts — never block text, URLs, or tokens —
  and a partial or truncated snapshot is never returned.
- `AnytypeError::is_authentication()` now exposes a secret-safe,
  `anytype-api`-level classification for direct and nested gRPC authentication
  failures without requiring callers to depend on `anytype-rpc` or format
  source diagnostics
- test contexts now provide cleanup-owned custom types, source objects, and
  templates through `create_template_fixtures`: the helper takes finite,
  complete pre-create inventories of all types, space-wide active and archived
  objects, and every active type's templates, sends one authenticated heart
  `TemplateCreateFromObject` request per source, registers every validated ID
  before response classification or follow-up reads, and verifies exact
  list/GET evidence. Teardown sends each mutation once in reverse dependency
  order and proves every template absent and every source/type archived. One
  shared ID registry prevents generic and private cleanup paths from dispatching
  the same ID twice
- Test contexts now provide a cleanup-registered collection-layout type fixture
  through the narrow heart RPC, with immediate safe-id registration and bounded
  exact-layout verification through the ordinary REST getter; production REST
  `TypeLayout` remains restricted to the four layouts the server accepts.
- Add privately proven collection-object and second-view test fixtures. The
  object helper accepts only a context-owned collection type, snapshots its
  exact existing object IDs, and binds the create result to active
  space/type/layout identity. It atomically claims the authoritative ID, exact
  object cleanup entry, and private `(space, object, type)` provenance; generic
  or private collisions leave all three registries unchanged, and ordinary
  cleanup registration cannot authorize view mutation. The view helper first
  requires the REST object to retain the exact proven type ID, then
  cross-checks every REST-visible field against the exact `ObjectShow` root and
  dataview block, copies the complete proto,
  sends one authenticated create RPC, requires one exact full-view event and a
  distinct server-assigned ID, and finitely verifies the complete two-view REST
  result. Returned view IDs are now privately registered immediately,
  before response/event classification, while collection deletion remains the
  sole cleanup operation for those nested views.
- Add cleanup-owned representative Kanban fixtures for disposable real-server
  tests. The helper registers its custom card and collection types, Select
  property, options, view, and cards before follow-up reads; adds and verifies
  the exact Heart-internal grouping relation independently from the REST
  property key; forces membership reads across two-item pages; and proves column
  movement through an ordinary property update. Verification
  fails closed for missing or wrong-format grouping relations, deleted options,
  filtered views, malformed pagination, identity collisions, or incomplete
  cleanup provenance.
- Add a test-only disposable-space lifecycle that registers validated REST
  create ID/name/model provenance before verification only after a strict
  complete pre-create inventory proves the ID is neither current nor
  pre-existing. It revalidates that exact provenance before the irreversible
  exact-ID `SpaceDelete` RPC, reconciles uncertain delete responses through
  strict bounded absence, and structurally deduplicates its private registry.
  No production space-delete API is exposed; malformed, mismatched, concurrent,
  or ambiguous evidence favors a leak over deleting existing state.
- object requests now offer `delete_once()` for soft-delete workflows that
  must reconcile an uncertain response without middleware replaying `DELETE`
- bounded predicate-based semantic read-after-write verification with finite
  timeout and validated attempt limits, retry of successful-but-stale values,
  cancellation-safe backoff, and secret-safe terminal classifications
- finite, incrementally enforced HTTP response ceilings for generic JSON,
  document JSON, bounded error bodies, and separately governed raw file
  downloads; object reads support a per-request ceiling within the configured
  document allowance, and oversized responses return the secret-safe
  `ResponseTooLarge` error

- REST file download/delete APIs (`download_bytes`, `delete`) and unified file
  upload selection: simple path/byte uploads use REST, while URL uploads and
  uploads with style, details, or creation context retain the richer gRPC path
- configurable REST file requests with image widths, `HEAD` metadata, byte
  ranges, conditional headers and preserved HTTP control statuses, plus
  permanent deletion through the `skip_bin` option
- `FilePreloadRequest::from_url` builds a preload request from a remote URL,
  complementing the existing `from_path`; the unified upload path routes URL
  preloads through the gRPC backend
- space-scoped REST chat APIs for chat listing/creation, plain message listing,
  single-message lookup, message search, deletion, reactions, and read state
- direct REST chat message add/edit builders, dynamic filters for chat listings,
  and typed SSE message streams with configurable initial-message limits and
  heartbeat intervals
- structured gRPC chat message blocks, pin state, unread-reaction state, and
  attachment replacement for rich message publishing and full-fidelity reads
- new `resolve` module: name and id resolution helpers as `AnytypeClient` methods —
  `resolve_space_id`, `resolve_type`, `resolve_type_id`, `resolve_type_ids`,
  `resolve_type_key`, `resolve_template`, `resolve_view_id`, `resolve_property_id`, `resolve_chat_target`
  (returns the new `ChatTarget` struct), `resolve_chat_ids`, `resolve_chat_name`,
  `resolve_message_id`, and `resolve_message_ids` (resolve a chat message id or
  `order_id` into a message id, and the batch form).
  Moved from the anyr cli so all clients share the same "name or id" conventions.
  `ChatTarget` and `DEFAULT_CHAT_NAME` are exported in the prelude.
- new error variant `AnytypeError::Ambiguous`, returned by the `resolve_*` helpers
  when a space, type, chat, or view name matches more than one item; ambiguity
  errors now include up to 10 deterministic, deduplicated candidate ids and
  display names through the new `ResolveCandidate` type; resolver scans use a
  hard 1,000-row limit and return `ResolutionLimitExceeded` rather than a
  possibly false unique/not-found result, preserve direct view-id priority,
  and select candidates independently of upstream row order; safe duplicate
  representatives take precedence over malformed alternatives with the same id
- bounded template resolution with a direct-id fast path, exact-id precedence,
  archived exclusion, deterministic stable-id candidate deduplication, checked
  sparse pagination, fail-closed accounting for malformed matching rows with
  safe same-id representative recovery, and a final GET that verifies space,
  canonical generic template type id/key, archive, and selected identity while
  the validated endpoint path establishes the owning object type

### Changed

- Behaviorally observable error formatting change: `AnytypeError` `Display`,
  `Debug`, and its standard error source chain now omit all free-form
  identities, messages, last errors, and typed upstream sources that could
  contain request or document content. Raw public fields remain available
  through explicit variant matching; callers that parsed human-readable error
  strings or traversed sources must switch to variants, fields, or
  `AnytypeError::diagnostic()`.
- Normalized troubleshooting examples and repository configuration formatting.
- `list_chats_in` now uses REST; cross-space chat discovery, structured message
  publishing/full-fidelity reads, and reconnecting multi-chat subscriptions
  continue to use gRPC
- chat discovery, CRUD, REST overlap, and normal streaming tests now exercise
  the configured real server; disconnect/reconnect fault coverage was removed
  with the semantic mock and remains deferred to the reviewed external
  fault-injection work
- successful REST mutation endpoints may return an empty body, which is now
  handled as a unit response
- removed skia as dependency (was used to generate image file for the files example)
- files example requires setting path and file type to path and type of existing
  local files, instead of generating them locally

- bumped dependencies: zbus-secret-service-keyring-store from 0.2.2 to 0.3.0

## [0.3.2]

### Added

- new helper `client::find_grpc(program)` to discover a local Anytype gRPC port by scanning listeners for a process prefix and probing candidate ports.

## [0.3.1] - anytype - 2026-02-16

### Added

- new function `backup_space()` to export any space, format: Markdown, Protobuf, or Json; with/without Files, and other options.
- file upload/preload request options: `created_in_context` and `created_in_context_ref`
- chat message text styles: `toggle_header1`, `toggle_header2`, `toggle_header3`
- new gRPC `process_watcher` module for reusable process lifecycle tracking (subscribe/wait/reconnect/unsubscribe), with cancellation-channel support and configurable timeouts/fallbacks.
- archived object management APIs on `AnytypeClient`:
  - `list_archived(space_id)` builder with `limit`, `offset`, and `types` filters.
  - `count_archived(space_id)` to count archived objects.
  - `delete_archived(space_id, &[String])` to hard-delete archived objects in gRPC batches of 200.
  - `delete_all_archived(space_id)` to delete all archived objects by paging archived IDs and deleting in repeated batches (200 per delete request) with settle delay and progress debug logs.

### Changed

- bumped anytype-rpc to 0.3.0
- removed generate-markdown example

## [0.3.0] - anytype - 2026-01-28

Major update:

- adds gRPC backend for Files and Chats.
- Refactored keystore to use db-keystore (sqlite) for file-based keystore

### Added

- `take_items()` on `PaginatedResult<T>`
- gRPC files module with list/search/get/upload/download/preload support.
- gRPC file list/search filters for name, extension, size, and file type.
- gRPC file downloads now support explicit destination file paths via `to_file()` (and `to_dir()` alias).
- gRPC chat streaming API with subscription control, reconnect, and preview support.
- chat message send with helpers for text marks
- functions to generate web links: `Object::get_link`, `Object::get_link_shared`, and `objects::object_link`, `objects::object_link_shared`
- new example: [agenda](./examples/agenda.rs) - Collect top-10 tasks (sorted by date modified and priority) and recent documents, and send in a chat message.

### Changed

- simplified KeyStore implementation leveraging new keyring_core apis.
  - KeyStoreFile replaced by db-keystore::DbKeyStore. Uses local sqlite file (turso rust-native implementation), with optional encryption. Default key store is still OS keyring.
- gRPC feature is enabled by default; disable with `default-features = false` if you only need REST.
- Apache-2.0 license
- bumped dependencies (markdown2pdf -> 0.2.1)

### BREAKING

- Build changes
  - protoc and libgit2 must be installed for build from source or cargo install
- ClientConfig::base_url changed from String to Option<String>
- Changes to authentication apis
  - is_authenticated() replaced with auth_status().http.is_authenticated() and auth_status().grpc.is_authenticated().
  - keystore is now configured in ClientConfig. set_key_store() and load_key() no longer needed.
  - If using file-based keystore, default path is ~/.local/state/keystore.db
  - removed SecretApiKey

## [0.2.9] - anytype - 2026-01-17

### Added

- Documentation (README.md): listed limitations of the rest api
- Optional feature flags to select os keystore flavor on linux

### Changed

- clippy fixes

## [0.2.8] - anytype - 2026-01-12

### Changed

- Switch reqwest to rustls with native roots to avoid OpenSSL build-time dependencies.

## [0.2.7] - anytype - 2026-01-12

### Changed

- BREAKING: `Property.as_date()` return type was `Option<&str>`, now `Option<DateTime<FixedOffset>>`, to match `Object.get_property_date()`.

## [0.2.5] - anytype 2026-01-10

### Added

- Initial GitHub release.
