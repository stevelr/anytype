# Changelog

All notable changes to this project will be documented in this file.

The format is based on Keep a Changelog, and this project adheres to Semantic Versioning.

## [Unreleased]

### Changed

- Map the new anytype-api HTTP deadline errors onto existing MCP error
  classes. Mutation-scoped deadline expirations and explicitly indeterminate
  mutation outcomes return `mutation_indeterminate`; other deadline
  expirations classify as upstream failures with dedicated
  `http_deadline` and `http_mutation_indeterminate` diagnostic categories.
  Collection-member adds treat the new indeterminate acknowledgement outcome
  as an indeterminate membership operation.
- Classify a rate-limited (429) mutation as indeterminate instead of a
  definitive upstream rejection, matching the HTTP timeout policy: the server
  may have applied the write before rate-limiting the response, so the fixed
  conflict error now directs callers to observe fresh state before retrying.
- On Windows, staging single-instance exclusion now relies on the exclusively
  locked `instance.lock` — which Windows refuses to unlink or replace while
  held — instead of an undocumented directory lock, and directory-entry
  durability is delegated to the NTFS metadata journal while file contents
  remain explicitly flushed before every publication.

### Added

- Run the artifact data-plane acceptance and adversarial suites on every
  released target of the platform matrix. The portable CI job declares one row
  per target — Linux x86_64/aarch64, macOS aarch64, Windows x86_64/aarch64 —
  and each row executes both compiled artifact control planes serially: the
  library plane, which also locks the exact artifact catalog and schema
  snapshots, and the spawned `headless_stdio_e2e` plane, which adds the
  adversarial case matrix. The live-gate manifest pins the platform rows, both
  suite command lines, and the whole-target shape of the live steps, so a
  narrowing filter cannot silently drop artifact owners; it normalizes
  workflow line endings before hashing so the reviewed digest survives a
  Windows checkout, and a floor test rejects a collapsed artifact selection in
  either plane. Live real-backend rows remain Linux-only.

- Accept client-root narrowing on stable stdio with two live protocol rows in
  the spawned artifact matrix owner. The intersecting row advertises the MCP
  roots capability, answers exactly one bounded `roots/list` snapshot with the
  physical import root, imports real bytes through the retained root, receives
  the uniform hidden-resource refusal for the excluded export root, and proves
  the session decision is frozen rather than re-queried. The static-fallback
  row advertises no roots capability, is never asked, and keeps both
  configured roots effective. Both rows must report the same advertised
  catalog and the same `artifact_status` projection, and every other spawned
  child must still keep stdout response-only.

- Prove the artifact configuration-selection contract end to end. The policy
  scenario family gains a no-selected-file compatibility row: the fixture
  renders a complete policy that no server selects, and every control plane —
  scripted, direct, stable, and preview — must advertise the unreduced
  read-write catalog while reporting zero import and export roots, no staging,
  and the fixed roots-required guidance for every root-based call. The same
  family now closes the truth table that the required `spaces.read_only`
  declaration defines: the two accepted rows start a server, and the two
  refused rows start a bounded production child on both spawned stdio profiles
  that must exit before its first protocol frame with an empty stdout and one
  path-free, credential-free diagnostic. A new unit test in
  `artifact_config.rs` pins both refusal texts, which the acceptance harness
  restates because it cannot name the production module.
- Retire the last pending adversarial robustness rows: all 122 matrix cases
  now state executed evidence. The crash-restart owner adds the CRASH-04
  durable-record corruption rejection (`artifact state reconciliation
  failed`, corrupt bytes retained byte-for-byte, restart succeeds after
  restoration); the validator-flood owner runs FLOOD-01/02/03 against a
  dedicated small pinned validator fixture binary (production hashes pinned
  executables under a 128 MiB ceiling that debug test binaries exceed); the
  lifecycle owner carries the FLOOD-07 failing-call burst registered to it;
  and the partial-write, TTL, and quota owners recorded HAND-03/05/16,
  PART-01..07, PART-11, FLOOD-04/05, and CLEAN-03/04 live.

- Make artifact staging durable across crashes and restarts. The staging root
  is now a closed layout (`instance.lock`, `records/`, `payloads/`, `tmp/`,
  `tombstones/`) in which every state transition is flushed before it becomes
  visible, deletions publish identity-bound tombstones first, and startup
  reconciliation resumes interrupted deletions, truncates uncommitted upload
  bytes, reaps torn allocations, and revives retained import-reconciliation
  uncertainty before the listener binds. Unknown entries, corrupt records, and
  unproven cleanup barriers fail activation with the fixed
  `artifact state reconciliation failed` category without deleting anything; a
  runtime durability failure shuts the server down with the fixed
  `artifact staging durability uncertain` category.
- Migrate staging roots written by earlier releases on first activation:
  the legacy `.any-mcp-staging.lock`, flat payload files, and legacy
  temporaries are removed with the same owner-private single-link proof that
  release applied, and activation fails closed if the legacy lock is still
  held by a live older instance.
