use txoscope_core::OutPoint;
mod common;

use common::utxo;
use txoscope_core::{
	backend::{
		BtcSendRequest, ManualReservationExactRequest, ManualReservationFirstAvailableRequest,
		Orchestrator, PlannedExactOutpointsRequest, PlannedSpecificOutpointRequest,
		SelectionRequest,
	},
	Account, Availability, Capability, CapabilitySet, DomainError, WalletScope,
};
use txoscope_store_memory::InMemoryStateStore;

fn orchestrator(
	account_caps: &[Capability], scopes: &[(&str, &[Capability])],
) -> Orchestrator<InMemoryStateStore> {
	let mut orch = Orchestrator::with_store(InMemoryStateStore::default());
	orch.add_account(Account::new(
		"account",
		"Account",
		vec![],
		CapabilitySet::new(account_caps.iter().copied()),
	))
	.unwrap();
	for (priority, (id, caps)) in scopes.iter().enumerate() {
		orch.add_scope(WalletScope::new(
			*id,
			"account",
			"descriptor",
			"external",
			CapabilitySet::new(caps.iter().copied()),
			priority as u32,
		))
		.unwrap();
		orch.add_utxo(utxo(id, 0, "account", id, 10_000 + priority as u64)).unwrap();
	}
	orch
}

fn requests() -> Vec<(SelectionRequest, Vec<Capability>)> {
	use Capability::{BtcSpend, ReserveSupport, SingleTargetReserve};
	vec![
		(BtcSendRequest::new("op", "account", 1).into(), vec![BtcSpend]),
		(
			PlannedExactOutpointsRequest::new("op", "account", vec![OutPoint::new("allowed", 0)])
				.into(),
			vec![ReserveSupport],
		),
		(
			PlannedSpecificOutpointRequest::new("op", "account", OutPoint::new("allowed", 0))
				.into(),
			vec![ReserveSupport],
		),
		(
			ManualReservationExactRequest::new("op", "account", OutPoint::new("allowed", 0), 100)
				.into(),
			vec![ReserveSupport, SingleTargetReserve],
		),
		(
			ManualReservationFirstAvailableRequest::new(
				"op",
				"account",
				vec![OutPoint::new("allowed", 0)],
				100,
			)
			.into(),
			vec![ReserveSupport, SingleTargetReserve],
		),
	]
}

#[test]
fn every_request_requires_each_capability_on_account_and_scope() {
	for (request, required) in requests() {
		for missing in &required {
			let partial: Vec<_> = required.iter().copied().filter(|c| c != missing).collect();
			let scope_error = match &request {
				SelectionRequest::BtcSend(_)
				| SelectionRequest::ManualReservationFirstAvailable(_) => {
					DomainError::NoEligibleScope { account_id: "account".into() }
				},
				_ => DomainError::MissingCapabilities { scope_id: "allowed".into() },
			};
			for (mut orch, expected) in [
				(
					orchestrator(&partial, &[("allowed", &required)]),
					DomainError::MissingAccountCapabilities { account_id: "account".into() },
				),
				(orchestrator(&required, &[("allowed", &partial)]), scope_error),
			] {
				assert_eq!(
					orch.prepare_operation(request.clone()),
					Err(expected),
					"{request:?}, missing {missing:?}"
				);
				assert!(orch.get_lock(&OutPoint::new("allowed", 0)).unwrap().is_none());
				assert!(orch.list_operations().unwrap().is_empty());
				assert!(orch.pending_intents().unwrap().is_empty());
				assert_eq!(
					orch.get_utxo(&OutPoint::new("allowed", 0)).unwrap().unwrap().availability,
					Availability::Selectable
				);
			}
		}
		let mut orch = orchestrator(&required, &[("allowed", &required)]);
		assert_eq!(
			orch.prepare_operation(request).unwrap().selected_outpoints,
			vec![OutPoint::new("allowed", 0)]
		);
	}
}

#[test]
fn btc_send_excludes_unauthorized_inputs_and_change_even_with_better_priority_and_value() {
	use Capability::BtcSpend;
	let orch = orchestrator(&[BtcSpend], &[("blocked", &[]), ("allowed", &[BtcSpend])]);
	let selection = orch.propose_operation(BtcSendRequest::new("op", "account", 1)).unwrap();
	assert_eq!(selection.selected_outpoints, vec![OutPoint::new("allowed", 0)]);
	assert_eq!(selection.change_scope_id, "allowed".into());
	assert_eq!(
		orch.propose_operation(BtcSendRequest::new("op", "account", 15_000)),
		Err(DomainError::InsufficientFunds)
	);
}

