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

Build the current workspace prerelease and confirm that the existing `anyr`
credentials can reach Anytype:

```sh
cargo build -p any-mcp
anyr auth status --pretty
realpath target/debug/any-mcp
```

The last command prints the absolute path to the binary built in this checkout.
Replace `/absolute/path/to/anytype/target/debug/any-mcp` in both examples below
with that platform-specific absolute path; the workspace build does not install
`any-mcp` on `PATH`. On Windows, resolve `target\debug\any-mcp.exe` and prefer
the JSON/TOML-safe forward-slash form, for example
`C:/repo/target/debug/any-mcp.exe`. If native backslashes are retained, double
every backslash in either quoted format, for example
`C:\\repo\\target\\debug\\any-mcp.exe`; a single backslash can be parsed as an
escape.

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
      "command": "/absolute/path/to/anytype/target/debug/any-mcp",
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
command = "/absolute/path/to/anytype/target/debug/any-mcp"
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
- `ANY_MCP_TOOLSETS` is absent by default. A present selector is an exact,
  comma-separated list of at most 16 linked optional registry names, sorted
  canonically at startup. Malformed, duplicate, unknown, and unfinished names
  fail closed without being echoed. The linked production names are
  `body-blocks`, `chats`, `files`, `members`, `schema`, and `views-write`;
  acceptance-blocked
  `discussions` remains rejected;
- `ANY_MCP_JSON_RESPONSE_BYTES` defaults to 8 MiB and has a maximum of 64 MiB;
  and
- `ANY_MCP_DOCUMENT_RESPONSE_BYTES` defaults to 64 MiB, has a maximum of 64
  MiB, and must be at least the ordinary JSON budget.

Protocol mode, catalog profile, optional toolsets, and read-only access are
independent startup selectors. Each uses an exact fail-closed grammar and
cannot be changed by MCP input after startup. Before any nonempty optional
selection can authenticate or perform I/O, its effective
`ANYTYPE_RATE_LIMIT_MAX_RETRIES` value must be in `1..=5`; empty-selection
Phase 1 startup retains the existing `anytype-api` behavior.

The offline production integration matrix composes all six linked registries
together in compact and standard, read-write and read-only configurations. It
locks their exact catalogs and canonical status, stable/preview contract
identity, gRPC requirement union, disabled stale-call rejection, and aggregate
`o200k_base` catalog cost. The same matrix proves that an absent selector leaves
all four reviewed Phase 1 catalog snapshots and `server_status` unchanged. A
leave-one-registry-out sweep also proves that every omitted registry remains
unreachable while the other five are active.
The production ownership audit independently binds each of the 29 domain tools,
`optional_toolset_status`, and the file byte-resource family to one fast and one
real-headless executable scenario. It rejects missing, duplicate, unknown, or
untyped catalog and scenario entries. Compile-bound runner tables execute all
seven fast workflow groups and all six spawned real-headless registry
workflows. The files workflow verifies real upload, metadata, bounded reads,
resources, independent download, diagnostics, and cleanup. A separate
all-selected sentinel composes stable read-write and preview read-only children
and performs one real-backend read per registry.

The default-off `body-blocks` registry exposes `body_block_list` plus five
write workflows for one-block create, update, delete, move, and bounded rich
page creation. It uses the typed `anytype-api` body model only. Reads return
exact block identity, document order, and a canonical snapshot hash; mutations
require that hash and verify their result. Rich page construction is a finite
flat plan and reports complete, partial, or indeterminate evidence without
claiming transactionality. Generated tables conservatively count their root,
two layout regions, rows, columns, and logical `rows × columns` capacity
against the 256-block plan ceiling. Success separately requires Heart's exact
sparse subtree: no cells without a header, or grey-background empty paragraph
cells under the first header row only.
Read-only mode retains only `body_block_list`. Acceptance executes that read
through stable and preview stdio and compares the complete normalized result.
Domain error parity likewise preserves `isError`, ordered content, exact
structured code/message/candidates, and its canonical JSON text duplicate.
The R5 handlers do not request URL metadata, expose protobufs, or use a mock
server. The closed
`{ "kind": "bookmark", "url": string }` constructor is available to
`body_block_create` and `rich_page_create`. It permits one ordinary
`BlockCreate` and requires `BookmarkState::Empty` with no target-object
readback. It does not add `BlockBookmarkFetch`, metadata, redirect, import, or
fetch controls.

### Object-tag exclusion policy status

`any-mcp` does not currently enforce an object tag such as `never-access` as
an access policy. An investigation found that ordinary object, search, and
saved-view list responses already include assigned select and multi-select
tags, so those pages do not inherently require one follow-up request per
object. Canonical manual-collection membership returns IDs only and needs a
separate policy-aware query; filtering its current pages afterward would leak
protected membership through pagination. Global search, linked or embedded
objects, chat/discussion inheritance, schema mutation, and write races also
need resolved contracts before such a guard can be advertised.

