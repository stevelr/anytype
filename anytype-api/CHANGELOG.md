# Changelog

All notable changes to this project will be documented in this file.

The format is based on Keep a Changelog, and this project adheres to Semantic Versioning.

## [Unreleased]

### Changed

- `KeyStore` now keeps every credential for a service (HTTP token, gRPC
  account id, account key, session token) in a single keystore entry
  (`user = "credentials"`, a JSON document) instead of one entry per
  credential, so OS keyrings prompt once per application rather than up to
  four times. Legacy per-credential entries are read transparently and
  migrated to the single entry on first access; `clear_all_credentials`
  removes both layouts. The public `KeyStore` API is unchanged.

- Add `scope_grpc_deadline` so a caller-owned absolute workflow deadline caps
  all nested generated gRPC calls, readiness waits, and propagated
  `grpc-timeout` remainders without exposing `anytype-rpc` as a direct
  dependency to higher-level applications.
- Validate direct gRPC chat text, reaction, chat/message identifiers, and the
  legacy `read_all` space argument before transport. Cleanup-owned live
  coverage independently reads back `send_text` and reaction add/remove state.
  `read_all` remains account-global because Heart's wire request has no scope;
  the shared-server live tier does not execute it.
- Align the README and crate-level documentation on the current HTTP/gRPC
  credential model, transport coverage, fluent builder API, and a compact
  panic-free quick start.
- Run the required disposable live tier on pushes to `main` and nightly, and
  run the connected upstream-characterization tier weekly. Tier 2 also builds,
  tests, and documents the crate from its packaged tarball with its packaged
  `anytype-rpc` dependency.
- Construct universal object share links locally from validated space and object
  IDs. This replaces the retired `ObjectShareByLink` Heart RPC, whose retained
  compatibility stub terminates the server when called. The disposable
  object-link live test is now registered in the protected live manifest,
  which owns 21 required cases.
- Serialize first-use gRPC client initialization behind the client cache,
  prefer a nonempty session token over an account key, and classify missing
  credentials and connection failures without exposing their values. Local
  endpoint discovery now checks `lsof` exit status and has deterministic
  coverage for candidate filtering and responsive-port selection.
- Send the `spaceUxType` chat detail as well as the legacy `withChat` flag when
  creating chat spaces over gRPC. Heart 0.50.10 selects chat UX from the detail
  while its REST space readback reports the immutable regular-space type, so
  the disposable test accepts that response as well as APIs that expose the
  UX as a `chat` discriminator. The test allows up to 30 seconds for fresh CI
  state to converge, uses a valid reader-tier auto-approve invite, accepts the
  two observed guest-permission readback shapes, and waits for ACL revocation
  to settle before replacing an invite. Its cleanup-owned soak server retains
  network access because Heart delegates space sharing to its coordinator;
  enablement retries only the definitive pre-admission `NO_SUCH_SPACE` result.
- Let disposable headless live workflows select the Anytype CLI through
  `ANYTYPE_CLI_BIN`, defaulting to `anytype` on `PATH`.
- Expose side-effect-free loading of Anytype CLI account credentials through
  `GrpcCredentials::from_cli_config`, distinguishing a missing config from an
  unreadable or malformed one.
- Route object listings containing number or checkbox filters through the
  space-scoped REST search endpoint, preserving their JSON scalar types while
  the upstream flat-query object-list parser rejects them. A single positive
  type filter maps to search's dedicated type selector, which works while a
  fresh server's custom-property index settles. Other object-list filters
  remain on the original GET endpoint.
- Enable the disposable-space recovery harness on Windows by creating private
  ACLs for its state directory and ledger files, and accepting ownership only
  by the process user, LocalSystem, or Built-in Administrators. File and
  directory handles open reparse points without following them, then reject
  those objects before recovery I/O. File contents are flushed before each
  rename; directory-entry persistence is delegated to the NTFS metadata
  journal because Windows rejects `FlushFileBuffers` on directory handles.
