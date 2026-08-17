+++
title = "Rust libraries"
weight = 10
+++

# Rust libraries

The workspace separates its user-facing command from reusable Rust libraries.
Start with the high-level `anytype` crate unless your application needs a lower
level or specialized surface.

| Package       | Purpose                                                                      | Documentation                                                                                          |
| ------------- | ---------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------ |
| `anytype`     | Fluent client spanning the supported REST API and selected gRPC capabilities | [Guide](https://github.com/stevelr/anytype/tree/main/anytype-api) · [API](https://docs.rs/anytype)     |
| `anytype-rpc` | Generated low-level client for Anytype's gRPC interface                      | [Guide](https://github.com/stevelr/anytype/tree/main/anytype-rpc) · [API](https://docs.rs/anytype-rpc) |
| `any-edit`    | Markdown conversion and external-editor workflows used by `anyr md`          | [Guide](https://github.com/stevelr/anytype/tree/main/any-edit)                                         |
| `anyback`     | Archive traversal and inspection used by `anyr backup`                       | [Guide](https://github.com/stevelr/anytype/tree/main/anyback) · [API](https://docs.rs/anyback)         |
| `any-mcp`     | Workflow-oriented MCP server used by `anyr mcp`                              | [Guide](https://github.com/stevelr/anytype/tree/main/any-mcp)                                          |

`anytype` prefers REST when it provides equivalent behavior. It uses
`anytype-rpc` for capabilities that REST does not expose or represents with
less fidelity.