#[test]
fn exact_requests_reject_ineligible_scope_despite_another_eligible_scope() {
	for (request, required) in requests().into_iter().skip(1).take(3) {
		let mut orch = orchestrator(&required, &[("allowed", &[]), ("other", &required)]);
		assert_eq!(
			orch.prepare_operation(request),
			Err(DomainError::MissingCapabilities { scope_id: "allowed".into() })
		);
		assert!(orch.get_lock(&OutPoint::new("allowed", 0)).unwrap().is_none());
		assert!(orch.pending_intents().unwrap().is_empty());
	}
}

#[test]
fn first_available_skips_ineligible_scopes_and_rejects_only_ineligible_candidates() {
	use Capability::{ReserveSupport, SingleTargetReserve};
	let caps = [ReserveSupport, SingleTargetReserve];
	let orch = orchestrator(&caps, &[("a-blocked", &[ReserveSupport]), ("z-allowed", &caps)]);
	let selection = orch
		.propose_operation(ManualReservationFirstAvailableRequest::new(
			"op",
			"account",
			vec![OutPoint::new("a-blocked", 0), OutPoint::new("z-allowed", 0)],
			100,
		))
		.unwrap();
	assert_eq!(selection.selected_outpoints, vec![OutPoint::new("z-allowed", 0)]);
	assert_eq!(selection.change_scope_id, "z-allowed".into());
	assert_eq!(
		orch.propose_operation(ManualReservationFirstAvailableRequest::new(
			"op",
			"account",
			vec![OutPoint::new("a-blocked", 0)],
			100
		)),
		Err(DomainError::NoCandidateAvailable)
	);
}

#[test]
fn capabilities_cannot_be_combined_across_scopes() {
	use Capability::{ReserveSupport, SingleTargetReserve};
	let orch = orchestrator(
		&[ReserveSupport, SingleTargetReserve],
		&[("allowed", &[ReserveSupport]), ("other", &[SingleTargetReserve])],
	);
	for (request, expected) in [
		(
			SelectionRequest::from(ManualReservationExactRequest::new(
				"op",
				"account",
				OutPoint::new("allowed", 0),
				100,
			)),
			DomainError::MissingCapabilities { scope_id: "allowed".into() },
		),
		(
			ManualReservationFirstAvailableRequest::new(
				"op",
				"account",
				vec![OutPoint::new("allowed", 0), OutPoint::new("other", 0)],
				100,
			)
			.into(),
			DomainError::NoEligibleScope { account_id: "account".into() },
		),
	] {
		assert_eq!(orch.propose_operation(request), Err(expected));
	}
}

#[test]
fn mixed_exact_inputs_fail_without_reserving_the_eligible_input() {
	use Capability::ReserveSupport;
	let mut orch =
		orchestrator(&[ReserveSupport], &[("allowed", &[ReserveSupport]), ("blocked", &[])]);
	assert_eq!(
		orch.prepare_operation(PlannedExactOutpointsRequest::new(
			"op",
			"account",
			vec![OutPoint::new("allowed", 0), OutPoint::new("blocked", 0)]
		)),
		Err(DomainError::MissingCapabilities { scope_id: "blocked".into() })
	);
	for outpoint in [OutPoint::new("allowed", 0), OutPoint::new("blocked", 0)] {
		assert!(orch.get_lock(&outpoint).unwrap().is_none());
		assert_eq!(
			orch.get_utxo(&outpoint).unwrap().unwrap().availability,
			Availability::Selectable
		);
	}
	assert!(orch.list_operations().unwrap().is_empty());
	assert!(orch.pending_intents().unwrap().is_empty());
}

#[test]
fn account_without_scopes_has_no_eligible_selection() {
	for (request, required) in requests() {
		let mut orch = orchestrator(&required, &[]);
		orch.add_utxo(utxo("allowed", 0, "account", "allowed", 10_000)).unwrap();
		let expected = match &request {
			SelectionRequest::BtcSend(_) | SelectionRequest::ManualReservationFirstAvailable(_) => {
				DomainError::NoEligibleScope { account_id: "account".into() }
			},
			_ => DomainError::ScopeNotFound("allowed".into()),
		};
		assert_eq!(orch.propose_operation(request), Err(expected));
	}
}

#[test]
fn exact_capability_error_is_independent_of_eligible_siblings() {
	for (request, required) in requests().into_iter().skip(1).take(3) {
		for scopes in
			[vec![("allowed", &[][..])], vec![("allowed", &[][..]), ("other", required.as_slice())]]
		{
			let orch = orchestrator(&required, &scopes);
			assert_eq!(
				orch.propose_operation(request.clone()),
				Err(DomainError::MissingCapabilities { scope_id: "allowed".into() })
			);
		}
	}
}

