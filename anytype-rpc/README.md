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

## Logical deadlines

`AnytypeGrpcClient` applies a resolved `GrpcTimeoutPolicy` to generated RPCs.
The default boundaries are 120 seconds for credential setup and ordinary
unary calls, 30 minutes for long imports, exports, uploads, and downloads, 120
seconds through streaming response headers, and five seconds for cleanup.
Established-stream idle and total-lifetime boundaries are disabled by default.

Policy resolution has three modes:

- An explicit `AnytypeGrpcConfig::grpc_timeouts` policy wins and ignores the
  process environment. Each `None` field disables that boundary.
- Without an explicit policy, `ANYTYPE_GRPC_TIMEOUT_SECS=1..3600` gives
  credential, ordinary, long, and stream-setup calls one inherited value; `0`
  disables those four boundaries. In either case, established-stream idle and
  lifetime remain disabled and cleanup retains its five-second default.
- Without either setting, the defaults above apply.

Finite credential, ordinary, stream-setup, idle, and lifetime values must be
one through 3,600 seconds. Long unary values may be as high as 7,200 seconds;
cleanup may be at most 30 seconds. The environment value must be an exact
ASCII decimal with no sign, whitespace, or leading zero. Invalid policy or
environment values fail before connection activity.

Generated methods are classified as credential, ordinary, long, stream setup,
or cleanup operations. An unknown generated RPC is conservatively treated as
an ordinary mutation. Reads that expire are reported as aborted, mutations
that may have reached Heart are indeterminate, and established-stream expiry
terminates the stream. Inspect the typed `GrpcDeadlineError` class, outcome,
source, and elapsed time. A transport failure is consumed after conversion: the
wrapper retains its error-type marker and closed tonic status code, but not the
original error value or payload. Standard source traversal returns a synthetic
status containing that code and fixed redacted text.

The effective boundary is the earliest of the selected policy duration, an
absolute `GrpcEnclosingDeadline`, and an existing, tighter `grpc-timeout`. It
includes service readiness, and the remaining budget is propagated after
readiness. `StreamSetup` is intentionally different: the library profile is a
local response-header/setup boundary and is not sent as `grpc-timeout`, because
tonic applies that header to the whole stream. Only a caller-supplied
whole-call `grpc-timeout` is propagated for this class, with its remaining
absolute budget reduced by readiness time; the library setup and enclosing
budgets remain local to response setup.

When idle or lifetime limits are enabled, `GrpcStreamDeadline` observes raw,
nonempty HTTP data frames before tonic decodes a message. Such progress resets
idle; empty frames do not, and neither progress nor reconnect resets total
lifetime or an enclosing deadline. Reconnect backoff, resubscription, and
caller-provided replay work can remain inside those absolute bounds.

`client_commands()` returns generated clients over the deadline-aware
`AnytypeGrpcService`; `deadline_channel()` exposes a clone of that service for
other generated clients. `channel()` remains a raw `tonic::transport::Channel`
for compatibility. Calls made through the raw channel bypass this logical
policy, and a disabled boundary is unbounded unless the caller supplies a
separate timeout.

## Related projects

- [anytype](https://crates.io/crates/anytype) An ergonomic Anytype API client in Rust. Includes http rest api plus gRPC backend using this crate, for access to Files and Chats.

  The dependency remains one-way: `anytype` uses `anytype-rpc`; this crate does
  not depend on `anytype`.

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
