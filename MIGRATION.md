# Migration guide

## 0.4.0 — Unreleased

Version 0.4.0 changes the public status vocabulary and upgrades SQLite databases
to schema 4. Update downstream enum matches and serialized-value consumers
before adopting this release. Back up the database before opening it with 0.4.0;
opening performs the migration automatically.

### Optional serialization and schema features

`txoscope-core` provides independent, opt-in `serde` and `schema` features for
`OnchainState`, `Availability`, `OperationPhase`, and `LockPurpose`. `serde` adds
serialization/deserialization; `schema` adds `utoipa::ToSchema`. Both expose the
current canonical snake_case values and can be enabled separately or together.
Neither feature is enabled by default. Enable them on the `txoscope-core`
dependency; they are not feature flags on the `txoscope` facade.

### SQLite migration mechanism

Opening a store automatically runs ordered forward migrations under a SQLite
`BEGIN IMMEDIATE` transaction. Schema changes, data rewrites, version updates,
and final schema/foreign-key validation commit together. Any error rolls back
the entire upgrade; correct the cause and reopen to retry. Concurrent openers
serialize before reading the version (with the store's five-second busy timeout).

The registry now targets schema version 4. Empty databases start at version 0;
existing version 1, 2 and 3 files migrate through the ordered steps to 4. Non-empty files
without a version, newer versions, and malformed current schemas are rejected.
Current schemas are validated, not repaired by silently creating missing tables.
Journal mode is changed to WAL only after successful migration.

To add a migration, implement the next numbered step in its own module under
`src/migrations/` in the SQLite crate and register it in `MIGRATIONS` in
`src/migrations/mod.rs`. `SqliteSchema` validates that the
sequence is contiguous and derives its target version from that sequence. Keep
existing steps immutable.
Use source-version SQL values for data conversion instead of current domain
decoders. Do not commit, change transaction settings, or perform external side
effects inside a step. Update `SqliteSchema::validate` for the resulting schema
and test both upgrades and fresh databases. A step may deliberately discard data
if the release documents that policy; the mechanism does not require indefinite
legacy decoding support.

There are no down migrations. Back up the database before a version-changing
upgrade; restoring that backup is the supported downgrade path. Do not run old
and new binaries concurrently against an upgraded database.

### Protocol-neutral status vocabulary and SQLite schema 4

Replace `Availability::BlockedByL2` with `Availability::BlockedBySpend`
(`blocked_by_spend`). It describes spent or spending-in-flight outputs.
`OperationPhase::AwaitingL2Finality` and `Capability::L2Settlement` are removed.
Core strum parsing, serde serialization and utoipa schemas expose only current
canonical values. Historical lock-purpose aliases and the `rgb_receive`
capability alias are no longer accepted by core parsing. Asset selection still
uses caller-supplied facts; its algorithms and constraints are unchanged.

SQLite schema 3 upgrades to 4 atomically when opened. The migration owns the old
vocabulary and rewrites:

- `awaiting_l2_finality` to `awaiting_confirmation` in operations and bindings.
- `blocked_by_l2` to `blocked_by_spend` only for stored `spent` or
  `spending_in_flight` chain states.
- `rgb_receive` to `single_target_reserve` in account and scope capabilities;
  `l2_settlement` is removed without granting any replacement capability.
- `single_target_reserve`, `anchor_with_fee_support`, `rgb_receive_preparation`,
  `rgb_l1_send` and `channel_funding` lock purposes to `planned_selection` in
  both locks and intent effects.

The migration validates actual SQLite storage types, historical vocabulary,
paired fields and active ownership consistency. Original operation/binding
phases must agree before normalization. Pending and failed requests without
active ownership retain their selection data, including requests whose scopes
or inputs do not exist, so recovery can process them normally.

An old `blocked_by_l2` row with confirmed, unknown or unconfirmed-incoming chain
state is ambiguous and fails migration. Inconsistent late-phase records also
fail: a lifecycle label cannot supply missing chain facts. The migration never
substitutes `locked`, since releasing a binding could then reopen the output.
Failures roll back data and schema version; correct the historical data using
authoritative facts before retrying. Successful upgrades are not rerun on later
opens. Released spent/in-flight outputs remain unselectable after restart and
lagging unspent observations.

Older binaries do not support schema 4. Back up the database before upgrading;
rolling back a binary requires restoring its compatible database backup.
Do not lower `PRAGMA user_version` to bypass this restriction: the rewritten
values are incompatible with the old vocabulary. Restoring a pre-upgrade backup
also discards changes made since that backup; retain the upgraded database if
those changes need to be recovered separately.

## 0.2.0 — 2026-09-23

Version 0.2.0 updates the public API. Applications and custom store backends must
update their Rust types and method calls; identifier and outpoint storage stays
compatible.

### Account, scope, and operation identifiers

`AccountId`, `WalletScopeId`, and `OperationId` are distinct owned string types,
re-exported by `txoscope` and `txoscope-core`. Bootstrap data, requests, results,
observations, errors, store traits, and queries use the corresponding ID type.
Manual reservation IDs are `OperationId` values; custom
`ReservationIdGenerator::generate` implementations must return that type.

Construct IDs with `AccountId::new("account")` or `"account".into()` when the type
is known. Lifecycle and lookup methods take typed references, such as
`service.release_operation(&operation_id)`. An account ID cannot be passed to an
operation method.

Use `id.as_str()` to borrow the text or `id.into_string()` to recover an owned
string at an external boundary. IDs do not implicitly dereference to strings or
convert between identifier types. They preserve opaque strings exactly, without
validation, trimming, case folding, or normalization.

### Transaction outputs

All semantic output fields and arguments use `OutPoint`, `Vec<OutPoint>`, or
`&OutPoint`. This includes candidates, selection results, input verification,
reservations, queries, errors, lifecycle results, and store contracts.

Parse external text once with `let outpoint: OutPoint = input.parse()?`, or
construct a known output with `OutPoint::new(txid, vout)`. Use `Display` or
`to_string()` at a text boundary. Parsing splits at the last `:` and requires a
`u32` output index; malformed input returns `DomainError::InvalidRequest`.
Transaction identifiers remain opaque: empty identifiers, embedded colons,
whitespace, and non-hex identifiers are preserved. Numeric indexes are rendered
in decimal form. Selection tie-breaks retain the previous serialized text order.

### Asset planning and managed-output queries

Replace the two arguments to `plan_asset_selection` with one request. Preview
continues to borrow an `AssetSelectionRequest`:

```rust
let preview = service.preview_asset_selection(&selection)?;
let planned = service.plan_asset_selection(PlanAssetSelectionRequest {
    operation_id: OperationId::new("asset-send"),
    selection,
})?;
```

A complete, compiler-checked request example is provided in
[`PlanAssetSelectionRequest`](crates/txoscope/src/asset_selection.rs) rustdoc.
Both asset methods are available on services with custom `Clock` and
`ReservationIdGenerator` implementations. Reservation cleanup uses the configured
clock in preview and planning; asset plans do not add an expiry option.

`managed_utxo(&outpoint)` now returns `Option<ManagedUtxoInfo>` instead of a tuple.
Replace tuple destructuring or positional access with named fields:

```rust
if let Some(info) = service.managed_utxo(&outpoint)? {
    let account_id: AccountId = info.account_id;
    let value_sats: u64 = info.value_sats;
}
```

Unknown outputs still return `None`; known outputs retain their account and value.

### Errors and extensible capabilities

`DomainError` implements `Display` and `std::error::Error`. Display messages are
human-readable diagnostics, not a stable format to parse. `DomainError` and
`Capability` are `#[non_exhaustive]`; downstream matches must include a wildcard
arm for future variants. Lifecycle and reconciliation enums retain their existing
exhaustive contracts.

- Handle `InvalidRequest { reason }` for empty or duplicate explicit/exact input
  selections, duplicate asset candidates, and malformed outpoint text. Invalid
  selection structures are rejected before reservation cleanup or persistence.
- Handle `MissingAccountCapabilities { account_id }` when an account lacks the
  required capabilities. `MissingCapabilities { scope_id }` identifies an actual
  scope; it no longer carries an `account=...` pseudo-identifier.
- Handle `NoEligibleScope { account_id }` when the account has the required
  capabilities but no registered scope can satisfy the operation. This replaces
  the synthetic `ScopeNotFound("no eligible scope")` error.
- `CorruptedState` continues to report invalid stored/backend selection structure,
  unreadable persisted values, and internal invariants. `Storage` reports backend
  access failures.

### Other operation changes in 0.2

Store implementations must provide `StateStore::release_selected_operation` for
atomic reservation expiry and validate phase advancement inside their write
transaction or snapshot. Replace `ManagedUtxo::complete_binding()` with
`advance_binding(phase)`. Bindings and locks survive construction, signing, and
broadcast until an observed spend or explicit release; advancing beyond Selected
ends reservation expiry. `complete_operation` returns the still-bound inputs on
each phase advance; repeating the current phase returns an empty list.

`LockPurpose::FeeSupport` identifies allocation-free asset fee inputs, and
selection, commit, and recovery enforce account/scope capabilities. Custom
backends must preserve the fee-support role in persisted intents and locks.

### Persistence and release policy

The API cleanup preserves SQLite TEXT values and schema version 3. Existing
SQLite data remains readable without a migration. The new `fee_support` lock
purpose is readable by 0.2; older binaries cannot decode newly written values
using that purpose.

Before 1.0, breaking API or persistence changes require a minor version bump;
from 1.0 onward they require a major bump. See [VERSIONING.md](VERSIONING.md).
Automated public API snapshots and SemVer comparison checks remain a follow-up;
v0.2.0 provides the compatibility baseline.

## 0.3.0

No database migration is required. `txoscope::SelectionConstraints` is
now a re-export of `txoscope_core::AssetSelectionConstraints`; its fields and
selection semantics are unchanged. Pure integrations can call `AssetSelectionPolicy::select`
with eligible `AssetCandidate` facts. Callers must establish ownership,
capabilities and availability before invoking this policy. Exact selections
preserve caller order and override the automatic merge preference.

Automatic selection now returns `NoEligibleAssetCandidates { rejections }` when
all supplied candidates are excluded, instead of `InsufficientAsset { have: 0, .. }`.
Each rejection includes its outpoint and a domain error explaining ownership,
capability, missing observations or availability. Exact requests return the
specific rejected-input error. Callers handling insufficient-balance errors
should also handle this variant and surface its reasons. `InsufficientAsset`
continues to describe insufficient amounts among eligible inputs.
