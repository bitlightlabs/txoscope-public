mod common;

use common::base_store;
use txoscope_core::{
	backend::{
		LockPurpose, OperationSelection, ReconciliationPlanner, SelectionEffect, StateBatch,
		StateRead, StateStore,
	},
	Availability, ManagedUtxo, ObservationScope, ObservationSnapshot, ObservedConfirmation,
	ObservedUtxo, OnchainState, OperationPhase, OutPoint,
};

#[test]
fn test_discovery_tracks_generic_observed_utxo() {
	let mut store = base_store();

	let op = OutPoint::new("tx1", 0);
	let observed =
		ObservedUtxo::new(op.clone(), "rgb", "rgb_operational_scope", 10000).confirmed(100, 1000);

	let snapshot = ObservationSnapshot::new(1000, ObservationScope::Incremental).add_utxo(observed);

	let plan = ReconciliationPlanner::new(&store).plan(&snapshot).unwrap();
	store.apply_reconciliation(plan).unwrap();

	let utxo = store.get_utxo(&op).unwrap().unwrap();
	assert_eq!(utxo.account_id, "rgb".into());
	assert_eq!(utxo.scope_id, "rgb_operational_scope".into());
	assert_eq!(utxo.onchain_state, OnchainState::Confirmed);
}

#[test]
fn test_conflict_persistence() {
	let mut store = base_store();
	let op = OutPoint::new("tx1", 0);

	// Setup a reserved UTXO
	let mut utxo = ManagedUtxo::new(op.clone(), "btc", "btc_primary_scope", 50000);
	utxo.availability = Availability::Reserved;
	store.put_utxo(utxo).unwrap();

	// Observe it as spent
	let observed =
		ObservedUtxo::new(op.clone(), "btc", "btc_primary_scope", 50000).spent("tx_spending");

	let snapshot = ObservationSnapshot::new(2000, ObservationScope::Incremental).add_utxo(observed);

	let plan = ReconciliationPlanner::new(&store).plan(&snapshot).unwrap();
	store.apply_reconciliation(plan).unwrap();

	let utxo = store.get_utxo(&op).unwrap().unwrap();
	assert_eq!(utxo.onchain_state, OnchainState::Spent);
	let conflict = utxo.observation_conflict.unwrap();
	assert!(conflict.message.contains("spent by tx_spending"));
	assert_eq!(conflict.recorded_at, 2000);
}

#[test]
fn test_no_resurrection_after_spend() {
	let mut store = base_store();
	let op = OutPoint::new("tx1", 0);

	// Setup a reserved UTXO bound to an operation
	let mut utxo = ManagedUtxo::new(op.clone(), "btc", "btc_primary_scope", 50000);
	utxo.availability = Availability::Reserved;
	utxo.operation_binding = Some(txoscope_core::OperationBinding {
		operation_id: "op1".into(),
		phase: txoscope_core::OperationPhase::Selected,
	});
	store.put_utxo(utxo).unwrap();

	// 1. Reconcile as spent
	let observed = ObservedUtxo::new(op.clone(), "btc", "btc_primary_scope", 50000).spent("tx_ext");
	let snapshot = ObservationSnapshot::new(3000, ObservationScope::Incremental).add_utxo(observed);
	let plan = ReconciliationPlanner::new(&store).plan(&snapshot).unwrap();
	store.apply_reconciliation(plan).unwrap();

	let utxo_after_reconcile = store.get_utxo(&op).unwrap().unwrap();
	assert_eq!(utxo_after_reconcile.onchain_state, OnchainState::Spent);

	// 2. Release operation (cleanup)
	store.release_operation(&"op1".into(), txoscope_core::OperationPhase::RollingBack).unwrap();

	// 3. Verify it is NOT selectable
	let utxo_after_release = store.get_utxo(&op).unwrap().unwrap();
	assert_eq!(utxo_after_release.onchain_state, OnchainState::Spent);
	assert_ne!(utxo_after_release.availability, Availability::Selectable);
}

