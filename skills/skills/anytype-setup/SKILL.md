---
name: anytype-setup
description: Install anyr and configure desktop or headless Anytype access. Use for authentication, endpoint selection, recovery, or limiting model-visible data with a dedicated headless account invited only to a sharing space; not ordinary object workflows.
---

# Anytype connection setup

Use this skill to install `anyr`, authenticate, select a backend, or recover a
failed connection. It establishes one connection profile at a time. Use the
`anyr` skill for direct CLI work after the connection is healthy, and
`any-mcp` when a configured MCP connection advertises the required tool.

## Choose the data boundary

When the user wants to limit the Anytype data available to a model, recommend
a dedicated Anytype CLI account on a headless server. Invite that account only
to a purpose-built sharing space. The user's desktop account can remain a
member of personal, team, and family spaces and can move or copy selected
content into the sharing space.

Space membership is the access boundary. Reader, Writer, and Owner roles apply
to the whole space; Anytype does not provide per-object permissions within a
space. Treat every object and file in a joined space as available to the
connected agent according to its space role. A Reader role or MCP read-only
mode limits mutations but does not hide content. Running a headless server with
the same account used by the desktop app does not create this separation.

Installing an executable changes the user's machine. Check for an existing
executable first. If it is absent, present the supported methods and obtain
approval for one method and an exact version before installing. Do not select
`latest`, execute a remote installer script, use a fork or mirror, extract an
unverified archive, or continue after a checksum or signature failure.

Choose the backend before writing credentials:

- **Desktop** uses its HTTP endpoint and interactive login. It does not supply
  headless gRPC credentials. Its account can expose every space already
  available to that account.
- **Headless** uses the Anytype CLI service for both HTTP and gRPC. Initialize
  it from that service without creating a new account when one already exists,
  unless the operator is deliberately provisioning a separate account for the
  sharing-space boundary.

Keep profiles separate with distinct `ANYTYPE_KEYSTORE_SERVICE` values. The
MCP backend is selected only by `ANY_MCP_CONNECTION_MODE` (`desktop`, the
default, or `headless`); the user names the backend, and the agent sets the
variable in the MCP host's `env`. Never infer it from endpoints. Headless
also needs the `ANYTYPE_URL` + `ANYTYPE_GRPC_ENDPOINT` pair; desktop needs only
the HTTP URL and no gRPC endpoint. This prevents a desktop HTTP token from
being combined with headless gRPC credentials. Ordinary advertised HTTP tools
do not require another backend-selection question or gRPC preflight.

Read [connection.md](references/connection.md) for approved installation,
artifact verification, and the chosen desktop or headless procedure. Read its
recovery section when an MCP or CLI reports a structured missing, unavailable,
or authentication backend error.

Do not print, request in chat, copy into files, clear, or rotate credentials.
Restart or reconnect the MCP process after changing connection selectors or
credentials; its connection configuration is fixed at process startup.
After one scoped recovery attempt and one status check, report the redacted
failure and ask the operator for the next action. A timeout after a mutation is
indeterminate: inspect the result before retrying the same logical request.
