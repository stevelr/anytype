# Changelog

All notable changes to this project will be documented in this file.

The format is based on Keep a Changelog, and this project adheres to Semantic Versioning.

## [Unreleased]

### Added

- Add the production-unlinked `chat_message_delete` mutation slice through
  `anytype-api` REST only. Its closed input requires an exact space, chat,
  message, canonical UTC-millisecond `expected_modified_at`, and the literal
  `delete_message` confirmation. One exact preflight binds identity and the
  advisory timestamp before exactly one non-replayed DELETE. Success requires
  bounded authoritative absence, with every verification read capped by the
  remaining three-second verification and common request budgets;
  accepted-but-unverified and uncertain
  dispatches remain mutation-indeterminate, including uncertain-plus-absent.
  Direct-router validation and read-only checks perform no HTTP. Pure
  state-machine and actual handler-control tests prove accepted, rejected,
  permission, not-found, cancellation, deadline, bounded, and single-dispatch
  behavior without a mock server, and disposable real-server
  acceptance covers a stale edit sentinel, direct deletion, a test-owned
  spawned stdio process, exact absence, and registered cleanup. Transport
  faults and latency remain deferred to the P4 fault-injection design.
- Link the complete default-off `views-write` production registry. Read-write
  mode exposes exactly `collection_member_list`, `collection_member_add`, and
  `collection_member_remove`; read-only mode retains only the list workflow.
  The immutable descriptor requires authenticated HTTP and gRPC through the
  shared `anytype-api` client, owns the reviewed direct/stdio/headless
  scenarios, preserves default Phase 1 discovery byte-for-byte, and keeps the
  approved 3,000-domain/3,500-selected catalog ceilings and 61-item maximum
  result snapshot. The shared disposable A/B/C scenario covers direct,
  stable-stdio, and preview-stdio parity, canonical pagination, saved-view
  independence, no-op writes, cursor/query rejection, concurrency, cleanup,
  and exact work counters without a mock or fault server. Genuine viewer 403
  mutation evidence remains externally blocked because the available
  read-only fixture is not cleanup-safe to mutate; the fixed classifier and
  zero-I/O parity tests remain explicit non-substitutes.
- Design a separate default-off `discussions` registry with one read-only
  `object_discussion_get` workflow. The closed result distinguishes normal
  absence from an attached `discussion_id`, binds the exact space and Basic or
  Note parent, verifies the derived Heart discussion type and deterministic
  parent unique key through a typed `anytype-api` primitive, and exposes no
  comments or message metadata. The design keeps the approved six-tool `chats`
  contracts byte-identical and defers MCP attachment because Heart has no
  detach pair, raw repeat returns a generic error, and raw attach resolves only
  by object ID. Direct and spawned-stdio acceptance uses cleanup-owned real
  spaces; the operator-supplied read-only `Page One` fixture is optional
  corroboration, never mutated. Fault injection remains P4.
- Add the production-unlinked `chat_message_add` mutation slice through
  `anytype-api` REST only. The strict workflow accepts 1..8,192-scalar plain
  paragraph text, an optional exact reply target, and a required process-local
  idempotency key. It resolves scope before cohort admission, preflights a
  reply by exact GET without projecting its unreturned text, sends at most one
  non-replayed POST, validates the server-assigned ID, and requires one exact
  readback of text, paragraph, marks, attachments, and reply identity.
  Concurrent identical callers share the leader result without another GET or
  POST; changed normalized input conflicts; later successful replay performs
  one fresh exact GET and never compares mutable presentation or content.
  Definitive POST rejection and post-dispatch uncertainty before an assigned
  ID remain terminal for the key. Once POST returns a valid assigned ID, the
  candidate is retained before verification; ordinary not-found,
  authentication/permission, bounded-result, upstream GET, timeout, and
  cancellation outcomes remain safe for later exact-GET-only retry and can
  never dispatch a second POST. Direct-router and persistent preview-stdio
  acceptance use a cleanup-owned disposable real chat, prove exact leader,
  replay, conflict, missing-reply retry, retained-capacity, reply leader, and
  reply replay work, register every message immediately, and leave no
  prefix-owned space. Concurrent cohort admission and completion run through
  the actual router with a test-only deterministic admission gate, proving one
  POST, one leader GET, zero waiter GETs, and identical returned detail without
  timing or latency injection. A test-owned child-process stdio harness covers
  the exact reviewed registry, while a second spawned child proves the shipped
  composition still rejects the production-unlinked tool. One absolute
  deadline now covers resolution, admission, detached leader execution,
  verification, and the earlier of each waiter or leader deadline. Fixed
  catalog/result token evidence plus exact direct/preview rejection,
  pre-cancellation, terminal, retained-capacity, and retryable reply-preflight
  tests require no fault or mock server. Boxed parity scenarios also lock the
  ordinary 2-MiB test-stack regression. Fault and latency injection remains
  deferred to the P4 fault-injection design.
