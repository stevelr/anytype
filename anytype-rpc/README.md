# Anytype gRPC client

[![release](https://img.shields.io/github/v/tag/stevelr/anytype?sort=semver&filter=anytype-rpc-v*&label=release)](https://github.com/stevelr/anytype/releases?q=anytype-rpc-v&expanded=true)
[![docs.rs](https://img.shields.io/docsrs/anytype-rpc?label=docs.rs)](https://docs.rs/anytype-rpc)
[![crates.io](https://img.shields.io/crates/v/anytype-rpc.svg)](https://crates.io/crates/anytype-rpc)

The gRPC api isn't officially supported (by Anytype) for third party clients. However, it's used heavily by Anytype applications, including the desktop app and headless cli, and it's the only way for applications to access certain functionality that is not available over the HTTP api, such as Files, Chats, Blocks, and Relations.

Before using this crate, check whether [anytype](https://crates.io/crates/anytype) and its `AnytypeClient` interface meets your needs.

`AnytypeClient`'s api:

- has a more ergonomic, hand-designed api surface.
- uses HTTP/REST for most (>90%) apis, as recommended by the Anytype team, only using gRPC backend (through this crate) to fill gaps.
- will be more stable across releases. This crate's api is automatically generated from the anytype-heart protobuf definitions.

## Compatibility

| anytype-rpc version | anytype-heart version |
| ------------------- | --------------------- |
| 0.5.0               | 0.50.10               |
| 0.3.0 – 0.3.1       | 0.48.0                |
| 0.2.1               | 0.44                  |

## Related projects

- [anytype](https://crates.io/crates/anytype) An ergonomic Anytype API client in Rust. Includes http rest api plus gRPC backend using this crate, for access to Files and Chats.

- [anyr](https://crates.io/crates/anyr) a CLI tool for listing, searching, and performing CRUD operations on anytype objects. via `anytype`, also includes operations on Files and Chats.

## Building

`config::load_headless_config` reads the account fields used for gRPC
authentication from an explicit path or the default
`~/.anytype/config.json`. It returns `None` only when the file is absent;
unreadable and malformed files are errors.

For normal builds, you need a rust toolchain. `protoc` is not required, as the crate ships with generated Rust sources in `src/gen`.

```
cargo build
```

To regenerate `src/gen` from anytype-heart's protobuf files, you need

- `protoc` (from the protobuf package)
- `just` (to run the justfile recipe)
- `curl`, `tar` and `bash`

```
just gen-protos
```

By default, this uses the `develop` branch. You can also pull from a specific git branch, tag, or commit:

```
just gen-protos ref=develop
just gen-protos ref=0.50.10
just gen-protos ref=abcdef123
```

Each generated source records the UTC generation date and the requested ref.
An unprefixed release version such as `0.50.10` resolves the corresponding
Anytype Heart `v0.50.10` tag.

## License

Apache License, Version 2.0