#[test]
fn spent_utxo_does_not_reopen_when_later_observed_unspent() {
	let mut store = base_store();
	let op = OutPoint::new("tx-reopen", 0);

	let mut utxo = ManagedUtxo::new(op.clone(), "btc", "btc_primary_scope", 50_000);
	utxo.mark_spent();
	store.put_utxo(utxo).unwrap();

	let snapshot = ObservationSnapshot::new(3_100, ObservationScope::Incremental).add_utxo(
		ObservedUtxo::new(op.clone(), "btc", "btc_primary_scope", 50_000).confirmed(101, 3_100),
	);

	let plan = ReconciliationPlanner::new(&store).plan(&snapshot).unwrap();
	store.apply_reconciliation(plan).unwrap();

	let persisted = store.get_utxo(&op).unwrap().unwrap();
	assert_eq!(persisted.onchain_state, OnchainState::Spent);
	assert_eq!(persisted.availability, Availability::BlockedByL2);
}

#[test]
fn test_incremental_snapshot_does_not_mark_missing() {
	let mut store = base_store();
	let op1 = OutPoint::new("tx1", 0);
	let op2 = OutPoint::new("tx2", 0);

	store.put_utxo(ManagedUtxo::new(op1.clone(), "btc", "btc_primary_scope", 10000)).unwrap();
	store.put_utxo(ManagedUtxo::new(op2.clone(), "btc", "btc_primary_scope", 20000)).unwrap();

	// Snapshot only contains op1
	let snapshot = ObservationSnapshot::new(4000, ObservationScope::Incremental)
		.add_utxo(ObservedUtxo::new(op1.clone(), "btc", "btc_primary_scope", 10000));

	let plan = ReconciliationPlanner::new(&store).plan(&snapshot).unwrap();
	store.apply_reconciliation(plan).unwrap();

	// op2 should still be selectable
	let utxo2 = store.get_utxo(&op2).unwrap().unwrap();
	assert_eq!(utxo2.availability, Availability::Selectable);

	// Now a FULL snapshot missing op2
	let snapshot_full = ObservationSnapshot::new(4001, ObservationScope::Full)
		.add_utxo(ObservedUtxo::new(op1.clone(), "btc", "btc_primary_scope", 10000));

	let plan_full = ReconciliationPlanner::new(&store).plan(&snapshot_full).unwrap();
	store.apply_reconciliation(plan_full).unwrap();

	// op2 should now be Recovering (Missing)
	let utxo2_full = store.get_utxo(&op2).unwrap().unwrap();
	assert_eq!(utxo2_full.availability, Availability::Recovering);
}

#[test]
fn test_recovery_from_missing_and_conflict() {
	let mut store = base_store();
	let op = OutPoint::new("tx1", 0);

	// 1. Setup a UTXO in Recovering state with a conflict
	let mut utxo = ManagedUtxo::new(op.clone(), "btc", "btc_primary_scope", 10000);
	utxo.availability = Availability::Recovering;
	utxo.observation_conflict = Some(txoscope_core::ObservationConflict {
		message: "Old conflict".to_string(),
		recorded_at: 100,
	});
	store.put_utxo(utxo).unwrap();

	// 2. Observe it as healthy (Unspent + Confirmed)
	let observed =
		ObservedUtxo::new(op.clone(), "btc", "btc_primary_scope", 10000).confirmed(100, 1000);
	let snapshot = ObservationSnapshot::new(2000, ObservationScope::Incremental).add_utxo(observed);

	let plan = ReconciliationPlanner::new(&store).plan(&snapshot).unwrap();
	store.apply_reconciliation(plan).unwrap();

	// 3. Verify it is healed
	let utxo_healed = store.get_utxo(&op).unwrap().unwrap();
	assert_eq!(utxo_healed.availability, Availability::Selectable);
	assert!(utxo_healed.observation_conflict.is_none());
	assert_eq!(utxo_healed.onchain_state, OnchainState::Confirmed);
}