Any future tag guard would constrain only this MCP process. Other Anytype
clients can change object tags, and the current API cannot atomically bind a
tag preflight to a later mutation. Continue to use Anytype space permissions
as the authorization boundary.

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

The shipped server gives each Tokio worker an explicit 8 MiB stack. This
finite cross-platform setting keeps the aggregate typed dispatcher reliable in
debug and production builds. It raises reserved virtual address space per
worker; physical commitment remains operating-system dependent.

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

The optional registry foundation composes selected typed tools, resources, and
resource templates after the Phase 1 profile without changing any Phase 1
contract. It sorts every inventory deterministically, rejects collisions and
incomplete ownership, unions transport requirements, and applies read-only
mutation removal independently of compact or standard. A nonempty selection
also adds the immutable read-only `optional_toolset_status` tool; it reports
only canonical configured and active registry names and performs no
environment, credential, resolver, or upstream access. Disabled optional tool
and resource names return method-not-found before argument decoding or I/O.
Stable and experimental protocol modes use the same composed catalog.

Only `compact` and `standard` application profiles exist. The optional
`body-blocks`, `chats`, `members`, `files`, `schema`, and `views-write`
registries are linked and can be selected explicitly or combined in one
comma-separated `ANY_MCP_TOOLSETS` value; they are absent by default.
Acceptance-blocked `discussions`, plus proposed `admin`, are not selectable in
this release. Their names become valid selectors only when a complete,
independently reviewed production registry is linked.

The `body-blocks` R5 registry provides six workflow tools for stable typed
body pages, verified single-block create/update/delete/move, and finite rich
page construction; read-only mode retains only body listing. All schemas use
closed nonrecursive variants, fail opaque and read-restricted content closed,
permit inert bookmark creation without network fetching, and accept YouTube
creation only as an exact 11-character video ID normalized to inert canonical
document data.
Rich
construction is explicitly non-atomic and returns bounded applied, failed, and
not-attempted evidence without compensation or automatic write resumption.
Single-block create verification derives the exact parent and sibling index
from the pre-write snapshot, rejects collateral identity, order, value, and
structure drift, and permits restriction refresh only on the insertion
parent. When that parent is an opaque page root, its opaque kind and duplicated
child count remain exact while the protobuf-derived approximate byte count may
refresh with the child list; all non-parent opaque summaries remain exact.
Generated table descendants must form one canonical sparse Heart subtree.
Move verification applies the same derived-summary rule only to its old and
new structural parents, while requiring exact post-move DFS order and leaving
every other opaque summary and block value unchanged.
Delete verification applies it only to the removed subtree's direct parent,
proves the exact filtered DFS order and sibling shifts, and keeps every
surviving non-parent value exact.
For a value-only update, only the opaque page root's protobuf-derived byte
summary may refresh; its kind, duplicated unchanged child count, structure,
restrictions, and presentation remain exact, as do every non-root block except
the one explicitly updated field.

The finite `anytype-api` body lifecycle caps decoded Show at
4,194,304 bytes and every non-Show body gRPC response—including foreground and
fallback ObjectClose—at 65,536 bytes, owns cancellation-resilient bounded
cleanup, shares one absolute deadline, and exposes the exact first-write-poll
boundary. MCP body editors preserve the configured verification timeout and
delays while capping inherited attempts at three; configured one- or
two-attempt policies remain narrower. Live primitive create, update, delete,
and move counters therefore accept one through three semantic-verification
Show/confirmed-close rounds while still requiring one write and zero fallback
or limit-rejection counters. Close overrun is cleanup failure; mutation
overrun after polling is indeterminate. The design's paired maximum
request-plus-result contexts remain below 200,000 `o200k_base` tokens.
Ordinary gRPC acceptance uses only a cleanup-owned real Anytype server across
direct, stable-stdio, and preview-stdio paths. The removed semantic mock/custom
server is prohibited; latency, connection, malformed/status, and retry faults
remain P4 behind the separately reviewed fault-injection design.
Raw stable/preview body-frame parity permits only the preview protocol's
required `resultType: complete` field on result envelopes. JSON-RPC error
envelopes receive no normalization: response IDs, versions, error
code/message/data, and shapes remain exact. Duplicated text content, structured
payloads, cursors, snapshot hashes, opaque summaries, and domain IDs also
remain exact.
The separate cross-fixture semantic comparison normalizes generated IDs,
snapshot/cursor tokens, and only `approx_bytes` inside `kind: unsupported`
content because that summary is the wire block's `encoded_len` and therefore
includes generated-ID lengths. Opaque kind and child count, typed content,
presentation, restrictions, ordering, counts, status, errors, and idempotency
stay exact.
Independently created pagination fixtures use the same title in every
transport; typed text is never normalized.

