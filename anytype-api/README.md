# anytype

An ergonomic Anytype API client in Rust.

[![release](https://img.shields.io/github/v/tag/stevelr/anytype?sort=semver&filter=anytype-v*&label=release)](https://github.com/stevelr/anytype/releases?q=anytype-v&expanded=true)
[![docs.rs](https://img.shields.io/docsrs/anytype?label=docs.rs)](https://docs.rs/anytype)
[![crates.io](https://img.shields.io/crates/v/anytype.svg)](https://crates.io/crates/anytype)

**[Home](https://github.com/stevelr/anytype) &nbsp; | &nbsp; [Documentation](https://docs.rs/anytype) &nbsp; | &nbsp; [Examples](https://github.com/stevelr/anytype/blob/main/anytype-api/examples/)**

## Overview

`anytype` provides an ergonomic rust client for [Anytype](https://anytype.io). It supports listing, searches, and CRUD operations on Objects, Properties, Spaces, Tags, Types, Members, Views, Files, and Chats, with optional key storage and caching. REST is preferred when it has equivalent functionality; gRPC supplies richer file metadata/upload options, structured chat messages, and event streaming.

Applications authenticate with Anytype servers using access tokens. One token is required for http apis, and if gRPC apis are used (for files or chats), an additional gRPC token is required. The `anytype` library helps generate tokens and store them in a KeyStore.

### Features

- 100% coverage of Anytype API 2025-11-08
- gRPC back-end provides extensions not available in REST (rich file operations, structured chat blocks, and full event streaming)
- Paginated responses and async Streams
- Integrates with OS Keyring for secure storage of credentials (HTTP + gRPC)
- Http middleware with debug logging, retries, and rate limit handling
- Client-side caching (spaces, properties, types)
- Name and id resolution helpers (`resolve` module): accept a space, type,
  chat, view, or property by name, key, or id; ambiguous names return up to 10
  deterministic candidate ids and display names for an actionable retry, and
  bounded scans fail explicitly instead of guessing from partial results;
  candidate ordering is independent of upstream page order
- Nested filter expression builder
- Parameter validation
- Metrics
- used in:
  - [anyr](https://github.com/stevelr/anytype/tree/main/anyr) - list, search, and manipulate anytype objects
  - [any-edit](https://github.com/stevelr/anytype/tree/main/any-edit) - edit anytype docs in markdown in external editor

### Bounded HTTP responses

Buffered REST responses have finite byte ceilings. Ordinary JSON defaults to
8 MiB, single-object/document JSON to 64 MiB, bounded error bodies to 64 KiB,
and raw file downloads to a separate 256 MiB policy. Truthful oversized
`Content-Length` responses are rejected before their body is read; responses
without a usable length are stopped at the first byte over the ceiling. SSE
chat events remain incremental streams and are not buffered as JSON.

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
10 MiB outgoing markdown body. The hard maxima are 64 MiB for ordinary and document JSON, 1 MiB for error
bodies, and 1 GiB for raw files. `AnytypeError::ResponseTooLarge` contains only
the selected ceiling and optional declared length; it never retains a response
body, URL, request payload, or credential.

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

See the [Examples](./examples/README.md) folder for more code samples.

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
    .range("bytes=0-4095")
    .if_none_match("\"cached-etag\"")
    .download()
    .await?;

println!("status: {}, type: {:?}", response.status, response.metadata.content_type);
```

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

## Status and Compatibility

The crate has 100% coverage of the Anytype REST api 2025-11-08.

Plus:

- View Layouts (grid, kanban, calendar, gallery, graph) implemented in the desktop app but not in the api spec 2025-11-08.

- gRPC back-end provides API extensions for features not available in the REST api:
  - File metadata, listing/search, preload, URL upload, and rich upload options.
  - Structured chat blocks, full-fidelity message reads, chat-object search,
    name resolution, cross-chat previews, and reconnecting subscriptions.

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

Run smoke test

```sh
cargo test --test smoke_test -- --nocapture
```

Run all tests

```shell
cargo test -- --nocapture
```

Integration tests require a running Anytype server and environment variables. See `src/client.rs` for details.

## License

Apache License, Version 2.0

## Contributing

Feedback, Issues and Pull Requests are welcome.