- Link the complete default-off `schema` production registry. Read-write mode
  exposes exactly `space_create`, `space_update`, `type_get`, `type_create`,
  `type_update`, `property_create`, `property_update`, `tag_create`, and
  `tag_update`; read-only mode retains only `type_get`, with common
  `optional_toolset_status` counted once. The descriptor composes the reviewed
  slices through one per-runtime handler cohort, requires authenticated HTTP
  and gRPC through `anytype-api`, preserves byte-identical no-selection Phase 1
  catalogs/status, and rejects absent or stale mutation calls before decoding
  or I/O. Actual composed `o200k_base` snapshots lock 7,856 schema-domain and
  8,112 schema-selected contribution tokens beneath the 9,500/10,000 ceilings,
  plus all profile/access/mixed totals and per-tool maximum representative
  results. Aggregate dispatch and success-path mutation boundaries use
  heap-owned futures so the complete registry stays within the default worker
  stack. Direct routing and one spawned production-stdio disposable workflow
  cover the exact nine-tool inventory and cleanup without mock or fault
  servers; independent property-scoped API reads verify the created and updated
  tag IDs, names, colors, and ownership rather than trusting MCP output alone.
- Add the `collection_member_list`, `collection_member_add`, and
  `collection_member_remove` workflows that comprise the production
  `views-write` registry. The strict list tool resolves one space, binds an
  opaque cursor to the exact resolved space, collection, limit, registry, and
  operation, and delegates one page to `anytype-api`'s canonical direct
  membership primitive. Results expose only ordered `object_id` values, cap
  both schema and runtime at 61, preserve verified total and overlap-boundary
  evidence in process-local cursor state, and fail closed on identity,
  counter, duplicate, shift, or terminal-arithmetic inconsistencies. The
  locked maximum result is 33,650 bytes and 31,770 `o200k_base` tokens; 62 is
  rejected. Direct-router tests and a feature-gated, test-only spawned stdio
  child use an authenticated disposable collection whose saved view hides one
  still-listed member. The child runs the real stdio server path with
  deterministic acceptance seams around the same handlers; it neither replaces
  nor alters the immutable production registry inventory. The two desired-state mutations accept only exact collection and
  object IDs, use the independent canonical observer for preflight and
  verification, return zero-write success when already desired, and otherwise
  dispatch one non-replayed POST or one logical replay-safe DELETE. Successful
  writes require a complete observation within ten attempts; post-dispatch
  cancellation, malformed success, retry-admission status, transport failure,
  or incomplete evidence returns fixed reread guidance without redispatch.
  Stable-ID list calls prove exactly one logical/physical HTTP GET, one
  canonical membership round, one subscribe, one confirmed foreground close,
  zero cleanup fallbacks, and cursor-mismatch rejection with zero membership
  I/O. One shared actual-router, spawned-stable-stdio, and
  spawned-preview-stdio scenario seeds A, leaves target B absent, and retains
  absent C as a control. It applies list/add/remove to a Set/query object,
  rejects limit and collection cursor rebinding before membership I/O, covers
  add/no-op/remove/no-op and both read-only mutation gates, and checks exact
  A/B/C identity, canonical `limit=1` pagination, saved-view independence,
  object survival, transport parity, and exact logical/physical HTTP, observer,
  query, subscribe, foreground-close, fallback, and write deltas. A
  post-preflight barrier drives two concurrent B adds through the actual direct
  and spawned handlers, then proves a bounded safe outcome, one canonical B,
  and no effect on A or C. The barrier is not a latency or fault server.
  Test-only child modes append payload-free counter snapshots to a private
  file. A separate offline direct/stable/preview process test invokes the
  same production 403 classifier twice and proves authentication mapping,
  parity, no redispatch, and zero HTTP/mutation work; it is explicitly not a
  substitute for genuine live permission denial. Add treats only completed
  400/401/403/404/409/422 POST statuses as
  definitive; redirects, 408/410/425/429, every other 4xx, 5xx, transport, and
  malformed-success outcomes are indeterminate. Token snapshots lock all
  three contributions under stable and preview protocol envelopes and the complete
  61-item result. Deterministic transport-fault cases remain deferred to the
  P4 fault injector. The registry stays unavailable until `any-uda.4`.
  Genuine live collection POST 403 evidence remains unsafe without a
  disposable non-owner collection and owner cleanup; read-only fixtures remain
  untouched and invalid credentials are not treated as permission evidence.
  An earlier live attempt could not enter the scenario because disposable-space
  creation applied but its response never completed; every ledger-named space
  was removed with absence proved. A later run entered the shared scenario and
  exposed debug-build worker stack overflows in add and list, now corrected by
  boxing their operation/executor boundaries. The next run reached stable
  `CancelAddBeforeMark` but its add response timed out with empty child stderr;
  cleanup acknowledged deletion and proved absence. Injected cancellation now
  uses a handler-local token instead of canceling rmcp's response channel. This
  final correction and strengthened shared scenario pass offline gates. Boxing
  the preview dispatcher and optional-registry aggregate keeps the reviewed
  tool future within the default child-worker stack. The authorized
  direct/stable/preview real-server rerun completed every assertion and left
  healthy HTTP/gRPC, an empty disposable prefix, no child or metrics file, and
  no current run ledger. Independent review remains pending.
