# Known issues & Troubleshooting

## Tracking issues in `anytype-heart`

Issues in the `anytype` library should be filed in this repo.

The following issues were found during testing `anytype` and are filed in the `anytype-heart` repo.

**Issues**

- [2879](https://github.com/anyproto/anytype-heart/issues/2879) bool and number filters in list queries.
- [2887](https://github.com/anyproto/anytype-heart/issues/2887) sorting search results by `due_date`
- [2883](https://github.com/anyproto/anytype-heart/issues/2883) Crash: nil account-key from DeriveKeysFromMasterNode

**PRs**

- [2880](https://github.com/anyproto/anytype-heart/pull/2880) using "type" ids in list filters
- [2881](https://github.com/anyproto/anytype-heart/pull/2881) bool and number filters in list queries
- [2882](https://github.com/anyproto/anytype-heart/pull/2882) select tags in filters PR
- [2884](https://github.com/anyproto/anytype-heart/pull/2884) crash: nil account-key from DeriveKeysFromMasterNode

## Debug logging

To enable debug logging, set `RUST_LOG=debug`

To log http requests and responses, set `RUST_LOG=anytype::http_json=trace`

To enable logging in examples, set RUST_LOG in the environment, and initialize tracing in main().

```rust
    use tracing_subscriber::EnvFilter;
    let _ = tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .try_init();
```

## OS Keyring may require signed executables

The OS keyring feature for key storage may not work if the app is unsigned (symptom: either you get a hard error, or you get prompted authenticate with the OS too often). File-based key storage is available as a fallback.

## Spurious 500 errors

While running unit tests on `anytype`, which can hammer the server pretty hard and run into rate limits, I sometimes encounter spurious 500 errors from the anytype server when creating objects. In nearly all cases, retrying the request, with the exact same bytes, succeeds. To shield the library caller from this, the http_client middleware automatically retries requests up to 3 times if it encounters a connection timeout, server timeout, or 500 error.
