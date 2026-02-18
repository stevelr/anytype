# Anytype gRPC client

[![release](https://img.shields.io/github/v/tag/stevelr/anytype?sort=semver&filter=anytype-rpc-v*&label=release)](https://github.com/stevelr/anytype/releases?q=anytype-rpc-v&expanded=true)
[![docs.rs](https://img.shields.io/docsrs/anytype-rpc?label=docs.rs)](https://docs.rs/anytype-rpc)
[![crates.io](https://img.shields.io/crates/v/anytype-rpc.svg)](https://crates.io/crates/anytype-rpc)

The gRPC api isn't officially supported (by Anytype) for third party clients. However, it's used heavily by Anytype applications, including the desktop app and headless cli, and it's the only way for applications to access certain functionality that is not available over the HTTP api, such as Files, Chats, Blocks, and Relations.

## Status and plan

- This crate is a dependency of [anytype](https://crates.io/crates/anytype), which requires that this crate is maintained and kept up to date.

- We will try to follow semver versioning policy, but if you plan to use this crate directly for a production release, we recommend you pin the version of this crate in Cargo.toml and check for updates periodically with `cargo outdated`.

- This crate includes some limited cli examples to list spaces and import and export objects.

## Compatibility

| anytype-rpc version       | anytype-heart version |
| ------------------------- | --------------------- |
| 0.3.0-beta.1 (unreleased) | 0.48.0-rc.2           |
| 0.2.1                     | 0.44                  |

## Related projects

- [anytype](https://crates.io/crates/anytype) An ergonomic Anytype API client in Rust. Includes http rest api plus gRPC backend using this crate, for access to Files and Chats.

- [anyr](https://crates.io/crates/anyr) a CLI tool for listing, searching, and performing CRUD operations on anytype objects. via `anytype`, also includes operations on Files and Chats.

## Building

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
just gen-protos ref=abcdef123
```

## License

Apache License, Version 2.0