- Extend the closed artifact adversarial inventory with production staging
  regressions for uniform bearer refusal, route, direction, and space binding,
  cross-transport not-found equivalence classes, strict request grammar, rate
  recovery, and request-permit shedding. The
  partial-write owner now drives malformed raw HTTP, disconnect/resume, offset,
  hash, consumed-handle replay, inventory, quota-allocation, and download-range
  assertions. Four spawned cancellation owners pause before export publication,
  inside atomic publication, after file-import dispatch, and after document
  update dispatch; the
  process-crash owner captures a deterministic kill during a JSON-RPC frame.
  Cleanup owners cover failed import/export rollback, release and TTL
  invalidation, required-validator refusal, and read-only catalog isolation.
  Flood owners measure maximum-occupancy aggregate status, oversized validator
  output, validator deadline enforcement, descendant cleanup, oversized
  document-export refusal, and bounded spawned diagnostics.
  These and the remaining handle, process-crash, output-flood, and
  failure-cleanup rows stay pending until their live case-specific owners pass.
- Add closed real-server adversarial artifact coverage for 43 traversal,
  filesystem-alias, filename, MIME, and metadata cases. Direct-router and
  stable/preview stdio owners enforce exact case partitions, fixed refusal
  payloads, retained-root access and successful-open counters, descriptor-bound
  redacted-log audits, private-root/staging invariants, and platform capability
  records. Artifact status now exposes only aggregate remaining staging bytes
  and entries so acceptance runs can prove quota restoration without record
  metadata.
- Complete real-server dynamic filesystem coverage for `SYM-01` through
  `SYM-13`, `RACE-01` through `RACE-10`, and `HLINK-01` through `HLINK-06`.
  Direct owners synchronize import, export, and document races at typed
  one-shot gates; stable stdio repeats import and export gate rows in isolated
  child processes. Windows junction and reparse rows verify native reparse tags
  before containment checks, with fixed capability outcomes when unavailable.
- Make dynamic race and reparse acceptance evidence match the closed matrix:
  stable stdio inventories all four repeated dynamic rows, import rows perform
  their exact rename-over, extension, truncation, or retained-root-swap
  mutation, and moved export roots require a classified refusal with no
  published file. Windows reparse targets contain verified sentinels, while
  unsupported Unix-only rows remain explicit capability outcomes.
- Preserve staged bytes, registry ownership, and charged quota after the
  `HLINK-05` hostile-link cleanup conflict. A later release returns the same
  fixed conflict after the outside link is removed, preventing retry-pathname
  deletion from changing the staged record.
- Protect every cleanup-owned ignored live test with serial direct-router,
  spawned-stdio, and discussions-process gates. Closed offline manifests and
  executable-count checks reject renamed, missing, skipped, or zero-test
  cases; object-edit coverage uses a disposable space, and the redundant
  direct-body orphan is retired in favor of the shared body scenario.
- Restrict both self-hosted live jobs to manual dispatches and trusted pushes
  to `main`, after the hosted contract matrix succeeds. Immutable action pins,
  silent bounded test transcripts, and descriptor-bound post-start server-log
  evidence keep pull-request code and stale or replaced logs outside the
  protected runner boundary. Each driver runs in a uniquely named transient
  user scope, and failure artifacts contain only fixed validation categories
  and counters rather than server-log bytes.
- Add `rich_page_resume` for one retained runtime facade recovery claim over
  the never-attempted suffix of a partial rich-page receipt. Recovery re-proves
  page, type, and authored prefix evidence, and refuses uncertain or attempted
  boundaries.
- Verified authenticated Streamable HTTP interoperability with MCP Inspector
  2.1.0 and Claude Code 2.1.220, including lifecycle, SSE, catalog parity, and
  session termination.
- Add a spawned crash-restart acceptance owner that kills production children
  mid-upload, mid-import-dispatch, and mid-export-commit, then proves restart
  recovery: previous-generation handles return the byte-uniform `not_found`,
  the space holds at most one dispatched candidate, an interrupted export
  destination is absent or hash-correct, a second process on an owned staging
  root is rejected at startup without disturbing the first, and a full
  happy-path import succeeds after recovery. Three acceptance-only production
  points pause inside atomic export publication and after import and document
  dispatch to make the kill and cancellation windows exact.
- Prove offline that a staging cleanup pass never removes an unindexed entry,
  that the request-rate ceiling sheds a real wire burst and resumes, that the
  listener refuses duplicated and whitespace-bearing `Authorization` headers,
  and that query, prefix, oversized-path, and percent-encoded rejections all
  precede authentication. A dedicated failing-call burst now backs the
  diagnostic-flood row, asserting byte-uniform bounded refusals.
- Require every executed failure-robustness inventory row to state checkable
  evidence: an offline unit owner or a recorded live-owner run. A row cannot
  be promoted without evidence, and a live-only row cannot claim offline
  proof, so the implemented count can no longer drift from executable tests.
- Abort a local file export whose request cancellation is observed before the
  atomic publication link: the private temporary is discarded, the destination
  is never created, and the outcome classifies as a conflict. A cancellation
  arriving after publication has started never rolls the destination back.