R5 widens only the closed create union with the inert bookmark shape documented
above. Direct, stable-stdio, and preview-stdio acceptance creates it against a
real server and independently verifies the exact URL, empty state, and absent
target object.

R4 also fixes emoji and callout payloads at the current 64-byte API ceiling,
requires both UTF-16 mark endpoints to be `u32` scalar boundaries, and gives
relation keys one lowercase ASCII grammar with 0..64 exact-unique link relation
entries on create and update. A replay that recovers a retained page candidate
returns and retains an index-zero partial receipt and never resumes body writes.

The production `schema` registry includes `space_create` and `space_update`.
Both workflows use `anytype-api` only and return just a validated space ID,
name, and optional description. Create supports a bounded process-local
`idempotency_key`; update resolves one exact space, requires at least one
nonempty replacement field, preserves omissions, sends one PATCH, and does not
support description clearing. Post-dispatch timeout, cancellation, transport,
5xx, malformed response, or failed semantic readback is reported as an
indeterminate conflict.

Direct-router and spawned preview-stdio happy paths are exercised against
cleanup-registered disposable spaces on an authenticated real server. Tests
that must induce latency, connection faults, malformed responses, or exact
worst-case retries remain deferred to the external P4 fault-injection design.

The approved `schema` design includes bounded complete replacement or clearing
of non-featured type recommendations after the API gained a cache-independent
featured/recommended classification. Omission preserves the current set, an
explicit empty list clears it, and at most 20 unique-key property
specifications replace it while exact featured evidence remains unchanged. The
API classification operation now has finite per-RPC deadlines and
cancellation-resilient owned `ObjectClose` cleanup. The production `schema`
registry includes cache-independent `type_get`, verified and
idempotent `type_create`, and one-write `type_update` with semantic no-ops,
complete ordered preserve/replace/clear behavior, exact featured-vector
protection, and conservative post-dispatch uncertainty. Selecting `schema`
requires both authenticated HTTP and gRPC through the shared `anytype-api`
client.

The complete production registry keeps aggregate dispatch and every schema
mutation success path behind heap-owned future boundaries so standard worker
stacks remain bounded. Its spawned stable-stdio acceptance runs all nine tools
in one cleanup-owned workflow and independently re-reads created and updated
tags through the exact property-scoped `anytype-api` path, including tag ID,
name, color, space, and property ownership.

Direct-router and preview-stdio parity runs those type workflows only against
cleanup-registered disposable real-server types using the production
classifier. The acceptance matrix measures HTTP and Show/Close work, covers
the separate 24/144 no-op and 45/265 write HTTP ceilings, exact successful
Show/Close/fallback counters, metadata-plus-recommendation replacement,
read-only and authentication parity, ambiguity and scope/layout rejection,
cancellation cleanup, 20-item create/update boundaries with zero-I/O 21-item
rejection, and catalog, adversarial-input, and maximum-result token snapshots.
Synthetic transport failures remain deferred to the external P4
fault-injection work.

The production `schema` registry includes `tag_create` and `tag_update`
through `anytype-api` only. Both workflows resolve one space and
1..256-scalar property reference, prove space ownership and `select` or
`multi_select` format with one cache-independent terminal property page, and
return the closed `{ "tag": TagSummary }` envelope containing only the tag ID,
key, name, and color. Create defaults an omitted color to `grey`, supports finite
process-local idempotency, sends one POST, and verifies the scoped tag.
Update requires an exact `tag_id` plus at least one non-null name, key, or
color, preflights that scoped tag, sends one PATCH, and verifies every supplied
field. Preflight and readback use a terminal property-owned tag page because
the upstream exact-tag endpoint accepts globally valid cross-property IDs.
Both mutations disable automatic property-cache refresh and invalidate the
affected space cache, so a primed cache cannot expand their work.

Direct-router and preview-stdio acceptance uses cleanup-owned select
properties in disposable real-server spaces. Stable-ID calls prove three
logical and physical HTTP operations for create and four for update, while
name and key resolution remains within the reviewed 34/199 and 35/205
ceilings. The maximum complete `CallToolResult` is 5,320 bytes and 3,381
`o200k_base` tokens. Wrong-format calls fail before a tag write. The current
test environment provides only an owner credential, so genuine HTTP 403
permission coverage remains an external acceptance blocker. Deterministic latency,
connection-fault, and retry-maximum cases remain deferred to the P4
fault-injection design.

