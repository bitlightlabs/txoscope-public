use txoscope::{
	AccountId, BootstrapAccountConfig, BootstrapConfig, BootstrapScopeConfig, Capability,
	CapabilitySet, ManagedUtxoInfo, ObservationScope, ObservationSnapshot, ObservedUtxo,
	OperationId, OutPoint, ReserveFirstAvailableManualUtxoRequest, ReserveManualUtxoRequest,
	TxoscopeService, WalletScopeId,
};
use txoscope_store_memory::InMemoryStateStore;

fn parsed_outpoint(value: &str) -> OutPoint {
	value.parse().unwrap()
}

#[derive(Debug, Clone, Copy)]
struct FixedClock(u64);

impl txoscope::Clock for FixedClock {
	fn now_utc_unix_secs(&self) -> u64 {
		self.0
	}
}

#[derive(Debug, Clone, Copy)]
struct FixedReservationId(&'static str);

impl txoscope::ReservationIdGenerator for FixedReservationId {
	fn generate(&self, now_utc_unix_secs: u64) -> OperationId {
		OperationId::new(format!("{}-{now_utc_unix_secs}", self.0))
	}
}

fn bootstrap_config() -> BootstrapConfig {
	BootstrapConfig {
		owner: "test-owner".to_string(),
		accounts: vec![
			BootstrapAccountConfig {
				id: "btc".into(),
				name: "BTC".to_string(),
				capabilities: CapabilitySet::new([
					Capability::BtcSpend,
					Capability::BtcReceive,
					Capability::FeeSupport,
				]),
			},
			BootstrapAccountConfig {
				id: "rgb".into(),
				name: "RGB".to_string(),
				capabilities: CapabilitySet::new([
					Capability::BtcReceive,
					Capability::ReserveSupport,
					Capability::SingleTargetReserve,
					Capability::L2Settlement,
				]),
			},
		],
		scopes: vec![
			BootstrapScopeConfig {
				id: "btc-scope".into(),
				account_id: "btc".into(),
				descriptor_ref: "bdk".to_string(),
				derivation_scope: "external".to_string(),
				capabilities: CapabilitySet::new([
					Capability::BtcSpend,
					Capability::BtcReceive,
					Capability::FeeSupport,
				]),
				priority: 0,
			},
			BootstrapScopeConfig {
				id: "rgb-operational".into(),
				account_id: "rgb".into(),
				descriptor_ref: "rgb".to_string(),
				derivation_scope: "rgb-operational".to_string(),
				capabilities: CapabilitySet::new([
					Capability::BtcReceive,
					Capability::ReserveSupport,
					Capability::SingleTargetReserve,
					Capability::L2Settlement,
				]),
				priority: 0,
			},
			BootstrapScopeConfig {
				id: "rgb-btc".into(),
				account_id: "rgb".into(),
				descriptor_ref: "rgb".to_string(),
				derivation_scope: "rgb-btc".to_string(),
				capabilities: CapabilitySet::new([
					Capability::BtcReceive,
					Capability::ReserveSupport,
					Capability::FeeSupport,
				]),
				priority: 10,
			},
		],
	}
}

fn seed_snapshot() -> ObservationSnapshot {
	ObservationSnapshot::new(100, ObservationScope::Full)
		.add_utxo(
			ObservedUtxo::new(txoscope::OutPoint::new("tx-btc", 0), "btc", "btc-scope", 50_000)
				.confirmed(100, 100),
		)
		.add_utxo(
			ObservedUtxo::new(
				txoscope::OutPoint::new("tx-rgb-colored", 0),
				"rgb",
				"rgb-operational",
				10_000,
			)
			.confirmed(100, 100),
		)
		.add_utxo(
			ObservedUtxo::new(txoscope::OutPoint::new("tx-rgb-btc", 1), "rgb", "rgb-btc", 20_000)
				.confirmed(100, 100),
		)
}

fn open_service() -> TxoscopeService<InMemoryStateStore> {
	let store = InMemoryStateStore::default();
	let mut service = TxoscopeService::with_store(store);
	service.bootstrap(bootstrap_config()).unwrap();
	service.reconcile_snapshot(seed_snapshot()).unwrap();
	service
}

#[test]
fn ownership_query_tracks_signed_inputs_until_release() {
	let mut service = open_service();
	let outpoint = parsed_outpoint("tx-rgb-colored:0");
	let operation_id = OperationId::new("signed-owner");
	assert_eq!(service.owning_operation_id_for_outpoint(&outpoint).unwrap(), None);
	assert_eq!(
		service.owning_operation_id_for_outpoint(&OutPoint::new("unknown", 0)).unwrap(),
		None
	);
	service
		.plan_exact_outpoints(txoscope::PlanExactOutpointsRequest {
			operation_id: operation_id.clone(),
			target_account_id: "rgb".into(),
			selected_outpoints: vec![outpoint.clone()],
		})
		.unwrap();
	assert_eq!(
		service.selected_operation_id_for_outpoint(&outpoint).unwrap(),
		Some(operation_id.clone())
	);
	assert_eq!(
		service.owning_operation_id_for_outpoint(&outpoint).unwrap(),
		Some(operation_id.clone())
	);
	for phase in [txoscope::OperationPhase::Constructing, txoscope::OperationPhase::Signed] {
		service.complete_operation(&operation_id, phase, vec![outpoint.clone()]).unwrap();
		assert_eq!(service.selected_operation_id_for_outpoint(&outpoint).unwrap(), None);
		assert_eq!(
			service.owning_operation_id_for_outpoint(&outpoint).unwrap(),
			Some(operation_id.clone())
		);
		assert!(!service.is_outpoint_selectable(&outpoint).unwrap());
	}
	service.release_operation(&operation_id).unwrap();
	assert_eq!(service.owning_operation_id_for_outpoint(&outpoint).unwrap(), None);
	assert!(service.is_outpoint_selectable(&outpoint).unwrap());
}

#[test]
fn broadcast_ownership_ends_on_release_or_spend_without_making_inputs_selectable() {
	for release in [false, true] {
		let mut service = open_service();
		let outpoint = parsed_outpoint("tx-rgb-colored:0");
		let operation_id = OperationId::new("broadcast-owner");
		service
			.plan_exact_outpoints(txoscope::PlanExactOutpointsRequest {
				operation_id: operation_id.clone(),
				target_account_id: "rgb".into(),
				selected_outpoints: vec![outpoint.clone()],
			})
			.unwrap();
		for phase in [
			txoscope::OperationPhase::Broadcast,
			txoscope::OperationPhase::AwaitingConfirmation,
			txoscope::OperationPhase::AwaitingL2Finality,
		] {
			service.complete_operation(&operation_id, phase, vec![outpoint.clone()]).unwrap();
			assert_eq!(service.selected_operation_id_for_outpoint(&outpoint).unwrap(), None);
			assert_eq!(
				service.owning_operation_id_for_outpoint(&outpoint).unwrap(),
				Some(operation_id.clone())
			);
			assert!(!service.is_outpoint_selectable(&outpoint).unwrap());
		}
		if release {
			service.release_operation(&operation_id).unwrap();
			assert_eq!(service.owning_operation_id_for_outpoint(&outpoint).unwrap(), None);
			assert!(!service.is_outpoint_selectable(&outpoint).unwrap());
			// A stale unspent observation cannot clear broadcast spending protection.
			service.reconcile_snapshot(seed_snapshot()).unwrap();
			assert_eq!(service.owning_operation_id_for_outpoint(&outpoint).unwrap(), None);
			assert!(!service.is_outpoint_selectable(&outpoint).unwrap());
		}
		service
			.reconcile_snapshot(
				ObservationSnapshot::new(101, ObservationScope::Incremental).add_utxo(
					ObservedUtxo::new(outpoint.clone(), "rgb", "rgb-operational", 10_000)
						.confirmed(101, 101)
						.spent("spending-tx"),
				),
			)
			.unwrap();
		assert_eq!(service.owning_operation_id_for_outpoint(&outpoint).unwrap(), None);
		assert!(!service.is_outpoint_selectable(&outpoint).unwrap());
		assert!(service.is_outpoint_spent(&outpoint).unwrap());
	}
}

#[test]
fn first_available_manual_reservation_skips_scope_without_single_target_reserve() {
	let mut service = open_service();
	let reservation = service
		.reserve_first_available_manual_utxo(ReserveFirstAvailableManualUtxoRequest {
			target_account_id: "rgb".into(),
			candidate_outpoints: vec![
				parsed_outpoint("tx-rgb-colored:0"),
				parsed_outpoint("tx-rgb-btc:1"),
			],
			ttl_secs: 60,
		})
		.unwrap()
		.unwrap();

	assert_eq!(reservation.outpoint, parsed_outpoint("tx-rgb-colored:0"));
	assert_eq!(
		service.selected_operation_id_for_outpoint(&parsed_outpoint("tx-rgb-colored:0")).unwrap(),
		Some(reservation.reservation_id)
	);
}

#[test]
fn unknown_confirmation_outpoint_is_not_selectable_for_manual_reservation() {
	let store = InMemoryStateStore::default();
	let mut service = TxoscopeService::with_store(store);
	service.bootstrap(bootstrap_config()).unwrap();
	service
		.reconcile_snapshot(ObservationSnapshot::new(200, ObservationScope::Full).add_utxo(
			ObservedUtxo::new(
				txoscope::OutPoint::new("tx-rgb-unknown", 0),
				"rgb",
				"rgb-operational",
				15_000,
			),
		))
		.unwrap();

	assert!(!service.is_outpoint_selectable(&parsed_outpoint("tx-rgb-unknown:0")).unwrap());
	assert!(service
		.reserve_first_available_manual_utxo(ReserveFirstAvailableManualUtxoRequest {
			target_account_id: "rgb".into(),
			candidate_outpoints: vec![parsed_outpoint("tx-rgb-unknown:0")],
			ttl_secs: 60,
		})
		.unwrap()
		.is_none());
}

#[test]
fn manual_reservation_uses_injected_clock_and_id_generator() {
	let store = InMemoryStateStore::default();
	let mut service = TxoscopeService::with_store_and_dependencies(
		store,
		FixedClock(1_000),
		FixedReservationId("manual-reservation-fixed"),
	);
	service.bootstrap(bootstrap_config()).unwrap();
	service
		.reconcile_snapshot(
			ObservationSnapshot::new(200, ObservationScope::Full).add_utxo(
				ObservedUtxo::new(
					txoscope::OutPoint::new("tx-rgb-manual", 0),
					"rgb",
					"rgb-operational",
					15_000,
				)
				.confirmed(200, 200),
			),
		)
		.unwrap();

	let reservation = service
		.reserve_manual_utxo(ReserveManualUtxoRequest {
			target_account_id: "rgb".into(),
			outpoint: parsed_outpoint("tx-rgb-manual:0"),
			ttl_secs: 60,
		})
		.unwrap();

	assert_eq!(reservation.reservation_id, OperationId::new("manual-reservation-fixed-1000"));
	assert_eq!(reservation.expires_at, 1_060);
}

#[test]
fn first_available_manual_reservation_uses_now_for_id_generation() {
	let store = InMemoryStateStore::default();
	let mut service = TxoscopeService::with_store_and_dependencies(
		store,
		FixedClock(2_000),
		FixedReservationId("manual-reservation-fixed"),
	);
	service.bootstrap(bootstrap_config()).unwrap();
	service.reconcile_snapshot(seed_snapshot()).unwrap();

	let reservation = service
		.reserve_first_available_manual_utxo(ReserveFirstAvailableManualUtxoRequest {
			target_account_id: "rgb".into(),
			candidate_outpoints: vec![
				parsed_outpoint("tx-rgb-colored:0"),
				parsed_outpoint("tx-rgb-btc:1"),
			],
			ttl_secs: 90,
		})
		.unwrap()
		.unwrap();

	assert_eq!(reservation.reservation_id, OperationId::new("manual-reservation-fixed-2000"));
	assert_eq!(reservation.expires_at, 2_090);
	assert_eq!(reservation.outpoint, parsed_outpoint("tx-rgb-colored:0"));
}

#[test]
fn facade_preserves_identifier_types_across_planning_queries_and_release() {
	let mut service = open_service();
	let account_id = AccountId::new("rgb");
	let operation_id = OperationId::new("typed-operation");
	let scope_id = WalletScopeId::new("rgb-operational");
	let outpoint = parsed_outpoint("tx-rgb-colored:0");
	let planned = service
		.plan_exact_outpoints(txoscope::PlanExactOutpointsRequest {
			operation_id: operation_id.clone(),
			target_account_id: account_id.clone(),
			selected_outpoints: vec![outpoint.clone()],
		})
		.unwrap();
	let planned_operation: &OperationId = &planned.operation_id;
	let change_scope: &WalletScopeId = &planned.change_scope_id;
	assert_eq!(planned_operation, &operation_id);
	assert_eq!(change_scope, &scope_id);
	let owner: AccountId = service.utxo_account_id(&outpoint).unwrap().unwrap();
	let managed: ManagedUtxoInfo = service.managed_utxo(&outpoint).unwrap().unwrap();
	let selected: OperationId =
		service.selected_operation_id_for_outpoint(&outpoint).unwrap().unwrap();
	assert_eq!(owner, account_id);
	assert_eq!(managed, ManagedUtxoInfo { account_id, value_sats: 10_000 });
	assert_eq!(selected, operation_id);
	assert_eq!(service.get_planned_selection(&operation_id).unwrap(), Some(planned));
	service.verify_planned_inputs(&operation_id, vec![outpoint.clone()]).unwrap();
	service
		.complete_operation(&operation_id, txoscope::OperationPhase::Signed, vec![outpoint.clone()])
		.unwrap();
	assert!(!service.operation_has_expiry(&operation_id).unwrap());
	service.release_operation(&operation_id).unwrap();
	assert!(service.is_outpoint_selectable(&outpoint).unwrap());

	let reservation = service
		.reserve_manual_utxo(ReserveManualUtxoRequest {
			target_account_id: owner,
			outpoint: outpoint.clone(),
			ttl_secs: 60,
		})
		.unwrap();
	let reservation_id: OperationId = reservation.reservation_id;
	service.release_operation(&reservation_id).unwrap();
}

#[test]
fn parsed_outpoints_flow_through_observation_planning_and_manual_reservation(
) -> txoscope::DomainResult<()> {
	let mut service = open_service();
	let outpoint: OutPoint = " opaque:txid :10".parse()?;
	assert_eq!(outpoint.txid, " opaque:txid ");
	service.reconcile_snapshot(
		ObservationSnapshot::new(300, ObservationScope::Incremental).add_utxo(
			ObservedUtxo::new(outpoint.clone(), "rgb", "rgb-operational", 20_000)
				.confirmed(300, 300),
		),
	)?;
	let operation_id = OperationId::new("parsed-outpoint");
	let planned = service.plan_exact_outpoints(txoscope::PlanExactOutpointsRequest {
		operation_id: operation_id.clone(),
		target_account_id: AccountId::new("rgb"),
		selected_outpoints: vec![outpoint.clone()],
	})?;
	let selected: &[OutPoint] = &planned.selected_outpoints;
	assert_eq!(selected, std::slice::from_ref(&outpoint));
	service.verify_planned_inputs(&operation_id, vec![outpoint.clone()])?;
	let released: Vec<OutPoint> = service.release_operation(&operation_id)?;
	assert_eq!(released, vec![outpoint.clone()]);
	let reservation = service.reserve_manual_utxo(ReserveManualUtxoRequest {
		target_account_id: AccountId::new("rgb"),
		outpoint: outpoint.clone(),
		ttl_secs: 60,
	})?;
	let reserved: &OutPoint = &reservation.outpoint;
	assert_eq!(reserved, &outpoint);
	assert_eq!(service.get_manual_reservation(&outpoint)?, Some(reservation.clone()));
	service.release_operation(&reservation.reservation_id)?;
	assert!(service.is_outpoint_selectable(&outpoint)?);
	Ok(())
}

#[test]
fn configured_dependencies_support_asset_preview_and_planning_at_reservation_expiry() {
	use std::{cell::Cell, rc::Rc};
	use txoscope::{
		AssetInputCandidate, AssetSelectionRequest, DomainError, PlanAssetSelectionRequest,
		SelectionConstraints,
	};

	struct AdjustableClock(Rc<Cell<u64>>);
	impl txoscope::Clock for AdjustableClock {
		fn now_utc_unix_secs(&self) -> u64 {
			self.0.get()
		}
	}

	for preview_first in [false, true] {
		let now = Rc::new(Cell::new(1_000));
		let mut service = TxoscopeService::with_store_and_dependencies(
			InMemoryStateStore::default(),
			AdjustableClock(now.clone()),
			FixedReservationId("configured"),
		);
		service.bootstrap(bootstrap_config()).unwrap();
		service.reconcile_snapshot(seed_snapshot()).unwrap();
		let outpoint = parsed_outpoint("tx-rgb-colored:0");
		let reservation = service
			.reserve_manual_utxo(ReserveManualUtxoRequest {
				target_account_id: "rgb".into(),
				outpoint: outpoint.clone(),
				ttl_secs: 60,
			})
			.unwrap();
		assert_eq!(reservation.reservation_id, OperationId::new("configured-1000"));
		assert_eq!(reservation.expires_at, 1_060);
		let request = PlanAssetSelectionRequest {
			operation_id: OperationId::new("configured-asset-plan"),
			selection: AssetSelectionRequest {
				target_account_id: "rgb".into(),
				candidates: vec![AssetInputCandidate {
					outpoint: outpoint.clone(),
					asset_amount: 10,
				}],
				asset_amount: 1,
				required_value_sats: 1,
				constraints: SelectionConstraints {
					exact_outpoints: None,
					allow_asset_merge: false,
					allow_fee_support: false,
				},
			},
		};
		now.set(1_059);
		let error = service.preview_asset_selection(&request.selection).unwrap_err();
		let DomainError::NoEligibleAssetCandidates { rejections } = &error else {
			panic!("expected excluded candidate diagnostics, got {error:?}");
		};
		assert_eq!(rejections.len(), 1);
		assert_eq!(rejections[0].outpoint, outpoint);
		assert!(rejections[0].reason.to_string().contains("owned by operation configured-1000"));
		assert!(error.to_string().contains("none of the supplied asset inputs can be used"));
		assert_eq!(service.plan_asset_selection(request.clone()), Err(error));
		assert_eq!(service.get_manual_reservation(&outpoint).unwrap(), Some(reservation.clone()));
		assert!(service.get_planned_selection(&request.operation_id).unwrap().is_none());

		now.set(1_060);
		let expired = service.utxo_status(&outpoint).unwrap().unwrap();
		assert_eq!(expired.lock.as_ref().unwrap().expires_at, Some(1_060));
		assert_eq!(
			expired.lock.as_ref().unwrap().purpose,
			txoscope::LockPurpose::ManualReservation
		);
		assert_eq!(expired.operation.as_ref().unwrap().phase, txoscope::OperationPhase::Selected);
		assert!(!expired.is_selectable());
		assert_eq!(service.get_manual_reservation(&outpoint).unwrap(), Some(reservation.clone()));
		if preview_first {
			assert_eq!(
				service.preview_asset_selection(&request.selection).unwrap(),
				vec![outpoint.clone()]
			);
			assert!(service.get_manual_reservation(&outpoint).unwrap().is_none());
			assert!(service.is_outpoint_selectable(&outpoint).unwrap());
		}
		let plan = service.plan_asset_selection(request.clone()).unwrap();
		assert_eq!(plan.operation_id, request.operation_id);
		assert_eq!(plan.selected_outpoints, vec![outpoint.clone()]);
		assert_eq!(plan.expires_at, None);
		assert!(service.get_manual_reservation(&outpoint).unwrap().is_none());
		assert!(!service.is_outpoint_selectable(&outpoint).unwrap());
		let info: ManagedUtxoInfo = service.managed_utxo(&outpoint).unwrap().unwrap();
		assert_eq!(info.account_id, AccountId::new("rgb"));
		assert_eq!(info.value_sats, 10_000);
		assert_eq!(service.managed_utxo(&OutPoint::new("unknown", 0)).unwrap(), None);
	}
}

#[test]
fn unified_utxo_status_tracks_lifecycle_and_remains_read_only() {
	use txoscope::{Availability, LockPurpose, OnchainState, OperationPhase};
	let mut service = open_service();
	let outpoint = parsed_outpoint("tx-rgb-colored:0");
	assert_eq!(service.utxo_status(&parsed_outpoint("missing:0")).unwrap(), None);
	let initial = service.utxo_status(&outpoint).unwrap().unwrap();
	assert_eq!(initial.account_id, AccountId::new("rgb"));
	assert_eq!(initial.scope_id, WalletScopeId::new("rgb-operational"));
	assert_eq!(initial.value_sats, 10_000);
	assert_eq!(initial.onchain_state, OnchainState::Confirmed);
	assert_eq!(initial.availability, Availability::Selectable);
	assert!(initial.is_selectable());
	assert!(initial.operation.is_none() && initial.lock.is_none());
	let operation_id = OperationId::new("status-lifecycle");
	service
		.plan_exact_outpoints(txoscope::PlanExactOutpointsRequest {
			operation_id: operation_id.clone(),
			target_account_id: "rgb".into(),
			selected_outpoints: vec![outpoint.clone()],
		})
		.unwrap();
	for phase in [
		OperationPhase::Selected,
		OperationPhase::Constructing,
		OperationPhase::Signed,
		OperationPhase::Broadcast,
		OperationPhase::AwaitingConfirmation,
		OperationPhase::AwaitingL2Finality,
	] {
		if phase != OperationPhase::Selected {
			service.complete_operation(&operation_id, phase, vec![outpoint.clone()]).unwrap();
		}
		let status = service.utxo_status(&outpoint).unwrap().unwrap();
		assert_eq!(status.operation.as_ref().unwrap().operation_id, operation_id);
		assert_eq!(status.operation.as_ref().unwrap().phase, phase);
		assert_eq!(status.lock.as_ref().unwrap().operation_id, operation_id);
		assert_eq!(status.lock.as_ref().unwrap().purpose, LockPurpose::PlannedSelection);
		assert_eq!(status.lock.as_ref().unwrap().expires_at, None);
		assert!(!status.is_selectable());
		assert!(status
			.unavailable_reason
			.as_ref()
			.unwrap()
			.to_string()
			.contains("status-lifecycle"));
		assert_eq!(status.is_selectable(), service.is_outpoint_selectable(&outpoint).unwrap());
	}
	service.release_operation(&operation_id).unwrap();
	let status = service.utxo_status(&outpoint).unwrap().unwrap();
	assert!(status.operation.is_none() && status.lock.is_none());
	assert_eq!(status.onchain_state, OnchainState::SpendingInFlight);
	assert!(!status.is_selectable());
	service
		.reconcile_snapshot(
			ObservationSnapshot::new(101, ObservationScope::Incremental).add_utxo(
				ObservedUtxo::new(outpoint.clone(), "rgb", "rgb-operational", 10_000)
					.confirmed(101, 101)
					.spent("spend"),
			),
		)
		.unwrap();
	let status = service.utxo_status(&outpoint).unwrap().unwrap();
	assert_eq!(status.onchain_state, OnchainState::Spent);
	assert!(!status.is_selectable());
}