- Add the production-unlinked four-workflow `chats` read slice through
  `anytype-api` only: bounded chat discovery, opaque older-message pagination,
  exact message reads, and in-chat full-text search. The minimized projections
  expose canonical UTC-millisecond timestamps, Unicode-safe text prefixes and
  counts, formatting/attachment summary flags, and no rich blocks, reactions,
  attachment details, or private REST state. Process-local cursors seal the
  validated upstream history anchor plus page number, bind offset cursors to
  resolved scope/query/limit, reject duplicates and non-progress, and cap one
  history lineage at 64 pages. Pure tests lock schemas, bounds, minimization,
  cursor secrecy, and 32/48-KiB result ceilings; direct-router and preview-stdio
  acceptance uses one cleanup-owned disposable real chat. The slice remains
  absent from production discovery until the complete independently reviewed
  six-tool registry lands; deterministic transport faults remain deferred to
  the P4 fault-injection service.
  Independent review corrections now reject every non-chat upstream layout,
  lock compact/standard and read-write/read-only catalog hashes plus
  adversarial maximum result bytes/tokens in a tokenizer-versioned snapshot,
  including typed exact-limit/plus-one fixtures with four-byte Unicode,
  combining marks, escaped and prompt-injection text. Exact projection-key and
  identifier checks fail closed on returned-ID mismatch and malformed IDs. The
  disposable real-server suite continues and restarts chat, history, and search
  lineages through direct dispatch and one persistent preview-stdio session,
  rejects cursor/limit mismatch before I/O, checks ambiguity and fixed
  redaction, proves one logical HTTP call and at most six physical attempts per
  stable-ID read, and proves exact cleanup.
- Add the production-unlinked schema property slice with exact
  `property_create` and `property_update` contracts. The handlers enforce all
  closed formats, select-only 1..20 tag batches, exact create idempotency,
  cache-independent one-write mutations, semantic update no-ops, direct
  property verification, a single exact terminal tag page, minimized outputs,
  conservative post-dispatch uncertainty, and format/tag preservation.
  Direct and preview-stdio disposable real-server coverage locks primed and
  unprimed cache behavior, exact logical/physical counters, 20/21 boundaries,
  cancellation, authentication, cleanup, and production-unlinked registry
  status; external latency and transport fault cases remain behind the P4
  fault-injection design.
- Amend the approved `views-write` canonical membership-page prerequisite to
  match real Heart offset-window evidence: `total` and exact row arithmetic
  establish terminal versus continued pages, while Heart's relative
  `prev_count` and `next_count` fields must both remain zero. Continuation still
  requires exact echoed subscription IDs, a checked offset, total stability,
  and an ordered overlap boundary; it never treats a zero relative counter as
  terminal by itself. Real collection scopes also ignore an `id` sort, so the
  canonical request now sends no sort and preserves Heart's direct collection
  order without post-sorting; duplicate rows and boundary shifts still fail
  closed.
- Amend the approved schema-toolset design after `any-93eo` with one closed
  `type_update.recommended_properties` desired-state field: omission preserves,
  an explicit empty array clears only replaceable recommendations, and 1..20
  unique-key specifications replace the complete ordered set. The delta locks
  cache-independent featured-property protection, finite deadline-bound
  ObjectShow/ObjectClose ownership, zero-write semantic no-ops, one
  non-replayed update, bounded HTTP/gRPC classification and verification,
  conservative post-dispatch uncertainty, unchanged result/token ceilings,
  and direct, stdio, and disposable-real-server acceptance without a mock.
- Add production-unlinked `space_create` and `space_update` schema-toolset
  workflows through `anytype-api` only. They enforce strict bounded inputs,
  read-only removal, finite process-local create idempotency, exact preflight
  and semantic readback, one-write mutation discipline, redacted indeterminate
  outcomes, and minimized space metadata. Direct and spawned preview-stdio
  acceptance uses cleanup-registered disposable spaces on an authenticated
  real server. Pure tests lock the reviewed 61/133 ceilings; inducing exact
  retry maxima and transport faults remains deferred to the P4 fault injector.
- Add the production-unlinked schema type slice with exact `type_get`,
  `type_create`, and `type_update` contracts. The handlers enforce strict
  20-item property bounds, process-local create idempotency, cache-independent
  preflight, finite owned classification, omission/preserve versus explicit
  ordered replacement/clear, featured-vector stability, semantic no-ops, one
  non-replayed mutation, minimized results, and direct/preview-stdio parity on
  cleanup-owned disposable real-server types while leaving `schema` absent
  from production. Separate exact HTTP and Show/Close/fallback budgets,
  metadata-plus-recommendation parity, read-only/authentication/error cases,
  cancellation cleanup, zero-I/O boundary rejection, and catalog/input/result
  token snapshots replace semantic mocks; transport-fault injection remains
  deferred to the P4 follow-up.