The production `views-write` registry implements
`collection_member_list`, `collection_member_add`, and
`collection_member_remove` through `anytype-api`'s canonical direct-membership
operations. The list input is exactly `space`, `collection_id`, and optional non-null
`limit`/`cursor`; it deliberately accepts no view, filter, sort, layout, query,
or Kanban field. The default limit is 20 and the reviewed maximum is 61.
Results contain only canonical-order `{ "object_id": ... }` summaries, while
opaque process-local cursors bind the resolved space ID, exact collection,
limit, operation, registry, preceding total, next offset, and overlap boundary.
Saved-view presentation therefore cannot hide a direct member from this tool.

Both mutations accept exactly `space`, `collection_id`, and `object_id`.
Collection and object values are stable IDs, never names, queries, views, or
filters. Add returns a fixed `membership: "present"` result; remove returns
`membership: "absent"`. A complete independent preflight observation returns
success with zero writes when that state already holds. Otherwise add sends
one non-replayed, non-redirected POST and remove sends one logical replay-safe DELETE, then a
ten-attempt, three-second independent observer must prove the desired state.
No response message is treated as state evidence. Cancellation or any other
uncertainty after dispatch returns fixed conflict guidance to reread before
retrying, and the handler never redispatches.

For add, a completed POST preserves its exact status through `anytype-api`.
Only 400, 401, 403, 404, 409, and 422 are definitive rejections. Redirects,
408, 410, 425, 429, every other 4xx, every 5xx, transport failures, and
malformed or incomplete success responses remain indeterminate.

The registry is default-off and contributes exactly the three membership tools
in read-write mode and only `collection_member_list` in read-only mode.
Selecting it requires authenticated HTTP and gRPC through `anytype-api`.
Authenticated disposable acceptance defines one shared scenario for the actual
`AnyMcpServer` router and separately spawned stable and preview stdio children.
All three drivers use the same reviewed handlers as the immutable production
descriptor. Deterministic cancellation and concurrency seams remain confined
to a feature-gated acceptance registry; the spawned acceptance binary is not
the shipped `any-mcp` binary and is not built by default. The child appends
payload-free counter snapshots to a private metrics file. The scenario seeds
only A, leaves B absent as the mutation target, and keeps C absent as a control.
It applies list, add, and remove to a Set/query object, rejects limit and
collection cursor rebinding, covers add/no-op/remove/no-op cycles, both sides of
both dispatch-marker cancellation boundaries, both read-only mutation gates,
exact result identity, object survival, and a saved view that hides B.
Stable-ID success performs exactly one logical and physical HTTP GET, one
canonical membership round, one subscribe, and one confirmed foreground close
with no fallback. Cursor binding is checked before the membership primitive, so
a mismatched collection or limit performs zero HTTP or membership I/O. Direct,
stable-stdio, and preview-stdio scenarios assert cursor mismatch, strict query
rejection, read-only behavior, identical results, and exact logical/physical
HTTP, observer, query, subscribe, foreground-close, fallback, and write deltas.
Canonical pagination must contain A and B exactly once before remove, then only
A afterward; independent observers continuously keep C absent. A barrier at
the actual handler's post-preflight boundary sends two concurrent B additions
through each router, proves bounded aggregate work and a safe verified outcome,
and checks that neither A nor C changes. This is a concurrency seam, not a
latency or network-fault server. Stable and preview protocol envelopes are both
included in every profile/access token snapshot. A separate offline
direct/stable/preview process test feeds HTTP 403 into the production rejection
classifier twice and proves authentication mapping, transport parity, no
redispatch, and zero HTTP or mutation work. This pure classifier test is not
permission acceptance. Genuine direct-router and spawned-stdio HTTP 403
coverage remains blocked until a disposable non-owner collection with owner
cleanup is available; the persistent read-only fixture is never mutated and
invalid credentials are not used as a permission substitute.

A second shared disposable scenario covers representative layouts without
adding a Kanban-specific MCP surface. It verifies Basic and Collection type
layouts, Grid and Kanban saved views, filtered view pagination, and ordinary
Select-property column movement through `object_update`. The same direct,
shipped stable-stdio, and shipped preview-stdio workflow removes and re-adds a
card through the generic collection-member tools, walks canonical membership
with `limit: 1`, and independently confirms that saved-view visibility never
changes direct membership. The shipped server's explicit finite worker stack
keeps filtered layout inventory reliable. Each shipped child uses the
disposable environment and a registered stop-and-wait action that completes
before fixture or space cleanup. All fixtures are cleanup-owned; `test12` is
not mutated, and deterministic fault cases remain deferred to the P4
fault-injection design.

