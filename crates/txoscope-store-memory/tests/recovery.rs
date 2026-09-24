use txoscope_core::OutPoint;
mod common;

use common::{base_store, utxo};
use txoscope_core::{
	backend::{
		BtcSendRequest, IntentStatus, LockPurpose, OperationSelection, Orchestrator,
		SelectionEffect, StateStore,
	},
	Availability, DomainError, OperationPhase,
};
use txoscope_store_memory::InMemoryStateStore;

#[test]
fn recover_pending_intents_replays_and_commits_selection() {
	let mut store = base_store();
	store.put_utxo(utxo("txr1", 0, "btc", "btc_primary_scope", 70_000)).unwrap();

	let selection = Orchestrator::<InMemoryStateStore>::with_store(store.clone())
		.propose_operation(BtcSendRequest::new("op-recover", "btc", 50_000))
		.unwrap();

	store.persist_intent(selection).unwrap();

	let mut orch = Orchestrator::<InMemoryStateStore>::with_store(store);
	let report = orch.recover_pending_intents().unwrap();

	assert_eq!(report.replayed_operations, vec![txoscope_core::OperationId::from("op-recover")]);
	assert!(report.failed_operations.is_empty());
	assert_eq!(
		orch.get_utxo(&OutPoint::new("txr1", 0)).unwrap().unwrap().availability,
		Availability::Reserved
	);
	assert!(orch.pending_intents().unwrap().is_empty());
	assert_eq!(
		orch.get_operation(&"op-recover".into()).unwrap().unwrap().phase,
		OperationPhase::Selected
	);
}

#[test]
fn cleanup_stale_reservations_releases_selected_operation() {
	let mut orch = Orchestrator::<InMemoryStateStore>::with_store(base_store());
	orch.add_utxo(utxo("txc1", 0, "btc", "btc_primary_scope", 90_000)).unwrap();

	let selection =
		orch.prepare_operation(BtcSendRequest::new("op-cleanup", "btc", 50_000)).unwrap();
	assert_eq!(selection.selected_outpoints, vec![OutPoint::new("txc1", 0)]);

	let max_age = 0;
	let report = orch.cleanup_stale_reservations(max_age).unwrap();

	assert_eq!(report.released_operations, vec![txoscope_core::OperationId::from("op-cleanup")]);
	assert_eq!(report.released_outpoints, vec![OutPoint::new("txc1", 0)]);
	assert_eq!(
		orch.get_utxo(&OutPoint::new("txc1", 0)).unwrap().unwrap().availability,
		Availability::Selectable
	);
	assert_eq!(
		orch.get_operation(&"op-cleanup".into()).unwrap().unwrap().phase,
		OperationPhase::RollingBack
	);
}

#[test]
fn prepare_pending_operation_persists_pending_intent_without_locking_utxo() {
	let mut orch = Orchestrator::<InMemoryStateStore>::with_store(base_store());
	orch.add_utxo(utxo("txp1", 0, "btc", "btc_primary_scope", 80_000)).unwrap();

	let selection =
		orch.prepare_pending_operation(BtcSendRequest::new("op-pending", "btc", 50_000)).unwrap();

	assert_eq!(selection.selected_outpoints, vec![OutPoint::new("txp1", 0)]);
	assert_eq!(
		orch.get_utxo(&OutPoint::new("txp1", 0)).unwrap().unwrap().availability,
		Availability::Selectable
	);
	assert_eq!(
		orch.get_intent(&"op-pending".into()).unwrap().unwrap().status,
		IntentStatus::Pending
	);
	assert!(orch.get_operation(&"op-pending".into()).unwrap().is_none());
}

#[test]
fn commit_recorded_intent_replays_pending_selection() {
	let mut orch = Orchestrator::<InMemoryStateStore>::with_store(base_store());
	orch.add_utxo(utxo("txp2", 0, "btc", "btc_primary_scope", 80_000)).unwrap();

	orch.prepare_pending_operation(BtcSendRequest::new("op-commit", "btc", 50_000)).unwrap();

	orch.commit_recorded_intent(&"op-commit".into()).unwrap();

	assert_eq!(
		orch.get_utxo(&OutPoint::new("txp2", 0)).unwrap().unwrap().availability,
		Availability::Reserved
	);
	assert_eq!(
		orch.get_intent(&"op-commit".into()).unwrap().unwrap().status,
		IntentStatus::Committed
	);
	assert_eq!(
		orch.get_operation(&"op-commit".into()).unwrap().unwrap().phase,
		OperationPhase::Selected
	);
}