- Record the first verified live-server runs for the gated cancellation owner
  (PART-08/09/10/12 through the export-prepublication, atomic-publication,
  import-post-dispatch, and document-post-dispatch points), the crash-restart
  owner (HAND-04, CRASH-01/02/03/05/07), the mid-frame capture (CRASH-06),
  direct failed-operation teardown (CLEAN-01/02/06), and the read-only
  catalog owners (CLEAN-07/08), and promote those sixteen rows to executed
  with live-owner evidence. The verified cancellation driver now follows the
  MCP contract: a cancelled request normally receives no response frame, and
  any response that does arrive must be the fixed conflict result. CLEAN-07
  asserts the fixed bounded read-only refusal and CLEAN-08 the strict
  schema refusal, matching the production dispatch design; the PART-10
  replay proves single dispatch through the idempotency `reused` flag and
  the PART-12 replay expects the definitive body-precondition conflict.

### Fixed

- Repair the live acceptance harness against the durable staging layout and
  live-server contracts: staging snapshots walk the closed durable layout
  and classify in-progress versus published records from their durable
  state; the spawned stdio driver runs its blocking transactions on the
  blocking pool so gate-race selects are actually raced; the acceptance-gate
  coordinator bounds its reach wait separately from its post-reach release
  deadline; single-dispatch proofs use idempotent replay because file
  objects never appear in the space object list; the HAND-07 cross-space
  probe uses a valid-format foreign space ID so the refusal is the staging
  space binding; and the second-owner startup rejection expects the durable
  layout's fixed `invalid staging policy` category.
- Send env-configured acceptance gate nonces at the full 64-hex-character
  length production requires, so spawned child gates arm instead of rejecting
  their configuration at startup.
- End the kill-mid-frame stdout capture exactly at its pause point instead of
  draining the pipe after the kill, so the truncated-fragment evidence is
  deterministic rather than dependent on flush timing.
- Signal a validator's process group only while its leader is still unreaped,
  observing exit with a no-reap wait first. The previous order signalled the
  group after the leader was reaped, leaving a window where a recycled pid
  could route the kill to an unrelated process group.
- Keep configured validator process groups under the kill-on-drop guard until
  stdout, stderr, exit status, and result-shape validation all succeed. A
  validator that leaves a descendant behind after oversized output or another
  refusal can no longer leak that process beyond the request boundary.
- Reap staged records whose live payload handle was surrendered — failed
  transfers, aborted writes, and shutdown racing an active upload — by
  reopening the payload through its durable identity, and persist terminal
  pathname-cleanup evidence only once an unlink target is in hand, so routine
  failures can no longer strand startup-blocking reconciliation evidence.
- Keep a failed staging allocation from poisoning the next startup: payload
  creation removes its own partial file, the allocation path removes its
  durable record after a payload failure, and restart reconciliation reaps an
  allocated record whose crash left a same-named payload behind.
- Leave expired staging records alone while a live import settlement or export
  stream holds their state lock; cleanup claims them on a later pass instead
  of persisting evidence underneath the active operation.
- Self-heal a staging layout left partial by a crash during first activation
  instead of refusing every later startup.
- Validate durable staging records at runtime against process-controlled
  invariants only. A stepped wall clock no longer closes staging authority,
  and records outside a newly lowered `staging_ttl` or `artifact_bytes` are
  reaped at restart instead of failing activation.
- Shed only the probing request when a staging authority check fails
  transiently (for example descriptor exhaustion under a connection flood);
  proven namespace replacement still closes authority permanently.
- Record a local file or document export whose waiter vanished as its proven
  terminal outcome when the commit demonstrably succeeded, instead of blanket
  indeterminate.
- Drain artifact settlements during stdio runtime shutdown and report the
  fixed durability category instead of a generic service-task failure.
- Reject ASCII control bytes and units in native artifact paths before I/O,
  and classify Windows reserved device components as invalid path syntax with
  the fixed validation response.
- Reject import and export root identifiers that collide after ASCII case
  folding, preventing platform- and operator-ambiguous aliases while
  preserving the configured spelling of non-colliding IDs.
- Validate local file and document export destinations before reserving their
  idempotency keys, and release reservations after other definite
  prepublication failures. Traversal, unknown roots, collisions, and staging
  preflight refusals now reject repeatedly without retaining unbounded
  in-flight operation entries; uncertain publication remains terminal and
  retains staging ownership for cleanup.
- Rewind retained artifact readers before Anytype uploads and hold an exclusive
  staging-record lease through validation and dispatch, preventing shared file
  offsets from producing empty or interleaved imports. Multi-chunk import
  verification now accepts a missing upstream ETag only when the complete
  streamed bytes match the source's expected SHA-256; exports still require a
  strong ETag. Artifact cancellation and timeout paths now emit their fixed
  controlled-failure diagnostic before returning. Validator inputs are rewound
  for every configured process; validator, upload, document, and staged-export
  reads use a bounded blocking pool with explicit offsets, so cancellation
  cannot corrupt a shared cursor or strand unbounded file work. Expiry cleanup
  releases the global record map before waiting on a stream and survives
  cancellation after record removal. The live chat-search check now allows a
  bounded indexing-convergence window before it reports a missing cleanup-owned
  seed.

---

## [Unreleased - 260806]

### Changed

- The production MCP process is now launched as `anyr mcp`, as all binaries
  are consolidated into anyr. This create contains all mcp functionality as a library.
