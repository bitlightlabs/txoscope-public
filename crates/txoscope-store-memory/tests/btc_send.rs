use txoscope_core::OutPoint;
mod common;

use common::{base_orchestrator, utxo};
use txoscope_core::{
	backend::{
		BtcSendRequest, LockPurpose, OperationSelection, PlannedExactOutpointsRequest,
		SelectionEffect,
	},
	Availability, DomainError, OperationPhase, UtxoRole,
};

#[test]
fn btc_send_selects_from_btc_scope_and_locks_inputs() {
	let mut orch = base_orchestrator();
	let mut spend = utxo("txa", 0, "btc", "btc_primary_scope", 40_000);
	spend.role = UtxoRole::SpendPreferred;
	orch.add_utxo(spend).unwrap();
	orch.add_utxo(utxo("txb", 1, "btc", "btc_primary_scope", 30_000)).unwrap();

	let selection =
		orch.prepare_operation(BtcSendRequest::new("op-btc-send", "btc", 50_000)).unwrap();

	assert_eq!(selection.change_scope_id, "btc_primary_scope".into());
	assert_eq!(selection.selected_outpoints.len(), 2);
	assert_eq!(
		orch.get_utxo(&OutPoint::new("txa", 0)).unwrap().unwrap().availability,
		Availability::Reserved
	);
}

#[test]
fn btc_send_proposal_is_read_only_until_applied() {
	let mut orch = base_orchestrator();
	let mut spend = utxo("txc", 0, "btc", "btc_primary_scope", 60_000);
	spend.role = UtxoRole::SpendPreferred;
	orch.add_utxo(spend).unwrap();

	let req = BtcSendRequest::new("op-btc-propose", "btc", 50_000);

	let selection = orch.propose_operation(req).unwrap();
	assert_eq!(selection.selected_outpoints, vec![OutPoint::new("txc", 0)]);
	assert_eq!(
		orch.get_utxo(&OutPoint::new("txc", 0)).unwrap().unwrap().availability,
		Availability::Selectable
	);

	orch.apply_selection(&selection).unwrap();
	assert_eq!(
		orch.get_utxo(&OutPoint::new("txc", 0)).unwrap().unwrap().availability,
		Availability::Reserved
	);
}

#[test]
fn apply_selection_is_atomic_when_any_input_is_unavailable() {
	let mut orch = base_orchestrator();
	let mut reserved = utxo("txd", 0, "btc", "btc_primary_scope", 30_000);
	reserved.availability = Availability::Reserved;
	orch.add_utxo(reserved).unwrap();
	orch.add_utxo(utxo("txe", 1, "btc", "btc_primary_scope", 40_000)).unwrap();

	let selection = OperationSelection {
		operation_id: "op-atomic".into(),
		selected_outpoints: vec![OutPoint::new("txd", 0), OutPoint::new("txe", 1)],
		change_scope_id: "btc_primary_scope".into(),
		effects: vec![
			SelectionEffect::LockUtxo {
				outpoint: OutPoint::new("txd", 0),
				purpose: LockPurpose::BtcSend,
			},
			SelectionEffect::LockUtxo {
				outpoint: OutPoint::new("txe", 1),
				purpose: LockPurpose::BtcSend,
			},
		],
		expires_at: None,
	};

	let err = orch.apply_selection(&selection).unwrap_err();
	assert_eq!(
		err,
		DomainError::UtxoUnavailable {
			outpoint: OutPoint::new("txd", 0),
			availability: Availability::Reserved,
		}
	);
	assert_eq!(
		orch.get_utxo(&OutPoint::new("txd", 0)).unwrap().unwrap().availability,
		Availability::Reserved
	);
	assert_eq!(
		orch.get_utxo(&OutPoint::new("txe", 1)).unwrap().unwrap().availability,
		Availability::Selectable
	);
	assert_eq!(orch.get_utxo(&OutPoint::new("txe", 1)).unwrap().unwrap().operation_binding, None);
}

#[test]
fn apply_selection_is_idempotent_for_same_operation() {
	let mut orch = base_orchestrator();
	orch.add_utxo(utxo("txf", 0, "btc", "btc_primary_scope", 80_000)).unwrap();

	let selection =
		orch.propose_operation(BtcSendRequest::new("op-btc-idempotent", "btc", 50_000)).unwrap();

	orch.apply_selection(&selection).unwrap();
	orch.apply_selection(&selection).unwrap();

	let utxo = orch.get_utxo(&OutPoint::new("txf", 0)).unwrap().unwrap();
	assert_eq!(utxo.availability, Availability::Reserved);
	assert_eq!(utxo.operation_binding.unwrap().operation_id, "op-btc-idempotent".into());
	assert_eq!(
		orch.get_operation(&"op-btc-idempotent".into()).unwrap().unwrap().phase,
		OperationPhase::Selected
	);
	assert!(orch.pending_intents().unwrap().is_empty());
}

#[test]
fn planned_exact_outpoints_preserves_caller_order() {
	let mut orch = base_orchestrator();
	orch.add_utxo(utxo("tx-order-a", 0, "btc", "btc_primary_scope", 20_000)).unwrap();
	orch.add_utxo(utxo("tx-order-b", 1, "btc", "btc_primary_scope", 30_000)).unwrap();

	let selection = orch
		.prepare_operation(PlannedExactOutpointsRequest::new(
			"op-preserve-order",
			"btc",
			vec![OutPoint::new("tx-order-b", 1), OutPoint::new("tx-order-a", 0)],
		))
		.unwrap();

	assert_eq!(
		selection.selected_outpoints,
		vec![OutPoint::new("tx-order-b", 1), OutPoint::new("tx-order-a", 0)]
	);
}