#[test]
fn test_unknown_confirmation_discovery() {
	let mut store = base_store();
	let op = OutPoint::new("tx1", 0);

	// Observed with Unknown confirmation
	let observed = ObservedUtxo::new(op.clone(), "btc", "btc_primary_scope", 10000);
	assert_eq!(observed.confirmation, ObservedConfirmation::Unknown);

	let snapshot = ObservationSnapshot::new(1000, ObservationScope::Incremental).add_utxo(observed);

	let plan = ReconciliationPlanner::new(&store).plan(&snapshot).unwrap();
	store.apply_reconciliation(plan).unwrap();

	let utxo = store.get_utxo(&op).unwrap().unwrap();
	assert_eq!(utxo.onchain_state, OnchainState::Unknown);
	assert_eq!(utxo.availability, Availability::BlockedByPolicy);
}

#[test]
fn test_observed_metadata_mismatch_blocks_selection_without_rewriting_local_state() {
	let mut store = base_store();
	let op = OutPoint::new("tx-meta", 0);

	store.put_utxo(ManagedUtxo::new(op.clone(), "btc", "btc_primary_scope", 50_000)).unwrap();

	let snapshot = ObservationSnapshot::new(5000, ObservationScope::Incremental).add_utxo(
		ObservedUtxo::new(op.clone(), "rgb", "rgb_operational_scope", 60_000).confirmed(101, 5000),
	);

	let plan = ReconciliationPlanner::new(&store).plan(&snapshot).unwrap();
	store.apply_reconciliation(plan).unwrap();

	let utxo = store.get_utxo(&op).unwrap().unwrap();
	assert_eq!(utxo.account_id, "btc".into());
	assert_eq!(utxo.scope_id, "btc_primary_scope".into());
	assert_eq!(utxo.value_sats, 50_000);
	assert_eq!(utxo.availability, Availability::BlockedByPolicy);
	assert!(utxo
		.observation_conflict
		.as_ref()
		.is_some_and(|conflict| conflict.message.contains("Observed metadata mismatch")));
}

#[test]
fn spent_reconcile_clears_binding_and_lock_for_reserved_utxo() {
	let mut store = base_store();
	let op = OutPoint::new("tx-spent", 0);

	store.put_utxo(ManagedUtxo::new(op.clone(), "rgb", "rgb_operational_scope", 50_000)).unwrap();
	store
		.apply_batch(StateBatch {
			selection: OperationSelection {
				operation_id: "op-spent".into(),
				selected_outpoints: vec![op.clone()],
				change_scope_id: "rgb_operational_scope".into(),
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

	let snapshot = ObservationSnapshot::new(6000, ObservationScope::Incremental).add_utxo(
		ObservedUtxo::new(op.clone(), "rgb", "rgb_operational_scope", 50_000).spent("tx-ext"),
	);
	store
		.apply_reconciliation(ReconciliationPlanner::new(&store).plan(&snapshot).unwrap())
		.unwrap();

	let utxo = store.get_utxo(&op).unwrap().unwrap();
	assert!(utxo.operation_binding.is_none());
	assert_eq!(utxo.availability, Availability::BlockedByL2);
	assert!(store.get_lock(&op).unwrap().is_none());
}

#[test]
fn releasing_conflicted_reservation_restores_blocked_by_policy() {
	let mut store = base_store();
	let op = OutPoint::new("tx-conflicted", 0);

	store.put_utxo(ManagedUtxo::new(op.clone(), "rgb", "rgb_operational_scope", 50_000)).unwrap();
	store
		.apply_batch(StateBatch {
			selection: OperationSelection {
				operation_id: "op-conflicted".into(),
				selected_outpoints: vec![op.clone()],
				change_scope_id: "rgb_operational_scope".into(),
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

	let snapshot = ObservationSnapshot::new(6100, ObservationScope::Incremental).add_utxo(
		ObservedUtxo::new(op.clone(), "btc", "btc_primary_scope", 60_000).confirmed(101, 6100),
	);
	store
		.apply_reconciliation(ReconciliationPlanner::new(&store).plan(&snapshot).unwrap())
		.unwrap();
	store.release_operation(&"op-conflicted".into(), OperationPhase::RollingBack).unwrap();

	let utxo = store.get_utxo(&op).unwrap().unwrap();
	assert!(utxo.operation_binding.is_none());
	assert_eq!(utxo.availability, Availability::BlockedByPolicy);
	assert!(utxo.observation_conflict.is_some());
}
