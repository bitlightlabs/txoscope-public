use crate::{
	errors::{DomainError, DomainResult},
	operation::{InputSelectionRequest, InputSelectionSpec, SelectedInputs},
	policy::prefer_scope,
	state::{Availability, UtxoRole},
	store::StateRead,
	AccountId, ManagedUtxo, OutPoint, WalletScope, WalletScopeId,
};

pub struct SelectionContext<'a> {
	pub state: &'a dyn StateRead,
	pub account_id: &'a AccountId,
	pub account_scope_ids: &'a [WalletScopeId],
	/// Scopes belonging to the account that each satisfy all request capabilities.
	pub eligible_scope_ids: &'a [WalletScopeId],
}

impl SelectionContext<'_> {
	fn is_policy_spendable(&self, utxo: &ManagedUtxo) -> DomainResult<bool> {
		Ok(utxo.is_policy_spendable() && self.state.get_lock(&utxo.outpoint)?.is_none())
	}

	fn selectable_utxos(&self) -> DomainResult<Vec<OutPoint>> {
		let mut selected = Vec::new();
		for utxo in self.state.list_utxos()? {
			if utxo.account_id != *self.account_id {
				continue;
			}
			if !self.eligible_scope_ids.iter().any(|scope_id| scope_id == &utxo.scope_id) {
				continue;
			}
			if self.is_policy_spendable(&utxo)? {
				selected.push(utxo.outpoint.clone());
			}
		}
		Ok(selected)
	}

	fn preferred_scope(&self) -> DomainResult<WalletScope> {
		let scopes: Vec<WalletScope> = self
			.eligible_scope_ids
			.iter()
			.map(|scope_id| self.state.get_scope(scope_id))
			.collect::<DomainResult<Vec<_>>>()?
			.into_iter()
			.flatten()
			.collect();
		prefer_scope(scopes.iter())
			.cloned()
			.ok_or_else(|| DomainError::NoEligibleScope { account_id: self.account_id.clone() })
	}

	fn is_selectable_candidate(&self, outpoint: &OutPoint) -> DomainResult<bool> {
		let Some(utxo) = self.state.get_utxo(outpoint)? else {
			return Ok(false);
		};
		Ok(utxo.account_id == *self.account_id
			&& self.eligible_scope_ids.iter().any(|scope_id| scope_id == &utxo.scope_id)
			&& self.is_policy_spendable(&utxo)?)
	}

	fn require_selectable_candidate(&self, outpoint: &OutPoint) -> DomainResult<()> {
		let utxo = self
			.state
			.get_utxo(outpoint)?
			.ok_or_else(|| DomainError::UtxoNotFound(outpoint.clone()))?;
		if utxo.account_id != *self.account_id {
			return Err(DomainError::AccountNotFound(self.account_id.clone()));
		}
		if !self.eligible_scope_ids.iter().any(|scope_id| scope_id == &utxo.scope_id) {
			if self.account_scope_ids.contains(&utxo.scope_id)
				&& self
					.state
					.get_scope(&utxo.scope_id)?
					.is_some_and(|scope| scope.account_id == *self.account_id)
			{
				return Err(DomainError::MissingCapabilities { scope_id: utxo.scope_id });
			}
			return Err(DomainError::ScopeNotFound(utxo.scope_id));
		}
		if utxo.availability != Availability::Selectable {
			return Err(DomainError::UtxoUnavailable {
				outpoint: outpoint.clone(),
				availability: utxo.availability,
			});
		}
		if !self.is_policy_spendable(&utxo)? {
			return Err(DomainError::UtxoPolicyBlocked {
				outpoint: outpoint.clone(),
				reason: format!(
						"utxo is not safely spendable: onchain_state={:?}, conflict={}, lock_present={}",
						utxo.onchain_state,
						utxo.observation_conflict.is_some(),
						self.state.get_lock(outpoint)?.is_some(),
					),
			});
		}
		Ok(())
	}
}

pub trait UtxoSelector {
	fn select(
		&self, ctx: &SelectionContext<'_>, req: &InputSelectionRequest,
	) -> DomainResult<SelectedInputs>;
}

pub struct TargetValueSelector;
pub struct CandidateInputSelector;

#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct SelectorRegistry;

impl SelectorRegistry {
	pub(crate) fn selector_for(&self, spec: &InputSelectionSpec) -> &'static dyn UtxoSelector {
		match spec {
			InputSelectionSpec::TargetValue { .. } => &TargetValueSelector,
			InputSelectionSpec::ExactOutpoints { .. }
			| InputSelectionSpec::FirstSelectable { .. } => &CandidateInputSelector,
		}
	}
}

