# Anytype Rust Tools and Clients

This repository is a rust workspace for Anytype automation, with client libraries and cli tools.

## Projects

<table>
<tr>
<td width="50%" valign="top">
<h3><a href="./anytype-api/">📦 anytype-api</a></h3>
<p>An ergonomic Anytype API client in Rust</p>
</td>
<td width="50%" valign="top">
<h3><a href="./anyr/">⌨️ anyr</a></h3>
<p>List, search, and manipulate Anytype objects from the command line</p>
</td>
</tr>
<tr>
<td width="50%" valign="top">
<h3><a href="./any-edit/">✏️ any-edit</a></h3>
<p>Edit Anytype documents in an external editor</p>
</td>
<td width="50%" valign="top">
<h3><a href="./anytype-rpc/">🔌 anytype-rpc</a></h3>
<p>Experimental Rust gRPC client for Anytype</p>
</td>
</tr>
<tr>
<td width="50%" valign="top">
<h3><a href="./any-mcp/">🔗 any-mcp</a></h3>
<p>Bounded, workflow-oriented MCP server for Anytype</p>
</td>
<td width="50%" valign="top">
<h3><a href="./anyback/">💾 anyback</a></h3>
<p>Backup, restore, and inspect Anytype spaces</p>
</td>
</tr>
</table>

## Build and run

The workspace keeps private test executables alongside its libraries, but
selects `anyr` for Cargo commands that do not name a package. Run the
user-facing CLI from the repository root:

```sh
cargo run -- -h
```

Use `--workspace` or `-p PACKAGE` when you need to build or test other
workspace members.

The Nix development shell supplies the Rust version pinned by
`rust-toolchain.toml`, protobuf, `just`, `jq`, a C compiler, `gate`, and Python
3.14:

```sh
nix develop
```

GitHub Actions workflows are manual-only while the expanded matrix is being
qualified. The CI and build workflows offer platform selectors for Linux
x86_64 and arm64, macOS arm64, and Windows x86_64 and arm64; CI also has a
native Arch Linux lane. Nix drives Linux and macOS builds, Windows builds run
natively, and Linux builds can produce loadable x86_64 or arm64 OCI image
archives. The release-artifact workflow generates shell and PowerShell
installers and Homebrew formulae, then validates the POSIX artifacts on both
Linux and macOS without publishing a release.

## Compatibility notes

- [Numeric and checkbox filter status](FILTER_STATUS.md) records the
  supported condition, value-encoding, and endpoint matrix plus the disposition
  of the historical upstream parsing bug.
