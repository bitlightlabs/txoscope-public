use std::collections::BTreeSet;

use txoscope_core::backend::{LockPurpose, OperationSelection, SelectionEffect, StateStore};

use crate::{
	AccountId, Capability, Clock, DomainError, DomainResult, OperationId, OutPoint,
	PlannedSelection, ReservationIdGenerator, TxoscopeService,
};

/// Asset facts supplied by the asset runtime. Values and availability come from the ledger.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AssetInputCandidate {
	pub outpoint: OutPoint,
	/// Zero denotes an allocation-free fee-support input.
	pub asset_amount: u64,
}

pub use txoscope_core::AssetSelectionConstraints as SelectionConstraints;
use txoscope_core::{AssetCandidate, AssetCandidateRejection, AssetSelectionPolicy};

/// Select a single asset and the bitcoin value needed to carry it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AssetSelectionRequest {
	pub target_account_id: AccountId,
	pub candidates: Vec<AssetInputCandidate>,
	pub asset_amount: u64,
	/// Includes the transaction builder's fee budget.
	pub required_value_sats: u64,
	pub constraints: SelectionConstraints,
}

/// Select asset inputs and bind them to a caller-owned operation.
///
/// Preview and planning share the selection request. The account, scopes, and
/// candidate observations must already be registered with the service.
///
/// ```no_run
/// use txoscope::{
///     AssetInputCandidate, AssetSelectionRequest, DomainResult, OperationId,
///     OutPoint, PlanAssetSelectionRequest, SelectionConstraints, TxoscopeService,
/// };
/// use txoscope_store_memory::InMemoryStateStore;
///
/// # fn plan(service: &mut TxoscopeService<InMemoryStateStore>) -> DomainResult<()> {
/// let selection = AssetSelectionRequest {
///     target_account_id: "asset-account".into(),
///     candidates: vec![AssetInputCandidate {
///         outpoint: "tx1:0".parse::<OutPoint>()?,
///         asset_amount: 100,
///     }],
///     asset_amount: 10,
///     required_value_sats: 1_000,
///     constraints: SelectionConstraints {
///         exact_outpoints: None,
///         allow_asset_merge: false,
///         allow_fee_support: false,
///     },
/// };
/// let preview = service.preview_asset_selection(&selection)?;
/// let planned = service.plan_asset_selection(PlanAssetSelectionRequest {
///     operation_id: OperationId::new("asset-send"),
///     selection,
/// })?;
/// assert_eq!(planned.selected_outpoints, preview);
/// # Ok(())
/// # }
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlanAssetSelectionRequest {
	pub operation_id: OperationId,
	pub selection: AssetSelectionRequest,
}

