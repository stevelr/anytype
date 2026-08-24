+++
title = "Connections and gRPC"
weight = 10
+++

# Choose endpoints and credentials

Anytype Toolbox uses HTTP for the public REST API and gRPC for capabilities the
REST API does not expose. The transports authenticate separately.

## Limit model-visible data

An Anytype connection can read from the spaces available to its account. A
desktop account may already belong to personal, team, and family spaces, so
connecting an agent to that account can make those spaces available to its
tools. Installing a skill alone does not connect to Anytype or grant this
access.

For a smaller data boundary, provision a dedicated Anytype CLI account in an
isolated headless profile. Create a purpose-built sharing space in the desktop
app, invite only the headless account, and move or copy selected content into
that space. Running a headless server with the desktop account does not reduce
the account's existing space memberships.

Choose Reader for read workflows and Writer when the workflow must change
content. Owner is unnecessary for ordinary agent work. These roles apply to
the entire space. Anytype does not provide per-object permissions within a
space, so treat every object and file in the sharing space as available to the
connected agent according to its role. MCP read-only mode limits mutations but
does not hide content.

Keep the invitation link out of chat and logs. Supply it locally during setup:

```sh
anyr init-cli --join "$INVITE_LINK"
```

Use a dedicated operating-system account, home directory, or equivalent
Anytype CLI profile so initialization cannot reuse the desktop account's
configuration. Do not use `--force` in a profile containing an account you
want to preserve.

## Endpoint defaults

### `ANYTYPE_URL` or `--url URL`

Selects the HTTP endpoint. It defaults to `http://127.0.0.1:31012` when the
selected keystore already contains gRPC credentials, and to
`http://127.0.0.1:31009` otherwise.

### `ANYTYPE_GRPC_ENDPOINT` or `--grpc URL`

Selects the gRPC endpoint. It defaults to `http://127.0.0.1:31010`.

### `ANYTYPE_KEYSTORE` or `--keystore SPEC`

Selects the optional credential store. See [Keystores](/reference/keystores/).

### `ANYTYPE_KEYSTORE_SERVICE` or `--keystore-service NAME`

Selects the credential namespace. It defaults to `anyr`.

Command-line endpoint values take precedence over environment values. HTTP
tokens are specific to their endpoint, so changing `ANYTYPE_URL` may require a
new login.

The desktop app normally provides HTTP on `127.0.0.1:31009`. The Anytype CLI
headless server provides gRPC on `127.0.0.1:31010` and HTTP on
`127.0.0.1:31012`.

## Select an MCP connection mode

`anyr mcp` resolves one connection identity before constructing its client.
Set `ANY_MCP_CONNECTION_MODE=desktop` (the default) for the desktop HTTP API,
or `ANY_MCP_CONNECTION_MODE=headless` for the paired Anytype CLI HTTP and gRPC
services. Saved credentials never select the mode implicitly.

Desktop mode uses `ANYTYPE_URL` or `http://127.0.0.1:31009` and rejects an
explicit `ANYTYPE_GRPC_ENDPOINT`. This does not delete stored gRPC credentials.
Headless mode uses the defaults `http://127.0.0.1:31012` and
`http://127.0.0.1:31010`. If either endpoint is customized, configure both and
use the same host for each so the server cannot accidentally combine a desktop
HTTP API with another process's gRPC service.

MCP startup checks HTTP only. The static catalog stays available if gRPC is
stopped or unauthenticated, and HTTP-only tools continue normally. The server
checks gRPC once, immediately before dispatch, only for these tools:

- `object_archive`;
- every `body-blocks` tool;
- `type_update`; and
- `collection_member_list`, `collection_member_add`, and
  `collection_member_remove`.

`server_status` does not perform another network check. It reports whether
gRPC is configured and the last observation made by a gRPC-backed invocation.
Restart the MCP process after changing endpoints or credentials.

## Authenticate for HTTP only

Keep the desktop app running, then use its four-digit login flow:

```sh
anyr auth login
anyr auth status --pretty
```

This provisions an HTTP token. Commands marked **gRPC backend required** need
the headless-server setup below as well.

## Authenticate for gRPC

Follow the [Anytype CLI installation instructions](https://github.com/anyproto/anytype-cli)
and start its headless server. Then initialize both credential families:

```sh
anyr init-cli
anyr auth status --pretty
```

`init-cli` reuses the Anytype CLI account configuration when possible, creates
a fresh HTTP token, stores HTTP and gRPC credentials, and verifies both
connections. Use `ANYTYPE_CLI_BIN=/absolute/path/to/anytype` when the Anytype
CLI executable is not on `PATH`.

If an existing Anytype CLI config does not expose its account key, run
`anyr auth set-grpc --help` for explicit credential inputs or use
`anyr init-cli --force` to create a separate account. The forced form does not
join existing spaces automatically.

## Check the selected connection

```sh
anyr auth status --pretty
```

The result reports credential presence and live pings for HTTP and gRPC
independently. A stored credential can still fail its ping when the selected
server is stopped, the endpoint is wrong, or the token belongs to another
endpoint.