- Retain a bounded copy of a failed live-gate entry's output as a run
  artifact (opt-in via `ANYTYPE_API_GATE_OUTPUT`): the disposable server's
  per-run throwaway credentials make the output safe to keep, and entry
  failures were undiagnosable without it.
- Make the protected live workflow manual-only, with required, soak, or all
  tier selection, Python 3.14, and the Rust version selected from
  `rust-toolchain.toml`.
- Exclude the server-backed integration targets from the manual CI matrix's
  offline test step, matching the `just test` offline gate; the live
  workflow remains the home of server-backed coverage.
- Run the protected live tiers on GitHub-hosted runners against a disposable
  namespace-isolated headless server with ephemeral credentials
  (`.github/scripts/provision-headless-server.sh`), replacing the retired
  self-hosted `anytype-headless` runner; the soak tier's host reset script
  is obsolete because every run starts from a clean server.
- Bound every REST wire request with one absolute logical deadline. Defaults
  are 120 seconds for ordinary requests, 600 seconds for file and multipart
  requests, and separate 120-second SSE open and error-body phases. Explicit
  `HttpTimeoutPolicy` values support finite or disabled boundaries;
  `ANYTYPE_HTTP_TIMEOUT_SECS` supplies inherited process policy. Established
  SSE idle and lifetime limits remain disabled unless configured. Typed
  secret-safe timeout outcomes and saturating per-class metrics distinguish
  aborted reads, indeterminate mutations, terminated streams, and caller
  transport timeouts.
- Apply a validated `GrpcTimeoutPolicy` to the cached gRPC client, with
  credential, ordinary, long-operation, stream-setup, cleanup, optional idle,
  and optional lifetime classes. Absolute enclosing and tighter caller bounds
  win; reads abort while possibly dispatched mutations remain indeterminate.
  Chat reconnect, capped backoff, and watermark catch-up now remain inside the
  stream lifetime; queued chat events survive output backpressure and a
  simultaneous disconnect, and backoff resets after two delivered decoded
  events. Redacted transport failures discard their original payload while
  retaining the status code, process-watcher diagnostics omit peer text and
  process content, and local gRPC discovery gives each connect/version probe
  one two-second budget.
- Restrict automatic HTTP replay to `GET`, `HEAD`, and `OPTIONS`. Mutation
  `POST`, `PATCH`, `DELETE`, and unapproved `PUT` dispatch once; ambiguous
  transport, timeout, 408, 429, 504, and server failures require a fresh state
  observation before application retry.
- Classify mutation failures precisely at the dispatch boundary. Connection
  and request-construction failures that provably precede dispatch keep their
  typed transport error and reqwest source instead of an indeterminate
  outcome, a response completed exactly at deadline expiry is returned rather
  than converted to a timeout, and a failed error-body read no longer masks an
  already-known ambiguous mutation status. Established SSE caller transport
  timeouts emit the standard structured tracing observation.

- Reconcile the maintained HTTP/gRPC coverage inventory with the completed
  any-dm9k campaign. Remaining gaps now distinguish direct crate coverage from
  cross-crate evidence, reuse existing ticket owners, and define bounded
  helper and loopback-fixture scope before filing.
- Accept any bounded, sendable asynchronous reader for retained REST file
  uploads, allowing capability-owning callers to use cursor-independent readers
  without reopening a filesystem path. The stream rejects both shorter and
  longer sources than its declared exact length.

### Added

- Add `ClientConfig::grpc_timeouts` and its fluent builder, inherited
  `ANYTYPE_GRPC_TIMEOUT_SECS` resolution, typed gRPC deadline/control errors,
  and deadline-aware generated-client access through `anytype-rpc`.
- Add `count_archived_bounded`, which returns an exact archived-object count
  only when exhaustion is proven within the caller's logical-page budget.
  Preserve exhaustive `count_archived` behavior, validate archive pagination
  evidence, and avoid constructing partial `Type` values from ID-only Heart
  details.
- Add direct coverage for authentication transitions, paged lookup helpers,
  search/filter wire shapes, legacy gRPC file downloads, URL uploads, direct
  chat reads and edits, selective chat unsubscription, and second-view
  continuation. The protected live manifest now owns 19 required cases.
