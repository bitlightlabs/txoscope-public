# TxoScope

`txoscope` is a library for account-aware and scope-aware
UTXO observation, reconciliation, selection, reservation, and operation
lifecycle management.

It consumes normalized UTXO observations and candidate inputs from a
wallet, indexer, or protocol runtime. Core logic is independent of any
particular backend.

## Feature

- Normalizes and reconciles observed UTXO state
- Tracks account and wallet-scope ownership
- Selects eligible UTXOs
- Reserves inputs for operations
- Persists operation intents
- Supports deterministic replay and recovery
- Provides various storage backends

## Non-goals

txoscope consumes normalized wallet facts and candidate inputs. It does not
replace the wallet, signer, chain indexer, protocol runtime, or transaction
builder.

It does not implement any of:

- mnemonic, seed, or xpriv / xpub handling
- address derivation
- descriptor ownership
- signing
- transaction construction or broadcast
- blockchain scanning
- Esplora, Electrum, or RPC clients
- a replacement wallet database

## Architecture

```text
Wallet / indexer / protocol runtime
              │
              │ observations / candidates
              ▼
         TxoscopeService
              │
     ┌────────┴────────┐
     │                 │
 reconciliation    selection
     │                 │
     └──── operation ──┘
              │
              ▼
       StateStore trait
```

Application code should use `TxoscopeService` from the `txoscope` crate.
`txoscope-core` provides the domain model and invariant-preserving primitives.
`txoscope` provides the high-level application service and public facade.

`AssetSelectionPolicy::select` is a pure core policy over eligible `AssetCandidate`
facts. The service reads managed UTXOs, checks capabilities and availability,
constructs candidates, calls the policy, builds operation effects and persists.
Preview and planning share this policy; simulations can call it directly.

txoscope can be embedded by wallet, node, RGB node, or other UTXO-based
applications.

## Workspace

| Crate | Role |
|---|---|
| `txoscope-core` | Domain model and invariant-preserving primitives: accounts, wallet scopes, capabilities, managed UTXOs, observation, reconciliation, selection, operations, intents, locks, and store traits |
| `txoscope` | High-level application service and public facade. Prefer `TxoscopeService` over calling core orchestration APIs directly |
| `txoscope-store-memory` | In-memory backend for tests, examples, simulation, and ephemeral use |
| `txoscope-store-sqlite` | SQLite backend for transactional updates, persisted intents, and restart/recovery state |
| `txoscope-sim` | Simulation, invariant, and metrics helpers |

## Example

The example below builds against the current facade API. The same program lives
at [`crates/txoscope/examples/observe_and_reserve.rs`](crates/txoscope/examples/observe_and_reserve.rs).

```rust
use txoscope::{
    AccountId, BootstrapAccountConfig, BootstrapConfig, BootstrapScopeConfig, Capability,
    CapabilitySet, ObservationScope, ObservationSnapshot, ObservedConfirmation,
    ObservedSpendStatus, ObservedUtxo, OperationId, OutPoint, ReserveSpecificUtxoRequest,
    TxoscopeService, WalletScopeId,
};
use txoscope_store_memory::InMemoryStateStore;

fn main() -> txoscope::DomainResult<()> {
    let store = InMemoryStateStore::default();
    let mut service = TxoscopeService::with_store(store);
    let account_id = AccountId::new("alice");
    let scope_id = WalletScopeId::new("alice-btc-main");
    let outpoint: OutPoint = "tx1:0".parse()?;

    service.bootstrap(BootstrapConfig {
        owner: "example-wallet".to_string(),
        accounts: vec![BootstrapAccountConfig {
            id: account_id.clone(),
            name: "Alice".to_string(),
            capabilities: CapabilitySet::new([
                Capability::BtcSpend,
                Capability::BtcReceive,
                Capability::ReserveSupport,
            ]),
        }],
        scopes: vec![BootstrapScopeConfig {
            id: scope_id.clone(),
            account_id: account_id.clone(),
            descriptor_ref: "main".to_string(),
            derivation_scope: "external".to_string(),
            capabilities: CapabilitySet::new([
                Capability::BtcSpend,
                Capability::BtcReceive,
                Capability::ReserveSupport,
            ]),
            priority: 10,
        }],
    })?;

    service.reconcile_snapshot(ObservationSnapshot {
        scope: ObservationScope::Full,
        timestamp: 1_735_000_000,
        utxos: vec![ObservedUtxo {
            outpoint: outpoint.clone(),
            account_id: account_id.clone(),
            scope_id: scope_id.clone(),
            value_sats: 50_000,
            confirmation: ObservedConfirmation::Confirmed {
                height: 840_000,
                timestamp: 1_735_000_000,
            },
            spend_status: ObservedSpendStatus::Unspent,
        }],
    })?;

    let planned = service.reserve_specific_utxo(ReserveSpecificUtxoRequest {
        operation_id: OperationId::new("op-1"),
        target_account_id: account_id.clone(),
        outpoint,
        expires_at: Some(1_735_000_300),
    })?;

    println!(
        "reserved {}",
        planned.selected_outpoints.iter().map(ToString::to_string).collect::<Vec<_>>().join(", ")
    );
    Ok(())
}
```

```bash
cargo run -p txoscope --example observe_and_reserve
```

## Development

See [CONTRIBUTING.md](CONTRIBUTING.md) and [RELEASING.md](RELEASING.md).

## Status

txoscope is currently pre-1.0.

Public APIs and persistence formats may still evolve between minor releases.

## License

Licensed under either of:

- Apache License, Version 2.0
- MIT License

at your option.
