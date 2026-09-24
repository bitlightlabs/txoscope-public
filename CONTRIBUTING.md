# Contributing

Thanks for helping improve txoscope.

## Setup

Development and CI use **stable Rust**.

```bash
rustup toolchain install stable --profile minimal --component rustfmt,clippy
```

Optional local Git hooks (Conventional Commits):

```bash
./scripts/install-git-hooks.sh
```

CI does not depend on these hooks. They only help catch commit-message issues before you push.

## Before submitting a PR

Run the same checks as CI:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps
```

Keep contributions inside txoscope's library boundary.