impl UtxoSelector for TargetValueSelector {
	fn select(
		&self, ctx: &SelectionContext<'_>, req: &InputSelectionRequest,
	) -> DomainResult<SelectedInputs> {
		let InputSelectionSpec::TargetValue { amount_sats } = &req.spec else { unreachable!() };
		let sort_policy = SelectionSortPolicy;
		let change_scope_id = ctx.preferred_scope()?.id.clone();
		let mut candidates = ctx.selectable_utxos()?;
		sort_policy.sort_candidate_ids(ctx.state, &req.spec, &mut candidates)?;
		let selected = sort_policy
			.accumulate_by_value(ctx.state, candidates, *amount_sats)?
			.ok_or(DomainError::InsufficientFunds)?;
		Ok(SelectedInputs { outpoints: selected, change_scope_id })
	}
}

impl UtxoSelector for CandidateInputSelector {
	fn select(
		&self, ctx: &SelectionContext<'_>, req: &InputSelectionRequest,
	) -> DomainResult<SelectedInputs> {
		let outpoints = match &req.spec {
			InputSelectionSpec::ExactOutpoints { outpoints } => {
				for outpoint in outpoints {
					ctx.require_selectable_candidate(outpoint)?;
				}
				outpoints.clone()
			},
			InputSelectionSpec::FirstSelectable { candidates } => {
				let mut selected = None;
				for outpoint in candidates {
					if ctx.is_selectable_candidate(outpoint)? {
						selected = Some(outpoint.clone());
						break;
					}
				}
				let selected = selected.ok_or(DomainError::NoCandidateAvailable)?;
				vec![selected]
			},
			InputSelectionSpec::TargetValue { .. } => unreachable!(),
		};
		let first = outpoints
			.first()
			.ok_or_else(|| DomainError::CorruptedState("selector returned no inputs".into()))?;
		let change_scope_id = ctx
			.state
			.get_utxo(first)?
			.ok_or_else(|| DomainError::UtxoNotFound(first.clone()))?
			.scope_id;
		Ok(SelectedInputs { outpoints, change_scope_id })
	}
}

#[derive(Debug, Default, Clone, Copy)]
struct SelectionSortPolicy;

impl SelectionSortPolicy {
	fn sort_candidate_ids(
		&self, state: &dyn StateRead, spec: &InputSelectionSpec, candidates: &mut Vec<OutPoint>,
	) -> DomainResult<()> {
		let mut keyed = Vec::with_capacity(candidates.len());
		for id in candidates.drain(..) {
			let Some(utxo) = state.get_utxo(&id)? else {
				return Err(DomainError::CorruptedState(format!(
					"selector candidate missing managed utxo: {id}"
				)));
			};
			keyed.push((self.selection_sort_key(spec, &utxo), id));
		}
		keyed.sort_by_key(|(key, _)| *key);
		*candidates = keyed.into_iter().map(|(_, id)| id).collect();
		Ok(())
	}

	fn accumulate_by_value(
		&self, state: &dyn StateRead, mut utxos: Vec<OutPoint>, target: u64,
	) -> DomainResult<Option<Vec<OutPoint>>> {
		let mut acc = 0u64;
		let mut selected = Vec::new();
		for utxo in utxos.drain(..) {
			let Some(candidate) = state.get_utxo(&utxo)? else {
				return Err(DomainError::CorruptedState(format!(
					"candidate disappeared during accumulation: {utxo}"
				)));
			};
			acc = acc.saturating_add(candidate.value_sats);
			selected.push(utxo);
			if acc >= target {
				return Ok(Some(selected));
			}
		}
		Ok(None)
	}

	fn selection_sort_key(
		&self, spec: &InputSelectionSpec, utxo: &ManagedUtxo,
	) -> (u8, u64, u8, u64) {
		match spec {
			InputSelectionSpec::TargetValue { .. } => {
				(0u8, utxo.value_sats, self.role_weight(utxo.role), 0u64)
			},
			InputSelectionSpec::ExactOutpoints { .. }
			| InputSelectionSpec::FirstSelectable { .. } => (0u8, 0u64, 0u8, 0u64),
		}
	}

	fn role_weight(&self, role: UtxoRole) -> u8 {
		match role {
			UtxoRole::SpendPreferred => 0,
			UtxoRole::General => 1,
			UtxoRole::ConsolidationCandidate => 2,
			_ => 10,
		}
	}
}
