# Changelog

All notable changes to this project will be documented in this file.

The format is based on Keep a Changelog, and this project adheres to Semantic Versioning.

## [Unreleased]

### Added

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

- Extract the `object_search` filter grammar into a shared bounded MCP DTO
  matching the supported `anytype-api` formats and conditions one-to-one.
  Preserve aggregate count/value/depth limits, identifier validation,
  semantically canonical cursor bindings, and the known upstream
  numeric/checkbox behavior without client-side post-pagination emulation.
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
