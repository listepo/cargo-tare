# Everything CI would run.
check:
    cargo fmt --check
    cargo clippy --all-targets -- -D warnings
    cargo test

# Benchmarks on a COPY of a real workspace; see docs/bench.md.
bench workspace:
    scripts/bench.sh {{workspace}}
