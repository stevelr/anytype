---
name: anytype-setup
description: Install anyr and, when needed, the Anytype headless CLI; connect safely to an existing desktop app or headless server. Use for authentication, endpoint selection, connection diagnosis, and bounded credential recovery; not ordinary object workflows.
---

# Anytype connection setup

Use this skill to install `anyr`, authenticate, select a backend, or recover a
failed connection. It establishes one connection profile at a time. Use the
`anyr` skill for direct CLI work after the connection is healthy, and
`any-mcp` when a configured MCP connection advertises the required tool.

Choose the backend before writing credentials:

- **Desktop** uses its HTTP endpoint and interactive login. It does not supply
  headless gRPC credentials.
- **Headless** uses the Anytype CLI service for both HTTP and gRPC. Initialize
  it from that service without creating a new account when one already exists.

Keep profiles separate with distinct `ANYTYPE_KEYSTORE_SERVICE` values. The
MCP backend is selected only by `ANY_MCP_CONNECTION_MODE` (`desktop`, the
default, or `headless`); the user names the backend, the agent sets the
variable in the MCP host's `env` — never infer it from endpoints. Headless
also needs the `ANYTYPE_URL` + `ANYTYPE_GRPC_ENDPOINT` pair; desktop needs only
the HTTP URL and no gRPC endpoint. This prevents a desktop HTTP token from
being combined with headless gRPC credentials. Ordinary advertised HTTP tools do not require another
backend-selection question or gRPC preflight.

Read [connection.md](references/connection.md) for installation and the chosen
desktop or headless procedure. Read its recovery section when an MCP or CLI
reports a structured missing, unavailable, or authentication backend error.

Do not print, request in chat, copy into files, clear, or rotate credentials.
Restart or reconnect the MCP process after changing connection selectors or
credentials; its connection configuration is fixed at process startup.
After one scoped recovery attempt and one status check, report the redacted
failure and ask the operator for the next action. A timeout after a mutation is
indeterminate: inspect the result before retrying the same logical request.
