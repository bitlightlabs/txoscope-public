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
