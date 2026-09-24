//! High-level application service and public facade for txoscope operations.
//!
//! `txoscope` is independently developed by Bitlight. It orchestrates
//! account-aware, scope-aware UTXO observation, reconciliation, selection,
//! reservation, and operation lifecycle. It consumes normalized wallet facts
//! and candidate inputs; it does not replace a wallet, signer, indexer, or
//! transaction builder.
//!
//! [`TxoscopeService`] is the application-facing API for bootstrapping
//! managed accounts/scopes, reconciling wallet observations, planning input
//! selections, and creating short-lived manual reservations.
//!
//! Typical usage:
//!
//! - Construct a backend-specific store in the caller.
//! - Wrap it in [`TxoscopeService::with_store`].
//! - Bootstrap accounts/scopes once.
//! - Reconcile snapshots as wallet observations arrive.
//! - Pass [`OutPoint`] values to operation, reservation, and query methods.
//!   Parse external `"txid:vout"` text once at the integration boundary.
//!
//! Time semantics:
//!
//! - `expires_at` values are UTC Unix timestamps in seconds.
//! - `ttl_secs` values are wall-clock durations in seconds and are clamped to
//!   the range `1..=604800` (7 days).
//! - `ReconcileSummary::tick` is an internal monotonic tick, not wall-clock time.
//!
//! Identifier semantics:
//!
//! - [`AccountId`], [`WalletScopeId`], and [`OperationId`] distinguish opaque identifiers.
//! - Operation planning methods require caller-provided [`OperationId`] values.
//! - Manual reservation methods generate opaque reservation IDs internally.
mod asset_selection;
mod clock;
mod mapper;
mod reservation_id;

pub use asset_selection::{
	AssetInputCandidate, AssetSelectionRequest, PlanAssetSelectionRequest, SelectionConstraints,
};

use std::collections::BTreeSet;

use txoscope_core::{
	backend::{
		ManualReservationExactRequest, ManualReservationFirstAvailableRequest, OperationSelection,
		Orchestrator, PlannedExactOutpointsRequest, PlannedSpecificOutpointRequest, StateStore,
	},
	Account, WalletScope,
};

pub use clock::{Clock, UtcClock};
pub use reservation_id::{RandomReservationIdGenerator, ReservationIdGenerator};
pub use txoscope_core::{
	AccountId, AssetCandidateRejection, Availability, Capability, CapabilitySet, CleanupReport,
	DomainError, DomainResult, LockPurpose, ObservationScope, ObservationSnapshot,
	ObservedConfirmation, ObservedSpendStatus, ObservedUtxo, OnchainState, OperationBinding,
	OperationId, OperationPhase, OutPoint, UtxoLockStatus, UtxoStatus, WalletScopeId,
};

/// Account definition used during service bootstrap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BootstrapAccountConfig {
	/// Stable account identifier chosen by the caller.
	pub id: AccountId,
	/// Human-readable account name.
	pub name: String,
	/// Capability set allowed for the account.
	pub capabilities: CapabilitySet,
}

/// Scope definition used during service bootstrap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BootstrapScopeConfig {
	/// Stable scope identifier chosen by the caller.
	pub id: WalletScopeId,
	/// Account that owns this scope.
	pub account_id: AccountId,
	/// Caller-owned descriptor reference.
	pub descriptor_ref: String,
	/// Caller-owned derivation scope label.
	pub derivation_scope: String,
	/// Capability subset enabled for this scope.
	pub capabilities: CapabilitySet,
	/// Lower values are preferred during selection.
	pub priority: u32,
}

/// Full bootstrap payload for creating the managed account/scope topology.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BootstrapConfig {
	/// Logical owner label stored in txoscope state.
	pub owner: String,
	/// Accounts to register before scopes are added.
	pub accounts: Vec<BootstrapAccountConfig>,
	/// Scopes to attach to previously declared accounts.
	pub scopes: Vec<BootstrapScopeConfig>,
}

/// Request to plan an operation against an exact set of outpoints.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanExactOutpointsRequest {
	/// Caller-provided operation identifier.
	pub operation_id: OperationId,
	/// Account that must own all selected outpoints.
	pub target_account_id: AccountId,
	/// Exact transaction outputs.
	pub selected_outpoints: Vec<OutPoint>,
}

/// Request to reserve one specific outpoint for a caller-owned operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReserveSpecificUtxoRequest {
	/// Caller-provided operation identifier.
	pub operation_id: OperationId,
	/// Account that must own the outpoint.
	pub target_account_id: AccountId,
	/// Transaction output to reserve.
	pub outpoint: OutPoint,
	/// Optional UTC Unix timestamp in seconds for lock expiry.
	pub expires_at: Option<u64>,
}

/// Result of a successful planning or operation-bound reservation call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedSelection {
	/// Operation identifier bound to the selection.
	pub operation_id: OperationId,
	/// Selected transaction outputs.
	pub selected_outpoints: Vec<OutPoint>,
	/// Scope that should receive any resulting change.
	pub change_scope_id: WalletScopeId,
	/// Optional UTC Unix timestamp in seconds for expiry.
	pub expires_at: Option<u64>,
}

/// Request to bind one operation to an explicit caller-provided input set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplyExplicitSelectionRequest {
	/// Caller-provided operation identifier.
	pub operation_id: OperationId,
	/// Exact transaction outputs.
	pub selected_outpoints: Vec<OutPoint>,
	/// Scope that should receive any resulting change.
	pub change_scope_id: WalletScopeId,
	/// Optional UTC Unix timestamp in seconds for expiry.
	pub expires_at: Option<u64>,
}

/// Request to create a manual reservation for one outpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReserveManualUtxoRequest {
	/// Account that must own the outpoint.
	pub target_account_id: AccountId,
	/// Transaction output to reserve.
	pub outpoint: OutPoint,
	/// Reservation lifetime in seconds. The service clamps this to `1..=604800` (7 days).
	pub ttl_secs: u64,
}

/// Request to create a manual reservation from the first selectable candidate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReserveFirstAvailableManualUtxoRequest {
	/// Account that must own the selected outpoint.
	pub target_account_id: AccountId,
	/// Candidate transaction outputs.
	pub candidate_outpoints: Vec<OutPoint>,
	/// Reservation lifetime in seconds. The service clamps this to `1..=604800` (7 days).
	pub ttl_secs: u64,
}

/// Result of a manual reservation created by the service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManualReservation {
	/// Service-generated opaque reservation identifier.
	pub reservation_id: OperationId,
	/// Reserved transaction output.
	pub outpoint: OutPoint,
	/// Reservation expiry as a UTC Unix timestamp in seconds.
	pub expires_at: u64,
}

/// Account ownership and bitcoin value of a managed transaction output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedUtxoInfo {
	pub account_id: AccountId,
	pub value_sats: u64,
}

/// Summary of one reconciliation pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReconcileSummary {
	/// Monotonic internal tick after reconciliation.
	pub tick: u64,
	/// Number of observed UTXOs supplied in the snapshot.
	pub observed_utxos: usize,
	/// Number of reconciliation actions applied.
	pub actions_applied: usize,
	/// Observation scope used for this reconciliation pass.
	pub scope: ObservationScope,
}

/// Backend-agnostic high-level service built on top of a caller-supplied store.
#[derive(Debug)]
pub struct TxoscopeService<S, C = UtcClock, G = RandomReservationIdGenerator> {
	orchestrator: Orchestrator<S>,
	clock: C,
	reservation_ids: G,
}

impl<S: StateStore> TxoscopeService<S> {
	/// Wraps a concrete store backend in the facade service.
	pub fn with_store(store: S) -> Self {
		Self::with_store_and_dependencies(store, UtcClock, RandomReservationIdGenerator)
	}
}

