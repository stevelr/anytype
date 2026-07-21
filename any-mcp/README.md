# any-mcp

A workflow-oriented Model Context Protocol server for Anytype.

`any-mcp` is designed for reliable agent workflows such as discovering, reading, and safely editing documents.

**Status: Pre-release, under active development. Only tested on Linux and MacOS.**

This is not intended to replace [anytype-mcp, the official MCP server](https://github.com/anyproto/anytype-mcp)
which wraps the OpenAPI. They are complementary.

Use anytype-mcp for:

- simplicity
- official distribution and low-friction installation
- breadth (currently)
- one-shot operations

Use any-mcp (this) for:

- reliable workflows
  - safer mutations
  - concurrency, timeouts, cancellation
  - pagination and response-size limits
  - more specific errors
- more stable/predictable token costs
- multiple catalogs - select based on your model's context limits
- functionality only available through APIs
- stronger credential handling

## Quick start

Install by downloading the `any-mcp` release from the github releases page.

Confirm that the existing `anyr` credentials can reach Anytype:

```sh
anyr auth status --pretty
```

An MCP host starts that binary and communicates with it over stdio.
The server does not perform login or print credentials. It reuses the endpoint
and keystore selected by `ANYTYPE_URL`, `ANYTYPE_GRPC_ENDPOINT`,
`ANYTYPE_KEYSTORE`, and `ANYTYPE_KEYSTORE_SERVICE` (default `anyr`). The default
startup is the stable `2025-11-25` protocol, the four-tool compact profile, and
read-write access. For a safer first registration, select compact read-only
explicitly:

```json
{
	"mcpServers": {
		"anytype": {
			"command": "/absolute/path/to/any-mcp",
			"env": {
				"ANY_MCP_PROTOCOL": "stable",
				"ANY_MCP_PROFILE": "compact",
				"ANY_MCP_READ_ONLY": "1",
				"ANYTYPE_URL": "http://127.0.0.1:31009",
				"ANYTYPE_KEYSTORE": "file:path=/replace/with/your/anytype-keys.db",
				"ANYTYPE_KEYSTORE_SERVICE": "anyr"
			}
		}
	}
}
```

Use a platform-appropriate keystore path or keep the host's existing Anytype
environment instead of copying credentials into configuration. When
`ANYTYPE_KEYSTORE=env`, supply `ANYTYPE_KEY_HTTP_TOKEN` only through the host
environment or another secret facility; never put its value in prompts, tool
arguments, or logs.

Codex uses the same settings in `config.toml` and can forward the operator's
existing non-secret selectors:

```toml
[mcp_servers.anytype]
command = "/absolute/path/to/any-mcp"
env = { ANY_MCP_PROTOCOL = "stable", ANY_MCP_PROFILE = "compact", ANY_MCP_READ_ONLY = "1" }
env_vars = [
  "ANYTYPE_URL",
  "ANYTYPE_GRPC_ENDPOINT",
  "ANYTYPE_KEYSTORE",
  "ANYTYPE_KEYSTORE_SERVICE",
]
```

See [stdio protocol verification](STDIO_CONFORMANCE.md) for the tested Codex,
Claude Code, and MCP Inspector registration commands and their exact pinned
protocol revisions. Client registration is separate from Anytype login: create
and store credentials with `anyr` or Anytype before starting the MCP host.

## Phase 1 foundations

The crate provides an authenticated stdio runtime, a complete static Phase 1
tool and resource catalog, and bounded wire contracts for every workflow.

### Authenticated stdio runtime

The server keeps one authenticated `anytype-api` client alive for the process
and serves MCP over stdin/stdout. At startup it loads credentials using the
same environment and keystore configuration as `anyr`, requires a successful
HTTP ping, and checks gRPC whenever gRPC credentials are configured. The
standard read-write catalog additionally requires configured, healthy gRPC
because `object_archive` proves archived presence through Anytype's gRPC search
surface. Compact read-write and both read-only catalogs can start HTTP-only.
Configured-but-unhealthy gRPC always fails startup, even for an HTTP-complete
catalog. Startup failures exit non-zero before protocol output with a concise
diagnostic on stderr.

Supported Anytype settings:

- `ANYTYPE_URL` and `ANYTYPE_GRPC_ENDPOINT` select endpoints;
- `ANYTYPE_KEYSTORE` selects the keystore (`env` supports no-persistence
  deployments using `ANYTYPE_KEY_HTTP_TOKEN` for HTTP and either
  `ANYTYPE_KEY_ACCOUNT_KEY` or `ANYTYPE_KEY_SESSION_TOKEN` for optional gRPC);
  and
- `ANYTYPE_KEYSTORE_SERVICE` selects the existing credential service and
  defaults to `anyr` for compatibility.

Operational settings are bounded defensively:

All numeric settings below require an integer of at least 1 as well as the
stated maximum.

- `ANY_MCP_PROTOCOL` is absent or exactly `stable` for the production
  initialize-based protocol. Exact value `experimental-2026-07-28` enables the
  stateless preview; every other value fails startup before authentication;
- `ANY_MCP_PROFILE` accepts exactly `compact` (default) or `standard`;
- `ANY_MCP_MAX_CONCURRENCY` defaults to 8 and has a maximum of 64;
- `ANY_MCP_REQUEST_TIMEOUT_SECS` defaults to 30 and has a maximum of 300;
- `ANY_MCP_STARTUP_TIMEOUT_SECS` defaults to 15 and has a maximum of 120;
- `ANY_MCP_READ_ONLY` accepts exactly `0` (default) or `1`; `1` omits all
  mutation tools and rejects stale direct mutation calls before decoding or I/O;
- `ANY_MCP_JSON_RESPONSE_BYTES` defaults to 8 MiB and has a maximum of 64 MiB;
  and
- `ANY_MCP_DOCUMENT_RESPONSE_BYTES` defaults to 64 MiB, has a maximum of 64
  MiB, and must be at least the ordinary JSON budget.

Protocol mode, catalog profile, and read-only access are independent startup
selectors. Each uses an exact fail-closed grammar and cannot be changed by MCP
input after startup.

The response budgets are enforced while chunks arrive, before workflow
pagination or projection. A truthful oversized `Content-Length` fails before
body allocation, and absent or misleading framing cannot exceed the streamed
total. Oversized upstream JSON maps to the stable `bounded_result` tool error;
the error carries no upstream body, URL, or credential. File downloads use
the separate finite `anytype-api` raw-file policy rather than either MCP JSON
budget, while SSE chat events remain incremental streams.

The 64 MiB document default is a compatibility ceiling, not a normal
allocation target: a valid 10 MiB markdown body can approach 60 MiB after JSON
escaping, and `object_get` must receive that complete upstream JSON before it
can return character chunks. Buffers grow incrementally. In the worst case,
the default concurrency of 8 permits roughly 512 MiB of document response
buffers plus decoding and result overhead. Operators with smaller documents
or tighter memory limits should lower `ANY_MCP_DOCUMENT_RESPONSE_BYTES` and/or
`ANY_MCP_MAX_CONCURRENCY`; oversized documents then fail explicitly with
`bounded_result`.

Every workflow handler uses the runtime execution seam, which includes permit
wait in its timeout and observes request cancellation. The client
is shared without a mutex held across upstream awaits. Closing stdin cleanly
closes the permit pool, signals the process shutdown token, cancels running and
waiting operations, and drains the selected protocol service.

Each operation emits a structured completion diagnostic by default with a
validated static operation name, a monotonic per-runtime server correlation
ID, elapsed milliseconds, outcome, and sanitized upstream category/status.
Handler conversion and result-encoding failures remain inside this operation
boundary and emit distinct fixed categories instead of a false success.
The correlation ID is generated by `any-mcp`, not copied from the raw
peer-controlled MCP request ID. Error values, URLs, bodies, credentials, and
raw MCP IDs are never formatted. Operators can explicitly override the
`any_mcp::operation=info` level through `RUST_LOG`.

### Protocol and wire-contract boundaries

- [`rmcp`](https://docs.rs/rmcp/) 2.2.0 with the `server`, `macros`, `schemars`,
  and `transport-io` features;
- production advertises rmcp's latest released protocol, exactly `2025-11-25`,
  and uses the standard `initialize`/`notifications/initialized` lifecycle.
  Released revisions from the oldest explicitly regression-tested revision,
  `2024-11-05`, through `2025-11-25` negotiate on that lifecycle. Unknown
  revisions fall back to the stable server default. Protocol negotiation is
  between MCP hosts/clients and the server; language models do not select a
  wire revision;
- stateless MCP `2026-07-28` is compiled and schema-tested but available only
  with `ANY_MCP_PROTOCOL=experimental-2026-07-28`. Its `server/discover`,
  per-request version/capability metadata, optional validated client identity,
  `-32022` fallback, result discrimination, and cache hints share the same
  server handler/catalog implementation as stable mode. A first request can
  never opt an ordinary process into this preview, and stable startup rejects
  an initialize request for the compiled preview revision;
- preview responses include `resultType: complete`; discovery and the static
  tool/resource catalogs carry positive public cache hints, while authenticated
  document reads are immediately stale and private. Unsupported versions use
  error `-32022` with exact `supported` and `requested` data;
- newline-delimited input/output frames in both eras are capped at 2 MiB. The
  stable transport preserves rmcp dispatch while a cancellation-safe decoder
  returns one `-32700` response with explicit `id: null` per syntactically
  malformed frame and one `-32600` response per oversized or well-formed
  invalid frame. Valid JSON-RPC notification shapes never receive a response,
  including when their parameters cannot be decoded. Decoder and service
  responses share one stdout writer. The preview path allows at most 64 active
  requests; both paths preserve cancellation and prompt EOF shutdown;
- preview request IDs accept every bounded string, including the schema-valid
  empty string, plus exactly represented signed/unsigned JSON integers. Strings
  are capped at 256 bytes and integers at serde_json's exact i64/u64 range as
  deliberate transport resource/response-correlation bounds;
- an `anytype-api`-only application dependency through the `anytype` crate;
  `any-mcp` never depends directly on generated `anytype-rpc` support;
- reusable strict JSON Schema 2020-12 input/output contracts with
  `additionalProperties: false`, bounded domain strings, stable object
  summaries, and canonical
  `anytype://spaces/<space_id>/objects/<object_id>` resource URIs;
- reusable pagination defaults of 20 and a hard maximum of 100, with opaque,
  versioned continuation cursors bound to normalized query parameters and the
  issuing server process;
- at most 4,096 live process-local cursors and 65,536 bytes of normalized query
  material per cursor fingerprint. Body chunk metadata is capped at
  100,000,000 total Unicode characters, while the configured document-response
  byte ceiling normally becomes the tighter complete-body bound;
- transport-neutral handler helpers that execute upstream calls and bounded
  conversion under the runtime controls, encode only through the declared
  typed contract, verify upstream offset/limit and result count before cursor
  issuance, and advance continuations from the checked upstream page window;
- deterministic object adapters with explicit summary-only, selected-property,
  and fail-closed bounded-all projection modes; projected values cover every
  Anytype property format with closed finite wire schemas and never include a
  body, snippet, or unrequested property, and summary modification timestamps
  are validated as nonempty bounded RFC 3339 date-times;
- Unicode-safe document body chunks defaulting to 20,000 characters and capped
  at 100,000, plus reusable caps for identifiers, projections, filters, filter
  values, and filter nesting;
- fail-closed rejection of free-form JSON/maps, unbounded arrays and strings,
  impractically bounded numbers, undiscriminated unions, and unsupported
  patterned-object or tuple-array schema applicators;
- typed tool contracts that link each validated output schema to its success
  encoder and select only the exact read, create, or destructive-update
  annotation profile;
- compact JSON text fallbacks matching each typed `structuredContent` result;
  and stable, bounded, secret-safe execution error bodies that convert
  resolver-provided candidate ids and names, discard malformed alternatives,
  refuse empty ambiguity output, and classify resolver scan limits as bounded
  results; and
- diagnostics use a tracing subscriber whose writer is always stderr; the
  `anytype-api` HTTP targets are metadata-only at every trace level. That
  guarantee does not cover other dependency targets, so the server still
  denies all `anytype` and `rmcp` target prefixes through a non-overridable
  metadata filter outside `RUST_LOG`; this whole-prefix filter is required
  defense in depth, not a redundant HTTP-only safeguard.

### Production catalog profiles and read-only mode

| Startup selection                   | Read-write tools | Read-only tools |
| ----------------------------------- | ---------------: | --------------: |
| default / `ANY_MCP_PROFILE=compact` |                4 |               3 |
| `ANY_MCP_PROFILE=standard`          |               14 |              10 |

| Tool               | Compact | Standard | Bounded workflow                                                                              |
| ------------------ | :-----: | :------: | --------------------------------------------------------------------------------------------- |
| `server_status`    |    ✓    |    ✓     | Redacted endpoint, selected profile/access, startup availability, and stable enabled toolsets |
| `object_search`    |    ✓    |    ✓     | One checked page of summaries with bounded filters, projection, and cursor                    |
| `object_get`       |    ✓    |    ✓     | One exact object with bounded properties and optional body chunk/full-body hash               |
| `object_edit`      |    ✓    |    ✓     | Ordered exact-match whole-body edit with required hash and one PATCH                          |
| `space_list`       |         |    ✓     | One checked page of space summaries                                                           |
| `type_list`        |         |    ✓     | One checked page of types in one resolved space                                               |
| `property_list`    |         |    ✓     | One checked property page, optionally scoped to a resolved type                               |
| `tag_list`         |         |    ✓     | One checked tag-option page for one resolved select property                                  |
| `template_list`    |         |    ✓     | One checked page of body-free template summaries                                              |
| `view_list`        |         |    ✓     | One checked page of views for one list object                                                 |
| `view_object_list` |         |    ✓     | One selected view page with explicit bounded projection                                       |
| `object_create`    |         |    ✓     | One POST, bounded verification, and optional process-lifetime idempotency key                 |
| `object_update`    |         |    ✓     | Explicit whole-field replacement with optional body-hash precondition and one update          |
| `object_archive`   |         |    ✓     | One soft-delete dispatch with bounded state confirmation                                      |

Read-only mode removes `object_edit` from compact and `object_create`,
`object_update`, `object_edit`, and `object_archive` from standard. Every
retained tool keeps the identical complete contract and handler.

Standard read-write startup requires both HTTP and gRPC availability. Missing
gRPC fails admission rather than dynamically omitting `object_archive`, so all
four catalog inventories remain exact: compact read-write 4, compact read-only
3, standard read-write 14, and standard read-only 10. Missing HTTP fails every
selection.

`tools/list` is a static, cursor-free catalog selected once at startup with
`ANY_MCP_PROFILE`. The default `compact` profile advertises the coherent
existing-document workflow `server_status`, `object_search`, `object_get`, and
`object_edit`. Set `ANY_MCP_PROFILE=standard` to advertise exactly 14 tools:
`object_archive`, `object_create`, `object_edit`, `object_get`,
`object_search`, `object_update`, `property_list`, `server_status`,
`space_list`, `tag_list`, `template_list`, `type_list`, `view_list`, and
`view_object_list`. `ANY_MCP_READ_ONLY=1` is orthogonal: it omits
`object_edit` from compact and the four `object_*` mutations from standard.
The catalog is built once from the same typed contracts used by dispatch and
then filtered, so a shared tool name has an identical complete description,
input schema, output schema, annotation, and handler in every profile.
Unknown or non-Unicode profile values fail startup without echoing their value.

Only `compact` and `standard` exist. Proposed `schema`, `members`,
`views-write`, `files`, `chats`, and `admin` toolsets are not implemented or
selectable in this release.

The server also advertises static resource and tool capabilities without
`listChanged` or resource subscriptions. `resources/templates/list` exposes
the canonical `anytype://spaces/{space_id}/objects/{object_id}` document
template, `resources/list` is intentionally empty, and `resources/read`
returns one complete bounded Markdown document.

### Status and schema discovery handlers

The discovery handlers are exposed as typed production tools. `server_status`
returns only the selected application profile, read-only state, a parsed and
redacted HTTP endpoint, API revision, startup probe availability, and enabled
toolsets. Compact reports `core` and `documents`; standard additionally reports
`discovery`, `properties`, `templates`, and `views`; read-write standard also
reports `create` and `advanced_mutations`. URL user information, passwords, query
parameters, and fragments are removed before encoding.

`space_list`, `type_list`, `property_list`, `tag_list`, and `template_list`
each request exactly one explicit upstream page and use the shared opaque
cursor integrity checks. Space, type, and property references use the bounded
`anytype-api` resolvers, so ambiguity returns actionable candidate IDs instead
of selecting an arbitrary match. Type-scoped property discovery filters one
upstream property window against the resolved type's linked property IDs;
sparse pages still advance by the checked upstream window.

Property summaries never contain tag options. Select and multi-select counts
come from a separate `tags(...).limit(1).offset(0)` page's bounded `total`;
the handler also verifies that zero, one, and larger totals agree with the
first-page item count and continuation flag. Callers use `tag_list` to retrieve
options explicitly. Before that tag page, `tag_list` verifies the resolved
property through one cache-independent scoped GET; a cold client cache never
causes an implicit all-properties scan. Template results reuse the summary-only
object adapter and therefore contain no body or implicit property projection.

Local TCP fixture tests exercise the real `anytype-api` fluent builders and
verify exact paths and decoded queries for every paginated discovery handler,
including page continuation, sparse pages, cursor mismatch without I/O,
resolver errors, response ceilings, and secret-safe upstream failures.

### Object archive workflow

The transport-neutral `object_archive` handler soft-archives exactly one
object through the ordinary Anytype object DELETE endpoint. It never invokes
archived-object purge, bulk deletion, delete-all, or space mutation APIs. The
handler resolves the space, reads the active object, and validates its exact
safe object, space, and type identities before mutation. It marks dispatch
immediately before one non-replayed DELETE under the shared runtime controls
and document-response ceiling.

The DELETE response is dispatch evidence only and can never establish success.
After every non-definitively-rejected dispatch—including a matching,
false, malformed, mismatched, oversized, transport, timeout-status, redirect,
or other uncertain response—the handler performs finite independent
read-after-write confirmation instead of another DELETE. Within hard attempt,
time, page, and item caps, confirmation must prove the exact id absent from the
active HTTP object surface and present in the original-type-scoped archived
gRPC search surface. Unproven, incomplete, unavailable, or unsafe evidence
returns fixed mutation-indeterminate guidance. Definitive authentication,
validation, not-found, conflict, and rate-limit rejections retain their
ordinary errors.

Its typed result contains the archived object id, the confirmed boolean state,
and the canonical Anytype resource URI. The tool contract is destructive,
non-idempotent, read-write, and closed-world. A reusable mutation-access gate
rejects stale direct calls before resolver or upstream I/O when the production
catalog selects read-only operation.

### Shared mutation values

Object create and update use one closed property and icon contract. Property
keys and relation identifiers are path-safe and bounded, scalar values are
finite, numbers and RFC 3339 timestamps have canonical semantic forms, and
multi-select, file, and object identifiers are sorted and deduplicated after a
raw-input cap. Empty string and list clears can match an omitted returned
property only after the handler validates that key and format against the
effective object type; select, number, date, icon, and name clears are not
invented where the upstream API has no distinct supported form.

Mutation handlers also share an opt-in one-way dispatch marker. Cancellation,
request timeout, or shutdown before the first write poll retains the ordinary
redacted upstream result. Once a write may have been dispatched, the same
controlled failures return a fixed `conflict` result stating that the mutation
may have applied and requiring a reread before retry. The marker is cloneable,
atomic, sticky, and created once per invocation; normal operation errors remain
the handler's responsibility to classify explicitly. Create and update share a
conservative rejection classifier: local validation and authorization failures
and a small allowlist of definitive HTTP rejection statuses may return their
ordinary error, while timeouts, transport failures, malformed or oversized
responses, exhausted retries, HTTP 408 and unrecognized 4xx/5xx statuses are
indeterminate after dispatch. The classifier uses only variants and status
codes and never incorporates upstream text.

The same classifier consumes `anytype-api`'s secret-safe authentication seam:
explicit nested gRPC authentication rejections return the fixed
`authentication` result and are definitive after dispatch, while non-auth gRPC
transport and operation failures remain redacted `upstream` or
mutation-indeterminate results. `any-mcp` never depends directly on
`anytype-rpc` or formats its source diagnostics.

### Object update workflow

The transport-neutral `object_update` handler replaces only fields explicitly
supplied by the caller. Omitted name, body, properties, type, and icon fields
remain unchanged, and JSON `null` is rejected rather than treated as omission.
`body_markdown` is a complete body replacement; an empty string is its explicit
clear form. Replacement bodies accept at most 100,000 Unicode characters and
remain subject to the 10 MiB document-byte ceiling. Empty text, URL, email, and
phone strings plus empty multi-select, file, and object lists are the only
property clear forms. Select, number, date, checkbox, name, and icon clearing
are not advertised because the upstream object-update API has no distinct
supported clear form.

Anytype's canonical read form is distinct from its safe write form for a
closed plain-line subset. Across create, update, and exact edit, empty bodies
and single lines containing Unicode alphanumeric characters, internal ASCII
spaces, and underscores are mapped to one unescaped write form and one exact
canonical form (escaped underscores plus `"   \n"`). Raw and already-canonical
inputs therefore share the same verified body and do not double-escape on
replay. Canonical expansion counts against both body ceilings before I/O.
Punctuation, multiline Markdown, and ambiguous backslash forms remain
byte-exact; a server rewrite of those unsupported forms fails closed.

Before writing, the handler resolves the complete effective object type,
rejects archived or malformed type metadata, and requires every supplied
property key and format to match its schema exactly. Property assignments are
sent in deterministic key order, and semantic verification accounts for
canonical numbers and timestamps plus reordered or deduplicated set values.

Callers can supply the complete-body SHA-256 returned by `object_get` as
`expected_body_sha256`, including when guarding a non-body mutation. The
handler reads and hashes the complete current body under the document response
ceiling and returns `conflict` before the single update request when it is
stale. A body replacement without this precondition is allowed, but can
overwrite a concurrent edit. Anytype does not provide an atomic compare-and-
swap primitive, so a best-effort race remains between the precondition read and
the update.

After one update request, the handler performs bounded semantic GET retries for
eventual consistency and verifies safe object/space identity, the effective
type, every requested observable field, and the relevant complete body hash.
A malformed or mismatched update response, transport uncertainty, exhausted
verification, or cancellation, timeout, or shutdown after dispatch returns the
fixed `conflict` outcome requiring a reread before retry. A definitive 4xx
response remains an ordinary classified error. Results contain only the
bounded updated summary, canonical resource link, and body hash when a body or
hash precondition was supplied; they never echo the document body.

### Exact-match object edit workflow

The transport-neutral `object_edit` handler applies at most 100 ordered
literal replacements to one complete Markdown body. `old_text` is nonempty,
`new_text` may be empty to delete matches, and `expected_matches` defaults to
one and is capped at 1,000. Matching and replacement are left-to-right and
non-overlapping. Each edit sees the result of every preceding edit, so order is
part of the request semantics. Each fragment and every intermediate body stay
within the 100,000-Unicode-character body limit and shared document-byte
ceiling; expansion is checked before allocating the replacement body.

`expected_body_sha256` is required and hashes the exact complete current body.
The handler resolves and validates the space and stable object identity, reads
the complete bounded body, then checks the hash and every sequential match
count before polling a write. A stale hash or count mismatch returns the fixed
`conflict` result after the read and sends no PATCH. If all preconditions hold,
the handler sends exactly one whole-body PATCH and performs finite semantic
GET verification of the new complete-body hash. Anytype has no atomic compare-
and-swap primitive, so another writer can still race between the precondition
read and that PATCH.

Definitive rejection, including HTTP 429, remains an ordinary classified
error. HTTP 408, redirects, transport or server uncertainty, malformed or
oversized responses, verification exhaustion, and cancellation, timeout, or
shutdown after dispatch return the fixed mutation-indeterminate `conflict`
guidance even when a recovery read happens to match. Results contain only the
bounded object summary, canonical resource link, and verified new SHA-256;
they never return the body.

### Object create workflow

The transport-neutral `object_create` handler sends exactly one POST and uses
bounded semantic verification to retry stale or transient GETs before reporting
success. Space and full non-archived type references use the bounded
`anytype-api` resolvers. Optional templates use the public direct-id or exact
1,000-row resolver and are fetched by id to revalidate archive, space, and type
id/key for the generic template object; the endpoint path scopes the owning
object type. The immediate POST response and final verification GET are both
revalidated. A success requires safe matching object, space, and type id/key
plus semantic agreement for each caller-supplied name, Markdown body, icon,
and typed property in both representations. The MCP result contains only a
bounded object summary and canonical resource link—not the body or an implicit
property projection.

All optional fields reject explicit JSON `null`; omission means that the field
is absent from the create payload. Names are nonempty, while an explicitly
empty Markdown body is sent. Empty property lists mean no assignments and
empty relation lists explicitly clear those assignments. Create consumes the
shared closed mutation values: property keys are strict ASCII, numbers and RFC
3339 timestamps are canonical, set-valued identifiers are capped before being
sorted and deduplicated, and all eleven current property formats and three icon
forms are bounded. Markdown input accepts at most 100,000 Unicode scalar
values.

The shared plain-line representation contract described above also governs
create normalization and idempotency. Create stores the exact expected
canonical form in its normalized input before fingerprinting and semantic
verification, then derives the separate unescaped wire form immediately before
the POST. Leading or trailing spaces, other newline forms, Markdown
punctuation, ambiguous backslashes or escapes, and multiline bodies remain
byte-exact. If Anytype rewrites one of those unproven forms, verification
returns the fixed post-dispatch conflict instead of trimming whitespace or
weakening Markdown meaning.

An optional caller-generated `idempotency_key` deduplicates the explicit,
domain-separated version-1 canonical create fingerprint for the process
lifetime. The fingerprint uses the expected canonical stored-body
representation, not the separate wire form sent by POST. A supported raw plain
line and its exact already-canonical form therefore join the same cohort;
meaningful near-misses remain distinct. Identical sequential or concurrent
calls share one supervised in-flight attempt without holding the registry mutex
across network waits, and verified successes are returned from the finite cache
without I/O. Key reuse with different parameters conflicts before a write.
Safe pre-POST failures and
definitive 4xx/validation/authentication rejections free the entry for retry.
After possible acceptance, timeout, cancellation, transport/server failure,
oversized or malformed response, identity mismatch, verifier exhaustion, task
panic, or abort becomes the same fixed indeterminate conflict directing the
caller to reread/search before retry. This applies on the first keyed or
unkeyed call; keyed indeterminate entries remain terminal so retry cannot issue
a second POST, and an identical keyed retry receives the same fixed reread
guidance without I/O. Only reuse with a different fingerprint receives the
generic key-conflict guidance. Cancelled leaders and waiters cannot abandon or
duplicate the supervised cohort. The registry has a fixed capacity and fails
closed when full. Read-only access is rejected before even a cached success is
inspected.

### View discovery workflows

The `view_list` and `view_object_list` production tools provide one bounded
page at a time.
They resolve space and view names through `anytype-api`, so ambiguous view
names return bounded candidate IDs instead of selecting an arbitrary match.
`view_object_list` validates the resolver-returned view ID and sets it on the
fluent request builder before listing. Unsafe upstream identifiers fail with a
fixed secret-safe error before an object-list request. Successful calls return
stable object summaries, canonical resource URIs, and only explicitly
requested bounded property projections. Document bodies and snippets are never
included. Continuation cursors bind the space, list, view, normalized
projection, and limit, and are issued only after the upstream offset, limit,
and returned item count have been checked.

### Object discovery and reads

The `object_search` and `object_get` production tools implement the bounded
Phase 1 read path.

- `object_search` resolves an optional space and space-local type references,
  executes exactly one upstream page, and validates returned offset, limit,
  item count, and continuation metadata before issuing a cursor. Global search
  type values are treated as keys because a name or id cannot be resolved
  without a space. Results contain stable summaries plus only the explicitly
  requested property keys; document bodies, snippets, and implicit full
  property sets are never returned.
  Archived objects are omitted from this core discovery workflow while the
  cursor still advances by the checked upstream page window.
- MCP filters use one shared closed tagged model, currently consumed by
  `object_search`, for text, number, select,
  multi-select, date, checkbox, file, URL, email, phone, object-reference,
  empty, and nonempty conditions. Each supported format and condition converts
  directly to the corresponding `anytype-api` filter without client-side
  post-pagination emulation. Filter count, value count, nesting depth, scalar
  lengths, arrays, and numeric magnitude are bounded. Set operands advertise
  1..100 values, and the recursive expression schema requires at least one
  nonempty condition or child array while retaining omission defaults.
  Select references are 1..512 Unicode scalars, preserve whitespace, and reject
  commas because the upstream request encoding uses comma delimiters. Boolean and numeric
  filters are passed through unchanged; they remain subject to the
  known upstream [anytype-heart#2879](https://github.com/anyproto/anytype-heart/issues/2879)
  limitation instead of being silently rewritten with different semantics.
  File and object filter operands are validated as safe bounded identifiers
  before any upstream request. Cursor identity sorts and deduplicates
  commutative condition groups, nested groups, and set-valued operands while
  the upstream request retains the caller's original order and values; the raw
  request must still fit the existing 65,536-byte normalized-query ceiling.
- `object_get` resolves the space but requires a stable object id. It returns
  all properties only when the bounded set fits, or exactly an explicit
  projection. An optional body request is indexed in Unicode characters,
  defaults to 20,000 characters, caps at 100,000, reports continuation and
  total character counts, and hashes the complete current body even when only
  a chunk is returned. The unreturned body remainder never enters the MCP
  result.

All omittable read-input fields distinguish omission from explicit JSON
`null`. Omission selects the documented default; `null` is malformed and can
never broaden a scoped search to global search or a selected projection to all
properties. Space-scoped type resolver results are revalidated as bounded,
nonempty type keys before they enter a cursor binding or upstream search.

### Document resources

The transport-neutral resource handler advertises exactly one RFC 6570
template:

```text
anytype://spaces/{space_id}/objects/{object_id}
```

`resources/list` deliberately returns no object instances; use the paginated
`object_search` workflow for discovery. `resources/read` accepts only the
canonical scheme, authority, and path shape, performs no percent-decoding or
URI normalization, verifies the returned object and space identity, and
returns one complete `text/markdown` content item. Complete bodies of at most
100,000 Unicode characters are returned without truncation. Larger bodies
produce a stable `bounded_result` error directing the caller to `object_get`
body chunking.

Each read uses the configured document-response byte ceiling under the shared
concurrency, timeout, cancellation, and shutdown controls. Its typed resource
descriptor carries byte size, user/assistant audience, priority, and a strict
RFC 3339 `lastModified` annotation when Anytype supplies one. Properties,
snippets, and document content are never duplicated into descriptor metadata.
The production server routes these resource methods through the same shared
runtime and advertises their static capability alongside the tool catalog.

## Source layout

- `src/config.rs` — validated environment and operational limits.
- `src/logging.rs` — stderr-only tracing setup.
- `src/runtime.rs` — authenticated client, controls, and stdio lifecycle.
- `src/main.rs` — non-interactive startup and binary exit behavior.
- `src/lib.rs` — shared crate surface for the binary and tests.
- `src/domain.rs` — bounded values, object summaries, and resource URIs.
- `src/discovery.rs` — typed status and schema-discovery contracts and
  transport-neutral handlers.
- `src/schema.rs` — strict input/output schema generation.
- `src/protocol.rs` — tool contracts and annotation profiles.
- `src/resources.rs` — exact document template, empty instance listing, and
  bounded resource reads.
- `src/result.rs` — structured results with compact JSON text fallbacks.
- `src/error.rs` — stable, redacted tool execution errors.
- `src/filters.rs` — shared bounded filter DTOs and exact `anytype-api`
  conversion.
- `src/handler_support.rs` — controlled handler execution and checked page
  continuation helpers.
- `src/object_output.rs` — validated summaries and bounded property projection.
- `src/object_read.rs` — typed one-page object search and chunked object-get
  handlers.
- `src/object_archive.rs` — single-object soft archive contract and handler.
- `src/object_update.rs` — conflict-aware whole-field update contract and
  read-after-write verifier.
- `src/object_edit.rs` — conflict-safe ordered exact-match edit contract and
  verified single-PATCH handler.
- `src/object_create.rs` — verified create contract, closed write inputs, and
  bounded process-lifetime idempotency coordination.
- `src/validation.rs` — reusable collection, filter, and body chunk bounds.
- `src/pagination.rs` — bounded pagination inputs and result pages.
- `src/cursor.rs` — opaque process-lifetime, query-bound cursor registry.
- `src/view_handlers.rs` — bounded view discovery and selected-view object
  listing workflows.
- `src/server.rs` — server identity, capabilities, and stable protocol
  declaration.
- `src/stdio.rs` — bounded stable lifecycle and explicitly gated stateless
  2026-07-28 adapter.
- `src/server/headless_integration.rs` — ignored cleanup-safe production-router
  tests against an authenticated headless Anytype server.
- `tests/snapshots/` — reviewed deterministic compact/standard and
  read-write/read-only tool catalogs,
  including every schema and annotation.
- `tests/stdio_conformance.rs` — portable production-process protocol
  regression and preview/stable acceptance harness.
- `tests/support/` — shared bounded process driver, transport-neutral live
  scenario, and catalog-to-live-ownership audit.
- `tests/headless_stdio_e2e.rs` — ignored production stdio-to-real-Anytype
  workflow with independent `anytype-api` readback and cleanup.
- `tests/schema/mcp-2026-07-28.json` — official draft schema used only as a
  test oracle for actual preview requests and results.
- `STDIO_CONFORMANCE.md` — reproducible test, Inspector, and client discovery
  evidence with current compatibility limits.
- `TESTING.md` — executable test architecture, live ownership, evidence, and
  CI cadence contract.

## Testing

The unit suite locks every Phase 1 tool input schema, output schema, and exact
annotation in all four deterministic profile/read-only catalog snapshots. A
separate
fail-closed graph audit resolves only local `#/$defs` references with explicit
cycle tracking, validates every reachable composition branch, and rejects
unknown schema forms, strings without `maxLength`, arrays without `maxItems`,
or object schemas that permit unknown map keys. Security-focused tests also
cover cursor tamper/expiry/capacity, exact Unicode character and response-byte
boundaries, zero-write mutation conflicts, complete Anytype error
classification, redaction across protocol/error/diagnostic surfaces, and
read-only defense in depth.

Catalog changes are never accepted through an environment variable. Follow
the explicit regeneration and review procedure in
[`tests/snapshots/README.md`](tests/snapshots/README.md), including its pinned
`o200k_base` token-count audit. The complete serialized default compact
`tools/list` result is 9,669 tokens, strictly below 10,000 tokens (5% of the
internal 200,000-token compatibility-policy floor), with 331 tokens of
headroom. Its 2% material-growth boundary is 9,863 tokens, retaining 137 tokens
of headroom. Compact read-only is 8,380 tokens. Exact reviewed baselines also
measure explicit standard (22,909) and standard read-only (15,654), plus
schema-valid representative search/get results; any
count drift fails, and growth of at least 2% requires a recorded material-growth
rationale. Then run:

```sh
cargo test -p any-mcp
```

The `.github/workflows/any-mcp.yml` matrix runs the library schema, catalog,
budget, and unit tests plus the real-process stdio suites on Linux, macOS, and
Windows. The process harness uses only portable Rust process, TCP, path,
environment, thread, and channel APIs; it does not depend on Unix signals,
`/tmp`, executable suffixes, or shell scripts.

The test harness treats transport and upstream backend as independent axes.
Ordinary tests use the in-process router or the real stdio binary with a
scripted HTTP fixture for deterministic protocol and handler feedback. The
ignored live baseline runs the same reusable standard-profile scenarios
through the in-process production router and the spawned production stdio
binary against real headless Anytype. Together those scenarios execute every
advertised tool and resource operation, verify mutations independently through
`anytype-api`, and prove the complete MCP-wire-to-Anytype path. Compact,
read-only, and preview configurations use focused real-headless risk sentinels
rather than a Cartesian matrix. A typed catalog audit maps every advertised
standard operation to exactly one executable scenario and fails on missing,
duplicate, unknown, or non-executable owners. Pure schema, catalog, framing,
and validation tests remain the only no-backend cases; production has no
test-mode backend selector. See [`TESTING.md`](TESTING.md) for the maintained
architecture and evidence contract.

This is an OS-family portability gate, not a claim that CI exercises every CPU
architecture. The workspace targets Linux x86_64/aarch64, macOS aarch64, and
Windows x86_64/aarch64; the current dist configuration produces macOS aarch64,
Linux x86_64/aarch64, and Windows x86_64 artifacts. No external `any-mcp`
release is published by this documentation change.

## Headless integration tests

The ignored live suite uses `with_test_context`, checks authenticated HTTP and
gRPC before work, and runs serially so mutation verification does not compete
with itself for the server's rate limit. Every created object, type, and
property is registered immediately for cleanup. It requires a running headless
server, a test space selected by `.test-env`, and `anyr auth status` reporting
both HTTP and gRPC pings as OK. Run the direct-router and spawned-stdio targets
explicitly from the repository root:

```sh
source .test-env
cargo test -p any-mcp --lib headless_ -- --ignored --test-threads=1
cargo test -p any-mcp --test headless_stdio_e2e -- --ignored --test-threads=1
```

The selectable `headless_direct_standard_*` and
`headless_stdio_standard_*` cases cover discovery, document/resource access,
views, mutations, and archive through both entry paths. They execute all 14
standard tools and `resources/list`, `resources/templates/list`, and
`resources/read`, including bounded cursor terminality and binding,
ambiguity, explicit view selection, idempotent create, independent
read-after-write visibility, stale/count edit conflicts, and active/archive
evidence. Existing focused live regressions remain alongside this acceptance
baseline. The direct command selects exactly 13 intended `headless_` cases;
the spawned target contains exactly 8 ignored live cases.

The compact and read-only cases prove representative real reads and catalog
filtering; direct read-only also proves defense-in-depth mutation rejection.
The preview case uses stateless discovery and drives representative read and
mutation behavior through the real stdio process. Failure records contain the
scenario and generated fixture IDs, protocol metadata, bounded
request/outcome-category counts, structural stderr byte/line/category metrics,
and cleanup outcome—never raw diagnostic lines, unknown fields, arguments,
bodies, edit fragments, upstream errors, or credentials.
Direct cases additionally report `anytype-api` HTTP metric deltas; the spawned
production child intentionally has no test-only metrics interface.

When `.test-env` selects an explicit-path file/SQLite keystore, the stdio
fixture content-verifies a test-owned snapshot of the main database and WAL,
preserves Windows drive and ordinary colon-bearing paths plus cipher/suffix
options, and removes the temporary main/WAL/SHM files. The child specification
contains exactly one path pointing only to that snapshot. Plain defaults and
missing, empty, or duplicate file/SQLite paths are rejected because they cannot
be isolated safely; keep the source quiescent while the snapshot is created.

The dedicated `headless-e2e` CI job is intentionally Linux/self-hosted rather
than part of the portable hosted-runner matrix. Runners labeled
`anytype-headless` must provide a running isolated Anytype server and set the
repository variable `ANY_MCP_HEADLESS_ENV_FILE` to a readable, protected
environment file with the same endpoint, keystore, and test-space settings as
`.test-env`. It should also set `ANY_MCP_HEADLESS_REDACTED_LOG_FILE` to a
runner-produced server log with credentials and content removed; the job keeps
that protected artifact for seven days on failure. Protect the
`anytype-headless` environment so untrusted pull-request code cannot reach the
self-hosted runner or credentials. The job runs serially on every matching
MCP/API pull request and main/tag update; branch protection and release
automation should require its latest green result. Fork pull requests are
excluded before the protected runner is selected. A separate unconditional
scheduled job invokes an operator-owned absolute reset script, provisions a
clean isolated server, and then runs the same two explicit targets; this keeps
path filters from hiding backend drift.

`space_list` continuation uses two disposable spaces created and immediately
registered through the test-only `anytype-api` fixture lifecycle. Their complete
REST visibility proves that `limit=1` must continue before the production MCP
router walks the hard-bounded cursor chain to terminality, rejects a cursor
rebound to a different limit, detects repeated items/cursors, and observes both
exact fixture IDs; teardown irreversibly deletes only those self-created IDs
and requires bounded absence evidence. `template_list` uses a private custom
type and two cleanup-owned templates from the narrow test helper owned by
`anytype-api`; it walks `limit=1` cursors until both exact fixture IDs are seen
and the terminal page is proven, rejecting query changes, cursor or item loops,
and traversal beyond a fixed bound.

Collection coverage creates and immediately registers a custom
collection-layout type through the same narrow helper, then uses a private
type-bound create-provenance path to atomically claim the exact collection and
its sole cleanup dispatch, then clone its fully
cross-checked default dataview into a cleanup-owned second view. Ordinary
object cleanup registration cannot grant this mutation authority.
`view_list(limit=1)` walks both exact ordinary-API IDs and names to a terminal
page under a hard bound, rejects the same cursor with either a changed limit or
list ID, and detects repeated items or cursors. The added view ID is also passed
explicitly through `view_object_list`, preserving the selected-view path. The
required heart RPCs stay inside `anytype-api`, so `any-mcp` retains its
`anytype-api`-only dependency boundary.

## Build

```sh
cargo build -p any-mcp
```

## Protocol channel

Stdout is reserved exclusively for MCP protocol frames. Redacted diagnostics
are emitted to stderr; credentials and full upstream response bodies are never
included in runtime error formatting or startup diagnostics.

The production-process regression harness checks the complete advertised
catalog, document resources, structured success and error results,
cancellation, malformed and unknown requests, clean EOF, and stdout/stderr
purity across profile and read-only modes. It also verifies preview stateless
discovery, stable lifecycle negotiation, and exact malformed-frame recovery
before and after stable initialization. See
[stdio protocol verification](STDIO_CONFORMANCE.md)
for commands, external-tool evidence, and the precise limits of the current
compatibility claim.

## License

Apache License, Version 2.0