- Add production-unlinked `tag_create` and `tag_update` schema workflows
  through `anytype-api` only. The closed contracts resolve one space and
  1..256-scalar property reference, prove space ownership plus `select` or
  `multi_select` format with a terminal cache-independent property page,
  default create color to grey, require an exact scoped update tag, and return
  only a closed `{ "tag": TagSummary }` envelope. Create uses process-local
  idempotency, both paths send at most one mutation, and terminal property-owned
  tag-page reads verify exact ownership and every requested field while
  `.no_cache_refresh()` prevents a primed property cache from adding hidden
  tag pagination. Direct-router and
  preview-stdio acceptance uses cleanup-owned disposable real-server
  properties, proves exact stable-ID 3/3 create and 4/4 update HTTP work, locks
  the reviewed 34/199 and 35/205 ceilings plus a 5,320-byte/3,381-token maximum
  complete `CallToolResult`, and rejects wrong-format calls before a write.
  Genuine 403 coverage remains externally blocked because the available real
  server exposes only an owner credential. Latency and transport-fault cases
  remain deferred to the P4 fault-injection design, and the incomplete
  `schema` registry stays unavailable.
- Add the default-off production `files` toolset through `anytype-api` only:
  bounded exact-ID metadata and ranged byte reads, strong-validator and HTTP
  evidence reconciliation, native MCP image/audio/text/blob content selected
  by MIME type and negotiated revision, and canonical hash-bound
  `anytype-file://bytes/...` resource reads. Strict schemas, frame and token
  ceilings, pure state-machine coverage, and disposable real-server direct and
  preview-stdio tests lock the contract. `file_upload` accepts at most 64 KiB
  of canonical inline base64, sends exactly one bounded multipart POST,
  retains its candidate ID,
  and proves identity, metadata, complete length, MIME essence, and SHA-256
  before success. Same-key retries reuse verified output or safely reverify a
  retained candidate without another POST. Space names resolve through
  1-MiB-per-page evidence limits; stable IDs skip the scan. The read-write
  registry exposes metadata, read, and upload while read-only mode removes only
  upload; both modes retain the single hash-bound resource template and no
  listed resources. The acceptance matrix includes exact text boundary
  selection, four deterministic 64-KiB byte patterns, maximum-field tool and
  resource budgets, scoped upload cohorts, cancellation classification, exact
  upload request metrics, and real-server cleanup. One absolute deadline now
  covers resolution, cohort waiting, POST, and verification; waiters cannot
  extend a leader. Cohort admission itself is deadline-bound and cannot return
  cached/conflict/capacity outcomes after expiry or strand an unstarted leader.
  Cohorts invalidate on the atomically coupled non-secret HTTP credential
  generation, preventing cached cross-principal success. Direct and stdio
  acceptance reread returned hash-bound resource URIs and lock complete
  read-write/read-only catalogs, templates, and status calls. Token fixtures
  use the worst allowed four-byte Unicode scalar. Malformed responses,
  latency, rate limiting, and transport faults remain deferred to the external
  P4 fault-injection plan.
- Add a transport-neutral exported-Markdown no-op protocol scenario with fast
  exact-forwarding and lossy-repeat regressions plus ignored serial direct and
  production-stdio real-server cases. Independent stable REST exports and
  fresh `ObjectShow` identity/type/order reads prove byte and typed-semantic
  stability for the approved rich cohort while recording block-ID churn.
- Design the default-off `views-write` collection-membership registry with
  canonical filter-independent list pages, verified single-object add/remove
  desired states, finite HTTP and gRPC work budgets, conservative
  post-dispatch uncertainty, and direct plus spawned-stdio disposable
  real-server acceptance. Review corrections fix the token-dense measured
  result maximum at 61 items, define strict ordered overlap pagination and
  counter evidence, reject overlap-only continuation pages, disambiguate POST
  rejection semantics, and require cross-platform gates. The selector remains
  unsupported until independent review and implementation are complete.
- Add the explicitly selected, read-only `members` toolset with bounded
  `member_list` pagination and exact `member_get` reads. Results expose only
  validated member IDs, explicit space-local names, closed roles, and closed
  statuses; network identities, global names, and icons never enter MCP
  schemas, results, or diagnostics. Pure zero-I/O tests cover strict inputs and
  pre-cancellation, while cleanup-owned real-server direct-router and
  production-stdio scenarios verify bounded pages, exact response identity,
  minimized results, read-only parity, and the erased future boundaries needed
  for default-stack execution. Malformed, latency, 5xx, retry, and
  connection-fault cases remain deferred to the P4 fault-injection design;
  member tests no longer contain a custom HTTP server.
