# Stdio protocol verification

This document separates reproducible server evidence from client discovery.

## Current status

The production binary defaults to rmcp's latest released protocol, exactly
`2025-11-25`, and the standard `initialize`/`notifications/initialized`
lifecycle. Stateless MCP `2026-07-28` is a compiled and schema-tested preview
available only with exact environment value
`ANY_MCP_PROTOCOL=experimental-2026-07-28`. Absence or exact `stable` selects
production; all other values fail startup without being echoed. No first frame,
including `server/discover`, can implicitly select the preview.
Stable startup also rejects an initialize request that names the compiled
preview revision, preventing rmcp's draft-aware known-version negotiation from
promoting an ordinary process.

Application catalog selection is independent of protocol selection. The
absence of `ANY_MCP_PROFILE` selects compact; exact `standard` opts into the
fourteen-tool compatibility catalog. Stable and preview transports expose the
same selected tool contracts, and read-only filtering remains orthogonal.

Stable startup returns exact `-32700` parse and `-32600` invalid-request errors
while waiting for a valid initialize request. The preview retains discovery,
per-request metadata, result discrimination, cache hints, and inline version
errors. Both modes use the same handler and catalog implementation.

Closing stdin, `SIGINT`, and Unix `SIGTERM` are clean shutdown paths before or
after protocol initialization. They stop admission, cancel active work, drain
runtime-owned artifact settlement and staging work, and return a successful
process status without writing protocol-invalid diagnostics to stdout.

Both eras return one JSON-RPC error `-32700` with an explicitly present null
response ID for each syntactically malformed newline frame and continue reading
the stream. The stable path uses a bounded decoder in front of rmcp dispatch,
with one shared writer for decoder errors and service responses. Oversized or
syntactically valid but structurally invalid frames return `-32600`; every input
and output frame is capped at 2 MiB. Valid JSON-RPC notification shapes never
receive a response, even when a standard notification's parameters fail typed
decoding; invalid objects with a missing method or non-2.0 version still receive
`-32600` with explicit `id: null`.

