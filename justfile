_default:
    just --list

test:
    cargo test --locked --workspace --all-features --exclude anytype
    cargo test --locked -p anytype --all-features --lib

check:
    gate check