impl<S: StateStore, C: Clock, G: ReservationIdGenerator> TxoscopeService<S, C, G> {
	/// Wraps a concrete store backend in the facade service with caller-supplied dependencies.
	pub fn with_store_and_dependencies(store: S, clock: C, reservation_ids: G) -> Self {
		Self { orchestrator: Orchestrator::with_store(store), clock, reservation_ids }
	}

	/// Registers the owner, accounts, and scopes declared in `cfg`.
	///
	/// Callers typically invoke this once before reconciliation or selection.
	pub fn bootstrap(&mut self, cfg: BootstrapConfig) -> DomainResult<()> {
		self.orchestrator.set_owner(cfg.owner);
		for account in cfg.accounts {
			self.orchestrator.add_account(Account::new(
				account.id,
				account.name,
				Vec::new(),
				account.capabilities,
			))?;
		}
		for scope in cfg.scopes {
			self.orchestrator.add_scope(WalletScope::new(
				scope.id,
				scope.account_id,
				scope.descriptor_ref,
				scope.derivation_scope,
				scope.capabilities,
				scope.priority,
			))?;
		}
		Ok(())
	}

	/// Reconciles one observation snapshot and returns a summary of applied work.
	///
	/// This advances the internal tick by one before applying reconciliation.
	pub fn reconcile_snapshot(
		&mut self, snapshot: ObservationSnapshot,
	) -> DomainResult<ReconcileSummary> {
		self.orchestrator.advance_tick(1)?;
		let scope = snapshot.scope;
		let observed_utxos = snapshot.utxos.len();
		let plan = self.orchestrator.reconcile_snapshot(snapshot)?;
		Ok(ReconcileSummary {
			tick: self.orchestrator.current_tick()?,
			observed_utxos,
			actions_applied: plan.actions.len(),
			scope,
		})
	}

	/// Plans an operation against the exact outpoints provided by the caller.
	///
	/// On success the selected UTXOs are reserved for `operation_id`.
	pub fn plan_exact_outpoints(
		&mut self, req: PlanExactOutpointsRequest,
	) -> DomainResult<PlannedSelection> {
		let selection =
			self.orchestrator.prepare_operation(PlannedExactOutpointsRequest::from(req))?;
		Ok(selection.into())
	}

	/// Applies an explicit caller-provided selection without re-running account-scoped selection.
	///
	/// This supports mixed-account flows. Every input and the change scope must belong to a
	/// registered account and scope that both grant `ReserveSupport`. The concrete input set
	/// is validated and locked as a single operation.
	pub fn apply_explicit_selection(
		&mut self, req: ApplyExplicitSelectionRequest,
	) -> DomainResult<PlannedSelection> {
		if req.selected_outpoints.is_empty() {
			return Err(DomainError::InvalidRequest {
				reason: "explicit selection requires at least one outpoint".into(),
			});
		}
		let mut unique_outpoints = BTreeSet::new();
		for outpoint in &req.selected_outpoints {
			if !unique_outpoints.insert(outpoint) {
				return Err(DomainError::InvalidRequest {
					reason: format!("explicit selection includes duplicate outpoint: {outpoint}"),
				});
			}
		}
		if self.orchestrator.get_scope(&req.change_scope_id)?.is_none() {
			return Err(DomainError::ScopeNotFound(req.change_scope_id));
		}
		let selection = OperationSelection {
			operation_id: req.operation_id,
			effects: req
				.selected_outpoints
				.iter()
				.cloned()
				.map(|outpoint| txoscope_core::backend::SelectionEffect::LockUtxo {
					outpoint,
					purpose: LockPurpose::PlannedSelection,
				})
				.collect(),
			selected_outpoints: req.selected_outpoints,
			change_scope_id: req.change_scope_id,
			expires_at: req.expires_at,
		};
		self.orchestrator.apply_selection(&selection)?;
		Ok(selection.into())
	}

	/// Reserves one specific outpoint for a caller-owned operation.
	///
	/// The caller chooses `operation_id`; txoscope validates ownership and applies
	/// a planned-selection lock to the UTXO.
	pub fn reserve_specific_utxo(
		&mut self, req: ReserveSpecificUtxoRequest,
	) -> DomainResult<PlannedSelection> {
		let selection =
			self.orchestrator.prepare_operation(PlannedSpecificOutpointRequest::from(req))?;
		Ok(selection.into())
	}

	/// Creates a manual reservation with a service-generated reservation ID.
	///
	/// The resulting reservation expires at a UTC Unix timestamp in seconds.
	pub fn reserve_manual_utxo(
		&mut self, req: ReserveManualUtxoRequest,
	) -> DomainResult<ManualReservation> {
		let (reservation_id, expires_at) = self.next_manual_reservation(req.ttl_secs);
		let selection = self.orchestrator.prepare_operation(ManualReservationExactRequest::new(
			reservation_id,
			req.target_account_id,
			req.outpoint,
			expires_at,
		))?;
		self.manual_reservation_from_selection(selection)
	}

	fn next_manual_reservation(&self, ttl_secs: u64) -> (OperationId, u64) {
		let now = self.clock.now_utc_unix_secs();
		// Cap at 7 days: reservations aligned to long-lived commitments (e.g. an RGB receive
		// invoice's expiry, which defends the blinded UTXO binding) must be able to outlive
		// the old 1-hour ceiling, or the protection silently lapses before the commitment.
		let expires_at = now.saturating_add(ttl_secs.clamp(1, 7 * 24 * 60 * 60));
		let reservation_id = self.reservation_ids.generate(now);
		(reservation_id, expires_at)
	}

	fn expire_stale_candidate_reservations(
		&mut self, candidate_outpoints: &[OutPoint],
	) -> DomainResult<()> {
		for outpoint in candidate_outpoints {
			let _ = self.get_active_manual_reservation(outpoint)?;
		}
		Ok(())
	}

	fn manual_reservation_from_selection(
		&self, selection: OperationSelection,
	) -> DomainResult<ManualReservation> {
		let reservation_id = selection.operation_id.clone();
		let outpoint = selection.selected_outpoints.first().cloned().ok_or_else(|| {
			DomainError::CorruptedState(format!(
				"manual reservation selection {} has no selected outpoint",
				selection.operation_id
			))
		})?;
		let expires_at = selection.expires_at.ok_or_else(|| {
			DomainError::CorruptedState(format!(
				"manual reservation selection {} is missing expires_at",
				selection.operation_id
			))
		})?;
		Ok(ManualReservation { reservation_id, outpoint, expires_at })
	}

	/// Creates a manual reservation from the first selectable candidate.
	///
	/// Returns `Ok(None)` when none of the candidates is currently selectable.
	pub fn reserve_first_available_manual_utxo(
		&mut self, req: ReserveFirstAvailableManualUtxoRequest,
	) -> DomainResult<Option<ManualReservation>> {
		self.expire_stale_candidate_reservations(&req.candidate_outpoints)?;
		let (reservation_id, expires_at) = self.next_manual_reservation(req.ttl_secs);
		match self.orchestrator.prepare_operation(ManualReservationFirstAvailableRequest::new(
			reservation_id,
			req.target_account_id,
			req.candidate_outpoints,
			expires_at,
		)) {
			Ok(selection) => self.manual_reservation_from_selection(selection).map(Some),
			Err(DomainError::NoCandidateAvailable) => Ok(None),
			Err(err) => Err(err),
		}
	}

