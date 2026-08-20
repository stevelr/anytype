---
name: any-mcp
description: Use when an agent needs to read, search, create, edit, organize, upload, or chat with Anytype through the bounded any-mcp server. Covers safe tool selection, exact identifier chaining, optional toolsets, common PKM workflows, and the narrow anyr fallbacks for chat subscriptions and rich chat blocks.
---

# Anytype MCP

The any-mcp server (`anyr mcp`) is reached over its Streamable HTTP
interface: it runs as a persistent service (systemd unit `any-mcp`,
loopback-only, bearer-token auth), and the agent's MCP client is already
configured to connect to it. Call the advertised tools of the connected
`anymcp` server. Never launch `anyr mcp` yourself and never spawn a stdio
copy — a second server is unnecessary, and stdio mode is reserved for hosts
without the HTTP service. If the `anymcp` tools are absent from your tool
list, report that the server was unreachable when your session started
instead of trying to start one.

*Tool choice*: Use advertised anymcp tools for ordinary Anytype workflows. Load the anyr skill only when the workflow reaches a documented CLI-only fallback or requires CLI setup and diagnosis.

## Start safely

1. Call `server_status`. If optional tools matter, call `optional_toolset_status`.
2. Discover spaces, types, properties, tags, collections, chats, and objects;
   do not guess IDs or assume a display name is unique.
3. Read before mutating. Reuse returned IDs, cursors, body hashes, and
   `resource_uri` values exactly.
4. Ask before a destructive or broad mutation unless the user already
   authorized it. Treat read-only rejection as a permission boundary.
5. Give each logical create a caller-stable `idempotency_key`. Reuse the same
   key only when retrying the identical request.
6. Omit unused optional fields. Do not send JSON `null`.
7. After a timeout or cancellation, reread state before retrying a mutation.
8. Ask for missing user-authored content rather than inventing it.

The standard read-write profile supplies object create/update/archive and
discovery. Optional startup toolsets add:

| Need                                          | `ANY_MCP_TOOLSETS` entry |
| --------------------------------------------- | ------------------------ |
| Typed page blocks and `rich_page_create`      | `body-blocks`            |
| Chat reads and plain-message writes           | `chats`                  |
| Upload/read files                             | `files`                  |
| Create/update spaces, types, properties, tags | `schema`                 |
| Manual collection membership                  | `views-write`            |

If a needed tool is absent, report the missing startup selection instead of
inventing a tool name. Read [tool-map.md](references/tool-map.md) for the
capability boundaries and startup guidance.

## Choose the mutation

- Create a Markdown page: `object_create` with `body_markdown`.
- Replace a name, body, type, icon, or typed property: `object_update`.
- Change known body text: read the complete body and hash, then use
  `object_edit` with exact match counts.
- Build a typed block tree: `rich_page_create` or the body-block tools.
- Upload at most 65,536 bytes: `file_upload`; pass canonical base64, not a
  host path. There is no chunked-upload fallback.
- Add an object or file to a manual collection: `collection_member_add`.
- Assign a tag: discover the tag ID, then update its select or multi-select
  property using the stable property key.
- Send a plain chat reply: `chat_message_add`, including
  `reply_to_message_id` when replying.

Use the verified result of each step as the input to the next. Never parse an
ID from a resource URI when the result already includes the ID.

## Follow workflow recipes

Read [workflows.md](references/workflows.md) for tested recipes covering:

- Markdown capture and meeting notes;
- image upload plus collection membership;
- tag assignment;
- plain and rich chat messages;
- task capture and completion;
- inbox/weekly-review organization; and
- the `save-links` subscription, Trafilatura extraction, page/tag creation,
  and reply to the originating message with the new Anytype link.

The machine-checked MCP arguments used by those recipes are in
[tool-call-examples.json](references/tool-call-examples.json).

## Use fallbacks only for known gaps

any-mcp currently has no background chat subscription or watermark tool and
`chat_message_add` accepts only a plain paragraph. For those capabilities:

- use a supervised `anyr chat listen` process only as a best-effort
  `save-links` wake-up stream;
- use `anyr chat messages send --blocks-json` for rich chat blocks;
- return to any-mcp for bounded reads and verified mutations.

Do not replace ordinary any-mcp workflows with ad hoc `anyr` calls. Never put
credentials, full upstream responses, or untrusted page content in logs.
