use txoscope_core::{
	backend::{
		BtcSendRequest, LockPurpose, OperationSelection, Orchestrator,
		PlannedSpecificOutpointRequest, ReconciliationPlanner, SelectionEffect, StateBatch,
		StateRead, StateStore,
	},
	Account, Availability, Capability, CapabilitySet, DomainError, ManagedUtxo, ObservationScope,
	ObservationSnapshot, ObservedConfirmation, ObservedSpendStatus, ObservedUtxo, OnchainState,
	OperationBinding, OperationPhase, OutPoint, WalletScope,
};
use txoscope_store_sqlite::SqliteStateStore;

type TestOrchestrator = Orchestrator<SqliteStateStore>;

const FORWARD_PHASES: [OperationPhase; 5] = [
	OperationPhase::Constructing,
	OperationPhase::Signed,
	OperationPhase::Broadcast,
	OperationPhase::AwaitingConfirmation,
	OperationPhase::AwaitingL2Finality,
];

fn initialize(store: &mut SqliteStateStore) {
	let capabilities = CapabilitySet::new([Capability::BtcSpend, Capability::ReserveSupport]);
	store.put_account(Account::new("btc", "BTC", vec![], capabilities.clone())).unwrap();
	store
		.put_scope(WalletScope::new("scope", "btc", "descriptor", "external", capabilities, 0))
		.unwrap();
	store.put_utxo(ManagedUtxo::new(OutPoint::new("input", 0), "btc", "scope", 50_000)).unwrap();
}

fn base_store() -> SqliteStateStore {
	let mut store = SqliteStateStore::open_in_memory().unwrap();
	initialize(&mut store);
	store
}

fn prepare(store: SqliteStateStore) -> TestOrchestrator {
	let mut orch = Orchestrator::with_store(store);
	orch.prepare_operation(
		PlannedSpecificOutpointRequest::new("owner", "btc", OutPoint::new("input", 0))
			.expires_at(1),
	)
	.unwrap();
	orch
}

fn reserve_input(store: &mut SqliteStateStore) {
	let selection = OperationSelection {
		operation_id: "owner".into(),
		selected_outpoints: vec![OutPoint::new("input", 0)],
		change_scope_id: "scope".into(),
		effects: vec![SelectionEffect::LockUtxo {
			outpoint: OutPoint::new("input", 0),
			purpose: LockPurpose::PlannedSelection,
		}],
		expires_at: Some(1),
	};
	store.persist_intent(selection.clone()).unwrap();
	store
		.apply_batch(StateBatch {
			selection,
			phase: OperationPhase::Selected,
			owner: "test".into(),
		})
		.unwrap();
	store.mark_intent_committed(&"owner".into()).unwrap();
}

fn observed() -> ObservedUtxo {
	ObservedUtxo::new(OutPoint::new("input", 0), "btc", "scope", 50_000).confirmed(100, 1_000)
}

fn snapshot(observed: ObservedUtxo) -> ObservationSnapshot {
	ObservationSnapshot::new(1_000, ObservationScope::Incremental).add_utxo(observed)
}

fn assert_owned(orch: &TestOrchestrator, phase: OperationPhase) {
	let utxo = orch.get_utxo(&OutPoint::new("input", 0)).unwrap().unwrap();
	assert_eq!(
		utxo.operation_binding,
		Some(OperationBinding { operation_id: "owner".into(), phase })
	);
	assert_eq!(
		orch.get_lock(&OutPoint::new("input", 0)).unwrap().unwrap().operation_id,
		"owner".into()
	);
	assert_eq!(orch.get_operation(&"owner".into()).unwrap().unwrap().phase, phase);
	assert!(!orch.is_outpoint_selectable(&OutPoint::new("input", 0)).unwrap());
}

fn assert_phase_state(orch: &TestOrchestrator, phase: OperationPhase) {
	assert_owned(orch, phase);
	let utxo = orch.get_utxo(&OutPoint::new("input", 0)).unwrap().unwrap();
	if matches!(phase, OperationPhase::Constructing | OperationPhase::Signed) {
		assert_eq!(utxo.onchain_state, OnchainState::Confirmed);
		assert_eq!(utxo.availability, Availability::Reserved);
	} else {
		assert_in_flight(orch);
	}
}