- Add ignored serial production-stdio disposable lifecycle sentinels that
  prove exact create/read object and space identity, registered fallible child
  shutdown, cleanup before a deliberate callback panic resumes, and exact
  absence through a fresh cache-disabled direct exact-ID request.
- Add the startup-only optional toolset registry foundation: exact secret-safe
  `ANY_MCP_TOOLSETS` parsing, landed-registry resolution, deterministic typed
  tool/resource composition, collision and ownership validation, transport
  requirement union, read-only mutation removal, disabled-call rejection, and
  immutable `optional_toolset_status`. Test-only registries lock profile and
  stable/preview contract identity, representative composition snapshots, and
  per-registry/common token ceilings while the absent selector preserves every
  Phase 1 catalog byte and token. Nonempty selections also admit only effective
  HTTP retry limits `1..=5` before authentication or I/O.
- Add an ignored serial tier-2 production-router scenario proving live numeric
  and checkbox filter matches by exact identity, with continuation derived from
  checked upstream pagination and no client-side post-pagination emulation.
- Design document `designs/body-block-tools.md` (`any-2f0g.22`): a default-off
  workflow-oriented `body-blocks` MCP registry with bounded typed body reads,
  verified create/update/delete/move operations, flat rich-page plans, honest
  partial and indeterminate outcomes, closed network behavior, and exact work,
  frame, schema, and token budgets.
- Map verified body-block mutation uncertainty from `anytype-api` directly to
  the fixed secret-safe mutation-indeterminate conflict result and runtime
  category; this is plumbing for the separately tracked optional body workflow
  tools.
- Classify the new `anytype-api` `AnytypeError::BodyGraph` variant in the
  exhaustive error and health mappings (`ToolErrorCode::Upstream`, health
  status `body_graph`). Plumbing only: no `any-mcp` tool can surface a body
  read yet.
- Design document `designs/filters.md` (any-2f0g.4.1): bounded tagged MCP
  filter DTO model — format/condition matrix, one-to-one anytype-api mapping,
  excluded combinations and upstream limitations, hard bounds, cursor binding
  rules, and the shared-module conversion strategy.
- Design document `designs/body-block-model.md` (any-2f0g.18): typed, bounded,
  fail-closed anytype-api body block model over ObjectShow and block RPCs —
  BodySnapshot/BodyBlock trees, closed v1 content/style/mark variants, opaque
  unsupported reads, graph validation, context/space ownership, verified
  mutation evidence, limits, and forward compatibility.

### Changed

- Add strict server-side flat-`and` shared filters to `space_list`, `type_list`,
  `property_list`, `tag_list`, `template_list`, and `view_object_list`. Reject
  recursive/`or` shapes, `view_list` filters, and the dishonest
  `property_list` filter-plus-type combination before I/O; bind canonical
  filter semantics into continuation cursors without rewriting the upstream
  query. Lock the capability matrix, positive/rejected scripted paths, exact
  request forwarding, and reviewed standard catalog/token growth. The shared
  standard direct-router and spawned-stdio discovery scenarios now prove exact
  filtered identities plus filter-bound cursor rejection in prefix-authorized
  disposable spaces. Spawned children are registered for stop-and-wait cleanup
  before protocol initialization, and direct-router dispatches run as separate
  Tokio tasks so the full filter matrix passes on Rust's default test-thread
  stack.
- Extract the `object_search` filter grammar into a shared bounded MCP DTO
  matching the supported `anytype-api` formats and conditions one-to-one.
  Preserve aggregate count/value/depth limits, identifier validation,
  semantically canonical cursor bindings, and unchanged numeric/checkbox
  forwarding without client-side post-pagination emulation. Live tier-2
  evidence now proves the configured backend accepts both filter shapes and
  returns exact expected identities.
  Equivalent logical-group and set-operand permutations/duplicates now share a
  cursor identity without changing their upstream presentation, and Date
  filters include Anytype's supported `in` condition. Select operands use a
  dedicated 1..512-scalar, comma-free reference so the upstream comma-delimited
  encoding is unambiguous. Set arrays advertise 1..100 items, and the recursive
  expression schema requires at least one nonempty member array while retaining
  the runtime checks. Lock every conversion and the reviewed catalog/token
  changes with focused tests and exact snapshots.
- Complete the prerelease documentation contract with current stable/preview
  startup, compact/standard and read-only catalogs, host registration,
  credentials, bounds, mutation uncertainty, security, cross-platform gates,
  resource behavior, and reviewed token baselines. Add `any-mcp` to the
  workspace project index and expand public crate/catalog rustdoc without
  claiming unimplemented optional toolsets or a published release.
- Make the latest released MCP protocol (`2025-11-25`) and its
  initialize/initialized lifecycle the production stdio default. The stateless
  `2026-07-28` implementation remains compiled and schema-tested, but now
  requires exact `ANY_MCP_PROTOCOL=experimental-2026-07-28`; invalid selectors
  fail startup without echoing their value, and input frames cannot select the
  preview. Stable and preview modes retain one handler/catalog implementation.
  Document and test released negotiation through the `2024-11-05` minimum.

