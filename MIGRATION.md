# Migration guide

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
