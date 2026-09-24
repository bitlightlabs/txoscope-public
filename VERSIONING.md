# Versioning

`txoscope` follows [Semantic Versioning](https://semver.org/). All workspace crates
share `workspace.package.version` and are released together under one tag.

The workspace is currently pre-1.0. Version 0.2.0 was released on 2026-09-23.
Breaking API and persistence changes may be collected into an upcoming minor
version; each preparation PR does not require a separate version bump.

## Compatibility axes

Version decisions cover both the public Rust API and persisted data:

1. **API compatibility** — public types, functions, and traits in all workspace crates.
2. **Persistence compatibility** — SQLite schema and serialized values that existing databases or binaries must understand.

| Change | Before 1.0 | From 1.0 onward |
|---|---|---|
| Backward-compatible bug fix or internal refactor | PATCH, such as `0.2.0 → 0.2.1` | PATCH, such as `1.0.0 → 1.0.1` |
| Backward-compatible public API or storage addition | MINOR, such as `0.2.0 → 0.3.0` | MINOR, such as `1.0.0 → 1.1.0` |
| Breaking public API or incompatible persistence change | MINOR, such as `0.2.0 → 0.3.0` | MAJOR, such as `1.0.0 → 2.0.0` |

Before 1.0, a corrective API addition may use PATCH only when it is necessary to
restore an existing workflow broken by a released defect, preserves every existing
API contract, and changes no persistence format. The changelog must identify the
defect and explain why the addition is corrective. This narrow exception does not
cover new features or storage additions; those still require MINOR. From 1.0
onward, public API additions require MINOR as shown above.

Cargo treats successive `0.x` minor versions as potentially incompatible.
`1.0.0` will be released when the public API and storage layouts are considered
stable. Public API snapshots and automated SemVer comparison checks remain a
follow-up, using v0.2.0 as the compatibility baseline.

## Compatibility review

Changes to public types, trait methods, SQLite schema, or serialized values need
an explicit compatibility assessment. Check both whether the new code can read
existing datasets and whether older binaries can read newly written values.
Document migration steps or downgrade limitations in [MIGRATION.md](MIGRATION.md)
and the changelog. A required persistence migration is a breaking change even
when Rust method signatures remain unchanged.