fn assert_in_flight(orch: &TestOrchestrator) {
	let utxo = orch.get_utxo(&OutPoint::new("input", 0)).unwrap().unwrap();
	assert_eq!(utxo.onchain_state, OnchainState::SpendingInFlight);
	assert_eq!(utxo.availability, Availability::BlockedByL2);
	assert!(!orch.is_outpoint_selectable(&OutPoint::new("input", 0)).unwrap());
}

fn assert_rival_rejected(orch: &mut TestOrchestrator) {
	assert_eq!(
		orch.prepare_operation(BtcSendRequest::new("rival", "btc", 1)),
		Err(DomainError::InsufficientFunds),
	);
}

#[test]
fn forward_phase_progression_retains_ownership_and_disables_reservation_expiry() {
	let mut orch = prepare(base_store());
	let mut original_lock = orch.get_lock(&OutPoint::new("input", 0)).unwrap().unwrap();
	assert_eq!(original_lock.expires_at, Some(1));
	original_lock.expires_at = None;
	for (index, phase) in FORWARD_PHASES.into_iter().enumerate() {
		let actual = if index == 0 { vec![OutPoint::new("input", 0)] } else { Vec::new() };
		assert_eq!(
			orch.complete_operation(&"owner".into(), phase, actual).unwrap(),
			vec![OutPoint::new("input", 0)]
		);
		assert_phase_state(&orch, phase);
		assert_eq!(orch.get_lock(&OutPoint::new("input", 0)).unwrap(), Some(original_lock.clone()));
		assert!(orch.complete_operation(&"owner".into(), phase, Vec::new()).unwrap().is_empty());

		// A signed input still appears unspent, and a broadcast can lag behind the indexer.
		orch.reconcile_snapshot(snapshot(observed())).unwrap();
		assert_phase_state(&orch, phase);
		orch.advance_tick(10_000).unwrap();
		let cleanup = orch.cleanup_stale_reservations(0).unwrap();
		assert!(cleanup.released_operations.is_empty());
		assert!(cleanup.released_outpoints.is_empty());
		assert_owned(&orch, phase);
		assert_rival_rejected(&mut orch);
	}
}

#[test]
fn selected_can_skip_directly_to_each_forward_phase_without_releasing_inputs() {
	for phase in FORWARD_PHASES {
		let mut orch = prepare(base_store());
		assert_eq!(
			orch.complete_operation(&"owner".into(), phase, vec![OutPoint::new("wrong", 0)]),
			Err(DomainError::PlannedInputsMismatch { operation_id: "owner".into() }),
		);
		assert_owned(&orch, OperationPhase::Selected);
		assert_eq!(
			orch.complete_operation(&"owner".into(), phase, vec![OutPoint::new("input", 0)])
				.unwrap(),
			vec![OutPoint::new("input", 0)],
		);
		orch.reconcile_snapshot(snapshot(observed())).unwrap();
		assert_phase_state(&orch, phase);
		assert_rival_rejected(&mut orch);
	}
}

#[test]
fn lagging_and_missing_observations_preserve_bound_and_legacy_in_flight_inputs() {
	for legacy_unbound in [false, true] {
		let mut orch = if legacy_unbound {
			let mut store = base_store();
			let mut utxo = store.get_utxo(&OutPoint::new("input", 0)).unwrap().unwrap();
			utxo.onchain_state = OnchainState::SpendingInFlight;
			utxo.availability = Availability::BlockedByL2;
			store.put_utxo(utxo).unwrap();
			Orchestrator::with_store(store)
		} else {
			let mut orch = prepare(base_store());
			orch.complete_operation(
				&"owner".into(),
				OperationPhase::Broadcast,
				vec![OutPoint::new("input", 0)],
			)
			.unwrap();
			orch
		};
		for confirmation in [
			ObservedConfirmation::Unknown,
			ObservedConfirmation::Mempool,
			ObservedConfirmation::Confirmed { height: 100, timestamp: 1_000 },
		] {
			let mut observation = observed();
			observation.confirmation = confirmation;
			orch.reconcile_snapshot(snapshot(observation)).unwrap();
			assert_in_flight(&orch);
			assert_rival_rejected(&mut orch);
		}

		let mut missing = observed();
		missing.spend_status = ObservedSpendStatus::Missing;
		orch.reconcile_snapshot(snapshot(missing)).unwrap();
		orch.reconcile_snapshot(snapshot(observed())).unwrap();
		assert_in_flight(&orch);
		orch.reconcile_snapshot(ObservationSnapshot::new(1_001, ObservationScope::Full)).unwrap();
		orch.reconcile_snapshot(snapshot(observed())).unwrap();
		assert_in_flight(&orch);
		assert_rival_rejected(&mut orch);
		if legacy_unbound {
			assert!(orch.get_lock(&OutPoint::new("input", 0)).unwrap().is_none());
			assert!(orch
				.get_utxo(&OutPoint::new("input", 0))
				.unwrap()
				.unwrap()
				.operation_binding
				.is_none());
		} else {
			assert_owned(&orch, OperationPhase::Broadcast);
		}
	}
}

