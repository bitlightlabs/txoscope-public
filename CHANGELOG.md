# Changelog

All notable changes to this project will be documented in this file.

The project follows [Semantic Versioning](https://semver.org/). See [VERSIONING.md](./VERSIONING.md) for the workspace policy.

## Unreleased — 0.3.0

- Expose the pure `AssetSelectionPolicy`, eligible `AssetCandidate` facts and `AssetSelectionConstraints` in core. Preview and planning delegate to the policy; simulations exercise it without service dependencies. Existing `txoscope::SelectionConstraints` remains a re-export, preserving source compatibility and persistence formats.
- Distinguish entirely excluded asset candidates from insufficient eligible balances, returning per-input rejection diagnostics and specific errors for exact inputs.
- Add the read-only `utxo_status` query with chain state, availability, operation/phase, lock purpose/expiry and selection diagnostics, sharing the selection invariant.
- Enable crates.io publishing for all five crates, add versioned workspace dependencies and packaged documentation/licenses, and document dependency-ordered release validation. The new public policy API uses a minor version under the workspace versioning policy.

## 0.2.1 — 2026-09-23

- Fix the recovery query gap introduced in 0.2.0: Signed and later inputs retain ownership, but the existing Selected-only query cannot expose that owner for recovery. Add `owning_operation_id_for_outpoint` across all binding phases while leaving the existing query, lifecycle rules, spending protection, and persistence format unchanged. This corrective addition uses the pre-1.0 PATCH exception in [VERSIONING.md](VERSIONING.md) to restore the existing recovery workflow.

## 0.2.0 — 2026-09-23

- Wrap asset planning in `PlanAssetSelectionRequest`, return named `ManagedUtxoInfo` query results, and support asset preview/planning with configured clocks and reservation ID generators. Mark `Capability` non-exhaustive and document the complete 0.2 migration and pre-1.0 versioning policy.
- Use typed `OutPoint` values throughout selection, reservation, query, and store APIs. Parse external `txid:vout` text at the boundary while preserving opaque transaction identifiers, SQLite storage, and selection ordering; see [MIGRATION.md](./MIGRATION.md).
- Replace interchangeable account, scope, and operation strings with `AccountId`, `WalletScopeId`, and `OperationId` across public APIs and store contracts. Opaque identifier text and SQLite storage remain unchanged; see [MIGRATION.md](./MIGRATION.md).
- Implement `Display` and `std::error::Error` for non-exhaustive `DomainError`. Distinguish invalid caller requests, missing account capabilities, and absent eligible scopes from stored-state corruption and scope-specific errors. Reject malformed selections before changing reservations; see [MIGRATION.md](./MIGRATION.md).
- Keep operation inputs bound and locked through construction, signing, and broadcast until an observed spend or explicit release. Broadcasting starts spending protection, which stale unspent observations cannot clear, including for legacy unbound in-flight inputs. Advanced operations no longer expire as selection reservations.
- Replace the backend `ManagedUtxo::complete_binding()` helper with phase-aware `advance_binding(phase)`. `complete_operation` now returns the still-bound inputs on each phase advance, rather than only on the first advance.
- Require `StateStore::release_selected_operation` for atomic reservation expiry, validate phase advancement inside store writes, and preserve existing input ownership when applying delayed discovery plans.
- Enforce account/scope capabilities during selection, explicit application, commit, and recovery.
- Require and persist the fee-support input role for asset fee inputs.
- Advance the workspace to 0.2.0 for the added `LockPurpose::FeeSupport` variant. Existing SQLite data remains readable; older binaries cannot decode newly written `fee_support` lock purposes.

## 0.1.0

First tagged workspace release. All crates share `version = "0.1.0"`.

- `txoscope-core` — domain model, reconciliation, selection, operations, and store traits
- `txoscope` — application-facing `TxoscopeService` facade
- `txoscope-store-memory` — in-memory `StateStore` backend
- `txoscope-store-sqlite` — SQLite `StateStore` backend (`PRAGMA user_version` schema versioning)
- `txoscope-sim` — invariant checker and metrics helpers

This is a pre-1.0 release. Public APIs and persistence formats may still evolve.
