# anytype

An ergonomic Anytype API client in Rust.

**[Home](https://github.com/stevelr/anytype) &nbsp; | &nbsp; [Documentation](https://docs.rs/anytype) &nbsp; | &nbsp; [Examples](https://github.com/stevelr/anytype/blob/main/anytype-api/examples/)**

## Overview

`anytype` provides an ergonomic rust client for [Anytype](https://anytype.io). It supports listing, searches, and CRUD operations on Objects, Properties, Spaces, Tags, Types, Members, and Views, with optional key storage and caching. gRPC extensions (enabled by default) add file operations (upload/download/list/search).

### Features

- 100% coverage of Anytype API 2025-11-08
- Optional gRPC back-end provides API extensions for features not available in the REST api (Files)
- Paginated responses and async Streams
- Integrates with OS Keyring for secure storage of credentials (HTTP + gRPC)
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
    let config = ClientConfig::default().app_name("my-app");
    let client = AnytypeClient::with_config(config)?;
    if !client.auth_status()?.http.is_authenticated() {
        // prompt user for auth code if needed
        client
            .authenticate_interactive(
                |challenge_id| {
                    use std::io::{self, Write};
                    println!("Challenge ID: {challenge_id}");
                    print!("Enter 4-digit code: ");
                    io::stdout().flush().map_err(|err| AnytypeError::Auth {
                        message: err.to_string(),
                    })?;
                    let mut code = String::new();
                    io::stdin().read_line(&mut code).map_err(|err| AnytypeError::Auth {
                        message: err.to_string(),
                    })?;
                    Ok(code.trim().to_string())
                },
                false,
            )
            .await?;
    }

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

See the [Examples](./examples/README.md) folder for more code samples.

## Files (gRPC)

File operations require the `grpc` feature (enabled by default).

```rust
let file_id = "file_object_id";
let path = client
    .files()
    .download(file_id)
    .to_dir("/tmp")
    .download()
    .await?;
println!("downloaded to {}", path.display());
```

## Status and Compatibility

The crate has 100% coverage of the Anytype REST api 2025-11-08.

Plus:

- View Layouts (grid, kanban, calendar, gallery, graph) implemented in the desktop app but not in the api spec 2025-11-08.

- gRPC back-end provides API extensions for features not available in the REST api:
  - Files api for listings, search, upload, and download.

### What's missing?

The current version of the http backend api does not provide access to some data stored by the Anytype app. Data that is current inaccessible from the http api:

- ~~Files~~ _Update:_ Files support now available with the gRPC back-end
- Blocks. Pages and other document-like objects can be exported as markdown, but markdown export is somewhat lossy, for example, in tables, markdown export preserves table layout, with bold and italic styling, but foreground and background colors are lost.
- Relationships - only a subset of relation types are available in the REST api.
- Chats and Messages

## Keystore (Advanced topic)

You don't need to read all this to use `anytype`, but it needs to be documented somewhere.

> **TL;DR**:
> (1) First, try the defaults - it should "just work".
> (2). If you're doing development and/or don't want to deal with pop-up approval prompts, set
>
> ```
> export ANYTYPE_KEYSTORE=file
> export ANYTYPE_KEYSTORE_SERVICE=anyr
> ```
>
> (3) If you're using the gRPC backend, use the headless cli server and [init-cli-keys.sh](../scripts/init-cli-keys.sh)

Authentication tokens for http and gRPC are stored in a KeyStore, which can be an OS-managed keyring or a file-based keystore. The file-based keystore is an sqlite file (via turso, a rust-native sqlite implementation), with optional encryption. The keystore is selected in `ClientConfig::keystore`, when constructing an `AnytypeClient`, or in the environment variable `ANYTYPE_KEYSTORE`. These both use the same string format to specify the implementation and settings:

- If not specified, in config or the environment, the platform default keystore is used (usually the secure OS keyring). On linux, the default is kernel keyutils.
- The first word of the keystore spec is the name of the implementation, either "file", or one of the OS keystores [in the keyring crate](https://github.com/open-source-cooperative/keyring-rs/blob/main/src/lib.rs).
- The name may be followed by a ':' (colon) and one or more key=value settings (called 'modifiers'), separated by colons.

The "file" keystore modifiers are documented in the README for [db-keystore](https://docs.rs/db-keystore/latest/db_keystore/). The most common option is "path" which sets the path to the db file. File keystore supports on-disk encryption by setting `cipher` and `hexkey`.

Examples:

- `--keystore file` use file (sqlite) keystore in the default location
- `--keystore file:path=/path/to/my/file.db"` use file (sqlite) keystore in custom location
- `--keystore file:path=/path/to/my/file.db:cipher=aegis256:hexkey=HEX_KEY` file keystore in custom location with 256-bit encryption with the hex key (64 hex digits)
- `--keystore secret-service` on linux, use the dbus-based secret service keystore
- `--keystore keyutils` linux kernel keyutils keystore (default on linux)
- `--keystore keyring` macos user keyring (default on macos)
- `--keystore windows` Windows Credential Store (default on windows)

### Key schema and service scope

All keystore implementations (file, macos keychain, linux keyutils, etc.) store keys as a triple (service, user, secret), using the terminology of the [keyring](https://crates.io/crates/keyring) crate. "service" is usually used for the application name, such as "anyr" or "any-edit". It's displayed to the user in permission prompts when requesting access. The "user" field is used to store the name of the key: "http_token", "account_key", or "session_token". "secret" is the actual key, stored as bytes, although all secrets used by `anytype` are valid utf8 strings.

In some of the example programs in the `anytype` crate, the service name is set to `anyr`, so it can use the same http token that anyr does. `anyr` makes it easy to generate an http token with (`anyr auth login`), which is saved to the keystore. The "service" name used to retrieve keys from the keystore is, by default, derived from `ClientConfig::app_name` to make it unique for every app, but it can be customized by setting `ClientConfig::keystore_service`, or by setting the environment variable `ANYTYPE_KEYSTORE_SERVICE`. A developer can choose to let a collection of app share auth tokens, as we did in the examples, by using a common service name.

### Adding keys to the keystore

For authenticating with the desktop app, call `authenticate_interactive`, which causes the app to display a 4-digit code that the the user must enter in the console to generate an http token, which is then stored in the keystore.

For gRPC authentication, it is recommended to use the headless cli server. To add a key to a keystore for use with the cli, use the `anyr` tool. (`anyr auth set-http`, `anyr auth set-grpc`, etc.) See [anyr](https://github.com/stevelr/anytype/tree/main/anyr) for details.

See the script ../scripts/init-cli-keys.sh for help initializing the cli and saving gRPC and http authentication tokens to the keystore.

### Encryption

Enable encryption on the file keystore in one of the following ways:

- (in code) set `EncryptionOpts::cipher` and `EncryptionOpts::hexkey`
- (with anyr cli) use `--keystore file:cipher=aegis256:hexkey=HEXKEY`
- (in environment) `ANYTYPE_KEYSTORE=file:cipher=aegis256:hexkey=HEXKEY`

Supported ciphers include `aegis256` (recommended for most uses) and `aes256gcm`. See [Turso Database Encryption](https://docs.turso.tech/tursodb/encryption) for more info and options. For a 256-bit key, hexkey is 64 hex digits. A key can be generated with `openssl rand -hex 32`

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

## Building

Requirements:

- protoc - (from the protobuf package. On macos, `brew install protobuf`)
- libgit2

```sh
cargo build
```

## Testing

Set environment flags for unit and integration tests. You'll also need a running anytype server (cli or desktop).

```sh
# optional: HTTP endpoint. Default: http://127.0.0.1:31012
#    Headless cli default port is 31012. Desktop app uses port 31009
export ANYTYPE_TEST_URL=
# optional: path to file-based keystore.
#    Default: $XDG_STATE_HOME/anytype-test-keys.db or $HOME/.local/state/anytype-test-keys.db
export ANYTYPE_TEST_KEY_FILE=
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