The stable requirements come from the official
[MCP 2025-11-25 lifecycle](https://modelcontextprotocol.io/specification/2025-11-25/basic/lifecycle)
and [versioning rules](https://modelcontextprotocol.io/specification/2025-11-25/basic/versioning).
Preview requirements come from the official
[2026-07-28 release-candidate announcement](https://blog.modelcontextprotocol.io/posts/2026-07-28-release-candidate/),
[versioning rules](https://modelcontextprotocol.io/specification/draft/basic/versioning),
and [`server/discover` contract](https://modelcontextprotocol.io/specification/draft/server/discover).

## Automated harness

Run the production-process tests with:

```sh
cargo test -p any-mcp --test stdio_conformance --no-fail-fast
```

The test harness uses only Rust standard-library process, loopback TCP, thread,
channel, and deadline APIs. It therefore avoids shell and Unix-only process
assumptions in the test path. Each test starts the private process-test
wrapper for the real `anyr mcp` entrypoint
and a bounded authenticated Anytype HTTP fixture, then retains all stdout and
stderr bytes until clean stdin EOF.

The bounded production-process driver is shared with the ignored live suite;
the fast conformance cases keep their scripted HTTP fixture and five-second
deadline. The live target starts the same binary with inherited headless
credentials and a longer finite deadline. Individually selectable shared
scenarios execute all 14 standard tools and all three resource operations
through production stdio, then independently verify stored state through
`anytype-api`:

```sh
source .test-env
export ANYTYPE_DISPOSABLE_TEST_PROCESS=1
# Set raw_log and reviewed_log to distinct absolute mode-0600 files. Run
# review-server-log.py as a supervisor-owned background process before tests.
export ANY_MCP_HEADLESS_REDACTED_LOG_FILE="$raw_log"
export ANY_MCP_HEADLESS_REVIEWED_LOG_FILE="$reviewed_log"
export ANY_MCP_LIVE_PRIVATE_DIR="$(mktemp -d)"
chmod 0700 "$ANY_MCP_LIVE_PRIVATE_DIR"
python3 any-mcp/scripts/reviewed-evidence.py start "$reviewed_log" \
  "$ANY_MCP_LIVE_PRIVATE_DIR/reviewed-context" > "$ANY_MCP_LIVE_PRIVATE_DIR/evidence.env"
set -a; source "$ANY_MCP_LIVE_PRIVATE_DIR/evidence.env"; set +a
bash any-mcp/scripts/run-live-cgroup.sh test stdio -- \
  cargo test -p any-mcp --features acceptance-harness --test headless_stdio_e2e -- \
  --ignored --test-threads=1
bash any-mcp/scripts/run-live-cgroup.sh test discussions -- \
  cargo test -p any-mcp --features acceptance-harness \
  --test discussions_stdio_acceptance -- --ignored --test-threads=1
rm -rf -- "$ANY_MCP_LIVE_PRIVATE_DIR"
```

The same complete scenario set also runs through the production direct router.
A typed audit ties every advertised standard operation to exactly one
executable live scenario. Stable/preview malformed-frame and catalog
permutations remain deterministic against scripted HTTP, while compact,
read-only, and preview use focused real-headless sentinels. The process driver
records only request IDs, method names, fixed outcome categories, structural
stderr byte/line/category counts, generated fixture IDs, protocol metadata, and
cleanup status; it omits raw diagnostic lines and unknown fields, arguments,
response content, edit fragments, credentials, and raw upstream errors. Direct
cases can report `anytype-api` HTTP metric deltas. The spawned
production child owns a separate client and deliberately exposes no test-only
HTTP metrics endpoint, so spawned evidence uses MCP request/result/error
category counts instead.

The discussions process target starts dedicated stable and preview children
over one disposable space. It verifies absent, attached, repeated, and
wrong-scope results across both protocol modes, then hands the returned
discussion ID to `chat_message_list`.

The direct-router, spawned baseline, and discussions targets keep their
server-backed scenarios ignored by default. The direct-router library filter
also includes focused cross-entry regressions for optional registries and
files. Run the targets or select scenarios by name; the harness inventory is
authoritative as coverage evolves.

The harness bounds individual stdout frames to 2 MiB, aggregate stdout to
8 MiB, individual stderr lines to 64 KiB, aggregate stderr to 1 MiB, and the
in-process frame queue to 32 entries. Process cleanup closes stdin, waits only
to a deadline, kills a process that does not exit, waits for it, and joins both
reader threads even when another cleanup step fails. Fixture cleanup releases
hanging requests and joins the accept thread and every connection worker;
worker panics fail the test instead of being detached.

The preview, stable, and malformed-recovery production-process tests cover:

- stateless `server/discover`, no preview initialization lifecycle, and exact
  `-32022` version fallback data;
- required per-request protocol version and capabilities, optional validated
  client identity, and schema-valid omission across discovery/list/tool calls;
- exact response correlation for bounded string IDs, including the empty
  string permitted by the locked RequestId schema;
- `resultType: complete` on every successful preview result and mandatory
  public/private TTL hints on all cacheable results;
- stable `initialize`/`notifications/initialized`, exact negotiation from the
  oldest explicitly regression-tested released revision (`2024-11-05`)
  through `2025-11-25`, and clean EOF;
- exact pinned-host requests captured on 2026-07-20: Codex CLI 0.144.6 requests
  `2025-06-18`, while Claude Code 2.1.214 and Inspector 0.22.0 request
  `2025-11-25`;
- stable malformed/non-initialize first frames cannot activate preview, while
  an explicitly configured preview remains independently exercised;
- exact compact read-write (4) and standard read-only (10) tool catalogs,
  plus compact read-write identity across stable and preview transports and
  pre-protocol rejection of HTTP-only standard read-write in both modes;
- strict schemas and annotations for every advertised tool;
- `resources/list`, `resources/templates/list`, and `resources/read`;
- structured success and execution-error results;
- dispatch and input validation for every advertised tool;
- cancellation of in-flight fixture I/O;
- unknown tools and methods, malformed JSON-RPC request objects, syntactically
  malformed first and post-initialize frames, repeated parse errors, oversized
  frames, malformed standard and unknown notifications with no response,
  invalid notification-like objects with exact null-ID errors, and continued
  operation through a following ping and real tool call;
- LF-delimited JSON-only stdout in compact read-write and standard read-only
  modes; and
- stderr diagnostics that exclude credentials, protocol input values, and
  upstream document bodies.

The production-unlinked `files` read slice has an additional scripted preview
stdio scenario. It dispatches `file_read` through the same composed-router seam,
verifies exact `Range` and `If-Range` forwarding, and proves that native image
bytes occur in only the single payload block rather than being duplicated in
`structuredContent`. Transport-neutral tests separately lock MIME and charset
fallbacks, stable-revision audio behavior, canonical hash-bound resource reads,
validator conflicts, empty files, malformed URIs before I/O, and the 70,000-byte
text boundary (including one byte over), 96-KiB result, per-tool, and registry
token ceilings. Text admission uses a bounded counting sink rather than a
complete temporary encoded frame. This is fast scripted coverage from a
test-only registry, not the bounded name-resolver/retry seam or real-headless
acceptance required before the files registry can be linked into production.
The frame/token matrix separately covers all-zero, all-`0xff`, sequential, and
fixed-seed `0x0A11F17E` 64-KiB payloads for tool and resource results, plus
maximum legal IDs, URI, MIME, ETag, date, and numeric fields. Each corpus is
decoded from both result forms and compared byte-for-byte with its source and
SHA-256, while content ordering and single-copy payload placement are asserted.
Resource cases also cover cross-space/object
identity, current MIME refresh, bounded evidence, cancellation, timeout,
logical/physical retry metrics, and secret-free diagnostics.
Separate HEAD, GET, and mixed 429/504/terminal-transport-close sequences prove
six physical attempts with no seventh or later logical operation. Preview
resource reads are private with zero TTL; template discovery remains positively
cached and public.
Nonempty overrun sentinels also prove that `412` remains a conflict and `416`
remains validation, with neither result carrying a partial payload.

Preview frames are also checked against the vendored official draft JSON Schema;
that schema and its validator are test-only and add no production startup or
per-request evaluation cost. Purity and exchange depth are independent
assertions. Every stdout frame must
be one LF-terminated JSON object with `"jsonrpc":"2.0"`. The comprehensive
compact read-write case must emit exactly 13 stable or 15 preview response
frames; standard read-only must emit 19 stable or 21 preview frames. A missing
exchange cannot pass merely because the bytes that remain are pure.

The real `tools/list` entries are recursively canonicalized with the same
object-key ordering as the reviewed fixtures, then each complete pretty-printed
profile/read-only payload is compared with its corresponding included snapshot.
This locks every name, description, nested input/output schema, annotation,
array order, and omission on the actual stdio wire without copying or creating
a second catalog fixture. A focused real-process test proves that compact's
selected tools are byte-identical on the stable and explicitly enabled preview
transports.

The focused decoder acceptance test can be rerun with:

```sh
cargo test -p any-mcp --test stdio_conformance \
  malformed_json_returns_parse_error_and_preserves_the_stream
```

The same helper runs in compact read-write and standard read-only modes. Exact response
counts make an extra error frame, a dropped frame, or a desynchronized
following response fail independently of stdout byte-purity checks.

## Released compatibility matrix

Protocol revisions are requested by MCP hosts/clients and negotiated by the
server. A language model does not select the wire revision. Controlled stdio
handshake probes recorded the first initialize frame from each pinned host on
2026-07-20; the production-process matrix test replays the recorded client
name, version, and requested revision and verifies a usable post-initialize
ping. The raw probe frames contained no credentials but are not retained as
repository fixtures, so the claim is deliberately limited to these three
recorded fields.

| Host/client   | Pinned version | Requested revision | Stable result       |
| ------------- | -------------: | -----------------: | ------------------- |
| Codex CLI     |        0.144.6 |       `2025-06-18` | accepted and echoed |
| Claude Code   |        2.1.214 |       `2025-11-25` | accepted and echoed |
| MCP Inspector |         0.22.0 |       `2025-11-25` | accepted and echoed |

The oldest explicitly regression-tested released revision is `2024-11-05`.
Tests also cover `2025-03-26`,
`2025-06-18`, and the production default `2025-11-25`; an unknown revision
falls back to `2025-11-25`. This matrix is evidence for the pinned host builds,
not a promise about untested future client releases.

## External tool evidence

The commands below are optional discovery smoke tests. They do not replace the
preview acceptance tests.

Every live smoke requires `ANYTYPE_KEYSTORE` and an explicit
`ANY_MCP_CONNECTION_MODE`. Desktop mode uses HTTP only. Headless mode requires
the paired `ANYTYPE_URL` and `ANYTYPE_GRPC_ENDPOINT` values when either endpoint
is customized. HTTP workflows start after their HTTP probe even when saved
gRPC credentials cannot reach the headless service. A gRPC-only workflow makes
its own bounded admission probe before dispatch. `ANYTYPE_KEYSTORE_SERVICE` is
optional and defaults to `anyr`; tokens and account keys remain inside the
selected keystore and are never copied into client configuration.

On 2026-07-20, pinned
[`@modelcontextprotocol/inspector` 0.22.0](https://github.com/modelcontextprotocol/inspector)
successfully started the binary through stdio and listed the catalog now named
`standard`: 14 tools in read-write mode and 10 in read-only mode. Inspector
requested `2025-11-25` through the stable lifecycle. The run used an
isolated npm configuration/cache and an authenticated local Anytype test
environment. From the repository root, substitute equivalent Anytype
configuration for `.test-env` when necessary:

```sh
cargo build -p anyr
source .test-env
: "${ANYTYPE_URL:?required for this smoke test}"
: "${ANYTYPE_GRPC_ENDPOINT:?required for this smoke test}"
: "${ANYTYPE_KEYSTORE:?required for this smoke test}"
ANYTYPE_KEYSTORE_SERVICE="${ANYTYPE_KEYSTORE_SERVICE:-anyr}"
binary="$(realpath target/debug/anyr)"
inspector_state="$(mktemp -d)"

NPM_CONFIG_USERCONFIG="$inspector_state/npmrc" \
NPM_CONFIG_CACHE="$inspector_state/npm-cache" \
  npx -y @modelcontextprotocol/inspector@0.22.0 --cli -- env -i \
  ANYTYPE_URL="$ANYTYPE_URL" \
  ANYTYPE_GRPC_ENDPOINT="$ANYTYPE_GRPC_ENDPOINT" \
  ANY_MCP_CONNECTION_MODE=headless \
  ANYTYPE_KEYSTORE="$ANYTYPE_KEYSTORE" \
  ANYTYPE_KEYSTORE_SERVICE="$ANYTYPE_KEYSTORE_SERVICE" \
  ANY_MCP_PROFILE=standard \
  "$binary" mcp --method tools/list | jq -r '.tools | length'
# 14

NPM_CONFIG_USERCONFIG="$inspector_state/npmrc" \
NPM_CONFIG_CACHE="$inspector_state/npm-cache" \
  npx -y @modelcontextprotocol/inspector@0.22.0 --cli -- env -i \
  ANYTYPE_URL="$ANYTYPE_URL" \
  ANYTYPE_GRPC_ENDPOINT="$ANYTYPE_GRPC_ENDPOINT" \
  ANY_MCP_CONNECTION_MODE=headless \
  ANYTYPE_KEYSTORE="$ANYTYPE_KEYSTORE" \
  ANYTYPE_KEYSTORE_SERVICE="$ANYTYPE_KEYSTORE_SERVICE" \
  ANY_MCP_PROFILE=standard ANY_MCP_READ_ONLY=1 "$binary" mcp --method tools/list \
  | jq -r '.tools | length'
# 10

rm -r -- "$inspector_state"
```

The official
[`modelcontextprotocol/conformance`](https://github.com/modelcontextprotocol/conformance)
server runner was checked again on 2026-07-20. Its current server command
requires `--url`; it has no command/spawn option for a stdio server:

```sh
npx -y @modelcontextprotocol/conformance@latest server --help
```

It is therefore not treated as a compatible stdio gate, and no official
conformance pass is claimed from it.

## Client configuration evidence

Client configuration discovery and client-to-server protocol compatibility
are separate claims. On 2026-07-20, Codex CLI 0.144.6 accepted a real stdio
registration in a fresh isolated configuration root:

```sh
binary="$(realpath target/debug/anyr)"
codex_root="$(mktemp -d)"
CODEX_HOME="$codex_root" codex mcp add \
  --env ANY_MCP_READ_ONLY=1 anytype -- "$binary" mcp
CODEX_HOME="$codex_root" codex mcp get anytype
CODEX_HOME="$codex_root" codex mcp list
rm -r -- "$codex_root"
```

The isolated registration reported an enabled stdio server with the exact
binary path and redacted environment. A separate ephemeral smoke used the
existing client authentication but ignored user configuration, persisted no
session, and forwarded only the endpoint and keystore selectors needed by this
smoke. `ANYTYPE_KEYSTORE_SERVICE` is normalized to its documented `anyr`
default before forwarding, while credentials remain inside the selected
keystore:

```sh
binary="$(realpath target/debug/anyr)"
source .test-env
: "${ANYTYPE_URL:?required for this smoke test}"
: "${ANYTYPE_GRPC_ENDPOINT:?required for this smoke test}"
: "${ANYTYPE_KEYSTORE:?required for this smoke test}"
export ANYTYPE_KEYSTORE_SERVICE="${ANYTYPE_KEYSTORE_SERVICE:-anyr}"
codex exec --ephemeral --ignore-user-config --skip-git-repo-check \
  -C /tmp -s read-only \
  -c "mcp_servers.anytype.command=\"$binary\"" \
  -c 'mcp_servers.anytype.env={ANY_MCP_CONNECTION_MODE="headless",ANY_MCP_READ_ONLY="1"}' \
  -c 'mcp_servers.anytype.env_vars=["ANYTYPE_URL","ANYTYPE_GRPC_ENDPOINT","ANYTYPE_KEYSTORE","ANYTYPE_KEYSTORE_SERVICE"]' \
  --json \
  'Use the anytype MCP server to call server_status exactly once. Do not run shell commands or use any other tool.'
```

The completed MCP tool event carried structured `server_status` content with
HTTP and gRPC available. That capture predates the catalog-profile selector;
the current compact default additionally reports `profile: "compact"`,
`read_only: true`, and `enabled_toolsets: ["core", "documents"]`. No persistent
Codex user/project configuration or session was changed. For an intentional
persistent setup, the supported configuration follows the official
[Codex MCP configuration](https://developers.openai.com/codex/mcp/):

```toml
[mcp_servers.anytype]
command = "/absolute/path/to/anytype/target/debug/anyr"
args = ["mcp"]
env = { ANY_MCP_CONNECTION_MODE = "headless", ANY_MCP_READ_ONLY = "1" }
env_vars = [
  "ANYTYPE_URL",
  "ANYTYPE_GRPC_ENDPOINT",
  "ANYTYPE_KEYSTORE",
  "ANYTYPE_KEYSTORE_SERVICE",
]
```

Replace the command with the absolute path printed by
`realpath target/debug/anyr` after the build above. On Windows, resolve
`target\debug\anyr.exe` and paste the JSON/TOML-safe forward-slash form, for
example `C:/repo/target/debug/anyr.exe`. If native backslashes are retained,
double every backslash in the quoted TOML value, for example
`C:\\repo\\target\\debug\\anyr.exe`; a single backslash can be parsed as an
escape. The workspace build does not install `any-mcp` on `PATH`.

Claude Code 2.1.214 was also exercised on 2026-07-20. Its official local-scope
registration was run with an isolated configuration directory and disposable
project, so it did not touch the user's normal local or project configuration:

```sh
repo_root="$PWD"
binary="$(realpath target/debug/anyr)"
claude_config="$(mktemp -d)"
claude_project="$(mktemp -d)"
source .test-env
: "${ANYTYPE_URL:?required for this smoke test}"
: "${ANYTYPE_GRPC_ENDPOINT:?required for this smoke test}"
: "${ANYTYPE_KEYSTORE:?required for this smoke test}"
ANYTYPE_KEYSTORE_SERVICE="${ANYTYPE_KEYSTORE_SERVICE:-anyr}"
cd "$claude_project"
CLAUDE_CONFIG_DIR="$claude_config" claude mcp add \
  --transport stdio --scope local anytype \
  -e ANYTYPE_URL="$ANYTYPE_URL" \
  -e ANYTYPE_GRPC_ENDPOINT="$ANYTYPE_GRPC_ENDPOINT" \
  -e ANY_MCP_CONNECTION_MODE=headless \
  -e ANYTYPE_KEYSTORE="$ANYTYPE_KEYSTORE" \
  -e ANYTYPE_KEYSTORE_SERVICE="$ANYTYPE_KEYSTORE_SERVICE" \
  -e ANY_MCP_READ_ONLY=1 -- "$binary" mcp
CLAUDE_CONFIG_DIR="$claude_config" claude mcp get anytype
CLAUDE_CONFIG_DIR="$claude_config" claude mcp list
cd "$repo_root"
rm -r -- "$claude_config" "$claude_project"
```

Both health commands reported `Connected`. A second headless run used an
inline one-server configuration, strict MCP isolation, no session persistence,
and an allowlist containing only `mcp__anytype__server_status`:

```sh
binary="$(realpath target/debug/anyr)"
source .test-env
: "${ANYTYPE_URL:?required for this smoke test}"
: "${ANYTYPE_GRPC_ENDPOINT:?required for this smoke test}"
: "${ANYTYPE_KEYSTORE:?required for this smoke test}"
ANYTYPE_KEYSTORE_SERVICE="${ANYTYPE_KEYSTORE_SERVICE:-anyr}"
claude_mcp_config="$(jq -cn \
  --arg command "$binary" \
  --arg url "$ANYTYPE_URL" \
  --arg grpc "$ANYTYPE_GRPC_ENDPOINT" \
  --arg keystore "$ANYTYPE_KEYSTORE" \
  --arg keystore_service "$ANYTYPE_KEYSTORE_SERVICE" \
  '{mcpServers:{anytype:{command:$command,args:["mcp"],env:{ANYTYPE_URL:$url,ANYTYPE_GRPC_ENDPOINT:$grpc,ANYTYPE_KEYSTORE:$keystore,ANYTYPE_KEYSTORE_SERVICE:$keystore_service,ANY_MCP_CONNECTION_MODE:"headless",ANY_MCP_READ_ONLY:"1"}}}}')"
claude -p --no-session-persistence --strict-mcp-config \
  --mcp-config "$claude_mcp_config" \
  --allowedTools mcp__anytype__server_status \
  --disallowedTools Bash Read Write Edit WebSearch WebFetch \
  --output-format stream-json --verbose \
  'Call the anytype MCP server_status tool exactly once. Do not use any other tool.'
```

The completed tool event carried the same structured availability result. No
persistent Claude MCP configuration or session was changed. See the official
[Claude Code MCP guide](https://docs.claude.com/en/docs/claude-code/mcp) for
persistent configuration.

Codex, Claude Code, and Inspector currently select the production stable path.
Preview `2026-07-28` compatibility is established by the explicitly configured
production-process schema and wire acceptance suite rather than inferred from
client registration or stable client behavior.
