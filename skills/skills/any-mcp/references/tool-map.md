# any-mcp tool map

## Startup

The MCP host connects to `anyr mcp` over stdio or Streamable HTTP. Agents call
the advertised tools and leave server lifecycle and configuration to the host
operator. A broad personal-knowledge workflow can select:

```text
ANY_MCP_PROFILE=standard
ANY_MCP_READ_ONLY=0
ANY_MCP_TOOLSETS=body-blocks,chats,files,schema,views-write
```

These values are operator choices, not package defaults. Add only the
registries the workflow needs. `members` is useful for member inspection but
is not required by these recipes. Mutation tools are absent when
`ANY_MCP_READ_ONLY=1`.

For an ordinary HTTP workflow, call its advertised HTTP tool directly; a gRPC
preflight is unnecessary. Call `server_status` when connection information is
needed and `optional_toolset_status` only when the requested optional toolset
matters. A selected toolset or profile can require gRPC, while the HTTP tools
do not inherit that requirement.

If a tool returns a structured missing, unavailable, or authentication backend
error, stop and route once to `anytype-setup`. Preserve credentials and report
the redacted category if setup cannot restore the selected connection. Local
recovery can verify the existing service and repeat the applicable setup step;
remote recovery requires the endpoint owner because an agent must not restart
or reconfigure a remote service. Do not mix desktop HTTP with headless gRPC.
Start an existing headless account normally; account creation or forced
initialization needs explicit operator authorization.

The host also selects exactly one MCP connection mode:

- `ANY_MCP_CONNECTION_MODE=desktop` uses HTTP and never admits gRPC-backed
  tools. It rejects a gRPC endpoint selector but leaves stored credentials
  untouched.
- `ANY_MCP_CONNECTION_MODE=headless` pairs the headless HTTP and gRPC
  endpoints. Customized connections provide both endpoints on the same host.

The catalog remains stable when gRPC is stopped or unauthenticated. The server
performs one bounded admission check only when a gRPC-backed tool is invoked.
It reports `grpc_not_configured`, `grpc_unavailable`, or `authentication`
without exposing endpoint or credential details. `server_status` reports the
last observation and does not probe either backend.

These tools require the headless gRPC backend:

| Catalog | Tools |
| --- | --- |
| Core | `object_archive` |
| `body-blocks` | `body_block_create`, `body_block_delete`, `body_block_list`, `body_block_move`, `body_block_update`, `rich_page_create`, `rich_page_resume` |
| `schema` | `type_update` |
| `views-write` | `collection_member_list`, `collection_member_add`, `collection_member_remove` |

All other advertised tools use HTTP. `type_update` is conservatively gated as
a whole because its recommended-property path uses gRPC.

## Core tools

| Goal               | Tool             | Important contract                                 |
| ------------------ | ---------------- | -------------------------------------------------- |
| Find objects       | `object_search`  | Bodies are omitted; follow its cursor              |
| Read an object     | `object_get`     | Stable object ID required; request body explicitly |
| Create a page/task | `object_create`  | Omit unused fields; stable retry key               |
| Replace fields     | `object_update`  | Typed complete property replacements               |
| Patch known text   | `object_edit`    | Complete-body hash and exact match count           |
| Archive one object | `object_archive` | Soft archive, not bulk deletion                    |
| Discover schema    | list tools       | Resolve names to exact keys and IDs                |

List tools include `space_list`, `type_list`, `property_list`, `tag_list`,
`template_list`, `view_list`, and `view_object_list`. Pagination cursors are
opaque and bound to the original query.

## Optional tools used by the recipes

| Toolset       | Tools                                                           |
| ------------- | --------------------------------------------------------------- |
| `body-blocks` | `rich_page_create`, `body_block_list/create/update/delete/move` |
| `chats`       | `chat_list`, `chat_message_list/get/search/add/delete`          |
| `files`       | `file_upload`, `file_metadata`, `file_read`                     |
| `schema`      | `tag_create` plus space/type/property/tag mutation tools        |
| `views-write` | `collection_member_list/add/remove`                             |

`chat_message_add` is deliberately plain text. There is no any-mcp background
subscription, watermark, or rich-chat write tool. `anyr chat listen --after`
can perform a gRPC catch-up listing before live events, but its watermark does
not parameterize the live subscription. Use the documented narrow fallbacks;
do not claim any-mcp performed those operations.

## Typed property assignments

`object_create.properties` and `object_update.properties` are arrays. Common
forms are:

```json
[
  { "format": "checkbox", "key": "done", "checkbox": true },
  { "format": "multi_select", "key": "tag", "multi_select": ["TAG_ID"] },
  { "format": "files", "key": "attachments", "files": ["FILE_ID"] },
  { "format": "url", "key": "source", "url": "https://example.com/article" }
]
```

Resolve property keys and confirm their formats first. Select and multi-select
values take tag IDs, not tag display names. An empty ID array explicitly clears
the property.

## Resource links and retries

Mutation results return verified IDs and often a canonical `resource_uri`,
such as `anytype://spaces/SPACE_ID/objects/OBJECT_ID`. Reuse that URI when a
workflow must link to the page. Do not hand-build the URI unless the tool did
not return it.

An idempotency key is process-local duplicate control, not a global database
key. Keep it stable across an identical retry and change it when any normalized
input changes. A timeout or cancellation leaves the mutation outcome
indeterminate: search or read first, then retry only when that state proves
the identical write did not take effect.
