# any-mcp tool map

## Startup

The server (`anyr mcp`) runs as a persistent Streamable HTTP service the MCP
host connects to; agents call the advertised tools and never launch a server
themselves (stdio launch exists only for hosts without the HTTP service).
For broad PKM work the service typically runs with:

```text
ANY_MCP_PROFILE=standard
ANY_MCP_READ_ONLY=0
ANY_MCP_TOOLSETS=body-blocks,chats,files,schema,views-write
```

Add only the registries the workflow needs. `members` is useful for member
inspection but is not required by these recipes. Mutation tools are absent
when `ANY_MCP_READ_ONLY=1`.

Call `server_status` first. Call `optional_toolset_status` when any optional
tool is needed. If startup or authentication is unhealthy, stop and report the
redacted error; never ask for credentials in a prompt.

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
input changes. If the outcome is uncertain, search/read before issuing another
write.
