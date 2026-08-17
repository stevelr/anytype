#!/usr/bin/env bash

set -euo pipefail

runner_temp=${RUNNER_TEMP:-/tmp}
package_target=$(mktemp -d "$runner_temp/package-target-XXXXXX")
isolation_root=$(mktemp -d "$runner_temp/crate-isolation-XXXXXX")
trap 'rm -rf -- "$isolation_root" "$package_target"' EXIT

CARGO_TARGET_DIR="$package_target" \
  cargo package --locked --no-verify -p anytype-rpc -p anytype

rpc_archive=$(find "$package_target/package" -maxdepth 1 -type f -name 'anytype-rpc-*.crate' -print -quit)
api_archive=$(find "$package_target/package" -maxdepth 1 -type f -name 'anytype-*.crate' ! -name 'anytype-rpc-*.crate' -print -quit)
test -n "$rpc_archive" && test -f "$rpc_archive"
test -n "$api_archive" && test -f "$api_archive"

tar -xzf "$rpc_archive" -C "$isolation_root"
tar -xzf "$api_archive" -C "$isolation_root"
rpc_dir=$(find "$isolation_root" -mindepth 1 -maxdepth 1 -type d -name 'anytype-rpc-*' -print -quit)
api_dir=$(find "$isolation_root" -mindepth 1 -maxdepth 1 -type d -name 'anytype-*' ! -name 'anytype-rpc-*' -print -quit)
test -n "$rpc_dir" && test -f "$rpc_dir/Cargo.toml"
test -n "$api_dir" && test -f "$api_dir/Cargo.toml"

mkdir -p "$isolation_root/.cargo"
printf '[patch.crates-io]\nanytype-rpc = { path = "%s" }\n' "$rpc_dir" \
  > "$isolation_root/.cargo/config.toml"

cd "$isolation_root"
export CARGO_INCREMENTAL=0
export CARGO_PROFILE_DEV_DEBUG=0
export CARGO_PROFILE_TEST_DEBUG=0
export CARGO_TARGET_DIR="$isolation_root/target"
cargo build --manifest-path "$rpc_dir/Cargo.toml"
cargo test --manifest-path "$rpc_dir/Cargo.toml" --lib
cargo doc --manifest-path "$rpc_dir/Cargo.toml" --no-deps
cargo build --manifest-path "$api_dir/Cargo.toml" --all-features
cargo test --manifest-path "$api_dir/Cargo.toml" --all-features --lib
cargo doc --manifest-path "$api_dir/Cargo.toml" --all-features --no-deps