- Maintenance commands are now `anyr mcp init` (create default config file)
  and `anyr mcp check` (check config syntax).

### Fixed

- Correct the README claim that artifact root activation intersects the
  configured policy with MCP client roots at activation time. The selected
  TOML policy is the only source of root authority, and no transport widens
  it; the client-root intersection is a separate session-scoped narrowing
  layer applied on stable stdio only (see Added). Replace the stale artifact
  data-plane roadmap section, which described the implemented `artifacts`
  registry as not yet selectable.
- Report selected TOML syntax and schema failures with redacted line, column,
  known schema path, and problem category instead of only the generic
  `invalid any-mcp TOML configuration` message.

### Added

- Complete terminal artifact data-plane acceptance for quota, TTL cleanup,
  create-new collisions, pre-dispatch cancellation, restart reconciliation,
  stale-generation rejection, bounded sequential staging ranges, exact raw MCP
  frame byte/token ceilings, and counts-only lifecycle diagnostics. Cleanup
  evidence is emitted only after private file removal succeeds, and spawned
  process capture rejects unconsumed stdout frames or non-allowlisted stderr
  fields.
- Narrow local artifact root authority on stable stdio with one bounded,
  session-scoped MCP `roots/list` snapshot. When the initialized client
  advertises the roots capability, a local artifact path is effective only
  when it lies beneath both a configured root and at least one client root, so
  a client root outside every configured root grants nothing and an empty
  snapshot denies every local root. The snapshot never widens the configured
  policy, is taken once per session, and `notifications/roots/list_changed` is
  ignored, so a changed client root needs a new session. A client that
  advertises no roots capability keeps the configured policy unchanged, as do
  preview stdio and the HTTP transport. A snapshot that cannot be frozen
  securely — transport failure, timeout, more than 64 roots, a duplicate
  alias, or a URI that is not a canonical local `file:` directory — disables
  local root operations for the rest of the session instead of falling back to
  the broader configured policy. Staged operations are unaffected, and client
  root URIs and display names never appear in diagnostics or receipts.
- Add artifact content acceptance scenarios — a representative MIME matrix,
  document canonicalization, and configured validators — to the multi-transport
  artifact harness. Every scenario runs on the scripted, stable, and preview
  stdio control planes over both the local-roots and staging data planes, and
  all of them must observe identical content evidence. The MIME matrix round
  trips binary, UTF-8 text, PNG, RIFF/WAVE, and out-of-tree payloads and
  compares declared, stored, and exported essence with verified byte lengths
  and hashes. The canonicalization scenario requires that re-importing an
  exported canonical body is reported as a no-op, and records every lossy
  difference as a closed category, including the appended plain-text hard
  break and importer escaping that rewrites the dispatched bytes before
  Anytype canonicalizes anything. The validator scenarios declare one real
  host `file(1)`-compatible executable pinned by absolute path and SHA-256 and
  probe matched, mismatched, and out-of-scope MIME declarations, so an
  optional validator reports the rejection while the import proceeds and a
  required one refuses it; `ANY_MCP_ACCEPTANCE_VALIDATOR` selects an exact
  executable when the host keeps one outside `PATH`. Select the new live
  target with `--features acceptance-harness --test headless_stdio_e2e
  headless_artifact_content_spawned_scenarios`.
- Add artifact policy and configuration acceptance scenarios — space policy
  omitted, empty, and restricted elsewhere, read-only mode, and disabled
  staging — to the multi-transport artifact harness. Each scenario is a
  complete server configuration executed across the direct router and the
  scripted, stable, and preview stdio control planes, and every control plane
  is required to report the same advertised catalog, the same artifact status
  projection, and the same refusal code and guidance. Probes classify before
  any Anytype write, so a denied configuration creates nothing and an accepted
  staging allocation is released within its scenario. An offline test loads
  every rendered scenario policy through the production TOML parser.
- Document operator setup for the artifact data plane in the README:
  environment-only credentials, registry and policy selection, local stdio and
  remote HTTP staging topologies, space policy shapes, read-only mode, and the
  complete limit, quota, and time-to-live table.
- Add a reusable multi-transport acceptance harness for the artifact data
  plane. `tests/support/artifact_acceptance.rs` declares a closed matrix of
  four control planes (scripted JSON-RPC frames, in-process direct router,
  spawned stable stdio, spawned preview stdio) crossed with two data planes
  (authorized local roots and the remote HTTP staging service), and an offline
  inventory test proves the direct-router and spawned targets together execute
  all eight transports exactly once. Every transport runs the same
  content-free smoke scenario — file import and export, document create,
  export and update, and an explicit staging allocate and release — compares
  its advertised catalog against the reviewed
  `tests/snapshots/artifact-catalog.snap` fixture, and then compares the
  complete executed matrix for exact byte-length and hash parity. The harness
  owns fixture discipline: a prefix-authorized disposable space, a private
  policy tree with owner-only sources, immediate registration of every created
  object and file, exact teardown, and rejection of skipped disposable
  admission. Setting `ANY_MCP_ARTIFACT_SERVER_LOG` audits the appended window
  of a captured Anytype server log by streaming it, and fails on any panic,
  fatal, or error class outside the already isolated upstream set while
  reporting only counts and fixed category names. Two ignored live targets
  select the matrix: the direct-router case in `server::headless_integration`
  and the spawned case in the `headless_stdio_e2e` acceptance-harness target.

