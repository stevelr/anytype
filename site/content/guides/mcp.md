+++
title = "MCP server"
weight = 30
+++

# Connect an MCP client to Anytype

`anyr mcp` runs the Anytype Toolbox Model Context Protocol server. It uses the
same endpoints and credentials as `anyr`; it does not log in or print
credentials.

The server is prerelease software. Start with the compact, read-only catalog,
then enable mutations or optional toolsets after checking the advertised tools.

## Before you register the server

Install `anyr`, authenticate, and confirm the connection:

```sh
anyr auth status --pretty
command -v anyr
```

The second command prints the executable path on POSIX shells. MCP clients
usually require an absolute command path. On Windows, locate `anyr.exe` and use
an absolute path accepted by the client's JSON or TOML parser.

Most tools use HTTP. A fixed subset requires the headless gRPC backend, but
missing credentials or a stopped gRPC service does not prevent the MCP server
from starting or block unrelated HTTP tools. See
[Connections and gRPC](/reference/connections/) for the exact boundary.

## Register a stdio client

Use this JSON shape in clients that accept an `mcpServers` object:

```json
{
  "mcpServers": {
    "anytype": {
      "command": "/absolute/path/to/anyr",
      "args": ["mcp"],
      "env": {
        "ANY_MCP_CONNECTION_MODE": "desktop",
        "ANY_MCP_PROFILE": "compact",
        "ANY_MCP_READ_ONLY": "1"
      }
    }
  }
}
```

Codex uses the equivalent TOML entry:

```toml
[mcp_servers.anytype]
command = "/absolute/path/to/anyr"
args = ["mcp"]
env = { ANY_MCP_CONNECTION_MODE = "desktop", ANY_MCP_PROFILE = "compact", ANY_MCP_READ_ONLY = "1" }
env_vars = [
  "ANYTYPE_URL",
  "ANYTYPE_GRPC_ENDPOINT",
  "ANYTYPE_KEYSTORE",
  "ANYTYPE_KEYSTORE_SERVICE",
]
```

Forward non-secret selectors from the host environment. When
`ANYTYPE_KEYSTORE=env`, provide credential variables through the client's
secret facility or inherited process environment. Do not put credential values
in prompts, tool arguments, or committed client configuration.

## Serve Streamable HTTP

Stdio is the default. Streamable HTTP is an explicit, authenticated loopback
mode for clients that cannot start a local process:

```sh
umask 077
openssl rand -base64 48 | tr '+/' '-_' | tr -d '=\n' > /absolute/path/to/mcp-token

ANY_MCP_TRANSPORT=streamable-http \
ANY_MCP_HTTP_AUTH=static-token \
ANY_MCP_HTTP_TOKEN_FILE=/absolute/path/to/mcp-token \
anyr mcp
```

The listener defaults to `127.0.0.1:8000` and cannot bind a non-loopback
address. Send `Authorization: Bearer TOKEN` with requests to `/mcp`. The token
file must contain one 43 to 512 byte base64url token; on Unix it must belong to
the current user and grant no group or other access.

Use `ANY_MCP_HTTP_BIND` to select another loopback socket. Browser clients
also need an exact, comma-separated `ANY_MCP_HTTP_ALLOWED_ORIGINS` list. The
default host allowlist is `localhost`, `127.0.0.1`, and `::1`; replace it with
`ANY_MCP_HTTP_ALLOWED_HOSTS` when the client sends another `Host` authority.

OAuth deployments use `ANY_MCP_HTTP_AUTH=oauth-resource-server` and must set
`ANY_MCP_HTTP_RESOURCE_URI`, `ANY_MCP_HTTP_ISSUER`,
`ANY_MCP_HTTP_AUTHORIZATION_SERVER`, `ANY_MCP_HTTP_JWKS_URI`, and
`ANY_MCP_HTTP_AUDIENCE`. The resource URI must be HTTPS and end in `/mcp`; the
authorization server must equal the issuer in this release. The required scope
defaults to `anytype.mcp` and can be changed with
`ANY_MCP_HTTP_REQUIRED_SCOPE`.

