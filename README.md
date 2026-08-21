# Anytype Toolbox

Automate Anytype with anyr CLI, external editor, or MCP server.

The unified binary `anyr` can manage spaces and objects, work with files and chats,
edit documents in Markdown, export and restore spaces, and run a workflow-oriented MCP server.

The rust client library [`anytype`](https://crates.io/crates/anytype) provides an ergonomic Rust
interface for Anytype's HTTP/REST API and (optionally) the gRPC API for additional capabilities.

**[Installation](https://github.com/stevelr/anytype/releases)** | **[Documentation](https://docs.anytype-toolbox.org)** | **[Source](https://github.com/stevelr/anytype)**

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
operations require gRPC credentials. Most MCP tools work with the desktop HTTP
API; its [connection guide](https://docs.anytype-toolbox.org/reference/connections/)
lists the headless-only tools and covers both credential families.

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
| Guide an AI agent through Anytype workflows                       | [Anytype Toolbox Skills](./skills/README.md)                                      |
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

## Agent Skills

[Anytype Toolbox Skills](./skills/README.md) is an independently versioned,
community-maintained bundle for Codex, Claude Code, and other Agent Skills
hosts. Install the `anyr` skill for direct CLI workflows or `any-mcp` for an AI
host already connected to `anyr mcp`. The bundle is not affiliated with or
endorsed by Anytype.

List the available skills before installing:

```sh
npx skills add stevelr/anytype --list
```

The skills guide covers individual and combined installs, exact release
archives, host marketplaces, prerequisites, updates, removal, and package
verification.

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