- Configuration file search order: command-line `-c`/`--config`, then
  environment `ANY_MCP_CONFIG`, then `any-mcp.toml` in the current directory.
  If no file is found, use built-in defaults.

- Add an explicitly selected authenticated Streamable HTTP transport on a
  loopback-only `/mcp` listener. Stable protocol sessions support bounded SSE,
  principal-bound session IDs, process-lifetime idempotency partitions, fixed
  admission limits, exact Host and Origin policy, and graceful shutdown. The
  experimental protocol remains stateless. Authentication supports an
  owner-only static-token file or a bounded OAuth resource-server profile with
  startup-fetched JWKS and exact issuer, audience, subject, expiry, algorithm,
  and scope validation. Add real-socket process conformance coverage for
  authentication, lifecycle, CORS, session isolation and termination, catalog
  parity, limits, protocol negotiation, and secret-safe failures.
- Add load and fault boundary coverage for the Streamable HTTP transport. Each
  case drives one enforced boundary past its limit over real loopback sockets
  and proves the fixed shed-and-recover behavior: the session ceiling under
  concurrent initialize contention, the process-global request-rate budget
  under burst and across principals, the 2 MiB body ceiling at and one byte
  over the edge including a streamed body, the admitted-request concurrency
  cap, an idle SSE reader that disconnects, a slow reader that applies
  backpressure to its own connection only, drain-then-cancel graceful shutdown
  with work inside and outside the deadline, and an abrupt client disconnect
  during an in-flight mutation, which keeps exactly one upstream write and
  replays the recorded outcome on a keyed retry. The suite is deterministic and
  offline: gates and published counters replace sleeps, and one scripted
  loopback upstream stands in for Anytype.
- Link the default-off `artifacts` registry with eight payload-free workflows.
  Arbitrary MIME files and strict UTF-8 Markdown/plain text move through
  configured capability-relative roots or an authenticated loopback staging
  service; MCP arguments and results contain only logical locations, opaque
  handles, identities, counts, hashes, MIME evidence, and bounded receipts.
  Imports retain and revalidate source handles before one non-replayed Anytype
  mutation. Exports stream into create-new atomic destinations. Document create
  supports bounded typed properties; update requires the current canonical body
  hash and recognizes verified no-ops.
  Staging uses exact-size quota reservations, expiring secret handles,
  sequential resumable upload ranges, immutable full or single-range downloads,
  fixed Host/Origin/header policy, and explicit release. On Linux, optional
  startup-pinned `file-mime` validators run a fixed native-binary contract with cleared
  environment, bounded I/O, process and aggregate input limits, timeout, and
  required or optional failure policy. Add closed schemas, catalog token
  snapshots, strict text, handle, staging HTTP, idempotency, and typed
  fast/real-headless ownership tests.
- Add `any-mcp config init` to create an owner-only, valid starter policy with
  create-new semantics, and `any-mcp config check` to apply normal secure-file
  and schema validation without starting Anytype. Both accept `-c FILE` or
  `--config FILE`. Add `-V` and `--version` for the executable and Cargo
  package version, and accept `-c ABSOLUTE_PATH` as the server selector alias.
- Add a build-only manual release-workflow trigger with an explicit source
  branch or tag and a choice of every supported cargo-dist target or all
  targets. Manual runs cannot enter release or Homebrew publication jobs.
- Add the startup artifact policy foundation. `--config ABSOLUTE_PATH` takes
  precedence over `ANY_MCP_CONFIG`, with no filename discovery and safe
  no-file defaults. Selected owner-controlled TOML files use a closed version
  1 schema for deliberate writable space policy, separate import/read and
  export/create roots, staging and validator declarations, and bounded
  artifact, transfer, quota, TTL, cleanup, receipt, Markdown, and process
  limits. Logical root IDs support normalized Unicode letters, decimal digits,
  and combining marks while rejecting invisible characters. Physical roots
  and operation paths support native Unix bytes or Windows WTF-16 through
  canonical unpadded base64url. Activated roots retain directory handles,
  use capability-relative no-follow walks on Linux, macOS, and Windows, and
  allow MCP client roots only to narrow static policy. Imports reject unsafe
  ownership, permissions or ACLs, hard links, reparse points, and filesystem
  boundary changes. Exports write bounded owner-private temporaries and publish
  complete create-new destinations atomically without overwrite; cancellation
  and failures remove the private temporary. Roots, staging, and validators
  activate only when the `artifacts` registry is selected in writable mode.
- Enforce selected Anytype space policy through one frozen startup authority.
  Omitted and explicit-empty allowlists remain distinct; configured names
  resolve once to canonical IDs, and post-resolution aliases or duplicates
  fail closed. The runtime's policy-aware client checks every ordinary
  or response-bounded resolver result before domain I/O, while exact-ID
  document and file resources check the same authority before HTTP.
  Restricted policy rejects unscoped object search and creation of an
  identifier not knowable at startup. Restricted `space_list` scans under
  finite page and row ceilings, removes disallowed and malformed evidence
  before result construction, ignores upstream totals, and issues a cursor
  only after observing another permitted row. Catalog construction also
  rejects every tool or resource family without an explicit policy owner.