HTTP-only variables are rejected unless `ANY_MCP_TRANSPORT=streamable-http`.
The listener is deliberately local; put a separately managed TLS reverse proxy
in front of it for remote access.

## Catalog and runtime settings

| Variable | Values and default |
| --- | --- |
| `ANY_MCP_CONNECTION_MODE` | `desktop` (default) or `headless`; selects one coherent endpoint pair |
| `ANY_MCP_PROFILE` | `compact` (default) or `standard` |
| `ANY_MCP_READ_ONLY` | `0` (default) or `1`; read-only omits mutation tools |
| `ANY_MCP_PROTOCOL` | `stable` (default) or `experimental-2026-07-28` |
| `ANY_MCP_TOOLSETS` | Comma-separated optional toolsets; absent by default |
| `ANY_MCP_MAX_CONCURRENCY` | `1..=64`; default `8` |
| `ANY_MCP_REQUEST_TIMEOUT_SECS` | `1..=300`; default `30` |
| `ANY_MCP_STARTUP_TIMEOUT_SECS` | `1..=120`; default `15` |
| `ANY_MCP_JSON_RESPONSE_BYTES` | Ordinary JSON response limit; default 8 MiB |
| `ANY_MCP_DOCUMENT_RESPONSE_BYTES` | Document response limit; default 64 MiB |

The linked optional toolsets are `artifacts`, `body-blocks`, `chats`, `files`,
`members`, `schema`, and `views-write`. Selection is exact and
comma-separated. The catalog remains advertised when gRPC is unavailable.
Each gRPC-backed invocation receives one bounded admission check before its
handler runs; unrelated HTTP calls never perform that check.

## Configuration file

Create and validate the strict `any-mcp.toml` policy:

```sh
anyr mcp init
anyr mcp check
```

Both commands accept `--config FILE`. Server startup accepts the same option,
or `ANY_MCP_CONFIG`. Without either selector, an `any-mcp.toml` in the current
directory is used when present.

The policy controls permitted Anytype spaces and the default-off artifact data
plane. A minimal local artifact policy is:

```toml
schema_version = 1

[spaces]
read_only = false
allowed = ["Personal"]

[[roots.import]]
id = "inbox"
path = "/absolute/operator-owned/import"

[[roots.export]]
id = "outbox"
path = "/absolute/operator-owned/export"
```

Then select the registry and policy in the MCP client environment:

```text
ANY_MCP_TOOLSETS=artifacts
ANY_MCP_CONFIG=/absolute/path/to/any-mcp.toml
```

Import roots allow existing-file reads. Export roots allow create-new writes;
they do not grant overwrite access. Tools receive a logical root ID and a
relative path. Absolute paths, parent traversal, links, and unlisted roots are
rejected.

For a remote MCP client, configure the policy's loopback staging service and
place a separately managed TLS reverse proxy in front of it. The MCP server
does not bind a public staging listener or fetch caller-supplied URLs.

## Check a running server

Call `server_status` after the client connects. It reports whether gRPC is
configured, the last observed gRPC state, the selected profile, read-only mode,
and optional catalogs without probing a backend or returning credentials.
`never` means that no gRPC-backed tool has needed an admission check in this
process. When artifacts are enabled, call
`artifact_status` to check effective root and staging authority.

Startup failures are written to stderr before protocol output. Check these in
order:

1. `ANY_MCP_CONNECTION_MODE` selects the intended desktop or headless backend.
2. `anyr auth status --pretty` reports the required transport as healthy.
3. The MCP client inherited the intended endpoint and keystore selectors.
4. `anyr mcp check --config FILE` accepts the selected policy.
5. The profile and optional toolset names use the exact values above.

Restart or reconnect the MCP process after changing connection selectors or
credentials. A gRPC-backed tool reports one stable, redacted category when it
cannot run: `grpc_not_configured`, `grpc_unavailable`, or `authentication`.
Do not clear saved credentials merely because a configured headless service is
temporarily stopped.

The crate's [stdio conformance record](https://github.com/stevelr/anytype/blob/main/any-mcp/docs/STDIO_CONFORMANCE.md)
lists the stable and preview protocol revisions tested with supported clients.
