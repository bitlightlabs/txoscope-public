use txoscope_core::{
	backend::{
		LockPurpose, OperationSelection, ReconciliationPlanner, SelectionEffect, StateBatch,
		StateRead, StateStore,
	},
	Availability, ManagedUtxo, ObservationScope, ObservationSnapshot, ObservedUtxo, OnchainState,
	OperationBinding, OperationPhase, OutPoint,
};
use txoscope_store_sqlite::SqliteStateStore;

#[test]
fn test_sqlite_conflict_persistence() {
	let mut store = SqliteStateStore::open_in_memory().unwrap();

	store
		.put_account(txoscope_core::Account::new(
			"btc",
			"BTC",
			vec![],
			txoscope_core::CapabilitySet::new([]),
		))
		.unwrap();
	store
		.put_scope(txoscope_core::WalletScope::new(
			"scope1",
			"btc",
			"desc",
			"m/0",
			txoscope_core::CapabilitySet::new([]),
			10,
		))
		.unwrap();

	let op = OutPoint::new("tx1", 0);
	let mut utxo = ManagedUtxo::new(op.clone(), "btc", "scope1", 50000);
	utxo.availability = Availability::Reserved;
	store.put_utxo(utxo).unwrap();

	let observed = ObservedUtxo::new(op.clone(), "btc", "scope1", 50000).spent("tx_spending");

	let snapshot = ObservationSnapshot::new(2000, ObservationScope::Incremental).add_utxo(observed);

	let plan = ReconciliationPlanner::new(&store).plan(&snapshot).unwrap();
	store.apply_reconciliation(plan).unwrap();

	let utxo = store.get_utxo(&op).unwrap().unwrap();
	assert_eq!(utxo.onchain_state, OnchainState::Spent);
	let conflict = utxo.observation_conflict.unwrap();
	assert!(conflict.message.contains("spent by tx_spending"));
}

#[test]
fn test_sqlite_no_resurrection_after_spend() {
	let mut store = SqliteStateStore::open_in_memory().unwrap();

	store
		.put_account(txoscope_core::Account::new(
			"btc",
			"BTC",
			vec![],
			txoscope_core::CapabilitySet::new([]),
		))
		.unwrap();
	store
		.put_scope(txoscope_core::WalletScope::new(
			"scope1",
			"btc",
			"desc",
			"m/0",
			txoscope_core::CapabilitySet::new([]),
			10,
		))
		.unwrap();

	let op = OutPoint::new("tx1", 0);
	let mut utxo = ManagedUtxo::new(op.clone(), "btc", "scope1", 50000);
	utxo.availability = Availability::Reserved;
	utxo.operation_binding = Some(txoscope_core::OperationBinding {
		operation_id: "op1".into(),
		phase: txoscope_core::OperationPhase::Selected,
	});
	store.put_utxo(utxo).unwrap();

	let snapshot = ObservationSnapshot::new(3000, ObservationScope::Incremental)
		.add_utxo(ObservedUtxo::new(op.clone(), "btc", "scope1", 50000).spent("tx_ext"));

	let plan = ReconciliationPlanner::new(&store).plan(&snapshot).unwrap();
	store.apply_reconciliation(plan).unwrap();

	store.release_operation(&"op1".into(), txoscope_core::OperationPhase::RollingBack).unwrap();

	let utxo_final = store.get_utxo(&op).unwrap().unwrap();
	assert_eq!(utxo_final.onchain_state, OnchainState::Spent);
	assert_ne!(utxo_final.availability, Availability::Selectable);
}