- Add a reusable `scripted-http-fixture` feature for downstream contract tests.
  Its loopback server applies fixed ceilings to the finite response sequence
  and captured request methods, paths, and bodies, while errors and debug output
  retain only payload-free categories and sizes. The chat history/edit sequence
  fixture exercises the shared boundary.
- Add a closed ignored-test inventory and protected disposable live gate. The
  manifest separates 19 required cleanup-owned tests, three scheduled
  characterization probes, and two excluded ambient or manual probes. Reproduce
  the offline inventory with:

  ```sh
  cargo test --locked -p anytype --test live_gate_manifest
  ```

  Reproduce the required manifest driver with the protected env-file contract:

  ```sh
  test -n "${ANY_MCP_HEADLESS_ENV_FILE:-}"
  test -r "$ANY_MCP_HEADLESS_ENV_FILE"
  set -a
  source "$ANY_MCP_HEADLESS_ENV_FILE"
  set +a
  test "${ANYTYPE_KEYSTORE:-}" = env
  test -n "${ANYTYPE_KEYSTORE_SERVICE:-}"
  test -n "${ANYTYPE_KEY_HTTP_TOKEN:-}"
  test -n "${ANYTYPE_KEY_SESSION_TOKEN:-${ANYTYPE_KEY_ACCOUNT_KEY:-}}"
  export ANYTYPE_DISPOSABLE_TEST_PROCESS=1
  python3 anytype-api/scripts/run-live-gate.py required anytype-api/tests/live-gate-manifest.toml
  ```
- Replace seven superseded ambient filter cases with stronger matrix rows for
  checkbox hydration, inclusive ranges, and numeric-plus-checkbox conjunctions
  across both list endpoints. The two former ambient Set/view probes now create
  source-backed Sets and collections inside cleanup-owned disposable spaces and
  run in the required live gate.

---

## [Unreleased - 260806]

### Added

- Add live file coverage for permanent delete, REST-vs-gRPC upload backend
  auto-selection (path, reader, and rich-option promotion), and
  conditional/ranged downloads (metadata `HEAD`, `206`, `412`, `416`, and a
  locally rejected zero-length range). The README records the verified
  `anytype-cli` 0.3.6 file-endpoint behavior: `206`/`412`/`416` are served, no
  `ETag`/`Last-Modified` validators are sent, no request timeout is applied by
  default, and permanent deletion can take about 154 seconds to return `204`.
  The live test uses a finite 180-second ceiling aligned with the CLI test gate.
- Add a live show/close lifecycle measurement test proving a bare gRPC
  `ObjectShow` holds no server-side open state (verified via
  `DebugOpenedObjects` with an `ObjectOpen` validation leg), that one
  `ObjectClose` releases an opened object, and that `BodyRequest::fetch`'s
  owned foreground close is server-confirmed and leaves no object open.

- Add typed space administration methods for chat-space creation, permanent
  deletion, invitation creation/listing/revocation, and sharing controls.

- Add retained-handle streaming to the unified file upload builder. The new
  `reader` source accepts an already-open Tokio file plus its exact length,
  constructs a length-bounded REST multipart body without reopening a path or
  buffering the complete payload, and rejects incompatible gRPC-only rich
  options.
- Add an ignored, prefix-authorized real-server probe for the upstream
  `ObjectAddDiscussion` repeat-create defect. It dispatches exactly two raw
  RPCs on one cleanup-owned parent, requires the observed second
  `UNKNOWN_ERROR` with no usable ID, then proves the original relation and
  complete derived identity through a fresh typed read. The accompanying
  evidence separates that behavior from the malformed persistent read-only
  fixture and provides payload-free, operator-ready reproductions for both
  upstream reports.
- Add an opt-in `test-fixtures` feature with a narrow, production-validated
  typed body snapshot constructor for downstream block-count and atomic
  read-restriction contract tests, canonical/malformed table fixtures, and a
  boolean-only check that verifies a test buffer omits configured HTTP/gRPC
  credential bytes without returning them. The feature is disabled by default,
  is not enabled by production dependents, and does not make snapshots
  deserializable.