	/// Cancels an operation and releases its locks and bindings.
	///
	/// Inputs already marked as spending remain blocked after cancellation; an
	/// unspent observation cannot make a broadcast transaction's inputs selectable.
	pub fn release_operation(&mut self, operation_id: &OperationId) -> DomainResult<Vec<OutPoint>> {
		self.orchestrator.release_operation(operation_id)
	}

	/// Advances an operation to `completed_phase` while retaining its input ownership.
	///
	/// When transitioning out of `selected`, the caller must supply the actual
	/// inputs used to construct the transaction so txoscope can verify they still
	/// match the planned selection. `Constructing` and `Signed` keep the reservation
	/// and observed chain state. `Broadcast`, `AwaitingConfirmation`, and
	/// `AwaitingL2Finality` mark the inputs as spending in flight.
	///
	/// Locks and bindings remain until a spent observation or explicit cancellation.
	/// Advancing beyond `Selected` also ends automatic reservation expiry.
	/// Returns the still-bound inputs updated by this advance; repeating the current
	/// phase returns an empty list.
	pub fn complete_operation(
		&mut self, operation_id: &OperationId, completed_phase: OperationPhase,
		actual_outpoints: impl IntoIterator<Item = OutPoint>,
	) -> DomainResult<Vec<OutPoint>> {
		self.orchestrator.complete_operation(operation_id, completed_phase, actual_outpoints)
	}

	/// Releases selected operations whose age exceeds `max_age` ticks.
	pub fn cleanup_stale_reservations(&mut self, max_age: u64) -> DomainResult<CleanupReport> {
		self.orchestrator.cleanup_stale_reservations(max_age)
	}

	/// Returns the currently planned selection for `operation_id`, if present.
	pub fn get_planned_selection(
		&self, operation_id: &OperationId,
	) -> DomainResult<Option<PlannedSelection>> {
		Ok(self.orchestrator.get_intent(operation_id)?.map(|intent| intent.selection.into()))
	}

	/// Verifies that the actual input set exactly matches the planned selection.
	///
	/// This compares unordered sets of transaction outputs.
	pub fn verify_planned_inputs(
		&self, operation_id: &OperationId, actual_outpoints: impl IntoIterator<Item = OutPoint>,
	) -> DomainResult<()> {
		self.orchestrator.verify_planned_inputs(operation_id, actual_outpoints)
	}

	/// Returns the manual reservation still in `Selected` for `outpoint`, if any.
	///
	/// This does not expire stale reservations automatically. Locks retained by an
	/// operation that has advanced beyond `Selected` are no longer expiring leases.
	pub fn get_manual_reservation(
		&self, outpoint: &OutPoint,
	) -> DomainResult<Option<ManualReservation>> {
		let Some(lock) = self.orchestrator.get_lock(outpoint)? else {
			return Ok(None);
		};
		if lock.purpose != LockPurpose::ManualReservation {
			return Ok(None);
		}
		let selected_operation = self
			.orchestrator
			.get_operation(&lock.operation_id)?
			.is_some_and(|operation| operation.phase == OperationPhase::Selected);
		let selected_binding = self
			.orchestrator
			.get_utxo(outpoint)?
			.and_then(|utxo| utxo.operation_binding)
			.is_some_and(|binding| {
				binding.operation_id == lock.operation_id
					&& binding.phase == OperationPhase::Selected
			});
		if !selected_operation || !selected_binding {
			return Ok(None);
		}
		let Some(expires_at) = lock.expires_at else {
			return Err(DomainError::CorruptedState(format!(
				"manual reservation lock for outpoint {outpoint} is missing expires_at",
			)));
		};
		Ok(Some(ManualReservation {
			reservation_id: lock.operation_id,
			outpoint: outpoint.clone(),
			expires_at,
		}))
	}

	/// Returns the active manual reservation, expiring only leases still in `Selected`.
	pub fn get_active_manual_reservation(
		&mut self, outpoint: &OutPoint,
	) -> DomainResult<Option<ManualReservation>> {
		let Some(reservation) = self.get_manual_reservation(outpoint)? else {
			return Ok(None);
		};
		if reservation.expires_at <= self.clock.now_utc_unix_secs() {
			let _ = self.orchestrator.release_selected_operation(&reservation.reservation_id)?;
			return Ok(None);
		}
		Ok(Some(reservation))
	}

	/// Returns the owning account ID for `outpoint`, if the UTXO is known.
	pub fn utxo_account_id(&self, outpoint: &OutPoint) -> DomainResult<Option<AccountId>> {
		Ok(self.orchestrator.get_utxo(outpoint)?.map(|utxo| utxo.account_id))
	}

	/// Returns `true` if `outpoint` is currently selectable.
	pub fn is_outpoint_selectable(&self, outpoint: &OutPoint) -> DomainResult<bool> {
		self.orchestrator.is_outpoint_selectable(outpoint)
	}

	/// Return recorded chain state, availability, ownership/phase, lock and rejection reason.
	///
	/// Unknown outputs return `None`. This is read-only: expired reservations remain
	/// recorded until cleanup, and request-specific account/scope capabilities are not
	/// evaluated. Backend reads do not promise a transactional snapshot or reserve inputs.
	pub fn utxo_status(&self, outpoint: &OutPoint) -> DomainResult<Option<UtxoStatus>> {
		self.orchestrator.utxo_status(outpoint)
	}

	/// Returns account ownership and bitcoin value for a known output.
	pub fn managed_utxo(&self, outpoint: &OutPoint) -> DomainResult<Option<ManagedUtxoInfo>> {
		Ok(self.orchestrator.get_utxo(outpoint)?.map(|utxo| ManagedUtxoInfo {
			account_id: utxo.account_id,
			value_sats: utxo.value_sats,
		}))
	}

	/// Returns the operation ID currently bound to `outpoint`, in any binding phase.
	///
	/// Ownership remains visible through `Selected`, `Constructing`, `Signed`,
	/// `Broadcast`, and subsequent confirmation/finality phases. Unknown or unbound
	/// outputs return `None`, including after explicit release or an observed spend.
	/// This is not a selectability check: released broadcast inputs remain blocked
	/// even though they no longer have an owner. Use [`Self::is_outpoint_selectable`]
	/// to check whether an output can be selected.
	pub fn owning_operation_id_for_outpoint(
		&self, outpoint: &OutPoint,
	) -> DomainResult<Option<OperationId>> {
		Ok(self
			.orchestrator
			.get_utxo(outpoint)?
			.and_then(|utxo| utxo.operation_binding)
			.map(|binding| binding.operation_id))
	}

	/// Returns the operation ID bound to `outpoint` only while its phase is `Selected`.
	/// Use [`Self::owning_operation_id_for_outpoint`] for ownership in later phases.
	pub fn selected_operation_id_for_outpoint(
		&self, outpoint: &OutPoint,
	) -> DomainResult<Option<OperationId>> {
		Ok(self
			.orchestrator
			.get_utxo(outpoint)?
			.and_then(|utxo| utxo.operation_binding)
			.filter(|binding| binding.phase == OperationPhase::Selected)
			.map(|binding| binding.operation_id))
	}

	/// Returns `true` if the known on-chain state for `outpoint` is spent.
	pub fn is_outpoint_spent(&self, outpoint: &OutPoint) -> DomainResult<bool> {
		Ok(self
			.orchestrator
			.get_utxo(outpoint)?
			.is_some_and(|utxo| utxo.onchain_state == txoscope_core::OnchainState::Spent))
	}