An earlier live mutation run was blocked before the scenario callback when
disposable-space creation applied but its response did not complete; both
ledger-named spaces were removed and absence proved. A later run entered the
shared scenario and exposed debug-build worker stack overflows in the add and
list handlers; both operation/executor boundaries are now boxed. The next run
progressed through the normal stable add calls, then the stable
`CancelAddBeforeMark` child timed out waiting for its add response with empty
stderr. Cleanup acknowledged deletion and independently proved absence, and
both transports remained healthy. The harness now gives injected cancellation
a handler-local token so it cannot cancel rmcp's response channel. The
preview dispatcher and optional-registry aggregate also box the reviewed tool
future, keeping debug-build workers within their default stack. The final
authorized direct/stable/preview run completed every A/B/C, cancellation,
concurrency, pagination, and cleanup assertion; HTTP and gRPC remained healthy,
the disposable prefix was empty afterward, and no child, metrics file, or
current run ledger remained.
Latency, dropped connections, malformed bodies, and injected 5xx behavior are
explicitly deferred to the P4 fault-injection design.

The default-off production `chats` registry implements `chat_list`,
`chat_message_list`, `chat_message_get`, `chat_message_search`,
`chat_message_add`, and `chat_message_delete`. Read-only mode retains exactly
the four reads. The registry contributes no resources or templates, adds the
common optional status once, and requires authenticated HTTP but not gRPC. It
uses REST through `anytype-api` only. Chat lists default
to 10 and cap at 20; message
lists and searches default to 8 and cap at 12. Older-history cursors keep one
validated opaque server anchor and a one-based page number only in the bounded
process-local cursor registry, never in MCP output or diagnostics, and stop at
64 pages. Results minimize names, text, authors, timestamps, reply identity,
formatting presence, and attachment counts; they never expose marks,
attachment details, reactions, read state, order/state IDs, or structured
blocks. Exact reads reject text beyond 8,192 Unicode scalar values, while list
and search text is truncated only at scalar boundaries with exact counts and
flags. Direct and preview-stdio acceptance uses one cleanup-owned disposable
real chat and registered messages. Latency, dropped connections, malformed
responses, and forced 5xx cases remain behind the P4 fault-injection design.
Chat discovery also requires every returned object to have the exact `chat`
layout and resolved space identity; any other upstream shape fails closed.
The reviewed `o200k_base` snapshot locks compact and standard base/selected
catalog hashes, read-write/read-only inventories, each tool's token cost, and
adversarial maximum result bytes and dual-encoding tokens. Typed fixtures also
lock maximum item counts and exact at-ceiling/plus-one encodings across
four-byte Unicode, combining marks, escape-heavy strings, and prompt-injection
text. The real-server acceptance runs every read through direct dispatch and
one persistent preview-stdio session; both paths continue and restart chat,
history, and search cursors and reject cursor/limit reuse before HTTP. Each
ordinary stable-ID read performs exactly one logical HTTP operation with at
most six physical attempts. Exact injected retry sequences remain deferred
with the other transport faults rather than being emulated by a semantic
server.

The approved [attached discussions design](designs/attached-discussions-toolset.md)
keeps page discussions separate from ordinary chats. Its production-unlinked
candidate contains only `object_discussion_get`, which returns normal `absent`
state or the stable `discussionId` attached to one exact Basic or Note parent.
It does not read comments or expose attachment as an MCP mutation. The
candidate requires authenticated HTTP and gRPC through `anytype-api`, performs
no write dispatches, and has the same contract in read-only mode. Its returned
ID can feed separately reviewed bounded chat-message tools unchanged without
altering their schemas, cursors, or snapshots. The shipped server rejects both
the `discussions` selector and stale `object_discussion_get` calls.

The cleanup-owned current-server acceptance scenario passes, but production
acceptance remains blocked on the mandatory configured viewer fixture. Its
existing `DiscussionObject` fails closed because its unique key is not the
Heart-defined `discussion-{parent_id}` value. Heart used that exact binding
from the introduction of discussions, so this implementation does not accept
the distinct legacy derived-chat key or weaken parent binding. The fixture
must be corrected or migrated upstream and the ignored viewer-positive test
must pass before this registry is considered accepted for release.
The non-default `acceptance-harness` feature builds a test-owned discussions
binary only; it does not alter the shipped registry inventory. Cleanup-owned
acceptance drives that binary as separate stable and preview OS processes,
checks Basic and Note absence, Action rejection, wrong-space rejection, exact
attached identity, unchanged chat-message handoff, repeated stable output, and
exact HTTP and Show/Close work. Offline direct, stable, and preview coverage
locks strict inputs, unknown tools, read-only parity, framed pre-I/O
cancellation, deadline and authentication classification, redaction, result
encoding, and zero-work rejection paths without a semantic mock or fault
server.

