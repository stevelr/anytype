# anytype

An ergonomic Anytype API client in Rust.

**[Home](https://github.com/stevelr/anytype) &nbsp; | &nbsp; [Documentation](https://docs.rs/anytype) &nbsp; | &nbsp; [Examples](https://github.com/stevelr/anytype/blob/main/anytype-api/examples/)**

## Overview

`anytype` provides an ergonomic rust client for the [Anytype](https://anytype.io) REST API. It supports listing, searches, and CRUD operations on Objects, Properties, Spaces, Tags, Types, Members, and Views, with optional key storage and caching.

### Features

- 100% coverage of Anytype API 2025-11-08
- Paginated responses and async Streams
- Integrates with OS Keyring for secure storage of api key (optional)
- Http middleware with debug logging, retries, and rate limit handling
- Client-side caching (spaces, properties, types)
- Nested filter expression builder
- Parameter validation
- Metrics
- used in:
  - [anyr](https://github.com/stevelr/anytype/tree/main/anyr) - list, search, and manipulate anytype objects
  - [any-edit](https://github.com/stevelr/anytype/tree/main/any-edit) - edit anytype docs in markdown in external editor

## Quick start

```rust
use anytype::prelude::*;

#[tokio::main]
async fn main() -> Result<(), AnytypeError> {

    // Create a client
    let client = AnytypeClient::new("my-app")?
        .set_key_store(KeyStoreFile::new("my-app")?);
    client.load_key(false)?;

    // List spaces
    let spaces = client.spaces().list().await?;
    for space in spaces.iter() {
        println!("{}", &space.name);
    }

    // get the first space
    let space1 = spaces.iter().next().unwrap();
    // Create an object
    let obj = client.new_object(&space1.id, "page")
        .name("My Document")
        .body("# Hello World")
        .create().await?;
    println!("Created object: {}", obj.id);

    // Search, with filtering and sorting
    let results = client.search_in(&space1.id)
        .text("meeting notes")
        .types(["page", "note"])
        .sort_desc("last_modified_date")
        .limit(10)
        .execute().await?;
    for doc in results.iter() {
        println!("{} {}",
            doc.get_property_date("last_modified_date").unwrap_or_default(),
            doc.name.as_deref().unwrap_or("(unnamed)"));
    }

    // delete object
    client.object(&space1.id, &obj.id).delete().await?;
    Ok(())
}
```

## Status and Compatibility

The crate has 100% coverage of the Anytype REST api 2025-11-08.

Plus:

- View Layouts (grid, kanban, calendar, gallery, graph) implemented in the desktop app but not in the api spec 2025-11-08.

### What's missing?

The current version of the backend api does not provide access to some data stored by the Anytype app. Data that is current inaccessible from the http api:

- Blocks. Pages and other document-like objects can be exported as markdown, but markdown export is somewhat lossy, for example, in tables, markdown export preserves table layout, with bold and italic styling, but foreground and background colors are lost.
- Files (images, videos, etc.)
- Relationships - only a subset of relation types are available in the REST api.
- chats and messages

Because of these limitations, it is not yet possible with this crate or with [anyr](../anyr) to export a complete space. We are investigating using the gRPC api backend to access some of these additional features.

## Known issues & Troubleshooting

See [Troubleshooting](./Troubleshooting.md)

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

To enable verification for _all_ new objects, types, and properties, add `.ensure_available(VerifyConfig::default())` to the config when creating the client. Setting this in the client configuration is not recommended except for an environment like unit tests where you're hammering the server and need to get results immediately. If verification is enabled in the client config, it will be applied to all `create` calls, unless disabled on a per-call basis by using `.no_verify()`:

```rust,no_run
let obj = client.new_object("space_id", "page").name("Quicker note").no_verify().create().await?;
```

## Testing

Set environment flags for unit and integration tests. You'll also need a running anytype server (cli or desktop).

```sh
# headless cli uses port 31012. desktop port 31009
export ANYTYPE_TEST_URL=http://127.0.0.1:31012
# path to file containing api key
export ANYTYPE_TEST_KEY_FILE=$HOME/.config/anytype/api.key
# optional: set space id for testing. If not set, uses first space with "test" in the name
export ANYTYPE_TEST_SPACE_ID=
# optional: enable debug logging
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

Licensed under either of:

- Apache License, Version 2.0 (`LICENSE-APACHE`)
- MIT License (`LICENSE-MIT`)

## Contributing

Feedback, Issues and Pull Requests are welcome.