- Add a public protobuf-free finite body RPC seam. `BodyRpcConfig` shares one
  absolute deadline across gRPC acquisition, `ObjectShow`, bounded foreground
  and cancellation fallback `ObjectClose`, one-shot writes, and verification.
  Tonic decoder limits reject Show responses above 4 MiB and every mutation or
  close response above 64 KiB before body allocation/decode. Closed lifecycle
  errors, exact payload-free counters, and the pre-poll write counter expose
  cleanup and dispatch certainty without retaining identifiers or upstream
  response text. Independently-deadlined workflow steps can share one metrics
  observer for exact aggregate lifecycle accounting. Body reads and writes now
  enforce 1..64-byte control-free
  emoji values and both UTF-16 mark endpoints as ordered in-bounds Unicode
  scalar boundaries.
- Add typed attached-discussion discovery and idempotent ensure operations for
  Basic and Note parent objects. A required-layout REST wire preflight plus
  bounded, cleanup-owned gRPC reads verify the exact parent relation and the
  derived discussion's space, smart-block type, layout, and deterministic
  unique key. Ensure reads before writing, dispatches at most one
  `ObjectAddDiscussion`, retains the returned candidate, and reconciles every
  dispatched outcome without replay; cancellation leaves reconciliation in an
  owned task. A closed payload-free error kind preserves authentication,
  malformed-evidence, cleanup, deadline, upstream, and indeterminate outcome
  classes. Public cumulative work counters and one finite absolute operation
  deadline make HTTP, show, close, reconciliation, and write ownership
  observable. Definitive pre-acceptance authentication or permission failures
  preserve their original classification without dispatching a synthetic
  close; accepted or indeterminate shows still own bounded cleanup. Pure
  lifecycle/state-machine tests and cleanup-owned disposable
  real-server coverage replace any semantic mock or fault emulation.
- Add `collection_member_add`, a singular non-replayed collection mutation
  seam that preserves the exact completed non-success HTTP status instead of
  collapsing 401, 403, 404, or 410 into broad error variants. Redirects remain
  disabled; transport, response-read, and malformed-success failures remain
  indeterminate errors. The existing multi-object view helper is unchanged.
- Extend payload-free collection-membership metrics with exact observer-query,
  collection-add, and collection-remove dispatch counters. The observer count
  starts only after exact REST collection/object identity validation, so
  Set/query rejection remains distinguishable from a canonical membership
  query. These shared-clone counters let bounded desired-state workflows prove
  no-op zero writes and one logical write without retaining identifiers.
- Add a cleanup-owned test helper that attaches one exact-name saved-view
  filter to a privately proven collection view. It performs one authenticated
  filter-add RPC and requires the server-assigned filter identity plus exact
  proto and REST readback before returning, enabling real-server tests to prove
  that canonical direct membership remains independent of view presentation.
- Add per-upload multipart request, successful response, and error-response
  byte ceilings for REST file uploads. The builder computes the complete
  serialized one-part body before authentication or I/O, and the upload POST
  is never replayed by middleware. Add bounded space-name resolution whose
  finite response ceiling applies independently to every resolver page while
  stable IDs still return without a request.
- Add a non-secret in-memory HTTP credential generation counter. It advances
  on every set or clear operation so process-local coordination can invalidate
  principal-bound results without retaining or hashing bearer material. The
  credential and generation are replaced under one lock, so observers cannot
  see new or cleared credentials paired with the preceding generation.

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
  and preserves typed secret-safe gRPC authentication failures. Public,
  payload-free client metrics now count query rounds, subscribe attempts,
  confirmed foreground cleanup, and detached cleanup fallbacks independently.
  Set/query objects fail before subscription.
- Add evidence-backed REST chat prerequisites for bounded MCP workflows:
  message timestamp conversion is fallible and canonical UTC milliseconds,
  older-history pages preserve server order behind a 256-byte opaque
  before-anchor and 12-item ceiling, and verified text/format edits require an
  independent exact readback whose `modified_at` strictly advances. Scripted
  transport and prefix-authorized disposable-server coverage lock routes,
  terminal pagination, insertion stability, typed failures, and cleanup.