- Complete the terminal v1 acceptance matrix with 27 ignored library
  real-headless cases, all using prefix-authorized disposable spaces, and 21
  ignored spawned-stdio cases. The files workflow now proves native
  `text/plain` charset, image, and audio media types alongside binary ranges
  and hashes. An independent recursive audit covers every all-selected
  optional input/output schema across compact/standard and read-write/read-only
  catalogs, including closed `minProperties`/`maxProperties` constraints.
  Portable CI pins Rust 1.96, while protected Linux jobs run both complete
  inventories and reject invalid disposable admission or any reported skip.
- Add a versioned `skills/any-mcp` agent skill with progressively disclosed
  capability guidance and schema-checked PKM recipes for documents, files,
  collections, tags, tasks, chat, and save-link ingestion. It explicitly
  limits `anyr` fallback use to a best-effort chat listener and rich chat
  blocks, while documenting that any-mcp exposes neither a background
  subscription/atomic watermark nor styled chat writes.
- Add the reviewed R5 inert-bookmark constructor to `body_block_create` and
  `rich_page_create`. The closed `{ "kind": "bookmark", "url": string }`
  input maps only to `anytype-api` `NewBlock::bookmark`, dispatches ordinary
  `BlockCreate`, and requires exact `BookmarkState::Empty` readback with no
  target object. Metadata fetching, `BlockBookmarkFetch`, URL import,
  redirects, and fetch/update controls remain absent. Direct, stable-stdio,
  and preview-stdio real-server coverage independently verifies the stored
  URL and inert state. The executable `o200k_base` snapshots record a
  108-token read-write catalog increase, revised per-tool/domain/profile
  ceilings, and request, result, error, frame, and context cells below 200,000
  tokens.
- Add a terminal offline integration matrix for all six production optional
  registries. It locks exact compact/standard read-write and read-only
  inventories, canonical status under reversed selector order, stable/preview
  contract identity, transport requirement union, all 29 disabled stale tool
  calls, the disabled file resource, and all 18 read-only mutation calls before
  decoding or HTTP work. A six-cell leave-one-registry-out matrix repeats stale
  tool and resource rejection with the other five registries enabled. A
  reviewed aggregate token snapshot records all four composed catalogs and
  every registry's independent ceiling while the same test proves the four
  Phase 1 snapshots and `server_status` remain unchanged without a selector.
- Extend the typed live-scenario ownership audit to the exact production
  optional surface: 29 domain tools, `optional_toolset_status`, and the file
  byte-resource family. Every operation now owns one fast and one real-headless
  scenario binding, and catalog, binding, or executable-scenario drift fails
  deterministically. Seven compile-bound fast runners exercise every selected
  production route before I/O; six compile-bound spawned runners execute every
  registry's complete real-headless workflow.
- Add a cleanup-safe files acceptance workflow through spawned stable and
  preview production stdio children. It verifies upload, metadata, bounded
  ranges, byte-resource decoding and hashes, independent API download,
  protocol parity, redacted diagnostics, child termination, and exact cleanup
  against a real headless Anytype server.
- Add an aggregate stable read-write and preview read-only spawned sentinel
  with all six registries selected. It locks the complete catalogs and status,
  performs one real-backend read per registry, and proves a read-only stale
  mutation leaves independently observed collection state unchanged.