#[test]
fn explicit_release_reuses_prebroadcast_inputs_but_quarantines_broadcast_inputs() {
	for phase in FORWARD_PHASES {
		let mut orch = prepare(base_store());
		orch.complete_operation(&"owner".into(), phase, vec![OutPoint::new("input", 0)]).unwrap();
		assert_eq!(
			orch.release_operation(&"owner".into()).unwrap(),
			vec![OutPoint::new("input", 0)]
		);
		assert!(orch.release_operation(&"owner".into()).unwrap().is_empty());
		assert!(orch.get_lock(&OutPoint::new("input", 0)).unwrap().is_none());
		assert!(orch
			.get_utxo(&OutPoint::new("input", 0))
			.unwrap()
			.unwrap()
			.operation_binding
			.is_none());
		orch.reconcile_snapshot(snapshot(observed())).unwrap();
		if matches!(phase, OperationPhase::Constructing | OperationPhase::Signed) {
			assert!(orch.is_outpoint_selectable(&OutPoint::new("input", 0)).unwrap());
			assert_eq!(
				orch.prepare_operation(BtcSendRequest::new("rival", "btc", 1))
					.unwrap()
					.selected_outpoints,
				vec![OutPoint::new("input", 0)],
			);
		} else {
			assert_in_flight(&orch);
			assert_rival_rejected(&mut orch);
		}
	}
}

#[test]
fn observed_spend_removes_ownership_and_later_advancement_cannot_resurrect_input() {
	let mut orch = prepare(base_store());
	orch.complete_operation(
		&"owner".into(),
		OperationPhase::Signed,
		vec![OutPoint::new("input", 0)],
	)
	.unwrap();
	orch.reconcile_snapshot(snapshot(observed().spent("spending_tx"))).unwrap();
	assert!(orch.get_lock(&OutPoint::new("input", 0)).unwrap().is_none());
	assert!(orch
		.get_utxo(&OutPoint::new("input", 0))
		.unwrap()
		.unwrap()
		.operation_binding
		.is_none());

	for phase in [
		OperationPhase::Broadcast,
		OperationPhase::AwaitingConfirmation,
		OperationPhase::AwaitingL2Finality,
	] {
		assert!(orch.complete_operation(&"owner".into(), phase, Vec::new()).unwrap().is_empty());
		assert_eq!(orch.get_operation(&"owner".into()).unwrap().unwrap().phase, phase);
		orch.reconcile_snapshot(snapshot(observed())).unwrap();
		let utxo = orch.get_utxo(&OutPoint::new("input", 0)).unwrap().unwrap();
		assert_eq!(utxo.onchain_state, OnchainState::Spent);
		assert_eq!(utxo.availability, Availability::BlockedByL2);
		assert!(utxo.operation_binding.is_none());
		assert!(orch.get_lock(&OutPoint::new("input", 0)).unwrap().is_none());
		assert_rival_rejected(&mut orch);
	}
	assert!(orch.release_operation(&"owner".into()).unwrap().is_empty());
	assert_eq!(
		orch.get_utxo(&OutPoint::new("input", 0)).unwrap().unwrap().onchain_state,
		OnchainState::Spent
	);
}

