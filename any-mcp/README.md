# any-mcp

A workflow-oriented Model Context Protocol server for Anytype.

`any-mcp` is designed for reliable agent workflows such as discovering, reading, and safely editing documents.

**Status: Pre-release, under active development. Only tested on Linux and MacOS.**

Agents can use the versioned [`any-mcp` skill](../skills/any-mcp/SKILL.md) for
safe tool selection and tested PKM recipes, including Markdown capture, file
and collection organization, tagging, tasks, chat, and save-link ingestion.
The skill documents the narrow `anyr` fallbacks for rich chat blocks and chat
listeners that are not currently exposed by any-mcp, without claiming the
listener workaround provides a durable or atomic watermark.

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
cargo build -p anyr
anyr auth status --pretty
realpath target/debug/anyr
```

The last command prints the absolute path to the binary built in this checkout.
Replace `/absolute/path/to/anytype/target/debug/anyr` in both examples below
with that platform-specific absolute path; the workspace build does not install
`anyr` on `PATH`. On Windows, resolve `target\debug\anyr.exe` and prefer
the JSON/TOML-safe forward-slash form, for example
`C:/repo/target/debug/anyr.exe`. If native backslashes are retained, double
every backslash in either quoted format, for example
`C:\\repo\\target\\debug\\anyr.exe`; a single backslash can be parsed as an
escape.

An MCP host starts that binary and communicates with it over stdio.
The server does not perform login or print credentials. It reuses the endpoint
and keystore selected by `ANYTYPE_URL`, `ANYTYPE_GRPC_ENDPOINT`,
`ANYTYPE_KEYSTORE`, and `ANYTYPE_KEYSTORE_SERVICE` (default `anyr`). It also
accepts `anyr --keystore SPEC mcp ...`; that explicit option wins over the
selected MCP TOML file and environment. The default startup is the stable
`2025-11-25` protocol, the four-tool compact profile, and read-write access.
For a safer first registration, select compact read-only explicitly:

```json
{
  "mcpServers": {
    "anytype": {
      "command": "/absolute/path/to/anytype/target/debug/anyr",
      "args": ["mcp"],
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
command = "/absolute/path/to/anytype/target/debug/anyr"
args = ["mcp"]
env = { ANY_MCP_PROTOCOL = "stable", ANY_MCP_PROFILE = "compact", ANY_MCP_READ_ONLY = "1" }
env_vars = [
  "ANYTYPE_URL",
  "ANYTYPE_GRPC_ENDPOINT",
  "ANYTYPE_KEYSTORE",
  "ANYTYPE_KEYSTORE_SERVICE",
]
```

The [stdio protocol verification](docs/STDIO_CONFORMANCE.md) records the pinned
stable and preview protocol revisions tested with Codex, Claude Code, and MCP
Inspector. Client registration is separate from Anytype login: create and
store credentials with `anyr` or Anytype before starting the MCP host.

## Artifact data plane

The default-off `artifacts` registry moves file and document payloads through
authorized local roots or a loopback staging service while MCP carries only
logical locations, opaque handles, hashes, sizes, and small receipts. Select it
with `ANY_MCP_TOOLSETS=artifacts`; nothing is granted until an explicitly
selected policy file declares roots or staging.

The startup policy is an optional TOML file selected explicitly by `--config`
or `ANY_MCP_CONFIG`, with no automatic discovery beyond an `any-mcp.toml` in
the working directory. It separates import and export roots, permits optional
Anytype space restrictions, applies finite transfer and validator limits, and
makes local exports create-new only. Selected files must declare writable space
access explicitly so future access controls can default to read-only. Mounted
roots are admitted by required filesystem behavior instead of a filesystem-type
label; remote mounts may still hang outside application cancellation. Root and
local operation paths preserve platform-native path values without requiring
UTF-8 or lossy conversion. Read-only mode exposes status metadata without
activating root, staging, or validator authority.

Existing inline file tools and their limits are unchanged by this registry.
[Operator setup](#operator-setup) walks through a complete local or remote
deployment.

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
  `artifacts`, `body-blocks`, `chats`, `files`, `members`, `schema`, and
  `views-write`;
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

### Artifact policy file

The optional artifact policy file grants filesystem and space authority to the
artifact workflows without adding ambient path access. Generate an
owner-only starter file in the current directory, then validate it without
starting Anytype:

```sh
anyr mcp init
anyr mcp check
```

Both commands accept `-c FILE` or `--config FILE`. Initialization uses
create-new behavior and never overwrites an existing file. Server startup
accepts `-c ABSOLUTE_PATH`, `--config ABSOLUTE_PATH`, or `ANY_MCP_CONFIG`; the
command-line value wins. When neither is present, an existing `any-mcp.toml`
in the current directory is selected, followed by built-in defaults when that
file is absent. Print the consolidated binary version with `anyr -V` or
`anyr --version`.

An explicitly selected file must be an owner-controlled regular UTF-8 file no
larger than 256 KiB. The schema is closed and versioned. Unknown fields,
unsupported versions, unsafe file permissions, linked config files, duplicate
logical names, and invalid limits fail before protocol output. Selected MVP
files must declare `spaces.read_only = false` so a future read-only space
default cannot silently reinterpret an older writable configuration.
TOML syntax and schema failures report a redacted line and column, the known
schema path when available, and a safe reason without echoing configuration
values or physical paths. Run `anyr mcp check --config FILE` to validate a
policy without starting Anytype.

```toml
schema_version = 1

[spaces]
read_only = false
allowed = [{ name = "Personal" }]

[auth]
# Select exactly one. File selectors are platform-independent.
keystore.file = "/absolute/path/to/keystore.db"
# On Linux, use this instead of `keystore.file` to select Secret Service.
# keystore.secret-service = true

[limits]
artifact_bytes = 268435456
transfer_chunk_bytes = 8388608
staging_total_bytes = 1073741824

[[roots.import]]
id = "inbox"
path = "/absolute/operator-owned/import"

[[roots.export]]
id = "outbox"
path = "/absolute/operator-owned/export"

[staging]
enabled = true
root = "/absolute/operator-owned/private-staging"
bind = "127.0.0.1:8765"
public_base_url = "http://127.0.0.1:8765/artifacts/v1/"
```

Import roots grant existing-file reads. Export roots grant create-new writes,
with no overwrite mode. Tools use only the logical root ID plus a validated
relative path. Root IDs accept Unicode letters, decimal digits, and combining
marks plus ASCII `-` and `_`; they are trimmed at Pattern_White_Space
boundaries and normalized to NFC. Invisible default-ignorable characters are
rejected. IDs must also remain unique across import and export roots after
ASCII case folding, so spellings such as `inbox` and `INBOX` cannot coexist.

Ordinary `path` values are UTF-8. A native OS path that is not valid UTF-8 can
use one canonical unpadded base64url representation:

```toml
path_native = { encoding = "unix-bytes-base64url", value = "L2..." }
```

Windows uses `windows-wtf16le-base64url`. The parser decodes these values
directly into native OS strings and applies component, traversal, prefix, and
length checks without lossy Unicode conversion. ASCII C0 controls and DEL are
invalid in either native representation; Windows reserved device components
are also invalid path syntax, so both classes return fixed `validation` errors
before root lookup or filesystem I/O. Root activation retains
directory handles. The selected TOML policy is the only source of authority; on
stable stdio an MCP client that advertises the roots capability can additionally
narrow it (see [Client roots](#client-roots)), never widen it.
Capability-relative file walks on Linux,
macOS, and Windows reject links and reparse points, cross-filesystem
redirection, unsafe permissions or ACLs, hard-linked imports, traversal,
over-limit files, and export collisions. Export bytes remain in an
owner-private temporary file in the destination directory until commit, which
atomically publishes a complete create-new destination without replacing an
existing entry. Failed, cancelled, and dropped exports remove only their
private temporary file. Local destination syntax is validated before an
idempotency key is reserved, so a rejected path can be corrected and retried
without retaining process-lifetime operation state. Other definite failures
before publication, including unavailable roots, collisions, and staging
preflight refusal, also release the reservation; uncertain commit or
publication outcomes remain terminal to prevent redispatch and retain staging
ownership for cleanup.

Container bind mounts, Docker volumes, Nix bind mounts, and locally mounted
encrypted or object-backed filesystems can be used when they satisfy the same
capability checks. Mount type labels are advisory and are often hidden by a
container or sandbox. Avoid NFS and other network mounts for active artifact
roots because network stalls can cause unpredictable filesystem delays.

### Operator setup

Artifact authority is granted in four independent steps. Each one fails closed
on its own, so an incomplete setup refuses work instead of widening access.

1. **Credentials, from the environment only.** The server performs no login and
   never accepts credentials through MCP. It reuses `ANYTYPE_URL`,
   `ANYTYPE_GRPC_ENDPOINT`, `ANYTYPE_KEYSTORE`, and `ANYTYPE_KEYSTORE_SERVICE`
   (default `anyr`). With `ANYTYPE_KEYSTORE=env`, supply `ANYTYPE_KEY_HTTP_TOKEN`
   (and `ANYTYPE_KEY_ACCOUNT_KEY` for gRPC-backed workflows) through the host
   environment or a secret facility. Do not place secrets in the policy file:
   its `[auth]` table selects *which* keystore to read, never a secret value.
2. **Select the registry.** `ANY_MCP_TOOLSETS=artifacts` advertises the eight
   artifact tools. Without the selector they are absent from `tools/list`.
3. **Select the policy.** `--config ABSOLUTE_PATH` or `ANY_MCP_CONFIG` chooses
   the strict TOML file. Validate it before starting a client with
   `anyr mcp check --config FILE`.
4. **Verify at run time.** Call `artifact_status`. It reports
   `local_roots_active`, `import_root_count`, `export_root_count`,
   `staging_configured`, `staging_active`, remaining staging bytes and entries,
   and the validator counts, so an operator can confirm the authority a client
   actually received without exposing record metadata.

#### Local clients (stdio)

A local MCP host starts `anyr mcp` over stdio and moves bytes through
authorized directories on the same machine. Declare at least one import root
for reads and one export root for create-new writes, and leave `[staging]` out:

```toml
schema_version = 1

[spaces]
read_only = false
allowed = [{ name = "Personal" }]

[[roots.import]]
id = "inbox"
path = "/absolute/operator-owned/import"

[[roots.export]]
id = "outbox"
path = "/absolute/operator-owned/export"
```

```json
{
  "mcpServers": {
    "anytype": {
      "command": "/absolute/path/to/anytype/target/debug/anyr",
      "args": ["mcp"],
      "env": {
        "ANY_MCP_TOOLSETS": "artifacts",
        "ANY_MCP_CONFIG": "/absolute/path/to/any-mcp.toml"
      }
    }
  }
}
```

Clients address bytes as a logical root ID plus a relative path, for example
`{"local": {"root": "inbox", "path": "reports/q3.md"}}`. Absolute paths, `..`,
symlinks, and unlisted root IDs are refused, and an unauthorized root and an
absent file are both reported through the same fixed not-found message so a
caller cannot probe the filesystem layout.

#### Client roots

Stable stdio serves one client session per process, so it takes one bounded
`roots/list` snapshot from a client that advertises the roots capability and
uses it as a session-scoped narrowing layer. A local artifact path is effective
only when it lies beneath both a configured root and at least one client root,
so a client root outside every configured root grants nothing and an empty
snapshot denies every local root. The snapshot is taken at most once per
session; `notifications/roots/list_changed` is ignored, so a changed
client root needs a new session.

A client that advertises no roots capability keeps the configured policy
unchanged, as do preview stdio and the HTTP transport. A snapshot that cannot be
frozen securely — transport failure, timeout, more than 64 roots, a duplicate
alias, or a URI that is not a canonical local `file:` directory — disables local
root operations for the rest of the session rather than falling back to the
broader configured policy. Staged operations are unaffected. Client root URIs
and display names never appear in diagnostics or receipts.

#### Remote clients (HTTP staging)

When the MCP client and its files are not on the server's filesystem, enable
the staging service and omit local roots from client calls. Staging binds a
loopback address only; terminate TLS in a separately managed reverse proxy and
point `public_base_url` at that proxy:

```toml
[staging]
enabled = true
root = "/absolute/operator-owned/private-staging"
bind = "127.0.0.1:8765"
public_base_url = "https://artifacts.example.internal/artifacts/v1/"
```

Uploads are two steps: `artifact_stage_upload` allocates an exact-size record
and returns an opaque handle, an upload URL that contains only the non-secret
record identifier, and an expiry. The client then `PUT`s the exact bytes with
`Authorization: Bearer <handle>` and passes `{"staged_handle": "<handle>"}` as
the tool `source`. Exports reverse this with `{"remote": true}`, and
`artifact_release` deletes the staged bytes once the client has read them.
Handles never appear in URLs, and the server never fetches a caller-supplied
URL. Staging requires activated local roots: it is a data plane on top of the
policy, not a replacement for it, and `artifact_status` reports
`staging_active: false` whenever roots are inactive.

The staging root is a closed durable layout owned exclusively by one server
instance: `instance.lock` plus the `records/`, `payloads/`, `tmp/`, and
`tombstones/` directories. Every staging state transition is flushed to disk
before it becomes visible, and startup reconciles the layout — resuming
interrupted deletions, truncating uncommitted upload bytes, and reviving
retained import-reconciliation evidence — before the listener binds. Roots
written by releases before this layout are migrated automatically on first
activation. Activation fails with the fixed `artifact state reconciliation
failed` category when the root holds unknown entries or evidence that a prior
cleanup crossed its deletion barrier without proof of completion; a runtime
durability failure shuts the server down with the fixed `artifact staging
durability uncertain` category. Both conditions require the operator to
inspect the staging root (or point staging at a fresh empty directory) before
restarting. On Windows, single-instance exclusion relies on the exclusively
locked `instance.lock`, which Windows refuses to unlink or replace while
held, and directory-entry durability is delegated to the NTFS metadata
journal.

#### Space policy

`spaces.allowed` selects one of three shapes, checked by every space resolver
before any request is constructed.

| Configuration                              | Effect                                                                                                 |
| ------------------------------------------ | ------------------------------------------------------------------------------------------------------ |
| `allowed` omitted                          | Every space the account can otherwise reach is permitted.                                              |
| `allowed = []`                             | No space is permitted; space-scoped calls report `authentication`.                                     |
| `allowed = [{ id = "…" }, { name = "…" }]` | Only these spaces are permitted; names resolve once at startup and the canonical ID set is then fixed. |

A restricted policy also rejects unscoped global search and space creation.
Because entries resolve once, a space renamed after startup keeps the identity
it was granted, and a new process is required to pick up a policy change.

#### Read-only mode

`ANY_MCP_READ_ONLY=1` is a server mode, not a policy field; a selected policy
file must still declare `spaces.read_only = false`. A read-only server removes
every artifact mutation from its catalog, keeps only `artifact_status`, and
activates no roots, no staging service, and no validators. A mutation named
anyway is refused with the fixed message
`This Anytype server is read-only. Mutating workflows are disabled.` Read-only
is the recommended first registration for a new client.

#### Quotas, deadlines, and time-to-live

Every `[limits]` value is optional, clamped to the range below, and rejected
outside it. Lowering `artifact_bytes` also lowers the omitted defaults of the
single-artifact limits that depend on it.

| Key                           | Default    | Range                 |
| ----------------------------- | ---------- | --------------------- |
| `artifact_bytes`              | 268435456  | 65536 – 1073741824    |
| `transfer_chunk_bytes`        | 8388608    | 65536 – 67108864      |
| `staging_total_bytes`         | 1073741824 | 1048576 – 17179869184 |
| `staging_entries`             | 256        | 1 – 4096              |
| `staging_ttl_secs`            | 900        | 60 – 86400            |
| `staging_connections`         | 64         | 1 – 256               |
| `staging_requests`            | 64         | 1 – 256               |
| `staging_requests_per_minute` | 600        | 1 – 10000             |
| `staging_header_bytes`        | 16384      | 4096 – 65536          |
| `staging_header_secs`         | 5          | 1 – 30                |
| `staging_no_progress_secs`    | 30         | 1 – 120               |
| `receipt_bytes`               | 16384      | 2048 – 65536          |
| `operation_secs`              | 300        | 1 – 900               |
| `cleanup_batch`               | 64         | 1 – 1024              |
| `discovery_rows`              | 1000       | 1 – 10000             |
| `markdown_bytes`              | 10485760   | 1 – 67108864          |
| `markdown_chars`              | 100000     | 1 – 1000000           |
| `validator_processes`         | 4          | 1 – 16                |
| `validator_total_input_bytes` | 536870912  | 1 – 2147483648        |

Staged records expire after `staging_ttl_secs` and are removed in batches of at
most `cleanup_batch`, so an abandoned client upload cannot retain the quota
indefinitely. `operation_secs` bounds one artifact tool call end to end.

Limits are also checked against each other, and an inconsistent set is rejected
at load: `transfer_chunk_bytes` and `markdown_bytes` may not exceed
`artifact_bytes`; `artifact_bytes` may not exceed `validator_total_input_bytes`
or, when staging is enabled, `staging_total_bytes`; `cleanup_batch` may not
exceed `staging_entries`; and neither staging deadline nor any validator
timeout may exceed `operation_secs`.

#### Validators

Validators are optional and operator-declared. Each `[[validators]]` entry
pins an absolute executable path plus its expected SHA-256, and runs with fixed
arguments, a cleared environment, bounded input and output, a deadline, and a
process-group boundary. A `required = true` validator blocks the operation when
it rejects or is unavailable; an optional one contributes a bounded receipt
category. MCP callers can never supply an executable, flag, environment
variable, or command template. Validator execution is available on Linux today;
macOS and Windows retain the validated configuration and report the validator
unavailable, so a required validator blocks matching operations there.

### Token-free artifact workflows

Select `ANY_MCP_TOOLSETS=artifacts` to move file and document payloads without
putting their bytes in MCP messages:

- `artifact_status` reports capability counts and availability;
- `artifact_stage_upload` allocates an exact-size remote upload and returns the
  bearer separately from its URL;
- `artifact_release` removes one authenticated staged artifact;
- `file_import` and `file_export` stream arbitrary MIME files between Anytype
  and an authorized local root or remote stage; and
- `document_import_create`, `document_import_update`, and `document_export`
  transfer strict UTF-8 Markdown or plain text and return identities, counts,
  hashes, and bounded receipts. Document creation can also apply up to 50 typed
  properties validated against the resolved Anytype type.

Local paths contain a logical root ID and relative path. If no roots are
declared, root-based calls explain that you must select a policy file with
`ANY_MCP_CONFIG` or `--config`. Imports verify source identity, size, and
SHA-256 before mutation. Exports use create-new publication and never overwrite
an existing destination. Document update requires the current canonical body
hash and skips the mutation when the replacement is already canonical.

Remote staging binds only a configured loopback address. An external deployment
terminates HTTPS in a separately managed reverse proxy and points
`public_base_url` at that proxy. Bearers remain in the `Authorization` header
and are never embedded in URLs. The server does not fetch URLs. A workflow that
downloads a user-supplied link must do that outside any-mcp before staging the
result.

Configured `file-mime` validators use a pinned native executable, fixed
arguments, a cleared environment, bounded input and output, a process deadline,
and a process-group boundary. Required validator failures block the operation;
optional failures produce a bounded receipt category. MCP callers cannot
provide executable paths, flags, environment variables, or validator command
templates.

Validator execution is currently available on Linux. macOS and Windows retain
the validated configuration but report the validator unavailable until their
retained-handle sandbox implementations land; a required validator therefore
blocks matching operations on those platforms.

```toml
[[validators]]
id = "mime"
driver = "file-mime"
path = "/absolute/path/to/native-file-executable"
sha256 = "0000000000000000000000000000000000000000000000000000000000000000"
required = false
mime = ["*/*"]
timeout_secs = 5
memory_bytes = 268435456
input_bytes = 268435456
stdout_bytes = 65536
stderr_bytes = 65536
fields = 32
field_bytes = 4096
platform = "linux-retained-fd-v1"
```

Replace the zero digest with the lowercase SHA-256 of the admitted executable.

Space policy is active independently of the artifact registry. An
omitted `spaces.allowed` permits every space that the configured Anytype
account can otherwise access. An explicit empty array permits none. ID entries
are validated directly; name entries resolve once during authenticated
startup, and the resulting canonical ID set stays fixed for the process.
Every ordinary or response-bounded space resolver checks that set before
constructing a domain request. Exact-ID document and file resources perform
the same check before HTTP. A restricted policy rejects global
`object_search` unless a space is supplied, and only an omitted allowlist may
create a new space whose identifier was not knowable at startup.
`space_list` filters disallowed rows before output and, under a restricted
policy, scans at most 10 upstream pages and the configured
`limits.discovery_rows` ceiling. Its opaque cursor is issued only when another
permitted row was observed, so disallowed identities, totals, and continuation
hints do not enter the result.

Catalog construction assigns every advertised tool to an explicit global,
filtered-global, optional-resolved, resolved-space, or conditional-create
policy owner. It rejects an unclassified tool or resource family, so adding a
new catalog operation cannot silently omit the space boundary.

The offline production integration matrix composes all seven linked registries
together in compact and standard, read-write and read-only configurations. It
locks their exact catalogs and canonical status, stable/preview contract
identity, gRPC requirement union, disabled stale-call rejection, and aggregate
`o200k_base` catalog cost. The same matrix proves that an absent selector leaves
all four reviewed Phase 1 catalog snapshots and `server_status` unchanged. A
leave-one-registry-out sweep also proves that every omitted registry remains
unreachable while the other six are active.
The production ownership audit independently binds each of the 38 domain tools,
`optional_toolset_status`, and the file byte-resource family to one fast and one
real-headless executable scenario. It rejects missing, duplicate, unknown, or
untyped catalog and scenario entries. Compile-bound runner tables execute all
eight fast workflow groups and all seven spawned real-headless registry
workflows. The files workflow verifies real upload, metadata, bounded reads,
resources, independent download, diagnostics, and cleanup. A separate
all-selected sentinel composes stable read-write and preview read-only children
and performs one real-backend read per registry.

The default-off `body-blocks` registry exposes `body_block_list` plus six
write workflows for one-block create, update, delete, move, and bounded rich
page creation and recovery. It uses the typed `anytype-api` body model only. Reads return
exact block identity, document order, and a canonical snapshot hash; mutations
require that hash and verify their result. Rich page construction is a finite
flat plan and reports complete, partial, or indeterminate evidence without
claiming transactionality. Generated tables conservatively count their root,
two layout regions, rows, columns, and logical `rows × columns` capacity
against the 256-block plan ceiling. Success separately requires Heart's exact
sparse subtree: no cells without a header, or grey-background empty paragraph
cells under the first header row only.
`rich_page_resume` can complete only the never-attempted suffix of a retained
partial `rich_page_create` receipt in the same runtime facade. It accepts one
claim per receipt after it re-proves the page, retained page type, and authored
prefix. Restart, facade replacement or eviction, missing evidence, an attempted
or indeterminate boundary, and a consumed claim return `conflict`. It never
replays a page create or an uncertain block write. When recovery refuses,
read the page with `body_block_list` before choosing deliberate primitive
mutations.
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
`artifacts`, `body-blocks`, `chats`, `members`, `files`, `schema`, and `views-write`
registries are linked and can be selected explicitly or combined in one
comma-separated `ANY_MCP_TOOLSETS` value; they are absent by default.
Acceptance-blocked `discussions`, plus proposed `admin`, are not selectable in
this release. Their names become valid selectors only when a complete,
independently reviewed production registry is linked.

The `body-blocks` R5 registry provides seven workflow tools for stable typed
body pages, verified single-block create/update/delete/move, and finite rich
page construction; read-only mode retains only body listing. All schemas use
closed nonrecursive variants, fail opaque and read-restricted content closed,
permit inert bookmark creation without network fetching, and accept YouTube
creation only as an exact 11-character video ID normalized to inert canonical
document data.
Rich construction is non-atomic and returns bounded applied, failed, and
not-attempted evidence without compensation. `rich_page_resume` can claim one
retained partial receipt and issue only its never-attempted suffix after
re-proving the page, type, and authored prefix.
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
the shipped `anyr mcp` process and is not built by default. The child appends
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

The approved attached-discussions design keeps page discussions separate from
ordinary chats. Its production-unlinked
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
real stdio frames through this exact reviewed production registry. Deadline
state-machine regressions use virtual time, while deadline-independent cache,
capacity, and pre-dispatch states are tested without artificial expiry. The
slice exposes no edit, attachment,
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
  Unsupported condition/value combinations remain explicit errors, and the
  historical upstream parsing report remains open.
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

- `src/artifact_config.rs`: selected TOML schema, native paths, and limits.
- `src/artifact_roots.rs`: retained root capabilities and client narrowing.
- `src/artifact_client_roots.rs`: session-scoped `roots/list` snapshot decision.
- `src/space_policy.rs`: frozen canonical Anytype-space authorization.
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
- `tests/discussions_stdio_acceptance.rs`: ignored stable and preview process
  acceptance for cleanup-owned attached discussions.
- `tests/live_gate_manifest.rs`: offline closed-inventory and workflow-filter
  checks for every ignored live target.
- `tests/schema/mcp-2026-07-28.json` — official draft schema used only as a
  test oracle for actual preview requests and results.
- `docs/STDIO_CONFORMANCE.md` — reproducible test, Inspector, and client
  discovery evidence with current compatibility limits.

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
budget, and unit tests plus the real-process stdio suites on one row per
released target: Linux x86_64/aarch64, macOS aarch64, and Windows
x86_64/aarch64. Every row also runs both compiled artifact control planes —
the library plane, which locks the exact artifact catalog and schema
snapshots, and the spawned `headless_stdio_e2e` plane, which adds the
adversarial case matrix — serially. The process harness uses only portable
Rust process, TCP, path, environment, thread, and channel APIs; it does not
depend on Unix signals, `/tmp`, executable suffixes, or shell scripts.

The workflow is manual-only during matrix qualification. Its tier selector
always runs the portable matrix, then optionally adds the existing headless or
clean-server live lane. Every job installs the Rust version selected by
`rust-toolchain.toml`.

The release workflow can also be started manually for a selected branch or
tag. Manual runs build either all cargo-dist targets or one selected target
from the supported architecture list. They upload build artifacts for
inspection but never publish a GitHub release or Homebrew formula.

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
test-mode backend selector. The executable catalog audit and disposable live
scenarios maintain the architecture and evidence contract.

The portable matrix now carries one row per released target, including the two
aarch64 rows, so it is an architecture gate as well as an OS-family gate. The
aarch64 rows use hosted arm labels; a fork without access to them must move
those rows to self-hosted labels rather than drop them. Live real-backend rows
stay Linux-only, because the self-hosted headless environment and its
systemd-scoped containment helper exist only there: on macOS and Windows the
pipeline proves the portable artifact matrix (configuration, path,
state-machine, staging-record, adversarial case matrix, and catalog/schema
snapshots) but not the live protocol and staging rows. The current dist
configuration produces macOS aarch64, Linux x86_64/aarch64, and Windows
x86_64 artifacts. No external `any-mcp` release is published by this
documentation change.

## Headless integration tests

The ignored live suite checks authenticated HTTP and gRPC before work and runs
serially so mutation verification does not compete with itself for the
server's rate limit. Library cases use prefix-authorized disposable spaces; no
direct-router case mutates an ambient fixture. Spawned disposable workflows
register each production child for stop-and-wait cleanup before protocol
initialization, while focused spawned profile sentinels retain their
cleanup-owned test context. Every created object, type, and property is
registered immediately for cleanup. The suite requires a running headless
server, env-only disposable credentials from `.test-env`, and `anyr auth
status` reporting both HTTP and gRPC pings as OK. Run the direct-router,
spawned-stdio, and discussion-process targets explicitly from the repository
root:

```sh
source .test-env
# Set redacted_log to the absolute mode-0600 reviewed server event file.
export ANYTYPE_DISPOSABLE_TEST_PROCESS=1
export ANY_MCP_HEADLESS_REDACTED_LOG_FILE="$redacted_log"
export ANY_MCP_LIVE_PRIVATE_DIR="$(mktemp -d)"
chmod 0700 "$ANY_MCP_LIVE_PRIVATE_DIR"
python3 any-mcp/scripts/reviewed-evidence.py start "$redacted_log" \
  "$ANY_MCP_LIVE_PRIVATE_DIR/reviewed-context" > "$ANY_MCP_LIVE_PRIVATE_DIR/evidence.env"
set -a; source "$ANY_MCP_LIVE_PRIVATE_DIR/evidence.env"; set +a
bash any-mcp/scripts/run-live-cgroup.sh test direct -- \
  cargo test -p any-mcp --lib headless_ -- --ignored --test-threads=1
bash any-mcp/scripts/run-live-cgroup.sh test stdio -- \
  cargo test -p any-mcp --features acceptance-harness --test headless_stdio_e2e -- \
  --ignored --test-threads=1
bash any-mcp/scripts/run-live-cgroup.sh test discussions -- \
  cargo test -p any-mcp --features acceptance-harness \
  --test discussions_stdio_acceptance -- --ignored --test-threads=1
rm -rf -- "$ANY_MCP_LIVE_PRIVATE_DIR"
```

The protected workflow validates
`ANYTYPE_TEST_SPACE_PREFIX` as 1 through 485 ASCII letters, digits, hyphens, or
underscores after sourcing its environment, exports the dedicated-process
gate, and rejects captured test output that reports a skipped admission or an
unexpected executable count. The hosted test lane compares every ignored
library and whole-binary test name with closed manifests without contacting a
server. Protected CI therefore cannot pass without running the disposable
callbacks.

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
this acceptance baseline. `server::headless_integration` contains the
direct-router cases, while the library command also selects focused cross-entry
regressions for the optional registries and files. The spawned target enables
its live cases with the `acceptance-harness` feature.

### Artifact data-plane acceptance matrix

`tests/support/artifact_acceptance.rs` is the reusable harness for artifact
acceptance. It declares a closed transport matrix of four control planes
(scripted JSON-RPC frames, in-process direct router, spawned stable stdio,
spawned preview stdio) crossed with two data planes (authorized local roots and
the remote HTTP staging service). An offline inventory test proves that the
direct-router and spawned targets together execute all eight transports exactly
once, so a transport cannot silently lose coverage.

Every transport runs the same smoke scenario — file import/export, document
create/export/update, and an explicit staging allocate/release — and returns
content-free evidence: the exact advertised artifact catalog snapshot, verified
byte lengths, and SHA-256 hashes. Each run compares the advertised catalog with
the reviewed `tests/snapshots/artifact-catalog.snap` fixture and then compares
the complete executed matrix for exact parity, so a divergence between the wire
envelope, the router, and either protocol revision fails the suite.

The spawned matrix owner also carries the two client-root protocol rows. One
stable stdio session advertises the MCP roots capability and answers the single
bounded `roots/list` snapshot with the physical import root only: the retained
import root still imports real bytes, the configured export root is refused
with the uniform hidden-resource rejection, and a second local operation proves
the session decision is frozen rather than re-queried. The other session
advertises no roots capability and must never be asked, so both configured
roots stay effective and the child emits no server-initiated frame at all. Both
rows must report the same advertised catalog and the same `artifact_status`
projection, because narrowing is a per-session authority decision that must not
be observable in the advertised surface.

Two further scenario families reuse the same harness. The policy family
executes a complete server configuration — no selected file, space policy
omitted, empty, or restricted elsewhere, read-only mode, and disabled staging
— across the scripted, direct, stable, and preview control planes, and every
one of them must report the same advertised catalog, the same
`artifact_status` projection, and the same refusal code and guidance. The
no-selected-file row is the compatibility mode: the fixture still owns a
complete policy on disk, no child selects it, and the server must advertise
the unreduced read-write catalog while reporting zero roots and no staging and
refusing every root-based call with the fixed roots-required guidance.

The same family then closes the configuration-selection truth table that the
required `spaces.read_only` declaration defines. A started server proves the
two accepted rows (no selected file, and a selected file declaring
`read_only = false`); the two refused rows start a bounded production child on
both spawned stdio profiles, and each must die before its first protocol frame
with an empty stdout and exactly one error diagnostic — `required field is
missing` at the located schema path for an absent declaration, and
`selected any-mcp configuration must declare spaces.read_only = false` for
`read_only = true`. Neither diagnostic may contain a fixture path or a
credential, and both profiles must report the identical reason.

The content family proves what the artifact contract does with real bytes on
both data planes. `mime_matrix` imports and exports binary, UTF-8 text, PNG,
RIFF/WAVE, and out-of-tree payloads, comparing declared, stored, and exported
essence with verified lengths and hashes. `document_canonicalization` creates
from Markdown, exports the canonical body, requires that re-importing that
exact body is reported as a no-op, and records every lossy difference as a
closed category: the appended plain-text hard break, importer escaping that
rewrites the dispatched bytes before Anytype canonicalizes anything, and a
non-canonical Markdown rewrite. `validator_optional` and `validator_required`
probe a matched, a mismatched, and an out-of-scope MIME declaration, so an
optional validator reports the rejection while the import proceeds and a
required one refuses it.

The lifecycle family runs six spawned-production scenarios against the same
real backend. Quota coverage independently exhausts the configured entry and
aggregate-byte reservations. TTL coverage waits for the minimum admitted
expiry, requires both a stale `404` and an exact lock-only staging snapshot,
and checks the fixed counts-only cleanup event. A concurrent create-new export
race must yield exactly one verified destination and one `conflict` without
changing the winner. Cancellation uses an acceptance-only scheduling pause
around the production pre-dispatch boundary, sends the standard MCP
`notifications/cancelled` notification, requires no response frame for that
request, and then reuses the same staged source successfully. An abrupt child
termination leaves one completed private stage for the next production child
to reconcile; the old bearer must fail through both MCP and HTTP while a fresh
generation remains usable.

The payload-ceiling scenario uploads bytes only through the staging HTTP data
plane, imports both a small payload and a one-MiB ceiling payload, and measures
the complete JSON-RPC responses with exact serialized-byte and cl100k token
counts. Both remain below fixed 16-KiB/4096-token ceilings, may differ by at
most 128 bytes/32 tokens, and an `artifact_bytes + 1` reservation is refused as
`bounded_result`. Payload bytes never enter an MCP argument, result, transcript,
or diagnostic.

The adversarial family owns a closed 122-case inventory. Its case-status map
keeps unproved rows pending until their executable owner passes, while
implemented rows are executed or explicitly unsupported for a required
platform primitive. Pending closure owners include raw-listener assertions for
partial offsets, short bodies, resumable disconnects, overruns, incomplete
imports, replayed chunks, allocation-time quota refusal, and rejected download
ranges, plus a spawned kill-mid-frame assertion that
accepts only complete JSON-RPC frames followed by one bounded, truncated final
fragment. Their inventory status advances only after the live owner passes.
Cleanup owners likewise assert failed import/export rollback, single-use
release, TTL invalidation, required-validator refusal, and read-only catalog
isolation without promoting those rows before live execution. A cleanup pass
is separately proven offline to never remove an entry absent from the staging
state index. A spawned crash-restart owner kills production children
mid-upload, during the Anytype import dispatch, and inside the atomic export
commit, then requires byte-uniform `not_found` for every previous-generation
handle, at most one dispatched candidate object, an absent or hash-correct
export destination, startup rejection of a second process on an owned staging
root, and a complete happy-path import after recovery. Every executed
failure-robustness row must additionally state its checkable evidence — an
offline unit owner or a recorded live-owner run — and the inventory test
rejects a promotion without evidence or an offline claim on a live-only
owner.
Flood owners measure aggregate status at maximum staged-record occupancy and
bound/redact the spawned child's diagnostic burst.
Implemented coverage exercises traversal and native-path grammar,
capability-indistinguishable root refusals, volume case and normalization
aliases, Windows device names, staging-record case sensitivity, bounded names
and payloads, MIME conflicts, invalid document encodings, and hostile upstream
metadata. The implemented dynamic set covers `SYM-01` through `SYM-13`,
`RACE-01` through `RACE-10`, and `HLINK-01` through `HLINK-06`. Import races
exercise rename-over, extension, truncation, and retained-root swap outcomes;
the export-root swap requires a classified failure with no remaining file.
Windows junction and non-junction reparse probes seed a target sentinel, verify
their native tags, and prove refusal leaves both target inventory and object
inventory unchanged. A host without the required primitive records its fixed
capability outcome.
Direct owners and spawned stdio owners enforce their exact partitions, and the
spawned gate separately owns production startup rejections. Each owner
records a fixed case ID only after its exact outcome, resource inventory,
staging state, and cleanup assertions pass. Windows-only aliases and
non-activating validator targets are explicit capability records rather than
silent skips. Direct traversal and alias checks also snapshot test-only
retained-root access and successful-open counters, while every live owner binds
its log audit to one opened owner-private descriptor and rejects bounded-window
matches for fixture paths, transient handles, or credentials.

The final protocol and robustness families cover handle guessing, expiry,
restart, replay, direction, route, and space binding; malformed and partial
HTTP transfers; abrupt process loss and private-state reconciliation;
validator and response floods; and cleanup after every refusal class. Each of
the 50 `HAND`, `PART`, `CRASH`, `FLOOD`, and `CLEAN` rows names one executable
staging, spawned-lifecycle, validator, direct-teardown, or read-only-catalog
owner. The case partition checks that these owners cover the exact closed set
once; a row leaves the pending partition only with its executable assertion.
The TTL owner proves an expired handle is uniformly unavailable and restores
quota. It also compares the complete normalized MCP not-found payload for
unknown, expired, and cross-space handles, and separately compares staging HTTP
status and body for expired and wrong-route requests. The partial-write owner
reuses a consumed upload handle with a new operation key and requires one object
plus a fixed not-found refusal. The payload owner constructs a document above
the configured Markdown ceiling and requires a bounded error frame without
publication.
Four private child gates make cancellation timing explicit: before and during
file-export publication, after file-import dispatch, and after document-update
dispatch. Their owners require a conflict result, no partial destination or
spliced document, at most one imported object, idempotent retry settlement, and
complete child cleanup.

The hard-link cleanup case retains the staging reservation after a hostile-link
conflict, including after the outside link is removed. That stable conflict
prevents a later pathname-based deletion from changing the staged record.

The content validator scenarios declare one real host `file(1)`-compatible
executable pinned by absolute path and SHA-256; nothing is shipped, and the
fixture admits exactly the ownership and mode that the production validator
boundary admits. Set `ANY_MCP_ACCEPTANCE_VALIDATOR` to an exact executable path
when the host keeps one outside `PATH`. The validator-flood scenarios instead
copy the acceptance process binary into the same private, immutable boundary,
where it produces oversized stdout or stderr, exceeds the configured deadline,
and leaves a descendant for the process-group cleanup assertion. Validator
execution is Linux-only, so other platforms keep the validated declaration and
expect zero available validators.

Fixture discipline is part of the harness rather than each scenario: a
prefix-authorized disposable space, a private `0700` policy tree with `0600`
sources and operator policy, immediate registration of every created object and
file, exact teardown when the fixture is dropped, and rejection of skipped
disposable admission. The protected gate requires
`ANY_MCP_HEADLESS_REDACTED_LOG_FILE` to name its captured Anytype server log.
Direct and spawned owners audit only the descriptor-bound appended window and
fail on any panic, fatal, or error class outside the already isolated upstream
set; only counts and fixed category names are reported, never log lines.

Select the live acceptance targets explicitly: six direct-router cases by
their exact paths, and the five spawned cases by their shared prefix:

```sh
cargo test -p any-mcp --lib headless_artifact_direct_transport_matrix_scenario \
  -- --ignored --exact \
  server::headless_integration::headless_artifact_direct_transport_matrix_scenario
cargo test -p any-mcp --lib headless_artifact_policy_direct_scenarios \
  -- --ignored --exact \
  server::headless_integration::headless_artifact_policy_direct_scenarios
cargo test -p any-mcp --lib headless_artifact_traversal_direct_scenarios \
  -- --ignored --exact \
  server::headless_integration::headless_artifact_traversal_direct_scenarios
cargo test -p any-mcp --lib headless_artifact_alias_metadata_direct_scenarios \
  -- --ignored --exact \
  server::headless_integration::headless_artifact_alias_metadata_direct_scenarios
cargo test -p any-mcp --lib headless_artifact_bounded_metadata_direct_scenarios \
  -- --ignored --exact \
  server::headless_integration::headless_artifact_bounded_metadata_direct_scenarios
cargo test -p any-mcp --lib headless_artifact_dynamic_filesystem_direct_scenarios \
  -- --ignored --exact \
  server::headless_integration::headless_artifact_dynamic_filesystem_direct_scenarios
cargo test -p any-mcp --features acceptance-harness --test headless_stdio_e2e \
  headless_artifact_ -- --ignored --test-threads=1
```

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
content removed. The job records the opened regular file's device, inode,
length, and bounded trailing anchor before testing. The body audit accepts only
allow-listed events appended after that offset when the identity and anchor are
unchanged. On failure, CI validates at most 64 KiB from that fresh reviewed
window and retains only fixed validity categories and event counters in a
mode-0600 artifact for seven days; it never uploads raw server-log bytes. Each
live driver runs in a unique transient user scope with
an exact-unit cleanup trap and a manager-enforced runtime ceiling, so runners
must provide an available systemd user manager. Protect the `anytype-headless`
environment so untrusted code cannot reach the self-hosted runner or
credentials. Both live jobs run only after the hosted contract matrix on a
manual dispatch or a push to `main`; pull requests and tag pushes run the
hosted offline inventory only. The clean-server job first invokes an
operator-owned absolute reset script, then runs the same three explicit
targets.

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
cargo build -p anyr
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
[stdio protocol verification](docs/STDIO_CONFORMANCE.md) for the exact
compatibility claim and reproducible client commands.

## Streamable HTTP transport

Stdio is the default. `ANY_MCP_TRANSPORT=streamable-http` selects an
authenticated loopback Streamable HTTP listener instead; one process never
serves both. The HTTP transport exposes the same tools, schemas, structured
results, profiles, read-only rules, and optional toolsets as stdio — the
domain handlers are shared, and handlers never observe raw headers or bearer
values.

The listener serves one fixed `/mcp` endpoint and binds loopback only
(default `127.0.0.1:8000`); a non-loopback bind is rejected. Remote
deployments terminate TLS in a same-host reverse proxy that forwards to
loopback, preserves the `Authorization`, `Origin`, `Host`, MCP session and
version, and `Last-Event-ID` headers, disables SSE buffering, and never logs
credentials. Forwarded-identity headers (`X-Forwarded-*`, `Forwarded`) are
ignored and stripped.

Every request passes fixed gates in order — exact `Host` allowlist, exact
`Origin` allowlist with fail-closed CORS, a process-global request-rate
window, bearer authentication, a 64-request concurrency bound, and 2 MiB
bounded body collection — before any JSON decoding or handler work.
Authentication is required on every request and is separate from the Anytype
keystore:

- `ANY_MCP_HTTP_AUTH=static-token` reads one 43..512-byte base64url token
  from the owner-only regular file named by `ANY_MCP_HTTP_TOKEN_FILE` and
  compares it in constant time. Intended for a single local operator or
  debugger.
- `ANY_MCP_HTTP_AUTH=oauth-resource-server` implements the MCP
  protected-resource role for one configured external issuer: RFC 9728
  metadata at the two fixed well-known paths, JWT access tokens validated
  against a bounded, cached JWKS (`RS256`/`ES256`/`EdDSA` only), and exact
  issuer, audience, expiry, subject, and scope checks. Requires
  `ANY_MCP_HTTP_RESOURCE_URI`, `ANY_MCP_HTTP_ISSUER`,
  `ANY_MCP_HTTP_AUTHORIZATION_SERVER`, `ANY_MCP_HTTP_JWKS_URI`, and
  `ANY_MCP_HTTP_AUDIENCE`; `ANY_MCP_HTTP_REQUIRED_SCOPE` defaults to
  `anytype.mcp`.

Stable mode accepts protocol revisions `2025-03-26`, `2025-06-18`, and
`2025-11-25` over stateful sessions with optional SSE; revision `2024-11-05`
remains stdio-only. Sessions are bound to the authenticated principal —
another principal presenting a stolen session ID observes exactly an unknown
session — and the process admits at most `ANY_MCP_HTTP_MAX_SESSIONS`
(default 32) concurrent sessions. Mutation idempotency is process-lifetime
and partitioned by principal, so a client that loses its session can safely
retry an uncertain create after re-initializing. With
`ANY_MCP_PROTOCOL=experimental-2026-07-28`, `/mcp` instead serves the
stateless preview: one bounded POST per request returning
`application/json`, with GET and DELETE rejected.

Browser clients require `ANY_MCP_HTTP_ALLOWED_ORIGINS` (exact serialized
origins; absent means every Origin-bearing request is rejected) and must use
a fetch-based SSE reader because native `EventSource` cannot attach the
`Authorization` header. Cookies and query-string tokens are never accepted.

Remaining settings: `ANY_MCP_HTTP_BIND`, `ANY_MCP_HTTP_ALLOWED_HOSTS`
(exact authorities, default local names), `ANY_MCP_HTTP_REQUESTS_PER_MINUTE`
(default 120), and `ANY_MCP_HTTP_SHUTDOWN_SECS` (drain deadline, default
10). Configuration is validated before any Anytype credential access;
diagnostics never echo configured values, tokens, session IDs, or bodies.
These authentication, listener, proxy, and admission requirements are part
of the supported operator contract.

## License

Apache License, Version 2.0