- Link the default-off `body-blocks` registry with one bounded list workflow
  and five mutation workflows for typed block creation, targeted update,
  confirmed subtree deletion, same-object movement, and finite rich-page
  plans. The tools use `anytype-api` only, return exact block identities and a
  canonical snapshot hash, fail closed on read restrictions and malformed
  bodies, and require fresh snapshot evidence before mutation. Process-local
  create idempotency, exact write-dispatch observation, semantic readback,
  digest-bound pagination, and complete/partial/indeterminate rich receipts
  prevent unsafe replay or false success. Read-only mode retains only the list
  tool; callers cannot provide bookmark fetch controls, raw protobufs, or
  opaque mutations. Direct, protocol, spawned-stdio, schema, token, and disposable
  real-server verification accompany the registry without a mock server;
  deterministic transport faults remain deferred to the P4 fault-injection
  design. The exact R5 six-tool schema snapshot is 24,394 `o200k_base` tokens,
  below the independently reviewed 25,108-token domain ceiling. Production
  gates count canonical request and dual-encoded result tokens, enforce
  complete-frame bytes, reject maximum dense legal values above their
  operation ceilings, and admit exact greatest-under boundary fixtures while
  keeping accepted paired exchanges below the 200K context floor.
  `body_block_create` uses 6,554 tokens under its reviewed 6,600-token
  per-tool ceiling. Further schema growth requires reduction or an explicit
  ceiling review. A production
  rich-prefix scheduler now owns verified receipts and permanently terminates
  every partial or indeterminate boundary; pending-candidate recovery reduces
  each bounded observation into one cached replay receipt without resuming the
  plan. Exact 2,048/2,049-block, UTF-16 endpoint, lifecycle-counter,
  protocol-aware raw stable/preview frame, descriptor, and atomic
  read-restriction regressions close the acceptance matrix. Raw-frame parity
  permits only the preview protocol's required `resultType: complete` on result
  envelopes; JSON-RPC error envelopes receive no exception. Envelope IDs,
  duplicated text, structured payloads, error code/message/data, and every
  domain value remain exact. Cross-fixture semantic evidence normalizes only
  generated identities, snapshot/cursor tokens, and the `encoded_len`-derived
  byte estimate of explicitly unsupported blocks; opaque kind and child count,
  all typed content, presentation, restrictions, ordering, status, errors, and
  idempotency remain exact. Domain tool-error evidence validates the complete
  `isError` result, exact code/message structure, ordered content, and
  canonical text duplicate; stale-cursor conflicts are compared across
  direct, stable stdio, preview stdio, and raw protocol frames. Read-only
  stable/preview processes execute the retained list tool against one
  cleanup-owned shared page before mutation predecode rejection. The
  independently created pagination pages use one transport-neutral title so
  their server-generated title blocks remain semantically comparable without
  normalizing text. Ignored body acceptance fails closed unless a
  parent-created, owner-private reviewed JSONL derivative contains one fresh
  run marker, at least one allow-listed event, and none of the configured HTTP
  or gRPC credential bytes. Its fixture-heavy shared workflow is heap-owned
  for default-stack execution.
  The production router returns optional-registry futures before constructing
  the unrelated Phase-1 aggregate, and the live 20-block pagination proof
  retains its cursor-bound limit across three exact pages. Create readback now
  proves the pre-derived insertion parent/index, exact prior structure and
  values, parent-only restriction refresh, and one closed canonical generated
  table subtree instead of misclassifying legitimate parent refresh as an
  indeterminate conflict. Opaque page roots now retain their exact kind and
  structural child count while allowing only the insertion parent's
  protobuf-derived approximate byte summary to refresh. Move readback applies
  that exception only to the old and new structural parents and now also
  proves exact post-move DFS order. Delete readback applies it only to the
  removed subtree's direct parent and proves the exact surviving DFS order.
  Rich table plan accounting conservatively includes both layout regions and
  logical `rows × columns` capacity. API/MCP receipt verification separately
  requires Heart's exact sparse table, ordered regions/rows/columns, no cells
  without a header, and grey-background empty paragraph cells under the first
  header row only, with no missing, extra, foreign, or nested nodes.
  Value-only update readback permits only the opaque page root's derived byte
  summary to refresh while preserving its exact kind, unchanged child count,
  structure, restrictions, and presentation. Every body editor now caps the
  inherited client verification policy at three attempts without widening a
  smaller attempt or timeout policy. Live primitive mutation metrics accept
  the designed one-to-three semantic verification rounds instead of requiring
  the first read to observe the write, while retaining one write and exact
  close/fallback/limit counters.
- Document the feasibility and security limits of a future object-tag
  exclusion policy. REST object, search, and saved-view pages already include
  assigned select and multi-select tags, while canonical collection
  membership remains identity-only and cannot be safely post-filtered without
  pagination leakage. No `never-access` policy is implemented or advertised;
  global search, chat/discussion inheritance, link/embed behavior, schema
  protection, and mutation races still require a reviewed design.
- Add the production-unlinked `discussions` candidate with exactly one
  read-only `object_discussion_get` workflow. It resolves one explicit space,
  validates one exact parent ID, and reuses the typed `anytype-api` attached
  discussion read to return a closed `absent`/`attached` union containing only
  stable IDs. The registry requires authenticated HTTP and gRPC, remains
  available unchanged in read-only mode, performs no writes, exposes no
  comments, and leaves the linked `chats` contracts unchanged. Strict schema,
  default-off, error, redaction, byte, token, direct-router, stdio, and
  cleanup-owned real-server coverage accompany the candidate. The shipped
  server rejects both its selector and stale method calls. Production
  acceptance remains blocked because the mandatory configured read-only
  fixture contains a `DiscussionObject` whose unique key violates Heart's
  exact deterministic parent binding; no legacy-key allowlist is accepted.
  The upstream fixture must be corrected or migrated and the viewer-positive
  test must pass. A non-default test-owned binary supplies real stable and
  preview OS-process coverage without changing the shipped inventory; exact
  scope/layout, input, cancellation, deadline, authentication, redaction,
  result, and work matrices accompany it. Transport fault injection remains
  deferred to P4.
- Add one transport-neutral, cleanup-owned real-server scenario for ordinary
  MCP workflows across representative Basic, Collection, Grid, filtered, and
  Kanban layouts. Direct routing and the shipped stable/preview stdio process
  use only `type_list`, `view_list`, `view_object_list`, `object_update`, and
  `collection_member_list`/`add`/`remove`: status-column movement remains an
  ordinary Select-property update, and canonical membership is independently
  proved complete even when a saved view hides members. Both view and
  membership paths paginate at `limit: 1`; no layout-specific tool, mock
  server, or fault server is introduced. Deterministic fault cases remain
  deferred to the P4 fault-injection design, and the persistent read-only
  fixture is never mutated. Configure the shipped Tokio runtime with a finite
  8 MiB worker stack so aggregate typed dispatch remains reliable during
  filtered layout inventory in debug and production builds, lock that worker
  flavor and stack behavior with an isolated regression probe, and register
  every spawned child for guaranteed stop-and-wait before disposable cleanup.