### Fixed

- Classify bounded collection-membership evidence failures from `anytype-api`
  as fixed upstream MCP errors and redacted health diagnostics.
- Classify the bounded and malformed file-response evidence errors introduced
  by `anytype-api` exhaustively: header-budget failures map to
  `bounded_result`, malformed upstream file headers map to `upstream`, mutation
  uncertainty remains conservative, and diagnostics retain only fixed
  categories plus the numeric HTTP status.
- Make profile admission truthful about transport dependencies: standard
  read-write now requires authenticated HTTP and gRPC before advertising its
  fixed fourteen-tool catalog, while compact and read-only catalogs may start
  HTTP-only. Configured-but-unhealthy gRPC still fails every selection,
  `server_status` no longer reports omitted mutation toolsets in read-only
  mode, and stable/preview startup share the same admission policy.
- Require independent, finite active-absence and original-type-scoped archived
  readback before every `object_archive` success. The single non-replayed DELETE
  response is dispatch evidence only; matching, malformed, mismatched, and
  uncertain responses cannot bypass stored-state verification, and unproven
  outcomes retain fixed mutation-indeterminate guidance.
- Preserve exact body verification while avoiding Anytype Markdown
  double-escaping for the closed plain-line subset shared by `object_create`,
  `object_update`, and `object_edit`. Raw and canonical plain bodies containing
  underscores now use one unescaped write form and one exact canonical
  fingerprint, body-hash, and verification form; ambiguous Markdown remains
  byte-exact and fails closed on upstream rewrites. Canonical expansion is
  included in body limits before I/O.

### Added

- Add a shared bounded production-process driver and transport-neutral live
  scenario suite, executing all 14 standard tools and all three resource
  operations through both the direct production router and spawned production
  stdio against headless Anytype. Add focused compact, read-only, and preview
  real-headless sentinels; independent `anytype-api` readback; typed exhaustive
  catalog ownership; bounded redacted failure evidence; content-verified
  file/SQLite keystore isolation including WAL and cipher suffixes; explicit
  protected live CI targets; and an unconditional scheduled clean-server soak,
  while preserving the fast scripted HTTP protocol suite. Heap-own the
  exhaustive production tool-dispatch future so repeated stdio calls cannot
  overflow a Tokio worker stack, and keep child-abort stderr private until the
  parent scenario classifies it structurally and records fixture cleanup
  outcome.
- Harden live acceptance follow-up by parsing Windows drive and ordinary
  colon-bearing file-keystore paths without colon splitting, rejecting
  duplicate/missing paths before snapshotting, proving the rebuilt child spec
  points only to one WAL-aware snapshot, asserting the exact compact catalog
  on the preview live wire, and retaining only structural stderr counts so
  unregistered bodies, edit fragments, credentials, and cipher material cannot
  enter failure reports.
- Map structurally classified nested Anytype gRPC authentication failures to
  the fixed secret-safe MCP `authentication` error, including definitive
  post-dispatch rejection handling, without depending directly on
  `anytype-rpc` or formatting source diagnostics.
- Add startup-selected `compact` (default) and `standard` application catalog
  profiles, with read-only mode remaining orthogonal and `server_status`
  reporting the selected profile and stable toolsets. Shared names retain
  identical complete contracts. Enforce deterministic complete-`tools/list`
  and representative-result budgets with compact canonical JSON and pinned
  `o200k_base` tokenization. The reviewed 9,423-token compact catalog remains
  below 5% of the internal 200,000-token compatibility-policy floor with 577
  tokens of headroom; exact baselines cover all four profile/read-only
  envelopes, reject drift, and require an
  explicit rationale for material growth of at least 2%. Validate the current
  production output schemas against reviewed search/get results, and run the
  schema/unit and portable real-process stdio suites on Linux, macOS, and
  Windows in a dedicated CI matrix.
- Add bounded, explicitly selected stdio adapters: the released rmcp
  initialization lifecycle is the production default, while stateless MCP
  `2026-07-28` discovery, per-request metadata/version validation, `-32022`
  negotiation errors, complete-result discrimination, cache hints, concurrent
  cancellation, clean EOF, and the full standard read-write/read-only tool and resource
  surface. Validate real preview process exchanges against the official draft
  schema. The stable decoder now emits exactly one JSON-RPC parse error for
  each syntactically malformed newline frame with an explicit null response
  ID, distinguishes oversized and well-formed invalid requests, never replies
  to valid JSON-RPC notification shapes, and remains usable for subsequent
  requests.
  Decoder errors and rmcp service responses share one bounded stdout writer;
  input framing is cancellation-safe and capped at 2 MiB. Treat client identity
  as optional per the locked schema and preserve empty-string request IDs
  through response correlation.