#[test]
fn empty_exact_request_is_invalid_even_without_eligible_scopes() {
	use Capability::ReserveSupport;
	for scopes in [vec![], vec![("blocked", &[][..])]] {
		let orch = orchestrator(&[ReserveSupport], &scopes);
		assert_eq!(
			orch.propose_operation(PlannedExactOutpointsRequest::new("op", "account", vec![])),
			Err(DomainError::InvalidRequest {
				reason: "exact selection requires at least one outpoint".into()
			})
		);
	}
}

fn explicit_selection(
	purpose: txoscope_core::backend::LockPurpose,
) -> txoscope_core::backend::OperationSelection {
	use txoscope_core::backend::{OperationSelection, SelectionEffect};
	OperationSelection {
		operation_id: "explicit".into(),
		selected_outpoints: vec![OutPoint::new("allowed", 0)],
		change_scope_id: "allowed".into(),
		effects: vec![SelectionEffect::LockUtxo { outpoint: OutPoint::new("allowed", 0), purpose }],
		expires_at: Some(100),
	}
}

#[test]
fn direct_apply_checks_every_purposes_account_input_and_change_capabilities() {
	use txoscope_core::backend::{IntentStatus, LockPurpose};
	use Capability::{BtcSpend, ReserveSupport, SingleTargetReserve};
	for (purpose, required) in [
		(LockPurpose::BtcSend, vec![BtcSpend]),
		(LockPurpose::PlannedSelection, vec![ReserveSupport]),
		(LockPurpose::ManualReservation, vec![ReserveSupport, SingleTargetReserve]),
	] {
		for missing in &required {
			let partial: Vec<_> = required.iter().copied().filter(|cap| cap != missing).collect();
			for (mut orch, change, expected) in [
				(
					orchestrator(&partial, &[("allowed", &required)]),
					"allowed",
					DomainError::MissingAccountCapabilities { account_id: "account".into() },
				),
				(
					orchestrator(&required, &[("allowed", &partial), ("other", &required)]),
					"other",
					DomainError::MissingCapabilities { scope_id: "allowed".into() },
				),
				(
					orchestrator(&required, &[("allowed", &required), ("change", &partial)]),
					"change",
					DomainError::MissingCapabilities { scope_id: "change".into() },
				),
			] {
				let mut selection = explicit_selection(purpose);
				selection.change_scope_id = change.into();
				assert_eq!(orch.apply_selection(&selection), Err(expected));
				assert!(orch.get_lock(&OutPoint::new("allowed", 0)).unwrap().is_none());
				assert!(orch.list_operations().unwrap().is_empty());
				assert_eq!(
					orch.get_intent(&"explicit".into()).unwrap().unwrap().status,
					IntentStatus::Failed
				);
			}
		}
		let mut orch = orchestrator(&required, &[("allowed", &required)]);
		let selection = explicit_selection(purpose);
		orch.apply_selection(&selection).unwrap();
		orch.apply_selection(&selection).unwrap();
		assert_eq!(orch.get_lock(&OutPoint::new("allowed", 0)).unwrap().unwrap().purpose, purpose);
	}
}

#[test]
fn application_commit_and_recovery_revalidate_revoked_capabilities() {
	use txoscope_core::backend::IntentStatus;
	for (request, required) in requests() {
		for revoke_account in [false, true] {
			for path in 0..3 {
				let mut orch = orchestrator(&required, &[("allowed", &required)]);
				let selection = orch.propose_operation(request.clone()).unwrap();
				if path != 0 {
					orch.persist_selection_intent(selection.clone()).unwrap();
				}
				if revoke_account {
					orch.add_account(Account::new(
						"account",
						"Account",
						vec!["allowed".into()],
						CapabilitySet::default(),
					))
					.unwrap();
				} else {
					orch.add_scope(WalletScope::new(
						"allowed",
						"account",
						"descriptor",
						"external",
						CapabilitySet::default(),
						0,
					))
					.unwrap();
				}
				if path == 2 {
					let report = orch.recover_pending_intents().unwrap();
					assert_eq!(
						report.failed_operations,
						vec![txoscope_core::OperationId::from("op")]
					);
					assert!(report.replayed_operations.is_empty());
				} else {
					let result = if path == 0 {
						orch.apply_selection(&selection)
					} else {
						orch.commit_recorded_intent(&"op".into())
					};
					let expected = if revoke_account {
						DomainError::MissingAccountCapabilities { account_id: "account".into() }
					} else {
						DomainError::MissingCapabilities { scope_id: "allowed".into() }
					};
					assert_eq!(result, Err(expected));
				}
				assert!(orch.get_lock(&OutPoint::new("allowed", 0)).unwrap().is_none());
				assert!(orch.list_operations().unwrap().is_empty());
				assert_eq!(
					orch.get_intent(&"op".into()).unwrap().unwrap().status,
					IntentStatus::Failed
				);
			}
		}
	}
}