- Add a cache-independent exact space GET for bounded mutation preflight and
  semantic readback, plus an opaque disposable-test claim that safely registers
  a space created through another reviewed client surface immediately after
  its exact response is returned.
- Make type-property classification finite and cancellation-safe with bounded
  tonic and outer `ObjectShow`/`ObjectClose` deadlines, an owned close guard,
  one detached cleanup fallback, payload-free lifecycle errors, and a
  remaining-budget verifier seam for compound MCP readback rounds. Show and
  Close own independent windows, cleanup errors take precedence, and public
  payload-free counters expose Show, Close, fallback, and confirmed cleanup
  success/exhaustion work.
- Add a bounded direct collection-membership observation that exact-checks
  space, collection, and object identity, rejects Set/query lists, and combines
  a canonical collection-scoped Heart query with independent unscoped index
  controls before and after absence. Unique client-owned subscription IDs,
  finite RPC deadlines, and cancellation-resilient cleanup protect Heart's
  app-global subscription registry. Only complete exact evidence returns
  `present` or `absent`; view filters, pagination, malformed counters, and
  incomplete index evidence cannot manufacture an absence result.

### Fixed

- Preserve REST model fidelity for types, properties, tags, and members by
  retaining their `object` discriminator and decoding member icons as `Icon`.
  File detail parsing now accepts integral numeric `addedDate` values and no
  longer synthesizes `targetObjectId` from `createdInContext`.
- Run the HTTP deadline tests in real time until the fixture accepts the
  request and only then freeze the clock: under `start_paused`, auto-advance
  could virtually expire the deadline while the real TCP connect was still
  in flight, leaving the fixture parked forever on a slow runner.
- Make two unit tests portable to the CI qualification matrix: the gRPC
  port-discovery test now tolerates a host without `lsof` the same way
  `find_grpc` does (discovery unavailable, not an error), and the scripted
  auth keystore cleanup detects the replaced-by-directory case from metadata
  because macOS reports `EPERM` instead of `EISDIR` when unlinking a
  directory.
- Fix the `cargo doc` warning caused by `default_platform_keyring` linking to
  the private `resolve_default_store` helper.
- Fix the disposable test-harness sweep failing every run when an unrelated
  ambient space has an empty name: strict name validation now applies only to
  prefix-authorized (deletable) spaces, while unauthorized spaces keep
  identity and control-character checks and are recorded unselected.

- Correct the crate's coverage and Cargo-feature documentation. The README and
  crate docs no longer claim 100% REST coverage; they now describe direct REST
  coverage separately from gRPC-equivalent coverage and point at
  `docs/http-grpc-overlap.md` for the current transport mapping. Space
  backup docs no longer describe a `grpc` Cargo feature: `default = []`,
  `anytype-rpc` is an unconditional dependency, and gRPC-backed methods are
  gated only by run-time gRPC credentials.

- Make shared integration-test contexts require `ANYTYPE_TEST_SPACE_PREFIX`,
  create a fresh uniquely named space for each test, and delete the exact
  cleanup-owned space after success, callback error, or panic. Missing and
  invalid prefixes now fail with an actionable configuration error instead of
  falling through to legacy ambient-space selection and an opaque auth error;
  test and example helpers no longer consult ambient space-ID variables.
  Ownership inventories accept pre-existing spaces with empty or otherwise
  opaque names while retaining strict validation for newly created and deleted
  test-space identities. Ordinary test-space creation and deletion is
  single-flight within each test process to avoid Heart returning indeterminate
  HTTP 500 responses after committing concurrent space mutations.
- Make validation-only and shared integration-test clients honor
  `ANYTYPE_KEYSTORE` when configured and use the in-memory `env` keystore when
  it is absent, so offline tests do not depend on an available platform
  keyring.
- Refresh the live mutation retry inventory for the four deliberately
  single-attempt property/tag mutations that assert cache-independent physical
  request bounds.
