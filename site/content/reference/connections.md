+++
title = "Connections and gRPC"
weight = 10
+++

# Choose endpoints and credentials

Anytype Toolbox uses HTTP for the public REST API and gRPC for capabilities the
REST API does not expose. The transports authenticate separately.

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