#[test]
fn fail_recorded_intent_marks_pending_selection_failed() {
	let mut orch = Orchestrator::<InMemoryStateStore>::with_store(base_store());
	orch.add_utxo(utxo("txp3", 0, "btc", "btc_primary_scope", 80_000)).unwrap();

	orch.prepare_pending_operation(BtcSendRequest::new("op-fail", "btc", 50_000)).unwrap();

	orch.fail_recorded_intent(&"op-fail".into()).unwrap();

	assert_eq!(
		orch.get_utxo(&OutPoint::new("txp3", 0)).unwrap().unwrap().availability,
		Availability::Selectable
	);
	assert_eq!(orch.get_intent(&"op-fail".into()).unwrap().unwrap().status, IntentStatus::Failed);
	assert!(orch.get_operation(&"op-fail".into()).unwrap().is_none());
}

#[test]
fn persist_intent_rejects_reusing_operation_id_for_different_selection() {
	let mut store = base_store();
	store.put_utxo(utxo("txi1", 0, "btc", "btc_primary_scope", 60_000)).unwrap();
	store.put_utxo(utxo("txi2", 0, "btc", "btc_primary_scope", 70_000)).unwrap();

	store
		.persist_intent(OperationSelection {
			operation_id: "op-reused".into(),
			selected_outpoints: vec![OutPoint::new("txi1", 0)],
			change_scope_id: "btc_primary_scope".into(),
			effects: vec![SelectionEffect::LockUtxo {
				outpoint: OutPoint::new("txi1", 0),
				purpose: LockPurpose::BtcSend,
			}],
			expires_at: None,
		})
		.unwrap();

	let err = store
		.persist_intent(OperationSelection {
			operation_id: "op-reused".into(),
			selected_outpoints: vec![OutPoint::new("txi2", 0)],
			change_scope_id: "btc_primary_scope".into(),
			effects: vec![SelectionEffect::LockUtxo {
				outpoint: OutPoint::new("txi2", 0),
				purpose: LockPurpose::BtcSend,
			}],
			expires_at: None,
		})
		.unwrap_err();

	assert_eq!(err, DomainError::IntentConflict { operation_id: "op-reused".into() });
}

#[test]
fn complete_operation_verifies_planned_inputs_and_allows_monotonic_phase_advance() {
	let mut orch = Orchestrator::<InMemoryStateStore>::with_store(base_store());
	orch.add_utxo(utxo("txdone", 0, "btc", "btc_primary_scope", 80_000)).unwrap();

	orch.prepare_operation(BtcSendRequest::new("op-complete", "btc", 50_000)).unwrap();

	let err = orch
		.complete_operation(
			&"op-complete".into(),
			OperationPhase::Signed,
			vec![OutPoint::new("wrong", 0)],
		)
		.unwrap_err();
	assert_eq!(err, DomainError::PlannedInputsMismatch { operation_id: "op-complete".into() });

	let completed = orch
		.complete_operation(
			&"op-complete".into(),
			OperationPhase::Signed,
			vec![OutPoint::new("txdone", 0)],
		)
		.unwrap();
	assert_eq!(completed, vec![OutPoint::new("txdone", 0)]);
	assert_eq!(
		orch.get_utxo(&OutPoint::new("txdone", 0)).unwrap().unwrap().availability,
		Availability::Reserved
	);
	assert_eq!(
		orch.get_utxo(&OutPoint::new("txdone", 0)).unwrap().unwrap().onchain_state,
		txoscope_core::OnchainState::Confirmed
	);
	assert_eq!(
		orch.get_operation(&"op-complete".into()).unwrap().unwrap().phase,
		OperationPhase::Signed
	);

	let advanced = orch
		.complete_operation(
			&"op-complete".into(),
			OperationPhase::Broadcast,
			Vec::<OutPoint>::new(),
		)
		.unwrap();
	assert_eq!(advanced, vec![OutPoint::new("txdone", 0)]);
	assert_eq!(
		orch.get_operation(&"op-complete".into()).unwrap().unwrap().phase,
		OperationPhase::Broadcast
	);
}