#[test]
fn reconciliation_planned_before_broadcast_cannot_clear_the_spending_state() {
	let mut store = base_store();
	reserve_input(&mut store);
	let mut missing = observed();
	missing.spend_status = ObservedSpendStatus::Missing;
	let missing_plan = ReconciliationPlanner::new(&store).plan(&snapshot(missing)).unwrap();
	store.apply_reconciliation(missing_plan).unwrap();
	let stale_plan =
		ReconciliationPlanner::new(&store).plan(&snapshot(observed().mempool())).unwrap();
	assert!(!stale_plan.actions.is_empty());

	store.complete_operation(&"owner".into(), OperationPhase::Broadcast).unwrap();
	store.apply_reconciliation(stale_plan).unwrap();
	assert_eq!(
		store.get_utxo(&OutPoint::new("input", 0)).unwrap().unwrap().onchain_state,
		OnchainState::SpendingInFlight
	);
	store.release_operation(&"owner".into(), OperationPhase::RollingBack).unwrap();
	let mut orch = Orchestrator::with_store(store);
	orch.reconcile_snapshot(snapshot(observed())).unwrap();
	assert_in_flight(&orch);
	assert_rival_rejected(&mut orch);
}

#[test]
fn reopening_preserves_signed_and_broadcast_input_ownership() {
	let directory = tempfile::TempDir::new().unwrap();
	let path = directory.path().join("operation-lifecycle.sqlite");
	let mut store = SqliteStateStore::open(&path).unwrap();
	initialize(&mut store);
	let mut orch = prepare(store);
	orch.complete_operation(
		&"owner".into(),
		OperationPhase::Signed,
		vec![OutPoint::new("input", 0)],
	)
	.unwrap();
	drop(orch);

	let mut orch = Orchestrator::with_store(SqliteStateStore::open(&path).unwrap());
	assert_phase_state(&orch, OperationPhase::Signed);
	assert_eq!(orch.get_lock(&OutPoint::new("input", 0)).unwrap().unwrap().expires_at, None);
	orch.reconcile_snapshot(snapshot(observed())).unwrap();
	assert_rival_rejected(&mut orch);
	assert_eq!(
		orch.complete_operation(&"owner".into(), OperationPhase::Broadcast, Vec::new()).unwrap(),
		vec![OutPoint::new("input", 0)]
	);
	drop(orch);

	let mut orch = Orchestrator::with_store(SqliteStateStore::open(&path).unwrap());
	orch.reconcile_snapshot(snapshot(observed())).unwrap();
	assert_phase_state(&orch, OperationPhase::Broadcast);
	assert!(orch.cleanup_stale_reservations(0).unwrap().released_operations.is_empty());
	assert_rival_rejected(&mut orch);
	orch.reconcile_snapshot(snapshot(observed().spent("spending_tx"))).unwrap();
	drop(orch);

	let orch = Orchestrator::with_store(SqliteStateStore::open(&path).unwrap());
	assert!(orch.get_lock(&OutPoint::new("input", 0)).unwrap().is_none());
	let utxo = orch.get_utxo(&OutPoint::new("input", 0)).unwrap().unwrap();
	assert!(utxo.operation_binding.is_none());
	assert_eq!(utxo.onchain_state, OnchainState::Spent);
}

#[test]
fn conditional_cleanup_cannot_release_inputs_that_advanced_after_the_expiry_check() {
	let mut store = base_store();
	reserve_input(&mut store);
	let stale_candidate = store.get_operation(&"owner".into()).unwrap().unwrap();
	assert_eq!(stale_candidate.phase, OperationPhase::Selected);
	assert_eq!(
		store.complete_operation(&"owner".into(), OperationPhase::Signed).unwrap(),
		vec![OutPoint::new("input", 0)]
	);
	let before_utxo = store.get_utxo(&OutPoint::new("input", 0)).unwrap();
	let before_lock = store.get_lock(&OutPoint::new("input", 0)).unwrap();
	let before_operation = store.get_operation(&"owner".into()).unwrap();
	let before_tick = store.current_tick().unwrap();

	assert_eq!(store.release_selected_operation(&stale_candidate.operation_id).unwrap(), None);
	assert!(store.complete_operation(&"owner".into(), OperationPhase::Signed).unwrap().is_empty());
	assert_eq!(store.get_utxo(&OutPoint::new("input", 0)).unwrap(), before_utxo);
	assert_eq!(store.get_lock(&OutPoint::new("input", 0)).unwrap(), before_lock);
	assert_eq!(store.get_operation(&"owner".into()).unwrap(), before_operation);
	assert_eq!(store.current_tick().unwrap(), before_tick);
	let mut orch = Orchestrator::with_store(store);
	assert_phase_state(&orch, OperationPhase::Signed);
	assert_rival_rejected(&mut orch);
}