- Prevent disposable-space safety sweeps from orphaning an allocated recovery
  plan after a failed inventory. A same-process retry now discards an
  incomplete plan or resumes the exact completed plan before allocating a new
  one.
- Make the ignored numeric/checkbox compatibility matrix operator-reproducible:
  both unfiltered endpoints must first return the exact cleanup-owned cohort;
  semantic mismatches require three stable observations and render only
  redacted identity relations/counts; API failures retain only safe status
  classes; and the 22-row report is emitted only after verified disposable
  space absence. Document the exact CLI, Heart, API, command, endpoint matrix,
  and upstream issue/PR disposition without exposing fixture identities or
  response bodies.
- Require verified table-create receipts to contain the complete canonical
  sparse Heart subtree: exact ordered regions, columns, and rows; no cells for
  a no-header table; or one grey-background empty paragraph cell per column
  under the first header row only. Missing, extra, foreign, nested, or
  malformed cells no longer satisfy receipt verification.

- Make disposable-space readiness convergence finite and observable with an
  exact 20-second/50-attempt budget. Readiness now resolves only the exact
  `@page` key, verifies it through a cache-independent GET on the same space
  path, rejects mismatched ID/key/archive evidence, and retains only a closed
  final stage/category plus attempt count while preserving cleanup precedence.
  Pre-readiness create failures now retain an equally closed setup
  stage/category that distinguishes rejected or indeterminate requests and
  invalid ID/model/name/ambient-identity responses without exposing values.
  Numeric/checkbox callback failures now retain a closed fixture or exact
  comparison stage and payload-free `TestError`/API diagnostic category, so
  callback execution is distinguishable from pre-callback convergence without
  exposing fixture or request values. The ignored compatibility matrix now
  completes all eleven fixed cases independently on object-list and scoped
  search before cleanup and asserts afterward, emitting only the 22 canonical
  static endpoint/case labels, closed failure categories, and validated HTTP
  status codes/classes when available.
  Setup, readiness, and callback evidence now uses exhaustive enums end to end,
  preventing callers from forging diagnostic strings. The cleanup-owned filter
  fixture also replaces its cache-only `due_date` lookup and fallback with the
  bounded cache-independent property resolver required by disposable clients.
- Reconcile `anytype-heart#2879` with exact query-encoding unit regressions and
  a cleanup-owned real-server matrix over object-list and scoped-search
  endpoints. On `anytype-cli` 0.3.6, all eleven object-list cells return HTTP
  400; scoped search returns six exact-identity passes and five bounded semantic
  failures. The object-list issue remains open, while the scoped-search
  mismatches require separate upstream reports. The matrix covers all six
  numeric comparisons, both checkbox comparisons, integer and negative-decimal
  values, and missing-property semantics without coercion or client-side
  filtering.
- Property and tag create/update builders now offer a cache-independent
  `no_cache_refresh()` path. It invalidates a primed space-property cache after
  the write instead of silently collecting every tag page, so mutation request
  counts remain independent of cache state and callers can own one explicitly
  bounded readback page.

- Empty-filter integration coverage now creates and cleanup-registers its own
  exact object instead of assuming the ambient test space is nonempty.
- Space creation now rejects an empty name through configured client limits
  before HTTP dispatch; the live validation suite no longer creates untracked
  unnamed spaces while probing server-dependent behavior.
- Unauthenticated live tests now use unique empty file keystores, so valid
  ambient `env` credentials cannot silently authenticate their control clients.
- Text-property integration setup now uses the existing finite definitive-429
  retry seam before registering the created object for cleanup.
- Pagination-offset integration coverage now owns and filters an exact
  cleanup-registered object cohort, so concurrent tests cannot shift its page
  windows through unrelated ambient-space mutations.
- Search requests now reject pagination limits outside `1..=1000` with the
  stable validation error before global or space-scoped HTTP dispatch.
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
- integration tests accept the current missing-token authentication error; at
  that release, boolean/numeric filter cases were temporarily ignored against
  the then-current server and tracked by
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
  only block IDs and structural counts.
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
- new `resolve` module: name and id resolution helpers as `AnytypeClient` methods:
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
