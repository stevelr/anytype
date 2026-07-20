# any-mcp

A bounded, workflow-oriented Model Context Protocol server for Anytype.

`any-mcp` is designed for reliable agent workflows such as discovering,
reading, and safely editing documents. It is intentionally not a one-for-one
mirror of the Anytype API.

## Phase 1 foundations

The current crate establishes the workspace, dependency, protocol, and shared
wire-contract boundaries for later runtime and handler work:

- [`rmcp`](https://docs.rs/rmcp/) 2.2.0 with the `server`, `macros`, `schemars`,
  and `transport-io` features;
- upcoming MCP protocol revision `2026-07-28`, selected explicitly to align with
  the SDK's imminent release/API direction;
- an `anytype-api`-only application dependency through the `anytype` crate;
  `any-mcp` never depends directly on generated `anytype-rpc` support; and
- reusable strict JSON Schema 2020-12 input/output contracts with
  `additionalProperties: false`, bounded domain strings, stable object
  summaries, and canonical
  `anytype://spaces/<space_id>/objects/<object_id>` resource URIs;
- exact annotation profiles for read, create, and destructive update tools;
- compact JSON text fallbacks matching each typed `structuredContent` result;
  and stable, bounded, secret-safe execution error bodies; and
- a minimal binary that constructs the server scaffold without writing to
  stdout.

The authenticated stdio runtime, tools, resources, operational controls, and
Anytype client lifecycle are added in subsequent Phase 1 work. Until then, the
binary exits after constructing the scaffold.

## Source layout

- `src/main.rs` — minimal binary entry point.
- `src/lib.rs` — shared crate surface for the binary and tests.
- `src/domain.rs` — bounded values, object summaries, and resource URIs.
- `src/schema.rs` — strict input/output schema generation.
- `src/protocol.rs` — tool contracts and annotation profiles.
- `src/result.rs` — structured results with compact JSON text fallbacks.
- `src/error.rs` — stable, redacted tool execution errors.
- `src/server.rs` — server identity, capabilities, and upcoming protocol
  declaration.

## Build

```sh
cargo build -p any-mcp
```

## Protocol channel

When stdio transport is enabled, stdout is reserved exclusively for MCP
protocol frames. Redacted diagnostics are emitted to stderr, and credentials
or full upstream response bodies must never be logged.

## License

Apache License, Version 2.0
