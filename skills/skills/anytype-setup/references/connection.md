# Connect `anyr` to Anytype

Install a released `anyr` binary from the
[Anytype Toolbox releases](https://github.com/stevelr/anytype/releases), use
`brew install stevelr/tap/anyr` on macOS, or use `cargo install anyr` when a
Rust toolchain is appropriate. Confirm the executable before changing any
connection state:

```sh
anyr --version
```

Choose one backend while configuring or switching a connection. Ask when that
choice is genuinely ambiguous; an ordinary operation on an already configured
MCP connection uses its selected mode without asking again.

## Desktop

Start the existing Anytype desktop application. Select an isolated desktop
profile and authenticate with the code shown by the application:

```sh
export ANYTYPE_URL=http://127.0.0.1:31009
export ANYTYPE_KEYSTORE_SERVICE=anyr-desktop
anyr auth login
anyr auth status
```

For `anyr mcp`, set `ANY_MCP_CONNECTION_MODE=desktop` in the MCP host and
omit `ANYTYPE_GRPC_ENDPOINT` from that process environment, e.g.:

```json
{
  "anytype": {
    "command": "anyr",
    "args": ["mcp"],
    "env": {
      "ANY_MCP_CONNECTION_MODE": "desktop",
      "ANYTYPE_URL": "http://127.0.0.1:31009",
      "ANYTYPE_KEYSTORE_SERVICE": "anyr-desktop"
    }
  }
}
```

Omitting this
non-secret selector from one isolated profile does not remove saved gRPC
credentials from another keystore profile.

HTTP commands can proceed when `ping.http` is successful. A missing or failed
gRPC check does not block an HTTP-only workflow. Do not add headless gRPC
credentials to this profile.

## Headless

Install the [Anytype CLI](https://github.com/anyproto/anytype-cli) by following
its release instructions when it is absent, and confirm its executable before
changing connection state:

```sh
anytype --help
```

Start the operator's existing Anytype CLI account with `anytype serve`, then
use a separate headless profile:

```sh
export ANYTYPE_URL=http://127.0.0.1:31012
export ANYTYPE_GRPC_ENDPOINT=http://127.0.0.1:31010
export ANYTYPE_KEYSTORE_SERVICE=anyr-headless
anyr init-cli
anyr auth status
```

For `anyr mcp`, set `ANY_MCP_CONNECTION_MODE=headless` in the MCP host, e.g.:

```json
{
  "anytype": {
    "command": "anyr",
    "args": ["mcp"],
    "env": {
      "ANY_MCP_CONNECTION_MODE": "headless",
      "ANYTYPE_URL": "http://127.0.0.1:31012",
      "ANYTYPE_GRPC_ENDPOINT": "http://127.0.0.1:31010",
      "ANYTYPE_KEYSTORE_SERVICE": "anyr-headless"
    }
  }
}
```

When customized
endpoints are needed, provide both HTTP and gRPC endpoints for the same host;
do not combine transports from different Anytype processes.

`anyr init-cli` reuses an existing CLI account when its reusable credentials
are available. Do not pass `--force` for ordinary setup: it intentionally
ignores the existing CLI configuration and creates a new account. Use it only
after the operator explicitly requests a new account or a destructive reset.

Headless HTTP and gRPC are a pair. Commands that require gRPC proceed only
when both applicable pings succeed; HTTP-only commands need only the HTTP
result. Preserve the selected URL, gRPC endpoint, and keystore service for
later invocations, such as in the host's protected environment configuration.

## Status and bounded recovery

`anyr auth status` reports credential presence and a redacted ping for each
configured transport. Check the transport the requested command uses. Do not
use an absent gRPC credential as a reason to block an HTTP-only operation.

For a structured MCP or CLI error that says a backend is missing,
unavailable, or unauthenticated, route to this setup procedure once:

1. Confirm the intended desktop or headless profile and inspect `anyr auth
   status` without exposing its stored values.
2. For a local service, verify that the operator's existing application or
   CLI service is running. For `grpc_unavailable` in an explicitly selected
   headless profile with saved credentials, use the operator's normal service
   supervisor or start `anytype serve` once when authorized; do not spawn a
   duplicate daemon. Wait for one bounded readiness interval, then repeat only
   the applicable status check. Authenticate again only for an authentication
   error, not merely because the service was stopped.
3. For a remote service, do not restart it, guess its endpoint, or create an
   account. Report the redacted category and request its endpoint or operator
   assistance.

If the MCP host is connected to a stopped headless service and the operator
chooses the running desktop app instead, update that host's protected
environment to the isolated desktop profile, remove the gRPC endpoint selector
from that process configuration, and reconnect or restart the MCP process.
Do not copy or delete credentials while switching profiles.

Do not run `anyr auth logout`, delete a keystore, clear credential variables,
or reset account data as connection recovery. Those actions discard usable
credentials and require separate authorization. Removing an endpoint selector
from one isolated MCP process configuration is profile selection, not
credential deletion.

If an initialization or mutation command times out or is cancelled, its result
may already exist. Read or search the relevant state before retrying. Reuse an
idempotency key only for the identical request; otherwise report the
indeterminate outcome.