impl<S: StateStore, C: Clock, G: ReservationIdGenerator> TxoscopeService<S, C, G> {
	/// Preview uses the same selector as planning, without creating an operation or locks.
	/// Expired manual reservations are cleaned up using the service clock.
	pub fn preview_asset_selection(
		&mut self, req: &AssetSelectionRequest,
	) -> DomainResult<Vec<OutPoint>> {
		if req.asset_amount == 0 || req.candidates.is_empty() {
			return Err(DomainError::InsufficientFunds);
		}
		AssetSelectionPolicy::validate_inputs(
			req.candidates.iter().map(|candidate| &candidate.outpoint),
			req.asset_amount,
			&req.constraints,
		)?;
		let mut candidate_outpoints: Vec<_> =
			req.candidates.iter().map(|candidate| candidate.outpoint.clone()).collect();
		candidate_outpoints.sort_by_key(ToString::to_string);
		self.expire_stale_candidate_reservations(&candidate_outpoints)?;
		let account = self
			.orchestrator
			.get_account(&req.target_account_id)?
			.ok_or_else(|| DomainError::AccountNotFound(req.target_account_id.clone()))?;
		if !account.capabilities.contains(&Capability::ReserveSupport) {
			return Err(DomainError::MissingAccountCapabilities { account_id: account.id.clone() });
		}
		let mut registered_scope_ids = BTreeSet::new();
		let mut eligible_scope_ids = BTreeSet::new();
		let mut fee_scope_ids = BTreeSet::new();
		for scope_id in &account.scopes {
			if let Some(scope) = self.orchestrator.get_scope(scope_id)? {
				if scope.account_id == account.id {
					registered_scope_ids.insert(scope.id.clone());
					if scope.capabilities.contains(&Capability::ReserveSupport) {
						if scope.capabilities.contains(&Capability::FeeSupport) {
							fee_scope_ids.insert(scope.id.clone());
						}
						eligible_scope_ids.insert(scope.id);
					}
				}
			}
		}
		if eligible_scope_ids.is_empty()
			&& !req
				.constraints
				.exact_outpoints
				.as_ref()
				.is_some_and(|outpoints| !outpoints.is_empty())
		{
			return Err(DomainError::NoEligibleScope { account_id: account.id.clone() });
		}

		let mut eligible = Vec::new();
		let mut rejections = Vec::new();
		for candidate in &req.candidates {
			let Some(utxo) = self.orchestrator.utxo_status(&candidate.outpoint)? else {
				rejections.push(AssetCandidateRejection {
					outpoint: candidate.outpoint.clone(),
					reason: DomainError::UtxoNotFound(candidate.outpoint.clone()),
				});
				continue;
			};
			if utxo.account_id != req.target_account_id {
				rejections.push(AssetCandidateRejection {
					outpoint: candidate.outpoint.clone(),
					reason: DomainError::UtxoPolicyBlocked {
						outpoint: candidate.outpoint.clone(),
						reason: format!(
							"input belongs to account {}, not requested account {}",
							utxo.account_id, req.target_account_id
						),
					},
				});
				continue;
			}
			if !eligible_scope_ids.contains(&utxo.scope_id) {
				let reason = if registered_scope_ids.contains(&utxo.scope_id) {
					DomainError::MissingCapabilities { scope_id: utxo.scope_id }
				} else {
					DomainError::ScopeNotFound(utxo.scope_id)
				};
				rejections
					.push(AssetCandidateRejection { outpoint: candidate.outpoint.clone(), reason });
				continue;
			}
			if candidate.asset_amount == 0 {
				let error = if !req.constraints.allow_fee_support {
					Some(DomainError::UtxoPolicyBlocked {
						outpoint: candidate.outpoint.clone(),
						reason: "fee-support inputs are disabled".into(),
					})
				} else if !account.capabilities.contains(&Capability::FeeSupport) {
					Some(DomainError::MissingAccountCapabilities { account_id: account.id.clone() })
				} else if !fee_scope_ids.contains(&utxo.scope_id) {
					Some(DomainError::MissingCapabilities { scope_id: utxo.scope_id.clone() })
				} else {
					None
				};
				if let Some(reason) = error {
					rejections.push(AssetCandidateRejection {
						outpoint: candidate.outpoint.clone(),
						reason,
					});
					continue;
				}
			}
			if let Some(reason) = utxo.unavailable_reason {
				rejections
					.push(AssetCandidateRejection { outpoint: candidate.outpoint.clone(), reason });
			} else {
				eligible.push(AssetCandidate {
					outpoint: candidate.outpoint.clone(),
					asset_amount: candidate.asset_amount,
					value_sats: utxo.value_sats,
				});
			}
		}
		if let Some(exact) = &req.constraints.exact_outpoints {
			for outpoint in exact {
				if let Some(rejection) = rejections.iter().find(|r| &r.outpoint == outpoint) {
					return Err(rejection.reason.clone());
				}
			}
		} else if eligible.is_empty() {
			return Err(DomainError::NoEligibleAssetCandidates { rejections });
		}
		AssetSelectionPolicy::select(
			&eligible,
			req.asset_amount,
			req.required_value_sats,
			&req.constraints,
		)
	}

	/// Select and persist the operation before returning any inputs to a transaction builder.
	pub fn plan_asset_selection(
		&mut self, req: PlanAssetSelectionRequest,
	) -> DomainResult<PlannedSelection> {
		let PlanAssetSelectionRequest { operation_id, selection: req } = req;
		let selected_outpoints = self.preview_asset_selection(&req)?;
		let fee_outpoints: BTreeSet<_> = req
			.candidates
			.iter()
			.filter(|candidate| candidate.asset_amount == 0)
			.map(|candidate| &candidate.outpoint)
			.collect();
		let first = selected_outpoints.first().ok_or(DomainError::InsufficientFunds)?;
		let change_scope_id = self
			.orchestrator
			.get_utxo(first)?
			.ok_or_else(|| DomainError::UtxoNotFound(first.clone()))?
			.scope_id;
		let effects = selected_outpoints
			.iter()
			.map(|outpoint| SelectionEffect::LockUtxo {
				outpoint: outpoint.clone(),
				purpose: if fee_outpoints.contains(outpoint) {
					LockPurpose::FeeSupport
				} else {
					LockPurpose::PlannedSelection
				},
			})
			.collect();
		let selection = OperationSelection {
			operation_id,
			selected_outpoints,
			change_scope_id,
			effects,
			expires_at: None,
		};
		self.orchestrator.apply_selection(&selection)?;
		Ok(selection.into())
	}
}
