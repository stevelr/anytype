# anytype

An ergonomic Anytype API client in Rust.

[![release](https://img.shields.io/github/v/tag/stevelr/anytype?sort=semver&filter=anytype-v*&label=release)](https://github.com/stevelr/anytype/releases?q=anytype-v&expanded=true)
[![docs.rs](https://img.shields.io/docsrs/anytype?label=docs.rs)](https://docs.rs/anytype)
[![crates.io](https://img.shields.io/crates/v/anytype.svg)](https://crates.io/crates/anytype)

**[Home](https://github.com/stevelr/anytype) &nbsp; | &nbsp; [Documentation](https://docs.rs/anytype) &nbsp; | &nbsp; [Examples](https://github.com/stevelr/anytype/blob/main/anytype-api/examples/)**

## Overview

`anytype` provides an ergonomic rust client for [Anytype](https://anytype.io). It supports listing, searches, and CRUD operations on Objects, Properties, Spaces, Tags, Types, Members, Views, Files, and Chats, with optional key storage and caching. REST is preferred when it has equivalent functionality; gRPC supplies richer file metadata/upload options, structured chat messages, attached object discussions, and event streaming.

Applications authenticate with Anytype servers using access tokens. One token is required for http apis, and if gRPC apis are used (for files or chats), an additional gRPC token is required. The `anytype` library helps generate tokens and store them in a KeyStore.

Call `AnytypeError::is_authentication()` when an embedding application needs
stable authentication guidance. The predicate recognizes direct HTTP and
configuration failures plus structurally typed nested gRPC authentication
failures without exposing or parsing response messages, URLs, or credentials,
and callers do not need a direct `anytype-rpc` dependency.

### Features

- 100% coverage of Anytype API 2025-11-08
- gRPC back-end provides extensions not available in REST (rich file operations, structured chat blocks, and full event streaming)
- Paginated responses and async Streams
- Integrates with OS Keyring for secure storage of credentials (HTTP + gRPC)
- HTTP middleware with secret-safe metadata logging, retries, and rate limit handling
- Client-side caching (spaces, properties, types)
- Name and id resolution helpers (`resolve` module): accept a space, type,
  template, chat, view, or property by name, key, or id; ambiguous names return up to 10
  deterministic candidate ids and display names for an actionable retry, and
  bounded scans fail explicitly instead of guessing from partial results;
  candidate ordering is independent of upstream page order. Explicit type IDs
  that need metadata use one cache-independent scoped GET and reject a
  mismatched returned identity instead of priming the all-types cache. Direct
  property reads provide the same metadata-only exact-identity path and never
  expand tags. Explicit-ID tag lookup follows that GET with a separately
  paginated 1,000-row scan whose total and page windows must remain coherent
  before a match or not-found result is accepted, without priming all space
  properties or guessing from incomplete results. Template
  resolution uses a direct-id GET or an exact 1,000-row scan and re-fetches the
  selected template to verify its space, canonical generic template type
  id/key, and non-archived state; the validated endpoint path establishes the
  owning object type. Malformed rows that match the requested template name
  fail closed unless a valid row with the same stable id supplies the safe
  representative.
- Typed, bounded body-block reads (`body` module): validated block trees with
  exact IDs and order over gRPC `ObjectShow`, plus verified typed create,
  append, update, delete, move, and bounded non-transactional batch operations
- Nested filter expression builder
- Parameter validation
- Metrics
- used in:
  - [anyr](https://github.com/stevelr/anytype/tree/main/anyr) - list, search, and manipulate anytype objects
  - [any-edit](https://github.com/stevelr/anytype/tree/main/any-edit) - edit anytype docs in markdown in external editor

Numeric filters support `eq`, `ne`, `lt`, `lte`, `gt`, and `gte`; checkbox
filters support `eq` and `ne`. Typed values pass through unchanged in search
expressions and become canonical number text or lowercase boolean text only
where a list endpoint requires URL query values. The client does not coerce
strings to numbers or booleans, accept checkbox `1`/`0` aliases, or emulate
server filtering after pagination. See the workspace
[filter status](../FILTER_STATUS.md) for the live compatibility matrix and
the disposition of the historical upstream limitation.

### Bounded HTTP responses

Buffered REST responses have finite byte ceilings. Ordinary JSON defaults to
8 MiB, single-object/document JSON to 64 MiB, bounded error bodies to 64 KiB,
and raw file downloads to a separate 256 MiB policy. Truthful oversized
`Content-Length` responses are rejected before their body is read; responses
without a usable length are stopped at the first byte over the ceiling. SSE
chat events remain incremental rather than buffered as JSON, but each pending
event (including its delimiter) has a separate 1 MiB default ceiling. Incoming
transport chunks are consumed without copying them into the event buffer, and
one chunk may contain several independently bounded events. Overflow terminates
the stream with `AnytypeError::ChatSseEventTooLarge` before the one-over byte is
appended. Stream space and chat IDs are validated as path-safe before URL
construction or diagnostic logging.

Applications can lower or raise the defaults within the library's hard
maxima. An individual object read can choose a smaller ceiling but cannot
exceed the configured document allowance:

```rust,no_run
use anytype::prelude::*;

# async fn example() -> Result<(), AnytypeError> {
let config = ClientConfig {
    response_limits: ResponseLimits {
        json_bytes: 4 * 1024 * 1024,
        document_bytes: 24 * 1024 * 1024,
        error_bytes: 32 * 1024,
        file_bytes: 128 * 1024 * 1024,
        chat_sse_event_bytes: 512 * 1024,
    },
    ..ClientConfig::default()
};
let client = AnytypeClient::with_config(config)?;
let object = client
    .object("space-id", "object-id")
    .response_limit_bytes(12 * 1024 * 1024)
    .get()
    .await?;
# let _ = object;
# Ok(())
# }
```

The 64 MiB document default accommodates worst-case JSON escaping of a valid
10 MiB outgoing markdown body. The hard maxima are 64 MiB for ordinary and
document JSON and chat SSE events, 1 MiB for error bodies, and 1 GiB for raw
files. `AnytypeError::ResponseTooLarge` contains only
the selected ceiling and optional declared length; it never retains a response
body, URL, request payload, or credential.

### Retry safety

Automatic response, rate-limit, and transport retries are restricted to HTTP
methods already classified as replay-safe: `GET`, `HEAD`, `PUT`, `DELETE`, and
`OPTIONS`. `POST` and `PATCH` are sent exactly once. A 429, timeout-status,
server-status, disconnect, or timeout from one of those mutation methods is
returned to the caller without replaying its body, because the server may have
applied a write even when the client did not receive a usable response.

The client disables reqwest's lower-level retry and redirect handling so every
additional send passes through this method-aware policy and its metrics. A 3xx
response is returned as an API error without forwarding the bearer credential
or request body to the `Location`. Consequently, redirect or retry policies
set on a `ClientBuilder` passed to `AnytypeClient::with_client` are intentionally
overridden; timeout, proxy, DNS, TLS, and user-agent customization is retained.

`ClientConfig::rate_limit_max_retries` continues to control consecutive 429
retries for replay-safe requests; zero disables that rate-limit-specific cap.
Independently, one cumulative ceiling permits at most six physical attempts
across 429, retryable-status, connection, and timeout failures, and the counter
never resets when the failure class changes. It does not opt mutation requests
into retries. HTTP metrics expose independent `logical_operations` and
`physical_attempts` counters; the existing `total_requests` field retains its
physical-request meaning.

`http_credential_generation()` exposes only a monotonic process-local number.
It advances whenever the in-memory HTTP key is set or cleared, allowing
principal-bound caches to invalidate entries without reading, retaining, or
hashing the credential itself. Credential replacement and generation advance
share one synchronization boundary; no observer can see a mixed pair.

### Secret-safe HTTP diagnostics

The library-owned HTTP diagnostics remain metadata-only at every `RUST_LOG`
level. The `anytype::http` target reports stable error variants with an HTTP
status, validated method, and bounded path-only context when available.
`anytype::http_json=trace` adds request/response byte counts and query-field
counts, but never logs request or response bodies.

No directive for those two HTTP targets enables query values, headers, full
URLs, bearer tokens, credential-bearing URL components, or Anytype document
content. This guarantee is HTTP-specific: other `anytype` tracing targets are
outside its scope, so applications enabling them need an appropriate filter.

Standard `AnytypeError` `Display` and `Debug` output and its error source chain
exclude all free-form messages, identities, candidate values, last errors,
paths from malformed targets, and typed upstream sources that could contain
request or document content. Use `error.diagnostic()` for structured
application logs. Raw public fields—including `ApiError::message`,
`RateLimitExceeded::header`, validation messages, resolver identities, and
typed sources—remain available through explicit variant matching and must not
be logged without an application policy.

## Quick start

```rust
use anytype::prelude::*;

const PROJECT_SPACE: &str = "Projects";
const CHAT_SPACE: &str = "Chat";

//! Agenda automation:
//! - list top 10 tasks sorted by priority
//! - list 10 most recent documents containing the text "meeting notes"
//! - send the lists in a rich-text chat message with colors and hyperlinks
#[tokio::main]
async fn main() -> Result<(), AnytypeError> {
    let config = ClientConfig {
        app_name: "agenda".to_string(),
        keystore_service: Some("anyr".to_string()),
        ..Default::default()
    };
    let client = AnytypeClient::with_config(config)?;
    let space = client.lookup_space_by_name(PROJECT_SPACE).await?;

    // List 10 tasks sorted by priority
    let mut tasks = client
        .search_in(&space.id)
        .types(vec!["task"])
        .sort_desc("last_modified_date")
        .limit(40)
        .execute()
        .await?
        .into_response()
        .take_items();
    tasks.sort_by_key(|t| t.get_property_u64("priority").unwrap_or_default());

    // Get 10 most recent pages or notes containing the text "meeting notes"
    // sort most recent on top
    let recent_note_docs = client
        .search_in(&space.id)
        .text("meeting notes")
        .types(["page", "note"])
        .sort_desc("last_modified_date")
        .limit(10)
        .execute()
        .await?;

    // Build the message with colored status indicators
    let mut message = MessageContent::new()
        .text("Good morning Jim,\n")
        .bold("Here are your tasks\n");
    for task in tasks.iter().take(10) {
        let priority = task.get_property_u64("priority").unwrap_or_default();
        let name = task.name.as_deref().unwrap_or("(unnamed)");
        message = message.text(&format!("{priority} "));
        message = status_color(message, task);
        message = message.text(&format!(" {name}\n"));
    }

    // add list of docs with hyperlinks
    message = message.bold("\nand recent notes:\n");
    for doc in &recent_note_docs {
        let date = doc
            .get_property_date("last_modified_date")
            .unwrap_or_default()
            .format("%Y-%m-%d %H:%M");
        let name = doc.name.as_deref().unwrap_or("(unnamed)");
        message = message
            .text(&format!("{date} "))
            .link(name, doc.get_link())
            .nl();
    }

    // Send it over chat message
    let chat = client.chats().space_chat(CHAT_SPACE).get().await?;
    client
        .chats()
        .add_message(chat.id)
        .content(message)
        .send()
        .await?;

    Ok(())
}
```

Search pagination limits must be between 1 and 1000 inclusive. Both global and
space-scoped search reject `0` or larger values with `AnytypeError::Validation`
before sending an HTTP request.

See the [Examples](./examples/README.md) folder for more code samples.

For soft-delete workflows that reconcile uncertain responses themselves,
`client.object(space_id, object_id).delete_once()` sends exactly one HTTP
request attempt. Ordinary `delete()` retains the client's replay-safe DELETE
retry policy.

Anytype's canonical Markdown read representation is not always safe to send
back unchanged: for example, a literal underscore in a plain line is returned
escaped. `objects::plain_markdown_representation` provides separate `wire()`
and `canonical()` forms for the deliberately closed subset of empty bodies and
single plain lines containing alphanumeric characters, internal ASCII spaces,
and underscores. It accepts either raw or already-canonical values and is
idempotent on replay. It returns `None` for punctuation, multiline Markdown,
and ambiguous backslash forms; callers must reject or separately verify those
forms rather than guess at Markdown equivalence or blindly replay export bytes.

The ignored disposable-space matrix in
`tests/test_markdown_fidelity.rs` characterizes the current server's narrower
export/replacement behavior with two stable REST reads and two fresh
`ObjectShow` reads on each side of an exact exported-Markdown replacement.
Representative headings, bullet/numbered lists, checkboxes, a one-line quote,
a link, Unicode, and multiline paragraphs retain byte-identical exports.
Consecutive quote lines, fenced code, tables, literal underscores, and explicit
backslash escapes drift at the byte and typed-block-content levels; they have
no replay-stability contract. Every tested PATCH also replaces block IDs, even
when exported bytes stay identical, so exported-Markdown replacement never
promises block identity. The matrix currently establishes no intermediate
typed-semantic-only cohort.

## Archived Object Cleanup

```rust,no_run
use anytype::prelude::*;

# async fn example(client: &AnytypeClient, space_id: &str) -> Result<(), AnytypeError> {
let count = client.count_archived(space_id).await?;
println!("archived before delete: {count}");

let deleted = client.delete_all_archived(space_id).await?;
println!("deleted archived objects: {deleted}");
# Ok(())
# }
```

## Files

Simple uploads, byte downloads, and deletion use REST. File listing, search,
metadata, preload, URL upload, and uploads with style/context options use gRPC.

```rust
let space_id = "space_id";
let file_id = "file_object_id";
let bytes = client.files().download_bytes(space_id, file_id).await?;
tokio::fs::write("/tmp/download", bytes).await?;
```

For image variants, byte ranges, cache validators, or response metadata, use
the configurable request API. It preserves `206 Partial Content`,
`304 Not Modified`, `412 Precondition Failed`, and `416 Range Not Satisfiable`
statuses for the caller to handle:

```rust
let response = client
    .files()
    .download_request(space_id, file_id)
    .width(640)
    .byte_range(0, 4096)
    .response_limit_bytes(4097)
    .error_limit_bytes(64 * 1024)
    .header_evidence_limit_bytes(4096)
    .max_attempts(6)
    .if_none_match("\"cached-etag\"")
    .download()
    .await?;

println!("status: {}, type: {:?}", response.status, response.metadata.content_type);
```

These controls are per request: they never widen or mutate the configured
global response limits. Successful GETs require one canonical `Content-Length`
that matches the buffered body. Partial responses additionally require one
canonical `Content-Range` consistent with the requested range and body.
`Content-Type`, `ETag`, `Last-Modified`, and `Accept-Ranges` are parsed and
validated; duplicates, non-UTF-8 values, contradictions, truncation, and
allowlisted header evidence over the selected ceiling fail with typed,
secret-safe errors. The header ceiling is checked independently before body or
retry processing on every physical response, including intermediate 429 and
retryable-status responses. The attempt ceiling is cumulative across 429,
retryable status, connection, and timeout replays.

Use `files().metadata(space_id, file_id)` for a simple `HEAD` request. File
deletion moves the object to the bin by default; permanent deletion is explicit:

```rust
client
    .files()
    .delete_request(space_id, file_id)
    .permanently()
    .delete()
    .await?;
```

`files().upload(space).from_path(path).upload()` selects REST for a simple
path upload and returns a normalized `FileObject`. Adding `file_type`, `style`,
`details`, or creation-context options selects the richer gRPC upload.

REST uploads can apply request-local ceilings without changing the client
configuration:

```rust
let file = client
    .files()
    .upload(space_id)
    .bytes("report.txt", b"bounded bytes".to_vec())
    .mime("text/plain")
    .multipart_limit_bytes(71_680)
    .response_limit_bytes(65_536)
    .error_limit_bytes(65_536)
    .upload()
    .await?;
```

The multipart ceiling includes the complete boundary and part headers and is
checked before authentication or network I/O. The successful and error-body
ceilings are independent, and the REST upload POST is sent at most once.

Call `resolve_space_id_bounded(reference, page_limit)` when a workflow needs a
request-local ceiling on every name-resolution page. Stable space IDs still
return without I/O; names retain the normal finite scan and ambiguity rules.

`files().preload(space)` accepts either `from_path(path)` or `from_url(url)` as
its source and always runs over gRPC, returning the preload file id.

## Attached Discussions (REST + gRPC)

Pages and notes can own one derived discussion object. This is not an ordinary
space chat: scope begins with the exact parent, and successful discovery proves
the derived object's space, discussion smart-block type, discussion layout, and
deterministic `discussion-<parent_id>` unique key.

```rust,no_run
use anytype::prelude::*;

# async fn example(client: &AnytypeClient) -> Result<(), AnytypeError> {
let current = client
    .attached_discussion("space_id", "parent_object_id")
    .get()
    .await?;

if current.discussion_id().is_none() {
    let attached = client
        .attached_discussion("space_id", "parent_object_id")
        .ensure()
        .await?;
    println!("{}", attached.discussion_id().unwrap_or_default());
}
# Ok(())
# }
```

`get` returns the closed `AttachedDiscussion::Absent` or
`AttachedDiscussion::Attached` state after a cache-independent REST parent
preflight and bounded gRPC reads. The exact REST wire requires an explicit
layout, and only Basic- and Note-layout parents are accepted. `ensure` reads
first and never calls the upstream attachment RPC for an already attached
parent. When absent, it dispatches at most one mutation and then rereads the
parent and independently verifies the derived discussion; transport errors,
malformed evidence, and an unconfirmed final state are not retried. Once
dispatch begins, reconciliation continues in an owned task even if the caller
cancels its future. Each gRPC call has a finite deadline capped at five seconds,
the whole operation has a caller-adjustable absolute deadline capped at thirty
seconds, and every show owns a separate bounded close.
The total budget reserves time for each owned close and, once a write is
admitted, for one fresh reconciliation read.

`AttachedDiscussionErrorKind` provides closed, payload-free classifications for
unsupported layouts, malformed identity evidence, RPC and operation deadlines,
cleanup failure, upstream failure, owned-task failure, and indeterminate
mutation outcomes. gRPC unauthenticated and permission-denied statuses remain
structural authentication errors without retaining status text. Use
`client.attached_discussion_metrics()` to inspect cumulative parent GET, show,
accepted-show, close, successful-close, write-dispatch, and reconciliation
counters.

## Chats

Space-scoped chat listing, creation, plain-message CRUD, lookup/search,
reactions, read state, and per-chat SSE streams use REST:

```rust
use futures::StreamExt;

let chats = client.chats().in_space("space_id");
let page = chats
    .list()
    .filter(Filter::text_contains("name", "team"))
    .limit(20)
    .list()
    .await?;
let message_id = chats
    .add_message("chat_id", MessageContent::new().bold("Hello"))
    .send()
    .await?;
let first_history = chats.older_messages("chat_id").limit(8).get().await?;
if let Some(before) = first_history.next_before {
    let older = chats
        .older_messages("chat_id")
        .before(before)
        .limit(8)
        .get()
        .await?;
    println!("{} older messages", older.messages.len());
}
let edit = chats
    .edit_message(
        "chat_id",
        &message_id,
        MessageContent::new().italic("Edited"),
    )
    .send_verified()
    .await?;
assert!(edit.after.modified_at > edit.before.modified_at);
let mut events = chats
    .message_stream("chat_id")
    .limit(20)
    .heartbeat_seconds(15)
    .open()
    .await?;
while let Some(event) = events.next().await {
    if let ChatHttpEvent::MessageAdded { message } = event? {
        println!("{}", message.content.text);
    }
}
```

Structured message blocks, full-fidelity reads, cross-chat previews, reconnect
watermarks, and dynamic subscription control remain available as gRPC
extensions because the REST representation omits blocks and per-user state.

Older REST history uses a typed page with a 1 through 12 item limit. Its
`next_before` value is an equality-only opaque server token limited to 256
ASCII graphic bytes. Pass it only to the next `older_messages` request; do not
parse or sort it. Each returned window preserves Heart's oldest-to-newest
order, while continuation moves to an older window. Message timestamps fail
closed when the server value is out of range and format canonically with UTC
millisecond precision through `canonical_chat_timestamp`. `send_verified`
performs GET, PATCH, and an independent GET and fails when the supported edit
does not strictly advance `modified_at`.

## Rich Chat Streaming (gRPC)

```rust
use anytype::prelude::*;
use futures::StreamExt;

// print chat messages as they arrive
async fn follow_chat(client: AnytypeClient, chat_obj_id: &str) -> Result<(), AnytypeError> {
    let ChatStreamHandle { mut events, .. } = client
        .chat_stream()
        .subscribe_chat(chat_obj_id)
        .build();

    while let Some(event) = events.next().await {
        if let ChatEvent::MessageAdded { chat_id, message } = event {
            println!("[{chat_id}] {}: {}", message.creator, message.content.text);
        }
    }
    Ok(())
}
```

## Body Blocks (gRPC)

The `body` module reads the rich body of an object (paragraphs, headings,
lists, callouts, tables, bookmarks, LaTeX/Mermaid/YouTube embeds) as a typed,
bounded tree with exact block IDs and exact child order:

```rust
use anytype::prelude::*;

async fn print_body(client: &AnytypeClient) -> Result<(), AnytypeError> {
    let snapshot = client.blocks().body("space_id", "object_id").fetch().await?;
    for block in snapshot.iter() {
        if let BlockContent::Text(text) = &block.content {
            println!("{:?}: {}", text.style, text.text);
        }
    }
    Ok(())
}
```

Reads are fail-closed: duplicate, cyclic, orphaned, dangling, oversized, or
malformed block graphs fail whole with a typed `AnytypeError::BodyGraph`
error — a partial or truncated tree is never returned. Per-request
`BodyLimits` can tighten (never widen) the hard ceilings on block count,
depth, fanout, text size, and mark count. Content the typed layer does not
model (dataviews, widgets, unknown styles or marks from newer servers) reads
as an explicit `Unsupported` marker carrying only a content-free structural
summary, so trees from newer hearts stay complete, ordered, and honest.
Every accepted `ObjectShow` is followed by a best-effort `ObjectClose`, even
when graph validation rejects the returned view; failed shows do not issue a
close request.

Mutations start from a snapshot and accept only typed constructors and targets
that belong to that snapshot. Every write is sent once, then a bounded fresh
`ObjectShow` read must prove the exact ID, rich state, and sibling/parent
position before success is returned:

```rust,no_run
use anytype::prelude::*;

async fn append_checked_item(
    client: &AnytypeClient,
    snapshot: &BodySnapshot,
) -> Result<BlockMutation, AnytypeError> {
    snapshot
        .edit(client)
        .append(NewBlock::checkbox("verified task", false)?)
        .await
}
```

`apply_all` is explicitly non-transactional: it returns verified receipts for
the completed prefix, the first failure, and the untouched suffix. Timeout,
transport uncertainty, or verification exhaustion returns
`BodyMutationIndeterminate` with the last complete snapshot when available;
callers must reread before retrying. Bookmark creation has an SSRF-safe policy:
it validates and stores an unfetched absolute HTTP(S) URL but never invokes the
server's URL-fetch RPC. YouTube embeds accept only canonicalizable HTTPS
`youtube.com`/`youtu.be` video URLs. Divider style and the complete link-card
appearance (card style, icon size, description mode, and bounded relation-key
list) are typed updates. System singleton, file, table-structural, unsupported,
and operation-restricted targets are rejected before dispatch.
That fail-closed anchor policy also applies to a sibling target's parent and
the existing first child used to encode a first-child insertion. Verified
table creation proves the canonical ordered columns/rows layout regions,
direct column and row membership, dimensions, and exact first-row header state;
aggregate descendant counts are never accepted as table evidence.

## Cache-independent Space Reads

Use `client.space(space_id).get_direct()` when an exact mutation preflight or
read-after-write check must bypass the process space cache. It performs one
scoped REST GET, rejects a response carrying a different space ID, and returns
the exact result without reading or priming the cache.

Property and tag mutation builders also provide `no_cache_refresh()`. The
default behavior continues to refresh a primed property cache, including all
tag pages for select properties. The cache-independent mode performs no hidden
tag reads after the write and instead invalidates that space's property cache;
use `property(...).get_direct()` and an explicitly limited `tags(...).limit(n)`
page for bounded semantic readback.

## Type Property Classification (REST + gRPC)

`Type.properties` is the REST server's flattened visible list: featured
properties appear before ordinary recommended properties, but the wire model
does not expose the boundary and may omit system-featured definitions. Do not
infer replaceability from list position or known property keys. Use the
source-backed classification read when preparing or verifying an exact type
property replacement:

```rust,no_run
use anytype::prelude::*;

# async fn example(client: &AnytypeClient) -> Result<(), AnytypeError> {
let properties = client
    .get_type("space_id", "type_id")
    .classify_properties()
    .await?;

for property in properties.replaceable() {
    println!("{} ({})", property.name, property.key);
}
# Ok(())
# }
```

The read does not inspect or prime the all-types or all-properties caches. It
combines one cache-independent REST type GET with one gRPC `ObjectShow` of the
same type and reconciles the REST definitions against Heart's authoritative
`recommendedFeaturedRelations` and `recommendedRelations` source lists. The
returned `recommended` list is the complete non-featured set replaced by
`UpdateTypeRequest::properties` and cleared by `clear_properties`.

`ObjectShow` and its exact matching `ObjectClose` both carry tonic deadlines
and outer timeouts. A close guard is armed before show dispatch; cancellation
or timeout during either boundary starts at most one detached five-second
close fallback. `classify_properties()` uses the five-second Show maximum,
while `classify_properties_with_deadline()` accepts a nonzero Show budget of at
most five seconds. Every explicit or detached close owns a fresh independent
five-second window, even when a caller's readback budget has expired. Public
counters expose Show, Close, fallback, and confirmed cleanup success/failure
work without retaining payloads. Cleanup failures take precedence over Show
response errors.

The source lists are capped at 1,000 combined links. Duplicate, overlapping,
malformed, missing, extra, or cross-source-inconsistent evidence fails the
whole read rather than truncating or guessing. The transports are not an
atomic snapshot, so a concurrent edit or eventual-consistency window may
require rereading. gRPC credentials are required. `featured_ids` preserves the
exact source list; `featured` contains only definitions visible on the REST
type. Hidden and file recommendation lists are separate Heart concepts and are
not part of this replaceable-property model.

## Members

List members with `client.members(space_id).list()` and read one exact member
with `client.member(space_id, member_id).get()`. The exact-read builder accepts
the REST API's object-shaped IDs, `_participant` IDs, and network identities;
the value must remain a URL-unreserved path segment of at most 256 bytes.

## Direct Collection Membership

Saved collection views can hide members through filters and pagination. Use
`observe_collection_membership` when a workflow needs bounded evidence about
one exact object in one exact manual collection:

```rust,no_run
use anytype::prelude::*;

# async fn example(client: &AnytypeClient) -> anytype::Result<()> {
let observation = client
    .observe_collection_membership("space-id", "collection-id", "object-id")
    .await?;
match observation.state {
    CollectionMembershipState::Present => println!("direct member"),
    CollectionMembershipState::Absent => println!("not a direct member"),
}
# Ok(())
# }
```

The read exact-checks the REST collection and object identities and rejects
Set/query lists. It runs an independent unscoped exact-object query before the
collection-scoped query; an absent result also requires the same unscoped proof
afterward. This control/scoped/control sequence prevents a transient missing
index row from being misreported as absence. Saved view filters and sorts are
never consulted. Each app-global Heart subscription has a unique client-owned
ID, a finite deadline, and cancellation-resilient bounded cleanup. Missing
counters, malformed identities, cleanup failures, or incomplete control
evidence return an error rather than `Absent`. After a mutation has been
dispatched, callers must treat every such error as an indeterminate mutation
outcome and perform a fresh read before deciding whether retry is safe.

Use `collection_member_add` when a workflow must add exactly one member and
classify a completed HTTP rejection conservatively:

```rust,no_run
use anytype::prelude::*;

# async fn example(client: &AnytypeClient) -> anytype::Result<()> {
match client
    .collection_member_add("space-id", "collection-id", "object-id")
    .await?
{
    CollectionMemberAddOutcome::Acknowledged => {}
    CollectionMemberAddOutcome::Rejected { status } => eprintln!("HTTP {status}"),
}
# Ok(())
# }
```

The method sends one POST attempt, never follows a redirect, and returns the
exact completed non-success status without reading or exposing its response
body. A transport failure, incomplete or oversized success response, or
malformed success body remains an error because it cannot prove whether the
server applied the mutation. `view_add_objects` remains the general
multi-object API and does not provide this status-preserving contract.

Use `collection_membership_page` to enumerate the same canonical direct
membership scope without consulting a selected or saved view:

```rust,no_run
use anytype::prelude::*;

# async fn example(client: &AnytypeClient) -> anytype::Result<()> {
let first = client
    .collection_membership_page("space-id", "collection-id", 20, None)
    .await?;
if let Some(next) = first.continuation {
    let second = client
        .collection_membership_page("space-id", "collection-id", 20, Some(next))
        .await?;
    println!("{} direct members so far", first.object_ids.len() + second.object_ids.len());
}
# Ok(())
# }
```

Public pages contain at most 61 validated 1..256-byte safe entity IDs in
Heart's direct collection order. Collection scopes ignore an `id` sort, so the
request carries no sort and the client preserves the returned order without
post-sorting. Each page performs one cache-independent logical HTTP
GET (one through six physical attempts through the shared no-seventh-send
pipeline), one non-replayed Heart subscribe, and one foreground unsubscribe;
an interrupted or failed cleanup can arm only one bounded drop fallback.
A continuation reads one private overlap row to prove its prior boundary and
total are unchanged, then discards that row. Real Heart offset windows report
the complete total while leaving both relative counters at zero, so checked
total/offset/row arithmetic determines whether another page exists. Changed
totals or boundaries, overlap-only results, malformed or nonzero relative
counters, unexpected dependencies, cleanup failure, and Set/query targets fail
closed instead of producing an empty or truncated page. Separate pages are not
snapshot-isolated; restart from the first page after concurrent membership
changes.

`client.collection_membership_metrics()` returns cumulative, payload-free
counters for validated direct-observer query phases, membership query rounds,
subscribe attempts, foreground close attempts and successes, fallback close
attempts, and collection add/remove dispatches. The observer count starts only
after the exact REST collection and object identities pass validation, so a
Set/query rejection can be distinguished from a canonical membership query.
Cloned clients share the same counters; the snapshot never retains collection,
object, or subscription identifiers.

## Status and Compatibility

The crate has 100% coverage of the Anytype REST api 2025-11-08.

Plus:

- View Layouts (grid, kanban, calendar, gallery, graph) implemented in the desktop app but not in the api spec 2025-11-08.

- gRPC back-end provides API extensions for features not available in the REST api:
  - File metadata, listing/search, preload, URL upload, and rich upload options.
  - Structured chat blocks, full-fidelity message reads, chat-object search,
    name resolution, cross-chat previews, and reconnecting subscriptions.
  - Exact featured versus replaceable type-property classification.

### Apis not covered

The current Anytype http backend api does not provide access to some data in Anytype vaults.

- ~~Files~~ *Update:* REST supports basic transfer; gRPC supplies richer file operations.
- ~~Chats and Messages~~ *Update:* REST supports chat management and plain message operations; gRPC supplies structured messages and richer streams.
- Blocks. Pages and other document-like objects can be exported as markdown, but markdown export is somewhat lossy, for example, in tables, markdown export preserves table layout, with bold and italic styling, but foreground and background colors are lost.
- Relationships - only a subset of relation types are available in the REST api.

## Keystore

A Keystore stores authentication tokens for http and grpc endpoints. Various implementations store keys in memory, on disk, or in the OS Keyring

More info about using and configuring keystores is in [Keystores](./Keystores.md)

## Known issues & Troubleshooting

See [Troubleshooting](./Troubleshooting.md)

For keystore-related issues, see [Keystores](./Keystores.md)

## Eventual Consistency

Anytype servers have "eventual consistency" (This is a feature of practical distributed systems, not a bug!). How you might encounter this in your programs:

- Create a new property and then immediately create a type with the property, and get an error that the property does not exist.
- Create a new type and then create an object with the type, and get an error that the type does not exist.
- Delete an object, then immediately search for it, and find it.

The amount of time needed for "settling" seems to be 1 second or less.

`anytype` can perform validation checks after creating objects (objects, types, properties, and spaces) to ensure they are present before `create()` returns. Since this verification can cause delays, it's opt-in. While there are some knobs you can tune to adjust backoff time and number of retries, the easiest way to add verification is to call `ensure_available()` before `create` for critical calls:

```rust,no_run
let obj = client.new_object("space_id", "page").name("Quick note").ensure_available().create().await?;
```

For mutation workflows that must confirm more than availability, use
`verify_semantic` with a predicate over a freshly fetched value. It retries
successful-but-stale values as well as transient not-found, transport, retry,
and server failures. Verification always has both a wall-clock deadline and a
validated nonzero attempt cap no larger than `MAX_VERIFY_ATTEMPTS`; legacy zero
and oversized values safely clamp to that hard ceiling, and zero-delay
configurations remain finite and cancellation-safe. Fetched values and upstream
error text are never retained in the terminal verification timeout.

To enable verification for *all* new objects, types, and properties, add `.ensure_available(VerifyConfig::default())` to the config when creating the client. Setting this in the client configuration is not recommended except for an environment like unit tests where you're hammering the server and need to get results immediately. If verification is enabled in the client config, it will be applied to all `create` calls, unless disabled on a per-call basis by using `.no_verify()`:

```rust,no_run
let obj = client.new_object("space_id", "page").name("Quicker note").no_verify().create().await?;
```

## Building

Requirements:

- protoc (from the protobuf package) in your PATH. On macos, `brew install protobuf`
- libgit2 in your library path.

```sh
cargo build
```

## Testing

Set environment flags for unit and integration tests. You'll also need a running anytype server (cli or desktop).

```sh
# HTTP endpoint. Default: http://127.0.0.1:31012
#    Headless cli default port is 31012. Desktop app uses port 31009
export ANYTYPE_URL=http://127.0.0.1:31012
# Set the same for ANYTYPE_TEST_URL
export ANYTYPE_TEST_URL=$ANYTYPE_URL
# optional: set keystore to custom path
export ANYTYPE_KEYSTORE=file:path=$HOME/.local/state/anytype-test-keys.db
# optional: set space id for testing. If not set, uses first space with "test" in the name
export ANYTYPE_TEST_SPACE_ID=
# optional: enable debug logging. Default "info"
export RUST_LOG=
# optional: disable rate limits. If not disabled, tests will take longer to run
export ANYTYPE_DISABLE_RATE_LIMIT=1
```

Keystore modifiers use `:key=value` boundaries. Path values may contain a
Windows drive colon or ordinary colons that are not followed by another
modifier key and `=`.

`ANYTYPE_KEYSTORE=env` is also supported for the test process when its HTTP
and optional gRPC credentials are already present in the environment.
Unauthenticated control tests explicitly use unique empty temporary file
keystores, so ambient `env` credentials cannot change their expected result.

Run smoke test

```sh
cargo test --test smoke_test -- --nocapture
```

Run all tests

```shell
cargo test -- --nocapture
```

When the real server's mutation rate limit remains enabled, use
`cargo test -- --test-threads=1` to keep the full live suite from flooding its
shared endpoint. Pagination coverage owns a uniquely filtered, cleanup-tracked
object cohort and does not depend on unrelated ambient-space objects.
Space-creation requests validate a nonempty bounded name before HTTP; validation
coverage never probes this rule by creating an untracked unnamed space.
Empty-filter coverage likewise owns its expected object rather than depending
on pre-existing content in the configured test space.

Integration tests require a running Anytype server and environment variables. See `src/client.rs` for details.

The crate no longer ships a semantic gRPC mock server. Successful gRPC
behavior is covered with cleanup-owned resources against the configured real
Anytype server. Protocol and reducer edge cases use scripted transport handlers
or constructed values without pretending to implement Anytype semantics.
Disconnect, latency, and other connection-fault scenarios require the reviewed
external fault-injection harness and are not emulated by an in-process gRPC
service.

Chat resolver integration tests create cleanup-owned chats and messages in a
fresh prefix-authorized space on the configured real HTTP and gRPC endpoints.
Supporting REST reads and the REST SSE test use the same disposable tier so the
resolver and stream files remain runnable when the server has no ambient
spaces. Broader pre-existing REST CRUD, search, reaction, and read-state cases
remain in the ambient `test_chats` tier and are not part of the mock migration.
Every created message is registered immediately, before stream waits or
assertions, and the gRPC stream worker is shut down before teardown.

Body reader integration tests create cleanup-owned objects in a fresh
prefix-authorized space on the configured real HTTP endpoint, then verify typed
reads, ordering, close-safe repeat reads, tightened limits, and missing-object
failures through the configured gRPC endpoint. The adjacent dataview test was
not formerly mock-backed, but shares the disposable tier so the body test file
does not require ambient inventory.

The body, chat-discovery, chat-prerequisites, and chat-stream files are ignored in ordinary test
runs. Run each in its own admitted, single-threaded process; do not run these
commands concurrently:

```sh
source .test-env
ANYTYPE_DISPOSABLE_TEST_PROCESS=1 cargo test -p anytype --test test_body -- --ignored --test-threads=1 --nocapture
ANYTYPE_DISPOSABLE_TEST_PROCESS=1 cargo test -p anytype --test test_chat_discovery -- --ignored --test-threads=1 --nocapture
ANYTYPE_DISPOSABLE_TEST_PROCESS=1 cargo test -p anytype --test test_chat_prerequisites -- --ignored --test-threads=1 --nocapture
ANYTYPE_DISPOSABLE_TEST_PROCESS=1 cargo test -p anytype --test test_chat_stream -- --ignored --test-threads=1 --nocapture
ANYTYPE_DISPOSABLE_TEST_PROCESS=1 cargo test -p anytype --test test_kanban_fixture -- --ignored --test-threads=1 --nocapture
ANYTYPE_DISPOSABLE_TEST_PROCESS=1 cargo test -p anytype --test test_process_watcher watcher_completes_on_real_import_finish_fallback -- --ignored --exact --test-threads=1 --nocapture
```

Process watcher import-finish coverage uses a real Markdown import in the fresh
cleanup-owned space created by `with_disposable_space_context`. The watcher
subscribes and unsubscribes from the configured gRPC server, accepts empty-space
fallback events only for import requests that explicitly enable the fallback,
and applies fixed timeouts to every live stage. The test is ignored under
ordinary runs because it requires a configured real server and explicit
disposable-process admission. The real server may complete the ordinary import
process before publishing the import-finish event; the test uses the same
subscription for a second bounded wait and proves that no new process was
correlated while observing that fallback.

Tests that need a custom collection can use the hidden
`TestContext::create_collection_type_fixture` helper. Anytype's REST type
create/update contract rejects collection layout, so this test-only helper uses
the narrow heart RPC, registers the returned type for cleanup before any
follow-up read, and verifies it through the ordinary scoped REST getter.

Tests must create the object through
`TestContext::create_collection_fixture`; ordinary cleanup registration does
not grant view-mutation authority. This helper accepts only a collection type
owned by the context, takes a complete type-scoped pre-create snapshot, and
atomically records its cleanup dispatch and exact `(space, object, type)`
provenance. Any collision with an authoritative cleanup ID or existing private
claim is rejected without changing either registry.
`TestContext::create_collection_view_fixture` then requires that provenance,
requires the REST object to retain the exact proven type ID, and cross-checks
every REST-visible default-view field against the exact
`ObjectShow` root and `dataview` block, clones the full proto, and issues one
`BlockDataviewViewCreate` RPC. It requires exactly one matching view-set event,
a distinct server-assigned ID, and complete nested-view equality before a
finite exact two-view REST verification. Collection teardown owns the added
view; there is no general view-create production API.
`TestContext::add_collection_name_filter_fixture` may then add one exact-name
filter only to that cleanup-owned view. It accepts initially unfiltered REST
and `ObjectShow` evidence, sends one authenticated filter-add RPC, and requires
the assigned filter ID and complete value to reread identically through both
surfaces. Collection teardown owns the filter with the view; this remains test
infrastructure, not a production view-filter API.

Representative Kanban tests can use `TestContext::create_kanban_fixture` inside
`with_disposable_space_context`. The helper creates and immediately registers a
custom card type, its Select grouping property and two status options, a
collection, an existing server view converted to Kanban, and three cards. It
adds the grouping relation through heart before setting the layout, rejects
pre-existing filters, resolves Heart's internal relation key separately from
the REST property key, and independently rereads the exact relation format,
view grouping key, tags, membership, and card values. Membership verification
uses two-item pages so pagination is exercised rather than bypassed.
`move_kanban_item_fixture` performs an ordinary object Select-property update
and requires the moved card and complete board to reread exactly. Missing or
wrong-format relations, removed options, filtered views, malformed pagination,
or unregistered resources fail closed. Collection deletion owns view cleanup;
property cleanup owns its options.

Tests that need disposable spaces should use
`TestContext::create_space_fixture`. It creates through the authenticated REST
API after taking a complete bounded inventory whose pagination, IDs, names, and
uniqueness are validated. A response is registered exactly once only when its
valid ID was absent from that inventory, its name exactly matches the request,
and it is a regular space distinct from the context space. The private registry
retains that exact ID/name provenance. Untrusted or ambiguous responses are
allowed to leak rather than authorize deletion of ambient state. Registration
occurs before follow-up verification. Teardown revalidates exact ID/name/model
provenance through the same strict inventory before Anytype's irreversible
`SpaceDelete` RPC, then requires complete bounded REST evidence that the ID is
gone even when the delete response is uncertain. There is intentionally no
general test registration or production space-delete API.

Whole live suites should prefer `with_disposable_space_context`. It creates a
fresh cleanup-owned space under the mandatory `ANYTYPE_TEST_SPACE_PREFIX`.
That ASCII prefix is an explicit authorization to delete **every** space whose
current name starts with it, case-insensitively; reserve it exclusively for
tests. Missing or invalid configuration returns a typed `DisposableRun::Skipped`
before credential access or filesystem I/O. One same-host backend-wide file
lease serializes participating runs. An owner-private durable ledger and
disk-backed enumerate-before-delete offset-pagination plans recover interrupted
matching runs without a count ceiling or an in-memory inventory. Each fixed
pagination window shares one deadline; a changing total discards the plan and
restarts at offset zero. New names use 128 bits of operating-system randomness.
Readiness, the two immediate
pre-delete checks, and final absence proof all use cache-disabled direct exact-ID
reads. The helper cleans registered children first and retains callback,
cleanup, deletion, absence, ledger, and panic outcomes; an unproven absence is
always dominant without discarding the original typed error or simultaneous
cleanup evidence. Remote backends require an equivalent scheduler lease and are
otherwise rejected. Operators must not create, rename, or delete spaces through
another client while the helper holds its lease.
Disposable runs require `ANYTYPE_KEYSTORE=env`, an explicit
`ANYTYPE_KEYSTORE_SERVICE`, a nonempty `ANYTYPE_KEY_HTTP_TOKEN`, and at least
one nonempty gRPC session token or account key. They must run in a dedicated
single-threaded integration-test process admitted with
`ANYTYPE_DISPOSABLE_TEST_PROCESS=1`; the process must not mutate its environment.
File, keyring, implicit, unknown, malformed, and over-budget credential forms
skip before authentication, private state, or mutation. The helper creates no
credential file. For a spawned production child, call
`ctx.disposable_child_environment().unwrap().configure(&mut command)` before
spawn, then register an idempotent stop-and-wait handle with
`ctx.spawn_owned_child(...)`. Configuration uses `env_clear`, reconstructs only
the approved endpoints, finite limits, MCP settings, and exact accepted
credential names, and rechecks the whole environment/argument block budget.
The helper records child-running state before invoking the spawn closure and
stops all registered children before resource cleanup and space deletion.
Recovery refuses every cleanup plan and prefix sweep while a prior ledger says
its child may still run. The first refusal durably records that the operator
must prove the child stopped or is gone. Only after that proof may the operator
set `ANYTYPE_DISPOSABLE_RECOVER_STOPPED_RUN` to the exact recorded `.json` run
handle for one invocation; the helper persists the stopped transition before
applying that ledger's plan, and rejects stale or repeated confirmations.
Destructive execution is enabled only where owner and owner-only permissions
can be proved for the runtime directory and every recovery target. Unix opens
and removes exact components relative to verified directory handles with
no-follow semantics. Windows fails closed before authentication until native
DACL, ownership, and reparse-point verification is implemented.

Tests that need templates can use the hidden
`TestContext::create_template_fixtures` helper with one to sixteen source
names. It creates a private custom type and source object for each requested
template, invokes the authenticated heart template-from-object RPC exactly once
per source, and verifies the returned IDs through a finite complete type-scoped
list plus exact GETs. Complete bounded type, space-wide active/archived object,
and global template inventories prove create responses did not reuse
pre-existing IDs; the global inventory also proves the new template is owned
only by the expected type, while list and GET generic-template identities must
agree. The helper registers every created ID before classifying
the RPC response or reading it back. Teardown issues each template, source, and
type archive request once in reverse dependency order, then proves the
templates absent and the sources and type archived. Production consumers do
not gain a template mutation API.

## License

Apache License, Version 2.0

## Contributing

Feedback, Issues and Pull Requests are welcome.
