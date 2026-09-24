#![allow(dead_code)]

use txoscope_core::{
	backend::{Orchestrator, StateStore},
	Account, Capability, CapabilitySet, ManagedUtxo, OutPoint, WalletScope,
};
use txoscope_store_memory::InMemoryStateStore;

pub fn base_store() -> InMemoryStateStore {
	let mut store = InMemoryStateStore::default();

	store
		.put_account(Account::new(
			"btc",
			"BTC",
			vec![],
			CapabilitySet::new([
				Capability::BtcSpend,
				Capability::BtcReceive,
				Capability::ReserveSupport,
			]),
		))
		.unwrap();
	store
		.put_account(Account::new(
			"rgb",
			"L2",
			vec![],
			CapabilitySet::new([
				Capability::BtcReceive,
				Capability::SingleTargetReserve,
				Capability::FeeSupport,
				Capability::ReserveSupport,
				Capability::L2Settlement,
			]),
		))
		.unwrap();

	store
		.put_scope(WalletScope::new(
			"btc_primary_scope",
			"btc",
			"wpkh(desc-btc)",
			"m/84'/0'/0'/0/*",
			CapabilitySet::new([
				Capability::BtcSpend,
				Capability::BtcReceive,
				Capability::ReserveSupport,
			]),
			10,
		))
		.unwrap();

	store
		.put_scope(WalletScope::new(
			"rgb_operational_scope",
			"rgb",
			"wpkh(desc-rgb-op)",
			"m/84'/0'/10'/0/*",
			CapabilitySet::new([
				Capability::BtcReceive,
				Capability::SingleTargetReserve,
				Capability::ReserveSupport,
				Capability::L2Settlement,
			]),
			10,
		))
		.unwrap();

	store
		.put_scope(WalletScope::new(
			"rgb_btc_support_scope",
			"rgb",
			"wpkh(desc-rgb-btc)",
			"m/84'/0'/10'/1/*",
			CapabilitySet::new([
				Capability::FeeSupport,
				Capability::ReserveSupport,
				Capability::ConsolidationSource,
				Capability::BtcReceive,
			]),
			20,
		))
		.unwrap();

	store
}

pub fn base_orchestrator() -> Orchestrator<InMemoryStateStore> {
	Orchestrator::with_store(base_store())
}

pub fn utxo(txid: &str, vout: u32, account: &str, scope: &str, value_sats: u64) -> ManagedUtxo {
	ManagedUtxo::new(OutPoint::new(txid, vout), account, scope, value_sats)
}
