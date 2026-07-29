default:
    @just --list

run *args:
    cargo run -- {{args}}

watch *args:
    cargo run -- --watch {{args}}

test:
    cargo test

check:
    cargo fmt --check
    cargo clippy --all-targets --all-features -- -D warnings
    cargo test
