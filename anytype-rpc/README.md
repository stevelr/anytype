# Anytype gRPC client

The gRPC api is subject to change and isn't officially supported for third party clients.

This crate is experimental. If you need to use gRPC because some functionality isn't available in the
[anytype](https://crates.io/crates/anytype) REST api, this crate may help.

A very limited cli can list spaces and import and export objects.

## Recommended

- [anytype](https://crates.io/crates/anytype) a supported Anytype client that uses Anytype's official REST API.

- [anyr](https://crates.io/crates/anyr) a CLI tool for listing, searching, and performing CRUD operations on anytype objects.

## Building

Ensure you have 'protoc' from the protobuf package in your path. On macos, 'brew install protobuf'

Uses [tonic-prost-build](https://crates.io/crates/tonic-prost-build)

## License

Apache License, Version 2.0