#[test]
fn spent_utxo_does_not_reopen_when_later_observed_unspent() {
	let mut store = SqliteStateStore::open_in_memory().unwrap();

	store
		.put_account(txoscope_core::Account::new(
			"btc",
			"BTC",
			vec![],
			txoscope_core::CapabilitySet::new([]),
		))
		.unwrap();
	store
		.put_scope(txoscope_core::WalletScope::new(
			"scope1",
			"btc",
			"desc",
			"m/0",
			txoscope_core::CapabilitySet::new([]),
			10,
		))
		.unwrap();

	let op = OutPoint::new("tx-reopen", 0);
	let mut utxo = ManagedUtxo::new(op.clone(), "btc", "scope1", 50_000);
	utxo.mark_spent();
	store.put_utxo(utxo).unwrap();

	let snapshot = ObservationSnapshot::new(3_100, ObservationScope::Incremental)
		.add_utxo(ObservedUtxo::new(op.clone(), "btc", "scope1", 50_000).confirmed(101, 3_100));
	store
		.apply_reconciliation(ReconciliationPlanner::new(&store).plan(&snapshot).unwrap())
		.unwrap();

	let persisted = store.get_utxo(&op).unwrap().unwrap();
	assert_eq!(persisted.onchain_state, OnchainState::Spent);
	assert_eq!(persisted.availability, Availability::BlockedByL2);
}

#[test]
fn unknown_confirmation_discovery_is_blocked_by_policy() {
	let mut store = SqliteStateStore::open_in_memory().unwrap();
	store
		.put_account(txoscope_core::Account::new(
			"btc",
			"BTC",
			vec![],
			txoscope_core::CapabilitySet::new([]),
		))
		.unwrap();
	store
		.put_scope(txoscope_core::WalletScope::new(
			"scope1",
			"btc",
			"desc",
			"m/0",
			txoscope_core::CapabilitySet::new([]),
			10,
		))
		.unwrap();

	let op = OutPoint::new("tx-unknown", 0);
	let snapshot = ObservationSnapshot::new(900, ObservationScope::Incremental)
		.add_utxo(ObservedUtxo::new(op.clone(), "btc", "scope1", 40_000));
	store
		.apply_reconciliation(ReconciliationPlanner::new(&store).plan(&snapshot).unwrap())
		.unwrap();

	let utxo = store.get_utxo(&op).unwrap().unwrap();
	assert_eq!(utxo.onchain_state, OnchainState::Unknown);
	assert_eq!(utxo.availability, Availability::BlockedByPolicy);
}

#[test]
fn full_incremental_full_does_not_mark_missing_on_incremental_gap() {
	let mut store = SqliteStateStore::open_in_memory().unwrap();
	store
		.put_account(txoscope_core::Account::new(
			"btc",
			"BTC",
			vec![],
			txoscope_core::CapabilitySet::new([]),
		))
		.unwrap();
	store
		.put_scope(txoscope_core::WalletScope::new(
			"scope1",
			"btc",
			"desc",
			"m/0",
			txoscope_core::CapabilitySet::new([]),
			10,
		))
		.unwrap();

	let op1 = OutPoint::new("tx1", 0);
	let op2 = OutPoint::new("tx2", 0);

	let full_one = ObservationSnapshot::new(1000, ObservationScope::Full)
		.add_utxo(ObservedUtxo::new(op1.clone(), "btc", "scope1", 50_000).confirmed(100, 1000))
		.add_utxo(ObservedUtxo::new(op2.clone(), "btc", "scope1", 60_000).confirmed(100, 1000));
	store
		.apply_reconciliation(ReconciliationPlanner::new(&store).plan(&full_one).unwrap())
		.unwrap();

	let incremental = ObservationSnapshot::new(1100, ObservationScope::Incremental)
		.add_utxo(ObservedUtxo::new(op1.clone(), "btc", "scope1", 50_000).confirmed(101, 1100));
	store
		.apply_reconciliation(ReconciliationPlanner::new(&store).plan(&incremental).unwrap())
		.unwrap();
	assert_eq!(store.get_utxo(&op2).unwrap().unwrap().availability, Availability::Selectable);

	let full_two = ObservationSnapshot::new(1200, ObservationScope::Full)
		.add_utxo(ObservedUtxo::new(op1.clone(), "btc", "scope1", 50_000).confirmed(102, 1200));
	store
		.apply_reconciliation(ReconciliationPlanner::new(&store).plan(&full_two).unwrap())
		.unwrap();
	assert_eq!(store.get_utxo(&op2).unwrap().unwrap().availability, Availability::Recovering);
}

