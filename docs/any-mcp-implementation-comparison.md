# Anytype MCP implementation comparison

Snapshot: 2026-07-21. This compares this workspace's work-in-progress
[`any-mcp`](../any-mcp/README.md) with the official
[`anyproto/anytype-mcp`](https://github.com/anyproto/anytype-mcp) repository at
release 1.2.9 (`21ef5c0`, 2026-06-29). The upstream server loads the running
Anytype instance's OpenAPI document at startup, so its exact live tool catalog
can differ from the 34 non-authentication operations in its checked-in
specification.

## Summary

The projects optimize for different things. Upstream `anytype-mcp` is a thin,
official OpenAPI-to-MCP adapter: it makes a broad REST surface available with
little hand-written MCP code and is easy to install from npm. Our `any-mcp` is
a curated workflow server: it accepts a smaller catalog and more implementation
work in exchange for stable, bounded contracts, safer agent-oriented mutations,
optional capabilities, MCP resources, and access to capabilities exposed by
`anytype-api` over either REST or selected gRPC paths.

Consequently, upstream is the better choice for immediate breadth and low-friction
installation. Our implementation is the stronger foundation when predictable
model context, mutation safety, secret handling, cross-transport Anytype
coverage, and testable workflow semantics matter more than exposing every REST
operation immediately.

## Implementation trade-offs

| Area | This workspace: `any-mcp` | Upstream: `anyproto/anytype-mcp` |
| --- | --- | --- |
| Anytype boundary | Depends only on the typed Rust `anytype-api` crate. `anytype-api` can hide REST/gRPC selection and richer resolution behind one application boundary; `any-mcp` does not depend directly on `anytype-rpc`. | Fetches `/docs/openapi.json`, converts every non-auth operation into a tool, and invokes it with `openapi-client-axios`. Its ceiling is the API published in that OpenAPI document. |
| MCP surface | Hand-designed workflows: 4 compact or 14 standard Phase 1 tools, optional read-only mode, strict schemas, structured results, opaque cursors, and one bounded document resource template. | A direct operation catalog. The checked-in spec yields 34 tools across search, spaces, lists, members, objects, properties, tags, types, and templates. Tool calls return JSON serialized as text; the converter calculates output schemas, but the server does not advertise them or return `structuredContent`. |
| Catalog stability | Startup-selected profiles and toolsets keep the default schema/token cost stable. Dispatch, discovery, read-only policy, stable MCP, and the preview protocol use the same typed contracts. | Automatically tracks the server's OpenAPI surface, which is convenient but means names, schemas, tool count, and context cost can change with the connected Anytype version. All converted operations are exposed together. |
| Mutation semantics | Workflow-specific verification, idempotency where useful, body-hash conflict checks, exact-match editing, bounded readback, explicit destructive annotations, and fail-closed ambiguity handling. | Mostly forwards one HTTP operation. This is simple and gives direct API fidelity, but does not add cross-call conflict protection, independent readback, workflow idempotency, or agent-specific safety policy beyond the OpenAPI schema. |
| Bounds and errors | Enforces concurrency, request and startup timeouts, frame and response-byte limits, pagination limits, schema bounds, cancellation, secret-safe diagnostics, and stable typed MCP errors with `isError`. | Relies mainly on the generated OpenAPI schema and HTTP client. There is no comparable request concurrency, response-size, pagination, or operation-timeout policy. HTTP errors are serialized into ordinary text content rather than MCP `isError`. |
| Credentials and diagnostics | Reuses the `anyr` keystore/environment configuration; credentials need not appear in MCP configuration. Stdout is protocol-only and diagnostics are deliberately redacted. | npm setup and the `get-key` helper are convenient, but normal examples embed the bearer token in a JSON-valued environment variable. Current diagnostics log call parameters, request bodies, the operation lookup, and raw HTTP errors to stderr, which is a weaker redaction posture. |
| Packaging and maturity | Rust workspace prerelease, built locally, with Phase 1 verification and release work still open. It is substantially more code and more expensive to evolve. | Official npm package with one-command `npx` setup, client install links, and a small generic implementation. It is released and easier for end users to adopt today. |
| Extensibility | Domain handlers can compose multiple Anytype calls and can use future MCP resources, notifications, transports, and tasks without pretending they are OpenAPI operations. New domains require explicit design, implementation, and tests. | New REST operations often appear automatically when OpenAPI changes. Non-REST behavior and multi-step workflows require custom code outside the converter's core model. |

### Pros and cons in brief

Our main advantages are bounded and stable model-facing contracts, safer
mutations, stronger credential and diagnostic hygiene, richer MCP features, and
the wider long-term capability ceiling of `anytype-api`. Its disadvantages are
the currently narrower tool catalog, local Rust build, greater maintenance
cost, and WIP status; the open Phase 1 correctness and headless-verification
tickets should be completed before presenting it as more production-ready than
the official package.

Upstream's main advantages are official distribution, simple setup, small code
size, broad REST CRUD coverage, and automatic alignment with a running server's
OpenAPI document. Its disadvantages follow from the same generic design: a
large all-at-once catalog, unstable context cost, text-only results, limited
workflow semantics, no MCP resource surface, and no route to capabilities that
Anytype does not publish in OpenAPI.

## Phase 2: optional domain toolsets

Open Phase 2 tickets cover the selector/registry foundation plus schema,
members, views-write, narrow admin, files, and chats toolsets, followed by an
isolation and end-to-end verification gate (`any-x90` through `any-48h`, plus
the split review and selector tickets).

- **Schema:** upstream is ahead on availability. Its checked-in catalog already
  exposes create/get/update/delete/list for types, properties, and tags. Our
  Phase 2 schema toolset should not merely duplicate those calls: its value is
  reviewed replacement semantics, resolver behavior, bounded schemas and
  pagination, mutation verification, explicit annotations, and a catalog that
  is absent unless selected.
- **Members:** upstream already exposes list/get members. Our planned toolset is
  intentionally read-only and opt-in, with explicit privacy boundaries,
  bounded pagination, consistent space resolution, and read-only enforcement.
  Upstream wins on immediate utility; ours aims for a clearer permission and
  disclosure contract.
- **Views and lists:** upstream already lists views and objects and can add or
  remove list objects. Our views-write work can compose those primitives with
  stable selectors, checked pagination, verification, and destructive/update
  annotations. Again, the benefit is workflow safety rather than endpoint
  novelty.
- **Narrow admin:** upstream directly exposes space creation/update and object
  deletion, but its checked-in catalog has no archived-object administration.
  Our design explicitly excludes permanent purge and requires an include,
  defer, or omit decision for every candidate operation. This is less broad but
  safer for an agent-facing admin surface.
- **Files:** upstream contains generic multipart upload support, so a file
  operation published by a live OpenAPI document can become a tool
  automatically; files are absent from its checked-in 34-operation spec. Its
  converter accepts absolute host paths without the root/allowlist, byte-limit,
  deletion-policy, or resource-delivery design required by our Phase 2 file
  tickets. Our approach is slower but has a materially stronger filesystem
  trust boundary and can incorporate richer `anytype-api` file capabilities.
- **Chats:** chats are absent from upstream's checked-in catalog. It can expose
  future REST chat operations generically, but not richer gRPC-only forms or a
  designed streaming/reconnect workflow. Our tickets begin with bounded
  REST-first plain messages and require concrete justification before adding
  streaming, attachments, rich blocks, or reactions.

The Phase 2 terminal ticket is an important differentiator: it requires
disabled toolsets to be non-invocable, `tools/list` and `server_status` to agree,
stable and preview protocols to share contracts, default Phase 1 snapshots to
remain unchanged, and every optional tool/resource to own mock and real-headless
scenarios. Upstream's dynamic all-operation catalog has no equivalent isolation
contract, but also avoids the selector machinery entirely.

## Phase 3: resource subscriptions and update notifications

Phase 3 (`any-x4b`, `any-mna`, `any-eti`) is conditional on finding a dependable
Anytype change source, then designs and implements MCP resource subscriptions
and `notifications/resources/updated` with ordering, deduplication, reconnect,
backpressure, cancellation, and lifetime rules.

Upstream advertises only MCP tools and maps one request to one HTTP operation;
it has no resources, subscriptions, or update notifications. An OpenAPI
converter cannot derive a trustworthy subscription lifecycle from ordinary
CRUD endpoints. Our `anytype-api` boundary, including selected gRPC
capabilities, gives this phase a plausible route, but the ticket correctly says
to stop if no complete change source exists. Until that proof, this is an
architectural option rather than a present advantage.

## Phase 4: authenticated Streamable HTTP transport

Phase 4 (`any-5v6`, `any-01c`, `any-ucd`) adds a separately authenticated
Streamable HTTP transport with localhost-default binding, Origin validation,
per-request authorization, session/concurrency limits, audit logging, safe
shutdown, and protection of upstream Anytype credentials.

Both implementations currently ship stdio entry points. Upstream's proxy can
accept a generic SDK transport internally, but its CLI wires only stdio and
does not define a remote-server trust model. Our phase is broader and more
security-conscious than simply attaching an HTTP transport. The cost is three
design/review/implementation gates for a feature many local desktop users do
not need.

## Phase 5: MCP tasks

Phase 5 (`any-sf6`, `any-yif`, `any-uwa`) is deliberately conditional: MCP tasks
are added only if a genuinely long-running, cancellable Anytype workflow is
proven and target clients support it. The design must cover progress/results,
persistence or session behavior, cancellation, cleanup, and ordinary-tool
fallback.

Upstream has no MCP task lifecycle; each generated tool waits for one HTTP
operation. Its generic approach is preferable for normal CRUD because it keeps
the implementation small. Our planned task support would be superior for a
proven export, import, sync, or other long operation, particularly if that
workflow needs non-OpenAPI capabilities, but the review ticket's ability to
recommend no implementation is the right guard against speculative protocol
complexity.

## Recommendation

Treat the implementations as complementary rather than attempting feature-for-
feature parity. Use upstream as the reference for which REST operations users
expect to work immediately and for distribution ergonomics. Preserve our
workflow-oriented design where it provides a concrete benefit: stable optional
catalogs, bounded typed results, verified mutations, resources and
notifications, stronger secret handling, or access beyond OpenAPI. A generated
REST tool should not be reimplemented locally unless the curated workflow can
state and test what safety, composition, or capability it adds.

## Sources

- Local design: [`docs/anytype-mcp-design.md`](anytype-mcp-design.md)
- Local implementation status: [`any-mcp/README.md`](../any-mcp/README.md)
- Local roadmap: open `br` tickets labelled `mcp` and `phase-2` through
  `phase-5`, inspected on 2026-07-21
- Upstream overview and setup:
  [`README.md`](https://github.com/anyproto/anytype-mcp/blob/21ef5c01144fc1d64482ab9f8f67ca555d210896/README.md)
- Upstream OpenAPI conversion:
  [`src/openapi/parser.ts`](https://github.com/anyproto/anytype-mcp/blob/21ef5c01144fc1d64482ab9f8f67ca555d210896/src/openapi/parser.ts)
- Upstream MCP dispatch and result handling:
  [`src/mcp/proxy.ts`](https://github.com/anyproto/anytype-mcp/blob/21ef5c01144fc1d64482ab9f8f67ca555d210896/src/mcp/proxy.ts)
- Upstream HTTP invocation and upload behavior:
  [`src/client/http-client.ts`](https://github.com/anyproto/anytype-mcp/blob/21ef5c01144fc1d64482ab9f8f67ca555d210896/src/client/http-client.ts)