#[test]
fn crafted_selection_cannot_omit_or_duplicate_lock_effects() {
	use txoscope_core::backend::{LockPurpose, SelectionEffect};
	use Capability::BtcSpend;
	for case in 0..5 {
		let mut orch = orchestrator(&[BtcSpend], &[("allowed", &[BtcSpend]), ("blocked", &[])]);
		let mut selection = explicit_selection(LockPurpose::BtcSend);
		match case {
			0 => selection.effects.clear(),
			1 => selection.selected_outpoints.push(OutPoint::new("blocked", 0)),
			2 => selection.effects.push(SelectionEffect::LockUtxo {
				outpoint: OutPoint::new("blocked", 0),
				purpose: LockPurpose::BtcSend,
			}),
			3 => selection.effects.push(selection.effects[0].clone()),
			_ => selection.selected_outpoints.push(OutPoint::new("allowed", 0)),
		}
		assert!(matches!(orch.apply_selection(&selection), Err(DomainError::CorruptedState(_))));
		assert!(orch.get_lock(&OutPoint::new("allowed", 0)).unwrap().is_none());
		assert!(orch.get_lock(&OutPoint::new("blocked", 0)).unwrap().is_none());
		assert!(orch.list_operations().unwrap().is_empty());
	}
}

#[test]
fn memory_batch_rejects_capability_bypass_without_orchestrator() {
	use txoscope_core::backend::{LockPurpose, StateBatch, StateRead, StateStore};
	use txoscope_core::OperationPhase;
	let mut store = common::base_store();
	store.put_utxo(utxo("allowed", 0, "btc", "btc_primary_scope", 50_000)).unwrap();
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
	let mut selection = explicit_selection(LockPurpose::BtcSend);
	selection.change_scope_id = "btc_primary_scope".into();
	let tick = store.current_tick().unwrap();
	assert_eq!(
		store.apply_batch(StateBatch {
			selection,
			phase: OperationPhase::Selected,
			owner: "test".into()
		}),
		Err(DomainError::MissingCapabilities { scope_id: "btc_primary_scope".into() })
	);
	assert_eq!(store.current_tick().unwrap(), tick);
	assert!(store.get_lock(&OutPoint::new("allowed", 0)).unwrap().is_none());
	assert!(store.list_operations().unwrap().is_empty());
}

#[test]
fn memory_replay_rejects_rollback_and_different_lock_identity() {
	use txoscope_core::backend::{LockPurpose, StateBatch, StateRead, StateStore};
	use txoscope_core::OperationPhase;
	for mode in 0..3 {
		let mut store = common::base_store();
		store.put_utxo(utxo("allowed", 0, "rgb", "rgb_operational_scope", 50_000)).unwrap();
		let mut selection = explicit_selection(LockPurpose::ManualReservation);
		selection.change_scope_id = "rgb_operational_scope".into();
		let mut batch =
			StateBatch { selection, phase: OperationPhase::Selected, owner: "test".into() };
		store.apply_batch(batch.clone()).unwrap();
		match mode {
			0 => {
				store.release_operation(&"explicit".into(), OperationPhase::RollingBack).unwrap();
			},
			1 => {
				batch.selection.effects = vec![txoscope_core::backend::SelectionEffect::LockUtxo {
					outpoint: OutPoint::new("allowed", 0),
					purpose: LockPurpose::PlannedSelection,
				}];
			},
			_ => {
				batch.selection.expires_at = Some(200);
			},
		}
		let previous = store.get_lock(&OutPoint::new("allowed", 0)).unwrap();
		let tick = store.current_tick().unwrap();
		let result = store.apply_batch(batch);
		if mode == 0 {
			assert!(matches!(result, Err(DomainError::InvalidOperationTransition { .. })));
		} else {
			assert!(matches!(result, Err(DomainError::IntentConflict { .. })));
		}
		assert_eq!(store.get_lock(&OutPoint::new("allowed", 0)).unwrap(), previous);
		assert_eq!(store.current_tick().unwrap(), tick);
	}
}
