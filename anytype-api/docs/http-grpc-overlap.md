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

The [`resolve` module](../src/resolve.rs) allows user-facing tools to accept
names and keys where the server requires IDs:

- `resolve_space_id`: space name or ID to space ID.
- `resolve_type`, `resolve_type_id`, and `resolve_type_key`: type key, name, or
  ID to a typed `Type`, ID, or key.
- `resolve_type_ids`: multiple type keys, names, or IDs to type IDs.
- `resolve_view_id`: view name or ID to view ID.
- `resolve_property_id`: property key or ID to property ID.
- `resolve_chat_target` and `resolve_chat_ids`: chat or space name/ID, with
  optional space context, to `ChatTarget` or chat IDs.
- `resolve_chat_name`: chat ID to display name.

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

The [`SetProperty` trait](../src/properties.rs) builds the JSON shape expected
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
- One HTTP pipeline supplies serialization, typed API errors, tracing,
  absolute per-request deadlines, read-only retries, rate-limit backoff,
  response-size validation, and request/byte/timeout metrics. File and
  multipart REST requests use the long profile; each paginated page uses the
  standard profile. REST SSE keeps open, error-body, established-idle, and
  established-lifetime boundaries separate.
- One gRPC deadline service classifies credential, ordinary, long-operation,
  stream-setup, and cleanup calls, then applies the earliest configured,
  enclosing, or caller boundary. Stream setup is local through response
  headers so it does not accidentally become a whole-stream tonic timeout;
  optional established idle and lifetime controllers cover raw transport
  progress and reconnect work. `AnytypeGrpcClient::channel()` is the explicit
  raw compatibility bypass; `client_commands()` and `deadline_channel()` use
  the policy-aware service.
- Authentication helpers combine challenge creation, user-code exchange,
  in-memory credential installation, and optional keystore persistence.

The dependency direction remains `anytype` to `anytype-rpc`. Deadline policy
does not introduce a reverse `anytype-rpc` dependency on this crate.

## gRPC-only extensions

The following capabilities need a running Anytype CLI server and gRPC
credentials in the configured keystore. Some have lower-fidelity REST
counterparts. The methods listed here execute gRPC or select it when the
corresponding option is present.

### Connection and process primitives

- `AnytypeClient::grpc_client` creates the authenticated low-level client, and
  `AnytypeClient::ping_grpc` verifies that connection.
- `ProcessWatcher::subscribe`, `wait_for_process`, `wait_for_generation`, and
  `unsubscribe` observe import, export, and file processes over the session
  event stream.

### Files

- `files().list(...).list()`, `files().search(...).search()`, and
  `files().get(...).get()` return rich file-object metadata through gRPC.
- `files().download(...).download()` is the legacy direct-to-path gRPC
  download. `download_bytes`, `download_request`, and `metadata` use REST.
- `FileUploadRequest::from_url`, `file_type`, `style`, `details`,
  `created_in_context`, and `created_in_context_ref` select gRPC for the unified
  upload. Plain path, byte, and reader uploads use REST.
- `files().preload(...).preload()` and
  `files().discard_preload(...).discard()` implement the preload lifecycle.

### Chats and chat events

- `list_chats()` without a space lists across spaces through gRPC.
  `search_chats`, `search_chats_in`, `get_chat`, `resolve_chat_by_name`, and
  `space_chat` provide gRPC discovery and rich object reads. A space-scoped
  `list_chats_in(...).list()` uses REST.
- The direct `ChatClient` message builders use gRPC: `send_text`, `edit_text`,
  `toggle_reaction`, `add_message`, `edit_message`, `delete_message`,
  `list_messages`, `get_messages`, `read_messages`, `unread_messages`, and
  `read_all_account`. `SpaceChatsClient`, returned by `chats().in_space(...)`,
  is the REST chat surface.
- gRPC replies add structured `MessageBlock` values, per-user `ChatState`, and
  message read, mention, synchronization, and unread-reaction flags. REST
  replies omit those fields.
- `resolve_chat_target`, `resolve_chat_ids`, and `resolve_message_id(s)` need
  gRPC when they resolve names, default space chats, or order IDs. Exact IDs
  with the required space context pass through without a gRPC request.
- `AnytypeClient::chat_stream`, `ChatStreamBuilder`, and `ChatStreamControl`
  provide reconnecting multi-chat events, cross-chat previews, dynamic
  subscriptions, and catch-up watermarks through gRPC.

### Spaces, backups, and archived objects

- `create_chat_space` and `delete_space` create a chat workspace and
  permanently delete a space through gRPC. Ordinary `new_space` creation uses
  REST.
- `list_space_invites`, `create_space_invite`, and `revoke_space_invite`
  manage member and guest invitations through gRPC.
- `enable_space_sharing` and `disable_space_sharing` change public sharing
  through gRPC.
- `list_archived`, `count_archived`, `count_archived_bounded`,
  `delete_archived`, and `delete_all_archived` inspect or permanently remove
  archived objects through gRPC.
- `backup_space(...).backup()` exports a space as JSON, protobuf, or Markdown
  and can include selected objects, nested objects, files, archived objects,
  backlinks, and space metadata.

### Body blocks, discussions, types, and collections

- `blocks().body(...).fetch()` reads the exact typed block graph through gRPC.
  `BodySnapshot::edit` returns a `BodyEditor`; its `create`, `append`, `update`,
  `delete`, `move_block`, and `apply_all` methods execute verified gRPC
  mutations.
- `attached_discussion(...).get()` and `ensure()` combine an exact REST parent
  preflight with gRPC object show, close, and optional discussion creation.
  Both credential families are required.
- `TypeRequest::classify_properties` and
  `classify_properties_with_deadline` combine a direct REST type read with
  gRPC source lists to distinguish featured and recommended properties.
- `observe_collection_membership` and `collection_membership_page` combine
  REST identity checks with bounded gRPC subscriptions to read canonical
  direct collection membership.

### Overlapping file operations

Basic upload, byte download, `HEAD` metadata, byte ranges, conditional
downloads, and deletion are REST-backed. Prefer `files().upload`,
`download_bytes`, `download_request`, `metadata`, and `delete_request` for
those cases. The upload builder automatically moves to gRPC only when its
source or options require it.

Object share links are constructed locally from validated space and object
IDs. They do not call the retired `ObjectShareByLink` RPC.

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
transport conversions are in [`chats.rs`](../src/chats.rs), and the
REST-versus-gRPC block-loss test is in
[`test_chat_discovery.rs`](../tests/test_chat_discovery.rs).

## Backend boundary in the source

REST endpoint wrappers and ergonomics live primarily in the entity modules and
[`http_client.rs`](../src/http_client.rs). The additional gRPC surface is
isolated to [`files.rs`](../src/files.rs), [`chats.rs`](../src/chats.rs),
[`chat_stream.rs`](../src/chat_stream.rs),
[`process_watcher.rs`](../src/process_watcher.rs), the gRPC portions of
[`spaces.rs`](../src/spaces.rs), [`body.rs`](../src/body.rs),
[`body_mutation.rs`](../src/body_mutation.rs),
[`attached_discussions.rs`](../src/attached_discussions.rs),
[`types.rs`](../src/types.rs), and [`views.rs`](../src/views.rs).
`anytype-rpc` remains the lower-level gRPC
client; applications using `anytype` normally do not need to construct protobuf
requests directly.
