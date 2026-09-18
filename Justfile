# Everything CI would run.
check:
    cargo fmt --check
    cargo clippy --all-targets -- -D warnings
    cargo test

# The other two platforms compile. Only `src/sys/` differs between them, and only a build says
# so; the tests still run where the machine is. Needs the std of both targets:
#   rustup target add x86_64-unknown-linux-gnu x86_64-pc-windows-msvc
check-cross:
    cargo check --target x86_64-unknown-linux-gnu
    cargo check --target x86_64-pc-windows-msvc

# Benchmarks on a COPY of a real workspace; see docs/bench.md.
bench workspace:
    scripts/bench.sh {{workspace}}

# The same for the cargo home: it works on a clone of it, never on ~/.cargo itself.
bench-home:
    scripts/bench-cargo-home.sh