`chat_message_add` accepts exact plain paragraph text from 1 through 8,192
Unicode scalar values, a required process-local idempotency key, and an
optional exact reply target. A new key may perform one reply preflight GET,
exactly one non-replayed POST, and one exact assigned-ID GET. Initial success
requires the requested text, paragraph/no-mark/no-attachment shape, and reply
identity. Identical concurrent calls share that leader result; reuse with
different resolved scope, chat, text, or reply conflicts before domain I/O.
After verified success, later replay never sends another POST and instead
returns one freshly validated exact GET, so independent changes to message
content or presentation are visible and do not defeat duplicate control.
Definitive POST rejection and uncertainty before Anytype returns a valid
assigned ID are terminal for that key during the process. After a valid ID is
returned, the process retains that candidate before verification. Initial
verification may therefore return an ordinary not-found,
authentication/permission, bounded-result, or upstream GET error; every later
identical retry performs only a fresh exact GET for the retained ID and never
another POST. Reply preflight validates exact scoped identity and timestamps
but does not apply the returned-detail text ceiling to the unreturned target.
Resolution, admission, detached leader work, and verification share one
absolute invocation deadline; a waiter observes the earlier of its own and the
leader's deadline. The fixed catalog/result snapshot keeps the actual tool at
or below its reviewed 2,000-token ceiling. Deterministic process tests drive
real stdio frames through this exact reviewed production registry. The slice
exposes no edit, attachment,
rich block, reaction, read-state, pin-state, streaming, or gRPC capability.

`chat_message_delete` accepts exact space, chat, and message identities, the
canonical 24-character UTC-millisecond `modified_at` returned by an exact
message read, and the literal `delete_message` confirmation. It performs one
exact preflight and compares the timestamp byte-for-byte before dispatching
exactly one non-replayed DELETE. The timestamp is advisory rather than an
atomic revision: equal-millisecond edits and a writer racing after preflight
can still evade it. A successful result additionally requires bounded exact
GET verification to observe authoritative absence. A lost, malformed,
cancelled, or timed-out DELETE response remains mutation-indeterminate even if
verification later observes absence; the handler never retries DELETE.
Accepted-but-unverified deletion is also indeterminate. The result contains
only resolved identities, `deleted: true`, and the accepted prior timestamp;
message content never enters results or diagnostics. Each verification read
is capped by both the remaining three-second/ten-attempt verification budget
and the common request deadline. A stable-ID invocation admits at most 12
logical operations and 67 physical attempts; maximum name resolution raises
those aggregate ceilings to 23 and 133 respectively, with exactly one physical
DELETE in either case. The complete production `chats` registry composes this
slice without broadening its contract.

The production `schema` registry includes `property_create` and
`property_update` through `anytype-api` only. Create accepts every closed
property format, restricts an optional 1..20 tag batch to select formats,
deduplicates retries with an optional process-local key, disables hidden cache
refresh work, verifies property metadata through direct reads, and consumes
exactly one terminal 20-item tag page. Update resolves and preflights one exact
property, returns semantic no-ops without a PATCH, otherwise sends one
non-replayed PATCH, preserves format and tags, and verifies the required name
plus optional key. Both workflows expose only bounded property/tag summaries.
Direct-router and preview-stdio acceptance covers primed and unprimed caches,
exact logical/physical counters, the 20/21 boundary, cancellation, auth,
idempotency, and cleanup-owned disposable real-server properties. Latency,
malformed-success, 5xx, and connection-fault injection remain deferred to the
external P4 design.

The complete registry exposes exactly nine domain tools in read-write mode and
only `type_get` in read-only mode; common `optional_toolset_status` is added
once. Its compact recursively key-sorted `o200k_base` snapshot records 7,856
domain tokens and an 8,112-token selected contribution, below the reviewed
9,500/10,000 ceilings. The same snapshot locks compact, standard, read-only,
mixed-registry, per-tool, and maximum representative-result measurements. A
spawned production-stdio disposable workflow exercises all nine tools and
independent API readback before exact cleanup.

