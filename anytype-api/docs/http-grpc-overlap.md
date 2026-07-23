# API surface: REST enhancements and gRPC extensions

This document describes the parts of `anytype` that add developer ergonomics
to the Anytype REST API and the Rust features that still require gRPC. It is not
an endpoint-by-endpoint REST reference: the crate covers the Anytype REST API
dated 2025-11-08, and the generated rustdoc is the reference for individual
methods and fields.

## Transport model

`AnytypeClient` presents one Rust API over two transports:

- REST is used for the standard auth, space, type, property, tag, object,
  template, view, member, search, basic file-transfer, and space-scoped chat
  operations.
- gRPC is used where REST has no equivalent or returns less information. Those
  capabilities are listed in [gRPC-only extensions](#grpc-only-extensions).
- File upload chooses a backend automatically. A path or byte upload without
  rich options uses REST; URL uploads and uploads with `file_type`, `style`,
  `details`, `created_in_context`, or `created_in_context_ref` use gRPC.

The transports authenticate separately. REST methods need
`HttpCredentials`; a method listed as gRPC-backed needs `GrpcCredentials` in
the configured `KeyStore`. `AnytypeClient::auth_status`, `ping_http`, and
`ping_grpc` report or test the two sides independently.

## Developer-experience layer over REST

### Consistent request builders

REST endpoints are exposed as fluent builders instead of raw URL, query-string,
and JSON construction. The common shape is:

```text
client.<entities>(...).filters/options().list()
client.<entity>(...).get() or .delete()
client.new_<entity>(...).fields().create()
client.update_<entity>(...).fields().update()
```

Terminal verbs and builder names are consistent across spaces, objects, types,
properties, tags, templates, views, members, files, and chats. Required
arguments are accepted by the entry-point method; optional values are added by
chaining. String-like inputs generally accept `Into<String>`, and collection
inputs generally accept `IntoIterator`, so callers do not have to reshape
owned and borrowed values just to construct a request.

Single-field REST response envelopes are unwrapped before returning. For
example, object endpoints return `Object`, not an internal
`{ "object": ... }` response type.

### Name, key, and ID resolution

The [`resolve` module](../anytype-api/src/resolve.rs) allows user-facing tools to accept
names and keys where the server requires IDs:

| Helper                                                  | Accepted input                                     | Result                   |
| ------------------------------------------------------- | -------------------------------------------------- | ------------------------ |
| `resolve_space_id`                                      | space name or ID                                   | space ID                 |
| `resolve_type` / `resolve_type_id` / `resolve_type_key` | type key, name, or ID                              | typed `Type`, ID, or key |
| `resolve_type_ids`                                      | multiple type keys, names, or IDs                  | type IDs                 |
| `resolve_view_id`                                       | view name or ID                                    | view ID                  |
| `resolve_property_id`                                   | property key or ID                                 | property ID              |
| `resolve_chat_target` / `resolve_chat_ids`              | chat or space name/ID, with optional space context | `ChatTarget` or chat IDs |
| `resolve_chat_name`                                     | chat ID                                            | display name             |

Values that syntactically look like IDs are passed through when possible.
Name matching is case-insensitive and reports `AnytypeError::Ambiguous` rather
than silently choosing among duplicates. A leading `@` explicitly identifies a
type key, such as `@page`.

The client cache complements these helpers. It caches spaces, types, and
properties and indexes types and properties by both ID and key. Public lookup
helpers include `lookup_space_by_name`, `lookup_types`,
`lookup_type_by_key`, `lookup_properties`, `lookup_property_by_key`, and
`lookup_property_tag`. Select-property tags can be addressed by ID, key, or
name. The cache can be disabled or cleared when an application needs to observe
changes made by other clients immediately.

### Request-value conversion

The [`SetProperty` trait](../anytype-api/src/properties.rs) builds the JSON shape expected
by object create and update endpoints through typed setters such as
`set_text`, `set_number`, `set_date`, `set_checkbox`, `set_select`,
`set_multi_select`, `set_files`, and `set_objects`.

`AnytypeClient::set_properties` is the higher-level string-to-object adapter.
Given a `Type` and key/value string pairs, it:

- validates that every property belongs to the type;
- parses number and boolean strings;
- splits comma-separated file, object, and multi-select values;
- resolves select and multi-select values supplied as tag ID, key, or name;
- emits the correctly shaped property JSON on a new- or update-object builder.

Filters receive similar treatment. `Filter` constructors encode only sensible
field/operator/value combinations, `FilterExpression` supports nested AND/OR
groups, and `Sort::asc` / `Sort::desc` build the wire representation. The raw
public filter types remain available as an escape hatch.

### Typed replies and object accessors

REST JSON is converted into domain types including `Object`, `Type`,
`Property`, `PropertyValue`, `Tag`, `Space`, `Member`, `View`, `ChatMessage`,
and `FileObject`. Important conveniences include:

- `PropertyValue` is a tagged enum for text, number, date, checkbox, select,
  multi-select, file, object, URL, email, and phone values.
- `PropertyValue::as_*` and `Object::get_property_*` convert reply values to
  strings, numbers, booleans, dates, arrays, and `Tag` values without callers
  traversing JSON.
- Null arrays returned for multi-select, file, or object properties are
  normalized to empty vectors.
- Wire strings for layouts, colors, roles, states, file types, message styles,
  and similar tokens are represented by Rust enums. Forward-compatible chat
  and file models preserve unknown values where the server can add new tokens.
- The unified file uploader normalizes both REST and gRPC upload responses into
  `FileObject`.

### Cross-cutting helpers

- `PagedResult<T>` dereferences to its first `PaginatedResponse<T>` and can
  fetch all remaining pages with `collect_all()` or `into_stream()`.
- Configurable validation limits reject malformed IDs, excessive names,
  markdown, tags, and query strings before network I/O.
- Create builders support optional eventual-consistency verification through
  `ensure_available`; `VerifyConfig` controls retries and backoff.
- One HTTP pipeline supplies serialization, typed API errors, tracing, retries,
  rate-limit backoff, response-size validation, and request/byte metrics.
- Authentication helpers combine challenge creation, user-code exchange,
  in-memory credential installation, and optional keystore persistence.

## gRPC-only extensions

The following public Rust features use gRPC because REST lacks the capability
or equivalent fidelity. Some surrounding APIs also have REST variants; the
table calls out only the additional gRPC behavior.

| Area             | Capability unavailable from REST                                                                                                        | Primary Rust entry points                                                                            |
| ---------------- | --------------------------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------- |
| Files            | List/search/get rich file-object metadata                                                                                               | `files().list`, `files().search`, `files().get`                                                      |
| Files            | URL upload and uploads with file type, placement style, details, or creation context                                                    | `files().upload(...).from_url(...)`; rich options on `FileUploadRequest`                             |
| Files            | Preload lifecycle                                                                                                                       | `files().preload`, `files().discard_preload`                                                         |
| Files            | Ask Anytype Heart to download directly to a local destination path                                                                      | legacy `files().download`                                                                            |
| Chats            | Cross-space chat discovery, full-text chat-object search, ID/name lookup through rich object metadata, and default space-chat discovery | `chats().list_chats`, `search_chats*`, `get_chat`, `resolve_chat_by_name`, `space_chat`              |
| Chats            | Structured message blocks and full-fidelity replies, including per-user `ChatState` and fields absent from REST replies                 | `chats().add_message`, `edit_message`, `list_messages`, `get_messages` and the `MessageBlock*` types |
| Chats            | Mark messages unread and perform global/cross-chat read operations                                                                      | `chats().unread_messages`, `chats().read_all`                                                        |
| Chat events      | Reconnecting multi-chat event stream, cross-chat previews, runtime subscribe/unsubscribe, and per-chat catch-up watermarks              | `AnytypeClient::chat_stream` and `ChatStreamControl`                                                 |
| Archived objects | Search/count the archive and permanently delete archived objects in batches                                                             | `list_archived`, `count_archived`, `delete_archived`, `delete_all_archived`                          |
| Backups          | Export a space as JSON, protobuf, or Markdown with archive, file, nesting, back-link, and schema options                                | `backup_space` / `BackupSpaceRequest`                                                                |
| Processes        | Subscribe to import/export/file process events, wait for completion, reconnect, cancel, and collect progress                            | `ProcessWatcher`                                                                                     |
| Sharing          | Ask Heart for an object's share-by-link URL                                                                                             | `AnytypeClient::get_share_link`                                                                      |

### Overlapping file operations

Basic upload, byte download, `HEAD` metadata, byte ranges, conditional
downloads, and deletion are REST-backed. Prefer `files().upload`,
`download_bytes`, `download_request`, `metadata`, and `delete_request` for
those cases. The upload builder automatically moves to gRPC only when its
source or options require it.

## Chat message fidelity: REST/OpenAPI versus gRPC

`chats().in_space(space_id)` is the REST surface. It supports chat listing and
creation; plain message add/edit/get/list/search/delete; reactions and read
state; and a typed Server-Sent Events stream for one chat. The direct `chats()`
message builders use gRPC. Several simple mutations exist on both transports
for compatibility; their presence on the gRPC client does not imply that REST
lacks the basic operation.

### Chat content model

The REST and gRPC message models share a simple top-level `MessageContent`
representation:

- one plain text string;
- one style for the whole string;
- zero or more ranged inline marks.

Both models surround that content with message-level attachments and an
optional reply target when adding a message.

REST stops at that representation. It does not accept Markdown for conversion
to chat blocks and does not have a `blocks` field. A newline in `text` does not
introduce a separately styled block: the one `style` value still applies to the
whole message.

The gRPC model retains that top-level `MessageContent` and adds an ordered
`Vec<MessageBlock>`. Each text block can have its own style and marks, so one
message can contain, for example, a heading followed by a paragraph and a
language-tagged code block. The current `anytype` gRPC add and edit builders
require top-level `MessageContent` even when structured blocks are supplied.

| Capability                                              | REST/OpenAPI chat       | `anytype` gRPC chat  |
| ------------------------------------------------------- | ----------------------- | -------------------- |
| Plain text                                              | Yes                     | Yes                  |
| One style applied to the whole top-level text           | Yes                     | Yes                  |
| Ranged inline marks                                     | Yes                     | Yes                  |
| Multiple independently styled text blocks               | No                      | Yes                  |
| Checkbox checked state                                  | No                      | Yes, on a text block |
| Code/text language metadata                             | No                      | Yes, on a text block |
| Dedicated Anytype object/file/image/bookmark link block | No                      | Yes                  |
| LaTeX, Mermaid, or Graphviz embed block                 | No                      | Yes                  |
| Structured editor-block quote                           | No                      | Yes                  |
| Structured chat-message quote                           | No; only a reply target | Yes                  |
| Native chat table block                                 | No                      | No                   |

The chat-specific gRPC block union contains `Text`, `Link`, `Embed`,
`EditorQuote`, and `MessageQuote`. It does not contain a table block, even
though the wider Anytype editor model used by page objects supports tables.
Represent a table in chat as plain or code-formatted text, an attachment, or an
appropriate embed rather than as a native chat table.

### REST write and read fidelity

The OpenAPI add and edit bodies expose `text`, `style`, `marks`, and
`attachments`; add also exposes `reply_to_message_id`. A mark contains `from`,
`to`, `type`, and optional `param`, while an attachment contains `target` and
`type`. REST get, list, search, and SSE message events return the same text,
style, marks, and attachments, plus reactions, reply information, and pin
state.

The [checked-in OpenAPI schema](openapi.json) declares the style, mark type,
and attachment type as plain strings rather than enums. It illustrates
`paragraph`, `bold`, and `image`, but does not make the complete Heart
vocabulary part of the documented HTTP contract. `anytype` nevertheless maps
the known Heart values to their REST strings:

- styles: `paragraph`, `header1` through `header4`, `quote`, `code`, `title`,
  `checkbox`, `bulleted`, `numbered`, `toggle`, `toggle_header1` through
  `toggle_header3`, `description`, and `callout`;
- marks: `strikethrough`, `keyboard`, `italic`, `bold`, `underscored`, `link`,
  `text_color`, `background_color`, `mention`, `emoji`, and `object`;
- attachments: `file`, `image`, and `link`.

`MessageTextStyle::Marked` maps to the REST spelling `bulleted`. Because the
OpenAPI strings are not enumerated, these additional spellings should be
treated as current Heart/client behavior rather than a versioned OpenAPI
guarantee. Unknown REST strings are retained in the corresponding `Other`
variants on read, but the current derived string conversion emits the literal
`other` when those variants are written again; unknown-value write-back is not
lossless.

REST reads never expose gRPC `MessageBlock` values. The server does not flatten
structured blocks into the REST `content` field: an HTTP reader sees only the
message's separate top-level `MessageContent`. A client publishing structured
gRPC blocks should therefore keep the top-level content meaningful and aligned
with the blocks; it acts as the only representation visible to REST clients.
The repository's chat integration coverage verifies this loss explicitly by
publishing a distinct gRPC heading block and asserting that the REST reply has
an empty `blocks` vector.

The gRPC read path additionally returns per-user `ChatState` and message flags
such as `read`, `mention_read`, `has_mention`, `synced`, and
`unread_reaction`. REST lacks those reply fields, although it provides separate
operations for changing or querying some read and reaction state. Conversely,
REST can supply `creator_name`, which the gRPC conversion leaves unset.

For forward compatibility, the gRPC conversion records unknown numeric enum
values while reading, but arbitrary outgoing string values do not fit its
protobuf enums. The current client maps unknown outgoing styles, marks, and
attachment kinds to `paragraph`, `bold`, and `file`, respectively; unknown
numeric link-block and embed-processor values remain representable.

Use the direct `chats()` gRPC builders when structured `MessageBlock` values,
per-user chat state, cross-space discovery, unread-state mutation, or
reconnecting multi-chat subscriptions are required. Relevant models and
transport conversions are in [`chats.rs`](../anytype-api/src/chats.rs), and the
REST-versus-gRPC block-loss test is in
[`test_chat_discovery.rs`](../anytype-api/tests/test_chat_discovery.rs).

## Backend boundary in the source

REST endpoint wrappers and ergonomics live primarily in the entity modules and
[`http_client.rs`](../anytype-api/src/http_client.rs). The additional gRPC
surface is isolated to [`files.rs`](../anytype-api/src/files.rs),
[`chats.rs`](../anytype-api/src/chats.rs),
[`chat_stream.rs`](../anytype-api/src/chat_stream.rs),
[`process_watcher.rs`](../anytype-api/src/process_watcher.rs), the
archived/backup portions of [`spaces.rs`](../anytype-api/src/spaces.rs), and
share-link retrieval in [`objects.rs`](../anytype-api/src/objects.rs).
`anytype-rpc` remains the lower-level gRPC
client; applications using `anytype` normally do not need to construct protobuf
requests directly.
