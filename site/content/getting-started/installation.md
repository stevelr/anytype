+++
title = "Install and connect"
weight = 10
+++

# Install and connect

`anyr` is the user-facing command for Anytype Toolbox. Install it, connect one
Anytype environment, and confirm only the credentials required by the chosen
workflow. Desktop and most headless workflows use HTTP; gRPC credentials are
needed only for capabilities identified as headless-only.

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
required capability. File, chat, invitation, backup, and a fixed subset of MCP
operations may also require gRPC credentials; most MCP tools can use the
desktop HTTP API. See the [connection guide](/reference/connections/) for the
exact boundary, and run `anyr auth set-grpc --help` for the accepted, explicit
credential sources.

## Connect to a headless server

Install the separate
[Anytype headless CLI](https://github.com/anyproto/anytype-cli) when you need
its gRPC-backed capabilities. Start that server, then run:

```sh
anyr init-cli
```

`init-cli` reuses the server account when its config is present, creates a fresh
HTTP token, stores both credential families, and verifies authenticated HTTP and
gRPC access. Use `ANYTYPE_CLI_BIN` when the `anytype` executable is not on
`PATH`. If the CLI config exists but its account key lives in the OS keychain
(macOS, desktop Linux), either enter the key with
`anyr auth set-grpc --account-key` or run `anyr init-cli --force` to create a
new account.

Once gRPC credentials are stored, `anyr` commands default to the headless
server's HTTP port (`31012`) instead of the desktop app (`31009`); set
`ANYTYPE_URL` or `--url` to choose explicitly.

## Confirm the connection

```sh
anyr auth status --pretty
anyr space list --table
```

The status result distinguishes missing credentials from a failed live ping.
Continue with the [CLI quick reference](/cli/quick-reference/) after both
transports required by your workflow report healthy access.