	/// Returns `true` if the persisted operation carries an expiry timestamp.
	pub fn operation_has_expiry(&self, operation_id: &OperationId) -> DomainResult<bool> {
		Ok(self
			.orchestrator
			.get_operation(operation_id)?
			.and_then(|operation| operation.expires_at)
			.is_some())
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn parsed_outpoint(value: &str) -> OutPoint {
		value.parse().unwrap()
	}
	use txoscope_core::{
		backend::{OperationSelection, SelectionEffect},
		ManagedUtxo,
	};
	use txoscope_store_memory::InMemoryStateStore;

	fn service_with_mixed_utxos() -> TxoscopeService<InMemoryStateStore> {
		let mut service = TxoscopeService::with_store(InMemoryStateStore::default());
		service
			.bootstrap(BootstrapConfig {
				owner: "test-owner".to_string(),
				accounts: vec![
					BootstrapAccountConfig {
						id: "btc".into(),
						name: "BTC".to_string(),
						capabilities: CapabilitySet::new([
							Capability::BtcSpend,
							Capability::ReserveSupport,
							Capability::BtcReceive,
							Capability::FeeSupport,
						]),
					},
					BootstrapAccountConfig {
						id: "rgb".into(),
						name: "RGB".to_string(),
						capabilities: CapabilitySet::new([
							Capability::BtcReceive,
							Capability::FeeSupport,
							Capability::L2Settlement,
							Capability::ReserveSupport,
							Capability::SingleTargetReserve,
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
							Capability::ReserveSupport,
							Capability::BtcReceive,
							Capability::FeeSupport,
						]),
						priority: 0,
					},
					BootstrapScopeConfig {
						id: "rgb-scope".into(),
						account_id: "rgb".into(),
						descriptor_ref: "rgb".to_string(),
						derivation_scope: "rgb".to_string(),
						capabilities: CapabilitySet::new([
							Capability::BtcReceive,
							Capability::FeeSupport,
							Capability::L2Settlement,
							Capability::ReserveSupport,
							Capability::SingleTargetReserve,
						]),
						priority: 0,
					},
				],
			})
			.unwrap();
		service
			.orchestrator
			.add_utxo(ManagedUtxo::new(OutPoint::new("tx-rgb", 0), "rgb", "rgb-scope", 25_000))
			.unwrap();
		service
			.orchestrator
			.add_utxo(ManagedUtxo::new(OutPoint::new("tx-btc", 1), "btc", "btc-scope", 40_000))
			.unwrap();
		service
	}

	#[test]
	fn asset_fee_inputs_require_fee_support_on_account_and_scope() {
		for missing_on_account in [false, true] {
			let mut service = service_with_mixed_utxos();
			service
				.orchestrator
				.add_scope(WalletScope::new(
					"fee",
					"rgb",
					"descriptor",
					"external",
					CapabilitySet::new([Capability::ReserveSupport, Capability::FeeSupport]),
					0,
				))
				.unwrap();
			service
				.orchestrator
				.add_utxo(ManagedUtxo::new(OutPoint::new("fee", 0), "rgb", "fee", 50_000))
				.unwrap();
			if missing_on_account {
				service
					.orchestrator
					.add_account(Account::new(
						"rgb",
						"RGB",
						vec!["rgb-scope".into(), "fee".into()],
						CapabilitySet::new([Capability::ReserveSupport]),
					))
					.unwrap();
			} else {
				service
					.orchestrator
					.add_scope(WalletScope::new(
						"fee",
						"rgb",
						"descriptor",
						"external",
						CapabilitySet::new([Capability::ReserveSupport]),
						0,
					))
					.unwrap();
			}
			let mut req = AssetSelectionRequest {
				target_account_id: "rgb".into(),
				candidates: vec![
					AssetInputCandidate {
						outpoint: parsed_outpoint("tx-rgb:0"),
						asset_amount: 10_000,
					},
					AssetInputCandidate { outpoint: parsed_outpoint("fee:0"), asset_amount: 0 },
				],
				asset_amount: 1,
				required_value_sats: 40_000,
				constraints: SelectionConstraints {
					exact_outpoints: None,
					allow_asset_merge: false,
					allow_fee_support: true,
				},
			};
			assert_eq!(
				service.preview_asset_selection(&req),
				Err(DomainError::InsufficientValue { needed: 40_000, have: 25_000 })
			);
			assert!(matches!(
				service.plan_asset_selection(PlanAssetSelectionRequest {
					operation_id: "auto-denied".into(),
					selection: req.clone()
				}),
				Err(DomainError::InsufficientValue { .. })
			));
			req.constraints.exact_outpoints =
				Some(vec![parsed_outpoint("tx-rgb:0"), parsed_outpoint("fee:0")]);
			let expected = if missing_on_account {
				DomainError::MissingAccountCapabilities { account_id: "rgb".into() }
			} else {
				DomainError::MissingCapabilities { scope_id: "fee".into() }
			};
			assert_eq!(service.preview_asset_selection(&req), Err(expected.clone()));
			assert_eq!(
				service.plan_asset_selection(PlanAssetSelectionRequest {
					operation_id: "exact-denied".into(),
					selection: req
				}),
				Err(expected)
			);
			assert!(service.orchestrator.get_lock(&parsed_outpoint("tx-rgb:0")).unwrap().is_none());
			assert!(service.orchestrator.get_lock(&parsed_outpoint("fee:0")).unwrap().is_none());
		}
	}

	#[test]
	fn asset_plan_preserves_fee_role_without_requiring_it_on_carrier_change() {
		let mut service = service_with_mixed_utxos();
		service
			.orchestrator
			.add_scope(WalletScope::new(
				"rgb-scope",
				"rgb",
				"descriptor",
				"external",
				CapabilitySet::new([Capability::ReserveSupport]),
				0,
			))
			.unwrap();
		service
			.orchestrator
			.add_scope(WalletScope::new(
				"fee",
				"rgb",
				"descriptor",
				"external",
				CapabilitySet::new([Capability::ReserveSupport, Capability::FeeSupport]),
				0,
			))
			.unwrap();
		service
			.orchestrator
			.add_utxo(ManagedUtxo::new(OutPoint::new("fee", 0), "rgb", "fee", 50_000))
			.unwrap();
		let mut req = AssetSelectionRequest {
			target_account_id: "rgb".into(),
			candidates: vec![
				AssetInputCandidate { outpoint: parsed_outpoint("tx-rgb:0"), asset_amount: 10_000 },
				AssetInputCandidate { outpoint: parsed_outpoint("fee:0"), asset_amount: 0 },
			],
			asset_amount: 1,
			required_value_sats: 40_000,
			constraints: SelectionConstraints {
				exact_outpoints: Some(vec![parsed_outpoint("tx-rgb:0"), parsed_outpoint("fee:0")]),
				allow_asset_merge: false,
				allow_fee_support: false,
			},
		};
		assert!(matches!(
			service.preview_asset_selection(&req),
			Err(DomainError::UtxoPolicyBlocked { .. })
		));
		req.constraints.exact_outpoints = None;
		req.constraints.allow_fee_support = true;
		assert_eq!(
			service.preview_asset_selection(&req).unwrap(),
			vec![parsed_outpoint("tx-rgb:0"), parsed_outpoint("fee:0")]
		);
		let planned = service
			.plan_asset_selection(PlanAssetSelectionRequest {
				operation_id: "fee-plan".into(),
				selection: req,
			})
			.unwrap();
		assert_eq!(planned.change_scope_id, WalletScopeId::new("rgb-scope"));
		assert_eq!(
			service.orchestrator.get_lock(&parsed_outpoint("fee:0")).unwrap().unwrap().purpose,
			LockPurpose::FeeSupport
		);
		let intent =
			service.orchestrator.get_intent(&OperationId::new("fee-plan")).unwrap().unwrap();
		assert_eq!(
			intent.selection.effects[1],
			SelectionEffect::LockUtxo {
				outpoint: parsed_outpoint("fee:0"),
				purpose: LockPurpose::FeeSupport
			}
		);
	}

	#[test]
	fn explicit_selection_enforces_input_and_change_capabilities_atomically() {
		for case in 0..4 {
			let mut service = service_with_mixed_utxos();
			let mut change_scope_id = WalletScopeId::new("btc-scope");
			let expected = match case {
				0 => {
					service
						.orchestrator
						.add_account(Account::new(
							"rgb",
							"RGB",
							vec!["rgb-scope".into()],
							CapabilitySet::default(),
						))
						.unwrap();
					DomainError::MissingAccountCapabilities { account_id: "rgb".into() }
				},
				1 => {
					service
						.orchestrator
						.add_scope(WalletScope::new(
							"rgb-scope",
							"rgb",
							"descriptor",
							"external",
							CapabilitySet::default(),
							0,
						))
						.unwrap();
					DomainError::MissingCapabilities { scope_id: "rgb-scope".into() }
				},
				2 => {
					service
						.orchestrator
						.add_scope(WalletScope::new(
							"change",
							"btc",
							"descriptor",
							"external",
							CapabilitySet::default(),
							0,
						))
						.unwrap();
					change_scope_id = "change".into();
					DomainError::MissingCapabilities { scope_id: "change".into() }
				},
				_ => {
					service
						.orchestrator
						.add_account(Account::new(
							"change-account",
							"Change",
							vec![],
							CapabilitySet::default(),
						))
						.unwrap();
					service
						.orchestrator
						.add_scope(WalletScope::new(
							"change",
							"change-account",
							"descriptor",
							"external",
							CapabilitySet::new([Capability::ReserveSupport]),
							0,
						))
						.unwrap();
					change_scope_id = "change".into();
					DomainError::MissingAccountCapabilities { account_id: "change-account".into() }
				},
			};
			assert_eq!(
				service.apply_explicit_selection(ApplyExplicitSelectionRequest {
					operation_id: "denied".into(),
					selected_outpoints: vec![
						parsed_outpoint("tx-btc:1"),
						parsed_outpoint("tx-rgb:0")
					],
					change_scope_id,
					expires_at: None,
				}),
				Err(expected)
			);
			for outpoint in [parsed_outpoint("tx-btc:1"), parsed_outpoint("tx-rgb:0")] {
				assert!(service.orchestrator.get_lock(&outpoint).unwrap().is_none());
				assert_eq!(
					service.orchestrator.get_utxo(&outpoint).unwrap().unwrap().availability,
					Availability::Selectable
				);
			}
			assert!(service.orchestrator.list_operations().unwrap().is_empty());
		}
	}

	#[test]
	fn asset_preview_validates_duplicates_and_expires_locks_before_capability_denial() {
		for revoke_account in [false, true] {
			let mut service = service_with_mixed_utxos();
			service
				.orchestrator
				.prepare_operation(ManualReservationExactRequest::new(
					"expired",
					"rgb",
					parsed_outpoint("tx-rgb:0"),
					0,
				))
				.unwrap();
			if revoke_account {
				service
					.orchestrator
					.add_account(Account::new(
						"rgb",
						"RGB",
						vec!["rgb-scope".into()],
						CapabilitySet::default(),
					))
					.unwrap();
			} else {
				service
					.orchestrator
					.add_scope(WalletScope::new(
						"rgb-scope",
						"rgb",
						"descriptor",
						"external",
						CapabilitySet::default(),
						0,
					))
					.unwrap();
			}
			let candidate =
				AssetInputCandidate { outpoint: parsed_outpoint("tx-rgb:0"), asset_amount: 10_000 };
			let mut req = AssetSelectionRequest {
				target_account_id: "rgb".into(),
				candidates: vec![candidate.clone(), candidate],
				asset_amount: 1,
				required_value_sats: 1,
				constraints: SelectionConstraints {
					exact_outpoints: None,
					allow_asset_merge: false,
					allow_fee_support: false,
				},
			};
			assert_eq!(
				service.preview_asset_selection(&req),
				Err(DomainError::InvalidRequest { reason: "duplicate asset candidate".into() })
			);
			assert!(service.orchestrator.get_lock(&parsed_outpoint("tx-rgb:0")).unwrap().is_some());
			req.candidates.pop();
			let expected = if revoke_account {
				DomainError::MissingAccountCapabilities { account_id: "rgb".into() }
			} else {
				DomainError::NoEligibleScope { account_id: "rgb".into() }
			};
			assert_eq!(service.preview_asset_selection(&req), Err(expected));
			assert!(service.orchestrator.get_lock(&parsed_outpoint("tx-rgb:0")).unwrap().is_none());
			assert!(service.is_outpoint_selectable(&parsed_outpoint("tx-rgb:0")).unwrap());
			req.constraints.exact_outpoints =
				Some(vec![parsed_outpoint("tx-rgb:0"), parsed_outpoint("tx-rgb:0")]);
			assert_eq!(
				service.preview_asset_selection(&req),
				Err(DomainError::InvalidRequest { reason: "duplicate exact input".into() })
			);
		}
	}

	#[test]
	fn asset_preview_and_plan_report_no_capable_scopes() {
		let mut service = service_with_mixed_utxos();
		service
			.orchestrator
			.add_scope(WalletScope::new(
				"rgb-scope",
				"rgb",
				"descriptor",
				"external",
				CapabilitySet::default(),
				0,
			))
			.unwrap();
		let mut req = AssetSelectionRequest {
			target_account_id: "rgb".into(),
			candidates: vec![AssetInputCandidate {
				outpoint: parsed_outpoint("tx-rgb:0"),
				asset_amount: 10_000,
			}],
			asset_amount: 1,
			required_value_sats: 1,
			constraints: SelectionConstraints {
				exact_outpoints: None,
				allow_asset_merge: false,
				allow_fee_support: false,
			},
		};
		let expected = DomainError::NoEligibleScope { account_id: "rgb".into() };
		assert_eq!(service.preview_asset_selection(&req), Err(expected.clone()));
		assert_eq!(
			service.plan_asset_selection(PlanAssetSelectionRequest {
				operation_id: "denied".into(),
				selection: req.clone()
			}),
			Err(expected)
		);
		req.constraints.exact_outpoints = Some(vec![]);
		let expected = DomainError::InvalidRequest {
			reason: "exact asset selection requires at least one outpoint".into(),
		};
		assert_eq!(service.preview_asset_selection(&req), Err(expected.clone()));
		assert_eq!(
			service.plan_asset_selection(PlanAssetSelectionRequest {
				operation_id: "empty-denied".into(),
				selection: req.clone()
			}),
			Err(expected)
		);
		req.constraints.exact_outpoints = Some(vec![parsed_outpoint("tx-rgb:0")]);
		assert_eq!(
			service.preview_asset_selection(&req),
			Err(DomainError::MissingCapabilities { scope_id: "rgb-scope".into() })
		);
		assert!(service.orchestrator.get_lock(&parsed_outpoint("tx-rgb:0")).unwrap().is_none());
		assert!(service.get_planned_selection(&OperationId::new("denied")).unwrap().is_none());
	}

	#[test]
	fn asset_preview_and_plan_exclude_unauthorized_scopes() {
		let mut service = service_with_mixed_utxos();
		service
			.orchestrator
			.add_scope(WalletScope::new(
				"blocked",
				"rgb",
				"descriptor",
				"external",
				CapabilitySet::default(),
				0,
			))
			.unwrap();
		service
			.orchestrator
			.add_utxo(ManagedUtxo::new(OutPoint::new("blocked", 0), "rgb", "blocked", 50_000))
			.unwrap();
		let mut req = AssetSelectionRequest {
			target_account_id: "rgb".into(),
			candidates: vec![
				AssetInputCandidate {
					outpoint: parsed_outpoint("blocked:0"),
					asset_amount: 10_000,
				},
				AssetInputCandidate { outpoint: parsed_outpoint("tx-rgb:0"), asset_amount: 10_000 },
			],
			asset_amount: 5_000,
			required_value_sats: 10_000,
			constraints: SelectionConstraints {
				exact_outpoints: None,
				allow_asset_merge: false,
				allow_fee_support: true,
			},
		};
		assert_eq!(
			service.preview_asset_selection(&req).unwrap(),
			vec![parsed_outpoint("tx-rgb:0")]
		);
		let planned = service
			.plan_asset_selection(PlanAssetSelectionRequest {
				operation_id: "authorized".into(),
				selection: req.clone(),
			})
			.unwrap();
		assert_eq!(planned.selected_outpoints, vec![parsed_outpoint("tx-rgb:0")]);
		assert_eq!(planned.change_scope_id, WalletScopeId::new("rgb-scope"));
		assert!(service.orchestrator.get_lock(&parsed_outpoint("blocked:0")).unwrap().is_none());
		service.release_operation(&OperationId::new("authorized")).unwrap();

		req.constraints.exact_outpoints = Some(vec![parsed_outpoint("blocked:0")]);
		let expected = DomainError::MissingCapabilities { scope_id: "blocked".into() };
		assert_eq!(service.preview_asset_selection(&req), Err(expected.clone()));
		assert_eq!(
			service.plan_asset_selection(PlanAssetSelectionRequest {
				operation_id: "denied".into(),
				selection: req.clone()
			}),
			Err(expected)
		);
		assert!(service.get_planned_selection(&OperationId::new("denied")).unwrap().is_none());

		req.constraints.exact_outpoints = None;
		req.candidates[0].asset_amount = 0;
		req.required_value_sats = 40_000;
		assert_eq!(
			service.preview_asset_selection(&req),
			Err(DomainError::InsufficientValue { needed: 40_000, have: 25_000 })
		);
		assert!(matches!(
			service.plan_asset_selection(PlanAssetSelectionRequest {
				operation_id: "no-support".into(),
				selection: req
			}),
			Err(DomainError::InsufficientValue { .. })
		));
	}

	#[test]
	fn asset_preview_and_plan_require_account_reserve_support() {
		let mut service = service_with_mixed_utxos();
		service
			.orchestrator
			.add_account(Account::new(
				"rgb",
				"RGB",
				vec!["rgb-scope".into()],
				CapabilitySet::default(),
			))
			.unwrap();
		let req = AssetSelectionRequest {
			target_account_id: "rgb".into(),
			candidates: vec![AssetInputCandidate {
				outpoint: parsed_outpoint("tx-rgb:0"),
				asset_amount: 10_000,
			}],
			asset_amount: 5_000,
			required_value_sats: 10_000,
			constraints: SelectionConstraints {
				exact_outpoints: None,
				allow_asset_merge: false,
				allow_fee_support: false,
			},
		};
		let expected = DomainError::MissingAccountCapabilities { account_id: "rgb".into() };
		assert_eq!(service.preview_asset_selection(&req), Err(expected.clone()));
		assert_eq!(
			service.plan_asset_selection(PlanAssetSelectionRequest {
				operation_id: "denied".into(),
				selection: req
			}),
			Err(expected)
		);
		assert!(service.orchestrator.get_lock(&parsed_outpoint("tx-rgb:0")).unwrap().is_none());
		assert!(service.get_planned_selection(&OperationId::new("denied")).unwrap().is_none());
	}

	#[test]
	fn asset_selection_skips_reserved_anchor_and_locks_the_available_one() {
		let mut service = service_with_mixed_utxos();
		service
			.orchestrator
			.add_utxo(ManagedUtxo::new(OutPoint::new("small", 0), "rgb", "rgb-scope", 15_000))
			.unwrap();
		service
			.reserve_manual_utxo(ReserveManualUtxoRequest {
				target_account_id: "rgb".into(),
				outpoint: parsed_outpoint("tx-rgb:0"),
				ttl_secs: 600,
			})
			.unwrap();
		let request = AssetSelectionRequest {
			target_account_id: "rgb".into(),
			candidates: vec![
				AssetInputCandidate { outpoint: parsed_outpoint("tx-rgb:0"), asset_amount: 20_000 },
				AssetInputCandidate { outpoint: parsed_outpoint("small:0"), asset_amount: 10_000 },
			],
			asset_amount: 5_000,
			required_value_sats: 10_000,
			constraints: SelectionConstraints {
				exact_outpoints: None,
				allow_asset_merge: false,
				allow_fee_support: false,
			},
		};
		assert_eq!(
			service.preview_asset_selection(&request).unwrap(),
			vec![parsed_outpoint("small:0")]
		);
		assert!(service.is_outpoint_selectable(&parsed_outpoint("small:0")).unwrap());
		let plan = service
			.plan_asset_selection(PlanAssetSelectionRequest {
				operation_id: "funding".into(),
				selection: request.clone(),
			})
			.unwrap();
		assert_eq!(plan.selected_outpoints, vec![parsed_outpoint("small:0")]);
		assert!(matches!(
			service.plan_asset_selection(PlanAssetSelectionRequest {
				operation_id: "other".into(),
				selection: request
			}),
			Err(DomainError::NoEligibleAssetCandidates { .. })
		));
		assert!(service.orchestrator.get_operation(&OperationId::new("other")).unwrap().is_none());
	}

	#[test]
	fn asset_selection_merges_anchors_with_allocation_free_support() {
		let mut service = service_with_mixed_utxos();
		for (txid, value) in [("anchor2", 15_000), ("support", 30_000)] {
			service
				.orchestrator
				.add_utxo(ManagedUtxo::new(OutPoint::new(txid, 0), "rgb", "rgb-scope", value))
				.unwrap();
		}
		let mut request = AssetSelectionRequest {
			target_account_id: "rgb".into(),
			candidates: vec![
				AssetInputCandidate { outpoint: parsed_outpoint("tx-rgb:0"), asset_amount: 3_000 },
				AssetInputCandidate { outpoint: parsed_outpoint("anchor2:0"), asset_amount: 2_000 },
				AssetInputCandidate { outpoint: parsed_outpoint("support:0"), asset_amount: 0 },
			],
			asset_amount: 5_000,
			required_value_sats: 60_000,
			constraints: SelectionConstraints {
				exact_outpoints: None,
				allow_asset_merge: false,
				allow_fee_support: true,
			},
		};
		assert!(matches!(
			service.preview_asset_selection(&request),
			Err(DomainError::InsufficientAsset { .. })
		));
		request.constraints = SelectionConstraints {
			exact_outpoints: None,
			allow_asset_merge: true,
			allow_fee_support: true,
		};
		assert_eq!(
			service.preview_asset_selection(&request).unwrap(),
			vec![
				parsed_outpoint("tx-rgb:0"),
				parsed_outpoint("anchor2:0"),
				parsed_outpoint("support:0")
			]
		);
		request.required_value_sats = 70_001;
		assert!(matches!(
			service.preview_asset_selection(&request),
			Err(DomainError::InsufficientValue { .. })
		));
	}

	#[test]
	fn asset_selection_rejects_duplicates_and_foreign_accounts_without_locks() {
		let mut service = service_with_mixed_utxos();
		let mut request = AssetSelectionRequest {
			target_account_id: "rgb".into(),
			candidates: vec![AssetInputCandidate {
				outpoint: parsed_outpoint("tx-btc:1"),
				asset_amount: 10,
			}],
			asset_amount: 1,
			required_value_sats: 1,
			constraints: SelectionConstraints {
				exact_outpoints: None,
				allow_asset_merge: false,
				allow_fee_support: false,
			},
		};
		let error = service.preview_asset_selection(&request).unwrap_err();
		let DomainError::NoEligibleAssetCandidates { rejections } = error else {
			panic!("expected excluded candidate diagnostics");
		};
		assert_eq!(rejections.len(), 1);
		assert_eq!(rejections[0].outpoint, parsed_outpoint("tx-btc:1"));
		assert!(rejections[0].reason.to_string().contains("belongs to account btc"));
		request.candidates[0].outpoint = parsed_outpoint("missing:0");
		let DomainError::NoEligibleAssetCandidates { rejections } =
			service.preview_asset_selection(&request).unwrap_err()
		else {
			panic!("expected unknown candidate diagnostics");
		};
		assert_eq!(rejections[0].reason, DomainError::UtxoNotFound(parsed_outpoint("missing:0")));
		request.constraints.exact_outpoints = Some(vec![parsed_outpoint("missing:0")]);
		assert_eq!(
			service.preview_asset_selection(&request),
			Err(DomainError::UtxoNotFound(parsed_outpoint("missing:0")))
		);
		request.constraints.exact_outpoints = None;
		request.candidates.push(request.candidates[0].clone());
		assert!(matches!(
			service.plan_asset_selection(PlanAssetSelectionRequest {
				operation_id: "invalid".into(),
				selection: request
			}),
			Err(DomainError::InvalidRequest { .. })
		));
		assert!(service
			.orchestrator
			.get_operation(&OperationId::new("invalid"))
			.unwrap()
			.is_none());
		assert!(service.is_outpoint_selectable(&parsed_outpoint("tx-btc:1")).unwrap());
	}

	#[test]
	fn invalid_selection_requests_preserve_expired_reservations_and_state() {
		for case in 0..7 {
			let mut service = service_with_mixed_utxos();
			service
				.orchestrator
				.prepare_operation(ManualReservationExactRequest::new(
					"expired",
					"rgb",
					parsed_outpoint("tx-rgb:0"),
					0,
				))
				.unwrap();
			let tick = service.orchestrator.current_tick().unwrap();
			let operations = service.orchestrator.list_operations().unwrap();
			let intent = service.orchestrator.get_intent(&OperationId::new("expired")).unwrap();
			let inputs: Vec<_> = [parsed_outpoint("tx-rgb:0"), parsed_outpoint("tx-btc:1")]
				.into_iter()
				.map(|outpoint| {
					(
						outpoint.clone(),
						service.orchestrator.get_utxo(&outpoint).unwrap(),
						service.orchestrator.get_lock(&outpoint).unwrap(),
					)
				})
				.collect();
			let selected_outpoints = if case % 2 == 0 {
				Vec::new()
			} else {
				vec![parsed_outpoint("tx-rgb:0"), parsed_outpoint("tx-rgb:0")]
			};
			let result = match case {
				0 | 1 => service.apply_explicit_selection(ApplyExplicitSelectionRequest {
					operation_id: "invalid".into(),
					selected_outpoints,
					change_scope_id: "rgb-scope".into(),
					expires_at: None,
				}),
				2 | 3 => service.plan_exact_outpoints(PlanExactOutpointsRequest {
					operation_id: "invalid".into(),
					target_account_id: "rgb".into(),
					selected_outpoints,
				}),
				_ => {
					let candidate = AssetInputCandidate {
						outpoint: parsed_outpoint("tx-rgb:0"),
						asset_amount: 10,
					};
					let req = AssetSelectionRequest {
						target_account_id: "rgb".into(),
						candidates: if case == 4 {
							vec![candidate.clone(), candidate]
						} else {
							vec![candidate]
						},
						asset_amount: 1,
						required_value_sats: 1,
						constraints: SelectionConstraints {
							exact_outpoints: (case != 4).then_some(selected_outpoints),
							allow_asset_merge: false,
							allow_fee_support: false,
						},
					};
					assert!(matches!(
						service.preview_asset_selection(&req),
						Err(DomainError::InvalidRequest { .. })
					));
					service.plan_asset_selection(PlanAssetSelectionRequest {
						operation_id: "invalid".into(),
						selection: req,
					})
				},
			};
			assert!(matches!(result, Err(DomainError::InvalidRequest { .. })), "case {case}");
			assert_eq!(service.orchestrator.current_tick().unwrap(), tick);
			assert_eq!(service.orchestrator.list_operations().unwrap(), operations);
			assert_eq!(
				service.orchestrator.get_intent(&OperationId::new("expired")).unwrap(),
				intent
			);
			assert!(service
				.orchestrator
				.get_intent(&OperationId::new("invalid"))
				.unwrap()
				.is_none());
			for (outpoint, utxo, lock) in inputs {
				assert_eq!(service.orchestrator.get_utxo(&outpoint).unwrap(), utxo);
				assert_eq!(service.orchestrator.get_lock(&outpoint).unwrap(), lock);
			}
		}
	}

	#[test]
	fn asset_selection_keeps_serialized_outpoint_order_for_equal_value_carriers() {
		let mut service = service_with_mixed_utxos();
		for vout in [2, 10] {
			service
				.orchestrator
				.add_utxo(ManagedUtxo::new(OutPoint::new("same", vout), "rgb", "rgb-scope", 50_000))
				.unwrap();
		}
		let request = AssetSelectionRequest {
			target_account_id: "rgb".into(),
			candidates: [2, 10]
				.into_iter()
				.map(|vout| AssetInputCandidate {
					outpoint: OutPoint::new("same", vout),
					asset_amount: 10,
				})
				.collect(),
			asset_amount: 1,
			required_value_sats: 1,
			constraints: SelectionConstraints {
				exact_outpoints: None,
				allow_asset_merge: false,
				allow_fee_support: false,
			},
		};
		let expected = vec![OutPoint::new("same", 10)];
		assert_eq!(service.preview_asset_selection(&request).unwrap(), expected);
		assert_eq!(
			service
				.plan_asset_selection(PlanAssetSelectionRequest {
					operation_id: "ordered".into(),
					selection: request
				})
				.unwrap()
				.selected_outpoints,
			expected
		);
		assert!(service.is_outpoint_selectable(&OutPoint::new("same", 2)).unwrap());
	}

	#[test]
	fn advanced_manual_operations_survive_the_original_reservation_expiry() {
		struct FixedClock(u64);
		impl Clock for FixedClock {
			fn now_utc_unix_secs(&self) -> u64 {
				self.0
			}
		}

		for phase in [
			OperationPhase::Constructing,
			OperationPhase::Signed,
			OperationPhase::Broadcast,
			OperationPhase::AwaitingConfirmation,
			OperationPhase::AwaitingL2Finality,
		] {
			let mut service = TxoscopeService {
				orchestrator: service_with_mixed_utxos().orchestrator,
				clock: FixedClock(1_000),
				reservation_ids: RandomReservationIdGenerator,
			};
			let reservation = service
				.reserve_manual_utxo(ReserveManualUtxoRequest {
					target_account_id: "rgb".into(),
					outpoint: parsed_outpoint("tx-rgb:0"),
					ttl_secs: 1,
				})
				.unwrap();
			assert_eq!(reservation.expires_at, 1_001);
			assert_eq!(
				service.get_active_manual_reservation(&parsed_outpoint("tx-rgb:0")).unwrap(),
				Some(reservation.clone())
			);
			service
				.complete_operation(
					&reservation.reservation_id,
					phase,
					vec![parsed_outpoint("tx-rgb:0")],
				)
				.unwrap();
			service.clock.0 = 1_002;

			assert_eq!(service.get_manual_reservation(&parsed_outpoint("tx-rgb:0")).unwrap(), None);
			assert_eq!(
				service.get_active_manual_reservation(&parsed_outpoint("tx-rgb:0")).unwrap(),
				None
			);
			assert_eq!(
				service
					.reserve_first_available_manual_utxo(ReserveFirstAvailableManualUtxoRequest {
						target_account_id: "rgb".into(),
						candidate_outpoints: vec![parsed_outpoint("tx-rgb:0")],
						ttl_secs: 1,
					})
					.unwrap(),
				None
			);
			assert!(service.cleanup_stale_reservations(0).unwrap().released_operations.is_empty());
			assert!(!service.is_outpoint_selectable(&parsed_outpoint("tx-rgb:0")).unwrap());
			let operation =
				service.orchestrator.get_operation(&reservation.reservation_id).unwrap().unwrap();
			assert_eq!(operation.phase, phase);
			assert_eq!(operation.expires_at, None);
			let lock =
				service.orchestrator.get_lock(&parsed_outpoint("tx-rgb:0")).unwrap().unwrap();
			assert_eq!(lock.operation_id, reservation.reservation_id);
			assert_eq!(lock.expires_at, None);
			let binding = service
				.orchestrator
				.get_utxo(&parsed_outpoint("tx-rgb:0"))
				.unwrap()
				.unwrap()
				.operation_binding
				.unwrap();
			assert_eq!(binding.operation_id, reservation.reservation_id);
			assert_eq!(binding.phase, phase);
		}
	}

	#[test]
	fn get_manual_reservation_reports_missing_expiry_as_corrupted_state() {
		let mut service = TxoscopeService::with_store(InMemoryStateStore::default());
		service
			.bootstrap(BootstrapConfig {
				owner: "test-owner".to_string(),
				accounts: vec![BootstrapAccountConfig {
					id: "btc".into(),
					name: "BTC".to_string(),
					capabilities: CapabilitySet::new([
						Capability::BtcSpend,
						Capability::ReserveSupport,
						Capability::SingleTargetReserve,
						Capability::BtcReceive,
					]),
				}],
				scopes: vec![BootstrapScopeConfig {
					id: "btc-scope".into(),
					account_id: "btc".into(),
					descriptor_ref: "bdk".to_string(),
					derivation_scope: "external".to_string(),
					capabilities: CapabilitySet::new([
						Capability::BtcSpend,
						Capability::ReserveSupport,
						Capability::SingleTargetReserve,
						Capability::BtcReceive,
					]),
					priority: 0,
				}],
			})
			.unwrap();
		service
			.orchestrator
			.add_utxo(ManagedUtxo::new(OutPoint::new("tx-manual", 0), "btc", "btc-scope", 25_000))
			.unwrap();
		service
			.orchestrator
			.apply_selection(&OperationSelection {
				operation_id: "manual-corrupt".into(),
				selected_outpoints: vec![parsed_outpoint("tx-manual:0")],
				change_scope_id: "btc-scope".into(),
				effects: vec![SelectionEffect::LockUtxo {
					outpoint: parsed_outpoint("tx-manual:0"),
					purpose: LockPurpose::ManualReservation,
				}],
				expires_at: None,
			})
			.unwrap();

		let err = service.get_manual_reservation(&parsed_outpoint("tx-manual:0")).unwrap_err();
		assert!(matches!(
			err,
			DomainError::CorruptedState(message)
				if message.contains("manual reservation lock")
					&& message.contains("tx-manual:0")
		));
	}

	#[test]
	fn apply_explicit_selection_locks_mixed_account_inputs() {
		let mut service = service_with_mixed_utxos();

		let selection = service
			.apply_explicit_selection(ApplyExplicitSelectionRequest {
				operation_id: "mixed-top-up".into(),
				selected_outpoints: vec![parsed_outpoint("tx-rgb:0"), parsed_outpoint("tx-btc:1")],
				change_scope_id: "btc-scope".into(),
				expires_at: None,
			})
			.unwrap();

		assert_eq!(
			selection.selected_outpoints,
			vec![parsed_outpoint("tx-rgb:0"), parsed_outpoint("tx-btc:1")]
		);
		assert_eq!(selection.change_scope_id, WalletScopeId::new("btc-scope"));
		assert!(service
			.verify_planned_inputs(
				&OperationId::new("mixed-top-up"),
				vec![parsed_outpoint("tx-rgb:0"), parsed_outpoint("tx-btc:1")]
			)
			.is_ok());
		for outpoint in [parsed_outpoint("tx-rgb:0"), parsed_outpoint("tx-btc:1")] {
			let utxo = service.orchestrator.get_utxo(&outpoint).unwrap().unwrap();
			assert_eq!(utxo.availability, Availability::Reserved);
			assert!(utxo.operation_binding.as_ref().is_some_and(|binding| {
				binding.operation_id == OperationId::new("mixed-top-up")
					&& binding.phase == OperationPhase::Selected
			}));
			let lock = service.orchestrator.get_lock(&outpoint).unwrap().unwrap();
			assert_eq!(lock.operation_id, OperationId::new("mixed-top-up"));
			assert_eq!(lock.purpose, LockPurpose::PlannedSelection);
		}
	}

	#[test]
	fn apply_explicit_selection_rejects_empty_inputs() {
		let mut service = service_with_mixed_utxos();

		let err = service
			.apply_explicit_selection(ApplyExplicitSelectionRequest {
				operation_id: "empty-top-up".into(),
				selected_outpoints: Vec::new(),
				change_scope_id: "btc-scope".into(),
				expires_at: None,
			})
			.unwrap_err();

		assert!(matches!(
			err,
			DomainError::InvalidRequest { reason: message }
				if message.contains("explicit selection")
					&& message.contains("at least one outpoint")
		));
		assert!(service
			.orchestrator
			.get_operation(&OperationId::new("empty-top-up"))
			.unwrap()
			.is_none());
	}

	#[test]
	fn apply_explicit_selection_rejects_duplicate_inputs() {
		let mut service = service_with_mixed_utxos();

		let err = service
			.apply_explicit_selection(ApplyExplicitSelectionRequest {
				operation_id: "duplicate-top-up".into(),
				selected_outpoints: vec![parsed_outpoint("tx-rgb:0"), parsed_outpoint("tx-rgb:0")],
				change_scope_id: "btc-scope".into(),
				expires_at: None,
			})
			.unwrap_err();

		assert!(matches!(
			err,
			DomainError::InvalidRequest { reason: message }
				if message.contains("duplicate outpoint")
					&& message.contains("tx-rgb:0")
		));
		assert!(service
			.orchestrator
			.get_operation(&OperationId::new("duplicate-top-up"))
			.unwrap()
			.is_none());
	}

	#[test]
	fn apply_explicit_selection_rejects_unknown_change_scope() {
		let mut service = service_with_mixed_utxos();

		let err = service
			.apply_explicit_selection(ApplyExplicitSelectionRequest {
				operation_id: "unknown-change-scope".into(),
				selected_outpoints: vec![parsed_outpoint("tx-rgb:0")],
				change_scope_id: "missing-scope".into(),
				expires_at: None,
			})
			.unwrap_err();

		assert!(
			matches!(err, DomainError::ScopeNotFound(scope) if scope == WalletScopeId::new("missing-scope"))
		);
		assert!(service
			.orchestrator
			.get_operation(&OperationId::new("unknown-change-scope"))
			.unwrap()
			.is_none());
	}
}