- Add ignored, cleanup-safe headless production-router coverage in
  `headless_default_discovery_routes_paginate_and_report_ambiguity`,
  `headless_view_body_and_resource_routes_are_complete_and_bound`, and
  `headless_mutations_are_visible_idempotent_and_conflict_safe`, including
  authenticated dual-protocol prerequisites, cursor binding, explicit view
  selection, complete resources, idempotent create, mutation visibility, and
  zero-write stale exact-edit conflicts. Add cleanup-safe create-body
  canonicalization and verified archive representatives in
  `headless_create_body_canonicalization_is_verified_once` and
  `headless_archive_applies_and_returns_verified_success`, with exact
  applied-state and cleanup evidence before teardown.
  Exercise `space_list` limit-one continuation and cursor/query binding with
  two immediately registered disposable spaces whose exact IDs are removed and
  verified absent during test-context teardown; walk the bounded cursor chain
  without item/cursor loops through terminality and require both fixture IDs.
  Collection/view coverage now creates and immediately registers its own
  collection-layout type through the narrow `anytype-api` test fixture instead
  of ambient system schema. Add a privately create-proven, exact-type-bound
  collection with one atomically claimed cleanup dispatch and a cleanup-owned
  second view through that same test-only boundary, walk
  `view_list(limit=1)` without item/cursor loops to an
  exact ordinary-API terminal result, reject limit and list-ID cursor rebinding,
  and retain explicit selected-view object listing with the added view ID.
  Template discovery now creates two cleanup-owned templates through the
  test-only `anytype-api` fixture and walks `template_list(limit=1)` to a proven
  terminal page, requiring both exact fixture IDs, stable query-bound cursors,
  no repeated cursor or item, and a fixed traversal bound.
- Add the initial bounded, workflow-oriented `any-mcp` scaffold using `rmcp`
  2.2.0, including the compiled `2026-07-28` preview contract.
- Add authenticated long-lived Anytype client startup, bounded and cancellable
  upstream execution, request/startup timeouts, stderr-only diagnostics, and
  clean stdio EOF shutdown.
- Harden runtime shutdown to cancel active and queued operations on EOF, emit
  safe structured operation outcomes, and deny payload-bearing dependency
  tracing targets independently of `RUST_LOG` directives.
- Enable operation diagnostics by default with server-generated correlation
  IDs and variant-only Anytype error categories/status codes.
- Add strict JSON Schema 2020-12 input/output contracts, bounded object
  summaries and resource URIs, standard tool annotations, structured results
  with compact JSON text fallbacks, and stable secret-safe execution errors.
- Add reusable bounded pagination, versioned process-lifetime cursors bound to
  normalized queries, Unicode-safe body chunking, and collection/filter caps.
- Convert candidate-rich `anytype-api` resolver ambiguities directly into
  bounded MCP errors, retaining valid alternatives alongside malformed ones;
  resolver scan-limit failures map to the stable `bounded_result` code.
- Enforce configurable finite Anytype JSON and document response budgets while
  chunks arrive, and map secret-safe oversized-response failures to
  `bounded_result`.
- Add transport-neutral handler execution/encoding, checked cursor advancement
  from exact requested/upstream page metadata after bounded result-count
  checks, and deterministic object summary/property adapters with explicit
  finite projection modes and closed value schemas.
- Validate object-summary last-modified timestamps as nonempty bounded RFC 3339
  date-times at construction, deserialization, and schema boundaries.
- Classify handler conversion and result-encoding failures inside the runtime
  operation boundary so diagnostics cannot report false success outcomes.
- Add typed, transport-neutral `view_list` and `view_object_list` handlers with
  resolver-backed view selection, exact one-page reads, checked continuations,
  stable resource-linked object summaries, bounded property projections, and
  fail-closed validation of resolver-returned view identifiers.
- Add typed `server_status`, `space_list`, `type_list`, `property_list`,
  `tag_list`, and `template_list` handlers with one-page opaque continuations,
  resolver-backed references, redacted endpoint status, summary-only template
  output, and bounded tag counts that never expand property options and verify
  the first page's item/total/continuation consistency.
- Make `tag_list` fetch resolved property metadata through one cache-independent
  exact-identity scoped GET, preventing a cold production cache from expanding
  the request into an all-properties scan.
- Add typed, bounded `object_search` and `object_get` workflow handlers with
  resolver-backed references, constrained filters and sorting, exact one-page
  cursor integrity, explicit property projections, Unicode-safe body chunks,
  and complete-current-body SHA-256 values.
- Reject explicit null for every omittable object-read input, validate file
  and object filter operands as safe ids before I/O, and revalidate resolved
  type keys before cursor binding or search dispatch.
- Add one shared, strictly bounded mutation-value contract with deterministic
  number, date, property, icon, and set-ID normalization plus semantic
  read-after-write comparison for future object create and update handlers.
- Add a one-way mutation-dispatch marker and opt-in controlled execution seam
  that distinguishes safe pre-dispatch cancellation, timeout, and shutdown
  from fixed conflict-class indeterminate outcomes that require rereading
  before retry.