#[test]
fn recovering_bound_operation_restores_reserved_when_unspent_again() {
	let mut store = SqliteStateStore::open_in_memory().unwrap();
	store
		.put_account(txoscope_core::Account::new(
			"rgb",
			"RGB",
			vec![],
			txoscope_core::CapabilitySet::new([]),
		))
		.unwrap();
	store
		.put_scope(txoscope_core::WalletScope::new(
			"scope1",
			"rgb",
			"desc",
			"m/0",
			txoscope_core::CapabilitySet::new([]),
			10,
		))
		.unwrap();

	let op = OutPoint::new("tx1", 0);
	let mut utxo = ManagedUtxo::new(op.clone(), "rgb", "scope1", 50_000);
	utxo.availability = Availability::Recovering;
	utxo.operation_binding =
		Some(OperationBinding { operation_id: "op1".into(), phase: OperationPhase::Selected });
	store.put_utxo(utxo).unwrap();

	let snapshot = ObservationSnapshot::new(1300, ObservationScope::Incremental)
		.add_utxo(ObservedUtxo::new(op.clone(), "rgb", "scope1", 50_000));
	store
		.apply_reconciliation(ReconciliationPlanner::new(&store).plan(&snapshot).unwrap())
		.unwrap();

	let restored = store.get_utxo(&op).unwrap().unwrap();
	assert_eq!(restored.availability, Availability::Reserved);
}

#[test]
fn spent_reconcile_clears_binding_and_lock_for_reserved_utxo() {
	let mut store = SqliteStateStore::open_in_memory().unwrap();
	store
		.put_account(txoscope_core::Account::new(
			"rgb",
			"RGB",
			vec![],
			txoscope_core::CapabilitySet::new([txoscope_core::Capability::ReserveSupport]),
		))
		.unwrap();
	store
		.put_scope(txoscope_core::WalletScope::new(
			"scope1",
			"rgb",
			"desc",
			"m/0",
			txoscope_core::CapabilitySet::new([txoscope_core::Capability::ReserveSupport]),
			10,
		))
		.unwrap();

	let op = OutPoint::new("tx1", 0);
	store.put_utxo(ManagedUtxo::new(op.clone(), "rgb", "scope1", 50_000)).unwrap();
	store
		.apply_batch(StateBatch {
			selection: OperationSelection {
				operation_id: "op1".into(),
				selected_outpoints: vec![op.clone()],
				change_scope_id: "scope1".into(),
				effects: vec![SelectionEffect::LockUtxo {
					outpoint: op.clone(),
					purpose: LockPurpose::PlannedSelection,
				}],
				expires_at: None,
			},
			phase: OperationPhase::Selected,
			owner: "test".to_string(),
		})
		.unwrap();

	let snapshot = ObservationSnapshot::new(1_400, ObservationScope::Incremental)
		.add_utxo(ObservedUtxo::new(op.clone(), "rgb", "scope1", 50_000).spent("tx-spent"));
	store
		.apply_reconciliation(ReconciliationPlanner::new(&store).plan(&snapshot).unwrap())
		.unwrap();

	let spent = store.get_utxo(&op).unwrap().unwrap();
	assert!(spent.operation_binding.is_none());
	assert_eq!(spent.availability, Availability::BlockedByL2);
	assert!(store.get_lock(&op).unwrap().is_none());
}

#[test]
fn observed_metadata_mismatch_blocks_selection_without_rewriting_local_state() {
	let mut store = SqliteStateStore::open_in_memory().unwrap();
	store
		.put_account(txoscope_core::Account::new(
			"btc",
			"BTC",
			vec![],
			txoscope_core::CapabilitySet::new([]),
		))
		.unwrap();
	store
		.put_scope(txoscope_core::WalletScope::new(
			"scope1",
			"btc",
			"desc",
			"m/0",
			txoscope_core::CapabilitySet::new([]),
			10,
		))
		.unwrap();

	let op = OutPoint::new("tx-meta", 0);
	store.put_utxo(ManagedUtxo::new(op.clone(), "btc", "scope1", 50_000)).unwrap();

	let snapshot = ObservationSnapshot::new(1_450, ObservationScope::Incremental)
		.add_utxo(ObservedUtxo::new(op.clone(), "rgb", "scope2", 60_000).confirmed(102, 1450));
	store
		.apply_reconciliation(ReconciliationPlanner::new(&store).plan(&snapshot).unwrap())
		.unwrap();

	let utxo = store.get_utxo(&op).unwrap().unwrap();
	assert_eq!(utxo.account_id, "btc".into());
	assert_eq!(utxo.scope_id, "scope1".into());
	assert_eq!(utxo.value_sats, 50_000);
	assert_eq!(utxo.availability, Availability::BlockedByPolicy);
	assert!(utxo
		.observation_conflict
		.as_ref()
		.is_some_and(|conflict| conflict.message.contains("Observed metadata mismatch")));
}

