# Anytype Toolbox

Automate your Anytype workspace from the terminal, scripts, an external editor,
or an AI client.

Anytype Toolbox is a community project built around one command: `anyr`. It can
manage spaces and objects, work with files and chats, edit documents as
Markdown, export and restore spaces, and run a workflow-oriented MCP server.
Rust applications can use the supported API surface through the workspace's
client libraries.

[![release](https://img.shields.io/github/v/tag/stevelr/anytype?sort=semver&filter=anyr-v*&label=release)](https://github.com/stevelr/anytype/releases?q=anyr-v&expanded=true)
[![crates.io](https://img.shields.io/crates/v/anyr.svg)](https://crates.io/crates/anyr)

**[Documentation](https://docs.anytype-toolbox.org) ·
[Releases](https://github.com/stevelr/anytype/releases) ·
[Rust API](https://docs.rs/anytype)**

## Start with `anyr`

[Install `anyr`](./anyr/README.md#install), then choose the connection that
matches your Anytype installation.

With the Anytype desktop app running, authenticate interactively:

```sh
anyr auth login
anyr auth status --pretty
```

Desktop login provisions HTTP access. `auth status` reports HTTP and gRPC
credentials separately because some file, chat, invitation, backup, and MCP
operations require gRPC credentials. The [credential guide](./anyr/README.md#generating-and-saving-credentials) covers both.

For a running Anytype headless server, initialize and verify both credential
families together:

```sh
anyr init-cli
```

Confirm the connection and discover the command surface:

```sh
anyr space list --table
anyr --help
```

The [CLI guide](./anyr/README.md) covers endpoints, credential storage, output
formats, shell completions, and every command group.

## What you can do

| Goal                                                              | Entry point                                                                       |
| ----------------------------------------------------------------- | --------------------------------------------------------------------------------- |
| Manage spaces, objects, types, properties, files, and chats       | [`anyr` quick reference](https://docs.anytype-toolbox.org/cli/quick-reference/)   |
| Search and automate Anytype with JSON or table output             | [`anyr` CLI guide](./anyr/README.md)                                              |
| Edit Anytype documents in an external Markdown editor             | [`anyr md`](./any-edit/README.md)                                                 |
| Export a space, collection, type, or tagged selection as Markdown | [Markdown export guide](https://docs.anytype-toolbox.org/guides/export-markdown/) |
| Create, inspect, compare, and restore backups                     | [`anyr backup`](./anyback/README.md)                                              |
| Connect an AI client through the Model Context Protocol           | [`anyr mcp`](./any-mcp/README.md)                                                 |
| Build an Anytype integration in Rust                              | [`anytype` client library](./anytype-api/README.md)                               |

For example:

```sh
# List pages in a space.
anyr object list "Work" --type page --table

# Edit a document after configuring EDITOR or EDITOR_COMMAND.
anyr md edit "Work" OBJECT_ID

# Export a space as Markdown with files and property metadata.
anyr backup export \
  --space "Work" \
  --format markdown \
  --include-files \
  --include-properties \
  --dest ./work-markdown.zip
```

Commands write compact JSON to stdout by default, which keeps pipelines
predictable. Use `--pretty` for readable JSON or `--table` for terminal output.
Diagnostics and progress remain on stderr.

## Rust libraries and components

CLI users install `anyr`. The other workspace packages provide its libraries
and specialized integration surfaces:

| Package                                         | Purpose                                                                          |
| ----------------------------------------------- | -------------------------------------------------------------------------------- |
| [`anytype` Rust crate](./anytype-api/README.md) | High-level client spanning the supported REST API and selected gRPC capabilities |
| [`anytype-rpc`](./anytype-rpc/README.md)        | Generated low-level client for Anytype's gRPC interface                          |
| [`any-edit`](./any-edit/README.md)              | Markdown conversion and external-editor workflows used by `anyr md`              |
| [`anyback`](./anyback/README.md)                | Backup, restore, archive inspection, and Markdown export used by `anyr backup`   |
| [`any-mcp`](./any-mcp/README.md)                | Workflow-oriented MCP server used by `anyr mcp`                                  |

The `anytype` Rust crate is the recommended starting point for applications. It
prefers the supported REST API when REST provides equivalent behavior and uses
`anytype-rpc` for capabilities that require the gRPC interface.

## Build the workspace

The Nix development shell provides the pinned Rust toolchain and native build
dependencies:

```sh
nix develop
cargo build
cargo run -- --help
```

Cargo commands that do not select a package build `anyr`, the workspace's
default member. Use `--workspace` or `-p PACKAGE` to work with every package or
one library.

## License

Apache License 2.0. See [LICENSE-APACHE](./LICENSE-APACHE).