#[test]
fn rejected_replay_releases_previously_applied_pending_batch() {
	use txoscope_core::backend::{StateBatch, StateRead};
	use txoscope_core::{Capability, CapabilitySet, WalletScope};
	for purpose in [
		LockPurpose::BtcSend,
		LockPurpose::PlannedSelection,
		LockPurpose::ManualReservation,
		LockPurpose::FeeSupport,
	] {
		for path in 0..3 {
			let mut store = base_store();
			let mut account = store.get_account(&"btc".into()).unwrap().unwrap();
			account.capabilities = CapabilitySet::new([
				Capability::BtcSpend,
				Capability::ReserveSupport,
				Capability::SingleTargetReserve,
				Capability::FeeSupport,
			]);
			store.put_account(account.clone()).unwrap();
			store
				.put_scope(WalletScope::new(
					"btc_primary_scope",
					"btc",
					"descriptor",
					"external",
					account.capabilities,
					0,
				))
				.unwrap();
			store.put_utxo(utxo("crash", 0, "btc", "btc_primary_scope", 50_000)).unwrap();
			let selection = OperationSelection {
				operation_id: "crash".into(),
				selected_outpoints: vec![OutPoint::new("crash", 0)],
				change_scope_id: "btc_primary_scope".into(),
				effects: vec![SelectionEffect::LockUtxo {
					outpoint: OutPoint::new("crash", 0),
					purpose,
				}],
				expires_at: Some(100),
			};
			store.persist_intent(selection.clone()).unwrap();
			store
				.apply_batch(StateBatch {
					selection,
					phase: OperationPhase::Selected,
					owner: "test".into(),
				})
				.unwrap();
			store
				.put_scope(WalletScope::new(
					"btc_primary_scope",
					"btc",
					"descriptor",
					"external",
					CapabilitySet::default(),
					0,
				))
				.unwrap();
			let mut orch = Orchestrator::with_store(store);
			assert_eq!(
				orch.complete_operation(
					&"crash".into(),
					OperationPhase::Constructing,
					vec![OutPoint::new("crash", 0)]
				),
				Err(DomainError::IntentNotCommitted("crash".into()))
			);
			match path {
				0 => {
					assert!(matches!(
						orch.commit_recorded_intent(&"crash".into()),
						Err(DomainError::MissingCapabilities { .. })
					));
				},
				1 => {
					assert_eq!(
						orch.recover_pending_intents().unwrap().failed_operations,
						vec![txoscope_core::OperationId::from("crash")]
					);
				},
				_ => {
					orch.fail_recorded_intent(&"crash".into()).unwrap();
				},
			}
			assert!(orch.get_lock(&OutPoint::new("crash", 0)).unwrap().is_none());
			assert_eq!(
				orch.get_utxo(&OutPoint::new("crash", 0)).unwrap().unwrap().availability,
				Availability::Selectable
			);
			assert_eq!(
				orch.get_operation(&"crash".into()).unwrap().unwrap().phase,
				OperationPhase::RollingBack
			);
			assert_eq!(
				orch.get_intent(&"crash".into()).unwrap().unwrap().status,
				IntentStatus::Failed
			);
			assert_eq!(
				orch.complete_operation(
					&"crash".into(),
					OperationPhase::Constructing,
					vec![OutPoint::new("crash", 0)]
				),
				Err(DomainError::IntentNotCommitted("crash".into()))
			);
		}
	}
}