The default-off `files` registry provides `file_metadata`, `file_read`, and
`file_upload` in read-write mode; read-only mode removes only `file_upload`.
`file_metadata` performs an
exact object-identity preflight and bounded `HEAD`; `file_read` returns at most
65,536 bytes with reconciled range, size, MIME, strong ETag, and modification
date evidence. Successful reads contain compact structured metadata once plus
exactly one native MCP payload: image, revision-supported audio, bounded UTF-8
text resource, or base64 blob resource. Every read also identifies a canonical
hash-bound
`anytype-file://bytes/{space_id}/{file_id}/{offset}/{length}/{sha256}` URI;
the matching internal resource reader re-fetches the exact range and rejects
identity, length, representation, or digest drift as not found. Text frames are
capped at 70,000 encoded bytes and all file results at 96 KiB. `file_upload`
accepts only canonical inline base64 of 1 through 65,536 decoded bytes and
never accepts a host path or URL. It sends one multipart POST under a
71,680-byte request ceiling, retains the candidate, and requires an exact
object preflight, metadata `HEAD`, and complete bounded hash readback. Same-key
retries never repeat the POST: verified results are reused and retained
candidates receive safe read-only reverification. Space names use bounded
1-MiB resolver pages; stable IDs avoid resolver I/O. The registry uses HTTP
only, lists no resource instances, and exposes the same single hash-bound
resource template in both access modes.

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
cursor integrity checks. Each optionally accepts one strict flat `and` group
of shared filters and forwards every leaf through the endpoint's server-side
query builder. Recursive groups and `or` are rejected. `property_list` also
rejects combining a filter with `type`, whose linked-property scope is applied
after one upstream window. Space, type, and property references use the bounded
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
- MCP filters use one shared closed tagged leaf model for text, number, select,
  multi-select, date, checkbox, file, URL, email, phone, object-reference,
  empty, and nonempty conditions. Each supported format and condition converts
  directly to the corresponding `anytype-api` filter without client-side
  post-pagination emulation. `object_search` accepts the recursive expression;
  `space_list`, `type_list`, `property_list`, `tag_list`, `template_list`, and
  `view_object_list` accept one nonempty flat `and` conjunction. `view_list`
  has no upstream filter builder and rejects a `filters` field. The optional
  members tools accept no raw filter; current chat and file toolsets remain
  unimplemented even though their future API builders can accept flat filters.
  Filter count, value count, nesting depth, scalar
  lengths, arrays, and numeric magnitude are bounded. Set operands advertise
  1..100 values, and the recursive expression schema requires at least one
  nonempty condition or child array while retaining omission defaults.
  Select references are 1..512 Unicode scalars, preserve whitespace, and reject
  commas because the upstream request encoding uses comma delimiters. Boolean
  and numeric filters are passed through unchanged. Tier-2 production-router
  conformance proves the configured backend returns the exact numeric and
  checkbox matches while continuation follows the checked upstream page;
  `any-mcp` never rewrites the filters or scans extra pages locally.
  The workspace [filter-status matrix](../FILTER_STATUS.md) distinguishes
  this verified production path from unsupported condition/value combinations
  and tracks closure of the historical upstream parsing report.
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

### Optional member discovery

Set `ANY_MCP_TOOLSETS=members` before startup to add `member_list`,
`member_get`, and the common `optional_toolset_status` tool. The registry uses
HTTP only and remains available in read-only mode. `member_list` resolves one
space and returns exactly one checked upstream page with the common default 20
and maximum 100 item limits; continuation cursors bind the resolved space and
requested limit. `member_get` performs one exact scoped read and rejects a
mismatched returned identity.

Member results contain only `id`, an optional explicit space-local `name`,
`role`, and `status`. They never expose network identity, global/fallback name,
or icon data. Upstream authorization remains authoritative, and disabled
member tool calls fail before argument decoding or upstream access.

Both tools apply the common request cancellation, timeout, retry, and redacted
diagnostic controls. A name resolver uses at most 11 logical HTTP operations;
one list or exact-get operation adds one. Pure zero-I/O tests cover strict
runtime decoding and pre-cancellation. Cleanup-owned real-server direct-router
and production-stdio tests cover bounded member pages, exact returned identity,
minimized output, read-only parity, and the erased dispatch/operation future
boundaries required to stay within the default worker stack. Malformed
responses, latency, 5xx, retry, and connection-fault cases remain deferred to
the P4 fault-injection server design; the member tests contain no custom HTTP
server.

### Optional files workflows

Set `ANY_MCP_TOOLSETS=files` before startup to add `file_metadata`,
`file_read`, `file_upload`, the hash-bound file-byte resource template, and the
common `optional_toolset_status` tool. The selector remains default-off and uses
HTTP only. Read-only mode keeps metadata, reads, and resource reads while
removing upload before argument decoding or upstream access.

One upload request carries only a bounded display name, optional MIME essence,
canonical base64 bytes, and a process-local idempotency key. It has no host
path, URL, delete, preload, rich-file, or filesystem-root surface. File reads
return at most 65,536 bytes as exactly one native image/audio/text/blob content
block plus bounded structured metadata; the returned URI can reread only that
exact hash-bound chunk.

Resolution, cohort admission and waiting, the single POST, and complete
verification share one absolute invocation deadline. A waiter never extends
the leader's deadline. Admission lock contention is deadline-bound, and an
expired admission cannot return cached success or retain an unsupervised
running entry. Upload cohorts are isolated per runtime and invalidated
by the client's non-secret HTTP credential generation, so replacing or
clearing credentials cannot replay a success cached for an earlier principal.

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
- `src/discussion_toolset.rs` — exact read-only attached-discussion discovery
  and its production-unlinked optional-registry descriptor.