#[test]
fn spending_in_flight_unspent_observation_preserves_spending_protection() {
	let mut store = SqliteStateStore::open_in_memory().unwrap();
	store
		.put_account(txoscope_core::Account::new(
			"rgb",
			"RGB",
			vec![],
			txoscope_core::CapabilitySet::new([]),
		))
		.unwrap();
	store
		.put_scope(txoscope_core::WalletScope::new(
			"scope1",
			"rgb",
			"desc",
			"m/0",
			txoscope_core::CapabilitySet::new([]),
			10,
		))
		.unwrap();

	let op = OutPoint::new("tx2", 0);
	let mut utxo = ManagedUtxo::new(op.clone(), "rgb", "scope1", 50_000);
	utxo.onchain_state = OnchainState::SpendingInFlight;
	utxo.availability = Availability::BlockedByL2;
	store.put_utxo(utxo).unwrap();

	let snapshot = ObservationSnapshot::new(1_500, ObservationScope::Incremental)
		.add_utxo(ObservedUtxo::new(op.clone(), "rgb", "scope1", 50_000).mempool());
	store
		.apply_reconciliation(ReconciliationPlanner::new(&store).plan(&snapshot).unwrap())
		.unwrap();

	let restored = store.get_utxo(&op).unwrap().unwrap();
	assert_eq!(restored.onchain_state, OnchainState::SpendingInFlight);
	assert_eq!(restored.availability, Availability::BlockedByL2);
}

#[test]
fn releasing_conflicted_reservation_restores_blocked_by_policy() {
	let mut store = SqliteStateStore::open_in_memory().unwrap();
	store
		.put_account(txoscope_core::Account::new(
			"rgb",
			"RGB",
			vec![],
			txoscope_core::CapabilitySet::new([txoscope_core::Capability::ReserveSupport]),
		))
		.unwrap();
	store
		.put_scope(txoscope_core::WalletScope::new(
			"scope1",
			"rgb",
			"desc",
			"m/0",
			txoscope_core::CapabilitySet::new([txoscope_core::Capability::ReserveSupport]),
			10,
		))
		.unwrap();

	let op = OutPoint::new("tx-conflicted", 0);
	store.put_utxo(ManagedUtxo::new(op.clone(), "rgb", "scope1", 50_000)).unwrap();
	store
		.apply_batch(StateBatch {
			selection: OperationSelection {
				operation_id: "op-conflicted".into(),
				selected_outpoints: vec![op.clone()],
				change_scope_id: "scope1".into(),
				effects: vec![SelectionEffect::LockUtxo {
					outpoint: op.clone(),
					purpose: LockPurpose::PlannedSelection,
				}],
				expires_at: None,
			},
			phase: OperationPhase::Selected,
			owner: "test".to_string(),
		})
		.unwrap();

	let snapshot = ObservationSnapshot::new(1_600, ObservationScope::Incremental)
		.add_utxo(ObservedUtxo::new(op.clone(), "btc", "scope2", 60_000).confirmed(103, 1600));
	store
		.apply_reconciliation(ReconciliationPlanner::new(&store).plan(&snapshot).unwrap())
		.unwrap();
	store
		.release_operation(&"op-conflicted".into(), txoscope_core::OperationPhase::RollingBack)
		.unwrap();

	let restored = store.get_utxo(&op).unwrap().unwrap();
	assert!(restored.operation_binding.is_none());
	assert_eq!(restored.availability, Availability::BlockedByPolicy);
	assert!(restored.observation_conflict.is_some());
}