#[test]
fn conditional_selected_cleanup_cannot_be_undone_by_stale_backend_completion() {
	let mut store = base_store();
	reserve_input(&mut store);
	let original_operation = store.get_operation(&"owner".into()).unwrap();
	let original_lock = store.get_lock(&OutPoint::new("input", 0)).unwrap();
	let original_tick = store.current_tick().unwrap();
	assert!(store
		.complete_operation(&"owner".into(), OperationPhase::Selected)
		.unwrap()
		.is_empty());
	assert_eq!(store.get_operation(&"owner".into()).unwrap(), original_operation);
	assert_eq!(store.get_lock(&OutPoint::new("input", 0)).unwrap(), original_lock);
	assert_eq!(store.current_tick().unwrap(), original_tick);

	assert_eq!(
		store.release_selected_operation(&"owner".into()).unwrap(),
		Some(vec![OutPoint::new("input", 0)])
	);
	let released_operation = store.get_operation(&"owner".into()).unwrap();
	let released_tick = store.current_tick().unwrap();
	assert_eq!(
		store.complete_operation(&"owner".into(), OperationPhase::Signed),
		Err(DomainError::InvalidOperationTransition {
			operation_id: "owner".into(),
			from: OperationPhase::RollingBack,
			to: OperationPhase::Signed,
		}),
	);
	assert_eq!(store.get_operation(&"owner".into()).unwrap(), released_operation);
	assert_eq!(store.current_tick().unwrap(), released_tick);
	assert!(store.get_lock(&OutPoint::new("input", 0)).unwrap().is_none());
	let utxo = store.get_utxo(&OutPoint::new("input", 0)).unwrap().unwrap();
	assert!(utxo.operation_binding.is_none());
	assert_eq!(utxo.onchain_state, OnchainState::Confirmed);
	assert_eq!(utxo.availability, Availability::Selectable);
}

#[test]
fn stale_discovery_cannot_overwrite_an_input_imported_and_broadcast_since_planning() {
	let mut store = SqliteStateStore::open_in_memory().unwrap();
	let stale_plan = ReconciliationPlanner::new(&store).plan(&snapshot(observed())).unwrap();
	assert_eq!(stale_plan.actions.len(), 1);
	initialize(&mut store);
	reserve_input(&mut store);
	store.complete_operation(&"owner".into(), OperationPhase::Broadcast).unwrap();
	let before_utxo = store.get_utxo(&OutPoint::new("input", 0)).unwrap();
	let before_lock = store.get_lock(&OutPoint::new("input", 0)).unwrap();

	store.apply_reconciliation(stale_plan).unwrap();
	assert_eq!(store.get_utxo(&OutPoint::new("input", 0)).unwrap(), before_utxo);
	assert_eq!(store.get_lock(&OutPoint::new("input", 0)).unwrap(), before_lock);
	assert_eq!(
		store.release_operation(&"owner".into(), OperationPhase::RollingBack).unwrap(),
		vec![OutPoint::new("input", 0)]
	);
	let mut orch = Orchestrator::with_store(store);
	orch.reconcile_snapshot(snapshot(observed())).unwrap();
	assert_in_flight(&orch);
	assert_rival_rejected(&mut orch);
}

#[test]
fn first_available_preserves_serialized_outpoint_order_for_numeric_vouts() {
	let mut store = base_store();
	let capabilities =
		CapabilitySet::new([Capability::ReserveSupport, Capability::SingleTargetReserve]);
	store.put_account(Account::new("btc", "BTC", vec![], capabilities.clone())).unwrap();
	store
		.put_scope(WalletScope::new("scope", "btc", "descriptor", "external", capabilities, 0))
		.unwrap();
	for vout in [2, 10] {
		store
			.put_utxo(ManagedUtxo::new(OutPoint::new("tx", vout), "btc", "scope", 50_000))
			.unwrap();
	}
	let mut orch = Orchestrator::with_store(store);
	let selection = orch
		.prepare_operation(txoscope_core::backend::ManualReservationFirstAvailableRequest::new(
			"ordered",
			"btc",
			vec![OutPoint::new("tx", 2), OutPoint::new("tx", 10)],
			100,
		))
		.unwrap();
	assert_eq!(selection.selected_outpoints, vec![OutPoint::new("tx", 10)]);
	assert!(orch.get_lock(&OutPoint::new("tx", 10)).unwrap().is_some());
	assert!(orch.is_outpoint_selectable(&OutPoint::new("tx", 2)).unwrap());
}