#[test]
fn recovery_does_not_reapply_a_batch_rolled_back_before_failure_status_was_written() {
	use txoscope_core::backend::{StateBatch, StateRead};
	let mut store = base_store();
	store.put_utxo(utxo("rollback", 0, "btc", "btc_primary_scope", 50_000)).unwrap();
	let selection = Orchestrator::with_store(store.clone())
		.propose_operation(BtcSendRequest::new("rollback", "btc", 1))
		.unwrap();
	store.persist_intent(selection.clone()).unwrap();
	store
		.apply_batch(StateBatch {
			selection,
			phase: OperationPhase::Selected,
			owner: "test".into(),
		})
		.unwrap();
	store.release_operation(&"rollback".into(), OperationPhase::RollingBack).unwrap();
	assert_eq!(
		store.get_intent(&"rollback".into()).unwrap().unwrap().status,
		IntentStatus::Pending
	);
	let mut orch = Orchestrator::with_store(store);
	assert_eq!(
		orch.recover_pending_intents().unwrap().failed_operations,
		vec![txoscope_core::OperationId::from("rollback")]
	);
	assert!(orch.get_lock(&OutPoint::new("rollback", 0)).unwrap().is_none());
	assert_eq!(orch.get_intent(&"rollback".into()).unwrap().unwrap().status, IntentStatus::Failed);
}

#[test]
fn rejecting_an_intent_cannot_release_a_committed_reservation() {
	use txoscope_core::backend::{StateBatch, StateRead};
	let mut store = base_store();
	store.put_utxo(utxo("committed", 0, "btc", "btc_primary_scope", 50_000)).unwrap();
	let selection = Orchestrator::with_store(store.clone())
		.propose_operation(BtcSendRequest::new("committed", "btc", 1))
		.unwrap();
	store.persist_intent(selection.clone()).unwrap();
	store
		.apply_batch(StateBatch {
			selection,
			phase: OperationPhase::Selected,
			owner: "test".into(),
		})
		.unwrap();
	store.mark_intent_committed(&"committed".into()).unwrap();
	assert_eq!(
		store.mark_intent_failed(&"committed".into()),
		Err(DomainError::IntentNotPending("committed".into()))
	);
	assert!(store.get_lock(&OutPoint::new("committed", 0)).unwrap().is_some());
	assert_eq!(
		store.get_intent(&"committed".into()).unwrap().unwrap().status,
		IntentStatus::Committed
	);
}

#[test]
fn recovery_retries_authorized_intent_after_rejecting_pending_holder() {
	use txoscope_core::backend::{PlannedSpecificOutpointRequest, StateBatch, StateRead};
	use txoscope_core::{Capability, CapabilitySet};
	for (plan_id, send_id) in [("a-plan", "b-send"), ("b-plan", "a-send")] {
		let mut store = base_store();
		store.put_utxo(utxo("shared", 0, "btc", "btc_primary_scope", 50_000)).unwrap();
		let orch = Orchestrator::with_store(store.clone());
		let plan = orch
			.propose_operation(PlannedSpecificOutpointRequest::new(
				plan_id,
				"btc",
				OutPoint::new("shared", 0),
			))
			.unwrap();
		let send = orch.propose_operation(BtcSendRequest::new(send_id, "btc", 1)).unwrap();
		store.persist_intent(plan).unwrap();
		store.persist_intent(send.clone()).unwrap();
		store
			.apply_batch(StateBatch {
				selection: send,
				phase: OperationPhase::Selected,
				owner: "test".into(),
			})
			.unwrap();
		let mut account = store.get_account(&"btc".into()).unwrap().unwrap();
		account.capabilities = CapabilitySet::new([Capability::ReserveSupport]);
		store.put_account(account).unwrap();
		let mut orch = Orchestrator::with_store(store);
		assert!(matches!(
			orch.commit_recorded_intent(&plan_id.into()),
			Err(DomainError::UtxoUnavailable { .. })
		));
		assert_eq!(
			orch.get_intent(&plan_id.into()).unwrap().unwrap().status,
			IntentStatus::Pending
		);
		let report = orch.recover_pending_intents().unwrap();
		assert_eq!(report.replayed_operations, vec![txoscope_core::OperationId::from(plan_id)]);
		assert_eq!(report.failed_operations, vec![txoscope_core::OperationId::from(send_id)]);
		assert_eq!(
			orch.get_lock(&OutPoint::new("shared", 0)).unwrap().unwrap().operation_id,
			plan_id.into()
		);
		assert!(orch.pending_intents().unwrap().is_empty());
	}
}