- Add one secret-safe, conservative mutation-rejection classifier so create
  and update can distinguish definitive local/HTTP rejection from ambiguous
  transport, timeout, response, retry, and server outcomes after dispatch.
- Add the typed `object_archive` soft-delete workflow with destructive
  annotations, pre-I/O read-only enforcement, safe preflight/evidence identity
  validation, one non-replayed DELETE, and a minimal verified archived-state
  result. Every non-definitively-rejected dispatch uses finite independent
  active-absence plus original-type-scoped archive confirmation; the DELETE
  body is never success evidence, while unproven outcomes return fixed
  mutation-indeterminate guidance.
- Add the exact Anytype document resource template and transport-neutral
  resource handlers with strict canonical URI parsing, intentionally empty
  instance listing, complete 100,000-character markdown reads, document-byte
  ceilings, resource annotations and size, identity verification, shared
  cancellation/timeout controls, and secret-safe bounded errors directing
  larger reads to `object_get` chunking.
- Add typed `object_update` whole-field replacement with non-null omittable
  inputs, the shared bounded property/icon values, exact effective-type schema
  checks, documented empty clear forms, optional complete-body SHA-256 stale
  conflict checks before writing, one no-verify update, bounded semantic
  read-after-write retries, and fixed indeterminate conflict handling for every
  post-dispatch ambiguity.
- Add typed `object_create` with create annotations, closed and bounded
  shared property/icon inputs, nonempty names, an exact 100,000-character body
  ceiling, bounded and revalidated space/type/template resolution, finite
  semantic read-after-write convergence, summary-only output, fixed first-call
  indeterminate guidance after possible POST dispatch, and a supervised finite
  process-lifetime idempotency registry keyed by a versioned canonical
  fingerprint that cannot strand cohorts or issue a duplicate create; identical
  retries of terminal indeterminate keys retain the fixed reread guidance,
  while mismatched fingerprints remain ordinary key conflicts.
- Normalize only evidence-backed single-line plain Markdown into Anytype's
  stable canonical stored form before the create fingerprint and semantic
  expectations, then derive its separate unescaped wire form for the sole POST;
  require both the POST response and final GET to match all requested semantics.
  Keep Markdown syntax, escapes, and other whitespace exact and indeterminate
  when rewritten rather than applying broad trimming or equivalence.
- Add typed `object_edit` with destructive annotations, required complete-body
  SHA-256 concurrency checks, bounded ordered non-overlapping literal edits,
  zero-write stale/count conflicts, one whole-body no-verify PATCH, finite
  semantic verification, summary-and-new-hash output, and fixed indeterminate
  handling for every ambiguous post-dispatch outcome.
- Wire the complete static Phase 1 production catalog with exact typed schemas,
  annotations, handler routing, document resources, and cursor-free
  `tools/list`; advertise static capabilities without change notifications or
  subscriptions.
- Add strict `ANY_MCP_READ_ONLY=0|1` configuration. Read-only mode omits all
  four mutation tools and rejects stale direct mutation calls before argument
  decoding, resolver work, or upstream I/O.
- Add a cross-platform production-process stdio regression harness with
  bounded deadlines, authenticated loopback fixtures, exact profile/read-only
  catalogs, all-tool dispatch, resource reads, structured results,
  cancellation, unknown and invalid requests, clean EOF, and complete
  stdout/stderr purity checks. Cover preview MCP 2026-07-28 discovery plus
  malformed-first and post-initialize parse recovery, including repeated and
  oversized frames followed by successful requests in both server modes.
- Bound each captured protocol frame, diagnostic line, aggregate stream, and
  in-process frame queue; make process and fixture teardown close, kill, wait,
  and join every owned worker on success and failure. Assert exact response
  counts separately from byte purity and require JSON-RPC 2.0 on every stdout
  object.
- Compare all four real profile/read-only `tools/list` wire payloads against
  the reviewed canonical catalog snapshots, including exact descriptions,
  nested input/output schemas, annotations, ordering, and omissions without
  duplicating fixture contents.
- Document reproducible isolated MCP Inspector 0.22.0, Codex CLI 0.144.6, and
  Claude Code 2.1.214 compatibility checks. Record exact 14/10 Inspector tool
  counts, live `server_status` calls from both released clients, and the
  official conformance runner's current HTTP-only server interface without
  claiming an inapplicable stdio conformance pass.
- Add reviewed deterministic profile/read-only catalog snapshots, an independent
  fail-closed local-reference graph audit for recursive schema bounds and exact
  annotation audits, exact character/byte boundary coverage, exhaustive Anytype
  error-classifier assertions, and cross-surface secret-redaction checks.

### Changed

- Harden schema contracts to reject unconstrained nested values, maps, arrays,
  numbers, untagged unions, and unsupported dynamic schema applicators; link
  success encoding to each declared output type and require bounded candidates
  before returning ambiguity errors.
