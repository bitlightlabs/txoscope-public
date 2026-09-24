# Publishing 0.3.0

All five crates enable publishing and share version 0.3.0. Internal dependencies
specify both path and version: development uses the checkout and published
packages resolve their dependencies from the registry. Each package includes
the workspace README and both license texts.

Before publishing, run from this workspace with current stable Cargo:

```sh
cargo test --workspace --all-features
cargo clippy --workspace --all-targets --all-features -- -D warnings
RUSTDOCFLAGS='-D warnings' cargo doc --workspace --all-features --no-deps
cargo package --workspace
cargo publish --workspace --dry-run
```

Workspace packaging verifies the built archives together using Cargo's temporary
registry, including unpublished sibling versions. It does not upload packages.
For a dirty development checkout, add `--allow-dirty` for local validation only.
When repeating packaging after changes at the same version, use a fresh
`CARGO_TARGET_DIR` to avoid stale temporary-registry dependency archives.
Review the archive file lists under `target/package`, especially license files,
and publish from the reviewed, clean release commit.

A maintainer with crates.io ownership and credentials should publish in dependency
order, waiting for each dependency to become available before its dependents:

1. `cargo publish -p txoscope-core`
2. `cargo publish -p txoscope-store-memory`
3. `cargo publish -p txoscope-store-sqlite`
4. `cargo publish -p txoscope`
5. `cargo publish -p txoscope-sim`

Check crate-name ownership and version availability on crates.io before release.
Packaging/dry-run success does not establish publishing permission. Publishing,
release tagging and merging require the normal maintainer release review.
