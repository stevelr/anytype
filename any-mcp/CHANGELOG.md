# Changelog

All notable changes to this project will be documented in this file.

The format is based on Keep a Changelog, and this project adheres to Semantic Versioning.

## [Unreleased]

### Added

- Add ignored, cleanup-safe headless production-router coverage in
  `headless_default_discovery_routes_paginate_and_report_ambiguity`,
  `headless_view_body_and_resource_routes_are_complete_and_bound`, and
  `headless_mutations_are_visible_idempotent_and_conflict_safe`, including
  authenticated dual-protocol prerequisites, cursor binding, explicit view
  selection, complete resources, idempotent create, mutation visibility, and
  zero-write stale exact-edit conflicts. Add the cleanup-safe blocked-behavior
  diagnostics `headless_diagnostic_archive_applies_before_fixed_error` and
  `headless_diagnostic_create_body_applies_before_indeterminate_error`, which
  prove applied state and cleanup before accepting their fixed error codes.
  Exercise one bounded ambient `space_list` page with current-space evidence
  and optional cursor validation without assuming terminality.
- Add the initial bounded, workflow-oriented `any-mcp` scaffold using `rmcp`
  2.2.0 and protocol revision `2026-07-28`.
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
  annotations, pre-I/O read-only enforcement, safe resolver/response identity
  validation, and a minimal verified archived-state result.
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
  bounded deadlines, authenticated loopback fixtures, exact normal/read-only
  catalogs, all-tool dispatch, resource reads, structured results,
  cancellation, unknown and invalid requests, clean EOF, and complete
  stdout/stderr purity checks. Freeze modern MCP 2026-07-28 discovery and
  malformed-JSON parse handling as ignored acceptance tests until their
  tracked blockers are implemented, without claiming conformance prematurely.
- Bound each captured protocol frame, diagnostic line, aggregate stream, and
  in-process frame queue; make process and fixture teardown close, kill, wait,
  and join every owned worker on success and failure. Assert exact response
  counts separately from byte purity and require JSON-RPC 2.0 on every stdout
  object.
- Add reviewed deterministic normal/read-only catalog snapshots, an independent
  fail-closed local-reference graph audit for recursive schema bounds and exact
  annotation audits, exact character/byte boundary coverage, exhaustive Anytype
  error-classifier assertions, and cross-surface secret-redaction checks.

### Changed

- Harden schema contracts to reject unconstrained nested values, maps, arrays,
  numbers, untagged unions, and unsupported dynamic schema applicators; link
  success encoding to each declared output type and require bounded candidates
  before returning ambiguity errors.