- Link the complete default-off `chats` production registry through HTTP-only
  `anytype-api` workflows. Read-write mode exposes exactly `chat_list`,
  `chat_message_list`, `chat_message_get`, `chat_message_search`,
  `chat_message_add`, and `chat_message_delete`; read-only mode retains the four
  reads, with common `optional_toolset_status` counted once. The immutable
  descriptor composes the independently reviewed slices with per-runtime
  mutation state, no resources, templates, gRPC, rich-message, streaming, or
  deferred names. No-selection Phase 1 catalogs, status, transport, and token
  output remain byte-identical. A canonical `o200k_base` snapshot locks all
  six per-tool costs, the 5,609-token read-write and 3,811-token read-only
  domain totals beneath 8,500/6,500 ceilings, full profile/access/mixed
  catalogs, and the component adversarial result boundaries. Direct stable and
  preview dispatch preserve identical contracts; absent and read-only calls
  stop before decoding or HTTP. The cleanup-owned real-server scenarios cover
  list, history, search, exact get, add/replay/conflict, delete, and verified
  absence without a mock server. Synthetic latency, connection, malformed,
  forced-5xx, and retry-maximum cases remain deferred to the P4 fault-injection
  design.
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
- Revise the rich body-block tool and API model contracts to R4 pending
  independent rereview. The default-off six/one `body-blocks` registry
  now has exhaustive closed projection, constructor, update, and result shapes;
  explicit checkbox, alignment, background, link-relation, divider, and
  YouTube-ID rules; fail-closed read restrictions; complete structural-plan and
  retained-replay semantics; exact partial/authentication/candidate outcomes;
  and paired request-plus-result token proofs below 200,000 tokens. Production
  linkage requires `any-2f0g.35` plus independent review `any-2f0g.36`: a
  bounded `anytype-api` Show/Close/write lifecycle with a 4 MiB Show cap, 64 KiB
  cap for every non-Show body response including both close paths,
  cancellation-resilient cleanup, one absolute deadline, and an exact write-poll
  certainty seam. R4 also fixes emoji/callout input at 64 bytes, validates both
  UTF-16 endpoints as `u32` scalar boundaries, closes relation-key grammar and
  unique update arrays, and makes successful pending-candidate replay retain an
  index-zero partial receipt without resuming writes. Direct, stable-stdio, preview-stdio, and
  cleanup-owned real-server acceptance covers relation detection/removal/move,
  targeted update, heading append, and rich construction. The removed semantic
  gRPC mock/custom server is prohibited; synthetic latency, connection,
  malformed/status, and retry faults remain P4 behind reviewed fault injection.
- Map verified body-block mutation uncertainty from `anytype-api` directly to
  the fixed secret-safe mutation-indeterminate conflict result and runtime
  category; this is plumbing for the separately tracked optional body workflow
  tools.
- Classify the new `anytype-api` `AnytypeError::BodyGraph` variant in the
  exhaustive error and health mappings (`ToolErrorCode::Upstream`, health
  status `body_graph`). Plumbing only: no `any-mcp` tool can surface a body
  read yet.
- Define a bounded tagged MCP filter DTO model (any-2f0g.4.1): format/condition
  matrix, one-to-one anytype-api mapping, excluded combinations and upstream
  limitations, hard bounds, cursor binding rules, and the shared-module
  conversion strategy.
- Define a typed, bounded, fail-closed anytype-api body block model
  (any-2f0g.18) over ObjectShow and block RPCs —
  BodySnapshot/BodyBlock trees, closed v1 content/style/mark variants, opaque
  unsupported reads, graph validation, context/space ownership, verified
  mutation evidence, limits, and forward compatibility.

### Changed

- Describe ignored live stdio scenarios by target and behavior instead of
  maintaining stale exact case counts in public conformance documentation.
- Remove hand-maintained exact ignored-test case counts from the README so
  acceptance coverage changes cannot silently stale the documentation.
- Move stdio compatibility evidence under `any-mcp/docs/` and update its public
  links while removing stale links to non-public acceptance and design
  artifacts.
- Reconcile numeric and checkbox filter documentation with current live
  acceptance and the workspace-wide direct-client matrix. The production MCP
  contract remains unchanged: typed filter values pass through once, checked
  server pagination remains authoritative, and no result-page emulation is
  introduced.
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

- Heap-own fixture-heavy production-router dispatch futures so the complete
  direct live matrix runs on Rust's default test-thread stack without overflow.
  Protected headless jobs now export the dedicated disposable-process gate,
  require the exact 1..485-character ASCII prefix grammar, and cannot report
  success when a live callback was skipped.
- Stabilize the chat-add idempotency deadline regression by separating
  deadline-independent terminal, capacity, and pre-dispatch state assertions
  from Tokio virtual-time waiter deadlines. Production still rejects an
  expired invocation before cached, conflict, or capacity outcomes and waiters
  still observe the earlier of the leader and caller deadlines.
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
