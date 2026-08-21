# any-mcp

**[Anytype Toolbox documentation](https://docs.anytype-toolbox.org/) ·
[MCP setup and reference](https://docs.anytype-toolbox.org/guides/mcp/) ·
[Stdio conformance](docs/STDIO_CONFORMANCE.md)**

`any-mcp` is the workflow-oriented Model Context Protocol server library
embedded in `anyr mcp`. The package does not install an `any-mcp` executable.
The site guide owns client registration, runtime settings, and operator usage.

Status: prerelease. Portable tests cover Linux, macOS, and Windows; release
qualification is ongoing.

## Design boundary

The server exposes bounded workflows instead of mirroring the Anytype API one
endpoint at a time. Each tool has a closed input schema, finite work and
response limits, typed errors, and mutation-specific replay rules. One
`anytype-api` client owns credentials, rate limits, deadlines, and transport
selection for the process.

The default compact catalog covers common document discovery, reads, and exact
edits with four tools. The standard catalog adds the complete base workflow
set. Read-only mode omits mutation tools and rejects stale direct mutation
calls before upstream I/O.

Optional registries are linked at build time and selected at startup. Shipped
registries cover artifacts, typed body blocks, chats, files, members, schema,
and view writes. Selection cannot change after protocol startup. A registry is
advertised only when its complete contract and required backend are available.

The versioned [`any-mcp` skill](../skills/skills/any-mcp/SKILL.md) supplies
agent-facing tool selection and higher-level personal-knowledge workflows. It
is separate from the server protocol and catalog implementation.

## Runtime

`anyr mcp` serves either stdio or Streamable HTTP through the library. Stdio
reserves stdout for protocol frames; startup and redacted diagnostics use
stderr. The stable transport uses the released `2025-11-25` protocol. An
explicit experimental selector enables the stateless `2026-07-28` preview.

Startup validates configuration before authentication, then requires a healthy
HTTP connection. It checks gRPC when configured and whenever the selected
catalog requires it. Configured but unhealthy gRPC fails startup so a process
cannot advertise a catalog against the wrong backend.

Concurrency, request time, startup time, and buffered response sizes have
validated finite ceilings. Cancellation stops undispatched work. A mutation
that may have crossed its dispatch boundary returns indeterminate guidance and
is not replayed automatically.

## Tool contracts

Workflow handlers separate these stages:

1. Decode and validate a closed request.
2. Resolve bounded Anytype identities.
3. Apply access, read-only, work, and token budgets.
4. Dispatch each unsafe mutation at most once.
5. Verify fresh server state when the workflow contract promises verification.
6. Return a structured result or a secret-safe error classification.

List workflows use bounded pagination and opaque continuation state. Document
workflows limit both upstream payloads and model-visible output. Catalog
metadata reports which backend and optional registry a workflow needs without
exposing credentials or endpoint details.

## Artifact data plane

The default-off artifact registry keeps large file and document payloads out of
MCP frames. MCP carries logical roots, relative paths, opaque handles, hashes,
sizes, and receipts. Bytes move through operator-authorized local roots or a
loopback staging service.

A strict, versioned TOML policy grants import roots, create-new export roots,
optional staging, Anytype space access, and finite transfer limits. The policy
does not contain credential values. Invalid schema fields, unsafe permissions,
linked configuration files, duplicate logical names, and unsupported versions
fail before protocol output.

Local roots are retained as filesystem capabilities. Traversal rejects parent
components, links and reparse points, cross-filesystem redirection, unsafe
permissions, hard-linked imports, over-limit files, and export collisions.
Exports remain in an owner-private temporary file until an atomic create-new
publication. Failed or cancelled operations remove only their own temporary
state.

Stable stdio clients that advertise MCP roots can narrow the configured root
policy for the session. Client roots cannot add authority. An invalid or
unavailable roots snapshot disables local-root operations for that session.

Remote clients use the optional staging service. It binds loopback and expects
a separately managed TLS reverse proxy. Upload and download handles are opaque,
finite-lived capabilities. Handles do not appear in URLs, and the server does
not fetch caller-supplied URLs.

## Security properties

- Credentials come from `anytype-api` keystores or inherited environment
  secrets. MCP input cannot set or retrieve them.
- Tool schemas reject unknown fields and oversized inputs before backend I/O.
- Read-only mode removes mutation tools and enforces the boundary again during
  dispatch.
- Diagnostics omit credentials, bodies, physical artifact paths, handles, and
  full upstream responses.
- HTTP authentication, sessions, body collection, request concurrency, and
  graceful drain all have finite resource boundaries.
- Artifact idempotency records distinguish definitive pre-dispatch failures
  from uncertain publication or mutation outcomes.

These controls bound the server process. The Anytype account, selected spaces,
filesystem policy, MCP host, reverse proxy, and operating-system sandbox remain
operator-controlled trust boundaries.

## Protocol verification

[`docs/STDIO_CONFORMANCE.md`](docs/STDIO_CONFORMANCE.md) records the stable and
preview stdio revisions exercised with Codex, Claude Code, and MCP Inspector.
The test suite also drives stable and preview Streamable HTTP over loopback,
including authentication, sessions, event-stream resumption, cancellation,
slow readers, shutdown, and protocol framing. Spawned stdio cancellation tests
observe upstream closure before a one-permit follow-up read checks capacity.
Expected process exits are matched by their fixed failure category while
diagnostic metadata remains available for timeout investigation.

Schema snapshots keep tool names, descriptions, input schemas, annotations,
resources, and catalog composition reviewable. Optional registries have direct
router tests and spawned-process coverage so protocol behavior is checked at
the same boundary clients use.

## Development

Run the portable checks from the workspace root:

```sh
cargo test --locked -p any-mcp
cargo clippy --locked -p any-mcp --all-targets
cargo fmt --all -- --check
```

Acceptance-only child binaries are gated by crate features and are not release
entry points. Live tests require a disposable Anytype account, reviewed
redacted server logs, and the admission variables documented by the test
harness. Ordinary tests use scripted upstreams and do not require credentials.

The official [`anytype-mcp`](https://github.com/anyproto/anytype-mcp) server is
a separate project. It favors direct OpenAPI coverage and its own distribution
and compatibility model.