- `src/schema.rs` — strict input/output schema generation.
- `src/schema_toolset.rs` — complete production schema descriptor and
  composition/token gates.
- `src/schema_space_toolset.rs`, `src/schema_type_toolset.rs`,
  `src/schema_property_toolset.rs`, and `src/schema_tag_toolset.rs` — bounded
  schema workflow contracts, handlers, and real-server acceptance.
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
  workflow with independent `anytype-api` readback, disposable lifecycle and
  panic sentinels, and cleanup.
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
`tools/list` result is 9,658 tokens, strictly below 10,000 tokens (5% of the
internal 200,000-token compatibility-policy floor), with 342 tokens of
headroom. Its 2% material-growth boundary is 9,852 tokens, retaining 148 tokens
of headroom. Compact read-only is 8,369 tokens. Exact reviewed baselines also
measure explicit standard (36,135) and standard read-only (28,880), plus
schema-valid representative search/get results; any
count drift fails, and growth of at least 2% requires a recorded material-growth
rationale. Flat filters add 13,226 tokens to each standard catalog because each
standalone tool schema must include the exhaustive closed leaf union; the
resulting catalogs occupy 18.068% and 14.440% of the 200,000-token support
floor. Then run:

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

The ignored live suite checks authenticated HTTP and gRPC before work and runs
serially so mutation verification does not compete with itself for the
server's rate limit. Every standard direct-router and spawned-stdio scenario
uses a prefix-authorized disposable space; the spawned production child is
registered for stop-and-wait cleanup before protocol initialization. Every
created object, type, and property is registered immediately for cleanup. The
suite requires a running headless server, env-only disposable credentials
from `.test-env`, and `anyr auth status` reporting both HTTP and gRPC pings as
OK. Run the direct-router and spawned-stdio targets explicitly from the
repository root:

```sh
source .test-env
# Prepare redacted_log and run_marker with the private derivative recipe in
# TESTING.md.
export ANYTYPE_DISPOSABLE_TEST_PROCESS=1
export ANY_MCP_HEADLESS_REDACTED_LOG_FILE="$redacted_log"
export ANY_MCP_HEADLESS_LOG_RUN_MARKER="$run_marker"
test -r "$ANY_MCP_HEADLESS_REDACTED_LOG_FILE"
cargo test -p any-mcp --lib headless_ -- --ignored --test-threads=1
cargo test -p any-mcp --features acceptance-harness --test headless_stdio_e2e -- --ignored --test-threads=1
```

The selectable `headless_direct_standard_*` and
`headless_stdio_standard_*` cases cover discovery, document/resource access,
views, mutations, exported-Markdown no-op replacement, and archive through
both entry paths. They execute all 14
standard tools and `resources/list`, `resources/templates/list`, and
`resources/read`, including bounded cursor terminality and binding,
ambiguity, explicit view selection, idempotent create, independent
read-after-write visibility, stale/count edit conflicts, and active/archive
evidence. Discovery additionally proves exact identities for a forwarded flat
list filter and rejects a continuation cursor whose filter binding changes
through both entry paths. Existing focused live regressions remain alongside
this acceptance baseline. `server::headless_integration` contains 19 ignored
direct-router cases; the library command also selects seven focused
cross-entry optional-registry regressions. The spawned target contains exactly
18 ignored live cases.

The shared Markdown no-op scenario independently waits for stable REST exports
and fresh `ObjectShow` identity/type/order evidence, supplies the complete
export plus its independently checked SHA-256 to `object_update`, and repeats
both MCP and `anytype-api` reads. It locks byte and typed-semantic identity for
the approved headings, lists, checkboxes, one-line quote, link, Unicode, and
multiline-paragraph cohort while recording the expected block-ID churn rather
than treating unchanged Markdown as proof that the block graph was unchanged.

Two spawned-stdio disposable lifecycle sentinels create and read an object by
its exact object and space IDs through the production MCP process. The normal
case and a deliberate callback-panic case both require the registered child
stop-and-wait record before independently constructing a fresh cache-disabled
client and proving absence through a direct request for the exact disposable
space ID. The panic sentinel catches the resumed panic
only outside `with_disposable_space_context`, after child and fixture cleanup
have completed.

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
`.test-env`. It must also set `ANY_MCP_HEADLESS_REDACTED_LOG_FILE` to an
absolute, readable runner-produced JSONL event file with credentials and
content removed. The job copies it into a parent-created `0600` derivative,
appends one fresh run marker, and the test verifies exact provenance,
allow-listed fields, and absence of credentials loaded from the configured
keystore; the job keeps that protected derivative for seven days on failure. Protect the
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
