+++
title = "Install and connect"
weight = 10
+++

# Install and connect

`anyr` is the user-facing command for Anytype Toolbox. Install it, connect one
Anytype environment, and confirm the HTTP and gRPC credential families before
choosing a workflow.

You need either a running Anytype desktop app or a separately installed,
running Anytype headless CLI server. Anytype Toolbox connects to that existing
environment; it does not install or start Anytype itself.

## Install `anyr`

### macOS with Homebrew

```sh
brew install stevelr/tap/anyr
```

### Linux

Download the archive for your architecture and its checksum from the
[GitHub releases](https://github.com/stevelr/anytype/releases) page. Verify the
download against the published checksum, then extract `anyr` to a directory on
`PATH`.

### Windows PowerShell

Download the Windows archive and its checksum from the
[GitHub releases](https://github.com/stevelr/anytype/releases) page. Verify the
download against the published checksum, then place `anyr.exe` in a directory
on `PATH`.

## Connect to the desktop app

Keep the Anytype desktop app running while you authenticate:

```sh
anyr auth login
anyr auth status --pretty
```

The desktop login provisions an HTTP token. `auth status` reports HTTP and gRPC
credentials independently because commands use the transport that provides the
required capability. File, chat, invitation, backup, and MCP workflows may also
require gRPC credentials. See the
[credential guide](https://github.com/stevelr/anytype/blob/main/anyr/README.md)
for the full configuration reference, and run `anyr auth set-grpc --help` for
the accepted, explicit credential sources.

## Connect to a headless server

This site assumes the separate Anytype headless CLI is already installed. Start
that server, then run:

```sh
anyr init-cli
```

`init-cli` reuses the server account when its config is present, creates a fresh
HTTP token, stores both credential families, and verifies authenticated HTTP and
gRPC access. Use `ANYTYPE_CLI_BIN` when the `anytype` executable is not on
`PATH`.

## Confirm the connection

```sh
anyr auth status --pretty
anyr space list --table
```

The status result distinguishes missing credentials from a failed live ping.
Continue with the [CLI quick reference](/cli/quick-reference/) after both
transports required by your workflow report healthy access.
